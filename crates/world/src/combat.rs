//! Melee combat: who swung at whom, and what it cost them.
//!
//! Three packets, all confirmed against one live capture taken by walking a
//! level-one warrior into a Northshire wolf and swinging until one of them
//! died -- `wow-cli world --enter Testwolf --attack --stay 40 --capture`. The
//! numbers below are read off that capture, and the tests at the bottom are
//! built from its literal bytes.
//!
//! **The attack opcodes were confirmed by reaction, not by transcription.**
//! Nothing acknowledges an opcode. The first attempt sent `CMSG_ATTACKSWING`
//! from five units away without turning to face the target, and the server
//! answered with two different empty-bodied refusals, three times each, and no
//! damage. Closing to melee range and facing the target first turned those
//! into `SMSG_ATTACKSTART` and fifteen swings. A wrong opcode number produces
//! silence; a right one produces a specific, repeatable reaction, and that
//! difference is the whole test.

use crate::protocol::{Error, Reader};
use crate::update::read_packed_guid;

/// Bits seen set in [`MeleeSwing::hit_info`], and only those.
///
/// The full 3.3.5a mask has around twenty flags. Naming the ones a single
/// capture actually exercised, and leaving the rest as a number, is the same
/// rule `describe_cast_failure` follows: a wrong *name* for a flag never fails
/// loudly, it just misexplains combat to whoever reads the log next.
pub mod hit_info {
    /// Set on every landed swing in the capture, both directions.
    pub const NORMAL_SWING: u32 = 0x0000_0002;
    /// The swing did not connect: damage, and every damage block in it, read
    /// zero, and the victim state reads zero rather than one.
    pub const MISS: u32 = 0x0000_0010;
    /// A critical hit. Both swings carrying this bit did roughly double the
    /// damage of the ones without it -- 12 and 8 against a spread of 4 to 6 --
    /// which is what makes this reading a measurement rather than a guess.
    pub const CRITICAL: u32 = 0x0000_0200;
    /// The packet carries one more `u32` after its usual tail.
    ///
    /// **Named for what it does to the layout, not for what it means**, and
    /// the difference matters. What is established is a perfect correlation
    /// across 39 captured swings: all 35 packets of 42 bytes have this bit
    /// clear, and all 4 packets of 46 bytes have it set. That is a structural
    /// fact and this parser depends on it.
    ///
    /// What the extra number *is* remains open. It read `2` on two swings
    /// that dealt no damage at all and `1` on two that dealt 5 and 4, which
    /// is consistent with an amount blocked -- a swing blocked in full deals
    /// nothing -- but "consistent with" is not "confirmed", and a wrong name
    /// on a combat log misexplains a fight to whoever reads it next. See
    /// [`MeleeSwing::extra_amount`].
    pub const CARRIES_EXTRA_AMOUNT: u32 = 0x0000_2000;
}

/// One damage component of a swing: physical, or a school of magic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SwingDamage {
    /// `1` for physical, which is the only value the capture contains.
    pub school_mask: u32,
    /// The same figure as [`Self::damage`], carried again as a float. Both are
    /// kept rather than one dropped: they agreed in all fifteen captured
    /// swings, and that agreement is a free check that the block's fields are
    /// where this parser thinks they are.
    pub float_damage: f32,
    pub damage: u32,
}

