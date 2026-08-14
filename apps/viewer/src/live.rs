//! Standing in a live world.
//!
//! This is where the two halves of the project meet. Until now the renderer
//! drew a world nobody was in, and the protocol reached a world nothing drew:
//! this module logs in, enters as a character, and hands back where that
//! character is standing and what is around it, in the renderer's own
//! coordinates.
//!
//! The join is cheaper than it looks, for one reason worth recording: **the
//! network position is already in the renderer's world space.** No conversion.
//! That is not true of the coordinates in the data files -- ADT placements are
//! stored measured inwards from the grid corner with the axes permuted, which
//! is why [`crate::scene::placement_position`] exists -- so the natural
//! assumption is that the network needs a conversion too. It does not, and
//! applying one puts the camera in a plausible-looking but entirely wrong part
//! of the map.

use anyhow::{bail, Context, Result};
use glam::Vec3;
use mpq::Chain;

/// One thing standing in the world near the player.
pub struct Entity {
    pub guid: u64,
    /// `CreatureDisplayInfo` id, which is what selects a model and its skins.
    pub display_id: u32,
    /// Interpolated along a monster move's path if one is in flight -- see
    /// `world::state::Entity::interpolated_position` -- not the raw
    /// last-reported position, which only ever holds a path's start.
    pub position: Vec3,
    pub orientation: f32,
    pub scale: f32,
    pub kind: world::ObjectType,
    pub level: Option<u32>,
    /// How fast this entity is travelling, in world units per second, and zero
    /// when it is standing. The renderer picks a stand, walk or run cycle from
    /// it -- see `crate::world::Motion`.
    pub speed: f32,
    /// Whether this unit is lying dead, and so should be drawn face down.
    /// Outranks `speed` when choosing a cycle: a creature killed mid-charge
    /// keeps the charge's speed for a moment after it stops being able to use
    /// it.
    ///
    /// A *ghost* is deliberately not "dead" for this purpose -- see
    /// `world::state::Entity::is_corpse`. A released player runs back to their
    /// body on their own feet.
    pub dead: bool,
    /// How long ago it was seen to die, when this client watched it happen.
    /// `None` for a corpse that was already lying there -- see
    /// `world::state::Entity::died_at`.
    pub died_ms_ago: Option<u32>,
    /// How long ago it last swung at something.
    pub swung_ms_ago: Option<u32>,
    /// Whether it is in a melee, on either side of it.
    pub fighting: bool,
    /// The five character-creation numbers, for a *player*.
    ///
    /// `None` for every creature, and that is the whole distinction: a
    /// creature's looks are in its display id, while display 49 is every human
    /// male alive and says nothing about any of them. Carried rather than
    /// resolved here because turning it into textures composes a skin, which
    /// is far too expensive to redo every frame -- the caller caches on it.
    pub appearance: Option<::world::Appearance>,
    /// Whether this unit is carrying its weapon in hand rather than stowed.
    ///
    /// Read from replicated state for *everyone, this client's own character
    /// included* -- unlike position, which the server never echoes back. That
    /// asymmetry is worth stating because it is the opposite of the trap
    /// documented on `own_entity`: the server does republish what a client
    /// says about its sheath, so the round trip is the confirmation rather
    /// than a thing to work around.
    pub sheathed: bool,
}

