//! What an NPC says when you greet it.
//!
//! `SMSG_GOSSIP_MESSAGE` answers [`ClientOpcode::GossipHello`](crate::ClientOpcode),
//! and this parses it. It is a menu -- a block of text, a list of things the
//! player can click, and a list of quests the NPC is offering.
//!
//! **Nothing here was transcribed.** The layout was read out of the bytes of
//! three live replies and then checked, field by field, against the server's
//! own world database -- which is a source the client is never sent, and so is
//! the same class of evidence as `Item.dbc` pairing a loot entry with its
//! display id. This is the whole reason gossip was the first NPC request
//! attempted: it is *answered*, where the equip write had to be confirmed by
//! watching a field move.
//!
//! **Three replies, three different shapes, and that is the point.** A layout
//! agreeing with one packet is nearly free -- most of a gossip menu is zeroes
//! and any plausible reading survives them. What separates a real layout from a
//! plausible one is a set of samples where the *counts differ*:
//!
//! | greeted | bytes | shape | checked against |
//! |---|---|---|---|
//! | Innkeeper Farley (295) | 136 | menu 1291, 3 options, 0 quests | `gossip_menu_option`'s three rows, icons and text |
//! | Marshal McBride (197) | 24 | menu 4048, 0 options, 0 quests | `creature_template.gossip_menu_id` |
//! | Deputy Willem (823) | 57 | menu 57020, 0 options, 1 quest | `quest_template`'s title, level and flags |
//!
//! Every one of those consumed its body exactly, and the three counts are what
//! prove the two variable-length blocks are where this says they are: a reading
//! that had the quest block in the wrong place would still parse Farley's
//! packet, which has no quests in it.
//!
//! **The strongest single check is the one the packet did not control.**
//! Farley's menu in the database has *four* options and only three arrived --
//! the missing one is a Hallowe'en seasonal line the server filters out. The
//! three that came carried indices 1, 2 and 3, with 0 absent. So
//! [`GossipOption::index`] is the server's own option id and **not** a position
//! in the list, exactly like a loot slot: a client that renumbered its rows
//! would send the wrong choice, and would do it only for NPCs whose menus are
//! conditionally filtered -- which is a bug that hides until it matters.

use crate::protocol::{Error, Reader};

/// One clickable line in a gossip menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GossipOption {
    /// **The server's own option id, and the handle a reply has to use.** Not
    /// a position in [`Gossip::options`]: the server filters options the
    /// player does not qualify for and does *not* close the numbering up. See
    /// the module comment -- Farley's menu arrives as 1, 2, 3 with 0 missing.
    ///
    /// Same rule, and the same trap, as [`crate::LootItem::slot`].
    pub index: u32,
    /// Which icon the original client draws beside the line -- a speech
    /// bubble, a coin, a trainer's book.
    ///
    /// **Deliberately not interpreted.** Observed as 0, 1 and 5, and the
    /// database agrees those are the values it holds, but what each *draws* is
    /// a question about the original interface rather than about this packet.
    /// Naming them from memory is the mistake `describe_cast_failure` exists
    /// to refuse.
    pub icon: u8,
    /// Whether choosing this line opens a box for the player to type into --
    /// the shape a bank or guild-name prompt takes.
    ///
    /// Only `0` has been observed. Kept as the raw byte for the same reason as
    /// [`GossipOption::icon`], and named after the database column
    /// (`gossip_menu_option.BoxCoded`) rather than after a guess at its effect.
    pub coded: u8,
    /// Copper this option costs to choose. Zero on everything observed.
    pub money: u32,
    /// The line as the player reads it: "I want to browse your goods."
    pub message: String,
    /// The confirmation text shown in the box, when there is one. Empty on
    /// everything observed, which is consistent with [`GossipOption::coded`]
    /// being zero throughout.
    pub box_message: String,
}

/// One quest an NPC is offering, as the *menu* lists it.
///
/// This is the one-line summary that goes in the list, not the quest itself:
/// there is no objective text, no reward and no description here. Those come
/// from a separate request, which is why accepting a quest cannot be done from
/// this packet alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GossipQuest {
    /// Row in the server's `quest_template`. **Not** a client-side table: no
    /// DBC ships quest text, which is the fact that makes this whole milestone
    /// a protocol one rather than a format one.
    pub quest_id: u32,
    /// Whether the quest is available, already taken, or ready to hand in.
    /// Observed only as `2`. Raw, and for the usual reason.
    pub icon: u32,
    /// The quest's level, which is a different thing from the level required
    /// to take it. **Signed**: a few quests in the game carry -1 to mean "the
    /// player's own level", and reading it unsigned would report such a quest
    /// as level 4,294,967,295.
    pub level: i32,
    /// `quest_template.Flags`. Confirmed rather than decoded: quest 783
    /// arrived carrying 524296, which is exactly what the database holds for
    /// it. Which bit means what is not asked here.
    pub flags: u32,
    /// Whether the quest can be taken again. Only `0` observed.
    pub repeatable: u8,
    /// The title as it appears in the quest log: "A Threat Within".
    pub title: String,
}

