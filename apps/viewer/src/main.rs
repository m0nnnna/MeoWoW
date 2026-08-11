//! Windowed asset viewer.
//!
//! Shows a texture or a model from a WoW 3.3.5a installation. It exists to
//! prove the layers work together in one process: the archive layer finds a
//! file, the format crates decode it, and the GPU draws it.
//!
//! It also runs headless (`--screenshot`), rendering one frame to a PNG without
//! opening a window, which keeps the render path checkable from a terminal and
//! in CI.

mod model;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;
use model::{LoadedModel, Variations};
use mpq::Chain;
use render::camera::Orbit;
use render::capture::Offscreen;
use render::mesh::{BoneBuffer, DepthBuffer, MeshRenderer};
use render::{texture::upload_blp, Blitter, Gpu, UploadedTexture};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// Shown when nothing else is asked for: present in every locale of a stock
/// install, so the window is never empty.
const DEFAULT_TEXTURE: &str = r"Interface\Icons\Spell_Fire_Fireball02.blp";

#[derive(Parser, Clone)]
#[command(name = "wow-viewer", about = "View WoW 3.3.5a client assets")]
struct Args {
    /// Path to the installation's `Data` directory.
    #[arg(long, short, env = "WOW_DATA")]
    data: PathBuf,

    #[arg(long, default_value = "enUS")]
    locale: String,

    /// Archive path of a texture to show.
    #[arg(long)]
    texture: Option<String>,

    /// Archive path of a model to show. `.mdx` is rewritten to `.m2`.
    #[arg(long)]
    model: Option<String>,

    /// Creature display id; supplies both the model and its skins.
    #[arg(long)]
    creature: Option<u32>,

    /// Level of detail, 0 being the most detailed.
    #[arg(long, default_value_t = 0)]
    lod: u32,

    /// Render one frame to this PNG and exit, without opening a window.
    #[arg(long)]
    screenshot: Option<PathBuf>,

    /// Camera yaw in degrees, for reproducible screenshots.
    #[arg(long)]
    yaw: Option<f32>,

    /// Camera pitch in degrees.
    #[arg(long)]
    pitch: Option<f32>,

    /// Animation index to play. Omit for the bind pose.
    #[arg(long)]
    anim: Option<usize>,

    /// Time within the animation, in milliseconds.
    #[arg(long, default_value_t = 0)]
    anim_time: u32,

    #[arg(long, default_value_t = 1280)]
    width: u32,

    #[arg(long, default_value_t = 720)]
    height: u32,
}

/// What the viewer is currently showing.
enum Scene {
    Texture(Box<UploadedTexture>),
    Model(Box<LoadedModel>),
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    let mut chain = Chain::open_wow_data(&args.data, &args.locale)
        .with_context(|| format!("opening archives under {}", args.data.display()))?;

