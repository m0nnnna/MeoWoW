//! Resolving the lighting that applies somewhere, at some hour.
//!
//! Its own module because the chain has four tables in it and reads badly
//! inline: a position picks a [`Light`], a condition picks a [`LightParams`],
//! and that owns eighteen colour curves and six scalar ones, addressed by
//! arithmetic rather than by a column.
//!
//! **Nothing here names what a band means.** Eighteen colours arrive in a fixed
//! order and this client has confirmed none of their meanings against the data,
//! so they are printed by index with their behaviour beside them. Naming them
//! from memory is the mistake `describe_cast_failure` exists to avoid -- a wrong
//! offset eventually fails loudly, a wrong *name* just misexplains for ever.
//! What the printout gives instead is the evidence to identify them: how each
//! band moves across the day.

use anyhow::{Context, Result};
use dbc::schema::{
    float_band_id, int_band_id, Light, LightFloatBand, LightIntBand, LightParams,
    DAY_HALF_MINUTES, FLOAT_BANDS_PER_PARAMS, INT_BANDS_PER_PARAMS,
};
use mpq::Chain;

/// Unpacks a band colour. **Blue is the low byte.**
///
/// Which way round the bytes go is exactly the sort of thing that produces a
/// blue sky or an orange one with equal confidence, so it was settled by what
/// the data *does* across a day rather than by a remembered convention. Sampling
/// one of Azeroth's sky bands at midnight, dawn and noon gives, blue-first:
///
/// | | midnight | dawn | noon |
/// |---|---|---|---|
/// | blue first | (0, 12, 32) near-black blue | (255, 171, 64) orange | (58, 162, 207) sky blue |
/// | red first | (32, 12, 0) dark brown | (64, 171, 255) blue | (207, 162, 58) ochre |
///
/// One of those is a sky and the other is not. Three bands agree, and the
/// disagreement is largest exactly where it is most obvious -- a sunrise.
fn colour(packed: u32) -> (u8, u8, u8) {
    (
        ((packed >> 16) & 0xFF) as u8,
        ((packed >> 8) & 0xFF) as u8,
        (packed & 0xFF) as u8,
    )
}

fn describe_colour(packed: u32) -> String {
    let (r, g, b) = colour(packed);
    format!("{packed:#08x}  rgb({r:>3},{g:>3},{b:>3})")
}

/// Samples a curve at a time of day, holding at the ends and interpolating
/// between keys.
///
/// Wrapping rather than clamping at the end of the day would be wrong: the
/// curves are authored with a key at or near midnight and the client is
/// expected to hold the last value until the first, which is what "the sky
/// stops changing at 23:00" would otherwise look like.
fn sample_int(band: &dbc::schema::LightIntBandRow<'_>, at: u32) -> Option<u32> {
    let count = band.count() as usize;
    if count == 0 {
        return None;
    }
    let mut previous = band.key(0)?;
    if at <= previous.0 {
        return Some(previous.1);
    }
    for index in 1..count {
        let key = band.key(index)?;
        if at <= key.0 {
            let span = key.0.saturating_sub(previous.0).max(1);
            let t = (at - previous.0) as f32 / span as f32;
            return Some(blend(previous.1, key.1, t));
        }
        previous = key;
    }
    Some(previous.1)
}

/// Blends two packed colours channel by channel.
fn blend(from: u32, to: u32, t: f32) -> u32 {
    let mut out = 0u32;
    for shift in [0, 8, 16] {
        let a = ((from >> shift) & 0xFF) as f32;
        let b = ((to >> shift) & 0xFF) as f32;
        let value = (a + (b - a) * t).round().clamp(0.0, 255.0) as u32;
        out |= value << shift;
    }
    out
}

fn sample_float(band: &dbc::schema::LightFloatBandRow<'_>, at: u32) -> Option<f32> {
    let count = band.count() as usize;
    if count == 0 {
        return None;
    }
    let mut previous = band.key(0)?;
    if at <= previous.0 {
        return Some(previous.1);
    }
    for index in 1..count {
        let key = band.key(index)?;
        if at <= key.0 {
            let span = key.0.saturating_sub(previous.0).max(1) as f32;
            let t = (at - previous.0) as f32 / span;
            return Some(previous.1 + (key.1 - previous.1) * t);
        }
        previous = key;
    }
    Some(previous.1)
}

