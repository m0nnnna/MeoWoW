//! Windowed asset viewer.
//!
//! Shows a texture or a model from a WoW 3.3.5a installation. It exists to
//! prove the layers work together in one process: the archive layer finds a
//! file, the format crates decode it, and the GPU draws it.
//!
//! It also runs headless (`--screenshot`), rendering one frame to a PNG without
//! opening a window, which keeps the render path checkable from a terminal and
//! in CI.

mod live;
mod model;
mod scene;
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
            live::drawable_entities(&live.state, live.guid)
                .iter()
                .map(|entity| world::EntityPlacement {
                    display_id: entity.display_id,
                    position: entity.position,
                    orientation: entity.orientation,
                    scale: entity.scale,
                    moving: entity.moving,
                })
                .collect();
        let undrawable = world.set_entities(gpu, meshes, chain, &placements);
        if undrawable > 0 {
            tracing::warn!("{undrawable} object(s) had no loadable model");
        }
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
const LIVE_WALK_SPEED: f32 = 7.0;

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
                if !self.dragging {
                    self.last_cursor = None;
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    let pressed = event.state == ElementState::Pressed;
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
                        match &mut self.camera {
                            Camera::Orbit(c) => c.orbit(dx, dy),
                            // Dragging turns the view, so the world follows the
                            // cursor rather than moving against it.
                            Camera::Fly(c) => c.look(dx, -dy),
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
                    let placements: Vec<crate::world::EntityPlacement> =
                        live::drawable_entities(&live.state, live.guid)
                            .iter()
                            .map(|entity| crate::world::EntityPlacement {
                                display_id: entity.display_id,
                                position: entity.position,
                                orientation: entity.orientation,
                                scale: entity.scale,
                                moving: entity.moving,
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
    /// Terrain height, jumping and collision are out of scope here: Z is
    /// whatever the server last reported, so walking across sloped ground
    /// leaves the character floating or sinking. That is expected, not a bug
    /// -- see the movement section of `docs/PROTOCOL.md`.
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
            live.position.x += dx * LIVE_WALK_SPEED * sign * dt;
            live.position.y += dy * LIVE_WALK_SPEED * sign * dt;
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
        if let Camera::Fly(fly) = &mut self.camera {
            const BEHIND: f32 = 9.0;
            const ABOVE: f32 = 4.0;
            let yaw = live.orientation;
            fly.position = live.position
                - glam::Vec3::new(yaw.cos(), yaw.sin(), 0.0) * BEHIND
                + glam::Vec3::Z * ABOVE;
            fly.yaw = yaw;
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
        match live.connection.drain(Duration::from_millis(1), 64) {
            // Every batch has to go through all four kinds of change replicate
            // handles -- object updates, relayed movement, monster moves,
            // destroys -- or the world ends up quietly frozen wherever the
            // dropped kind would have moved something.
            Ok(packets) => live.fold_failures += live::replicate(&mut live.state, &packets),
            Err(e) => tracing::warn!("draining the live connection failed: {e:#}"),
        }
        if self.last_ping.elapsed() >= ::world::client::PING_INTERVAL {
            // Fire and forget: waiting for the pong would block the render
            // thread for a round trip. The drain above collects it.
            if let Err(e) = live.connection.send_ping(0) {
                tracing::warn!("keepalive ping failed: {e:#}");
            }
            self.last_ping = Instant::now();
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
                    ui.label(camera.describe());
                    ui.weak(match camera {
                        Camera::Orbit(_) => "drag to orbit, scroll to zoom",
                        Camera::Fly(_) => {
                            "drag to look, WASD to move, space/Q for height, \
                             shift to sprint, scroll for speed"
                        }
                    });
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
