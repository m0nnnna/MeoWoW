//! Levelling up. One packet, `SMSG_LEVELUP_INFO`, sent to the character it
//! happened to and nobody else -- a level-up is personal stat gains, not
//! something the wire hands to a bystander the way a monster's death cry is.
//!
//! **This is an event, not a field.** Nothing about "you levelled up a
//! moment ago" survives to be read later; it exists for exactly the frame it
//! arrives on, the same shape as [`crate::chat::ChatMessage`] -- see
//! `Replication::level_ups`.

/// A parsed `SMSG_LEVELUP_INFO`.
///
/// Layout confirmed against the server this project develops against
/// (`C:\azerothcore-wotlk`, `WorldPackets::Misc::LevelUpInfo::Write`): a
/// level, a health delta, `MAX_POWERS` (7) power deltas and `MAX_STATS` (5)
/// stat deltas, all `u32`, for 56 bytes exactly -- which is also the size the
/// packet's own constructor declares. The order names itself: `SharedDefines.h`
/// lists the seven powers as mana, rage, focus, energy, happiness, rune,
/// runic power, and the five stats as strength, agility, stamina, intellect,
/// spirit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LevelUp {
    pub new_level: u32,
    pub health_delta: u32,
    /// Mana, rage, focus, energy, happiness, rune, runic power, in that
    /// order -- see the module doc.
    pub power_delta: [u32; 7],
    /// Strength, agility, stamina, intellect, spirit, in that order.
    pub stat_delta: [u32; 5],
}

/// Parses `SMSG_LEVELUP_INFO`: fourteen `u32`s, 56 bytes exactly.
///
/// Both running out and having bytes left over are errors -- the rule every
/// parser in this crate follows, because a wrong stride here would read a
/// stat gain out of the middle of the next field and still produce a number
/// that looks plausible.
pub fn parse(body: &[u8]) -> Result<LevelUp, crate::protocol::Error> {
    const FIELDS: usize = 1 + 1 + 7 + 5;
    const WANT: usize = FIELDS * 4;
    if body.len() < WANT {
        return Err(crate::protocol::Error::Truncated {
            what: "SMSG_LEVELUP_INFO",
            at: 0,
            need: WANT,
            len: body.len(),
        });
    }
    if body.len() > WANT {
        return Err(crate::protocol::Error::Trailing {
            what: "SMSG_LEVELUP_INFO",
            got: body.len() - WANT,
        });
    }
    let mut values = body.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap()));
    let mut next = || values.next().expect("checked length above");
    let new_level = next();
    let health_delta = next();
    let power_delta = std::array::from_fn(|_| next());
    let stat_delta = std::array::from_fn(|_| next());
    Ok(LevelUp {
        new_level,
        health_delta,
        power_delta,
        stat_delta,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_of(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    /// A real-shaped body: level 6, a health gain, a mana gain (the only
    /// nonzero power for a caster), and a stamina/intellect gain.
    #[test]
    fn reads_a_levelup() {
        let mut values = vec![6, 12];
        values.extend([3, 0, 0, 0, 0, 0, 0]); // power_delta: mana only
        values.extend([0, 0, 2, 1, 0]); // stat_delta: stamina, intellect
        let parsed = parse(&body_of(&values)).expect("56-byte body");
        assert_eq!(parsed.new_level, 6);
        assert_eq!(parsed.health_delta, 12);
        assert_eq!(parsed.power_delta, [3, 0, 0, 0, 0, 0, 0]);
        assert_eq!(parsed.stat_delta, [0, 0, 2, 1, 0]);
    }

    /// Both directions of the length rule, on the one field count this
    /// packet actually has.
    #[test]
    fn a_body_of_the_wrong_length_is_an_error() {
        assert!(parse(&body_of(&[0; 13])).is_err(), "short body accepted");
        assert!(parse(&body_of(&[0; 15])).is_err(), "trailing bytes accepted");
    }

    /// The field order is what a caller reads a specific stat off of, so a
    /// swapped pair would parse cleanly and hand back the wrong number --
    /// pin each of the twelve trailing fields to a distinct value so a
    /// transposition anywhere shows up as a mismatch rather than as two
    /// zeros agreeing with each other.
    #[test]
    fn every_field_lands_in_its_own_slot() {
        let values: Vec<u32> = (1..=14).collect();
        let parsed = parse(&body_of(&values)).unwrap();
        assert_eq!(parsed.new_level, 1);
        assert_eq!(parsed.health_delta, 2);
        assert_eq!(parsed.power_delta, [3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(parsed.stat_delta, [10, 11, 12, 13, 14]);
    }
}
