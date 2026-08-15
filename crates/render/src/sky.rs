//! The sky, drawn as a gradient rather than cleared to a colour.
//!
//! `Light.dbc` describes the sky as five colours stacked from zenith to
//! horizon -- see `dbc::light::bands::SKY`, where the identification is written
//! up. This draws them.
//!
//! **There is no skybox and this is not a substitute for one.**
//! `LightParams.light_skybox_id` is 0 on the row that lights Elwynn, and the
//! 124 rows `LightSkybox.dbc` does have are named things like
//! `StratholmeSkybox` and `CavernsOfTimeSky` -- special places, not the
//! ordinary outdoor world. The gradient is what the ordinary outdoor world
//! actually has.
//!
//! No geometry, no dome mesh: one oversized triangle, and the view ray per
//! pixel is unprojected from the very matrix the scene is drawn with. That last
//! part is deliberate and is the same rule the picking ray follows -- a sky
//! rebuilt from the camera's angles would agree with the scene only until
//! somebody changed the projection, and the failure would be a horizon that
//! sits slightly off the ground it meets.

use bytemuck::{Pod, Zeroable};

use crate::{mesh::DEPTH_FORMAT, Gpu};

/// How many colours the gradient has. Matches `dbc::light::SkyGradient`, which
/// is `[[f32; 3]; 5]` -- kept as a plain array here because this crate knows
/// about GPUs and not about DBC tables.
pub const LAYERS: usize = 5;

/// The gradient, zenith first and horizon last.
pub type Gradient = [[f32; 3]; LAYERS];

/// One channel of a `Light.dbc` colour, in the space the GPU wants it.
///
/// **The bands are display values, and an sRGB target re-encodes what a shader
/// writes to it.** `LightIntBand` stores bytes the original client pushed
/// straight at an 8-bit framebuffer, so 49 meant 49 on screen. Handed to an
/// sRGB target as-is, 49/255 is read as a *linear* 0.19 and encoded up to 123 --
/// and the error grows the darker the colour is, which is why it went unnoticed
/// in every daylight render and turned midnight over Elwynn into a bright
/// afternoon blue. Undoing the encode here is what makes the byte in the table
/// the byte on the screen.
///
/// The same double-encode applies to the diffuse and ambient terms, and is
/// deliberately **not** fixed here: those multiply textures that were decoded to
/// linear on sample, so what space their factor belongs in is a real question
/// rather than an arithmetic slip. The sky is written straight to the target,
/// where there is only one right answer.
pub fn to_linear(channel: f32) -> f32 {
    if channel <= 0.040_45 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    inverse_view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    /// Zenith first. `vec4` rather than `vec3` because a uniform array of
    /// `vec3` is padded to 16 bytes per element anyway, and spelling that out
    /// is cheaper than discovering it as a colour that reads the next band's
    /// red channel.
    layers: [[f32; 4]; LAYERS],
    /// Unit direction towards the sun or moon, with how visible it is in `w`.
    body: [f32; 4],
    /// Its colour, from `Light.dbc` band 9.
    body_colour: [f32; 4],
}

const SHADER: &str = r#"
const LAYERS: u32 = 5u;

struct Params {
    inverse_view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    layers: array<vec4<f32>, LAYERS>,
    body: vec4<f32>,
    body_colour: vec4<f32>,
};

// How big the sun is, as the cosine of its angular radius, and how far its
// glow reaches. **Chosen.** The real sun subtends half a degree; every game
// ever made draws it larger and the original is no exception. A degree and a
// half of disc with a wide soft falloff is what reads as a sun rather than as
// a headlight.
const DISC_COS: f32 = 0.99966;   // ~1.5 degrees
const DISC_EDGE: f32 = 0.99930;  // ~2.1 degrees, for a soft rim
const GLOW_POWER: f32 = 220.0;

@group(0) @binding(0) var<uniform> params: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

