//! Auctions: the first list this client cannot bound.
//!
//! Every list this client has read so far arrives whole. A vendor sends its
//! whole stock, a trainer its whole spellbook, a mailbox the whole inbox, a
//! guild its whole roster, a trade window seven slots of which six can hold
//! anything. In each case the count at the front of the packet is the length
//! of the thing itself, and a client that stores the reply stores the truth.
//!
//! `SMSG_AUCTION_LIST_RESULT` is not that. It carries **two counts**: how many
//! records are in this packet, and how many matched the search. On this realm
//! the first is capped at [`PAGE_ROWS`] and the second is however many
//! auctions exist. A live auction house on a populated realm holds tens of
//! thousands, and no opcode anywhere returns them.
//!
//! That single fact is what this milestone is about, and it changes three
//! things a client would otherwise get wrong:
//!
//! ## The page is not the list, so it cannot be cached as one
//!
//! [`AuctionPage`] is deliberately not called an auction *list* and
//! deliberately holds [`AuctionPage::offset`] -- the position it was asked
//! for. A page with no offset is a set of rows with no way to say what they
//! are rows *of*, and the mistake it invites is the expensive one: merging
//! successive pages into a growing collection that looks like the auction
//! house and is a stale union of snapshots taken at different times. Auctions
//! are bid on and bought out while the reader is paging. There is no moment
//! at which the union was ever true.
//!
//! So this module has no accumulator and the state that holds it keeps exactly
//! one page. It is the same reasoning as [`crate::quest_cache`]'s refusal to
//! answer `Option<&QuestInfo>` -- the type is shaped so the wrong thing cannot
//! be expressed -- and the opposite conclusion from [`crate::mail::Inbox`],
//! which *is* the whole thing and can be stored as one.
//!
//! ## Sorting is the server's job, and doing it locally looks identical
//!
//! [`AuctionSearch::sort`] travels in the request. That is unique in this
//! protocol: nothing else this client sends tells the server what order to
//! answer in. It exists because sorting is only meaningful over the whole
//! match, and a client holding fifty of forty thousand rows that sorts them
//! is sorting the wrong set.
//!
//! The failure is silent and worse than useless. Fifty rows sorted by price
//! ascending *look* like the cheapest fifty; they are an arbitrary fifty in
//! price order, and the cheapest auction in the house may be on page nine.
//! A player reading that column makes a decision on it. There is no rendering
//! difference between the right answer and the wrong one, which puts this in
//! the same family as the guild-chat line that drew in the wrong colour: a
//! plausible different answer never errors.
//!
//! AzerothCore only *applies* the sort block when the match exceeds one page,
//! which is a detail of this build and not something to rely on -- but it does
//! mean the small fixture this module was measured against cannot show the
//! sort working. Both halves are stated because only one of them is testable
//! here.
//!
//! ## The server states a rate limit, in the reply, in milliseconds
//!
//! Every list result ends with [`AuctionPage::search_delay_ms`] -- 300 on this
//! realm. That is the server saying how long to wait before searching again,
//! and it is the first time in this protocol that a limit has been *stated*
//! rather than discovered by tripping it. Three keepalives dropped the world
//! connection once because the server enforced a minimum ping interval nothing
//! announced; clicking a party loot control repeatedly closed the socket. Here
//! the number is in the packet, so a search box that fires on every keystroke
//! has no excuse.
//!
//! ## What bounds the block
//!
//! Nine of this block's ten requests are conditional on an auctioneer NPC:
//! wrong guid, out of range, dead, or not an auctioneer, and the server
//! returns without a word. A silent send is indistinguishable from a wrong
//! opcode, which is the failure mode this project has hit in every city
//! service so far.
//!
//! The tenth is
//! [`AuctionListPendingSales`](crate::ClientOpcode::AuctionListPendingSales).
//! Its handler reads eight bytes, discards them, and sends a reply. It checks
//! nothing -- not the NPC, not the range, not the level, not the body. It is a
//! **stronger** bounding instrument than `CMSG_GUILD_ROSTER` was, because its
//! answer does not depend on state at all: on this realm it is always a `u32`
//! zero, since the server's own loop over the records is commented out. A
//! reply that cannot vary cannot be mistaken for a reply that varied.
//!
//! ## The record is fat for the same reason a mailed item's is
//!
//! An auctioned item is in nobody's inventory. It is not a replicated object,
//! there is no guid for it in this client's world, and no query answers for
//! one -- exactly [`crate::mail::MailItem`]'s situation. So the record carries
//! everything inline: entry, stack count, charges, seven enchantment slots,
//! the random property and its suffix factor. [`RECORD_BYTES`] is the result,
//! and it is **measured** rather than counted from a struct: see
//! [`measure_stride`], and the note on it about why byte accounting alone
//! cannot settle it here.

use crate::protocol::{Error, Reader};

/// Enchantment slots written per auctioned item.
///
/// Seven, the same fixed zero-padded array as [`crate::mail::MailItem`]'s and
/// the trainer record's three prerequisites. Defined here rather than shared
/// with `mail` on purpose: two packets agreeing on a number today is not the
/// same fact as one packet's layout, and a shared constant would make a
/// divergence in either look like a bug in both.
pub const INSPECTED_ENCHANT_SLOTS: usize = 7;

/// Bytes in one auction record. **Measured on the wire**; see
/// [`measure_stride`].
///
/// Two ids, twenty-one enchantment words, five item words, an owner guid,
/// four money words, a bidder guid and the current bid.
pub const RECORD_BYTES: usize = 4
    + 4
    + INSPECTED_ENCHANT_SLOTS * 3 * 4
    + 4 * 5
    + 8
    + 4 * 4
    + 8
    + 4;

/// Bytes before the first record: the row count for this page.
pub const HEADER_BYTES: usize = 4;

/// Bytes after the last record: the total match count and the search delay.
///
/// **The reason a wrong stride does not run off the end here.** A trailing
/// block means a stride out by a word consumes the footer as record data and
/// then reads the last record's tail as the total -- the parse "succeeds" and
/// the number a client shows the player is garbage. What catches it is the
/// cursor: with a fixed-width record and no strings anywhere, `finish()` is a
/// complete check, which is precisely what it was not for the guild roster.
pub const FOOTER_BYTES: usize = 8;

/// Rows the server will put in one page. Observed, not negotiated: nothing in
/// the request asks for a page size and nothing in the reply states one.
///
/// Named so the interface can say "50 of 1,284" rather than implying the
/// number is a property of the search.
pub const PAGE_ROWS: usize = 50;

/// Durations the server accepts, in minutes.
///
/// It multiplies the field by sixty and then compares against exactly twelve,
/// twenty-four and forty-eight hours -- so anything else is **dropped in
/// silence**, with no result packet, and looks like a wrong opcode. That is
/// the reason this is an enum and not a `u32`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum AuctionDuration {
    TwelveHours = 720,
    TwentyFourHours = 1440,
    FortyEightHours = 2880,
}

impl AuctionDuration {
    /// The value as it travels.
    pub fn minutes(self) -> u32 {
        self as u32
    }
}

/// One enchantment slot on an auctioned item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuctionEnchant {
    pub id: u32,
    pub duration: u32,
    pub charges: u32,
}

