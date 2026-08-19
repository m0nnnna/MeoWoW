//! Guilds: the first list of people who are **not in the world**.
//!
//! Everything this client has read about another character so far has been
//! about somebody it could, in principle, walk up to. A replicated player is
//! an object with fields. A creature is an object with fields. Even 4.20's
//! party summary -- the packet written precisely because a group member two
//! zones away has no object -- describes somebody who is *logged in*, moving
//! around a map, with a health value that is currently true.
//!
//! A guild roster is mostly people who logged out days ago. There is no
//! object, no update, no query that answers for them, and no second source to
//! check any of it against. Every field about a guild member is whatever
//! `SMSG_GUILD_ROSTER` chose to carry, and the discipline 4.20 established --
//! an `Option` for anything the packet may not have said, and a bar drawn
//! empty rather than full -- applies to the whole record rather than to a few
//! vitals.
//!
//! ## The conditional field belongs to the absent
//!
//! The roster's member record is variable-length, and the variation is not
//! only in its strings. **Four bytes exist only for members who are
//! offline.**
//!
//! ```text
//! u64  guid
//! u8   status          <- 0 offline, otherwise a mask of online/afk/dnd/mobile
//! cstr name
//! u32  rank
//! u8   level
//! u8   class
//! u8   gender
//! u32  area
//! f32  offline_days    <- WRITTEN ONLY WHEN status == 0
//! cstr public_note
//! cstr officer_note
//! ```
//!
//! That is a conditional layout in the middle of a variable-length record
//! inside a list, which [`crate::mail`] already names as the worst place for
//! one: a misreading does not leave bytes at the end, it desynchronises every
//! record after it and turns the rest of the packet into plausible garbage.
//! Names come out as fragments of the previous member's note, levels come out
//! of the middle of a float, and nothing errors until the cursor reaches the
//! end short or long.
//!
//! It is also the wrong way round from a client author's instinct. The people
//! you can see carry *less*, because a member who is online has nothing to say
//! about how long they have been away.
//!
//! **The sample that can refute it has to contain both kinds**, and this is
//! knowable before a single packet is read, which is the whole point of the
//! rule this project keeps rediscovering. A roster where everybody is offline
//! cannot separate "always read the float" from "read it when offline"; a
//! roster where everybody is online cannot separate "never read it" from "read
//! it when offline". The natural first fixture -- make a guild, log both test
//! characters in, ask for the roster -- is exactly the second of those. The
//! fixture here is six members with one of them logged in.
//!
//! ## The packet says why its own field is blank
//!
//! The officer note is written for every member and is an empty string for all
//! of them when the reader's rank lacks [`RankRights::VIEW_OFFICER_NOTE`]. So
//! "this member has no officer note" and "you are not allowed to see officer
//! notes" arrive as identical bytes -- the shape this project calls *nothing
//! happened is two findings wearing one sentence*.
//!
//! Unusually, they are separable, and from the same packet: the roster carries
//! the rank table, and the reader's own rank is on their own member record. So
//! [`Roster::officer_notes_visible`] answers the question with no second
//! request, and an interface can draw an empty column headed *hidden* rather
//! than an empty column headed *none*.
//!
//! ## The rank names arrive before the count that says how many are real
//!
//! `SMSG_GUILD_QUERY_RESPONSE` writes **ten** rank names, always, most of them
//! empty, and then the emblem, and then the number of ranks that exist. A
//! parser that tried to read `rank_count` strings cannot: the count is behind
//! them. Reading fewer than ten leaves the five emblem words landing on string
//! bytes, and emblem words are small integers, so every one of them would look
//! perfectly plausible.
//!
//! ## One number, two meanings, and no way to tell
//!
//! `SMSG_GUILD_COMMAND_RESULT`'s result `8` is both *you do not have
//! permission* and *the guild master cannot leave their own guild*. They are
//! two names for one constant in the server's own enum, they arrive for
//! unrelated commands, and nothing in the packet separates them. This is
//! [`crate::trade`]'s `BUSY` over again, and it is handled the same way:
//! [`describe_command_result`] names only what has been produced deliberately
//! and returns the raw number for the rest.
//!
//! ## What bounds the silent requests
//!
//! Most of the guild block is silent on success, and the usual move is to
//! send an answered neighbour first. Here the answered neighbour is the
//! *same* request: `CMSG_GUILD_ROSTER` sent by a character **in no guild** is
//! answered by `SMSG_GUILD_COMMAND_RESULT` carrying command `5` and result
//! `9`. One send, from a character with nothing set up, bounds the roster
//! opcode number and the command-result layout together -- and unlike trade's
//! refusal it needs no deliberate mistake, because "I am not in a guild" is an
//! ordinary state rather than an error.

use crate::protocol::{Error, Reader};

/// Rank slots a guild can have, and the number of rank names
/// `SMSG_GUILD_QUERY_RESPONSE` always writes.
///
/// Ten is a hard limit rather than a convention: the server's rank vector is
/// capped at it, and the query response writes a fixed array of that size
/// whatever the guild actually uses. See the module comment for why the count
/// arriving *after* the names is load-bearing.
pub const MAX_RANKS: usize = 10;

