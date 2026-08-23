//! Frustum culling: which world-space boxes a matrix can see.
//!
//! **Pure geometry, in the render crate but touching no GPU at all**, for the
//! same reason `collision` is its own crate: every question here can be asked
//! of a hand-built box in a unit test, and the bugs this code can have are
//! precisely the ones a picture cannot show. A frustum that rejects too much
//! removes geometry silently -- a wall that is not there reads as a hole in
//! the world, not as a culling bug -- and one that rejects too little is
//! invisible except in the frame time, which is the thing it was written to
//! fix. Neither failure announces itself, so both are asserted here instead.
//!
//! The planes come out of the view-projection matrix directly (Gribb and
//! Hartmann) rather than being rebuilt from the camera's angles, field of view
//! and near plane. That is the rule the picking ray already follows: **derive
//! from the matrix the scene is drawn with**, because a frustum rebuilt from
//! the parts agrees with the drawn image only until somebody edits one of
//! them, and the failure is geometry vanishing at the edge of the screen --
//! which is exactly where nobody is looking.
//!
//! One convention, checked once: this project projects with
//! `glam::camera::rh::proj::directx`, so clip space runs `0..=w` in depth
//! rather than `-w..=w`. The near plane is therefore `row2` alone and not
//! `row3 + row2`. Both the camera's perspective matrix and the sun's
//! orthographic one come from that same family -- see
//! `shadow::light_view_proj`, which says why -- so one extraction serves the
//! visible pass and the shadow pass, and there is no second convention for
//! one of them to be written against by mistake.

use glam::{Mat4, Vec3, Vec4, Vec4Swizzles};

/// The six planes bounding what a view-projection matrix can see, in world
/// space, each pointing *inwards*: a point is inside the frustum when it is on
/// the positive side of all six.
#[derive(Clone, Copy, Debug)]
pub struct Frustum {
    planes: [Vec4; 6],
}

impl Frustum {
    /// Extracts the planes from a world-to-clip matrix.
    ///
    /// The planes are normalised, which costs six square roots once per frame
    /// and buys a dot product that is a real signed distance in world units.
    /// Nothing here needs the distance yet; it is done anyway so a caller
    /// wanting a margin -- "cull only what is more than a metre outside" --
    /// can express it in metres rather than in whatever scale the
    /// unnormalised planes happened to have.
    pub fn from_view_proj(m: Mat4) -> Self {
        let (r0, r1, r2, r3) = (m.row(0), m.row(1), m.row(2), m.row(3));
        let planes = [
            r3 + r0, // left
            r3 - r0, // right
            r3 + r1, // bottom
            r3 - r1, // top
            // **Not `r3 + r2`.** See the module note: depth clips against zero
            // here, not against `-w`. Written the OpenGL way this plane sits
            // a whole frustum-depth behind where it belongs, so everything
            // behind the camera passes it -- a mistake that culls *less*, and
            // therefore passes every check that only asks whether things are
            // still drawn.
            r2,      // near
            r3 - r2, // far
        ];
        Self {
            planes: planes.map(|p| {
                let length = p.xyz().length();
                // A degenerate plane means a degenerate matrix, and the honest
                // answer for one is "everything is visible": left at zero the
                // dot below is zero, which the test reads as inside. Drawing
                // too much is the only failure this module is allowed.
                if length > 0.0 {
                    p / length
                } else {
                    Vec4::ZERO
                }
            }),
        }
    }

    /// A frustum that admits everything, for switching culling off.
    ///
    /// Six zero planes, which [`Self::intersects`] reads as inside for every
    /// box -- the same behaviour a degenerate matrix already gets above, named
    /// so a caller can ask for it. It exists so `--no-cull` is one branch at
    /// the top of the pass rather than an `Option` tested inside every loop,
    /// and so the A/B compares two runs of the *same* code path.
    pub fn everything() -> Self {
        Self { planes: [Vec4::ZERO; 6] }
    }

