//! GPU tests that need no game data -- only an adapter.
//!
//! These use synthetic geometry so they can run anywhere, and they pin the
//! parts of the pipeline that are easy to get backwards: depth comparison,
//! clip-space handedness, and blending.

use render::capture::Offscreen;
use render::mesh::{
    BlendMode, CameraUniform, DepthBuffer, GpuMesh, MeshRenderer, MeshVertex, RenderState,
};
use render::Gpu;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

fn gpu() -> Option<Gpu> {
    match Gpu::block(None) {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            eprintln!("skipping: no GPU adapter ({e})");
            None
        }
    }
}

macro_rules! require_gpu {
    () => {
        match gpu() {
            Some(g) => g,
            None => return,
        }
    };
}

/// A 1x1 texture of one colour, so a draw's output identifies its source.
fn solid(gpu: &Gpu, rgba: [u8; 4]) -> wgpu::TextureView {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // Unorm rather than Srgb so the byte written is the byte sampled.
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// A screen-filling triangle at a fixed clip-space depth.
///
/// Winding is clockwise to match the front face the pipelines expect, and
/// normals point at the light so shading is a no-op and the output colour is
/// the texture's.
fn quad(z: f32) -> [MeshVertex; 3] {
    let n = [0.0, 0.0, 1.0];
    [
        MeshVertex {
            position: [-1.0, -1.0, z],
            normal: n,
            uv: [0.0, 0.0],
        },
        MeshVertex {
            position: [-1.0, 3.0, z],
            normal: n,
            uv: [0.0, 0.0],
        },
        MeshVertex {
            position: [3.0, -1.0, z],
            normal: n,
            uv: [0.0, 0.0],
        },
    ]
}

/// An identity camera, so vertex positions are clip coordinates directly and
/// the test controls depth exactly.
fn identity_camera() -> CameraUniform {
    CameraUniform {
        view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
        eye: [0.0, 0.0, 1.0, 1.0],
        light: [0.0, 0.0, 1.0, 0.0],
    }
}

fn centre_pixel(pixels: &[u8], width: u32, height: u32) -> [u8; 4] {
    let (x, y) = (width / 2, height / 2);
    let o = ((y * width + x) * 4) as usize;
    [pixels[o], pixels[o + 1], pixels[o + 2], pixels[o + 3]]
}

/// Renders two overlapping triangles and returns the centre pixel.
fn render_pair(gpu: &Gpu, first_z: f32, second_z: f32) -> [u8; 4] {
    let (w, h) = (64u32, 64u32);
    let target = Offscreen::new(gpu, w, h, FORMAT);
    let depth = DepthBuffer::new(gpu, w, h);
    let mut meshes = MeshRenderer::new(gpu, FORMAT);

    let state = RenderState {
        blend: BlendMode::Opaque,
        two_sided: true,
        depth_write: true,
    };
    meshes.prepare(gpu, [state]);
    meshes.update_camera(gpu, &identity_camera());

    let first = GpuMesh::upload(gpu, &quad(first_z), &[0, 1, 2]);
    let second = GpuMesh::upload(gpu, &quad(second_z), &[0, 1, 2]);
    let red = meshes.material_bind_group(gpu, &solid(gpu, [255, 0, 0, 255]));
    let green = meshes.material_bind_group(gpu, &solid(gpu, [0, 255, 0, 255]));

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth.view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            multiview_mask: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(meshes.get(state).expect("pipeline"));
        pass.set_bind_group(0, meshes.camera_bind_group(), &[]);

        pass.set_bind_group(1, &red, &[]);
        pass.set_vertex_buffer(0, first.vertices.slice(..));
        pass.set_index_buffer(first.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..3, 0, 0..1);

        pass.set_bind_group(1, &green, &[]);
        pass.set_vertex_buffer(0, second.vertices.slice(..));
        pass.set_index_buffer(second.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..3, 0, 0..1);
    }
    gpu.queue.submit([encoder.finish()]);

    let pixels = target.read_rgba(gpu).expect("readback");
    centre_pixel(&pixels, w, h)
}

/// Nearer geometry must win regardless of draw order. Getting the depth
/// comparison or the clip-space Z direction backwards passes one of these two
/// cases and fails the other, which is why both are checked.
#[test]
fn depth_test_keeps_the_nearer_surface() {
    let gpu = require_gpu!();

    // Far red first, then near green: green must win.
    let px = render_pair(&gpu, 0.8, 0.2);
    assert!(
        px[1] > 200 && px[0] < 60,
        "near geometry drawn second should win, got {px:?}"
    );

    // Near red first, then far green: red must survive.
    let px = render_pair(&gpu, 0.2, 0.8);
    assert!(
        px[0] > 200 && px[1] < 60,
        "far geometry drawn second must be rejected, got {px:?}"
    );
}

/// Each distinct state gets its own pipeline, and repeats are cached rather
/// than rebuilt.
#[test]
fn pipelines_are_cached_per_state() {
    let gpu = require_gpu!();
    let mut meshes = MeshRenderer::new(&gpu, FORMAT);

    let a = RenderState {
        blend: BlendMode::Opaque,
        two_sided: false,
        depth_write: true,
    };
    let b = RenderState {
        two_sided: true,
        ..a
    };
    let c = RenderState {
        blend: BlendMode::Additive,
        two_sided: true,
        depth_write: false,
    };

    meshes.prepare(&gpu, [a, b, c, a, b]);
    assert_eq!(meshes.pipeline_count(), 3);
    assert!(meshes.get(a).is_some());
    assert!(meshes.get(c).is_some());
}

/// Every blend mode must produce a usable pipeline; a shader or state error
/// only surfaces when the pipeline is actually built.
#[test]
fn all_blend_modes_build() {
    let gpu = require_gpu!();
    let mut meshes = MeshRenderer::new(&gpu, FORMAT);

    for blend in [
        BlendMode::Opaque,
        BlendMode::AlphaKey,
        BlendMode::Blend,
        BlendMode::Additive,
    ] {
        for two_sided in [false, true] {
            let state = RenderState {
                blend,
                two_sided,
                depth_write: !blend.is_transparent(),
            };
            meshes.prepare(&gpu, [state]);
            assert!(meshes.get(state).is_some(), "{blend:?} two_sided={two_sided}");
        }
    }
    assert_eq!(meshes.pipeline_count(), 8);
}