/// Prints the lighting that applies at a position on a map, at an hour.
///
/// `hour` is in game hours; the realm's own clock comes from
/// `SMSG_LOGIN_SETTIMESPEED` and is printed by `wow-cli world --enter`.
pub fn report(chain: &mut Chain, map_id: u32, x: f32, y: f32, hour: f32) -> Result<()> {
    let lights = Light::parse(&chain.read(Light::PATH)?)?;
    let params_table = LightParams::parse(&chain.read(LightParams::PATH)?)?;
    let int_bands = LightIntBand::parse(&chain.read(LightIntBand::PATH)?)?;
    let float_bands = LightFloatBand::parse(&chain.read(LightFloatBand::PATH)?)?;

    let at = ((hour / 24.0) * DAY_HALF_MINUTES as f32).rem_euclid(DAY_HALF_MINUTES as f32) as u32;
    println!("map {map_id} at {x:.1}, {y:.1}, hour {hour:.2} (band time {at} of {DAY_HALF_MINUTES})");

    // Every light on this map, nearest first, with the default -- the one whose
    // radius covers everything -- called out. Printing the runners-up matters:
    // "the wrong light was chosen" and "the right light has these colours" look
    // identical from one row.
    let mut candidates: Vec<(f32, dbc::schema::LightRow<'_>)> = lights
        .iter()
        .filter(|row| row.map_id() == map_id)
        .map(|row| {
            let (dx, dy) = (row.x() - x, row.y() - y);
            ((dx * dx + dy * dy).sqrt(), row)
        })
        .collect();
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));

    println!("\n  {} light(s) on this map:", candidates.len());
    for (distance, row) in candidates.iter().take(6) {
        let global = row.x() == 0.0 && row.y() == 0.0 && row.z() == 0.0;
        println!(
            "    light {:<6} at {:>9.1},{:>9.1}  falloff {:.0}..{:.0}  {}{}",
            row.id(),
            row.x(),
            row.y(),
            row.falloff_start(),
            row.falloff_end(),
            if global {
                "default for the map".to_string()
            } else {
                format!("{distance:.0} away")
            },
            if *distance <= row.falloff_end() { "  <- contains this point" } else { "" },
        );
    }

    // The nearest light whose outer radius reaches the point, falling back to
    // the map default. A position covered by nothing at all is a real state --
    // say so rather than lighting it arbitrarily.
    let chosen = candidates
        .iter()
        .find(|(distance, row)| *distance <= row.falloff_end() && row.falloff_end() > 0.0)
        .or_else(|| {
            candidates
                .iter()
                .find(|(_, row)| row.x() == 0.0 && row.y() == 0.0 && row.z() == 0.0)
        })
        .context("no light on this map covers that point, and it has no default")?;

    let params_id = chosen.1.params_clear();
    println!(
        "\n  using light {} -> LightParams {params_id} (the clear-weather set)",
        chosen.1.id()
    );
    if let Some(params) = params_table.iter().find(|row| row.id() == params_id) {
        println!(
            "    glow {:.3}, skybox {}, highlight sky {}",
            params.glow(),
            params.light_skybox_id(),
            params.highlight_sky()
        );
    }

    // The whole point of the command: every band, its value now, and how it
    // moves across the day. A band that is dark at midnight and bright at noon
    // is a daylight colour; one that never changes is not.
    println!("\n  colour bands (18), sampled now and at four hours of the day:");
    println!(
        "    {:<5} {:<8} {:<30} {}",
        "band", "keys", "now", "00:00 / 06:00 / 12:00 / 18:00 brightness"
    );
    for band in 0..INT_BANDS_PER_PARAMS {
        let id = int_band_id(params_id, band);
        let Some(row) = int_bands.iter().find(|row| row.id() == id) else {
            println!("    {band:<5} <no row {id}>");
            continue;
        };
        let now = sample_int(&row, at);
        let across: Vec<String> = [0.0, 6.0, 12.0, 18.0]
            .iter()
            .map(|h| {
                let t = ((h / 24.0) * DAY_HALF_MINUTES as f32) as u32;
                match sample_int(&row, t) {
                    // Sum of channels: a crude brightness, but enough to show
                    // which bands follow the sun and which do not.
                    Some(c) => {
                        let (r, g, b) = colour(c);
                        let sum = u32::from(r) + u32::from(g) + u32::from(b);
                        format!("{sum:>4}")
                    }
                    None => "   -".into(),
                }
            })
            .collect();
        println!(
            "    {band:<5} {:<8} {:<30} {}",
            row.count(),
            now.map(describe_colour).unwrap_or_else(|| "empty".into()),
            across.join(" /")
        );
    }

    println!("\n  scalar bands (6):");
    for band in 0..FLOAT_BANDS_PER_PARAMS {
        let id = float_band_id(params_id, band);
        let Some(row) = float_bands.iter().find(|row| row.id() == id) else {
            println!("    {band:<5} <no row {id}>");
            continue;
        };
        let across: Vec<String> = [0.0, 6.0, 12.0, 18.0]
            .iter()
            .map(|h| {
                let t = ((h / 24.0) * DAY_HALF_MINUTES as f32) as u32;
                sample_float(&row, t)
                    .map(|v| format!("{v:>10.2}"))
                    .unwrap_or_else(|| "         -".into())
            })
            .collect();
        println!(
            "    {band:<5} {:<8} now {:>10.2}   across the day {}",
            row.count(),
            sample_float(&row, at).unwrap_or(f32::NAN),
            across.join(" /")
        );
    }
    Ok(())
}