/// Where the player is and what can be seen from there.
pub struct LiveWorld {
    pub character: String,
    /// The character's own guid, needed as the mover in every movement packet
    /// this client sends.
    pub guid: u64,
    pub map_id: u32,
    /// Folder under `World\Maps`, which is what the streaming renderer needs.
    pub map_directory: String,
    pub map_name: String,
    pub position: Vec3,
    pub orientation: f32,
    /// Everything replicated so far: every object in range, kept live rather
    /// than a one-time snapshot of who was around at login. `connect` seeds it
    /// from the login burst; the caller folds every later batch into it too
    /// (see [`replicate`]), which is what makes creatures and other players
    /// move on screen instead of standing wherever they were at login forever.
    pub state: world::WorldState,
    /// Packets that failed to fold into `state`, summed over the whole
    /// session rather than per batch -- a per-batch count resets to zero every
    /// frame and hides a decode that is failing steadily.
    pub fold_failures: usize,
    /// Opcodes whose first undecodable body has already been logged.
    ///
    /// `WorldState::replicate` goes to the trouble of carrying each failed
    /// packet's *body* out with the error, and the only reason to do that is
    /// so somebody can read the shape later. Several parsers in `world::spell`
    /// deliberately refuse layouts nobody has captured yet and say in their
    /// own doc comments that a capture is what would settle them -- and a
    /// counter alone cannot tell "the server never sent it" from "it arrived
    /// and we could not read it", which this project has already paid for
    /// once. The first body per opcode is the one that teaches; the rest are
    /// noise, and a busy zone would produce plenty.
    reported_failures: std::collections::HashSet<u16>,
    /// How this character is dressed, resolved once at login.
    ///
    /// Resolved here rather than per frame because it cannot change without a
    /// barber, and reading three DBCs every frame to learn the same answer
    /// would be the login burst's thirty-seven seconds all over again.
    pub look: std::rc::Rc<crate::character::Look>,
    /// Distinguishes this look in the renderer's model cache.
    pub look_key: u64,
    /// Kept alive rather than dropped at the end of [`connect`]: the viewer
    /// walks the character over this same connection, and RC4 header state
    /// cannot be shared or rewound, so a fresh connection could not pick up
    /// where this one left off.
    pub connection: world::Connection,
    /// The heading each entity is currently *drawn* at, as opposed to the one
    /// the world says it should have.
    ///
    /// Kept because a facing arrives as a step change -- a creature acquires a
    /// target and the correct heading becomes a different number between one
    /// frame and the next -- and a model that jumps to it reads as a snap. See
    /// [`LiveWorld::ease_facings`].
    facings: std::collections::HashMap<u64, f32>,
}

/// What to connect to.
pub struct Login<'a> {
    pub host: &'a str,
    pub port: u16,
    pub user: &'a str,
    pub password: &'a str,
    pub realm: Option<&'a str>,
    pub character: &'a str,
    pub locale: &'a str,
}

