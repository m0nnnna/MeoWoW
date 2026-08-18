//! Chat.
//!
//! `SMSG_MESSAGECHAT` is the most layout-dependent packet this client parses.
//! Its shape is decided by its own first byte: a monster's line carries the
//! speaker's name inline, a channel line carries the channel, an achievement
//! line carries an id on the end, and an ordinary say carries none of those.
//! Every one of those variants is the same handful of leading fields followed
//! by something different, which is precisely the shape where a wrong guess
//! parses perfectly and returns nonsense.
//!
//! So the defence this project already knows about is applied deliberately:
//! every variant is read through one cursor and ends at [`Reader::finish`], and
//! the tests below walk each variant *and* assert that trailing bytes fail. A
//! variant read with the wrong shape does not produce a slightly wrong message;
//! it produces a message whose text is somebody's guid, and the leftover count
//! is the only thing that says so.

use crate::protocol::{Error, Reader};

/// Where a line of chat came from.
///
/// Only the values this client can currently receive or send are named; the
/// full 3.3.5a table is longer, and an unnamed value is kept as its number
/// rather than folded into `Say`, so an unfamiliar line shows up as
/// unfamiliar rather than as a plausible wrong one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatType {
    Say,
    Party,
    Raid,
    Guild,
    Officer,
    Yell,
    Whisper,
    WhisperForeign,
    WhisperInform,
    Emote,
    TextEmote,
    System,
    Channel,
    MonsterSay,
    MonsterParty,
    MonsterYell,
    MonsterEmote,
    MonsterWhisper,
    RaidBossEmote,
    RaidBossWhisper,
    BattlegroundNeutral,
    BattlegroundAlliance,
    BattlegroundHorde,
    Achievement,
    GuildAchievement,
    Other(u8),
}

impl ChatType {
    /// **`System` is zero and `Say` is one.**
    ///
    /// Worth stating loudly, because assuming the obvious costs a silent
    /// failure rather than an error: a client that sends type 0 is claiming to
    /// be the server announcing something, which it is not allowed to do, so
    /// the message is dropped with no reply at all. That is indistinguishable
    /// from a malformed packet, a muted account, or a wrong opcode, and it
    /// sent this milestone chasing all three. `CLAUDE.md` already records a
    /// result-code enum off by one as an earlier bug of the same shape; this
    /// is the same mistake in a different table.
    pub fn from_id(id: u8) -> Self {
        match id {
            0x00 => ChatType::System,
            0x01 => ChatType::Say,
            0x02 => ChatType::Party,
            0x03 => ChatType::Raid,
            0x04 => ChatType::Guild,
            0x05 => ChatType::Officer,
            0x06 => ChatType::Yell,
            0x07 => ChatType::Whisper,
            0x08 => ChatType::WhisperForeign,
            0x09 => ChatType::WhisperInform,
            0x0A => ChatType::Emote,
            0x0B => ChatType::TextEmote,
            0x0C => ChatType::MonsterSay,
            0x0D => ChatType::MonsterParty,
            0x0E => ChatType::MonsterYell,
            0x0F => ChatType::MonsterWhisper,
            0x10 => ChatType::MonsterEmote,
            0x11 => ChatType::Channel,
            0x24 => ChatType::BattlegroundNeutral,
            0x25 => ChatType::BattlegroundAlliance,
            0x26 => ChatType::BattlegroundHorde,
            0x29 => ChatType::RaidBossEmote,
            0x2A => ChatType::RaidBossWhisper,
            0x30 => ChatType::Achievement,
            0x31 => ChatType::GuildAchievement,
            other => ChatType::Other(other),
        }
    }

    pub fn id(self) -> u8 {
        match self {
            ChatType::System => 0x00,
            ChatType::Say => 0x01,
            ChatType::Party => 0x02,
            ChatType::Raid => 0x03,
            ChatType::Guild => 0x04,
            ChatType::Officer => 0x05,
            ChatType::Yell => 0x06,
            ChatType::Whisper => 0x07,
            ChatType::WhisperForeign => 0x08,
            ChatType::WhisperInform => 0x09,
            ChatType::Emote => 0x0A,
            ChatType::TextEmote => 0x0B,
            ChatType::MonsterSay => 0x0C,
            ChatType::MonsterParty => 0x0D,
            ChatType::MonsterYell => 0x0E,
            ChatType::MonsterWhisper => 0x0F,
            ChatType::MonsterEmote => 0x10,
            ChatType::Channel => 0x11,
            ChatType::BattlegroundNeutral => 0x24,
            ChatType::BattlegroundAlliance => 0x25,
            ChatType::BattlegroundHorde => 0x26,
            ChatType::RaidBossEmote => 0x29,
            ChatType::RaidBossWhisper => 0x2A,
            ChatType::Achievement => 0x30,
            ChatType::GuildAchievement => 0x31,
            ChatType::Other(id) => id,
        }
    }

