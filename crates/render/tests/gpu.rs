//! GPU tests that need no game data -- only an adapter.
//!
//! These use synthetic geometry so they can run anywhere, and they pin the
//! parts of the pipeline that are easy to get backwards: depth comparison,
//! clip-space handedness, and blending.

use render::capture::Offscreen;
use render::mesh::{
    BlendMode, BoneBuffer, CameraUniform, DepthBuffer, GpuMesh, Instance, InstanceBuffer,
    MeshRenderer, MeshVertex, RenderState, Winding,
};
use render::Gpu;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// One device for the whole binary, created once.
///
/// **These tests deadlocked when each made its own.** The harness runs them on
/// separate threads, so eleven `Gpu::block` calls raced to enumerate adapters
/// and create DX12 devices at once; the run hung with thirty-seven threads and
/// six seconds of CPU between them, indefinitely. Single-threaded the same
/// eleven finish in under six seconds.
///
/// It is a *race*, which is the part that matters: it does not always hang, so
/// a green run is no evidence the next one will be. That is why this is fixed
/// rather than papered over with `RUST_TEST_THREADS=1` -- a workaround in an
/// environment variable is one nobody applies on the run that matters.
///
/// Sharing is safe and is what a real application does anyway: wgpu's device
/// and queue are `Send + Sync`, and every test here only reads from them.
fn gpu() -> Option<&'static Gpu> {
    static GPU: std::sync::OnceLock<Option<Gpu>> = std::sync::OnceLock::new();
    GPU.get_or_init(|| match Gpu::block(None) {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            eprintln!("skipping: no GPU adapter ({e})");
            None
        }
    })
    .as_ref()
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
/// the texture's. `weights` of zero selects the unskinned path.
fn quad_weighted(z: f32, bone: u8, weight: u8) -> [MeshVertex; 3] {
    let vertex = |x: f32, y: f32| MeshVertex {
        position: [x, y, z],
        normal: [0.0, 0.0, 1.0],
        uv: [0.0, 0.0],
        bone_indices: [bone, 0, 0, 0],
        bone_weights: [weight, 0, 0, 0],
    };
    [vertex(-1.0, -1.0), vertex(-1.0, 3.0), vertex(3.0, -1.0)]
}

fn quad(z: f32) -> [MeshVertex; 3] {
    quad_weighted(z, 0, 0)
}

/// A bone palette holding a single transform.
fn bones_with(gpu: &Gpu, meshes: &MeshRenderer, matrices: &[glam::Mat4]) -> BoneBuffer {
    let buffer = meshes.create_bones(gpu, matrices.len().max(1));
    let raw: Vec<[[f32; 4]; 4]> = matrices.iter().map(|m| m.to_cols_array_2d()).collect();
    meshes.update_bones(gpu, &buffer, &raw);
    buffer
}

/// An identity camera, so vertex positions are clip coordinates directly and
/// the test controls depth exactly.
fn identity_camera() -> CameraUniform {
    CameraUniform {
        view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
        eye: [0.0, 0.0, 1.0, 1.0],
        light: [0.0, 0.0, 1.0, 0.0],
        // Unlit: the shaders read `sun.w` of zero as "no light data" and use
        // their placeholder, which is what this test has always exercised.
        sun: [0.0; 4],
        ambient: [0.0; 4],
        fog: [0.0; 4],
        fog_range: [0.0; 4],
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
        winding: Winding::Clockwise,
    };
    meshes.prepare(gpu, [state]);
    meshes.update_camera(gpu, &identity_camera());
    let bones = bones_with(gpu, &meshes, &[glam::Mat4::IDENTITY]);
    // Geometry is authored in clip space here, so the instance transform is
    // identity.
    let instances = InstanceBuffer::upload(gpu, &[Instance::IDENTITY]);

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
        pass.set_bind_group(2, &bones.bind_group, &[]);
        pass.set_vertex_buffer(1, instances.buffer.slice(..));

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
        winding: Winding::Clockwise,
    };
    let b = RenderState {
        two_sided: true,
        ..a
    };
    let c = RenderState {
        blend: BlendMode::Additive,
        two_sided: true,
        depth_write: false,
        winding: Winding::Clockwise,
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
                winding: Winding::Clockwise,
            };
            meshes.prepare(&gpu, [state]);
            assert!(meshes.get(state).is_some(), "{blend:?} two_sided={two_sided}");
        }
    }
    assert_eq!(meshes.pipeline_count(), 8);
}

