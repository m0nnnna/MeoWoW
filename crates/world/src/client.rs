//! Driving a world server connection.
//!
//! The shape of the exchange:
//!
//! ```text
//! server -> SMSG_AUTH_CHALLENGE   (plaintext header, carries the seed)
//! client -> CMSG_AUTH_SESSION     (plaintext header, proves the session key)
//!           ...both sides start the header cipher...
//! server -> SMSG_AUTH_RESPONSE    (encrypted from here on)
//! client -> CMSG_CHAR_ENUM
//! server -> SMSG_CHAR_ENUM
//! ```
//!
//! The cipher starts *between* the second and third messages, and that seam is
//! the whole difficulty. Encrypting the session header, or failing to encrypt
//! the one after it, desynchronises the keystream immediately and every later
//! packet decodes to noise.
//!
//! Reads use `read_exact` straight onto the socket rather than a buffered
//! reader. RC4 has no way back: a byte pulled into a buffer and decrypted
//! speculatively cannot be un-decrypted, so the reader must consume exactly the
//! bytes it has decided to decrypt and not one more.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use auth::SESSION_KEY_LEN;

use crate::crypt::HeaderCrypt;
use crate::opcode::{self, ClientOpcode};
use crate::protocol::{self, AuthResponse, Character};

/// Port a 3.3.5a world server listens on, absent a realm list saying otherwise.
pub const DEFAULT_PORT: u16 = 8085;

