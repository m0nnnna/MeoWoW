//! Which spells are currently sitting on a unit.
//!
//! **This module exists because a toggle cannot be switched off without it.**
//! Stealth, a druid's forms, a warrior's stances: each is turned *on* by
//! casting a spell, and pressing that spell again does not turn it off.
//! `CMSG_CAST_SPELL` for a spell whose aura is already held draws no reply of
//! any number — not a refusal, not an acknowledgement — so a rogue pressing
//! Stealth a second time stays hidden and nothing anywhere says why. The
//! opcode that ends it is [`crate::ClientOpcode::CancelAura`], and it wants
//! the *spell id of the aura being dropped*, which is exactly the fact
//! nothing on this client held.
//!
//! The state fields answer a different question. `UNIT_FIELD_BYTES_1` says a
//! unit *is* stealthed and `UNIT_FIELD_BYTES_2` says which form it is in;
//! neither says which of the several spells that produce that state is the one
//! responsible. Guessing is not available: a druid's Prowl and a rogue's
//! Stealth set the same bit, and the ranks of one spell are different ids.
//!
//! ## The two opcodes
//!
//! `SMSG_AURA_UPDATE_ALL` (`0x0495`) replaces everything known about one
//! unit; `SMSG_AURA_UPDATE` (`0x0496`) carries a single slot. They share a
//! record layout and differ only in how many records follow the guid, which is
//! why one function reads both.
//!
//! ```text
//!   packed guid
//!   repeat:
//!     u8  slot
//!     u32 spell id            -- zero means "this slot is now empty"
//!     u8  flags
//!     u8  caster level
//!     u8  stacks or charges
//!     packed guid caster      -- only when FLAG_CASTER is clear
//!     u32 max duration, u32 duration   -- only when FLAG_DURATION is set
//! ```
//!
//! **The conditional fields are the risk**, and this is the packet shape
//! `CLAUDE.md` warns about: a reader that skipped them would parse the common
//! case perfectly, because the common case has `FLAG_CASTER` set and no
//! duration and so contains neither. The cursor is what catches it — every
//! record is read through one and the body must be consumed exactly.
//!
//! Checked against three captures from the local realm that exercise
//! different shapes: a stealth apply (10 bytes, one record, no caster and no
//! duration), a stealth removal (7 bytes, the `spell id == 0` form), and a
//! creature's list under a six-byte packed guid (15 bytes). Each consumed to
//! the last byte, which is the only check here that a conditional field being
//! read at the wrong time would fail.

use std::collections::HashMap;

use crate::protocol::{Error, Reader};
use crate::update::read_packed_guid;

/// The aura flag that means "the caster is the target", so no caster guid
/// follows.
///
/// Named because it is a *presence* flag for a field rather than a property of
/// the aura, and the two read identically at a call site.
const FLAG_CASTER: u8 = 0x08;

/// The aura flag that means two duration words follow.
const FLAG_DURATION: u8 = 0x20;

/// One spell sitting on a unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aura {
    /// Which of the unit's aura slots this occupies. The server's own index,
    /// not a position in a list -- see the note on `SMSG_GOSSIP_MESSAGE`'s
    /// option ids: an index off the wire is an id.
    pub slot: u8,
    /// The spell, and **the reason this module exists**: it is what
    /// `CMSG_CANCEL_AURA` is asked in terms of.
    pub spell_id: u32,
    /// The raw flag byte. Two bits are interpreted here, to decide whether
    /// optional fields follow; the rest are left as a number rather than
    /// named, because nothing has checked what they do.
    pub flags: u8,
    /// The level of whoever cast it.
    pub caster_level: u8,
    /// Stacks for a stacking aura, charges otherwise. The server picks which
    /// it sends and does not say which it picked, so this is one number and
    /// carries both meanings.
    pub stacks: u8,
    /// Who cast it, when the packet said. `None` means the flags claimed the
    /// target cast it on itself -- an absence with a meaning, not a gap.
    pub caster: Option<u64>,
    /// How long it lasts and how much is left, in milliseconds, when the
    /// packet carried them. Absent for a toggle like Stealth, which is exactly
    /// what "until something breaks it" looks like on the wire.
    pub duration: Option<(u32, u32)>,
}

/// What one aura packet said about one unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraUpdate {
    pub guid: u64,
    /// Whether this replaces everything known about the unit
    /// (`SMSG_AURA_UPDATE_ALL`) or amends single slots
    /// (`SMSG_AURA_UPDATE`).
    ///
    /// **The distinction is load-bearing and is not derivable from the
    /// contents.** A one-record `_ALL` and a one-record single update look
    /// identical, and applying the first as an amendment leaves every aura the
    /// unit has just lost still on file. Same trap as a create block against a
    /// values block in `crates/world/src/update.rs`.
    pub replaces_all: bool,
    /// Every record in the packet, including the removals -- a record whose
    /// `spell_id` is zero says its slot is now empty, and dropping those here
    /// would make a removal indistinguishable from a packet that never arrived.
    pub records: Vec<Aura>,
}

