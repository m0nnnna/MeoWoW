//! Parties: who is in the group, and what is happening to them.
//!
//! Five packets, and they divide the way the project's two instruments do.
//! `SMSG_GROUP_LIST` is the group *as it stands* -- a complete statement,
//! resent in full whenever anything changes -- and `SMSG_PARTY_MEMBER_STATS`
//! is a *change* to one member. The first is what the party frames are drawn
//! from; the second exists for exactly the case the replicated world cannot
//! cover, which is a group member standing in another zone.
//!
//! **That split is the whole reason this milestone is not just an interface
//! one.** A party member within visibility range is an ordinary replicated
//! player: health, power, level and position all arrive in object updates and
//! this client has read them since 4.1. A party member two zones away is not
//! replicated at all -- no object, no fields, nothing -- and the only thing
//! the server sends about them is this. A client that drew party frames out of
//! replicated state alone would look completely correct right up until the
//! moment the party split up, which is when a party frame is for.
//!
//! ## How the layouts were established
//!
//! `CMSG_GROUP_INVITE` is **answered**, and that is why it is the request this
//! milestone attempts first. Every other outgoing group message is silent, and
//! a silent write fails identically whether the opcode is wrong, the body is
//! wrong, or the server declined -- the trap [`crate::ClientOpcode::BuyItem`]
//! documents and `CMSG_LIST_INVENTORY` was used to escape. An invite produces
//! `SMSG_PARTY_COMMAND_RESULT` **either way**: it names the operation, the
//! player asked for, and a result code that differs between success and every
//! kind of refusal. So one send with a deliberately misspelt name and one with
//! a real one bound the opcode, the body and the reply layout at once, before
//! any packet with a variable-length list in it had to be read.
//!
//! Everything after that was measured against a live two-client party on the
//! local realm, with the accounts and characters `CLAUDE.md` records -- the
//! only rig that can produce a group at all, since a party of one does not
//! exist.

use crate::protocol::{Error, Reader};
use crate::update::read_packed_guid;

/// Somebody has asked this character to join their group.
///
/// The name is the inviter's, and it is the *only* handle the packet carries:
/// there is no guid here, which is consistent with an invite being the thing
/// you send to a person you have only read off a chat line. Accepting is
/// [`crate::Connection::group_accept`], which likewise identifies nothing,
/// because a character can hold one pending invite at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupInvite {
    /// Who is asking.
    pub from: String,
    /// Whether this is a real invite or the server telling this character an
    /// invite was aimed at them and could not be delivered -- the shape sent
    /// when the invitee is already in another group.
    ///
    /// The distinction is the server's, not a guess: the same opcode carries
    /// `1` for the invite proper and `0` for the notification, and a client
    /// that offered an Accept button for the `0` case would be offering a
    /// button that cannot work.
    pub can_accept: bool,
}

/// Parses `SMSG_GROUP_INVITE`.
pub fn parse_group_invite(body: &[u8]) -> Result<GroupInvite, Error> {
    let mut r = Reader::new(body, "SMSG_GROUP_INVITE");
    let can_accept = r.u8()? != 0;
    let from = r.cstring()?;
    // Three fields this client has no use for: a `u32`, then a count of
    // realm names with a `u32` each, then a trailing `u32`. The count has
    // only ever arrived as zero. They are read rather than skipped wholesale
    // so that the cursor still has to consume the body exactly -- the check
    // that has caught four separate world-protocol bugs here.
    let _unknown = r.u32()?;
    let realms = r.u8()?;
    for _ in 0..realms {
        let _realm = r.u32()?;
    }
    let _also_unknown = r.u32()?;
    r.finish()?;
    Ok(GroupInvite { from, can_accept })
}

/// One member of a party, as `SMSG_GROUP_LIST` describes them.
///
/// **Everything here is the group's view of the member, not the world's.**
/// The name and guid are permanent; the status byte is the only live fact in
/// the record, and it is coarse -- online, dead, away. Health and level do not
/// appear at all, which is what [`MemberStats`] is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyMember {
    pub name: String,
    pub guid: u64,
    /// Online, dead, ghost, PvP, away -- see [`MemberStatus`].
    pub status: u8,
    /// Which sub-group of a raid this member is in. Always `0` in a party.
    pub subgroup: u8,
    /// Assistant, main tank, main assist. Raid roles; `0` in a party.
    pub flags: u8,
    /// The dungeon-finder role. `0` outside it.
    pub roles: u8,
}

impl PartyMember {
    /// Whether this member is connected. **Not the same as being alive** and
    /// not the same as being visible: an offline member stays in the list.
    pub fn is_online(&self) -> bool {
        self.status & MemberStatus::ONLINE != 0
    }

    /// Whether this member is dead -- as a corpse or as a ghost, which are two
    /// different bits and both mean the frame should be dimmed.
    pub fn is_dead(&self) -> bool {
        self.status & (MemberStatus::DEAD | MemberStatus::GHOST) != 0
    }
}

