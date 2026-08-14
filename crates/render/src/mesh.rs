//! Drawing M2 geometry.
//!
//! One pipeline per distinct render state, built on demand and cached. M2
//! materials vary along three axes that a pipeline cannot switch at draw time
//! -- blending, face culling, and whether alpha is tested -- so the combination
//! is the cache key.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};

use crate::Gpu;

/// Depth format used everywhere. Reversed-Z is not worth the complication at
/// the scale of a single model.
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Vertex layout handed to the GPU.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct MeshVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    /// Indices into the model's bone list, addressed directly.
    pub bone_indices: [u8; 4],
    /// Influences summing to 255; all zero means the vertex is not skinned.
    pub bone_weights: [u8; 4],
}

/// A per-object transform, supplied as instance data.
///
/// Instance attributes rather than a uniform, so a scene can hold thousands of
/// placements in one buffer and a draw selects its own by index -- no rebinding
/// between objects, and identical models can later share a draw call.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Instance {
    pub model: [[f32; 4]; 4],
}

impl Instance {
    pub fn from_cols_array_2d(model: [[f32; 4]; 4]) -> Self {
        Self { model }
    }

    pub const IDENTITY: Self = Self {
        model: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct CameraUniform {
    pub view_proj: [[f32; 4]; 4],
    pub eye: [f32; 4],
    /// Direction *towards* the light, plus an unused w.
    pub light: [f32; 4],
    /// Colour of the direct light, and in `w` how much of it to apply. Zero
    /// there means "no lighting data": the shaders fall back to the fixed
    /// ambient-plus-headlight they used before this existed, so a scene with no
    /// `Light.dbc` -- an offline model view, a test -- still reads as shape
    /// rather than going black.
    pub sun: [f32; 4],
    /// Ambient colour, plus an unused w.
    pub ambient: [f32; 4],
    /// Fog colour, plus an unused w.
    pub fog: [f32; 4],
    /// `x` where fog begins, `y` where it is total, both in world units. `y`
    /// of zero disables fog.
    pub fog_range: [f32; 4],
}

impl CameraUniform {
    /// The lighting a scene with no light data gets: none, which the shaders
    /// read as "use the placeholder".
    pub const UNLIT: ([f32; 4], [f32; 4], [f32; 4], [f32; 4]) = (
        [0.0; 4],
        [0.0; 4],
        [0.0; 4],
        [0.0; 4],
    );
}

/// How a batch's material maps onto fixed-function state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendMode {
    /// Fully opaque.
    Opaque,
    /// Binary transparency via `discard`; still writes depth, so it can be
    /// drawn with the opaque pass.
    AlphaKey,
    /// Standard source-alpha blending.
    Blend,
    /// Additive, for glows and effects.
    Additive,
}

impl BlendMode {
    /// Maps an M2 material's blend field.
    ///
    /// The values beyond 3 are variations on modulation that all read
    /// acceptably as alpha blending until the shader models them properly.
    pub fn from_m2(blend: u16) -> Self {
        match blend {
            0 => Self::Opaque,
            1 => Self::AlphaKey,
            3 => Self::Additive,
            _ => Self::Blend,
        }
    }

    /// Whether the batch belongs in the transparent pass, which must be drawn
    /// after everything opaque and back to front.
    pub fn is_transparent(self) -> bool {
        matches!(self, Self::Blend | Self::Additive)
    }
}

/// Which face of a triangle is the front.
///
/// M2 and WMO disagree: M2 winds clockwise, WMO counter-clockwise. Using one
/// convention for both culls exactly the surfaces you want to see -- a WMO roof
/// vanishes and you look straight through it at the interior ceiling, which
/// reads as a hole rather than as a culling bug.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Winding {
    Clockwise,
    CounterClockwise,
}

impl Winding {
    fn to_wgpu(self) -> wgpu::FrontFace {
        match self {
            Self::Clockwise => wgpu::FrontFace::Cw,
            Self::CounterClockwise => wgpu::FrontFace::Ccw,
        }
    }
}

