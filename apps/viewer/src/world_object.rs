//! Loading a WMO into drawable geometry.
//!
//! Groups are merged into one vertex and index buffer. They are separate files
//! because the client draws them selectively through portals, but until portal
//! culling exists there is no reason to keep them apart on the GPU, and one
//! buffer means one bind per object.

use anyhow::{Context, Result};
use glam::{Mat4, Quat, Vec3};
use mpq::Chain;
use render::mesh::{BlendMode, GpuMesh, MeshVertex, RenderState, Winding};
use render::{texture::upload_blp, Gpu, UploadedTexture};
use std::collections::HashMap;

use crate::model::Draw;

pub struct LoadedWmo {
    pub mesh: GpuMesh,
    pub draws: Vec<Draw>,
    pub textures: Vec<UploadedTexture>,
    pub min: Vec3,
    pub max: Vec3,
    pub path: String,
    pub wmo_id: u32,
    pub group_bounds: Vec<(Vec3, Vec3)>,
    pub group_surface_ids: Vec<u32>,
    /// Whether each group is enclosed -- lit by the WMO's own lighting
    /// rather than the outdoor sun. See [`wmo::GroupInfo::is_interior`].
    ///
    /// **What tells a building's floor from a fence's rail.** Both carry
    /// ordinary walkable collision, so "a modelled floor was found here" --
    /// what `App::modeled_floor` used to mean, on its own -- cannot tell an
    /// abbey interior from a garden fence the character has jumped onto.
    /// Reported live as rain cutting out while jumping a fence outdoors.
    pub group_interior: Vec<bool>,
    pub vertex_count: usize,
    pub triangle_count: usize,
    pub group_count: usize,
    /// Triangles skipped because they are collision geometry.
    pub collision_triangles: usize,
    /// Everything solid, in model space.
    ///
    /// **Both the drawn triangles and the collision-only ones**, because a
    /// wall you can see is as solid as one you cannot. The 0xFF material marks
    /// the triangles that exist *only* to be collided with -- an invisible
    /// barrier across a doorway, a ramp under a stair -- and they were being
    /// counted and dropped. Keeping the drawn ones as well is what makes the
    /// abbey's actual walls stop anybody.
    pub collision: Vec<[[f32; 3]; 3]>,
    /// What each collision triangle is made of, as a `TerrainType` row id,
    /// parallel to [`Self::collision`].
    ///
    /// `u8::MAX` where the triangle has no material at all (the `0xFF`
    /// collision-only marker) or its material says `None` -- and those two are
    /// deliberately the same answer, because "an invisible barrier" and "a
    /// wall that declines to say" both mean this client must not claim to know
    /// what a foot landed on. See [`wmo::Material::ground_type`].
    pub collision_footing: Vec<u8>,
    pub collision_area: Vec<u32>,
    pub doodads: Vec<Vec<Doodad>>,
    pub doodad_sets: Vec<String>,
    pub missing_textures: Vec<String>,
}

#[derive(Clone)]
pub struct Doodad {
    pub path: String,
    pub transform: Mat4,
}

/// The `TerrainType` row a WMO material names when it declines to say what
/// its surface is made of.
///
/// **Row 10, `None` -- not row 0.** Out on the terrain the silent value is 0
/// and row 0 is also `Dirt`; in here they are different rows and 91% of
/// materials are the silent one. Reading this as 0 would make every wall in
/// the game claim to be dirt. See [`wmo::Material::ground_type`].
const NO_TERRAIN: u32 = 10;

#[derive(Clone, Copy)]
pub struct WmoArea {
    pub row_id: u32,
    pub wmo_id: u32,
    pub area_table_id: u32,
    pub ambience_id: Option<u32>,
    pub zone_music: Option<u32>,
}

#[derive(Default)]
pub struct WmoAreas {
    by_group: HashMap<(u32, u32), WmoArea>,
    by_root: HashMap<u32, WmoArea>,
    by_row: HashMap<u32, WmoArea>,
}

