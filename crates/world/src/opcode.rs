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

    /// Draw or stow the weapon: one `u32` naming a [`crate::SheathState`].
    ///
    /// **Sheathing is a client-side decision, and this is the surprise.** A
    /// whole fight was driven against the realm -- selection, swings landing
    /// both ways, the in-combat flag appearing in `UNIT_FLAGS` -- and byte 0
    /// of `UNIT_FIELD_BYTES_2` never moved off zero. The server does not draw
    /// a weapon for you and never will; it only records what the client says
    /// here and republishes it so *other* players see it.
    ///
    /// Confirmed the way `AttackSwing` was, by varying the input rather than
    /// waiting for a reply: nothing acknowledges this, but sending each state
    /// in turn moves that byte to the matching value and back.
    SetSheathed = 0x01E0,

    /// Wear the item in a given inventory slot, letting the server choose
    /// which equipment slot it belongs in.
    ///
    /// **Confirmed by effect, not by transcription.** Nothing acknowledges
    /// this, and an outgoing number that is wrong is read as some *other*
    /// valid request rather than refused -- the trap `CMSG_ATTACKSWING`
    /// documents. What makes it checkable is that the result is loud: the
    /// item's guid leaves its slot in `PLAYER_FIELD_INV_SLOT_HEAD` and
    /// reappears at an equipment index, and both halves arrive in the next
    /// object update. Sending this and watching a specific guid move between
    /// two specific fields is a statement that could have failed.
    ///
    /// The body is two bytes: the source bag, then the source slot.
    /// [`crate::inventory::OWN_SLOT_ARRAY`] is the bag value meaning "not in a
    /// bag, this is an index into the player's own array".
    ///
    /// Deliberately the *auto* form rather than one naming a destination. The
    /// server picking the slot is what makes this useful twice over: it is the
    /// simplest possible write, and its choice is a fact about the item that
    /// this client would otherwise have to guess at -- which is how the
    /// equipment slot vocabulary beyond the four originally confirmed was
    /// filled in.
    AutoEquipItem = 0x010A,

    /// Release the spirit: give up the body and become a ghost at the nearest
    /// graveyard. Carries one byte the server reads and discards.
    ///
    /// Refused in silence when the player is alive or is already a ghost, which
    /// is worth knowing before concluding the opcode is wrong -- the first
    /// attempt at this produced nothing at all because the character had been
    /// killed in an earlier run and was already released.
    RepopRequest = 0x015A,
    /// Take the body back, standing at the corpse. Carries the corpse's guid,
    /// **unpacked** -- eight plain bytes, unlike almost every other guid this
    /// protocol sends.
    ///
    /// Refused in silence unless the player is dead, has released, has a
    /// corpse, is within reclaim range of it, and the reclaim delay has
    /// elapsed. Five ways to get nothing back, none of which says which.
    ReclaimCorpse = 0x01D2,

    // Movement. These are `MSG_` rather than `CMSG_`: the same opcode travels
    // in both directions, the client reporting its own movement and the server
    // relaying someone else's. Only the framing differs -- inbound packets are
    // about a guid that is not ours.
    MoveStartForward = 0x00B5,
    MoveStartBackward = 0x00B6,
    MoveStop = 0x00B7,
    /// Sidestepping. A separate axis from forward and backward, with its own
    /// start and stop: a character can begin strafing without stopping running,
    /// and the opcode names only the axis that changed while the flags carry
    /// the whole state.
    ///
    /// These three are the same three that a real client's capture showed this
    /// project dropping -- `0x00B8`, `0x00B9` and `0x00BA` were among the five
    /// unnamed movement opcodes in `wow-cli moves`, each carrying a body that
    /// parsed as `{packed guid, MovementInfo}` and consumed to the byte. The
    /// capture said *these opcodes are movement*; it could not say which
    /// movement each was, which is why they went unnamed at the time.
    MoveStartStrafeLeft = 0x00B8,
    MoveStartStrafeRight = 0x00B9,
    MoveStopStrafe = 0x00BA,
    MoveJump = 0x00BB,
    /// The end of a jump or a fall. Carries the total time spent in the air in
    /// `MovementInfo::fall_time`, which is what fall damage is computed from --
    /// so a client that jumps and never lands is one the server believes is
    /// still falling.
    MoveFallLand = 0x00C9,
    MoveSetFacing = 0x00DA,
    MoveHeartbeat = 0x00EE,
    /// Confirming a teleport within the same map. The server sends this
    /// opcode, and the client must send it back before the move takes effect.
    ///
    /// **Not optional, and its absence is silent.** Until the acknowledgement
    /// arrives the server holds the character at the old position *and
    /// discards every movement packet the client sends* -- so a client that
    /// ignores this is frozen where it stood, while believing it is walking.
    /// Found because a released ghost reclaimed its corpse from 58 yards away
    /// when the limit is 39: the ghost had never actually left the body.
    MoveTeleportAck = 0x00C7,

    /// Ask the server where this character's body is. Empty request; the
    /// reply shares the opcode.
    ///
    /// The replicated world cannot answer this: corpse-type objects include
    /// the bones of bodies already reclaimed, they all carry their owner's
    /// guid, and a graveyard accumulates them. One live run saw seven while
    /// the server had two.
    CorpseQuery = 0x0216,
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

    /// What the sky is doing: a state, an intensity, and whether it changed
    /// abruptly. Sent on entering a zone and whenever the zone's weather turns.
    pub const WEATHER: u16 = 0x02F4;
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

    /// Every `MSG_MOVE_*` that carries a plain `{packed guid, MovementInfo}`
    /// and is relayed to the players who can see the mover.
    ///
    /// **This client read three of these twenty-four and threw the rest away.**
    /// The gap was found by watching a real 3.3.5a client walk about and asking
    /// `wow-cli moves` which opcodes in the capture had that shape: five more
    /// turned up, and `MSG_MOVE_SET_FACING` alone was 93% of that client's
    /// entire movement stream. Every one of 1,202 packets across those five
    /// parsed as a packed guid followed by a `MovementInfo` and consumed its
    /// body **exactly**, and every one named the single player in view -- where
    /// the control in the same capture, `MONSTER_MOVE`, managed 75 of 944 with
    /// 358 left over across 23 different guids.
    ///
    /// The list is completed from the server's own dispatch table rather than
    /// from the five that happened to be observed, because "which opcodes
    /// exist" is a question about the protocol and not about what a particular
    /// player did in three minutes -- nobody swam, pitched or flew during that
    /// capture, and those bodies are the same shape regardless. The five
    /// confirmed by observation are `START_BACKWARD`, `START_STRAFE_LEFT`,
    /// `START_STRAFE_RIGHT`, `STOP_STRAFE` and `SET_FACING`, plus `FALL_LAND`
    /// in an earlier run and the three already handled.
    ///
    /// The discriminator worth keeping is that `MSG_` travels in both
    /// directions and `CMSG_` does not: `CMSG_MOVE_FALL_RESET`,
    /// `CMSG_MOVE_SET_FLY` and `CMSG_MOVE_CHNG_TRANSPORT` share the same
    /// handler and are deliberately **not** here, because nothing relays them.
    pub const MOVE_RELAYED: [u16; 24] = [
        0x00B5, // START_FORWARD
        0x00B6, // START_BACKWARD
        0x00B7, // STOP
        0x00B8, // START_STRAFE_LEFT
        0x00B9, // START_STRAFE_RIGHT
        0x00BA, // STOP_STRAFE
        0x00BB, // JUMP
        0x00BC, // START_TURN_LEFT
        0x00BD, // START_TURN_RIGHT
        0x00BE, // STOP_TURN
        0x00BF, // START_PITCH_UP
        0x00C0, // START_PITCH_DOWN
        0x00C1, // STOP_PITCH
        0x00C2, // SET_RUN_MODE
        0x00C3, // SET_WALK_MODE
        0x00C9, // FALL_LAND
        0x00CA, // START_SWIM
        0x00CB, // STOP_SWIM
        0x00DA, // SET_FACING
        0x00DB, // SET_PITCH
        0x00EE, // HEARTBEAT
        0x0359, // START_ASCEND
        0x035A, // STOP_ASCEND
        0x03A7, // START_DESCEND
    ];

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

    /// The server moving this character within the current map, and asking to
    /// be told the client noticed: `{packed guid, u32 counter, MovementInfo}`.
    /// See [`crate::ClientOpcode::MoveTeleportAck`] for why answering matters.
    pub const MOVE_TELEPORT_ACK: u16 = 0x00C7;

    /// The answer to [`crate::ClientOpcode::CorpseQuery`], sharing its
    /// number. See [`crate::death::parse_corpse_query`].
    pub const CORPSE_QUERY: u16 = 0x0216;

    /// Where to run back to: `{u32 map, f32 x, f32 y, f32 z}` naming the
    /// graveyard a released ghost was sent to.
    ///
    /// **This is why the corpse run needs no `WorldSafeLocs.dbc`.** The
    /// obvious reading of a graveyard run is that the client picks the nearest
    /// graveyard out of the table and shows it; the server picks it and says
    /// which, so the table is only needed to put a *name* on the place. A map
    /// of `0xFFFFFFFF` with three zeroes is the same packet used to take the
    /// marker back off the minimap, which is what arrives on resurrection.
    pub const DEATH_RELEASE_LOC: u16 = 0x0378;
    /// Sent on death: how long before the corpse can be reclaimed, in
    /// milliseconds. Observed carrying exactly 30000.
    pub const CORPSE_RECLAIM_DELAY: u16 = 0x0269;
    /// A unit's threat list dropping someone: `{packed guid, packed guid}`,
    /// the list's owner then whoever left it. Arrives twice on death, once per
    /// creature that had us.
    pub const THREAT_REMOVE: u16 = 0x0484;
    /// The fight ending because one side stopped existing. Empty body.
    pub const CANCEL_COMBAT: u16 = 0x014E;
    /// Equipment losing durability from dying. Not parsed.
    pub const DURABILITY_DAMAGE_DEATH: u16 = 0x02BD;
}

