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
mod icon;
mod icon_art;
mod items;
mod liquid;
mod live;
mod maps;
mod minimap;
mod model;
mod scene;
mod sky;
mod signin;
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
    ///
    /// **Optional since the sign-in screen exists.** Without it the viewer
    /// opens the screen, which remembers the directory in
    /// `%APPDATA%\open-wow\login.toml` and can be pointed at one with a folder
    /// picker -- so the ordinary way to run this client is now to double-click
    /// it. Given here it wins, because a flag typed on purpose outranks a
    /// setting typed once: that is what makes a probe reproducible.
    #[arg(long, short, env = "WOW_DATA")]
    data: Option<PathBuf>,

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

    /// Radius in tiles around the chosen one: 1 is a 3x3, 2 a 5x5.
    ///
    /// **A streaming world raises this to at least 1 whatever is asked**, so
    /// the tile a character is walking towards has already arrived -- see
    /// `world::MIN_STREAM_RADIUS`. The old default of 0 meant only the tile
    /// under the camera was ever loaded, and crossing a boundary put the
    /// character in the void until the next one caught up.
    #[arg(long, default_value_t = 1)]
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

    /// Draw no shadows at all.
    ///
    /// **The A/B is the point.** A shadow is the one thing in this renderer
    /// whose absence and whose failure look the same from a distance, and the
    /// cheapest way to tell "the map is empty" from "the map is fine and the
    /// scene is dark" is to turn it off and look again.
    #[arg(long)]
    no_shadows: bool,

    /// Duplicate every replicated creature this many times, spread around
    /// where it stands.
    ///
    /// **Because the question has moved from "is it fast" to "what happens
    /// when it is busy".** A hundred frames a second in an empty abbey says
    /// nothing about a city with forty people throwing spells, and waiting to
    /// find out is not a measurement -- it is a report. This makes the
    /// crowded case reproducible on a realm with four characters on it, at a
    /// number somebody chose.
    ///
    /// Duplicates rather than invented creatures: every copy goes through the
    /// same look resolution, the same bucketing, the same instance buffer and
    /// the same pose as the thing it was copied from, so what it measures is
    /// the real path rather than a model of it. `1` is off.
    ///
    /// **Not a benchmark of the server.** Nothing here is sent anywhere; the
    /// copies exist for exactly as long as one frame's drawn list.
    #[arg(long, default_value_t = 1)]
    stress: u32,

    /// Draw the headless frame this many times and report what it costs.
    ///
    /// **A steady-state number, which one frame cannot give.** The first
    /// frame of a session pays for pipeline compilation, texture residency
    /// and a cold driver -- measured at 48 ms for a frame whose repeat cost
    /// is a fraction of that -- so a single `--screenshot` timing is a
    /// measurement of warm-up. Repeating the same frame and taking the
    /// *minimum* is the closest thing to what the window sees, and it is a
    /// number this client can produce for itself rather than asking somebody
    /// to go and play.
    ///
    /// Reports CPU encode time and GPU completion separately, and honours
    /// `--width`/`--height`, so fill rate and geometry can be told apart:
    /// a cost that scales with the pixel count is shading; one that does not
    /// is vertices and draw calls.
    #[arg(long)]
    bench: Option<u32>,

    /// How frames are handed to the display: `fifo` waits for the monitor,
    /// `immediate` does not, `mailbox` replaces an undisplayed frame.
    ///
    /// **Exists to identify the gap between frames.** Under `fifo` a client
    /// that misses the refresh window waits for the next one, so a frame
    /// taking a little over budget costs a whole extra interval -- which from
    /// inside the process is indistinguishable from the scheduler not handing
    /// it back, and was measured at 6.4 ms on average and 38 ms at worst.
    /// Running the same session with `immediate` answers it: if the gap
    /// collapses, the client is being *paced* rather than starved, and the
    /// fix is to fit inside the window rather than to hunt for a stall.
    ///
    /// Defaults to whatever the surface lists first, which is what it has
    /// always done -- named here rather than changed, because the default is
    /// not the bug and quietly picking another would hide the measurement.
    #[arg(long)]
    present_mode: Option<String>,

    /// Submit every draw, culling nothing against the frustum.
    ///
    /// **The instrument the culling is checked with, and the reason it can be
    /// checked at all.** Culling is only correct if it changes the frame time
    /// and nothing else, and "nothing else" is not something a person can see
    /// by looking at one render: a missing room, a missing hillside or a
    /// missing doodad in a city of three thousand looks exactly like a city.
    /// Two `--screenshot` runs differing by this flag alone must produce the
    /// same pixels, which is an assertion a machine can make and an eye
    /// cannot. `render::cull` has the unit tests; this has the picture.
    #[arg(long)]
    no_cull: bool,

    /// How wide the shadowed region around the camera is, in world units.
    ///
    /// Exposed because it is the only number here whose right value is a
    /// judgement: small is sharp and small, wide is soft and covers the hill
    /// behind you. One cascade, so it is one or the other.
    #[arg(long, default_value_t = 110.0)]
    shadow_radius: f32,

    /// How many texels across the shadow map is.
    #[arg(long, default_value_t = 2048)]
    shadow_size: u32,

    /// Also write every log line, and any panic, to this file.
    ///
    /// **A crash is the one failure that destroys its own evidence.** The
    /// window goes, and with it the console scrollback that held the last
    /// thing the client said -- so every report of one so far has arrived with
    /// logs that stop well short of the moment of death. Shell redirection
    /// would do the same job, except that it is remembered on the runs that do
    /// not crash and forgotten on the one that does.
    ///
    /// Opened for append and *checked* at startup rather than on first write:
    /// a log file that silently went nowhere is worse than none, because it is
    /// believed. See the panic hook in `main`.
    #[arg(long)]
    log_file: Option<PathBuf>,

    /// Write the sun's depth map to this PNG, after rendering.
    ///
    /// `--screenshot` only, and the reason it exists is the reason every
    /// format here has a dump command: a shadow map is the one buffer nothing
    /// displays, and an empty one, a map of the sky and a map with its depth
    /// axis reversed all come out as a world that is uniformly lit or
    /// uniformly dark. Near the sun is dark, the far plane is white, and
    /// anything the pass never drew is pure white.
    #[arg(long)]
    shadow_dump: Option<PathBuf>,

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

impl Args {
    /// Whether the command line already answered everything the sign-in
    /// screen would ask.
    ///
    /// **Three separate answers, not one flag.** `--texture`, `--model`,
    /// `--creature`, `--wmo` and `--map` each name something to draw off
    /// disk; `--realm-host` with a user and a character names a session to
    /// open. Either is a complete instruction, and a client that stopped to
    /// ask again would break every probe in `docs/ROADMAP.md`. Anything less
    /// -- including the bare double-click this whole screen exists for --
    /// falls through to asking.
    fn is_self_contained(&self) -> bool {
        let offline = self.texture.is_some()
            || self.model.is_some()
            || self.creature.is_some()
            || self.wmo.is_some()
            || self.map.is_some();
        let live = self.realm_host.is_some() && self.user.is_some() && self.character.is_some();
        offline || live
    }
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

/// Seconds since the Unix epoch, or `0` if the clock is before it.
///
/// **The only wall clock in this file**, and it is used for exactly one thing:
/// stamping a remembered questgiver so the interface can say *when* it was
/// seen. Everything else here is timed with `Instant`, which is monotonic and
/// cannot go backwards -- but an `Instant` is meaningless across a restart,
/// and "seen three hours ago" has to survive one. Nothing branches on this
/// value, so a clock that jumped costs a misleading sentence rather than a
/// wrong pin.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// Sends every panic to the log before the process goes, backtrace included.
///
/// **`RUST_BACKTRACE` is forced rather than consulted.** A crash that is only
/// diagnosable when somebody remembered to set an environment variable is
/// diagnosable on the runs that did not crash; `force_capture` costs nothing
/// on a run that never panics, because it only runs when one does.
///
/// The default hook is kept and called afterwards, so stderr still says what
/// it always said. This one exists to put the same words *in the log stream*,
/// which is the half that survives the window closing.
///
/// It cannot catch everything, and saying so is the point: a driver fault, a
/// wgpu device loss that aborts, or an out-of-memory kill are not panics and
/// leave nothing here. A log that ends with a panic and one that ends
/// mid-frame are therefore different findings -- the second says to look
/// outside Rust.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(
            "panic at {}: {}\n{}",
            info.location().map(|l| l.to_string()).unwrap_or_else(|| "?".into()),
            info.payload()
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string payload>".into()),
            std::backtrace::Backtrace::force_capture(),
        );
        default(info);
    }));
}

fn main() -> Result<()> {
    let args = Args::parse();

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".into());
    match &args.log_file {
        // Opened here, so a path that cannot be written fails the run instead
        // of producing a client that looks instrumented and is not.
        Some(path) => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("opening log file {}", path.display()))?;
            use tracing_subscriber::fmt::writer::MakeWriterExt;
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_ansi(false)
                .with_writer(std::io::stdout.and(std::sync::Arc::new(file)))
                .init();
            tracing::info!("logging to {}", path.display());
        }
        None => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .init();
        }
    }
    install_panic_hook();

    // The sign-in screen supplies this when the command line does not, so the
    // archives are opened later and possibly more than once. Everything that
    // needs them asks `App::chain`, which is empty until one is.
    let mut chain = match &args.data {
        Some(data) => match open_data(data, &args.locale) {
            Ok(chain) => chain,
            // **A bad `--data` is fatal only when nothing can ask about it.**
            // With a sign-in screen coming there is somewhere to put the
            // complaint and a folder picker to fix it with, and exiting
            // instead would mean the one path a person double-clicks dies at a
            // console they never see.
            Err(e) if !args.is_self_contained() => {
                tracing::warn!("{e:#}");
                Chain::new()
            }
            Err(e) => return Err(e),
        },
        None => Chain::new(),
    };

    if let Some(path) = args.screenshot.clone() {
        // **A screenshot with no archives is refused rather than rendered.**
        // It would produce a perfectly plausible empty picture -- the one
        // failure mode this project has paid for repeatedly -- and there is no
        // sign-in screen on this path to ask.
        if args.data.is_none() {
            anyhow::bail!("--screenshot needs --data (or WOW_DATA): there is no window to ask in");
        }
        return screenshot(&args, &mut chain, &path);
    }

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(args, chain);
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// Opens the archive set, and checks that it *is* one.
///
/// **`Chain::open_wow_data` skips members it cannot find**, correctly -- no
/// two installs carry the same set of optional patches -- so it answers `Ok`
/// with nothing in it for a directory that holds no archives at all. That is
/// exactly the mistake a folder picker makes easy: choosing the install's root
/// rather than its `Data` folder. Reading a file that must be there separates
/// the two, and *reading* is the right test rather than listing: an MPQ
/// resolves by hash, so a file absent from `(listfile)` still reads perfectly
/// and a directory check would answer about the wrong thing.
fn open_data(data: &std::path::Path, locale: &str) -> Result<Chain> {
    let mut chain = Chain::open_wow_data(data, locale)
        .with_context(|| format!("opening archives under {}", data.display()))?;
    chain.read(dbc::schema::Map::PATH).with_context(|| {
        format!(
            "{} does not look like a WoW 3.3.5a Data directory: the archives there \
             hold no {}",
            data.display(),
            dbc::schema::Map::PATH
        )
    })?;
    Ok(chain)
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
    build_offline_scene(
        gpu,
        meshes,
        terrain_renderer,
        liquid_renderer,
        liquid_types,
        chain,
        args,
    )
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
    let scene = world_for_live(gpu, meshes, chain, args, &live)?;
    Ok((scene, Some(live)))
}

