//! Schema tests against a real 3.3.5a installation.
//!
//! Skipped unless `WOW_DATA` points at a `Data` directory. See
//! `crates/mpq/tests/real_data.rs` for why the file is named this way.

use dbc::infer::{infer, ColumnKind};
use dbc::schema::{
    AreaTable, CreatureDisplayInfo, CreatureModelData, CreatureSoundData, FootstepTerrainLookup,
    LightSkybox, Map, SoundEntries, SoundType, Spell, SpellDuration, SpellRadius,
    SpellVisual, SpellVisualKit, TerrainType, WorldMapArea, WorldMapOverlay, WorldSafeLocs,
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
        LightSkybox,
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

/// `map_id` was identified by a gap, not a hit rate: `Map.dbc` is small and
/// dense (135 rows spanning ids 0-724), so a column of small integers lands
/// on it fairly often by chance alone. The candidate column has to clear a
/// control by a wide margin, not merely score high in isolation.
#[test]
fn world_safe_locs_map_column_beats_a_control_by_a_wide_margin() {
    let mut chain = require_data!();
    let maps = Map::parse(&chain.read(Map::PATH).unwrap()).unwrap();
    let locs = WorldSafeLocs::parse(&chain.read(WorldSafeLocs::PATH).unwrap()).unwrap();

    let map_ids: std::collections::HashSet<u32> = maps.iter().map(|m| m.id()).collect();

    let candidate_hits = locs.iter().filter(|l| map_ids.contains(&l.map_id())).count();
    // Control: the graveyard's own id, read as if it were a map id. It
    // shares no relationship with Map.dbc at all.
    let control_hits = locs.iter().filter(|l| map_ids.contains(&l.id())).count();

    assert_eq!(
        candidate_hits,
        locs.len(),
        "every graveyard's map_id should resolve to a real Map.dbc row"
    );
    assert!(
        control_hits * 2 < candidate_hits,
        "control ({control_hits}) should be nowhere near the candidate ({candidate_hits}) \
         or the check proves nothing"
    );
}

#[test]
fn world_safe_locs_columns_land_on_the_right_values() {
    let mut chain = require_data!();
    let locs = WorldSafeLocs::parse(&chain.read(WorldSafeLocs::PATH).unwrap()).unwrap();

    let stormwind = locs.iter().find(|l| l.id() == 1).expect("graveyard 1");
    assert_eq!(stormwind.map_id(), 0, "Stormwind's graveyard is on Eastern Kingdoms");
    assert_eq!(stormwind.name(), "Stormwind");
    assert!((stormwind.x() - -9115.0).abs() < 1.0);
    assert!((stormwind.z() - 96.0).abs() < 1.0);

    // Blizzard's own placeholder row: named "Reuse" and never actually used,
    // which is why its coordinates are all zero rather than a parse bug.
    let reuse = locs.iter().find(|l| l.id() == 1036).expect("graveyard 1036");
    assert_eq!(reuse.name(), "Reuse");
    assert_eq!((reuse.x(), reuse.y(), reuse.z()), (0.0, 0.0, 0.0));
}

/// **`SpellVisualKit::anim` was identified by what it varies with**, which is
/// the only thing that could have identified it.
///
/// `AnimationData` is 506 rows numbered 0..505, so a column of small integers
/// resolving into it is nearly free -- two other columns in this same table
/// resolve 100% of the time and are not animations. The argument is that this
/// column's *values change with the moment the kit belongs to*, in the
/// direction anyone who has played the game would predict: kits named by
/// `SpellVisual`'s precast slot reach the ready poses, its casting slot the
/// cast gestures, its channel slot the channelled ones.
///
/// The impact slot is the half that makes this a measurement rather than a
/// story: its animations are overwhelmingly `CombatCritical`, `CombatWound`
/// and `Knockdown` -- the **victim's** flinch, not the caster's gesture --
/// which is why the client reads three of the six slots and not this one.
///
/// The controls are the two columns that resolve just as well. Neither shows
/// any moment structure at all, which is what a coincidence of magnitudes
/// looks like.
#[test]
fn spell_visual_kit_animation_varies_with_the_moment() {
    use std::collections::{HashMap, HashSet};

    let mut chain = require_data!();
    let visuals = SpellVisual::parse(&chain.read(SpellVisual::PATH).unwrap()).unwrap();
    let kits = SpellVisualKit::parse(&chain.read(SpellVisualKit::PATH).unwrap()).unwrap();
    let animations =
        dbc::schema::AnimationData::parse(&chain.read(dbc::schema::AnimationData::PATH).unwrap())
            .unwrap();

    let animation_name: HashMap<u32, String> = animations
        .iter()
        .map(|row| (row.id(), row.name().to_string()))
        .collect();
    // Raw column access, because the controls are columns this schema
    // deliberately does not transcribe.
    let column: HashMap<u32, Vec<u32>> = kits
        .iter()
        .map(|row| {
            let raw = row.raw();
            (row.id(), vec![raw.u32(2), raw.u32(16), raw.u32(17)])
        })
        .collect();

    // Which kits each moment names.
    let moment = |slot: fn(&dbc::schema::SpellVisualRow<'_>) -> u32| -> HashSet<u32> {
        visuals
            .iter()
            .map(|visual| slot(&visual))
            .filter(|id| *id != 0 && *id != u32::MAX && column.contains_key(id))
            .collect()
    };
    let precast = moment(|v| v.precast_kit());
    let casting = moment(|v| v.casting_kit());
    let channel = moment(|v| v.channel_kit());
    let impact = moment(|v| v.impact_kit());

    /// What fraction of a moment's animations belong to a named family.
    fn share(
        ids: &HashSet<u32>,
        column: &HashMap<u32, Vec<u32>>,
        which: usize,
        names: &HashMap<u32, String>,
        family: fn(&str) -> bool,
    ) -> (usize, f64) {
        let resolved: Vec<&String> = ids
            .iter()
            .filter_map(|kit| column.get(kit)?.get(which))
            .filter(|value| **value != 0 && **value != u32::MAX)
            .filter_map(|value| names.get(value))
            .collect();
        if resolved.is_empty() {
            return (0, 0.0);
        }
        let hits = resolved.iter().filter(|name| family(name)).count();
        (resolved.len(), hits as f64 / resolved.len() as f64)
    }

    let ready = |name: &str| name.starts_with("ReadySpell") || name == "SpellPrecast";
    let cast = |name: &str| name.starts_with("SpellCast");
    let channelled = |name: &str| name.starts_with("ChannelCast");
    let flinch = |name: &str| name.starts_with("Combat") || name.starts_with("Knock");

    // The transcribed column, moment by moment. Each family is the plurality
    // of its own moment and a fraction of any other -- the fractions are not
    // near 1.0 because most spells in the table are creature abilities using
    // ordinary gestures, which is exactly why a *comparison* rather than a
    // threshold is what settles it.
    let (n, precast_ready) = share(&precast, &column, 0, &animation_name, ready);
    assert!(n > 500, "only {n} precast animations to test");
    let (_, precast_cast) = share(&precast, &column, 0, &animation_name, cast);
    assert!(
        precast_ready > 0.25 && precast_ready > 3.0 * precast_cast,
        "precast kits should reach the ready poses: {precast_ready:.3} against \
         {precast_cast:.3} for the cast gestures"
    );

    let (n, casting_cast) = share(&casting, &column, 0, &animation_name, cast);
    assert!(n > 1000, "only {n} casting animations to test");
    let (_, casting_ready) = share(&casting, &column, 0, &animation_name, ready);
    assert!(
        casting_cast > 0.30 && casting_cast > 6.0 * casting_ready,
        "casting kits should reach the cast gestures: {casting_cast:.3} against \
         {casting_ready:.3}"
    );

    let (_, channel_share) = share(&channel, &column, 0, &animation_name, channelled);
    assert!(
        channel_share > 0.60,
        "channel kits should reach the channelled cycles: {channel_share:.3}"
    );

    // The finding: an impact poses the *victim*.
    let (_, impact_flinch) = share(&impact, &column, 0, &animation_name, flinch);
    let (_, impact_cast) = share(&impact, &column, 0, &animation_name, cast);
    assert!(
        impact_flinch > 0.40 && impact_cast < 0.10,
        "impact kits should reach the wound reactions rather than the caster's \
         gesture: {impact_flinch:.3} flinching against {impact_cast:.3} casting"
    );

    // And the controls: two columns that resolve into `AnimationData` just as
    // reliably and carry no moment structure whatsoever.
    for control in [1usize, 2] {
        for (label, ids) in [("precast", &precast), ("casting", &casting), ("channel", &channel)] {
            for family in [ready, cast, channelled] {
                let (_, got) = share(ids, &column, control, &animation_name, family);
                assert!(
                    got < 0.20,
                    "control column {} carries moment structure on {label}: {got:.3}",
                    if control == 1 { 16 } else { 17 }
                );
            }
        }
    }
}

/// `SpellVisualKit::sound` was identified by type, not by validity:
/// `SpellVisualKit`'s own ids are 56% dense over their range, so almost any
/// small integer in a row lands on one -- the actual argument is that the
/// values this column holds resolve to `SoundEntries` rows of the *spell*
/// type at 99.9%, not merely to real rows at all.
#[test]
fn spell_visual_kit_sound_resolves_to_spell_type_sounds() {
    let mut chain = require_data!();
    let kits = SpellVisualKit::parse(&chain.read(SpellVisualKit::PATH).unwrap()).unwrap();
    let sounds = SoundEntries::parse(&chain.read(SoundEntries::PATH).unwrap()).unwrap();
    let sound_type: std::collections::HashMap<u32, SoundType> = sounds
        .iter()
        .map(|row| (row.id(), SoundType::from_raw(row.sound_type())))
        .collect();

    let candidates: Vec<u32> = kits
        .iter()
        .map(|row| row.sound())
        .filter(|id| *id != 0 && *id != u32::MAX)
        .collect();
    assert!(candidates.len() > 4000, "expected thousands of candidates, got {}", candidates.len());

    let resolved: Vec<u32> = candidates
        .iter()
        .filter(|id| sound_type.contains_key(id))
        .copied()
        .collect();
    let resolve_rate = resolved.len() as f64 / candidates.len() as f64;
    assert!(resolve_rate > 0.99, "resolve rate {resolve_rate:.3} should exceed 99%");

    // `SoundType::Other(1)`: type 1 is not yet confirmed enough to name in
    // `SoundType` itself (see its own doc comment -- "not clean enough to
    // put a name on" at the table level), but it is exactly clean enough
    // here, scoped to the rows this column actually points at.
    let spell_typed = resolved
        .iter()
        .filter(|id| sound_type.get(id) == Some(&SoundType::Other(1)))
        .count();
    let spell_rate = spell_typed as f64 / resolved.len() as f64;
    assert!(spell_rate > 0.99, "spell-type rate {spell_rate:.3} should exceed 99%");
}

/// `Spell::spell_visual` (field 131) resolves into a real `SpellVisual` row
/// for effectively every spell that names one -- the same test as
/// `duration_index`, and what backs trusting field 131 rather than field 132.
#[test]
fn spell_visual_link_resolves_for_every_nonzero_spell() {
    let mut chain = require_data!();
    let spells = Spell::parse(&chain.read(Spell::PATH).unwrap()).unwrap();
    let visuals = SpellVisual::parse(&chain.read(SpellVisual::PATH).unwrap()).unwrap();
    let visual_ids: std::collections::HashSet<u32> = visuals.iter().map(|v| v.id()).collect();

    let nonzero: Vec<u32> = spells
        .iter()
        .map(|s| s.spell_visual())
        .filter(|id| *id != 0)
        .collect();
    assert!(nonzero.len() > 20_000, "expected tens of thousands, got {}", nonzero.len());

    let resolved = nonzero.iter().filter(|id| visual_ids.contains(id)).count();
    let rate = resolved as f64 / nonzero.len() as f64;
    assert!(rate > 0.99, "resolve rate {rate:.3} should exceed 99%");
}

/// `SpellVisual::has_missile` and `Spell::speed` are transcribed from two
/// separately-offset columns in two different tables. If the offsets are
/// right, they describe the same fact -- whether a spell has a projectile --
/// and should agree far more than chance. If either offset were wrong this
/// would most likely read as noise near 50/50, not as a strong but imperfect
/// correlation.
#[test]
fn spell_visual_has_missile_tracks_spell_speed() {
    let mut chain = require_data!();
    let spells = Spell::parse(&chain.read(Spell::PATH).unwrap()).unwrap();
    let visuals = SpellVisual::parse(&chain.read(SpellVisual::PATH).unwrap()).unwrap();
    let missile_by_visual: std::collections::HashMap<u32, bool> =
        visuals.iter().map(|v| (v.id(), v.has_missile())).collect();

    let mut with_speed = (0u32, 0u32); // (has_missile, total)
    let mut without_speed = (0u32, 0u32);
    for spell in spells.iter() {
        let Some(&has_missile) = missile_by_visual.get(&spell.spell_visual()) else {
            continue;
        };
        let bucket = if spell.speed() > 0.0 {
            &mut with_speed
        } else {
            &mut without_speed
        };
        bucket.1 += 1;
        if has_missile {
            bucket.0 += 1;
        }
    }

    let rate = |b: (u32, u32)| b.0 as f64 / b.1 as f64;
    assert!(with_speed.1 > 1000 && without_speed.1 > 1000, "population too small to test");
    assert!(
        rate(with_speed) > 0.8,
        "speed>0 should mostly have a missile, got {:.3}",
        rate(with_speed)
    );
    assert!(
        rate(without_speed) < 0.1,
        "speed==0 should rarely have a missile, got {:.3}",
        rate(without_speed)
    );
}

/// The decisive test: which `SpellVisual` column is which was settled by the
/// sound's own name, the same evidence `INTERFACE_CLICK` rests on. Six
/// well-known spells, picked so each moment (precast, casting, impact, and a
/// persistent state) is checked against a sound whose name states what it is.
#[test]
fn spell_visual_kit_columns_land_on_the_right_moment() {
    let mut chain = require_data!();
    let spells = Spell::parse(&chain.read(Spell::PATH).unwrap()).unwrap();
    let visuals = SpellVisual::parse(&chain.read(SpellVisual::PATH).unwrap()).unwrap();
    let kits = SpellVisualKit::parse(&chain.read(SpellVisualKit::PATH).unwrap()).unwrap();
    let sounds = SoundEntries::parse(&chain.read(SoundEntries::PATH).unwrap()).unwrap();

    let visual = |id: u32| visuals.iter().find(|v| v.id() == id).expect("visual exists");
    let kit_sound = |kit_id: u32| {
        kits.iter()
            .find(|k| k.id() == kit_id)
            .map(|k| k.sound())
            .filter(|id| *id != 0)
    };
    let sound_name = |id: u32| {
        sounds
            .iter()
            .find(|s| s.id() == id)
            .map(|s| s.name().to_string())
            .expect("sound exists")
    };
    // Case-insensitive: this build's names are not consistently capitalised
    // ("Precast Fire Low" vs "PrecastMagicLow").
    let names = |kit_id: u32| kit_sound(kit_id).map(sound_name).unwrap_or_default().to_lowercase();

    for (spell_id, name) in [(116, "Frostbolt"), (133, "Fireball"), (686, "Shadow Bolt")] {
        let spell = spells.iter().find(|s| s.id() == spell_id).expect("spell exists");
        assert_eq!(spell.name(), name);
        let v = visual(spell.spell_visual());
        assert!(
            names(v.precast_kit()).contains("precast"),
            "{name}'s precast kit should be named as a precast"
        );
        assert!(
            names(v.casting_kit()).contains("cast"),
            "{name}'s casting kit should be named as a cast"
        );
        assert!(
            names(v.impact_kit()).contains("impact"),
            "{name}'s impact kit should be named as an impact"
        );
    }

    // Power Word: Shield has no impact -- it never resolves against a target
    // the way a missile does -- but its persistent shield is column 4.
    let shield = spells.iter().find(|s| s.id() == 17).expect("spell exists");
    assert_eq!(shield.name(), "Power Word: Shield");
    let v = visual(shield.spell_visual());
    assert!(
        names(v.state_kit()).contains("shield"),
        "Power Word: Shield's state kit should name a shield"
    );
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


/// Every page states a rectangle the right way round, or states nothing.
///
/// `max`/`min` are names this schema *earned*: if any row had them the other
/// way round, `Page::has_bounds` would silently exclude a real page and the
/// zone it draws would open the continent map instead.
///
/// Exactly three of the 108 rows carry all zeroes, and they are named rather
/// than counted -- a count passes just as well when a page that used to state
/// a rectangle stops doing so, which is the change worth hearing about.
#[test]
fn world_map_pages_state_their_bounds_max_first() {
    let mut chain = require_data!();
    let table = WorldMapArea::parse(&chain.read(WorldMapArea::PATH).expect("the table"))
        .expect("parsing WorldMapArea");

    let (mut stated, mut silent) = (0usize, Vec::new());
    for row in table.iter() {
        let (x_max, x_min, y_max, y_min) = (row.x_max(), row.x_min(), row.y_max(), row.y_min());
        if x_max == 0.0 && x_min == 0.0 && y_max == 0.0 && y_min == 0.0 {
            silent.push(row.directory().to_string());
            continue;
        }
        assert!(
            x_max > x_min && y_max > y_min,
            "page {} ({}) states x {x_min}..{x_max}, y {y_min}..{y_max}",
            row.id(),
            row.directory()
        );
        stated += 1;
    }
    assert_eq!(stated, 105, "pages stating a rectangle");
    assert_eq!(silent, ["Dalaran", "TheNexus", "UtgardeKeep"]);
}

/// The overlay hit box is top/left/bottom/right, and the alternative reading
/// is excluded rather than merely unchosen.
///
/// Four columns of pixel numbers admit two orderings and both parse. What
/// separates them is containment: a patch's clickable box should lie inside
/// the patch's own texture rectangle, and read the other way the very first
/// row's box starts seventy-seven pixels outside it. Asserting only that the
/// chosen reading fits would pass just as well under the wrong one on a table
/// where the two happened to agree, so this counts **both** readings over the
/// whole table and requires the chosen one to win by a margin no coincidence
/// covers.
///
/// The result: **862 of 868 rows fit as transcribed, 123 fit the other way**,
/// which is a seven-to-one separation rather than a coincidence.
///
/// **It is not unanimous, and the six exceptions are recorded rather than
/// filtered away.** Five spill between one and seventy pixels over an edge of
/// their own texture (`PURGATIONISLE`, `HIGHPERCH`, `THEHEAP`,
/// `TheLivingWood`, `SilvermoonCity`) and `KHARANOS` states a box whose bottom
/// is above its top, which no ordering of these four columns can rescue. That
/// is authoring slop in the table; a filter tuned until the count came out
/// clean would be describing a tidier table than the one that ships.
#[test]
fn overlay_hit_rects_lie_inside_their_textures() {
    let mut chain = require_data!();
    let table = WorldMapOverlay::parse(&chain.read(WorldMapOverlay::PATH).expect("the table"))
        .expect("parsing WorldMapOverlay");

    let (mut checked, mut chosen_fits, mut swapped_fits) = (0usize, 0usize, 0usize);
    for row in table.iter() {
        // A patch with no size, or one that states no box at all, has nothing
        // to be inside of. Both are common and neither is a reading of these
        // columns -- 158 of the 988 rows are one or the other.
        let no_box = row.hit_top() == 0
            && row.hit_left() == 0
            && row.hit_bottom() == 0
            && row.hit_right() == 0;
        if row.width() == 0 || row.height() == 0 || row.texture().is_empty() || no_box {
            continue;
        }
        let (left, top) = (row.offset_x(), row.offset_y());
        let (right, bottom) = (left + row.width(), top + row.height());
        let inside = |l: u32, t: u32, r: u32, b: u32| {
            l >= left && t >= top && r <= right && b <= bottom && r > l && b > t
        };
        checked += 1;
        // As transcribed: 13 is the top, 14 the left, 15 the bottom, 16 the
        // right.
        if inside(row.hit_left(), row.hit_top(), row.hit_right(), row.hit_bottom()) {
            chosen_fits += 1;
        }
        // And the same four numbers read as left/top/right/bottom, which is
        // the reading a reader who had not measured would assume.
        if inside(row.hit_top(), row.hit_left(), row.hit_bottom(), row.hit_right()) {
            swapped_fits += 1;
        }
    }

    assert!(checked > 800, "only {checked} overlays stated both rectangles");
    assert!(
        chosen_fits * 100 >= checked * 99,
        "only {chosen_fits} of {checked} hit boxes lie inside their own texture"
    );
    assert!(
        swapped_fits * 4 < checked,
        "the other reading fits {swapped_fits} of {checked} rows, which is not a \
         separation this table can be read from"
    );
}

/// The projection, checked against art rather than against itself.
///
/// `Stormwind` is a page in its own right *and* a patch drawn on the `Elwynn`
/// page, so the world rectangle one row states can be projected through
/// another row and landed on a picture whose pixel position a third table
/// gives independently. Nothing here is derived from anything else here:
/// disagreement would mean the projection is wrong, and three of the four
/// possible readings miss by more than the whole overlay is wide.
#[test]
fn a_citys_page_projects_onto_the_patch_that_draws_it() {
    let mut chain = require_data!();
    let table = WorldMapArea::parse(&chain.read(WorldMapArea::PATH).expect("the table"))
        .expect("parsing WorldMapArea");
    let overlays = WorldMapOverlay::parse(&chain.read(WorldMapOverlay::PATH).expect("the table"))
        .expect("parsing WorldMapOverlay");
    let atlas = dbc::worldmap::Atlas::from_table(&table);

    let elwynn = atlas
        .pages()
        .iter()
        .find(|p| p.directory == "Elwynn")
        .expect("the Elwynn page");
    let stormwind = atlas
        .pages()
        .iter()
        .find(|p| p.directory == "Stormwind")
        .expect("the Stormwind page");
    let patch = overlays
        .iter()
        .find(|row| {
            row.world_map_area_id() == elwynn.id && row.texture().eq_ignore_ascii_case("STORMWIND")
        })
        .expect("Elwynn's STORMWIND patch");

    // Where the city's own rectangle lands on the zone page...
    let (left, top) = elwynn.project_pixels(stormwind.x_max, stormwind.y_max);
    let (right, bottom) = elwynn.project_pixels(stormwind.x_min, stormwind.y_min);
    // ...against where the picture of it actually sits.
    let (art_left, art_top) = (patch.offset_x() as f32, patch.offset_y() as f32);
    let (art_right, art_bottom) = (
        art_left + patch.width() as f32,
        art_top + patch.height() as f32,
    );
    // A generous margin on purpose: the city page's rectangle and the drawn
    // patch are authored separately and are not expected to agree to the
    // pixel. What is being excluded is the flips, which miss by hundreds.
    let near = |a: f32, b: f32| (a - b).abs() < 120.0;
    assert!(
        near(left, art_left) && near(right, art_right),
        "horizontal {left:.0}..{right:.0} against the patch's {art_left:.0}..{art_right:.0}"
    );
    assert!(
        near(top, art_top) && near(bottom, art_bottom),
        "vertical {top:.0}..{bottom:.0} against the patch's {art_top:.0}..{art_bottom:.0}"
    );
    // The same numbers with an axis reversed, which is what a different
    // reading of the bounds would produce.
    assert!(
        !near(dbc::worldmap::PAGE_WIDTH - left, art_left),
        "a flipped horizontal axis lands on the patch too"
    );
    assert!(
        !near(dbc::worldmap::PAGE_HEIGHT - top, art_top),
        "a flipped vertical axis lands on the patch too"
    );
}

/// `FootstepTerrainLookup`'s terrain column is a `TerrainType.sound_id` and
/// not a `TerrainType` row id, and the sounds' own names say so.
///
/// The two readings are off by one from each other all the way down a
/// twelve-row table, so both parse and both produce a plausible-looking client.
/// What separates them is that `SoundEntries` labels its rows: taking only the
/// footstep sounds whose name states a material -- `CharacterSmallSnow`,
/// `CharacterMediumLargeWood` -- and asking which reading agrees, one wins by a
/// wide margin and the other is wrong nearly everywhere.
///
/// The unanimous halves are asserted separately from the tally, because a
/// margin can survive a single column being wrong and these two cannot: under
/// the losing reading, terrain 4's five snow sounds all become `Wood` and
/// terrain 5's five wood sounds all become `Grass`.
#[test]
fn the_footstep_terrain_column_is_a_sound_id() {
    let mut chain = require_data!();
    let terrain = TerrainType::parse(&chain.read(TerrainType::PATH).expect("TerrainType"))
        .expect("parsing TerrainType");
    let lookup =
        FootstepTerrainLookup::parse(&chain.read(FootstepTerrainLookup::PATH).expect("lookup"))
            .expect("parsing FootstepTerrainLookup");
    let sounds = SoundEntries::parse(&chain.read(SoundEntries::PATH).expect("SoundEntries"))
        .expect("parsing SoundEntries");
    let named: std::collections::HashMap<u32, String> =
        sounds.iter().map(|r| (r.id(), r.name().to_string())).collect();

    const MATERIALS: [&str; 8] = [
        "dirt", "metal", "stone", "snow", "wood", "grass", "leaves", "sand",
    ];
    let material_of = |name: &str| {
        let lower = name.to_lowercase();
        MATERIALS.into_iter().find(|m| lower.contains(m))
    };
    // What each reading claims the terrain value means, as lower-case words.
    // `Metallic` is trimmed to `metal` so a name match is about the material
    // rather than about English word endings.
    let claims = |value: u32, by_sound_id: bool| -> Vec<String> {
        terrain
            .iter()
            .filter(|r| if by_sound_id { r.sound_id() == value } else { r.id() == value })
            .map(|r| r.name().to_lowercase().replace("metallic", "metal"))
            .collect()
    };
    let agrees = |claims: &[String], material: &str| {
        claims.iter().any(|c| c.contains(material) || material.contains(c.as_str()))
    };

    let (mut by_sound, mut by_row, mut voters) = (0usize, 0usize, 0usize);
    for row in lookup.iter() {
        let Some(name) = named.get(&row.sound()) else {
            continue;
        };
        let Some(material) = material_of(name) else {
            continue;
        };
        voters += 1;
        if agrees(&claims(row.terrain(), true), material) {
            by_sound += 1;
        }
        if agrees(&claims(row.terrain(), false), material) {
            by_row += 1;
        }
        // The two decisive columns, each unanimous under the right reading.
        if row.terrain() == 4 {
            assert_eq!(material, "snow", "terrain 4 reached {name}, which is not snow");
        }
        if row.terrain() == 5 {
            assert_eq!(material, "wood", "terrain 5 reached {name}, which is not wood");
        }
    }

    assert!(voters >= 40, "only {voters} sounds name a material at all");
    assert!(
        by_sound >= 2 * by_row,
        "the sound-id reading should win outright: {by_sound} against {by_row} of {voters}"
    );
}

/// Every sound a footstep row names exists, and the splash column is a
/// different *kind* of sound from the step column.
///
/// The type is what tells the two columns apart without recalling their order:
/// a splash is `SoundEntries` type 20 and says `Splash` in its own name, where
/// every ordinary footstep is type 3.
#[test]
fn footstep_rows_reach_real_sounds_of_the_right_kind() {
    let mut chain = require_data!();
    let lookup =
        FootstepTerrainLookup::parse(&chain.read(FootstepTerrainLookup::PATH).expect("lookup"))
            .expect("parsing FootstepTerrainLookup");
    let sounds = SoundEntries::parse(&chain.read(SoundEntries::PATH).expect("SoundEntries"))
        .expect("parsing SoundEntries");
    let rows: std::collections::HashMap<u32, (u32, String)> = sounds
        .iter()
        .map(|r| (r.id(), (r.sound_type(), r.name().to_string())))
        .collect();

    let (mut steps, mut splashes, mut splash_named, mut missing) = (0usize, 0usize, 0usize, 0usize);
    for row in lookup.iter() {
        match rows.get(&row.sound()) {
            Some((kind, _)) => {
                steps += 1;
                assert_eq!(*kind, 3, "footstep {} is sound type {kind}", row.sound());
            }
            None => missing += 1,
        }
        if row.sound_splash() == 0 {
            continue;
        }
        match rows.get(&row.sound_splash()) {
            Some((_, name)) => {
                splashes += 1;
                if name.to_lowercase().contains("splash") {
                    splash_named += 1;
                }
            }
            None => missing += 1,
        }
    }

    assert_eq!(missing, 0, "{missing} footstep sound ids name no row");
    assert_eq!(steps, lookup.len(), "every row names a step sound");
    assert!(
        splash_named * 10 >= splashes * 9,
        "only {splash_named} of {splashes} splash sounds are called `Splash`"
    );
}

/// `CreatureSoundData`'s footstep column names a group that exists, and that
/// is not a free result.
///
/// Every other column in that table holds `SoundEntries` ids, which run to
/// 18,019 over 12,941 rows -- so landing on a valid one proves nothing. The
/// footstep groups are 23 distinct values scattered over 0..=188, so a wrong
/// column lands inside that set by luck and almost never.
#[test]
fn the_creature_footstep_column_names_a_group_that_exists() {
    let mut chain = require_data!();
    let lookup =
        FootstepTerrainLookup::parse(&chain.read(FootstepTerrainLookup::PATH).expect("lookup"))
            .expect("parsing FootstepTerrainLookup");
    let creatures =
        CreatureSoundData::parse(&chain.read(CreatureSoundData::PATH).expect("CreatureSoundData"))
            .expect("parsing CreatureSoundData");
    let groups: std::collections::HashSet<u32> =
        lookup.iter().map(|r| r.creature_footstep_id()).collect();

    let (mut set, mut inside) = (0usize, 0usize);
    // The control: the aggro column, which is a sound id and must *not* look
    // like a group. Asserting only that the footstep column resolves would
    // pass just as well if every column did.
    let (mut control_set, mut control_inside) = (0usize, 0usize);
    for row in creatures.iter() {
        if row.footstep_group() != 0 {
            set += 1;
            inside += usize::from(groups.contains(&row.footstep_group()));
        }
        if row.aggro() != 0 {
            control_set += 1;
            control_inside += usize::from(groups.contains(&row.aggro()));
        }
    }

    assert!(groups.len() < 30, "{} groups is too many to be selective", groups.len());
    assert!(set > 500, "only {set} rows name a footstep group");
    assert_eq!(inside, set, "{} of {set} groups do not exist", set - inside);
    assert!(
        control_inside * 20 < control_set,
        "the aggro column looks like a group too: {control_inside} of {control_set}"
    );
}

/// The star dome is a row of `LightSkybox`, found by name, and the outdoor
/// world names no skybox of its own.
///
/// **Three claims in one test because they only mean anything together.** That
/// `Stars.mdx` is in this table is what makes drawing it a transcription
/// rather than an invention; that Azeroth and Kalimdor name *no* skybox is
/// what makes the five-band gradient the ordinary sky rather than a stand-in
/// for one; and the lookup by file name is what stops either claim resting on
/// a row id that could be anything.
///
/// If a later build moves the star dome, this fails and says so, which is the
/// point -- a client that silently drew no stars would look exactly like a
/// clear night that happened to be dark.
#[test]
fn the_star_dome_is_a_light_skybox_row_and_the_outdoor_world_names_none() {
    let mut chain = require_data!();
    let lighting =
        dbc::light::Lighting::load(|path| chain.read(path).ok()).expect("lighting tables");

    let dome = lighting.star_dome().expect("no LightSkybox row named Stars.mdx");
    assert!(
        dome.to_ascii_lowercase().ends_with(r"stars\stars.mdx"),
        "the star dome should live in Environments\\Stars, got {dome}"
    );

    // Every params row an outdoor light on either continent can select.
    let mut outdoor = Vec::new();
    for row in lighting.lights().iter() {
        if row.map_id() != 0 && row.map_id() != 1 {
            continue;
        }
        for id in [row.params_clear(), row.params_storm()] {
            if id != 0 && !outdoor.contains(&id) {
                outdoor.push(id);
            }
        }
    }
    assert!(
        outdoor.len() > 100,
        "only {} outdoor light params: the population is too small to mean anything",
        outdoor.len()
    );
    let named: Vec<u32> = outdoor
        .iter()
        .copied()
        .filter(|&id| lighting.skybox_model(lighting.skybox_of(id)).is_some())
        .collect();
    assert!(
        named.is_empty(),
        "an outdoor light named a skybox after all: {named:?}"
    );

    // And the table is not empty, or the check above would pass for the wrong
    // reason -- 158 of the 850 params rows do name one, they are simply all
    // somewhere this client is not standing.
    let anywhere = lighting
        .params()
        .iter()
        .filter(|row| lighting.skybox_model(row.light_skybox_id()).is_some())
        .count();
    assert_eq!(
        anywhere, 158,
        "158 LightParams rows named a skybox when this was measured"
    );
}