/// The bits of [`PartyMember::status`].
///
/// Named from the server's own enum rather than from a guess at what each
/// draws, and only the three this client acts on are used anywhere. The rest
/// are here so a status byte can be printed as something a reader recognises
/// instead of as a number.
pub struct MemberStatus;

impl MemberStatus {
    pub const ONLINE: u8 = 0x01;
    pub const PVP: u8 = 0x02;
    pub const DEAD: u8 = 0x04;
    pub const GHOST: u8 = 0x08;
    pub const PVP_FFA: u8 = 0x10;
    pub const AFK: u8 = 0x40;
    pub const DND: u8 = 0x80;
}

/// How the group divides what it kills.
///
/// Present only when the group has other members in it -- see [`Party`] for
/// why that conditional matters and what its absence means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LootRule {
    /// Free-for-all, round robin, master looter, group loot, need before
    /// greed. **Not interpreted here**: what each does is a rule about how the
    /// server hands out loot, and naming them from memory is the mistake
    /// [`crate::spell::describe_cast_failure`] exists to refuse.
    pub method: u8,
    /// Who picks, under master loot. Zero under every other method.
    pub master: u64,
    /// The item quality at and above which the group rolls. Raw.
    pub threshold: u8,
    pub dungeon_difficulty: u8,
    pub raid_difficulty: u8,
    /// The 3.3 dynamic raid difficulty flag: whether the raid difficulty is a
    /// heroic one. Derived by the server from the field above it.
    pub raid_heroic: u8,
}

/// Renders a loot rule as a line worth showing, naming nothing this project
/// has not behaviourally confirmed.
///
/// **Every field here is currently a raw number, on purpose.** `method` and
/// `threshold` are small public-facing enums with well-known names elsewhere
/// -- but "well known elsewhere" is exactly the trap `describe_cast_failure`
/// exists to refuse: a name asserts an observation, not a memory, and this
/// project has only ever watched one number come off a live realm (`3`, on
/// every party formed here so far) with nothing yet distinguishing what any
/// of them actually *does* to a loot roll. When a future session tests that
/// -- forms a master-loot party and confirms only the leader can assign an
/// item, say -- name it here rather than in the caller, the same way
/// [`describe_party_result`] is the one place a result code becomes English.
pub fn describe_loot_rule(rule: &LootRule, master_name: Option<&str>) -> String {
    let mut line = format!("loot: method {}, threshold {}", rule.method, rule.threshold);
    if rule.master != 0 {
        match master_name {
            Some(name) => line.push_str(&format!(", master {name}")),
            None => line.push_str(&format!(", master {:#018x}", rule.master)),
        }
    }
    line
}

/// The whole group, as the server states it.
///
/// **This packet is a complete statement and replaces whatever came before.**
/// It is resent in full to every member on every change -- someone joining,
/// leaving, dying, being promoted -- which is what makes party state one of
/// the few pieces of replicated state here that needs no accounting: there is
/// no merge to get wrong and no update that can be missed permanently, because
/// the next one says everything again.
///
/// **An empty member list does *not* mean there is no group, and the packet
/// that proves it arrives one packet before the one you were waiting for.**
/// The obvious reading -- a party of one does not exist, so nobody else in the
/// list means no party -- was written here first and is wrong. Forming a group
/// sends the leader *two* lists in a row: a 28-byte one naming the group, no
/// other members and **itself as leader**, and then the real one once the
/// invitee is in. A client keyed on emptiness reports "you left the group" for
/// a moment every single time a group is formed.
///
/// What separates them is [`Party::leader`]: the group-of-one carries a real
/// leader guid, and the "you are in no group" form carries **zero**. That is a
/// fact about the bytes rather than an interpretation of a flag, which is why
/// it is what [`Party::in_group`] tests. The leave form also sets `0x10` in
/// [`Party::group_type`] -- recorded because it was observed, not relied on,
/// since one sample of one flag bit is a guess and the leader guid is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Party {
    /// Raid, battleground, dungeon-finder. `0` for an ordinary party.
    ///
    /// Kept raw. The bit this client would most like to name is the one the
    /// server sets on the "you are no longer in a group" form of this packet,
    /// and that state is read off the empty member list instead -- a fact that
    /// does not depend on interpreting a flag.
    pub group_type: u8,
    /// This character's own sub-group, role flags and dungeon-finder role.
    /// The three fields the server sends about *you* before it sends the
    /// others, which is why they are not in a [`PartyMember`]: the reader is
    /// never in their own member list.
    pub own_subgroup: u8,
    pub own_flags: u8,
    pub own_roles: u8,
    /// The group's own guid.
    pub guid: u64,
    /// Increments every time the server sends this packet. Useful only for
    /// telling two otherwise identical lists apart in a log.
    pub counter: u32,
    /// **Everyone but the reader.** The server leaves the recipient out of
    /// their own list, so a two-person party arrives with one member in it,
    /// and a party frame drawing this list directly is correct -- the player's
    /// own frame is a different frame.
    pub members: Vec<PartyMember>,
    /// Who leads. May be this character, in which case the guid appears here
    /// and in no member record.
    pub leader: u64,
    /// Absent when [`Party::members`] is empty, which is the not-in-a-group
    /// case. The server does not send the loot block for a group with nobody
    /// else in it.
    pub loot: Option<LootRule>,
}

