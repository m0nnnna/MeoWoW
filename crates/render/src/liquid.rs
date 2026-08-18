//! Water, lava and slime: a transparent sheet laid over the terrain.
//!
//! Its own pipeline for the same reason terrain has one -- the material is
//! unlike anything else in the scene. A liquid surface blends rather than
//! occludes, scrolls rather than sits still, and **lava is not lit at all**: it
//! is the one surface in this world that emits rather than reflects, so running
//! it through the sun-and-ambient term the terrain and mesh shaders share would
//! make a lava lake go dark at midnight.
//!
//! # The animation is on the CPU, and deliberately
//!
//! `LiquidType.dbc` names its art with a `%d` standing for a frame number, and
//! there are thirty of them for water. Cross-fading two frames in the shader
//! would need a texture array, a second sampler and a blend factor; picking
//! *which frame's bind group to bind* needs none of those and produces the same
//! picture, because the frames are authored to be played rather than mixed.
//! Which frame is a function of the clock alone, so the caller passes a time
//! and [`LiquidRenderer`] does no state-keeping at all.
//!
//! # Depth is what stops a shore being a hard blue line
//!
//! Every vertex carries how deep the liquid is beneath it, and the alpha is
//! taken from that. Without it a river is a flat sheet that ends abruptly along
//! a cell boundary, which reads as a rendering bug rather than as a bank.

use crate::mesh::DEPTH_FORMAT;
use crate::Gpu;
use bytemuck::{Pod, Zeroable};

/// One corner of a liquid cell.
///
/// The tint and the emissive flag ride on the vertex rather than in a
/// per-sheet uniform. A tile has a few dozen sheets and each would otherwise
/// need its own uniform buffer and bind group; the alternative costs eight
/// floats per vertex on geometry that is two triangles per cell.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct LiquidVertex {
    pub position: [f32; 3],
    /// `u`, `v`, then how fast the surface scrolls and whether it is emissive.
    ///
    /// Packed together because a vertex attribute is a `vec4` whatever is put
    /// in it, and three of these four are per-sheet constants that would
    /// otherwise want a uniform each.
    pub uv_motion: [f32; 4],
    /// Surface colour, with the drawn alpha in `w`.
    pub tint: [f32; 4],
    /// `emissive`, then whether the surface art is **alpha-keyed**.
    ///
    /// The second is the one that decides whether this looks like water at
    /// all. See [`Look::alpha_keyed`].
    pub mode: [f32; 2],
}

/// Per-frame state every liquid sheet shares.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    /// Direction towards the sun, with `w` non-zero once real light data
    /// exists -- the same convention the terrain shader uses, so the two agree
    /// about whether they are lit or on the fixed headlight fallback.
    light: [f32; 4],
    sun: [f32; 4],
    ambient: [f32; 4],
    fog: [f32; 4],
    /// Fog start and end in `x` and `y`, then the clock in seconds.
    fog_range: [f32; 4],
}

const SHADER: &str = r#"
struct Params {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    light: vec4<f32>,
    sun: vec4<f32>,
    ambient: vec4<f32>,
    fog: vec4<f32>,
    fog_range: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(1) @binding(0) var surface: texture_2d<f32>;
@group(1) @binding(1) var surface_sampler: sampler;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) uv_motion: vec4<f32>,
    @location(2) tint: vec4<f32>,
    @location(3) mode: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec4<f32>,
    @location(2) world: vec3<f32>,
    @location(3) emissive: f32,
    @location(4) alpha_keyed: f32,
};

@vertex
fn vs(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = params.view_proj * vec4<f32>(in.position, 1.0);
    out.world = in.position;
    out.tint = in.tint;
    out.emissive = in.mode.x;
    out.alpha_keyed = in.mode.y;

    // The two axes scroll at different rates. Equal rates slide the whole
    // sheet along one diagonal, which reads as the texture being dragged
    // rather than as water moving.
    let time = params.fog_range.z;
    let speed = in.uv_motion.z;
    out.uv = in.uv_motion.xy + vec2<f32>(time * speed, time * speed * 0.63);
    return out;
}

