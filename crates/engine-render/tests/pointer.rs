//! The agreement test M28's design promises: the cursor ray in
//! `engine-core` is the inverse of the projection the renderer actually draws
//! with.
//!
//! `engine_core::input::Pointer` writes the inverse of
//! `scene_renderer::view_projection` out longhand, because engine-core cannot
//! depend on engine-render. This crate is the only one that can see both, so
//! this is where the two spellings are held to each other — the `water.wgsl`
//! precedent, applied to a transform instead of a shader.
//!
//! No GPU: this is pure matrix arithmetic, so it runs everywhere, including
//! CI where the render tests skip.

use engine_core::components::Camera;
use engine_core::input::{InputState, Pointer, Viewport};
use engine_core::math::{Mat4, Quat, Vec2, Vec3};
use engine_render::scene_renderer::view_projection;

/// Project a world point back through the renderer's own matrix and report
/// where it lands as a cursor: `[0, 1]` across the frame, origin top-left.
fn project_to_cursor(point: Vec3, camera: &Camera, model: Mat4, viewport: &Viewport) -> Vec2 {
    let clip = view_projection(camera, model, viewport.aspect()) * point.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    Vec2::new((ndc.x + 1.0) * 0.5, (1.0 - ndc.y) * 0.5)
}

fn pointer_at(cursor: Vec2, camera: &Camera, model: Mat4, viewport: &Viewport) -> Pointer {
    let mut input = InputState::default();
    input.set_cursor(cursor);
    Pointer::resolve(&input, viewport, Some((*camera, model)))
}

#[test]
fn a_cursor_ray_projects_back_to_the_cursor_it_came_from() {
    let camera = Camera {
        fov: 52.0,
        near: 0.1,
        far: 500.0,
        active: true,
    };
    // An off-axis camera with a real rotation: an identity model matrix would
    // pass even if the ray ignored the camera's orientation entirely.
    let model = Mat4::from_rotation_translation(
        Quat::from_euler(glam::EulerRot::XYZ, -0.6, 0.9, 0.0),
        Vec3::new(-4.0, 12.0, 9.0),
    );
    // A wide frame, so an aspect handled as 1.0 anywhere shows up.
    let viewport = Viewport::new(1280, 540, None);

    for cursor in [
        Vec2::new(0.5, 0.5),
        Vec2::ZERO,
        Vec2::new(1.0, 0.0),
        Vec2::new(0.0, 1.0),
        Vec2::ONE,
        Vec2::new(0.17, 0.83),
    ] {
        let pointer = pointer_at(cursor, &camera, model, &viewport);
        let (origin, direction) = pointer.ray.expect("the camera is there");
        // Anywhere along the ray must project to the same cursor — that is
        // what makes it *the* ray through that pixel.
        for distance in [1.0, 7.5, 120.0] {
            let landed =
                project_to_cursor(origin + direction * distance, &camera, model, &viewport);
            assert!(
                (landed - cursor).length() < 1e-4,
                "cursor {cursor} at {distance} m came back as {landed}"
            );
        }
    }
}

#[test]
fn the_aspect_ratio_is_the_frames_and_not_a_guess() {
    let camera = Camera::default();
    let model = Mat4::from_translation(Vec3::new(0.0, 3.0, 0.0));
    let cursor = Vec2::new(0.9, 0.2);

    // The same cursor in two different frames is two different directions —
    // that is the resolution dependence M28 §5 documents rather than hides.
    let wide = pointer_at(cursor, &camera, model, &Viewport::new(1920, 540, None));
    let square = pointer_at(cursor, &camera, model, &Viewport::new(540, 540, None));
    let wide_dir = wide.ray.unwrap().1;
    let square_dir = square.ray.unwrap().1;
    assert!(
        (wide_dir - square_dir).length() > 0.05,
        "a wider frame must aim further out: {wide_dir} vs {square_dir}"
    );

    // And each is right for its own frame, by the renderer's own matrix.
    for (pointer, viewport) in [
        (wide, Viewport::new(1920, 540, None)),
        (square, Viewport::new(540, 540, None)),
    ] {
        let (origin, direction) = pointer.ray.unwrap();
        let landed = project_to_cursor(origin + direction * 10.0, &camera, model, &viewport);
        assert!((landed - cursor).length() < 1e-4, "{landed} for {viewport:?}");
    }
}

#[test]
fn the_ground_point_under_the_cursor_is_on_screen_where_the_cursor_is() {
    // The arena shooter's rig: a camera above and behind, tipped down. This
    // is the call a top-down game aims with, so it is worth pinning against
    // the projection rather than only against the plane arithmetic.
    let camera = Camera {
        fov: 46.0,
        ..Camera::default()
    };
    let model = Mat4::from_rotation_translation(
        Quat::from_euler(glam::EulerRot::XYZ, -60f32.to_radians(), 0.0, 0.0),
        Vec3::new(0.0, 20.0, 11.5),
    );
    let viewport = Viewport::new(960, 540, None);

    let cursor = Vec2::new(0.31, 0.66);
    let pointer = pointer_at(cursor, &camera, model, &viewport);
    let ground = pointer.ground(0.9).expect("the camera is there");
    assert!((ground.y - 0.9).abs() < 1e-4, "{ground}");
    let landed = project_to_cursor(ground, &camera, model, &viewport);
    assert!((landed - cursor).length() < 1e-4, "{landed} vs {cursor}");
}