impl Party {
    /// Whether this packet says the character is in a group at all.
    ///
    /// **Tests the leader, not the member list.** See the type comment: a
    /// group with nobody else in it yet is a real state the server sends
    /// during every group's first second, and it carries a leader. Only the
    /// "you are in no group" form zeroes one.
    pub fn in_group(&self) -> bool {
        self.leader != 0
    }

    /// Whether there is anybody but the reader in the group -- which is a
    /// different question from [`Self::in_group`], and the one a party frame
    /// asks: the player's own frame is drawn separately, so a group of one has
    /// nothing for this interface to put on screen.
    pub fn has_members(&self) -> bool {
        !self.members.is_empty()
    }

    /// Whether the given guid leads this group.
    pub fn is_leader(&self, guid: u64) -> bool {
        self.leader == guid && guid != 0
    }

    /// Finds a member by guid.
    pub fn member(&self, guid: u64) -> Option<&PartyMember> {
        self.members.iter().find(|m| m.guid == guid)
    }
}

/// Whether a group type means the dungeon finder put it together.
///
/// The one flag that has to be interpreted rather than carried, because an
/// LFG group's `SMSG_GROUP_LIST` carries two extra fields immediately after
/// the header -- so a reader that ignored this bit would parse every field
/// after it five bytes late, and only inside a dungeon finder group.
const GROUPTYPE_LFG: u8 = 0x08;

/// Parses `SMSG_GROUP_LIST`.
pub fn parse_group_list(body: &[u8]) -> Result<Party, Error> {
    let mut r = Reader::new(body, "SMSG_GROUP_LIST");
    let group_type = r.u8()?;
    let own_subgroup = r.u8()?;
    let own_flags = r.u8()?;
    let own_roles = r.u8()?;
    if group_type & GROUPTYPE_LFG != 0 {
        // A saved-dungeon status byte and the dungeon's id. Read and dropped:
        // this client does not use the dungeon finder, and the fields exist
        // here only so that everything after them lands at the right offset.
        let _dungeon_state = r.u8()?;
        let _dungeon = r.u32()?;
    }
    let guid = r.u64()?;
    let counter = r.u32()?;
    let count = r.u32()?;
    let mut members = Vec::with_capacity(count.min(40) as usize);
    for _ in 0..count {
        let name = r.cstring()?;
        let guid = r.u64()?;
        let status = r.u8()?;
        let subgroup = r.u8()?;
        let flags = r.u8()?;
        let roles = r.u8()?;
        members.push(PartyMember {
            name,
            guid,
            status,
            subgroup,
            flags,
            roles,
        });
    }
    let leader = r.u64()?;
    // The conditional the layout turns on, and the reason `count` rather than
    // `members.is_empty()` is what gates it: they are the same number, and
    // reading the block when the server did not send it consumes bytes that
    // are not there -- which at least fails loudly. Reading it when the
    // server *did* send it and this code thought otherwise would leave six
    // bytes of trailing data, which `finish` also catches. Both halves are
    // checked by the cursor rather than assumed.
    let loot = if count > 0 {
        Some(LootRule {
            method: r.u8()?,
            master: r.u64()?,
            threshold: r.u8()?,
            dungeon_difficulty: r.u8()?,
            raid_difficulty: r.u8()?,
            raid_heroic: r.u8()?,
        })
    } else {
        None
    };
    r.finish()?;
    Ok(Party {
        group_type,
        own_subgroup,
        own_flags,
        own_roles,
        guid,
        counter,
        members,
        leader,
        loot,
    })
}