impl AuctionEnchant {
    /// Whether this slot holds anything. All seven travel whether or not the
    /// item can hold that many.
    pub fn is_set(&self) -> bool {
        self.id != 0
    }
}

/// One auction, as a list result describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Auction {
    /// The server's own id for this auction, and the handle every request in
    /// the block names it by. **Not a row position** -- the same rule the
    /// gossip menu, the loot slots and the trainer rows each proved
    /// separately, and the one that makes paging safe at all: page two's rows
    /// carry the same ids they would have carried on page one.
    pub id: u32,
    /// The item's entry. Its *name* is not here and never is -- it comes from
    /// [`crate::ClientOpcode::ItemQuerySingle`], keyed by entry, so one
    /// answer names every copy in the house.
    pub item: u32,
    pub enchants: [AuctionEnchant; INSPECTED_ENCHANT_SLOTS],
    /// Negative for a suffix, positive for a property, zero for neither.
    pub random_property: i32,
    pub suffix_factor: u32,
    /// How many are in the stack. A stack is auctioned and bought whole.
    pub count: u32,
    pub spell_charges: u32,
    /// The server writes a literal zero here and the original client ignores
    /// it. Kept as a field rather than skipped so the record accounts for
    /// itself byte for byte.
    pub flags: u32,
    pub owner: u64,
    /// The opening price the seller set.
    pub start_bid: u32,
    /// What must be *added* to the current bid, not the total to send.
    ///
    /// Zero when nobody has bid yet, because there is nothing to outbid. See
    /// [`Auction::next_bid`], which is the only place the two cases are
    /// combined -- doing it at each call site is how one of them ends up
    /// wrong.
    pub min_increment: u32,
    /// Zero means the seller offered no buyout.
    pub buyout: u32,
    /// Milliseconds remaining, not a timestamp.
    ///
    /// So it is only true at the instant the packet was built, and a window
    /// left open counts down from a number that is already stale. The
    /// interface treats it as one of four bands rather than a clock for that
    /// reason -- see [`Auction::band`].
    pub time_left_ms: u32,
    /// Zero when nobody has bid.
    pub bidder: u64,
    /// The current bid, zero when there is none.
    pub bid: u32,
}

/// How much time is left, to the precision the original client showed.
///
/// The wire carries milliseconds and the interface shows a band, which is not
/// a loss: the number is a countdown captured when the packet was built, so
/// showing it as a clock claims a precision the client does not have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TimeBand {
    /// Under thirty minutes.
    Short,
    /// Under two hours.
    Medium,
    /// Under twelve hours.
    Long,
    /// Twelve hours or more.
    VeryLong,
}

impl TimeBand {
    pub fn label(self) -> &'static str {
        match self {
            TimeBand::Short => "Short",
            TimeBand::Medium => "Medium",
            TimeBand::Long => "Long",
            TimeBand::VeryLong => "Very Long",
        }
    }
}

impl Auction {
    /// Whether anybody has bid.
    pub fn has_bid(&self) -> bool {
        self.bidder != 0
    }

    /// The smallest price [`crate::ClientOpcode::AuctionPlaceBid`] will
    /// accept.
    ///
    /// **The two cases are different fields**, which is the whole reason this
    /// exists: with no bid the floor is the seller's opening price, and with
    /// one it is the current bid plus the increment the server computed. A
    /// client that always sent `bid + min_increment` would send zero on an
    /// unbid auction and be refused; one that always sent `start_bid` would
    /// underbid every contested auction and be refused differently.
    pub fn next_bid(&self) -> u32 {
        if self.has_bid() {
            self.bid.saturating_add(self.min_increment)
        } else {
            self.start_bid
        }
    }

    /// Whether the seller offered a buyout at all.
    pub fn can_buy_out(&self) -> bool {
        self.buyout != 0
    }

    /// Whether this character is the seller.
    ///
    /// Worth asking before drawing a bid button: the server refuses a bid on
    /// your own auction, and it refuses one placed by *another character on
    /// the same account* with the identical code. The second case cannot be
    /// detected here, because a client knows its own guid and not its
    /// account's other characters.
    pub fn is_own(&self, player: u64) -> bool {
        self.owner == player
    }

    /// Which band the remaining time falls in.
    pub fn band(&self) -> TimeBand {
        const MINUTE: u32 = 60 * 1000;
        match self.time_left_ms {
            ms if ms < 30 * MINUTE => TimeBand::Short,
            ms if ms < 120 * MINUTE => TimeBand::Medium,
            ms if ms < 720 * MINUTE => TimeBand::Long,
            _ => TimeBand::VeryLong,
        }
    }
}

/// One page of a search, and the two counts that make it a page.
///
/// See this module's header for why successive pages are not merged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuctionPage {
    /// The rows in this packet. At most [`PAGE_ROWS`].
    pub auctions: Vec<Auction>,
    /// How many auctions matched the search, over the whole house.
    ///
    /// **This is the field the milestone is named for.** It can exceed
    /// `auctions.len()` without bound, and nothing else in this protocol has
    /// ever done that.
    pub total: u32,
    /// How long the server wants between searches, in milliseconds.
    pub search_delay_ms: u32,
    /// The offset this page was asked for.
    ///
    /// Not on the wire -- the reply says nothing about where in the match it
    /// starts, so the requester has to remember. A page without it cannot say
    /// what it is a page of, and every paging control needs it.
    pub offset: u32,
}

impl AuctionPage {
    /// Whether this one packet holds the entire match.
    ///
    /// True for an owner or bidder list, which never page, and for any search
    /// matching [`PAGE_ROWS`] or fewer.
    pub fn is_whole(&self) -> bool {
        self.offset == 0 && self.auctions.len() as u32 == self.total
    }

    /// How many pages the match spans, at this server's page size.
    ///
    /// Derived from [`PAGE_ROWS`] rather than from `auctions.len()`, which is
    /// the *last* page's length on the last page and would say one page too
    /// few every time somebody looked at it.
    pub fn pages(&self) -> u32 {
        if self.total == 0 {
            return 0;
        }
        self.total.div_ceil(PAGE_ROWS as u32)
    }

    /// The offset of the next page, or `None` at the end of the match.
    pub fn next_offset(&self) -> Option<u32> {
        let next = self.offset + self.auctions.len() as u32;
        (!self.auctions.is_empty() && next < self.total).then_some(next)
    }

    /// The offset of the previous page, or `None` at the start.
    pub fn previous_offset(&self) -> Option<u32> {
        (self.offset != 0).then(|| self.offset.saturating_sub(PAGE_ROWS as u32))
    }

    /// Whether this page starts past the end of the match.
    ///
    /// **A real state and not a defensive check.** The server answers a
    /// `listfrom` beyond the match with an empty page and the true total, so
    /// asking for row 100 of a 39-row match is answered rather than refused --
    /// and the naive page arithmetic then says "page 3 of 1", which is
    /// nonsense on screen and was printed by this project's own probe before
    /// anything asserted otherwise.
    ///
    /// It happens for a reason nobody has to be careless about: narrowing the
    /// search while on page nine leaves the offset where it was and the match
    /// far shorter than it used to be.
    pub fn past_the_end(&self) -> bool {
        self.auctions.is_empty() && self.offset > 0
    }

