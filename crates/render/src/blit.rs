//! Draws a texture to a render target, letterboxed and composited over a
//! checkerboard.
//!
//! This is the viewer's whole render path for now, and it doubles as the first
//! real exercise of the upload path: if a DXT texture reaches the screen
//! looking correct, the compressed upload is working end to end.

use bytemuck::{Pod, Zeroable};

use crate::Gpu;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    uv_scale: [f32; 2],
    checker: f32,
    _pad: f32,
}

const SHADER: &str = r#"
struct Params {
    uv_scale: vec2<f32>,
    checker: f32,
    _pad: f32,
};

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> params: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One oversized triangle covers the viewport without a vertex buffer.
@vertex
fn vs(@builtin(vertex_index) idx: u32) -> VsOut {
    var out: VsOut;
    let corner = vec2<f32>(f32((idx << 1u) & 2u), f32(idx & 2u));
    out.pos = vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
    // Clip space is y-up, texture space is y-down.
    out.uv = vec2<f32>(corner.x, 1.0 - corner.y);
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    // Checkerboard, so transparent regions read as transparent rather than
    // blending invisibly into a flat clear colour.
    let cell = floor(in.pos.xy / params.checker);
    let light = ((cell.x + cell.y) % 2.0) == 0.0;
    let bg = vec3<f32>(select(0.17, 0.25, light));

    let uv = (in.uv - 0.5) * params.uv_scale + 0.5;
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        // Outside the letterboxed image: darken so the extent is obvious.
        return vec4<f32>(bg * 0.45, 1.0);
    }

    let texel = textureSample(tex, samp, uv);
    return vec4<f32>(mix(bg, texel.rgb, texel.a), 1.0);
}
"#;

pub struct Blitter {
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
}

impl Blitter {
    pub fn new(gpu: &Gpu, target: wgpu::TextureFormat) -> Self {
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("blit"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });

        let bind_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("blit binds"),
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
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                    ],
                });

        let layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("blit layout"),
                bind_group_layouts: &[Some(&bind_layout)],
                immediate_size: 0,
            });

        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("blit"),
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
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

        let params = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blit params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_layout,
            sampler: crate::texture::default_sampler(gpu),
            params,
        }
    }

    /// Scales UVs so the image fits inside the target without distortion.
    ///
    /// The scale is applied to texture coordinates, so the axis that needs
    /// *more* than the full image is the one that grows past `[0, 1]` and
    /// becomes letterbox.
    fn uv_scale(tex: (u32, u32), target: (u32, u32)) -> [f32; 2] {
        let tex_aspect = tex.0 as f32 / tex.1.max(1) as f32;
        let target_aspect = target.0 as f32 / target.1.max(1) as f32;
        if target_aspect > tex_aspect {
            [target_aspect / tex_aspect, 1.0]
        } else {
            [1.0, tex_aspect / target_aspect]
        }
    }

    /// Records a full-target draw of `view`.
    pub fn draw(
        &self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        target_size: (u32, u32),
        texture: &wgpu::TextureView,
        texture_size: (u32, u32),
    ) {
        gpu.write_buffer(
            &self.params,
            0,
            bytemuck::bytes_of(&Params {
                uv_scale: Self::uv_scale(texture_size, target_size),
                checker: 16.0,
                _pad: 0.0,
            }),
        );

        let binds = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit binds"),
            layout: &self.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(texture),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.02,
                        g: 0.02,
                        b: 0.03,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            multiview_mask: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &binds, &[]);
        pass.draw(0..3, 0..1);
    }
}
