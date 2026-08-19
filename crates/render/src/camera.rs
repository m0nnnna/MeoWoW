//! An orbit camera for inspecting a single object.
//!
//! WoW's world is **Z-up and right-handed**, so the camera keeps `+Z` as up
//! rather than the `+Y` most engine samples assume. Getting that wrong lays
//! every model on its side, which is easy to mistake for a broken vertex
//! layout.

use glam::{Mat4, Vec3};

use crate::mesh::CameraUniform;

/// A camera orbiting a target point.
#[derive(Clone, Copy, Debug)]
pub struct Orbit {
    pub target: Vec3,
    pub distance: f32,
    /// Rotation about the up axis, in radians.
    pub yaw: f32,
    /// Elevation above the horizon, in radians. Clamped short of the poles so
    /// the up vector never becomes degenerate.
    pub pitch: f32,
    pub fov_y: f32,
    pub near: f32,
    pub far: f32,
}

impl Default for Orbit {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            distance: 10.0,
            // Three-quarter view: more informative than face-on for judging
            // whether geometry is correct.
            yaw: -0.9,
            pitch: 0.35,
            fov_y: 50f32.to_radians(),
            near: 0.05,
            far: 5000.0,
        }
    }
}

impl Orbit {
    /// Frames a bounding box so the whole thing is visible.
    pub fn frame(min: Vec3, max: Vec3) -> Self {
        let target = (min + max) * 0.5;
        let radius = ((max - min).length() * 0.5).max(0.01);
        let mut camera = Self {
            target,
            ..Default::default()
        };
        // Back off far enough that the bounding sphere fits the vertical field
        // of view, with a small margin. The sphere rather than the box, because
        // the camera orbits and the box's silhouette changes as it does.
        camera.distance = radius / (camera.fov_y * 0.5).tan() * 1.08;
        camera.near = (radius * 0.01).max(0.01);
        camera.far = (camera.distance + radius * 4.0).max(10.0);
        camera
    }

    pub fn eye(&self) -> Vec3 {
        let (sp, cp) = self.pitch.sin_cos();
        let (sy, cy) = self.yaw.sin_cos();
        // Spherical coordinates about the Z axis.
        self.target
            + Vec3::new(cp * cy, cp * sy, sp) * self.distance
    }

    pub fn orbit(&mut self, delta_yaw: f32, delta_pitch: f32) {
        self.yaw += delta_yaw;
        const LIMIT: f32 = std::f32::consts::FRAC_PI_2 - 0.02;
        self.pitch = (self.pitch + delta_pitch).clamp(-LIMIT, LIMIT);
    }

    /// Multiplicative zoom, so each notch feels the same at any distance.
    pub fn zoom(&mut self, factor: f32) {
        self.distance = (self.distance * factor).clamp(0.05, 20_000.0);
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let view = glam::camera::rh::view::look_at_mat4(self.eye(), self.target, Vec3::Z);
        // wgpu's NDC is Z in 0..1 with Y up -- the DirectX/Metal convention.
        // glam's `vulkan` module is also Z 0..1 but Y *down*, which would flip
        // the image; `opengl` uses Z -1..1, which would clip wrongly.
        let proj = glam::camera::rh::proj::directx::perspective(
            self.fov_y,
            aspect.max(0.001),
            self.near,
            self.far,
        );
        proj * view
    }

