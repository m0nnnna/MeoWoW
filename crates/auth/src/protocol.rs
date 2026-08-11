//! Logon server packets.
//!
//! Three exchanges, strictly ordered: challenge, proof, realm list. Everything
//! is little-endian and nothing is length-prefixed at the top level, so a
//! reader has to know each message's shape to know how much to consume.

use crate::srp6::KEY_LEN;

/// Opcodes, as the first byte of every message in both directions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    LogonChallenge = 0x00,
    LogonProof = 0x01,
    RealmList = 0x10,
}

/// Why the server refused. Only the values a client can usefully distinguish
/// are named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthResult {
    Success,
    /// Wrong account name or password. The server deliberately does not say
    /// which.
    UnknownAccount,
    AccountBanned,
    AccountSuspended,
    AlreadyOnline,
    /// The client build is not one this realm accepts.
    VersionInvalid,
    VersionUpdate,
    ParentalControl,
    Other(u8),
}

impl AuthResult {
    pub fn from_code(code: u8) -> Self {
        match code {
            0x00 => Self::Success,
            0x04 | 0x05 => Self::UnknownAccount,
            0x03 => Self::AccountBanned,
            0x0C => Self::AccountSuspended,
            0x06 => Self::AlreadyOnline,
            0x09 => Self::VersionInvalid,
            0x0A => Self::VersionUpdate,
            0x0F => Self::ParentalControl,
            other => Self::Other(other),
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Success => "success".into(),
            Self::UnknownAccount => "unknown account or wrong password".into(),
            Self::AccountBanned => "account banned".into(),
            Self::AccountSuspended => "account suspended".into(),
            Self::AlreadyOnline => "account already online".into(),
            Self::VersionInvalid => "client build rejected".into(),
            Self::VersionUpdate => "client build needs updating".into(),
            Self::ParentalControl => "blocked by parental controls".into(),
            Self::Other(code) => format!("refused with code {code:#04x}"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{what}: expected at least {need} bytes, got {got}")]
    Truncated {
        what: &'static str,
        need: usize,
        got: usize,
    },
    #[error("expected opcode {expected:#04x}, got {got:#04x}")]
    UnexpectedOpcode { expected: u8, got: u8 },
    #[error("server refused: {0}")]
    Refused(String),
    #[error("server sent a {got}-byte prime; this client only supports {KEY_LEN}")]
    UnsupportedPrime { got: usize },
}

/// The client build this speaks for.
pub const BUILD: u16 = 12340;
pub const VERSION: [u8; 3] = [3, 3, 5];

/// Builds the opening challenge.
///
/// Several fields are four-character tags stored **reversed**, which is a
/// consequence of them being written as little-endian integers rather than as
/// strings.
pub fn logon_challenge(username: &str, locale: &str) -> Vec<u8> {
    let user = username.to_uppercase();
    let user = user.as_bytes();

    let mut body = Vec::with_capacity(30 + user.len());
    body.extend_from_slice(b"WoW\0");
    body.extend_from_slice(&VERSION);
    body.extend_from_slice(&BUILD.to_le_bytes());
    body.extend_from_slice(b"68x\0"); // "x86"
    body.extend_from_slice(b"niW\0"); // "Win"
    let mut tag: Vec<u8> = locale.bytes().take(4).collect();
    tag.resize(4, b' ');
    tag.reverse();
    body.extend_from_slice(&tag);
    body.extend_from_slice(&0u32.to_le_bytes()); // timezone bias
    body.extend_from_slice(&0u32.to_le_bytes()); // client IP, unused
    body.push(user.len() as u8);
    body.extend_from_slice(user);

    let mut packet = Vec::with_capacity(body.len() + 4);
    packet.push(Opcode::LogonChallenge as u8);
    // Protocol revision. Not an error code despite sharing the field.
    packet.push(8);
    packet.extend_from_slice(&(body.len() as u16).to_le_bytes());
    packet.extend_from_slice(&body);
    packet
}

/// What the server offers in reply to a challenge.
#[derive(Debug)]
pub struct Challenge {
    pub b_pub: [u8; KEY_LEN],
    pub salt: [u8; 32],
    /// Set when the realm wants a PIN or authenticator, which this client
    /// cannot answer.
    pub security_flags: u8,
}

pub fn parse_challenge(data: &[u8]) -> Result<Challenge, Error> {
    let need = |n: usize, what: &'static str| -> Result<(), Error> {
        if data.len() < n {
            Err(Error::Truncated {
                what,
                need: n,
                got: data.len(),
            })
        } else {
            Ok(())
        }
    };

    need(3, "challenge header")?;
    if data[0] != Opcode::LogonChallenge as u8 {
        return Err(Error::UnexpectedOpcode {
            expected: Opcode::LogonChallenge as u8,
            got: data[0],
        });
    }
    // data[1] is unused padding.
    let result = AuthResult::from_code(data[2]);
    if !result.is_success() {
        return Err(Error::Refused(result.describe()));
    }

    // B, then generator and prime with their own lengths, then the salt.
    let mut at = 3;
    need(at + KEY_LEN + 1, "challenge B")?;
    let b_pub: [u8; KEY_LEN] = data[at..at + KEY_LEN].try_into().unwrap();
    at += KEY_LEN;

    let g_len = data[at] as usize;
    at += 1;
    need(at + g_len + 1, "challenge generator")?;
    at += g_len;

    let n_len = data[at] as usize;
    at += 1;
    if n_len != KEY_LEN {
        return Err(Error::UnsupportedPrime { got: n_len });
    }
    need(at + n_len + 32 + 16 + 1, "challenge salt")?;
    at += n_len;

    let salt: [u8; 32] = data[at..at + 32].try_into().unwrap();
    at += 32;
    // Sixteen bytes of CRC salt follow, used only by the warden check.
    at += 16;
    let security_flags = data[at];

    Ok(Challenge {
        b_pub,
        salt,
        security_flags,
    })
}

/// Builds the proof message.
pub fn logon_proof(a_pub: &[u8; KEY_LEN], m1: &[u8; 20]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(75);
    packet.push(Opcode::LogonProof as u8);
    packet.extend_from_slice(a_pub);
    packet.extend_from_slice(m1);
    // CRC of the client binary. Servers that do not check it accept zeros.
    packet.extend_from_slice(&[0u8; 20]);
    packet.push(0); // key count
    packet.push(0); // security flags
    packet
}

/// Reads the server's proof, returning its `M2`.
pub fn parse_proof(data: &[u8]) -> Result<[u8; 20], Error> {
    if data.len() < 2 {
        return Err(Error::Truncated {
            what: "proof header",
            need: 2,
            got: data.len(),
        });
    }
    if data[0] != Opcode::LogonProof as u8 {
        return Err(Error::UnexpectedOpcode {
            expected: Opcode::LogonProof as u8,
            got: data[0],
        });
    }
    let result = AuthResult::from_code(data[1]);
    if !result.is_success() {
        return Err(Error::Refused(result.describe()));
    }
    if data.len() < 22 {
        return Err(Error::Truncated {
            what: "proof M2",
            need: 22,
            got: data.len(),
        });
    }
    Ok(data[2..22].try_into().unwrap())
}

pub fn realm_list_request() -> Vec<u8> {
    let mut packet = Vec::with_capacity(5);
    packet.push(Opcode::RealmList as u8);
    packet.extend_from_slice(&0u32.to_le_bytes());
    packet
}

#[derive(Debug, Clone)]
pub struct Realm {
    pub name: String,
    /// `host:port`, which is where the world server lives.
    pub address: String,
    pub kind: u8,
    pub locked: bool,
    pub flags: u8,
    pub population: f32,
    pub characters: u8,
    pub timezone: u8,
    pub id: u8,
}

impl Realm {
    /// Realms still being built, or otherwise not joinable.
    pub fn is_offline(&self) -> bool {
        self.flags & 0x02 != 0
    }
}

pub fn parse_realm_list(data: &[u8]) -> Result<Vec<Realm>, Error> {
    if data.len() < 9 {
        return Err(Error::Truncated {
            what: "realm list header",
            need: 9,
            got: data.len(),
        });
    }
    if data[0] != Opcode::RealmList as u8 {
        return Err(Error::UnexpectedOpcode {
            expected: Opcode::RealmList as u8,
            got: data[0],
        });
    }
    // uint16 size, uint32 unused, uint16 count.
    let count = u16::from_le_bytes([data[7], data[8]]) as usize;

    let mut at = 9;
    let mut realms = Vec::with_capacity(count);
    for index in 0..count {
        let truncated = |need: usize| Error::Truncated {
            what: "realm entry",
            need,
            got: data.len(),
        };
        if at + 3 > data.len() {
            return Err(truncated(at + 3));
        }
        let kind = data[at];
        let locked = data[at + 1] != 0;
        let flags = data[at + 2];
        at += 3;

        // Two NUL-terminated strings, whose lengths are not given anywhere.
        let mut read_string = || -> Result<String, Error> {
            let start = at;
            while at < data.len() && data[at] != 0 {
                at += 1;
            }
            if at >= data.len() {
                return Err(Error::Truncated {
                    what: "realm string",
                    need: at + 1,
                    got: data.len(),
                });
            }
            let text = String::from_utf8_lossy(&data[start..at]).into_owned();
            at += 1;
            Ok(text)
        };
        let name = read_string()?;
        let address = read_string()?;

        if at + 7 > data.len() {
            return Err(truncated(at + 7));
        }
        let population = f32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        let characters = data[at + 4];
        let timezone = data[at + 5];
        let id = data[at + 6];
        at += 7;

        // A realm running a different build appends its version.
        if flags & 0x04 != 0 {
            if at + 5 > data.len() {
                return Err(truncated(at + 5));
            }
            at += 5;
        }

        tracing::debug!("realm {index}: {name} at {address}");
        realms.push(Realm {
            name,
            address,
            kind,
            locked,
            flags,
            population,
            characters,
            timezone,
            id,
        });
    }
    Ok(realms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_declares_the_right_build() {
        let packet = logon_challenge("tester", "enUS");
        assert_eq!(packet[0], Opcode::LogonChallenge as u8);

        let size = u16::from_le_bytes([packet[2], packet[3]]) as usize;
        assert_eq!(size, packet.len() - 4, "declared size must cover the body");

        let body = &packet[4..];
        assert_eq!(&body[0..4], b"WoW\0");
        assert_eq!(&body[4..7], &VERSION);
        assert_eq!(u16::from_le_bytes([body[7], body[8]]), BUILD);
    }

    /// Tags are stored reversed, being little-endian integers rather than text.
    #[test]
    fn four_character_tags_are_reversed() {
        let packet = logon_challenge("tester", "enUS");
        let body = &packet[4..];
        assert_eq!(&body[9..13], b"68x\0", "platform");
        assert_eq!(&body[13..17], b"niW\0", "os");
        assert_eq!(&body[17..21], b"SUne", "locale");
    }

    /// The account name is upper-cased, matching how the verifier was stored.
    #[test]
    fn the_username_is_upper_cased() {
        let packet = logon_challenge("Tester", "enUS");
        let body = &packet[4..];
        let len = body[29] as usize;
        assert_eq!(&body[30..30 + len], b"TESTER");
    }

    fn challenge_bytes(result: u8) -> Vec<u8> {
        let mut data = vec![Opcode::LogonChallenge as u8, 0, result];
        data.extend_from_slice(&[0xAB; KEY_LEN]); // B
        data.push(1);
        data.push(7); // g
        data.push(KEY_LEN as u8);
        data.extend_from_slice(&crate::srp6::N_LE);
        data.extend_from_slice(&[0xCD; 32]); // salt
        data.extend_from_slice(&[0; 16]); // crc salt
        data.push(0); // security flags
        data
    }

    #[test]
    fn parses_a_successful_challenge() {
        let parsed = parse_challenge(&challenge_bytes(0)).unwrap();
        assert_eq!(parsed.b_pub, [0xAB; KEY_LEN]);
        assert_eq!(parsed.salt, [0xCD; 32]);
        assert_eq!(parsed.security_flags, 0);
    }

    /// A refusal must surface as an error rather than being read as data.
    #[test]
    fn a_refused_challenge_is_an_error() {
        let err = parse_challenge(&challenge_bytes(4)).unwrap_err();
        assert!(
            matches!(&err, Error::Refused(reason) if reason.contains("unknown account")),
            "got {err}"
        );
    }

    /// Truncation must be reported, not read past.
    #[test]
    fn a_short_challenge_is_rejected() {
        let full = challenge_bytes(0);
        for cut in [3, 10, 40, full.len() - 1] {
            assert!(
                parse_challenge(&full[..cut]).is_err(),
                "accepted a {cut}-byte challenge"
            );
        }
    }

    #[test]
    fn rejects_an_unexpected_prime_length() {
        let mut data = challenge_bytes(0);
        // The prime length byte sits after B, the generator length and g.
        data[3 + KEY_LEN + 2] = 16;
        assert!(matches!(
            parse_challenge(&data),
            Err(Error::UnsupportedPrime { got: 16 })
        ));
    }

    #[test]
    fn proof_round_trips() {
        let packet = logon_proof(&[1; KEY_LEN], &[2; 20]);
        assert_eq!(packet[0], Opcode::LogonProof as u8);
        assert_eq!(packet.len(), 1 + KEY_LEN + 20 + 20 + 2);

        let mut reply = vec![Opcode::LogonProof as u8, 0];
        reply.extend_from_slice(&[7u8; 20]);
        reply.extend_from_slice(&[0u8; 10]);
        assert_eq!(parse_proof(&reply).unwrap(), [7u8; 20]);
    }

    #[test]
    fn a_refused_proof_is_an_error() {
        let err = parse_proof(&[Opcode::LogonProof as u8, 4]).unwrap_err();
        assert!(matches!(err, Error::Refused(_)));
    }

    fn realm_list_bytes(realms: &[(&str, &str)]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&(realms.len() as u16).to_le_bytes());
        for (name, address) in realms {
            body.push(0); // kind
            body.push(0); // locked
            body.push(0); // flags
            body.extend_from_slice(name.as_bytes());
            body.push(0);
            body.extend_from_slice(address.as_bytes());
            body.push(0);
            body.extend_from_slice(&0.5f32.to_le_bytes());
            body.push(3); // characters
            body.push(1); // timezone
            body.push(1); // id
        }
        body.extend_from_slice(&0u16.to_le_bytes());

        let mut packet = vec![Opcode::RealmList as u8];
        packet.extend_from_slice(&(body.len() as u16).to_le_bytes());
        packet.extend_from_slice(&body);
        packet
    }

    /// Realm entries hold two NUL-terminated strings whose lengths appear
    /// nowhere, so the parser has to scan for them.
    #[test]
    fn parses_realms_with_variable_length_strings() {
        let data = realm_list_bytes(&[
            ("Nekos Farm", "192.168.1.7:8085"),
            ("A Much Longer Realm Name", "10.0.0.2:8085"),
        ]);
        let realms = parse_realm_list(&data).unwrap();

        assert_eq!(realms.len(), 2);
        assert_eq!(realms[0].name, "Nekos Farm");
        assert_eq!(realms[0].address, "192.168.1.7:8085");
        assert_eq!(realms[1].name, "A Much Longer Realm Name");
        assert_eq!(realms[1].characters, 3);
    }

    #[test]
    fn an_empty_realm_list_parses() {
        assert!(parse_realm_list(&realm_list_bytes(&[])).unwrap().is_empty());
    }

    /// A list claiming more realms than it contains must error rather than
    /// read past the buffer.
    #[test]
    fn a_truncated_realm_list_is_rejected() {
        let mut data = realm_list_bytes(&[("Realm", "host:8085")]);
        data[7] = 5; // claim five realms
        assert!(parse_realm_list(&data).is_err());
    }
}