/// Everything about a batch that selects a pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RenderState {
    pub blend: BlendMode,
    pub two_sided: bool,
    pub depth_write: bool,
    pub winding: Winding,
}

const SHADER: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    light: vec4<f32>,
    sun: vec4<f32>,
    ambient: vec4<f32>,
    fog: vec4<f32>,
    fog_range: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;
// Storage rather than uniform: bone counts are per-model and reach 315, which
// a fixed-size uniform array would have to over-allocate for.
@group(2) @binding(0) var<storage, read> bones: array<mat4x4<f32>>;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) bone_indices: vec4<u32>,
    @location(4) bone_weights: vec4<f32>,
    // Per-instance model matrix, one column per location.
    @location(5) model_0: vec4<f32>,
    @location(6) model_1: vec4<f32>,
    @location(7) model_2: vec4<f32>,
    @location(8) model_3: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    // Carried so the fragment stage can measure distance for fog. The clip
    // position cannot answer that after the divide.
    @location(2) world: vec3<f32>,
};

@vertex
fn vs(in: VsIn) -> VsOut {
    var out: VsOut;

    let w = in.bone_weights;
    let total = w.x + w.y + w.z + w.w;

    var position = vec4<f32>(in.position, 1.0);
    var normal = in.normal;

    // Unweighted vertices exist -- rigid props parented to a single bone leave
    // the weights blank -- so they pass through untransformed.
    if (total > 0.001) {
        let p = vec4<f32>(in.position, 1.0);
        let n = vec4<f32>(in.normal, 0.0);
        var skinned = vec4<f32>(0.0);
        var skinned_n = vec3<f32>(0.0);

        skinned = skinned + (bones[in.bone_indices.x] * p) * w.x;
        skinned = skinned + (bones[in.bone_indices.y] * p) * w.y;
        skinned = skinned + (bones[in.bone_indices.z] * p) * w.z;
        skinned = skinned + (bones[in.bone_indices.w] * p) * w.w;

        skinned_n = skinned_n + (bones[in.bone_indices.x] * n).xyz * w.x;
        skinned_n = skinned_n + (bones[in.bone_indices.y] * n).xyz * w.y;
        skinned_n = skinned_n + (bones[in.bone_indices.z] * n).xyz * w.z;
        skinned_n = skinned_n + (bones[in.bone_indices.w] * n).xyz * w.w;

        // Weights are quantised to 255ths and do not always sum to exactly 1.
        position = skinned / total;
        normal = skinned_n;
    }

    let model = mat4x4<f32>(in.model_0, in.model_1, in.model_2, in.model_3);
    let world = model * position;
    out.clip = camera.view_proj * world;
    out.world = world.xyz;
    // Placements are rigid with uniform scale, so the model matrix rotates
    // normals correctly without a separate inverse-transpose.
    out.normal = (model * vec4<f32>(normal, 0.0)).xyz;
    out.uv = in.uv;
    return out;
}

// Placeholder lighting: a single directional term plus ambient, enough to read
// shape. Real lighting comes from the light DBCs much later.
// Lighting from the world's own tables when there is any, and the old fixed
// headlight when there is not -- an offline model view has no hour and no
// place, and must still read as shape rather than going black. `sun.w` is the
// switch: zero means no data. Shared verbatim with the terrain shader, because
// terrain and the things standing on it disagreeing about what noon looks like
// is exactly the seam a player would notice first.
fn sky_light(normal: vec3<f32>) -> vec3<f32> {
    let n = normalize(normal);
    if (camera.sun.w <= 0.0) {
        let ndl = max(dot(n, normalize(camera.light.xyz)), 0.0);
        return vec3<f32>(0.38 + 0.62 * ndl);
    }
    let ndl = max(dot(n, normalize(camera.light.xyz)), 0.0);
    return camera.ambient.rgb + camera.sun.rgb * ndl * camera.sun.w;
}

