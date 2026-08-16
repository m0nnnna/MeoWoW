//! What a vendor is selling.
//!
//! `SMSG_LIST_INVENTORY` answers a `CMSG_LIST_INVENTORY`, or arrives when a
//! gossip option that means "show me your wares" is chosen -- which is how the
//! first one here was obtained, and is worth stating because it makes the
//! reply doubly informative: it confirms
//! [`GossipSelectOption`](crate::ClientOpcode) at the same time, since nothing
//! but a correctly understood selection would have produced a stock list.
//!
//! **The layout is confirmed by a relationship the packet does not control**,
//! the same shape that settled `SMSG_LOOT_RESPONSE`: each entry carries an
//! item id *and* its display id, and those two are bound together by
//! `Item.dbc`, which the server never sends. Innkeeper Farley's twelve rows
//! pair 159 with 18084, 414 with 21904 and 422 with 6352, and the database
//! agrees on every one. Shift the block by a byte and that pairing breaks
//! where a length check would still pass.
//!
//! It is also checked by *count and order*: the twelve rows are exactly the
//! twelve in the server's `npc_vendor` table for that creature, in the same
//! order, and 8 + 1 + 12 * 32 is 393 -- the body's exact length.
//!
//! **The price is the field worth reading this module for.** It is not
//! `Item.dbc`'s `BuyPrice`. The server applies the buyer's reputation discount
//! before sending, and the arithmetic is unmistakable across three very
//! different values: 25 arrives as 23, 500 as 475, 2000 as 1900 -- `BuyPrice *
//! 0.95`, truncated. A client that displayed the table's price would show the
//! wrong number to every player at any standing other than neutral, and
//! nothing about the result would look wrong. **The wire is authoritative for
//! price; the table is not.**
//!
//! That reading is now confirmed by a consequence rather than by a table:
//! buying one row quoted at 23 took **exactly 23 copper** out of the purse,
//! where the table says 25. A price field read from the wrong offset could
//! not have predicted the charge.

use crate::protocol::{Error, Reader};

/// How many bytes one stock row occupies. Load-bearing: it is what makes the
/// body length a check rather than a coincidence.
const ITEM_BYTES: usize = 32;

/// One thing a vendor will sell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorItem {
    /// **The vendor's own slot number, and the handle a purchase has to
    /// use.** Observed as 1-based and consecutive here, but it is sent per row
    /// rather than implied by position for the same reason a loot slot and a
    /// gossip option index are: the server is free to leave holes, and a
    /// client that counted rows would buy the wrong thing exactly when it
    /// did. Not re-derived from the list's order.
    pub slot: u32,
    /// Row in `Item.dbc` and in the server's `item_template`.
    pub entry: u32,
    /// Row in `ItemDisplayInfo`. Sent so a client can draw the icon without an
    /// item query -- and it is what confirms this layout, since `Item.dbc`
    /// binds it to [`VendorItem::entry`] independently of anything the server
    /// chose to send.
    pub display_id: u32,
    /// How many are left in stock, or `None` when the vendor has an endless
    /// supply -- which arrives as `-1` rather than as a large number, so it is
    /// read signed and turned into an absence.
    pub remaining: Option<u32>,
    /// What it costs, in copper, **after the server has applied the buyer's
    /// reputation discount**.
    ///
    /// Use this and not `Item.dbc`'s `BuyPrice`; see the module comment. The
    /// discount is why: the same item is a different price to different
    /// players, and only the server knows which.
    pub price: u32,
    /// How many the buyer gets for one purchase at [`VendorItem::price`].
    ///
    /// **Confirmed by effect rather than by agreement.** Every row of the only
    /// vendor captured holds `5`, so matching `item_template.BuyCount` proved
    /// nothing on its own -- a constant agreeing with a constant. What settled
    /// it was buying one: exactly `price` copper left the purse and the item
    /// that arrived carried a **stack count of 5**. One purchase, one price,
    /// five items, and the field that predicted the five was this one.
    pub buy_count: u32,
    /// Whether this costs something other than money -- a currency, honour,
    /// tokens -- as a row in `ItemExtendedCost`, or `None` for a plain
    /// purchase.
    ///
    /// Every row observed is `0`, and the same is true of the `npc_vendor`
    /// rows behind them, so the two agree without either being tested. A
    /// vendor that actually takes tokens is what would confirm it.
    pub extended_cost: Option<u32>,
    /// The one field here that nothing has explained.
    ///
    /// Zero on all twelve rows of the only vendor captured, which is exactly
    /// as much as this client knows about it. **Deliberately unnamed**: it
    /// sits where item durability plausibly would, and naming it that from
    /// plausibility is the mistake `describe_cast_failure` exists to refuse.
    /// The first non-zero one settles it.
    pub unknown: u32,
}