// One oversized triangle covers the viewport without a vertex buffer.
@vertex
fn vs(@builtin(vertex_index) idx: u32) -> VsOut {
    var out: VsOut;
    let corner = vec2<f32>(f32((idx << 1u) & 2u), f32(idx & 2u));
    out.ndc = corner * 2.0 - 1.0;
    // Depth 1.0 is the far plane, which is where a sky belongs even though the
    // pipeline writes no depth: it means a driver that ignores the write mask
    // still loses every subsequent comparison rather than winning them all.
    out.pos = vec4<f32>(out.ndc, 1.0, 1.0);
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    // Unproject the far plane and look back at the eye. The perspective
    // divide matters: without it a wide field of view bends the horizon.
    let far = params.inverse_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let dir = normalize(far.xyz / far.w - params.eye.xyz);

    // The world is Z-up, so the Z component of a unit ray *is* the sine of its
    // elevation. Below the horizon the gradient holds rather than mirroring:
    // there is ground down there, and where there is not, the sky should meet
    // it at its own colour rather than climbing back towards the zenith.
    let elevation = asin(clamp(dir.z, -1.0, 1.0));
    let t = clamp(elevation / (3.14159265 * 0.5), 0.0, 1.0);

    // **The heights are chosen; only the order is measured.** Nothing in
    // `Light.dbc` says how far up each of the five sits -- the original client
    // keeps that in a dome mesh, not in a table -- so this is the same kind of
    // choice as the sun's arc, and is written down as one.
    //
    // Evenly spaced was the first attempt and a render refused it: at 22.5
    // degrees apart the two blue layers sit above 60 degrees, which a camera
    // parked behind a character's shoulder never looks at, and midday Elwynn
    // came back under a nearly white sky. Weighting them towards the horizon
    // is not just a nicer picture, it is the direction the physics goes -- the
    // air a view ray crosses grows as the ray flattens, so a sky changes
    // fastest at the bottom and barely at all near the top. Squaring puts the
    // layers at 0, 5.6, 22.5, 50.6 and 90 degrees.
    let u = pow(t, 0.5);

    // `u` is 0 at the horizon and 1 overhead; the array runs the other way.
    let f = (1.0 - u) * f32(LAYERS - 1u);
    let lower = u32(floor(f));
    let upper = min(lower + 1u, LAYERS - 1u);
    var colour = mix(
        params.layers[lower].rgb,
        params.layers[upper].rgb,
        f - floor(f),
    );

    // The sun, or the moon -- one band serves both, because only one of them
    // is ever up. See `dbc::light::bands::DISC`.
    //
    // Added to the sky rather than blended over it: a disc and its glow are
    // light *arriving*, so they brighten what is behind them instead of
    // replacing it. That is also what makes a storm dim them for free, since
    // `body.w` carries how much of the sky is not overcast.
    let facing = dot(dir, params.body.xyz);
    if (params.body.w > 0.0 && facing > 0.0) {
        // **Fading in from the horizon is what makes the handover smooth.**
        // The caller draws whichever of the two is up, so the direction
        // handed here jumps to the opposite side of the sky the instant the
        // sun sets. Without a fade that is a disc teleporting across the
        // horizon; with one, the sun dims out as it goes down and the moon
        // comes up dim on the other side, and the crossing is invisible
        // because both are at zero when it happens.
        let risen = smoothstep(0.0, 0.12, params.body.xyz.z);
        let strength = params.body.w * risen;
        let glow = pow(facing, GLOW_POWER);
        let disc = smoothstep(DISC_EDGE, DISC_COS, facing);
        colour += params.body_colour.rgb * (glow * 0.45 + disc) * strength;
    }
    return vec4<f32>(colour, 1.0);
}
"#;

/// Draws the sky gradient into a pass that already has the world's depth
/// buffer attached.
pub struct SkyRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    /// Whether the target will encode what the shader writes -- see
    /// [`to_linear`]. Asked of the format rather than assumed, because the
    /// offscreen path and the surface do not have to agree on it.
    srgb_target: bool,
}

impl SkyRenderer {
    pub fn new(gpu: &Gpu, target: wgpu::TextureFormat) -> Self {
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("sky"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });

        let bind_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("sky binds"),
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
                label: Some("sky layout"),
                bind_group_layouts: &[Some(&bind_layout)],
                immediate_size: 0,
            });

        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("sky"),
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
                    targets: &[Some(target.into())],
                }),
                primitive: wgpu::PrimitiveState::default(),
                // Writes no depth and refuses none: the sky is drawn first and
                // everything in the world is meant to cover it. Sharing the
                // world's depth attachment rather than running in a pass of its
                // own is what lets it do that without a second clear.
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

        let params = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sky params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_layout,
            params,
            srgb_target: target.is_srgb(),
        }
    }

    /// A `Light.dbc` colour in the space this renderer's target wants.
    ///
    /// Public because the pass's clear colour has to make the same conversion,
    /// and a second copy of it would be a second thing to get wrong.
    pub fn encode(&self, colour: [f32; 3]) -> [f32; 3] {
        if self.srgb_target {
            colour.map(to_linear)
        } else {
            colour
        }
    }

    /// Records the sky into `pass`.
    ///
    /// `view_proj` must be the matrix the rest of the pass is drawn with -- it
    /// is inverted here rather than rebuilt, so passing anything else makes the
    /// horizon disagree with the ground.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        gpu: &Gpu,
        pass: &mut wgpu::RenderPass<'_>,
        view_proj: glam::Mat4,
        eye: glam::Vec3,
        gradient: &Gradient,
        // Direction *towards* the sun. The moon is drawn at the opposite end
        // of the same arc once this points below the horizon.
        sun: glam::Vec3,
        // The disc's colour, `Light.dbc` band 9.
        disc: [f32; 3],
        // How much of it gets through: 1 in clear weather, 0 under a storm.
        visibility: f32,
    ) {
        // Converted per layer here rather than per pixel in the shader, so the
        // blend between two layers happens in linear light -- which is what
        // stops a gradient running down to black from banding.
        let mut layers = [[0.0f32; 4]; LAYERS];
        for (out, colour) in layers.iter_mut().zip(gradient) {
            let [r, g, b] = self.encode(*colour);
            *out = [r, g, b, 1.0];
        }
        // Whichever of the two is above the horizon. `sun` is an arc that goes
        // below at night, and the moon is simply the other end of it -- which
        // is why one colour band serves both.
        let body = if sun.z >= 0.0 { sun } else { -sun };
        let [br, bg, bb] = self.encode(disc);
        gpu.queue.write_buffer(
            &self.params,
            0,
            bytemuck::bytes_of(&Params {
                inverse_view_proj: view_proj.inverse().to_cols_array_2d(),
                eye: [eye.x, eye.y, eye.z, 1.0],
                layers,
                body: [body.x, body.y, body.z, visibility.clamp(0.0, 1.0)],
                body_colour: [br, bg, bb, 1.0],
            }),
        );

        let binds = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sky binds"),
            layout: &self.bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: self.params.as_entire_binding(),
            }],
        });

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &binds, &[]);
        pass.draw(0..3, 0..1);
    }
}