/// How often to send a keepalive.
///
/// This is a *lower* bound dressed as an interval, and getting it wrong in the
/// obvious direction is punished. The stock server counts any ping arriving
/// less than about 27 seconds after the previous one as "overspeed" and
/// disconnects after a couple of them, so a client that pings eagerly to be
/// safe is dropped faster than one that does not ping at all.
///
/// Confirmed the hard way: at five-second pings the live realm closed the
/// connection after the third, which surfaced as an unexpected end of stream
/// and looked exactly like a desynchronised header cipher.
pub const PING_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not reach the world server at {address}: {source}")]
    Connect {
        address: String,
        #[source]
        source: std::io::Error,
    },
    #[error("network error during {what}: {source}")]
    Io {
        what: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Protocol(#[from] protocol::Error),
    #[error("world server refused the session: {reason} (code {code:#04x})")]
    Refused { code: u8, reason: &'static str },
    #[error("expected {expected}, but the server sent {}", opcode::describe(*got))]
    Unexpected { expected: &'static str, got: u16 },
    #[error("gave up waiting for {0} after {1} intervening packets")]
    NoReply(&'static str, usize),
    #[error("the server refused to log this character in (code {code:#04x})")]
    LoginFailed { code: u8 },
    #[error("still queued at position {0} after waiting")]
    StillQueued(u32),
    #[error("the realm address {0:?} is not a host:port pair")]
    BadRealmAddress(String),
}

/// How many unrelated packets to skip while waiting for an expected one.
///
/// The session handshake only volunteers a handful -- addon verdicts, the cache
/// version, tutorial flags -- but entering the world sends a burst of several
/// dozen: action bars, spell lists, faction standings, the motd. The bound
/// exists to stop a stream that has lost its place from looping forever, so it
/// has to sit well above the largest healthy burst rather than snugly above it.
const MAX_SKIPPED: usize = 512;

/// A live, authenticated world connection.
pub struct Connection {
    stream: TcpStream,
    /// Absent until the session handshake completes; its presence is what marks
    /// the boundary where headers start being encrypted.
    crypt: Option<HeaderCrypt>,
    /// Reported by `SMSG_AUTH_RESPONSE`; 2 is Wrath.
    pub expansion: u8,
    /// Origin for the tick counts sent back in time-sync responses. The server
    /// only checks that they advance sensibly, so any fixed origin will do.
    started: std::time::Instant,
    ping_sequence: u32,
}

/// Written by hand rather than derived: the cipher holds key-derived state,
/// and a connection is exactly the kind of thing that ends up in an error log.
impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("peer", &self.stream.peer_addr().ok())
            .field("encrypted", &self.crypt.is_some())
            .field("expansion", &self.expansion)
            .finish()
    }
}

/// One received packet, header already stripped.
pub struct Packet {
    pub opcode: u16,
    pub body: Vec<u8>,
}

impl Connection {
    /// Connects and completes the session handshake.
    ///
    /// `realm_id` is the id from the logon server's realm list. Sending the
    /// wrong one is refused: the server looks the session up per realm.
    pub fn open(
        address: &str,
        account: &str,
        realm_id: u32,
        session_key: &[u8; SESSION_KEY_LEN],
        timeout: Duration,
    ) -> Result<Self, Error> {
        let mut connection = Self {
            stream: connect(address, timeout)?,
            crypt: None,
            expansion: 0,
            started: std::time::Instant::now(),
            ping_sequence: 0,
        };
        connection.handshake(account, realm_id, session_key)?;
        Ok(connection)
    }

    fn handshake(
        &mut self,
        account: &str,
        realm_id: u32,
        session_key: &[u8; SESSION_KEY_LEN],
    ) -> Result<(), Error> {
        // --- the server opens, in the clear
        let challenge = self.receive()?;
        if challenge.opcode != opcode::server::AUTH_CHALLENGE {
            return Err(Error::Unexpected {
                expected: "SMSG_AUTH_CHALLENGE",
                got: challenge.opcode,
            });
        }
        let challenge = protocol::parse_auth_challenge(&challenge.body)?;

        // --- our reply, still in the clear
        let client_seed = random_seed();
        let body = protocol::auth_session(
            account,
            realm_id,
            &client_seed,
            &challenge.server_seed,
            session_key,
        )?;
        self.send(ClientOpcode::AuthSession, &body)?;

        // --- and from the next byte in either direction, encrypted
        self.crypt = Some(HeaderCrypt::new(session_key));

        // A queue placement is a real answer, not a failure; the server sends a
        // second response when a slot frees. Waiting indefinitely would be
        // wrong for a CLI, so report the position and let the caller retry.
        match self.await_auth_response()? {
            AuthResponse::Ok { expansion } => {
                self.expansion = expansion;
                Ok(())
            }
            AuthResponse::Queued { position } => Err(Error::StillQueued(position)),
            AuthResponse::Refused { code } => Err(Error::Refused {
                code,
                reason: protocol::describe_response(code),
            }),
        }
    }

    fn await_auth_response(&mut self) -> Result<AuthResponse, Error> {
        let packet = self.expect(opcode::server::AUTH_RESPONSE, "SMSG_AUTH_RESPONSE")?;
        Ok(protocol::parse_auth_response(&packet.body)?)
    }

    /// Asks for the account's characters on this realm.
    pub fn characters(&mut self) -> Result<Vec<Character>, Error> {
        self.send(ClientOpcode::CharEnum, &[])?;
        let packet = self.expect(opcode::server::CHAR_ENUM, "SMSG_CHAR_ENUM")?;
        Ok(protocol::parse_char_enum(&packet.body)?)
    }

    /// Creates a character, returning the server's verdict.
    ///
    /// Present so the character list can be exercised against real data: an
    /// account with no characters proves the handshake but leaves every field
    /// offset in `SMSG_CHAR_ENUM` untested, and a wrong offset there parses
    /// perfectly and returns nonsense.
    pub fn create_character(
        &mut self,
        name: &str,
        look: &protocol::Appearance,
    ) -> Result<u8, Error> {
        self.send(ClientOpcode::CharCreate, &protocol::char_create(name, look))?;
        let packet = self.expect(opcode::server::CHAR_CREATE, "SMSG_CHAR_CREATE")?;
        Ok(protocol::parse_result_code(&packet.body, "SMSG_CHAR_CREATE")?)
    }

    /// Deletes a character by guid, returning the server's verdict.
    pub fn delete_character(&mut self, guid: u64) -> Result<u8, Error> {
        self.send(ClientOpcode::CharDelete, &protocol::char_delete(guid))?;
        let packet = self.expect(opcode::server::CHAR_DELETE, "SMSG_CHAR_DELETE")?;
        Ok(protocol::parse_result_code(&packet.body, "SMSG_CHAR_DELETE")?)
    }

    /// Enters the world as one character.
    ///
    /// The reply is not a single packet but a burst of them -- action bars,
    /// spell lists, faction standings, the motd -- with the position buried in
    /// the middle. Everything not asked for is skipped, except the housekeeping
    /// the server requires an answer to; see [`Connection::housekeep`].
    pub fn enter_world(&mut self, guid: u64) -> Result<protocol::WorldPosition, Error> {
        self.send(ClientOpcode::PlayerLogin, &protocol::player_login(guid))?;
        let packet = self.expect(
            opcode::server::LOGIN_VERIFY_WORLD,
            "SMSG_LOGIN_VERIFY_WORLD",
        )?;
        Ok(protocol::parse_login_verify_world(&packet.body)?)
    }

    /// Reads until the wanted opcode arrives, discarding what the server
    /// volunteers along the way.
    ///
    /// Skipped packets are still read *in full* -- the body has to leave the
    /// socket even when it is not wanted, or the next header is read from the
    /// middle of it.
    pub fn expect(&mut self, opcode: u16, name: &'static str) -> Result<Packet, Error> {
        for _ in 0..MAX_SKIPPED {
            let packet = self.receive()?;
            if packet.opcode == opcode {
                return Ok(packet);
            }
            self.housekeep(&packet)?;
        }
        Err(Error::NoReply(name, MAX_SKIPPED))
    }

    /// Answers the packets the server requires a reply to, whatever else is
    /// being waited for.
    ///
    /// Time sync is not optional: a server that stops hearing responses drops
    /// the session, and because the drop arrives as a plain connection close it
    /// is indistinguishable from a parser that lost its place in the stream.
    /// Handling it here rather than in a caller means no wait loop can forget.
    ///
    /// A login refusal is turned into an error rather than skipped -- it is the
    /// answer to the request, just not the hoped-for one, and skipping it would
    /// leave the caller waiting for a packet that is never coming.
    fn housekeep(&mut self, packet: &Packet) -> Result<(), Error> {
        match packet.opcode {
            opcode::server::TIME_SYNC_REQ => {
                let counter = protocol::parse_time_sync_req(&packet.body)?;
                let ticks = self.started.elapsed().as_millis() as u32;
                self.send(
                    ClientOpcode::TimeSyncResp,
                    &protocol::time_sync_resp(counter, ticks),
                )?;
                tracing::debug!("answered time sync {counter} with {ticks} ms");
            }
            opcode::server::CHARACTER_LOGIN_FAILED => {
                let code = protocol::parse_result_code(&packet.body, "SMSG_CHARACTER_LOGIN_FAILED")?;
                return Err(Error::LoginFailed { code });
            }
            other => tracing::debug!(
                "skipping {} ({} bytes)",
                crate::opcode::describe(other),
                packet.body.len()
            ),
        }
        Ok(())
    }

    /// Sends one movement packet for a mover we control.
    pub fn send_movement(
        &mut self,
        opcode: ClientOpcode,
        mover: u64,
        info: &crate::movement::MovementInfo,
    ) -> Result<(), Error> {
        self.send(opcode, &protocol::movement(mover, info))
    }

    /// Walks a character in a straight line and reports where it ended up.
    ///
    /// Movement is a *stream*, not a request: start, a heartbeat every so
    /// often, then stop. Sending only the endpoints is the obvious shortcut and
    /// the wrong one -- the server integrates position against elapsed time to
    /// decide whether a move was possible, and a single jump across the whole
    /// distance is exactly the shape of a speed hack.
    ///
    /// Nothing acknowledges any of this. A rejected move produces no error;
    /// the server simply keeps its own idea of where the character is. The only
    /// honest confirmation is to ask again later -- see the CLI, which
    /// reconnects and reads the position back from a fresh character list.
    pub fn walk(
        &mut self,
        mover: u64,
        from: crate::update::Position,
        heading: f32,
        distance: f32,
        speed: f32,
    ) -> Result<(crate::update::Position, Vec<Packet>), Error> {
        self.travel(
            mover,
            from,
            heading,
            crate::motion::Motion {
                forward: true,
                ..Default::default()
            },
            distance,
            speed,
        )
    }

    /// The same, in any direction the movement keys can express.
    ///
    /// [`Self::walk`] is this with `forward` held, and exists because most
    /// callers want exactly that. The general form is what makes strafing
    /// testable from the command line: a character *facing* one way and
    /// *travelling* another is the whole difference between walking and
    /// sidestepping, and it is a difference no single heading can express.
    ///
    /// The opcodes and flags come from [`crate::motion::Motion`] rather than
    /// being chosen here, so this rig and the viewer cannot disagree about
    /// what a strafe is -- which matters, because this rig is how the viewer's
    /// version gets checked against a real server.
    pub fn travel(
        &mut self,
        mover: u64,
        from: crate::update::Position,
        facing: f32,
        motion: crate::motion::Motion,
        distance: f32,
        speed: f32,
    ) -> Result<(crate::update::Position, Vec<Packet>), Error> {
        use crate::movement::MovementInfo;

        // A tenth of a second between heartbeats, which is roughly what a real
        // client sends while moving.
        const STEP: Duration = Duration::from_millis(100);

        let steps = ((distance / speed) / STEP.as_secs_f32()).ceil().max(1.0) as u32;
        // Where the character *goes*, which is not where it faces once
        // strafing is involved.
        let (dx, dy) = motion.direction(facing);
        let flags = motion.flags();

        let mut at = crate::update::Position {
            orientation: facing,
            ..from
        };
        let start = MovementInfo {
            flags,
            time: self.tick(),
            position: at,
            ..MovementInfo::default()
        };
        for opcode in crate::motion::Motion::transitions(Default::default(), motion) {
            self.send_movement(opcode, mover, &start)?;
        }

        // Kept and returned rather than discarded: movement is unacknowledged,
        // so what the server volunteers during it is the only evidence
        // available about whether it was accepted.
        let mut seen = Vec::new();

        for step in 1..=steps {
            std::thread::sleep(STEP);
            let travelled = (distance * step as f32 / steps as f32).min(distance);
            at.x = from.x + dx * travelled;
            at.y = from.y + dy * travelled;

            let beat = MovementInfo {
                flags,
                time: self.tick(),
                position: at,
                ..MovementInfo::default()
            };
            self.send_movement(ClientOpcode::MoveHeartbeat, mover, &beat)?;

            // Drain whatever arrived so the socket does not back up and the
            // time-sync answers keep flowing.
            seen.extend(self.drain(Duration::from_millis(1), 64)?);
        }

        // Stopping matters: a character left in a moving state keeps moving in
        // the server's simulation after the client goes quiet. Both axes get
        // stopped, by the same transition logic that started them.
        let stop = MovementInfo {
            flags: 0,
            time: self.tick(),
            position: at,
            ..MovementInfo::default()
        };
        for opcode in crate::motion::Motion::transitions(motion, Default::default()) {
            self.send_movement(opcode, mover, &stop)?;
        }
        seen.extend(self.drain(Duration::from_millis(300), 128)?);
        Ok((at, seen))
    }

    /// Jumps on the spot and lands again, reporting what arrived in between.
    ///
    /// The whole arc, because a jump is a pair of statements and the second is
    /// the one that is easy to forget: `MSG_MOVE_JUMP` says a character left
    /// the ground at a given velocity, and `MSG_MOVE_FALL_LAND` says it
    /// arrived. Without the landing the server goes on believing the character
    /// is in the air, and nothing complains.
    ///
    /// Timed against [`crate::motion::GRAVITY`] rather than slept for a round
    /// number, so `fall_time` is the time the arc actually took -- that field
    /// is what fall damage is computed from, and a value that disagrees with
    /// the height fallen is exactly the kind of inconsistency a server with
    /// movement checks looks for.
    pub fn jump_in_place(
        &mut self,
        mover: u64,
        at: crate::update::Position,
    ) -> Result<Vec<Packet>, Error> {
        use crate::movement::{Falling, MovementInfo};
        use crate::update::movement_flags;

        let jump = crate::motion::Jump::begin((0.0, 0.0), 0.0);
        let takeoff = MovementInfo {
            flags: movement_flags::FALLING,
            time: self.tick(),
            position: at,
            fall_time: 0,
            falling: Some(Falling {
                velocity: jump.velocity,
                sin_angle: jump.sin_angle,
                cos_angle: jump.cos_angle,
                xy_speed: jump.xy_speed,
            }),
            ..MovementInfo::default()
        };
        self.send_movement(ClientOpcode::MoveJump, mover, &takeoff)?;

        // Up and back down: `2v/g` seconds, which for the constants involved
        // is a little under a second.
        let airborne = Duration::from_secs_f32(2.0 * crate::motion::JUMP_VELOCITY / crate::motion::GRAVITY);
        let mut seen = self.drain(airborne, 128)?;

        let landing = MovementInfo {
            flags: 0,
            time: self.tick(),
            position: at,
            fall_time: airborne.as_millis() as u32,
            ..MovementInfo::default()
        };
        self.send_movement(ClientOpcode::MoveFallLand, mover, &landing)?;
        seen.extend(self.drain(Duration::from_millis(300), 128)?);
        Ok(seen)
    }

    /// Turns on the spot, without translating.
    ///
    /// Needed because orientation only reaches the server as a side effect of
    /// the position in a movement packet: a character that turns and does not
    /// walk would otherwise keep its old facing for everyone else.
    pub fn set_facing(
        &mut self,
        mover: u64,
        at: crate::update::Position,
        heading: f32,
    ) -> Result<(), Error> {
        let info = crate::movement::MovementInfo {
            time: self.tick(),
            position: crate::update::Position {
                orientation: heading,
                ..at
            },
            ..crate::movement::MovementInfo::default()
        };
        self.send_movement(ClientOpcode::MoveSetFacing, mover, &info)
    }

    /// Turns continuously on the spot for a short burst, the way holding `A`
    /// or `D` with no forward key does, and reports what came back.
    ///
    /// The same shape as [`Self::travel`]'s start/heartbeat/stop, but turning
    /// rather than translating: `MoveStartTurnLeft`/`Right`, a few heartbeats
    /// with the orientation advancing between them, then `MoveStopTurn`. This
    /// exists to confirm those three relayed opcodes against a real realm --
    /// see `foss-wow#37` -- the same way [`Self::travel`] originally confirmed
    /// strafing.
    pub fn turn_in_place(
        &mut self,
        mover: u64,
        at: crate::update::Position,
        clockwise: bool,
        duration: Duration,
        radians_per_sec: f32,
    ) -> Result<Vec<Packet>, Error> {
        use crate::movement::MovementInfo;

        const STEP: Duration = Duration::from_millis(100);
        let steps = (duration.as_secs_f32() / STEP.as_secs_f32()).ceil().max(1.0) as u32;
        let rate = if clockwise { -radians_per_sec } else { radians_per_sec };

        let start = MovementInfo {
            time: self.tick(),
            position: at,
            ..MovementInfo::default()
        };
        let start_opcode = if clockwise {
            ClientOpcode::MoveStartTurnRight
        } else {
            ClientOpcode::MoveStartTurnLeft
        };
        self.send_movement(start_opcode, mover, &start)?;

        let mut seen = Vec::new();
        let mut heading = at.orientation;
        for _ in 0..steps {
            std::thread::sleep(STEP);
            heading += rate * STEP.as_secs_f32();
            let beat = MovementInfo {
                time: self.tick(),
                position: crate::update::Position { orientation: heading, ..at },
                ..MovementInfo::default()
            };
            self.send_movement(ClientOpcode::MoveHeartbeat, mover, &beat)?;
            seen.extend(self.drain(Duration::from_millis(1), 64)?);
        }

        let stop = MovementInfo {
            time: self.tick(),
            position: crate::update::Position { orientation: heading, ..at },
            ..MovementInfo::default()
        };
        self.send_movement(ClientOpcode::MoveStopTurn, mover, &stop)?;
        seen.extend(self.drain(Duration::from_millis(300), 128)?);
        Ok(seen)
    }

    /// Switches between running and walking, confirming
    /// `MoveSetRunMode`/`MoveSetWalkMode` -- see `foss-wow#37`.
    ///
    /// A single statement rather than a held key: 3.3.5a has no walk key,
    /// only a toggle (bound to `\`` by default), so there is no "while held"
    /// state to model the way [`crate::motion::Motion`] models the movement
    /// keys.
    pub fn set_run_mode(
        &mut self,
        mover: u64,
        at: crate::update::Position,
        walking: bool,
    ) -> Result<(), Error> {
        use crate::movement::MovementInfo;
        use crate::update::movement_flags;

        let info = MovementInfo {
            flags: if walking { movement_flags::WALKING } else { 0 },
            time: self.tick(),
            position: at,
            ..MovementInfo::default()
        };
        let opcode = if walking {
            ClientOpcode::MoveSetWalkMode
        } else {
            ClientOpcode::MoveSetRunMode
        };
        self.send_movement(opcode, mover, &info)
    }

    /// Tells the server what this client has selected.
    ///
    /// A bare guid, unpacked -- `CMSG_SET_SELECTION` predates the packed-guid
    /// encoding used by the update blocks, and writing a packed one here sends
    /// a shorter packet that the server reads as a truncated selection.
    ///
    /// Nothing is expected back. The server answers only by acting differently
    /// later (a spell that now has a victim, an attack that lands), which is
    /// worth stating because "no reply" and "the packet was wrong" look
    /// identical from here.
    pub fn set_selection(&mut self, guid: u64) -> Result<(), Error> {
        self.send(ClientOpcode::SetSelection, &guid.to_le_bytes())
    }

    /// Asks who a guid is. The answer arrives as `SMSG_NAME_QUERY_RESPONSE`,
    /// or not at all -- see [`crate::names`] for why that has to be tolerated
    /// rather than waited on.
    pub fn ask_player_name(&mut self, guid: u64) -> Result<(), Error> {
        self.send(ClientOpcode::NameQuery, &crate::query::name_query(guid))
    }

    /// Asks what a creature entry is called.
    pub fn ask_creature_name(&mut self, entry: u32, guid: u64) -> Result<(), Error> {
        self.send(
            ClientOpcode::CreatureQuery,
            &crate::query::creature_query(entry, guid),
        )
    }

    /// Says something.
    ///
    /// `language` is not optional in practice: see
    /// [`crate::chat::language_for_race`]. Sending `LANG_UNIVERSAL` from an
    /// ordinary account is refused with no reply at all, which looks exactly
    /// like a malformed packet.
    ///
    /// Fire and forget otherwise: what comes back is the server relaying the
    /// line to everyone in range, this client included, through the ordinary
    /// packet stream. There is no reply to wait for, and waiting for one would
    /// block the render thread.
    pub fn say(
        &mut self,
        chat_type: crate::chat::ChatType,
        language: u32,
        target: &str,
        text: &str,
    ) -> Result<(), Error> {
        self.send(
            ClientOpcode::MessageChat,
            &crate::chat::message_chat(chat_type, language, target, text),
        )
    }

    /// Asks to cast a spell, at a target or at yourself.
    ///
    /// Nothing acknowledges success -- what follows is the world reacting, or
    /// `SMSG_CAST_FAILED` saying why not. As with every other send here, that
    /// means "no reply" has to be given time before it is read as a refusal.
    pub fn cast_spell(&mut self, spell_id: u32, target: Option<u64>) -> Result<(), Error> {
        self.send(
            ClientOpcode::CastSpell,
            &crate::spell::cast_spell(spell_id, target),
        )
    }

    /// Starts auto-attacking a target.
    ///
    /// A bare unpacked guid, the same encoding `CMSG_SET_SELECTION` uses --
    /// both predate the packed form the update blocks use. Nothing
    /// acknowledges the request directly; what follows is the server driving
    /// an exchange of swings on its own timer, which is the only way to tell
    /// this was understood. See [`ClientOpcode::AttackSwing`] for how that
    /// number was confirmed.
    pub fn attack_swing(&mut self, target: u64) -> Result<(), Error> {
        self.send(ClientOpcode::AttackSwing, &target.to_le_bytes())
    }

    /// Stops auto-attacking. Takes no body -- there is only ever one attack in
    /// progress, so there is nothing to name.
    pub fn attack_stop(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::AttackStop, &[])
    }

    /// Draws or stows the weapon.
    ///
    /// Nothing acknowledges this either, but unlike most sends here it *is*
    /// directly observable: the server writes the value straight into byte 0
    /// of the sender's `UNIT_FIELD_BYTES_2`, so the next object update carries
    /// it back. That is what confirmed both this opcode and the field, by
    /// sending each state in turn and watching the byte follow.
    ///
    /// **The server never sends this on its own.** Entering combat does not
    /// draw a weapon -- see [`crate::combat::SheathState`] -- so a client that
    /// does not call this leaves every character it controls permanently
    /// stowed, and every other player sees them that way.
    pub fn set_sheathed(&mut self, state: crate::combat::SheathState) -> Result<(), Error> {
        self.send(
            ClientOpcode::SetSheathed,
            &crate::combat::set_sheathed(state),
        )
    }

    /// Wears the item in a slot of the character's own inventory array.
    ///
    /// The server chooses which equipment slot it goes to, refuses politely if
    /// the item cannot be worn, and swaps if that slot is occupied -- so a
    /// caller needs to know only where the item *is*, not where it belongs.
    ///
    /// See [`ClientOpcode::AutoEquipItem`] for how the opcode was confirmed:
    /// nothing acknowledges the send, but the item's guid visibly moves
    /// between two fields of the player's own object, which is a result that
    /// could have failed to appear.
    pub fn equip_item(&mut self, slot: crate::inventory::InventorySlot) -> Result<(), Error> {
        self.send(
            ClientOpcode::AutoEquipItem,
            // The source *bag*, then the source slot. A slot from
            // `InventorySlot` is always an index into the player's own array,
            // which is exactly what `OWN_SLOT_ARRAY` says -- the type makes
            // the pairing unable to drift.
            &[
                crate::inventory::OWN_SLOT_ARRAY,
                slot.index() as u8,
            ],
        )
    }

    /// Sends [`ClientOpcode::SwapItemCandidate`] (`CMSG_SWAP_ITEM`) with a
    /// `{dst_bag, dst_slot, src_bag, src_slot}` body -- **confirmed live**,
    /// see the opcode's own doc comment for how `foss-wow#55` settled it.
    /// `255` for a bag means the player's own array, the same convention
    /// `equip_item` uses. Works for any pair: two backpack slots, an
    /// equipped slot and a backpack slot, or a slot inside an equipped bag.
    pub fn swap_item_candidate(
        &mut self,
        dst_bag: u8,
        dst_slot: u8,
        src_bag: u8,
        src_slot: u8,
    ) -> Result<(), Error> {
        self.send(
            ClientOpcode::SwapItemCandidate,
            &[dst_bag, dst_slot, src_bag, src_slot],
        )
    }

    /// Opens the loot on a corpse.
    ///
    /// The guid goes out **unpacked** -- eight plain bytes. Worth stating,
    /// because most guids on this wire are packed and using the packed form
    /// here would produce a shorter body that the server reads as a different
    /// message entirely.
    pub fn loot(&mut self, target: u64) -> Result<(), Error> {
        self.send(ClientOpcode::Loot, &target.to_le_bytes())
    }

    /// Takes the money off the corpse currently open.
    pub fn loot_money(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::LootMoney, &[])
    }

    /// Takes one slot off the corpse currently open.
    ///
    /// `slot` is the server's own loot index -- [`crate::LootItem::slot`] --
    /// and **not** a position in any list this client built. See
    /// [`ClientOpcode::AutoStoreLootItem`].
    pub fn loot_item(&mut self, slot: u8) -> Result<(), Error> {
        self.send(ClientOpcode::AutoStoreLootItem, &[slot])
    }

    /// Closes the loot window, releasing the corpse for anyone else.
    ///
    /// See [`ClientOpcode::LootRelease`]: skipping this leaves the body locked.
    pub fn loot_release(&mut self, target: u64) -> Result<(), Error> {
        self.send(ClientOpcode::LootRelease, &target.to_le_bytes())
    }

    /// Greets an NPC, asking for whatever menu it has.
    ///
    /// The guid goes out **unpacked**, like [`Connection::loot`]'s. See
    /// [`ClientOpcode::GossipHello`] for why this is the first NPC request to
    /// attempt and what a silence would and would not prove.
    pub fn gossip_hello(&mut self, target: u64) -> Result<(), Error> {
        self.send(ClientOpcode::GossipHello, &target.to_le_bytes())
    }

    /// Chooses a line from the menu a greeting produced.
    ///
    /// `option` is the **server's** option id, straight off
    /// [`GossipOption::index`](crate::GossipOption) -- not a position in
    /// whatever list a caller built. See
    /// [`ClientOpcode::GossipSelectOption`]: a filtered menu leaves holes in
    /// the numbering, so re-indexing picks the wrong line.
    ///
    /// `menu` is the id from the same reply, sent back so the server knows
    /// which menu is being answered. The trailing empty string is the box
    /// text a *coded* option would carry; every option observed so far has
    /// `coded == 0`, and sending an empty one is what a client with nothing
    /// to type does.
    pub fn gossip_select(&mut self, target: u64, menu: u32, option: u32) -> Result<(), Error> {
        let mut body = Vec::with_capacity(17);
        body.extend_from_slice(&target.to_le_bytes());
        body.extend_from_slice(&menu.to_le_bytes());
        body.extend_from_slice(&option.to_le_bytes());
        body.push(0);
        self.send(ClientOpcode::GossipSelectOption, &body)
    }

    /// Opens a questgiver's list of quests.
    pub fn questgiver_hello(&mut self, npc: u64) -> Result<(), Error> {
        self.send(ClientOpcode::QuestgiverHello, &npc.to_le_bytes())
    }

    /// Asks for one quest's offer text -- the scroll shown before accepting.
    ///
    /// The trailing byte is a **`u8`** here. Its sibling
    /// [`Connection::accept_quest`] takes a `u32` in the same position, and
    /// the two requests are otherwise identically shaped -- so the widths are
    /// written out once each rather than shared, because a wrong-width
    /// trailing field produces silence rather than an error.
    pub fn query_quest(&mut self, npc: u64, quest: u32) -> Result<(), Error> {
        let mut body = Vec::with_capacity(13);
        body.extend_from_slice(&npc.to_le_bytes());
        body.extend_from_slice(&quest.to_le_bytes());
        body.push(0);
        self.send(ClientOpcode::QuestgiverQueryQuest, &body)
    }

    /// Takes a quest. Confirmed by effect: the quest appears in the player's
    /// own replicated quest log.
    pub fn accept_quest(&mut self, npc: u64, quest: u32) -> Result<(), Error> {
        let mut body = Vec::with_capacity(16);
        body.extend_from_slice(&npc.to_le_bytes());
        body.extend_from_slice(&quest.to_le_bytes());
        // A `u32` here, not the `u8` `query_quest` sends. See that method.
        body.extend_from_slice(&0u32.to_le_bytes());
        self.send(ClientOpcode::QuestgiverAcceptQuest, &body)
    }

    /// Offers a finished quest for hand-in. No trailing field on this one.
    pub fn complete_quest(&mut self, npc: u64, quest: u32) -> Result<(), Error> {
        let mut body = Vec::with_capacity(12);
        body.extend_from_slice(&npc.to_le_bytes());
        body.extend_from_slice(&quest.to_le_bytes());
        self.send(ClientOpcode::QuestgiverCompleteQuest, &body)
    }

    /// Takes the reward and finishes a quest. `reward` chooses among the
    /// optional rewards and is `0` for a quest offering none.
    pub fn choose_quest_reward(
        &mut self,
        npc: u64,
        quest: u32,
        reward: u32,
    ) -> Result<(), Error> {
        let mut body = Vec::with_capacity(16);
        body.extend_from_slice(&npc.to_le_bytes());
        body.extend_from_slice(&quest.to_le_bytes());
        body.extend_from_slice(&reward.to_le_bytes());
        self.send(ClientOpcode::QuestgiverChooseReward, &body)
    }

    /// Asks what a quest actually is. One id per request -- see
    /// [`ClientOpcode::QuestQuery`] for why there is deliberately no bulk
    /// form.
    pub fn query_quest_info(&mut self, quest: u32) -> Result<(), Error> {
        self.send(ClientOpcode::QuestQuery, &quest.to_le_bytes())
    }

    /// Asks what mark belongs over one NPC's head.
    pub fn query_questgiver_status(&mut self, npc: u64) -> Result<(), Error> {
        self.send(ClientOpcode::QuestgiverStatusQuery, &npc.to_le_bytes())
    }

    /// Asks the same about every questgiver in range at once. No body.
    pub fn query_questgiver_status_multiple(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::QuestgiverStatusMultipleQuery, &[])
    }

    /// Asks where a set of quests' objectives are on the map.
    ///
    /// **Answers only for quests in the player's own log**, and the server
    /// refuses outright above 25 ids, so this caps rather than letting the
    /// caller send a request that will be dropped in silence.
    pub fn query_quest_poi(&mut self, quests: &[u32]) -> Result<(), Error> {
        const MAX: usize = 25;
        let quests = &quests[..quests.len().min(MAX)];
        let mut body = Vec::with_capacity(4 + quests.len() * 4);
        body.extend_from_slice(&(quests.len() as u32).to_le_bytes());
        for quest in quests {
            body.extend_from_slice(&quest.to_le_bytes());
        }
        self.send(ClientOpcode::QuestPoiQuery, &body)
    }

    /// Asks a vendor for its stock list, without going through a gossip menu.
    ///
    /// The guid goes out unpacked. See [`ClientOpcode::ListInventory`].
    pub fn list_inventory(&mut self, vendor: u64) -> Result<(), Error> {
        self.send(ClientOpcode::ListInventory, &vendor.to_le_bytes())
    }

    /// Buys `count` of one stock row from the open vendor.
    ///
    /// `slot` is the **server's** vendor slot, from
    /// [`VendorItem::slot`](crate::VendorItem), and `entry` the item it named.
    /// Both travel so the server can check they agree -- which also means a
    /// caller must not invent either from a list position.
    ///
    /// `bag` is where to put it; [`crate::inventory::OWN_SLOT_ARRAY`] means
    /// the player's own array and lets the server choose the slot, the same
    /// convention [`Connection::equip_item`] uses.
    ///
    /// Nothing acknowledges this -- see [`ClientOpcode::BuyItem`] for the
    /// effect that confirms it.
    pub fn buy_item(
        &mut self,
        vendor: u64,
        slot: u32,
        entry: u32,
        count: u32,
        bag: u8,
    ) -> Result<(), Error> {
        // **Item before slot, and the count is a `u32`.** Both were guessed
        // wrong first time and the result was total silence -- see
        // [`ClientOpcode::BuyItem`]. Twenty-one bytes, not eighteen.
        let mut body = Vec::with_capacity(21);
        body.extend_from_slice(&vendor.to_le_bytes());
        body.extend_from_slice(&entry.to_le_bytes());
        body.extend_from_slice(&slot.to_le_bytes());
        body.extend_from_slice(&count.to_le_bytes());
        body.push(bag);
        self.send(ClientOpcode::BuyItem, &body)
    }

    /// Sells an item to the open vendor.
    ///
    /// The item is named by **guid** rather than by slot, deliberately: a guid
    /// cannot go stale the way an index can, so a request that races an
    /// inventory change is refused instead of selling whatever moved into that
    /// slot. `count` of `0` means the whole stack.
    pub fn sell_item(&mut self, vendor: u64, item: u64, count: u32) -> Result<(), Error> {
        // Twenty bytes: the count is a `u32` like the buy request's, not the
        // `u8` a stack size would suggest.
        let mut body = Vec::with_capacity(20);
        body.extend_from_slice(&vendor.to_le_bytes());
        body.extend_from_slice(&item.to_le_bytes());
        body.extend_from_slice(&count.to_le_bytes());
        self.send(ClientOpcode::SellItem, &body)
    }

    /// Acknowledges a teleport within the current map.
    ///
    /// **The server will not finish the move until this arrives, and will
    /// throw away every movement packet sent in the meantime.** Both halves are
    /// silent, which is what makes forgetting it expensive: the character
    /// stands still on the server while the client walks it around locally, and
    /// nothing anywhere reports a problem.
    ///
    /// The reply is `{packed guid, u32, u32}`. The server reads the last two
    /// as flags and a time and uses neither, so the counter it sent is echoed
    /// into the first -- returning what was sent is a better default than
    /// inventing a value, and costs nothing.
    pub fn acknowledge_teleport(&mut self, mover: u64, counter: u32) -> Result<(), Error> {
        let mut body = Vec::with_capacity(16);
        crate::update::write_packed_guid(mover, &mut body);
        body.extend_from_slice(&counter.to_le_bytes());
        body.extend_from_slice(&self.tick().to_le_bytes());
        self.send(ClientOpcode::MoveTeleportAck, &body)
    }

    /// Asks where this character's body is. The request has no body at all.
    pub fn query_corpse(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::CorpseQuery, &[])
    }

    /// Releases the spirit, turning a corpse into a ghost at the graveyard.
    ///
    /// The body is one byte the server reads and throws away. Sending nothing
    /// at all is *not* the same thing: the read happens before any of the
    /// checks, so a zero-length body is a short read rather than a request.
    ///
    /// Nothing acknowledges this directly. What confirms it is the state
    /// changing -- `PLAYER_FLAGS` gaining its ghost bit, health going to one,
    /// and a corpse object appearing where the body fell.
    pub fn release_spirit(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::RepopRequest, &[0])
    }

    /// Takes the body back, resurrecting at the corpse.
    ///
    /// The guid is **unpacked**, like [`Self::attack_swing`]'s and unlike the
    /// packed form the update blocks use. Getting that wrong would send a
    /// shorter body that reads as a different guid entirely, and the failure
    /// would be silence -- this request has five separate silent refusals
    /// already, so it is the last place to also be guessing at an encoding.
    pub fn reclaim_corpse(&mut self, corpse: u64) -> Result<(), Error> {
        self.send(ClientOpcode::ReclaimCorpse, &corpse.to_le_bytes())
    }

    /// Milliseconds since the connection opened, as the movement clock.
    pub fn tick(&self) -> u32 {
        self.started.elapsed().as_millis() as u32
    }

    /// Reads packets until the server stops sending for `quiet_for`.
    ///
    /// Entering the world produces a burst rather than a reply, and nothing in
    /// it marks the end -- the only signal that the initial state is complete
    /// is the stream going quiet. So the read timeout is temporarily shortened
    /// and a timeout is treated as success rather than as an error.
    ///
    /// Housekeeping still runs on everything collected, because a burst can be
    /// long enough to contain a time-sync request.
    pub fn drain(&mut self, quiet_for: Duration, limit: usize) -> Result<Vec<Packet>, Error> {
        let restore = self.stream.read_timeout().ok().flatten();
        self.stream
            .set_read_timeout(Some(quiet_for))
            .map_err(|source| Error::Io {
                what: "shortening the read timeout",
                source,
            })?;

        let mut collected = Vec::new();
        let outcome = loop {
            if collected.len() >= limit {
                break Ok(());
            }
            match self.receive() {
                Ok(packet) => {
                    if let Err(error) = self.housekeep(&packet) {
                        break Err(error);
                    }
                    collected.push(packet);
                }
                // A quiet stream is the expected end of a burst, not a fault.
                Err(Error::Io { source, .. })
                    if matches!(
                        source.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    break Ok(())
                }
                Err(error) => break Err(error),
            }
        };

        self.stream
            .set_read_timeout(restore)
            .map_err(|source| Error::Io {
                what: "restoring the read timeout",
                source,
            })?;
        outcome.map(|()| collected)
    }

    /// Sends a keepalive without waiting for the echo.
    ///
    /// The right call from anything with a frame to render. [`Connection::ping`]
    /// blocks until the pong arrives -- a round trip on a real realm, some tens
    /// of milliseconds, and up to the read timeout if the server stalls -- which
    /// is several dropped frames every time it fires. The pong is picked up by
    /// the next [`Connection::drain`] and discarded, which is all the caller
    /// wanted from it anyway.
    ///
    /// Call no more often than [`PING_INTERVAL`]; the server treats a faster
    /// rate as abuse and disconnects.
    pub fn send_ping(&mut self, latency: u32) -> Result<(), Error> {
        self.ping_sequence = self.ping_sequence.wrapping_add(1);
        let sequence = self.ping_sequence;
        self.send(ClientOpcode::Ping, &protocol::ping(sequence, latency))
    }

    /// Sends a keepalive and waits for its echo.
    ///
    /// Blocks for a round trip, so prefer [`Connection::send_ping`] anywhere
    /// responsiveness matters. Useful when the round-trip time is itself the
    /// thing being measured.
    ///
    /// Call no more often than [`PING_INTERVAL`]; the server treats a faster
    /// rate as abuse and disconnects.
    pub fn ping(&mut self, latency: u32) -> Result<(), Error> {
        self.ping_sequence = self.ping_sequence.wrapping_add(1);
        let sequence = self.ping_sequence;
        self.send(ClientOpcode::Ping, &protocol::ping(sequence, latency))?;
        let packet = self.expect(opcode::server::PONG, "SMSG_PONG")?;
        let echoed = protocol::parse_pong(&packet.body)?;
        if echoed != sequence {
            // Not fatal, but it means the stream is not where it is believed to
            // be, which is worth knowing before something subtler goes wrong.
            tracing::warn!("pong echoed {echoed}, expected {sequence}");
        }
        Ok(())
    }

    /// Sends one packet, encrypting the header if the cipher is running.
    pub fn send(&mut self, opcode: ClientOpcode, body: &[u8]) -> Result<(), Error> {
        let mut packet = protocol::client_packet(opcode, body);
        if let Some(crypt) = self.crypt.as_mut() {
            crypt.encrypt(&mut packet[..protocol::CLIENT_HEADER_LEN]);
        }
        self.stream.write_all(&packet).map_err(|source| Error::Io {
            what: "sending a packet",
            source,
        })
    }

    /// Reads one packet.
    ///
    /// The header arrives in two reads because its length is not known until
    /// its first byte has been decrypted, and RC4 forbids reading further ahead
    /// than that.
    pub fn receive(&mut self) -> Result<Packet, Error> {
        let mut header = [0u8; protocol::SERVER_HEADER_LEN_LARGE];

        self.read_exact(&mut header[..1], "a packet header")?;
        if let Some(crypt) = self.crypt.as_mut() {
            crypt.decrypt(&mut header[..1]);
        }

        let header_len = protocol::server_header_len(header[0]);
        self.read_exact(&mut header[1..header_len], "a packet header")?;
        if let Some(crypt) = self.crypt.as_mut() {
            crypt.decrypt(&mut header[1..header_len]);
        }

        let parsed = protocol::parse_server_header(&header[..header_len])?;
        let mut body = vec![0u8; parsed.body_len];
        self.read_exact(&mut body, "a packet body")?;

        tracing::debug!(
            "received {} ({} bytes)",
            crate::opcode::describe(parsed.opcode),
            body.len()
        );
        Ok(Packet {
            opcode: parsed.opcode,
            body,
        })
    }

    fn read_exact(&mut self, into: &mut [u8], what: &'static str) -> Result<(), Error> {
        self.stream
            .read_exact(into)
            .map_err(|source| Error::Io { what, source })
    }
}

