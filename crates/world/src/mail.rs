//! Mail: the first **effect with no request**.
//!
//! Every other packet this client reads is the far end answering something.
//! A vendor list follows a request for it, a trade status follows a trade, a
//! monster move follows a creature this session asked to have replicated. Even
//! the ones that look unprompted -- weather, a mirror timer -- are the server
//! describing a world this character walked into.
//!
//! `SMSG_RECEIVED_MAIL` is not that. It arrives because **somebody else did
//! something**, at a moment this client had nothing outstanding, and there is
//! no send anywhere in this session to correlate it against. That is the
//! novelty of this milestone, and the first thing worth writing down about it
//! is what the packet actually carries.
//!
//! ## The packet that says mail arrived says nothing else
//!
//! It is **four bytes and they are zero**. No sender, no subject, no count, no
//! mail id. The server writes a literal `uint32(0)` and sends it, and the only
//! honest thing a client can draw on receiving one is *an indicator that
//! something is unread*.
//!
//! The obvious follow-up -- ask what it was -- is not available either, and
//! that is the second half of the shape. `CMSG_GET_MAIL_LIST` names a mailbox,
//! and the server's `CanOpenMailBox` refuses anything that is not a mailbox
//! game object or a mailbox NPC the character can reach. So an arrival is
//! knowable anywhere in the world and its *contents* are knowable only within
//! five units of a physical object. A mail interface is therefore two frames
//! rather than one, and no amount of protocol work collapses them.
//!
//! There is exactly one thing between the two: [`parse_next_mail_time`], the
//! reply to `MSG_QUERY_NEXT_MAIL_TIME`, which names senders with no mailbox
//! involved -- and names **at most two of them**, because the server stops
//! after two. That is the original client's "you have new mail from X" and it
//! is a summary by construction, not a list that happened to be short.
//!
//! ## A mailed item is not in the world, which is why the record is fat
//!
//! An item in a bag is a replicated object: the client holds its guid, its
//! fields, its stack count and its durability, and the vendor and trade
//! packets can name it with a `(bag, slot)` pair or a guid and say nothing
//! else. **A mailed item is attached to nothing.** It has been taken out of
//! the sender's inventory and is not in the receiver's, so there is no object
//! anywhere in this client's replicated state to look up and no query that
//! answers for one.
//!
//! So [`MailItem`] carries everything inline -- entry, count, charges,
//! durability, seven enchantment slots -- and it is the reason this record is
//! 118 bytes where a trade slot is 73. The list is self-contained because its
//! contents are outside the world.
//!
//! It also explains the **third way this client names an item**. Inventory
//! moves address `(bag, slot)`; `CMSG_SELL_ITEM` names a full 64-bit guid;
//! [`ClientOpcode::MailTakeItem`](crate::ClientOpcode::MailTakeItem) names a
//! bare **32-bit low guid**, which is what the record carries and the only
//! handle that exists for a thing with no object. Reading it as the low half
//! of a full guid and rebuilding the high half would be inventing a fact.
//!
//! ## Each record announces its own length, and this server's number is wrong
//!
//! Every mail in `SMSG_MAIL_LIST_RESULT` begins with a `u16` size. That is
//! the kind of field this project reaches for -- the redundant slot index that
//! caught a trade stride, the redundant `dir:` lines in `md5translate.trs` --
//! because a length that agrees with the parse is confirmation for free.
//!
//! It does not agree. AzerothCore computes the number from a hand-written
//! expression counting **eight** four-byte fields where the writer writes
//! **seven**, so the announced size is [`RECORD_SIZE_OVERCOUNT`] bytes greater
//! than the record on every mail. That is a fact about this realm's build and
//! it is asserted here rather than assumed, for the reason the whole project
//! keeps rediscovering: a number nobody checks is a number nobody can trust.
//!
//! What matters is what the parser does with it. **It checks the size and
//! never seeks by it.** A parser that trusted the field would land four bytes
//! into the second record on the first mail it read and turn the rest of the
//! packet into plausible garbage; one that ignored it entirely would give up a
//! per-record check and report a stride error as a length at the end of the
//! body. Reading it and comparing keeps the diagnosis where the mistake is.
//!
//! ## Three widths for one field, decided by a byte
//!
//! The sender is a `u64` player guid for an ordinary letter and a `u32` entry
//! for one from an auction house, a creature, a game object or a calendar
//! event -- a conditional layout in the middle of a variable-length record
//! inside a list, which is the worst place for one, because getting it wrong
//! desynchronises everything after it rather than leaving bytes at the end.
//!
//! The server's own switch has **no default arm**: a type it does not know
//! writes no sender at all. This parser does not copy that. An unrecognised
//! type is refused by name ([`Error::MailSenderType`]) because a width nobody
//! has observed is not a width, and guessing zero would produce a record that
//! parses and describes the wrong letter.

use crate::protocol::{Error, Reader};

