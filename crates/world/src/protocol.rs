//! World server packet framing and bodies.
//!
//! Framing here is asymmetric in two ways that both bite, so they are stated
//! up front:
//!
//! - **The two directions use different header shapes.** Client-to-server is a
//!   16-bit size and a *32-bit* opcode; server-to-client is a 16-bit size and a
//!   *16-bit* opcode. Six bytes out, four bytes in.
//! - **The size field is big-endian**, alone among every integer in this
//!   protocol. It also counts the opcode, not just the body, so it is not the
//!   payload length.
//!
//! On top of that, a server packet longer than 0x7FFF grows a third size byte,
//! flagged by the top bit of the first. That path is rare -- most packets are a
//! few dozen bytes -- which is exactly why it is worth handling deliberately
//! rather than discovering it the first time a large character list arrives.

use std::io::Write;

use auth::SESSION_KEY_LEN;
use sha1::{Digest, Sha1};

use crate::opcode::ClientOpcode;

/// The build this client claims. Must match what the logon server accepted.
pub const BUILD: u32 = 12340;

/// Bytes of header the server prefixes to a normal packet: size, then opcode.
pub const SERVER_HEADER_LEN: usize = 4;
/// The same, when the size needed a third byte.
pub const SERVER_HEADER_LEN_LARGE: usize = 5;

/// Upper bound on a single packet's payload.
///
/// A sanity limit, not a protocol constant: 3.3.5a's largest real packets are
/// well under a megabyte, and the three-byte size field can express eight.
///
/// It is worth being precise about what this does and does not catch, because
/// it is tempting to treat it as a check on the header cipher and it is not
/// one. A wrong key decrypts the size to a uniformly random number. Half the
/// time the flag bit lands set, the size is read from three bytes, and this
/// limit rejects it immediately. The other half it is read from two bytes,
/// cannot exceed 0x7FFF, and sails straight through.
///
/// So this bounds the damage -- no multi-megabyte allocation from a garbage
/// length -- but the thing that actually detects a mis-keyed cipher is that the
/// opcode never matches what was expected and the exchange fails. Both matter;
/// neither alone is sufficient.
pub const MAX_PACKET: usize = 1 << 20;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{what}: needed {need} bytes at offset {at}, packet holds {len}")]
    Truncated {
        what: &'static str,
        at: usize,
        need: usize,
        len: usize,
    },
    #[error("{what}: {got} trailing bytes left unread")]
    Trailing { what: &'static str, got: usize },
    #[error(
        "packet claims {got} bytes, over the {MAX_PACKET}-byte limit; \
         the header cipher is almost certainly keyed wrong"
    )]
    Oversized { got: usize },
    #[error("packet header claims {got} bytes, too few to hold even an opcode")]
    Undersized { got: usize },
    #[error("expected {expected}, got {}", crate::opcode::describe(*got))]
    UnexpectedOpcode { expected: &'static str, got: u16 },
    #[error("server refused the session: {0}")]
    Refused(String),
    #[error("string field {what} is not terminated")]
    UnterminatedString { what: &'static str },
    #[error("compression failed: {0}")]
    Compress(#[from] std::io::Error),
    #[error("compressed payload declared {declared} bytes but expanded to {got}")]
    CompressedLength { declared: usize, got: usize },
    #[error("unknown object update type {got}")]
    UnknownUpdateType { got: u8 },
    #[error("unknown object type {got}")]
    UnknownObjectType { got: u8 },
    #[error("{what}: target flags {flags:#x} are not a shape this parser has seen live")]
    UnsupportedSpellTarget { what: &'static str, flags: u32 },
    #[error(
        "SMSG_SPELL_GO: {count} miss entries, but no live capture has ever carried one -- \
         their shape is unconfirmed"
    )]
    UnconfirmedSpellMisses { count: u8 },
    #[error(
        "SMSG_ATTACKERSTATEUPDATE: {count} damage blocks. Every captured swing carried \
         exactly one, and the per-block width cannot be separated from the packet's tail \
         until one carries a different number -- see `combat::MeleeSwing`"
    )]
    UnconfirmedSwingDamageBlocks { count: u8 },
    #[error("SMSG_POWER_UPDATE: power type {got} is past the end of the power array")]
    UnknownPowerType { got: u8 },
    #[error(
        "SMSG_LIST_INVENTORY: {count} rows need {expected} bytes but the body has {got} left -- \
         the row count and the body disagree, so the header is not where this parser thinks"
    )]
    VendorRowCount {
        count: u8,
        expected: usize,
        got: usize,
    },

    /// The trainer row count is a `u32` rather than the vendor's `u8`, so a
    /// header read at the wrong offset asks for gigabytes. Checked before the
    /// allocation, not after it.
    #[error(
        "SMSG_TRAINER_LIST: {count} spells need at least {expected} bytes but the body has {got} \
         left -- the row count and the body disagree, so the header is not where this parser thinks"
    )]
    TrainerRowCount {
        count: u32,
        expected: usize,
        got: usize,
    },
}

