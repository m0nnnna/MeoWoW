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

    /// Asks what a game object entry *is*.
    ///
    /// The answer's `kind` is what identifies a mailbox; see
    /// [`ClientOpcode::GameObjectQuery`]. `guid` may be `0` -- the answer is
    /// keyed on the entry alone, exactly as for creatures and items.
    pub fn ask_gameobject(&mut self, entry: u32, guid: u64) -> Result<(), Error> {
        self.send(
            ClientOpcode::GameObjectQuery,
            &crate::query::gameobject_query(entry, guid),
        )
    }

    /// Interacts with a game object -- see [`ClientOpcode::GameObjectUse`]
    /// for what "interact" resolves to and why it is unpacked.
    pub fn use_gameobject(&mut self, guid: u64) -> Result<(), Error> {
        self.send(ClientOpcode::GameObjectUse, &guid.to_le_bytes())
    }

    /// Asks what an item entry is -- its name, quality and the rest.
    ///
    /// `guid` may be `0`: the server keys the answer on the entry alone, and
    /// most things a client wants named (a loot row, a vendor's stock) are
    /// not objects it holds. See [`crate::query::item_query`].
    pub fn ask_item(&mut self, entry: u32, guid: u64) -> Result<(), Error> {
        self.send(
            ClientOpcode::ItemQuerySingle,
            &crate::query::item_query(entry, guid),
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

    /// Casts a spell at a game object -- opening a lock, per
    /// [`crate::spell::target_flags::GAMEOBJECT`].
    pub fn cast_spell_at_gameobject(&mut self, spell_id: u32, guid: u64) -> Result<(), Error> {
        self.send(
            ClientOpcode::CastSpell,
            &crate::spell::cast_spell_at_gameobject(spell_id, guid),
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

    /// Drops an aura this character is carrying, by spell id.
    ///
    /// **The way to switch a toggle off.** See
    /// [`ClientOpcode::CancelAura`](crate::ClientOpcode::CancelAura): casting
    /// the spell a second time is silently ignored, so this is not an
    /// optimisation over a recast, it is the only thing that works.
    pub fn cancel_aura(&mut self, spell_id: u32) -> Result<(), Error> {
        self.send(ClientOpcode::CancelAura, &spell_id.to_le_bytes())
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

    /// Takes an equipped (or carried) item off and lets the server drop it
    /// into the first free backpack square -- `CMSG_AUTOSTORE_BAG_ITEM`, the
    /// counterpart of [`Self::equip_item`].
    ///
    /// `slot` is an index into the player's own array, so both bag bytes are
    /// [`crate::inventory::OWN_SLOT_ARRAY`]: source "own array, this slot",
    /// destination "own array, you pick". This is the opcode `foss-wow#55`
    /// mistook `CMSG_SWAP_ITEM` for -- confirmed live doing exactly this when
    /// the source slot was an equipped one.
    pub fn store_in_backpack(
        &mut self,
        slot: crate::inventory::InventorySlot,
    ) -> Result<(), Error> {
        self.send(
            ClientOpcode::AutoStoreBagItem,
            &[
                crate::inventory::OWN_SLOT_ARRAY,
                slot.index() as u8,
                crate::inventory::OWN_SLOT_ARRAY,
            ],
        )
    }

    /// Uses an item where it sits: `(bag, slot)` addressed exactly as
    /// [`Self::equip_item`] and [`Self::swap_item_candidate`] address theirs.
    ///
    /// `spell_id` is the item's own on-use spell -- see
    /// [`crate::spell::use_item`] for why there is nowhere else to get it and
    /// why the guid goes out unpacked.
    pub fn use_item(
        &mut self,
        bag: u8,
        slot: u8,
        item_guid: u64,
        spell_id: u32,
        target: Option<u64>,
    ) -> Result<(), Error> {
        self.send(
            ClientOpcode::UseItem,
            &crate::spell::use_item(bag, slot, item_guid, spell_id, target),
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

    /// Asks a trainer what it will teach this character.
    ///
    /// The guid goes out unpacked, like the vendor's. See
    /// [`ClientOpcode::TrainerList`] -- this one is *answered*, which is why
    /// it is the first thing sent at a trainer and the thing that bounds the
    /// silent purchase below it.
    pub fn trainer_list(&mut self, trainer: u64) -> Result<(), Error> {
        self.send(ClientOpcode::TrainerList, &trainer.to_le_bytes())
    }

    /// Learns one spell from the open trainer.
    ///
    /// `spell` is the id from [`TrainerSpell::spell`](crate::TrainerSpell) and
    /// **not** a row position -- the server's list is filtered per character,
    /// so a position means different things to different readers while an id
    /// does not.
    ///
    /// Answered by `SMSG_TRAINER_BUY_SUCCEEDED` on success and by **nothing at
    /// all** on refusal, so a caller should check
    /// [`TrainerSpellState::is_learnable`](crate::TrainerSpellState::is_learnable)
    /// before spending a send it cannot interpret the failure of.
    pub fn trainer_buy_spell(&mut self, trainer: u64, spell: u32) -> Result<(), Error> {
        let mut body = Vec::with_capacity(12);
        body.extend_from_slice(&trainer.to_le_bytes());
        body.extend_from_slice(&spell.to_le_bytes());
        self.send(ClientOpcode::TrainerBuySpell, &body)
    }

    /// Asks another player to trade.
    ///
    /// The guid goes out unpacked, like the vendor's and the trainer's.
    ///
    /// **This send is silent when it works and answered when it does not.**
    /// A trade that starts is announced to the *partner*, not to the sender,
    /// so nothing comes back here until their client replies. Every refusal
    /// does come back, naming a [`crate::TradeStatus`] -- and that asymmetry
    /// is what makes the block confirmable at all: pointed at a guid that is
    /// not a player, this is answered immediately, from one client, with
    /// nobody else logged in. See [`ClientOpcode::InitiateTrade`].
    pub fn initiate_trade(&mut self, partner: u64) -> Result<(), Error> {
        self.send(ClientOpcode::InitiateTrade, &partner.to_le_bytes())
    }

    /// Agrees to open a trade window somebody else asked for.
    ///
    /// **The first request in this client that exists because another person
    /// has to say yes.** Answered with `OPEN_WINDOW` at *both* ends, which is
    /// also the first time a send from here has produced a packet at somebody
    /// else's client.
    pub fn begin_trade(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::BeginTrade, &[])
    }

    /// Declines a trade because this client is busy.
    ///
    /// One of three mutually exclusive answers to an offer -- this,
    /// [`Self::begin_trade`], and the ignore form this client does not send.
    /// The server closes the trade on either refusal, so exactly one of the
    /// three goes out per offer.
    pub fn busy_trade(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::BusyTrade, &[])
    }

    /// Accepts what is on the table.
    ///
    /// `token` is the `u32` from [`crate::TradeStatus::OpenWindow`]. **The
    /// body cannot be confirmed on this realm**: the server's handler reads
    /// nothing from it, so no observation here separates four bytes from
    /// none. It is sent because the risk is one-sided -- a server checking a
    /// minimum size refuses an empty body and none refuse a body they ignore
    /// -- and the token is what goes in it because it is the only number the
    /// server has offered that belongs to this trade. See
    /// [`ClientOpcode::AcceptTrade`].
    ///
    /// Silent at the sender. The partner gets `TRADE_ACCEPT`; both get
    /// `TRADE_COMPLETE` once the second accept lands.
    pub fn accept_trade(&mut self, token: u32) -> Result<(), Error> {
        self.send(ClientOpcode::AcceptTrade, &token.to_le_bytes())
    }

    /// Takes an accept back. The server answers **the other end** with
    /// `BACK_TO_TRADE`.
    pub fn unaccept_trade(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::UnacceptTrade, &[])
    }

    /// Calls the trade off.
    pub fn cancel_trade(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::CancelTrade, &[])
    }

    /// Puts one carried item into a trade slot.
    ///
    /// `bag` and `slot` address the item exactly as [`Self::equip_item`] and
    /// [`Self::swap_item_candidate`] address theirs, so
    /// [`HeldItem::address`](crate::HeldItem::address) supplies both.
    ///
    /// `trade_slot` is one of the seven, and
    /// [`trade::NONTRADED_SLOT`](crate::trade::NONTRADED_SLOT) is the one that
    /// does not change hands -- putting something there offers nothing.
    ///
    /// **Confirmed by effect and by nothing else**: the server answers by
    /// resending the whole offer to both ends, so the item appearing in the
    /// reflected offer is the proof the three bytes were understood.
    pub fn set_trade_item(&mut self, trade_slot: u8, bag: u8, slot: u8) -> Result<(), Error> {
        self.send(ClientOpcode::SetTradeItem, &[trade_slot, bag, slot])
    }

    /// Takes an item back out of a trade slot.
    pub fn clear_trade_item(&mut self, trade_slot: u8) -> Result<(), Error> {
        self.send(ClientOpcode::ClearTradeItem, &[trade_slot])
    }

    /// Puts copper on the table.
    ///
    /// **Refused with `BUSY` when the sender does not have it**, which is one
    /// value doing two unrelated jobs -- it means "the other end is already
    /// trading" everywhere else. Reported raw rather than explained, for the
    /// reason `describe_cast_failure` names exactly one code.
    pub fn set_trade_gold(&mut self, copper: u32) -> Result<(), Error> {
        self.send(ClientOpcode::SetTradeGold, &copper.to_le_bytes())
    }

    /// Asks a flight master where it can send this character.
    ///
    /// The guid goes out unpacked, like the vendor's and the trainer's. See
    /// [`ClientOpcode::TaxiQueryAvailableNodes`].
    pub fn taxi_query_nodes(&mut self, npc: u64) -> Result<(), Error> {
        self.send(ClientOpcode::TaxiQueryAvailableNodes, &npc.to_le_bytes())
    }

    /// Buys a flight from `from` to `to`.
    ///
    /// Both are [`dbc::schema::TaxiNodes`] row ids, and `from` must be the
    /// node the **server** named in [`TaxiMenu::current_node`](crate::TaxiMenu)
    /// rather than one this client worked out from the player's position --
    /// see [`ClientOpcode::ActivateTaxi`].
    ///
    /// Answered either way, so unlike most requests in this crate a caller
    /// can tell a refusal from a misunderstanding.
    pub fn activate_taxi(&mut self, npc: u64, from: u32, to: u32) -> Result<(), Error> {
        let mut body = Vec::with_capacity(16);
        body.extend_from_slice(&npc.to_le_bytes());
        body.extend_from_slice(&from.to_le_bytes());
        body.extend_from_slice(&to.to_le_bytes());
        self.send(ClientOpcode::ActivateTaxi, &body)
    }

    /// Posts a letter.
    ///
    /// `mailbox` is the guid of a mailbox this character can reach; see
    /// [`ClientOpcode::GetMailList`] for why a game master's own guid also
    /// works and why that is a trap rather than a shortcut. `items` are full
    /// item guids out of this character's own bags.
    ///
    /// **Answered either way** by
    /// [`SEND_MAIL_RESULT`](crate::opcode::server::SEND_MAIL_RESULT), whose
    /// action field echoes [`MailAction::Send`](crate::mail::MailAction), so
    /// one send bounds the opcode, the body and the reply together -- the
    /// move `CMSG_GROUP_INVITE` made for parties and `CMSG_LIST_INVENTORY`
    /// made for buying.
    ///
    /// The body is built by [`crate::mail::send_mail_body`] rather than here,
    /// so the one structure in this block that travels outward is defined in
    /// the same module that reads its consequences.
    #[allow(clippy::too_many_arguments)]
    pub fn send_mail(
        &mut self,
        mailbox: u64,
        receiver: &str,
        subject: &str,
        text: &str,
        money: u32,
        cod: u32,
        items: &[u64],
    ) -> Result<(), Error> {
        let body = crate::mail::send_mail_body(mailbox, receiver, subject, text, money, cod, items);
        self.send(ClientOpcode::SendMail, &body)
    }

    /// Asks a mailbox for the inbox. See [`ClientOpcode::GetMailList`].
    pub fn get_mail_list(&mut self, mailbox: u64) -> Result<(), Error> {
        self.send(ClientOpcode::GetMailList, &mailbox.to_le_bytes())
    }

    /// Takes the copper out of one letter.
    pub fn mail_take_money(&mut self, mailbox: u64, mail: u32) -> Result<(), Error> {
        self.send(ClientOpcode::MailTakeMoney, &mail_request(mailbox, mail))
    }

    /// Takes one attachment.
    ///
    /// `item` is the **32-bit low guid** off
    /// [`MailItem::guid`](crate::mail::MailItem), which is the only handle a
    /// mailed item has -- see [`ClientOpcode::MailTakeItem`].
    pub fn mail_take_item(&mut self, mailbox: u64, mail: u32, item: u32) -> Result<(), Error> {
        let mut body = mail_request(mailbox, mail);
        body.extend_from_slice(&item.to_le_bytes());
        self.send(ClientOpcode::MailTakeItem, &body)
    }

    /// Marks a letter as opened.
    ///
    /// **The one silent request in this block.** Its effect shows up only in
    /// the next inbox, so a caller that wants to confirm it has to re-ask --
    /// which is also what the interface does anyway, for the reason the
    /// trainer list is re-asked rather than edited after a purchase.
    pub fn mail_mark_as_read(&mut self, mailbox: u64, mail: u32) -> Result<(), Error> {
        self.send(ClientOpcode::MailMarkAsRead, &mail_request(mailbox, mail))
    }

    /// Sends a letter back to whoever sent it.
    ///
    /// The trailing guid is the field the server reads and ignores, taking the
    /// sender off its own copy instead. Sent as zero rather than filled in,
    /// because a value this client would have to guess at and the server never
    /// consults is a value it should not be inventing.
    pub fn mail_return_to_sender(&mut self, mailbox: u64, mail: u32) -> Result<(), Error> {
        let mut body = mail_request(mailbox, mail);
        body.extend_from_slice(&0u64.to_le_bytes());
        self.send(ClientOpcode::MailReturnToSender, &body)
    }

    /// Throws a letter away.
    ///
    /// **Refused when there is cash on delivery on it**, and the refusal
    /// arrives as an ordinary result rather than as silence.
    pub fn mail_delete(&mut self, mailbox: u64, mail: u32, template: u32) -> Result<(), Error> {
        let mut body = mail_request(mailbox, mail);
        body.extend_from_slice(&template.to_le_bytes());
        self.send(ClientOpcode::MailDelete, &body)
    }

    /// Copies a letter's text into a paper item.
    pub fn mail_create_text_item(&mut self, mailbox: u64, mail: u32) -> Result<(), Error> {
        self.send(
            ClientOpcode::MailCreateTextItem,
            &mail_request(mailbox, mail),
        )
    }

    /// Asks what is waiting, from anywhere in the world.
    ///
    /// The only question about mail that does not need a mailbox, and it
    /// answers with **at most two senders** -- see
    /// [`crate::mail::NextMailTime`].
    pub fn query_next_mail_time(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::QueryNextMailTime, &[])
    }

    /// Greets an auctioneer, and asks which house it serves.
    ///
    /// **The only request in the block whose reply names a house.** See
    /// [`crate::auction::AuctionHouse`].
    pub fn auction_hello(&mut self, auctioneer: u64) -> Result<(), Error> {
        self.send(
            ClientOpcode::AuctionHello,
            &crate::auction::hello_body(auctioneer),
        )
    }

    /// Asks for sales awaiting collection -- **the auction block's bounding
    /// instrument**.
    ///
    /// The handler behind it checks nothing at all and always replies. So one
    /// of these, sent from anywhere in the world with no auctioneer and no
    /// fixture, is what separates "the opcode or the socket is wrong" from
    /// "the request was declined". Every other send here is silent when its
    /// auctioneer does not resolve, and a silent send is indistinguishable
    /// from a wrong opcode -- the failure this project has walked into in
    /// every city service so far.
    ///
    /// See [`ClientOpcode::AuctionListPendingSales`].
    pub fn auction_list_pending_sales(&mut self, auctioneer: u64) -> Result<(), Error> {
        self.send(
            ClientOpcode::AuctionListPendingSales,
            &crate::auction::pending_sales_body(auctioneer),
        )
    }

    /// Searches. `offset` is the row to start at, in units of rows and not of
    /// pages.
    ///
    /// **The caller must also tell the world state what it asked for**, with
    /// [`WorldState::expect_auction_page`](crate::WorldState::expect_auction_page):
    /// the reply does not carry the offset, so nothing downstream can work it
    /// out. Both calls are here rather than one because this type does not
    /// own the state and the state does not own the socket; the pairing is
    /// stated in both doc comments and exercised by the probe.
    pub fn auction_list_items(
        &mut self,
        auctioneer: u64,
        offset: u32,
        search: &crate::auction::AuctionSearch,
    ) -> Result<(), Error> {
        self.send(
            ClientOpcode::AuctionListItems,
            &crate::auction::list_items_body(auctioneer, offset, search),
        )
    }

    /// Asks what this character is selling. Does not page.
    pub fn auction_list_owner_items(&mut self, auctioneer: u64) -> Result<(), Error> {
        self.send(
            ClientOpcode::AuctionListOwnerItems,
            &crate::auction::list_owner_items_body(auctioneer, 0),
        )
    }

    /// Asks what this character is bidding on.
    ///
    /// `outbid` are auctions this client believes it has been outbid on; the
    /// server looks each one up and adds it to the reply. An empty slice is a
    /// complete request -- see
    /// [`crate::auction::list_bidder_items_body`] for what the server does
    /// with a count that disagrees with the body.
    pub fn auction_list_bidder_items(
        &mut self,
        auctioneer: u64,
        outbid: &[u32],
    ) -> Result<(), Error> {
        self.send(
            ClientOpcode::AuctionListBidderItems,
            &crate::auction::list_bidder_items_body(auctioneer, 0, outbid),
        )
    }

    /// Posts an auction out of one or more stacks in this character's bags.
    ///
    /// Returns `Ok(false)` without sending when the request is one the server
    /// would **drop in silence** -- no items, a zero count, a zero guid or a
    /// zero opening bid. Being refused here with a reason beats being ignored
    /// there without one. See [`crate::auction::sell_item_body`].
    pub fn auction_sell_item(
        &mut self,
        auctioneer: u64,
        items: &[(u64, u32)],
        bid: u32,
        buyout: u32,
        duration: crate::auction::AuctionDuration,
    ) -> Result<bool, Error> {
        match crate::auction::sell_item_body(auctioneer, items, bid, buyout, duration) {
            Some(body) => {
                self.send(ClientOpcode::AuctionSellItem, &body)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Bids, or buys out -- **the same request**, separated only by whether
    /// `price` equals the auction's buyout.
    ///
    /// Returns `Ok(false)` without sending for the two inputs the server
    /// drops in silence.
    pub fn auction_place_bid(
        &mut self,
        auctioneer: u64,
        auction: u32,
        price: u32,
    ) -> Result<bool, Error> {
        match crate::auction::place_bid_body(auctioneer, auction, price) {
            Some(body) => {
                self.send(ClientOpcode::AuctionPlaceBid, &body)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Cancels one of this character's own auctions.
    ///
    /// The goods come back **as mail**, so confirming this end to end needs an
    /// inbox -- 4.27 is what makes 4.30 checkable, and neither milestone's
    /// opcodes touch the other's.
    pub fn auction_remove_item(&mut self, auctioneer: u64, auction: u32) -> Result<(), Error> {
        self.send(
            ClientOpcode::AuctionRemoveItem,
            &crate::auction::remove_item_body(auctioneer, auction),
        )
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

    /// Destroys `count` of a carried item outright, addressed by slot rather
    /// than by guid -- unlike [`Connection::sell_item`]. See
    /// [`ClientOpcode::DestroyItem`] for what is and is not confirmed here,
    /// including why the body carries three trailing zero bytes nothing in
    /// this crate reads back.
    ///
    /// `bag` is [`crate::inventory::OWN_SLOT_ARRAY`] for the player's own
    /// array, the same convention every other slot-addressed send in this
    /// module uses.
    pub fn destroy_item(&mut self, bag: u8, slot: u8, count: u8) -> Result<(), Error> {
        self.send(ClientOpcode::DestroyItem, &[bag, slot, count, 0, 0, 0])
    }

    /// Asks a player to join this character's group, naming them by the name
    /// they type on a chat line rather than by guid.
    ///
    /// See [`ClientOpcode::GroupInvite`] for why the name is the right handle
    /// here and why this is the party request to attempt first. The trailing
    /// `u32` is read and discarded by the server; zero is what a client with
    /// nothing to say sends.
    pub fn group_invite(&mut self, name: &str) -> Result<(), Error> {
        let mut body = Vec::with_capacity(name.len() + 5);
        body.extend_from_slice(name.as_bytes());
        body.push(0);
        body.extend_from_slice(&0u32.to_le_bytes());
        self.send(ClientOpcode::GroupInvite, &body)
    }

    /// Asks for the guild roster. See [`ClientOpcode::GuildRoster`].
    ///
    /// **The one request in this block that is answered whether or not it can
    /// work**, which is what bounds the silent ones: sent by a character in no
    /// guild it returns a
    /// [`CommandResult`](crate::guild::CommandResult) rather than nothing.
    pub fn guild_roster(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::GuildRoster, &[])
    }

    /// Asks what a guild is called, by id. Answers for any guild.
    pub fn guild_query(&mut self, guild_id: u32) -> Result<(), Error> {
        self.send(
            ClientOpcode::GuildQuery,
            &crate::guild::guild_query_body(guild_id),
        )
    }

    /// Asks for the guild's summary -- name, founding date, counts.
    pub fn guild_info(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::GuildInfo, &[])
    }

    /// Asks a player to join, by name.
    pub fn guild_invite(&mut self, name: &str) -> Result<(), Error> {
        self.send(
            ClientOpcode::GuildInvite,
            &crate::guild::named_player_body(name),
        )
    }

    /// Accepts the pending guild invitation. Nothing identifies which, because
    /// a character can hold only one -- the same shape as
    /// [`Connection::group_accept`], and here the body is genuinely empty
    /// rather than a zero word.
    pub fn guild_accept(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::GuildAccept, &[])
    }

    /// Declines the pending guild invitation.
    pub fn guild_decline(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::GuildDecline, &[])
    }

    /// Leaves the guild. Refused for the guild master.
    pub fn guild_leave(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::GuildLeave, &[])
    }

    /// Moves a member up one rank, by name.
    pub fn guild_promote(&mut self, name: &str) -> Result<(), Error> {
        self.send(
            ClientOpcode::GuildPromote,
            &crate::guild::named_player_body(name),
        )
    }

    /// Moves a member down one rank, by name.
    pub fn guild_demote(&mut self, name: &str) -> Result<(), Error> {
        self.send(
            ClientOpcode::GuildDemote,
            &crate::guild::named_player_body(name),
        )
    }

    /// Throws a member out, by name.
    pub fn guild_remove(&mut self, name: &str) -> Result<(), Error> {
        self.send(
            ClientOpcode::GuildRemove,
            &crate::guild::named_player_body(name),
        )
    }

    /// Sets the message of the day.
    pub fn guild_motd(&mut self, text: &str) -> Result<(), Error> {
        self.send(ClientOpcode::GuildMotd, &crate::guild::text_body(text))
    }

    /// Sets the longer information text.
    pub fn guild_info_text(&mut self, text: &str) -> Result<(), Error> {
        self.send(ClientOpcode::GuildInfoText, &crate::guild::text_body(text))
    }

    /// Sets a member's public note.
    pub fn guild_set_public_note(&mut self, member: &str, note: &str) -> Result<(), Error> {
        self.send(
            ClientOpcode::GuildSetPublicNote,
            &crate::guild::member_note_body(member, note),
        )
    }

    /// Sets a member's officer note. Identical body to
    /// [`Connection::guild_set_public_note`] and a different opcode.
    pub fn guild_set_officer_note(&mut self, member: &str, note: &str) -> Result<(), Error> {
        self.send(
            ClientOpcode::GuildSetOfficerNote,
            &crate::guild::member_note_body(member, note),
        )
    }

    /// Accepts the pending invite. Nothing identifies *which* invite because a
    /// character can hold only one.
    pub fn group_accept(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::GroupAccept, &0u32.to_le_bytes())
    }

    /// Declines the pending invite.
    pub fn group_decline(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::GroupDecline, &[])
    }

    /// Leaves the group, or breaks it up if this character leads it. See
    /// [`ClientOpcode::GroupDisband`]: one opcode does both, and the server
    /// decides which.
    pub fn group_disband(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::GroupDisband, &[])
    }

    /// Throws a member out by guid. The trailing `u32` is the length of a
    /// vote-kick reason string; zero means an ordinary kick and no string
    /// follows.
    pub fn group_uninvite(&mut self, member: u64) -> Result<(), Error> {
        let mut body = Vec::with_capacity(12);
        body.extend_from_slice(&member.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        self.send(ClientOpcode::GroupUninviteGuid, &body)
    }

    /// Hands leadership to another member.
    pub fn group_set_leader(&mut self, member: u64) -> Result<(), Error> {
        self.send(ClientOpcode::GroupSetLeader, &member.to_le_bytes())
    }

    /// Changes the party's loot rule. See [`ClientOpcode::GroupSetLootMethod`]
    /// for the body layout and why nothing here checks leadership -- the
    /// server already refuses a non-leader's request in silence, and this
    /// client's own callers refuse it first using [`crate::group::Party::is_leader`].
    pub fn set_loot_method(&mut self, method: u32, master: u64, threshold: u32) -> Result<(), Error> {
        let mut body = Vec::with_capacity(16);
        body.extend_from_slice(&method.to_le_bytes());
        body.extend_from_slice(&master.to_le_bytes());
        body.extend_from_slice(&threshold.to_le_bytes());
        self.send(ClientOpcode::GroupSetLootMethod, &body)
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

    /// Acknowledges a transfer to another map.
    pub fn acknowledge_worldport(&mut self) -> Result<(), Error> {
        self.send(ClientOpcode::MoveWorldportAck, &[])
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

    /// Accepts a spirit healer's offer to resurrect at the graveyard, the way
    /// back to life that needs no corpse run.
    ///
    /// The guid is the spirit healer's, **unpacked**, and must match the one
    /// [`SMSG_SPIRIT_HEALER_CONFIRM`](crate::opcode::server::SPIRIT_HEALER_CONFIRM)
    /// just carried -- the server re-resolves it and refuses in silence if it
    /// is not a spirit healer within interaction range. Sending it unprompted
    /// does nothing: the confirm is what makes the character eligible.
    ///
    /// Nothing acknowledges it directly. What confirms it is the ghost flag
    /// clearing and health returning, both replicated -- and, usually, a
    /// same-map teleport to the graveyard nearest the body.
    pub fn spirit_healer_activate(&mut self, healer: u64) -> Result<(), Error> {
        self.send(ClientOpcode::SpiritHealerActivate, &healer.to_le_bytes())
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
                // A quiet stream is the expected end of a burst, not a fault
                // -- and on Windows "quiet" has three spellings, one of which
                // has no `ErrorKind`. See [`is_quiet_stream`].
                Err(Error::Io { source, .. }) if is_quiet_stream(&source) => break Ok(()),
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

        // **The only read here that may legitimately time out.** Nothing has
        // been consumed yet, so giving up costs nothing and is how `drain`
        // learns the stream has gone quiet.
        self.read_exact(&mut header[..1], "a packet header")?;

        // **Past this point the packet must be finished or the connection is
        // dead.** Reading that byte advanced the TCP stream, and decrypting it
        // advances the RC4 header cipher; abandoning the packet now leaves the
        // cipher a byte ahead of the stream forever, and every subsequent
        // header decrypts to garbage. It does not fail loudly -- it fails as
        // "packet claims 7099367 bytes", thousands of times, while the client
        // carries on rendering a world it can no longer hear.
        //
        // A caller draining with a 1ms timeout -- which the viewer does, every
        // frame -- hits this whenever a header straddles two TCP segments. So
        // the remainder of the packet gets a timeout long enough that only a
        // genuinely broken connection can trip it.
        let restore = self.stream.read_timeout().ok().flatten();
        let _ = self.stream.set_read_timeout(Some(PACKET_COMPLETION_TIMEOUT));
        let finished = self.receive_after_first_byte(&mut header);
        let _ = self.stream.set_read_timeout(restore);
        finished
    }

    /// The rest of a packet, once its first byte has been committed to.
    fn receive_after_first_byte(&mut self, header: &mut [u8]) -> Result<Packet, Error> {
        if let Some(crypt) = self.crypt.as_mut() {
            crypt.decrypt(&mut header[..1]);
        }

        let header_len = protocol::server_header_len(header[0]);
        self.read_committed(&mut header[1..header_len], "a packet header")?;
        if let Some(crypt) = self.crypt.as_mut() {
            crypt.decrypt(&mut header[1..header_len]);
        }

        let parsed = protocol::parse_server_header(&header[..header_len])?;
        let mut body = vec![0u8; parsed.body_len];
        self.read_committed(&mut body, "a packet body")?;

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

    /// Fills `into` completely, tolerating a stream that goes quiet part-way
    /// through, and **never discarding what it has already taken**.
    ///
    /// **`read_exact` cannot be used past a packet's first byte.** Its
    /// contract on failure is that the buffer contents are unspecified and
    /// *how many bytes were consumed is unspecified too* -- and consumed is
    /// consumed: those bytes have left the TCP stream for good. Before the
    /// header cipher existed that was survivable, because a fresh read
    /// simply resynchronised on the next packet. With RC4 it is permanent:
    /// every byte taken from the stream but never fed through the cipher
    /// leaves it one step out for the rest of the connection, and every
    /// later header decrypts to noise.
    ///
    /// [`Connection::receive`] already lengthens the timeout past the first
    /// byte for exactly this reason, and that narrows the window without
    /// closing it -- one `WouldBlock`, `TimedOut`, or Windows 997 arriving
    /// mid-header is enough, and 997 is documented on [`is_quiet_stream`] as
    /// having killed a live session once already from the *first-byte* path,
    /// where the damage is far smaller. Seen live as two
    /// `packet claims 6146905 bytes` warnings and then a process that was
    /// gone: a desynchronised stream hands garbage lengths to parsers, and a
    /// parser that sizes an allocation from a garbage count asks for
    /// gigabytes, which Windows refuses and Rust answers by aborting --
    /// no panic, no unwind, no backtrace, nothing in the log at all.
    ///
    /// So a quiet stream here is waited on rather than reported, bounded by
    /// [`PACKET_COMPLETION_TIMEOUT`] overall so a connection that has
    /// genuinely gone away still ends.
    fn read_committed(&mut self, into: &mut [u8], what: &'static str) -> Result<(), Error> {
        let deadline = std::time::Instant::now() + PACKET_COMPLETION_TIMEOUT;
        let mut filled = 0;
        while filled < into.len() {
            match self.stream.read(&mut into[filled..]) {
                // A clean end of stream mid-packet is a dead connection, and
                // must not spin: there is nothing more coming.
                Ok(0) => {
                    return Err(Error::Io {
                        what,
                        source: std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "the connection closed part-way through a packet",
                        ),
                    })
                }
                Ok(read) => filled += read,
                Err(source) if source.kind() == std::io::ErrorKind::Interrupted => {}
                Err(source) if is_quiet_stream(&source) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(Error::Io { what, source });
                    }
                }
                Err(source) => return Err(Error::Io { what, source }),
            }
        }
        Ok(())
    }
}

/// How long the rest of a packet may take once its first byte has been read.
///
/// Generous on purpose. This is not a latency budget -- it is the window in
/// which abandoning the read would corrupt the cipher, so the only thing that
/// should ever trip it is a connection that has actually gone away.
const PACKET_COMPLETION_TIMEOUT: Duration = Duration::from_secs(15);

impl Error {
    /// Whether this error means the stream itself can no longer be read, as
    /// opposed to one packet this client could not make sense of.
    ///
    /// **The distinction is whether framing survived.** A body that will not
    /// parse is a parser problem: `receive` had already read the length,
    /// consumed exactly that many bytes and left the cursor between packets,
    /// so the next read is still aligned and the session is worth keeping.
    /// A framing failure is the opposite -- there is no way back. Once the
    /// header cipher is out of step every subsequent length is noise, and no
    /// amount of reading re-synchronises an RC4 stream; the only cure is a
    /// new connection.
    ///
    /// Callers use this to decide whether to end the session rather than to
    /// carry on warning once a frame at a world they can no longer hear,
    /// which is what the viewer did for the whole of a live session that
    /// ended in a crash.
    pub fn is_connection_lost(&self) -> bool {
        match self {
            // A real socket error. A *quiet* one is the ordinary end of a
            // burst and never reaches a caller as an error at all.
            Error::Io { source, .. } => !is_quiet_stream(source),
            Error::Connect { .. } => true,
            // Both mean a length field decrypted to nonsense, which is the
            // signature of a desynchronised header cipher. Every other
            // protocol error is about a body, and leaves framing intact.
            Error::Protocol(
                protocol::Error::Oversized { .. } | protocol::Error::Undersized { .. },
            ) => true,
            _ => false,
        }
    }
}

/// Whether a read error means "no data yet" rather than a broken connection.
///
/// **`WouldBlock` and `TimedOut` are not the whole set on Windows.** A socket
/// with a read timeout there can also surface `ERROR_IO_PENDING` (997,
/// *"Overlapped I/O operation is in progress"*), which Rust maps to no
/// `ErrorKind` at all -- so matching on kind alone classifies a perfectly
/// ordinary quiet stream as a fault. That is exactly what killed a live
/// session: one 997 during a header read, and the connection spent the next
/// nine minutes reporting packets of seven megabytes.
pub(crate) fn is_quiet_stream(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) || matches!(error.raw_os_error(), Some(997) | Some(10035) | Some(10060))
}

