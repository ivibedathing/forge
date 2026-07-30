//! Pixel-level tests for M13 particle billboards: hand-built
//! `ParticleInstance` lists rendered through the real screenshot path.
//!
//! Constructing instances directly (rather than stepping an emitter) keeps
//! these tests about the *renderer*: quad expansion, soft-disc falloff,
//! alpha blending, and back-to-front ordering. The simulation side is pinned
//! GPU-free in `engine-core/src/particles.rs`.
//!
//! Like the other render tests, every test skips cleanly without a GPU.

use engine_core::components::ParticleBlend;
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
        &scene.water_items(),
        &scene.road_items(),
        particles,
        &camera,
        camera_transform.matrix(),
        scene.lights().resolved(),
        scene.environment,
        0.0,
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
        velocity: Vec3::ZERO,
        stretch: 0.0,
        blend: ParticleBlend::Alpha,
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
        alpha: 0.0,
        ..particle(Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0))
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

// ── M17: additive blending and velocity stretching ──────────────────────────

#[test]
fn additive_particles_only_ever_brighten() {
    if !gpu_available() {
        return;
    }
    // The defining property of the additive path, and the reason fire uses it:
    // an additive sprite adds light to what is behind it, so no pixel it
    // touches can come out darker than it went in. An alpha-blended dark sprite
    // would darken the same pixels.
    let empty = render(&[]);
    let lit = render(&[ParticleInstance {
        blend: ParticleBlend::Additive,
        ..particle(Vec3::ZERO, Vec3::new(0.5, 0.25, 0.05))
    }]);

    let mut brightened = 0usize;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let before = empty.pixel(x, y);
            let after = lit.pixel(x, y);
            for channel in 0..3 {
                assert!(
                    after[channel] >= before[channel],
                    "additive blending darkened ({x}, {y}) channel {channel}: \
                     {} -> {}",
                    before[channel],
                    after[channel]
                );
            }
            if after[..3] != before[..3] {
                brightened += 1;
            }
        }
    }
    assert!(
        brightened > 100,
        "the sprite should have brightened a disc of pixels, only {brightened} changed"
    );
}

#[test]
fn stacked_additive_particles_climb_toward_white() {
    if !gpu_available() {
        return;
    }
    // Why fire is additive rather than "orange smoke": overlapping flame gets
    // *hotter*. Four dim orange sprites on the same spot must sum brighter than
    // one — under alpha blending they would converge on the sprite's own color
    // instead, however many you stack.
    let dim = ParticleInstance {
        alpha: 0.3,
        blend: ParticleBlend::Additive,
        ..particle(Vec3::ZERO, Vec3::new(0.6, 0.3, 0.08))
    };
    let one = render(&[dim]);
    let four = render(&[dim, dim, dim, dim]);

    let single = one.pixel(CENTER.0, CENTER.1);
    let stacked = four.pixel(CENTER.0, CENTER.1);
    assert!(
        stacked[0] > single[0] && stacked[1] > single[1],
        "stacking additive sprites must accumulate: {single:?} -> {stacked:?}"
    );
}

#[test]
fn additive_particles_draw_after_alpha_blended_ones() {
    if !gpu_available() {
        return;
    }
    // Documented ordering: alpha first, additive after, regardless of depth —
    // a flame glows through the smoke above it. The dark alpha sprite is nearer
    // the camera, so a purely back-to-front pass would let it hide the flame;
    // the split pass must let the flame through anyway.
    let image = render(&[
        ParticleInstance {
            blend: ParticleBlend::Additive,
            ..particle(Vec3::new(0.0, 0.0, -0.5), Vec3::new(1.0, 0.5, 0.1))
        },
        ParticleInstance {
            alpha: 0.9,
            ..particle(Vec3::new(0.0, 0.0, 0.5), Vec3::new(0.02, 0.02, 0.02))
        },
    ]);
    let [r, ..] = image.pixel(CENTER.0, CENTER.1);
    assert!(
        r > 120,
        "the additive sprite must survive the alpha sprite in front of it, got r={r}"
    );
}

#[test]
fn stretching_elongates_a_sprite_along_its_velocity() {
    if !gpu_available() {
        return;
    }
    // A stretched sprite covers more of its direction of travel and no more
    // across it. The velocity is world +Y and the camera looks down -Z, so
    // "along" is screen-vertical and "across" is screen-horizontal.
    // A small sprite, so there is frame left to grow into: at size 1.0 the disc
    // already spans the whole 256-pixel view.
    let small = ParticleInstance {
        size: 0.2,
        ..particle(Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0))
    };
    let round = render(&[small]);
    let streak = render(&[ParticleInstance {
        velocity: Vec3::new(0.0, 4.0, 0.0),
        stretch: 0.15,
        ..small
    }]);

    // Measured against the empty frame, not an absolute threshold: the
    // background here is the renderer's clear color, which is not black.
    let background = render(&[]);
    let lit = |image: &Image, along: bool| -> usize {
        (0..SIZE)
            .filter(|&i| {
                let (x, y) = if along {
                    (CENTER.0, i)
                } else {
                    (i, CENTER.1)
                };
                image.pixel(x, y)[0] as i32 - background.pixel(x, y)[0] as i32 > 4
            })
            .count()
    };

    assert!(
        lit(&streak, true) > lit(&round, true) + 20,
        "stretching must lengthen the sprite along its velocity: {} -> {}",
        lit(&round, true),
        lit(&streak, true)
    );
    assert_eq!(
        lit(&streak, false),
        lit(&round, false),
        "stretching must not widen the sprite across its velocity"
    );
}

#[test]
fn zero_stretch_is_identical_to_no_velocity_data() {
    if !gpu_available() {
        return;
    }
    // The bit-exactness pin for the M13 sprites: a fast-moving particle that
    // never asked to be stretched must render byte for byte as it did before
    // the stretch path existed, whatever its velocity happens to be.
    let still = render(&[particle(Vec3::ZERO, Vec3::new(0.8, 0.4, 0.1))]);
    let moving = render(&[ParticleInstance {
        velocity: Vec3::new(3.0, -7.0, 2.0),
        stretch: 0.0,
        ..particle(Vec3::ZERO, Vec3::new(0.8, 0.4, 0.1))
    }]);
    assert_eq!(
        still.pixels, moving.pixels,
        "velocity must not affect an unstretched sprite"
    );
}
