//! The editor's orbit camera — editor state, never scene data.

use engine_core::components::Camera;
use glam::{Mat4, Quat, Vec2, Vec3};

pub struct OrbitCamera {
    pub target: Vec3,
    /// Radians about world +Y.
    pub yaw: f32,
    /// Radians about camera-local X; negative looks down.
    pub pitch: f32,
    pub distance: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            target: Vec3::new(0.0, 1.0, 0.0),
            yaw: 0.0,
            pitch: -0.35,
            distance: 10.0,
        }
    }
}

impl OrbitCamera {
    pub fn rotation(&self) -> Quat {
        Quat::from_rotation_y(self.yaw) * Quat::from_rotation_x(self.pitch)
    }

    /// Camera pose (camera-to-world), looking down local −Z at the target —
    /// the same convention scene cameras use, so `view_projection` is shared.
    pub fn model(&self) -> Mat4 {
        let rotation = self.rotation();
        let eye = self.target - rotation * Vec3::NEG_Z * self.distance;
        Mat4::from_rotation_translation(rotation, eye)
    }

    pub fn eye(&self) -> Vec3 {
        self.model().w_axis.truncate()
    }

    /// The Camera component equivalent, for the shared projection math.
    pub fn component(&self) -> Camera {
        Camera {
            fov: 60.0,
            near: 0.1,
            far: 1000.0,
            active: true,
        }
    }

    pub fn orbit(&mut self, delta: Vec2) {
        self.yaw -= delta.x * 0.008;
        self.pitch = (self.pitch - delta.y * 0.008).clamp(
            -std::f32::consts::FRAC_PI_2 + 0.01,
            std::f32::consts::FRAC_PI_2 - 0.01,
        );
    }

    pub fn pan(&mut self, delta: Vec2) {
        let rotation = self.rotation();
        let scale = self.distance * 0.0015;
        self.target += rotation * Vec3::X * -delta.x * scale;
        self.target += rotation * Vec3::Y * delta.y * scale;
    }

    pub fn zoom(&mut self, scroll: f32) {
        self.distance = (self.distance * (1.0 - scroll * 0.001)).clamp(0.5, 500.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_camera_looks_at_the_target() {
        let camera = OrbitCamera::default();
        let forward = camera.rotation() * Vec3::NEG_Z;
        let to_target = (camera.target - camera.eye()).normalize();
        assert!((forward - to_target).length() < 1e-5);
    }

    #[test]
    fn zoom_never_reaches_zero() {
        let mut camera = OrbitCamera::default();
        for _ in 0..100 {
            camera.zoom(10_000.0);
        }
        assert!(camera.distance >= 0.5);
    }
}
