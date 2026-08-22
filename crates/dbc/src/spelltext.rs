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
//! What is deliberately *not* resolved:
//!
//! - `${$m1+0.15*$SPH}` arithmetic -- needs the caster's spell power and
//!   attack power, which the tooltip has no access to here.
//! - `$<mult>` variables -- a further table, `SpellDescriptionVariables`.
//! - `$?s12345[a][b]` conditionals, `$gmale:female;` and `$lsingular:plural;`
//!   -- these need the player, not the spell.
//! - `$h` proc chance, `$n` charges, `$x` chain targets, `$i` max targets,
//!   `$u` stacks, and several rarer letters besides: no column for any of
//!   them has been confirmed, so none of them is guessed at.
//!
//! **`$o` is the one exception, and it is arithmetic rather than a column.**
//! A periodic effect's total is `$s` (one above the stored base) times the
//! number of ticks the confirmed `period` and `duration` columns produce --
//! nothing here is a fourth unconfirmed number, it is the same three
//! multiplied together. Foss-wow#143: this was left unresolved for a
//! session first, on the reasoning that the *formula* had not been checked
//! against this client's own data the way every other token here was.
//!
//! **The first version of that formula was still wrong, and the wrongness
//! was instructive.** Every "Food" and "Drink" spell (433, 434, 435, and
//! `Drink`'s mana effect) stores `effect_aura_period: 0` on the very effect
//! `$o` reads from, so a period-must-be-positive guard leaves the most
//! common user of this token permanently unresolved -- the exact case Kake
//! asked about. **A stored zero is not "not periodic", it is "the engine's
//! own default applies"**: AzerothCore's `AuraEffect::CalculatePeriodic`
//! (`SpellAuraEffects.cpp:607`) sets `m_amplitude = 1 * IN_MILLISECONDS`
//! whenever the column reads zero, before dividing the aura's duration by
//! it for the tick count actually applied in play -- confirmed by reading
//! the source per `CLAUDE.md` rule 2, not by guessing at a nicer-sounding
//! constant. (A first guess of a 5-second tick, prompted by the short
//! `tooltip` column's `$/5;s1`, was refuted by `Drink`'s own mana effect:
//! its real period is 2200ms and its short tooltip *also* says `/5`, so
//! that divisor is fixed authoring boilerplate and not a real interval.)
//!
//! **Frequency counts deliberately do not live in this comment.** An earlier
//! version of it hand-counted uses per construct, and running [`scan`] for
//! real turned up letters that count had never named at all and put its
//! `$l` estimate off by nearly seven times -- a stale number in a doc
//! comment is exactly the "confidently wrong" failure this file's own
//! design rule exists to avoid, one level up. `wow-cli spell tokens` reads
//! the real counts off whichever build's data is loaded, which a comment
//! cannot do once a new patch changes them.
//!
//! Worth stating why `$m`/`$M` *is* implemented despite most of their uses
//! sitting inside a `${...}` expression this file already refuses: the
//! columns behind it fell out of confirming `$s`, so resolving the ones
//! outside braces cost one array lookup. The ones inside stay unresolved
//! regardless, because the brace expression around them does.

use std::collections::HashMap;

/// Reads one spell's numbers, resolving the two effect columns that are
/// stored as an index into another table rather than as a value.
///
/// An index that names no row resolves to nothing rather than to zero, so a
/// token backed by missing data stays visible instead of claiming "0 sec".
/// Shared between the viewer's tooltip loader and `wow-cli spell tokens`,
/// which both need the exact same mapping -- a second copy here is exactly
/// the "two scanners for one grammar" risk this module's own doc comment
/// warns about, just one level removed from token parsing.
pub fn values_from_row(
    row: &crate::schema::SpellRow<'_>,
    durations: &HashMap<u32, i32>,
    radii: &HashMap<u32, f32>,
) -> Values {
    Values {
        base: [
            row.effect_base_points(),
            row.effect_base_points_2(),
            row.effect_base_points_3(),
        ],
        die_sides: [
            row.effect_die_sides(),
            row.effect_die_sides_2(),
            row.effect_die_sides_3(),
        ],
        radius: [
            radii.get(&row.effect_radius_index()).copied().unwrap_or(0.0),
            radii.get(&row.effect_radius_index_2()).copied().unwrap_or(0.0),
            radii.get(&row.effect_radius_index_3()).copied().unwrap_or(0.0),
        ],
        period: [
            row.effect_aura_period(),
            row.effect_aura_period_2(),
            row.effect_aura_period_3(),
        ],
        duration_ms: durations.get(&row.duration_index()).copied().unwrap_or(0),
    }
}

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