    pub fn uniform(&self, aspect: f32) -> CameraUniform {
        let eye = self.eye();
        // A key light over the viewer's shoulder, so rotating the model
        // actually changes the shading.
        let light = (eye - self.target).normalize_or_zero() + Vec3::new(0.0, 0.0, 0.6);
        CameraUniform {
            view_proj: self.view_proj(aspect).to_cols_array_2d(),
            eye: [eye.x, eye.y, eye.z, 1.0],
            light: [light.x, light.y, light.z, 0.0],
            // Unlit by default. A camera knows where it is, not what hour it
            // is; the viewer overwrites these once it has a world clock and a
            // position to resolve a light for. See `CameraUniform::UNLIT`.
            sun: [0.0; 4],
            ambient: [0.0; 4],
            fog: [0.0; 4],
            fog_range: [0.0; 4],
            // A camera does not know where the sun is either, and a shadow
            // strength of zero is what tells the shaders not to ask.
            light_view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
            shadow: CameraUniform::NO_SHADOW,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Up is `+Z`; a camera at zero pitch must sit level with its target.
    #[test]
    fn zero_pitch_is_level() {
        let camera = Orbit {
            pitch: 0.0,
            distance: 5.0,
            ..Default::default()
        };
        assert!((camera.eye().z - camera.target.z).abs() < 1e-5);
    }

    #[test]
    fn positive_pitch_looks_down_from_above() {
        let camera = Orbit {
            pitch: 0.5,
            ..Default::default()
        };
        assert!(camera.eye().z > camera.target.z);
    }

    #[test]
    fn pitch_cannot_reach_the_pole() {
        let mut camera = Orbit::default();
        camera.orbit(0.0, 100.0);
        assert!(camera.pitch < std::f32::consts::FRAC_PI_2);
        camera.orbit(0.0, -200.0);
        assert!(camera.pitch > -std::f32::consts::FRAC_PI_2);
    }

    /// Framing must put the whole box inside the view frustum.
    #[test]
    fn framing_fits_the_bounds() {
        let (min, max) = (Vec3::new(-2.0, -3.0, 0.0), Vec3::new(3.0, 2.0, 4.0));
        let camera = Orbit::frame(min, max);
        assert_eq!(camera.target, (min + max) * 0.5);

        let radius = (max - min).length() * 0.5;
        assert!(camera.distance > radius, "camera is inside the model");
        assert!(camera.far > camera.distance);
        assert!(camera.near > 0.0 && camera.near < camera.distance);

        // Every corner must project inside the clip volume.
        let vp = camera.view_proj(16.0 / 9.0);
        for i in 0..8 {
            let corner = Vec3::new(
                if i & 1 == 0 { min.x } else { max.x },
                if i & 2 == 0 { min.y } else { max.y },
                if i & 4 == 0 { min.z } else { max.z },
            );
            let clip = vp * corner.extend(1.0);
            assert!(clip.w > 0.0, "corner {i} is behind the camera");
            let ndc = clip.truncate() / clip.w;
            assert!(
                ndc.x.abs() <= 1.0 && ndc.y.abs() <= 1.0 && (0.0..=1.0).contains(&ndc.z),
                "corner {i} falls outside the frustum at {ndc:?}"
            );
        }
    }

    #[test]
    fn zoom_is_multiplicative_and_bounded() {
        let mut camera = Orbit {
            distance: 10.0,
            ..Default::default()
        };
        camera.zoom(0.5);
        assert!((camera.distance - 5.0).abs() < 1e-5);
        for _ in 0..200 {
            camera.zoom(0.5);
        }
        assert!(camera.distance >= 0.05);
    }
}

/// A free-flying camera, for moving through a world rather than around an
/// object.
///
/// Shares the orbit camera's conventions: `+Z` up, and the same projection so
/// switching between them does not change what the scene looks like.
#[derive(Clone, Copy, Debug)]
pub struct Fly {
    pub position: Vec3,
    /// Heading, measured the same way as [`Orbit::yaw`].
    pub yaw: f32,
    pub pitch: f32,
    pub fov_y: f32,
    pub near: f32,
    pub far: f32,
    /// Units per second at normal speed.
    pub speed: f32,
}

impl Default for Fly {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            yaw: 0.0,
            pitch: -0.2,
            fov_y: 65f32.to_radians(),
            near: 0.5,
            // A continent tile is 533 units across, so the far plane has to
            // clear several of them to be useful.
            far: 12_000.0,
            speed: 60.0,
        }
    }
}

impl Fly {
    /// Places the camera to look at a bounding box from outside it, matching
    /// what the orbit camera would have shown.
    pub fn looking_at(min: Vec3, max: Vec3) -> Self {
        Self::from_orbit(&Orbit::frame(min, max))
    }

    /// Converts an orbit camera into a free one aimed the same way.
    ///
    /// Keeping the two convertible means a bearing means the same thing in
    /// either mode, so a screenshot angle is reproducible whichever is active.
    pub fn from_orbit(orbit: &Orbit) -> Self {
        let eye = orbit.eye();
        let to_target = orbit.target - eye;
        let mut camera = Self {
            position: eye,
            far: (orbit.far * 4.0).max(12_000.0),
            // Scale movement to the scene: crossing it should take seconds,
            // not minutes.
            speed: (to_target.length() * 0.35).clamp(5.0, 400.0),
            ..Default::default()
        };
        camera.yaw = to_target.y.atan2(to_target.x);
        camera.pitch = to_target.z.atan2(to_target.truncate().length());
        camera
    }