    /// Find one auction in this page by the server's id.
    pub fn get(&self, id: u32) -> Option<&Auction> {
        self.auctions.iter().find(|a| a.id == id)
    }
}

/// Which auction house an auctioneer belongs to, from `MSG_AUCTION_HELLO`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuctionHouse {
    /// The NPC that was greeted, echoed back. Every subsequent request in the
    /// block has to carry it again.
    pub auctioneer: u64,
    /// The house's id: `AuctionHouse.dbc`'s row, not the NPC's faction.
    ///
    /// **The only field in the whole block that says which set of goods is
    /// being looked at.** Nothing in a list result, a bid or a cancellation
    /// mentions it, so a client that lets the player greet a second
    /// auctioneer without noticing is showing one house's rows and sending
    /// another house's requests.
    pub house: u32,
    /// Whether the house is open for business. The server writes a literal
    /// one; a zero is the client's cue to draw nothing.
    pub enabled: bool,
}

/// What a post, a bid or a cancellation was for.
///
/// The reply echoes it, which is what ties an answer to its request -- the
/// same property that made `SMSG_SEND_MAIL_RESULT` cheap to bound and its
/// absence is what made trade expensive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuctionAction {
    Sell,
    Cancel,
    Bid,
    /// A number this parser has not seen. Kept rather than refused, because
    /// the packet is fixed-width and an unknown action desynchronises nothing.
    Other(u32),
}

impl AuctionAction {
    pub fn from_wire(value: u32) -> Self {
        match value {
            0 => AuctionAction::Sell,
            1 => AuctionAction::Cancel,
            2 => AuctionAction::Bid,
            other => AuctionAction::Other(other),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AuctionAction::Sell => "post",
            AuctionAction::Cancel => "cancel",
            AuctionAction::Bid => "bid",
            AuctionAction::Other(_) => "unknown action",
        }
    }
}

/// The result of a post, a bid or a cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuctionOutcome {
    /// The auction the server acted on. **Zero on most failures**, because
    /// several refusals are decided before any auction is looked up -- so a
    /// zero here is "no auction", not "auction number zero".
    pub auction: u32,
    pub action: AuctionAction,
    /// Zero is success. Anything else is left as a number; see
    /// [`describe_auction_error`].
    pub error: u32,
    /// A second code, present **only when `error` is zero and `action` is
    /// not**.
    ///
    /// A conditional trailing field, which is normally the shape this project
    /// refuses to guess at -- but nothing follows it, the packet is
    /// fixed-width, and the cursor's leftover check therefore decides it
    /// rather than a hypothesis. The condition is asserted from the *body
    /// length*, not assumed from the other two fields, so a build that
    /// disagrees fails loudly instead of silently reading four bytes of
    /// nothing.
    pub bid_error: Option<u32>,
}

impl AuctionOutcome {
    pub fn succeeded(&self) -> bool {
        self.error == 0
    }
}

/// Name the auction error codes this project has actually observed.
///
/// **Only the ones observed**, and everything else comes back as its number,
/// for the reason `describe_cast_failure` does the same: a wrong offset
/// eventually fails loudly and a wrong *name* for a status code never does.
/// A visible `13` says "not implemented"; a confidently wrong sentence says
/// nothing and is believed.
pub fn describe_auction_error(code: u32) -> String {
    match code {
        0 => "ok".to_string(),
        2 => "the server declined it (database error)".to_string(),
        3 => "not enough money".to_string(),
        10 => "you cannot bid on your own auction".to_string(),
        other => format!("auction error {other}"),
    }
}

/// Somebody outbid this character, or this character won.
///
/// **Arrives unprompted**, like `SMSG_RECEIVED_MAIL` -- and unlike it, this
/// one says what happened. It is the second packet in this client that answers
/// nothing and the first that carries a payload worth drawing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BidderNotification {
    /// Which auction house. Not useful on its own, but it is what the server
    /// leads with.
    pub location: u32,
    pub auction: u32,
    /// Who now holds the auction -- **zero when this character does**, which
    /// is how a win is told apart from a loss without a second field.
    pub bidder: u64,
    pub bid: u32,
    /// How much more would have been needed.
    pub increment: u32,
    pub item: u32,
}

/// One of this character's auctions has sold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnerNotification {
    pub auction: u32,
    pub bid: u32,
    pub item: u32,
}

/// One sort key in a search request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SortKey {
    /// The column, as the original client numbers them. Left as a number
    /// rather than an enum: the server accepts anything under its own maximum
    /// and this project has confirmed none of the meanings.
    pub column: u8,
    pub descending: bool,
}

/// Everything `CMSG_AUCTION_LIST_ITEMS` can filter on.
///
/// [`AuctionSearch::any`] is the one to start from: every numeric filter is
/// disabled by [`ANY`] rather than by zero, and a zeroed struct means "class
/// 0, subclass 0, quality 0", which is a real and very narrow search. That is
/// the trap this type exists to close -- `Default` is deliberately **not**
/// implemented, so nobody gets the narrow search by writing `..Default::default()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuctionSearch {
    /// Substring of the item's name, matched case-insensitively by the
    /// server. Empty matches everything.
    pub name: String,
    /// Item level floor and ceiling. Zero for either disables it.
    pub level_min: u8,
    pub level_max: u8,
    /// `InventoryType` -- the equipment slot. [`ANY`] for no filter.
    pub slot: u32,
    /// Item class. [`ANY`] for no filter.
    pub class: u32,
    /// Item subclass. [`ANY`] for no filter.
    pub subclass: u32,
    /// Item quality. [`ANY`] for no filter.
    pub quality: u32,
    /// Only what this character can use. **The most expensive filter in the
    /// protocol**: the server answers it by walking the character's whole
    /// skill map and spell map, so it is the one flag worth not setting by
    /// default.
    pub usable: bool,
    /// Ask for everything, ignoring every filter above.
    ///
    /// The server caps the answer at 55,000 rows and builds it in one packet.
    /// It is here because it is on the wire, and the interface does not offer
    /// it: the original client rate-limits it to once every fifteen minutes,
    /// and a request that can return a multi-megabyte packet is not something
    /// to attach to a button somebody can lean on.
    pub get_all: bool,
    /// Which columns to sort by, in order. See this module's header for why
    /// this belongs in the request.
    pub sort: Vec<SortKey>,
}

/// The value that disables a numeric filter. **Not zero** -- zero is a real
/// item class.
pub const ANY: u32 = 0xFFFF_FFFF;

impl AuctionSearch {
    /// A search with every filter off. The honest starting point, and the
    /// only constructor, so the narrow all-zeroes search cannot be reached by
    /// accident.
    pub fn any() -> Self {
        AuctionSearch {
            name: String::new(),
            level_min: 0,
            level_max: 0,
            slot: ANY,
            class: ANY,
            subclass: ANY,
            quality: ANY,
            usable: false,
            get_all: false,
            sort: Vec::new(),
        }
    }

    /// The same, narrowed to a name substring.
    pub fn named(name: &str) -> Self {
        AuctionSearch {
            name: name.to_string(),
            ..AuctionSearch::any()
        }
    }
}