/// Logs in, enters the world, and reads the initial object update.
pub fn connect(chain: &mut Chain, login: &Login<'_>) -> Result<LiveWorld> {
    let timeout = std::time::Duration::from_secs(10);

    tracing::info!("logging in to {}:{}", login.host, login.port);
    let session = auth::login(
        login.host,
        login.port,
        login.user,
        login.password,
        login.locale,
        timeout,
    )?;

    let realm = match login.realm {
        Some(wanted) => session
            .realms
            .iter()
            .find(|realm| realm.name.eq_ignore_ascii_case(wanted))
            .with_context(|| format!("no realm named {wanted:?}"))?,
        None => session
            .realms
            .first()
            .context("the logon server offered no realms")?,
    };
    tracing::info!("realm {:?} at {}", realm.name, realm.address);

    let (host, port) = world::client::split_realm_address(&realm.address)?;
    let mut connection = world::Connection::open(
        &format!("{host}:{port}"),
        login.user,
        realm.id as u32,
        &session.session_key,
        timeout,
    )?;

    let characters = connection.characters()?;
    let character = characters
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(login.character))
        .with_context(|| {
            let names: Vec<&str> = characters.iter().map(|c| c.name.as_str()).collect();
            format!(
                "no character named {:?} on {}; this account has {names:?}",
                login.character, realm.name
            )
        })?;

    let landed = connection.enter_world(character.guid)?;
    tracing::info!(
        "in world as {} on map {} at {:.1}, {:.1}, {:.1}",
        character.name,
        landed.map,
        landed.x,
        landed.y,
        landed.z
    );

    // Nothing marks the end of the login burst, so read until it goes quiet.
    let burst = read_login_burst(&mut connection)?;
    let mut state = world::WorldState::new();
    // Held rather than counted on the spot: the burst is the widest variety of
    // packets this client ever sees at once, so it is the likeliest place for
    // a shape no parser has confirmed to turn up -- and the body is what would
    // settle it. Logged once `LiveWorld` exists to remember which opcodes have
    // already been reported.
    let burst_report = replicate(&mut state, &burst);
    tracing::info!(
        "login burst: {} packets, {} objects, {} spells",
        burst.len(),
        state.len(),
        state.spells.spells.len(),
    );

    let appearance = crate::character::Appearance {
        race: character.race,
        gender: character.gender,
        skin: character.skin,
        face: character.face,
        hair_style: character.hair_style,
        hair_colour: character.hair_color,
        facial_hair: character.facial_hair,
    };
    // Straight from the character list, which sends *display* ids -- so our own
    // body needs no `Item.dbc` lookup at all, where another player's visible
    // items arrive as entry ids and do. In the order the wire sends them, which
    // is also the order they must be painted: see `resolve_wearing`.
    // Display id *and* inventory type: the type is what says whether an item
    // switches on a glove or a boot, and the character list carries it.
    let equipment: Vec<(u32, u8)> = character
        .equipment
        .iter()
        .map(|slot| (slot.display_id, slot.inventory_type))
        .collect();
    let look = crate::character::resolve_wearing(chain, appearance, &equipment);
    tracing::info!(
        "character look: skin {:?}, body {:?}, hair {:?}, geosets {:?}, wearing {:?}",
        look.skin,
        look.body,
        look.hair,
        look.geosets,
        equipment.iter().filter(|(id, _)| *id != 0).count()
    );

    let (map_directory, map_name) = map_directory(chain, landed.map)?;
    let mut live = LiveWorld {
        character: character.name.clone(),
        guid: character.guid,
        map_id: landed.map,
        map_directory,
        map_name,
        // Already in world space -- see the module comment.
        position: Vec3::new(landed.x, landed.y, landed.z),
        orientation: landed.orientation,
        state,
        look: std::rc::Rc::new(look),
        look_key: crate::character::look_key(&appearance, &equipment),
        fold_failures: 0,
        reported_failures: std::collections::HashSet::new(),
        connection,
        facings: std::collections::HashMap::new(),
    };
    live.note_failures(&burst_report);
    Ok(live)
}

/// How long the client will wait for the login burst before showing the world.
///
/// A budget rather than a target: whatever has not arrived by now keeps
/// arriving through the ordinary per-frame pump, which is the same code path
/// that handles every later packet anyway.
const BURST_BUDGET: std::time::Duration = std::time::Duration::from_millis(2500);

/// Reads the initial world state, bounded by the clock.
///
/// `Connection::drain` stops when the stream goes quiet *or* when a packet
/// limit is reached, and nothing bounds how long that takes. In an empty zone
/// it returns in milliseconds; in Northshire, which emits a monster move
/// roughly fourteen times a second and is therefore never quiet, it kept going
/// until it had collected its 512-packet limit -- **thirty-seven seconds**,
/// during which the client had not drawn a frame.
///
/// That looked for a while like a slow `Spell.dbc` read, because the symptom
/// was the action bar filling half a minute after login. It was not: the
/// spellbook was in the burst all along, at the end of it, and the DBC load
/// takes 185ms. The lesson is the ordinary one -- measure the thing rather
/// than the thing next to it.
///
/// So the burst is now bounded by wall clock as well. Anything still in flight
/// is not lost; it arrives through the pump a frame or two later, and the
/// interface fills in as it does.
fn read_login_burst(
    connection: &mut world::Connection,
) -> Result<Vec<world::client::Packet>, world::client::Error> {
    // The budget can only be checked between chunks, so the chunk size sets
    // how far past it this can overshoot: a chunk does not return until it has
    // its packets or the stream goes quiet, and at Northshire's fourteen
    // packets a second a chunk of 64 takes four and a half seconds on its own.
    // Sixteen keeps the overshoot near a second, which is the real bound here
    // rather than the budget.
    const CHUNK: usize = 16;

    let deadline = std::time::Instant::now() + BURST_BUDGET;
    let mut burst = Vec::new();
    loop {
        // A short quiet window so a genuinely idle zone returns immediately.
        let chunk = connection.drain(std::time::Duration::from_millis(200), CHUNK)?;
        let went_quiet = chunk.is_empty();
        burst.extend(chunk);
        if went_quiet || std::time::Instant::now() >= deadline {
            return Ok(burst);
        }
    }
}