    pub fn forward(&self) -> Vec3 {
        let (sp, cp) = self.pitch.sin_cos();
        let (sy, cy) = self.yaw.sin_cos();
        Vec3::new(cp * cy, cp * sy, sp)
    }

    /// Right-hand side of the view, always level with the horizon so strafing
    /// never rolls the camera.
    pub fn right(&self) -> Vec3 {
        self.forward().cross(Vec3::Z).normalize_or_zero()
    }

    pub fn look(&mut self, delta_yaw: f32, delta_pitch: f32) {
        self.yaw += delta_yaw;
        const LIMIT: f32 = std::f32::consts::FRAC_PI_2 - 0.02;
        self.pitch = (self.pitch + delta_pitch).clamp(-LIMIT, LIMIT);
    }

    /// Moves by a direction expressed in camera-local axes: `x` right, `y`
    /// forward, `z` world-up.
    ///
    /// Vertical movement uses world up rather than the camera's, so looking
    /// down while ascending still goes up.
    pub fn travel(&mut self, local: Vec3, seconds: f32, fast: bool) {
        let multiplier = if fast { 6.0 } else { 1.0 };
        let step = self.speed * multiplier * seconds;
        self.position +=
            (self.right() * local.x + self.forward() * local.y + Vec3::Z * local.z) * step;
    }

    pub fn view_proj(&self, aspect: f32) -> glam::Mat4 {
        let view = glam::camera::rh::view::look_to_mat4(self.position, self.forward(), Vec3::Z);
        let proj = glam::camera::rh::proj::directx::perspective(
            self.fov_y,
            aspect.max(0.001),
            self.near,
            self.far,
        );
        proj * view
    }

    pub fn uniform(&self, aspect: f32) -> CameraUniform {
        // Key light behind the viewer, so surfaces face-on are brightest.
        let light = -self.forward() + Vec3::new(0.0, 0.0, 0.8);
        CameraUniform {
            view_proj: self.view_proj(aspect).to_cols_array_2d(),
            eye: [self.position.x, self.position.y, self.position.z, 1.0],
            light: [light.x, light.y, light.z, 0.0],
            // Unlit by default. A camera knows where it is, not what hour it
            // is; the viewer overwrites these once it has a world clock and a
            // position to resolve a light for. See `CameraUniform::UNLIT`.
            sun: [0.0; 4],
            ambient: [0.0; 4],
            fog: [0.0; 4],
            fog_range: [0.0; 4],
            // A camera does not know where the sun is either, and a shadow
            // strength of zero is what tells the shaders not to ask.
            light_view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
            shadow: CameraUniform::NO_SHADOW,
        }
    }
}

/// A world-space ray, as produced by [`Camera::ray_through`].
#[derive(Clone, Copy, Debug)]
pub struct Ray {
    pub origin: Vec3,
    /// Unit length.
    pub direction: Vec3,
}

impl Ray {
    /// How far along the ray it first enters an axis-aligned box, if it does.
    ///
    /// The slab test. Axis-aligned rather than oriented because the things
    /// being picked here are creatures, which rotate only about `Z`: a box
    /// whose horizontal extent is the model's widest lets that rotation happen
    /// inside it for free, and being a little generous about what counts as a
    /// hit is the right error for a target selector to make.
    ///
    /// The compact form of this test divides by the direction and lets an
    /// infinite slope stand in for "parallel to these planes". That is fine
    /// until the ray also *lies in* one of them, where the infinity meets a
    /// zero extent and `0 * inf` is `NaN` -- and a `NaN` fails every
    /// comparison it is in, so the box silently stops being clickable rather
    /// than erroring. An axis-aligned ray is the ordinary case for a camera
    /// looking along a world axis, so the parallel case is written out.
    pub fn hits_box(&self, min: Vec3, max: Vec3) -> Option<f32> {
        let mut entry = 0.0f32;
        let mut exit = f32::INFINITY;
        for axis in 0..3 {
            let direction = self.direction[axis];
            let (low, high) = (min[axis], max[axis]);
            if direction == 0.0 {
                // Parallel to this pair of planes: the ray is either between
                // them forever or misses the box outright.
                if self.origin[axis] < low || self.origin[axis] > high {
                    return None;
                }
                continue;
            }
            let first = (low - self.origin[axis]) / direction;
            let second = (high - self.origin[axis]) / direction;
            entry = entry.max(first.min(second));
            exit = exit.min(first.max(second));
        }
        // `entry` starts at zero, so a box behind the ray leaves `exit`
        // negative and is rejected here along with one the slabs never share.
        (exit >= entry).then_some(entry)
    }
}

