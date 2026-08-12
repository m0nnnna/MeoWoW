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
    let burst = connection.drain(std::time::Duration::from_millis(1500), 512)?;
    let mut state = world::WorldState::new();
    let fold_failures = replicate(&mut state, &burst);

    let (map_directory, map_name) = map_directory(chain, landed.map)?;
    Ok(LiveWorld {
        character: character.name.clone(),
        guid: character.guid,
        map_id: landed.map,
        map_directory,
        map_name,
        // Already in world space -- see the module comment.
        position: Vec3::new(landed.x, landed.y, landed.z),
        orientation: landed.orientation,
        state,
        fold_failures,
        connection,
    })
}

/// Folds a batch of drained packets into replicated world state.
///
/// A thin wrapper over `WorldState::replicate`, which is the single place
/// this opcode dispatch lives -- it used to be duplicated here and in
/// `tools/wow-cli`, and two independent tables over the same state machine
/// drift silently: a new opcode wired into one and not the other freezes
/// whatever it should have moved, unnoticed. The viewer only wants a failure
/// count, so that is all this returns; `wow-cli` wants the fuller
/// `world::Replication` and calls the method directly.
pub fn replicate(state: &mut world::WorldState, packets: &[world::client::Packet]) -> usize {
    state.replicate(packets, None).failures.len()
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
