//! Ray picking: from a viewport pixel to an entity name.
//!
//! Pure math over `RenderItem`s — no GPU picking pass, no ID buffer. Scenes
//! at this scale are a handful of meshes; a CPU ray-vs-AABB test is exact
//! enough for selection and costs nothing to keep correct.

use engine_core::scene::RenderItem;
use glam::{Mat4, Vec2, Vec3};

/// A world-space ray from the camera through a viewport pixel.
///
/// `pixel` and `size` are in the viewport's own coordinates (origin
/// top-left). Uses wgpu's 0..1 clip depth: near plane at z=0, far at z=1.
pub fn ray_through(view_projection: Mat4, size: Vec2, pixel: Vec2) -> (Vec3, Vec3) {
    let ndc = Vec2::new(
        pixel.x / size.x * 2.0 - 1.0,
        1.0 - pixel.y / size.y * 2.0,
    );
    let inverse = view_projection.inverse();
    let near = inverse.project_point3(Vec3::new(ndc.x, ndc.y, 0.0));
    let far = inverse.project_point3(Vec3::new(ndc.x, ndc.y, 1.0));
    (near, (far - near).normalize())
}

/// The entity whose mesh the ray hits first, if any.
pub fn pick<'a>(items: &'a [RenderItem], origin: Vec3, direction: Vec3) -> Option<&'a str> {
    let mut best: Option<(f32, &str)> = None;
    for item in items {
        if let Some(t) = ray_aabb(origin, direction, world_aabb(item)) {
            if best.is_none_or(|(bt, _)| t < bt) {
                best = Some((t, &item.entity));
            }
        }
    }
    best.map(|(_, name)| name)
}

/// World-space AABB of an item: local AABB corners through the model matrix.
pub fn world_aabb(item: &RenderItem) -> (Vec3, Vec3) {
    let mut local_min = Vec3::splat(f32::MAX);
    let mut local_max = Vec3::splat(f32::MIN);
    for p in &item.mesh.positions {
        let p = Vec3::from_array(*p);
        local_min = local_min.min(p);
        local_max = local_max.max(p);
    }

    let mut world_min = Vec3::splat(f32::MAX);
    let mut world_max = Vec3::splat(f32::MIN);
    for i in 0..8 {
        let corner = Vec3::new(
            if i & 1 == 0 { local_min.x } else { local_max.x },
            if i & 2 == 0 { local_min.y } else { local_max.y },
            if i & 4 == 0 { local_min.z } else { local_max.z },
        );
        let world = item.model.transform_point3(corner);
        world_min = world_min.min(world);
        world_max = world_max.max(world);
    }
    (world_min, world_max)
}

/// Slab test; returns the entry distance when the ray hits.
fn ray_aabb(origin: Vec3, direction: Vec3, (min, max): (Vec3, Vec3)) -> Option<f32> {
    let mut t_enter = 0.0f32;
    let mut t_exit = f32::MAX;
    for axis in 0..3 {
        let (o, d, lo, hi) = (origin[axis], direction[axis], min[axis], max[axis]);
        if d.abs() < 1e-8 {
            if o < lo || o > hi {
                return None;
            }
            continue;
        }
        let (t0, t1) = ((lo - o) / d, (hi - o) / d);
        let (t0, t1) = (t0.min(t1), t0.max(t1));
        t_enter = t_enter.max(t0);
        t_exit = t_exit.min(t1);
        if t_enter > t_exit {
            return None;
        }
    }
    Some(t_enter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::components::Material;
    use engine_core::mesh::BuiltinMesh;

    fn cube_at(name: &str, position: Vec3) -> RenderItem {
        RenderItem {
            entity: name.to_string(),
            mesh: std::sync::Arc::new(BuiltinMesh::Cube.data()),
            model: Mat4::from_translation(position),
            material: Material::default(),
            textures: Default::default(),
            terrain: None,
            joints: Vec::new(),
        }
    }

    #[test]
    fn picks_the_nearest_of_two_cubes() {
        let items = vec![
            cube_at("Far", Vec3::new(0.0, 0.0, -10.0)),
            cube_at("Near", Vec3::new(0.0, 0.0, -5.0)),
        ];
        // Ray from origin straight down -Z.
        let hit = pick(&items, Vec3::ZERO, Vec3::NEG_Z);
        assert_eq!(hit, Some("Near"));
    }

    #[test]
    fn misses_everything_off_to_the_side() {
        let items = vec![cube_at("Cube", Vec3::new(0.0, 0.0, -5.0))];
        assert_eq!(pick(&items, Vec3::ZERO, Vec3::Y), None);
    }

    #[test]
    fn ray_through_center_of_viewport_goes_forward() {
        let camera = crate::camera::OrbitCamera::default();
        let view_projection = engine_render::scene_renderer::view_projection(
            &camera.component(),
            camera.model(),
            16.0 / 9.0,
        );
        let (origin, direction) = ray_through(
            view_projection,
            Vec2::new(1600.0, 900.0),
            Vec2::new(800.0, 450.0),
        );
        let forward = camera.rotation() * Vec3::NEG_Z;
        assert!((direction - forward).length() < 1e-3, "{direction:?} vs {forward:?}");
        assert!((origin - camera.eye()).length() < 0.2, "origin near the eye");
    }
}