/// Skinning must actually move geometry, and an unweighted vertex must not be
/// moved at all.
///
/// The bone translates far along +X. A weighted triangle is pushed off screen
/// and disappears; an identical unweighted one stays put. Testing both
/// directions is what distinguishes working skinning from a shader that
/// silently ignores the bone matrix.
#[test]
fn skinning_moves_weighted_vertices_only() {
    let gpu = require_gpu!();
    let (w, h) = (32u32, 32u32);

    let render = |weight: u8| -> [u8; 4] {
        let target = Offscreen::new(&gpu, w, h, FORMAT);
        let depth = DepthBuffer::new(&gpu, w, h);
        let mut meshes = MeshRenderer::new(&gpu, FORMAT);
        let state = RenderState {
            blend: BlendMode::Opaque,
            two_sided: true,
            depth_write: true,
            winding: Winding::Clockwise,
        };
        meshes.prepare(&gpu, [state]);
        meshes.update_camera(&gpu, &identity_camera());

        // Bone 0 shoves everything well outside the clip volume.
        let bones = bones_with(
            &gpu,
            &meshes,
            &[glam::Mat4::from_translation(glam::Vec3::new(10.0, 0.0, 0.0))],
        );
        let mesh = GpuMesh::upload(&gpu, &quad_weighted(0.5, 0, weight), &[0, 1, 2]);
        let instances = InstanceBuffer::upload(&gpu, &[Instance::IDENTITY]);
        let red = meshes.material_bind_group(&gpu, &solid(&gpu, [255, 0, 0, 255]));

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
            pass.set_bind_group(2, &bones.bind_group, &[]);
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_vertex_buffer(1, instances.buffer.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..3, 0, 0..1);
        }
        gpu.queue.submit([encoder.finish()]);
        let pixels = target.read_rgba(&gpu).expect("readback");
        centre_pixel(&pixels, w, h)
    };

    let unweighted = render(0);
    assert!(
        unweighted[0] > 200,
        "an unweighted vertex must ignore the bone, got {unweighted:?}"
    );

    let weighted = render(255);
    assert!(
        weighted[0] < 40,
        "a fully weighted vertex must follow the bone off screen, got {weighted:?}"
    );
}

// --- the sky -------------------------------------------------------------
//
// The gradient's five colours are `Light.dbc`'s, and which end of the array is
// the horizon was settled against the data in `dbc` -- see
// `dbc::light::bands::SKY`. What is *not* settled there is whether the shader
// then paints them the right way up, which is exactly the kind of off-by-one
// that renders perfectly and tints the world upside down. These render pixels
// and read them back, so they are evidence about the shader rather than about
// a second copy of its arithmetic in Rust.

/// Renders the sky alone and returns the frame.
fn render_sky(
    gpu: &Gpu,
    forward: glam::Vec3,
    up: glam::Vec3,
    gradient: &render::sky::Gradient,
    size: (u32, u32),
) -> Vec<u8> {
    let (w, h) = size;
    let target = Offscreen::new(gpu, w, h, FORMAT);
    let depth = DepthBuffer::new(gpu, w, h);
    let sky = render::SkyRenderer::new(gpu, FORMAT);

    let eye = glam::Vec3::ZERO;
    let view = glam::camera::rh::view::look_to_mat4(eye, forward, up);
    // A 90 degree vertical field of view, so the frame spans exactly 45
    // degrees either side of where the camera points and the arithmetic in
    // the assertions below is checkable by hand.
    let proj = glam::camera::rh::proj::directx::perspective(
        std::f32::consts::FRAC_PI_2,
        w as f32 / h as f32,
        0.1,
        1000.0,
    );

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
                    // Green, which appears in no gradient used here: a pixel
                    // the sky failed to cover is then obvious rather than
                    // passing as a dark band.
                    load: wgpu::LoadOp::Clear(wgpu::Color::GREEN),
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
        // No sun: these tests are about the gradient, and a disc added on top
        // of it would be a second thing changing the pixels they measure.
        sky.draw(
            gpu,
            &mut pass,
            proj * view,
            eye,
            gradient,
            glam::Vec3::Z,
            [0.0; 3],
            0.0,
        );
    }
    gpu.queue.submit([encoder.finish()]);
    target.read_rgba(gpu).expect("readback")
}

fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let o = ((y * width + x) * 4) as usize;
    [pixels[o], pixels[o + 1], pixels[o + 2], pixels[o + 3]]
}

/// The zenith end of the array is drawn at the top of the sky, not the bottom.
#[test]
fn the_sky_gradient_is_painted_the_right_way_up() {
    let gpu = require_gpu!();
    let (w, h) = (32u32, 64u32);

    // A ramp from red overhead to blue at the horizon. Every layer differs, so
    // an ordering error cannot hide in two neighbours that happen to match.
    let mut gradient = [[0.0f32; 3]; render::sky::LAYERS];
    for (i, layer) in gradient.iter_mut().enumerate() {
        let t = i as f32 / (render::sky::LAYERS - 1) as f32;
        *layer = [1.0 - t, 0.0, t];
    }

    // Looking along +X, level. The frame then runs from 45 degrees up at the
    // top row to 45 degrees down at the bottom.
    let frame = render_sky(&gpu, glam::Vec3::X, glam::Vec3::Z, &gradient, (w, h));

    let column: Vec<[u8; 4]> = (0..h).map(|y| pixel(&frame, w, w / 2, y)).collect();
    assert!(
        column.iter().all(|p| p[1] < 40),
        "the sky did not cover the frame; some pixels are still the clear colour"
    );
    assert!(
        column[0][0] > column[h as usize - 1][0] + 60,
        "the top of the sky is not the red end: top {:?}, bottom {:?}",
        column[0],
        column[h as usize - 1]
    );
    // Monotone down the column, not merely different at the ends -- a shader
    // that mirrored the gradient about the horizon would pass the check above.
    // The lower half is all horizon-side, and holds rather than reversing.
    for pair in column.windows(2) {
        assert!(
            pair[1][0] <= pair[0][0] + 2,
            "red climbs going down the sky: {:?} then {:?}",
            pair[0],
            pair[1]
        );
    }
}

/// Straight up is the first layer; below the horizon holds the last one.
#[test]
fn the_sky_holds_its_ends_at_the_zenith_and_underfoot() {
    let gpu = require_gpu!();
    let (w, h) = (32u32, 32u32);
    // Five clearly separated greys, so the layer a pixel came from is legible
    // from its value alone.
    let gradient: render::sky::Gradient = [
        [0.02, 0.02, 0.02],
        [0.10, 0.10, 0.10],
        [0.25, 0.25, 0.25],
        [0.50, 0.50, 0.50],
        [1.00, 1.00, 1.00],
    ];

    // Up is X here because the view's up vector cannot be parallel to the
    // direction it looks along.
    let above = render_sky(&gpu, glam::Vec3::Z, glam::Vec3::X, &gradient, (w, h));
    let zenith = centre_pixel(&above, w, h);
    assert!(
        zenith[0] < 60,
        "looking straight up did not give the darkest layer: {zenith:?}"
    );

    let below = render_sky(&gpu, -glam::Vec3::Z, glam::Vec3::X, &gradient, (w, h));
    let underfoot = centre_pixel(&below, w, h);
    assert!(
        underfoot[0] > 240,
        "below the horizon must hold the horizon colour rather than climbing \
         back towards the zenith: {underfoot:?}"
    );
}

// --- precipitation -------------------------------------------------------
//
// Rain that can only be seen by starting a server and typing `.wchange` is
// rain nothing can check. These render it against a known background and read
// the pixels back, which is what makes "it stopped falling" a test failure
// rather than a thing somebody notices in a month.

