//! Windowed asset viewer.
//!
//! The first milestone with pixels. It exists to prove three things are wired
//! together in one process: the archive layer finds a file, the BLP layer
//! decodes it, and the GPU displays it -- specifically via the compressed
//! upload path, which until now was only a design claim.
//!
//! It can also run headless (`--screenshot`), rendering one frame to a PNG
//! without opening a window. That keeps the render path checkable from a
//! terminal and in CI.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;
use mpq::Chain;
use render::{capture::Offscreen, texture::upload_blp, Blitter, Gpu, UploadedTexture};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// Shown on startup so the window is never empty, and a reliable presence in
/// every locale of a stock install.
const DEFAULT_TEXTURE: &str = r"Interface\Icons\Spell_Fire_Fireball02.blp";

#[derive(Parser, Clone)]
#[command(name = "wow-viewer", about = "View WoW 3.3.5a client assets")]
struct Args {
    /// Path to the installation's `Data` directory.
    #[arg(long, short, env = "WOW_DATA")]
    data: PathBuf,

    #[arg(long, default_value = "enUS")]
    locale: String,

    /// Archive path of the texture to show.
    #[arg(long, default_value = DEFAULT_TEXTURE)]
    texture: String,

    /// Render one frame to this PNG and exit, without opening a window.
    #[arg(long)]
    screenshot: Option<PathBuf>,

    #[arg(long, default_value_t = 1280)]
    width: u32,

    #[arg(long, default_value_t = 720)]
    height: u32,
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

    let paths = blp_paths(&mut chain);
    tracing::info!("{} textures available", paths.len());

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(args, chain, paths);
    event_loop.run_app(&mut app)?;
    Ok(())
}

fn blp_paths(chain: &mut Chain) -> Vec<String> {
    chain
        .list()
        .unwrap_or_default()
        .into_iter()
        .filter(|n| n.to_lowercase().ends_with(".blp"))
        .collect()
}

/// Loads and uploads one texture, reporting what happened for the overlay.
fn load_texture(gpu: &Gpu, chain: &mut Chain, path: &str) -> Result<UploadedTexture> {
    let bytes = chain.read(path)?;
    let parsed = blp::Blp::parse(&bytes)?;
    Ok(upload_blp(gpu, &parsed, path))
}

// ---------------------------------------------------------------- headless

fn screenshot(args: &Args, chain: &mut Chain, out: &std::path::Path) -> Result<()> {
    let gpu = Gpu::block(None)?;
    tracing::info!("adapter: {}", gpu.describe());

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let target = Offscreen::new(&gpu, args.width, args.height, format);
    let blitter = Blitter::new(&gpu, format);
    let texture = load_texture(&gpu, chain, &args.texture)?;

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("screenshot"),
        });
    blitter.draw(
        &gpu,
        &mut encoder,
        &target.view,
        (target.width, target.height),
        &texture.view,
        (texture.width, texture.height),
    );
    gpu.queue.submit([encoder.finish()]);

    let rgba = target.read_rgba(&gpu)?;
    write_png(out, &rgba, target.width, target.height)?;

    println!(
        "{}\n  {}x{} {:?}, {} mip levels, {}\n  rendered {}x{} -> {}",
        args.texture,
        texture.width,
        texture.height,
        texture.format,
        texture.mip_levels,
        match texture.fallback_reason {
            None => "uploaded compressed".to_string(),
            Some(r) => format!("CPU-decoded ({r})"),
        },
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
    blitter: Blitter,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    texture: Option<UploadedTexture>,
}

struct App {
    args: Args,
    chain: Chain,
    paths: Vec<String>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    /// Current texture path, and any error from loading it.
    current: String,
    error: Option<String>,
    filter: String,
    matches: Vec<String>,
    last_frame: Instant,
    frame_ms: f32,
}

impl App {
    fn new(args: Args, chain: Chain, paths: Vec<String>) -> Self {
        let current = args.texture.clone();
        let mut app = Self {
            args,
            chain,
            paths,
            window: None,
            renderer: None,
            current,
            error: None,
            filter: "icons\\spell_fire".into(),
            matches: Vec::new(),
            last_frame: Instant::now(),
            frame_ms: 0.0,
        };
        app.refilter();
        app
    }

    /// Recomputes the picker list. Done on edit rather than per frame -- the
    /// candidate set is over 100k paths.
    fn refilter(&mut self) {
        let needle = self.filter.to_lowercase();
        self.matches = self
            .paths
            .iter()
            .filter(|p| p.to_lowercase().contains(&needle))
            .take(200)
            .cloned()
            .collect();
    }

