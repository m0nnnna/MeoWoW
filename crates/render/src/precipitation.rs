//! Rain and snow: a field of camera-relative billboards, drawn over the world.
//!
//! **No vertex buffer, no instance buffer, and no CPU-side particle list.** A
//! raindrop needs a position, a speed and nothing else, and all three are pure
//! functions of the drop's index -- so the draw is `draw(0..6, 0..count)` and
//! every drop hashes its own seed in the vertex shader. That is not cleverness
//! for its own sake: a CPU-side list would have to be uploaded every frame to
//! move, and the thing being uploaded would be entirely derivable from a
//! counter.
//!
//! The field is an axis-aligned box that **follows the camera by wrapping**.
//! Each drop's position is taken modulo the box around the eye, so walking
//! forward brings drops round from behind rather than leaving the weather
//! behind, and no drop has to be created or destroyed. Drops are
//! indistinguishable, so the wrap is invisible.
//!
//! This is deliberately *not* an M2 particle system. Precipitation in the
//! original client is camera-relative geometry rather than a model in the
//! scene, and `Light.dbc`'s weather already says how hard it is falling --
//! whereas emitters on a torch or a spell need the M2 emitter block, per-bone
//! placement, and colour and alpha tracks over a particle's life. Doing this
//! one first is what gives those a billboard path to arrive into.

use bytemuck::{Pod, Zeroable};

use crate::{mesh::DEPTH_FORMAT, Gpu};

/// What is falling. Mirrors `world::Precipitation`, which this crate cannot
/// see: `render` knows about GPUs, not about the protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Rain,
    Snow,
}

/// How the field is shaped, in world units.
///
/// **Every number here is chosen.** Nothing in the game's data describes a
/// raindrop -- `SMSG_WEATHER` gives a state and an intensity and stops -- so
/// these are tuned by looking, and they live in one struct so that looking is
/// the only thing needed to change them.
#[derive(Clone, Copy, Debug)]
pub struct Shape {
    /// Edge of the cube of weather that follows the camera. Large enough that
    /// the far face is past anything the eye reads as "near", small enough that
    /// a given drop count still fills it.
    pub box_size: f32,
    /// How fast drops fall, in units per second.
    pub fall_speed: f32,
    /// Half-width of a drop, across its travel.
    pub width: f32,
    /// Half-length of a drop, along its travel. Rain is a streak because it
    /// falls further than the eye integrates; snow is very nearly round.
    pub length: f32,
    /// Sideways drift, in units per second. Rain slants; snow wanders.
    pub drift: f32,
    /// How far off plumb a drop's *streak* leans, as a slope in x and y.
    /// Separate from `drift`, which moves where a drop is rather than which
    /// way it points -- and needed because rain falling exactly along the
    /// world's up axis draws every streak parallel to the window's edges and
    /// reads as a fence.
    pub slant: [f32; 2],
    /// How many drops at full intensity.
    pub density: u32,
    /// How opaque a drop is at full intensity.
    pub alpha: f32,
    /// 0 draws a streak with hard ends, 1 a round flake.
    ///
    /// A raindrop's ends are hard because they are where it *was* and where it
    /// will be -- the streak is the exposure, not the drop. A snowflake is
    /// slow enough to be seen as itself, so it needs a falloff along its
    /// travel as well as across it, and without one it reads as a tiny bar.
    pub roundness: f32,
}

impl Shape {
    pub const RAIN: Self = Self {
        box_size: 60.0,
        fall_speed: 42.0,
        width: 0.018,
        length: 0.5,
        drift: 5.0,
        slant: [0.16, 0.09],
        density: 16000,
        alpha: 0.45,
        roundness: 0.0,
    };

    /// Snow falls an order of magnitude slower and is a flake rather than a
    /// streak, which is most of what distinguishes the two on screen.
    pub const SNOW: Self = Self {
        box_size: 60.0,
        fall_speed: 4.5,
        width: 0.055,
        length: 0.07,
        drift: 1.5,
        // Snow drifts rather than slants: it has no speed to be blown off.
        slant: [0.03, 0.02],
        density: 5000,
        alpha: 0.8,
        roundness: 1.0,
    };