    if let Some(path) = args.screenshot.clone() {
        return screenshot(&args, &mut chain, &path);
    }

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(args, chain);
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// Resolves the command line into something to draw.
fn build_scene(gpu: &Gpu, chain: &mut Chain, args: &Args) -> Result<Scene> {
    if let Some(display_id) = args.creature {
        let (path, variations) = model::creature(chain, display_id)?;
        let loaded = model::load(gpu, chain, &path, &variations, args.lod)?;
        return Ok(Scene::Model(Box::new(loaded)));
    }
    if let Some(path) = &args.model {
        let loaded = model::load(gpu, chain, path, &Variations::default(), args.lod)?;
        return Ok(Scene::Model(Box::new(loaded)));
    }
    let path = args.texture.as_deref().unwrap_or(DEFAULT_TEXTURE);
    let parsed = blp::Blp::parse(&chain.read(path)?)?;
    Ok(Scene::Texture(Box::new(upload_blp(gpu, &parsed, path))))
}

fn initial_camera(scene: &Scene, args: &Args) -> Orbit {
    let mut camera = match scene {
        Scene::Model(m) => Orbit::frame(m.min, m.max),
        Scene::Texture(_) => Orbit::default(),
    };
    if let Some(yaw) = args.yaw {
        camera.yaw = yaw.to_radians();
    }
    if let Some(pitch) = args.pitch {
        camera.pitch = pitch.to_radians();
    }
    camera
}

/// Records one frame. Shared by the windowed and headless paths so the two
/// cannot drift apart.
#[allow(clippy::too_many_arguments)]
fn draw_scene(
    gpu: &Gpu,
    encoder: &mut wgpu::CommandEncoder,
    color: &wgpu::TextureView,
    depth: &wgpu::TextureView,
    size: (u32, u32),
    scene: &Scene,
    camera: &Orbit,
    blitter: &Blitter,
    meshes: &MeshRenderer,
    material_binds: &[wgpu::BindGroup],
    bones: Option<&BoneBuffer>,
) {
    match scene {
        Scene::Texture(tex) => {
            blitter.draw(gpu, encoder, color, size, &tex.view, (tex.width, tex.height));
        }
        Scene::Model(m) => {
            let aspect = size.0 as f32 / size.1.max(1) as f32;
            meshes.update_camera(gpu, &camera.uniform(aspect));

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("model"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.06,
                            b: 0.08,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
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

            pass.set_bind_group(0, meshes.camera_bind_group(), &[]);
            if let Some(bones) = bones {
                pass.set_bind_group(2, &bones.bind_group, &[]);
            }
            pass.set_vertex_buffer(0, m.mesh.vertices.slice(..));
            pass.set_index_buffer(m.mesh.indices.slice(..), wgpu::IndexFormat::Uint32);

            for draw in &m.draws {
                let (Some(pipeline), Some(binds)) =
                    (meshes.get(draw.state), material_binds.get(draw.texture))
                else {
                    continue;
                };
                pass.set_pipeline(pipeline);
                pass.set_bind_group(1, binds, &[]);
                pass.draw_indexed(
                    draw.first_index..draw.first_index + draw.index_count,
                    0,
                    0..1,
                );
            }
        }
    }
}

/// Evaluates a pose and uploads it, returning the matrices for reuse.
///
/// The bind pose is all-identity rather than a special case: an M2's vertices
/// are already stored in bind pose, so identity matrices reproduce exactly what
/// the unskinned renderer drew.
fn upload_pose(
    gpu: &Gpu,
    meshes: &MeshRenderer,
    bones: &BoneBuffer,
    m: &LoadedModel,
    anim: Option<usize>,
    time_ms: u32,
) {
    let pose: Vec<[[f32; 4]; 4]> = match anim {
        Some(seq) if seq < m.sequences.len() => {
            // Wrap into the sequence so a caller can pass a free-running clock.
            let duration = m.sequences[seq].duration_ms.max(1);
            let t = time_ms % duration;
            m2::Model::pose_bones(&m.bones, seq, t)
                .iter()
                .map(|mat| mat.to_cols_array_2d())
                .collect()
        }
        _ => vec![glam::Mat4::IDENTITY.to_cols_array_2d(); m.bones.len().max(1)],
    };
    meshes.update_bones(gpu, bones, &pose);
}

fn material_bind_groups(
    gpu: &Gpu,
    meshes: &MeshRenderer,
    scene: &Scene,
) -> Vec<wgpu::BindGroup> {
    match scene {
        Scene::Model(m) => m
            .textures
            .iter()
            .map(|t| meshes.material_bind_group(gpu, &t.view))
            .collect(),
        Scene::Texture(_) => Vec::new(),
    }
}

fn describe(scene: &Scene) -> String {
    match scene {
        Scene::Texture(t) => format!(
            "{}x{} {:?}, {} mips, {}",
            t.width,
            t.height,
            t.format,
            t.mip_levels,
            match t.fallback_reason {
                None => "uploaded compressed".to_string(),
                Some(r) => format!("CPU-decoded ({r})"),
            }
        ),
        Scene::Model(m) => {
            let mut s = format!(
                "{}\n{} vertices, {} triangles\n{} draw calls, {} textures",
                m.path,
                m.vertex_count,
                m.triangle_count,
                m.draws.len(),
                m.textures.len()
            );
            // Submesh ids identify geosets -- hair, armour, facial features --
            // which is what character customisation will switch on later.
            let mut ids: Vec<u16> = m.draws.iter().map(|d| d.submesh_id).collect();
            ids.sort_unstable();
            ids.dedup();
            let shown: Vec<String> = ids.iter().take(12).map(u16::to_string).collect();
            s.push_str(&format!(
                "\ngeosets: {}{}",
                shown.join(" "),
                if ids.len() > 12 { " ..." } else { "" }
            ));
            if !m.missing_textures.is_empty() {
                s.push_str(&format!(
                    "\nunresolved: {}",
                    m.missing_textures.join(", ")
                ));
            }
            s
        }
    }
}

// ---------------------------------------------------------------- headless

fn screenshot(args: &Args, chain: &mut Chain, out: &std::path::Path) -> Result<()> {
    let gpu = Gpu::block(None)?;
    tracing::info!("adapter: {}", gpu.describe());

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let target = Offscreen::new(&gpu, args.width, args.height, format);
    let depth = DepthBuffer::new(&gpu, args.width, args.height);
    let blitter = Blitter::new(&gpu, format);
    let mut meshes = MeshRenderer::new(&gpu, format);

    let scene = build_scene(&gpu, chain, args)?;
    let camera = initial_camera(&scene, args);
    let mut bones = None;
    if let Scene::Model(m) = &scene {
        meshes.prepare(&gpu, m.draws.iter().map(|d| d.state));
        let buffer = meshes.create_bones(&gpu, m.bones.len());
        upload_pose(&gpu, &meshes, &buffer, m, args.anim, args.anim_time);
        bones = Some(buffer);
    }
    let binds = material_bind_groups(&gpu, &meshes, &scene);

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("screenshot"),
        });
    draw_scene(
        &gpu,
        &mut encoder,
        &target.view,
        &depth.view,
        (target.width, target.height),
        &scene,
        &camera,
        &blitter,
        &meshes,
        &binds,
        bones.as_ref(),
    );
    gpu.queue.submit([encoder.finish()]);