// How dark the troughs between ripples get on an alpha-keyed surface. The
// crests reach the tint exactly; nothing goes to black, because the water has
// a colour of its own that the ripples modulate rather than supply.
const RIPPLE_FLOOR: f32 = 0.62;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let texel = textureSample(surface, surface_sampler, in.uv);

    // **Water's art stores no colour, and lava's does.** `lake_a` and
    // `ocean_h` are near-black -- mean RGB 3.6 and 4.1 of 255 -- with their
    // whole ripple pattern in the alpha channel, while `lava` and `slime` are
    // ordinary opaque colour textures. `LiquidType.material_id` says which: 1
    // is the alpha-keyed pair, 2 the colour pair. Running the colour rule over
    // both is a multiply by nearly zero, which is a black river that is
    // otherwise perfectly placed, perfectly animated and perfectly lit.
    var colour: vec3<f32>;
    if (in.alpha_keyed > 0.5) {
        colour = in.tint.rgb * mix(RIPPLE_FLOOR, 1.0, texel.a);
    } else {
        colour = texel.rgb * in.tint.rgb;
    }

    // **Lava is emissive and water is not.** A liquid surface is flat, so its
    // normal is the world's up axis and the diffuse term is one number for the
    // whole sheet -- but applying it to lava would put a lava lake out at
    // night, which is the one thing everybody knows lava does not do.
    var lit: vec3<f32>;
    if (params.sun.w <= 0.0) {
        lit = colour * (0.55 + 0.45 * max(params.light.z, 0.0));
    } else {
        let ndl = max(params.light.z, 0.0);
        lit = colour * (params.ambient.rgb + params.sun.rgb * ndl * params.sun.w);
    }
    // Mixed rather than branched: a hard switch makes the two categories two
    // different materials, and `emissive` is authored as a fraction so a
    // future half-glowing liquid needs no new code path.
    lit = mix(lit, colour, in.emissive);

    // The same fog as the terrain it meets. Water fading at a different
    // distance from the shore it laps against is a seam a player sees at once.
    if (params.fog_range.y > 0.0) {
        let distance = length(in.world - params.eye.xyz);
        let t = clamp(
            (distance - params.fog_range.x) / max(params.fog_range.y - params.fog_range.x, 1.0),
            0.0,
            1.0
        );
        lit = mix(lit, params.fog.rgb, t);
    }
    return vec4<f32>(lit, in.tint.a);
}
"#;

/// Pipeline and per-frame uniform for liquid surfaces.
pub struct LiquidRenderer {
    pipeline: wgpu::RenderPipeline,
    params_layout: wgpu::BindGroupLayout,
    params_bind: wgpu::BindGroup,
    params: wgpu::Buffer,
    surface_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl LiquidRenderer {
    pub fn new(gpu: &Gpu, target: wgpu::TextureFormat) -> Self {
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("liquid"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });

        let params_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("liquid params"),
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

        let surface_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("liquid surface"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });

        let layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("liquid"),
                bind_group_layouts: &[Some(&params_layout), Some(&surface_layout)],
                immediate_size: 0,
            });

        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("liquid"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<LiquidVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![
                            0 => Float32x3,
                            1 => Float32x4,
                            2 => Float32x4,
                            3 => Float32x2
                        ],
                    })],
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
                    // **No culling.** A liquid sheet is a single-sided surface
                    // with no inside, and a swimming character sees it from
                    // below -- culling the back face would make the underside
                    // of every river vanish exactly when a player is under it,
                    // which is the one moment they are looking.
                    cull_mode: None,
                    ..Default::default()
                },
                // Tests depth so a river is hidden by the hill in front of it;
                // does not write it, so the terrain visible *through* the water
                // is not occluded by the water's own depth. Writing would make
                // a riverbed disappear under an opaque-looking sheet whose
                // colour is nonetheless transparent.
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
            label: Some("liquid params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let params_bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("liquid params"),
            layout: &params_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: params.as_entire_binding(),
            }],
        });

        Self {
            pipeline,
            params_layout,
            params_bind,
            params,
            surface_layout,
            sampler: gpu.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("liquid sampler"),
                // Repeating, because the surface art tiles across a sheet far
                // larger than one copy of it -- and because the scroll walks
                // the coordinates off the end of the texture every few seconds.
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::Repeat,
                address_mode_w: wgpu::AddressMode::Repeat,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            }),
        }
    }

    pub fn pipeline(&self) -> &wgpu::RenderPipeline {
        &self.pipeline
    }

    /// Bind group holding one animation frame of one liquid's surface art.
    pub fn bind_surface(&self, gpu: &Gpu, view: &wgpu::TextureView) -> wgpu::BindGroup {
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("liquid surface"),
            layout: &self.surface_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// Uploads the state every sheet shares this frame and binds it.
    ///
    /// Takes the lighting terms rather than computing any, so liquid is lit by
    /// the values the terrain around it was lit by. Two derivations of the same
    /// sun agree only until one of them is changed -- the rule that unprojects
    /// the picking ray from the matrix the scene was drawn with.
    #[allow(clippy::too_many_arguments)]
    pub fn begin<'a>(
        &'a self,
        gpu: &Gpu,
        pass: &mut wgpu::RenderPass<'a>,
        view_proj: glam::Mat4,
        eye: glam::Vec3,
        light: glam::Vec3,
        sun: [f32; 3],
        sun_strength: f32,
        ambient: [f32; 3],
        fog: [f32; 3],
        fog_range: (f32, f32),
        seconds: f32,
    ) {
        let params = Params {
            view_proj: view_proj.to_cols_array_2d(),
            eye: [eye.x, eye.y, eye.z, 1.0],
            light: [light.x, light.y, light.z, 0.0],
            sun: [sun[0], sun[1], sun[2], sun_strength],
            ambient: [ambient[0], ambient[1], ambient[2], 1.0],
            fog: [fog[0], fog[1], fog[2], 1.0],
            fog_range: [fog_range.0, fog_range.1, seconds, 0.0],
        };
        gpu.queue
            .write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.params_bind, &[]);
    }

    /// Exposed so a caller can build its own bind groups against the same
    /// layout without reaching into the renderer.
    pub fn params_layout(&self) -> &wgpu::BindGroupLayout {
        &self.params_layout
    }
}

/// How a liquid category is drawn.
///
/// **Every number here is chosen by looking**, exactly like
/// `precipitation::Shape` -- `LiquidType.dbc` describes darkening and particle
/// scales but says nothing about the tint a surface should be given, and the
/// art is authored to be composited by a renderer this project has not
/// reproduced. They live together so that looking is the only thing needed to
/// change them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// Multiplied into the sampled texel.
    pub tint: [f32; 3],
    /// Alpha where the liquid is at its deepest. The shallows fade towards
    /// nothing from here using the per-vertex depth.
    pub alpha: f32,
    /// Texture coordinates per second the surface scrolls by.
    pub scroll: f32,
    /// 0 lights the surface with the world's sun, 1 leaves it glowing.
    pub emissive: f32,
    /// Whether this liquid's art stores its pattern in the **alpha** channel
    /// with a black RGB, rather than being an ordinary colour texture.
    ///
    /// **Measured, and it tracks `LiquidType.material_id` exactly.** Exported
    /// and averaged over their whole 256x256:
    ///
    /// | texture | material | mean RGB | alpha |
    /// |---|---|---|---|
    /// | `river\lake_a` | 1 | 3.6 | 54, ranging 17-255 |
    /// | `ocean\ocean_h` | 1 | 4.1 | 56, ranging 0-255 |
    /// | `lava\lava` | 2 | 176, 23, 0 | 255 flat |
    /// | `slime\slime` | 2 | 69, 133, 19 | 255 flat |
    ///
    /// So material 1 supplies *ripples* and the client supplies the colour,
    /// while material 2 supplies both and needs no tint at all. Treating them
    /// alike multiplies a tint by ~0.014 and produces a black river -- which
    /// is placed correctly, animated correctly and lit correctly, and so looks
    /// like anything but a texture-channel mistake.
    pub alpha_keyed: bool,
}

