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
        }
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