/// A bounds-checked cursor over a packet body.
///
/// Every field in this protocol is read by offset, and a wrong offset parses
/// perfectly and returns nonsense -- the recurring failure of this whole
/// project. Reading through a cursor at least guarantees the offsets are
/// *consecutive*, which removes the arithmetic slips, and [`Reader::finish`]
/// catches a layout that came out the wrong total length.
pub struct Reader<'a> {
    data: &'a [u8],
    at: usize,
    what: &'static str,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], what: &'static str) -> Self {
        Self { data, at: 0, what }
    }

    /// Takes exactly `need` bytes, or fails.
    ///
    /// Public so that anything with a length-prefixed body of its own -- the
    /// quest cache file, for one -- gets the same running-out-of-input check
    /// the packet parsers do, rather than a hand-rolled slice that panics.
    pub fn take(&mut self, need: usize) -> Result<&'a [u8], Error> {
        if self.at + need > self.data.len() {
            return Err(Error::Truncated {
                what: self.what,
                at: self.at,
                need,
                len: self.data.len(),
            });
        }
        let slice = &self.data[self.at..self.at + need];
        self.at += need;
        Ok(slice)
    }

    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    /// Reads a fixed-width run of bytes that is never interpreted as a number.
    pub fn bytes<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        Ok(self.take(N)?.try_into().unwrap())
    }

    pub fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    /// A signed 32-bit word.
    ///
    /// Its own accessor rather than a cast at the call site, because the
    /// difference is not cosmetic: a mirror timer's rate is `-1` while the bar
    /// drains, and read unsigned that is 4,294,967,295 -- a drowning bar that
    /// appears to be refilling. The sign has to be in the *read*.
    pub fn i32(&mut self) -> Result<i32, Error> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn f32(&mut self) -> Result<f32, Error> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    /// Reads a NUL-terminated string, consuming the terminator.
    ///
    /// Lengths appear nowhere, so this has to scan. Names are Latin-1 on the
    /// wire for most locales, so decoding is lossy rather than fallible: a
    /// mangled name is better than a refused character list.
    pub fn cstring(&mut self) -> Result<String, Error> {
        let start = self.at;
        while self.at < self.data.len() && self.data[self.at] != 0 {
            self.at += 1;
        }
        if self.at >= self.data.len() {
            return Err(Error::UnterminatedString { what: self.what });
        }
        let text = String::from_utf8_lossy(&self.data[start..self.at]).into_owned();
        self.at += 1;
        Ok(text)
    }

    pub fn skip(&mut self, n: usize) -> Result<(), Error> {
        self.take(n).map(|_| ())
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.at
    }

    /// Takes everything left, for the tail of a packet whose fields are not
    /// confirmed.
    ///
    /// Deliberately *takes* rather than peeks, so [`Reader::finish`] still
    /// passes: a parser that keeps an unread tail is making a claim -- "I know
    /// this much and no more" -- and one that silently ignored the rest would
    /// be making no claim at all. Print the body, do not drop it.
    pub fn rest(&mut self) -> &'a [u8] {
        let from = self.at;
        self.at = self.data.len();
        &self.data[from..]
    }

    /// Asserts the body was consumed exactly.
    ///
    /// This is the check that catches a field of the wrong width: the values
    /// all parse, and the leftovers at the end are the only evidence.
    pub fn finish(self) -> Result<(), Error> {
        if self.remaining() != 0 {
            return Err(Error::Trailing {
                what: self.what,
                got: self.remaining(),
            });
        }
        Ok(())
    }
}

/// Builds a client packet: six bytes of header, then the body.
///
/// The header is returned unencrypted; the caller encrypts exactly the first
/// six bytes once the cipher is running. Keeping that split here means the
/// encryption boundary is a slice length in one place rather than a rule
/// spread across call sites.
pub fn client_packet(opcode: ClientOpcode, body: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(CLIENT_HEADER_LEN + body.len());
    // Size counts the opcode as well as the body, and is the one big-endian
    // field in the protocol.
    let size = (body.len() + 4) as u16;
    packet.extend_from_slice(&size.to_be_bytes());
    packet.extend_from_slice(&(opcode as u32).to_le_bytes());
    packet.extend_from_slice(body);
    packet
}

/// Bytes of header on a client packet: 16-bit size, 32-bit opcode.
pub const CLIENT_HEADER_LEN: usize = 6;

/// A decoded server header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerHeader {
    pub opcode: u16,
    /// Payload length, with the opcode already subtracted.
    pub body_len: usize,
}

/// Whether a server header carries a third size byte, from its first byte.
///
/// The caller must decrypt one byte, ask this, then decrypt the rest -- RC4
/// cannot be rewound, so the header length has to be known before the
/// remaining bytes are consumed.
pub fn server_header_len(first: u8) -> usize {
    if first & 0x80 != 0 {
        SERVER_HEADER_LEN_LARGE
    } else {
        SERVER_HEADER_LEN
    }
}

/// Decodes a decrypted server header.
pub fn parse_server_header(header: &[u8]) -> Result<ServerHeader, Error> {
    // The caller sizes the read from `server_header_len`, so a short slice here
    // is a caller bug rather than a malformed packet -- but this is public, and
    // indexing past the end would panic where every other parse error returns.
    let need = header
        .first()
        .map_or(SERVER_HEADER_LEN, |&first| server_header_len(first));
    if header.len() < need {
        return Err(Error::Truncated {
            what: "server header",
            at: 0,
            need,
            len: header.len(),
        });
    }

    let (size, opcode_at) = if header[0] & 0x80 != 0 {
        // Three size bytes, big-endian, with the flag bit masked out of the
        // first. The flag is not part of the number.
        let size = ((header[0] as usize & 0x7F) << 16)
            | ((header[1] as usize) << 8)
            | header[2] as usize;
        (size, 3)
    } else {
        (((header[0] as usize) << 8) | header[1] as usize, 2)
    };

    // Size counts the two opcode bytes, so anything below that is malformed
    // rather than merely empty.
    if size < 2 {
        return Err(Error::Undersized { got: size });
    }
    let body_len = size - 2;
    if body_len > MAX_PACKET {
        return Err(Error::Oversized { got: body_len });
    }

    let opcode = u16::from_le_bytes([header[opcode_at], header[opcode_at + 1]]);
    Ok(ServerHeader { opcode, body_len })
}

/// What `SMSG_AUTH_CHALLENGE` carries.
#[derive(Debug, Clone)]
pub struct AuthChallenge {
    /// Kept as opaque bytes rather than a `u32`.
    ///
    /// The seed is echoed into a hash and never interpreted as a number, so
    /// treating it as bytes means the wire order and the hash order cannot
    /// disagree -- which is otherwise an easy and completely silent bug.
    pub server_seed: [u8; 4],
}