    /// Whether this kind carries the speaker's name inline.
    ///
    /// Creatures do, because a client cannot name-query something that may
    /// already be dead by the time the answer arrives. Players do not: their
    /// guid is all that is sent, and the name has to come from a query.
    pub fn carries_sender_name(self) -> bool {
        matches!(
            self,
            ChatType::MonsterSay
                | ChatType::MonsterYell
                | ChatType::MonsterEmote
                | ChatType::MonsterWhisper
                | ChatType::RaidBossEmote
                | ChatType::RaidBossWhisper
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            ChatType::Say => "say",
            ChatType::Party => "party",
            ChatType::Raid => "raid",
            ChatType::Guild => "guild",
            ChatType::Officer => "officer",
            ChatType::Yell => "yell",
            ChatType::Whisper | ChatType::WhisperInform | ChatType::WhisperForeign => "whisper",
            ChatType::Emote | ChatType::TextEmote => "emote",
            ChatType::System => "system",
            ChatType::Channel => "channel",
            ChatType::MonsterSay => "say",
            ChatType::MonsterParty => "party",
            ChatType::MonsterYell => "yell",
            ChatType::MonsterEmote | ChatType::RaidBossEmote => "emote",
            ChatType::MonsterWhisper | ChatType::RaidBossWhisper => "whisper",
            ChatType::BattlegroundNeutral
            | ChatType::BattlegroundAlliance
            | ChatType::BattlegroundHorde => "battleground",
            ChatType::Achievement | ChatType::GuildAchievement => "achievement",
            ChatType::Other(_) => "chat",
        }
    }
}

/// One line of chat off the wire.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub chat_type: ChatType,
    pub language: u32,
    pub sender: u64,
    /// Present for the kinds that carry it inline -- see
    /// [`ChatType::carries_sender_name`] -- and also for anything that
    /// arrived as `SMSG_GM_MESSAGECHAT`, whatever [`ChatType`] it carries; see
    /// [`parse_gm_message_chat`]. Otherwise `None`, and the name has to come
    /// from the name cache.
    pub sender_name: Option<String>,
    pub target: u64,
    /// Set for [`ChatType::Channel`].
    pub channel: Option<String>,
    pub text: String,
    /// AFK, DND, GM and so on.
    pub tag: u8,
}

/// Parses `SMSG_MESSAGECHAT`.
pub fn parse_message_chat(body: &[u8]) -> Result<ChatMessage, Error> {
    parse_message_chat_body(body, "SMSG_MESSAGECHAT", false)
}

/// Parses `SMSG_GM_MESSAGECHAT`.
///
/// **Measured from a live capture, not shared with the ordinary parser above
/// despite the identical opcode comment that used to sit here** ("a GM's line
/// shares the body, so it shares the parser") -- that was never actually
/// tested against a GM-flagged account until this project's own party chat
/// went out from one and came back undecodable with seventeen trailing bytes.
/// A GM message carries the sender's name inline, exactly like a monster's
/// line, regardless of what [`ChatType`] the message itself is: a `Party`
/// line from `Testwolf`, a GM-level character on the local realm, came back
/// as `0x02 0x07000000 <8-byte guid> 0x00000000 0x09000000 "Testwolf\0" <8
/// zero bytes> 0x05000000 "test\0" 0x00`, which is exactly the monster-shaped
/// layout -- a name length, the name, then the ordinary target/length/text/tag
/// tail -- and consumes the packet to its last byte. `ChatType::Party` itself
/// says nothing about this; only the *opcode* the server chose to answer on
/// does, which is why this needs a name distinct from
/// [`ChatType::carries_sender_name`] rather than a new variant of it.
pub fn parse_gm_message_chat(body: &[u8]) -> Result<ChatMessage, Error> {
    parse_message_chat_body(body, "SMSG_GM_MESSAGECHAT", true)
}