/// What one member's `SMSG_PARTY_MEMBER_STATS` said changed.
///
/// **Every field is optional and that is the packet, not a convenience.** A
/// mask names which of twenty fields follow, and the server sends only the
/// ones that moved -- a member taking damage sends current health and nothing
/// else. Storing the absent ones as zero would report a party member in
/// another zone as being level 0 with no mana the instant they were hit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemberStats {
    /// Who this is about. Sent **packed**, unlike every guid in
    /// `SMSG_GROUP_LIST`.
    pub guid: u64,
    /// The raw mask, kept so a log can say which fields the server thought it
    /// was sending even where this parser dropped them.
    pub mask: u32,
    /// Online, dead, away -- the same bits as [`PartyMember::status`], but
    /// **sixteen wide here** where the group list sends eight.
    pub status: Option<u16>,
    pub health: Option<u32>,
    pub max_health: Option<u32>,
    /// Which of mana, rage, energy, runic power this member uses.
    pub power_type: Option<u8>,
    /// **Sixteen bits, where the health above is thirty-two.** Not a
    /// transcription slip: no power pool in 3.3.5 exceeds 65535 and health
    /// does, so the packet spends its bytes where they are needed.
    pub power: Option<u16>,
    pub max_power: Option<u16>,
    pub level: Option<u16>,
    /// The zone id, for the "which part of the world are they in" line a party
    /// frame shows. Resolvable through `AreaTable.dbc`, which this client
    /// already reads for music and exploration.
    pub zone: Option<u16>,
    /// **A position quantised to two *signed* sixteen-bit words**, which is
    /// the whole reason a party member's dot on the world map is coarse: it is
    /// a whole-unit truncation of a float, not the float.
    ///
    /// The signedness was measured rather than assumed, and it is the one
    /// field in this packet where the wrong reading is silent. Elwynn is at
    /// negative coordinates, so `Watcher` at `-8735.9, -67.0` arrives as
    /// `0xdde1, 0xffbd` -- read unsigned that is `56801, 65469`, which is a
    /// perfectly plausible pair of numbers somewhere in the world and lands a
    /// map dot in Kalimdor. Read signed it is `-8735, -67`, matching the
    /// position that client independently reports for itself to within the
    /// truncation. `Testwolf` at `-8921.1, -119.1` agrees the same way, and
    /// two characters in two different places is what makes it a measurement
    /// instead of a coincidence.
    pub position: Option<(i16, i16)>,
}

/// Parses `SMSG_PARTY_MEMBER_STATS`, and `SMSG_PARTY_MEMBER_STATS_FULL` once
/// its leading byte has been taken off.
///
/// **Everything past the fields this client uses is skipped by length rather
/// than by structure**, and the difference matters: the pet block and the aura
/// block are variable-length, so the tail cannot simply be ignored -- a mask
/// naming an aura block makes every field after it move. The skip therefore
/// walks the same flag order the server writes in and consumes each block
/// properly, so the cursor still ends exactly at the end of the body. That
/// check is what would catch this table being wrong.
pub fn parse_party_member_stats(body: &[u8]) -> Result<MemberStats, Error> {
    let mut r = Reader::new(body, "SMSG_PARTY_MEMBER_STATS");
    let guid = read_packed_guid(&mut r)?;
    let mask = r.u32()?;
    let mut stats = MemberStats {
        guid,
        mask,
        ..Default::default()
    };

    // The order is the bit order, and the bits are in the order the server
    // writes them -- so this reads as one long sequence of conditionals
    // rather than as a table lookup. A table would be shorter and would hide
    // the widths, which are the part that varies.
    if mask & flag::STATUS != 0 {
        stats.status = Some(r.u16()?);
    }
    if mask & flag::CUR_HP != 0 {
        stats.health = Some(r.u32()?);
    }
    if mask & flag::MAX_HP != 0 {
        stats.max_health = Some(r.u32()?);
    }
    if mask & flag::POWER_TYPE != 0 {
        stats.power_type = Some(r.u8()?);
    }
    if mask & flag::CUR_POWER != 0 {
        stats.power = Some(r.u16()?);
    }
    if mask & flag::MAX_POWER != 0 {
        stats.max_power = Some(r.u16()?);
    }
    if mask & flag::LEVEL != 0 {
        stats.level = Some(r.u16()?);
    }
    if mask & flag::ZONE != 0 {
        stats.zone = Some(r.u16()?);
    }
    if mask & flag::POSITION != 0 {
        stats.position = Some((r.u16()? as i16, r.u16()? as i16));
    }
    if mask & flag::AURAS != 0 {
        skip_aura_block(&mut r)?;
    }
    if mask & flag::PET_GUID != 0 {
        let _pet = r.u64()?;
    }
    if mask & flag::PET_NAME != 0 {
        let _name = r.cstring()?;
    }
    if mask & flag::PET_MODEL_ID != 0 {
        let _model = r.u16()?;
    }
    if mask & flag::PET_CUR_HP != 0 {
        let _hp = r.u32()?;
    }
    if mask & flag::PET_MAX_HP != 0 {
        let _hp = r.u32()?;
    }
    if mask & flag::PET_POWER_TYPE != 0 {
        let _kind = r.u8()?;
    }
    if mask & flag::PET_CUR_POWER != 0 {
        let _power = r.u16()?;
    }
    if mask & flag::PET_MAX_POWER != 0 {
        let _power = r.u16()?;
    }
    if mask & flag::PET_AURAS != 0 {
        skip_aura_block(&mut r)?;
    }
    if mask & flag::VEHICLE_SEAT != 0 {
        let _seat = r.u32()?;
    }
    r.finish()?;
    Ok(stats)
}

/// Parses `SMSG_PARTY_MEMBER_STATS_FULL`, which is the same packet behind one
/// extra leading byte.
///
/// The byte says whether the member is in the same instance as the reader.
/// Kept separate from [`parse_party_member_stats`] rather than handled with a
/// boolean, because the two are different opcodes and a parser that took a
/// flag would let a caller pass the wrong one.
pub fn parse_party_member_stats_full(body: &[u8]) -> Result<MemberStats, Error> {
    let (first, rest) = body.split_first().ok_or(Error::Truncated {
        what: "SMSG_PARTY_MEMBER_STATS_FULL",
        at: 0,
        need: 1,
        len: 0,
    })?;
    let _same_instance = *first;
    parse_party_member_stats(rest)
}

