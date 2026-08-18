//! Drawing what an M2 emitter produces: camera-facing sprites, and the strips
//! a ribbon leaves behind.
//!
//! Deliberately knows nothing about M2. It is handed a list of sprites -- a
//! position, a size, a colour and a rectangle of a texture -- and a list of
//! ribbon vertices, exactly as [`crate::precipitation`] is handed a shape and
//! an intensity rather than a `SMSG_WEATHER`. The emitter records, their
//! tracks and the simulation live in the `m2` crate, which is where they can
//! be tested without a GPU.
//!
//! # This is the opposite trade from precipitation
//!
//! Rain has no CPU-side list at all: a drop's position is a pure function of
//! its index, so the draw is `draw(0..6, 0..count)` and nothing is uploaded.
//! A particle cannot be, and the reason is worth stating because it looks like
//! a missed simplification. A drop is indistinguishable from every other drop
//! and immortal; a particle is born at wherever its emitter happened to be,
//! carries a lifespan from a track that is itself animated, and dies. Its
//! position is a function of the *history* of a bone, which no shader has.
//!
//! So the instance buffer is uploaded per frame. What is kept from the rain is
//! the rest: six vertices expanded in the shader, depth tested and not
//! written, and no sorting.

use bytemuck::{Pod, Zeroable};

use crate::{mesh::DEPTH_FORMAT, Gpu};

/// One camera-facing sprite.
///
/// Mirrors `m2::particles::Sprite`, which this crate cannot see. Padded to
/// four `vec4`s because a storage-class mismatch between Rust and WGSL is a
/// silent misread rather than an error.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct SpriteInstance {
    /// World position of the centre, with the sprite's rotation in `w`.
    pub position: [f32; 4],
    /// Half-width and half-height in world units, then two spare.
    pub size: [f32; 4],
    /// Colour with alpha. Expected already in the target's own space -- pass
    /// it through [`ParticleRenderer::encode`].
    pub color: [f32; 4],
    /// The cell of the texture to show, as `u0, v0, u1, v1`.
    pub uv: [f32; 4],
}

/// One corner of a ribbon strip.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct RibbonVertex {
    pub position: [f32; 3],
    pub _pad: f32,
    pub uv: [f32; 2],
    pub _pad2: [f32; 2],
    pub color: [f32; 4],
}

/// How a sprite's colour reaches the target.
///
/// The same vocabulary as [`crate::mesh::BlendMode`] but only the two members
/// emitters actually use: the archive survey counts 22,347 additive and 3,901
/// alpha out of 26,374, with 126 in the two modulate modes that are folded in
/// here as alpha and are still wrong. Stated rather than hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Blend {
    /// Standard source-alpha. Smoke, dust.
    Alpha,
    /// Additive, and the majority: fire, sparks, glows.
    Additive,
}

impl Blend {
    /// Maps an M2 blend value. 3 and 4 are the additive pair; everything else
    /// reads acceptably as alpha until the modulate modes are modelled.
    pub fn from_m2(blend: u8) -> Self {
        match blend {
            3 | 4 => Self::Additive,
            _ => Self::Alpha,
        }
    }