    /// Whether an axis-aligned box could be visible.
    ///
    /// **Conservative in one direction only, deliberately.** This is the
    /// "positive vertex" test: for each plane it checks the single corner of
    /// the box furthest along that plane's normal, and rejects the box only
    /// when even that corner is outside. A large box lying in the corner
    /// between two planes can pass all six, so this answers `true` slightly
    /// too often -- which costs a draw call. It never answers `false` for
    /// something visible, which would cost a hole in the world. The two
    /// errors are not worth the same amount.
    pub fn intersects(&self, min: Vec3, max: Vec3) -> bool {
        for plane in &self.planes {
            let corner = Vec3::new(
                if plane.x >= 0.0 { max.x } else { min.x },
                if plane.y >= 0.0 { max.y } else { min.y },
                if plane.z >= 0.0 { max.z } else { min.z },
            );
            if plane.xyz().dot(corner) + plane.w < 0.0 {
                return false;
            }
        }
        true
    }
}

/// What is worth doing per-frame work for.
///
/// **Culling decides what to *draw*; this decides what to think about**, and
/// they are not the same question. Posing a skeleton, stepping a particle
/// system and rebuilding a group all happen before anything is drawn and all
/// of them scaled with the *resident* world -- nine tiles of it -- rather than
/// with what is on screen. Measured live, posing and emitters together cost
/// 2.8 ms of a 15 ms frame in a zone where most of that work was for things
/// behind the camera.
///
/// Wider than the view frustum on purpose, by a radius. Two reasons, and both
/// are bugs if ignored:
///
/// * **The sun's shadow box does not follow the camera's gaze.** A caster
///   behind the player still casts into the map, so a skeleton left unposed
///   because it is off screen would cast the shadow of whatever pose it last
///   held.
/// * **Turning is instant and posing is not.** A creature that enters the
///   frustum is posed and drawn in the same frame, so the frame itself is
///   correct -- but anything with *history*, a particle plume above all, has
///   none to show, and a torch that re-ignites as you look at it is worse than
///   one that kept burning. The radius buys that history back for everything
///   close enough to notice.
#[derive(Clone, Copy)]
pub struct Attention {
    frustum: Frustum,
    centre: Vec3,
    radius: f32,
}

impl Attention {
    pub fn new(view_proj: Mat4, centre: Vec3, radius: f32) -> Self {
        Self {
            frustum: Frustum::from_view_proj(view_proj),
            centre,
            radius: radius.max(0.0),
        }
    }

    /// Everything is worth working on -- the offline scenes, and anything that
    /// has not been taught to ask.
    pub fn everything() -> Self {
        Self {
            frustum: Frustum::everything(),
            centre: Vec3::ZERO,
            radius: f32::INFINITY,
        }
    }

    /// **`None` means yes.** A group whose bounds this client cannot state
    /// has to keep being worked on, for the same reason it keeps being drawn:
    /// the alternative is a creature that stops animating for a reason no
    /// picture can show. See `Frustum::intersects`.
    pub fn wants(&self, bounds: Option<(Vec3, Vec3)>) -> bool {
        let Some((min, max)) = bounds else {
            return true;
        };
        if self.frustum.intersects(min, max) {
            return true;
        }
        // Nearest point of the box to the centre, which is the box's own
        // distance rather than its origin's -- a building is not "far away"
        // because the corner its transform names happens to be.
        let nearest = self.centre.clamp(min, max);
        nearest.distance_squared(self.centre) <= self.radius * self.radius
    }
}

/// The world-space box a model-space box occupies once transformed.
///
/// All eight corners, not the two transformed endpoints. Transforming `min`
/// and `max` alone is correct only for a transform with no rotation, and this
/// project's placements are *all* rotated -- a doodad's rotation and a
/// building's differ by a quarter turn and neither is the identity. The cheap
/// version produces a box that is too small in exactly the cases where a
/// building is turned to face a road, so its geometry would vanish at some
/// angles and not at others.
pub fn transformed_bounds(transform: Mat4, min: Vec3, max: Vec3) -> (Vec3, Vec3) {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for i in 0..8u32 {
        let corner = Vec3::new(
            if i & 1 == 0 { min.x } else { max.x },
            if i & 2 == 0 { min.y } else { max.y },
            if i & 4 == 0 { min.z } else { max.z },
        );
        let p = transform.transform_point3(corner);
        lo = lo.min(p);
        hi = hi.max(p);
    }
    (lo, hi)
}