pub fn parse_auth_challenge(body: &[u8]) -> Result<AuthChallenge, Error> {
    let mut reader = Reader::new(body, "SMSG_AUTH_CHALLENGE");
    // A leading 1, whose purpose is not documented anywhere useful.
    let _one = reader.u32()?;
    let server_seed = reader.bytes::<4>()?;
    // Two further sixteen-byte random numbers. 3.3.5a's client reads them and
    // does nothing with them -- they only acquire a use in Cataclysm, where the
    // connection handshake needs a second seed. Skipped, but not ignorable:
    // getting the count wrong is what `finish` below is here to catch, and it
    // did catch it. The first attempt skipped one of them and left sixteen
    // bytes on the floor.
    reader.skip(32)?;
    reader.finish()?;
    Ok(AuthChallenge { server_seed })
}

/// The proof that we hold the session key the logon server issued.
///
/// `SHA1(account | 0 | client_seed | server_seed | session_key)`. No length
/// prefixes and no separators, so every field's width has to be exactly right;
/// the account name is hashed without its terminating NUL even though the
/// packet carries one.
pub fn auth_digest(
    account: &str,
    client_seed: &[u8; 4],
    server_seed: &[u8; 4],
    session_key: &[u8; SESSION_KEY_LEN],
) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(account.to_uppercase().as_bytes());
    hasher.update(0u32.to_le_bytes());
    hasher.update(client_seed);
    hasher.update(server_seed);
    hasher.update(session_key);
    hasher.finalize().into()
}

/// Builds the `CMSG_AUTH_SESSION` body.
///
/// Sent with a *plaintext* header: the cipher is keyed by this exchange, so it
/// cannot yet be running when this goes out. Everything after it is encrypted.
pub fn auth_session(
    account: &str,
    realm_id: u32,
    client_seed: &[u8; 4],
    server_seed: &[u8; 4],
    session_key: &[u8; SESSION_KEY_LEN],
) -> Result<Vec<u8>, Error> {
    let account = account.to_uppercase();
    let digest = auth_digest(&account, client_seed, server_seed, session_key);

    let mut body = Vec::with_capacity(96);
    body.extend_from_slice(&BUILD.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes()); // login server id
    body.extend_from_slice(account.as_bytes());
    body.push(0);
    body.extend_from_slice(&0u32.to_le_bytes()); // login server type
    body.extend_from_slice(client_seed);
    body.extend_from_slice(&0u32.to_le_bytes()); // region id
    body.extend_from_slice(&0u32.to_le_bytes()); // battlegroup id
    body.extend_from_slice(&realm_id.to_le_bytes());
    body.extend_from_slice(&0u64.to_le_bytes()); // DoS response, unchecked here
    body.extend_from_slice(&digest);
    body.extend_from_slice(&addon_block()?);
    Ok(body)
}

/// The addon manifest: an uncompressed length, then a zlib stream.
///
/// A real client lists every loaded addon so the server can flag which are
/// blocked. This one loads none, but still sends a well-formed empty manifest
/// rather than omitting the field: servers differ in how gracefully they take a
/// zero-length block, and an empty list is unambiguous everywhere.
fn addon_block() -> Result<Vec<u8>, Error> {
    // Count, then entries, then the client's clock. With no addons that is two
    // zeroed words.
    let mut manifest = Vec::new();
    manifest.extend_from_slice(&0u32.to_le_bytes()); // addon count
    manifest.extend_from_slice(&0u32.to_le_bytes()); // current time

    let mut encoder =
        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&manifest)?;
    let compressed = encoder.finish()?;

    let mut block = Vec::with_capacity(4 + compressed.len());
    // The length is of the *decompressed* manifest; the server sizes its
    // output buffer from it.
    block.extend_from_slice(&(manifest.len() as u32).to_le_bytes());
    block.extend_from_slice(&compressed);
    Ok(block)
}

/// The server's verdict on the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthResponse {
    Ok {
        /// 0 for classic, 1 for Burning Crusade, 2 for Wrath.
        expansion: u8,
    },
    /// Placed in the login queue; the server will send another response later.
    Queued { position: u32 },
    Refused { code: u8 },
}

/// Names the response codes a client can act on differently.
///
/// The interesting cases are the ones a player would need a different sentence
/// for: a wrong build, a full server and a banned account all fail, but for
/// reasons that call for different responses.
pub fn describe_response(code: u8) -> &'static str {
    match code {
        0x0C => "ok",
        0x0D => "authentication failed",
        0x0E => "rejected",
        0x0F => "bad server proof",
        0x10 => "server unavailable",
        0x11 => "internal server error",
        0x14 => "client build rejected",
        0x15 => "unknown account",
        0x16 => "incorrect password",
        0x17 => "session expired",
        0x18 => "server shutting down",
        0x19 => "already logging in",
        0x1A => "logon server not found",
        0x1C => "account banned",
        0x1D => "account already online",
        0x1E => "account out of time",
        0x1F => "database busy",
        0x20 => "account suspended",
        0x21 => "blocked by parental controls",
        _ => "refused for an unrecognised reason",
    }
}

pub fn parse_auth_response(body: &[u8]) -> Result<AuthResponse, Error> {
    let mut reader = Reader::new(body, "SMSG_AUTH_RESPONSE");
    let code = reader.u8()?;
    match code {
        0x0C => {
            reader.skip(4)?; // billing time remaining
            reader.skip(1)?; // billing flags
            reader.skip(4)?; // billing time rested
            let expansion = reader.u8()?;
            reader.finish()?;
            Ok(AuthResponse::Ok { expansion })
        }
        0x1B => {
            let position = reader.u32()?;
            // A trailing "free character migration" flag; not acted on.
            reader.skip(1)?;
            reader.finish()?;
            Ok(AuthResponse::Queued { position })
        }
        // A refusal carries no further fields, and some servers append padding
        // anyway, so the body is not checked for exact length here.
        code => Ok(AuthResponse::Refused { code }),
    }
}