impl LiveWorld {
    /// Records a fold's failures: counts them all, and logs the first body
    /// seen for each opcode.
    ///
    /// The body is the whole point. Several parsers in `world::spell` refuse
    /// layouts nobody has captured -- a `SMSG_SPELL_GO` carrying a miss, a
    /// cast aimed at a location rather than a unit -- and each says in its own
    /// doc comment that a capture is what would settle it. Without this the
    /// packet that would answer the question arrives, increments a counter,
    /// and is discarded; the next person to look sees a number and no way to
    /// tell whether the shape ever showed up at all.
    pub fn note_failures(&mut self, report: &world::state::Replication) {
        self.fold_failures += report.failures.len();
        for (opcode, error, body) in &report.failures {
            if !self.reported_failures.insert(*opcode) {
                continue;
            }
            match body {
                Ok(bytes) => tracing::warn!(
                    "undecoded opcode {opcode:#06x} ({} bytes): {error}. First body: {}",
                    bytes.len(),
                    hex_preview(bytes, 64)
                ),
                Err(e) => tracing::warn!(
                    "undecoded opcode {opcode:#06x}: {error}; its body was \
                     unavailable too: {e}"
                ),
            }
        }
    }
}

/// The first `limit` bytes of a body, for eyeballing a layout that would not
/// parse.
fn hex_preview(body: &[u8], limit: usize) -> String {
    let shown: Vec<String> = body.iter().take(limit).map(|b| format!("{b:02x}")).collect();
    if body.len() > limit {
        format!("{}...", shown.join(" "))
    } else {
        shown.join(" ")
    }
}

/// Folds a batch of drained packets into replicated world state.
///
/// A thin wrapper over `WorldState::replicate`, which is the single place
/// this opcode dispatch lives -- it used to be duplicated here and in
/// `tools/wow-cli`, and two independent tables over the same state machine
/// drift silently: a new opcode wired into one and not the other freezes
/// whatever it should have moved, unnoticed.
///
/// The whole `Replication` is handed back, not a summary. This used to return
/// only a failure count, which was fine until `replicate` grew a category the
/// caller had to act on: chat is *returned* rather than stored, so a caller
/// that discards the report discards every line anyone said. That went wrong
/// in `wow-cli` first -- a two-client test looked like chat never being
/// delivered, when it had arrived in a drain whose report was thrown away.
/// One dispatch table does not save a caller from ignoring what it produces.
/// Answers a teleport the server is waiting on, and moves this client to where
/// it was sent.
///
/// **Both halves are required and both fail silently.** Until the
/// acknowledgement arrives the server holds the character at the old position
/// *and discards every movement packet it sends*, so a viewer that ignores this
/// walks around a world that has stopped listening. And a viewer that
/// acknowledges without moving is now wrong about where it is by however far it
/// was sent, which shows up as the camera somewhere the server disagrees with.
///
/// Returns whether anything was answered, so a caller can log it rather than
/// wonder.
pub fn answer_teleport(live: &mut LiveWorld) -> bool {
    let Some(teleport) = live.state.pending_teleport.take() else {
        return false;
    };
    // Only ever sent about us, but checked rather than assumed: acknowledging
    // somebody else's teleport with our guid is a write, and a wrong write is
    // read as some other valid request rather than refused.
    if teleport.mover != live.guid {
        return false;
    }
    if live
        .connection
        .acknowledge_teleport(teleport.mover, teleport.counter)
        .is_err()
    {
        return false;
    }
    let at = teleport.info.position;
    live.position = Vec3::new(at.x, at.y, at.z);
    live.orientation = at.orientation;
    true
}