/// Reads either aura opcode.
///
/// `replaces_all` selects which one, and it comes from the opcode rather than
/// from the body because the body cannot say.
pub fn parse_aura_update(body: &[u8], replaces_all: bool) -> Result<AuraUpdate, Error> {
    let mut r = Reader::new(
        body,
        if replaces_all {
            "SMSG_AURA_UPDATE_ALL"
        } else {
            "SMSG_AURA_UPDATE"
        },
    );
    let guid = read_packed_guid(&mut r)?;

    let mut records = Vec::new();
    // **Reads to the end rather than to a count, because there is no count.**
    // A single update holds exactly one record and the `_ALL` form holds
    // however many the unit has, including none -- an empty list is a real and
    // common packet (a two-byte body naming a unit with nothing on it), and it
    // has to parse rather than fail.
    while r.remaining() > 0 {
        let slot = r.u8()?;
        let spell_id = r.u32()?;
        if spell_id == 0 {
            // The removal form stops here. Everything below describes an aura
            // that exists, and a slot being cleared has none of it.
            records.push(Aura {
                slot,
                spell_id: 0,
                flags: 0,
                caster_level: 0,
                stacks: 0,
                caster: None,
                duration: None,
            });
            continue;
        }
        let flags = r.u8()?;
        let caster_level = r.u8()?;
        let stacks = r.u8()?;
        let caster = if flags & FLAG_CASTER == 0 {
            Some(read_packed_guid(&mut r)?)
        } else {
            None
        };
        let duration = if flags & FLAG_DURATION != 0 {
            Some((r.u32()?, r.u32()?))
        } else {
            None
        };
        records.push(Aura {
            slot,
            spell_id,
            flags,
            caster_level,
            stacks,
            caster,
            duration,
        });
    }

    r.finish()?;
    Ok(AuraUpdate {
        guid,
        replaces_all,
        records,
    })
}

/// Every aura currently on one unit, by slot.
///
/// A map rather than a list because the wire is slot-addressed: an update
/// names a slot and either fills or empties it, and a list would have to
/// search for the right entry and would silently append on a miss.
pub type Auras = HashMap<u8, Aura>;

/// Folds one packet into what is known about a unit.
///
/// **Removals are applied, not skipped**, which is the whole reason this is a
/// function rather than an `extend`: a record with `spell_id == 0` empties its
/// slot, and treating the records as things to insert would leave a cancelled
/// stealth on file forever -- and the client would then keep offering to
/// cancel an aura the character no longer has.
pub fn apply(known: &mut Auras, update: &AuraUpdate) {
    if update.replaces_all {
        known.clear();
    }
    for record in &update.records {
        if record.spell_id == 0 {
            known.remove(&record.slot);
        } else {
            known.insert(record.slot, record.clone());
        }
    }
}

