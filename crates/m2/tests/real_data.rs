//! Model tests against a real 3.3.5a installation.
//!
//! Skipped unless `WOW_DATA` points at a `Data` directory.

use m2::{model_path, skin_path, Model, Skin};
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

fn load(chain: &mut Chain, path: &str) -> Model {
    Model::parse(&chain.read(path).unwrap_or_else(|e| panic!("{path}: {e}")))
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn load_skin(chain: &mut Chain, path: &str) -> Skin {
    Skin::parse(&chain.read(path).unwrap_or_else(|e| panic!("{path}: {e}")))
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn parses_a_known_model() {
    let mut chain = require_data!();
    let path = r"Creature\BogBeast\BogBeast.m2";
    let model = load(&mut chain, path);

    assert_eq!(model.version(), m2::VERSION_WOTLK);
    assert_eq!(model.name(), "BogBeast");
    assert_eq!(model.vertex_count(), 985);
    assert_eq!(model.bones().len(), 70);
    assert_eq!(model.textures().len(), 2);
    assert_eq!(model.materials().len(), 2);

    // Creature skins are supplied by CreatureDisplayInfo, not named in-model.
    assert!(model.textures().iter().all(|t| !t.is_hardcoded()));
    assert_eq!(model.textures()[0].kind, 11);
}

/// Normals are unit vectors by construction, so their lengths are the sharpest
/// available probe on the vertex layout: a wrong stride or field offset breaks
/// them immediately while everything still parses.
#[test]
fn vertex_layout_produces_unit_normals() {
    let mut chain = require_data!();
    let model = load(&mut chain, r"Creature\BogBeast\BogBeast.m2");

    for (i, v) in model.vertices().iter().enumerate() {
        let len: f32 = v.normal.iter().map(|c| c * c).sum::<f32>().sqrt();
        assert!(
            (0.98..=1.02).contains(&len),
            "vertex {i} normal has length {len}"
        );
        let weight: u32 = v.bone_weights.iter().map(|&w| w as u32).sum();
        assert!(
            weight == 0 || (250..=260).contains(&weight),
            "vertex {i} bone weights sum to {weight}"
        );
    }
}

/// Skin indices are two levels deep -- triangles index the vertex map, which
/// indexes the model. Resolving them wrongly yields plausible but scrambled
/// geometry, so this checks the resolution explicitly.
#[test]
fn resolves_submesh_geometry() {
    let mut chain = require_data!();
    let model = load(&mut chain, r"Creature\BogBeast\BogBeast.m2");
    let skin = load_skin(&mut chain, r"Creature\BogBeast\BogBeast00.skin");

    skin.validate(model.vertex_count()).expect("index tables");
    assert_eq!(skin.submeshes().len(), 2);
    assert_eq!(skin.batches().len(), 2);

    let total: u32 = skin.submeshes().iter().map(|s| s.index_count).sum();
    assert_eq!(
        total as usize,
        skin.triangles().len(),
        "submeshes should tile the index array"
    );

    for sub in skin.submeshes() {
        let indices = skin.submesh_indices(sub).expect("resolves");
        assert_eq!(indices.len(), sub.index_count as usize);
        assert!(indices.iter().all(|&i| (i as usize) < model.vertex_count()));
        assert_eq!(indices.len() % 3, 0);
    }
}

/// Every batch must resolve through the texture combo table to a real texture
/// slot; that indirection is what the renderer performs per draw call.
#[test]
fn batches_resolve_to_texture_slots() {
    let mut chain = require_data!();
    let model = load(&mut chain, r"Creature\BogBeast\BogBeast.m2");
    let skin = load_skin(&mut chain, r"Creature\BogBeast\BogBeast00.skin");

    let combos = model.texture_combos();
    let textures = model.textures();
    for batch in skin.batches() {
        let combo = combos
            .get(batch.texture_combo_index as usize)
            .expect("combo index in range");
        assert!(
            (*combo as usize) < textures.len(),
            "combo points outside the texture list"
        );
        assert!((batch.material_index as usize) < model.materials().len());
    }
}

/// `indexCount` is a 16-bit field and a few models overflow it. The true span
/// is recovered from the next submesh's start.
#[test]
fn repairs_sixteen_bit_count_overflow() {
    let mut chain = require_data!();
    let path = r"World\DUNGEON\Sunwell\Sunwell_Bushes.m2";
    let model = load(&mut chain, path);
    let skin = load_skin(&mut chain, &skin_path(path, 0));

    assert_eq!(skin.triangles().len(), 93_456);
    let sub = &skin.submeshes()[0];
    assert!(sub.counts_repaired, "this submesh should have been repaired");
    // Stored as 27,920, which is 93,456 wrapped at 65,536.
    assert_eq!(sub.index_count, 93_456);
    skin.validate(model.vertex_count()).expect("index tables");
}

/// The repair must not fire on a submesh that is already correct.
///
/// This skin has a submesh legitimately declaring 1,728 indices whose distance
/// to the next start is 67,264 -- congruent modulo 65,536, and therefore a trap
/// for a repair rule that checks congruence alone.
#[test]
fn does_not_repair_a_valid_submesh() {
    let mut chain = require_data!();
    let path = r"Environments\Stars\NexusRaid_SkyA.m2";
    let model = load(&mut chain, path);
    let skin = load_skin(&mut chain, &skin_path(path, 0));

    skin.validate(model.vertex_count()).expect("index tables");
    let sub = &skin.submeshes()[43];
    assert!(!sub.counts_repaired);
    assert_eq!(sub.index_count, 1_728);
}

/// `CreatureModelData` stores `.mdx` paths that no longer exist as files. The
/// rewrite to `.m2` has to hold for the whole table, not just the examples.
#[test]
fn every_creature_model_path_resolves() {
    let mut chain = require_data!();
    let table = dbc::schema::CreatureModelData::parse(
        &chain
            .read(dbc::schema::CreatureModelData::PATH)
            .expect("CreatureModelData"),
    )
    .expect("parse");

    let paths: Vec<String> = table
        .iter()
        .map(|row| model_path(row.model_name()))
        .filter(|p| !p.is_empty())
        .collect();
    assert!(paths.len() > 1000);

    let missing: Vec<&String> = paths.iter().filter(|p| !chain.contains(p)).collect();
    let ratio = missing.len() as f32 / paths.len() as f32;
    assert!(
        ratio < 0.02,
        "{}/{} creature models did not resolve, e.g. {:?}",
        missing.len(),
        paths.len(),
        &missing[..missing.len().min(5)]
    );
}

/// Broad structural sweep. `wow-cli m2 survey` covers all 22k; this samples so
/// the suite stays quick.
#[test]
fn sampled_models_parse_and_validate() {
    let mut chain = require_data!();
    let names: Vec<String> = chain
        .list()
        .unwrap()
        .into_iter()
        .filter(|n| n.to_lowercase().ends_with(".m2"))
        .step_by(29)
        .take(700)
        .collect();

    let mut failures = Vec::new();
    let mut checked = 0;
    for name in &names {
        let Ok(bytes) = chain.read(name) else { continue };
        let model = match Model::parse(&bytes) {
            Ok(m) => m,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        checked += 1;
        for issue in model.validate() {
            failures.push(format!("{name}: {issue}"));
        }

        let path = skin_path(name, 0);
        if let Ok(sb) = chain.read(&path) {
            match Skin::parse(&sb) {
                Ok(skin) => {
                    if let Err(e) = skin.validate(model.vertex_count()) {
                        failures.push(format!("{path}: {e}"));
                    }
                }
                Err(e) => failures.push(format!("{path}: {e}")),
            }
        }
    }
    assert!(checked > 500, "sample too small: {checked}");
    assert!(
        failures.is_empty(),
        "{} failures: {:#?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
}
