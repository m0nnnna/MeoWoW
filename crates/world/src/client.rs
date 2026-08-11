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
    #[error("still queued at position {0} after waiting")]
    StillQueued(u32),
    #[error("the realm address {0:?} is not a host:port pair")]
    BadRealmAddress(String),
}

/// How many unrelated packets to skip while waiting for an expected one.
///
/// The server volunteers a handful after login -- addon verdicts, the cache
/// version, tutorial flags -- and more on other realm configurations. A bound
/// keeps a confused stream from looping forever without being so tight that a
/// chatty but healthy server trips it.
const MAX_SKIPPED: usize = 64;

/// A live, authenticated world connection.
pub struct Connection {
    stream: TcpStream,
    /// Absent until the session handshake completes; its presence is what marks
    /// the boundary where headers start being encrypted.
    crypt: Option<HeaderCrypt>,
    /// Reported by `SMSG_AUTH_RESPONSE`; 2 is Wrath.
    pub expansion: u8,
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

    /// Reads until the wanted opcode arrives, discarding what the server
    /// volunteers along the way.
    ///
    /// Skipped packets are still read *in full* -- the body has to leave the
    /// socket even when it is not wanted, or the next header is read from the
    /// middle of it.
    pub fn expect(&mut self, opcode: u16, name: &'static str) -> Result<Packet, Error> {
        for skipped in 0..MAX_SKIPPED {
            let packet = self.receive()?;
            if packet.opcode == opcode {
                return Ok(packet);
            }
            tracing::debug!(
                "skipping {} ({} bytes) while waiting for {name}",
                crate::opcode::describe(packet.opcode),
                packet.body.len()
            );
            if skipped + 1 == MAX_SKIPPED {
                return Err(Error::NoReply(name, MAX_SKIPPED));
            }
        }
        Err(Error::NoReply(name, MAX_SKIPPED))
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

        // --- character list
        let mut header = [0u8; 6];
        stream.read_exact(&mut header).expect("enum header");
        crypt.from_client.apply_keystream(&mut header);
        let opcode = u32::from_le_bytes(header[2..6].try_into().unwrap());
        assert_eq!(opcode, 0x0037, "expected CMSG_CHAR_ENUM");

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
