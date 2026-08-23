//! Everything alight in a scene: which emitters are running, where their
//! particles have got to, and the geometry that comes out.
//!
//! This is where the three layers meet, and it lives in the viewer for the
//! same reason [`crate::model`] does. `m2` owns the emitter records and steps
//! them forward with no GPU in sight; `render` draws sprites and strips and
//! has never heard of an M2; only here is there both a world full of
//! placements and a device to draw them with.
//!
//! # A system belongs to a placement, not to a model
//!
//! Two braziers built from one file are two fires, and they must not flicker
//! in step. So the state is keyed by *placement* -- see
//! [`crate::world::Group::emitting_ids`] for why that key is a guid or a hash
//! and never a position in a vector.
//!
//! # Emitters that go out of view are forgotten, deliberately
//!
//! A system whose placement stopped being submitted this frame is dropped
//! rather than kept warm. Keeping them would grow without limit as a player
//! walks across a continent, and the cost of dropping one is that a fire
//! restarts when you turn back to look at it -- which is invisible, because a
//! plume reaches its steady state in under a second.

use std::collections::HashMap;

use std::borrow::Cow;

use glam::Mat4;
use render::particles::{Blend, ParticleRenderer, RibbonVertex, SpriteInstance};
use render::Gpu;

/// One emitter on one placement.
type Key = (u64, u16);

/// One contiguous run of geometry that shares a texture and a blend mode.
struct Batch {
    range: std::ops::Range<u32>,
    /// Address of the [`render::UploadedTexture`] the sprites sample, used to
    /// find the bind group. An address rather than an index because the same
    /// index means different textures on different models.
    sheet: usize,
    blend: Blend,
}

/// Merge adjacent ranges only. Keeping the original order preserves alpha
/// compositing while repeated placements that use the same sheet and blend
/// mode can share one draw submission.
fn push_batch(
    batches: &mut Vec<Batch>,
    range: std::ops::Range<u32>,
    sheet: usize,
    blend: Blend,
) {
    if let Some(previous) = batches.last_mut() {
        if previous.sheet == sheet
            && previous.blend == blend
            && previous.range.end == range.start
        {
            previous.range.end = range.end;
            return;
        }
    }
    batches.push(Batch {
        range,
        sheet,
        blend,
    });
}

/// A group of placements to step this frame.
///
/// Static groups borrow their stored arrays rather than copying them each
/// frame. A *held* torch's placement is the wielder's transform times the
/// animated hand, computed fresh each frame and stored nowhere, so that path
/// owns its matrices. Only models that actually emit reach here -- 6,429 of
/// the archives' 22,844 -- so the owned path is still a handful of matrices,
/// not a copy of the scene.
pub struct Source<'a> {
    /// The emitters themselves, and the textures they sample.
    ///
    /// Three borrowed slices rather than a `CachedModel`, so the offline model
    /// view can feed this too: that path holds a `LoadedModel`, and the two
    /// types differ in everything except the parts an emitter needs. A shared
    /// path is what makes `--model ... --screenshot` a way of *looking* at a
    /// flame, which is the only way an emitter can be checked at all.
    pub particles: &'a [m2::ParticleEmitter],
    pub ribbons: &'a [m2::RibbonEmitter],
    pub textures: &'a [render::UploadedTexture],
    /// One per placement, already in world space.
    pub placements: Cow<'a, [Mat4]>,
    /// Identities parallel to `placements`.
    pub ids: Cow<'a, [u64]>,
    /// The placement's posed skeleton, when it has one. `None` means the
    /// model is drawn in its bind pose, in which case every bone matrix is the
    /// identity and an emitter sits wherever its `position` says -- still, but
    /// in the right place. That is the case for every doodad.
    pub pose: Option<&'a [Mat4]>,
    /// Which animation the model is playing, and how far into it. Emitters
    /// carry a rate, a lifespan and an enable *per sequence*, so a torch can
    /// be authored to gutter in one cycle and roar in another.
    pub sequence: usize,
    pub time_ms: u32,
}

/// Live emitter state for a whole scene, and the geometry it produced.
pub struct Emitters {
    particles: HashMap<Key, m2::ParticleSystem>,
    ribbons: HashMap<Key, m2::RibbonTrail>,
    /// Bind groups for emitter texture sheets, keyed by the address of the
    /// uploaded texture.
    ///
    /// Cannot reuse `CachedModel::binds`: those were built against the mesh
    /// pipeline's layout, and a bind group belongs to the layout it was made
    /// with. Keyed by address because a texture lives as long as its model
    /// does and no two live textures share one.
    sheets: HashMap<usize, wgpu::BindGroup>,
    sprites: Vec<SpriteInstance>,
    vertices: Vec<RibbonVertex>,
    batches: Vec<Batch>,
    ribbon_batches: Vec<Batch>,
    /// All numbers, always. "Nothing was dropped" and "there was nothing to
    /// drop" are different states, and a counter that speaks only on failure
    /// cannot tell them apart -- this project has paid for that three times.
    pub live_systems: usize,
    pub live_sprites: usize,
    pub live_ribbons: usize,
    pub live_batches: usize,
}

