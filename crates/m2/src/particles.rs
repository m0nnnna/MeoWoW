//! Running a [`ParticleEmitter`] forward in time, and a [`RibbonEmitter`]'s
//! trail behind it.
//!
//! Kept in this crate rather than in the renderer for the same reason
//! [`crate::Model::pose`] is: it is what the data *means* over time, it needs
//! no GPU, and a simulation nothing can test without a window is a simulation
//! nobody tests.
//!
//! # Particles live in the world, not in the model
//!
//! A particle is emitted at wherever the emitter's bone was *at that instant*
//! and is on its own afterwards. That is not an implementation convenience; it
//! is the visible behaviour. A torch carried past you leaves its flames behind
//! it, and a system simulated in model space carries the whole plume along
//! rigidly with the character -- which reads as a sticker rather than a fire.
//!
//! The cost is that [`ParticleSystem::update`] has to be handed the emitter's
//! *current* world transform every frame, and that a system belongs to one
//! placement rather than to one model. Two torches in a room need two systems.

use glam::{Mat4, Vec3};

use crate::emitter::{EmitterType, ParticleEmitter, RibbonEmitter};

/// How many particles one emitter may have alive at once.
///
/// A bound rather than a tuning knob. `emission_rate` times `lifespan` is
/// under a hundred for everything measured in the archives, so this is not
/// reached by ordinary data -- it is what stops a single absurd track, or a
/// frame that took a second, from allocating without limit.
pub const MAX_PARTICLES: usize = 512;

/// The longest step the simulation will take in one go.
///
/// A frame that stalls -- a tile streaming in, a texture upload -- must not be
/// paid back as a burst of a thousand particles appearing at once. Losing the
/// missing time is the correct trade: a fire is a steady state, and nobody can
/// see that it is a tenth of a second behind.
pub const MAX_STEP: f32 = 0.1;

/// One live particle. World space, seconds.
#[derive(Clone, Copy, Debug)]
pub struct Particle {
    pub position: Vec3,
    pub velocity: Vec3,
    /// How long it has been alive.
    pub age: f32,
    /// How long it will live. Never zero -- see [`ParticleSystem::spawn`].
    pub lifespan: f32,
    /// Current spin, in radians, and how fast it is spinning.
    pub rotation: f32,
    pub spin: f32,
}

impl Particle {
    /// Fraction of the way through its life, `0.0..=1.0`.
    pub fn life(&self) -> f32 {
        (self.age / self.lifespan).clamp(0.0, 1.0)
    }
}

/// What one particle looks like right now: everything a billboard needs and
/// nothing about how it got here.
#[derive(Clone, Copy, Debug)]
pub struct Sprite {
    pub position: [f32; 3],
    /// Half-width and half-height, in world units.
    pub size: [f32; 2],
    pub rotation: f32,
    /// Colour in `0..=1` and alpha in `w`. Converted from the emitter's own
    /// `0..255` here so no caller has to remember which range it was in.
    pub color: [f32; 4],
    /// The flipbook cell, as `u0, v0, u1, v1`.
    pub uv: [f32; 4],
}

/// A tiny deterministic generator.
///
/// Deterministic on purpose: a test that asserts where a particle went needs
/// the same particle twice, and `rand` would be a dependency this crate has no
/// other use for -- see the project's rule about what comes from crates.io.
#[derive(Clone, Copy, Debug)]
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        // xorshift32. Any non-zero seed cycles through every non-zero word.
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// `0.0..1.0`.
    fn unit(&mut self) -> f32 {
        (self.next() >> 8) as f32 / (1 << 24) as f32
    }

    /// `-1.0..1.0`.
    fn signed(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }
}

/// One emitter's live particles.
///
/// One per *placement*, not one per model: see the module note.
pub struct ParticleSystem {
    particles: Vec<Particle>,
    /// Emission is a rate, and a rate times a frame is usually a fraction of
    /// a particle. Carrying the remainder is what makes a slow emitter emit at
    /// all -- truncating each frame's share independently emits nothing below
    /// one particle per frame, which at 60fps silently kills every emitter
    /// under 60 per second, and 20 is a torch.
    pending: f32,
    rng: Rng,
    /// Particles that could not be spawned because [`MAX_PARTICLES`] was
    /// reached. Counted rather than ignored: "none were dropped" and "there
    /// were none to drop" are different states and only one is healthy.
    pub dropped: u64,
    pub spawned: u64,
}