/// Attachment slots in one letter. The client's own limit, and the reason the
/// item count is a `u8` that never approaches its range.
pub const MAX_MAIL_ITEMS: usize = 12;

/// Enchantment slots written per attached item.
///
/// Seven, and all seven travel whether or not the item can hold that many --
/// the same zero-padded fixed array as the trainer record's three
/// prerequisites. Named because it is the whole difference between a 118-byte
/// record and a 46-byte one.
pub const INSPECTED_ENCHANT_SLOTS: usize = 7;

/// Bytes in one attached-item record.
///
/// Index, low guid, entry, seven enchantment triples, then six words and a
/// trailing byte. **Confirmed by the record's own leading index** the way a
/// trade slot is: twelve consecutive correct indices do not survive a stride
/// that is wrong by a word.
pub const ITEM_BYTES: usize = 1 + 4 + 4 + INSPECTED_ENCHANT_SLOTS * 3 * 4 + 6 * 4 + 1;

/// How much larger a mail record claims to be than it is.
///
/// **Measured on the wire, predicted from the server's source.** The size
/// expression in `MailHandler.cpp` counts `4 * 8` for the block between the
/// sender and the subject, and the writer writes seven such fields: COD, a
/// zero word, the stationery, the money, the check mask, the expiry as a
/// float, and the template id. The eighth does not exist.
///
/// This is *not* used to advance the cursor -- see the module comment. It is
/// the constant a per-record check is stated in terms of, so that a realm
/// writing the field correctly is refused loudly with its body printed rather
/// than misread quietly.
pub const RECORD_SIZE_OVERCOUNT: usize = 4;

/// The smallest a mail record can be: the fixed head, an empty subject and
/// body, no attachments, and the *narrower* of the two sender widths.
///
/// Used only to refuse an impossible count before allocating for it, the same
/// guard `SMSG_LIST_INVENTORY` and `SMSG_TRAINER_LIST` carry. A `u8` count
/// read at the wrong offset asks for hundreds of records out of a body that
/// cannot hold two.
const MIN_RECORD_BYTES: usize = 2 + 4 + 1 + 4 + 7 * 4 + 1 + 1 + 1;

/// Who sent a letter.
///
/// **The variant decides the width of the field it was read from**, which is
/// why this is an enum rather than a number and a tag: a caller holding the
/// two separately can pair a player's 64-bit guid with an auction's tag and
/// nothing stops it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailSender {
    /// An ordinary letter from a character. **Eight bytes**, a full player
    /// guid, which is the handle a name query takes -- so this is the only
    /// sender this client can put a name to without a database.
    Player(u64),
    /// The auction house. Four bytes, the auction's own id.
    Auction(u32),
    /// A creature, by `creature_template` entry. The original client answers
    /// this one by sending `CMSG_CREATURE_QUERY`.
    Creature(u32),
    /// A game object, by entry.
    GameObject(u32),
    /// A calendar event, by id.
    Calendar(u32),
}

impl MailSender {
    /// The raw type byte, for a caller that wants to print it.
    pub fn kind(self) -> u8 {
        match self {
            Self::Player(_) => 0,
            Self::Auction(_) => 2,
            Self::Creature(_) => 3,
            Self::GameObject(_) => 4,
            Self::Calendar(_) => 5,
        }
    }

    /// Bytes this sender occupied in the record.
    pub fn wire_bytes(self) -> usize {
        match self {
            Self::Player(_) => 8,
            _ => 4,
        }
    }

    /// The player guid, where there is one. `None` for every other kind, and
    /// deliberately not a zero: a caller that resolved zero to a name would
    /// draw "the auction house" as an unnamed character.
    pub fn player(self) -> Option<u64> {
        match self {
            Self::Player(guid) => Some(guid),
            _ => None,
        }
    }
}

/// What the check mask says has already happened to a letter.
///
/// The mask travels as a `u32` whose low byte is the whole of it. **Only the
/// bits this project has produced are acted on**; the rest are carried in
/// [`Mail::flags`] and named nowhere, for the standing reason that a wrong
/// name for a flag never errors.
pub mod check {
    /// The reader has opened it. Set by `CMSG_MAIL_MARK_AS_READ`.
    pub const READ: u8 = 0x01;
    /// It came back -- the recipient returned it, or it expired unread. A
    /// returned letter may not be returned again.
    pub const RETURNED: u8 = 0x02;
    /// The body has already been copied into a paper item, so
    /// `CMSG_MAIL_CREATE_TEXT_ITEM` will be refused.
    pub const COPIED: u8 = 0x04;
    /// This letter is the cash from somebody paying a cash-on-delivery.
    pub const COD_PAYMENT: u8 = 0x08;
    /// There is body text worth showing. Set by the sender's handler when the
    /// body is non-empty, which makes it the one flag that is a fact about
    /// the letter rather than about what has been done to it.
    pub const HAS_BODY: u8 = 0x10;
}

