//! Player-to-player trade: the first exchange in this client where **both
//! ends have to act**.
//!
//! Everything before this block is one side asking and the other answering. A
//! vendor sells to whoever asks, a trainer teaches whoever asks, a flight
//! master flies whoever asks -- and in each case the second party is the
//! server, which is always listening. Trade is the first time the far end is
//! *another person's client*, which may be looking the other way, may decline,
//! and in the ordinary case has to send a packet of its own before this client
//! learns that anything happened at all.
//!
//! That single fact reshapes what can be measured, and it is worth stating
//! before any layout:
//!
//! **A successful `CMSG_INITIATE_TRADE` is silent to its sender.** The server
//! answers it by sending `SMSG_TRADE_STATUS` to the *partner*, carrying
//! [`TradeStatus::Begin`] and the initiator's guid. Nothing comes back to the
//! initiator until the partner's client replies with `CMSG_BEGIN_TRADE`, at
//! which point both sides get [`TradeStatus::OpenWindow`]. So the usual
//! bounding move -- send an answered request first, then the silent one -- has
//! nothing to work with inside this block: *every* request here is silent on
//! success.
//!
//! **The refusals are what talk back, and that is the instrument.** Every one
//! of the preconditions in the server's initiate handler answers the *sender*
//! with a status naming the reason. Sending `CMSG_INITIATE_TRADE` at a guid
//! that is not a player produces [`TradeStatus::NoTarget`] immediately, from
//! one client, with nobody else logged in -- and that one send bounds the
//! opcode, the body and the reply layout together, exactly as
//! `CMSG_LIST_INVENTORY` did for `CMSG_BUY_ITEM` and `CMSG_GROUP_INVITE` did
//! for the party block. The novelty is only that here the bounding request is
//! the **failing case of the request under test** rather than a neighbouring
//! opcode. A refusal that names a reason is a reply.
//!
//! ## The status word is a length as well as a reason
//!
//! `SMSG_TRADE_STATUS` is a `u32` status followed by a tail **whose shape
//! depends on the status**. That is a conditional layout, the shape that cost
//! 4.25 a whole feature when one flag bit was wrong -- but this one cannot
//! fail the same way, and the difference is worth having in writing. There the
//! two branches were *the same length* for some point counts, so a wrong
//! branch parsed cleanly and lost the route. Here the branches have different
//! lengths and the body is short, so a misreading leaves the cursor with bytes
//! in hand and `Reader::finish` refuses the packet in the log.
//!
//! Which means the **body length is itself evidence** about the status, and
//! the two agree independently:
//!
//! | status | body | tail |
//! |---|---|---|
//! | [`TradeStatus::Begin`] | 12 | the initiator's guid |
//! | [`TradeStatus::OpenWindow`] | 8 | a `u32` this module calls a token |
//! | [`TradeStatus::NoTarget`] | 13 | three fields the server writes as zero |
//! | everything else observed | 4 | none |
//!
//! ## The server tells the other person what you offered, and never tells you
//!
//! `SMSG_TRADE_STATUS_EXTENDED` is not "the trade". It is **one side of it**,
//! and which side is the first byte of the body: `1` means *what the other
//! person is offering you* and `0` means *what you are offering them*.
//!
//! The obvious design follows from that -- draw each half from the packet
//! that describes it -- and it is **wrong**, which is the most useful thing
//! this milestone found. Putting an item on the table or money beside it
//! makes the server send the `1` form to the *partner* and nothing at all to
//! the person who did it. Over a complete two-client trade -- two items, one
//! sum of money, both ends accepting -- **every** extended packet at both
//! clients carried `theirs = 1` and not one carried `0`. The `0` form is real
//! and reachable, but only when an enchant or a socket is being applied to
//! the non-traded slot, which is the one path in the server that sends both
//! forms.
//!
//! So **this client's own half of the window is the only thing in this whole
//! block it has to remember for itself.** Every other window in the six-part
//! services block is drawn from the server's own words; this one is half
//! local, and [`TradeSession::ours`] is that half -- item guids this client
//! put there, per slot.
//!
//! **What makes a local record safe here is that no refusal is quiet.** The
//! request is silent when it works, and every way it can fail -- a slot past
//! the seventh, an item that cannot be traded, an item already in another
//! slot -- answers with `TRADE_CANCELED` and ends the trade outright. There
//! is no state in which the client believes an item is on the table and the
//! server disagrees while the window is still open. That is what a remembered
//! half rests on, and it is worth stating because the usual rule in this
//! project points the other way: a silent request normally means the client
//! must not believe its own intentions.
//!
//! And even where the `0` form does arrive it is **not a substitute** for the
//! local record: it names an item's entry, display id and count, and nowhere
//! in the seventy-two bytes is the item's own guid. A client cannot work out
//! from it which of two identical stacks it put down.
//!
//! ## The slot byte is redundant, and that is what makes it useful
//!
//! Each of the seven slot records begins with its own index, and this server
//! writes all seven every time, in order -- so the byte says nothing a counter
//! could not. It is read anyway and checked against the counter, because a
//! record stride wrong by a word puts that byte in the middle of a `u32` and
//! the check then fails on the first record instead of on none of them. Same
//! use as the redundant `dir:` lines in `md5translate.trs`: a field that
//! carries no information still carries *confirmation*.
//!
//! ## What is deliberately not named
//!
//! The server's header lists two dozen status codes and transcribing them
//! would take a minute. Only the ones this project has actually produced are
//! named, for the standing reason: a wrong name for a status never errors, it
//! misexplains what happened and sends the next reader somewhere else. Each
//! variant below records how it was produced.