/// A whole gossip menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gossip {
    /// Who is talking. Sent **unpacked**, as it was in the request.
    pub npc: u64,
    /// Row in the server's `gossip_menu`, echoed back so a reply can name the
    /// menu it is answering.
    ///
    /// This is also the field that identified the header, and it did so on
    /// three NPCs at once: it equals `creature_template.gossip_menu_id` for
    /// each of them -- 1291, 4048 and 57020 -- which is a number the client is
    /// never given and could not have guessed three times.
    pub menu_id: u32,
    /// Which block of greeting text to show, in the server's `npc_text`.
    ///
    /// **The client cannot resolve this and that is not a bug here.** The text
    /// lives in the world database like everything else in this packet; there
    /// is a separate query for it. What is confirmed is the *number*: menu
    /// 1291's text id is 820 in the database and 820 on the wire.
    pub text_id: u32,
    pub options: Vec<GossipOption>,
    /// The quests this NPC will offer, filtered by the server to the ones this
    /// character may actually take.
    ///
    /// **An empty list is a statement, not a gap.** Greeting Marshal McBride
    /// with a brand-new character returned zero quests, which looked at first
    /// like the quest block being in the wrong place -- and was correct: every
    /// quest he starts is gated behind `A Threat Within`, which someone else
    /// gives out. The population could not exhibit the thing being looked for,
    /// which is this project's most frequently repaid lesson.
    pub quests: Vec<GossipQuest>,
}

impl Gossip {
    /// Whether there is anything at all to show.
    ///
    /// A menu with no options and no quests is a real reply -- McBride's was
    /// exactly that -- and it means the NPC has only its greeting text.
    pub fn is_empty(&self) -> bool {
        self.options.is_empty() && self.quests.is_empty()
    }
}

