//! Spells: what this character knows, and asking to cast one.
//!
//! `SMSG_INITIAL_SPELLS` arrives unprompted during the login burst and is the
//! only place the spellbook comes from -- there is no query for it. Miss it and
//! the character appears to know nothing, which looks like a missing feature
//! rather than a dropped packet.
//!
//! Two counted lists back to back, and that is the whole risk here: a wrong
//! width in the first list does not fail, it just consumes the wrong number of
//! bytes and reads the cooldown list from the middle of the spell list. The
//! cursor catches it at the end, which is the only place it can be caught.

use crate::protocol::{Error, Reader};
use crate::update::write_packed_guid;

/// One spell the character knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownSpell {
    pub id: u32,
    /// Zero in every observation so far. Read rather than skipped so its width
    /// is asserted along with everything else.
    pub slot: u16,
}

/// A spell or category that is not ready yet. **Sixteen bytes.**
///
/// **This was read as eight bytes for two milestones, and the reasoning that
/// got it wrong is worth more than the correction.** The width was derived
/// from a single packet: a level-one warrior who had just cast `59752` logged
/// in with a cooldown count of `4` and exactly `32` bytes left, which divides
/// evenly only at 8 -- and the first word decoded to `59752`, which looked
/// like confirmation. Every earlier observation had been an *empty* list,
/// which exercises no entry width at all.
///
/// Both steps are the traps this project has already written down. A count at
/// the front of a packet need not be the length of the thing it counts: the
/// server writes `m_spellCooldowns.size()` and then `continue`s past any entry
/// that is flagged not to be sent or has no `SpellInfo`, so **the count
/// over-reports and dividing by it is not a stride measurement**. And one
/// packet cannot give a stride anyway -- it can only say which candidate
/// accounts for the body, given assumptions about everything else.
///
/// What settled it was a second sample with different numbers, and content
/// rather than arithmetic. `Roguetest` logged in holding Stealth: count `5`,
/// **sixteen** bytes remaining, which the old reading takes as two entries --
/// `(1784, 0)` and `(0, 0)`. **A cooldown entry for spell zero is not a
/// thing.** The 16-byte reading takes the same bytes as one entry for spell
/// 1784 with no cast item, no category and both cooldowns at zero, and that is
/// what the server's own builder writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpellCooldown {
    pub spell_id: u32,
    /// The item that triggered it, or zero for a spell cast directly.
    pub item_id: u16,
    /// Which shared category the cooldown belongs to; zero for none.
    pub category: u16,
    /// Milliseconds left on this spell's own cooldown.
    ///
    /// **The server puts the time in exactly one of this and
    /// [`category_cooldown_ms`](Self::category_cooldown_ms), chosen by whether
    /// the spell has a category** -- a categorised spell reads zero here and
    /// carries its remaining time in the other field. A reader that took this
    /// one alone would report every potion and every categorised ability as
    /// ready. See [`SpellCooldown::remaining_ms`].
    pub cooldown_ms: u32,
    /// Milliseconds left on the shared category, under the same rule.
    pub category_cooldown_ms: u32,
}

/// What the server writes in the category word to mean "no end".
///
/// **Not a duration, and read as one it is 24.8 days.** The builder has a
/// special case for a cooldown running past its infinity horizon and emits a
/// literal `(1, 0x80000000)` pair rather than a time. A toggle uses it: a rogue
/// holding Stealth reports exactly this against spell `1784`, which is the
/// server saying "this is on", not "this is unavailable until September".
pub const INDEFINITE: u32 = 0x8000_0000;

impl SpellCooldown {
    /// Whether this entry is the server's "no end" marker rather than a time.
    pub fn is_indefinite(&self) -> bool {
        self.category_cooldown_ms == INDEFINITE
    }

    /// How long until this spell can be used, whichever field the server chose
    /// to put it in, and `None` when the entry is not a duration at all.
    ///
    /// **An `Option` rather than a number, because both wrong answers here are
    /// silent.** The server puts the time in the spell's own word *or* in its
    /// category's, never both, so a reader taking `cooldown_ms` alone reports
    /// every categorised ability as ready. And taking the larger of the two
    /// without checking [`is_indefinite`](Self::is_indefinite) turns a toggle's
    /// marker into a twenty-four-day cooldown, which draws as a button greyed
    /// out for the rest of the session -- observed against a stealthed rogue
    /// before anything believed it.
    pub fn remaining_ms(&self) -> Option<u32> {
        if self.is_indefinite() {
            return None;
        }
        Some(self.cooldown_ms.max(self.category_cooldown_ms))
    }
}

/// How many bytes one cooldown entry occupies.
const COOLDOWN_RECORD: usize = 16;

/// The spellbook, as it arrives at login.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitialSpells {
    pub spells: Vec<KnownSpell>,
    pub cooldowns: Vec<SpellCooldown>,
    /// How many cooldown entries the packet *said* it held.
    ///
    /// Kept beside the entries rather than discarded because the two routinely
    /// disagree and the disagreement is the server's, not a parse error: the
    /// count is written before a loop that skips entries. Carrying it lets a
    /// caller report the gap instead of a reader silently deciding the packet
    /// was fine -- the same call [`crate::mail`] makes about a record size that
    /// is wrong by a fixed amount.
    pub cooldowns_announced: u16,
}