/// Reads past an aura block: a 64-bit mask, then a spell id and a byte for
/// each bit set in it.
fn skip_aura_block(r: &mut Reader<'_>) -> Result<(), Error> {
    let mask = r.u64()?;
    for bit in 0..64 {
        if mask & (1u64 << bit) != 0 {
            let _spell = r.u32()?;
            let _unknown = r.u8()?;
        }
    }
    Ok(())
}

/// The bits of [`MemberStats::mask`].
pub mod flag {
    pub const STATUS: u32 = 0x0000_0001;
    pub const CUR_HP: u32 = 0x0000_0002;
    pub const MAX_HP: u32 = 0x0000_0004;
    pub const POWER_TYPE: u32 = 0x0000_0008;
    pub const CUR_POWER: u32 = 0x0000_0010;
    pub const MAX_POWER: u32 = 0x0000_0020;
    pub const LEVEL: u32 = 0x0000_0040;
    pub const ZONE: u32 = 0x0000_0080;
    pub const POSITION: u32 = 0x0000_0100;
    pub const AURAS: u32 = 0x0000_0200;
    pub const PET_GUID: u32 = 0x0000_0400;
    pub const PET_NAME: u32 = 0x0000_0800;
    pub const PET_MODEL_ID: u32 = 0x0000_1000;
    pub const PET_CUR_HP: u32 = 0x0000_2000;
    pub const PET_MAX_HP: u32 = 0x0000_4000;
    pub const PET_POWER_TYPE: u32 = 0x0000_8000;
    pub const PET_CUR_POWER: u32 = 0x0001_0000;
    pub const PET_MAX_POWER: u32 = 0x0002_0000;
    pub const PET_AURAS: u32 = 0x0004_0000;
    pub const VEHICLE_SEAT: u32 = 0x0008_0000;
}

/// What a group command was about, and whether it worked.
///
/// **The packet that made this milestone tractable.** See the module comment:
/// it is the only answer any group request gets, and it arrives on failure as
/// well as success, so it separates "the opcode was not understood" from "it
/// was understood and declined" in a single send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyCommandResult {
    /// Which operation is being reported on -- see [`PartyOperation`].
    pub operation: u32,
    /// Who it was about. **Echoed from the request** for an invite, which is
    /// what makes a deliberately misspelt name such a cheap test: the name
    /// comes back unchanged and unresolved, so a reply at all proves the
    /// server read the string out of the body at the offset this client wrote
    /// it to.
    pub member: String,
    /// Zero means it worked. Everything else is a reason, and this client
    /// names only the ones it has actually produced -- see
    /// [`describe_party_result`].
    pub result: u32,
    /// A cooldown in seconds, for the two dungeon-finder vote-kick results
    /// that carry one. Zero everywhere else.
    pub cooldown: u32,
}

impl PartyCommandResult {
    /// Whether the operation succeeded.
    pub fn is_ok(&self) -> bool {
        self.result == PartyResult::OK
    }
}

/// Parses `SMSG_PARTY_COMMAND_RESULT`.
pub fn parse_party_command_result(body: &[u8]) -> Result<PartyCommandResult, Error> {
    let mut r = Reader::new(body, "SMSG_PARTY_COMMAND_RESULT");
    let operation = r.u32()?;
    let member = r.cstring()?;
    let result = r.u32()?;
    let cooldown = r.u32()?;
    r.finish()?;
    Ok(PartyCommandResult {
        operation,
        member,
        result,
        cooldown,
    })
}

/// The values of [`PartyCommandResult::operation`].
pub struct PartyOperation;

impl PartyOperation {
    pub const INVITE: u32 = 0;
    pub const UNINVITE: u32 = 1;
    pub const LEAVE: u32 = 2;
    pub const SWAP: u32 = 4;
}

/// The values of [`PartyCommandResult::result`] this client has observed.
pub struct PartyResult;

impl PartyResult {
    pub const OK: u32 = 0;
    /// No such player, or nobody by that name is online. **The most useful
    /// code here**, because it is the one a deliberately wrong name produces
    /// and so is what bounds the opcode and the body.
    pub const BAD_PLAYER_NAME: u32 = 1;
    pub const TARGET_NOT_IN_GROUP: u32 = 2;
    pub const GROUP_FULL: u32 = 3;
    pub const ALREADY_IN_GROUP: u32 = 4;
    pub const NOT_IN_GROUP: u32 = 5;
    pub const NOT_LEADER: u32 = 6;
    pub const PLAYER_WRONG_FACTION: u32 = 7;
    pub const IGNORING_YOU: u32 = 8;
}