/// Bank tabs a guild can buy, and the number of per-tab permission pairs every
/// rank record carries.
///
/// Six, always, whether or not the guild owns any: the rank record is fixed
/// width and the tabs it describes may not exist. Nothing here reads a bank --
/// the pairs are parsed because they are between the fields that matter and
/// skipping them by a computed length would be seeking by an unchecked number.
pub const BANK_TABS: usize = 6;

/// Bytes in one rank record: two words and six pairs.
///
/// **A fixed stride inside a variable-length packet**, which makes it the one
/// thing here that can be checked by arithmetic alone -- the rank block's
/// length is `rank_count * RANK_BYTES` and the member block starts exactly
/// there. 4.24's lesson applies to how it was arrived at rather than to the
/// number: the server's own packet writer is a better hypothesis than its
/// database schema was, and it is still only a hypothesis until a body of the
/// predicted length arrives.
pub const RANK_BYTES: usize = 4 + 4 + BANK_TABS * (4 + 4);

/// The smallest a member record can be: guid, status, an empty name, the four
/// fixed fields, no offline float, and two empty notes.
///
/// Used only to refuse an impossible count before allocating for it, the same
/// guard the vendor, trainer and mail lists carry. The *online* record is the
/// short one, which is why the minimum omits the float.
const MIN_MEMBER_BYTES: usize = 8 + 1 + 1 + 4 + 1 + 1 + 1 + 4 + 1 + 1;

/// The bits of [`GuildMember::status`].
///
/// Named because the roster's conditional field keys off this byte being
/// *zero*, and "zero" is a statement about the whole mask rather than about
/// the online bit: a member who is away is online, and their record is the
/// short one.
pub struct MemberStatus;

impl MemberStatus {
    pub const ONLINE: u8 = 0x01;
    pub const AFK: u8 = 0x02;
    pub const DND: u8 = 0x04;
    /// Signed in from the companion app rather than the game. Parsed, never
    /// produced here, and named so an unexpected `8` reads as something known
    /// rather than as a corrupt record.
    pub const MOBILE: u8 = 0x08;
}

/// The bits of [`GuildRank::rights`] this client acts on.
///
/// Only the ones something here reads are named. The rest of the word is
/// carried raw for the same reason [`crate::group::describe_loot_rule`] prints
/// numbers: a permission this client has never exercised is a permission it
/// cannot describe.
///
/// **Every one of them includes `0x40`**, which the server sets on an empty
/// rank as a base bit, so a test for a single right must mask rather than
/// compare.
pub struct RankRights;

impl RankRights {
    /// The base bit present on every right, including the empty one.
    pub const BASE: u32 = 0x0000_0040;
    /// Whether the officer note column carries anything. See the module
    /// comment: this is what separates "hidden" from "none".
    pub const VIEW_OFFICER_NOTE: u32 = 0x0000_4000;
    pub const INVITE: u32 = 0x0000_0010;
    pub const REMOVE: u32 = 0x0000_0020;
    pub const PROMOTE: u32 = 0x0000_0080;
    pub const DEMOTE: u32 = 0x0000_0100;
    pub const SET_MOTD: u32 = 0x0000_1000;
    pub const EDIT_PUBLIC_NOTE: u32 = 0x0000_2000;
    pub const EDIT_OFFICER_NOTE: u32 = 0x0000_8000;
}

/// One rank's permissions, as the roster describes them.
///
/// The rank's *name* is not here: it arrives in `SMSG_GUILD_QUERY_RESPONSE`
/// and the two packets are joined by position in their respective lists. That
/// is a join on an index rather than on an id, which this project normally
/// refuses -- and it is unavoidable, because neither packet carries a rank id
/// at all. What makes it safe is that a member's `rank` field is that same
/// position, so all three lists are indexed the same way by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuildRank {
    /// The permission mask. See [`RankRights`].
    pub rights: u32,
    /// Copper this rank may withdraw from the guild bank per day.
    pub withdraw_gold_limit: u32,
    /// Per-tab rights and withdrawal limits, six of them, whether or not the
    /// guild owns six tabs.
    pub tabs: [BankTabRights; BANK_TABS],
}

/// One rank's rights over one guild bank tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BankTabRights {
    pub rights: u32,
    pub withdraw_item_limit: u32,
}

/// One person on the roster.
///
/// **Almost nothing here can be checked against anything.** A replicated
/// player's level is in an update field and a party member's is in a stats
/// packet; a guild member's is in this record and nowhere else, because they
/// logged out on Tuesday. The fields are therefore carried exactly as sent,
/// with no defaulting and no inference, and the one field that may genuinely
/// be absent is an `Option` rather than a zero.
#[derive(Debug, Clone, PartialEq)]
pub struct GuildMember {
    pub guid: u64,
    /// The raw status mask. See [`MemberStatus`], and
    /// [`GuildMember::is_online`] for the test the record's own layout keys
    /// off.
    pub status: u8,
    pub name: String,
    /// Which rank, as an index into [`Roster::ranks`] and into the names in
    /// [`GuildInfo::ranks`]. `0` is the guild master.
    pub rank: u32,
    pub level: u8,
    /// The character's class, as a `ChrClasses` row. Not resolved here.
    pub class: u8,
    pub gender: u8,
    /// The `AreaTable` row this character was last standing in.
    ///
    /// For an offline member this is where they logged out, which is a fact
    /// about the past however current it looks beside the online members'.
    pub area: u32,
    /// Days since this member logged out -- **`None` for a member who is
    /// online, because the field is not written at all for them.**
    ///
    /// A duration and not an instant: the server computes it as
    /// `(now - logout) / 86400` at the moment the packet is built, so it ages
    /// with the packet rather than with the clock. `0.0` means *just now* and
    /// never *unknown*; unknown is the `None`.
    pub offline_days: Option<f32>,
    pub public_note: String,
    /// The officer note, which is an empty string both when there is none and
    /// when the reader may not see one. Ask [`Roster::officer_notes_visible`]
    /// before drawing it as absent.
    pub officer_note: String,
}

