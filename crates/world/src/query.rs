//! Asking the server what things are called.
//!
//! Nothing in an object update carries a name. A player's arrives only in
//! answer to `CMSG_NAME_QUERY`, and a creature's only in answer to
//! `CMSG_CREATURE_QUERY` -- so a client that never asks shows a world of
//! anonymous things, which is exactly where milestone 4.1 left it.
//!
//! Both responses are variable-length in ways worth stating, because this is
//! the shape of packet that has cost this project the most time: a field of
//! the wrong width parses perfectly and returns nonsense, and the only cheap
//! evidence is a cursor reporting leftovers. Everything here therefore parses
//! through [`Reader`] and ends in [`Reader::finish`].
//!
//! The creature response is parsed **in full** rather than stopping after the
//! name, even though the name is all this client currently wants. Stopping
//! early would throw away the one check that makes the rest trustworthy: if
//! the tail does not line up, some earlier field is the wrong width, and the
//! name that was read before it is no more reliable than the fields that were
//! skipped.

use crate::protocol::{Error, Reader};
use crate::update::read_packed_guid;

/// How many name slots a creature carries. Only the first is normally set;
/// the rest exist for locales that decline names by gender.
const CREATURE_NAMES: usize = 4;
/// Quest item ids in the creature response.
const CREATURE_QUEST_ITEMS: usize = 6;
/// Display ids in the creature response.
const CREATURE_MODELS: usize = 4;
/// Declined-name forms, when a locale supplies them.
const DECLINED_NAMES: usize = 5;

/// Name slots in the item response. Only the first is ever populated -- the
/// server writes three bare zero bytes for the rest, which are empty strings
/// rather than absent fields and must still be read.
const ITEM_NAMES: usize = 4;
/// Damage entries. **Two, not five** -- this array shrank in 3.1.0, and a
/// client reading the pre-3.1 count would run off the end of the packet
/// three entries later with no idea which field was wrong.
const ITEM_DAMAGES: usize = 2;
/// The six typed resistances that follow `armor`.
const ITEM_RESISTANCES: usize = 6;
/// Spell slots on an item.
const ITEM_SPELLS: usize = 5;
/// Fields written per spell slot: id, trigger, charges, cooldown, category,
/// category cooldown.
///
/// **Six, and always six.** The server's writer has three branches here --
/// a spell with per-item cooldowns, a spell falling back to the DBC's, and
/// an empty slot -- and every one of them writes six `u32`s. The branches
/// change the *values* and not the shape, which is the only reason this
/// block can be read as fixed-size.
const ITEM_SPELL_FIELDS: usize = 6;
/// Gem sockets.
const ITEM_SOCKETS: usize = 3;

/// `CMSG_NAME_QUERY`'s body: who is this guid?
pub fn name_query(guid: u64) -> Vec<u8> {
    guid.to_le_bytes().to_vec()
}

/// `CMSG_CREATURE_QUERY`'s body: what is this creature entry?
///
/// Carries the guid as well as the entry. The entry is what the answer is
/// keyed by -- every wolf of a kind shares one -- but the server wants to know
/// which instance asked.
pub fn creature_query(entry: u32, guid: u64) -> Vec<u8> {
    let mut body = entry.to_le_bytes().to_vec();
    body.extend_from_slice(&guid.to_le_bytes());
    body
}

/// What `SMSG_NAME_QUERY_RESPONSE` said about one player.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerName {
    pub guid: u64,
    /// `None` when the server does not know the guid. That is a real answer,
    /// not a failure: asking again will not help, so the caller needs to be
    /// able to stop asking.
    pub name: Option<String>,
    /// Empty for a player on this realm.
    pub realm: String,
    pub race: u8,
    pub gender: u8,
    pub class: u8,
}