/// Builds the streaming world around a character that is already in it.
///
/// Split out of [`build_live_scene`] because the sign-in screen reaches this
/// point by a different road: it has chosen a realm and a character through
/// two lists rather than off the command line, and by then the window, the GPU
/// and the archives all already exist. **One function so both roads build the
/// same world** -- a second copy of this would be the place a feature added
/// for one path quietly failed to reach the other, which is how
/// `--screenshot` ended up not posing a single creature.
fn world_for_live(
    gpu: &Gpu,
    meshes: &mut MeshRenderer,
    chain: &mut Chain,
    args: &Args,
    live: &live::LiveWorld,
) -> Result<Scene> {
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
            drawable_with_own(live, live.position, (0.0, 0.0), 0.0, false, false)
                .iter()
                .map(|entity| {
                    let (look, look_key) =
                        entity_look(&mut player_looks, chain, &items, live, entity);
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
                        stealthed: entity.stealthed,
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
        world.update_animations(gpu, meshes, &render::cull::Attention::everything());
    }

    Ok(Scene::Streaming(Box::new(world)))
}

fn build_offline_scene(
    gpu: &Gpu,
    meshes: &MeshRenderer,
    terrain_renderer: &TerrainRenderer,
    liquid_renderer: &render::LiquidRenderer,
    liquid_types: &mut liquid::LiquidTypes,
    chain: &mut Chain,
    args: &Args,
) -> Result<Scene> {
    if let Some(display_id) = args.creature {
        let mut sources = model::Sources::default();
        let (path, variations) = model::creature(&mut sources, chain, display_id)?;
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
            model::load_dressed(gpu, meshes, chain, &path, &variations, args.lod, look.as_ref())?;
        // What this display costs the first time it comes into view, with no
        // realm and no window in the way.
        //
        // The same numbers the streaming path prints, from the same struct --
        // and reachable offline, which is what makes `--creature` a probe for
        // the load hitch rather than only a picture of one creature. A cost
        // measured against a live realm carries the realm's own latency; this
        // one carries nothing but the archives.
        tracing::info!(
            "display {display_id} ({path}) loaded in {:?} ({})",
            loaded.timings.total,
            loaded.timings.summary(),
        );
        return Ok(Scene::Model(Box::new(loaded)));
    }
    if let Some(path) = &args.model {
        let loaded = model::load(gpu, meshes, chain, path, &Variations::default(), args.lod)?;
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
                meshes,
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

/// How far the ground snap may move the character in one frame.
///
/// Not a physics constant and not a step height: it is the line between
/// "settling onto the surface under you" and "being teleported to whatever
/// answered before the real floor arrived". Generous on purpose -- a real
/// staircase or slope moves a character a fraction of this per frame, and
/// the only thing it needs to exclude is a large relocation to an incomplete
/// or unrelated surface.
///
const MAX_GROUND_SNAP: f32 = 5.0;

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

/// How far above the found floor the terrain field must sit before a ray is
/// treated as underground and the terrain fallback is refused for it.
///
/// **No longer the search bound for `floor_under_footing` itself** -- it was,
/// and that was foss-wow#137's second bug. `floor_under_footing`'s own `step`
/// parameter was built for climbing a stair: it bounds candidates from
/// *above* (`ceiling = from_z + step`) and leaves the depth below completely
/// unlimited, which is the opposite of what this constant's old doc comment
/// promised ("how far below"). Passed as that `step`, 5.0 units of upward
/// slack was "nowhere near large enough to reach a cave's ceiling" right up
/// until it measured one: a tunnel whose roof sits five units above the
/// walking floor lets `floor_under_footing` admit the roof itself as a
/// candidate floor whenever the character's head and the local ceiling
/// happen to be within reach of each other -- and `is_floor`/`floor_hit` take
/// `abs(normal.z)`, so a downward-facing ceiling passes the same near-
/// horizontal test a floor does (see `the_floor_under_you_is_the_one_you_are_
/// standing_on`, which depends on exactly that for a box's underside -- the
/// sign cannot be trusted to tell floor from ceiling here any more than it
/// could for the wall duck/pull-in decision). The real floor never needed an
/// upper bound at all: it is always *below* the query point, so any margin
/// admits it. `CAMERA_FLOOR_STEP` is that margin now, kept deliberately
/// small; this constant keeps its one remaining job, the underground
/// threshold below.
const CAMERA_FLOOR_REACH: f32 = 5.0;

/// The upper margin `floor_under_footing` is searched with from the camera
/// path -- how far above the query point a candidate floor may sit and still
/// be accepted, not how deep the search reaches (that is unlimited, and
/// covers whatever `CAMERA_FLOOR_REACH` used to be trying to promise).
///
/// Comfortably larger than ordinary terrain noise and a stair's `STEP_HEIGHT`
/// (0.8), and nowhere near `CAMERA_FLOOR_REACH`: a five-unit margin is what
/// let a low cave ceiling answer for a query aimed at the character's own
/// head. The true floor under an indoor or underground character sits below
/// `focus.z`, never above it, so shrinking this cannot lose it.
const CAMERA_FLOOR_STEP: f32 = 1.0;

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
    const REFINE: usize = 6;
    let span = eye - focus;
    // Unknown terrain (not streamed in yet) is treated as clear, the same
    // direction the rest of the streaming code fails in -- stopping short on
    // a tile that has not loaded would yank the camera into the character
    // every time the world was still catching up.
    let clear = |t: f32| -> bool {
        let at = focus + span * t;
        match ground_at(at.x, at.y) {
            Some(ground) => at.z >= ground + CAMERA_GROUND_CLEARANCE,
            None => true,
        }
    };
    let mut allowed = 1.0f32;
    for step in 1..=STEPS {
        let t = step as f32 / STEPS as f32;
        if !clear(t) {
            // The coarse pass only brackets *which* twelfth the ground comes
            // up in; snapping `allowed` to that twelfth is what a cave ceiling
            // turned into a visible pop -- a step at the surrounding rock's
            // edge (present in one frame, absent from `ground_at` in the
            // next) flipped the bracket by a whole twelfth on its own, with
            // nothing else about the camera having moved. Bisecting inside
            // the bracket makes `allowed` a continuous function of the ray
            // instead of one quantised to n/STEPS.
            let mut lo = (step - 1) as f32 / STEPS as f32;
            let mut hi = t;
            for _ in 0..REFINE {
                let mid = (lo + hi) * 0.5;
                if clear(mid) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            allowed = lo;
            break;
        }
    }
    let pulled = focus + span * allowed;
    // **Logged whenever this pass actually shortens the ray**, the same
    // reasoning `pull_camera_in_front_of_walls` carries its own two lines
    // for -- this is the pass that turned out to be the real cause the
    // three attempts at that one never touched: unlogged, it silently
    // walked the eye back to the character every time `ground_at` answered
    // for the wrong surface (the report a cave floor read as "eighteen
    // units underground" against the outdoor terrain field, before this
    // pass learned to ask the collision mesh first).
    //
    // **`trace`, not `debug`, for the same reason `pull_camera_in_front_of_
    // walls`' two lines are.** `allowed < 1.0` is the ordinary case on any
    // multi-level interior -- a balcony, a stairwell, an uneven floor -- and
    // this pass runs once per sampled yaw, so a stairwell logged this every
    // frame, identical value or not, right alongside the other two lines
    // that got the same fix.
    if allowed < 1.0 {
        tracing::trace!(
            "camera ground clearance: allowed={allowed:.3} of the ray, eye {:.2},{:.2},{:.2} -> {:.2},{:.2},{:.2}",
            eye.x, eye.y, eye.z, pulled.x, pulled.y, pulled.z
        );
    }
    pulled
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
/// face. See [`HIDE_OWN_MODEL_DISTANCE`], which is what stops that from
/// happening now.
const CAMERA_MIN_PULL_IN: f32 = 1.5;

/// The farthest the follow camera's nominal orbit may reach while the
/// character is standing on a modelled building floor.
///
/// **`pull_camera_in_front_of_walls` tests one ray, and a single ray cannot
/// tell "this room's wall stopped me" from "I slipped through a gap and
/// eventually grazed something else."** Reported from Northshire Abbey's
/// bell-tower stairs as the camera ending up outside the building, looking
/// back at its own roof -- and the trace this client did not yet have showed
/// why neither existing pass caught it: the wheel was zoomed to
/// [`ui::camera::MAX_DISTANCE`] (30 units, the maximum it allows), and
/// `pull_camera_in_front_of_walls` reported a hit 23 units out and pulled the
/// eye in to just short of it. That hit was real -- the function did exactly
/// what it always does -- it just was not the near wall of the small room the
/// character was standing in; the ray had already left through an opening and
/// only found *something* solid, far enough away for a rooftop and the forest
/// beyond it to fill the frame. The default distance is 9 (see
/// `ui::Camera::default`), which is what every earlier indoor milestone
/// happened to be tested at and why none of them saw this. 15 is generous
/// room for an interior view without reopening the 23-unit gap a full
/// zoom-out proved possible.
const CAMERA_INDOOR_DISTANCE_CAP: f32 = 15.0;

/// How far to either side of dead centre the follow camera samples an
/// additional ray, in radians, when deciding how close a wall or ceiling has
/// pulled it in.
///
/// **The wall/ceiling test only ever asks about the one ray to the
/// character, and a clear centre ray says nothing about the rest of what is
/// on screen.** Reported live as a "bleed" -- the character framed correctly
/// in a stairwell, and open valley visible through what should have been a
/// wall, off to one side. The eye itself was close and reasonably placed;
/// the corner it was sitting next to just was not visible along the one ray
/// this client had ever asked about. Two side samples at a plausible edge of
/// the field of view (the follow camera's is 65 degrees, see [`Fly::fov_y`]'s
/// default; half of that is 32.5, so this sits a little inside the true
/// edge rather than at it) and taking whichever pulled hardest is not a
/// proof the whole frustum is clear -- three rays are not infinitely many --
/// but it catches a corner the size of the one reported, which spanned a
/// third of the frame.
const CAMERA_FOV_SAMPLE_ANGLE: f32 = 25f32 * (std::f32::consts::PI / 180.0);

/// Samples `eye_at` at `center_yaw` and at [`CAMERA_FOV_SAMPLE_ANGLE`] to
/// either side, and returns whichever pulled the eye in hardest -- placed on
/// the *centre* ray, not left at the angled sample's own position, so the
/// character stays framed dead centre.
///
/// A free function taking `eye_at` as a parameter, rather than three calls
/// inlined at the one call site, so the policy -- sample a few angles, keep
/// the strictest, apply it to the centre -- has something to be tested
/// against without a live world behind it. See [`CAMERA_FOV_SAMPLE_ANGLE`]
/// for what this is guarding against.
fn tightest_eye_at_center(
    center_yaw: f32,
    distance: f32,
    focus: glam::Vec3,
    eye_at: impl Fn(f32, f32) -> glam::Vec3,
) -> glam::Vec3 {
    // **The centre sample is kept, because the last line usually wants it
    // again.** Each `eye_at` marches the ground in up to eighteen steps and
    // then casts a wall ray, every one of them fanned across every resident
    // tile -- so this function is four of the most expensive queries in the
    // frame, and one of the four was a duplicate whenever nothing obstructed
    // the view. Outdoors that is nearly always.
    let centre = eye_at(center_yaw, distance);
    let tightest = [
        (centre - focus).length(),
        (eye_at(center_yaw + CAMERA_FOV_SAMPLE_ANGLE, distance) - focus).length(),
        (eye_at(center_yaw - CAMERA_FOV_SAMPLE_ANGLE, distance) - focus).length(),
    ]
    .into_iter()
    .fold(f32::INFINITY, f32::min);
    let wanted = tightest.min(distance);
    // Exactly the arguments `centre` was computed with, so this is the same
    // answer rather than a nearly-identical one -- the comparison is against
    // `distance` itself and not a tolerance, because a tolerance here would
    // silently return a stale eye for a slightly different orbit.
    if wanted == distance {
        return centre;
    }
    eye_at(center_yaw, wanted)
}

/// The orbit distance to actually build the camera at, capped indoors.
///
/// A free function rather than inlined at the one call site so the cap has
/// something to be tested against without a live world -- see
/// [`CAMERA_INDOOR_DISTANCE_CAP`].
fn indoor_capped_distance(distance: f32, standing_on_a_floor: bool) -> f32 {
    if standing_on_a_floor {
        distance.min(CAMERA_INDOOR_DISTANCE_CAP)
    } else {
        distance
    }
}

/// Below this, the character's own body is left out of the drawn list
/// entirely, the way the original hides it in first person.
///
/// **Strictly under [`ui::camera::MIN_DISTANCE`] (2.5) on purpose.** Zooming
/// the wheel in as far as it goes never gets closer than that, so this can
/// only be reached by [`pull_camera_in_front_of_walls`] shortening the ray
/// against something solid -- a wall directly behind the character, or a
/// cave ceiling close overhead, down to [`CAMERA_MIN_PULL_IN`] at the
/// closest. Reported from live play as the camera "going first person" in a
/// low tunnel: correct once the body is out of the way, and a screenful of
/// the inside of a face until then.
const HIDE_OWN_MODEL_DISTANCE: f32 = 2.0;

/// Pulls the camera in until nothing solid is between it and the character.
///
/// Buildings, not terrain: [`pull_camera_out_of_the_ground`] already handles
/// the ground it stands on, and it does that by sampling a height field, which
/// knows nothing about a wall. Standing inside the abbey with the camera
/// outside it -- the view passing through a wall and looking back in -- is what
/// this fixes, and no amount of ground sampling could.
///
/// **A wall is pulled in front of; a near-horizontal hit is ducked under or
/// over instead**, and only the second half is new. Both used to get the
/// identical response -- shorten the ray toward the character -- which is
/// right for a wall behind them and wrong for a low tunnel roof: a ceiling a
/// stride above the character's head reads, on that response, as the eye
/// landing on the character's own face, reported from live play as the
/// camera "going first person" in the Northshire cave. "Near-horizontal" is
/// the same `abs(normal.z)` test that already tells a floor from a wall for
/// a walking body -- see `collision::FLOOR_NORMAL_Z`.
///
/// **Which way to duck is read from the hit's *position*, not the
/// triangle's normal sign.** The magnitude test is trustworthy regardless of
/// which way a triangle winds -- `abs` cannot disagree with itself -- but
/// the sign is a claim about which side of the mesh is "outside", and this
/// crate never checked it: every use up to now only ever asked how steep a
/// surface was, never which way it faced, so a WMO wound the other way from
/// what a test cube happens to produce would silently invert "ceiling" and
/// "floor" here specifically. A hit above the focus point is something
/// overhead whichever way its normal claims to point, and a hit below it is
/// something underfoot -- the question this needs an answer to, asked in
/// terms nothing about the source data has to be trusted for.
fn pull_camera_in_front_of_walls(
    focus: glam::Vec3,
    eye: glam::Vec3,
    first_hit: impl Fn(glam::Vec3, glam::Vec3) -> Option<(f32, glam::Vec3)>,
) -> glam::Vec3 {
    let span = eye - focus;
    let length = span.length();
    if length < 1e-3 {
        return eye;
    }
    let Some((t, normal)) = first_hit(focus, eye) else {
        // **The previously-silent branch.** Reported live as the camera
        // ending up outside the abbey while the character stood in an upper
        // room -- every other frame in that session logged a `camera duck`
        // or `camera pull-in` line, which means the one frame that actually
        // escaped took *this* path and left no trace of it. `trace`, not
        // `debug`: this is the ordinary case everywhere outdoors, and would
        // drown a `debug` capture the way the other two lines never do.
        tracing::trace!(
            "camera clear: nothing between focus {:.2},{:.2},{:.2} and eye {:.2},{:.2},{:.2} ({:.2} units)",
            focus.x, focus.y, focus.z, eye.x, eye.y, eye.z, length
        );
        return eye;
    };
    if normal.z.abs() >= collision::FLOOR_NORMAL_Z {
        let hit = focus + span * t;
        let ducked = if hit.z >= focus.z {
            // Something overhead: stay at or below the hit, minus a little air.
            glam::Vec3::new(eye.x, eye.y, eye.z.min(hit.z - CAMERA_WALL_CLEARANCE))
        } else {
            // Something underfoot, in the way from below: rise above it.
            glam::Vec3::new(eye.x, eye.y, eye.z.max(hit.z + CAMERA_WALL_CLEARANCE))
        };
        // **Logged because three guesses at this exact report is the
        // alternative.** Every earlier attempt reasoned about a low ceiling
        // from first principles and shipped without a single number from the
        // actual archway that keeps triggering it -- this is what the next
        // report should carry back instead: the hit fraction, the normal
        // this client read off the real geometry, and where the duck put the
        // eye, in one line rather than another guess.
        //
        // **`trace`, not `debug`, for the same reason the clear branch above
        // is.** Outdoors, clear is the ordinary case and duck is rare enough
        // to be worth debug's default visibility. Indoors, near any low
        // ceiling or stairwell, duck becomes the ordinary case instead --
        // reported live as 4,508 of these two branches' lines in 34 seconds
        // near a stairwell, most of them the identical result logged again
        // because nothing had moved. A debug capture drowns in it exactly
        // the way the outdoor comment already warned about.
        tracing::trace!(
            "camera duck: t={t:.3} normal=({:.2},{:.2},{:.2}) hit.z={:.2} \
             focus.z={:.2} eye {:.2},{:.2},{:.2} -> {:.2},{:.2},{:.2}",
            normal.x, normal.y, normal.z, hit.z, focus.z,
            eye.x, eye.y, eye.z, ducked.x, ducked.y, ducked.z
        );
        return ducked;
    }
    // The hit is a fraction of the way out; back off a fixed distance from it
    // and never come closer to the character than the floor above.
    //
    // **`CAMERA_MIN_PULL_IN` is a floor on `stopped`, not a fact about
    // `length`.** A single call always had `length` starting at the full
    // nominal orbit distance, comfortably above 1.5, so `clamp`'s two bounds
    // never crossed. `pull_camera_clear_of_the_building` feeds this
    // function's own output back in as the next call's `eye` -- and a duck
    // is bounded by nothing but a nearby ceiling, so a second pass can be
    // handed a `length` already under 1.5. `clamp(min, max)` panics the
    // moment `min > max`, live, mid-frame: crashed the client entirely,
    // reported back as "same issue, no improvement" because from outside a
    // crash and an uncaught escape look identical for one frame. Capping the
    // floor at `length` itself is the only sane answer at that range anyway
    // -- closer than 1.5 is impossible without leaving the segment.
    let stopped =
        (t * length - CAMERA_WALL_CLEARANCE).clamp(CAMERA_MIN_PULL_IN.min(length), length);
    let pulled = focus + span * (stopped / length);
    // `trace`, not `debug` -- see the identical note on `camera duck` above.
    // This is the branch a stairwell hits on every one of the follow
    // camera's three sampled rays, every frame, so at `debug` it dominated a
    // capture with thousands of lines repeating the same static result.
    tracing::trace!(
        "camera pull-in: t={t:.3} normal=({:.2},{:.2},{:.2}) stopped={stopped:.2} of \
         {length:.2} eye {:.2},{:.2},{:.2} -> {:.2},{:.2},{:.2}",
        normal.x, normal.y, normal.z, eye.x, eye.y, eye.z, pulled.x, pulled.y, pulled.z
    );
    pulled
}

/// Applies [`pull_camera_in_front_of_walls`] until it stops moving the eye,
/// rather than once.
///
/// **The wall pass proves the segment it returns is clear; the duck branch
/// does not.** A wall-shortened eye is provably fine, because it only ever
/// slides in along the very ray that was just tested. A *ducked* eye is a
/// different segment entirely -- straight down from wherever the orbit's
/// horizontal offset happened to land -- and nothing has asked whether
/// *that* line crosses a wall. In a small room it usually does: two
/// live-captured frames one mouse-tick apart aimed almost the same ray, one
/// found a low roof far out and ducked under it, x and y untouched, ending
/// up 11 units from the character; the other found the room's own nearby
/// wall first and pulled in to 2. Both were the same wall -- the duck had
/// simply never been asked about it. Bounded rather than run to a fixed
/// point: two ducks trading places every pass is possible in principle
/// (nothing here proves it terminates), and a bounded loop degrades to "not
/// perfectly caught" instead of hanging a frame.
fn pull_camera_clear_of_the_building(
    focus: glam::Vec3,
    eye: glam::Vec3,
    first_hit: impl Fn(glam::Vec3, glam::Vec3) -> Option<(f32, glam::Vec3)>,
) -> glam::Vec3 {
    const MAX_PASSES: usize = 4;
    let mut candidate = eye;
    for _ in 0..MAX_PASSES {
        let next = pull_camera_in_front_of_walls(focus, candidate, &first_hit);
        if (next - candidate).length() < 1e-3 {
            return next;
        }
        candidate = next;
    }
    candidate
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

/// Rebuilds yaw and pitch so the camera looks at `focus` from wherever `eye`
/// actually ended up.
///
/// `pull_camera_out_of_the_ground` and the wall-pulling half of
/// `pull_camera_in_front_of_walls` only ever slide `eye` along the orbit's own
/// view ray, so the original yaw and pitch still happen to aim at `focus`
/// afterwards. **The ceiling-duck half does not** -- it deliberately moves
/// `eye` straight down in `z` and leaves `x`/`y` alone, on purpose, so a low
/// roof lowers the camera instead of zooming it in over the character's
/// shoulder. `follow_camera_to_character` used to hand that adjusted `eye`
/// to the `Fly` camera while keeping the *pre-adjustment* yaw and pitch, so
/// the position moved but the aim did not: a duck left the eye somewhere new
/// while the view kept pointing where the un-ducked position would have
/// looked, past the character rather than at them. In a tight stairwell with
/// a low, sloped roof the duck engages on nearly every frame, and each
/// step's slightly different hit point moves the eye again without ever
/// correcting where it looks -- reported from Northshire Abbey's stairs as
/// the camera swinging wildly through the walls on the way up.
fn face_focus_from(eye: glam::Vec3, focus: glam::Vec3) -> (f32, f32) {
    let to_focus = focus - eye;
    (
        to_focus.y.atan2(to_focus.x),
        to_focus.z.atan2(to_focus.truncate().length()),
    )
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

/// How dark a shadow gets, as a fraction of the direct light it removes.
///
/// **Chosen, and not from `Light.dbc`.** Band 8 of the eighteen is the only
/// one that is exactly neutral on every sample of every outdoor row -- 3,744
/// of 3,744, against one to six percent for every other band -- so it is a
/// scalar stored in a colour column rather than a colour, and a storm lowers
/// it on 1,052 of the 1,331 samples where it moves at all. That is the
/// shape a shadow strength would have. It is *not* named as one here: "band 8
/// is a packed scalar the weather weakens" is what was measured, and which
/// scalar it is has not been, and a wrong name for it would never fail
/// loudly. See `wow-cli light --band-survey`.
///
/// Not 1.0, because ambient light is not the only thing that reaches a
/// shadow: bounced light does too, and this renderer has no term for it.
const SHADOW_STRENGTH: f32 = 0.72;

/// The sky's own geometry and the sun's shadow map: everything a *place* has
/// and a model viewer does not.
///
/// A struct rather than five more arguments, and for a reason this project
/// has already paid for once -- `draw_streaming` takes no liquid cache
/// because a cache that can be passed wrong is worse than no parameter. These
/// five have to agree with each other (the map's size decides the receiver's
/// filter width, the radius decides its normal offset), so they travel
/// together or they drift.
struct Atmosphere<'a> {
    celestial: &'a render::CelestialRenderer,
    sky: &'a sky::SkyScene,
    /// `None` when shadows are switched off, which is a real state rather than
    /// a failure: `--no-shadows` is the A/B this milestone is judged by.
    shadow: Option<&'a render::ShadowMap>,
    /// Half the width of the shadow box, in world units.
    radius: f32,
    /// How dark a shadow gets, before the weather is applied.
    strength: f32,
}

/// Where to aim the shadow box.
///
/// **Ahead of the camera rather than on it.** A box centred on the eye spends
/// half its texels on ground behind the viewer, and the whole reason the box
/// is small is that its texels are precious. Two thirds forward is the usual
/// compromise and it is a choice, not a measurement.
///
/// **And on the ground rather than at eye height, which is the half that was
/// wrong first.** The box is 220 units deep along the sun's axis; a camera
/// three hundred units up over Elwynn therefore had the whole landscape
/// *outside* it, and the render came back with 162 pixels of 576,000
/// different from the same scene drawn with `--no-shadows`. Nothing failed:
/// the pass ran, the map filled with the depth of empty air, and every
/// surface that asked was told it was lit. A shadow box aimed at nothing and
/// a shadow feature that does not exist are the same picture, which is
/// exactly why the A/B was worth taking before believing the first one.
/// Multiplies the drawn list, spreading the copies out. See [`Args::stress`].
///
/// **Jittered, and by a hash rather than a counter.** Copies stacked at one
/// point would share a tile, a cell and very likely a bucket, so the load
/// would be a hundred creatures the client can treat as one -- which is the
/// cheap case, not the expensive one. Spread out they stream, cull, collide
/// and pose independently, the way a crowd does.
///
/// The guid is offset into a range the server cannot use, because every cache
/// downstream is keyed by it -- bone buffers, emitter identities, remembered
/// looks. A copy that collided with a real guid would quietly replace a real
/// creature's state and the measurement would be of something else.
fn stress_crowd(drawn: &mut Vec<live::Entity>, factor: u32) {
    if factor <= 1 || drawn.is_empty() {
        return;
    }
    let original = drawn.len();
    for copy in 1..factor as u64 {
        for index in 0..original {
            let mut clone = drawn[index].clone();
            // A cheap spatial hash of (guid, copy): far enough apart to land
            // in different grid cells, close enough to stay in view.
            let mix = clone.guid.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ copy.wrapping_mul(0x1234_5678_9ABC_DEF);
            let dx = ((mix >> 8) & 0x3F) as f32 - 32.0;
            let dy = ((mix >> 20) & 0x3F) as f32 - 32.0;
            clone.position.x += dx;
            clone.position.y += dy;
            clone.guid = clone.guid.wrapping_add(copy << 56);
            drawn.push(clone);
        }
    }
}

/// The present mode asked for, if the surface supports it.
///
/// **Falls back rather than failing, and says so.** A mode the driver does not
/// offer is a fact about the machine, not a mistake by the person who asked --
/// and silently getting a different mode than the flag named is exactly how a
/// measurement gets believed when it should not be. The warning is the point.
fn chosen_present_mode(
    caps: &wgpu::SurfaceCapabilities,
    wanted: Option<&str>,
) -> wgpu::PresentMode {
    let Some(wanted) = wanted else {
        return caps.present_modes[0];
    };
    let mode = match wanted.to_ascii_lowercase().as_str() {
        "fifo" => Some(wgpu::PresentMode::Fifo),
        "fiforelaxed" | "fifo-relaxed" => Some(wgpu::PresentMode::FifoRelaxed),
        "immediate" => Some(wgpu::PresentMode::Immediate),
        "mailbox" => Some(wgpu::PresentMode::Mailbox),
        _ => None,
    };
    match mode {
        Some(mode) if caps.present_modes.contains(&mode) => mode,
        Some(mode) => {
            tracing::warn!(
                "this surface cannot present {mode:?}; using {:?} instead. Available: {:?}",
                caps.present_modes[0],
                caps.present_modes,
            );
            caps.present_modes[0]
        }
        None => {
            tracing::warn!(
                "unknown present mode {wanted:?}; using {:?}.                  Try fifo, fifo-relaxed, immediate or mailbox",
                caps.present_modes[0],
            );
            caps.present_modes[0]
        }
    }
}

fn shadow_centre(world: &world::World, camera: &Camera, radius: f32) -> glam::Vec3 {
    let ahead = camera.forward();
    // Flattened, because the box is aimed along the ground: following the
    // camera's pitch would swing the whole shadowed region into the sky the
    // moment somebody looked up.
    let flat = glam::Vec3::new(ahead.x, ahead.y, 0.0).normalize_or_zero();
    let at = camera.eye() + flat * radius * 0.6;
    // The ground under the aim point, or the eye's own height for a tile that
    // has not streamed in -- which is the same fallback every other height
    // query here makes, and is wrong in the direction that costs a shadow
    // rather than a crash.
    let ground = world.height_at(at.x, at.y).unwrap_or(at.z);
    glam::Vec3::new(at.x, at.y, ground)
}

/// What one frame cost: draw calls actually submitted, and the CPU
/// milliseconds spent producing them.
///
/// **Counts and times together, because the fix differs by which half is
/// large.** A frame that spends its budget *encoding* nine thousand draw
/// calls is bound by submission and wants culling; one that spends it in
/// `submit`/`present` is waiting on the GPU and wants fewer triangles or a
/// cheaper shader. Reported as a single `frame_ms` those are one number and
/// the same finding, and this project has already spent two guesses on a
/// stutter whose visible marker was printed by the thing that cost the frame
/// rather than by the cause -- see `CLAUDE.md`'s "a marker that a slow thing
/// finished looks exactly like its cause".
///
/// Every count is gathered at the `draw_indexed` rather than derived from
/// what is resident, because that difference *is* the question: how much of
/// the resident world reaches the command buffer. And the phases are each
/// measured rather than attributed -- see "a breakdown whose parts do not sum
/// to its total is not a breakdown", which is what caught a model being
/// re-parsed once per costume outside everything being timed. `other_ms`
/// below is that missing third, named instead of hidden.
#[derive(Default, Clone, Copy)]
struct FrameProfile {
    /// Draw calls in the sun's depth-only pass, which draws the resident
    /// world a second time. Nothing is culled from it -- 4.29 shipped saying
    /// so, and named this the first thing to measure if the frame rate is
    /// short.
    shadow_draws: u32,
    /// Terrain chunk draws in the visible pass: 256 per resident tile, each
    /// with a bind group of its own.
    terrain_draws: u32,
    /// Building, doodad and creature draws in the visible pass.
    model_draws: u32,
    /// Groups walked to produce those draws, whether or not they drew
    /// anything. A group is one model and every placement of it on one tile.
    groups: u32,
    /// Placements those groups carry, summed. The gap between this and
    /// `groups` is what instancing is saving; the gap between `model_draws`
    /// and `groups` is what a model's material count is costing.
    instances: u32,
    /// Triangles submitted, instances included. Kept beside the draw counts
    /// so "too much geometry" and "too many submissions" can be told apart:
    /// the original client draws this same city at 160fps, so a triangle
    /// count that looks ordinary points squarely at the other half.
    triangles: u32,
    /// Draws the frustum test skipped, across both passes.
    ///
    /// **Printed beside what was drawn, always, including when it is zero.**
    /// A culling pass that has quietly stopped working and a view with
    /// nothing off screen produce the same picture and the same three draw
    /// counts; only this number tells them apart. Same rule as the
    /// placeholder-texture counter, which could not distinguish "none were
    /// wrong" from "there were none" until it printed both.
    culled_draws: u32,

    /// How long `redraw` itself took, start to finish.
    ///
    /// **The number that separates "our frame is slow" from "the frame loop
    /// is slow".** `frame_ms` runs from one redraw's start to the next, so it
    /// includes the event loop, winit's input handling and whatever waits
    /// between presenting and being asked for the next frame. Subtracting the
    /// phases below from `frame_ms` puts all of that in the same bucket as
    /// untimed work inside the draw, and those two want opposite
    /// investigations. Same rule as splitting a load's read from its parse:
    /// timed together, either fix looked equally reasonable.
    redraw_ms: f32,
    /// Building the interface, which is a full egui pass over every frame.
    ///
    /// The largest single phase in the frame once culling and the collision
    /// fix had landed -- 3.70 ms of 14.48, measured -- which is why it is
    /// split below rather than left as one number.
    ui_ms: f32,
    /// The part of `ui_ms` spent assembling the debug window's *text* before
    /// the egui pass begins: the scene summary, the replicated-object census,
    /// the emitter counts and this profile's own line.
    ///
    /// **Its own number because the two halves have unrelated fixes.** Text a
    /// person reads does not need rebuilding sixty times a second and can
    /// simply be throttled; egui's layout and tessellation of the action
    /// bars, minimap, tracker and party frames cannot, and wants fewer or
    /// cheaper widgets. Guessing between them is how this investigation
    /// wasted a guess on the camera and another on draw calls.
    ui_text_ms: f32,
    /// The game's own interface -- frames, bars, minimap, tracker, chat and
    /// whichever panels are open -- inside the egui pass.
    ui_hud_ms: f32,
    /// The debug window sitting on top of it, inside the same pass.
    ///
    /// **Separated because one of these is the product and the other is
    /// scaffolding.** Three and a half milliseconds spent drawing the
    /// interface a player actually uses is a design conversation; the same
    /// spent re-laying out a stats paragraph nobody reads at sixty hertz is a
    /// bug with an obvious fix. `ui_ms` cannot tell them apart, and
    /// `ui_text_ms` has already ruled out *building* those strings at 0.10 ms
    /// -- laying them out is a different question.
    ui_stats_ms: f32,
    /// Assembling the interface's inputs before the egui pass can begin:
    /// over a thousand lines of cloning and formatting, every frame.
    ui_snapshot_ms: f32,
    /// The four ungated parts of that snapshot. Everything else in it is
    /// behind an "is this panel open" test and costs nothing while closed.
    ui_markers_ms: f32,
    ui_bars_ms: f32,
    ui_panels_ms: f32,
    ui_map_ms: f32,
    /// The whole egui pass, closure included. Subtracting `ui_hud_ms` and
    /// `ui_stats_ms` leaves what egui spends beginning and ending it.
    ui_egui_ms: f32,
    /// Walking the character: collision, sliding, the outgoing movement
    /// stream.
    movement_ms: f32,
    /// Placing the follow camera, which is the heaviest collision user in the
    /// frame -- four orbit rays, each marched through up to eighteen floor
    /// lookups, each fanned across every resident tile.
    camera_ms: f32,
    /// Draining the world connection.
    network_ms: f32,
    /// Zone music, ambience and footsteps -- the last of which asks the
    /// collision mesh what is underfoot.
    sound_ms: f32,
    /// Admitting and evicting tiles.
    stream_ms: f32,
    /// Rebuilding every replicated entity's instance buffer, which happens
    /// every frame by design -- see the note at the call site.
    entities_ms: f32,
    /// Posing skeletons.
    animations_ms: f32,
    /// Stepping particle and ribbon emitters.
    emitters_ms: f32,
    /// Waiting for a surface texture. **Its own number on purpose**: this is
    /// where a GPU-bound frame blocks, and folded into `encode_ms` it would
    /// read as expensive submission and send the reader to cull something
    /// that was never the cost.
    acquire_ms: f32,
    /// Tessellating the interface, uploading its geometry and encoding its
    /// render pass -- the part of the frame the headless path never does.
    /// Carved out of `encode_ms`, which contained it unnamed.
    interface_ms: f32,
    /// Writing the command buffer -- every `set_pipeline`, `set_bind_group`
    /// and `draw_indexed` above, plus egui's own pass.
    encode_ms: f32,
    /// `queue.submit`: handing the finished command buffer to the driver.
    ///
    /// **Split from the present below, because they answer different
    /// questions.** This one scales with what is *in* the buffer -- draw
    /// calls, pipeline switches, bind group switches -- so if it is the large
    /// half, fewer and better-batched draws help. Presenting does not care
    /// how the frame was built at all. Reported as one number they are the
    /// same finding, and the fix for one is wasted effort on the other:
    /// exactly the trap `other` was hiding twice already.
    submit_ms: f32,
    /// `queue.present`: giving the frame to the swapchain and the compositor.
    ///
    /// Nothing this client does to the scene changes it. A large number here
    /// is the display path, and the honest response is to say so rather than
    /// to go and optimise geometry.
    present_ms: f32,
    /// The gap before this frame: from the previous redraw's *end* to this
    /// one's start, stamped at both ends rather than derived.
    ///
    /// **It was derived, and the derivation was wrong.** `frame_ms` runs
    /// start-to-start, so it measures the *previous* frame's period, and
    /// `outside = frame_ms - redraw_ms` was subtracting *this* frame's redraw
    /// from it. Two different frames. Worse, the worst-frame picker selects
    /// the largest `frame_ms`, which is by definition a frame whose
    /// predecessor was slow -- so the subtraction charged that slow
    /// predecessor's redraw to this frame's gap, every time, and reported a
    /// 6.3 ms average gap that survived turning vsync off because it was
    /// never about the display at all.
    ///
    /// A measurement that is a difference of two things measured on different
    /// frames is not a measurement. Stamped now.
    gap_ms: f32,
    /// Events handled in the gap before this frame, and what they cost.
    ///
    /// **Both, because the two failure modes look identical in a total.** A
    /// hundred cheap cursor moves and one expensive event give the same
    /// millisecond figure and want opposite fixes -- the first is a rate
    /// problem (steering warps the pointer back to where it was pressed, and
    /// a warp is itself motion, so that path can feed itself), the second is
    /// a cost problem. Same reason `collision::Probe` counts lookups as well
    /// as candidates.
    gap_events: u32,
    gap_events_ms: f32,
    /// What the previous frame's log line cost to emit.
    ///
    /// **Charged to this frame on purpose.** The line is written after the
    /// draw is measured, so its cost falls in the gap `frame_ms` attributes
    /// to the *next* frame -- where, until this existed, it was indistinguish-
    /// able from the scheduler not handing the process back. An instrument
    /// that quietly contributes to the thing it measures is the worst kind,
    /// and this project has already been caught reading a marker printed by
    /// the expensive thing as if it were the cause.
    log_ms: f32,
    /// Where the character is standing, whether a foot landed, and starting
    /// the sinks. See `App::update_sound`.
    sound_area_ms: f32,
    sound_steps_ms: f32,
    sound_play_ms: f32,
    /// Sound clips played this session, and how many of those needed an
    /// archive read. **Both**, because a cache that has stopped caching
    /// sounds exactly the same -- see `sound::Effects::reads`.
    clip_reads: u64,
    clip_plays: u64,
    /// The same pair for music and ambience -- see `sound::Channel::reads`.
    track_reads: u64,
    track_starts: u64,
    /// Entity instance buffers reused against created -- see `InstancePool`.
    buffers_reused: u64,
    buffers_created: u64,
    /// What a crowd costs, counted rather than inferred.
    ///
    /// **The frame is fine at a hundred and something and the question has
    /// moved on**: what happens in a city with forty people throwing spells.
    /// Every phase left is roughly a millisecond, and which of them explodes
    /// depends on things nothing was reporting -- how many *distinct*
    /// skeletons are being posed, how many particle systems are alive, how
    /// many sprites they are producing. A frame that is busy and a frame that
    /// is merely full look identical in a millisecond total, and they have
    /// completely different futures.
    skeletons: usize,
    entity_groups: usize,
    live_systems: usize,
    live_sprites: usize,
    live_ribbons: usize,
    /// Buffer writes staged this frame, and their bytes -- see `Gpu::writes`.
    /// **The staging belt is flushed at `submit`**, so every one of these is
    /// paid for in a phase that names none of them.
    write_calls: u64,
    write_bytes: u64,
    /// Collision work this frame -- see `collision::Probe`. Beside the times
    /// because a millisecond cannot say whether the cost is many cheap
    /// queries or a few expensive ones, and the two want different fixes.
    collision: collision::Probe,
    /// Frames drawn since this profile was last reported, and the slowest and
    /// fastest of them.
    ///
    /// **Because a once-a-second sample of a stutter is a lottery.** The
    /// first version of this logged whichever frame happened to be current
    /// when the second elapsed, and reported 79-103 fps for a session whose
    /// complaint was that moving indoors was slow. It was not wrong; it was
    /// answering a different question. What a person feels is the worst
    /// frame, so that is the one kept.
    frames: u32,
    worst_ms: f32,
    best_ms: f32,
}

impl FrameProfile {
    /// Everything above that was measured separately, so `other_ms` can be
    /// what is left rather than an attribution.
    fn accounted_ms(&self) -> f32 {
        self.ui_ms
            + self.movement_ms
            + self.camera_ms
            + self.network_ms
            + self.sound_ms
            + self.stream_ms
            + self.entities_ms
            + self.animations_ms
            + self.emitters_ms
            + self.acquire_ms
            + self.log_ms
            + self.encode_ms
            + self.submit_ms
            + self.present_ms
    }

    /// Three lines: what was submitted, what it cost, and how the frame
    /// times were spread. Shared by the debug window and the log so the two
    /// cannot disagree.
    fn describe(&self) -> String {
        let draws = self.terrain_draws + self.model_draws + self.shadow_draws;
        // Inside `redraw` but not measured by any phase above. Kept apart
        // from `outside` below -- see [`Self::redraw_ms`].
        let unaccounted = (self.redraw_ms - self.accounted_ms()).max(0.0);
        // Everything between this redraw ending and the next one starting:
        // the event loop, winit, input, and any wait for the display.
        let outside = self.gap_ms;
        format!(
            "{draws} draws/frame = {} terrain + {} models + {} shadow, \
             {} culled | {} groups, {} instances, {} ktris\n\
             redraw {:.1}: ui {:.1} = snapshot {:.1} (markers {:.1}, bars {:.1}, \
             panels {:.1}, map {:.1}) + egui {:.1} (hud {:.1}, stats {:.1}) \
             + text {:.1} | \
             move {:.1} | camera {:.1} | \
             net {:.1} | \
             sound {:.1} (area {:.1}, steps {:.1}, play {:.1}) | stream {:.1} | \
             entities {:.1} | anim {:.1} | \
             emitters {:.1} | acquire {:.1} | encode {:.1} (interface {:.1}) | \
             submit {:.1} | \
             present {:.1} | \
             rest {:.1} ms; outside redraw {:.1} = {} events in {:.1} + \
             {:.1} idle + {:.1} log ms\n\
             collision: {} queries, {} candidates ({} per query) | \
             clips {} played from {} reads, tracks {} started from {} reads | \
             instance buffers {} reused, {} created | \
             {} buffer writes staging {} KiB\
             load: {} skeletons over {} entity groups, {} emitters alive \
             ({} sprites, {} trail quads){}",
            self.terrain_draws,
            self.model_draws,
            self.shadow_draws,
            self.culled_draws,
            self.groups,
            self.instances,
            self.triangles / 1000,
            self.redraw_ms,
            self.ui_ms,
            self.ui_snapshot_ms,
            self.ui_markers_ms,
            self.ui_bars_ms,
            self.ui_panels_ms,
            self.ui_map_ms,
            self.ui_egui_ms,
            self.ui_hud_ms,
            self.ui_stats_ms,
            self.ui_text_ms,
            self.movement_ms,
            self.camera_ms,
            self.network_ms,
            self.sound_ms,
            self.sound_area_ms,
            self.sound_steps_ms,
            self.sound_play_ms,
            self.stream_ms,
            self.entities_ms,
            self.animations_ms,
            self.emitters_ms,
            self.acquire_ms,
            self.encode_ms,
            self.interface_ms,
            self.submit_ms,
            self.present_ms,
            unaccounted,
            outside,
            self.gap_events,
            self.gap_events_ms,
            // What the operating system did not hand back. **Not ours**, and
            // saying so is the point of measuring it: with `ControlFlow::Poll`
            // and a `request_redraw` at the end of every frame there is
            // nothing here this client chose to wait for, so a large number is
            // the driver, the compositor or the scheduler.
            (outside - self.gap_events_ms - self.log_ms).max(0.0),
            self.log_ms,
            self.collision.queries,
            self.collision.candidates,
            self.collision.candidates / self.collision.queries.max(1),
            self.clip_plays,
            self.clip_reads,
            self.track_starts,
            self.track_reads,
            self.buffers_reused,
            self.buffers_created,
            self.write_calls,
            self.write_bytes / 1024,
            self.skeletons,
            self.entity_groups,
            self.live_systems,
            self.live_sprites,
            self.live_ribbons,
            // Only where a spread was gathered, which is the log's line and
            // not the window's -- the window shows the frame that just
            // happened and has no second to average over.
            if self.frames > 0 {
                format!(
                    "\n{} frames in the last second, worst {:.1} ms, best {:.1} ms",
                    self.frames, self.worst_ms, self.best_ms
                )
            } else {
                String::new()
            },
        )
    }
}

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
    // The sky's geometry and the sun's shadow map. `None` for the offline
    // scenes, which have no place and therefore no sky -- the same reason
    // they get no gradient and no weather.
    atmosphere: Option<&Atmosphere<'_>>,
    // Filled as the pass is written, not read here. See [`FrameProfile`] for
    // why the counts are taken at the `draw_indexed` rather than off what is
    // resident.
    profile: &mut FrameProfile,
    // `false` submits everything -- see `Args::no_cull`.
    cull: bool,
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
            atmosphere,
            profile,
            cull,
        );
        return;
    }
    // A world holds many meshes, so it cannot go through the single-mesh path.
    if !terrain_parts.is_empty() || matches!(scene, Scene::World(_)) {
        if let Scene::World(world) = scene {
            world.update_animations(gpu, meshes, (seconds * 1000.0) as u32);
        }
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
                if let Some(item_bones) = item
                    .animation
                    .as_ref()
                    .map(|animation| &animation.buffer)
                    .or(bones)
                {
                    pass.set_bind_group(2, &item_bones.bind_group, &[]);
                }
                pass.set_vertex_buffer(0, item.mesh.vertices.slice(..));
                pass.set_index_buffer(item.mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                let instances =
                    item.instance_start..item.instance_start + item.instance_count;
                for (draw_index, draw) in item.draws.iter().enumerate() {
                    let (Some(pipeline), Some(bind)) =
                        (meshes.get(draw.state), binds.get(draw.texture))
                    else {
                        continue;
                    };
                    let Some(texture_bind) = item.texture_animation.bind(draw_index) else {
                        continue;
                    };
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(1, bind, &[]);
                    pass.set_bind_group(3, texture_bind, &[]);
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

            let texture_animation = match scene {
                Scene::Model(model) => &model.texture_animation,
                _ => return,
            };
            for (draw_index, draw) in draw_list.iter().enumerate() {
                let (Some(pipeline), Some(binds)) =
                    (meshes.get(draw.state), material_binds.get(draw.texture))
                else {
                    continue;
                };
                let Some(texture_bind) = texture_animation.bind(draw_index) else {
                    continue;
                };
                pass.set_pipeline(pipeline);
                pass.set_bind_group(1, binds, &[]);
                pass.set_bind_group(3, texture_bind, &[]);
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
            m2::Model::pose_bones_with_global_loops(
                &m.bones,
                seq,
                t,
                m.texture_animation.global_sequences(),
            )
                .iter()
                .map(|mat| mat.to_cols_array_2d())
                .collect()
        }
        _ => vec![glam::Mat4::IDENTITY.to_cols_array_2d(); m.bones.len().max(1)],
    };
    meshes.update_bones(gpu, bones, &pose);
    let sequence = anim.filter(|s| *s < m.sequences.len()).unwrap_or(0);
    let duration = m
        .sequences
        .get(sequence)
        .map(|s| s.duration_ms.max(1))
        .unwrap_or(1);
    m.texture_animation
        .update(gpu, meshes, sequence, time_ms % duration);
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
            m2::Model::pose_bones_with_global_loops(
                &m.bones,
                sequence,
                time_ms,
                m.texture_animation.global_sequences(),
            )
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
    atmosphere: Option<&Atmosphere<'_>>,
    profile: &mut FrameProfile,
    cull: bool,
) {
    let aspect = size.0 as f32 / size.1.max(1) as f32;
    // **The one uniform, kept.** The liquid pass has its own bind group and
    // therefore its own copy of the sun, the ambient and the fog -- and a
    // second *derivation* of those would agree with the terrain's only until
    // somebody edited one of them. Water lit half a stop off the shore it laps
    // against is a seam nothing would catch but an eye. Same rule as the
    // picking ray being unprojected from the matrix the scene was drawn with.
    // **Where the sun is, decided once.** The gradient's disc, the world's
    // diffuse term and the shadow box all take this one vector, so a sun the
    // shadows disagree with is not a state that exists.
    let sun = lighting.map_or(glam::Vec3::Z, |(_, hour)| sun_direction(hour));
    // A storm both hides the sun and softens what it casts, and the same
    // number does both -- see the gradient's `visibility` below.
    let clear = 1.0 - falling.map_or(0.0, |f| f.intensity);
    // `None` at night, under a storm, with shadows switched off, and wherever
    // there is no light data to put a sun in the sky. Each of those is a
    // reason for no shadows rather than a failure to produce any.
    let cast = atmosphere
        .filter(|_| lighting.is_some())
        .and_then(|a| a.shadow.map(|map| (a, map)))
        .and_then(|(a, map)| {
            let centre = shadow_centre(world, camera, a.radius);
            render::shadow::light_view_proj(centre, sun, a.radius, map.size())
                .map(|matrix| (a, map, matrix))
        })
        .filter(|(a, _, _)| a.strength * clear > 0.0);

    // **Built here rather than beside the pass that uses it**, because the
    // shadow pass sits between this point and where `view_proj` used to be
    // computed, and both passes want a frustum. One derivation, from the very
    // matrix the scene is drawn with -- the same rule the picking ray follows,
    // and for the same reason: a frustum rebuilt from the camera's angles
    // agrees with the drawn image only until somebody edits one of them, and
    // the failure is geometry vanishing at the edge of the screen.
    let view_proj = camera.view_proj(aspect);
    // `Frustum::everything` rather than an `Option` threaded through every
    // test below: a frustum that admits everything is a real frustum, and one
    // branch at the top beats six `if let`s in the hot loops.
    let frustum = if cull {
        render::cull::Frustum::from_view_proj(view_proj)
    } else {
        render::cull::Frustum::everything()
    };

    let lit = lit_uniform(
        camera,
        aspect,
        sky,
        lighting,
        cast.map(|(a, map, matrix)| ShadowTerms {
            matrix,
            radius: a.radius,
            texels: map.size(),
            strength: a.strength * clear,
        }),
    );
    meshes.update_camera(gpu, &lit);

    // **Before the world's pass, and in its own.** A depth-only pass with no
    // colour attachment cannot share the scene's, and the scene's fragments
    // read the map this one writes -- so the order is not a preference.
    if let Some((_, map, matrix)) = cast {
        // **The same test the rasteriser is already making, moved earlier.**
        // The sun's box is orthographic and depth-clipped like any other, so
        // a caster outside it contributes nothing to the map whether or not
        // it is submitted -- this cannot change the picture, only what it
        // costs to draw. And it is the change the shadow milestone itself
        // named as the first thing to measure: 110 units of box was being
        // handed the whole resident world, nine tiles of it, every frame.
        let shadow_frustum = if cull {
            render::cull::Frustum::from_view_proj(matrix)
        } else {
            render::cull::Frustum::everything()
        };
        map.set_matrix(gpu, matrix);
        let mut pass = map.begin(encoder);
        pass.set_pipeline(map.terrain_pipeline());
        for tile in world.tiles() {
            pass.set_vertex_buffer(0, tile.terrain.mesh.vertices.slice(..));
            pass.set_index_buffer(
                tile.terrain.mesh.indices.slice(..),
                wgpu::IndexFormat::Uint32,
            );
            for chunk in &tile.terrain.chunks {
                if !shadow_frustum.intersects(chunk.min, chunk.max) {
                    profile.culled_draws += 1;
                    continue;
                }
                profile.shadow_draws += 1;
                profile.triangles += chunk.index_count / 3;
                pass.draw_indexed(
                    chunk.first_index..chunk.first_index + chunk.index_count,
                    0,
                    0..1,
                );
            }
        }
        for group in world.tiles().flat_map(|t| t.groups.iter()).chain(world.entities()) {
            if group
                .bounds
                .is_some_and(|(min, max)| !shadow_frustum.intersects(min, max))
            {
                profile.culled_draws += group.model.draws.len() as u32;
                continue;
            }
            // The pose the visible pass will use, not a second evaluation of
            // it: a shadow computed from the bind pose while the creature runs
            // is a silhouette standing beside its own model.
            let Some(group_bones) = group
                .animation
                .and_then(|key| world.entity_bone_buffer(key))
                .or_else(|| {
                    group
                        .map_animation
                        .as_deref()
                        .and_then(|key| world.map_bone_buffer(key))
                })
                .or(bones)
            else {
                continue;
            };
            pass.set_bind_group(1, &group_bones.bind_group, &[]);
            pass.set_vertex_buffer(0, group.model.mesh.vertices.slice(..));
            pass.set_vertex_buffer(1, group.instances.buffer.slice(..));
            pass.set_index_buffer(
                group.model.mesh.indices.slice(..),
                wgpu::IndexFormat::Uint32,
            );
            for draw in &group.model.draws {
                // Glass, glows and sprays cast nothing. A torch flame with a
                // solid shadow is worse than a torch flame with none.
                if draw.state.blend.is_transparent() {
                    continue;
                }
                // A building's rooms, one at a time -- see `Group::part_bounds`.
                if group
                    .part_bounds
                    .as_ref()
                    .and_then(|parts| parts.get(draw.submesh_id as usize))
                    .is_some_and(|(min, max)| !shadow_frustum.intersects(*min, *max))
                {
                    profile.culled_draws += 1;
                    continue;
                }
                let Some(bind) = group.model.binds.get(draw.texture) else {
                    continue;
                };
                pass.set_pipeline(
                    if draw.state.blend == render::mesh::BlendMode::AlphaKey {
                        map.mesh_alpha_pipeline()
                    } else {
                        map.mesh_pipeline()
                    },
                );
                // Bound for both pipelines even though only the alpha one
                // reads it: a pipeline layout declares the group, so leaving
                // it unset is a validation error rather than a saved bind.
                pass.set_bind_group(2, bind, &[]);
                profile.shadow_draws += 1;
                profile.triangles += (draw.index_count / 3) * group.count;
                pass.draw_indexed(
                    draw.first_index..draw.first_index + draw.index_count,
                    0,
                    0..group.count,
                );
            }
        }
    }
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
        sun,
        sky.encode(lighting.map_or([1.0; 3], |(sample, _)| sample.disc)),
        // A storm hides the sun. `Light.dbc` has nothing to say about this,
        // but the alternative is a sun burning through an overcast sky.
        clear,
    );

    // Then everything that is *on* the sky, over the gradient and under the
    // world. See `sky::SkyScene::draw` for why the three are in this order.
    if let Some(a) = atmosphere {
        a.sky.draw(
            gpu,
            &mut pass,
            a.celestial,
            view_proj,
            camera.eye(),
            sun,
            lighting.as_ref().map(|(sample, _)| sample),
            lighting.map_or(12.0, |(_, hour)| hour),
            falling.map_or(0.0, |f| f.intensity),
            seconds,
        );
    }

    pass.set_bind_group(0, meshes.camera_bind_group(), &[]);
    pass.set_pipeline(terrain_renderer.pipeline());
    for tile in world.tiles() {
        pass.set_vertex_buffer(0, tile.terrain.mesh.vertices.slice(..));
        pass.set_index_buffer(tile.terrain.mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        for chunk in &tile.terrain.chunks {
            // **Per chunk, not per tile.** The camera stands on the tile it
            // is looking across, so a tile-level box is a box the camera is
            // inside and it passes every test there is; 256 chunks is the
            // grain at which "behind you" starts to mean anything.
            if !frustum.intersects(chunk.min, chunk.max) {
                profile.culled_draws += 1;
                continue;
            }
            pass.set_bind_group(1, &chunk.bind_group, &[]);
            profile.terrain_draws += 1;
            profile.triangles += chunk.index_count / 3;
            pass.draw_indexed(
                chunk.first_index..chunk.first_index + chunk.index_count,
                0,
                0..1,
            );
        }
    }

    // The map's own geometry and the server's objects draw identically; only
    // where the transforms came from, and which bone buffer they bind,
    // differs. Tile M2s and replicated entities can each carry an animated
    // bone buffer, so bind group 2 is chosen fresh per group instead of once
    // for the whole pass.
    for group in world.tiles().flat_map(|t| t.groups.iter()).chain(world.entities()) {
        {
            profile.groups += 1;
            profile.instances += group.count;
            if group
                .bounds
                .is_some_and(|(min, max)| !frustum.intersects(min, max))
            {
                profile.culled_draws += group.model.draws.len() as u32;
                continue;
            }
            let group_bones = group
                .animation
                .and_then(|key| world.entity_bone_buffer(key))
                .or_else(|| {
                    group
                        .map_animation
                        .as_deref()
                        .and_then(|key| world.map_bone_buffer(key))
                })
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
            for (draw_index, draw) in group.model.draws.iter().enumerate() {
                // A building's rooms, one at a time. This is the test that
                // matters in a city: Ironforge is one placement of one model
                // with 1,162 batches and 104 groups, and every batch of it
                // was submitted to show one room of it.
                if group
                    .part_bounds
                    .as_ref()
                    .and_then(|parts| parts.get(draw.submesh_id as usize))
                    .is_some_and(|(min, max)| !frustum.intersects(*min, *max))
                {
                    profile.culled_draws += 1;
                    continue;
                }
                // **The group's override, not the material's own state**, and
                // only where one was asked for. A tint with alpha under one is
                // invisible through an opaque pipeline -- the blend has to be
                // switched on for the number to mean anything -- so the two
                // travel together. See `world::Group::translucent`.
                let state = if group.translucent {
                    crate::world::translucent(draw.state)
                } else {
                    draw.state
                };
                let (Some(pipeline), Some(bind), Some(texture_bind)) = (
                    meshes.get(state),
                    group.model.binds.get(draw.texture),
                    group.model.texture_animation.bind(draw_index),
                )
                else {
                    continue;
                };
                pass.set_pipeline(pipeline);
                pass.set_bind_group(1, bind, &[]);
                pass.set_bind_group(3, texture_bind, &[]);
                profile.model_draws += 1;
                profile.triangles += (draw.index_count / 3) * group.count;
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
    let shadow = (!args.no_shadows).then(|| {
        render::ShadowMap::new(
            &gpu,
            args.shadow_size,
            meshes.bone_layout(),
            meshes.material_layout(),
        )
    });
    if let Some(map) = &shadow {
        meshes.attach_shadow_map(&gpu, map.view());
    }
    let terrain_renderer = TerrainRenderer::new(&gpu, format, meshes.camera_layout());
    let sky = render::SkyRenderer::new(&gpu, format);
    let mut celestial = render::CelestialRenderer::new(&gpu, format);
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

    // After the lighting tables, because the star dome is found through them.
    // **A headless render draws the sky and the shadows and still draws no
    // HUD** -- which is the distinction 4.24 had to learn the hard way, and is
    // why this milestone is one of the few whose picture half a screenshot
    // really can confirm.
    let mut sky_scene =
        sky::SkyScene::load(&gpu, &meshes, chain, &mut celestial, lighting.as_ref());
    let frame_lighting = resolve_lighting(
        lighting.as_ref(),
        live.as_ref(),
        weather,
        offline_map,
        args.hour,
        camera_eye,
    );
    sky_scene.set_skybox(
        &gpu,
        &meshes,
        chain,
        &mut celestial,
        lighting.as_ref(),
        frame_lighting.map_or(0, |(sample, _)| sample.skybox_id),
    );
    tracing::info!("{}", sky_scene.describe());

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
            // **Everything, offline.** One frame has no history to lose and
            // the warm-up below exists precisely to build some, so refusing
            // work here would make a headless render of a particle system
            // draw an emitter that had just been switched on -- the trap
            // `warm_emitters` already documents.
            let all = render::cull::Attention::everything();
            world.update_animations(&gpu, &meshes, &all);
            for _ in 0..60 {
                world.update_emitters(&gpu, &mut particles, &mut emitters, 1.0 / 60.0, &all);
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
    let mut headless = FrameProfile::default();
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
        frame_lighting,
        // The same fixed clock the weather gets, and for the same reason: a
        // river whose surface scrolled with the wall clock would make two
        // screenshots of one scene differ, which is precisely what
        // `--screenshot` exists not to do.
        4.0,
        Some(&Atmosphere {
            celestial: &celestial,
            sky: &sky_scene,
            shadow: shadow.as_ref(),
            radius: args.shadow_radius,
            strength: SHADOW_STRENGTH,
        }),
        &mut headless,
        !args.no_cull,
    );
    // **The headless path's whole reason for carrying one.** `--screenshot`
    // draws no HUD, so the debug window's copy of this cannot be captured
    // here -- and the counts are the half of the reading that does not need
    // a session at all, since what a frame *submits* is decided by what is
    // resident and where the camera is, both of which this path sets up
    // exactly as the windowed one does. The times are not comparable (one
    // frame, cold caches, no present); the counts are.
    tracing::info!("headless frame: {}", headless.describe());
    // **How long the GPU actually takes, measured rather than inferred.**
    // Every phase in the profile above is CPU time; none of them can say
    // whether the card is the limit. `submit` and the gap between frames are
    // both places a GPU-bound client waits, and both look like CPU cost from
    // the outside -- so this polls to completion and prints the real number.
    // Run at two resolutions it separates fill rate from geometry: a cost
    // that doubles with the pixel count is shading, one that does not is
    // vertices and draw calls.
    let gpu_started = Instant::now();
    gpu.queue.submit([encoder.finish()]);
    let submitted = gpu_started.elapsed().as_secs_f32() * 1000.0;
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("the device should finish the frame");
    tracing::info!(
        "gpu frame at {}x{}: {:.2} ms to submit, {:.2} ms until the GPU had finished          (first frame -- pipelines and residency are still cold; use --bench)",
        target.width,
        target.height,
        submitted,
        gpu_started.elapsed().as_secs_f32() * 1000.0,
    );

    if let Some(rounds) = args.bench.filter(|n| *n > 0) {
        let mut encode = Vec::with_capacity(rounds as usize);
        let mut total = Vec::with_capacity(rounds as usize);
        for _ in 0..rounds {
            let round = Instant::now();
            let mut encoder =
                gpu.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("bench"),
                    });
            let mut counts = FrameProfile::default();
            draw_scene(
                &gpu, &mut encoder, &target.view, &depth.view,
                (target.width, target.height), &scene, &camera, &blitter,
                &meshes, &terrain_renderer, &liquid_renderer, &liquid_types,
                &sky, &precipitation, &particles, &emitters,
                resolve_precipitation(weather, 4.0), &binds, bones.as_ref(),
                &world_binds, &identity, frame_lighting, 4.0,
                Some(&Atmosphere {
                    celestial: &celestial,
                    sky: &sky_scene,
                    shadow: shadow.as_ref(),
                    radius: args.shadow_radius,
                    strength: SHADOW_STRENGTH,
                }),
                &mut counts,
                !args.no_cull,
            );
            gpu.queue.submit([encoder.finish()]);
            // **Before the poll**, so this is what the CPU spent rather than
            // what it waited for. The two are the whole question.
            encode.push(round.elapsed().as_secs_f32() * 1000.0);
            gpu.device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("the device should finish the frame");
            total.push(round.elapsed().as_secs_f32() * 1000.0);
        }
        let stat = |v: &mut Vec<f32>| {
            v.sort_by(f32::total_cmp);
            (v[0], v[v.len() / 2], v[v.len() - 1])
        };
        let (e_min, e_mid, e_max) = stat(&mut encode);
        let (t_min, t_mid, t_max) = stat(&mut total);
        // **The minimum, not the mean.** Every sample is the true cost plus
        // whatever else the machine was doing, so the smallest is the closest
        // to the truth and the spread says how noisy the room was.
        tracing::info!(
            "bench {rounds} frames at {}x{}: cpu encode+submit min {e_min:.2} median              {e_mid:.2} max {e_max:.2} ms | gpu complete min {t_min:.2} median              {t_mid:.2} max {t_max:.2} ms",
            target.width,
            target.height,
        );
    }

    if let (Some(path), Some(map)) = (&args.shadow_dump, &shadow) {
        let depth = map.read_depth(&gpu)?;
        // Stretched to the range actually present rather than shown raw. An
        // orthographic box 440 units deep holding a landscape 40 units tall
        // uses a tenth of its range, and printed raw that is a white square
        // with a slightly-less-white square in it -- a picture that says
        // "empty" about a map that is full.
        let (lo, hi) = depth
            .iter()
            .filter(|d| **d < 1.0)
            .fold((1.0f32, 0.0f32), |(lo, hi), d| (lo.min(*d), hi.max(*d)));
        let span = (hi - lo).max(1e-6);
        let rgba: Vec<u8> = depth
            .iter()
            .flat_map(|d| {
                let v = if *d >= 1.0 {
                    255
                } else {
                    (((d - lo) / span) * 224.0) as u8
                };
                [v, v, v, 255]
            })
            .collect();
        write_png(path, &rgba, map.size(), map.size())?;
        let drawn = depth.iter().filter(|d| **d < 1.0).count();
        // Both numbers, always. "Nothing warned" is not "nothing was wrong".
        println!(
            "shadow map {0}x{0}: {drawn} texels drawn, {1} left at the far plane, depth {lo:.4}..{hi:.4} -> {2}",
            map.size(),
            depth.len() - drawn,
            path.display()
        );
    }

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
    /// The pipelines for what is drawn *on* the sky, and the three things
    /// drawn there. Two objects for the same reason the particles are: one is
    /// grown before the pass opens and the other only read once it has.
    celestial: render::CelestialRenderer,
    sky_scene: sky::SkyScene,
    /// The sun's depth map. `None` under `--no-shadows`, which exists because
    /// the honest way to judge a shadow is to be able to turn it off.
    shadow: Option<render::ShadowMap>,
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
    /// Guild chat.
    ///
    /// Refused locally when this character is in no guild, exactly as `Party`
    /// is when there is no group -- and for the identical reason: the server
    /// drops a guild line from a guildless character in **silence**, which is
    /// indistinguishable from a broken send.
    Guild,
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
            ChatChannel::Guild => (::world::ChatType::Guild, ""),
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
            ChatChannel::Guild => Some("guild".into()),
            ChatChannel::Yell => Some("yell".into()),
            ChatChannel::Whisper(name) => Some(format!("to {name}")),
        }
    }
}

struct App {
    args: Args,
    chain: Chain,
    /// The sign-in screen, present until a character has been entered as.
    ///
    /// **Its presence is the mode**, so nothing can be both signing in and in
    /// the world -- the same reason `swimming` is an `Option<Liquid>` rather
    /// than a bool beside a liquid. While it is here the world is not drawn,
    /// the HUD is not built, and the keyboard belongs to the panel.
    ///
    /// `None` from the start when the command line said what to connect to:
    /// `--realm-host --user --character` is a complete answer to everything
    /// this screen would ask, and a probe that stopped to ask again would not
    /// be a probe.
    signin: Option<signin::SignIn>,
    /// Set by the sign-in screen's Quit button and acted on where the event
    /// loop is in scope -- see the `RedrawRequested` arm.
    quit: bool,
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
    /// Set from a `Resized` event reporting a zero-area window -- what
    /// Windows sends a minimized window's client area as, rather than any
    /// `Occluded` event, which this platform does not deliver. While set,
    /// `redraw` skips the surface acquire and present entirely instead of
    /// repeatedly configuring a degenerate swapchain and blocking on
    /// `get_current_texture` against a surface nothing can present to --
    /// `foss-wow#138`, seen live as the window "closing itself out for a few
    /// moments" on minimize.
    minimized: bool,
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
    /// How far the wall/ceiling-avoiding eye currently sits from the
    /// character, easing towards whatever `tightest_eye_at_center` asks for
    /// this frame -- see [`App::camera_follow_wall_distance`]. `None` until
    /// the first frame places it.
    camera_wall_distance: Option<f32>,
    /// The height the character's own *drawn* body is standing at, easing
    /// towards `live.position.z` -- cosmetic only. `live.position.z` itself
    /// is the authoritative height: what collision, the camera's focus
    /// point and outgoing movement packets all use, unsmoothed, on purpose.
    /// This is a second, purely visual copy for exactly the reason
    /// `camera_z` exists one layer up -- see `App::camera_follow_z` and
    /// foss-wow#53. `None` until the first frame places it.
    drawn_own_z: Option<f32>,
    /// How far the eye actually ended up from the character this frame,
    /// after `pull_camera_out_of_the_ground` and `pull_camera_in_front_of_
    /// walls` have both had their say -- as opposed to `camera_distance`,
    /// which is the *nominal* orbit distance the wheel sets and says
    /// nothing about a low ceiling or a wall shortening it. Read by the
    /// draw call that decides whether to include the character's own body
    /// among the entities placed this frame: a wall directly behind the
    /// character, or a cave ceiling close overhead, pulls this well inside
    /// the model, and drawing it there is a screenful of the inside of a
    /// face rather than a character standing in a tight space. `None`
    /// before the first frame has placed a camera at all.
    camera_eye_distance: Option<f32>,
    /// When the client started, which is the only clock the weather has.
    started: Instant,
    frame_ms: f32,
    /// Exponential moving average of `1000.0 / frame_ms`, smoothed so the
    /// debug window's FPS reading is legible rather than changing every
    /// frame -- the raw reciprocal of a single frame's time swings wildly
    /// even when performance is steady.
    fps: f32,
    /// What the last frame submitted and what each phase of it cost. Not
    /// smoothed: the counts are exact facts about one frame and averaging
    /// them would hide the spike that is worth seeing.
    profile: FrameProfile,
    /// The slowest frame since the last log line, kept so the log reports the
    /// frame a person actually felt rather than whichever one the clock
    /// landed on. See [`FrameProfile::frames`].
    worst: FrameProfile,
    /// Accumulated by [`App::window_event`] between redraws and drained by
    /// the next one. See [`FrameProfile::gap_events`].
    gap_events: u32,
    gap_events_ms: f32,
    /// What the last log line cost, drained by the next frame. See
    /// [`FrameProfile::log_ms`].
    pending_log_ms: f32,
    /// When the last redraw arm finished, so the next one can measure the gap
    /// rather than infer it. See [`FrameProfile::gap_ms`]. Stamped in the
    /// event handler rather than inside `redraw`, because `redraw` has half a
    /// dozen early returns and a stamp that misses one reports the whole of
    /// the next frame as a gap.
    redraw_ended: Option<Instant>,
    /// The adapter's description, asked for once. See the call site.
    gpu_line: Option<String>,
    /// Written by [`App::build_ui`] and read by the frame that called it.
    /// See [`FrameProfile::ui_text_ms`].
    ui_text_ms: f32,
    /// Written by [`App::build_ui`] from inside the egui closure. See
    /// [`FrameProfile::ui_hud_ms`].
    ui_hud_ms: f32,
    ui_stats_ms: f32,
    ui_snapshot_ms: f32,
    ui_egui_ms: f32,
    sound_area_ms: f32,
    sound_steps_ms: f32,
    sound_play_ms: f32,
    ui_markers_ms: f32,
    ui_bars_ms: f32,
    ui_panels_ms: f32,
    ui_map_ms: f32,
    /// When the breakdown was last written to the log. **It goes to the log
    /// as well as to the window on purpose** -- `--screenshot` draws no HUD,
    /// so a reading that exists only in the debug window cannot be captured
    /// headlessly, and a frame-rate report from the window is exactly the
    /// kind of thing this project has twice had to reproduce.
    profile_logged: Instant,
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
    modeled_floor: bool,
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
    status_text: Option<PendingStatusText>,
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
    sound_enabled: bool,
    music_enabled: bool,
    audio: Option<rodio::OutputStream>,
    music: sound::Channel,
    ambience: sound::Channel,
    /// The rain or snow loop, independent of the zone's own ambience: a
    /// storm is a state of the *sky*, not of the area, and can start or stop
    /// under a zone that never changes what it otherwise sounds like. See
    /// `weather_ambience`.
    weather_channel: sound::Channel,
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
    /// Where the questgivers this character has walked past were standing, and
    /// what was over their heads at the time.
    ///
    /// **The only thing this client draws that is not a server answer**, and
    /// it exists because there is no opcode that asks where a quest is before
    /// you have found it. See `world::spawns` for what that costs and how the
    /// interface is obliged to say so.
    givers: ::world::Questgivers,
    /// Where that cache is written. `None` when no configuration directory
    /// could be found, exactly like `quest_cache_path`.
    giver_cache_path: Option<std::path::PathBuf>,
    /// The quest log the remembered marks were last reconciled against, so
    /// `Questgivers::forget_offering` runs when it changes rather than every
    /// frame.
    giver_marks_log: Vec<u32>,
    /// When the questgiver cache was last written.
    giver_saved_at: Instant,
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
    /// The open vendor's stock, or `None`. Same shape as [`Self::trainer`]
    /// and for the same reason: a vendor is very often also a gossip NPC,
    /// and Innkeeper Farley carries both the vendor and the questgiver bits
    /// alongside her innkeeper one -- so this is a separate field rather
    /// than a variant of one "NPC window" state, cleared by the questgiver
    /// window's Close button and replaced whenever another NPC is greeted.
    vendor: Option<VendorSession>,

    /// The open auction window, or `None`. See [`AuctionSession`].
    auction: Option<AuctionSession>,
    /// The mailbox currently open, or `None`.
    ///
    /// **Existence is the flag**, like the trainer's and the taxi's, and it
    /// holds the mailbox's **guid** rather than a boolean because every mail
    /// request names it: the server checks on each one that the object is
    /// still a mailbox this character can reach. A window that outlived the
    /// walk away from it would send requests refused for a reason nothing on
    /// screen explains, so it closes itself -- see [`App::mailbox_in_reach`].
    mailbox: Option<u64>,

    /// Whether the guild window is open.
    ///
    /// A `bool` rather than a guid, unlike the mailbox beside it: a roster is
    /// not attached to anything in the world, needs nothing in reach, and
    /// cannot be walked away from. That is the whole novelty of the milestone
    /// stated as a field type.
    guild_open: bool,

    /// Which guild's invitation has already been announced in chat.
    ///
    /// The invitation is *state* and the line is an *event*, so something has
    /// to stop it being said on every pump. Keyed by the guild's name rather
    /// than by a bare flag, so a second invitation from a different guild is
    /// announced rather than swallowed by the first one's flag.
    guild_invitation_said: Option<String>,
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
    // Where the *own* body is drawn -- `live.position` with its `z` eased,
    // see `App::drawn_own_z`. `drawable_entities` below still gets
    // `live.position` itself: only the own body's small, cosmetic wobble is
    // being hidden here, not anything about how other entities are placed
    // relative to the camera.
    own_position: glam::Vec3,
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
        own_position,
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
#[derive(Clone, Copy, Debug, PartialEq)]
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

/// What actually falls this frame, once indoors is accounted for.
///
/// `SMSG_WEATHER` is a *zone* state -- the realm does not stop sending rain
/// the moment a character steps through a doorway, and `resolve_precipitation`
/// has no notion of a roof at all: the field is a camera-relative box with no
/// world geometry in it (see `render::precipitation`), so nothing there would
/// ever have stopped it. `indoors` is `App::modeled_floor`, the same signal
/// footsteps already use to fall back from the terrain to a building's own
/// floor material -- "a building's floor outranks the terrain under it,
/// which is what makes an interior an interior" (see `drive_live_movement`),
/// so a rained-on abbey interior is the same class of bug as a footstep that
/// sounds like grass indoors.
fn precipitation_for_frame(
    weather: ::world::WeatherChange,
    seconds: f32,
    indoors: bool,
) -> Option<Falling> {
    if indoors {
        return None;
    }
    resolve_precipitation(weather, seconds)
}

/// Which rain/snow ambience loop this weather wants, if any.
///
/// A separate mapping from `resolve_precipitation`'s, and deliberately not
/// reusing `Weather::precipitation`: the renderer only has an intensity float
/// to scale one shared box of billboards by, but the archive has a *named*
/// loop per tier (`Weather - RainHeavy`, not "rain, hard"), and the wire
/// already tells us which one via the `Weather` variant itself. `BlackRain`/
/// `BlackSnow` fold onto the heavy loop -- there is no `Weather - BlackRain`
/// row, and black rain is the more violent of the two rains regardless.
fn weather_ambience(weather: ::world::Weather) -> Option<sound::WeatherAmbience> {
    use ::world::Weather::*;
    match weather {
        LightRain => Some(sound::WeatherAmbience::RainLight),
        MediumRain => Some(sound::WeatherAmbience::RainMedium),
        HeavyRain | BlackRain => Some(sound::WeatherAmbience::RainHeavy),
        LightSnow => Some(sound::WeatherAmbience::SnowLight),
        MediumSnow => Some(sound::WeatherAmbience::SnowMedium),
        HeavySnow | BlackSnow => Some(sound::WeatherAmbience::SnowHeavy),
        _ => None,
    }
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

/// What a frame's shadow map is, for the surfaces that will read it.
///
/// Five numbers that have to agree: the matrix decides where a surface looks
/// in the map, the radius and the texel count decide how far it steps off its
/// own normal first, and the strength decides how much notice it takes.
#[derive(Clone, Copy)]
struct ShadowTerms {
    matrix: glam::Mat4,
    radius: f32,
    texels: u32,
    strength: f32,
}

/// The camera uniform with real lighting folded in, or the placeholder when
/// there is none.
fn lit_uniform(
    camera: &Camera,
    aspect: f32,
    // Only for its colour conversion -- see the fog term below.
    sky: &render::SkyRenderer,
    lighting: Option<(dbc::light::Sample, f32)>,
    shadow: Option<ShadowTerms>,
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
    if let Some(shadow) = shadow {
        uniform.light_view_proj = shadow.matrix.to_cols_array_2d();
        uniform.shadow = [
            shadow.strength,
            // One texel, in the map's own texture coordinates: the PCF step.
            1.0 / shadow.texels.max(1) as f32,
            // And one texel in *world* units, which is what a surface steps
            // along its normal before asking. A shorter step leaves acne and a
            // longer one detaches a shadow from the thing casting it, and both
            // scale with the same number -- so this is derived from the box
            // rather than tuned against one screenshot.
            render::shadow::texel_size(shadow.radius, shadow.texels) * 1.5,
            0.0,
        ];
    }
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
/// How one replicated entity should be dressed, and the cache key that goes
/// with it.
///
/// **One function because there are two call sites and they must not drift.**
/// The windowed loop and `--screenshot` each built this inline, and a
/// screenshot that dressed people differently from the window is evidence
/// about neither -- the comment saying so was already in both copies, which is
/// the shape this project keeps finding: a trap documented at one call site
/// does not protect the next one. Making it an argument nobody can forget to
/// pass is what the rule actually recommends.
///
/// Three sources, in order of what actually knows:
///
/// - our own body's look was resolved at login from the character list;
/// - another player's comes off their update fields;
/// - a creature has none, and is dressed from its display id inside the
///   renderer.
///
/// **And a fourth case that overrules all three: a unit wearing a body that is
/// not its own gets no look at all.** A druid in bear form is still a night
/// elf and its appearance fields still say so, but none of it applies to the
/// model on screen: the composed character skin is uploaded into whichever
/// texture slot happens to be first, and `Look::shows` filters the bear's
/// geosets by rules written about hairstyles, beards and glove variants --
/// where "variant 1 is the bare body" is a fact about character models and
/// about nothing else. Handing it `None` sends the renderer down the path
/// every creature already takes, which is the right one: a bear's skin is a
/// property of its display id.
fn entity_look(
    cache: &mut std::collections::HashMap<u64, Rc<character::Look>>,
    chain: &mut Chain,
    items: &crate::items::Items,
    live: &live::LiveWorld,
    entity: &live::Entity,
) -> (Option<Rc<character::Look>>, u64) {
    if entity.transformed {
        return (None, 0);
    }
    if entity.guid == live.guid {
        return (Some(live.look.clone()), live.look_key);
    }
    let Some(appearance) = entity.appearance else {
        return (None, 0);
    };
    let (look, key) = player_look(cache, chain, items, appearance, &entity.visible_items);
    (Some(look), key)
}

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

/// An open vendor's stock, held the same way [`TrainerSession`] is and for
/// the identical reason: every field the server sent -- the discounted
/// price, what remains in stock -- was computed for this character at this
/// moment, and this client has no way to recompute any of it.
struct VendorSession {
    npc: u64,
    /// Resolved at request time, like the trainer's and the questgiver's.
    name: String,
    /// `None` between the request and the reply. Drawn as an empty window
    /// rather than as nothing, for the same reason the trainer's is: a
    /// window that appeared only once the reply arrived would make a slow
    /// realm look like a click that did not register.
    list: Option<::world::VendorList>,
}


/// The open auction window, and everything about it the wire does not carry.
///
/// **The offset is the interesting field.** A list result says how many rows
/// it holds and how many matched, and nothing at all about where in the match
/// those rows sit -- so the requester is the only thing that knows, and this
/// is where it is kept. Everything else here is either what the server said or
/// what the player picked.
struct AuctionSession {
    /// The auctioneer every request in this block has to name again.
    npc: u64,
    /// Which house it serves, once the greeting says so. `None` until then.
    ///
    /// The only field in the whole block that distinguishes two auctioneers,
    /// and a client that let the player walk from one to another without
    /// noticing would show one house's rows while sending another's requests.
    house: Option<u32>,
    tab: ui::AuctionTab,
    /// The row the current search was asked to start at.
    offset: u32,
    /// What is being searched for. Empty is everything.
    search: String,
    /// The selected auction, by the **server's id**: paging must not move a
    /// selection to whatever is now in that slot.
    selected: Option<u32>,
    /// Whether a request has gone out with no reply yet, so the window can say
    /// "asking" rather than looking like a search that matched nothing --
    /// the same picture and a different fact.
    waiting: bool,
}

impl AuctionSession {
    /// The request that matches this session's tab.
    ///
    /// One function so the tab and the request cannot disagree; three separate
    /// call sites is how a Browse tab ends up showing an owner list.
    fn ask(&self, connection: &mut ::world::Connection) -> Result<(), ::world::client::Error> {
        match self.tab {
            ui::AuctionTab::Browse => {
                let mut search = ::world::AuctionSearch::any();
                search.name = self.search.clone();
                connection.auction_list_items(self.npc, self.offset, &search)
            }
            // Neither of these pages, so neither carries an offset.
            ui::AuctionTab::Bids => connection.auction_list_bidder_items(self.npc, &[]),
            ui::AuctionTab::Selling => connection.auction_list_owner_items(self.npc),
        }
    }
}

struct Questgiver {
    npc: u64,
    /// Resolved at greeting time rather than per frame: an NPC's name cannot
    /// change while you are talking to it.
    name: String,
    /// What the greeting said this NPC has, in the order it said it.
    offered: Vec<u32>,
    /// The gossip menu's own speech lines -- "I'd like to browse your
    /// goods.", or a custom NPC's own scripted choices -- as sent, in order.
    /// Kept whole rather than reduced to labels: a click needs the option's
    /// own id back, and `menu_id` below.
    options: Vec<::world::GossipOption>,
    /// The menu id the current `options` came from. Sent back with a
    /// selection so the server knows which menu is being answered -- see
    /// `Connection::gossip_select`. A submenu's reply carries its own id,
    /// which is what makes clicking through a multi-step gossip tree work:
    /// each answer updates this before the next one is sent.
    menu_id: u32,
    /// The one quest whose text is on screen, if the player has picked one or
    /// the server volunteered it.
    showing: Option<u32>,
    /// Which of `showing`'s optional rewards is currently picked, for a
    /// `Complete` press to send. Reset to `0` every time `showing` changes,
    /// including to the same quest again -- a stale pick surviving a reopen
    /// would be a choice the player never actually made this time.
    selected_reward: usize,
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
        self.options = gossip.options.clone();
        self.menu_id = gossip.menu_id;
        // One quest and nothing else to choose between: show it straight
        // away rather than making the player click a list of one. **Only
        // when there are no speech options too** -- a menu offering both a
        // line to click and a quest to take is not "nothing else to choose
        // between", and jumping straight to the quest would hide the lines
        // the player never got to read.
        if self.options.is_empty() && self.offered.len() == 1 {
            self.showing = self.offered.first().copied();
            self.selected_reward = 0;
        }
    }

    /// Answers a speech line, whatever menu it came from.
    fn choose_option(&self, live: &mut live::LiveWorld, index: u32) {
        if let Err(e) = live.connection.gossip_select(self.npc, self.menu_id, index) {
            tracing::warn!("choosing gossip option {index} at {:#018x} failed: {e:#}", self.npc);
        }
    }

    /// The server put one quest's scroll on screen without being asked.
    fn note_quest_offered(&mut self, quest: u32) {
        if !self.offered.contains(&quest) {
            self.offered.push(quest);
        }
        self.showing = Some(quest);
        self.selected_reward = 0;
    }
}

struct PendingCombatText {
    world_pos: glam::Vec3,
    text: String,
    kind: ui::CombatTextKind,
    spawned: Instant,
}

struct PendingStatusText {
    text: String,
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

/// What to write in a letter's "from" column.
///
/// **Only a player sender has a name that can be asked for**, and even then
/// only once the query comes back. Everything else -- an auction, a creature,
/// a game object, a calendar event -- arrives as a bare entry into a table
/// this client does not have, so it is *labelled* with what is actually known
/// rather than given a name nothing answered for. Same rule as printing `$s1`
/// on a tooltip whose column was never confirmed: a visible `auction 4242`
/// says "not resolved" and a fabricated name says nothing and is believed.
///
/// A free function rather than a method because a method borrows the whole of
/// `App`, and the view this feeds is built while the renderer and the item
/// icon cache are already borrowed out of it.
fn mail_sender_label(state: &::world::WorldState, sender: ::world::MailSender) -> String {
    use ::world::MailSender as S;
    match sender {
        // Sender zero is what the server writes for a letter with no person
        // behind it -- the console sent it, or the game did. Not drawn as a
        // guid, and not left blank either.
        S::Player(0) => "the game".to_string(),
        S::Player(guid) => state
            .names
            .player(guid)
            .flatten()
            .map(str::to_string)
            .unwrap_or_else(|| format!("player {guid:#x}")),
        S::Auction(id) => format!("auction {id}"),
        S::Creature(entry) => format!("creature {entry}"),
        S::GameObject(entry) => format!("object {entry}"),
        S::Calendar(id) => format!("calendar {id}"),
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
        let mut chain = chain;
        let signin = (!args.is_self_contained()).then(|| {
            let mut signin = signin::SignIn::new();
            // The command line's data directory outranks the remembered one,
            // and is shown in the settings panel as the answer already given
            // -- rather than silently used while the panel displays something
            // else, which is a setting that lies about itself.
            if let Some(data) = args.data.clone() {
                signin.screen.settings.data = Some(data);
            }
            // The same rule for a partly-given session: `--realm-host` with no
            // `--character` is not enough to skip the screen, but it is an
            // answer to one of its questions, and making somebody retype what
            // they just passed on the command line would be absurd.
            if let Some(host) = &args.realm_host {
                signin.screen.settings.server = if args.realm_port == auth::client::DEFAULT_PORT {
                    host.clone()
                } else {
                    format!("{host}:{}", args.realm_port)
                };
            }
            if let Some(user) = &args.user {
                signin.screen.settings.account = user.clone();
            }
            if let Some(realm) = &args.realm {
                signin.screen.settings.realm = Some(realm.clone());
            }
            if let Some(character) = &args.character {
                signin.screen.settings.character = Some(character.clone());
            }
            if chain.archives().next().is_none() {
                if let Some(data) = signin.screen.settings.data.clone() {
                    match open_data(&data, &signin.screen.settings.locale) {
                        Ok(opened) => chain = opened,
                        Err(e) => signin.screen.failed(format!("{e:#}")),
                    }
                }
            }
            signin.set_names(signin::Names::load(&mut chain));
            signin
        });
        // Read at startup rather than with the spellbook: the two tables the
        // map needs are small and, unlike a spellbook, they do not depend on
        // anything the server sends -- so `M` works before the character has
        // finished logging in, and the arrow simply has nowhere to be yet.
        //
        // **All three read whatever the chain holds, including nothing.** A
        // client started with no data directory has an empty chain until the
        // sign-in screen supplies one, so these come back empty and are read
        // again by `reload_tables` when it does. Each already tolerates a file
        // that will not read -- that is what makes an install missing an
        // optional table a client that draws less rather than one that will
        // not start -- so an empty chain needs no new case.
        let maps = maps::Maps::load(&mut chain);
        let taxi_network = taxi::Network::load(&mut chain);
        // Read at startup for the same reason: 1.5MB of text naming every
        // tile's picture, which does not depend on the server and would
        // otherwise be parsed on the frame the player first looks at it.
        let minimap = minimap::Minimap::load(&mut chain);
        Self {
            signin,
            quit: false,
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
            minimized: false,
            capture_anchor: None,
            left_travel: 0.0,
            right_travel: 0.0,
            error: None,
            last_frame: Instant::now(),
            camera_z: None,
            camera_wall_distance: None,
            drawn_own_z: None,
            camera_eye_distance: None,
            // The weather's own clock. Separate from `last_frame` because that
            // one is reset every frame, and a falling drop needs a monotone
            // total rather than a delta.
            started: Instant::now(),
            frame_ms: 0.0,
            fps: 0.0,
            profile: FrameProfile::default(),
            worst: FrameProfile::default(),
            gap_events: 0,
            gap_events_ms: 0.0,
            pending_log_ms: 0.0,
            redraw_ended: None,
            gpu_line: None,
            ui_text_ms: 0.0,
            ui_hud_ms: 0.0,
            ui_stats_ms: 0.0,
            ui_snapshot_ms: 0.0,
            ui_egui_ms: 0.0,
            sound_area_ms: 0.0,
            sound_steps_ms: 0.0,
            sound_play_ms: 0.0,
            ui_markers_ms: 0.0,
            ui_bars_ms: 0.0,
            ui_panels_ms: 0.0,
            ui_map_ms: 0.0,
            profile_logged: Instant::now(),
            anim: None,
            anim_time_ms: 0,
            playing: true,
            speed: 1.0,
            live: None,
            live_move: ::world::motion::Motion::default(),
            jump: None,
            swimming: None,
            floor_material: None,
            modeled_floor: false,
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
            status_text: None,
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
            givers: ::world::Questgivers::new(),
            giver_cache_path: None,
            giver_marks_log: Vec::new(),
            giver_saved_at: Instant::now(),
            quest_marks: std::collections::HashMap::new(),
            quest_marks_asked: std::collections::HashMap::new(),
            quest_marks_log: Vec::new(),
            area: None,
            sounds: sound::Sounds::default(),
            effects: sound::Effects::default(),
            pending_sounds: Vec::new(),
            impact_delay_ms,
            attackers: std::collections::HashSet::new(),
            sound_enabled: true,
            music_enabled: true,
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
            weather_channel: sound::Channel::new(),
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
            vendor: None,
            auction: None,
            mailbox: None,
            guild_open: false,
            guild_invitation_said: None,
            flight: None,
            taxi: None,
            looting: None,
            own_corpse_query_sent: false,
            modifiers: Default::default(),
        }
    }

    /// Reopens the archives from whatever the sign-in screen currently says.
    ///
    /// **A failure empties the chain rather than leaving the old one.** The
    /// alternative is a client reading Elwynn out of the directory somebody
    /// just navigated away from while the settings panel shows a path it never
    /// opened -- a setting that lies about itself, which is the one thing a
    /// settings panel must not do.
    fn reopen_data(&mut self) {
        let Some(screen) = self.signin.as_ref().map(|s| &s.screen) else {
            return;
        };
        let (data, locale) = (screen.settings.data.clone(), screen.settings.locale.clone());
        let opened = match &data {
            Some(data) => open_data(data, &locale).map_err(|e| format!("{e:#}")),
            None => Err("no data directory chosen yet".to_string()),
        };
        match opened {
            Ok(chain) => {
                self.chain = chain;
                self.reload_tables();
                if let Some(signin) = self.signin.as_mut() {
                    signin.screen.note(format!(
                        "reading {}",
                        data.as_ref().expect("Ok implies a path").display()
                    ));
                }
            }
            Err(message) => {
                self.chain = Chain::new();
                self.reload_tables();
                if let Some(signin) = self.signin.as_mut() {
                    signin.screen.failed(message);
                }
            }
        }
    }

    /// Re-reads everything that comes off disk and depends on no session.
    ///
    /// Called at startup through [`App::new`] and again whenever the data
    /// directory changes. Each of these tolerates an archive set that holds
    /// nothing -- which is what an empty chain is -- so this is also how they
    /// are *cleared* when a directory turns out not to be an installation.
    fn reload_tables(&mut self) {
        self.maps = maps::Maps::load(&mut self.chain);
        self.taxi_network = taxi::Network::load(&mut self.chain);
        self.minimap = minimap::Minimap::load(&mut self.chain);
        let names = signin::Names::load(&mut self.chain);
        if let Some(signin) = self.signin.as_mut() {
            signin.set_names(names);
        }
    }

    /// Enters the world as the character chosen on the sign-in screen, and
    /// puts the screen away.
    ///
    /// **The screen is only dismissed once the world is standing.** Every
    /// failure below leaves it up with the reason on it, because the
    /// alternative is a black window and a line in a log file nobody has open
    /// -- and one of these failures, `world_for_live`, reads a couple of
    /// hundred megabytes of terrain and can genuinely fail on a bad install.
    fn enter_world(&mut self, entering: signin::Entering) {
        let signin::Entering {
            realm,
            connection,
            character,
        } = entering;
        let live = match live::enter(&mut self.chain, connection, &realm, &character) {
            Ok(live) => live,
            Err(e) => {
                tracing::error!("entering the world failed: {e:#}");
                if let Some(signin) = self.signin.as_mut() {
                    signin.screen.failed(format!("{e:#}"));
                }
                return;
            }
        };

        // The same three clocks `resumed` starts on the command-line path, and
        // for the same reason: they measure from the moment the connection is
        // ready to drive, not from whenever the window was created -- which on
        // this path was however long ago somebody started typing.
        self.last_heartbeat = Instant::now();
        self.last_ping = Instant::now();
        self.last_undrawable_warned = 0;
        let started = Instant::now();
        self.lighting = dbc::light::Lighting::load(|path| self.chain.read(path).ok());
        tracing::info!(
            "lighting tables loaded in {:?} ({})",
            started.elapsed(),
            if self.lighting.is_some() { "ok" } else { "unavailable" }
        );
        self.camera = live_camera(&live, &self.args);

        let Some(r) = self.renderer.as_mut() else {
            // No window means no GPU to build a world on, and there is no
            // sign-in screen without one either: unreachable, and it says so
            // rather than unwrapping.
            tracing::error!("entered the world with no renderer; nothing will be drawn");
            return;
        };
        let scene = match world_for_live(&r.gpu, &mut r.meshes, &mut self.chain, &self.args, &live)
        {
            Ok(scene) => scene,
            Err(e) => {
                tracing::error!("building the world failed: {e:#}");
                if let Some(signin) = self.signin.as_mut() {
                    signin.screen.failed(format!("{e:#}"));
                }
                return;
            }
        };
        r.meshes.prepare(&r.gpu, scene_states(&scene));
        // Sized for the largest skeleton rather than for this character's,
        // exactly as `resumed` does -- see BIND_POSE_BONES. Written to the
        // bind pose rather than left zeroed, because a zero matrix collapses
        // a model to the origin *silently*.
        let bones = r.meshes.create_bones(&r.gpu, BIND_POSE_BONES);
        r.meshes
            .update_bones(&r.gpu, &bones, &bind_pose(BIND_POSE_BONES));
        r.bones = Some(bones);
        r.world_binds = world_bind_groups(&r.gpu, &r.meshes, &scene);
        r.material_binds = material_bind_groups(&r.gpu, &r.meshes, &scene);
        r.scene = Some(scene);

        tracing::info!(
            "in the world as {} on {} ({})",
            live.character,
            live.realm,
            live.map_name
        );
        self.live = Some(live);
        // Last, and only now: this is what switches the client out of the
        // sign-in mode, and every early return above deliberately did not.
        self.signin = None;
    }

    fn reload_live_world(&mut self) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let Some(r) = self.renderer.as_mut() else {
            tracing::error!("reloaded the world with no renderer; nothing will be drawn");
            return;
        };
        let scene = match world_for_live(&r.gpu, &mut r.meshes, &mut self.chain, &self.args, live) {
            Ok(scene) => scene,
            Err(e) => {
                tracing::error!("building the transferred world failed: {e:#}");
                r.scene = None;
                return;
            }
        };
        r.meshes.prepare(&r.gpu, scene_states(&scene));
        let bones = r.meshes.create_bones(&r.gpu, BIND_POSE_BONES);
        r.meshes
            .update_bones(&r.gpu, &bones, &bind_pose(BIND_POSE_BONES));
        r.bones = Some(bones);
        r.world_binds = world_bind_groups(&r.gpu, &r.meshes, &scene);
        r.material_binds = material_bind_groups(&r.gpu, &r.meshes, &scene);
        r.scene = Some(scene);
        self.camera = live_camera(live, &self.args);
        tracing::info!("loaded transferred world {} ({})", live.map_id, live.map_name);
    }

    /// Draws the sign-in screen, and does whatever it asked for.
    ///
    /// Its own frame rather than a branch inside `redraw`'s: there is no
    /// scene, no camera, no world to stream and no HUD to build, so what this
    /// shares with the ordinary path is the surface and the egui pass and
    /// nothing else.
    fn draw_sign_in(&mut self, window: &Arc<Window>) {
        // Taken out of `self` for the closure below, and put back unless the
        // world was entered. A screen that draws itself while borrowing the
        // renderer is a borrow this is not worth fighting.
        let Some(mut signin) = self.signin.take() else {
            return;
        };
        let style = self.hud.profile.style;
        let Some(r) = self.renderer.as_mut() else {
            self.signin = Some(signin);
            return;
        };
        let input = r.egui_state.take_egui_input(window);
        let ctx = r.egui_ctx.clone();
        let mut outcome = signin::Outcome::Continue;
        let output = ctx.run_ui(input, |ctx| {
            outcome = signin.update(ctx, &style);
        });
        r.egui_state
            .handle_platform_output(window, output.platform_output.clone());
        self.signin = Some(signin);

        self.paint_sign_in(output);

        match outcome {
            signin::Outcome::Continue => {}
            signin::Outcome::Quit => self.quit = true,
            signin::Outcome::Theme(theme) => {
                // **The whole style is written and saved.** A theme here is a
                // way of filling the layout file in, not a second set of
                // defaults sitting under it -- see `ui::theme`.
                self.hud.profile.style = theme.style();
                self.minimap_range = self.hud.profile.style.minimap_range;
                self.hud.save();
                tracing::info!("theme set to {}", theme.name());
            }
            signin::Outcome::DataChanged => self.reopen_data(),
            signin::Outcome::Enter(entering) => self.enter_world(*entering),
        }
    }

    /// Clears the window and draws one egui pass over it.
    ///
    /// **The clear is the point.** The ordinary path leaves the colour target
    /// written by the scene and egui loads it; with no scene nothing writes it
    /// at all, and `LoadOp::Load` over an unwritten surface is whatever the
    /// driver last had there -- which is to say a panel floating over
    /// garbage, intermittently, on some machines and not others.
    fn paint_sign_in(&mut self, mut output: egui::FullOutput) {
        let Some(r) = self.renderer.as_mut() else {
            output.textures_delta.clear();
            return;
        };
        use wgpu::CurrentSurfaceTexture as Acquired;
        let (frame, reconfigure) = match r.surface.get_current_texture() {
            Acquired::Success(frame) => (frame, false),
            Acquired::Suboptimal(frame) => (frame, true),
            Acquired::Lost | Acquired::Outdated => {
                r.surface.configure(&r.gpu.device, &r.config);
                output.textures_delta.clear();
                return;
            }
            Acquired::Timeout | Acquired::Occluded => {
                output.textures_delta.clear();
                return;
            }
            Acquired::Validation => {
                tracing::error!("surface validation error while acquiring a frame");
                output.textures_delta.clear();
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
                label: Some("sign-in"),
            });

        let clipped = r.egui_ctx.tessellate(output.shapes, output.pixels_per_point);
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                r.egui_renderer
                    .update_texture(&r.gpu.device, &r.gpu.queue, *id, delta);
            }
        }
        let desc = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [r.config.width, r.config.height],
            pixels_per_point: output.pixels_per_point,
        };
        r.egui_renderer
            .update_buffers(&r.gpu.device, &r.gpu.queue, &mut encoder, &clipped, &desc);
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sign-in"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Near-black rather than the panel's own colour: the
                        // panel is a lit thing on a dark screen, and a
                        // backdrop matching it would leave it with no edge.
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.012,
                            g: 0.012,
                            b: 0.018,
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
            r.egui_renderer
                .render(&mut pass.forget_lifetime(), &clipped, &desc);
        }
        for id in &output.textures_delta.free {
            r.egui_renderer.free_texture(id);
        }
        r.gpu.queue.submit([encoder.finish()]);
        r.gpu.queue.present(frame);
        output.textures_delta.clear();
        if reconfigure {
            r.surface.configure(&r.gpu.device, &r.config);
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
            .with_window_icon(icon::window_icon())
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
            // **Whatever the surface happens to list first**, which is a
            // choice nobody made and nobody could see. Printed below,
            // because "the client is slow" and "the client is waiting for
            // the monitor" are the same 6 ms of gap from the outside and
            // want opposite responses.
            present_mode: chosen_present_mode(&caps, self.args.present_mode.as_deref()),
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            color_space: wgpu::SurfaceColorSpace::Auto,
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&gpu.device, &config);
        tracing::info!(
            "surface {:?} at {}x{}, present {:?} (available {:?}), max frame latency {}",
            format,
            config.width,
            config.height,
            config.present_mode,
            caps.present_modes,
            config.desired_maximum_frame_latency,
        );

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
        // **The shadow map is built from the mesh renderer's own layouts and
        // then handed back to it.** The two need each other: the shadow pass
        // binds a model's existing pose and texture, and the visible pass
        // reads the map the shadow pass wrote. Neither can be constructed
        // holding the other, so the mesh renderer starts with a 1x1
        // placeholder map and is pointed at the real one here.
        let shadow = (!self.args.no_shadows).then(|| {
            render::ShadowMap::new(
                &gpu,
                self.args.shadow_size,
                meshes.bone_layout(),
                meshes.material_layout(),
            )
        });
        if let Some(map) = &shadow {
            meshes.attach_shadow_map(&gpu, map.view());
        }
        let terrain_renderer = TerrainRenderer::new(&gpu, format, meshes.camera_layout());
        let sky = render::SkyRenderer::new(&gpu, format);
        let mut celestial = render::CelestialRenderer::new(&gpu, format);
        let precipitation = render::PrecipitationRenderer::new(&gpu, format);
        let particles = render::ParticleRenderer::new(&gpu, format);
        let emitters = emitters::Emitters::new();
        let liquid_renderer = render::LiquidRenderer::new(&gpu, format);
        let mut liquid_types = liquid::LiquidTypes::default();
        let depth = DepthBuffer::new(&gpu, config.width, config.height);

        // **Nothing is built while the sign-in screen is up**, and that is the
        // whole reason the scene is an `Option` on this path rather than a
        // failure: there is no data directory to read, no realm to connect to
        // and nothing to draw until somebody has said. What replaces it is one
        // clear pass and one panel -- see `redraw`.
        let scene = match self.signin {
            Some(_) => None,
            None => match build_scene(
                &gpu,
                &terrain_renderer,
                &liquid_renderer,
                &mut liquid_types,
                &mut meshes,
                &mut self.chain,
                &self.args,
            ) {
                Ok((scene, live)) => {
                    self.offline_map =
                        offline_map_id(&mut self.chain, self.args.map.as_deref());
                    if live.is_some() || (self.offline_map.is_some() && self.args.hour.is_some())
                    {
                        // Start the movement and keepalive clocks from the
                        // moment the connection is actually ready to drive,
                        // not from whenever the window happened to be created.
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
            },
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
        // After the lighting tables, because the star dome is a row of
        // `LightSkybox` and is found through them rather than by a path
        // written here.
        let sky_scene = sky::SkyScene::load(
            &gpu,
            &meshes,
            &mut self.chain,
            &mut celestial,
            self.lighting.as_ref(),
        );
        tracing::info!("{}", sky_scene.describe());

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
            celestial,
            sky_scene,
            shadow,
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
            // Counted with the window events: from the frame's point of view
            // this is the same thing -- work done in the gap, at the mouse's
            // rate rather than the frame's. Steering warps the pointer back
            // to where it was pressed, and a warp is itself motion, so this
            // is exactly the path that could feed itself.
            let started = Instant::now();
            self.device_motion(delta.0, delta.1);
            self.gap_events_ms += started.elapsed().as_secs_f32() * 1000.0;
            self.gap_events += 1;
        }
    }

    /// Times every event that is not the redraw itself, and hands the rest
    /// to [`App::handle_window_event`].
    ///
    /// **The wrapper exists because `outside redraw` is a bucket and buckets
    /// hide things.** `frame_ms` runs start-to-start, so it covers the gap
    /// between one redraw ending and the next beginning as well as the redraw
    /// -- and that gap held 26 to 36 ms in six of one session's sixty-two
    /// seconds, with a perfectly ordinary 8-13 ms redraw underneath it. Two
    /// completely different things live in that gap: input this client chose
    /// to process, and time the operating system simply did not give it back.
    /// The first is ours to fix and the second is not, and a single number
    /// cannot say which. Same reason the frame's own phases were split out of
    /// `other` rather than attributed to the likeliest suspect.
    ///
    /// A wrapper rather than a timer threaded through the body: that body has
    /// half a dozen early returns, and a measurement that misses the paths
    /// somebody forgets is worse than none -- it reads as a cheap event.
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: WindowId,
        event: WindowEvent,
    ) {
        // The redraw is measured from the inside, by `redraw` itself, and
        // must not also be counted here.
        if matches!(event, WindowEvent::RedrawRequested) {
            self.handle_window_event(event_loop, id, event);
            self.redraw_ended = Some(Instant::now());
            return;
        }
        let started = Instant::now();
        self.handle_window_event(event_loop, id, event);
        self.gap_events_ms += started.elapsed().as_secs_f32() * 1000.0;
        self.gap_events += 1;
    }

}

impl App {
    fn handle_window_event(
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

        // **The sign-in screen owns the keyboard and the mouse outright**, the
        // same way a chat line being typed does -- and for a stronger reason
        // than tidiness. egui reports a key as consumed only when one of *its*
        // widgets has focus, and this panel reads the event queue itself, so
        // every key it handles arrives here unconsumed as well. Without this
        // return, typing an account name walks a character that does not
        // exist, and `F1` opens the layout editor over a screen with no
        // layout in it.
        //
        // Redraw and close still get through, because those are the window's
        // business rather than the world's.
        if self.signin.is_some()
            && !matches!(
                event,
                WindowEvent::RedrawRequested
                    | WindowEvent::CloseRequested
                    | WindowEvent::Resized(_)
            )
        {
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
                self.save_giver_cache();
                event_loop.exit()
            }
            WindowEvent::Resized(size) => {
                // A zero-area size is Windows' report of a minimized window,
                // not a real target to render into -- see `App::minimized`.
                self.minimized = size.width == 0 || size.height == 0;
                if !self.minimized {
                    r.config.width = size.width.max(1);
                    r.config.height = size.height.max(1);
                    r.surface.configure(&r.gpu.device, &r.config);
                    r.depth.resize(&r.gpu, r.config.width, r.config.height);
                }
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
                        if self.modifiers.control_key() {
                            match code {
                                KeyCode::KeyS => {
                                    self.toggle_sound();
                                    window.request_redraw();
                                    return;
                                }
                                KeyCode::KeyM => {
                                    self.toggle_music();
                                    window.request_redraw();
                                    return;
                                }
                                _ => {}
                            }
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
                            // `T` asks the target to trade. The original
                            // client puts this on a right-click menu, which
                            // this interface does not have -- right-click in
                            // the world already means "select and attack",
                            // and overloading it with a menu would be a
                            // gesture that sometimes swings and sometimes
                            // does not.
                            KeyCode::KeyT => {
                                self.initiate_trade();
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
                            // `G` for the guild roster. 3.3.5a puts it
                            // behind the social frame's tabs; there is no
                            // social frame here, so it gets the letter.
                            //
                            // Opening **asks**, rather than drawing whatever
                            // was last said: a roster is a list of people who
                            // are not in the world, so nothing else in this
                            // client would ever notice it going stale.
                            KeyCode::KeyG => {
                                self.guild_open = !self.guild_open;
                                if self.guild_open {
                                    self.ask_guild();
                                }
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
                // The sign-in screen's Quit button, honoured here because
                // this is where the event loop is in scope. `redraw` itself
                // takes no `ActiveEventLoop`, and threading one through it
                // for one button would put an exit path in every frame.
                if self.quit {
                    event_loop.exit();
                    return;
                }
                window.request_redraw();
            }
            _ => {}
        }
    }

    fn redraw(&mut self, window: &Arc<Window>) {
        let now = Instant::now();
        self.frame_ms = now.duration_since(self.last_frame).as_secs_f32() * 1000.0;
        self.last_frame = now;
        if self.frame_ms > 0.0 {
            let instant_fps = 1000.0 / self.frame_ms;
            self.fps = if self.fps == 0.0 {
                instant_fps
            } else {
                self.fps + (instant_fps - self.fps) * 0.1
            };
        }

        // **Its own frame, and it returns.** Nothing below here has anything
        // to draw before a character has been chosen: no scene, no camera to
        // move, no connection to pump and no HUD to build.
        if self.signin.is_some() {
            self.draw_sign_in(window);
            return;
        }

        // **Reset here, before anything can add to it**, and after the
        // sign-in return above: a profile carried over from a frame that
        // returned early would be attributed to this one.
        // **Drained here, not at the end of the frame.** These accumulated
        // during the gap `frame_ms` has just measured, so they belong to this
        // reading; left until the end they would be attributed to a gap that
        // has not happened yet.
        let mut profile = FrameProfile {
            gap_ms: self
                .redraw_ended
                .map_or(0.0, |end| end.elapsed().as_secs_f32() * 1000.0),
            gap_events: std::mem::take(&mut self.gap_events),
            gap_events_ms: std::mem::take(&mut self.gap_events_ms),
            log_ms: std::mem::take(&mut self.pending_log_ms),
            ..FrameProfile::default()
        };
        let redraw_started = Instant::now();
        let phase = Instant::now();
        let mut ui_output = self.build_ui(window);
        profile.ui_ms = phase.elapsed().as_secs_f32() * 1000.0;
        profile.ui_text_ms = self.ui_text_ms;
        profile.ui_hud_ms = self.ui_hud_ms;
        profile.ui_stats_ms = self.ui_stats_ms;
        profile.ui_snapshot_ms = self.ui_snapshot_ms;
        profile.ui_egui_ms = self.ui_egui_ms;
        profile.ui_markers_ms = self.ui_markers_ms;
        profile.ui_bars_ms = self.ui_bars_ms;
        profile.ui_panels_ms = self.ui_panels_ms;
        profile.ui_map_ms = self.ui_map_ms;
        let camera = self.camera;

        // Movement integrates real elapsed time, so travel speed does not
        // depend on frame rate. A live world is driven by the character's
        // walk, not a free-flying camera -- see `drive_live_movement`.
        if self.live.is_some() {
            let phase = Instant::now();
            self.drive_live_movement();
            profile.movement_ms = phase.elapsed().as_secs_f32() * 1000.0;
            // **Unconditionally, and after movement rather than inside it.**
            // A taxi flight returns early from `drive_live_movement`, and
            // while the camera placement lived in that function's tail the
            // view stayed where the character took off from -- watching an
            // empty field while the minimap tracked them across the map.
            let dt = (self.frame_ms / 1000.0).max(0.0);
            let phase = Instant::now();
            self.follow_camera_to_character(dt);
            profile.camera_ms = phase.elapsed().as_secs_f32() * 1000.0;
            let phase = Instant::now();
            self.pump_live_connection();
            // After the pump, because the spellbook it needs arrives through
            // it, and because both want the archive chain. Timed with the
            // pump: both are "what the connection cost this frame".
            self.load_spell_data();
            profile.network_ms = phase.elapsed().as_secs_f32() * 1000.0;
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
        let phase = Instant::now();
        self.update_sound();
        profile.sound_ms = phase.elapsed().as_secs_f32() * 1000.0;
        profile.sound_area_ms = self.sound_area_ms;
        profile.sound_steps_ms = self.sound_steps_ms;
        profile.sound_play_ms = self.sound_play_ms;

        let Some(r) = self.renderer.as_mut() else {
            ui_output.textures_delta.clear();
            return;
        };

        // A minimized window has no surface worth acquiring -- movement,
        // the network pump and sound above have already run, so the world
        // does not desync while minimized; only the GPU work below, which
        // nothing could see anyway, is skipped. See `App::minimized`.
        if self.minimized {
            ui_output.textures_delta.clear();
            return;
        }

        // Stream before drawing, so newly admitted tiles appear this frame.
        if let Some(Scene::Streaming(world)) = r.scene.as_mut() {
            let eye = match camera {
                Camera::Fly(f) => f.position,
                Camera::Orbit(o) => o.eye(),
            };
            let phase = Instant::now();
            world.update(
                &r.gpu,
                &mut r.meshes,
                &r.terrain_renderer,
                &r.liquid_renderer,
                &mut self.chain,
                eye,
            );
            profile.stream_ms = phase.elapsed().as_secs_f32() * 1000.0;
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
            let phase = Instant::now();
            if self.args.entities {
                if let Some(live) = self.live.as_mut() {
                    // The keys, not the wire: the server never relays our own
                    // movement back to us. Held means running -- there is no
                    // walk toggle here, and `LIVE_RUN_SPEED` is the run speed.
                    // F2. See `App::entity_flip`.
                    let flip = if self.entity_flip { std::f32::consts::PI } else { 0.0 };
                    // **Cosmetic only** -- see `App::drawn_own_z`. `live.position.z`
                    // itself is left untouched for collision, the camera's
                    // focus point and outgoing movement, exactly the split
                    // `camera_z` already makes for the camera's own follow
                    // height.
                    let own_z = Self::ease_towards(
                        &mut self.drawn_own_z,
                        live.position.z,
                        self.frame_ms / 1000.0,
                        0.09,
                        3.0,
                    );
                    let own_position = glam::Vec3::new(live.position.x, live.position.y, own_z);
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
                        own_position,
                        pace,
                        lean,
                        self.jump.is_some(),
                        self.swimming.is_some(),
                    );
                    // See `App::own_body_drawn`: submitted-and-not-drawn and
                    // never-submitted are the same report from the window.
                    let own_drawn = drawn.iter().any(|entity| entity.guid == live.guid);
                    // **Measured before this, not after.** `own_drawn` and
                    // `App::own_body_drawn`'s diagnostic ask whether the body
                    // was *available* to draw -- a real disappearance, not a
                    // choice made about one that is fine. Removing it here
                    // keeps that question and this one from being conflated
                    // into a warning about a body that never went missing.
                    if self
                        .camera_eye_distance
                        .is_some_and(|distance| distance < HIDE_OWN_MODEL_DISTANCE)
                    {
                        drawn.retain(|entity| entity.guid != live.guid);
                    }
                    live.ease_facings(&mut drawn, self.frame_ms / 1000.0);
                    // **A crowd, on demand.** See `Args::stress`.
                    stress_crowd(&mut drawn, self.args.stress);
                    let placements: Vec<crate::world::EntityPlacement> =
                        drawn
                            .iter()
                            .map(|entity| {
                                let (look, look_key) =
                                    entity_look(looks, chain, items, live, entity);
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
                                    stealthed: entity.stealthed,
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
            profile.entities_ms = phase.elapsed().as_secs_f32() * 1000.0;
            let phase = Instant::now();
            // **Built once for both passes and for the same reason the
            // frustum is built once inside the draw**: two derivations of
            // "what is worth working on" agree only until somebody edits one,
            // and the failure is a creature that animates but does not draw,
            // or draws without animating. The radius is the shadow box's, so
            // anything that can still cast is still posed.
            let attention = render::cull::Attention::new(
                camera.view_proj(r.config.width as f32 / r.config.height.max(1) as f32),
                camera.eye(),
                self.args.shadow_radius,
            );
            world.update_animations(&r.gpu, &r.meshes, &attention);
            profile.animations_ms = phase.elapsed().as_secs_f32() * 1000.0;
            let phase = Instant::now();
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
                &attention,
            );
            profile.emitters_ms = phase.elapsed().as_secs_f32() * 1000.0;
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
        // **Timed on its own.** With a present mode that waits, this is where
        // a GPU-bound frame blocks -- and folded into the encode below it
        // reads as expensive submission, which sends the reader to cull
        // geometry that was never the cost.
        let phase = Instant::now();
        let acquired = r.surface.get_current_texture();
        profile.acquire_ms = phase.elapsed().as_secs_f32() * 1000.0;
        let (frame, reconfigure) = match acquired {
            Acquired::Success(frame) => (frame, false),
            // Suboptimal still yields a usable frame. Present it before
            // reconfiguring: wgpu forbids configuring an acquired surface.
            Acquired::Suboptimal(frame) => (frame, true),
            // Routine on resize and display changes: reconfigure, skip a frame.
            Acquired::Lost | Acquired::Outdated => {
                r.surface.configure(&r.gpu.device, &r.config);
                ui_output.textures_delta.clear();
                return;
            }
            Acquired::Timeout | Acquired::Occluded => {
                ui_output.textures_delta.clear();
                return;
            }
            Acquired::Validation => {
                tracing::error!("surface validation error while acquiring a frame");
                ui_output.textures_delta.clear();
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let phase = Instant::now();
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
        // Before the pass, because loading a backdrop builds pipelines and
        // bind groups and a render pass has already taken the encoder. On
        // Azeroth and Kalimdor this is always id 0 and does nothing at all --
        // no outdoor light on either continent names a skybox.
        r.sky_scene.set_skybox(
            &r.gpu,
            &r.meshes,
            &mut self.chain,
            &mut r.celestial,
            self.lighting.as_ref(),
            lighting.map_or(0, |(sample, _)| sample.skybox_id),
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
                precipitation_for_frame(
                    weather,
                    self.started.elapsed().as_secs_f32(),
                    self.modeled_floor,
                ),
                &r.material_binds,
                r.bones.as_ref(),
                &r.world_binds,
                &r.identity,
                lighting,
                self.started.elapsed().as_secs_f32(),
                Some(&Atmosphere {
                    celestial: &r.celestial,
                    sky: &r.sky_scene,
                    shadow: r.shadow.as_ref(),
                    radius: self.args.shadow_radius,
                    strength: SHADOW_STRENGTH,
                }),
                &mut profile,
                !self.args.no_cull,
            );
        }

        // **Everything from here to the end of the pass is the interface**,
        // and none of it exists in the headless path -- which encodes and
        // submits the *whole world* in 0.73 ms for Northshire and 1.93 ms for
        // Ironforge, against 5.51 ms of `encode` plus `submit` live. The world
        // is not the difference. Tessellation, egui's own buffer uploads and
        // its render pass all sat inside `encode` and `submit` with nothing
        // naming them, which is exactly how the minimap's index scan hid
        // inside `ui` for six rounds.
        let interface_started = Instant::now();
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
        profile.interface_ms = interface_started.elapsed().as_secs_f32() * 1000.0;

        profile.encode_ms = phase.elapsed().as_secs_f32() * 1000.0;
        let phase = Instant::now();
        r.gpu.queue.submit([encoder.finish()]);
        profile.submit_ms = phase.elapsed().as_secs_f32() * 1000.0;
        let phase = Instant::now();
        r.gpu.queue.present(frame);
        profile.present_ms = phase.elapsed().as_secs_f32() * 1000.0;
        ui_output.textures_delta.clear();
        if reconfigure {
            r.surface.configure(&r.gpu.device, &r.config);
        }

        // **Read after the draw, because the draw is not the only caller.**
        // The camera, the character's own footing and the footstep lookup all
        // query the same grids earlier in this frame, and a read taken before
        // the pass would attribute their work to the next one.
        if let Some(Scene::Streaming(world)) = r.scene.as_ref() {
            profile.collision = world.collision_probe();
            (profile.buffers_reused, profile.buffers_created) =
                world.instance_pool_counts();
            (profile.skeletons, profile.entity_groups) = world.entity_load();
        }
        // **Read here, after the draw and before the submit that pays for
        // them.** Taken any earlier and the frame's own writes would be
        // charged to the next one.
        (profile.write_calls, profile.write_bytes) = r.gpu.take_writes();
        (profile.clip_reads, profile.clip_plays) = self.effects.clip_reads();
        // Music, ambience and weather together: three channels, one number,
        // because the question is "is anything still re-reading the
        // archive" rather than which of the three it was.
        let (music_reads, music_starts) = self.music.track_reads();
        let (ambience_reads, ambience_starts) = self.ambience.track_reads();
        let (weather_reads, weather_starts) = self.weather_channel.track_reads();
        profile.track_reads = music_reads + ambience_reads + weather_reads;
        profile.track_starts = music_starts + ambience_starts + weather_starts;
        profile.live_systems = r.emitters.live_systems;
        profile.live_sprites = r.emitters.live_sprites;
        profile.live_ribbons = r.emitters.live_ribbons;
        profile.redraw_ms = redraw_started.elapsed().as_secs_f32() * 1000.0;
        self.profile = profile;

        // **The slowest frame of the second, not the latest one.** See
        // `FrameProfile::frames`: a once-a-second snapshot of a stutter is a
        // lottery, and the first version of this reported 79-103 fps for a
        // session whose complaint was that moving indoors was slow.
        // **This frame's own period**: the gap before it plus its own redraw.
        // Not `frame_ms`, which runs start-to-start and therefore describes
        // the *previous* frame -- pairing it with this frame's phases is the
        // mistake `FrameProfile::gap_ms` documents at length.
        let period = profile.gap_ms + profile.redraw_ms;
        self.worst.frames += 1;
        if period > self.worst.worst_ms {
            let frames = self.worst.frames;
            let best = self.worst.best_ms;
            self.worst = profile;
            self.worst.frames = frames;
            self.worst.best_ms = best;
            self.worst.worst_ms = period;
        }
        if period < self.worst.best_ms || self.worst.best_ms == 0.0 {
            self.worst.best_ms = period;
        }

        // Once a second rather than per frame: at sixty frames a second this
        // line would *be* the log, and the thing it is meant to make findable
        // -- a zone where the frame rate collapses -- lasts far longer than
        // one frame. Same reason the undrawable-model warning speaks on
        // change rather than on every rebuild.
        if self.profile_logged.elapsed() >= Duration::from_secs(1) {
            self.profile_logged = Instant::now();
            let emitting = Instant::now();
            tracing::info!(
                "worst frame {:.1} ms ({:.0} fps avg): {}",
                self.worst.worst_ms,
                self.fps,
                self.worst.describe()
            );
            self.pending_log_ms = emitting.elapsed().as_secs_f32() * 1000.0;
            self.worst = FrameProfile::default();
        }
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
        Self::ease_towards(state, target, dt, TAU, SNAP)
    }

    /// A fraction of the remaining error per second, not a maximum rate --
    /// see [`Self::camera_follow_z`], which this generalises. Extracted once
    /// a second caller needed the identical shape with different constants:
    /// see [`Self::camera_follow_wall_distance`].
    fn ease_towards(state: &mut Option<f32>, target: f32, dt: f32, tau: f32, snap: f32) -> f32 {
        let smoothed = match *state {
            Some(v) if (target - v).abs() <= snap && dt > 0.0 => {
                v + (target - v) * (1.0 - (-dt / tau).exp())
            }
            _ => target,
        };
        *state = Some(smoothed);
        smoothed
    }

    /// How far the wall/ceiling-avoiding eye orbits, easing towards whatever
    /// [`tightest_eye_at_center`] asks for.
    ///
    /// **The same shake `camera_follow_z` was written for, one layer up.**
    /// `first_obstruction` answers with whichever triangle is nearest, and a
    /// position shifting by a fraction of a unit near a mesh seam can flip
    /// that answer, or flip a hit between the duck and pull-in branches --
    /// each one perfectly correct on its own frame, and rigidly followed, a
    /// visible judder as the character merely turns. Reported live as the
    /// camera feeling "stuck" once the worst of the escape itself was fixed.
    /// `SNAP` is larger than `camera_follow_z`'s: walking through a doorway
    /// from a cramped stairwell into a real hall is a legitimate double-digit
    /// jump in one step, and easing that over a tenth of a second would read
    /// as the camera lagging rather than as attached.
    fn camera_follow_wall_distance(state: &mut Option<f32>, target: f32, dt: f32) -> f32 {
        const TAU: f32 = 0.09;
        const SNAP: f32 = 5.0;
        Self::ease_towards(state, target, dt, TAU, SNAP)
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
                    let slid =
                        world.slide(live.position, wanted, BODY_RADIUS, BODY_HEIGHT, STEP_HEIGHT);
                    // **Previously silent.** A full block and a slow climb
                    // look identical in the `stand` log above -- that one
                    // only fires when the standing height *changes*, and a
                    // character pushed flat against an un-climbable riser
                    // never gets that far. Logged whenever `slide` granted
                    // less than half of what was asked, which a normal walk
                    // against open air or a gentle slope never triggers.
                    let intended = (wanted - live.position).truncate().length();
                    let achieved = (slid - live.position).truncate().length();
                    // **`enabled!` first, and the world queries only after
                    // it.** Everything inside this block is *argument*
                    // evaluation -- five ray casts and two floor lookups,
                    // each fanned across every resident tile -- so `debug!`
                    // discarding the line does not discard the work that
                    // built it. And the condition is not the rare case it
                    // reads as: `slide` granting less than half is what every
                    // frame walking up a staircase or along a wall looks
                    // like, which is precisely the situation being reported
                    // as slow. Sibling of the camera's own three lines, which
                    // were moved to `trace` for flooding a log near a
                    // stairwell -- that fixed the volume; this is the half
                    // where the cost is in the arguments and a level change
                    // would not have touched it.
                    if intended > 1e-4
                        && achieved < intended * 0.5
                        && tracing::enabled!(tracing::Level::DEBUG)
                    {
                        // Horizontal probes at a few heights, the same way
                        // `crosses_wall` samples -- a vertical probe at
                        // `wanted` finds the ordinary floor first (its normal
                        // is near `(0,0,1)`, not a wall's), which is exactly
                        // what the first attempt at this measured. Extended
                        // a full unit past `wanted`, in the direction of
                        // travel, so a wall only fractions of a unit ahead is
                        // actually crossed rather than just grazed.
                        let direction = (wanted - live.position).truncate().normalize_or_zero();
                        let reach = live.position.truncate() + direction;
                        let heights: Vec<(f32, Option<f32>)> = [
                            STEP_HEIGHT + 0.05,
                            STEP_HEIGHT + 0.3,
                            STEP_HEIGHT + 0.6,
                            STEP_HEIGHT + 0.9,
                            BODY_HEIGHT * 0.9,
                        ]
                        .into_iter()
                        .map(|offset| {
                            let from = glam::Vec3::new(
                                live.position.x,
                                live.position.y,
                                live.position.z + offset,
                            );
                            let to = glam::Vec3::new(reach.x, reach.y, live.position.z + offset);
                            (offset, world.first_obstruction(from, to).map(|(t, _)| t))
                        })
                        .collect();
                        // What `floor_under_footing` itself says is reachable
                        // from here and from `wanted` -- the character's feet
                        // stayed pinned at the base the whole time this was
                        // captured, and this says whether that is because no
                        // floor was found at all, or because one was found
                        // and something downstream never applied it.
                        let floor_here =
                            world.floor_under_footing(live.position, STEP_HEIGHT);
                        let floor_wanted =
                            world.floor_under_footing(wanted, STEP_HEIGHT);
                        tracing::debug!(
                            "slide refused: {:.2} of {:.2} requested, from {:.2},{:.2},{:.2} \
                             towards {:.2},{:.2},{:.2} -> {:.2},{:.2},{:.2}, crossed at \
                             heights (offset, hit-fraction) {:?}, floor_under_footing here \
                             {:?} at wanted {:?} (radius {BODY_RADIUS})",
                            achieved, intended,
                            live.position.x, live.position.y, live.position.z,
                            wanted.x, wanted.y, wanted.z,
                            slid.x, slid.y, slid.z,
                            heights,
                            floor_here,
                            floor_wanted,
                        );
                    }
                    slid
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
            // **The floor outranks the terrain whenever it answered at all --
            // not merely when it happens to be the taller of the two.** This
            // was `ground.max(floor)`, on the reasoning that "a floor laid
            // over ground holds the character up", which is true of a
            // building -- its floor sits *above* the ground it is built on --
            // and false of a cave: a cave's walkable floor sits *below* the
            // surface directly overhead, so `max` picked the surface every
            // time, and a character walking deeper underground was carried
            // back up to daylight the moment the real cave floor read lower
            // than the terrain above it. Reported from live play as a
            // "teleport" out of the Northshire cave.
            //
            // `floor_under_footing` has already done the one check that
            // matters -- whether the collision mesh answers for this spot at
            // all, bounded to a step above where the character already is,
            // so a roof or an upper floor is never returned as `floor` in
            // the first place. Once it has answered, that answer is what the
            // character is standing on; the terrain height field is the
            // fallback for everywhere the collision mesh has nothing to say,
            // indoors or out.
            let stand = floor.or(ground);
            // **Read off the same preference**, not asked again. The
            // character is on the collision mesh's floor exactly when
            // `underfoot` answered, so deriving the surface from a second
            // test would be two answers to one question -- and the frame
            // they disagree on is a footstep that sounds like the ground
            // under the floorboards.
            self.floor_material = underfoot.and_then(|(_, surface)| surface);
            self.modeled_floor = underfoot.is_some();
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
                // **A large vertical relocation is refused.**
                //
                // A building's collision is filed under the single tile
                // holding its origin, and tiles are admitted a few per frame.
                // So there is a window at login where the character's own
                // tile is resident and answering with terrain height, while
                // the tile carrying the floor under their feet has not
                // arrived yet. An interior floor can be far below or above
                // that incomplete terrain answer, and one frame in that
                // window can move the character to the wrong surface.
                //
                // And the drop is **permanent**, which is what makes refusing
                // it worth the special case rather than letting it settle. A
                // floor search only considers surfaces at or below where it
                // starts, so the floor that streams in a frame later is now
                // above the character and will never be found. They are under
                // the city for good.
                //
                // Deliberately narrow. It refuses only a large vertical move,
                // so walking across ordinary slopes and stairs remains
                // unchanged while a terrain fallback cannot relocate a
                // character onto an unrelated surface.
                let streaming = match self.renderer.as_ref().and_then(|r| r.scene.as_ref()) {
                    Some(Scene::Streaming(world)) => world.still_streaming(),
                    _ => false,
                };
                let delta = z - live.position.z;
                if delta.abs() <= MAX_GROUND_SNAP {
                    live.position.z = z;
                } else {
                    tracing::debug!(
                        "ground snap refused: current {:.3}, candidate {:.3}, delta {:.3}, \
                         tiles streaming {}",
                        live.position.z,
                        z,
                        delta,
                        streaming,
                    );
                }
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
    }

    /// Puts the camera behind the character.
    ///
    /// **Its own method because it is not movement**, and having it live in
    /// the tail of `drive_live_movement` cost a real bug: a taxi flight
    /// returns early from that function -- deliberately, so no input,
    /// collision or ground assignment runs -- and took the camera placement
    /// with it. The character took off, the minimap tracked them across the
    /// map, and the view stayed in Westfall watching an empty field. The
    /// streaming centre follows the camera, so the world stopped loading with
    /// it.
    ///
    /// The lesson is the early return's, not the flight's: a wholesale
    /// replacement is the right shape for the position writes *because* they
    /// must all be skipped together, and it is the wrong shape for anything
    /// that merely happened to share the function.
    fn follow_camera_to_character(&mut self, dt: f32) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let (position, orientation) = (live.position, live.orientation);
        let follow_z = Self::camera_follow_z(&mut self.camera_z, position.z, dt);
        let camera_at = glam::Vec3::new(position.x, position.y, follow_z);
        // Kept off the terrain. Sampled here, while the scene is still
        // borrowed, because the camera is behind `&mut self` and the world
        // behind the renderer -- and the alternative, cloning a height field
        // per frame, would be absurd for twelve lookups.
        let focus = camera_at + glam::Vec3::Z * FOLLOW_HEIGHT;
        // Read once, before the orbit itself is even built: see
        // `CAMERA_INDOOR_DISTANCE_CAP` for why the *distance* the wall/ceiling
        // ray gets asked to check has to be bounded before that ray is cast,
        // not corrected after the fact.
        let standing_on = match self.renderer.as_ref().and_then(|r| r.scene.as_ref()) {
            Some(Scene::Streaming(world)) => world.floor_under_footing(focus, CAMERA_FLOOR_STEP),
            _ => None,
        };
        let distance = indoor_capped_distance(self.camera_distance, standing_on.is_some());
        let center_yaw = orientation + self.camera_yaw_offset;
        let pitch = self.camera_pitch;
        // A plain local, not `&mut self.camera_wall_distance` -- the eye
        // closures below borrow `self.renderer` for the whole match, and the
        // borrow checker cannot see the two fields are disjoint through a
        // method call any better here than it could for `camera_follow_z`.
        // Written back to the real field once the match, and the borrow it
        // holds, are both done.
        let mut wall_distance_state = self.camera_wall_distance;
        let eye = match self.renderer.as_ref().and_then(|r| r.scene.as_ref()) {
            Some(Scene::Streaming(world)) => {
                // **Whether the raw terrain field is worth asking at all this
                // frame, decided once rather than per sample.** A cave is a
                // hollow shell -- the solid rock it is carved into carries no
                // collision geometry of its own, because nothing needs it --
                // so a sample that steps past the tunnel's modelled walls
                // finds no floor there and, unchallenged, falls back to the
                // terrain field, which answers for the hillside overhead
                // instead. That is exactly the shape of the report that came
                // back after the first fix: pitched down, the desired eye
                // stays close over the character and inside the tunnel;
                // levelled or looking up, the orbit swings it out behind the
                // character and past the modelled shell. The tell is the same
                // one `foss-wow#135` used for the character's own feet --
                // terrain sitting well *above* the floor, the opposite of
                // what a building's elevated floor looks like -- checked once
                // at the focus point and trusted for the whole ray, since a
                // tunnel's local extent is short enough that "underground
                // here" means "underground for this ray".
                let underground = standing_on
                    .zip(world.height_at(focus.x, focus.y))
                    .is_some_and(|((floor_z, _), terrain_z)| {
                        terrain_z > floor_z + CAMERA_FLOOR_REACH
                    });
                let ground_at = |x: f32, y: f32| {
                    // **The floor outranks the terrain here for the same
                    // reason it does for the character's own feet** -- see
                    // `foss-wow#135`. `focus.z`, not each sample's own
                    // height: a fixed reference is enough, because the floor
                    // this is ever going to matter for is the one already
                    // known to be within `FOLLOW_HEIGHT` of it.
                    let floor = world
                        .floor_under_footing(glam::Vec3::new(x, y, focus.z), CAMERA_FLOOR_STEP)
                        .map(|(z, _)| z);
                    match (floor, underground) {
                        (Some(z), _) => Some(z),
                        // Underground with no floor of its own at this exact
                        // point: outside the tunnel's shell, in unmodelled
                        // rock. The terrain field is not a fact about this
                        // point and must not stand in for one -- treated as
                        // clear, the same as a tile that has not streamed in
                        // yet.
                        (None, true) => None,
                        (None, false) => world.height_at(x, y),
                    }
                };
                let wall_at = |from, to| world.first_obstruction(from, to);
                // Ground first, then walls, and the order matters: the
                // ground pass marches outwards and can only ever shorten the
                // ray, so the wall test that follows is asking about a line
                // the eye could actually have reached.
                let eye_at = |yaw: f32, distance: f32| -> glam::Vec3 {
                    let placed = orbit_around(camera_at, yaw, pitch, distance);
                    let above_ground = pull_camera_out_of_the_ground(focus, placed.position, ground_at);
                    pull_camera_clear_of_the_building(focus, above_ground, wall_at)
                };
                // **A clear centre ray says nothing about the rest of the
                // frame.** See `tightest_eye_at_center` and
                // `CAMERA_FOV_SAMPLE_ANGLE`.
                let raw = tightest_eye_at_center(center_yaw, distance, focus, eye_at);
                // **And a clean answer this frame says nothing about the
                // last one.** See `camera_follow_wall_distance`: eased the
                // same way the vertical follow already was, for the same
                // reason -- a wall/ceiling classification flipping between
                // two adjacent, individually-correct frames is a judder, not
                // a fact worth reproducing instantly.
                let eased = Self::camera_follow_wall_distance(
                    &mut wall_distance_state,
                    (raw - focus).length(),
                    dt,
                )
                .min(distance);
                eye_at(center_yaw, eased)
            }
            _ => orbit_around(camera_at, center_yaw, pitch, distance).position,
        };
        self.camera_wall_distance = wall_distance_state;
        self.camera_eye_distance = Some((eye - focus).length());
        if let Camera::Fly(fly) = &mut self.camera {
            // Only the placement, so the free-camera fields a screenshot or the
            // overlay may have set are left alone.
            fly.position = eye;
            // Re-derived from where `eye` actually ended up, not copied from
            // `placed` -- see `face_focus_from` for why the ceiling duck needs
            // this and the ground/wall passes merely tolerate it.
            (fly.yaw, fly.pitch) = face_focus_from(eye, focus);
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
            // `/g` mirrors `/p` exactly, including the local refusal: a
            // guild line from a character in no guild is dropped by the
            // server without a word, so refusing here is the only way the
            // player finds out. Checked again on every ordinary line, because
            // the sticky channel outlives the guild it was switched for --
            // the same trap `/p` documents.
            "g" | "guild" => {
                if !self.in_guild() {
                    self.notice("you are not in a guild.".into());
                } else if argument.is_empty() {
                    self.chat_channel = ChatChannel::Guild;
                } else {
                    self.send_on_channel(&ChatChannel::Guild, argument);
                }
            }
            // The two halves of answering a guild invitation. Commands rather
            // than a prompt frame, deliberately and with the cost stated: an
            // invitation times out, so a chat line is weaker than the party
            // frame's two buttons, and this is named in the milestone's
            // not-done list rather than left to look finished.
            "gaccept" => self.answer_guild_invitation(true),
            "gdecline" => self.answer_guild_invitation(false),
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
        // And the same for guild, for the same reason and by the same door.
        // `/g`'s doc comment in `run_command` claimed this check happened
        // here from the day it was written; it did not, so a sticky `Guild`
        // that outlived its guild sent lines the server dropped in silence.
        // A comment describing a check is not a check.
        if matches!(channel, ChatChannel::Guild) && !self.in_guild() {
            self.notice("you are not in a guild.".into());
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
        // **A spell already sitting on the character is switched off, not cast
        // again**, and that is not an optimisation over a recast -- a recast
        // does nothing at all. `CMSG_CAST_SPELL` for a spell whose aura is
        // held draws no reply of any number: not a refusal, not an
        // acknowledgement, nothing to log and nothing to show. Reported from
        // play as "the stealth took but the recast doesn't unstealth", which
        // is precisely what a request the server discards before it has
        // anything to say looks like from the outside.
        //
        // **Asked of the aura list rather than of the state fields**, and the
        // difference is the whole reason `world::aura` exists.
        // `UNIT_FIELD_BYTES_1` says the character *is* stealthed and cannot
        // say which of the several spells that produce that state to cancel --
        // a rogue's Stealth and a druid's Prowl set the same bit, and two
        // ranks of one spell are two different ids. `CMSG_CANCEL_AURA` wants
        // the id.
        if live
            .state
            .auras
            .get(&live.guid)
            .is_some_and(|held| ::world::aura::holds(held, spell))
        {
            match live.connection.cancel_aura(spell) {
                Ok(()) => {
                    tracing::debug!("cancelled {name} ({spell})");
                    self.action_flash = Some(((bar, slot), Instant::now()));
                    // **No cooldown is started.** Dropping a toggle is not a
                    // cast: predicting one here would grey the button the
                    // player has just pressed to *stop* doing something, and
                    // the global cooldown in particular is a property of
                    // casting.
                }
                Err(e) => {
                    tracing::warn!("cancelling {spell} failed: {e:#}");
                    self.chat
                        .push(Line::Chat(local_notice(format!("could not cancel: {e}"))));
                }
            }
            return;
        }
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
                // A spell with no cooldown of its own -- most of them -- still
                // sits behind the global one, and without this the bar shows
                // nothing at all for pressing it. See
                // `WorldState::start_global_cooldown`'s doc comment.
                live.state.start_global_cooldown(Instant::now());
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
    ///
    /// **`entity.lootable()` also rules a unit out, and not for the reason
    /// `is_dead_or_ghost()` already covers.** `foss-wow#141`: *Milly's
    /// Harvest*'s grapes are a creature with a **zero max health**, not a
    /// missing one -- `is_dead_or_ghost` requires `max > 0` precisely so an
    /// unreplicated health bar cannot masquerade as a kill, and that guard
    /// correctly reads a *genuine* zero the same way. `UNIT_DYNAMIC_FLAGS`'
    /// lootable bit is the server's own answer to "can this be looted right
    /// now", independent of health entirely, and something the server marked
    /// lootable is never something to swing at.
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
            && !entity.lootable()
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
        if !self.sound_enabled {
            self.effects.stop();
            self.ambience.stop();
            self.weather_channel.stop();
            self.footstep_phase = None;
            self.pending_sounds.clear();
        }
        // **Three timers, because `sound` did not move when the obvious fix
        // landed.** Caching clip bytes took archive reads from 145 to 2 and
        // the phase stayed at 3.7 ms, so reading the file was never the cost
        // -- which leaves resolving where the character is standing, deciding
        // whether a foot landed, and actually starting the sinks. Those have
        // nothing in common and one of them owns the number.
        let timing_area = Instant::now();
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
        let context = match streaming_world
            .and_then(|world| world.area_context_at_position(at, STEP_HEIGHT))
        {
            Some(context) => {
                self.area = Some(context.area);
                context
            }
            None => match self.area {
                Some(area) => crate::world::AreaContext {
                    area,
                    zone_music: None,
                    ambience: None,
                },
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
            .zone_with_overrides(context.area, context.zone_music, context.ambience)
            .map(|zone| zone.for_time(when))
            .unwrap_or((None, None));
        let music = music.filter(|_| self.music_enabled);
        let ambience = ambience.filter(|_| self.sound_enabled);
        // A storm is a state of the sky, not of the zone: it can start or
        // stop under an area whose own ambience never changes, and it must
        // stop the moment the character is under a roof -- `modeled_floor`
        // is exactly the signal `precipitation_for_frame` already uses for
        // the same reason, so the drops stop falling and the loop stops
        // playing on the same frame. See `weather_ambience`.
        let weather_sound = weather_ambience(frame_weather(self.live.as_ref(), &self.args).weather)
            .filter(|_| !self.modeled_floor && self.sound_enabled)
            .map(sound::WeatherAmbience::sound_id);

        // One roll per call is fine: a channel only consults it when it is
        // actually starting something, which is rare.
        let roll = self.last_frame.elapsed().subsec_nanos() as f32 / 1_000_000_000.0;
        // **Logged when it changes, not every frame.** A zone's music is the
        // sort of thing that is either obviously working or obviously not, and
        // "obviously" needs an ear -- so this leaves a trail that says what it
        // decided, which is readable without one. That has caught more in this
        // project than looking has.
        if (self.music.playing(), self.ambience.playing(), self.weather_channel.playing())
            != (music, ambience, weather_sound)
        {
            tracing::debug!(
                "area {} at {when:?}: music {:?} -> {music:?}, ambience {:?} -> {ambience:?}, \
                 weather {:?} -> {weather_sound:?}",
                context.area,
                self.music.playing(),
                self.ambience.playing(),
                self.weather_channel.playing(),
            );
        }

        let Some(audio) = self.audio.as_ref() else {
            return;
        };
        let mixer = audio.mixer();


        self.sound_area_ms = timing_area.elapsed().as_secs_f32() * 1000.0;
        let timing_steps = Instant::now();

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
                // WMO materials that name no terrain; a modeled floor still
                // outranks the terrain even when it names no footing.
                let footing = match self.floor_material {
                    Some(row) => Some(sound::Footing::Surface(row as u32)),
                    None if !self.modeled_floor => world
                        .footing_at(live.position.x, live.position.y)
                        .map(sound::Footing::Ground),
                    None => None,
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
        self.sound_steps_ms = timing_steps.elapsed().as_secs_f32() * 1000.0;
        let timing_play = Instant::now();
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
        if self.sound_enabled {
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
        } else {
            self.pending_sounds.clear();
        }
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
        // Same volume knob as the zone's own ambience: a storm's patter is
        // that same kind of background loop, not a third category a player
        // would expect its own slider for.
        self.weather_channel.play(
            mixer,
            &self.sounds,
            &mut self.chain,
            weather_sound,
            self.args.ambience_volume,
            roll,
        );
        self.sound_play_ms = timing_play.elapsed().as_secs_f32() * 1000.0;
    }

    fn toggle_sound(&mut self) {
        self.sound_enabled = !self.sound_enabled;
        if !self.sound_enabled {
            self.effects.stop();
            self.ambience.stop();
            self.weather_channel.stop();
            self.footstep_phase = None;
            self.pending_sounds.clear();
        }
        let text = if self.sound_enabled {
            "Sound Effects Enabled"
        } else {
            "Sound Effects Disabled"
        };
        tracing::info!("{text}");
        self.status_text = Some(PendingStatusText {
            text: text.to_string(),
            spawned: Instant::now(),
        });
    }

    fn toggle_music(&mut self) {
        self.music_enabled = !self.music_enabled;
        if !self.music_enabled {
            self.music.stop();
        }
        let text = if self.music_enabled {
            "Music Enabled"
        } else {
            "Music Disabled"
        };
        tracing::info!("{text}");
        self.status_text = Some(PendingStatusText {
            text: text.to_string(),
            spawned: Instant::now(),
        });
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

    /// Everything a bag or character square's tooltip draws beyond the name.
    /// Every field answers its "not yet known" default until the same
    /// `SMSG_ITEM_QUERY_SINGLE_RESPONSE` `item_name` reads from has arrived --
    /// see that function's own doc comment for why the fallback is honest
    /// rather than invented.
    ///
    /// Takes the spellbook, archive chain and atlas to resolve an on-use
    /// item's effect line -- see `spells::Spellbook::resolve_extra` -- as
    /// three more disjoint borrows alongside `live`, for the same reason
    /// `item_name` above takes its pieces separately rather than `&self`.
    fn item_tooltip(
        live: Option<&live::LiveWorld>,
        spells: &mut spells::Spellbook,
        chain: &mut Chain,
        maps: &maps::Maps,
        entry: u32,
    ) -> ui::frames::BagItemTooltip {
        let Some(info) = live.and_then(|live| live.state.names.item(entry)).flatten() else {
            return ui::frames::BagItemTooltip::default();
        };
        // `weapon_delay` is the tell rather than `item_class`: every weapon
        // has a nonzero swing speed and nothing else does, so this needs no
        // opinion about what `ItemClass` values mean -- it reads what the
        // packet's own weapon-only fields say directly.
        let weapon = (info.weapon_delay > 0).then_some(ui::frames::WeaponStats {
            damage_min: info.damage_min,
            damage_max: info.damage_max,
            delay_ms: info.weapon_delay,
        });
        let stats = info
            .stats
            .iter()
            .filter(|stat| stat.value != 0)
            .map(|stat| {
                let label = ::world::query::item_stat_label(stat.stat_type)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("Stat {}", stat.stat_type));
                (label, stat.value)
            })
            .collect();
        // Resolved on demand rather than at login, because which items exist
        // is not known until well after it -- see `resolve_extra`'s own doc
        // comment. Whatever the spell's own description could not resolve
        // (most often food's `$o1`, a periodic total this project's
        // `dbc::spelltext` deliberately leaves unconfirmed) stays visible as
        // a token rather than becoming a guessed number.
        let use_description = info
            .use_spell()
            .map(|spell| {
                spells.resolve_extra(chain, spell);
                let text = spells.description(spell);
                // `$z` is not a spell column at all -- it names wherever
                // this character's hearth is bound, which only
                // `SMSG_BINDPOINTUPDATE` says. Resolved here rather than
                // inside `dbc::spelltext`, which stays player-agnostic on
                // purpose -- see that module's doc comment on `$g`/`$l`.
                // Left as the literal token if the bind point has not
                // arrived yet or names an area this build has no name for,
                // the same fallback every other token here uses.
                match (text.contains("$z"), live.and_then(|live| live.state.home_bind)) {
                    (true, Some(bind)) => match maps.area_name(bind.area_id) {
                        Some(area) => text.replace("$z", &area),
                        None => text,
                    },
                    _ => text,
                }
            })
            .unwrap_or_default();
        ui::frames::BagItemTooltip {
            quality: info.quality,
            item_level: info.item_level,
            required_level: info.required_level,
            description: info.description.clone(),
            armor: info.armor,
            weapon,
            stats,
            use_description,
        }
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

    /// What the objective tracker draws.
    ///
    /// **A second view of the quest log, never a second copy of it.** Every
    /// line comes from the same three places the log's rows do -- the quest
    /// cache for the title and level, the player's own fields and bags for the
    /// counters, `self.objectives` for the markers -- so the two frames cannot
    /// drift. What the tracker adds is the one thing the log does not have:
    /// how far away the nearest marker is.
    ///
    /// Returns `None` before there is a session, which is not the same as a
    /// character with nothing to do; see `ui::HudData::tracker`.
    fn tracker_view(&self) -> Option<ui::TrackerView> {
        let live = self.live.as_ref()?;
        let player = live.state.get(live.guid);
        let log: Vec<u32> = player
            .map(|player| player.quest_log_ids())
            .unwrap_or_default();
        let level = player.and_then(|player| player.level()).unwrap_or(0);

        let mut quests: Vec<ui::TrackedQuest> = log
            .iter()
            .map(|id| {
                let complete = player
                    .and_then(|player| player.quest_is_complete(*id))
                    .unwrap_or(false);
                let (title, quest_level, progress) = match self.quests.answer(*id) {
                    ::world::Answer::Known(info) => (
                        info.title.clone(),
                        Some(info.level),
                        self.quest_progress(info),
                    ),
                    // The id, which a player can report, rather than a
                    // placeholder title, which they cannot. The log's rows
                    // make the same call.
                    _ => (format!("Quest {id}"), None, Vec::new()),
                };
                ui::TrackedQuest {
                    id: *id,
                    title,
                    progress,
                    complete,
                    // **The server's own trivial verdict where there is one.**
                    // `quest_marks` is keyed by NPC rather than by quest, so
                    // there is no mark to consult for a quest already in the
                    // log -- a quest you are carrying is never on offer. So
                    // this passes `false` and the band is decided by the
                    // arithmetic alone, which is honest here: the grey band
                    // exists to say "do not bother taking this", and a quest
                    // already taken is past that question.
                    difficulty: ui::Difficulty::of(
                        quest_level.unwrap_or(0),
                        level,
                        false,
                    ),
                    level: quest_level,
                    distance: self.nearest_objective(*id),
                }
            })
            .collect();

        // Finished quests first -- "go and hand this in" outranks anything
        // still in progress -- and the rest by how far away they are, nearest
        // first. **A quest with no markers sorts last rather than as zero**,
        // which is the difference between "you are standing on it" and "the
        // realm did not say".
        quests.sort_by(|a, b| {
            b.complete.cmp(&a.complete).then_with(|| {
                a.distance
                    .unwrap_or(f32::INFINITY)
                    .total_cmp(&b.distance.unwrap_or(f32::INFINITY))
            })
        });
        let total = quests.len();
        quests.truncate(self.hud.profile.style.tracker_quests);
        Some(ui::TrackerView { quests, total })
    }

    /// Yards to the nearest point of any marker the realm gave for one quest,
    /// or `None` when it gave none.
    ///
    /// **Measured from the walked position, not from replicated state.** The
    /// server never relays this client's own movement back, so the entity for
    /// our guid holds the position the character logged in at, forever. That
    /// trap has caught four callers in this file already -- the thing that
    /// draws the player, the thing that aims at it, a loot range check, and
    /// the world map -- and here it would produce a distance that was correct
    /// once and then counted down to a place nobody is standing.
    ///
    /// Flat distance, ignoring height, because a POI point has no height: the
    /// server sends an `(x, y)` pair per point and nothing else.
    fn nearest_objective(&self, quest: u32) -> Option<f32> {
        let live = self.live.as_ref()?;
        let markers = self.objectives.markers(quest);
        markers
            .iter()
            .filter(|poi| poi.map_id == live.map_id)
            .flat_map(|poi| poi.points.iter())
            .map(|(x, y)| {
                let dx = *x as f32 - live.position.x;
                let dy = *y as f32 - live.position.y;
                (dx * dx + dy * dy).sqrt()
            })
            .min_by(f32::total_cmp)
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
            // **Print the fields the branches below decide on, every time --
            // not only when none of them match.** `foss-wow#141`'s grapes
            // were guessed to be a zero-max-health, lootable quest prop and
            // the guess was wrong: a live report came back describing
            // ordinary alive-creature behaviour, which means either the
            // health or the lootable half of that guess (or both) does not
            // hold for this entry, and there is no way to tell which from
            // outside. "Nothing happened" is two findings wearing one
            // sentence, and so is "the wrong branch ran" -- both want the
            // actual field values, not another guess.
            if let Some(live) = self.live.as_ref() {
                if let Some(entity) = live.state.get(guid) {
                    tracing::info!(
                        "right-click {guid:#018x}: entry {:?}, type {:?}, health {:?}/{:?}, \
                         dead_or_ghost {}, lootable {}, npc_flags {:?}",
                        entity.entry(),
                        entity.object_type,
                        entity.health(),
                        entity.max_health(),
                        entity.is_dead_or_ghost(),
                        entity.lootable(),
                        entity.npc_flags(),
                    );
                }
            }
            // **Before the talk branch, because a mailbox is not an NPC and
            // would fall through every one of the tests below to nothing.**
            // A game object carries no `UNIT_NPC_FLAGS`, so `will_talk` says
            // no, `is_attack_candidate` says no and `is_loot_candidate` says
            // no -- and a right-click on the one object in the world this
            // milestone is about would do nothing at all while looking
            // exactly like a click that missed.
            if self.is_mailbox(guid) {
                self.open_mailbox(guid);
            } else if self.runs_an_auction_house(guid) {
                // **Before the talk branch, and that ordering is the whole
                // reason this arm exists here.** An auctioneer's entire
                // `UNIT_NPC_FLAGS` word is the auctioneer bit -- it does not
                // gossip -- so `is_talk_candidate` says yes on "any bit set"
                // and `greet` would send a `CMSG_GOSSIP_HELLO` the server
                // answers with nothing. That is indistinguishable from a
                // click that missed, which is exactly what the mailbox arm
                // above was added to stop happening.
                self.open_auction_house(guid);
            } else if self.is_talk_candidate(guid) {
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
            } else if self.wants_kneeling_cast(guid) {
                // **Before the generic game-object arm, and for the same
                // reason mailbox and auction sit above talk/attack/loot.**
                // `foss-wow#141`: `CMSG_GAMEOBJ_USE` -- what the arm below
                // sends -- does nothing at all for a chest, because
                // AzerothCore's `GameObject::Use()` has no case for
                // `GAMEOBJECT_TYPE_CHEST`. A chest is opened by casting a
                // spell at it instead -- and a locked gathering node wants
                // the same cast for the same reason `needs_kneeling_cast`
                // checks a caption rather than a type: reported live as
                // looting with no animation or cast bar through this same
                // catch-all arm.
                self.open_with_kneeling_cast(guid);
            } else if self.is_usable_gameobject(guid) {
                // **Last, and deliberately a catch-all.** Every branch above
                // this one already ruled itself in or out for a specific
                // reason; whatever reaches here is a game object this client
                // has no bespoke window for -- a door, a lever -- and "use
                // it" is the one thing right-click means for all of them. A
                // mailbox and anything wanting a kneeling cast never reach
                // this arm: both claimed above, once their name query has
                // answered.
                self.use_gameobject(guid);
            }
        }
    }

    /// Whether this guid is a game object this client can send
    /// [`ClientOpcode::GameObjectUse`] at.
    ///
    /// Unlike [`Self::is_mailbox`], this asks nothing about *what kind* of
    /// game object it is -- the server decides what "use" means per object,
    /// and every kind answers the same request. So this only has to rule out
    /// what is not a game object at all, and does not need the name query
    /// mailboxes wait on.
    fn is_usable_gameobject(&self, guid: u64) -> bool {
        let Some(live) = self.live.as_ref() else {
            return false;
        };
        let Some(object) = live.state.get(guid) else {
            return false;
        };
        object.object_type == ::world::ObjectType::GameObject
    }

    /// Sends the interact request for a game object -- see
    /// [`ClientOpcode::GameObjectUse`].
    fn use_gameobject(&mut self, guid: u64) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if let Err(e) = live.connection.use_gameobject(guid) {
            tracing::warn!("using game object {guid:#018x} failed: {e:#}");
            return;
        }
        tracing::info!("used game object {guid:#018x}");
    }

    /// Opens a chest, or anything else [`Self::wants_kneeling_cast`] --
    /// see [`::world::spell::OPEN_LOCK_KNEELING`] for why this is a spell
    /// cast rather than [`Self::use_gameobject`], and for the scope this
    /// does not yet cover: an object gated behind a real gathering or
    /// lockpicking skill will be refused by this same spell, correctly,
    /// since this client does not read `Lock.dbc` and cannot tell the
    /// different lock types apart before asking.
    fn open_with_kneeling_cast(&mut self, guid: u64) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if let Err(e) = live
            .connection
            .cast_spell_at_gameobject(::world::spell::OPEN_LOCK_KNEELING, guid)
        {
            tracing::warn!("opening game object {guid:#018x} failed: {e:#}");
            return;
        }
        tracing::info!("opening game object {guid:#018x}");
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
                options: questgiver
                    .options
                    .iter()
                    .map(|option| ui::QuestgiverOption {
                        index: option.index,
                        message: option.message.clone(),
                    })
                    .collect(),
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
                reward_choices: Vec::new(),
                selected_reward: 0,
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
            // Same reasoning as `rewards`: ids, not names, until `foss-wow#56`
            // sends `CMSG_ITEM_QUERY_SINGLE`.
            reward_choices: quest
                .reward_choices
                .iter()
                .map(|reward| format!("item {} x{}", reward.item, reward.count))
                .collect(),
            // Clamped rather than trusted: `showing` resets this to `0`
            // whenever the quest changes, but a quest cache entry that
            // updates its choice list under an already-open window (an
            // edge nothing has exercised) must not index past the end of
            // one that just got shorter.
            selected_reward: questgiver
                .selected_reward
                .min(quest.reward_choices.len().saturating_sub(1)),
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
        let ui::QuestgiverView::Quest {
            action,
            selected_reward,
            ..
        } = view
        else {
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
        //
        // `selected_reward` is whichever row the player last clicked, `0` by
        // default -- correct on its own for a quest with nothing to choose
        // between, and now a real choice rather than a hardcoded one for a
        // quest that offers several. See `QuestgiverView::Quest::reward_choices`.
        if matches!(action, ui::QuestgiverAction::Complete) {
            if let Err(e) = live
                .connection
                .choose_quest_reward(npc, quest, selected_reward as u32)
            {
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

    /// Whether this character is in a guild at all.
    ///
    /// Read off the replicated field rather than off the stored roster,
    /// because the roster is only there once somebody has pressed `G`: a
    /// character who has never opened the window is still in their guild, and
    /// keying the chat refusal off the roster would refuse the first guild
    /// line of every session.
    fn in_guild(&self) -> bool {
        self.live.as_ref().is_some_and(|live| {
            live.state
                .get(live.guid)
                .and_then(|player| player.fields.get(::world::update::fields::PLAYER_GUILDID))
                .is_some_and(|id| id != 0)
        })
    }

    /// Accepts or declines the pending guild invitation.
    ///
    /// **Neither request identifies anything** -- both bodies are empty and
    /// the server resolves which guild from the invitation it recorded when
    /// the invite went out, because a character holds one at a time. The local
    /// record is cleared either way: it exists only so the prompt can say who
    /// is asking, and an answered invitation has nothing left to say.
    fn answer_guild_invitation(&mut self, accept: bool) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if live.state.guild_invitation.is_none() {
            self.notice("nobody has asked you to join a guild.".into());
            return;
        }
        let sent = if accept {
            live.connection.guild_accept()
        } else {
            live.connection.guild_decline()
        };
        if let Err(e) = sent {
            tracing::warn!("answering a guild invitation failed: {e:#}");
            self.notice(format!("could not answer: {e}"));
            return;
        }
        live.state.guild_invitation = None;
        // Nothing else is said locally. **An accept is answered by
        // `SMSG_GUILD_EVENT`** naming the join, which is the server agreeing;
        // saying "joined" here would state as fact the thing that reply is
        // about to answer, and a decline is silent by design.
    }

    /// Asks for the roster, and for the guild's name alongside it.
    ///
    /// **Two requests, because the roster does not name its own guild.**
    /// `SMSG_GUILD_ROSTER` carries the members, the ranks' permissions and the
    /// message of the day, and nowhere in it is the guild's id or its name --
    /// those come from `CMSG_GUILD_QUERY`, which is answered for any guild id
    /// at all and reaches the id through the player's own replicated
    /// `PLAYER_GUILDID`.
    ///
    /// The rank *names* are in the same second packet, which is why a roster
    /// drawn before it arrives shows rank numbers: the two halves of a row
    /// genuinely come from two packets.
    fn ask_guild(&mut self) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if let Err(e) = live.connection.guild_roster() {
            tracing::warn!("asking for the guild roster failed: {e:#}");
            self.notice(format!("could not ask for the roster: {e}"));
            return;
        }
        // An absent field is a zero, and zero is "no guild" -- so this is
        // skipped rather than sent with a zero id, which the server drops
        // without a reply and which would look exactly like a wrong opcode.
        let guild = live
            .state
            .get(live.guid)
            .and_then(|player| player.fields.get(::world::update::fields::PLAYER_GUILDID))
            .unwrap_or(0);
        if guild != 0 {
            if let Err(e) = live.connection.guild_query(guild) {
                tracing::warn!("asking what guild {guild} is called failed: {e:#}");
            }
        }
    }

    /// Whether this guid is a mailbox that can be opened.
    ///
    /// **The only question in this client answered by asking what an object
    /// *is*.** A display id draws a mailbox and a bench with equal
    /// confidence; `SMSG_GAMEOBJECT_QUERY_RESPONSE`'s type is the only thing
    /// that tells them apart, and it arrives a moment after the object does.
    ///
    /// So an entry whose answer has not come back yet is `false` here, which
    /// is right and worth stating: the alternative is opening a window for
    /// something that turns out to be a bench. The cost is that the very
    /// first right-click on a freshly-streamed mailbox can miss, and the
    /// query is claimed once per *entry* rather than per object, so the
    /// second click works and every later mailbox of the same kind works at
    /// once.
    fn is_mailbox(&self, guid: u64) -> bool {
        let Some(live) = self.live.as_ref() else {
            return false;
        };
        let Some(object) = live.state.get(guid) else {
            return false;
        };
        if object.object_type != ::world::ObjectType::GameObject {
            return false;
        }
        object
            .entry()
            .and_then(|entry| live.state.names.gameobject(entry).flatten())
            .is_some_and(|info| info.is_mailbox())
    }

    /// Whether this guid wants `OPEN_LOCK_KNEELING` cast at it rather than a
    /// plain [`Self::use_gameobject`] -- see
    /// [`::world::query::GameObjectInfo::needs_kneeling_cast`] for why that
    /// is not only chests (`foss-wow`, reported live: a locked gathering
    /// node looted with no cast bar and no animation at all through the
    /// plain-use path), and [`Self::is_mailbox`]'s doc comment for the
    /// identical first-click-can-miss reasoning this shares.
    fn wants_kneeling_cast(&self, guid: u64) -> bool {
        let Some(live) = self.live.as_ref() else {
            return false;
        };
        let Some(object) = live.state.get(guid) else {
            return false;
        };
        if object.object_type != ::world::ObjectType::GameObject {
            return false;
        }
        object
            .entry()
            .and_then(|entry| live.state.names.gameobject(entry).flatten())
            .is_some_and(|info| info.needs_kneeling_cast())
    }

    /// Opens a mailbox and asks it what is inside.
    ///
    /// The window appears *before* the answer, like the trainer's and unlike
    /// the loot window's: `CMSG_GET_MAIL_LIST` is always answered, so an
    /// empty window is a reply still in flight rather than a click that did
    /// not register -- and an inbox with nothing in it is a real and ordinary
    /// state that has to be drawn as itself.
    fn open_mailbox(&mut self, guid: u64) {
        let Some(live) = self.live.as_mut() else { return };
        if let Err(e) = live.connection.get_mail_list(guid) {
            tracing::warn!("asking mailbox {guid:#x} for its contents failed: {e:#}");
            return;
        }
        tracing::info!("opened mailbox {guid:#018x}");
        self.mailbox = Some(guid);
    }

    /// Whether the open mailbox is still there and still in reach.
    ///
    /// **The server checks this on every single mail request** -- the object
    /// must still exist, still be a mailbox, and still be within about ten
    /// units -- and every refusal is the request simply doing nothing. So the
    /// window closes itself when the character walks away, rather than
    /// staying open to send things that are silently dropped.
    ///
    /// Distance is measured from where this client thinks it *is*, never from
    /// replicated state: the server does not relay our own movement back, so
    /// our replicated position is wherever we logged in. That trap has been
    /// walked into by four separate callers in this project, which is why the
    /// position is passed in rather than looked up.
    fn mailbox_in_reach(state: &::world::WorldState, mailbox: Option<u64>, from: glam::Vec3) -> bool {
        // The server logs "maximal 10 is allowed" and then applies its own
        // model-aware box test, which is more generous than a point distance.
        // Ten is used here because being slightly too permissive costs one
        // refused request, and being too strict closes a window somebody is
        // in the middle of using.
        const REACH: f32 = 10.0;
        let Some(guid) = mailbox else {
            return false;
        };
        state
            .get(guid)
            .and_then(|object| object.position)
            .is_some_and(|at| {
                let (dx, dy, dz) = (at.x - from.x, at.y - from.y, at.z - from.z);
                (dx * dx + dy * dy + dz * dz).sqrt() <= REACH
            })
    }

    /// Takes everything out of one letter, then asks the mailbox again.
    ///
    /// **Re-asked rather than edited**, the decision the trainer window made
    /// and for the same reason: these requests can be refused, and a window
    /// that struck a letter off its own list would show an inbox the server
    /// disagrees with and say nothing about it.
    ///
    /// The money goes first and the attachments after it, which is the order
    /// the server's own refusals want: taking an attachment off a
    /// cash-on-delivery letter *spends* money, and a letter whose copper has
    /// already been collected is the cheapest state to stop halfway in.
    fn take_mail(&mut self, id: u32) {
        let Some(mailbox) = self.mailbox else { return };
        let letter = self
            .live
            .as_ref()
            .and_then(|live| live.state.mail.as_ref())
            .and_then(|inbox| inbox.get(id))
            .cloned();
        let Some(letter) = letter else {
            // Every refusal here says so. All four ways out of the trade
            // window's offer were silent, and the result was a live report of
            // "I couldn't give him an item" with every line of code correct.
            tracing::warn!("take from mail {id}: no such letter in the inbox");
            return;
        };
        let Some(live) = self.live.as_mut() else { return };
        if letter.money > 0 {
            if let Err(e) = live.connection.mail_take_money(mailbox, id) {
                tracing::warn!("taking money from mail {id} failed: {e:#}");
                return;
            }
        }
        for item in &letter.items {
            // The **32-bit low guid**, which is the only handle a mailed item
            // has: it is not a replicated object, so there is no full guid on
            // the wire to widen this to.
            if let Err(e) = live.connection.mail_take_item(mailbox, id, item.guid) {
                tracing::warn!(
                    "taking attachment {} from mail {id} failed: {e:#}",
                    item.guid
                );
                return;
            }
        }
        // Silent either way, and the only request in this block that is. Its
        // effect shows up in the list below and nowhere else.
        if let Err(e) = live.connection.mail_mark_as_read(mailbox, id) {
            tracing::warn!("marking mail {id} read failed: {e:#}");
        }
        if let Err(e) = live.connection.get_mail_list(mailbox) {
            tracing::warn!("re-asking the mailbox failed: {e:#}");
        }
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
            options: Vec::new(),
            menu_id: 0,
            showing: None,
            selected_reward: 0,
        });
        // **Cleared before the new one is decided, not overwritten after.**
        // Greeting a plain NPC while a trainer's list is open would otherwise
        // leave that list on screen belonging to somebody the player has
        // stopped talking to, and its purchases would go to an NPC the server
        // no longer considers open -- refused in silence, which is the one
        // failure this client cannot diagnose.
        self.trainer = None;
        self.vendor = None;
        self.taxi = None;
        // And the auction window with them, for the identical reason: bids
        // sent to an auctioneer the server no longer considers open are
        // refused in silence.
        self.auction = None;

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
                            name: name.clone(),
                            list: None,
                        })
                    }
                    Err(e) => tracing::warn!("asking {guid:#x} for a trainer list failed: {e:#}"),
                }
            }
        }

        // **A vendor is asked in the same breath as it is greeted**, exactly
        // like the trainer and the flight master above. `CMSG_LIST_INVENTORY`
        // is the one request in this block that is answered even for an NPC
        // with nothing to sell -- see `world::vendor` -- so there is no
        // silence to interpret the way there is for a trainer's.
        if self.is_vendor(guid) {
            if let Some(live) = self.live.as_mut() {
                match live.connection.list_inventory(guid) {
                    Ok(()) => {
                        self.vendor = Some(VendorSession {
                            npc: guid,
                            name,
                            list: None,
                        })
                    }
                    Err(e) => tracing::warn!("asking {guid:#x} for its stock failed: {e:#}"),
                }
            }
        }
    }



    /// The auction window, built from the one page the world state holds.
    ///
    /// **Truncated to what the window can draw**, and the page arithmetic goes
    /// with it: the server's fifty is a cap on what it will send, not a step,
    /// because `listfrom` is a row index. So the window pages by
    /// `VISIBLE_ROWS` and asks from row `offset`, and `total` -- the server's
    /// own number -- keeps the range line honest whichever page size is used.
    #[allow(clippy::too_many_arguments)]
    fn auction_view(
        session: Option<&AuctionSession>,
        live: Option<&live::LiveWorld>,
        items: &mut items::Items,
        chain: &mut Chain,
        gpu: &Gpu,
        egui_renderer: &mut egui_wgpu::Renderer,
    ) -> Option<ui::AuctionView> {
        let session = session?;
        let (tab, offset, search, selected, waiting, house) = (
            session.tab,
            session.offset,
            session.search.clone(),
            session.selected,
            session.waiting,
            session.house,
        );

        // What is needed off the live world, gathered before anything takes
        // `self` mutably for icons.
        let gathered: Vec<(::world::Auction, String)> = live
            .and_then(|live| live.state.auctions.as_ref().map(|page| (live, page)))
            .map(|(live, page)| {
                page.auctions
                    .iter()
                    .take(ui::frames::auction::VISIBLE_ROWS)
                    .map(|auction| {
                        // The seller is a guid and the roster of everybody
                        // selling is not something this client has. A name
                        // query answers for one, and until it does the guid's
                        // low half is drawn -- honest, and better than a blank
                        // column that would read as an auction with no owner.
                        let seller = live
                            .state
                            .names
                            .player(auction.owner)
                            .flatten()
                            .map(|name| name.to_string())
                            .unwrap_or_else(|| format!("player {}", auction.owner & 0xFFFF_FFFF));
                        (auction.clone(), seller)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let total = live
            .and_then(|live| live.state.auctions.as_ref())
            .map(|page| page.total)
            .unwrap_or(0);

        let rows = gathered
            .into_iter()
            .map(|(auction, seller)| {
                let name = Self::item_name(live, items, auction.item);
                let icon = items.icon(gpu, egui_renderer, chain, auction.item);
                ui::AuctionRow {
                    id: auction.id,
                    name,
                    count: auction.count,
                    icon,
                    seller,
                    bid: auction.bid,
                    // Worked out in one place, from the two different fields
                    // it comes from -- see `world::auction::Auction::next_bid`.
                    next_bid: auction.next_bid(),
                    buyout: auction.buyout,
                    band: match auction.band() {
                        ::world::TimeBand::Short => ui::frames::auction::TimeBand::Short,
                        ::world::TimeBand::Medium => ui::frames::auction::TimeBand::Medium,
                        ::world::TimeBand::Long => ui::frames::auction::TimeBand::Long,
                        ::world::TimeBand::VeryLong => ui::frames::auction::TimeBand::VeryLong,
                    },
                    own: live.is_some_and(|live| auction.is_own(live.guid)),
                }
            })
            .collect();

        Some(ui::AuctionView {
            tab,
            rows,
            total,
            offset,
            page_rows: ui::frames::auction::VISIBLE_ROWS as u32,
            selected,
            search,
            waiting,
            house,
        })
    }

    /// Whether this unit's replicated flags say it runs an auction house.
    ///
    /// Its own predicate beside [`App::offers_training`] for the same reason:
    /// the one place naming bit `0x200000` is the place carrying the evidence.
    /// Measured on Auctioneer Buckler, whose whole flag word is `0x200000` and
    /// nothing else -- an auctioneer does not gossip, sell or train -- and who
    /// answers `MSG_AUCTION_HELLO` from zero units away. The three NPCs
    /// standing at this project's fixture spot carry `0x10283`, `0x3` and
    /// `0x80` and none of them answers it.
    fn runs_an_auction_house(&self, guid: u64) -> bool {
        /// `UNIT_NPC_FLAG_AUCTIONEER`. See [`App::runs_an_auction_house`].
        const AUCTIONEER: u32 = 0x0020_0000;
        self.live
            .as_ref()
            .and_then(|live| live.state.get(guid))
            .and_then(|entity| entity.npc_flags())
            .is_some_and(|flags| flags & AUCTIONEER != 0)
    }

    /// Sends whatever this session's tab asks for, and records the offset.
    ///
    /// **The two calls are here, adjacent, and nowhere else.** The offset goes
    /// out on the wire and is also told to the world state, because the reply
    /// does not carry it -- and a pair of calls that must agree is exactly the
    /// shape this project keeps getting wrong when it is spread over two call
    /// sites.
    fn ask_auctions(&mut self) {
        let Some(session) = self.auction.as_mut() else {
            return;
        };
        let offset = session.offset;
        let browsing = session.tab == ui::AuctionTab::Browse;
        let Some(live) = self.live.as_mut() else {
            return;
        };
        // The two lists that do not page always start at row zero, and telling
        // the state otherwise would label their rows with an offset they do
        // not have.
        live.state
            .expect_auction_page(if browsing { offset } else { 0 });
        let Some(session) = self.auction.as_ref() else {
            return;
        };
        match session.ask(&mut live.connection) {
            Ok(()) => {
                if let Some(session) = self.auction.as_mut() {
                    session.waiting = true;
                }
            }
            Err(e) => {
                tracing::warn!("asking the auction house failed: {e:#}");
                self.notice(format!("could not ask the auction house: {e}"));
            }
        }
    }

    /// Opens the auction window at an auctioneer.
    ///
    /// The greeting and the first search go out together, exactly as a
    /// trainer's list does -- and the greeting is the one that matters,
    /// because it is the only packet in the block that names the house.
    fn open_auction_house(&mut self, guid: u64) {
        // Any page held from a previous auctioneer is dropped before the new
        // one is asked. Rows from one house under a title naming another is
        // precisely the confusion `AuctionHouse` exists to prevent, and there
        // is no packet anywhere that would say it had happened.
        if let Some(live) = self.live.as_mut() {
            live.state.auctions = None;
            live.state.auction_house = None;
            if let Err(e) = live.connection.auction_hello(guid) {
                tracing::warn!("greeting auctioneer {guid:#x} failed: {e:#}");
                return;
            }
        }
        self.auction = Some(AuctionSession {
            npc: guid,
            house: None,
            tab: ui::AuctionTab::Browse,
            offset: 0,
            search: String::new(),
            selected: None,
            waiting: true,
        });
        self.ask_auctions();
    }

    /// Acts on a control under the auction list.
    ///
    /// Every arm here is reached only when
    /// `ui::frames::auction::control_live` said the control would do
    /// something, so the refusals below are belt and braces rather than the
    /// only guard -- but they log, because all four of the ways this can be a
    /// no-op are otherwise silent, and a silent send is the one failure this
    /// client cannot diagnose.
    fn auction_control(&mut self, click: ui::AuctionClick) {
        let page = ui::frames::auction::VISIBLE_ROWS as u32;
        match click {
            ui::AuctionClick::PreviousPage | ui::AuctionClick::NextPage => {
                let Some(session) = self.auction.as_mut() else {
                    return;
                };
                session.offset = match click {
                    ui::AuctionClick::NextPage => session.offset.saturating_add(page),
                    _ => session.offset.saturating_sub(page),
                };
                // The selection is dropped rather than carried: it names a row
                // that is about to stop being on screen, and a Bid button that
                // still pointed at it would spend money on something nobody
                // can see.
                session.selected = None;
                self.ask_auctions();
            }
            ui::AuctionClick::Bid | ui::AuctionClick::Buyout => {
                let Some((npc, id, price)) = self.selected_auction_price(click) else {
                    tracing::info!("a bid was asked for with nothing selected to bid on");
                    return;
                };
                let Some(live) = self.live.as_mut() else {
                    return;
                };
                match live.connection.auction_place_bid(npc, id, price) {
                    // A zero id or a zero price; refused here rather than
                    // dropped by the server without a word.
                    Ok(false) => {
                        tracing::info!("auction {id} at {price} is not a bid the server accepts")
                    }
                    Ok(true) => self.notice(format!(
                        "bid {} on auction {id}",
                        ui::frames::auction::money(price)
                    )),
                    Err(e) => tracing::warn!("bidding on auction {id} failed: {e:#}"),
                }
            }
            ui::AuctionClick::Cancel => {
                let Some(session) = self.auction.as_ref() else {
                    return;
                };
                let (npc, Some(id)) = (session.npc, session.selected) else {
                    tracing::info!("a cancellation was asked for with nothing selected");
                    return;
                };
                if let Some(live) = self.live.as_mut() {
                    match live.connection.auction_remove_item(npc, id) {
                        // The goods come back as **mail**, not to the bag, so
                        // the notice says where to look for them.
                        Ok(()) => self.notice(format!(
                            "cancelled auction {id}; the goods come back by mail"
                        )),
                        Err(e) => tracing::warn!("cancelling auction {id} failed: {e:#}"),
                    }
                }
            }
        }
    }

    /// What a bid or a buyout on the selection would cost.
    ///
    /// The two prices come from **different fields** and picking the wrong one
    /// is refused in silence: a bid is the current bid plus the server's own
    /// increment, or the opening price when nobody has bid, and a buyout is
    /// the seller's number. See `world::auction::Auction::next_bid`.
    fn selected_auction_price(&self, click: ui::AuctionClick) -> Option<(u64, u32, u32)> {
        let session = self.auction.as_ref()?;
        let id = session.selected?;
        let live = self.live.as_ref()?;
        let page = live.state.auctions.as_ref()?;
        let auction = page.get(id)?;
        let price = match click {
            ui::AuctionClick::Buyout => auction.buyout,
            _ => auction.next_bid(),
        };
        (price > 0).then_some((session.npc, id, price))
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

    /// Whether this unit's replicated flags say it sells anything.
    ///
    /// Its own predicate beside [`Self::offers_training`], for the identical
    /// reason: the one place naming these bits is the place that can carry
    /// the evidence for what they mean. Innkeeper Farley's `0x10283` is the
    /// evidence here rather than for the trainer -- `0x283` is exactly
    /// `VENDOR (0x80) | VENDOR_FOOD (0x200) | QUESTGIVER (0x2) | GOSSIP
    /// (0x1)`, and she sells food and drink from behind the bar. All five
    /// vendor sub-flags are checked together rather than the base bit alone,
    /// because nothing here has yet seen a vendor of a *narrower* kind
    /// (ammunition, poison, reagents) carrying anything but its own bit --
    /// see [`Self::offers_training`]'s two-sided confirmation for why a
    /// single flag word is not proof enough on its own to lean on only one.
    fn is_vendor(&self, guid: u64) -> bool {
        /// `VENDOR | VENDOR_AMMO | VENDOR_FOOD | VENDOR_POISON |
        /// VENDOR_REAGENT`. See [`App::is_vendor`].
        const VENDOR_MASK: u32 = 0x80 | 0x100 | 0x200 | 0x400 | 0x800;
        self.live
            .as_ref()
            .and_then(|live| live.state.get(guid))
            .and_then(|entity| entity.npc_flags())
            .is_some_and(|flags| flags & VENDOR_MASK != 0)
    }

    /// Whether right-clicking this thing should open its loot.
    ///
    /// The mirror of [`Self::is_attack_candidate`] and deliberately as narrow:
    /// a dead unit, and not the player's own corpse. It makes no attempt to
    /// know whether there is anything *on* the body, because the client cannot
    /// know that until it asks -- and asking about an empty corpse is answered
    /// with a release rather than an error, so the cost of being wrong is one
    /// packet and no window.
    ///
    /// **`entity.lootable()` also qualifies a unit, on its own, and this is
    /// the other half of `is_attack_candidate`'s `foss-wow#141` fix.**
    /// *Milly's Harvest*'s grapes never satisfy `is_dead_or_ghost` -- their
    /// max health is genuinely zero, not merely unreplicated, and that guard
    /// cannot tell the two apart from the value alone. The lootable bit does
    /// not need to: it is the server's own statement that this is loot right
    /// now, independent of what the health fields say.
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
        matches!(entity.object_type, ::world::ObjectType::Unit)
            && (entity.is_dead_or_ghost() || entity.lootable())
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
    /// Asks the current target to trade.
    ///
    /// **The one request in this client whose success is invisible to its
    /// sender.** The server announces a started trade to the *partner* and
    /// says nothing back here until their client answers, so this opens
    /// nothing and draws nothing: the window appears when `OPEN_WINDOW`
    /// arrives, which is after somebody else has pressed a button.
    ///
    /// A local note is written all the same, because who was asked is nowhere
    /// on the wire -- the initiator is never told, and without this the window
    /// would open with no name in it a second after this client chose the
    /// name itself.
    ///
    /// Refused locally in the two cases this client can actually tell apart,
    /// and not in any of the others: the server has a dozen preconditions
    /// (distance, life, stun, faction, logging out) and reimplementing them
    /// here would be guessing at rules that differ per realm. What is checked
    /// is only that there *is* a target and that it is a player -- everything
    /// else is left to the server, which answers a refusal with a reason.
    fn initiate_trade(&mut self) {
        let Some(target) = self.target else {
            self.chat
                .push(Line::Chat(local_notice("Select somebody to trade with.".into())));
            return;
        };
        let is_player = self
            .live
            .as_ref()
            .and_then(|live| live.state.get(target))
            .is_some_and(|entity| entity.is_player());
        if !is_player {
            // Refused here rather than sent: the server answers a trade
            // request aimed at a creature with `NO_TARGET`, which is a real
            // reply and would be reported as one -- but "that is not a
            // person" is something this client knows for itself, and a round
            // trip to be told so is a round trip that reads as a bug.
            self.chat.push(Line::Chat(local_notice(
                "You can only trade with another player.".into(),
            )));
            return;
        }
        let Some(live) = self.live.as_mut() else { return };
        tracing::info!("asking {target:#018x} to trade");
        if let Err(e) = live.connection.initiate_trade(target) {
            tracing::warn!("asking to trade failed: {e:#}");
            return;
        }
        live.state.note_trade_request(target);
        // Said out loud, because the send is silent and the window does not
        // open until the other person's client answers: without this, pressing
        // the key does nothing observable for as long as they take to notice.
        let name = live
            .state
            .names
            .player(target)
            .flatten()
            .map(str::to_string)
            .unwrap_or_else(|| "them".into());
        self.chat.push(Line::Chat(local_notice(format!(
            "Waiting for {name} to answer."
        ))));
    }

    /// Answers an offer of a trade.
    ///
    /// Two opcodes rather than one with a flag, because that is what the
    /// protocol has -- and the server closes the trade on the refusal, so
    /// exactly one of these goes out per offer.
    fn answer_trade_offer(&mut self, answer: ui::frames::TradeOfferAnswer) {
        let Some(live) = self.live.as_mut() else { return };
        let sent = match answer {
            ui::frames::TradeOfferAnswer::Accept => live.connection.begin_trade(),
            ui::frames::TradeOfferAnswer::Decline => live.connection.busy_trade(),
        };
        match sent {
            // Cleared locally on a decline, because the server's own
            // `TRADE_CANCELED` is what would clear it and there is no reason
            // to leave a prompt on screen waiting for a packet that confirms a
            // decision the player has already made.
            Ok(()) => {
                if answer == ui::frames::TradeOfferAnswer::Decline {
                    live.state.trade = None;
                }
            }
            Err(e) => tracing::warn!("answering a trade offer failed: {e:#}"),
        }
    }

    /// Puts one carried item on the trade table.
    ///
    /// **The whole of the modal rule this milestone adds**, and it is one
    /// rule in one place: while a trade window is open, a right-click in the
    /// bag window offers the item rather than equipping or using it. That is
    /// deliberate and it does swallow the ordinary gesture -- which is the
    /// shape this project has a rule about -- so it is worth saying why it is
    /// the right trade here. A trade window is open for seconds and every one
    /// of them is somebody else waiting; an equip is undoable and a trade is
    /// not; and the offered item is visible in the window before anything is
    /// irreversible.
    ///
    /// Two things are refused before the send, both because the server
    /// **cancels the entire trade** rather than declining the request: an item
    /// already on the table, and a full table. Discovering either by sending
    /// costs the player the whole trade.
    fn offer_item(&mut self, at: Option<::world::inventory::Where>) -> bool {
        // **Every way out of here says which way it went.** A right-click that
        // offers nothing and a right-click that was never a trade gesture at
        // all are the same observation otherwise, and they want opposite
        // investigations -- the standing rule in `CLAUDE.md`, and this
        // function had four silent refusals when it was written. The first
        // live test came back "I couldn't give him an item", which is exactly
        // the sentence that cannot be acted on.
        let Some(at) = at else {
            tracing::debug!("right-click: no bag address for that square");
            return false;
        };
        let Some(live) = self.live.as_ref() else {
            return false;
        };
        let Some(session) = live.state.trade.as_ref() else {
            tracing::debug!("right-click at {at:?}: no trade open, so activating instead");
            return false;
        };
        if !session.open {
            tracing::debug!("right-click at {at:?}: trade offered but not open yet");
            return false;
        }
        let Some(carried) = ::world::inventory::carried(&live.state, live.guid)
            .into_iter()
            .find(|carried| carried.at == at)
        else {
            tracing::debug!("right-click at {at:?}: nothing carried there");
            return false;
        };
        let item = carried.item.guid;

        if session.already_offered(item) {
            self.chat.push(Line::Chat(local_notice(
                "That is already on the table.".into(),
            )));
            return true;
        }
        let Some(slot) = session.first_free_slot() else {
            self.chat
                .push(Line::Chat(local_notice("There is no room left to trade.".into())));
            return true;
        };

        let (bag, square) = at.address();
        let Some(live) = self.live.as_mut() else {
            return true;
        };
        tracing::info!("offering item {item:#x} (bag {bag}, slot {square}) in trade slot {slot}");
        if let Err(e) = live.connection.set_trade_item(slot, bag, square) {
            tracing::warn!("offering an item failed: {e:#}");
            return true;
        }
        // Recorded beside the send, because nothing will record it for us:
        // the server sends the offer to the *other* person and never back
        // here, so this is the only thing our half of the window is drawn
        // from. See `world::trade`.
        live.state.note_trade_item(slot, item);
        true
    }

    /// Sells one carried item to the open vendor.
    ///
    /// **The same modal rule `offer_item` established, on a vendor window
    /// instead of a trade one.** Reported from live play: while a vendor's
    /// stock is open, a right-click in the bag window has to sell rather
    /// than equip or use -- a player standing at a vendor deciding what to
    /// sell has already committed to that gesture meaning "sell", and
    /// treating it as "equip" would put the item on with nothing left to
    /// undo it with.
    fn sell_item_to_vendor(&mut self, at: Option<::world::inventory::Where>) -> bool {
        let Some(at) = at else {
            return false;
        };
        let Some(session) = self.vendor.as_ref() else {
            return false;
        };
        let npc = session.npc;
        let Some(live) = self.live.as_ref() else {
            return false;
        };
        let Some(carried) = ::world::inventory::carried(&live.state, live.guid)
            .into_iter()
            .find(|carried| carried.at == at)
        else {
            tracing::debug!("right-click at {at:?}: nothing carried there");
            return false;
        };
        let item = carried.item.guid;

        let Some(live) = self.live.as_mut() else {
            return true;
        };
        tracing::info!("selling item {item:#018x} to vendor {npc:#018x}");
        // Zero means the whole stack -- see `Connection::sell_item` -- which
        // is what a right-click with no quantity prompt anywhere in this
        // interface has to mean, the same choice `AutoStoreLootItem` and
        // every other one-click inventory gesture here makes.
        if let Err(e) = live.connection.sell_item(npc, item, 0) {
            tracing::warn!("selling item {item:#018x} failed: {e:#}");
        }
        true
    }

    /// Acts on a click in the trade window.
    fn act_on_trade(&mut self, click: ui::frames::TradeClick) {
        let Some(live) = self.live.as_mut() else { return };
        let Some(session) = live.state.trade.as_ref() else {
            // Logged rather than ignored: a click that reached the frame and
            // produced no send is the shape that reads as a broken window.
            tracing::warn!("the trade window was clicked with no trade open");
            return;
        };
        let token = session.token;
        match click {
            ui::frames::TradeClick::Clear(slot) => {
                if let Err(e) = live.connection.clear_trade_item(slot) {
                    tracing::warn!("taking an item back failed: {e:#}");
                    return;
                }
                live.state.note_trade_clear(slot);
            }
            ui::frames::TradeClick::Accept => {
                if let Err(e) = live.connection.accept_trade(token) {
                    tracing::warn!("accepting the trade failed: {e:#}");
                    return;
                }
                // Local: the server reports an accept to the *partner* and
                // says nothing to whoever made it until the trade completes.
                live.state.note_trade_accept();
            }
            ui::frames::TradeClick::Cancel => {
                if let Err(e) = live.connection.cancel_trade() {
                    tracing::warn!("cancelling the trade failed: {e:#}");
                    return;
                }
                // Cleared locally as well as by the `TRADE_CANCELED` that
                // follows. A window that stayed up until the packet arrived is
                // one the player has already told to go away.
                live.state.trade = None;
            }
        }
    }

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
        // Read, not re-eased: `self.drawn_own_z` was already advanced once
        // this frame by the draw pass, and calling `ease_towards` a second
        // time here would advance its state twice in one frame. Falls back
        // to the raw height only before the first draw has ever run.
        let own_position = glam::Vec3::new(
            live.position.x,
            live.position.y,
            self.drawn_own_z.unwrap_or(live.position.z),
        );
        let entities = drawable_with_own(
            live,
            own_position,
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

    /// Reads this character's questgiver cache, and remembers where to write
    /// it back.
    ///
    /// **Named by character as well as by realm** -- see
    /// `world::spawns::Questgivers::path_for`. Failing to load is not failing
    /// to run: without it the map starts empty and fills as the player walks,
    /// which is what it does on a new realm anyway.
    fn load_giver_cache(&mut self, realm: &str, character: &str) {
        let base = ui::default_path()
            .ok()
            .and_then(|path| path.parent().map(PathBuf::from));
        let Some(base) = base else {
            tracing::info!("no configuration directory -- questgivers will not be remembered");
            return;
        };
        let path = ::world::Questgivers::path_for(&base, realm, character);
        match ::world::Questgivers::load(&path) {
            Ok(cache) => {
                tracing::info!(
                    "{} questgiver(s) remembered for {character:?} on {realm:?} ({})",
                    cache.len(),
                    path.display()
                );
                self.givers = cache;
            }
            Err(error) => tracing::warn!("could not read {}: {error}", path.display()),
        }
        self.giver_cache_path = Some(path);
    }

    /// Writes the questgiver cache, if anything was learned.
    fn save_giver_cache(&mut self) {
        if !self.givers.is_dirty() {
            return;
        }
        let Some(path) = self.giver_cache_path.clone() else {
            return;
        };
        let count = self.givers.len();
        match self.givers.save(&path) {
            Ok(()) => tracing::info!("{count} questgiver(s) remembered in {}", path.display()),
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
        // The same shape, and guarded the same way. Two caches rather than one
        // because they answer questions with different lifetimes: what a quest
        // *is* is the realm's business and the same for everybody on it, and
        // what is over an NPC's head is one character's progress.
        if self.giver_cache_path.is_none() {
            if let Some((realm, character)) = self
                .live
                .as_ref()
                .map(|live| (live.realm.clone(), live.character.clone()))
            {
                self.load_giver_cache(&realm, &character);
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
        // Longer than the quest cache's, because this one goes dirty far more
        // readily: every new NPC that comes into range is news, so a walk
        // through a city dirties it continuously where a quest cache goes
        // quiet as soon as the log is described.
        const GIVER_SAVE_INTERVAL: Duration = Duration::from_secs(60);
        if self.giver_saved_at.elapsed() >= GIVER_SAVE_INTERVAL {
            self.giver_saved_at = Instant::now();
            self.save_giver_cache();
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
        // Same shape again: a vendor's stock, collected during the drain and
        // filed after it for the reason every other list in this pump is.
        let mut vendor_lists: Vec<::world::VendorList> = Vec::new();
        // Whether a guild event arrived that makes an open roster stale.
        // Collected rather than acted on inside the loop for the reason every
        // other flag here is: the connection is borrowed for the whole drain.
        let mut refresh_guild = false;
        let mut taxi_menus: Vec<::world::TaxiMenu> = Vec::new();
        // The auction block's three, collected for the same reason: the
        // connection is borrowed for the whole drain, so nothing inside the
        // loop may touch `self` again.
        let mut auction_house: Option<::world::AuctionHouse> = None;
        let mut auction_answered = false;
        let mut auction_outcomes: Vec<::world::AuctionOutcome> = Vec::new();
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
        let mut worldport_changed = false;
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
                match live::answer_worldport(&mut self.chain, live) {
                    Ok(true) => {
                        worldport_changed = true;
                        tracing::info!(
                            "world-port acknowledged at map {} ({:.1}, {:.1}, {:.1})",
                            live.map_id,
                            live.position.x,
                            live.position.y,
                            live.position.z
                        );
                    }
                    Ok(false) => {}
                    Err(e) => tracing::warn!("world-port acknowledgement failed: {e:#}"),
                }
                // **Mail arrived, and a sentence is the whole of what can
                // honestly be drawn for it.**
                //
                // `SMSG_RECEIVED_MAIL` is four bytes of zero: no sender, no
                // subject, no count. There is nothing to put in a widget, and
                // finding out what came needs a mailbox, which may be a
                // continent away. So the packet becomes a line of text, which
                // is exactly as much as it says.
                //
                // Said per packet rather than from the flag next to it,
                // because the flag cannot tell "one arrived just now" from
                // "one was already waiting" -- and the second is not news.
                for _ in 0..report.received_mail {
                    self.chat.push(Line::Chat(local_notice(
                        "You have new mail.".to_string(),
                    )));
                }

                // **Something happened to the guild.**
                //
                // Said as a line and never folded into the roster, because an
                // event says only *that* somebody signed on -- their level,
                // zone and notes are not in it. Editing a row from one would
                // be inventing the fields it does not carry, so the honest
                // response is to re-ask, and the window does that when it is
                // open.
                //
                // The types are named only where naming one is safe. A
                // sign-on and a sign-off carry the member's name in their
                // first parameter and are unambiguous; the rest print their
                // number, the same rule `describe_cast_failure` follows.
                for event in &report.guild_events {
                    let who = event.params.first().map(String::as_str).unwrap_or("");
                    let text = match event.kind {
                        ::world::guild::GuildEventType::SIGNED_ON => {
                            format!("{who} has come online.")
                        }
                        ::world::guild::GuildEventType::SIGNED_OFF => {
                            format!("{who} has gone offline.")
                        }
                        ::world::guild::GuildEventType::JOINED => {
                            format!("{who} has joined the guild.")
                        }
                        ::world::guild::GuildEventType::LEFT => {
                            format!("{who} has left the guild.")
                        }
                        ::world::guild::GuildEventType::MOTD => who.to_string(),
                        other => format!("guild event {other} {:?}", event.params),
                    };
                    self.chat.push(Line::Chat(local_notice(text)));
                    // Re-asked rather than edited, and only while somebody is
                    // looking: a roster nobody has open does not need to be
                    // current, and asking on every sign-on in a large guild
                    // would be a packet per member per login.
                    if self.guild_open {
                        refresh_guild = true;
                    }
                }
                // What the server said about a guild request.
                //
                // Drawn where the player is looking, for the reason the party
                // command result is: **the reply echoes the command**, so it
                // is the only thing tying an answer to a question in a block
                // where almost every request is otherwise silent.
                for result in &report.guild_results {
                    let text = ::world::guild::describe_command_result(
                        result.command,
                        result.result,
                        &result.name,
                    );
                    self.chat.push(Line::Chat(local_notice(text)));
                }
                // Somebody has asked this character to join a guild.
                //
                // A line rather than a prompt frame, and the cost is real: an
                // invitation times out, so this is weaker than the party
                // frame's two buttons. Named in the milestone's not-done list
                // rather than left looking finished.
                if let Some(invite) = live.state.guild_invitation.clone() {
                    if self.guild_invitation_said.as_deref() != Some(invite.guild.as_str()) {
                        self.guild_invitation_said = Some(invite.guild.clone());
                        self.chat.push(Line::Chat(local_notice(format!(
                            "{} invites you to join {}.  /gaccept or /gdecline",
                            invite.inviter, invite.guild
                        ))));
                    }
                } else {
                    self.guild_invitation_said = None;
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
                        // The auctioneer's greeting, and the **only packet
                        // in the block that names a house**. Folded into the
                        // session rather than only into the world state,
                        // because the window's title is what tells the player
                        // that walking to a different auctioneer changed which
                        // goods they are looking at.
                        ::world::opcode::server::AUCTION_HELLO => {
                            match ::world::auction::parse_auction_hello(&packet.body) {
                                Ok(house) => {
                                    tracing::debug!(
                                        "auctioneer {:#018x} serves house {} ({})",
                                        house.auctioneer,
                                        house.house,
                                        if house.enabled { "open" } else { "shut" }
                                    );
                                    auction_house = Some(house);
                                }
                                Err(error) => {
                                    tracing::warn!("an auction greeting would not parse: {error}")
                                }
                            }
                        }
                        // Any of the three lists. The state parses them; what
                        // is needed here is only that one arrived, so the
                        // window can stop saying it is waiting -- "asking" and
                        // "nothing matched" are the same picture and different
                        // facts.
                        ::world::opcode::server::AUCTION_LIST_RESULT
                        | ::world::opcode::server::AUCTION_OWNER_LIST_RESULT
                        | ::world::opcode::server::AUCTION_BIDDER_LIST_RESULT => {
                            auction_answered = true;
                        }
                        // What a post, a bid or a cancellation did. Every one
                        // of these is worth a chat line: the request that
                        // caused it was one the player pressed a button for,
                        // and a success and a refusal look identical from the
                        // outside.
                        ::world::opcode::server::AUCTION_COMMAND_RESULT => {
                            match ::world::auction::parse_command_result(&packet.body) {
                                Ok(outcome) => auction_outcomes.push(outcome),
                                Err(error) => {
                                    tracing::warn!("an auction result would not parse: {error}")
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
                        // A vendor's stock. Answered even for an NPC with
                        // nothing to sell -- see `world::vendor` -- so unlike
                        // the trainer's this one needs no silence to
                        // interpret.
                        ::world::opcode::server::LIST_INVENTORY => {
                            match ::world::vendor::parse_vendor_list(&packet.body) {
                                Ok(list) => {
                                    tracing::debug!(
                                        "vendor {:#018x} sells {} item(s)",
                                        list.vendor,
                                        list.items.len()
                                    );
                                    vendor_lists.push(list);
                                }
                                Err(error) => {
                                    tracing::warn!("a vendor list would not parse: {error}")
                                }
                            }
                        }
                        // The reward screen. **Not purely an event any
                        // more.** It usually arrives while the quest it is
                        // about is already showing -- the answer to an
                        // explicit `CMSG_QUESTGIVER_COMPLETE_QUEST` -- but a
                        // questgiver with exactly one thing to do, and
                        // nothing left to hand in, skips the gossip menu
                        // *and* the request-items screen and sends this as
                        // the very first reply to a greeting. `showing` was
                        // still `None` for that case, so nothing ever drew
                        // -- reported live as Eagan Peltskinner's window
                        // opening and staying completely empty. Filing the
                        // quest id here the same way `QUESTGIVER_QUEST_
                        // DETAILS` already does costs nothing when the quest
                        // was already on screen -- `note_quest_offered` just
                        // re-affirms what `showing` already held.
                        ::world::opcode::server::QUESTGIVER_OFFER_REWARD => {
                            match ::world::quest::parse_questgiver_event(
                                &packet.body,
                                "SMSG_QUESTGIVER_OFFER_REWARD",
                            ) {
                                Ok(event) => {
                                    tracing::debug!(
                                        "the reward screen is open for quest {}",
                                        event.quest
                                    );
                                    offered.push(event.quest);
                                }
                                Err(error) => {
                                    tracing::warn!("a reward screen would not parse: {error}")
                                }
                            }
                        }
                        // The same shape as the reward screen just above,
                        // and the same fix for the identical reason: a
                        // questgiver whose one quest is not yet ready to
                        // turn in -- still missing a kill count, say --
                        // likewise skips the gossip menu and sends this
                        // directly, and `showing` needs to be set for the
                        // window to draw anything at all rather than stay
                        // empty while quietly logging that nothing is
                        // wrong.
                        ::world::opcode::server::QUESTGIVER_REQUEST_ITEMS => {
                            match ::world::quest::parse_questgiver_event(
                                &packet.body,
                                "SMSG_QUESTGIVER_REQUEST_ITEMS",
                            ) {
                                Ok(event) => {
                                    tracing::debug!(
                                        "quest {} is not finished yet",
                                        event.quest
                                    );
                                    offered.push(event.quest);
                                }
                                Err(error) => {
                                    tracing::warn!("a request-items screen would not parse: {error}")
                                }
                            }
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
                                    // **And into the cache, which is a
                                    // different question.** `quest_marks` is
                                    // this frame's answer about an NPC in
                                    // range and is thrown away whenever the
                                    // log changes; the cache is what survives
                                    // the NPC walking out of view and the
                                    // client being shut down. Both are fed
                                    // from the one reply so they cannot
                                    // disagree about what the server said.
                                    self.givers.mark(status.npc, status.mark, unix_now());
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

        // **A guild event says a row changed and not what to.** So the
        // answer is to ask again rather than to edit, and only while the
        // window is open -- a roster nobody is looking at does not need to be
        // current, and re-asking on every sign-on in a large guild would be a
        // packet per member per login.
        //
        // Only the roster, not the name query beside it: the guild's name and
        // rank names do not change when somebody logs in, and they are
        // already cached from the first ask.
        if refresh_guild {
            if let Err(e) = live.connection.guild_roster() {
                tracing::warn!("re-asking the guild roster failed: {e:#}");
            }
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

        // Filed by guid, like the trainer's: two vendors can be in reach at
        // once, and a stock list filed against the wrong session would put
        // one NPC's wares in a window belonging to another.
        for list in vendor_lists {
            match self
                .vendor
                .as_mut()
                .filter(|session| session.npc == list.vendor)
            {
                Some(session) => session.list = Some(list),
                None => tracing::debug!(
                    "a vendor list for {:#018x} arrived with no window open for it",
                    list.vendor
                ),
            }
        }

        // **Filed by guid, like the trainer's**, and for a reason with more
        // teeth here: `.npc add` stacks auctioneers at a foot, two auctioneers
        // in a city can serve *different houses*, and nothing in a list result
        // or a bid says which house it belongs to. A greeting filed against
        // the wrong session would put one house's name over another house's
        // rows and no packet would ever say so.
        if let Some(house) = auction_house {
            match self
                .auction
                .as_mut()
                .filter(|session| session.npc == house.auctioneer)
            {
                Some(session) => session.house = Some(house.house),
                None => tracing::debug!(
                    "an auction greeting for {:#018x} arrived with no window open for it",
                    house.auctioneer
                ),
            }
        }
        // A list arrived. Which one is the world state's business; what the
        // window needs is only that the wait is over, because "asking" and
        // "nothing matched" are the same picture and different facts.
        if auction_answered {
            if let Some(session) = self.auction.as_mut() {
                session.waiting = false;
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
                    // **The windows close because the conversation is over.**
                    // Taking off is the flight master's answer, and a list of
                    // destinations left on screen while the character is in
                    // the air offers flights from a node they have already
                    // left -- which the server refuses, from a window that
                    // looks perfectly live. Closed on the takeoff rather than
                    // on the click, so a refused flight leaves the list up to
                    // try again with.
                    self.taxi = None;
                    self.questgiver = None;
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

        // **Which quests an NPC offers, recorded before the window is.** A
        // gossip menu names its own NPC and its quest ids, so this is the one
        // place in the client that learns the pair -- and it is what lets a
        // remembered exclamation be retired *precisely* when the quest behind
        // it is accepted, instead of every remembered mark being distrusted
        // the moment anything is. Outside the `questgiver` check on purpose:
        // the pairing is worth keeping whether or not a window is open.
        for gossip in &greetings {
            if !gossip.quests.is_empty() {
                let ids: Vec<u32> = gossip.quests.iter().map(|quest| quest.quest_id).collect();
                self.givers.offers(gossip.npc, &ids);
            }
        }

        let had_offer = !offered.is_empty();
        if let Some(questgiver) = self.questgiver.as_mut() {
            for gossip in &greetings {
                questgiver.note_gossip(gossip);
            }
            for quest in offered {
                questgiver.note_quest_offered(quest);
            }
        }
        // **Whatever a greeting or an unrequested offer just put on screen
        // needs the same two requests a clicked list row gets.** `note_gossip`
        // shows a menu of exactly one straight away, and `note_quest_offered`
        // is the server volunteering a quest with nobody having asked -- both
        // can set `showing` with no `CMSG_QUEST_QUERY` ever sent for it, which
        // otherwise leaves the window saying "Asking the server..." forever:
        // the only other place that sends it is the click handler for a
        // multi-quest list. Gated on this frame actually having produced a
        // greeting or an offer, so an open window does not resend the scroll
        // request every frame while it waits.
        if !greetings.is_empty() || had_offer {
            if let Some(quest) = self.questgiver.as_ref().and_then(|g| g.showing) {
                if let Some(npc) = self.questgiver.as_ref().map(|g| g.npc) {
                    if let Err(e) = live.connection.query_quest(npc, quest) {
                        tracing::warn!("asking for quest {quest}'s scroll failed: {e:#}");
                    }
                }
                for quest in self.quests.take_unknown(&[quest], 1) {
                    if let Err(e) = live.connection.query_quest_info(quest) {
                        tracing::warn!("asking what quest {quest} is failed: {e:#}");
                        self.quests.give_up(quest);
                    } else {
                        self.quest_asked_at.insert(quest, Instant::now());
                    }
                }
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
                hud::NameRequest::GameObject { entry, guid } => {
                    live.connection.ask_gameobject(entry, guid)
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
        // so they are named here rather than found by walking the bags. **A
        // trade partner's half of the table is the same case** and had to be
        // added for the same reason -- those entries belong to somebody else
        // and appear in nothing this character carries, so a window built off
        // the bags alone shows the other person's goods as bare numbers.
        let mut looted: Vec<u32> = live
            .state
            .loot
            .as_ref()
            .map(|loot| loot.items.iter().map(|item| item.entry).collect())
            .unwrap_or_default();
        looted.extend(
            live.state
                .trade
                .as_ref()
                .and_then(|session| session.theirs.as_ref())
                .into_iter()
                .flat_map(|offer| offer.items.iter().map(|item| item.entry)),
        );
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
        // **The remembered set gets the precise version of that same
        // reconciliation, and it must.** Clearing it wholesale would empty the
        // map every time the player accepted anything, because most of what is
        // in it is miles away and cannot be re-asked. So only the NPCs this
        // client has actually *seen* offering a quest now in the log lose
        // their mark. See `world::spawns::Questgivers::forget_offering`.
        if self.giver_marks_log != log {
            self.giver_marks_log = log.clone();
            let retired = self.givers.forget_offering(&log);
            if retired > 0 {
                tracing::debug!("{retired} remembered questgiver(s) have nothing left to offer");
            }
        }
        // Where every talkable NPC in range is standing. Cheap and
        // unconditional: `see` only dirties the cache for a creature that is
        // new or has actually moved, which is what keeps a city walk from
        // rewriting the file every frame.
        let map_id = live.map_id;
        let sightings: Vec<(u64, u32, f32, f32, f32)> = live
            .state
            .iter()
            .filter(|entity| entity.guid != live.guid && entity.will_talk())
            .filter_map(|entity| {
                let entry = entity.entry()?;
                let at = entity.position?;
                Some((entity.guid, entry, at.x, at.y, at.z))
            })
            .collect();
        for (guid, entry, x, y, z) in sightings {
            self.givers.see(guid, entry, map_id, x, y, z);
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

        // **A mailbox closes itself when the character walks away from it.**
        //
        // Every mail request names the mailbox and the server re-checks the
        // reach on each one, refusing by doing nothing at all. So a window
        // left open past the walk is a window whose clicks are dropped in
        // silence, which is the one failure this client cannot diagnose --
        // and the fix is not to explain it afterwards but to not offer it.
        //
        // Measured from `live.position`, which is where this client thinks it
        // is. Replicated state holds our *login* position forever, and a
        // window that closed as soon as the character walked ten units from
        // the spawn would be the fifth caller to relearn that.
        let standing = live.position;
        if self.mailbox.is_some() && !Self::mailbox_in_reach(&live.state, self.mailbox, standing) {
            tracing::info!("mailbox closed: out of reach");
            self.mailbox = None;
        }
        // And the auction window, for the identical reason and with the same
        // measurement: every request in the block resolves its auctioneer
        // through `GetNPCIfCanInteractWith`, which refuses past five units --
        // **in silence**. A window left open after the player walked away is a
        // window whose every button does nothing and says nothing.
        if let Some(npc) = self.auction.as_ref().map(|session| session.npc) {
            const REACH: f32 = 8.0;
            let near = live
                .state
                .get(npc)
                .and_then(|entity| entity.position)
                .is_some_and(|at| {
                    let (dx, dy, dz) = (
                        at.x - standing.x,
                        at.y - standing.y,
                        at.z - standing.z,
                    );
                    dx * dx + dy * dy + dz * dz <= REACH * REACH
                });
            if !near {
                tracing::info!("auction window closed: the auctioneer is out of reach");
                self.auction = None;
            }
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

        // **Last, because these take `&mut self`** -- a notice and a re-ask
        // both do -- and the connection above is borrowed out of `self` for
        // the whole of this function.
        for outcome in auction_outcomes {
            let line = if outcome.succeeded() {
                format!("auction {}: {} accepted", outcome.auction, outcome.action.label())
            } else {
                // The number as well as the words, because only the codes this
                // project has actually observed have words -- everything else
                // comes back as its number rather than as a name nobody can
                // check.
                format!(
                    "auction {}: {} refused -- {} ({})",
                    outcome.auction,
                    outcome.action.label(),
                    ::world::auction::describe_auction_error(outcome.error),
                    outcome.error
                )
            };
            tracing::info!("{line}");
            self.notice(line);
            // Re-asked rather than edited in place, the same decision the
            // trainer list makes after a purchase: a bid changes the minimum
            // increment, the bidder and possibly whether the auction still
            // exists, and only the server knows which.
            if outcome.succeeded() {
                self.ask_auctions();
            }
        }
        if worldport_changed {
            self.reload_live_world();
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
        let text_started = Instant::now();

        // **Asked once, not sixty times a second.** `Gpu::describe` calls
        // `adapter.get_info()`, which is a driver query, and the answer is a
        // fact about the machine that cannot change while the session runs.
        // It was being rebuilt every frame to print one unchanging line in
        // the debug window.
        let gpu_line = self
            .gpu_line
            .get_or_insert_with(|| r.gpu.describe())
            .clone();
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
        let (error, frame_ms, fps) = (self.error.clone(), self.frame_ms, self.fps);
        // The previous frame's, necessarily -- this one has not been drawn
        // yet. That is the honest reading and not a lag worth hiding: a
        // number a frame old is a number about a frame that finished, and
        // smoothing it to look current would be the same mistake as sorting
        // an absent distance as zero.
        let profile = self.profile.describe();
        self.ui_text_ms = text_started.elapsed().as_secs_f32() * 1000.0;
        // **Everything between here and the egui pass.** `build_ui` snapshots
        // the whole interface's inputs -- player and target frames, action
        // bars, chat, quest marks, loot markers, the spellbook, the bags --
        // into owned values before the closure, because the closure cannot
        // borrow `self` while egui mutates it. That snapshot is over a
        // thousand lines of cloning and formatting and it runs every frame,
        // and until this timer existed it was inside `ui_ms` with no way to
        // see it: `hud`, `stats` and `text` together came to 0.43 ms of a
        // 3.28 ms phase, so the rest of it is here.
        let snapshot_started = Instant::now();
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
            let mut view = match live.state.get(guid) {
                Some(entity) => Some(hud::unit_view(entity, hud::unit_name(&live.state, entity))),
                // Out of visibility range is not out of the group: a party
                // member's frame falls back to the party packet, and the
                // target frame agreeing with it is the whole point of
                // targeting one by clicking their row rather than the world.
                None => hud::party_target_view(&live.state, guid),
            };
            // Combo points name their own target on the wire, so this is the
            // one place that can tell whether they belong on *this* frame --
            // switching targets does not clear `WorldState::combo_points`
            // itself (nothing tells this client to), so a stale count against
            // whatever was targeted before must not leak onto the new target.
            if let (Some(view), Some(combo)) = (&mut view, live.state.combo_points) {
                if combo.target == guid {
                    view.combo_points = Some(combo.count);
                }
            }
            view
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
        // **The four blocks that run whatever is on screen.** Every panel
        // below is gated on being open -- spellbook, bags, character, quest
        // log, map -- so a closed one costs nothing. These are not: world
        // markers, the action bars, the map/minimap group and whatever is
        // left. 2.74 ms goes somewhere in here and four timers name it
        // without anyone having to read a thousand lines and pick a
        // favourite, which is how the last five guesses went.
        let timing_markers = Instant::now();
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
        let status_text = self.status_text.as_ref().and_then(|entry| {
            let elapsed = now.saturating_duration_since(entry.spawned).as_secs_f32();
            (elapsed < ui::ACTION_STATUS_FADE_TIME).then(|| ui::StatusText {
                text: entry.text.clone(),
                elapsed,
            })
        });

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
        self.ui_markers_ms = timing_markers.elapsed().as_secs_f32() * 1000.0;
        let timing_bars = Instant::now();
        let mut bars: Vec<Vec<ui::frames::action_bar::SlotView>> = Vec::new();
        for bar in 0..ui::frames::action_bar::BARS {
            let mut slots = Vec::with_capacity(ui::frames::action_bar::SLOTS);
            for slot in 0..ui::frames::action_bar::SLOTS {
                let spell = self.hud.profile.bars.get(bar, slot).map(|id| {
                    let icon = self.spells.icon(&r.gpu, &mut r.egui_renderer, &mut self.chain, id);
                    // Auto-attack is a toggle, not a cast, and never sits
                    // behind the global cooldown -- see
                    // `WorldState::start_global_cooldown`'s doc comment. Its
                    // own sweep would otherwise flash every time an unrelated
                    // spell is cast, which reads as "attacking is on
                    // cooldown" and is not a sentence that means anything.
                    let cooldown_fraction = self
                        .live
                        .as_ref()
                        .map(|live| {
                            let own = live.state.cooldown_fraction(id, now);
                            if id == spells::AUTO_ATTACK {
                                own
                            } else {
                                own.max(live.state.global_cooldown_fraction(now))
                            }
                        })
                        .unwrap_or(0.0);
                    let press_fraction = match self.action_flash {
                        Some(((f_bar, f_slot), pressed)) if (f_bar, f_slot) == (bar, slot) => {
                            let elapsed = now.saturating_duration_since(pressed);
                            1.0 - (elapsed.as_secs_f32() / ACTION_FLASH.as_secs_f32()).min(1.0)
                        }
                        _ => 0.0,
                    };
                    // The one persistent state a slot can be in, as opposed to
                    // a momentary flash: auto-attack stays "on" for as long as
                    // the character is swinging, which a 200ms flash cannot
                    // say. See `ui::frames::action_bar::SlotSpell::active`.
                    let active = id == spells::AUTO_ATTACK
                        && self.live.as_ref().is_some_and(|live| {
                            live.state.attacking.contains_key(&live.guid)
                        });
                    ui::frames::action_bar::SlotSpell {
                        id,
                        name: self.spells.name(id),
                        rank: self.spells.rank(id),
                        description: self.spells.description(id),
                        icon,
                        cooldown_fraction,
                        press_fraction,
                        active,
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
        // The trade window, and the prompt that offers one.
        //
        // **Built from two different kinds of thing**, which is what makes
        // this window unlike every other one here. Their half is a packet.
        // Our half is this client's own record of what it put down, because
        // the server sends an offer to the *partner* and never back to its
        // author -- so `session.ours` is item guids, and turning them into
        // pictures means looking each one up in this character's own
        // inventory. See `world::trade`.
        self.ui_bars_ms = timing_bars.elapsed().as_secs_f32() * 1000.0;
        let timing_panels = Instant::now();
        let mut trade_offer: Option<ui::frames::TradeOfferView> = None;
        let trade: Option<ui::TradeView> = match self.live.as_ref().and_then(|live| {
            live.state
                .trade
                .as_ref()
                .map(|session| (session.clone(), live.guid))
        }) {
            Some((session, own_guid)) => {
                // The partner's name, or nothing. Never a guid: a window
                // headed with a hex number is one that says the client has
                // failed rather than that a query is outstanding.
                let partner = session
                    .partner
                    .and_then(|guid| {
                        self.live
                            .as_ref()
                            .and_then(|live| live.state.names.player(guid).flatten())
                            .map(str::to_string)
                    })
                    .unwrap_or_default();

                if session.awaiting_our_answer() {
                    // An offer nobody has answered yet. The window itself is
                    // not drawn: there is nothing in it, and a table with no
                    // squares to fill would look like a trade already under
                    // way.
                    trade_offer = Some(ui::frames::TradeOfferView { from: partner });
                    None
                } else if !session.open {
                    // **This end asked and is waiting.** Nothing is drawn --
                    // and specifically *not* the prompt, which is the mistake
                    // the two ends holding identical state invites: before the
                    // window opens, the only thing separating "waiting for
                    // them" from "they are waiting for me" is which end
                    // pressed the key, and that is nowhere on the wire. A
                    // prompt here would ask the player whether they accept
                    // their own request.
                    None
                } else {
                    let mut view = ui::TradeView {
                        partner,
                        their_money: session.theirs.as_ref().map(|o| o.money).unwrap_or(0),
                        our_money: session.our_money,
                        they_accepted: session.partner_accepted,
                        we_accepted: session.accepted,
                        ..Default::default()
                    };

                    if let Some(offer) = session.theirs.as_ref() {
                        for item in &offer.items {
                            let Some(square) = view.theirs.get_mut(item.slot as usize) else {
                                continue;
                            };
                            let name =
                                Self::item_name(self.live.as_ref(), &self.items, item.entry);
                            let icon = self.items.icon(
                                &r.gpu,
                                &mut r.egui_renderer,
                                &mut self.chain,
                                item.entry,
                            );
                            square.item = Some(ui::frames::TradeSquareItem {
                                count: item.count,
                                label: name,
                                icon,
                            });
                        }
                    }

                    // Our half, resolved guid by guid against what this
                    // character is carrying. An item whose object has not
                    // replicated still draws as occupied -- the same
                    // distinction the bag window keeps, and for the same
                    // reason: a replication gap must not read as an empty
                    // square in a window about who owns what.
                    let carried = self
                        .live
                        .as_ref()
                        .map(|live| ::world::inventory::held(&live.state, own_guid))
                        .unwrap_or_default();
                    for (slot, held) in session.ours.iter().enumerate() {
                        let Some(guid) = *held else { continue };
                        let found = carried.iter().find(|item| item.guid == guid);
                        let entry = found.and_then(|item| item.entry).unwrap_or(0);
                        let count = found.map(|item| item.count).unwrap_or(1);
                        let name = if entry == 0 {
                            // Never blank: an unnamed square and an empty one
                            // must not look alike in this window.
                            format!("item {guid:#x}")
                        } else {
                            Self::item_name(self.live.as_ref(), &self.items, entry)
                        };
                        let icon = (entry != 0)
                            .then(|| {
                                self.items.icon(
                                    &r.gpu,
                                    &mut r.egui_renderer,
                                    &mut self.chain,
                                    entry,
                                )
                            })
                            .flatten();
                        if let Some(square) = view.ours.get_mut(slot) {
                            square.item =
                                Some(ui::frames::TradeSquareItem {
                                    count,
                                    label: name,
                                    icon,
                                });
                        }
                    }
                    Some(view)
                }
            }
            None => None,
        };

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

        let vendor: Option<ui::VendorView> = self.vendor.as_ref().map(|session| {
            let rows = session
                .list
                .as_ref()
                .map(|list| {
                    list.items
                        .iter()
                        .map(|item| {
                            let icon = self.items.icon(
                                &r.gpu,
                                &mut r.egui_renderer,
                                &mut self.chain,
                                item.entry,
                            );
                            let name = Self::item_name(self.live.as_ref(), &self.items, item.entry);
                            ui::VendorRow {
                                slot: item.slot,
                                entry: item.entry,
                                name,
                                price: item.price,
                                buy_count: item.buy_count,
                                remaining: item.remaining,
                                icon,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            ui::VendorView {
                name: session.name.clone(),
                rows,
            }
        });

        // The mailbox. Built here beside the trainer's because it needs the
        // renderer for the attachment icons, and rebuilt every frame from
        // `WorldState::mail` rather than kept: the inbox is replaced whole by
        // every `SMSG_MAIL_LIST_RESULT`, and a retained copy would draw a
        // letter that has already been emptied as though it still held
        // something.
        // Two passes on purpose. The first reads replicated state and the
        // name cache; the second asks the icon cache, which wants the GPU and
        // the archive chain. Doing both at once needs `&World` and `&mut
        // Items` out of the same `self` inside one closure, and the rows are
        // cheap to carry across.
        let mail: Option<ui::MailView> = self.mailbox.map(|_| {
            let Some(live) = self.live.as_ref() else {
                return ui::MailView::default();
            };
            let Some(inbox) = live.state.mail.as_ref() else {
                // Open, asked, nothing back yet. An empty view rather than no
                // window: the request is always answered, so this state lasts
                // one round trip, and a window that waited for the reply
                // would make a slow realm look like a click that missed.
                return ui::MailView::default();
            };
            let rows: Vec<ui::MailRow> = inbox
                .mails
                .iter()
                .map(|letter| ui::MailRow {
                    id: letter.id,
                    sender: mail_sender_label(&live.state, letter.sender),
                    subject: letter.subject.clone(),
                    body: letter.body.clone(),
                    money: letter.money,
                    attachments: letter
                        .items
                        .iter()
                        .map(|item| ui::MailAttachment {
                            count: item.count,
                            // Filled in below: the entry travels as the icon
                            // for one pass so that the GPU is not needed
                            // while replicated state is borrowed.
                            icon: None,
                        })
                        .collect(),
                    read: letter.is_read(),
                    days_left: letter.days_left,
                    state: if letter.has_anything() {
                        ui::MailRowState::Collectable
                    } else {
                        ui::MailRowState::Empty
                    },
                })
                .collect();
            ui::MailView {
                rows,
                // Named because nothing else in the protocol names them: the
                // list is capped at fifty and again by the packet size, and
                // the letters that did not fit are the oldest ones.
                withheld: inbox.withheld(),
            }
        });
        // The guild roster.
        //
        // **Every field here comes from one packet and has no second source.**
        // A replicated player's level can be checked against their object and
        // a party member's against theirs; a guild member who logged out on
        // Tuesday is whatever `SMSG_GUILD_ROSTER` said, so nothing is filled
        // in, defaulted or inferred -- including the zone, which is resolved
        // to a name only where `AreaTable` has one and left as a number where
        // it does not.
        let guild: Option<ui::GuildView> = self.guild_open.then(|| {
            let Some(live) = self.live.as_ref() else {
                return ui::GuildView::default();
            };
            let Some(roster) = live.state.guild_roster.as_ref() else {
                // Open, asked, nothing back yet. `CMSG_GUILD_ROSTER` is
                // answered either way, so this lasts one round trip -- and a
                // window that waited for the reply would make a slow realm
                // look like a key press that missed.
                return ui::GuildView::default();
            };
            let guild_id = live
                .state
                .get(live.guid)
                .and_then(|player| player.fields.get(::world::update::fields::PLAYER_GUILDID))
                .unwrap_or(0);
            let info = live.state.guilds.get(&guild_id);
            let rows = roster
                .members
                .iter()
                .map(|member| ui::GuildRow {
                    name: member.name.clone(),
                    level: member.level,
                    // The rank's *name* is in the other packet. Until it
                    // arrives the number is drawn, which is honest -- a blank
                    // column would read as a rank with no name rather than as
                    // one not yet asked about.
                    rank: info
                        .and_then(|info| info.ranks.get(member.rank as usize))
                        .cloned()
                        .unwrap_or_else(|| format!("rank {}", member.rank)),
                    // Exactly one of these two is ever set, because the packet
                    // writes exactly one: the offline float is absent for a
                    // member who is logged in.
                    zone: member.is_online().then(|| {
                        self.maps
                            .area_name(member.area)
                            .unwrap_or_else(|| format!("area {}", member.area))
                    }),
                    offline_days: member.offline_days,
                    public_note: member.public_note.clone(),
                    officer_note: member.officer_note.clone(),
                })
                .collect();
            ui::GuildView {
                name: info.map(|info| info.name.clone()).unwrap_or_default(),
                motd: roster.motd.clone(),
                rows,
                // The packet says why its own column is empty: the reader's
                // rank is on their own row and the rank's rights are in the
                // same body, so "hidden" and "none" are separable here and
                // are drawn differently.
                officer_notes: match roster.officer_notes_visible(live.guid) {
                    Some(true) => ui::OfficerNotes::Visible,
                    Some(false) => ui::OfficerNotes::Hidden,
                    None => ui::OfficerNotes::Unknown,
                },
            }
        });
        // The attachment entries, in the same order the rows were built, so
        // the second pass can pair them up without borrowing the world again.
        let attachment_entries: Vec<Vec<u32>> = self
            .live
            .as_ref()
            .and_then(|live| live.state.mail.as_ref())
            .map(|inbox| {
                inbox
                    .mails
                    .iter()
                    .map(|letter| letter.items.iter().map(|item| item.entry).collect())
                    .collect()
            })
            .unwrap_or_default();
        let mail = mail.map(|mut view| {
            for (row, entries) in view.rows.iter_mut().zip(&attachment_entries) {
                for (slot, entry) in row.attachments.iter_mut().zip(entries) {
                    slot.icon =
                        self.items
                            .icon(&r.gpu, &mut r.egui_renderer, &mut self.chain, *entry);
                }
            }
            view
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
        self.ui_panels_ms = timing_panels.elapsed().as_secs_f32() * 1000.0;
        let timing_map = Instant::now();
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
        // Every remembered questgiver on the continent the character is on.
        //
        // **Filtered to the map here rather than in either frame**, because
        // the cache holds every continent at once and two of them share a
        // coordinate range -- a Kalimdor NPC drawn on an Elwynn page would
        // land somewhere entirely plausible. The world map narrows it again by
        // page rectangle and the minimap by the disc; both need the continent
        // settled first.
        let giver_pins: Vec<maps::QuestgiverPin> = self
            .live
            .as_ref()
            .map(|live| {
                self.givers
                    .on_map(live.map_id)
                    .map(|known| maps::QuestgiverPin {
                        // The creature's name where one has ever arrived, and
                        // its entry where none has. **Never an invented
                        // name** -- `creature 823` is checkable and visibly
                        // unfinished, and a plausible wrong one is believed.
                        label: live
                            .state
                            .names
                            .creature(known.entry)
                            .flatten()
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("creature {}", known.entry)),
                        x: known.x,
                        y: known.y,
                        // A `?` for a quest of theirs that is finished, a `!`
                        // for one to take. `Incomplete` -- a quest of theirs
                        // in the log and not done -- is deliberately drawn as
                        // a turn-in too: it is the same NPC to come back to,
                        // and the difference between "come back later" and
                        // "come back now" is what the tracker's own counters
                        // are for.
                        turn_in: matches!(
                            known.mark,
                            Some(::world::quest::QuestgiverMark::Complete)
                                | Some(::world::quest::QuestgiverMark::Incomplete)
                        ),
                        live: known.live,
                    })
                    .collect()
            })
            .unwrap_or_default();

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
        // The sub-zone the surface says the character is in -- a WMO floor
        // outranks the terrain beneath it, while open ground still uses the
        // terrain chunk's finer area.
        let area_name = r
            .scene
            .as_ref()
            .and_then(|scene| match scene {
                Scene::Streaming(world) => self
                    .live
                    .as_ref()
                    .and_then(|live| world.area_at_position(live.position, STEP_HEIGHT)),
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
                &giver_pins,
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
            let interior = self.live.as_ref().and_then(|live| {
                r.scene.as_ref().and_then(|scene| match scene {
                    Scene::Streaming(world) => {
                        world.wmo_minimap_at_position(live.position, STEP_HEIGHT)
                    }
                    _ => None,
                })
            });
            self.minimap.build_view(
                &r.gpu,
                &mut r.egui_renderer,
                &mut self.chain,
                standing,
                self.live.as_ref().map(|live| live.map_directory.as_str()).unwrap_or_default(),
                area_name.as_deref(),
                interior.as_ref(),
                range,
                &objectives,
                &giver_pins,
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
        self.ui_map_ms = timing_map.elapsed().as_secs_f32() * 1000.0;
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
                    let tooltip = Self::item_tooltip(
                        self.live.as_ref(),
                        &mut self.spells,
                        &mut self.chain,
                        &self.maps,
                        entry,
                    );
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
                            quality: tooltip.quality,
                            item_level: tooltip.item_level,
                            required_level: tooltip.required_level,
                            description: tooltip.description,
                            armor: tooltip.armor,
                            weapon: tooltip.weapon,
                            stats: tooltip.stats,
                            use_description: tooltip.use_description,
                        }),
                    };
                    bags_where[index] = Some(carried.at);
                }
            }
            slots
        } else {
            Vec::new()
        };
        // **Built here rather than beside the other views**, because it needs
        // the renderer for its icons and the renderer is borrowed out of
        // `self` for this whole block: everything after this point takes
        // `&self`, which would extend that borrow past them.
        //
        // Disjoint fields rather than `&mut self` for the same reason -- an
        // icon needs the archive chain and the egui renderer at once.
        let auction_view = Self::auction_view(
            self.auction.as_ref(),
            self.live.as_ref(),
            &mut self.items,
            &mut self.chain,
            &r.gpu,
            &mut r.egui_renderer,
        );
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
                        let tooltip = Self::item_tooltip(
                            self.live.as_ref(),
                            &mut self.spells,
                            &mut self.chain,
                            &self.maps,
                            entry,
                        );
                        let icon = (entry != 0).then(|| {
                            self.items
                                .icon(&r.gpu, &mut r.egui_renderer, &mut self.chain, entry)
                        });
                        ui::frames::BagItem {
                            entry,
                            name,
                            count: held.count,
                            icon: icon.flatten(),
                            quality: tooltip.quality,
                            item_level: tooltip.item_level,
                            required_level: tooltip.required_level,
                            description: tooltip.description,
                            armor: tooltip.armor,
                            weapon: tooltip.weapon,
                            stats: tooltip.stats,
                            use_description: tooltip.use_description,
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

        let tracker = self.tracker_view();

        let mut hud_response = ui::HudResponse::default();
        let spellbook_open = self.spellbook_open;
        let bags_open = self.bags_open;
        let character_open = self.character_open;
        let quest_log_open = self.quest_log_open;
        let selected_quest = self.selected_quest;
        let interface = &mut self.hud;
        let editing = interface.edit.active;
        let layout_status = interface.status.clone();

        // **Two `Cell`s rather than two more phases threaded through the
        // frame.** The egui pass is one closure that borrows most of the
        // interface, so a timer inside it cannot write to `self`; and the
        // question -- is the cost the game's interface or the stats window
        // sitting on top of it -- needs exactly two numbers to answer. The
        // debug window is the suspect: it re-lays out a paragraph of text
        // that changes every frame, and text layout is one of egui's more
        // expensive operations. `ui_text` already ruled out *building* those
        // strings at 0.10 ms; laying them out is a different question and
        // this is where the answer is.
        self.ui_snapshot_ms = snapshot_started.elapsed().as_secs_f32() * 1000.0;

        let hud_ms = std::cell::Cell::new(0.0f32);
        let stats_ms = std::cell::Cell::new(0.0f32);
        let egui_started = Instant::now();
        let output = ctx.run_ui(input, |ctx| {
            let drawing_hud = Instant::now();
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
                    status_text: status_text.as_ref(),
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
                    vendor: vendor.as_ref(),
                    mail: mail.as_ref(),
                    guild: guild.as_ref(),
                    auction: auction_view.as_ref(),
                    trade: trade.as_ref(),
                    trade_offer: trade_offer.as_ref(),
                    taxi: taxi_view.as_ref(),
                    // `None` when shut, like the spellbook and the log.
                    world_map: map_view.as_ref(),
                    // No flag: a minimap is never opened or shut.
                    minimap: Some(&minimap_view),
                    // Nor is the tracker. `None` only before there is a
                    // world -- see `HudData::tracker`.
                    tracker: tracker.as_ref(),
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

            hud_ms.set(drawing_hud.elapsed().as_secs_f32() * 1000.0);

            let drawing_stats = Instant::now();
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
                        "BC compression: {} | {fps:.0} fps | {frame_ms:.1} ms/frame | {pipelines} pipelines",
                        if bc { "yes" } else { "no" }
                    ));
                    ui.label(egui::RichText::new(&profile).monospace());
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
            stats_ms.set(drawing_stats.elapsed().as_secs_f32() * 1000.0);
        });
        // The whole pass, closure included: subtracting the two below
        // leaves what egui itself spends beginning and ending it.
        self.ui_egui_ms = egui_started.elapsed().as_secs_f32() * 1000.0;
        self.ui_hud_ms = hud_ms.get();
        self.ui_stats_ms = stats_ms.get();

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

        // **`foss-wow#140`.** The window used to close only when the server
        // said the corpse was empty, which left no way to walk away from one
        // with items still on it. `release_loot` already existed for exactly
        // this -- its own doc comment says "sent on closing the window" --
        // it just had no caller until the window grew a button to press.
        if hud_response.loot_closed {
            self.release_loot();
        }

        // A right-clicked bag square auto-equips whatever is there. `index`
        // is a square position, not a slot -- resolved back through
        // `bags_where`, built alongside `bags` a moment ago.
        if let Some(index) = hud_response.activate_item {
            let at = bags_where.get(index).copied().flatten();
            // **Modal, and deliberately so.** While a trade window is open a
            // right-click puts the item on the table instead of equipping or
            // using it, and while a vendor's stock is open it sells instead.
            // Both return whether they took the gesture, so the ordinary
            // path is not also run -- a click that both offered an item and
            // equipped it would be two requests from one press, and the
            // second would cancel the trade the first started.
            if !self.offer_item(at) && !self.sell_item_to_vendor(at) {
                self.activate_item(at);
            }
        }

        // **Reported from live play**: a bag item dragged out of the window
        // and dropped over open ground used to just sit there, stuck to the
        // cursor, with no way to let go of it. The confirmation prompt
        // stands between that drop and this -- `destroy_item` is only ever
        // set once the player has pressed Destroy on it. `index` is the same
        // square-position-not-a-slot `activate_item` resolves above.
        if let Some(index) = hud_response.destroy_item {
            let at = bags_where.get(index).copied().flatten();
            let carried = at.zip(self.live.as_ref()).and_then(|(at, live)| {
                ::world::inventory::carried(&live.state, live.guid)
                    .into_iter()
                    .find(|carried| carried.at == at)
            });
            match carried {
                Some(carried) => {
                    let (bag, slot) = carried.at.address();
                    // The whole stack: nothing in this interface offers a
                    // quantity to destroy only part of one.
                    let count = carried.item.count.min(u8::MAX as u32) as u8;
                    tracing::info!("destroying item at bag {bag} slot {slot} (count {count})");
                    if let Some(live) = self.live.as_mut() {
                        if let Err(e) = live.connection.destroy_item(bag, slot, count) {
                            tracing::warn!("destroying an item failed: {e:#}");
                        }
                    }
                }
                // Moved, sold, already gone by the time the player answered
                // the prompt -- nothing left to destroy, and nothing wrong
                // either.
                None => tracing::debug!(
                    "destroy confirmed for row {index}, but nothing is carried there any more"
                ),
            }
        }

        if let Some(click) = hud_response.trade {
            self.act_on_trade(click);
        }
        if let Some(answer) = hud_response.trade_offer {
            tracing::info!("trade offer answered: {answer:?}");
            self.answer_trade_offer(answer);
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

        // **A tracker click opens the log at that quest and never closes
        // it.** The two paths are separate for exactly this reason: a click
        // in the log toggles a highlight, and one on the tracker is a request
        // to go and read the thing. Making the tracker toggle too would mean
        // clicking a tracked quest while the log was open shut the log, which
        // is the opposite of what the gesture asks for.
        if let Some(quest) = hud_response.tracker_quest {
            self.quest_log_open = true;
            self.selected_quest = Some(quest);
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

        // A vendor row was clicked. Same reasoning as the trainer row above:
        // the window only reports rows still in stock, and both the slot and
        // the entry are carried from the row rather than invented, because
        // the server checks the two still agree.
        if let Some((slot, entry)) = hud_response.buy_item {
            let npc = self.vendor.as_ref().map(|session| session.npc);
            match (npc, self.live.as_mut()) {
                (Some(npc), Some(live)) => {
                    tracing::debug!("buying item {entry} (slot {slot}) from {npc:#018x}");
                    if let Err(e) = live.connection.buy_item(
                        npc,
                        slot,
                        entry,
                        1,
                        ::world::inventory::OWN_SLOT_ARRAY,
                    ) {
                        tracing::warn!("buying item {entry} failed: {e:#}");
                    }
                }
                _ => tracing::warn!("a vendor row was clicked with no vendor open"),
            }
        }

        // A letter was clicked. The window reports only letters it said had
        // something in them, and it carries the **mail id** rather than a row
        // position -- the inbox is filtered, so positions do not close up.
        if let Some(id) = hud_response.take_mail {
            tracing::debug!("taking everything out of mail {id}");
            self.take_mail(id);
        }

        // A guild member was clicked.
        //
        // **The window reports only members who are online**, and it reports
        // a *name* rather than a row -- which is not merely safer here, it is
        // the only handle there is: every guild request in the protocol names
        // a player by name, and a roster guid is no use for whispering.
        //
        // Switching the sticky channel rather than sending anything: the
        // click says who to talk to, not what to say, and a client that
        // silently sent something would be inventing the message.
        if let Some(name) = hud_response.whisper_guild_member.clone() {
            tracing::debug!("whispering guild member {name}");
            self.chat_channel = ChatChannel::Whisper(name);
        }

        // The auction window's three answers, in the order the frame reports
        // them. Each logs, because every one of them can end in a request the
        // server drops without a word.
        if let Some(tab) = hud_response.auction_tab {
            if let Some(session) = self.auction.as_mut() {
                if session.tab != tab {
                    tracing::debug!("auction window switched to {tab:?}");
                    session.tab = tab;
                    // Back to the start of the match: an offset carried across
                    // a tab change would ask the bidder list for row 96, which
                    // it reads and ignores, and label the answer page nine.
                    session.offset = 0;
                    session.selected = None;
                    self.ask_auctions();
                }
            }
        }
        if let Some(id) = hud_response.select_auction {
            tracing::debug!("auction {id} selected");
            if let Some(session) = self.auction.as_mut() {
                // A second click on the selected row clears it, so there is a
                // way to put the Bid button back to sleep without paging.
                session.selected = (session.selected != Some(id)).then_some(id);
            }
        }
        if let Some(click) = hud_response.auction_click {
            tracing::debug!("auction control {click:?}");
            self.auction_control(click);
        }

        // **Logged whenever the window reports anything at all**, so a click
        // that reached the frame and one that never did can be told apart
        // from the log alone. Rare -- a press of a button, not a frame event.
        if hud_response.questgiver.picked.is_some()
            || hud_response.questgiver.chosen_option.is_some()
            || hud_response.questgiver.chosen_reward.is_some()
            || hud_response.questgiver.acted.is_some()
            || hud_response.questgiver.closed
        {
            tracing::info!(
                "questgiver window: picked {:?}, chosen_option {:?}, chosen_reward {:?}, \
                 acted {:?}, closed {}",
                hud_response.questgiver.picked,
                hud_response.questgiver.chosen_option,
                hud_response.questgiver.chosen_reward,
                hud_response.questgiver.acted,
                hud_response.questgiver.closed
            );
        }
        if let Some(quest) = hud_response.questgiver.picked {
            if let Some(questgiver) = self.questgiver.as_mut() {
                questgiver.showing = Some(quest);
                questgiver.selected_reward = 0;
            }
            // Ask for the scroll as a real client does, in that order: the
            // server checks at each step that this NPC actually offers the
            // quest, and skipping straight to the accept would work or not for
            // reasons nothing here could tell apart.
            self.ask_for_quest_scroll(quest);
        }
        // A speech line was chosen. Left in `List` view rather than moved to
        // `showing` -- it is not a quest -- so the window simply waits for
        // whatever the server sends back: a new menu (`note_gossip` replaces
        // `options` and `quests` in place, which is what lets a multi-step
        // gossip tree be clicked through one line at a time), or nothing at
        // all for a line that only ever meant "close the conversation".
        if let Some(index) = hud_response.questgiver.chosen_option {
            if let (Some(questgiver), Some(live)) =
                (self.questgiver.as_ref(), self.live.as_mut())
            {
                questgiver.choose_option(live, index);
            }
        }
        // A reward row was clicked. Purely local -- nothing goes out until
        // `Complete` is pressed, the same way picking a bag slot to move
        // does not move it -- so this only has to update which row the
        // window highlights for the next frame.
        if let Some(index) = hud_response.questgiver.chosen_reward {
            if let Some(questgiver) = self.questgiver.as_mut() {
                questgiver.selected_reward = index;
            }
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
            self.vendor = None;
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
        // Facing along -X, so a wall crossing the ray has a normal close to
        // the X axis -- near-vertical, the shape that takes the shorten-the-
        // ray branch rather than the ceiling-duck one.
        let wall_normal = glam::Vec3::new(1.0, 0.0, 0.0);

        assert_eq!(
            pull_camera_in_front_of_walls(focus, eye, |_, _| None),
            eye,
            "nothing in the way must leave the camera alone"
        );

        // Something four units out along a ten-unit ray.
        let pulled =
            pull_camera_in_front_of_walls(focus, eye, |_, _| Some((0.4, wall_normal)));
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
        let against_the_back =
            pull_camera_in_front_of_walls(focus, eye, |_, _| Some((0.02, wall_normal)));
        assert!((against_the_back - focus).length() >= CAMERA_MIN_PULL_IN - 1e-3);
    }

    /// **A low ceiling ducks the eye down; it does not pull the eye in to the
    /// character.** Reported from live play as the camera "going first
    /// person" in a cave -- a near-horizontal hit used to get the identical
    /// treatment a wall does, which reads as the eye landing on the
    /// character's own face rather than as a tunnel with a low roof.
    ///
    /// The normal handed in here points down, the way a real ceiling's
    /// should -- but the assertions never look at the *sign* the function
    /// was given, only at where the eye ends up, because that is also true
    /// of the function under test now. See the next one for why that
    /// distinction earned its own test rather than being incidental.
    #[test]
    fn a_low_ceiling_ducks_the_camera_rather_than_pulling_it_in() {
        let focus = glam::Vec3::new(0.0, 0.0, 2.0);
        // Up and back, the ordinary orbit shape: the eye sits above and
        // behind the character.
        let eye = glam::Vec3::new(-6.0, 0.0, 5.0);
        // A ceiling's outward normal points down, into the room under it.
        let ceiling_normal = glam::Vec3::new(0.0, 0.0, -1.0);
        // Hit two fifths of the way out, at z = 2.0 + 0.4*3.0 = 3.2.
        let pulled = pull_camera_in_front_of_walls(focus, eye, |_, _| {
            Some((0.4, ceiling_normal))
        });

        assert!(
            (pulled.z - (3.2 - CAMERA_WALL_CLEARANCE)).abs() < 1e-3,
            "the eye should sit just under the ceiling, at {:?}",
            pulled
        );
        // The horizontal offset the orbit asked for survives untouched --
        // ducking is not the same move as pulling in, and a camera that also
        // crept forward on a low ceiling would zoom in over a whole cave
        // passage exactly the way this was reported to.
        assert_eq!((pulled.x, pulled.y), (eye.x, eye.y));
        assert!(
            pulled.z < eye.z,
            "a ceiling that was hit must lower the eye, not leave it alone"
        );
    }

    /// **The fix that actually closed the report.** The first attempt read
    /// which way to duck off the hit triangle's own normal, on the
    /// assumption that a WMO's winding gives an outward-facing one the way a
    /// hand-built test cube does -- and nothing in this crate had ever
    /// actually checked that assumption before, because every earlier user
    /// of a floor-like normal only ever took its magnitude. Reported back
    /// from the same cave with the fix in place: the camera still cut in
    /// close. This is the version after that -- ducking decided from where
    /// the hit sits relative to the character, not from which way its
    /// triangle claims to face -- and this test is what a mis-wound ceiling
    /// looks like: the normal handed in points *up*, the wrong way for a
    /// ceiling, and the eye must still duck down rather than rise into the
    /// rock that was just hit.
    #[test]
    fn ducking_reads_the_hits_position_not_its_normals_claimed_side() {
        let focus = glam::Vec3::new(0.0, 0.0, 2.0);
        let eye = glam::Vec3::new(-6.0, 0.0, 5.0);
        // Backwards from what a real ceiling's normal should be -- exactly
        // the shape a WMO wound the other way from this crate's test cubes
        // would produce.
        let inverted_normal = glam::Vec3::new(0.0, 0.0, 1.0);
        let pulled = pull_camera_in_front_of_walls(focus, eye, |_, _| {
            Some((0.4, inverted_normal))
        });

        assert!(
            pulled.z < eye.z,
            "a hit above the character must duck the eye down regardless of \
             which way its normal points, but the eye ended up at {pulled:?}"
        );
        assert!(
            (pulled.z - (3.2 - CAMERA_WALL_CLEARANCE)).abs() < 1e-3,
            "should duck to the same height a correctly-wound ceiling would: {pulled:?}"
        );
    }

    /// **Crashed the client, live.** `CAMERA_MIN_PULL_IN` (1.5) was a floor
    /// on `stopped` that assumed `length` -- the distance to whatever `eye`
    /// this call was given -- always started at the *full* nominal orbit
    /// distance, comfortably above it. True for a single call; false once
    /// `pull_camera_clear_of_the_building` started feeding one call's output
    /// back in as the next call's `eye`, because a duck is bounded by a
    /// nearby ceiling and nothing else, so a second pass can be handed an
    /// `eye` under 1.5 units from `focus`. `clamp(1.5, length)` with
    /// `length` under 1.5 panics -- reported back as "same issue, no
    /// improvement" because a crash and an uncaught escape look identical
    /// for the one frame a player sees before either happens.
    #[test]
    fn a_wall_closer_than_the_minimum_pull_in_does_not_panic() {
        let focus = glam::Vec3::new(0.0, 0.0, 2.0);
        // Already well under `CAMERA_MIN_PULL_IN` before the wall test even
        // runs -- exactly what a prior duck can hand this function.
        let eye = focus + glam::Vec3::X * 1.0;
        let wall_normal = glam::Vec3::new(1.0, 0.0, 0.0);
        // A hit near the far end, so `t * length` alone would also clamp
        // below `length` -- this is not about `CAMERA_WALL_CLEARANCE`.
        let pulled = pull_camera_in_front_of_walls(focus, eye, |_, _| Some((0.9, wall_normal)));
        assert!(
            (pulled - focus).length() <= 1.0 + 1e-3,
            "must not overshoot the segment it was given: {pulled:?}"
        );
    }

    /// **The escape a single pass cannot see.** Modelled on a real capture:
    /// an open window at head height with a solid sill below it. The
    /// original, higher orbit ray sails straight through the open window and
    /// only finds a roof far outside; ducking under that roof preserves the
    /// window's horizontal offset untouched, because ducking only ever
    /// touches height. The *lower*, ducked ray now aims at the solid sill
    /// instead of the open window -- a wall this test's mock reports only
    /// when asked about a line passing below `z=2.8` at `x=-3`. A single
    /// [`pull_camera_in_front_of_walls`] pass never asks that second
    /// question; [`pull_camera_clear_of_the_building`] does, by construction.
    #[test]
    fn a_duck_that_lands_past_a_sill_is_caught_on_the_next_pass() {
        let focus = glam::Vec3::new(0.0, 0.0, 2.0);
        // The nominal orbit's own choice -- far and a little above the
        // window -- comes from wherever the wheel and the drag left it, and
        // is not itself the bug.
        let nominal = glam::Vec3::new(-11.0, 0.0, 6.0);

        let sill_and_window = |_from: glam::Vec3, to: glam::Vec3| -> Option<(f32, glam::Vec3)> {
            let span = to - focus;
            if span.x.abs() > 1e-6 {
                let t_wall = -3.0 / span.x;
                if (0.0..=1.0).contains(&t_wall) && focus.z + t_wall * span.z <= 2.8 {
                    return Some((t_wall, glam::Vec3::new(1.0, 0.0, 0.0)));
                }
            }
            // Through the open window: nothing here until a distant roof,
            // reachable only by a ray that still points roughly where the
            // nominal orbit did.
            (to.x <= -10.0).then_some((0.5, glam::Vec3::new(0.0, 0.0, -1.0)))
        };

        // Control: a single pass really does reproduce the escape, so the
        // fix below is not just agreeing with an untriggered mock.
        let single_pass = pull_camera_in_front_of_walls(focus, nominal, sill_and_window);
        assert!(
            (single_pass.x - nominal.x).abs() < 1e-3,
            "the control should still show the duck leaving x untouched: {single_pass:?}"
        );

        let clear = pull_camera_clear_of_the_building(focus, nominal, sill_and_window);
        assert!(
            (clear - focus).length() < 5.0,
            "a second pass should catch the sill and pull the eye back near the room, \
             not leave it {:.1} units out at {clear:?}",
            (clear - focus).length()
        );
        assert!(
            clear.x > -4.0,
            "the eye should end up on the near side of the sill, not past it: {clear:?}"
        );
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

    /// The clearance boundary is a continuous point on the ray, not one of
    /// twelve fixed stops -- foss-wow#137's "stutter" was the eye snapping
    /// between adjacent n/12 fractions of the ray as the true crossing point
    /// drifted back and forth across a step boundary under a cave ceiling.
    #[test]
    fn ground_clearance_is_not_snapped_to_a_twelfth() {
        let focus = glam::Vec3::new(0.0, 0.0, 10.0);
        let wanted = focus + glam::Vec3::new(0.0, 0.0, -1.0) * 20.0;
        // Flat ground at z = 8: the true clearance boundary sits at
        // t = (10 - 8.5) / 20 = 0.075, inside the coarse pass's very first
        // twelfth (0..0.083). The old code always answered t = 0 here --
        // right back on the subject -- because it could not stop anywhere
        // inside a bracket, only at its edges.
        let ground = |_x: f32, _y: f32| Some(8.0f32);
        let eye = pull_camera_out_of_the_ground(focus, wanted, ground);
        let expected_z = 8.0 + CAMERA_GROUND_CLEARANCE;
        assert!(
            (eye.z - expected_z).abs() < 0.05,
            "ground clearance was snapped to a coarse step: eye.z={:.3}, expected close to {:.3}",
            eye.z,
            expected_z
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

    /// When `eye` never left the orbit's own view ray -- the ground pass and
    /// the wall-pulling half of the ceiling/wall test both only ever shorten
    /// it -- re-deriving yaw and pitch from `eye` must reproduce the angles
    /// the orbit was already given, not merely something that also happens to
    /// look at `focus`.
    #[test]
    fn face_focus_from_agrees_with_the_orbit_it_came_from() {
        let feet = glam::Vec3::new(4.0, -7.0, 12.0);
        let focus = feet + glam::Vec3::Z * FOLLOW_HEIGHT;
        for (yaw, pitch) in [(0.0, 0.0), (1.2, 0.4), (-2.1, -0.6), (0.3, FOLLOW_PITCH_LIMIT)] {
            let placed = orbit_around(feet, yaw, pitch, 8.0);
            let (got_yaw, got_pitch) = face_focus_from(placed.position, focus);
            assert!(
                (got_yaw - yaw).abs() < 1e-4 && (got_pitch - pitch).abs() < 1e-4,
                "on-ray eye should give back the same angles: wanted ({yaw}, {pitch}), got ({got_yaw}, {got_pitch})"
            );
        }
    }

    /// **The duck bug.** `pull_camera_in_front_of_walls` deliberately steps
    /// `eye` straight down and off the orbit's view ray under a low ceiling --
    /// see `ducking_reads_the_hits_position_not_its_normals_claimed_side` --
    /// so an eye built that way is no longer looking at `focus` under the
    /// stale angles the orbit was given. Reproduced here without a live world:
    /// take an ordinary up-and-back orbit eye and duck it straight down, the
    /// exact shape `pull_camera_in_front_of_walls` produces, then check the
    /// re-derived angles actually point at the character instead of past
    /// them.
    #[test]
    fn face_focus_from_recovers_a_ducked_eye() {
        let focus = glam::Vec3::new(0.0, 0.0, 2.0);
        let placed = orbit_around(focus - glam::Vec3::Z * FOLLOW_HEIGHT, 0.0, 0.3, 6.0);
        // The duck only ever touches z, per
        // `a_low_ceiling_ducks_the_camera_rather_than_pulling_it_in`.
        let ducked = glam::Vec3::new(placed.position.x, placed.position.y, focus.z + 0.4);
        assert!(
            (ducked - focus).normalize().z > (placed.position - focus).normalize().z,
            "the test fixture is not actually a duck"
        );

        let (yaw, pitch) = face_focus_from(ducked, focus);
        let (sp, cp) = pitch.sin_cos();
        let (sy, cy) = yaw.sin_cos();
        let forward = glam::Vec3::new(cp * cy, cp * sy, sp);
        let to_focus = (focus - ducked).normalize();
        assert!(
            forward.dot(to_focus) > 0.9999,
            "the re-derived angles do not look at the character: forward {forward:?} vs {to_focus:?}"
        );

        // The bug this replaces: the stale, pre-duck pitch aimed well past the
        // character from the ducked position.
        let (stale_sp, stale_cp) = placed.pitch.sin_cos();
        let (stale_sy, stale_cy) = placed.yaw.sin_cos();
        let stale_forward = glam::Vec3::new(stale_cp * stale_cy, stale_cp * stale_sy, stale_sp);
        assert!(
            stale_forward.dot(to_focus) < forward.dot(to_focus),
            "the stale angles should be a worse aim at the character than the re-derived ones"
        );
    }

    /// **The escape this reproduces.** `RUST_LOG=wow_viewer=debug` on a live
    /// Northshire Abbey attic showed `camera pull-in: t=0.782 ... stopped=23.11
    /// of 30.00` -- the wheel was at `ui::camera::MAX_DISTANCE` (30), and the
    /// single ray the wall test casts found nothing until 23 units out, well
    /// past any wall of the small room the character stood in. Outdoors the
    /// same 30 must survive untouched -- an open field is exactly where a full
    /// zoom-out is a reasonable thing to want.
    #[test]
    fn the_indoor_cap_only_applies_indoors() {
        assert!(
            (indoor_capped_distance(30.0, true) - CAMERA_INDOOR_DISTANCE_CAP).abs() < 1e-5,
            "30 units indoors is exactly the escape this exists to prevent"
        );
        assert!(
            (indoor_capped_distance(30.0, false) - 30.0).abs() < 1e-5,
            "outdoors must keep the full zoom range"
        );
        // Below the cap, indoors changes nothing -- a normal room at the
        // default distance (9, see `ui::Camera::default`) must look identical
        // to how every earlier indoor milestone was tested.
        assert!((indoor_capped_distance(9.0, true) - 9.0).abs() < 1e-5);
    }

    /// **The bleed.** A live capture showed the character correctly framed
    /// and close to a wall, with open valley visible through a corner off to
    /// one side -- a centre ray that never crossed anything, next to a
    /// corner only a few degrees off centre. This fixture makes that corner
    /// literal: `eye_at` returns a point far from `focus` for every yaw
    /// except the one matching a side sample, which it reports as very
    /// close instead.
    #[test]
    fn a_corner_only_a_side_sample_sees_still_pulls_in_the_centre() {
        let focus = glam::Vec3::new(0.0, 0.0, 0.0);
        let center_yaw = 0.5;
        let corner_yaw = center_yaw + CAMERA_FOV_SAMPLE_ANGLE;
        let eye_at = |yaw: f32, distance: f32| -> glam::Vec3 {
            let effective = if (yaw - corner_yaw).abs() < 1e-4 {
                2.0
            } else {
                distance
            };
            focus + glam::Vec3::new(yaw.cos(), yaw.sin(), 0.0) * effective
        };

        let result = tightest_eye_at_center(center_yaw, 10.0, focus, eye_at);
        assert!(
            (result - focus).length() < 2.5,
            "the corner only the side sample saw should still have pulled the \
             centre in, not left it at {result:?}"
        );
        // Pulled in along the *centre* direction, not moved to the corner's.
        let expected_direction = glam::Vec3::new(center_yaw.cos(), center_yaw.sin(), 0.0);
        assert!(
            (result - focus).normalize().dot(expected_direction) > 0.999,
            "the character must stay framed dead centre: {result:?}"
        );
    }

    /// A centre ray that is already the tightest must be left alone -- this
    /// is the ordinary case, and it must not cost anything or wobble.
    #[test]
    fn an_unobstructed_view_is_unaffected_by_the_side_samples() {
        let focus = glam::Vec3::new(0.0, 0.0, 0.0);
        let center_yaw = 1.1;
        let eye_at = |yaw: f32, distance: f32| {
            focus + glam::Vec3::new(yaw.cos(), yaw.sin(), 0.0) * distance
        };
        let result = tightest_eye_at_center(center_yaw, 10.0, focus, eye_at);
        assert!((result - eye_at(center_yaw, 10.0)).length() < 1e-4);
    }

    /// **The judder.** `first_obstruction` flipping which triangle answers,
    /// or a hit flipping between the duck and pull-in branches, moved the
    /// wall-avoiding distance a little every frame even while the character
    /// stood still and merely turned -- reported live as the camera feeling
    /// "stuck" once the escape itself was fixed. A small back-and-forth must
    /// not read on screen as two large snaps.
    #[test]
    fn small_frame_to_frame_noise_is_smoothed_not_snapped() {
        let mut state = Some(6.0);
        // A one-unit flicker, the shape a flipped classification produces,
        // not a real change in the room.
        let eased = App::camera_follow_wall_distance(&mut state, 5.0, 1.0 / 60.0);
        assert!(
            eased > 5.0 && eased < 6.0,
            "one frame of noise moved the camera to {eased}, not partway towards it"
        );
    }

    /// A real change of room -- walking out of a cramped stairwell into a
    /// hall -- is not noise, and gliding it over a tenth of a second would
    /// read as lag rather than as the camera being attached to the
    /// character. Larger than `camera_follow_z`'s own threshold on purpose:
    /// see `camera_follow_wall_distance`.
    #[test]
    fn a_large_jump_snaps_instead_of_gliding() {
        let mut state = Some(2.0);
        let eased = App::camera_follow_wall_distance(&mut state, 14.0, 1.0 / 60.0);
        assert!(
            (eased - 14.0).abs() < 1e-4,
            "a real room change should snap, not ease: got {eased}"
        );
    }

    /// The very first frame has nothing to ease from.
    #[test]
    fn the_first_frame_has_no_history_to_ease_from() {
        let mut state = None;
        let eased = App::camera_follow_wall_distance(&mut state, 7.0, 1.0 / 60.0);
        assert_eq!(eased, 7.0);
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

#[cfg(test)]
mod sign_in_tests {
    use super::*;

    /// **The one decision that can break every probe in the tree.** A command
    /// line that already says what to draw or what to connect to must not stop
    /// to ask, and one that does not must. Both halves are asserted, because
    /// each fails in a way the other cannot see: too eager and
    /// `docs/ROADMAP.md`'s screenshots hang on a login panel forever, too shy
    /// and a double-click opens a fireball icon nobody asked for.
    #[test]
    fn the_command_line_decides_whether_to_ask() {
        let asks = |argv: &[&str]| {
            let mut full = vec!["wow-viewer"];
            full.extend_from_slice(argv);
            !Args::parse_from(full).is_self_contained()
        };

        // Nothing at all: the double-click this screen exists for.
        assert!(asks(&[]));
        // A data directory says where the archives are and nothing about what
        // to draw with them.
        assert!(asks(&["--data", "D:/Games/WoW/Data"]));

        // Each offline scene is a complete instruction on its own.
        assert!(!asks(&["--texture", r"Interface\Icons\Foo.blp"]));
        assert!(!asks(&["--model", r"Creature\Wolf\Wolf.m2"]));
        assert!(!asks(&["--creature", "49"]));
        assert!(!asks(&["--wmo", r"World\wmo\a.wmo"]));
        assert!(!asks(&["--map", "Azeroth"]));

        // A session needs all three parts. Two of them is a half-typed
        // command line, and the screen is a better answer to that than an
        // error is -- it arrives with those two already filled in.
        assert!(!asks(&[
            "--realm-host", "127.0.0.1", "--user", "OWC33", "--character", "Testwolf"
        ]));
        assert!(asks(&["--realm-host", "127.0.0.1"]));
        assert!(asks(&["--realm-host", "127.0.0.1", "--user", "OWC33"]));
        assert!(asks(&["--user", "OWC33", "--character", "Testwolf"]));
    }
}

#[cfg(test)]
mod weather_tests {
    use super::*;

    fn rain(intensity: f32) -> ::world::WeatherChange {
        ::world::WeatherChange {
            weather: ::world::Weather::HeavyRain,
            intensity,
            abrupt: false,
        }
    }

    /// **The bug, as a test that fails on the old rule.** A guard standing in
    /// a building reported rain falling around them: `resolve_precipitation`
    /// has no notion of a roof (the field is a camera-relative box, see
    /// `render::precipitation`), so it produced the exact same `Falling`
    /// indoors as out. `precipitation_for_frame` is the fix, and this is the
    /// one comparison that matters -- everything else about the weather is
    /// unchanged by the roof.
    #[test]
    fn indoors_suppresses_precipitation_regardless_of_weather() {
        let weather = rain(0.8);
        assert!(precipitation_for_frame(weather, 4.0, false).is_some());
        assert!(
            precipitation_for_frame(weather, 4.0, true).is_none(),
            "rain must not fall indoors"
        );
    }

    /// Outdoors, gating on `indoors` must not have changed anything about
    /// *what* falls -- only `resolve_precipitation`'s existing answer passed
    /// through unmodified. A test that only checked "indoors is None" could
    /// pass by suppressing outdoor rain too.
    #[test]
    fn outdoors_is_untouched() {
        let weather = rain(0.8);
        assert_eq!(
            precipitation_for_frame(weather, 4.0, false),
            resolve_precipitation(weather, 4.0)
        );
    }

    /// Every rain and snow tier maps to the loop the archive actually names
    /// for it -- see `sound::WeatherAmbience::sound_id` for where the ids
    /// themselves are confirmed against `SoundEntries.dbc`. `BlackRain` and
    /// `BlackSnow` are asserted here specifically because there is no
    /// `Weather - BlackRain` row for them to fall through to on their own;
    /// the heavy tier is a deliberate choice, not a default.
    #[test]
    fn every_rain_and_snow_tier_names_a_loop() {
        use ::world::Weather::*;
        use sound::WeatherAmbience::*;
        assert_eq!(weather_ambience(LightRain), Some(RainLight));
        assert_eq!(weather_ambience(MediumRain), Some(RainMedium));
        assert_eq!(weather_ambience(HeavyRain), Some(RainHeavy));
        assert_eq!(weather_ambience(BlackRain), Some(RainHeavy));
        assert_eq!(weather_ambience(LightSnow), Some(SnowLight));
        assert_eq!(weather_ambience(MediumSnow), Some(SnowMedium));
        assert_eq!(weather_ambience(HeavySnow), Some(SnowHeavy));
        assert_eq!(weather_ambience(BlackSnow), Some(SnowHeavy));
    }

    /// Fine weather, fog, the sandstorms and thunder are all dry as far as
    /// this client is concerned (see `world::Weather::precipitation`'s own
    /// doc comment) and must not start a rain or snow loop playing over
    /// silence.
    #[test]
    fn dry_weather_names_no_loop() {
        use ::world::Weather::*;
        assert_eq!(weather_ambience(Fine), None);
        assert_eq!(weather_ambience(Fog), None);
        assert_eq!(weather_ambience(Thunders), None);
        assert_eq!(weather_ambience(LightSandstorm), None);
    }
}
