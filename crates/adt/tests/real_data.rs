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
