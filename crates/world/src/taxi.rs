//! Flight paths: where a flight master will send you, and the ride itself.
//!
//! **This is the milestone where the server moves the player**, and that is
//! the fact the whole module is shaped around rather than a detail of it.
//! Everything this client has ever done to its own character it did itself:
//! the keys drive the two horizontal axes, Z is read out of the height field,
//! and the results are *reported* with `MSG_MOVE_*`. The server has never
//! contradicted any of it -- `crates/world` documents at length that our own
//! movement is never even relayed back, so replicated state holds the login
//! position forever.
//!
//! A taxi flight inverts that for its duration. The server owns the
//! character's position, sends it as a spline, and the client's job is to
//! stop driving and start following. Two consequences fall out immediately
//! and neither is optional:
//!
//! * **Input must stop being applied**, or the player walks off their own
//!   gryphon. The flags a movement packet carries are the client's statement
//!   about itself, and during a flight that statement is not the client's to
//!   make.
//! * **Whatever writes the character's altitude every frame must stop.**
//!   `position.z = ground` is unconditional and correct for four milestones
//!   of walking, and it is exactly what dragged a swimmer to the riverbed in
//!   4.18 -- a per-frame assignment silently destroying state that has to
//!   survive the frame. A flight is that same bug waiting, one thousand feet
//!   up, and it would draw as a gryphon taxiing along the ground.
//!
//! ## What comes off the wire and what comes out of the tables
//!
//! The wire is deliberately thin here. `SMSG_SHOWTAXINODES` says *which*
//! nodes this character may fly from and to, and nothing else: no names, no
//! positions, no routes. Every one of those is in the client's own tables --
//! [`dbc::schema::TaxiNodes`] for the places, `TaxiPath` for the routes and
//! `TaxiPathNode` for the waypoints -- which is why 4.25 is a *format*
//! milestone wearing a protocol milestone's clothes, and why the tables were
//! measured before a packet was sent.
//!
//! ## The mask
//!
//! Known nodes arrive as a **bit array**, fourteen `u32`s, one bit per
//! [`dbc::schema::TaxiNodes`] row id. That is the same shape as
//! `PLAYER_EXPLORED_ZONES`, which 4.17 measured against two characters whose
//! set bits fell in different words -- so the reading is not a guess here, it
//! is a shape this project has already had to get right once. The same rule
//! applies too: **an absent or zero word is "none known", never "unknown"**,
//! or a fresh character gets a flight map showing the whole continent.
//!
//! Fourteen words is 448 bits for 364 nodes, so the tail is slack rather than
//! meaningful, and a bit set past the last real node names nothing.

use crate::protocol::{Error, Reader};

/// How many `u32`s of node bitmask travel in `SMSG_SHOWTAXINODES`.
///
/// Fixed, not derived from the packet: the body has no count in it, so this
/// number *is* the layout. It makes the whole body a fixed
/// [`SHOW_NODES_BYTES`], which is the check.
pub const MASK_WORDS: usize = 14;

/// Total size of a well-formed `SMSG_SHOWTAXINODES` body.
///
/// `4 + 8 + 4 + 14 * 4`. Worth naming because a fixed-size body is the
/// cheapest possible confirmation that a layout is right -- there is no
/// variable-length block to absorb a mistake, so a wrong mask size shows up
/// as leftover bytes on the very first packet rather than as a plausible
/// wrong answer.
pub const SHOW_NODES_BYTES: usize = 4 + 8 + 4 + MASK_WORDS * 4;