/// How a character should look at creation.
///
/// The appearance fields are indices into the race's options in
/// `CharSections.dbc`, so their valid ranges depend on race and gender and the
/// server validates them. Zero is in range for every combination.
#[derive(Debug, Clone, Copy)]
pub struct Appearance {
    pub race: u8,
    pub class: u8,
    pub gender: u8,
    pub skin: u8,
    pub face: u8,
    pub hair_style: u8,
    pub hair_color: u8,
    pub facial_hair: u8,
}

impl Appearance {
    /// A combination valid on every 3.3.5a realm, for when the caller only
    /// wants *a* character rather than a particular one.
    pub fn human_warrior() -> Self {
        Self {
            race: 1,
            class: 1,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        }
    }
}

pub fn char_create(name: &str, look: &Appearance) -> Vec<u8> {
    let mut body = Vec::with_capacity(name.len() + 11);
    body.extend_from_slice(name.as_bytes());
    body.push(0);
    body.push(look.race);
    body.push(look.class);
    body.push(look.gender);
    body.push(look.skin);
    body.push(look.face);
    body.push(look.hair_style);
    body.push(look.hair_color);
    body.push(look.facial_hair);
    // Outfit id, which picks the starting gear set. Always zero from a real
    // client.
    body.push(0);
    body
}

pub fn char_delete(guid: u64) -> Vec<u8> {
    guid.to_le_bytes().to_vec()
}

/// Reads a one-byte result code, as both character-management replies use.
pub fn parse_result_code(body: &[u8], what: &'static str) -> Result<u8, Error> {
    let mut reader = Reader::new(body, what);
    reader.u8()
}

/// Creation succeeded.
///
/// These codes are positions in one long shared enum that also covers the
/// logon results, the realm list and account creation, so they cannot be
/// derived from anything local -- an extra entry anywhere earlier shifts every
/// later value. This client had them one too low at first, and a successful
/// creation therefore reported "server error" while the character appeared on
/// the realm regardless. Both constants below are confirmed against a live
/// server rather than counted off a list.
pub const CHAR_CREATE_SUCCESS: u8 = 0x2F;
/// Deletion succeeded.
pub const CHAR_DELETE_SUCCESS: u8 = 0x47;

/// Names the character-management outcomes worth distinguishing.
///
/// The full table runs to some thirty codes covering faction changes, arena
/// teams and guild membership; only the ones a bare test client can actually
/// provoke are named, and the rest report their number rather than being
/// guessed at.
pub fn describe_char_result(code: u8) -> &'static str {
    match code {
        0x2E => "creation in progress",
        CHAR_CREATE_SUCCESS => "created",
        0x30 => "server error",
        0x31 => "creation failed",
        0x32 => "that name is already taken",
        0x33 => "character creation is disabled",
        0x34 => "the realm does not allow both factions on one account",
        0x35 => "the realm is full",
        0x36 => "the account is at its character limit",
        0x39 => "the account lacks the required expansion",
        0x3A => "that class needs an expansion the account lacks",
        // Confirmed live: a death knight was refused with this until the
        // account had a level 55 character, which is the documented rule.
        0x3B => "that class needs an existing level 55 character",
        0x46 => "deletion in progress",
        CHAR_DELETE_SUCCESS => "deleted",
        0x48 => "deletion failed",
        _ => "an unrecognised outcome",
    }
}

/// Asks to enter the world as one character.
pub fn player_login(guid: u64) -> Vec<u8> {
    guid.to_le_bytes().to_vec()
}

/// Where the server says the character actually is.
///
/// This is the first thing that ties the protocol to the renderer: the map and
/// position here are the same coordinate space the ADT terrain is built in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldPosition {
    pub map: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub orientation: f32,
}

pub fn parse_login_verify_world(body: &[u8]) -> Result<WorldPosition, Error> {
    let mut reader = Reader::new(body, "SMSG_LOGIN_VERIFY_WORLD");
    let position = WorldPosition {
        map: reader.u32()?,
        x: reader.f32()?,
        y: reader.f32()?,
        z: reader.f32()?,
        orientation: reader.f32()?,
    };
    reader.finish()?;
    Ok(position)
}

/// The keepalive. `latency` is the client's own estimate, in milliseconds.
///
/// Not optional on a long-lived connection: a server that stops hearing pings
/// drops the session, and the drop looks exactly like a parser bug that ate the
/// stream.
pub fn ping(sequence: u32, latency: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(8);
    body.extend_from_slice(&sequence.to_le_bytes());
    body.extend_from_slice(&latency.to_le_bytes());
    body
}

pub fn parse_pong(body: &[u8]) -> Result<u32, Error> {
    let mut reader = Reader::new(body, "SMSG_PONG");
    let sequence = reader.u32()?;
    reader.finish()?;
    Ok(sequence)
}

/// The server's clock-sync poll, which must be answered to stay connected.
pub fn parse_time_sync_req(body: &[u8]) -> Result<u32, Error> {
    let mut reader = Reader::new(body, "SMSG_TIME_SYNC_REQ");
    let counter = reader.u32()?;
    reader.finish()?;
    Ok(counter)
}

/// Answers a clock-sync poll. `ticks` is the client's uptime in milliseconds;
/// the server only checks that it advances sensibly.
pub fn time_sync_resp(counter: u32, ticks: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(8);
    body.extend_from_slice(&counter.to_le_bytes());
    body.extend_from_slice(&ticks.to_le_bytes());
    body
}

/// Builds a movement packet body: the mover's guid, then its state.
///
/// The leading packed guid is easy to leave out, because the client is
/// obviously talking about itself. 3.3.5a added it so that a player controlling
/// something else -- a vehicle, a mind-controlled creature -- can say *which*
/// thing moved. Omitting it does not fail cleanly: the server reads the first
/// bytes of the movement flags as a guid and everything after shifts.
pub fn movement(mover: u64, info: &crate::movement::MovementInfo) -> Vec<u8> {
    let mut body = Vec::with_capacity(40);
    crate::update::write_packed_guid(mover, &mut body);
    info.write(&mut body);
    body
}

