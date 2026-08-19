//! What a character is standing *on*: which of a chunk's texture layers wins
//! at a point, so the ground can be asked what it is made of.
//!
//! Terrain is drawn as up to four textures blended by three alpha maps, and
//! that blend is the only thing in the whole format that says a particular
//! square yard is road rather than grass. The area id says which *zone* it is
//! and nothing about the surface; the liquid sheets say where water is and
//! nothing about the bank.
//!
//! # The weights are the shader's, not a second opinion
//!
//! The terrain shader composites the layers as a chain of `mix`es:
//!
//! ```wgsl
//! color = mix(color, layer1, a.r);
//! color = mix(color, layer2, a.g);
//! color = mix(color, layer3, a.b);
//! ```
//!
//! so layer 3 contributes `a3`, layer 2 `a2 * (1 - a3)`, layer 1
//! `a1 * (1 - a2) * (1 - a3)` and layer 0 whatever is left. [`weights`]
//! computes exactly that, and [`dominant_layer`] takes the largest. Deriving
//! the footing from anything else -- "the last layer over half", say -- would
//! be a second answer to a question the picture already answers, and the two
//! would agree until somebody changed the shader.
//!
//! # Why the grid is coarse
//!
//! An alpha map is 64x64 over a 33-yard chunk, which is finer than a footstep
//! needs and 4KB per chunk to keep resident. [`footing_grid`] reduces it to
//! 16x16 -- a little over two yards a cell, against a walk cycle that covers
//! two and a half yards a second -- and that is 256 bytes per chunk instead.
//! The alpha maps themselves are uploaded to the GPU and dropped, so this grid
//! is what survives to answer for a character's feet.

use crate::{Chunk, ALPHA_SIZE, UNIT_SIZE};

/// Cells across one chunk in a footing grid.
pub const FOOTING_GRID: usize = 16;

/// Texture layers a chunk can carry, and so the range of a footing value.
pub const MAX_LAYERS: usize = 4;

/// How much each layer contributes to the drawn colour at one alpha cell.
///
/// Indexed by layer. Always four entries even where the chunk has fewer, and
/// the absent ones are zero -- a layer that is not there cannot be stood on.
pub fn weights(chunk: &Chunk, row: usize, col: usize) -> [f32; MAX_LAYERS] {
    let at = |map: &Vec<u8>| -> f32 {
        map.get(row * ALPHA_SIZE + col).copied().unwrap_or(0) as f32 / 255.0
    };
    let a: Vec<f32> = chunk.alpha_maps.iter().map(at).collect();
    let mut out = [0.0; MAX_LAYERS];
    // Walked from the top layer down, carrying how much of the picture the
    // layers above have already claimed -- which is the `mix` chain read
    // backwards.
    let mut remaining = 1.0f32;
    for layer in (0..chunk.layers.len().min(MAX_LAYERS)).rev() {
        // `alpha_maps` holds one map per layer *after* the first, so layer
        // `n`'s map is at `n - 1` and layer 0 has none: it is the base and
        // takes whatever the others leave.
        let alpha = if layer == 0 {
            1.0
        } else {
            a.get(layer - 1).copied().unwrap_or(0.0)
        };
        out[layer] = remaining * alpha;
        remaining *= 1.0 - alpha;
    }
    out
}

/// Which layer a foot lands on at one alpha cell.
///
/// `None` for a chunk with no layers at all, which is real: unfinished terrain
/// and some instance floors carry none, and inventing layer 0 for them would
/// be asserting a material that is not there.
pub fn dominant_layer(chunk: &Chunk, row: usize, col: usize) -> Option<usize> {
    if chunk.layers.is_empty() {
        return None;
    }
    let weights = weights(chunk, row, col);
    (0..chunk.layers.len().min(MAX_LAYERS))
        .max_by(|a, b| weights[*a].total_cmp(&weights[*b]))
}

/// Reduces a chunk's blend to one dominant layer per [`FOOTING_GRID`] cell.
///
/// Sampled at each cell's centre rather than averaged: a road one cell wide
/// should read as road at its middle, where averaging the alpha across the
/// cell and *then* picking a winner would let the surrounding grass outvote
/// it. `u8::MAX` marks a cell whose chunk names no layers.
pub fn footing_grid(chunk: &Chunk) -> Vec<u8> {
    let per_cell = ALPHA_SIZE / FOOTING_GRID;
    (0..FOOTING_GRID * FOOTING_GRID)
        .map(|i| {
            let (row, col) = (i / FOOTING_GRID, i % FOOTING_GRID);
            let centre = |c: usize| c * per_cell + per_cell / 2;
            dominant_layer(chunk, centre(row), centre(col)).map_or(u8::MAX, |l| l as u8)
        })
        .collect()
}