/// Either camera, so the renderer does not care which is in use.
#[derive(Clone, Copy, Debug)]
pub enum Camera {
    Orbit(Orbit),
    Fly(Fly),
}

impl Camera {
    pub fn uniform(&self, aspect: f32) -> CameraUniform {
        match self {
            Self::Orbit(c) => c.uniform(aspect),
            Self::Fly(c) => c.uniform(aspect),
        }
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        match self {
            Self::Orbit(c) => c.view_proj(aspect),
            Self::Fly(c) => c.view_proj(aspect),
        }
    }

    pub fn eye(&self) -> Vec3 {
        match self {
            Self::Orbit(c) => c.eye(),
            Self::Fly(c) => c.position,
        }
    }

    /// Where the camera is looking, as a unit vector.
    ///
    /// Used to aim the shadow box ahead of the viewer rather than around
    /// them -- half a shadow map spent behind the player is half a shadow map
    /// nobody will ever see.
    pub fn forward(&self) -> Vec3 {
        match self {
            Self::Orbit(c) => (c.target - c.eye()).normalize_or_zero(),
            Self::Fly(c) => c.forward(),
        }
    }

    /// The camera's own right and up axes in world space -- what a billboard
    /// is widened along.
    ///
    /// Built from the same `look_to`/`look_at` the view matrix is, not from
    /// the yaw and pitch separately. A sprite widened along a basis that
    /// disagrees with the projection leans, and a leaning sprite reads as a
    /// bad texture rather than as a stale copy of an angle -- the same trap as
    /// a picking ray rebuilt from the camera instead of unprojected from the
    /// matrix the scene was drawn with.
    pub fn billboard_basis(&self) -> (Vec3, Vec3) {
        let view = match self {
            Self::Orbit(c) => {
                glam::camera::rh::view::look_at_mat4(c.eye(), c.target, Vec3::Z)
            }
            Self::Fly(c) => {
                glam::camera::rh::view::look_to_mat4(c.position, c.forward(), Vec3::Z)
            }
        };
        // The view matrix's rows are the camera's axes: it maps world space
        // into view space, so its transpose maps back.
        (view.row(0).truncate(), view.row(1).truncate())
    }

    /// Where a world point lands on screen, in the same coordinates
    /// [`Camera::ray_through`] takes.
    ///
    /// `None` when the point is behind the camera, where the perspective
    /// divide would otherwise fold it back into view -- a target standing
    /// behind you would be marked in front of you, mirrored.
    pub fn project(&self, point: Vec3, viewport: (f32, f32)) -> Option<(f32, f32)> {
        let (width, height) = viewport;
        if !(width > 0.0 && height > 0.0) {
            return None;
        }
        let clip = self.view_proj(width / height) * point.extend(1.0);
        if clip.w <= 0.0 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        Some((
            (ndc.x + 1.0) * 0.5 * width,
            (1.0 - ndc.y) * 0.5 * height,
        ))
    }

    /// A world-space ray through a point on the screen, given in pixels from
    /// the top-left.
    ///
    /// Unprojected from the same matrix the scene is drawn with rather than
    /// rebuilt from the camera's angles. The two would agree only as long as
    /// nobody changed the projection, and a picking ray that disagrees with the
    /// view by a little is far harder to notice than one that disagrees by a
    /// lot -- clicks land on the creature next to the one under the cursor.
    ///
    /// Returns `None` only if the view-projection cannot be inverted, which a
    /// degenerate viewport (zero width or height) produces.
    pub fn ray_through(&self, pixel: (f32, f32), viewport: (f32, f32)) -> Option<Ray> {
        let (width, height) = viewport;
        if !(width > 0.0 && height > 0.0) {
            return None;
        }
        let inverse = self.view_proj(width / height).inverse();
        if !inverse.is_finite() {
            return None;
        }

        // Clip space is x right, y *up*, and -- this being the DirectX-style
        // projection this project uses throughout -- depth from 0 at the near
        // plane to 1 at the far one, not -1 to 1.
        let x = 2.0 * pixel.0 / width - 1.0;
        let y = 1.0 - 2.0 * pixel.1 / height;
        let unproject = |depth: f32| {
            let point = inverse * glam::Vec4::new(x, y, depth, 1.0);
            point.truncate() / point.w
        };

        let near = unproject(0.0);
        let direction = (unproject(1.0) - near).normalize_or_zero();
        (direction != Vec3::ZERO).then_some(Ray {
            origin: near,
            direction,
        })
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Orbit(c) => format!(
                "orbit: yaw {:.0}\u{b0} pitch {:.0}\u{b0} distance {:.1}",
                c.yaw.to_degrees(),
                c.pitch.to_degrees(),
                c.distance
            ),
            Self::Fly(c) => format!(
                "fly: [{:.0} {:.0} {:.0}] yaw {:.0}\u{b0} pitch {:.0}\u{b0} {:.0} u/s",
                c.position.x,
                c.position.y,
                c.position.z,
                c.yaw.to_degrees(),
                c.pitch.to_degrees(),
                c.speed
            ),
        }
    }
}

