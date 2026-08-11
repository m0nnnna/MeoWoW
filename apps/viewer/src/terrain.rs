//! Turning a terrain tile into drawable geometry.
//!
//! Each chunk becomes its own draw call using its base texture layer. Alpha
//! blending between the up-to-four layers needs a dedicated shader and is not
//! wired up yet, so what renders is the height field with each chunk's dominant
//! texture -- enough to prove the geometry, which is the part that is easy to
//! get subtly wrong.

use std::collections::HashMap;

use anyhow::{Context, Result};
use glam::Vec3;
use mpq::Chain;
use render::mesh::{BlendMode, GpuMesh, MeshVertex, RenderState, Winding};
use render::{texture::upload_blp, Gpu, UploadedTexture};

use crate::model::Draw;

pub struct LoadedTerrain {
    pub mesh: GpuMesh,
    pub draws: Vec<Draw>,
    pub textures: Vec<UploadedTexture>,
    pub min: Vec3,
    pub max: Vec3,
    pub path: String,
    pub vertex_count: usize,
    pub triangle_count: usize,
    pub chunk_count: usize,
    pub holes: usize,
    pub doodad_placements: usize,
    pub object_placements: usize,
    pub missing_textures: Vec<String>,
}

/// Builds the 8x8 grid of quads for one chunk.
///
/// Each cell is four triangles fanning out from the inner-lattice sample at its
/// centre, which is what the extra 8x8 samples are for. Two triangles per cell
/// would ignore them and lose the detail they carry.
fn emit_chunk_indices(base: u32, chunk: &adt::Chunk, indices: &mut Vec<u32>) {
    for cell_y in 0..8usize {
        for cell_x in 0..8usize {
            // Holes are punched at 4x4 granularity, two cells per hole.
            if chunk.has_hole(cell_x / 2, cell_y / 2) {
                continue;
            }
            // Row `cell_y` starts a 9-sample outer row; the inner row follows.
            let outer = |row: usize, col: usize| base + (row * 17 + col) as u32;
            let inner = base + (cell_y * 17 + 9 + cell_x) as u32;

            let top_left = outer(cell_y, cell_x);
            let top_right = outer(cell_y, cell_x + 1);
            let bottom_left = outer(cell_y + 1, cell_x);
            let bottom_right = outer(cell_y + 1, cell_x + 1);

            for (a, b) in [
                (top_left, top_right),
                (top_right, bottom_right),
                (bottom_right, bottom_left),
                (bottom_left, top_left),
            ] {
                indices.extend_from_slice(&[inner, a, b]);
            }
        }
    }
}