/// Renders precipitation over a flat background and returns the frame.
fn render_weather(
    gpu: &Gpu,
    shape: &render::precipitation::Shape,
    intensity: f32,
    seconds: f32,
    size: (u32, u32),
) -> Vec<u8> {
    let (w, h) = size;
    let target = Offscreen::new(gpu, w, h, FORMAT);
    let depth = DepthBuffer::new(gpu, w, h);
    let weather = render::PrecipitationRenderer::new(gpu, FORMAT);

    // Above the field's centre looking level, so drops fill the frame.
    let eye = glam::Vec3::new(0.0, 0.0, 20.0);
    let view = glam::camera::rh::view::look_to_mat4(eye, glam::Vec3::X, glam::Vec3::Z);
    let proj = glam::camera::rh::proj::directx::perspective(
        std::f32::consts::FRAC_PI_2,
        w as f32 / h as f32,
        0.1,
        1000.0,
    );

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
        // White on black, so "how much rain" is just how bright the frame is.
        weather.draw(
            gpu,
            &mut pass,
            proj * view,
            eye,
            shape,
            [1.0, 1.0, 1.0],
            intensity,
            seconds,
        );
    }
    gpu.queue.submit([encoder.finish()]);
    target.read_rgba(gpu).expect("readback")
}

/// Mean brightness of a frame, 0 to 255.
fn mean_value(pixels: &[u8]) -> f32 {
    let sum: u64 = pixels.chunks_exact(4).map(|p| p[0] as u64).sum();
    sum as f32 / (pixels.len() / 4) as f32
}

/// It falls, it falls harder when the intensity rises, and it stops.
#[test]
fn precipitation_scales_with_intensity_and_ceases_when_clear() {
    let gpu = require_gpu!();
    let (w, h) = (256u32, 256u32);
    let rain = render::precipitation::Shape::RAIN;

    let none = mean_value(&render_weather(&gpu, &rain, 0.0, 1.0, (w, h)));
    assert_eq!(
        none, 0.0,
        "clear weather drew something: nothing must fall at intensity zero"
    );

    let light = mean_value(&render_weather(&gpu, &rain, 0.4, 1.0, (w, h)));
    let heavy = mean_value(&render_weather(&gpu, &rain, 1.0, 1.0, (w, h)));
    assert!(light > 0.0, "light rain drew nothing at all");
    assert!(
        heavy > light * 1.5,
        "heavier rain is not heavier: light {light}, heavy {heavy}"
    );
}

/// The field moves, and it is still there long after it started.
///
/// **The second half is the one worth having.** Drops fall by `speed * time`,
/// and an `f32` holding an hour of that has lost enough precision to collapse
/// the field into a grid -- a failure that needs a client left running to
/// appear, which is to say one nobody would ever catch by looking. `draw`
/// wraps the clock for exactly this reason; this is what says so.
#[test]
fn precipitation_moves_and_survives_a_long_session() {
    let gpu = require_gpu!();
    let (w, h) = (256u32, 256u32);
    let rain = render::precipitation::Shape::RAIN;

    let first = render_weather(&gpu, &rain, 1.0, 1.0, (w, h));
    let later = render_weather(&gpu, &rain, 1.0, 1.15, (w, h));
    let differing = first
        .chunks_exact(4)
        .zip(later.chunks_exact(4))
        .filter(|(a, b)| a[0].abs_diff(b[0]) > 8)
        .count();
    assert!(
        differing > (w * h) as usize / 200,
        "the field is not moving: only {differing} pixels changed in 150ms"
    );

    // An hour in, and the same amount of rain is falling.
    let fresh = mean_value(&first);
    let old = mean_value(&render_weather(&gpu, &rain, 1.0, 3600.0, (w, h)));
    assert!(
        old > fresh * 0.6 && old < fresh * 1.6,
        "an hour of rain thinned out or piled up: {fresh} then {old}"
    );
}

/// Snow is not rain drawn slower: it is visibly a different thing.
#[test]
fn snow_is_shorter_and_rounder_than_rain() {
    let gpu = require_gpu!();
    let rain = render::precipitation::Shape::RAIN;
    let snow = render::precipitation::Shape::SNOW;
    assert!(
        snow.length < rain.length && snow.fall_speed < rain.fall_speed,
        "snow must be a slower, shorter thing than rain"
    );
    assert!(
        snow.roundness > rain.roundness,
        "snow needs a falloff along its travel or it draws as a tiny bar, \
         which is what it looked like before `roundness` existed"
    );

    // And both actually draw, which the constants alone would not prove.
    for (name, shape) in [("rain", &rain), ("snow", &snow)] {
        let mean = mean_value(&render_weather(&gpu, shape, 1.0, 1.0, (256, 256)));
        assert!(mean > 0.0, "{name} drew nothing");
    }
}