/// A sentence for a party result code, or the raw number.
///
/// **Deliberately partial, and for the reason `describe_cast_failure` is.** A
/// wrong field offset eventually fails loudly; a wrong *name* for a status
/// code never does -- it confidently misexplains what happened and sends the
/// next reader somewhere else. So only codes this client has actually produced
/// against a live realm are named here, and everything else comes back as its
/// number. The list grows when a run produces a new one, not when somebody
/// remembers an enum.
///
/// The `%s` in the original client's strings is this member's name, which the
/// caller has in [`PartyCommandResult::member`] and can put where it belongs.
pub fn describe_party_result(code: u32) -> String {
    match code {
        PartyResult::OK => "it worked".to_string(),
        PartyResult::BAD_PLAYER_NAME => {
            "no player by that name is online, or the name is misspelt".to_string()
        }
        PartyResult::ALREADY_IN_GROUP => "they are already in a group".to_string(),
        PartyResult::NOT_IN_GROUP => "you are not in a group".to_string(),
        PartyResult::NOT_LEADER => "you do not lead this group".to_string(),
        other => format!("party result {other}"),
    }
}

#[cfg(test)]
mod loot_rule_tests {
    use super::*;

    /// The exact values a live capture on the local realm gave: `method 3,
    /// threshold 2, master 0x0`. No name for `3`, on purpose -- see
    /// `describe_loot_rule`'s doc comment.
    #[test]
    fn a_captured_rule_prints_its_raw_numbers() {
        let rule = LootRule {
            method: 3,
            master: 0,
            threshold: 2,
            dungeon_difficulty: 0,
            raid_difficulty: 0,
            raid_heroic: 0,
        };
        assert_eq!(describe_loot_rule(&rule, None), "loot: method 3, threshold 2");
    }

    /// A non-zero master is shown, named if the caller can and by guid if
    /// not -- either way it is never omitted, since a master looter with an
    /// invisible name is the harder failure to notice.
    #[test]
    fn a_master_is_shown_named_or_by_guid() {
        let rule = LootRule {
            method: 2,
            master: 0x0000_0001,
            threshold: 2,
            dungeon_difficulty: 0,
            raid_difficulty: 0,
            raid_heroic: 0,
        };
        assert_eq!(
            describe_loot_rule(&rule, None),
            "loot: method 2, threshold 2, master 0x0000000000000001"
        );
        assert_eq!(
            describe_loot_rule(&rule, Some("Testwolf")),
            "loot: method 2, threshold 2, master Testwolf"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every body in this module is a real capture from the local realm,
    /// written as the hex the probe printed so a reader can check the bytes
    /// against the assertions without running anything.
    fn bytes(hex: &str) -> Vec<u8> {
        hex.split_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16).expect("hex"))
            .collect()
    }

    // `Testwolf` (guid 1, level 5, 136 health, at -8921.1, -119.1) invited
    // `Watcher` (guid 3, level 1, 60 health, at -8735.9, -67.0) on the local
    // realm, and `Watcher` accepted. Both clients' captures are below, and the
    // pairing is what makes them evidence: each character's own client
    // independently reported its level, health and position, and the packet
    // the *other* client received has to agree with all three.
    const INVITE_AT_WATCHER: &str = "01 54 65 73 74 77 6f 6c 66 00 00 00 00 00 00 00 00 00  \n        00";
    const LIST_AT_LEADER_BEFORE_JOIN: &str = "00 00 00 00 01 00 00 00 00 00 50 1f 00 00 00 00 00 00  \n        00 00 01 00 00 00 00 00 00 00";
    const LIST_AT_LEADER: &str = "00 00 00 00 01 00 00 00 00 00 50 1f 01 00 00 00 01 00  \n        00 00 57 61 74 63 68 65 72 00 03 00 00 00 00 00 00 00  \n        01 00 00 00 01 00 00 00 00 00 00 00 03 00 00 00 00 00  \n        00 00 00 02 00 00 00";
    const LIST_AT_MEMBER: &str = "00 00 00 00 01 00 00 00 00 00 50 1f 02 00 00 00 01 00  \n        00 00 54 65 73 74 77 6f 6c 66 00 01 00 00 00 00 00 00  \n        00 01 00 00 00 01 00 00 00 00 00 00 00 03 00 00 00 00  \n        00 00 00 00 02 00 00 00";
    const LIST_AFTER_LEAVING: &str = "10 00 00 00 01 00 00 00 00 00 50 1f 03 00 00 00 00 00  \n        00 00 00 00 00 00 00 00 00 00";
    const STATS_ABOUT_WATCHER: &str = "01 03 ff ff 07 00 01 00 3c 00 00 00 3c 00 00 00 01 00  \n        00 e8 03 01 00 0c 00 e1 dd bd ff 00 00 00 00 00 00 00  \n        00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00  \n        00 00 00 00 00 00 00 00 00 00 00 00 00 00 00";
    const STATS_ABOUT_TESTWOLF: &str = "01 01 ff ff 07 00 01 00 88 00 00 00 88 00 00 00 01 00  \n        00 e8 03 05 00 0c 00 27 dd 89 ff 00 00 00 00 00 00 00  \n        00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00  \n        00 00 00 00 00 00 00 00 00 00 00 00 00 00 00";
    const INVITE_ACCEPTED: &str = "00 00 00 00 57 61 74 63 68 65 72 00 00 00 00 00 00 00  \n        00 00";
    const LEAVE_ACCEPTED: &str = "02 00 00 00 54 65 73 74 77 6f 6c 66 00 00 00 00 00 00  \n        00 00 00";