/// Parses `SMSG_NAME_QUERY_RESPONSE`.
pub fn parse_name_query_response(body: &[u8]) -> Result<PlayerName, Error> {
    let mut r = Reader::new(body, "SMSG_NAME_QUERY_RESPONSE");
    let guid = read_packed_guid(&mut r)?;

    // A non-zero flag means "no such player", and the packet stops there.
    // Reading on regardless would take the next packet's bytes as a name.
    if r.u8()? != 0 {
        r.finish()?;
        return Ok(PlayerName {
            guid,
            name: None,
            realm: String::new(),
            race: 0,
            gender: 0,
            class: 0,
        });
    }

    let name = r.cstring()?;
    let realm = r.cstring()?;
    let race = r.u8()?;
    let gender = r.u8()?;
    let class = r.u8()?;

    // Declined names are a Russian-locale feature: five grammatical forms,
    // present only when the trailing flag says so. Skipped rather than kept --
    // but they must still be *read*, or `finish` reports them as leftovers and
    // a correct parse looks like a broken one.
    if r.u8()? != 0 {
        for _ in 0..DECLINED_NAMES {
            r.cstring()?;
        }
    }

    r.finish()?;
    Ok(PlayerName {
        guid,
        name: Some(name),
        realm,
        race,
        gender,
        class,
    })
}

/// `CMSG_ITEM_QUERY_SINGLE`'s body: what is this item entry?
///
/// Carries the guid after the entry, the same shape as
/// [`creature_query`]. **The server reads only the entry** -- its handler
/// takes one `u32` and stops -- so the guid is padding as far as any answer
/// is concerned. It is sent anyway because that is the body the real client
/// sends, an unread tail costs nothing, and a *short* body is the one
/// version that could fail against a server that reads what the client
/// really sends. Writing is the risky direction here, so this errs long.
///
/// Pass `0` for the guid when asking about an entry nothing in the world is
/// holding -- a loot row, a vendor's stock -- which is most of them.
pub fn item_query(entry: u32, guid: u64) -> Vec<u8> {
    let mut body = entry.to_le_bytes().to_vec();
    body.extend_from_slice(&guid.to_le_bytes());
    body
}

/// What `SMSG_ITEM_QUERY_SINGLE_RESPONSE` said about one item entry.
///
/// A deliberate subset of a packet with roughly ninety fields: what the
/// interface draws, plus the one field that lets the parse be checked
/// against something the server did not send. Everything else is read (it
/// has to be, or [`Reader::finish`] reports leftovers) and dropped.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemInfo {
    pub entry: u32,
    /// `None` when the server has no such entry -- flagged in the entry's
    /// top bit, like the creature response.
    pub name: Option<String>,
    /// The flavour line, empty for most items.
    pub description: String,
    /// **The check.** `Item.dbc` maps entry to display id independently, and
    /// the server never sends that table -- so the two agreeing is evidence
    /// the whole preceding parse is aligned, in the same way the
    /// entry-to-display-id pairing confirmed `SMSG_LOOT_RESPONSE`.
    pub display_id: u32,
    /// 0 poor .. 6 artifact. What colours the name.
    pub quality: u32,
    pub item_class: u32,
    pub sub_class: u32,
    pub inventory_type: u32,
    pub item_level: u32,
    pub required_level: u32,
    /// How many fit in one square. Negative in the table means "unlimited",
    /// which the wire sends as a negative `i32`.
    pub stackable: i32,
    /// Squares this item provides if it is a bag; 0 if it is not.
    pub container_slots: u32,
    pub buy_price: i32,
    pub sell_price: u32,
    pub max_durability: u32,
    /// The spells this item carries, in wire order.
    ///
    /// **Kept rather than skipped because using an item needs one.**
    /// `CMSG_USE_ITEM` carries the spell id the item is being used *for*, and
    /// there is nowhere else to get it: the item-to-spell mapping is server
    /// data, exactly like the name. A Hearthstone with its spell block
    /// discarded is an item a client can see and cannot use.
    pub spells: Vec<ItemSpell>,
}