impl GuildMember {
    /// Whether this member is logged in *in any state*, including away and
    /// busy.
    ///
    /// This is the test the record's own layout keys off, and it is the whole
    /// mask rather than [`MemberStatus::ONLINE`]: the server writes the
    /// offline float when the byte is zero, so a member flagged only as away
    /// would take the wrong branch under a narrower test. No such member has
    /// been observed -- the server sets the online bit alongside the others --
    /// but a client that assumed so would be trusting an invariant nothing
    /// states.
    pub fn is_online(&self) -> bool {
        self.status != 0
    }

    pub fn is_afk(&self) -> bool {
        self.status & MemberStatus::AFK != 0
    }

    pub fn is_dnd(&self) -> bool {
        self.status & MemberStatus::DND != 0
    }
}

/// The whole roster, as one packet describes it.
///
/// Resent in full whenever it is asked for, like `SMSG_GROUP_LIST` and unlike
/// the party stats packet: there is no incremental member update. What *is*
/// incremental is [`GuildEvent`], which says a person signed on or off without
/// saying anything else about them, so the honest response to one is to ask
/// for the roster again rather than to edit a row.
#[derive(Debug, Clone, PartialEq)]
pub struct Roster {
    /// The message of the day, shown on login by the original client.
    pub motd: String,
    /// The longer information text, which the original client puts on a
    /// separate tab. Empty on most guilds.
    pub info: String,
    pub ranks: Vec<GuildRank>,
    pub members: Vec<GuildMember>,
}

impl Roster {
    /// Find a member by guid.
    pub fn member(&self, guid: u64) -> Option<&GuildMember> {
        self.members.iter().find(|m| m.guid == guid)
    }

    /// Whether the officer-note column carries anything for this reader.
    ///
    /// **The packet answers this about itself**, which is the reason it is
    /// worth a method: the reader's own member record names their rank, and
    /// the rank block names that rank's rights. An empty officer note is then
    /// *hidden* or *absent* rather than ambiguous.
    ///
    /// `None` when the reader is not on their own roster, which does not
    /// happen and is not asserted away: a client that guessed `true` there
    /// would label every note absent, and one that guessed `false` would label
    /// every note hidden. Neither is a thing worth drawing.
    pub fn officer_notes_visible(&self, reader: u64) -> Option<bool> {
        let rank = self.member(reader)?.rank as usize;
        let rights = self.ranks.get(rank)?.rights;
        Some(rights & RankRights::VIEW_OFFICER_NOTE != 0)
    }

    /// How many members are logged in, in any state.
    pub fn online(&self) -> usize {
        self.members.iter().filter(|m| m.is_online()).count()
    }
}

/// Parse `SMSG_GUILD_ROSTER`.
///
/// The member count comes first, the two texts next, and the rank count only
/// after them -- so nothing about this packet can be reached by arithmetic
/// until two variable-length strings have been read. That ordering is why the
/// count guard below is applied against the *remaining* bytes rather than
/// against the body length.
pub fn parse_roster(body: &[u8]) -> Result<Roster, Error> {
    let mut r = Reader::new(body, "SMSG_GUILD_ROSTER");
    let member_count = r.u32()?;
    let motd = r.cstring()?;
    let info = r.cstring()?;
    let rank_count = r.u32()?;

    // Both counts are `u32` and both sit behind variable-length fields, so a
    // header read one string out of place asks for gigabytes of records. The
    // guard is stated in terms of what is actually left, and it is checked
    // before either allocation.
    let need = (rank_count as usize)
        .saturating_mul(RANK_BYTES)
        .saturating_add((member_count as usize).saturating_mul(MIN_MEMBER_BYTES));
    if need > r.remaining() {
        return Err(Error::GuildRosterCounts {
            members: member_count,
            ranks: rank_count,
            expected: need,
            got: r.remaining(),
        });
    }

    let mut ranks = Vec::with_capacity(rank_count as usize);
    for _ in 0..rank_count {
        let rights = r.u32()?;
        let withdraw_gold_limit = r.u32()?;
        let mut tabs = [BankTabRights::default(); BANK_TABS];
        for tab in tabs.iter_mut() {
            tab.rights = r.u32()?;
            tab.withdraw_item_limit = r.u32()?;
        }
        ranks.push(GuildRank {
            rights,
            withdraw_gold_limit,
            tabs,
        });
    }

    let mut members = Vec::with_capacity(member_count as usize);
    for _ in 0..member_count {
        let guid = r.u64()?;
        let status = r.u8()?;
        let name = r.cstring()?;
        let rank = r.u32()?;
        let level = r.u8()?;
        let class = r.u8()?;
        let gender = r.u8()?;
        let area = r.u32()?;
        // The four bytes that exist only for the absent. See the module
        // comment: this branch is the whole shape of the milestone, and
        // taking it the wrong way desynchronises every record after it
        // rather than failing here.
        let offline_days = if status == 0 { Some(r.f32()?) } else { None };
        let public_note = r.cstring()?;
        let officer_note = r.cstring()?;
        members.push(GuildMember {
            guid,
            status,
            name,
            rank,
            level,
            class,
            gender,
            area,
            offline_days,
            public_note,
            officer_note,
        });
    }

    r.finish()?;
    Ok(Roster {
        motd,
        info,
        ranks,
        members,
    })
}