/// One enchantment slot on an attached item.
///
/// Three words each, seven of them, zero almost everywhere. Parsed and carried
/// because dropping them would mean seeking past them by a width nothing
/// checks -- the same reason the trade record's gems are read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MailEnchant {
    pub id: u32,
    pub duration: u32,
    pub charges: u32,
}

/// One item attached to a letter.
///
/// Everything here is inline because there is no object to query -- see the
/// module comment. That makes this the only item description in the client
/// that is complete on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailItem {
    /// Its position in the letter, which the server writes into the record.
    /// Redundant as information, load-bearing as evidence: it is the stride
    /// check.
    pub index: u8,
    /// **The 32-bit low guid, and the only handle this item has.** It is what
    /// [`ClientOpcode::MailTakeItem`](crate::ClientOpcode::MailTakeItem)
    /// takes. Not widened to a full guid here, because the high half is not
    /// on the wire and inventing it would be inventing a fact.
    pub guid: u32,
    /// The item entry -- `Item.dbc`'s row, and what gives the icon.
    pub entry: u32,
    pub enchants: [MailEnchant; INSPECTED_ENCHANT_SLOTS],
    /// Signed: the sign is what separates a random suffix from a prefix.
    pub random_property: i32,
    pub suffix_factor: u32,
    /// Stack size.
    pub count: u32,
    /// Charges on a wand or a trinket.
    pub charges: u32,
    pub max_durability: u32,
    pub durability: u32,
}

/// One letter in the inbox.
#[derive(Debug, Clone, PartialEq)]
pub struct Mail {
    /// The server's own id, and the handle every other mail request takes.
    /// **Never a row position** -- the list is filtered (deleted, undelivered
    /// and expired letters are skipped) so positions do not close up, exactly
    /// like a loot slot and a gossip option.
    pub id: u32,
    pub sender: MailSender,
    /// Cash on delivery: taking an attachment costs this much, and the money
    /// goes back to the sender as another letter.
    pub cod: u32,
    /// `Stationery.dbc` -- which envelope the original client drew.
    pub stationery: u32,
    /// Copper enclosed.
    pub money: u32,
    /// The check mask; see [`check`].
    pub flags: u8,
    /// Days until it expires, as a float. Thirty for an ordinary letter,
    /// three when there is a cash-on-delivery on it, ninety when the sender
    /// was a game master -- which is a property worth knowing before reading
    /// a fixture's value as evidence about anything.
    pub days_left: f32,
    /// `MailTemplate.dbc`, for the letters the game writes itself.
    pub template_id: u32,
    pub subject: String,
    pub body: String,
    pub items: Vec<MailItem>,
    /// What the record said its own length was. See [`RECORD_SIZE_OVERCOUNT`]
    /// -- carried rather than dropped, because a probe that prints the two
    /// numbers side by side is what turned this from a guess into a
    /// measurement.
    pub announced_bytes: u16,
    /// What it actually was.
    pub actual_bytes: usize,
}

impl Mail {
    /// Whether the reader has opened it.
    pub fn is_read(&self) -> bool {
        self.flags & check::READ != 0
    }

    /// Whether anything is attached or enclosed.
    pub fn has_anything(&self) -> bool {
        self.money != 0 || !self.items.is_empty()
    }
}

/// The inbox, as the server chose to describe it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inbox {
    /// **How many letters the server counted, which is not how many it
    /// sent.** The list is capped at fifty and again by the packet size, and
    /// the surplus is counted here and nowhere else.
    ///
    /// The distinction is the whole reason this is not `mails.len()`: a
    /// client showing the number of rows it drew tells somebody with sixty
    /// letters that they have fifty, and the ten it silently dropped are the
    /// oldest ones -- the ones about to expire.
    pub total: u32,
    /// The letters that fit.
    pub mails: Vec<Mail>,
}

impl Inbox {
    /// How many the server counted but did not send.
    pub fn withheld(&self) -> u32 {
        self.total.saturating_sub(self.mails.len() as u32)
    }

    /// One letter by its id. **By id, never by position** -- see [`Mail::id`].
    pub fn get(&self, id: u32) -> Option<&Mail> {
        self.mails.iter().find(|mail| mail.id == id)
    }

    /// Letters the reader has not opened.
    pub fn unread(&self) -> impl Iterator<Item = &Mail> {
        self.mails.iter().filter(|mail| !mail.is_read())
    }
}