/// Reads a movement packet relayed from another mover.
pub fn parse_movement(
    body: &[u8],
) -> Result<(u64, crate::movement::MovementInfo), Error> {
    let mut reader = Reader::new(body, "MSG_MOVE_*");
    let mover = crate::update::read_packed_guid(&mut reader)?;
    let info = crate::movement::MovementInfo::read(&mut reader)?;
    reader.finish()?;
    Ok((mover, info))
}

/// A teleport within the current map, which the client must acknowledge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Teleport {
    /// Who is being moved. Always this client -- the server does not send
    /// these for anybody else -- but checked rather than assumed, because
    /// acknowledging someone else's teleport with our own guid is the kind of
    /// write that gets read as a different valid request.
    pub mover: u64,
    /// The server's ordering counter, echoed straight back.
    pub counter: u32,
    /// Where the character now is. The reason this is worth parsing rather
    /// than only replying to: a client that acks without moving is a client
    /// whose own idea of its position is now wrong by however far it was sent.
    pub info: crate::movement::MovementInfo,
}

/// Reads `MSG_MOVE_TELEPORT_ACK` as the server sends it:
/// `{packed guid, u32 counter, MovementInfo}`.
pub fn parse_teleport(body: &[u8]) -> Result<Teleport, Error> {
    let mut reader = Reader::new(body, "MSG_MOVE_TELEPORT_ACK");
    let mover = crate::update::read_packed_guid(&mut reader)?;
    let counter = reader.u32()?;
    let info = crate::movement::MovementInfo::read(&mut reader)?;
    reader.finish()?;
    Ok(Teleport {
        mover,
        counter,
        info,
    })
}

/// Equipment slots reported per character.
///
/// Nineteen worn slots -- head through tabard, including both weapons and the
/// ranged slot -- followed by the four bag slots, which the character screen
/// needs because bags are visible on the model.
///
/// The count was wrong at first, and the way it was wrong is worth recording:
/// twenty slots parsed every field of a real character without complaint and
/// left twenty-seven bytes over, because three slots at nine bytes each is
/// exactly the kind of error that no individual field can detect. Only
/// [`Reader::finish`] caught it.
pub const EQUIPMENT_SLOTS: usize = 23;

/// One slot's appearance, which is all the character screen needs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Equipment {
    pub display_id: u32,
    pub inventory_type: u8,
    pub enchant_aura: u32,
}

#[derive(Debug, Clone)]
pub struct Character {
    pub guid: u64,
    pub name: String,
    pub race: u8,
    pub class: u8,
    pub gender: u8,
    pub skin: u8,
    pub face: u8,
    pub hair_style: u8,
    pub hair_color: u8,
    pub facial_hair: u8,
    pub level: u8,
    pub zone: u32,
    pub map: u32,
    pub position: [f32; 3],
    pub guild_id: u32,
    pub flags: u32,
    /// Pending appearance change, faction change and similar.
    pub customize_flags: u32,
    pub first_login: bool,
    pub pet_display_id: u32,
    pub pet_level: u32,
    pub pet_family: u32,
    pub equipment: [Equipment; EQUIPMENT_SLOTS],
}

impl Character {
    /// Dead and not yet resurrected.
    pub fn is_ghost(&self) -> bool {
        self.flags & 0x2000 != 0
    }

    /// The server will demand a rename before this character can be played.
    pub fn needs_rename(&self) -> bool {
        self.flags & 0x4000 != 0
    }

    pub fn has_pet(&self) -> bool {
        self.pet_display_id != 0
    }
}

