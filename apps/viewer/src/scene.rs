//! Assembling a terrain tile together with everything standing on it.
//!
//! This is the first thing in the project that is a *world* rather than a single
//! asset: terrain, the buildings placed on it, and the doodads scattered across
//! it, all in one coordinate system.
//!
//! Placements are grouped by model path, so a forest of five hundred identical
//! trees loads one mesh and draws it with five hundred instances.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use glam::{Mat4, Quat, Vec3};
use mpq::Chain;
use render::mesh::{GpuMesh, Instance, InstanceBuffer};
use render::{Gpu, UploadedTexture};

use crate::model::Draw;

/// One mesh drawn at one or more transforms.
pub struct Placed {
    pub mesh: GpuMesh,
    pub draws: Vec<Draw>,
    pub textures: Vec<UploadedTexture>,
    /// Range within the scene's instance buffer.
    pub instance_start: u32,
    pub instance_count: u32,
}

pub struct WorldScene {
    /// One entry per tile; terrain has its own pipeline.
    pub terrain: Vec<crate::terrain::LoadedTerrain>,
    pub items: Vec<Placed>,
    pub instances: InstanceBuffer,
    pub min: Vec3,
    pub max: Vec3,
    pub label: String,
    pub tiles_loaded: usize,
    pub terrain_triangles: usize,
    pub object_instances: usize,
    pub doodad_instances: usize,
    pub unique_models: usize,
    pub draw_calls: usize,
    pub skipped: Vec<String>,
}

/// Half the world grid, in units. Placement coordinates are measured inwards
/// from the far corner, so converting them means subtracting from this.
const MAP_CENTRE: f32 = 32.0 * adt::TILE_SIZE;

/// Converts an ADT placement position into world space.
///
/// Placements are stored with the axes permuted relative to terrain vertices:
/// the stored `y` is height, and the other two run *inwards* from the grid
/// corner. Getting this wrong puts every object in a plausible-looking but
/// entirely different part of the map.
pub fn placement_position(raw: [f32; 3]) -> Vec3 {
    Vec3::new(MAP_CENTRE - raw[2], MAP_CENTRE - raw[0], raw[1])
}

/// Builds the rotation for a placement.
///
/// Rotations are Euler degrees in the game's internal Y-up space. Yaw carries
/// almost all of the meaning -- doodads are rarely tilted -- and it is offset by
/// 90 degrees because the stored angle is measured from a different axis than
/// the model's forward.
pub fn placement_rotation(rotation: [f32; 3]) -> Quat {
    Quat::from_rotation_z((rotation[1] - 90.0).to_radians())
        * Quat::from_rotation_y((-rotation[0]).to_radians())
        * Quat::from_rotation_x(rotation[2].to_radians())
}

fn transform(raw_position: [f32; 3], rotation: [f32; 3], scale: f32) -> Mat4 {
    Mat4::from_scale_rotation_translation(
        Vec3::splat(scale),
        placement_rotation(rotation),
        placement_position(raw_position),
    )
}

