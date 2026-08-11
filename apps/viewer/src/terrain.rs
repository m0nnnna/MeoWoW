//! Turning a terrain tile into drawable geometry.
//!
//! Each chunk becomes its own draw call using its base texture layer. Alpha
//! blending between the up-to-four layers needs a dedicated shader and is not
//! wired up yet, so what renders is the height field with each chunk's dominant
//! texture -- enough to prove the geometry, which is the part that is easy to
//! get subtly wrong.

use anyhow::{Context, Result};
use glam::Vec3;
use mpq::Chain;
use render::mesh::{GpuMesh, MeshVertex};
use render::terrain::{pack_blend_map, TerrainRenderer, MAX_LAYERS};
use render::{texture::upload_blp, Gpu, UploadedTexture};

/// One chunk's slice of the tile mesh, with its layers already bound.
pub struct ChunkDraw {
    pub first_index: u32,
    pub index_count: u32,
    pub bind_group: wgpu::BindGroup,
}

pub struct LoadedTerrain {
    pub mesh: GpuMesh,
    pub chunks: Vec<ChunkDraw>,
    /// Kept alive because the chunk bind groups reference their views.
    pub textures: Vec<UploadedTexture>,
    /// Likewise: dropping these would invalidate the bindings.
    #[allow(dead_code)]
    pub blend_maps: Vec<wgpu::Texture>,
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

pub fn load(
    gpu: &Gpu,
    renderer: &TerrainRenderer,
    chain: &mut Chain,
    map: &str,
    x: usize,
    y: usize,
) -> Result<LoadedTerrain> {
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
    for name in &tile.textures {
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
    }
    // A bind group cannot leave a slot empty, so chunks with fewer than four
    // layers pad with this. Its blend channel is zero, so it never shows.
    let blank = crate::model::placeholder(gpu);

    let mut vertices: Vec<MeshVertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut chunk_draws: Vec<ChunkDraw> = Vec::new();
    let mut blend_maps: Vec<wgpu::Texture> = Vec::new();
    let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let mut holes = 0usize;

    for chunk in tile.chunks.iter() {
        let base = vertices.len() as u32;
        for i in 0..chunk.heights.len() {
            let p = Vec3::from(chunk.vertex_position(i));
            min = min.min(p);
            max = max.max(p);

            let n = chunk.normals.get(i).copied().unwrap_or([0, 0, 127]);
            let normal = Vec3::new(n[0] as f32, n[1] as f32, n[2] as f32) / 127.0;

            // Chunk-local coordinates, 0 to 1. The shader multiplies up for the
            // tileset and uses these directly for the blend map.
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

        let blend = upload_blend_map(gpu, chunk);
        let views: Vec<&wgpu::TextureView> = (0..MAX_LAYERS)
            .map(|layer| {
                chunk
                    .layers
                    .get(layer)
                    .and_then(|l| textures.get(l.texture_id as usize))
                    .map(|t| &t.view)
                    .unwrap_or(&blank.view)
            })
            .collect();
        let layers: [&wgpu::TextureView; MAX_LAYERS] =
            [views[0], views[1], views[2], views[3]];

        let blend_view = blend.create_view(&wgpu::TextureViewDescriptor::default());
        chunk_draws.push(ChunkDraw {
            first_index: start,
            index_count: count,
            bind_group: renderer.bind_chunk(gpu, &layers, &blend_view),
        });
        blend_maps.push(blend);
    }

    if vertices.is_empty() {
        anyhow::bail!("{path} produced no geometry");
    }

    let triangle_count = indices.len() / 3;
    textures.push(blank);
    Ok(LoadedTerrain {
        mesh: GpuMesh::upload(gpu, &vertices, &indices),
        chunks: chunk_draws,
        textures,
        blend_maps,
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

/// Uploads a chunk's packed alpha maps as a single RGBA texture.
fn upload_blend_map(gpu: &Gpu, chunk: &adt::Chunk) -> wgpu::Texture {
    let size = adt::ALPHA_SIZE as u32;
    let packed = pack_blend_map(&chunk.alpha_maps, adt::ALPHA_SIZE);

    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("blend map"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // Coverage, not colour: no sRGB curve.
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &packed,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size * 4),
            rows_per_image: Some(size),
        },
        wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
    );
    texture
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