/// Parses `SMSG_INITIAL_SPELLS`.
pub fn parse_initial_spells(body: &[u8]) -> Result<InitialSpells, Error> {
    let mut r = Reader::new(body, "SMSG_INITIAL_SPELLS");
    // Always zero. Not a count -- reading it as one truncates the spellbook to
    // nothing and looks exactly like a character that knows no spells.
    let _unknown = r.u8()?;

    let spell_count = r.u16()?;
    let mut spells = Vec::with_capacity(spell_count as usize);
    for _ in 0..spell_count {
        spells.push(KnownSpell {
            id: r.u32()?,
            slot: r.u16()?,
        });
    }

    let cooldowns_announced = r.u16()?;
    // **Read by what is there, not by what the count claims.** Looping to the
    // count runs off the end of the packet whenever the server skipped an
    // entry, which fails the whole spellbook -- and losing the spellbook is
    // how this was noticed: the action bar has nothing to draw.
    let present = r.remaining() / COOLDOWN_RECORD;
    if present > cooldowns_announced as usize {
        // The count only ever over-reports, so more entries than announced
        // means the entry width is wrong rather than that the server was
        // generous. That is a parse error and must not be absorbed.
        return Err(Error::Trailing {
            what: "SMSG_INITIAL_SPELLS cooldowns",
            got: r.remaining(),
        });
    }
    let mut cooldowns = Vec::with_capacity(present);
    for _ in 0..present {
        cooldowns.push(SpellCooldown {
            spell_id: r.u32()?,
            item_id: r.u16()?,
            category: r.u16()?,
            cooldown_ms: r.u32()?,
            category_cooldown_ms: r.u32()?,
        });
    }

    // Still exact: whatever is left over is not a whole entry, and a body that
    // ends mid-entry is the evidence of a wrong width.
    r.finish()?;
    Ok(InitialSpells {
        spells,
        cooldowns,
        cooldowns_announced,
    })
}

/// Which of a spell's possible targets the client is supplying.
///
/// Only the three this client can currently mean. The mask has a dozen more
/// bits, each adding its own field to the packet, so naming only what is sent
/// keeps the writer honest about what it can actually express.
pub mod target_flags {
    /// No target: the spell acts on the caster.
    pub const SELF: u32 = 0x0000_0000;
    /// A unit, whose guid follows packed.
    pub const UNIT: u32 = 0x0000_0002;
    /// A game object, whose guid follows packed -- confirmed against
    /// AzerothCore's `SpellInfo.h` (`TARGET_FLAG_GAMEOBJECT`). What
    /// [`foss-wow#141`]'s lock-opening cast needs: a chest is not a unit,
    /// and sending [`UNIT`] at one would claim a shape the server does not
    /// read the same way -- `Spell::EffectOpenLock` reads `gameObjTarget`,
    /// which only a `GameObject`-flagged guid populates.
    pub const GAMEOBJECT: u32 = 0x0000_0800;
}

/// `CMSG_CAST_SPELL`'s body, on yourself or at a unit.
///
/// `target` is `None` for a spell on yourself. A wrong target mask here is the
/// dangerous kind of mistake: the server reads the following bytes according
/// to the mask it was given, so claiming a unit target and not supplying a
/// guid does not fail -- it reads the next fields as one.
pub fn cast_spell(spell_id: u32, target: Option<u64>) -> Vec<u8> {
    let mut body = Vec::new();
    // Distinguishes several casts of the same spell in flight at once. Zero is
    // correct for a client that casts one at a time.
    body.push(0u8);
    body.extend_from_slice(&spell_id.to_le_bytes());
    body.push(0u8); // cast flags
    write_cast_targets(target, &mut body);
    body
}

/// The universal, no-skill "Opening" spell that satisfies `Lock.dbc`'s
/// `LOCKTYPE_OPEN_KNEELING` (13) -- what an ordinary quest pickup object
/// (a chest with `chest.lockId` pointing at a `LOCK_KEY_SKILL` case of that
/// type) needs cast at it, rather than [`crate::opcode::ClientOpcode::GameObjectUse`],
/// which `GameObject::Use()` does nothing at all with for a chest.
///
/// **Not read out of `Spell.dbc` -- found live, because it could not be
/// found any other way.** `Spell.dbc`'s effect-type and effect-misc-value
/// columns are not transcribed anywhere in this project (`Spell` in
/// `crates/dbc/src/schema.rs` names 234 fields down to a handful; those two
/// are not among them), and `CanOpenLock` on the server matches a cast
/// against a lock by comparing exactly those two numbers -- so the only
/// candidates available were fifteen-odd same-named `Spell.dbc` rows titled
/// "Opening", indistinguishable from outside. Tried against `Lock` id 43
/// (`foss-wow#141`'s "Milly's Harvest", `chest.lockId = 43`,
/// `Type[1] = LOCK_KEY_SKILL`, `Index[1] = 13`) on the local AzerothCore
/// realm via `wow-cli --cast 6478 --cast-at-object <guid>`: every other
/// candidate was refused (`SMSG_CAST_FAILED`, mostly `SPELL_FAILED_BAD_TARGETS`);
/// this one was not, and produced `SMSG_SPELL_START` → `SMSG_SPELL_GO` →
/// `SMSG_LOOT_RESPONSE` in order -- a real cast, landing, then loot. The
/// `SMSG_SPELL_START` is also what answers Kake's "there's a pause, it's
/// not instant": this cast has a real wind-up, and the cast bar this client
/// already draws for any caster (see `apps/viewer/src/main.rs`'s cast-bar
/// construction) will show it without needing to know that.
///
/// **Scope, stated rather than silently assumed**: this satisfies exactly
/// one `LockType`, `OPEN_KNEELING`. A chest gated behind an actual skill
/// (`LOCKTYPE_PICKLOCK`, `LOCKTYPE_MINING`, ...) will be refused by this
/// same spell, correctly -- `Lock.dbc` is not read here, so this client
/// cannot yet tell the two kinds of chest apart in advance and only finds
/// out from the refusal.
pub const OPEN_LOCK_KNEELING: u32 = 6478;

/// `CMSG_CAST_SPELL`'s body, aimed at a game object -- opening a lock, per
/// [`target_flags::GAMEOBJECT`].
pub fn cast_spell_at_gameobject(spell_id: u32, guid: u64) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(0u8);
    body.extend_from_slice(&spell_id.to_le_bytes());
    body.push(0u8); // cast flags
    body.extend_from_slice(&target_flags::GAMEOBJECT.to_le_bytes());
    write_packed_guid(guid, &mut body);
    body
}

/// The `SpellCastTargets` block that ends both a cast and an item use.
///
/// **One writer, because two would drift.** `CMSG_USE_ITEM` ends in exactly
/// this structure and the server reads it with the same code it reads a
/// cast's with; a second copy here would be a second place to get the mask
/// wrong, and the failure mode is not a rejection -- the server reads the
/// bytes that follow *according to the mask it was given*, so claiming a
/// unit target without supplying a guid makes it take the next fields as
/// one. Same rule as defining a both-ways structure once.
pub fn write_cast_targets(target: Option<u64>, body: &mut Vec<u8>) {
    match target {
        Some(guid) => {
            body.extend_from_slice(&target_flags::UNIT.to_le_bytes());
            write_packed_guid(guid, body);
        }
        None => body.extend_from_slice(&target_flags::SELF.to_le_bytes()),
    }
}