    let rgba = target.read_rgba(&gpu)?;
    write_png(out, &rgba, target.width, target.height)?;
    println!(
        "{}\nrendered {}x{} -> {}",
        describe(&scene),
        target.width,
        target.height,
        out.display()
    );
    Ok(())
}

fn write_png(path: &std::path::Path, rgba: &[u8], width: u32, height: u32) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgba)?;
    Ok(())
}

// ----------------------------------------------------------------- windowed

struct Renderer {
    gpu: Gpu,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    depth: DepthBuffer,
    blitter: Blitter,
    meshes: MeshRenderer,
    material_binds: Vec<wgpu::BindGroup>,
    bones: Option<BoneBuffer>,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    scene: Option<Scene>,
}

struct App {
    args: Args,
    chain: Chain,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    camera: Orbit,
    dragging: bool,
    last_cursor: Option<(f64, f64)>,
    error: Option<String>,
    last_frame: Instant,
    frame_ms: f32,
    /// Selected sequence, or `None` for the bind pose.
    anim: Option<usize>,
    /// Elapsed time within the current sequence.
    anim_time_ms: u32,
    playing: bool,
    speed: f32,
}

impl App {
    fn new(args: Args, chain: Chain) -> Self {
        Self {
            args,
            chain,
            window: None,
            renderer: None,
            camera: Orbit::default(),
            dragging: false,
            last_cursor: None,
            error: None,
            last_frame: Instant::now(),
            frame_ms: 0.0,
            anim: None,
            anim_time_ms: 0,
            playing: true,
            speed: 1.0,
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("open-wow viewer")
            .with_inner_size(winit::dpi::LogicalSize::new(
                self.args.width,
                self.args.height,
            ));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        let gpu = Gpu::block(None).expect("gpu");
        let surface = gpu
            .instance
            .create_surface(window.clone())
            .expect("create surface");

        let caps = surface.get_capabilities(&gpu.adapter);
        // Prefer an sRGB target so sRGB textures land on screen correctly.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: caps.present_modes[0],
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            color_space: wgpu::SurfaceColorSpace::Auto,
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&gpu.device, &config);

        let egui_ctx = egui::Context::default();
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            None,
            None,
            None,
        );
        let egui_renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            format,
            egui_wgpu::RendererOptions::default(),
        );

        let blitter = Blitter::new(&gpu, format);
        let mut meshes = MeshRenderer::new(&gpu, format);
        let depth = DepthBuffer::new(&gpu, config.width, config.height);

        let scene = match build_scene(&gpu, &mut self.chain, &self.args) {
            Ok(scene) => Some(scene),
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                tracing::error!("{e:#}");
                None
            }
        };
        if let Some(scene) = &scene {
            self.camera = initial_camera(scene, &self.args);
            if let Scene::Model(m) = scene {
                meshes.prepare(&gpu, m.draws.iter().map(|d| d.state));
            }
        }
        let mut bones = None;
        if let Some(Scene::Model(m)) = &scene {
            bones = Some(meshes.create_bones(&gpu, m.bones.len()));
            // Default to the first sequence that actually has keyframes, so
            // the model is moving on arrival rather than standing in bind pose.
            self.anim = self.args.anim.or_else(|| {
                (!m.sequences.is_empty()).then_some(0)
            });
        }
        let material_binds = scene
            .as_ref()
            .map(|s| material_bind_groups(&gpu, &meshes, s))
            .unwrap_or_default();

        self.renderer = Some(Renderer {
            gpu,
            surface,
            config,
            depth,
            blitter,
            meshes,
            material_binds,
            bones,
            egui_ctx,
            egui_state,
            egui_renderer,
            scene,
        });
        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let (Some(window), Some(r)) = (self.window.clone(), self.renderer.as_mut()) else {
            return;
        };
        if r.egui_state.on_window_event(&window, &event).consumed {
            window.request_redraw();
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                r.config.width = size.width.max(1);
                r.config.height = size.height.max(1);
                r.surface.configure(&r.gpu.device, &r.config);
                r.depth.resize(&r.gpu, r.config.width, r.config.height);
                window.request_redraw();
            }
            WindowEvent::MouseInput { state, button, .. } if button == MouseButton::Left => {
                self.dragging = state == ElementState::Pressed;
                if !self.dragging {
                    self.last_cursor = None;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let now = (position.x, position.y);
                if self.dragging {
                    if let Some(prev) = self.last_cursor {
                        // Roughly half a turn across the window, which reads as
                        // direct manipulation rather than a flick.
                        const SPEED: f32 = 0.008;
                        self.camera.orbit(
                            -(now.0 - prev.0) as f32 * SPEED,
                            (now.1 - prev.1) as f32 * SPEED,
                        );
                    }
                }
                self.last_cursor = Some(now);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 60.0,
                };
                self.camera.zoom(0.88f32.powf(notches));
            }
            WindowEvent::RedrawRequested => {
                self.redraw(&window);
                window.request_redraw();
            }
            _ => {}
        }
    }
}