/// Splits a realm list address, supplying the default port when it has none.
pub fn split_realm_address(address: &str) -> Result<(String, u16), Error> {
    match address.rsplit_once(':') {
        Some((host, port)) => {
            let port = port
                .parse()
                .map_err(|_| Error::BadRealmAddress(address.to_string()))?;
            Ok((host.to_string(), port))
        }
        None => Ok((address.to_string(), DEFAULT_PORT)),
    }
}

fn random_seed() -> [u8; 4] {
    use rand::RngCore;
    let mut seed = [0u8; 4];
    rand::rng().fill_bytes(&mut seed);
    seed
}

fn connect(address: &str, timeout: Duration) -> Result<TcpStream, Error> {
    let resolved = address
        .to_socket_addrs()
        .map_err(|source| Error::Connect {
            address: address.to_string(),
            source,
        })?
        .next()
        .ok_or_else(|| Error::Connect {
            address: address.to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "name resolved to no addresses",
            ),
        })?;

    let stream = TcpStream::connect_timeout(&resolved, timeout).map_err(|source| Error::Connect {
        address: address.to_string(),
        source,
    })?;
    stream.set_read_timeout(Some(timeout)).map_err(|source| Error::Io {
        what: "setting the read timeout",
        source,
    })?;
    stream.set_write_timeout(Some(timeout)).map_err(|source| Error::Io {
        what: "setting the write timeout",
        source,
    })?;
    stream.set_nodelay(true).map_err(|source| Error::Io {
        what: "disabling Nagle",
        source,
    })?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::EQUIPMENT_SLOTS;
    use hmac::{Hmac, Mac};
    use rc4::consts::U20;
    use rc4::{KeyInit, Rc4, StreamCipher};
    use sha1::{Digest, Sha1};
    use std::net::TcpListener;

    const KEY: [u8; SESSION_KEY_LEN] = [0x3Cu8; SESSION_KEY_LEN];
    const SERVER_SEED: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];

    /// The server's half of the header cipher, derived here from the protocol
    /// description rather than by reusing [`HeaderCrypt`].
    ///
    /// That is the point of the exercise: a client checked against its own
    /// cipher proves only that the code is self-consistent. Two independent
    /// derivations agreeing is evidence that the derivation is right.
    struct ServerCrypt {
        to_client: Rc4<U20>,
        from_client: Rc4<U20>,
    }

    impl ServerCrypt {
        fn new(session_key: &[u8; SESSION_KEY_LEN]) -> Self {
            fn direction(seed: &[u8], key: &[u8]) -> Rc4<U20> {
                let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(seed).unwrap();
                mac.update(key);
                let derived = mac.finalize().into_bytes();
                let mut cipher = Rc4::<U20>::new(rc4::Key::<U20>::from_slice(&derived));
                let mut drop = [0u8; 1024];
                cipher.apply_keystream(&mut drop);
                cipher
            }
            // Written out here rather than imported, so a typo in the client's
            // copy of the seeds would show up as a failing test.
            let to_client = direction(
                &[
                    0xCC, 0x98, 0xAE, 0x04, 0xE8, 0x97, 0xEA, 0xCA, 0x12, 0xDD, 0xC0, 0x93, 0x42,
                    0x91, 0x53, 0x57,
                ],
                session_key,
            );
            let from_client = direction(
                &[
                    0xC2, 0xB3, 0x72, 0x3C, 0xC6, 0xAE, 0xD9, 0xB5, 0x34, 0x3C, 0x53, 0xEE, 0x2F,
                    0x43, 0x67, 0xCE,
                ],
                session_key,
            );
            Self {
                to_client,
                from_client,
            }
        }
    }

    /// How the mock server should behave for one test.
    #[derive(Clone)]
    struct Behaviour {
        account: String,
        characters: u8,
        refuse: Option<u8>,
        /// The key the *server* keys its cipher with. Normally [`KEY`]; a test
        /// sets it differently to put the two sides deliberately out of step.
        crypt_key: [u8; SESSION_KEY_LEN],
        /// Whether to check the client's digest. Turned off when a test wants
        /// the handshake to reach the encrypted stage despite a key mismatch.
        verify_digest: bool,
    }

    impl Behaviour {
        fn new(account: &str, characters: u8) -> Self {
            Self {
                account: account.to_string(),
                characters,
                refuse: None,
                crypt_key: KEY,
                verify_digest: true,
            }
        }
    }

    /// A world server good enough to complete a handshake against.
    fn serve_once(listener: TcpListener, behaviour: Behaviour) {
        let Behaviour {
            account,
            characters,
            refuse,
            crypt_key,
            verify_digest,
        } = behaviour;
        let (mut stream, _) = listener.accept().expect("accept");

        // --- challenge, in the clear
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&SERVER_SEED);
        body.extend_from_slice(&[0x77u8; 16]);
        body.extend_from_slice(&[0x88u8; 16]);
        write_server_packet(&mut stream, None, 0x01EC, &body);

        // --- the client's session, also in the clear
        let mut header = [0u8; 6];
        stream.read_exact(&mut header).expect("session header");
        let size = u16::from_be_bytes([header[0], header[1]]) as usize;
        let opcode = u32::from_le_bytes(header[2..6].try_into().unwrap());
        assert_eq!(opcode, 0x01ED, "expected CMSG_AUTH_SESSION");
        let mut session = vec![0u8; size - 4];
        stream.read_exact(&mut session).expect("session body");

        // Check the digest the way a server would: recompute it and compare.
        let name_at = 8;
        let name_end = name_at + session[name_at..].iter().position(|&b| b == 0).unwrap();
        let sent_account = String::from_utf8_lossy(&session[name_at..name_end]).into_owned();
        assert_eq!(sent_account, account.to_uppercase());

        let after_name = name_end + 1;
        let client_seed: [u8; 4] = session[after_name + 4..after_name + 8].try_into().unwrap();
        let digest_at = after_name + 4 + 4 + 4 + 4 + 4 + 8;
        let sent_digest = &session[digest_at..digest_at + 20];

        if verify_digest {
            let mut hasher = Sha1::new();
            hasher.update(sent_account.as_bytes());
            hasher.update(0u32.to_le_bytes());
            hasher.update(client_seed);
            hasher.update(SERVER_SEED);
            hasher.update(KEY);
            let expected: [u8; 20] = hasher.finalize().into();
            assert_eq!(sent_digest, expected, "client digest did not verify");
        }

        // --- everything past here is encrypted
        let mut crypt = ServerCrypt::new(&crypt_key);

        if let Some(code) = refuse {
            write_server_packet(&mut stream, Some(&mut crypt), 0x01EE, &[code]);
            return;
        }

        let mut response = vec![0x0Cu8];
        response.extend_from_slice(&0u32.to_le_bytes());
        response.push(0);
        response.extend_from_slice(&0u32.to_le_bytes());
        response.push(2);
        write_server_packet(&mut stream, Some(&mut crypt), 0x01EE, &response);

        // Volunteered packets the client must skip past rather than choke on.
        write_server_packet(&mut stream, Some(&mut crypt), 0x02EF, &0u32.to_le_bytes());
        write_server_packet(&mut stream, Some(&mut crypt), 0x04AB, &3u32.to_le_bytes());
        write_server_packet(&mut stream, Some(&mut crypt), 0x00FD, &[0u8; 32]);
        // A clock-sync poll in the middle of the burst. The client must answer
        // this unprompted, while it is waiting for something else entirely.
        write_server_packet(&mut stream, Some(&mut crypt), 0x0390, &99u32.to_le_bytes());

        // --- character list request
        //
        // Ordering here is not the obvious one and the test originally got it
        // wrong. The client sends CMSG_CHAR_ENUM *before* it has read the burst
        // above, so its time-sync answer necessarily arrives second -- it
        // cannot answer a packet it has not read yet. So accept either order
        // rather than demanding the tidy one.
        let mut answered_time_sync = false;
        loop {
            let (opcode, body) = read_client_packet(&mut stream, &mut crypt);
            match opcode {
                0x0391 => {
                    assert_eq!(
                        u32::from_le_bytes(body[0..4].try_into().unwrap()),
                        99,
                        "the time-sync counter must be echoed"
                    );
                    answered_time_sync = true;
                }
                0x0037 => break,
                other => panic!("unexpected client opcode {other:#06x}"),
            }
        }

        let mut body = vec![characters];
        for index in 0..characters {
            body.extend_from_slice(&(index as u64 + 1).to_le_bytes());
            body.extend_from_slice(format!("Tester{index}").as_bytes());
            body.push(0);
            body.extend_from_slice(&[1, 1, 0, 2, 3, 4, 5, 6]);
            body.push(80);
            body.extend_from_slice(&12u32.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(&(-8949.95f32).to_le_bytes());
            body.extend_from_slice(&(-132.49f32).to_le_bytes());
            body.extend_from_slice(&83.53f32.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            body.push(0);
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            for _ in 0..EQUIPMENT_SLOTS {
                body.extend_from_slice(&0u32.to_le_bytes());
                body.push(0);
                body.extend_from_slice(&0u32.to_le_bytes());
            }
        }
        write_server_packet(&mut stream, Some(&mut crypt), 0x003B, &body);

        // The answer is still in flight if it has not arrived yet; the client
        // sends it while draining toward the reply just written.
        if !answered_time_sync {
            let (opcode, body) = read_client_packet(&mut stream, &mut crypt);
            assert_eq!(opcode, 0x0391, "client never answered the time sync");
            assert_eq!(u32::from_le_bytes(body[0..4].try_into().unwrap()), 99);
        }
    }

    /// Reads one client packet, decrypting its header.
    fn read_client_packet(stream: &mut TcpStream, crypt: &mut ServerCrypt) -> (u32, Vec<u8>) {
        let mut header = [0u8; 6];
        stream.read_exact(&mut header).expect("client header");
        crypt.from_client.apply_keystream(&mut header);
        let size = u16::from_be_bytes([header[0], header[1]]) as usize;
        let opcode = u32::from_le_bytes(header[2..6].try_into().unwrap());
        let mut body = vec![0u8; size - 4];
        stream.read_exact(&mut body).expect("client body");
        (opcode, body)
    }

    fn write_server_packet(
        stream: &mut TcpStream,
        crypt: Option<&mut ServerCrypt>,
        opcode: u16,
        body: &[u8],
    ) {
        let size = body.len() + 2;
        let mut packet = Vec::new();
        if size > 0x7FFF {
            packet.push(0x80 | (size >> 16) as u8);
            packet.push((size >> 8) as u8);
            packet.push(size as u8);
        } else {
            packet.push((size >> 8) as u8);
            packet.push(size as u8);
        }
        packet.extend_from_slice(&opcode.to_le_bytes());
        let header_len = packet.len();
        if let Some(crypt) = crypt {
            crypt.to_client.apply_keystream(&mut packet[..header_len]);
        }
        packet.extend_from_slice(body);
        stream.write_all(&packet).expect("write packet");
    }

    fn spawn_server(behaviour: Behaviour) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || serve_once(listener, behaviour));
        address
    }

    /// The whole handshake over a real socket, against a server that derives
    /// the cipher and the digest independently.
    #[test]
    fn completes_the_handshake_and_lists_characters() {
        let address = spawn_server(Behaviour::new("account33", 2));
        let mut connection = Connection::open(
            &address,
            "account33",
            1,
            &KEY,
            Duration::from_secs(5),
        )
        .expect("handshake");

        assert_eq!(connection.expansion, 2, "Wrath expansion not reported");

        let characters = connection.characters().expect("character list");
        assert_eq!(characters.len(), 2);
        assert_eq!(characters[0].name, "Tester0");
        assert_eq!(characters[1].name, "Tester1");
        assert_eq!(characters[0].level, 80);
    }

    /// An account with no characters is a normal outcome, not an error -- and
    /// it is what a fresh account actually returns.
    #[test]
    fn an_account_with_no_characters_succeeds() {
        let address = spawn_server(Behaviour::new("account33", 0));
        let mut connection =
            Connection::open(&address, "account33", 1, &KEY, Duration::from_secs(5)).unwrap();
        assert!(connection.characters().unwrap().is_empty());
    }

    /// A refusal must surface with its reason rather than being parsed as a
    /// successful response.
    #[test]
    fn a_refused_session_reports_why() {
        let address = spawn_server(Behaviour {
            refuse: Some(0x14),
            ..Behaviour::new("account33", 0)
        });
        let err =
            Connection::open(&address, "account33", 1, &KEY, Duration::from_secs(5)).unwrap_err();
        assert!(
            matches!(err, Error::Refused { code: 0x14, .. }),
            "got: {err}"
        );
        assert!(err.to_string().contains("client build rejected"));
    }

    /// Two ciphers keyed differently must not silently appear to work.
    ///
    /// This is the failure mode the header cipher makes possible: RC4 has no
    /// integrity check, so a mis-keyed client does not get an error, it gets a
    /// header full of plausible-looking noise. The server here skips its digest
    /// check on purpose, so the handshake reaches the encrypted stage and the
    /// mismatch is genuinely in the cipher rather than in the proof.
    ///
    /// The property under test is narrow but the important one: never `Ok`, and
    /// never a hang.
    #[test]
    fn a_mis_keyed_cipher_never_looks_like_success() {
        let address = spawn_server(Behaviour {
            verify_digest: false,
            ..Behaviour::new("account33", 1)
        });

        let started = std::time::Instant::now();
        let wrong = [0x01u8; SESSION_KEY_LEN];
        let result = Connection::open(&address, "account33", 1, &wrong, Duration::from_secs(2));

        assert!(result.is_err(), "a wrong key completed the handshake");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "a wrong key hung rather than failing"
        );
    }

    #[test]
    fn realm_addresses_split_into_host_and_port() {
        assert_eq!(
            split_realm_address("108.174.48.199:8085").unwrap(),
            ("108.174.48.199".to_string(), 8085)
        );
        // A realm list may omit the port; the default applies.
        assert_eq!(
            split_realm_address("realm.example.com").unwrap(),
            ("realm.example.com".to_string(), DEFAULT_PORT)
        );
        assert!(split_realm_address("host:not-a-port").is_err());
    }
}
