//! Solid-world queries: what a character can walk through, and what it stands
//! on.
//!
//! **Collision is entirely the client's job here, and that is a measurement
//! rather than an assumption.** A character driven by this client walked
//! through the wall of Northshire Abbey and a *second* client, watching, drew
//! it happening -- so nothing on the server corrected the position, and nothing
//! will. The two-client rig said so; see `docs/ROADMAP.md`.
//!
//! Deliberately knows nothing about MPQ, DBC, the protocol or the GPU. It takes
//! triangles in world space and answers two questions:
//!
//! - [`World::slide`], for moving horizontally without passing through a wall;
//! - [`World::floor_under`], for what height to stand at.
//!
//! That makes the whole of it testable from a unit test with a hand-built box,
//! which is the same reason the `ui` crate depends on neither `world` nor
//! `render`.

use glam::{Vec2, Vec3};

/// How far a surface can lean before it stops being a floor and becomes a wall.
///
/// **Chosen, and it is the one number here a player would feel.** Nothing in
/// the game's data states a slope limit. At 0.5 -- sixty degrees from vertical
/// -- the abbey's steps and the hills around it are walkable while its walls
/// are not, which is what the constant is for. A surface whose normal has less
/// vertical component than this is climbed *around* rather than up.
///
/// **Public so the camera can ask the identical question.** It tells a floor
/// from a wall for a walking body; the camera needs the same distinction for
/// a different reason -- a near-horizontal hit (floor *or* ceiling, by the
/// same `abs(normal.z)` test) wants ducking under or over, where a wall wants
/// pulling in front of. Using a second, locally-chosen threshold there would
/// let the two disagree about which is which for the exact triangles this
/// number already answers for.
pub const FLOOR_NORMAL_Z: f32 = 0.5;

/// The tag of a triangle nobody labelled.
///
/// See [`World::add_tagged`]. `add` uses it, so a caller that has no opinion
/// about surfaces does not have to have one.
pub const UNTAGGED: u8 = u8::MAX;

/// A triangle in world space.
///
/// Stored as three points rather than a point and two edges because the
/// queries want the points, and deriving edges is a subtraction where
/// reconstructing points is not.
#[derive(Clone, Copy, Debug)]
pub struct Triangle {
    pub a: Vec3,
    pub b: Vec3,
    pub c: Vec3,
}

impl Triangle {
    pub fn new(a: Vec3, b: Vec3, c: Vec3) -> Self {
        Self { a, b, c }
    }

    /// The unit normal, or `None` for a degenerate triangle.
    ///
    /// Degenerate triangles are real: a merged WMO carries some, and a zero
    /// normal normalises to NaN, which then poisons every comparison it
    /// reaches instead of failing.
    pub fn normal(&self) -> Option<Vec3> {
        let n = (self.b - self.a).cross(self.c - self.a);
        (n.length_squared() > 1e-12).then(|| n.normalize())
    }

    /// Whether this surface is flat enough to stand on.
    pub fn is_floor(&self) -> bool {
        self.normal().is_some_and(|n| n.z.abs() >= FLOOR_NORMAL_Z)
    }

    fn min(&self) -> Vec3 {
        self.a.min(self.b).min(self.c)
    }

    /// A point just off this triangle's face, on the near side of whoever is
    /// asking, for asking "is the floor this wall stands on itself
    /// reachable".
    ///
    /// **The point on the triangle closest to `approaching_from`, not the
    /// triangle's own lowest edge.** A real WMO riser is rarely the small,
    /// tread-sized quad a synthetic reproduction builds -- one candidate
    /// measured on `NSabbey.wmo`'s `Stairs1` had its lowest edge more than a
    /// full body radius away from the body actually pressing against it, on
    /// a different part of the same long triangle. Querying there asks about
    /// a tread the body has nothing to do with; querying at the point
    /// [`push_out`] itself measures distance from asks about the part of the
    /// wall actually in contact.
    ///
    /// **Nudged along the wall's own normal, not straight at
    /// `approaching_from`.** A first attempt nudged directly towards it and
    /// broke on a wide riser: the vector towards a body standing squarely in
    /// front is dominated by *how wide* the wall is, not by which side of it
    /// anyone is on, and barely moves across the seam at all. The normal has
    /// no such component by construction -- it is perpendicular to the wall,
    /// full stop -- so only its *sign* is decided by which side
    /// `approaching_from` is on, and the step itself is a fixed, small
    /// distance across the seam rather than however far away the body
    /// happens to be standing.
    fn foot_towards(&self, approaching_from: Vec2) -> Vec2 {
        const NUDGE: f32 = 0.02;
        let (a, b, c) = (self.a.truncate(), self.b.truncate(), self.c.truncate());
        let closest = closest_point_on_triangle_2d(approaching_from, a, b, c);
        let Some(normal) = self.normal() else {
            return closest;
        };
        let flat = Vec2::new(normal.x, normal.y);
        if flat.length_squared() < 1e-9 {
            return closest;
        }
        let flat = flat.normalize();
        let sign = (approaching_from - closest).dot(flat).signum();
        closest + flat * (NUDGE * sign)
    }

    fn max(&self) -> Vec3 {
        self.a.max(self.b).max(self.c)
    }

    /// Where a downward ray from `above` crosses this triangle, if it does.
    ///
    /// Returns the height, not a distance, because every caller wants "what Z
    /// do I stand at" and converting back is an opportunity to get a sign
    /// wrong.
    pub fn floor_hit(&self, above: Vec2) -> Option<f32> {
        let n = self.normal()?;
        // A wall has no "height at this point" worth reporting: a vertical
        // triangle is crossed by a downward ray at a grazing angle, and the
        // answer is unstable and useless.
        if n.z.abs() < FLOOR_NORMAL_Z {
            return None;
        }
        // Barycentric containment in the XY projection. The triangle is not
        // vertical, so the projection is non-degenerate.
        let (a, b, c) = (self.a.truncate(), self.b.truncate(), self.c.truncate());
        let v0 = b - a;
        let v1 = c - a;
        let v2 = above - a;
        let denominator = v0.x * v1.y - v1.x * v0.y;
        if denominator.abs() < 1e-9 {
            return None;
        }
        let u = (v2.x * v1.y - v1.x * v2.y) / denominator;
        let v = (v0.x * v2.y - v2.x * v0.y) / denominator;
        // A small tolerance, so a point exactly on the seam between two
        // triangles lands on one of them rather than falling between both.
        const EDGE: f32 = -1e-4;
        if u < EDGE || v < EDGE || u + v > 1.0 - EDGE {
            return None;
        }
        Some(self.a.z + u * (self.b.z - self.a.z) + v * (self.c.z - self.a.z))
    }

    /// Whether the segment `from` -> `to` passes through this triangle.
    ///
    /// Moller-Trumbore, and it exists for one job: catching a step large
    /// enough to end up on the far side of a wall without ever overlapping it.
    /// The push-out resolver cannot see that case by construction, because it
    /// only ever looks at where the move *ended*.
    fn crossed_by(&self, from: Vec3, to: Vec3) -> bool {
        self.hit_at(from, to).is_some()
    }

    /// How far along the segment `from` -> `to` it meets this triangle, as a
    /// fraction, or `None` if it misses.
    ///
    /// The same intersection [`Triangle::crossed_by`] does -- it discarded the
    /// distance and answered a yes/no, which is all a movement check needs.
    /// A camera needs the number: "something is in the way" tells you nothing
    /// about where to stop.
    fn hit_at(&self, from: Vec3, to: Vec3) -> Option<f32> {
        let direction = to - from;
        let edge1 = self.b - self.a;
        let edge2 = self.c - self.a;
        let h = direction.cross(edge2);
        let determinant = edge1.dot(h);
        // Parallel to the triangle: a grazing path along a wall, which is what
        // sliding produces and must not be treated as a crossing.
        if determinant.abs() < 1e-9 {
            return None;
        }
        let inverse = 1.0 / determinant;
        let s = from - self.a;
        let u = inverse * s.dot(h);
        if !(-1e-5..=1.0 + 1e-5).contains(&u) {
            return None;
        }
        let q = s.cross(edge1);
        let v = inverse * direction.dot(q);
        if v < -1e-5 || u + v > 1.0 + 1e-5 {
            return None;
        }
        let t = inverse * edge2.dot(q);
        (0.0..=1.0).contains(&t).then_some(t)
    }