// Distance fog, applied after lighting. `fog_range.y` of zero disables it.
fn fogged(colour: vec3<f32>, world: vec3<f32>) -> vec3<f32> {
    if (camera.fog_range.y <= 0.0) {
        return colour;
    }
    let distance = length(world - camera.eye.xyz);
    let t = clamp((distance - camera.fog_range.x) / max(camera.fog_range.y - camera.fog_range.x, 1.0), 0.0, 1.0);
    return mix(colour, camera.fog.rgb, t);
}

fn shade(normal: vec3<f32>, uv: vec2<f32>, world: vec3<f32>) -> vec4<f32> {
    let texel = textureSample(tex, samp, uv);
    return vec4<f32>(fogged(texel.rgb * sky_light(normal), world), texel.a);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    return shade(in.normal, in.uv, in.world);
}

// Cutout materials: reject rather than blend, so the batch can still write
// depth and be drawn in the opaque pass.
@fragment
fn fs_alpha_key(in: VsOut) -> @location(0) vec4<f32> {
    let c = shade(in.normal, in.uv, in.world);
    if (c.a < 0.5) {
        discard;
    }
    return vec4<f32>(c.rgb, 1.0);
}
"#;

/// Geometry resident on the GPU.
pub struct GpuMesh {
    pub vertices: wgpu::Buffer,
    pub indices: wgpu::Buffer,
    pub index_count: u32,
}

impl GpuMesh {
    /// Uploads geometry.
    ///
    /// Empty input still allocates a one-element buffer: a zero-sized buffer
    /// cannot be sliced, and slicing is unconditional at draw time. Callers
    /// should reject degenerate meshes rather than rely on this, but a panic
    /// deep in the render pass is a poor way to find out.
    pub fn upload(gpu: &Gpu, vertices: &[MeshVertex], indices: &[u32]) -> Self {
        use wgpu::util::DeviceExt;
        let blank_vertex = [MeshVertex {
            position: [0.0; 3],
            normal: [0.0, 0.0, 1.0],
            uv: [0.0; 2],
            bone_indices: [0; 4],
            bone_weights: [0; 4],
        }];
        let vertices = if vertices.is_empty() {
            &blank_vertex[..]
        } else {
            vertices
        };
        let blank_index = [0u32];
        let index_count = indices.len() as u32;
        let indices = if indices.is_empty() {
            &blank_index[..]
        } else {
            indices
        };
        Self {
            vertices: gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("mesh vertices"),
                    contents: bytemuck::cast_slice(vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
            indices: gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("mesh indices"),
                    contents: bytemuck::cast_slice(indices),
                    usage: wgpu::BufferUsages::INDEX,
                }),
            index_count,
        }
    }
}

/// Cached pipelines plus the camera binding they share.
pub struct MeshRenderer {
    shader: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    target_format: wgpu::TextureFormat,
    pipelines: HashMap<RenderState, wgpu::RenderPipeline>,
    pub camera_buffer: wgpu::Buffer,
    camera_layout: wgpu::BindGroupLayout,
    camera_bind: wgpu::BindGroup,
    material_layout: wgpu::BindGroupLayout,
    bone_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

/// Bone matrices for one model, plus the binding that exposes them.
pub struct BoneBuffer {
    pub buffer: wgpu::Buffer,
    pub bind_group: wgpu::BindGroup,
    pub count: usize,
}

impl MeshRenderer {
    pub fn new(gpu: &Gpu, target_format: wgpu::TextureFormat) -> Self {
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("mesh"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });

        let camera_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("camera"),
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

        let material_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("material"),
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