/// The flight master's offer: where you are, and everywhere you may go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaxiMenu {
    /// The flight master. Sent unpacked.
    pub npc: u64,
    /// The [`dbc::schema::TaxiNodes`] row the player is standing at.
    ///
    /// **The server decides this, not the client**, and that is worth keeping
    /// rather than recomputing from the player's position. A flight master
    /// stands near its landing pad but not on it, the nearest node by
    /// distance is not always the one it serves, and the server additionally
    /// picks by *faction* -- two nodes can share a town. A client that worked
    /// it out geometrically would be right almost everywhere and silently
    /// wrong in exactly the contested places.
    pub current_node: u32,
    /// One bit per node id, fourteen words, low bit of word zero is node 0.
    pub known: [u32; MASK_WORDS],
    /// The leading `u32`, which is `1` on everything observed.
    ///
    /// **Deliberately kept and deliberately unnamed.** It is written as a
    /// literal `1` by the server with no comment, and a constant nobody has
    /// seen vary cannot be identified -- the same refusal `LiquidType`'s
    /// categories, the trainer `kind` and `TaxiPathNode`'s flags all get.
    pub unknown: u32,
}

impl TaxiMenu {
    /// Whether this character may fly from or to a node.
    ///
    /// Out-of-range ids answer `false` rather than panicking: the mask covers
    /// 448 bits and the table has 364 rows, so an id past the end is a real
    /// possibility on a modified realm and "not known" is the honest answer.
    pub fn knows(&self, node: u32) -> bool {
        let (word, bit) = (node as usize / 32, node % 32);
        self.known
            .get(word)
            .is_some_and(|w| w & (1 << bit) != 0)
    }

    /// Every node id this character may use, ascending.
    pub fn known_nodes(&self) -> impl Iterator<Item = u32> + '_ {
        (0..(MASK_WORDS * 32) as u32).filter(|node| self.knows(*node))
    }

    /// How many nodes are known. Cheap, and the number a live check reads
    /// first: a fresh character knows one or two, and a taxi cheat knows the
    /// lot, so this separates the fixture states at a glance.
    pub fn count(&self) -> u32 {
        self.known.iter().map(|word| word.count_ones()).sum()
    }
}

/// Parses `SMSG_SHOWTAXINODES`.
///
/// The body is fixed-size, so the cursor finishing exactly empty is a real
/// check rather than a formality -- see [`SHOW_NODES_BYTES`].
pub fn parse_taxi_menu(body: &[u8]) -> Result<TaxiMenu, Error> {
    let mut r = Reader::new(body, "SMSG_SHOWTAXINODES");
    let unknown = r.u32()?;
    let npc = r.u64()?;
    let current_node = r.u32()?;
    let mut known = [0u32; MASK_WORDS];
    for word in known.iter_mut() {
        *word = r.u32()?;
    }
    r.finish()?;
    Ok(TaxiMenu {
        npc,
        current_node,
        known,
        unknown,
    })
}

/// Why a flight was refused, or that it was accepted.
///
/// **Only [`TaxiReply::Ok`] is named**, and everything else prints its number.
/// That is this project's standing rule about status enums and it is worth
/// restating here because the temptation is unusually strong: the server's
/// header lists thirteen tidy names and transcribing them would take one
/// minute. A wrong *name* for a status code never errors -- it confidently
/// misexplains what happened and sends the next reader somewhere else, which
/// is precisely what `describe_cast_failure` exists to refuse. Each value
/// gets named by the session that actually produces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaxiReply {
    /// The flight was accepted and the ride is starting. Observed.
    Ok,
    /// Anything else. The number travels so a caller can report it.
    Refused(u32),
}

impl TaxiReply {
    pub fn from_code(code: u32) -> Self {
        match code {
            0 => Self::Ok,
            other => Self::Refused(other),
        }
    }