    /// The shortest distance from a vertical segment to this triangle, in the
    /// horizontal plane only, and the direction to push away along.
    ///
    /// Horizontal because a character is a cylinder being pushed out of a
    /// wall, and the push has to leave its height alone -- resolving in three
    /// dimensions against a sloped wall would lift the character up it.
    ///
    /// **Says nothing about heights, on purpose, and that is a performance
    /// fact rather than a tidiness one.** The band this is tested against
    /// comes from [`World::wall_exemption`], which costs a *grid lookup* --
    /// so a caller that computes the band before knowing whether the triangle
    /// is even within arm's reach pays that lookup for every candidate the
    /// grid handed back. Measured live in Northshire abbey: 4,193 lookups and
    /// **3.1 million candidate triangles in a single frame**, against 70 and
    /// 9,274 standing outside, and 10-22ms of a 20-38ms frame. Split so the
    /// two rejections that need no lookup -- a floor, and anything further
    /// than `radius` away -- come first. See [`World::slide`].
    fn push_out_horizontally(&self, at: Vec2, radius: f32) -> Option<Vec2> {
        // Floors do not block horizontal movement, whatever their extent.
        if self.is_floor() {
            return None;
        }
        let (a, b, c) = (self.a.truncate(), self.b.truncate(), self.c.truncate());
        let closest = closest_point_on_triangle_2d(at, a, b, c);
        let away = at - closest;
        let distance = away.length();
        if distance >= radius {
            return None;
        }
        // Dead centre of a wall: push along the wall's own normal rather than
        // a zero vector, which would normalise to NaN.
        if distance < 1e-5 {
            let n = self.normal()?;
            let flat = Vec2::new(n.x, n.y);
            let fallback = if flat.length_squared() > 1e-9 {
                flat.normalize()
            } else {
                Vec2::X
            };
            return Some(fallback * radius);
        }
        Some(away / distance * (radius - distance))
    }

    /// Whether the body standing between `low` and `high` overlaps this
    /// triangle vertically at all.
    ///
    /// Without this a character standing on a floor is permanently being
    /// pushed sideways by the floor it is standing on.
    fn overlaps_band(&self, low: f32, high: f32) -> bool {
        self.min().z <= high && self.max().z >= low
    }

    /// The cheapest possible statement about the band, made before the band
    /// is known.
    ///
    /// [`World::wall_exemption`] only ever *raises* the bottom of the band --
    /// it is documented as never removing an exemption, and returns exactly
    /// `from_z + step` when it finds nothing. So a triangle whose top is
    /// already under `from_z + step` is excluded whatever the exemption turns
    /// out to be, and the lookup that would have computed it can be skipped
    /// outright. This is the test that lets the floors and treads a body is
    /// standing among cost nothing.
    fn under_any_band(&self, from_z: f32, step: f32) -> bool {
        self.max().z < from_z + step
    }
}

/// Closest point to `p` on triangle `abc`, in two dimensions.
fn closest_point_on_triangle_2d(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> Vec2 {
    if point_in_triangle_2d(p, a, b, c) {
        return p;
    }
    let mut best = closest_point_on_segment(p, a, b);
    let mut best_d = p.distance_squared(best);
    for (s, e) in [(b, c), (c, a)] {
        let q = closest_point_on_segment(p, s, e);
        let d = p.distance_squared(q);
        if d < best_d {
            best = q;
            best_d = d;
        }
    }
    best
}

fn closest_point_on_segment(p: Vec2, a: Vec2, b: Vec2) -> Vec2 {
    let ab = b - a;
    let length_squared = ab.length_squared();
    if length_squared < 1e-12 {
        return a;
    }
    a + ab * ((p - a).dot(ab) / length_squared).clamp(0.0, 1.0)
}

fn point_in_triangle_2d(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
    let sign = |p: Vec2, q: Vec2, r: Vec2| (p.x - r.x) * (q.y - r.y) - (q.x - r.x) * (p.y - r.y);
    let d1 = sign(p, a, b);
    let d2 = sign(p, b, c);
    let d3 = sign(p, c, a);
    let negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(negative && positive)
}

/// Edge of one cell of the lookup grid, in world units.
///
/// A terrain tile is 533 units and a building is tens, so this is sized to a
/// building rather than to a tile: big enough that the abbey spans only a
/// handful of cells, small enough that a query touches a few dozen triangles
/// rather than a few thousand. Not tuned against a profile -- there has not
/// been one -- so it is stated as the guess it is.
const CELL: f32 = 8.0;

/// Everything solid, indexed so a query looks at what is nearby.
///
/// Built once per streamed tile and thrown away with it: rebuilding is cheap
/// beside reading the tile off disk, and a grid that outlived its tile would be
/// a set of invisible walls where a building used to be.
/// How much work the grid did, since it was last asked.
///
/// **Counted at [`World::near`], which is the one place every query narrows
/// through.** A collision cost has two completely different shapes and one
/// symptom: *many* cheap queries (a camera sampling four orbit rays at
/// eighteen heights each, across nine tiles) and *expensive* ones (a cell
/// holding half a city's triangles because one placement spans nine tiles).
/// The first wants fewer callers, the second wants a better index, and a
/// millisecond total cannot tell them apart. Both numbers, always -- the same
/// reason the placeholder-texture counter prints its zero.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Probe {
    /// Narrowing lookups made.
    pub queries: u64,
    /// Triangles those lookups handed back to be tested one at a time. This
    /// is the number a better index would move.
    pub candidates: u64,
}

#[derive(Default)]
pub struct World {
    /// Interior mutability because every query here takes `&self` and must
    /// keep doing so -- the alternative is a `&mut` borrow on the collision
    /// world threaded through the camera, the movement code and the footstep
    /// lookup, which would be a real change to the design in order to count
    /// something.
    probe: std::cell::Cell<Probe>,
    triangles: Vec<Triangle>,
    /// One opaque label per triangle, parallel to [`Self::triangles`].
    ///
    /// **This crate does not know what a tag means**, deliberately: it is pure
    /// geometry, and the meaning is the caller's. The viewer puts a WMO
    /// material's terrain row here so a footstep on a wooden floor can sound
    /// like wood, and puts nothing at all on an M2's collision mesh, which
    /// carries no such field.
    ///
    /// Parallel rather than a field on [`Triangle`] because `Triangle` is the
    /// type every query takes and returns by value, and widening it would put
    /// a byte nobody reads into every intersection test.
    tags: Vec<u8>,
    surface_ids: Vec<u32>,
    /// Cell coordinate to the triangles overlapping it. A triangle spanning
    /// several cells appears in each, which is what makes a lookup a lookup
    /// rather than a search.
    cells: std::collections::HashMap<(i32, i32), Vec<u32>>,
}

impl World {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    pub fn triangle_count(&self) -> usize {
        self.triangles.len()
    }

    /// Adds a triangle, skipping degenerate ones.
    pub fn add(&mut self, triangle: Triangle) {
        self.add_tagged(triangle, UNTAGGED);
    }

    /// Adds a triangle carrying an opaque per-surface label.
    ///
    /// The label comes back from [`Self::floor_under_tagged`], and means
    /// whatever the caller decided it means -- see [`Self::tags`].
    pub fn add_tagged(&mut self, triangle: Triangle, tag: u8) {
        self.add_tagged_with_id(triangle, tag, 0);
    }