/// One of an item's five spell slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemSpell {
    /// Zero for an empty slot, which most are.
    pub id: u32,
    /// What makes it fire. [`ITEM_SPELLTRIGGER_ON_USE`] is the one a click
    /// acts on; the others go off by themselves.
    pub trigger: u32,
}

/// `ITEM_SPELLTRIGGER_ON_USE`: the trigger a "use this" click acts on.
///
/// **Only this one is named**, and the rest are deliberately left as raw
/// numbers. This project does not transcribe enum members it has not
/// confirmed, and this value is confirmed the way the rest of the protocol
/// here is -- by effect, against a real consumable whose stack count drops
/// when it is used. Naming the neighbours from memory is the habit that
/// produced `CHAT_MSG_SAY = 0x00`.
pub const ITEM_SPELLTRIGGER_ON_USE: u32 = 0;

impl ItemInfo {
    /// The spell a click on this item should cast, if any.
    ///
    /// `None` for anything with no on-use spell -- a sword, a trade good, a
    /// bag -- which is most items, and is why a caller has to treat "not
    /// usable" as an ordinary answer rather than a failure.
    pub fn use_spell(&self) -> Option<u32> {
        self.spells
            .iter()
            .find(|spell| spell.id != 0 && spell.trigger == ITEM_SPELLTRIGGER_ON_USE)
            .map(|spell| spell.id)
    }
}

