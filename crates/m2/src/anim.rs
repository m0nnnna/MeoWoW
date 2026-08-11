//! Animation: sequences, keyframe tracks, and posing a skeleton.
//!
//! An M2 stores one track per animated property per bone, and each track holds
//! a *separate* keyframe list per animation sequence. So sampling means picking
//! the sequence first, then the keyframe within it -- there is no single global
//! timeline.
//!
//! Rotations are stored as four 16-bit integers rather than floats, with a
//! sign-dependent decode that is easy to get subtly wrong; see
//! [`decompress_quat_component`].

use glam::{Mat4, Quat, Vec3};

/// How values between keyframes are produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    /// Hold the previous keyframe until the next one.
    None,
    Linear,
    /// Spline forms. Their control points are stored inline with the values;
    /// until that is decoded, they are sampled linearly, which is visually
    /// close for the bone tracks that use them.
    Hermite,
    Bezier,
}

impl Interpolation {
    pub(crate) fn from_raw(v: u16) -> Self {
        match v {
            0 => Self::None,
            2 => Self::Hermite,
            3 => Self::Bezier,
            _ => Self::Linear,
        }
    }
}

/// Reads a fixed-size value out of a track's value array.
pub trait Keyframe: Copy {
    const SIZE: usize;
    fn read(bytes: &[u8]) -> Self;
    /// Blend between two keyframes. `t` is in `0..=1`.
    fn blend(a: Self, b: Self, t: f32) -> Self;
}

