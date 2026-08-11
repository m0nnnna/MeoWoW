//! WMO tests against a real 3.3.5a installation.
//!
//! Skipped unless `WOW_DATA` points at a `Data` directory.

use mpq::Chain;
use wmo::{group_path, is_group_path, Chunks, Group, Root};

const FARM: &str = r"World\wmo\Azeroth\Buildings\Human_Farm\Farm.wmo";

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

/// Loads a root and the `MOGN` block its groups need for their names.
fn load_root(chain: &mut Chain, path: &str) -> (Root, Vec<u8>) {
    let bytes = chain.read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let root = Root::parse(&bytes).unwrap_or_else(|e| panic!("{path}: {e}"));
    let names = Chunks::find(&bytes, b"MOGN").unwrap_or(&[]).to_vec();
    (root, names)
}

#[test]
fn parses_a_known_object() {
    let mut chain = require_data!();
    let (root, names) = load_root(&mut chain, FARM);

    assert_eq!(root.header.group_count, 2);
    assert_eq!(root.materials.len(), 14);
    assert_eq!(root.textures().len(), 14);
    // Furnished and empty variants of the same building.
    assert!(root.doodad_sets.len() >= 2);
    assert!(root.doodads.len() > 100);

    // Every material must resolve to a real texture path.
    for material in &root.materials {
        let path = root.texture(material.texture1);
        assert!(path.to_lowercase().ends_with(".blp"), "got {path:?}");
        assert!(chain.contains(path), "{path} is not in the archives");
    }

    let (exterior, _) = (
        Group::parse(&chain.read(&group_path(FARM, 0)).unwrap(), &names).unwrap(),
        (),
    );
    let interior = Group::parse(&chain.read(&group_path(FARM, 1)).unwrap(), &names).unwrap();
    assert_eq!(exterior.name, "exterior");
    assert_eq!(interior.name, "interior");
    assert!(!exterior.is_interior());
    assert!(interior.is_interior());
    // Only the interior bakes lighting into vertex colours.
    assert!(interior.has_vertex_colors());
    assert_eq!(interior.vertex_colors.len(), interior.vertices.len());
}

/// Collision geometry sits in the same arrays as visible surfaces and is
/// excluded by `MOPY`, not by being absent. Drawing it would put invisible
/// walls on screen.
#[test]
fn collision_triangles_are_outside_every_render_batch() {
    let mut chain = require_data!();
    let (root, names) = load_root(&mut chain, FARM);

    let mut total_collision = 0usize;
    for gi in 0..root.header.group_count as usize {
        let group = Group::parse(&chain.read(&group_path(FARM, gi)).unwrap(), &names).unwrap();
        for (tri, material) in group.triangle_materials.iter().enumerate() {
            if !material.is_collision_only() {
                continue;
            }
            total_collision += 1;
            let first = tri * 3;
            let drawn = group.batches.iter().any(|b| {
                let start = b.start_index as usize;
                first >= start && first < start + b.index_count as usize
            });
            assert!(!drawn, "group {gi} triangle {tri} is collision but batched");
        }
    }
    assert!(total_collision > 0, "fixture has no collision geometry");
}

/// Batches must tile the index array without overlapping or running past it.
#[test]
fn batches_are_contiguous_and_in_range() {
    let mut chain = require_data!();
    let (root, names) = load_root(&mut chain, FARM);

    for gi in 0..root.header.group_count as usize {
        let group = Group::parse(&chain.read(&group_path(FARM, gi)).unwrap(), &names).unwrap();
        group.validate().unwrap_or_else(|e| panic!("group {gi}: {e}"));

        let mut cursor = 0u32;
        for (bi, batch) in group.batches.iter().enumerate() {
            assert!(
                batch.start_index >= cursor,
                "group {gi} batch {bi} overlaps the previous one"
            );
            cursor = batch.start_index + batch.index_count as u32;
            assert!(cursor as usize <= group.indices.len());
            assert_eq!(batch.index_count % 3, 0);
            assert!((batch.material_id as usize) < root.materials.len());
        }
    }
}

/// Group files parse as WMOs, so anything walking the listfile has to tell
/// them apart from roots or it loads each wall as its own building.
#[test]
fn group_files_are_distinguishable_in_the_real_listfile() {
    let mut chain = require_data!();
    let all: Vec<String> = chain
        .list()
        .unwrap()
        .into_iter()
        .filter(|n| n.to_lowercase().ends_with(".wmo"))
        .collect();

    let roots: Vec<&String> = all.iter().filter(|n| !is_group_path(n)).collect();
    let groups = all.len() - roots.len();
    assert!(roots.len() > 1000, "only {} roots", roots.len());
    assert!(groups > roots.len(), "expected more groups than roots");

    // Every root's first group must exist, which is the real proof the naming
    // rule is right rather than merely self-consistent.
    let mut checked = 0;
    // Stride chosen so the sample spreads across ~1,985 roots and still
    // yields a hundred or so checks.
    for root in roots.iter().step_by(15).take(120) {
        let bytes = chain.read(root.as_str()).unwrap();
        let Ok(parsed) = Root::parse(&bytes) else {
            continue;
        };
        if parsed.header.group_count == 0 {
            continue;
        }
        assert!(
            chain.contains(&group_path(root, 0)),
            "{root} declares {} groups but _000 is missing",
            parsed.header.group_count
        );
        checked += 1;
    }
    assert!(checked > 50, "only checked {checked} objects");
}

/// Broad structural sweep. `wow-cli wmo survey` covers all 1,985 objects; this
/// samples so the suite stays quick.
#[test]
fn sampled_objects_parse_and_validate() {
    let mut chain = require_data!();
    let roots: Vec<String> = chain
        .list()
        .unwrap()
        .into_iter()
        .filter(|n| n.to_lowercase().ends_with(".wmo") && !is_group_path(n))
        .step_by(11)
        .take(160)
        .collect();

    let mut failures = Vec::new();
    let mut groups_checked = 0;
    for path in &roots {
        let Ok(bytes) = chain.read(path) else { continue };
        let root = match Root::parse(&bytes) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{path}: {e}"));
                continue;
            }
        };
        let names = Chunks::find(&bytes, b"MOGN").unwrap_or(&[]).to_vec();
        for gi in 0..root.header.group_count as usize {
            let gpath = group_path(path, gi);
            let Ok(gbytes) = chain.read(&gpath) else {
                failures.push(format!("{gpath}: missing"));
                continue;
            };
            match Group::parse(&gbytes, &names) {
                Ok(group) => {
                    groups_checked += 1;
                    if let Err(e) = group.validate() {
                        failures.push(format!("{gpath}: {e}"));
                    }
                }
                Err(e) => failures.push(format!("{gpath}: {e}")),
            }
        }
    }
    assert!(groups_checked > 200, "only {groups_checked} groups");
    assert!(
        failures.is_empty(),
        "{} failures: {:#?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
}