    /// The invite names its sender and nothing else -- there is no guid in it,
    /// which is what makes accepting an unidentified request.
    #[test]
    fn an_invite_carries_the_inviters_name() {
        let invite = parse_group_invite(&bytes(INVITE_AT_WATCHER)).expect("invite parses");
        assert_eq!(invite.from, "Testwolf");
        assert!(invite.can_accept);
    }

    /// The whole group, read from both ends of the same party.
    ///
    /// **Each end's facts come from the other end's client**, which is this
    /// project's strongest shape: the structure goes out through one session,
    /// through the server, and back in through another, so nothing here is
    /// checked against itself. `Watcher`'s guid and name are what `Watcher`'s
    /// own client reported for itself, and they arrive in the packet
    /// `Testwolf` received.
    #[test]
    fn a_group_list_names_the_other_member_from_either_end() {
        let at_leader = parse_group_list(&bytes(LIST_AT_LEADER)).expect("leader's list parses");
        assert!(at_leader.in_group());
        assert_eq!(at_leader.leader, 1, "Testwolf leads");
        assert_eq!(
            at_leader.members.len(),
            1,
            "the reader is left out of their own list"
        );
        assert_eq!(at_leader.members[0].name, "Watcher");
        assert_eq!(at_leader.members[0].guid, 3);
        assert!(at_leader.members[0].is_online());
        assert!(!at_leader.members[0].is_dead());

        let at_member = parse_group_list(&bytes(LIST_AT_MEMBER)).expect("member's list parses");
        assert_eq!(at_member.members[0].name, "Testwolf");
        assert_eq!(at_member.members[0].guid, 1);
        // The same group, seen from both sides: one guid, one leader, and a
        // counter that differs because it increments per send.
        assert_eq!(at_leader.guid, at_member.guid);
        assert_eq!(at_leader.leader, at_member.leader);
        assert_ne!(at_leader.counter, at_member.counter);

        // The loot block, which is the part the body's own length proves: the
        // parse consumes it exactly, and these six fields are its last
        // thirteen bytes.
        let loot = at_leader
            .loot
            .expect("a group with members states its loot rule");
        assert_eq!(loot.method, 3);
        assert_eq!(loot.threshold, 2);
        assert_eq!(loot.master, 0);
    }

    /// **The packet that refuted the obvious reading.** Forming a group sends
    /// the leader a list with nobody else in it, one packet before the real
    /// one -- so "no members means no group" reports every group's creation as
    /// a departure. What separates the two is the leader guid, and both halves
    /// are asserted here: a test that only checked the leave form would pass
    /// just as well under the wrong rule.
    #[test]
    fn a_group_of_one_is_still_a_group_and_an_empty_leader_is_not() {
        let forming =
            parse_group_list(&bytes(LIST_AT_LEADER_BEFORE_JOIN)).expect("group-of-one parses");
        assert!(!forming.has_members(), "nobody else is in it yet");
        assert!(forming.in_group(), "but it is a group, and it has a leader");
        assert_eq!(forming.leader, 1);
        assert!(forming.loot.is_none(), "no members, no loot block");

        let left = parse_group_list(&bytes(LIST_AFTER_LEAVING)).expect("the leave form parses");
        assert!(!left.has_members());
        assert!(
            !left.in_group(),
            "a zero leader is the server saying there is none"
        );
        assert_eq!(left.leader, 0);
        assert!(left.loot.is_none());
        // Observed rather than relied on. One sample of one flag bit is a
        // guess; the leader guid above is not.
        assert_eq!(left.group_type, 0x10);
    }

    /// Twenty conditional fields behind one mask, and the check that they are
    /// all in the right place is that the cursor lands exactly on the end of
    /// the body -- which `parse_party_member_stats` enforces with `finish`.
    ///
    /// Every value below was reported independently by the client the packet
    /// is *about*: `Watcher` says it is level 1 with 60 health, and this is
    /// the packet `Testwolf` received.
    #[test]
    fn member_stats_agree_with_what_that_character_reports_about_itself() {
        let watcher =
            parse_party_member_stats(&bytes(STATS_ABOUT_WATCHER)).expect("Watcher's stats parse");
        assert_eq!(watcher.guid, 3);
        assert_eq!(
            watcher.mask, 0x0007_FFFF,
            "the server sent every field it has"
        );
        assert_eq!(watcher.health, Some(60));
        assert_eq!(watcher.max_health, Some(60));
        assert_eq!(watcher.level, Some(1));
        assert_eq!(watcher.zone, Some(12), "Elwynn Forest");
        // Rage, and a maximum of 1000 -- which is what `CLAUDE.md` records for
        // a warrior read out of the update fields, arrived at by a completely
        // different route.
        assert_eq!(watcher.power_type, Some(1));
        assert_eq!(watcher.max_power, Some(1000));

        let testwolf =
            parse_party_member_stats(&bytes(STATS_ABOUT_TESTWOLF)).expect("Testwolf's stats parse");
        assert_eq!(testwolf.guid, 1);
        assert_eq!(testwolf.health, Some(136));
        assert_eq!(testwolf.level, Some(5), "a different level from Watcher's");
    }