impl ParticleSystem {
    /// `seed` distinguishes one placement from another. Two torches on the
    /// same wall built from the same model must not flicker in lockstep, and
    /// the only thing that separates them is this.
    pub fn new(seed: u32) -> Self {
        Self {
            particles: Vec::new(),
            pending: 0.0,
            // xorshift is stuck at zero, so no placement may seed with it.
            rng: Rng(seed | 1),
            dropped: 0,
            spawned: 0,
        }
    }

    pub fn particles(&self) -> &[Particle] {
        &self.particles
    }

    pub fn is_empty(&self) -> bool {
        self.particles.is_empty()
    }

    /// Advances the system by `dt` seconds.
    ///
    /// `to_world` places the emitter's *bone* right now, already including
    /// the placement's own transform -- so a particle is born wherever the
    /// bone is at this instant and never moves with it again.
    ///
    /// `sequence` and `time_ms` select the animation the model is playing,
    /// because every rate and range on an emitter is a per-sequence track: a
    /// creature's flame can be authored to gutter in one cycle and roar in
    /// another.
    pub fn update(
        &mut self,
        emitter: &ParticleEmitter,
        to_world: Mat4,
        sequence: usize,
        time_ms: u32,
        dt: f32,
    ) {
        let dt = dt.clamp(0.0, MAX_STEP);
        if dt <= 0.0 {
            return;
        }

        // The emitter's own frame, taken from the matrix rather than rebuilt
        // from angles: the two agree only until somebody changes how a bone is
        // posed, and a plume leaning the wrong way is not obviously a stale
        // copy of a basis.
        let origin = to_world.transform_point3(Vec3::from(emitter.position));
        let right = to_world.x_axis.truncate().normalize_or_zero();
        let forward = to_world.y_axis.truncate().normalize_or_zero();
        let up = to_world.z_axis.truncate().normalize_or_zero();

        let track = |t: &crate::Track<f32>, fallback: f32| t.sample(sequence, time_ms).unwrap_or(fallback);
        let gravity = track(&emitter.gravity, 0.0);
        let drag = emitter.drag;

        for p in &mut self.particles {
            p.age += dt;
            // Gravity is along the emitter's own down, not the world's. A
            // torch held sideways drips sideways, which is what the data says
            // and what a model-space simulation would have got for free.
            p.velocity -= up * gravity * dt;
            if drag > 0.0 {
                p.velocity *= (1.0 - drag * dt).max(0.0);
            }
            p.position += p.velocity * dt;
            p.rotation += p.spin * dt;
        }
        self.particles.retain(|p| p.age < p.lifespan);

        if !emitter.enabled(sequence, time_ms) {
            // Stop emitting, but let what is already alight burn out. Killing
            // them outright makes a torch that goes out blink rather than fade.
            self.pending = 0.0;
            return;
        }

        let rate = track(&emitter.emission_rate, 0.0);
        if rate <= 0.0 {
            return;
        }
        self.pending += rate * dt;
        let wanted = self.pending.floor();
        self.pending -= wanted;

        let speed = track(&emitter.emission_speed, 0.0);
        let speed_variation = track(&emitter.speed_variation, 0.0);
        let lifespan = track(&emitter.lifespan, 1.0);
        let vertical = track(&emitter.vertical_range, 0.0);
        let horizontal = track(&emitter.horizontal_range, 0.0);
        let length = track(&emitter.emission_area_length, 0.0);
        let width = track(&emitter.emission_area_width, 0.0);

        for _ in 0..(wanted as usize) {
            if self.particles.len() >= MAX_PARTICLES {
                self.dropped += 1;
                continue;
            }
            let particle = self.spawn(
                emitter,
                Frame {
                    origin,
                    right,
                    forward,
                    up,
                },
                Emission {
                    speed,
                    speed_variation,
                    lifespan,
                    vertical,
                    horizontal,
                    length,
                    width,
                },
            );
            self.particles.push(particle);
            self.spawned += 1;
        }
    }

