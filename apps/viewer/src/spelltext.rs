//! Filling in the numbers a spell description leaves blank.
//!
//! `Spell.dbc` does not store descriptions, it stores templates. Heroic Strike
//! is `"A strong attack that increases melee damage by $s1"`, Battle Shout is
//! `"...within $a1 yards by $s1.  Lasts $d."`, and the real client substitutes
//! each token from the spell's own effect columns and two index tables. Show
//! the string as stored and the player reads `$s1` where a number belongs.
//!
//! **An unresolved token is left exactly as written.** That is the whole
//! design rule here, and it is the same one `describe_cast_failure` follows:
//! this file resolves the constructs whose meaning was *confirmed against the
//! data* (see the column comments in `dbc::schema::Spell`, each of which
//! records the test that found it) and passes everything else through
//! untouched. A visible `$s1` tells the one person who can act on it that a
//! feature is missing. A number substituted from a column that was guessed at
//! is indistinguishable from a correct one, gets believed, and is wrong about
//! how much damage an ability does.
//!
//! What is deliberately *not* resolved, with the count of uses across the
//! 31,780 non-empty descriptions in build 12340:
//!
//! - `${$m1+0.15*$SPH}` arithmetic (1,731) -- needs the caster's spell power
//!   and attack power, which the tooltip has no access to here.
//! - `$<mult>` variables (191) -- a further table, `SpellDescriptionVariables`.
//! - `$?s12345[a][b]` conditionals (58), `$gmale:female;` (96) and
//!   `$lsingular:plural;` (53) -- these need the player, not the spell.
//! - `$h` proc chance, `$n` charges, `$x` chain targets, `$i` max targets,
//!   `$u` stacks, `$o` total-over-time: no column for any of them has been
//!   confirmed, so none of them is guessed at.
//!
//! Worth stating why `$m`/`$M` *is* implemented despite only 182 of its 1,296
//! uses sitting outside a `${...}` expression: the columns behind it fell out
//! of confirming `$s`, so it cost one array lookup. The other 1,114 uses stay
//! unresolved regardless, because the brace expression around them does.

use std::collections::HashMap;

/// The numbers one spell's description can refer to.
///
/// Read straight off the confirmed columns; the arithmetic that turns them
/// into what a player reads lives in [`resolve`] rather than in the loader, so
/// it can be tested without a game installation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Values {
    /// `effect_base_points`, as stored -- one *below* the displayed value.
    pub base: [i32; 3],
    pub die_sides: [i32; 3],
    /// Yards, already resolved through `SpellRadius`.
    pub radius: [f32; 3],
    /// Milliseconds between ticks.
    pub period: [i32; 3],
    /// Milliseconds, already resolved through `SpellDuration`.
    pub duration_ms: i32,
}