/// Read one list result: a search page, an owner list or a bidder list.
///
/// All three share this layout exactly, which is the reason one function reads
/// all three and the reason the owner list is worth sending at all as a check:
/// its `total` always equals its `count`, so a body whose two numbers disagree
/// says the footer is not where this parser thinks it is.
///
/// `offset` is what the caller asked for. It is not on the wire -- see
/// [`AuctionPage::offset`].
pub fn parse_auction_page(
    body: &[u8],
    offset: u32,
    what: &'static str,
) -> Result<AuctionPage, Error> {
    let mut reader = Reader::new(body, what);
    let count = reader.u32()?;

    // The count comes first and the records are fixed-width, so a count read
    // at the wrong offset asks for an allocation the body cannot possibly
    // fill. Checked before reserving, like the vendor, trainer and mail
    // lists -- the allocation should not be the thing that finds out.
    let need = (count as usize)
        .checked_mul(RECORD_BYTES)
        .ok_or(Error::AuctionRowCount {
            count,
            expected: usize::MAX,
            got: reader.remaining(),
        })?;
    if need + FOOTER_BYTES > reader.remaining() {
        return Err(Error::AuctionRowCount {
            count,
            expected: need + FOOTER_BYTES,
            got: reader.remaining(),
        });
    }

    let mut auctions = Vec::with_capacity(count as usize);
    for _ in 0..count {
        auctions.push(parse_auction(&mut reader)?);
    }

    let total = reader.u32()?;
    let search_delay_ms = reader.u32()?;
    reader.finish()?;

    Ok(AuctionPage {
        auctions,
        total,
        search_delay_ms,
        offset,
    })
}

fn parse_auction(reader: &mut Reader<'_>) -> Result<Auction, Error> {
    let id = reader.u32()?;
    let item = reader.u32()?;

    let mut enchants = [AuctionEnchant::default(); INSPECTED_ENCHANT_SLOTS];
    for slot in &mut enchants {
        slot.id = reader.u32()?;
        slot.duration = reader.u32()?;
        slot.charges = reader.u32()?;
    }

    Ok(Auction {
        id,
        item,
        enchants,
        random_property: reader.i32()?,
        suffix_factor: reader.u32()?,
        count: reader.u32()?,
        spell_charges: reader.u32()?,
        flags: reader.u32()?,
        owner: reader.u64()?,
        start_bid: reader.u32()?,
        min_increment: reader.u32()?,
        buyout: reader.u32()?,
        time_left_ms: reader.u32()?,
        bidder: reader.u64()?,
        bid: reader.u32()?,
    })
}

/// Read `MSG_AUCTION_HELLO`'s reply.
pub fn parse_auction_hello(body: &[u8]) -> Result<AuctionHouse, Error> {
    let mut reader = Reader::new(body, "MSG_AUCTION_HELLO");
    let house = AuctionHouse {
        auctioneer: reader.u64()?,
        house: reader.u32()?,
        enabled: reader.u8()? != 0,
    };
    reader.finish()?;
    Ok(house)
}

/// Read `SMSG_AUCTION_COMMAND_RESULT`.
///
/// **The fourth word's presence is decided by the body's length, not by the
/// first three fields.** The server's condition is "no error and a non-zero
/// action", and copying that condition here would mean a build that wrote the
/// word unconditionally -- or never -- parsed as success with a garbage code
/// or failed at the cursor with a misleading message. Reading what is there
/// and checking the two agree keeps the diagnosis where a disagreement is.
pub fn parse_command_result(body: &[u8]) -> Result<AuctionOutcome, Error> {
    let mut reader = Reader::new(body, "SMSG_AUCTION_COMMAND_RESULT");
    let auction = reader.u32()?;
    let action = AuctionAction::from_wire(reader.u32()?);
    let error = reader.u32()?;
    let bid_error = if reader.remaining() >= 4 {
        Some(reader.u32()?)
    } else {
        None
    };
    reader.finish()?;
    Ok(AuctionOutcome {
        auction,
        action,
        error,
        bid_error,
    })
}

/// Read `SMSG_AUCTION_BIDDER_NOTIFICATION`.
///
/// Seven words on the wire and six fields here: the last is a zero the server
/// writes and names nothing. It is read so the cursor accounts for the whole
/// body rather than skipped.
pub fn parse_bidder_notification(body: &[u8]) -> Result<BidderNotification, Error> {
    let mut reader = Reader::new(body, "SMSG_AUCTION_BIDDER_NOTIFICATION");
    let notification = BidderNotification {
        location: reader.u32()?,
        auction: reader.u32()?,
        bidder: reader.u64()?,
        bid: reader.u32()?,
        increment: reader.u32()?,
        item: reader.u32()?,
    };
    let _unused = reader.u32()?;
    reader.finish()?;
    Ok(notification)
}

/// Read `SMSG_AUCTION_OWNER_NOTIFICATION`.
///
/// The server writes an unused word, an unused guid, an unused word and an
/// unused float around the three fields that mean anything. Every one is read
/// rather than seeked past.
pub fn parse_owner_notification(body: &[u8]) -> Result<OwnerNotification, Error> {
    let mut reader = Reader::new(body, "SMSG_AUCTION_OWNER_NOTIFICATION");
    let auction = reader.u32()?;
    let bid = reader.u32()?;
    let _unused = reader.u32()?;
    let _unused_guid = reader.u64()?;
    let item = reader.u32()?;
    let _unused = reader.u32()?;
    let _unused_float = reader.f32()?;
    reader.finish()?;
    Ok(OwnerNotification { auction, bid, item })
}

/// Read `SMSG_AUCTION_LIST_PENDING_SALES`, and return how many records it
/// announced.
///
/// **On this realm the answer is always zero and the body is always four
/// bytes**, because the server's loop over the records is commented out in its
/// own source. That is what makes this the block's bounding instrument: an
/// answer that cannot vary with state cannot be confused for one that did.
///
/// A non-zero count is therefore refused rather than parsed, because the
/// record shape has never been observed and inventing one would produce a
/// structure that parses and describes nothing.
pub fn parse_pending_sales(body: &[u8]) -> Result<u32, Error> {
    let mut reader = Reader::new(body, "SMSG_AUCTION_LIST_PENDING_SALES");
    let count = reader.u32()?;
    if count != 0 {
        return Err(Error::UnconfirmedPendingSales { count });
    }
    reader.finish()?;
    Ok(count)
}

/// `MSG_AUCTION_HELLO`'s body: the auctioneer's guid, unpacked.
pub fn hello_body(auctioneer: u64) -> Vec<u8> {
    auctioneer.to_le_bytes().to_vec()
}

/// `CMSG_AUCTION_LIST_PENDING_SALES`' body: eight bytes the server reads and
/// discards.
///
/// It takes the auctioneer's guid because that is what the original client
/// sends and because a body of the right *length* is the only thing this
/// handler needs -- it never looks at the value. Sending a real guid costs
/// nothing and keeps the send honest if a later build starts checking.
pub fn pending_sales_body(auctioneer: u64) -> Vec<u8> {
    auctioneer.to_le_bytes().to_vec()
}