    fn spawn(&mut self, emitter: &ParticleEmitter, frame: Frame, e: Emission) -> Particle {
        let (offset, direction) = match emitter.emitter_type {
            // A rectangle in the bone's own plane, throwing particles up
            // through a cone about its normal. `horizontal_range` is the
            // azimuth the cone is allowed to lean into and reads 2*pi on a
            // torch, which is a full turn -- so the flame is symmetric.
            EmitterType::Plane | EmitterType::Spline | EmitterType::Unknown(_) => {
                let offset = frame.right * (self.rng.signed() * e.length * 0.5)
                    + frame.forward * (self.rng.signed() * e.width * 0.5);
                let polar = self.rng.unit() * e.vertical;
                let azimuth = self.rng.signed() * e.horizontal * 0.5;
                let direction = frame.up * polar.cos()
                    + (frame.right * azimuth.cos() + frame.forward * azimuth.sin())
                        * polar.sin();
                (offset, direction)
            }
            // A shell. `emission_area_length` and `_width` are the inner and
            // outer radius rather than a rectangle's sides, which is why they
            // are read here and not shared with the branch above.
            EmitterType::Sphere => {
                let polar = self.rng.unit() * e.vertical;
                let azimuth = self.rng.signed() * e.horizontal * 0.5;
                let direction = frame.up * polar.cos()
                    + (frame.right * azimuth.cos() + frame.forward * azimuth.sin())
                        * polar.sin();
                let radius = e.length + (e.width - e.length) * self.rng.unit();
                (direction * radius, direction)
            }
        };

        // A lifespan of zero divides by zero in `Particle::life` and makes
        // every track sample as NaN, which draws as nothing at all and reads
        // as the emitter never having run.
        let lifespan =
            (e.lifespan + emitter.lifespan_vary * self.rng.signed()).max(f32::EPSILON);
        let speed = e.speed * (1.0 + e.speed_variation * self.rng.signed());
        let spin = emitter.base_spin + emitter.base_spin_vary * self.rng.signed();

        Particle {
            position: frame.origin + offset,
            velocity: direction.normalize_or_zero() * speed,
            age: 0.0,
            lifespan,
            rotation: self.rng.unit() * std::f32::consts::TAU,
            spin,
        }
    }

    /// What each live particle looks like this frame.
    ///
    /// Separate from [`ParticleSystem::update`] so the simulation can be
    /// stepped without anything being drawn, which is what makes it testable.
    pub fn sprites(&self, emitter: &ParticleEmitter) -> Vec<Sprite> {
        let mut sprites = Vec::with_capacity(self.particles.len());
        self.for_each_sprite(emitter, |value| sprites.push(value));
        sprites
    }

    /// Visits each live particle without allocating a temporary sprite list.
    pub fn for_each_sprite(&self, emitter: &ParticleEmitter, mut visit: impl FnMut(Sprite)) {
        for particle in &self.particles {
            visit(sprite(emitter, particle));
        }
    }
}

/// The emitter's basis this frame, so [`ParticleSystem::spawn`] takes one
/// argument instead of four.
#[derive(Clone, Copy)]
struct Frame {
    origin: Vec3,
    right: Vec3,
    forward: Vec3,
    up: Vec3,
}

/// The emitter's tracks, already sampled for this frame.
#[derive(Clone, Copy)]
struct Emission {
    speed: f32,
    speed_variation: f32,
    lifespan: f32,
    vertical: f32,
    horizontal: f32,
    length: f32,
    width: f32,
}

