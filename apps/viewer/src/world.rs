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
use crate::scene::{object_rotation, placement_position, placement_rotation};
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
    /// Set only for a replicated-entity group: display id plus which cycle
    /// this bucket plays, used to look up its own animated bone buffer instead
    /// of drawing with the scene's shared bind pose. The bucket has to be part
    /// of the key, not just the display id -- a species with instances
    /// standing, walking and running at once needs three different poses live
    /// at once, and a display-id-only key would have the last of them
    /// overwrite the others' buffers every rebuild. `None` for every
    /// tile-owned group (doodads and buildings never animate) and when the
    /// model has no matching sequence to play.
    pub animation: Option<(u32, Motion)>,
}

pub struct Tile {
    pub terrain: LoadedTerrain,
    pub groups: Vec<Group>,
    /// Kept alongside the uploaded mesh so the ground can be *asked* about,
    /// not only drawn -- see [`World::height_at`]. The GPU copy cannot answer:
    /// reading a vertex buffer back per frame to find out how high the ground
    /// is under one character would be absurd.
    heights: TileHeights,
}

/// One tile's height field, in a form that answers "how high is the ground
/// here" cheaply.
///
/// Deliberately not the parsed [`adt::Chunk`]s it was built from. Those carry
/// their alpha maps -- 4KB per layer per chunk, megabytes per tile -- and
/// keeping them resident for every loaded tile to consult 145 floats each would
/// cost more memory than the entire rest of the streaming set.
struct TileHeights {
    /// The tile's origin corner. Both axes run *inwards* (negative) from it,
    /// the same convention placements and vertices use -- see
    /// `docs/RENDERING.md`.
    origin: (f32, f32),
    /// The 16x16 grid, indexed by how far in from `origin` a chunk sits rather
    /// than by its order in the file or by its stored `IndexX`/`IndexY`.
    ///
    /// Derived from each chunk's own recorded position on purpose. The file
    /// order and the stored indices are both *correlated* with the grid -- and
    /// the stored `IndexX` tracks the world *y* axis, which is exactly the kind
    /// of swap this project has paid for twice. A position is the thing being
    /// asked about, so a position is what places the chunk.
    chunks: Vec<Option<ChunkHeights>>,
}

/// The part of a chunk that answers for its own footprint.
struct ChunkHeights {
    position: [f32; 3],
    holes: u16,
    heights: Vec<f32>,
}

impl TileHeights {
    fn new(chunks: &[adt::Chunk]) -> Self {
        // Both axes run inwards, so the origin corner is the largest of each.
        let origin = chunks.iter().fold((f32::MIN, f32::MIN), |(x, y), chunk| {
            (x.max(chunk.position[0]), y.max(chunk.position[1]))
        });

        let mut grid = Vec::new();
        grid.resize_with(adt::CHUNK_COUNT, || None);
        for chunk in chunks {
            let (cx, cy) = (
                ((origin.0 - chunk.position[0]) / adt::CHUNK_SIZE).round() as i64,
                ((origin.1 - chunk.position[1]) / adt::CHUNK_SIZE).round() as i64,
            );
            let side = adt::CHUNKS_PER_TILE as i64;
            if !(0..side).contains(&cx) || !(0..side).contains(&cy) {
                // A tile whose chunks do not form the 16x16 grid is a parser
                // bug, not terrain: say so rather than silently dropping it.
                tracing::warn!(
                    "chunk at {:?} sits at {cx},{cy} of its tile, outside the grid",
                    chunk.position
                );
                continue;
            }
            grid[(cy * side + cx) as usize] = Some(ChunkHeights {
                position: chunk.position,
                holes: chunk.holes,
                heights: chunk.heights.clone(),
            });
        }

        Self {
            origin,
            chunks: grid,
        }
    }

