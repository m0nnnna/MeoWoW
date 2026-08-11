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
        use crate::movement::MovementInfo;
        use crate::update::movement_flags;

        // A tenth of a second between heartbeats, which is roughly what a real
        // client sends while moving.
        const STEP: Duration = Duration::from_millis(100);

        let steps = ((distance / speed) / STEP.as_secs_f32()).ceil().max(1.0) as u32;
        let (dx, dy) = (heading.cos(), heading.sin());

        let mut at = crate::update::Position {
            orientation: heading,
            ..from
        };
        let start = MovementInfo {
            flags: movement_flags::FORWARD,
            time: self.tick(),
            position: at,
            ..MovementInfo::default()
        };
        self.send_movement(ClientOpcode::MoveStartForward, mover, &start)?;

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
                flags: movement_flags::FORWARD,
                time: self.tick(),
                position: at,
                ..MovementInfo::default()
            };
            self.send_movement(ClientOpcode::MoveHeartbeat, mover, &beat)?;

            // Drain whatever arrived so the socket does not back up and the
            // time-sync answers keep flowing.
            seen.extend(self.drain(Duration::from_millis(1), 64)?);
        }

        // Stopping matters: a character left in the FORWARD state keeps moving
        // in the server's simulation after the client goes quiet.
        let stop = MovementInfo {
            flags: 0,
            time: self.tick(),
            position: at,
            ..MovementInfo::default()
        };
        self.send_movement(ClientOpcode::MoveStop, mover, &stop)?;
        seen.extend(self.drain(Duration::from_millis(300), 128)?);
        Ok((at, seen))
    }

    /// Milliseconds since the connection opened, as the movement clock.
    fn tick(&self) -> u32 {
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

    /// Sends a keepalive and waits for its echo.
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
