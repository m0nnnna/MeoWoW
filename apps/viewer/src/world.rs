//! A streaming world: tiles load and unload as the camera moves.
//!
//! Three things make this work at world scale.
//!
//! **Models are cached across tiles.** Elwynn's trees appear on every tile;
//! loading them per tile would multiply memory by the number of tiles resident.
//!
//! **Each placement is owned by exactly one tile** — the tile its position falls
//! in. An object straddling a border is listed by every tile it touches, and
//! without a single owner it is drawn once per listing and evicted only
//! partially.
//!
//! **Loading is budgeted per frame.** Reading and uploading a tile takes long
//! enough to be visible as a stall, so only a couple are admitted per frame and
//! the rest wait.

use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::Instant;

use glam::{Mat4, Vec3};
use mpq::Chain;
use render::mesh::{BoneBuffer, Instance, InstanceBuffer, MeshRenderer};
use render::{Gpu, TerrainRenderer, UploadedTexture};

use crate::model::Draw;
use crate::scene::{placement_position, placement_rotation};
use crate::terrain::LoadedTerrain;

/// Tiles kept beyond the load radius before being evicted.
///
/// Without this margin a camera sitting on a tile boundary reloads the same
/// tiles every time it drifts a metre.
const EVICT_MARGIN: i32 = 1;

/// Tiles admitted per frame. Loading is synchronous, so this caps the stall.
const LOAD_BUDGET: usize = 2;

/// A model resident on the GPU, shared by every tile that places it.
pub struct CachedModel {
    pub mesh: render::mesh::GpuMesh,
    pub draws: Vec<Draw>,
    pub binds: Vec<wgpu::BindGroup>,
    /// Populated whenever the loader has them (every M2, not a WMO), even
    /// though only replicated entities currently animate. Doodads and
    /// buildings are always drawn in the bind pose regardless, so carrying
    /// this costs them nothing and keeps one loader path instead of two.
    pub bones: Vec<m2::AnimatedBone>,
    pub sequences: Vec<m2::Sequence>,
    /// The model's own extent, in its local space. Carried so a replicated
    /// entity can be clicked on: a click needs a volume to test a ray against,
    /// and the model already knows how big it is. `None` for a WMO, which is
    /// never a click target.
    pub bounds: Option<(Vec3, Vec3)>,
    /// Held because the bind groups reference their views.
    #[allow(dead_code)]
    textures: Vec<UploadedTexture>,
}

/// One model and the transforms it takes on a single tile.
pub struct Group {
    pub model: Rc<CachedModel>,
    pub instances: InstanceBuffer,
    pub count: u32,
    /// Set only for a replicated-entity group: display id plus whether this
    /// is the group's moving bucket (walk cycle) or standing bucket (stand
    /// cycle), used to look up its own animated bone buffer instead of
    /// drawing with the scene's shared bind pose. The bucket has to be part
    /// of the key, not just the display id -- a species with both moving and
    /// standing instances needs two different poses live at once, and a
    /// display-id-only key would have the later of the two overwrite the
    /// other's buffer every rebuild. `None` for every tile-owned group
    /// (doodads and buildings never animate) and when the model has no
    /// matching sequence to play.
    pub animation: Option<(u32, bool)>,
}

pub struct Tile {
    pub terrain: LoadedTerrain,
    pub groups: Vec<Group>,
}

#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub tiles_resident: usize,
    pub tiles_pending: usize,
    pub models_cached: usize,
    pub instances: usize,
    pub draw_calls: usize,
    pub tiles_failed: usize,
    /// Server-placed objects currently drawn.
    pub entities: usize,
}

/// One server-placed object waiting to be turned into an instance.
pub struct EntityPlacement {
    pub display_id: u32,
    pub position: Vec3,
    /// Heading in radians, as the server reports it.
    pub orientation: f32,
    pub scale: f32,
    /// Whether a monster move is in flight for this specific instance. Drives
    /// whether its display id's group is drawn with a walk cycle -- see
    /// `Group::animated_display_id`.
    pub moving: bool,
    /// How this instance is dressed, for a player character.
    ///
    /// `None` for everything else, which is nearly everything: a creature's
    /// appearance is already in its display id. A player's is not -- display
    /// 49 is every human male alive -- so the look travels with the placement
    /// and takes part in the cache key. Without that, the second human male in
    /// view wears the first one's skin.
    pub look: Option<std::rc::Rc<crate::character::Look>>,
    /// Distinguishes one look from another for caching. Zero means undressed.
    pub look_key: u64,
}