/// Parses `SMSG_ITEM_QUERY_SINGLE_RESPONSE`.
///
/// The whole packet, not just the name -- see the module comment. That
/// matters more here than anywhere else in this file, because the body runs
/// to some ninety fields and contains **one genuinely variable block**
/// (`stats_count`, then two words per stat). Everything after that block
/// shifts if the count is misread, and a name read *before* it would still
/// look perfect. `finish` is what notices.
///
/// The layout is AzerothCore's `HandleItemQuerySingleOpcode` -- the function
/// that *writes* the packet, not one of the many that read an
/// `ItemTemplate`. That distinction has already cost this project once (see
/// the movement-block note in `CLAUDE.md`), and it matters here: the packet
/// is **not** the struct. `BuyCount` sits in `ItemTemplate` between the
/// flags and the buy price and is never sent, and `_Spell` has seven fields
/// of which six travel.
pub fn parse_item_query_response(body: &[u8]) -> Result<ItemInfo, Error> {
    let mut r = Reader::new(body, "SMSG_ITEM_QUERY_SINGLE_RESPONSE");
    let entry = r.u32()?;

    // Same convention as the creature response: the top bit means "no such
    // entry" and the packet is just those four bytes.
    const NOT_FOUND: u32 = 0x8000_0000;
    if entry & NOT_FOUND != 0 {
        r.finish()?;
        return Ok(ItemInfo {
            entry: entry & !NOT_FOUND,
            name: None,
            description: String::new(),
            display_id: 0,
            quality: 0,
            item_class: 0,
            sub_class: 0,
            inventory_type: 0,
            item_level: 0,
            required_level: 0,
            stackable: 0,
            container_slots: 0,
            buy_price: 0,
            sell_price: 0,
            max_durability: 0,
            spells: Vec::new(),
        });
    }

    let item_class = r.u32()?;
    let sub_class = r.u32()?;
    let _sound_override_subclass = r.u32()? as i32;

    let mut names = Vec::with_capacity(ITEM_NAMES);
    for _ in 0..ITEM_NAMES {
        names.push(r.cstring()?);
    }

    let display_id = r.u32()?;
    let quality = r.u32()?;
    let _flags = r.u32()?;
    let _flags2 = r.u32()?;
    let buy_price = r.u32()? as i32;
    let sell_price = r.u32()?;
    let inventory_type = r.u32()?;
    let _allowable_class = r.u32()? as i32;
    let _allowable_race = r.u32()? as i32;
    let item_level = r.u32()?;
    let required_level = r.u32()?;
    let _required_skill = r.u32()?;
    let _required_skill_rank = r.u32()?;
    let _required_spell = r.u32()?;
    let _required_honor_rank = r.u32()?;
    let _required_city_rank = r.u32()?;
    let _required_reputation_faction = r.u32()?;
    let _required_reputation_rank = r.u32()?;
    let _max_count = r.u32()? as i32;
    let stackable = r.u32()? as i32;
    let container_slots = r.u32()?;

    // The one variable-length block. Everything below moves if this is
    // wrong, which is what makes `finish` the real test of this parser.
    let stats_count = r.u32()?;
    for _ in 0..stats_count {
        let _stat_type = r.u32()?;
        let _stat_value = r.u32()? as i32;
    }

    let _scaling_stat_distribution = r.u32()?;
    let _scaling_stat_value = r.u32()?;
    for _ in 0..ITEM_DAMAGES {
        let _min = r.f32()?;
        let _max = r.f32()?;
        let _damage_type = r.u32()?;
    }

    let _armor = r.u32()?;
    for _ in 0..ITEM_RESISTANCES {
        r.u32()?;
    }

    let _delay = r.u32()?;
    let _ammo_type = r.u32()?;
    let _ranged_mod_range = r.f32()?;
    let mut spells = Vec::with_capacity(ITEM_SPELLS);
    for _ in 0..ITEM_SPELLS {
        let id = r.u32()?;
        let trigger = r.u32()?;
        // Charges, cooldown, category, category cooldown -- read to keep the
        // cursor aligned, not kept. `ITEM_SPELL_FIELDS` is what says six.
        for _ in 2..ITEM_SPELL_FIELDS {
            r.u32()?;
        }
        spells.push(ItemSpell { id, trigger });
    }

    let _bonding = r.u32()?;
    let description = r.cstring()?;
    let _page_text = r.u32()?;
    let _language_id = r.u32()?;
    let _page_material = r.u32()?;
    let _start_quest = r.u32()?;
    let _lock_id = r.u32()?;
    let _material = r.u32()? as i32;
    let _sheath = r.u32()?;
    let _random_property = r.u32()? as i32;
    let _random_suffix = r.u32()? as i32;
    let _block = r.u32()?;
    let _item_set = r.u32()?;
    let max_durability = r.u32()?;
    let _area = r.u32()?;
    let _map = r.u32()?;
    let _bag_family = r.u32()?;
    let _totem_category = r.u32()?;
    for _ in 0..ITEM_SOCKETS {
        let _color = r.u32()?;
        let _content = r.u32()?;
    }
    let _socket_bonus = r.u32()?;
    let _gem_properties = r.u32()?;
    let _required_disenchant_skill = r.u32()? as i32;
    let _armor_damage_modifier = r.f32()?;
    let _duration = r.u32()?;
    let _item_limit_category = r.u32()? as i32;
    let _holiday_id = r.u32()?;

    r.finish()?;
    Ok(ItemInfo {
        entry,
        name: names.into_iter().next().filter(|name| !name.is_empty()),
        description,
        display_id,
        quality,
        item_class,
        sub_class,
        inventory_type,
        item_level,
        required_level,
        stackable,
        container_slots,
        buy_price,
        sell_price,
        max_durability,
        spells,
    })
}

/// What `SMSG_CREATURE_QUERY_RESPONSE` said about one creature entry.
#[derive(Debug, Clone, PartialEq)]
pub struct CreatureInfo {
    pub entry: u32,
    /// `None` when the server has no such entry.
    pub name: Option<String>,
    /// The `<Gryphon Master>` line under a name, when there is one.
    pub sub_name: String,
    pub type_flags: u32,
    pub creature_type: u32,
    pub family: u32,
    pub rank: u32,
}