impl Default for Emitters {
    fn default() -> Self {
        Self::new()
    }
}

impl Emitters {
    pub fn new() -> Self {
        Self {
            particles: HashMap::new(),
            ribbons: HashMap::new(),
            sheets: HashMap::new(),
            sprites: Vec::new(),
            vertices: Vec::new(),
            batches: Vec::new(),
            ribbon_batches: Vec::new(),
            live_systems: 0,
            live_sprites: 0,
            live_ribbons: 0,
            live_batches: 0,
        }
    }

    /// Steps every emitter and builds this frame's geometry.
    ///
    /// `dt` is real elapsed seconds. It is clamped inside the simulation, not
    /// here -- see `m2::particles::MAX_STEP` -- so a caller cannot forget.
    pub fn update<'a>(
        &mut self,
        gpu: &Gpu,
        renderer: &mut ParticleRenderer,
        sources: impl Iterator<Item = Source<'a>>,
        dt: f32,
    ) {
        self.sprites.clear();
        self.vertices.clear();
        self.batches.clear();
        self.ribbon_batches.clear();

        let mut seen: std::collections::HashSet<Key> = std::collections::HashSet::new();
        let mut blends: std::collections::BTreeSet<Blend> = Default::default();

        for source in sources {
            for (placement, &id) in source.placements.iter().zip(source.ids.iter()) {
                for (index, emitter) in source.particles.iter().enumerate() {
                    let key = (id, index as u16);
                    seen.insert(key);
                    let bone = source
                        .pose
                        .and_then(|pose| pose.get(emitter.bone as usize))
                        .copied()
                        .unwrap_or(Mat4::IDENTITY);
                    let system = self
                        .particles
                        .entry(key)
                        // Seeded from the placement's own identity, so two
                        // torches on one wall do not flicker in lockstep --
                        // and from the emitter index too, or a model with a
                        // flame and its own smoke would emit both in step.
                        .or_insert_with(|| {
                            m2::ParticleSystem::new((id as u32) ^ ((index as u32) << 16))
                        });
                    system.update(
                        emitter,
                        *placement * bone,
                        source.sequence,
                        source.time_ms,
                        dt,
                    );

                    let start = self.sprites.len() as u32;
                    let blend = Blend::from_m2(emitter.blend);
                    system.for_each_sprite(emitter, |sprite| {
                        self.sprites.push(SpriteInstance {
                            position: [
                                sprite.position[0],
                                sprite.position[1],
                                sprite.position[2],
                                sprite.rotation,
                            ],
                            size: [sprite.size[0], sprite.size[1], 0.0, 0.0],
                            color: renderer.encode(sprite.color),
                            uv: sprite.uv,
                        });
                    });
                    let end = self.sprites.len() as u32;
                    if end > start {
                        if let Some(sheet) =
                            source.textures.get(emitter.texture as usize)
                        {
                            blends.insert(blend);
                            push_batch(
                                &mut self.batches,
                                start..end,
                                std::ptr::from_ref(sheet) as usize,
                                blend,
                            );
                            self.sheets.entry(std::ptr::from_ref(sheet) as usize).or_insert_with(
                                || renderer.sheet_bind_group(gpu, &sheet.view),
                            );
                        } else {
                            // The emitter names a texture the model does not
                            // have. Not silently skipped: the survey says this
                            // happens on none of the 26,374 emitters in the
                            // archives, so if it ever happens it is a parse
                            // bug rather than odd data.
                            tracing::warn!(
                                "emitter {index} wants texture {} of {}, which does not exist",
                                emitter.texture,
                                source.textures.len(),
                            );
                            self.sprites.truncate(start as usize);
                        }
                    }
                }

                for (index, emitter) in source.ribbons.iter().enumerate() {
                    // Offset so a model with both a flame and a trail does not
                    // give them one key between them.
                    let key = (id, 0x8000 | index as u16);
                    seen.insert(key);
                    let bone = source
                        .pose
                        .and_then(|pose| pose.get(emitter.bone as usize))
                        .copied()
                        .unwrap_or(Mat4::IDENTITY);
                    let trail = self.ribbons.entry(key).or_default();
                    trail.update(
                        emitter,
                        *placement * bone,
                        source.sequence,
                        source.time_ms,
                        dt,
                    );

                    let start = self.vertices.len() as u32;
                    let (heights, color) =
                        m2::RibbonTrail::appearance(emitter, source.sequence, source.time_ms);
                    strip(&mut self.vertices, trail.edges(), heights, color);
                    let end = self.vertices.len() as u32;
                    if end > start {
                        // A ribbon names several textures and animates which
                        // one is showing; only the first is used here, which
                        // is right for every emitter measured (all 1,572 name
                        // exactly one) and stated rather than assumed.
                        let Some(sheet) = emitter
                            .textures
                            .first()
                            .and_then(|t| source.textures.get(*t as usize))
                        else {
                            self.vertices.truncate(start as usize);
                            continue;
                        };
                        // A trail is a glow. There is no blend field on a
                        // ribbon record at all -- it names a *material* index,
                        // and resolving that is work this does not do yet, so
                        // additive is chosen because every ribbon in the
                        // archives is a magical effect.
                        blends.insert(Blend::Additive);
                        push_batch(
                            &mut self.ribbon_batches,
                            start..end,
                            std::ptr::from_ref(sheet) as usize,
                            Blend::Additive,
                        );
                        self.sheets
                            .entry(std::ptr::from_ref(sheet) as usize)
                            .or_insert_with(|| renderer.sheet_bind_group(gpu, &sheet.view));
                    }
                }
            }
        }

        // Anything not submitted this frame has gone out of view or died. See
        // the module note on why it is dropped rather than kept warm.
        self.particles.retain(|key, _| seen.contains(key));
        self.ribbons.retain(|key, _| seen.contains(key));
        // A bind group outlives its texture only if the model was evicted, and
        // a stale one would then reference a freed view. Pruned against what
        // this frame actually used, for the same reason the systems are.
        let used: std::collections::HashSet<usize> = self
            .batches
            .iter()
            .chain(&self.ribbon_batches)
            .map(|b| b.sheet)
            .collect();
        self.sheets.retain(|address, _| used.contains(address));

        self.live_systems = self.particles.len() + self.ribbons.len();
        self.live_sprites = self.sprites.len();
        self.live_ribbons = self.vertices.len() / 6;
        self.live_batches = self.batches.len() + self.ribbon_batches.len();

        renderer.begin(gpu, blends);
        renderer.reserve(gpu, self.sprites.len(), self.vertices.len());
        renderer.upload_sprites(gpu, &self.sprites);
        renderer.upload_ribbons(gpu, &self.vertices);
    }

    /// Records everything into a pass that already holds the world and its
    /// depth.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, renderer: &ParticleRenderer) {
        for batch in &self.batches {
            let Some(sheet) = self.sheets.get(&batch.sheet) else {
                continue;
            };
            renderer.draw_sprites(pass, sheet, batch.blend, batch.range.clone());
        }
        for batch in &self.ribbon_batches {
            let Some(sheet) = self.sheets.get(&batch.sheet) else {
                continue;
            };
            renderer.draw_ribbon(pass, sheet, batch.blend, batch.range.clone());
        }
    }

    /// One line for the debug overlay.
    pub fn describe(&self) -> String {
        format!(
            "{} emitter(s) alight, {} particles, {} particle batches, {} trail quads",
            self.live_systems, self.live_sprites, self.live_batches, self.live_ribbons
        )
    }
}