pub struct World {
    map: String,
    wdt: adt::Wdt,
    radius: i32,
    max_doodads: usize,
    /// `None` marks a model that failed to load, so it is not retried on every
    /// tile that places it.
    cache: HashMap<String, Option<Rc<CachedModel>>>,
    /// Entity models are cached separately, by display id rather than by path.
    ///
    /// Not merely convenient: a creature display id supplies *skins* on top of
    /// its model path, so two display ids sharing one path are different-looking
    /// creatures. Keying by path would give the second one the first one's hide.
    /// Keyed by display *and* look: two players of one race share a display
    /// id and not a face. Zero is the undressed key every creature uses.
    entity_cache: HashMap<(u32, u64), Option<Rc<CachedModel>>>,
    /// Objects the server placed, as opposed to the ones the map files did.
    /// Owned by the world rather than a tile: they move, and they do not belong
    /// to the tile they happen to be standing on.
    entities: Vec<Group>,
    /// Animated bone buffers for replicated-entity groups, keyed by
    /// `Group::animation` and reused across rebuilds rather than reallocated:
    /// `update_bones` rewrites a buffer's contents in place, so the GPU
    /// buffer and its bind group only need to be created once per
    /// (display id, moving) pair, not once per rebuild tick.
    entity_bones: HashMap<(u32, bool), BoneBuffer>,
    /// Origin for the animation clock. The server does not say which frame of
    /// a walk cycle a creature is on -- nothing does, since 3.3.5a leaves that
    /// entirely to the client -- so any fixed origin that advances is enough
    /// to loop a sequence convincingly.
    started: Instant,
    tiles: HashMap<(i32, i32), Tile>,
    pending: VecDeque<(i32, i32)>,
    queued: HashSet<(i32, i32)>,
    failed: HashSet<(i32, i32)>,
    pub stats: Stats,
}

/// Which tile a world position sits on.
///
/// Inverts the tile grid: a tile's origin is at `(32 - tile_y) * TILE_SIZE` on
/// x and `(32 - tile_x) * TILE_SIZE` on y, with both axes running negative, so
/// the axes are swapped as well as inverted.
pub fn tile_at(position: Vec3) -> (i32, i32) {
    let x = (32.0 - position.y / adt::TILE_SIZE).floor() as i32;
    let y = (32.0 - position.x / adt::TILE_SIZE).floor() as i32;
    (x, y)
}

/// Centre of a tile in world space.
pub fn tile_centre(tile: (i32, i32)) -> Vec3 {
    Vec3::new(
        (32.0 - tile.1 as f32 - 0.5) * adt::TILE_SIZE,
        (32.0 - tile.0 as f32 - 0.5) * adt::TILE_SIZE,
        0.0,
    )
}

impl World {
    pub fn new(chain: &mut Chain, map: &str, radius: i32, max_doodads: usize) -> anyhow::Result<Self> {
        let wdt = adt::Wdt::parse(&chain.read(&adt::wdt_path(map))?)?;
        Ok(Self {
            map: map.to_string(),
            wdt,
            radius: radius.max(0),
            max_doodads,
            cache: HashMap::new(),
            entity_cache: HashMap::new(),
            entities: Vec::new(),
            entity_bones: HashMap::new(),
            started: Instant::now(),
            tiles: HashMap::new(),
            pending: VecDeque::new(),
            queued: HashSet::new(),
            failed: HashSet::new(),
            stats: Stats::default(),
        })
    }

    pub fn has_tile(&self, tile: (i32, i32)) -> bool {
        tile.0 >= 0
            && tile.1 >= 0
            && self.wdt.has_tile(tile.0 as usize, tile.1 as usize)
    }

    /// A reasonable starting position: above the centre of a tile, high enough
    /// to clear its terrain.
    pub fn spawn_above(&self, chain: &mut Chain, tile: (i32, i32)) -> Vec3 {
        let mut centre = tile_centre(tile);
        let highest = chain
            .read(&adt::tile_path(&self.map, tile.0 as usize, tile.1 as usize))
            .ok()
            .and_then(|b| adt::Adt::parse(&b, self.wdt.big_alpha()).ok())
            .map(|t| {
                t.chunks
                    .iter()
                    .flat_map(|c| c.heights.iter().map(move |h| h + c.position[2]))
                    .fold(f32::MIN, f32::max)
            })
            .unwrap_or(0.0);
        centre.z = highest + 120.0;
        centre
    }

