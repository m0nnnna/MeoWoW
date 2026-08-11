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
