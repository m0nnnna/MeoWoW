//! Schema tests against a real 3.3.5a installation.
//!
//! Skipped unless `WOW_DATA` points at a `Data` directory. See
//! `crates/mpq/tests/real_data.rs` for why the file is named this way.

use dbc::infer::{infer, ColumnKind};
use dbc::schema::{
    AreaTable, CreatureDisplayInfo, CreatureModelData, Map, Spell, SpellDuration, SpellRadius,
};
use dbc::Dbc;
use mpq::Chain;

fn chain() -> Option<Chain> {
    let data = std::env::var_os("WOW_DATA")?;
    Some(Chain::open_wow_data(data, "enUS").expect("opening archives"))
}

macro_rules! require_data {
    () => {
        match chain() {
            Some(c) => c,
            None => {
                eprintln!("skipping: WOW_DATA not set");
                return;
            }
        }
    };
}

/// Field counts are build-specific, and a mismatch means the schema is being
/// applied to the wrong client version.
#[test]
fn transcribed_schemas_match_build_12340() {
    let mut chain = require_data!();
    macro_rules! check {
        ($($t:ty),*) => {$({
            let bytes = chain.read(<$t>::PATH).expect(<$t>::PATH);
            <$t>::parse(&bytes).unwrap_or_else(|e| panic!("{}: {e}", <$t>::NAME));
        })*};
    }
    check!(
        Map,
        AreaTable,
        CreatureDisplayInfo,
        CreatureModelData,
        Spell,
        SpellDuration,
        SpellRadius
    );
}

/// The effect columns behind a description's `$s1`-style tokens.
///
/// These were located by properties of the whole table rather than
/// transcribed -- each one's test is recorded on the column in
/// `dbc::schema::Spell` -- and this pins the result to specific spells, so a
/// shifted index fails loudly here instead of quoting a wrong number at a
/// player. The values chosen are ones whose meaning is visible in the
/// description text sitting next to them.
#[test]
fn spell_effect_columns_land_on_the_right_values() {
    let mut chain = require_data!();
    let spells = Spell::parse(&chain.read(Spell::PATH).unwrap()).unwrap();
    let durations = SpellDuration::parse(&chain.read(SpellDuration::PATH).unwrap()).unwrap();
    let radii = SpellRadius::parse(&chain.read(SpellRadius::PATH).unwrap()).unwrap();
    let find = |id: u32| spells.iter().find(|s| s.id() == id).expect("spell exists");

    // Heroic Strike rank 1: "increases melee damage by $s1", and the value is
    // stored one below what the tooltip says.
    let heroic = find(78);
    assert_eq!(heroic.name(), "Heroic Strike");
    assert_eq!(heroic.effect_base_points(), 10, "$s1 reads as 11");

    // Frostbolt rank 1 stores its slow negative: "slowing movement speed by
    // $s1%" has to print 40, which is the check that the sign is handled and
    // that the column is the slow rather than the damage.
    assert_eq!(find(116).effect_base_points(), -41);

    // Battle Shout rank 1 exercises both index tables at once: "within $a1
    // yards ... Lasts $d" is 30 yards for 2 minutes in this build.
    let shout = find(6673);
    let radius = radii
        .iter()
        .find(|r| r.id() == shout.effect_radius_index())
        .expect("radius index names a real row");
    assert_eq!(radius.radius(), 30.0);
    let duration = durations
        .iter()
        .find(|d| d.id() == shout.duration_index())
        .expect("duration index names a real row");
    assert_eq!(duration.duration().min(duration.max_duration()), 120_000);

    // A periodic effect, for the `$t1` column. Rejuvenation ticks every 3
    // seconds, which its own description quotes as `$t1`.
    let rejuv = find(774);
    assert_eq!(rejuv.name(), "Rejuvenation");
    assert_eq!(rejuv.effect_aura_period(), 3000);
}