use crate::protocol::{Error, Reader};

/// Slots in one side's offer. Six tradeable, plus one that is not.
pub const TRADE_SLOTS: usize = 7;

/// The slot that does **not** change hands: it holds the item an enchant or a
/// socket is being applied *to*, so it goes back to its owner. Named because a
/// window that draws it among the six is offering something that will not be
/// given, which is the sort of wrong that costs somebody an item.
pub const NONTRADED_SLOT: u8 = 6;

/// Bytes in one slot record: its index, then eighteen words.
///
/// **Confirmed by accounting for the whole body**, which is the only check
/// available here -- there is no string at the end to land on, the way
/// [`crate::trainer`]'s greeting settled its stride. What stands in for it is
/// the leading index byte of each record: seven consecutive small integers in
/// the right order cannot survive a stride that is wrong by a word.
pub const SLOT_BYTES: usize = 1 + 18 * 4;

/// Bytes before the first slot record.
const EXTENDED_HEADER_BYTES: usize = 1 + 4 + 4 + 4 + 4 + 4;

/// The whole of `SMSG_TRADE_STATUS_EXTENDED`, which is a **fixed size**.
///
/// A fixed-length body is its own confirmation, the same property that made
/// `SMSG_SHOWTAXINODES` the cheapest packet in the previous milestone: nothing
/// variable-length can absorb a misreading, so a wrong field width shows up as
/// leftover bytes on the very first capture rather than as a plausible list.
pub const EXTENDED_BYTES: usize = EXTENDED_HEADER_BYTES + TRADE_SLOTS * SLOT_BYTES;

/// What the server has just said about the trade.
///
/// **Only values this project has produced are named**, and each one records
/// what produced it. The rest travel as [`TradeStatus::Other`] with their
/// number intact, exactly as [`crate::taxi::TaxiReply`] does and for the same
/// reason.
///
/// Note which variants carry data: those are not a convenience, they are the
/// packet's conditional layout, and a reader that treated the body as a bare
/// `u32` would leave eight bytes unread on every [`TradeStatus::Begin`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeStatus {
    /// The other end is already trading, or is trading with somebody else.
    ///
    /// **Observed** by initiating at a player already in a trade. The server
    /// also answers a gold request naming more money than the sender has with
    /// this same code, which is a second and quite different meaning for one
    /// value -- so it is reported and never explained to the player.
    Busy,
    /// Somebody wants to trade with you, and this is who.
    ///
    /// **The only packet in this block that names a guid**, and the reason the
    /// layout is confirmable by content rather than only by length: the guid
    /// is the initiator's, and on a two-client rig it can be checked against a
    /// value the other end knows independently.
    ///
    /// **Observed** on the partner's client the moment the initiator sent.
    Begin { partner: u64 },
    /// Both sides may now put things in. Arrives at *both* ends, in answer to
    /// the partner's `CMSG_BEGIN_TRADE`.
    ///
    /// The `u32` is written by this server as a literal zero and is echoed
    /// back by the original client in later requests; nothing here has seen it
    /// be anything else, so it is carried rather than interpreted.
    /// **Observed** at both ends.
    OpenWindow { token: u32 },
    /// The trade is off -- cancelled by either end, or refused by the server
    /// for a reason it did not itemise. **Observed** by cancelling.
    Cancelled,
    /// The *other* side has pressed accept. Note the direction: this never
    /// reports the reader's own accept back to them.
    ///
    /// **Observed** on one client when the other accepted.
    Accepted,
    /// An accept has been withdrawn, because one side changed the offer after
    /// accepting it. **Observed** by changing an offer that had been accepted.
    BackToTrade,
    /// The exchange happened. **Observed** at both ends, with the items and
    /// the money then confirmed in the two characters' own inventories.
    Complete,
    /// Nobody there. **Observed** by initiating at a guid that is not a player
    /// -- which is the send that bounds this whole block, since it is answered
    /// from one client with nobody else logged in.
    NoTarget,
    /// Too far away, or the target is in flight. **Observed** by walking out
    /// of range and initiating -- the server's limit is ten units.
    TooFar,
    /// Anything else. The number travels so a caller can report it, and
    /// nothing here pretends to know what it means.
    Other(u32),
}