/// Turns a trail's edges into two triangles per gap.
///
/// The strip's width comes from each edge's *own* up axis, captured when it was
/// laid down -- see `m2::particles::Edge::up`. Rebuilding it from the bone's
/// current orientation instead twists the whole ribbon along its length every
/// time the wielder turns, which reads as the texture being wrong.
fn strip(out: &mut Vec<RibbonVertex>, edges: &[m2::particles::Edge], heights: [f32; 2], color: [f32; 4]) {
    if edges.len() < 2 {
        return;
    }
    let corner = |edge: &m2::particles::Edge, upper: bool, u: f32| RibbonVertex {
        position: (edge.centre
            + edge.up * if upper { heights[0] } else { -heights[1] })
        .to_array(),
        _pad: 0.0,
        uv: [u, if upper { 0.0 } else { 1.0 }],
        _pad2: [0.0; 2],
        color,
    };

    let last = (edges.len() - 1) as f32;
    for (i, pair) in edges.windows(2).enumerate() {
        let (near, far) = (&pair[0], &pair[1]);
        // `u` runs along the strip from its head, so the texture is laid down
        // the trail rather than repeated per segment.
        let (u0, u1) = (i as f32 / last, (i + 1) as f32 / last);
        // The tail fades out; the head is at full strength. Without this a
        // ribbon ends in a hard edge that pops when its last segment expires.
        let fade = |t: f32| {
            let mut c = color;
            c[3] *= 1.0 - t;
            c
        };
        let mut a = corner(near, true, u0);
        let mut b = corner(near, false, u0);
        let mut c = corner(far, true, u1);
        let mut d = corner(far, false, u1);
        a.color = fade(u0);
        b.color = fade(u0);
        c.color = fade(u1);
        d.color = fade(u1);
        out.extend_from_slice(&[a, b, c, b, d, c]);
    }
}