    pub fn accepted(self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// Parses `SMSG_ACTIVATETAXIREPLY`.
///
/// **The opcode that makes this whole block cheap to confirm.** It arrives
/// whether the flight was accepted or refused, so one send bounds the opcode,
/// the body layout and the reply layout at once -- the same move
/// `CMSG_GROUP_INVITE` made for parties and `CMSG_LIST_INVENTORY` for the
/// vendor block. Everything else in flight paths is silent or is confirmed by
/// the character visibly moving.
pub fn parse_activate_reply(body: &[u8]) -> Result<TaxiReply, Error> {
    let mut r = Reader::new(body, "SMSG_ACTIVATETAXIREPLY");
    let code = r.u32()?;
    r.finish()?;
    Ok(TaxiReply::from_code(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A menu with two nodes known, in **different words** of the mask.
    ///
    /// Two words rather than one, deliberately: a single set bit in word zero
    /// is consistent with the mask being read at any offset, and with the
    /// words being in either order. `PLAYER_EXPLORED_ZONES` needed exactly
    /// this and got it from two characters whose bits fell in different
    /// words; here it is built in.
    fn menu(nodes: &[u32]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&0xf130_0000_0000_0042u64.to_le_bytes());
        body.extend_from_slice(&2u32.to_le_bytes());
        let mut mask = [0u32; MASK_WORDS];
        for node in nodes {
            mask[*node as usize / 32] |= 1 << (node % 32);
        }
        for word in mask {
            body.extend_from_slice(&word.to_le_bytes());
        }
        body
    }

    #[test]
    fn the_body_is_a_fixed_size() {
        assert_eq!(menu(&[]).len(), SHOW_NODES_BYTES);
        assert_eq!(SHOW_NODES_BYTES, 72);
    }

    #[test]
    fn parses_a_menu() {
        let parsed = parse_taxi_menu(&menu(&[2, 100])).unwrap();
        assert_eq!(parsed.npc, 0xf130_0000_0000_0042);
        assert_eq!(parsed.current_node, 2);
        assert_eq!(parsed.unknown, 1);
    }

    /// The bit arithmetic, checked across a word boundary in both directions.
    /// Node 100 lives in word 3 at bit 4, and getting either half wrong reads
    /// a different node as known -- silently, since every id resolves to
    /// something in a 364-row table.
    #[test]
    fn a_bit_names_its_own_node_across_word_boundaries() {
        let parsed = parse_taxi_menu(&menu(&[2, 31, 32, 100, 447])).unwrap();
        for known in [2, 31, 32, 100, 447] {
            assert!(parsed.knows(known), "node {known} should be known");
        }
        for unknown in [0, 1, 3, 30, 33, 99, 101, 446] {
            assert!(!parsed.knows(unknown), "node {unknown} should not be");
        }
        assert_eq!(parsed.count(), 5);
        assert_eq!(
            parsed.known_nodes().collect::<Vec<_>>(),
            vec![2, 31, 32, 100, 447]
        );
    }

    /// **An empty mask is "none known", never "unknown".** A fresh character
    /// really does know nothing, and reading that as missing information
    /// would hand them a map of the whole continent -- the same rule
    /// `PLAYER_EXPLORED_ZONES` needed, where an absent word is a zero.
    #[test]
    fn an_empty_mask_is_no_nodes_rather_than_unknown() {
        let parsed = parse_taxi_menu(&menu(&[])).unwrap();
        assert_eq!(parsed.count(), 0);
        assert_eq!(parsed.known_nodes().count(), 0);
        assert!(!parsed.knows(0));
    }

    /// An id past the mask is answered rather than panicked on.
    #[test]
    fn a_node_past_the_mask_is_simply_not_known() {
        let parsed = parse_taxi_menu(&menu(&[2])).unwrap();
        assert!(!parsed.knows(448));
        assert!(!parsed.knows(u32::MAX));
    }

    /// Both running out of input and having input left over are errors.
    #[test]
    fn a_body_of_the_wrong_size_is_refused() {
        let mut short = menu(&[]);
        short.pop();
        assert!(parse_taxi_menu(&short).is_err());

        let mut long = menu(&[]);
        long.push(0);
        assert!(parse_taxi_menu(&long).is_err());
    }

    /// Only zero is named. The rest keep their number so a report can quote
    /// it, and none of them are given a meaning this project has not seen.
    #[test]
    fn only_the_accepted_code_is_named() {
        assert_eq!(
            parse_activate_reply(&0u32.to_le_bytes()).unwrap(),
            TaxiReply::Ok
        );
        assert!(parse_activate_reply(&0u32.to_le_bytes()).unwrap().accepted());
        for code in [1u32, 4, 6, 11, 99] {
            let reply = parse_activate_reply(&code.to_le_bytes()).unwrap();
            assert_eq!(reply, TaxiReply::Refused(code));
            assert!(!reply.accepted());
        }
    }
}

/// A flight in progress: the route the server sent, and how far along it is.
///
/// **The client does not choose this path and must not improve on it.** The
/// route is whatever the server put in the spline, and the same points also
/// exist in `TaxiPathNode` -- which makes the table tempting, since it is
/// right there and needs no packet. Preferring it would be the picking-ray
/// rule from the other side: two derivations of one fact agree until one of
/// them changes, and here the disagreement draws a character flying beside
/// their own gryphon on any realm whose path rows have been edited.
///
/// Interpolation is **linear between the points, by arc length**. The real
/// client curves through them, and the difference is a metre of cornering on
/// a route measured in hundreds. Stated rather than hidden: it is a
/// deliberate approximation, and the type that makes it is the honest place
/// to record it.
#[derive(Debug, Clone, PartialEq)]
pub struct Flight {
    /// Every point of the route, in order, as the server sent them.
    points: Vec<(f32, f32, f32)>,
    /// Cumulative distance to each point.
    ///
    /// Precomputed because the alternative is measuring the whole polyline
    /// once per frame, and because it is what makes "half way" mean half the
    /// *distance* rather than half the *points* -- a route whose points bunch
    /// up at one end would otherwise crawl there and race elsewhere.
    lengths: Vec<f32>,
    /// Milliseconds the whole route takes, from the packet.
    duration_ms: u32,
}

impl Flight {
    /// Builds a flight from a monster-move's spline.
    ///
    /// Returns `None` for anything that cannot describe a route: fewer than
    /// two points, a zero duration, or a route of zero length. All three are
    /// real packet shapes -- a stop carries no points at all -- and none is
    /// an error worth reporting, so the absence is the answer.
    pub fn new(points: &[(f32, f32, f32)], duration_ms: u32) -> Option<Self> {
        if points.len() < 2 || duration_ms == 0 {
            return None;
        }
        let mut lengths = Vec::with_capacity(points.len());
        let mut total = 0.0;
        lengths.push(0.0);
        for pair in points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (dx, dy, dz) = (b.0 - a.0, b.1 - a.1, b.2 - a.2);
            total += (dx * dx + dy * dy + dz * dz).sqrt();
            lengths.push(total);
        }
        if total <= 0.0 {
            return None;
        }
        Some(Self {
            points: points.to_vec(),
            lengths,
            duration_ms,
        })
    }

    pub fn duration_ms(&self) -> u32 {
        self.duration_ms
    }

    /// Total length of the route in world units.
    pub fn length(&self) -> f32 {
        *self.lengths.last().unwrap_or(&0.0)
    }

    /// How many points the server sent.
    pub fn points(&self) -> usize {
        self.points.len()
    }

    /// Whether the flight has run out.
    pub fn finished(&self, elapsed_ms: u32) -> bool {
        elapsed_ms >= self.duration_ms
    }

    /// Where the character is `elapsed_ms` into the flight, and which way it
    /// is heading.
    ///
    /// The heading comes from the **segment being flown**, not from a
    /// difference between successive frames. The two look identical while
    /// moving and differ at a corner and at a stall -- and a frame-difference
    /// heading is undefined exactly when the character stops, which is what
    /// left creatures facing a constant wrong direction before 3.5.
    pub fn at(&self, elapsed_ms: u32) -> ((f32, f32, f32), f32) {
        let clamped = elapsed_ms.min(self.duration_ms);
        let fraction = f64::from(clamped) / f64::from(self.duration_ms);
        let target = (fraction as f32) * self.length();

        let mut segment = match self.lengths.binary_search_by(|probe| {
            probe.partial_cmp(&target).unwrap_or(std::cmp::Ordering::Less)
        }) {
            Ok(exact) => exact,
            Err(after) => after.saturating_sub(1),
        };
        segment = segment.min(self.points.len() - 2);

        let (a, b) = (self.points[segment], self.points[segment + 1]);
        let span = self.lengths[segment + 1] - self.lengths[segment];
        let along = if span > 0.0 {
            ((target - self.lengths[segment]) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let position = (
            a.0 + (b.0 - a.0) * along,
            a.1 + (b.1 - a.1) * along,
            a.2 + (b.2 - a.2) * along,
        );
        let heading = (b.1 - a.1).atan2(b.0 - a.0);
        (position, heading)
    }
}

#[cfg(test)]
mod flight_tests {
    use super::*;

    /// An L-shaped route whose legs are deliberately different lengths -- 100
    /// east then 300 north. Equal legs would make "half the distance" and
    /// "half the points" the same answer, which is exactly the degeneracy
    /// that would let a points-based interpolation pass.
    fn route() -> Flight {
        Flight::new(
            &[(0.0, 0.0, 0.0), (100.0, 0.0, 0.0), (100.0, 300.0, 0.0)],
            4000,
        )
        .unwrap()
    }

    #[test]
    fn the_ends_are_the_ends() {
        let f = route();
        assert_eq!(f.at(0).0, (0.0, 0.0, 0.0));
        assert_eq!(f.at(4000).0, (100.0, 300.0, 0.0));
        assert_eq!(f.length(), 400.0);
        assert_eq!(f.points(), 3);
    }

    /// **Progress is by distance, not by point count.** A quarter of the time
    /// is 100 units along a 400-unit route, which lands exactly on the corner.
    /// Splitting the time evenly between segments would put the corner at the
    /// halfway mark instead, drawing a gryphon that dawdles down the short leg
    /// and sprints up the long one.
    #[test]
    fn a_fraction_of_the_time_is_that_fraction_of_the_distance() {
        let f = route();
        assert_eq!(f.at(1000).0, (100.0, 0.0, 0.0), "quarter time = the corner");
        assert_eq!(f.at(2000).0, (100.0, 100.0, 0.0), "half time = 200 units");
    }

    /// The heading is the segment's, so it is right on the first frame rather
    /// than after two.
    #[test]
    fn the_heading_comes_from_the_segment_being_flown() {
        let f = route();
        assert!(f.at(500).1.abs() < 1e-5, "first leg runs east");
        assert!(
            (f.at(3000).1 - std::f32::consts::FRAC_PI_2).abs() < 1e-5,
            "second leg runs north"
        );
    }

    /// Past the end it holds the destination rather than running off it. The
    /// caller ends the flight on [`Flight::finished`]; this must not depend on
    /// the caller being punctual.
    #[test]
    fn overrunning_holds_the_destination() {
        let f = route();
        assert_eq!(f.at(99_999).0, (100.0, 300.0, 0.0));
        assert!(f.finished(4000));
        assert!(!f.finished(3999));
    }

    /// Packets that cannot describe a route are refused rather than
    /// half-accepted.
    #[test]
    fn a_route_that_is_not_one_is_refused() {
        assert!(Flight::new(&[], 1000).is_none());
        assert!(Flight::new(&[(1.0, 2.0, 3.0)], 1000).is_none());
        assert!(Flight::new(&[(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)], 0).is_none());
        assert!(Flight::new(&[(5.0, 5.0, 5.0), (5.0, 5.0, 5.0)], 1000).is_none());
    }

    /// Altitude is interpolated like everything else -- the check that a
    /// flight genuinely leaves the ground rather than tracking it.
    #[test]
    fn altitude_is_part_of_the_route() {
        let f =
            Flight::new(&[(0.0, 0.0, 100.0), (0.0, 0.0, 100.0), (0.0, 200.0, 300.0)], 1000)
                .unwrap();
        assert!((f.at(500).0 .2 - 200.0).abs() < 0.01);
    }
}
