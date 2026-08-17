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

use crate::character::Stance;
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
    /// Where things this model carries hang from. Empty for everything that
    /// carries nothing, which is nearly everything.
    pub attachments: Vec<m2::Attachment>,
    /// The model's own extent, in its local space. Carried so a replicated
    /// entity can be clicked on: a click needs a volume to test a ray against,
    /// and the model already knows how big it is. `None` for a WMO, which is
    /// never a click target.
    pub bounds: Option<(Vec3, Vec3)>,
    /// Held because the bind groups reference their views.
    #[allow(dead_code)]
    textures: Vec<UploadedTexture>,
    /// Everything solid about this model, in its own space, ready to be
    /// transformed by each placement -- see the `collision` crate. Empty for
    /// the many models that are scenery: a tuft of grass has no collision mesh
    /// and the original lets you walk through it too.
    pub collision: Vec<[[f32; 3]; 3]>,
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
    /// Set only when this group *is* an item held by another group, in which
    /// case its instance transforms are recomputed every frame from the
    /// wielder's pose. See [`Held`].
    pub held: Option<Held>,
}

/// A group that hangs off another group's skeleton.
///
/// Modelled as an ordinary [`Group`] rather than as something the wielder owns,
/// so the draw loop needs no knowledge of it at all: a sword is a mesh with
/// transforms like any other, and the only thing that makes it held is *where
/// those transforms come from*. Its own [`Group::animation`] stays `None` --
/// a weapon's skeleton is rigid and draws in its bind pose against the scene's
/// identity palette; the movement all comes from the hand.
pub struct Held {
    /// The wielder's animation key, which is how its current pose is found.
    /// `None` when the wielder has no cycle to play, in which case the hand
    /// stays at its bind-pose position -- still correct, just still.
    pub wielder: Option<(u32, Motion)>,
    /// The wielder's own per-instance transforms, kept because the held item's
    /// transform is this times the hand.
    ///
    /// A copy rather than a reference to the wielder's group: the two are
    /// rebuilt together and the alternative is threading a lifetime through
    /// every group in the scene to save a handful of matrices.
    pub wielders: Vec<Mat4>,
    /// Bone in the *wielder's* skeleton that the item hangs from.
    pub bone: usize,
    /// The attachment point, in the wielder's model space. A point, not an
    /// offset -- see [`m2::Attachment`].
    pub offset: Vec3,
}

pub struct Tile {
    pub terrain: LoadedTerrain,
    pub groups: Vec<Group>,
    /// Everything solid on this tile, in world space.
    ///
    /// Built with the tile and evicted with it. Rebuilding costs nothing
    /// beside reading the tile off disk, and a set that outlived its tile
    /// would be invisible walls where a building used to be.
    solid: collision::World,
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
    /// Which `AreaTable` row this chunk belongs to, and so which music and
    /// ambience the player standing on it should hear.
    ///
    /// Stored per *chunk* rather than per tile because it genuinely varies
    /// within one: a tile is a third of a mile square and Elwynn's tiles carry
    /// Goldshire, the abbey and open forest at once. Keying sound off the tile
    /// would change the music a third of a mile from where the zone does.
    area_id: u32,
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
                area_id: chunk.area_id,
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

    /// Which area a position sits in, or `None` off this tile.
    ///
    /// Deliberately shares `height_at`'s indexing rather than repeating it.
    /// That arithmetic is the subject of a long comment about the stored
    /// chunk indices tracking the *other* axis, and two copies of it would
    /// agree right up until someone fixed one of them.
    fn area_at(&self, x: f32, y: f32) -> Option<u32> {
        let side = adt::CHUNKS_PER_TILE as i64;
        let cx = (((self.origin.0 - x) / adt::CHUNK_SIZE).floor() as i64).clamp(0, side - 1);
        let cy = (((self.origin.1 - y) / adt::CHUNK_SIZE).floor() as i64).clamp(0, side - 1);
        let chunk = self.chunks.get((cy * side + cx) as usize)?.as_ref()?;
        // Zero means the chunk names no area, which is a real answer and not
        // a missing one -- plenty of open water and unfinished terrain does.
        (chunk.area_id != 0).then_some(chunk.area_id)
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
    /// -- see [`Motion::from_pace`].
    ///
    /// A speed rather than the "is it moving" flag this used to be, because
    /// the two cycles a moving creature can play are told apart by nothing
    /// else: a wolf padding along its patrol and a wolf charging you are both
    /// simply *moving*, and drawing the walk cycle for both makes the charge
    /// look like the model is being dragged.
    pub speed: f32,
    /// How fast it is turning on the spot, positive to the left -- see
    /// [`Motion::from_pace`]. Zero for everything the server drives; nothing on
    /// the wire says a creature is turning where it stands.
    pub turning: f32,
    /// Off the ground. False for everything the server drives: a creature's
    /// movement arrives as a path along the surface, and `MOVEFLAG_FALLING` on
    /// a relayed player is not read yet.
    pub airborne: bool,
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
    /// How this unit holds itself when armed. Taken from the look rather than
    /// passed separately, so it cannot disagree with the geometry it belongs
    /// to -- see `set_entities`.
    ///
    /// Only meaningful with a weapon drawn: the stance decides which ready and
    /// attack cycles are tried, and a stowed weapon leaves the character at
    /// ease whatever it is carrying.
    pub stance: Stance,
    /// Whether this unit's weapons are stowed rather than in its hands.
    ///
    /// Folded into the group key rather than read per instance, because it
    /// changes *which attachment* a held item hangs from and every instance in
    /// a group shares one answer. Two players in identical armour, one with a
    /// sword drawn and one without, must not share a bucket.
    pub sheathed: bool,
    /// How long ago this unit's sheath state was seen to change, if it was
    /// seen to -- see `world::state::Entity::sheath_changed_at`. `None` for
    /// a unit that entered view already in its current state, which must
    /// not play the draw/stow transition for a change nobody watched.
    pub sheath_changed_ms_ago: Option<u32>,
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
    /// Weapons and shields, keyed by path *and* texture.
    ///
    /// The texture has to be in the key: `Sword_1H_Short_A_02` is one file and
    /// the two items that use it -- a rusty one and a green one -- differ only
    /// in the skin the display row names. Keying by path alone would give the
    /// second character the first one's blade.
    held_cache: HashMap<(String, String), Option<Rc<CachedModel>>>,
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

/// Where a held item is drawn: the wielder's placement, through the hand, at
/// the attachment point.
///
/// A free function because the composition is the whole feature and the order
/// of it is the part that is easy to get wrong in a way nothing reports.
/// `offset` is a **point in the wielder's model space, not a delta from the
/// bone** -- an M2 attachment stores the same coordinate as the bone's own
/// pivot, and a bone matrix is built pivot-relative so the bind pose is the
/// identity. Adding the offset to the hand's translation instead of placing it
/// *through* the hand counts the pivot twice, which puts the sword out at
/// arm's length from the fist and looks like a model with a bad origin.
fn held_transform(wielder: Mat4, hand: Mat4, offset: Vec3) -> Mat4 {
    wielder * hand * Mat4::from_translation(offset)
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
            held_cache: HashMap::new(),
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
        let mut solid = collision::World::new();
        for (path, transforms) in groups {
            let Some(model) = self.model(gpu, meshes, chain, &path) else {
                continue;
            };
            // Placed into world space here rather than kept in model space and
            // transformed per query: a building is placed once and asked about
            // sixty times a second, and the grid can only index what has a
            // world position.
            for transform in &transforms {
                for triangle in &model.collision {
                    let p = |v: [f32; 3]| transform.transform_point3(Vec3::from(v));
                    solid.add(collision::Triangle::new(
                        p(triangle[0]),
                        p(triangle[1]),
                        p(triangle[2]),
                    ));
                }
            }
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
                held: None,
            });
        }