    fn state(self) -> wgpu::BlendState {
        match self {
            Self::Alpha => wgpu::BlendState::ALPHA_BLENDING,
            // Source scaled by its own alpha and *added*: an emitter fades a
            // particle out through its alpha track, and with `One` as the
            // source factor a dying spark would stay at full brightness until
            // it vanished.
            Self::Additive => wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::SrcAlpha,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::Zero,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
            },
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    view_proj: [[f32; 4]; 4],
    /// The camera's right axis in world space, which is what a billboard is
    /// widened along.
    right: [f32; 4],
    /// The camera's up axis.
    up: [f32; 4],
}

const SHADER: &str = r#"
struct Params {
    view_proj: mat4x4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(1) @binding(0) var sheet: texture_2d<f32>;
@group(1) @binding(1) var sheet_sampler: sampler;

struct Sprite {
    @location(0) position: vec4<f32>,
    @location(1) size: vec4<f32>,
    @location(2) color: vec4<f32>,
    @location(3) uv: vec4<f32>,
};

struct Out {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

// Two triangles, as corner offsets in -1..1. The same six the rain uses.
const CORNERS = array<vec2<f32>, 6>(
    vec2<f32>(-1.0, -1.0), vec2<f32>( 1.0, -1.0), vec2<f32>( 1.0,  1.0),
    vec2<f32>(-1.0, -1.0), vec2<f32>( 1.0,  1.0), vec2<f32>(-1.0,  1.0),
);

@vertex
fn vs_sprite(@builtin(vertex_index) vertex: u32, sprite: Sprite) -> Out {
    let corner = CORNERS[vertex];

    // Spin about the view axis, so a rotating spark turns in the plane the
    // viewer sees rather than about some world axis they cannot infer.
    let angle = sprite.position.w;
    let c = cos(angle);
    let s = sin(angle);
    let turned = vec2<f32>(corner.x * c - corner.y * s, corner.x * s + corner.y * c);

    let offset = params.right.xyz * (turned.x * sprite.size.x)
               + params.up.xyz * (turned.y * sprite.size.y);

    var out: Out;
    out.clip = params.view_proj * vec4<f32>(sprite.position.xyz + offset, 1.0);
    // The corner runs -1..1 and the cell is stored as its two corners, so the
    // texture coordinate is the halfway point between them. `v` is flipped
    // because a texture's origin is its top-left and the quad's is its bottom.
    out.uv = vec2<f32>(
        mix(sprite.uv.x, sprite.uv.z, turned.x * 0.5 + 0.5),
        mix(sprite.uv.w, sprite.uv.y, turned.y * 0.5 + 0.5),
    );
    out.color = sprite.color;
    return out;
}

struct RibbonIn {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

@vertex
fn vs_ribbon(vertex: RibbonIn) -> Out {
    var out: Out;
    out.clip = params.view_proj * vec4<f32>(vertex.position, 1.0);
    out.uv = vertex.uv;
    out.color = vertex.color;
    return out;
}

@fragment
fn fs(in: Out) -> @location(0) vec4<f32> {
    let texel = textureSample(sheet, sheet_sampler, in.uv);
    return texel * in.color;
}
"#;

/// Draws the sprites and strips an emitter produces.
pub struct ParticleRenderer {
    shader: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    params_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
    params_bind: wgpu::BindGroup,
    sprite_pipelines: std::collections::BTreeMap<Blend, wgpu::RenderPipeline>,
    ribbon_pipelines: std::collections::BTreeMap<Blend, wgpu::RenderPipeline>,
    target_format: wgpu::TextureFormat,
    srgb_target: bool,
    /// One buffer reused every frame rather than one per emitter. Emitters are
    /// drawn one at a time anyway -- each has its own texture -- and a buffer
    /// per emitter would allocate on every placement that came into view.
    instances: wgpu::Buffer,
    instance_capacity: usize,
    vertices: wgpu::Buffer,
    vertex_capacity: usize,
    /// Sprites and strips drawn since the last [`ParticleRenderer::begin`],
    /// and how many were skipped for having no pipeline or no room. Both
    /// numbers, always: a counter that only speaks on failure cannot tell
    /// "none were wrong" from "there were none".
    pub drawn: u32,
    pub skipped: u32,
}

/// Sprites and vertices grow in these steps, so a scene that gains one
/// emitter does not reallocate.
const INSTANCE_STEP: usize = 1024;

impl ParticleRenderer {
    pub fn new(gpu: &Gpu, target: wgpu::TextureFormat) -> Self {
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("particles"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });

        let params_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("particle params"),
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

        let texture_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("particle sheet"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float {
                                    filterable: true,
                                },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(
                                wgpu::SamplerBindingType::Filtering,
                            ),
                            count: None,
                        },
                    ],
                });

