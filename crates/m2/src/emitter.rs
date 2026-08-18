//! Particle and ribbon emitters: the parts of a model that are not geometry.
//!
//! A torch's flame, a spell's sparks and a sword's trail are not in the
//! `.skin` file at all. They are *descriptions* -- a bone to hang off, a rate,
//! a lifespan, and colour, alpha and size curves over a particle's own life --
//! and the geometry is produced at runtime.
//!
//! # Two kinds of track, and they are not interchangeable
//!
//! An emitter carries both:
//!
//! * [`Track`], the ordinary [`crate::anim::Track`] used everywhere else in
//!   the format. It is indexed by *animation sequence* and its timestamps are
//!   milliseconds into that sequence, so it answers "how fast is this emitter
//!   emitting right now, given what the model is doing".
//! * [`PartTrack`], the format's `M2PartTrack`. It has no sequences at all and
//!   its timestamps are **a fraction of one particle's life**, stored as a
//!   `u16` over `0..=32767`, so it answers "what colour is a particle that is
//!   two thirds dead".
//!
//! Reading one as the other parses cleanly and produces nonsense: a lifetime
//! fraction read as a millisecond count puts every keyframe inside the first
//! thirty-three seconds of the animation, which for a looping torch is simply
//! "the last key, forever".
//!
//! # A particle's colour is 0..255 and a ribbon's is 0..1
//!
//! `Club_1H_Torch_A_01`'s particle colour track reads `(255, 72, 0)`,
//! `(223, 138, 47)`, `(255, 234, 177)` -- orange to pale yellow, which is a
//! flame. `CelestialDragonWyrm`'s ribbon reads `(0.0, 0.96, 1.0)`. Two
//! records, two ranges, and getting it backwards produces a plausible-looking
//! dark ramp in one direction and a blown-out white trail in the other, with
//! nothing to say which was meant.
//!
//! That is not a guess from two samples. `wow-cli m2 emitters --survey` counts
//! the keys with any component above 1.0, which no normalised colour can have:
//! **77,410 of 78,377 particle keys, and 0 of 1,572 ribbon keys.** The 967
//! particle keys under 1.0 are the genuinely near-black ones. See
//! [`ParticleEmitter::color`] and [`RibbonEmitter::color`].

use crate::anim::{Keyframe, Track};

/// Bytes per `M2Particle` record in the WotLK build.
///
/// **Measured, not transcribed.** See `wow-cli m2 emitters --survey`, which
/// checks the one property a wrong stride destroys: every offset a record
/// refers to must land *after* the whole block of records, and the first of
/// them must land within a 16-byte alignment pad of its end. A stride a word
/// short puts the last record's tracks inside the block; a word long runs the
/// block into the data.
pub const PARTICLE_SIZE: usize = 476;

/// Bytes per `M2Ribbon` record in the WotLK build. Measured the same way.
pub const RIBBON_SIZE: usize = 176;

/// Bytes per `M2PartTrack`: two `(count, offset)` pairs.
pub const PART_TRACK_SIZE: usize = 16;

/// How an emitter chooses a direction for a new particle.
///
/// The values were not transcribed: `wow-cli m2 emitters --survey` tallies
/// this byte across every emitter in the archives, and only these appear. A
/// byte read at the wrong offset takes dozens of values instead of three.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmitterType {
    /// Emits from a rectangle, up through the bone's local frame.
    Plane,
    /// Emits outwards from a sphere.
    Sphere,
    /// Emits along the emitter's own spline. Rare, and drawn as a plane here.
    Spline,
    /// Anything else, kept rather than mapped so a survey can report it.
    Unknown(u8),
}