    /// Brings the resident set in line with where the camera is.
    pub fn update(
        &mut self,
        gpu: &Gpu,
        meshes: &mut MeshRenderer,
        terrain_renderer: &TerrainRenderer,
        chain: &mut Chain,
        camera: Vec3,
    ) {
        let centre = tile_at(camera);

        // Evict first, so memory is released before anything new is admitted.
        let limit = self.radius + EVICT_MARGIN;
        self.tiles.retain(|tile, _| {
            (tile.0 - centre.0).abs() <= limit && (tile.1 - centre.1).abs() <= limit
        });
        self.pending.retain(|tile| {
            (tile.0 - centre.0).abs() <= self.radius
                && (tile.1 - centre.1).abs() <= self.radius
        });
        self.queued = self.pending.iter().copied().collect();

        // Queue what is missing, nearest first, so the view fills in outwards.
        let mut wanted: Vec<(i32, i32)> = Vec::new();
        for dy in -self.radius..=self.radius {
            for dx in -self.radius..=self.radius {
                let tile = (centre.0 + dx, centre.1 + dy);
                if self.has_tile(tile)
                    && !self.tiles.contains_key(&tile)
                    && !self.queued.contains(&tile)
                    && !self.failed.contains(&tile)
                {
                    wanted.push(tile);
                }
            }
        }
        wanted.sort_by_key(|t| (t.0 - centre.0).pow(2) + (t.1 - centre.1).pow(2));
        for tile in wanted {
            self.queued.insert(tile);
            self.pending.push_back(tile);
        }

        for _ in 0..LOAD_BUDGET {
            let Some(tile) = self.pending.pop_front() else {
                break;
            };
            self.queued.remove(&tile);
            match self.load_tile(gpu, meshes, terrain_renderer, chain, tile) {
                Ok(loaded) => {
                    self.tiles.insert(tile, loaded);
                }
                Err(e) => {
                    tracing::warn!("tile {},{} failed: {e}", tile.0, tile.1);
                    self.failed.insert(tile);
                }
            }
        }

        self.refresh_stats();
    }

    fn refresh_stats(&mut self) {
        let entities: usize = self.entities.iter().map(|g| g.count as usize).sum();
        let instances: usize = self
            .tiles
            .values()
            .flat_map(|t| t.groups.iter())
            .map(|g| g.count as usize)
            .sum::<usize>()
            + entities;
        let draw_calls: usize = self
            .tiles
            .values()
            .map(|t| {
                t.terrain.chunks.len()
                    + t.groups.iter().map(|g| g.model.draws.len()).sum::<usize>()
            })
            .sum::<usize>()
            + self
                .entities
                .iter()
                .map(|g| g.model.draws.len())
                .sum::<usize>();
        self.stats = Stats {
            tiles_resident: self.tiles.len(),
            tiles_pending: self.pending.len(),
            models_cached: self.cache.values().filter(|m| m.is_some()).count()
                + self.entity_cache.values().filter(|m| m.is_some()).count(),
            instances,
            draw_calls,
            tiles_failed: self.failed.len(),
            entities,
        };
    }

    fn load_tile(
        &mut self,
        gpu: &Gpu,
        meshes: &mut MeshRenderer,
        terrain_renderer: &TerrainRenderer,
        chain: &mut Chain,
        tile: (i32, i32),
    ) -> anyhow::Result<Tile> {
        let (x, y) = (tile.0 as usize, tile.1 as usize);
        let terrain = crate::terrain::load(gpu, terrain_renderer, chain, &self.map, x, y)?;
        let parsed = adt::Adt::parse(
            &chain.read(&adt::tile_path(&self.map, x, y))?,
            self.wdt.big_alpha(),
        )?;

        // Own only the placements whose position falls on this tile, so a
        // border-straddling object belongs to exactly one owner and is neither
        // drawn twice nor left behind when a neighbour is evicted.
        let mut groups: HashMap<String, Vec<Mat4>> = HashMap::new();
        let mut budget = self.max_doodads;
        for placement in &parsed.objects {
            let position = placement_position(placement.position);
            if tile_at(position) != tile {
                continue;
            }
            groups
                .entry(placement.path.to_string())
                .or_default()
                .push(Mat4::from_scale_rotation_translation(
                    Vec3::ONE,
                    placement_rotation(placement.rotation),
                    position,
                ));
        }
        for placement in &parsed.doodads {
            if budget == 0 {
                break;
            }
            let position = placement_position(placement.position);
            if tile_at(position) != tile {
                continue;
            }
            budget -= 1;
            groups
                .entry(placement.path.to_string())
                .or_default()
                .push(Mat4::from_scale_rotation_translation(
                    Vec3::splat(placement.scale),
                    placement_rotation(placement.rotation),
                    position,
                ));
        }

        let mut built = Vec::new();
        for (path, transforms) in groups {
            let Some(model) = self.model(gpu, meshes, chain, &path) else {
                continue;
            };
            meshes.prepare(gpu, model.draws.iter().map(|d| d.state));
            let raw: Vec<Instance> = transforms
                .iter()
                .map(|t| Instance::from_cols_array_2d(t.to_cols_array_2d()))
                .collect();
            built.push(Group {
                model,
                instances: InstanceBuffer::upload(gpu, &raw),
                count: raw.len() as u32,
                animation: None,
            });
        }

        Ok(Tile {
            terrain,
            groups: built,
        })
    }