/// A vendor's whole stock list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorList {
    /// Who is selling. Sent unpacked.
    pub vendor: u64,
    pub items: Vec<VendorItem>,
}

impl VendorList {
    /// Whether the vendor has nothing to offer.
    ///
    /// A real state rather than an error: a vendor whose stock is exhausted,
    /// or one the player cannot buy from, still answers.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Parses `SMSG_LIST_INVENTORY`.
///
/// Read through a cursor that must end exactly at the end of the body -- both
/// running out of input and having input left over are errors. That matters
/// more than usual here because the row count is a single byte followed by a
/// fixed-size array: a miscounted row does not shift one value, it
/// desynchronises every row after it, and an item id read from the wrong
/// offset is still a plausible-looking number.
pub fn parse_vendor_list(body: &[u8]) -> Result<VendorList, Error> {
    let mut r = Reader::new(body, "SMSG_LIST_INVENTORY");

    let vendor = r.u64()?;
    let count = r.u8()?;

    // **Check the count against the body before trusting it**, rather than
    // discovering the mismatch field by field. A row count is a single byte
    // and the rows are fixed-size, so the arithmetic is exact: anything else
    // means the header is not where this thinks it is, and saying so here
    // names the actual problem instead of reporting that some item's
    // extended-cost field ran off the end.
    let expected = count as usize * ITEM_BYTES;
    if r.remaining() != expected {
        return Err(Error::VendorRowCount {
            count,
            expected,
            got: r.remaining(),
        });
    }

    let mut items = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let slot = r.u32()?;
        let entry = r.u32()?;
        let display_id = r.u32()?;
        // Signed on the wire: an endless supply is -1, and reading it
        // unsigned would report four billion in stock.
        let remaining = match r.u32()? as i32 {
            negative if negative < 0 => None,
            finite => Some(finite as u32),
        };
        let price = r.u32()?;
        let unknown = r.u32()?;
        let buy_count = r.u32()?;
        let extended_cost = match r.u32()? {
            0 => None,
            row => Some(row),
        };

        items.push(VendorItem {
            slot,
            entry,
            display_id,
            remaining,
            price,
            buy_count,
            extended_cost,
            unknown,
        });
    }

    r.finish()?;
    Ok(VendorList { vendor, items })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first three rows of Innkeeper Farley's stock, exactly as a live
    /// realm sent them, with the header and the row count edited to match.
    ///
    /// Three rows rather than one because the check that matters is the
    /// entry/display pairing holding across *different* items -- one row
    /// agreeing with `Item.dbc` is a weaker statement than three doing so at
    /// three different offsets.
    const FARLEY: [u8; 105] = [
        0xc7, 0xd2, 0x00, 0x27, 0x01, 0x00, 0x30, 0xf1, // guid
        0x03, // three rows
        // slot 1: Refreshing Spring Water
        0x01, 0x00, 0x00, 0x00, // vendor slot 1
        0x9f, 0x00, 0x00, 0x00, // entry 159
        0xa4, 0x46, 0x00, 0x00, // display 18084
        0xff, 0xff, 0xff, 0xff, // unlimited
        0x17, 0x00, 0x00, 0x00, // 23 copper
        0x00, 0x00, 0x00, 0x00, // unknown
        0x05, 0x00, 0x00, 0x00, // five per purchase
        0x00, 0x00, 0x00, 0x00, // no extended cost
        // slot 2: Dalaran Sharp
        0x02, 0x00, 0x00, 0x00, //
        0x9e, 0x01, 0x00, 0x00, // entry 414
        0x90, 0x55, 0x00, 0x00, // display 21904
        0xff, 0xff, 0xff, 0xff, //
        0x76, 0x00, 0x00, 0x00, // 118 copper
        0x00, 0x00, 0x00, 0x00, //
        0x05, 0x00, 0x00, 0x00, //
        0x00, 0x00, 0x00, 0x00, //
        // slot 3: Dwarven Mild
        0x03, 0x00, 0x00, 0x00, //
        0xa6, 0x01, 0x00, 0x00, // entry 422
        0xd0, 0x18, 0x00, 0x00, // display 6352
        0xff, 0xff, 0xff, 0xff, //
        0xdb, 0x01, 0x00, 0x00, // 475 copper
        0x00, 0x00, 0x00, 0x00, //
        0x05, 0x00, 0x00, 0x00, //
        0x00, 0x00, 0x00, 0x00, //
    ];