/// Which footing-grid cell a world position falls in, as `(row, col)`.
///
/// **The axes are the renderer's own**, and that is deliberate rather than
/// merely convenient. The terrain mesh gives each vertex `uv = (col / 8, row /
/// 8)` from [`crate::lattice_coords`], and the shader samples the blend map
/// with that `uv` -- so the alpha map's *column* runs along the axis
/// [`crate::height_in_chunk`] calls `col` (world Y) and its *row* along the one
/// that function calls `row` (world X). Rebuilding that from the format
/// documentation instead would be a second derivation of something that has to
/// agree exactly with what is drawn, and a footing rotated a quarter turn
/// against the picture is a road that sounds like grass a few yards to one
/// side.
pub fn footing_cell(position: [f32; 3], x: f32, y: f32) -> Option<(usize, usize)> {
    let u = (position[0] - x) / UNIT_SIZE;
    let v = (position[1] - y) / UNIT_SIZE;
    // Same tolerance and clamp as `height_in_chunk`, for the same reason: a
    // point exactly on a chunk's far edge lands a hair outside it in float.
    const EDGE_TOLERANCE: f32 = 0.01;
    if !(-EDGE_TOLERANCE..=8.0 + EDGE_TOLERANCE).contains(&u)
        || !(-EDGE_TOLERANCE..=8.0 + EDGE_TOLERANCE).contains(&v)
    {
        return None;
    }
    let cell = |t: f32| {
        ((t.clamp(0.0, 8.0) / 8.0) * FOOTING_GRID as f32).floor() as usize
    }
    .min(FOOTING_GRID - 1);
    Some((cell(u), cell(v)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Layer;

    fn chunk(layers: usize, alphas: Vec<Vec<u8>>) -> Chunk {
        Chunk {
            index: (0, 0),
            position: [0.0, 0.0, 0.0],
            area_id: 0,
            holes: 0,
            heights: vec![0.0; crate::HEIGHTS_PER_CHUNK],
            normals: Vec::new(),
            layers: (0..layers)
                .map(|i| Layer {
                    texture_id: i as u32,
                    flags: 0,
                    alpha_offset: 0,
                    effect_id: 0,
                })
                .collect(),
            alpha_maps: alphas,
            doodad_refs: Vec::new(),
            object_refs: Vec::new(),
        }
    }

    /// The base layer wins where nothing is painted over it, and the painted
    /// layer wins where it is opaque. The two halves are asserted together
    /// because a rule that only ever answers "layer 0" passes the first.
    #[test]
    fn the_opaque_layer_is_the_one_underfoot() {
        let mut alpha = vec![0u8; ALPHA_SIZE * ALPHA_SIZE];
        for row in 0..ALPHA_SIZE {
            for col in 0..ALPHA_SIZE / 2 {
                alpha[row * ALPHA_SIZE + col] = 255;
            }
        }
        let chunk = chunk(2, vec![alpha]);
        assert_eq!(dominant_layer(&chunk, 10, 10), Some(1));
        assert_eq!(dominant_layer(&chunk, 10, 50), Some(0));
    }

    /// A layer painted over another is what a foot lands on, even though the
    /// one below it is still fully opaque -- which is the whole reason the
    /// weights are computed rather than the alphas compared.
    #[test]
    fn a_later_layer_covers_an_earlier_one() {
        let full = vec![255u8; ALPHA_SIZE * ALPHA_SIZE];
        let chunk = chunk(3, vec![full.clone(), full]);
        assert_eq!(dominant_layer(&chunk, 0, 0), Some(2));
        let weights = weights(&chunk, 0, 0);
        assert_eq!(weights[2], 1.0);
        assert_eq!(weights[1], 0.0);
        assert_eq!(weights[0], 0.0);
    }

    /// A chunk with no layers has no footing, rather than layer 0 by default.
    #[test]
    fn no_layers_is_no_footing() {
        assert_eq!(dominant_layer(&chunk(0, Vec::new()), 0, 0), None);
        assert!(footing_grid(&chunk(0, Vec::new()))
            .iter()
            .all(|c| *c == u8::MAX));
    }

    /// The cell lookup runs the same way round as the height lookup: its first
    /// coordinate tracks world X and its second world Y, both counting
    /// *inwards* from the chunk's origin corner.
    #[test]
    fn footing_cells_run_the_same_way_the_heights_do() {
        let origin = [100.0, 200.0, 0.0];
        assert_eq!(footing_cell(origin, 100.0, 200.0), Some((0, 0)));
        // A whole chunk in from the corner on both axes is the far cell.
        let span = 8.0 * UNIT_SIZE;
        assert_eq!(
            footing_cell(origin, 100.0 - span, 200.0 - span),
            Some((FOOTING_GRID - 1, FOOTING_GRID - 1))
        );
        // Moving along world X alone moves only the first coordinate. Taken
        // at three tenths of the way rather than a half: a cell boundary lands
        // either side of itself depending on the last bit of the division, and
        // a test that straddles one is testing float rounding.
        let (row, col) = footing_cell(origin, 100.0 - span * 0.3, 200.0).unwrap();
        assert_eq!(col, 0);
        assert_eq!(row, 4);
        // Off the chunk entirely is not a cell at all.
        assert_eq!(footing_cell(origin, 100.0 + 5.0, 200.0), None);
    }
}
