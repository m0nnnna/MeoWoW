//! Quests, and where their objectives are on the map.
//!
//! **This module is why this client does not ship anybody's quest database.**
//! WotLK shipped its own quest tracker, so the server already stores the map
//! markers and hands them over on request -- 8,953 of 9,464 quests carry them
//! on the development realm. Asking is strictly better than shipping a copy:
//! the answer cannot drift as content is patched, and it is *correct* on a
//! private realm with custom quests, where a shipped database is simply wrong.
//!
//! **The one constraint shapes everything built on top.** The server answers
//! only for quests **in the player's own log**, and takes at most 25 ids per
//! request. So this covers "where do I go for the quest I am on" completely,
//! and says nothing whatever about a quest not yet accepted -- which is a
//! different feature needing a different source.
//!
//! Confirmed field by field against the server's own `quest_poi` and
//! `quest_poi_points` tables, which the client is never sent:
//!
//! | quest | on the wire | in the database |
//! |---|---|---|
//! | 85 | area 30, point (-9924, 38) | area 30, (-9924, 38) |
//! | 106 | area 39, point (-9930, 500) | area 39, (-9930, 500) |
//! | 783 | area 30, point (-8903, -163) | area 30, (-8903, -163) |
//! | 16 | 15 marker areas | 15 |
//!
//! And a sanity check no table could give: quest 783's marker is *Speak with
//! Marshal McBride*, and it arrived at (-8903, -163) while the character
//! standing in Northshire was at (-8928, -187). The numbers are real world
//! coordinates in the right place, not merely well-formed.

use crate::protocol::{Error, Reader};

/// One marker area for a quest objective -- a polygon on the world map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestPoi {
    /// The marker's index within this quest's own set.
    pub id: u32,
    /// Which objective this marks, or `None` for a marker belonging to the
    /// quest as a whole rather than to one of its objectives.
    ///
    /// **Signed on the wire and `-1` is the common case** -- all three
    /// single-objective quests captured carry it. Reading it unsigned would
    /// report objective 4,294,967,295 and quietly attach every marker to an
    /// objective that does not exist.
    pub objective_index: Option<u32>,
    pub map_id: u32,
    /// A row in `WorldMapArea.dbc`, **not** `AreaTable.dbc**. Named after the
    /// server's own column (`quest_poi.WorldMapAreaId`) rather than after a
    /// guess: the two tables are different and both are keyed by small
    /// integers, so a wrong reading would resolve to a plausible wrong place.
    pub world_map_area_id: u32,
    pub floor_id: u32,
    /// The polygon. One point is a pin; several outline a region.
    ///
    /// **Coordinates are world coordinates and are signed**, which the capture
    /// makes unambiguous: Northshire sits at negative x and negative y, and
    /// every point captured there is negative in both.
    pub points: Vec<(i32, i32)>,
    /// Two fields nothing has explained. Zero and one on every marker
    /// captured, and **deliberately unnamed**: the server's own struct calls
    /// them `Unk3` and `Unk4`, so there is nothing to transcribe even if
    /// transcribing were wanted.
    pub unknown: (u32, u32),
}

/// Every marker the server returned for one quest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestPoiSet {
    pub quest_id: u32,
    /// **Empty is a real and common answer**, not a failure. The server sends
    /// a zero count for a quest it has no markers for *and* for a quest that
    /// is not in the player's log -- the two are indistinguishable here, which
    /// is worth knowing before reading an empty set as "this quest has no
    /// objectives on the map".
    pub markers: Vec<QuestPoi>,
}

