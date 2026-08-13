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
    PlayerLogin = 0x003D,
    Ping = 0x01DC,
    AuthSession = 0x01ED,
    TimeSyncResp = 0x0391,
    /// What this client has selected. The interface could keep a target to
    /// itself, but the server is the one that decides whether a spell or an
    /// attack has a legal victim, so it has to be told.
    SetSelection = 0x013D,

    /// Nothing in an object update carries a name; these are how a client
    /// learns one. Players are asked for by guid, creatures by entry -- every
    /// wolf of a kind shares one answer.
    NameQuery = 0x0050,
    CreatureQuery = 0x0060,
    MessageChat = 0x0095,
    /// Ask to cast. What comes back is either the world reacting or
    /// `SMSG_CAST_FAILED` explaining why not.
    CastSpell = 0x012E,

    /// Start and stop auto-attacking. Auto-attack is a *state*, not an action:
    /// one swing request starts an exchange the server then drives on its own
    /// timer, which is why there is no per-swing message to send.
    ///
    /// **These two numbers are the only unverified constants in this enum, and
    /// they are verified by reaction rather than by transcription.** Nothing
    /// acknowledges an opcode as such, so the test is that sending
    /// `AttackSwing` at a live hostile produces a stream of combat packets
    /// that was not arriving before, and `AttackStop` ends it. A wrong number
    /// here is worse than a wrong number in a parser -- an outgoing message
    /// can be read as some *other* valid request -- so it was sent first at a
    /// level-one character on a test realm with nothing to lose, and the
    /// reaction checked before either was trusted.
    AttackSwing = 0x0141,
    AttackStop = 0x0142,

    // Movement. These are `MSG_` rather than `CMSG_`: the same opcode travels
    // in both directions, the client reporting its own movement and the server
    // relaying someone else's. Only the framing differs -- inbound packets are
    // about a guid that is not ours.
    MoveStartForward = 0x00B5,
    MoveStartBackward = 0x00B6,
    MoveStop = 0x00B7,
    MoveJump = 0x00BB,
    MoveSetFacing = 0x00DA,
    MoveHeartbeat = 0x00EE,
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
    /// Login refused after the character was chosen, unlike the auth-stage
    /// refusals; carries its own reason code.
    pub const CHARACTER_LOGIN_FAILED: u16 = 0x0041;
    pub const UPDATE_OBJECT: u16 = 0x00A9;
    pub const DESTROY_OBJECT: u16 = 0x00AA;
    /// The same payload as [`UPDATE_OBJECT`], zlib-deflated behind a length.
    pub const COMPRESSED_UPDATE_OBJECT: u16 = 0x01F6;
    /// The server asks periodically; ignoring it eventually drops the session.
    pub const TIME_SYNC_REQ: u16 = 0x0390;
    pub const MOTD: u16 = 0x033D;
    pub const ACCOUNT_DATA_TIMES: u16 = 0x0209;
    pub const LOGIN_SETTIMESPEED: u16 = 0x0042;
    /// A creature following a server-computed path. The most common packet in
    /// a populated zone by a wide margin.
    pub const MONSTER_MOVE: u16 = 0x00DD;
    /// Relayed movement from another mover, sharing the client opcodes.
    pub const MOVE_START_FORWARD: u16 = 0x00B5;
    pub const MOVE_STOP: u16 = 0x00B7;
    pub const MOVE_HEARTBEAT: u16 = 0x00EE;

    /// Answers to the two name queries. Neither is guaranteed to arrive: the
    /// server simply does not reply to a guid it has forgotten, which is why
    /// the name cache has to time requests out rather than wait.
    pub const NAME_QUERY_RESPONSE: u16 = 0x0051;
    pub const CREATURE_QUERY_RESPONSE: u16 = 0x0061;
    pub const MESSAGECHAT: u16 = 0x0096;
    /// The spellbook, sent unprompted during the login burst. There is no
    /// query for it: miss the packet and the character appears to know nothing.
    pub const INITIAL_SPELLS: u16 = 0x012A;
    pub const CAST_FAILED: u16 = 0x0130;
    pub const SPELL_START: u16 = 0x0131;
    pub const SPELL_GO: u16 = 0x0132;
    pub const SPELL_COOLDOWN: u16 = 0x0134;

    /// Melee. All three arrived in one capture of a level-one warrior fighting
    /// a wolf, and each is named for what its body turned out to hold rather
    /// than from a table -- see [`crate::combat`].
    pub const ATTACK_START: u16 = 0x0143;
    pub const ATTACK_STOP: u16 = 0x0144;
    /// One swing landing or missing. The workhorse of combat: fifteen of these
    /// in a fight that lasted under a minute.
    pub const ATTACKER_STATE_UPDATE: u16 = 0x014A;
    /// Two empty-bodied refusals that arrive when a swing cannot happen. Both
    /// were produced by attacking from out of range while facing away, three
    /// times each, and *which* is which is not established -- neither carries
    /// a payload to tell them apart, and no experiment has yet isolated one
    /// condition without the other. Named for what they are.
    pub const ATTACK_SWING_REFUSED_A: u16 = 0x0145;
    pub const ATTACK_SWING_REFUSED_B: u16 = 0x0146;
    /// One unit's power changing without a whole object update behind it.
    /// Confirmed by its last captured value agreeing with what the
    /// object-update path independently reported -- see
    /// [`crate::update::PowerUpdate`].
    pub const POWER_UPDATE: u16 = 0x0480;
    /// Who is on a unit's threat list. See [`crate::combat::ThreatUpdate`].
    /// Damage from a spell rather than a swing. Captured from a Wrath cast at
    /// a Young Nightsaber; see `combat::parse_spell_damage`.
    pub const SPELL_NON_MELEE_DAMAGE_LOG: u16 = 0x0250;
    pub const THREAT_UPDATE: u16 = 0x0483;

    /// The same body as [`MESSAGECHAT`], sent for a GM's lines.
    pub const GM_MESSAGECHAT: u16 = 0x03B3;
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
        server::CHARACTER_LOGIN_FAILED => "SMSG_CHARACTER_LOGIN_FAILED",
        server::UPDATE_OBJECT => "SMSG_UPDATE_OBJECT",
        server::DESTROY_OBJECT => "SMSG_DESTROY_OBJECT",
        server::COMPRESSED_UPDATE_OBJECT => "SMSG_COMPRESSED_UPDATE_OBJECT",
        server::TIME_SYNC_REQ => "SMSG_TIME_SYNC_REQ",
        server::MOTD => "SMSG_MOTD",
        server::ACCOUNT_DATA_TIMES => "SMSG_ACCOUNT_DATA_TIMES",
        server::LOGIN_SETTIMESPEED => "SMSG_LOGIN_SETTIMESPEED",
        server::MONSTER_MOVE => "SMSG_MONSTER_MOVE",
        server::MOVE_START_FORWARD => "MSG_MOVE_START_FORWARD",
        server::MOVE_STOP => "MSG_MOVE_STOP",
        server::MOVE_HEARTBEAT => "MSG_MOVE_HEARTBEAT",
        server::NAME_QUERY_RESPONSE => "SMSG_NAME_QUERY_RESPONSE",
        server::CREATURE_QUERY_RESPONSE => "SMSG_CREATURE_QUERY_RESPONSE",
        server::MESSAGECHAT => "SMSG_MESSAGECHAT",
        server::INITIAL_SPELLS => "SMSG_INITIAL_SPELLS",
        server::CAST_FAILED => "SMSG_CAST_FAILED",
        server::SPELL_START => "SMSG_SPELL_START",
        server::SPELL_GO => "SMSG_SPELL_GO",
        server::SPELL_COOLDOWN => "SMSG_SPELL_COOLDOWN",
        server::ATTACK_START => "SMSG_ATTACKSTART",
        server::ATTACK_STOP => "SMSG_ATTACKSTOP",
        server::ATTACKER_STATE_UPDATE => "SMSG_ATTACKERSTATEUPDATE",
        server::ATTACK_SWING_REFUSED_A => "SMSG_ATTACKSWING_REFUSED(0x0145)",
        server::ATTACK_SWING_REFUSED_B => "SMSG_ATTACKSWING_REFUSED(0x0146)",
        server::POWER_UPDATE => "SMSG_POWER_UPDATE",
        server::SPELL_NON_MELEE_DAMAGE_LOG => "SMSG_SPELLNONMELEEDAMAGELOG",
        server::THREAT_UPDATE => "SMSG_THREAT_UPDATE",
        server::GM_MESSAGECHAT => "SMSG_GM_MESSAGECHAT",
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
