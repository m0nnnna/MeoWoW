//! Talking to the logon server.
//!
//! The conversation is strictly ordered and each reply's length depends on its
//! contents, so this reads greedily into a buffer and lets the parsers decide
//! what is complete. Timeouts are explicit: an unreachable realm should fail in
//! seconds with a clear message rather than hang.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::protocol::{self, Realm};
use crate::srp6;

/// Port the logon server listens on.
pub const DEFAULT_PORT: u16 = 3724;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not reach {address}: {source}")]
    Connect {
        address: String,
        #[source]
        source: std::io::Error,
    },
    #[error("network error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Protocol(#[from] protocol::Error),
    #[error(transparent)]
    Srp(#[from] srp6::Error),
    #[error("the server's proof did not verify; it does not know this account's password")]
    ServerProofFailed,
    #[error(
        "this realm requires a PIN or authenticator (security flags {0:#04x}), \
         which this client cannot supply"
    )]
    NeedsSecondFactor(u8),
    #[error("connection closed by the server during {0}")]
    Closed(&'static str),
    #[error("account exists but the password was rejected")]
    WrongPassword,
    #[error("no such account on this realm")]
    NoSuchAccount,
}

/// A completed login.
#[derive(Debug)]
pub struct LoggedIn {
    /// The key the world server expects its header cipher to be keyed with.
    /// Deliberately not printed by `Debug`.
    pub session_key: [u8; srp6::SESSION_KEY_LEN],
    pub realms: Vec<Realm>,
}

/// Runs challenge, proof and realm list against a logon server.
pub fn login(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    locale: &str,
    timeout: Duration,
) -> Result<LoggedIn, Error> {
    let address = format!("{host}:{port}");
    let mut stream = connect(&address, timeout)?;

    stream.write_all(&protocol::logon_challenge(username, locale))?;
    let reply = read_message(&mut stream, "challenge")?;
    // A refusal here means the account was not found: the server has not seen
    // any password yet, so it cannot be rejecting one.
    let challenge = match protocol::parse_challenge(&reply) {
        Err(protocol::Error::Refused(reason)) if reason.contains("unknown account") => {
            return Err(Error::NoSuchAccount)
        }
        other => other?,
    };

    // A PIN or authenticator would need extra fields appended to the proof.
    if challenge.security_flags != 0 {
        return Err(Error::NeedsSecondFactor(challenge.security_flags));
    }

    let private = srp6::Client::random_private();
    let session = srp6::Client::respond(
        username,
        password,
        &challenge.salt,
        &challenge.b_pub,
        &private,
    )?;

    stream.write_all(&protocol::logon_proof(&session.a_pub, &session.m1))?;
    let reply = read_message(&mut stream, "proof")?;
    // By this point the account is known to exist -- the challenge carried its
    // salt -- so the same refusal code means the proof did not match.
    let m2 = match protocol::parse_proof(&reply) {
        Err(protocol::Error::Refused(reason)) if reason.contains("unknown account") => {
            return Err(Error::WrongPassword)
        }
        other => other?,
    };

    // Verifying M2 is what makes this mutual: without it, anything that accepted
    // our proof could hand us a realm list pointing wherever it liked.
    if !session.verify_server(&m2) {
        return Err(Error::ServerProofFailed);
    }

    stream.write_all(&protocol::realm_list_request())?;
    let reply = read_message(&mut stream, "realm list")?;
    let realms = protocol::parse_realm_list(&reply)?;

    Ok(LoggedIn {
        session_key: session.key,
        realms,
    })
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

    let stream = TcpStream::connect_timeout(&resolved, timeout).map_err(|source| {
        Error::Connect {
            address: address.to_string(),
            source,
        }
    })?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    // Each message is small and the exchange is strictly turn-taking, so
    // Nagle's algorithm only adds latency.
    stream.set_nodelay(true)?;
    Ok(stream)
}