/// Parses `SMSG_QUEST_POI_QUERY_RESPONSE`.
///
/// Read through a cursor that must end exactly at the end of the body. This
/// packet nests three levels of counted array, so a single miscount does not
/// shift one value -- it desynchronises everything after it, and a coordinate
/// read from the wrong offset is still a plausible-looking number.
pub fn parse_quest_poi(body: &[u8]) -> Result<Vec<QuestPoiSet>, Error> {
    let mut r = Reader::new(body, "SMSG_QUEST_POI_QUERY_RESPONSE");

    let quests = r.u32()?;
    let mut sets = Vec::with_capacity(quests as usize);
    for _ in 0..quests {
        let quest_id = r.u32()?;
        let marker_count = r.u32()?;
        let mut markers = Vec::with_capacity(marker_count as usize);
        for _ in 0..marker_count {
            let id = r.u32()?;
            // Signed: -1 means "not tied to one objective". See the field.
            let objective_index = match r.u32()? as i32 {
                negative if negative < 0 => None,
                index => Some(index as u32),
            };
            let map_id = r.u32()?;
            let world_map_area_id = r.u32()?;
            let floor_id = r.u32()?;
            let unknown = (r.u32()?, r.u32()?);
            let point_count = r.u32()?;
            let mut points = Vec::with_capacity(point_count as usize);
            for _ in 0..point_count {
                points.push((r.u32()? as i32, r.u32()? as i32));
            }
            markers.push(QuestPoi {
                id,
                objective_index,
                map_id,
                world_map_area_id,
                floor_id,
                points,
                unknown,
            });
        }
        sets.push(QuestPoiSet { quest_id, markers });
    }

    r.finish()?;
    Ok(sets)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first three quests of a live reply, verbatim, with the quest count
    /// edited from 4 to 3 and the fourth quest's bytes dropped.
    ///
    /// Three rather than one because the check that matters is every quest's
    /// area and coordinates agreeing with the server's tables at three
    /// different offsets.
    const CAPTURED: [u8; 4 + 3 * 48] = [
        0x03, 0x00, 0x00, 0x00, // three quests
        // quest 106, one marker, area 39, point (-9930, 500)
        0x6a, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, //
        0x00, 0x00, 0x00, 0x00, // marker id 0
        0xff, 0xff, 0xff, 0xff, // objective -1
        0x00, 0x00, 0x00, 0x00, // map 0
        0x27, 0x00, 0x00, 0x00, // world map area 39
        0x00, 0x00, 0x00, 0x00, // floor 0
        0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, // unknown pair
        0x01, 0x00, 0x00, 0x00, // one point
        0x36, 0xd9, 0xff, 0xff, 0xf4, 0x01, 0x00, 0x00, // (-9930, 500)
        // quest 85, one marker, area 30, point (-9924, 38)
        0x55, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, //
        0x00, 0x00, 0x00, 0x00, //
        0xff, 0xff, 0xff, 0xff, //
        0x00, 0x00, 0x00, 0x00, //
        0x1e, 0x00, 0x00, 0x00, // world map area 30
        0x00, 0x00, 0x00, 0x00, //
        0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, //
        0x01, 0x00, 0x00, 0x00, //
        0x3c, 0xd9, 0xff, 0xff, 0x26, 0x00, 0x00, 0x00, // (-9924, 38)
        // quest 783, one marker, area 30, point (-8903, -163)
        0x0f, 0x03, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, //
        0x00, 0x00, 0x00, 0x00, //
        0xff, 0xff, 0xff, 0xff, //
        0x00, 0x00, 0x00, 0x00, //
        0x1e, 0x00, 0x00, 0x00, //
        0x00, 0x00, 0x00, 0x00, //
        0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, //
        0x01, 0x00, 0x00, 0x00, //
        0x39, 0xdd, 0xff, 0xff, 0x5d, 0xff, 0xff, 0xff, // (-8903, -163)
    ];

    #[test]
    fn a_captured_response_parses_exactly() {
        let sets = parse_quest_poi(&CAPTURED).unwrap();
        assert_eq!(sets.len(), 3);
        assert_eq!(
            sets.iter().map(|s| s.quest_id).collect::<Vec<_>>(),
            vec![106, 85, 783]
        );
        assert!(sets.iter().all(|s| s.markers.len() == 1));
    }

    /// **The check that makes the rest trustworthy.**
    ///
    /// Every area and coordinate here lives in the server's `quest_poi` and
    /// `quest_poi_points` tables, which no client is ever sent. Three quests
    /// agreeing at three different offsets cannot happen by accident, and a
    /// reading shifted by one field would still parse.
    #[test]
    fn the_markers_agree_with_the_servers_own_poi_tables() {
        let sets = parse_quest_poi(&CAPTURED).unwrap();
        let seen: Vec<(u32, u32, (i32, i32))> = sets
            .iter()
            .map(|set| {
                let marker = &set.markers[0];
                (set.quest_id, marker.world_map_area_id, marker.points[0])
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                (106, 39, (-9930, 500)),
                (85, 30, (-9924, 38)),
                (783, 30, (-8903, -163)),
            ]
        );
    }

    /// **Coordinates are signed, and Northshire is the proof.**
    ///
    /// Quest 783's marker is "speak with Marshal McBride", and it arrived at
    /// (-8903, -163) while the character standing in Northshire was at
    /// (-8928, -187) -- twenty-five yards away. Read unsigned, those become
    /// four-billion-ish numbers on the far side of the map, which is a bug no
    /// length check would catch.
    #[test]
    fn coordinates_are_signed_world_positions() {
        let sets = parse_quest_poi(&CAPTURED).unwrap();
        let (x, y) = sets[2].markers[0].points[0];
        assert!(x < 0 && y < 0, "Northshire is negative in both axes");
        // Within sight of where the character actually stood.
        assert!((x - -8928).abs() < 100 && (y - -187).abs() < 100);
    }

    /// `-1` means "not one objective in particular" and must not read as four
    /// billion.
    #[test]
    fn a_negative_objective_index_reads_as_absent() {
        let sets = parse_quest_poi(&CAPTURED).unwrap();
        assert!(sets
            .iter()
            .all(|s| s.markers[0].objective_index.is_none()));
    }

    /// A real objective index survives the signed read, so the conversion
    /// cannot have turned every index into `None`.
    #[test]
    fn a_real_objective_index_survives() {
        let mut body = CAPTURED.to_vec();
        // The first marker's objective index.
        body[16..20].copy_from_slice(&4u32.to_le_bytes());
        let sets = parse_quest_poi(&body).unwrap();
        assert_eq!(sets[0].markers[0].objective_index, Some(4));
        // And nothing after it moved.
        assert_eq!(sets[0].markers[0].world_map_area_id, 39);
        assert_eq!(sets[1].quest_id, 85);
    }

    /// A quest with no markers is a real answer -- and it is what a quest
    /// *not in the log* looks like too, which is why the doc says so.
    #[test]
    fn a_quest_with_no_markers_parses() {
        let body = [
            0x01, 0x00, 0x00, 0x00, // one quest
            0x0f, 0x03, 0x00, 0x00, // quest 783
            0x00, 0x00, 0x00, 0x00, // no markers
        ];
        let sets = parse_quest_poi(&body).unwrap();
        assert_eq!(sets[0].quest_id, 783);
        assert!(sets[0].markers.is_empty());
    }

    /// A count that overruns must fail rather than return what it managed.
    #[test]
    fn a_truncated_reply_is_an_error() {
        let mut body = CAPTURED.to_vec();
        body[0] = 4; // claims four quests, carries three
        assert!(parse_quest_poi(&body).is_err());
    }

    /// Trailing bytes are an error too -- the half that catches a field read
    /// too narrow.
    #[test]
    fn trailing_bytes_are_an_error() {
        let mut body = CAPTURED.to_vec();
        body.push(0);
        assert!(parse_quest_poi(&body).is_err());
    }
}