/// Where the viewer's own emitters go when there is no world, only a model.
///
/// The offline model view draws one model at the origin, which is the cheapest
/// way to *look* at an emitter -- and looking is the only way to check one.
/// A composite assembled at runtime needs a way to be seen as itself.
pub fn single_model<'a>(
    model: &'a crate::model::LoadedModel,
    pose: &'a [Mat4],
    sequence: usize,
    time_ms: u32,
) -> Source<'a> {
    Source {
        particles: &model.particles,
        ribbons: &model.ribbons,
        textures: &model.textures,
        placements: vec![Mat4::IDENTITY].into(),
        ids: vec![1].into(),
        pose: Some(pose),
        sequence,
        time_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn edge(x: f32, age: f32) -> m2::particles::Edge {
        m2::particles::Edge {
            centre: Vec3::new(x, 0.0, 0.0),
            up: Vec3::Z,
            age,
        }
    }

    /// Two edges make one quad; three make two. A strip of one makes nothing,
    /// which is the case that would otherwise index past the end.
    #[test]
    fn a_strip_is_two_triangles_per_gap() {
        let mut out = Vec::new();
        strip(&mut out, &[edge(0.0, 0.0)], [0.1, 0.1], [1.0; 4]);
        assert!(out.is_empty(), "a single edge is not a strip");

        strip(&mut out, &[edge(0.0, 0.0), edge(1.0, 0.1)], [0.1, 0.1], [1.0; 4]);
        assert_eq!(out.len(), 6);

        out.clear();
        strip(
            &mut out,
            &[edge(0.0, 0.0), edge(1.0, 0.1), edge(2.0, 0.2)],
            [0.1, 0.1],
            [1.0; 4],
        );
        assert_eq!(out.len(), 12);
    }

    /// The strip has width, and it has it along each edge's own axis.
    #[test]
    fn a_strip_is_widened_along_the_edges_up_axis() {
        let mut out = Vec::new();
        strip(&mut out, &[edge(0.0, 0.0), edge(1.0, 0.1)], [0.25, 0.75], [1.0; 4]);
        let zs: Vec<f32> = out.iter().map(|v| v.position[2]).collect();
        let top = zs.iter().cloned().fold(f32::MIN, f32::max);
        let bottom = zs.iter().cloned().fold(f32::MAX, f32::min);
        assert!((top - 0.25).abs() < 1e-5, "top at {top}");
        assert!((bottom + 0.75).abs() < 1e-5, "bottom at {bottom}");
    }

    /// The tail is fainter than the head, so a strip does not end in a line
    /// that pops when its last segment expires.
    #[test]
    fn a_strip_fades_towards_its_tail() {
        let mut out = Vec::new();
        strip(
            &mut out,
            &[edge(0.0, 0.0), edge(1.0, 0.1), edge(2.0, 0.2)],
            [0.1, 0.1],
            [1.0; 4],
        );
        let head = out.first().unwrap().color[3];
        let tail = out.last().unwrap().color[3];
        assert!(head > tail, "head {head} is not brighter than tail {tail}");
        assert!(tail < 0.01, "the tail did not reach transparent: {tail}");
    }

    #[test]
    fn only_adjacent_batches_with_the_same_material_are_merged() {
        let mut batches = Vec::new();
        push_batch(&mut batches, 0..4, 7, Blend::Alpha);
        push_batch(&mut batches, 4..9, 7, Blend::Alpha);
        push_batch(&mut batches, 9..11, 7, Blend::Additive);
        push_batch(&mut batches, 11..12, 8, Blend::Additive);
        push_batch(&mut batches, 12..13, 7, Blend::Additive);

        assert_eq!(batches.len(), 4);
        assert_eq!(batches[0].range, 0..9);
        assert_eq!(batches[1].range, 9..11);
        assert_eq!(batches[2].range, 11..12);
        assert_eq!(batches[3].range, 12..13);
    }
}
