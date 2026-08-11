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
    pub position: Vec3,
    pub orientation: f32,
    pub scale: f32,
    pub kind: world::ObjectType,
    pub level: Option<u32>,
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
    pub entities: Vec<Entity>,
    /// Objects seen but not drawable, and why. Reported rather than hidden:
    /// silently drawing a subset of the world looks like a rendering bug.
    pub skipped: Vec<String>,
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
    let (entities, skipped) = collect_entities(&burst, character.guid);

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
        entities,
        skipped,
        connection,
    })
}

/// Pulls the drawable objects out of a burst of update packets.
///
/// A packet that fails to parse costs its own contents and nothing else, so it
/// is counted and skipped rather than aborting: losing one packet should not
/// turn into an empty world.
fn collect_entities(
    packets: &[world::client::Packet],
    own_guid: u64,
) -> (Vec<Entity>, Vec<String>) {
    use world::update::{self, Block};

    let mut entities = Vec::new();
    let mut skipped = Vec::new();
    let mut failed = 0usize;
    let mut without_model = 0usize;

    for packet in packets {
        let blocks = match packet.opcode {
            world::opcode::server::UPDATE_OBJECT => update::parse_update_object(&packet.body),
            world::opcode::server::COMPRESSED_UPDATE_OBJECT => {
                update::parse_compressed_update_object(&packet.body)
            }
            _ => continue,
        };
        let Ok(blocks) = blocks else {
            failed += 1;
            continue;
        };

        for block in blocks {
            let Block::Create {
                guid,
                object_type,
                movement,
                fields,
                ..
            } = block
            else {
                continue;
            };
            // The player's own body is where the camera is; drawing it would
            // fill the view from inside the mesh.
            if guid == own_guid {
                continue;
            }
            let Some(position) = movement.position else {
                continue;
            };
            // Only units and players carry a display id. Game objects are
            // modelled through a different table and are left for later.
            let Some(display_id) = fields.get(update::fields::UNIT_DISPLAY_ID) else {
                without_model += 1;
                continue;
            };
            if display_id == 0 {
                without_model += 1;
                continue;
            }

            entities.push(Entity {
                guid,
                display_id,
                position: Vec3::new(position.x, position.y, position.z),
                orientation: position.orientation,
                // Scale is a float stored in a u32 field. A missing or zero
                // scale means "normal", not "invisible".
                scale: fields
                    .get_f32(update::fields::OBJECT_SCALE)
                    .filter(|s| *s > 0.0)
                    .unwrap_or(1.0),
                kind: object_type,
                level: fields.get(update::fields::UNIT_LEVEL),
            });
        }
    }

    if failed > 0 {
        skipped.push(format!("{failed} update packet(s) failed to parse"));
    }
    if without_model > 0 {
        skipped.push(format!("{without_model} object(s) with no display id"));
    }
    (entities, skipped)
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
