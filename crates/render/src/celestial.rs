//! The things drawn *on* the sky: the star dome, a cloud band, and whatever
//! model `LightSkybox` names for the place the camera is standing.
//!
//! Its own pipeline rather than the mesh renderer's, and the reason is that
//! every rule the mesh pipeline enforces is wrong here. A sky model is
//! **unlit** -- `Stars.m2`'s seven materials all set the M2 unlit flag -- so
//! running it through `sky_light` would dim the stars at night, which is the
//! one hour they exist for. It is **not fogged**, because fog is what distance
//! does to something in the world and the sky is not in the world. And it
//! neither writes depth nor tests it, because it is drawn immediately after
//! the gradient and everything solid is meant to cover it.
//!
//! What it shares with the mesh renderer is the vertex layout, deliberately:
//! sky models are M2s and arrive through the same loader, so a second vertex
//! type would mean a second conversion to keep in step.
//!
//! **Everything here is drawn centred on the camera.** The model matrix
//! translates to the eye, so the dome moves with the player and never gets
//! closer -- which is what makes a hemisphere twenty-five units across read as
//! a sky rather than as a tent.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};

use crate::mesh::{BlendMode, GpuMesh, MeshVertex, Winding, DEPTH_FORMAT};
use crate::Gpu;

// **The pipeline key is the blend mode and nothing else**, where
// [`crate::mesh::RenderState`] needs four axes. Nothing on the sky writes
// depth, so that is not a choice; and nothing on the sky is culled, so the
// winding is not one either -- see `cull_mode` below for why that is a
// decision rather than an omission.

/// Where a sky object sits and what colour it comes out.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    /// `rgb` multiplies the texture, `a` is how much of it to draw at all.
    tint: [f32; 4],
    /// Added to every texture coordinate, so a cloud band can drift without
    /// its geometry moving. `zw` unused.
    uv_offset: [f32; 4],
}

const SHADER: &str = r#"
struct Params {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    tint: vec4<f32>,
    uv_offset: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = params.view_proj * params.model * vec4<f32>(in.position, 1.0);
    out.uv = in.uv + params.uv_offset.xy;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let texel = textureSample(tex, samp, in.uv);
    // **The tint's alpha multiplies the texture's, and that is the fade.**
    // `Stars.blp` is pure white with the whole starfield in its alpha channel
    // -- 1.88% of its texels are lit -- so scaling alpha is what makes stars
    // come out and go in, where scaling `rgb` would only make white stars
    // grey ones.
    return vec4<f32>(texel.rgb * params.tint.rgb, texel.a * params.tint.a);
}
"#;

/// One object's placement and tint, with the binding that exposes it.
///
/// **One buffer per object rather than one shared buffer, and that is not
/// tidiness.** `queue.write_buffer` is ordered before every command in the
/// submission it precedes, so two writes to one buffer between two draws in
/// the same pass give *both* draws the second value. The star dome would
/// silently take the cloud band's tint. Owning the buffer makes that
/// unrepresentable rather than merely avoided.
pub struct Placement {
    params: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

impl Placement {
    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind
    }
}