/// A guild's identity: what `CMSG_GUILD_QUERY` answers with.
///
/// **Answered for any guild id, with no membership required**, which makes it
/// the guild block's counterpart to `SMSG_QUEST_QUERY_RESPONSE`: it is the one
/// request that can name a guild this character has nothing to do with, and
/// therefore the only thing that can put a guild name under another player's
/// name plate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuildInfo {
    pub id: u32,
    pub name: String,
    /// The rank names, **trimmed to the count the packet ends with**. Indexed
    /// the same way [`GuildMember::rank`] is.
    pub ranks: Vec<String>,
    pub emblem: Emblem,
}

/// The tabard. Five numbers, drawn by nothing here.
///
/// Parsed rather than skipped because they sit between the rank names and the
/// rank count, and skipping them by a computed width would be seeking through
/// the very region a wrong string count would corrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Emblem {
    pub style: u32,
    pub color: u32,
    pub border_style: u32,
    pub border_color: u32,
    pub background: u32,
}

/// Parse `SMSG_GUILD_QUERY_RESPONSE`.
///
/// Ten rank names always travel. See the module comment for why the count at
/// the end cannot be used to decide how many to read.
pub fn parse_guild_info(body: &[u8]) -> Result<GuildInfo, Error> {
    let mut r = Reader::new(body, "SMSG_GUILD_QUERY_RESPONSE");
    let id = r.u32()?;
    let name = r.cstring()?;
    let mut ranks = Vec::with_capacity(MAX_RANKS);
    for _ in 0..MAX_RANKS {
        ranks.push(r.cstring()?);
    }
    let emblem = Emblem {
        style: r.u32()?,
        color: r.u32()?,
        border_style: r.u32()?,
        border_color: r.u32()?,
        background: r.u32()?,
    };
    let rank_count = r.u32()?;
    r.finish()?;

    if rank_count as usize > MAX_RANKS {
        return Err(Error::GuildRankCount { got: rank_count });
    }
    // The trailing names are empty padding, and dropping them here rather than
    // at the point of use means nothing downstream has to know that a guild
    // with five ranks still sent ten strings.
    ranks.truncate(rank_count as usize);

    Ok(GuildInfo {
        id,
        name,
        ranks,
        emblem,
    })
}

/// What the server said about a guild request.
///
/// The reply that bounds this whole block. It echoes the *command* it is
/// about, which is what ties an answer to a question the way
/// `SMSG_SEND_MAIL_RESULT` does and `SMSG_TRADE_STATUS` does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    /// See [`GuildCommand`].
    pub command: u32,
    /// The player or guild name the command was about, empty for commands that
    /// name nobody.
    pub name: String,
    /// See [`describe_command_result`], and note that `8` means two things.
    pub result: u32,
}

/// The values of [`CommandResult::command`] this client sends.
pub struct GuildCommand;

impl GuildCommand {
    pub const CREATE: u32 = 0;
    pub const INVITE: u32 = 1;
    pub const QUIT: u32 = 3;
    /// **The bounding one.** Asked by a character in no guild, it comes back
    /// with [`GuildResult::PLAYER_NOT_IN_GUILD`], which confirms the roster
    /// opcode and this packet's layout in a single send.
    pub const ROSTER: u32 = 5;
    pub const PROMOTE: u32 = 6;
    pub const DEMOTE: u32 = 7;
    pub const REMOVE: u32 = 8;
    pub const CHANGE_LEADER: u32 = 10;
    pub const EDIT_MOTD: u32 = 11;
    pub const PUBLIC_NOTE: u32 = 19;
}

/// The values of [`CommandResult::result`].
pub struct GuildResult;

impl GuildResult {
    pub const SUCCESS: u32 = 0;
    /// **Two meanings and no way to tell.** The server's enum spells `8` both
    /// `ERR_GUILD_PERMISSIONS` and `ERR_GUILD_LEADER_LEAVE`, for unrelated
    /// commands, and nothing in the packet separates them. Named here so the
    /// collision is recorded; [`describe_command_result`] deliberately does
    /// not turn it into a sentence.
    pub const PERMISSIONS_OR_LEADER_LEAVE: u32 = 8;
    pub const PLAYER_NOT_IN_GUILD: u32 = 9;
    pub const PLAYER_NOT_FOUND: u32 = 11;
    pub const ALREADY_IN_GUILD: u32 = 3;
}

