//! Terrain tests against a real 3.3.5a installation.
//!
//! Skipped unless `WOW_DATA` points at a `Data` directory.

use adt::{tile_path, wdt_path, Adt, Wdt, CHUNKS_PER_TILE, CHUNK_COUNT, HEIGHTS_PER_CHUNK};
use mpq::Chain;

/// Northshire, in Elwynn Forest.
const MAP: &str = "Azeroth";
const TILE: (usize, usize) = (32, 48);

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

fn load_tile(chain: &mut Chain, map: &str, x: usize, y: usize) -> Adt {
    let wdt = Wdt::parse(&chain.read(&wdt_path(map)).expect("wdt")).expect("parse wdt");
    let path = tile_path(map, x, y);
    Adt::parse(&chain.read(&path).expect(&path), wdt.big_alpha()).expect(&path)
}

#[test]
fn reads_a_map_definition() {
    let mut chain = require_data!();
    let wdt = Wdt::parse(&chain.read(&wdt_path(MAP)).unwrap()).unwrap();

    // Eastern Kingdoms occupies a fraction of the 64x64 grid.
    assert!(wdt.tile_count() > 500 && wdt.tile_count() < 4096);
    assert!(wdt.has_tile(TILE.0, TILE.1));
    assert!(!wdt.has_tile(0, 0), "the grid corner is open ocean");

    // Every tile the WDT declares must actually exist, since the client uses
    // this table to decide what to stream.
    for (x, y) in wdt.tiles().into_iter().step_by(37) {
        assert!(
            chain.contains(&tile_path(MAP, x, y)),
            "{MAP} declares tile {x},{y} but the file is absent"
        );
    }
}

#[test]
fn parses_a_known_tile() {
    let mut chain = require_data!();
    let tile = load_tile(&mut chain, MAP, TILE.0, TILE.1);

    assert_eq!(tile.chunks.len(), CHUNK_COUNT);
    assert!(!tile.textures.is_empty());
    assert!(tile.doodads.len() > 100, "Elwynn is densely furnished");

    // This tile holds Northshire Abbey.
    assert!(
        tile.objects
            .iter()
            .any(|o| o.path.to_uppercase().contains("NSABBEY")),
        "expected the abbey among {:?}",
        tile.objects.iter().map(|o| &o.path).collect::<Vec<_>>()
    );

    // Placements name their model through an offset table, so a wrong lookup
    // shows up as empty or garbage paths.
    for doodad in tile.doodads.iter().take(50) {
        assert!(
            doodad.path.to_lowercase().ends_with(".mdx")
                || doodad.path.to_lowercase().ends_with(".m2"),
            "doodad path {:?}",
            doodad.path
        );
        assert!(doodad.scale > 0.0 && doodad.scale < 100.0);
    }
}

/// Chunks tile exactly, so shared edges must agree. A mismatch means the
/// interleaved height lattice is being read with the wrong stride -- an error
/// that otherwise produces plausible-looking landscape.
#[test]
fn chunks_meet_at_their_edges() {
    let mut chain = require_data!();
    let wdt = Wdt::parse(&chain.read(&wdt_path(MAP)).unwrap()).unwrap();

    let mut checked = 0;
    for (x, y) in wdt.tiles().into_iter().step_by(53).take(25) {
        let path = tile_path(MAP, x, y);
        let Ok(bytes) = chain.read(&path) else { continue };
        let tile = Adt::parse(&bytes, wdt.big_alpha()).expect(&path);
        tile.validate().unwrap_or_else(|e| panic!("{path}: {e}"));
        checked += 1;
    }
    assert!(checked > 10, "only checked {checked} tiles");
}