pub fn load(gpu: &Gpu, chain: &mut Chain, map: &str, x: usize, y: usize) -> Result<LoadedTerrain> {
    let wdt_path = adt::wdt_path(map);
    let wdt = adt::Wdt::parse(&chain.read(&wdt_path)?)
        .with_context(|| format!("parsing {wdt_path}"))?;
    if !wdt.has_tile(x, y) {
        anyhow::bail!("{map} has no tile at {x},{y}");
    }

    let path = adt::tile_path(map, x, y);
    let tile = adt::Adt::parse(&chain.read(&path)?, wdt.big_alpha())
        .with_context(|| format!("parsing {path}"))?;

    // Textures are shared across chunks, so upload each one once.
    let mut textures: Vec<UploadedTexture> = Vec::new();
    let mut missing_textures = Vec::new();
    let mut by_path: HashMap<String, usize> = HashMap::new();
    for name in &tile.textures {
        let slot = textures.len();
        let uploaded = chain
            .read(name)
            .ok()
            .and_then(|b| blp::Blp::parse(&b).ok())
            .map(|parsed| upload_blp(gpu, &parsed, name));
        match uploaded {
            Some(t) => textures.push(t),
            None => {
                missing_textures.push(name.clone());
                textures.push(crate::model::placeholder(gpu));
            }
        }
        by_path.insert(name.clone(), slot);
    }
    if textures.is_empty() {
        textures.push(crate::model::placeholder(gpu));
    }

    let mut vertices: Vec<MeshVertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut draws: Vec<Draw> = Vec::new();
    let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let mut holes = 0usize;

    for (ci, chunk) in tile.chunks.iter().enumerate() {
        let base = vertices.len() as u32;
        for i in 0..chunk.heights.len() {
            let p = Vec3::from(chunk.vertex_position(i));
            min = min.min(p);
            max = max.max(p);

            // Normals are signed bytes with the axes in the game's order.
            let n = chunk.normals.get(i).copied().unwrap_or([0, 0, 127]);
            let normal = Vec3::new(n[0] as f32, n[1] as f32, n[2] as f32) / 127.0;

            // Terrain UVs repeat eight times across a chunk, matching how the
            // tileset textures are authored.
            let (row, col, inner) = adt::lattice_coords(i);
            let column_offset = if inner { 0.5 } else { 0.0 };
            let u = (col as f32 + column_offset) / 8.0;
            let v = row as f32 * 0.5 / 8.0;

            vertices.push(MeshVertex {
                position: p.into(),
                normal: normal.into(),
                uv: [u, v],
                bone_indices: [0; 4],
                bone_weights: [0; 4],
            });
        }

        let start = indices.len() as u32;
        emit_chunk_indices(base, chunk, &mut indices);
        let count = indices.len() as u32 - start;
        if count == 0 {
            continue;
        }
        holes += (0..16).filter(|i| chunk.has_hole(i % 4, i / 4)).count();

        // Layer 0 is the chunk's base texture and is always fully opaque.
        let texture = chunk
            .layers
            .first()
            .and_then(|l| tile.textures.get(l.texture_id as usize))
            .and_then(|name| by_path.get(name).copied())
            .unwrap_or(0);

        draws.push(Draw {
            first_index: start,
            index_count: count,
            state: RenderState {
                blend: BlendMode::Opaque,
                two_sided: false,
                depth_write: true,
                // Terrain winds clockwise like M2, *not* like WMO. Guessing
                // from the neighbouring format culls almost every triangle and
                // leaves a handful of slivers rather than an empty screen,
                // which reads as broken geometry instead of a culling bug.
                winding: Winding::Clockwise,
            },
            texture,
            submesh_id: ci as u16,
        });
    }

    if vertices.is_empty() {
        anyhow::bail!("{path} produced no geometry");
    }

    let triangle_count = indices.len() / 3;
    Ok(LoadedTerrain {
        mesh: GpuMesh::upload(gpu, &vertices, &indices),
        draws,
        textures,
        min,
        max,
        path,
        vertex_count: vertices.len(),
        triangle_count,
        chunk_count: tile.chunks.len(),
        holes,
        doodad_placements: tile.doodads.len(),
        object_placements: tile.objects.len(),
        missing_textures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_chunk(holes: u16) -> adt::Chunk {
        adt::Chunk {
            index: (0, 0),
            position: [0.0; 3],
            area_id: 0,
            holes,
            heights: vec![0.0; adt::HEIGHTS_PER_CHUNK],
            normals: vec![[0, 0, 127]; adt::HEIGHTS_PER_CHUNK],
            layers: Vec::new(),
            alpha_maps: Vec::new(),
            doodad_refs: Vec::new(),
            object_refs: Vec::new(),
        }
    }

    /// Every cell is four triangles around its centre sample, so a full chunk
    /// is 64 cells x 4 triangles x 3 indices.
    #[test]
    fn a_full_chunk_emits_every_cell() {
        let mut indices = Vec::new();
        emit_chunk_indices(0, &flat_chunk(0), &mut indices);
        assert_eq!(indices.len(), 8 * 8 * 4 * 3);
        // Nothing may reference a sample outside the chunk.
        assert!(indices.iter().all(|&i| (i as usize) < adt::HEIGHTS_PER_CHUNK));
    }

    /// A hole covers a 2x2 block of cells, not one.
    #[test]
    fn holes_remove_four_cells_each() {
        let mut indices = Vec::new();
        emit_chunk_indices(0, &flat_chunk(0b1), &mut indices);
        let full = 8 * 8 * 4 * 3;
        assert_eq!(indices.len(), full - 2 * 2 * 4 * 3);
    }

    /// Indices are offset by the chunk's base so several chunks can share one
    /// buffer.
    #[test]
    fn indices_are_offset_by_the_base() {
        let mut indices = Vec::new();
        emit_chunk_indices(1000, &flat_chunk(0), &mut indices);
        assert!(indices.iter().all(|&i| i >= 1000));
    }
}