        let layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("particle layout"),
                bind_group_layouts: &[Some(&params_layout), Some(&texture_layout)],
                immediate_size: 0,
            });

        let params = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("particle params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let params_bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("particle params"),
            layout: &params_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: params.as_entire_binding(),
            }],
        });

        let instances = new_instance_buffer(gpu, INSTANCE_STEP);
        let vertices = new_vertex_buffer(gpu, INSTANCE_STEP);

        Self {
            shader,
            layout,
            params_layout,
            texture_layout,
            sampler: crate::texture::default_sampler(gpu),
            params,
            params_bind,
            sprite_pipelines: Default::default(),
            ribbon_pipelines: Default::default(),
            target_format: target,
            srgb_target: target.is_srgb(),
            instances,
            instance_capacity: INSTANCE_STEP,
            vertices,
            vertex_capacity: INSTANCE_STEP,
            drawn: 0,
            skipped: 0,
        }
    }

    /// Converts a colour authored for display into whatever the target wants.
    ///
    /// The same job [`crate::sky::SkyRenderer::encode`] does, and for the same
    /// reason: an sRGB target re-encodes whatever a shader writes, so a tint
    /// handed over as a display value comes out brighter than it was authored.
    /// A flame is one of the few things in a scene bright enough for that to
    /// be invisible, which is exactly why it needs saying.
    pub fn encode(&self, colour: [f32; 4]) -> [f32; 4] {
        if !self.srgb_target {
            return colour;
        }
        let to_linear = |c: f32| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        [
            to_linear(colour[0]),
            to_linear(colour[1]),
            to_linear(colour[2]),
            // Alpha is a coverage, not a colour, and is never gamma encoded.
            colour[3],
        ]
    }

    /// Builds the pipelines a frame will need, and resets the counters.
    ///
    /// Split from [`ParticleRenderer::draw_sprites`] because building a
    /// pipeline needs `&mut self` and a render pass already holds the encoder
    /// -- the same shape as [`crate::mesh::MeshRenderer::prepare`].
    pub fn begin(&mut self, gpu: &Gpu, blends: impl IntoIterator<Item = Blend>) {
        self.drawn = 0;
        self.skipped = 0;
        for blend in blends {
            self.sprite_pipelines.entry(blend).or_insert_with(|| {
                build(
                    gpu,
                    &self.shader,
                    &self.layout,
                    self.target_format,
                    blend,
                    Geometry::Sprite,
                )
            });
            self.ribbon_pipelines.entry(blend).or_insert_with(|| {
                build(
                    gpu,
                    &self.shader,
                    &self.layout,
                    self.target_format,
                    blend,
                    Geometry::Ribbon,
                )
            });
        }
    }

    /// Uploads the camera basis for this frame.
    ///
    /// `view_proj` must be the matrix the rest of the pass is drawn with, and
    /// `right`/`up` the camera's own axes -- taken from the same camera, not
    /// rebuilt from angles. A billboard widened along a basis that disagrees
    /// with the projection is a sprite that leans, which reads as a bad
    /// texture rather than as a stale matrix.
    pub fn set_camera(
        &self,
        gpu: &Gpu,
        view_proj: glam::Mat4,
        right: glam::Vec3,
        up: glam::Vec3,
    ) {
        gpu.queue.write_buffer(
            &self.params,
            0,
            bytemuck::bytes_of(&Params {
                view_proj: view_proj.to_cols_array_2d(),
                right: [right.x, right.y, right.z, 0.0],
                up: [up.x, up.y, up.z, 0.0],
            }),
        );
    }

    /// A bind group for one emitter's texture sheet.
    pub fn sheet_bind_group(&self, gpu: &Gpu, view: &wgpu::TextureView) -> wgpu::BindGroup {
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("particle sheet"),
            layout: &self.texture_layout,
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

    /// Makes room for `count` sprites and `vertices` ribbon corners.
    ///
    /// Called before the pass opens, because growing a buffer needs `&mut
    /// self`. A frame that needs more than was reserved draws what fits and
    /// counts the rest in [`ParticleRenderer::skipped`] rather than growing
    /// mid-pass, which cannot be done at all.
    pub fn reserve(&mut self, gpu: &Gpu, sprites: usize, vertices: usize) {
        if sprites > self.instance_capacity {
            let wanted = sprites.next_multiple_of(INSTANCE_STEP);
            self.instances = new_instance_buffer(gpu, wanted);
            self.instance_capacity = wanted;
        }
        if vertices > self.vertex_capacity {
            let wanted = vertices.next_multiple_of(INSTANCE_STEP);
            self.vertices = new_vertex_buffer(gpu, wanted);
            self.vertex_capacity = wanted;
        }
    }

    /// Records one emitter's sprites.
    ///
    /// `at` is where in the reserved buffer these sprites were written by
    /// [`ParticleRenderer::upload_sprites`]; every emitter's batch shares one
    /// buffer so that a scene with fifty torches does not make fifty of them.
    pub fn draw_sprites(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        sheet: &wgpu::BindGroup,
        blend: Blend,
        range: std::ops::Range<u32>,
    ) -> bool {
        let Some(pipeline) = self.sprite_pipelines.get(&blend) else {
            return false;
        };
        if range.is_empty() {
            return true;
        }
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.params_bind, &[]);
        pass.set_bind_group(1, sheet, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..6, range);
        true
    }

    /// Records one ribbon's strip.
    pub fn draw_ribbon(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        sheet: &wgpu::BindGroup,
        blend: Blend,
        range: std::ops::Range<u32>,
    ) -> bool {
        let Some(pipeline) = self.ribbon_pipelines.get(&blend) else {
            return false;
        };
        if range.is_empty() {
            return true;
        }
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.params_bind, &[]);
        pass.set_bind_group(1, sheet, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(range, 0..1);
        true
    }

    /// Writes the whole frame's sprites in one go. Returns how many fitted.
    pub fn upload_sprites(&mut self, gpu: &Gpu, sprites: &[SpriteInstance]) -> usize {
        let fitting = sprites.len().min(self.instance_capacity);
        self.skipped += (sprites.len() - fitting) as u32;
        self.drawn += fitting as u32;
        if fitting > 0 {
            gpu.queue
                .write_buffer(&self.instances, 0, bytemuck::cast_slice(&sprites[..fitting]));
        }
        fitting
    }

    /// Writes the whole frame's ribbon geometry. Returns how many fitted.
    pub fn upload_ribbons(&mut self, gpu: &Gpu, vertices: &[RibbonVertex]) -> usize {
        let fitting = vertices.len().min(self.vertex_capacity);
        self.skipped += (vertices.len() - fitting) as u32;
        if fitting > 0 {
            gpu.queue
                .write_buffer(&self.vertices, 0, bytemuck::cast_slice(&vertices[..fitting]));
        }
        fitting
    }

    /// Held so a caller can rebuild bind groups against the same layout.
    pub fn params_layout(&self) -> &wgpu::BindGroupLayout {
        &self.params_layout
    }
}