    fn select(&mut self, path: String) {
        let Some(r) = self.renderer.as_mut() else {
            return;
        };
        match load_texture(&r.gpu, &mut self.chain, &path) {
            Ok(tex) => {
                r.texture = Some(tex);
                self.current = path;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{path}: {e}")),
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
        // Prefer an sRGB target so the sRGB textures land on screen correctly.
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
        self.renderer = Some(Renderer {
            gpu,
            surface,
            config,
            blitter,
            egui_ctx,
            egui_state,
            egui_renderer,
            texture: None,
        });
        self.window = Some(window);

        let initial = self.current.clone();
        self.select(initial);
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

        // egui gets first refusal on input so the picker is usable.
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
                window.request_redraw();
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

        // Build the UI first: it needs &mut self, and the renderer borrow below
        // would otherwise conflict.
        let (ui_output, chosen) = self.build_ui(window);
        if let Some(path) = chosen {
            self.select(path);
        }

        let Some(r) = self.renderer.as_mut() else {
            return;
        };
        use wgpu::CurrentSurfaceTexture as Acquired;
        let frame = match r.surface.get_current_texture() {
            Acquired::Success(frame) => frame,
            // Suboptimal still yields a usable frame; reconfiguring next time
            // restores the fast path without dropping this one.
            Acquired::Suboptimal(frame) => {
                r.surface.configure(&r.gpu.device, &r.config);
                frame
            }
            // Routine on resize and display changes: reconfigure, skip a frame.
            Acquired::Lost | Acquired::Outdated => {
                r.surface.configure(&r.gpu.device, &r.config);
                return;
            }
            // Nothing to draw into, or nothing worth drawing.
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

        let target_size = (r.config.width, r.config.height);
        if let Some(tex) = &r.texture {
            r.blitter.draw(
                &r.gpu,
                &mut encoder,
                &view,
                target_size,
                &tex.view,
                (tex.width, tex.height),
            );
        }

        // egui on top of the blit.
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
            size_in_pixels: [r.config.width, r.config.height],
            pixels_per_point: ui_output.pixels_per_point,
        };
        r.egui_renderer.update_buffers(
            &r.gpu.device,
            &r.gpu.queue,
            &mut encoder,
            &clipped,
            &desc,
        );
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

    /// Runs one egui frame, returning its output and any newly picked texture.
    fn build_ui(&mut self, window: &Arc<Window>) -> (egui::FullOutput, Option<String>) {
        let Some(r) = self.renderer.as_mut() else {
            return (egui::FullOutput::default(), None);
        };
        let input = r.egui_state.take_egui_input(window);
        let ctx = r.egui_ctx.clone();

        let gpu_line = r.gpu.describe();
        let bc = r.gpu.supports_bc();
        let tex_info = r.texture.as_ref().map(|t| {
            (
                t.width,
                t.height,
                format!("{:?}", t.format),
                t.mip_levels,
                t.compressed,
                t.fallback_reason,
                t.bytes_uploaded,
            )
        });

        let mut chosen = None;
        let mut filter = self.filter.clone();
        let matches = self.matches.clone();
        let (current, error, frame_ms, total) = (
            self.current.clone(),
            self.error.clone(),
            self.frame_ms,
            self.paths.len(),
        );

        let output = ctx.run_ui(input, |ctx| {
            egui::Window::new("open-wow")
                .default_width(430.0)
                .show(ctx, |ui| {
                    ui.label(egui::RichText::new(&gpu_line).strong());
                    ui.label(format!(
                        "BC texture compression: {}",
                        if bc { "yes" } else { "no (CPU decode)" }
                    ));
                    ui.label(format!("{frame_ms:.1} ms/frame"));
                    ui.separator();

                    ui.label(egui::RichText::new(&current).monospace());
                    match &tex_info {
                        Some((w, h, fmt, mips, compressed, reason, bytes)) => {
                            ui.label(format!("{w}x{h}, {mips} mip levels"));
                            ui.label(format!("GPU format: {fmt}"));
                            ui.label(if *compressed {
                                format!("uploaded compressed, {} KiB", bytes / 1024)
                            } else {
                                format!(
                                    "CPU-decoded ({}), {} KiB",
                                    reason.unwrap_or("?"),
                                    bytes / 1024
                                )
                            });
                        }
                        None => {
                            ui.label("no texture loaded");
                        }
                    }
                    if let Some(e) = &error {
                        ui.colored_label(egui::Color32::from_rgb(220, 120, 120), e);
                    }

                    ui.separator();
                    ui.label(format!("{total} textures in the archives"));
                    ui.horizontal(|ui| {
                        ui.label("filter:");
                        ui.text_edit_singleline(&mut filter);
                    });
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .show(ui, |ui| {
                            for path in &matches {
                                if ui.selectable_label(*path == current, path).clicked() {
                                    chosen = Some(path.clone());
                                }
                            }
                            if matches.len() == 200 {
                                ui.weak("... narrow the filter to see more");
                            }
                        });
                });
        });

        if filter != self.filter {
            self.filter = filter;
            self.refilter();
        }
        if let Some(r) = self.renderer.as_mut() {
            r.egui_state
                .handle_platform_output(window, output.platform_output.clone());
        }
        (output, chosen)
    }
}