        tracing::debug!(
            "tile {},{} is solid in {} triangles",
            tile.0,
            tile.1,
            solid.triangle_count()
        );
        Ok(Tile {
            terrain,
            groups: built,
            heights: TileHeights::new(&parsed.chunks),
            solid,
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

    /// Which `AreaTable` area a position sits in, if its tile is resident.
    ///
    /// `None` while the tile is still streaming in, which a caller must not
    /// confuse with "nowhere": the answer arrives a moment later, and treating
    /// the gap as a zone change would stop the music every time the player
    /// crossed a tile boundary ahead of the loader.
    pub fn area_at(&self, x: f32, y: f32) -> Option<u32> {
        let tile = tile_at(Vec3::new(x, y, 0.0));
        self.tiles.get(&tile)?.heights.area_at(x, y)
    }

    /// Where a character ends up moving from `from` towards `to`.
    ///
    /// **Consults the tiles the move touches, not just the one it starts on.**
    /// A building straddling a tile boundary belongs to exactly one owner --
    /// see `load_tile` -- so asking only the starting tile would let a
    /// character walk through the half of the abbey that belongs to its
    /// neighbour.
    ///
    /// Falls through untouched when nothing solid is resident, which is the
    /// common case: most of a tile is open ground, and a query that found
    /// nothing must not perturb the position by so much as a float.
    pub fn slide(&self, from: Vec3, to: Vec3, radius: f32, height: f32, step: f32) -> Vec3 {
        let mut at = to;
        for tile in self.tiles_touching(from, to) {
            if tile.solid.is_empty() {
                continue;
            }
            at = tile.solid.slide(from, at, radius, height, step);
        }
        at
    }

    /// The height of any building floor under a point, or `None` for open
    /// ground where the terrain height field is the answer.
    pub fn floor_under(&self, at: Vec3, step: f32) -> Option<f32> {
        let tile = self.tiles.get(&tile_at(at))?;
        tile.solid
            .floor_under(at.truncate(), at.z, step)
    }

    /// Every resident tile a straight move between two points could touch.
    ///
    /// A short move is one or two tiles; listing them rather than assuming the
    /// start's is what makes a tile seam an implementation detail instead of a
    /// hole in the world.
    fn tiles_touching(&self, from: Vec3, to: Vec3) -> impl Iterator<Item = &Tile> {
        let (a, b) = (tile_at(from), tile_at(to));
        let xs = a.0.min(b.0)..=a.0.max(b.0);
        let ys = a.1.min(b.1)..=a.1.max(b.1);
        xs.flat_map(move |x| ys.clone().map(move |y| (x, y)))
            .filter_map(|key| self.tiles.get(&key))
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

        let entry = self.build(gpu, meshes, chain, path, &crate::model::Variations::default());
        if entry.is_none() {
            tracing::debug!("could not load {path}");
        }
        self.cache.insert(path.to_string(), entry.clone());
        entry
    }

    /// Loads a path into a `CachedModel`, without consulting or filling a cache.
    ///
    /// Shared by the tile loader and the held-item loader, which want the same
    /// work under different keys: a doodad is identified by its path alone,
    /// while a sword and the same sword in a different finish are one path and
    /// two textures.
    fn build(
        &self,
        gpu: &Gpu,
        meshes: &MeshRenderer,
        chain: &mut Chain,
        path: &str,
        variations: &crate::model::Variations,
    ) -> Option<Rc<CachedModel>> {
        // A named struct rather than the seven-element tuple this used to
        // thread through: the two branches produce the same set of parts, and
        // a tuple that long is a place for two of them to be swapped silently.
        struct Built {
            mesh: render::mesh::GpuMesh,
            draws: Vec<Draw>,
            textures: Vec<UploadedTexture>,
            bones: Vec<m2::AnimatedBone>,
            sequences: Vec<m2::Sequence>,
            attachments: Vec<m2::Attachment>,
            bounds: Option<(Vec3, Vec3)>,
            collision: Vec<[[f32; 3]; 3]>,
        }

        let lower = path.to_lowercase();
        let built = if lower.ends_with(".wmo") {
            // No skeleton to speak of, so nothing to animate and nothing to
            // hang off it.
            crate::world_object::load(gpu, chain, path, None)
                .map(|w| Built {
                    mesh: w.mesh,
                    draws: w.draws,
                    textures: w.textures,
                    bones: Vec::new(),
                    sequences: Vec::new(),
                    attachments: Vec::new(),
                    bounds: None,
                    collision: w.collision,
                })
                .ok()
        } else {
            crate::model::load(gpu, chain, path, variations, 0)
                .map(|m| {
                    // Named rather than dropped. `load_dressed` has always
                    // collected these and callers have always thrown them
                    // away -- which is how every humanoid NPC rendered white
                    // in silence once already. A weapon whose skin fails to
                    // resolve is a grey sword, not a missing one, and nothing
                    // else would ever say so.
                    if !m.missing_textures.is_empty() {
                        tracing::warn!(
                            "{path} drew with {} placeholder texture(s): {}",
                            m.missing_textures.len(),
                            m.missing_textures.join(", ")
                        );
                    }
                    Built {
                        mesh: m.mesh,
                        draws: m.draws,
                        textures: m.textures,
                        bones: m.bones,
                        sequences: m.sequences,
                        attachments: m.attachments,
                        bounds: Some((m.min, m.max)),
                        collision: m.collision,
                    }
                })
                .ok()
        };

        built.map(|b| {
            let binds = b
                .textures
                .iter()
                .map(|t| meshes.material_bind_group(gpu, &t.view))
                .collect();
            Rc::new(CachedModel {
                mesh: b.mesh,
                draws: b.draws,
                binds,
                bones: b.bones,
                sequences: b.sequences,
                attachments: b.attachments,
                bounds: b.bounds,
                textures: b.textures,
                collision: b.collision,
            })
        })
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
        let mut sheathed: HashMap<u64, bool> = HashMap::new();
        let mut grouped: HashMap<(u32, Motion, u64), Vec<Mat4>> = HashMap::new();
        // One clock reading for the whole rebuild, so two units that died in
        // the same frame land in the same bucket and share a pose rather than
        // missing each other by a millisecond.
        let now_ms = self.started.elapsed().as_millis() as u32;
        for placement in placements {
            // **One key, computed once, used by every side table.** A game
            // object shares the display-id space with creatures and means
            // something else by it, and a drawn weapon hangs somewhere a
            // stowed one does not, so both are folded in here rather than
            // widening every tuple -- the key already exists to keep things
            // that look different apart.
            //
            // Computed once because it was not, briefly, and the looks table
            // was then filled under the bare look key and read under the
            // combined one. Every match was an accident of the extra terms
            // being zero for units, and the first non-zero one would have
            // undressed the player rather than failing.
            let key = placement.look_key
                ^ game_object_key(placement.kind)
                ^ sheath_key(placement.sheathed);
            looks.insert(key, placement.look.clone());
            kinds.insert(key, placement.kind);
            sheathed.insert(key, placement.sheathed);
            // The first held item with a resting place decides which of
            // `Sheath`/`HipSheath` a transition plays -- there is only ever
            // one weapon-shaped decision to make here, since a shield's own
            // rest point (28) is centred and mirrors neither cycle any
            // better than the other.
            let rest_kind = placement
                .look
                .as_deref()
                .and_then(|look| look.held.iter().find_map(|item| item.stowed))
                .map(|attachment| {
                    if matches!(attachment, 32 | 33) {
                        RestKind::Hip
                    } else {
                        RestKind::Back
                    }
                });
            let sheathing = match (placement.sheath_changed_ms_ago, rest_kind) {
                (Some(age), Some(rest)) => Some((age, rest)),
                _ => None,
            };
            grouped
                .entry((
                    placement.display_id,
                    Motion::resolve(
                        placement.speed,
                        placement.turning,
                        placement.airborne,
                        placement.dead,
                        placement.died_ms_ago,
                        placement.swung_ms_ago,
                        placement.fighting,
                        now_ms,
                        // Stowed weapons leave the hands free, so the stance
                        // only applies while something is drawn.
                        if placement.sheathed {
                            Stance::Unarmed
                        } else {
                            placement.stance
                        },
                        sheathing,
                    ),
                    key,
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

            // Whatever this group is carrying, as groups of its own. Built here
            // rather than in the draw loop because a held item is an ordinary
            // group in every respect except where its transforms come from --
            // see `Held`.
            let stowed = sheathed.get(&look_key).copied().unwrap_or(false);
            // Whether this rebuild actually resolved to the model's own
            // `Sheath`/`HipSheath` cycle, as opposed to falling back to
            // `Stand` because the model has neither -- see `plays_once`'s
            // doc comment. Only the real cycle earns holding the item in
            // hand for its duration; a model with no such cycle gets today's
            // instant switch, which is correct for it rather than merely
            // acceptable.
            let mid_transition = matches!(motion, Motion::Sheathing(..))
                && resolved.is_some_and(|i| {
                    matches!(
                        model.sequences[i].id,
                        SHEATH_ANIMATION_ID | HIP_SHEATH_ANIMATION_ID
                    )
                });
            for item in look.iter().flat_map(|look| look.held.iter()) {
                // Stowed items move to their resting place; one with nowhere to
                // rest -- a bow, a thrown axe -- stays in the hand, which is
                // what a `sheathe_type` of zero means rather than a gap.
                //
                // Mid-transition, the item stays in the hand regardless of
                // which way `stowed` points: the animation is the hand
                // travelling to or from the resting place, and moving the
                // item there instantly would have it arrive before the hand
                // does and the animation catch up around empty air.
                let wanted = if mid_transition {
                    item.attachment
                } else {
                    match stowed {
                        true => sheath_override().or(item.stowed).unwrap_or(item.attachment),
                        false => item.attachment,
                    }
                };
                let Some(attachment) = model
                    .attachments
                    .iter()
                    .find(|a| a.id == wanted)
                    .copied()
                else {
                    // A model that has no such attachment point cannot hold the
                    // item, and drawing it at the model's origin instead would
                    // put a sword through the character's feet. Named, because
                    // "this race has no right hand" is a real finding.
                    tracing::debug!(
                        "display {display_id} has no attachment {wanted} for {} ({})",
                        item.model,
                        if stowed { "stowed" } else { "drawn" }
                    );
                    continue;
                };
                let Some(held_model) = self.held_model(gpu, meshes, chain, item) else {
                    // Deliberately not counted as an undrawable *object*: the
                    // wielder is on screen and the count exists to say how much
                    // of the world is missing.
                    tracing::warn!("{} could not be loaded", item.model);
                    continue;
                };
                meshes.prepare(gpu, held_model.draws.iter().map(|d| d.state));
                // Trace, not debug: this runs on every rebuild, several times a
                // second, and at debug it buried a session's log in nineteen
                // megabytes of the same line. What is worth saying once --
                // which item resolved to which file -- is said at login by
                // `character::held_items`.
                tracing::trace!(
                    "display {display_id} holds {} on bone {} at {:?} ({} draw(s), {} instance(s))",
                    item.model,
                    attachment.bone,
                    attachment.position,
                    held_model.draws.len(),
                    transforms.len(),
                );
                // Seeded with the bind-pose answer rather than with zeroes.
                // `update_animations` overwrites this before the frame is
                // drawn, but a buffer that must be written before it is read
                // should still start as something *visible*: a zero matrix
                // collapses a model to the origin in complete silence, which
                // this project has already lost a whole feature to once.
                let bind_pose: Vec<Instance> = transforms
                    .iter()
                    .map(|t| {
                        Instance::from_cols_array_2d(
                            (*t * Mat4::from_translation(Vec3::from(attachment.position)))
                                .to_cols_array_2d(),
                        )
                    })
                    .collect();
                built.push(Group {
                    model: held_model,
                    instances: InstanceBuffer::upload(gpu, &bind_pose),
                    count: raw.len() as u32,
                    animation: None,
                    held: Some(Held {
                        wielder: animation,
                        wielders: transforms.clone(),
                        bone: attachment.bone as usize,
                        offset: Vec3::from(attachment.position),
                    }),
                });
            }

            built.push(Group {
                model,
                instances: InstanceBuffer::upload(gpu, &raw),
                count: raw.len() as u32,
                animation,
                held: None,
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
        // Poses are kept rather than discarded because a second consumer needs
        // the same matrices: an item in a hand is placed by the very bone the
        // wielder was posed with. Recomputing it separately would be the
        // opposite of the rule that says two things which must agree exactly
        // should be derived from one source -- and a hand that disagreed with
        // its own model by a frame is a sword that trails behind the arm.
        let mut poses: HashMap<(u32, Motion), Vec<Mat4>> = HashMap::new();
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
            let posed = m2::Model::pose_bones(&group.model.bones, sequence, time_ms);
            let pose: Vec<[[f32; 4]; 4]> = posed.iter().map(|m| m.to_cols_array_2d()).collect();
            meshes.update_bones(gpu, bones, &pose);
            poses.insert((display_id, motion), posed);
        }

        // Then everything hanging off those poses. A held item's transform is
        // the wielder's own instance transform times the hand's animated
        // matrix, so it has to be rewritten every frame for the same reason the
        // pose does -- a weapon updated at the rebuild rate visibly lags the
        // arm holding it.
        for group in &self.entities {
            let Some(held) = &group.held else { continue };
            // No pose means the wielder had no cycle to play. Its bones are
            // identity, so the hand is at its bind-pose position and the item
            // belongs there -- still, but in the right place.
            let hand = held
                .wielder
                .and_then(|key| poses.get(&key))
                .and_then(|pose| pose.get(held.bone))
                .copied()
                .unwrap_or(Mat4::IDENTITY);
            let instances: Vec<Instance> = held
                .wielders
                .iter()
                .map(|t| {
                    Instance::from_cols_array_2d(
                        held_transform(*t, hand, held.offset).to_cols_array_2d(),
                    )
                })
                .collect();
            if let Some(first) = held.wielders.first() {
                tracing::trace!(
                    "held item at {:?} (wielder at {:?}, posed: {})",
                    held_transform(*first, hand, held.offset).transform_point3(Vec3::ZERO),
                    first.transform_point3(Vec3::ZERO),
                    held.wielder.is_some_and(|key| poses.contains_key(&key)),
                );
            }
            group.instances.write(gpu, &instances);
        }
    }

    /// The animated bone buffer for a `Group::animation` key, if `set_entities`
    /// gave it one this rebuild.
    pub fn entity_bone_buffer(&self, key: (u32, Motion)) -> Option<&BoneBuffer> {
        self.entity_bones.get(&key)
    }

    /// Loads a weapon or shield, with the skin its item display names.
    ///
    /// The texture arrives as a bare name -- `Sword_1H_Short_A_02Rusty` -- and
    /// is resolved against the model's own directory by the same
    /// [`crate::model::Variations`] path a creature's skin takes. That is not a
    /// coincidence worth hiding: a weapon leaves its texture slot to be filled
    /// at runtime exactly as a creature does, and the loader already knew how.
    fn held_model(
        &mut self,
        gpu: &Gpu,
        meshes: &MeshRenderer,
        chain: &mut Chain,
        item: &crate::character::HeldItem,
    ) -> Option<Rc<CachedModel>> {
        let key = (item.model.clone(), item.texture.clone());
        if let Some(cached) = self.held_cache.get(&key) {
            return cached.clone();
        }
        let variations = crate::model::Variations(vec![item.texture.clone()]);
        let entry = self.build(gpu, meshes, chain, &item.model, &variations);
        self.held_cache.insert(key, entry.clone());
        entry
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
                    attachments: loaded.attachments,
                    // This is the cache click-to-target reads from, so this is
                    // the one that has to carry the model's extent.
                    bounds: Some((loaded.min, loaded.max)),
                    textures: loaded.textures,
                    // A replicated entity's own body is not scenery: creatures
                    // and players are moved by the server, and colliding with
                    // them is a different feature from colliding with the
                    // world. Left empty rather than filled in unused.
                    collision: Vec::new(),
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
        // `trace`, for the same reason as `own body` in `live.rs`: this is
        // asked once per object per frame, so at debug it is most of the log.
        tracing::trace!("game object display {display_id} -> {path}");
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

/// `OWC_SHEATH_ATTACH=<id>` hangs every stowed item from one attachment point.
///
/// A diagnostic, kept for the same reason `OWC_NO_CULL` is: it is how the
/// resting places were identified, and it is how the next one will be. The
/// geometry narrows the candidates to a handful -- a mirrored pair high on the
/// shoulder blades, another on the upper back, a third at the hips -- but
/// which of them a greatsword actually uses is a question about how a model
/// looks, and only a render answers those. Sweeping it beats rebuilding once
/// per guess.
fn sheath_override() -> Option<u32> {
    std::env::var("OWC_SHEATH_ATTACH").ok()?.trim().parse().ok()
}

/// Keeps a unit with its weapon drawn out of the same bucket as one without.
///
/// The same trick as [`game_object_key`], and for the same reason: the sheath
/// state decides which attachment a held item hangs from, so two otherwise
/// identical characters need separate groups or the first drawn would put
/// everyone's sword in their hand.
fn sheath_key(sheathed: bool) -> u64 {
    if sheathed {
        0x5ea7_4ed0_0000_0000
    } else {
        0
    }
}

/// `AnimationData.dbc` rows for the cycles this client plays -- public spec
/// (documented on wowdev.wiki as part of the client's own animation-id
/// table), not derived from any server implementation. Every 3.3.5a model's
/// sequences use the same ids for the same actions.
const STAND_ANIMATION_ID: u16 = 0;
const WALK_ANIMATION_ID: u16 = 4;
const RUN_ANIMATION_ID: u16 = 5;
/// `Walkbackwards`, whose own fallback in `AnimationData.dbc` is `Walk`.
const WALK_BACK_ANIMATION_ID: u16 = 13;
/// `AnimationData.dbc` rows 11 and 12, read from the table rather than
/// remembered.
const SHUFFLE_LEFT_ANIMATION_ID: u16 = 11;
const SHUFFLE_RIGHT_ANIMATION_ID: u16 = 12;
/// `AnimationData` rows 38 and 40. Read from the table: the *sequence* indices
/// for these on the human male are 31 and 17, which is exactly the confusion
/// that makes transcribing an id out of a model listing a mistake.
const JUMP_ANIMATION_ID: u16 = 38;
const FALL_ANIMATION_ID: u16 = 40;
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
/// `Attack2H`, whose own fallback in the table is `Attack1H`.
const ATTACK_2H_ANIMATION_ID: u16 = 18;
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
/// `Ready2H`, whose own fallback in the table is `ReadyUnarmed`.
const READY_2H_ANIMATION_ID: u16 = 27;
const READY_UNARMED_ANIMATION_ID: u16 = 25;
/// The hand travelling to or from a weapon's resting place. `AnimationData`
/// rows 32 and 65, confirmed against `wow-cli m2 anims` on the human male
/// rather than assumed from the row numbers alone: both list at 1000ms,
/// named `Sheath` and `HipSheath` respectively.
const SHEATH_ANIMATION_ID: u16 = 32;
const HIP_SHEATH_ANIMATION_ID: u16 = 65;

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

/// The same shape of ceiling as `ATTACK_CEILING_MS`, for the draw/stow
/// transition. `Sheath` and `HipSheath` are both 1000ms on the human male
/// (`wow-cli m2 anims`); this gives a little margin without freezing the
/// hand on its follow-through for noticeably longer than the motion itself.
const SHEATH_CEILING_MS: u32 = 1_500;

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
    /// Backing up. A cycle of its own rather than the walk played in reverse:
    /// the model carries `Walkbackwards`, and a character reversing at a run
    /// looks like a sprint performed facing the wrong way.
    WalkBack,
    /// Off the ground.
    ///
    /// One state for the whole arc rather than the three the table offers.
    /// `AnimationData` has `JumpStart` (37), `Jump` (38), `JumpEnd` (39) and
    /// `Fall` (40), and sequencing them properly needs to know how far through
    /// a jump is and whether it is still rising -- which the caller knows and
    /// this enum, keyed for a cache, deliberately does not. `Jump` reads
    /// correctly for the whole flight; the other three are the refinement, not
    /// the feature.
    Airborne,
    /// Turning on the spot, and which way.
    ///
    /// **Not sidestepping.** That was the first reading and it was reported
    /// back as the character shimmying while it strafed -- see
    /// [`Motion::from_pace`], where the data that should have said so first is
    /// written up.
    ///
    /// The models carry these -- `ShuffleLeft` and `ShuffleRight`, sequences 38
    /// and 39 on the human male, half a second each. They were nearly declared
    /// absent: `m2 anims` defaults to thirty of a hundred and fifty-six
    /// sequences, and the first search for them came back empty from a list
    /// that stopped at index 29. A truncated listing answers a different
    /// question from the one asked.
    ///
    /// Both cycles advance the character by 0.00, which is right rather than
    /// suspicious: they are stepping motions played *in place* while the
    /// movement system does the travelling, exactly like `Walkbackwards`.
    Shuffle(Side),
    /// Toppling over, from the world-clock millisecond it began.
    Dying(u32),
    /// Settled: lying still, and no longer tied to when death happened.
    Dead,
    /// Mid-swing, from the world-clock millisecond the blow landed.
    Attacking(u32, Stance),
    /// Weapon out: guard up rather than at ease. Held for as long as the
    /// weapon is drawn, not only during a fight -- a character standing in
    /// town with a greatsword in hand still grips it with both hands.
    Ready(Stance),
    /// Moving between the hand and its resting place, from the world-clock
    /// millisecond the transition began.
    ///
    /// `RestKind` says which of `Sheath`/`HipSheath` to try -- the same
    /// weapon plays a different cycle depending on whether it rides the back
    /// or the hip, and that is a fact about the *item*, not about the state
    /// asking for it, so it travels in the key rather than being decided
    /// twice.
    Sheathing(u32, RestKind),
}

/// Where a transitioning weapon rests, which chooses between the two
/// draw/stow cycles a character model carries -- see [`Motion::Sheathing`].
///
/// Not a bare `bool`: `character::sheath_rule` already names the resting
/// *attachment*, and re-deriving "hip or back" from a bool at every call site
/// would be the guess this project keeps refusing to make twice.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RestKind {
    /// `Sheath` -- a two-hander or a shield, slung across the back.
    Back,
    /// `HipSheath` -- a one-hander, at the belt.
    Hip,
}

/// Which way a character is sidestepping.
///
/// Positive lateral is *left*, matching `world::motion::Motion::lateral`, whose
/// `Axis::Positive` is `strafe_left`. Stated because the two enums live in
/// different crates and a sign convention agreed by accident is one that gets
/// broken by accident.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Side {
    Left,
    Right,
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
    /// **The sign says which way.** A magnitude alone cannot tell walking
    /// forward from backing up, and 3.3.5a has a whole animation for the
    /// latter (`Walkbackwards`, row 13). Backing up at the run speed and
    /// playing the run cycle is what this did before, and it reads as the
    /// character sprinting while facing the wrong way -- reported from play as
    /// exactly that. Negative is backwards; there is only one backwards cycle,
    /// so its magnitude chooses nothing.
    ///
    /// **Strafing is travelling, and travelling plays a travelling cycle**, so
    /// the caller folds a sidestep into `forward` and there is nothing left
    /// for a lateral term to decide. Which cycle that should be was argued
    /// from renders three times and flipped three times; what ended it was
    /// `AnimationData.dbc`'s `body_flags`, whose **bit 64 marks exactly the
    /// twenty-eight animations that carry the character somewhere** and
    /// nothing else in 506 rows. `ShuffleLeft` and `ShuffleRight` do not carry
    /// it -- they hold `Stand`'s exact value -- which is precisely how the
    /// last report described them: *he stands perfectly still and his feet
    /// shuffle*. See `live_pace` in the viewer for the two pieces of evidence
    /// that had been used instead and do not hold.
    ///
    /// So the shuffles are what turning on the spot gets: `turning` is the
    /// signed rate the A and D keys apply, and it only chooses a cycle when
    /// nothing else is happening. **That last part is still the one claim here
    /// without direct evidence** -- a table saying a cycle does not travel says
    /// what it is not. It is now at least the only in-place lateral cycle
    /// there is, put to the only in-place lateral gesture there is.
    ///
    /// `turning` is positive to the left, matching both `Axis::Positive` and
    /// [`Side`]. Replicated creatures pass zero, and that is an absence rather
    /// than a claim: nothing on the wire says a creature is turning in place.
    pub fn from_pace(forward: f32, turning: f32) -> Self {
        if forward > 0.0 {
            return if forward < RUN_SPEED {
                Motion::Walk
            } else {
                Motion::Run
            };
        }
        if forward < 0.0 {
            return Motion::WalkBack;
        }
        if turning > 0.0 {
            Motion::Shuffle(Side::Left)
        } else if turning < 0.0 {
            Motion::Shuffle(Side::Right)
        } else {
            Motion::Stand
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
    #[allow(clippy::too_many_arguments)]
    pub fn resolve(
        speed: f32,
        lateral: f32,
        airborne: bool,
        dead: bool,
        died_ms_ago: Option<u32>,
        swung_ms_ago: Option<u32>,
        fighting: bool,
        now_ms: u32,
        stance: Stance,
        sheathing: Option<(u32, RestKind)>,
    ) -> Self {
        if dead {
            return match died_ms_ago {
                Some(age) if age < ONE_SHOT_CEILING_MS => {
                    Motion::Dying(bucket(now_ms.saturating_sub(age)))
                }
                _ => Motion::Dead,
            };
        }
        // **Below dead and above everything else.** A corpse does not jump,
        // and a character mid-jump is not standing, walking or swinging
        // whatever the keys say.
        if airborne {
            return Motion::Airborne;
        }
        let moving = Motion::from_pace(speed, lateral);
        if moving == Motion::Stand {
            if let Some(age) = swung_ms_ago {
                if age < ATTACK_CEILING_MS {
                    return Motion::Attacking(bucket(now_ms.saturating_sub(age)), stance);
                }
            }
            // **Below a swing, above the ready/at-ease fallback.** A swing
            // mid-transition is the more informative of the two and wins,
            // the same reasoning that lets a run outrank a swing above. A
            // sheath change is rarer and briefer than either, so it loses
            // only to the state that is itself rare and brief.
            if let Some((changed_ms_ago, rest)) = sheathing {
                if changed_ms_ago < SHEATH_CEILING_MS {
                    return Motion::Sheathing(bucket(now_ms.saturating_sub(changed_ms_ago)), rest);
                }
            }
            // **A drawn weapon is enough on its own.** This used to require
            // `fighting`, which was right while nothing was ever drawn outside
            // a fight -- and wrong the moment weapons appeared, because a
            // character standing with a sword out was drawn in the at-ease
            // idle with an open hand and the grip floating through it.
            if fighting || stance != Stance::Unarmed {
                return Motion::Ready(stance);
            }
        }
        moving
    }

    /// Whether this cycle is about a fight or a death, and so worth a log line.
    fn is_notable(self) -> bool {
        !matches!(
            self,
            Motion::Stand
                | Motion::Walk
                | Motion::Run
                | Motion::WalkBack
                | Motion::Shuffle(_)
                | Motion::Airborne
        )
    }

    /// When a one-shot cycle began, on the caller's world clock.
    fn started_at(self) -> Option<u32> {
        match self {
            Motion::Dying(at) | Motion::Attacking(at, _) | Motion::Sheathing(at, _) => Some(at),
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
            // The fallback is the table's own (row 13 names row 4), and it is
            // the right one: a model with no reverse cycle walking forwards
            // while it retreats is odd, where standing still as it slides is
            // the bug this whole family exists to avoid.
            Motion::WalkBack => &[WALK_BACK_ANIMATION_ID, WALK_ANIMATION_ID],
            // **The table's own fallback, which was right and was overruled.**
            // `AnimationData` sends both shuffles to row 0, Stand. That was
            // read as a mistake and replaced with the travelling cycles, on
            // the rule that a model sliding on the spot is worse than one
            // standing -- but that rule is about a unit that is *going*
            // somewhere, and a shuffle is a unit that is not. The table was
            // describing what the cycle is, and the disagreement was the first
            // sign that it had been given the wrong job.
            // Falling back to the run rather than to standing: a character
            // sailing through the air in its idle pose is the same failure as
            // one sliding along the ground in it.
            Motion::Airborne => &[JUMP_ANIMATION_ID, FALL_ANIMATION_ID, RUN_ANIMATION_ID],
            Motion::Shuffle(Side::Left) => {
                &[SHUFFLE_LEFT_ANIMATION_ID, STAND_ANIMATION_ID]
            }
            Motion::Shuffle(Side::Right) => {
                &[SHUFFLE_RIGHT_ANIMATION_ID, STAND_ANIMATION_ID]
            }
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
            // **The chains follow `AnimationData.dbc`'s own fallback column,
            // not a guess.** Row 18 (`Attack2H`) names 17 (`Attack1H`), which
            // names 16 (`AttackUnarmed`), which names 0 (`Stand`); rows 26 and
            // 27 (`Ready1H`, `Ready2H`) both name 25 (`ReadyUnarmed`). Reading
            // the table beats inventing an order, and the table happens to
            // agree with what a two-handed model should do when it has no
            // two-handed cycle.
            //
            // The final `Stand` is not decoration: `Creature\Wolf\Wolf.m2`
            // has `AttackUnarmed` and `Death` and **no ready stance at all**,
            // so without it a wolf entering combat resolves to no sequence and
            // draws its bind pose, stiff and T-posed.
            Motion::Attacking(_, Stance::TwoHand) => &[
                ATTACK_2H_ANIMATION_ID,
                ATTACK_1H_ANIMATION_ID,
                ATTACK_UNARMED_ANIMATION_ID,
                STAND_ANIMATION_ID,
            ],
            Motion::Attacking(_, _) => &[
                ATTACK_1H_ANIMATION_ID,
                ATTACK_UNARMED_ANIMATION_ID,
                STAND_ANIMATION_ID,
            ],
            Motion::Ready(Stance::TwoHand) => &[
                READY_2H_ANIMATION_ID,
                READY_UNARMED_ANIMATION_ID,
                STAND_ANIMATION_ID,
            ],
            Motion::Ready(_) => &[
                READY_1H_ANIMATION_ID,
                READY_UNARMED_ANIMATION_ID,
                STAND_ANIMATION_ID,
            ],
            // Falls back to plain standing on a model with no draw/stow
            // cycle at all -- a creature, say. `set_entities` reads whether
            // the *resolved* sequence actually is one of the two before
            // holding the item in hand for the duration, exactly the
            // question `plays_once` answers about clamping: the fallback
            // changes what the animation means, not just what plays.
            Motion::Sheathing(_, RestKind::Back) => &[SHEATH_ANIMATION_ID, STAND_ANIMATION_ID],
            Motion::Sheathing(_, RestKind::Hip) => &[HIP_SHEATH_ANIMATION_ID, STAND_ANIMATION_ID],
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
        DEATH_ANIMATION_ID
            | ATTACK_1H_ANIMATION_ID
            | ATTACK_2H_ANIMATION_ID
            | ATTACK_UNARMED_ANIMATION_ID
            | SHEATH_ANIMATION_ID
            | HIP_SHEATH_ANIMATION_ID
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

    /// In the bind pose a held item lands exactly on the attachment point, in
    /// the wielder's own frame -- turned with the wielder, not left facing
    /// north.
    ///
    /// The rotation is the half that a plausible-looking wrong answer gets
    /// wrong. `wielder.translation + offset` puts the sword in the right place
    /// for a character facing north and beside a character facing any other
    /// way, which on a moving player reads as the weapon orbiting them.
    #[test]
    fn a_held_item_sits_at_the_attachment_point_in_the_wielders_frame() {
        let quarter = std::f32::consts::FRAC_PI_2;
        let wielder = Mat4::from_rotation_translation(
            glam::Quat::from_rotation_z(quarter),
            Vec3::new(100.0, 200.0, 30.0),
        );
        // A right hand: forward a little, out to -Y, up at chest height.
        let offset = Vec3::new(-0.06, -0.48, 0.90);

        let placed = held_transform(wielder, Mat4::IDENTITY, offset)
            .transform_point3(Vec3::ZERO);
        // A quarter turn about Z takes (x, y) to (-y, x).
        let expected = Vec3::new(100.0 + 0.48, 200.0 - 0.06, 30.9);
        assert!(
            (placed - expected).length() < 1e-4,
            "held item at {placed} rather than {expected}"
        );
        assert!(
            (placed - wielder.transform_point3(Vec3::ZERO)).length() > 0.5,
            "the item was drawn at the wielder's origin, not in its hand"
        );
    }

    /// The hand's own matrix moves the item, and moves it *with* the wielder's
    /// placement rather than in world space.
    ///
    /// This is the composition order. Swap the two and a character standing a
    /// hundred metres from the origin swings a sword that stays near the
    /// origin -- which looks like the weapon failing to load rather than like
    /// a matrix in the wrong order.
    #[test]
    fn the_hands_animation_applies_inside_the_wielders_placement() {
        let wielder = Mat4::from_translation(Vec3::new(1000.0, 0.0, 0.0));
        let raised = Mat4::from_translation(Vec3::new(0.0, 0.0, 1.0));
        let offset = Vec3::new(0.0, -0.5, 1.0);

        let placed = held_transform(wielder, raised, offset).transform_point3(Vec3::ZERO);
        assert!(
            (placed - Vec3::new(1000.0, -0.5, 2.0)).length() < 1e-4,
            "hand motion applied outside the placement: {placed}"
        );
    }

    /// Backing up is its own cycle, at any pace, and forward is unaffected.
    ///
    /// Reported from play: retreating ran the *forward run* animation at the
    /// full run speed, so the character appeared to sprint while facing the
    /// wrong way. The sign of the speed is what carries the direction, and the
    /// magnitude chooses nothing once it is negative -- there is only one
    /// backwards cycle.
    #[test]
    fn retreating_has_its_own_cycle_whatever_the_pace() {
        assert_eq!(Motion::from_pace(-4.5, 0.0), Motion::WalkBack);
        assert_eq!(Motion::from_pace(-0.5, 0.0), Motion::WalkBack);
        assert_eq!(Motion::from_pace(-9.0, 0.0), Motion::WalkBack);
        // The forward answers must be untouched, which is the half that stops
        // a sign bug from turning every walk into a retreat.
        assert_eq!(Motion::from_pace(0.0, 0.0), Motion::Stand);
        assert_eq!(Motion::from_pace(2.5, 0.0), Motion::Walk);
        assert_eq!(Motion::from_pace(7.0, 0.0), Motion::Run);
    }

    /// Turning on the spot has its own cycle; travelling never does.
    ///
    /// **This test is the shape of the bug that produced it.** The shuffle was
    /// first given to sidestepping, and a character strafing at the run speed
    /// with a half-second in-place cycle on its legs was reported back as
    /// shimmying. So the assertion that matters is the *negative* one: no
    /// amount of travelling, in any direction, may select a shuffle.
    #[test]
    fn only_turning_on_the_spot_shuffles() {
        assert_eq!(Motion::from_pace(0.0, 3.0), Motion::Shuffle(Side::Left));
        assert_eq!(Motion::from_pace(0.0, -3.0), Motion::Shuffle(Side::Right));

        // Travelling outranks it, in every direction and even while turning.
        assert_eq!(Motion::from_pace(7.0, 3.0), Motion::Run);
        assert_eq!(Motion::from_pace(2.5, -3.0), Motion::Walk);
        assert_eq!(Motion::from_pace(-4.5, 3.0), Motion::WalkBack);

        // And standing still, turning at nothing, is standing still.
        assert_eq!(Motion::from_pace(0.0, 0.0), Motion::Stand);
    }

    /// The shuffle cycles are the ids the table names, and they loop.
    #[test]
    fn turning_resolves_to_the_shuffle_cycles() {
        let left = Motion::Shuffle(Side::Left).animation_ids();
        let right = Motion::Shuffle(Side::Right).animation_ids();
        assert_eq!(left.first(), Some(&SHUFFLE_LEFT_ANIMATION_ID));
        assert_eq!(right.first(), Some(&SHUFFLE_RIGHT_ANIMATION_ID));
        assert_ne!(left.first(), right.first(), "both sides play one cycle");
    }

    /// And the backwards cycle falls back to the forward walk, per the table.
    #[test]
    fn retreating_falls_back_to_walking() {
        let ids = Motion::WalkBack.animation_ids();
        assert_eq!(ids.first(), Some(&WALK_BACK_ANIMATION_ID));
        assert_eq!(ids.last(), Some(&WALK_ANIMATION_ID));
        // Not to standing: a model sliding backwards on the spot is the bug
        // the travelling cycles exist to avoid.
        assert!(!ids.contains(&STAND_ANIMATION_ID));
    }

    /// A drawn weapon holds the guard up on its own, without a fight.
    ///
    /// Reported the first time weapons were ever drawn: *"the hand is open
    /// holding the sword like he was precombat"*. Standing at ease resolved to
    /// the plain idle, whose hands are open, so the grip floated through the
    /// fingers. The `Ready` stance was already here and was gated on
    /// `fighting`, which was indistinguishable from correct while nothing was
    /// ever drawn outside combat.
    #[test]
    fn a_drawn_weapon_is_enough_to_hold_the_guard() {
        // Not fighting, standing still, weapon out.
        assert_eq!(
            Motion::resolve(0.0, 0.0, false, false, None, None, false, 4_000, Stance::TwoHand, None),
            Motion::Ready(Stance::TwoHand),
        );
        // And with nothing drawn it still relaxes, which is the half that
        // stops this from simply making everyone stand guard forever.
        assert_eq!(
            Motion::resolve(0.0, 0.0, false, false, None, None, false, 4_000, Stance::Unarmed, None),
            Motion::Stand,
        );
        // Moving still outranks it: a character runs, weapon or no weapon.
        assert_eq!(
            Motion::resolve(7.0, 0.0, false, false, None, None, false, 4_000, Stance::TwoHand, None),
            Motion::Run,
        );
    }

    /// A two-handed weapon asks for the two-handed cycles first, and every
    /// chain still ends somewhere a model without them can go.
    ///
    /// The order is `AnimationData.dbc`'s own fallback column, not a guess:
    /// row 18 (`Attack2H`) names 17, row 27 (`Ready2H`) names 25.
    #[test]
    fn the_two_handed_stance_prefers_its_own_cycles() {
        let ready = Motion::Ready(Stance::TwoHand).animation_ids();
        assert_eq!(ready.first(), Some(&READY_2H_ANIMATION_ID));
        let swing = Motion::Attacking(0, Stance::TwoHand).animation_ids();
        assert_eq!(swing.first(), Some(&ATTACK_2H_ANIMATION_ID));

        // A one-handed character must not get them, or every dagger is held
        // like a greatsword -- the half that a "prefers 2H" assertion alone
        // would pass without.
        assert!(!Motion::Ready(Stance::OneHand)
            .animation_ids()
            .contains(&READY_2H_ANIMATION_ID));
        assert!(!Motion::Attacking(0, Stance::OneHand)
            .animation_ids()
            .contains(&ATTACK_2H_ANIMATION_ID));

        for stance in [Stance::Unarmed, Stance::OneHand, Stance::TwoHand] {
            for motion in [Motion::Ready(stance), Motion::Attacking(0, stance)] {
                assert_eq!(
                    motion.animation_ids().last(),
                    Some(&STAND_ANIMATION_ID),
                    "{motion:?} can resolve to nothing"
                );
            }
        }
    }

    /// A swing that plays once must be recognised whichever grip threw it.
    ///
    /// `plays_once` keys on the resolved animation id, so a new attack id has
    /// to be added there as well -- miss it and a two-handed swing *loops*,
    /// which is a character flailing rather than striking.
    #[test]
    fn a_two_handed_swing_plays_once_like_the_others() {
        assert!(plays_once(ATTACK_2H_ANIMATION_ID));
        assert!(plays_once(ATTACK_1H_ANIMATION_ID));
        assert!(!plays_once(READY_2H_ANIMATION_ID));
    }

    /// The same trap in a new pair of ids: miss either in `plays_once` and a
    /// draw or a stow loops, which is a character repeatedly drawing a
    /// weapon that is already drawn.
    #[test]
    fn sheathing_plays_once_like_the_other_one_shots() {
        assert!(plays_once(SHEATH_ANIMATION_ID));
        assert!(plays_once(HIP_SHEATH_ANIMATION_ID));
        assert!(!plays_once(STAND_ANIMATION_ID));
    }

    /// A recent sheath change, standing still, resolves to the transition --
    /// and to the cycle the caller named, not a guessed one.
    #[test]
    fn a_recent_sheath_change_plays_the_transition() {
        assert_eq!(
            Motion::resolve(
                0.0, 0.0, false, false, None, None, false, 4_000, Stance::Unarmed,
                Some((200, RestKind::Hip)),
            ),
            Motion::Sheathing(3_800, RestKind::Hip)
        );
        assert_eq!(
            Motion::resolve(
                0.0, 0.0, false, false, None, None, false, 4_000, Stance::Unarmed,
                Some((200, RestKind::Back)),
            ),
            Motion::Sheathing(3_800, RestKind::Back)
        );
    }

    /// **A swing is the more informative of the two and wins**, the same
    /// reasoning that lets a run outrank a swing above: a character
    /// mid-transition who is also mid-swing should read as fighting, not as
    /// fumbling with a weapon.
    #[test]
    fn a_swing_outranks_a_sheath_change() {
        assert_eq!(
            Motion::resolve(
                0.0, 0.0, false, false, None, Some(50), true, 4_000, Stance::OneHand,
                Some((50, RestKind::Hip)),
            ),
            Motion::Attacking(3_900, Stance::OneHand)
        );
    }

    /// The transition lapses once it has had time to finish, the same way a
    /// fall settles -- otherwise a unit that changed its sheath state once
    /// stays frozen mid-transition for the rest of the session.
    #[test]
    fn a_sheath_transition_lapses_once_it_has_had_time_to_finish() {
        assert_eq!(
            Motion::resolve(
                0.0, 0.0, false, false, None, None, true, 10_000, Stance::OneHand,
                Some((SHEATH_CEILING_MS + 1, RestKind::Hip)),
            ),
            Motion::Ready(Stance::OneHand),
            "a stale sheath change should have lapsed into the ordinary ready stance"
        );
    }

    /// **Only interrupts standing**, the same rule a swing follows: a
    /// character drawing a weapon while sprinting plays the run, not a
    /// whole-body cycle this engine cannot blend into it.
    #[test]
    fn a_sheath_change_does_not_interrupt_movement() {
        assert_eq!(
            Motion::resolve(
                7.0, 0.0, false, false, None, None, false, 4_000, Stance::OneHand,
                Some((50, RestKind::Hip)),
            ),
            Motion::Run
        );
    }

    /// A corpse does not walk, however fast it was going a moment ago.
    ///
    /// The speed attached to a creature killed mid-charge is stale but not
    /// zero -- nothing zeroes it, because dying is not a movement packet. So
    /// death has to outrank it explicitly, and this is the case that made the
    /// original `from_speed` insufficient rather than merely incomplete.
    #[test]
    fn death_outranks_a_stale_speed() {
        assert_eq!(
            Motion::resolve(7.0, 0.0, false, true, None, None, false, 10_000, Stance::Unarmed, None),
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
            Motion::resolve(0.0, 0.0, false, true, Some(200), None, false, 10_000, Stance::Unarmed, None),
            Motion::Dying(9_800)
        );
        assert_eq!(Motion::resolve(0.0, 0.0, false, true, None, None, false, 10_000, Stance::Unarmed, None), Motion::Dead);
    }

    /// And it stops falling eventually, rather than holding a one-shot bucket
    /// for the life of the corpse.
    #[test]
    fn a_fall_settles_once_it_has_had_time_to_finish() {
        assert_eq!(
            Motion::resolve(0.0, 0.0, false, true, Some(ONE_SHOT_CEILING_MS + 1), None, false, 10_000, Stance::Unarmed, None),
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
        let first = Motion::resolve(0.0, 0.0, false, true, Some(100), None, false, 5_000, Stance::Unarmed, None);
        // A second later: the death is a second older and the clock a second
        // further on, which is the same death.
        let later = Motion::resolve(0.0, 0.0, false, true, Some(1_100), None, false, 6_000, Stance::Unarmed, None);
        assert_eq!(first, later, "the bucket moved with the clock");

        // Two deaths a second apart must *not* share, or one pops to the
        // other's frame.
        let other = Motion::resolve(0.0, 0.0, false, true, Some(100), None, false, 6_000, Stance::Unarmed, None);
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
        let reference = Motion::resolve(0.0, 0.0, false, false, None, Some(40), true, 8_000, Stance::Unarmed, None);
        for frame in 0..8u32 {
            let drift = frame * 3;
            let seen = Motion::resolve(0.0, 0.0, false,
                false,
                None,
                Some(40 + frame * 16),
                true,
                8_000 + frame * 16 + drift,
                Stance::Unarmed,
                None,
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
            Motion::resolve(0.0, 0.0, false, false, None, Some(ATTACK_CEILING_MS + 1), true, 9_000, Stance::Unarmed, None),
            Motion::Ready(Stance::Unarmed)
        );
    }

    /// A swing interrupts standing and not running: a creature chasing you
    /// swings as it runs, and the swing played over the run reads as a
    /// stumble.
    #[test]
    fn a_swing_interrupts_standing_but_not_running() {
        assert_eq!(
            Motion::resolve(0.0, 0.0, false, false, None, Some(50), true, 4_000, Stance::Unarmed, None),
            Motion::Attacking(3_900, Stance::Unarmed)
        );
        assert_eq!(Motion::resolve(7.0, 0.0, false, false, None, Some(50), true, 4_000, Stance::Unarmed, None), Motion::Run);
        // And an old swing has stopped mattering.
        assert_eq!(
            Motion::resolve(0.0, 0.0, false, false, None, Some(ONE_SHOT_CEILING_MS + 1), false, 4_000, Stance::Unarmed, None),
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
            Motion::resolve(0.0, 0.0, false, false, None, None, true, 4_000, Stance::Unarmed, None),
            Motion::Ready(Stance::Unarmed)
        );
        // Out of the fight, it relaxes.
        assert_eq!(
            Motion::resolve(0.0, 0.0, false, false, None, None, false, 4_000, Stance::Unarmed, None),
            Motion::Stand
        );
        // A swing still beats the guard, and running still beats both.
        assert_eq!(
            Motion::resolve(0.0, 0.0, false, false, None, Some(50), true, 4_000, Stance::Unarmed, None),
            Motion::Attacking(3_900, Stance::Unarmed)
        );
        assert_eq!(Motion::resolve(7.0, 0.0, false, false, None, None, true, 4_000, Stance::Unarmed, None), Motion::Run);
        // And a corpse is not "in a fight" whatever the map still says.
        assert_eq!(Motion::resolve(0.0, 0.0, false, true, None, None, true, 4_000, Stance::Unarmed, None), Motion::Dead);
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
        assert_eq!(Motion::Attacking(99, Stance::Unarmed).started_at(), Some(99));
        for looping in [Motion::Stand, Motion::Walk, Motion::Run, Motion::Ready(Stance::Unarmed), Motion::Dead] {
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
        for motion in [Motion::Attacking(0, Stance::Unarmed), Motion::Ready(Stance::Unarmed)] {
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
            Motion::Attacking(0, Stance::Unarmed),
            Motion::Ready(Stance::Unarmed),
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
        assert_eq!(Motion::from_pace(0.0, 0.0), Motion::Stand);
        // 3.3.5a's own default ground speeds.
        assert_eq!(Motion::from_pace(2.5, 0.0), Motion::Walk, "the walk speed");
        assert_eq!(Motion::from_pace(4.5, 0.0), Motion::Walk, "backing up");
        assert_eq!(Motion::from_pace(7.0, 0.0), Motion::Run, "the run speed");
        // A creature crawling is still walking, not standing: standing is
        // reserved for no move in flight at all, so a slow patrol animates.
        assert_eq!(Motion::from_pace(0.2, 0.0), Motion::Walk);
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
