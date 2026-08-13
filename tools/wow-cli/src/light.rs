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
use dbc::light::Lighting;
use dbc::schema::{DAY_HALF_MINUTES, FLOAT_BANDS_PER_PARAMS, INT_BANDS_PER_PARAMS};
use mpq::Chain;

fn describe_colour(rgb: [f32; 3]) -> String {
    let [r, g, b] = rgb.map(|c| (c * 255.0).round() as u32);
    format!("rgb({r:>3},{g:>3},{b:>3})")
}

/// Prints the lighting that applies at a position on a map, at an hour.
///
/// `hour` is in game hours; the realm's own clock comes from
/// `SMSG_LOGIN_SETTIMESPEED` and is printed by `wow-cli world --enter`.
pub fn report(chain: &mut Chain, map_id: u32, x: f32, y: f32, hour: f32) -> Result<()> {
    let lighting = Lighting::load(|path| chain.read(path).ok())
        .context("could not read the lighting tables")?;

    let at = ((hour / 24.0) * DAY_HALF_MINUTES as f32).rem_euclid(DAY_HALF_MINUTES as f32) as u32;
    println!("map {map_id} at {x:.1}, {y:.1}, hour {hour:.2} (band time {at} of {DAY_HALF_MINUTES})");

    // Every light on this map, nearest first, with the map's default called
    // out. Printing the runners-up matters: "the wrong light was chosen" and
    // "the right light has these colours" look identical from one row.
    let mut candidates: Vec<(f32, u32, f32, f32, bool)> = lighting
        .lights()
        .iter()
        .filter(|row| row.map_id() == map_id)
        .map(|row| {
            let (dx, dy) = (row.x() - x, row.y() - y);
            let global = row.x() == 0.0 && row.y() == 0.0 && row.z() == 0.0;
            (
                (dx * dx + dy * dy).sqrt(),
                row.id(),
                row.falloff_start(),
                row.falloff_end(),
                global,
            )
        })
        .collect();
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));

    println!("
  {} light(s) on this map:", candidates.len());
    for (distance, id, start, end, global) in candidates.iter().take(6) {
        println!(
            "    light {id:<6} falloff {start:.0}..{end:.0}  {}{}",
            if *global {
                "default for the map".to_string()
            } else {
                format!("{distance:.0} away")
            },
            if !global && distance <= end { "  <- contains this point" } else { "" },
        );
    }

    // Resolved through the same code the renderer uses. A verification tool
    // that computed this its own way would stop being evidence about the
    // renderer the moment either drifted -- see the module docs.
    let (params_id, distance) = lighting
        .params_at(map_id, x, y)
        .context("no light on this map covers that point, and it has no default")?;
    println!(
        "
  using LightParams {params_id} (clear weather), from a light {}",
        if distance.is_finite() {
            format!("{distance:.0} away")
        } else {
            "that is the map default".into()
        }
    );
    if let Some(params) = lighting.params().iter().find(|row| row.id() == params_id) {
        println!(
            "    glow {:.3}, skybox {}, highlight sky {}",
            params.glow(),
            params.light_skybox_id(),
            params.highlight_sky()
        );
    }

    let sample = lighting
        .sample(map_id, x, y, (at / 2) as u32)
        .context("nothing to sample")?;
    println!(
        "
  what the renderer will use:
    diffuse {}
    ambient {}
    sky     {}
    fog     {} from {:.0} to {:.0}",
        describe_colour(sample.diffuse),
        describe_colour(sample.ambient),
        describe_colour(sample.sky),
        describe_colour(sample.fog),
        sample.fog_start,
        sample.fog_end
    );

    // Every band, sampled now and across the day. This is what identifies a
    // band by what it does, which is the only thing that has identified one
    // yet: band 6 was found this way after band 0 was rejected by a render.
    println!("
  colour bands (18), now and at four hours:");
    println!("    {:<5} {:<22} {}", "band", "now", "00:00 / 06:00 / 12:00 / 18:00 brightness");
    for band in 0..INT_BANDS_PER_PARAMS {
        let now = lighting.colour(params_id, band, at);
        let across: Vec<String> = [0.0f32, 6.0, 12.0, 18.0]
            .iter()
            .map(|h| {
                let t = ((h / 24.0) * DAY_HALF_MINUTES as f32) as u32;
                match lighting.colour(params_id, band, t) {
                    Some(c) => format!("{:>4}", (c.iter().sum::<f32>() * 255.0).round() as u32),
                    None => "   -".into(),
                }
            })
            .collect();
        println!(
            "    {band:<5} {:<22} {}",
            now.map(describe_colour).unwrap_or_else(|| "empty".into()),
            across.join(" /")
        );
    }

    println!("
  scalar bands (6):");
    for band in 0..FLOAT_BANDS_PER_PARAMS {
        let across: Vec<String> = [0.0f32, 6.0, 12.0, 18.0]
            .iter()
            .map(|h| {
                let t = ((h / 24.0) * DAY_HALF_MINUTES as f32) as u32;
                lighting
                    .scalar(params_id, band, t)
                    .map(|v| format!("{v:>10.2}"))
                    .unwrap_or_else(|| "         -".into())
            })
            .collect();
        println!(
            "    {band:<5} now {:>10.2}   across the day {}",
            lighting.scalar(params_id, band, at).unwrap_or(f32::NAN),
            across.join(" /")
        );
    }
    Ok(())
}