    /// Loads a model, or returns the cached one. Failures are cached too.
    fn model(
        &mut self,
        gpu: &Gpu,
        meshes: &MeshRenderer,
        chain: &mut Chain,
        path: &str,
    ) -> Option<Rc<CachedModel>> {
        if let Some(cached) = self.cache.get(path) {
            return cached.clone();
        }

        let lower = path.to_lowercase();
        let built = if lower.ends_with(".wmo") {
            // No skeleton to speak of, so nothing to animate.
            crate::world_object::load(gpu, chain, path, None)
                .map(|w| (w.mesh, w.draws, w.textures, Vec::new(), Vec::new(), None))
                .ok()
        } else {
            crate::model::load(gpu, chain, path, &crate::model::Variations::default(), 0)
                .map(|m| {
                    (
                        m.mesh,
                        m.draws,
                        m.textures,
                        m.bones,
                        m.sequences,
                        Some((m.min, m.max)),
                    )
                })
                .ok()
        };

        let entry = built.map(|(mesh, draws, textures, bones, sequences, bounds)| {
            let binds = textures
                .iter()
                .map(|t| meshes.material_bind_group(gpu, &t.view))
                .collect();
            Rc::new(CachedModel {
                mesh,
                draws,
                binds,
                bones,
                sequences,
                bounds,
                textures,
            })
        });
        if entry.is_none() {
            tracing::debug!("could not load {path}");
        }
        self.cache.insert(path.to_string(), entry.clone());
        entry
    }

    pub fn tiles(&self) -> impl Iterator<Item = &Tile> {
        self.tiles.values()
    }

    /// Objects placed by the server, drawn alongside the map's own geometry.
    pub fn entities(&self) -> &[Group] {
        &self.entities
    }

    /// How big a replicated entity's model is, for hit-testing a click.
    ///
    /// Only answers for a model already loaded, which is the right limit: an
    /// entity whose model has not loaded is not on screen either, and letting
    /// a click select something invisible would be worse than letting it miss.
    pub fn entity_bounds(&self, display_id: u32) -> Option<(Vec3, Vec3)> {
        // Bounds are a property of the mesh, not of how it is dressed, so the
        // undressed entry answers for every look of the same model.
        self.entity_cache.get(&(display_id, 0))?.as_ref()?.bounds
    }

