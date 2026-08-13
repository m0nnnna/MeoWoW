//! Windowed asset viewer.
//!
//! Shows a texture or a model from a WoW 3.3.5a installation. It exists to
//! prove the layers work together in one process: the archive layer finds a
//! file, the format crates decode it, and the GPU draws it.
//!
//! It also runs headless (`--screenshot`), rendering one frame to a PNG without
//! opening a window, which keeps the render path checkable from a terminal and
//! in CI.

mod character;
mod hud;
mod live;
mod model;
mod scene;
mod spells;
mod spelltext;
mod terrain;
mod world;
mod world_object;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use model::{LoadedModel, Variations};
use mpq::Chain;
use render::camera::{Camera, Fly, Orbit};
use render::capture::Offscreen;
use render::mesh::{BoneBuffer, DepthBuffer, MeshRenderer};
use render::TerrainRenderer;
use render::{texture::upload_blp, Blitter, Gpu, UploadedTexture};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{KeyCode, PhysicalKey};
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

    /// Archive path of a world object's root `.wmo`.
    #[arg(long)]
    wmo: Option<String>,

    /// Draw only this WMO group, for isolating geometry.
    #[arg(long)]
    wmo_group: Option<usize>,

    /// Map directory to load terrain from, e.g. `Azeroth`.
    #[arg(long)]
    map: Option<String>,

    /// Terrain tile coordinates within the map, as `x,y`.
    #[arg(long, default_value = "32,48")]
    tile: String,

    /// Load the tile's buildings and doodads too, not just its terrain.
    #[arg(long)]
    world: bool,

    /// Stream tiles in and out as the camera moves, instead of loading a fixed
    /// block once.
    #[arg(long)]
    stream: bool,

    /// Radius in tiles around the chosen one: 0 is a single tile, 1 a 3x3.
    #[arg(long, default_value_t = 0)]
    radius: usize,

    /// Cap on doodad placements, since a dense tile has hundreds.
    #[arg(long, default_value_t = 4000)]
    max_doodads: usize,

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

    /// Orbit the scene instead of flying through it. Worlds default to flying.
    #[arg(long)]
    orbit: bool,

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

    /// Log in to this logon server and stand where the character is standing.
    ///
    /// Implies `--stream`, and picks the map from the server rather than
    /// `--map`: the point is that the world decides where you are.
    #[arg(long)]
    realm_host: Option<String>,

    #[arg(long, default_value_t = auth::client::DEFAULT_PORT)]
    realm_port: u16,

    #[arg(long)]
    user: Option<String>,

    /// Prefer `WOW_PASSWORD` so it stays out of shell history.
    #[arg(long, env = "WOW_PASSWORD", hide_env_values = true)]
    password: Option<String>,

    /// Realm to enter. Defaults to the first one offered.
    #[arg(long)]
    realm: Option<String>,

    /// Character to enter the world as.
    #[arg(long)]
    character: Option<String>,

    /// Draw the creatures and players the server reported around us.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    entities: bool,
}

/// What the viewer is currently showing.
enum Scene {
    Texture(Box<UploadedTexture>),
    Model(Box<LoadedModel>),
    WorldObject(Box<world_object::LoadedWmo>),
    Terrain(Box<terrain::LoadedTerrain>),
    World(Box<scene::WorldScene>),
    /// A streaming world; its contents change as the camera moves.
    Streaming(Box<world::World>),
}

impl Scene {
    /// Geometry shared by the model and world-object paths, so both draw
    /// through the same code.
    fn geometry(&self) -> Option<(&render::mesh::GpuMesh, &[model::Draw])> {
        match self {
            Scene::Model(m) => Some((&m.mesh, &m.draws)),
            Scene::WorldObject(w) => Some((&w.mesh, &w.draws)),
            Scene::Texture(_) | Scene::World(_) | Scene::Terrain(_)
            | Scene::Streaming(_) => None,
        }
    }

    fn textures(&self) -> &[UploadedTexture] {
        match self {
            Scene::Model(m) => &m.textures,
            Scene::WorldObject(w) => &w.textures,
            Scene::Texture(_) | Scene::World(_) | Scene::Terrain(_)
            | Scene::Streaming(_) => &[],
        }
    }

    fn bounds(&self) -> Option<(glam::Vec3, glam::Vec3)> {
        match self {
            Scene::Model(m) => Some((m.min, m.max)),
            Scene::WorldObject(w) => Some((w.min, w.max)),
            Scene::Terrain(t) => Some((t.min, t.max)),
            Scene::World(w) => Some((w.min, w.max)),
            // A streaming world has no fixed extent; the camera is placed
            // explicitly instead of framed.
            Scene::Texture(_) | Scene::Streaming(_) => None,
        }
    }
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
///
/// The second return value is present only when the world was entered over the
/// network; it says where the character is standing and what is around it.
/// Kept beside the scene rather than inside it because it describes how the
/// scene was chosen, not what is in it.
fn build_scene(
    gpu: &Gpu,
    terrain_renderer: &TerrainRenderer,
    meshes: &mut MeshRenderer,
    chain: &mut Chain,
    args: &Args,
) -> Result<(Scene, Option<live::LiveWorld>)> {
    if args.realm_host.is_some() {
        return build_live_scene(gpu, meshes, chain, args);
    }
    build_offline_scene(gpu, terrain_renderer, chain, args).map(|scene| (scene, None))
}

/// Logs in and builds a streaming world around wherever the character is.
fn build_live_scene(
    gpu: &Gpu,
    meshes: &mut MeshRenderer,
    chain: &mut Chain,
    args: &Args,
) -> Result<(Scene, Option<live::LiveWorld>)> {
    let host = args.realm_host.as_deref().expect("checked by the caller");
    let login = live::Login {
        host,
        port: args.realm_port,
        user: args.user.as_deref().context("--realm-host needs --user")?,
        password: args
            .password
            .as_deref()
            .context("--realm-host needs --password or WOW_PASSWORD")?,
        realm: args.realm.as_deref(),
        character: args
            .character
            .as_deref()
            .context("--realm-host needs --character")?,
        locale: &args.locale,
    };

    let live = live::connect(chain, &login)?;
    let mut world = world::World::new(
        chain,
        &live.map_directory,
        args.radius as i32,
        args.max_doodads,
    )?;

    if args.entities {
        let placements: Vec<world::EntityPlacement> =
            drawable_with_own(&live, 0.0)
                .iter()
                .map(|entity| world::EntityPlacement {
                    display_id: entity.display_id,
                    position: entity.position,
                    orientation: entity.orientation,
                    scale: entity.scale,
                    speed: entity.speed,
                    // Only our own body is dressed; everything else takes its
                    // appearance from its display id.
                    look: (entity.guid == live.guid).then(|| live.look.clone()),
                    look_key: if entity.guid == live.guid { live.look_key } else { 0 },
                })
                .collect();
        let undrawable = world.set_entities(gpu, meshes, chain, &placements);
        if undrawable > 0 {
            tracing::warn!("{undrawable} object(s) had no loadable model");
        }
        // Pose them once, here, because this path renders a single frame and
        // never reaches the loop that does it every frame. Without this every
        // skinned entity draws against a bone palette nothing has written --
        // and a headless screenshot of a populated zone came back with no
        // creatures in it at all, which is how this was found. See
        // `MeshRenderer::create_bones`.
        world.update_animations(gpu, meshes);
    }

    Ok((Scene::Streaming(Box::new(world)), Some(live)))
}

fn build_offline_scene(
    gpu: &Gpu,
    terrain_renderer: &TerrainRenderer,
    chain: &mut Chain,
    args: &Args,
) -> Result<Scene> {
    if let Some(display_id) = args.creature {
        let (path, variations) = model::creature(chain, display_id)?;
        let loaded = model::load(gpu, chain, &path, &variations, args.lod)?;
        return Ok(Scene::Model(Box::new(loaded)));
    }
    if let Some(path) = &args.model {
        let loaded = model::load(gpu, chain, path, &Variations::default(), args.lod)?;
        return Ok(Scene::Model(Box::new(loaded)));
    }
    if let Some(path) = &args.wmo {
        let loaded = world_object::load(gpu, chain, path, args.wmo_group)?;
        return Ok(Scene::WorldObject(Box::new(loaded)));
    }
    if let Some(map) = &args.map {
        let tile = parse_tile(&args.tile)?;
        if args.stream {
            let world = world::World::new(chain, map, args.radius as i32, args.max_doodads)?;
            return Ok(Scene::Streaming(Box::new(world)));
        }
        if args.world {
            let loaded = scene::load(
                gpu,
                terrain_renderer,
                chain,
                map,
                tile,
                args.radius,
                args.max_doodads,
            )?;
            return Ok(Scene::World(Box::new(loaded)));
        }
        let loaded = terrain::load(gpu, terrain_renderer, chain, map, tile.0, tile.1)?;
        return Ok(Scene::Terrain(Box::new(loaded)));
    }
    let path = args.texture.as_deref().unwrap_or(DEFAULT_TEXTURE);
    let parsed = blp::Blp::parse(&chain.read(path)?)?;
    Ok(Scene::Texture(Box::new(upload_blp(gpu, &parsed, path))))
}

/// Bone palette size for scenes drawn in the bind pose.
///
/// Sized for the largest skeleton rather than for one matrix, and the
/// difference is not cosmetic. A vertex whose bone index runs past the end of
/// the palette does not fail loudly: the storage read returns zero, the skinned
/// position collapses to the origin, and the model **silently disappears**.
///
/// That is precisely how it was found. Doodads kept rendering while every
/// creature vanished, because a tree indexes bone 0 and a character model
/// indexes sixty. The scene looked like the entities had never been placed at
/// all, which sent the search to the protocol rather than to the palette.
const BIND_POSE_BONES: usize = 512;

/// A palette of identity matrices, leaving every model in its bind pose.
fn bind_pose(count: usize) -> Vec<[[f32; 4]; 4]> {
    vec![glam::Mat4::IDENTITY.to_cols_array_2d(); count]
}

/// Which direction, if any, is currently being reported to the world server.
///
/// Tracked separately from the held keys so a transition -- key pressed,
/// released, or swapped -- can be told apart from "still moving the same way",
/// which is what decides whether a `MoveStart*`/`MoveStop` is due or just
/// another heartbeat.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LiveMove {
    Forward,
    Backward,
}