impl App {
    fn redraw(&mut self, window: &Arc<Window>) {
        let now = Instant::now();
        self.frame_ms = now.duration_since(self.last_frame).as_secs_f32() * 1000.0;
        self.last_frame = now;

        let ui_output = self.build_ui(window);
        let camera = self.camera;

        // Advance the clock before posing, from real elapsed time so playback
        // speed is independent of frame rate.
        if self.playing {
            let step = (self.frame_ms * self.speed).max(0.0) as u32;
            self.anim_time_ms = self.anim_time_ms.wrapping_add(step);
        }
        let (anim, anim_time) = (self.anim, self.anim_time_ms);

        let Some(r) = self.renderer.as_mut() else {
            return;
        };
        if let (Some(Scene::Model(m)), Some(bones)) = (&r.scene, &r.bones) {
            upload_pose(&r.gpu, &r.meshes, bones, m, anim, anim_time);
        }

        use wgpu::CurrentSurfaceTexture as Acquired;
        let frame = match r.surface.get_current_texture() {
            Acquired::Success(frame) => frame,
            // Suboptimal still yields a usable frame; reconfiguring restores
            // the fast path next time without dropping this one.
            Acquired::Suboptimal(frame) => {
                r.surface.configure(&r.gpu.device, &r.config);
                frame
            }
            // Routine on resize and display changes: reconfigure, skip a frame.
            Acquired::Lost | Acquired::Outdated => {
                r.surface.configure(&r.gpu.device, &r.config);
                return;
            }
            Acquired::Timeout | Acquired::Occluded => return,
            Acquired::Validation => {
                tracing::error!("surface validation error while acquiring a frame");
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = r
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });

        let size = (r.config.width, r.config.height);
        if let Some(scene) = &r.scene {
            draw_scene(
                &r.gpu,
                &mut encoder,
                &view,
                &r.depth.view,
                size,
                scene,
                &camera,
                &r.blitter,
                &r.meshes,
                &r.material_binds,
                r.bones.as_ref(),
            );
        }

        let clipped = r
            .egui_ctx
            .tessellate(ui_output.shapes, ui_output.pixels_per_point);
        // Each entry may carry several partial updates for one texture.
        for (id, deltas) in &ui_output.textures_delta.set {
            for delta in deltas {
                r.egui_renderer
                    .update_texture(&r.gpu.device, &r.gpu.queue, *id, delta);
            }
        }
        let desc = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [size.0, size.1],
            pixels_per_point: ui_output.pixels_per_point,
        };
        r.egui_renderer
            .update_buffers(&r.gpu.device, &r.gpu.queue, &mut encoder, &clipped, &desc);
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                multiview_mask: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            r.egui_renderer
                .render(&mut pass.forget_lifetime(), &clipped, &desc);
        }
        for id in &ui_output.textures_delta.free {
            r.egui_renderer.free_texture(id);
        }

        r.gpu.queue.submit([encoder.finish()]);
        r.gpu.queue.present(frame);
    }

    fn build_ui(&mut self, window: &Arc<Window>) -> egui::FullOutput {
        let Some(r) = self.renderer.as_mut() else {
            return egui::FullOutput::default();
        };
        let input = r.egui_state.take_egui_input(window);
        let ctx = r.egui_ctx.clone();

        let gpu_line = r.gpu.describe();
        let bc = r.gpu.supports_bc();
        let pipelines = r.meshes.pipeline_count();
        let summary = r.scene.as_ref().map(describe);
        let (error, frame_ms) = (self.error.clone(), self.frame_ms);
        let camera = self.camera;

        // Snapshot what the picker needs, so the UI closure does not borrow the
        // renderer while it mutates animation state.
        let animations: Vec<(usize, String, u32)> = match &r.scene {
            Some(Scene::Model(m)) => m
                .sequences
                .iter()
                .enumerate()
                .map(|(i, seq)| {
                    (
                        i,
                        m.sequence_names
                            .get(i)
                            .cloned()
                            .unwrap_or_else(|| format!("#{i}")),
                        seq.duration_ms,
                    )
                })
                .collect(),
            _ => Vec::new(),
        };
        let animated_bones = match &r.scene {
            Some(Scene::Model(m)) => m.bones.iter().filter(|b| b.is_animated()).count(),
            _ => 0,
        };
        let (mut anim, mut playing, mut speed, mut anim_time) =
            (self.anim, self.playing, self.speed, self.anim_time_ms);

        let output = ctx.run_ui(input, |ctx| {
            egui::Window::new("open-wow")
                .default_width(430.0)
                .show(ctx, |ui| {
                    ui.label(egui::RichText::new(&gpu_line).strong());
                    ui.label(format!(
                        "BC compression: {} | {frame_ms:.1} ms/frame | {pipelines} pipelines",
                        if bc { "yes" } else { "no" }
                    ));
                    ui.separator();
                    match &summary {
                        Some(s) => ui.label(egui::RichText::new(s).monospace()),
                        None => ui.label("nothing loaded"),
                    };
                    if let Some(e) = &error {
                        ui.colored_label(egui::Color32::from_rgb(220, 120, 120), e);
                    }
                    if !animations.is_empty() {
                        ui.separator();
                        ui.label(format!(
                            "{} animations, {animated_bones} animated bones",
                            animations.len()
                        ));
                        ui.horizontal(|ui| {
                            if ui.button(if playing { "pause" } else { "play" }).clicked() {
                                playing = !playing;
                            }
                            if ui.button("restart").clicked() {
                                anim_time = 0;
                            }
                            ui.add(egui::Slider::new(&mut speed, 0.0..=2.0).text("speed"));
                        });
                        let current = anim
                            .and_then(|i| animations.get(i))
                            .map(|(_, n, d)| format!("{n} ({d} ms)"))
                            .unwrap_or_else(|| "bind pose".into());
                        ui.label(format!("playing: {current}  t={anim_time} ms"));

                        egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                            if ui.selectable_label(anim.is_none(), "bind pose").clicked() {
                                anim = None;
                            }
                            for (i, name, duration) in &animations {
                                let label = format!("{i:>3}  {name}  {duration} ms");
                                if ui.selectable_label(anim == Some(*i), label).clicked() {
                                    anim = Some(*i);
                                    anim_time = 0;
                                }
                            }
                        });
                    }

                    ui.separator();
                    ui.label(format!(
                        "camera: yaw {:.0}\u{b0}  pitch {:.0}\u{b0}  distance {:.2}",
                        camera.yaw.to_degrees(),
                        camera.pitch.to_degrees(),
                        camera.distance
                    ));
                    ui.weak("drag to orbit, scroll to zoom");
                });
        });

        // Selecting a different animation restarts it; otherwise keep the
        // clock the UI may have reset.
        self.anim_time_ms = if anim != self.anim { 0 } else { anim_time };
        self.anim = anim;
        self.playing = playing;
        self.speed = speed;

        if let Some(r) = self.renderer.as_mut() {
            r.egui_state
                .handle_platform_output(window, output.platform_output.clone());
        }
        output
    }
}