/// Whether this unit is currently carrying a given spell's aura.
///
/// The question the action bar asks before deciding whether pressing a spell
/// means "cast this" or "stop doing this".
pub fn holds(known: &Auras, spell_id: u32) -> bool {
    known.values().any(|aura| aura.spell_id == spell_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SMSG_AURA_UPDATE` as the local realm sent it when `Roguetest` cast
    /// Stealth on itself, captured 2026-08-22. Ten bytes.
    ///
    /// Kept as the bytes rather than as a built struct, like every other wire
    /// fixture here: the assertions are then about the packet and not about
    /// this test's idea of one.
    const STEALTH_APPLIED: [u8; 10] = [
        0x01, 0x0b, 0x00, 0xf8, 0x06, 0x00, 0x00, 0x1f, 0x02, 0x01,
    ];

    /// The same character a moment later, after `CMSG_CANCEL_AURA`. Seven
    /// bytes, and the shape that says a slot is empty.
    const STEALTH_REMOVED: [u8; 7] = [0x01, 0x0b, 0x00, 0x00, 0x00, 0x00, 0x00];

    /// `SMSG_AURA_UPDATE_ALL` about a creature, under a six-byte packed guid.
    /// Fifteen bytes, one record.
    const CREATURE_LIST: [u8; 15] = [
        0xdb, 0x0e, 0x0b, 0x2b, 0x01, 0x30, 0xf1, 0x00, 0x54, 0x18, 0x01, 0x00, 0x19, 0x01, 0x00,
    ];

    /// An `_ALL` naming a unit with nothing on it: two bytes, no records.
    const EMPTY_LIST: [u8; 2] = [0x01, 0x0b];

    /// The packet this whole module was written for, field by field.
    ///
    /// **The flag byte is checked as well as the spell id**, because it is what
    /// decides whether two optional blocks follow, and a reading that got them
    /// wrong would still produce the right spell id here -- the caster guid and
    /// the durations are both *absent* in this capture, so a parser that read
    /// them unconditionally fails on length and one that never read them passes
    /// this packet and fails on the next.
    #[test]
    fn a_stealth_apply_reads_field_by_field() {
        let update = parse_aura_update(&STEALTH_APPLIED, false).expect("parse");
        assert_eq!(update.guid, 0x0b);
        assert!(!update.replaces_all);
        assert_eq!(update.records.len(), 1);

        let aura = &update.records[0];
        assert_eq!(aura.slot, 0);
        assert_eq!(aura.spell_id, 1784, "Stealth");
        assert_eq!(aura.flags, 0x1f);
        assert_eq!(aura.caster_level, 2, "Roguetest is level 2");
        assert_eq!(aura.stacks, 1);
        assert_eq!(
            aura.caster, None,
            "FLAG_CASTER was set, so no caster guid follows"
        );
        assert_eq!(
            aura.duration, None,
            "a toggle has no duration, which is what 'until something breaks it' looks like"
        );
    }

    /// A cleared slot arrives as a spell id of zero and stops there.
    ///
    /// Seven bytes is the evidence: a parser that went on to read the flags,
    /// level and stacks would run off the end, and one that treated the record
    /// as an insertion would leave a cancelled stealth on file forever.
    #[test]
    fn a_removal_is_a_spell_id_of_zero() {
        let update = parse_aura_update(&STEALTH_REMOVED, false).expect("parse");
        assert_eq!(update.records.len(), 1);
        assert_eq!(update.records[0].spell_id, 0);

        let mut known = Auras::new();
        apply(
            &mut known,
            &parse_aura_update(&STEALTH_APPLIED, false).unwrap(),
        );
        assert!(holds(&known, 1784));
        apply(&mut known, &update);
        assert!(
            !holds(&known, 1784),
            "the slot was filled again instead of being emptied"
        );
        assert!(known.is_empty());
    }

    /// A six-byte packed guid, so the record does not start where a
    /// one-byte one would leave it.
    ///
    /// The guid width varies per packet and is the one part of the header that
    /// cannot be assumed; getting it wrong shifts every field after it and
    /// still parses, which is why this fixture is here beside the short one.
    #[test]
    fn a_creature_list_reads_under_a_wide_packed_guid() {
        let update = parse_aura_update(&CREATURE_LIST, true).expect("parse");
        assert_eq!(update.guid, 0xf130_0001_2b00_0b0e);
        assert!(update.replaces_all);
        assert_eq!(update.records.len(), 1);
        assert_eq!(update.records[0].spell_id, 0x0001_1854);
        assert_eq!(update.records[0].caster_level, 1);
    }

    /// A unit with no auras is a real packet and must parse rather than fail.
    ///
    /// It is also the one that separates the two opcodes: as an `_ALL` it
    /// means "this unit now has nothing", which has to *clear* what was known.
    /// Read as an amendment it would mean nothing at all, and a unit whose
    /// last aura fell off would keep it.
    #[test]
    fn an_empty_list_clears_what_was_known() {
        let update = parse_aura_update(&EMPTY_LIST, true).expect("parse");
        assert_eq!(update.guid, 0x0b);
        assert!(update.records.is_empty());

        let mut known = Auras::new();
        apply(
            &mut known,
            &parse_aura_update(&STEALTH_APPLIED, false).unwrap(),
        );
        assert!(holds(&known, 1784));
        apply(&mut known, &update);
        assert!(
            known.is_empty(),
            "an _ALL with no records is a statement that the unit has none"
        );
    }

    /// The same empty body read as a single update must **not** clear
    /// anything.
    ///
    /// Asserted next to its opposite deliberately: the two bodies are
    /// byte-identical and only the opcode separates them, so a test of one
    /// alone passes with the distinction removed entirely.
    #[test]
    fn a_single_update_never_clears_the_rest() {
        let mut known = Auras::new();
        apply(
            &mut known,
            &parse_aura_update(&STEALTH_APPLIED, false).unwrap(),
        );
        apply(
            &mut known,
            &parse_aura_update(&EMPTY_LIST, false).expect("parse"),
        );
        assert!(
            holds(&known, 1784),
            "an amendment carrying no records erased the unit's auras"
        );
    }

    /// A body with a field too many or too few is refused rather than
    /// half-read.
    #[test]
    fn a_truncated_record_is_an_error() {
        assert!(parse_aura_update(&STEALTH_APPLIED[..9], false).is_err());
        let mut long = STEALTH_APPLIED.to_vec();
        long.push(0x00);
        assert!(
            parse_aura_update(&long, false).is_err(),
            "a trailing byte is the evidence of a field of the wrong width"
        );
    }
}