/// Parses `SMSG_MAIL_LIST_RESULT`.
///
/// The whole body is consumed and the cursor must finish empty, which is what
/// makes a variable-length list of variable-length records checkable at all.
/// Between that and the header sit two local checks that localise a mistake
/// instead of reporting it as a length at the end: each record's announced
/// size (see [`RECORD_SIZE_OVERCOUNT`]) and each attached item's own index.
pub fn parse_inbox(body: &[u8]) -> Result<Inbox, Error> {
    let mut r = Reader::new(body, "SMSG_MAIL_LIST_RESULT");

    let total = r.u32()?;
    let shown = r.u8()?;

    // A count read at the wrong offset asks for records the body cannot hold.
    // Checked before allocating, the same guard the vendor and trainer lists
    // carry.
    let need = shown as usize * MIN_RECORD_BYTES;
    if need > r.remaining() {
        return Err(Error::MailRowCount {
            count: shown,
            expected: need,
            got: r.remaining(),
        });
    }

    let mut mails = Vec::with_capacity(shown as usize);
    for _ in 0..shown {
        mails.push(parse_mail(&mut r)?);
    }

    r.finish()?;
    Ok(Inbox { total, mails })
}

/// One record out of the inbox list.
fn parse_mail(r: &mut Reader<'_>) -> Result<Mail, Error> {
    let began = r.offset();

    let announced_bytes = r.u16()?;
    let id = r.u32()?;
    let kind = r.u8()?;

    // The conditional width. Refused rather than guessed for an unknown type
    // -- see the module comment on why copying the server's defaultless switch
    // would be the wrong shape here.
    let sender = match kind {
        0 => MailSender::Player(r.u64()?),
        2 => MailSender::Auction(r.u32()?),
        3 => MailSender::Creature(r.u32()?),
        4 => MailSender::GameObject(r.u32()?),
        5 => MailSender::Calendar(r.u32()?),
        got => return Err(Error::MailSenderType { got, id }),
    };

    let cod = r.u32()?;
    // Written as a literal zero and commented "probably changed in 3.3.3" in
    // the server's own source. Read rather than skipped, so the field's
    // position is asserted by the parse rather than by a width nothing checks.
    let _unknown = r.u32()?;
    let stationery = r.u32()?;
    let money = r.u32()?;
    // A `u8` mask in a `u32` field. Narrowed here rather than at the call
    // site, so a caller cannot compare it against a bit pattern that is
    // wrong by three bytes of zero.
    let flags = r.u32()? as u8;
    let days_left = r.f32()?;
    let template_id = r.u32()?;
    let subject = r.cstring()?;
    let body = r.cstring()?;

    let item_count = r.u8()?;
    let mut items = Vec::with_capacity(item_count as usize);
    for index in 0..item_count {
        let got = r.u8()?;
        // **The stride check**, and the reason a wrong item width is reported
        // at the item rather than as leftovers at the end of a packet holding
        // six other letters.
        if got != index {
            return Err(Error::MailItemOutOfOrder {
                expected: index,
                got,
            });
        }

        let guid = r.u32()?;
        let entry = r.u32()?;
        let mut enchants = [MailEnchant::default(); INSPECTED_ENCHANT_SLOTS];
        for slot in &mut enchants {
            slot.id = r.u32()?;
            slot.duration = r.u32()?;
            slot.charges = r.u32()?;
        }
        let random_property = r.i32()?;
        let suffix_factor = r.u32()?;
        let count = r.u32()?;
        let charges = r.u32()?;
        let max_durability = r.u32()?;
        let durability = r.u32()?;
        // One trailing byte the server writes as zero and nothing has
        // explained. Consumed, not skipped past by a width.
        let _unknown = r.u8()?;

        items.push(MailItem {
            index: got,
            guid,
            entry,
            enchants,
            random_property,
            suffix_factor,
            count,
            charges,
            max_durability,
            durability,
        });
    }

    let actual_bytes = r.offset() - began;
    if announced_bytes as usize != actual_bytes + RECORD_SIZE_OVERCOUNT {
        return Err(Error::MailRecordSize {
            id,
            announced: announced_bytes,
            actual: actual_bytes,
        });
    }

    Ok(Mail {
        id,
        sender,
        cod,
        stationery,
        money,
        flags,
        days_left,
        template_id,
        subject,
        body,
        items,
        announced_bytes,
        actual_bytes,
    })
}

/// What the server just did with a mail request, and how it went.
///
/// **Every mail request but one is answered by this**, which is the opposite
/// of the trade block and makes mail far cheaper to bound: a send that comes
/// back naming the action it was for has confirmed its own opcode. The
/// exception is `CMSG_MAIL_MARK_AS_READ`, which is silent either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailResult {
    /// Which letter, or zero for a send (there is no id until it exists).
    pub id: u32,
    /// What was attempted. **The server echoes this back**, so it is the
    /// field that ties a reply to a request -- and the reason a probe can
    /// send several things at once and still read the answers apart.
    pub action: MailAction,
    /// Zero is success. Named values only where this project produced them.
    pub result: u32,
    /// The inventory error behind a `result` of 1, where there was one.
    pub equip_error: Option<u32>,
    /// Which item was taken, and how many -- present only on a *successful*
    /// take. See [`parse_mail_result`] on why that is not the same condition
    /// as the action being a take.
    pub taken: Option<(u32, u32)>,
}