/// `CMSG_USE_ITEM`'s body: click a thing in a bag and have it do what it does.
///
/// Layout from AzerothCore's `HandleUseItemOpcode` -- for a packet the
/// *client* writes, the authority is the server's **reader**, and that
/// reader is `bagIndex >> slot >> castCount >> spellId >> itemGUID >>
/// glyphIndex >> castFlags` followed by the cast targets.
///
/// Two things in it are worth stating because both are the shape that fails
/// silently rather than loudly:
///
/// - **`item_guid` goes out raw, not packed.** `ObjectGuid`'s stream
///   operator reads a plain `u64`. Getting this backwards is precisely the
///   bug that deleted the player when `SMSG_DESTROY_OBJECT` was read as
///   packed, and here it would shorten the body so everything after it --
///   including the target mask -- is read from the wrong offset.
/// - **`spell_id` is the item's own on-use spell**, from
///   [`crate::query::ItemInfo::use_spell`], and there is nowhere else to get
///   it. The server looks the item up by position *and* checks the spell, so
///   a plausible-looking wrong id is refused rather than acted on.
///
/// `bag` is [`crate::inventory::OWN_SLOT_ARRAY`] for anything in the
/// player's own array, matching what `equip_item` and `swap_item_candidate`
/// already send.
pub fn use_item(
    bag: u8,
    slot: u8,
    item_guid: u64,
    spell_id: u32,
    target: Option<u64>,
) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(bag);
    body.push(slot);
    // Cast count, as in `cast_spell`: zero for a client with one use in
    // flight at a time.
    body.push(0u8);
    body.extend_from_slice(&spell_id.to_le_bytes());
    body.extend_from_slice(&item_guid.to_le_bytes());
    // Glyph index. The server range-checks this before it looks at anything
    // else and refuses the whole request if it is out of range, so a junk
    // value here reads as "no such item".
    body.extend_from_slice(&0u32.to_le_bytes());
    body.push(0u8); // cast flags
    write_cast_targets(target, &mut body);
    body
}

/// One spell now on cooldown, from `SMSG_SPELL_COOLDOWN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpellCooldownEvent {
    pub spell_id: u32,
    pub cooldown_ms: u32,
}

/// Parses `SMSG_SPELL_COOLDOWN`.
///
/// Sent once per cast that actually starts a cooldown, unlike
/// `SMSG_INITIAL_SPELLS`'s login-time cooldown list. This one is a raw,
/// unpacked guid, a flags byte the client has no use for yet, and then
/// `(spellId: u32, cooldown: u32)` pairs with no count: whatever bytes remain
/// after the header are read eight at a time. That is still a cursor
/// invariant this parser can assert -- an odd number of trailing bytes fails
/// the same way a wrong count would elsewhere. The per-entry width matches
/// what `SpellCooldown` turned out to actually be on the wire (eight bytes,
/// not the sixteen public documentation suggested), which is some evidence
/// for this shape without being a direct observation of it.
///
/// **The opcode itself has not been observed.** Two live casts of spell
/// `59752` ("Every Man for Himself") against `wow1.nekos.farm`, one held open
/// six seconds afterward, both started a real cooldown -- confirmed by the
/// next login's `SMSG_INITIAL_SPELLS` carrying it, and by a recast being
/// refused -- but neither capture contained opcode `0x0134` at all, only
/// `SMSG_SPELL_START`/`SMSG_SPELL_GO` and an unrecognised `0x0496` twice.
/// Either this realm does not send `SMSG_SPELL_COOLDOWN` for this ability, or
/// the opcode number itself is wrong. This parser and its fold into
/// `WorldState` are therefore exercised only by unit tests today; the next
/// person chasing a cooldown sweep that never appears should start here, not
/// assume the sweep math is at fault.
pub fn parse_spell_cooldown(body: &[u8]) -> Result<(u64, Vec<SpellCooldownEvent>), Error> {
    let mut r = Reader::new(body, "SMSG_SPELL_COOLDOWN");
    let caster = r.u64()?;
    let _flags = r.u8()?;

    let mut cooldowns = Vec::new();
    while r.remaining() > 0 {
        cooldowns.push(SpellCooldownEvent {
            spell_id: r.u32()?,
            cooldown_ms: r.u32()?,
        });
    }
    r.finish()?;
    Ok((caster, cooldowns))
}

/// The header shared by `SMSG_SPELL_START` and `SMSG_SPELL_GO`: two packed
/// guids, a cast count, and the spell id -- identical byte-for-byte in a
/// live capture of the two packets from the same cast (see both parsers'
/// pinned regression tests). Reading it once keeps the two parsers from
/// silently drifting apart the way two independently-written copies of a
/// shared prefix tend to.
fn read_cast_header(r: &mut Reader) -> Result<(u64, u64, u8, u32), Error> {
    let cast_item = crate::update::read_packed_guid(r)?;
    let caster = crate::update::read_packed_guid(r)?;
    let cast_count = r.u8()?;
    let spell_id = r.u32()?;
    Ok((cast_item, caster, cast_count, spell_id))
}

