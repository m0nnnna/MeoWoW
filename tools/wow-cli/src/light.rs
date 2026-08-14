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

/// Asks whether `Light.dbc`'s third parameter column really is the stormy one.
///
/// **The column names in the schema are a reading, not a measurement**, and a
/// wrong one here is invisible: the renderer would pick a perfectly valid
/// `LightParams` row that simply is not the weather it claims. So the question
/// is not "could column 9 be storm" but "is it *darker because* it is storm".
///
/// The property a storm must have is that its light is dimmer and greyer than
/// clear weather at the same place and hour. That is checked across every light
/// on every map at once, against the clear column as its own control -- and a
/// column that is not weather at all has no reason to be systematically darker.
pub fn weather_check(chain: &mut Chain, hour: f32) -> Result<()> {
    let lighting = dbc::light::Lighting::load(|path| chain.read(path).ok())
        .context("could not load the lighting tables")?;
    let at = ((hour / 24.0) * DAY_HALF_MINUTES as f32) as u32 % DAY_HALF_MINUTES;

    let luminance = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    // How far a colour is from grey, as the spread between its channels. Rain
    // desaturates; a column that is merely a different *time* would not.
    let saturation = |c: [f32; 3]| {
        let max = c[0].max(c[1]).max(c[2]);
        let min = c[0].min(c[1]).min(c[2]);
        max - min
    };

    let (mut pairs, mut darker, mut greyer, mut identical) = (0usize, 0usize, 0usize, 0usize);
    let (mut clear_sum, mut storm_sum) = (0.0f64, 0.0f64);
    let (mut closer_fog, mut fog_pairs) = (0usize, 0usize);
    let (mut clear_fog, mut storm_fog) = (0.0f64, 0.0f64);
    for row in lighting.lights().iter() {
        // **Outdoors only.** Weather happens on the two continents; a dungeon's
        // eight columns are filled in because the row has eight columns, and
        // including them buries the signal in rows where the question is
        // meaningless. Maps 0 and 1 are Eastern Kingdoms and Kalimdor.
        if row.map_id() != 0 && row.map_id() != 1 {
            continue;
        }
        let (clear, storm) = (row.params_clear(), row.params_storm());
        if clear == 0 || storm == 0 {
            continue;
        }
        if clear == storm {
            identical += 1;
            continue;
        }
        let (Some(a), Some(b)) = (
            lighting.colour(clear, dbc::light::bands::DIFFUSE, at),
            lighting.colour(storm, dbc::light::bands::DIFFUSE, at),
        ) else {
            continue;
        };
        pairs += 1;
        clear_sum += luminance(a) as f64;
        storm_sum += luminance(b) as f64;
        if luminance(b) < luminance(a) {
            darker += 1;
        }
        if saturation(b) < saturation(a) {
            greyer += 1;
        }
        // Fog is the sharpest of the three, and the most physical: you cannot
        // see as far in the rain. A column that is not weather has no reason to
        // pull the horizon in.
        if let (Some(fa), Some(fb)) = (
            lighting.scalar(clear, dbc::light::scalars::FOG_END, at),
            lighting.scalar(storm, dbc::light::scalars::FOG_END, at),
        ) {
            fog_pairs += 1;
            clear_fog += fa as f64;
            storm_fog += fb as f64;
            if fb < fa {
                closer_fog += 1;
            }
        }
    }

    let pct = |n: usize| 100.0 * n as f32 / pairs.max(1) as f32;
    println!("
Light.dbc, clear column against storm column, at {hour:.0}:00");
    println!("  {pairs} lights name a different row for each, {identical} name the same row");
    println!("  storm is darker on {darker} of them ({:.1}%)", pct(darker));
    println!("  storm is greyer on {greyer} of them ({:.1}%)", pct(greyer));
    println!(
        "  mean diffuse luminance: clear {:.3}, storm {:.3}",
        clear_sum / pairs.max(1) as f64,
        storm_sum / pairs.max(1) as f64
    );
    println!(
        "  storm pulls the fog in on {closer_fog} of {fog_pairs} ({:.1}%)",
        100.0 * closer_fog as f32 / fog_pairs.max(1) as f32
    );
    println!(
        "  mean fog end: clear {:.0}, storm {:.0}",
        clear_fog / fog_pairs.max(1) as f64,
        storm_fog / fog_pairs.max(1) as f64
    );
    // The rows that actually matter. Northshire is covered by none of
    // Azeroth's positioned lights -- the nearest is 124,000 units away -- so
    // the map default is what lights the zone this client is usually standing
    // in. A statistic over 200 special-purpose lights (a glowing crater, a
    // haunted wood) says little about the one row the renderer will use.
    for map_id in [0u32, 1] {
        let Some(row) = lighting
            .lights()
            .iter()
            .find(|r| r.map_id() == map_id && r.x() == 0.0 && r.y() == 0.0 && r.z() == 0.0)
        else {
            continue;
        };
        println!(
            "
  map {map_id} default light: clear params {}, storm params {}",
            row.params_clear(),
            row.params_storm()
        );
        println!(
            "    {:>6}  {:>22}  {:>22}  {:>10}  {:>10}",
            "hour", "clear diffuse", "storm diffuse", "clear fog", "storm fog"
        );
        for hour in [0.0f32, 6.0, 12.0, 18.0, 22.0] {
            let t = ((hour / 24.0) * DAY_HALF_MINUTES as f32) as u32 % DAY_HALF_MINUTES;
            let show = |id: u32| {
                lighting
                    .colour(id, dbc::light::bands::DIFFUSE, t)
                    .map(|c| format!("{:.2} {:.2} {:.2}", c[0], c[1], c[2]))
                    .unwrap_or_else(|| "-".into())
            };
            let fog = |id: u32| {
                lighting
                    .scalar(id, dbc::light::scalars::FOG_END, t)
                    .map(|v| format!("{v:.0}"))
                    .unwrap_or_else(|| "-".into())
            };
            println!(
                "    {hour:>6.0}  {:>22}  {:>22}  {:>10}  {:>10}",
                show(row.params_clear()),
                show(row.params_storm()),
                fog(row.params_clear()),
                fog(row.params_storm()),
            );
        }
    }
    Ok(())
}