    pub fn for_kind(kind: Kind) -> Self {
        match kind {
            Kind::Rain => Self::RAIN,
            Kind::Snow => Self::SNOW,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    /// Colour, with the drawn alpha in `w`.
    colour: [f32; 4],
    /// `box_size`, `fall_speed`, `width`, `length`.
    shape: [f32; 4],
    /// `time` in seconds, `drift`, then the streak's slant in x and y.
    motion: [f32; 4],
    /// `roundness`, and three spare.
    extra: [f32; 4],
}

const SHADER: &str = r#"
struct Params {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    colour: vec4<f32>,
    shape: vec4<f32>,
    motion: vec4<f32>,
    extra: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;

// Where the near fade runs from and to, in world units. Chosen by looking:
// close enough that the field still surrounds the camera, far enough that no
// drop is drawn at a size the eye reads as geometry.
const NEAR_FADE_FROM: f32 = 1.5;
const NEAR_FADE_TO: f32 = 7.0;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    /// Position across the drop and along it, -1 to 1, for the soft edges.
    @location(0) across: f32,
    @location(2) along: f32,
    /// How much of the drop to draw, for the near fade.
    @location(1) fade: f32,
};

// An integer hash, so a drop's seed is a function of its index and nothing has
// to be stored. Any decent avalanche will do; what matters is that three
// consecutive indices do not give three correlated coordinates, which is why
// each axis hashes a different derived key rather than an adjacent one.
fn hash(x: u32) -> u32 {
    var h = x;
    h ^= h >> 16u;
    h *= 0x7feb352du;
    h ^= h >> 15u;
    h *= 0x846ca68bu;
    h ^= h >> 16u;
    return h;
}

fn rand01(x: u32) -> f32 {
    return f32(hash(x) & 0xffffffu) / f32(0x1000000u);
}

@vertex
fn vs(@builtin(vertex_index) vertex: u32, @builtin(instance_index) drop: u32) -> VsOut {
    let box_size = params.shape.x;
    let time = params.motion.x;

    let key = drop * 4u;
    let seed = vec3<f32>(rand01(key), rand01(key + 1u), rand01(key + 2u));
    // Speed varies per drop, or the whole field falls as one sheet and reads
    // as a moving texture rather than as weather.
    let speed = params.shape.y * (0.75 + 0.5 * rand01(key + 3u));

    var p = seed * box_size;
    p.z -= speed * time;
    // Drift is a slow circle rather than a straight line, so drops do not
    // eventually all agree on a direction the way a constant wind would.
    let phase = seed.x * 6.2831853;
    p.x += params.motion.y * sin(time * 0.7 + phase);
    p.y += params.motion.y * cos(time * 0.5 + phase);

    // **The wrap is what makes the field infinite.** Taking the offset from
    // the eye modulo the box keeps every drop within half a box of the camera
    // however far it walks or however long it falls, and costs one fract.
    let rel = p - params.eye.xyz;
    let centre = params.eye.xyz + (fract(rel / box_size + 0.5) - 0.5) * box_size;

    // Two triangles, corners derived from the vertex index rather than looked
    // up, so there is no buffer at all.
    let corner = vec2<f32>(
        select(-1.0, 1.0, (vertex & 1u) == 1u),
        select(-1.0, 1.0, ((vertex + 1u) / 3u % 2u) == 1u),
    );

    // A drop is a billboard around its travel: `fall` is the way it goes and
    // `across` is perpendicular to both that and the view, which is what keeps
    // a streak edge-on-proof from any angle.
    let offset = params.eye.xyz - centre;
    let range = length(offset);
    let to_eye = offset / max(range, 1e-6);
    // Slanted rather than plumb. Rain that falls exactly along the world's up
    // axis reads as a picket fence, because every streak on screen is then
    // parallel to every other and to the edges of the window.
    let fall = normalize(vec3<f32>(params.motion.z, params.motion.w, 1.0));
    var across = cross(fall, to_eye);
    let len = length(across);
    // Looking straight down the fall axis leaves no perpendicular; any
    // direction is then as good as another and the drop is a dot anyway.
    across = select(vec3<f32>(1.0, 0.0, 0.0), across / max(len, 1e-6), len > 1e-4);

    let world = centre + across * corner.x * params.shape.z + fall * corner.y * params.shape.w;

    var out: VsOut;
    out.pos = params.view_proj * vec4<f32>(world, 1.0);
    out.across = corner.x;
    out.along = corner.y;
    // **Fade out what is close.** A drop is a fixed size in the world, so one
    // an arm's length from the eye covers a third of the screen and reads as a
    // white bar rather than as rain -- which is exactly what the first render
    // of this looked like. Near drops are also the ones a real eye cannot
    // focus on, so removing them is not a cheat. The far end fades too, or the
    // wrapping box would end in a wall of drops at a visible distance.
    let near = smoothstep(NEAR_FADE_FROM, NEAR_FADE_TO, range);
    let far = 1.0 - smoothstep(box_size * 0.35, box_size * 0.5, range);
    out.fade = near * far;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    // Soft across the drop and hard along it: a raindrop's edges blur, its
    // ends do not, because the ends are where it was and where it will be.
    let across = 1.0 - abs(in.across);
    let along = 1.0 - abs(in.along);
    // A streak keeps its ends; a flake loses them.
    let ends = mix(1.0, along * along, params.extra.x);
    return vec4<f32>(
        params.colour.rgb,
        params.colour.a * across * across * ends * in.fade,
    );
}
"#;

/// Draws rain or snow over a world that has already been rendered.
pub struct PrecipitationRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
}

impl PrecipitationRenderer {
    pub fn new(gpu: &Gpu, target: wgpu::TextureFormat) -> Self {
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("precipitation"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });

        let bind_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("precipitation binds"),
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    }],
                });

        let layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("precipitation layout"),
                bind_group_layouts: &[Some(&bind_layout)],
                immediate_size: 0,
            });

        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("precipitation"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    // Billboards face wherever the maths put them, and a drop
                    // seen from behind is still a drop.
                    cull_mode: None,
                    ..Default::default()
                },
                // **Tests depth but does not write it.** Testing hides drops
                // behind a hill, which is what stops rain falling through the
                // abbey wall; not writing lets thousands of unsorted drops
                // blend without each one occluding the next by accident.
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

        let params = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("precipitation params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_layout,
            params,
        }
    }

    /// How many drops an intensity is worth.
    ///
    /// Squared rather than linear because the server eases weather in over
    /// tens of seconds, and a linear count makes the first tenth of a storm
    /// arrive as a visible scattering of drops out of a clear sky. Nothing else
    /// depends on the curve, so it is stated here rather than argued about at
    /// the call site.
    pub fn drops(shape: &Shape, intensity: f32) -> u32 {
        let t = intensity.clamp(0.0, 1.0);
        (shape.density as f32 * t * t) as u32
    }

    /// Records the weather into `pass`, which must already hold the world and
    /// its depth.
    ///
    /// `colour` is expected in the target's own space -- pass it through
    /// [`crate::sky::SkyRenderer::encode`] alongside the sky, or the rain will
    /// be brighter than the sky it falls out of.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        gpu: &Gpu,
        pass: &mut wgpu::RenderPass<'_>,
        view_proj: glam::Mat4,
        eye: glam::Vec3,
        shape: &Shape,
        colour: [f32; 3],
        intensity: f32,
        seconds: f32,
    ) {
        let drops = Self::drops(shape, intensity);
        if drops == 0 {
            return;
        }
        // **Wrapped here rather than trusted from the caller.** A drop's fall
        // is `speed * seconds`, and an `f32` holding half an hour of it has
        // lost enough precision to quantise a drop narrower than its own
        // width -- so rain would slowly turn into a flickering grid on a
        // client left running. Ten minutes keeps the product small, and the
        // discontinuity is a field of identical drops jumping, which is
        // nothing. The clamp lives at the point it cannot be skipped, the same
        // reason `Camera::radians_per_pixel` guards on every call.
        let seconds = seconds.rem_euclid(600.0);
        gpu.write_buffer(
            &self.params,
            0,
            bytemuck::bytes_of(&Params {
                view_proj: view_proj.to_cols_array_2d(),
                eye: [eye.x, eye.y, eye.z, 1.0],
                colour: [
                    colour[0],
                    colour[1],
                    colour[2],
                    shape.alpha * intensity.clamp(0.0, 1.0),
                ],
                shape: [shape.box_size, shape.fall_speed, shape.width, shape.length],
                motion: [seconds, shape.drift, shape.slant[0], shape.slant[1]],
                extra: [shape.roundness, 0.0, 0.0, 0.0],
            }),
        );

        let binds = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("precipitation binds"),
            layout: &self.bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: self.params.as_entire_binding(),
            }],
        });

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &binds, &[]);
        pass.draw(0..6, 0..drops);
    }
}