impl EmitterType {
    pub fn from_raw(v: u8) -> Self {
        match v {
            1 => Self::Plane,
            2 => Self::Sphere,
            3 => Self::Spline,
            other => Self::Unknown(other),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Plane => "plane",
            Self::Sphere => "sphere",
            Self::Spline => "spline",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// A curve over a single particle's life.
///
/// The format calls this an `FBlock`. Unlike [`Track`] it is not per-sequence
/// and its timestamps are not milliseconds; see the module note.
#[derive(Clone, Debug, Default)]
pub struct PartTrack<T> {
    /// Fraction of a particle's life, `0.0..=1.0`, ascending.
    ///
    /// Stored on the wire as a `u16` where `32767` is the end of life --
    /// `Club_1H_Torch_A_01`'s tracks read `0, 16384, 32767`, which is
    /// start/half/end and is what identified the encoding.
    pub times: Vec<f32>,
    pub values: Vec<T>,
}

impl<T: Keyframe> PartTrack<T> {
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Samples at `age`, a fraction of the particle's life.
    ///
    /// Clamps at both ends, like [`Track::sample`]: a particle that outlives
    /// its last key holds it rather than vanishing.
    pub fn sample(&self, age: f32) -> Option<T> {
        if self.values.is_empty() {
            return None;
        }
        if self.values.len() == 1 || self.times.len() < 2 {
            return Some(self.values[0]);
        }
        let next = self.times.partition_point(|&t| t <= age);
        if next == 0 {
            return Some(self.values[0]);
        }
        if next >= self.times.len() {
            return self.values.last().copied();
        }
        let (t0, t1) = (self.times[next - 1], self.times[next]);
        let (v0, v1) = (self.values[next - 1], *self.values.get(next)?);
        if t1 <= t0 {
            return Some(v0);
        }
        Some(T::blend(v0, v1, (age - t0) / (t1 - t0)))
    }
}

/// One particle emitter.
///
/// Every field in the record is parsed, including the ones this renderer does
/// not yet act on: an unread field is invisible, and a parsed one shows up in
/// `wow-cli m2 emitters` where somebody can notice it is wrong.
#[derive(Clone, Debug)]
pub struct ParticleEmitter {
    /// The emitter's id in the model's own numbering. `-1` on most models.
    pub id: i32,
    pub flags: u32,
    /// Where the emitter sits, in the *bone's* frame -- unlike
    /// [`crate::Attachment::position`], which is a model-space point. A
    /// torch's flame reads `(0.64, -0.001, -0.07)` off bone 10, which is the
    /// tip of the handle rather than anywhere near the model origin.
    pub position: [f32; 3],
    /// Bone this hangs from, indexing [`crate::Model::bones`].
    pub bone: u16,
    /// **A direct index into [`crate::Model::textures`], not through the
    /// texture combo table.** `Club_1H_Torch_A_01` names texture 1, which is
    /// `FLAMELICKSMALL.BLP`; read through the combos it would land on the
    /// runtime slot the handle uses.
    pub texture: u16,
    /// The same blending vocabulary a [`crate::Material`] uses. A flame reads
    /// 4, which is additive.
    pub blend: u8,
    pub emitter_type: EmitterType,
    /// Row in `ParticleColor.dbc`, or 0. Not read here.
    pub color_index: u16,
    pub particle_type: u8,
    pub head_or_tail: u8,
    pub texture_tile_rotation: i16,
    /// The texture is a flipbook of `rows * columns` cells; a torch is 4x4.
    pub rows: u16,
    pub columns: u16,

    pub emission_speed: Track<f32>,
    pub speed_variation: Track<f32>,
    /// Half-angle of the emission cone, in radians, about the bone's up axis.
    pub vertical_range: Track<f32>,
    /// Spread about the bone's forward axis, in radians.
    pub horizontal_range: Track<f32>,
    pub gravity: Track<f32>,
    /// Seconds a particle lives.
    pub lifespan: Track<f32>,
    /// Random spread on `lifespan`, in seconds.
    pub lifespan_vary: f32,
    /// Particles per second.
    pub emission_rate: Track<f32>,
    pub emission_rate_vary: f32,
    pub emission_area_length: Track<f32>,
    pub emission_area_width: Track<f32>,
    pub z_source: Track<f32>,

    /// Colour over a particle's life, **in 0..255**, not 0..1 -- unlike
    /// [`RibbonEmitter::color`], which is normalised. See the module note.
    pub color: PartTrack<[f32; 3]>,
    /// Opacity over a particle's life, `0.0..=1.0`.
    pub alpha: PartTrack<f32>,
    /// Width and height over a particle's life, in model units.
    pub scale: PartTrack<[f32; 2]>,
    pub scale_vary: [f32; 2],
    /// Which flipbook cell the head of a particle shows, over its life.
    pub head_cell: PartTrack<u16>,
    pub tail_cell: PartTrack<u16>,

    pub tail_length: f32,
    pub twinkle_speed: f32,
    pub twinkle_percent: f32,
    pub twinkle_scale: [f32; 2],
    pub burst_multiplier: f32,
    pub drag: f32,
    pub base_spin: f32,
    pub base_spin_vary: f32,
    pub spin: f32,
    pub spin_vary: f32,
    /// Two corners of the tumble box: random angular velocity per particle.
    pub tumble: [f32; 6],
    pub wind: [f32; 3],
    pub wind_time: f32,
    pub follow_speed: [f32; 2],
    pub follow_scale: [f32; 2],
    pub spline_points: Vec<[f32; 3]>,
    /// Whether the emitter runs at all, per sequence. A torch that is only
    /// alight during one animation says so here.
    pub enabled_in: Track<u8>,
}

impl ParticleEmitter {
    /// Whether the emitter is running in this sequence, at this time.
    ///
    /// Driven by [`ParticleEmitter::enabled_in`] rather than by
    /// [`ParticleEmitter::flags`], because that one is per-sequence and
    /// observable: a model with the track present and zero in a sequence must
    /// not emit during it. A model with no track at all always emits, which
    /// is the overwhelming case.
    pub fn enabled(&self, sequence: usize, time_ms: u32) -> bool {
        self.enabled_in.sample(sequence, time_ms).unwrap_or(1) != 0
    }

    /// Number of cells in the flipbook, at least one.
    pub fn cells(&self) -> u32 {
        u32::from(self.rows.max(1)) * u32::from(self.columns.max(1))
    }
}

/// A trail: a strip of geometry left behind a moving bone.
///
/// The same idea as a particle emitter with the particles joined up -- a sword
/// swing's arc, a comet's tail. An edge is emitted `edges_per_second` times a
/// second and lives `edge_lifetime` seconds; the strip is the edges still
/// alive, in the order they were laid down.
#[derive(Clone, Debug)]
pub struct RibbonEmitter {
    pub id: i32,
    /// Bone this hangs from, indexing [`crate::Model::bones`].
    pub bone: u16,
    /// Where the strip is generated, in the bone's frame.
    pub position: [f32; 3],
    /// Indices into [`crate::Model::textures`].
    pub textures: Vec<u16>,
    /// Indices into [`crate::Model::materials`].
    pub materials: Vec<u16>,
    /// Colour, **normalised to 0..1** -- unlike [`ParticleEmitter::color`],
    /// which is 0..255. Measured rather than assumed; see the module note.
    pub color: Track<[f32; 3]>,
    pub alpha: Track<f32>,
    /// How far the strip reaches above and below the bone's own point, which
    /// together are its width.
    pub height_above: Track<f32>,
    pub height_below: Track<f32>,
    pub edges_per_second: f32,
    pub edge_lifetime: f32,
    pub gravity: f32,
    pub rows: u16,
    pub columns: u16,
    pub texture_slot: Track<u16>,
    pub visibility: Track<u8>,
}

impl RibbonEmitter {
    /// Whether the strip should be drawn at all right now.
    pub fn visible(&self, sequence: usize, time_ms: u32) -> bool {
        self.visibility.sample(sequence, time_ms).unwrap_or(1) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp() -> PartTrack<f32> {
        PartTrack {
            times: vec![0.0, 0.5, 1.0],
            values: vec![0.0, 1.0, 0.0],
        }
    }

    /// A particle's own clock, not the animation's. Sampling at 0.25 must land
    /// halfway up the first leg, which is what separates a lifetime fraction
    /// from a millisecond count: as milliseconds, 0.25 is before every key.
    #[test]
    fn part_tracks_sample_over_a_lifetime_fraction() {
        let t = ramp();
        assert_eq!(t.sample(0.0), Some(0.0));
        assert!((t.sample(0.25).unwrap() - 0.5).abs() < 1e-5);
        assert_eq!(t.sample(0.5), Some(1.0));
        assert!((t.sample(0.75).unwrap() - 0.5).abs() < 1e-5);
    }

    /// Clamps rather than vanishing, both ends.
    #[test]
    fn part_tracks_clamp_outside_their_keys() {
        let t = ramp();
        assert_eq!(t.sample(-1.0), Some(0.0));
        assert_eq!(t.sample(9.0), Some(0.0));
        let empty: PartTrack<f32> = PartTrack::default();
        assert_eq!(empty.sample(0.5), None);
    }

    /// Only three values exist, and everything else has to survive as itself
    /// so a survey can report it rather than silently becoming a plane.
    #[test]
    fn emitter_types_keep_what_they_do_not_recognise() {
        assert_eq!(EmitterType::from_raw(1), EmitterType::Plane);
        assert_eq!(EmitterType::from_raw(2), EmitterType::Sphere);
        assert_eq!(EmitterType::from_raw(3), EmitterType::Spline);
        assert_eq!(EmitterType::from_raw(7), EmitterType::Unknown(7));
    }
}