impl TradeStatus {
    /// Whether this status ends the trade, so the window should close.
    ///
    /// Deliberately conservative: only the outcomes actually produced are
    /// treated as terminal. An unnamed status leaves the window open, which is
    /// recoverable, where closing on a status that merely reported something
    /// would drop a live trade on the floor.
    pub fn ends_trade(self) -> bool {
        matches!(self, Self::Cancelled | Self::Complete | Self::NoTarget)
    }
}

/// Parses `SMSG_TRADE_STATUS`.
///
/// The tail is read per status and the reader is made to finish empty. That is
/// what turns the body length into evidence: a status believed to be bare that
/// in fact carries a payload cannot pass this silently.
///
/// **An unrecognised status is allowed to carry a tail** and the tail is
/// dropped, because the alternative is refusing to report a status the server
/// genuinely sent. The caller keeps the raw body -- every probe in this tree
/// prints bodies rather than lengths for exactly this case.
pub fn parse_trade_status(body: &[u8]) -> Result<TradeStatus, Error> {
    let mut r = Reader::new(body, "SMSG_TRADE_STATUS");
    let code = r.u32()?;
    let status = match code {
        0 => TradeStatus::Busy,
        1 => TradeStatus::Begin { partner: r.u64()? },
        2 => TradeStatus::OpenWindow { token: r.u32()? },
        3 => TradeStatus::Cancelled,
        4 => TradeStatus::Accepted,
        6 => {
            // Three zero fields the server writes and the original client
            // ignores. Consumed rather than left, so `finish` still means what
            // it says -- and consumed by *remaining* rather than by a width,
            // because the widths of fields that are always zero cannot be
            // measured and should not be asserted.
            let left = r.remaining();
            r.skip(left)?;
            TradeStatus::NoTarget
        }
        7 => TradeStatus::BackToTrade,
        8 => TradeStatus::Complete,
        10 => TradeStatus::TooFar,
        other => {
            // See the doc comment: an unnamed status is reported, not refused,
            // and its tail is not guessed at.
            let left = r.remaining();
            r.skip(left)?;
            TradeStatus::Other(other)
        }
    };
    r.finish()?;
    Ok(status)
}