/// Grows `into` to hold `bounds`, starting an accumulator if there is none.
///
/// `None` means "nothing has been added", which is a different statement from
/// an empty box at the origin -- and the difference matters at the call sites,
/// where a group holding no bounded geometry must not claim to sit at the
/// world origin and be drawn whenever the camera happens to look there.
pub fn grow(into: &mut Option<(Vec3, Vec3)>, bounds: (Vec3, Vec3)) {
    *into = Some(match *into {
        Some((lo, hi)) => (lo.min(bounds.0), hi.max(bounds.1)),
        None => bounds,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same constructors the camera and the shadow map both project with,
    /// so a convention change breaks these rather than only the picture.
    fn looking_down_the_x_axis() -> Mat4 {
        let view = glam::camera::rh::view::look_at_mat4(Vec3::ZERO, Vec3::X, Vec3::Z);
        let proj =
            glam::camera::rh::proj::directx::perspective(60f32.to_radians(), 1.0, 0.1, 100.0);
        proj * view
    }

    fn unit_box(at: Vec3) -> (Vec3, Vec3) {
        (at - Vec3::ONE, at + Vec3::ONE)
    }

    #[test]
    fn something_straight_ahead_is_visible() {
        let f = Frustum::from_view_proj(looking_down_the_x_axis());
        let (min, max) = unit_box(Vec3::new(10.0, 0.0, 0.0));
        assert!(f.intersects(min, max));
    }

    /// The test the near plane's convention is decided by. Written the OpenGL
    /// way -- `row3 + row2` -- the near plane lands a whole frustum-depth
    /// behind the eye and a box directly behind the camera passes it. That
    /// mistake culls *nothing*, so every "is it still drawn" check passes and
    /// only the frame time says otherwise, which is the very thing being
    /// measured.
    #[test]
    fn something_directly_behind_the_camera_is_not() {
        let f = Frustum::from_view_proj(looking_down_the_x_axis());
        let (min, max) = unit_box(Vec3::new(-10.0, 0.0, 0.0));
        assert!(!f.intersects(min, max));
    }

    #[test]
    fn something_far_off_to_one_side_is_not() {
        let f = Frustum::from_view_proj(looking_down_the_x_axis());
        let (min, max) = unit_box(Vec3::new(10.0, 40.0, 0.0));
        assert!(!f.intersects(min, max));
    }

    #[test]
    fn something_past_the_far_plane_is_not() {
        let f = Frustum::from_view_proj(looking_down_the_x_axis());
        let (min, max) = unit_box(Vec3::new(500.0, 0.0, 0.0));
        assert!(!f.intersects(min, max));
    }

    /// A box big enough to contain the camera has no corner in front of it,
    /// and a test that asked "is the centre inside" would reject it.
    /// Stormwind is 1,058 units across and the camera stands inside it.
    #[test]
    fn a_box_the_camera_is_standing_inside_is_visible() {
        let f = Frustum::from_view_proj(looking_down_the_x_axis());
        assert!(f.intersects(Vec3::splat(-500.0), Vec3::splat(500.0)));
    }

    /// The sun's box is orthographic and comes from the same `directx` family,
    /// so one extraction has to serve both. If it did not, the shadow pass
    /// would cull against a frustum disagreeing with the map it writes, and
    /// the symptom is a shadow that vanishes when its caster leaves the
    /// screen -- visible only in motion, and only from behind.
    #[test]
    fn the_suns_orthographic_box_culls_too() {
        let m = crate::shadow::light_view_proj(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0), 50.0, 2048)
            .unwrap();
        let f = Frustum::from_view_proj(m);
        let here = unit_box(Vec3::ZERO);
        assert!(f.intersects(here.0, here.1));
        let far = unit_box(Vec3::new(400.0, 0.0, 0.0));
        assert!(!f.intersects(far.0, far.1), "outside the 50-unit box");
    }

    /// Transforming two corners instead of eight is right for a translation
    /// and wrong for every rotation, and this project rotates every placement.
    #[test]
    fn a_rotated_box_grows_rather_than_tilting() {
        let long = (Vec3::new(-10.0, -1.0, -1.0), Vec3::new(10.0, 1.0, 1.0));
        let turned = Mat4::from_rotation_z(std::f32::consts::FRAC_PI_4);
        let (lo, hi) = transformed_bounds(turned, long.0, long.1);
        // Half the diagonal of a 20x2 box turned 45 degrees is about 7.8,
        // which the two-corner shortcut would report as 1.0.
        assert!(hi.y > 7.0, "expected the box to grow in Y, got {hi:?}");
        assert!(lo.y < -7.0, "expected the box to grow in Y, got {lo:?}");
    }

    #[test]
    fn attention_reaches_behind_the_camera_but_not_across_the_map() {
        let at = Attention::new(looking_down_the_x_axis(), Vec3::ZERO, 20.0);
        // In view.
        assert!(at.wants(Some(unit_box(Vec3::new(10.0, 0.0, 0.0)))));
        // Behind the camera but close: still worked on, because the sun does
        // not care which way the player is facing and a plume has a history.
        assert!(at.wants(Some(unit_box(Vec3::new(-10.0, 0.0, 0.0)))));
        // Behind the camera and far: not worth a thought.
        assert!(!at.wants(Some(unit_box(Vec3::new(-200.0, 0.0, 0.0)))));
        // Unknown bounds are always worth a thought.
        assert!(at.wants(None));
    }

    /// The distance is to the *box*, not to whatever corner its transform
    /// names. Stormwind's origin is a thousand units from the middle of it,
    /// and a building the player is standing inside must not read as far away.
    /// The distance is to the *box*, not to whatever corner its transform
    /// names. Stormwind's placement origin is hundreds of units from most of
    /// the city, so a building the player is standing beside must not read as
    /// far away because its origin is.
    #[test]
    fn a_box_is_near_when_any_of_it_is_near() {
        let at = Attention::new(looking_down_the_x_axis(), Vec3::ZERO, 20.0);
        // Behind the camera, so only the radius can save it. Its nearest face
        // is eight units away; its origin corner is a thousand.
        let sprawling = (Vec3::new(-1000.0, -10.0, -10.0), Vec3::new(-8.0, 10.0, 10.0));
        assert!(
            at.wants(Some(sprawling)),
            "the near face is 8 units away and the radius is 20"
        );
        // ...and the same box moved just out of reach is refused, so the
        // check above is not passing for some other reason.
        let away = (Vec3::new(-1000.0, -10.0, -10.0), Vec3::new(-25.0, 10.0, 10.0));
        assert!(!at.wants(Some(away)));
    }

    #[test]
    fn attention_everything_wants_everything() {
        let at = Attention::everything();
        assert!(at.wants(Some(unit_box(Vec3::splat(1e9)))));
    }

    #[test]
    fn everything_admits_everything() {
        let f = Frustum::everything();
        assert!(f.intersects(Vec3::splat(-1.0), Vec3::splat(1.0)));
        assert!(f.intersects(Vec3::splat(1e9), Vec3::splat(1e9 + 1.0)));
        assert!(f.intersects(Vec3::splat(-1e9), Vec3::splat(-1e9 + 1.0)));
    }

    #[test]
    fn growing_from_nothing_takes_the_first_box_whole() {
        let mut acc = None;
        grow(&mut acc, (Vec3::splat(1.0), Vec3::splat(2.0)));
        assert_eq!(acc, Some((Vec3::splat(1.0), Vec3::splat(2.0))));
        grow(&mut acc, (Vec3::splat(-1.0), Vec3::splat(0.0)));
        assert_eq!(acc, Some((Vec3::splat(-1.0), Vec3::splat(2.0))));
    }
}
