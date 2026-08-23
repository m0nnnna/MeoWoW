//! Reader for M2 models, the format used for every animated object in the
//! game: creatures, characters, doodads, weapons, spell effects.
//!
//! Written from the public format documentation at <https://wowdev.wiki/M2>.
//!
//! The header is a flat list of `(count, offset)` pairs pointing into the rest
//! of the file. Nothing is self-describing and nothing is bounds-checked by the
//! format, so every array read here is validated against the file length --
//! a malformed offset would otherwise be an out-of-bounds read on data we do
//! not control.
//!
//! **Geometry does not live in the `.m2` file.** It holds a vertex pool, but
//! the triangles that reference it are in separate `.skin` files, one per level
//! of detail. See [`skin`].

pub mod anim;
pub mod emitter;
pub mod event;
pub mod particles;
pub mod skin;

pub use anim::{Fixed16, Interpolation, Keyframe, Keyframes, Pose, Sequence, Track};
pub use emitter::{EmitterType, PartTrack, ParticleEmitter, RibbonEmitter};
pub use event::Event;
pub use particles::{Particle, ParticleSystem, RibbonTrail, Sprite};
pub use skin::Skin;

use glam::{Mat4, Quat, Vec3};

use std::fmt;

/// The version a 3.3.5a client ships. Other builds move fields around.
pub const VERSION_WOTLK: u32 = 264;

/// Bytes per vertex: position, weights, indices, normal, two UV sets.
const VERTEX_SIZE: usize = 48;
/// Bytes per `M2CompBone` in this version.
const BONE_SIZE: usize = 88;
const TEXTURE_SIZE: usize = 16;
const TEXTURE_TRANSFORM_SIZE: usize = 60;
const MATERIAL_SIZE: usize = 4;
/// Bytes per `M2Attachment`: id, bone, two bytes of padding, a position, and a
/// 20-byte visibility track.
const ATTACHMENT_SIZE: usize = 40;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not an M2 model (magic {0:?})")]
    BadMagic([u8; 4]),
    #[error("file is {got} bytes, too short for an M2 header")]
    TooShort { got: usize },
    #[error("unsupported M2 version {got} (this reader targets {VERSION_WOTLK})")]
    UnsupportedVersion { got: u32 },
    #[error(
        "{what}: {count} entries of {stride} bytes at offset {offset} runs past the \
         {len}-byte file"
    )]
    ArrayOutOfBounds {
        what: &'static str,
        count: u32,
        offset: u32,
        stride: usize,
        len: usize,
    },
}

/// A `(count, offset)` pair. Every variable-length field in the format is one
/// of these.
#[derive(Clone, Copy, Debug, Default)]
pub struct Array {
    pub count: u32,
    pub offset: u32,
}

/// Sequentially reads the fixed-layout header.
///
/// Reading in order rather than by absolute offset keeps the code honest: the
/// field order *is* the layout, so a missing field shifts everything after it
/// and shows up immediately rather than silently reading a neighbour.
struct HeaderReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> HeaderReader<'a> {
    fn u32(&mut self) -> u32 {
        let v = self
            .data
            .get(self.pos..self.pos + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .unwrap_or(0);
        self.pos += 4;
        v
    }

    fn f32(&mut self) -> f32 {
        f32::from_bits(self.u32())
    }

    fn array(&mut self) -> Array {
        Array {
            count: self.u32(),
            offset: self.u32(),
        }
    }
}

/// Header fields this reader uses. Lights, cameras and events are read past
/// but not kept -- reading in order is what keeps the emitter offsets honest.
#[derive(Debug, Default)]
struct Header {
    version: u32,
    name: Array,
    global_flags: u32,
    global_loops: Array,
    sequences: Array,
    bones: Array,
    key_bone_lookup: Array,
    vertices: Array,
    skin_profiles: u32,
    colors: Array,
    textures: Array,
    texture_weights: Array,
    texture_transforms: Array,
    materials: Array,
    bone_combos: Array,
    texture_combos: Array,
    texture_coord_combos: Array,
    texture_transform_combos: Array,
    bounding_box: [f32; 6],
    bounding_sphere_radius: f32,
    collision_box: [f32; 6],
    collision_sphere_radius: f32,
    /// The *collision* mesh, a far coarser thing than the drawn geometry --
    /// tens of triangles where the render mesh has thousands. Parsed because a
    /// fence a character walks through is not a fence; see the `collision`
    /// crate.
    collision_indices: Array,
    collision_positions: Array,
    attachments: Array,
    /// Trails: a sword swing's arc, a comet's tail. See [`emitter`].
    ribbons: Array,
    /// Flames, sparks, smoke. See [`emitter`].
    particles: Array,
    /// Timed moments inside an animation -- a footfall, a weapon connecting.
    /// See [`event`].
    events: Array,
}

/// One vertex in the model's shared pool.
#[derive(Clone, Copy, Debug)]
pub struct Vertex {
    pub position: [f32; 3],
    /// Weights of up to four influencing bones, summing to 255.
    pub bone_weights: [u8; 4],
    /// Indices into the model's bone list, via the bone combo table.
    pub bone_indices: [u8; 4],
    pub normal: [f32; 3],
    /// Two UV sets; the second is used by multi-texture materials.
    pub uv: [[f32; 2]; 2],
}

/// A texture slot. Most are filled in at runtime rather than by filename.
#[derive(Clone, Debug)]
pub struct Texture {
    /// 0 means `filename` is used directly. Anything else is a slot the client
    /// fills from elsewhere -- type 1 is the creature skin named by
    /// `CreatureDisplayInfo`, 11 to 13 are the monster skins, and so on.
    pub kind: u32,
    pub flags: u32,
    pub filename: String,
}

impl Texture {
    /// Whether the model names its own texture, rather than expecting the
    /// client to supply one.
    pub fn is_hardcoded(&self) -> bool {
        self.kind == 0 && !self.filename.is_empty()
    }
}

/// Render state for a batch: blending and the usual raster toggles.
#[derive(Clone, Copy, Debug)]
pub struct Material {
    pub flags: u16,
    pub blend: u16,
}

impl Material {
    pub fn unlit(&self) -> bool {
        self.flags & 0x01 != 0
    }
    pub fn two_sided(&self) -> bool {
        self.flags & 0x04 != 0
    }
    pub fn depth_test_disabled(&self) -> bool {
        self.flags & 0x08 != 0
    }
    pub fn depth_write_disabled(&self) -> bool {
        self.flags & 0x10 != 0
    }
}

/// A node in the skeleton. Animation tracks are not parsed yet.
#[derive(Clone, Copy, Debug)]
pub struct Bone {
    /// Well-known slot (root, head, hand, ...), or -1 for an ordinary bone.
    pub key_bone_id: i32,
    pub flags: u32,
    /// Index of the parent bone, or -1 at the root. A model may have several
    /// roots.
    pub parent: i16,
    pub submesh_id: u16,
    /// Point the bone rotates about, in model space.
    pub pivot: [f32; 3],
}