/// Column *indices* are the part a field-count check cannot catch, so these
/// assert on content that would break if a column shifted.
#[test]
fn map_columns_land_on_the_right_values() {
    let mut chain = require_data!();
    let maps = Map::parse(&chain.read(Map::PATH).unwrap()).unwrap();

    let azeroth = maps.iter().find(|m| m.id() == 0).expect("map 0 exists");
    assert_eq!(azeroth.directory(), "Azeroth");
    assert_eq!(azeroth.name(), "Eastern Kingdoms");
    assert_eq!(azeroth.instance_type(), 0, "a continent is not an instance");
    assert_eq!(azeroth.minimap_icon_scale(), 1.0);

    // Northrend is a WotLK continent, so its expansion column must say so.
    let northrend = maps.iter().find(|m| m.id() == 571).expect("Northrend");
    assert_eq!(northrend.directory(), "Northrend");
    assert_eq!(northrend.expansion_id(), 2);
}

#[test]
fn spell_localized_columns_land_on_the_right_values() {
    let mut chain = require_data!();
    let spells = Spell::parse(&chain.read(Spell::PATH).unwrap()).unwrap();

    let touch = spells.iter().find(|s| s.id() == 5).expect("spell 5");
    assert_eq!(touch.name(), "Death Touch");
    assert!(
        touch.description().starts_with("Instantly Kills the target"),
        "description column misaligned: {:?}",
        touch.description()
    );
}

/// The display-info to model-data hop is how a creature becomes a file path,
/// and it is the first thing the renderer will need.
#[test]
fn creature_display_info_resolves_to_a_model_file() {
    let mut chain = require_data!();
    let display =
        CreatureDisplayInfo::parse(&chain.read(CreatureDisplayInfo::PATH).unwrap()).unwrap();
    let models =
        CreatureModelData::parse(&chain.read(CreatureModelData::PATH).unwrap()).unwrap();

    let row = display.iter().find(|d| d.id() == 16).expect("display 16");
    assert_eq!(row.texture_variation_0(), "HumanThiefSkin");

    let model = models
        .iter()
        .find(|m| m.id() == row.model_id())
        .expect("display info points at a real model row");
    assert!(
        model.model_name().to_lowercase().ends_with(".mdx"),
        "expected an .mdx path, got {:?}",
        model.model_name()
    );
    assert!(model.collision_height() > 0.0);
}

/// Some tables byte-pack their columns, so the word-based typed layer must
/// refuse them rather than silently misread.
#[test]
fn byte_packed_tables_are_rejected_by_the_typed_layer() {
    let mut chain = require_data!();
    let bytes = chain
        .read(r"DBFilesClient\SpellItemEnchantmentCondition.dbc")
        .unwrap();
    let raw = Dbc::parse(&bytes).expect("generic parse tolerates byte packing");
    assert!(!raw.is_uniform());
    assert_eq!(raw.fields(), 31);
    assert_eq!(raw.record_size(), 64);
}

/// Every DBC the install actually contains must parse. A regression here means
/// the generic reader has lost a case.
#[test]
fn every_table_in_the_install_parses() {
    let mut chain = require_data!();
    let names: Vec<String> = chain
        .list()
        .unwrap()
        .into_iter()
        .filter(|n| {
            let l = n.to_lowercase();
            l.starts_with("dbfilesclient\\") && l.ends_with(".dbc")
        })
        .collect();
    assert!(names.len() > 200, "expected a full table set");

    let mut failures = Vec::new();
    for name in &names {
        // A tombstoned table is absent by design, not a parse failure.
        let Ok(bytes) = chain.read(name) else {
            continue;
        };
        if let Err(e) = Dbc::parse(&bytes) {
            failures.push(format!("{name}: {e}"));
        }
    }
    assert!(failures.is_empty(), "tables failed to parse: {failures:#?}");
}

/// Inference is the tool for transcribing a new table, so it needs to stay
/// trustworthy on a table whose real layout we know.
#[test]
fn inference_recovers_a_known_layout() {
    let mut chain = require_data!();
    let t = Dbc::parse(&chain.read(r"DBFilesClient\LoadingScreens.dbc").unwrap()).unwrap();
    let columns = infer(&t);

    // ID, name, file path, has-widescreen.
    assert_eq!(columns[0].kind, ColumnKind::Int);
    assert_eq!(columns[1].kind, ColumnKind::String);
    assert_eq!(columns[2].kind, ColumnKind::String);
    assert_eq!(columns[3].kind, ColumnKind::Bool);
}