/// Say what a guild command result means, or say nothing.
///
/// Same rule as [`crate::spell::describe_cast_failure`] and
/// [`crate::group::describe_loot_rule`]: only codes this client has actually
/// produced against a realm get a sentence, and everything else prints its
/// number. A wrong sentence here misexplains what happened and sends the next
/// reader somewhere else, which costs more than a bare number.
///
/// `8` is deliberately absent even though it has been produced, because the
/// two meanings the server gives it are not separable from the packet.
pub fn describe_command_result(command: u32, result: u32, name: &str) -> String {
    match result {
        GuildResult::SUCCESS => format!("guild command {command} succeeded"),
        GuildResult::PLAYER_NOT_IN_GUILD => "You are not in a guild.".into(),
        GuildResult::PLAYER_NOT_FOUND => format!("There is no player named {name}."),
        GuildResult::ALREADY_IN_GUILD => format!("{name} is already in a guild."),
        other => format!("guild command {command} returned {other}"),
    }
}

/// Parse `SMSG_GUILD_COMMAND_RESULT`.
pub fn parse_command_result(body: &[u8]) -> Result<CommandResult, Error> {
    let mut r = Reader::new(body, "SMSG_GUILD_COMMAND_RESULT");
    let command = r.u32()?;
    let name = r.cstring()?;
    let result = r.u32()?;
    r.finish()?;
    Ok(CommandResult {
        command,
        name,
        result,
    })
}

/// Something happened to the guild.
///
/// The only *push* in the block, and the reason a roster does not have to be
/// polled. Its tail is conditional on its type: the four events about a person
/// arriving or leaving carry that person's guid and the rest carry nothing,
/// which is a shape the cursor catches rather than one that hides -- a wrong
/// reading leaves eight bytes over at the end of a very short packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuildEvent {
    /// See [`GuildEventType`].
    pub kind: u8,
    /// Names, however many the event needs -- one for a sign-on, two for a
    /// promotion (who, and by whom), three for a rank change.
    pub params: Vec<String>,
    /// Whose guid, for the four events that carry one.
    pub guid: Option<u64>,
}

/// The values of [`GuildEvent::kind`].
///
/// Named in full because they are a closed set the server writes literally,
/// and because the four with a guid have to be named to parse the packet at
/// all -- unlike a status code, this enum is load-bearing rather than
/// descriptive.
pub struct GuildEventType;

impl GuildEventType {
    pub const PROMOTION: u8 = 0;
    pub const DEMOTION: u8 = 1;
    pub const MOTD: u8 = 2;
    pub const JOINED: u8 = 3;
    pub const LEFT: u8 = 4;
    pub const REMOVED: u8 = 5;
    pub const LEADER_IS: u8 = 6;
    pub const LEADER_CHANGED: u8 = 7;
    pub const DISBANDED: u8 = 8;
    pub const TABARD_CHANGE: u8 = 9;
    pub const RANK_UPDATED: u8 = 10;
    pub const RANK_DELETED: u8 = 11;
    pub const SIGNED_ON: u8 = 12;
    pub const SIGNED_OFF: u8 = 13;

    /// Whether an event of this type carries a trailing guid.
    ///
    /// The four that do are exactly the four about somebody arriving or
    /// leaving. Stated once, here, rather than at the parse site, because the
    /// same rule decides whether a *sent* event could be round-tripped -- the
    /// lesson from defining a both-ways structure once.
    pub fn carries_guid(kind: u8) -> bool {
        matches!(
            kind,
            Self::JOINED | Self::LEFT | Self::SIGNED_ON | Self::SIGNED_OFF
        )
    }
}

/// Parse `SMSG_GUILD_EVENT`.
pub fn parse_guild_event(body: &[u8]) -> Result<GuildEvent, Error> {
    let mut r = Reader::new(body, "SMSG_GUILD_EVENT");
    let kind = r.u8()?;
    let count = r.u8()?;
    let mut params = Vec::with_capacity(count as usize);
    for _ in 0..count {
        params.push(r.cstring()?);
    }
    let guid = if GuildEventType::carries_guid(kind) {
        Some(r.u64()?)
    } else {
        None
    };
    r.finish()?;
    Ok(GuildEvent {
        kind,
        params,
        guid,
    })
}

/// Somebody has asked this character to join their guild.
///
/// Two names and **no guid and no guild id**, so accepting it identifies
/// nothing: `CMSG_GUILD_ACCEPT` has an empty body and the server resolves the
/// guild from the invitation it recorded when the invite went out. Exactly
/// [`crate::group::GroupInvite`]'s shape, and for the same reason -- a
/// character holds one pending invitation at a time, so there is nothing to
/// disambiguate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuildInvitation {
    pub inviter: String,
    pub guild: String,
}

/// Parse `SMSG_GUILD_INVITE`.
pub fn parse_guild_invite(body: &[u8]) -> Result<GuildInvitation, Error> {
    let mut r = Reader::new(body, "SMSG_GUILD_INVITE");
    let inviter = r.cstring()?;
    let guild = r.cstring()?;
    r.finish()?;
    Ok(GuildInvitation { inviter, guild })
}

/// The guild's summary: what `CMSG_GUILD_INFO` answers with.
///
/// Small, and interesting for one field. See [`PackedDate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuildSummary {
    pub name: String,
    pub founded: PackedDate,
    pub members: u32,
    /// Distinct *accounts*, which is smaller than the member count wherever
    /// one person has several characters in the guild -- and is the only place
    /// the wire ever says so.
    pub accounts: u32,
}

