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

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::Instant;

use glam::{Mat4, Vec3};
use mpq::Chain;
use render::mesh::{BlendMode, BoneBuffer, Instance, InstanceBuffer, MeshRenderer, RenderState};
use render::{Gpu, TerrainRenderer, UploadedTexture};

use ::world::combat::Hand;

use crate::character::Stance;
use crate::model::Draw;
use crate::scene::{
    map_animation_sequence, map_animation_time, object_rotation, placement_position,
    placement_rotation,
};
use crate::terrain::LoadedTerrain;

/// The smallest radius a *streaming* world may run at.
///
/// **Radius zero is not a small world, it is a broken one**, and the
/// difference is worth stating because zero was the default. Only the tile
/// under the camera is queued, so the tile a character is about to walk into
/// is by construction not there yet: they cross the boundary, spend a moment
/// in the void, and the ground appears underneath them a frame or two later.
/// Kake described exactly that -- "the next chunk won't load until the player
/// crosses into that chunk so you enter the void for a second".
///
/// One is the minimum that can be correct: at 3x3 the eight neighbours are
/// admitted while the character is still on the middle tile, so the ground
/// they are walking onto has already arrived. It is a floor rather than a
/// default so that `--radius 0` cannot quietly reintroduce it; a caller
/// wanting a single tile wants the non-streaming scene, which is a different
/// path entirely.
const MIN_STREAM_RADIUS: i32 = 1;

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
    pub texture_animation: crate::model::TextureAnimation,
    pub doodads: Vec<Vec<crate::world_object::Doodad>>,
    /// Populated whenever the loader has them (every M2, not a WMO), even
    /// though only replicated entities currently animate. Doodads and
    /// buildings are always drawn in the bind pose regardless, so carrying
    /// this costs them nothing and keeps one loader path instead of two.
    pub bones: std::rc::Rc<Vec<m2::AnimatedBone>>,
    pub sequences: Vec<m2::Sequence>,
    /// Where things this model carries hang from. Empty for everything that
    /// carries nothing, which is nearly everything.
    pub attachments: Vec<m2::Attachment>,
    /// What this model burns, sprays and trails. Empty for a WMO, which has no
    /// emitters of its own -- a brazier inside a building is a doodad placed by
    /// it, and arrives here as its own model.
    pub particles: Vec<m2::ParticleEmitter>,
    pub ribbons: Vec<m2::RibbonEmitter>,
    /// When this model's feet hit the ground, one sorted list of
    /// milliseconds per sequence.
    ///
    /// Read from the model's own timed events -- see [`m2::event`], which
    /// carries the measurement that identified them. Empty for everything with
    /// no legs, which is most models, and empty *per sequence* for the cycles
    /// a creature does not walk in.
    pub footfalls: std::rc::Rc<Vec<Vec<u32>>>,
    /// The model's own extent, in its local space. Carried so a replicated
    /// entity can be clicked on: a click needs a volume to test a ray against,
    /// and the model already knows how big it is. `None` for a WMO, which is
    /// never a click target.
    pub bounds: Option<(Vec3, Vec3)>,
    pub render_bounds: Option<(Vec3, Vec3)>,
    /// Held because the bind groups reference their views -- and read
    /// directly by the emitters, whose sprites are bound against a different
    /// pipeline layout and so cannot reuse `binds`.
    pub textures: Vec<UploadedTexture>,
    /// Everything solid about this model, in its own space, ready to be
    /// transformed by each placement -- see the `collision` crate. Empty for
    /// the many models that are scenery: a tuft of grass has no collision mesh
    /// and the original lets you walk through it too.
    pub collision: Vec<[[f32; 3]; 3]>,
    /// What each collision triangle is made of, as a `TerrainType` row id,
    /// parallel to [`Self::collision`] and `u8::MAX` where nothing says.
    ///
    /// **Empty for an M2**, and that is the honest answer rather than a gap: a
    /// model's collision mesh carries no material at all, so a footbridge's
    /// planks and a boulder are the same silence. Only a WMO's materials name
    /// a surface. See `wmo::Material::ground_type`.
    pub collision_footing: Vec<u8>,
    pub collision_area: Vec<u32>,
    pub wmo_id: Option<u32>,
    pub group_bounds: Vec<(Vec3, Vec3)>,
    pub group_surface_ids: Vec<u32>,
}

/// One model and the transforms it takes on a single tile.
pub struct Group {
    pub model: Rc<CachedModel>,
    pub instances: InstanceBuffer,
    pub count: u32,
    pub bounds: Option<(Vec3, Vec3)>,
    /// Set only for a replicated-entity group: display id plus which cycle
    /// this bucket plays, used to look up its own animated bone buffer instead
    /// of drawing with the scene's shared bind pose. The bucket has to be part
    /// of the key, not just the display id -- a species with instances
    /// standing, walking and running at once needs three different poses live
    /// at once, and a display-id-only key would have the last of them
    /// overwrite the others' buffers every rebuild. `None` when the model has
    /// no matching sequence to play.
    pub animation: Option<(u32, Motion)>,
    /// Model path for a tile-owned M2 playing its ambient sequence.
    pub map_animation: Option<String>,
    /// Set only when this group *is* an item held by another group, in which
    /// case its instance transforms are recomputed every frame from the
    /// wielder's pose. See [`Held`].
    pub held: Option<Held>,
    /// Whether this group's draws are forced to blend, whatever their
    /// materials say -- which is what a per-instance tint with alpha under one
    /// needs to be visible at all.
    ///
    /// **A tint alone does nothing.** A character's body is an opaque material
    /// and an opaque pipeline ignores the alpha it is handed, so a stealthed
    /// rogue tinted to 45% draws at full strength and the whole feature looks
    /// unimplemented. The two have to travel together, which is why this is a
    /// property of the group rather than something the draw loop infers.
    ///
    /// Depth writing goes with it: translucent geometry that writes depth
    /// hides whatever is drawn behind it later in the frame, so a crouching
    /// rogue would punch its own silhouette out of the grass.
    pub translucent: bool,
    /// This group's transforms, on the CPU, and **only when the model has an
    /// emitter**.
    ///
    /// The GPU copy in `instances` cannot answer: a particle is born at the
    /// emitter's world position and reading a vertex buffer back per frame to
    /// find out where fifty torches are would be absurd. Kept empty for
    /// everything else, which is 16,415 of the 22,844 models in the archives,
    /// so the memory is a rounding error rather than a copy of the scene.
    pub emitting: Vec<Mat4>,
    /// A stable identity per entry in [`Group::emitting`].
    ///
    /// Entity groups are rebuilt **every frame** and their order changes
    /// whenever a creature spawns, dies or changes cycle -- so a particle
    /// system keyed on a position in the vector would jump between creatures
    /// and restart every plume several times a second. A guid does not move.
    /// Doodads use a hash of their tile, path and placement index, which is
    /// equally stable and survives the tile being drawn again.
    pub emitting_ids: Vec<u64>,
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

/// The transforms worth keeping on the CPU for a group.
///
/// Empty unless the model actually emits something, which is the whole point:
/// a forest of five hundred trees would otherwise carry five hundred matrices
/// nothing will ever read.
fn emitting_placements(model: &CachedModel, transforms: &[Mat4]) -> Vec<Mat4> {
    if model.particles.is_empty() && model.ribbons.is_empty() {
        return Vec::new();
    }
    transforms.to_vec()
}

/// A stable id for each of a doodad group's placements.
///
/// Not the index: two tiles routinely place the same model, and a bare index
/// would give the fifth torch on one tile the same identity as the fifth on
/// its neighbour -- one plume for two fires. The tile is in the hash for
/// exactly that reason.
fn doodad_ids(model: &CachedModel, tile: (i32, i32), path: &str, count: usize) -> Vec<u64> {
    if model.particles.is_empty() && model.ribbons.is_empty() {
        return Vec::new();
    }
    use std::hash::{Hash, Hasher};
    (0..count)
        .map(|i| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            tile.hash(&mut hasher);
            path.hash(&mut hasher);
            i.hash(&mut hasher);
            hasher.finish()
        })
        .collect()
}

fn entity_doodad_ids(
    model: &CachedModel,
    guids: &[u64],
    path: &str,
    doodad_index: usize,
) -> Vec<u64> {
    if model.particles.is_empty() && model.ribbons.is_empty() {
        return Vec::new();
    }
    use std::hash::{Hash, Hasher};
    guids
        .iter()
        .map(|guid| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            guid.hash(&mut hasher);
            path.hash(&mut hasher);
            doodad_index.hash(&mut hasher);
            hasher.finish()
        })
        .collect()
}

/// A bucket's posed skeleton this frame, with the cycle it was posed from.
///
/// The sequence and the time are carried alongside the matrices because an
/// emitter needs *both*: the bones say where it is and the sequence says how
/// fast it should be emitting. Deriving the second separately would be two
/// answers to one question, and a flame running on a different cycle from the
/// arm it hangs off is the kind of disagreement nothing reports.
pub struct FramePose {
    pub bones: Vec<Mat4>,
    pub sequence: usize,
    pub time_ms: u32,
}

struct MapAnimation {
    model: Rc<CachedModel>,
    bones: BoneBuffer,
    sequence: usize,
}

/// What is lying over a point on the ground.
///
/// Carries the *category* rather than merely the type id, because the id alone
/// cannot be acted on: `LiquidType` row 181 is called `Orange Slime` and is
/// categorised as water, so anything reading the name or guessing from the id
/// would burn a player in a harmless pond. See `dbc::schema::LiquidCategory`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Liquid {
    /// Height of the surface, in the same world coordinates as the ground.
    pub surface: f32,
    /// Row of `LiquidType.dbc`.
    pub liquid_type: u16,
    pub category: dbc::schema::LiquidCategory,
}

pub struct Tile {
    pub terrain: LoadedTerrain,
    pub groups: Vec<Group>,
    pub wmos: Vec<WmoInstance>,
    /// Everything solid on this tile, in world space.
    ///
    /// Built with the tile and evicted with it. Rebuilding costs nothing
    /// beside reading the tile off disk, and a set that outlived its tile
    /// would be invisible walls where a building used to be.
    solid: collision::World,
    /// The world-space box [`Tile::solid`] actually occupies, or `None` when
    /// the tile holds nothing solid at all.
    ///
    /// **A tile's collision is not confined to the tile**, and that is the
    /// whole reason this exists. A world object is filed under the single
    /// tile containing its *origin* -- deliberately, so it is neither drawn
    /// twice nor left behind when a neighbour is evicted -- and a building
    /// bigger than a tile therefore spills its geometry into neighbours that
    /// know nothing about it. Stormwind is 1,058 by 1,060 units against a
    /// 533-unit tile: **it spans three tiles by three and every triangle of
    /// it is filed under one.**
    ///
    /// So a collision query cannot choose which tiles to ask by looking at
    /// where the *character* is. It has to ask every resident tile whose
    /// geometry could reach the point, and this box is what makes that cheap.
    solid_bounds: Option<(Vec3, Vec3)>,
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
    /// The liquid sheets lying over this chunk, kept for the same reason the
    /// heights are: the GPU copy cannot be asked how deep the river is under
    /// one character.
    ///
    /// A chunk can carry several -- a pool in a cave beneath a river needs two
    /// surfaces at one place -- so [`TileHeights::liquid_at`] takes the
    /// highest one the position falls inside rather than the first.
    liquid: Vec<adt::LiquidInstance>,
    /// Which texture layer a foot lands on, over a coarse grid -- see
    /// [`adt::footing`].
    ///
    /// Kept for the same reason the heights are, and it is the same argument
    /// one step further on: the alpha maps that decide this are uploaded to
    /// the GPU and dropped, and the GPU cannot be asked what a character is
    /// standing on. `u8::MAX` where the chunk names no layers.
    footing: Vec<u8>,
    /// Each layer's `GroundEffectTexture` id, which is what says what the
    /// surface is made of. Zero where a layer names none, and that is a real
    /// answer: 3,002 of Azeroth's 390,011 layers do not.
    effects: Vec<u32>,
}

