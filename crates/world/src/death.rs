//! Dying, releasing, and getting the body back.
//!
//! Three states, not two, and the difference is the whole of a corpse run. A
//! player who has been killed lies where they fell with no health and no corpse
//! *object* in the world. Releasing turns them into a ghost at a graveyard and
//! creates the corpse object they now have to run back to. Reclaiming it puts
//! them back in their body.
//!
//! `Entity::is_dead_or_ghost` and `Entity::is_ghost` read the first two apart;
//! this module carries what the server volunteers about the third -- where the
//! graveyard is, and how long the body has to lie there first.

use crate::protocol::{Error, Reader};

/// Where a released ghost was sent, straight from the server.
///
/// **The graveyard is chosen by the server, not by the client.** That is worth
/// stating because the obvious design is the opposite -- pick the nearest row
/// of `WorldSafeLocs.dbc` and walk there -- and building it that way would put
/// a table lookup on the critical path of a feature that needs none. The table
/// is only wanted later, to put a *name* on a place we are already being told
/// the coordinates of.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReleaseLocation {
    /// The map the graveyard is on, or `None` for the "clear the marker" form.
    pub map: Option<u32>,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl ReleaseLocation {
    /// Whether this is the form that removes the marker rather than placing
    /// one. The same opcode does both jobs, separated only by a map of
    /// `0xFFFFFFFF` -- which is why `map` is an `Option` here and not a bare
    /// number a caller would have to remember to check.
    pub fn is_clear(&self) -> bool {
        self.map.is_none()
    }
}

/// Reads `SMSG_DEATH_RELEASE_LOC`: `{u32 map, f32 x, f32 y, f32 z}`.
pub fn parse_release_location(body: &[u8]) -> Result<ReleaseLocation, Error> {
    let mut r = Reader::new(body, "SMSG_DEATH_RELEASE_LOC");
    let map = r.u32()?;
    let location = ReleaseLocation {
        map: (map != u32::MAX).then_some(map),
        x: r.f32()?,
        y: r.f32()?,
        z: r.f32()?,
    };
    r.finish()?;
    Ok(location)
}

/// The answer to "where is my body", as the server gives it.
///
/// **This exists because "the corpse in view" is not answerable from the
/// replicated world.** A graveyard collects corpse-type objects -- real bodies
/// and the bones left behind by ones already reclaimed -- and they all carry
/// the same owner. One run here saw *seven* while the server had two, and
/// picking by owner alone chose a stale one at the previous death site, ran
/// fifty-eight yards to it and was refused. The server knows which body is
/// current; asking is both simpler and correct.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CorpseLocation {
    /// The map to show the body on, which is the *entrance* map when the body
    /// is inside a dungeon and the player is not.
    pub map: i32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// The map the body is really on. Equal to [`Self::map`] outside dungeons,
    /// and the pair is kept separate rather than collapsed because a run back
    /// wants the first and a "which instance" question wants the second.
    pub corpse_map: i32,
}

/// Reads `MSG_CORPSE_QUERY`'s reply. `None` means the server says there is no
/// corpse -- a one-byte body, distinct from a parse failure.
pub fn parse_corpse_query(body: &[u8]) -> Result<Option<CorpseLocation>, Error> {
    let mut r = Reader::new(body, "MSG_CORPSE_QUERY");
    if r.u8()? == 0 {
        r.finish()?;
        return Ok(None);
    }
    let found = CorpseLocation {
        map: r.u32()? as i32,
        x: r.f32()?,
        y: r.f32()?,
        z: r.f32()?,
        corpse_map: r.u32()? as i32,
    };
    // A trailing word the server documents only as "unknown", read so the
    // cursor still has to end level. Refusing to name it is deliberate; so is
    // refusing to ignore it.
    let _unknown = r.u32()?;
    r.finish()?;
    Ok(Some(found))
}

/// Reads `SMSG_CORPSE_RECLAIM_DELAY`: milliseconds before the body can be
/// taken back.
///
/// Sent on death, and observed carrying exactly `30000`. Kept as the number on
/// the wire rather than a `Duration` for the same reason every other timer here
/// is: the wire's unit is the thing being tested, and converting on the way in
/// hides a factor-of-a-thousand mistake behind a plausible-looking value.
pub fn parse_reclaim_delay(body: &[u8]) -> Result<u32, Error> {
    let mut r = Reader::new(body, "SMSG_CORPSE_RECLAIM_DELAY");
    let delay = r.u32()?;
    r.finish()?;
    Ok(delay)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes of a real graveyard, as they would arrive for a human dying in
    /// Elwynn: map 0 and a position on it.
    #[test]
    fn a_graveyard_parses_and_is_not_a_clear() {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&(-8_749.1f32).to_le_bytes());
        body.extend_from_slice(&1_042.4f32.to_le_bytes());
        body.extend_from_slice(&91.5f32.to_le_bytes());
        let at = parse_release_location(&body).unwrap();
        assert_eq!(at.map, Some(0));
        assert!(!at.is_clear());
    }

    /// The same opcode with a map of `0xFFFFFFFF` means "take the marker off
    /// the minimap", and a caller that read `map` as a plain number would send
    /// the player to map 4294967295.
    #[test]
    fn an_all_ones_map_is_a_clear_rather_than_a_destination() {
        let mut body = Vec::new();
        body.extend_from_slice(&u32::MAX.to_le_bytes());
        body.extend_from_slice(&0f32.to_le_bytes());
        body.extend_from_slice(&0f32.to_le_bytes());
        body.extend_from_slice(&0f32.to_le_bytes());
        let at = parse_release_location(&body).unwrap();
        assert!(at.is_clear());
        assert_eq!(at.map, None);
    }

    /// Cursor discipline: running out of input and having input left over are
    /// both errors, which is what has caught four separate world-protocol bugs
    /// in this crate already.
    #[test]
    fn a_short_or_long_body_is_refused() {
        assert!(parse_release_location(&[0; 15]).is_err());
        assert!(parse_release_location(&[0; 17]).is_err());
        assert!(parse_reclaim_delay(&[0; 3]).is_err());
        assert!(parse_reclaim_delay(&[0; 5]).is_err());
    }

    #[test]
    fn the_reclaim_delay_is_milliseconds_off_the_wire() {
        assert_eq!(parse_reclaim_delay(&30_000u32.to_le_bytes()).unwrap(), 30_000);
    }

    /// "No corpse" is one byte and is not an error -- a client that treated it
    /// as one would report a broken query every time a living player asked.
    #[test]
    fn no_corpse_is_a_one_byte_answer_rather_than_a_failure() {
        assert_eq!(parse_corpse_query(&[0]).unwrap(), None);
    }

    #[test]
    fn a_corpse_location_parses_and_consumes_its_body() {
        let mut body = vec![1];
        body.extend_from_slice(&0i32.to_le_bytes());
        body.extend_from_slice(&(-8_949.95f32).to_le_bytes());
        body.extend_from_slice(&(-132.49f32).to_le_bytes());
        body.extend_from_slice(&83.53f32.to_le_bytes());
        body.extend_from_slice(&0i32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        let at = parse_corpse_query(&body).unwrap().unwrap();
        assert_eq!(at.map, 0);
        assert!((at.x + 8_949.95).abs() < 0.01);
        // One byte short and one byte long are both errors.
        assert!(parse_corpse_query(&body[..body.len() - 1]).is_err());
    }
}