/// A date as the server's clock read it, packed into one word.
///
/// **Not a timestamp.** The server takes its own local calendar breakdown and
/// packs the fields, so what arrives is a wall-clock reading with no seconds
/// and no timezone: it cannot be converted back to an instant without knowing
/// where the server thinks it is. A client that treated it as a Unix time, or
/// as UTC, would show a founding date some hours out and would never find out
/// -- the failure mode this project calls *a number nobody can check*.
///
/// The layout is confirmed rather than transcribed: the weekday is stored
/// *and* derivable from the date, so [`PackedDate::weekday_agrees`] checks the
/// packing against itself, and the fixture guild's founding is checked against
/// the realm's own `guild.createdate` in the integration tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedDate {
    /// Years since 2000. Stored as `tm_year - 100`, and `tm_year` counts from
    /// 1900.
    pub year: u32,
    /// 1-12. Stored zero-based.
    pub month: u32,
    /// 1-31. Stored zero-based.
    pub day: u32,
    /// 0 = Sunday, as `tm_wday`. **Redundant with the date**, which is what
    /// makes it evidence.
    pub weekday: u32,
    pub hour: u32,
    pub minute: u32,
    /// The word as it arrived, kept so a body that fails the redundancy check
    /// can still be printed.
    pub raw: u32,
}

impl PackedDate {
    /// Unpack the word.
    ///
    /// ```text
    /// bits  0..5   minute
    /// bits  6..10  hour
    /// bits 11..13  weekday
    /// bits 14..19  day - 1
    /// bits 20..23  month
    /// bits 24..31  year - 100
    /// ```
    pub fn unpack(raw: u32) -> Self {
        PackedDate {
            year: 2000 + (raw >> 24),
            month: ((raw >> 20) & 0xF) + 1,
            day: ((raw >> 14) & 0x3F) + 1,
            weekday: (raw >> 11) & 0x7,
            hour: (raw >> 6) & 0x1F,
            minute: raw & 0x3F,
            raw,
        }
    }