/// Pipelines and bindings for everything drawn on the sky.
pub struct CelestialRenderer {
    shader: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    target_format: wgpu::TextureFormat,
    pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    params_layout: wgpu::BindGroupLayout,
    material_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl CelestialRenderer {
    pub fn new(gpu: &Gpu, target_format: wgpu::TextureFormat) -> Self {
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("celestial"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });

        let params_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("celestial params"),
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
                    label: Some("celestial material"),
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
                label: Some("celestial"),
                bind_group_layouts: &[Some(&params_layout), Some(&material_layout)],
                immediate_size: 0,
            });

        Self {
            shader,
            layout,
            target_format,
            pipelines: HashMap::new(),
            params_layout,
            material_layout,
            sampler: crate::texture::default_sampler(gpu),
        }
    }

    /// Allocates a placement, zeroed.
    ///
    /// Zero is safe here where it is not for a bone palette: a zero tint draws
    /// nothing at all, and a caller that forgets to write one gets an empty
    /// sky rather than a dome collapsed to a point.
    pub fn placement(&self, gpu: &Gpu) -> Placement {
        let params = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("celestial params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("celestial params"),
            layout: &self.params_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: params.as_entire_binding(),
            }],
        });
        Placement { params, bind }
    }

    /// Places and tints one object.
    ///
    /// `view_proj` must be the matrix the rest of the frame is drawn with --
    /// the same rule the gradient and the picking ray follow, and for the same
    /// reason: a sky built from a second derivation of the camera meets the
    /// ground only until somebody edits one of them.
    pub fn set(
        &self,
        gpu: &Gpu,
        placement: &Placement,
        view_proj: glam::Mat4,
        model: glam::Mat4,
        tint: [f32; 4],
        uv_offset: [f32; 2],
    ) {
        gpu.queue.write_buffer(
            &placement.params,
            0,
            bytemuck::bytes_of(&Params {
                view_proj: view_proj.to_cols_array_2d(),
                model: model.to_cols_array_2d(),
                tint,
                uv_offset: [uv_offset[0], uv_offset[1], 0.0, 0.0],
            }),
        );
    }

    /// Binds one texture for drawing.
    pub fn material_bind_group(&self, gpu: &Gpu, view: &wgpu::TextureView) -> wgpu::BindGroup {
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("celestial material"),
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

    /// Builds every pipeline a draw list will need, before the pass takes the
    /// encoder. Same arrangement as [`crate::MeshRenderer::prepare`].
    pub fn prepare(&mut self, gpu: &Gpu, blends: impl IntoIterator<Item = BlendMode>) {
        for blend in blends {
            if !self.pipelines.contains_key(&blend) {
                let pipeline =
                    build_pipeline(gpu, &self.shader, &self.layout, self.target_format, blend);
                self.pipelines.insert(blend, pipeline);
            }
        }
    }

    pub fn get(&self, blend: BlendMode) -> Option<&wgpu::RenderPipeline> {
        self.pipelines.get(&blend)
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
    mode: BlendMode,
) -> wgpu::RenderPipeline {
    let blend = match mode {
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

    gpu.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("celestial"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<MeshVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3,
                        1 => Float32x3,
                        2 => Float32x2
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                // Inert, because nothing here is culled -- stated rather than
                // defaulted so a later reader can see that the choice was
                // considered and does not matter.
                front_face: Winding::CounterClockwise.to_wgpu(),
                // **Never culled, and this is a decision rather than an
                // oversight.** Every one of these is a hull seen from the
                // inside, which is exactly the arrangement that has already
                // cost this project two bugs -- a WMO roof that vanished and
                // an M2 that read as facing away. A culled sky is not a subtly
                // wrong sky; it is no sky, and "the feature is missing" and
                // "the winding is backwards" produce the same black night.
                cull_mode: None,
                ..Default::default()
            },
            // Shares the world's depth attachment so the sky can be drawn into
            // the same pass, and neither writes nor tests: it goes down first
            // and everything solid covers it.
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
        })
}

