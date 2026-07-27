//! The translate gizmo: three world-axis arrows, drawn as egui overlay
//! lines, dragged along their axis.
//!
//! The math half (this module) is pure and unit-tested: projecting axes to
//! screen, hit-testing the pointer against them, and turning a pointer ray
//! into a distance along the grabbed axis. Continuous drags preview in
//! memory; the caller commits once on release (principle #3).

use glam::{Mat4, Vec2, Vec3};

use crate::pick::ray_through;

pub const AXES: [(Vec3, egui::Color32); 3] = [
    (Vec3::X, egui::Color32::from_rgb(230, 70, 70)),
    (Vec3::Y, egui::Color32::from_rgb(90, 200, 90)),
    (Vec3::Z, egui::Color32::from_rgb(80, 130, 240)),
];

/// World length of the gizmo arms, scaled with camera distance so the gizmo
/// keeps a usable screen size.
pub fn arm_length(origin: Vec3, eye: Vec3) -> f32 {
    (origin - eye).length() * 0.18
}

/// Project a world point into viewport pixels; `None` when behind the eye.
pub fn project(view_projection: Mat4, size: Vec2, world: Vec3) -> Option<Vec2> {
    let clip = view_projection * world.extend(1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    Some(Vec2::new(
        (ndc.x + 1.0) * 0.5 * size.x,
        (1.0 - ndc.y) * 0.5 * size.y,
    ))
}

/// Distance in pixels from `point` to the segment `a`–`b`.
pub fn distance_to_segment(point: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = ((point - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0);
    (a + ab * t - point).length()
}

/// Which axis (index into [`AXES`]) the pointer is grabbing, if any.
pub fn hit_axis(
    view_projection: Mat4,
    size: Vec2,
    origin: Vec3,
    arm: f32,
    pointer: Vec2,
) -> Option<usize> {
    let root = project(view_projection, size, origin)?;
    let mut best: Option<(f32, usize)> = None;
    for (i, (axis, _)) in AXES.iter().enumerate() {
        let Some(tip) = project(view_projection, size, origin + *axis * arm) else {
            continue;
        };
        let d = distance_to_segment(pointer, root, tip);
        if d < 10.0 && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, i));
        }
    }
    best.map(|(_, i)| i)
}

/// The parameter `t` along `axis` through `origin` closest to the pointer's
/// pick ray — the core of axis-constrained dragging. The drag delta is
/// `t_now - t_grab`, in world units.
pub fn axis_parameter(
    view_projection: Mat4,
    size: Vec2,
    origin: Vec3,
    axis: Vec3,
    pointer: Vec2,
) -> f32 {
    let (ray_origin, ray_direction) = ray_through(view_projection, size, pointer);

    // Closest points of two lines: origin + axis*t and ray_origin + dir*s.
    let w = origin - ray_origin;
    let a = axis.dot(axis);
    let b = axis.dot(ray_direction);
    let c = ray_direction.dot(ray_direction);
    let d = axis.dot(w);
    let e = ray_direction.dot(w);
    let denominator = a * c - b * b;
    if denominator.abs() < 1e-8 {
        return 0.0; // Axis parallel to the view ray; no useful parameter.
    }
    (b * e - c * d) / denominator
}

/// A drag in progress.
pub struct Drag {
    pub entity: String,
    pub axis: usize,
    /// Axis parameter at grab time.
    pub grab_t: f32,
    /// The entity's `Transform.position` as written in the file at grab time.
    pub start_position: Vec3,
    /// Live preview offset, world units.
    pub delta: Vec3,
}

impl Drag {
    pub fn preview_position(&self) -> Vec3 {
        self.start_position + self.delta
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::OrbitCamera;

    fn setup() -> (Mat4, Vec2) {
        let camera = OrbitCamera::default();
        let view_projection = engine_render::scene_renderer::view_projection(
            &camera.component(),
            camera.model(),
            16.0 / 9.0,
        );
        (view_projection, Vec2::new(1600.0, 900.0))
    }

    #[test]
    fn projecting_the_camera_target_lands_mid_viewport() {
        let (view_projection, size) = setup();
        let p = project(view_projection, size, Vec3::new(0.0, 1.0, 0.0)).unwrap();
        assert!((p - size * 0.5).length() < 1.0, "{p:?}");
    }

    #[test]
    fn dragging_along_x_recovers_the_x_offset() {
        let (view_projection, size) = setup();
        let origin = Vec3::new(0.0, 1.0, 0.0);

        // Project two known world points on the X axis; the axis parameter
        // recovered from their screen positions must match their offsets.
        let t0 = axis_parameter(
            view_projection,
            size,
            origin,
            Vec3::X,
            project(view_projection, size, origin).unwrap(),
        );
        let t3 = axis_parameter(
            view_projection,
            size,
            origin,
            Vec3::X,
            project(view_projection, size, origin + Vec3::X * 3.0).unwrap(),
        );
        assert!((t0 - 0.0).abs() < 1e-3, "{t0}");
        assert!((t3 - 3.0).abs() < 1e-3, "{t3}");
    }

    #[test]
    fn hit_axis_finds_the_arm_under_the_pointer() {
        let (view_projection, size) = setup();
        let origin = Vec3::new(0.0, 1.0, 0.0);
        let arm = 2.0;

        let on_x = project(view_projection, size, origin + Vec3::X * arm * 0.7).unwrap();
        assert_eq!(hit_axis(view_projection, size, origin, arm, on_x), Some(0));

        let on_y = project(view_projection, size, origin + Vec3::Y * arm * 0.7).unwrap();
        assert_eq!(hit_axis(view_projection, size, origin, arm, on_y), Some(1));

        let nowhere = Vec2::new(40.0, 40.0);
        assert_eq!(hit_axis(view_projection, size, origin, arm, nowhere), None);
    }
}
