//! Terrain rendering: up to four texture layers blended per chunk.
//!
//! Terrain needs its own pipeline because its material is unlike anything else
//! in the scene. Every other surface samples one texture; a terrain chunk
//! samples four and mixes them with a per-chunk alpha map, which cannot be
//! expressed through the single-texture material binding the mesh pipeline
//! uses.

use crate::mesh::{MeshVertex, DEPTH_FORMAT};
use crate::Gpu;

/// Layers a chunk may carry. The first is opaque and the rest blend over it.
pub const MAX_LAYERS: usize = 4;

const SHADER: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    light: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

@group(1) @binding(0) var layer0: texture_2d<f32>;
@group(1) @binding(1) var layer1: texture_2d<f32>;
@group(1) @binding(2) var layer2: texture_2d<f32>;
@group(1) @binding(3) var layer3: texture_2d<f32>;
// Alpha for layers 1 to 3 packed into r, g and b.
@group(1) @binding(4) var blend: texture_2d<f32>;
@group(1) @binding(5) var tile_sampler: sampler;
@group(1) @binding(6) var blend_sampler: sampler;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    // Position within the chunk, 0 to 1 on each axis.
    @location(2) uv: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
};

@vertex
fn vs(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(in.position, 1.0);
    out.normal = in.normal;
    out.uv = in.uv;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    // Tileset art is authored to repeat once per height-sample cell, so it
    // tiles eight times across a chunk. Sampling at the chunk's own 0..1
    // coordinates instead stretches a single copy over the whole thing.
    let tiled = in.uv * 8.0;

    // The blend map is addressed in chunk space and must clamp, or the edge
    // texels wrap and stitch a seam along two sides of every chunk.
    let a = textureSample(blend, blend_sampler, in.uv);

    var color = textureSample(layer0, tile_sampler, tiled).rgb;
    color = mix(color, textureSample(layer1, tile_sampler, tiled).rgb, a.r);
    color = mix(color, textureSample(layer2, tile_sampler, tiled).rgb, a.g);
    color = mix(color, textureSample(layer3, tile_sampler, tiled).rgb, a.b);

    let n = normalize(in.normal);
    let ndl = max(dot(n, normalize(camera.light.xyz)), 0.0);
    return vec4<f32>(color * (0.45 + 0.55 * ndl), 1.0);
}
"#;

/// Pipeline and bindings for terrain.
pub struct TerrainRenderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    tile_sampler: wgpu::Sampler,
    blend_sampler: wgpu::Sampler,
}

impl TerrainRenderer {
    pub fn new(gpu: &Gpu, target: wgpu::TextureFormat, camera_layout: &wgpu::BindGroupLayout) -> Self {
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("terrain"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });

        let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let sampler_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };

        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("terrain chunk"),
                entries: &[
                    texture_entry(0),
                    texture_entry(1),
                    texture_entry(2),
                    texture_entry(3),
                    texture_entry(4),
                    sampler_entry(5),
                    sampler_entry(6),
                ],
            });

        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("terrain"),
                bind_group_layouts: &[Some(camera_layout), Some(&layout)],
                immediate_size: 0,
            });

        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("terrain"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        // Shares the mesh vertex so terrain can be built with
                        // the same code; the skinning fields go unread.
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
                    module: &shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(target.into())],
                }),
                primitive: wgpu::PrimitiveState {
                    front_face: wgpu::FrontFace::Cw,
                    cull_mode: Some(wgpu::Face::Back),
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

        Self {
            pipeline,
            layout,
            tile_sampler: crate::texture::default_sampler(gpu),
            blend_sampler: gpu.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("blend sampler"),
                // Clamped, because the blend map spans exactly one chunk and
                // repeating it stitches a seam along two edges.
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
        }
    }

    pub fn pipeline(&self) -> &wgpu::RenderPipeline {
        &self.pipeline
    }

    /// Binds one chunk's four layers and its blend map.
    ///
    /// `layers` must hold exactly [`MAX_LAYERS`] views; callers pad with a
    /// blank texture, since a bind group cannot leave a slot empty.
    pub fn bind_chunk(
        &self,
        gpu: &Gpu,
        layers: &[&wgpu::TextureView; MAX_LAYERS],
        blend: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terrain chunk"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(layers[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(layers[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(layers[2]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(layers[3]),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(blend),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&self.tile_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::Sampler(&self.blend_sampler),
                },
            ],
        })
    }
}

/// Packs a chunk's per-layer alpha maps into one RGBA texture.
///
/// Layers 1 to 3 go into red, green and blue; layer 0 is the base and needs no
/// coverage of its own. Chunks with fewer layers leave the unused channels at
/// zero, which makes those layers contribute nothing.
pub fn pack_blend_map(alpha_maps: &[Vec<u8>], size: usize) -> Vec<u8> {
    let mut out = vec![0u8; size * size * 4];
    for (channel, map) in alpha_maps.iter().take(MAX_LAYERS - 1).enumerate() {
        for (i, &value) in map.iter().take(size * size).enumerate() {
            out[i * 4 + channel] = value;
        }
    }
    // Alpha is unused by the shader but must be sane for any tooling that
    // inspects the texture.
    for i in 0..size * size {
        out[i * 4 + 3] = 255;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_map_packs_layers_into_channels() {
        let size = 4;
        let maps = vec![
            vec![10u8; size * size],
            vec![20u8; size * size],
            vec![30u8; size * size],
        ];
        let packed = pack_blend_map(&maps, size);
        assert_eq!(packed.len(), size * size * 4);
        assert_eq!(&packed[..4], &[10, 20, 30, 255]);
    }

    /// A chunk with one extra layer leaves the other channels clear, so those
    /// layers blend in at zero strength.
    #[test]
    fn missing_layers_are_transparent() {
        let size = 2;
        let packed = pack_blend_map(&[vec![255u8; size * size]], size);
        assert_eq!(&packed[..4], &[255, 0, 0, 255]);
    }

    /// A base-only chunk shows nothing but layer 0.
    #[test]
    fn a_chunk_without_extra_layers_is_all_base() {
        let packed = pack_blend_map(&[], 2);
        assert!(packed.chunks_exact(4).all(|p| p[..3] == [0, 0, 0]));
    }

    /// Never write past the packed buffer, whatever the source map's length.
    #[test]
    fn oversized_maps_are_truncated() {
        let packed = pack_blend_map(&[vec![7u8; 10_000]], 4);
        assert_eq!(packed.len(), 4 * 4 * 4);
    }
}
