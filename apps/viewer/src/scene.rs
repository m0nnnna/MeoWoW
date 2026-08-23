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
use render::mesh::{BoneBuffer, GpuMesh, Instance, InstanceBuffer, MeshRenderer};
use render::{Gpu, UploadedTexture};

use crate::model::Draw;

/// One mesh drawn at one or more transforms.
pub struct Placed {
    pub mesh: GpuMesh,
    pub draws: Vec<Draw>,
    pub textures: Vec<UploadedTexture>,
    pub texture_animation: crate::model::TextureAnimation,
    /// Range within the scene's instance buffer.
    pub instance_start: u32,
    pub instance_count: u32,
    pub animation: Option<PlacedAnimation>,
}

pub struct PlacedAnimation {
    pub buffer: BoneBuffer,
    bones: std::rc::Rc<Vec<m2::AnimatedBone>>,
    global_sequences: Vec<u32>,
    sequence: usize,
    duration_ms: u32,
    flags: u32,
}

impl PlacedAnimation {
    fn new(gpu: &Gpu, meshes: &MeshRenderer, model: &crate::model::LoadedModel) -> Option<Self> {
        let sequence = map_animation_sequence(&model.sequences)?;
        let definition = model.sequences[sequence];
        if !model.bones.iter().any(|bone| bone.is_animated()) {
            return None;
        }
        Some(Self {
            buffer: meshes.create_bones(gpu, model.bones.len()),
            bones: std::rc::Rc::clone(&model.bones),
            global_sequences: model.texture_animation.global_sequences().to_vec(),
            sequence,
            duration_ms: definition.duration_ms,
            flags: definition.flags,
        })
    }