    pub fn add_tagged_with_id(&mut self, triangle: Triangle, tag: u8, surface_id: u32) {
        if triangle.normal().is_none() {
            return;
        }
        let index = self.triangles.len() as u32;
        let (min, max) = (triangle.min(), triangle.max());
        for x in cell_of(min.x)..=cell_of(max.x) {
            for y in cell_of(min.y)..=cell_of(max.y) {
                self.cells.entry((x, y)).or_default().push(index);
            }
        }
        self.triangles.push(triangle);
        self.tags.push(tag);
        self.surface_ids.push(surface_id);
    }

    /// The one cell a point falls in, borrowed rather than collected.
    ///
    /// **A point query touches exactly one cell**, so there is nothing to
    /// merge and nothing to deduplicate -- and `near` would nevertheless
    /// allocate a `Vec`, copy the cell into it, sort it and dedup it. That is
    /// the whole of `floor_under_tagged_with_id`, which is the single most
    /// called function in the frame: the follow camera marches the ground in
    /// up to eighteen steps per sampled yaw, three yaws per frame, each fanned
    /// across every resident tile, and the character's own footing and the
    /// footstep lookup ask as well. Measured at roughly seven hundred calls a
    /// frame, all of them allocating.
    ///
    /// Returning a borrow rather than filling a scratch buffer is deliberate:
    /// `slide` iterates a candidate list and calls `wall_exemption` -- and so
    /// this -- from inside that loop, so a single shared scratch would be
    /// corrupted by its own re-entry. An immutable borrow cannot be.
    fn cell_at(&self, centre: Vec2) -> &[u32] {
        self.cells
            .get(&(cell_of(centre.x), cell_of(centre.y)))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Every triangle whose cell touches the given square, without repeats.
    fn near(&self, centre: Vec2, radius: f32) -> Vec<u32> {
        let mut found: Vec<u32> = Vec::new();
        for x in cell_of(centre.x - radius)..=cell_of(centre.x + radius) {
            for y in cell_of(centre.y - radius)..=cell_of(centre.y + radius) {
                if let Some(cell) = self.cells.get(&(x, y)) {
                    found.extend_from_slice(cell);
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        let mut probe = self.probe.get();
        probe.queries += 1;
        probe.candidates += found.len() as u64;
        self.probe.set(probe);
        found
    }

    /// Reads the work counters and zeroes them, so a caller reading once a
    /// frame gets that frame's figure rather than the session's.
    pub fn take_probe(&self) -> Probe {
        self.probe.replace(Probe::default())
    }

    /// What height to stand at under `at`, searching downward from `from_z`.
    ///
    /// **Downward from a little above the character, not from the sky.** A
    /// query that took the highest surface anywhere above the ground would
    /// stand a character on the abbey's roof the moment they walked under it.
    /// Starting from where they already are, plus a step, is what makes a
    /// staircase climbable and a roof irrelevant.
    ///
    /// `None` when nothing solid is under the point at all, which is the
    /// common case outdoors -- the caller falls back to the terrain height
    /// field, which is a different and much cheaper structure.
    pub fn floor_under(&self, at: Vec2, from_z: f32, step: f32) -> Option<f32> {
        self.floor_under_tagged(at, from_z, step).map(|(z, _)| z)
    }

    /// [`Self::floor_under`], and which surface it landed on.
    ///
    /// **One search, two answers, deliberately.** Asking for the height and
    /// then asking separately what is under the same point is two derivations
    /// of the same fact, and they agree only until a tie breaks differently --
    /// at which point a character stands on the floorboards and hears the
    /// flagstone beside them. Same reasoning as unprojecting the picking ray
    /// from the matrix the scene is drawn with.
    ///
    /// The tag is `None` where the winning triangle was added untagged, which
    /// is every M2 collision mesh and the whole world before something chose
    /// to label a surface.
    pub fn floor_under_tagged(&self, at: Vec2, from_z: f32, step: f32) -> Option<(f32, Option<u8>)> {
        self.floor_under_tagged_with_id(at, from_z, step)
            .map(|(z, tag, _)| (z, tag))
    }

    pub fn floor_under_tagged_with_id(
        &self,
        at: Vec2,
        from_z: f32,
        step: f32,
    ) -> Option<(f32, Option<u8>, Option<u32>)> {
        let ceiling = from_z + step;
        let mut best: Option<(f32, Option<u8>, Option<u32>)> = None;
        // The counters still see this: one narrowing, whatever it hands back.
        let candidates = self.cell_at(at);
        let mut probe = self.probe.get();
        probe.queries += 1;
        probe.candidates += candidates.len() as u64;
        self.probe.set(probe);
        for &index in candidates {
            let triangle = self.triangles[index as usize];
            let Some(z) = triangle.floor_hit(at) else {
                continue;
            };
            if z > ceiling {
                continue;
            }
            if best.is_none_or(|(b, _, _)| z > b) {
                let tag = self
                    .tags
                    .get(index as usize)
                    .copied()
                    .filter(|t| *t != UNTAGGED);
                let surface_id = self
                    .surface_ids
                    .get(index as usize)
                    .copied()
                    .filter(|id| *id != 0);
                best = Some((z, tag, surface_id));
            }
        }
        best
    }

    /// How high a wall may be treated as climbable rather than an obstacle,
    /// given where its own foot sits.
    ///
    /// **A staircase whose treads are shallower than the body's own radius
    /// defeats the plain single-riser allowance.** `push_out`/`blocked`
    /// ignore anything below `from_z + step` on the theory that it is the
    /// one riser being climbed right now -- true as long as the *next* riser
    /// is farther away than `radius`. On a tread shallower than that
    /// (measured: 0.33 units of run against a 0.55 body radius, on
    /// `NSabbey.wmo`'s `Stairs1`), the second riser sits well within
    /// `radius` while the body is still standing at the foot of the first,
    /// so it is tested as an ordinary wall and the whole flight refuses
    /// every step -- reported as "can't walk up them without jumping",
    /// reproduced offline as `slide` returning `achieved: 0.000` on every
    /// call against the real geometry.
    ///
    /// **The fix is not a chain -- a first attempt at one does not work,
    /// and the reason is worth keeping.** Searching upward for a reachable
    /// floor at one fixed `(x, y)` finds every riser stacked directly above
    /// that point, which is the right shape for a ladder and the wrong one
    /// for a staircase: real treads step *forward* as they rise, so the
    /// second riser's own tread is never at the same `(x, y)` as the first
    /// riser's. The chain found nothing past the very first hop on both the
    /// real geometry and a synthetic reproduction of it, because there was
    /// never anything to chain *to* from a single point.
    ///
    /// What actually works needs only one lookup: **if the tread this wall
    /// itself rises from is within `step` of `from_z`, the wall is exactly
    /// as climbable as the riser sitting on that tread would be, whether or
    /// not the body has walked there yet** -- so it is exempted up to its
    /// own top, `wall_top`, rather than only up to `from_z + step`.
    /// `wall_foot` has to be asked for at the wall's own position for this
    /// to mean anything -- see [`Triangle::foot_towards`].
    ///
    /// **Only ever adds an exemption, never removes one.** `wall_top` is
    /// still floored at `from_z + step`: a wall shorter than a single step
    /// was already fully exempt before this existed, and nothing here may
    /// shrink that. Where no floor is found under `wall_foot` at all --
    /// open ground, a standalone fence, a wall whose foot is not standing on
    /// a climbable tread -- this returns exactly `from_z + step`, identical
    /// to every case before this existed.
    fn wall_exemption(&self, wall_foot: Vec2, wall_top: f32, from_z: f32, step: f32) -> f32 {
        // A hair above the wall's own top, not exactly at it: `push_out`'s
        // own exclusion test is `max().z < low`, and a wall exempted up to
        // precisely its own top would tie that comparison and still be
        // tested, rather than skipped.
        const CLEARANCE: f32 = 0.05;
        if self.floor_under(wall_foot, from_z, step).is_some() {
            (wall_top + CLEARANCE).max(from_z + step)
        } else {
            from_z + step
        }
    }

    /// Moves a character from `from` towards `to`, sliding along anything
    /// solid instead of passing through it.
    ///
    /// The character is a vertical cylinder: `radius` across, from its feet up
    /// to `height`. Both are the caller's, because this crate has no opinion
    /// about how big a night elf is.
    ///
    /// **Resolved by pushing out of overlaps rather than by sweeping.** A swept
    /// test finds the first surface a path crosses and stops there, which is
    /// exact and, at a wall met at a shallow angle, stops dead. Pushing the
    /// destination back out of everything it ends up inside gives sliding for
    /// free: the component of the move along the wall survives, and only the
    /// component into it is removed. The cost is that a fast enough step could
    /// pass clean through a thin wall in one frame, which is why the caller is
    /// expected to keep steps small -- at the run speed and sixty frames a
    /// second a step is a fifth of a unit against walls a unit thick.
    ///
    /// **`step` lifts the bottom of the cylinder, and that is what makes a
    /// staircase climbable.** A stair riser is a vertical face, so it is a
    /// wall by every test here and pushes the character back off it -- which
    /// is exactly what was reported: the abbey's steps and every small bump
    /// stopped anybody dead, while `floor_under` sat there ready to stand them
    /// on the tread they could not reach. Ignoring anything whose top is below
    /// the character's feet plus `step` lets them pass horizontally, and the
    /// caller's ground query then lifts them onto it.
    ///
    /// So `step` is precisely "how tall a thing may be and still be walked
    /// over". Too small and stairs block; too large and a fence is a kerb.
    pub fn slide(&self, from: Vec3, to: Vec3, radius: f32, height: f32, step: f32) -> Vec3 {
        if self.triangles.is_empty() {
            return to;
        }
        let mut at = to;
        // Two passes: the first push can leave the character inside a second
        // wall, which is exactly what an inside corner is. More than two buys
        // very little and costs a lookup each.
        for _ in 0..2 {
            let mut correction = Vec2::ZERO;
            for index in self.near(at.truncate(), radius) {
                let triangle = self.triangles[index as usize];
                // **The two rejections that cost nothing, first.** Everything
                // below `wall_exemption` is a grid lookup per candidate, and
                // the grid hands back every triangle sharing a cell -- a
                // thousand of them in a building. A floor, and anything
                // further away than the body can reach, is refused here for
                // the price of a distance: see `push_out_horizontally`.
                let Some(push) = triangle.push_out_horizontally(at.truncate(), radius) else {
                    continue;
                };
                if triangle.under_any_band(from.z, step) {
                    continue;
                }
                // Per triangle, not once for the whole pass: see
                // `wall_exemption`. A riser one or more steps ahead of where
                // the body actually is can still be exempt, if the tread it
                // rises from is itself reachable from `from.z` -- ordinarily
                // just `from.z + step`, unchanged from before this existed.
                let foot = triangle.foot_towards(at.truncate());
                let ceiling = self.wall_exemption(foot, triangle.max().z, from.z, step);
                if triangle.overlaps_band(ceiling, ceiling + height) {
                    // Largest push wins per pass rather than summing: two faces
                    // of one wall both push the same way, and adding them
                    // ejects the character twice as far as either asked for.
                    if push.length_squared() > correction.length_squared() {
                        correction = push;
                    }
                }
            }
            if correction == Vec2::ZERO {
                break;
            }
            at.x += correction.x;
            at.y += correction.y;
        }

        // **A last-resort refusal, and it is what makes the guarantee real.**
        // Push-out only ever looks at where the move ended, so a step big
        // enough to clear a wall entirely -- a teleport, a lag spike, a frame
        // that took a tenth of a second -- lands on the far side having
        // overlapped nothing. Testing the *path* catches that, where testing
        // the destination cannot. Being left where you started is worse than
        // sliding and far better than being outside the world.
        if self.crosses_wall(from, at, height, step) || self.blocked(at, radius, height, step) {
            // Unless the start was already inside something, in which case
            // refusing would weld the character in place for ever.
            if !self.blocked(from, radius, height, step) {
                return from;
            }
        }
        at
    }

    /// Whether a straight path from `from` to `to` passes through a wall.
    ///
    /// Sampled at the feet, the middle and the head rather than as a swept
    /// cylinder: three segments catch anything a character-sized body could
    /// pass through, and a proper sweep is a great deal of arithmetic for a
    /// case that only arises when something has already gone wrong.
    pub fn crosses_wall(&self, from: Vec3, to: Vec3, height: f32, step: f32) -> bool {
        if self.triangles.is_empty() {
            return false;
        }
        let span = (to.truncate() - from.truncate()).length();
        let middle = (from.truncate() + to.truncate()) * 0.5;
        let candidates = self.near(middle, span * 0.5 + CELL);
        // Sampled from the step height upward, for the same reason the
        // push-out band starts there: a sample at the ankles would find every
        // stair riser and refuse the move that the band above it just allowed.
        for offset in [step + 0.05, (step + height) * 0.5, height * 0.9] {
            let lift = Vec3::new(0.0, 0.0, offset);
            for &index in &candidates {
                let triangle = self.triangles[index as usize];
                if triangle.is_floor() {
                    continue;
                }
                if triangle.crossed_by(from + lift, to + lift) {
                    return true;
                }
            }
        }
        false
    }

    /// How far along `from` -> `to` the first solid surface is, as a fraction,
    /// or `None` for a clear line.
    ///
    /// **Floors count here, and that is the difference from
    /// [`World::crosses_wall`].** That one asks whether a *body* can walk a
    /// path and so ignores the ground it walks on; this one exists for the
    /// camera, which is stopped by a floor above it and a ceiling below it
    /// exactly as it is stopped by a wall. Asking the same question for both
    /// would either let the camera through walls or refuse to let a character
    /// walk anywhere.
    ///
    /// A bare segment rather than a swept sphere: the camera is a point, and
    /// the caller keeps its own margin so the near plane does not end up
    /// inside the surface it stopped at.
    pub fn first_hit(&self, from: Vec3, to: Vec3) -> Option<f32> {
        if self.triangles.is_empty() {
            return None;
        }
        let span = (to.truncate() - from.truncate()).length();
        let middle = (from.truncate() + to.truncate()) * 0.5;
        self.near(middle, span * 0.5 + CELL)
            .into_iter()
            .filter_map(|index| self.triangles[index as usize].hit_at(from, to))
            .fold(None, |nearest: Option<f32>, t| {
                Some(nearest.map_or(t, |best| best.min(t)))
            })
    }

    /// [`World::first_hit`], plus the surface normal of whichever triangle
    /// answered.
    ///
    /// **What lets a caller tell a wall from a floor or a ceiling** among the
    /// shapes `first_hit` already treats alike -- everything solid, by
    /// design. A wall wants the eye pulled in front of it; a low ceiling
    /// wants the eye ducked under it instead, and doing the first for the
    /// second reads as the camera collapsing onto the character rather than
    /// as a tunnel with a low roof. The normal is what tells the two apart,
    /// the same `abs(normal.z)` test [`Triangle::is_floor`] already makes.
    ///
    /// Every stored triangle has a normal by construction -- [`World::add_tagged`]
    /// refuses a degenerate one before it is ever kept -- so this answers
    /// whenever `first_hit` would.
    pub fn first_hit_with_normal(&self, from: Vec3, to: Vec3) -> Option<(f32, Vec3)> {
        if self.triangles.is_empty() {
            return None;
        }
        let span = (to.truncate() - from.truncate()).length();
        let middle = (from.truncate() + to.truncate()) * 0.5;
        self.near(middle, span * 0.5 + CELL)
            .into_iter()
            .filter_map(|index| {
                self.triangles[index as usize]
                    .hit_at(from, to)
                    .map(|t| (t, index))
            })
            .fold(None, |nearest: Option<(f32, u32)>, (t, index)| {
                Some(match nearest {
                    Some(best) if best.0 <= t => best,
                    _ => (t, index),
                })
            })
            .and_then(|(t, index)| {
                self.triangles[index as usize]
                    .normal()
                    .map(|normal| (t, normal))
            })
    }

    /// Whether a cylinder here overlaps anything solid.
    pub fn blocked(&self, at: Vec3, radius: f32, height: f32, step: f32) -> bool {
        // See `wall_exemption` and `slide`, which this must agree with: a
        // move `slide`'s push-out phase considers fine must not be vetoed
        // here by a stricter idea of which walls are exempt.
        self.near(at.truncate(), radius).into_iter().any(|index| {
            let triangle = self.triangles[index as usize];
            // The same order as `slide`, and it has to stay the same order:
            // this and `slide`'s push-out phase must agree about which walls
            // are exempt, so they make the identical tests in the identical
            // sequence. See `push_out_horizontally`.
            if triangle.push_out_horizontally(at.truncate(), radius).is_none()
                || triangle.under_any_band(at.z, step)
            {
                return false;
            }
            let foot = triangle.foot_towards(at.truncate());
            let ceiling = self.wall_exemption(foot, triangle.max().z, at.z, step);
            triangle.overlaps_band(ceiling, ceiling + height)
        })
    }
}

fn cell_of(v: f32) -> i32 {
    (v / CELL).floor() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a straight flight of risers along `+X`, `width` wide, each
    /// `riser_h` tall and `tread_d` deep, plus a flat approach on the ground
    /// before the first one. Shared by the shallow- and deep-tread tests
    /// below, which differ only in `tread_d`.
    fn flight_of_stairs(riser_h: f32, tread_d: f32, width: f32, count: usize) -> World {
        let mut world = World::new();
        for i in 0..count {
            let x0 = i as f32 * tread_d;
            let x1 = x0 + tread_d;
            let z0 = i as f32 * riser_h;
            let z1 = z0 + riser_h;
            world.add(Triangle::new(
                Vec3::new(x0, -width, z0),
                Vec3::new(x0, width, z0),
                Vec3::new(x0, -width, z1),
            ));
            world.add(Triangle::new(
                Vec3::new(x0, width, z0),
                Vec3::new(x0, width, z1),
                Vec3::new(x0, -width, z1),
            ));
            world.add(Triangle::new(
                Vec3::new(x0, -width, z1),
                Vec3::new(x1, -width, z1),
                Vec3::new(x0, width, z1),
            ));
            world.add(Triangle::new(
                Vec3::new(x1, -width, z1),
                Vec3::new(x1, width, z1),
                Vec3::new(x0, width, z1),
            ));
        }
        world.add(Triangle::new(
            Vec3::new(-2.0, -width, 0.0),
            Vec3::new(0.0, -width, 0.0),
            Vec3::new(-2.0, width, 0.0),
        ));
        world.add(Triangle::new(
            Vec3::new(0.0, -width, 0.0),
            Vec3::new(0.0, width, 0.0),
            Vec3::new(-2.0, width, 0.0),
        ));
        world
    }

    /// **foss-wow#126.** A flight of risers shallower in run than the body's
    /// own radius -- measured on `NSabbey.wmo`'s `Stairs1`: 0.33 units of
    /// tread depth against `BODY_RADIUS`'s 0.55 -- refused every step of the
    /// whole climb, reported as "can't walk up them without jumping". At the
    /// foot of the first riser the second one already sits within `radius`,
    /// so it was tested as an ordinary wall no different from a fence, and
    /// the last-resort refusal in [`World::slide`] sent every attempted step
    /// straight back to `from`.
    ///
    /// The exact geometry and starting position an offline replay of the
    /// live report reproduced the bug against, before `wall_exemption` and
    /// `Triangle::foot_towards` existed to fix it.
    #[test]
    fn a_shallow_riser_does_not_block_the_step_onto_it() {
        let world = flight_of_stairs(0.44, 0.33, 2.0, 6);
        let (radius, height, step) = (0.55f32, 2.0f32, 0.8f32);
        let from = Vec3::new(-0.1, 0.0, 0.0);
        let wanted = Vec3::new(-0.02, 0.0, 0.0);

        let slid = world.slide(from, wanted, radius, height, step);
        assert!(
            (slid - wanted).length() < 1e-3,
            "the step onto the first riser should have been granted in full: {slid:?}"
        );
    }

    /// **The control.** A staircase with the same riser height but a tread
    /// deep enough that the second riser sits outside `radius` behaves
    /// exactly as it always did -- this is what confirms the fix only ever
    /// *adds* an exemption, on the one specific geometry that needed it,
    /// rather than loosening wall collision in general.
    #[test]
    fn an_ordinary_deep_tread_flight_is_unaffected() {
        let world = flight_of_stairs(0.44, 1.2, 2.0, 6);
        let (radius, height, step) = (0.55f32, 2.0f32, 0.8f32);
        let from = Vec3::new(-0.1, 0.0, 0.0);
        let wanted = Vec3::new(-0.02, 0.0, 0.0);

        let slid = world.slide(from, wanted, radius, height, step);
        assert!(
            (slid - wanted).length() < 1e-3,
            "an ordinary staircase must climb exactly as it did before: {slid:?}"
        );
    }

    /// **The other half of "only ever adds an exemption".** A wall standing
    /// on open ground -- nothing above it is a climbable riser stacked on a
    /// tread within `step`, because there is no tread at all -- must still
    /// block, exactly as before this existed.
    #[test]
    fn a_standalone_wall_on_open_ground_still_blocks() {
        let mut world = World::new();
        let width = 2.0;
        // A single wall 1.5 units tall -- taller than any riser this file's
        // other tests use, and taller than `step` -- with open, flat ground
        // on both sides of it.
        world.add(Triangle::new(
            Vec3::new(-5.0, -width, 0.0),
            Vec3::new(-5.0, width, 0.0),
            Vec3::new(5.0, -width, 0.0),
        ));
        world.add(Triangle::new(
            Vec3::new(-5.0, width, 0.0),
            Vec3::new(5.0, width, 0.0),
            Vec3::new(5.0, -width, 0.0),
        ));
        world.add(Triangle::new(
            Vec3::new(0.0, -width, 0.0),
            Vec3::new(0.0, width, 0.0),
            Vec3::new(0.0, -width, 1.5),
        ));
        world.add(Triangle::new(
            Vec3::new(0.0, width, 0.0),
            Vec3::new(0.0, width, 1.5),
            Vec3::new(0.0, -width, 1.5),
        ));

        let (radius, height, step) = (0.55f32, 2.0f32, 0.8f32);
        let from = Vec3::new(-0.1, 0.0, 0.0);
        let wanted = Vec3::new(0.3, 0.0, 0.0);

        let slid = world.slide(from, wanted, radius, height, step);
        assert!(
            (slid - from).length() < 1e-3,
            "a standalone wall taller than a step must still refuse the walk-through: {slid:?}"
        );
    }

    /// A tagged surface reports its tag, an untagged one reports none, and the
    /// tag belongs to the triangle that actually won.
    ///
    /// The last part is what the test exists for: two floors at different
    /// heights under one point is exactly a building's floorboards over its
    /// cellar, and reporting the tag of the wrong one is a wrong sound with a
    /// right height, which nothing else here would catch.
    #[test]
    fn a_floor_reports_the_tag_of_the_surface_that_won() {
        let mut world = World::new();
        let flat = |z: f32| {
            Triangle::new(
                Vec3::new(-5.0, -5.0, z),
                Vec3::new(5.0, -5.0, z),
                Vec3::new(0.0, 5.0, z),
            )
        };
        world.add_tagged(flat(0.0), 4);
        world.add_tagged(flat(2.0), 2);
        world.add(flat(-3.0));

        // Standing above both: the higher one wins and brings its own tag.
        assert_eq!(
            world.floor_under_tagged(Vec2::new(0.0, 0.0), 3.0, 0.5),
            Some((2.0, Some(2)))
        );
        // Below the upper floor: the lower one wins, with its own.
        assert_eq!(
            world.floor_under_tagged(Vec2::new(0.0, 0.0), 0.2, 0.5),
            Some((0.0, Some(4)))
        );
        // The untagged one answers `None` rather than a made-up value.
        assert_eq!(
            world.floor_under_tagged(Vec2::new(0.0, 0.0), -2.9, 0.5),
            Some((-3.0, None))
        );
        // And the untagged view agrees with the tagged one about the height,
        // which is the property that lets both exist.
        assert_eq!(world.floor_under(Vec2::new(0.0, 0.0), 3.0, 0.5), Some(2.0));
    }

    /// A degenerate triangle is skipped, and skipping it must not slide every
    /// later triangle's tag by one. The tags are a parallel array, so this is
    /// the failure it can have.
    #[test]
    fn a_skipped_triangle_does_not_shift_the_tags() {
        let mut world = World::new();
        let zero = Vec3::ZERO;
        world.add_tagged(Triangle::new(zero, zero, zero), 9);
        world.add_tagged(
            Triangle::new(
                Vec3::new(-5.0, -5.0, 1.0),
                Vec3::new(5.0, -5.0, 1.0),
                Vec3::new(0.0, 5.0, 1.0),
            ),
            7,
        );
        assert_eq!(world.triangle_count(), 1);
        assert_eq!(
            world.floor_under_tagged(Vec2::new(0.0, 0.0), 2.0, 0.5),
            Some((1.0, Some(7)))
        );
    }

    /// An axis-aligned box, as twelve triangles, from `min` to `max`.
    fn box_at(min: Vec3, max: Vec3) -> Vec<Triangle> {
        let c = |x: f32, y: f32, z: f32| Vec3::new(x, y, z);
        let (a, b) = (min, max);
        let corners = [
            c(a.x, a.y, a.z),
            c(b.x, a.y, a.z),
            c(b.x, b.y, a.z),
            c(a.x, b.y, a.z),
            c(a.x, a.y, b.z),
            c(b.x, a.y, b.z),
            c(b.x, b.y, b.z),
            c(a.x, b.y, b.z),
        ];
        const FACES: [[usize; 4]; 6] = [
            // Wound the opposite way from every other face here on purpose:
            // the naive [0,1,2,3] order gives this face the same +Z normal
            // the top face has, which is outward for the top and *inward*
            // for the bottom. Nothing before `first_hit_with_normal` ever
            // asked a box which way its faces pointed -- `is_floor` and
            // `floor_hit` both take `abs(normal.z)`, so the sign was free to
            // be wrong -- and this is the first test that reads it.
            [0, 3, 2, 1], // bottom
            [4, 5, 6, 7], // top
            [0, 1, 5, 4],
            [1, 2, 6, 5],
            [2, 3, 7, 6],
            [3, 0, 4, 7],
        ];
        FACES
            .iter()
            .flat_map(|f| {
                [
                    Triangle::new(corners[f[0]], corners[f[1]], corners[f[2]]),
                    Triangle::new(corners[f[0]], corners[f[2]], corners[f[3]]),
                ]
            })
            .collect()
    }

    /// A room's worth of floor packed into the cells a body stands in, which
    /// is what a building actually looks like to the grid.
    ///
    /// Deliberately floors: they are the triangles `push_out` refuses first
    /// and cheapest, and the ones the old ordering paid a **grid lookup**
    /// apiece for before finding that out.
    fn a_crowded_floor(tiles: i32) -> Vec<Triangle> {
        let mut out = Vec::new();
        for x in 0..tiles {
            for y in 0..tiles {
                let (x, y) = (x as f32 * 0.25, y as f32 * 0.25);
                let q = |dx: f32, dy: f32| Vec3::new(x + dx, y + dy, 0.0);
                out.push(Triangle::new(q(0.0, 0.0), q(0.25, 0.0), q(0.25, 0.25)));
                out.push(Triangle::new(q(0.0, 0.0), q(0.25, 0.25), q(0.0, 0.25)));
            }
        }
        out
    }

    /// **Walking across a crowded floor must not cost a grid lookup per
    /// triangle in the cell.**
    ///
    /// This is the shape of the abbey report, reduced. `slide` narrows once
    /// through the grid and then, for every candidate that came back, asked
    /// `wall_exemption` -- which is itself a grid lookup. That is O(n) lookups
    /// and O(n^2) triangle tests for one step, and in a building `n` is the
    /// thousand triangles sharing a cell. Measured live in Northshire abbey:
    /// **4,193 lookups and 3.1 million candidate triangles in one frame**,
    /// against 70 and 9,274 standing outside in the open, and 10-22 ms of a
    /// 20-38 ms frame.
    ///
    /// **A count, not a duration.** A timing assertion would be flaky, would
    /// pass on a fast machine with the bug present, and would say nothing
    /// about *why* -- exactly the reasoning the model-cache regression test
    /// already uses when it counts archive reads rather than milliseconds.
    /// The bound is deliberately loose: this is not tuning, it is the
    /// difference between a constant and a per-triangle cost.
    #[test]
    fn sliding_through_a_crowded_cell_does_not_look_up_the_grid_per_triangle() {
        let world = world_with(a_crowded_floor(24));
        assert!(
            world.triangle_count() > 1000,
            "the sample has to be crowded or it proves nothing: {} triangles",
            world.triangle_count()
        );
        // The candidates one narrowing hands back, which is what the old
        // ordering then multiplied by itself.
        world.take_probe();
        let from = Vec3::new(2.0, 2.0, 0.0);
        world.slide(from, Vec3::new(2.2, 2.0, 0.0), 0.55, 2.0, 0.5);
        let probe = world.take_probe();
        assert!(
            probe.queries < 40,
            "one step should narrow through the grid a handful of times, not              once per triangle sharing the cell -- made {} lookups over {}              candidates",
            probe.queries,
            probe.candidates
        );
    }

    /// The same bound for the standing-still check, which walks the identical
    /// candidate list and made the identical mistake.
    ///
    /// Its own test because `slide` calls it twice and would mask it: a fix
    /// applied to one and not the other still leaves a per-triangle lookup in
    /// every frame, and `slide`'s own count would look two thirds better.
    #[test]
    fn asking_whether_a_crowded_cell_blocks_you_is_not_per_triangle_either() {
        let world = world_with(a_crowded_floor(24));
        world.take_probe();
        assert!(!world.blocked(Vec3::new(2.0, 2.0, 0.0), 0.55, 2.0, 0.5));
        let probe = world.take_probe();
        assert!(
            probe.queries < 10,
            "expected a handful of lookups, got {} over {} candidates",
            probe.queries,
            probe.candidates
        );
    }

    /// **A point query must not pay for merging cells it never touched.**
    ///
    /// `floor_under_tagged_with_id` is the most called function in the frame
    /// -- the follow camera alone marches the ground eighteen times per
    /// sampled yaw, three yaws a frame, fanned across every resident tile --
    /// and it asks about a single point, which is a single cell. It used to
    /// go through `near`, which allocates, copies, sorts and deduplicates.
    ///
    /// Asserting the *answer* is unchanged rather than that it is fast: the
    /// fast path is only allowed to exist because one cell cannot contain a
    /// duplicate, and the thing that would break is the height it returns.
    #[test]
    fn the_single_cell_path_answers_exactly_as_the_general_one_does() {
        let world = world_with(a_crowded_floor(24));
        for (x, y) in [(0.1, 0.1), (2.0, 2.0), (3.7, 1.2), (5.9, 5.9)] {
            let at = Vec2::new(x, y);
            let fast = world.floor_under_tagged_with_id(at, 1.0, 0.5);
            // The general path, reached through the same grid.
            let slow = world
                .near(at, 0.0)
                .into_iter()
                .filter_map(|i| world.triangles[i as usize].floor_hit(at))
                .filter(|z| *z <= 1.5)
                .fold(None, |b: Option<f32>, z| Some(b.map_or(z, |b| b.max(z))));
            assert_eq!(fast.map(|(z, _, _)| z), slow, "at {at:?}");
        }
    }

    /// The counters themselves, because a probe stuck at zero would make both
    /// tests above pass with the bug fully present -- and that is the most
    /// likely way for this to rot. See `Probe`.
    #[test]
    fn the_probe_counts_something_and_resets() {
        let world = world_with(a_crowded_floor(4));
        world.take_probe();
        world.floor_under(Vec2::new(0.2, 0.2), 1.0, 0.5);
        let probe = world.take_probe();
        assert_eq!(probe.queries, 1, "one narrowing, counted");
        assert!(probe.candidates > 0, "the cell is not empty");
        assert_eq!(
            world.take_probe(),
            Probe::default(),
            "reading has to zero it, or a per-frame figure is a session total"
        );
    }

    fn world_with(triangles: Vec<Triangle>) -> World {
        let mut world = World::new();
        for t in triangles {
            world.add(t);
        }
        world
    }

    /// **A wall between two points is found, at the right distance**, and open
    /// air between them is not.
    ///
    /// This is what a third-person camera needs and what `crosses_wall` cannot
    /// give it: that one answers yes or no, and a camera pulled all the way in
    /// whenever *anything* was in the way would sit in the character's head
    /// every time they walked past a doorframe.
    ///
    /// Both halves, because a query that always reported a hit would pass a
    /// test that only checked the wall.
    #[test]
    fn the_first_hit_is_found_and_measured() {
        // A wall two units thick, its near face at x = 4.
        let world = world_with(box_at(
            Vec3::new(4.0, -10.0, 0.0),
            Vec3::new(6.0, 10.0, 5.0),
        ));
        let eye = Vec3::new(10.0, 0.0, 2.0);
        let from = Vec3::new(0.0, 0.0, 2.0);

        let t = world.first_hit(from, eye).expect("the wall was missed");
        // Ten units out, the near face at four: four tenths of the way.
        assert!((t - 0.4).abs() < 1e-3, "hit reported at {t}");

        // Along the wall rather than through it: nothing in the way.
        assert_eq!(
            world.first_hit(Vec3::new(0.0, 0.0, 2.0), Vec3::new(0.0, 9.0, 2.0)),
            None,
            "open air reported a hit, which would jam the camera against nothing"
        );
        // And over the top of it, which is the case a yes/no answer for the
        // whole segment would get wrong.
        assert_eq!(
            world.first_hit(Vec3::new(0.0, 0.0, 8.0), Vec3::new(10.0, 0.0, 8.0)),
            None,
            "a line clearing the wall was stopped by it"
        );
    }

    /// **What separates a wall from a ceiling.** Both can stop the camera --
    /// `a_floor_blocks_the_camera_but_not_a_walker` below covers that a floor
    /// does too -- but only the normal says which is which, and that is the
    /// one thing a caller needs to duck the eye under a low roof instead of
    /// pulling it all the way in to the character, which is what treating
    /// every hit alike used to do.
    #[test]
    fn first_hit_with_normal_tells_a_wall_from_a_ceiling() {
        // The same wall as `the_first_hit_is_found_and_measured`: near
        // vertical, its normal close to horizontal.
        let wall = world_with(box_at(Vec3::new(4.0, -10.0, 0.0), Vec3::new(6.0, 10.0, 5.0)));
        let (t, normal) = wall
            .first_hit_with_normal(Vec3::new(0.0, 0.0, 2.0), Vec3::new(10.0, 0.0, 2.0))
            .expect("the wall was missed");
        assert!((t - 0.4).abs() < 1e-3, "hit reported at {t}");
        assert!(
            normal.z.abs() < 0.1,
            "a wall's normal should be close to horizontal: {normal:?}"
        );

        // A low ceiling: a thin slab overhead, hit from directly below.
        let ceiling = world_with(box_at(Vec3::new(-5.0, -5.0, 3.0), Vec3::new(5.0, 5.0, 3.2)));
        let (_, normal) = ceiling
            .first_hit_with_normal(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 5.0))
            .expect("the ceiling was missed");
        assert!(
            normal.z < -0.9,
            "a ceiling's outward normal should point down, into the room under it: {normal:?}"
        );
    }

    /// **A floor stops the camera and does not stop a walker.** The two
    /// queries disagree on purpose: `crosses_wall` skips floors so a character
    /// can walk on them, and `first_hit` must not, or the eye drops through
    /// the ground it is standing over.
    #[test]
    fn a_floor_blocks_the_camera_but_not_a_walker() {
        let world = world_with(box_at(
            Vec3::new(-5.0, -5.0, 0.0),
            Vec3::new(5.0, 5.0, 0.2),
        ));
        let above = Vec3::new(0.0, 0.0, 3.0);
        let below = Vec3::new(0.0, 0.0, -3.0);
        assert!(
            world.first_hit(above, below).is_some(),
            "the camera would pass through the floor"
        );
        assert!(
            !world.crosses_wall(
                Vec3::new(-3.0, 0.0, 0.2),
                Vec3::new(3.0, 0.0, 0.2),
                2.0,
                0.5
            ),
            "walking across a floor must stay allowed"
        );
    }

    /// The whole point: a character cannot end up on the far side of a wall.
    #[test]
    fn a_wall_cannot_be_walked_through() {
        // A wall across the y axis at x = 0, two units thick.
        let world = world_with(box_at(
            Vec3::new(-1.0, -10.0, 0.0),
            Vec3::new(1.0, 10.0, 5.0),
        ));
        let from = Vec3::new(-4.0, 0.0, 0.0);
        let to = Vec3::new(-1.5, 0.0, 0.0);
        let at = world.slide(from, to, 0.5, 2.0, 0.0);
        assert!(
            at.x < -1.4,
            "walked into the wall: ended at {at:?}"
        );
        // And the far side stays unreachable even when aimed straight at it.
        let through = world.slide(from, Vec3::new(4.0, 0.0, 0.0), 0.5, 2.0, 0.0);
        assert!(
            through.x < 0.0,
            "walked clean through a two-unit wall to {through:?}"
        );
    }

    /// Running at a wall on the diagonal carries you along it.
    ///
    /// The half that separates sliding from stopping: a hard stop would leave
    /// the character where it started, with neither component of the move
    /// surviving.
    #[test]
    fn a_diagonal_run_slides_along_the_wall() {
        let world = world_with(box_at(
            Vec3::new(-1.0, -20.0, 0.0),
            Vec3::new(1.0, 20.0, 5.0),
        ));
        let from = Vec3::new(-2.0, 0.0, 0.0);
        // Straight at the wall and along it in equal measure.
        let to = Vec3::new(-1.0, 1.0, 0.0);
        let at = world.slide(from, to, 0.5, 2.0, 0.0);
        assert!(at.x < -1.4, "the wall was entered: {at:?}");
        assert!(
            at.y > 0.5,
            "the move along the wall was thrown away with the move into it: {at:?}"
        );
    }

    /// Open ground is left exactly alone.
    ///
    /// A collision system that quietly nudges a character in empty space is
    /// worse than none: it shows up as drift nobody can attribute.
    #[test]
    fn nothing_solid_means_nothing_changes() {
        let world = world_with(box_at(
            Vec3::new(50.0, 50.0, 0.0),
            Vec3::new(52.0, 52.0, 5.0),
        ));
        let to = Vec3::new(3.0, 4.0, 1.0);
        assert_eq!(world.slide(Vec3::new(0.0, 0.0, 1.0), to, 0.5, 2.0, 0.0), to);
        assert_eq!(World::new().slide(Vec3::ZERO, to, 0.5, 2.0, 0.0), to);
    }

    /// A floor is stood on, and a roof overhead is not.
    #[test]
    fn the_floor_under_you_is_the_one_you_are_standing_on() {
        // A box from z 0 to 5: its lid at 5 is a roof, its base at 0 a floor.
        let world = world_with(box_at(
            Vec3::new(-5.0, -5.0, 0.0),
            Vec3::new(5.0, 5.0, 5.0),
        ));
        let at = Vec2::ZERO;

        // Standing inside on the base: the roof is above and must be ignored.
        assert_eq!(world.floor_under(at, 0.0, 0.5), Some(0.0));
        // Standing on the roof: it is what holds you up.
        assert_eq!(world.floor_under(at, 5.0, 0.5), Some(5.0));
        // Nothing under a point outside the box at all.
        assert_eq!(world.floor_under(Vec2::new(50.0, 50.0), 0.0, 0.5), None);
    }

    /// A margin sized to reach a low ceiling finds one; a margin sized only
    /// for stair-stepping does not -- foss-wow#137's second bug.
    ///
    /// `floor_under_tagged` bounds candidates from *above*
    /// (`ceiling = from_z + step`) and leaves the depth below unlimited, so a
    /// generous `step` does not search "deeper", it searches "higher" -- and
    /// `is_floor`/`floor_hit` take `abs(normal.z)`, so a downward-facing
    /// ceiling passes the same near-horizontal test a floor does (see
    /// `the_floor_under_you_is_the_one_you_are_standing_on`, which depends on
    /// that same ambiguity for a box's underside). A camera querying near a
    /// character's own head with a five-unit margin -- chosen to comfortably
    /// reach a floor several units *below* -- ends up just as able to reach a
    /// tunnel roof five units *above*.
    #[test]
    fn a_small_step_does_not_mistake_a_ceiling_for_a_floor() {
        let mut world = World::new();
        // A floor at z = 0.0, normal +Z.
        world.add(Triangle::new(
            Vec3::new(-5.0, -5.0, 0.0),
            Vec3::new(5.0, -5.0, 0.0),
            Vec3::new(0.0, 5.0, 0.0),
        ));
        // A low ceiling at z = 5.0, normal -Z.
        world.add(Triangle::new(
            Vec3::new(-5.0, -5.0, 5.0),
            Vec3::new(0.0, 5.0, 5.0),
            Vec3::new(5.0, -5.0, 5.0),
        ));
        let at = Vec2::ZERO;
        // Querying near head height (2.2) with a margin generous enough to
        // reach a floor below reproduces the misread: the ceiling is the
        // only candidate within it, and wins.
        assert_eq!(world.floor_under(at, 2.2, 5.0), Some(5.0));
        // A margin sized for stair-stepping and terrain noise, not for
        // reaching a room's ceiling, finds the real floor instead -- and
        // still finds it, because the search below has no limit at all.
        assert_eq!(world.floor_under(at, 2.2, 1.0), Some(0.0));
    }

    /// A step up is climbed; a wall of the same shape is not.
    ///
    /// These are the same query with one number changed, which is the point:
    /// `step` is what separates a stair from an obstacle, and it belongs to
    /// the caller rather than to the geometry.
    #[test]
    fn a_step_is_climbed_and_a_ledge_is_not() {
        let world = world_with(box_at(
            Vec3::new(-5.0, -5.0, 0.0),
            Vec3::new(5.0, 5.0, 1.0),
        ));
        let at = Vec2::ZERO;
        // Standing at ground level with a generous step: the top is reachable.
        assert_eq!(world.floor_under(at, 0.0, 1.5), Some(1.0));
        // With a small step the top is out of reach, and what answers instead
        // is the box's *underside* -- which is a floor too, and is the surface
        // actually under the character's feet. An earlier version of this test
        // expected `None` and was wrong about its own fixture: a closed box
        // has a bottom.
        assert_eq!(world.floor_under(at, 0.0, 0.2), Some(0.0));
    }

    /// A wall is not a floor and a floor is not a wall.
    #[test]
    fn surfaces_are_sorted_by_how_far_they_lean() {
        let flat = Triangle::new(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        let wall = Triangle::new(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        );
        assert!(flat.is_floor());
        assert!(!wall.is_floor());
        // A degenerate triangle answers "no" rather than producing a NaN that
        // poisons every comparison downstream.
        let sliver = Triangle::new(Vec3::ZERO, Vec3::X, Vec3::X * 2.0);
        assert!(sliver.normal().is_none());
        assert!(!sliver.is_floor());
    }

    /// A step is walked over; a wall of the same footprint is not.
    ///
    /// **Both halves in one test, because the fix for the first is a way of
    /// failing the second.** Raising the collision band by the step height is
    /// what lets a stair riser be climbed -- and raise it far enough and every
    /// wall in the game becomes a kerb. The step height *is* the line between
    /// "walked over" and "walked into", so the test has to hold something on
    /// each side of it.
    #[test]
    fn a_low_step_is_walked_over_and_a_wall_is_not() {
        const STEP: f32 = 0.7;
        let low = world_with(box_at(
            Vec3::new(-1.0, -10.0, 0.0),
            Vec3::new(1.0, 10.0, 0.3),
        ));
        let tall = world_with(box_at(
            Vec3::new(-1.0, -10.0, 0.0),
            Vec3::new(1.0, 10.0, 5.0),
        ));
        let from = Vec3::new(-3.0, 0.0, 0.0);
        let to = Vec3::new(-1.2, 0.0, 0.0);

        // The riser is shorter than a step, so nothing stops the character
        // reaching it. Standing *on* it is the caller's ground query, not this
        // one -- see `floor_under`.
        assert_eq!(
            low.slide(from, to, 0.5, 2.0, STEP),
            to,
            "a step shorter than the step height blocked the move"
        );
        // The same footprint, taller than a character: still a wall.
        let stopped = tall.slide(from, to, 0.5, 2.0, STEP);
        assert!(
            stopped.x < to.x - 0.1,
            "a five-unit wall was stepped over: {stopped:?}"
        );

        // And with no step allowance at all, the riser blocks -- which is the
        // behaviour that was reported, and is what the parameter exists to
        // change rather than something it changed by accident.
        let without = low.slide(from, to, 0.5, 2.0, 0.0);
        assert!(
            without.x < to.x - 0.1,
            "the riser stopped blocking for some reason other than the step: {without:?}"
        );
    }

    /// An inside corner ejects the character out of both walls, not one.
    #[test]
    fn an_inside_corner_does_not_trap_you_in_a_wall() {
        let mut triangles = box_at(Vec3::new(-1.0, -20.0, 0.0), Vec3::new(1.0, 0.0, 5.0));
        triangles.extend(box_at(
            Vec3::new(-20.0, -1.0, 0.0),
            Vec3::new(0.0, 1.0, 5.0),
        ));
        let world = world_with(triangles);
        // Aimed into the corner where the two meet.
        let at = world.slide(
            Vec3::new(-4.0, 4.0, 0.0),
            Vec3::new(-0.6, 0.6, 0.0),
            0.6,
            2.0,
            0.0,
        );
        assert!(
            !world.blocked(at, 0.6, 2.0, 0.0),
            "ended up inside a wall at {at:?}"
        );
    }
}