/// Each chunk occupies exactly one cell of the 16x16 grid, and the grid spans
/// exactly one tile. This is what places tiles correctly next to each other.
#[test]
fn chunks_form_a_regular_grid() {
    let mut chain = require_data!();
    let tile = load_tile(&mut chain, MAP, TILE.0, TILE.1);

    for (i, chunk) in tile.chunks.iter().enumerate() {
        // Chunks are stored with the first index advancing fastest. Note the
        // stored `IndexX` tracks the *y* world axis and vice versa, which is
        // the usual WoW axis swap.
        assert_eq!(
            chunk.index,
            ((i % CHUNKS_PER_TILE) as u32, (i / CHUNKS_PER_TILE) as u32)
        );
        assert_eq!(chunk.heights.len(), HEIGHTS_PER_CHUNK);

        // A chunk's own vertices must stay inside its cell.
        let xs: Vec<f32> = (0..HEIGHTS_PER_CHUNK).map(|v| chunk.vertex_position(v)[0]).collect();
        let ys: Vec<f32> = (0..HEIGHTS_PER_CHUNK).map(|v| chunk.vertex_position(v)[1]).collect();
        let span = |v: &[f32]| {
            v.iter().cloned().fold(f32::MIN, f32::max) - v.iter().cloned().fold(f32::MAX, f32::min)
        };
        assert!(
            (span(&xs) - adt::CHUNK_SIZE).abs() < 0.01,
            "chunk {i} spans {} in x, expected {}",
            span(&xs),
            adt::CHUNK_SIZE
        );
        assert!((span(&ys) - adt::CHUNK_SIZE).abs() < 0.01);
    }

    // Opposite corners of the tile are one tile apart on each axis.
    let first = tile.chunks[0].position;
    let last = tile.chunks[CHUNK_COUNT - 1].position;
    let step = adt::CHUNK_SIZE * (CHUNKS_PER_TILE - 1) as f32;
    assert!((first[0] - last[0] - step).abs() < 0.1);
    assert!((first[1] - last[1] - step).abs() < 0.1);
}

/// Sampling the surface at a stored sample returns that sample, on real
/// terrain.
///
/// The unit test for this builds a synthetic height field; this one runs the
/// same property over an entire tile of Elwynn, which brings real slopes, real
/// holes and real coordinate magnitudes -- a chunk eight thousand units from
/// the origin has a float ulp of about a millimetre, and a boundary test that
/// is a hair too strict rejects points that are genuinely on the terrain.
#[test]
fn sampled_heights_agree_with_the_vertices_they_sit_on() {
    let mut chain = require_data!();
    let tile = load_tile(&mut chain, MAP, TILE.0, TILE.1);

    let mut sampled = 0;
    let mut holed = 0;
    for (i, chunk) in tile.chunks.iter().enumerate() {
        for index in 0..HEIGHTS_PER_CHUNK {
            let p = chunk.vertex_position(index);
            let Some(height) = chunk.height_at(p[0], p[1]) else {
                // The only legitimate refusal inside a chunk's own footprint.
                assert!(chunk.holes != 0, "chunk {i} refused sample {index}");
                holed += 1;
                continue;
            };
            assert!(
                (height - p[2]).abs() < 0.05,
                "chunk {i} sample {index}: got {height}, vertex is at {}",
                p[2]
            );
            sampled += 1;
        }
    }
    assert!(sampled > CHUNK_COUNT * 100, "only sampled {sampled} points");
    // Not an assertion about this tile, just worth seeing in the log.
    eprintln!("{sampled} samples matched, {holed} fell in holes");
}