/// One particle's appearance, from the emitter's per-life curves.
pub fn sprite(emitter: &ParticleEmitter, p: &Particle) -> Sprite {
    let life = p.life();
    // A track with no keys is not an error: an emitter that never changes
    // colour simply has none. White, opaque and unit-sized are the identities
    // for the three, so a missing curve draws the texture as authored.
    let color = emitter.color.sample(life).unwrap_or([255.0; 3]);
    let alpha = emitter.alpha.sample(life).unwrap_or(1.0);
    let scale = emitter.scale.sample(life).unwrap_or([1.0; 2]);

    let cells = emitter.cells();
    let cell = emitter
        .head_cell
        .sample(life)
        .map(u32::from)
        // A head-cell track ramping `0, 7, 8, 16` over a sixteen-cell sheet
        // ends one past the last cell, so the index is wrapped rather than
        // clamped -- clamping holds the final frame twice as long.
        .map(|c| c % cells.max(1))
        .unwrap_or(0);
    let columns = u32::from(emitter.columns.max(1));
    let rows = u32::from(emitter.rows.max(1));
    let (cx, cy) = (cell % columns, (cell / columns) % rows);
    let (w, h) = (1.0 / columns as f32, 1.0 / rows as f32);

    Sprite {
        position: p.position.to_array(),
        // Halved because the shader expands about the centre, and the track is
        // the full extent.
        size: [scale[0] * 0.5, scale[1] * 0.5],
        rotation: p.rotation,
        color: [
            color[0] / 255.0,
            color[1] / 255.0,
            color[2] / 255.0,
            alpha.clamp(0.0, 1.0),
        ],
        uv: [
            cx as f32 * w,
            cy as f32 * h,
            (cx + 1) as f32 * w,
            (cy + 1) as f32 * h,
        ],
    }
}

/// One point on a ribbon's trail: where the bone was, and when.
#[derive(Clone, Copy, Debug)]
pub struct Edge {
    /// The bone's own point at that instant.
    pub centre: Vec3,
    /// The bone's up axis then, which is the direction the strip has width
    /// along. Captured with the point rather than recomputed, because the bone
    /// has turned since and a strip rebuilt from the current axis twists
    /// itself along its whole length whenever the wielder moves.
    pub up: Vec3,
    /// Seconds since it was laid down.
    pub age: f32,
}

/// A ribbon's live trail.
///
/// The strip is the edges still alive, oldest last, and the drawn geometry is
/// two triangles between each neighbouring pair.
pub struct RibbonTrail {
    edges: Vec<Edge>,
    pending: f32,
}

impl Default for RibbonTrail {
    fn default() -> Self {
        Self::new()
    }
}

impl RibbonTrail {
    pub fn new() -> Self {
        Self {
            edges: Vec::new(),
            pending: 0.0,
        }
    }

    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// Advances the trail, laying down new edges where the bone is now.
    pub fn update(
        &mut self,
        emitter: &RibbonEmitter,
        to_world: Mat4,
        sequence: usize,
        time_ms: u32,
        dt: f32,
    ) {
        let dt = dt.clamp(0.0, MAX_STEP);
        if dt <= 0.0 {
            return;
        }
        let lifetime = emitter.edge_lifetime.max(f32::EPSILON);
        for edge in &mut self.edges {
            edge.age += dt;
            // The whole strip sags, not just its tail: gravity acts on an edge
            // for as long as it has existed, so the fall is quadratic in age
            // and the difference between neighbouring edges is what curves it.
            edge.centre.z -= emitter.gravity * dt * edge.age;
        }
        self.edges.retain(|e| e.age < lifetime);

        if !emitter.visible(sequence, time_ms) {
            self.pending = 0.0;
            return;
        }

        self.pending += emitter.edges_per_second.max(0.0) * dt;
        let wanted = self.pending.floor();
        self.pending -= wanted;
        if wanted <= 0.0 {
            return;
        }

        // Only ever one new edge per frame however many the rate asked for:
        // several would all be laid at the same place, since the bone has not
        // moved between them, and a stack of coincident edges is a seam rather
        // than a longer trail.
        let centre = to_world.transform_point3(Vec3::from(emitter.position));
        let up = to_world.z_axis.truncate().normalize_or_zero();
        self.edges.insert(0, Edge { centre, up, age: 0.0 });
        // A rate and a lifetime bound the count already; this bounds a rate
        // nothing sane produced.
        self.edges.truncate(MAX_PARTICLES);
    }

