//! The transform gizmos: translate, rotate, and scale, drawn as egui overlay
//! lines and dragged in the viewport. `W`/`R`/`S` switch the mode.
//!
//! All three modes work on world axes mapped straight to `Transform` field
//! components: the X arm moves `position[0]`, the X ring adds degrees to
//! `rotation[0]`, the X handle adds to `scale[0]`. That keeps a gizmo gesture
//! and the file edit it produces legible as the same thing — matching the
//! component-wise Euler convention the rest of the engine uses.
//!
//! The math half (this module) is pure and unit-tested: projecting axes and
//! rings to screen, hit-testing the pointer against them, and turning a
//! pointer ray into a parameter along the grabbed axis (a distance for
//! translate/scale, an angle for rotate). Continuous drags preview in
//! memory; the caller commits once on release (principle #3).

use engine_core::components::Transform;
use glam::{Mat4, Vec2, Vec3};

use crate::pick::ray_through;

pub const AXES: [(Vec3, egui::Color32); 3] = [
    (Vec3::X, egui::Color32::from_rgb(230, 70, 70)),
    (Vec3::Y, egui::Color32::from_rgb(90, 200, 90)),
    (Vec3::Z, egui::Color32::from_rgb(80, 130, 240)),
];

/// Pixel radius within which the pointer counts as touching a gizmo part.
const GRAB_DISTANCE: f32 = 10.0;

/// Segments per rotation ring; drawing and hit-testing share
/// [`ring_points`] so they agree on the shape.
pub const RING_SEGMENTS: usize = 48;

/// Which `Transform` field the gizmo edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GizmoMode {
    Translate,
    Rotate,
    Scale,
}

impl GizmoMode {
    /// The `Transform` field name this mode writes.
    pub fn field(self) -> &'static str {
        match self {
            GizmoMode::Translate => "position",
            GizmoMode::Rotate => "rotation",
            GizmoMode::Scale => "scale",
        }
    }
}

/// World length of the gizmo arms (and ring radius), scaled with camera
/// distance so the gizmo keeps a usable screen size.
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
        if d < GRAB_DISTANCE && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, i));
        }
    }
    best.map(|(_, i)| i)
}

/// The in-plane basis a ring around `axis` is measured against. One shared
/// definition so [`ring_points`] and [`ring_angle`] cannot disagree about
/// where angle zero is.
fn ring_basis(axis: Vec3) -> (Vec3, Vec3) {
    let u = axis.any_orthonormal_vector();
    (u, axis.cross(u))
}

/// The closed polyline of the rotation ring around `axis`, in world space.
/// First and last points coincide.
pub fn ring_points(origin: Vec3, axis: Vec3, radius: f32) -> Vec<Vec3> {
    let (u, v) = ring_basis(axis);
    (0..=RING_SEGMENTS)
        .map(|i| {
            let a = i as f32 / RING_SEGMENTS as f32 * std::f32::consts::TAU;
            origin + (u * a.cos() + v * a.sin()) * radius
        })
        .collect()
}

/// Which axis ring (index into [`AXES`]) the pointer is grabbing, if any.
pub fn hit_ring(
    view_projection: Mat4,
    size: Vec2,
    origin: Vec3,
    radius: f32,
    pointer: Vec2,
) -> Option<usize> {
    let mut best: Option<(f32, usize)> = None;
    for (i, (axis, _)) in AXES.iter().enumerate() {
        let screen: Vec<Option<Vec2>> = ring_points(origin, *axis, radius)
            .into_iter()
            .map(|p| project(view_projection, size, p))
            .collect();
        for pair in screen.windows(2) {
            let (Some(a), Some(b)) = (pair[0], pair[1]) else {
                continue;
            };
            let d = distance_to_segment(pointer, a, b);
            if d < GRAB_DISTANCE && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, i));
            }
        }
    }
    best.map(|(_, i)| i)
}

/// The pointer's angle in degrees around `axis` on the ring's plane — the
/// rotate analogue of [`axis_parameter`]. The drag delta is
/// `angle_delta(now, grab)`. `None` when the plane is edge-on to the view
/// ray and the intersection is unstable.
pub fn ring_angle(
    view_projection: Mat4,
    size: Vec2,
    origin: Vec3,
    axis: Vec3,
    pointer: Vec2,
) -> Option<f32> {
    let (ray_origin, ray_direction) = ray_through(view_projection, size, pointer);
    let denominator = axis.dot(ray_direction);
    if denominator.abs() < 1e-4 {
        return None;
    }
    let t = axis.dot(origin - ray_origin) / denominator;
    let hit = ray_origin + ray_direction * t - origin;
    let (u, v) = ring_basis(axis);
    Some(hit.dot(v).atan2(hit.dot(u)).to_degrees())
}