/// Vertices and indices for a band of sky wrapped around the camera.
///
/// **The shape comes from the texture, not from taste.**
/// `Environments\Stars\StarsAndClouds.blp` is 512x256 with its alpha fading to
/// *exactly zero* along the top edge and tapering along the bottom, and its
/// first and last columns agree to a mean of 2.7 of 255 where an unrelated
/// column differs by 75. That is a panorama authored to be wrapped around the
/// horizon and faded out at both edges -- so it is drawn as a band on the
/// dome, not as a sheet overhead, and `repeat` copies of it go round.
///
/// `low` and `high` are elevations in radians. Positions are on the unit
/// sphere, so a caller scales to whatever radius it wants the sky at; the
/// curvature is what stops the band reading as a wall when the player looks up
/// at it.
///
/// The texture's top edge is the transparent one, so it is mapped to `high`:
/// the clouds thicken downwards, which is the way the asset was painted.
pub fn cloud_band(segments: usize, low: f32, high: f32, repeat: f32) -> (Vec<MeshVertex>, Vec<u32>) {
    let segments = segments.max(3);
    let mut vertices = Vec::with_capacity((segments + 1) * 2);
    let mut indices = Vec::with_capacity(segments * 6);
    for i in 0..=segments {
        // The seam vertex is emitted twice -- once at u=0 and once at
        // u=repeat -- because a shared vertex cannot hold two texture
        // coordinates. Wrapping the index instead would run the whole texture
        // backwards across the last segment.
        let a = i as f32 / segments as f32 * std::f32::consts::TAU;
        let (sin_a, cos_a) = a.sin_cos();
        let u = i as f32 / segments as f32 * repeat;
        for (row, elevation) in [(0.0f32, high), (1.0, low)] {
            let (sin_e, cos_e) = elevation.sin_cos();
            vertices.push(MeshVertex {
                position: [cos_a * cos_e, sin_a * cos_e, sin_e],
                // Pointing at the camera at the centre. Unread by the sky
                // shader, which has no lighting -- present because the vertex
                // layout is shared with everything else that is drawn.
                normal: [-cos_a * cos_e, -sin_a * cos_e, -sin_e],
                uv: [u, row],
                bone_indices: [0; 4],
                bone_weights: [0; 4],
            });
        }
    }
    for i in 0..segments as u32 {
        let (a, b) = (i * 2, i * 2 + 2);
        indices.extend_from_slice(&[a, a + 1, b + 1, a, b + 1, b]);
    }
    (vertices, indices)
}

/// Uploads a [`cloud_band`].
pub fn upload_cloud_band(
    gpu: &Gpu,
    segments: usize,
    low: f32,
    high: f32,
    repeat: f32,
) -> GpuMesh {
    let (vertices, indices) = cloud_band(segments, low, high, repeat);
    GpuMesh::upload(gpu, &vertices, &indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cloud_band_lies_on_the_unit_sphere_between_its_two_elevations() {
        let (low, high) = (0.05f32, 0.5);
        let (vertices, indices) = cloud_band(32, low, high, 4.0);
        assert_eq!(indices.len(), 32 * 6);
        for v in &vertices {
            let p = glam::Vec3::from(v.position);
            // On the sphere, or the band is a cone and the horizon it is meant
            // to hug drifts with the camera's pitch.
            assert!(
                (p.length() - 1.0).abs() < 1e-5,
                "off the unit sphere: {p:?} at {}",
                p.length()
            );
            let elevation = p.z.asin();
            assert!(
                elevation >= low - 1e-4 && elevation <= high + 1e-4,
                "outside the band: {elevation}"
            );
        }
    }

    #[test]
    fn the_transparent_edge_of_the_texture_goes_at_the_top() {
        // The measurement this encodes: `StarsAndClouds.blp`'s alpha is zero
        // along its top rows and heaviest low down. Mapping v=0 to the *low*
        // elevation would put the empty half of the texture on the horizon and
        // the cloud base overhead -- which renders perfectly and is upside
        // down, the same shape of mistake as a sky gradient read from the
        // wrong end.
        let (vertices, _) = cloud_band(8, 0.05, 0.5, 1.0);
        let top = vertices.iter().find(|v| v.uv[1] == 0.0).unwrap();
        let bottom = vertices.iter().find(|v| v.uv[1] == 1.0).unwrap();
        assert!(
            top.position[2] > bottom.position[2],
            "v=0 must be the higher ring: {} against {}",
            top.position[2],
            bottom.position[2]
        );
    }

    #[test]
    fn the_seam_is_a_duplicated_vertex_rather_than_a_wrapped_index() {
        // The first and last columns of the band must sit at the same place
        // and carry different texture coordinates. Sharing them instead would
        // draw the whole texture backwards across one segment -- a single
        // mirrored wedge in an otherwise correct sky, which reads as a bad
        // texture rather than as a topology mistake.
        let repeat = 4.0;
        let (vertices, _) = cloud_band(16, 0.05, 0.5, repeat);
        let first = vertices.first().unwrap();
        let last = vertices[vertices.len() - 2];
        for axis in 0..3 {
            assert!(
                (first.position[axis] - last.position[axis]).abs() < 1e-5,
                "the seam must close in space"
            );
        }
        assert_eq!(first.uv[0], 0.0);
        assert_eq!(last.uv[0], repeat);
    }
}