pub fn parse_char_enum(body: &[u8]) -> Result<Vec<Character>, Error> {
    let mut reader = Reader::new(body, "SMSG_CHAR_ENUM");
    let count = reader.u8()? as usize;

    let mut characters = Vec::with_capacity(count);
    for _ in 0..count {
        let guid = reader.u64()?;
        let name = reader.cstring()?;
        let race = reader.u8()?;
        let class = reader.u8()?;
        let gender = reader.u8()?;
        let skin = reader.u8()?;
        let face = reader.u8()?;
        let hair_style = reader.u8()?;
        let hair_color = reader.u8()?;
        let facial_hair = reader.u8()?;
        let level = reader.u8()?;
        let zone = reader.u32()?;
        let map = reader.u32()?;
        let position = [reader.f32()?, reader.f32()?, reader.f32()?];
        let guild_id = reader.u32()?;
        let flags = reader.u32()?;
        let customize_flags = reader.u32()?;
        let first_login = reader.u8()? != 0;
        let pet_display_id = reader.u32()?;
        let pet_level = reader.u32()?;
        let pet_family = reader.u32()?;

        let mut equipment = [Equipment::default(); EQUIPMENT_SLOTS];
        for slot in equipment.iter_mut() {
            slot.display_id = reader.u32()?;
            slot.inventory_type = reader.u8()?;
            slot.enchant_aura = reader.u32()?;
        }

        characters.push(Character {
            guid,
            name,
            race,
            class,
            gender,
            skin,
            face,
            hair_style,
            hair_color,
            facial_hair,
            level,
            zone,
            map,
            position,
            guild_id,
            flags,
            customize_flags,
            first_login,
            pet_display_id,
            pet_level,
            pet_family,
            equipment,
        });
    }

    // The equipment block is by far the largest part of an entry and the
    // easiest to get wrong by one slot. Consuming the body exactly is what
    // proves the count is right -- a nineteen-slot read would leave nine bytes
    // per character behind, and every field would still have parsed.
    reader.finish()?;
    Ok(characters)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_header_is_big_endian_size_and_wide_opcode() {
        let packet = client_packet(ClientOpcode::CharEnum, &[]);
        assert_eq!(packet.len(), CLIENT_HEADER_LEN);
        // Four, because the size counts the opcode -- and big-endian, so the
        // low byte is last.
        assert_eq!(&packet[0..2], &[0x00, 0x04]);
        assert_eq!(&packet[2..6], &0x37u32.to_le_bytes());
    }

    #[test]
    fn the_client_size_covers_the_body() {
        let packet = client_packet(ClientOpcode::AuthSession, &[9u8; 300]);
        let size = u16::from_be_bytes([packet[0], packet[1]]) as usize;
        assert_eq!(size, 304);
        assert_eq!(packet.len(), size + 2);
    }

    fn server_header(size: usize, opcode: u16) -> Vec<u8> {
        let mut header = Vec::new();
        if size > 0x7FFF {
            header.push(0x80 | (size >> 16) as u8);
            header.push((size >> 8) as u8);
            header.push(size as u8);
        } else {
            header.push((size >> 8) as u8);
            header.push(size as u8);
        }
        header.extend_from_slice(&opcode.to_le_bytes());
        header
    }

    #[test]
    fn a_short_server_header_parses() {
        let header = server_header(2 + 24, crate::opcode::server::AUTH_CHALLENGE);
        assert_eq!(header.len(), SERVER_HEADER_LEN);
        assert_eq!(server_header_len(header[0]), SERVER_HEADER_LEN);

        let parsed = parse_server_header(&header).unwrap();
        assert_eq!(parsed.opcode, 0x01EC);
        assert_eq!(parsed.body_len, 24);
    }

    /// Over 0x7FFF the size grows a third byte, flagged in the top bit of the
    /// first. A reader that always took two bytes would take the flagged byte
    /// as part of the number and land on a wildly wrong length.
    #[test]
    fn a_large_server_header_grows_a_third_size_byte() {
        let body = 0x12345;
        let header = server_header(body + 2, 0x003B);
        assert_eq!(header.len(), SERVER_HEADER_LEN_LARGE);
        assert_eq!(server_header_len(header[0]), SERVER_HEADER_LEN_LARGE);

        let parsed = parse_server_header(&header).unwrap();
        assert_eq!(parsed.opcode, 0x003B);
        assert_eq!(parsed.body_len, body, "the flag bit leaked into the size");
    }

    /// The boundary itself: 0x7FFF stays short, 0x8000 goes long.
    #[test]
    fn the_large_packet_boundary_is_at_0x8000() {
        assert_eq!(server_header_len(server_header(0x7FFF, 1)[0]), SERVER_HEADER_LEN);
        assert_eq!(
            server_header_len(server_header(0x8000, 1)[0]),
            SERVER_HEADER_LEN_LARGE
        );
    }

    /// A size past the limit must fail loudly rather than have its bytes
    /// awaited and its buffer allocated.
    #[test]
    fn an_absurd_size_is_rejected_rather_than_awaited() {
        let header = server_header(0x7F_FFFF, 0x003B);
        assert!(matches!(
            parse_server_header(&header),
            Err(Error::Oversized { .. })
        ));
    }

    /// The limit must sit above every packet a real server sends, or a large
    /// but legitimate one would be refused. A two-byte size cannot reach it at
    /// all, which is the half of the space this check provably does not cover.
    #[test]
    fn the_limit_clears_every_two_byte_size() {
        assert!(
            MAX_PACKET > 0x7FFF,
            "the limit would reject packets with a short header"
        );
        let parsed = parse_server_header(&server_header(0x7FFF, 0x003B)).unwrap();
        assert_eq!(parsed.body_len, 0x7FFD);
    }

    /// A size below two cannot even hold the opcode it claims to count.
    #[test]
    fn a_size_too_small_for_an_opcode_is_rejected() {
        assert!(matches!(
            parse_server_header(&[0x00, 0x01, 0x3B, 0x00]),
            Err(Error::Undersized { got: 1 })
        ));
    }

    fn challenge_body(seed: [u8; 4]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&seed);
        body.extend_from_slice(&[0xAB; 16]);
        body.extend_from_slice(&[0xCD; 16]);
        body
    }

    /// Pinned against the live server: a real 3.3.5a challenge is forty bytes,
    /// not the twenty-four a single trailing random number would give.
    #[test]
    fn the_challenge_is_forty_bytes() {
        assert_eq!(challenge_body([0; 4]).len(), 40);
        assert!(parse_auth_challenge(&challenge_body([0; 4])).is_ok());
    }

    #[test]
    fn the_challenge_seed_survives_verbatim() {
        let parsed = parse_auth_challenge(&challenge_body([0xDE, 0xAD, 0xBE, 0xEF])).unwrap();
        assert_eq!(parsed.server_seed, [0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn a_truncated_challenge_is_rejected() {
        let full = challenge_body([1, 2, 3, 4]);
        for cut in [0, 4, 8, full.len() - 1] {
            assert!(
                parse_auth_challenge(&full[..cut]).is_err(),
                "accepted a {cut}-byte challenge"
            );
        }
    }

    /// A longer challenge than expected means the layout is wrong, not that
    /// there is extra to ignore.
    #[test]
    fn an_overlong_challenge_is_rejected() {
        let mut body = challenge_body([1, 2, 3, 4]);
        body.push(0);
        assert!(matches!(
            parse_auth_challenge(&body),
            Err(Error::Trailing { .. })
        ));
    }

    /// The digest is unsalted concatenation, so every input must actually reach
    /// it. Vary each in turn and require the result to move.
    #[test]
    fn every_digest_input_changes_the_digest() {
        let key = [0x5Au8; SESSION_KEY_LEN];
        let base = auth_digest("ACCOUNT33", &[1, 2, 3, 4], &[5, 6, 7, 8], &key);

        assert_ne!(base, auth_digest("OTHER", &[1, 2, 3, 4], &[5, 6, 7, 8], &key));
        assert_ne!(
            base,
            auth_digest("ACCOUNT33", &[9, 2, 3, 4], &[5, 6, 7, 8], &key)
        );
        assert_ne!(
            base,
            auth_digest("ACCOUNT33", &[1, 2, 3, 4], &[9, 6, 7, 8], &key)
        );
        assert_ne!(
            base,
            auth_digest("ACCOUNT33", &[1, 2, 3, 4], &[5, 6, 7, 8], &[0u8; SESSION_KEY_LEN])
        );
    }

    /// Client and server seeds are hashed in a fixed order. Swapping them is a
    /// mistake that only a live server would catch, so pin it here.
    #[test]
    fn the_seed_order_matters() {
        let key = [0x5Au8; SESSION_KEY_LEN];
        assert_ne!(
            auth_digest("A", &[1, 2, 3, 4], &[5, 6, 7, 8], &key),
            auth_digest("A", &[5, 6, 7, 8], &[1, 2, 3, 4], &key),
        );
    }

    /// The account name is upper-cased, matching the logon stage.
    #[test]
    fn the_account_is_upper_cased_in_the_digest() {
        let key = [0x11u8; SESSION_KEY_LEN];
        assert_eq!(
            auth_digest("account33", &[1; 4], &[2; 4], &key),
            auth_digest("ACCOUNT33", &[1; 4], &[2; 4], &key),
        );
    }

    #[test]
    fn the_session_body_carries_the_build_and_account() {
        let body = auth_session("account33", 1, &[1; 4], &[2; 4], &[3u8; SESSION_KEY_LEN]).unwrap();
        assert_eq!(&body[0..4], &BUILD.to_le_bytes());
        assert_eq!(&body[8..18], b"ACCOUNT33\0");
    }

    /// The addon block's length field describes the manifest before
    /// compression, not the bytes on the wire. Confusing the two makes the
    /// server allocate the wrong buffer and drop the whole block.
    #[test]
    fn the_addon_block_declares_its_uncompressed_length() {
        let block = addon_block().unwrap();
        let declared = u32::from_le_bytes(block[0..4].try_into().unwrap()) as usize;
        assert_eq!(declared, 8, "an empty manifest is a count and a timestamp");
        assert_ne!(declared, block.len() - 4, "declared the compressed length");

        // And it must really be zlib: the stream starts with the 0x78 CMF byte.
        assert_eq!(block[4], 0x78);
    }

    #[test]
    fn an_accepted_session_reports_its_expansion() {
        let mut body = vec![0x0C];
        body.extend_from_slice(&0u32.to_le_bytes());
        body.push(0);
        body.extend_from_slice(&0u32.to_le_bytes());
        body.push(2);
        assert_eq!(
            parse_auth_response(&body).unwrap(),
            AuthResponse::Ok { expansion: 2 }
        );
    }

    #[test]
    fn a_queued_session_reports_its_position() {
        let mut body = vec![0x1B];
        body.extend_from_slice(&42u32.to_le_bytes());
        body.push(0);
        assert_eq!(
            parse_auth_response(&body).unwrap(),
            AuthResponse::Queued { position: 42 }
        );
    }

    /// A refusal must not be read as success, whatever the reason.
    #[test]
    fn refusals_keep_their_code() {
        for code in [0x0D, 0x14, 0x1C, 0x1D, 0x21] {
            assert_eq!(
                parse_auth_response(&[code]).unwrap(),
                AuthResponse::Refused { code }
            );
        }
        assert_eq!(describe_response(0x14), "client build rejected");
        assert_eq!(describe_response(0x1D), "account already online");
    }

    /// A movement packet must survive its own parser, guid included.
    ///
    /// The leading packed guid is the part worth pinning. It is easy to omit --
    /// the client is obviously talking about itself -- and omitting it does not
    /// fail cleanly: the server reads the first bytes of the movement flags as
    /// a guid and every field after shifts, producing a valid-looking move to
    /// somewhere else entirely.
    #[test]
    fn a_movement_packet_round_trips_with_its_mover() {
        use crate::movement::MovementInfo;
        use crate::update::Position;

        let info = MovementInfo {
            flags: crate::update::movement_flags::FORWARD,
            time: 12_345,
            position: Position {
                x: -8949.95,
                y: -132.49,
                z: 83.53,
                orientation: 3.14,
            },
            ..MovementInfo::default()
        };

        let body = movement(0x32, &info);
        let (mover, parsed) = parse_movement(&body).unwrap();
        assert_eq!(mover, 0x32);
        assert_eq!(parsed, info);

        // And the guid really is on the wire ahead of the state: dropping it
        // must not still parse, or the test proves nothing.
        assert!(
            parse_movement(&body[1..]).is_err(),
            "the packet parsed without its guid, so the guid is not load-bearing"
        );
    }

    /// A large guid packs to more than one byte, so the offset of everything
    /// after it moves. Both ends must agree about that.
    #[test]
    fn a_wide_mover_guid_shifts_the_body() {
        use crate::movement::MovementInfo;

        let info = MovementInfo::standing(Default::default(), 7);
        let narrow = movement(0x32, &info);
        let wide = movement(0xF130_0000_3370_0BA9, &info);
        assert!(wide.len() > narrow.len());

        let (mover, parsed) = parse_movement(&wide).unwrap();
        assert_eq!(mover, 0xF130_0000_3370_0BA9);
        assert_eq!(parsed, info);
    }

    #[test]
    fn a_creation_request_carries_the_name_and_appearance() {
        let body = char_create("Testwolf", &Appearance::human_warrior());
        assert_eq!(&body[0..9], b"Testwolf\0");
        // Race, class, gender, five appearance indices, outfit id.
        assert_eq!(body.len(), 9 + 9);
        assert_eq!(body[9], 1, "race");
        assert_eq!(body[10], 1, "class");
    }

    #[test]
    fn a_deletion_request_is_a_bare_guid() {
        assert_eq!(char_delete(0x1122334455667788), 0x1122334455667788u64.to_le_bytes());
    }

    /// These sit in one long shared enum, so they cannot be checked against
    /// anything local -- the values below were read back from a live server.
    /// Pinning them stops a later "tidy-up" from renumbering them by counting.
    #[test]
    fn the_character_result_codes_are_the_confirmed_values() {
        assert_eq!(CHAR_CREATE_SUCCESS, 0x2F);
        assert_eq!(CHAR_DELETE_SUCCESS, 0x47);
        assert_eq!(describe_char_result(CHAR_CREATE_SUCCESS), "created");
        assert_eq!(describe_char_result(CHAR_DELETE_SUCCESS), "deleted");
        assert_eq!(
            describe_char_result(0x3B),
            "that class needs an existing level 55 character"
        );
        assert_eq!(describe_char_result(0xFE), "an unrecognised outcome");
    }

    #[test]
    fn an_empty_result_packet_is_rejected() {
        assert!(parse_result_code(&[], "SMSG_CHAR_CREATE").is_err());
    }

    fn char_entry(name: &str, level: u8, pet_family: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0x1122334455667788u64.to_le_bytes());
        body.extend_from_slice(name.as_bytes());
        body.push(0);
        body.extend_from_slice(&[1, 2, 0, 3, 4, 5, 6, 7]); // race..facial hair
        body.push(level);
        body.extend_from_slice(&12u32.to_le_bytes()); // zone
        body.extend_from_slice(&0u32.to_le_bytes()); // map
        body.extend_from_slice(&(-8949.95f32).to_le_bytes());
        body.extend_from_slice(&(-132.49f32).to_le_bytes());
        body.extend_from_slice(&83.53f32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes()); // guild
        body.extend_from_slice(&0u32.to_le_bytes()); // flags
        body.extend_from_slice(&0u32.to_le_bytes()); // customize flags
        body.push(1); // first login
        body.extend_from_slice(&0u32.to_le_bytes()); // pet display
        body.extend_from_slice(&0u32.to_le_bytes()); // pet level
        body.extend_from_slice(&pet_family.to_le_bytes());
        for slot in 0..EQUIPMENT_SLOTS {
            body.extend_from_slice(&(slot as u32).to_le_bytes());
            body.push(slot as u8);
            body.extend_from_slice(&0u32.to_le_bytes());
        }
        body
    }

    #[test]
    fn an_empty_character_list_parses() {
        assert!(parse_char_enum(&[0]).unwrap().is_empty());
    }

    /// Pinned against a live server: a real `SMSG_CHAR_ENUM` holding one
    /// character named "Testwolf" was 279 bytes. That single number is what
    /// fixes the equipment slot count, which nothing else in the packet
    /// reveals -- every field parses at twenty slots just as happily as at
    /// twenty-three.
    #[test]
    fn a_real_entry_is_the_observed_length() {
        let mut body = vec![1];
        body.extend_from_slice(&char_entry("Testwolf", 1, 0));
        assert_eq!(body.len(), 279);
        assert_eq!(parse_char_enum(&body).unwrap()[0].name, "Testwolf");
    }

    #[test]
    fn characters_parse_with_their_equipment() {
        let mut body = vec![2];
        body.extend_from_slice(&char_entry("Kaelthas", 80, 1));
        body.extend_from_slice(&char_entry("Bo", 1, 0));
        let characters = parse_char_enum(&body).unwrap();

        assert_eq!(characters.len(), 2);
        assert_eq!(characters[0].name, "Kaelthas");
        assert_eq!(characters[0].level, 80);
        assert_eq!(characters[0].guid, 0x1122334455667788);
        assert_eq!(characters[0].equipment.len(), EQUIPMENT_SLOTS);
        assert_eq!(characters[0].equipment[5].display_id, 5);
        assert_eq!(characters[0].equipment[19].inventory_type, 19);
        // Names have no length prefix, so a second entry parsing at all proves
        // the first one's scan stopped in the right place.
        assert_eq!(characters[1].name, "Bo");
        assert_eq!(characters[1].level, 1);
    }

    /// One slot too few leaves nine bytes per character behind, and every
    /// individual field still parses. Only the total length catches it.
    #[test]
    fn a_wrong_equipment_count_is_caught_by_the_length() {
        let mut body = vec![1];
        let mut entry = char_entry("Solo", 1, 0);
        entry.truncate(entry.len() - 9);
        body.extend_from_slice(&entry);
        assert!(
            parse_char_enum(&body).is_err(),
            "a nineteen-slot entry was accepted"
        );

        let mut body = vec![1];
        body.extend_from_slice(&char_entry("Solo", 1, 0));
        body.extend_from_slice(&[0u8; 9]);
        assert!(matches!(
            parse_char_enum(&body),
            Err(Error::Trailing { got: 9, .. })
        ));
    }

    /// A count larger than the body must error rather than read past it.
    #[test]
    fn an_overstated_count_is_rejected() {
        let mut body = vec![4];
        body.extend_from_slice(&char_entry("Solo", 1, 0));
        assert!(matches!(
            parse_char_enum(&body),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn character_flags_decode() {
        let mut body = vec![1];
        let mut entry = char_entry("Ghosty", 70, 0);
        // The flags word sits after guid, name+NUL, eight appearance bytes,
        // level, zone, map, three floats and the guild id.
        let flags_at = 8 + 7 + 8 + 1 + 4 + 4 + 12 + 4;
        entry[flags_at..flags_at + 4].copy_from_slice(&0x2000u32.to_le_bytes());
        body.extend_from_slice(&entry);

        let character = &parse_char_enum(&body).unwrap()[0];
        assert!(character.is_ghost());
        assert!(!character.needs_rename());
        assert!(!character.has_pet());
    }
}