impl WmoAreas {
    pub fn load(chain: &mut Chain) -> Self {
        let Some(table) = chain
            .read(dbc::schema::WmoAreaTable::PATH)
            .ok()
            .and_then(|bytes| dbc::schema::WmoAreaTable::parse(&bytes).ok())
        else {
            return Self::default();
        };
        let mut by_group = HashMap::new();
        let mut by_root = HashMap::new();
        let mut by_row = HashMap::new();
        for row in table.iter() {
            let area = WmoArea {
                row_id: row.id(),
                wmo_id: row.wmo_id(),
                area_table_id: row.area_table_id(),
                ambience_id: (row.ambience_id() != 0).then_some(row.ambience_id()),
                zone_music: (row.zone_music() != 0).then_some(row.zone_music()),
            };
            if area.area_table_id == 0 && area.ambience_id.is_none() && area.zone_music.is_none() {
                continue;
            }
            if row.wmo_group_id() == -1 {
                by_root.insert(row.wmo_id(), area);
            } else if let Ok(group) = u32::try_from(row.wmo_group_id()) {
                by_group.insert((row.wmo_id(), group), area);
            }
            by_row.insert(area.row_id, area);
        }
        Self {
            by_group,
            by_root,
            by_row,
        }
    }

    fn get(&self, wmo_id: u32, group_id: u32) -> Option<WmoArea> {
        self.by_group
            .get(&(wmo_id, group_id))
            .copied()
            .or_else(|| self.by_root.get(&wmo_id).copied())
            .map(|area| self.with_root(area))
    }

    pub fn by_id(&self, row_id: u32) -> Option<WmoArea> {
        self.by_row.get(&row_id).copied().map(|area| self.with_root(area))
    }