fn parse_message_chat_body(
    body: &[u8],
    label: &'static str,
    always_carries_name: bool,
) -> Result<ChatMessage, Error> {
    let mut r = Reader::new(body, label);
    let chat_type = ChatType::from_id(r.u8()?);
    let language = r.u32()?;
    let sender = r.u64()?;
    // Reserved, and always zero in practice. Read rather than skipped so its
    // width is asserted by `finish` like everything else.
    let _flags = r.u32()?;

    let mut sender_name = None;
    let mut channel = None;
    let target;

    if always_carries_name || chat_type.carries_sender_name() {
        // A length *and* a NUL terminator, and the length counts the
        // terminator. Reading the string by its length rather than to its NUL
        // would leave the terminator behind and desynchronise everything after
        // it -- which `finish` catches, but only at the end.
        let _length = r.u32()?;
        sender_name = Some(r.cstring()?);
        target = r.u64()?;
        // A non-player target names itself inline too, for the same reason
        // the sender does.
        if target != 0 && !is_player_guid(target) {
            let _length = r.u32()?;
            r.cstring()?;
        }
    } else {
        if chat_type == ChatType::Channel {
            channel = Some(r.cstring()?);
        }
        target = r.u64()?;
    }

    let _length = r.u32()?;
    let text = r.cstring()?;
    let tag = r.u8()?;

    // Achievement lines carry the achievement on the end. Nothing else does,
    // and reading it unconditionally would consume the next packet's bytes.
    if matches!(
        chat_type,
        ChatType::Achievement | ChatType::GuildAchievement
    ) {
        let _achievement = r.u32()?;
    }

    r.finish()?;
    Ok(ChatMessage {
        chat_type,
        language,
        sender,
        sender_name,
        target,
        channel,
        text,
        tag,
    })
}

/// Whether a guid names a player.
///
/// The high 16 bits are a type tag, and players are type zero -- which makes
/// "is a player" the same question as "are the top bits clear". Named because
/// the alternative reads as an arbitrary mask.
fn is_player_guid(guid: u64) -> bool {
    guid != 0 && (guid >> 48) == 0
}

/// `CMSG_MESSAGECHAT`'s body: say something.
///
/// `target` is the channel name for [`ChatType::Channel`] and the recipient's
/// name for [`ChatType::Whisper`]; every other kind ignores it.
///
/// Note the asymmetry with what comes back: the type goes out as a **32-bit**
/// value and arrives as a single byte. Same field, two widths, one direction
/// each -- exactly the kind of thing that is transcribed correctly once and
/// guessed at the second time.
pub fn message_chat(chat_type: ChatType, language: u32, target: &str, text: &str) -> Vec<u8> {
    let mut body = (chat_type.id() as u32).to_le_bytes().to_vec();
    body.extend_from_slice(&language.to_le_bytes());
    if matches!(chat_type, ChatType::Channel | ChatType::Whisper) {
        body.extend_from_slice(target.as_bytes());
        body.push(0);
    }
    body.extend_from_slice(text.as_bytes());
    body.push(0);
    body
}

/// The "universal" language, which every race reads.
///
/// **Do not send this.** It is what a GM command speaks, and an ordinary
/// account saying something in it is refused -- silently, with no reply and no
/// error, which is indistinguishable from a malformed packet. That cost a
/// round of debugging here: the first `CMSG_MESSAGECHAT` this client sent was
/// correct in every field except this one, produced no echo, and looked
/// exactly like a layout bug.
///
/// Use [`language_for_race`] instead.
pub const LANG_UNIVERSAL: u32 = 0;

