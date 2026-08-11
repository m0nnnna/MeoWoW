//! World server opcodes.
//!
//! 3.3.5a defines roughly 1400 of these. Only the ones this client actually
//! sends or recognises are named -- an exhaustive table would be a large block
//! of unverified constants, and an unrecognised opcode is not an error anyway:
//! the server volunteers plenty the client is free to ignore.

/// Client-to-server opcodes.
///
/// On the wire these are 32-bit, unlike the server's 16-bit ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ClientOpcode {
    CharCreate = 0x0036,
    CharEnum = 0x0037,
    CharDelete = 0x0038,
    Ping = 0x01DC,
    AuthSession = 0x01ED,
}

/// The server-to-client opcodes this client reacts to.
///
/// Anything not listed is passed through as a raw number; see [`describe`].
pub mod server {
    pub const CHAR_ENUM: u16 = 0x003B;
    pub const PONG: u16 = 0x01DD;
    pub const AUTH_CHALLENGE: u16 = 0x01EC;
    pub const AUTH_RESPONSE: u16 = 0x01EE;
    pub const TUTORIAL_FLAGS: u16 = 0x00FD;
    pub const ADDON_INFO: u16 = 0x02EF;
    pub const CLIENTCACHE_VERSION: u16 = 0x04AB;
    pub const LOGIN_VERIFY_WORLD: u16 = 0x0236;
    pub const CHAR_CREATE: u16 = 0x003A;
    pub const CHAR_DELETE: u16 = 0x003C;
}

/// A human-readable name for an incoming opcode, for logs and dumps.
///
/// Unknown opcodes render as their number rather than being rejected, because
/// the server sends a good many this client has no interest in.
pub fn describe(opcode: u16) -> String {
    let name = match opcode {
        server::CHAR_ENUM => "SMSG_CHAR_ENUM",
        server::PONG => "SMSG_PONG",
        server::AUTH_CHALLENGE => "SMSG_AUTH_CHALLENGE",
        server::AUTH_RESPONSE => "SMSG_AUTH_RESPONSE",
        server::TUTORIAL_FLAGS => "SMSG_TUTORIAL_FLAGS",
        server::ADDON_INFO => "SMSG_ADDON_INFO",
        server::CLIENTCACHE_VERSION => "SMSG_CLIENTCACHE_VERSION",
        server::LOGIN_VERIFY_WORLD => "SMSG_LOGIN_VERIFY_WORLD",
        server::CHAR_CREATE => "SMSG_CHAR_CREATE",
        server::CHAR_DELETE => "SMSG_CHAR_DELETE",
        other => return format!("opcode {other:#06x}"),
    };
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client and server halves of a pair share a number in 3.3.5a --
    /// `CMSG_CHAR_ENUM` is 0x37 and `SMSG_CHAR_ENUM` is 0x3B, which are
    /// *different*, unlike the auth pair. Pin both so a transcription slip
    /// cannot quietly swap them.
    #[test]
    fn the_handshake_opcodes_are_the_documented_values() {
        assert_eq!(ClientOpcode::AuthSession as u32, 0x01ED);
        assert_eq!(server::AUTH_CHALLENGE, 0x01EC);
        assert_eq!(server::AUTH_RESPONSE, 0x01EE);
        assert_eq!(ClientOpcode::CharEnum as u32, 0x0037);
        assert_eq!(server::CHAR_ENUM, 0x003B);
    }

    #[test]
    fn unknown_opcodes_describe_as_numbers() {
        assert_eq!(describe(server::CHAR_ENUM), "SMSG_CHAR_ENUM");
        assert_eq!(describe(0x1234), "opcode 0x1234");
    }
}
