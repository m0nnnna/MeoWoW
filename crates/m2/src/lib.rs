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
pub mod skin;

pub use anim::{Interpolation, Keyframe, Keyframes, Pose, Sequence, Track};
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
const MATERIAL_SIZE: usize = 4;

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

    fn skip_floats(&mut self, n: usize) {
        self.pos += n * 4;
    }
}

/// Header fields this reader uses. Trailing fields (lights, cameras, particle
/// emitters) are deliberately not parsed yet.
#[derive(Debug, Default)]
struct Header {
    version: u32,
    name: Array,
    global_flags: u32,
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
    bounding_box: [f32; 6],
    bounding_sphere_radius: f32,
    collision_box: [f32; 6],
    collision_sphere_radius: f32,
    attachments: Array,
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

/// A bone together with its animation tracks.
pub struct AnimatedBone {
    pub bone: Bone,
    pub translation: Track<Vec3>,
    pub rotation: Track<Quat>,
    pub scale: Track<Vec3>,
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
        let _global_loops = r.array();
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
        let _texture_coord_combos = r.array();
        let _texture_weight_combos = r.array();
        let _texture_transform_combos = r.array();

        for slot in &mut h.bounding_box {
            *slot = r.f32();
        }
        h.bounding_sphere_radius = r.f32();
        for slot in &mut h.collision_box {
            *slot = r.f32();
        }
        h.collision_sphere_radius = r.f32();

        let _collision_indices = r.array();
        let _collision_positions = r.array();
        let _collision_normals = r.array();
        h.attachments = r.array();
        r.skip_floats(0);

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
        let mut out = vec![Mat4::IDENTITY; bones.len()];
        for &i in &bone_order(bones) {
            let b = &bones[i];
            let translation = b.translation.sample(sequence, time_ms).unwrap_or(Vec3::ZERO);
            let rotation = b.rotation.sample(sequence, time_ms).unwrap_or(Quat::IDENTITY);
            let scale = b.scale.sample(sequence, time_ms).unwrap_or(Vec3::ONE);

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

    /// Maps a submesh's local bone slots to indices in [`Model::bones`].
    pub fn bone_combos(&self) -> Vec<u16> {
        self.u16_table("bone_combos", self.header.bone_combos)
    }

    pub fn attachment_count(&self) -> u32 {
        self.header.attachments.count
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