/// What a [`MailResult`] is about.
///
/// The server's own numbering, echoed from the request. Unknown values keep
/// their number rather than becoming an error: a reply this client did not ask
/// for is still evidence about what the server is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailAction {
    Send,
    MoneyTaken,
    ItemTaken,
    ReturnedToSender,
    Deleted,
    MadePermanent,
    Other(u32),
}

impl MailAction {
    fn from(code: u32) -> Self {
        match code {
            0 => Self::Send,
            1 => Self::MoneyTaken,
            2 => Self::ItemTaken,
            3 => Self::ReturnedToSender,
            4 => Self::Deleted,
            5 => Self::MadePermanent,
            other => Self::Other(other),
        }
    }
}

/// Success. The one result value named here, for the reason
/// `describe_cast_failure` names exactly one: a wrong name for a status code
/// never errors, it misexplains and sends the next reader somewhere else.
pub const MAIL_OK: u32 = 0;

/// The result that puts an inventory error in the tail. Named because the
/// *layout* depends on it, which makes it a fact about the packet rather than
/// a label on a number.
pub const MAIL_ERR_EQUIP_ERROR: u32 = 1;

/// Parses `SMSG_SEND_MAIL_RESULT`.
///
/// **The tail is conditional on the result first and the action second**, and
/// the order matters more than it looks. A take that succeeded carries the
/// item and the count; a take that failed with an inventory error carries the
/// error *instead*, not as well. A parser that branched on the action first
/// would be correct on every success and wrong on exactly the case it exists
/// to explain -- and it would be wrong silently, reading a four-byte error as
/// an item guid and then running out of body.
///
/// Which is caught here the same way the trade status is: the three shapes are
/// twelve, sixteen and twenty bytes, the cursor must finish empty, and so the
/// body length is independent evidence about the branch.
pub fn parse_mail_result(body: &[u8]) -> Result<MailResult, Error> {
    let mut r = Reader::new(body, "SMSG_SEND_MAIL_RESULT");
    let id = r.u32()?;
    let action = MailAction::from(r.u32()?);
    let result = r.u32()?;

    let mut equip_error = None;
    let mut taken = None;
    if result == MAIL_ERR_EQUIP_ERROR {
        equip_error = Some(r.u32()?);
    } else if action == MailAction::ItemTaken {
        taken = Some((r.u32()?, r.u32()?));
    }

    r.finish()?;
    Ok(MailResult {
        id,
        action,
        result,
        equip_error,
        taken,
    })
}

/// Parses `SMSG_RECEIVED_MAIL`.
///
/// **Four bytes, and they are zero.** The return type is `()` on purpose: the
/// packet's whole content is that it arrived, and returning the word would
/// invite a caller to draw it. What is asserted is the width -- a body of any
/// other length is not this packet, and the arrival is the only thing here
/// worth being sure of.
pub fn parse_received_mail(body: &[u8]) -> Result<(), Error> {
    let mut r = Reader::new(body, "SMSG_RECEIVED_MAIL");
    let _always_zero = r.u32()?;
    r.finish()
}

/// One unread letter, as named by `MSG_QUERY_NEXT_MAIL_TIME`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PendingMail {
    /// The sender's player guid, or zero when the sender is not a player.
    /// **Both fields are always written** -- the guid is zeroed for a
    /// non-player and the entry is zeroed for a player -- so this is a pair of
    /// fields where the record above uses one field of two widths, which is
    /// worth noticing: the same fact is on the wire in two different shapes in
    /// two packets of the same block.
    pub player: u64,
    /// The creature, object or auction entry, or zero for a player.
    pub entry: u32,
    /// The message type, matching [`MailSender::kind`].
    pub kind: u32,
    pub stationery: u32,
    /// Seconds until it can be collected, negative once it can be.
    pub delay: f32,
}

/// What `MSG_QUERY_NEXT_MAIL_TIME` came back with.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NextMailTime {
    /// **A sentinel, not a time**, despite the opcode's name. The server
    /// writes `0.0` when there is unread mail and `-86400.0` -- one day
    /// negative -- when there is none, and writes it before it knows anything
    /// about *when* anything arrives.
    ///
    /// Carried raw and interpreted through [`Self::has_unread`], so nothing
    /// downstream can quietly treat it as a duration.
    pub marker: f32,
    /// At most **two**, and the cap is the server's: it stops after two
    /// distinct senders. An interface drawing this as "your mail" would be
    /// telling somebody with nine letters that they have two.
    pub pending: Vec<PendingMail>,
}

impl NextMailTime {
    /// Whether the server says anything is waiting.
    ///
    /// Read off the sign of [`Self::marker`] rather than off the list being
    /// non-empty, because the two are not the same statement: the list holds
    /// senders and the marker holds the answer, and a reader with unread mail
    /// from a sender the server had already listed gets a positive answer and
    /// an empty list.
    pub fn has_unread(&self) -> bool {
        self.marker >= 0.0
    }
}