/// Loads a square block of tiles and everything placed on them.
///
/// `radius` 0 is a single tile, 1 is the 3x3 around it, and so on. Tiles that
/// do not exist are skipped rather than failing: coastlines are ragged, and a
/// block near one is mostly ocean.
pub fn load(
    gpu: &Gpu,
    terrain_renderer: &render::TerrainRenderer,
    chain: &mut Chain,
    map: &str,
    centre: (usize, usize),
    radius: usize,
    max_doodads: usize,
) -> Result<WorldScene> {
    let wdt = adt::Wdt::parse(&chain.read(&adt::wdt_path(map))?)?;

    let mut instances: Vec<Instance> = Vec::new();
    let mut terrain_parts: Vec<crate::terrain::LoadedTerrain> = Vec::new();
    let mut items: Vec<Placed> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let (mut object_min, mut object_max) = (min, max);
    let mut terrain_triangles = 0usize;
    let mut tiles_loaded = 0usize;

    // Placements are deduplicated by unique id: an object straddling a tile
    // border is listed by *every* tile it touches, so loading a block without
    // this draws the same building several times over itself.
    let mut object_groups: HashMap<String, Vec<Mat4>> = HashMap::new();
    let mut doodad_groups: HashMap<String, Vec<Mat4>> = HashMap::new();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut doodad_budget = max_doodads;

    let low = |v: usize| v.saturating_sub(radius);
    for y in low(centre.1)..=(centre.1 + radius).min(adt::TILES_PER_MAP - 1) {
        for x in low(centre.0)..=(centre.0 + radius).min(adt::TILES_PER_MAP - 1) {
            if !wdt.has_tile(x, y) {
                continue;
            }
            let terrain = match crate::terrain::load(gpu, terrain_renderer, chain, map, x, y) {
                Ok(t) => t,
                Err(e) => {
                    skipped.push(format!("terrain {x},{y}: {e}"));
                    continue;
                }
            };
            tiles_loaded += 1;
            terrain_triangles += terrain.triangle_count;
            min = min.min(terrain.min);
            max = max.max(terrain.max);

            // Terrain is already in world space and uses its own pipeline.
            terrain_parts.push(terrain);

            let Ok(bytes) = chain.read(&adt::tile_path(map, x, y)) else {
                continue;
            };
            let Ok(parsed) = adt::Adt::parse(&bytes, wdt.big_alpha()) else {
                continue;
            };

            for placement in &parsed.objects {
                if !seen.insert(placement.unique_id) {
                    continue;
                }
                object_groups
                    .entry(placement.path.to_string())
                    .or_default()
                    .push(transform(placement.position, placement.rotation, 1.0));
            }
            for placement in &parsed.doodads {
                if doodad_budget == 0 {
                    break;
                }
                if !seen.insert(placement.unique_id) {
                    continue;
                }
                doodad_budget -= 1;
                doodad_groups
                    .entry(placement.path.to_string())
                    .or_default()
                    .push(transform(
                        placement.position,
                        placement.rotation,
                        placement.scale,
                    ));
            }
        }
    }

    if tiles_loaded == 0 {
        anyhow::bail!("no tiles exist around {},{} in {map}", centre.0, centre.1);
    }
    object_min = object_min.min(min);
    object_max = object_max.max(max);

    let (mut object_instances, mut doodad_instances) = (0usize, 0usize);

    let mut ordered: Vec<(String, Vec<Mat4>)> = object_groups.into_iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, transforms) in ordered {
        match crate::world_object::load(gpu, chain, &path, None) {
            Ok(loaded) => {
                object_instances += transforms.len();
                push_group(
                    &mut items,
                    &mut instances,
                    &mut object_min,
                    &mut object_max,
                    loaded.mesh,
                    loaded.draws,
                    loaded.textures,
                    (loaded.min, loaded.max),
                    &transforms,
                );
            }
            Err(e) => skipped.push(format!("{path}: {e}")),
        }
    }

    let mut ordered: Vec<(String, Vec<Mat4>)> = doodad_groups.into_iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, transforms) in ordered {
        // Doodads draw in bind pose; nothing here animates yet.
        match crate::model::load(gpu, chain, &path, &crate::model::Variations::default(), 0) {
            Ok(loaded) => {
                doodad_instances += transforms.len();
                push_group(
                    &mut items,
                    &mut instances,
                    &mut object_min,
                    &mut object_max,
                    loaded.mesh,
                    loaded.draws,
                    loaded.textures,
                    (loaded.min, loaded.max),
                    &transforms,
                );
            }
            Err(e) => skipped.push(format!("{path}: {e}")),
        }
    }

    let draw_calls: usize = items.iter().map(|i| i.draws.len()).sum::<usize>()
        + terrain_parts.iter().map(|t| t.chunks.len()).sum::<usize>();
    let unique_models = items.len();
    Ok(WorldScene {
        instances: InstanceBuffer::upload(gpu, &instances),
        terrain: terrain_parts,
        items,
        min,
        max,
        label: format!(
            "{map} {}x{} tiles around {},{}",
            radius * 2 + 1,
            radius * 2 + 1,
            centre.0,
            centre.1
        ),
        tiles_loaded,
        terrain_triangles,
        object_instances,
        doodad_instances,
        unique_models,
        draw_calls,
        skipped,
    })
}

