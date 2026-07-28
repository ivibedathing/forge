//! Pixel-level tests for M13 particle billboards: hand-built
//! `ParticleInstance` lists rendered through the real screenshot path.
//!
//! Constructing instances directly (rather than stepping an emitter) keeps
//! these tests about the *renderer*: quad expansion, soft-disc falloff,
//! alpha blending, and back-to-front ordering. The simulation side is pinned
//! GPU-free in `engine-core/src/particles.rs`.
//!
//! Like the other render tests, every test skips cleanly without a GPU.

use engine_core::math::Vec3;
use engine_core::mesh::BuiltinAssets;
use engine_core::particles::ParticleInstance;
use engine_core::Scene;
use engine_render::offscreen::{self, Image};
use engine_render::Gpu;

const SIZE: u32 = 256;
const CENTER: (u32, u32) = (SIZE / 2, SIZE / 2);

fn gpu_available() -> bool {
    let available = pollster::block_on(Gpu::new(Gpu::default_instance(), None)).is_ok();
    if !available {
        eprintln!("skipping: no usable GPU on this machine");
    }
    available
}

/// Render a particle list over an empty scene: camera 4 units up the +Z
/// axis looking at the origin, no meshes, lighting irrelevant (billboards
/// are unlit).
fn render(particles: &[ParticleInstance]) -> Image {
    let scene = Scene::from_source(
        r#"{"name": "particles", "entities": [
            {"name": "Off", "components": [{"type": "AmbientLight", "intensity": 0.0}]},
            {"name": "Eye", "components": [
                {"type": "Transform", "position": [0.0, 0.0, 4.0]},
                {"type": "Camera", "active": true}
            ]}
        ]}"#,
        "test.json",
    )
    .expect("test scene should be valid");
    let (camera, camera_transform) = scene.camera(None).expect("test scene needs a camera");
    let items = scene
        .render_items(&BuiltinAssets)
        .expect("no assets to fail on");
    offscreen::render(
        &items,
        particles,
        &camera,
        camera_transform.matrix(),
        scene.lights().resolved(),
        SIZE,
        SIZE,
        &scene.hud_items(),
        &[],
    )
    .expect("offscreen render failed")
}

fn particle(position: Vec3, color: Vec3) -> ParticleInstance {
    ParticleInstance {
        position,
        size: 1.0,
        color,
        alpha: 1.0,
    }
}

#[test]
fn billboard_draws_a_soft_disc_over_the_background() {
    if !gpu_available() {
        return;
    }
    let background = render(&[]);
    let image = render(&[particle(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0))]);

    // At the center the falloff is ~1 (the pixel grid sits half a pixel off
    // the sprite center, so a sliver of background survives the blend) and
    // the pixel is dominated by the particle's own linear color.
    let [r, g, b, _] = image.pixel(CENTER.0, CENTER.1);
    assert!(
        r >= 250 && g <= 12 && b <= 12,
        "sprite center should be nearly pure red, got {:?}",
        [r, g, b]
    );

    // The soft edge: red falls off monotonically-ish moving out from the
    // center — a ring sample partway out is dimmer than the center but
    // still redder than the background.
    let partway = image.pixel(CENTER.0 + 30, CENTER.1);
    let clear = background.pixel(CENTER.0 + 30, CENTER.1);
    assert!(
        partway[0] < r && partway[0] > clear[0],
        "the disc should fade, not cut off: center {r}, partway {}, background {}",
        partway[0],
        clear[0]
    );

    // Far from the sprite the frame is untouched background.
    assert_eq!(
        image.pixel(10, 10),
        background.pixel(10, 10),
        "pixels outside the quad must be background"
    );
}

#[test]
fn alpha_zero_particles_change_nothing() {
    if !gpu_available() {
        return;
    }
    let empty = render(&[]);
    let invisible = render(&[ParticleInstance {
        position: Vec3::ZERO,
        size: 1.0,
        color: Vec3::new(1.0, 1.0, 1.0),
        alpha: 0.0,
    }]);
    // A fully faded particle still draws its quad, but blends to nothing —
    // byte-identical output is what keeps end-of-life particles invisible
    // rather than faintly gray.
    assert_eq!(empty.pixels, invisible.pixels, "alpha 0 must be a no-op");
}

#[test]
fn nearer_particles_draw_over_farther_ones() {
    if !gpu_available() {
        return;
    }
    // Red sits nearer the camera (+Z toward the eye at z=4). Listing it
    // FIRST proves the renderer sorts by camera distance rather than
    // trusting submission order — back-to-front, green first, red on top.
    let image = render(&[
        particle(Vec3::new(0.0, 0.0, 1.0), Vec3::new(1.0, 0.0, 0.0)),
        particle(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0)),
    ]);

    // Red on top at ~0.98 alpha: red saturates, and only the blend's sliver
    // of the green underneath survives. The wrong order would invert this —
    // a green pixel with a sliver of red.
    let [r, g, _, _] = image.pixel(CENTER.0, CENTER.1);
    assert!(
        r >= 250 && g <= 60,
        "the nearer red sprite must win the center pixel, got r={r} g={g}"
    );
}