/// `CMSG_AUCTION_LIST_ITEMS`' body -- the heaviest request this client builds.
///
/// The fields are in the server's read order and the widths are not uniform:
/// the two levels are bytes, the four category filters are words, `usable` and
/// `get_all` are bytes again, and the sort block is a count followed by pairs
/// of bytes. Written once here rather than at the call site for the reason
/// every two-way structure in this crate is: a body built by its caller has
/// nothing to round-trip against.
pub fn list_items_body(auctioneer: u64, offset: u32, search: &AuctionSearch) -> Vec<u8> {
    let mut body = Vec::with_capacity(40 + search.name.len() + search.sort.len() * 2);
    body.extend_from_slice(&auctioneer.to_le_bytes());
    body.extend_from_slice(&offset.to_le_bytes());
    body.extend_from_slice(search.name.as_bytes());
    body.push(0);
    body.push(search.level_min);
    body.push(search.level_max);
    body.extend_from_slice(&search.slot.to_le_bytes());
    body.extend_from_slice(&search.class.to_le_bytes());
    body.extend_from_slice(&search.subclass.to_le_bytes());
    body.extend_from_slice(&search.quality.to_le_bytes());
    body.push(u8::from(search.usable));
    body.push(u8::from(search.get_all));
    body.push(search.sort.len() as u8);
    for key in &search.sort {
        body.push(key.column);
        body.push(u8::from(key.descending));
    }
    body
}

/// `CMSG_AUCTION_LIST_OWNER_ITEMS`' body.
///
/// The offset travels and is **read and ignored** -- this list does not page.
/// It is sent anyway because the handler reads it, and a handler reading past
/// the end of a short body refuses the packet.
pub fn list_owner_items_body(auctioneer: u64, offset: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&auctioneer.to_le_bytes());
    body.extend_from_slice(&offset.to_le_bytes());
    body
}

/// `CMSG_AUCTION_LIST_BIDDER_ITEMS`' body.
///
/// **The server checks this one's length against its own count** -- the only
/// request in the block that validates its body's shape -- and on a mismatch
/// it logs "Client sent bad opcode!!!" and treats the count as zero rather
/// than refusing. So a wrong count here degrades into a *correct-looking*
/// shorter answer, which is why the count is derived from the slice and can
/// never disagree with it.
pub fn list_bidder_items_body(auctioneer: u64, offset: u32, outbid: &[u32]) -> Vec<u8> {
    let mut body = Vec::with_capacity(16 + outbid.len() * 4);
    body.extend_from_slice(&auctioneer.to_le_bytes());
    body.extend_from_slice(&offset.to_le_bytes());
    body.extend_from_slice(&(outbid.len() as u32).to_le_bytes());
    for id in outbid {
        body.extend_from_slice(&id.to_le_bytes());
    }
    body
}

/// `CMSG_AUCTION_SELL_ITEM`'s body.
///
/// `items` are `(full item guid, count)` pairs from this character's own
/// inventory -- the *second* of this client's three item-addressing schemes,
/// the one `CMSG_SELL_ITEM` uses. Several may travel and the server merges
/// them into one auction of one stack, so this is not "post several
/// auctions": it is "post one auction out of several stacks".
///
/// A zero guid or a zero count makes the server **abandon the packet in
/// silence**, so both are refused here instead, by returning nothing. Being
/// refused by your own client with a reason beats being ignored by the server
/// without one.
pub fn sell_item_body(
    auctioneer: u64,
    items: &[(u64, u32)],
    bid: u32,
    buyout: u32,
    duration: AuctionDuration,
) -> Option<Vec<u8>> {
    if items.is_empty() || bid == 0 {
        return None;
    }
    if items.iter().any(|&(guid, count)| guid == 0 || count == 0 || count > 1000) {
        return None;
    }
    let mut body = Vec::with_capacity(24 + items.len() * 12);
    body.extend_from_slice(&auctioneer.to_le_bytes());
    body.extend_from_slice(&(items.len() as u32).to_le_bytes());
    for &(guid, count) in items {
        body.extend_from_slice(&guid.to_le_bytes());
        body.extend_from_slice(&count.to_le_bytes());
    }
    body.extend_from_slice(&bid.to_le_bytes());
    body.extend_from_slice(&buyout.to_le_bytes());
    body.extend_from_slice(&duration.minutes().to_le_bytes());
    Some(body)
}

/// `CMSG_AUCTION_PLACE_BID`'s body.
///
/// A price equal to the auction's buyout **is** the buyout -- there is no
/// second opcode. See [`Auction::next_bid`] for the smallest price that will
/// be accepted, and note that a zero id or a zero price is dropped in silence,
/// which is why neither is representable as a successful call.
pub fn place_bid_body(auctioneer: u64, auction: u32, price: u32) -> Option<Vec<u8>> {
    if auction == 0 || price == 0 {
        return None;
    }
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&auctioneer.to_le_bytes());
    body.extend_from_slice(&auction.to_le_bytes());
    body.extend_from_slice(&price.to_le_bytes());
    Some(body)
}

/// `CMSG_AUCTION_REMOVE_ITEM`'s body.
pub fn remove_item_body(auctioneer: u64, auction: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&auctioneer.to_le_bytes());
    body.extend_from_slice(&auction.to_le_bytes());
    body
}

/// How well one candidate record stride explains a captured list result.
///
/// Produced by [`measure_stride`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrideFit {
    pub stride: usize,
    /// Whether the header, `count` records and the footer account for every
    /// byte with none left over.
    pub accounts_for_body: bool,
    /// What lands where the total should be, if anything did.
    pub total: Option<u32>,
    /// What lands where the search delay should be.
    pub delay: Option<u32>,
    /// Whether the total is at least the row count. **The discriminator when
    /// only one packet is available**: a total smaller than the page it came
    /// with is impossible, and a stride out by a word reads it out of the
    /// middle of a money field or a guid, where the number is arbitrary.
    pub total_is_possible: bool,
}

/// Score candidate record strides against a captured list result.
///
/// **Reads the bytes itself rather than calling [`parse_auction_page`]**, for
/// the reason `trainer::measure_stride` and `wow-cli m2 events --strides` do:
/// the parser is what is under test, and a probe built on it confirms only its
/// own author's assumption.
///
/// Byte accounting is weaker here than it looks, and in a way the trainer list
/// was not. There the body ends in a string and any stride that leaves a NUL
/// "accounts for the body"; here everything is fixed-width, so byte accounting
/// alone is exact **for a single count** -- and exactly that is the problem,
/// because it cannot separate the stride from the footer. A stride four bytes
/// larger with a footer four bytes smaller per record fits nothing, but a
/// stride `S` with footer `F` and a stride `S` with footer `F + 4` and one
/// fewer record both do.
///
/// **The measurement that settles it is two packets with different counts.**
/// `len = HEADER + count * stride + FOOTER`, so the difference of two lengths
/// divided by the difference of two counts is the stride, and neither the
/// header nor the footer appears in it. That is what [`stride_between`] does,
/// and it is the number this module's [`RECORD_BYTES`] was set from.
pub fn measure_stride(body: &[u8], candidates: &[usize]) -> Vec<StrideFit> {
    candidates
        .iter()
        .map(|&stride| {
            let mut fit = StrideFit {
                stride,
                accounts_for_body: false,
                total: None,
                delay: None,
                total_is_possible: false,
            };
            if body.len() < HEADER_BYTES {
                return fit;
            }
            let count = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
            let Some(start) = count.checked_mul(stride).map(|n| n + HEADER_BYTES) else {
                return fit;
            };
            if start + FOOTER_BYTES > body.len() {
                return fit;
            }
            fit.accounts_for_body = start + FOOTER_BYTES == body.len();
            let word = |at: usize| {
                u32::from_le_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]])
            };
            let total = word(start);
            fit.total = Some(total);
            fit.delay = Some(word(start + 4));
            fit.total_is_possible = total as usize >= count;
            fit
        })
        .collect()
}