#[allow(clippy::too_many_arguments)]
fn push_group(
    items: &mut Vec<Placed>,
    instances: &mut Vec<Instance>,
    min: &mut Vec3,
    max: &mut Vec3,
    mesh: GpuMesh,
    draws: Vec<Draw>,
    textures: Vec<UploadedTexture>,
    local_bounds: (Vec3, Vec3),
    transforms: &[Mat4],
) {
    let start = instances.len() as u32;
    for t in transforms {
        // Grow the scene bounds by the transformed corners, so framing accounts
        // for where objects actually ended up.
        for i in 0..8 {
            let corner = Vec3::new(
                if i & 1 == 0 { local_bounds.0.x } else { local_bounds.1.x },
                if i & 2 == 0 { local_bounds.0.y } else { local_bounds.1.y },
                if i & 4 == 0 { local_bounds.0.z } else { local_bounds.1.z },
            );
            let world = t.transform_point3(corner);
            *min = min.min(world);
            *max = max.max(world);
        }
        instances.push(Instance::from_cols_array_2d(t.to_cols_array_2d()));
    }
    items.push(Placed {
        mesh,
        draws,
        textures,
        instance_start: start,
        instance_count: transforms.len() as u32,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Placement coordinates run inwards from the grid corner, and the stored
    /// middle component is height.
    #[test]
    fn placement_positions_are_permuted_and_inverted() {
        // Northshire Abbey, as stored in Azeroth_32_48.adt.
        let world = placement_position([17245.0, 80.0, 25964.0]);
        assert!((world.z - 80.0).abs() < 1e-3, "height should pass through");

        // Tile 32,48 spans these ranges; the abbey must land inside them.
        let tile_x = (32.0 - 48.0) * adt::TILE_SIZE;
        let tile_y = (32.0 - 32.0) * adt::TILE_SIZE;
        assert!(
            world.x <= tile_x && world.x >= tile_x - adt::TILE_SIZE,
            "x {} outside {}..{}",
            world.x,
            tile_x - adt::TILE_SIZE,
            tile_x
        );
        assert!(
            world.y <= tile_y && world.y >= tile_y - adt::TILE_SIZE,
            "y {} outside {}..{}",
            world.y,
            tile_y - adt::TILE_SIZE,
            tile_y
        );
    }

    /// The conversion is its own mirror, so the map centre maps to itself.
    #[test]
    fn map_centre_is_a_fixed_point() {
        let world = placement_position([MAP_CENTRE, 0.0, MAP_CENTRE]);
        assert!(world.x.abs() < 1e-3 && world.y.abs() < 1e-3);
    }

    /// An unrotated placement must still be upright: yaw only turns about the
    /// up axis.
    #[test]
    fn rotation_keeps_up_pointing_up() {
        let up = placement_rotation([0.0, 0.0, 0.0]) * Vec3::Z;
        assert!((up - Vec3::Z).length() < 1e-5, "up became {up:?}");

        let turned = placement_rotation([0.0, 45.0, 0.0]) * Vec3::Z;
        assert!((turned - Vec3::Z).length() < 1e-5, "yaw tilted the model");
    }

    #[test]
    fn yaw_rotates_about_the_vertical_axis() {
        let a = placement_rotation([0.0, 90.0, 0.0]) * Vec3::X;
        // 90 degrees of yaw, minus the 90 degree offset, is no rotation.
        assert!((a - Vec3::X).length() < 1e-5, "got {a:?}");

        let b = placement_rotation([0.0, 180.0, 0.0]) * Vec3::X;
        assert!((b - Vec3::Y).length() < 1e-4, "got {b:?}");
    }
}