/// Parses `SMSG_CREATURE_QUERY_RESPONSE`.
///
/// The whole packet, not just the name -- see the module comment for why.
pub fn parse_creature_query_response(body: &[u8]) -> Result<CreatureInfo, Error> {
    let mut r = Reader::new(body, "SMSG_CREATURE_QUERY_RESPONSE");
    let entry = r.u32()?;

    // The top bit means "no such entry", and the packet is just the entry.
    // This is a different convention from the name query's separate flag byte,
    // which is the kind of inconsistency worth naming rather than smoothing
    // over: reading this one as a plain entry yields a creature numbered two
    // billion whose name is whatever came next.
    const NOT_FOUND: u32 = 0x8000_0000;
    if entry & NOT_FOUND != 0 {
        r.finish()?;
        return Ok(CreatureInfo {
            entry: entry & !NOT_FOUND,
            name: None,
            sub_name: String::new(),
            type_flags: 0,
            creature_type: 0,
            family: 0,
            rank: 0,
        });
    }

    let mut names = Vec::with_capacity(CREATURE_NAMES);
    for _ in 0..CREATURE_NAMES {
        names.push(r.cstring()?);
    }
    let sub_name = r.cstring()?;
    // The "right-click to talk to me" hint, e.g. `Directions`.
    let _icon = r.cstring()?;

    let type_flags = r.u32()?;
    let creature_type = r.u32()?;
    let family = r.u32()?;
    let rank = r.u32()?;
    let _kill_credit = [r.u32()?, r.u32()?];
    for _ in 0..CREATURE_MODELS {
        r.u32()?;
    }
    let _health_modifier = r.f32()?;
    let _power_modifier = r.f32()?;
    let _racial_leader = r.u8()?;
    for _ in 0..CREATURE_QUEST_ITEMS {
        r.u32()?;
    }
    let _movement_id = r.u32()?;

    r.finish()?;
    Ok(CreatureInfo {
        entry,
        name: names.into_iter().next().filter(|name| !name.is_empty()),
        sub_name,
        type_flags,
        creature_type,
        family,
        rank,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Built with the real writer, so the packed encoding these tests feed in
    /// is the one the wire actually uses rather than this test's idea of it.
    fn packed(guid: u64) -> Vec<u8> {
        let mut out = Vec::new();
        crate::update::write_packed_guid(guid, &mut out);
        out
    }

    fn cstr(text: &str) -> Vec<u8> {
        let mut out = text.as_bytes().to_vec();
        out.push(0);
        out
    }

    fn name_response(name: &str, declined: bool) -> Vec<u8> {
        let mut body = packed(0x35);
        body.push(0); // found
        body.extend(cstr(name));
        body.extend(cstr("")); // same realm
        body.extend([1, 0, 1]); // race, gender, class
        body.push(u8::from(declined));
        if declined {
            for form in 0..DECLINED_NAMES {
                body.extend(cstr(&format!("{name}{form}")));
            }
        }
        body
    }

    #[test]
    fn a_player_name_parses() {
        let parsed = parse_name_query_response(&name_response("Watcher", false)).unwrap();
        assert_eq!(parsed.guid, 0x35);
        assert_eq!(parsed.name.as_deref(), Some("Watcher"));
        assert_eq!(parsed.realm, "");
        assert_eq!((parsed.race, parsed.gender, parsed.class), (1, 0, 1));
    }

    /// Declined names are absent for every locale this project tests against,
    /// which is exactly why they are worth a test: the flag that introduces
    /// them is the last byte of the common case, so reading it wrongly is
    /// invisible until a Russian client connects.
    #[test]
    fn declined_names_are_consumed_rather_than_left_over() {
        let parsed = parse_name_query_response(&name_response("Watcher", true)).unwrap();
        assert_eq!(parsed.name.as_deref(), Some("Watcher"));
    }

    /// An unknown guid is an answer, not an error: the caller has to be able
    /// to stop asking, and a parse failure would keep it retrying forever.
    #[test]
    fn an_unknown_guid_is_a_name_of_none() {
        let mut body = packed(0x99);
        body.push(1);
        let parsed = parse_name_query_response(&body).unwrap();
        assert_eq!(parsed.guid, 0x99);
        assert_eq!(parsed.name, None);
    }

    /// The check that catches a field of the wrong width.
    #[test]
    fn trailing_bytes_are_an_error() {
        let mut body = name_response("Watcher", false);
        body.push(0);
        assert!(parse_name_query_response(&body).is_err());
    }

    #[test]
    fn a_truncated_response_is_an_error() {
        let body = name_response("Watcher", false);
        for cut in 1..body.len() {
            assert!(
                parse_name_query_response(&body[..cut]).is_err(),
                "{cut} bytes parsed as a whole response"
            );
        }
    }

    fn creature_response(entry: u32, name: &str) -> Vec<u8> {
        let mut body = entry.to_le_bytes().to_vec();
        body.extend(cstr(name));
        for _ in 1..CREATURE_NAMES {
            body.extend(cstr(""));
        }
        body.extend(cstr("Wolf Trainer")); // sub name
        body.extend(cstr("")); // icon
        for value in [0x1000u32, 1, 1, 0, 0, 0] {
            body.extend(value.to_le_bytes());
        }
        for _ in 0..CREATURE_MODELS {
            body.extend(297u32.to_le_bytes());
        }
        body.extend(1.0f32.to_le_bytes());
        body.extend(1.0f32.to_le_bytes());
        body.push(0); // racial leader
        for _ in 0..CREATURE_QUEST_ITEMS {
            body.extend(0u32.to_le_bytes());
        }
        body.extend(0u32.to_le_bytes()); // movement id
        body
    }

    #[test]
    fn a_creature_name_parses() {
        let parsed = parse_creature_query_response(&creature_response(299, "Young Wolf")).unwrap();
        assert_eq!(parsed.entry, 299);
        assert_eq!(parsed.name.as_deref(), Some("Young Wolf"));
        assert_eq!(parsed.sub_name, "Wolf Trainer");
        assert_eq!(parsed.creature_type, 1);
    }

    /// The two responses signal "not found" in completely different ways --
    /// a separate flag byte for players, the entry's top bit for creatures.
    /// Reading the creature form as a plain entry gives a creature numbered
    /// two billion whose name is whatever bytes came next.
    #[test]
    fn an_unknown_entry_is_flagged_in_the_entry_itself() {
        let body = (299u32 | 0x8000_0000).to_le_bytes().to_vec();
        let parsed = parse_creature_query_response(&body).unwrap();
        assert_eq!(parsed.entry, 299);
        assert_eq!(parsed.name, None);
    }

    /// Parsing the whole packet is what makes the name trustworthy: if the
    /// tail does not line up, an earlier field was the wrong width and the
    /// name is no better than the bytes that were skipped.
    #[test]
    fn a_creature_response_must_consume_its_tail() {
        let mut body = creature_response(299, "Young Wolf");
        body.extend(0u32.to_le_bytes());
        assert!(parse_creature_query_response(&body).is_err());

        let short = creature_response(299, "Young Wolf");
        assert!(parse_creature_query_response(&short[..short.len() - 2]).is_err());
    }

    /// A creature with an empty name is reported as having none, rather than
    /// as being called "".
    #[test]
    fn an_empty_name_is_no_name() {
        let parsed = parse_creature_query_response(&creature_response(1, "")).unwrap();
        assert_eq!(parsed.name, None);
    }

    /// The queries are what this client writes, and a wrong write is accepted
    /// as some other valid message rather than refused -- so pin their bodies.
    ///
    /// Note the asymmetry worth not smoothing over: a name query is a plain
    /// 64-bit guid, *not* the packed form the response answers with.
    #[test]
    fn the_queries_carry_what_the_server_keys_on() {
        assert_eq!(
            name_query(0x0F13_0000_1234_5678),
            0x0F13_0000_1234_5678u64.to_le_bytes()
        );

        let body = creature_query(299, 0xABCD);
        assert_eq!(body.len(), 12);
        assert_eq!(&body[..4], &299u32.to_le_bytes());
        assert_eq!(&body[4..], &0xABCDu64.to_le_bytes());

        let body = item_query(2224, 0);
        assert_eq!(body.len(), 12);
        assert_eq!(&body[..4], &2224u32.to_le_bytes());
    }

    /// Builds an item response with a chosen number of stat entries.
    ///
    /// **The stat count is a parameter on purpose.** It is the packet's only
    /// variable-length block, and a fixture with one fixed count cannot tell
    /// a parser that reads the count from one that hardcodes it -- both pass.
    /// Every field below the block is filled with a distinct marker so a
    /// misread count shows up as a *wrong value* rather than only as a
    /// length error.
    fn item_response(entry: u32, name: &str, stats: u32) -> Vec<u8> {
        let mut body = entry.to_le_bytes().to_vec();
        body.extend(1u32.to_le_bytes()); // class: weapon
        body.extend(7u32.to_le_bytes()); // subclass
        body.extend(0u32.to_le_bytes()); // sound override subclass
        body.extend(cstr(name));
        for _ in 1..ITEM_NAMES {
            body.extend(cstr(""));
        }
        body.extend(12345u32.to_le_bytes()); // display id
        body.extend(2u32.to_le_bytes()); // quality: uncommon
        body.extend(0u32.to_le_bytes()); // flags
        body.extend(0u32.to_le_bytes()); // flags2
        body.extend(25u32.to_le_bytes()); // buy price
        body.extend(5u32.to_le_bytes()); // sell price
        body.extend(21u32.to_le_bytes()); // inventory type
        body.extend((-1i32).to_le_bytes()); // allowable class
        body.extend((-1i32).to_le_bytes()); // allowable race
        body.extend(3u32.to_le_bytes()); // item level
        body.extend(1u32.to_le_bytes()); // required level
        for _ in 0..7 {
            body.extend(0u32.to_le_bytes()); // skill..reputation rank
        }
        body.extend(0u32.to_le_bytes()); // max count
        body.extend(20i32.to_le_bytes()); // stackable
        body.extend(0u32.to_le_bytes()); // container slots

        body.extend(stats.to_le_bytes());
        for stat in 0..stats {
            body.extend((stat + 1).to_le_bytes()); // stat type
            body.extend(((stat as i32) * 2).to_le_bytes()); // stat value
        }

        body.extend(0u32.to_le_bytes()); // scaling stat distribution
        body.extend(0u32.to_le_bytes()); // scaling stat value
        for _ in 0..ITEM_DAMAGES {
            body.extend(1.5f32.to_le_bytes());
            body.extend(3.5f32.to_le_bytes());
            body.extend(0u32.to_le_bytes());
        }
        body.extend(0u32.to_le_bytes()); // armor
        for _ in 0..ITEM_RESISTANCES {
            body.extend(0u32.to_le_bytes());
        }
        body.extend(1900u32.to_le_bytes()); // delay
        body.extend(0u32.to_le_bytes()); // ammo type
        body.extend(0.0f32.to_le_bytes()); // ranged mod range
        for _ in 0..ITEM_SPELLS {
            for _ in 0..ITEM_SPELL_FIELDS {
                body.extend(0u32.to_le_bytes());
            }
        }
        body.extend(1u32.to_le_bytes()); // bonding
        body.extend(cstr("A well-used blade."));
        for _ in 0..5 {
            body.extend(0u32.to_le_bytes()); // page text..lock id
        }
        body.extend(1u32.to_le_bytes()); // material
        body.extend(1u32.to_le_bytes()); // sheath
        body.extend(0u32.to_le_bytes()); // random property
        body.extend(0u32.to_le_bytes()); // random suffix
        body.extend(0u32.to_le_bytes()); // block
        body.extend(0u32.to_le_bytes()); // item set
        body.extend(55u32.to_le_bytes()); // max durability
        body.extend(0u32.to_le_bytes()); // area
        body.extend(0u32.to_le_bytes()); // map
        body.extend(0u32.to_le_bytes()); // bag family
        body.extend(0u32.to_le_bytes()); // totem category
        for _ in 0..ITEM_SOCKETS {
            body.extend(0u32.to_le_bytes()); // colour
            body.extend(0u32.to_le_bytes()); // content
        }
        body.extend(0u32.to_le_bytes()); // socket bonus
        body.extend(0u32.to_le_bytes()); // gem properties
        body.extend((-1i32).to_le_bytes()); // required disenchant skill
        body.extend(0.0f32.to_le_bytes()); // armor damage modifier
        body.extend(0u32.to_le_bytes()); // duration
        body.extend((-1i32).to_le_bytes()); // item limit category
        body.extend(0u32.to_le_bytes()); // holiday id
        body
    }

    #[test]
    fn an_item_parses() {
        let parsed = parse_item_query_response(&item_response(2224, "Small Dagger", 2)).unwrap();
        assert_eq!(parsed.entry, 2224);
        assert_eq!(parsed.name.as_deref(), Some("Small Dagger"));
        assert_eq!(parsed.description, "A well-used blade.");
        assert_eq!(parsed.display_id, 12345);
        assert_eq!(parsed.quality, 2);
        assert_eq!(parsed.stackable, 20);
        assert_eq!(parsed.max_durability, 55);
        assert_eq!(parsed.sell_price, 5);
    }

    /// **The test that separates a real parse from a lucky one.** The stats
    /// block is the packet's only variable-length section, and a parser that
    /// hardcoded its size would read one count correctly and shift every
    /// field below the block for the other. Asserting `max_durability` --
    /// which sits far past the block -- pins that the whole tail realigned,
    /// not just that the packet happened to end where expected.
    ///
    /// Same shape as the gossip work, where three NPCs were greeted
    /// specifically so the two variable counts would disagree.
    #[test]
    fn the_stat_block_shifts_everything_below_it() {
        for stats in [0u32, 1, 5, 10] {
            let parsed =
                parse_item_query_response(&item_response(2224, "Small Dagger", stats)).unwrap();
            assert_eq!(
                parsed.max_durability, 55,
                "{stats} stats: the tail did not realign"
            );
            assert_eq!(parsed.display_id, 12345, "{stats} stats");
            assert_eq!(parsed.description, "A well-used blade.", "{stats} stats");
        }
    }

    /// An entry the server has never heard of: four bytes with the top bit
    /// set, and nothing else. Reading it as a plain entry gives an item
    /// numbered two billion and takes the next packet's bytes as its name.
    #[test]
    fn an_unknown_item_entry_is_flagged_in_the_entry() {
        let body = (2224u32 | 0x8000_0000).to_le_bytes().to_vec();
        let parsed = parse_item_query_response(&body).unwrap();
        assert_eq!(parsed.entry, 2224);
        assert_eq!(parsed.name, None);
        assert_eq!(parsed.display_id, 0);
    }

    #[test]
    fn an_item_with_trailing_bytes_is_an_error() {
        let mut body = item_response(2224, "Small Dagger", 2);
        body.push(0);
        assert!(parse_item_query_response(&body).is_err());
    }

    /// Every truncation is an error, including ones that stop on a field
    /// boundary -- which is the case a length check alone would wave through.
    #[test]
    fn a_truncated_item_response_is_an_error() {
        let body = item_response(2224, "Small Dagger", 2);
        for cut in 1..body.len() {
            assert!(
                parse_item_query_response(&body[..cut]).is_err(),
                "{cut} bytes parsed as a whole response"
            );
        }
    }
}