/// Parses the server's half of `MSG_QUERY_NEXT_MAIL_TIME`.
///
/// A `MSG_` opcode, so the request and the reply share a number and only the
/// direction separates them -- the same shape as the movement opcodes and
/// `MSG_TABARDVENDOR_ACTIVATE`.
pub fn parse_next_mail_time(body: &[u8]) -> Result<NextMailTime, Error> {
    let mut r = Reader::new(body, "MSG_QUERY_NEXT_MAIL_TIME");
    let marker = r.f32()?;
    let count = r.u32()?;

    // Fixed-width records, so the count and the body length check each other.
    const PENDING_BYTES: usize = 8 + 4 + 4 + 4 + 4;
    let need = count as usize * PENDING_BYTES;
    if need > r.remaining() {
        return Err(Error::MailRowCount {
            // The count here is a `u32`; narrowed for the shared error, which
            // cannot be reached with a real value past 255 -- the server stops
            // at two.
            count: count.min(u8::MAX as u32) as u8,
            expected: need,
            got: r.remaining(),
        });
    }

    let mut pending = Vec::with_capacity(count as usize);
    for _ in 0..count {
        pending.push(PendingMail {
            player: r.u64()?,
            entry: r.u32()?,
            kind: r.u32()?,
            stationery: r.u32()?,
            delay: r.f32()?,
        });
    }

    r.finish()?;
    Ok(NextMailTime { marker, pending })
}

/// Parses `SMSG_SHOW_MAILBOX`: which mailbox the server has just opened.
///
/// Eight bytes, the object's guid. Sent when a mailbox is interacted with
/// through a gossip menu; a game object clicked directly does not produce one,
/// so this is a convenience and never the thing that tells a client a mailbox
/// exists.
pub fn parse_show_mailbox(body: &[u8]) -> Result<u64, Error> {
    let mut r = Reader::new(body, "SMSG_SHOW_MAILBOX");
    let guid = r.u64()?;
    r.finish()?;
    Ok(guid)
}

