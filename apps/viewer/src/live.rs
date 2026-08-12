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
    /// Whether a monster move is currently in flight for this entity, which
    /// is what the renderer uses to decide whether to animate a walk cycle
    /// rather than draw the bind pose.
    pub moving: bool,
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
    /// Kept alive rather than dropped at the end of [`connect`]: the viewer
    /// walks the character over this same connection, and RC4 header state
    /// cannot be shared or rewound, so a fresh connection could not pick up
    /// where this one left off.
    pub connection: world::Connection,
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
        fold_failures: 0,
        reported_failures: std::collections::HashSet::new(),
        connection,
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
pub fn drawable_entities(state: &world::WorldState, own_guid: u64) -> Vec<Entity> {
    use world::update;

    let now = std::time::Instant::now();
    let mut entities = Vec::new();
    for entity in state.iter() {
        // The player's own body is where the camera is; drawing it would
        // fill the view from inside the mesh.
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
            orientation: position.orientation,
            // Scale is a float stored in a u32 field. A missing or zero scale
            // means "normal", not "invisible".
            scale: entity
                .fields
                .get_f32(update::fields::OBJECT_SCALE)
                .filter(|s| *s > 0.0)
                .unwrap_or(1.0),
            kind: entity.object_type,
            level: entity.level(),
            moving: entity.is_moving(now),
        });
    }
    entities
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