/// The head every mail request but the send and the list shares:
/// `{u64 mailbox, u32 mail id}`.
///
/// Written once because six requests carry it. Two copies of a shared prefix
/// drift, and the outgoing half of this protocol has nothing to announce a
/// drift with -- the standing reason a structure travelling both ways is
/// defined once and round-tripped.
fn mail_request(mailbox: u64, mail: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&mailbox.to_le_bytes());
    body.extend_from_slice(&mail.to_le_bytes());
    body
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

    /// A quiet stream has three spellings on Windows, and only two have a
    /// `ErrorKind`.
    ///
    /// The third, `ERROR_IO_PENDING` (997), is what a real session actually
    /// hit: one of them during a header read was classified as a broken
    /// connection, and the stream never recovered.
    #[test]
    fn a_quiet_stream_is_recognised_by_all_its_spellings() {
        use std::io::{Error as IoError, ErrorKind};
        assert!(is_quiet_stream(&IoError::from(ErrorKind::WouldBlock)));
        assert!(is_quiet_stream(&IoError::from(ErrorKind::TimedOut)));
        // 997 maps to no `ErrorKind` at all, which is the whole problem.
        assert!(is_quiet_stream(&IoError::from_raw_os_error(997)));
        assert!(is_quiet_stream(&IoError::from_raw_os_error(10035)));
        // And a genuinely broken connection is still broken.
        assert!(!is_quiet_stream(&IoError::from(ErrorKind::ConnectionReset)));
        assert!(!is_quiet_stream(&IoError::from(ErrorKind::UnexpectedEof)));
    }

    /// **A body that will not parse must not cost the session; a framing
    /// failure must.** The difference is whether the next read is still
    /// aligned: a parse error means `receive` already consumed exactly the
    /// bytes the length announced, and an `Oversized`/`Undersized` length
    /// means the header cipher is out of step and never coming back.
    #[test]
    fn only_a_framing_failure_counts_as_a_lost_connection() {
        use std::io::{Error as IoError, ErrorKind};

        // The desync signature, both spellings.
        assert!(Error::Protocol(protocol::Error::Oversized { got: 6146905 })
            .is_connection_lost());
        assert!(Error::Protocol(protocol::Error::Undersized { got: 1 }).is_connection_lost());

        // A dead socket.
        assert!(Error::Io {
            what: "a packet body",
            source: IoError::from(ErrorKind::ConnectionReset),
        }
        .is_connection_lost());

        // A quiet one is not a fault at all -- it is how a burst ends.
        assert!(!Error::Io {
            what: "a packet header",
            source: IoError::from(ErrorKind::WouldBlock),
        }
        .is_connection_lost());

        // Body-level complaints leave the stream aligned, so the session is
        // still worth keeping -- including the impossible count that used to
        // abort the process.
        assert!(!Error::Protocol(protocol::Error::ImpossibleCount {
            what: "quest POI sets",
            count: u32::MAX as usize,
            left: 4,
        })
        .is_connection_lost());
        assert!(!Error::Protocol(protocol::Error::Trailing {
            what: "SMSG_GUILD_ROSTER",
            got: 7,
        })
        .is_connection_lost());
        assert!(!Error::Protocol(protocol::Error::UnknownObjectType { got: 9 })
            .is_connection_lost());
        assert!(!Error::NoReply("SMSG_TRAINER_LIST", 12).is_connection_lost());
    }

    /// A packet whose header arrives in two pieces is still read whole, even
    /// when the caller's read timeout is far shorter than the gap.
    ///
    /// **This is the bug that killed a live session, and it is silent.** The
    /// reader takes the first header byte, decrypts it -- advancing the RC4
    /// header cipher -- and then needs three more. Under the viewer's 1ms
    /// drain timeout, a header split across two TCP segments used to abandon
    /// the packet there, leaving the cipher one byte ahead of the stream
    /// *forever*. Nothing errors at the time; what follows is thousands of
    /// "packet claims 7099367 bytes" while the client keeps rendering a world
    /// it can no longer hear.
    ///
    /// So the delay here is deliberately much longer than the timeout: on the
    /// old code the first `receive` fails and the second returns garbage, and
    /// on the fixed code both packets arrive intact.
    #[test]
    fn a_header_split_across_segments_does_not_desynchronise_the_cipher() {
        use std::io::Write;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut crypt = ServerCrypt::new(&KEY);
            // Build one packet, then dribble it: one byte, a pause far longer
            // than the reader's timeout, then the rest.
            let body = [0xAAu8; 8];
            let size = body.len() + 2;
            let mut packet = vec![(size >> 8) as u8, size as u8];
            packet.extend_from_slice(&0x1234u16.to_le_bytes());
            crypt.to_client.apply_keystream(&mut packet[..4]);
            packet.extend_from_slice(&body);

            stream.write_all(&packet[..1]).expect("first byte");
            stream.flush().ok();
            std::thread::sleep(Duration::from_millis(120));
            stream.write_all(&packet[1..]).expect("the rest");
            stream.flush().ok();

            // A second, whole packet, to prove the cipher is still aligned.
            write_server_packet(&mut stream, Some(&mut crypt), 0x5678, &[0xBB; 4]);
            std::thread::sleep(Duration::from_millis(200));
        });

        let stream = TcpStream::connect(address).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_millis(1)))
            .expect("timeout");
        let mut connection = Connection {
            stream,
            crypt: Some(HeaderCrypt::new(&KEY)),
            expansion: 2,
            started: std::time::Instant::now(),
            ping_sequence: 0,
        };

        // Retry the *first byte* until the dribbled packet starts arriving --
        // that read is allowed to time out, and does. Everything after it must
        // not.
        let first = loop {
            match connection.receive() {
                Ok(packet) => break packet,
                Err(Error::Io { source, .. }) if is_quiet_stream(&source) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("split header broke the read: {e}"),
            }
        };
        assert_eq!(first.opcode, 0x1234, "the split packet");
        assert_eq!(first.body, vec![0xAAu8; 8]);

        let second = loop {
            match connection.receive() {
                Ok(packet) => break packet,
                Err(Error::Io { source, .. }) if is_quiet_stream(&source) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("the cipher desynchronised: {e}"),
            }
        };
        assert_eq!(
            second.opcode, 0x5678,
            "the packet after a split header decoded to the wrong opcode,              which is what a desynchronised header cipher looks like"
        );
        assert_eq!(second.body, vec![0xBBu8; 4]);
        server.join().ok();
    }

    /// **A read that has already taken bytes must never give them back.**
    /// `read_exact` does exactly that: on any error its contract leaves both
    /// the buffer contents *and the number of bytes consumed* unspecified,
    /// and consumed bytes are gone from the TCP stream for good. Feed it a
    /// stream that dribbles, with gaps longer than the socket's timeout, and
    /// it eats part of the buffer and reports failure -- which past a
    /// packet's first byte means the RC4 header cipher is permanently one
    /// step out.
    ///
    /// Seen live as two `packet claims 6146905 bytes` warnings followed by a
    /// process that was simply gone, with no panic in the log: garbage
    /// lengths reach parsers, a parser sizes an allocation from a garbage
    /// count, and the allocation failure aborts without unwinding.
    ///
    /// The timeout here is far shorter than the gaps on purpose, so the old
    /// code fails and the fixed code waits.
    #[test]
    fn a_committed_read_waits_out_a_dribbling_stream_rather_than_losing_bytes() {
        use std::io::Write;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            // Sixteen bytes, four at a time, with gaps far longer than the
            // reader's 1ms timeout.
            for chunk in 0..4u8 {
                stream.write_all(&[chunk; 4]).expect("chunk");
                stream.flush().ok();
                std::thread::sleep(Duration::from_millis(30));
            }
            std::thread::sleep(Duration::from_millis(100));
        });

        let stream = TcpStream::connect(address).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_millis(1)))
            .expect("timeout");
        let mut connection = Connection {
            stream,
            crypt: None,
            expansion: 2,
            started: std::time::Instant::now(),
            ping_sequence: 0,
        };

        let mut got = [0u8; 16];
        connection
            .read_committed(&mut got, "a dribbled buffer")
            .expect("a committed read must wait out a quiet stream");
        assert_eq!(
            got,
            [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3],
            "every byte must arrive, in order, with none dropped on a timeout"
        );
        server.join().ok();
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