/// One item sitting in a trade slot.
///
/// Field *positions* are confirmed by the body accounting for exactly
/// [`EXTENDED_BYTES`]; field *meanings* are not all equally well established,
/// and the doc comments say which is which. [`TradeItem::entry`],
/// [`TradeItem::display_id`] and [`TradeItem::count`] are checkable without
/// the wire at all -- put a known item in and the entry is the one `.additem`
/// used, with the display id `Item.dbc` independently gives that entry, which
/// is the same paired check that confirmed the vendor list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeItem {
    /// Which of the seven slots. The server's own index, written into the
    /// record -- see the module comment on why it is read despite being
    /// redundant.
    pub slot: u8,
    /// The item entry, the handle `CMSG_ITEM_QUERY_SINGLE` and `Item.dbc` both
    /// take. **Confirmed** against a known item placed deliberately.
    pub entry: u32,
    /// The model, so the window can draw an icon without a query.
    /// **Confirmed** by agreeing with `Item.dbc`'s display id for
    /// [`TradeItem::entry`], which is an independent source.
    pub display_id: u32,
    /// Stack size. **Confirmed** against a deliberately uneven stack, because
    /// a count of one is the value every wrong reading also produces.
    pub count: u32,
    /// A wrapped gift: the window is supposed to hide the stats and show who
    /// wrapped it. Never non-zero here, so it is carried and not acted on.
    pub wrapped: bool,
    /// Who wrapped it, or zero. Meaningless without [`TradeItem::wrapped`].
    pub gift_creator: u64,
    /// The permanent enchantment, if any.
    pub enchant: u32,
    /// Three socket slots. Always three, zero-padded, like the trainer
    /// record's prerequisite spells.
    pub gems: [u32; 3],
    /// Who crafted it, or zero for everything a vendor sold.
    pub creator: u64,
    /// Charges left on a wand or a trinket. Left unsigned because nothing has
    /// produced a negative one, and a name for a value nobody has seen is what
    /// this project keeps refusing to write.
    pub charges: u32,
    /// The random-suffix scaling factor. Carried, not interpreted.
    pub suffix_factor: u32,
    /// The random property, **signed**: the sign distinguishes a suffix from a
    /// prefix. Read as `i32` rather than cast at the call site, for the reason
    /// `Reader::i32` exists -- a mirror timer read unsigned counts *up*
    /// forever.
    pub random_property: i32,
    /// A lock, for a locked box. Carried, not interpreted.
    pub lock_id: u32,
    /// Durability, out of [`TradeItem::max_durability`]. The original client's
    /// trade window shows a damaged item's condition, and this is where it
    /// comes from.
    pub max_durability: u32,
    pub durability: u32,
}

/// One side's half of an open trade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeOffer {
    /// **Whose offer this is**, off the first byte of the body: `true` means
    /// the partner's, `false` means the reader's own. See the module comment
    /// -- getting this wrong draws a convincing window that describes the
    /// wrong person's goods.
    pub theirs: bool,
    /// Echoed from [`TradeStatus::OpenWindow`]. Zero on this server.
    pub token: u32,
    /// The two slot counts the server writes, which are the same number in
    /// every capture. Kept as a pair rather than collapsed, because two fields
    /// that agree in every sample are not thereby one field.
    pub slot_count: u32,
    pub slot_count_again: u32,
    /// Copper on the table from this side.
    pub money: u32,
    /// A spell to be cast on whatever is in [`NONTRADED_SLOT`] once the trade
    /// goes through -- an enchant, a socket. Carried; nothing here casts it.
    pub spell: u32,
    /// The occupied slots only. An empty slot is seventy-two zero bytes and is
    /// dropped here, so an empty list means an empty offer rather than seven
    /// things a caller has to know to ignore.
    pub items: Vec<TradeItem>,
}

impl TradeOffer {
    /// Whether this side has put nothing at all on the table.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.money == 0
    }

    /// What is in one slot, if anything.
    pub fn item_at(&self, slot: u8) -> Option<&TradeItem> {
        self.items.iter().find(|i| i.slot == slot)
    }

    /// The six slots that actually change hands. See [`NONTRADED_SLOT`].
    pub fn traded(&self) -> impl Iterator<Item = &TradeItem> {
        self.items.iter().filter(|i| i.slot != NONTRADED_SLOT)
    }
}