pub fn replicate(
    state: &mut world::WorldState,
    packets: &[world::client::Packet],
) -> world::Replication {
    state.replicate(packets, None)
}

/// Turns replicated state into what the renderer and the summary text both
/// need: drawable objects with a position and a model, excluding the
/// character's own body.
///
/// Read fresh from `state` rather than cached, since state changes every time
/// a packet is folded in -- see [`replicate`]. Callers that draw from this
/// (as opposed to describing it in text) are expected to throttle how often
/// they call it; see the viewer's entity-rebuild timer.
pub fn drawable_entities(
    state: &world::WorldState,
    own_guid: u64,
    own_position: Vec3,
) -> Vec<Entity> {
    use world::update;

    let now = std::time::Instant::now();
    let mut entities = Vec::new();
    for entity in state.iter() {
        // The player's own body is deliberately not built here. Not because
        // it should not be drawn -- it should, and [`own_entity`] does it --
        // but because *this* function reads replicated state, and replicated
        // state is wrong about where we are. The server never relays our own
        // movement back to us, so this entity's position is still wherever the
        // character logged in, however far it has since walked. Drawing from
        // it would leave the body standing at the login spot while the camera
        // walked away.
        if entity.guid == own_guid {
            continue;
        }
        // Interpolated, not the raw last-reported position: a monster move
        // only ever reports a path's start and end, and drawing the start for
        // the whole duration is exactly the jump this exists to remove.
        let Some(position) = entity.interpolated_position(now) else {
            continue;
        };
        // Only units and players carry a display id. Game objects are
        // modelled through a different table and are left for later.
        let Some(display_id) = entity.display_id() else {
            continue;
        };
        if display_id == 0 {
            continue;
        }

        entities.push(Entity {
            guid: entity.guid,
            display_id,
            position: Vec3::new(position.x, position.y, position.z),
            // Asked of the *world*, not of the entity, so a creature told to
            // face a unit rather than an angle actually turns to it -- see
            // `WorldState::facing_of`. Falls back to the entity's own answer,
            // which is what it was before and is right for everything that is
            // simply travelling.
            orientation: state
                .facing_of(
                    entity.guid,
                    now,
                    // Where *we* actually are, which replicated state does not
                    // know -- see `WorldState::facing_of`. Without this a
                    // creature attacking the player faces the player's login
                    // spot.
                    Some((
                        own_guid,
                        world::Position {
                            x: own_position.x,
                            y: own_position.y,
                            z: own_position.z,
                            orientation: 0.0,
                        },
                    )),
                )
                .unwrap_or(position.orientation),
            // Scale is a float stored in a u32 field. A missing or zero scale
            // means "normal", not "invisible".
            scale: entity
                .fields
                .get_f32(update::fields::OBJECT_SCALE)
                .filter(|s| *s > 0.0)
                .unwrap_or(1.0),
            kind: entity.object_type,
            level: entity.level(),
            // Zero when no move is in flight, which is what "standing" means
            // here -- see `world::state::Entity::move_speed`.
            speed: entity.move_speed(now).unwrap_or(0.0),
            dead: entity.is_corpse(),
            died_ms_ago: entity.dying_for(now).map(|d| d.as_millis() as u32),
            swung_ms_ago: entity.swung_ago(now).map(|d| d.as_millis() as u32),
            fighting: state.is_fighting(entity.guid),
            appearance: entity.appearance(),
            sheathed: !entity.sheath().drawn(),
        });
    }
    entities
}