/// The target-flags-then-guid-then-optional-trailing-u32 tail shared by both
/// packets, past whatever each puts before it. Refuses anything other than
/// `target_flags::UNIT` or `target_flags::GAMEOBJECT` -- see [`SpellStart`]'s
/// doc comment for why.
///
/// **`GAMEOBJECT` confirmed the same way `UNIT` was, live -- and the trailing
/// `u32` turned out not to be unconditional.** `foss-wow#141`'s lock-opening
/// cast (`crate::spell::OPEN_LOCK_KNEELING` at a chest) echoes back the same
/// flags-then-packed-guid shape on both `SMSG_SPELL_START` and
/// `SMSG_SPELL_GO` against the local AzerothCore realm, but the packet ends
/// there -- no trailing word at all, where the one `UNIT`-targeted capture on
/// hand always had four bytes left. [`SpellStart::trailing`]'s own doc
/// comment already guessed this might be a mana cost gated on *something*;
/// the two live shapes now on hand say that something is at least partly the
/// target kind, not only `cast_flags` -- a cost to spend mana against makes
/// sense for a unit and not for a chest. So this reads it only when the
/// packet actually has it left, rather than assuming a fixed width the
/// gameobject shape does not have.
fn read_unit_target_tail(r: &mut Reader, what: &'static str) -> Result<(u64, Option<u32>), Error> {
    let target_flags = r.u32()?;
    if target_flags != target_flags::UNIT && target_flags != target_flags::GAMEOBJECT {
        return Err(Error::UnsupportedSpellTarget {
            what,
            flags: target_flags,
        });
    }
    let target = crate::update::read_packed_guid(r)?;
    let trailing = if r.remaining() > 0 {
        Some(r.u32()?)
    } else {
        None
    };
    Ok((target, trailing))
}

/// One cast beginning to wind up, from `SMSG_SPELL_START`.
///
/// **Confirmed against exactly one live packet, and refuses to guess past
/// what that packet actually showed.** `wow-cli world --cast 5185
/// --cast-self` against `wow1.nekos.farm` (Healing Touch, cast by
/// `Testdruid`, guid `0x33`) produced a 27-byte body this decodes with
/// nothing left over -- see `a_spell_start_matches_a_captured_live_packet`.
/// The spell id reads `5185` and the cast time reads `1500` ms, both exactly
/// where they should be for that cast.
///
/// **Even `--cast-self` did not send `target_flags::SELF`.** The capture
/// carries an explicit `target_flags::UNIT` naming the caster's own guid as
/// the target. No capture of an actual `SELF` target, an item target, or a
/// positional target (ground-targeted AOE spells use those) exists, so this
/// parser errors on any target flags other than `UNIT` rather than reading
/// whatever would follow a shape nobody has seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpellStart {
    /// The first of the header's two packed guids. Equal to `caster` in the
    /// one capture this is confirmed against -- a spell not cast from an
    /// item -- so which of the two is really an item guid is not
    /// independently distinguished yet.
    pub cast_item: u64,
    pub caster: u64,
    pub cast_count: u8,
    pub spell_id: u32,
    /// Opaque. `0x0802` in the one capture on hand; no bit has been isolated.
    pub cast_flags: u32,
    pub cast_time_ms: u32,
    /// Who the cast is aimed at -- see the struct's own doc comment for why
    /// this is unconditionally a unit rather than an `Option`.
    pub target: u64,
    /// A u32 that followed the target in the one `UNIT`-targeted capture on
    /// hand: `100`, suspiciously close to a mana cost, but not confirmed to
    /// be one. **Confirmed absent, not merely zero, for a `GAMEOBJECT`
    /// target** -- `foss-wow#141`'s live chest cast ends right after the
    /// guid, which is what an `Option` here is for for rather than a `0`
    /// that would claim the field was present and happened to read zero.
    pub trailing: Option<u32>,
}

/// Parses `SMSG_SPELL_START`. See [`SpellStart`] for what is and is not
/// confirmed about this shape.
pub fn parse_spell_start(body: &[u8]) -> Result<SpellStart, Error> {
    let mut r = Reader::new(body, "SMSG_SPELL_START");
    let (cast_item, caster, cast_count, spell_id) = read_cast_header(&mut r)?;
    let cast_flags = r.u32()?;
    let cast_time_ms = r.u32()?;
    let (target, trailing) = read_unit_target_tail(&mut r, "SMSG_SPELL_START")?;
    r.finish()?;
    Ok(SpellStart {
        cast_item,
        caster,
        cast_count,
        spell_id,
        cast_flags,
        cast_time_ms,
        target,
        trailing,
    })
}

/// One cast landing, from `SMSG_SPELL_GO`.
///
/// Confirmed against the `SMSG_SPELL_GO` that arrived alongside the
/// `SMSG_SPELL_START` capture documented on [`SpellStart`] -- same cast, same
/// wire dump. The header is byte-for-byte identical to that packet's, which
/// is why [`parse_spell_go`] shares [`read_cast_header`] rather than
/// re-deriving it. Past the header this packet carries no cast time; instead
/// it carries a hit list, a miss list, and a server timestamp, then the same
/// target tail as `SMSG_SPELL_START`, refused on the same terms for anything
/// but `target_flags::UNIT` or `target_flags::GAMEOBJECT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpellGo {
    pub cast_item: u64,
    pub caster: u64,
    pub cast_count: u8,
    pub spell_id: u32,
    /// Opaque, and not the same bit pattern as `SpellStart::cast_flags` in
    /// the one pair of captures on hand (`0x0900` here against `0x0802`
    /// there).
    pub cast_flags: u32,
    /// The server's own millisecond clock, not wall time -- a monotonic
    /// counter with no shared origin with anything this client keeps.
    pub timestamp: u32,
    /// Guids the cast actually hit. One entry in the capture -- the caster's
    /// own guid, for a self-heal -- each a raw unpacked guid rather than the
    /// packed form used everywhere else in this packet, which the capture
    /// shows clearly: eight bytes reading `33 00 00 00 00 00 00 00`, not a
    /// packed mask.
    pub hits: Vec<u64>,
    /// Who the cast is aimed at, and the same trailing u32 as `SpellStart` --
    /// see [`SpellStart::trailing`]'s doc comment for why it is an `Option`.
    pub target: u64,
    pub trailing: Option<u32>,
}