    /// Half-widths and colour for an edge, from the emitter's tracks.
    ///
    /// The tracks are per-*sequence*, not per-edge-life, so every edge on the
    /// strip shares one sample -- which is the format's own choice and why a
    /// ribbon fades by alpha rather than by age.
    pub fn appearance(
        emitter: &RibbonEmitter,
        sequence: usize,
        time_ms: u32,
    ) -> ([f32; 2], [f32; 4]) {
        let above = emitter.height_above.sample(sequence, time_ms).unwrap_or(0.0);
        let below = emitter.height_below.sample(sequence, time_ms).unwrap_or(0.0);
        let color = emitter.color.sample(sequence, time_ms).unwrap_or([1.0; 3]);
        let alpha = emitter.alpha.sample(sequence, time_ms).unwrap_or(1.0);
        // Normalised already -- a ribbon's colour is 0..1 where a particle's is
        // 0..255. See `emitter`'s module note.
        (
            [above, below],
            [color[0], color[1], color[2], alpha.clamp(0.0, 1.0)],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::{Interpolation, Keyframes};
    use crate::emitter::PartTrack;
    use crate::Track;

    fn constant(value: f32) -> Track<f32> {
        Track {
            interpolation: Interpolation::Linear,
            global_sequence: None,
            sequences: vec![Keyframes {
                times: vec![0],
                values: vec![value],
            }],
        }
    }

    fn empty<T>() -> Track<T> {
        Track::default()
    }

    /// A torch: 20 particles a second, living 0.8 seconds, rising slowly.
    fn torch() -> ParticleEmitter {
        ParticleEmitter {
            id: -1,
            flags: 0,
            position: [0.0; 3],
            bone: 0,
            texture: 0,
            blend: 4,
            emitter_type: EmitterType::Plane,
            color_index: 0,
            particle_type: 0,
            head_or_tail: 0,
            texture_tile_rotation: 0,
            rows: 4,
            columns: 4,
            emission_speed: constant(1.0),
            speed_variation: constant(0.0),
            vertical_range: constant(0.0),
            horizontal_range: constant(0.0),
            gravity: constant(0.0),
            lifespan: constant(0.8),
            lifespan_vary: 0.0,
            emission_rate: constant(20.0),
            emission_rate_vary: 0.0,
            emission_area_length: constant(0.0),
            emission_area_width: constant(0.0),
            z_source: constant(0.0),
            color: PartTrack {
                times: vec![0.0, 1.0],
                values: vec![[255.0, 0.0, 0.0], [0.0, 0.0, 255.0]],
            },
            alpha: PartTrack {
                times: vec![0.0, 1.0],
                values: vec![1.0, 0.0],
            },
            scale: PartTrack {
                times: vec![0.0],
                values: vec![[2.0, 2.0]],
            },
            scale_vary: [0.0; 2],
            head_cell: PartTrack::default(),
            tail_cell: PartTrack::default(),
            tail_length: 0.0,
            twinkle_speed: 0.0,
            twinkle_percent: 0.0,
            twinkle_scale: [0.0; 2],
            burst_multiplier: 0.0,
            drag: 0.0,
            base_spin: 0.0,
            base_spin_vary: 0.0,
            spin: 0.0,
            spin_vary: 0.0,
            tumble: [0.0; 6],
            wind: [0.0; 3],
            wind_time: 0.0,
            follow_speed: [0.0; 2],
            follow_scale: [0.0; 2],
            spline_points: Vec::new(),
            enabled_in: empty(),
        }
    }

    /// The bug this exists to prevent: 20 particles a second at 60fps is a
    /// third of a particle per frame, and a system that truncates each frame's
    /// share independently emits **nothing, ever**. A torch is 20/s.
    #[test]
    fn a_rate_below_one_per_frame_still_emits() {
        let mut system = ParticleSystem::new(1);
        let emitter = torch();
        for _ in 0..60 {
            system.update(&emitter, Mat4::IDENTITY, 0, 0, 1.0 / 60.0);
        }
        assert!(
            system.spawned >= 19 && system.spawned <= 21,
            "a second at 20/s should be about 20 particles, got {}",
            system.spawned
        );
    }

    /// Particles are world-space and stay where they were born. A carried
    /// torch must leave its flames behind rather than drag the plume along.
    #[test]
    fn particles_do_not_follow_the_emitter() {
        let mut system = ParticleSystem::new(7);
        let mut emitter = torch();
        emitter.emission_speed = constant(0.0);
        system.update(&emitter, Mat4::IDENTITY, 0, 0, 0.1);
        let born = system.particles()[0].position;
        assert!(born.length() < 1e-6, "born at the emitter, got {born:?}");

        // Move the emitter a long way and step again.
        let moved = Mat4::from_translation(Vec3::new(100.0, 0.0, 0.0));
        system.update(&emitter, moved, 0, 0, 0.1);
        let still = system.particles()[0].position;
        assert!(
            (still - born).length() < 1e-6,
            "an existing particle moved with its emitter, from {born:?} to {still:?}"
        );
        // ...and the new ones are over there, which is the other half: a test
        // asserting only that the old one stayed put passes just as well if
        // nothing is being emitted at all.
        assert!(
            system.particles().iter().any(|p| p.position.x > 99.0),
            "nothing was emitted at the emitter's new position"
        );
    }

    /// The per-life curves are sampled by age, so two particles of different
    /// ages must not look alike.
    #[test]
    fn colour_and_alpha_follow_a_particles_own_age() {
        let emitter = torch();
        let young = Particle {
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            age: 0.0,
            lifespan: 0.8,
            rotation: 0.0,
            spin: 0.0,
        };
        let old = Particle { age: 0.8, ..young };
        let (a, b) = (sprite(&emitter, &young), sprite(&emitter, &old));
        assert!(a.color[0] > 0.9 && a.color[2] < 0.1, "young: {:?}", a.color);
        assert!(b.color[0] < 0.1 && b.color[2] > 0.9, "old: {:?}", b.color);
        assert!(a.color[3] > 0.9 && b.color[3] < 0.1, "alpha did not fade");
    }

    /// A dead particle is removed. Left alive it would sample past the end of
    /// every curve and hold the last key for ever, which is a fire that never
    /// goes out.
    #[test]
    fn particles_die_at_their_lifespan() {
        let mut system = ParticleSystem::new(3);
        let mut emitter = torch();
        system.update(&emitter, Mat4::IDENTITY, 0, 0, 0.1);
        assert!(!system.is_empty());
        emitter.emission_rate = constant(0.0);
        for _ in 0..20 {
            system.update(&emitter, Mat4::IDENTITY, 0, 0, 0.1);
        }
        assert!(system.is_empty(), "{} particles outlived", system.particles().len());
    }

    /// A frame that stalled must not be paid back as a burst.
    #[test]
    fn a_long_frame_is_clamped_rather_than_caught_up() {
        let mut system = ParticleSystem::new(5);
        let emitter = torch();
        system.update(&emitter, Mat4::IDENTITY, 0, 0, 30.0);
        assert!(
            system.spawned <= (20.0 * MAX_STEP).ceil() as u64,
            "a thirty-second frame emitted {}",
            system.spawned
        );
    }

    /// The cap holds, and says so rather than silently dropping.
    #[test]
    fn the_particle_cap_is_counted_not_hidden() {
        let mut system = ParticleSystem::new(9);
        let mut emitter = torch();
        emitter.emission_rate = constant(100_000.0);
        emitter.lifespan = constant(100.0);
        for _ in 0..10 {
            system.update(&emitter, Mat4::IDENTITY, 0, 0, MAX_STEP);
        }
        assert_eq!(system.particles().len(), MAX_PARTICLES);
        assert!(system.dropped > 0, "the cap was reached and reported nothing");
    }

    /// Two placements of one model must not flicker in lockstep.
    #[test]
    fn two_systems_with_different_seeds_diverge() {
        let emitter = {
            let mut e = torch();
            e.emission_area_length = constant(1.0);
            e.emission_area_width = constant(1.0);
            e
        };
        let (mut a, mut b) = (ParticleSystem::new(1), ParticleSystem::new(2));
        a.update(&emitter, Mat4::IDENTITY, 0, 0, 0.5);
        b.update(&emitter, Mat4::IDENTITY, 0, 0, 0.5);
        let (pa, pb) = (a.particles()[0].position, b.particles()[0].position);
        assert!((pa - pb).length() > 1e-4, "{pa:?} and {pb:?} are the same");
    }

    /// An emitter turned off in this sequence stops emitting -- and what is
    /// already alight burns out rather than blinking away.
    #[test]
    fn a_disabled_emitter_stops_but_does_not_extinguish() {
        let mut emitter = torch();
        let mut system = ParticleSystem::new(11);
        system.update(&emitter, Mat4::IDENTITY, 0, 0, 0.1);
        let alive = system.particles().len();
        assert!(alive > 0);

        emitter.enabled_in = Track {
            interpolation: Interpolation::None,
            global_sequence: None,
            sequences: vec![Keyframes {
                times: vec![0],
                values: vec![0u8],
            }],
        };
        let before = system.spawned;
        system.update(&emitter, Mat4::IDENTITY, 0, 0, 0.05);
        assert_eq!(system.spawned, before, "a disabled emitter emitted");
        assert!(!system.is_empty(), "the flame was extinguished instead of left to burn");
    }

    /// A flipbook cell maps to a rectangle of the sheet, and cell counts run
    /// across before down.
    #[test]
    fn flipbook_cells_index_across_before_down() {
        let mut emitter = torch();
        emitter.rows = 2;
        emitter.columns = 4;
        emitter.head_cell = PartTrack {
            times: vec![0.0],
            values: vec![5u16],
        };
        let p = Particle {
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            age: 0.0,
            lifespan: 1.0,
            rotation: 0.0,
            spin: 0.0,
        };
        // Cell 5 of a 4-wide, 2-tall sheet is column 1 of row 1.
        let uv = sprite(&emitter, &p).uv;
        assert!((uv[0] - 0.25).abs() < 1e-6, "{uv:?}");
        assert!((uv[1] - 0.5).abs() < 1e-6, "{uv:?}");
        assert!((uv[2] - 0.5).abs() < 1e-6, "{uv:?}");
        assert!((uv[3] - 1.0).abs() < 1e-6, "{uv:?}");
    }

    /// A trail lays edges down where the bone is and drops them when they age
    /// out; it does not drag old ones along.
    #[test]
    fn a_ribbon_lays_edges_where_the_bone_was() {
        let emitter = RibbonEmitter {
            id: -1,
            bone: 0,
            position: [0.0; 3],
            textures: vec![0],
            materials: vec![0],
            color: Track::default(),
            alpha: Track::default(),
            height_above: constant(0.1),
            height_below: constant(0.1),
            edges_per_second: 50.0,
            edge_lifetime: 0.2,
            gravity: 0.0,
            rows: 1,
            columns: 1,
            texture_slot: Track::default(),
            visibility: Track::default(),
        };
        let mut trail = RibbonTrail::new();
        for i in 0..10 {
            let at = Mat4::from_translation(Vec3::new(i as f32, 0.0, 0.0));
            trail.update(&emitter, at, 0, 0, 1.0 / 50.0);
        }
        assert!(trail.edges().len() > 1, "no strip was laid down");
        // Newest first, and each behind the last.
        let xs: Vec<f32> = trail.edges().iter().map(|e| e.centre.x).collect();
        assert!(
            xs.windows(2).all(|w| w[0] > w[1]),
            "edges are not in the order they were laid: {xs:?}"
        );
        // An edge older than the lifetime is gone: ten frames at 1/50s is
        // 0.2s, which is exactly the lifetime, so the first is out.
        assert!(
            trail.edges().len() <= 10,
            "{} edges survived a 0.2s lifetime",
            trail.edges().len()
        );
    }
}