/// A tenth of a second between heartbeats, matching `Connection::walk`'s
/// cadence -- roughly what a real client sends while moving.
const LIVE_HEARTBEAT_EVERY: Duration = Duration::from_millis(100);

/// Units per second on foot. Not tunable from the command line: it is a
/// property of the character, not the viewer.
///
/// 7.0 is the *run* speed -- 3.3.5a walks at 2.5, and walking is a toggle
/// nothing here sends -- which is why the character's own body draws with the
/// run cycle. See `crate::world::Motion`.
const LIVE_RUN_SPEED: f32 = 7.0;

/// Radians per second turned by the A/D keys. Not verified against a
/// reference client -- see the facing note in `docs/RENDERING.md` -- but close
/// enough that the character does not spin wildly or crawl.
const LIVE_TURN_RATE: f32 = std::f32::consts::PI;

/// Which movement keys are currently held.
#[derive(Default, Clone, Copy)]
struct KeyState {
    forward: bool,
    back: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    fast: bool,
}

impl KeyState {
    fn set(&mut self, code: KeyCode, pressed: bool) -> bool {
        let slot = match code {
            KeyCode::KeyW | KeyCode::ArrowUp => &mut self.forward,
            KeyCode::KeyS | KeyCode::ArrowDown => &mut self.back,
            KeyCode::KeyA | KeyCode::ArrowLeft => &mut self.left,
            KeyCode::KeyD | KeyCode::ArrowRight => &mut self.right,
            KeyCode::Space | KeyCode::KeyE => &mut self.up,
            KeyCode::KeyQ | KeyCode::ControlLeft => &mut self.down,
            KeyCode::ShiftLeft | KeyCode::ShiftRight => &mut self.fast,
            _ => return false,
        };
        *slot = pressed;
        true
    }

    /// Movement in camera-local axes: x right, y forward, z world up.
    fn direction(&self) -> glam::Vec3 {
        let axis = |positive, negative| match (positive, negative) {
            (true, false) => 1.0,
            (false, true) => -1.0,
            _ => 0.0,
        };
        glam::Vec3::new(
            axis(self.right, self.left),
            axis(self.forward, self.back),
            axis(self.up, self.down),
        )
    }
}

/// Parses an `x,y` tile coordinate.
fn parse_tile(spec: &str) -> Result<(usize, usize)> {
    let (x, y) = spec
        .split_once(',')
        .with_context(|| format!("expected `x,y`, got {spec:?}"))?;
    Ok((x.trim().parse()?, y.trim().parse()?))
}

/// Picks a camera for the scene.
///
/// Worlds fly by default and single assets orbit: orbiting a nine-tile block
/// means circling something two kilometres wide, which is useless for looking
/// at anything in it.
fn initial_camera(scene: &Scene, args: &Args) -> Camera {
    let mut orbit = match scene.bounds() {
        Some((min, max)) => Orbit::frame(min, max),
        None => Orbit::default(),
    };
    // Apply the requested bearing to the orbit first, then convert. That way
    // `--yaw`/`--pitch` mean the same thing in both modes: a direction to view
    // the scene *from*, not a direction to stare in from wherever we happen to
    // be standing.
    if let Some(yaw) = args.yaw {
        orbit.yaw = yaw.to_radians();
    }
    if let Some(pitch) = args.pitch {
        orbit.pitch = pitch.to_radians();
    }

    if matches!(scene, Scene::World(_)) && !args.orbit {
        Camera::Fly(Fly::from_orbit(&orbit))
    } else {
        Camera::Orbit(orbit)
    }
}

/// Puts the camera where the character is standing.
///
/// Behind and above the character's own position rather than exactly on it: at
/// eye height inside the body the view is filled by the inside of the player's
/// own mesh, and nothing looks like it worked.
fn live_camera(live: &live::LiveWorld, args: &Args) -> Camera {
    const BEHIND: f32 = 9.0;
    const ABOVE: f32 = 4.0;

    let yaw = args
        .yaw
        .map(f32::to_radians)
        .unwrap_or(live.orientation);
    let mut fly = Fly {
        position: live.position
            - glam::Vec3::new(yaw.cos(), yaw.sin(), 0.0) * BEHIND
            + glam::Vec3::Z * ABOVE,
        yaw,
        pitch: -0.15,
        // Walking pace rather than the flying speed a survey wants: the point
        // here is to stand somewhere, not to cross a continent.
        speed: 30.0,
        ..Default::default()
    };
    if let Some(pitch) = args.pitch {
        fly.pitch = pitch.to_radians();
    }
    Camera::Fly(fly)
}

