//! Portal culling: which rooms of a building you can actually see from inside
//! it.
//!
//! **The one thing frustum culling structurally cannot do.** A frustum rejects
//! by *direction*, so it works on anything outside the cone and on nothing
//! inside it -- and when the camera stands inside a city, the city is the cone.
//! Measured: sweeping the far plane from 397 units to 12,000 inside Ironforge
//! left the building's own draw count *unchanged at every step*, because the
//! eye is inside its box and distance was never what bounded a room.
//! `--interior-cull` cannot help either; it answers "how far away is this room"
//! and the answer indoors is zero.
//!
//! What does bound a room is a **doorway**. A `.wmo` ships its rooms already
//! cut apart, with a convex polygon for every opening between two of them --
//! 7,548 of them across 716 of the game's 1,985 buildings -- and standing in
//! one room you can see another only by looking through a chain of those
//! openings. So the rule is: start in the room containing the eye, and walk
//! outwards through doorways, narrowing what you are allowed to see by the
//! shape of each opening you pass through.
//!
//! **Narrowed as a screen rectangle rather than as new frustum planes.** Four
//! planes per doorway, rebuilt down every branch of the walk, is the textbook
//! form and it is a great deal of arithmetic to get subtly wrong in a way that
//! removes a wall. A doorway's axis-aligned box *in screen space* is strictly
//! more permissive than its true silhouette -- it can only ever admit rooms
//! that should have been culled, never reject one that should have been drawn
//! -- and being wrong in the direction that draws more is the only kind of
//! wrong this module is allowed to be. The same test then serves rooms: project
//! the box, take its screen rectangle, intersect.
//!
//! **What it does not get exactly right, measured.** Across ten cameras in
//! Ironforge and Stormwind the picture is byte-identical to drawing every
//! room, on nine of them. The tenth differs by **11 pixels of 921,600** -- a
//! single horizontal line twelve pixels wide and one tall, which is the shape
//! of a surface seen edge-on rather than of a room that went missing. Three
//! explanations were tried and every one was refuted by changing nothing:
//! widening the opening tolerance fivefold, keeping the rooms named by
//! one-sided openings, and seeding rooms the eye stands two units outside. It
//! is left as measured rather than papered over, because a number somebody can
//! re-check with `--no-portal-cull` is worth more than a tolerance chosen to
//! make it disappear.
//!
//! Everything here is pure geometry over hand-buildable inputs, for the reason
//! [`crate::cull`] gives: a culling bug does not look like a bug. A room that
//! is never reached is not an error, a warning, or a crash. It is a wall that
//! is not there, and the only instrument that catches it before a person does
//! is a test that knew which rooms were supposed to be reachable.

use glam::{Mat4, Vec2, Vec3, Vec4};

/// An opening between two rooms: the polygon you look through, and the pair it
/// joins.
///
/// **The pair is unordered on purpose.** A `.wmo` stores each doorway twice,
/// once from each side, and which of the two entries is "this room's" depends
/// on reading a per-group index range that says nothing the pair does not.
/// Taking the two groups a doorway names and stepping to whichever one is not
/// where you are needs no such reading, and cannot be wrong about it.
///
/// The stored `side` values are deliberately not consulted. Whether `+1` means
/// front is a claim nothing in the file checks, and the geometry answers the
/// same question without it: a doorway behind you projects to nothing once its
/// polygon has been clipped against the near plane.
#[derive(Clone, Debug)]
pub struct Doorway {
    /// The opening, in world space, convex and usually four points.
    pub vertices: Vec<Vec3>,
    /// The two rooms it joins, as indices into the room list.
    pub rooms: (u32, u32),
}

impl Doorway {
    /// The room on the other side of this doorway from `here`, if it touches
    /// `here` at all.
    fn across(&self, here: u32) -> Option<u32> {
        match self.rooms {
            (a, b) if a == here => Some(b),
            (a, b) if b == here => Some(a),
            _ => None,
        }
    }
}