/// The language a race speaks, which is what its own chat has to be sent in.
///
/// A character may only speak a language it knows, and every race knows
/// exactly one at the start. Getting this wrong does not garble the text --
/// that is what happens when someone *hears* a language they lack -- it stops
/// the message being accepted at all.
pub fn language_for_race(race: u8) -> u32 {
    match race {
        1 => 7,   // Human -> Common
        2 => 1,   // Orc -> Orcish
        3 => 6,   // Dwarf -> Dwarvish
        4 => 2,   // Night Elf -> Darnassian
        5 => 33,  // Undead -> Gutterspeak
        6 => 3,   // Tauren -> Taurahe
        7 => 13,  // Gnome -> Gnomish
        8 => 14,  // Troll -> Troll
        10 => 10, // Blood Elf -> Thalassian
        11 => 35, // Draenei -> Draenei
        // An unfamiliar race is likelier to be a misread field than a real
        // race, so fall back to the commonest language rather than to
        // Universal, which would be refused outright.
        _ => 7,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cstr(text: &str) -> Vec<u8> {
        let mut out = text.as_bytes().to_vec();
        out.push(0);
        out
    }

    /// A length-prefixed, NUL-terminated string, where the length counts the
    /// terminator -- the wire's own redundant encoding.
    fn counted(text: &str) -> Vec<u8> {
        let mut out = ((text.len() + 1) as u32).to_le_bytes().to_vec();
        out.extend(cstr(text));
        out
    }

    fn header(chat_type: ChatType, sender: u64) -> Vec<u8> {
        let mut out = vec![chat_type.id()];
        out.extend(0u32.to_le_bytes()); // language
        out.extend(sender.to_le_bytes());
        out.extend(0u32.to_le_bytes()); // reserved
        out
    }

    fn say(text: &str) -> Vec<u8> {
        let mut body = header(ChatType::Say, 0x35);
        body.extend(0u64.to_le_bytes()); // target
        body.extend(counted(text));
        body.push(0); // tag
        body
    }

    #[test]
    fn a_player_saying_something_parses() {
        let parsed = parse_message_chat(&say("hello")).unwrap();
        assert_eq!(parsed.chat_type, ChatType::Say);
        assert_eq!(parsed.sender, 0x35);
        assert_eq!(parsed.text, "hello");
        assert_eq!(
            parsed.sender_name, None,
            "a player's name comes from a query, never inline"
        );
    }

    /// **The exact bytes captured live** from a GM-flagged character
    /// (`Testwolf`, local realm, GM level 3) sending party chat -- see
    /// [`parse_gm_message_chat`]'s doc comment. `parse_message_chat` on this
    /// same body must fail rather than silently misread it: reading it as an
    /// ordinary player line (no inline name) lands the parser in the middle
    /// of the name-length field and text, and the assertion here is that
    /// *both* readings are checked, not just that the right one succeeds --
    /// the ordinary parser returning `Ok` with garbage would be invisible in
    /// a test that only tried `parse_gm_message_chat`.
    #[test]
    fn a_gm_flagged_players_line_carries_its_name_inline() {
        let body: Vec<u8> = [
            0x02, 0x07, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x54, 0x65, 0x73, 0x74, 0x77, 0x6f, 0x6c,
            0x66, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00,
            0x74, 0x65, 0x73, 0x74, 0x00, 0x00,
        ]
        .to_vec();

        assert!(
            parse_message_chat(&body).is_err(),
            "the ordinary parser must refuse a GM-shaped body rather than misread it"
        );

        let parsed = parse_gm_message_chat(&body).expect("gm body should parse whole");
        assert_eq!(parsed.chat_type, ChatType::Party);
        assert_eq!(parsed.language, 7);
        assert_eq!(parsed.sender, 1);
        assert_eq!(parsed.sender_name.as_deref(), Some("Testwolf"));
        assert_eq!(parsed.target, 0);
        assert_eq!(parsed.text, "test");
        assert_eq!(parsed.tag, 0);
    }

    /// A creature carries its name inline, which is a *different shape of
    /// packet* rather than an extra optional field. Parsing it as a player's
    /// say reads the name's length as the target guid.
    #[test]
    fn a_creature_carries_its_name_inline() {
        let mut body = header(ChatType::MonsterSay, 0xF130_0000_2B00_0BBA);
        body.extend(counted("Young Wolf"));
        body.extend(0u64.to_le_bytes()); // no target
        body.extend(counted("growls"));
        body.push(0);

        let parsed = parse_message_chat(&body).unwrap();
        assert_eq!(parsed.chat_type, ChatType::MonsterSay);
        assert_eq!(parsed.sender_name.as_deref(), Some("Young Wolf"));
        assert_eq!(parsed.text, "growls");
    }

    /// And a creature addressing another creature names *both* inline.
    #[test]
    fn a_creature_addressing_a_creature_names_both() {
        let target = 0xF130_0000_2B00_0BB5u64;
        let mut body = header(ChatType::MonsterWhisper, 0xF130_0000_2B00_0BBA);
        body.extend(counted("Young Wolf"));
        body.extend(target.to_le_bytes());
        body.extend(counted("Garrick Padfoot"));
        body.extend(counted("..."));
        body.push(0);

        let parsed = parse_message_chat(&body).unwrap();
        assert_eq!(parsed.sender_name.as_deref(), Some("Young Wolf"));
        assert_eq!(parsed.target, target);
        assert_eq!(parsed.text, "...");
    }

    /// A creature addressing a *player* does not, because the client can
    /// already name a player. Same opcode, same type, one field different.
    #[test]
    fn a_creature_addressing_a_player_names_only_itself() {
        let mut body = header(ChatType::MonsterWhisper, 0xF130_0000_2B00_0BBA);
        body.extend(counted("Young Wolf"));
        body.extend(0x35u64.to_le_bytes()); // a player guid
        body.extend(counted("hello"));
        body.push(0);

        let parsed = parse_message_chat(&body).unwrap();
        assert_eq!(parsed.text, "hello");
        assert_eq!(parsed.target, 0x35);
    }

    #[test]
    fn a_channel_line_carries_its_channel() {
        let mut body = header(ChatType::Channel, 0x35);
        body.extend(cstr("General"));
        body.extend(0u64.to_le_bytes());
        body.extend(counted("anyone selling?"));
        body.push(0);

        let parsed = parse_message_chat(&body).unwrap();
        assert_eq!(parsed.channel.as_deref(), Some("General"));
        assert_eq!(parsed.text, "anyone selling?");
    }

    /// The achievement id sits *after* the tag, where nothing else has
    /// anything. Reading it unconditionally eats the next packet's bytes;
    /// not reading it leaves four behind.
    #[test]
    fn an_achievement_line_carries_its_id() {
        let mut body = header(ChatType::Achievement, 0x35);
        body.extend(0u64.to_le_bytes());
        body.extend(counted("earned something"));
        body.push(0);
        body.extend(1234u32.to_le_bytes());

        assert!(parse_message_chat(&body).is_ok());

        // The same bytes read as an ordinary say leave the id behind, which is
        // exactly what `finish` exists to notice.
        let mut as_say = body.clone();
        as_say[0] = ChatType::Say.id();
        assert!(parse_message_chat(&as_say).is_err());
    }

    /// The cheap check that catches every one of the above being read with
    /// the wrong shape.
    #[test]
    fn trailing_and_truncated_bodies_are_errors() {
        let mut extra = say("hello");
        extra.push(0);
        assert!(parse_message_chat(&extra).is_err());

        let body = say("hello");
        for cut in 1..body.len() {
            assert!(
                parse_message_chat(&body[..cut]).is_err(),
                "{cut} bytes parsed as a whole message"
            );
        }
    }

    /// A type this client has no name for must keep its number rather than
    /// becoming a plausible wrong one.
    #[test]
    fn an_unknown_type_keeps_its_number() {
        assert_eq!(ChatType::from_id(0x7E), ChatType::Other(0x7E));
        assert_eq!(ChatType::Other(0x7E).id(), 0x7E);
    }

    /// Every named type must survive the round trip, or a message is sent as
    /// one kind and read back as another.
    #[test]
    fn chat_types_round_trip_through_their_ids() {
        for id in 0u8..=0x40 {
            let chat_type = ChatType::from_id(id);
            assert_eq!(chat_type.id(), id, "{chat_type:?} does not round-trip");
        }
    }

    /// Writing is riskier than reading: a wrong body is accepted as some other
    /// valid message rather than refused.
    #[test]
    fn what_we_send_carries_a_channel_only_when_there_is_one() {
        let plain = message_chat(ChatType::Say, LANG_UNIVERSAL, "", "hello");
        let with_channel = message_chat(ChatType::Channel, LANG_UNIVERSAL, "General", "hello");
        assert_eq!(
            with_channel.len() - plain.len(),
            "General".len() + 1,
            "the channel name and its terminator are the only difference"
        );

        let whisper = message_chat(ChatType::Whisper, LANG_UNIVERSAL, "Watcher", "hi");
        assert!(whisper.ends_with(b"Watcher\0hi\0"));
    }
}