    #[test]
    fn a_captured_stock_list_parses_exactly() {
        let list = parse_vendor_list(&FARLEY).unwrap();
        assert_eq!(list.vendor, 0xf130_0001_2700_d2c7);
        assert_eq!(list.items.len(), 3);
        assert!(!list.is_empty());
        assert_eq!(list.items[0].slot, 1);
    }

    /// **The check that makes the rest trustworthy.**
    ///
    /// Entry and display id are bound by `Item.dbc`, which the server never
    /// sends, so three adjacent pairs all agreeing with an outside table
    /// cannot happen by accident. Shift the row by one byte and every pairing
    /// breaks while the parse still succeeds.
    #[test]
    fn every_entry_and_display_id_agree_with_item_dbc() {
        let list = parse_vendor_list(&FARLEY).unwrap();
        let pairs: Vec<(u32, u32)> = list
            .items
            .iter()
            .map(|item| (item.entry, item.display_id))
            .collect();
        assert_eq!(
            pairs,
            vec![(159, 18084), (414, 21904), (422, 6352)],
            "entry and display must pair the way Item.dbc says they do"
        );
    }

    /// **The price on the wire is not the price in the table**, and this pins
    /// the difference so nobody "fixes" it back.
    ///
    /// `item_template.BuyPrice` is 25, 125 and 500 for these three; the wire
    /// carries 23, 118 and 475. That is the buyer's reputation discount --
    /// `BuyPrice * 0.95`, truncated -- applied by the server before sending.
    /// A client that displayed the table's number would be wrong for every
    /// player at any standing but neutral, silently.
    #[test]
    fn the_price_is_the_discounted_one_and_not_the_tables() {
        let list = parse_vendor_list(&FARLEY).unwrap();
        let prices: Vec<u32> = list.items.iter().map(|item| item.price).collect();
        assert_eq!(prices, vec![23, 118, 475]);

        // Stated as the relationship rather than as three constants, because
        // the relationship is the finding.
        for (price, base) in prices.iter().zip([25u32, 125, 500]) {
            assert_eq!(*price, base * 95 / 100, "reputation discount on {base}");
            assert_ne!(*price, base, "the table's price must not be what arrives");
        }
    }

    /// An endless supply is `-1`, not a large number.
    #[test]
    fn unlimited_stock_reads_as_absent_rather_than_four_billion() {
        let list = parse_vendor_list(&FARLEY).unwrap();
        assert!(list.items.iter().all(|item| item.remaining.is_none()));
    }

    /// A finite stock keeps its number, so the signed read cannot have turned
    /// every count into `None`.
    ///
    /// Built by editing a real sample rather than invented whole: the field's
    /// *position* is confirmed by the tests above and only the sign handling
    /// is asserted here.
    #[test]
    fn a_finite_stock_survives_the_signed_read() {
        let mut body = FARLEY.to_vec();
        body[21..25].copy_from_slice(&7u32.to_le_bytes());
        let list = parse_vendor_list(&body).unwrap();
        assert_eq!(list.items[0].remaining, Some(7));
        // And nothing after it moved.
        assert_eq!(list.items[0].price, 23);
        assert_eq!(list.items[1].entry, 414);
    }

    /// A row count that overruns must fail rather than return what it managed.
    #[test]
    fn a_truncated_row_list_is_an_error() {
        let mut body = FARLEY.to_vec();
        body[8] = 4; // claims four rows, carries three
        assert!(parse_vendor_list(&body).is_err());
    }

    /// Trailing bytes are an error too -- the half that catches a field read
    /// too narrow, which no "ran out of input" check ever sees.
    #[test]
    fn trailing_bytes_are_an_error() {
        let mut body = FARLEY.to_vec();
        body.push(0);
        assert!(parse_vendor_list(&body).is_err());
    }

    /// The row size is what makes the body length a check. Farley's full reply
    /// was 393 bytes for twelve rows, which is `8 + 1 + 12 * 32` exactly.
    #[test]
    fn the_row_size_accounts_for_the_captured_body_length() {
        assert_eq!(8 + 1 + 12 * ITEM_BYTES, 393);
        assert_eq!(FARLEY.len(), 8 + 1 + 3 * ITEM_BYTES);
    }

    /// A vendor with nothing to sell is a real reply, not an error.
    #[test]
    fn an_empty_stock_list_parses() {
        let body = [0xc7, 0xd2, 0x00, 0x27, 0x01, 0x00, 0x30, 0xf1, 0x00];
        let list = parse_vendor_list(&body).unwrap();
        assert!(list.is_empty());
    }
}