/// How quickly a creature closes the gap between where it is drawn looking and
/// where it should be, as an exponential time constant in seconds.
///
/// **Deliberately not a maximum turn rate**, which is what this was first and
/// which fails in a way a player finds immediately. A fixed cap works only
/// while the target's angular speed stays under it -- and angular speed is
/// `v / r`, so a player running circles at melee range trivially exceeds any
/// cap chosen to look unhurried. Past that point the error does not settle at
/// something large, it *grows without bound*: the creature turns slower than
/// the player orbits and ends up facing nowhere in particular. Reported from
/// play as a wolf that "couldn't update fast enough" and then simply gave up
/// pointing at anything.
///
/// Closing a fixed *fraction* of the remaining error each second has no such
/// mode. A big turn -- acquiring a target -- covers most of its distance in
/// about this long and reads as a fast turn rather than a snap, while
/// continuous tracking settles at a lag of `omega * TURN_TAU` radians, which
/// at a realistic orbit is under ten degrees and stays there however fast the
/// player circles.
///
/// Chosen rather than measured, and stated as such: no table anywhere says how
/// fast a creature's head turns.
const TURN_TAU: f32 = 0.06;

/// One step of the turn: `drawn` moved `closed` of the way to `wanted`, the
/// short way round.
///
/// A free function so the behaviour can be tested without a live connection --
/// which matters here, because the property that was got wrong is only visible
/// over many steps and is invisible in any single one.
fn eased_angle(drawn: f32, wanted: f32, closed: f32) -> f32 {
    let mut delta = wanted - drawn;
    while delta > std::f32::consts::PI {
        delta -= std::f32::consts::TAU;
    }
    while delta < -std::f32::consts::PI {
        delta += std::f32::consts::TAU;
    }
    (drawn + delta * closed).rem_euclid(std::f32::consts::TAU)
}

/// How much of the remaining turn is closed over `dt` seconds.
fn turn_fraction(dt: f32) -> f32 {
    1.0 - (-dt.clamp(0.0, 0.25) / TURN_TAU).exp()
}

impl LiveWorld {
    /// Eases each entity's drawn heading toward the one the world reports.
    ///
    /// **The world's answer is a step change and a model must not be.** A
    /// creature acquiring a target goes from "facing the way it walked in" to
    /// "facing its victim" between one frame and the next, and applying that
    /// directly is the snap this exists to remove. Positions do not need
    /// this -- they arrive as paths with durations, already smooth -- which is
    /// why only the heading is carried here.
    ///
    /// The turn takes the short way round, so a creature crossing north turns
    /// a few degrees rather than most of a circle -- and closes a fraction of
    /// the remaining error rather than moving at a fixed rate, for the reason
    /// [`TURN_TAU`] gives at length.
    ///
    /// Entities that have gone are dropped, so this cannot grow for the life
    /// of the session; one that is new adopts its reported heading immediately
    /// rather than easing in from an arbitrary direction.
    pub fn ease_facings(&mut self, entities: &mut [Entity], dt: f32) {
        // Frame-rate independent: the fraction closed depends on how much time
        // passed, not on how many frames did. A long frame catches up more, so
        // a stutter does not leave every creature pointing the wrong way.
        let closed = turn_fraction(dt);
        for entity in entities.iter_mut() {
            // The player's own body is driven by the keys, which already turn
            // it smoothly; easing it again would make the character lag the
            // camera that follows it.
            if entity.guid == self.guid {
                continue;
            }
            let wanted = entity.orientation;
            let drawn = match self.facings.get(&entity.guid) {
                Some(drawn) => eased_angle(*drawn, wanted, closed),
                // Newly in view: face where it should, with nothing to ease
                // from.
                None => wanted,
            };
            self.facings.insert(entity.guid, drawn);
            entity.orientation = drawn;
        }
        let live: std::collections::HashSet<u64> = entities.iter().map(|e| e.guid).collect();
        // (The player's own guid is never inserted, so it is never retained.)
        self.facings.retain(|guid, _| live.contains(guid));
    }
}

