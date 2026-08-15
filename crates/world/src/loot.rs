//! What is on a corpse.
//!
//! `SMSG_LOOT_RESPONSE` answers `CMSG_LOOT`, and this parses it. Everything
//! here was measured against a live realm rather than transcribed -- see
//! [`ClientOpcode::Loot`](crate::ClientOpcode) for how the opcode itself was
//! confirmed -- and the layout below is checked by a property the packet
//! cannot fake.
//!
//! **Getting a corpse with anything on it was the hard part, and three
//! attempts failed in a way that taught nothing.** A GM `.die` kill generates
//! no loot at all, so the first runs came back with the *empty* form: ten
//! bytes, a guid and two small numbers, which says almost nothing about where
//! an item list would live. Killing with `.damage` produced empty corpses too,
//! because an ordinary low-level creature usually rolls nothing.
//!
//! The fix was to stop hoping and pick a creature that **must** drop
//! something: `creature_loot_template` names a handful with a 100% chance, and
//! spawning one with `.npc add` put a guaranteed drop within reach. That is
//! the same rule as checking a property test's population before believing a
//! flat result -- a sample that cannot exhibit the thing being looked for is
//! not evidence that it does not exist.
//!
//! **The layout is confirmed by a relationship the packet does not control.**
//! Each item block carries an entry *and* a display id, and those two are
//! bound together by `Item.dbc`, which the server never sends. The captured
//! packet holds entry 2070 and display 6353, and both `Item.dbc` and the
//! server's own `item_template` independently say that item 2070 -- Darnassian
//! Bleu -- has display 6353. Two adjacent fields agreeing with an external
//! table is a far stronger statement than either field merely looking like a
//! plausible number, which is the trap this project keeps paying for.

use crate::protocol::{Error, Reader};

/// How long the short form is: a guid, a loot type and a status byte.
///
/// The two forms of this packet are told apart by **length alone** -- there is
/// no discriminator in the body -- so this constant is load-bearing rather
/// than decorative. See [`parse_loot_response`].
const SHORT_FORM: usize = 8 + 1 + 1;

/// One stack on a corpse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LootItem {
    /// Which loot slot this is, and **the handle a request has to use**. It is
    /// the server's index into this corpse's loot, not a position in the list
    /// this parse produced: a corpse whose first slot has already been taken
    /// still numbers the rest from where they were.
    pub slot: u8,
    pub entry: u32,
    pub count: u32,
    /// Row in `ItemDisplayInfo`, sent so a client can draw the icon without
    /// an item query. It is also what confirmed this layout -- see the module
    /// comment.
    pub display_id: u32,
    pub random_property_id: u32,
    pub random_suffix: u32,
    /// Whether this slot may be taken, rolled for, or only looked at. The
    /// values are **not** interpreted here: only `4` has been observed, and
    /// naming the rest from memory is exactly the mistake
    /// `describe_cast_failure` exists to avoid.
    pub slot_type: u8,
}

/// What a corpse is holding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loot {
    /// Whose corpse. Sent unpacked, as it was in the request.
    pub guid: u64,
    /// Observed as `1` on a creature corpse and `0` on an empty one. Left as a
    /// number rather than an enum for the same reason as
    /// [`LootItem::slot_type`].
    pub loot_type: u8,
    /// In copper. Zero when there is none -- and note that money is *not* one
    /// of the item slots: it is taken by its own request.
    pub money: u32,
    pub items: Vec<LootItem>,
    /// Set when the server sent the **short form**: a guid, a loot type and
    /// one more byte, and then nothing.
    ///
    /// This is a real shape, not a truncation, and treating it as one would
    /// make every empty corpse a parse error. It is ten bytes where the full
    /// form's header alone is fourteen, so the money and item-count fields are
    /// genuinely absent rather than zero -- a parser that read them anyway
    /// would be inventing four bytes it was never sent.
    ///
    /// The byte is returned raw. Only `0` has been observed, and naming a
    /// status code from memory is precisely what `describe_cast_failure`
    /// exists to refuse.
    pub error: Option<u8>,
}

impl Loot {
    /// Whether there is anything at all here.
    ///
    /// Worth having as a question because an empty corpse is answered with a
    /// release rather than with an empty window: the server closes it for you.
    pub fn is_empty(&self) -> bool {
        self.money == 0 && self.items.is_empty()
    }
}

/// Parses `SMSG_LOOT_RESPONSE`.
///
/// Parsed through a cursor that must end exactly at the end of the body. Both
/// halves of that matter and this project has four separate bugs on record
/// that were invisible field by field and obvious the moment a cursor reported
/// leftovers -- a packet sixteen bytes longer than expected, three missing
/// equipment slots, a result code off by one, and a position block read as
/// nine floats instead of eight.
pub fn parse_loot_response(body: &[u8]) -> Result<Loot, Error> {
    let mut r = Reader::new(body, "SMSG_LOOT_RESPONSE");

    let guid = r.u64()?;
    let loot_type = r.u8()?;

    // **The short form is a different message wearing the same opcode.** An
    // empty corpse comes back as guid, loot type and one status byte -- ten
    // bytes, where the full form's header alone is fourteen. It is length that
    // distinguishes them, which is unusual enough to be worth stating: there
    // is no discriminator in the body.
    //
    // Reading the money and count fields anyway would invent four bytes the
    // server never sent, and refusing the packet would make every empty corpse
    // a parse error. See `Loot::error`.
    if body.len() == SHORT_FORM {
        let error = r.u8()?;
        r.finish()?;
        return Ok(Loot {
            guid,
            loot_type,
            money: 0,
            items: Vec::new(),
            error: Some(error),
        });
    }

    let money = r.u32()?;
    let count = r.u8()?;

    let mut items = Vec::with_capacity(count as usize);
    for _ in 0..count {
        items.push(LootItem {
            slot: r.u8()?,
            entry: r.u32()?,
            count: r.u32()?,
            display_id: r.u32()?,
            random_property_id: r.u32()?,
            random_suffix: r.u32()?,
            slot_type: r.u8()?,
        });
    }

    r.finish()?;
    Ok(Loot {
        guid,
        loot_type,
        money,
        items,
        error: None,
    })
}