    /// Replaces the server-placed objects.
    ///
    /// Returns how many could not be drawn, which is worth surfacing: a world
    /// missing half its creatures because their models failed to load looks
    /// exactly like a world where the protocol never reported them.
    ///
    /// A display id splits into up to two groups -- moving and standing --
    /// rather than one, because several instances routinely share a display
    /// id (a zone's wolves, say) and animating or not animating *all* of them
    /// together is wrong either way: seen live, it read as "every creature
    /// plays a walk cycle forever," because in a populated zone there is
    /// almost always at least one instance of a given species moving at any
    /// moment. The split costs nothing extra worth avoiding: the model is
    /// already cached by display id in `entity_cache`, so the second lookup
    /// for the same display id is a clone of the same `Rc`, not a reload.
    pub fn set_entities(
        &mut self,
        gpu: &Gpu,
        meshes: &mut MeshRenderer,
        chain: &mut Chain,
        placements: &[EntityPlacement],
    ) -> usize {
        // Keyed by look as well as display: see `EntityPlacement::look`.
        let mut looks: HashMap<u64, Option<std::rc::Rc<crate::character::Look>>> = HashMap::new();
        let mut grouped: HashMap<(u32, bool, u64), Vec<Mat4>> = HashMap::new();
        for placement in placements {
            looks.insert(placement.look_key, placement.look.clone());
            grouped
                .entry((placement.display_id, placement.moving, placement.look_key))
                .or_default()
                .push(Mat4::from_scale_rotation_translation(
                    Vec3::splat(placement.scale),
                    // No quarter-turn here, unlike the doodad path: that
                    // offset corrects for ADT placement rotations being
                    // measured from a different axis than an M2's forward
                    // (see `docs/RENDERING.md`, "Placement coordinates"), a
                    // fact about *that* data source, not about M2 models in
                    // general. Network orientation is a different value with
                    // a different, already-consistent convention: direction
                    // of travel is `(orientation.cos(), orientation.sin())`
                    // everywhere else in this codebase (`Connection::walk`,
                    // `drive_live_movement`, `interpolated_position`), and an
                    // M2's forward is +X, so rotating by the raw angle already
                    // points +X at that direction with nothing to correct.
                    // The quarter-turn was carried over from the doodad
                    // formula without being re-derived for this different
                    // input, and facing was never checked against a live
                    // reference to catch it -- see the same doc section.
                    glam::Quat::from_rotation_z(placement.orientation),
                    placement.position,
                ));
        }

        let mut built = Vec::new();
        let mut undrawable = 0;
        let mut wanted_bones: HashSet<(u32, bool)> = HashSet::new();
        for ((display_id, moving, look_key), transforms) in grouped {
            let look = looks.get(&look_key).cloned().flatten();
            let Some(model) = self.entity_model(gpu, meshes, chain, display_id, look_key, look.as_deref())
            else {
                undrawable += transforms.len();
                continue;
            };
            meshes.prepare(gpu, model.draws.iter().map(|d| d.state));
            let raw: Vec<Instance> = transforms
                .iter()
                .map(|t| Instance::from_cols_array_2d(t.to_cols_array_2d()))
                .collect();

            // Walking while moving, standing while not -- not every model has
            // both, or either. Only the buffer is created here; `update_animations`
            // writes the pose every frame rather than only on this rebuild, so
            // neither cycle visibly stutters at the rebuild rate.
            let sequence_id = if moving { WALK_ANIMATION_ID } else { STAND_ANIMATION_ID };
            let has_sequence = model.sequences.iter().any(|s| s.id == sequence_id);
            let animation = has_sequence.then(|| {
                self.entity_bones
                    .entry((display_id, moving))
                    .or_insert_with(|| meshes.create_bones(gpu, model.bones.len().max(1)));
                (display_id, moving)
            });
            if let Some(key) = animation {
                wanted_bones.insert(key);
            }

            built.push(Group {
                model,
                instances: InstanceBuffer::upload(gpu, &raw),
                count: raw.len() as u32,
                animation,
            });
        }
        // Drop bone buffers for creatures that changed bucket or left view,
        // rather than growing this cache for the life of the session.
        self.entity_bones.retain(|key, _| wanted_bones.contains(key));

        self.entities = built;
        self.refresh_stats();
        undrawable
    }

    /// Re-evaluates every currently-animated group's pose against the clock.
    ///
    /// Deliberately not folded into `set_entities` and not gated by the same
    /// rebuild that repositions entities: rewriting a bone buffer's contents
    /// is one `write_buffer` per animated model, cheap enough to do every
    /// frame. Tying animation to the coarser rebuild made a walk cycle
    /// visibly stutter -- noticed by actually watching it live, not
    /// predicted -- since it only ever advanced a few times a second instead
    /// of once per frame.
    pub fn update_animations(&self, gpu: &Gpu, meshes: &MeshRenderer) {
        for group in &self.entities {
            let Some((display_id, moving)) = group.animation else {
                continue;
            };
            let sequence_id = if moving { WALK_ANIMATION_ID } else { STAND_ANIMATION_ID };
            let (Some(sequence), Some(bones)) = (
                group.model.sequences.iter().position(|s| s.id == sequence_id),
                self.entity_bones.get(&(display_id, moving)),
            ) else {
                continue;
            };
            let duration = group.model.sequences[sequence].duration_ms.max(1);
            let time_ms = (self.started.elapsed().as_millis() as u32) % duration;
            let pose: Vec<[[f32; 4]; 4]> = m2::Model::pose_bones(&group.model.bones, sequence, time_ms)
                .iter()
                .map(|m| m.to_cols_array_2d())
                .collect();
            meshes.update_bones(gpu, bones, &pose);
        }
    }

