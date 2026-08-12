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