/// Parses `SMSG_TRADE_STATUS_EXTENDED`.
///
/// The body is a fixed [`EXTENDED_BYTES`] and both ends of that are checked:
/// the reader must not run out and must not have anything left. Between them
/// sits the per-record index check, which is what localises a stride error to
/// a record rather than reporting it as a length at the end.
pub fn parse_trade_offer(body: &[u8]) -> Result<TradeOffer, Error> {
    let mut r = Reader::new(body, "SMSG_TRADE_STATUS_EXTENDED");

    let theirs = r.u8()? != 0;
    let token = r.u32()?;
    let slot_count = r.u32()?;
    let slot_count_again = r.u32()?;
    let money = r.u32()?;
    let spell = r.u32()?;

    let mut items = Vec::new();
    for index in 0..TRADE_SLOTS {
        let slot = r.u8()?;
        // **The stride check.** The byte is redundant as information and is
        // the whole confirmation as evidence: a 73-byte record read at 69 or
        // 77 puts this read inside a `u32`, and seven consecutive correct
        // indices do not happen by accident.
        if slot as usize != index {
            return Err(Error::TradeSlotOutOfOrder {
                expected: index as u8,
                got: slot,
            });
        }

        let entry = r.u32()?;
        let display_id = r.u32()?;
        let count = r.u32()?;
        let wrapped = r.u32()? != 0;
        let gift_creator = r.u64()?;
        let enchant = r.u32()?;
        let gems = [r.u32()?, r.u32()?, r.u32()?];
        let creator = r.u64()?;
        let charges = r.u32()?;
        let suffix_factor = r.u32()?;
        let random_property = r.i32()?;
        let lock_id = r.u32()?;
        let max_durability = r.u32()?;
        let durability = r.u32()?;

        // An empty slot is all zeroes, so the entry is what says whether there
        // is anything here. Dropped rather than kept as a `None`, with the
        // slot index carried on the item so nothing is lost.
        if entry != 0 {
            items.push(TradeItem {
                slot,
                entry,
                display_id,
                count,
                wrapped,
                gift_creator,
                enchant,
                gems,
                creator,
                charges,
                suffix_factor,
                random_property,
                lock_id,
                max_durability,
                durability,
            });
        }
    }

    r.finish()?;

    Ok(TradeOffer {
        theirs,
        token,
        slot_count,
        slot_count_again,
        money,
        spell,
        items,
    })
}

/// An open trade, or an offer of one waiting to be answered.
///
/// **Assembled from four different packets and one local fact**, which is
/// unusual in this crate and is the direct consequence of both ends having to
/// act. The two halves of the window arrive separately and asynchronously; who
/// the partner is arrives only at the *invited* end; and the *inviting* end
/// knows it only because it chose it, which is not on the wire at all.
///
/// Hence [`TradeSession::partner`] being an `Option`. It is the same rule the
/// party frames follow: a value nobody has stated is `None` rather than zero,
/// because a zero guid draws a name of nothing while claiming to know one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TradeSession {
    /// Who is on the other side, where this client has been told or already
    /// knew. `None` at the inviting end until the window opens is *not* what
    /// happens -- an initiator knows who it asked -- so `None` here means the
    /// window opened without this client ever learning the partner, which
    /// happens only if the `Begin` was missed.
    pub partner: Option<u64>,
    /// Echoed back on accept. See [`TradeStatus::OpenWindow`].
    pub token: u32,
    /// **Which end of the offer this client is on.** `true` when this client
    /// asked, `false` when somebody asked it.
    ///
    /// Not on the wire in either direction, and needed because the two ends
    /// hold the *same* state before the window opens -- a partner guid and
    /// `open: false`. Without it an interface has no way to tell "waiting for
    /// them to answer" from "they are waiting for me", and the natural
    /// mistake is to show the initiator a prompt asking whether they accept
    /// their own request.
    pub we_asked: bool,
    /// Whether the window is actually open, or this is still an unanswered
    /// offer.
    ///
    /// **The distinction is load-bearing and not cosmetic**: a refusal code
    /// arriving before the window opens is the server declining the request,
    /// and the same code arriving after it opens means something else
    /// entirely -- see [`TradeStatus::Busy`], which does both jobs.
    pub open: bool,
    /// What the partner is offering, as last restated by the server.
    pub theirs: Option<TradeOffer>,
    /// What this character has put on the table, by **item guid**, per slot.
    ///
    /// **Local, and the only half-window in this client that is.** The server
    /// sends the offer to the *other* person and not back to its author -- see
    /// the module comment, where the measurement is -- so there is nothing to
    /// read this from. It is what this client asked for.
    ///
    /// Safe to believe because no refusal here is quiet: every way
    /// `CMSG_SET_TRADE_ITEM` can fail cancels the trade, so an open window and
    /// a disputed item are not a state that exists.
    ///
    /// Guids rather than entries, because a guid is what identifies *which*
    /// of two identical stacks was put down -- and because the caller has to
    /// resolve it against its own inventory for the icon and the count
    /// anyway. The packet's own-half form could not supply this even where it
    /// arrives: it carries an entry and a count and no item guid at all.
    pub ours: [Option<u64>; TRADE_SLOTS],
    /// Copper this client has put on the table. Local, for the same reason.
    pub our_money: u32,
    /// Whether the partner has pressed accept.
    pub partner_accepted: bool,
    /// Whether this client has sent an accept. **Local**: the server never
    /// reports a client's own accept back to it, only the consequences.
    pub accepted: bool,
}