/// The sun is drawn where the sun is, and a storm puts it out.
///
/// **The "somewhere else" half is what makes it a test.** A shader that
/// brightened the whole sky by the disc colour would pass a check that only
/// looked at the centre of the frame, and would look like a sun in exactly one
/// screenshot.
#[test]
fn the_sun_is_drawn_where_the_sun_is() {
    let gpu = require_gpu!();
    let (w, h) = (64u32, 64u32);
    // A black sky, so anything bright in the frame is the disc.
    let gradient: render::sky::Gradient = [[0.0; 3]; render::sky::LAYERS];

    let render_with = |sun: glam::Vec3, visibility: f32| -> Vec<u8> {
        let target = Offscreen::new(&gpu, w, h, FORMAT);
        let depth = DepthBuffer::new(&gpu, w, h);
        let sky = render::SkyRenderer::new(&gpu, FORMAT);
        let eye = glam::Vec3::ZERO;
        // Looking straight along +X at the horizon, with the sun placed on
        // that same axis but well above it.
        let view = glam::camera::rh::view::look_to_mat4(eye, glam::Vec3::X, glam::Vec3::Z);
        let proj = glam::camera::rh::proj::directx::perspective(
            std::f32::consts::FRAC_PI_2,
            1.0,
            0.1,
            1000.0,
        );
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
            sky.draw(
                &gpu,
                &mut pass,
                proj * view,
                eye,
                &gradient,
                sun.normalize(),
                [1.0, 1.0, 1.0],
                visibility,
            );
        }
        gpu.queue.submit([encoder.finish()]);
        target.read_rgba(&gpu).expect("readback")
    };

    // Sun dead ahead and a little above the horizon: it lands above centre.
    let ahead = glam::Vec3::new(1.0, 0.0, 0.30);
    let frame = render_with(ahead, 1.0);
    let brightest = frame
        .chunks_exact(4)
        .enumerate()
        .max_by_key(|(_, p)| p[0])
        .expect("a pixel");
    assert!(
        brightest.1[0] > 200,
        "no disc was drawn at all: brightest pixel {:?}",
        brightest.1
    );
    let (x, y) = ((brightest.0 as u32) % w, (brightest.0 as u32) / w);
    assert!(
        x.abs_diff(w / 2) < 4,
        "the sun is not on the axis it was placed on: column {x} of {w}"
    );
    assert!(y < h / 2, "the sun was placed above the horizon, drawn at row {y}");

    // And it is a *disc*, not a wash: the corners stay black.
    let corner = pixel(&frame, w, 1, h - 2);
    assert!(
        corner[0] < 30,
        "the whole sky brightened rather than a disc being drawn: {corner:?}"
    );

    // Behind the camera it is not drawn at all.
    let behind = render_with(glam::Vec3::new(-1.0, 0.0, 0.30), 1.0);
    assert!(
        behind.chunks_exact(4).all(|p| p[0] < 30),
        "a sun behind the camera was drawn in front of it"
    );

    // A storm puts it out.
    let overcast = render_with(ahead, 0.0);
    assert!(
        overcast.chunks_exact(4).all(|p| p[0] < 30),
        "the sun burned through a full storm"
    );
}