impl TileHeights {
    fn new(chunks: &[adt::Chunk], liquid: &adt::TileLiquid) -> Self {
        // Both axes run inwards, so the origin corner is the largest of each.
        let origin = chunks.iter().fold((f32::MIN, f32::MIN), |(x, y), chunk| {
            (x.max(chunk.position[0]), y.max(chunk.position[1]))
        });

        let mut grid = Vec::new();
        grid.resize_with(adt::CHUNK_COUNT, || None);
        // Enumerated, because `TileLiquid` is indexed by a chunk's position in
        // the *file* while this grid is indexed by where the chunk sits in the
        // world. The two orderings agree in practice and are not the same
        // statement, which is the distinction the comment on `chunks` below
        // was written for.
        for (file_index, chunk) in chunks.iter().enumerate() {
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
                liquid: liquid.chunk(file_index).to_vec(),
                footing: adt::footing::footing_grid(chunk),
                effects: chunk.layers.iter().map(|l| l.effect_id).collect(),
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

    /// What the ground is made of at a position, as a `GroundEffectTexture`
    /// id.
    ///
    /// `None` where the tile is not resident, the chunk names no layers, or
    /// the winning layer names no ground effect -- three different reasons
    /// that all mean the same thing to a caller, which is that this client
    /// does not know and must not invent a material.
    ///
    /// Shares `height_at`'s chunk indexing deliberately, for the reason
    /// `area_at` does.
    fn footing_at(&self, x: f32, y: f32) -> Option<u32> {
        let side = adt::CHUNKS_PER_TILE as i64;
        let cx = (((self.origin.0 - x) / adt::CHUNK_SIZE).floor() as i64).clamp(0, side - 1);
        let cy = (((self.origin.1 - y) / adt::CHUNK_SIZE).floor() as i64).clamp(0, side - 1);
        let chunk = self.chunks.get((cy * side + cx) as usize)?.as_ref()?;
        let (row, col) = adt::footing::footing_cell(chunk.position, x, y)?;
        let layer = chunk.footing.get(row * adt::footing::FOOTING_GRID + col)?;
        let effect = chunk.effects.get(*layer as usize)?;
        (*effect != 0).then_some(*effect)
    }

    /// The liquid surface at a position: how high it is and what it is.
    ///
    /// **The highest sheet wins**, not the first. A chunk can carry more than
    /// one -- a cave pool under a river is exactly that -- and taking whichever
    /// came first in the file would put a character swimming in the lower of
    /// two surfaces while standing in the upper one.
    ///
    /// `None` means no liquid here, which is different from liquid at some
    /// height below the character: a river the player is standing beside still
    /// answers with its surface, and it is the *caller* that decides whether
    /// being above it means anything.
    fn liquid_at(&self, x: f32, y: f32) -> Option<(f32, u16)> {
        let side = adt::CHUNKS_PER_TILE as i64;
        let cx = (((self.origin.0 - x) / adt::CHUNK_SIZE).floor() as i64).clamp(0, side - 1);
        let cy = (((self.origin.1 - y) / adt::CHUNK_SIZE).floor() as i64).clamp(0, side - 1);
        let chunk = self.chunks.get((cy * side + cx) as usize)?.as_ref()?;
        chunk
            .liquid
            .iter()
            .filter_map(|sheet| {
                sheet
                    .height_at(chunk.position, x, y)
                    .map(|h| (h, sheet.liquid_type))
            })
            .max_by(|a, b| a.0.total_cmp(&b.0))
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
    /// The server's own id for this object.
    ///
    /// Carried only so an emitter can be told which creature it belongs to
    /// between frames -- everything else here is drawn from a bucket and does
    /// not care which instance is which. See [`Group::emitting_ids`].
    pub guid: u64,
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
    /// In liquid deep enough to swim in.
    ///
    /// True for the viewer's own character, decided locally against the `MH2O`
    /// sheets -- and **true for a replicated player too**, because
    /// `movement_flags::SWIMMING` travels with their relayed movement, so
    /// somebody else crossing a river is drawn swimming without this client
    /// having to sample the water under them. False for creatures: a
    /// `SMSG_MONSTER_MOVE` carries a path and no flags.
    pub swimming: bool,
    /// Whether this unit has no health left, so it should be drawn down rather
    /// than standing. Outranks `speed`: a creature killed mid-charge still has
    /// the charge's speed attached to it.
    pub dead: bool,
    /// How long ago it was seen to *die*, when this client watched it happen.
    /// `None` for a corpse that was already lying there when it came into
    /// view -- which must start settled rather than topple over again.
    pub died_ms_ago: Option<u32>,
    /// How long ago it last swung at something, and with which hand -- a
    /// dual-wielder's two weapons swing on their own timers and play
    /// different cycles.
    pub swung_ms_ago: Option<(u32, Hand)>,
    /// What this unit is doing about a spell, already resolved to the
    /// animation chain it should play. `None` for the overwhelming majority
    /// of units at any instant, and for every spell whose `SpellVisual`
    /// names no animation.
    pub spell: Option<SpellPose>,
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
    /// Whether this unit is stealthed, and so drawn faded and crouched.
    ///
    /// Takes part in the *bucket* key and deliberately **not** in the model
    /// cache key. The distinction matters here more than anywhere else it has
    /// come up: two rogues in identical gear, one stealthed, need separate
    /// buckets because they play different cycles and blend differently -- but
    /// they are the same mesh, the same skin and the same geosets, and folding
    /// stealth into the cache key would compose a second 512x512 character
    /// texture and re-read the model the instant somebody crouched. See
    /// `stealth_key`.
    pub stealthed: bool,
}

pub struct World {
    map: String,
    wdt: adt::Wdt,
    radius: i32,
    max_doodads: usize,
    wmo_areas: crate::world_object::WmoAreas,
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
    /// The archive bytes and skeletons behind those three caches, keyed by
    /// file rather than by display id.
    ///
    /// Sits beside them rather than inside any one of them because it is what
    /// they have in common: every humanoid NPC in the game is one `.m2` and
    /// forty-seven `.anim` files, and the caches above are keyed by costume.
    /// See [`crate::model::Sources`] for the measurement.
    sources: crate::model::Sources,
    /// Surface art for every liquid type seen so far, keyed by type id.
    ///
    /// Owned by the world rather than by a tile because it is shared across
    /// all of them: three liquid types cover the whole of Azeroth, and a
    /// per-tile cache would reload thirty frames of `lake_a` for each of the
    /// 529 tiles that carry water. It also outlives eviction on purpose --
    /// walking out of a river's tile and back in must not re-read its art.
    liquid_types: crate::liquid::LiquidTypes,
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
    map_animations: HashMap<String, MapAnimation>,
    map_frame_poses: RefCell<HashMap<String, FramePose>>,
    /// Animated bone buffers for replicated-entity groups, keyed by
    /// `Group::animation` and reused across rebuilds rather than reallocated:
    /// `update_bones` rewrites a buffer's contents in place, so the GPU
    /// buffer and its bind group only need to be created once per
    /// (display id, moving) pair, not once per rebuild tick.
    entity_bones: HashMap<(u32, Motion), BoneBuffer>,
    /// `(display id, motion)` buckets that were being posed as of the
    /// previous [`Self::update_animations`] call.
    ///
    /// **Compared against, never mutated mid-frame.** The first version of
    /// this feature kept one "current motion" value per display id and
    /// overwrote it as each bucket was processed -- so with two buckets of
    /// the same species live at once (some wolves standing, some walking,
    /// which is the ordinary case, not an edge one), whichever bucket was
    /// processed second every frame saw the first one's write and read it
    /// as *its own* transition, forever. That is what "wolves twitch and
    /// slide, NPCs slide" was: every standing wolf blending toward Walk and
    /// every walking one blending toward Stand, every single frame, with
    /// the blend never allowed to finish because the next frame reset it
    /// again. Comparing against a frozen snapshot of the *previous* frame
    /// means a bucket's own history cannot be disturbed by a sibling bucket
    /// processed earlier or later in the same pass.
    active_motion_buckets: RefCell<std::collections::HashSet<(u32, Motion)>>,
    /// Where a currently-blending bucket started and what it blends from.
    /// Written once, the frame a bucket is first found absent from
    /// [`Self::active_motion_buckets`]; read every frame after, gated on
    /// elapsed time, until the blend window passes. Left in place after
    /// that rather than cleared early -- a stale entry is inert, since
    /// nothing re-reads it once its own bucket's elapsed time exceeds the
    /// window -- and pruned alongside `entity_bones` in `set_entities`.
    blending: RefCell<HashMap<(u32, Motion), (Motion, Instant)>>,
    /// The most recent motion touched for a given display id, across
    /// whichever of its buckets was processed most recently.
    ///
    /// Consulted **only** at the instant a bucket is found newly active, to
    /// pick what it blends from -- never compared frame-to-frame the way
    /// the old, buggy design compared a "current motion" value, which is
    /// what made same-frame write order matter. Which sibling bucket wrote
    /// this last within one frame can make a single newly-appearing
    /// bucket's blend source slightly wrong; it cannot make an
    /// already-settled bucket re-detect a transition, because settled
    /// buckets never consult this at all.
    last_motion_per_display: RefCell<HashMap<u32, Motion>>,
    /// This frame's posed skeletons, by animation bucket.
    ///
    /// Written by [`World::update_animations`], which already has them, and
    /// read by the emitters -- a flame on a creature's hand has to hang off
    /// the same matrix the hand was drawn with. Recomputing it would be the
    /// exact mistake `poses` was kept to avoid for held items: two derivations
    /// that agree until one of them is edited, and a fire that trails a frame
    /// behind the arm carrying it.
    frame_poses: RefCell<HashMap<(u32, Motion), FramePose>>,
    /// Which animation bucket each replicated entity is in, rebuilt with the
    /// entities themselves.
    ///
    /// The buckets are shared -- every wolf walking is drawn from one pose --
    /// so going the other way, from a guid to the cycle it is playing, needs a
    /// map rather than a search. Footsteps are the caller: the sound a
    /// character's feet make is per character, and the cycle they are timed
    /// from is per bucket.
    entity_buckets: RefCell<HashMap<u64, (u32, Motion)>>,

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

pub struct WmoInstance {
    pub path: String,
    pub model: Rc<CachedModel>,
    pub transform: Mat4,
}

fn world_bounds(model: &CachedModel, transforms: &[Mat4]) -> Option<(Vec3, Vec3)> {
    if model.wmo_id.is_none() {
        return None;
    }
    let (min, max) = model.render_bounds?;
    let corners = [
        Vec3::new(min.x, min.y, min.z),
        Vec3::new(max.x, min.y, min.z),
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(max.x, max.y, min.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(min.x, max.y, max.z),
        Vec3::new(max.x, max.y, max.z),
    ];
    transforms
        .iter()
        .flat_map(|transform| corners.iter().map(|corner| transform.transform_point3(*corner)))
        .fold(None, |bounds, point| {
            Some(match bounds {
                Some((min, max)) => (min.min(point), max.max(point)),
                None => (point, point),
            })
        })
}

#[derive(Clone, Copy)]
pub struct AreaContext {
    pub area: u32,
    pub zone_music: Option<u32>,
    pub ambience: Option<u32>,
}

pub struct WmoMinimap {
    pub path: String,
    pub position: Vec3,
    pub groups: Vec<(usize, Vec3, Vec3)>,
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
/// The arithmetic is [`adt::tile_at`]'s and is deliberately not repeated here:
/// the minimap's viewport asks the same question from a crate that cannot see
/// this module, and two copies of a grid inversion agree until one of them is
/// touched. This is the `Vec3` convenience over it.
pub fn tile_at(position: Vec3) -> (i32, i32) {
    adt::tile_at(position.x, position.y)
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
    /// `radius` is in tiles around the one the camera is on, and is raised to
    /// [`MIN_STREAM_RADIUS`] however low it is asked for -- see there.
    pub fn new(chain: &mut Chain, map: &str, radius: i32, max_doodads: usize) -> anyhow::Result<Self> {
        let wdt = adt::Wdt::parse(&chain.read(&adt::wdt_path(map))?)?;
        let wmo_areas = crate::world_object::WmoAreas::load(chain);
        Ok(Self {
            map: map.to_string(),
            wdt,
            radius: radius.max(MIN_STREAM_RADIUS),
            max_doodads,
            wmo_areas,
            cache: HashMap::new(),
            entity_cache: HashMap::new(),
            held_cache: HashMap::new(),
            sources: crate::model::Sources::default(),
            liquid_types: crate::liquid::LiquidTypes::default(),
            npc_looks: None,
            npc_looks_tried: false,
            game_objects: None,
            game_objects_tried: false,
            entities: Vec::new(),
            map_animations: HashMap::new(),
            map_frame_poses: RefCell::new(HashMap::new()),
            entity_bones: HashMap::new(),
            active_motion_buckets: RefCell::new(std::collections::HashSet::new()),
            blending: RefCell::new(HashMap::new()),
            last_motion_per_display: RefCell::new(HashMap::new()),
            frame_poses: RefCell::new(HashMap::new()),
            entity_buckets: RefCell::new(HashMap::new()),
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
        liquid_renderer: &render::LiquidRenderer,
        chain: &mut Chain,
        camera: Vec3,
    ) {
        let centre = tile_at(camera);

        // Evict first, so memory is released before anything new is admitted.
        //
        // **A tile is kept if it is near *or* if what it holds still reaches
        // the camera**, and the second half is not an optimisation -- without
        // it the collision fix is only two thirds of a fix. A world object is
        // filed under the tile containing its origin, and that origin can sit
        // at the *edge* of what the object covers: Stormwind is filed under
        // (30,48) and spans (29..31, 48..50), so its owner is a corner of its
        // own footprint. With a 3x3 residency, standing anywhere on the y=50
        // row evicts the one tile holding every triangle of the city, and the
        // character falls through exactly as before.
        //
        // Keeping it costs one tile's buffers while a player is inside a
        // building bigger than a tile, which is precisely when they are wanted.
        let limit = self.radius + EVICT_MARGIN;
        self.tiles.retain(|tile, loaded| {
            let near =
                (tile.0 - centre.0).abs() <= limit && (tile.1 - centre.1).abs() <= limit;
            near || solid_reaches(loaded.solid_bounds, camera, camera)
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
            match self.load_tile(gpu, meshes, terrain_renderer, liquid_renderer, chain, tile) {
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
        liquid_renderer: &render::LiquidRenderer,
        chain: &mut Chain,
        tile: (i32, i32),
    ) -> anyhow::Result<Tile> {
        let (x, y) = (tile.0 as usize, tile.1 as usize);
        let terrain = crate::terrain::load(
            gpu,
            terrain_renderer,
            liquid_renderer,
            &mut self.liquid_types,
            chain,
            &self.map,
            x,
            y,
        )?;
        let parsed = adt::Adt::parse(
            &chain.read(&adt::tile_path(&self.map, x, y))?,
            self.wdt.big_alpha(),
        )?;

        // Own only the placements whose position falls on this tile, so a
        // border-straddling object belongs to exactly one owner and is neither
        // drawn twice nor left behind when a neighbour is evicted.
        let mut groups: HashMap<String, Vec<Mat4>> = HashMap::new();
        let mut wmo_placements = Vec::new();
        let mut budget = self.max_doodads;
        for placement in &parsed.objects {
            let position = placement_position(placement.position);
            if tile_at(position) != tile {
                continue;
            }
            let path = placement.path.to_string();
            let transform = Mat4::from_scale_rotation_translation(
                Vec3::ONE,
                // A WMO, not an M2: a different quarter of a turn.
                object_rotation(placement.rotation),
                position,
            );
            groups.entry(path.clone()).or_default().push(transform);
            wmo_placements.push((path, placement.doodad_set as usize, transform));
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

        for (path, set, parent) in wmo_placements {
            let Some(model) = self.model(gpu, meshes, chain, &path) else {
                continue;
            };
            let Some(doodads) = model.doodads.get(set) else {
                continue;
            };
            for doodad in doodads {
                groups.entry(doodad.path.clone()).or_default().push(parent * doodad.transform);
            }
        }

        let mut built = Vec::new();
        let mut wmos = Vec::new();
        let mut solid = collision::World::new();
        // Grown as triangles are added, so it costs one comparison per vertex
        // rather than a second pass over the whole set.
        let mut solid_bounds: Option<(Vec3, Vec3)> = None;
        for (path, transforms) in groups {
            let Some(model) = self.model(gpu, meshes, chain, &path) else {
                continue;
            };
            if model.wmo_id.is_some() {
                wmos.extend(transforms.iter().map(|transform| WmoInstance {
                    path: path.clone(),
                    model: Rc::clone(&model),
                    transform: *transform,
                }));
            }
            // Placed into world space here rather than kept in model space and
            // transformed per query: a building is placed once and asked about
            // sixty times a second, and the grid can only index what has a
            // world position.
            for transform in &transforms {
                for (index, triangle) in model.collision.iter().enumerate() {
                    let p = |v: [f32; 3]| transform.transform_point3(Vec3::from(v));
                    // The surface travels with the triangle rather than being
                    // looked up again later: by the time a character is
                    // standing on this, the model it came from is one of
                    // hundreds and the only thing identifying the triangle is
                    // the triangle. An M2's list is empty and every one of its
                    // triangles is untagged, which is what `add` already does.
                    let (a, b, c) = (p(triangle[0]), p(triangle[1]), p(triangle[2]));
                    solid_bounds = Some(match solid_bounds {
                        Some((lo, hi)) => (
                            lo.min(a).min(b).min(c),
                            hi.max(a).max(b).max(c),
                        ),
                        None => (a.min(b).min(c), a.max(b).max(c)),
                    });
                    solid.add_tagged_with_id(
                        collision::Triangle::new(a, b, c),
                        model
                            .collision_footing
                            .get(index)
                            .copied()
                            .unwrap_or(collision::UNTAGGED),
                        model.collision_area.get(index).copied().unwrap_or(0),
                    );
                }
            }
            meshes.prepare(gpu, model.draws.iter().map(|d| d.state));
            let map_animation = self.ensure_map_animation(gpu, meshes, &path, &model);
            let raw: Vec<Instance> = transforms
                .iter()
                .map(|t| Instance::from_cols_array_2d(t.to_cols_array_2d()))
                .collect();
            let bounds = world_bounds(&model, &transforms);
            built.push(Group {
                emitting: emitting_placements(&model, &transforms),
                emitting_ids: doodad_ids(&model, tile, &path, transforms.len()),
                model,
                instances: InstanceBuffer::upload(gpu, &raw),
                count: raw.len() as u32,
                bounds,
                animation: None,
                map_animation,
                held: None,
                // Scenery. Nothing about a tile's doodads or buildings is
                // ever tinted, so their materials answer for themselves.
                translucent: false,
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
            wmos,
            heights: TileHeights::new(&parsed.chunks, &parsed.liquid),
            solid_bounds,
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

    /// The liquid surface over a position, and what kind of liquid it is.
    ///
    /// `None` for dry ground *and* for a tile that has not streamed in yet,
    /// and the caller wants the same thing for both: leave the character
    /// walking. Guessing that an unloaded tile is dry is the safe direction --
    /// a character who walks on the surface of a river for one frame while its
    /// tile loads is a smaller wrong than one who starts swimming in a field.
    ///
    /// Only terrain liquid. A fountain inside a building is WMO liquid, which
    /// this cannot answer for -- the same boundary [`World::height_at`] draws
    /// against building floors.
    pub fn liquid_at(&self, x: f32, y: f32) -> Option<Liquid> {
        let tile = tile_at(Vec3::new(x, y, 0.0));
        let (surface, liquid_type) = self.tiles.get(&tile)?.heights.liquid_at(x, y)?;
        Some(Liquid {
            surface,
            liquid_type,
            category: self.liquid_types.category(liquid_type),
        })
    }

    /// The surface art for the liquids on this world's tiles.
    ///
    /// **Exposed so the draw cannot use the wrong cache.** A tile's liquid
    /// geometry names its type by id and nothing else; the frames that id
    /// resolves to live here, because this is the cache `load_tile` built them
    /// into. The renderer keeps a second one for the offline scenes, and
    /// drawing a streaming tile with *that* one is exactly the bug this
    /// accessor exists to prevent -- every sheet resolved to no art, every
    /// draw was skipped, and the water was built, uploaded and never
    /// submitted. Whoever holds the geometry holds the cache for it.
    pub fn liquid_types(&self) -> &crate::liquid::LiquidTypes {
        &self.liquid_types
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

    pub fn area_at_position(&self, at: Vec3, step: f32) -> Option<u32> {
        self.area_context_at_position(at, step).map(|context| context.area)
    }

    pub fn area_context_at_position(&self, at: Vec3, step: f32) -> Option<AreaContext> {
        if let Some((_, _, surface)) = self.floor_under_surface(at, step) {
            if let Some(area) = surface.and_then(|id| self.wmo_areas.by_id(id)) {
                return Some(AreaContext {
                    area: area.area_table_id,
                    zone_music: area.zone_music,
                    ambience: area.ambience_id,
                });
            }
        }
        self.area_at(at.x, at.y).map(|area| AreaContext {
            area,
            zone_music: None,
            ambience: None,
        })
    }

    pub fn wmo_minimap_at_position(&self, at: Vec3, step: f32) -> Option<WmoMinimap> {
        let surface_id = self.floor_under_surface(at, step)?.2?;
        let area = self.wmo_areas.by_id(surface_id)?;
        for tile in self.tiles_touching(at, at) {
            for instance in &tile.wmos {
                if instance.model.wmo_id != Some(area.wmo_id) {
                    continue;
                }
                let local = instance.transform.inverse().transform_point3(at);
                for (group_index, group_surface) in instance.model.group_surface_ids.iter().enumerate() {
                    if *group_surface != surface_id {
                        continue;
                    }
                    let Some((min, max)) = instance.model.group_bounds.get(group_index).copied() else {
                        continue;
                    };
                    if (min.x..=max.x).contains(&local.x)
                        && (min.y..=max.y).contains(&local.y)
                        && (min.z..=max.z).contains(&local.z)
                    {
                        let groups = instance
                            .model
                            .group_bounds
                            .iter()
                            .enumerate()
                            .map(|(index, (min, max))| (index, *min, *max))
                            .collect();
                        return Some(WmoMinimap {
                            path: instance.path.clone(),
                            position: local,
                            groups,
                        });
                    }
                }
            }
        }
        None
    }

    /// When this entity's feet are due to land in the cycle it is playing right
    /// now, with where in that cycle it currently is.
    ///
    /// Returns `(sequence, footfall times, time into the cycle, cycle length)`,
    /// the last three in milliseconds. The sequence is handed back because a
    /// caller comparing phases has to tell a cycle that *wrapped* from one
    /// that was *replaced*, and those look identical from the clock alone. `None` for an entity that is not drawn, has no cycle, or
    /// whose model carries no footfall events -- which is most models, since
    /// only 762 of 22,844 have feet the format describes.
    ///
    /// **Read from this frame's pose rather than recomputed.** The pose is
    /// what the character was drawn at, and a footstep timed from a second
    /// clock would drift against the legs it is meant to belong to -- the same
    /// reason a held item is placed from the wielder's own posed hand.
    pub fn footfalls_of(&self, guid: u64) -> Option<(usize, Vec<u32>, u32, u32)> {
        let bucket = self.entity_buckets.borrow().get(&guid).copied()?;
        let poses = self.frame_poses.borrow();
        let frame = poses.get(&bucket)?;
        let model = self
            .entities
            .iter()
            .find(|group| group.animation == Some(bucket))
            .map(|group| &group.model)?;
        let times = model.footfalls.get(frame.sequence)?;
        if times.is_empty() {
            return None;
        }
        let duration = model.sequences.get(frame.sequence)?.duration_ms.max(1);
        Some((frame.sequence, times.clone(), frame.time_ms, duration))
    }

    /// What the ground is made of at a position, as a `GroundEffectTexture`
    /// id. `None` while the tile is streaming, or where the ground says
    /// nothing about its surface.
    pub fn footing_at(&self, x: f32, y: f32) -> Option<u32> {
        let tile = tile_at(Vec3::new(x, y, 0.0));
        self.tiles.get(&tile)?.heights.footing_at(x, y)
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
    /// How far along `from` -> `to` the first solid surface is, as a fraction
    /// of the way, or `None` for a clear line.
    ///
    /// For the camera: a wall between the character and where the eye wants to
    /// sit is a wall the eye has to stop at, or the view ends up outside the
    /// building looking through it.
    ///
    /// **The nearest hit across every tile the line touches**, not the first
    /// tile's answer -- a segment near a tile edge crosses two, and taking
    /// whichever came first in the map would let the camera through a wall
    /// depending on which way the character was facing.
    ///
    /// The surface normal travels with the winning hit, the same reasoning
    /// [`Self::floor_under_footing`] already carries a tag with its winning
    /// height: a second, separate lookup for "which way does the *nearest*
    /// triangle face" could disagree with this one about which triangle that
    /// was. It is what lets the camera tell a wall from a low ceiling -- see
    /// `collision::World::first_hit_with_normal`.
    pub fn first_obstruction(&self, from: Vec3, to: Vec3) -> Option<(f32, Vec3)> {
        let mut nearest: Option<(f32, Vec3)> = None;
        for tile in self.tiles_touching(from, to) {
            if tile.solid.is_empty() {
                continue;
            }
            if let Some((t, normal)) = tile.solid.first_hit_with_normal(from, to) {
                nearest = Some(match nearest {
                    Some(best) if best.0 <= t => best,
                    _ => (t, normal),
                });
            }
        }
        nearest
    }

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

    /// Whether tiles are still being admitted.
    ///
    /// **Read by the movement code before it lowers the character**, which is
    /// not a decoration. Tiles arrive a few per frame, and a building's
    /// collision is filed under the one tile containing its origin -- so
    /// there is a window at login where the character's *own* tile is
    /// resident, the terrain height answers, and the tile holding the floor
    /// underfoot has not arrived. Snapping to the terrain in that window puts
    /// a character standing on Stormwind's gryphon platform at the terrain
    /// height seventy units below the city, and the drop is permanent: a
    /// floor search only looks at or below where it starts, so the floor that
    /// streams in a frame later is above them and never found again.
    pub fn still_streaming(&self) -> bool {
        !self.pending.is_empty()
    }

    /// What is holding a character up under a point: how high it is, and what
    /// it is made of as a `TerrainType` row id.
    ///
    /// `None` overall for open ground, where the terrain height field is the
    /// answer. `None` for the *surface* where the floor came from an M2, or
    /// from a WMO material that names no terrain -- which is 91% of them,
    /// because most of a building is walls and roof.
    /// **Asks every resident tile whose geometry could reach the point**, and
    /// keeps the highest floor.
    ///
    /// This used to look up the single tile the character was standing on,
    /// which is the assumption that made all of Stormwind non-solid -- see
    /// [`World::tiles_touching`]. Highest wins for the same reason it does
    /// within one tile: a balcony over a courtyard is what holds you up, not
    /// the flagstones under it.
    ///
    /// The surface tag travels with the winning height rather than being
    /// looked up again, exactly as `floor_under_tagged` does inside a tile --
    /// two derivations of one fact agree until a tie breaks differently, and
    /// the frame they disagree on is a character standing on floorboards and
    /// hearing stone.
    pub fn floor_under_footing(&self, at: Vec3, step: f32) -> Option<(f32, Option<u8>)> {
        self.floor_under_surface(at, step)
            .map(|(z, footing, _)| (z, footing))
    }

    pub fn floor_under_surface(
        &self,
        at: Vec3,
        step: f32,
    ) -> Option<(f32, Option<u8>, Option<u32>)> {
        let mut best: Option<(f32, Option<u8>, Option<u32>)> = None;
        for tile in self.tiles_touching(at, at) {
            if tile.solid.is_empty() {
                continue;
            }
            if let Some((z, footing, area)) =
                tile.solid.floor_under_tagged_with_id(at.truncate(), at.z, step)
            {
                if best.is_none_or(|(b, _, _)| z > b) {
                    best = Some((z, footing, area));
                }
            }
        }
        best
    }

    /// Every resident tile a straight move between two points could touch.
    ///
    /// A short move is one or two tiles; listing them rather than assuming the
    /// start's is what makes a tile seam an implementation detail instead of a
    /// hole in the world.
    /// Every resident tile whose **collision geometry** could reach the box
    /// between two points.
    ///
    /// **Selected by what a tile holds, not by where the query is**, and the
    /// difference is the whole of `foss-wow` Stormwind bug. The old version
    /// walked the tile coordinates between `from` and `to` -- which is right
    /// only if a tile's collision stays inside the tile, and it does not: a
    /// world object is filed under the one tile containing its origin, so
    /// Stormwind's 1,058-by-1,060-unit shell is filed under a single 533-unit
    /// tile and physically covers nine. Standing over any of the other eight,
    /// the query asked tiles that hold nothing and the character fell through
    /// a city that was drawn perfectly around them.
    ///
    /// Every building in Elwynn is a fraction of a tile, which is why this
    /// was invisible for four milestones and surfaced the first time anyone
    /// walked into a capital.
    ///
    /// Iterating all resident tiles is deliberate rather than lazy: streaming
    /// keeps a handful, the test is an AABB overlap, and a cleverer index
    /// would be a second structure to keep in step with the first.
    fn tiles_touching(&self, from: Vec3, to: Vec3) -> impl Iterator<Item = &Tile> {
        let lo = from.min(to);
        let hi = from.max(to);
        self.tiles
            .values()
            .filter(move |tile| solid_reaches(tile.solid_bounds, lo, hi))
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

    fn ensure_map_animation(
        &mut self,
        gpu: &Gpu,
        meshes: &MeshRenderer,
        path: &str,
        model: &Rc<CachedModel>,
    ) -> Option<String> {
        let sequence = map_animation_sequence(&model.sequences)?;
        if !model.bones.iter().any(|bone| bone.is_animated())
            && !model.texture_animation.is_animated()
        {
            return None;
        }
        self.map_animations
            .entry(path.to_string())
            .or_insert_with(|| MapAnimation {
                model: Rc::clone(model),
                bones: meshes.create_bones(gpu, model.bones.len()),
                sequence,
            });
        Some(path.to_string())
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
            bones: std::rc::Rc<Vec<m2::AnimatedBone>>,
            sequences: Vec<m2::Sequence>,
            attachments: Vec<m2::Attachment>,
            particles: Vec<m2::ParticleEmitter>,
            ribbons: Vec<m2::RibbonEmitter>,
            footfalls: std::rc::Rc<Vec<Vec<u32>>>,
            bounds: Option<(Vec3, Vec3)>,
            collision: Vec<[[f32; 3]; 3]>,
            collision_footing: Vec<u8>,
            collision_area: Vec<u32>,
            wmo_id: Option<u32>,
            group_bounds: Vec<(Vec3, Vec3)>,
            group_surface_ids: Vec<u32>,
            render_bounds: Option<(Vec3, Vec3)>,
            doodads: Vec<Vec<crate::world_object::Doodad>>,
            texture_animation: crate::model::TextureAnimation,
        }

        let lower = path.to_lowercase();
        let built = if lower.ends_with(".wmo") {
            // No skeleton to speak of, so nothing to animate and nothing to
            // hang off it.
            crate::world_object::load_with_areas(gpu, chain, path, None, Some(&self.wmo_areas))
                .map(|w| {
                    let texture_animation = crate::model::TextureAnimation::empty(
                        gpu,
                        meshes,
                        w.draws.len(),
                    );
                    Built {
                        mesh: w.mesh,
                        draws: w.draws,
                        textures: w.textures,
                        texture_animation,
                        bones: Default::default(),
                        sequences: Vec::new(),
                        attachments: Vec::new(),
                        particles: Vec::new(),
                        ribbons: Vec::new(),
                        footfalls: Default::default(),
                        bounds: None,
                        collision: w.collision,
                        collision_footing: w.collision_footing,
                        collision_area: w.collision_area,
                        wmo_id: Some(w.wmo_id),
                        group_bounds: w.group_bounds,
                        group_surface_ids: w.group_surface_ids,
                        render_bounds: Some((w.min, w.max)),
                        doodads: w.doodads,
                    }
                })
                .ok()
        } else {
            crate::model::load(gpu, meshes, chain, path, variations, 0)
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
                        texture_animation: m.texture_animation,
                        bones: m.bones,
                        sequences: m.sequences,
                        attachments: m.attachments,
                        particles: m.particles,
                        ribbons: m.ribbons,
                        footfalls: m.footfalls,
                        bounds: Some((m.min, m.max)),
                        collision: m.collision,
                        // An M2 names no surface; see `CachedModel`.
                        collision_footing: Vec::new(),
                        collision_area: Vec::new(),
                        wmo_id: None,
                        group_bounds: Vec::new(),
                        group_surface_ids: Vec::new(),
                        render_bounds: Some((m.min, m.max)),
                        doodads: Vec::new(),
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
                texture_animation: b.texture_animation,
                doodads: b.doodads,
                bones: b.bones,
                sequences: b.sequences,
                attachments: b.attachments,
                particles: b.particles,
                ribbons: b.ribbons,
                footfalls: b.footfalls,
                bounds: b.bounds,
                textures: b.textures,
                collision: b.collision,
                collision_footing: b.collision_footing,
                collision_area: b.collision_area,
                wmo_id: b.wmo_id,
                group_bounds: b.group_bounds,
                group_surface_ids: b.group_surface_ids,
                render_bounds: b.render_bounds,
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
        // **Stealth is the fourth term and it is not part of `key`**, which is
        // the whole point of it being here instead. `key` is handed to
        // `entity_model` as the model cache's key, so anything folded into it
        // buys a second model load; two rogues in identical gear, one crouched,
        // are one mesh with one composed skin drawn twice. What they cannot
        // share is the *bucket*: they play different cycles and blend
        // differently. Four terms in the tuple, three in the cache key.
        let mut grouped: HashMap<(u32, Motion, u64, bool), Vec<Mat4>> = HashMap::new();
        // Parallel to `grouped` and pushed in lockstep with it, so entry `i`
        // of a bucket's transforms and entry `i` of its guids are the same
        // object. Two vectors rather than a vector of pairs because the
        // transforms are uploaded wholesale and the guids never are.
        let mut grouped_guids: HashMap<(u32, Motion, u64, bool), Vec<u64>> = HashMap::new();
        // The same association the other way round, kept because a caller
        // holding a guid cannot search a shared bucket for it.
        let mut bucket_of_guid: HashMap<u64, (u32, Motion)> = HashMap::new();
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
            let motion = Motion::resolve(
                placement.speed,
                placement.turning,
                placement.airborne,
                placement.swimming,
                placement.dead,
                placement.died_ms_ago,
                placement.swung_ms_ago,
                placement.fighting,
                now_ms,
                // Stowed weapons leave the hands free, so the stance only
                // applies while something is drawn.
                if placement.sheathed {
                    Stance::Unarmed
                } else {
                    placement.stance
                },
                sheathing,
                placement.spell,
            );
            let bucket = (
                placement.display_id,
                // Applied after rather than inside: see `Motion::crouched`.
                if placement.stealthed {
                    motion.crouched()
                } else {
                    motion
                },
                key,
                placement.stealthed,
            );
            grouped_guids
                .entry(bucket)
                .or_default()
                .push(placement.guid);
            bucket_of_guid.insert(placement.guid, (bucket.0, bucket.1));
            grouped
                .entry(bucket)
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
        for ((display_id, motion, look_key, stealthed), transforms) in grouped {
            // Taken rather than borrowed: `grouped` was consumed by the loop
            // and its parallel guids have exactly the same keys, so a missing
            // entry would be a bug rather than a case to handle.
            let guids = grouped_guids
                .remove(&(display_id, motion, look_key, stealthed))
                .unwrap_or_default();
            let look = looks.get(&look_key).cloned().flatten();
            let kind = kinds.get(&look_key).copied().unwrap_or(::world::ObjectType::Unit);
            let Some(model) =
                self.entity_model(gpu, meshes, chain, display_id, look_key, look.as_deref(), kind)
            else {
                undrawable += transforms.len();
                continue;
            };
            // Both the materials' own states and, for a stealthed bucket, the
            // blended overrides the draw loop will actually ask for. A
            // pipeline that was never prepared is a draw silently skipped, so
            // the override is declared here rather than discovered at submit
            // time -- the same reason `prepare` exists at all.
            meshes.prepare(gpu, model.draws.iter().map(|d| d.state));
            if stealthed {
                meshes.prepare(gpu, model.draws.iter().map(|d| translucent(d.state)));
            }
            let tint = if stealthed {
                STEALTH_FADE
            } else {
                Instance::OPAQUE
            };
            let raw: Vec<Instance> = transforms
                .iter()
                .map(|t| Instance::tinted(t.to_cols_array_2d(), tint))
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
                let bind_pose_transforms: Vec<Mat4> = transforms
                    .iter()
                    .map(|t| *t * Mat4::from_translation(Vec3::from(attachment.position)))
                    .collect();
                // Tinted with its wielder. A dagger drawn at full strength in
                // a hand that is 45% there is worse than either alone -- it
                // reads as a floating weapon rather than as a hidden rogue.
                let bind_pose: Vec<Instance> = bind_pose_transforms
                    .iter()
                    .map(|t| Instance::tinted(t.to_cols_array_2d(), tint))
                    .collect();
                if stealthed {
                    meshes.prepare(gpu, held_model.draws.iter().map(|d| translucent(d.state)));
                }
                let bounds = world_bounds(&held_model, &bind_pose_transforms);
                built.push(Group {
                    // A torch in a hand is the case this exists for, and its
                    // placements are rewritten every frame by
                    // `update_animations` along with the item's own transform.
                    emitting: emitting_placements(&held_model, &bind_pose_transforms),
                    // The wielder's guid, mixed so a torch in a hand and the
                    // hand's owner do not collide on one key. A held item is
                    // one per wielder, so the wielder identifies it.
                    emitting_ids: if held_model.particles.is_empty()
                        && held_model.ribbons.is_empty()
                    {
                        Vec::new()
                    } else {
                        guids.iter().map(|g| g ^ 0x4845_4c44).collect()
                    },
                    model: held_model,
                    instances: InstanceBuffer::upload(gpu, &bind_pose),
                    count: raw.len() as u32,
                    bounds,
                    animation: None,
                    map_animation: None,
                    translucent: stealthed,
                    held: Some(Held {
                        wielder: animation,
                        wielders: transforms.clone(),
                        bone: attachment.bone as usize,
                        offset: Vec3::from(attachment.position),
                    }),
                });
            }

            let child_doodads = if kind == ::world::ObjectType::GameObject {
                model.doodads.first().cloned().unwrap_or_default()
            } else {
                Vec::new()
            };
            for (doodad_index, doodad) in child_doodads.iter().enumerate() {
                let Some(doodad_model) = self.model(gpu, meshes, chain, &doodad.path) else {
                    continue;
                };
                let doodad_transforms: Vec<Mat4> = transforms
                    .iter()
                    .map(|transform| *transform * doodad.transform)
                    .collect();
                meshes.prepare(gpu, doodad_model.draws.iter().map(|d| d.state));
                let map_animation = self.ensure_map_animation(
                    gpu,
                    meshes,
                    &doodad.path,
                    &doodad_model,
                );
                let raw: Vec<Instance> = doodad_transforms
                    .iter()
                    .map(|transform| Instance::from_cols_array_2d(transform.to_cols_array_2d()))
                    .collect();
                let bounds = world_bounds(&doodad_model, &doodad_transforms);
                built.push(Group {
                    emitting: emitting_placements(&doodad_model, &doodad_transforms),
                    emitting_ids: entity_doodad_ids(
                        &doodad_model,
                        &guids,
                        &doodad.path,
                        doodad_index,
                    ),
                    model: doodad_model,
                    instances: InstanceBuffer::upload(gpu, &raw),
                    count: raw.len() as u32,
                    bounds,
                    animation: None,
                    map_animation,
                    held: None,
                    translucent: false,
                });
            }

            let bounds = world_bounds(&model, &transforms);
            built.push(Group {
                emitting: emitting_placements(&model, &transforms),
                emitting_ids: if model.particles.is_empty() && model.ribbons.is_empty() {
                    Vec::new()
                } else {
                    guids.clone()
                },
                model,
                instances: InstanceBuffer::upload(gpu, &raw),
                count: raw.len() as u32,
                bounds,
                animation,
                map_animation: None,
                held: None,
                translucent: stealthed,
            });
        }
        // Drop bone buffers for creatures that changed bucket or left view,
        // rather than growing this cache for the life of the session.
        self.entity_bones.retain(|key, _| wanted_bones.contains(key));
        // Same reasoning, for the blend bookkeeping -- otherwise every
        // one-shot bucket a session ever creates (`Motion::Dying`,
        // `Attacking`, `Sheathing` all carry a timestamp in the key) stays
        // in `blending` forever, since nothing else ever removes an entry.
        self.blending
            .borrow_mut()
            .retain(|key, _| wanted_bones.contains(key));
        {
            let wanted_displays: std::collections::HashSet<u32> =
                wanted_bones.iter().map(|(display, _)| *display).collect();
            self.last_motion_per_display
                .borrow_mut()
                .retain(|display, _| wanted_displays.contains(display));
        }

        // Rebuilt wholesale for the same reason `frame_poses` is: an entity
        // that left view this pass must stop having a bucket, or a footstep
        // would keep being timed from a cycle nothing is drawing.
        *self.entity_buckets.borrow_mut() = bucket_of_guid;
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
        let mut poses: HashMap<(u32, Motion), (Vec<Mat4>, usize, u32)> = HashMap::new();
        let now_ms = self.started.elapsed().as_millis() as u32;

        let active_map_animations: HashSet<&str> = self
            .tiles
            .values()
            .flat_map(|tile| tile.groups.iter())
            .chain(self.entities.iter())
            .filter_map(|group| group.map_animation.as_deref())
            .collect();
        let mut map_poses = HashMap::new();
        for path in active_map_animations {
            let Some(animation) = self.map_animations.get(path) else {
                continue;
            };
            let definition = animation.model.sequences[animation.sequence];
            let time_ms = map_animation_time(definition.duration_ms, definition.flags, now_ms);
            let pose = m2::Model::pose_bones_with_global_loops(
                &animation.model.bones,
                animation.sequence,
                time_ms,
                animation.model.texture_animation.global_sequences(),
            );
            let upload: Vec<[[f32; 4]; 4]> =
                pose.iter().map(|matrix| matrix.to_cols_array_2d()).collect();
            meshes.update_bones(gpu, &animation.bones, &upload);
            animation.model.texture_animation.update(
                gpu,
                meshes,
                animation.sequence,
                time_ms,
            );
            map_poses.insert(
                path.to_string(),
                FramePose {
                    bones: pose,
                    sequence: animation.sequence,
                    time_ms,
                },
            );
        }
        *self.map_frame_poses.borrow_mut() = map_poses;

        // Poses a single motion at its own clock -- exactly what this
        // function always computed, pulled out so a transition's outgoing
        // cycle can be posed the same way as its incoming one, below.
        let pose_for = |model: &CachedModel, motion: Motion| -> Option<(Vec<Mat4>, usize, u32)> {
            let sequence = sequence_for(model, motion)?;
            let duration = model.sequences[sequence].duration_ms.max(1);
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
            let played = model.sequences[sequence].id;
            let time_ms = if played == DEATH_ANIMATION_ID && motion == Motion::Dead {
                // Settled onto the last frame of the fall. No start time is
                // needed or wanted: this pose is the same however long ago the
                // unit died, which is why every settled corpse of a display
                // can share one bucket.
                duration - 1
            } else if plays_once(played)
                || (matches!(motion, Motion::CastRelease(..)) && !always_loops(played))
            {
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
            Some((
                m2::Model::pose_bones_with_global_loops(
                    &model.bones,
                    sequence,
                    time_ms,
                    model.texture_animation.global_sequences(),
                ),
                sequence,
                time_ms,
            ))
        };

        let now = Instant::now();
        let mut next_active: std::collections::HashSet<(u32, Motion)> =
            std::collections::HashSet::new();

        for group in &self.entities {
            let Some((display_id, motion)) = group.animation else {
                continue;
            };
            let (Some(bones), Some((posed, sequence, time_ms))) = (
                self.entity_bones.get(&(display_id, motion)),
                pose_for(&group.model, motion),
            ) else {
                continue;
            };
            let key = (display_id, motion);
            next_active.insert(key);

            // A bucket newly found active gets a blend source from whatever
            // this display's *other* buckets last recorded; one already
            // active from last frame keeps whatever `Self::blending` has on
            // file for it (or nothing, if it settled or never blended). Pure
            // and pulled out to `bucket_transition` precisely so the
            // ordering bug this exists to prevent (see
            // `Self::active_motion_buckets`'s doc comment) can be pinned by
            // a test that never opens a window.
            match bucket_transition(
                &self.active_motion_buckets.borrow(),
                &self.last_motion_per_display.borrow(),
                display_id,
                motion,
            ) {
                BucketTransition::New(Some(from)) => {
                    self.blending.borrow_mut().insert(key, (from, now));
                }
                BucketTransition::New(None) => {
                    self.blending.borrow_mut().remove(&key);
                }
                BucketTransition::Continuing => {}
            }
            self.last_motion_per_display
                .borrow_mut()
                .insert(display_id, motion);

            let blend_from = self.blending.borrow().get(&key).and_then(|(from, started)| {
                let elapsed = now.saturating_duration_since(*started).as_millis() as u32;
                (elapsed < TRANSITION_BLEND_MS).then_some((*from, elapsed))
            });

            let final_pose = match blend_from.and_then(|(from, elapsed)| {
                pose_for(&group.model, from)
                    .map(|(_, old_sequence, old_time_ms)| (old_sequence, old_time_ms, elapsed))
            }) {
                // Bone-local translation, rotation and scale are blended
                // before the parent chain is composed. Blending completed
                // model-space matrices moves every child independently and
                // changes limb lengths during the transition.
                Some((old_sequence, old_time_ms, elapsed)) => blend_poses_with_global_loops(
                    &group.model.bones,
                    old_sequence,
                    old_time_ms,
                    sequence,
                    time_ms,
                    elapsed as f32 / TRANSITION_BLEND_MS as f32,
                    group.model.texture_animation.global_sequences(),
                ),
                None => posed,
            };

            let pose: Vec<[[f32; 4]; 4]> =
                final_pose.iter().map(|m| m.to_cols_array_2d()).collect();
            meshes.update_bones(gpu, bones, &pose);
            poses.insert((display_id, motion), (final_pose, sequence, time_ms));
        }
        // Replaced wholesale, not merged: a bucket that stopped animating this
        // frame must stop having a pose, or an emitter would keep hanging off
        // the skeleton of a creature that has despawned.
        *self.frame_poses.borrow_mut() = poses
            .iter()
            .map(|(key, (bones, sequence, time_ms))| {
                (
                    *key,
                    FramePose {
                        bones: bones.clone(),
                        sequence: *sequence,
                        time_ms: *time_ms,
                    },
                )
            })
            .collect();
        // Replaces last frame's snapshot wholesale rather than being updated
        // bucket-by-bucket above: a bucket absent from `self.entities` this
        // pass (its creature died, despawned, or changed motion) has to
        // stop counting as active, or its *next* reappearance would be
        // missed as a transition.
        *self.active_motion_buckets.borrow_mut() = next_active;

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
                .and_then(|(pose, _, _)| pose.get(held.bone))
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

    /// Steps everything alight in the world and builds this frame's geometry.
    ///
    /// Runs **after** [`World::update_animations`], and the order is the whole
    /// point: a flame on a creature's hand hangs off the very matrix the hand
    /// was drawn with, and posing afterwards would leave every emitter a frame
    /// behind the skeleton carrying it. Same reasoning as held items, which
    /// are placed from the same poses one loop above.
    pub fn update_emitters(
        &self,
        gpu: &Gpu,
        renderer: &mut render::ParticleRenderer,
        emitters: &mut crate::emitters::Emitters,
        dt: f32,
    ) {
        let poses = self.frame_poses.borrow();
        let map_poses = self.map_frame_poses.borrow();
        let now_ms = self.started.elapsed().as_millis() as u32;

        let mut sources = Vec::new();
        for group in self.tiles().flat_map(|t| t.groups.iter()).chain(&self.entities) {
            if group.emitting.is_empty() {
                continue;
            }
            // A held item's placement is *not* what was stored at build time:
            // the item follows an animated hand, and `update_animations`
            // rewrote its instance buffer without touching the CPU copy. It is
            // recomputed here from the same pose rather than mirrored into
            // `emitting`, because two copies of a per-frame transform drift
            // and only one of them is the one being drawn.
            let (placements, pose) = match &group.held {
                Some(held) => {
                    let frame = held.wielder.and_then(|key| poses.get(&key));
                    let hand = frame
                        .and_then(|f| f.bones.get(held.bone))
                        .copied()
                        .unwrap_or(Mat4::IDENTITY);
                    (
                        std::borrow::Cow::<[Mat4]>::Owned(
                            held.wielders
                                .iter()
                                .map(|t| held_transform(*t, hand, held.offset))
                                .collect(),
                        ),
                        // The item's *own* skeleton is rigid and unposed; the
                        // movement all comes from the hand, which is already
                        // in the transform above.
                        None,
                    )
                }
                None => (
                    std::borrow::Cow::<[Mat4]>::Borrowed(&group.emitting),
                    group
                        .animation
                        .and_then(|key| poses.get(&key))
                        .or_else(|| {
                            group
                                .map_animation
                                .as_ref()
                                .and_then(|key| map_poses.get(key))
                        }),
                ),
            };

            // A doodad has no animation bucket and so no sequence of its own.
            // Sequence 0 on a running clock is the right answer rather than a
            // fallback: a torch ships exactly one, and its emitter tracks live
            // in it.
            let (sequence, time_ms) = match pose {
                Some(frame) => (frame.sequence, frame.time_ms),
                None => (
                    0,
                    group
                        .model
                        .sequences
                        .first()
                        .map(|s| now_ms % s.duration_ms.max(1))
                        .unwrap_or(0),
                ),
            };

            sources.push(crate::emitters::Source {
                particles: &group.model.particles,
                ribbons: &group.model.ribbons,
                textures: &group.model.textures,
                ids: std::borrow::Cow::Borrowed(&group.emitting_ids),
                placements,
                pose: pose.map(|f| f.bones.as_slice()),
                sequence,
                time_ms,
            });
        }
        emitters.update(gpu, renderer, sources.into_iter(), dt);
    }

    /// The animated bone buffer for a `Group::animation` key, if `set_entities`
    /// gave it one this rebuild.
    pub fn entity_bone_buffer(&self, key: (u32, Motion)) -> Option<&BoneBuffer> {
        self.entity_bones.get(&key)
    }

    pub fn map_bone_buffer(&self, key: &str) -> Option<&BoneBuffer> {
        self.map_animations.get(key).map(|animation| &animation.bones)
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
        //
        // Everything from here down is a *miss*, and a miss is synchronous on
        // the render thread the first time each new display comes into view.
        // That is the whole of the stutter this instrument exists to price.
        let began = Instant::now();
        let npc_look = look.is_none().then(|| self.npc_look(chain, display_id)).flatten();
        let looked_up = began.elapsed();
        let look = look.or(npc_look.as_ref());

        // Two statements rather than one chain: both halves want the cache
        // mutably, and the model path is owned by the time the second runs.
        let resolved = crate::model::creature(&mut self.sources, chain, display_id);
        let loaded = resolved.and_then(|(path, variations)| {
                crate::model::load_dressed_with(
                    gpu,
                    meshes,
                chain,
                &mut self.sources,
                &path,
                &variations,
                0,
                look,
            )
        });
        // Read out here rather than inside the closure below: the load holds
        // the only mutable borrow of the cache, and this is the first point
        // after it where anything may look at the cache at all.
        let (hits, misses) = self.sources.counts();

        let entry = loaded
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
                // One line per first-time load, naming the display id and
                // where the milliseconds went.
                //
                // **This is a measurement, not a symptom.** The placeholder
                // warning above coincides with every stutter, which is exactly
                // why it was mistaken for a cause twice: it is a marker that a
                // load just *finished*. A cost that nobody has timed is how
                // this project once spent an afternoon blaming a `Spell.dbc`
                // read that takes 185ms for a thirty-seven-second login. Kept
                // at info, and kept per-load rather than aggregated, because
                // the question a frozen frame asks is "which creature", and an
                // average cannot answer it.
                // Both cache numbers on every line, deliberately. A hit count
                // alone cannot separate "the shared cache is working" from
                // "nothing has been loaded twice yet", and the second is what
                // a regression here would look like.
                tracing::info!(
                    "display {display_id} loaded in {:?} \
                     (look {looked_up:?}, {}) [source cache {hits} hit / {misses} miss]",
                    began.elapsed(),
                    loaded.timings.summary(),
                );
                let binds = loaded
                    .textures
                    .iter()
                    .map(|t| meshes.material_bind_group(gpu, &t.view))
                    .collect();
                Rc::new(CachedModel {
                    mesh: loaded.mesh,
                    draws: loaded.draws,
                    binds,
                    texture_animation: loaded.texture_animation,
                    doodads: Vec::new(),
                    bones: loaded.bones,
                    sequences: loaded.sequences,
                    attachments: loaded.attachments,
                    particles: loaded.particles,
                    ribbons: loaded.ribbons,
                    footfalls: loaded.footfalls,
                    // This is the cache click-to-target reads from, so this is
                    // the one that has to carry the model's extent.
                    bounds: Some((loaded.min, loaded.max)),
                    textures: loaded.textures,
                    // A replicated entity's own body is not scenery: creatures
                    // and players are moved by the server, and colliding with
                    // them is a different feature from colliding with the
                    // world. Left empty rather than filled in unused.
                    collision: Vec::new(),
                    collision_footing: Vec::new(),
                    collision_area: Vec::new(),
                    wmo_id: None,
                    group_bounds: Vec::new(),
                    group_surface_ids: Vec::new(),
                    render_bounds: Some((loaded.min, loaded.max)),
                })
            })
            // Timed on this side too: a load that *fails* still reads the
            // archives, so an undrawable creature costs a frame exactly like a
            // drawn one and would otherwise be the one stutter with no line
            // against it.
            .map_err(|e| tracing::debug!("display id {display_id}: {e} (after {:?})", began.elapsed()))
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

/// How much of a stealthed unit is drawn, as a multiplier on everything its
/// textures produce.
///
/// Chosen rather than measured, and stated as such: no table anywhere says how
/// faint a hidden rogue should be. Far enough down to read as "not really
/// there", far enough up that the silhouette, the gear and which way it is
/// facing all stay legible -- a stealthed player is something you are meant to
/// be able to spot.
const STEALTH_FADE: [f32; 4] = [1.0, 1.0, 1.0, 0.45];

/// The same material, drawn so a tint's alpha means something.
///
/// **Both halves, and only one of them is obvious.** Switching the blend on is
/// what makes the fade visible at all; switching depth writing off is what
/// stops the faded body from carving its own silhouette out of everything
/// drawn after it. A translucent surface that writes depth rejects the grass
/// behind it, so the rogue reads as a person-shaped hole rather than as a
/// person you can see through -- which looks like a rendering fault rather
/// than like stealth.
///
/// A material that already blends is left exactly as it is: additive
/// geometry -- a glow on a weapon -- is not made more transparent by being
/// forced through alpha blending, it is made *wrong*.
pub fn translucent(state: RenderState) -> RenderState {
    if state.blend.is_transparent() {
        return state;
    }
    RenderState {
        blend: BlendMode::Blend,
        depth_write: false,
        ..state
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
/// `AnimationData` rows 41, 42 and 45, read from the table the same way the
/// shuffles were -- `wow-cli dbc dump AnimationData` names them `SwimIdle`,
/// `Swim` and `SwimBackwards`. Rows 43 and 44 are `SwimLeft`/`SwimRight` and
/// are deliberately unused: a strafing swimmer is carried by `Swim` with the
/// body turned, exactly as a strafing runner is carried by the run.
const SWIM_IDLE_ANIMATION_ID: u16 = 41;
const SWIM_ANIMATION_ID: u16 = 42;
const SWIM_BACK_ANIMATION_ID: u16 = 45;
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
/// The *other* hand swinging. `AttackOff` (87), whose own fallback column
/// reads `AttackOffPierce` (88) → `AttackUnarmed` (16) → `Stand` (0).
///
/// A dual-wielding character swings two weapons on two independent timers,
/// and before this every one of those swings drew the main-hand cycle -- so
/// a rogue with a dagger in each hand stabbed with the right hand twice as
/// often as the fight actually called for and never used the left at all.
/// Which swings are which is not guesswork: `SMSG_ATTACKERSTATEUPDATE`
/// carries it in `hit_info`, see [`world::combat::hit_info::OFF_HAND`].
const ATTACK_OFF_ANIMATION_ID: u16 = 87;
const ATTACK_OFF_PIERCE_ANIMATION_ID: u16 = 88;
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
/// rows **89 and 90**, `Sheath` and `HipSheath`.
///
/// **These were 32 and 65, which are the human male's *sequence indices* for
/// those two cycles** -- read out of a `wow-cli m2 anims` listing, whose first
/// column is a position in the model rather than an animation id. The comment
/// warning against exactly that mistake is eleven lines above, on `Jump` and
/// `Fall`, and this walked into it anyway.
///
/// It is worth recording what the two wrong numbers actually did, because
/// neither errored and only one of them did nothing. Id 32 is `SpellCast`,
/// which no character model carries at all, so stowing a weapon on the back
/// fell through to `Stand` -- the transition simply never played. Id 65 is
/// `EmoteTalkQuestion`, which the human male *does* carry (sequence 55, 1800ms,
/// in an external `.anim`), so stowing a one-hander played a character asking
/// a question with its hands for nearly two seconds. Both read as "the sheath
/// animation is a bit underwhelming" rather than as a wrong row, which is the
/// whole reason [`animation_constants_name_the_rows_they_claim`] now exists.
const SHEATH_ANIMATION_ID: u16 = 89;
const HIP_SHEATH_ANIMATION_ID: u16 = 90;
/// Crouched: `AnimationData` rows **120, 119 and 223**, `StealthStand`,
/// `StealthWalk` and `StealthRun`.
///
/// Read out of the table, not off a model listing, and the difference is the
/// one that has already cost this project two silent bugs: `HumanMale.m2`
/// carries all three at *sequence indices* 110, 111 and 146. Transcribing
/// those would name `SwimIdle`, `Drown` and nothing at all -- the first two of
/// which exist, so it would have played a plausible wrong cycle rather than
/// failing.
///
/// The chains below are the table's own fallback column: 223 names 119, which
/// names 4 (`Walk`); 120 names 0 (`Stand`). Every playable model carries all
/// three -- checked, with keyed bones and an external `.anim` behind each --
/// so the fallbacks are for the stealthed *creature*, which the flag applies
/// to just as much.
const STEALTH_STAND_ANIMATION_ID: u16 = 120;
const STEALTH_WALK_ANIMATION_ID: u16 = 119;
const STEALTH_RUN_ANIMATION_ID: u16 = 223;

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

/// And the same again for a cast's follow-through.
///
/// The cast animations are 1000ms on the human male (`SpellCastOmni` and
/// `SpellCastDirected` both), and unlike a swing there is no auto-repeat to
/// snap the pose back to the start -- an instant ability used once and then
/// not again would hold its last frame for as long as this allows. Kept
/// tighter than the attack ceiling for that reason: the failure here is a
/// character stuck mid-flourish, not a cycle cut short.
const CAST_CEILING_MS: u32 = 1_200;

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

/// The animation ids a motion tries, in order, ending somewhere every model
/// can go.
///
/// **This was a `&'static [u16]` and could not stay one.** Every cycle here
/// used to be nameable in the enum -- walking, dying, swinging -- so the chain
/// could be written as a literal. A *cast* cannot be: which animation a spell
/// plays is data, named by `SpellVisualKit` and chained by `AnimationData`'s
/// own fallback column, and there are 17,837 spells with one. So the chain
/// travels by value.
///
/// It is `Copy`, `Eq` and `Hash` because it rides inside [`Motion`], which is
/// a cache key -- and that is sound rather than merely convenient: a chain is
/// entirely determined by its head, so two motions with the same head can
/// never carry different tails and the extra bytes in the key partition
/// nothing new.
///
/// [`Self::MAX`] is twelve because the deepest chain in `AnimationData` is
/// eleven (`FlySpellCastDirected`, whose tail wanders through
/// `FlyClose`/`FlyOpen` -- and *those two name each other*, so anything
/// walking this column needs a cycle guard as well as a length cap; see
/// [`Cycle::chain_from`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Cycle {
    ids: [u16; Cycle::MAX],
    len: u8,
}

impl Cycle {
    pub const MAX: usize = 12;

    /// The chain, in the order it should be tried.
    ///
    /// Longer input is truncated *and said so*, rather than silently: a
    /// truncated chain still draws something plausible, which is exactly the
    /// failure mode this whole file keeps running into.
    pub fn of(ids: &[u16]) -> Self {
        let mut cycle = Cycle::default();
        if ids.len() > Self::MAX {
            tracing::warn!(
                "animation chain {ids:?} is longer than {} and was cut short",
                Self::MAX
            );
        }
        for (slot, id) in cycle.ids.iter_mut().zip(ids) {
            *slot = *id;
            cycle.len += 1;
        }
        cycle
    }

    /// Walks `AnimationData`'s own `fallback` column from `head` down to
    /// `Stand`, which every model has.
    ///
    /// `fallback` maps an id to the one to try instead. Two properties of
    /// that column make the walk less trivial than it looks: `Stand`'s own
    /// fallback is *not* zero (it names 147, `Stand` again by another name),
    /// so the walk stops **at** `Stand` rather than following it; and
    /// `FlyClose` and `FlyOpen` name each other, so a chain can loop. Both
    /// are guarded, and the chain always ends at `Stand` whether or not the
    /// table got there.
    pub fn chain_from(head: u16, fallback: &HashMap<u16, u16>) -> Self {
        let mut ids = Vec::with_capacity(Self::MAX);
        let mut at = head;
        while ids.len() < Self::MAX - 1 && at != STAND_ANIMATION_ID && !ids.contains(&at) {
            ids.push(at);
            match fallback.get(&at) {
                Some(&next) => at = next,
                None => break,
            }
        }
        ids.push(STAND_ANIMATION_ID);
        Cycle::of(&ids)
    }
}

impl std::ops::Deref for Cycle {
    type Target = [u16];

    fn deref(&self) -> &[u16] {
        &self.ids[..self.len as usize]
    }
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
    /// In liquid deep enough to swim in, and whether moving through it.
    ///
    /// **Two states rather than one**, because a swimmer treading water and a
    /// swimmer crossing a lake are as different as standing and running, and
    /// the table names them separately -- `SwimIdle` is a body held upright and
    /// `Swim` is one lying flat and stroking. Playing the second while still
    /// reads as a character swimming on the spot.
    ///
    /// Backwards is its own cycle for the same reason `WalkBack` is: the model
    /// carries `SwimBackwards`, and reversing looks like a sprint performed
    /// facing the wrong way without it.
    Swim(Pace),
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
    /// Mid-swing, from the world-clock millisecond the blow landed, and with
    /// which hand -- see [`Hand`].
    Attacking(u32, Stance, Hand),
    /// Winding up a cast: the pose held for as long as the cast bar runs.
    ///
    /// Carries the chain rather than a spell id because that is what the
    /// renderer needs and because it is what keeps the *cache* honest: two
    /// characters casting different spells that pose the body identically
    /// share a bucket, and two casting the same spell in different forms do
    /// not. The chain comes from `SpellVisual`'s **precast** kit -- the
    /// moment whose animations are named `ReadySpellOmni` and
    /// `ReadySpellDirected`.
    ///
    /// A state rather than an event, and unlike every other one-shot here it
    /// carries no start time: it is true exactly while the server says a cast
    /// is in flight, which is a replicated fact rather than something this
    /// client watched happen once. Same distinction as `UNIT_FIELD_TARGET`
    /// against `SMSG_MONSTER_MOVE`'s facing block.
    Casting(Cycle),
    /// A cast landing, from the world-clock millisecond it did.
    ///
    /// `SpellVisual`'s **casting** kit -- `SpellCastOmni`, `SpellCastDirected`,
    /// and for a melee ability the swing it actually looks like (`Attack1H`
    /// for Sinister Strike, `Special1H` for Eviscerate). This is the only
    /// cast animation an *instant* spell ever gets, and instants are most of
    /// what a melee character casts, so it is the more visible half.
    CastRelease(u32, Cycle),
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
    /// Crouched and moving quietly, at whichever of the three stealth paces
    /// the ground speed calls for.
    ///
    /// A family of its own rather than a modifier on `Stand`/`Walk`/`Run`,
    /// because the models carry it as three separate cycles and because it has
    /// to sit in the bucket key: two rogues side by side, one stealthed, must
    /// not share a pose.
    Stealth(Creep),
}

/// How fast a stealthed unit is creeping.
///
/// Three states because the table has three cycles, and they are not the same
/// three as [`Pace`]: `Pace` distinguishes *direction* -- a swimmer going
/// backwards has its own animation -- and this distinguishes *speed*, because
/// `StealthWalk` and `StealthRun` are separate rows and there is no
/// `StealthWalkBackwards`. Reusing `Pace` here would have made backing up out
/// of a fight play the forward creep and thrown away the walk/run distinction
/// in the same move.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Creep {
    /// Crouched on the spot, including backing up: `StealthWalk` is the only
    /// cycle there is for either direction.
    Still,
    Walk,
    Run,
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

/// What a unit is doing about a spell right now, already resolved to the
/// animation it should play.
///
/// The chains are resolved by the caller rather than here because this module
/// has no game data: which animation a spell poses is
/// `SpellVisualKit`'s business, and the placement builder is where the
/// spellbook lives. See [`crate::spells::CastAnimations`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SpellPose {
    /// A cast is in flight, and this is the wind-up to hold while it runs.
    WindUp(Cycle),
    /// A cast landed this many milliseconds ago.
    Released(u32, Cycle),
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

/// Which way a swimmer is going, if anywhere.
///
/// Its own enum rather than reusing [`Motion::from_pace`]'s walk/run split,
/// because swimming has no second gear: the model carries one `Swim` cycle and
/// a character crossing a lake plays it whether they are dawdling or not.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Pace {
    /// Treading water.
    Still,
    Forward,
    Backward,
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
    /// - **Swimming sits directly below airborne and above everything else.**
    ///   A character in deep water is not standing, running or holding a
    ///   guard, whatever the keys and the weapon say -- and unlike the
    ///   ready/at-ease question, there is no swimming variant of any of those
    ///   cycles to fall back to. It loses to airborne only because a jump that
    ///   ends in water is cleared by the movement code on the frame it lands,
    ///   so the two are never both true for longer than a frame.
    ///
    /// `now_ms` is the caller's world clock, and the one-shot stamps are
    /// derived from it by subtraction so they land on the same timeline
    /// `update_animations` reads.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve(
        speed: f32,
        lateral: f32,
        airborne: bool,
        swimming: bool,
        dead: bool,
        died_ms_ago: Option<u32>,
        swung_ms_ago: Option<(u32, Hand)>,
        fighting: bool,
        now_ms: u32,
        stance: Stance,
        sheathing: Option<(u32, RestKind)>,
        spell: Option<SpellPose>,
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
        if swimming {
            // Forward and backward only: a sidestep in water is carried by the
            // forward stroke with the body turned, the same choice
            // `Motion::from_pace` makes for a strafing runner on land.
            return Motion::Swim(if speed > 0.0 {
                Pace::Forward
            } else if speed < 0.0 {
                Pace::Backward
            } else {
                Pace::Still
            });
        }
        let moving = Motion::from_pace(speed, lateral);
        if moving == Motion::Stand {
            // **A cast outranks a swing, and the wind-up outranks the
            // release.** A character casting is auto-attacking at the same
            // time on most of these fights, so the two states are routinely
            // both true and the order decides which one is ever seen. A cast
            // is the deliberate act and the swing is the automatic one, which
            // is the same reason a run outranks a swing two rules up.
            //
            // Wind-up first because it is a *state* and the release is an
            // *event*: while the server says a cast is in flight, that is
            // true right now, where "a cast landed 400ms ago" is a statement
            // about the past that a new cast has already superseded. The
            // reverse order lets the previous cast's follow-through eat the
            // next one's wind-up, which reads as casting working only every
            // other time.
            match spell {
                Some(SpellPose::WindUp(cycle)) => return Motion::Casting(cycle),
                Some(SpellPose::Released(age, cycle)) if age < CAST_CEILING_MS => {
                    return Motion::CastRelease(bucket(now_ms.saturating_sub(age)), cycle)
                }
                _ => {}
            }
            if let Some((age, hand)) = swung_ms_ago {
                if age < ATTACK_CEILING_MS {
                    return Motion::Attacking(bucket(now_ms.saturating_sub(age)), stance, hand);
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

    /// The same motion, performed crouched.
    ///
    /// **A transformation applied after [`Motion::resolve`] rather than a
    /// thirteenth argument to it**, and the reason is not only that the
    /// argument list is already long. `resolve` answers "what is this body
    /// doing"; stealth does not change the answer, it changes the *posture the
    /// answer is performed in*. Separating them puts the whole precedence rule
    /// in one `match` anybody can read, instead of an early return buried
    /// among eleven other conditions -- and it is testable on its own, against
    /// a `Motion` rather than against twelve arguments.
    ///
    /// **What passes through is the interesting half.** Dying, lying dead,
    /// jumping and swimming are things a body is doing that a crouch cannot
    /// describe, and the server clears the stealth flag for none of them -- so
    /// a rogue who stealths and then jumps off a wall must play the jump, not
    /// a creep in mid-air.
    ///
    /// Everything else collapses to a crouch, the combat states included. That
    /// is not an approximation: swinging, casting, and drawing or stowing a
    /// weapon each break stealth server-side, so the flag is already gone by
    /// the time any of them is replicated. Mapping them anyway is what keeps
    /// this total rather than leaving a hole for the frame in between.
    fn crouched(self) -> Motion {
        match self {
            Motion::Dying(_) | Motion::Dead | Motion::Airborne | Motion::Swim(_) => self,
            Motion::Run => Motion::Stealth(Creep::Run),
            // Backing up creeps too. `AnimationData` has no reverse stealth
            // cycle at all, so the choice is the forward creep or standing
            // still while sliding -- and this project has already settled that
            // one, three times, in `from_pace`.
            Motion::Walk | Motion::WalkBack => Motion::Stealth(Creep::Walk),
            // Standing, the two shuffles, and every combat state: turning on
            // the spot while crouched has no cycle either, and `StealthStand`
            // is at least the right posture where `ShuffleLeft` is not.
            _ => Motion::Stealth(Creep::Still),
        }
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
                | Motion::Swim(_)
                // Continuous, like walking, and this line runs on every
                // rebuild -- which is every frame. A rogue that crouched once
                // would otherwise write a line a frame for as long as it
                // stayed hidden.
                | Motion::Stealth(_)
        )
    }

    /// When a one-shot cycle began, on the caller's world clock.
    fn started_at(self) -> Option<u32> {
        match self {
            Motion::Dying(at)
            | Motion::Attacking(at, _, _)
            | Motion::Sheathing(at, _)
            | Motion::CastRelease(at, _) => Some(at),
            // **`Casting` is deliberately absent.** It is held for as long as
            // the cast bar runs rather than played once from a stamp, so it
            // has nothing to time from -- and giving it one would rebuild the
            // bone buffer every time a second caster started.
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
    ///
    /// The cast cycles are the exception to every sentence above: their chain
    /// is data rather than a literal, so it arrives already built and is
    /// handed straight back. See [`Cycle`].
    fn animation_ids(self) -> Cycle {
        if let Motion::Casting(cycle) | Motion::CastRelease(_, cycle) = self {
            return cycle;
        }
        let ids: &'static [u16] = match self {
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
            // Falling back to the *other* swim cycle rather than to standing
            // or running, for the reason `Airborne` falls back to the run: a
            // body held upright and walking through a lake is a worse picture
            // than one stroking while still. Every playable model carries all
            // three, so the fallbacks are for creatures.
            Motion::Swim(Pace::Still) => &[SWIM_IDLE_ANIMATION_ID, SWIM_ANIMATION_ID],
            Motion::Swim(Pace::Forward) => &[SWIM_ANIMATION_ID, SWIM_IDLE_ANIMATION_ID],
            Motion::Swim(Pace::Backward) => {
                &[SWIM_BACK_ANIMATION_ID, SWIM_ANIMATION_ID, SWIM_IDLE_ANIMATION_ID]
            }
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
            // **The off hand outranks the grip**, and it has to: a
            // dual-wielder is by definition holding two one-handers, so
            // `Stance::TwoHand` and `Hand::Off` cannot both be true and there
            // is nothing to arbitrate. The chain is again the table's own --
            // `AttackOff` (87) names `AttackOffPierce` (88), which names
            // `AttackUnarmed` (16), which names `Stand`.
            Motion::Attacking(_, _, Hand::Off) => &[
                ATTACK_OFF_ANIMATION_ID,
                ATTACK_OFF_PIERCE_ANIMATION_ID,
                ATTACK_UNARMED_ANIMATION_ID,
                STAND_ANIMATION_ID,
            ],
            Motion::Attacking(_, Stance::TwoHand, _) => &[
                ATTACK_2H_ANIMATION_ID,
                ATTACK_1H_ANIMATION_ID,
                ATTACK_UNARMED_ANIMATION_ID,
                STAND_ANIMATION_ID,
            ],
            Motion::Attacking(..) => &[
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
            // The table's own fallback column, unedited: 120 names 0, 119
            // names 4, and 223 names 119. A creature with no crouch at all
            // therefore ends up walking or standing rather than frozen, which
            // is the same answer every other family here reaches.
            Motion::Stealth(Creep::Still) => &[STEALTH_STAND_ANIMATION_ID, STAND_ANIMATION_ID],
            Motion::Stealth(Creep::Walk) => &[STEALTH_WALK_ANIMATION_ID, WALK_ANIMATION_ID],
            Motion::Stealth(Creep::Run) => &[
                STEALTH_RUN_ANIMATION_ID,
                STEALTH_WALK_ANIMATION_ID,
                WALK_ANIMATION_ID,
            ],
            // Returned above, where the chain they carry is handed straight
            // back. Unreachable rather than empty, but an empty chain is the
            // honest thing to write here: there is no literal to give.
            Motion::Casting(_) | Motion::CastRelease(..) => &[],
        };
        Cycle::of(ids)
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
            | ATTACK_OFF_ANIMATION_ID
            | ATTACK_OFF_PIERCE_ANIMATION_ID
            | SHEATH_ANIMATION_ID
            | HIP_SHEATH_ANIMATION_ID
    )
}

/// Whether a cycle repeats until something else happens.
///
/// The mirror of [`plays_once`], and it exists because a **cast** cannot use
/// that list. A spell's animation is whatever `SpellVisualKit` names -- 17,837
/// spells name one, between them reaching `SpellCastOmni`, `Attack1H`,
/// `Special1H`, `BattleRoar` and dozens more -- so "is this id in the
/// hardcoded set of one-shots" is a question that cannot be asked about it.
///
/// What *can* be asked is the other way round: the looping cycles are the
/// small, closed, known set, because they are exactly the ones this client
/// resolves by name. Anything a `SpellVisual` names is a gesture, and a
/// gesture held on its last frame is a pose while a gesture looped is a tic.
///
/// This keeps the rule the original was built on -- decide from the animation
/// that **resolved**, not from the motion that asked -- which is what lets a
/// cast falling back to `Stand` on a model with no cast cycle still loop
/// rather than freeze. That fallback is not hypothetical: `AnimationData`
/// chains every cast animation down to `Stand`, and a wolf has none of them.
fn always_loops(animation_id: u16) -> bool {
    matches!(
        animation_id,
        STAND_ANIMATION_ID
            | WALK_ANIMATION_ID
            | RUN_ANIMATION_ID
            | WALK_BACK_ANIMATION_ID
            | SHUFFLE_LEFT_ANIMATION_ID
            | SHUFFLE_RIGHT_ANIMATION_ID
            | SWIM_IDLE_ANIMATION_ID
            | SWIM_ANIMATION_ID
            | SWIM_BACK_ANIMATION_ID
            | READY_1H_ANIMATION_ID
            | READY_2H_ANIMATION_ID
            | READY_UNARMED_ANIMATION_ID
            // All three crouch cycles loop: stealth is a state held until
            // something breaks it, not a gesture. Absent from `plays_once`,
            // so they would loop anyway -- listed here because this is the
            // list that answers for an id a *spell* named, and Prowl is a
            // spell whose `SpellVisual` could perfectly well name one of
            // these. Held on its last frame, `StealthStand` is a statue.
            | STEALTH_STAND_ANIMATION_ID
            | STEALTH_WALK_ANIMATION_ID
            | STEALTH_RUN_ANIMATION_ID
    )
}

/// How long a transition blends the outgoing cycle into the incoming one,
/// rather than cutting straight to the new cycle's first frame.
///
/// No table names this -- there is no `AnimationData` column for it, the
/// same way there is no column for [`sound::DAYLIGHT_HOURS`] -- so this is a
/// plausible short window and nothing more. Short enough that drawing a
/// weapon or landing a jump does not read as slow motion.
const TRANSITION_BLEND_MS: u32 = 150;

/// What [`bucket_transition`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BucketTransition {
    /// This bucket was already active last frame -- whatever
    /// `World::blending` has on file for it, if anything, stands.
    Continuing,
    /// This bucket was not active last frame. `Some` names what to blend
    /// from; `None` means either this display has never been seen before,
    /// or its last-recorded motion *is* this one already (a bucket that
    /// vanished and immediately reappeared with nothing else happening in
    /// between) -- either way, nothing to blend from.
    New(Option<Motion>),
}

/// Decides whether a `(display id, motion)` bucket just became active and,
/// if so, what to seed its blend from.
///
/// Pure and free of `World` entirely -- deliberately, so the ordering bug
/// this exists to prevent (see `World::active_motion_buckets`'s doc
/// comment) can be pinned by a test that never opens a window. The bug was
/// comparing against a value sibling buckets of the same display could
/// still be writing *this same frame*; `active_last_frame` is a frozen
/// snapshot precisely so that cannot happen here.
fn bucket_transition(
    active_last_frame: &std::collections::HashSet<(u32, Motion)>,
    last_motion_per_display: &HashMap<u32, Motion>,
    display_id: u32,
    motion: Motion,
) -> BucketTransition {
    if active_last_frame.contains(&(display_id, motion)) {
        return BucketTransition::Continuing;
    }
    let from = last_motion_per_display
        .get(&display_id)
        .copied()
        .filter(|seen| *seen != motion);
    BucketTransition::New(from)
}

/// Interpolates two bone palettes, per bone.
///
/// **Local transforms rather than completed model-space matrices.** A child
/// must remain attached to its parent while that parent turns; interpolating
/// their model-space translations independently shortens or stretches the
/// chain. Translation and scale lerp; rotation slerps; then parents compose.
///
/// Production code always has a set of global loops to pass and calls
/// `blend_poses_with_global_loops` directly; this no-loops wrapper survives
/// only as a shorthand for the tests below.
#[cfg(test)]
fn blend_poses(
    bones: &[m2::AnimatedBone],
    from_sequence: usize,
    from_time_ms: u32,
    to_sequence: usize,
    to_time_ms: u32,
    t: f32,
) -> Vec<Mat4> {
    blend_poses_with_global_loops(
        bones,
        from_sequence,
        from_time_ms,
        to_sequence,
        to_time_ms,
        t,
        &[],
    )
}

fn blend_poses_with_global_loops(
    bones: &[m2::AnimatedBone],
    from_sequence: usize,
    from_time_ms: u32,
    to_sequence: usize,
    to_time_ms: u32,
    t: f32,
    global_loops: &[u32],
) -> Vec<Mat4> {
    m2::Model::blend_bones_with_global_loops(
        bones,
        from_sequence,
        from_time_ms,
        to_sequence,
        to_time_ms,
        t,
        global_loops,
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


/// Whether a tile holding collision bounded by `bounds` could answer a query
/// over the horizontal box `lo`..`hi`.
///
/// **The predicate that was wrong**, extracted so it can be tested without a
/// GPU. The old rule was "is this tile's coordinate between the two ends of
/// the query", which silently assumes a tile's collision stays inside the
/// tile. A world object is filed under the single tile containing its origin,
/// so it does not: Stormwind is filed under tile (30,48) and physically
/// covers a three-by-three block.
///
/// Horizontal only. A floor query walks down from the character's own `z`
/// and the vertical span of a tile's geometry says nothing useful about
/// whether it is underfoot -- a tile holding a tower reaches hundreds of
/// units up, and excluding it because the character is at ground level would
/// reintroduce the same class of hole.
fn solid_reaches(bounds: Option<(Vec3, Vec3)>, lo: Vec3, hi: Vec3) -> bool {
    match bounds {
        Some((tlo, thi)) => thi.x >= lo.x && tlo.x <= hi.x && thi.y >= lo.y && tlo.y <= hi.y,
        // A tile with nothing solid on it can never be the answer, and saying
        // so here keeps the caller from paying for an empty query.
        None => false,
    }
}

#[cfg(test)]
mod tests {

    /// **The Stormwind bug, as a test that fails on the old rule.**
    ///
    /// The numbers are the real ones. `STORMWIND.WMO` is placed at world
    /// (-8931, 540), which is tile (30,48), and its own bounds are 1,058 by
    /// 1,060 units against a 533-unit tile -- so its collision physically
    /// covers tiles (29..31, 48..50) while every triangle of it is filed
    /// under (30,48) alone.
    ///
    /// A character standing over tile (31,49) is therefore standing on
    /// geometry that belongs to a different tile. The old selector picked
    /// tiles by the *query's* coordinates and would ask (31,49), which holds
    /// nothing -- and the character fell through a city drawn perfectly
    /// around them.
    #[test]
    fn a_building_bigger_than_a_tile_is_solid_from_its_neighbours() {
        // Stormwind's shell, in world space, filed under one tile.
        let origin = Vec3::new(-8931.0, 540.0, 100.0);
        let bounds = Some((
            origin + Vec3::new(-850.4, -504.3, -99.8),
            origin + Vec3::new(208.0, 555.7, 276.6),
        ));

        // Somewhere inside the city but over a neighbouring tile.
        let over_a_neighbour = origin + Vec3::new(-700.0, 400.0, 0.0);
        assert_ne!(
            tile_at(over_a_neighbour),
            tile_at(origin),
            "the sample has to sit on a different tile or it proves nothing"
        );
        assert!(
            solid_reaches(bounds, over_a_neighbour, over_a_neighbour),
            "the owning tile must answer for a point over its neighbour"
        );

        // And the old rule, stated explicitly so the regression is named
        // rather than merely absent: it asked only the tile under the query.
        let old_rule_would_ask = tile_at(over_a_neighbour) == tile_at(origin);
        assert!(!old_rule_would_ask, "this is exactly what used to be asked");
    }

    /// The other half: a tile whose geometry is nowhere near is not consulted.
    /// Without this the fix would be "ask everything", which is not a fix so
    /// much as a refusal to choose -- and it would make every floor query
    /// scan every resident tile.
    #[test]
    fn a_tile_whose_geometry_is_far_away_is_not_consulted() {
        let bounds = Some((Vec3::new(0.0, 0.0, 0.0), Vec3::new(100.0, 100.0, 50.0)));
        let far = Vec3::new(5000.0, 5000.0, 0.0);
        assert!(!solid_reaches(bounds, far, far));
        // Just outside, by a unit.
        let just_outside = Vec3::new(101.0, 50.0, 0.0);
        assert!(!solid_reaches(bounds, just_outside, just_outside));
        // Just inside.
        let just_inside = Vec3::new(99.0, 50.0, 0.0);
        assert!(solid_reaches(bounds, just_inside, just_inside));
    }

    /// A tile with nothing solid never answers, whatever is asked of it.
    #[test]
    fn an_empty_tile_is_never_consulted() {
        let anywhere = Vec3::ZERO;
        assert!(!solid_reaches(None, anywhere, anywhere));
    }

    /// **Height is deliberately not part of the test.** A floor query starts
    /// at the character's own `z` and searches downward, so a tile holding a
    /// cathedral spire reaches far above them and still holds the flagstones
    /// they are standing on. Filtering by the vertical span would reintroduce
    /// the same kind of hole one axis over.
    #[test]
    fn altitude_does_not_exclude_a_tile() {
        let bounds = Some((Vec3::new(0.0, 0.0, -500.0), Vec3::new(100.0, 100.0, 900.0)));
        let at_ground = Vec3::new(50.0, 50.0, 0.0);
        let high_above = Vec3::new(50.0, 50.0, 5000.0);
        assert!(solid_reaches(bounds, at_ground, at_ground));
        assert!(solid_reaches(bounds, high_above, high_above));
    }
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

    fn two_key_track<T: Copy>(from: T, to: T) -> m2::anim::Track<T> {
        m2::anim::Track {
            interpolation: m2::anim::Interpolation::Linear,
            global_sequence: None,
            sequences: vec![
                m2::anim::Keyframes {
                    times: vec![0],
                    values: vec![from],
                },
                m2::anim::Keyframes {
                    times: vec![0],
                    values: vec![to],
                },
            ],
        }
    }

    fn animated_bone(from: Mat4, to: Mat4, parent: i16) -> m2::AnimatedBone {
        let (from_scale, from_rotation, from_translation) =
            from.to_scale_rotation_translation();
        let (to_scale, to_rotation, to_translation) = to.to_scale_rotation_translation();
        m2::AnimatedBone {
            bone: m2::Bone {
                key_bone_id: -1,
                flags: 0,
                parent,
                submesh_id: 0,
                pivot: [0.0; 3],
            },
            translation: two_key_track(from_translation, to_translation),
            rotation: two_key_track(from_rotation, to_rotation),
            scale: two_key_track(from_scale, to_scale),
        }
    }

    /// The boundaries of a blend must be exact: a transition's first frame
    /// is entirely the outgoing pose and its last is entirely the incoming
    /// one, or a cycle that never moves would still visibly hitch at the
    /// moment `update_animations` calls the blend finished.
    #[test]
    fn a_blend_reaches_both_poses_exactly_at_its_ends() {
        let from = vec![Mat4::from_translation(Vec3::new(0.0, 0.0, 0.0))];
        let to = vec![Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0))];
        let bones = vec![animated_bone(from[0], to[0], -1)];
        assert_eq!(blend_poses(&bones, 0, 0, 1, 0, 0.0), from);
        assert_eq!(blend_poses(&bones, 0, 0, 1, 0, 1.0), to);
    }

    /// Translation blends linearly, same as a naive matrix lerp would --
    /// this is the case a raw lerp gets right, so it is not what
    /// distinguishes `blend_poses` from one.
    #[test]
    fn a_blend_halfway_through_is_halfway_between() {
        let from = vec![Mat4::from_translation(Vec3::new(0.0, 0.0, 0.0))];
        let to = vec![Mat4::from_translation(Vec3::new(10.0, 20.0, 0.0))];
        let bones = vec![animated_bone(from[0], to[0], -1)];
        let mid = blend_poses(&bones, 0, 0, 1, 0, 0.5)[0].transform_point3(Vec3::ZERO);
        assert!((mid - Vec3::new(5.0, 10.0, 0.0)).length() < 1e-4, "{mid}");
    }

    /// **The case a raw matrix lerp gets wrong.** Blending two rotations by
    /// averaging their matrices does not produce a rotation at all -- the
    /// result is not orthonormal, and a unit-length vector fed through it
    /// comes out shorter, which is a limb visibly shrinking mid-swing rather
    /// than turning. `blend_poses` decomposes and slerps for exactly this
    /// reason, so a vector's length must survive the blend.
    #[test]
    fn a_blended_rotation_keeps_vectors_the_same_length() {
        let from = vec![Mat4::from_rotation_z(0.0)];
        let to = vec![Mat4::from_rotation_z(std::f32::consts::PI)]; // a half turn
        let bones = vec![animated_bone(from[0], to[0], -1)];
        let arm = Vec3::new(1.0, 0.0, 0.0);
        for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let blended = blend_poses(&bones, 0, 0, 1, 0, t)[0];
            let rotated = blended.transform_vector3(arm);
            assert!(
                (rotated.length() - 1.0).abs() < 1e-4,
                "at t={t} the arm changed length to {} -- a raw matrix lerp would do this at t=0.5",
                rotated.length()
            );
        }
    }

    /// Blending never sees a `t` outside `0.0..=1.0` in practice -- elapsed
    /// time is checked against the window before this is called -- but a
    /// clamp costs nothing and a caller that got the arithmetic wrong should
    /// not extrapolate past either pose.
    #[test]
    fn an_out_of_range_t_is_clamped() {
        let from = vec![Mat4::from_translation(Vec3::new(0.0, 0.0, 0.0))];
        let to = vec![Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0))];
        let bones = vec![animated_bone(from[0], to[0], -1)];
        assert_eq!(blend_poses(&bones, 0, 0, 1, 0, -1.0), from);
        assert_eq!(blend_poses(&bones, 0, 0, 1, 0, 2.0), to);
    }

    #[test]
    fn a_blended_child_stays_attached_to_its_turning_parent() {
        let root = animated_bone(
            Mat4::IDENTITY,
            Mat4::from_rotation_z(std::f32::consts::PI),
            -1,
        );
        let child = animated_bone(
            Mat4::from_translation(Vec3::X),
            Mat4::from_translation(Vec3::X),
            0,
        );
        let pose = blend_poses(&[root, child], 0, 0, 1, 0, 0.5);
        let root_position = pose[0].transform_point3(Vec3::ZERO);
        let child_position = pose[1].transform_point3(Vec3::ZERO);
        assert!((child_position.distance(root_position) - 1.0).abs() < 1e-4);
    }

    /// **The bug this whole mechanism exists to prevent, pinned directly.**
    /// Reported live: every wolf around a player twitching, sliding, or
    /// walking in place -- and every NPC sliding too. The cause was that the
    /// first version of this feature kept one "current motion" value per
    /// *display id*, shared by every bucket of that species -- so with two
    /// wolves alive at once in different motions (the ordinary case, since
    /// wolves wander independently), whichever bucket a frame's loop reached
    /// second saw the first one's write and read it as its own transition.
    /// Forever, because the next frame reset it right back.
    ///
    /// A single simulated wolf pack: display 299 has a Stand bucket and a
    /// Walk bucket, both already active. Checking either against the
    /// *other's* presence in the same, frozen `active_last_frame` snapshot
    /// must read `Continuing` -- not `New` -- however many times and in
    /// whatever order they are checked, which is what a `HashSet` snapshot
    /// guarantees and a mutable shared value did not.
    #[test]
    fn two_buckets_of_the_same_display_do_not_retrigger_each_other() {
        const WOLF: u32 = 299;
        let active: std::collections::HashSet<(u32, Motion)> =
            [(WOLF, Motion::Stand), (WOLF, Motion::Walk)].into_iter().collect();
        let mut last_motion = HashMap::new();
        last_motion.insert(WOLF, Motion::Walk);

        for _ in 0..3 {
            assert_eq!(
                bucket_transition(&active, &last_motion, WOLF, Motion::Stand),
                BucketTransition::Continuing,
                "the standing wolves must not see the walking ones as a transition"
            );
            assert_eq!(
                bucket_transition(&active, &last_motion, WOLF, Motion::Walk),
                BucketTransition::Continuing,
                "the walking wolves must not see the standing ones as a transition"
            );
        }
    }

    /// The case the mechanism exists *for*, so the fix above did not lose
    /// it: a bucket that genuinely is new blends from the display's last
    /// recorded motion.
    #[test]
    fn a_genuinely_new_bucket_blends_from_the_last_recorded_motion() {
        let active = std::collections::HashSet::new(); // nothing active yet
        let mut last_motion = HashMap::new();
        last_motion.insert(7, Motion::Stand);

        assert_eq!(
            bucket_transition(&active, &last_motion, 7, Motion::Run),
            BucketTransition::New(Some(Motion::Stand))
        );
    }

    /// A display never seen before has nothing to blend from -- blending
    /// out of an invented "previous" pose would itself be a new snap, not a
    /// fix for one.
    #[test]
    fn a_display_seen_for_the_first_time_has_no_blend_source() {
        let active = std::collections::HashSet::new();
        let last_motion = HashMap::new();
        assert_eq!(
            bucket_transition(&active, &last_motion, 7, Motion::Stand),
            BucketTransition::New(None)
        );
    }

    /// A bucket that vanished and reappeared with the display's last motion
    /// unchanged (nothing else happened while it was gone) has nothing new
    /// to blend from either.
    #[test]
    fn a_bucket_reappearing_as_its_own_last_motion_does_not_blend() {
        let active = std::collections::HashSet::new(); // this bucket just dropped out
        let mut last_motion = HashMap::new();
        last_motion.insert(7, Motion::Stand);
        assert_eq!(
            bucket_transition(&active, &last_motion, 7, Motion::Stand),
            BucketTransition::New(None)
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
            Motion::resolve(0.0, 0.0, false, false, false, None, None, false, 4_000, Stance::TwoHand, None, None),
            Motion::Ready(Stance::TwoHand),
        );
        // And with nothing drawn it still relaxes, which is the half that
        // stops this from simply making everyone stand guard forever.
        assert_eq!(
            Motion::resolve(0.0, 0.0, false, false, false, None, None, false, 4_000, Stance::Unarmed, None, None),
            Motion::Stand,
        );
        // Moving still outranks it: a character runs, weapon or no weapon.
        assert_eq!(
            Motion::resolve(7.0, 0.0, false, false, false, None, None, false, 4_000, Stance::TwoHand, None, None),
            Motion::Run,
        );
    }

    /// Swimming outranks the ground cycles and loses only to death and to
    /// being airborne.
    ///
    /// **Both halves are asserted, and that is the point.** A test that only
    /// checked "a swimmer swims" passes just as well under a rule that returns
    /// `Swim` unconditionally -- which would lay every corpse in the world out
    /// stroking. So each thing swimming beats and each thing it loses to is
    /// named, the same shape as the auto-attack exception in 4.3.
    #[test]
    fn swimming_sits_between_airborne_and_the_ground_cycles() {
        let swim = |speed: f32| {
            Motion::resolve(speed, 0.0, false, true, false, None, None, false, 4_000, Stance::Unarmed, None, None)
        };
        // It beats running, standing and a drawn weapon's guard.
        assert_eq!(swim(7.0), Motion::Swim(Pace::Forward));
        assert_eq!(swim(0.0), Motion::Swim(Pace::Still));
        assert_eq!(swim(-2.5), Motion::Swim(Pace::Backward));
        assert_eq!(
            Motion::resolve(0.0, 0.0, false, true, false, None, None, true, 4_000, Stance::TwoHand, None, None),
            Motion::Swim(Pace::Still),
            "a swimmer does not hold a guard"
        );
        // A sidestep in water is carried by the forward stroke, not by a
        // shuffle: the turning input must not reach `from_pace`.
        assert_eq!(
            Motion::resolve(0.0, 1.0, false, true, false, None, None, false, 4_000, Stance::Unarmed, None, None),
            Motion::Swim(Pace::Still),
        );

        // And it loses to the two things above it.
        assert_eq!(
            Motion::resolve(7.0, 0.0, true, true, false, None, None, false, 4_000, Stance::Unarmed, None, None),
            Motion::Airborne,
            "a jump still in flight outranks the water it is heading for"
        );
        assert_eq!(
            Motion::resolve(7.0, 0.0, false, true, true, None, None, false, 4_000, Stance::Unarmed, None, None),
            Motion::Dead,
            "a drowned corpse does not keep swimming"
        );

        // The cycles each state reaches for, so a renumbered constant cannot
        // silently swap the stroke for the tread.
        assert_eq!(
            Motion::Swim(Pace::Forward).animation_ids().first(),
            Some(&SWIM_ANIMATION_ID)
        );
        assert_eq!(
            Motion::Swim(Pace::Still).animation_ids().first(),
            Some(&SWIM_IDLE_ANIMATION_ID)
        );
        assert_eq!(
            Motion::Swim(Pace::Backward).animation_ids().first(),
            Some(&SWIM_BACK_ANIMATION_ID)
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
        let swing = Motion::Attacking(0, Stance::TwoHand, Hand::Main).animation_ids();
        assert_eq!(swing.first(), Some(&ATTACK_2H_ANIMATION_ID));

        // A one-handed character must not get them, or every dagger is held
        // like a greatsword -- the half that a "prefers 2H" assertion alone
        // would pass without.
        assert!(!Motion::Ready(Stance::OneHand)
            .animation_ids()
            .contains(&READY_2H_ANIMATION_ID));
        assert!(!Motion::Attacking(0, Stance::OneHand, Hand::Main)
            .animation_ids()
            .contains(&ATTACK_2H_ANIMATION_ID));

        for stance in [Stance::Unarmed, Stance::OneHand, Stance::TwoHand] {
            for motion in [Motion::Ready(stance), Motion::Attacking(0, stance, Hand::Main)] {
                assert_eq!(
                    motion.animation_ids().last(),
                    Some(&STAND_ANIMATION_ID),
                    "{motion:?} can resolve to nothing"
                );
            }
        }
    }

    /// Crouching changes the posture and not what the body is doing.
    ///
    /// **The pass-through half is the one worth asserting.** A rogue who
    /// stealths and then jumps off a wall keeps the flag -- the server clears
    /// it for none of these -- so a `crouched` that mapped everything would
    /// draw a creep in mid-air and a corpse frozen in a crouch. Both are
    /// states the flag can genuinely coexist with, unlike the combat set
    /// below, which cannot.
    #[test]
    fn crouching_replaces_the_ground_cycles_and_nothing_else() {
        assert_eq!(Motion::Run.crouched(), Motion::Stealth(Creep::Run));
        assert_eq!(Motion::Walk.crouched(), Motion::Stealth(Creep::Walk));
        assert_eq!(Motion::WalkBack.crouched(), Motion::Stealth(Creep::Walk));
        assert_eq!(Motion::Stand.crouched(), Motion::Stealth(Creep::Still));
        assert_eq!(
            Motion::Shuffle(Side::Left).crouched(),
            Motion::Stealth(Creep::Still)
        );

        for airborne_or_wet in [
            Motion::Airborne,
            Motion::Swim(Pace::Forward),
            Motion::Swim(Pace::Still),
            Motion::Dying(0),
            Motion::Dead,
        ] {
            assert_eq!(
                airborne_or_wet.crouched(),
                airborne_or_wet,
                "{airborne_or_wet:?} is not something a crouch can describe"
            );
        }
    }

    /// Every crouch chain ends somewhere a model without the cycles can go.
    ///
    /// The chains are `AnimationData.dbc`'s own fallback column -- 223 names
    /// 119, which names 4 -- and the last entry is the load-bearing one: the
    /// stealth flag is set on *units*, so a stealthed creature with none of
    /// the three reaches this code, and a chain that ended at 119 would leave
    /// it in its bind pose sliding along the ground.
    #[test]
    fn the_crouch_chains_fall_back_to_ordinary_movement() {
        assert_eq!(
            Motion::Stealth(Creep::Run).animation_ids().first(),
            Some(&STEALTH_RUN_ANIMATION_ID)
        );
        assert_eq!(
            Motion::Stealth(Creep::Walk).animation_ids().first(),
            Some(&STEALTH_WALK_ANIMATION_ID)
        );
        assert_eq!(
            Motion::Stealth(Creep::Still).animation_ids().first(),
            Some(&STEALTH_STAND_ANIMATION_ID)
        );

        assert_eq!(
            Motion::Stealth(Creep::Still).animation_ids().last(),
            Some(&STAND_ANIMATION_ID)
        );
        for moving in [Creep::Walk, Creep::Run] {
            assert_eq!(
                Motion::Stealth(moving).animation_ids().last(),
                Some(&WALK_ANIMATION_ID),
                "a creeping {moving:?} with no crouch cycle would freeze"
            );
        }

        // Held rather than looped is the failure this guards: a `StealthStand`
        // stopped on its last frame is a statue in a crouch.
        for creep in [Creep::Still, Creep::Walk, Creep::Run] {
            for id in Motion::Stealth(creep).animation_ids().iter() {
                assert!(!plays_once(*id), "animation {id} would stop mid-crouch");
            }
        }
    }

    /// Fading a model needs the blend *and* the depth write, and must leave
    /// anything that already blends exactly as it is.
    ///
    /// Each half fails differently and neither failure looks like the other.
    /// Without the blend the tint is discarded and the rogue draws at full
    /// strength -- the feature looks unimplemented. Without the depth change
    /// the rogue draws as a person-shaped hole in the grass behind it, which
    /// looks like a rendering fault. And forcing alpha blending onto an
    /// additive glow does not make it fainter, it makes it wrong.
    #[test]
    fn fading_a_material_switches_the_blend_and_stops_writing_depth() {
        let opaque = RenderState {
            blend: BlendMode::Opaque,
            two_sided: false,
            depth_write: true,
            winding: render::mesh::Winding::CounterClockwise,
        };
        let faded = translucent(opaque);
        assert_eq!(faded.blend, BlendMode::Blend);
        assert!(!faded.depth_write);
        assert_eq!(faded.winding, opaque.winding, "culling is not a fade");

        for already in [BlendMode::Blend, BlendMode::Additive] {
            let state = RenderState {
                blend: already,
                ..opaque
            };
            assert_eq!(
                translucent(state).blend,
                already,
                "{already:?} was rewritten by a fade that had nothing to do"
            );
        }

        // A cutout material -- foliage, a beard -- is opaque as far as the
        // pipeline is concerned and does need switching, or a stealthed
        // character's hair stays solid while the rest of it fades.
        assert_eq!(
            translucent(RenderState {
                blend: BlendMode::AlphaKey,
                ..opaque
            })
            .blend,
            BlendMode::Blend
        );
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

    /// **The off hand swings its own arm**, and the main-hand chain must not
    /// contain that cycle -- the half a "prefers `AttackOff`" assertion alone
    /// would pass without, and the half that was actually broken: every swing
    /// a dual-wielder made drew the main-hand cycle.
    ///
    /// The chain itself is `AnimationData`'s own fallback column, walked in
    /// the table: 87 names 88, which names 16, which names 0.
    #[test]
    fn the_off_hand_swings_the_other_arm() {
        let off = Motion::Attacking(0, Stance::OneHand, Hand::Off).animation_ids();
        assert_eq!(off.first(), Some(&ATTACK_OFF_ANIMATION_ID));
        assert_eq!(
            &*off,
            &[
                ATTACK_OFF_ANIMATION_ID,
                ATTACK_OFF_PIERCE_ANIMATION_ID,
                ATTACK_UNARMED_ANIMATION_ID,
                STAND_ANIMATION_ID
            ]
        );

        for stance in [Stance::Unarmed, Stance::OneHand, Stance::TwoHand] {
            let main = Motion::Attacking(0, stance, Hand::Main).animation_ids();
            assert!(
                !main.contains(&ATTACK_OFF_ANIMATION_ID),
                "{stance:?} in the main hand reached for the off-hand cycle"
            );
        }
        // A dual-wielder cannot be holding a two-hander, so there is nothing
        // to arbitrate -- but the arm that decides has to be the *hand*, or
        // the rule silently depends on a combination that cannot happen.
        assert_eq!(
            Motion::Attacking(0, Stance::TwoHand, Hand::Off)
                .animation_ids()
                .first(),
            Some(&ATTACK_OFF_ANIMATION_ID)
        );
    }

    /// And the hand travels all the way from the wire: a swing's `hit_info`
    /// decides it, not what the attacker has equipped.
    #[test]
    fn the_wire_decides_which_hand_swung() {
        let swing = |hand| {
            Motion::resolve(
                0.0,
                0.0,
                false,
                false,
                false,
                None,
                Some((50, hand)),
                true,
                4_000,
                Stance::OneHand,
                None,
                None,
            )
        };
        assert_eq!(swing(Hand::Off), Motion::Attacking(3_900, Stance::OneHand, Hand::Off));
        assert_eq!(
            swing(Hand::Main),
            Motion::Attacking(3_900, Stance::OneHand, Hand::Main)
        );
        // Different buckets, so the two arms are posed from different bone
        // buffers rather than one overwriting the other.
        assert_ne!(swing(Hand::Off), swing(Hand::Main));
    }

    /// The same trap in a new pair of ids: miss either in `plays_once` and a
    /// draw or a stow loops, which is a character repeatedly drawing a
    /// weapon that is already drawn.
    #[test]
    fn sheathing_plays_once_like_the_other_one_shots() {
        assert!(plays_once(SHEATH_ANIMATION_ID));
        assert!(plays_once(HIP_SHEATH_ANIMATION_ID));
        assert!(!plays_once(STAND_ANIMATION_ID));
        // And the off hand, which is the third pair to walk into this.
        assert!(plays_once(ATTACK_OFF_ANIMATION_ID));
        assert!(plays_once(ATTACK_OFF_PIERCE_ANIMATION_ID));
    }

    /// **A cast cannot use `plays_once`'s list and must not need to.**
    ///
    /// Which animation a spell plays is whatever `SpellVisualKit` names --
    /// thousands of spells between them reaching dozens of ids -- so the
    /// question has to be asked the other way round, of the small closed set
    /// of cycles that loop. Both halves are asserted because either alone
    /// passes under the wrong rule: a gesture must hold its last frame, and
    /// the `Stand` a model without that gesture falls back to must not.
    #[test]
    fn a_cast_holds_its_gesture_and_loops_the_fallback() {
        // Two of the ids `SpellVisual`'s casting kits actually name, neither
        // of which is in `plays_once`'s list and neither of which loops.
        const SPELL_CAST_OMNI: u16 = 54;
        const SPELL_CAST_DIRECTED: u16 = 53;
        assert!(!always_loops(SPELL_CAST_OMNI));
        assert!(!always_loops(SPELL_CAST_DIRECTED));
        // The fallback every cast chain ends at, which a model with no cast
        // cycle gets -- a wolf, say. Frozen on the last frame of a stand is
        // a statue, which is the bug `plays_once` was written to avoid in
        // the first place.
        assert!(always_loops(STAND_ANIMATION_ID));
        assert!(always_loops(READY_1H_ANIMATION_ID));
        assert!(always_loops(RUN_ANIMATION_ID));
    }

    /// A cast in flight outranks the swings going on underneath it, and the
    /// wind-up outranks the previous cast's follow-through.
    ///
    /// Both orderings matter and neither is arbitrary. Auto-attack keeps
    /// swinging through a cast, so without the first rule the cast is never
    /// drawn at all. And a release lingers for up to [`CAST_CEILING_MS`],
    /// which is longer than the gap between two casts of a fast spell, so
    /// without the second the wind-up of every cast after the first is eaten
    /// by the one before it.
    #[test]
    fn a_cast_outranks_the_swings_and_the_previous_cast() {
        let wind_up = SpellPose::WindUp(Cycle::of(&[53, 0]));
        let release = SpellPose::Released(200, Cycle::of(&[54, 0]));
        let resolve = |spell| {
            Motion::resolve(
                0.0,
                0.0,
                false,
                false,
                false,
                None,
                Some((50, Hand::Main)),
                true,
                4_000,
                Stance::OneHand,
                None,
                Some(spell),
            )
        };
        assert_eq!(resolve(wind_up), Motion::Casting(Cycle::of(&[53, 0])));
        assert_eq!(
            resolve(release),
            Motion::CastRelease(3_800, Cycle::of(&[54, 0]))
        );

        // Moving still outranks both, for the reason it outranks a swing:
        // nothing here can blend an upper body onto a run, so a cast played
        // over one reads as a stumble.
        assert_eq!(
            Motion::resolve(
                7.0,
                0.0,
                false,
                false,
                false,
                None,
                None,
                false,
                4_000,
                Stance::OneHand,
                None,
                Some(wind_up),
            ),
            Motion::Run
        );
        // And a corpse does not cast, however recently it did.
        assert_eq!(
            Motion::resolve(
                0.0,
                0.0,
                false,
                false,
                true,
                None,
                None,
                false,
                10_000,
                Stance::OneHand,
                None,
                Some(release),
            ),
            Motion::Dead
        );
    }

    /// The follow-through lapses, or a character who cast once stands frozen
    /// mid-flourish for the rest of the session. Same shape as the sheath
    /// transition and the fall.
    #[test]
    fn a_cast_release_lapses_once_it_has_had_time_to_finish() {
        let stale = SpellPose::Released(CAST_CEILING_MS + 1, Cycle::of(&[54, 0]));
        assert_eq!(
            Motion::resolve(
                0.0, 0.0, false, false, false, None, None, true, 10_000, Stance::OneHand, None,
                Some(stale),
            ),
            Motion::Ready(Stance::OneHand),
            "a lapsed cast should fall through to the ordinary standing rules"
        );
    }

    /// A chain built from `AnimationData`'s fallback column always ends
    /// somewhere every model can go, and survives the two things that column
    /// actually does.
    ///
    /// `FlyClose` and `FlyOpen` name each other, so the walk must not loop
    /// forever; and `Stand`'s own fallback is not zero, so the walk must stop
    /// *at* standing rather than following it onwards.
    #[test]
    fn a_fallback_chain_terminates_even_when_the_table_does_not() {
        let mut fallback = HashMap::new();
        // A pair that names each other, exactly like the table's own.
        fallback.insert(200, 201);
        fallback.insert(201, 200);
        let looping = Cycle::chain_from(200, &fallback);
        assert_eq!(&*looping, &[200, 201, STAND_ANIMATION_ID]);

        // A chain that simply runs out.
        assert_eq!(&*Cycle::chain_from(300, &fallback), &[300, STAND_ANIMATION_ID]);
        // And one that is already there.
        assert_eq!(
            &*Cycle::chain_from(STAND_ANIMATION_ID, &fallback),
            &[STAND_ANIMATION_ID]
        );

        // Every chain ends at standing, whatever the table said.
        let mut long = HashMap::new();
        for id in 1..40u16 {
            long.insert(id, id + 1);
        }
        let deep = Cycle::chain_from(1, &long);
        assert_eq!(deep.last(), Some(&STAND_ANIMATION_ID));
        assert!(deep.len() <= Cycle::MAX);
    }

    /// A recent sheath change, standing still, resolves to the transition --
    /// and to the cycle the caller named, not a guessed one.
    #[test]
    fn a_recent_sheath_change_plays_the_transition() {
        assert_eq!(
            Motion::resolve(
                0.0, 0.0, false, false, false, None, None, false, 4_000, Stance::Unarmed,
                Some((200, RestKind::Hip)), None,
            ),
            Motion::Sheathing(3_800, RestKind::Hip)
        );
        assert_eq!(
            Motion::resolve(
                0.0, 0.0, false, false, false, None, None, false, 4_000, Stance::Unarmed,
                Some((200, RestKind::Back)), None,
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
                0.0, 0.0, false, false, false, None, Some((50, Hand::Main)), true, 4_000, Stance::OneHand,
                Some((50, RestKind::Hip)), None,
            ),
            Motion::Attacking(3_900, Stance::OneHand, Hand::Main)
        );
    }

    /// The transition lapses once it has had time to finish, the same way a
    /// fall settles -- otherwise a unit that changed its sheath state once
    /// stays frozen mid-transition for the rest of the session.
    #[test]
    fn a_sheath_transition_lapses_once_it_has_had_time_to_finish() {
        assert_eq!(
            Motion::resolve(
                0.0, 0.0, false, false, false, None, None, true, 10_000, Stance::OneHand,
                Some((SHEATH_CEILING_MS + 1, RestKind::Hip)), None,
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
                7.0, 0.0, false, false, false, None, None, false, 4_000, Stance::OneHand,
                Some((50, RestKind::Hip)), None,
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
            Motion::resolve(7.0, 0.0, false, false, true, None, None, false, 10_000, Stance::Unarmed, None, None),
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
            Motion::resolve(0.0, 0.0, false, false, true, Some(200), None, false, 10_000, Stance::Unarmed, None, None),
            Motion::Dying(9_800)
        );
        assert_eq!(Motion::resolve(0.0, 0.0, false, false, true, None, None, false, 10_000, Stance::Unarmed, None, None), Motion::Dead);
    }

    /// And it stops falling eventually, rather than holding a one-shot bucket
    /// for the life of the corpse.
    #[test]
    fn a_fall_settles_once_it_has_had_time_to_finish() {
        assert_eq!(
            Motion::resolve(0.0, 0.0, false, false, true, Some(ONE_SHOT_CEILING_MS + 1), None, false, 10_000, Stance::Unarmed, None, None),
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
        let first = Motion::resolve(0.0, 0.0, false, false, true, Some(100), None, false, 5_000, Stance::Unarmed, None, None);
        // A second later: the death is a second older and the clock a second
        // further on, which is the same death.
        let later = Motion::resolve(0.0, 0.0, false, false, true, Some(1_100), None, false, 6_000, Stance::Unarmed, None, None);
        assert_eq!(first, later, "the bucket moved with the clock");

        // Two deaths a second apart must *not* share, or one pops to the
        // other's frame.
        let other = Motion::resolve(0.0, 0.0, false, false, true, Some(100), None, false, 6_000, Stance::Unarmed, None, None);
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
        let reference = Motion::resolve(0.0, 0.0, false, false, false, None, Some((40, Hand::Main)), true, 8_000, Stance::Unarmed, None, None);
        for frame in 0..8u32 {
            let drift = frame * 3;
            let seen = Motion::resolve(0.0, 0.0, false,
                false, false,
                None,
                Some((40 + frame * 16, Hand::Main)),
                true,
                8_000 + frame * 16 + drift,
                Stance::Unarmed,
                None, None,
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
            Motion::resolve(0.0, 0.0, false, false, false, None, Some((ATTACK_CEILING_MS + 1, Hand::Main)), true, 9_000, Stance::Unarmed, None, None),
            Motion::Ready(Stance::Unarmed)
        );
    }

    /// A swing interrupts standing and not running: a creature chasing you
    /// swings as it runs, and the swing played over the run reads as a
    /// stumble.
    #[test]
    fn a_swing_interrupts_standing_but_not_running() {
        assert_eq!(
            Motion::resolve(0.0, 0.0, false, false, false, None, Some((50, Hand::Main)), true, 4_000, Stance::Unarmed, None, None),
            Motion::Attacking(3_900, Stance::Unarmed, Hand::Main)
        );
        assert_eq!(Motion::resolve(7.0, 0.0, false, false, false, None, Some((50, Hand::Main)), true, 4_000, Stance::Unarmed, None, None), Motion::Run);
        // And an old swing has stopped mattering.
        assert_eq!(
            Motion::resolve(0.0, 0.0, false, false, false, None, Some((ONE_SHOT_CEILING_MS + 1, Hand::Main)), false, 4_000, Stance::Unarmed, None, None),
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
            Motion::resolve(0.0, 0.0, false, false, false, None, None, true, 4_000, Stance::Unarmed, None, None),
            Motion::Ready(Stance::Unarmed)
        );
        // Out of the fight, it relaxes.
        assert_eq!(
            Motion::resolve(0.0, 0.0, false, false, false, None, None, false, 4_000, Stance::Unarmed, None, None),
            Motion::Stand
        );
        // A swing still beats the guard, and running still beats both.
        assert_eq!(
            Motion::resolve(0.0, 0.0, false, false, false, None, Some((50, Hand::Main)), true, 4_000, Stance::Unarmed, None, None),
            Motion::Attacking(3_900, Stance::Unarmed, Hand::Main)
        );
        assert_eq!(Motion::resolve(7.0, 0.0, false, false, false, None, None, true, 4_000, Stance::Unarmed, None, None), Motion::Run);
        // And a corpse is not "in a fight" whatever the map still says.
        assert_eq!(Motion::resolve(0.0, 0.0, false, false, true, None, None, true, 4_000, Stance::Unarmed, None, None), Motion::Dead);
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
        assert_eq!(Motion::Attacking(99, Stance::Unarmed, Hand::Main).started_at(), Some(99));
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
        for motion in [Motion::Attacking(0, Stance::Unarmed, Hand::Main), Motion::Ready(Stance::Unarmed)] {
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
            Motion::Attacking(0, Stance::Unarmed, Hand::Main),
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
        // Dry: this fixture exists to pin the chunk *indexing*, which liquid
        // shares but does not affect. `liquid_at` gets its own test below.
        TileHeights::new(&chunks, &adt::TileLiquid::default())
    }

    /// A tile with one sheet of water answers for the ground it covers and
    /// nowhere else.
    ///
    /// The point is the two-step lookup: `TileHeights` places a chunk by its
    /// recorded *position*, and the sheet then places itself within that chunk
    /// by an offset rectangle. Either step being wrong puts water in a
    /// plausible place that is not the right one, which is precisely what
    /// `wow-cli adt liquid` had to measure over a whole map to rule out.
    #[test]
    fn liquid_answers_only_over_the_cells_it_covers() {
        let tile = (32, 48);
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
                        0.0,
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

        // One flat sheet over the first chunk in file order, covering the four
        // cells nearest its origin corner and standing 5 units up.
        let mut payload = vec![0u8; adt::CHUNK_COUNT * 12];
        let instances_at = payload.len();
        payload[0..4].copy_from_slice(&(instances_at as u32).to_le_bytes());
        payload[4..8].copy_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&5u16.to_le_bytes());
        payload.extend_from_slice(&0u16.to_le_bytes());
        payload.extend_from_slice(&5f32.to_le_bytes());
        payload.extend_from_slice(&5f32.to_le_bytes());
        payload.extend_from_slice(&[0, 0, 2, 2]);
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());

        let heights = TileHeights::new(&chunks, &adt::TileLiquid::parse(&payload));
        // Inside the sheet: chunk 0's corner, one cell in on both axes.
        let inside = heights.liquid_at(
            origin.0 - adt::UNIT_SIZE,
            origin.1 - adt::UNIT_SIZE,
        );
        assert_eq!(inside, Some((5.0, 5)), "over the sheet");
        // Past the rectangle within the same chunk: still dry.
        assert_eq!(
            heights.liquid_at(origin.0 - 5.0 * adt::UNIT_SIZE, origin.1 - adt::UNIT_SIZE),
            None,
            "past the rectangle's edge"
        );
        // A different chunk entirely: dry, and not the first chunk's answer
        // leaking across because the grid lookup fell back to zero.
        assert_eq!(
            heights.liquid_at(
                origin.0 - 3.0 * adt::CHUNK_SIZE,
                origin.1 - 3.0 * adt::CHUNK_SIZE
            ),
            None,
            "a chunk with no sheet"
        );
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

    /// **Every animation id this client hardcodes, checked against the row it
    /// claims to be.**
    ///
    /// This exists because two of them were wrong for a whole milestone and
    /// nothing could have said so. `SHEATH_ANIMATION_ID` was 32 and
    /// `HIP_SHEATH_ANIMATION_ID` was 65 -- the human male's *sequence indices*
    /// for `Sheath` and `HipSheath`, lifted out of a `wow-cli m2 anims`
    /// listing whose first column is a position in the model. A wrong
    /// animation id cannot fail loudly: 32 is `SpellCast`, which no character
    /// model carries, so the stow transition silently fell back to `Stand`;
    /// 65 is `EmoteTalkQuestion`, which the human male does carry, so the
    /// other half played an 1800ms hand gesture. One did nothing and one did
    /// something plausible, and neither is distinguishable from "the
    /// animation is fine" without opening the table.
    ///
    /// So the check is by **name**, which is the only thing in
    /// `AnimationData.dbc` that cannot be arrived at by a coincidence of small
    /// integers -- the same reason `CreatureSoundData`'s columns were
    /// identified by the names of the sounds they reach rather than by their
    /// values being valid.
    #[test]
    fn animation_constants_name_the_rows_they_claim() {
        let Some(data) = std::env::var_os("WOW_DATA") else {
            eprintln!("skipping: WOW_DATA not set");
            return;
        };
        let mut chain = Chain::open_wow_data(data, "enUS").expect("opening archives");
        let table = dbc::schema::AnimationData::parse(
            &chain
                .read(dbc::schema::AnimationData::PATH)
                .expect("AnimationData"),
        )
        .expect("parsing AnimationData");
        let name_of = |id: u16| {
            table
                .iter()
                .find(|row| row.id() == u32::from(id))
                .unwrap_or_else(|| panic!("no AnimationData row with id {id}"))
                .name()
                .to_string()
        };

        for (id, name) in [
            (STAND_ANIMATION_ID, "Stand"),
            (WALK_ANIMATION_ID, "Walk"),
            (RUN_ANIMATION_ID, "Run"),
            (WALK_BACK_ANIMATION_ID, "Walkbackwards"),
            (SHUFFLE_LEFT_ANIMATION_ID, "ShuffleLeft"),
            (SHUFFLE_RIGHT_ANIMATION_ID, "ShuffleRight"),
            (JUMP_ANIMATION_ID, "Jump"),
            (FALL_ANIMATION_ID, "Fall"),
            (SWIM_IDLE_ANIMATION_ID, "SwimIdle"),
            (SWIM_ANIMATION_ID, "Swim"),
            (SWIM_BACK_ANIMATION_ID, "SwimBackwards"),
            (DEATH_ANIMATION_ID, "Death"),
            (DEAD_ANIMATION_ID, "Dead"),
            (ATTACK_1H_ANIMATION_ID, "Attack1H"),
            (ATTACK_2H_ANIMATION_ID, "Attack2H"),
            (ATTACK_UNARMED_ANIMATION_ID, "AttackUnarmed"),
            (ATTACK_OFF_ANIMATION_ID, "AttackOff"),
            (ATTACK_OFF_PIERCE_ANIMATION_ID, "AttackOffPierce"),
            (READY_1H_ANIMATION_ID, "Ready1H"),
            (READY_2H_ANIMATION_ID, "Ready2H"),
            (READY_UNARMED_ANIMATION_ID, "ReadyUnarmed"),
            (SHEATH_ANIMATION_ID, "Sheath"),
            (HIP_SHEATH_ANIMATION_ID, "HipSheath"),
        ] {
            assert_eq!(name_of(id), name, "animation id {id}");
        }
    }

    /// The other half of the mistake above: a *sequence index* is not an
    /// animation id, and on the model this client draws most they disagree for
    /// nearly every cycle.
    ///
    /// Asserted rather than described, because the wrong numbers were only
    /// wrong on that distinction, and a comment saying so had already been
    /// written -- eleven lines above the two constants that ignored it.
    #[test]
    fn a_sequence_index_is_not_an_animation_id() {
        let Some(data) = std::env::var_os("WOW_DATA") else {
            eprintln!("skipping: WOW_DATA not set");
            return;
        };
        let mut chain = Chain::open_wow_data(data, "enUS").expect("opening archives");
        let bytes = chain
            .read(r"Character\Human\Male\HumanMale.m2")
            .expect("HumanMale.m2");
        let model = m2::Model::parse(&bytes).expect("parsing HumanMale.m2");
        let sequences = model.sequences();

        // The two the constants got wrong, stated as the numbers they were.
        assert_eq!(
            sequences[32].id, 89,
            "sequence 32 is Sheath, whose animation id is 89 -- reading the \
             index as the id asks for SpellCast"
        );
        assert_eq!(
            sequences[65].id, 90,
            "sequence 65 is HipSheath, whose animation id is 90 -- reading the \
             index as the id asks for EmoteTalkQuestion"
        );
        // And the general fact, so this is about the file rather than about
        // two rows: most sequences sit at an index that is not their id.
        let agreeing = sequences
            .iter()
            .enumerate()
            .filter(|(index, sequence)| *index as u16 == sequence.id)
            .count();
        assert!(
            agreeing * 4 < sequences.len(),
            "{agreeing} of {} sequences sit at their own id, which would make \
             the two readings hard to tell apart",
            sequences.len()
        );
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