/// Parses `SMSG_LOOT_RELEASE_RESPONSE`: which corpse was closed.
///
/// Nine bytes -- a guid and one byte that has only ever been observed as `1`,
/// so it is returned raw rather than named.
pub fn parse_loot_release(body: &[u8]) -> Result<(u64, u8), Error> {
    let mut r = Reader::new(body, "SMSG_LOOT_RELEASE_RESPONSE");
    let guid = r.u64()?;
    let flag = r.u8()?;
    r.finish()?;
    Ok((guid, flag))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact bytes a live realm sent, kept as the known-good constant this
    /// project's conventions ask for.
    ///
    /// Garrick Padfoot, spawned with `.npc add 103` because his loot table has
    /// a 100% entry, killed with `.damage 5000`, opened with `CMSG_LOOT`.
    const CAPTURED: [u8; 36] = [
        0x42, 0xbf, 0x00, 0x67, 0x00, 0x00, 0x30, 0xf1, // guid
        0x01, // loot type
        0x05, 0x00, 0x00, 0x00, // 5 copper
        0x01, // one item
        0x00, // loot slot 0
        0x16, 0x08, 0x00, 0x00, // entry 2070
        0x01, 0x00, 0x00, 0x00, // count 1
        0xd1, 0x18, 0x00, 0x00, // display 6353
        0x00, 0x00, 0x00, 0x00, // random property
        0x00, 0x00, 0x00, 0x00, // random suffix
        0x04, // slot type
    ];

    #[test]
    fn a_captured_response_parses_exactly() {
        let loot = parse_loot_response(&CAPTURED).unwrap();
        assert_eq!(loot.guid, 0xf130_0000_6700_bf42);
        assert_eq!(loot.loot_type, 1);
        assert_eq!(loot.money, 5);
        assert_eq!(loot.items.len(), 1);

        let item = &loot.items[0];
        assert_eq!(item.slot, 0);
        assert_eq!(item.entry, 2070);
        assert_eq!(item.count, 1);
        assert_eq!(item.slot_type, 4);
        assert!(!loot.is_empty());
    }

    /// **The check that makes the rest trustworthy.**
    ///
    /// Entry and display id are bound together by `Item.dbc`, which the server
    /// never sends -- so two adjacent fields agreeing with an external table
    /// cannot happen by accident. Shift the item block by even one byte and
    /// this pairing breaks, where a length check would still pass.
    ///
    /// Item 2070 is Darnassian Bleu and its display is 6353, per `Item.dbc`
    /// *and* the server's own `item_template`, which agree independently.
    #[test]
    fn the_entry_and_display_id_agree_with_item_dbc() {
        let loot = parse_loot_response(&CAPTURED).unwrap();
        assert_eq!(
            (loot.items[0].entry, loot.items[0].display_id),
            (2070, 6353),
            "entry and display must pair the way Item.dbc says they do"
        );
    }

    /// The short form, exactly as a live realm sent it for an empty corpse.
    ///
    /// **This must parse, not fail.** It is a real message and it is what
    /// three earlier attempts kept producing -- so a parser that refused it
    /// would turn the commonest case into an error. It is also ten bytes where
    /// the full form's header alone is fourteen, so the money and count fields
    /// are genuinely absent: reading them would invent four bytes.
    #[test]
    fn the_short_form_parses_as_an_empty_corpse() {
        let body = [
            0x8e, 0xb6, 0x00, 0x1b, 0x15, 0x00, 0x30, 0xf1, // guid
            0x00, // loot type
            0x00, // status, returned raw
        ];
        let loot = parse_loot_response(&body).unwrap();
        assert_eq!(loot.guid, 0xf130_0015_1b00_b68e);
        assert_eq!(loot.error, Some(0));
        assert_eq!(loot.money, 0);
        assert!(loot.items.is_empty());
        assert!(loot.is_empty());
    }

    /// The two forms are told apart by length alone, so the full form must
    /// *not* be mistaken for the short one and vice versa.
    #[test]
    fn a_populated_response_is_not_read_as_the_short_form() {
        let loot = parse_loot_response(&CAPTURED).unwrap();
        assert_eq!(loot.error, None, "the full form has no status byte");
        assert_eq!(loot.money, 5);
    }

    /// A body with an item count that overruns must fail rather than return
    /// what it managed to read.
    #[test]
    fn a_truncated_item_list_is_an_error() {
        let mut body = CAPTURED.to_vec();
        body[13] = 2; // claims two items, carries one
        assert!(parse_loot_response(&body).is_err());
    }

    /// Trailing bytes are an error too. This is the half that catches a field
    /// read too narrow, which no "ran out of input" check ever sees.
    #[test]
    fn trailing_bytes_are_an_error() {
        let mut body = CAPTURED.to_vec();
        body.push(0);
        assert!(parse_loot_response(&body).is_err());
    }

    /// The release response, as captured.
    #[test]
    fn a_captured_release_parses() {
        let body = [0x42, 0xbf, 0x00, 0x67, 0x00, 0x00, 0x30, 0xf1, 0x01];
        assert_eq!(
            parse_loot_release(&body).unwrap(),
            (0xf130_0000_6700_bf42, 1)
        );
    }
}