/// Parses `SMSG_SPELL_GO`. See [`SpellGo`] for what is and is not confirmed
/// about this shape.
///
/// **Refuses any miss entries.** The capture this is confirmed against had a
/// miss count of zero, so a miss entry's own shape -- believed elsewhere to
/// carry at least a reason byte -- has never actually been seen on the wire.
/// Reading a nonzero miss count as if it were more raw guids would be
/// exactly the kind of confident, unverified layout this project's own rules
/// warn against, so a nonzero count is an error instead.
pub fn parse_spell_go(body: &[u8]) -> Result<SpellGo, Error> {
    let mut r = Reader::new(body, "SMSG_SPELL_GO");
    let (cast_item, caster, cast_count, spell_id) = read_cast_header(&mut r)?;
    let cast_flags = r.u32()?;
    let timestamp = r.u32()?;

    let hit_count = r.u8()?;
    let mut hits = Vec::with_capacity(hit_count as usize);
    for _ in 0..hit_count {
        hits.push(r.u64()?);
    }

    let miss_count = r.u8()?;
    if miss_count != 0 {
        return Err(Error::UnconfirmedSpellMisses { count: miss_count });
    }

    let (target, trailing) = read_unit_target_tail(&mut r, "SMSG_SPELL_GO")?;
    r.finish()?;
    Ok(SpellGo {
        cast_item,
        caster,
        cast_count,
        spell_id,
        cast_flags,
        timestamp,
        hits,
        target,
        trailing,
    })
}

/// What `SMSG_CAST_FAILED` said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CastFailed {
    pub cast_count: u8,
    pub spell_id: u32,
    pub reason: u8,
}

/// Parses `SMSG_CAST_FAILED`.
///
/// Deliberately **does not** call `finish`. Some reasons carry extra data
/// whose shape depends on the reason -- a required reagent, a missing skill --
/// and this client does not interpret those yet. Refusing the packet because
/// of trailing bytes would turn "you are out of range" into a decode error,
/// which is worse than ignoring a detail. Every other parser in this crate
/// asserts full consumption; this one says why it cannot.
pub fn parse_cast_failed(body: &[u8]) -> Result<CastFailed, Error> {
    let mut r = Reader::new(body, "SMSG_CAST_FAILED");
    Ok(CastFailed {
        cast_count: r.u8()?,
        spell_id: r.u32()?,
        reason: r.u8()?,
    })
}