/// Whether this opcode carries `{packed guid, MovementInfo}` from another
/// mover.
///
/// One list, consulted by the dispatcher that folds these and by the capture
/// analysis that hunts for ones being missed. Two copies would drift, and the
/// way they would drift is the analysis reporting an opcode as handled after
/// someone removed it from the fold -- a tool agreeing with itself, which is
/// the one shape of evidence this project does not accept.
pub fn is_relayed_movement(opcode: u16) -> bool {
    server::MOVE_RELAYED.contains(&opcode)
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
        server::MOVE_TELEPORT_ACK => "MSG_MOVE_TELEPORT_ACK",
        server::CORPSE_QUERY => "MSG_CORPSE_QUERY",
        server::DEATH_RELEASE_LOC => "SMSG_DEATH_RELEASE_LOC",
        server::CORPSE_RECLAIM_DELAY => "SMSG_CORPSE_RECLAIM_DELAY",
        server::THREAT_REMOVE => "SMSG_THREAT_REMOVE",
        server::CANCEL_COMBAT => "SMSG_CANCEL_COMBAT",
        server::DURABILITY_DAMAGE_DEATH => "SMSG_DURABILITY_DAMAGE_DEATH",
        server::GM_MESSAGECHAT => "SMSG_GM_MESSAGECHAT",
        // Understood as movement without this client caring which movement it
        // is: every one of them is a position for a mover, and that is all it
        // does with them. The number stays visible so a log still says which.
        other if is_relayed_movement(other) => {
            return format!("MSG_MOVE_* relayed ({other:#06x})")
        }
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