/// One swing landing or missing, from `SMSG_ATTACKERSTATEUPDATE`.
///
/// **What is confirmed, and how.** The two guids are read directly: the
/// capture's attacker alternates between the character (`0x32`) and the wolf
/// (`0xf130000006000bde`) exactly as the fight did. [`Self::damage`] agrees
/// with both figures inside the damage block in every swing. [`Self::overkill`]
/// read zero for fourteen swings and `7` for the fifteenth -- the killing
/// blow, an 8-damage critical against a wolf with 1 health left, which is the
/// only reading of that field consistent with the fight ending there.
///
/// **What is not, and why the parser refuses rather than guesses.** Every
/// captured swing carried exactly one damage block, so the block's width and
/// the packet's trailing fields cannot be told apart: a 12-byte block followed
/// by nine bytes of tail and a 20-byte block followed by one byte of tail
/// describe the same 42 bytes. The reading here is the 12-byte one, chosen on
/// evidence rather than preference -- under the other reading the field that
/// separates a hit from a miss (`1` on all eleven landed swings, `0` on both
/// misses) would be the *absorbed* amount, and an absorb of exactly one on
/// every landed hit is not what absorption means. One capture carrying two
/// damage blocks would settle it in a single observation by its length alone,
/// so [`parse_melee_swing`] errors on any count but one instead of quietly
/// reading the wrong width.
#[derive(Debug, Clone, PartialEq)]
pub struct MeleeSwing {
    pub hit_info: u32,
    pub attacker: u64,
    pub victim: u64,
    /// Total damage dealt, already summed over [`Self::damage_blocks`].
    pub damage: u32,
    /// Damage past what the victim had left -- non-zero only on a killing
    /// blow.
    pub overkill: u32,
    pub damage_blocks: Vec<SwingDamage>,
    /// `1` on every landed swing, `0` on both misses. See the struct's own
    /// doc comment for why this is read here rather than inside the damage
    /// block.
    pub victim_state: u32,
    /// Read zero in all fifteen captured swings, so nothing distinguishes what
    /// they mean. Named for what they are -- a u32 and a byte at the end of
    /// the packet -- rather than for what they might be.
    pub trailing: u32,
    pub trailing_byte: u8,
    /// Present exactly when [`hit_info::CARRIES_EXTRA_AMOUNT`] is set, which
    /// is what makes this packet 46 bytes rather than 42.
    ///
    /// Deliberately not named `blocked`. Four captures carry it: `2` on two
    /// swings that dealt no damage, `1` on two that dealt 5 and 4. A block
    /// absorbing a whole swing would explain that, and so would two or three
    /// other things, and the cost of guessing is a combat log that confidently
    /// says the wrong word. Left as a number until an observation pins it --
    /// the same rule `describe_cast_failure` and `SpellCooldown::second`
    /// already live by.
    pub extra_amount: Option<u32>,
}

impl MeleeSwing {
    pub fn missed(&self) -> bool {
        self.hit_info & hit_info::MISS != 0
    }

    pub fn critical(&self) -> bool {
        self.hit_info & hit_info::CRITICAL != 0
    }

    /// Whether this swing killed the victim.
    ///
    /// Overkill is non-zero only when the damage exceeded what was left, so it
    /// is proof of a kill but not a requirement for one -- a swing landing for
    /// exactly the remaining health kills with an overkill of zero. Treated as
    /// a hint rather than as the death signal for that reason; the authority
    /// on a unit being dead is its health field reaching zero.
    pub fn overkilled(&self) -> bool {
        self.overkill > 0
    }
}

/// Parses `SMSG_ATTACKERSTATEUPDATE`. See [`MeleeSwing`] for what one capture
/// does and does not establish.
pub fn parse_melee_swing(body: &[u8]) -> Result<MeleeSwing, Error> {
    let mut r = Reader::new(body, "SMSG_ATTACKERSTATEUPDATE");
    let hit_info = r.u32()?;
    let attacker = read_packed_guid(&mut r)?;
    let victim = read_packed_guid(&mut r)?;
    let damage = r.u32()?;
    let overkill = r.u32()?;

    let count = r.u8()?;
    if count != 1 {
        return Err(Error::UnconfirmedSwingDamageBlocks { count });
    }
    let mut damage_blocks = Vec::with_capacity(count as usize);
    for _ in 0..count {
        damage_blocks.push(SwingDamage {
            school_mask: r.u32()?,
            float_damage: r.f32()?,
            damage: r.u32()?,
        });
    }

    let victim_state = r.u32()?;
    let trailing = r.u32()?;
    let trailing_byte = r.u8()?;
    // Gated on the bit rather than on how many bytes happen to be left: a
    // parser that read "one more u32 if any remain" would silently absorb a
    // field of any other shape that turned up here, which is exactly how a
    // wrong layout stops announcing itself.
    let extra_amount = (hit_info & hit_info::CARRIES_EXTRA_AMOUNT != 0)
        .then(|| r.u32())
        .transpose()?;
    r.finish()?;

    Ok(MeleeSwing {
        hit_info,
        attacker,
        victim,
        damage,
        overkill,
        damage_blocks,
        victim_state,
        trailing,
        trailing_byte,
        extra_amount,
    })
}