/// The player's own body, drawn from where this client believes it is.
///
/// Split from [`drawable_entities`] because the two have different sources of
/// truth, and mixing them is the bug this exists to avoid. Everything about
/// *what the body looks like* -- its model, its size -- comes from replicated
/// state, which is authoritative for appearance and never changes. Everything
/// about *where it is* comes from the caller's own movement simulation, which
/// is the only thing that knows: the server does not echo our movement back,
/// so the replicated position is frozen at login.
///
/// `speed` likewise comes from the keys being held rather than from
/// `Entity::move_speed`, which reads the same replicated movement that never
/// arrives for us. Without it the character would slide across the ground in
/// its standing pose -- the exact bug 3.5 hit for *other* players, arriving
/// here by a different route.
pub fn own_entity(
    state: &world::WorldState,
    own_guid: u64,
    position: Vec3,
    orientation: f32,
    speed: f32,
) -> Option<Entity> {
    use world::update;

    let entity = state.get(own_guid)?;
    let display_id = entity.display_id().filter(|id| *id != 0)?;
    // Worth a line: a body that is not drawn and a body drawn somewhere
    // unexpected look identical from the outside, and this says which.
    tracing::debug!(
        "own body: display {display_id} at {:.1}, {:.1}, {:.1} facing {orientation:.2}, \
         weapon {:?}",
        position.x,
        position.y,
        position.z,
        entity.sheath(),
    );
    Some(Entity {
        guid: own_guid,
        display_id,
        position,
        orientation,
        scale: entity
            .fields
            .get_f32(update::fields::OBJECT_SCALE)
            .filter(|s| *s > 0.0)
            .unwrap_or(1.0),
        kind: entity.object_type,
        level: entity.level(),
        speed,
        // The player's own death is read from replicated state like anyone
        // else's -- it is the one part of our own condition the server *does*
        // tell us about, unlike our position.
        dead: entity.is_corpse(),
        died_ms_ago: entity
            .dying_for(std::time::Instant::now())
            .map(|d| d.as_millis() as u32),
        swung_ms_ago: entity
            .swung_ago(std::time::Instant::now())
            .map(|d| d.as_millis() as u32),
        fighting: state.is_fighting(own_guid),
        // Our own appearance is already resolved -- see `LiveWorld::look` --
        // and came from the character list rather than from these fields,
        // which is the source this project has confirmed. No reason to make
        // the caller resolve it a second way.
        appearance: None,
        // Read from replicated state like everyone else's, and unlike our
        // position: the server does echo this one back -- see
        // `world::state::Entity::sheath`.
        sheathed: !entity.sheath().drawn(),
    })
}

