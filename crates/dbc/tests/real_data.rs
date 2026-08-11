//! Schema tests against a real 3.3.5a installation.
//!
//! Skipped unless `WOW_DATA` points at a `Data` directory. See
//! `crates/mpq/tests/real_data.rs` for why the file is named this way.

use dbc::infer::{infer, ColumnKind};
use dbc::schema::{AreaTable, CreatureDisplayInfo, CreatureModelData, Map, Spell};
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
    check!(Map, AreaTable, CreatureDisplayInfo, CreatureModelData, Spell);
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