/// Two units entering melee, from `SMSG_ATTACKSTART`.
///
/// Sixteen bytes of two *unpacked* guids -- unlike almost everything else in
/// combat, which packs them. The capture shows it plainly: the first of these
/// to arrive read `32 00 00 00 00 00 00 00` then the wolf's guid, and the
/// second, when the wolf swung back, read the same two the other way round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttackStart {
    pub attacker: u64,
    pub victim: u64,
}

pub fn parse_attack_start(body: &[u8]) -> Result<AttackStart, Error> {
    let mut r = Reader::new(body, "SMSG_ATTACKSTART");
    let attacker = r.u64()?;
    let victim = r.u64()?;
    r.finish()?;
    Ok(AttackStart { attacker, victim })
}

/// Melee ending, from `SMSG_ATTACKSTOP`.
///
/// Packed guids here, where `SMSG_ATTACKSTART` used raw ones -- the two
/// packets disagree about their own encoding, which is worth stating because
/// reusing one parser for both would read this as a truncated `AttackStart`
/// and produce two plausible, wrong guids rather than an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttackStop {
    pub attacker: u64,
    pub victim: u64,
    /// Read zero in both captured stops, including the one that followed a
    /// death. Not named for a meaning nothing has demonstrated.
    pub trailing: u32,
}

pub fn parse_attack_stop(body: &[u8]) -> Result<AttackStop, Error> {
    let mut r = Reader::new(body, "SMSG_ATTACKSTOP");
    let attacker = read_packed_guid(&mut r)?;
    let victim = read_packed_guid(&mut r)?;
    let trailing = r.u32()?;
    r.finish()?;
    Ok(AttackStop {
        attacker,
        victim,
        trailing,
    })
}
/// Damage from a spell, as `SMSG_SPELLNONMELEEDAMAGELOG` reports it.
///
/// The other half of the combat log. A melee swing arrives as
/// [`MeleeSwing`]; everything cast arrives here, and until now this client
/// counted the opcode and dropped it.
///
/// **Captured before it was parsed.** `wow-cli world --enter Testdruid
/// --target Nightsaber --cast 5176` puts a Wrath into a Young Nightsaber and
/// records the reply, and the 46-byte body it produced anchors every field
/// named below: the target guid matches the creature that was selected, the
/// caster guid matches the druid, the spell id reads 5176, the damage reads 17
/// on a level-one Wrath, and the school reads 8 -- Wrath is a Nature spell.
/// Five independent agreements in one packet.
#[derive(Debug, Clone, PartialEq)]
pub struct SpellDamage {
    /// Who was hit.
    pub target: u64,
    /// Who cast it.
    pub caster: u64,
    pub spell_id: u32,
    pub damage: u32,
    /// Damage past what the target had left. Zero in the capture, which is
    /// consistent with a 42-health creature taking 17 -- consistent, not
    /// confirmed.
    pub overkill: u32,
    /// Bitmask: `8` is Nature, which is what the captured Wrath reported.
    pub school_mask: u32,
    /// **Everything after the school, unread.** Twenty bytes, all zero in the
    /// only capture on hand.
    ///
    /// They are widely said to be absorb, resist, a periodic flag, block and
    /// hit info, and that may well be right -- but a hit that was partly
    /// resisted is exactly the packet nobody here has captured, and a wrong
    /// *name* on a combat log misexplains a fight to whoever reads it next.
    /// The same restraint `MeleeSwing::extra_amount` and
    /// `describe_cast_failure` already apply. Kept whole rather than skipped so
    /// the shape survives the refusal: the first non-zero one settles it.
    pub trailing: Vec<u8>,
}