/// A setting sun fades out, and the moon comes up on the other side.
///
/// **The handover is the point.** One `Light.dbc` band serves the sun and the
/// moon because only one is ever up, so the direction handed to the shader
/// jumps to the opposite side of the sky the instant the sun sets. Without a
/// fade at the horizon that is a disc teleporting; the test that catches it has
/// to look at the sky *behind* the camera as well as in front.
#[test]
fn a_setting_sun_hands_over_to_the_moon() {
    let gpu = require_gpu!();
    let gradient: render::sky::Gradient = [[0.0; 3]; render::sky::LAYERS];
    let (w, h) = (48u32, 48u32);

    // How bright the sky gets when the camera looks along `at`, for a sun in
    // direction `sun`.
    let brightest = |sun: glam::Vec3, at: glam::Vec3| -> u8 {
        let target = Offscreen::new(&gpu, w, h, FORMAT);
        let depth = DepthBuffer::new(&gpu, w, h);
        let sky = render::SkyRenderer::new(&gpu, FORMAT);
        let eye = glam::Vec3::ZERO;
        let view = glam::camera::rh::view::look_to_mat4(eye, at.normalize(), glam::Vec3::Z);
        let proj = glam::camera::rh::proj::directx::perspective(
            std::f32::consts::FRAC_PI_2,
            1.0,
            0.1,
            1000.0,
        );
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
            sky.draw(
                &gpu,
                &mut pass,
                proj * view,
                eye,
                &gradient,
                sun.normalize(),
                [1.0, 1.0, 1.0],
                1.0,
            );
        }
        gpu.queue.submit([encoder.finish()]);
        let frame = target.read_rgba(&gpu).expect("readback");
        frame.chunks_exact(4).map(|p| p[0]).max().unwrap_or(0)
    };

    let east = glam::Vec3::new(1.0, 0.0, 0.0);
    let west = glam::Vec3::new(-1.0, 0.0, 0.0);

    // High in the east: bright there, nothing behind.
    let high = glam::Vec3::new(1.0, 0.0, 0.6);
    assert!(brightest(high, east) > 200, "a risen sun was not drawn");
    assert!(brightest(high, west) < 30, "the sun was also drawn behind us");

    // Sinking towards the horizon: dimmer, and still in the east.
    let low = glam::Vec3::new(1.0, 0.0, 0.05);
    let sinking = brightest(low, east);
    assert!(
        sinking < brightest(high, east),
        "a sun near the horizon is not dimmer than one overhead: {sinking}"
    );

    // At the horizon exactly, it is out -- which is what lets the moon take
    // over without either of them jumping.
    assert!(
        brightest(east, east) < 30 && brightest(east, west) < 30,
        "a sun sitting exactly on the horizon is still lit"
    );

    // Below it, the body is up on the *other* side: that is the moon.
    let night = glam::Vec3::new(1.0, 0.0, -0.6);
    assert!(
        brightest(night, west) > 200,
        "the moon is not up when the sun is down"
    );
    assert!(
        brightest(night, east) < 30,
        "the set sun is still being drawn where it set"
    );
}

/// The liquid pipeline builds, which is the only thing that compiles its WGSL.
///
/// **A shader is not checked by `cargo build`.** It is a string handed to the
/// driver at pipeline creation, so a syntax error in it -- a `const` between
/// `@fragment` and its function, an attribute the vertex layout does not
/// supply -- is a runtime panic in whichever binary happens to draw water
/// first. The whole render crate can be green with a shader that cannot
/// compile, which is exactly what happened while this was being written.
///
/// Building the pipeline is the check. It costs one device that already
/// exists and it fails loudly here rather than in front of somebody flying
/// over a river.
#[test]
fn the_liquid_pipeline_compiles() {
    let Some(gpu) = gpu() else { return };
    let renderer = render::LiquidRenderer::new(gpu, FORMAT);
    // And its bind group layout accepts a real texture view, so the surface
    // binding matches what the shader declares.
    let view = solid(gpu, [0, 0, 0, 128]);
    let _ = renderer.bind_surface(gpu, &view);
}