/// Shortest signed difference `a − b` in degrees, in `(-180, 180]`.
pub fn angle_delta(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    if d > 180.0 {
        d - 360.0
    } else {
        d
    }
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
    pub mode: GizmoMode,
    /// Index into [`AXES`], doubling as the component index of the edited
    /// field.
    pub axis: usize,
    /// Parameter at grab time: an axis distance (translate/scale, world
    /// units) or a ring angle (rotate, degrees).
    pub grab_t: f32,
    /// The entity's `Transform` as written in the file at grab time.
    pub start: Transform,
    /// Live scalar delta along the grabbed axis, same units as `grab_t`.
    pub delta: f32,
}

impl Drag {
    /// `start` with the delta applied to the grabbed component of the
    /// mode's field — what this frame previews and what release commits.
    pub fn preview_transform(&self) -> Transform {
        let mut t = self.start;
        match self.mode {
            GizmoMode::Translate => t.position[self.axis] += self.delta,
            GizmoMode::Rotate => t.rotation[self.axis] += self.delta,
            GizmoMode::Scale => t.scale[self.axis] += self.delta,
        }
        t
    }

    /// The full value of the mode's field to write on release.
    pub fn value(&self) -> Vec3 {
        let t = self.preview_transform();
        match self.mode {
            GizmoMode::Translate => t.position,
            GizmoMode::Rotate => t.rotation,
            GizmoMode::Scale => t.scale,
        }
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

    #[test]
    fn ring_angle_recovers_a_known_angle() {
        let (view_projection, size) = setup();
        let origin = Vec3::new(0.0, 1.0, 0.0);

        // Place a world point at a known angle on the Y ring (the ground
        // plane — well off edge-on for the default camera), project it,
        // and recover the angle from its screen position.
        for expected in [0.0f32, 30.0, 130.0, -75.0] {
            let (u, v) = ring_basis(Vec3::Y);
            let radians = expected.to_radians();
            let world = origin + (u * radians.cos() + v * radians.sin()) * 2.0;
            let screen = project(view_projection, size, world).unwrap();
            let angle = ring_angle(view_projection, size, origin, Vec3::Y, screen).unwrap();
            assert!(
                angle_delta(angle, expected).abs() < 0.1,
                "expected {expected}, got {angle}"
            );
        }
    }

    #[test]
    fn angle_delta_takes_the_short_way_around() {
        assert!((angle_delta(30.0, 10.0) - 20.0).abs() < 1e-4);
        assert!((angle_delta(10.0, 30.0) + 20.0).abs() < 1e-4);
        // Crossing the ±180 seam: 170° → -170° is +20°, not -340°.
        assert!((angle_delta(-170.0, 170.0) - 20.0).abs() < 1e-4);
        assert!((angle_delta(170.0, -170.0) + 20.0).abs() < 1e-4);
    }

    #[test]
    fn hit_ring_finds_the_ring_under_the_pointer() {
        let (view_projection, size) = setup();
        let origin = Vec3::new(0.0, 1.0, 0.0);
        let radius = 2.0;

        // A point on the Y ring must hit it — probed at 45° around the
        // ring, away from where the X and Z rings cross it. A far corner
        // must miss everything.
        let (u, v) = ring_basis(Vec3::Y);
        let on_ring = project(view_projection, size, origin + (u + v) * radius * 0.7071).unwrap();
        assert_eq!(
            hit_ring(view_projection, size, origin, radius, on_ring),
            Some(1)
        );

        let nowhere = Vec2::new(40.0, 40.0);
        assert_eq!(
            hit_ring(view_projection, size, origin, radius, nowhere),
            None
        );
    }

    #[test]
    fn drags_apply_their_delta_to_the_grabbed_component() {
        let start = Transform {
            position: Vec3::new(1.0, 2.0, 3.0),
            rotation: Vec3::new(0.0, 45.0, 0.0),
            scale: Vec3::ONE,
        };
        let drag = |mode, axis, delta| Drag {
            entity: "e".into(),
            mode,
            axis,
            grab_t: 0.0,
            start,
            delta,
        };

        let moved = drag(GizmoMode::Translate, 0, 2.5);
        assert_eq!(moved.preview_transform().position, Vec3::new(3.5, 2.0, 3.0));
        assert_eq!(moved.value(), Vec3::new(3.5, 2.0, 3.0));

        let rotated = drag(GizmoMode::Rotate, 1, -90.0);
        assert_eq!(rotated.value(), Vec3::new(0.0, -45.0, 0.0));
        assert_eq!(rotated.preview_transform().position, start.position);

        let scaled = drag(GizmoMode::Scale, 2, 0.5);
        assert_eq!(scaled.value(), Vec3::new(1.0, 1.0, 1.5));
    }
}