/// A description of a cast failure.
///
/// **Deliberately almost empty**, and that is the interesting part.
///
/// The obvious thing to write here is the whole `SpellCastResult` table from
/// memory or from a wiki page. This milestone had just finished paying for
/// exactly that: `CHAT_MSG_SAY` was assumed to be `0x00` when `0x00` is
/// `CHAT_MSG_SYSTEM`, and the resulting silence cost three rounds of debugging.
/// A cast-failure table is the same hazard with a worse failure mode -- a wrong
/// name here does not error, it *confidently misexplains* why something did not
/// work, and sends the next person looking in the wrong place.
///
/// So reasons are named only once observed against a live realm, one at a time.
/// Everything else keeps its number, which is honest and still actionable.
pub fn describe_cast_failure(reason: u8) -> String {
    match reason {
        // Observed: a self-targeted Heroic Strike (a melee attack that
        // requires an enemy) against a live 3.3.5a realm. The name is this
        // codebase's own description of the observation, not a claim about
        // what the enum constant is called.
        0x0C => "the spell cannot be cast on that target".to_string(),
        other => format!("reason {other:#04x}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spellbook(spells: &[u32], cooldowns: &[u32]) -> Vec<u8> {
        let mut body = vec![0u8];
        body.extend((spells.len() as u16).to_le_bytes());
        for id in spells {
            body.extend(id.to_le_bytes());
            body.extend(0u16.to_le_bytes());
        }
        body.extend((cooldowns.len() as u16).to_le_bytes());
        for id in cooldowns {
            body.extend(id.to_le_bytes());
            body.extend(0u16.to_le_bytes()); // cast item
            body.extend(0u16.to_le_bytes()); // category
            body.extend(1500u32.to_le_bytes()); // the spell's own cooldown
            body.extend(0u32.to_le_bytes()); // the category's
        }
        body
    }

    #[test]
    fn a_spellbook_parses() {
        let parsed = parse_initial_spells(&spellbook(&[6603, 78, 2457], &[78])).unwrap();
        assert_eq!(
            parsed.spells.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![6603, 78, 2457]
        );
        assert_eq!(parsed.cooldowns.len(), 1);
        assert_eq!(parsed.cooldowns[0].cooldown_ms, 1500);
        assert_eq!(parsed.cooldowns[0].remaining_ms(), Some(1500));
    }

    /// The server's "no end" marker is not a twenty-four-day cooldown.
    ///
    /// Taken verbatim from the entry a stealthed `Roguetest` logs in with:
    /// spell 1784, own cooldown `1`, category cooldown `0x80000000`. Read as a
    /// duration that is 2,147,483,648ms, and the Stealth button would be greyed
    /// out for the rest of the session -- which reads as the toggle being
    /// broken rather than as a misparsed field.
    #[test]
    fn an_indefinite_cooldown_is_a_marker_and_not_a_duration() {
        let mut body = vec![0u8];
        body.extend(0u16.to_le_bytes());
        body.extend(1u16.to_le_bytes());
        body.extend(1784u32.to_le_bytes());
        body.extend(0u16.to_le_bytes()); // cast item
        body.extend(0u16.to_le_bytes()); // category
        body.extend(1u32.to_le_bytes()); // the literal 1 the builder writes
        body.extend(INDEFINITE.to_le_bytes());

        let parsed = parse_initial_spells(&body).unwrap();
        let entry = parsed.cooldowns[0];
        assert_eq!(entry.spell_id, 1784);
        assert!(entry.is_indefinite());
        assert_eq!(
            entry.remaining_ms(),
            None,
            "the marker was read as a duration"
        );
    }

    /// A categorised spell carries its time in the *other* word, and
    /// `remaining_ms` has to find it there.
    ///
    /// **The half a reader taking `cooldown_ms` alone would get wrong**, and
    /// it would get it wrong silently: every potion and every categorised
    /// ability would report as ready, which draws as an action bar with no
    /// sweep on it rather than as an error.
    #[test]
    fn a_categorised_cooldown_carries_its_time_in_the_category_word() {
        let mut body = vec![0u8];
        body.extend(0u16.to_le_bytes());
        body.extend(1u16.to_le_bytes());
        body.extend(6603u32.to_le_bytes());
        body.extend(0u16.to_le_bytes()); // cast item
        body.extend(11u16.to_le_bytes()); // a category
        body.extend(0u32.to_le_bytes()); // the spell's own: zero, as the server writes it
        body.extend(9000u32.to_le_bytes()); // the category's

        let parsed = parse_initial_spells(&body).unwrap();
        assert_eq!(parsed.cooldowns[0].cooldown_ms, 0);
        assert_eq!(parsed.cooldowns[0].category, 11);
        assert_eq!(parsed.cooldowns[0].remaining_ms(), Some(9000));
    }

    #[test]
    fn an_empty_spellbook_parses() {
        let parsed = parse_initial_spells(&spellbook(&[], &[])).unwrap();
        assert!(parsed.spells.is_empty());
        assert!(parsed.cooldowns.is_empty());
    }

    /// **The same captured bytes that were once read as eight-byte entries,
    /// now read as sixteen -- and they are their own refutation.**
    ///
    /// Captured from `wow1.nekos.farm` after casting `59752`, "Every Man for
    /// Himself", and kept verbatim through the correction because that is what
    /// makes the correction checkable. Count `4`, thirty-two bytes.
    ///
    /// Read at 8 bytes it gives four entries: `59752`, **`0`**, `72752`,
    /// **`0`**. A cooldown entry for spell zero is not a thing the server
    /// writes, and two of the four were exactly that -- the padding of the
    /// wider record, read as records. At 16 it gives two entries, both real
    /// spells, with a count that over-reports by two because the server writes
    /// the map's size and then skips entries.
    ///
    /// Asserted as *two* entries with the announced count kept beside them, so
    /// a revert to the narrow reading fails on the count of entries rather
    /// than on a value nobody would look at.
    #[test]
    fn the_cooldown_list_matches_a_captured_live_packet() {
        let mut body = vec![0u8]; // unknown leading byte
        body.extend(0u16.to_le_bytes()); // no known spells, to keep this focused
        body.extend(4u16.to_le_bytes()); // cooldown_count, as captured
        body.extend([
            0x68, 0xe9, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // spell 59752
            0x30, 0x1c, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, //
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // spell 72752
        ]);

        let parsed = parse_initial_spells(&body).unwrap();
        assert_eq!(
            parsed.cooldowns.iter().map(|c| c.spell_id).collect::<Vec<_>>(),
            vec![59752, 72752],
            "no entry may name spell zero"
        );
        assert_eq!(
            parsed.cooldowns_announced, 4,
            "the count over-reports and the packet is not wrong for it"
        );
    }

    /// The second sample, and the one that could not be explained away.
    ///
    /// `Roguetest` logging in holding Stealth: count `5`, **sixteen** bytes.
    /// The narrow reading takes that as two entries, the second of which names
    /// spell zero; and unlike the capture above, sixteen bytes cannot be
    /// divided by the announced count at all, under either reading. **When a
    /// population cannot separate two candidates, change the population** --
    /// this is the sample that did.
    #[test]
    fn a_count_of_five_arrives_with_one_entry() {
        let mut body = vec![0u8];
        body.extend(0u16.to_le_bytes());
        body.extend(5u16.to_le_bytes());
        body.extend([
            0xf8, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // spell 1784, Stealth
        ]);

        let parsed = parse_initial_spells(&body).unwrap();
        assert_eq!(parsed.cooldowns.len(), 1);
        assert_eq!(parsed.cooldowns[0].spell_id, 1784);
        assert_eq!(parsed.cooldowns_announced, 5);
    }

    /// More entries than the count announced means the width is wrong, and
    /// that is refused rather than absorbed.
    ///
    /// The count only ever over-reports -- the server writes the map's size
    /// and then skips -- so the other direction cannot happen legitimately.
    /// Without this check a halved entry width would read every packet as
    /// "twice as many cooldowns, all fine".
    #[test]
    fn more_entries_than_announced_is_refused() {
        let mut body = vec![0u8];
        body.extend(0u16.to_le_bytes());
        body.extend(1u16.to_le_bytes()); // one announced
        body.extend([0u8; 32]); // two entries' worth
        assert!(parse_initial_spells(&body).is_err());
    }

    /// Two counted lists back to back is the whole risk: a wrong width in the
    /// first does not fail, it reads the second from the middle of the first.
    /// Only the cursor running out -- or having bytes left -- says so.
    #[test]
    fn a_wrong_width_shows_up_as_leftovers() {
        let body = spellbook(&[6603, 78], &[]);

        let mut extra = body.clone();
        extra.push(0);
        assert!(parse_initial_spells(&extra).is_err());

        for cut in 1..body.len() {
            assert!(
                parse_initial_spells(&body[..cut]).is_err(),
                "{cut} bytes parsed as a whole spellbook"
            );
        }
    }

    /// A count that promises more than the packet holds must fail rather than
    /// allocate what it was told to.
    #[test]
    fn an_overstated_count_is_rejected() {
        let mut body = vec![0u8];
        body.extend(60_000u16.to_le_bytes());
        assert!(parse_initial_spells(&body).is_err());
    }

    fn cooldown_body(caster: u64, flags: u8, entries: &[(u32, u32)]) -> Vec<u8> {
        let mut body = caster.to_le_bytes().to_vec();
        body.push(flags);
        for (spell, cooldown) in entries {
            body.extend(spell.to_le_bytes());
            body.extend(cooldown.to_le_bytes());
        }
        body
    }

    #[test]
    fn a_spell_cooldown_parses_one_entry() {
        let body = cooldown_body(0x0000_0000_0000_0032, 0, &[(78, 1500)]);
        let (caster, cooldowns) = parse_spell_cooldown(&body).unwrap();
        assert_eq!(caster, 0x32);
        assert_eq!(cooldowns.len(), 1);
        assert_eq!(cooldowns[0].spell_id, 78);
        assert_eq!(cooldowns[0].cooldown_ms, 1500);
    }

    #[test]
    fn a_spell_cooldown_parses_several_entries_with_no_count_field() {
        let body = cooldown_body(0x32, 1, &[(78, 1500), (172, 6000), (2457, 0)]);
        let (_, cooldowns) = parse_spell_cooldown(&body).unwrap();
        assert_eq!(
            cooldowns.iter().map(|c| (c.spell_id, c.cooldown_ms)).collect::<Vec<_>>(),
            vec![(78, 1500), (172, 6000), (2457, 0)]
        );
    }

    #[test]
    fn an_empty_spell_cooldown_parses_to_no_entries() {
        let body = cooldown_body(0x32, 0, &[]);
        let (caster, cooldowns) = parse_spell_cooldown(&body).unwrap();
        assert_eq!(caster, 0x32);
        assert!(cooldowns.is_empty());
    }

    /// There is no count field here, so a trailing partial entry is the only
    /// evidence of a wrong width -- the same cursor discipline as every other
    /// parser in this crate, just without a count to get wrong in the first
    /// place.
    #[test]
    fn a_partial_trailing_entry_is_rejected() {
        let mut body = cooldown_body(0x32, 0, &[(78, 1500)]);
        body.push(0xFF); // one stray byte, not a whole entry
        assert!(parse_spell_cooldown(&body).is_err());
    }

    #[test]
    fn a_truncated_header_is_rejected() {
        for cut in 0..9 {
            let body = cooldown_body(0x32, 0, &[(78, 1500)]);
            assert!(
                parse_spell_cooldown(&body[..cut]).is_err(),
                "{cut} bytes parsed as a whole header"
            );
        }
    }

    /// Pinned to the real `SMSG_SPELL_START` captured from `wow1.nekos.farm`
    /// (`wow-cli world --cast 5185 --cast-self`, Healing Touch cast by
    /// `Testdruid`, guid `0x33`). Every field here is exactly what that
    /// capture held; see `SpellStart`'s doc comment for what is and is not
    /// confirmed about the shape.
    #[test]
    fn a_spell_start_matches_a_captured_live_packet() {
        let body: [u8; 27] = [
            0x01, 0x33, 0x01, 0x33, 0x00, 0x41, 0x14, 0x00, 0x00, 0x02, 0x08, 0x00, 0x00, 0xdc,
            0x05, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x33, 0x64, 0x00, 0x00, 0x00,
        ];
        let parsed = parse_spell_start(&body).unwrap();
        assert_eq!(parsed.cast_item, 0x33);
        assert_eq!(parsed.caster, 0x33);
        assert_eq!(parsed.cast_count, 0);
        assert_eq!(parsed.spell_id, 5185);
        assert_eq!(parsed.cast_flags, 0x0802);
        assert_eq!(parsed.cast_time_ms, 1500);
        assert_eq!(parsed.target, 0x33);
        assert_eq!(parsed.trailing, Some(100));
    }

    /// **`foss-wow#141`, confirmed live against the local realm.** Casting
    /// [`OPEN_LOCK_KNEELING`] at a chest echoes `target_flags::GAMEOBJECT`
    /// (`0x800`) back with the same flags-then-packed-guid shape -- but the
    /// packet ends right there, four bytes shorter than the `UNIT`-targeted
    /// capture above. Before this, `read_unit_target_tail` refused the flag
    /// outright: the loot still worked (a separate code path), but nothing
    /// populated `self.casts`, so the cast bar this client already draws for
    /// any caster silently never appeared. Built by editing the
    /// captured-unit-cast fixture above at the flags word and truncating the
    /// trailing four bytes it does not have, rather than from a second
    /// capture, since the two packets otherwise share one shape.
    #[test]
    fn a_spell_start_at_a_gameobject_parses_with_no_trailing_word() {
        let body: [u8; 23] = [
            0x01, 0x33, 0x01, 0x33, 0x00, 0x41, 0x14, 0x00, 0x00, 0x02, 0x08, 0x00, 0x00, 0xdc,
            0x05, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x01, 0x33,
        ];
        let parsed = parse_spell_start(&body).unwrap();
        assert_eq!(parsed.target, 0x33);
        assert_eq!(parsed.trailing, None);
    }

    /// Pinned to the `SMSG_SPELL_GO` captured in the same batch as the
    /// `SMSG_SPELL_START` above -- same cast, same wire dump.
    #[test]
    fn a_spell_go_matches_a_captured_live_packet() {
        let body: [u8; 37] = [
            0x01, 0x33, 0x01, 0x33, 0x00, 0x41, 0x14, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x74,
            0xd4, 0x37, 0x5a, 0x01, 0x33, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
            0x00, 0x00, 0x00, 0x01, 0x33, 0x51, 0x00, 0x00, 0x00,
        ];
        let parsed = parse_spell_go(&body).unwrap();
        assert_eq!(parsed.cast_item, 0x33);
        assert_eq!(parsed.caster, 0x33);
        assert_eq!(parsed.cast_count, 0);
        assert_eq!(parsed.spell_id, 5185);
        assert_eq!(parsed.cast_flags, 0x0900);
        assert_eq!(parsed.timestamp, 0x5a37d474);
        assert_eq!(parsed.hits, vec![0x33]);
        assert_eq!(parsed.target, 0x33);
        assert_eq!(parsed.trailing, Some(0x51));
    }

    /// Both parsers assert full consumption, the same cursor discipline as
    /// every other parser in this crate: a wrong field width here reads the
    /// next field from the middle of this one, and only a leftover or missing
    /// byte says so.
    #[test]
    fn a_wrong_width_in_spell_start_or_go_shows_up_as_leftovers_or_truncation() {
        let start: [u8; 27] = [
            0x01, 0x33, 0x01, 0x33, 0x00, 0x41, 0x14, 0x00, 0x00, 0x02, 0x08, 0x00, 0x00, 0xdc,
            0x05, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x33, 0x64, 0x00, 0x00, 0x00,
        ];
        // Exactly one shorter length is not an error any more: 23 bytes ends
        // right after the packed guid, which `foss-wow#141` confirmed live
        // is the whole `GAMEOBJECT`-targeted packet -- see
        // `a_spell_start_at_a_gameobject_parses_with_no_trailing_word`. Every
        // *other* length still has to fail: a cursor invariant that only
        // held for one specific width was never the real invariant.
        const NO_TRAILING_WORD: usize = 23;
        let mut extra = start.to_vec();
        extra.push(0);
        assert!(parse_spell_start(&extra).is_err());
        for cut in 0..start.len() {
            if cut == NO_TRAILING_WORD {
                assert_eq!(parse_spell_start(&start[..cut]).unwrap().trailing, None);
                continue;
            }
            assert!(
                parse_spell_start(&start[..cut]).is_err(),
                "{cut} bytes parsed as a whole SMSG_SPELL_START"
            );
        }

        let go: [u8; 37] = [
            0x01, 0x33, 0x01, 0x33, 0x00, 0x41, 0x14, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x74,
            0xd4, 0x37, 0x5a, 0x01, 0x33, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
            0x00, 0x00, 0x00, 0x01, 0x33, 0x51, 0x00, 0x00, 0x00,
        ];
        const GO_NO_TRAILING_WORD: usize = 33;
        let mut extra = go.to_vec();
        extra.push(0);
        assert!(parse_spell_go(&extra).is_err());
        for cut in 0..go.len() {
            if cut == GO_NO_TRAILING_WORD {
                assert_eq!(parse_spell_go(&go[..cut]).unwrap().trailing, None);
                continue;
            }
            assert!(
                parse_spell_go(&go[..cut]).is_err(),
                "{cut} bytes parsed as a whole SMSG_SPELL_GO"
            );
        }
    }

    /// A target shape other than `target_flags::UNIT` has never been
    /// captured live, and the parser says so with a specific error rather
    /// than misreading whatever would follow it.
    #[test]
    fn an_unrecognised_target_shape_is_refused_by_name() {
        let mut start: [u8; 27] = [
            0x01, 0x33, 0x01, 0x33, 0x00, 0x41, 0x14, 0x00, 0x00, 0x02, 0x08, 0x00, 0x00, 0xdc,
            0x05, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x33, 0x64, 0x00, 0x00, 0x00,
        ];
        // Flip the target flags to something other than UNIT (offset 17..21).
        start[17] = 0x00;
        let error = parse_spell_start(&start).unwrap_err();
        assert!(
            matches!(
                error,
                crate::protocol::Error::UnsupportedSpellTarget { flags: 0, .. }
            ),
            "{error}"
        );
    }

    /// A nonzero miss count has never been captured live either, and the
    /// per-entry shape it would need is exactly the kind of thing this
    /// project's rules say not to invent.
    #[test]
    fn a_nonzero_miss_count_is_refused_rather_than_guessed() {
        let mut go: [u8; 37] = [
            0x01, 0x33, 0x01, 0x33, 0x00, 0x41, 0x14, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x74,
            0xd4, 0x37, 0x5a, 0x01, 0x33, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
            0x00, 0x00, 0x00, 0x01, 0x33, 0x51, 0x00, 0x00, 0x00,
        ];
        go[26] = 1; // miss_count, actually zero in the capture
        let error = parse_spell_go(&go).unwrap_err();
        assert!(
            matches!(
                error,
                crate::protocol::Error::UnconfirmedSpellMisses { count: 1 }
            ),
            "{error}"
        );
    }

    /// The target mask decides how the server reads everything after it, so a
    /// self cast and a targeted one are different lengths and must stay that
    /// way. Claiming a unit target without supplying a guid does not fail --
    /// it reads whatever comes next as one.
    #[test]
    fn a_targeted_cast_carries_a_guid_and_a_self_cast_does_not() {
        let on_self = cast_spell(6603, None);
        assert_eq!(on_self.len(), 1 + 4 + 1 + 4);
        assert_eq!(&on_self[6..10], &target_flags::SELF.to_le_bytes());

        let on_target = cast_spell(6603, Some(0xF130_0000_2B00_0BBA));
        assert!(on_target.len() > on_self.len());
        assert_eq!(&on_target[6..10], &target_flags::UNIT.to_le_bytes());

        // And the guid must be the packed form, which the update layer already
        // round-trips -- so read it back with the real reader.
        let mut r = Reader::new(&on_target[10..], "guid");
        assert_eq!(
            crate::update::read_packed_guid(&mut r).unwrap(),
            0xF130_0000_2B00_0BBA
        );
        r.finish().unwrap();
    }

    #[test]
    fn the_spell_id_survives_the_write() {
        let body = cast_spell(0x1234_5678, None);
        assert_eq!(&body[1..5], &0x1234_5678u32.to_le_bytes());
    }

    /// `foss-wow#141`: a chest is opened by casting at it, not by
    /// [`crate::opcode::ClientOpcode::GameObjectUse`], and the flag has to
    /// say `GAMEOBJECT` rather than `UNIT` -- a unit-flagged guid at
    /// `Spell::EffectOpenLock` never populates `gameObjTarget`, so the cast
    /// would silently do nothing to the chest at all.
    #[test]
    fn a_gameobject_cast_carries_the_gameobject_flag_and_a_guid() {
        let body = cast_spell_at_gameobject(OPEN_LOCK_KNEELING, 0xF110_0277_1500_05A8);
        assert_eq!(&body[1..5], &OPEN_LOCK_KNEELING.to_le_bytes());
        assert_eq!(&body[6..10], &target_flags::GAMEOBJECT.to_le_bytes());
        assert_ne!(target_flags::GAMEOBJECT, target_flags::UNIT);

        let mut r = Reader::new(&body[10..], "guid");
        assert_eq!(
            crate::update::read_packed_guid(&mut r).unwrap(),
            0xF110_0277_1500_05A8
        );
        r.finish().unwrap();
    }

    /// A failure this client has not observed keeps its number.
    ///
    /// A wrong name here would not error -- it would confidently misexplain
    /// why a cast failed and send the next reader looking in the wrong place.
    /// Only reasons actually seen against a live realm get words.
    #[test]
    fn unobserved_failures_keep_their_number() {
        assert!(describe_cast_failure(0x0C).contains("target"));
        assert!(describe_cast_failure(0xEE).contains("0xee"));
        assert!(describe_cast_failure(0x4A).contains("0x4a"));
    }

    /// The one parser here that does not assert full consumption, because the
    /// tail's shape depends on the reason and refusing it would turn "out of
    /// range" into a decode error.
    #[test]
    fn a_cast_failure_tolerates_data_it_does_not_understand() {
        let mut body = vec![0u8];
        body.extend(6603u32.to_le_bytes());
        body.push(0x4A);
        body.extend(1234u32.to_le_bytes()); // reason-specific detail

        let parsed = parse_cast_failed(&body).unwrap();
        assert_eq!(parsed.spell_id, 6603);
        assert_eq!(parsed.reason, 0x4A);
    }
}