impl TradeSession {
    /// A trade this client has just asked for. The partner is known because
    /// this end chose them.
    pub fn requested(partner: u64) -> Self {
        Self {
            partner: Some(partner),
            we_asked: true,
            ..Default::default()
        }
    }

    /// A trade somebody else has just asked for.
    pub fn offered(partner: u64) -> Self {
        Self {
            partner: Some(partner),
            ..Default::default()
        }
    }

    /// Whether this client is being asked and has not answered yet.
    ///
    /// **The one question an interface has to get right before the window
    /// opens**, and neither half of it is on the wire: the two ends hold
    /// identical state at this point, and only [`Self::we_asked`] separates
    /// them. Getting it wrong shows the person who pressed the key a prompt
    /// asking whether they accept their own request.
    pub fn awaiting_our_answer(&self) -> bool {
        !self.open && !self.we_asked
    }

    /// Whether both ends have accepted, so the server is about to move things.
    pub fn both_accepted(&self) -> bool {
        self.accepted && self.partner_accepted
    }

    /// Whether this side has put anything at all on the table.
    pub fn we_offer_anything(&self) -> bool {
        self.our_money != 0 || self.ours.iter().any(Option::is_some)
    }

    /// Records an item this client has just put in a slot.
    ///
    /// **Also clears both accepts**, because the server does: changing an
    /// offer withdraws every agreement about it, and a client that kept its
    /// own accept lit would show a button as pressed that the server no
    /// longer counts. The partner's is cleared on the same reasoning -- their
    /// `BACK_TO_TRADE` is on its way but has not arrived, and a window that
    /// says they have accepted a table they have not seen is worse than one a
    /// beat behind.
    pub fn put(&mut self, slot: u8, item: u64) {
        if let Some(held) = self.ours.get_mut(slot as usize) {
            *held = Some(item);
            self.accepted = false;
            self.partner_accepted = false;
        }
    }

    /// Records an item taken back out of a slot.
    pub fn take_back(&mut self, slot: u8) {
        if let Some(held) = self.ours.get_mut(slot as usize) {
            *held = None;
            self.accepted = false;
            self.partner_accepted = false;
        }
    }

    /// Records money put on the table.
    pub fn put_money(&mut self, copper: u32) {
        self.our_money = copper;
        self.accepted = false;
        self.partner_accepted = false;
    }

    /// The first slot with nothing in it, or `None` when the six that change
    /// hands are full.
    ///
    /// **[`NONTRADED_SLOT`] is never offered**, which is the whole reason this
    /// is a method rather than a loop at the call site: a window that filled
    /// the seventh square while looking for room would put an item on the
    /// table that is going to be handed straight back.
    pub fn first_free_slot(&self) -> Option<u8> {
        (0..NONTRADED_SLOT).find(|slot| self.ours[*slot as usize].is_none())
    }

    /// Whether this item is already somewhere on our side of the table.
    ///
    /// The server treats putting one item in two slots as a cheat and
    /// **cancels the whole trade** for it, so this is checked before sending
    /// rather than discovered afterwards.
    pub fn already_offered(&self, item: u64) -> bool {
        self.ours.iter().any(|held| *held == Some(item))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status word decides the body length, so each shape is asserted
    /// separately -- and, crucially, each is asserted to be **refused** at the
    /// wrong length. A test that only checked the right shapes would pass just
    /// as well if the parser ignored trailing bytes, which is the mistake
    /// `Reader::finish` exists to prevent.
    #[test]
    fn status_tails_are_per_status() {
        let mut begin = 1u32.to_le_bytes().to_vec();
        begin.extend_from_slice(&0x0000_0001_0000_0007u64.to_le_bytes());
        assert_eq!(
            parse_trade_status(&begin).unwrap(),
            TradeStatus::Begin {
                partner: 0x0000_0001_0000_0007
            }
        );

        let mut open = 2u32.to_le_bytes().to_vec();
        open.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            parse_trade_status(&open).unwrap(),
            TradeStatus::OpenWindow { token: 0 }
        );

        assert_eq!(
            parse_trade_status(&8u32.to_le_bytes()).unwrap(),
            TradeStatus::Complete
        );

        // A `Begin` without its guid is truncated, not a bare status.
        assert!(parse_trade_status(&1u32.to_le_bytes()).is_err());
        // And a `Complete` that came with a guid is not silently accepted.
        let mut fat = 8u32.to_le_bytes().to_vec();
        fat.extend_from_slice(&0u64.to_le_bytes());
        assert!(parse_trade_status(&fat).is_err());
    }