/// Builds the body of `CMSG_SEND_MAIL`.
///
/// **The one request in this block whose body is not two or three fields**,
/// and the only one this client can get wrong in an interesting way. Written
/// as a function rather than inline at the sender for the reason every
/// two-way structure in this crate is defined once: a body built in the
/// caller has nothing to round-trip against.
///
/// The trailing `u64` and `u8` are constants the server reads and ignores.
/// They are sent because the risk is one-sided -- a handler reading past the
/// end of a short body refuses the packet, and none refuse a body they ignore.
///
/// `items` are **full item guids** from the sender's own inventory, which is
/// the third addressing scheme again: the outgoing side names an item the way
/// `CMSG_SELL_ITEM` does, and the incoming side names it by a bare low guid,
/// because on the way out it is still an object and on the way in it is not.
pub fn send_mail_body(
    mailbox: u64,
    receiver: &str,
    subject: &str,
    text: &str,
    money: u32,
    cod: u32,
    items: &[u64],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(64 + receiver.len() + subject.len() + text.len());
    body.extend_from_slice(&mailbox.to_le_bytes());
    body.extend_from_slice(receiver.as_bytes());
    body.push(0);
    body.extend_from_slice(subject.as_bytes());
    body.push(0);
    body.extend_from_slice(text.as_bytes());
    body.push(0);
    // Stationery, then a word the server reads and never uses.
    body.extend_from_slice(&41u32.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes());
    body.push(items.len() as u8);
    for (index, item) in items.iter().enumerate() {
        body.push(index as u8);
        body.extend_from_slice(&item.to_le_bytes());
    }
    body.extend_from_slice(&money.to_le_bytes());
    body.extend_from_slice(&cod.to_le_bytes());
    body.extend_from_slice(&0u64.to_le_bytes());
    body.push(0);
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one mail record the way the server does, **including the size
    /// field's four-byte overcount**, so every test below goes through the
    /// check rather than around it.
    fn record(id: u32, sender: MailSender, subject: &str, text: &str, items: &[(u32, u32, u32)]) -> Vec<u8> {
        let mut rest = Vec::new();
        rest.extend_from_slice(&id.to_le_bytes());
        rest.push(sender.kind());
        match sender {
            MailSender::Player(guid) => rest.extend_from_slice(&guid.to_le_bytes()),
            MailSender::Auction(e)
            | MailSender::Creature(e)
            | MailSender::GameObject(e)
            | MailSender::Calendar(e) => rest.extend_from_slice(&e.to_le_bytes()),
        }
        for word in [0u32, 0, 41, 1234, check::HAS_BODY as u32] {
            rest.extend_from_slice(&word.to_le_bytes());
        }
        rest.extend_from_slice(&30.0f32.to_le_bytes());
        rest.extend_from_slice(&0u32.to_le_bytes());
        rest.extend_from_slice(subject.as_bytes());
        rest.push(0);
        rest.extend_from_slice(text.as_bytes());
        rest.push(0);
        rest.push(items.len() as u8);
        for (index, (guid, entry, count)) in items.iter().enumerate() {
            rest.push(index as u8);
            rest.extend_from_slice(&guid.to_le_bytes());
            rest.extend_from_slice(&entry.to_le_bytes());
            for _ in 0..INSPECTED_ENCHANT_SLOTS * 3 {
                rest.extend_from_slice(&0u32.to_le_bytes());
            }
            rest.extend_from_slice(&0i32.to_le_bytes());
            rest.extend_from_slice(&0u32.to_le_bytes());
            rest.extend_from_slice(&count.to_le_bytes());
            for _ in 0..3 {
                rest.extend_from_slice(&0u32.to_le_bytes());
            }
            rest.push(0);
        }

        let actual = 2 + rest.len();
        let mut out = ((actual + RECORD_SIZE_OVERCOUNT) as u16).to_le_bytes().to_vec();
        out.extend_from_slice(&rest);
        out
    }

    fn inbox(total: u32, records: &[Vec<u8>]) -> Vec<u8> {
        let mut body = total.to_le_bytes().to_vec();
        body.push(records.len() as u8);
        for r in records {
            body.extend_from_slice(r);
        }
        body
    }

    /// The item record's width is the biggest single number in this module,
    /// so it is asserted before anything is asked to depend on it.
    #[test]
    fn an_attachment_is_a_hundred_and_eighteen_bytes() {
        assert_eq!(ITEM_BYTES, 118);
    }

    /// Two letters whose sender *widths* differ, in one packet. That is the
    /// point: a reading that used one width for both parses the first record
    /// and turns the second into noise, and nothing but the whole-body
    /// accounting says so.
    #[test]
    fn two_sender_widths_in_one_list() {
        let body = inbox(
            2,
            &[
                record(7, MailSender::Player(0x0000_0001_0000_0003), "hello", "text", &[]),
                record(9, MailSender::Auction(4242), "sold", "", &[]),
            ],
        );
        let parsed = parse_inbox(&body).unwrap();
        assert_eq!(parsed.total, 2);
        assert_eq!(parsed.mails.len(), 2);
        assert_eq!(
            parsed.mails[0].sender,
            MailSender::Player(0x0000_0001_0000_0003)
        );
        assert_eq!(parsed.mails[0].subject, "hello");
        assert_eq!(parsed.mails[1].sender, MailSender::Auction(4242));
        assert_eq!(parsed.mails[1].id, 9);
    }

    /// **The odd assertion that documents the server's arithmetic.** A record
    /// whose announced size equals its real one is *refused*, because that is
    /// not what this realm writes -- and a parser silently tolerant of both
    /// would have no per-record check at all.
    #[test]
    fn the_announced_size_is_checked_and_is_four_too_many() {
        let mut body = inbox(1, &[record(1, MailSender::Player(3), "s", "b", &[])]);
        let mail = &parse_inbox(&body).unwrap().mails[0];
        assert_eq!(mail.announced_bytes as usize, mail.actual_bytes + 4);

        // Correct it, and the parser says so rather than shrugging.
        let honest = (body[5] as u16 | (body[6] as u16) << 8) - RECORD_SIZE_OVERCOUNT as u16;
        body[5..7].copy_from_slice(&honest.to_le_bytes());
        assert!(matches!(
            parse_inbox(&body).unwrap_err(),
            Error::MailRecordSize { .. }
        ));
    }

    /// An unknown sender type is refused by name. The alternative -- copying
    /// the server's defaultless switch and consuming nothing -- produces a
    /// record that parses and describes a different letter.
    #[test]
    fn an_unknown_sender_type_is_refused_rather_than_sized_at_zero() {
        let mut body = inbox(1, &[record(1, MailSender::Creature(300), "s", "b", &[])]);
        // The type byte sits after the size and the id.
        body[5 + 2 + 4] = 9;
        assert!(matches!(
            parse_inbox(&body).unwrap_err(),
            Error::MailSenderType { got: 9, .. }
        ));
    }

    /// Attachments keep their own index, and the index is the stride check.
    #[test]
    fn attachments_carry_everything_inline() {
        let body = inbox(
            1,
            &[record(
                5,
                MailSender::Player(1),
                "here",
                "",
                &[(0x1234, 2070, 5), (0x1235, 6948, 1)],
            )],
        );
        let mail = &parse_inbox(&body).unwrap().mails[0];
        assert_eq!(mail.items.len(), 2);
        assert_eq!(mail.items[0].index, 0);
        assert_eq!(mail.items[0].guid, 0x1234);
        assert_eq!(mail.items[0].entry, 2070);
        assert_eq!(mail.items[0].count, 5);
        assert_eq!(mail.items[1].index, 1);
        assert_eq!(mail.items[1].entry, 6948);
    }

    /// The count the server sends is not the count it has, and the surplus is
    /// reachable. A client reading `mails.len()` reports the wrong number to
    /// exactly the people who need the right one.
    #[test]
    fn the_total_is_not_the_number_of_rows() {
        let body = inbox(60, &[record(1, MailSender::Player(1), "s", "", &[])]);
        let parsed = parse_inbox(&body).unwrap();
        assert_eq!(parsed.total, 60);
        assert_eq!(parsed.mails.len(), 1);
        assert_eq!(parsed.withheld(), 59);
    }

    /// The three shapes of a result, and -- the half that matters -- that a
    /// failed *take* carries the equip error and **not** the item pair. A
    /// parser branching on the action first passes every other assertion here.
    #[test]
    fn a_result_tail_branches_on_the_result_before_the_action() {
        let ok = |id: u32, action: u32, result: u32, tail: &[u32]| {
            let mut body = id.to_le_bytes().to_vec();
            body.extend_from_slice(&action.to_le_bytes());
            body.extend_from_slice(&result.to_le_bytes());
            for word in tail {
                body.extend_from_slice(&word.to_le_bytes());
            }
            body
        };

        let sent = parse_mail_result(&ok(0, 0, 0, &[])).unwrap();
        assert_eq!(sent.action, MailAction::Send);
        assert_eq!(sent.result, MAIL_OK);
        assert_eq!(sent.taken, None);

        let took = parse_mail_result(&ok(11, 2, 0, &[0x99, 5])).unwrap();
        assert_eq!(took.action, MailAction::ItemTaken);
        assert_eq!(took.taken, Some((0x99, 5)));
        assert_eq!(took.equip_error, None);

        // A take that failed on inventory space: one word, not two.
        let full = parse_mail_result(&ok(11, 2, MAIL_ERR_EQUIP_ERROR, &[50])).unwrap();
        assert_eq!(full.action, MailAction::ItemTaken);
        assert_eq!(full.equip_error, Some(50));
        assert_eq!(full.taken, None);

        // And the wrong branch does not pass quietly: the two-word tail a
        // take-first parser would expect is refused at this result.
        assert!(parse_mail_result(&ok(11, 2, MAIL_ERR_EQUIP_ERROR, &[50, 0])).is_err());
    }

    /// The leading float is a sentinel and the sign is the whole of it.
    #[test]
    fn the_next_mail_marker_is_a_sign_not_a_duration() {
        let mut none = (-86400.0f32).to_le_bytes().to_vec();
        none.extend_from_slice(&0u32.to_le_bytes());
        let parsed = parse_next_mail_time(&none).unwrap();
        assert!(!parsed.has_unread());
        assert!(parsed.pending.is_empty());

        let mut some = 0.0f32.to_le_bytes().to_vec();
        some.extend_from_slice(&1u32.to_le_bytes());
        some.extend_from_slice(&3u64.to_le_bytes());
        for word in [0u32, 0, 41] {
            some.extend_from_slice(&word.to_le_bytes());
        }
        some.extend_from_slice(&(-1.0f32).to_le_bytes());
        let parsed = parse_next_mail_time(&some).unwrap();
        assert!(parsed.has_unread());
        assert_eq!(parsed.pending.len(), 1);
        assert_eq!(parsed.pending[0].player, 3);
    }

    /// The arrival packet carries nothing, and the width is the only claim
    /// worth asserting about it.
    #[test]
    fn an_arrival_is_four_bytes_of_nothing() {
        assert!(parse_received_mail(&0u32.to_le_bytes()).is_ok());
        assert!(parse_received_mail(&[]).is_err());
        assert!(parse_received_mail(&0u64.to_le_bytes()).is_err());
    }

    /// The outgoing body round-trips against the field order the server reads,
    /// which is the only check available for a request nothing echoes.
    #[test]
    fn the_send_body_puts_the_counts_where_the_handler_reads_them() {
        let body = send_mail_body(0x1122_3344, "Facetest", "hi", "there", 500, 0, &[0x77]);
        let mut r = Reader::new(&body, "CMSG_SEND_MAIL");
        assert_eq!(r.u64().unwrap(), 0x1122_3344);
        assert_eq!(r.cstring().unwrap(), "Facetest");
        assert_eq!(r.cstring().unwrap(), "hi");
        assert_eq!(r.cstring().unwrap(), "there");
        assert_eq!(r.u32().unwrap(), 41);
        assert_eq!(r.u32().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.u8().unwrap(), 0);
        assert_eq!(r.u64().unwrap(), 0x77);
        assert_eq!(r.u32().unwrap(), 500);
        assert_eq!(r.u32().unwrap(), 0);
        assert_eq!(r.u64().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 0);
        r.finish().unwrap();
    }
}