    /// Whether the stored weekday matches the one the stored date implies.
    ///
    /// Three bits of redundancy, and the same use as the trade slot's leading
    /// index and `md5translate.trs`'s `dir:` lines: a field that repeats
    /// something already present costs nothing to check and refutes a wrong
    /// bit layout immediately. Every plausible mis-shift moves the day or the
    /// month without moving the weekday, so they stop agreeing.
    ///
    /// Sakamoto's method, which is exact for the proleptic Gregorian calendar
    /// and needs no date library.
    pub fn weekday_agrees(&self) -> bool {
        const T: [u32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        if !(1..=12).contains(&self.month) || !(1..=31).contains(&self.day) {
            return false;
        }
        let y = if self.month < 3 {
            self.year - 1
        } else {
            self.year
        };
        let computed = (y + y / 4 - y / 100 + y / 400 + T[(self.month - 1) as usize] + self.day) % 7;
        computed == self.weekday
    }
}

/// Parse `SMSG_GUILD_INFO`.
pub fn parse_guild_summary(body: &[u8]) -> Result<GuildSummary, Error> {
    let mut r = Reader::new(body, "SMSG_GUILD_INFO");
    let name = r.cstring()?;
    let founded = PackedDate::unpack(r.u32()?);
    let members = r.u32()?;
    let accounts = r.u32()?;
    r.finish()?;
    Ok(GuildSummary {
        name,
        founded,
        members,
        accounts,
    })
}

/// Body for `CMSG_GUILD_QUERY`: the guild id, and nothing else.
pub fn guild_query_body(guild_id: u32) -> Vec<u8> {
    guild_id.to_le_bytes().to_vec()
}

/// Body for the requests that name one player by name.
///
/// `CMSG_GUILD_INVITE`, `CMSG_GUILD_PROMOTE`, `CMSG_GUILD_DEMOTE`,
/// `CMSG_GUILD_REMOVE` and `CMSG_GUILD_LEADER` all take exactly this.
///
/// **A name and not a guid**, which is the one place the guild block is
/// friendlier than the rest of the protocol and the one place it is more
/// fragile: the server normalises the name and looks it up, so a member who is
/// offline can be promoted or removed by a client that has never seen their
/// guid -- and two guilds' worth of careful guid handling elsewhere buys
/// nothing here.
pub fn named_player_body(name: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(name.len() + 1);
    body.extend_from_slice(name.as_bytes());
    body.push(0);
    body
}

/// Body for `CMSG_GUILD_MOTD` and `CMSG_GUILD_INFO_TEXT`: one string.
pub fn text_body(text: &str) -> Vec<u8> {
    named_player_body(text)
}

/// Body for `CMSG_GUILD_SET_PUBLIC_NOTE` and `CMSG_GUILD_SET_OFFICER_NOTE`:
/// whose note, then the note.
///
/// The two opcodes share this body exactly and differ only in their number,
/// which is why they are built by one function -- two copies of a layout that
/// must agree is the drift this project defines structures once to avoid.
pub fn member_note_body(member: &str, note: &str) -> Vec<u8> {
    let mut body = named_player_body(member);
    body.extend_from_slice(note.as_bytes());
    body.push(0);
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cstr(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    fn rank_bytes(rights: u32) -> Vec<u8> {
        let mut v = rights.to_le_bytes().to_vec();
        v.extend_from_slice(&0u32.to_le_bytes());
        for _ in 0..BANK_TABS {
            v.extend_from_slice(&0u32.to_le_bytes());
            v.extend_from_slice(&0u32.to_le_bytes());
        }
        v
    }

    /// One member record, built the way the server builds it -- including the
    /// float that exists only when the status byte is zero.
    fn member_bytes(guid: u64, status: u8, name: &str, rank: u32, level: u8) -> Vec<u8> {
        let mut v = guid.to_le_bytes().to_vec();
        v.push(status);
        v.extend_from_slice(&cstr(name));
        v.extend_from_slice(&rank.to_le_bytes());
        v.push(level);
        v.push(1); // class
        v.push(0); // gender
        v.extend_from_slice(&12u32.to_le_bytes());
        if status == 0 {
            v.extend_from_slice(&2.5f32.to_le_bytes());
        }
        v.extend_from_slice(&cstr("pub"));
        v.extend_from_slice(&cstr("off"));
        v
    }

    fn roster_body() -> Vec<u8> {
        let mut body = 2u32.to_le_bytes().to_vec();
        body.extend_from_slice(&cstr("motd"));
        body.extend_from_slice(&cstr("info"));
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&rank_bytes(RankRights::BASE | RankRights::VIEW_OFFICER_NOTE));
        body.extend_from_slice(&member_bytes(1, MemberStatus::ONLINE, "Online", 0, 10));
        body.extend_from_slice(&member_bytes(2, 0, "Offline", 0, 20));
        body
    }

    #[test]
    fn roster_reads_both_kinds_of_member() {
        let roster = parse_roster(&roster_body()).expect("roster");
        assert_eq!(roster.motd, "motd");
        assert_eq!(roster.info, "info");
        assert_eq!(roster.ranks.len(), 1);
        assert_eq!(roster.members.len(), 2);

        // The finding, asserted from both sides. Either half alone passes
        // under a parser that always reads the float or never does.
        assert_eq!(roster.members[0].name, "Online");
        assert!(roster.members[0].is_online());
        assert_eq!(roster.members[0].offline_days, None);

        assert_eq!(roster.members[1].name, "Offline");
        assert!(!roster.members[1].is_online());
        assert_eq!(roster.members[1].offline_days, Some(2.5));

        // The second record's fields survived the first record's branch,
        // which is the thing a wrong branch destroys.
        assert_eq!(roster.members[1].level, 20);
        assert_eq!(roster.members[1].public_note, "pub");
        assert_eq!(roster.online(), 1);
    }

    /// A parser that read the offline float unconditionally would consume four
    /// bytes of the online member's public note and then run off the end. The
    /// point of this test is that the *cursor* is what catches it: nothing
    /// about the mis-parse is visible field by field.
    #[test]
    fn reading_the_offline_float_for_an_online_member_desynchronises() {
        let body = roster_body();
        let mut r = Reader::new(&body, "hand-parse");
        assert_eq!(r.u32().unwrap(), 2);
        r.cstring().unwrap();
        r.cstring().unwrap();
        assert_eq!(r.u32().unwrap(), 1);
        r.skip(RANK_BYTES).unwrap();
        // The online member, read as though it carried the float.
        r.u64().unwrap();
        r.u8().unwrap();
        assert_eq!(r.cstring().unwrap(), "Online");
        r.u32().unwrap();
        r.u8().unwrap();
        r.u8().unwrap();
        r.u8().unwrap();
        r.u32().unwrap();
        r.f32().unwrap(); // the four bytes that are not there
        // What comes out is not "pub" -- it is whatever the note's bytes and
        // the following record's guid happen to spell.
        assert_ne!(r.cstring().unwrap(), "pub");
    }

    #[test]
    fn officer_notes_are_hidden_or_absent_and_the_packet_says_which() {
        let visible = parse_roster(&roster_body()).expect("roster");
        assert_eq!(visible.officer_notes_visible(1), Some(true));

        // The same roster from a reader whose rank lacks the right.
        let mut body = 2u32.to_le_bytes().to_vec();
        body.extend_from_slice(&cstr("motd"));
        body.extend_from_slice(&cstr("info"));
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&rank_bytes(RankRights::BASE));
        body.extend_from_slice(&member_bytes(1, MemberStatus::ONLINE, "Online", 0, 10));
        body.extend_from_slice(&member_bytes(2, 0, "Offline", 0, 20));
        let hidden = parse_roster(&body).expect("roster");
        assert_eq!(hidden.officer_notes_visible(1), Some(false));

        // A reader who is not on the roster gets neither answer.
        assert_eq!(hidden.officer_notes_visible(99), None);
    }

