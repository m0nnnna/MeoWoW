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
    }
}