        let camera_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let camera_bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let bone_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("bones"),
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    }],
                });

        let layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mesh"),
                bind_group_layouts: &[
                    Some(&camera_layout),
                    Some(&material_layout),
                    Some(&bone_layout),
                ],
                immediate_size: 0,
            });

        Self {
            shader,
            layout,
            target_format,
            pipelines: HashMap::new(),
            camera_buffer,
            camera_layout,
            camera_bind,
            material_layout,
            bone_layout,
            sampler: crate::texture::default_sampler(gpu),
        }
    }

    /// Allocates a bone palette. Always at least one matrix, because a storage
    /// binding cannot be empty even when a model has no skeleton.
    /// Creates a bone palette, filled with identities rather than left zeroed.
    ///
    /// **The initialisation is the point, not a nicety.** A new wgpu buffer is
    /// zeroed, and a zero matrix multiplies every vertex to the origin -- so a
    /// palette that is created and never posed collapses its whole model to a
    /// point, silently, with nothing anywhere reporting an error. This project
    /// has already lost time to exactly that shape once, when a palette sized
    /// for one matrix made every skinned model invisible and the search went
    /// to the protocol instead of the renderer.
    ///
    /// It cost time again here: `--screenshot` places replicated entities but
    /// never calls `World::update_animations`, which is the only thing that
    /// writes a pose, so *every* creature and player in a headless render
    /// collapsed to the world origin. Nobody noticed because 3.5 was verified
    /// by watching a window, where the frame loop does pose them. Identity
    /// here turns that failure from "nothing is drawn" into "the bind pose is
    /// drawn" -- still wrong, but wrong in a way somebody can see.
    pub fn create_bones(&self, gpu: &Gpu, count: usize) -> BoneBuffer {
        use wgpu::util::DeviceExt;
        let count = count.max(1);
        let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
        let contents: Vec<[[f32; 4]; 4]> = vec![identity; count];
        let buffer = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bones"),
                contents: bytemuck::cast_slice(&contents),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            });
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bones"),
            layout: &self.bone_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        BoneBuffer {
            buffer,
            bind_group,
            count,
        }
    }

    /// Uploads a pose. Extra matrices beyond the buffer's capacity are dropped
    /// rather than overflowing it.
    pub fn update_bones(&self, gpu: &Gpu, bones: &BoneBuffer, pose: &[[[f32; 4]; 4]]) {
        let n = pose.len().min(bones.count);
        if n > 0 {
            gpu.queue
                .write_buffer(&bones.buffer, 0, bytemuck::cast_slice(&pose[..n]));
        }
    }

    pub fn camera_bind_group(&self) -> &wgpu::BindGroup {
        &self.camera_bind
    }

    /// The camera binding's layout, so other pipelines can share group 0 and
    /// the same uniform.
    pub fn camera_layout(&self) -> &wgpu::BindGroupLayout {
        &self.camera_layout
    }

    pub fn update_camera(&self, gpu: &Gpu, camera: &CameraUniform) {
        gpu.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(camera));
    }

    /// Binds one texture for drawing.
    pub fn material_bind_group(&self, gpu: &Gpu, view: &wgpu::TextureView) -> wgpu::BindGroup {
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("material"),
            layout: &self.material_layout,
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

    /// Returns the pipeline for a state, building it the first time it is
    /// asked for.
    pub fn pipeline(&mut self, gpu: &Gpu, state: RenderState) -> &wgpu::RenderPipeline {
        self.pipelines
            .entry(state)
            .or_insert_with(|| build_pipeline(gpu, &self.shader, &self.layout, self.target_format, state))
    }

    /// Builds every pipeline a draw list will need.
    ///
    /// Called before recording, because [`MeshRenderer::pipeline`] needs `&mut
    /// self` and a render pass already holds the encoder. Pre-warming lets the
    /// pass look pipelines up immutably.
    pub fn prepare(&mut self, gpu: &Gpu, states: impl IntoIterator<Item = RenderState>) {
        for state in states {
            self.pipeline(gpu, state);
        }
    }

    /// Looks up a pipeline built by [`MeshRenderer::prepare`].
    pub fn get(&self, state: RenderState) -> Option<&wgpu::RenderPipeline> {
        self.pipelines.get(&state)
    }

    pub fn pipeline_count(&self) -> usize {
        self.pipelines.len()
    }
}