/// Localized blocks are 17 columns wide, and detecting them is what makes a
/// wide table like Spell readable at all.
#[test]
fn inference_finds_localized_blocks() {
    let mut chain = require_data!();
    let t = Dbc::parse(&chain.read(Map::PATH).unwrap()).unwrap();
    let columns = infer(&t);

    assert_eq!(columns[5].kind, ColumnKind::Localized, "MapName_lang");
    assert_eq!(columns[21].kind, ColumnKind::LocaleMask);
    assert!(columns[6..21]
        .iter()
        .all(|c| c.kind == ColumnKind::LocalePad));
    assert_eq!(columns[22].kind, ColumnKind::Int, "AreaTableID");
}

/// A storm dims the world, greys it, and pulls the horizon in.
///
/// The three properties together are what make the storm column the storm
/// column, and each is checked against clear weather at the same place and hour
/// rather than against a constant -- the claim is about the *relationship*, and
/// the absolute numbers are Blizzard's business.
///
/// Northshire is the subject because it is where this client is usually
/// standing, and because no positioned light covers it: it gets map 0's default
/// row, which is the one that matters. That row names clear params 12 and storm
/// params 10.
#[test]
fn a_storm_dims_greys_and_shortens_the_view() {
    let mut chain = require_data!();
    let lighting = dbc::light::Lighting::load(|path| chain.read(path).ok())
        .expect("the lighting tables");

    // Northshire, at noon, where the difference is largest.
    let (map, x, y, minute) = (0u32, -8950.0f32, -132.5f32, 12 * 60);
    let clear = lighting
        .sample_in(map, x, y, minute, 0.0)
        .expect("clear weather");
    let storm = lighting
        .sample_in(map, x, y, minute, 1.0)
        .expect("stormy weather");

    let luminance = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let spread = |c: [f32; 3]| {
        c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
    };

    assert!(
        luminance(storm.diffuse) < luminance(clear.diffuse),
        "a storm is not dimmer: clear {:?}, storm {:?}",
        clear.diffuse,
        storm.diffuse
    );
    // **Greyness is asserted at dawn, not at noon**, because at noon there is
    // none to remove: clear midday light is already a perfectly neutral
    // 0.71/0.71/0.71 and a storm cannot be greyer than grey. The sun's colour
    // is what a storm takes away, so the test belongs at the hour the sun has
    // a colour -- 06:00 clear is 1.00/0.79/0.30, a strong orange.
    let dawn = 6 * 60;
    let clear_dawn = lighting.sample_in(map, x, y, dawn, 0.0).expect("clear dawn");
    let storm_dawn = lighting.sample_in(map, x, y, dawn, 1.0).expect("stormy dawn");
    assert!(
        spread(clear_dawn.diffuse) > 0.2,
        "dawn is not warm enough to test greyness against: {:?}",
        clear_dawn.diffuse
    );
    assert!(
        spread(storm_dawn.diffuse) < spread(clear_dawn.diffuse) * 0.5,
        "a storm does not grey out the dawn: clear {:?}, storm {:?}",
        clear_dawn.diffuse,
        storm_dawn.diffuse
    );
    assert!(
        storm.fog_end < clear.fog_end,
        "a storm does not pull the fog in: clear {}, storm {}",
        clear.fog_end,
        storm.fog_end
    );
    assert_ne!(
        storm.params_id, clear.params_id,
        "clear and stormy resolved to the same curves, so nothing was blended"
    );
}