    fn update(&self, gpu: &Gpu, meshes: &MeshRenderer, elapsed_ms: u32) {
        let pose = m2::Model::pose_bones_with_global_loops(
            &self.bones,
            self.sequence,
            map_animation_time(self.duration_ms, self.flags, elapsed_ms),
            &self.global_sequences,
        );
        let upload: Vec<[[f32; 4]; 4]> =
            pose.iter().map(|matrix| matrix.to_cols_array_2d()).collect();
        meshes.update_bones(gpu, &self.buffer, &upload);
    }
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

impl WorldScene {
    pub fn update_animations(&self, gpu: &Gpu, meshes: &MeshRenderer, elapsed_ms: u32) {
        for item in &self.items {
            if let Some(animation) = item.animation.as_ref() {
                animation.update(gpu, meshes, elapsed_ms);
                item.texture_animation.update(
                    gpu,
                    meshes,
                    animation.sequence,
                    map_animation_time(animation.duration_ms, animation.flags, elapsed_ms),
                );
            } else {
                item.texture_animation.update(gpu, meshes, 0, elapsed_ms);
            }
        }
    }
}

/// Half the world grid, in units. Placement coordinates are measured inwards
/// from the far corner, so converting them means subtracting from this.
const MAP_CENTRE: f32 = 32.0 * adt::TILE_SIZE;

pub(crate) fn map_animation_sequence(sequences: &[m2::Sequence]) -> Option<usize> {
    sequences
        .iter()
        .position(|sequence| sequence.id == 0)
        .or_else(|| sequences.iter().position(|sequence| sequence.id == 0x93))
        .or_else(|| (!sequences.is_empty()).then_some(0))
}

pub(crate) fn map_animation_time(duration_ms: u32, flags: u32, elapsed_ms: u32) -> u32 {
    let duration_ms = duration_ms.max(1);
    if flags & 1 != 0 {
        elapsed_ms.min(duration_ms)
    } else {
        elapsed_ms % duration_ms
    }
}

/// Converts an ADT placement position into world space.
///
/// Placements are stored with the axes permuted relative to terrain vertices:
/// the stored `y` is height, and the other two run *inwards* from the grid
/// corner. Getting this wrong puts every object in a plausible-looking but
/// entirely different part of the map.
pub fn placement_position(raw: [f32; 3]) -> Vec3 {
    Vec3::new(MAP_CENTRE - raw[2], MAP_CENTRE - raw[0], raw[1])
}

/// Builds the rotation for a **doodad** placement -- an M2 on the terrain.
///
/// No offset: an M2's forward is +X, so a stored yaw is already a world yaw.
///
/// This shipped as `-90`, was changed to `+90`, then to `+180`, and every one
/// of those was wrong. `-90` and `+90` are a quarter turn out and lay every
/// fence in Elwynn across its own line; `+180` was derived from a belief that
/// an M2 faces -X, which turned out to be an artefact of inside-out culling
/// rather than a fact about models.
///
/// What holds it down now is a measurement that does not care about any of
/// that: a fence is a *run*, and the run's direction comes from the
/// placements' own positions with no rotation involved at all. Across three
/// runs at different angles, `direction - yaw` is one constant and
/// `direction + yaw` is not -- so the yaw is not mirrored, and the offset is
/// zero modulo a half turn. The half turn is then settled by entities, which
/// are the same file format and demonstrably need none. See
/// [`tests::a_fence_run_lies_along_its_stored_yaw`].
pub fn placement_rotation(rotation: [f32; 3]) -> Quat {
    Quat::from_rotation_z(rotation[1].to_radians())
        * Quat::from_rotation_y((-rotation[0]).to_radians())
        * Quat::from_rotation_x(rotation[2].to_radians())
}

/// Builds the rotation for a **world object** placement -- a WMO.
///
/// A half turn, where a doodad takes none. The two really do differ: WMO and
/// M2 are different formats, authored to different forward axes, and only the
/// M2 one matches the network's heading convention.
///
/// Northshire Abbey appeared to want a quarter turn where the fences wanted a
/// half one, on the evidence that a quarter turn showed its portal from the
/// starting lawn. It did -- but that only says the door faced the lawn *under
/// that rotation*, not that the door belongs there. The building has four
/// sides and every rotation shows a door to somebody.
///
/// What settles where the entrance really is: **the lamp pillars beside it.**
/// They are doodads, so their world positions are fixed no matter what any
/// rotation does, and the cobbled path -- painted into the terrain, equally
/// unrotatable -- runs between them. Two references that cannot move say the
/// entrance faces the path, and only a half turn puts it there.
///
/// The general shape of the mistake: a movable thing was checked against
/// another movable thing. The fix was to find something nailed down.
pub fn object_rotation(rotation: [f32; 3]) -> Quat {
    Quat::from_rotation_z((rotation[1] + 180.0).to_radians())
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
#[allow(clippy::too_many_arguments)]
pub fn load(
    gpu: &Gpu,
    meshes: &MeshRenderer,
    terrain_renderer: &render::TerrainRenderer,
    liquid_renderer: &render::LiquidRenderer,
    liquid_types: &mut crate::liquid::LiquidTypes,
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
    let mut object_groups: HashMap<String, Vec<(Mat4, usize)>> = HashMap::new();
    let mut doodad_groups: HashMap<String, Vec<Mat4>> = HashMap::new();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut doodad_budget = max_doodads;

    let low = |v: usize| v.saturating_sub(radius);
    for y in low(centre.1)..=(centre.1 + radius).min(adt::TILES_PER_MAP - 1) {
        for x in low(centre.0)..=(centre.0 + radius).min(adt::TILES_PER_MAP - 1) {
            if !wdt.has_tile(x, y) {
                continue;
            }
            let terrain = match crate::terrain::load(
                gpu,
                terrain_renderer,
                liquid_renderer,
                liquid_types,
                chain,
                map,
                x,
                y,
            ) {
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
                    .push((transform(placement.position, placement.rotation, 1.0), placement.doodad_set as usize));
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

    let mut ordered: Vec<(String, Vec<(Mat4, usize)>)> = object_groups.into_iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, placements) in ordered {
        match crate::world_object::load(gpu, chain, &path, None) {
            Ok(loaded) => {
                let texture_animation = crate::model::TextureAnimation::empty(
                    gpu,
                    meshes,
                    loaded.draws.len(),
                );
                let transforms: Vec<Mat4> = placements.iter().map(|(transform, _)| *transform).collect();
                for (parent, set) in &placements {
                    let Some(doodads) = loaded.doodads.get(*set) else {
                        continue;
                    };
                    for doodad in doodads {
                        doodad_groups.entry(doodad.path.clone()).or_default().push(*parent * doodad.transform);
                    }
                }
                object_instances += transforms.len();
                push_group(
                    &mut items,
                    &mut instances,
                    &mut object_min,
                    &mut object_max,
                    loaded.mesh,
                    loaded.draws,
                    loaded.textures,
                    texture_animation,
                    None,
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
        match crate::model::load(gpu, meshes, chain, &path, &crate::model::Variations::default(), 0) {
            Ok(loaded) => {
                let animation = PlacedAnimation::new(gpu, meshes, &loaded);
                doodad_instances += transforms.len();
                push_group(
                    &mut items,
                    &mut instances,
                    &mut object_min,
                    &mut object_max,
                    loaded.mesh,
                    loaded.draws,
                    loaded.textures,
                    loaded.texture_animation,
                    animation,
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
    texture_animation: crate::model::TextureAnimation,
    animation: Option<PlacedAnimation>,
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
        texture_animation,
        instance_start: start,
        instance_count: transforms.len() as u32,
        animation,
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

    /// The yaw offset, measured against real placements rather than chosen.
    ///
    /// A rotation cannot be judged by eye: a wrong offset looks right for
    /// anything at zero or a half turn and wrong everywhere else, which is
    /// how Northshire Abbey read correctly while every fence in Elwynn was
    /// across its own line. Both `-90` and `+90` shipped, and both were the
    /// same 90 degrees wrong in opposite directions.
    ///
    /// So this measures. A fence is a *run*: copies of one model laid end to
    /// end along a straight line, and that line's direction comes from the
    /// placements' own positions with no rotation involved at all. Three runs
    /// of `ElwynnWoodFence01` in `Azeroth_32_48`, with their stored yaw and
    /// the direction their pieces actually lie in:
    ///
    /// | stored yaw | run direction |
    /// |---|---|
    /// | -130.0 | 50.5 |
    /// | -45.5 | 313.4 |
    /// | -45.0 | 313.4 |
    ///
    /// `direction - yaw` is the same constant for all three and
    /// `direction + yaw` is not, which is what rules out a *mirrored* yaw as
    /// well as fixing the offset. The model is 4.3 units long in X against
    /// 0.3 in Y, so its long axis is local X, and the constant is zero.
    #[test]
    fn a_fence_run_lies_along_its_stored_yaw() {
        const RUNS: [(f32, f32); 3] = [(-130.0, 50.5), (-45.5, 313.4), (-45.0, 313.4)];
        for (yaw, direction) in RUNS {
            let along = placement_rotation([0.0, yaw, 0.0]) * Vec3::X;
            let got = along.y.atan2(along.x).to_degrees();
            // A run has no near end and no far end, so the comparison is
            // modulo a half turn -- which is exactly why this test cannot
            // settle the remaining 180 on its own. An asymmetric building
            // does that; see `placement_rotation`.
            let delta = (got - direction + 90.0).rem_euclid(180.0) - 90.0;
            assert!(
                delta.abs() < 3.0,
                "stored yaw {yaw} put the fence at {got}, but its run lies at {direction}"
            );
        }
    }

    /// Pins the yaw offset, including its sign.
    ///
    /// The sign is the whole point: this test previously asserted the opposite
    /// one and passed, because both are internally consistent and nothing here
    /// knows which way a building faces. What decided it was a render of
    /// Northshire Abbey showing its door rather than its back wall -- see
    /// [`placement_rotation`]. These numbers exist so that result cannot be
    /// undone by accident.
    #[test]
    fn yaw_rotates_about_the_vertical_axis() {
        // A doodad takes no offset, so a stored yaw of 90 puts +X onto +Y.
        let a = placement_rotation([0.0, 90.0, 0.0]) * Vec3::X;
        assert!((a - Vec3::Y).length() < 1e-5, "got {a:?}");

        // A world object takes a half turn on top, which is what puts the
        // abbey's door on the side the path arrives from.
        let b = object_rotation([0.0, 90.0, 0.0]) * Vec3::X;
        assert!((b + Vec3::Y).length() < 1e-4, "got {b:?}");
    }
    fn sequence(id: u16, duration_ms: u32, flags: u32) -> m2::Sequence {
        m2::Sequence {
            id,
            variation: 0,
            duration_ms,
            move_speed: 0.0,
            flags,
            blend_time: 0,
            variation_next: -1,
            alias_next: 0,
        }
    }

    #[test]
    fn map_animation_resolves_id_zero_before_sequence_zero() {
        let sequences = [sequence(7, 10, 0), sequence(0, 20, 0)];
        assert_eq!(map_animation_sequence(&sequences), Some(1));
    }

    #[test]
    fn map_animation_uses_the_client_terminal_fallbacks() {
        let fallback = [sequence(7, 10, 0), sequence(0x93, 20, 0)];
        assert_eq!(map_animation_sequence(&fallback), Some(1));
        assert_eq!(map_animation_sequence(&[sequence(7, 10, 0)]), Some(0));
    }

    #[test]
    fn map_animation_flags_choose_loop_or_hold() {
        assert_eq!(map_animation_time(100, 0, 225), 25);
        assert_eq!(map_animation_time(100, 1, 225), 100);
    }
}