/// Reads one reply.
///
/// Messages carry no framing the reader can rely on before parsing, so this
/// takes whatever arrives in one read. Every reply in this exchange is sent as
/// a single write and comfortably fits one segment.
fn read_message(stream: &mut TcpStream, what: &'static str) -> Result<Vec<u8>, Error> {
    let mut buffer = vec![0u8; 4096];
    let read = stream.read(&mut buffer)?;
    if read == 0 {
        return Err(Error::Closed(what));
    }
    buffer.truncate(read);
    tracing::debug!("{what}: {read} bytes");
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::srp6::{interleave, KEY_LEN};
    use num_bigint::BigUint;
    use sha1::{Digest, Sha1};
    use std::net::TcpListener;

    fn sha1_of(parts: &[&[u8]]) -> [u8; 20] {
        let mut hasher = Sha1::new();
        for part in parts {
            hasher.update(part);
        }
        hasher.finalize().into()
    }

    /// A logon server good enough to authenticate against, written from the
    /// protocol rather than from the client, so a passing exchange is evidence
    /// the wire format is right.
    fn serve_once(listener: TcpListener, username: String, password: String, mode: Refuse) {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buffer = vec![0u8; 4096];

        // --- challenge
        let read = stream.read(&mut buffer).expect("read challenge");
        assert!(read > 0);
        assert_eq!(buffer[0], 0x00, "expected a logon challenge");

        let salt = [0x11u8; 32];
        let n = BigUint::from_bytes_le(&srp6::N_LE);
        let v = srp6::verifier(&username, &password, &salt);
        let b_private = BigUint::from_bytes_le(&[0x33u8; 19]);
        let b_public = ((BigUint::from(srp6::K) * &v)
            + BigUint::from(srp6::G).modpow(&b_private, &n))
            % &n;
        let mut b_le = b_public.to_bytes_le();
        b_le.resize(KEY_LEN, 0);

        if mode == Refuse::AtChallenge {
            // Unknown account: refused before any password is seen.
            stream.write_all(&[0x00, 0x00, 0x04]).expect("write");
            return;
        }

        let mut reply = vec![0x00u8, 0x00, 0x00];
        reply.extend_from_slice(&b_le);
        reply.push(1);
        reply.push(srp6::G);
        reply.push(KEY_LEN as u8);
        reply.extend_from_slice(&srp6::N_LE);
        reply.extend_from_slice(&salt);
        reply.extend_from_slice(&[0u8; 16]);
        reply.push(0);
        stream.write_all(&reply).expect("write challenge reply");

        // --- proof
        let read = stream.read(&mut buffer).expect("read proof");
        assert!(read >= 1 + KEY_LEN + 20);
        assert_eq!(buffer[0], 0x01);
        let a_pub: [u8; KEY_LEN] = buffer[1..1 + KEY_LEN].try_into().unwrap();
        let client_m1: [u8; 20] = buffer[1 + KEY_LEN..21 + KEY_LEN].try_into().unwrap();

        let b_le_arr: [u8; KEY_LEN] = b_le.clone().try_into().unwrap();
        let u = BigUint::from_bytes_le(&sha1_of(&[&a_pub, &b_le_arr]));
        let s = ((BigUint::from_bytes_le(&a_pub) * v.modpow(&u, &n)) % &n)
            .modpow(&b_private, &n);
        let mut s_le = s.to_bytes_le();
        s_le.resize(KEY_LEN, 0);
        let key = interleave(&s_le.try_into().unwrap());

        let hash_n = sha1_of(&[&srp6::N_LE]);
        let hash_g = sha1_of(&[&[srp6::G]]);
        let mut ng = [0u8; 20];
        for i in 0..20 {
            ng[i] = hash_n[i] ^ hash_g[i];
        }
        let expected_m1 = sha1_of(&[
            &ng,
            &sha1_of(&[username.to_uppercase().as_bytes()]),
            &salt,
            &a_pub,
            &b_le_arr,
            &key,
        ]);
        assert_eq!(client_m1, expected_m1, "client proof did not verify");

        if mode == Refuse::AtProof {
            // Same code as a missing account, but reached only once the salt
            // has been handed out -- so it can only mean a bad proof.
            stream.write_all(&[0x01, 0x04, 0x00, 0x00]).expect("write");
            return;
        }

        let m2 = sha1_of(&[&a_pub, &client_m1, &key]);
        let mut reply = vec![0x01u8, 0x00];
        reply.extend_from_slice(&m2);
        reply.extend_from_slice(&0u32.to_le_bytes()); // account flags
        reply.extend_from_slice(&0u32.to_le_bytes()); // survey id
        reply.extend_from_slice(&0u16.to_le_bytes());
        stream.write_all(&reply).expect("write proof reply");

        // --- realm list
        let read = stream.read(&mut buffer).expect("read realm request");
        assert!(read >= 1);
        assert_eq!(buffer[0], 0x10);

        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&1u16.to_le_bytes());
        body.push(0);
        body.push(0);
        body.push(0);
        body.extend_from_slice(b"Test Realm\0");
        body.extend_from_slice(b"127.0.0.1:8085\0");
        body.extend_from_slice(&0.25f32.to_le_bytes());
        body.push(2);
        body.push(1);
        body.push(1);
        body.extend_from_slice(&0u16.to_le_bytes());

        let mut reply = vec![0x10u8];
        reply.extend_from_slice(&(body.len() as u16).to_le_bytes());
        reply.extend_from_slice(&body);
        stream.write_all(&reply).expect("write realm list");
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Refuse {
        Never,
        AtChallenge,
        AtProof,
    }

    fn spawn_server(username: &str, password: &str, mode: Refuse) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (u, p) = (username.to_string(), password.to_string());
        std::thread::spawn(move || serve_once(listener, u, p, mode));
        port
    }

    /// The whole exchange over a real socket: challenge, proof, realm list.
    #[test]
    fn logs_in_against_a_server() {
        let port = spawn_server("tester", "hunter2", Refuse::Never);
        let result = login(
            "127.0.0.1",
            port,
            "tester",
            "hunter2",
            "enUS",
            Duration::from_secs(5),
        )
        .expect("login");

        assert_eq!(result.realms.len(), 1);
        assert_eq!(result.realms[0].name, "Test Realm");
        assert_eq!(result.realms[0].address, "127.0.0.1:8085");
        // The session key must be real, not left at its default.
        assert!(result.session_key.iter().any(|&b| b != 0));
    }

    /// The server sends the same refusal code for a missing account and a bad
    /// password. The stage is what separates them, and reporting one message
    /// for both makes a wrong password look like a missing account.
    #[test]
    fn refusals_are_reported_by_stage() {
        let port = spawn_server("tester", "hunter2", Refuse::AtChallenge);
        let err = login("127.0.0.1", port, "tester", "hunter2", "enUS", Duration::from_secs(5))
            .unwrap_err();
        assert!(matches!(err, Error::NoSuchAccount), "got: {err}");

        let port = spawn_server("tester", "hunter2", Refuse::AtProof);
        let err = login("127.0.0.1", port, "tester", "hunter2", "enUS", Duration::from_secs(5))
            .unwrap_err();
        assert!(matches!(err, Error::WrongPassword), "got: {err}");
    }

    /// An unreachable host fails quickly and says so, rather than hanging.
    #[test]
    fn an_unreachable_host_times_out() {
        // Port 1 on the loopback has nothing listening.
        let err = login(
            "127.0.0.1",
            1,
            "tester",
            "hunter2",
            "enUS",
            Duration::from_millis(400),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Connect { .. }), "got: {err}");
    }
}