impl SpellDamage {
    /// Whether this killed the target outright.
    pub fn overkilled(&self) -> bool {
        self.overkill > 0
    }
}

pub fn parse_spell_damage(body: &[u8]) -> Result<SpellDamage, Error> {
    let mut r = Reader::new(body, "SMSG_SPELLNONMELEEDAMAGELOG");
    let target = crate::update::read_packed_guid(&mut r)?;
    let caster = crate::update::read_packed_guid(&mut r)?;
    let spell_id = r.u32()?;
    let damage = r.u32()?;
    let overkill = r.u32()?;
    let school_mask = r.u32()?;
    let trailing = r.rest().to_vec();
    Ok(SpellDamage {
        target,
        caster,
        spell_id,
        damage,
        overkill,
        school_mask,
        trailing,
    })
}

/// One line of combat log for a spell that landed.
///
/// Deliberately the same shape as [`describe_swing`]: a reader should not be
/// able to tell from the sentence which packet it came from, only what
/// happened. The spell is named by id here because naming it needs
/// `Spell.dbc`, which this crate does not read -- the interface substitutes a
/// name when it has one.
pub fn describe_spell_damage(
    hit: &SpellDamage,
    own: u64,
    name_of: impl Fn(u64) -> String,
    spell_name: impl Fn(u32) -> Option<String>,
) -> String {
    let who = if hit.caster == own {
        "You".to_string()
    } else {
        name_of(hit.caster)
    };
    let verb = if hit.caster == own { "hit" } else { "hits" };
    let whom = if hit.target == own {
        "you".to_string()
    } else {
        name_of(hit.target)
    };
    let spell = spell_name(hit.spell_id).unwrap_or_else(|| format!("spell {}", hit.spell_id));
    let mut line = format!("{who} {verb} {whom} with {spell} for {}.", hit.damage);
    if hit.overkilled() {
        line.push_str(" Killing blow.");
    }
    line
}


/// One unit's threat list, from `SMSG_THREAT_UPDATE`.
///
/// The guid is the unit whose list it is -- the *victim* -- and the entries
/// are everyone on it. Confirmed structurally rather than by a single number,
/// because there is nothing outside the packet to check a threat figure
/// against: across a whole fight every one of these named the kobold as the
/// owner, carried exactly one entry naming this client, climbed monotonically
/// from 1360 to 4240 as the fight went on, and consumed its body exactly.
/// A layout that was wrong about where the count sat would not have produced a
/// clean tail on every packet, let alone a rising series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreatUpdate {
    /// Whose list this is.
    pub victim: u64,
    /// Who is on it, and by how much. Ordered as the packet sent them.
    pub entries: Vec<(u64, u32)>,
}

pub fn parse_threat_update(body: &[u8]) -> Result<ThreatUpdate, Error> {
    let mut r = Reader::new(body, "SMSG_THREAT_UPDATE");
    let victim = read_packed_guid(&mut r)?;
    let count = r.u32()?;
    // A count is a length taken off the wire, and this one has only ever been
    // seen as 1. Reserving on it would let a corrupt packet ask for gigabytes
    // before the reader ran out of input and said so.
    let mut entries = Vec::new();
    for _ in 0..count {
        let who = read_packed_guid(&mut r)?;
        entries.push((who, r.u32()?));
    }
    r.finish()?;
    Ok(ThreatUpdate { victim, entries })
}