/// Resolves a map id to the folder its terrain lives in.
fn map_directory(chain: &mut Chain, map_id: u32) -> Result<(String, String)> {
    use dbc::schema::Map;

    let maps = Map::parse(&chain.read(Map::PATH)?)?;
    let row = maps
        .iter()
        .find(|row| row.id() == map_id)
        .with_context(|| format!("no Map.dbc row for map {map_id}"))?;

    let directory = row.directory().to_string();
    if directory.is_empty() {
        bail!("map {map_id} has no terrain directory");
    }
    Ok((directory, row.name().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A creature must not fall behind a player who circles it, however fast
    /// they circle.**
    ///
    /// This is the property the first version got wrong, and it is invisible
    /// in any single step. Easing at a fixed maximum turn rate looks correct
    /// frame by frame and is fine while the target's angular speed stays under
    /// the cap -- but angular speed is `v / r`, and a player orbiting at melee
    /// range exceeds any cap chosen to look unhurried. Past that the error
    /// does not settle at something large, it grows without bound, and the
    /// creature ends up pointing nowhere near its victim. Reported from play,
    /// three wolves running, as one that "couldn't update fast enough".
    ///
    /// Closing a fraction of the error instead bounds the lag at
    /// `omega * TURN_TAU` whatever `omega` is, which is what this asserts --
    /// at an orbit far faster than any player can actually run.
    #[test]
    fn a_turn_does_not_fall_behind_a_fast_orbit() {
        let dt = 1.0 / 60.0;
        let closed = turn_fraction(dt);
        // Six radians a second: an orbit almost a full turn per second, well
        // past anything a player achieves and well past any sane fixed cap.
        let omega = 6.0f32;

        let mut drawn = 0.0f32;
        let mut wanted = 0.0f32;
        let mut worst: f32 = 0.0;
        for step in 0..600 {
            wanted = (wanted + omega * dt).rem_euclid(std::f32::consts::TAU);
            drawn = eased_angle(drawn, wanted, closed);
            // Let it reach its steady state before measuring.
            if step > 60 {
                let mut lag = (wanted - drawn).abs();
                if lag > std::f32::consts::PI {
                    lag = std::f32::consts::TAU - lag;
                }
                worst = worst.max(lag);
            }
        }
        // omega * TURN_TAU is 0.36 rad; allow a little for the discrete steps.
        assert!(
            worst < 0.5,
            "fell {worst} rad behind a {omega} rad/s orbit; a fixed turn rate \
             would have fallen behind without limit"
        );
    }

    /// A big turn is mostly done quickly -- acquiring a target should read as
    /// a fast turn, not as a slow swing nor as a snap.
    #[test]
    fn a_large_turn_completes_promptly_without_being_instant() {
        let dt = 1.0 / 60.0;
        let closed = turn_fraction(dt);
        let wanted = std::f32::consts::PI;

        let mut drawn = 0.0f32;
        // One frame must not get there, or it is a snap.
        drawn = eased_angle(drawn, wanted, closed);
        assert!(
            (wanted - drawn).abs() > 0.5,
            "the whole turn happened in one frame"
        );
        // A quarter of a second must have all but finished it: a half turn is
        // the worst case, and `PI * exp(-0.25 / TURN_TAU)` is about three
        // degrees.
        for _ in 1..15 {
            drawn = eased_angle(drawn, wanted, closed);
        }
        assert!(
            (wanted - drawn).abs() < 0.1,
            "still {} rad short after 250ms",
            (wanted - drawn).abs()
        );
    }

    /// The turn goes the short way: a creature crossing north turns a few
    /// degrees, not most of a circle.
    #[test]
    fn a_turn_across_north_goes_the_short_way() {
        // From just below a full turn to just above zero: two headings a few
        // degrees apart.
        let drawn = std::f32::consts::TAU - 0.05;
        let wanted = 0.05;
        let stepped = eased_angle(drawn, wanted, 0.5);
        // Halfway between them the short way is the wrap point itself.
        let mut from_zero = stepped;
        if from_zero > std::f32::consts::PI {
            from_zero -= std::f32::consts::TAU;
        }
        assert!(
            from_zero.abs() < 0.05,
            "turned the long way round: landed at {stepped}"
        );
    }

    /// The assumption the whole join rests on: a position off the network is
    /// already in the space the renderer streams tiles in, with no conversion.
    ///
    /// Both positions below are what the live server actually reported.
    /// Northshire must land on Azeroth 32,48 -- the tile that really does hold
    /// the abbey -- and Shadowglen on Kalimdor 30,12.
    #[test]
    fn network_positions_are_already_world_space() {
        assert_eq!(
            crate::world::tile_at(Vec3::new(-8949.95, -132.49, 83.53)),
            (32, 48),
            "Northshire"
        );
        assert_eq!(
            crate::world::tile_at(Vec3::new(10311.3, 832.5, 1326.4)),
            (30, 12),
            "Shadowglen"
        );
    }

    /// And the trap that makes the above worth asserting.
    ///
    /// Coordinates in the *data files* do need converting -- ADT placements are
    /// stored measured inwards from the grid corner with the axes permuted --
    /// so the natural move is to reuse that conversion here. Applying it to a
    /// network position puts the camera thousands of units away, on a tile that
    /// does not exist, and the failure looks like a streaming bug rather than a
    /// coordinate one.
    #[test]
    fn the_placement_conversion_must_not_be_applied() {
        let reported = Vec3::new(-8949.95, -132.49, 83.53);
        let wrongly_converted = crate::scene::placement_position(reported.to_array());
        assert_ne!(
            crate::world::tile_at(wrongly_converted),
            crate::world::tile_at(reported),
            "the placement conversion silently agreed, so it proves nothing"
        );
    }
}