    /// An unnamed code keeps its number rather than becoming an error or a
    /// default.
    #[test]
    fn unnamed_status_keeps_its_number() {
        assert_eq!(
            parse_trade_status(&23u32.to_le_bytes()).unwrap(),
            TradeStatus::Other(23)
        );
    }

    /// Builds one side's offer, with items in the slots named.
    fn extended(theirs: bool, money: u32, at: &[(u8, u32, u32, u32)]) -> Vec<u8> {
        let mut body = Vec::with_capacity(EXTENDED_BYTES);
        body.push(theirs as u8);
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&(TRADE_SLOTS as u32).to_le_bytes());
        body.extend_from_slice(&(TRADE_SLOTS as u32).to_le_bytes());
        body.extend_from_slice(&money.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        for slot in 0..TRADE_SLOTS as u8 {
            body.push(slot);
            match at.iter().find(|(s, ..)| *s == slot) {
                Some(&(_, entry, display, count)) => {
                    body.extend_from_slice(&entry.to_le_bytes());
                    body.extend_from_slice(&display.to_le_bytes());
                    body.extend_from_slice(&count.to_le_bytes());
                    for _ in 0..15 {
                        body.extend_from_slice(&0u32.to_le_bytes());
                    }
                }
                None => {
                    for _ in 0..18 {
                        body.extend_from_slice(&0u32.to_le_bytes());
                    }
                }
            }
        }
        body
    }

    /// The fixed size is the claim, so the fixture asserts it before the
    /// parser is asked anything.
    #[test]
    fn the_body_is_a_fixed_size() {
        assert_eq!(SLOT_BYTES, 73);
        assert_eq!(EXTENDED_BYTES, 532);
        assert_eq!(extended(true, 0, &[]).len(), EXTENDED_BYTES);
    }

    /// An offer with a gap in it: slot 0 empty, slot 1 and the non-traded slot
    /// occupied. The gap is the point -- a reading that collapsed the occupied
    /// slots into positions would report the second item as slot 0.
    #[test]
    fn slots_keep_their_own_index() {
        let body = extended(
            true,
            1234,
            &[(1, 2070, 6353, 5), (NONTRADED_SLOT, 7005, 1542, 1)],
        );
        let offer = parse_trade_offer(&body).unwrap();

        assert!(offer.theirs);
        assert_eq!(offer.money, 1234);
        assert_eq!(offer.items.len(), 2);

        let first = offer.item_at(1).expect("slot 1 is occupied");
        assert_eq!(first.entry, 2070);
        assert_eq!(first.display_id, 6353);
        assert_eq!(first.count, 5);

        assert!(offer.item_at(0).is_none());
        assert!(offer.item_at(NONTRADED_SLOT).is_some());

        // The non-traded slot is excluded from what actually changes hands.
        let traded: Vec<u8> = offer.traded().map(|i| i.slot).collect();
        assert_eq!(traded, vec![1]);
    }

    /// Which side the packet describes is a whole byte and the only thing
    /// separating two identically-shaped bodies.
    #[test]
    fn the_leading_byte_names_the_side() {
        assert!(parse_trade_offer(&extended(true, 0, &[])).unwrap().theirs);
        assert!(!parse_trade_offer(&extended(false, 0, &[])).unwrap().theirs);
    }

    /// A stride wrong by a word is caught at the *first* record by the index
    /// byte, not at the end by the length. Simulated by dropping four bytes out
    /// of the header, which is what shifts every record.
    #[test]
    fn a_shifted_record_is_caught_by_its_index() {
        let mut body = extended(true, 0, &[(0, 2070, 6353, 1)]);
        body.drain(1..5);
        let err = parse_trade_offer(&body).unwrap_err();
        assert!(
            matches!(err, Error::TradeSlotOutOfOrder { .. })
                || matches!(err, Error::Truncated { .. }),
            "expected a slot-order or truncation failure, got {err}"
        );
    }

    /// An empty offer is empty, and money alone is not.
    #[test]
    fn money_alone_is_an_offer() {
        assert!(
            parse_trade_offer(&extended(false, 0, &[]))
                .unwrap()
                .is_empty()
        );
        assert!(
            !parse_trade_offer(&extended(false, 1, &[]))
                .unwrap()
                .is_empty()
        );
    }
}