/// One `$`-construct found in a description, for counting rather than for
/// display -- see [`scan`] and `wow-cli spell tokens`.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenHit {
    /// A grouping key such as `"$s"`, `"$d"`, `"${...}"`, `"$?[a][b]"` --
    /// never the exact text, so `$s1` and `$s2` land in the same row of a
    /// frequency table instead of two.
    pub bucket: String,
    /// The construct's own source text for this one occurrence, e.g. `$s1`
    /// or `${$m1+0.15*$SPH}`. Exact when [`parse`] recognised the construct;
    /// a best-effort sample otherwise, since nothing here then knows where
    /// the construct actually ends -- see [`scan`]'s doc comment.
    pub raw: String,
    /// Whether `substitute` would print something other than this text
    /// verbatim. Never decided by re-deriving the grammar: this is `true`
    /// exactly when the real [`parse`] returned a replacement that differs
    /// from the source, so it cannot disagree with what a tooltip actually
    /// shows.
    pub resolved: bool,
}

/// Every `$`-construct in `text`, in the order they appear -- a reporting
/// tool, not a second implementation of [`substitute`].
///
/// The resolved/unresolved half of each [`TokenHit`] comes from calling the
/// identical [`parse`] that [`substitute`] calls on the identical bytes, so
/// it cannot drift from what a tooltip actually does -- the risk
/// [`referenced_spells`]'s doc comment already names for a smaller case.
/// What this function adds on top is purely cosmetic: a human-readable
/// `bucket` label and, for a construct [`parse`] does not recognise at all,
/// a best-effort guess at where it ends (nothing authoritative exists for
/// that case, because [`parse`] itself does not know). A wrong guess there
/// misgroups one row of a frequency table; it can never misinform a player,
/// which is the bar the rest of this file holds itself to.
pub fn scan(text: &str, spell: u32, values: &HashMap<u32, Values>) -> Vec<TokenHit> {
    let bytes = text.as_bytes();
    let mut hits = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        // A literal `$$` is an escape, not a token -- excluded so it does
        // not inflate a count of things that were never a construct at all.
        if bytes.get(i + 1) == Some(&b'$') {
            i += 2;
            continue;
        }
        match parse(text, i, spell, values) {
            Some((replacement, next)) => {
                let raw = &text[i..next];
                hits.push(TokenHit {
                    bucket: bucket_of(raw),
                    resolved: replacement != raw,
                    raw: raw.to_string(),
                });
                i = next;
            }
            None => {
                let end = sample_boundary(text, i);
                let raw = &text[i..end];
                hits.push(TokenHit {
                    bucket: bucket_of(raw),
                    raw: raw.to_string(),
                    resolved: false,
                });
                // Advancing past only the `$` itself, never past the guessed
                // sample, is what keeps this safe without a real boundary:
                // whatever character follows a `$` this file does not
                // recognise is, by definition, not itself a fresh `$`, so
                // the outer loop's plain-text scan naturally skips the rest
                // of the construct without this function having to know
                // where it ends.
                i += 1;
            }
        }
    }
    hits
}