    fn with_root(&self, area: WmoArea) -> WmoArea {
        let Some(root) = self.by_root.get(&area.wmo_id).copied() else {
            return area;
        };
        WmoArea {
            row_id: area.row_id,
            wmo_id: area.wmo_id,
            area_table_id: (area.area_table_id != 0)
                .then_some(area.area_table_id)
                .or((root.area_table_id != 0).then_some(root.area_table_id))
                .unwrap_or(0),
            ambience_id: area.ambience_id.or(root.ambience_id),
            zone_music: area.zone_music.or(root.zone_music),
        }
    }
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

fn doodad_transform(doodad: &wmo::DoodadDef) -> Mat4 {
    Mat4::from_scale_rotation_translation(
        Vec3::splat(doodad.scale),
        Quat::from_array(doodad.rotation).normalize(),
        Vec3::from(doodad.position),
    )
}

pub fn load(
    gpu: &Gpu,
    chain: &mut Chain,
    path: &str,
    only_group: Option<usize>,
) -> Result<LoadedWmo> {
    load_with_areas(gpu, chain, path, only_group, None)
}

pub fn load_with_areas(
    gpu: &Gpu,
    chain: &mut Chain,
    path: &str,
    only_group: Option<usize>,
    areas: Option<&WmoAreas>,
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
    let mut collision: Vec<[[f32; 3]; 3]> = Vec::new();
    let mut collision_footing: Vec<u8> = Vec::new();
    let mut collision_area: Vec<u32> = Vec::new();
    let group_bounds = root
        .groups
        .iter()
        .map(|group| (Vec3::from(group.bounding_box.0), Vec3::from(group.bounding_box.1)))
        .collect();
    // Off the root's own lightweight per-group table, not the group file --
    // the same source `group_bounds` above already reads, so this costs
    // nothing extra to open.
    let group_interior = root.groups.iter().map(|group| group.is_interior()).collect();
    let mut group_surface_ids = vec![0; root.header.group_count as usize];

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
        let surface_id = areas
            .and_then(|areas| areas.get(root.header.wmo_id, group.group_id))
            .map(|area| area.row_id)
            .unwrap_or(0);
        if let Some(surface) = group_surface_ids.get_mut(gi) {
            *surface = surface_id;
        }

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

        // Straight off `MOVI`, not off the render batches: a batch list omits
        // exactly the collision-only triangles, which are the ones most
        // deliberately placed to stop somebody.
        for (index, triangle) in group.indices.chunks_exact(3).enumerate() {
            let point = |i: u16| group.vertices.get(i as usize).copied();
            if let (Some(a), Some(b), Some(c)) =
                (point(triangle[0]), point(triangle[1]), point(triangle[2]))
            {
                collision.push([a, b, c]);
                // `MOPY` is parallel to `MOVI`'s triples, and the group's own
                // validation asserts exactly that -- `wow-cli wmo survey` runs
                // it over every group in the archives, so the index below is
                // checked rather than assumed.
                collision_footing.push(
                    group
                        .triangle_materials
                        .get(index)
                        .filter(|t| !t.is_collision_only())
                        .and_then(|t| root.materials.get(t.material_id as usize))
                        .map(|m| m.ground_type)
                        // A row id is 0..=11, so it fits a byte with room to
                        // spare for the "nothing" marker.
                        .filter(|row| *row != NO_TERRAIN && *row < u8::MAX as u32)
                        .map_or(u8::MAX, |row| row as u8),
                );
                collision_area.push(surface_id);
            }
        }

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
                texture_transform: None,
                submesh_id: gi as u16,
            });
            indices.extend(batch_indices.iter().map(|&i| base + i as u32));
        }
    }

    if vertices.is_empty() || indices.is_empty() {
        anyhow::bail!(
            "{path} produced no drawable geometry from {} groups",
            root.header.group_count
        );
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
        wmo_id: root.header.wmo_id,
        group_bounds,
        group_surface_ids,
        group_interior,
        vertex_count: vertices.len(),
        triangle_count,
        group_count,
        collision_triangles,
        collision,
        collision_footing,
        collision_area,
        doodads: root
            .doodad_sets
            .iter()
            .map(|set| {
                root.doodads_in_set(set)
                    .iter()
                    .filter(|doodad| !doodad.path.is_empty())
                    .map(|doodad| Doodad {
                        path: doodad.path.clone(),
                        transform: doodad_transform(doodad),
                    })
                    .collect()
            })
            .collect(),
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

    #[test]
    fn composes_doodad_translation_rotation_and_scale() {
        let rotation = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2).to_array();
        let doodad = wmo::DoodadDef {
            path: "child.m2".into(),
            position: [3.0, 4.0, 5.0],
            rotation,
            scale: 2.0,
            color: [255; 4],
        };
        let point = doodad_transform(&doodad).transform_point3(Vec3::X);
        assert!((point - Vec3::new(3.0, 6.0, 5.0)).length() < 1e-5);
    }

    #[test]
    fn wmo_group_area_inherits_root_audio_and_keeps_its_area() {
        let root = WmoArea {
            row_id: 10,
            wmo_id: 42,
            area_table_id: 100,
            ambience_id: Some(7),
            zone_music: Some(9),
        };
        let group = WmoArea {
            row_id: 11,
            wmo_id: 42,
            area_table_id: 200,
            ambience_id: None,
            zone_music: None,
        };
        let mut areas = WmoAreas::default();
        areas.by_root.insert(root.wmo_id, root);
        areas.by_group.insert((group.wmo_id, 3), group);
        areas.by_row.insert(group.row_id, group);
        let resolved = areas.get(42, 3).expect("group area");
        assert_eq!(resolved.area_table_id, 200);
        assert_eq!(resolved.ambience_id, Some(7));
        assert_eq!(resolved.zone_music, Some(9));
        assert_eq!(areas.by_id(11).map(|area| area.zone_music), Some(Some(9)));
    }

    #[test]
    fn wmo_group_without_a_row_uses_the_root_area() {
        let root = WmoArea {
            row_id: 10,
            wmo_id: 42,
            area_table_id: 100,
            ambience_id: Some(7),
            zone_music: Some(9),
        };
        let mut areas = WmoAreas::default();
        areas.by_root.insert(root.wmo_id, root);
        let resolved = areas.get(42, 3).expect("root area");
        assert_eq!(resolved.row_id, 10);
        assert_eq!(resolved.area_table_id, 100);
        assert_eq!(resolved.ambience_id, Some(7));
        assert_eq!(resolved.zone_music, Some(9));
    }
}