#[cfg(test)]
mod fly_tests {
    use super::*;

    #[test]
    fn forward_is_level_at_zero_pitch() {
        let camera = Fly {
            pitch: 0.0,
            yaw: 0.0,
            ..Default::default()
        };
        assert!((camera.forward() - Vec3::X).length() < 1e-5);
    }

    /// Strafing must never introduce roll, however far the camera is pitched.
    #[test]
    fn right_stays_level_when_looking_down() {
        for pitch in [-1.4, -0.5, 0.0, 0.5, 1.4f32] {
            let camera = Fly {
                pitch,
                ..Default::default()
            };
            assert!(
                camera.right().z.abs() < 1e-5,
                "pitch {pitch} tilted the right vector"
            );
        }
    }

    /// Vertical travel follows world up, not the view direction, so ascending
    /// while looking down still gains height.
    #[test]
    fn vertical_travel_ignores_pitch() {
        let mut camera = Fly {
            pitch: -1.2,
            speed: 10.0,
            ..Default::default()
        };
        let before = camera.position;
        camera.travel(Vec3::Z, 1.0, false);
        assert!(camera.position.z > before.z + 9.0);
        assert!((camera.position.x - before.x).abs() < 1e-4);
    }

    #[test]
    fn fast_travel_is_a_multiple_of_normal() {
        let step = |fast| {
            let mut camera = Fly {
                speed: 10.0,
                yaw: 0.0,
                pitch: 0.0,
                ..Default::default()
            };
            camera.travel(Vec3::Y, 1.0, fast);
            camera.position.x
        };
        assert!((step(false) - 10.0).abs() < 1e-4);
        assert!(step(true) > step(false) * 5.0);
    }

    #[test]
    fn pitch_cannot_reach_the_pole() {
        let mut camera = Fly::default();
        camera.look(0.0, 100.0);
        assert!(camera.pitch < std::f32::consts::FRAC_PI_2);
    }

    /// Framing a box must put the camera outside it and looking at it.
    #[test]
    fn looking_at_a_box_points_towards_it() {
        let (min, max) = (Vec3::new(-100.0, -100.0, 0.0), Vec3::new(100.0, 100.0, 50.0));
        let camera = Fly::looking_at(min, max);
        let centre = (min + max) * 0.5;

        let to_centre = (centre - camera.position).normalize();
        assert!(
            camera.forward().dot(to_centre) > 0.99,
            "camera is not aimed at the box"
        );
        assert!(camera.speed > 0.0 && camera.far > (centre - camera.position).length());
    }
}

#[cfg(test)]
mod pick_tests {
    use super::*;

    const VIEWPORT: (f32, f32) = (1600.0, 900.0);

    fn camera() -> Camera {
        Camera::Fly(Fly {
            position: Vec3::new(-10.0, 4.0, 3.0),
            yaw: 0.0,
            pitch: 0.0,
            ..Fly::default()
        })
    }