    /// The animated bone buffer for a `Group::animation` key, if `set_entities`
    /// gave it one this rebuild.
    pub fn entity_bone_buffer(&self, key: (u32, bool)) -> Option<&BoneBuffer> {
        self.entity_bones.get(&key)
    }

    /// Loads a creature model by display id, with the skins that id selects.
    fn entity_model(
        &mut self,
        gpu: &Gpu,
        meshes: &MeshRenderer,
        chain: &mut Chain,
        display_id: u32,
        look_key: u64,
        look: Option<&crate::character::Look>,
    ) -> Option<Rc<CachedModel>> {
        if let Some(cached) = self.entity_cache.get(&(display_id, look_key)) {
            return cached.clone();
        }

        let entry = crate::model::creature(chain, display_id)
            .and_then(|(path, variations)| {
                crate::model::load_dressed(gpu, chain, &path, &variations, 0, look)
            })
            .map(|loaded| {
                let binds = loaded
                    .textures
                    .iter()
                    .map(|t| meshes.material_bind_group(gpu, &t.view))
                    .collect();
                Rc::new(CachedModel {
                    mesh: loaded.mesh,
                    draws: loaded.draws,
                    binds,
                    bones: loaded.bones,
                    sequences: loaded.sequences,
                    // This is the cache click-to-target reads from, so this is
                    // the one that has to carry the model's extent.
                    bounds: Some((loaded.min, loaded.max)),
                    textures: loaded.textures,
                })
            })
            .map_err(|e| tracing::debug!("display id {display_id}: {e}"))
            .ok();

        self.entity_cache.insert((display_id, look_key), entry.clone());
        entry
    }
}

/// `AnimationData.dbc` rows for the walk and stand cycles -- public spec
/// (documented on wowdev.wiki as part of the client's own animation-id
/// table), not derived from any server implementation. Every 3.3.5a model's
/// sequences use the same ids for the same actions.
const WALK_ANIMATION_ID: u16 = 4;
const STAND_ANIMATION_ID: u16 = 0;

#[cfg(test)]
mod tests {
    use super::*;

    /// A position inside a tile must map back to it, on both axes and with the
    /// swap the grid uses.
    #[test]
    fn positions_map_back_to_their_tile() {
        for tile in [(32, 48), (0, 0), (63, 63), (24, 20)] {
            let centre = tile_centre(tile);
            assert_eq!(tile_at(centre), tile, "centre of {tile:?}");
        }
    }

    /// The known Northshire chunk origin belongs to tile 32,48.
    #[test]
    fn a_known_chunk_origin_lands_on_its_tile() {
        // Chunk 0 of Azeroth_32_48 begins at x = -8533.33, which is the tile's
        // *upper* bound: the tile covers everything below it. A point a hair
        // above that boundary belongs to the neighbour, so sample just inside.
        assert_eq!(tile_at(Vec3::new(-8534.0, -0.5, 236.0)), (32, 48));
        // The far corner of the same tile.
        assert_eq!(tile_at(Vec3::new(-9066.0, -533.0, 100.0)), (32, 48));
        // And immediately past the boundary is the neighbour.
        assert_eq!(tile_at(Vec3::new(-8533.0, -0.5, 236.0)), (32, 47));
    }

    /// Neighbouring tiles must be adjacent in the grid, not scattered.
    #[test]
    fn adjacent_positions_give_adjacent_tiles() {
        let base = tile_centre((32, 48));
        let east = tile_at(base + Vec3::new(0.0, -adt::TILE_SIZE, 0.0));
        let south = tile_at(base + Vec3::new(-adt::TILE_SIZE, 0.0, 0.0));
        assert_eq!(east, (33, 48));
        assert_eq!(south, (32, 49));
    }

    /// Height must not affect which tile a position is on.
    #[test]
    fn altitude_does_not_change_the_tile() {
        let centre = tile_centre((40, 30));
        for z in [-500.0, 0.0, 1000.0] {
            assert_eq!(tile_at(centre + Vec3::new(0.0, 0.0, z)), (40, 30));
        }
    }
}