fn build_pipeline(
    gpu: &Gpu,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
    state: RenderState,
) -> wgpu::RenderPipeline {
    let blend = match state.blend {
        BlendMode::Opaque | BlendMode::AlphaKey => None,
        BlendMode::Blend => Some(wgpu::BlendState::ALPHA_BLENDING),
        BlendMode::Additive => Some(wgpu::BlendState {
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
        }),
    };

    let entry = if state.blend == BlendMode::AlphaKey {
        "fs_alpha_key"
    } else {
        "fs"
    };

    gpu.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mesh"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[
                    Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<MeshVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![
                            0 => Float32x3,
                            1 => Float32x3,
                            2 => Float32x2,
                            3 => Uint8x4,
                            // Unorm so 255 arrives as 1.0 without a shader divide.
                            4 => Unorm8x4
                        ],
                    }),
                    Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Instance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![
                            5 => Float32x4,
                            6 => Float32x4,
                            7 => Float32x4,
                            8 => Float32x4
                        ],
                    }),
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                front_face: state.winding.to_wgpu(),
                cull_mode: if state.two_sided {
                    None
                } else {
                    Some(wgpu::Face::Back)
                },
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(state.depth_write),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
}

/// Per-object transforms for a whole scene.
pub struct InstanceBuffer {
    pub buffer: wgpu::Buffer,
    pub len: usize,
}

impl InstanceBuffer {
    pub fn upload(gpu: &Gpu, instances: &[Instance]) -> Self {
        use wgpu::util::DeviceExt;
        // A vertex buffer cannot be empty, so an empty scene still gets one
        // identity entry.
        let fallback = [Instance::IDENTITY];
        let data = if instances.is_empty() {
            &fallback[..]
        } else {
            instances
        };
        Self {
            buffer: gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("instances"),
                    contents: bytemuck::cast_slice(data),
                    // Writable so a buffer whose transforms are recomputed every
                    // frame -- an item held in an animated hand -- can be
                    // rewritten rather than reallocated per frame.
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                }),
            len: data.len(),
        }
    }

    /// Rewrites the transforms in place.
    ///
    /// Silently writes only what fits: the buffer's length was fixed when it
    /// was created, and a caller with more instances than that needs a new
    /// buffer, not a partial overwrite of somebody else's memory.
    pub fn write(&self, gpu: &Gpu, instances: &[Instance]) {
        let n = instances.len().min(self.len);
        if n > 0 {
            gpu.queue
                .write_buffer(&self.buffer, 0, bytemuck::cast_slice(&instances[..n]));
        }
    }
}

/// A depth attachment matching the colour target's size.
pub struct DepthBuffer {
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

impl DepthBuffer {
    pub fn new(gpu: &Gpu, width: u32, height: u32) -> Self {
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        Self {
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            width,
            height,
        }
    }

    /// Recreates the buffer if the target has changed size.
    pub fn resize(&mut self, gpu: &Gpu, width: u32, height: u32) {
        if self.width != width || self.height != height {
            *self = Self::new(gpu, width, height);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winding_maps_to_front_face() {
        assert_eq!(Winding::Clockwise.to_wgpu(), wgpu::FrontFace::Cw);
        assert_eq!(
            Winding::CounterClockwise.to_wgpu(),
            wgpu::FrontFace::Ccw
        );
    }

    #[test]
    fn maps_m2_blend_values() {
        assert_eq!(BlendMode::from_m2(0), BlendMode::Opaque);
        assert_eq!(BlendMode::from_m2(1), BlendMode::AlphaKey);
        assert_eq!(BlendMode::from_m2(2), BlendMode::Blend);
        assert_eq!(BlendMode::from_m2(3), BlendMode::Additive);
    }

    /// Alpha-keyed geometry is opaque as far as sorting is concerned: it
    /// discards rather than blends, so it can write depth in the first pass.
    #[test]
    fn alpha_key_is_not_transparent() {
        assert!(!BlendMode::AlphaKey.is_transparent());
        assert!(!BlendMode::Opaque.is_transparent());
        assert!(BlendMode::Blend.is_transparent());
        assert!(BlendMode::Additive.is_transparent());
    }
}