impl Look {
    /// Rivers, lakes and pools.
    pub const WATER: Self = Self {
        tint: [0.62, 0.78, 0.92],
        alpha: 0.72,
        scroll: 0.035,
        emissive: 0.0,
        alpha_keyed: true,
    };
    /// Sea water: deeper, greener and slower.
    pub const OCEAN: Self = Self {
        tint: [0.42, 0.62, 0.76],
        alpha: 0.86,
        scroll: 0.018,
        emissive: 0.0,
        alpha_keyed: true,
    };
    /// Lava. Opaque, and it does not go out at night.
    pub const MAGMA: Self = Self {
        tint: [1.0, 1.0, 1.0],
        alpha: 1.0,
        scroll: 0.012,
        emissive: 1.0,
        alpha_keyed: false,
    };
    /// Slime. Opaque like lava and lit like water -- it is not on fire.
    pub const SLIME: Self = Self {
        tint: [0.85, 1.0, 0.7],
        alpha: 1.0,
        scroll: 0.02,
        emissive: 0.35,
        alpha_keyed: false,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vertex must match what the pipeline declares, or the GPU reads the
    /// tint out of the middle of the next vertex -- which produces plausible
    /// coloured water rather than an error.
    #[test]
    fn the_vertex_is_the_size_its_attributes_claim() {
        assert_eq!(std::mem::size_of::<LiquidVertex>(), (3 + 4 + 4 + 2) * 4);
        // Offsets the `vertex_attr_array` above computes implicitly.
        assert_eq!(std::mem::offset_of!(LiquidVertex, uv_motion), 12);
        assert_eq!(std::mem::offset_of!(LiquidVertex, tint), 28);
        assert_eq!(std::mem::offset_of!(LiquidVertex, mode), 44);
    }

    /// Uniform buffers are laid out in 16-byte rows, and a `Params` that is not
    /// a multiple of that is silently padded differently by different backends.
    #[test]
    fn the_uniform_block_is_a_whole_number_of_rows() {
        assert_eq!(std::mem::size_of::<Params>() % 16, 0);
        assert_eq!(std::mem::size_of::<Params>(), 64 + 6 * 16);
    }

    /// Lava glows and water does not, which is the one distinction the shader
    /// branches on. Asserted here because it is a *property* rather than a
    /// taste: every other number in `Look` can be tuned by looking, and this
    /// one cannot be got wrong without a lava lake going dark at midnight.
    #[test]
    fn only_the_burning_liquids_are_emissive() {
        assert_eq!(Look::WATER.emissive, 0.0);
        assert_eq!(Look::OCEAN.emissive, 0.0);
        assert!(Look::MAGMA.emissive > 0.9, "lava must not be lit by the sun");
        assert!(Look::SLIME.emissive > 0.0, "slime glows a little");
        // And the opaque ones are opaque: a riverbed showing through lava
        // would read as the depth test being wrong.
        assert_eq!(Look::MAGMA.alpha, 1.0);
        assert!(Look::WATER.alpha < 1.0, "you can see the riverbed");

        // **Which channel the art lives in, asserted both ways.** Water and
        // ocean ship a black RGB with the ripples in alpha; lava and slime
        // ship ordinary colour. Asserting only that water is alpha-keyed would
        // pass just as well if everything were, which is the reading that
        // turns a lava lake into a flat orange tint with no texture in it.
        assert!(Look::WATER.alpha_keyed && Look::OCEAN.alpha_keyed);
        assert!(!Look::MAGMA.alpha_keyed && !Look::SLIME.alpha_keyed);
    }
}