/// A point on the seam between two chunks gets the same height from either
/// side.
///
/// Terrain chunks tile exactly -- `validate` already checks their stored edges
/// agree -- so the interpolation on top of them has to agree too. A stride or
/// axis error inside one chunk shows up here as a step at every seam, which is
/// what a character walking across a tile would feel as a stumble.
#[test]
fn neighbouring_chunks_agree_along_their_seam() {
    let mut chain = require_data!();
    let tile = load_tile(&mut chain, MAP, TILE.0, TILE.1);

    let mut checked = 0;
    for y in 0..CHUNKS_PER_TILE {
        for x in 0..CHUNKS_PER_TILE - 1 {
            let (here, next) = (tile.chunk(x, y).unwrap(), tile.chunk(x + 1, y).unwrap());
            // Wherever the two chunks' footprints touch, sample from both.
            for step in 1..8 {
                let along = step as f32 * adt::UNIT_SIZE;
                for (px, py) in [
                    (next.position[0], next.position[1] - along),
                    (next.position[0] - along, next.position[1]),
                ] {
                    let (Some(a), Some(b)) = (here.height_at(px, py), next.height_at(px, py))
                    else {
                        continue;
                    };
                    assert!(
                        (a - b).abs() < 0.05,
                        "chunks {x},{y} and {},{y} disagree at {px},{py}: {a} vs {b}",
                        x + 1
                    );
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 100, "only checked {checked} seam points");
}

/// The interpolated surface sits where the objects standing on it think the
/// ground is.
///
/// Everything else about heights here is checked against the height field
/// itself, which cannot catch a surface that is internally consistent and in
/// the wrong place. A tile's doodads are an independent statement about the
/// same ground: they were placed by an artist standing them on it, and they
/// arrive through a different chunk of the file with a different coordinate
/// convention. If the two agree, the surface a character stands on is the one
/// the world was built around.
///
/// Only the *median* offset is asserted, and loosely. Plenty of individual
/// doodads genuinely do not touch the ground -- hanging signs, rocks sunk into
/// a hillside, anything on a building -- so a tight bound on all of them would
/// fail on correct data. A wrong axis or a mis-indexed chunk does not shift the
/// median by a metre; it destroys any relationship at all.
#[test]
fn doodads_stand_on_the_interpolated_surface() {
    let mut chain = require_data!();
    let tile = load_tile(&mut chain, MAP, TILE.0, TILE.1);

    // Placements store the axes permuted, both horizontals measured inwards
    // from the far corner of the map, and the middle component as height. See
    // `docs/RENDERING.md`; written out here rather than shared with the
    // renderer's own converter deliberately, so this stays a second opinion.
    let centre = 32.0 * adt::TILE_SIZE;
    let mut offsets: Vec<f32> = Vec::new();
    for doodad in &tile.doodads {
        let (x, y, z) = (
            centre - doodad.position[2],
            centre - doodad.position[0],
            doodad.position[1],
        );
        // Whichever chunk owns the spot. Scanned rather than indexed so this
        // test does not depend on the grid convention it is checking.
        let Some(ground) = tile.chunks.iter().find_map(|c| c.height_at(x, y)) else {
            continue;
        };
        offsets.push(z - ground);
    }

    assert!(offsets.len() > 500, "only {} doodads landed on the tile", offsets.len());
    offsets.sort_by(f32::total_cmp);
    let median = offsets[offsets.len() / 2];
    let near_ground = offsets.iter().filter(|o| o.abs() < 1.0).count();
    eprintln!(
        "{} doodads: median offset {median:.3}, {near_ground} within a unit of the ground",
        offsets.len()
    );
    assert!(
        median.abs() < 1.0,
        "doodads sit a median {median} from the interpolated ground"
    );
    assert!(
        near_ground * 2 > offsets.len(),
        "only {near_ground} of {} doodads are within a unit of the ground",
        offsets.len()
    );
}

/// Alpha maps decode to a full 64x64 whichever way they were stored, and every
/// layer beyond the first has one.
#[test]
fn alpha_maps_decode_to_full_size() {
    let mut chain = require_data!();
    let tile = load_tile(&mut chain, MAP, TILE.0, TILE.1);

    let mut layered = 0;
    for chunk in &tile.chunks {
        assert_eq!(
            chunk.alpha_maps.len(),
            chunk.layers.len().saturating_sub(1),
            "one alpha map per layer after the base"
        );
        for map in &chunk.alpha_maps {
            assert_eq!(map.len(), adt::ALPHA_SIZE * adt::ALPHA_SIZE);
            layered += 1;
        }
        for layer in &chunk.layers {
            assert!(
                (layer.texture_id as usize) < tile.textures.len(),
                "layer references texture {} of {}",
                layer.texture_id,
                tile.textures.len()
            );
        }
    }
    assert!(layered > 100, "expected blended terrain, got {layered} maps");
}

/// Every texture and model a tile names must exist, or the terrain renders
/// with holes and placeholder art.
#[test]
fn referenced_assets_exist() {
    let mut chain = require_data!();
    let tile = load_tile(&mut chain, MAP, TILE.0, TILE.1);

    for texture in &tile.textures {
        assert!(chain.contains(texture), "missing texture {texture}");
    }
    for model in tile.object_models.iter().take(10) {
        assert!(chain.contains(model), "missing world object {model}");
    }
    for model in tile.doodad_models.iter().take(20) {
        // Placements still carry the historical `.mdx` extension.
        let resolved = m2::model_path(model);
        assert!(chain.contains(&resolved), "missing doodad {resolved}");
    }
}

/// `GroundEffectTexture`'s terrain column names the material its textures are
/// *called*, which is what makes it usable for footsteps.
///
/// The chain a footstep needs runs: map chunk -> texture layer -> `effect_id`
/// -> `GroundEffectTexture` -> a `TerrainType` row. Every link but the last is
/// a bare small integer with nothing to confirm it, and the last one is a
/// twelve-row table of names. What confirms the whole chain is one step
/// further out: a layer also names a **texture file**, and those filenames are
/// authored English. A column whose `Snow` rows are reached by files called
/// `..._snow_...` is the terrain column, and no coincidence of small integers
/// produces that.
///
/// Two rows are checked because their material word is unambiguous in a
/// filename. `Dirt` is not, and is deliberately left out: terrain row 0 is
/// `Dirt` *and* is what 22,708 of 24,981 ground effects say when they say
/// nothing at all, so agreement there would prove nothing either way.
#[test]
fn ground_effects_name_the_terrain_their_textures_are_called() {
    let mut chain = require_data!();
    let terrain =
        dbc::schema::TerrainType::parse(&chain.read(dbc::schema::TerrainType::PATH).expect("terrain"))
            .expect("parsing TerrainType");
    let textures = dbc::schema::GroundEffectTexture::parse(
        &chain.read(dbc::schema::GroundEffectTexture::PATH).expect("ground effects"),
    )
    .expect("parsing GroundEffectTexture");
    let ids: std::collections::HashSet<u32> = terrain.iter().map(|r| r.id()).collect();
    let effect: std::collections::HashMap<u32, u32> =
        textures.iter().map(|r| (r.id(), r.terrain_type())).collect();

    for row in textures.iter() {
        assert!(
            ids.contains(&row.terrain_type()),
            "ground effect {} names terrain {}, which is not a row",
            row.id(),
            row.terrain_type()
        );
    }

    let wdt = adt::Wdt::parse(&chain.read(&adt::wdt_path("Azeroth")).expect("wdt")).expect("wdt");
    let mut hits: std::collections::HashMap<u32, (usize, usize)> = std::collections::HashMap::new();
    let (mut tiles, mut resolved, mut layers) = (0usize, 0usize, 0usize);
    for (x, y) in wdt.tiles() {
        if tiles >= 250 {
            break;
        }
        let Ok(bytes) = chain.read(&adt::tile_path("Azeroth", x, y)) else {
            continue;
        };
        let Ok(tile) = adt::Adt::parse(&bytes, wdt.big_alpha()) else {
            continue;
        };
        tiles += 1;
        for chunk in &tile.chunks {
            for layer in &chunk.layers {
                if layer.effect_id == 0 {
                    continue;
                }
                layers += 1;
                let Some(&id) = effect.get(&layer.effect_id) else {
                    continue;
                };
                resolved += 1;
                let Some(name) = tile.textures.get(layer.texture_id as usize) else {
                    continue;
                };
                let lower = name.to_lowercase();
                for (terrain_id, word) in [(3u32, "snow"), (5, "grass")] {
                    if id != terrain_id {
                        continue;
                    }
                    let entry = hits.entry(terrain_id).or_default();
                    entry.1 += 1;
                    entry.0 += usize::from(lower.contains(word));
                }
            }
        }
    }

    // A layer naming a ground effect that does not exist would be the chain
    // breaking at its first link, and would be invisible in the tally below.
    assert_eq!(
        resolved, layers,
        "{} of {layers} layers name a GroundEffectTexture row that is not there",
        layers - resolved
    );
    for (terrain_id, word) in [(3u32, "snow"), (5, "grass")] {
        let (named, total) = hits.get(&terrain_id).copied().unwrap_or((0, 0));
        assert!(total > 100, "only {total} layers reach terrain {terrain_id}");
        assert!(
            named * 2 > total,
            "terrain {terrain_id} should be reached by textures called `{word}`: \
             {named} of {total}"
        );
    }
}

/// A chunk's footing grid says which layer is underfoot, and it agrees with
/// the alpha maps it was reduced from.
///
/// The property that matters is that it is not constant: a grid that answered
/// "layer 0" everywhere would look perfectly reasonable and would make every
/// road in the game sound like the field beside it.
#[test]
fn the_footing_grid_varies_across_a_real_tile() {
    let mut chain = require_data!();
    let wdt = adt::Wdt::parse(&chain.read(&adt::wdt_path("Azeroth")).expect("wdt")).expect("wdt");
    // Northshire's tile, which carries the abbey, its roads and open grass.
    let tile = adt::Adt::parse(
        &chain.read(&adt::tile_path("Azeroth", 32, 48)).expect("tile"),
        wdt.big_alpha(),
    )
    .expect("parsing tile");

    let (mut mixed, mut multi_layer) = (0usize, 0usize);
    for chunk in &tile.chunks {
        if chunk.layers.len() < 2 {
            continue;
        }
        multi_layer += 1;
        let grid = adt::footing::footing_grid(chunk);
        assert_eq!(grid.len(), adt::footing::FOOTING_GRID * adt::footing::FOOTING_GRID);
        for cell in &grid {
            assert!(
                (*cell as usize) < chunk.layers.len(),
                "footing names layer {cell} of {}",
                chunk.layers.len()
            );
        }
        if grid.iter().any(|c| *c != grid[0]) {
            mixed += 1;
        }
    }

    assert!(multi_layer > 100, "only {multi_layer} chunks have more than one layer");
    assert!(
        mixed * 2 > multi_layer,
        "only {mixed} of {multi_layer} multi-layer chunks have more than one footing -- \
         a grid that is constant per chunk is not reading the alpha maps"
    );
}