/// Places the camera over a streaming world's starting tile.
fn streaming_camera(world: &world::World, chain: &mut Chain, args: &Args) -> Result<Camera> {
    let tile = parse_tile(&args.tile)?;
    let mut fly = Fly {
        position: world.spawn_above(chain, (tile.0 as i32, tile.1 as i32)),
        pitch: -0.45,
        speed: 120.0,
        ..Default::default()
    };
    if let Some(yaw) = args.yaw {
        fly.yaw = yaw.to_radians();
    }
    if let Some(pitch) = args.pitch {
        fly.pitch = pitch.to_radians();
    }
    Ok(Camera::Fly(fly))
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
    camera: &Camera,
    blitter: &Blitter,
    meshes: &MeshRenderer,
    terrain_renderer: &TerrainRenderer,
    material_binds: &[wgpu::BindGroup],
    bones: Option<&BoneBuffer>,
    world_binds: &[Vec<wgpu::BindGroup>],
    identity: &render::mesh::InstanceBuffer,
) {
    // Terrain has its own pipeline, so both the tile and world scenes route
    // their landscape through here.
    let terrain_parts: &[terrain::LoadedTerrain] = match scene {
        Scene::Terrain(t) => std::slice::from_ref(t.as_ref()),
        Scene::World(w) => &w.terrain,
        _ => &[],
    };

    if let Scene::Streaming(world) = scene {
        draw_streaming(
            gpu,
            encoder,
            color,
            depth,
            size,
            world,
            camera,
            meshes,
            terrain_renderer,
            bones,
        );
        return;
    }
    // A world holds many meshes, so it cannot go through the single-mesh path.
    if !terrain_parts.is_empty() || matches!(scene, Scene::World(_)) {
        {
            let aspect = size.0 as f32 / size.1.max(1) as f32;
            meshes.update_camera(gpu, &camera.uniform(aspect));
            let empty: Vec<scene::Placed> = Vec::new();
            let (items, instances) = match scene {
                Scene::World(w) => (&w.items, Some(&w.instances)),
                _ => (&empty, None),
            };

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.42,
                            g: 0.55,
                            b: 0.70,
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

            // Landscape first: it is opaque and fills most of the frame, so
            // drawing it before the objects standing on it rejects the most
            // fragments.
            pass.set_pipeline(terrain_renderer.pipeline());
            pass.set_bind_group(0, meshes.camera_bind_group(), &[]);
            for part in terrain_parts {
                pass.set_vertex_buffer(0, part.mesh.vertices.slice(..));
                pass.set_index_buffer(part.mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                for chunk in &part.chunks {
                    pass.set_bind_group(1, &chunk.bind_group, &[]);
                    pass.draw_indexed(
                        chunk.first_index..chunk.first_index + chunk.index_count,
                        0,
                        0..1,
                    );
                }
            }

            pass.set_bind_group(0, meshes.camera_bind_group(), &[]);
            if let Some(bones) = bones {
                pass.set_bind_group(2, &bones.bind_group, &[]);
            }
            if let Some(instances) = instances {
                pass.set_vertex_buffer(1, instances.buffer.slice(..));
            }

            for (item, binds) in items.iter().zip(world_binds) {
                pass.set_vertex_buffer(0, item.mesh.vertices.slice(..));
                pass.set_index_buffer(item.mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                let instances =
                    item.instance_start..item.instance_start + item.instance_count;
                for draw in &item.draws {
                    let (Some(pipeline), Some(bind)) =
                        (meshes.get(draw.state), binds.get(draw.texture))
                    else {
                        continue;
                    };
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(1, bind, &[]);
                    pass.draw_indexed(
                        draw.first_index..draw.first_index + draw.index_count,
                        0,
                        instances.clone(),
                    );
                }
            }
        }
        return;
    }

    match scene.geometry() {
        None => {
            if let Scene::Texture(tex) = scene {
                blitter.draw(gpu, encoder, color, size, &tex.view, (tex.width, tex.height));
            }
        }
        Some((mesh, draw_list)) => {
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
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            // Single-asset scenes draw at the origin.
            pass.set_vertex_buffer(1, identity.buffer.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);

            for draw in draw_list {
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

/// One bind-group list per scene item, since each carries its own textures.
fn world_bind_groups(
    gpu: &Gpu,
    meshes: &MeshRenderer,
    scene: &Scene,
) -> Vec<Vec<wgpu::BindGroup>> {
    match scene {
        Scene::World(w) => w
            .items
            .iter()
            .map(|item| {
                item.textures
                    .iter()
                    .map(|t| meshes.material_bind_group(gpu, &t.view))
                    .collect()
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Every render state a scene needs, including multi-mesh worlds.
fn scene_states(scene: &Scene) -> Vec<render::mesh::RenderState> {
    match scene {
        Scene::World(w) => w
            .items
            .iter()
            .flat_map(|i| i.draws.iter().map(|d| d.state))
            .collect(),
        other => other
            .geometry()
            .map(|(_, draws)| draws.iter().map(|d| d.state).collect())
            .unwrap_or_default(),
    }
}

/// Draws a streaming world: terrain first, then the instanced objects on it.
#[allow(clippy::too_many_arguments)]
fn draw_streaming(
    gpu: &Gpu,
    encoder: &mut wgpu::CommandEncoder,
    color: &wgpu::TextureView,
    depth: &wgpu::TextureView,
    size: (u32, u32),
    world: &world::World,
    camera: &Camera,
    meshes: &MeshRenderer,
    terrain_renderer: &TerrainRenderer,
    bones: Option<&BoneBuffer>,
) {
    let aspect = size.0 as f32 / size.1.max(1) as f32;
    meshes.update_camera(gpu, &camera.uniform(aspect));

    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("streaming world"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: color,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: 0.42,
                    g: 0.55,
                    b: 0.70,
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
    pass.set_pipeline(terrain_renderer.pipeline());
    for tile in world.tiles() {
        pass.set_vertex_buffer(0, tile.terrain.mesh.vertices.slice(..));
        pass.set_index_buffer(tile.terrain.mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        for chunk in &tile.terrain.chunks {
            pass.set_bind_group(1, &chunk.bind_group, &[]);
            pass.draw_indexed(
                chunk.first_index..chunk.first_index + chunk.index_count,
                0,
                0..1,
            );
        }
    }

    // The map's own geometry and the server's objects draw identically; only
    // where the transforms came from, and which bone buffer they bind,
    // differs. Everything is rigid except a replicated entity group with an
    // animation to play -- see `world::Group::animation` -- so bind group 2
    // is chosen fresh per group instead of once for the whole pass.
    for group in world.tiles().flat_map(|t| t.groups.iter()).chain(world.entities()) {
        {
            let group_bones = group
                .animation
                .and_then(|key| world.entity_bone_buffer(key))
                .or(bones);
            if let Some(group_bones) = group_bones {
                pass.set_bind_group(2, &group_bones.bind_group, &[]);
            }
            pass.set_vertex_buffer(0, group.model.mesh.vertices.slice(..));
            pass.set_vertex_buffer(1, group.instances.buffer.slice(..));
            pass.set_index_buffer(
                group.model.mesh.indices.slice(..),
                wgpu::IndexFormat::Uint32,
            );
            for draw in &group.model.draws {
                let (Some(pipeline), Some(bind)) =
                    (meshes.get(draw.state), group.model.binds.get(draw.texture))
                else {
                    continue;
                };
                pass.set_pipeline(pipeline);
                pass.set_bind_group(1, bind, &[]);
                pass.draw_indexed(
                    draw.first_index..draw.first_index + draw.index_count,
                    0,
                    0..group.count,
                );
            }
        }
    }
}

fn material_bind_groups(
    gpu: &Gpu,
    meshes: &MeshRenderer,
    scene: &Scene,
) -> Vec<wgpu::BindGroup> {
    scene
        .textures()
        .iter()
        .map(|t| meshes.material_bind_group(gpu, &t.view))
        .collect()
}

/// Describes a connected world above whatever the scene itself reports.
///
/// Worth its own block rather than a line: when the camera is somewhere
/// unexpected the first question is always whether the *server* put it there or
/// the renderer did, and that is answerable only if the position the protocol
/// reported is on screen next to the tile the renderer chose.
fn describe_live(live: &live::LiveWorld) -> String {
    let tile = world::tile_at(live.position);
    let entities = live::drawable_entities(&live.state, live.guid);
    let mut text = format!(
        "{} on {} (map {}, {})\nat {:.1}, {:.1}, {:.1} facing {:.2} rad\n\
         tile {},{}\n{} objects drawable",
        live.character,
        live.map_name,
        live.map_id,
        live.map_directory,
        live.position.x,
        live.position.y,
        live.position.z,
        live.orientation,
        tile.0,
        tile.1,
        entities.len(),
    );

    let mut by_kind: std::collections::BTreeMap<&str, usize> = Default::default();
    for entity in &entities {
        *by_kind.entry(entity.kind.name()).or_default() += 1;
    }
    if !by_kind.is_empty() {
        let parts: Vec<String> = by_kind
            .iter()
            .map(|(kind, count)| format!("{count} {kind}"))
            .collect();
        text.push_str(&format!("\n  {}", parts.join(", ")));
    }

    // The nearest thing is what the camera is most likely pointed at, so it is
    // the one worth naming when checking that positions landed correctly.
    if let Some(nearest) = entities.iter().min_by(|a, b| {
        let (a, b) = (
            a.position.distance_squared(live.position),
            b.position.distance_squared(live.position),
        );
        a.total_cmp(&b)
    }) {
        text.push_str(&format!(
            "\nnearest {} guid {:#x} display {}{} at {:.0} units",
            nearest.kind.name(),
            nearest.guid,
            nearest.display_id,
            nearest
                .level
                .map(|l| format!(" level {l}"))
                .unwrap_or_default(),
            nearest.position.distance(live.position),
        ));
    }

    // Replication's own health check: the invariant this project's docs call
    // out is `created - removed == held`, and orphaned/failed counts should
    // stay at zero. Showing them here means a replication bug shows up as a
    // number that does not add up long before the world looks wrong.
    let stats = live.state.stats();
    text.push_str(&format!(
        "\n{} replicated ({} created, {} removed, {} moves, {} orphaned)",
        live.state.len(),
        stats.created,
        stats.removed,
        stats.movement_updates,
        stats.orphaned,
    ));
    if live.fold_failures > 0 {
        text.push_str(&format!(
            "\n{} update packet(s) failed to parse",
            live.fold_failures
        ));
    }
    text
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
        Scene::World(w) => {
            let mut s = format!(
                "{}
{} tiles, {} terrain triangles
{} buildings and {} doodads from {} unique                  models
{} draw calls",
                w.label,
                w.tiles_loaded,
                w.terrain_triangles,
                w.object_instances,
                w.doodad_instances,
                w.unique_models,
                w.draw_calls
            );
            if !w.skipped.is_empty() {
                s.push_str(&format!("
{} models could not load", w.skipped.len()));
            }
            s
        }
        Scene::Streaming(w) => {
            let s = w.stats;
            format!(
                "streaming\n{} tiles resident, {} queued, {} failed\n\
                 {} models cached, {} instances ({} server-placed)\n{} draw calls",
                s.tiles_resident,
                s.tiles_pending,
                s.tiles_failed,
                s.models_cached,
                s.instances,
                s.entities,
                s.draw_calls
            )
        }
        Scene::Terrain(t) => {
            let mut s = format!(
                "{}\n{} vertices, {} triangles\n{} chunks, {} draw calls, {} textures\n\
                 {} doodad and {} world object placements",
                t.path,
                t.vertex_count,
                t.triangle_count,
                t.chunk_count,
                t.chunks.len(),
                t.textures.len(),
                t.doodad_placements,
                t.object_placements
            );
            if t.holes > 0 {
                s.push_str(&format!("\n{} terrain holes", t.holes));
            }
            if !t.missing_textures.is_empty() {
                s.push_str(&format!("\nunresolved: {}", t.missing_textures.join(", ")));
            }
            s
        }
        Scene::WorldObject(w) => {
            let mut s = format!(
                "{}\n{} vertices, {} triangles\n{} groups, {} draw calls, {} textures",
                w.path,
                w.vertex_count,
                w.triangle_count,
                w.group_count,
                w.draws.len(),
                w.textures.len()
            );
            if w.collision_triangles > 0 {
                s.push_str(&format!(
                    "\n{} collision-only triangles (not drawn)",
                    w.collision_triangles
                ));
            }
            if !w.doodad_sets.is_empty() {
                s.push_str(&format!("\ndoodad sets: {}", w.doodad_sets.join(", ")));
            }
            if !w.missing_textures.is_empty() {
                s.push_str(&format!("\nunresolved: {}", w.missing_textures.join(", ")));
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
    let terrain_renderer = TerrainRenderer::new(&gpu, format, meshes.camera_layout());

    let (mut scene, live) = build_scene(&gpu, &terrain_renderer, &mut meshes, chain, args)?;
    let camera = match (&scene, &live) {
        (_, Some(live)) => live_camera(live, args),
        (Scene::Streaming(w), None) => streaming_camera(w, chain, args)?,
        _ => initial_camera(&scene, args),
    };
    if let Scene::Streaming(world) = &mut scene {
        // Headless renders one frame, so fill the resident set up front rather
        // than a couple of tiles at a time.
        let eye = match camera {
            Camera::Fly(f) => f.position,
            Camera::Orbit(o) => o.eye(),
        };
        for _ in 0..64 {
            world.update(&gpu, &mut meshes, &terrain_renderer, chain, eye);
            if world.stats.tiles_pending == 0 {
                break;
            }
        }
    }
    // Any scene with geometry needs its pipelines built and a bone palette
    // bound -- rigid world objects included, where the palette is identity.
    meshes.prepare(&gpu, scene_states(&scene));
    let bone_count = match &scene {
        Scene::Model(m) => m.bones.len(),
        _ => BIND_POSE_BONES,
    };
    let bone_buffer = meshes.create_bones(&gpu, bone_count);
    match &scene {
        Scene::Model(m) => upload_pose(&gpu, &meshes, &bone_buffer, m, args.anim, args.anim_time),
        _ => meshes.update_bones(&gpu, &bone_buffer, &bind_pose(bone_count)),
    }
    let bones = Some(bone_buffer);
    let binds = material_bind_groups(&gpu, &meshes, &scene);
    let world_binds = world_bind_groups(&gpu, &meshes, &scene);
    let identity = render::mesh::InstanceBuffer::upload(&gpu, &[render::mesh::Instance::IDENTITY]);

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
        &terrain_renderer,
        &binds,
        bones.as_ref(),
        &world_binds,
        &identity,
    );
    gpu.queue.submit([encoder.finish()]);

    let rgba = target.read_rgba(&gpu)?;
    write_png(out, &rgba, target.width, target.height)?;
    if let Some(live) = &live {
        println!("{}", describe_live(live));
    }
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
    terrain_renderer: TerrainRenderer,
    material_binds: Vec<wgpu::BindGroup>,
    world_binds: Vec<Vec<wgpu::BindGroup>>,
    identity: render::mesh::InstanceBuffer,
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
    camera: Camera,
    dragging: bool,
    last_cursor: Option<(f64, f64)>,
    error: Option<String>,
    keys: KeyState,
    last_frame: Instant,
    frame_ms: f32,
    /// Selected sequence, or `None` for the bind pose.
    anim: Option<usize>,
    /// Elapsed time within the current sequence.
    anim_time_ms: u32,
    playing: bool,
    speed: f32,
    /// Present when the world was entered over the network.
    live: Option<live::LiveWorld>,
    /// The direction currently reported to the server, if any -- `None` means
    /// the last packet sent was a `MoveStop`, or nothing has moved yet.
    live_move: Option<LiveMove>,
    last_heartbeat: Instant,
    last_ping: Instant,
    /// The `undrawable` count last logged, so entity rebuilding (which now
    /// runs every frame) warns once per change instead of every frame for the
    /// rest of the session.
    last_undrawable_warned: usize,
    /// The player's own interface: where every frame sits, what it looks like,
    /// and whether it is currently being rearranged.
    hud: ui::Hud,
    /// What this client has selected, and has told the server it has.
    target: Option<u64>,
    /// Where the left button went down, so a click can be told from the drag
    /// that turns the camera. Both arrive as the same pair of events.
    press_at: Option<(f64, f64)>,
    /// How far the camera has been swung around the character by dragging,
    /// added to the character's own facing rather than replacing it. Only
    /// meaningful while following a live character.
    camera_yaw_offset: f32,
    /// Chat scrollback, oldest first, capped at the style's limit. Owned here
    /// rather than in `world`: chat is a stream of events, and replicated
    /// state has no business growing without bound.
    ///
    /// Kept as the messages that *arrived*, not as the lines they rendered to.
    /// A line is rendered fresh every frame, so a speaker whose name query is
    /// still in flight when the line arrives gains their name the moment it
    /// answers. Rendering once at arrival stamped the guid in permanently --
    /// which is what a whisper from someone out of visibility range always
    /// looks like, since they were never in state to be asked about.
    ///
    /// Locally generated notices are stored as `System` messages from guid
    /// zero, so there is one path to render rather than two.
    ///
    /// Combat shares the scrollback so the two interleave in the order they
    /// happened, and is stored as the *swing* rather than as a finished
    /// sentence for the same reason chat is stored as the message: names
    /// resolve asynchronously, and 4.2's one real bug was a line stamped with
    /// a guid a moment before that guid's name arrived. Rendering both every
    /// frame means a name that turns up late fixes the lines already on
    /// screen.
    chat: Vec<Line>,
    /// Damage numbers currently rising and fading, oldest first. Pruned every
    /// frame once a number's age passes `1.0` -- see [`PendingCombatText`].
    combat_text: Vec<PendingCombatText>,
    /// Debug: add a half turn to every entity's facing, toggled with F2.
    ///
    /// Here because two observations disagree and a live A/B settles it in
    /// seconds where reasoning has already been wrong twice. A static render
    /// of the player at a server-confirmed heading says the current facing is
    /// right; watching anything actually *walk* says it is backwards. Rather
    /// than flip a constant on a guess for the third time, this lets the
    /// person at the window compare them directly.
    entity_flip: bool,
    /// Debug: draw M2 geometry with the opposite winding, toggled with F3.
    ///
    /// "The front of the pillars is missing and I can see inside them" is
    /// culling, not geometry -- and a model rendered inside-out can also read
    /// as one facing the wrong way, which is very likely why the two
    /// observations above disagree.
    flip_winding: bool,
    /// The line being typed, or `None` when not typing. While this is `Some`,
    /// keys are text rather than movement.
    composing: Option<String>,
    /// Spell names and icons, loaded from the archives if there are any.
    spells: spells::Spellbook,
    /// Whether the bars have been filled from the spellbook yet. The spellbook
    /// arrives in the login burst, so this cannot happen at construction.
    bars_seeded: bool,
    /// Held modifiers, which choose which bar a number key drives.
    modifiers: winit::keyboard::ModifiersState,
}

/// How far the pointer may travel between press and release and still count as
/// a click rather than as a look.
const CLICK_SLOP: f64 = 4.0;

/// The action-bar slot a key drives, if any.
///
/// The number row in order, `1` through `=`. Matched on the *physical* key
/// rather than the character it produces, because an action bar is muscle
/// memory about positions on the keyboard: the key left of Backspace should be
/// the twelfth slot whatever it prints.
fn action_slot(code: KeyCode) -> Option<usize> {
    Some(match code {
        KeyCode::Digit1 => 0,
        KeyCode::Digit2 => 1,
        KeyCode::Digit3 => 2,
        KeyCode::Digit4 => 3,
        KeyCode::Digit5 => 4,
        KeyCode::Digit6 => 5,
        KeyCode::Digit7 => 6,
        KeyCode::Digit8 => 7,
        KeyCode::Digit9 => 8,
        KeyCode::Digit0 => 9,
        KeyCode::Minus => 10,
        KeyCode::Equal => 11,
        _ => return None,
    })
}

/// Everything worth drawing, the player's own body included.
///
/// One function because four places need this list and they must agree: the
/// initial placement, the per-frame rebuild, and the click test all have to
/// see the same world, or a click lands on something that is not where it
/// looks. That is the same rule the action bar's slot geometry follows, and
/// the picking ray already follows by unprojecting the matrix the scene was
/// drawn with.
///
/// `speed` is the caller's own movement state rather than anything read back
/// from the server -- see [`live::own_entity`].
fn drawable_with_own(live: &live::LiveWorld, speed: f32) -> Vec<live::Entity> {
    let mut entities = live::drawable_entities(&live.state, live.guid);
    if let Some(own) = live::own_entity(
        &live.state,
        live.guid,
        live.position,
        live.orientation,
        speed,
    ) {
        entities.push(own);
    }
    entities
}

/// One entry in the scrollback, before it is turned into text.
///
/// The alternative -- rendering to a string at the moment of arrival -- is
/// what this project already got wrong once: a name that resolves a moment
/// later cannot reach a sentence that has already been built. Both arms are
/// rendered fresh every frame instead.
enum Line {
    Chat(::world::ChatMessage),
    Swing(::world::combat::MeleeSwing),
}

/// A damage number in flight, from the swing that spawned it to fully faded.
///
/// The world position is captured once, at the swing, and never re-read from
/// the entity afterwards -- see `hud::combat_text_anchor` for why: a killing
/// blow's number has to keep rising even after the corpse it came from is
/// destroyed and gone from replicated state. `extra_amount` plays no part in
/// `text` or `kind`: it is deliberately unnamed in `world::combat` because no
/// capture has confirmed what it means, and a number this client cannot
/// explain has no business on screen at all, let alone under a guessed label
/// like "blocked".
struct PendingCombatText {
    world_pos: glam::Vec3,
    text: String,
    kind: ui::CombatTextKind,
    spawned: Instant,
}

/// A line this client generated itself, shaped like one off the wire.
///
/// Sharing the wire type means the scrollback has one thing in it and one way
/// to render it, rather than an enum whose second arm is easy to forget when
/// the rendering changes.
fn local_notice(text: String) -> ::world::ChatMessage {
    ::world::ChatMessage {
        chat_type: ::world::ChatType::System,
        language: 0,
        sender: 0,
        sender_name: None,
        target: 0,
        channel: None,
        text,
        tag: 0,
    }
}

impl App {
    fn new(args: Args, chain: Chain) -> Self {
        Self {
            args,
            chain,
            window: None,
            renderer: None,
            camera: Camera::Orbit(Orbit::default()),
            keys: KeyState::default(),
            dragging: false,
            last_cursor: None,
            error: None,
            last_frame: Instant::now(),
            frame_ms: 0.0,
            anim: None,
            anim_time_ms: 0,
            playing: true,
            speed: 1.0,
            live: None,
            live_move: None,
            last_heartbeat: Instant::now(),
            last_ping: Instant::now(),
            last_undrawable_warned: 0,
            // Read before the window exists, so a layout that fails to parse
            // is reported at startup rather than at the first frame.
            hud: ui::Hud::load(),
            target: None,
            press_at: None,
            camera_yaw_offset: 0.0,
            chat: Vec::new(),
            combat_text: Vec::new(),
            entity_flip: false,
            flip_winding: false,
            composing: None,
            spells: spells::Spellbook::default(),
            bars_seeded: false,
            modifiers: Default::default(),
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
        let terrain_renderer = TerrainRenderer::new(&gpu, format, meshes.camera_layout());
        let depth = DepthBuffer::new(&gpu, config.width, config.height);

        let scene = match build_scene(
            &gpu,
            &terrain_renderer,
            &mut meshes,
            &mut self.chain,
            &self.args,
        ) {
            Ok((scene, live)) => {
                if live.is_some() {
                    // Start the movement and keepalive clocks from the moment
                    // the connection is actually ready to drive, not from
                    // whenever the window happened to be created.
                    self.last_heartbeat = Instant::now();
                    self.last_ping = Instant::now();
                    self.last_undrawable_warned = 0;
                }
                self.live = live;
                Some(scene)
            }
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                tracing::error!("{e:#}");
                None
            }
        };
        if let Some(scene) = &scene {
            self.camera = match (scene, &self.live) {
                (_, Some(live)) => live_camera(live, &self.args),
                (Scene::Streaming(w), None) => {
                    streaming_camera(w, &mut self.chain, &self.args)
                        .unwrap_or_else(|_| initial_camera(scene, &self.args))
                }
                _ => initial_camera(scene, &self.args),
            };
        }
        let mut bones = None;
        if let Some(scene) = &scene {
            meshes.prepare(&gpu, scene_states(scene));
            let bone_count = match scene {
                Scene::Model(m) => m.bones.len(),
                _ => BIND_POSE_BONES,
            };
            let buffer = meshes.create_bones(&gpu, bone_count);
            // Every model here is drawn in its bind pose, but the palette must
            // still cover the largest skeleton -- see BIND_POSE_BONES.
            meshes.update_bones(&gpu, &buffer, &bind_pose(bone_count));
            bones = Some(buffer);

            if let Scene::Model(m) = scene {
                // Start on a real animation rather than the bind pose.
                self.anim = self.args.anim.or_else(|| (!m.sequences.is_empty()).then_some(0));
            }
        }
        let world_binds = scene
            .as_ref()
            .map(|s| world_bind_groups(&gpu, &meshes, s))
            .unwrap_or_default();
        let identity =
            render::mesh::InstanceBuffer::upload(&gpu, &[render::mesh::Instance::IDENTITY]);
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
            terrain_renderer,
            material_binds,
            world_binds,
            identity,
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
                if self.dragging {
                    self.press_at = self.last_cursor;
                } else {
                    // A press and release in the same place is a click; the
                    // same two events with movement between them is the drag
                    // that turns the camera. Nothing else distinguishes them,
                    // so the distance has to be measured.
                    if let (Some(press), Some(release)) = (self.press_at.take(), self.last_cursor) {
                        let moved =
                            ((release.0 - press.0).powi(2) + (release.1 - press.1).powi(2)).sqrt();
                        if moved <= CLICK_SLOP {
                            self.click_at(release);
                        }
                    }
                    self.last_cursor = None;
                }
            }
            WindowEvent::ModifiersChanged(state) => {
                self.modifiers = state.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                let code = match event.physical_key {
                    PhysicalKey::Code(code) => Some(code),
                    _ => None,
                };

                // While typing, the keyboard belongs to the chat line. This
                // returns rather than falling through, so W does not walk the
                // character across the zone while a message is being written.
                if self.composing.is_some() {
                    if pressed {
                        self.type_into_chat(code, event.text.as_deref());
                    }
                    window.request_redraw();
                    return;
                }

                if let Some(code) = code {
                    // Held keys repeat; a toggle must fire once per press.
                    if pressed && !event.repeat {
                        // An action key before the movement keys, so a bound
                        // number is never also a movement binding.
                        if let Some(slot) = action_slot(code) {
                            let bar = if self.modifiers.shift_key() {
                                1
                            } else if self.modifiers.control_key() {
                                2
                            } else {
                                0
                            };
                            self.activate_slot(bar, slot);
                            window.request_redraw();
                            return;
                        }
                        match code {
                            KeyCode::F1 => self.hud.toggle_edit(),
                            KeyCode::F2 => {
                                self.entity_flip = !self.entity_flip;
                                let state = if self.entity_flip { "flipped" } else { "as shipped" };
                                self.chat.push(Line::Chat(local_notice(format!(
                                    "entity facing: {state}"
                                ))));
                                tracing::info!("entity facing {state}");
                            }
                            KeyCode::F3 => {
                                self.flip_winding = !self.flip_winding;
                                let state = if self.flip_winding { "reversed" } else { "as shipped" };
                                self.chat.push(Line::Chat(local_notice(format!(
                                    "model winding: {state}"
                                ))));
                                tracing::info!("model winding {state}");
                            }
                            // Only offer a chat line when there is somewhere
                            // to send it.
                            KeyCode::Enter | KeyCode::NumpadEnter if self.live.is_some() => {
                                self.composing = Some(String::new());
                                // Keys held when chat opened would otherwise
                                // stay held forever: nothing releases them,
                                // because every later key event is swallowed
                                // above. Clearing here makes the next movement
                                // tick send a MoveStop.
                                self.keys = KeyState::default();
                                window.request_redraw();
                                return;
                            }
                            _ => {}
                        }
                    }
                    self.keys.set(code, pressed);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let now = (position.x, position.y);
                if self.dragging {
                    if let Some(prev) = self.last_cursor {
                        // Roughly half a turn across the window, which reads as
                        // direct manipulation rather than a flick.
                        const SPEED: f32 = 0.008;
                        let (dx, dy) = (
                            -(now.0 - prev.0) as f32 * SPEED,
                            (now.1 - prev.1) as f32 * SPEED,
                        );
                        // Standing in a live world, the camera's yaw is owned
                        // by `drive_live_movement`, which rebuilds it from the
                        // character's facing every frame. Writing yaw here too
                        // would be overwritten before it was ever drawn, so
                        // the drag accumulates an offset that function adds.
                        let following = self.live.is_some();
                        match &mut self.camera {
                            Camera::Orbit(c) => c.orbit(dx, dy),
                            // Dragging turns the view, so the world follows the
                            // cursor rather than moving against it.
                            Camera::Fly(c) if following => c.look(0.0, -dy),
                            Camera::Fly(c) => c.look(dx, -dy),
                        }
                        if following {
                            self.camera_yaw_offset =
                                (self.camera_yaw_offset + dx).rem_euclid(std::f32::consts::TAU);
                        }
                    }
                }
                self.last_cursor = Some(now);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 60.0,
                };
                match &mut self.camera {
                    Camera::Orbit(c) => c.zoom(0.88f32.powf(notches)),
                    // Flying has no zoom; the wheel trims travel speed instead.
                    Camera::Fly(c) => {
                        c.speed = (c.speed * 1.15f32.powf(notches)).clamp(1.0, 5000.0)
                    }
                }
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

        // Movement integrates real elapsed time, so travel speed does not
        // depend on frame rate. A live world is driven by the character's
        // walk, not a free-flying camera -- see `drive_live_movement`.
        if self.live.is_some() {
            self.drive_live_movement();
            self.pump_live_connection();
            // After the pump, because the spellbook it needs arrives through
            // it, and because both want the archive chain.
            self.load_spell_data();
        } else if let Camera::Fly(fly) = &mut self.camera {
            let direction = self.keys.direction();
            if direction != glam::Vec3::ZERO {
                fly.travel(direction, self.frame_ms / 1000.0, self.keys.fast);
            }
        }

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

        // Stream before drawing, so newly admitted tiles appear this frame.
        if let Some(Scene::Streaming(world)) = r.scene.as_mut() {
            let eye = match camera {
                Camera::Fly(f) => f.position,
                Camera::Orbit(o) => o.eye(),
            };
            world.update(
                &r.gpu,
                &mut r.meshes,
                &r.terrain_renderer,
                &mut self.chain,
                eye,
            );
            // Every frame, not throttled: see `World::update_animations` for
            // why animation and instance-position rebuilding run on different
            // clocks.
            world.update_animations(&r.gpu, &r.meshes);

            // Also every frame, despite rebuilding every instance buffer.
            // This was originally throttled (see git history for
            // `LIVE_ENTITY_REBUILD_EVERY`) on the reasoning that rebuilding
            // is wasteful -- true, but untested against what it actually
            // costs at the entity counts this project deals with (tens to
            // one or two hundred). What the throttle actually did, watched
            // live, was decouple position from the now-every-frame animation:
            // legs mid-stride while the body itself only advanced ten times a
            // second reads as a stutter, not as a frame-rate problem. If a
            // much larger population ever makes this measurably expensive,
            // that is the moment to reintroduce a budget -- ideally one that
            // updates existing instances' transforms in place rather than
            // reallocating every buffer, rather than reaching for the same
            // timer again.
            if self.args.entities {
                if let Some(live) = &self.live {
                    // The keys, not the wire: the server never relays our own
                    // movement back to us. Held means running -- there is no
                    // walk toggle here, and `LIVE_RUN_SPEED` is the run speed.
                    let speed = if self.live_move.is_some() {
                        LIVE_RUN_SPEED
                    } else {
                        0.0
                    };
                    // F2. See `App::entity_flip`.
                    let flip = if self.entity_flip { std::f32::consts::PI } else { 0.0 };
                    let placements: Vec<crate::world::EntityPlacement> =
                        drawable_with_own(live, speed)
                            .iter()
                            .map(|entity| crate::world::EntityPlacement {
                                display_id: entity.display_id,
                                position: entity.position,
                                orientation: entity.orientation + flip,
                                scale: entity.scale,
                                speed: entity.speed,
                                look: (entity.guid == live.guid)
                                    .then(|| live.look.clone()),
                                look_key: if entity.guid == live.guid {
                                    live.look_key
                                } else {
                                    0
                                },
                            })
                            .collect();
                    let undrawable =
                        world.set_entities(&r.gpu, &mut r.meshes, &mut self.chain, &placements);
                    // Warn on change, not on every rebuild: this now runs
                    // every frame, and a zone with one unloadable model would
                    // otherwise log about it forever.
                    if undrawable > 0 && undrawable != self.last_undrawable_warned {
                        tracing::warn!("{undrawable} replicated object(s) had no loadable model");
                    }
                    self.last_undrawable_warned = undrawable;
                }
            }
        }

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
                &r.terrain_renderer,
                &r.material_binds,
                r.bones.as_ref(),
                &r.world_binds,
                &r.identity,
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

    /// Turns and walks the live character from held keys, sending whatever the
    /// movement stream requires, and keeps the camera behind it.
    ///
    /// Altitude follows the terrain: the two horizontal axes come from the
    /// keys, and Z is then read back out of the height field the ground is
    /// drawn from (see [`crate::world::World::height_at`]) rather than left at
    /// whatever the server last reported. Carrying a stale Z looked like four
    /// unrelated faults -- the character sinking into rising ground, a click
    /// marker landing off-centre because the picking ray starts at the eye and
    /// the eye is a fixed offset from a wrong altitude, hills refusing to be
    /// walked up, and *another* client seeing this one twitch as the server
    /// corrected an altitude that had been wrong for a while.
    ///
    /// Jumping, falling and collision remain out of scope, and so does
    /// standing on anything that is not terrain: a bridge or an upper floor is
    /// WMO geometry, which nothing here can be asked about. See the movement
    /// section of `docs/PROTOCOL.md`.
    fn drive_live_movement(&mut self) {
        use ::world::update::movement_flags;
        use ::world::{ClientOpcode, MovementInfo, Position};

        let Some(live) = self.live.as_mut() else {
            return;
        };
        let dt = (self.frame_ms / 1000.0).max(0.0);

        // Turning is purely local: nothing here reports it to the server
        // unless a translation is also in flight, since the position sent
        // with every Start/Heartbeat carries the current orientation anyway.
        let turn = match (self.keys.left, self.keys.right) {
            (true, false) => LIVE_TURN_RATE,
            (false, true) => -LIVE_TURN_RATE,
            _ => 0.0,
        };
        if turn != 0.0 {
            live.orientation =
                (live.orientation + turn * dt).rem_euclid(std::f32::consts::TAU);
        }

        let desired = if self.keys.forward {
            Some(LiveMove::Forward)
        } else if self.keys.back {
            Some(LiveMove::Backward)
        } else {
            None
        };

        if let Some(heading) = desired {
            let (dx, dy) = (live.orientation.cos(), live.orientation.sin());
            let sign = if heading == LiveMove::Forward { 1.0 } else { -1.0 };
            live.position.x += dx * LIVE_RUN_SPEED * sign * dt;
            live.position.y += dy * LIVE_RUN_SPEED * sign * dt;
        }

        // Stand on the ground under wherever those two axes put us.
        //
        // Every frame, not only while a key is held: the tile the character is
        // standing on may only have finished streaming in this frame, and the
        // very first sample -- the one that fixes the altitude the server sent
        // at login -- would otherwise wait for the player to press something.
        //
        // Assigned rather than clamped upward. Upward alone fixes sinking into
        // a hill and does nothing for the walk back down it, leaving the
        // character hanging in the air over the valley with no way back to the
        // ground. With no jumping or falling modelled, the feet are on the
        // terrain or the position is wrong.
        //
        // `None` -- the tile is not resident yet, or this is a hole in the
        // terrain -- deliberately leaves Z alone rather than substituting
        // anything. The server's altitude is stale, but it is a real place; a
        // guess is not.
        if let Some(Scene::Streaming(world)) = self.renderer.as_ref().and_then(|r| r.scene.as_ref())
        {
            if let Some(ground) = world.height_at(live.position.x, live.position.y) {
                live.position.z = ground;
            }
        }

        let position = Position {
            x: live.position.x,
            y: live.position.y,
            z: live.position.z,
            orientation: live.orientation,
        };

        if desired != self.live_move {
            let (opcode, flags) = match desired {
                Some(LiveMove::Forward) => (ClientOpcode::MoveStartForward, movement_flags::FORWARD),
                Some(LiveMove::Backward) => {
                    (ClientOpcode::MoveStartBackward, movement_flags::BACKWARD)
                }
                // Left in the FORWARD/BACKWARD state, the character keeps
                // moving in the server's own simulation after we go quiet.
                None => (ClientOpcode::MoveStop, 0),
            };
            let info = MovementInfo {
                flags,
                time: live.connection.tick(),
                position,
                ..MovementInfo::default()
            };
            if let Err(e) = live.connection.send_movement(opcode, live.guid, &info) {
                tracing::warn!("sending movement failed: {e:#}");
            }
            self.live_move = desired;
            self.last_heartbeat = Instant::now();
        } else if let Some(heading) = desired {
            if self.last_heartbeat.elapsed() >= LIVE_HEARTBEAT_EVERY {
                let flags = if heading == LiveMove::Forward {
                    movement_flags::FORWARD
                } else {
                    movement_flags::BACKWARD
                };
                let info = MovementInfo {
                    flags,
                    time: live.connection.tick(),
                    position,
                    ..MovementInfo::default()
                };
                if let Err(e) =
                    live.connection
                        .send_movement(ClientOpcode::MoveHeartbeat, live.guid, &info)
                {
                    tracing::warn!("sending heartbeat failed: {e:#}");
                }
                self.last_heartbeat = Instant::now();
            }
        } else if turn != 0.0 && self.last_heartbeat.elapsed() >= LIVE_HEARTBEAT_EVERY {
            // Turning on the spot. Without this the server never learns the new
            // facing until the next time we translate, so anyone watching sees
            // the character pointing the wrong way -- invisible from here,
            // because our own camera follows the local orientation regardless.
            let info = MovementInfo {
                flags: 0,
                time: live.connection.tick(),
                position,
                ..MovementInfo::default()
            };
            if let Err(e) =
                live.connection
                    .send_movement(ClientOpcode::MoveSetFacing, live.guid, &info)
            {
                tracing::warn!("sending facing failed: {e:#}");
            }
            self.last_heartbeat = Instant::now();
        }

        // Same offset as the initial placement in `live_camera`, recomputed
        // every frame so the camera tracks the character instead of flying
        // free. Pitch is left alone so a mouse drag can still look up or down.
        //
        // The yaw is *not* simply the character's orientation, though it was
        // at first, and the difference matters more than it looks. Recomputing
        // it here every frame is what makes the camera follow -- but a mouse
        // drag also writes a yaw, and this overwrote it a millisecond later,
        // so dragging sideways did nothing at all while dragging up and down
        // worked. That reads as half a broken camera rather than as two things
        // writing one field. The drag now accumulates into
        // `camera_yaw_offset`, which is added here instead of competing with
        // it: the camera swings around the character, and the character keeps
        // facing wherever it was facing, which is what a left drag does in the
        // game this is modelled on.
        if let Camera::Fly(fly) = &mut self.camera {
            const BEHIND: f32 = 9.0;
            const ABOVE: f32 = 4.0;
            let yaw = live.orientation + self.camera_yaw_offset;
            fly.position = live.position
                - glam::Vec3::new(yaw.cos(), yaw.sin(), 0.0) * BEHIND
                + glam::Vec3::Z * ABOVE;
            fly.yaw = yaw;
        }
    }

    /// Handles one keypress while a chat line is being written.
    ///
    /// `text` is what the key actually produced, which is the only thing that
    /// knows about layouts, shift and dead keys -- deriving a character from
    /// the physical key code instead would type QWERTY on an AZERTY keyboard.
    fn type_into_chat(&mut self, code: Option<KeyCode>, text: Option<&str>) {
        match code {
            Some(KeyCode::Escape) => {
                self.composing = None;
                return;
            }
            Some(KeyCode::Enter) | Some(KeyCode::NumpadEnter) => {
                let line = self.composing.take().unwrap_or_default();
                let line = line.trim();
                if !line.is_empty() {
                    self.send_chat(line);
                }
                return;
            }
            Some(KeyCode::Backspace) => {
                if let Some(buffer) = self.composing.as_mut() {
                    buffer.pop();
                }
                return;
            }
            _ => {}
        }

        // Control characters arrive here as text too -- Enter as "\r", Escape
        // as "\u{1b}" -- and appending them would put an unprintable glyph in
        // the message and send it.
        let Some(text) = text else { return };
        if let Some(buffer) = self.composing.as_mut() {
            for character in text.chars().filter(|c| !c.is_control()) {
                buffer.push(character);
            }
        }
    }

    /// Sends a line of chat, and says so locally if it cannot be sent.
    fn send_chat(&mut self, line: &str) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        // The character's own language: a message sent in Universal is
        // refused with no reply at all, which reads exactly like a malformed
        // packet. See `world::chat::language_for_race`.
        let race = live
            .state
            .get(live.guid)
            .and_then(|entity| entity.race())
            .unwrap_or(1);
        let language = ::world::chat::language_for_race(race);
        if let Err(e) = live
            .connection
            .say(::world::ChatType::Say, language, "", line)
        {
            tracing::warn!("sending chat failed: {e:#}");
            // Locally, so a failure to speak is visible where speaking
            // happens rather than only in a log nobody has open.
            self.chat.push(Line::Chat(local_notice(format!("could not send: {e}"))));
        }
        // Nothing is echoed locally on success: the server relays the line
        // back like any other, and adding it here too would show it twice.
    }

    /// Casts whatever is in an action slot.
    ///
    /// A bound key fires whether or not its bar is visible. Hiding a bar is
    /// about screen space, not about unbinding it -- and a key that silently
    /// stopped working because a checkbox was unticked would be a poor
    /// surprise mid-fight.
    fn activate_slot(&mut self, bar: usize, slot: usize) {
        let Some(spell) = self.hud.profile.bars.get(bar, slot) else {
            return;
        };
        let target = self.target;
        let name = self.spells.name(spell);
        let Some(live) = self.live.as_mut() else {
            return;
        };
        match live.connection.cast_spell(spell, target) {
            Ok(()) => tracing::debug!("cast {name} ({spell})"),
            Err(e) => {
                tracing::warn!("casting {spell} failed: {e:#}");
                self.chat.push(Line::Chat(local_notice(format!("could not cast: {e}"))));
            }
        }
    }

    /// Reads names and icons for whatever the character knows, once.
    ///
    /// Separate from `pump_live_connection` rather than tucked inside it: that
    /// function holds the connection mutably for its whole body, and this
    /// needs the archive chain and the spellbook at the same time.
    fn load_spell_data(&mut self) {
        if self.bars_seeded {
            return;
        }
        let known: std::collections::HashSet<u32> = match self.live.as_ref() {
            Some(live) => live.state.spells.spells.iter().map(|s| s.id).collect(),
            None => return,
        };
        if known.is_empty() {
            // Logged once a second rather than every frame: an empty spellbook
            // here means `SMSG_INITIAL_SPELLS` was not folded in, which is a
            // different problem from one that failed to parse.
            if self.last_frame.elapsed().as_millis() % 1000 < 20 {
                tracing::debug!("no spells replicated yet");
            }
            return;
        }
        self.spells = spells::Spellbook::load(&mut self.chain, &known);
        self.seed_action_bars();
    }

    /// Fills the first bar from the character's spellbook, once.
    ///
    /// The spellbook arrives in the login burst, so this cannot happen at
    /// startup; and it only runs when the bars are *entirely* empty, so it
    /// never overwrites a layout the user arranged. What it placed is logged,
    /// because the passive filter is the part most likely to be wrong and the
    /// symptom -- a bar full of weapon skills -- is obvious in one line.
    fn seed_action_bars(&mut self) {
        if self.bars_seeded {
            return;
        }
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let known: Vec<u32> = live.state.spells.spells.iter().map(|s| s.id).collect();
        if known.is_empty() {
            return;
        }
        self.bars_seeded = true;

        if !self.hud.profile.bars.is_empty() {
            tracing::debug!("action bars already arranged; leaving them alone");
            return;
        }
        // The character's own class: the filter turns on which skill line a
        // spell belongs to, and every class has its own.
        let class = live
            .state
            .get(live.guid)
            .and_then(|entity| entity.class())
            .unwrap_or(1);
        let castable = self.spells.castable(&known, class);
        let placed: Vec<String> = castable
            .iter()
            .take(ui::frames::action_bar::SLOTS)
            .enumerate()
            .map(|(slot, spell)| {
                self.hud.profile.bars.set(0, slot, Some(*spell));
                self.spells.name(*spell)
            })
            .collect();
        tracing::info!(
            "action bar seeded from {} known spells ({} castable): {}",
            known.len(),
            castable.len(),
            placed.join(", ")
        );
    }

    /// Selects whatever is under the cursor, and tells the server.
    fn click_at(&mut self, at: (f64, f64)) {
        let Some(r) = self.renderer.as_ref() else {
            return;
        };
        // A click on the interface belongs to the interface: clicking a health
        // bar must not target whatever is standing behind it.
        if self.hud.captures_pointer(&r.egui_ctx) {
            return;
        }
        let (Some(live), Some(Scene::Streaming(world))) = (self.live.as_ref(), r.scene.as_ref())
        else {
            return;
        };

        let viewport = (r.config.width as f32, r.config.height as f32);
        let Some(ray) = self.camera.ray_through((at.0 as f32, at.1 as f32), viewport) else {
            return;
        };
        // Rebuilt rather than cached: the same interpolated positions the
        // renderer drew this frame, so a click hits where the creature looks
        // like it is rather than where it last reported being.
        // The speed only chooses an animation, which a click test does not care
        // about -- but the same list has to come out here as the renderer drew,
        // so it is passed the same way rather than left at a default.
        let speed = if self.live_move.is_some() { LIVE_RUN_SPEED } else { 0.0 };
        let entities = drawable_with_own(live, speed);
        let picked = hud::pick(&ray, &entities, &|display_id| world.entity_bounds(display_id));

        self.set_target(picked);
    }

    /// Changes what is selected, telling the server when it actually changed.
    ///
    /// Clicking empty ground clears the selection, which goes out as a guid of
    /// zero rather than as no packet at all: the server holds the last
    /// selection it was given until it is told otherwise.
    fn set_target(&mut self, guid: Option<u64>) {
        if guid == self.target {
            return;
        }
        self.target = guid;
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if let Err(e) = live.connection.set_selection(guid.unwrap_or(0)) {
            tracing::warn!("sending the selection failed: {e:#}");
        }
    }

    /// Keeps a live connection alive: drains relayed traffic so time-sync
    /// requests keep getting answered, and pings no faster than
    /// `world::client::PING_INTERVAL` -- pinging faster is punished harder
    /// than not pinging at all.
    fn pump_live_connection(&mut self) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        // Senders of chat received this pump who are not in replicated state,
        // so their names can be asked for below.
        let mut unknown_speakers: Vec<u64> = Vec::new();
        match live.connection.drain(Duration::from_millis(1), 64) {
            // Every batch has to go through all the kinds of change replicate
            // handles -- object updates, relayed movement, monster moves,
            // destroys, names, chat -- or the world ends up quietly frozen
            // wherever the dropped kind would have moved something. The whole
            // report is used, not just its failure count: chat is *returned*
            // rather than stored, so ignoring the report loses every line.
            Ok(packets) => {
                let report = live::replicate(&mut live.state, &packets);
                live.note_failures(&report);
                // Why a cast did not happen, in the one place the player is
                // looking. This was missed on the first pass -- the failures
                // were parsed and then dropped on the floor, so pressing a key
                // did nothing and said nothing, which is the exact silent
                // failure that made sending chat cost three rounds of
                // debugging. Third time this crate has produced a category a
                // caller forgot to consume.
                for failure in &report.cast_failures {
                    let text = format!(
                        "{}: {}",
                        self.spells.name(failure.spell_id),
                        ::world::spell::describe_cast_failure(failure.reason)
                    );
                    tracing::debug!("cast refused -- {text}");
                    self.chat.push(Line::Chat(local_notice(text)));
                }
                // Fourth category this crate returns rather than stores, and
                // the fourth chance to drop one on the floor. Logged as well
                // as drawn, so "did the fight reach the client" is answerable
                // from a terminal -- the step that found 4.2's only real bug
                // before anyone looked at a window.
                for swing in &report.swings {
                    tracing::debug!(
                        "combat: {}",
                        hud::combat_entry(swing, live.guid, &live.state).rendered()
                    );
                    // Spawned above whoever was hit, missed and all -- a whiff
                    // wants a number just as much as a landed swing, or the
                    // fight looks like it stalled every time an attack fails
                    // to connect. Silently skipped if the victim's position
                    // has not replicated yet; the swing is still logged and
                    // still in the scrollback either way.
                    if let Some(pos) = live
                        .state
                        .get(swing.victim)
                        .and_then(|entity| entity.interpolated_position(Instant::now()))
                    {
                        let (text, kind) = if swing.missed() {
                            ("Miss".to_string(), ui::CombatTextKind::Miss)
                        } else if swing.critical() {
                            (swing.damage.to_string(), ui::CombatTextKind::Critical)
                        } else {
                            (swing.damage.to_string(), ui::CombatTextKind::Damage)
                        };
                        self.combat_text.push(PendingCombatText {
                            world_pos: glam::Vec3::new(pos.x, pos.y, pos.z),
                            text,
                            kind,
                            spawned: Instant::now(),
                        });
                    }
                    self.chat.push(Line::Swing(swing.clone()));
                }
                for message in &report.chat {
                    if message.sender != 0 && message.sender_name.is_none() {
                        unknown_speakers.push(message.sender);
                    }
                    // Logged as well as drawn, so the chat pipeline can be
                    // checked from a terminal. A window is the only way to see
                    // whether it *looks* right, but "did the line arrive and
                    // get attributed" is answerable without one.
                    tracing::debug!(
                        "chat: {}",
                        hud::chat_entry(message, &live.state).rendered()
                    );
                    self.chat.push(Line::Chat(message.clone()));
                }
            }
            Err(e) => tracing::warn!("draining the live connection failed: {e:#}"),
        }

        // A scrollback nobody trims is a leak with a user interface.
        let cap = self.hud.profile.style.chat_scrollback;
        if self.chat.len() > cap {
            let excess = self.chat.len() - cap;
            self.chat.drain(..excess);
        }

        // Names, a few per frame. The cache refuses to ask twice, so this is
        // safe to call every frame; the cap is only about not sending a
        // hundred packets in the frame after login.
        const NAMES_PER_FRAME: usize = 6;
        let asking = hud::names_to_ask(&mut live.state, &unknown_speakers, NAMES_PER_FRAME);
        for request in asking {
            let sent = match request {
                hud::NameRequest::Player { guid } => live.connection.ask_player_name(guid),
                hud::NameRequest::Creature { entry, guid } => {
                    live.connection.ask_creature_name(entry, guid)
                }
            };
            if let Err(e) = sent {
                tracing::warn!("asking for a name failed: {e:#}");
                break;
            }
        }
        if self.last_ping.elapsed() >= ::world::client::PING_INTERVAL {
            // Fire and forget: waiting for the pong would block the render
            // thread for a round trip. The drain above collects it.
            if let Err(e) = live.connection.send_ping(0) {
                tracing::warn!("keepalive ping failed: {e:#}");
            }
            self.last_ping = Instant::now();
        }

        // A target that died or walked out of range is gone from replicated
        // state, and a frame left showing the last numbers it had would be
        // indistinguishable from one that had stopped updating. Not sent to
        // the server: it removed the object itself, and cleared its own copy
        // of this selection when it did.
        if self.target.is_some_and(|guid| live.state.get(guid).is_none()) {
            self.target = None;
        }
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
        let summary = r.scene.as_ref().map(|scene| match &self.live {
            Some(live) => format!("{}\n\n{}", describe_live(live), describe(scene)),
            None => describe(scene),
        });
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

        // The interface draws from plain snapshots, so replicated state is
        // read out here and nothing inside the closure borrows it. Both are
        // rebuilt every frame; a held `UnitView` would keep showing whatever
        // the unit's health was when it was built.
        let player = self.live.as_ref().and_then(|live| {
            live.state
                .get(live.guid)
                .map(|entity| hud::unit_view(entity, live.character.clone()))
        });
        let target = self.target.and_then(|guid| {
            let live = self.live.as_ref()?;
            let entity = live.state.get(guid)?;
            Some(hud::unit_view(entity, hud::unit_name(&live.state, entity)))
        });

        // In egui's points rather than physical pixels, for both the marker
        // below and the combat text after it: the aspect ratio is the same
        // either way, so projecting through the point-sized viewport lands in
        // the coordinates egui paints in without a second conversion to get
        // wrong.
        let points = ctx.pixels_per_point().max(0.01);
        let viewport = (
            r.config.width as f32 / points,
            r.config.height as f32 / points,
        );

        // Where to bracket the selection.
        let target_marker = self.target.and_then(|guid| {
            let Some(Scene::Streaming(world)) = r.scene.as_ref() else {
                return None;
            };
            let entity = self.live.as_ref()?.state.get(guid)?;
            let at = entity.interpolated_position(std::time::Instant::now())?;
            let display_id = entity.display_id()?;
            let scale = entity
                .fields
                .get_f32(::world::update::fields::OBJECT_SCALE)
                .filter(|s| *s > 0.0)
                .unwrap_or(1.0);
            hud::marker_rect(
                &self.camera,
                viewport,
                glam::Vec3::new(at.x, at.y, at.z),
                scale,
                world.entity_bounds(display_id),
            )
        });

        // Every number still rising, oldest first. Pruned here rather than in
        // `pump_live_connection`, which runs on the network's schedule, not
        // the render clock -- an age has to be measured against the frame
        // that draws it, and a swing that never gets drawn (the window is
        // occluded, say) should not be able to dodge its own expiry.
        let lifetime = Duration::from_millis(self.hud.profile.style.combat_text_lifetime_ms);
        let now = Instant::now();
        self.combat_text
            .retain(|entry| now.saturating_duration_since(entry.spawned) < lifetime);
        let combat_text: Vec<ui::FloatingText> = self
            .combat_text
            .iter()
            .filter_map(|entry| {
                let pos = hud::combat_text_anchor(&self.camera, viewport, entry.world_pos)?;
                let age = now.saturating_duration_since(entry.spawned).as_secs_f32()
                    / lifetime.as_secs_f32().max(f32::EPSILON);
                Some(ui::FloatingText {
                    pos,
                    text: entry.text.clone(),
                    kind: entry.kind,
                    age,
                })
            })
            .collect();

        // Rendered fresh every frame from the messages that arrived, so names
        // that resolve after a line was received still reach it.
        let chat: Vec<ui::ChatEntry> = match self.live.as_ref() {
            Some(live) => self
                .chat
                .iter()
                .map(|line| match line {
                    Line::Chat(message) => hud::chat_entry(message, &live.state),
                    Line::Swing(swing) => hud::combat_entry(swing, live.guid, &live.state),
                })
                .collect(),
            None => Vec::new(),
        };
        let composing = self.composing.clone();
        let now = std::time::Instant::now();
        // Slot contents, resolved fresh each frame: an icon that failed to
        // load earlier can succeed later, a rearranged bar takes effect
        // immediately, and a cooldown sweep needs to be re-measured against
        // the clock every frame regardless of anything else changing.
        let mut bars: Vec<Vec<ui::frames::action_bar::SlotView>> = Vec::new();
        for bar in 0..ui::frames::action_bar::BARS {
            let mut slots = Vec::with_capacity(ui::frames::action_bar::SLOTS);
            for slot in 0..ui::frames::action_bar::SLOTS {
                let spell = self.hud.profile.bars.get(bar, slot).map(|id| {
                    let icon = self.spells.icon(&r.gpu, &mut r.egui_renderer, &mut self.chain, id);
                    let cooldown_fraction = self
                        .live
                        .as_ref()
                        .map(|live| live.state.cooldown_fraction(id, now))
                        .unwrap_or(0.0);
                    ui::frames::action_bar::SlotSpell {
                        id,
                        name: self.spells.name(id),
                        rank: self.spells.rank(id),
                        description: self.spells.description(id),
                        icon,
                        cooldown_fraction,
                    }
                });
                slots.push(ui::frames::action_bar::SlotView {
                    binding: ui::frames::action_bar::binding_label(bar, slot),
                    spell,
                });
            }
            bars.push(slots);
        }

        // Re-measured every frame against the clock for the same reason the
        // cooldown sweep is: the bar's fill has to move even though nothing
        // in replicated state changed between frames.
        let cast_bar = self.live.as_ref().and_then(|live| {
            let cast = live.state.active_cast(live.guid, now)?;
            Some(ui::CastBarView {
                spell_name: self.spells.name(cast.spell_id),
                progress: cast.progress_fraction(now),
                cast_time_ms: cast.duration_ms,
            })
        });

        let mut hud_response = ui::HudResponse::default();
        let interface = &mut self.hud;
        let editing = interface.edit.active;
        let layout_status = interface.status.clone();

        let output = ctx.run_ui(input, |ctx| {
            hud_response = interface.show(
                ctx,
                &ui::HudData {
                    player: player.as_ref(),
                    target: target.as_ref(),
                    target_marker,
                    combat_text: &combat_text,
                    chat: &chat,
                    composing: composing.as_deref(),
                    bars: &bars,
                    cast_bar: cast_bar.as_ref(),
                },
            );

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
                    ui.label(camera.describe());
                    ui.weak(match camera {
                        Camera::Orbit(_) => "drag to orbit, scroll to zoom",
                        Camera::Fly(_) => {
                            "drag to look, WASD to move, space/Q for height, \
                             shift to sprint, scroll for speed"
                        }
                    });
                    ui.weak(if editing {
                        "F1: stop editing the interface"
                    } else {
                        "click to target, F1 to rearrange the interface"
                    });
                    if let Some(status) = &layout_status {
                        ui.weak(format!("interface: {status}"));
                    }
                });
        });

        // Selecting a different animation restarts it; otherwise keep the
        // clock the UI may have reset.
        self.anim_time_ms = if anim != self.anim { 0 } else { anim_time };
        self.anim = anim;
        self.playing = playing;
        self.speed = speed;

        // A clicked slot casts, after the closure has released `self.hud`.
        if let Some((bar, slot)) = hud_response.activated {
            self.activate_slot(bar, slot);
        }

        if let Some(r) = self.renderer.as_mut() {
            r.egui_state
                .handle_platform_output(window, output.platform_output.clone());
        }
        output
    }
}