/// The five sky bands really are a sky, ordered zenith to horizon.
///
/// **The decisive hour is dawn, and it is the only one that discriminates.**
/// At noon or midnight almost any bright-to-dark reading of five bands looks
/// like a plausible gradient, so agreeing with one proves little. At sunrise a
/// sky is not a ramp: it is cool overhead and warm along the bottom, with the
/// warm half crossing in at a definite height. That crossing has a *side*, and
/// a side is something an ordering can get wrong.
///
/// It doubles as the byte-order check. Swapping red and blue negates every
/// difference asserted here, which would put the sunrise directly overhead --
/// so this failing means either the bands moved or `unpack` did.
#[test]
fn the_sky_bands_are_a_sky_and_the_horizon_is_the_last_one() {
    let mut chain = require_data!();
    let lighting = dbc::light::Lighting::load(|path| chain.read(path).ok())
        .expect("the lighting tables");
    // Northshire again: map 0's default row is what lights it, and a
    // positioned decorative light would answer a different question.
    let (map, x, y) = (0u32, -8950.0f32, -132.5f32);
    let at = |hour: u32| lighting.sample(map, x, y, hour * 60).expect("a sample");

    // Midnight: a night sky darkens all the way up, in every channel. The
    // weakest of the three claims, and the cheapest to keep honest.
    let night = at(0).sky;
    for pair in night.windows(2) {
        for c in 0..3 {
            assert!(
                pair[1][c] >= pair[0][c],
                "the midnight sky is not darkest overhead: {night:?}"
            );
        }
    }

    // Noon: red climbs from zenith to horizon while the sky loses its colour,
    // ending on an exactly neutral haze. Asserting the neutral *end* rather
    // than a monotone spread is deliberate -- the spread rises between the
    // first two bands, because the zenith at noon is nearly black and has
    // little colour to lose yet.
    let noon = at(12).sky;
    let spread = |c: [f32; 3]| c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]);
    for pair in noon.windows(2) {
        assert!(
            pair[1][0] >= pair[0][0],
            "midday red does not climb towards the horizon: {noon:?}"
        );
    }
    assert!(
        spread(noon[dbc::light::bands::HORIZON]) < 0.02,
        "the midday horizon is not the neutral end: {:?}",
        noon[dbc::light::bands::HORIZON]
    );
    assert!(
        spread(noon[dbc::light::bands::ZENITH]) > spread(noon[dbc::light::bands::HORIZON]),
        "the midday sky has no colour to lose: {noon:?}"
    );

    // And the one that could have come out either way. At both 06:00 and
    // 18:00 the warm half must be the horizon half, and the crossing must
    // happen once -- a sky that alternated warm and cool up its height would
    // be five bands that are not a gradient at all.
    for hour in [6u32, 18] {
        let sky = at(hour).sky;
        let warmth: Vec<f32> = sky.iter().map(|c| c[0] - c[2]).collect();
        assert!(
            warmth[dbc::light::bands::ZENITH] < 0.0,
            "the {hour}:00 zenith is warm, so the sun is overhead at dawn: {warmth:?}"
        );
        assert!(
            warmth[dbc::light::bands::HORIZON] > 0.1,
            "the {hour}:00 horizon is not warm, so nothing is rising there: {warmth:?}"
        );
        let crossings = warmth
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        assert_eq!(
            crossings, 1,
            "the {hour}:00 sky changes temperature {crossings} times: {warmth:?}"
        );
    }

    // Fog is the horizon, not a copy of it. If someone reintroduces a fog
    // band this fails and sends them to `bands::HORIZON` for the argument.
    assert_eq!(at(12).fog(), at(12).horizon());
}

/// Zero intensity is exactly clear weather, and the blend is monotone.
///
/// The half that stops a sign or lerp error from passing: a client that eased
/// the *wrong* way would still be dimmer at full storm, and asserting only the
/// endpoints would not notice. Intensity is what the server eases in and out,
/// so the midpoint has to sit between.
#[test]
fn weather_blends_from_clear_to_storm_in_order() {
    let mut chain = require_data!();
    let lighting = dbc::light::Lighting::load(|path| chain.read(path).ok())
        .expect("the lighting tables");
    let (map, x, y, minute) = (0u32, -8950.0f32, -132.5f32, 12 * 60);

    let plain = lighting.sample(map, x, y, minute).expect("clear");
    let none = lighting.sample_in(map, x, y, minute, 0.0).expect("clear");
    assert_eq!(
        plain, none,
        "no weather must be identical to the unweathered sample"
    );

    let fog: Vec<f32> = [0.0f32, 0.25, 0.5, 0.75, 1.0]
        .iter()
        .map(|&t| {
            lighting
                .sample_in(map, x, y, minute, t)
                .expect("a sample")
                .fog_end
        })
        .collect();
    for pair in fog.windows(2) {
        assert!(
            pair[1] <= pair[0],
            "fog end went the wrong way across the blend: {fog:?}"
        );
    }
    assert!(
        fog[0] > fog[4],
        "the blend did nothing at all: {fog:?}"
    );

    // Out-of-range intensities must clamp rather than extrapolate into
    // colours neither table describes.
    assert_eq!(
        lighting.sample_in(map, x, y, minute, 5.0),
        lighting.sample_in(map, x, y, minute, 1.0)
    );
    assert_eq!(
        lighting.sample_in(map, x, y, minute, -2.0),
        lighting.sample_in(map, x, y, minute, 0.0)
    );
}
