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

use glam::{Mat4, Vec3};
use mpq::Chain;
use render::mesh::{Instance, InstanceBuffer, MeshRenderer};
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
    /// Held because the bind groups reference their views.
    #[allow(dead_code)]
    textures: Vec<UploadedTexture>,
}

/// One model and the transforms it takes on a single tile.
pub struct Group {
    pub model: Rc<CachedModel>,
    pub instances: InstanceBuffer,
    pub count: u32,
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
    entity_cache: HashMap<u32, Option<Rc<CachedModel>>>,
    /// Objects the server placed, as opposed to the ones the map files did.
    /// Owned by the world rather than a tile: they move, and they do not belong
    /// to the tile they happen to be standing on.
    entities: Vec<Group>,
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
            crate::world_object::load(gpu, chain, path, None)
                .map(|w| (w.mesh, w.draws, w.textures))
                .ok()
        } else {
            crate::model::load(gpu, chain, path, &crate::model::Variations::default(), 0)
                .map(|m| (m.mesh, m.draws, m.textures))
                .ok()
        };

        let entry = built.map(|(mesh, draws, textures)| {
            let binds = textures
                .iter()
                .map(|t| meshes.material_bind_group(gpu, &t.view))
                .collect();
            Rc::new(CachedModel {
                mesh,
                draws,
                binds,
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

    /// Replaces the server-placed objects.
    ///
    /// Returns how many could not be drawn, which is worth surfacing: a world
    /// missing half its creatures because their models failed to load looks
    /// exactly like a world where the protocol never reported them.
    pub fn set_entities(
        &mut self,
        gpu: &Gpu,
        meshes: &mut MeshRenderer,
        chain: &mut Chain,
        placements: &[EntityPlacement],
    ) -> usize {
        use std::f32::consts::FRAC_PI_2;

        let mut grouped: HashMap<u32, Vec<Mat4>> = HashMap::new();
        for placement in placements {
            grouped
                .entry(placement.display_id)
                .or_default()
                .push(Mat4::from_scale_rotation_translation(
                    Vec3::splat(placement.scale),
                    // The quarter-turn matches the doodad path, which is known
                    // to orient these same models correctly: the offset is a
                    // property of where an M2's forward axis points, not of
                    // where the angle came from. Position is what this milestone
                    // is really asserting; facing is inferred from that
                    // consistency rather than checked against a reference.
                    glam::Quat::from_rotation_z(placement.orientation - FRAC_PI_2),
                    placement.position,
                ));
        }

        let mut built = Vec::new();
        let mut undrawable = 0;
        for (display_id, transforms) in grouped {
            let Some(model) = self.entity_model(gpu, meshes, chain, display_id) else {
                undrawable += transforms.len();
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
            });
        }

        self.entities = built;
        self.refresh_stats();
        undrawable
    }

    /// Loads a creature model by display id, with the skins that id selects.
    fn entity_model(
        &mut self,
        gpu: &Gpu,
        meshes: &MeshRenderer,
        chain: &mut Chain,
        display_id: u32,
    ) -> Option<Rc<CachedModel>> {
        if let Some(cached) = self.entity_cache.get(&display_id) {
            return cached.clone();
        }

        let entry = crate::model::creature(chain, display_id)
            .and_then(|(path, variations)| {
                crate::model::load(gpu, chain, &path, &variations, 0)
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
                    textures: loaded.textures,
                })
            })
            .map_err(|e| tracing::debug!("display id {display_id}: {e}"))
            .ok();

        self.entity_cache.insert(display_id, entry.clone());
        entry
    }
}

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