/// A point on the skeleton where a second model hangs: a weapon in a hand, a
/// shield on a forearm, a spell effect at a fingertip.
///
/// The attached model is drawn at [`Model::pose`]'s matrix for [`bone`] applied
/// to [`position`] -- so it follows the hand through the animation for free.
///
/// [`position`] is a **model-space point, not a delta from the bone**, which
/// the dump makes obvious: on every character model it is bit-for-bit the
/// pivot of the bone it names. That falls out of how a bone matrix is built
/// here (`translate(pivot) * transform * translate(-pivot)`, so the bind pose
/// is the identity rather than a translation), and it matters because treating
/// it as a delta adds the pivot in twice and puts the sword out at arm's length
/// from the hand -- a mistake that renders plausibly and is never an error.
///
/// [`bone`]: Attachment::bone
/// [`position`]: Attachment::position
#[derive(Clone, Copy, Debug)]
pub struct Attachment {
    /// Which slot this is, from a vocabulary shared by every model in the game.
    /// Only the ids confirmed against the archives are named here -- see
    /// [`Attachment::HAND_RIGHT`].
    pub id: u32,
    /// Bone this hangs from, indexing [`Model::bones`].
    pub bone: u16,
    /// Where the attached model sits, in model space. See the type's note: this
    /// is a point, not an offset.
    pub position: [f32; 3],
}

impl Attachment {
    /// Main hand.
    ///
    /// Named from the data rather than transcribed. `m2 attachments --survey`
    /// reads all 22,779 models: ids 1 and 2 are a mirrored pair sitting at the
    /// end of the two arm chains, and each keeps to its own side of the plane
    /// of symmetry (id 1 is on -Y in 684 models against 103 on +Y; id 2 is the
    /// reverse). Which side is which then follows from the one fact the
    /// renderer has already confirmed live -- an M2's forward is +X, drawn Z-up
    /// and right-handed, so the model's left is +Y and this id, on -Y, is the
    /// right hand.
    ///
    /// Corroborated independently by id 0, which sits just outboard of the +Y
    /// hand: that is the shield, and a shield is worn in the off hand.
    pub const HAND_RIGHT: u32 = 1;
    /// Off hand, the mirror of [`Attachment::HAND_RIGHT`].
    pub const HAND_LEFT: u32 = 2;
}

/// A bone together with its animation tracks.
pub struct AnimatedBone {
    pub bone: Bone,
    pub translation: Track<Vec3>,
    pub rotation: Track<Quat>,
    pub scale: Track<Vec3>,
}

#[derive(Clone, Debug)]
pub struct AnimatedTextureTransform {
    pub translation: Track<Vec3>,
    pub rotation: Track<Quat>,
    pub scale: Track<Vec3>,
}

impl AnimatedTextureTransform {
    pub fn is_animated(&self) -> bool {
        self.translation.is_animated() || self.rotation.is_animated() || self.scale.is_animated()
    }

    pub fn matrix(&self, sequence: usize, time_ms: u32, global_loops: &[u32]) -> Mat4 {
        let translation = sample_track_with_global(
            &self.translation,
            sequence,
            time_ms,
            global_loops,
            Vec3::ZERO,
        );
        let rotation = sample_track_with_global(
            &self.rotation,
            sequence,
            time_ms,
            global_loops,
            Quat::IDENTITY,
        );
        let scale = sample_track_with_global(&self.scale, sequence, time_ms, global_loops, Vec3::ONE);
        Mat4::from_translation(Vec3::new(0.5 + translation.x, 0.5 + translation.y, 0.0))
            * Mat4::from_quat(rotation)
            * Mat4::from_scale(Vec3::new(scale.x, scale.y, 1.0))
            * Mat4::from_translation(Vec3::new(-0.5, -0.5, 0.0))
    }
}

fn sample_track_with_global<T: anim::Keyframe>(
    track: &Track<T>,
    sequence: usize,
    time_ms: u32,
    global_loops: &[u32],
    fallback: T,
) -> T {
    let time_ms = track
        .global_sequence
        .and_then(|id| global_loops.get(id as usize).copied())
        .map(|duration| time_ms % duration.max(1))
        .unwrap_or(time_ms);
    track.sample(sequence, time_ms).unwrap_or(fallback)
}

impl AnimatedBone {
    /// Whether this bone moves at all.
    pub fn is_animated(&self) -> bool {
        self.translation.is_animated()
            || self.rotation.is_animated()
            || self.scale.is_animated()
    }
}

/// Orders bones so every parent is posed before its children.
///
/// M2 files normally store parents first, but that is a convention rather than
/// a guarantee, and a single out-of-order bone would silently drop its parent's
/// transform. Sorting by chain depth costs nothing at these sizes.
fn bone_order(bones: &[AnimatedBone]) -> Vec<usize> {
    let depth = |mut index: usize| {
        let mut depth = 0usize;
        // Bounded by the bone count; cycles are reported by `validate`.
        while let Ok(parent) = usize::try_from(bones[index].bone.parent) {
            if parent >= bones.len() || depth > bones.len() {
                break;
            }
            index = parent;
            depth += 1;
        }
        depth
    };
    let mut order: Vec<usize> = (0..bones.len()).collect();
    order.sort_by_key(|&i| depth(i));
    order
}

/// A parsed model.
pub struct Model {
    data: Vec<u8>,
    header: Header,
}

impl Model {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < 0x108 {
            return Err(Error::TooShort { got: bytes.len() });
        }
        if &bytes[..4] != b"MD20" {
            return Err(Error::BadMagic([bytes[0], bytes[1], bytes[2], bytes[3]]));
        }

        let mut r = HeaderReader {
            data: bytes,
            pos: 4,
        };
        let mut h = Header {
            version: r.u32(),
            ..Default::default()
        };
        if h.version != VERSION_WOTLK {
            return Err(Error::UnsupportedVersion { got: h.version });
        }

        h.name = r.array();
        h.global_flags = r.u32();
        h.global_loops = r.array();
        h.sequences = r.array();
        let _sequence_lookup = r.array();
        h.bones = r.array();
        h.key_bone_lookup = r.array();
        h.vertices = r.array();
        h.skin_profiles = r.u32();
        h.colors = r.array();
        h.textures = r.array();
        h.texture_weights = r.array();
        h.texture_transforms = r.array();
        let _replaceable_texture_lookup = r.array();
        h.materials = r.array();
        h.bone_combos = r.array();
        h.texture_combos = r.array();
        h.texture_coord_combos = r.array();
        let _texture_weight_combos = r.array();
        h.texture_transform_combos = r.array();

        for slot in &mut h.bounding_box {
            *slot = r.f32();
        }
        h.bounding_sphere_radius = r.f32();
        for slot in &mut h.collision_box {
            *slot = r.f32();
        }
        h.collision_sphere_radius = r.f32();