/// Water and lava are read out of different channels, and the vertex says
/// which.
///
/// Draws one full-screen sheet of each against a black background and reads
/// the middle pixel. The art is synthetic: a **black RGB with half alpha**,
/// which is what `lake_a` and `ocean_h` actually are. Read as a colour
/// texture that multiplies to nothing; read as alpha-keyed it comes out the
/// tint. So the two readings differ by the whole of the visible result, and
/// this test fails on the bug that shipped rather than merely describing it.
#[test]
fn an_alpha_keyed_surface_takes_its_colour_from_the_tint() {
    let Some(gpu) = gpu() else { return };
    let renderer = render::LiquidRenderer::new(gpu, FORMAT);
    // Black RGB, half alpha: no colour at all, all pattern.
    let art = solid(gpu, [0, 0, 0, 128]);
    let bind = renderer.bind_surface(gpu, &art);

    let draw = |alpha_keyed: f32| -> [u8; 4] {
        let (w, h) = (32u32, 32u32);
        let target = Offscreen::new(gpu, w, h, FORMAT);
        let depth = DepthBuffer::new(gpu, w, h);
        // A full-screen triangle in clip space, opaque, tinted pure red so
        // any colour reaching the target can only have come from the tint.
        let corners = [[-3.0f32, -1.0, 0.5], [1.0, -1.0, 0.5], [1.0, 3.0, 0.5]];
        let vertices: Vec<render::LiquidVertex> = corners
            .iter()
            .map(|p| render::LiquidVertex {
                position: *p,
                uv_motion: [0.0, 0.0, 0.0, 0.0],
                tint: [1.0, 0.0, 0.0, 1.0],
                mode: [1.0, alpha_keyed],
            })
            .collect();
        let buffer = {
            use wgpu::util::DeviceExt;
            gpu.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("liquid test"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                })
        };

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("liquid test"),
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
            // Emissive, so the sun term cannot muddy what is being measured.
            renderer.begin(
                gpu,
                &mut pass,
                glam::Mat4::IDENTITY,
                glam::Vec3::ZERO,
                glam::Vec3::Z,
                [1.0; 3],
                1.0,
                [1.0; 3],
                [0.0; 3],
                (0.0, 0.0),
                0.0,
            );
            pass.set_bind_group(1, &bind, &[]);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..3, 0..1);
        }
        gpu.queue.submit([encoder.finish()]);
        let pixels = target.read_rgba(gpu).expect("readback");
        centre_pixel(&pixels, w, h)
    };

    let keyed = draw(1.0);
    let coloured = draw(0.0);
    assert!(
        keyed[0] > 100,
        "alpha-keyed water should show its tint, got {keyed:?}"
    );
    assert!(
        coloured[0] < 20,
        "a black colour texture multiplies the tint away, got {coloured:?} \
         -- this is the reading that made every river in Elwynn black"
    );
}

/// Renders a list of sprites against black and hands back the pixels.
///
/// The camera looks down -X from a distance, so a sprite at the origin sits in
/// the middle of the frame and its size in world units maps to a predictable
/// number of pixels.
fn render_sprites(
    gpu: &Gpu,
    sprites: &[render::SpriteInstance],
    blend: render::particles::Blend,
    size: (u32, u32),
) -> Vec<u8> {
    let (w, h) = size;
    let target = Offscreen::new(gpu, w, h, FORMAT);
    let depth = DepthBuffer::new(gpu, w, h);
    let mut particles = render::ParticleRenderer::new(gpu, FORMAT);
    particles.begin(gpu, [blend]);
    particles.reserve(gpu, sprites.len(), 0);
    particles.upload_sprites(gpu, sprites);

    let eye = glam::Vec3::new(10.0, 0.0, 0.0);
    let view = glam::camera::rh::view::look_to_mat4(eye, -glam::Vec3::X, glam::Vec3::Z);
    let proj = glam::camera::rh::proj::directx::perspective(
        std::f32::consts::FRAC_PI_2,
        w as f32 / h as f32,
        0.1,
        1000.0,
    );
    // The camera's own axes, taken from the same view matrix the pass is drawn
    // with rather than rebuilt -- a billboard widened along a basis that
    // disagrees with the projection leans, and reads as a bad texture.
    let right = view.row(0).truncate();
    let up = view.row(1).truncate();
    particles.set_camera(gpu, proj * view, right, up);

    let white = solid(gpu, [255, 255, 255, 255]);
    let sheet = particles.sheet_bind_group(gpu, &white);

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
        assert!(
            particles.draw_sprites(&mut pass, &sheet, blend, 0..sprites.len() as u32),
            "no pipeline for {blend:?} -- `begin` was not told about it"
        );
    }
    gpu.queue.submit([encoder.finish()]);
    target.read_rgba(gpu).expect("readback")
}

fn sprite_at(position: [f32; 3], size: f32, color: [f32; 4]) -> render::SpriteInstance {
    render::SpriteInstance {
        position: [position[0], position[1], position[2], 0.0],
        size: [size, size, 0.0, 0.0],
        color,
        uv: [0.0, 0.0, 1.0, 1.0],
    }
}