/// The record stride implied by two list results with **different** counts.
///
/// `None` when the counts are equal -- which is the honest answer and the one
/// worth returning rather than a guess. Two packets carrying the same number
/// of rows cannot separate a stride from a header, however many of them there
/// are; this project has produced that tie often enough to make it a named
/// case rather than a caller's problem.
///
/// Neither the header nor the footer appears in the arithmetic, which is the
/// whole point: it measures the stride without assuming either.
pub fn stride_between(a: (usize, u32), b: (usize, u32)) -> Option<usize> {
    let ((short_len, short_count), (long_len, long_count)) = if a.1 < b.1 { (a, b) } else { (b, a) };
    if long_count == short_count || long_len < short_len {
        return None;
    }
    let rows = (long_count - short_count) as usize;
    let bytes = long_len - short_len;
    (bytes % rows == 0).then_some(bytes / rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A `SMSG_AUCTION_LIST_RESULT` exactly as the local realm sent it** to
    /// `Testwolf`, in answer to a search for "Rough Stone": 308 bytes, header
    /// and footer and all.
    ///
    /// Captured rather than constructed, and the distinction is the whole
    /// reason it is here. Every other test in this module builds its body from
    /// what this module believes the layout is, and a parser checked against
    /// its own author's assumption always passes -- which is exactly how the
    /// trainer list shipped for a milestone reading a stride eight bytes
    /// short.
    ///
    /// **Two rows and not one**, because one cannot separate a record stride
    /// from a header, and the two are deliberately different in the ways that
    /// matter: one has a bidder and no buyout, the other a buyout and no
    /// bidder, so both branches of [`Auction::next_bid`] and both of
    /// [`Auction::can_buy_out`] are exercised by real bytes. Their two owners
    /// differ too.
    ///
    /// Every value in it is independently known, because the fixture was
    /// seeded into the realm's own tables and read back by its own loader:
    /// auction 2000 is twenty Rough Stone from guid 1 opening at 1,000 with a
    /// 40,000 buyout; auction 2001 is seven of them from guid **3** with a
    /// 2,500 bid standing from guid **1** and no buyout. The **minimum
    /// increment of 125** is the server's own 5% of that bid and is the one
    /// number here this client did not choose.
    ///
    /// The owner and the bidder being *different* guids on the same record is
    /// deliberate and load-bearing: they are two 8-byte fields twenty-four
    /// bytes apart, and a fixture where they matched could not tell one from
    /// the other. This test asserted them the wrong way round on the first
    /// run, and the packet is what said so.
    const ROUGH_STONE: [u8; 308] = [
        // two rows
        0x02, 0x00, 0x00, 0x00,
        // record 0
        0xd1, 0x07, 0x00, 0x00, 0x13, 0x0b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0xd0, 0x07, 0x00, 0x00, 0x7d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x88, 0x29, 0xa3, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0xc4, 0x09, 0x00, 0x00,
        // record 1
        0xd0, 0x07, 0x00, 0x00, 0x13, 0x0b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0xe8, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x9c, 0x00, 0x00,
        0x08, 0xa2, 0x93, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
        // total, then the search delay: 300ms
        0x02, 0x00, 0x00, 0x00, 0x2c, 0x01, 0x00, 0x00,
    ];

    /// The captured packet, read by the parser, checked against facts the
    /// packet did not supply.
    ///
    /// This is the test that would have caught a wrong stride, a wrong field
    /// order or a swapped pair -- none of which the constructed tests above
    /// can, since they are built from the same belief the parser is.
    #[test]
    fn the_captured_packet_reads_as_the_realm_meant_it() {
        let page = parse_auction_page(&ROUGH_STONE, 0, "SMSG_AUCTION_LIST_RESULT").unwrap();
        assert_eq!(page.auctions.len(), 2);
        assert_eq!(page.total, 2);
        assert_eq!(page.search_delay_ms, 300);
        assert!(page.is_whole());

        // The order is the server's, and the ids are its own -- not row
        // positions, which is why the higher id comes first.
        let bid_on = &page.auctions[0];
        let for_sale = &page.auctions[1];
        assert_eq!(bid_on.id, 2001);
        assert_eq!(for_sale.id, 2000);
        assert_eq!(bid_on.item, 2835);
        assert_eq!(for_sale.item, 2835);

        // A bid standing, no buyout, and a floor that comes from the bid.
        assert_eq!(bid_on.count, 7);
        assert_eq!(bid_on.owner, 3);
        assert_eq!(bid_on.bidder, 1, "a different guid from the owner, on purpose");
        assert!(!bid_on.is_own(1), "guid 1 is the bidder here, not the seller");
        assert!(bid_on.is_own(3));
        assert_eq!(bid_on.start_bid, 2000);
        assert_eq!(bid_on.bid, 2500);
        assert_eq!(bid_on.min_increment, 125, "the server's own 5% of 2500");
        assert_eq!(bid_on.next_bid(), 2625);
        assert!(!bid_on.can_buy_out());

        // No bid, a buyout, and a floor that comes from the opening price.
        assert_eq!(for_sale.count, 20);
        assert_eq!(for_sale.owner, 1);
        assert_eq!(for_sale.bidder, 0);
        assert!(!for_sale.has_bid());
        assert_eq!(for_sale.start_bid, 1000);
        assert_eq!(for_sale.min_increment, 0);
        assert_eq!(for_sale.next_bid(), 1000);
        assert_eq!(for_sale.buyout, 40000);

        // Nothing was enchanted, and all seven slots travelled anyway --
        // which is what makes the record 148 bytes instead of 64.
        assert!(page.auctions.iter().all(|a| a.enchants.iter().all(|e| !e.is_set())));
    }

    /// The stride, measured off the captured packet and a second one whose
    /// count differs -- **the measurement that actually settled it**, and the
    /// one that does not assume the footer's width.
    ///
    /// 112 rows in 16,588 bytes is the owner list from the same session.
    #[test]
    fn the_captured_lengths_give_the_stride_without_assuming_the_footer() {
        assert_eq!(
            stride_between((ROUGH_STONE.len(), 2), (16588, 112)),
            Some(RECORD_BYTES)
        );
        // And the single-packet instrument agrees, which it can only do
        // *given* the footer -- see `measure_stride`.
        let fits = measure_stride(&ROUGH_STONE, &[140, 144, 148, 152]);
        let accounted: Vec<usize> = fits
            .iter()
            .filter(|f| f.accounts_for_body)
            .map(|f| f.stride)
            .collect();
        assert_eq!(accounted, vec![148]);
    }

    /// Build one record the way the server does, so every test below goes
    /// through the parser rather than around it.
    fn record(id: u32, item: u32, count: u32, bid: u32, bidder: u64, buyout: u32) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend_from_slice(&id.to_le_bytes());
        r.extend_from_slice(&item.to_le_bytes());
        for slot in 0..INSPECTED_ENCHANT_SLOTS as u32 {
            // Non-zero in the first slot only, so a stride error that shifts
            // the array shows up as an enchantment appearing somewhere else.
            let enchant: u32 = if slot == 0 { 3324 } else { 0 };
            r.extend_from_slice(&enchant.to_le_bytes());
            r.extend_from_slice(&0u32.to_le_bytes());
            r.extend_from_slice(&0u32.to_le_bytes());
        }
        r.extend_from_slice(&(-37i32).to_le_bytes()); // random property
        r.extend_from_slice(&96u32.to_le_bytes()); // suffix factor
        r.extend_from_slice(&count.to_le_bytes());
        r.extend_from_slice(&0u32.to_le_bytes()); // spell charges
        r.extend_from_slice(&0u32.to_le_bytes()); // flags
        r.extend_from_slice(&0x0000_0000_0000_0001u64.to_le_bytes()); // owner
        r.extend_from_slice(&100u32.to_le_bytes()); // start bid
        r.extend_from_slice(&(if bid == 0 { 0u32 } else { 5u32 }).to_le_bytes()); // increment
        r.extend_from_slice(&buyout.to_le_bytes());
        r.extend_from_slice(&(12 * 60 * 60 * 1000u32).to_le_bytes()); // time left
        r.extend_from_slice(&bidder.to_le_bytes());
        r.extend_from_slice(&bid.to_le_bytes());
        assert_eq!(r.len(), RECORD_BYTES);
        r
    }

    fn page(records: &[Vec<u8>], total: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&(records.len() as u32).to_le_bytes());
        for r in records {
            body.extend_from_slice(r);
        }
        body.extend_from_slice(&total.to_le_bytes());
        body.extend_from_slice(&300u32.to_le_bytes());
        body
    }

    #[test]
    fn record_is_one_hundred_and_forty_eight_bytes() {
        assert_eq!(RECORD_BYTES, 148);
    }

    #[test]
    fn reads_a_page_and_keeps_both_counts_apart() {
        let body = page(
            &[
                record(1, 2589, 20, 0, 0, 500),
                record(2, 4306, 10, 150, 7, 0),
            ],
            1284,
        );
        let page = parse_auction_page(&body, 0, "test").unwrap();
        assert_eq!(page.auctions.len(), 2);
        assert_eq!(page.total, 1284);
        assert_eq!(page.search_delay_ms, 300);
        // The point of the milestone: the page is not the list.
        assert!(!page.is_whole());
        assert_eq!(page.pages(), 26);
        assert_eq!(page.next_offset(), Some(2));
        assert_eq!(page.previous_offset(), None);
    }

    #[test]
    fn a_page_that_holds_everything_says_so() {
        let body = page(&[record(1, 2589, 20, 0, 0, 500)], 1);
        let page = parse_auction_page(&body, 0, "test").unwrap();
        assert!(page.is_whole());
        assert_eq!(page.pages(), 1);
        assert_eq!(page.next_offset(), None);
    }

    /// The two bid floors are different fields and the bug is picking one.
    #[test]
    fn the_next_bid_comes_from_a_different_field_in_each_case() {
        let body = page(
            &[
                record(1, 2589, 20, 0, 0, 500),
                record(2, 4306, 10, 150, 7, 0),
            ],
            2,
        );
        let page = parse_auction_page(&body, 0, "test").unwrap();
        let unbid = &page.auctions[0];
        let contested = &page.auctions[1];
        assert!(!unbid.has_bid());
        assert_eq!(unbid.next_bid(), 100); // the seller's opening price
        assert!(contested.has_bid());
        assert_eq!(contested.next_bid(), 155); // current bid plus increment
        // ...and the wrong rule for each is the other's answer.
        assert_ne!(unbid.next_bid(), unbid.bid + unbid.min_increment);
        assert_ne!(contested.next_bid(), contested.start_bid);
    }

    #[test]
    fn a_buyout_of_zero_is_no_buyout() {
        let body = page(&[record(2, 4306, 10, 150, 7, 0)], 1);
        let page = parse_auction_page(&body, 0, "test").unwrap();
        assert!(!page.auctions[0].can_buy_out());
    }

    #[test]
    fn an_empty_page_is_read_rather_than_refused() {
        let body = page(&[], 0);
        let page = parse_auction_page(&body, 0, "test").unwrap();
        assert!(page.auctions.is_empty());
        assert_eq!(page.total, 0);
        assert_eq!(page.pages(), 0);
        assert_eq!(page.next_offset(), None);
    }

    /// A stride wrong by a word must not parse. The footer is what would
    /// absorb it, so this is the check that the cursor is doing its job.
    #[test]
    fn a_short_body_is_refused_rather_than_read_into_the_footer() {
        let mut body = page(&[record(1, 2589, 20, 0, 0, 500)], 1);
        body.truncate(body.len() - 4);
        assert!(parse_auction_page(&body, 0, "test").is_err());
    }

    #[test]
    fn a_count_the_body_cannot_hold_is_refused_before_the_allocation() {
        let mut body = Vec::new();
        body.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        body.extend_from_slice(&[0u8; 8]);
        let error = parse_auction_page(&body, 0, "test").unwrap_err();
        assert!(matches!(error, Error::AuctionRowCount { .. }));
    }

    #[test]
    fn trailing_bytes_are_an_error() {
        let mut body = page(&[record(1, 2589, 20, 0, 0, 500)], 1);
        body.push(0);
        assert!(parse_auction_page(&body, 0, "test").is_err());
    }

    /// Paging arithmetic on the second page, where an off-by-one in
    /// `previous_offset` or `next_offset` would otherwise pass.
    #[test]
    fn the_middle_of_a_match_pages_both_ways() {
        let records: Vec<_> = (0..PAGE_ROWS)
            .map(|i| record(i as u32 + 100, 2589, 1, 0, 0, 0))
            .collect();
        let body = page(&records, 130);
        let page = parse_auction_page(&body, 50, "test").unwrap();
        assert!(!page.is_whole());
        assert_eq!(page.previous_offset(), Some(0));
        assert_eq!(page.next_offset(), Some(100));
        assert_eq!(page.pages(), 3);
    }

    /// The last page is short, and `pages()` must not be derived from it.
    #[test]
    fn the_last_page_does_not_shorten_the_page_count() {
        let records: Vec<_> = (0..30)
            .map(|i| record(i as u32 + 200, 2589, 1, 0, 0, 0))
            .collect();
        let body = page(&records, 130);
        let page = parse_auction_page(&body, 100, "test").unwrap();
        assert_eq!(page.pages(), 3);
        assert_eq!(page.next_offset(), None);
        assert_eq!(page.previous_offset(), Some(50));
    }

    #[test]
    fn the_stride_falls_out_of_two_different_counts() {
        let one = page(&[record(1, 2589, 1, 0, 0, 0)], 2);
        let two = page(
            &[record(1, 2589, 1, 0, 0, 0), record(2, 2589, 1, 0, 0, 0)],
            2,
        );
        assert_eq!(stride_between((one.len(), 1), (two.len(), 2)), Some(148));
        // Two packets of the same count cannot separate a stride from a
        // header, and saying so is the honest answer.
        assert_eq!(stride_between((one.len(), 1), (one.len(), 1)), None);
    }

    /// The single-packet instrument, and what it can and cannot do.
    #[test]
    fn one_packet_narrows_the_stride_without_settling_it() {
        let body = page(
            &[
                record(1, 2589, 20, 0, 0, 500),
                record(2, 4306, 10, 150, 7, 0),
            ],
            1284,
        );
        let fits = measure_stride(&body, &[144, 148, 152]);
        let right = fits.iter().find(|f| f.stride == 148).unwrap();
        assert!(right.accounts_for_body);
        assert_eq!(right.total, Some(1284));
        assert_eq!(right.delay, Some(300));
        assert!(right.total_is_possible);
        // A stride out by a word does not account for the body -- with a
        // fixed-width record and a fixed footer, byte accounting is exact.
        assert!(fits.iter().filter(|f| f.accounts_for_body).count() == 1);
    }

    #[test]
    fn a_command_result_without_a_fourth_word_is_not_invented() {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes()); // sell
        body.extend_from_slice(&2u32.to_le_bytes()); // an error
        let outcome = parse_command_result(&body).unwrap();
        assert_eq!(outcome.action, AuctionAction::Sell);
        assert!(!outcome.succeeded());
        assert_eq!(outcome.bid_error, None);
    }

    #[test]
    fn a_command_result_with_one_reads_it() {
        let mut body = Vec::new();
        body.extend_from_slice(&17u32.to_le_bytes());
        body.extend_from_slice(&2u32.to_le_bytes()); // bid
        body.extend_from_slice(&0u32.to_le_bytes()); // ok
        body.extend_from_slice(&0u32.to_le_bytes());
        let outcome = parse_command_result(&body).unwrap();
        assert_eq!(outcome.auction, 17);
        assert_eq!(outcome.action, AuctionAction::Bid);
        assert!(outcome.succeeded());
        assert_eq!(outcome.bid_error, Some(0));
    }

    #[test]
    fn an_unknown_action_keeps_its_number() {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&9u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        let outcome = parse_command_result(&body).unwrap();
        assert_eq!(outcome.action, AuctionAction::Other(9));
    }

    /// Every code this project has not observed comes back as a number.
    #[test]
    fn unobserved_error_codes_are_not_given_names() {
        assert_eq!(describe_auction_error(3), "not enough money");
        assert!(describe_auction_error(7).contains('7'));
        assert!(describe_auction_error(13).contains("13"));
    }

    #[test]
    fn hello_reads_the_house() {
        let mut body = Vec::new();
        body.extend_from_slice(&0xF130_0003_8F00_9CA3u64.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        body.push(1);
        let house = parse_auction_hello(&body).unwrap();
        assert_eq!(house.house, 1);
        assert!(house.enabled);
    }

    #[test]
    fn pending_sales_is_empty_and_a_non_empty_one_is_refused() {
        assert_eq!(parse_pending_sales(&0u32.to_le_bytes()).unwrap(), 0);
        let error = parse_pending_sales(&1u32.to_le_bytes()).unwrap_err();
        assert!(matches!(error, Error::UnconfirmedPendingSales { count: 1 }));
    }

    /// The all-filters-off search must not be the all-zeroes struct.
    #[test]
    fn the_default_search_disables_its_filters_with_something_other_than_zero() {
        let search = AuctionSearch::any();
        assert_eq!(search.class, ANY);
        assert_eq!(search.quality, ANY);
        let body = list_items_body(7, 0, &search);
        // guid, offset, the empty name's NUL, two level bytes, four filter
        // words, usable, get_all, and a zero sort count.
        assert_eq!(body.len(), 8 + 4 + 1 + 2 + 16 + 1 + 1 + 1);
        assert_eq!(&body[15..19], &ANY.to_le_bytes());
    }

    #[test]
    fn a_sort_block_travels_as_pairs_after_its_count() {
        let mut search = AuctionSearch::named("linen");
        search.sort = vec![
            SortKey { column: 4, descending: false },
            SortKey { column: 7, descending: true },
        ];
        let body = list_items_body(7, 50, &search);
        assert_eq!(&body[body.len() - 5..], &[2, 4, 0, 7, 1]);
        assert_eq!(&body[8..12], &50u32.to_le_bytes());
        assert_eq!(&body[12..18], b"linen\0");
    }

    #[test]
    fn a_sell_with_nothing_in_it_is_refused_here_rather_than_in_silence() {
        assert!(sell_item_body(7, &[], 100, 0, AuctionDuration::TwelveHours).is_none());
        assert!(sell_item_body(7, &[(9, 0)], 100, 0, AuctionDuration::TwelveHours).is_none());
        assert!(sell_item_body(7, &[(0, 1)], 100, 0, AuctionDuration::TwelveHours).is_none());
        assert!(sell_item_body(7, &[(9, 1)], 0, 0, AuctionDuration::TwelveHours).is_none());
        assert!(sell_item_body(7, &[(9, 1)], 100, 0, AuctionDuration::TwelveHours).is_some());
    }

    #[test]
    fn a_bid_of_nothing_is_refused_here_too() {
        assert!(place_bid_body(7, 0, 100).is_none());
        assert!(place_bid_body(7, 3, 0).is_none());
        assert!(place_bid_body(7, 3, 100).is_some());
    }

    /// The server accepts exactly three durations and drops anything else
    /// without a word, so the enum is the check.
    #[test]
    fn the_three_durations_are_the_minutes_the_server_compares_against() {
        assert_eq!(AuctionDuration::TwelveHours.minutes(), 720);
        assert_eq!(AuctionDuration::TwentyFourHours.minutes(), 1440);
        assert_eq!(AuctionDuration::FortyEightHours.minutes(), 2880);
    }

    #[test]
    fn the_bidder_list_body_cannot_disagree_with_its_own_count() {
        let body = list_bidder_items_body(7, 0, &[11, 22, 33]);
        assert_eq!(body.len(), 16 + 3 * 4);
        assert_eq!(&body[12..16], &3u32.to_le_bytes());
    }

    #[test]
    fn time_bands_cover_the_boundaries() {
        let band = |ms: u32| {
            let mut a = record(1, 1, 1, 0, 0, 0);
            a[132..136].copy_from_slice(&ms.to_le_bytes());
            let body = page(&[a], 1);
            parse_auction_page(&body, 0, "test").unwrap().auctions[0].band()
        };
        assert_eq!(band(0), TimeBand::Short);
        assert_eq!(band(29 * 60 * 1000), TimeBand::Short);
        assert_eq!(band(31 * 60 * 1000), TimeBand::Medium);
        assert_eq!(band(3 * 60 * 60 * 1000), TimeBand::Long);
        assert_eq!(band(20 * 60 * 60 * 1000), TimeBand::VeryLong);
    }
}