    /// Ground height at a position inside this tile, or `None` where the tile
    /// has no terrain (a hole) or the position is not on it at all.
    fn height_at(&self, x: f32, y: f32) -> Option<f32> {
        let side = adt::CHUNKS_PER_TILE as i64;
        // Clamped rather than rejected: a position exactly on the tile's far
        // edge computes as chunk 16 through float rounding, and the chunk's own
        // bounds check refuses anything genuinely past the edge anyway, since
        // the offset is remeasured there from the chunk's own position.
        let cx = (((self.origin.0 - x) / adt::CHUNK_SIZE).floor() as i64).clamp(0, side - 1);
        let cy = (((self.origin.1 - y) / adt::CHUNK_SIZE).floor() as i64).clamp(0, side - 1);
        let chunk = self.chunks.get((cy * side + cx) as usize)?.as_ref()?;
        adt::height_in_chunk(chunk.position, &chunk.heights, chunk.holes, x, y)
    }
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
    /// How fast this specific instance is travelling, in world units per
    /// second; zero when it is standing. Chooses which cycle its bucket plays
    /// -- see [`Motion::from_speed`].
    ///
    /// A speed rather than the "is it moving" flag this used to be, because
    /// the two cycles a moving creature can play are told apart by nothing
    /// else: a wolf padding along its patrol and a wolf charging you are both
    /// simply *moving*, and drawing the walk cycle for both makes the charge
    /// look like the model is being dragged.
    pub speed: f32,
    /// Whether this unit has no health left, so it should be drawn down rather
    /// than standing. Outranks `speed`: a creature killed mid-charge still has
    /// the charge's speed attached to it.
    pub dead: bool,
    /// How long ago it was seen to *die*, when this client watched it happen.
    /// `None` for a corpse that was already lying there when it came into
    /// view -- which must start settled rather than topple over again.
    pub died_ms_ago: Option<u32>,
    /// How long ago it last swung at something.
    pub swung_ms_ago: Option<u32>,
    /// Whether it is in a melee, on either side. Holds the guard up between
    /// swings instead of dropping back to the town idle.
    pub fighting: bool,
    /// What kind of thing this is.
    ///
    /// Decides which table the display id means: a unit's is a
    /// `CreatureDisplayInfo` row and a game object's is a
    /// `GameObjectDisplayInfo` row, and 603 is a wolf in one and an inn bench
    /// in the other.
    pub kind: ::world::ObjectType,
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
    /// Tables for dressing humanoid NPCs, read on first use rather than at
    /// construction: a scene with no replicated entities in it never needs
    /// them, and they are several megabytes of DBC.
    npc_looks: Option<crate::character::NpcAppearances>,
    /// Whether loading them has been attempted. Without this a failed load
    /// retries on every creature, which is the expensive failure repeated
    /// rather than reported.
    npc_looks_tried: bool,
    /// Models for doors, chests and mailboxes. Read on first use for the same
    /// reason the NPC tables are.
    game_objects: Option<dbc::schema::GameObjectDisplayInfo>,
    game_objects_tried: bool,
    /// Objects the server placed, as opposed to the ones the map files did.
    /// Owned by the world rather than a tile: they move, and they do not belong
    /// to the tile they happen to be standing on.
    entities: Vec<Group>,
    /// Animated bone buffers for replicated-entity groups, keyed by
    /// `Group::animation` and reused across rebuilds rather than reallocated:
    /// `update_bones` rewrites a buffer's contents in place, so the GPU
    /// buffer and its bind group only need to be created once per
    /// (display id, moving) pair, not once per rebuild tick.
    entity_bones: HashMap<(u32, Motion), BoneBuffer>,
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
            npc_looks: None,
            npc_looks_tried: false,
            game_objects: None,
            game_objects_tried: false,
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
                    // A WMO, not an M2: a different quarter of a turn.
                    object_rotation(placement.rotation),
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
            heights: TileHeights::new(&parsed.chunks),
        })
    }

    /// Ground height at a world position, or `None` if this client cannot
    /// answer for it.
    ///
    /// Three separate reasons for `None`, and the caller wants the same thing
    /// for all of them -- leave the altitude alone: the tile is not resident
    /// (streaming has not reached it, or the position is off the map), or the
    /// position falls on a hole, which is a doorway or a cave mouth whose floor
    /// the ADT does not describe. Guessing a height in any of those cases would
    /// put a character through the ground rather than on it.
    ///
    /// Only terrain. Standing on a bridge, a building's upper floor or any
    /// other WMO surface is a separate question this cannot answer -- see
    /// `docs/ROADMAP.md`.
    pub fn height_at(&self, x: f32, y: f32) -> Option<f32> {
        let tile = tile_at(Vec3::new(x, y, 0.0));
        self.tiles.get(&tile)?.heights.height_at(x, y)
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
        let mut kinds: HashMap<u64, ::world::ObjectType> = HashMap::new();
        let mut grouped: HashMap<(u32, Motion, u64), Vec<Mat4>> = HashMap::new();
        // One clock reading for the whole rebuild, so two units that died in
        // the same frame land in the same bucket and share a pose rather than
        // missing each other by a millisecond.
        let now_ms = self.started.elapsed().as_millis() as u32;
        for placement in placements {
            looks.insert(placement.look_key, placement.look.clone());
            kinds.insert(
                placement.look_key ^ game_object_key(placement.kind),
                placement.kind,
            );
            grouped
                .entry((
                    placement.display_id,
                    Motion::resolve(
                        placement.speed,
                        placement.dead,
                        placement.died_ms_ago,
                        placement.swung_ms_ago,
                        placement.fighting,
                        now_ms,
                    ),
                    // A game object shares the display-id space with creatures
                    // and means something else by it, so it cannot share a
                    // group with one. Folded into the look key rather than
                    // widening every tuple: the key already exists to keep
                    // things that look different apart.
                    placement.look_key ^ game_object_key(placement.kind),
                ))
                .or_default()
                .push(Mat4::from_scale_rotation_translation(
                    Vec3::splat(placement.scale),
                    // No offset. An M2's forward is +X and the network
                    // heading is already measured the same way, so the raw
                    // angle points an entity where it is going.
                    //
                    // A half turn was added here once, on the strength of a
                    // static render at a server-confirmed heading that
                    // appeared to show the model's back where its face
                    // belonged. It did -- but only because M2 geometry was
                    // being culled inside-out at the time (see
                    // `model::load_dressed`), and an inside-out model shows
                    // you the interior of its far surface, which reads
                    // exactly like a model facing away. Two bugs, one
                    // symptom, and the rotation was the innocent one.
                    //
                    // Settled by the person at the window: with the winding
                    // fixed and a live toggle to compare, every player,
                    // creature and NPC walks forwards at zero offset and
                    // backwards at half a turn.
                    glam::Quat::from_rotation_z(placement.orientation),
                    placement.position,
                ));
        }

        let mut built = Vec::new();
        let mut undrawable = 0;
        let mut wanted_bones: HashSet<(u32, Motion)> = HashSet::new();
        for ((display_id, motion, look_key), transforms) in grouped {
            let look = looks.get(&look_key).cloned().flatten();
            let kind = kinds.get(&look_key).copied().unwrap_or(::world::ObjectType::Unit);
            let Some(model) =
                self.entity_model(gpu, meshes, chain, display_id, look_key, look.as_deref(), kind)
            else {
                undrawable += transforms.len();
                continue;
            };
            meshes.prepare(gpu, model.draws.iter().map(|d| d.state));
            let raw: Vec<Instance> = transforms
                .iter()
                .map(|t| Instance::from_cols_array_2d(t.to_cols_array_2d()))
                .collect();

            // Running, walking or standing -- not every model has the cycle its
            // speed calls for, or any of them. Only the buffer is created here;
            // `update_animations` writes the pose every frame rather than only
            // on this rebuild, so no cycle visibly stutters at the rebuild rate.
            let resolved = sequence_for(&model, motion);
            // The one-shot cycles are rare, and each one is a claim that
            // something visible just happened -- so each is worth a line.
            //
            // Reporting whether a sequence was actually *found* is the point.
            // A model with no death cycle silently falls back to no animation
            // at all, which draws the bind pose: standing upright, exactly the
            // symptom this feature exists to remove. "Playing the death
            // animation" and "having a death animation to play" are different
            // claims, and only the second is checkable from here.
            if motion.is_notable() {
                match resolved {
                    Some(index) => tracing::debug!(
                        "display {display_id}: {motion:?} -> sequence {index} \
                         (animation id {}), {} instance(s)",
                        model.sequences[index].id,
                        transforms.len()
                    ),
                    None => tracing::debug!(
                        "display {display_id}: {motion:?} but the model has no \
                         matching cycle; it will draw its bind pose"
                    ),
                }
            }
            let animation = resolved.map(|_| {
                self.entity_bones
                    .entry((display_id, motion))
                    .or_insert_with(|| meshes.create_bones(gpu, model.bones.len().max(1)));
                (display_id, motion)
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
            let Some((display_id, motion)) = group.animation else {
                continue;
            };
            let (Some(sequence), Some(bones)) = (
                sequence_for(&group.model, motion),
                self.entity_bones.get(&(display_id, motion)),
            ) else {
                continue;
            };
            let duration = group.model.sequences[sequence].duration_ms.max(1);
            let now_ms = self.started.elapsed().as_millis() as u32;
            // A loop wraps; a cycle that plays once *holds its last frame*.
            //
            // Holding is what makes a corpse stay down. Wrapping a death cycle
            // would stand the creature back up and drop it again, forever,
            // which is worse than not animating at all -- and it is the
            // failure this branch exists to prevent, since every other cycle
            // in this client is a loop and the modulo was the obvious thing to
            // write.
            //
            // **The decision is made on the animation that resolved, not on
            // the motion that asked for it**, and the difference is not
            // academic. `Motion::Attacking` falls back to plain standing on a
            // model with no attack cycle, and a Stand frozen at its last frame
            // is a statue. Meanwhile `Motion::Dead` carries no start time at
            // all and yet must hold, because on most models it resolves to the
            // *fall* -- looping that is a creature dying over and over.
            let played = group.model.sequences[sequence].id;
            let time_ms = if played == DEATH_ANIMATION_ID && motion == Motion::Dead {
                // Settled onto the last frame of the fall. No start time is
                // needed or wanted: this pose is the same however long ago the
                // unit died, which is why every settled corpse of a display
                // can share one bucket.
                duration - 1
            } else if plays_once(played) {
                match motion.started_at() {
                    Some(started) => now_ms.saturating_sub(started).min(duration - 1),
                    // A play-once cycle with nothing to time it from: hold the
                    // end rather than loop, which is the safer of the two
                    // wrong answers.
                    None => duration - 1,
                }
            } else {
                now_ms % duration
            };
            let pose: Vec<[[f32; 4]; 4]> = m2::Model::pose_bones(&group.model.bones, sequence, time_ms)
                .iter()
                .map(|m| m.to_cols_array_2d())
                .collect();
            meshes.update_bones(gpu, bones, &pose);
        }
    }

    /// The animated bone buffer for a `Group::animation` key, if `set_entities`
    /// gave it one this rebuild.
    pub fn entity_bone_buffer(&self, key: (u32, Motion)) -> Option<&BoneBuffer> {
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
        kind: ::world::ObjectType,
    ) -> Option<Rc<CachedModel>> {
        if let Some(cached) = self.entity_cache.get(&(display_id, look_key)) {
            return cached.clone();
        }

        // A game object resolves to a *path*, which the tile loader's cache
        // already keys by -- and that cache understands both `.mdx` and `.wmo`,
        // which matters because a mailbox is a model and a ship is a building.
        // So game objects reuse it wholesale rather than growing a second
        // model cache that would load the abbey's benches once per bench.
        if kind == ::world::ObjectType::GameObject {
            let path = self.game_object_path(chain, display_id)?;
            return self.model(gpu, meshes, chain, &path);
        }

        // A humanoid NPC's body texture lives in `CreatureDisplayInfoExtra` and
        // nowhere else -- see `character::NpcAppearances::look`. Only consulted
        // when the caller supplied no look of its own, which is every case but
        // the player's own body: a player's appearance comes off the character
        // list, and a display id cannot answer for it.
        //
        // Not part of the cache key, and it must not be: this look is a pure
        // function of the display id, so `(display_id, 0)` already
        // distinguishes it from every other. A key that included it would
        // reload one model per creature *instance*.
        let npc_look = look.is_none().then(|| self.npc_look(chain, display_id)).flatten();
        let look = look.or(npc_look.as_ref());

        let entry = crate::model::creature(chain, display_id)
            .and_then(|(path, variations)| {
                crate::model::load_dressed(gpu, chain, &path, &variations, 0, look)
            })
            .map(|loaded| {
                // A texture that failed to load is a *white* creature, not a
                // missing one, and white is the one failure that looks
                // deliberate. `load_dressed` has always collected these and
                // every caller has always dropped them, so the whole
                // white-humanoid problem was invisible in the logs -- the same
                // shape as the packet body this project once refused and threw
                // away. Named, at warning level, with the display id that
                // produced it: that is enough to reproduce it offline with
                // `wow-viewer --creature <id> --screenshot`.
                if !loaded.missing_textures.is_empty() {
                    tracing::warn!(
                        "display {display_id} drew with {} placeholder texture(s): {}",
                        loaded.missing_textures.len(),
                        loaded.missing_textures.join(", ")
                    );
                }
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

    /// Which model a game object wears, loading the table on first use.
    fn game_object_path(&mut self, chain: &mut Chain, display_id: u32) -> Option<String> {
        use dbc::schema::GameObjectDisplayInfo;

        if !self.game_objects_tried {
            self.game_objects_tried = true;
            self.game_objects = chain
                .read(GameObjectDisplayInfo::PATH)
                .ok()
                .and_then(|bytes| GameObjectDisplayInfo::parse(&bytes).ok());
        }
        let path = self
            .game_objects
            .as_ref()?
            .iter()
            .find(|row| row.id() == display_id)
            .map(|row| row.model().to_string())?;
        if path.is_empty() {
            tracing::debug!("game object display {display_id} names no model");
            return None;
        }
        tracing::debug!("game object display {display_id} -> {path}");
        // The table names `.mdx` even where the archive ships `.m2`; the model
        // loader already rewrites that, and a `.wmo` passes through untouched.
        Some(path)
    }

    /// How a humanoid NPC is dressed, loading the tables on first use.
    fn npc_look(
        &mut self,
        chain: &mut Chain,
        display_id: u32,
    ) -> Option<crate::character::Look> {
        if !self.npc_looks_tried {
            self.npc_looks_tried = true;
            let started = Instant::now();
            self.npc_looks = crate::character::NpcAppearances::load(chain);
            // Timed because this is the only thing here that reads several
            // megabytes on a render frame, and because a cost nobody measured
            // is how a thirty-seven-second login went undiagnosed.
            tracing::info!(
                "npc appearance tables loaded in {:?} ({})",
                started.elapsed(),
                if self.npc_looks.is_some() { "ok" } else { "unavailable" }
            );
        }
        self.npc_looks.as_ref()?.look(display_id)
    }
}

/// Keeps game objects out of creatures' half of the display-id space.
///
/// An arbitrary constant xored into the cache key: display 603 is a wolf to a
/// unit and an inn bench to a game object, and without this the first of the
/// two loaded would answer for both.
fn game_object_key(kind: ::world::ObjectType) -> u64 {
    match kind {
        ::world::ObjectType::GameObject => 0x9111_0b1e_0000_0000,
        _ => 0,
    }
}

/// `AnimationData.dbc` rows for the cycles this client plays -- public spec
/// (documented on wowdev.wiki as part of the client's own animation-id
/// table), not derived from any server implementation. Every 3.3.5a model's
/// sequences use the same ids for the same actions.
const STAND_ANIMATION_ID: u16 = 0;
const WALK_ANIMATION_ID: u16 = 4;
const RUN_ANIMATION_ID: u16 = 5;
/// Falling over, and lying still afterwards. Two rows, not one, and the table
/// says so itself: `Dead` (6) lists `Death` (1) as its *fallback*, which is
/// exactly the relationship between them -- a model with no settled-corpse
/// cycle holds the last frame of the toppling one instead.
const DEATH_ANIMATION_ID: u16 = 1;
const DEAD_ANIMATION_ID: u16 = 6;
/// Swinging a weapon. `Attack1H` (17) falls back to `AttackUnarmed` (16) in
/// `AnimationData.dbc`'s own fallback column, which is the order tried here --
/// a wolf has no one-handed swing and a warrior has both.
const ATTACK_1H_ANIMATION_ID: u16 = 17;
const ATTACK_UNARMED_ANIMATION_ID: u16 = 16;
/// Standing *in* a fight, between swings: weapon up, guard raised. `Ready1H`
/// (26) falls back to `ReadyUnarmed` (25), again per the table's own column.
///
/// Worth having as a state of its own rather than letting combat look like
/// idling. A swing is an instant and a fight is a minute, so without this a
/// fighting character spends nearly all of it in the same relaxed stand it
/// uses in town -- which is what "the player stands still while the fight
/// happens" actually describes.
const READY_1H_ANIMATION_ID: u16 = 26;
const READY_UNARMED_ANIMATION_ID: u16 = 25;

/// How long any one-shot cycle is allowed to run before the unit is treated as
/// settled.
///
/// A ceiling rather than the real duration, because the thing that *chooses* a
/// motion (a placement being built from replicated state) has no model loaded
/// and cannot ask how long its death animation is. `update_animations`, which
/// does have the model, clamps to the real duration -- so this only has to be
/// long enough not to cut anything short. No 3.3.5a death or attack cycle is
/// close to three seconds.
const ONE_SHOT_CEILING_MS: u32 = 3_000;

/// A swing is done with sooner than a death, and the difference is visible.
///
/// A one-shot holds its last frame until the state lapses, so a ceiling far
/// longer than the animation leaves the unit **frozen on its follow-through**
/// until the next swing snaps it back to the start. At the ceiling above, with
/// swings roughly two seconds apart, a fighter would spend more of the fight
/// frozen mid-swing than moving. Long enough for the longest attack cycle in
/// the models this client draws (`Creature\Wolf\Wolf.m2`'s `AttackUnarmed` is
/// 1500ms, `HumanMale`'s 1000ms), and no longer.
const ATTACK_CEILING_MS: u32 = 1_500;

/// How coarsely a one-shot's start time is bucketed.
///
/// **This is not an optimisation, it is the difference between the animation
/// working and not.** The stamp is part of the bone-buffer's cache key, and it
/// is computed by subtracting one clock reading from another taken a moment
/// earlier in the same frame -- so left raw it lands on a slightly different
/// value *every frame*. Every frame would then be a fresh cache key, a fresh
/// bone buffer, and a model drawn in its bind pose because nothing has posed
/// the new buffer yet. The symptom is not a subtle inefficiency: it is a
/// character that flickers instead of swinging.
///
/// A tenth of a second is coarse enough to be stable across the drift between
/// two clock reads and fine enough that two units swinging a tenth of a second
/// apart still animate separately.
const ONE_SHOT_BUCKET_MS: u32 = 100;

/// Rounds a one-shot's start time to its bucket. See [`ONE_SHOT_BUCKET_MS`].
fn bucket(at_ms: u32) -> u32 {
    at_ms / ONE_SHOT_BUCKET_MS * ONE_SHOT_BUCKET_MS
}

/// Which cycle a replicated entity should be playing.
///
/// The looping states carry nothing; the one-shot states carry **when they
/// started**, as a world-clock millisecond stamp.
///
/// That stamp is in the key rather than looked up per entity because bone
/// poses are shared: every instance in a bucket is drawn from one buffer, so
/// two creatures can only share a pose if they are at the same point of the
/// same cycle. For a loop that is free -- everything standing is in step
/// anyway. For a one-shot it is the whole difficulty, and putting the start
/// time in the key solves it exactly: units that died together share a bucket
/// and units that died a second apart do not.
///
/// The stamp is *absolute*, not an age. An age changes every frame, which
/// would rebuild the bone buffer every frame and defeat the cache; a start
/// time is fixed for the life of the animation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Motion {
    Stand,
    Walk,
    Run,
    /// Toppling over, from the world-clock millisecond it began.
    Dying(u32),
    /// Settled: lying still, and no longer tied to when death happened.
    Dead,
    /// Mid-swing, from the world-clock millisecond the blow landed.
    Attacking(u32),
    /// In a fight but between swings: guard up rather than at ease.
    Ready,
}

/// Where a walk becomes a run, in world units per second.
///
/// 3.3.5a's default ground speeds are 2.5 walking and 7.0 running, so the
/// midpoint separates the two with the widest margin either way -- and a
/// creature whose speed has been scaled a little in either direction still
/// lands on the right side. It also sits above the 4.5 backing-up speed, so a
/// unit reversing reads as a walk rather than as a run played at a jog.
const RUN_SPEED: f32 = 4.75;

impl Motion {
    /// The cycle a ground speed calls for.
    ///
    /// Speed rather than a moving/not-moving flag because that flag cannot tell
    /// the two moving cycles apart, and picking one for everything is wrong
    /// both ways round: a patrolling creature drawn running skates ahead of its
    /// own legs, and a charging one drawn walking is dragged along by them.
    pub fn from_speed(speed: f32) -> Self {
        if speed <= 0.0 {
            Motion::Stand
        } else if speed < RUN_SPEED {
            Motion::Walk
        } else {
            Motion::Run
        }
    }

    /// What a unit should be playing, given everything known about it.
    ///
    /// The order of precedence is the whole of the rule and each step earns
    /// its place:
    ///
    /// - **Dead outranks everything.** A corpse does not walk, however fast
    ///   the last movement packet said it was going -- and a creature killed
    ///   mid-charge still has a stale speed attached to it.
    /// - **A death we watched plays the fall; one we did not starts settled.**
    ///   `died_ms_ago` is `None` for a corpse that was already lying there
    ///   when it came into view, which is most of them in a graveyard.
    /// - **A swing only interrupts standing.** A creature chasing you swings
    ///   as it runs, and a swing animation played over a run reads as a
    ///   stumble; the run is the more informative of the two, so it wins.
    ///
    /// `now_ms` is the caller's world clock, and the one-shot stamps are
    /// derived from it by subtraction so they land on the same timeline
    /// `update_animations` reads.
    pub fn resolve(
        speed: f32,
        dead: bool,
        died_ms_ago: Option<u32>,
        swung_ms_ago: Option<u32>,
        fighting: bool,
        now_ms: u32,
    ) -> Self {
        if dead {
            return match died_ms_ago {
                Some(age) if age < ONE_SHOT_CEILING_MS => {
                    Motion::Dying(bucket(now_ms.saturating_sub(age)))
                }
                _ => Motion::Dead,
            };
        }
        let moving = Motion::from_speed(speed);
        if moving == Motion::Stand {
            if let Some(age) = swung_ms_ago {
                if age < ATTACK_CEILING_MS {
                    return Motion::Attacking(bucket(now_ms.saturating_sub(age)));
                }
            }
            if fighting {
                return Motion::Ready;
            }
        }
        moving
    }

    /// Whether this cycle is about a fight or a death, and so worth a log line.
    fn is_notable(self) -> bool {
        !matches!(self, Motion::Stand | Motion::Walk | Motion::Run)
    }

    /// When a one-shot cycle began, on the caller's world clock.
    fn started_at(self) -> Option<u32> {
        match self {
            Motion::Dying(at) | Motion::Attacking(at) => Some(at),
            _ => None,
        }
    }

    /// The animation ids to try, in order.
    ///
    /// A model with no run cycle should still walk rather than freeze in its
    /// bind pose, which is what an unmatched id gets. Nothing falls back as far
    /// as standing: a model with neither travelling cycle has nothing sensible
    /// to play while it moves, and drawing it standing still as it slides along
    /// is the "every creature walks on the spot" bug from the other direction.
    fn animation_ids(self) -> &'static [u16] {
        match self {
            Motion::Stand => &[STAND_ANIMATION_ID],
            Motion::Walk => &[WALK_ANIMATION_ID],
            Motion::Run => &[RUN_ANIMATION_ID, WALK_ANIMATION_ID],
            Motion::Dying(_) => &[DEATH_ANIMATION_ID],
            // Settled last: a model with no lying-still cycle holds the final
            // frame of the toppling one, which `update_animations` produces by
            // clamping rather than looping. `AnimationData.dbc` lists exactly
            // this fallback against row 6.
            Motion::Dead => &[DEAD_ANIMATION_ID, DEATH_ANIMATION_ID],
            // Both combat stances end at plain standing, and that last entry
            // is not decoration. `Creature\Wolf\Wolf.m2` has `AttackUnarmed`
            // and `Death` and **no ready stance at all** -- so without a
            // fallback a wolf that entered combat would resolve to no sequence
            // and be drawn in its bind pose, stiff and T-posed, which is worse
            // than the idle it replaced. Checked against the real models
            // rather than assumed: `wow-cli m2 anims` lists what each one
            // actually carries.
            Motion::Attacking(_) => &[
                ATTACK_1H_ANIMATION_ID,
                ATTACK_UNARMED_ANIMATION_ID,
                STAND_ANIMATION_ID,
            ],
            Motion::Ready => &[
                READY_1H_ANIMATION_ID,
                READY_UNARMED_ANIMATION_ID,
                STAND_ANIMATION_ID,
            ],
        }
    }
}

/// Whether an animation id names a cycle that runs once and stops, rather than
/// one that repeats.
///
/// A property of the *animation*, not of the state that asked for it. A death
/// or a swing happens once; standing, walking and holding a guard go on until
/// something else happens. Getting this from the resolved id rather than from
/// the requesting motion is what lets a fallback change the answer -- see
/// [`World::update_animations`].
fn plays_once(animation_id: u16) -> bool {
    matches!(
        animation_id,
        DEATH_ANIMATION_ID | ATTACK_1H_ANIMATION_ID | ATTACK_UNARMED_ANIMATION_ID
    )
}

/// Which of a model's sequences a motion plays, if it has one.
///
/// Resolved here and consulted by both `set_entities` (which creates the bone
/// buffer) and `update_animations` (which writes the pose into it), so the two
/// cannot disagree about which sequence a bucket is playing -- a disagreement
/// that would not error anywhere, just pose one cycle into a buffer drawn as
/// another.
fn sequence_for(model: &CachedModel, motion: Motion) -> Option<usize> {
    motion
        .animation_ids()
        .iter()
        .find_map(|id| model.sequences.iter().position(|s| s.id == *id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A corpse does not walk, however fast it was going a moment ago.
    ///
    /// The speed attached to a creature killed mid-charge is stale but not
    /// zero -- nothing zeroes it, because dying is not a movement packet. So
    /// death has to outrank it explicitly, and this is the case that made the
    /// original `from_speed` insufficient rather than merely incomplete.
    #[test]
    fn death_outranks_a_stale_speed() {
        assert_eq!(
            Motion::resolve(7.0, true, None, None, false, 10_000),
            Motion::Dead,
            "a corpse was drawn running"
        );
    }

    /// A death this client watched plays the fall; one it did not starts
    /// settled. Otherwise every corpse in a graveyard topples over again each
    /// time the player walks into view of it.
    #[test]
    fn only_a_death_we_watched_plays_the_fall() {
        assert_eq!(
            Motion::resolve(0.0, true, Some(200), None, false, 10_000),
            Motion::Dying(9_800)
        );
        assert_eq!(Motion::resolve(0.0, true, None, None, false, 10_000), Motion::Dead);
    }

    /// And it stops falling eventually, rather than holding a one-shot bucket
    /// for the life of the corpse.
    #[test]
    fn a_fall_settles_once_it_has_had_time_to_finish() {
        assert_eq!(
            Motion::resolve(0.0, true, Some(ONE_SHOT_CEILING_MS + 1), None, false, 10_000),
            Motion::Dead
        );
    }

    /// **The stamp must not move as time passes.**
    ///
    /// This is the invariant the whole design rests on. The bone pose for a
    /// bucket is cached under the motion, so if the key changed every frame
    /// the buffer would be rebuilt every frame and the cache would be worse
    /// than useless. Writing the age into the key instead of the start time is
    /// the obvious mistake, and it would look *fine* -- the animation would
    /// play correctly and the cost would be invisible.
    #[test]
    fn a_one_shot_keeps_its_bucket_as_the_clock_advances() {
        let first = Motion::resolve(0.0, true, Some(100), None, false, 5_000);
        // A second later: the death is a second older and the clock a second
        // further on, which is the same death.
        let later = Motion::resolve(0.0, true, Some(1_100), None, false, 6_000);
        assert_eq!(first, later, "the bucket moved with the clock");

        // Two deaths a second apart must *not* share, or one pops to the
        // other's frame.
        let other = Motion::resolve(0.0, true, Some(100), None, false, 6_000);
        assert_ne!(first, other);
    }

    /// **The bucket has to survive the clocks disagreeing slightly**, which in
    /// the real thing they always do.
    ///
    /// The age and the current time are read at different moments of the same
    /// frame, so their difference lands a few milliseconds apart each time.
    /// The version of this test above uses arithmetic that happens to cancel
    /// exactly, so it passed while the running client flickered: every frame
    /// produced a new key, a new bone buffer, and a model drawn in its bind
    /// pose because nothing had posed the new buffer yet. Reported as "the
    /// attack animation is very jittery", and no test then in the suite could
    /// have found it.
    #[test]
    fn a_one_shot_bucket_survives_drift_between_two_clock_reads() {
        // The same swing, seen over eight frames, with the two clock readings
        // drifting a few milliseconds apart each time as they really do.
        let reference = Motion::resolve(0.0, false, None, Some(40), true, 8_000);
        for frame in 0..8u32 {
            let drift = frame * 3;
            let seen = Motion::resolve(
                0.0,
                false,
                None,
                Some(40 + frame * 16),
                true,
                8_000 + frame * 16 + drift,
            );
            assert_eq!(
                seen, reference,
                "frame {frame}: the bucket moved, so the bone buffer is rebuilt \
                 and the model draws its bind pose"
            );
        }
    }

    /// A swing lapses back to the guard once the animation is over, rather
    /// than freezing on its follow-through until the next one.
    ///
    /// The ceiling has to be close to the longest attack cycle in the models
    /// actually drawn -- a one-shot holds its last frame until the state
    /// lapses, so a generous ceiling is a character frozen mid-swing for most
    /// of the fight.
    #[test]
    fn a_swing_lapses_before_the_next_one_lands() {
        // Wolf `AttackUnarmed` is 1500ms, `HumanMale`'s 1000ms; swings are
        // roughly two seconds apart.
        assert!(
            ATTACK_CEILING_MS >= 1_500,
            "the longest attack cycle would be cut off"
        );
        assert!(
            ATTACK_CEILING_MS < 2_000,
            "a fighter would be frozen on its follow-through between swings"
        );
        assert_eq!(
            Motion::resolve(0.0, false, None, Some(ATTACK_CEILING_MS + 1), true, 9_000),
            Motion::Ready
        );
    }

    /// A swing interrupts standing and not running: a creature chasing you
    /// swings as it runs, and the swing played over the run reads as a
    /// stumble.
    #[test]
    fn a_swing_interrupts_standing_but_not_running() {
        assert_eq!(
            Motion::resolve(0.0, false, None, Some(50), true, 4_000),
            Motion::Attacking(3_900)
        );
        assert_eq!(Motion::resolve(7.0, false, None, Some(50), true, 4_000), Motion::Run);
        // And an old swing has stopped mattering.
        assert_eq!(
            Motion::resolve(0.0, false, None, Some(ONE_SHOT_CEILING_MS + 1), false, 4_000),
            Motion::Stand
        );
    }

    /// Between swings, a unit still in a fight keeps its guard up rather than
    /// dropping to the town idle.
    ///
    /// This is the state a fight is mostly *made of*: a swing is an instant
    /// and a fight is a minute, so without it a fighting character spends
    /// nearly all of the fight standing at ease -- which is what "the player
    /// stands still while the fight happens" actually describes, and it is not
    /// fixed by the swing animation alone.
    #[test]
    fn a_fighter_between_swings_keeps_its_guard_up() {
        assert_eq!(
            Motion::resolve(0.0, false, None, None, true, 4_000),
            Motion::Ready
        );
        // Out of the fight, it relaxes.
        assert_eq!(
            Motion::resolve(0.0, false, None, None, false, 4_000),
            Motion::Stand
        );
        // A swing still beats the guard, and running still beats both.
        assert_eq!(
            Motion::resolve(0.0, false, None, Some(50), true, 4_000),
            Motion::Attacking(3_900)
        );
        assert_eq!(Motion::resolve(7.0, false, None, None, true, 4_000), Motion::Run);
        // And a corpse is not "in a fight" whatever the map still says.
        assert_eq!(Motion::resolve(0.0, true, None, None, true, 4_000), Motion::Dead);
    }

    /// Which cycles hold their last frame is a property of the *animation*,
    /// not of the state that asked for it.
    ///
    /// That distinction is load-bearing in both directions. A wolf has no
    /// ready stance, so `Motion::Attacking` on one falls all the way back to
    /// Stand -- and a Stand frozen at its final frame is a statue. Meanwhile a
    /// wolf has no settled-corpse cycle either, so `Motion::Dead` resolves to
    /// the *fall*, which must hold rather than loop or the creature dies over
    /// and over. Keying off the requesting motion gets exactly one of those
    /// two right, whichever way round it is written.
    #[test]
    fn holding_the_last_frame_follows_the_animation_not_the_request() {
        assert!(plays_once(DEATH_ANIMATION_ID));
        assert!(plays_once(ATTACK_1H_ANIMATION_ID));
        assert!(plays_once(ATTACK_UNARMED_ANIMATION_ID));
        // The fallbacks a combat state can land on, which must keep looping.
        assert!(!plays_once(STAND_ANIMATION_ID), "a frozen stand is a statue");
        assert!(!plays_once(READY_1H_ANIMATION_ID));
        assert!(!plays_once(READY_UNARMED_ANIMATION_ID));
        assert!(!plays_once(WALK_ANIMATION_ID));
        assert!(!plays_once(RUN_ANIMATION_ID));
        // `Dead` (6) is a real lying-still loop where a model has one.
        assert!(!plays_once(DEAD_ANIMATION_ID));
    }

    /// Only the states that mark an instant carry one, and the looping states
    /// must not -- a walk with a start time would freeze mid-stride.
    #[test]
    fn only_the_states_that_mark_an_instant_carry_a_start_time() {
        assert_eq!(Motion::Dying(1234).started_at(), Some(1234));
        assert_eq!(Motion::Attacking(99).started_at(), Some(99));
        for looping in [Motion::Stand, Motion::Walk, Motion::Run, Motion::Ready, Motion::Dead] {
            assert_eq!(looping.started_at(), None, "{looping:?}");
        }
    }

    /// Every combat and death state has to end its fallback list somewhere a
    /// model is guaranteed to have, or it draws the bind pose.
    ///
    /// Found against real data rather than reasoned about: `Creature\Wolf\
    /// Wolf.m2` carries `AttackUnarmed` and `Death` and no ready stance at
    /// all, so a wolf entering combat resolved to nothing and would have been
    /// drawn T-posed.
    #[test]
    fn the_combat_stances_fall_back_to_standing() {
        for motion in [Motion::Attacking(0), Motion::Ready] {
            assert_eq!(
                motion.animation_ids().last(),
                Some(&STAND_ANIMATION_ID),
                "{motion:?} can resolve to nothing"
            );
        }
    }

    /// Every motion has to name at least one animation, or it silently draws
    /// the bind pose -- which this project has already paid for once.
    #[test]
    fn every_motion_names_an_animation() {
        for motion in [
            Motion::Stand,
            Motion::Walk,
            Motion::Run,
            Motion::Dying(0),
            Motion::Dead,
            Motion::Attacking(0),
            Motion::Ready,
        ] {
            assert!(!motion.animation_ids().is_empty(), "{motion:?}");
        }
    }

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

    /// A tile's worth of chunks, each flat at a height that says where in the
    /// grid it sits, handed over in *reverse* file order.
    ///
    /// The order is the point: `TileHeights` places a chunk by its recorded
    /// position, and a build that quietly relied on the order instead would
    /// pass every test that fed it chunks in order and put a character on the
    /// wrong hill in the world.
    fn heights_for_tile(tile: (i32, i32)) -> TileHeights {
        let origin = (
            (32.0 - tile.1 as f32) * adt::TILE_SIZE,
            (32.0 - tile.0 as f32) * adt::TILE_SIZE,
        );
        let mut chunks = Vec::new();
        for cy in 0..adt::CHUNKS_PER_TILE {
            for cx in 0..adt::CHUNKS_PER_TILE {
                chunks.push(adt::Chunk {
                    index: (cx as u32, cy as u32),
                    position: [
                        origin.0 - cx as f32 * adt::CHUNK_SIZE,
                        origin.1 - cy as f32 * adt::CHUNK_SIZE,
                        (cy * adt::CHUNKS_PER_TILE + cx) as f32,
                    ],
                    area_id: 0,
                    holes: 0,
                    heights: vec![0.0; adt::HEIGHTS_PER_CHUNK],
                    normals: vec![[0, 0, 127]; adt::HEIGHTS_PER_CHUNK],
                    layers: Vec::new(),
                    alpha_maps: Vec::new(),
                    doodad_refs: Vec::new(),
                    object_refs: Vec::new(),
                });
            }
        }
        chunks.reverse();
        TileHeights::new(&chunks)
    }

    /// A position resolves to the chunk it is actually standing on.
    #[test]
    fn a_position_finds_its_own_chunk() {
        let tile = (32, 48);
        let heights = heights_for_tile(tile);
        let origin = (
            (32.0 - tile.1 as f32) * adt::TILE_SIZE,
            (32.0 - tile.0 as f32) * adt::TILE_SIZE,
        );

        for cy in 0..adt::CHUNKS_PER_TILE {
            for cx in 0..adt::CHUNKS_PER_TILE {
                // The chunk's centre, which no rounding can push into a
                // neighbour.
                let x = origin.0 - (cx as f32 + 0.5) * adt::CHUNK_SIZE;
                let y = origin.1 - (cy as f32 + 0.5) * adt::CHUNK_SIZE;
                let expected = (cy * adt::CHUNKS_PER_TILE + cx) as f32;
                assert_eq!(
                    heights.height_at(x, y),
                    Some(expected),
                    "chunk {cx},{cy} at {x},{y}"
                );
            }
        }
    }

    /// The height field covers every position `tile_at` assigns to its tile --
    /// no gap in the middle of the map -- and nothing beyond that tile's own
    /// footprint.
    ///
    /// This is the join between two independently written pieces of the same
    /// convention: `tile_at` derives a tile from a position, and `TileHeights`
    /// derives an origin from the chunks it was given. Both are "inwards from
    /// the corner", and either could have been inverted on its own.
    ///
    /// The two are not quite complementary and must not be asserted to be. A
    /// tile's footprint here is closed at both ends, by the hair of tolerance
    /// `adt::height_in_chunk` allows so that a point exactly on a seam is not
    /// refused by float rounding; `tile_at`'s is half-open, as a grid's has to
    /// be. So both tiles either side of a seam answer for the seam itself,
    /// which costs nothing: `World::height_at` picks the tile first and only
    /// ever consults one of them.
    #[test]
    fn a_tiles_height_field_covers_exactly_its_tile() {
        let tile = (32, 48);
        let heights = heights_for_tile(tile);
        let centre = tile_centre(tile);
        let origin = (
            (32.0 - tile.1 as f32) * adt::TILE_SIZE,
            (32.0 - tile.0 as f32) * adt::TILE_SIZE,
        );
        // Rather wider than the tolerance being allowed for, and far narrower
        // than anything a wrong axis or a half-tile offset would produce.
        const SEAM: f32 = 0.1;

        // A grid across the tile and a little way past every edge of it.
        let steps = 40;
        for row in -4..=steps + 4 {
            for col in -4..=steps + 4 {
                let offset = |i: i32| (i as f32 / steps as f32 - 0.5) * adt::TILE_SIZE;
                let (x, y) = (centre.x + offset(row), centre.y + offset(col));
                let answered = heights.height_at(x, y).is_some();

                if tile_at(Vec3::new(x, y, 0.0)) == tile {
                    assert!(answered, "at {x},{y}, which is on tile {tile:?}");
                }
                if answered {
                    let (dx, dy) = (origin.0 - x, origin.1 - y);
                    let footprint = -SEAM..=adt::TILE_SIZE + SEAM;
                    assert!(
                        footprint.contains(&dx) && footprint.contains(&dy),
                        "answered at {x},{y}, which is {dx},{dy} in from the corner"
                    );
                }
            }
        }
    }

    /// The three cycles, and where the boundaries between them are.
    #[test]
    fn speed_chooses_the_cycle() {
        assert_eq!(Motion::from_speed(0.0), Motion::Stand);
        // 3.3.5a's own default ground speeds.
        assert_eq!(Motion::from_speed(2.5), Motion::Walk, "the walk speed");
        assert_eq!(Motion::from_speed(4.5), Motion::Walk, "backing up");
        assert_eq!(Motion::from_speed(7.0), Motion::Run, "the run speed");
        // A creature crawling is still walking, not standing: standing is
        // reserved for no move in flight at all, so a slow patrol animates.
        assert_eq!(Motion::from_speed(0.2), Motion::Walk);
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