fn new_instance_buffer(gpu: &Gpu, count: usize) -> wgpu::Buffer {
    gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("particle instances"),
        size: (count * std::mem::size_of::<SpriteInstance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn new_vertex_buffer(gpu: &Gpu, count: usize) -> wgpu::Buffer {
    gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ribbon vertices"),
        size: (count * std::mem::size_of::<RibbonVertex>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[derive(Clone, Copy)]
enum Geometry {
    Sprite,
    Ribbon,
}

fn build(
    gpu: &Gpu,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    target: wgpu::TextureFormat,
    blend: Blend,
    geometry: Geometry,
) -> wgpu::RenderPipeline {
    let sprite_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<SpriteInstance>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4
        ],
    };
    let ribbon_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<RibbonVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![
            0 => Float32x3, 1 => Float32x2, 2 => Float32x4
        ],
    };

    let (entry, buffers) = match geometry {
        Geometry::Sprite => ("vs_sprite", [Some(sprite_layout)]),
        Geometry::Ribbon => ("vs_ribbon", [Some(ribbon_layout)]),
    };

    gpu.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("particles"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                buffers: &buffers,
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target,
                    blend: Some(blend.state()),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                // A sprite faces wherever the camera basis put it and a ribbon
                // is a flat strip that is meant to be seen from both sides.
                cull_mode: None,
                ..Default::default()
            },
            // **Tests depth, does not write it.** Testing is what keeps a
            // torch's flame behind the wall it is on the far side of; not
            // writing is what lets hundreds of unsorted sprites blend without
            // each occluding the next.
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
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The survey says 3 and 4 are the additive pair and everything else is
    /// not. Asserting only that 4 is additive would pass just as well if every
    /// value mapped to additive, which is the mistake this project has a rule
    /// about.
    #[test]
    fn additive_is_three_and_four_and_nothing_else() {
        assert_eq!(Blend::from_m2(3), Blend::Additive);
        assert_eq!(Blend::from_m2(4), Blend::Additive);
        for other in [0u8, 1, 2, 5, 6, 7] {
            assert_eq!(Blend::from_m2(other), Blend::Alpha, "blend {other}");
        }
    }

    /// A `SpriteInstance` is four `vec4`s and the shader reads it as four
    /// `vec4`s. A mismatch is a silent misread of every field after the first.
    #[test]
    fn the_instance_layout_matches_what_the_shader_declares() {
        assert_eq!(std::mem::size_of::<SpriteInstance>(), 64);
        assert_eq!(std::mem::align_of::<SpriteInstance>(), 4);
        assert_eq!(std::mem::size_of::<RibbonVertex>(), 48);
    }
}
