//! Loading a WMO into drawable geometry.
//!
//! Groups are merged into one vertex and index buffer. They are separate files
//! because the client draws them selectively through portals, but until portal
//! culling exists there is no reason to keep them apart on the GPU, and one
//! buffer means one bind per object.

use anyhow::{Context, Result};
use glam::Vec3;
use mpq::Chain;
use render::mesh::{BlendMode, GpuMesh, MeshVertex, RenderState, Winding};
use render::{texture::upload_blp, Gpu, UploadedTexture};

use crate::model::Draw;

pub struct LoadedWmo {
    pub mesh: GpuMesh,
    pub draws: Vec<Draw>,
    pub textures: Vec<UploadedTexture>,
    pub min: Vec3,
    pub max: Vec3,
    pub path: String,
    pub vertex_count: usize,
    pub triangle_count: usize,
    pub group_count: usize,
    /// Triangles skipped because they are collision geometry.
    pub collision_triangles: usize,
    pub doodad_sets: Vec<String>,
    pub missing_textures: Vec<String>,
}

/// Maps a WMO material onto pipeline state.
///
/// WMO blend modes are their own enumeration, not M2's: 0 is opaque, 1 is the
/// alpha-tested cutout used for railings and foliage, and the rest blend.
fn render_state(material: &wmo::Material) -> RenderState {
    let blend = match material.blend_mode {
        0 => BlendMode::Opaque,
        1 => BlendMode::AlphaKey,
        _ => BlendMode::Blend,
    };
    RenderState {
        blend,
        two_sided: material.two_sided(),
        depth_write: !blend.is_transparent(),
        // Opposite to M2. Culling with M2's convention removes every outward
        // surface: a roof disappears and you see the interior ceiling through
        // it.
        winding: Winding::CounterClockwise,
    }
}

pub fn load(
    gpu: &Gpu,
    chain: &mut Chain,
    path: &str,
    only_group: Option<usize>,
) -> Result<LoadedWmo> {
    if wmo::is_group_path(path) {
        anyhow::bail!("{path} is a group file; load the root .wmo instead");
    }
    let bytes = chain
        .read(path)
        .with_context(|| format!("reading {path}"))?;
    let root = wmo::Root::parse(&bytes).with_context(|| format!("parsing {path}"))?;
    // Group names live in the root; the groups themselves only store offsets.
    let group_names = wmo::Chunks::find(&bytes, b"MOGN").unwrap_or(&[]).to_vec();

    // One texture per material slot, resolved once and shared by every batch.
    let mut textures = Vec::new();
    let mut missing_textures = Vec::new();
    for material in &root.materials {
        let file = root.texture(material.texture1).to_string();
        let uploaded = (!file.is_empty())
            .then(|| {
                let bytes = chain.read(&file).ok()?;
                let parsed = blp::Blp::parse(&bytes).ok()?;
                Some(upload_blp(gpu, &parsed, &file))
            })
            .flatten();
        match uploaded {
            Some(t) => textures.push(t),
            None => {
                missing_textures.push(if file.is_empty() {
                    "<unnamed material texture>".to_string()
                } else {
                    file
                });
                textures.push(crate::model::placeholder(gpu));
            }
        }
    }
    if textures.is_empty() {
        textures.push(crate::model::placeholder(gpu));
    }

    let mut vertices: Vec<MeshVertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut draws: Vec<Draw> = Vec::new();
    let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let (mut group_count, mut collision_triangles) = (0usize, 0usize);

    for gi in 0..root.header.group_count as usize {
        if only_group.is_some_and(|want| want != gi) {
            continue;
        }
        let gpath = wmo::group_path(path, gi);
        let Ok(gbytes) = chain.read(&gpath) else {
            continue;
        };
        let Ok(group) = wmo::Group::parse(&gbytes, &group_names) else {
            continue;
        };
        if group.validate().is_err() || group.vertices.is_empty() {
            continue;
        }
        group_count += 1;

        // Every group indexes from zero into its own vertex array, so merging
        // means offsetting each group's indices by what came before.
        let base = vertices.len() as u32;
        for (i, position) in group.vertices.iter().enumerate() {
            let p = Vec3::from(*position);
            min = min.min(p);
            max = max.max(p);
            vertices.push(MeshVertex {
                position: *position,
                normal: group.normals.get(i).copied().unwrap_or([0.0, 0.0, 1.0]),
                uv: group.uvs.get(i).copied().unwrap_or([0.0, 0.0]),
                // WMO geometry is rigid; the skinning path passes it through.
                bone_indices: [0; 4],
                bone_weights: [0; 4],
            });
        }

        collision_triangles += group
            .triangle_materials
            .iter()
            .filter(|t| t.is_collision_only())
            .count();

        for batch in &group.batches {
            let Some(batch_indices) = group.batch_indices(batch) else {
                continue;
            };
            let material = root
                .materials
                .get(batch.material_id as usize)
                .copied()
                .unwrap_or(wmo::Material {
                    flags: 0,
                    shader: 0,
                    blend_mode: 0,
                    texture1: 0,
                    texture2: 0,
                    diffuse_color: 0,
                    ground_type: 0,
                });

            draws.push(Draw {
                first_index: indices.len() as u32,
                index_count: batch_indices.len() as u32,
                state: render_state(&material),
                texture: (batch.material_id as usize).min(textures.len() - 1),
                submesh_id: gi as u16,
            });
            indices.extend(batch_indices.iter().map(|&i| base + i as u32));
        }
    }

    if vertices.is_empty() {
        anyhow::bail!("{path} produced no geometry from {} groups", root.header.group_count);
    }

    // Opaque first so the depth buffer is populated before anything blends.
    draws.sort_by_key(|d| d.state.blend.is_transparent());

    let triangle_count = indices.len() / 3;
    Ok(LoadedWmo {
        mesh: GpuMesh::upload(gpu, &vertices, &indices),
        draws,
        textures,
        min,
        max,
        path: path.to_string(),
        vertex_count: vertices.len(),
        triangle_count,
        group_count,
        collision_triangles,
        doodad_sets: root
            .doodad_sets
            .iter()
            .map(|s| format!("{} ({})", s.name, s.count))
            .collect(),
        missing_textures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_wmo_blend_modes() {
        let material = |blend_mode| wmo::Material {
            flags: 0,
            shader: 0,
            blend_mode,
            texture1: 0,
            texture2: 0,
            diffuse_color: 0,
            ground_type: 0,
        };
        assert_eq!(render_state(&material(0)).blend, BlendMode::Opaque);
        assert_eq!(render_state(&material(1)).blend, BlendMode::AlphaKey);
        assert_eq!(render_state(&material(2)).blend, BlendMode::Blend);
        // Cutouts still write depth; only true blending stops.
        assert!(render_state(&material(1)).depth_write);
        assert!(!render_state(&material(2)).depth_write);
    }
}