    /// **The one field where the wrong reading is silent**, and the reason two
    /// characters were needed rather than one.
    ///
    /// Elwynn is at negative coordinates. Read unsigned, `Watcher`'s position
    /// is `56801, 65469` -- a plausible pair of numbers that puts a map dot on
    /// the wrong continent and never errors. Read signed it is `-8735, -67`,
    /// which is where that client says it is. Two characters, two positions,
    /// both matching, is what makes this a measurement.
    #[test]
    fn a_members_position_is_signed_and_matches_where_they_stand() {
        let watcher = parse_party_member_stats(&bytes(STATS_ABOUT_WATCHER)).expect("parses");
        assert_eq!(watcher.position, Some((-8735, -67)));
        let testwolf = parse_party_member_stats(&bytes(STATS_ABOUT_TESTWOLF)).expect("parses");
        assert_eq!(testwolf.position, Some((-8921, -119)));
    }

    /// The only answer any group request gets, and it comes back for two
    /// different operations with the same shape.
    #[test]
    fn a_command_result_names_the_operation_and_who_it_was_about() {
        let invite =
            parse_party_command_result(&bytes(INVITE_ACCEPTED)).expect("invite result parses");
        assert_eq!(invite.operation, PartyOperation::INVITE);
        assert_eq!(invite.member, "Watcher");
        assert!(invite.is_ok());

        let leave = parse_party_command_result(&bytes(LEAVE_ACCEPTED)).expect("leave result parses");
        assert_eq!(leave.operation, PartyOperation::LEAVE);
        // The *sender's* name on a leave rather than the group's: this is the
        // server reporting who left, which happens to be us.
        assert_eq!(leave.member, "Testwolf");
        assert!(leave.is_ok());
    }

    /// The one bit of `group_type` that has to be interpreted rather than
    /// carried: an LFG group puts five extra bytes between the header and the
    /// group guid, so ignoring the flag reads every field after it late.
    ///
    /// Synthetic, and said so -- nothing here has ever been in a dungeon
    /// finder group. It exists because the conditional is in the parser, and
    /// an untested branch of a layout is exactly what desynchronises a body
    /// silently.
    #[test]
    fn an_lfg_group_list_puts_five_bytes_before_the_guid() {
        let ordinary = bytes(LIST_AT_LEADER_BEFORE_JOIN);
        let mut lfg = ordinary.clone();
        lfg[0] = GROUPTYPE_LFG;
        lfg.splice(4..4, [2, 0x02, 0x01, 0x00, 0x00]);

        let plain = parse_group_list(&ordinary).expect("ordinary group parses");
        let finder = parse_group_list(&lfg).expect("lfg group parses");
        assert_eq!(plain.guid, finder.guid);
        assert_eq!(plain.leader, finder.leader);

        // And the failure the flag exists to prevent: read as an ordinary
        // list, the same bytes give a different guid rather than an error.
        let mut misread = lfg.clone();
        misread[0] = 0;
        let wrong = parse_group_list(&misread);
        assert!(
            wrong.is_err() || wrong.unwrap().guid != plain.guid,
            "reading an LFG list as an ordinary one must not quietly agree"
        );
    }

    /// The absent fields are absent, not zero. A member hit while standing in
    /// another zone sends current health alone, and defaulting the rest would
    /// report them as level 0 with no mana.
    #[test]
    fn member_stats_read_only_the_fields_the_mask_names() {
        let mut body = Vec::new();
        crate::update::write_packed_guid(4, &mut body);
        body.extend_from_slice(&flag::CUR_HP.to_le_bytes());
        body.extend_from_slice(&42u32.to_le_bytes());

        let stats = parse_party_member_stats(&body).expect("stats parse");
        assert_eq!(stats.health, Some(42));
        assert_eq!(stats.max_health, None);
        assert_eq!(stats.level, None);
        assert_eq!(stats.zone, None);
        assert_eq!(stats.position, None);
    }

    /// An unnamed result code comes back as a number rather than as a
    /// plausible sentence. Same rule as `describe_cast_failure`.
    #[test]
    fn unnamed_party_results_describe_as_numbers() {
        assert_eq!(describe_party_result(PartyResult::OK), "it worked");
        assert_eq!(describe_party_result(199), "party result 199");
    }
}