/// A sprite is drawn where it was put, at the size it was given.
///
/// The pair is the point. Asserting only that something appeared passes just
/// as well when every sprite is drawn at the origin, which is exactly what a
/// zeroed instance buffer produces -- and geometry drawn at the wrong place
/// looks like geometry never drawn.
#[test]
fn a_sprite_lands_where_it_is_put() {
    let gpu = require_gpu!();
    let (w, h) = (128u32, 128u32);

    let centred = render_sprites(
        &gpu,
        &[sprite_at([0.0, 0.0, 0.0], 0.5, [1.0; 4])],
        render::particles::Blend::Alpha,
        (w, h),
    );
    assert!(
        centre_pixel(&centred, w, h)[0] > 200,
        "a sprite at the origin did not cover the middle of the frame"
    );

    // Well off to one side, and the middle must go dark again.
    let offset = render_sprites(
        &gpu,
        &[sprite_at([0.0, 6.0, 0.0], 0.5, [1.0; 4])],
        render::particles::Blend::Alpha,
        (w, h),
    );
    assert!(
        centre_pixel(&offset, w, h)[0] < 20,
        "a sprite moved sideways still covered the centre: it is being drawn \
         at a fixed place rather than at its own position"
    );
    assert!(
        mean_value(&offset) > 0.0,
        "the moved sprite vanished entirely instead of moving"
    );
}

/// Additive blending piles up and alpha does not.
///
/// Three fully-opaque sprites of a quarter grey in exactly the same place.
/// Added they come to three quarters; alpha-blended each one *replaces* the
/// last and the result is a quarter however many there are. 22,347 of the
/// archives' 26,374 emitters are additive, so getting this backwards makes
/// every fire in the game a flat grey smudge.
///
/// Opaque rather than half-transparent on purpose: at alpha 0.5 both modes
/// brighten with each sprite -- alpha converges on the source colour instead
/// of staying put -- and the test would be measuring the *rate* of two curves
/// that both go up, which sRGB encoding then flattens into a coin flip. This
/// version has one number that must not move at all.
#[test]
fn additive_sprites_accumulate_where_alpha_ones_do_not() {
    let gpu = require_gpu!();
    let (w, h) = (64u32, 64u32);
    let quarter = [0.25, 0.25, 0.25, 1.0];
    let one = [sprite_at([0.0; 3], 0.5, quarter)];
    let three = [one[0], one[0], one[0]];

    let sample = |sprites: &[render::SpriteInstance], blend| {
        centre_pixel(&render_sprites(&gpu, sprites, blend, (w, h)), w, h)[0]
    };
    let add_one = sample(&one, render::particles::Blend::Additive);
    let add_three = sample(&three, render::particles::Blend::Additive);
    let alpha_one = sample(&one, render::particles::Blend::Alpha);
    let alpha_three = sample(&three, render::particles::Blend::Alpha);

    assert!(add_one > 0, "one additive sprite drew nothing");
    assert!(
        add_three > add_one + 20,
        "three additive sprites are no brighter than one: {add_one} then {add_three}"
    );
    assert_eq!(
        alpha_one, alpha_three,
        "three opaque alpha sprites in one place must read as one"
    );
}

/// The counters say both numbers.
///
/// "Nothing was skipped" and "there was nothing to skip" are different states
/// and a counter that only speaks on failure cannot tell them apart -- this
/// project has paid for that twice.
#[test]
fn the_sprite_buffer_reports_what_it_dropped_and_what_it_drew() {
    let gpu = require_gpu!();
    let mut particles = render::ParticleRenderer::new(&gpu, FORMAT);
    particles.begin(&gpu, [render::particles::Blend::Additive]);

    let fits = vec![render::SpriteInstance::default(); 16];
    assert_eq!(particles.upload_sprites(&gpu, &fits), 16);
    assert_eq!(particles.drawn, 16);
    assert_eq!(particles.skipped, 0);

    // More than the initial reservation, without reserving.
    let too_many = vec![render::SpriteInstance::default(); 5000];
    let fitted = particles.upload_sprites(&gpu, &too_many);
    assert!(fitted < too_many.len());
    assert!(
        particles.skipped > 0,
        "the buffer overflowed and reported nothing"
    );

    // ...and reserving makes room, which is the other half: a test asserting
    // only that overflow is reported passes when nothing ever fits.
    particles.begin(&gpu, [render::particles::Blend::Additive]);
    particles.reserve(&gpu, too_many.len(), 0);
    assert_eq!(particles.upload_sprites(&gpu, &too_many), too_many.len());
    assert_eq!(particles.skipped, 0);
}