fn f32_at(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

impl Keyframe for Vec3 {
    const SIZE: usize = 12;
    fn read(b: &[u8]) -> Self {
        Vec3::new(f32_at(b, 0), f32_at(b, 4), f32_at(b, 8))
    }
    fn blend(a: Self, b: Self, t: f32) -> Self {
        a.lerp(b, t)
    }
}

impl Keyframe for f32 {
    const SIZE: usize = 4;
    fn read(b: &[u8]) -> Self {
        f32_at(b, 0)
    }
    fn blend(a: Self, b: Self, t: f32) -> Self {
        a + (b - a) * t
    }
}

/// Expands one component of a compressed quaternion.
///
/// The 16 bits span `-1..1` with the seam at zero rather than at the numeric
/// midpoint, so the mapping is *not* the obvious `v / 32767`:
///
/// | stored | value |
/// |--------|-------|
/// | `0` | `-1.0` |
/// | `32767` | `0.0` |
/// | `-32768` | `0.0` |
/// | `-1` | `+1.0` |
///
/// An identity rotation is therefore `(32767, 32767, 32767, -1)`, not all
/// `32767`. Using `v / 32767` gives the right answer only near zero, so limbs
/// drift instead of snapping and the error is easy to miss.
#[inline]
pub fn decompress_quat_component(v: i16) -> f32 {
    let shifted = if v < 0 {
        v as i32 + 32768
    } else {
        v as i32 - 32767
    };
    shifted as f32 / 32767.0
}

impl Keyframe for Quat {
    const SIZE: usize = 8;
    fn read(b: &[u8]) -> Self {
        let c = |o: usize| decompress_quat_component(i16::from_le_bytes([b[o], b[o + 1]]));
        // Stored x, y, z, w.
        Quat::from_xyzw(c(0), c(2), c(4), c(6))
    }
    fn blend(a: Self, b: Self, t: f32) -> Self {
        // Shortest-path spherical blend; `slerp` handles the sign flip that
        // would otherwise send a bone the long way round.
        a.slerp(b, t)
    }
}

/// Keyframes for one property, for one sequence.
#[derive(Clone, Debug, Default)]
pub struct Keyframes<T> {
    /// Timestamps in milliseconds, ascending.
    pub times: Vec<u32>,
    pub values: Vec<T>,
}

/// One animated property of one bone, across every sequence.
#[derive(Clone, Debug)]
pub struct Track<T> {
    pub interpolation: Interpolation,
    /// When set, the track runs on a shared global timer rather than the
    /// current sequence -- used for things that loop independently of what the
    /// model is doing.
    pub global_sequence: Option<u16>,
    /// One entry per sequence in the model.
    pub sequences: Vec<Keyframes<T>>,
}

impl<T> Default for Track<T> {
    fn default() -> Self {
        Self {
            interpolation: Interpolation::None,
            global_sequence: None,
            sequences: Vec::new(),
        }
    }
}

impl<T: Keyframe> Track<T> {
    pub fn is_animated(&self) -> bool {
        self.sequences.iter().any(|s| !s.values.is_empty())
    }

    /// Samples the track, or returns `None` if this sequence has no keyframes.
    ///
    /// Times before the first key clamp to it and times after the last clamp to
    /// that; callers wrap `time` to the sequence duration beforehand.
    pub fn sample(&self, sequence: usize, time: u32) -> Option<T> {
        let keys = self.sequences.get(sequence)?;
        if keys.values.is_empty() {
            return None;
        }
        if keys.values.len() == 1 || keys.times.len() < 2 {
            return Some(keys.values[0]);
        }

        // Keys are ascending, so a binary search finds the span directly.
        let next = keys.times.partition_point(|&t| t <= time);
        if next == 0 {
            return Some(keys.values[0]);
        }
        if next >= keys.times.len() {
            return keys.values.last().copied();
        }

        let (t0, t1) = (keys.times[next - 1], keys.times[next]);
        let (v0, v1) = (keys.values[next - 1], *keys.values.get(next)?);
        if self.interpolation == Interpolation::None || t1 <= t0 {
            return Some(v0);
        }
        let f = (time - t0) as f32 / (t1 - t0) as f32;
        Some(T::blend(v0, v1, f))
    }
}

/// One animation: an entry in the model's sequence list.
#[derive(Clone, Copy, Debug)]
pub struct Sequence {
    /// Row in `AnimationData.dbc`, which is where the human-readable name is.
    pub id: u16,
    /// Which variation of that animation this is; models often ship several
    /// idles that the client picks between.
    pub variation: u16,
    pub duration_ms: u32,
    pub move_speed: f32,
    pub flags: u32,
    pub blend_time: u32,
    /// Next variation in the chain, or -1.
    pub variation_next: i16,
    /// When a sequence carries no keys of its own, it borrows them from this
    /// index instead.
    pub alias_next: u16,
}

impl Sequence {
    /// Bytes per entry in this version.
    pub const SIZE: usize = 64;

    pub fn read(b: &[u8]) -> Self {
        let h = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
        let w = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        Self {
            id: h(0),
            variation: h(2),
            duration_ms: w(4),
            move_speed: f32_at(b, 8),
            flags: w(12),
            blend_time: w(28),
            variation_next: i16::from_le_bytes([b[60], b[61]]),
            alias_next: h(62),
        }
    }

    /// Whether this sequence's keyframes are stored in the `.m2` itself.
    ///
    /// WotLK moved most animation data into external `.anim` files and marks
    /// the ones that stayed behind with this bit. Reading a non-inline
    /// sequence's tracks out of the `.m2` yields whatever bytes happen to live
    /// at those offsets -- decoded quaternions come out non-unit, which is how
    /// this was found.
    pub fn is_inline(&self) -> bool {
        self.flags & 0x20 != 0
    }

    /// Whether the sequence borrows another entry's keyframes.
    ///
    /// An alias may also be inline: `0x60` is both, and carries its own data.
    /// Only a bare `0x40` has to follow [`Sequence::alias_next`].
    pub fn is_alias(&self) -> bool {
        self.flags & 0x40 != 0
    }
}

/// Path of the external `.anim` file holding a sequence's keyframes.
///
/// Named after the model, the animation id, and the variation:
/// `Creature\GnollMelee\GnollMelee0062-00.anim`.
pub fn external_anim_path(model_path: &str, sequence: &Sequence) -> String {
    let stem = model_path
        .strip_suffix(".m2")
        .or_else(|| model_path.strip_suffix(".M2"))
        .unwrap_or(model_path);
    format!("{stem}{:04}-{:02}.anim", sequence.id, sequence.variation)
}

/// A posed skeleton: one matrix per bone, in model space.
pub type Pose = Vec<Mat4>;

/// Builds the transform for a single bone, ignoring its parent.
///
/// Rotation and scale happen about the bone's pivot, so the pivot is moved to
/// the origin first and restored afterwards. Omitting that makes every rotation
/// swing the limb about the model origin instead.
pub fn local_transform(pivot: Vec3, translation: Vec3, rotation: Quat, scale: Vec3) -> Mat4 {
    Mat4::from_translation(pivot + translation)
        * Mat4::from_quat(rotation)
        * Mat4::from_scale(scale)
        * Mat4::from_translation(-pivot)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inverse of [`decompress_quat_component`], for round-trip tests.
    fn compress_quat_component(f: f32) -> i16 {
        let scaled = (f * 32767.0).round();
        if f <= 0.0 {
            (scaled + 32767.0) as i16
        } else {
            (scaled - 32768.0) as i16
        }
    }

    /// The seam is at zero, not at the numeric midpoint. Pinning the exact
    /// endpoints is the whole point: `v / 32767` passes near zero and fails
    /// here.
    #[test]
    fn quaternion_components_decompress_to_unit_range() {
        assert!((decompress_quat_component(0) + 1.0).abs() < 1e-4);
        assert!(decompress_quat_component(32767).abs() < 1e-6);
        assert!(decompress_quat_component(-32768).abs() < 1e-4);
        assert!((decompress_quat_component(-1) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn quaternion_components_round_trip() {
        for f in [-1.0, -0.5, 0.0, 0.25, 0.5, 1.0f32] {
            let back = decompress_quat_component(compress_quat_component(f));
            assert!((back - f).abs() < 1e-4, "{f} came back as {back}");
        }
    }

    /// Identity is `(32767, 32767, 32767, -1)`. Encoding it as all-`32767`
    /// yields `w = 0`, which is not a rotation at all.
    #[test]
    fn identity_quaternion_round_trips() {
        let mut bytes = Vec::new();
        for f in [0.0, 0.0, 0.0, 1.0f32] {
            bytes.extend_from_slice(&compress_quat_component(f).to_le_bytes());
        }
        let q = Quat::read(&bytes);
        assert!((q.w - 1.0).abs() < 1e-4, "w was {}", q.w);
        assert!(q.x.abs() < 1e-4 && q.y.abs() < 1e-4 && q.z.abs() < 1e-4);
        assert!(q.is_normalized(), "identity must be a unit quaternion");
    }

    #[test]
    fn sampling_holds_before_and_after_the_keys() {
        let track = Track {
            interpolation: Interpolation::Linear,
            global_sequence: None,
            sequences: vec![Keyframes {
                times: vec![100, 200],
                values: vec![Vec3::ZERO, Vec3::splat(10.0)],
            }],
        };
        assert_eq!(track.sample(0, 0), Some(Vec3::ZERO));
        assert_eq!(track.sample(0, 500), Some(Vec3::splat(10.0)));
        let mid = track.sample(0, 150).unwrap();
        assert!((mid.x - 5.0).abs() < 1e-4, "got {mid:?}");
    }

    /// `Interpolation::None` must hold the previous key rather than blend.
    #[test]
    fn step_interpolation_does_not_blend() {
        let track = Track {
            interpolation: Interpolation::None,
            global_sequence: None,
            sequences: vec![Keyframes {
                times: vec![0, 100],
                values: vec![1.0f32, 2.0],
            }],
        };
        assert_eq!(track.sample(0, 50), Some(1.0));
        assert_eq!(track.sample(0, 100), Some(2.0));
    }

    #[test]
    fn missing_sequences_sample_to_nothing() {
        let track: Track<Vec3> = Track::default();
        assert_eq!(track.sample(0, 0), None);
        assert!(!track.is_animated());
    }

    /// Rotation happens about the pivot, not the model origin.
    #[test]
    fn local_transform_rotates_about_the_pivot() {
        let pivot = Vec3::new(0.0, 0.0, 2.0);
        let rot = Quat::from_rotation_x(std::f32::consts::FRAC_PI_2);
        let m = local_transform(pivot, Vec3::ZERO, rot, Vec3::ONE);
        // A point at the pivot must not move.
        let moved = m.transform_point3(pivot);
        assert!((moved - pivot).length() < 1e-5, "pivot moved to {moved:?}");
    }
}