    /// Picking is the projection run backwards, so this runs it forwards
    /// first: take a point in the world, find the pixel it is drawn at, and
    /// cast a ray back through that pixel. It has to arrive where it started.
    ///
    /// The failure this guards against is not a ray that misses wildly -- that
    /// is obvious the first time anything is clicked -- but one that is off by
    /// a consistent little, from a sign or a depth-range convention. That lands
    /// clicks on the creature beside the one under the cursor, which reads as
    /// the server disagreeing about positions rather than as a bad ray.
    #[test]
    fn a_ray_returns_to_the_point_it_was_projected_from() {
        let camera = camera();
        for target in [
            Vec3::new(20.0, 6.0, 2.0),
            Vec3::new(40.0, -8.0, 9.0),
            Vec3::new(12.0, 4.0, 3.0),
        ] {
            let pixel = camera.project(target, VIEWPORT).unwrap();
            let ray = camera.ray_through(pixel, VIEWPORT).unwrap();
            let tolerance = Vec3::splat(0.05);
            assert!(
                ray.hits_box(target - tolerance, target + tolerance).is_some(),
                "the ray through {pixel:?} missed {target}"
            );
        }
    }

    /// The centre of the screen is where the camera is looking.
    #[test]
    fn the_centre_pixel_looks_straight_ahead() {
        let camera = camera();
        let ray = camera
            .ray_through((VIEWPORT.0 / 2.0, VIEWPORT.1 / 2.0), VIEWPORT)
            .unwrap();
        let forward = match camera {
            Camera::Fly(c) => c.forward(),
            Camera::Orbit(_) => unreachable!(),
        };
        assert!(
            ray.direction.dot(forward) > 0.9999,
            "{:?} is not {forward}",
            ray.direction
        );
    }

    /// Something behind the viewer is not clickable, however well the ray's
    /// infinite line happens to pass through it.
    #[test]
    fn a_box_behind_the_camera_is_not_hit() {
        let ray = Ray {
            origin: Vec3::ZERO,
            direction: Vec3::X,
        };
        assert!(ray
            .hits_box(Vec3::new(-20.0, -1.0, -1.0), Vec3::new(-10.0, 1.0, 1.0))
            .is_none());
        assert_eq!(
            ray.hits_box(Vec3::new(10.0, -1.0, -1.0), Vec3::new(20.0, 1.0, 1.0)),
            Some(10.0),
            "and something in front is hit at its near face"
        );
    }

    /// A ray running exactly parallel to a face -- and lying in its plane --
    /// is the case the compact slab test turns into `NaN`, which fails every
    /// comparison it touches and quietly makes the box unclickable. A camera
    /// looking along a world axis produces it routinely.
    #[test]
    fn a_ray_lying_in_a_face_still_hits() {
        let ray = Ray {
            origin: Vec3::ZERO,
            direction: Vec3::X,
        };
        // Zero thickness in Z, with the ray exactly in that plane.
        assert_eq!(
            ray.hits_box(Vec3::new(5.0, -1.0, 0.0), Vec3::new(6.0, 1.0, 0.0)),
            Some(5.0)
        );
        // And parallel but outside still misses.
        assert!(ray
            .hits_box(Vec3::new(5.0, -1.0, 3.0), Vec3::new(6.0, 1.0, 4.0))
            .is_none());
    }

    /// A ray starting inside a box has already hit it -- the camera standing
    /// in a creature should not have to leave before it can be clicked.
    #[test]
    fn a_ray_starting_inside_hits_at_zero() {
        let ray = Ray {
            origin: Vec3::ZERO,
            direction: Vec3::X,
        };
        assert_eq!(
            ray.hits_box(Vec3::splat(-1.0), Vec3::splat(1.0)),
            Some(0.0)
        );
    }

    /// A zero-sized viewport is what a minimised window reports.
    #[test]
    fn a_degenerate_viewport_yields_no_ray() {
        assert!(camera().ray_through((0.0, 0.0), (0.0, 0.0)).is_none());
        assert!(camera().ray_through((0.0, 0.0), (800.0, 0.0)).is_none());
    }

    /// A point behind the camera must not project.
    ///
    /// The perspective divide is happy to fold it back into view -- a negative
    /// `w` flips both axes -- so something standing behind you would be marked
    /// in front of you, mirrored, and moving the wrong way as you turn. The
    /// sign has to be checked before the divide, not after.
    #[test]
    fn a_point_behind_the_camera_does_not_project() {
        let camera = camera();
        // The camera sits at x = -10 looking towards +x.
        assert!(camera.project(Vec3::new(-40.0, 4.0, 3.0), VIEWPORT).is_none());
        assert!(camera.project(Vec3::new(20.0, 4.0, 3.0), VIEWPORT).is_some());
    }
}