/// Parses `SMSG_GOSSIP_MESSAGE`.
///
/// Read through a cursor that must end exactly at the end of the body. Both
/// halves matter: running out of input and having input left over are each an
/// error, and this project has four separate world-protocol bugs on record
/// that were invisible field by field and obvious the moment a cursor reported
/// leftovers.
///
/// It matters more than usual here. This packet contains two variable-length
/// arrays with strings in them, so a single miscounted field does not shift a
/// value -- it desynchronises everything after it, and a string read from the
/// wrong offset comes back as plausible-looking garbage rather than as a
/// failure.
pub fn parse_gossip_message(body: &[u8]) -> Result<Gossip, Error> {
    let mut r = Reader::new(body, "SMSG_GOSSIP_MESSAGE");

    let npc = r.u64()?;
    let menu_id = r.u32()?;
    let text_id = r.u32()?;

    let option_count = r.u32()?;
    let mut options = Vec::new();
    for _ in 0..option_count {
        options.push(GossipOption {
            index: r.u32()?,
            icon: r.u8()?,
            coded: r.u8()?,
            money: r.u32()?,
            message: r.cstring()?,
            box_message: r.cstring()?,
        });
    }

    let quest_count = r.u32()?;
    let mut quests = Vec::new();
    for _ in 0..quest_count {
        quests.push(GossipQuest {
            quest_id: r.u32()?,
            icon: r.u32()?,
            // Signed on purpose -- see `GossipQuest::level`.
            level: r.u32()? as i32,
            flags: r.u32()?,
            repeatable: r.u8()?,
            title: r.cstring()?,
        });
    }

    r.finish()?;
    Ok(Gossip {
        npc,
        menu_id,
        text_id,
        options,
        quests,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Innkeeper Farley, spawned with `.npc add 295` and greeted. Three
    /// options, no quests.
    ///
    /// Kept as the known-good constant this project's conventions ask for, and
    /// it is the sample that exercises the option block at all.
    const FARLEY: [u8; 136] = [
        0x8e, 0xcf, 0x00, 0x27, 0x01, 0x00, 0x30, 0xf1, // guid
        0x0b, 0x05, 0x00, 0x00, // menu 1291
        0x34, 0x03, 0x00, 0x00, // text 820
        0x03, 0x00, 0x00, 0x00, // three options
        0x01, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, // index 1, icon 5
        0x4d, 0x61, 0x6b, 0x65, 0x20, 0x74, 0x68, 0x69, 0x73, 0x20, 0x69, 0x6e, 0x6e, 0x20, 0x79,
        0x6f, 0x75, 0x72, 0x20, 0x68, 0x6f, 0x6d, 0x65, 0x2e, 0x00, // "Make this inn your home."
        0x00, // empty box message
        0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // index 2, icon 0
        0x57, 0x68, 0x61, 0x74, 0x20, 0x63, 0x61, 0x6e, 0x20, 0x49, 0x20, 0x64, 0x6f, 0x20, 0x61,
        0x74, 0x20, 0x61, 0x6e, 0x20, 0x69, 0x6e, 0x6e, 0x3f, 0x00, // "What can I do at an inn?"
        0x00, //
        0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, // index 3, icon 1
        0x49, 0x20, 0x77, 0x61, 0x6e, 0x74, 0x20, 0x74, 0x6f, 0x20, 0x62, 0x72, 0x6f, 0x77, 0x73,
        0x65, 0x20, 0x79, 0x6f, 0x75, 0x72, 0x20, 0x67, 0x6f, 0x6f, 0x64, 0x73, 0x2e,
        0x00, // "I want to browse your goods."
        0x00, //
        0x00, 0x00, 0x00, 0x00, // no quests
    ];

    /// Deputy Willem, greeted by a character who had never taken a quest. No
    /// options, one quest -- the complement of Farley's packet, and the only
    /// sample that exercises the quest block.
    const WILLEM: [u8; 57] = [
        0xc0, 0xd3, 0x00, 0x37, 0x03, 0x00, 0x30, 0xf1, // guid
        0xbc, 0xde, 0x00, 0x00, // menu 57020
        0x60, 0xc3, 0x00, 0x00, // text 50016
        0x00, 0x00, 0x00, 0x00, // no options
        0x01, 0x00, 0x00, 0x00, // one quest
        0x0f, 0x03, 0x00, 0x00, // quest 783
        0x02, 0x00, 0x00, 0x00, // icon 2
        0x01, 0x00, 0x00, 0x00, // level 1
        0x08, 0x00, 0x08, 0x00, // flags 0x00080008
        0x00, // not repeatable
        0x41, 0x20, 0x54, 0x68, 0x72, 0x65, 0x61, 0x74, 0x20, 0x57, 0x69, 0x74, 0x68, 0x69, 0x6e,
        0x00, // "A Threat Within"
    ];

    /// Marshal McBride: a menu with nothing in it at all, which is a real
    /// reply and the shortest this packet gets.
    const MCBRIDE: [u8; 24] = [
        0x19, 0xd3, 0x00, 0xc5, 0x00, 0x00, 0x30, 0xf1, // guid
        0xd0, 0x0f, 0x00, 0x00, // menu 4048
        0x4a, 0x13, 0x00, 0x00, // text 4938
        0x00, 0x00, 0x00, 0x00, // no options
        0x00, 0x00, 0x00, 0x00, // no quests
    ];

    #[test]
    fn a_captured_menu_parses_exactly() {
        let gossip = parse_gossip_message(&FARLEY).unwrap();
        assert_eq!(gossip.npc, 0xf130_0001_2700_cf8e);
        assert_eq!(gossip.menu_id, 1291);
        assert_eq!(gossip.text_id, 820);
        assert_eq!(gossip.options.len(), 3);
        assert!(gossip.quests.is_empty());
        assert!(!gossip.is_empty());
    }

    /// **The check that makes the rest trustworthy**, and the one the packet
    /// could not have arranged.
    ///
    /// These three lines, their icons and their option ids all live in the
    /// server's `gossip_menu_option` table, which no client is ever sent. A
    /// reading shifted by even one byte turns the messages into fragments of
    /// each other while still parsing, so agreement with an outside table is a
    /// far stronger statement than the parse merely succeeding.
    #[test]
    fn the_options_agree_with_the_servers_own_menu_table() {
        let gossip = parse_gossip_message(&FARLEY).unwrap();
        let seen: Vec<(u32, u8, &str)> = gossip
            .options
            .iter()
            .map(|option| (option.index, option.icon, option.message.as_str()))
            .collect();
        assert_eq!(
            seen,
            vec![
                (1, 5, "Make this inn your home."),
                (2, 0, "What can I do at an inn?"),
                (3, 1, "I want to browse your goods."),
            ]
        );
    }

    /// **An option index is the server's id, not a row number.**
    ///
    /// Menu 1291 has four rows in the database and three arrived: option 0,
    /// "Trick or Treat!", is seasonal and the server filtered it out. The
    /// numbering did *not* close up, so a client that used a row position
    /// would ask for the wrong thing -- and only when talking to an NPC whose
    /// menu happens to be conditional, which is exactly the kind of bug that
    /// hides until it is expensive.
    ///
    /// Pinned as its own test because it is a claim about the protocol rather
    /// than about this parse, and it is the same rule as [`crate::LootItem::slot`].
    #[test]
    fn option_indices_are_server_ids_with_a_filtered_row_missing() {
        let gossip = parse_gossip_message(&FARLEY).unwrap();
        let indices: Vec<u32> = gossip.options.iter().map(|option| option.index).collect();
        assert_eq!(indices, vec![1, 2, 3], "0 was filtered and left a hole");
        assert!(
            !indices.contains(&0),
            "if 0 is ever present here the filtering claim needs re-checking"
        );
    }

    /// The quest block, checked the same way: title, level and flags all come
    /// from `quest_template`, which the client is never sent.
    ///
    /// The flags value is the load-bearing half. A title could conceivably be
    /// matched by luck at a nearby offset; 524296 could not.
    #[test]
    fn the_quest_agrees_with_the_servers_own_quest_table() {
        let gossip = parse_gossip_message(&WILLEM).unwrap();
        assert_eq!(gossip.menu_id, 57020);
        assert!(gossip.options.is_empty());
        assert_eq!(gossip.quests.len(), 1);

        let quest = &gossip.quests[0];
        assert_eq!(quest.quest_id, 783);
        assert_eq!(quest.title, "A Threat Within");
        assert_eq!(quest.level, 1);
        assert_eq!(quest.flags, 524_296, "quest_template.Flags for 783");
        assert_eq!(quest.repeatable, 0);
    }

    /// A menu with neither options nor quests is a real reply and must parse.
    ///
    /// Refusing it would turn every NPC that has only greeting text into a
    /// parse error -- the same mistake as treating `SMSG_LOOT_RESPONSE`'s
    /// short form as a truncation.
    #[test]
    fn an_empty_menu_parses_rather_than_failing() {
        let gossip = parse_gossip_message(&MCBRIDE).unwrap();
        assert_eq!(gossip.menu_id, 4048);
        assert_eq!(gossip.text_id, 4938);
        assert!(gossip.is_empty());
    }

    /// **The two variable blocks must be told apart, and only a pair of
    /// samples can show it.**
    ///
    /// A layout that put the quest count somewhere else would still parse
    /// Farley's packet, which carries no quests, and would still parse
    /// McBride's, which carries nothing at all. Asserting that one sample has
    /// options and no quests while the other has quests and no options is what
    /// pins the two blocks to their own counts.
    #[test]
    fn the_option_and_quest_blocks_are_independent() {
        let farley = parse_gossip_message(&FARLEY).unwrap();
        let willem = parse_gossip_message(&WILLEM).unwrap();
        assert_eq!((farley.options.len(), farley.quests.len()), (3, 0));
        assert_eq!((willem.options.len(), willem.quests.len()), (0, 1));
    }

    /// A count that overruns must fail rather than return what it managed.
    #[test]
    fn a_truncated_option_list_is_an_error() {
        let mut body = FARLEY.to_vec();
        body[16] = 4; // claims four options, carries three
        assert!(parse_gossip_message(&body).is_err());
    }

    /// Trailing bytes are an error too -- the half that catches a field read
    /// too narrow, which no "ran out of input" check ever sees.
    #[test]
    fn trailing_bytes_are_an_error() {
        let mut body = WILLEM.to_vec();
        body.push(0);
        assert!(parse_gossip_message(&body).is_err());
    }

    /// A quest level of -1 means "scales to the player" and must not come back
    /// as four billion.
    ///
    /// No captured packet carries one, so this is built by editing a real
    /// sample rather than invented whole -- the field's *position* is
    /// confirmed by [`the_quest_agrees_with_the_servers_own_quest_table`] and
    /// only its signedness is being asserted here.
    #[test]
    fn a_negative_quest_level_stays_negative() {
        let mut body = WILLEM.to_vec();
        body[32..36].copy_from_slice(&(-1i32).to_le_bytes());
        let gossip = parse_gossip_message(&body).unwrap();
        assert_eq!(gossip.quests[0].level, -1);
        // And nothing after it moved: the title is still where it was.
        assert_eq!(gossip.quests[0].title, "A Threat Within");
    }
}