/// Fills in every token whose meaning is confirmed, and leaves the rest alone.
///
/// `spell` names the description's own spell, because a token may refer to a
/// *different* one: `Power Word: Shield` says `$6788d`, the duration of
/// `Weakened Soul`. That construct is 8,697 uses across the table and 98.9% of
/// its numbers are a real spell id, which is what makes it worth handling
/// rather than passing through.
pub fn substitute(text: &str, spell: u32, values: &HashMap<u32, Values>) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'$' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'$' {
                i += 1;
            }
            out.push_str(&text[start..i]);
            continue;
        }
        match parse(text, i, spell, values) {
            Some((replacement, next)) => {
                out.push_str(&replacement);
                i = next;
            }
            // Not something this file claims to understand. Copy the `$` and
            // carry on from the next byte, so an unknown construct survives
            // intact rather than being half-eaten.
            None => {
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

/// Every *other* spell a description's tokens refer to.
///
/// The loader needs this before it can resolve anything, because it reads
/// `Spell.dbc` scoped to the ids a character actually knows -- and
/// `Power Word: Shield` refers to `Weakened Soul`, which no character knows.
///
/// Deliberately in this file rather than in the loader: it has to agree with
/// [`substitute`] about what counts as a reference, and two scanners for one
/// grammar drift. Same rule as defining a both-ways structure once.
pub fn referenced_spells(text: &str, out: &mut std::collections::HashSet<u32>) {
    let bytes = text.as_bytes();
    for (i, _) in bytes.iter().enumerate().filter(|(_, b)| **b == b'$') {
        let rest = &text[i + 1..];
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            continue;
        }
        // A bare number is not a reference; it has to be followed by a token
        // letter, exactly as `parse` requires before it resolves one.
        if !rest[digits..].starts_with(|c: char| c.is_ascii_alphabetic()) {
            continue;
        }
        if let Ok(id) = rest[..digits].parse() {
            out.insert(id);
        }
    }
}

/// Reads one construct starting at `at`, which is known to be a `$`.
///
/// Returns the text to emit and where to resume, or `None` to leave the `$`
/// alone. A brace expression is refused here rather than parsed and dropped:
/// `${$m1*$<mult>}` has to reach the screen whole, or the reader cannot tell
/// an unimplemented multiplier from a missing one.
fn parse(
    text: &str,
    at: usize,
    spell: u32,
    values: &HashMap<u32, Values>,
) -> Option<(String, usize)> {
    let rest = &text[at + 1..];
    let mut chars = rest.char_indices();
    let (_, first) = chars.next()?;

    match first {
        '$' => return Some(("$".into(), at + 2)),
        // A brace expression is copied out whole, tokens and all. Skipping it
        // is not the same as ignoring it: without this the scanner walks *into*
        // the braces and resolves the `$m1` inside, turning
        // `${$m1+0.15*$SPH}` into `${11+0.15*$SPH}` -- which reads as a
        // finished sentence with one number in it and is wrong about the
        // other. Half-substituted is worse than untouched.
        '{' => {
            // The expressions in build 12340 nest parentheses, never braces.
            let end = at + 1 + rest.find('}')? + 1;
            // From `at`, not from `rest`: the `$` introducing the expression
            // is part of it.
            return Some((text[at..end].to_string(), end));
        }
        // A line break in the middle of a description.
        'b' | 'B' => return Some(("\n".into(), at + 2)),
        // `$/1000;s1` and `$*2;s1`: resolve the token, then scale it.
        '/' | '*' => {
            let end = rest.find(';')?;
            let divisor: f64 = rest[1..end].parse().ok()?;
            if divisor == 0.0 {
                return None;
            }
            let (value, next) = number_token(text, at + 1 + end + 1, spell, values)?;
            let scaled = if first == '/' { value / divisor } else { value * divisor };
            return Some((format_number(scaled), next));
        }
        // `$12345s1`: the same tokens, read off another spell's row.
        '0'..='9' => {
            let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            let other: u32 = rest[..digits].parse().ok()?;
            let (value, next) = value_token(text, at + 1 + digits, other, values)?;
            return Some((value, next));
        }
        _ => {}
    }
    value_token(text, at + 1, spell, values)
}

/// A bare token -- `s1`, `M2`, `a1`, `t1`, `d` -- resolved against one spell.
fn value_token(
    text: &str,
    at: usize,
    spell: u32,
    values: &HashMap<u32, Values>,
) -> Option<(String, usize)> {
    let value = values.get(&spell)?;
    let rest = text.get(at..)?;
    let letter = rest.chars().next()?;

    // `$d` takes no index; every other token needs one, and a token without
    // one is not a construct this file recognises.
    if letter == 'd' || letter == 'D' {
        // `$d` is a word: `$damage` in a description is prose, not a duration.
        if rest[1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
            return None;
        }
        if value.duration_ms <= 0 {
            return None;
        }
        return Some((format_duration(value.duration_ms), at + 1));
    }

    let index = rest[1..].chars().next()?.to_digit(10)? as usize;
    let slot = index.checked_sub(1).filter(|i| *i < 3)?;
    let next = at + 2;

    let text = match letter {
        // The displayed value is one above the stored one, and negative
        // values print positive: `Frostbolt` stores -41 for a slow its
        // description reads as "slowing movement speed by $s1%".
        's' | 'S' => format_number((value.base[slot] + 1).abs() as f64),
        'm' => format_number((value.base[slot] + 1).abs() as f64),
        'M' => format_number((value.base[slot] + value.die_sides[slot].max(1)).abs() as f64),
        'a' | 'A' => {
            if value.radius[slot] <= 0.0 {
                return None;
            }
            format_number(value.radius[slot] as f64)
        }
        't' | 'T' => {
            if value.period[slot] <= 0 {
                return None;
            }
            format_number(value.period[slot] as f64 / 1000.0)
        }
        _ => return None,
    };
    Some((text, next))
}

/// Like [`value_token`] but hands back the number, for `$/1000;s1` to scale.
fn number_token(
    text: &str,
    at: usize,
    spell: u32,
    values: &HashMap<u32, Values>,
) -> Option<(f64, usize)> {
    let (rendered, next) = value_token(text, at, spell, values)?;
    // Re-reading what `value_token` formatted keeps one definition of what
    // each token means. A duration renders as "2 min" and cannot be scaled,
    // which `parse` returns as unresolved rather than as a wrong number.
    Some((rendered.parse().ok()?, next))
}

/// Trims a number to what a tooltip should say: no decimal point on a whole
/// number, at most two places on anything else.
fn format_number(value: f64) -> String {
    if (value - value.round()).abs() < 1e-9 {
        format!("{}", value.round() as i64)
    } else {
        let text = format!("{value:.2}");
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Milliseconds as a tooltip reads them: `30 sec`, `2 min`, `1 hour`.
///
/// Deliberately not pluralised beyond `hours`. WoW's own descriptions carry
/// `$lsec:secs;` tokens precisely because the client cannot decide plurals
/// from the number alone, and those tokens are not resolved here -- inventing
/// a plural rule would disagree with the ones that are spelled out.
fn format_duration(ms: i32) -> String {
    let seconds = ms as f64 / 1000.0;
    if seconds < 60.0 {
        format!("{} sec", format_number(seconds))
    } else if seconds < 3600.0 {
        format!("{} min", format_number(seconds / 60.0))
    } else {
        let hours = seconds / 3600.0;
        let unit = if (hours - 1.0).abs() < 1e-9 { "hour" } else { "hours" };
        format!("{} {unit}", format_number(hours))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real rows for these spells, as read out of build 12340's
    /// `Spell.dbc` by the column indices recorded in `dbc::schema::Spell`.
    /// Pinned as literals so a change to those indices fails here rather than
    /// quietly printing different numbers.
    fn book() -> HashMap<u32, Values> {
        HashMap::from([
            // Heroic Strike rank 1: base 10, no duration, no radius.
            (78, Values { base: [10, 0, 0], ..Default::default() }),
            // Battle Shout rank 1: base 14, radius index 10 -> 30 yards,
            // duration index 4 -> 120000ms.
            (
                6673,
                Values {
                    base: [14, 14, 0],
                    radius: [30.0, 30.0, 0.0],
                    duration_ms: 120_000,
                    ..Default::default()
                },
            ),
            // Frostbolt rank 1: a slow stored negative.
            (116, Values { base: [-41, 17, 0], die_sides: [1, 3, 0], ..Default::default() }),
            // Weakened Soul, referred to by Power Word: Shield as `$6788d`.
            (6788, Values { duration_ms: 15_000, ..Default::default() }),
        ])
    }

    #[test]
    fn an_effect_value_is_one_above_what_the_file_stores() {
        assert_eq!(
            substitute(
                "A strong attack that increases melee damage by $s1.",
                78,
                &book()
            ),
            "A strong attack that increases melee damage by 11."
        );
    }

    /// `Frostbolt` stores -41 for a 40% slow. Printing the stored number would
    /// read "slowing movement speed by -40%", and printing it unbumped would
    /// read 41.
    #[test]
    fn a_reduction_prints_positive() {
        assert_eq!(
            substitute("slowing movement speed by $s1%", 116, &book()),
            "slowing movement speed by 40%"
        );
    }

    #[test]
    fn radius_and_duration_resolve_together() {
        assert_eq!(
            substitute(
                "increasing attack power of all raid and party members within $a1 yards by $s1.  Lasts $d.",
                6673,
                &book()
            ),
            "increasing attack power of all raid and party members within 30 yards by 15.  Lasts 2 min."
        );
    }

    /// 8,697 tokens in the table name a *different* spell, and 98.9% of those
    /// numbers are a real spell id.
    #[test]
    fn a_token_can_name_another_spell() {
        assert_eq!(
            substitute("cannot be cast on the target for $6788d.", 17, &book()),
            "cannot be cast on the target for 15 sec."
        );
    }

    #[test]
    fn a_scaled_token_divides() {
        assert_eq!(substitute("$/5;s1 seconds", 78, &book()), "2.2 seconds");
        assert_eq!(substitute("$*2;s1 damage", 78, &book()), "22 damage");
        // The scale applies to the resolved token, so an unresolvable one
        // takes the whole construct down with it rather than printing the
        // divisor's worth of nothing.
        assert_eq!(substitute("$/1000;d ticks", 78, &book()), "$/1000;d ticks");
    }

    #[test]
    fn a_range_reads_from_base_and_die_sides() {
        assert_eq!(substitute("$m2 to $M2 damage", 116, &book()), "18 to 20 damage");
    }

    /// The rule the whole file is built on: anything unconfirmed survives
    /// intact, including the `$` that introduces it.
    #[test]
    fn an_unimplemented_construct_is_left_exactly_as_written() {
        for text in [
            "causing ${$m1+0.15*$SPH} to ${$M1+0.15*$SPH} Holy damage",
            "for ${$m1*$<mult>} Frost damage",
            "$?s12345[one][another]",
            "$gHe:She; strikes",
            "$lsecond:seconds;",
            "restores $u charges",
        ] {
            assert_eq!(substitute(text, 78, &book()), text, "{text} was altered");
        }
    }

    /// A token naming a spell nothing is known about must not silently become
    /// a blank or a zero -- the description would then read as finished and
    /// say something false.
    #[test]
    fn an_unknown_spell_leaves_its_token_alone() {
        assert_eq!(
            substitute("lasts $999999d longer", 78, &book()),
            "lasts $999999d longer"
        );
        assert_eq!(substitute("increases by $s1", 4242, &book()), "increases by $s1");
    }

    /// A spell with no duration at all must not print "0 sec": the token is
    /// unresolvable, not zero.
    #[test]
    fn a_missing_value_is_not_printed_as_zero() {
        assert_eq!(substitute("Lasts $d.", 78, &book()), "Lasts $d.");
        assert_eq!(substitute("within $a1 yards", 78, &book()), "within $a1 yards");
    }

    /// `$damage` in prose is not a duration token followed by the word.
    #[test]
    fn a_word_beginning_with_d_is_not_a_duration() {
        assert_eq!(
            substitute("deals $damage to the target", 6673, &book()),
            "deals $damage to the target"
        );
    }

    #[test]
    fn durations_read_in_the_largest_whole_unit() {
        assert_eq!(format_duration(15_000), "15 sec");
        assert_eq!(format_duration(1_500), "1.5 sec");
        assert_eq!(format_duration(120_000), "2 min");
        assert_eq!(format_duration(3_600_000), "1 hour");
        assert_eq!(format_duration(7_200_000), "2 hours");
    }

    /// The reference scanner and the substituter have to agree about what a
    /// reference is, or the loader fetches rows nothing will ask for while the
    /// tokens that needed them stay unresolved.
    #[test]
    fn the_reference_scanner_finds_what_substitution_needs() {
        let text = "Shields for $6788d, and $12345s1 more, but not $99 or ${$m1}.";
        let mut found = std::collections::HashSet::new();
        referenced_spells(text, &mut found);
        assert_eq!(found, [6788, 12345].into_iter().collect());

        // With every id it named present, nothing of that shape is left over.
        let values = HashMap::from([
            (6788, Values { duration_ms: 15_000, ..Default::default() }),
            (12345, Values { base: [4, 0, 0], ..Default::default() }),
        ]);
        let done = substitute(text, 78, &values);
        assert!(
            !done.contains("$6788") && !done.contains("$12345"),
            "a reference the scanner reported was not resolved: {done}"
        );
    }

    #[test]
    fn a_literal_dollar_survives() {
        assert_eq!(substitute("costs $$5", 78, &book()), "costs $5");
    }

    #[test]
    fn a_line_break_token_becomes_one() {
        assert_eq!(substitute("first$bsecond", 78, &book()), "first\nsecond");
    }
}