    #[test]
    fn a_member_count_the_body_cannot_hold_is_refused_before_allocating() {
        let mut body = 100_000u32.to_le_bytes().to_vec();
        body.extend_from_slice(&cstr(""));
        body.extend_from_slice(&cstr(""));
        body.extend_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            parse_roster(&body),
            Err(Error::GuildRosterCounts { .. })
        ));
    }

    #[test]
    fn guild_info_reads_ten_names_and_keeps_the_ones_that_exist() {
        let mut body = 7u32.to_le_bytes().to_vec();
        body.extend_from_slice(&cstr("Cat Herders"));
        for i in 0..MAX_RANKS {
            body.extend_from_slice(&cstr(if i < 3 {
                ["Master", "Officer", "Member"][i]
            } else {
                ""
            }));
        }
        for v in [3u32, 5, 2, 7, 4] {
            body.extend_from_slice(&v.to_le_bytes());
        }
        body.extend_from_slice(&3u32.to_le_bytes());

        let info = parse_guild_info(&body).expect("guild info");
        assert_eq!(info.id, 7);
        assert_eq!(info.name, "Cat Herders");
        assert_eq!(info.ranks, ["Master", "Officer", "Member"]);
        // Five distinct values, so a swap between any two of them shows.
        assert_eq!(
            info.emblem,
            Emblem {
                style: 3,
                color: 5,
                border_style: 2,
                border_color: 7,
                background: 4,
            }
        );
    }

    /// Reading `rank_count` names instead of ten leaves the emblem words on
    /// string bytes -- and every one of them still looks like a plausible
    /// small integer, which is why the count cannot be trusted to bound the
    /// array even though it is right about how many ranks exist.
    #[test]
    fn reading_fewer_than_ten_rank_names_misplaces_the_emblem() {
        let mut body = 7u32.to_le_bytes().to_vec();
        body.extend_from_slice(&cstr("Cat Herders"));
        for i in 0..MAX_RANKS {
            body.extend_from_slice(&cstr(if i < 3 {
                ["Master", "Officer", "Member"][i]
            } else {
                ""
            }));
        }
        for v in [3u32, 5, 2, 7, 4] {
            body.extend_from_slice(&v.to_le_bytes());
        }
        body.extend_from_slice(&3u32.to_le_bytes());

        let mut r = Reader::new(&body, "hand-parse");
        r.u32().unwrap();
        r.cstring().unwrap();
        for _ in 0..3 {
            r.cstring().unwrap();
        }
        // Seven empty strings still to come; the emblem read here is the
        // seven zero bytes plus the real style word, and it is not 3.
        assert_ne!(r.u32().unwrap(), 3);
    }

    #[test]
    fn a_command_result_is_the_bounding_reply() {
        let mut body = GuildCommand::ROSTER.to_le_bytes().to_vec();
        body.extend_from_slice(&cstr(""));
        body.extend_from_slice(&GuildResult::PLAYER_NOT_IN_GUILD.to_le_bytes());
        let result = parse_command_result(&body).expect("command result");
        assert_eq!(result.command, GuildCommand::ROSTER);
        assert_eq!(result.result, GuildResult::PLAYER_NOT_IN_GUILD);
        assert_eq!(
            describe_command_result(result.command, result.result, &result.name),
            "You are not in a guild."
        );
    }

    /// `8` has two meanings and is deliberately not turned into a sentence.
    #[test]
    fn the_ambiguous_result_prints_its_number() {
        let text = describe_command_result(
            GuildCommand::QUIT,
            GuildResult::PERMISSIONS_OR_LEADER_LEAVE,
            "",
        );
        assert!(text.contains('8'), "{text}");
    }

    #[test]
    fn only_the_arrival_and_departure_events_carry_a_guid() {
        let mut body = vec![GuildEventType::SIGNED_ON, 1];
        body.extend_from_slice(&cstr("Watcher"));
        body.extend_from_slice(&3u64.to_le_bytes());
        let event = parse_guild_event(&body).expect("event");
        assert_eq!(event.kind, GuildEventType::SIGNED_ON);
        assert_eq!(event.params, ["Watcher"]);
        assert_eq!(event.guid, Some(3));

        // The other half: a motd change has no guid, and a parser that read
        // one would come up eight bytes short rather than silently.
        let mut body = vec![GuildEventType::MOTD, 1];
        body.extend_from_slice(&cstr("Mice are for sharing."));
        let event = parse_guild_event(&body).expect("event");
        assert_eq!(event.guid, None);
        assert_eq!(event.params, ["Mice are for sharing."]);
    }

    #[test]
    fn an_event_with_a_guid_where_none_belongs_is_refused() {
        let mut body = vec![GuildEventType::MOTD, 0];
        body.extend_from_slice(&7u64.to_le_bytes());
        assert!(matches!(
            parse_guild_event(&body),
            Err(Error::Trailing { .. })
        ));
    }

    #[test]
    fn an_invitation_names_two_things_and_identifies_neither() {
        let mut body = cstr("Testwolf");
        body.extend_from_slice(&cstr("Cat Herders"));
        let invite = parse_guild_invite(&body).expect("invite");
        assert_eq!(invite.inviter, "Testwolf");
        assert_eq!(invite.guild, "Cat Herders");
    }

    /// The redundancy check, run over a date whose weekday is known
    /// independently: 19 August 2026 is a Wednesday, `tm_wday` 3.
    #[test]
    fn a_packed_date_checks_its_own_weekday() {
        let raw = (26u32 << 24) | (7u32 << 20) | (18u32 << 14) | (3u32 << 11) | (14u32 << 6) | 39;
        let date = PackedDate::unpack(raw);
        assert_eq!(
            (date.year, date.month, date.day, date.hour, date.minute),
            (2026, 8, 19, 14, 39)
        );
        assert!(date.weekday_agrees());

        // Any shift of the day field moves the date without moving the
        // weekday, and the check notices.
        let wrong = PackedDate::unpack(raw + (1 << 14));
        assert_eq!(wrong.day, 20);
        assert!(!wrong.weekday_agrees());
    }

    #[test]
    fn a_note_body_is_two_strings() {
        assert_eq!(
            member_note_body("Watcher", "watches"),
            b"Watcher\0watches\0".to_vec()
        );
    }
}