/// One swing as a line of combat log.
///
/// Lives here, next to the parser, because both the viewer and `wow-cli` show
/// it and two hand-written versions of the same sentence drift -- the same
/// reasoning that keeps the action bar's slot geometry in one function. The
/// caller supplies naming rather than this crate reaching for it: names
/// resolve asynchronously and a guid that has no name yet must still produce
/// a line, not wait for one.
///
/// `own` is the reading player's guid, which is what turns a pair of names
/// into "You hit X" rather than "X hits Y" -- a combat log written entirely in
/// the third person is technically correct and unreadable in a fight.
pub fn describe_swing(swing: &MeleeSwing, own: u64, name_of: impl Fn(u64) -> String) -> String {
    let attacker_is_me = swing.attacker == own;
    let attacker = if attacker_is_me {
        "You".to_string()
    } else {
        name_of(swing.attacker)
    };
    let victim = if swing.victim == own {
        "you".to_string()
    } else {
        name_of(swing.victim)
    };

    if swing.missed() {
        // "You miss" against "Young Wolf misses": English subject-verb
        // agreement is the whole reason this is not one format string.
        let verb = if attacker_is_me { "miss" } else { "misses" };
        return format!("{attacker} {verb} {victim}.");
    }

    let verb = if attacker_is_me { "hit" } else { "hits" };
    let crit = if swing.critical() { " (critical)" } else { "" };
    let mut line = format!("{attacker} {verb} {victim} for {}{crit}.", swing.damage);
    if swing.overkilled() {
        // Overkill is the one field that says a swing was fatal, and a combat
        // log that does not mark the killing blow makes a fight hard to read
        // back.
        line.push_str(" Killing blow.");
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact 46 bytes `wow1.nekos.farm` sent when a level-one druid put a
    /// Wrath into a Young Nightsaber.
    ///
    /// Five anchors in one packet, which is what makes this a measurement
    /// rather than a layout that happens to parse: the target guid is the
    /// creature that was selected, the caster guid is the druid, the spell id
    /// is the one that was cast, the damage is right for a rank-one Wrath, and
    /// the school is Nature -- which is Wrath's school and nothing else's.
    const WRATH_HIT: [u8; 46] = [
        0xdf, 0x11, 0xc1, 0x06, 0xef, 0x07, 0x30, 0xf1, // target, packed
        0x01, 0x33, // caster, packed
        0x38, 0x14, 0x00, 0x00, // spell 5176
        0x11, 0x00, 0x00, 0x00, // 17 damage
        0x00, 0x00, 0x00, 0x00, // no overkill
        0x08, 0x00, 0x00, 0x00, // school 8, Nature
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn a_captured_spell_hit_decodes_to_what_was_cast() {
        let hit = parse_spell_damage(&WRATH_HIT).expect("captured live");
        assert_eq!(hit.target, 0xf130_0007_ef06_c111, "the nightsaber");
        assert_eq!(hit.caster, 0x33, "the druid");
        assert_eq!(hit.spell_id, 5176, "Wrath");
        assert_eq!(hit.damage, 17);
        assert_eq!(hit.overkill, 0);
        assert_eq!(hit.school_mask, 8, "Nature");
        assert!(!hit.overkilled());
        // Everything past the school is kept rather than skipped, so the first
        // non-zero one settles what it is.
        assert_eq!(hit.trailing.len(), 20);
        assert!(hit.trailing.iter().all(|b| *b == 0));
    }

    #[test]
    fn a_truncated_spell_hit_is_refused() {
        assert!(parse_spell_damage(&WRATH_HIT[..12]).is_err());
    }

    /// A spell line must read like a swing line: the reader should not be able
    /// to tell which packet it came from, only what happened.
    #[test]
    fn a_spell_hit_reads_as_a_sentence() {
        let hit = parse_spell_damage(&WRATH_HIT).unwrap();
        let line = describe_spell_damage(&hit, 0x33, |_| "Young Nightsaber".into(), |id| {
            (id == 5176).then(|| "Wrath".to_string())
        });
        assert_eq!(line, "You hit Young Nightsaber with Wrath for 17.");

        // Seen from the other end, and with no spell name available.
        let line = describe_spell_damage(&hit, 0xf130_0007_ef06_c111, |guid| {
            if guid == 0x33 { "Testdruid".into() } else { "you".into() }
        }, |_| None);
        assert_eq!(line, "Testdruid hits you with spell 5176 for 17.");
    }

    /// Overkill is what turns a hit into a killing blow, and the log has to say
    /// so -- the melee half already does.
    #[test]
    fn a_killing_spell_says_so() {
        let mut body = WRATH_HIT;
        body[18] = 3; // overkill
        let hit = parse_spell_damage(&body).unwrap();
        assert!(hit.overkilled());
        let line = describe_spell_damage(&hit, 0x33, |_| "Young Nightsaber".into(), |_| None);
        assert!(line.ends_with("Killing blow."), "got {line}");
    }

    /// The character's guid in the capture, and the wolf's.
    const ME: u64 = 0x32;
    const WOLF: u64 = 0xf130_0000_0600_0bde;

    /// A swing this client landed, verbatim from the capture: 4 damage,
    /// physical, no overkill.
    const HIT: &[u8] = &[
        0x02, 0x00, 0x00, 0x00, 0x01, 0x32, 0xcb, 0xde, 0x0b, 0x06, 0x30, 0xf1, 0x04, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x40, 0x04,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    /// The wolf missing, verbatim: every damage figure zero, and the victim
    /// state zero where a landed swing reads one.
    const MISS: &[u8] = &[
        0x10, 0x00, 0x00, 0x00, 0xcb, 0xde, 0x0b, 0x06, 0x30, 0xf1, 0x01, 0x32, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    /// The killing blow, verbatim: a critical for 8 against a wolf with 1
    /// health left, so 7 of it was overkill.
    const KILL: &[u8] = &[
        0x02, 0x02, 0x00, 0x00, 0x01, 0x32, 0xcb, 0xde, 0x0b, 0x06, 0x30, 0xf1, 0x08, 0x00, 0x00,
        0x00, 0x07, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x41, 0x08,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn a_landed_swing_matches_a_captured_live_packet() {
        let swing = parse_melee_swing(HIT).expect("the captured packet must parse");
        assert_eq!(swing.attacker, ME);
        assert_eq!(swing.victim, WOLF);
        assert_eq!(swing.damage, 4);
        assert_eq!(swing.overkill, 0);
        assert!(!swing.missed());
        assert!(!swing.critical());
        assert_eq!(swing.victim_state, 1);
        assert_eq!(
            swing.damage_blocks,
            vec![SwingDamage {
                school_mask: 1,
                float_damage: 4.0,
                damage: 4,
            }]
        );
    }

    /// The float and the integer are two encodings of one number, so their
    /// agreeing is a free check that the block's fields are where this parser
    /// believes -- a shifted offset would break the agreement long before it
    /// broke the parse.
    #[test]
    fn the_two_damage_figures_agree_in_every_captured_swing() {
        for body in [HIT, MISS, KILL] {
            let swing = parse_melee_swing(body).unwrap();
            let summed: u32 = swing.damage_blocks.iter().map(|d| d.damage).sum();
            assert_eq!(summed, swing.damage, "block damage disagrees with the total");
            for block in &swing.damage_blocks {
                assert_eq!(block.float_damage, block.damage as f32);
            }
        }
    }

    #[test]
    fn a_miss_carries_a_damage_block_of_zeroes() {
        let swing = parse_melee_swing(MISS).expect("a miss must parse");
        assert_eq!(swing.attacker, WOLF);
        assert_eq!(swing.victim, ME);
        assert!(swing.missed());
        assert_eq!(swing.damage, 0);
        // The discriminator the 12-byte damage block was chosen on: this reads
        // one on every landed swing and zero here. Under the competing
        // reading it would be an absorbed amount, which would mean every
        // landed hit in the capture absorbed exactly one point.
        assert_eq!(swing.victim_state, 0);
        assert_eq!(swing.damage_blocks.len(), 1, "a miss still carries a block");
    }

    #[test]
    fn a_killing_blow_reports_its_overkill() {
        let swing = parse_melee_swing(KILL).expect("the killing blow must parse");
        assert!(swing.critical());
        assert_eq!(swing.damage, 8);
        assert_eq!(swing.overkill, 7, "the wolf had 1 health left");
        assert!(swing.overkilled());
    }

    /// The count is the one field that would separate the damage block's width
    /// from the packet's tail, and no capture has ever carried anything but
    /// one. Reading a different count as if the width were known is exactly
    /// the guess this parser exists to refuse.
    #[test]
    fn an_unconfirmed_block_count_is_refused_rather_than_guessed() {
        let mut body = HIT.to_vec();
        body[20] = 2;
        assert!(matches!(
            parse_melee_swing(&body),
            Err(Error::UnconfirmedSwingDamageBlocks { count: 2 })
        ));
    }

    /// Trailing bytes are an error, not slack. Four separate world-protocol
    /// bugs in this project were invisible field by field and obvious the
    /// moment a cursor reported leftovers.
    #[test]
    fn a_swing_with_bytes_left_over_is_an_error() {
        let mut body = HIT.to_vec();
        body.push(0);
        assert!(parse_melee_swing(&body).is_err());
        assert!(parse_melee_swing(&HIT[..HIT.len() - 1]).is_err());
    }

    /// The 46-byte shape, verbatim from a later capture: a kobold swinging
    /// for nothing at all, with the extra field reading 2.
    ///
    /// This packet is the reason the parser gates on the bit rather than on
    /// bytes remaining. It was seen once, printed only as "4 trailing bytes
    /// left unread", and lost -- the tool counted the refusal without keeping
    /// the body. Printing refused bodies caught four more in three fights.
    #[test]
    fn the_longer_swing_shape_is_gated_on_its_flag() {
        let body = [
            0x02, 0x20, 0x00, 0x00, 0xcb, 0xdd, 0x0b, 0x06, 0x30, 0xf1, 0x01, 0x32, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x02, 0x00, 0x00, 0x00,
        ];
        assert_eq!(body.len(), 46);
        let swing = parse_melee_swing(&body).expect("the 46-byte shape must parse");
        assert_eq!(swing.hit_info, 0x2002);
        assert_eq!(swing.victim, ME);
        assert_eq!(swing.damage, 0, "this swing dealt nothing");
        assert_eq!(swing.extra_amount, Some(2));

        // And the correlation the parser depends on: with the bit cleared the
        // same trailing bytes must be refused rather than absorbed, or a
        // shape of some other length would be read as this one.
        let mut without = body;
        without[1] = 0x00;
        assert!(
            parse_melee_swing(&without).is_err(),
            "the extra field was read without its flag"
        );
    }

    /// And the 42-byte shape must not grow one.
    #[test]
    fn the_usual_swing_shape_has_no_extra_field() {
        assert_eq!(parse_melee_swing(HIT).unwrap().extra_amount, None);
    }

    /// Verbatim from the capture. The kobold's list, with this client the
    /// only thing on it.
    #[test]
    fn a_threat_update_matches_a_captured_live_packet() {
        let body = [
            0xcb, 0xdd, 0x0b, 0x06, 0x30, 0xf1, 0x01, 0x00, 0x00, 0x00, 0x01, 0x32, 0x50, 0x05,
            0x00, 0x00,
        ];
        let threat = parse_threat_update(&body).expect("the captured packet must parse");
        assert_eq!(threat.victim, 0xf130_0000_0600_0bdd);
        assert_eq!(threat.entries, vec![(ME, 1360)]);
    }

    /// The count is read from the wire, so a packet claiming more entries than
    /// it carries must run out of input rather than be trusted.
    #[test]
    fn a_threat_update_that_overclaims_its_count_fails() {
        let body = [
            0xcb, 0xdd, 0x0b, 0x06, 0x30, 0xf1, 0xff, 0xff, 0xff, 0xff, 0x01, 0x32, 0x50, 0x05,
            0x00, 0x00,
        ];
        assert!(parse_threat_update(&body).is_err());
    }

    fn named(guid: u64) -> String {
        match guid {
            WOLF => "Young Wolf".to_string(),
            other => format!("Unit {other:#x}"),
        }
    }

    /// The three sentences a fight is actually made of, from the three
    /// captured packets that produced them.
    #[test]
    fn a_swing_reads_as_a_line_of_combat_log() {
        let hit = parse_melee_swing(HIT).unwrap();
        assert_eq!(describe_swing(&hit, ME, named), "You hit Young Wolf for 4.");

        let miss = parse_melee_swing(MISS).unwrap();
        assert_eq!(describe_swing(&miss, ME, named), "Young Wolf misses you.");

        let kill = parse_melee_swing(KILL).unwrap();
        assert_eq!(
            describe_swing(&kill, ME, named),
            "You hit Young Wolf for 8 (critical). Killing blow."
        );
    }

    /// Read by someone who is neither party, the same packet has to stay
    /// third-person rather than claiming the observer swung.
    #[test]
    fn a_bystander_sees_both_sides_named() {
        let hit = parse_melee_swing(HIT).unwrap();
        assert_eq!(
            describe_swing(&hit, 0x999, named),
            "Unit 0x32 hits Young Wolf for 4."
        );
    }

    /// A guid whose name has not arrived yet must still produce a line. Names
    /// resolve asynchronously, and a combat log that waited for one would drop
    /// the fight it was meant to record.
    #[test]
    fn an_unnamed_attacker_still_produces_a_line() {
        let miss = parse_melee_swing(MISS).unwrap();
        let line = describe_swing(&miss, ME, |guid| format!("Creature {guid:#x}"));
        assert!(line.ends_with("misses you."), "{line}");
    }

    /// Verbatim from the capture: the first `SMSG_ATTACKSTART`, this client
    /// opening on the wolf, with both guids unpacked.
    #[test]
    fn attack_start_matches_a_captured_live_packet() {
        let body = [
            0x32, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xde, 0x0b, 0x00, 0x06, 0x00, 0x00,
            0x30, 0xf1,
        ];
        assert_eq!(
            parse_attack_start(&body).unwrap(),
            AttackStart {
                attacker: ME,
                victim: WOLF,
            }
        );
    }

    /// And the wolf answering, which is the same packet with the guids the
    /// other way round -- the check that neither is hard-coded to a side.
    #[test]
    fn attack_start_reads_the_other_direction_too() {
        let body = [
            0xde, 0x0b, 0x00, 0x06, 0x00, 0x00, 0x30, 0xf1, 0x32, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ];
        assert_eq!(
            parse_attack_start(&body).unwrap(),
            AttackStart {
                attacker: WOLF,
                victim: ME,
            }
        );
    }

    /// `SMSG_ATTACKSTOP` packs its guids where `SMSG_ATTACKSTART` does not.
    #[test]
    fn attack_stop_matches_a_captured_live_packet() {
        let body = [
            0xcb, 0xde, 0x0b, 0x06, 0x30, 0xf1, 0x01, 0x32, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(
            parse_attack_stop(&body).unwrap(),
            AttackStop {
                attacker: WOLF,
                victim: ME,
                trailing: 0,
            }
        );
    }

    /// The two packets really do disagree about guid encoding, so reading one
    /// with the other's parser has to fail rather than produce two plausible
    /// wrong guids.
    #[test]
    fn the_two_attack_packets_do_not_share_an_encoding() {
        let packed_stop = [
            0xcb, 0xde, 0x0b, 0x06, 0x30, 0xf1, 0x01, 0x32, 0x00, 0x00, 0x00, 0x00,
        ];
        assert!(
            parse_attack_start(&packed_stop).is_err(),
            "a packed-guid body must not read as two raw guids"
        );
    }
}