/// A grouping key for a `$`-construct's *kind*, discarding the index and any
/// spell id it names. Used only for display -- see [`TokenHit::bucket`].
fn bucket_of(raw: &str) -> String {
    let rest = raw.strip_prefix('$').unwrap_or(raw);
    // `$6788d` and `$12345s1` are cross-spell references -- the same kind of
    // token as `$d` and `$s1`, just borrowing another row's numbers, so the
    // leading digits are not part of the bucket.
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    match rest[digits..].chars().next() {
        Some('{') => "${...}".to_string(),
        Some('?') => "$?[a][b]".to_string(),
        Some('<') => "$<mult>".to_string(),
        Some('/') => "$/n;...".to_string(),
        Some('*') => "$*n;...".to_string(),
        Some(c) => format!("${c}"),
        None => "$".to_string(),
    }
}

/// A best-effort end for a construct [`parse`] refused, purely so [`scan`]
/// has *something* readable to show as an example -- never authoritative,
/// since a construct this file does not recognise has no boundary this file
/// can actually know. Cuts at the first of: whitespace or the sentence
/// running out (a bare unconfirmed letter like `$h` has no closing
/// punctuation at all), `;` (closes a scaled or gendered/pluralised token),
/// `]` (a conditional's second bracket) or `>` (a `$<mult>` variable) --
/// whichever comes first, and never past a fixed cap so one malformed
/// description cannot make one sample swallow the rest of the table's worth
/// of text.
fn sample_boundary(text: &str, at: usize) -> usize {
    const MAX_SAMPLE: usize = 24;
    let rest = &text[at + 1..];
    let cut = match rest.find(|c: char| c == ';' || c == ']' || c == '>' || c == '$' || c.is_whitespace()) {
        // `;`, `]` and `>` close the construct, so the sample includes them.
        // Whitespace and `$` only *stop* the search -- they belong to
        // whatever comes next, not to this token.
        Some(p) if matches!(rest.as_bytes()[p], b';' | b']' | b'>') => p + 1,
        Some(p) => p,
        None => rest.len(),
    };
    at + 1 + cut.min(rest.len()).min(MAX_SAMPLE)
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
        // See the module comment: not a fourth unconfirmed column, the same
        // three `$s`, `$t` and `$d` already use, multiplied together.
        //
        // **A stored period of zero is not "no ticking".** Every "Food" and
        // "Drink" spell in build 12340 (433, 434, 435, and `Drink`'s mana
        // effect) carries `effect_aura_period: 0` on the very effect their
        // own description's `$o1` refers to -- confirmed against
        // AzerothCore's `AuraEffect::CalculatePeriodic`
        // (`SpellAuraEffects.cpp:607`), which defaults `m_amplitude` to
        // exactly `1 * IN_MILLISECONDS` whenever the column reads zero,
        // before dividing the aura's duration by it (integer division) to
        // get the tick count actually applied in play. This is an
        // aura-engine constant, not a per-spell guess, and only `$o`
        // defaults it -- `$t` still reports a genuinely absent period as
        // unresolved, because nothing here has verified retail shows "1
        // sec" rather than nothing for a `$t` on a zero-period effect.
        'o' | 'O' => {
            if value.duration_ms <= 0 {
                return None;
            }
            let period = if value.period[slot] > 0 { value.period[slot] } else { 1000 };
            let ticks = value.duration_ms / period;
            format_number((value.base[slot] + 1).abs() as f64 * ticks as f64)
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

    /// `$o` is arithmetic over the three columns `$s`, `$t` and `$d` already
    /// use, not a fourth captured one -- see the module comment. Synthetic
    /// rather than a row out of `book()`, because none of the real captures
    /// there carry a periodic effect to exercise it with: 3 sec between
    /// ticks over a 12 sec duration is 4 ticks of (9 + 1), which is 40.
    #[test]
    fn total_over_time_multiplies_the_per_tick_amount_by_the_tick_count() {
        let values = HashMap::from([(
            27636,
            Values {
                base: [9, 0, 0],
                period: [3_000, 0, 0],
                duration_ms: 12_000,
                ..Default::default()
            },
        )]);
        assert_eq!(
            substitute("Restores $o1 health over $d.", 27636, &values),
            "Restores 40 health over 12 sec."
        );
    }

    /// Without a duration there is no tick count regardless of the period,
    /// so `$o` must stay a visible token rather than print "0 health" -- the
    /// same rule `a_missing_value_is_not_printed_as_zero` holds `$d` and
    /// `$a` to.
    #[test]
    fn total_over_time_is_unresolved_without_a_duration() {
        assert_eq!(
            substitute("Restores $o1 health.", 78, &book()),
            "Restores $o1 health."
        );
    }

    /// The real numbers off `Spell.dbc` row 433, "Food": base 16 (displayed
    /// as 17), `effect_aura_period` **zero**, duration 18000ms. Pinned as a
    /// literal for the same reason `book()`'s rows are, and kept separate
    /// from it because this is the case the module comment's AzerothCore
    /// citation exists to justify: a zero period defaults to a 1000ms tick,
    /// so 18 ticks of 17 is 306.
    #[test]
    fn a_zero_period_defaults_to_a_one_second_tick_the_way_the_real_engine_does() {
        let values = HashMap::from([(
            433,
            Values { base: [16, 0, 0], duration_ms: 18_000, ..Default::default() },
        )]);
        assert_eq!(
            substitute("Restores $o1 health over $d.", 433, &values),
            "Restores 306 health over 18 sec."
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

    /// `scan` and `substitute` must agree on which tokens resolve, or the
    /// report `scan` exists to build would describe a client that does not
    /// exist. Every case here has a matching assertion above through
    /// `substitute` directly.
    #[test]
    fn scan_agrees_with_substitute_about_what_resolves() {
        let hits = scan("A strong attack that increases melee damage by $s1.", 78, &book());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].bucket, "$s");
        assert_eq!(hits[0].raw, "$s1");
        assert!(hits[0].resolved);

        let hits = scan("Lasts $d.", 78, &book());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].bucket, "$d");
        assert!(!hits[0].resolved, "spell 78 has no duration to resolve $d from");
    }

    /// A brace expression is *recognised* -- `parse` returns `Some` for it,
    /// copying it through whole -- but that is not the same claim as
    /// resolved, and `scan` must not conflate the two the way a check of
    /// "did `parse` return `Some`" alone would.
    #[test]
    fn a_brace_expression_is_a_structural_bucket_and_not_resolved() {
        let hits = scan("causing ${$m1+0.15*$SPH} damage", 78, &book());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].bucket, "${...}");
        assert_eq!(hits[0].raw, "${$m1+0.15*$SPH}");
        assert!(!hits[0].resolved);
    }

    /// The three structural forms the ticket asks to be counted separately
    /// from plain tokens each land in their own bucket.
    #[test]
    fn structural_forms_bucket_separately() {
        assert_eq!(bucket_of("${$m1}"), "${...}");
        assert_eq!(bucket_of("$?s12345[a][b]"), "$?[a][b]");
        assert_eq!(bucket_of("$gHe:She;"), "$g");
        assert_eq!(bucket_of("$lsecond:seconds;"), "$l");
        assert_eq!(bucket_of("$<mult>"), "$<mult>");
    }

    /// A cross-spell reference buckets on the token it borrows, not on the
    /// id it names -- `$6788d` and `$12345s1` are the same *kind* of thing
    /// as `$d` and `$s1`, and a report that gave every id its own row would
    /// be thousands of rows of noise instead of one useful one.
    #[test]
    fn a_cross_spell_reference_buckets_on_its_token_not_its_id() {
        assert_eq!(bucket_of("$6788d"), "$d");
        assert_eq!(bucket_of("$12345s1"), "$s");
    }

    /// An unconfirmed bare letter has no closing punctuation at all, so the
    /// sample must stop at the next whitespace rather than swallowing the
    /// rest of the sentence.
    #[test]
    fn an_unconfirmed_letter_samples_only_the_token() {
        let hits = scan("restores $u charges over time", 78, &book());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].bucket, "$u");
        assert_eq!(hits[0].raw, "$u");
        assert!(!hits[0].resolved);
    }

    /// `$$` is a literal-dollar escape, not a construct, and must not appear
    /// in a token frequency report at all.
    #[test]
    fn a_literal_dollar_is_not_a_scanned_token() {
        assert!(scan("costs $$5", 78, &book()).is_empty());
    }
}