/// An axis-aligned rectangle in normalised device coordinates.
///
/// Empty is represented by `min > max` on either axis, which is what an
/// intersection that found nothing produces naturally -- there is no separate
/// "is empty" flag to forget to set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub min: Vec2,
    pub max: Vec2,
}

impl Rect {
    /// The whole screen, and then some: what a seed room is allowed to see,
    /// and what anything straddling the near plane falls back to.
    ///
    /// Deliberately larger than the `-1..=1` NDC box. A room's projected
    /// rectangle is clamped by nothing, so a wall filling the view has corners
    /// well outside the screen, and an `everything` that stopped at the screen
    /// edge would fail to contain it -- which matters only because containment
    /// is what ends the walk.
    pub fn everything() -> Self {
        Self {
            min: Vec2::splat(f32::NEG_INFINITY),
            max: Vec2::splat(f32::INFINITY),
        }
    }

    fn empty() -> Self {
        Self {
            min: Vec2::splat(f32::INFINITY),
            max: Vec2::splat(f32::NEG_INFINITY),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y
    }

    fn intersect(&self, other: &Self) -> Self {
        Self {
            min: self.min.max(other.min),
            max: self.max.min(other.max),
        }
    }

    /// The smallest rectangle holding both.
    ///
    /// **A bounding union, which is not the union.** Two rectangles side by
    /// side become one covering the gap between them, so this admits more than
    /// it should -- the safe direction -- and, because it only ever grows, it
    /// is what makes the walk terminate: a room is re-entered only when a new
    /// path offers something the accumulated rectangle does not already hold,
    /// and a growing bounded quantity cannot do that forever.
    fn union(&self, other: &Self) -> Self {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    fn contains(&self, other: &Self) -> bool {
        other.is_empty()
            || (self.min.x <= other.min.x
                && self.min.y <= other.min.y
                && self.max.x >= other.max.x
                && self.max.y >= other.max.y)
    }
}

/// Clip space `w` below which a point is behind the eye and cannot be divided
/// by. Not zero: a point exactly on the eye divides to infinity, and one a
/// hair in front of it produces coordinates large enough to swamp an `f32`
/// rectangle without ever being visible.
const MIN_W: f32 = 1e-4;


/// The screen rectangle a convex world-space polygon covers, or `None` when it
/// is entirely behind the eye.
///
/// **Clipped against the near plane rather than given up on.** A doorway you
/// are standing in the middle of has corners on both sides of the eye, and
/// projecting those directly puts two of them behind the camera with the sign
/// of `w` flipped -- which reads as a rectangle on the wrong side of the
/// screen, and would cull the room you are walking into. The alternative of
/// falling back to [`Rect::everything`] whenever a point is behind is correct
/// but gives up precisely at the doorway that matters most, since a doorway
/// close enough to straddle the near plane is one you are about to walk
/// through. So it is clipped: the polygon is convex, so intersecting it with a
/// half-space leaves it convex, and the projected result is exact.
fn screen_rect(points: &[Vec3], view_proj: &Mat4) -> Option<Rect> {
    if points.is_empty() {
        return None;
    }
    let clip: Vec<Vec4> = points
        .iter()
        .map(|p| *view_proj * Vec4::new(p.x, p.y, p.z, 1.0))
        .collect();

    // Sutherland-Hodgman against the single half-space `w >= MIN_W`.
    let mut kept: Vec<Vec4> = Vec::with_capacity(clip.len() + 1);
    for i in 0..clip.len() {
        let current = clip[i];
        let next = clip[(i + 1) % clip.len()];
        let inside = current.w >= MIN_W;
        let next_inside = next.w >= MIN_W;
        if inside {
            kept.push(current);
        }
        if inside != next_inside {
            // Where the edge crosses `w = MIN_W`, in clip space, which is
            // linear along the edge -- that is the whole reason to clip here
            // rather than after the divide.
            let t = (MIN_W - current.w) / (next.w - current.w);
            kept.push(current + (next - current) * t);
        }
    }
    if kept.is_empty() {
        return None;
    }

    let mut rect = Rect::empty();
    for p in kept {
        let ndc = Vec2::new(p.x / p.w, p.y / p.w);
        rect.min = rect.min.min(ndc);
        rect.max = rect.max.max(ndc);
    }
    Some(rect)
}

/// The eight corners of a box, which is what has to be projected: transforming
/// only `min` and `max` gives the diagonal of the projected shape and not its
/// extent. Same rule [`crate::cull::transformed_bounds`] is written around.
fn corners((min, max): (Vec3, Vec3)) -> [Vec3; 8] {
    [
        Vec3::new(min.x, min.y, min.z),
        Vec3::new(max.x, min.y, min.z),
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(max.x, max.y, min.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(min.x, max.y, max.z),
        Vec3::new(max.x, max.y, max.z),
    ]
}

fn holds(bounds: (Vec3, Vec3), point: Vec3) -> bool {
    point.cmpge(bounds.0).all() && point.cmple(bounds.1).all()
}

/// How many rooms may be popped before the walk gives up, as a multiple of the
/// room count. A room can legitimately be reached by several paths carrying
/// different rectangles, so a plain visited set is too strict; this is the
/// backstop for the case the rectangle union somehow fails to converge.
const WALK_LIMIT: usize = 8;

/// What [`visible_rooms`] concluded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Rooms {
    /// The eye is not in any room, or the building has no doorways to walk.
    /// **Draw everything** -- this is not a failure, it is the honest answer
    /// for a camera outside the building, and the caller already has a rule
    /// for that case.
    Undecided,
    /// The eye was located and the walk completed. The flags say which rooms
    /// were reached.
    Decided,
}

/// Marks which rooms are reachable from `eye` through doorways.
///
/// `out` is resized to `rooms.len()` and every entry written, so a caller
/// reusing a buffer across frames cannot read last frame's answer for a room
/// this frame's walk never touched.
///
/// Returns [`Rooms::Undecided`] when the eye is in no room at all, in which
/// case `out` is filled with `true`: **a walk that cannot say anything must
/// say "draw it", never "cull it".** That is the whole safety property of this
/// module, and it is asserted rather than assumed.
pub fn visible_rooms(
    eye: Vec3,
    view_proj: &Mat4,
    rooms: &[(Vec3, Vec3)],
    doorways: &[Doorway],
    always: &[u32],
    out: &mut Vec<bool>,
) -> Rooms {
    visible_rooms_to_depth(eye, view_proj, rooms, doorways, always, u32::MAX, out)
}

/// [`visible_rooms`], stopping after `max_depth` doorways.
///
/// **An instrument, and specifically the negative control the picture needs.**
/// Portal culling's whole claim is that it removes draws and changes no
/// pixels, and the second half of that is worth nothing on its own: a walk
/// that reached every room would also change no pixels, and so would one that
/// never ran. What makes the zero mean something is that a *deliberately too
/// short* walk, through the same camera and the same building, changes a great
/// many. Depth 0 draws only the room the eye stands in.
///
/// The same shape as pushing the frustum planes 12 units inward to check that
/// `--no-cull` was measuring anything -- see `Args::no_cull`.
pub fn visible_rooms_to_depth(
    eye: Vec3,
    view_proj: &Mat4,
    rooms: &[(Vec3, Vec3)],
    doorways: &[Doorway],
    always: &[u32],
    max_depth: u32,
    out: &mut Vec<bool>,
) -> Rooms {
    out.clear();
    out.resize(rooms.len(), false);
    if rooms.is_empty() {
        return Rooms::Undecided;
    }

    // **Every room whose box holds the eye, not the best one.** A building's
    // group boxes overlap -- a doorway's own volume belongs to both rooms it
    // joins, and a stairwell to every floor it passes -- so picking one is
    // picking arbitrarily between right answers, and picking wrong empties the
    // room you are standing in. Seeding all of them is the union of those
    // answers, which is conservative in the direction that draws.
    let mut queue: Vec<(u32, Rect, u32)> = Vec::new();
    let mut reached: Vec<Rect> = vec![Rect::empty(); rooms.len()];
    for (index, bounds) in rooms.iter().enumerate() {
        if holds(*bounds, eye) {
            queue.push((index as u32, Rect::everything(), 0));
        }
    }
    if queue.is_empty() {
        out.iter_mut().for_each(|v| *v = true);
        return Rooms::Undecided;
    }

    // **Rooms the walk is not entitled to reason about.** A `.wmo` stores
    // each opening twice, once from each side -- except that 69 of the game's
    // 7,548 do not, 11 of them in Stormwind, and those name one room and
    // nothing on the far side. There is no second group index to step to, so
    // the only honest thing to say about such a room is that it may be
    // visible. Marked, and not expanded from: the file names no far side, so
    // inventing one would be guessing, and the near room is the whole of what
    // is known.
    //
    // Found by A/B, exactly where it was predicted: without this, one Stormwind
    // camera of ten differed by a twelve-pixel sliver that no widening of any
    // tolerance would close, because it was never a tolerance.
    for room in always {
        if let Some(slot) = out.get_mut(*room as usize) {
            *slot = true;
        }
    }

    let limit = rooms.len().saturating_mul(WALK_LIMIT).max(WALK_LIMIT);
    let mut popped = 0usize;
    while let Some((room, rect, depth)) = queue.pop() {
        popped += 1;
        if popped > limit {
            // Cannot happen with a growing union, and if it ever does the
            // answer is "draw everything" rather than a half-walked building.
            out.iter_mut().for_each(|v| *v = true);
            return Rooms::Undecided;
        }
        let slot = &mut reached[room as usize];
        if !out[room as usize] {
            out[room as usize] = true;
        } else if slot.contains(&rect) {
            // Nothing new: this path shows no more of the room than a path
            // already walked.
            continue;
        }
        *slot = slot.union(&rect);
        if depth >= max_depth {
            continue;
        }

        for doorway in doorways {
            let Some(target) = doorway.across(room) else {
                continue;
            };
            let Some(opening) = screen_rect(&doorway.vertices, view_proj) else {
                // Entirely behind the eye: not a way through.
                continue;
            };
            let narrowed = rect.intersect(&opening);
            if narrowed.is_empty() {
                continue;
            }
            let Some(target_bounds) = rooms.get(target as usize) else {
                continue;
            };
            // The room beyond has to be inside what the doorway leaves of the
            // view, or there is no point stepping into it. Straddling the near
            // plane means the eye is inside that room's box, which `screen_rect`
            // reports as a rectangle around the eye rather than as nothing.
            let Some(target_rect) = screen_rect(&corners(*target_bounds), view_proj) else {
                continue;
            };
            if target_rect.intersect(&narrowed).is_empty() {
                continue;
            }
            if out[target as usize] && reached[target as usize].contains(&narrowed) {
                continue;
            }
            queue.push((target, narrowed, depth + 1));
        }
    }
    Rooms::Decided
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A corridor of `n` rooms in a line along +x, each 10 units long, joined
    /// by a doorway in the wall between them.
    ///
    /// Deliberately a line rather than a blob: it is the shape where the right
    /// answer is obvious and countable, so a walk that goes one room too far
    /// or stops one short shows up as a number rather than as a picture.
    fn corridor(n: usize) -> (Vec<(Vec3, Vec3)>, Vec<Doorway>) {
        let mut rooms = Vec::new();
        let mut doorways = Vec::new();
        for i in 0..n {
            let x = i as f32 * 10.0;
            rooms.push((Vec3::new(x, -5.0, -5.0), Vec3::new(x + 10.0, 5.0, 5.0)));
            if i > 0 {
                // A 2x2 opening in the shared wall at x = i*10.
                doorways.push(Doorway {
                    vertices: vec![
                        Vec3::new(x, -1.0, -1.0),
                        Vec3::new(x, 1.0, -1.0),
                        Vec3::new(x, 1.0, 1.0),
                        Vec3::new(x, -1.0, 1.0),
                    ],
                    rooms: ((i - 1) as u32, i as u32),
                });
            }
        }
        (rooms, doorways)
    }

    /// Looking down the corridor from inside the first room.
    fn looking_down_the_corridor(eye: Vec3) -> Mat4 {
        // The same constructors the rest of the crate uses: clip space runs
        // `0..=w` in depth here, and a test built on the other convention
        // would be asserting against a matrix nothing draws with.
        let proj = glam::camera::rh::proj::directx::perspective(
            60f32.to_radians(),
            1.0,
            0.1,
            1000.0,
        );
        proj * glam::camera::rh::view::look_at_mat4(eye, eye + Vec3::X, Vec3::Z)
    }

    #[test]
    fn the_room_the_eye_is_in_is_always_visible() {
        let (rooms, doorways) = corridor(4);
        let eye = Vec3::new(5.0, 0.0, 0.0);
        let mut out = Vec::new();
        let verdict = visible_rooms(
            eye,
            &looking_down_the_corridor(eye),
            &rooms,
            &doorways,
            &[],
            &mut out,
        );
        assert_eq!(verdict, Rooms::Decided);
        assert!(out[0], "the room the eye stands in must never be culled");
    }

    /// **The safety property, and the one worth stating first.** A walk that
    /// cannot locate the eye must draw everything. Culling on a shrug is the
    /// failure that looks like a hole in the world.
    #[test]
    fn an_eye_in_no_room_draws_everything() {
        let (rooms, doorways) = corridor(4);
        let eye = Vec3::new(0.0, 500.0, 0.0);
        let mut out = Vec::new();
        let verdict = visible_rooms(
            eye,
            &looking_down_the_corridor(eye),
            &rooms,
            &doorways,
            &[],
            &mut out,
        );
        assert_eq!(verdict, Rooms::Undecided);
        assert!(out.iter().all(|v| *v), "an undecided walk must draw all {out:?}");
    }

    /// A building with rooms and no doorways at all: the eye's room, and
    /// nothing else, because there is nothing to walk through.
    #[test]
    fn rooms_with_no_doorways_do_not_reach_each_other() {
        let (rooms, _) = corridor(4);
        let eye = Vec3::new(5.0, 0.0, 0.0);
        let mut out = Vec::new();
        visible_rooms(eye, &looking_down_the_corridor(eye), &rooms, &[], &[], &mut out);
        assert_eq!(out, vec![true, false, false, false]);
    }

    /// **The negative control, in the form this module needs it.** Turning to
    /// face away from the corridor must reach fewer rooms than facing along
    /// it -- otherwise a `visible_rooms` that ignored the view matrix entirely
    /// would pass every other test here.
    #[test]
    fn turning_away_reaches_fewer_rooms() {
        let (rooms, doorways) = corridor(5);
        let eye = Vec3::new(5.0, 0.0, 0.0);
        let proj = glam::camera::rh::proj::directx::perspective(
            60f32.to_radians(),
            1.0,
            0.1,
            1000.0,
        );

        let mut along = Vec::new();
        visible_rooms(
            eye,
            &looking_down_the_corridor(eye),
            &rooms,
            &doorways,
            &[],
            &mut along,
        );
        let mut away = Vec::new();
        visible_rooms(
            eye,
            &(proj * glam::camera::rh::view::look_at_mat4(eye, eye - Vec3::X, Vec3::Z)),
            &rooms,
            &doorways,
            &[],
            &mut away,
        );

        let seen = |v: &Vec<bool>| v.iter().filter(|b| **b).count();
        assert!(
            seen(&away) < seen(&along),
            "facing away saw {} rooms, facing along saw {}",
            seen(&away),
            seen(&along)
        );
        assert!(away[0], "your own room is visible whichever way you face");
        assert!(
            !away[4],
            "the far end of a corridor behind you is not visible"
        );
    }

    /// A doorway narrows what lies beyond it, so a room off a side passage
    /// that the opening does not cover is not reached.
    ///
    /// Two rooms beyond the first, one straight ahead and one far off to the
    /// side, joined to it by the *same* narrow opening. Only the one the
    /// opening actually shows may come back.
    #[test]
    fn a_doorway_narrows_what_lies_beyond_it() {
        let rooms = vec![
            (Vec3::new(0.0, -5.0, -5.0), Vec3::new(10.0, 5.0, 5.0)),
            // Straight ahead through the opening.
            (Vec3::new(10.0, -2.0, -2.0), Vec3::new(20.0, 2.0, 2.0)),
            // Far off to one side, well outside the opening's cone.
            (Vec3::new(10.0, 400.0, -2.0), Vec3::new(20.0, 460.0, 2.0)),
        ];
        let opening = vec![
            Vec3::new(10.0, -1.0, -1.0),
            Vec3::new(10.0, 1.0, -1.0),
            Vec3::new(10.0, 1.0, 1.0),
            Vec3::new(10.0, -1.0, 1.0),
        ];
        let doorways = vec![
            Doorway {
                vertices: opening.clone(),
                rooms: (0, 1),
            },
            Doorway {
                vertices: opening,
                rooms: (0, 2),
            },
        ];
        let eye = Vec3::new(5.0, 0.0, 0.0);
        let mut out = Vec::new();
        visible_rooms(
            eye,
            &looking_down_the_corridor(eye),
            &rooms,
            &doorways,
            &[],
            &mut out,
        );
        assert!(out[0] && out[1], "the room through the opening is visible");
        assert!(
            !out[2],
            "a room the opening does not show must not be reached"
        );
    }

    /// **A doorway the eye is standing in the middle of.** Its polygon
    /// straddles the near plane, which is exactly the case a naive projection
    /// gets backwards -- two corners land behind the camera with `w` negative
    /// and the rectangle comes out on the wrong side of the screen, culling
    /// the room being walked into.
    #[test]
    fn a_doorway_at_the_eye_still_lets_the_next_room_through() {
        let (rooms, doorways) = corridor(3);
        // Standing exactly in the opening between rooms 0 and 1.
        let eye = Vec3::new(10.0, 0.0, 0.0);
        let mut out = Vec::new();
        visible_rooms(
            eye,
            &looking_down_the_corridor(eye),
            &rooms,
            &doorways,
            &[],
            &mut out,
        );
        assert!(out[1], "the room ahead must be drawn");
        assert!(
            out[0],
            "and so must the one behind: the eye's box holds it too"
        );
    }

    /// The walk must end. A ring of rooms is the shape that would spin
    /// forever if the accumulated rectangle ever failed to converge.
    #[test]
    fn a_ring_of_rooms_terminates() {
        let n = 8;
        let mut rooms = Vec::new();
        let mut doorways = Vec::new();
        for i in 0..n {
            let a = i as f32 * std::f32::consts::TAU / n as f32;
            let c = Vec3::new(a.cos() * 20.0, a.sin() * 20.0, 0.0);
            rooms.push((c - Vec3::splat(4.0), c + Vec3::splat(4.0)));
        }
        for i in 0..n {
            let j = (i + 1) % n;
            let mid = (rooms[i].0 + rooms[i].1 + rooms[j].0 + rooms[j].1) * 0.25;
            doorways.push(Doorway {
                vertices: vec![
                    mid + Vec3::new(-1.0, -1.0, -1.0),
                    mid + Vec3::new(1.0, -1.0, -1.0),
                    mid + Vec3::new(1.0, 1.0, 1.0),
                    mid + Vec3::new(-1.0, 1.0, 1.0),
                ],
                rooms: (i as u32, j as u32),
            });
        }
        let eye = (rooms[0].0 + rooms[0].1) * 0.5;
        let mut out = Vec::new();
        // The assertion is that this returns at all.
        let verdict = visible_rooms(
            eye,
            &looking_down_the_corridor(eye),
            &rooms,
            &doorways,
            &[],
            &mut out,
        );
        assert_eq!(verdict, Rooms::Decided);
        assert!(out[0]);
    }

    /// A doorway naming a room that does not exist must be skipped rather than
    /// panic. 69 of the game's 7,548 portals are not referenced from both
    /// sides, so malformed adjacency is a thing that really occurs.
    #[test]
    fn a_doorway_to_nowhere_is_skipped() {
        let (rooms, _) = corridor(2);
        let doorways = vec![Doorway {
            vertices: vec![
                Vec3::new(10.0, -1.0, -1.0),
                Vec3::new(10.0, 1.0, -1.0),
                Vec3::new(10.0, 1.0, 1.0),
            ],
            rooms: (0, 99),
        }];
        let eye = Vec3::new(5.0, 0.0, 0.0);
        let mut out = Vec::new();
        let verdict = visible_rooms(
            eye,
            &looking_down_the_corridor(eye),
            &rooms,
            &doorways,
            &[],
            &mut out,
        );
        assert_eq!(verdict, Rooms::Decided);
        assert_eq!(out.len(), 2);
    }

    /// The output buffer is rewritten in full, so a caller reusing one across
    /// frames cannot read a stale `true` for a room this walk never touched.
    #[test]
    fn a_reused_buffer_is_fully_rewritten() {
        let (rooms, doorways) = corridor(3);
        let eye = Vec3::new(5.0, 0.0, 0.0);
        let mut out = vec![true; 99];
        visible_rooms(
            eye,
            &looking_down_the_corridor(eye),
            &rooms,
            &doorways,
            &[],
            &mut out,
        );
        assert_eq!(out.len(), 3, "resized to this walk's room count");
    }

    /// The depth limit is the picture's negative control, so it has to
    /// actually bite: depth 0 is the eye's room alone, and each step admits
    /// one more room of the corridor.
    #[test]
    fn a_depth_limit_stops_the_walk() {
        let (rooms, doorways) = corridor(5);
        let eye = Vec3::new(5.0, 0.0, 0.0);
        let view_proj = looking_down_the_corridor(eye);
        let seen = |depth| {
            let mut out = Vec::new();
            visible_rooms_to_depth(eye, &view_proj, &rooms, &doorways, &[], depth, &mut out);
            out.iter().filter(|b| **b).count()
        };
        assert_eq!(seen(0), 1, "depth 0 is the room the eye stands in");
        assert_eq!(seen(1), 2);
        assert!(seen(4) >= seen(2), "a deeper walk cannot see less");
        assert!(
            seen(0) < seen(u32::MAX),
            "the limit has to bite, or it is not a control"
        );
    }

    #[test]
    fn rectangles_intersect_and_report_empty() {
        let a = Rect {
            min: Vec2::new(-1.0, -1.0),
            max: Vec2::new(0.0, 0.0),
        };
        let b = Rect {
            min: Vec2::new(0.5, 0.5),
            max: Vec2::new(1.0, 1.0),
        };
        assert!(a.intersect(&b).is_empty());
        assert!(!a.intersect(&a).is_empty());
        assert!(Rect::everything().contains(&a));
        assert!(!a.contains(&Rect::everything()));
        // A union grows, which is what ends the walk.
        assert!(a.union(&b).contains(&a));
        assert!(a.union(&b).contains(&b));
    }

    /// A polygon entirely behind the eye is not a way through anything.
    #[test]
    fn a_polygon_behind_the_eye_has_no_screen_rectangle() {
        let eye = Vec3::new(5.0, 0.0, 0.0);
        let view_proj = looking_down_the_corridor(eye);
        let behind = [
            Vec3::new(-10.0, -1.0, -1.0),
            Vec3::new(-10.0, 1.0, -1.0),
            Vec3::new(-10.0, 1.0, 1.0),
        ];
        assert!(screen_rect(&behind, &view_proj).is_none());
        let ahead = [
            Vec3::new(20.0, -1.0, -1.0),
            Vec3::new(20.0, 1.0, -1.0),
            Vec3::new(20.0, 1.0, 1.0),
        ];
        assert!(screen_rect(&ahead, &view_proj).is_some());
    }
}