        h.collision_indices = r.array();
        h.collision_positions = r.array();
        // Read and dropped: the collision queries derive a face normal from
        // the three points, which cannot disagree with the winding the way a
        // stored one can.
        let _collision_normals = r.array();
        h.attachments = r.array();
        // Read through to the emitters rather than skipping to them: the field
        // order *is* the layout, and a missing array here would shift both
        // emitter blocks onto a neighbour's numbers, which parses.
        let _attachment_lookup = r.array();
        h.events = r.array();
        let _lights = r.array();
        let _cameras = r.array();
        let _camera_lookup = r.array();
        h.ribbons = r.array();
        h.particles = r.array();
        // `texture_combiner_combos` follows when `global_flags & 0x8`, and is
        // the last field. Nothing here reads it.

        let model = Self {
            data: bytes.to_vec(),
            header: h,
        };

        // Validate up front so later accessors cannot fail: a bad offset is a
        // property of the file, not of the call that happens to hit it.
        model.slice("vertices", model.header.vertices, VERTEX_SIZE)?;
        model.slice("bones", model.header.bones, BONE_SIZE)?;
        model.slice("textures", model.header.textures, TEXTURE_SIZE)?;
        model.slice("materials", model.header.materials, MATERIAL_SIZE)?;
        model.slice("texture_combos", model.header.texture_combos, 2)?;
        model.slice("bone_combos", model.header.bone_combos, 2)?;
        model.slice("attachments", model.header.attachments, ATTACHMENT_SIZE)?;
        model.slice("ribbons", model.header.ribbons, emitter::RIBBON_SIZE)?;
        model.slice("particles", model.header.particles, emitter::PARTICLE_SIZE)?;
        model.slice("events", model.header.events, event::EVENT_SIZE)?;
        Ok(model)
    }

    fn slice(&self, what: &'static str, array: Array, stride: usize) -> Result<&[u8], Error> {
        let start = array.offset as usize;
        let len = array.count as usize * stride;
        self.data
            .get(start..start + len)
            .ok_or(Error::ArrayOutOfBounds {
                what,
                count: array.count,
                offset: array.offset,
                stride,
                len: self.data.len(),
            })
    }

    fn u32_at(&self, offset: usize) -> u32 {
        self.data
            .get(offset..offset + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .unwrap_or(0)
    }

    fn f32_at(&self, offset: usize) -> f32 {
        f32::from_bits(self.u32_at(offset))
    }

    fn byte_at(&self, offset: usize) -> u8 {
        self.data.get(offset).copied().unwrap_or(0)
    }

    pub fn version(&self) -> u32 {
        self.header.version
    }

    pub fn global_flags(&self) -> u32 {
        self.header.global_flags
    }

    /// Internal model name, which is not always the filename.
    pub fn name(&self) -> &str {
        let start = self.header.name.offset as usize;
        // The stored length includes the terminating NUL.
        let len = (self.header.name.count as usize).saturating_sub(1);
        self.data
            .get(start..start + len)
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or("")
    }

    /// Number of `.skin` files this model expects.
    pub fn skin_count(&self) -> u32 {
        self.header.skin_profiles
    }

    pub fn sequence_count(&self) -> u32 {
        self.header.sequences.count
    }

    pub fn bounding_box(&self) -> ([f32; 3], [f32; 3]) {
        let b = self.header.bounding_box;
        ([b[0], b[1], b[2]], [b[3], b[4], b[5]])
    }

    pub fn bounding_sphere_radius(&self) -> f32 {
        self.header.bounding_sphere_radius
    }

    pub fn collision_box(&self) -> ([f32; 3], [f32; 3]) {
        let b = self.header.collision_box;
        ([b[0], b[1], b[2]], [b[3], b[4], b[5]])
    }

    pub fn vertex_count(&self) -> usize {
        self.header.vertices.count as usize
    }

    pub fn vertices(&self) -> Vec<Vertex> {
        let Ok(raw) = self.slice("vertices", self.header.vertices, VERTEX_SIZE) else {
            return Vec::new();
        };
        raw.chunks_exact(VERTEX_SIZE)
            .map(|v| {
                let f = |o: usize| f32::from_le_bytes(v[o..o + 4].try_into().unwrap());
                Vertex {
                    position: [f(0), f(4), f(8)],
                    bone_weights: [v[12], v[13], v[14], v[15]],
                    bone_indices: [v[16], v[17], v[18], v[19]],
                    normal: [f(20), f(24), f(28)],
                    uv: [[f(32), f(36)], [f(40), f(44)]],
                }
            })
            .collect()
    }

    pub fn textures(&self) -> Vec<Texture> {
        let Ok(raw) = self.slice("textures", self.header.textures, TEXTURE_SIZE) else {
            return Vec::new();
        };
        raw.chunks_exact(TEXTURE_SIZE)
            .map(|t| {
                let word = |o: usize| u32::from_le_bytes(t[o..o + 4].try_into().unwrap());
                let (kind, flags) = (word(0), word(4));
                let (len, offset) = (word(8) as usize, word(12) as usize);
                // The stored length counts the NUL terminator.
                let filename = self
                    .data
                    .get(offset..offset + len.saturating_sub(1))
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .unwrap_or("")
                    .to_string();
                Texture {
                    kind,
                    flags,
                    filename,
                }
            })
            .collect()
    }

    pub fn materials(&self) -> Vec<Material> {
        let Ok(raw) = self.slice("materials", self.header.materials, MATERIAL_SIZE) else {
            return Vec::new();
        };
        raw.chunks_exact(MATERIAL_SIZE)
            .map(|m| Material {
                flags: u16::from_le_bytes([m[0], m[1]]),
                blend: u16::from_le_bytes([m[2], m[3]]),
            })
            .collect()
    }

    pub fn global_sequence_durations(&self) -> Vec<u32> {
        self.slice("global_loops", self.header.global_loops, 4)
            .map(|raw| {
                raw.chunks_exact(4)
                    .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn bones(&self) -> Vec<Bone> {
        let Ok(raw) = self.slice("bones", self.header.bones, BONE_SIZE) else {
            return Vec::new();
        };
        raw.chunks_exact(BONE_SIZE)
            .map(|b| {
                let f = |o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
                Bone {
                    key_bone_id: i32::from_le_bytes(b[0..4].try_into().unwrap()),
                    flags: u32::from_le_bytes(b[4..8].try_into().unwrap()),
                    parent: i16::from_le_bytes(b[8..10].try_into().unwrap()),
                    submesh_id: u16::from_le_bytes([b[10], b[11]]),
                    // Three 20-byte animation tracks sit between the header
                    // fields and the pivot.
                    pivot: [f(76), f(80), f(84)],
                }
            })
            .collect()
    }

    /// Reads an `M2Track` at an absolute offset.
    ///
    /// A track's timestamps and values are *arrays of arrays*: the outer array
    /// has one entry per sequence, and each entry is itself a
    /// `(count, offset)` pair. Reading the outer array as the keyframes
    /// directly is the obvious mistake, and yields plausible-looking garbage.
    fn read_track<T: anim::Keyframe>(
        &self,
        base: usize,
        external: &std::collections::BTreeMap<usize, Vec<u8>>,
        inline: &[bool],
    ) -> anim::Track<T> {
        let h = |o: usize| {
            self.data
                .get(o..o + 2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .unwrap_or(0)
        };
        let array_at = |o: usize| Array {
            count: self.u32_at(o),
            offset: self.u32_at(o + 4),
        };

        let global = h(base + 2);
        let times_outer = array_at(base + 4);
        let values_outer = array_at(base + 12);

        // The two outer arrays are parallel; a mismatch means we are misreading
        // the layout, so take the shorter and let validation notice.
        let count = times_outer.count.min(values_outer.count) as usize;
        let mut sequences = Vec::with_capacity(count);
        for i in 0..count {
            // A sequence that is neither inline nor externally supplied has no
            // data here at all: its offsets address a file we were not given,
            // and reading them from the .m2 yields whatever happens to sit
            // there. Empty is the honest answer, and lets alias resolution or
            // the bind pose take over.
            if !inline.get(i).copied().unwrap_or(true) && !external.contains_key(&i) {
                sequences.push(anim::Keyframes {
                    times: Vec::new(),
                    values: Vec::new(),
                });
                continue;
            }

            let t = array_at(times_outer.offset as usize + i * 8);
            let v = array_at(values_outer.offset as usize + i * 8);

            // The outer array lives in the .m2, but for a sequence whose data
            // moved to an external file the inner offsets address *that* file.
            let source: &[u8] = external.get(&i).map(Vec::as_slice).unwrap_or(&self.data);
            let word = |at: usize| {
                source
                    .get(at..at + 4)
                    .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                    .unwrap_or(0)
            };

            let times: Vec<u32> = (0..t.count as usize)
                .map(|k| word(t.offset as usize + k * 4))
                .collect();
            let values: Vec<T> = (0..v.count as usize)
                .filter_map(|k| {
                    let at = v.offset as usize + k * T::SIZE;
                    source.get(at..at + T::SIZE).map(T::read)
                })
                .collect();

            sequences.push(anim::Keyframes { times, values });
        }

        anim::Track {
            interpolation: anim::Interpolation::from_raw(h(base)),
            // 0xFFFF means "no global sequence".
            global_sequence: (global != u16::MAX).then_some(global),
            sequences,
        }
    }

    /// The model's animations.
    pub fn sequences(&self) -> Vec<Sequence> {
        self.slice("sequences", self.header.sequences, Sequence::SIZE)
            .map(|raw| raw.chunks_exact(Sequence::SIZE).map(Sequence::read).collect())
            .unwrap_or_default()
    }

    /// Bones with their animation tracks decoded.
    ///
    /// Separate from [`Model::bones`] because decoding every track is far more
    /// work than reading the hierarchy, and most callers only want the latter.
    pub fn animated_bones(&self) -> Vec<AnimatedBone> {
        self.animated_bones_with(&Default::default())
    }

    pub fn animated_texture_transforms(&self) -> Vec<AnimatedTextureTransform> {
        self.animated_texture_transforms_with(&Default::default())
    }

    pub fn animated_texture_transforms_with(
        &self,
        external: &std::collections::BTreeMap<usize, Vec<u8>>,
    ) -> Vec<AnimatedTextureTransform> {
        let Ok(raw) = self.slice(
            "texture_transforms",
            self.header.texture_transforms,
            TEXTURE_TRANSFORM_SIZE,
        ) else {
            return Vec::new();
        };
        let inline: Vec<bool> = self.sequences().iter().map(|s| s.is_inline()).collect();
        (0..raw.len() / TEXTURE_TRANSFORM_SIZE)
            .map(|i| {
                let base = self.header.texture_transforms.offset as usize
                    + i * TEXTURE_TRANSFORM_SIZE;
                AnimatedTextureTransform {
                    translation: self.read_track(base, external, &inline),
                    rotation: self.read_track(base + 20, external, &inline),
                    scale: self.read_track(base + 40, external, &inline),
                }
            })
            .collect()
    }

    /// Bones with tracks decoded, using externally loaded `.anim` data for the
    /// sequences that need it.
    ///
    /// Keyed by sequence index. Sequences without [`Sequence::is_inline`] have
    /// no usable data in the `.m2`; see [`anim::external_anim_path`].
    pub fn animated_bones_with(
        &self,
        external: &std::collections::BTreeMap<usize, Vec<u8>>,
    ) -> Vec<AnimatedBone> {
        let Ok(raw) = self.slice("bones", self.header.bones, BONE_SIZE) else {
            return Vec::new();
        };
        let base = self.header.bones.offset as usize;
        let inline: Vec<bool> = self.sequences().iter().map(|s| s.is_inline()).collect();

        let mut bones: Vec<AnimatedBone> = self
            .bones()
            .into_iter()
            .enumerate()
            .map(|(i, bone)| {
                let at = base + i * BONE_SIZE;
                debug_assert!(raw.len() >= (i + 1) * BONE_SIZE);
                AnimatedBone {
                    bone,
                    // Three 20-byte tracks between the header fields and pivot.
                    translation: self.read_track(at + 16, external, &inline),
                    rotation: self.read_track(at + 36, external, &inline),
                    scale: self.read_track(at + 56, external, &inline),
                }
            })
            .collect();

        self.resolve_aliases(&mut bones);
        bones
    }

    /// Points alias sequences at the keyframes they borrow.
    ///
    /// A bare `0x40` sequence has neither inline data nor an external file; it
    /// names another entry through `alias_next`. Left unresolved it samples to
    /// nothing and the model snaps to bind pose.
    fn resolve_aliases(&self, bones: &mut [AnimatedBone]) {
        let sequences = self.sequences();

        // Follow the chain to the first entry that is not itself an alias,
        // bounded so a malformed cycle cannot hang the loader.
        let target = |start: usize| -> Option<usize> {
            let mut i = start;
            for _ in 0..8 {
                let s = sequences.get(i)?;
                if !s.is_alias() {
                    return Some(i);
                }
                let next = s.alias_next as usize;
                if next == i || next >= sequences.len() {
                    return None;
                }
                i = next;
            }
            None
        };

        let redirects: Vec<(usize, usize)> = (0..sequences.len())
            .filter(|&i| sequences[i].is_alias())
            .filter_map(|i| target(i).filter(|&t| t != i).map(|t| (i, t)))
            .collect();

        for bone in bones {
            for &(from, to) in &redirects {
                // Only fill genuinely empty slots: an alias may also be inline
                // (`0x60`) and carry perfectly good keys of its own.
                macro_rules! fill {
                    ($track:expr) => {
                        if let (Some(true), Some(source)) = (
                            $track.sequences.get(from).map(|k| k.values.is_empty()),
                            $track.sequences.get(to).cloned(),
                        ) {
                            if !source.values.is_empty() {
                                $track.sequences[from] = source;
                            }
                        }
                    };
                }
                fill!(bone.translation);
                fill!(bone.rotation);
                fill!(bone.scale);
            }
        }
    }

    /// Computes model-space matrices for every bone at a point in time.
    ///
    /// `time_ms` is relative to the start of the sequence; callers wrap it to
    /// the sequence's duration.
    pub fn pose(&self, bones: &[AnimatedBone], sequence: usize, time_ms: u32) -> Pose {
        Self::pose_bones(bones, sequence, time_ms)
    }

    /// Poses a skeleton without needing the model it came from.
    pub fn pose_bones(bones: &[AnimatedBone], sequence: usize, time_ms: u32) -> Pose {
        Self::pose_bones_with_global_loops(bones, sequence, time_ms, &[])
    }

    pub fn pose_bones_with_global_loops(
        bones: &[AnimatedBone],
        sequence: usize,
        time_ms: u32,
        global_loops: &[u32],
    ) -> Pose {
        let mut out = vec![Mat4::IDENTITY; bones.len()];
        for &i in &bone_order(bones) {
            let b = &bones[i];
            let translation =
                sample_track_with_global(&b.translation, sequence, time_ms, global_loops, Vec3::ZERO);
            let rotation =
                sample_track_with_global(&b.rotation, sequence, time_ms, global_loops, Quat::IDENTITY);
            let scale = sample_track_with_global(&b.scale, sequence, time_ms, global_loops, Vec3::ONE);

            let local = anim::local_transform(
                Vec3::from(b.bone.pivot),
                translation,
                rotation,
                scale,
            );
            out[i] = match usize::try_from(b.bone.parent) {
                Ok(parent) if parent < bones.len() => out[parent] * local,
                _ => local,
            };
        }
        out
    }

    /// Crossfades two sampled skeletons before composing their parent chains.
    pub fn blend_bones(
        bones: &[AnimatedBone],
        from_sequence: usize,
        from_time_ms: u32,
        to_sequence: usize,
        to_time_ms: u32,
        t: f32,
    ) -> Pose {
        Self::blend_bones_with_global_loops(
            bones,
            from_sequence,
            from_time_ms,
            to_sequence,
            to_time_ms,
            t,
            &[],
        )
    }

    pub fn blend_bones_with_global_loops(
        bones: &[AnimatedBone],
        from_sequence: usize,
        from_time_ms: u32,
        to_sequence: usize,
        to_time_ms: u32,
        t: f32,
        global_loops: &[u32],
    ) -> Pose {
        let t = t.clamp(0.0, 1.0);
        let mut out = vec![Mat4::IDENTITY; bones.len()];
        for &i in &bone_order(bones) {
            let b = &bones[i];
            let from_translation = sample_track_with_global(
                &b.translation,
                from_sequence,
                from_time_ms,
                global_loops,
                Vec3::ZERO,
            );
            let from_rotation = sample_track_with_global(
                &b.rotation,
                from_sequence,
                from_time_ms,
                global_loops,
                Quat::IDENTITY,
            );
            let from_scale = sample_track_with_global(
                &b.scale,
                from_sequence,
                from_time_ms,
                global_loops,
                Vec3::ONE,
            );
            let to_translation = sample_track_with_global(
                &b.translation,
                to_sequence,
                to_time_ms,
                global_loops,
                Vec3::ZERO,
            );
            let to_rotation = sample_track_with_global(
                &b.rotation,
                to_sequence,
                to_time_ms,
                global_loops,
                Quat::IDENTITY,
            );
            let to_scale = sample_track_with_global(
                &b.scale,
                to_sequence,
                to_time_ms,
                global_loops,
                Vec3::ONE,
            );

            let local = anim::local_transform(
                Vec3::from(b.bone.pivot),
                from_translation.lerp(to_translation, t),
                from_rotation.slerp(to_rotation, t),
                from_scale.lerp(to_scale, t),
            );
            out[i] = match usize::try_from(b.bone.parent) {
                Ok(parent) if parent < bones.len() => out[parent] * local,
                _ => local,
            };
        }
        out
    }

    fn u16_table(&self, what: &'static str, array: Array) -> Vec<u16> {
        self.slice(what, array, 2)
            .map(|raw| {
                raw.chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Maps a batch's texture slot to an index in [`Model::textures`].
    pub fn texture_combos(&self) -> Vec<u16> {
        self.u16_table("texture_combos", self.header.texture_combos)
    }

    pub fn texture_coord_combos(&self) -> Vec<u16> {
        self.u16_table("texture_coord_combos", self.header.texture_coord_combos)
    }

    pub fn texture_transform_combos(&self) -> Vec<u16> {
        self.u16_table(
            "texture_transform_combos",
            self.header.texture_transform_combos,
        )
    }

    /// Maps a submesh's local bone slots to indices in [`Model::bones`].
    pub fn bone_combos(&self) -> Vec<u16> {
        self.u16_table("bone_combos", self.header.bone_combos)
    }

    pub fn attachment_count(&self) -> u32 {
        self.header.attachments.count
    }

    /// Where other models hang off this one.
    pub fn attachments(&self) -> Vec<Attachment> {
        let Ok(raw) = self.slice("attachments", self.header.attachments, ATTACHMENT_SIZE) else {
            return Vec::new();
        };
        raw.chunks_exact(ATTACHMENT_SIZE)
            .map(|a| {
                let f = |o: usize| f32::from_le_bytes(a[o..o + 4].try_into().unwrap());
                Attachment {
                    id: u32::from_le_bytes(a[0..4].try_into().unwrap()),
                    bone: u16::from_le_bytes([a[4], a[5]]),
                    // Bytes 6..8 are padding that keeps the position aligned.
                    position: [f(8), f(12), f(16)],
                }
            })
            .collect()
    }

    /// The model's timed events, with their timestamps decoded.
    ///
    /// See [`Model::events_with`] for why the external map matters.
    pub fn events(&self) -> Vec<Event> {
        self.events_with(&Default::default())
    }

    /// The model's timed events, reading the sequences whose data moved to an
    /// external `.anim` file out of the bytes supplied for them.
    ///
    /// **A character's walk cycle is one of those sequences**, so a footfall
    /// read without the external files has an empty timestamp list exactly
    /// where the interesting one is -- which looks like a model that carries
    /// the event and never fires it. Same trap as [`Model::animated_bones`],
    /// and it is handled the same way.
    pub fn events_with(
        &self,
        external: &std::collections::BTreeMap<usize, Vec<u8>>,
    ) -> Vec<Event> {
        let Ok(raw) = self.slice("events", self.header.events, event::EVENT_SIZE) else {
            return Vec::new();
        };
        let inline: Vec<bool> = self.sequences().iter().map(|s| s.is_inline()).collect();
        let base = self.header.events.offset as usize;
        (0..raw.len() / event::EVENT_SIZE)
            .map(|i| {
                let at = base + i * event::EVENT_SIZE;
                let f = |o: usize| self.f32_at(o);
                Event {
                    identifier: [
                        self.byte_at(at),
                        self.byte_at(at + 1),
                        self.byte_at(at + 2),
                        self.byte_at(at + 3),
                    ],
                    data: self.u32_at(at + 4),
                    bone: self.u32_at(at + 8),
                    position: [f(at + 12), f(at + 16), f(at + 20)],
                    // An `M2TrackBase` is a `Track` with the values array
                    // removed: interpolation, global sequence, then one
                    // `(count, offset)` outer array of timestamp lists. The
                    // outer array therefore sits at +28, not at +24 where a
                    // full track's does.
                    times: self.read_timestamps(at + 28, external, &inline),
                }
            })
            .collect()
    }

    /// Reads an `M2TrackBase`'s per-sequence timestamp lists.
    ///
    /// Deliberately not [`Model::read_track`] with a dummy value type: that
    /// function takes the *shorter* of the two parallel outer arrays, and here
    /// there is no second array to be shorter than. Passing a zero-sized value
    /// through it would silently produce no sequences at all.
    fn read_timestamps(
        &self,
        outer_at: usize,
        external: &std::collections::BTreeMap<usize, Vec<u8>>,
        inline: &[bool],
    ) -> Vec<Vec<u32>> {
        let outer = Array {
            count: self.u32_at(outer_at),
            offset: self.u32_at(outer_at + 4),
        };
        (0..outer.count as usize)
            .map(|i| {
                if !inline.get(i).copied().unwrap_or(true) && !external.contains_key(&i) {
                    return Vec::new();
                }
                let inner = Array {
                    count: self.u32_at(outer.offset as usize + i * 8),
                    offset: self.u32_at(outer.offset as usize + i * 8 + 4),
                };
                let source: &[u8] = external.get(&i).map(Vec::as_slice).unwrap_or(&self.data);
                (0..inner.count as usize)
                    .map(|k| {
                        let at = inner.offset as usize + k * 4;
                        source
                            .get(at..at + 4)
                            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                            .unwrap_or(0)
                    })
                    .collect()
            })
            .collect()
    }

    pub fn particle_emitter_count(&self) -> u32 {
        self.header.particles.count
    }

    pub fn ribbon_emitter_count(&self) -> u32 {
        self.header.ribbons.count
    }

    /// Reads an `M2PartTrack`: a curve over one particle's own life.
    ///
    /// Two `(count, offset)` pairs and nothing else -- no interpolation type,
    /// no global sequence, no per-sequence outer array. The timestamps are
    /// `u16` fractions of a lifetime rather than milliseconds, which is the
    /// whole difference from [`Model::read_track`] and the thing that makes
    /// reading one as the other silently wrong. See [`emitter::PartTrack`].
    fn read_part_track<T: anim::Keyframe>(&self, base: usize) -> emitter::PartTrack<T> {
        let times = Array {
            count: self.u32_at(base),
            offset: self.u32_at(base + 4),
        };
        let values = Array {
            count: self.u32_at(base + 8),
            offset: self.u32_at(base + 12),
        };
        // The two are parallel by construction. Taking the shorter means a
        // misread layout produces a short track rather than reading values out
        // of whatever follows.
        let count = times.count.min(values.count) as usize;
        emitter::PartTrack {
            times: (0..count)
                .map(|i| {
                    let at = times.offset as usize + i * 2;
                    self.data
                        .get(at..at + 2)
                        .map(|b| anim::fixed16(u16::from_le_bytes([b[0], b[1]])))
                        .unwrap_or(0.0)
                })
                .collect(),
            values: (0..count)
                .filter_map(|i| {
                    let at = values.offset as usize + i * T::SIZE;
                    self.data.get(at..at + T::SIZE).map(T::read)
                })
                .collect(),
        }
    }

    /// Which sequences carry their keyframes in this file, for track reads.
    fn inline_sequences(&self) -> Vec<bool> {
        self.sequences().iter().map(|s| s.is_inline()).collect()
    }

    /// The model's particle emitters: flames, sparks, smoke.
    ///
    /// Empty for nearly every model. See [`emitter`] for what a record means
    /// and how its stride was measured.
    pub fn particle_emitters(&self) -> Vec<ParticleEmitter> {
        let Ok(raw) = self.slice("particles", self.header.particles, emitter::PARTICLE_SIZE) else {
            return Vec::new();
        };
        let base = self.header.particles.offset as usize;
        let inline = self.inline_sequences();
        let external = Default::default();

        (0..raw.len() / emitter::PARTICLE_SIZE)
            .map(|i| {
                let at = base + i * emitter::PARTICLE_SIZE;
                let f = |o: usize| self.f32_at(at + o);
                let h = |o: usize| {
                    self.data
                        .get(at + o..at + o + 2)
                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                        .unwrap_or(0)
                };
                let byte = |o: usize| self.data.get(at + o).copied().unwrap_or(0);
                let track = |o: usize| self.read_track::<f32>(at + o, &external, &inline);

                ParticleEmitter {
                    id: self.u32_at(at) as i32,
                    flags: self.u32_at(at + 4),
                    position: [f(8), f(12), f(16)],
                    bone: h(20),
                    texture: h(22),
                    // 24..40 are the two model filenames a "geometry" emitter
                    // spawns whole models from. Unread: nothing here draws one,
                    // and a path that is read and ignored reads as supported.
                    blend: byte(40),
                    emitter_type: emitter::EmitterType::from_raw(byte(41)),
                    color_index: h(42),
                    particle_type: byte(44),
                    head_or_tail: byte(45),
                    texture_tile_rotation: h(46) as i16,
                    rows: h(48),
                    columns: h(50),

                    emission_speed: track(52),
                    speed_variation: track(72),
                    vertical_range: track(92),
                    horizontal_range: track(112),
                    gravity: track(132),
                    lifespan: track(152),
                    lifespan_vary: f(172),
                    emission_rate: track(176),
                    emission_rate_vary: f(196),
                    emission_area_length: track(200),
                    emission_area_width: track(220),
                    z_source: track(240),

                    color: self.read_part_track(at + 260),
                    alpha: {
                        let raw: emitter::PartTrack<anim::Fixed16> =
                            self.read_part_track(at + 276);
                        emitter::PartTrack {
                            times: raw.times,
                            values: raw.values.into_iter().map(|v| v.0).collect(),
                        }
                    },
                    scale: self.read_part_track(at + 292),
                    scale_vary: [f(308), f(312)],
                    head_cell: self.read_part_track(at + 316),
                    tail_cell: self.read_part_track(at + 332),

                    tail_length: f(348),
                    twinkle_speed: f(352),
                    twinkle_percent: f(356),
                    twinkle_scale: [f(360), f(364)],
                    burst_multiplier: f(368),
                    drag: f(372),
                    base_spin: f(376),
                    base_spin_vary: f(380),
                    spin: f(384),
                    spin_vary: f(388),
                    tumble: [f(392), f(396), f(400), f(404), f(408), f(412)],
                    wind: [f(416), f(420), f(424)],
                    wind_time: f(428),
                    follow_speed: [f(432), f(440)],
                    follow_scale: [f(436), f(444)],
                    spline_points: {
                        let array = Array {
                            count: self.u32_at(at + 448),
                            offset: self.u32_at(at + 452),
                        };
                        (0..array.count as usize)
                            .map(|k| {
                                let p = array.offset as usize + k * 12;
                                [self.f32_at(p), self.f32_at(p + 4), self.f32_at(p + 8)]
                            })
                            .collect()
                    },
                    enabled_in: self.read_track::<u8>(at + 456, &external, &inline),
                }
            })
            .collect()
    }

    /// The model's ribbon emitters: trails behind a moving bone.
    pub fn ribbon_emitters(&self) -> Vec<RibbonEmitter> {
        let Ok(raw) = self.slice("ribbons", self.header.ribbons, emitter::RIBBON_SIZE) else {
            return Vec::new();
        };
        let base = self.header.ribbons.offset as usize;
        let inline = self.inline_sequences();
        let external = Default::default();

        (0..raw.len() / emitter::RIBBON_SIZE)
            .map(|i| {
                let at = base + i * emitter::RIBBON_SIZE;
                let f = |o: usize| self.f32_at(at + o);
                let h = |o: usize| {
                    self.data
                        .get(at + o..at + o + 2)
                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                        .unwrap_or(0)
                };
                let indices = |o: usize| {
                    let array = Array {
                        count: self.u32_at(at + o),
                        offset: self.u32_at(at + o + 4),
                    };
                    self.u16_table("ribbon indices", array)
                };

                RibbonEmitter {
                    id: self.u32_at(at) as i32,
                    bone: self.u32_at(at + 4) as u16,
                    position: [f(8), f(12), f(16)],
                    textures: indices(20),
                    materials: indices(28),
                    color: self.read_track::<[f32; 3]>(at + 36, &external, &inline),
                    alpha: {
                        let raw: anim::Track<anim::Fixed16> =
                            self.read_track(at + 56, &external, &inline);
                        anim::Track {
                            interpolation: raw.interpolation,
                            global_sequence: raw.global_sequence,
                            sequences: raw
                                .sequences
                                .into_iter()
                                .map(|k| anim::Keyframes {
                                    times: k.times,
                                    values: k.values.into_iter().map(|v| v.0).collect(),
                                })
                                .collect(),
                        }
                    },
                    height_above: self.read_track::<f32>(at + 76, &external, &inline),
                    height_below: self.read_track::<f32>(at + 96, &external, &inline),
                    edges_per_second: f(116),
                    edge_lifetime: f(120),
                    gravity: f(124),
                    rows: h(128),
                    columns: h(130),
                    texture_slot: self.read_track::<u16>(at + 132, &external, &inline),
                    visibility: self.read_track::<u8>(at + 152, &external, &inline),
                    // 172..176 are `priorityPlane` and two bytes of padding,
                    // which is what makes this record 176 rather than 172.
                }
            })
            .collect()
    }

    /// The collision mesh, as triangles in model space.
    ///
    /// Empty for the great many models that carry none -- a torch, a bush, a
    /// tuft of grass -- which is a real answer rather than a failure: those
    /// are things the original lets a character walk through too.
    ///
    /// **Its own mesh, not the drawn one.** A tree's render geometry is
    /// thousands of triangles of leaves; its collision is a handful around the
    /// trunk. Using the drawn mesh would be both far slower and wrong, because
    /// it would make the foliage solid.
    pub fn collision_triangles(&self) -> Vec<[[f32; 3]; 3]> {
        let (Ok(indices), Ok(positions)) = (
            self.slice("collision indices", self.header.collision_indices, 2),
            self.slice("collision positions", self.header.collision_positions, 12),
        ) else {
            return Vec::new();
        };
        let point = |i: usize| -> Option<[f32; 3]> {
            let o = i.checked_mul(12)?;
            let raw = positions.get(o..o + 12)?;
            let f = |k: usize| f32::from_le_bytes(raw[k..k + 4].try_into().unwrap());
            Some([f(0), f(4), f(8)])
        };
        indices
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]) as usize)
            .collect::<Vec<_>>()
            .chunks_exact(3)
            // An index past the end of the position array is dropped rather
            // than clamped: a triangle built from the wrong vertex is an
            // invisible wall in the wrong place, which is worse than a gap.
            .filter_map(|t| Some([point(t[0])?, point(t[1])?, point(t[2])?]))
            .collect()
    }

    /// The attachment with a given id, if this model has one.
    ///
    /// Ids are sparse and unordered -- a model with a right hand need not have
    /// a left, and the array is not indexed by id -- so this is a search rather
    /// than a lookup.
    pub fn attachment(&self, id: u32) -> Option<Attachment> {
        self.attachments().into_iter().find(|a| a.id == id)
    }

    pub fn color_count(&self) -> u32 {
        self.header.colors.count
    }

    pub fn texture_weight_count(&self) -> u32 {
        self.header.texture_weights.count
    }

    pub fn texture_transform_count(&self) -> u32 {
        self.header.texture_transforms.count
    }

    /// Bones whose parent index is out of range, which would break a skeleton
    /// walk. Should always be empty; kept as a cheap integrity check.
    pub fn invalid_parents(&self) -> Vec<(usize, i16)> {
        let bones = self.bones();
        bones
            .iter()
            .enumerate()
            .filter(|(_, b)| b.parent >= 0 && b.parent as usize >= bones.len())
            .map(|(i, b)| (i, b.parent))
            .collect()
    }

    /// Structural checks on the decoded data, as opposed to the header.
    ///
    /// These exist because a wrong field offset produces a model that parses
    /// perfectly and is quietly nonsense. Normals are the sharpest probe: they
    /// are unit vectors by construction, so if the vertex stride or the offset
    /// within it is wrong, their lengths stop being 1 immediately. Bone weights
    /// summing to 255 is the same idea one field over.
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();
        let vertices = self.vertices();

        let mut bad_normals = 0usize;
        let mut bad_weights = 0usize;
        let mut non_finite = 0usize;
        for v in &vertices {
            let len2: f32 = v.normal.iter().map(|c| c * c).sum();
            // Some models legitimately store a zero normal on degenerate
            // vertices, so only non-zero normals are required to be unit.
            if len2 > 0.01 && !(0.96..=1.04).contains(&len2) {
                bad_normals += 1;
            }
            let sum: u32 = v.bone_weights.iter().map(|&w| w as u32).sum();
            if sum != 0 && !(250..=260).contains(&sum) {
                bad_weights += 1;
            }
            if v.position.iter().any(|c| !c.is_finite()) {
                non_finite += 1;
            }
        }

        if bad_normals * 20 > vertices.len() {
            issues.push(format!(
                "{bad_normals}/{} normals are not unit length",
                vertices.len()
            ));
        }
        if bad_weights * 20 > vertices.len() {
            issues.push(format!(
                "{bad_weights}/{} bone weight sets do not sum to 255",
                vertices.len()
            ));
        }
        if non_finite > 0 {
            issues.push(format!("{non_finite} vertex positions are not finite"));
        }

        let bones = self.bones();
        let orphans = self.invalid_parents();
        if !orphans.is_empty() {
            issues.push(format!(
                "{} bones have out-of-range parents (e.g. bone {} -> {})",
                orphans.len(),
                orphans[0].0,
                orphans[0].1
            ));
        }
        // A cycle would hang any skeleton walk, so prove each chain terminates.
        for (i, _) in bones.iter().enumerate() {
            let mut steps = 0;
            let mut cur = i as i16;
            while cur >= 0 {
                let Some(b) = bones.get(cur as usize) else { break };
                cur = b.parent;
                steps += 1;
                if steps > bones.len() {
                    issues.push(format!("bone {i} sits in a parent cycle"));
                    break;
                }
            }
            if !issues.is_empty() && issues.last().unwrap().contains("cycle") {
                break;
            }
        }

        // The sharpest available probe on the attachment stride: every entry
        // names a bone, so a wrong stride walks into the middle of the next
        // record and the "bone" it reads is a float's low half. Those are
        // enormous or zero, and out of range immediately.
        let attachments = self.attachments();
        let stray = attachments
            .iter()
            .filter(|a| a.bone as usize >= bones.len())
            .count();
        if stray > 0 {
            issues.push(format!(
                "{stray}/{} attachments name a bone outside the {}-bone skeleton",
                attachments.len(),
                bones.len()
            ));
        }
        // Same idea one field over, measured against the model's *own* extent
        // rather than a constant. An attachment is a point on the model, so it
        // belongs inside the box the model already declares -- with room to
        // spare, since the box covers the bind pose and an attachment can sit
        // a little outside it.
        //
        // A fixed ceiling was tried first and was wrong in the direction that
        // matters: `Creature\TREE\AshenvaleTreeFalling01.m2` is a hundred and
        // fifty units tall, so its perfectly good attachment at Z=127 tripped
        // a limit chosen with characters in mind. A threshold that scales with
        // the subject cannot make that mistake.
        let (min, max) = self.bounding_box();
        let extent = (0..3)
            .map(|i| (max[i] - min[i]).abs())
            .fold(0.0f32, f32::max)
            .max(10.0);
        let wild = attachments
            .iter()
            .filter(|a| {
                a.position
                    .iter()
                    .any(|c| !c.is_finite() || c.abs() > extent * 4.0)
            })
            .count();
        if wild > 0 {
            issues.push(format!(
                "{wild}/{} attachments sit far outside the model's own {extent:.0}-unit extent",
                attachments.len()
            ));
        }

        issues
    }

    /// Reads a `f32` from an arbitrary offset, for exploratory work.
    pub fn raw_f32(&self, offset: usize) -> f32 {
        self.f32_at(offset)
    }
}

impl fmt::Debug for Model {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Model")
            .field("name", &self.name())
            .field("version", &self.header.version)
            .field("vertices", &self.header.vertices.count)
            .field("bones", &self.header.bones.count)
            .field("textures", &self.header.textures.count)
            .field("sequences", &self.header.sequences.count)
            .field("skins", &self.header.skin_profiles)
            .finish()
    }
}

/// Converts a model path from the form the DBCs use into the one the archives
/// actually contain.
///
/// `CreatureModelData` stores `.mdx` paths -- a leftover from the Warcraft III
/// era -- while the files shipped since Burning Crusade are `.m2`.
pub fn model_path(dbc_path: &str) -> String {
    let lower = dbc_path.to_lowercase();
    if let Some(stem) = lower.strip_suffix(".mdx") {
        format!("{}.m2", &dbc_path[..stem.len()])
    } else if lower.ends_with(".m2") {
        dbc_path.to_string()
    } else {
        format!("{dbc_path}.m2")
    }
}

/// Path of a model's `.skin` file for a given level of detail.
///
/// Skins are siblings of the model with a two-digit suffix: `Foo.m2` is
/// accompanied by `Foo00.skin` through `Foo03.skin`.
pub fn skin_path(model_path: &str, lod: u32) -> String {
    let stem = model_path
        .strip_suffix(".m2")
        .or_else(|| model_path.strip_suffix(".M2"))
        .unwrap_or(model_path);
    format!("{stem}{lod:02}.skin")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_mdx_paths_to_m2() {
        assert_eq!(
            model_path(r"Creature\BogBeast\BogBeast.mdx"),
            r"Creature\BogBeast\BogBeast.m2"
        );
        // Case is preserved on the part we keep.
        assert_eq!(
            model_path(r"CREATURE\Foo\BAR.MDX"),
            r"CREATURE\Foo\BAR.m2"
        );
        assert_eq!(model_path(r"a\b.m2"), r"a\b.m2");
    }

    #[test]
    fn derives_skin_paths() {
        assert_eq!(
            skin_path(r"Creature\BogBeast\BogBeast.m2", 0),
            r"Creature\BogBeast\BogBeast00.skin"
        );
        assert_eq!(
            skin_path(r"Creature\BogBeast\BogBeast.m2", 3),
            r"Creature\BogBeast\BogBeast03.skin"
        );
    }

    #[test]
    fn rejects_non_m2() {
        let bytes = vec![0u8; 0x200];
        assert!(matches!(Model::parse(&bytes), Err(Error::BadMagic(_))));
    }

    #[test]
    fn rejects_short_files() {
        assert!(matches!(
            Model::parse(b"MD20"),
            Err(Error::TooShort { .. })
        ));
    }

    #[test]
    fn rejects_other_versions() {
        let mut bytes = vec![0u8; 0x200];
        bytes[..4].copy_from_slice(b"MD20");
        bytes[4..8].copy_from_slice(&256u32.to_le_bytes());
        assert!(matches!(
            Model::parse(&bytes),
            Err(Error::UnsupportedVersion { got: 256 })
        ));
    }

    /// A count/offset pair pointing past the end must fail at parse time, not
    /// when something later reads it.
    #[test]
    fn rejects_out_of_bounds_arrays() {
        let mut bytes = vec![0u8; 0x200];
        bytes[..4].copy_from_slice(b"MD20");
        bytes[4..8].copy_from_slice(&VERSION_WOTLK.to_le_bytes());
        // vertices: 1000 entries at offset 0x100.
        bytes[60..64].copy_from_slice(&1000u32.to_le_bytes());
        bytes[64..68].copy_from_slice(&0x100u32.to_le_bytes());
        assert!(matches!(
            Model::parse(&bytes),
            Err(Error::ArrayOutOfBounds {
                what: "vertices",
                ..
            })
        ));
    }
}
