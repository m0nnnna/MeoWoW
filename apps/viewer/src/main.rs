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
mod emitters;
mod hud;
mod items;
mod liquid;
mod live;
mod maps;
mod minimap;
mod model;
mod scene;
mod sound;
mod spells;
mod taxi;
mod terrain;
mod world;
mod world_object;

use std::path::PathBuf;
use std::rc::Rc;
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
use winit::window::{CursorGrabMode, Window, WindowId};

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

    /// Music volume, 0 to 1. Zero switches zone music off entirely.
    ///
    /// Separate from ambience because they are genuinely different things to
    /// want: plenty of people play with the music off and the birdsong on.
    #[arg(long, default_value_t = 0.35)]
    music_volume: f32,
    /// Ambience volume, 0 to 1. Zero switches it off.
    #[arg(long, default_value_t = 0.6)]
    ambience_volume: f32,
    /// Combat sound volume, 0 to 1.
    #[arg(long, default_value_t = 0.8)]
    effects_volume: f32,
    /// How long after a swing is reported its impact sound plays, in
    /// milliseconds.
    ///
    /// **625ms, found by ear against the animation** -- which is the only
    /// thing that could have told us. The server reports a swing when it
    /// resolves it and the client only then *starts* the attack animation, so
    /// an impact played on arrival lands before the blade does. That is how it
    /// was reported: "the audio plays -> sword makes contact".
    ///
    /// The honest number is the time from the animation's first frame to the
    /// frame the weapon connects, and reading that out of the model is a
    /// separate piece of work -- M2 animations carry timed events and this
    /// client parses none of them. Until it does, this is a constant found by
    /// a person watching a sword and pressing a key, and it stays adjustable
    /// in play for exactly that reason.
    #[arg(long, default_value_t = 625)]
    impact_delay_ms: u64,
    /// Light the world as if it were this game hour, 0 to 24.
    ///
    /// Overrides the realm's own clock, which is otherwise where the hour comes
    /// from. Exists because the lighting curves are functions of the hour and
    /// the only way to see what one looks like is to be standing in it -- and
    /// waiting six real hours for dusk is not a debugging loop.
    #[arg(long)]
    hour: Option<f32>,

    /// Draw this weather instead of whatever the realm reports.
    ///
    /// Takes a raw `SMSG_WEATHER` state so the whole vocabulary is reachable
    /// and the same parser decides what it means: 0 fine, 1 fog, 3/4/5 rain,
    /// 6/7/8 snow. The same argument as `--hour` -- weather that can only be
    /// looked at by starting a server and typing `.wchange` is weather nothing
    /// headless can check, and this milestone's whole point is that it falls.
    #[arg(long)]
    weather: Option<u32>,

    /// How hard the `--weather` state is coming down, 0 to 1.
    #[arg(long, default_value_t = 1.0)]
    weather_intensity: f32,

    /// Write the logged-in character's composed skin to this PNG.
    ///
    /// The skin is ten regions of one 512x512 atlas -- face, arms, hands,
    /// torso, legs, feet -- assembled in memory from a dozen files, and a
    /// character seen at walking distance is far too small to say which of them
    /// got painted. Looking at the atlas answers that in one glance, which is
    /// the same reason every format here has a dump command.
    #[arg(long)]
    skin_out: Option<PathBuf>,

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
#[allow(clippy::too_many_arguments)]
fn build_scene(
    gpu: &Gpu,
    terrain_renderer: &TerrainRenderer,
    liquid_renderer: &render::LiquidRenderer,
    liquid_types: &mut liquid::LiquidTypes,
    meshes: &mut MeshRenderer,
    chain: &mut Chain,
    args: &Args,
) -> Result<(Scene, Option<live::LiveWorld>)> {
    if args.realm_host.is_some() {
        return build_live_scene(gpu, meshes, chain, args);
    }
    build_offline_scene(gpu, terrain_renderer, liquid_renderer, liquid_types, chain, args)
        .map(|scene| (scene, None))
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
    if let Some(path) = &args.skin_out {
        match &live.look.skin {
            Some(skin) => {
                write_png(path, &skin.rgba, skin.width, skin.height)?;
                tracing::info!("composed skin written to {}", path.display());
            }
            None => tracing::warn!("no composed skin to write"),
        }
    }
    let mut world = world::World::new(
        chain,
        &live.map_directory,
        args.radius as i32,
        args.max_doodads,
    )?;

    if args.entities {
        let mut player_looks = std::collections::HashMap::new();
        // No bag window in this headless path, so nothing else needs item
        // icons -- but dressing another player still needs `Item.dbc`'s
        // entry-to-display bridge, the same table this struct already reads
        // for icons.
        let items = crate::items::Items::load(chain);
        // And the cast animations, for the same reason the item bridge is
        // read here: a headless render that posed casters differently from
        // the window would be evidence about neither.
        let casts = crate::spells::CastAnimations::read(chain);
        let placements: Vec<world::EntityPlacement> =
            // A headless render has no movement driver to have decided, and
            // the character is standing still: not swimming.
            drawable_with_own(&live, (0.0, 0.0), 0.0, false, false)
                .iter()
                .map(|entity| {
                    // Same three sources as the windowed path -- see `redraw`.
                    // A screenshot that dressed people differently from the
                    // window would be the wrong kind of evidence about it.
                    let (look, look_key) = if entity.guid == live.guid {
                        (Some(live.look.clone()), live.look_key)
                    } else if let Some(appearance) = entity.appearance {
                        let (look, key) = player_look(
                            &mut player_looks,
                            chain,
                            &items,
                            appearance,
                            &entity.visible_items,
                        );
                        (Some(look), key)
                    } else {
                        (None, 0)
                    };
                    world::EntityPlacement {
                        guid: entity.guid,
                        display_id: entity.display_id,
                        position: entity.position,
                        orientation: entity.orientation,
                        scale: entity.scale,
                        speed: entity.speed,
                        turning: entity.turning,
                        airborne: entity.airborne,
                        swimming: entity.swimming,
                        dead: entity.dead,
                        died_ms_ago: entity.died_ms_ago,
                        swung_ms_ago: entity.swung_ms_ago,
                        spell: casts.pose(entity.casting_spell, entity.cast_landed),
                        fighting: entity.fighting,
                        kind: entity.kind,
                        stance: look.as_deref().map(|l| l.stance).unwrap_or_default(),
                        look,
                        look_key,
                        sheathed: entity.sheathed,
                        sheath_changed_ms_ago: entity.sheath_changed_ms_ago,
                    }
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
    liquid_renderer: &render::LiquidRenderer,
    liquid_types: &mut liquid::LiquidTypes,
    chain: &mut Chain,
    args: &Args,
) -> Result<Scene> {
    if let Some(display_id) = args.creature {
        let (path, variations) = model::creature(chain, display_id)?;
        // Dressed the same way the streaming world dresses it, so a screenshot
        // of a display id shows what a player standing next to that creature
        // would see. Rendering it undressed here instead would make this the
        // one path that cannot answer the question it exists for -- and the
        // white-ghost bug lived in exactly that gap: `--creature` looked fine
        // because it was drawing beasts, which never needed the extra table.
        let look = character::NpcAppearances::load(chain).and_then(|t| t.look(display_id));
        match &look {
            Some(look) => tracing::info!(
                "display {display_id}: body {:?}, hair {:?}, geosets {:?}",
                look.body,
                look.hair,
                look.geosets
            ),
            None => tracing::info!(
                "display {display_id} has no extended appearance; \
                 its skins come from its own display row"
            ),
        }
        let loaded =
            model::load_dressed(gpu, chain, &path, &variations, args.lod, look.as_ref())?;
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
                liquid_renderer,
                liquid_types,
                chain,
                map,
                tile,
                args.radius,
                args.max_doodads,
            )?;
            return Ok(Scene::World(Box::new(loaded)));
        }
        let loaded = terrain::load(
            gpu,
            terrain_renderer,
            liquid_renderer,
            liquid_types,
            chain,
            map,
            tile.0,
            tile.1,
        )?;
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

/// How far behind the character the camera sits, and the range the wheel may
/// move it through.
///
/// **Aliases of the ui crate's, not a second opinion.** The starting distance
/// is a saved preference and the range is what the slider offers, so a copy
/// here would be a value that agrees with the settings window until somebody
/// edits one of them. The near end is deliberately not zero: a true
/// first-person view would put the camera inside the character's own head
/// geometry, which this client draws, and the result is a screenful of the
/// inside of a face.
const FOLLOW_NEAR: f32 = ui::camera::MIN_DISTANCE;
const FOLLOW_FAR: f32 = ui::camera::MAX_DISTANCE;
/// How high above the character's feet the camera *looks*.
///
/// The point the view orbits around, not where the eye sits -- roughly chest
/// height on a human, so the character fills the middle of the frame rather
/// than hanging from the top of it.
const FOLLOW_HEIGHT: f32 = 2.2;

/// How far the camera may be tilted above and below its subject.
///
/// Short of straight up and straight down, where an orbit degenerates: at
/// exactly a quarter turn the view direction is parallel to the world's up
/// axis and the horizon spins freely around it.
const FOLLOW_PITCH_LIMIT: f32 = std::f32::consts::FRAC_PI_2 - 0.05;

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

/// Units per second retreating. 3.3.5a backpedals at 4.5, deliberately slower
/// than a run so that turning to flee costs something.
///
/// Hardcoded like [`LIVE_RUN_SPEED`] beside it, and for the same reason: the
/// authoritative figures are in the movement block of the object-create packet,
/// which carries nine speeds and which this client does not parse yet. Until it
/// does, a character with a speed buff moves at the default here. Reading them
/// off the wire is the right fix and is not this one.
const LIVE_BACK_SPEED: f32 = 4.5;

/// How fast the character is travelling for the keys currently held,
/// **signed** so the renderer can tell retreating from advancing.
///
/// One number for both uses -- what moves the character and what chooses its
/// animation -- because in every case they are the same number. It has been
/// two numbers once, for one commit, and that is the story worth keeping:
///
/// **The sidestep cycle was flip-flopped three times before anybody asked the
/// table.** Given the `Shuffle` cycles it came back from play as the character
/// shimmying; given the run, as "running sideways plays the forward
/// animation"; given `Shuffle` again, as "he stands perfectly still and his
/// feet shuffle". Each fix was argued from a render. The argument that ended
/// it is one column: `AnimationData.dbc`'s `body_flags` **bit 64 is set on
/// exactly the twenty-eight animations that carry the character somewhere** --
/// `Walk`, `Run`, `Walkbackwards`, `Sprint`, the swim, stealth and flight
/// families, the ground-to-flight transitions -- and on nothing else in 506
/// rows. `ShuffleLeft` and `ShuffleRight` do not have it. They sit at
/// `body_flags: 1`, which is `Stand`'s value, and `Stop`'s.
///
/// So the game's own table classes the shuffles with standing still, which is
/// precisely how the last report described them. There is no lateral cycle on
/// land that travels, so a sidestep plays the run -- and the shuffles are
/// what [`live_turning`] uses, for a character turning on the spot, which is
/// the one thing an in-place cycle is for.
///
/// **The evidence that had been used instead does not survive contact with the
/// same table.** A sequence's `move_speed` was read as "this cycle does not
/// travel": `Walkbackwards` declares `0.00` and carries the travel bit, and
/// only two of the twenty-eight travelling animations declare a speed at all.
/// `AnimationData`'s `fallback` was read as "the shuffles fall back to Stand":
/// their fallback is `0`, and so is `Walk`'s and `Run`'s, because `0` there
/// means *no fallback* rather than row zero.
fn live_pace(moving: ::world::motion::Motion) -> f32 {
    use ::world::motion::Axis;
    match moving.longitudinal() {
        // Backing up is the only direction with its own speed *and* its own
        // cycle.
        Some(Axis::Negative) => -LIVE_BACK_SPEED,
        // Forward, and forward with a sideways component: a diagonal is
        // mostly a run.
        Some(Axis::Positive) => LIVE_RUN_SPEED,
        // A pure sidestep. Travels at the run speed and plays the run, per
        // the travel bit above -- and reporting `0.0` here, which the
        // shuffle-cycle attempt did, stops the character moving at all,
        // because this same number is what the movement integrator scales the
        // direction by.
        None if moving.is_moving() => LIVE_RUN_SPEED,
        _ => 0.0,
    }
}

/// How far the *drawn* body is turned towards where it is travelling, in
/// radians, left positive.
///
/// **This is the answer the cycle question kept failing to be.** A character
/// sidestepping plays the run -- there is no lateral travel cycle in the art,
/// and `AnimationData` says so -- and it looked wrong anyway, because the run
/// was being played by a body pointing straight at the camera: *"the player
/// runs the exact same way as W when you hit Q"*. The original does not have
/// a sideways run because it does not need one. **It turns the model**, far
/// enough that the legs are seen from the side while the same forward run
/// plays.
///
/// So the rule is the movement's own: the body faces the direction it is
/// actually travelling, clamped to a limit. `atan2` of the two axes gives
/// that angle for free, and the clamp is what keeps a sidestep from reading
/// as a turn -- a character strafing around a target still faces the target,
/// which is the half of this that a full rotation would break.
///
/// **Render only.** `live.orientation` is what the movement direction and
/// every outgoing `MSG_MOVE_*` are computed from, and turning *that* would
/// send the character round in a circle. This is added where the body is
/// placed and nowhere else.
///
/// A pure retreat gets nothing: there is no lateral component to lean into,
/// and `Walkbackwards` exists precisely so a character can back away facing
/// forwards.
fn strafe_yaw(moving: ::world::motion::Motion, limit: f32) -> f32 {
    use ::world::motion::Axis;
    let lateral = match moving.lateral() {
        Some(Axis::Positive) => 1.0f32,
        Some(Axis::Negative) => -1.0,
        None => return 0.0,
    };
    let longitudinal = match moving.longitudinal() {
        Some(Axis::Positive) => 1.0f32,
        Some(Axis::Negative) => -1.0,
        None => 0.0,
    };
    lateral.atan2(longitudinal).clamp(-limit, limit)
}

/// How far a sidestep may turn the body, in degrees, and the choices `F4`
/// cycles through.
///
/// **A chosen number, and the reason it is a list is that it has to be looked
/// at.** Nothing in the archives says how far the original leans -- this is a
/// client rendering decision, not a table -- so the honest thing is to make
/// comparing them a keypress rather than a rebuild. 45 is the default because
/// it is the angle a diagonal already travels at, so a sidestep and a
/// forward-sidestep agree.
const STRAFE_YAW_CHOICES: [f32; 5] = [0.0, 30.0, 45.0, 60.0, 90.0];
const STRAFE_YAW_DEFAULT: usize = 2;

/// The rate the A/D keys are turning the character on the spot, signed, with
/// left positive.
///
/// Only ever non-zero while the character is standing still: turning *while*
/// travelling is already expressed by the run cycle and the heading it is drawn
/// at, and a shuffle laid over a run is a stumble. See
/// `world::Motion::from_pace` for what the shuffle is and what is still a
/// hypothesis about it.
fn live_turning(keys: KeyState, steering: bool, moving: ::world::motion::Motion) -> f32 {
    if steering || moving.is_moving() {
        return 0.0;
    }
    match (keys.left, keys.right) {
        (true, false) => LIVE_TURN_RATE,
        (false, true) => -LIVE_TURN_RATE,
        _ => 0.0,
    }
}

/// How wide the character is for collision, and how tall.
///
/// **Chosen.** The models declare a bounding box and it is the wrong shape to
/// use: it wraps the drawn geometry including an outstretched arm and a
/// weapon, where what has to fit through a doorway is the body. A little over
/// half a unit across and two units tall is a person at this scale, and the
/// abbey's doors are wide enough for it.
const BODY_RADIUS: f32 = 0.55;
const BODY_HEIGHT: f32 = 2.0;

/// How high a surface can be and still be stepped onto rather than walked into.
///
/// **One constant for two uses on purpose**: it is how far up `floor_under`
/// will look for something to stand on, *and* how tall an obstacle may be
/// before `slide` treats it as a wall. Those are the same idea -- "can I get
/// onto that" -- and two numbers would let a character be stopped by a step it
/// was simultaneously tall enough to stand on.
///
/// It started at 1.2 and came down. At a body height of 2.0 that was
/// waist-high, which climbs the abbey's stairs and also strolls over fences.
/// A little under half the body is a knee, which is about what a person steps
/// onto without thinking about it -- and it is the number to move, in one
/// place, if stairs still catch or fences stop catching.
const STEP_HEIGHT: f32 = 0.8;

/// How far a liquid surface must stand above the bed before a character swims
/// rather than wades.
///
/// Measured against [`BODY_HEIGHT`] rather than chosen freely: chest deep on a
/// two-unit body. Anything much smaller has a character swimming across a ford
/// and along every shoreline, where the water is a hand's breadth deep and the
/// `MH2O` sheet still covers the ground.
///
/// **The server does not have to agree, and mostly does not.** AzerothCore
/// counts a player as in water the moment the surface is above their feet at
/// all (`Map::GetLiquidData`), which is a different question -- it decides
/// whether to run a breath timer, not whether to play a swim cycle. The two
/// only have to agree about the deep middle, and they do.
const SWIM_DEPTH: f32 = 1.4;

/// How far below the surface a floating character's feet rest.
///
/// Not the same as [`SWIM_DEPTH`]: that is when swimming *starts*, this is
/// where the body sits once it has. Slightly less than the body height, so the
/// head clears the water.
const SWIM_FLOAT: f32 = 1.7;

/// Units per second gained by holding the rise key while swimming.
const SWIM_CLIMB_RATE: f32 = 3.0;

/// How steeply the camera must look down before moving forward dives.
///
/// Without a deadzone a character swimming on the level with the camera a
/// degree below horizontal sinks slowly and forever, which reads as the water
/// not holding them up.
const DIVE_PITCH_DEADZONE: f32 = 0.15;

/// Seconds to close most of the gap back to the surface when nothing is
/// pushing the character down.
const BUOYANCY_TAU: f32 = 0.45;

/// Radians per second turned by the A/D keys. Not verified against a
/// reference client -- see the facing note in `docs/RENDERING.md` -- but close
/// enough that the character does not spin wildly or crawl.
const LIVE_TURN_RATE: f32 = std::f32::consts::PI;

/// Which movement keys are currently held.
///
/// One set of keys serves two quite different cameras, so several of them mean
/// different things depending on whether a character is being driven. `Q`/`E`
/// strafe a live character and raise/lower a free camera; `Space` jumps or
/// rises. That dual reading is why the fields are named after the *key's
/// role* rather than after a direction in space -- `strafe_left` is what `Q`
/// does in the world, and `up` is what the free camera does with the keys the
/// world has no vertical use for.
#[derive(Default, Clone, Copy)]
struct KeyState {
    forward: bool,
    back: bool,
    left: bool,
    right: bool,
    strafe_left: bool,
    strafe_right: bool,
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
            KeyCode::KeyQ => &mut self.strafe_left,
            KeyCode::KeyE => &mut self.strafe_right,
            KeyCode::Space => &mut self.up,
            KeyCode::ControlLeft => &mut self.down,
            KeyCode::ShiftLeft | KeyCode::ShiftRight => &mut self.fast,
            _ => return false,
        };
        *slot = pressed;
        true
    }

    /// What these keys mean for a character in the world.
    ///
    /// `A`/`D` turn rather than strafe, which is the arrangement 3.3.5a ships
    /// with -- strafing is `Q`/`E`. Holding the right mouse button changes
    /// that: steering comes from the mouse, so `A`/`D` become strafe keys and
    /// the player can circle a target while facing it, which is the whole
    /// point of steering with the mouse.
    fn motion(&self, steering: bool) -> ::world::motion::Motion {
        ::world::motion::Motion {
            forward: self.forward,
            backward: self.back,
            strafe_left: self.strafe_left || (steering && self.left),
            strafe_right: self.strafe_right || (steering && self.right),
        }
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
    let yaw = args
        .yaw
        .map(f32::to_radians)
        .unwrap_or(live.orientation);
    let pitch = args.pitch.map(f32::to_radians).unwrap_or(-0.2);
    let mut fly = orbit_around(live.position, yaw, pitch, ui::Camera::default().start_distance());
    // Walking pace rather than the flying speed a survey wants: the point here
    // is to stand somewhere, not to cross a continent.
    fly.speed = 30.0;
    Camera::Fly(fly)
}

/// How far above the ground the camera is kept when it would otherwise sink
/// into it.
///
/// Small: the point is to stop the view passing *through* the terrain, not to
/// hold the camera at a respectful distance from it. Too large and looking up
/// at a character standing on a slope shoves the eye out into the open.
const CAMERA_GROUND_CLEARANCE: f32 = 0.5;

/// Pulls the camera in until it is not underground.
///
/// **The eye moves along its own view ray, never sideways or upward.** Lifting
/// it instead would keep the distance and break the framing -- the subject
/// would slide off centre, which is the bug just fixed -- and pushing it
/// sideways would swing the whole world. Shortening the distance is the one
/// move that leaves the picture pointing exactly where it did, which is why it
/// is what the game this models does.
///
/// Marched from the subject outwards rather than solved: the ground under a
/// straight line is not a straight line, so a camera that only checked its
/// destination would happily tunnel through a ridge between here and there and
/// come out the far side looking through it.
fn pull_camera_out_of_the_ground(
    focus: glam::Vec3,
    eye: glam::Vec3,
    ground_at: impl Fn(f32, f32) -> Option<f32>,
) -> glam::Vec3 {
    const STEPS: usize = 12;
    let span = eye - focus;
    let mut allowed = 1.0f32;
    for step in 1..=STEPS {
        let t = step as f32 / STEPS as f32;
        let at = focus + span * t;
        let Some(ground) = ground_at(at.x, at.y) else {
            // No terrain loaded there yet. Stopping short on a tile that has
            // not streamed in would yank the camera into the character every
            // time the world was still catching up, so an unknown height is
            // treated as clear -- the same direction the rest of the streaming
            // code fails in.
            continue;
        };
        if at.z < ground + CAMERA_GROUND_CLEARANCE {
            // Stop just before the offending sample rather than at it, so the
            // eye ends up above the ground rather than exactly on the surface
            // that rejected it.
            allowed = ((step - 1) as f32 / STEPS as f32).max(0.0);
            break;
        }
    }
    focus + span * allowed
}

/// How close a ghost has to be to its body before the server will hand it
/// back, in yards.
///
/// **The server's number, not a guess**: `CORPSE_RECLAIM_RADIUS` in
/// `Corpse.h`, checked in `HandleReclaimCorpseOpcode` alongside five other
/// conditions that all refuse in silence. Used here only to decide what the
/// prompt *says* -- the request is sent whenever it is pressed in range, and
/// the server remains the one that decides.
const CORPSE_RECLAIM_RADIUS: f32 = 39.0;

/// How far in front of a wall the eye stops, in units.
///
/// Enough that the near plane does not slice into the surface that stopped it,
/// which reads as the wall vanishing and the room beyond appearing -- exactly
/// the thing this exists to prevent, arriving by a different route.
const CAMERA_WALL_CLEARANCE: f32 = 0.35;

/// How close to the character the eye may be pushed by a wall.
///
/// A wall directly behind the character would otherwise put the eye inside the
/// head, and this client *draws* that head -- a screenful of the inside of a
/// face. The original hides the model at that range instead; until this one
/// does, stopping short is the smaller of the two wrongs.
const CAMERA_MIN_PULL_IN: f32 = 1.5;

/// Pulls the camera in until nothing solid is between it and the character.
///
/// Buildings, not terrain: [`pull_camera_out_of_the_ground`] already handles
/// the ground it stands on, and it does that by sampling a height field, which
/// knows nothing about a wall. Standing inside the abbey with the camera
/// outside it -- the view passing through a wall and looking back in -- is what
/// this fixes, and no amount of ground sampling could.
///
/// **Along the view ray and nowhere else**, for the same reason as the ground
/// version: shortening the distance is the only move that leaves the picture
/// pointing where it did. The subject stays centred; only the range changes.
fn pull_camera_in_front_of_walls(
    focus: glam::Vec3,
    eye: glam::Vec3,
    first_hit: impl Fn(glam::Vec3, glam::Vec3) -> Option<f32>,
) -> glam::Vec3 {
    let span = eye - focus;
    let length = span.length();
    if length < 1e-3 {
        return eye;
    }
    let Some(t) = first_hit(focus, eye) else {
        return eye;
    };
    // The hit is a fraction of the way out; back off a fixed distance from it
    // and never come closer to the character than the floor above.
    let stopped = (t * length - CAMERA_WALL_CLEARANCE).clamp(CAMERA_MIN_PULL_IN, length);
    focus + span * (stopped / length)
}

/// Places the eye on a sphere around a character, looking at them.
///
/// **Shared by the screenshot placement and the per-frame follow on purpose.**
/// They have to agree: a headless render exists to be evidence about what the
/// window shows, and two copies of this arithmetic would make it evidence about
/// itself. Same rule as unprojecting the picking ray from the matrix the scene
/// was drawn with.
fn orbit_around(feet: glam::Vec3, yaw: f32, pitch: f32, distance: f32) -> Fly {
    let focus = feet + glam::Vec3::Z * FOLLOW_HEIGHT;
    let (sp, cp) = pitch.sin_cos();
    let (sy, cy) = yaw.sin_cos();
    // The camera's own `forward()`, so the eye is placed by the formula the
    // view matrix reads it back with.
    let forward = glam::Vec3::new(cp * cy, cp * sy, sp);
    Fly {
        position: focus - forward * distance,
        yaw,
        pitch,
        ..Default::default()
    }
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
    liquid_renderer: &render::LiquidRenderer,
    liquid_types: &liquid::LiquidTypes,
    // Only the streaming world has a sky: the model and texture views are not
    // places, so there is no hour or position to resolve a gradient for. The
    // same is true of the weather.
    sky: &render::SkyRenderer,
    precipitation: &render::PrecipitationRenderer,
    // Everything alight, already stepped and uploaded for this frame by
    // `World::update_emitters`. Immutable here on purpose: a render pass holds
    // the encoder, so nothing may be built or grown once it is open.
    particles: &render::ParticleRenderer,
    emitters: &emitters::Emitters,
    falling: Option<Falling>,
    material_binds: &[wgpu::BindGroup],
    bones: Option<&BoneBuffer>,
    world_binds: &[Vec<wgpu::BindGroup>],
    identity: &render::mesh::InstanceBuffer,
    // The world's own lighting, when there is a place and an hour to resolve
    // it for. `None` everywhere else -- a model or texture view has neither,
    // and gets the placeholder headlight.
    lighting: Option<(dbc::light::Sample, f32)>,
    // Wall clock, which is what scrolls a liquid surface and steps its
    // animation. Passed in rather than read here so a headless render can pin
    // it and produce the same picture twice -- the same reason `--screenshot`
    // hands the precipitation a fixed clock.
    seconds: f32,
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
            liquid_renderer,
            sky,
            precipitation,
            particles,
            emitters,
            falling,
            bones,
            lighting,
            seconds,
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

            // The offline scene has no `Light.dbc` sample to hand -- it draws
            // under the placeholder headlight, which is what `camera.uniform`
            // above supplies. So the liquid gets the placeholder too, from the
            // same struct, rather than a set of constants chosen here that
            // would light the water differently from the shore.
            let unlit = camera.uniform(size.0 as f32 / size.1.max(1) as f32);
            draw_liquid(
                gpu,
                &mut pass,
                terrain_parts.iter().filter_map(|t| t.liquid.as_ref()),
                liquid_renderer,
                liquid_types,
                &unlit,
                camera.view_proj(size.0 as f32 / size.1.max(1) as f32),
                camera.eye(),
                seconds,
            );
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

            // A torch drawn on its own is the cheapest way to *look* at an
            // emitter, and looking is the only way one can be checked: no
            // amount of dumping says whether a flame reads as a flame. Same
            // reasoning as the composed character skin getting a dump command.
            let (right, up) = camera.billboard_basis();
            particles.set_camera(gpu, camera.view_proj(aspect), right, up);
            emitters.draw(&mut pass, particles);
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

/// Runs a lone model's emitters up to their steady state.
///
/// **Several steps, not one.** One frame of a particle system is an emitter
/// that has just been switched on -- a few sparks at the nozzle and no fire --
/// so a single-frame render says the feature does not work when it does. The
/// step is fixed rather than wall-clock so two runs produce the same picture,
/// the same reason `--screenshot` pins the weather's clock.
fn warm_emitters(
    gpu: &Gpu,
    particles: &mut render::ParticleRenderer,
    emitters: &mut emitters::Emitters,
    m: &LoadedModel,
    anim: Option<usize>,
    time_ms: u32,
    steps: usize,
) {
    if m.particles.is_empty() && m.ribbons.is_empty() {
        return;
    }
    let sequence = anim.filter(|s| *s < m.sequences.len()).unwrap_or(0);
    let duration = m
        .sequences
        .get(sequence)
        .map(|s| s.duration_ms.max(1))
        .unwrap_or(1);
    // **The clock advances across the warm-up, and a ribbon is why.** A trail
    // is the *history* of a bone: run sixty steps at one frozen instant and
    // every edge lands in the same place, which draws as nothing at all and
    // reads as ribbons being unimplemented. Stepping the animation makes the
    // bone move, and the strip appears. With `steps` of 1 -- the windowed
    // path, where the caller's own clock is already running -- this is a
    // no-op.
    for step in 0..steps.max(1) {
        let time_ms = (time_ms + (step as u32 * 1000 / 60)) % duration;
        let pose = if m.sequences.is_empty() {
            vec![glam::Mat4::IDENTITY; m.bones.len().max(1)]
        } else {
            m2::Model::pose_bones(&m.bones, sequence, time_ms)
        };
        emitters.update(
            gpu,
            particles,
            std::iter::once(emitters::single_model(m, &pose, sequence, time_ms)),
            1.0 / 60.0,
        );
    }
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

/// Draws every liquid sheet in a set of tiles.
///
/// Free-standing and taking an iterator because **both render paths need it**
/// -- the streaming world and the offline `--world` scene each hold their
/// tiles differently and each has to draw the same water. A liquid pass wired
/// into only one of them would leave every `--screenshot` showing a dry
/// riverbed, which is the one instrument this project has for checking a
/// render without a window.
///
/// Takes the *already built* camera uniform rather than the lighting sample:
/// see the note at its construction.
#[allow(clippy::too_many_arguments)]
fn draw_liquid<'a>(
    gpu: &Gpu,
    pass: &mut wgpu::RenderPass<'a>,
    tiles: impl Iterator<Item = &'a liquid::LoadedLiquid>,
    liquid_renderer: &'a render::LiquidRenderer,
    liquid_types: &'a liquid::LiquidTypes,
    lit: &render::mesh::CameraUniform,
    view_proj: glam::Mat4,
    eye: glam::Vec3,
    seconds: f32,
) {
    let mut began = false;
    let (mut drawn, mut skipped) = (0usize, 0usize);
    for sheet in tiles {
        // Deferred until there is something to draw, so a dry map never
        // touches the uniform buffer or swaps the pipeline.
        if !began {
            began = true;
            liquid_renderer.begin(
                gpu,
                pass,
                view_proj,
                eye,
                glam::Vec3::new(lit.light[0], lit.light[1], lit.light[2]),
                [lit.sun[0], lit.sun[1], lit.sun[2]],
                lit.sun[3],
                [lit.ambient[0], lit.ambient[1], lit.ambient[2]],
                [lit.fog[0], lit.fog[1], lit.fog[2]],
                (lit.fog_range[0], lit.fog_range[1]),
                seconds,
            );
        }
        pass.set_vertex_buffer(0, sheet.vertices.slice(..));
        pass.set_index_buffer(sheet.indices.slice(..), wgpu::IndexFormat::Uint32);
        for draw in &sheet.draws {
            // A type whose art did not resolve is skipped rather than drawn
            // untextured: a magenta river reads as a bug in the water, and the
            // warning naming the missing file has already been logged once.
            //
            // **But the skip says so.** Silently dropping a draw makes
            // geometry that was built and never submitted look exactly like
            // geometry that was never built -- and those want opposite
            // investigations. This is the line that separated them when the
            // streaming path drew nothing: the tiles reported 2,398 triangles
            // of water and every one of them was skipped here.
            let Some(frame) = liquid_types.frame_at(draw.liquid_type, seconds) else {
                skipped += 1;
                continue;
            };
            drawn += 1;
            pass.set_bind_group(1, frame, &[]);
            pass.draw_indexed(
                draw.first_index..draw.first_index + draw.index_count,
                0,
                0..1,
            );
        }
    }
    // **Both counters, always.** Warning only when something was skipped left
    // "nothing was skipped" and "nothing was iterated" as the same silence --
    // and those are opposite faults: a draw that resolved no art, and a draw
    // list that was empty because the tiles were never reached. One line
    // naming both numbers separates them in a single run.
    if skipped > 0 {
        tracing::warn!(
            drawn,
            skipped,
            "liquid sheets skipped for want of surface art -- the cache used to              draw is not the one that built this geometry"
        );
    } else {
        // **Only when it changes.** This runs every frame, and at `debug` it
        // wrote a third of a million lines and 68MB in half an hour -- burying
        // the once-per-event lines a live test is actually reading, which is
        // the exact mistake `own_entity` documents and this walked into
        // anyway. The count is what matters, so log the *transition*.
        use std::sync::atomic::{AtomicUsize, Ordering};
        static LAST: AtomicUsize = AtomicUsize::new(usize::MAX);
        if LAST.swap(drawn, Ordering::Relaxed) != drawn {
            tracing::debug!(drawn, "liquid sheets drawn");
        }
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
    liquid_renderer: &render::LiquidRenderer,
    // **No liquid cache parameter, deliberately.** The streaming world builds
    // its tiles' liquid into its *own* cache, so that is the only one whose
    // frames those tiles' type ids resolve against. Taking one from the caller
    // is what let the renderer's empty cache be passed in, which skipped every
    // draw and made the water invisible while every diagnostic said it had been
    // built. A parameter that can be wrong is worse than no parameter.
    sky: &render::SkyRenderer,
    precipitation: &render::PrecipitationRenderer,
    particles: &render::ParticleRenderer,
    emitters: &emitters::Emitters,
    falling: Option<Falling>,
    bones: Option<&BoneBuffer>,
    lighting: Option<(dbc::light::Sample, f32)>,
    seconds: f32,
) {
    let aspect = size.0 as f32 / size.1.max(1) as f32;
    // **The one uniform, kept.** The liquid pass has its own bind group and
    // therefore its own copy of the sun, the ambient and the fog -- and a
    // second *derivation* of those would agree with the terrain's only until
    // somebody edited one of them. Water lit half a stop off the shore it laps
    // against is a seam nothing would catch but an eye. Same rule as the
    // picking ray being unprojected from the matrix the scene was drawn with.
    let lit = lit_uniform(camera, aspect, sky, lighting);
    meshes.update_camera(gpu, &lit);
    // Built once here and handed to both the sky and the scene, rather than
    // each asking the camera for its own: the sky's horizon has to sit exactly
    // where the ground's does, and two derivations agree only until one of them
    // is edited. Same reasoning as the picking ray.
    let view_proj = camera.view_proj(aspect);

    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("streaming world"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: color,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(sky_colour(sky, lighting.as_ref())),
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

    // First, and into the world's own pass so it shares the depth buffer:
    // the sky writes no depth and refuses none, so everything below covers it
    // and no second clear is needed.
    sky.draw(
        gpu,
        &mut pass,
        view_proj,
        camera.eye(),
        &sky_gradient(lighting.as_ref()),
        // The same arc the world is lit by, so the shadows and the disc agree
        // about where the sun is -- one function, not two copies of an angle.
        lighting.map_or(glam::Vec3::Z, |(_, hour)| sun_direction(hour)),
        sky.encode(lighting.map_or([1.0; 3], |(sample, _)| sample.disc)),
        // A storm hides the sun. `Light.dbc` has nothing to say about this,
        // but the alternative is a sun burning through an overcast sky.
        1.0 - falling.map_or(0.0, |f| f.intensity),
    );

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

    // **After everything opaque and before the weather.** Liquid blends, so
    // it has to be drawn over the riverbed it is meant to be seen through --
    // and a raindrop landing in a river must be drawn over the water, not
    // under it, which is what puts precipitation after this rather than before.
    draw_liquid(
        gpu,
        &mut pass,
        world.tiles().filter_map(|t| t.terrain.liquid.as_ref()),
        liquid_renderer,
        world.liquid_types(),
        &lit,
        view_proj,
        camera.eye(),
        seconds,
    );

    // **After the liquid and before the weather.** A torch's flame has to be
    // drawn over the water it is reflected in rather than under it, for the
    // same reason the liquid comes after the riverbed; and rain falls in front
    // of a fire, not behind it. Both sprites and strips test depth without
    // writing it, so nothing here occludes anything else.
    //
    // The camera basis is taken from the matrix this pass is drawn with, not
    // rebuilt from the camera's angles -- a billboard widened along a basis
    // that disagrees with the projection leans, and reads as a bad texture
    // rather than as a stale copy. Same rule as the picking ray.
    let (right, up) = camera.billboard_basis();
    particles.set_camera(gpu, view_proj, right, up);
    emitters.draw(&mut pass, particles);

    // Last, so it falls in front of everything solid -- and inside the same
    // pass, so it can still be depth-tested against the world it is falling
    // through rather than pasted over it.
    if let Some(falling) = falling {
        let shape = render::precipitation::Shape::for_kind(falling.kind);
        precipitation.draw(
            gpu,
            &mut pass,
            view_proj,
            camera.eye(),
            &shape,
            drop_colour(sky, lighting.as_ref()),
            falling.intensity,
            falling.seconds,
        );
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
    let entities = live::drawable_entities(&live.state, live.guid, live.position);
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
        "\n{} replicated ({} created, {} removed, {} removals hit nothing, \
         {} moves, {} orphaned)",
        live.state.len(),
        stats.created,
        stats.removed,
        // The number whose absence hid a wire-format bug for a whole
        // milestone -- see `Stats::removed_unknown`. It belongs beside
        // `removed` rather than anywhere else: the two together are the
        // statement, and either alone reads as healthy.
        stats.removed_unknown,
        stats.movement_updates,
        stats.orphaned,
    ));
    // The relayed half, split out because it is the one under test: a mover
    // that snapped is a mover drawn the old, jumping way. See
    // `world::WorldState::apply_relayed_movement_at`.
    let snapped = stats.relayed_first_sample + stats.relayed_gap + stats.relayed_teleport;
    if stats.relayed_paths + snapped > 0 {
        text.push_str(&format!(
            "\n{} relayed walked, {snapped} snapped",
            stats.relayed_paths,
        ));
        if stats.relayed_paths > 0 {
            text.push_str(&format!(
                " (mean {}ms)",
                stats.relayed_interval_ms / stats.relayed_paths as u64
            ));
        }
    }
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
    let sky = render::SkyRenderer::new(&gpu, format);
    let precipitation = render::PrecipitationRenderer::new(&gpu, format);
    let liquid_renderer = render::LiquidRenderer::new(&gpu, format);
    let mut liquid_types = liquid::LiquidTypes::default();

    let (mut scene, live) = build_scene(
        &gpu,
        &terrain_renderer,
        &liquid_renderer,
        &mut liquid_types,
        &mut meshes,
        chain,
        args,
    )?;
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
            world.update(
                &gpu,
                &mut meshes,
                &terrain_renderer,
                &liquid_renderer,
                chain,
                eye,
            );
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

    // Read here rather than at startup: a texture or model view has no world to
    // light, and these are four tables worth of DBC.
    // A texture or model view has no world to light and skips four tables
    // worth of DBC; a map with an hour asked for it does need them, connected
    // or not.
    let offline_map = offline_map_id(chain, args.map.as_deref());
    let lighting = (live.is_some() || (offline_map.is_some() && args.hour.is_some()))
        .then(|| dbc::light::Lighting::load(|path| chain.read(path).ok()))
        .flatten();
    let camera_eye = match &camera {
        Camera::Fly(f) => f.position,
        Camera::Orbit(o) => o.eye(),
    };
    let weather = frame_weather(live.as_ref(), args);

    // **Stepped before the encoder opens, and stepped several times.** A
    // headless render draws one frame, and one frame of a particle system is
    // an emitter that has just been switched on: no fire, a few sparks at the
    // nozzle, and a picture that says the feature does not work. Running it
    // forward to its steady state is what makes a screenshot comparable to
    // what a window shows -- and the fixed step keeps two runs identical, the
    // same reason the weather gets a pinned clock below.
    let mut particles = render::ParticleRenderer::new(&gpu, format);
    let mut emitters = emitters::Emitters::new();
    match &scene {
        Scene::Streaming(world) => {
            world.update_animations(&gpu, &meshes);
            for _ in 0..60 {
                world.update_emitters(&gpu, &mut particles, &mut emitters, 1.0 / 60.0);
            }
        }
        Scene::Model(m) => warm_emitters(
            &gpu,
            &mut particles,
            &mut emitters,
            m,
            args.anim,
            args.anim_time,
            60,
        ),
        _ => {}
    }
    // Always, not only when something was drawn. Silence would be equally
    // what an empty scene produces, and "no emitters ran" and "there were
    // none" are the two answers this line exists to separate.
    tracing::info!("{}", emitters.describe());

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
        &liquid_renderer,
        &liquid_types,
        &sky,
        &precipitation,
        &particles,
        &emitters,
        // A fixed clock, so two screenshots of the same weather are the same
        // picture: a headless render exists to be compared against another
        // one, and a wall clock would make every drop move between runs.
        resolve_precipitation(weather, 4.0),
        &binds,
        bones.as_ref(),
        &world_binds,
        &identity,
        resolve_lighting(
            lighting.as_ref(),
            live.as_ref(),
            weather,
            offline_map,
            args.hour,
            camera_eye,
        ),
        // The same fixed clock the weather gets, and for the same reason: a
        // river whose surface scrolled with the wall clock would make two
        // screenshots of one scene differ, which is precisely what
        // `--screenshot` exists not to do.
        4.0,
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
    sky: render::SkyRenderer,
    precipitation: render::PrecipitationRenderer,
    /// The pipelines and buffers for everything alight, and the live emitter
    /// state that feeds them.
    ///
    /// Two objects rather than one because they are used at different times:
    /// the renderer is grown and written before the pass opens, and only read
    /// once it has.
    particles: render::ParticleRenderer,
    emitters: emitters::Emitters,
    liquid_renderer: render::LiquidRenderer,
    /// Surface art for the liquid types the offline scenes use.
    ///
    /// The streaming world keeps its own -- see `world::World::liquid_types` --
    /// because it loads and evicts tiles for the whole session, while this one
    /// serves the single scene `--map`/`--world` built at startup. Two caches
    /// rather than one shared: the bind groups here are referenced by geometry
    /// that never changes, and threading one cache through both owners would
    /// mean a borrow of the world held across every offline draw.
    liquid_types: liquid::LiquidTypes,
    material_binds: Vec<wgpu::BindGroup>,
    world_binds: Vec<Vec<wgpu::BindGroup>>,
    identity: render::mesh::InstanceBuffer,
    bones: Option<BoneBuffer>,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    scene: Option<Scene>,
}

/// Which `CMSG_MESSAGECHAT` an unprefixed line becomes.
///
/// A whisper carries the recipient's name inside the channel itself rather
/// than as a separate field, because that is what the composing indicator has
/// to show and what has to survive a sticky switch: "whisper" alone would
/// leave the player unable to tell *who* the next line goes to without
/// checking anywhere else.
#[derive(Debug, Clone, PartialEq)]
enum ChatChannel {
    Say,
    Party,
    Yell,
    Whisper(String),
}

impl ChatChannel {
    /// The `world::ChatType` this channel sends as, and the whisper target if
    /// there is one -- `message_chat` ignores the target for every other
    /// type, so `""` is not a special case there.
    fn wire(&self) -> (::world::ChatType, &str) {
        match self {
            ChatChannel::Say => (::world::ChatType::Say, ""),
            ChatChannel::Party => (::world::ChatType::Party, ""),
            ChatChannel::Yell => (::world::ChatType::Yell, ""),
            ChatChannel::Whisper(name) => (::world::ChatType::Whisper, name.as_str()),
        }
    }

    /// What to show before the composing text, or `None` for the default
    /// channel -- see `App::chat_channel`'s doc comment for why this exists
    /// at all rather than sending silently on whatever was last chosen.
    fn label(&self) -> Option<String> {
        match self {
            ChatChannel::Say => None,
            ChatChannel::Party => Some("party".into()),
            ChatChannel::Yell => Some("yell".into()),
            ChatChannel::Whisper(name) => Some(format!("to {name}")),
        }
    }
}

struct App {
    args: Args,
    chain: Chain,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    camera: Camera,
    dragging: bool,
    last_cursor: Option<(f64, f64)>,
    /// Whether the pointer is currently held by a drag: hidden, confined to
    /// the window, and warped back to where the gesture started after every
    /// movement.
    ///
    /// **Without this a long turn runs out of desk.** The pointer reaches the
    /// edge of the window and the camera simply stops turning, or leaves the
    /// window entirely and the next click lands in another application. Both
    /// read as the drag "sticking" rather than as the mouse having gone
    /// somewhere.
    cursor_captured: bool,
    /// Where a captured pointer is pinned, and where it is put back when the
    /// button comes up. The press position, so the cursor reappears where the
    /// gesture started rather than wherever the warping left it.
    capture_anchor: Option<(f64, f64)>,
    /// How far the pointer has travelled since each button went down.
    ///
    /// **Distance travelled, not the distance between press and release**,
    /// which is what separates a click from a look now that a captured pointer
    /// is warped back to its anchor: measured end to end, every drag would
    /// finish exactly where it started and so would be a click. Accumulating
    /// is also the more honest question -- a gesture that circles the camera
    /// and comes back is a drag by any reading.
    left_travel: f64,
    right_travel: f64,
    error: Option<String>,
    keys: KeyState,
    last_frame: Instant,
    /// The height the camera is currently orbiting around, easing towards the
    /// character's own -- see [`App::camera_follow_z`]. `None` until the first
    /// frame places it.
    camera_z: Option<f32>,
    /// When the client started, which is the only clock the weather has.
    started: Instant,
    frame_ms: f32,
    /// Selected sequence, or `None` for the bind pose.
    anim: Option<usize>,
    /// Elapsed time within the current sequence.
    anim_time_ms: u32,
    playing: bool,
    speed: f32,
    /// Present when the world was entered over the network.
    live: Option<live::LiveWorld>,
    /// The movement state currently reported to the server. Compared against
    /// what the keys say each frame; the difference is what has to be sent.
    live_move: ::world::motion::Motion,
    /// The jump in progress, if the character is off the ground.
    ///
    /// The server does not simulate the arc -- it is told the take-off and the
    /// landing and believes the client in between -- so this is the only copy
    /// of it, and it is why the landing has to be sent explicitly.
    jump: Option<::world::motion::Jump>,
    /// Whether the character is currently swimming, and in what.
    ///
    /// **A remembered state rather than a fresh test each frame**, because the
    /// server has to be *told* when it changes -- `MSG_MOVE_START_SWIM` and
    /// `MSG_MOVE_STOP_SWIM` are transitions, not a flag polled from nowhere,
    /// and a client that recomputed the condition without comparing it to what
    /// it last reported would either send nothing or send it every frame.
    ///
    /// Carries the liquid rather than a bare bool so the interface can say
    /// *what* is being swum in without asking the world a second time and
    /// possibly getting a different answer -- the character has moved by then.
    swimming: Option<world::Liquid>,
    /// Standing in liquid too shallow to swim in.
    ///
    /// Its own state rather than derived from [`App::swimming`], which is
    /// `None` for both a character on dry land and one wading a ford -- and
    /// those want opposite footstep sounds. `FootstepTerrainLookup` carries a
    /// splash column beside every ordinary one for exactly this.
    /// What the surface holding the character up is made of, as a
    /// `TerrainType` row id, when that surface is a building's floor.
    ///
    /// `None` outdoors, which is most of the time -- and then the footstep
    /// falls back to asking the terrain. Kept as state rather than queried
    /// where it is needed, because it has to be decided by **the same
    /// comparison that decides where the character stands**: floor and terrain
    /// are compared once, and asking again later could break the tie the other
    /// way and put a character on the floorboards hearing the ground beneath
    /// them.
    floor_material: Option<u8>,
    wading: bool,
    /// Where the player's own cycle was last time footsteps were checked, as
    /// `(sequence, milliseconds into it)`.
    ///
    /// Kept because a footfall is a *crossing*, not a state: the question each
    /// frame is which of the cycle's footfall timestamps the clock passed since
    /// the last reading, and with no previous reading there is nothing to have
    /// crossed. A changed sequence resets it rather than firing, or every
    /// change of gait would stamp a step at whatever moment the new cycle
    /// happened to be entered at.
    footstep_phase: Option<(usize, u32)>,

    /// Sustained forward movement, toggled rather than held.
    ///
    /// Cleared by pressing a movement key, which is what every game with an
    /// autorun does: a player who grabs the keys to dodge something should not
    /// have to remember to switch it off first.
    autorun: bool,
    last_heartbeat: Instant,
    last_ping: Instant,
    /// When the loot method was last changed. `None` until the first change
    /// this session -- see `App::cycle_loot_method`'s doc comment for why a
    /// cooldown exists at all: a realm was observed dropping the connection
    /// outright after several `CMSG_LOOT_METHOD`s sent in quick succession,
    /// the same class of failure `PING_INTERVAL` exists to avoid on a
    /// different opcode.
    last_loot_method_change: Option<Instant>,
    /// The `undrawable` count last logged, so entity rebuilding (which now
    /// runs every frame) warns once per change instead of every frame for the
    /// rest of the session.
    last_undrawable_warned: usize,
    /// Whether the player's own body was in the list handed to the renderer
    /// last frame, so its disappearance is reported **once, when it happens**.
    ///
    /// A character that has gone invisible is two completely different faults
    /// wearing one report: either the body stopped being submitted -- our own
    /// object left replicated state, or lost its display id -- or it was
    /// submitted and something downstream did not draw it. Those want opposite
    /// investigations and look identical from the window, which is the shape
    /// this project keeps paying for. Starts `true`, so a body that is never
    /// drawn at all says so on the first frame.
    own_body_drawn: bool,
    /// Which replicated object is this character's current corpse, and where.
    ///
    /// Resolved once per frame and kept, because the bracket drawn round the
    /// body and the request that asks for it back must name the same object --
    /// and picking it is not trivial: a graveyard is full of corpse-shaped
    /// objects, including the *bones* of bodies already reclaimed, which carry
    /// the same owner guid as the current one.
    own_corpse: Option<(u64, glam::Vec3)>,
    /// Composed looks for *other* players, keyed by `character::Appearance`.
    ///
    /// Cached because resolving one reads several DBCs and composes a skin
    /// texture from its layers, and entities are rebuilt every frame -- doing
    /// that per frame per player would be the thirty-seven-second login all
    /// over again, at sixty hertz. Keyed by appearance rather than by guid so
    /// two players who look alike share one entry, which is also exactly the
    /// key the renderer's model cache uses.
    player_looks: std::collections::HashMap<u64, Rc<character::Look>>,
    /// The world's lighting tables, read once when there is something to light.
    lighting: Option<dbc::light::Lighting>,
    /// `--map`'s `Map.dbc` id, so `--hour` lights an offline world too.
    offline_map: Option<u32>,
    /// The player's own interface: where every frame sits, what it looks like,
    /// and whether it is currently being rearranged.
    hud: ui::Hud,
    /// What this client has selected, and has told the server it has.
    target: Option<u64>,
    /// Where the left button went down, so a click can be told from the drag
    /// that turns the camera. Both arrive as the same pair of events.
    press_at: Option<(f64, f64)>,
    /// The same for the right button, which has the same two gestures on it:
    /// dragged it steers the character, clicked it selects and attacks.
    right_press_at: Option<(f64, f64)>,
    /// How far the camera has been swung around the character by dragging,
    /// added to the character's own facing rather than replacing it. Only
    /// meaningful while following a live character.
    camera_yaw_offset: f32,
    /// How far the camera is tilted above or below its subject, accumulated by
    /// dragging.
    ///
    /// Owned here rather than written straight into the camera for the same
    /// reason the yaw offset is: `drive_live_movement` rebuilds the camera from
    /// the character every frame, so anything written directly is overwritten
    /// before it is ever drawn.
    camera_pitch: f32,
    /// Whether the *right* button is down, which steers the character rather
    /// than the camera.
    ///
    /// The two drags are deliberately different verbs, as they are in the game
    /// this is modelled on: a left drag swings the camera and leaves the
    /// character facing where it was, a right drag turns the character and the
    /// camera comes along because it follows. Collapsing them into one would
    /// lose the ability to look sideways while running straight.
    steering: bool,
    /// How far behind the character the camera sits, in world units, set by
    /// the wheel. Only meaningful while following a live character -- a free
    /// camera has no subject to be a distance from, so the wheel trims its
    /// speed instead.
    camera_distance: f32,
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
    /// The action slot most recently activated, and when -- a brief flash so
    /// a *click* has something to show for itself.
    ///
    /// Deliberately not a claim that the cast landed, or even that it was
    /// sent: it is the same affordance retail gives a button the instant it
    /// is pressed, nothing more. It exists because an instant-cast spell has
    /// no cast bar (there is no cast *time* to show one for) and this
    /// realm's cooldown sweep does not reliably start either -- see
    /// `SpellCooldown`'s doc comment in `crates/world/src/spell.rs` -- so
    /// without this, pressing a key for an instant ability that is not on
    /// cooldown produces no visible response at all, silent success reading
    /// identical to a dropped keypress.
    action_flash: Option<((usize, usize), Instant)>,
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
    /// Which of [`STRAFE_YAW_CHOICES`] the drawn body leans by while
    /// sidestepping, cycled with `F4`.
    ///
    /// **The cycle question was asked four times and the answer was never a
    /// cycle.** Both of the two lateral options a land model has were shown at
    /// the window and both were rejected, correctly: the shuffles read as
    /// standing still with the feet moving, and the run read as *"the exact
    /// same way as W"*. What the original does is play that same run with the
    /// **model turned**, so the legs are seen from the side. There was never a
    /// third animation to find.
    ///
    /// How far it turns is not in any table -- it is a rendering decision --
    /// so this is the `F2` treatment applied to the number instead of to the
    /// choice: one key, one variable, the person at the window pressing it
    /// while sidestepping. Movement is untouched at every setting.
    strafe_yaw_choice: usize,
    /// The line being typed, or `None` when not typing. While this is `Some`,
    /// keys are text rather than movement.
    composing: Option<String>,
    /// Which channel an unprefixed line goes to next.
    ///
    /// **Sticky, and shown in the composing line for exactly that reason.**
    /// Switching it (`/p` alone, `/w Name` alone) changes every line typed
    /// after it until switched back -- a client that changed this silently
    /// would say a private line out loud, or a party line to nobody, the
    /// moment the player forgot which mode they were in.
    chat_channel: ChatChannel,
    /// Spell names and icons, loaded from the archives if there are any.
    spells: spells::Spellbook,
    /// Whether the bars have been filled from the spellbook yet. The spellbook
    /// arrives in the login burst, so this cannot happen at construction.
    bars_seeded: bool,
    /// Whether the spellbook is open. Runtime state rather than a field in
    /// `ui.toml`: where the book sits is a layout decision worth saving, and
    /// whether it happened to be open when the client last closed is not.
    spellbook_open: bool,
    /// Item icons, loaded from the archives if there are any.
    items: items::Items,
    /// World map pages and their art.
    maps: maps::Maps,
    /// The flight network from the client's own tables. Loaded once: three
    /// DBCs that never change, and the answer to "where can I fly" is a
    /// lookup rather than a scan.
    taxi_network: taxi::Network,
    /// How far the minimap sees, in world units across the disc.
    ///
    /// **Live state seeded from the style, and deliberately not written back**
    /// -- exactly what `camera_distance` is and for the same reason: the wheel
    /// must not rewrite a saved setting on every notch. `minimap_range` in
    /// `ui.toml` is where the disc *starts*, which is the thing a person
    /// editing a config file means.
    minimap_range: f32,
    /// The minimap's tile index and the art it has uploaded so far.
    ///
    /// **No open flag beside it, unlike the map.** A minimap is one of the
    /// frames that is simply there, so there is nothing to toggle and nothing
    /// that could disagree about whether it should be on screen.
    minimap: minimap::Minimap,
    /// Whether the map is open. Runtime state, like the spellbook: where the
    /// window sits is worth saving and whether it was open at exit is not.
    map_open: bool,
    /// Where the realm says the log's objectives are. Held even while the map
    /// is shut, for the same reason the quest cache is: what the log holds is
    /// what decides which ids to ask about, and closing a window is not a
    /// reason to throw away an answer already paid for.
    objectives: maps::Objectives,
    /// What mark belongs over each NPC's head, by guid, as the server last
    /// said.
    ///
    /// **Kept per guid rather than per creature entry.** Two Deputy Willems
    /// standing side by side can carry different marks -- the quest is on one
    /// of them as far as the server is concerned -- and more importantly the
    /// answer is about *this character's* progress, so it changes under a
    /// fixed entry as quests are taken and handed in.
    quest_marks: std::collections::HashMap<u64, ::world::quest::QuestgiverMark>,
    /// Guids a status request has gone out for and when, so a talker is asked
    /// about once rather than once a frame, and an unanswered request is
    /// eventually sent again.
    quest_marks_asked: std::collections::HashMap<u64, Instant>,
    /// The quest log the marks were asked against. **Every mark is stale the
    /// moment the log changes** -- accepting a quest turns its giver's
    /// exclamation into nothing and its ender's nothing into a question mark,
    /// and neither NPC sends anything to say so.
    quest_marks_log: Vec<u32>,
    /// The sound tables, and the two channels that play them.
    ///
    /// The output stream is held for its whole life on purpose: dropping it
    /// stops every sound, which is the sort of bug that presents as "audio
    /// works for one frame". `None` when no audio device could be opened at
    /// all, which is a perfectly ordinary state on a machine with no sound
    /// card and must not stop the client.
    /// The last area the player was known to be in.
    ///
    /// Kept because `area_at` answers `None` while a tile streams in, and a
    /// zone change is not the same event as a tile not being loaded yet.
    area: Option<u32>,
    sounds: sound::Sounds,
    /// One-shot combat sounds, and the ids waiting to be fired.
    ///
    /// Queued rather than played where they are noticed, because the swing
    /// loop holds `self.live` borrowed and playing needs the archive chain.
    /// Drained once per frame in `update_sound`.
    effects: sound::Effects,
    /// `(sound id, is an impact)`. Impacts are held back to land with the
    /// blade; voices fire at once -- a creature's yelp is its *reaction* and
    /// the packet is already the moment it reacted.
    pending_sounds: Vec<(u32, bool)>,
    /// How long an impact sound waits, in milliseconds.
    ///
    /// Runtime state rather than a fixed argument because the right value is
    /// found *by ear against the animation* and nothing else can tell you it
    /// -- so it is adjustable in play with `[` and `]`, which turns several
    /// rebuild-and-relaunch cycles into one session. Seeded from
    /// `--impact-delay-ms`.
    impact_delay_ms: u64,
    /// Who was already attacking last frame.
    ///
    /// **Derived from the replicated map rather than from an event**, which is
    /// the pattern this project's notes recommend: `SMSG_ATTACKSTART` is a
    /// statement made once and its count is all that survives folding, where
    /// `WorldState::attacking` says who is fighting whom for as long as it is
    /// true. A guid that is in it now and was not before has just noticed
    /// somebody.
    attackers: std::collections::HashSet<u64>,
    audio: Option<rodio::OutputStream>,
    music: sound::Channel,
    ambience: sound::Channel,
    /// Whether the bag window is open, on the same reasoning as
    /// `spellbook_open`.
    bags_open: bool,
    /// Whether the character panel is open.
    character_open: bool,
    /// Whether the quest log is open.
    quest_log_open: bool,
    /// Which quest the log has highlighted, if any. Interface state, so it
    /// lives here rather than in the layout file.
    selected_quest: Option<u32>,
    /// What the server has said about which quests, kept between sessions.
    ///
    /// **Held even when the log is shut**, because the log's contents are what
    /// tell us which ids to ask about, and closing a window is not a reason to
    /// forget answers already paid for.
    quests: ::world::QuestCache,
    /// Where that cache is written. `None` when no configuration directory
    /// could be found, in which case the cache still works for the session and
    /// simply is not persisted -- the same trade the layout file makes.
    quest_cache_path: Option<std::path::PathBuf>,
    /// When each outstanding quest query was sent, so one that is never
    /// answered can be given up on rather than counted as pending forever.
    quest_asked_at: std::collections::HashMap<u32, std::time::Instant>,
    /// The questgiver currently being talked to, or `None`.
    ///
    /// **Existence is the flag**, as it is for the loot window: this is set
    /// when a greeting is sent and cleared when the player closes it or walks
    /// out of range, and there is no separate boolean that could disagree.
    questgiver: Option<Questgiver>,
    /// The trainer list currently open, or `None`.
    ///
    /// **Existence is the flag**, like the questgiver above it -- and it is a
    /// *separate* field rather than a variant of one "NPC window" state,
    /// because a class trainer is usually a questgiver too. Llane Beshere
    /// carries both bits, so one right-click legitimately opens both windows,
    /// and folding them into one state would make the second arrival close
    /// the first.
    ///
    /// Cleared by the questgiver window's Close button, since the same click
    /// opened both, and replaced whenever another NPC is greeted.
    trainer: Option<TrainerSession>,
    /// The taxi flight currently being flown, or `None`.
    ///
    /// **While this is `Some`, the server owns the character's position** and
    /// this client's whole movement path stands down -- see
    /// [`App::drive_live_movement`], which returns early rather than applying
    /// input, collision, ground height or buoyancy. That is deliberately a
    /// replacement rather than a set of exceptions: a flight is a different
    /// mode of being, and adding `&& flight.is_none()` to five separate
    /// conditions is how one of them gets missed.
    flight: Option<ActiveFlight>,
    /// The flight master currently being talked to, and its menu once it
    /// arrives. Existence is the flag, like the trainer's.
    taxi: Option<TaxiSession>,
    /// When the quest cache was last written.
    ///
    /// **Saving only on a clean exit is not enough**, and the failure is
    /// silent: a crash, an alt-F4 the window manager does not deliver, or a
    /// kill from a terminal all lose every answer the session paid for, and
    /// the next launch simply asks again with nothing to say it ever knew.
    /// A periodic write costs one file per half minute of *new* discoveries
    /// and nothing at all once a realm's quests are known.
    quest_saved_at: Instant,
    /// The corpse this client has asked about, and whether the answer arrived.
    ///
    /// **The two states have to be distinguished, and the first version did
    /// not.** It kept only a guid and released the corpse whenever replicated
    /// state held no loot -- which is true on every frame between asking and
    /// being answered, so the release went out a frame after the request and
    /// the window never appeared. The log said `asked to loot` ten times and
    /// looked like a server that ignored the request.
    ///
    /// What this must *not* become is a second copy of "is the window open".
    /// That is decided entirely by whether the server sent anything, and lives
    /// in replicated state; this only records what we asked and whether we are
    /// still waiting, which is a fact about this client alone.
    looting: Option<Looting>,
    /// Whether this client has already asked the server for its own corpse
    /// since last becoming a ghost.
    ///
    /// Asked once per release rather than every frame, the same shape as
    /// `looting`: the answer arrives asynchronously into
    /// `WorldState::corpse_location`, and re-asking on every frame a corpse
    /// run happens to take would be one query packet per frame instead of
    /// one per death.
    own_corpse_query_sent: bool,
    /// Held modifiers, which choose which bar a number key drives.
    modifiers: winit::keyboard::ModifiersState,
}

/// Where a loot request has got to.
///
/// Two states rather than one guid, because "asked" and "open" need opposite
/// handling and look identical from replicated state alone -- neither has any
/// loot in it. See [`App::looting`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Looting {
    /// Sent, and no answer yet. Releasing now would close a corpse that was
    /// never opened, which is what the first version of this did every frame.
    Asked(u64),
    /// The server answered with something, so a window is showing. When it
    /// stops showing, this is the corpse to release.
    Open(u64),
}

/// The equipment slot a weapon swings from.
///
/// Named rather than written as `15` at the call site: the slot vocabulary was
/// measured against a live realm and the number means nothing without that.
/// See `world::inventory::InventorySlot::label`.
const MAIN_HAND_SLOT: usize = 15;

/// How far the pointer may travel between press and release and still count as
/// a click rather than as a look.
const CLICK_SLOP: f64 = 4.0;

/// Whether a gesture that moved this far was a click.
///
/// **Distance travelled, not the distance between press and release**, and the
/// difference is the whole reason this is a function. While a drag holds the
/// pointer the camera is turned by raw device movement and the pointer itself
/// does not go anywhere, so every drag would end exactly where it began --
/// measured end to end, a full turn of the camera is a click on whatever was
/// under the cursor when it started.
fn was_click(travel: f64) -> bool {
    travel <= CLICK_SLOP
}

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
/// `pace` is the caller's own movement state -- forward and lateral speed --
/// rather than anything read back from the server. See [`live::own_entity`].
fn drawable_with_own(
    live: &live::LiveWorld,
    pace: (f32, f32),
    // How far to turn the drawn body towards where it is going -- see
    // [`strafe_yaw`]. Added to the orientation here rather than to
    // `live.orientation`, which is what movement and every outgoing packet
    // are computed from.
    lean: f32,
    airborne: bool,
    swimming: bool,
) -> Vec<live::Entity> {
    let mut entities = live::drawable_entities(&live.state, live.guid, live.position);
    if let Some(own) = live::own_entity(
        &live.state,
        live.guid,
        live.position,
        live.orientation + lean,
        pace.0,
        pace.1,
        airborne,
        swimming,
    ) {
        entities.push(own);
    }
    entities
}

/// The lighting in force for a camera, if the world can say.
///
/// Wants three things: the tables, a map and position to choose a light for,
/// and an hour. The hour comes from the realm's own clock -- run forward from
/// when it was reported, since the server says it once -- unless `--hour`
/// overrides it.
fn resolve_lighting(
    lighting: Option<&dbc::light::Lighting>,
    live: Option<&live::LiveWorld>,
    // What the sky is doing. Resolved by `frame_weather` rather than read off
    // `live` here, so the storm blend and the falling drops cannot come from
    // different states -- see that function.
    weather: ::world::WeatherChange,
    // The map to light when there is no connection -- `--map`'s directory
    // resolved through `Map.dbc`. See `offline_map_id`.
    offline_map: Option<u32>,
    override_hour: Option<f32>,
    at: glam::Vec3,
) -> Option<(dbc::light::Sample, f32)> {
    let lighting = lighting?;
    // **Without a realm there is still a map and an hour**, and `--hour`'s
    // whole purpose is to look at a curve without waiting for it. It used to
    // resolve nothing at all offline: the flag parsed, the help text promised,
    // and an offline screenshot silently got the fallback gradient -- which is
    // a perfectly plausible sky, so nothing announced that the tables had not
    // been consulted. That cost a look at this milestone's first render.
    let (map_id, storm, hour) = match live {
        Some(live) => {
            let hour = match override_hour {
                Some(hour) => hour,
                None => {
                    let (time, learned) = live.state.game_time?;
                    let now = time.advanced(learned.elapsed());
                    now.minute_of_day() as f32 / 60.0
                }
            };
            (live.map_id, storm_of(weather), hour)
        }
        // Offline the hour has to be asked for: there is no clock to read, and
        // defaulting to noon would light every model view as an outdoor scene.
        None => (offline_map?, storm_of(weather), override_hour?),
    };
    let minute_of_day = (hour * 60.0).rem_euclid(1440.0) as u32;
    let sample = lighting.sample_in(map_id, at.x, at.y, minute_of_day, storm)?;
    Some((sample, hour))
}

/// The `Map.dbc` id of the map directory `--map` named, if it named one.
///
/// `Light.dbc` keys on the numeric id and the command line takes the folder
/// name, which is the only place the two ever have to be connected.
fn offline_map_id(chain: &mut Chain, directory: Option<&str>) -> Option<u32> {
    let directory = directory?;
    let bytes = chain.read(dbc::schema::Map::PATH).ok()?;
    let maps = dbc::schema::Map::parse(&bytes).ok()?;
    let id = maps
        .iter()
        .find(|row| row.directory().eq_ignore_ascii_case(directory))
        .map(|row| row.id());
    id
}

/// What to clear the sky to before the gradient is drawn over it.
///
/// **A safety net rather than the sky.** `SkyRenderer` covers every pixel of
/// the frame, so in the ordinary case this colour is never seen; it exists so
/// that a viewport the sky pass somehow misses reads as the horizon rather than
/// as whatever the last frame left there.
fn sky_colour(
    sky: &render::SkyRenderer,
    lighting: Option<&(dbc::light::Sample, f32)>,
) -> wgpu::Color {
    let horizon = lighting.map_or(
        dbc::light::DEFAULT_SKY[dbc::light::bands::HORIZON],
        |(sample, _)| sample.horizon(),
    );
    // Through the renderer's own conversion: a clear colour is written to the
    // target by the same rules a shader's output is, so if one of the two needs
    // undoing the encode then so does the other.
    let [r, g, b] = sky.encode(horizon);
    wgpu::Color { r: r as f64, g: g as f64, b: b as f64, a: 1.0 }
}

/// What is coming down this frame, and how hard.
#[derive(Clone, Copy, Debug)]
struct Falling {
    kind: render::precipitation::Kind,
    /// 0 to 1, as `SMSG_WEATHER` reports it.
    intensity: f32,
    /// Seconds since the client started, which is the field's only clock.
    seconds: f32,
}

/// How far towards the stormy curves a weather state takes the lighting.
///
/// A zone with no weather reports clear and blends nothing, so the common case
/// costs one comparison.
fn storm_of(weather: ::world::WeatherChange) -> f32 {
    if weather.weather.is_storm() {
        weather.intensity
    } else {
        0.0
    }
}

/// The weather in force this frame: `--weather` if given, else the realm's.
///
/// **One function, because two consumers must agree.** The lighting blends
/// towards the storm curves and the precipitation decides what falls, and a
/// client that greyed the sky from the server while raining from the command
/// line would be showing a state that exists nowhere. Same reasoning as
/// unprojecting the picking ray from the matrix the scene was drawn with.
fn frame_weather(live: Option<&live::LiveWorld>, args: &Args) -> ::world::WeatherChange {
    if let Some(raw) = args.weather {
        return ::world::WeatherChange {
            weather: ::world::Weather::from_raw(raw),
            intensity: args.weather_intensity.clamp(0.0, 1.0),
            abrupt: true,
        };
    }
    live.map(|live| live.state.weather).unwrap_or_default()
}

/// Whether anything is falling.
///
/// **Deliberately not `Weather::is_storm`.** That predicate exists to choose
/// between two sets of light curves and counts fog as a storm, which is right
/// for lighting and would rain on a misty morning here. See
/// `world::Weather::precipitation`.
fn resolve_precipitation(weather: ::world::WeatherChange, seconds: f32) -> Option<Falling> {
    let kind = match weather.weather.precipitation() {
        ::world::Precipitation::Rain => render::precipitation::Kind::Rain,
        ::world::Precipitation::Snow => render::precipitation::Kind::Snow,
        ::world::Precipitation::None => return None,
    };
    Some(Falling {
        kind,
        intensity: weather.intensity,
        seconds,
    })
}

/// What colour to draw a drop.
///
/// **Chosen, like everything else about a raindrop** -- `SMSG_WEATHER` sends a
/// state and an intensity and says nothing about how the water looks. It is
/// the horizon colour lightened towards white: taking the hour's own colour is
/// what stops rain being a grey overlay pasted on a golden dusk, and lightening
/// it is what keeps a streak visible against ground painted in that very
/// colour by the fog.
fn drop_colour(sky: &render::SkyRenderer, lighting: Option<&(dbc::light::Sample, f32)>) -> [f32; 3] {
    let horizon = lighting.map_or(
        dbc::light::DEFAULT_SKY[dbc::light::bands::HORIZON],
        |(sample, _)| sample.horizon(),
    );
    sky.encode(horizon.map(|c| c + (1.0 - c) * 0.35))
}

/// The gradient to hand the sky pass: the world's own, or the fixed fallback.
fn sky_gradient(lighting: Option<&(dbc::light::Sample, f32)>) -> render::sky::Gradient {
    lighting.map_or(dbc::light::DEFAULT_SKY, |(sample, _)| sample.sky)
}

/// Where the sun is at a given hour, as a direction *towards* it.
///
/// **Chosen, not measured.** No table in the client carries a sun position:
/// `Light.dbc` and its bands describe colours over time and say nothing about
/// direction, so this is a plausible arc rather than a reconstruction of the
/// original. It rises at 06:00, is overhead at noon, sets at 18:00 and spends
/// the night below the horizon, where the dot product clamps to zero and only
/// the ambient term remains -- which is what makes night look like night
/// without a second code path.
///
/// The axis it swings along is likewise a choice. Getting it wrong tilts
/// shadows the wrong way, which is visible and fixable; the thing that would
/// *not* be visible is claiming this came from the data.
fn sun_direction(hour: f32) -> glam::Vec3 {
    let t = (hour - 6.0) / 12.0 * std::f32::consts::PI;
    glam::Vec3::new(0.0, -t.cos(), t.sin()).normalize_or_zero()
}

/// The camera uniform with real lighting folded in, or the placeholder when
/// there is none.
fn lit_uniform(
    camera: &Camera,
    aspect: f32,
    // Only for its colour conversion -- see the fog term below.
    sky: &render::SkyRenderer,
    lighting: Option<(dbc::light::Sample, f32)>,
) -> render::mesh::CameraUniform {
    let mut uniform = camera.uniform(aspect);
    let Some((sample, hour)) = lighting else {
        return uniform;
    };
    let sun = sun_direction(hour);
    uniform.light = [sun.x, sun.y, sun.z, 0.0];
    // `w` carries "there is light data", which is what the shaders switch on.
    // **The same undoing of the sRGB encode as the sky, and for a reason that
    // can be derived rather than judged.** These two are a pure multiplier on
    // a texel: `shade` computes `texel.rgb * (ambient + sun * ndl)`. The
    // original client multiplied a texture *byte* by a light *byte* into an
    // 8-bit framebuffer, so its result was `T * L / 255` in display units.
    // Here the texture is decoded to linear on sample and the result is
    // re-encoded on write, so matching that requires a factor of `(L/255)^2.2`
    // -- which is exactly `to_linear`. Being faithful to the original and
    // being physically right turn out to be the same answer, which is the only
    // reason this is a change and not a preference.
    //
    // It was visible before it was derived: with the sky corrected and these
    // two left raw, the world stayed bright under a dusk sky and the two read
    // as disagreeing about the hour.
    let sun = sky.encode(sample.diffuse);
    let ambient = sky.encode(sample.ambient);
    uniform.sun = [sun[0], sun[1], sun[2], 1.0];
    uniform.ambient = [ambient[0], ambient[1], ambient[2], 0.0];
    // The horizon, not a band of its own -- see `dbc::light::bands::HORIZON`.
    // Distant terrain fades into the colour of the sky it meets, which is the
    // one thing about fog that cannot be got wrong by construction -- provided
    // both are written to the target in the same space. Fog is *mixed towards*
    // rather than multiplied by, so at full distance the pixel is this colour
    // exactly, and it needs the same undoing of the sRGB encode the sky does.
    // Without it the far hills come out brighter than the sky they meet, which
    // would look like the two disagreeing about the horizon.
    let fog = sky.encode(sample.fog());
    uniform.fog = [fog[0], fog[1], fog[2], 0.0];
    uniform.fog_range = [sample.fog_start, sample.fog_end, 0.0, 0.0];
    uniform
}

/// How another player looks, composing it once and remembering it.
///
/// A free function rather than a method on `App` because the caller holds a
/// mutable borrow of the renderer and an immutable one of the live world at the
/// same time; taking the cache and the archive chain as separate arguments
/// keeps those borrows disjoint, where `&mut self` would collide with both.
///
/// Returns the key as well as the look because the renderer's model cache is
/// keyed on it: two humans with different faces must not share one built model,
/// which is the bug `EntityPlacement::look_key` exists to prevent.
/// Resolves and caches another player's look, gear included.
///
/// `visible_items` are raw item entries off the wire -- see
/// `world::state::Entity::visible_item_entries` -- and `items` is what turns
/// each into the `(display id, inventory type)` pair `resolve_wearing` wants,
/// via the same `Item.dbc` pass the bag window already uses for icons. An
/// entry with no row (nothing worn, or an id `Item.dbc` does not have) drops
/// out rather than fabricating a display id -- the same rule the description
/// substituter follows.
///
/// The cache key folds equipment in through [`character::look_key`], not
/// just the face: two players sharing a race and appearance but not a
/// wardrobe must not share a composed skin.
fn player_look(
    cache: &mut std::collections::HashMap<u64, Rc<character::Look>>,
    chain: &mut Chain,
    items: &crate::items::Items,
    appearance: ::world::Appearance,
    visible_items: &[u32],
) -> (Rc<character::Look>, u64) {
    let appearance = character::Appearance::from(appearance);
    let equipment: Vec<(u32, u8)> = visible_items
        .iter()
        .filter_map(|entry| (*entry != 0).then(|| items.display(*entry)).flatten())
        .collect();
    let key = character::look_key(&appearance, &equipment);
    let look = cache
        .entry(key)
        .or_insert_with(|| {
            let look = character::resolve_wearing(chain, appearance, &equipment);
            tracing::debug!(
                "composed a look for {appearance:?}: body {:?}, hair {:?}, wearing {}",
                look.body,
                look.hair,
                equipment.len()
            );
            Rc::new(look)
        })
        .clone();
    (look, key)
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
    SpellHit(::world::combat::SpellDamage),
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
/// The NPC currently being talked to, and what it has said.
///
/// **Holds ids and not text.** Every string this produces on screen comes from
/// the quest cache -- answers to `CMSG_QUEST_QUERY`, verified against the
/// realm's own tables -- so there is one copy of a quest's title in this
/// client rather than two that could disagree. See
/// `world::quest::parse_questgiver_details` for why the questgiver's own text
/// packets are read for their quest id and nothing else.
/// An open conversation with a flight master.
///
/// Holds the **menu as sent**, for the reason the trainer session does:
/// the known-node mask and the departure node were both decided by the
/// server for this character and neither can be recomputed here. The
/// departure node in particular is one the client would get *wrong* --
/// live, the server named a node 573 units away where the nearest is 150.
struct TaxiSession {
    npc: u64,
    /// `None` between the request and the reply.
    menu: Option<::world::TaxiMenu>,
}

/// A taxi flight in progress.
///
/// The route and the clock are kept apart on purpose: [`::world::Flight`] is
/// pure geometry and is unit-tested without a window, and the only thing this
/// wrapper adds is *when it started*, which is the one part that needs a
/// running program.
struct ActiveFlight {
    route: ::world::Flight,
    started: Instant,
    /// What the character was facing when it took off, so the landing can put
    /// it back rather than leaving it pointing along the last leg. Cheap to
    /// keep and impossible to recover afterwards.
    orientation_before: f32,
}

/// An open conversation with a trainer.
///
/// Holds the **parsed list as sent** rather than a rebuilt view, which is the
/// opposite of what [`Questgiver`] does and deliberately so. A questgiver's
/// window is rebuilt every frame because its inputs -- the quest cache and the
/// log -- change underneath it. A trainer's list cannot: every field in it,
/// including the per-row availability and the discounted price, was computed
/// by the *server* for this character at the moment it was asked, and this
/// client has no way to recompute any of it. So it is kept, and re-asked when
/// something might have changed it.
struct TrainerSession {
    npc: u64,
    /// Resolved at request time, like the questgiver's: an NPC's name cannot
    /// change while you are talking to it.
    name: String,
    /// `None` between the request and the reply.
    ///
    /// Drawn as an empty window rather than as nothing, for the reason the
    /// questgiver window opens on the send: a window that appeared only once
    /// the reply arrived would make a slow realm look like a click that did
    /// not register.
    list: Option<::world::TrainerList>,
}

struct Questgiver {
    npc: u64,
    /// Resolved at greeting time rather than per frame: an NPC's name cannot
    /// change while you are talking to it.
    name: String,
    /// What the greeting said this NPC has, in the order it said it.
    offered: Vec<u32>,
    /// The one quest whose text is on screen, if the player has picked one or
    /// the server volunteered it.
    showing: Option<u32>,
}

impl Questgiver {
    /// Files what a greeting produced.
    ///
    /// **An empty menu is a real answer**, and the commonest one: an NPC with
    /// nothing for this character sends a gossip message with no options and
    /// no quests. The window stays open saying so rather than vanishing, which
    /// would be indistinguishable from a click that never registered.
    ///
    /// A method on this rather than on `App` so it borrows one field: the
    /// pump holds the connection mutably for its whole length, and a `&mut
    /// self` on the app would collide with it.
    fn note_gossip(&mut self, gossip: &::world::Gossip) {
        // Ignore an answer from an NPC other than the one being talked to --
        // two greetings can be in flight if the player clicks twice, and the
        // later window must not be filled by the earlier reply.
        if gossip.npc != self.npc {
            return;
        }
        self.offered = gossip.quests.iter().map(|quest| quest.quest_id).collect();
        // One quest and nothing else to choose between: show it straight away
        // rather than making the player click a list of one.
        if self.offered.len() == 1 {
            self.showing = self.offered.first().copied();
        }
    }

    /// The server put one quest's scroll on screen without being asked.
    fn note_quest_offered(&mut self, quest: u32) {
        if !self.offered.contains(&quest) {
            self.offered.push(quest);
        }
        self.showing = Some(quest);
    }
}

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
/// One `SMSG_PARTY_COMMAND_RESULT` as a line for the scrollback.
///
/// The operation and the code are both named only where this client has
/// actually seen them, and both fall back to their number -- the same rule
/// `world::group::describe_party_result` follows and for the same reason: a
/// wrong offset fails loudly, a wrong *name* for a status code never does.
///
/// The member's name is put in only when the packet carried one. The server
/// echoes back the name that was asked for, which is what makes a refused
/// invite say *who* it was refused for -- and it is empty for the operations
/// that are not about anybody.
fn describe_party_result(result: &::world::PartyCommandResult) -> String {
    let operation = match result.operation {
        ::world::PartyOperation::INVITE => "invite",
        ::world::PartyOperation::UNINVITE => "removal",
        ::world::PartyOperation::LEAVE => "leaving",
        _ => "group request",
    };
    let outcome = ::world::group::describe_party_result(result.result);
    if result.member.is_empty() {
        format!("{operation}: {outcome}")
    } else {
        format!("{operation} of {}: {outcome}", result.member)
    }
}

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
        // Read before `args` is moved into the struct below.
        let impact_delay_ms = args.impact_delay_ms;
        // Read before the window exists, so a layout that fails to parse is
        // reported at startup rather than at the first frame -- and bound here
        // rather than inline because the camera's starting distance is one of
        // the settings it carries.
        let hud = ui::Hud::load();
        // Read at startup rather than with the spellbook: the two tables the
        // map needs are small and, unlike a spellbook, they do not depend on
        // anything the server sends -- so `M` works before the character has
        // finished logging in, and the arrow simply has nowhere to be yet.
        let mut chain = chain;
        let maps = maps::Maps::load(&mut chain);
        let taxi_network = taxi::Network::load(&mut chain);
        // Read at startup for the same reason: 1.5MB of text naming every
        // tile's picture, which does not depend on the server and would
        // otherwise be parsed on the frame the player first looks at it.
        let minimap = minimap::Minimap::load(&mut chain);
        Self {
            // The saved preference, which the wheel then moves from. Kept as
            // live state rather than read from the profile every frame: the
            // wheel must not rewrite a saved setting on every scroll.
            camera_distance: hud.profile.camera.start_distance(),
            minimap_range: hud.profile.style.minimap_range,
            hud,
            args,
            chain,
            window: None,
            renderer: None,
            camera: Camera::Orbit(Orbit::default()),
            keys: KeyState::default(),
            dragging: false,
            last_cursor: None,
            cursor_captured: false,
            capture_anchor: None,
            left_travel: 0.0,
            right_travel: 0.0,
            error: None,
            last_frame: Instant::now(),
            camera_z: None,
            // The weather's own clock. Separate from `last_frame` because that
            // one is reset every frame, and a falling drop needs a monotone
            // total rather than a delta.
            started: Instant::now(),
            frame_ms: 0.0,
            anim: None,
            anim_time_ms: 0,
            playing: true,
            speed: 1.0,
            live: None,
            live_move: ::world::motion::Motion::default(),
            jump: None,
            swimming: None,
            floor_material: None,
            wading: false,
            footstep_phase: None,
            autorun: false,
            last_heartbeat: Instant::now(),
            last_ping: Instant::now(),
            last_loot_method_change: None,
            last_undrawable_warned: 0,
            own_body_drawn: true,
            own_corpse: None,
            player_looks: std::collections::HashMap::new(),
            offline_map: None,
            lighting: None,
            target: None,
            press_at: None,
            right_press_at: None,
            camera_yaw_offset: 0.0,
            // Slightly above the subject, looking gently down, which is where
            // the game this is modelled on starts.
            camera_pitch: -0.2,
            steering: false,
            chat: Vec::new(),
            combat_text: Vec::new(),
            action_flash: None,
            entity_flip: false,
            flip_winding: false,
            strafe_yaw_choice: STRAFE_YAW_DEFAULT,
            composing: None,
            chat_channel: ChatChannel::Say,
            spells: spells::Spellbook::default(),
            bars_seeded: false,
            spellbook_open: false,
            items: items::Items::default(),
            maps,
            taxi_network,
            minimap,
            map_open: false,
            objectives: maps::Objectives::default(),
            quest_marks: std::collections::HashMap::new(),
            quest_marks_asked: std::collections::HashMap::new(),
            quest_marks_log: Vec::new(),
            area: None,
            sounds: sound::Sounds::default(),
            effects: sound::Effects::default(),
            pending_sounds: Vec::new(),
            impact_delay_ms,
            attackers: std::collections::HashSet::new(),
            // Opened once, here, rather than on the first sound: enumerating
            // devices takes long enough to be a visible hitch, and doing it
            // mid-play would put that hitch on a zone boundary.
            audio: match rodio::OutputStreamBuilder::open_default_stream() {
                Ok(stream) => Some(stream),
                Err(error) => {
                    tracing::warn!("no audio device, the client will be silent: {error}");
                    None
                }
            },
            music: sound::Channel::new(),
            ambience: sound::Channel::new(),
            bags_open: false,
            character_open: false,
            quest_log_open: false,
            selected_quest: None,
            quests: ::world::QuestCache::new(),
            quest_cache_path: None,
            quest_asked_at: std::collections::HashMap::new(),
            quest_saved_at: Instant::now(),
            questgiver: None,
            trainer: None,
            flight: None,
            taxi: None,
            looting: None,
            own_corpse_query_sent: false,
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
            .with_title("MeoWoW")
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
        let sky = render::SkyRenderer::new(&gpu, format);
        let precipitation = render::PrecipitationRenderer::new(&gpu, format);
        let particles = render::ParticleRenderer::new(&gpu, format);
        let emitters = emitters::Emitters::new();
        let liquid_renderer = render::LiquidRenderer::new(&gpu, format);
        let mut liquid_types = liquid::LiquidTypes::default();
        let depth = DepthBuffer::new(&gpu, config.width, config.height);

        let scene = match build_scene(
            &gpu,
            &terrain_renderer,
            &liquid_renderer,
            &mut liquid_types,
            &mut meshes,
            &mut self.chain,
            &self.args,
        ) {
            Ok((scene, live)) => {
                self.offline_map = offline_map_id(&mut self.chain, self.args.map.as_deref());
                if live.is_some() || (self.offline_map.is_some() && self.args.hour.is_some()) {
                    // Start the movement and keepalive clocks from the moment
                    // the connection is actually ready to drive, not from
                    // whenever the window happened to be created.
                    self.last_heartbeat = Instant::now();
                    self.last_ping = Instant::now();
                    self.last_undrawable_warned = 0;
                    // Only a live world has a map and an hour to light.
                    let started = Instant::now();
                    self.lighting =
                        dbc::light::Lighting::load(|path| self.chain.read(path).ok());
                    tracing::info!(
                        "lighting tables loaded in {:?} ({})",
                        started.elapsed(),
                        if self.lighting.is_some() { "ok" } else { "unavailable" }
                    );
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
            sky,
            precipitation,
            particles,
            emitters,
            liquid_renderer,
            liquid_types,
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

    /// Raw mouse movement, independent of where the pointer is.
    ///
    /// Only used while a drag holds the pointer -- see [`App::device_motion`].
    /// Outside a drag the position-based `CursorMoved` is the right event,
    /// because what matters then is what the pointer is *over*.
    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        if let winit::event::DeviceEvent::MouseMotion { delta } = event {
            self.device_motion(delta.0, delta.1);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.clone() else {
            return;
        };
        // **A button that is not held down must never leave the camera
        // steering, whoever consumed the event.**
        //
        // The drag flags are set on press and cleared on release, and the
        // clear used to live past the `consumed` check below -- so a release
        // that egui claimed never reached it and the flag stayed set for good.
        // That is not a rare race: the loot window opens *on* a right-click
        // and appears under the cursor, so it swallows the very release that
        // ends the gesture. The camera then turned with every mouse movement,
        // with no button down and no way to stop it.
        //
        // Only the flags are cleared here. The click that a release also
        // represents is still decided below, and only for events egui did not
        // take -- otherwise clicking a window would also swing at whatever is
        // behind it.
        //
        // The general rule, and it is worth keeping: state that mirrors a
        // physical input has to be corrected from the input's *end*, not from
        // the path that usually handles it.
        match &event {
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button,
                ..
            } => {
                match button {
                    MouseButton::Left => self.dragging = false,
                    MouseButton::Right => self.steering = false,
                    _ => {}
                }
                // The pointer goes back the moment neither drag is live, and
                // from here rather than from the branch below, for the same
                // reason the flags are cleared here: a frame that appears
                // mid-gesture eats the release, and a cursor left hidden and
                // grabbed by that is far worse than a camera that keeps
                // turning.
                if !self.dragging && !self.steering {
                    self.release_cursor(&window);
                }
            }
            // Losing focus mid-drag is the other way a release never arrives:
            // alt-tab away with a button down and the window is simply not
            // told it came up again.
            WindowEvent::Focused(false) => {
                self.dragging = false;
                self.steering = false;
                self.press_at = None;
                self.right_press_at = None;
                self.release_cursor(&window);
            }
            _ => {}
        }

        // The renderer is borrowed *after* the block above, not with the
        // window at the top: releasing the pointer needs `&mut self`, and a
        // renderer borrow held across it would make the one correction that
        // must happen on every path the one that cannot compile.
        let Some(r) = self.renderer.as_mut() else {
            return;
        };
        // **Two mechanisms used to answer "is the pointer over the interface"
        // and they disagreed, which is `foss-wow#79`.**
        //
        // egui's own `consumed` does not count a layer in `Order::Background`
        // as an area the pointer is over. `ElementId::layer` puts the bags,
        // spellbook, character panel, quest log and map exactly there -- so a
        // press over any of them came back unconsumed, this function ran on,
        // started a camera drag and grabbed the cursor, and the click never
        // reached the square under it. The loot, questgiver and release
        // windows sit in `Order::Middle` and were unaffected, which is why
        // looting worked while the bag window "let the mouse through".
        //
        // `Hud::captures_pointer` is the authority that already blends egui's
        // opinion with the rectangles the interface actually drew -- its own
        // doc comment says the first alone is not enough -- and `pick_at` has
        // always used it. This is the same question, so it gets the same
        // answer from the same place rather than a second opinion.
        //
        // **Buttons only.** Swallowing `CursorMoved` here would stop
        // `last_cursor` tracking, and a press then reads its start position
        // from a stale field -- which is the shape of a bug this file has
        // already had once.
        let consumed = r.egui_state.on_window_event(&window, &event).consumed
            || (matches!(event, WindowEvent::MouseInput { .. })
                && self.hud.captures_pointer(&r.egui_ctx));
        // **Which way a button press went, and nothing else.** "The window
        // ignored my click" and "the click went past the window into the
        // world" are the same report from the far side of the screen and want
        // opposite investigations, and this is the fork in the road: taken,
        // the interface has the press and the rest of this function never
        // runs -- including the cursor grab, which is why a click on a button
        // no longer hides the pointer for its own duration.
        if let WindowEvent::MouseInput { state, button, .. } = &event {
            tracing::debug!(
                "{:?} {:?} at {:?} -> {}",
                button,
                state,
                self.last_cursor,
                if consumed { "the interface" } else { "the world" }
            );
        }
        if consumed {
            window.request_redraw();
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                // **Written on the way out, not every time a quest arrives.**
                // A save per answer would be a file write per packet during
                // the burst after login; a save here costs one write for a
                // whole session. The trade is that a crash loses the
                // session's discoveries, which costs only re-asking.
                self.save_quest_cache();
                event_loop.exit()
            }
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
                    self.left_travel = 0.0;
                    self.capture_cursor(&window);
                } else {
                    // A press and release with the pointer barely moving is a
                    // click; the same two events with travel between them is
                    // the drag that turns the camera. Nothing else
                    // distinguishes them, so the movement has to be measured --
                    // and it is measured as distance *travelled*, because a
                    // captured pointer is pinned and so ends every drag exactly
                    // where it began.
                    if let Some(press) = self.press_at.take() {
                        if was_click(self.left_travel) {
                            self.click_at(press);
                        }
                    }
                    // `last_cursor` deliberately survives the release.
                    //
                    // Clearing it here is what the first version did, and it
                    // quietly discarded every *second* click made without
                    // moving the mouse: the next press reads `press_at` from
                    // this field, finds `None`, and the release then has
                    // nothing to measure against. Rare with a left click,
                    // which is only a selection -- but right-click-to-attack
                    // is a gesture people repeat on the same pixel when it
                    // does not seem to have worked, which is precisely when it
                    // would have kept not working.
                    //
                    // Nothing needs it cleared: `CursorMoved` fires whether or
                    // not a button is down, so the field is current, and the
                    // drag branches only read it while their own button is
                    // held.
                }
            }
            // The right button does two different things depending on whether
            // it moves, exactly as the left button does: dragged, it steers the
            // character; clicked, it selects what is under it and attacks.
            //
            // Nothing but the distance travelled separates them, which is why
            // this mirrors the left button's structure rather than inventing a
            // second one -- and why `last_cursor` is *not* cleared on press.
            // A click that never moves the mouse has to measure as zero
            // distance, and it can only do that if the field still holds where
            // the press happened; the first version of this cleared it, and
            // right-clicking a creature without twitching the mouse did
            // nothing at all.
            WindowEvent::MouseInput { state, button, .. } if button == MouseButton::Right => {
                let pressed = state == ElementState::Pressed;
                self.steering = pressed && self.live.is_some();
                if pressed {
                    self.right_press_at = self.last_cursor;
                    self.right_travel = 0.0;
                    // Captured even when there is no character to steer: the
                    // right button still turns the camera, and half a gesture
                    // holding the pointer and half not would be worse than
                    // either.
                    self.capture_cursor(&window);
                } else {
                    if let Some(press) = self.right_press_at.take() {
                        if was_click(self.right_travel) {
                            self.right_click_at(press);
                        }
                    }
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
                        // Space jumps in the world and raises a free camera.
                        // Handled before the toggles so it can fall through to
                        // `keys.set` when there is no character to jump.
                        // **Except while swimming**, where Space is held to
                        // rise rather than tapped to leave the ground -- so it
                        // falls through to `keys.set` and becomes a held
                        // state. Jumping out of deep water is not a thing a
                        // character can do, and `begin_jump` refuses it
                        // independently: this branch decides which *input* the
                        // key is, and that one decides whether the action is
                        // legal.
                        if code == KeyCode::Space
                            && self.live.is_some()
                            && self.swimming.is_none()
                        {
                            self.begin_jump();
                            window.request_redraw();
                            return;
                        }
                        // Autorun, on the key 3.3.5a uses for it. Pressing it
                        // again, or pressing a key that means "stop", clears
                        // it -- see `drive_live_movement`.
                        if code == KeyCode::NumLock && self.live.is_some() {
                            self.autorun = !self.autorun;
                            window.request_redraw();
                            return;
                        }
                        // Grabbing the movement keys cancels autorun, which is
                        // what every game with one does: a player reaching for
                        // the keys to dodge something should not have to
                        // remember to switch it off first.
                        if self.autorun && matches!(code, KeyCode::KeyS | KeyCode::ArrowDown) {
                            self.autorun = false;
                        }
                        match code {
                            KeyCode::F1 => self.hud.toggle_edit(),
                            // `P` for the spellbook, as 3.3.5a binds it. Not a
                            // movement key and not a bar key, so it costs
                            // nothing that was already spoken for.
                            KeyCode::KeyP => {
                                self.spellbook_open = !self.spellbook_open;
                                // **Logged, because "the window did not open"
                                // and "the window opened empty" are the same
                                // report and want opposite investigations.**
                                // The count is the whole diagnosis: a book with
                                // rows that nobody can see is a layout problem,
                                // and a book with no rows is the spell filter.
                                if self.spellbook_open {
                                    tracing::info!(
                                        "spellbook opened: {} spell(s) it would list",
                                        Self::castable_spells(
                                            self.live.as_ref(),
                                            &self.spells
                                        )
                                        .len()
                                    );
                                } else {
                                    tracing::info!("spellbook closed");
                                }
                                window.request_redraw();
                                return;
                            }
                            // `B` for the bags, as 3.3.5a binds it. One window
                            // rather than the original's one-per-bag, so this
                            // is a single toggle rather than the original's
                            // separate keys for each bag.
                            KeyCode::KeyB => {
                                self.bags_open = !self.bags_open;
                                window.request_redraw();
                                return;
                            }
                            // `[` and `]` tune the impact delay by ear. See
                            // `App::impact_delay_ms` -- the number can only be
                            // found against the animation, so it is found in
                            // play rather than guessed between restarts.
                            KeyCode::BracketLeft | KeyCode::BracketRight => {
                                let step: i64 =
                                    if code == KeyCode::BracketRight { 50 } else { -50 };
                                self.impact_delay_ms =
                                    (self.impact_delay_ms as i64 + step).clamp(0, 3000) as u64;
                                let text =
                                    format!("impact delay {}ms", self.impact_delay_ms);
                                tracing::info!("{text}");
                                self.chat.push(Line::Chat(local_notice(text)));
                                window.request_redraw();
                                return;
                            }
                            // `C` for the character panel, as 3.3.5a binds it.
                            KeyCode::KeyC => {
                                self.character_open = !self.character_open;
                                window.request_redraw();
                                return;
                            }
                            // `L` for the quest log, as 3.3.5a binds it.
                            KeyCode::KeyL => {
                                self.quest_log_open = !self.quest_log_open;
                                window.request_redraw();
                                return;
                            }
                            // `M` for the world map, as 3.3.5a binds it.
                            KeyCode::KeyM => {
                                self.map_open = !self.map_open;
                                window.request_redraw();
                                return;
                            }
                            // `Z` draws and stows, as 3.3.5a binds it. The
                            // weapon's resting place comes from the item, so
                            // this only says drawn or not.
                            KeyCode::KeyZ => {
                                let drawn = self
                                    .live
                                    .as_ref()
                                    .and_then(|live| live.state.get(live.guid))
                                    .is_some_and(|entity| entity.sheath().drawn());
                                let wanted = if drawn {
                                    ::world::combat::SheathState::Unarmed
                                } else {
                                    ::world::combat::SheathState::Melee
                                };
                                self.set_sheath(wanted);
                                window.request_redraw();
                                return;
                            }
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
                            // See `App::strafe_yaw_choice`: hold `Q` or `E`
                            // and press this until the legs look right from
                            // behind, which is a comparison and not a guess.
                            KeyCode::F4 => {
                                self.strafe_yaw_choice =
                                    (self.strafe_yaw_choice + 1) % STRAFE_YAW_CHOICES.len();
                                let degrees = STRAFE_YAW_CHOICES[self.strafe_yaw_choice];
                                self.chat.push(Line::Chat(local_notice(format!(
                                    "sidestep lean: {degrees:.0} degrees"
                                ))));
                                tracing::info!("sidestep lean {degrees:.0} degrees");
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
                // **While the pointer is held, this event does not turn
                // anything.** The camera is driven by `device_motion` from raw
                // deltas instead, and letting both act would double every
                // movement -- so this branch only keeps `last_cursor` current
                // for the click that ends the gesture.
                if self.cursor_captured {
                    self.last_cursor = Some(now);
                    window.request_redraw();
                    return;
                }
                // Travel per held button. A gesture that is *not* captured --
                // a press egui took, or a platform that refused the grab --
                // still has to be told from a click.
                if let Some(prev) = self.last_cursor {
                    let step = ((now.0 - prev.0).powi(2) + (now.1 - prev.1).powi(2)).sqrt();
                    if self.press_at.is_some() {
                        self.left_travel += step;
                    }
                    if self.right_press_at.is_some() {
                        self.right_travel += step;
                    }
                }
                if self.steering {
                    // Steering turns the *character*. The camera needs no
                    // separate handling: it is rebuilt from the character's
                    // facing every frame, so turning the body brings the view
                    // with it, which is exactly what a right drag should feel
                    // like. Vertical movement orbits the camera over and under
                    // the character -- a character has no pitch to steer, but
                    // the view still has to keep them in the middle of it.
                    if let Some(prev) = self.last_cursor {
                        let speed = self
                            .hud
                            .profile
                            .camera
                            .radians_per_pixel(window.inner_size().width as f32);
                        let dx = -(now.0 - prev.0) as f32 * speed;
                        let dy = (now.1 - prev.1) as f32 * speed;
                        // Steering takes over the character's facing, so any
                        // swing a left drag had given the camera is folded
                        // away -- otherwise the view jumps by that offset the
                        // moment the character starts turning under it.
                        //
                        // Done here, on the first actual movement, rather than
                        // at the press: a right *click* must not snap the
                        // camera, and at the press there is no way to know yet
                        // which of the two gestures this is.
                        self.camera_yaw_offset = 0.0;
                        if let Some(live) = self.live.as_mut() {
                            live.orientation =
                                (live.orientation + dx).rem_euclid(std::f32::consts::TAU);
                        }
                        self.camera_pitch = (self.camera_pitch
                            - dy * self.hud.profile.camera.pitch_sign())
                        .clamp(-FOLLOW_PITCH_LIMIT, FOLLOW_PITCH_LIMIT);
                    }
                    self.last_cursor = Some(now);
                    window.request_redraw();
                    return;
                }
                if self.dragging {
                    if let Some(prev) = self.last_cursor {
                        let speed = self
                            .hud
                            .profile
                            .camera
                            .radians_per_pixel(window.inner_size().width as f32);
                        let (dx, dy) = (
                            -(now.0 - prev.0) as f32 * speed,
                            (now.1 - prev.1) as f32 * speed,
                        );
                        // Standing in a live world, **both** camera angles are
                        // owned by `drive_live_movement`, which rebuilds them
                        // from the character every frame. Writing either here
                        // would be overwritten before it was ever drawn, so
                        // the drag accumulates offsets that function applies.
                        //
                        // Pitch used to be the exception, written straight into
                        // the camera because the follow code left it alone --
                        // and that is exactly what made dragging up and down
                        // wrong. Tilting a camera that stays put swings its aim
                        // off the character; orbiting it keeps them centred.
                        let following = self.live.is_some();
                        match &mut self.camera {
                            Camera::Orbit(c) => c.orbit(dx, dy),
                            // Following: the angles are applied by the follow
                            // code, not here.
                            Camera::Fly(_) if following => {}
                            // Free-flying, there is no subject to orbit, so a
                            // drag turns the view in place.
                            Camera::Fly(c) => c.look(dx, -dy),
                        }
                        if following {
                            self.camera_yaw_offset =
                                (self.camera_yaw_offset + dx).rem_euclid(std::f32::consts::TAU);
                            self.camera_pitch = (self.camera_pitch
                                - dy * self.hud.profile.camera.pitch_sign())
                            .clamp(-FOLLOW_PITCH_LIMIT, FOLLOW_PITCH_LIMIT);
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
                // **The minimap answers the wheel first, and then nothing
                // else does.** Both this and the camera want the wheel, and
                // the camera's arm has never asked where the pointer is -- so
                // without the early return a scroll over the disc would zoom
                // the map *and* pull the camera in, which reads as the minimap
                // dragging the view around with it.
                if self.zoom_minimap(notches) {
                    return;
                }
                // Following a character, the wheel pulls the camera in and
                // pushes it out, which is what it does in the game. A free
                // camera has no subject to be a distance from, so there it
                // keeps trimming travel speed.
                if self.live.is_some() {
                    self.camera_distance = (self.camera_distance * 0.88f32.powf(notches))
                        .clamp(FOLLOW_NEAR, FOLLOW_FAR);
                } else {
                    match &mut self.camera {
                        Camera::Orbit(c) => c.zoom(0.88f32.powf(notches)),
                        Camera::Fly(c) => {
                            c.speed = (c.speed * 1.15f32.powf(notches)).clamp(1.0, 5000.0)
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // **The invariant, checked where it cannot be skipped.** A
                // captured pointer is invisible and confined, so a gesture
                // whose release never arrived -- swallowed by a window that
                // opened under the cursor, or lost with the focus -- would
                // leave the interface unusable with nothing on screen saying
                // why. Every path that ends a drag already releases it; this
                // is the one that catches the path nobody thought of.
                if self.cursor_captured && !self.dragging && !self.steering {
                    tracing::debug!("the pointer was still held with no button down");
                    self.release_cursor(&window);
                }
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
        // Read here for the same reason as the pair above: the renderer is
        // borrowed mutably below and holds that borrow for the whole draw, so
        // anything wanting `&self` has to have asked already.
        let pace = self.animation_pace();
        let lean = self.strafe_lean();

        // **Before the renderer is borrowed**, because this needs the archive
        // chain and the scene at the same time and the draw below holds the
        // renderer for its whole body.
        self.update_sound();

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
                &r.liquid_renderer,
                &mut self.chain,
                eye,
            );
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
                if let Some(live) = self.live.as_mut() {
                    // The keys, not the wire: the server never relays our own
                    // movement back to us. Held means running -- there is no
                    // walk toggle here, and `LIVE_RUN_SPEED` is the run speed.
                    // F2. See `App::entity_flip`.
                    let flip = if self.entity_flip { std::f32::consts::PI } else { 0.0 };
                    // Borrowed as fields rather than through `&mut self`: `r`
                    // already holds the renderer, and `live` the live world.
                    let looks = &mut self.player_looks;
                    let chain = &mut self.chain;
                    let items = &self.items;
                    // Eased before they are placed, so a creature turning to
                    // face its victim swings round rather than snapping. The
                    // player's own body is excluded inside `ease_facings` --
                    // its heading comes from the keys and is already smooth,
                    // and easing it would make the camera lag the character.
                    let mut drawn = drawable_with_own(
                        live,
                        pace,
                        lean,
                        self.jump.is_some(),
                        self.swimming.is_some(),
                    );
                    // See `App::own_body_drawn`: submitted-and-not-drawn and
                    // never-submitted are the same report from the window.
                    let own_drawn = drawn.iter().any(|entity| entity.guid == live.guid);
                    live.ease_facings(&mut drawn, self.frame_ms / 1000.0);
                    let placements: Vec<crate::world::EntityPlacement> =
                        drawn
                            .iter()
                            .map(|entity| {
                                // Three sources, in order of what actually
                                // knows: our own body's look was resolved at
                                // login from the character list; another
                                // player's comes off their update fields; a
                                // creature has none and is dressed from its
                                // display id inside the renderer.
                                let (look, look_key) = if entity.guid == live.guid {
                                    (Some(live.look.clone()), live.look_key)
                                } else if let Some(appearance) = entity.appearance {
                                    let (look, key) = player_look(
                                        looks,
                                        chain,
                                        items,
                                        appearance,
                                        &entity.visible_items,
                                    );
                                    (Some(look), key)
                                } else {
                                    (None, 0)
                                };
                                crate::world::EntityPlacement {
                                    guid: entity.guid,
                                    display_id: entity.display_id,
                                    position: entity.position,
                                    orientation: entity.orientation + flip,
                                    scale: entity.scale,
                                    speed: entity.speed,
                                    turning: entity.turning,
                                    airborne: entity.airborne,
                                    swimming: entity.swimming,
                                    dead: entity.dead,
                                    died_ms_ago: entity.died_ms_ago,
                                    swung_ms_ago: entity.swung_ms_ago,
                                    spell: self
                                        .spells
                                        .cast_animations
                                        .pose(entity.casting_spell, entity.cast_landed),
                                    fighting: entity.fighting,
                                    kind: entity.kind,
                                    stance: look
                                        .as_deref()
                                        .map(|l| l.stance)
                                        .unwrap_or_default(),
                                    look,
                                    look_key,
                                    sheathed: entity.sheathed,
                                    sheath_changed_ms_ago: entity.sheath_changed_ms_ago,
                                }
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

                    // On the change only, and naming which of the two things
                    // went wrong. Silence here while the character is
                    // invisible is itself the finding: the body was handed to
                    // the renderer and the fault is past this point.
                    if own_drawn != self.own_body_drawn {
                        match live.state.get(live.guid) {
                            None => tracing::warn!(
                                "the player's own body left replicated state -- \
                                 something removed guid {:#x}",
                                live.guid
                            ),
                            Some(entity) => tracing::warn!(
                                "the player's own body is {} the drawn list; \
                                 replicated display id {:?}",
                                if own_drawn { "back in" } else { "gone from" },
                                entity.display_id()
                            ),
                        }
                        self.own_body_drawn = own_drawn;
                    }
                }
            }

            // Posed *after* the rebuild, not before it, and every frame
            // regardless of whether the rebuild placed anything new.
            //
            // The order is the whole point. `set_entities` creates a bone
            // buffer the moment a bucket appears -- a unit starting a swing,
            // or dying -- and a fresh buffer holds only the bind pose until
            // something writes a real one into it. Posing first meant every
            // new bucket was drawn unposed for a frame: a character that
            // snapped to its bind pose at the start of every swing, which is
            // exactly what "the attack animation is very jittery" looks like.
            //
            // It stays every-frame rather than folding into the rebuild for
            // the original reason: animation and instance positions run on
            // different clocks, and tying a walk cycle to the coarser one made
            // it visibly stutter.
            world.update_animations(&r.gpu, &r.meshes);
            // ...and everything alight, after the poses it hangs off. A flame
            // on a hand is placed by the very matrix the hand was drawn with,
            // so stepping first would leave every emitter a frame behind the
            // skeleton carrying it -- exactly the lag a held weapon had before
            // it was moved after the pose for the same reason.
            //
            // `frame_ms` rather than a wall clock: a particle's fall is
            // integrated, so it needs how long the *last* frame took, and the
            // simulation clamps the step itself so a stall cannot be paid back
            // as a burst.
            world.update_emitters(
                &r.gpu,
                &mut r.particles,
                &mut r.emitters,
                self.frame_ms / 1000.0,
            );
        }

        if let (Some(Scene::Model(m)), Some(bones)) = (&r.scene, &r.bones) {
            upload_pose(&r.gpu, &r.meshes, bones, m, anim, anim_time);
        }
        if let Some(Scene::Model(m)) = &r.scene {
            // One step here, because the window runs at sixty of them a
            // second. The headless path warms up instead -- see
            // `warm_emitters`.
            warm_emitters(
                &r.gpu,
                &mut r.particles,
                &mut r.emitters,
                m,
                anim,
                anim_time,
                1,
            );
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
        // Resolved per frame, not per session: the clock runs, and the camera
        // can walk out of one light's radius into another's.
        let eye = match camera {
            Camera::Fly(f) => f.position,
            Camera::Orbit(o) => o.eye(),
        };
        let weather = frame_weather(self.live.as_ref(), &self.args);
        let lighting = resolve_lighting(
            self.lighting.as_ref(),
            self.live.as_ref(),
            weather,
            self.offline_map,
            self.args.hour,
            eye,
        );
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
                &r.liquid_renderer,
                &r.liquid_types,
                &r.sky,
                &r.precipitation,
                &r.particles,
                &r.emitters,
                resolve_precipitation(weather, self.started.elapsed().as_secs_f32()),
                &r.material_binds,
                r.bones.as_ref(),
                &r.world_binds,
                &r.identity,
                lighting,
                self.started.elapsed().as_secs_f32(),
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
    /// The height the camera orbits around, easing towards the character's.
    ///
    /// **A fraction of the remaining error per second, not a maximum rate.**
    /// A rate cap looks right in every frame and then falls arbitrarily far
    /// behind whenever the input moves faster than the cap -- the same trap
    /// the creature turning code documents at length. Closing a fraction
    /// bounds the lag at `speed * TAU` for any speed at all.
    ///
    /// Large jumps are taken whole rather than eased. A fall, a teleport or
    /// the first frame after login are not stairs, and gliding the camera down
    /// a cliff over a quarter of a second would look far stranger than the
    /// snap it replaced.
    ///
    /// A free function taking the field rather than a method taking `&mut
    /// self`: the caller already holds a mutable borrow of `self.live`, and
    /// the borrow checker cannot see that two fields are disjoint through a
    /// method call.
    fn camera_follow_z(state: &mut Option<f32>, target: f32, dt: f32) -> f32 {
        /// Seconds to close most of the gap. Short enough that the camera
        /// still reads as attached to the character, long enough to swallow a
        /// stair riser.
        const TAU: f32 = 0.09;
        /// Past this it is not a step, and easing it would be a slide.
        const SNAP: f32 = 3.0;

        let smoothed = match *state {
            Some(z) if (target - z).abs() <= SNAP && dt > 0.0 => {
                z + (target - z) * (1.0 - (-dt / TAU).exp())
            }
            _ => target,
        };
        *state = Some(smoothed);
        smoothed
    }

    /// What the character's own body should be *animating* at: how fast along
    /// its facing, and how fast it is turning on the spot.
    ///
    /// **Not how fast it travels** -- that is [`live_pace`] read directly by
    /// `drive_live_movement`, and the two parting company for one commit is
    /// what stopped sidestepping from moving anybody. One function so the
    /// frame that is drawn and the list a click is tested against cannot
    /// disagree, which is the same rule that unprojects the picking ray from
    /// the matrix the scene was drawn with.
    fn animation_pace(&self) -> (f32, f32) {
        (
            live_pace(self.live_move),
            // Turning on the spot, and only that: the shuffles carry nobody
            // anywhere, and a sidestep is carried by the run with the body
            // turned -- see [`strafe_yaw`].
            live_turning(self.keys, self.steering, self.live_move),
        )
    }

    /// How far the drawn body leans into a sidestep this frame, in radians.
    ///
    /// Read from [`App::strafe_yaw_choice`], which `F4` cycles.
    fn strafe_lean(&self) -> f32 {
        strafe_yaw(
            self.live_move,
            STRAFE_YAW_CHOICES[self.strafe_yaw_choice].to_radians(),
        )
    }

    /// Moves the character along the server's spline, and reports whether a
    /// flight is in progress at all.
    ///
    /// Returns `true` while flying, which is what makes
    /// [`Self::drive_live_movement`] stand down wholesale.
    ///
    /// **Landing restores the pre-flight facing** rather than leaving the
    /// character pointing along the last leg of the route. Not cosmetic: the
    /// final leg is usually a descent into a landing pad from whatever
    /// direction the route happened to arrive, and a character left facing
    /// that way has been silently turned by something the player did not do.
    fn advance_flight(&mut self) -> bool {
        let Some(flight) = self.flight.as_ref() else {
            return false;
        };
        let Some(live) = self.live.as_mut() else {
            // No connection: nothing to fly. Dropped rather than held, since
            // a flight resumed against a different session would be moving a
            // character the server has different ideas about.
            self.flight = None;
            return false;
        };

        let elapsed = flight.started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
        let ((x, y, z), heading) = flight.route.at(elapsed);
        live.position.x = x;
        live.position.y = y;
        live.position.z = z;
        live.orientation = heading;

        if flight.route.finished(elapsed) {
            tracing::info!("landed at {x:.1}, {y:.1}, {z:.1}");
            live.orientation = flight.orientation_before;
            self.flight = None;
        }
        true
    }

    fn drive_live_movement(&mut self) {
        use ::world::update::movement_flags;
        use ::world::{ClientOpcode, MovementInfo, Position};

        // **A flight replaces this function rather than modifying it.**
        //
        // The tempting shape is to guard the individual writes -- `if
        // self.flight.is_none()` beside the ground assignment, and again
        // beside the input, and again beside the jump. That is how 4.18's
        // swimming bug happened: `position.z = ground` was correct and
        // unconditional for four milestones, a second thing started writing
        // the same field, and the assignment quietly undid it every frame.
        // The lesson recorded then was to find *every* unconditional write to
        // a field that now has to persist -- and the reliable way to do that
        // is to not run any of them.
        //
        // So a flight returns before turning, input, collision, ground
        // height, buoyancy and the jump arc, and none of them acquire a new
        // condition. Nothing is reported to the server either: a movement
        // packet is the client's statement about itself, and while the server
        // is flying the character that statement is not the client's to make.
        if self.advance_flight() {
            return;
        }

        let Some(live) = self.live.as_mut() else {
            return;
        };
        let dt = (self.frame_ms / 1000.0).max(0.0);

        // Turning is purely local: nothing here reports it to the server
        // unless a translation is also in flight, since the position sent
        // with every Start/Heartbeat carries the current orientation anyway.
        //
        // While steering with the mouse, `A`/`D` are strafe keys instead --
        // see `KeyState::motion`. They must not do both: a key that turned the
        // character *and* pushed it sideways would send it round in a circle,
        // and the cause reads as a mouse problem rather than a keyboard one.
        let turn = if self.steering {
            0.0
        } else {
            match (self.keys.left, self.keys.right) {
                (true, false) => LIVE_TURN_RATE,
                (false, true) => -LIVE_TURN_RATE,
                _ => 0.0,
            }
        };
        if turn != 0.0 {
            live.orientation =
                (live.orientation + turn * dt).rem_euclid(std::f32::consts::TAU);
        }

        let mut desired = self.keys.motion(self.steering);
        // Autorun is forward that nobody is holding. Ignored while the player
        // is actually holding a longitudinal key, so pressing S while running
        // stops rather than fighting the toggle -- and `drive_live_movement`'s
        // caller clears the toggle in that case.
        if self.autorun && !desired.backward {
            desired.forward = true;
        }

        let (dx, dy) = desired.direction(live.orientation);
        if (dx, dy) != (0.0, 0.0) {
            // `direction` is already a unit vector carrying which way the
            // keys point, so all that is wanted here is a magnitude. This is
            // the caller that made a pace of zero mean "do not move", which
            // is why `live_pace` cannot answer an animation question with a
            // number the movement integrator also reads.
            let speed = live_pace(desired).abs();
            let wanted = glam::Vec3::new(
                live.position.x + dx * speed * dt,
                live.position.y + dy * speed * dt,
                live.position.z,
            );
            // **Buildings are solid, and nothing but this client says so.**
            // A character driven through the abbey wall was drawn inside it by
            // a second client watching, so the server neither corrects nor
            // objects. See the `collision` crate.
            live.position = match self.renderer.as_ref().and_then(|r| r.scene.as_ref()) {
                Some(Scene::Streaming(world)) => {
                    world.slide(live.position, wanted, BODY_RADIUS, BODY_HEIGHT, STEP_HEIGHT)
                }
                _ => wanted,
            };
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
        // ground.
        //
        // `None` -- the tile is not resident yet, or this is a hole in the
        // terrain -- deliberately leaves Z alone rather than substituting
        // anything. The server's altitude is stale, but it is a real place; a
        // guess is not.
        //
        // A jump rides *on top of* this rather than replacing it: the ground
        // is still whatever the terrain says, and the arc is a height above
        // it. That is what makes jumping up a slope work without a second
        // notion of where the ground is -- and it is also why landing is
        // detected from the arc reaching zero rather than from comparing two
        // altitudes, which would trigger every time the terrain rose under a
        // running character.
        // `Some(ms)` on the frame the ground is reached, carrying how long the
        // character was in the air -- which is the number fall damage is
        // measured from, and is gone the moment the jump is cleared.
        let mut landed: Option<u32> = None;
        // The ground under the character and the liquid over it, sampled
        // together while the scene is borrowed. Both are wanted *outside* this
        // block -- the swim decision needs the pair, since what makes water
        // swimmable is how far it stands above the bed, not its altitude.
        let mut stand_at: Option<f32> = None;
        let mut liquid_here: Option<world::Liquid> = None;
        if let Some(Scene::Streaming(world)) = self.renderer.as_ref().and_then(|r| r.scene.as_ref())
        {
            // A building's floor outranks the terrain under it, which is
            // what makes an interior an interior. Searched downward from where
            // the character already is plus a step, so a staircase is climbed
            // and the roof overhead is not stood on -- see
            // `collision::World::floor_under`.
            //
            // The terrain is still the fallback and still the common case:
            // most of the world is open ground, and the height field answers
            // for it far more cheaply than a triangle query ever could.
            let ground = world.height_at(live.position.x, live.position.y);
            let underfoot = world.floor_under_footing(live.position, STEP_HEIGHT);
            let floor = underfoot.map(|(z, _)| z);
            // The higher of the two, so a floor laid over ground holds the
            // character up -- but only a floor at or below head height, which
            // `floor_under` has already enforced.
            let stand = match (ground, floor) {
                (Some(g), Some(f)) => Some(g.max(f)),
                (some, None) | (None, some) => some,
            };
            // **Read off the very comparison above**, not asked again. The
            // character is on the building's floor exactly when the floor won
            // that `max`, so deriving the surface from a second test would be
            // two answers to one question -- and the frame they disagree on is
            // a footstep that sounds like the ground under the floorboards.
            self.floor_material = match (ground, underfoot) {
                (Some(g), Some((f, surface))) if f >= g => surface,
                (None, Some((_, surface))) => surface,
                _ => None,
            };
            // **Logged because the alternative is guessing.** A character
            // that judders going up steps has at least three candidate causes
            // -- two surfaces alternating, a floor and the terrain trading
            // places, or a horizontal move being refused and retried -- and
            // they are indistinguishable from the outside. One line naming
            // both candidate heights separates them in a single walk.
            if let Some(z) = stand {
                // Only where a building is involved. On a hillside the
                // terrain answers differently every frame by design, and
                // logging that would bury the case this exists for.
                if floor.is_some() && (z - live.position.z).abs() > 0.001 {
                    tracing::debug!(
                        "stand {:.3} -> {:.3} (terrain {:?}, floor {:?}) at {:.1},{:.1}",
                        live.position.z,
                        z,
                        ground.map(|g| (g * 1000.0).round() / 1000.0),
                        floor.map(|f| (f * 1000.0).round() / 1000.0),
                        live.position.x,
                        live.position.y,
                    );
                }
            }
            stand_at = stand;
            liquid_here = world.liquid_at(live.position.x, live.position.y);
        }

        // **Swimming, and whether this frame changed it.**
        //
        // The condition is not "the water is above my feet" -- that is ankle
        // deep at every shoreline and would have a character swimming across a
        // ford. It is that the surface stands at least [`SWIM_DEPTH`] above
        // whatever is holding the character up, which is what "deep enough to
        // swim in" means and is why the ground had to be sampled too.
        //
        // A liquid whose category this client does not recognise is *not*
        // swum in: `LiquidCategory::Unknown` means the table did not load or
        // named a value this build does not use, and starting to swim on the
        // strength of a number nobody could resolve is the fabrication the
        // category exists to refuse. It still draws.
        // Wading is liquid over the feet that is not deep enough to swim in.
        // Measured against the same ground sample the swim test uses, so the
        // two cannot disagree about where the bottom is.
        self.wading = liquid_here.is_some_and(|liquid| {
            stand_at.is_some_and(|ground| {
                liquid.surface > ground && liquid.surface - ground < SWIM_DEPTH
            })
        });
        let was_swimming = self.swimming.is_some();
        self.swimming = liquid_here.filter(|liquid| {
            liquid.category.is_swimmable()
                && stand_at.is_some_and(|ground| liquid.surface - ground >= SWIM_DEPTH)
        });
        let swim_changed = was_swimming != self.swimming.is_some();
        if swim_changed {
            tracing::debug!(
                swimming = self.swimming.is_some(),
                category = ?self.swimming.map(|l| l.category),
                surface = ?self.swimming.map(|l| l.surface),
                ground = ?stand_at,
                "swim state changed"
            );
        }

        // **Planted on the ground only when not swimming**, and this order is
        // the whole of it.
        //
        // The standing assignment used to run unconditionally, a few lines
        // earlier, and then the buoyancy below read `live.position.z` -- which
        // it had just set to the riverbed. Buoyancy closes a *fraction* of the
        // remaining gap per frame, so each frame started at the bottom, rose
        // three per cent of the way, and was put back. The character would
        // have walked along the bed of the river with the swim flag set and
        // the stroke cycle playing: a failure that looks like the feature
        // half-working rather than like an ordering mistake.
        //
        // Sampling the ground is still unconditional -- the swim test needs it,
        // since what makes water swimmable is how far it stands above the bed.
        // It is only the *assignment* that a swimmer opts out of.
        if self.swimming.is_none() {
            if let Some(z) = stand_at {
                live.position.z = z;
            }
        }

        if let Some(liquid) = self.swimming {
            // A jump does not survive hitting water, and neither does the
            // fall it would otherwise report. Cleared rather than left to
            // finish: a character who lands in a lake has landed, and an arc
            // still running underwater would keep pulling them down through
            // it.
            self.jump = None;

            // Where the body floats to with nothing pushing it: head above the
            // surface, body below.
            let rest = liquid.surface - SWIM_FLOAT;
            let floor = stand_at.unwrap_or(live.position.z);
            let mut z = live.position.z;
            if self.keys.up {
                z += SWIM_CLIMB_RATE * dt;
            } else if desired.is_moving() && self.camera_pitch < -DIVE_PITCH_DEADZONE {
                // **Diving is steered by the camera, not by a key.** That is
                // how the original does it, and it is also the only control
                // already in the player's hands that carries a vertical
                // direction -- adding a second one would give two ways to sink
                // that could disagree.
                z += self.camera_pitch.sin() * live_pace(desired).abs() * dt;
            } else {
                // Buoyancy: a fraction of the remaining distance per second
                // rather than a fixed rise, for the reason the camera's height
                // easing and the creature turn rate both document -- a rate
                // cap falls arbitrarily far behind whenever the input moves
                // faster than the cap, and a character dropped into a deep
                // lake is exactly that input.
                z += (rest - z) * (1.0 - (-dt / BUOYANCY_TAU).exp());
            }
            // Never above the surface and never through the bed. `max` on the
            // ceiling because in water shallower than `SWIM_FLOAT` the rest
            // height is *below* the bed, and clamping to an inverted range
            // panics.
            live.position.z = z.clamp(floor, rest.max(floor));
        }

        if let Some(jump) = self.jump.as_mut() {
            let down = jump.advance(dt);
            live.position.z += jump.height;
            if down {
                landed = Some(jump.elapsed_ms);
                self.jump = None;
            }
        }

        let position = Position {
            x: live.position.x,
            y: live.position.y,
            z: live.position.z,
            orientation: live.orientation,
        };

        // Every packet from here carries the *whole* movement state, jump
        // included, and only the opcode says what changed. Building the info
        // once and reusing it is what keeps that true: an earlier version
        // computed flags separately per branch, which is precisely how a
        // heartbeat comes to disagree with the start it is continuing.
        let airborne = self.jump.as_ref();
        // **The swimming bit and the pitch field travel together or not at
        // all.** `MovementInfo::has_pitch` emits a float whenever this flag is
        // set, so setting it here is what puts the field in every packet from
        // now on -- and the pitch that field carries is the camera's, because
        // while swimming the camera is what steers the dive. Sending the flag
        // and a stale zero would tell the server a swimmer is permanently
        // level.
        let swimming = self.swimming.is_some();
        let pitch = swimming.then_some(self.camera_pitch);
        let info_now = |live: &live::LiveWorld, extra: u32| MovementInfo {
            flags: desired.flags()
                | extra
                | if airborne.is_some() {
                    movement_flags::FALLING
                } else {
                    0
                }
                | if swimming { movement_flags::SWIMMING } else { 0 },
            time: live.connection.tick(),
            position,
            pitch,
            fall_time: airborne.map(|j| j.elapsed_ms).unwrap_or(0),
            falling: airborne.map(|j| ::world::movement::Falling {
                velocity: j.velocity,
                sin_angle: j.sin_angle,
                cos_angle: j.cos_angle,
                xy_speed: j.xy_speed,
            }),
            ..MovementInfo::default()
        };

        // **The swim transition goes first and unconditionally**, for exactly
        // the reason the landing above does: releasing a key on the frame the
        // water is entered would send the key change and swallow the entry,
        // leaving the server holding a character it believes is still walking
        // -- across a lake bed. Two different facts about one frame, and both
        // have to travel.
        if swim_changed {
            let opcode = if swimming {
                ClientOpcode::MoveStartSwim
            } else {
                ClientOpcode::MoveStopSwim
            };
            let info = info_now(live, 0);
            if let Err(e) = live.connection.send_movement(opcode, live.guid, &info) {
                tracing::warn!("sending swim transition failed: {e:#}");
            }
            self.last_heartbeat = Instant::now();
        }

        // The landing goes first and unconditionally.
        //
        // It is *not* an `else` of the transition below, which is where this
        // was first written and was wrong: releasing a key on the very frame
        // the ground is reached would have sent the key change and swallowed
        // the landing, leaving the server holding a character it believes is
        // still in the air. The two are different facts about the same frame
        // and both have to travel.
        if let Some(fall_time) = landed {
            let info = MovementInfo {
                // The falling bit is *cleared* here: this packet is the
                // statement that the fall is over. `fall_time` still carries
                // how long it lasted, which is what fall damage is computed
                // from.
                flags: desired.flags(),
                time: live.connection.tick(),
                position,
                fall_time,
                ..MovementInfo::default()
            };
            if let Err(e) =
                live.connection
                    .send_movement(ClientOpcode::MoveFallLand, live.guid, &info)
            {
                tracing::warn!("sending landing failed: {e:#}");
            }
            self.last_heartbeat = Instant::now();
        }

        let transitions = ::world::motion::Motion::transitions(self.live_move, desired);
        if !transitions.is_empty() {
            for opcode in transitions {
                let info = info_now(live, 0);
                if let Err(e) = live.connection.send_movement(opcode, live.guid, &info) {
                    tracing::warn!("sending movement failed: {e:#}");
                }
            }
            self.live_move = desired;
            self.last_heartbeat = Instant::now();
        } else if landed.is_none() && (desired.is_moving() || airborne.is_some()) {
            if self.last_heartbeat.elapsed() >= LIVE_HEARTBEAT_EVERY {
                let info = info_now(live, 0);
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

        // **The camera orbits the character; it does not sit behind them and
        // tilt.** Both angles are rebuilt here every frame, and the eye is
        // placed on a sphere around a point at the character's chest, so
        // whatever the pitch, the view still points at them.
        //
        // The first version placed the eye at a fixed height behind the
        // character and left pitch to the mouse. Swinging left and right was
        // then correct -- the eye really did travel around them -- while
        // dragging up and down only re-aimed a camera that stayed put, so the
        // character slid up and down the frame and out of it. Reported as the
        // camera keeping the player centred one way and "they go everywhere"
        // the other, which is exactly what half an orbit looks like.
        //
        // The yaw is *not* simply the character's orientation, though it was
        // at first. Recomputing it here is what makes the camera follow -- but
        // a mouse drag also wrote a yaw, and this overwrote it a millisecond
        // later, so dragging sideways did nothing at all. Both angles now
        // accumulate as offsets the drag owns and this applies, rather than two
        // places writing one field.
        // **The camera follows height with a lag, and the character does
        // not.** Standing is a discrete decision -- the feet are on this
        // triangle or that one -- so walking up the church steps moves Z in
        // jumps of a riser, and crossing where a building's floor meets the
        // terrain can flip between two answers a hair apart on consecutive
        // frames. Rigidly attached, the camera reproduces every one of those
        // as a shake, which is what was reported.
        //
        // Only the vertical is eased. Smoothing the horizontal too would make
        // the camera trail behind a running character, trading a shake for a
        // lag nobody asked for.
        let follow_z = Self::camera_follow_z(&mut self.camera_z, live.position.z, dt);
        let camera_at = glam::Vec3::new(live.position.x, live.position.y, follow_z);
        let placed = orbit_around(
            camera_at,
            live.orientation + self.camera_yaw_offset,
            self.camera_pitch,
            self.camera_distance,
        );
        // Kept off the terrain. Sampled here, while the scene is still
        // borrowed, because the camera is behind `&mut self` and the world
        // behind the renderer -- and the alternative, cloning a height field
        // per frame, would be absurd for twelve lookups.
        let focus = camera_at + glam::Vec3::Z * FOLLOW_HEIGHT;
        let eye = match self.renderer.as_ref().and_then(|r| r.scene.as_ref()) {
            Some(Scene::Streaming(world)) => {
                // Ground first, then walls, and the order matters: the ground
                // pass marches outwards and can only ever shorten the ray, so
                // the wall test that follows is asking about a line the eye
                // could actually have reached.
                let above_ground = pull_camera_out_of_the_ground(
                    focus,
                    placed.position,
                    |x, y| world.height_at(x, y),
                );
                pull_camera_in_front_of_walls(focus, above_ground, |from, to| {
                    world.first_obstruction(from, to)
                })
            }
            _ => placed.position,
        };
        if let Camera::Fly(fly) = &mut self.camera {
            // Only the placement, so the free-camera fields a screenshot or the
            // overlay may have set are left alone.
            fly.position = eye;
            fly.yaw = placed.yaw;
            fly.pitch = placed.pitch;
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

    /// Runs a slash command, or reports that it is not one.
    ///
    /// **`/` and not `.`**, and the difference is not cosmetic: a message
    /// beginning with `.` is a *server* command, parsed by the realm's own
    /// chat handler, which is how this client sends GM commands today. A
    /// message beginning with `/` is this client's, handled here and never
    /// sent. Sharing one prefix would mean guessing which end a line was meant
    /// for, and guessing wrong sends `/invite Watcher` out loud to everyone
    /// standing nearby.
    ///
    /// Every party request but the invite is **silent** -- the server
    /// acknowledges none of them -- so each of these says locally what it
    /// asked for. Without that, a `/leave` sent while not in a group and a
    /// `/leave` the server declined look identical, which is the failure mode
    /// the whole `world::group` block was written to escape.
    ///
    /// Returns `true` when the line was a command and has been dealt with.
    fn run_command(&mut self, line: &str) -> bool {
        let Some(rest) = line.strip_prefix('/') else {
            return false;
        };
        let (word, argument) = match rest.split_once(char::is_whitespace) {
            Some((word, argument)) => (word, argument.trim()),
            None => (rest, ""),
        };
        let word = word.to_ascii_lowercase();

        // Named rather than derived: a party command sent with no group is a
        // request the server will refuse in silence, and saying so here is
        // the difference between "nothing happened" and "you are not in a
        // group".
        let in_group = self
            .live
            .as_ref()
            .and_then(|live| live.state.party.as_ref())
            .is_some_and(|party| party.in_group());

        match word.as_str() {
            "invite" | "i" => {
                if argument.is_empty() {
                    self.notice("usage: /invite <name>".into());
                } else {
                    self.invite_to_party(argument);
                }
            }
            // One opcode does both, and the server decides which -- see
            // `ClientOpcode::GroupDisband`. Both spellings are accepted
            // because a leader typing `/leave` means the same thing.
            "leave" | "disband" => {
                if !in_group {
                    self.notice("you are not in a group.".into());
                } else {
                    self.leave_party();
                }
            }
            "kick" | "uninvite" => {
                if argument.is_empty() {
                    self.notice("usage: /kick <name>".into());
                } else {
                    self.kick_from_party(argument);
                }
            }
            "promote" | "leader" => {
                if argument.is_empty() {
                    self.notice("usage: /promote <name>".into());
                } else {
                    self.promote_in_party(argument);
                }
            }
            // `/p` mirrors `/invite`'s split: no argument switches the
            // sticky channel, an argument sends one line without touching
            // it. Refused locally either way when there is no group to hear
            // it -- a party line sent with no group is silently dropped by
            // the server, which is indistinguishable from a broken send.
            "p" | "party" => {
                if !in_group {
                    self.notice("you are not in a group.".into());
                } else if argument.is_empty() {
                    self.chat_channel = ChatChannel::Party;
                } else {
                    self.send_on_channel(&ChatChannel::Party, argument);
                }
            }
            "s" | "say" => {
                if argument.is_empty() {
                    self.chat_channel = ChatChannel::Say;
                } else {
                    self.send_on_channel(&ChatChannel::Say, argument);
                }
            }
            "y" | "yell" => {
                if argument.is_empty() {
                    self.chat_channel = ChatChannel::Yell;
                } else {
                    self.send_on_channel(&ChatChannel::Yell, argument);
                }
            }
            // Unlike the others, `/w` always needs a name -- there is no
            // sensible "whisper, to whoever" default -- so the split is on
            // the name rather than on whether there is an argument at all.
            "w" | "whisper" | "tell" => {
                if argument.is_empty() {
                    self.notice("usage: /w <name> [text]".into());
                } else {
                    let (name, text) = match argument.split_once(char::is_whitespace) {
                        Some((name, text)) => (name, text.trim()),
                        None => (argument, ""),
                    };
                    let channel = ChatChannel::Whisper(name.to_string());
                    if text.is_empty() {
                        self.chat_channel = channel;
                    } else {
                        self.send_on_channel(&channel, text);
                    }
                }
            }
            // Said rather than refused: a client that swallowed every
            // unrecognised slash line would make a typo indistinguishable
            // from a command that did nothing.
            other => self.notice(format!("no such command: /{other}")),
        }
        true
    }

    /// A line only this client says, in the scrollback where speaking happens.
    fn notice(&mut self, text: String) {
        self.chat.push(Line::Chat(local_notice(text)));
    }

    /// Asks a player, by name, to join the group.
    ///
    /// **The one group request that is answered**, which is why it is the one
    /// with nothing to report locally: `SMSG_PARTY_COMMAND_RESULT` comes back
    /// whether it worked or not, and that reply is drawn into the scrollback
    /// where it arrives. Saying "invited Watcher" here as well would state as
    /// fact the thing the reply is about to answer.
    fn invite_to_party(&mut self, name: &str) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if let Err(e) = live.connection.group_invite(name) {
            tracing::warn!("sending a group invite failed: {e:#}");
            self.notice(format!("could not invite: {e}"));
        }
    }

    /// Leaves the group, or breaks it up if this character leads it.
    fn leave_party(&mut self) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if let Err(e) = live.connection.group_disband() {
            tracing::warn!("leaving the group failed: {e:#}");
            self.notice(format!("could not leave: {e}"));
            return;
        }
        // Nothing local changes here. **The proof a leave worked is the
        // `SMSG_GROUP_LIST` that follows**, which is the server saying the
        // character is in no group -- clearing the party here as well would
        // make a refused request look exactly like an accepted one.
        self.notice("leaving the group.".into());
    }

    /// Throws a member out, resolving the name against the group.
    ///
    /// **The request names a guid**, so a name that is not in the party is
    /// refused here rather than sent: inventing a guid from a name the group
    /// does not hold would produce a well-formed request about nobody, and
    /// nothing acknowledges a kick.
    fn kick_from_party(&mut self, name: &str) {
        match self.party_guid(name) {
            Some(guid) => {
                let Some(live) = self.live.as_mut() else {
                    return;
                };
                if let Err(e) = live.connection.group_uninvite(guid) {
                    tracing::warn!("kicking from the group failed: {e:#}");
                    self.notice(format!("could not kick: {e}"));
                } else {
                    self.notice(format!("removing {name} from the group."));
                }
            }
            None => self.notice(format!("{name} is not in your group.")),
        }
    }

    /// Hands leadership to another member, by guid for the same reason.
    fn promote_in_party(&mut self, name: &str) {
        match self.party_guid(name) {
            Some(guid) => {
                let Some(live) = self.live.as_mut() else {
                    return;
                };
                if let Err(e) = live.connection.group_set_leader(guid) {
                    tracing::warn!("promoting a group member failed: {e:#}");
                    self.notice(format!("could not promote: {e}"));
                } else {
                    self.notice(format!("making {name} the group leader."));
                }
            }
            None => self.notice(format!("{name} is not in your group.")),
        }
    }

    /// A party member's guid, by name, case-insensitively.
    ///
    /// Reads the group list rather than the entity table on purpose: a member
    /// in another zone is not a replicated object at all, and looking them up
    /// among the things in visibility range would make kicking somebody
    /// depend on standing next to them.
    fn party_guid(&self, name: &str) -> Option<u64> {
        self.live
            .as_ref()?
            .state
            .party
            .as_ref()?
            .members
            .iter()
            .find(|member| member.name.eq_ignore_ascii_case(name))
            .map(|member| member.guid)
    }

    /// Answers the invite currently on screen.
    ///
    /// The pending invite is cleared here as well as by the server's reply,
    /// and that is not belt-and-braces: an accept is **silent**, and the group
    /// list confirming it takes a moment to arrive. Without clearing it the
    /// prompt stays up and a second click sends a second answer.
    fn answer_party_invite(&mut self, answer: ui::InviteAnswer) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let from = live
            .state
            .party_invite
            .as_ref()
            .map(|invite| invite.from.clone());
        let sent = match answer {
            ui::InviteAnswer::Accept => live.connection.group_accept(),
            ui::InviteAnswer::Decline => live.connection.group_decline(),
        };
        live.state.party_invite = None;
        match sent {
            Ok(()) => {
                if let Some(from) = from {
                    let verb = match answer {
                        ui::InviteAnswer::Accept => "joining",
                        ui::InviteAnswer::Decline => "declining",
                    };
                    self.notice(format!("{verb} {from}'s group."));
                }
            }
            Err(e) => {
                tracing::warn!("answering a group invite failed: {e:#}");
                self.notice(format!("could not answer the invite: {e}"));
            }
        }
    }

    /// Advances the party's loot method by one, wrapping after the fifth --
    /// the count `AzerothCore`'s own handler enforces (`lootMethod >
    /// NEED_BEFORE_GREED` is refused), read from its source per rule 2 and
    /// confirmed live rather than trusted outright: sending each of the five
    /// in turn produces a `SMSG_GROUP_LIST` carrying it back, and a sixth is
    /// silently dropped exactly like an invite to a name that does not
    /// exist. The threshold is left as it is -- this control changes one
    /// thing at a time.
    ///
    /// **Raw value `2` needs a master and got refused until this did
    /// something about it.** Cycling into it with no master looter set
    /// produced no reply at all, live -- every other transition answers with
    /// a fresh group list and this one alone did not, which is what
    /// distinguished "refused" from "the client never sent it" here. Rather
    /// than get stuck (the symptom this shipped with: the cycle would climb
    /// to `1` and then every further click silently did nothing, because `1
    /// -> 2` was the one transition the server was declining), this defaults
    /// the master to the reader's own guid the moment it is needed and never
    /// otherwise touches it -- whoever changes the rule becomes their own
    /// master looter until something else in this interface lets them pick
    /// someone.
    ///
    /// **Checked again here, not just trusted from the click.** The HUD only
    /// reports `party_loot_clicked` when it drew the line editable, which
    /// [`hud::party_loot_view`] sets from [`::world::group::Party::is_leader`]
    /// -- but that was true as of the *previous* frame's paint, and
    /// leadership can change between a paint and the click it produced (the
    /// group disbands, leadership is handed off). Silent, like every party
    /// request but the invite -- see [`Self::answer_party_invite`] for the
    /// one that is not, and why.
    ///
    /// **Rate-limited.** Clicking this repeatedly in quick succession was
    /// observed live to get the whole session disconnected -- the realm
    /// answered `10053` (connection aborted) rather than anything naming
    /// `CMSG_LOOT_METHOD` specifically, which reads exactly like the
    /// keepalive-ping punishment documented on [`::world::client::PING_INTERVAL`]
    /// wearing a different opcode. A fixed cooldown between sends is the same
    /// fix applied the same way.
    fn cycle_loot_method(&mut self) {
        const COOLDOWN: Duration = Duration::from_millis(600);
        if self
            .last_loot_method_change
            .is_some_and(|last| last.elapsed() < COOLDOWN)
        {
            return;
        }
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Some(party) = live.state.party.as_ref() else {
            return;
        };
        if !party.is_leader(live.guid) {
            return;
        }
        let Some(rule) = party.loot.as_ref() else {
            return;
        };
        const METHOD_COUNT: u32 = 5;
        // The one raw value observed live to need a member guid alongside
        // it -- see this method's doc comment.
        const NEEDS_MASTER: u32 = 2;
        let next_method = (rule.method as u32 + 1) % METHOD_COUNT;
        let master = if next_method == NEEDS_MASTER && rule.master == 0 {
            live.guid
        } else {
            rule.master
        };
        let threshold = rule.threshold as u32;
        let sent = live.connection.set_loot_method(next_method, master, threshold);
        self.last_loot_method_change = Some(Instant::now());
        if let Err(e) = sent {
            tracing::warn!("changing the loot method failed: {e:#}");
            // Locally, so a send that never left this machine is visible
            // where the click happened rather than only in a log nobody has
            // open -- the same reasoning as `send_chat`'s failure notice.
            self.notice(format!("could not change the loot method: {e}"));
        }
    }

    /// Sends a line of chat, and says so locally if it cannot be sent.
    ///
    /// A line beginning with `/` never reaches the wire -- see
    /// [`Self::run_command`] for why this client's own commands take a
    /// different prefix from the server's.
    fn send_chat(&mut self, line: &str) {
        if self.run_command(line) {
            return;
        }
        self.send_on_channel(&self.chat_channel.clone(), line);
    }

    /// Says `text` on `channel`, whether that came from the sticky channel or
    /// a one-shot `/p`, `/s`, `/y`, `/w`.
    ///
    /// **Party is checked again here**, not only at the `/p` call site in
    /// [`Self::run_command`]: the sticky channel can still be `Party` after
    /// the group it was switched for has since been left, and an ordinary
    /// unprefixed line typed in that state would otherwise be silently
    /// dropped by the server -- the exact failure this whole ticket exists to
    /// avoid, just reached from a different door.
    fn send_on_channel(&mut self, channel: &ChatChannel, text: &str) {
        if matches!(channel, ChatChannel::Party)
            && !self
                .live
                .as_ref()
                .and_then(|live| live.state.party.as_ref())
                .is_some_and(|party| party.in_group())
        {
            self.notice("you are not in a group.".into());
            return;
        }
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
        let (chat_type, target) = channel.wire();
        if let Err(e) = live.connection.say(chat_type, language, target, text) {
            tracing::warn!("sending chat failed: {e:#}");
            // Locally, so a failure to speak is visible where speaking
            // happens rather than only in a log nobody has open.
            self.chat.push(Line::Chat(local_notice(format!("could not send: {e}"))));
        }
        // Nothing is echoed locally on success: the server relays the line
        // back like any other, and adding it here too would show it twice.
    }

    /// Uses whatever is in an action slot.
    ///
    /// A bound key fires whether or not its bar is visible. Hiding a bar is
    /// about screen space, not about unbinding it -- and a key that silently
    /// stopped working because a checkbox was unticked would be a poor
    /// surprise mid-fight.
    ///
    /// Two kinds of thing can be in a slot and they travel by different
    /// opcodes -- see [`spells::AUTO_ATTACK`]. The slot itself still stores a
    /// plain spell id, so `ui.toml` is unchanged and a layout written before
    /// any of this existed still loads: which message to send is a fact about
    /// the *spell*, not about the slot, and deriving it here means a bar
    /// arranged by hand in the file behaves the same as one arranged in-game.
    fn activate_slot(&mut self, bar: usize, slot: usize) {
        let Some(spell) = self.hud.profile.bars.get(bar, slot) else {
            return;
        };
        if spell == spells::AUTO_ATTACK {
            self.toggle_auto_attack();
            self.action_flash = Some(((bar, slot), Instant::now()));
            return;
        }
        let target = self.target;
        let name = self.spells.name(spell);
        let Some(live) = self.live.as_mut() else {
            return;
        };
        match live.connection.cast_spell(spell, target) {
            Ok(()) => {
                tracing::debug!("cast {name} ({spell})");
                self.action_flash = Some(((bar, slot), Instant::now()));
                // Predicted, not server-confirmed -- see
                // `WorldState::predict_cooldown`'s doc comment for why the
                // server itself mostly stays quiet about an ordinary cast's
                // cooldown. A spell with none of its own reads `0` and
                // starts nothing.
                live.state.predict_cooldown(
                    spell,
                    Instant::now(),
                    self.spells.cooldown_ms(spell),
                );
            }
            Err(e) => {
                tracing::warn!("casting {spell} failed: {e:#}");
                self.chat.push(Line::Chat(local_notice(format!("could not cast: {e}"))));
            }
        }
    }

    /// Leaves the ground, if there is ground to leave.
    ///
    /// The jump is announced *here*, at the press, rather than by the movement
    /// driver on its next pass. `MSG_MOVE_JUMP` is the packet that tells the
    /// server a jump began and carries the take-off velocity it will simulate
    /// against; a heartbeat that merely arrived with the falling bit set would
    /// leave the server to guess when the character left the ground, and it
    /// does not guess -- it believes the position it was last told.
    ///
    /// Refused while already airborne. Double-jumping is not a thing the
    /// server accepts, and sending a second take-off mid-arc reports a
    /// velocity from an altitude the server has not seen yet.
    ///
    /// Refused while swimming too, for a different reason: there is no ground
    /// to push off. The key that would jump is bound to rising instead -- see
    /// the `KeyCode::Space` branch -- so this is the second of two independent
    /// refusals, and it is here as well as there because a caller that reached
    /// this from somewhere else would otherwise launch a swimmer out of a lake.
    fn begin_jump(&mut self) {
        use ::world::{ClientOpcode, MovementInfo, Position};

        if self.jump.is_some() || self.swimming.is_some() {
            return;
        }
        let moving = self.live_move;
        let Some(live) = self.live.as_mut() else {
            return;
        };
        // The heading at take-off, kept for the whole arc: a jump carries the
        // direction it began with, which is why turning in mid-air does not
        // steer it.
        let jump = ::world::motion::Jump::begin(moving.direction(live.orientation), LIVE_RUN_SPEED);
        let info = MovementInfo {
            flags: moving.flags() | ::world::update::movement_flags::FALLING,
            time: live.connection.tick(),
            position: Position {
                x: live.position.x,
                y: live.position.y,
                z: live.position.z,
                orientation: live.orientation,
            },
            fall_time: 0,
            falling: Some(::world::movement::Falling {
                velocity: jump.velocity,
                sin_angle: jump.sin_angle,
                cos_angle: jump.cos_angle,
                xy_speed: jump.xy_speed,
            }),
            ..MovementInfo::default()
        };
        if let Err(e) = live
            .connection
            .send_movement(ClientOpcode::MoveJump, live.guid, &info)
        {
            tracing::warn!("sending jump failed: {e:#}");
            return;
        }
        self.jump = Some(jump);
        self.last_heartbeat = Instant::now();
    }

    /// Starts or stops swinging at the current target.
    ///
    /// **Auto-attack is a state, not an action**, which is why this is a
    /// toggle rather than a send: `SMSG_ATTACKSTART` and `SMSG_ATTACKSTOP`
    /// bracket it, `WorldState::attacking` already folds both, and pressing
    /// the key a second time has to end the fight rather than start a second
    /// one. Reading whether we are attacking out of replicated state rather
    /// than keeping a local flag is deliberate: the server ends an attack on
    /// its own when the target dies or walks out of range, and a local flag
    /// would then be inverted -- the next press would send a stop for a fight
    /// that was already over, and look like the key had failed.
    ///
    /// Refusals are silent on the wire (see `crate::spells::AUTO_ATTACK` and
    /// `world::combat`), so the two conditions this client can check itself --
    /// having a target at all, and being connected -- are reported in the
    /// chat log rather than left to be inferred from nothing happening.
    fn toggle_auto_attack(&mut self) {
        let attacking = self
            .live
            .as_ref()
            .is_some_and(|live| live.state.attacking.contains_key(&live.guid));
        if attacking {
            self.stop_auto_attack();
        } else {
            self.start_auto_attack();
        }
    }

    /// Begins swinging, and says so if there is nothing to swing at.
    ///
    /// Separate from the toggle because right-clicking a creature must *start*
    /// a fight rather than flip one: right-clicking the thing you are already
    /// fighting would otherwise stop the fight, which is the opposite of what
    /// the gesture means everywhere it exists. Sending a second start at a
    /// target already being attacked is harmless -- the server is already in
    /// that state.
    fn start_auto_attack(&mut self) {
        let Some(target) = self.target else {
            self.chat
                .push(Line::Chat(local_notice("You have no target.".to_string())));
            return;
        };
        let Some(live) = self.live.as_mut() else {
            return;
        };
        match live.connection.attack_swing(target) {
            Ok(()) => tracing::debug!("auto-attack started on {target:#x}"),
            Err(e) => {
                tracing::warn!("starting auto-attack failed: {e:#}");
                self.chat
                    .push(Line::Chat(local_notice(format!("could not attack: {e}"))));
            }
        }
        // Swinging with the sword still on your back is the state this client
        // was in until now. **Nothing on the server fixes it** -- a whole fight
        // was driven against the realm without the sheath state moving off
        // zero, because drawing a weapon is a decision the client makes and
        // reports. See `world::combat::SheathState`.
        self.set_sheath(::world::combat::SheathState::Melee);
    }

    /// Draws or stows the weapon, and tells the server so other players see it.
    ///
    /// The drawn state is not tracked here on purpose: it is read back out of
    /// replicated state like everyone else's, because the server echoes this
    /// field for its sender -- see `world::state::Entity::sheath`. Keeping a
    /// local copy as well would give two answers that agree until they do not.
    fn set_sheath(&mut self, wanted: ::world::combat::SheathState) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if live.state.get(live.guid).map(|e| e.sheath()) == Some(wanted) {
            return;
        }
        if let Err(e) = live.connection.set_sheathed(wanted) {
            tracing::warn!("could not set sheath state: {e:#}");
        }
    }

    fn stop_auto_attack(&mut self) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        match live.connection.attack_stop() {
            Ok(()) => tracing::debug!("auto-attack stopped"),
            Err(e) => {
                tracing::warn!("stopping auto-attack failed: {e:#}");
                self.chat
                    .push(Line::Chat(local_notice(format!("could not stop attacking: {e}"))));
            }
        }
    }

    /// Whether right-clicking this thing should start a fight.
    ///
    /// **This is deliberately not a hostility test, because this client cannot
    /// yet make one.** A unit's faction arrives as `UNIT_FACTION`, but turning
    /// that into "hostile to *me*" needs `FactionTemplate.dbc`, which is not
    /// transcribed -- so inventing a judgement here would be exactly the
    /// fabricated-number problem `describe_cast_failure` exists to avoid, one
    /// layer up: a client that decides a guard is hostile attacks the guard.
    ///
    /// What it does instead is rule out the things that are *never* a fight on
    /// any reading -- yourself, a corpse, a bench, something already dead --
    /// and let the server arbitrate the rest, which it does anyway and is the
    /// only party that actually knows. The cost is that right-clicking a
    /// friendly NPC sends a swing the server refuses; the alternative was
    /// guessing, and a wrong guess here is an unprovoked attack rather than a
    /// blank.
    fn is_attack_candidate(&self, guid: u64) -> bool {
        let Some(live) = self.live.as_ref() else {
            return false;
        };
        if guid == live.guid {
            return false;
        }
        let Some(entity) = live.state.get(guid) else {
            return false;
        };
        matches!(
            entity.object_type,
            ::world::ObjectType::Unit | ::world::ObjectType::Player
        ) && !entity.is_dead_or_ghost()
    }

    /// Starts, stops or leaves alone the zone's music and ambience.
    ///
    /// Called every frame and cheap when nothing has changed -- each channel
    /// remembers what it is playing and returns immediately when asked for the
    /// same thing, which is the only reason calling it at the frame rate is
    /// sane.
    ///
    /// **A missing area is not silence.** `area_at` returns `None` while the
    /// tile under the player is still streaming in, and treating that as "no
    /// zone" would cut the music every time the player outran the loader. The
    /// last known area is kept and used until a real one replaces it.
    fn update_sound(&mut self) {
        if self.audio.is_none() {
            return;
        }
        // Where the character actually is. Read from the live session rather
        // than from replicated state, which holds the login position forever
        // -- the trap this project has now walked into from three separate
        // callers.
        let Some(at) = self.live.as_ref().map(|live| live.position) else {
            return;
        };

        let streaming_world = self
            .renderer
            .as_ref()
            .and_then(|r| r.scene.as_ref())
            .and_then(|scene| match scene {
                Scene::Streaming(world) => Some(world),
                _ => None,
            });
        let area = match streaming_world.and_then(|world| world.area_at(at.x, at.y)) {
            Some(area) => {
                self.area = Some(area);
                area
            }
            None => match self.area {
                Some(area) => area,
                None => return,
            },
        };

        // Without a clock, treat it as day. An offline view has no realm time
        // and picking night for it would be an odd default.
        let hour = self.live.as_ref().and_then(|live| {
            let (time, learned) = live.state.game_time?;
            Some(time.advanced(learned.elapsed()).minute_of_day() as f32 / 60.0)
        });
        let when = sound::TimeOfDay::at_hour(hour.unwrap_or(12.0));
        let (music, ambience) = self
            .sounds
            .zone(area)
            .map(|zone| zone.for_time(when))
            .unwrap_or((None, None));

        // One roll per call is fine: a channel only consults it when it is
        // actually starting something, which is rare.
        let roll = self.last_frame.elapsed().subsec_nanos() as f32 / 1_000_000_000.0;
        // **Logged when it changes, not every frame.** A zone's music is the
        // sort of thing that is either obviously working or obviously not, and
        // "obviously" needs an ear -- so this leaves a trail that says what it
        // decided, which is readable without one. That has caught more in this
        // project than looking has.
        if (self.music.playing(), self.ambience.playing()) != (music, ambience) {
            tracing::debug!(
                "area {area} at {when:?}: music {:?} -> {music:?}, ambience {:?} -> {ambience:?}",
                self.music.playing(),
                self.ambience.playing(),
            );
        }

        let Some(audio) = self.audio.as_ref() else {
            return;
        };
        let mixer = audio.mixer();


        // **Footsteps.** The model says when its feet land -- see
        // `m2::event::FOOTFALL`, which carries the measurement -- and the
        // ground says what they land on, so neither half is a timer this
        // client invented.
        //
        // The player's own only. There is no distance attenuation anywhere in
        // this file, so every creature in view would step as loudly as the
        // character does, and a starting zone has ninety-five of them.
        if let (Some(live), Some(world)) = (self.live.as_ref(), streaming_world) {
            let crossed = match world.footfalls_of(live.guid) {
                Some((sequence, times, now, duration)) => {
                    let fired = sound::footfalls_crossed(
                        self.footstep_phase,
                        sequence,
                        now,
                        duration,
                        &times,
                    );
                    self.footstep_phase = Some((sequence, now));
                    fired
                }
                None => {
                    // Nothing with feet in it is playing. Forgetting the phase
                    // is the point: returning to a walk must not fire for
                    // everything that "happened" while standing still.
                    self.footstep_phase = None;
                    0
                }
            };
            if crossed > 0 {
                // **A floor outranks the ground under it**, which is the
                // whole point: a character on the abbey's flagstones is not
                // standing on Elwynn's grass, however directly above it they
                // are. `floor_material` is `None` outdoors and for the 91% of
                // WMO materials that name no terrain, and then the ground
                // answers as before.
                let footing = match self.floor_material {
                    Some(row) => Some(sound::Footing::Surface(row as u32)),
                    None => world
                        .footing_at(live.position.x, live.position.y)
                        .map(sound::Footing::Ground),
                };
                if let Some(id) = live
                    .state
                    .get(live.guid)
                    .and_then(|entity| entity.display_id())
                    .and_then(|display| self.sounds.footstep(display, footing, self.wading))
                {
                    // Once, however many landed. Two feet inside one frame is a
                    // very short cycle rather than two steps a person could
                    // hear apart, and playing both stacks two copies of the
                    // same file at the same instant.
                    self.pending_sounds.push((id, false));
                    // One line per step, at trace. A footstep is the kind of
                    // feature that is either obviously working or obviously
                    // not, and "obviously" needs an ear -- so this leaves a
                    // trail saying which ground it thought it was on, which
                    // is readable without one. `crossed` is printed too: more
                    // than one means the cycle is short enough that a frame
                    // spans two contacts, which is worth knowing before
                    // anyone reports steps sounding sparse.
                    tracing::trace!(
                        "footstep: sound {id} on ground {footing:?}, wading {}, {crossed}                          contact(s) this frame",
                        self.wading,
                    );
                }
            }
        }

        // Combat sounds queued since the last frame. Swept every frame either
        // way -- a finished sink that is never dropped is a slow leak, which
        // is the kind that survives.
        self.effects.sweep();
        let volume = self.args.effects_volume;

        // A creature that has just started attacking someone. Compared against
        // last frame's set rather than driven by the packet, so it survives
        // the fold that reduces `SMSG_ATTACKSTART` to a counter.
        if let Some(live) = self.live.as_ref() {
            let now: std::collections::HashSet<u64> =
                live.state.attacking.keys().copied().collect();
            for guid in now.difference(&self.attackers) {
                if let Some(id) = live
                    .state
                    .get(*guid)
                    .and_then(|entity| entity.display_id())
                    .and_then(|display| self.sounds.creature(display))
                    .and_then(|voice| voice.get(sound::Voice::Aggro))
                {
                    self.pending_sounds.push((id, false));
                }
            }
            self.attackers = now;
        }
        for (id, impact) in std::mem::take(&mut self.pending_sounds) {
            if impact {
                // Held back so the clang lands with the blade rather than with
                // the packet -- see `Effects::delayed`.
                self.effects
                    .play_after(Duration::from_millis(self.impact_delay_ms), id, volume);
            } else {
                self.effects
                    .play(mixer, &self.sounds, &mut self.chain, id, volume, roll);
            }
        }
        self.effects
            .tick(mixer, &self.sounds, &mut self.chain, roll);
        self.music.play(
            mixer,
            &self.sounds,
            &mut self.chain,
            music,
            self.args.music_volume,
            roll,
        );
        self.ambience.play(
            mixer,
            &self.sounds,
            &mut self.chain,
            ambience,
            self.args.ambience_volume,
            roll,
        );
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
        // Loaded alongside the spellbook rather than the first time the bag
        // window opens. Both read large tables, and doing it here puts the
        // cost in the one place that already pays it -- opening a window
        // mid-fight and stalling a frame on `Item.dbc` would be a hitch a
        // player could feel and could not explain.
        self.items = items::Items::load(&mut self.chain);
        self.sounds = sound::Sounds::load(&mut self.chain);
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

        // **Whose bars are these?** Done here rather than at login because it
        // needs the spellbook: a leftover arrangement is taken to belong to
        // this character only if this character can cast all of it, and that
        // question cannot be asked before `SMSG_INITIAL_SPELLS` arrives.
        let castable_by_us: std::collections::HashSet<u32> = known.iter().copied().collect();
        let name = live.character.clone();
        let outcome = self
            .hud
            .use_character(&name, &|id| castable_by_us.contains(&id));
        tracing::info!("action bars for {name}: {outcome:?}");

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

    /// Which of the character's known spells belong on a bar or in the book.
    ///
    /// One function rather than the same three lines in the spellbook view and
    /// the seeder: they must agree about what a spellbook contains, and two
    /// copies of a filter agree only until one of them is changed.
    /// Takes its two pieces rather than `&self` on purpose: the caller drawing
    /// the book already holds the renderer mutably, and a method borrowing all
    /// of `self` could not be called from there.
    fn castable_spells(live: Option<&live::LiveWorld>, book: &spells::Spellbook) -> Vec<u32> {
        let Some(live) = live else {
            return Vec::new();
        };
        let known: Vec<u32> = live.state.spells.spells.iter().map(|s| s.id).collect();
        let class = live
            .state
            .get(live.guid)
            .and_then(|entity| entity.class())
            .unwrap_or(1);
        book.castable(&known, class)
    }

    /// One line per objective of a quest in the log: what it wants, and how
    /// far along it is.
    ///
    /// **Two objectives of a quest are counted from two different places, and
    /// that is not an inconsistency.** A kill or a use is counted by the
    /// *server*, in the player's own quest-log counters -- see
    /// `world::update::fields::QUEST_LOG_COUNTS`, where the packing was
    /// measured. An item objective is not there at all: `.additem` moves
    /// nothing in those fields, because the original client counts the items
    /// in the bags itself and the server only checks at hand-in. So this
    /// counts the bags for those, which this client can do because inventory
    /// is replicated.
    ///
    /// **The objective's *wire* slot indexes the counter, never its position
    /// in this list**, which is pruned. See `world::quest::QuestObjective`.
    /// What to call an item entry: the server's answer if it has given one,
    /// and `Item 2224` until then.
    ///
    /// **One helper rather than the lookup at each call site.** Four places
    /// draw an item name -- bag squares, the character panel, loot rows and
    /// a quest's item objectives -- and this project has already watched a
    /// returned value be silently dropped by three separate callers. A
    /// caller that forgets the cache here does not draw a wrong name, it
    /// draws the *old* one, which looks like the feature not working.
    ///
    /// A refusal (`Some(None)` -- the server says there is no such entry)
    /// falls back to the entry too. That is deliberate: an item the server
    /// disowns still occupies a square, and printing nothing would leave a
    /// blank nobody could diagnose.
    /// Takes the two pieces it reads rather than `&self`, deliberately: the
    /// frame builder holds `self.renderer` mutably for its whole length, so
    /// anything borrowing all of `self` cannot be called from inside it.
    fn item_name(live: Option<&live::LiveWorld>, items: &items::Items, entry: u32) -> String {
        live.and_then(|live| live.state.names.item(entry))
            .flatten()
            .and_then(|info| info.name.clone())
            .unwrap_or_else(|| items.name(entry))
    }

    fn quest_progress(&self, quest: &::world::quest::QuestInfo) -> Vec<String> {
        let Some(live) = self.live.as_ref() else {
            return Vec::new();
        };
        let Some(player) = live.state.get(live.guid) else {
            return Vec::new();
        };
        let mut lines = Vec::new();

        for objective in &quest.objectives {
            let Some(done) = player.quest_objective_progress(quest.id, objective.slot) else {
                continue;
            };
            // A slot carrying only an item drop has nothing to kill and no
            // count of its own; the drop is counted with the item objectives
            // below.
            let Some(target) = objective.target else {
                continue;
            };
            // The server's own wording when it has one; otherwise the name if
            // this client has ever been told it, and the id if not. **Never an
            // invented name** -- an id is checkable and a plausible wrong name
            // is not.
            let what = if !objective.text.is_empty() {
                objective.text.clone()
            } else {
                match target {
                    ::world::quest::ObjectiveTarget::Creature(entry) => live
                        .state
                        .names
                        .creature(entry)
                        .flatten()
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("creature {entry}")),
                    ::world::quest::ObjectiveTarget::GameObject(id) => format!("object {id}"),
                }
            };
            lines.push(format!("{what}: {done}/{}", objective.count.max(1)));
        }

        for item in &quest.item_objectives {
            let carried: u32 = ::world::inventory::carried(&live.state, live.guid)
                .iter()
                .filter(|held| held.item.entry == Some(item.item))
                .map(|held| held.item.count)
                .sum();
            lines.push(format!(
                "{}: {carried}/{}",
                Self::item_name(self.live.as_ref(), &self.items, item.item),
                item.count.max(1)
            ));
        }

        lines
    }

    /// Takes hold of the pointer for the duration of a drag.
    ///
    /// Hidden, confined to the window, and pinned to where the gesture
    /// started. **Confined rather than locked on purpose**: winit implements
    /// `Locked` on some platforms and `Confined` on others, and the pin below
    /// is what actually gives an unlimited turn, so the grab only has to stop
    /// the pointer escaping. A platform that refuses both still turns the
    /// camera -- it simply runs out of window, which is what it did before.
    fn capture_cursor(&mut self, window: &Window) {
        if self.cursor_captured {
            return;
        }
        self.capture_anchor = self.last_cursor;
        if window.set_cursor_grab(CursorGrabMode::Confined).is_err()
            && window.set_cursor_grab(CursorGrabMode::Locked).is_err()
        {
            tracing::debug!("this platform holds neither cursor grab mode");
        }
        window.set_cursor_visible(false);
        self.cursor_captured = true;
    }

    /// Gives the pointer back, where the drag began.
    ///
    /// Putting it back matters: a captured pointer has been warped to the
    /// anchor all through the drag, so releasing without restoring it leaves
    /// the visible cursor wherever the last warp put it -- which is the anchor
    /// anyway, but only by luck, and a platform that refused the warp would
    /// hand back a cursor at the window edge.
    fn release_cursor(&mut self, window: &Window) {
        if !self.cursor_captured {
            return;
        }
        let _ = window.set_cursor_grab(CursorGrabMode::None);
        window.set_cursor_visible(true);
        if let Some((x, y)) = self.capture_anchor.take() {
            let _ = window.set_cursor_position(winit::dpi::PhysicalPosition::new(x, y));
            self.last_cursor = Some((x, y));
        }
        self.cursor_captured = false;
    }

    /// Turns the camera from a raw device movement, while the pointer is held.
    ///
    /// **This exists because warping the pointer back to its anchor did not
    /// work, and the way it failed is worth keeping.** The first version read
    /// deltas from `CursorMoved` and then put the pointer back, so a drag
    /// could never run out of window. But a warp is a request to the OS whose
    /// own move event arrives later, and any real movement already queued
    /// behind it is then measured against the anchor rather than against where
    /// the pointer actually was -- so the camera jumped instead of turning.
    /// Worse, egui saw the pointer teleporting on every frame of a drag, and a
    /// widget that is somewhere else by the time the release lands cannot be
    /// clicked: the whole interface stopped responding.
    ///
    /// A raw device delta has no position in it at all, so there is nothing to
    /// fight over: the pointer sits still against the edge of the window while
    /// the mouse keeps reporting movement, and both the camera and egui get an
    /// honest answer.
    fn device_motion(&mut self, dx: f64, dy: f64) {
        if !self.cursor_captured {
            return;
        }
        // The gesture is still measured, so a press and release that moved the
        // mouse is a look rather than a click even though the pointer never
        // left the spot.
        let step = (dx * dx + dy * dy).sqrt();
        if self.press_at.is_some() {
            self.left_travel += step;
        }
        if self.right_press_at.is_some() {
            self.right_travel += step;
        }

        let Some(window) = self.window.clone() else {
            return;
        };
        let speed = self
            .hud
            .profile
            .camera
            .radians_per_pixel(window.inner_size().width as f32);
        let (turn, pitch) = (-dx as f32 * speed, dy as f32 * speed);
        if self.steering {
            // Right drag steers the character; the camera follows its facing.
            // See the `CursorMoved` branch this mirrors for why the swing is
            // folded away on the first movement rather than at the press.
            self.camera_yaw_offset = 0.0;
            if let Some(live) = self.live.as_mut() {
                live.orientation = (live.orientation + turn).rem_euclid(std::f32::consts::TAU);
            }
            self.camera_pitch = (self.camera_pitch
                - pitch * self.hud.profile.camera.pitch_sign())
            .clamp(-FOLLOW_PITCH_LIMIT, FOLLOW_PITCH_LIMIT);
            window.request_redraw();
            return;
        }
        if !self.dragging {
            return;
        }
        let following = self.live.is_some();
        match &mut self.camera {
            Camera::Orbit(c) => c.orbit(turn, pitch),
            Camera::Fly(_) if following => {}
            Camera::Fly(c) => c.look(turn, -pitch),
        }
        if following {
            self.camera_yaw_offset =
                (self.camera_yaw_offset + turn).rem_euclid(std::f32::consts::TAU);
            self.camera_pitch = (self.camera_pitch
                - pitch * self.hud.profile.camera.pitch_sign())
            .clamp(-FOLLOW_PITCH_LIMIT, FOLLOW_PITCH_LIMIT);
        }
        window.request_redraw();
    }

    /// Selects whatever is under the cursor, and tells the server.
    fn click_at(&mut self, at: (f64, f64)) {
        if let Some(picked) = self.pick_at(at) {
            self.set_target(picked);
        }
    }

    /// A right click: select, and start a fight if the thing selected is one.
    ///
    /// Right-click does *both* jobs in the game this is modelled on -- it is
    /// select-and-do-the-obvious-thing, where left-click only selects. Attack
    /// is the only "obvious thing" this client implements so far; talking to a
    /// vendor and looting a corpse are the same gesture and are not built.
    ///
    /// Clicking empty ground still clears the selection, exactly as a left
    /// click does. What it must not do is leave the previous target selected
    /// and attack *that* -- the click said "this", and "this" was nothing.
    fn right_click_at(&mut self, at: (f64, f64)) {
        let Some(picked) = self.pick_at(at) else {
            return;
        };
        self.set_target(picked);
        if let Some(guid) = picked {
            if self.is_talk_candidate(guid) {
                // **Before the attack branch, and that is a fix as much as a
                // feature.** Right-clicking a questgiver used to send a swing
                // the server refused -- `is_attack_candidate` rules out only
                // what is never a fight, and an innkeeper is not on that list.
                // `will_talk` is a replicated field rather than a guess, so
                // this is the one case that can be decided locally.
                self.greet(guid);
            } else if self.is_attack_candidate(guid) {
                self.start_auto_attack();
            } else if self.is_loot_candidate(guid) {
                // **The same gesture, split by whether the thing is alive.**
                // Right-click means "interact with that" everywhere it exists,
                // and on a body that is looting rather than swinging. The two
                // are mutually exclusive by construction --
                // `is_attack_candidate` already rules out anything dead -- so
                // this cannot both attack and loot.
                self.open_loot(guid);
            }
        }
    }

    /// What the questgiver window should be showing, or `None` when no
    /// conversation is open.
    ///
    /// **Rebuilt every frame from the cache and the log rather than kept.**
    /// Both can change under it -- an answer arrives, a quest is accepted --
    /// and a retained copy would show a stale Accept button on a quest already
    /// taken, which sends a request the server refuses for a reason the player
    /// cannot see.
    fn questgiver_view(&self) -> Option<ui::QuestgiverView> {
        let questgiver = self.questgiver.as_ref()?;
        let log: Vec<u32> = self
            .live
            .as_ref()
            .and_then(|live| live.state.get(live.guid))
            .map(|player| player.quest_log_ids())
            .unwrap_or_default();

        let Some(showing) = questgiver.showing else {
            // No single quest chosen: list what there is. An empty list is
            // drawn as an empty list, which is what an NPC with nothing to
            // offer this character genuinely has.
            return Some(ui::QuestgiverView::List {
                npc: questgiver.name.clone(),
                quests: questgiver
                    .offered
                    .iter()
                    .map(|id| {
                        let (title, level) = match self.quests.answer(*id) {
                            ::world::Answer::Known(quest) => {
                                (quest.title.clone(), quest.level)
                            }
                            _ => (format!("Quest {id}"), 0),
                        };
                        ui::QuestgiverRow {
                            id: *id,
                            title,
                            level,
                            turn_in: log.contains(id),
                        }
                    })
                    .collect(),
            });
        };

        let in_log = log.contains(&showing);
        let complete = self
            .live
            .as_ref()
            .and_then(|live| live.state.get(live.guid))
            .and_then(|player| player.quest_is_complete(showing))
            .unwrap_or(false);

        let ::world::Answer::Known(quest) = self.quests.answer(showing) else {
            // **Named and not described.** The window says what it is waiting
            // for rather than offering Accept over a blank body, which would
            // ask the player to agree to something nobody read to them.
            return Some(ui::QuestgiverView::Quest {
                id: showing,
                title: format!("Quest {showing}"),
                body: String::new(),
                objectives: Vec::new(),
                rewards: Vec::new(),
                action: ui::QuestgiverAction::Waiting,
            });
        };

        let action = match (in_log, complete) {
            (false, _) => ui::QuestgiverAction::Accept,
            (true, true) => ui::QuestgiverAction::Complete,
            (true, false) => ui::QuestgiverAction::Unfinished,
        };
        // Handing in shows what the quest says when finished; taking it shows
        // what the questgiver says. Falling back to the other rather than to a
        // blank, because plenty of quests leave one of the two empty.
        let body = if in_log && !quest.completed_text.is_empty() {
            quest.completed_text.clone()
        } else if !quest.details.is_empty() {
            quest.details.clone()
        } else {
            quest.objectives_text.clone()
        };

        Some(ui::QuestgiverView::Quest {
            id: showing,
            title: quest.title.clone(),
            body,
            objectives: if quest.objectives_text.is_empty() {
                Vec::new()
            } else {
                vec![quest.objectives_text.clone()]
            },
            // **Ids, not names.** Item names need `CMSG_ITEM_QUERY_SINGLE`,
            // which this client does not send yet (`foss-wow#56`), and a made
            // up name would be a fabricated string on a reward screen. An id
            // is checkable; a guess is not.
            rewards: quest
                .reward_items
                .iter()
                .map(|reward| format!("item {} x{}", reward.item, reward.count))
                .chain(
                    (quest.money > 0).then(|| format!("{} copper", quest.money)),
                )
                .collect(),
            action,
        })
    }

    /// Zooms the minimap if the pointer is over it, and says whether it did.
    ///
    /// The rectangle is computed from the layout rather than read off egui's
    /// hover state, the same choice the spellbook's wheel scrolling makes:
    /// the position is already known here, and asking egui would be
    /// consulting a second opinion about a question this code can answer.
    fn zoom_minimap(&mut self, notches: f32) -> bool {
        let element = self.hud.profile.get(ui::ElementId::Minimap);
        if !element.visible {
            return false;
        }
        let Some(r) = self.renderer.as_ref() else {
            return false;
        };
        let Some(pointer) = r.egui_ctx.input(|i| i.pointer.hover_pos()) else {
            return false;
        };
        let size = ui::frames::minimap::size(&self.hud.profile.style, element.scale);
        if !element.rect(r.egui_ctx.content_rect(), size).contains(pointer) {
            return false;
        }
        // Multiplicative, like the camera's: a fixed step is coarse at the
        // near end and useless at the far one. **The same sign as the
        // camera's, too** -- scrolling up pulls the camera in, so it has to
        // shrink the range here rather than grow it, or the two zooms fight
        // each other in the player's hand.
        self.minimap_range = (self.minimap_range * 0.8f32.powf(notches)).clamp(
            ui::frames::minimap::MIN_RANGE,
            ui::frames::minimap::MAX_RANGE,
        );
        true
    }

    /// Asks the questgiver for one quest's scroll, and the server for its
    /// text.
    ///
    /// Two different requests to two different ends, and both are needed:
    /// `CMSG_QUESTGIVER_QUERY_QUEST` is what makes the *accept* legal, and
    /// `CMSG_QUEST_QUERY` is what fills the window. Asking only the second
    /// would draw a quest that could not then be taken.
    fn ask_for_quest_scroll(&mut self, quest: u32) {
        let asking = self.quests.take_unknown(&[quest], 1);
        let Some(questgiver) = self.questgiver.as_ref() else {
            return;
        };
        let npc = questgiver.npc;
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if let Err(e) = live.connection.query_quest(npc, quest) {
            tracing::warn!("asking for quest {quest}'s scroll failed: {e:#}");
        }
        for quest in asking {
            if let Err(e) = live.connection.query_quest_info(quest) {
                tracing::warn!("asking what quest {quest} is failed: {e:#}");
                self.quests.give_up(quest);
            } else {
                self.quest_asked_at.insert(quest, Instant::now());
            }
        }
    }

    /// Presses the window's one button: take the quest, or hand it in.
    ///
    /// **Which of the two it is comes from the same place the button's label
    /// did**, rather than from a flag stored when the window opened. A copy
    /// would go stale exactly when it matters -- between the frame that drew
    /// `Accept` and the click that pressed it, the quest may already be in the
    /// log.
    fn act_on_quest(&mut self, quest: u32) {
        let Some(view) = self.questgiver_view() else {
            return;
        };
        let ui::QuestgiverView::Quest { action, .. } = view else {
            return;
        };
        let Some(npc) = self.questgiver.as_ref().map(|giver| giver.npc) else {
            return;
        };
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let sent = match action {
            ui::QuestgiverAction::Accept => live.connection.accept_quest(npc, quest),
            // `0` is the reward index. **Correct only where there is nothing
            // to choose**: a quest offering alternatives needs the player to
            // pick one, and this window has no way to say which yet -- so a
            // quest with choices would hand over the first, which is a wrong
            // answer rather than a missing feature.
            ui::QuestgiverAction::Complete => live.connection.complete_quest(npc, quest),
            // No button was drawn, so this cannot be reached by clicking one.
            ui::QuestgiverAction::Unfinished | ui::QuestgiverAction::Waiting => return,
        };
        if let Err(e) = sent {
            tracing::warn!("acting on quest {quest} failed: {e:#}");
            return;
        }
        // **The reward is taken as a second send, unconditionally.** The
        // server answers `CMSG_QUESTGIVER_COMPLETE_QUEST` with a reward screen
        // *or* with a still-wanted list, and it refuses the choose-reward that
        // follows in the second case -- which is exactly the right outcome,
        // and cheaper than a state machine that waits a frame to find out.
        if matches!(action, ui::QuestgiverAction::Complete) {
            if let Err(e) = live.connection.choose_quest_reward(npc, quest, 0) {
                tracing::warn!("taking quest {quest}'s reward failed: {e:#}");
            }
        }
        // Closed either way: the log is what says whether it worked, and a
        // window left open showing a stale Accept is worse than none.
        self.questgiver = None;
    }

    /// Whether right-clicking this thing should start a conversation.
    ///
    /// **The one interaction test this client can make locally**, because
    /// `UNIT_NPC_FLAGS` is replicated and non-zero means the unit offers
    /// *something* -- gossip, quests, a shop, an inn. Which of those it is
    /// stays the server's business; this only decides that talking is worth
    /// attempting, and a dead one is not.
    fn is_talk_candidate(&self, guid: u64) -> bool {
        let Some(live) = self.live.as_ref() else {
            return false;
        };
        if guid == live.guid {
            return false;
        }
        live.state
            .get(guid)
            .is_some_and(|entity| entity.will_talk() && !entity.is_dead_or_ghost())
    }

    /// Greets an NPC and opens the window its answer will fill.
    ///
    /// The window appears *empty* rather than not at all, unlike the loot
    /// window: a greeting is always answered by something, and a window that
    /// only appeared once the reply arrived would make a slow realm look like
    /// a click that did not register.
    fn greet(&mut self, guid: u64) {
        let name = self
            .live
            .as_ref()
            .and_then(|live| {
                live.state
                    .get(guid)
                    .map(|entity| hud::unit_name(&live.state, entity))
            })
            .unwrap_or_else(|| format!("Creature {guid:#x}"));
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if let Err(e) = live.connection.gossip_hello(guid) {
            tracing::warn!("greeting {guid:#x} failed: {e:#}");
            return;
        }
        self.questgiver = Some(Questgiver {
            npc: guid,
            name: name.clone(),
            offered: Vec::new(),
            showing: None,
        });
        // **Cleared before the new one is decided, not overwritten after.**
        // Greeting a plain NPC while a trainer's list is open would otherwise
        // leave that list on screen belonging to somebody the player has
        // stopped talking to, and its purchases would go to an NPC the server
        // no longer considers open -- refused in silence, which is the one
        // failure this client cannot diagnose.
        self.trainer = None;
        self.taxi = None;

        // **A trainer is asked in the same breath as it is greeted**, and the
        // flag is what decides. `UNIT_NPC_FLAGS` bit `0x10` is replicated, so
        // this is the same class of local decision `is_talk_candidate` already
        // makes -- and it is worth making rather than always sending, because
        // an ordinary NPC would answer a trainer request with nothing at all
        // and nothing is the one reply this client cannot interpret.
        //
        // The bit is **necessary and not sufficient**, and both halves were
        // measured. An Innkeeper Farley -- `0x10283`, every bit but this one
        // -- answers the identical request with *nothing*, from zero units
        // away, on the same character; that is the half saying the bit means
        // something rather than merely correlating with it. And a Grand
        // Master profession trainer carries `0x10` and still answers nothing
        // to a warrior who has none of its skill; that is the half saying it
        // does not mean enough. So an unanswered request leaves the window
        // open and empty rather than counting as an error, and the window
        // says which silence it is showing.
        // **A flight master is asked in the same breath as it is greeted**,
        // exactly like a trainer and off the same replicated flag word.
        // Dungar Longdrink, a Gryphon Master, carries 0x2003 and answers.
        const FLIGHTMASTER: u32 = 0x2000;
        let flies = self
            .live
            .as_ref()
            .and_then(|live| live.state.get(guid))
            .and_then(|entity| entity.npc_flags())
            .is_some_and(|flags| flags & FLIGHTMASTER != 0);
        if flies {
            if let Some(live) = self.live.as_mut() {
                match live.connection.taxi_query_nodes(guid) {
                    Ok(()) => self.taxi = Some(TaxiSession { npc: guid, menu: None }),
                    Err(e) => tracing::warn!("asking {guid:#x} for flights failed: {e:#}"),
                }
            }
        }

        if self.offers_training(guid) {
            if let Some(live) = self.live.as_mut() {
                match live.connection.trainer_list(guid) {
                    Ok(()) => {
                        self.trainer = Some(TrainerSession {
                            npc: guid,
                            name,
                            list: None,
                        })
                    }
                    Err(e) => tracing::warn!("asking {guid:#x} for a trainer list failed: {e:#}"),
                }
            }
        }
    }

    /// Whether this unit's replicated flags say it trains.
    ///
    /// Deliberately its own predicate beside [`Self::is_talk_candidate`]
    /// rather than an inline bit test, so the one place that names bit `0x10`
    /// is the place carrying the evidence for what it means. Confirmed by
    /// combination on three NPCs whose roles are known independently of the
    /// wire: Llane Beshere, a "Warrior Trainer", carries `0x33` -- gossip,
    /// questgiver, trainer, class trainer -- and answers a trainer request;
    /// an Innkeeper Farley carries `0x10283` with no trainer bit; a spirit
    /// healer standing over a corpse carries `0x4001`. Three flag words that
    /// differ, each agreeing with a role known from the creature's own name.
    fn offers_training(&self, guid: u64) -> bool {
        /// `UNIT_NPC_FLAG_TRAINER`. See [`App::offers_training`].
        const TRAINER: u32 = 0x10;
        self.live
            .as_ref()
            .and_then(|live| live.state.get(guid))
            .and_then(|entity| entity.npc_flags())
            .is_some_and(|flags| flags & TRAINER != 0)
    }

    /// Whether right-clicking this thing should open its loot.
    ///
    /// The mirror of [`Self::is_attack_candidate`] and deliberately as narrow:
    /// a dead unit, and not the player's own corpse. It makes no attempt to
    /// know whether there is anything *on* the body, because the client cannot
    /// know that until it asks -- and asking about an empty corpse is answered
    /// with a release rather than an error, so the cost of being wrong is one
    /// packet and no window.
    fn is_loot_candidate(&self, guid: u64) -> bool {
        let Some(live) = self.live.as_ref() else {
            return false;
        };
        if guid == live.guid {
            return false;
        }
        let Some(entity) = live.state.get(guid) else {
            return false;
        };
        matches!(entity.object_type, ::world::ObjectType::Unit) && entity.is_dead_or_ghost()
    }

    /// Asks what is on a corpse.
    ///
    /// The window that results is not opened here: it appears when the *server
    /// answers*, because until then this client does not know whether there is
    /// anything to show. An empty corpse is answered with a release, so the
    /// window correctly never appears for one -- see `world::state::WorldState::loot`.
    fn open_loot(&mut self, guid: u64) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if let Err(e) = live.connection.loot(guid) {
            tracing::warn!("could not open loot: {e:#}");
            return;
        }
        self.looting = Some(Looting::Asked(guid));
        tracing::debug!("asked to loot {guid:#x}");
    }

    /// Takes one row of the open loot.
    ///
    /// `take` carries what to ask for rather than a position -- see
    /// `ui::frames::loot`. A corpse whose earlier slots are gone still numbers
    /// the rest from where they were, so asking by row would take the wrong
    /// item, and nothing would report it.
    fn take_loot(&mut self, take: ui::frames::Take) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let result = match take {
            ui::frames::Take::Money => live.connection.loot_money(),
            ui::frames::Take::Item(slot) => live.connection.loot_item(slot),
        };
        if let Err(e) = result {
            tracing::warn!("could not take loot: {e:#}");
        }
    }

    /// Sends `CMSG_AUTOEQUIP_ITEM` for an item at a known location.
    ///
    /// Only `Where::Own` is sent: `Connection::equip_item` was confirmed by
    /// effect against an item in the player's own 39-slot array, and nothing
    /// here has confirmed the body a bag-nested source wants -- `foss-wow#55`
    /// left that unconfirmed rather than guess it, the same call made for the
    /// swap opcode. `at` being `None` covers both "nothing was there" and "the
    /// square was outside the backpack and `bags_where` never got that far".
    fn auto_equip(&mut self, at: Option<::world::inventory::Where>) {
        let Some(::world::inventory::Where::Own(slot)) = at else {
            return;
        };
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if let Err(e) = live.connection.equip_item(slot) {
            tracing::warn!("could not auto-equip: {e:#}");
        }
    }

    /// What a right-click on a bag square means, decided here because this is
    /// the side that knows what the item is.
    ///
    /// **Use beats equip, and an item that can do neither does nothing.** An
    /// item carrying an on-use spell is used where it sits; anything else is
    /// offered to the equipment slots as before. Deciding it in the `ui`
    /// crate is impossible -- it has a name, a count and an icon, and what an
    /// item *does* is server data that arrives with
    /// `SMSG_ITEM_QUERY_SINGLE_RESPONSE`.
    ///
    /// A square whose item has not been answered for yet falls through to the
    /// equip path rather than being swallowed. That is the safer of the two
    /// wrong answers: the server refuses an equip it does not like, where a
    /// silently dropped click reads as the window being broken -- which is
    /// exactly the report this whole area just came out of.
    fn activate_item(&mut self, at: Option<::world::inventory::Where>) {
        let Some(at) = at else { return };
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let held = ::world::inventory::carried(&live.state, live.guid)
            .into_iter()
            .find(|carried| carried.at == at);
        let usable = held.and_then(|carried| {
            let entry = carried.item.entry?;
            let spell = live.state.names.item(entry).flatten()?.use_spell()?;
            Some((carried, spell))
        });

        match usable {
            Some((carried, spell)) => {
                let (bag, slot) = at.address();
                let Some(live) = self.live.as_mut() else {
                    return;
                };
                if let Err(e) =
                    live.connection
                        .use_item(bag, slot, carried.item.guid, spell, None)
                {
                    tracing::warn!("could not use the item: {e:#}");
                }
            }
            None => self.auto_equip(Some(at)),
        }
    }

    /// Sends `SwapItemCandidate` for a completed bag-window drag.
    ///
    /// `from`/`to` are looked up through `bags_where` at the call site --
    /// see `HudResponse::move_item`'s doc comment for why a square position
    /// is not itself an address. Either end can be `Where::Own` (backpack, or
    /// an equipped slot dragged from the character panel) or
    /// `Where::InBag`, and the request names whichever pair it was given:
    /// there is nothing here restricting a move to same-container pairs.
    ///
    /// Whatever the server does with it is reported through
    /// `inventory_failures`/the object update that follows, not from here --
    /// nothing acknowledges this send on success, the same as `equip_item`.
    fn move_item(
        &mut self,
        from: Option<::world::inventory::Where>,
        to: Option<::world::inventory::Where>,
    ) {
        let (Some(from), Some(to)) = (from, to) else {
            return;
        };
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let (src_bag, src_slot) = from.address();
        let (dst_bag, dst_slot) = to.address();
        if let Err(e) = live
            .connection
            .swap_item_candidate(dst_bag, dst_slot, src_bag, src_slot)
        {
            tracing::warn!("could not move item: {e:#}");
        }
    }

    /// Closes the corpse, which is what unlocks it for anyone else.
    ///
    /// Sent on closing the window rather than left to the server: a corpse
    /// stays locked to whoever opened it, so a client that opens loot and
    /// wanders off leaves a body nobody can touch.
    fn release_loot(&mut self) {
        let Some(Looting::Open(guid)) = self.looting.take() else {
            // Nothing to release, or a request still waiting for its answer.
            // Releasing an unanswered request is the bug this enum exists to
            // prevent -- see `App::looting`.
            return;
        };
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if let Err(e) = live.connection.loot_release(guid) {
            tracing::warn!("could not release the corpse: {e:#}");
        }
        tracing::debug!("released {guid:#x}");
    }

    /// What is under the cursor, if the world is in a state to be asked.
    ///
    /// `None` means the question could not be put -- no world, or the pointer
    /// was over the interface -- which is different from `Some(None)`, the
    /// answer "nothing is there". Collapsing the two would make a click on a
    /// health bar clear the selection.
    fn pick_at(&self, at: (f64, f64)) -> Option<Option<u64>> {
        let r = self.renderer.as_ref()?;
        // A click on the interface belongs to the interface: clicking a health
        // bar must not target whatever is standing behind it.
        //
        // **Logged, because "the window ignored my click" and "the click went
        // past the window into the world" are the same report and want
        // opposite investigations.** One says the frame's own handler is not
        // reading the click; the other says the interface never claimed the
        // rectangle at all.
        if self.hud.captures_pointer(&r.egui_ctx) {
            tracing::debug!("click at {at:?} belongs to the interface");
            return None;
        }
        tracing::debug!(
            "click at {at:?} goes to the world; the interface claims {} rectangle(s)",
            self.hud.occupied_count()
        );
        // Read before the renderer's borrow narrows things: one function
        // answers this for the frame and for the click, so the list picked
        // through is the list drawn.
        let pace = self.animation_pace();
        let (Some(live), Some(Scene::Streaming(world))) = (self.live.as_ref(), r.scene.as_ref())
        else {
            return None;
        };

        let viewport = (r.config.width as f32, r.config.height as f32);
        let ray = self.camera.ray_through((at.0 as f32, at.1 as f32), viewport)?;
        // Rebuilt rather than cached: the same interpolated positions the
        // renderer drew this frame, so a click hits where the creature looks
        // like it is rather than where it last reported being.
        // The speed only chooses an animation, which a click test does not care
        // about -- but the same list has to come out here as the renderer drew,
        // so it is passed the same way rather than left at a default.
        let entities = drawable_with_own(
            live,
            pace,
            self.strafe_lean(),
            self.jump.is_some(),
            self.swimming.is_some(),
        );
        Some(hud::pick(&ray, &entities, &|display_id| {
            world.entity_bounds(display_id)
        }))
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
    /// Reads this realm's quest cache, and remembers where to write it back.
    ///
    /// **Failing to load is not failing to run.** A cache is an optimisation:
    /// without it every quest is simply asked for again. So a missing config
    /// directory or an unreadable file is logged and the client carries on
    /// with an empty one -- the same trade `Hud::load` makes for the layout,
    /// and for the same reason.
    fn load_quest_cache(&mut self, realm: &str) {
        let base = ui::default_path()
            .ok()
            .and_then(|path| path.parent().map(PathBuf::from));
        let Some(base) = base else {
            tracing::info!("no configuration directory -- quests will not be cached to disk");
            // Left as `None`, so this is retried rather than treated as done.
            return;
        };
        let path = ::world::QuestCache::path_for(&base, realm);
        match ::world::QuestCache::load(&path) {
            Ok(cache) => {
                tracing::info!(
                    "{} quest(s) already known for realm {realm:?} ({})",
                    cache.len(),
                    path.display()
                );
                self.quests = cache;
            }
            // Loud: silently starting empty would throw the player's cache
            // away every launch and nobody would ever notice.
            Err(error) => tracing::warn!("could not read {}: {error}", path.display()),
        }
        self.quest_cache_path = Some(path);
    }

    /// Writes the quest cache, if anything was learned.
    ///
    /// Guarded on `is_dirty` so a session that discovered nothing does not
    /// rewrite a file that is already correct.
    fn save_quest_cache(&mut self) {
        if !self.quests.is_dirty() {
            return;
        }
        let Some(path) = self.quest_cache_path.clone() else {
            return;
        };
        match self.quests.save(&path) {
            Ok(()) => tracing::info!("{} quest(s) cached to {}", self.quests.len(), path.display()),
            Err(error) => tracing::warn!("could not write {}: {error}", path.display()),
        }
    }

    fn pump_live_connection(&mut self) {
        // **Loaded here rather than at construction**, because the file is
        // named after the realm and the realm is not known until a connection
        // exists. Guarded by the path being unset rather than by a flag, so
        // there is nothing that could disagree about whether it has happened.
        if self.quest_cache_path.is_none() {
            if let Some(realm) = self.live.as_ref().map(|live| live.realm.clone()) {
                self.load_quest_cache(&realm);
            }
        }
        // **Before the borrow of `live` below**, which is what makes this the
        // top of the function rather than somewhere more obvious: saving takes
        // `&mut self` and the pump holds `live` mutably for its whole length.
        //
        // Long enough that a burst of answers after login produces one write
        // rather than twenty, short enough that a crash loses little. Cheap
        // when nothing was learned -- `save_quest_cache` returns immediately
        // unless the cache is dirty.
        const QUEST_SAVE_INTERVAL: Duration = Duration::from_secs(30);
        if self.quest_saved_at.elapsed() >= QUEST_SAVE_INTERVAL {
            self.quest_saved_at = Instant::now();
            self.save_quest_cache();
        }
        let Some(live) = self.live.as_mut() else {
            return;
        };
        // Senders of chat received this pump who are not in replicated state,
        // so their names can be asked for below.
        let mut unknown_speakers: Vec<u64> = Vec::new();
        // Greetings and volunteered scrolls, collected here and applied below
        // the `live` borrow -- the same shape `unknown_speakers` uses, and for
        // the same reason: filing them touches `self` as a whole.
        let mut greetings: Vec<::world::Gossip> = Vec::new();
        let mut offered: Vec<u32> = Vec::new();
        // Collected during the drain and filed after it, the same shape the
        // gossip messages above use -- the connection is borrowed for the
        // whole loop, so nothing inside it may touch `self` again.
        let mut trainer_lists: Vec<::world::TrainerList> = Vec::new();
        let mut taxi_menus: Vec<::world::TaxiMenu> = Vec::new();
        let mut refusals: Vec<u32> = Vec::new();
        // A spline the server sent for *this* character. See the arm below.
        let mut own_spline: Option<::world::update::MonsterMove> = None;
        // Whether a spell was learned this drain, so the list can be re-asked
        // once the packet loop is done with the connection. A `bool` rather
        // than a count: several successes in one drain still want exactly one
        // re-ask.
        let mut learned_a_spell = false;
        // Same shape again: pushing a line into the scrollback touches
        // `self.chat`, and `live` is borrowed for the whole drain.
        let mut party_results: Vec<String> = Vec::new();
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
                // Fifth category, and the one that punishes being dropped
                // hardest: an unacknowledged teleport makes the server discard
                // every movement packet this client sends, so the character
                // freezes where it stood while the viewer happily walks the
                // camera around. Stored on the state rather than returned
                // precisely so it survives a caller that forgets it -- but a
                // caller still has to answer it, which is the lesson this
                // project keeps re-learning.
                if live::answer_teleport(live) {
                    tracing::info!(
                        "teleported to {:.1}, {:.1}, {:.1}",
                        live.position.x,
                        live.position.y,
                        live.position.z
                    );
                    self.chat.push(Line::Chat(local_notice(
                        "You have been moved.".to_string(),
                    )));
                }
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
                // A cast's own sound, keyed by spell id rather than by
                // caster or weapon the way combat sounds are -- most spells
                // have no confirmed sound at all (see `Sounds::spell_cast`'s
                // doc comment), so this is silent far more often than not,
                // and that silence is correct rather than a gap.
                for start in &report.cast_starts {
                    if let Some(id) = self.sounds.spell_cast(start.spell_id) {
                        self.pending_sounds.push((id, false));
                    }
                }
                for go in &report.cast_landings {
                    if let Some(id) = self.sounds.spell_impact(go.spell_id) {
                        self.pending_sounds.push((id, false));
                    }
                }
                // Same reasoning, for a bag-window drag: `foss-wow#55` left
                // the gesture wired to nothing, so picking an item up and
                // dropping it did nothing and said nothing either -- exactly
                // the silent-failure shape the cast-failure block above was
                // already written to avoid.
                for failure in &report.inventory_failures {
                    let text = format!(
                        "Could not move item: {}",
                        ::world::inventory::describe_inventory_failure(failure.code)
                    );
                    tracing::debug!(
                        "inventory move refused -- {text} (items {:#018x}, {:#018x})",
                        failure.item_a,
                        failure.item_b
                    );
                    self.chat.push(Line::Chat(local_notice(text)));
                }
                // **Lava, slime, drowning and falling.** The same shape again,
                // and the fifth chance to parse a category and drop it -- but
                // this one matters differently: nothing else in the client
                // would say why a character standing in a lava lake is losing
                // health, because the *server* decides that entirely from its
                // own copy of the terrain. The client draws the lava, reports
                // where it is standing, and is told the cost. Inventing that
                // cost locally would be a number nobody can check.
                for hit in &report.environmental_damage {
                    let ours = hit.victim == live.guid;
                    let who = if ours {
                        "You".to_string()
                    } else {
                        // Never observed -- the server does not relay other
                        // people's drowning -- but named from the guid rather
                        // than assumed to be us, since assuming would put our
                        // own name on somebody else's death.
                        live.state
                            .names
                            .player(hit.victim)
                            .flatten()
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("{:#x}", hit.victim))
                    };
                    let text = hit.describe(&who, ours);
                    tracing::debug!("environmental damage -- {text}");
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
                    // The victim's voice, not the attacker's: a sword
                    // hitting a wolf is the wolf's yelp. A miss makes no
                    // sound at all, which is what the original does and what
                    // stops a whiffing fight sounding identical to a landing
                    // one.
                    // What the attacker's blow sounds like. A creature has a
                    // voice; a player has a *weapon*, and the two come from
                    // completely different tables.
                    //
                    // Only our own weapon is known. Another player's equipment
                    // arrives as visible-item fields this client does not read
                    // yet, so their swings fall back to nothing rather than to
                    // a guessed sword.
                    let weapon = (swing.attacker == live.guid)
                        .then(|| {
                            ::world::inventory::equipped(&live.state, live.guid)
                                [MAIN_HAND_SLOT]
                                .and_then(|item| item.entry)
                        })
                        .flatten()
                        .and_then(|entry| {
                            self.sounds.weapon_impact(entry, swing.critical())
                        });
                    match weapon {
                        Some(id) if !swing.missed() => {
                            self.pending_sounds.push((id, true))
                        }
                        _ => {
                            if let Some(id) = live
                                .state
                                .get(swing.attacker)
                                .and_then(|entity| entity.display_id())
                                .and_then(|display| self.sounds.creature(display))
                                .and_then(|voice| voice.get(sound::Voice::Attack))
                            {
                                // A creature's swing is a vocal effort, not an
                                // impact -- it happens as the blow starts.
                                self.pending_sounds.push((id, false));
                            }
                        }
                    }
                    if !swing.missed() {
                        let voice = live
                            .state
                            .get(swing.victim)
                            .and_then(|entity| entity.display_id())
                            .and_then(|display| self.sounds.creature(display));
                        if let Some(voice) = voice {
                            // `overkill` is non-zero only on a killing blow --
                            // the field that identified itself by reading 0
                            // for fourteen swings and 7 for the fifteenth.
                            let which = if swing.overkill > 0 {
                                sound::Voice::Death
                            } else {
                                sound::Voice::Wound
                            };
                            // The victim's cry is a reaction to being hit,
                            // so it belongs at the moment of contact too.
                            if let Some(id) = voice.get(which) {
                                self.pending_sounds.push((id, true));
                            }
 
                        }
                    }
                    self.chat.push(Line::Swing(swing.clone()));
                }
                // Spell damage, through the same two outlets as a swing: a
                // line in the scrollback and a number above whoever was hit.
                // Kept beside the melee loop rather than merged into it --
                // they carry different packets -- but producing the same
                // outputs, because a fight should read as one fight.
                for hit in &report.spell_damage {
                    tracing::debug!(
                        "combat: {}",
                        hud::spell_combat_entry(hit, live.guid, &live.state, Some(&self.spells))
                            .rendered()
                    );
                    if let Some(pos) = live
                        .state
                        .get(hit.target)
                        .and_then(|entity| entity.interpolated_position(Instant::now()))
                    {
                        self.combat_text.push(PendingCombatText {
                            world_pos: glam::Vec3::new(pos.x, pos.y, pos.z),
                            text: hit.damage.to_string(),
                            // No critical flag is read: whatever marks one
                            // lives in the twenty trailing bytes that were all
                            // zero in the only capture, and a number coloured
                            // "critical" on a guess is exactly the kind of
                            // confident wrongness `extra_amount` is kept unnamed
                            // to avoid.
                            kind: ui::CombatTextKind::Damage,
                            spawned: Instant::now(),
                        });
                    }
                    self.chat.push(Line::SpellHit(hit.clone()));
                }
                // **Read straight off the batch rather than through
                // `WorldState`.** A quest description is not replicated state
                // -- it is an answer to a question this client asked, and it
                // arrives once. Folding it into the world would make the cache
                // depend on an object that may not exist (the quest is not an
                // object at all), and `replicate` deliberately dispatches only
                // things that describe the world.
                // What an NPC said when greeted. Read off the batch for the
                // same reason quest descriptions are: this is an answer to a
                // question *this client* asked, not a description of the
                // world, and `replicate` deliberately dispatches only the
                // latter.
                for packet in &packets {
                    match packet.opcode {
                        ::world::opcode::server::GOSSIP_MESSAGE => {
                            match ::world::gossip::parse_gossip_message(&packet.body) {
                                Ok(gossip) => greetings.push(gossip),
                                Err(error) => {
                                    tracing::warn!("a gossip message would not parse: {error}")
                                }
                            }
                        }
                        // The server volunteering one quest's scroll, which is
                        // what a questgiver with exactly one thing to offer
                        // sends instead of a menu.
                        ::world::opcode::server::QUESTGIVER_QUEST_DETAILS => {
                            match ::world::quest::parse_questgiver_details(&packet.body) {
                                Ok(details) => offered.push(details.quest),
                                Err(error) => {
                                    tracing::warn!("quest details would not parse: {error}")
                                }
                            }
                        }
                        // What a trainer teaches. Kept whole rather than
                        // rebuilt per frame: every field in it -- the
                        // availability of each row, and the price after this
                        // character's reputation discount -- was decided by
                        // the server for this character at this moment, and
                        // the client cannot recompute any of it.
                        // **The server moving this client's own character**,
                        // which had never happened before flight paths. A
                        // busy zone carries hundreds of these per drain and
                        // `WorldState::replicate` already parses every one,
                        // so the cheap prefix test comes first and only a
                        // packet that is actually about us is parsed again.
                        ::world::opcode::server::MONSTER_MOVE
                            if ::world::update::monster_move_is_about(&packet.body, live.guid) =>
                        {
                            match ::world::update::parse_monster_move(&packet.body) {
                                // **Two points minimum, and the path rather
                                // than the endpoints.** A flight's route is
                                // its intermediates; a spline with one point
                                // or none is some other kind of server nudge
                                // and must not be flown.
                                Ok(mv) if mv.path.len() >= 2 && mv.duration > 0 => {
                                    tracing::info!(
                                        "the server is moving this character along {} point(s) over {}ms",
                                        mv.path.len(),
                                        mv.duration
                                    );
                                    own_spline = Some(mv);
                                }
                                Ok(mv) => tracing::debug!(
                                    "a move for this character carried {} point(s) over {}ms --                                      not a route, ignored",
                                    mv.path.len(),
                                    mv.duration
                                ),
                                Err(error) => {
                                    tracing::warn!("a move for this character would not parse: {error}")
                                }
                            }
                        }
                        ::world::opcode::server::SHOW_TAXI_NODES => {
                            match ::world::taxi::parse_taxi_menu(&packet.body) {
                                Ok(menu) => {
                                    tracing::debug!(
                                        "flight master {:#018x} at node {}, {} node(s) known",
                                        menu.npc,
                                        menu.current_node,
                                        menu.count()
                                    );
                                    taxi_menus.push(menu);
                                }
                                Err(error) => {
                                    tracing::warn!("a taxi menu would not parse: {error}")
                                }
                            }
                        }
                        ::world::opcode::server::ACTIVATE_TAXI_REPLY => {
                            match ::world::taxi::parse_activate_reply(&packet.body) {
                                // **The refusal is worth surfacing and the
                                // acceptance is not.** A flight that works
                                // announces itself by the character leaving
                                // the ground; one that is declined produces
                                // nothing a player can see, which reads as
                                // the click having missed.
                                Ok(::world::TaxiReply::Ok) => {
                                    tracing::debug!("the flight was accepted")
                                }
                                Ok(::world::TaxiReply::Refused(code)) => {
                                    tracing::info!("the flight was refused, code {code}");
                                    refusals.push(code);
                                }
                                Err(error) => {
                                    tracing::warn!("a taxi reply would not parse: {error}")
                                }
                            }
                        }
                        ::world::opcode::server::TRAINER_LIST => {
                            match ::world::trainer::parse_trainer_list(&packet.body) {
                                Ok(list) => {
                                    tracing::debug!(
                                        "trainer {:#018x} teaches {} spell(s): {:?}",
                                        list.trainer,
                                        list.spells.len(),
                                        list.greeting
                                    );
                                    trainer_lists.push(list);
                                }
                                Err(error) => {
                                    tracing::warn!("a trainer list would not parse: {error}")
                                }
                            }
                        }
                        // One spell learned. **The only reply a purchase
                        // gets**: the server declines every failure in
                        // silence, so this arriving is the success and its
                        // absence is a refusal. The list is re-asked rather
                        // than edited in place, because learning a spell can
                        // change *other* rows -- a rank whose prerequisite
                        // this was becomes available -- and only the server
                        // knows which.
                        ::world::opcode::server::TRAINER_BUY_SUCCEEDED => {
                            tracing::debug!("a spell was learned");
                            learned_a_spell = true;
                        }
                        // The reward screen. Its body is not read: the quest
                        // it is about is the one already on screen, and its
                        // text and rewards are in the cache. What matters is
                        // that it *arrived*, which is the server saying the
                        // hand-in is legal.
                        ::world::opcode::server::QUESTGIVER_OFFER_REWARD => {
                            tracing::debug!("the reward screen is open");
                        }
                        // The opposite answer to the same request: understood,
                        // and the quest is not finished. A statement about the
                        // character rather than about the send.
                        ::world::opcode::server::QUESTGIVER_REQUEST_ITEMS => {
                            tracing::debug!("that quest is not finished yet");
                        }
                        // What mark belongs over one NPC. Nine bytes, and the
                        // guid coming back is what confirms the request went
                        // out as the right opcode.
                        ::world::opcode::server::QUESTGIVER_STATUS => {
                            match ::world::quest::parse_questgiver_status(&packet.body) {
                                Ok(status) => {
                                    self.quest_marks_asked.remove(&status.npc);
                                    if let ::world::quest::QuestgiverMark::Unknown(raw) =
                                        status.mark
                                    {
                                        // Loud, and once per arrival: a value
                                        // this client has never produced
                                        // deliberately is the next thing worth
                                        // measuring, and it draws nothing
                                        // until it has been.
                                        tracing::info!(
                                            "questgiver {:#018x} carries status {raw}, \
                                             which nothing here has named",
                                            status.npc
                                        );
                                    }
                                    self.quest_marks.insert(status.npc, status.mark);
                                }
                                Err(error) => {
                                    tracing::warn!("a questgiver status would not parse: {error}")
                                }
                            }
                        }
                        // Where the log's objectives are. One reply answers
                        // for every id in the request, and an empty marker
                        // list is a real answer -- see `maps::Objectives` for
                        // why it is still not written to disk.
                        ::world::opcode::server::QUEST_POI_QUERY_RESPONSE => {
                            match ::world::quest::parse_quest_poi(&packet.body) {
                                Ok(sets) => {
                                    // **Info rather than debug, deliberately.**
                                    // A map with no pins on it is the symptom
                                    // of a request that never went out *and*
                                    // of a realm with nothing to say, and the
                                    // two want opposite investigations. One
                                    // line per reply, and replies are rare --
                                    // an id is asked about once.
                                    tracing::info!(
                                        "objectives for {} quest(s): {} marker(s), {} point(s)",
                                        sets.len(),
                                        sets.iter().map(|s| s.markers.len()).sum::<usize>(),
                                        sets.iter()
                                            .flat_map(|s| &s.markers)
                                            .map(|m| m.points.len())
                                            .sum::<usize>()
                                    );
                                    self.objectives.insert(&sets);
                                }
                                Err(error) => {
                                    tracing::warn!("a quest POI reply would not parse: {error}")
                                }
                            }
                        }
                        _ => {}
                    }
                }
                for packet in &packets {
                    if packet.opcode != ::world::opcode::server::QUEST_QUERY_RESPONSE {
                        continue;
                    }
                    match self.quests.insert(&packet.body) {
                        Ok(id) => {
                            self.quest_asked_at.remove(&id);
                            tracing::debug!("quest {id} described by the server");
                        }
                        // Loud, and it does not stop the pump. A body this
                        // client cannot read is a parser problem worth seeing;
                        // dropping the whole frame over it would be worse.
                        Err(error) => {
                            tracing::warn!("a quest description would not parse: {error}")
                        }
                    }
                }
                // **The only answer any group request gets**, and it is
                // returned rather than stored, which is exactly the shape
                // three separate callers dropped chat in. Every outgoing
                // party message but the invite is silent, so a result thrown
                // away here puts the interface back into the failure mode the
                // whole `world::group` block exists to escape: a send that
                // fails identically whether the opcode, the body or the
                // permission was wrong.
                for result in &report.party_results {
                    let line = describe_party_result(result);
                    tracing::info!("party: {line}");
                    party_results.push(line);
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

        for line in party_results {
            self.chat.push(Line::Chat(local_notice(line)));
        }

        // **Filed by guid, never accepted blindly.** Two trainers can be
        // within reach at once -- `.npc add` stacks spawns at a foot, and a
        // city's trainers stand in one room -- and a list filed against the
        // wrong session would offer purchases to an NPC that does not teach
        // them, which the server refuses in silence.
        for list in trainer_lists {
            match self
                .trainer
                .as_mut()
                .filter(|session| session.npc == list.trainer)
            {
                Some(session) => session.list = Some(list),
                None => tracing::debug!(
                    "a trainer list for {:#018x} arrived with no window open for it",
                    list.trainer
                ),
            }
        }

        // Filed by guid, like the trainer's: two flight masters can be in
        // reach and a menu filed against the wrong one would send its
        // flights from a node the player is not standing at.
        for menu in taxi_menus {
            match self.taxi.as_mut().filter(|s| s.npc == menu.npc) {
                Some(session) => session.menu = Some(menu),
                None => tracing::debug!(
                    "a taxi menu for {:#018x} arrived with no window open",
                    menu.npc
                ),
            }
        }
        for code in refusals {
            // Raw, never named. See `world::taxi::TaxiReply`.
            self.chat.push(Line::Chat(local_notice(format!(
                "The flight was refused (code {code})."
            ))));
        }

        // **Taking off.** Done here rather than in the packet loop for the
        // usual reason -- the connection is borrowed for the whole of it --
        // and the orientation is captured *before* anything else touches the
        // character, since it cannot be recovered once the flight starts.
        if let Some(mv) = own_spline {
            let points: Vec<(f32, f32, f32)> =
                mv.path.iter().map(|p| (p.x, p.y, p.z)).collect();
            match ::world::Flight::new(&points, mv.duration) {
                Some(route) => {
                    tracing::info!(
                        "taking off: {} points, {:.0} units, {}ms",
                        route.points(),
                        route.length(),
                        route.duration_ms()
                    );
                    self.flight = Some(ActiveFlight {
                        route,
                        started: Instant::now(),
                        orientation_before: live.orientation,
                    });
                    // A flight cancels every local motion state. None of
                    // these survive being put on a gryphon, and a jump arc
                    // still running would fight the spline for the altitude.
                    self.jump = None;
                    self.swimming = None;
                    self.autorun = false;
                }
                // Refused rather than half-flown. `Flight::new` rejects a
                // route with no length or no duration, and pretending to fly
                // one would pin the character at a single point for its
                // duration -- which reads as a freeze, not as a declined
                // packet.
                None => tracing::warn!(
                    "a spline for this character described no flyable route ({} points, {}ms)",
                    points.len(),
                    mv.duration
                ),
            }
        }

        // **The list is re-asked rather than edited in place.** Learning one
        // spell can change *other* rows -- a rank whose prerequisite this was
        // becomes available, and the row just bought turns from green to grey
        // -- and only the server knows which. Editing the row that was
        // clicked would leave a list that is right about one line and stale
        // about the rest, which is worse than one that is briefly empty.
        if learned_a_spell {
            // Through the `live` already borrowed above rather than a fresh
            // `self.live.as_mut()`: that borrow is still alive here, and
            // `self.trainer` is a different field so reading it is fine.
            if let Some(npc) = self.trainer.as_ref().map(|session| session.npc) {
                if let Err(e) = live.connection.trainer_list(npc) {
                    tracing::warn!("re-asking the trainer failed: {e:#}");
                }
            }
        }

        if let Some(questgiver) = self.questgiver.as_mut() {
            for gossip in &greetings {
                questgiver.note_gossip(gossip);
            }
            for quest in offered {
                questgiver.note_quest_offered(quest);
            }
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
        // Item names, on the same budget and for the same reason. `foss-wow#56`:
        // an item's name is server data -- `Item.dbc` has its model and not
        // what it is called -- so every bag square showing `Item 2224` is
        // waiting on this.
        const ITEMS_PER_FRAME: usize = 6;
        // An open loot window's rows are on screen without being owned yet,
        // so they are named here rather than found by walking the bags.
        let looted: Vec<u32> = live
            .state
            .loot
            .as_ref()
            .map(|loot| loot.items.iter().map(|item| item.entry).collect())
            .unwrap_or_default();
        let own_guid = live.guid;
        let asking = hud::items_to_ask(&mut live.state, own_guid, &looted, ITEMS_PER_FRAME);
        for entry in asking {
            // Guid zero: the server keys the answer on the entry, and most of
            // these are things no object in view is holding.
            if let Err(e) = live.connection.ask_item(entry, 0) {
                tracing::warn!("asking about an item failed: {e:#}");
                break;
            }
        }
        // Quests, a few per frame, exactly as names are asked for above and
        // for the same reasons: the cache refuses to ask twice, so calling
        // this every frame is safe, and the cap is only about not firing
        // twenty-five packets in the frame after login. That burst is not
        // hypothetical -- a drain with a packet bound and no clock cost this
        // project thirty-seven seconds before its first frame once already.
        const QUESTS_PER_FRAME: usize = 4;
        // Long enough that a slow realm is not written off, short enough that
        // a genuinely unanswered id stops being drawn as "asking..." forever.
        const QUEST_ANSWER_WINDOW: Duration = Duration::from_secs(10);

        let log: Vec<u32> = live
            .state
            .get(live.guid)
            .map(|player| player.quest_log_ids())
            .unwrap_or_default();
        for quest in self.quests.take_unknown(&log, QUESTS_PER_FRAME) {
            match live.connection.query_quest_info(quest) {
                Ok(()) => {
                    self.quest_asked_at.insert(quest, Instant::now());
                }
                Err(e) => {
                    tracing::warn!("asking about quest {quest} failed: {e:#}");
                    // Put it back, or it stays pending forever having never
                    // actually been sent -- the cache marked it in flight on
                    // the promise that the caller would send it.
                    self.quests.give_up(quest);
                    break;
                }
            }
        }
        // Where those quests' objectives are, on the same budget and for the
        // same reasons. **This one is asked about the log rather than about a
        // quest**: `CMSG_QUEST_POI_QUERY` answers only for quests the player
        // is actually carrying, so a quest handed in has to be forgotten or
        // its markers would outlive it on the map.
        self.objectives.retain_log(&log);
        const OBJECTIVES_PER_REQUEST: usize = 8;
        let ask = self
            .objectives
            .take_unknown(&log, OBJECTIVES_PER_REQUEST, Instant::now());
        if !ask.is_empty() {
            if let Err(e) = live.connection.query_quest_poi(&ask) {
                tracing::warn!("asking where {} quests' objectives are failed: {e:#}", ask.len());
                for quest in ask {
                    self.objectives.give_up(quest);
                }
            }
        }
        // What mark belongs over each nearby NPC's head, a few per frame.
        //
        // **The whole set is thrown away whenever the quest log changes**, and
        // that is not caution: taking a quest turns its giver's exclamation
        // into nothing and its ender's nothing into a question mark, and the
        // server does not volunteer either. A client that asked once would
        // leave an exclamation over an NPC with nothing left to give, which is
        // worse than no mark at all.
        if self.quest_marks_log != log {
            self.quest_marks_log = log.clone();
            self.quest_marks.clear();
            self.quest_marks_asked.clear();
        }
        const MARKS_PER_FRAME: usize = 6;
        // Long enough that a slow realm is not asked twice, short enough that
        // a lost reply costs a pause rather than the session.
        const MARK_RETRY: Duration = Duration::from_secs(15);
        let now = Instant::now();
        let ask: Vec<u64> = live
            .state
            .iter()
            .filter(|entity| entity.guid != live.guid && entity.will_talk())
            .map(|entity| entity.guid)
            .filter(|guid| !self.quest_marks.contains_key(guid))
            .filter(|guid| {
                self.quest_marks_asked
                    .get(guid)
                    .is_none_or(|sent| now.duration_since(*sent) >= MARK_RETRY)
            })
            .take(MARKS_PER_FRAME)
            .collect();
        for guid in ask {
            match live.connection.query_questgiver_status(guid) {
                Ok(()) => {
                    self.quest_marks_asked.insert(guid, now);
                }
                Err(e) => {
                    tracing::warn!("asking what mark {guid:#018x} wears failed: {e:#}");
                    break;
                }
            }
        }

        // **A question with no answer has to stop being a question.** Without
        // this the log would draw "asking the server..." for the rest of the
        // session, which is the state that looks like a hang rather than like
        // a realm that would not say.
        let stale: Vec<u32> = self
            .quest_asked_at
            .iter()
            .filter(|(_, asked)| asked.elapsed() > QUEST_ANSWER_WINDOW)
            .map(|(quest, _)| *quest)
            .collect();
        for quest in stale {
            self.quest_asked_at.remove(&quest);
            self.quests.give_up(quest);
            tracing::info!("quest {quest} was never described by the realm");
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
        //
        // A party member out of visibility range was often never replicated
        // to begin with, which `WorldState::still_targetable` treats as a
        // reason to keep the selection rather than clear it -- see its doc
        // comment for the bug this used to be.
        if self
            .target
            .is_some_and(|guid| !live.state.still_targetable(guid))
        {
            self.target = None;
        }

        // The corpse a released ghost has to run back to. See
        // `own_corpse_query_sent`'s doc comment for why this asks once
        // rather than every frame.
        let is_ghost = live.state.get(live.guid).is_some_and(|e| e.is_ghost());
        if is_ghost && !self.own_corpse_query_sent {
            if let Err(e) = live.connection.query_corpse() {
                tracing::warn!("asking for our corpse failed: {e:#}");
            }
            self.own_corpse_query_sent = true;
        } else if !is_ghost {
            // Cleared on the way back to life too, not only on release, so
            // a second death asks again rather than trusting a stale
            // `corpse_location` left over from the first one.
            self.own_corpse_query_sent = false;
        }
    }

    /// Releases the spirit, in response to a click on the release prompt.
    ///
    /// Nothing acknowledges this directly -- see
    /// `world::client::Connection::release_spirit`'s doc comment. What
    /// confirms it from here is the prompt disappearing on its own the next
    /// frame: it is drawn from `entity.is_ghost()`, not from a local flag
    /// this method could set, so a request the server silently refused would
    /// leave the prompt exactly where it was rather than lying about success.
    fn release_spirit(&mut self) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if let Err(e) = live.connection.release_spirit() {
            tracing::warn!("releasing the spirit failed: {e:#}");
            self.chat
                .push(Line::Chat(local_notice(format!("could not release: {e}"))));
        }
    }

    /// Asks for the body back, in response to a click on the ghost prompt.
    ///
    /// **Refused in silence six different ways**, and the handler makes no
    /// attempt to hide that: alive, in an arena, not yet released, no corpse,
    /// inside the thirty seconds after releasing, or further than
    /// [`CORPSE_RECLAIM_RADIUS`] from the body. The prompt is drawn from
    /// `is_ghost` rather than from anything set here, so a refusal leaves it
    /// exactly where it was -- which is the honest outcome, and the same shape
    /// the release prompt already had.
    ///
    /// The distance check is duplicated at the *prompt*, not here: the point
    /// of it there is to say how far there is left to walk, which is a thing
    /// to draw rather than a thing to enforce. The server decides.
    fn reclaim_corpse(&mut self) {
        let Some((corpse, _)) = self.own_corpse else {
            // Nothing to ask for yet. Silent: the prompt already says the
            // body is still being looked for.
            return;
        };
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if let Err(e) = live.connection.reclaim_corpse(corpse) {
            tracing::warn!("reclaiming the corpse failed: {e:#}");
            self.chat
                .push(Line::Chat(local_notice(format!("could not resurrect: {e}"))));
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
        // Emitters get their own line rather than being folded into the scene
        // summary: "no fires are lit" and "the emitter code is not running"
        // look identical from the window, and only the counts separate them.
        let emitters = r.emitters.describe();
        let summary = r.scene.as_ref().map(|scene| match &self.live {
            Some(live) => format!(
                "{}\n\n{}\n{emitters}",
                describe_live(live),
                describe(scene)
            ),
            None => format!("{}\n{emitters}", describe(scene)),
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
            match live.state.get(guid) {
                Some(entity) => Some(hud::unit_view(entity, hud::unit_name(&live.state, entity))),
                // Out of visibility range is not out of the group: a party
                // member's frame falls back to the party packet, and the
                // target frame agreeing with it is the whole point of
                // targeting one by clicking their row rather than the world.
                None => hud::party_target_view(&live.state, guid),
            }
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
        // The `!` and `?` over questgivers, projected the same way the
        // selection bracket is -- through `marker_rect`, from the very box a
        // click is tested against, so a mark cannot float away from the
        // creature it belongs to.
        let quest_marks: Vec<(egui::Rect, ui::frames::QuestMark)> = match (
            self.live.as_ref(),
            r.scene.as_ref(),
        ) {
            (Some(live), Some(Scene::Streaming(world))) => self
                .quest_marks
                .iter()
                .filter_map(|(guid, mark)| {
                    let mark = match mark {
                        ::world::quest::QuestgiverMark::Available => {
                            ui::frames::QuestMark::Available
                        }
                        ::world::quest::QuestgiverMark::AvailableTrivial => {
                            ui::frames::QuestMark::AvailableTrivial
                        }
                        ::world::quest::QuestgiverMark::Incomplete => {
                            ui::frames::QuestMark::Incomplete
                        }
                        ::world::quest::QuestgiverMark::Complete => {
                            ui::frames::QuestMark::Complete
                        }
                        // Nothing to say, or a value nobody has named. Both
                        // draw nothing; see `world::quest::QuestgiverMark`.
                        _ => return None,
                    };
                    let entity = live.state.get(*guid)?;
                    // A dead questgiver has nothing to offer until it gets
                    // back up, and a mark over a corpse reads as a bug.
                    if entity.is_dead_or_ghost() {
                        return None;
                    }
                    let at = entity.interpolated_position(std::time::Instant::now())?;
                    let display_id = entity.display_id()?;
                    let scale = entity
                        .fields
                        .get_f32(::world::update::fields::OBJECT_SCALE)
                        .filter(|s| *s > 0.0)
                        .unwrap_or(1.0);
                    let rect = hud::marker_rect(
                        &self.camera,
                        viewport,
                        glam::Vec3::new(at.x, at.y, at.z),
                        scale,
                        world.entity_bounds(display_id),
                    )?;
                    Some((rect, mark))
                })
                .collect(),
            _ => Vec::new(),
        };

        // `foss-wow#81`: which corpses sparkle. Unlike the quest marks above
        // this needs no query and no cache -- lootable is a replicated field
        // (`UNIT_DYNAMIC_FLAGS`), current the moment the object update that
        // carries it arrives, so every live entity is simply checked and
        // projected fresh each frame.
        let loot_markers: Vec<egui::Rect> = match (self.live.as_ref(), r.scene.as_ref()) {
            (Some(live), Some(Scene::Streaming(world))) => live
                .state
                .iter()
                .filter(|entity| entity.lootable())
                .filter_map(|entity| {
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
                })
                .collect(),
            _ => Vec::new(),
        };

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

        // Present exactly while the character is dead: first as the release
        // prompt, then as the way back. Absent while alive, the same
        // "existence is the flag" shape the loot window uses. See
        // `App::release_spirit` and `App::reclaim_corpse`.
        //
        // **A ghost had no prompt at all**, which is how a character could die
        // and have nothing to do about it ever again -- release worked, and
        // then the client offered no way back into the body it had just
        // learned the position of.
        let release_prompt = self.live.as_ref().and_then(|live| {
            let entity = live.state.get(live.guid)?;
            if !entity.is_dead_or_ghost() {
                return None;
            }
            if !entity.is_ghost() {
                return Some(ui::frames::ReleasePromptView {
                    text: "You have died.\nClick to release your spirit.".to_string(),
                });
            }
            // A ghost. What it can do depends on how far it has walked back.
            Some(ui::frames::ReleasePromptView {
                text: match self.own_corpse {
                    Some((_, at)) => {
                        let away = at.truncate().distance(live.position.truncate());
                        if away <= CORPSE_RECLAIM_RADIUS {
                            "You are a ghost.\nClick to return to your body.".to_string()
                        } else {
                            // The distance rather than a bare instruction: a
                            // prompt that says "go back" without saying how
                            // far is no better than the marker already on
                            // screen.
                            format!("You are a ghost.\nYour body is {away:.0} yards away.")
                        }
                    }
                    // The query has been sent and not yet answered, or the
                    // body is too far off to be a replicated object. Saying
                    // so beats an instruction that cannot be followed.
                    None => "You are a ghost.\nLooking for your body...".to_string(),
                },
            })
        });

        // The character's own current corpse: which object it is, and where.
        //
        // Resolved once and kept, because two things need it and they must not
        // disagree -- the bracket drawn around the body, and the request that
        // asks for it back. Available from the moment the server answers
        // `MSG_CORPSE_QUERY` -- see `own_corpse_query_sent`. The guid has to
        // come from a replicated object nearest that answer, exactly as
        // `report_reclaim` in `wow-cli` does it: corpse-shaped objects include
        // the bones of bodies already reclaimed, and bones carry the same
        // owner guid as the current body, so owner alone picks a stale one.
        self.own_corpse = self.live.as_ref().and_then(|live| {
            let body_at = live.state.corpse_location?;
            let (guid, position) = live
                .state
                .own_corpses(live.guid)
                .filter_map(|c| c.position.map(|p| (c.guid, p)))
                .min_by(|a, b| {
                    let d = |p: &::world::update::Position| {
                        (p.x - body_at.x).powi(2) + (p.y - body_at.y).powi(2)
                    };
                    d(&a.1).total_cmp(&d(&b.1))
                })?;
            Some((guid, glam::Vec3::new(position.x, position.y, position.z)))
        });
        let corpse_marker = self
            .own_corpse
            .and_then(|(_, at)| hud::corpse_marker_rect(&self.camera, viewport, at));

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
                    Line::SpellHit(hit) => {
                        hud::spell_combat_entry(hit, live.guid, &live.state, Some(&self.spells))
                    }
                })
                .collect(),
            None => Vec::new(),
        };
        // The raw buffer is what `run_command`/`send_chat` read on Enter; what
        // the frame draws is that buffer with the sticky channel's label in
        // front, when it has one -- see `ChatChannel::label`'s doc comment
        // for why a default channel deliberately draws no label at all.
        let composing = self.composing.as_ref().map(|text| match self.chat_channel.label() {
            Some(label) => format!("[{label}] {text}"),
            None => text.clone(),
        });
        let now = std::time::Instant::now();
        // How long the press flash lasts -- see `App::action_flash`'s doc
        // comment for why it exists at all. Short enough to read as "that one
        // registered" rather than as a cooldown of its own.
        const ACTION_FLASH: std::time::Duration = std::time::Duration::from_millis(200);
        if self
            .action_flash
            .is_some_and(|(_, pressed)| now.saturating_duration_since(pressed) >= ACTION_FLASH)
        {
            self.action_flash = None;
        }
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
                    let press_fraction = match self.action_flash {
                        Some(((f_bar, f_slot), pressed)) if (f_bar, f_slot) == (bar, slot) => {
                            let elapsed = now.saturating_duration_since(pressed);
                            1.0 - (elapsed.as_secs_f32() / ACTION_FLASH.as_secs_f32()).min(1.0)
                        }
                        _ => 0.0,
                    };
                    ui::frames::action_bar::SlotSpell {
                        id,
                        name: self.spells.name(id),
                        rank: self.spells.rank(id),
                        description: self.spells.description(id),
                        icon,
                        cooldown_fraction,
                        press_fraction,
                    }
                });
                slots.push(ui::frames::action_bar::SlotView {
                    binding: ui::frames::action_bar::binding_label(bar, slot),
                    spell,
                });
            }
            bars.push(slots);
        }

        // The spellbook, built only while it is open.
        //
        // Rebuilt every frame like the bars, and for the same reason: an icon
        // that failed to load earlier can succeed later, and a spell learned
        // mid-session should appear without a relog. The cost is a filter over
        // a few dozen ids, which is nothing beside the icons it resolves --
        // and those are cached by path inside `Spellbook`.
        // Resolved before the icons, because loading one borrows the renderer
        // and the archive chain while this borrows `self` whole.
        let castable = if self.spellbook_open {
            Self::castable_spells(self.live.as_ref(), &self.spells)
        } else {
            Vec::new()
        };
        let spellbook: Vec<ui::SpellbookEntry> = if self.spellbook_open {
            castable
                .into_iter()
                .map(|id| {
                    let icon =
                        self.spells.icon(&r.gpu, &mut r.egui_renderer, &mut self.chain, id);
                    ui::SpellbookEntry {
                        id,
                        name: self.spells.name(id),
                        rank: self.spells.rank(id),
                        icon,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };

        // The trainer window, built here for the same reason the spellbook is:
        // it resolves icons, and that borrows the renderer and the archive
        // chain, so it has to finish before anything reads `self` whole.
        //
        // **Names and icons come from `Spell.dbc`; everything else comes off
        // the wire.** The split is not a convenience -- a spell's name is a
        // fact about the game and the same for everybody, while its price and
        // its availability were computed by the server for *this* character
        // and cannot be recomputed here at all.
        let trainer: Option<ui::TrainerView> = self.trainer.as_ref().map(|session| {
            let rows = session
                .list
                .as_ref()
                .map(|list| {
                    list.spells
                        .iter()
                        .map(|spell| {
                            let icon = self.spells.icon(
                                &r.gpu,
                                &mut r.egui_renderer,
                                &mut self.chain,
                                spell.spell,
                            );
                            ui::TrainerRow {
                                spell: spell.spell,
                                name: self.spells.name(spell.spell),
                                cost: spell.cost,
                                required_level: spell.required_level,
                                state: match spell.state {
                                    ::world::TrainerSpellState::Available => {
                                        ui::TrainerRowState::Available
                                    }
                                    ::world::TrainerSpellState::Known => {
                                        ui::TrainerRowState::Known
                                    }
                                    // **Everything unrecognised is drawn as
                                    // out of reach, never as available.** The
                                    // two errors are not symmetric: an
                                    // unknown state drawn grey costs the
                                    // player a row they might have been able
                                    // to buy, and drawn green it sends a
                                    // request the server refuses in silence,
                                    // which reads as the client being broken.
                                    _ => ui::TrainerRowState::Unavailable,
                                },
                                icon,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            ui::TrainerView {
                // Between the request and the reply the window is open and
                // empty, and it says which of the two silences that is -- a
                // reply still coming, or a trainer that answered nothing.
                // They are genuinely different: a profession trainer with the
                // trainer bit set answers *nothing at all* to a character who
                // has none of its skill.
                greeting: match session.list.as_ref() {
                    Some(list) if list.is_empty() => {
                        format!("{} has nothing to teach you.", session.name)
                    }
                    Some(list) => list.greeting.clone(),
                    None => format!("Asking {}...", session.name),
                },
                rows,
            }
        });

        // The flight master's list. Needs no renderer -- there are no icons
        // -- but it is built here beside the trainer's so that every window
        // fed from an NPC conversation is assembled in one place.
        //
        // **The names and prices come from the client's tables and the
        // filtering from the server's mask**, and neither could do the
        // other's job: the wire sends no names, and the tables cannot know
        // where this character has been.
        let taxi_view: Option<ui::TaxiView> = self.taxi.as_ref().map(|session| {
            let Some(menu) = session.menu.as_ref() else {
                return ui::TaxiView::default();
            };
            ui::TaxiView {
                here: self
                    .taxi_network
                    .name(menu.current_node)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("Node {}", menu.current_node)),
                rows: self
                    .taxi_network
                    .destinations(menu.current_node, menu)
                    .into_iter()
                    .map(|d| ui::TaxiRow { node: d.node, name: d.name, cost: d.cost })
                    .collect(),
            }
        });

        // Built here rather than beside the other windows below because it
        // needs the renderer to upload its tiles, and the borrow of that has
        // to end before anything reads `self` whole.
        // **The map is built from the walked position, never from replicated
        // state.** The server does not relay our own movement back, so the
        // entity for `live.guid` holds the position the character logged in
        // at, forever. That trap has now caught three separate callers here --
        // the thing that draws the player, the thing that aims at it, and a
        // loot range check that reported fifteen units at a distance of two --
        // and a map is the one window where being wrong about it would look
        // entirely plausible. `live.position` is the walked one.
        let standing = self.live.as_ref().map(|live| maps::Standing {
            map_id: live.map_id,
            x: live.position.x,
            y: live.position.y,
            orientation: live.orientation,
        });
        // **The title comes from the cache or the marker says a number.**
        // A reward already reads `item 2224 x1` on this principle: a made
        // -up name cannot be checked and would be believed, where an id
        // is checkable and visibly unfinished.
        let log: Vec<u32> = self
            .live
            .as_ref()
            .and_then(|live| live.state.get(live.guid))
            .map(|player| player.quest_log_ids())
            .unwrap_or_default();
        let objectives: Vec<maps::Objective<'_>> = log
            .iter()
            .map(|quest| maps::Objective {
                label: match self.quests.answer(*quest) {
                    ::world::Answer::Known(info) => info.title.clone(),
                    // The title is still coming, or never came. Either way
                    // the marker is real and the id is what is honestly
                    // known about it.
                    _ => format!("quest {quest}"),
                },
                markers: self.objectives.markers(*quest),
            })
            .filter(|objective| !objective.markers.is_empty())
            .collect();
        // A member's own zone and position, not the vitals the party
        // frame reads -- `party_member_vitals` prefers a replicated
        // entity's position when there is one, but a page is picked by
        // *zone*, and the entity carries no such field (see
        // `PartyVitals::zone`'s doc comment). `MemberStats::position` and
        // `::zone` are read directly so the two always agree with each
        // other regardless of visibility range.
        let party_pins: Vec<maps::PartyMemberPin> = self
            .live
            .as_ref()
            .and_then(|live| live.state.party.as_ref())
            .map(|party| {
                party
                    .members
                    .iter()
                    .filter_map(|member| {
                        let stats = self.live.as_ref()?.state.party_stats.get(&member.guid)?;
                        let zone = stats.zone?;
                        let (x, y) = stats.position?;
                        Some(maps::PartyMemberPin {
                            name: member.name.clone(),
                            zone: zone as u32,
                            x: x as f32,
                            y: y as f32,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Which page the player is standing on, for the minimap's party dots.
        // **The same zone-equality rule the world map's dots use**, and it has
        // to be: `MemberStats` carries no continent id, so two members a
        // hundred units apart in `(x, y)` can be on different continents
        // entirely, and a minimap two hundred units across would draw one on
        // top of the other. See `maps::PartyMemberPin`.
        let standing_page = standing
            .and_then(|at| self.maps.page_at(at.map_id, at.x, at.y))
            .map(|page| page.id);
        let near_party: Vec<maps::PartyMemberPin> = party_pins
            .iter()
            .filter(|pin| {
                match (standing_page, self.maps.page_for_zone(pin.zone)) {
                    (Some(page), Some(theirs)) => theirs.id == page,
                    // Not "probably here": a member whose zone names no page,
                    // or a player standing where there is none, is a member
                    // this frame cannot honestly place.
                    _ => false,
                }
            })
            .map(|pin| maps::PartyMemberPin {
                name: pin.name.clone(),
                zone: pin.zone,
                x: pin.x,
                y: pin.y,
            })
            .collect();
        // The sub-zone the ground says the character is in -- `Northshire
        // Valley` rather than `Elwynn Forest`, which is what a minimap header
        // has always said. Off the terrain rather than off a replicated field
        // because the terrain is finer: every chunk names its own area, and
        // the server replicates only the zone.
        let area_name = r
            .scene
            .as_ref()
            .and_then(|scene| match scene {
                Scene::Streaming(world) => world.area_at(
                    standing.map(|at| at.x).unwrap_or_default(),
                    standing.map(|at| at.y).unwrap_or_default(),
                ),
                _ => None,
            })
            .and_then(|area| self.maps.area_name(area));

        let map_view = self.map_open.then(|| {
            // **Exploration comes from the player's own replicated field**,
            // not from anywhere this client could decide for itself. A patch
            // of the page is drawn only where `PLAYER_EXPLORED_ZONES` says the
            // character has been, which is what makes the map fill in as it is
            // walked -- and what stops it claiming ground nobody has covered.
            let explored_bits: std::collections::HashSet<u32> = self
                .live
                .as_ref()
                .and_then(|live| live.state.get(live.guid))
                .map(|player| {
                    (0..::world::update::fields::EXPLORED_ZONES_WORDS as u32 * 32)
                        .filter(|bit| player.has_explored(*bit))
                        .collect()
                })
                .unwrap_or_default();
            maps::build_view(
                &mut self.maps,
                &r.gpu,
                &mut r.egui_renderer,
                &mut self.chain,
                standing,
                &objectives,
                &party_pins,
                &|bit| explored_bits.contains(&bit),
            )
        });
        // The minimap, built every frame and unconditionally: it is one of
        // the frames that is simply there, so there is no open flag to test.
        // The tiles are re-placed each frame rather than cached because the
        // viewport slides over ground that does not move -- which is the
        // whole difference between this and a map page.
        let minimap_view = {
            let range = self.minimap_range;
            self.minimap.build_view(
                &r.gpu,
                &mut r.egui_renderer,
                &mut self.chain,
                standing,
                self.live.as_ref().map(|live| live.map_directory.as_str()).unwrap_or_default(),
                area_name.as_deref(),
                range,
                &objectives,
                &near_party,
            )
        };

        // The bag window, built only while it is open, and rebuilt every frame
        // for the same reasons the spellbook is.
        //
        // **The grid is the backpack's sixteen slots followed by each equipped
        // bag's, and every square is filled by its slot index rather than by
        // packing the items in order.** An item in backpack slot 25 is drawn in
        // the third square whether or not 23 and 24 hold anything: a bag that
        // reshuffled itself as things were used would make every position
        // meaningless, and the server's slot numbers are the only stable
        // identity a square has.
        //
        // The bags themselves (worn in slots 19-22) are not drawn as squares.
        // A bag is a container rather than a thing you carry, and drawing it
        // beside its own contents would show the same six items twice.
        // A parallel array to `bags`, naming where each square's item actually
        // lives on the server. `HudResponse::auto_equip` hands back a square
        // index into that same list -- see its doc comment -- and this is what
        // turns the index back into a `Where` the connection can act on.
        let mut bags_where: Vec<Option<::world::inventory::Where>> = Vec::new();
        let bags: Vec<ui::frames::BagSlot> = if self.bags_open {
            use ::world::inventory::{self as inv, Where};

            let mut slots = vec![ui::frames::BagSlot::default(); inv::BACKPACK_COUNT as usize];
            bags_where = vec![None; inv::BACKPACK_COUNT as usize];
            // The backpack's own squares have a fixed address whether or not
            // they hold anything -- unlike the occupied-only fill below, this
            // covers every square so a drag can be **dropped** on an empty
            // one, not just picked up from a full one.
            for i in 0..inv::BACKPACK_COUNT {
                bags_where[i as usize] = Some(Where::Own(
                    inv::InventorySlot::new(inv::BACKPACK_FIRST + i)
                        .expect("backpack slot is always in range"),
                ));
            }
            if let Some(live) = self.live.as_ref() {
                // Where each equipped bag's run of squares begins, in bag-slot
                // order. Built before the fill so a bag's contents can be
                // placed *by index* rather than appended -- an empty slot in
                // the middle of a bag has to stay empty.
                let mut base = std::collections::HashMap::new();
                for bag in inv::bags(&live.state, live.guid).into_iter().flatten() {
                    let Some(capacity) = bag.capacity else { continue };
                    let start = slots.len();
                    base.insert(bag.slot.index(), start);
                    slots.resize(start + capacity as usize, ui::frames::BagSlot::default());
                    bags_where.resize(start + capacity as usize, None);
                    // Same reasoning as the backpack fill above: every square
                    // in a bag's run gets an address immediately, occupied or
                    // not, so dropping on an empty one inside a bag works too.
                    for offset in 0..capacity {
                        bags_where[start + offset as usize] = Some(Where::InBag {
                            bag: bag.slot,
                            slot: offset as u16,
                        });
                    }
                }

                for carried in inv::carried(&live.state, live.guid) {
                    let index = match carried.at {
                        Where::Own(slot) => (slot.index() - inv::BACKPACK_FIRST) as usize,
                        Where::InBag { bag, slot } => match base.get(&bag.index()) {
                            Some(base) => base + slot as usize,
                            None => continue,
                        },
                    };
                    let Some(square) = slots.get_mut(index) else {
                        continue;
                    };
                    // An occupied slot whose item object never arrived still
                    // draws as occupied. `carried` keeps that distinction; a
                    // window that dropped it would show a replication gap as
                    // an empty bag.
                    let entry = carried.item.entry.unwrap_or(0);
                    // Resolved before `icon`, which takes `self.items` and
                    // `self.chain` mutably: the immutable borrow this needs
                    // has to end first.
                    let name = Self::item_name(self.live.as_ref(), &self.items, entry);
                    let icon = (entry != 0).then(|| {
                        self.items
                            .icon(&r.gpu, &mut r.egui_renderer, &mut self.chain, entry)
                    });
                    *square = ui::frames::BagSlot {
                        item: Some(ui::frames::BagItem {
                            entry,
                            name,
                            count: carried.item.count,
                            icon: icon.flatten(),
                        }),
                    };
                    bags_where[index] = Some(carried.at);
                }
            }
            slots
        } else {
            Vec::new()
        };
        let copper = self
            .live
            .as_ref()
            .map(|live| ::world::inventory::coinage(&live.state, live.guid))
            .unwrap_or(0);

        // The character panel: the nineteen worn slots, always all nineteen.
        //
        // Unlike the bag grid this is indexed by *identity* -- slot 7 is the
        // feet whether or not anything is on them -- so an empty slot is still
        // drawn, and its name comes from `InventorySlot::label`, which returns
        // `None` for the one slot this client could not measure. That `None`
        // becomes an empty label and an unnamed square, which is the honest
        // rendering of "we do not know what this is for".
        let character: Vec<ui::frames::EquipSlot> = if self.character_open {
            let worn = self
                .live
                .as_ref()
                .map(|live| ::world::inventory::equipped(&live.state, live.guid))
                .unwrap_or_default();
            (0..::world::inventory::EQUIPPED_COUNT)
                .map(|index| {
                    let slot = ::world::inventory::InventorySlot::new(index);
                    let held = worn[index as usize];
                    let item = held.map(|held| {
                        let entry = held.entry.unwrap_or(0);
                        // Before `icon`, as in the bag squares above.
                        let name =
                            Self::item_name(self.live.as_ref(), &self.items, entry);
                        let icon = (entry != 0).then(|| {
                            self.items
                                .icon(&r.gpu, &mut r.egui_renderer, &mut self.chain, entry)
                        });
                        ui::frames::BagItem {
                            entry,
                            name,
                            count: held.count,
                            icon: icon.flatten(),
                        }
                    });
                    ui::frames::EquipSlot {
                        label: slot
                            .and_then(|slot| slot.label())
                            .unwrap_or_default()
                            .to_string(),
                        item,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };

        // The loot window's rows, straight out of replicated state.
        //
        // **There is no "loot window is open" flag anywhere.** The window
        // exists exactly while the server says a corpse is open, and the
        // server closes an empty one for us -- so a flag here would be a
        // second copy of that decision, and the two would disagree the moment
        // a release arrived that this client did not send.
        //
        // Money is a row like any other, drawn first when there is some,
        // because it is taken by a click the same way an item is. It is the
        // one row whose identity is "money" rather than a slot number.
        let loot: Vec<ui::frames::LootRow> = match self.live.as_ref().and_then(|l| l.state.loot.as_ref())
        {
            Some(loot) => {
                let mut rows = Vec::with_capacity(loot.items.len() + 1);
                if loot.money > 0 {
                    let (g, s, c) = ::world::inventory::purse(loot.money);
                    rows.push(ui::frames::LootRow {
                        take: ui::frames::Take::Money,
                        name: format!("{g}g {s}s {c}c"),
                        count: 1,
                        icon: None,
                    });
                }
                for item in &loot.items {
                    // The icon comes from the item's *entry*, the same route
                    // the bags use, so a corpse and a bag draw the same thing
                    // the same way. The response also carries a display id
                    // directly, which would skip a table lookup -- and would
                    // then be a second path to the same picture that could
                    // drift from the first.
                    let name =
                        Self::item_name(self.live.as_ref(), &self.items, item.entry);
                    let icon = self
                        .items
                        .icon(&r.gpu, &mut r.egui_renderer, &mut self.chain, item.entry);
                    rows.push(ui::frames::LootRow {
                        // The server's slot, carried rather than derived. See
                        // `ui::frames::loot`.
                        take: ui::frames::Take::Item(item.slot),
                        name,
                        count: item.count,
                        icon,
                    });
                }
                rows
            }
            None => Vec::new(),
        };

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

        // **Built from the cache's three-state answer, not from an `Option`.**
        // A quest still being asked about and a quest with no objectives look
        // identical if the two are flattened, and the wrong one of those is
        // silent -- so the distinction is carried all the way from
        // `QuestCache::answer` to the row that gets drawn. See
        // `world::quest_cache`, and `ui::frames::quest_log::QuestDetail`.
        let quest_log: Vec<ui::QuestLogEntry> = if self.quest_log_open {
            self.live
                .as_ref()
                .and_then(|live| live.state.get(live.guid))
                .map(|player| player.quest_log_ids())
                .unwrap_or_default()
                .into_iter()
                .map(|id| ui::QuestLogEntry {
                    id,
                    // Off the log's own state field, not inferred from the
                    // objectives: the client cannot count what a drop is
                    // worth, and the server has already decided.
                    complete: self
                        .live
                        .as_ref()
                        .and_then(|live| live.state.get(live.guid))
                        .and_then(|player| player.quest_is_complete(id))
                        .unwrap_or(false),
                    detail: match self.quests.answer(id) {
                        ::world::Answer::Known(quest) => ui::QuestDetail::Known {
                            title: quest.title.clone(),
                            objective: quest.objectives_text.clone(),
                            level: quest.level,
                            progress: self.quest_progress(quest),
                        },
                        // Never asked and asked-but-waiting are both "the
                        // answer is coming" as far as a player is concerned;
                        // the first becomes the second within a frame or two.
                        ::world::Answer::Unknown | ::world::Answer::Pending => {
                            ui::QuestDetail::Waiting
                        }
                        ::world::Answer::Unanswered => ui::QuestDetail::Unanswered,
                    },
                })
                .collect()
        } else {
            Vec::new()
        };

        let questgiver_view = self.questgiver_view();

        // Both off replicated state, so a party that changed this frame is
        // drawn this frame. The rows are built from the *group list's* names
        // rather than the name cache -- an unreplicated member is not in the
        // cache at all. See `hud::party_view`.
        let party = self
            .live
            .as_ref()
            .map(|live| hud::party_view(&live.state))
            .unwrap_or_default();
        let party_loot = self
            .live
            .as_ref()
            .and_then(|live| hud::party_loot_view(&live.state, live.guid));
        let party_invite = self.live.as_ref().and_then(|live| {
            live.state
                .party_invite
                .as_ref()
                .map(|invite| ui::PartyInviteView {
                    from: invite.from.clone(),
                })
        });

        let mut hud_response = ui::HudResponse::default();
        let spellbook_open = self.spellbook_open;
        let bags_open = self.bags_open;
        let character_open = self.character_open;
        let quest_log_open = self.quest_log_open;
        let selected_quest = self.selected_quest;
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
                    quest_marks: &quest_marks,
                    loot_markers: &loot_markers,
                    loot_sparkle_time: self.started.elapsed().as_secs_f32(),
                    corpse_marker,
                    combat_text: &combat_text,
                    chat: &chat,
                    composing: composing.as_deref(),
                    bars: &bars,
                    cast_bar: cast_bar.as_ref(),
                    // `None` when closed rather than an empty list: an empty
                    // book and a closed one are different things, and the
                    // interface draws the first and hides the second.
                    spellbook: spellbook_open.then_some(spellbook.as_slice()),
                    bags: bags_open.then_some(bags.as_slice()),
                    copper,
                    character: character_open.then_some(character.as_slice()),
                    // `None` when shut rather than an empty list, like the
                    // spellbook: an empty log and a closed one are different
                    // things and the interface draws the first.
                    quest_log: quest_log_open.then_some(quest_log.as_slice()),
                    selected_quest,
                    // Existence is the flag, like the loot window: the window
                    // is on screen exactly while a conversation is open.
                    questgiver: questgiver_view.as_ref(),
                    trainer: trainer.as_ref(),
                    taxi: taxi_view.as_ref(),
                    // `None` when shut, like the spellbook and the log.
                    world_map: map_view.as_ref(),
                    // No flag: a minimap is never opened or shut.
                    minimap: Some(&minimap_view),
                    // No flag: the window exists exactly while the server
                    // says a corpse is open.
                    loot: (!loot.is_empty()).then_some(loot.as_slice()),
                    release_prompt: release_prompt.as_ref(),
                    // Emptiness is the flag, like the loot window: there is
                    // no separate "in a group" boolean that could disagree
                    // with the list.
                    party: &party,
                    party_loot: party_loot.clone(),
                    party_invite: party_invite.as_ref(),
                },
            );

            egui::Window::new("MeoWoW")
                // Over the interface rather than in among it: a window's
                // default order is now where the playing frames live, and a
                // stats readout buried under an action bar is a stats readout
                // nobody can read. Same reasoning as the edit window.
                .order(egui::Order::Foreground)
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
                        "left-click to target, right-click to target and attack, \
                         right-drag to steer, wheel to zoom, Q/E strafe, space \
                         jumps, Num Lock autoruns, Z draws or stows the weapon. \
                         P for the spellbook (click a spell then a slot; \
                         right-click a slot to clear it), B for the bags, \
                         C for the character panel, right-click a body to \
                         loot it, F1 to rearrange the interface"
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
            // Queued like every other effect -- see `pending_sounds`' doc
            // comment -- rather than played here, which has no mixer or
            // archive chain in scope without borrowing more of `self` than
            // this block needs.
            self.pending_sounds.push((sound::INTERFACE_CLICK, false));
        }

        // A clicked loot row takes it. `take` says what to ask for, not where
        // it was on screen -- see `ui::frames::loot`.
        if let Some(take) = hud_response.take_loot {
            self.take_loot(take);
        }

        // A right-clicked bag square auto-equips whatever is there. `index`
        // is a square position, not a slot -- resolved back through
        // `bags_where`, built alongside `bags` a moment ago.
        if let Some(index) = hud_response.activate_item {
            self.activate_item(bags_where.get(index).copied().flatten());
        }

        // A completed bag-window drag: both ends are square positions,
        // resolved through the same `bags_where` list `auto_equip` uses just
        // above. `bags_where` now names every square, occupied or not (see
        // where it is built), so a drop onto an empty square resolves too.
        if let Some((from, to)) = hud_response.move_item {
            self.move_item(
                bags_where.get(from).copied().flatten(),
                bags_where.get(to).copied().flatten(),
            );
        }

        // The same prompt does both halves of dying, and which one depends on
        // the state the prompt was drawn from -- see where it is built. See
        // `App::release_spirit` for why nothing here assumes either worked.
        if hud_response.release_clicked {
            let ghost = self
                .live
                .as_ref()
                .and_then(|live| live.state.get(live.guid))
                .is_some_and(|entity| entity.is_ghost());
            if ghost {
                self.reclaim_corpse();
            } else {
                self.release_spirit();
            }
        }

        // **The corpse has to be released, and only a transition says when.**
        //
        // The server clears its own loot once the body is empty, so the window
        // going away is the signal -- but "no loot yet" and "no loot any more"
        // are the same thing to look at, and the first version of this could
        // not tell them apart. It released a frame after asking, every time,
        // and the window never appeared.
        //
        // So the answer arriving is what promotes `Asked` to `Open`, and only
        // `Open` can be released. A client that never releases leaves the body
        // locked to it for everyone else on the realm, which is why this is
        // not simply left to the server.
        let open = self
            .live
            .as_ref()
            .is_some_and(|live| live.state.loot.is_some());
        match (self.looting, open) {
            (Some(Looting::Asked(guid)), true) => self.looting = Some(Looting::Open(guid)),
            (Some(Looting::Open(_)), false) => self.release_loot(),
            _ => {}
        }

        // A bar that was rearranged is written straight back to `ui.toml`.
        //
        // Saved here rather than left to the edit window's Save button because
        // arranging a bar is not editing the layout -- it happens mid-play,
        // with the edit window closed -- and a spell that has to be dragged on
        // again after every restart is worse than no spellbook at all. The
        // write is atomic (see `Profile::save`), and only happens on the frame
        // an assignment actually landed.
        // A row click just highlights: the log is a list, and picking one is
        // how a later milestone will say which quest the map should pin.
        if let Some(quest) = hud_response.selected_quest {
            // Clicking the highlighted row clears it, so there is a way back
            // to "nothing selected" without closing the window.
            self.selected_quest = (self.selected_quest != Some(quest)).then_some(quest);
        }

        // A destination was chosen. **Both node ids come from the server**:
        // the destination from the row (a `TaxiNodes` id, not a position),
        // and the departure from the menu the server sent. Recomputing the
        // latter from the player's position is the one mistake this request
        // invites, and it is wrong in the field rather than in theory --
        // live, the server named a departure node 573 units away when a
        // nearer one sat 150 units off.
        if let Some(node) = hud_response.fly_to {
            let asked = self
                .taxi
                .as_ref()
                .and_then(|s| s.menu.as_ref().map(|m| (s.npc, m.current_node)));
            match (asked, self.live.as_mut()) {
                (Some((npc, from)), Some(live)) => {
                    tracing::info!("buying a flight from node {from} to node {node}");
                    if let Err(e) = live.connection.activate_taxi(npc, from, node) {
                        tracing::warn!("buying a flight failed: {e:#}");
                    }
                }
                // Logged rather than ignored: a click that reached the frame
                // and produced no send is the shape that reads as a broken
                // window.
                _ => tracing::warn!("a flight was chosen with no menu open"),
            }
        }

        // A trainer row was clicked. The window only reports rows it said were
        // learnable, so nothing here re-checks that -- and nothing here
        // *invents* a spell id either: it is carried from the row, because the
        // server filters the list per character and a row position names a
        // different spell to two people at the same NPC.
        if let Some(spell) = hud_response.learn_spell {
            // The NPC is the one the open window belongs to, not the current
            // target. A player who clicked away between opening the window and
            // clicking a row would otherwise send a purchase to whatever is
            // selected, which the server refuses in silence.
            let npc = self.trainer.as_ref().map(|session| session.npc);
            match (npc, self.live.as_mut()) {
                (Some(npc), Some(live)) => {
                    tracing::debug!("learning spell {spell} from {npc:#018x}");
                    if let Err(e) = live.connection.trainer_buy_spell(npc, spell) {
                        tracing::warn!("learning spell {spell} failed: {e:#}");
                    }
                }
                // Logged rather than ignored: a click that reached the frame
                // and produced no send is exactly the shape that reads as the
                // window being broken.
                _ => tracing::warn!("a trainer row was clicked with no trainer open"),
            }
        }

        // **Logged whenever the window reports anything at all**, so a click
        // that reached the frame and one that never did can be told apart
        // from the log alone. Rare -- a press of a button, not a frame event.
        if hud_response.questgiver.picked.is_some()
            || hud_response.questgiver.acted.is_some()
            || hud_response.questgiver.closed
        {
            tracing::info!(
                "questgiver window: picked {:?}, acted {:?}, closed {}",
                hud_response.questgiver.picked,
                hud_response.questgiver.acted,
                hud_response.questgiver.closed
            );
        }
        if let Some(quest) = hud_response.questgiver.picked {
            if let Some(questgiver) = self.questgiver.as_mut() {
                questgiver.showing = Some(quest);
            }
            // Ask for the scroll as a real client does, in that order: the
            // server checks at each step that this NPC actually offers the
            // quest, and skipping straight to the accept would work or not for
            // reasons nothing here could tell apart.
            self.ask_for_quest_scroll(quest);
        }
        if let Some(quest) = hud_response.questgiver.acted {
            self.act_on_quest(quest);
        }
        if hud_response.questgiver.closed {
            self.questgiver = None;
            // **The trainer window closes with it, because one right-click
            // opened both.** `greet` always opens a conversation and opens a
            // trainer list beside it when the flags say so, so the Close
            // button is the only thing the player pressed and it has to mean
            // "done with this NPC" rather than "done with half of it". A
            // trainer list left behind would sit there offering purchases to
            // an NPC the player has walked away from -- which the server
            // refuses in silence, the one failure this client cannot explain.
            self.trainer = None;
            self.taxi = None;
        }

        // Clicking a party row targets that member. **A guid, not a row** --
        // and it goes through the same `set_target` a click in the world does,
        // so the selection the server is told about cannot disagree with the
        // one the target frame draws.
        //
        // Nothing checks whether the member is in visibility range: a
        // selection is the server's to accept or refuse, and refusing it here
        // would make targeting a party member depend on standing next to
        // them, which is the opposite of what a party frame is for.
        if let Some(guid) = hud_response.party_target {
            self.set_target(Some(guid));
            self.pending_sounds.push((sound::INTERFACE_CLICK, false));
        }

        if let Some(answer) = hud_response.party_invite {
            self.answer_party_invite(answer);
            self.pending_sounds.push((sound::INTERFACE_CLICK, false));
        }

        if hud_response.party_loot_clicked {
            self.cycle_loot_method();
            self.pending_sounds.push((sound::INTERFACE_CLICK, false));
        }

        if hud_response.layout_changed {
            self.hud.save();
        }

        if let Some(r) = self.renderer.as_mut() {
            r.egui_state
                .handle_platform_output(window, output.platform_output.clone());
        }
        output
    }
}

#[cfg(test)]
mod gesture_tests {
    use super::*;

    /// **A pinned pointer ends every drag where it began**, so the click test
    /// cannot be about where the release happened. Both halves are asserted:
    /// a gesture that barely moved is still a click, or the rule would have
    /// turned every click in the game into a look.
    #[test]
    fn travel_tells_a_click_from_a_look() {
        assert!(was_click(0.0), "a press and release that never moved");
        assert!(was_click(CLICK_SLOP), "a hand that shook a little");
        assert!(!was_click(CLICK_SLOP + 0.1));
        // A full turn of the camera: hundreds of pixels of movement, and the
        // pointer back exactly where it started.
        assert!(!was_click(900.0));
    }

    /// **A sidestep travels at the run speed and plays the run**, and the
    /// number saying so is the same number for both -- which is the whole
    /// point, because the one commit where it was two numbers is the commit
    /// where `Q` and `E` moved nobody.
    ///
    /// The cycle itself is settled in `AnimationData` rather than here; see
    /// [`live_pace`] and the test below it.
    #[test]
    fn a_sidestep_travels_at_the_run_speed_like_every_other_direction() {
        use ::world::motion::Motion;

        let sidestep = Motion {
            strafe_left: true,
            ..Default::default()
        };
        let diagonal = Motion {
            forward: true,
            strafe_right: true,
            ..Default::default()
        };
        let run = Motion {
            forward: true,
            ..Default::default()
        };
        let back = Motion {
            backward: true,
            ..Default::default()
        };
        for (name, motion, expected) in [
            ("sidestep", sidestep, LIVE_RUN_SPEED),
            ("run", run, LIVE_RUN_SPEED),
            ("diagonal", diagonal, LIVE_RUN_SPEED),
            ("retreat", back, -LIVE_BACK_SPEED),
            ("still", Motion::default(), 0.0),
        ] {
            assert_eq!(live_pace(motion), expected, "{name}");
        }
        // The other half: a *travelling* character must not also be reported
        // as turning on the spot, or the shuffle would be laid over the run.
        for (name, motion) in [("sidestep", sidestep), ("diagonal", diagonal)] {
            assert_eq!(
                live_turning(KeyState::default(), false, motion),
                0.0,
                "{name} must not report a turn as well"
            );
        }
    }

    /// **A wall stops the eye short; open air leaves it exactly where it
    /// was.** The second half is the one that would ruin the game if it broke:
    /// a camera that pulled in whenever the query was consulted would sit in
    /// the character's head all the time.
    #[test]
    fn a_wall_pulls_the_camera_in_and_open_air_does_not() {
        let focus = glam::Vec3::new(0.0, 0.0, 2.0);
        let eye = glam::Vec3::new(-10.0, 0.0, 2.0);

        assert_eq!(
            pull_camera_in_front_of_walls(focus, eye, |_, _| None),
            eye,
            "nothing in the way must leave the camera alone"
        );

        // Something four units out along a ten-unit ray.
        let pulled = pull_camera_in_front_of_walls(focus, eye, |_, _| Some(0.4));
        let range = (pulled - focus).length();
        assert!(
            (range - (4.0 - CAMERA_WALL_CLEARANCE)).abs() < 1e-3,
            "stopped at {range} rather than just short of the wall"
        );
        // Still on the view ray, or the subject slides off centre -- the bug
        // the ground version was written to avoid.
        assert!(
            (pulled - focus).normalize().dot((eye - focus).normalize()) > 0.9999,
            "the eye came off its own ray"
        );

        // A wall against the character's back must not put the eye inside the
        // head this client draws.
        let against_the_back = pull_camera_in_front_of_walls(focus, eye, |_, _| Some(0.02));
        assert!((against_the_back - focus).length() >= CAMERA_MIN_PULL_IN - 1e-3);
    }

    /// **A sidestep turns the drawn body towards where it is going; a
    /// straight run and a straight retreat turn nothing.**
    ///
    /// This is what the cycle question turned out to be about. There is no
    /// sideways run in the art because the original does not need one -- it
    /// plays the same forward run with the model turned, which is why a
    /// sidestep looked "exactly the same as W" with the body pointing at the
    /// camera.
    ///
    /// Both halves, and the second is the one that matters: leaning is only
    /// correct where there is a lateral component. A retreat that leaned would
    /// undo `Walkbackwards`, whose whole purpose is backing away *facing
    /// forwards*, and a forward run that leaned would be permanently crabbing.
    #[test]
    fn only_a_sidestep_leans_and_it_leans_towards_its_travel() {
        use ::world::motion::Motion;
        use std::f32::consts::FRAC_PI_2;

        let limit = 90f32.to_radians();
        let m = |forward, backward, left, right| Motion {
            forward,
            backward,
            strafe_left: left,
            strafe_right: right,
        };

        // Nothing lateral, nothing to lean into.
        for (name, motion) in [
            ("still", m(false, false, false, false)),
            ("run", m(true, false, false, false)),
            ("retreat", m(false, true, false, false)),
            ("both keys held", m(true, true, false, false)),
            ("both strafes held", m(false, false, true, true)),
        ] {
            assert_eq!(strafe_yaw(motion, limit), 0.0, "{name} must not lean");
        }

        // A pure sidestep travels at a right angle to the facing, and left is
        // the positive side -- the same sign convention as `Motion::direction`
        // and `Side`.
        assert!((strafe_yaw(m(false, false, true, false), limit) - FRAC_PI_2).abs() < 1e-5);
        assert!((strafe_yaw(m(false, false, false, true), limit) + FRAC_PI_2).abs() < 1e-5);

        // A diagonal travels at 45 degrees and leans exactly that far, which
        // is why 45 is the default limit: a sidestep and a forward-sidestep
        // then agree rather than snapping between two angles.
        let diagonal = strafe_yaw(m(true, false, true, false), limit);
        assert!((diagonal - FRAC_PI_2 / 2.0).abs() < 1e-5, "{diagonal}");

        // And the limit is a limit: at 30 degrees a sidestep leans 30, not 90.
        let tight = 30f32.to_radians();
        assert!((strafe_yaw(m(false, false, true, false), tight) - tight).abs() < 1e-5);
        assert_eq!(
            strafe_yaw(m(false, false, true, false), 0.0),
            0.0,
            "the zero choice must draw the body exactly as it did before"
        );
    }

    /// **Which cycle a sidestep plays was decided by a column, after three
    /// renders decided it wrongly.**
    ///
    /// Given the `Shuffle` cycles it was reported as shimmying; given the run,
    /// as "running sideways plays the forward animation"; given `Shuffle`
    /// again, as "he stands perfectly still and his feet shuffle". A render
    /// could not settle it because both readings draw a plausible picture --
    /// the same shape as the placement rotation that took four attempts.
    ///
    /// `AnimationData.dbc`'s `body_flags` settles it: bit 64 is set on exactly
    /// the animations that carry the character somewhere, and on nothing else
    /// in the table. This asserts both directions of that, because "the
    /// travelling ones have it" is worth nothing without "the shuffles do
    /// not" -- and the shuffles sharing `Stand`'s exact value is the finding.
    #[test]
    fn the_shuffles_are_not_travelling_cycles_and_walk_run_and_backwards_are() {
        const TRAVELS: u32 = 64;

        let Some(data) = std::env::var_os("WOW_DATA") else {
            eprintln!("skipping: WOW_DATA not set");
            return;
        };
        let mut chain = Chain::open_wow_data(data, "enUS").expect("opening archives");
        let table = dbc::schema::AnimationData::parse(
            &chain
                .read(dbc::schema::AnimationData::PATH)
                .expect("AnimationData"),
        )
        .expect("parsing AnimationData");

        let by_name = |wanted: &str| {
            table
                .iter()
                .find(|row| row.name() == wanted)
                .unwrap_or_else(|| panic!("no {wanted} row"))
                .body_flags()
        };

        for name in ["Walk", "Run", "Walkbackwards", "Sprint", "SwimLeft"] {
            assert!(
                by_name(name) & TRAVELS != 0,
                "{name} carries the character and must have the travel bit"
            );
        }
        for name in ["ShuffleLeft", "ShuffleRight"] {
            assert!(
                by_name(name) & TRAVELS == 0,
                "{name} would be a travelling cycle, which is what a sidestep wanted"
            );
            assert_eq!(
                by_name(name),
                by_name("Stand"),
                "{name} is classed with standing still, exactly as it looks on screen"
            );
        }

        // And the population, so the bit is a *measurement* rather than five
        // rows that happened to agree: every animation carrying it is a
        // locomotion cycle, in a table of five hundred.
        let travelling: Vec<String> = table
            .iter()
            .filter(|row| row.body_flags() & TRAVELS != 0)
            .map(|row| row.name().to_string())
            .collect();
        assert_eq!(travelling.len(), 28, "{travelling:?}");
        assert!(
            travelling.iter().all(|name| {
                ["Walk", "Run", "Swim", "Sprint", "Stealth", "Fly", "ToFly", "ToHover",
                 "ToGround", "Settle"]
                    .iter()
                    .any(|stem| name.contains(stem))
            }),
            "something that is not a locomotion cycle carries the travel bit: {travelling:?}"
        );
    }
}

#[cfg(test)]
mod chat_channel_tests {
    use super::*;

    /// Every channel but whisper ignores the target string -- `message_chat`
    /// only reads it for `Whisper`/`Channel` -- and whisper has to carry the
    /// recipient through, since that name is the whole reason the sticky
    /// channel exists.
    #[test]
    fn wire_carries_the_whisper_target_and_nothing_else_does() {
        assert_eq!(ChatChannel::Say.wire(), (::world::ChatType::Say, ""));
        assert_eq!(ChatChannel::Party.wire(), (::world::ChatType::Party, ""));
        assert_eq!(ChatChannel::Yell.wire(), (::world::ChatType::Yell, ""));
        assert_eq!(
            ChatChannel::Whisper("Watcher".into()).wire(),
            (::world::ChatType::Whisper, "Watcher")
        );
    }

    /// **The default channel draws no label, and every other one does.**
    /// `App`'s composing line only prefixes the buffer when this returns
    /// `Some` -- see the call site's doc comment -- so a `None` here for Say
    /// is what keeps ordinary typing looking exactly as it always did, and a
    /// label appearing at all is what tells the player the channel changed.
    #[test]
    fn only_the_default_channel_has_no_label() {
        assert_eq!(ChatChannel::Say.label(), None);
        assert_eq!(ChatChannel::Party.label(), Some("party".into()));
        assert_eq!(ChatChannel::Yell.label(), Some("yell".into()));
        assert_eq!(
            ChatChannel::Whisper("Watcher".into()).label(),
            Some("to Watcher".into())
        );
    }
}

#[cfg(test)]
mod camera_tests {
    use super::*;

    /// The character stays in the middle of the frame at every pitch.
    ///
    /// This is the whole of what a third-person camera has to do, and it is
    /// what broke: the eye used to sit at a fixed height behind the character
    /// while the mouse only re-aimed it, so swinging left and right kept them
    /// centred -- the eye really did travel around them -- and dragging up and
    /// down slid them off the top or bottom of the screen. Reported as the
    /// camera being correct one way and "they go everywhere" the other.
    ///
    /// Checked by projecting the focus point through the very matrix the scene
    /// is drawn with, rather than by re-deriving where the camera is pointing.
    #[test]
    fn the_subject_stays_centred_at_every_pitch() {
        let feet = glam::Vec3::new(-8975.0, -227.0, 74.0);
        let focus = feet + glam::Vec3::Z * FOLLOW_HEIGHT;

        for &pitch in &[-FOLLOW_PITCH_LIMIT, -0.6, -0.2, 0.0, 0.4, FOLLOW_PITCH_LIMIT] {
            for &yaw in &[0.0, 1.1, 3.0, 5.5] {
                for &distance in &[FOLLOW_NEAR, 9.0, FOLLOW_FAR] {
                    let fly = orbit_around(feet, yaw, pitch, distance);
                    let clip = fly.view_proj(16.0 / 9.0) * focus.extend(1.0);
                    assert!(
                        clip.w > 0.0,
                        "the subject is behind the camera at pitch {pitch}, yaw {yaw}"
                    );
                    // Normalised device coordinates: the centre of the screen
                    // is the origin, and the whole point is that the subject
                    // sits there whatever the angle.
                    let (nx, ny) = (clip.x / clip.w, clip.y / clip.w);
                    assert!(
                        nx.abs() < 1e-3 && ny.abs() < 1e-3,
                        "at pitch {pitch}, yaw {yaw}, distance {distance} the subject \
                         is at {nx:.3}, {ny:.3} rather than the centre"
                    );
                }
            }
        }
    }

    /// And the eye really is `distance` away, on the far side from where it
    /// looks -- an orbit, not a camera that stays put and turns.
    ///
    /// The half that "stays centred" alone would not catch: a camera welded to
    /// the subject's own position also keeps them dead centre, and shows the
    /// inside of their head.
    #[test]
    fn the_eye_orbits_rather_than_pivoting_in_place() {
        let feet = glam::Vec3::new(10.0, -20.0, 5.0);
        let focus = feet + glam::Vec3::Z * FOLLOW_HEIGHT;
        let mut heights = Vec::new();
        for &pitch in &[-0.8, -0.2, 0.0, 0.5] {
            let fly = orbit_around(feet, 0.7, pitch, 9.0);
            assert!(
                (fly.position.distance(focus) - 9.0).abs() < 1e-3,
                "the eye is {} from its subject, not 9",
                fly.position.distance(focus)
            );
            heights.push(fly.position.z);
        }
        // Tilting down puts the eye *above* the subject, so the heights must
        // fall as the pitch rises. A camera that only re-aimed would leave
        // every one of them identical.
        for pair in heights.windows(2) {
            assert!(
                pair[1] < pair[0],
                "the eye did not move as the pitch changed: {heights:?}"
            );
        }
    }

    /// The camera stops at the ground instead of going through it, and it does
    /// so by coming *in* rather than by rising.
    ///
    /// Rising would keep the distance and break the framing -- the subject
    /// would slide off centre, which is the bug this camera was just fixed for.
    /// So the test asserts the eye stays on the original ray, not merely that
    /// it ended up above ground.
    #[test]
    fn the_camera_comes_in_rather_than_sinking_into_a_hill() {
        let focus = glam::Vec3::new(0.0, 0.0, 10.0);
        // Flat ground at z = 8, with the camera aimed down into it.
        let ground = |_x: f32, _y: f32| Some(8.0f32);
        let wanted = focus + glam::Vec3::new(0.0, 0.0, -1.0) * 9.0;
        assert!(wanted.z < 8.0, "the test is not aiming underground");

        let eye = pull_camera_out_of_the_ground(focus, wanted, ground);
        assert!(
            eye.z >= 8.0,
            "the camera is still underground at {:.2}",
            eye.z
        );
        // Still on the ray: the direction from the subject is unchanged, so
        // the subject is still dead centre.
        let before = (wanted - focus).normalize();
        let after = (eye - focus).normalize();
        assert!(
            before.dot(after) > 0.999,
            "the camera left its own ray: {before:?} became {after:?}"
        );
        assert!(
            eye.distance(focus) < wanted.distance(focus),
            "the camera did not come in at all"
        );
    }

    /// Clear ground leaves the camera exactly where it asked to be.
    ///
    /// The half that stops a collision test from passing for a camera that
    /// simply always sits close to its subject.
    #[test]
    fn open_ground_does_not_move_the_camera() {
        let focus = glam::Vec3::new(0.0, 0.0, 100.0);
        let wanted = focus + glam::Vec3::new(1.0, 0.0, 0.2).normalize() * 9.0;
        // Ground far below, and ground that is not loaded at all.
        for ground in [
            (|_x: f32, _y: f32| Some(0.0f32)) as fn(f32, f32) -> Option<f32>,
            (|_x: f32, _y: f32| None) as fn(f32, f32) -> Option<f32>,
        ] {
            let eye = pull_camera_out_of_the_ground(focus, wanted, ground);
            assert!(
                eye.distance(wanted) < 1e-4,
                "the camera was pulled in over open ground: {eye:?}"
            );
        }
    }

    /// A ridge *between* the subject and the camera stops it, not just the
    /// ground beneath where it wanted to end up.
    ///
    /// A check that only tested the destination would tunnel straight through
    /// the hill and come out the far side looking back through it.
    #[test]
    fn a_ridge_in_the_way_stops_the_camera() {
        let focus = glam::Vec3::new(0.0, 0.0, 10.0);
        // A wall halfway out: high ground between 4 and 6 units along +X.
        let ground = |x: f32, _y: f32| Some(if (4.0..6.0).contains(&x) { 20.0 } else { 0.0 });
        let wanted = focus + glam::Vec3::X * 10.0;

        let eye = pull_camera_out_of_the_ground(focus, wanted, ground);
        assert!(
            eye.x < 4.0,
            "the camera tunnelled through the ridge to {:.2}",
            eye.x
        );
    }

    /// The default turn rate is the modest half-turn the old comment claimed,
    /// not the two and a half full turns the old fixed rate actually gave.
    ///
    /// The rate itself is the ui crate's now, and tested there; this pins the
    /// number a new player gets before touching anything.
    #[test]
    fn the_default_camera_is_not_the_old_accidental_rate() {
        let camera = ui::Camera::default();
        let default_rate = camera.radians_per_pixel(1920.0);
        assert!(
            (default_rate * 1920.0 - std::f32::consts::PI).abs() < 1e-4,
            "a full-width drag is no longer half a turn"
        );
        // The rate this replaced, which felt like a camera that would not sit
        // still.
        assert!(0.008 > default_rate * 4.0);
    }

    /// The pitch limit stops short of straight up and straight down, where an
    /// orbit degenerates and the horizon spins.
    #[test]
    fn the_pitch_limit_avoids_the_poles() {
        assert!(FOLLOW_PITCH_LIMIT < std::f32::consts::FRAC_PI_2);
        assert!(FOLLOW_PITCH_LIMIT > 1.0, "the camera can barely tilt");
    }
}

/// Buoyancy: where a swimmer's body settles, and how fast.
///
/// The integration in `drive_live_movement` is a few lines inside a function
/// that needs a GPU, a realm and a resident tile, so the arithmetic is mirrored
/// here rather than driven through it. What is being pinned is not the code but
/// the *ordering property* it depends on: a swimmer's altitude has to carry
/// from one frame to the next.
#[cfg(test)]
mod swim_tests {
    use super::*;

    /// Closing a fraction of the remaining gap, exactly as the movement code
    /// does, starting from `z` and running for `frames` at 60fps.
    fn float_towards(rest: f32, mut z: f32, frames: usize, reset_each_frame: Option<f32>) -> f32 {
        let dt = 1.0 / 60.0;
        for _ in 0..frames {
            // The bug: the ground assignment ran every frame, before this.
            if let Some(bed) = reset_each_frame {
                z = bed;
            }
            z += (rest - z) * (1.0 - (-dt / BUOYANCY_TAU).exp());
        }
        z
    }

    /// A swimmer rises to the surface within about a second.
    ///
    /// **And the same integration pinned to the riverbed goes nowhere**, which
    /// is the half that matters: the first version of this code planted the
    /// character on the ground every frame before the buoyancy read its own
    /// altitude, so it rose three per cent of the way and was put back. That
    /// draws as a character walking along the bottom with the swim cycle
    /// playing -- the feature apparently half-working, rather than an ordering
    /// mistake. Asserting only that a free body rises would pass under both.
    #[test]
    fn a_swimmer_rises_only_if_its_altitude_survives_the_frame() {
        // The river measured at world -10081, 340: bed at 8.1, surface 21.23.
        let (bed, surface) = (8.1f32, 21.23f32);
        let rest = surface - SWIM_FLOAT;

        // Exponential, so it approaches rather than arrives: one time
        // constant is 63% of the way and a second of it is 89%. The numbers
        // here are what `BUOYANCY_TAU` actually produces, not a round figure
        // -- a tolerance picked by eye would either pass a broken curve or
        // fail this correct one, which it did on the first attempt.
        let one_second = float_towards(rest, bed, 60, None);
        let travelled = (one_second - bed) / (rest - bed);
        assert!(
            travelled > 0.85,
            "a second of buoyancy covered only {:.0}% of the way up",
            travelled * 100.0
        );
        let free = float_towards(rest, bed, 120, None);
        assert!(
            (free - rest).abs() < 0.5,
            "two seconds should settle at the surface, got {free:.2} of {rest:.2}"
        );

        let pinned = float_towards(rest, bed, 120, Some(bed));
        assert!(
            pinned < bed + 1.0,
            "pinned to the bed, a swimmer must not appear to rise: got {pinned:.2}"
        );
        assert!(
            free - pinned > 8.0,
            "the two orderings have to differ by most of the river's depth, \
             or this test cannot tell them apart"
        );
    }

    /// The swim test is about depth over the bed, not about altitude.
    ///
    /// A puddle on a mountain top and a lake at sea level are the same
    /// question, and asking about the surface height alone answers it wrongly
    /// for one of them.
    #[test]
    fn swimming_starts_at_a_depth_not_at_a_height() {
        let deep_enough = |surface: f32, ground: f32| surface - ground >= SWIM_DEPTH;
        // The measured river: thirteen units deep.
        assert!(deep_enough(21.23, 8.1));
        // A ford at the same altitude is waded, not swum.
        assert!(!deep_enough(21.23, 20.5));
        // And a mountain tarn is swum despite being a thousand units up.
        assert!(deep_enough(1021.23, 1008.1));
        // The threshold is chest deep on a two-unit body, not ankle deep.
        assert!(SWIM_DEPTH > 1.0 && SWIM_DEPTH < BODY_HEIGHT);
    }

    /// A swimmer floats with their head clear of the surface.
    #[test]
    fn the_resting_body_keeps_its_head_out() {
        let surface = 21.23f32;
        let feet = surface - SWIM_FLOAT;
        let head = feet + BODY_HEIGHT;
        assert!(
            head > surface,
            "head at {head:.2} is under a surface at {surface:.2}"
        );
        // But not so high the body rides on top of the water like a boat.
        assert!(head - surface < 0.5, "the character is floating too high");
    }
}
