//! Pixel-level tests for M4 lighting: render small scenes offscreen and
//! assert on what came back.
//!
//! Like `headless_render.rs`, every test skips cleanly when the machine has
//! no usable GPU. Expectations are computed through `srgb_encode`, never
//! eyeballed — the render target is sRGB (an M4 decision), so a linear scene
//! value and its PNG byte differ by exactly that curve.

use engine_core::mesh::BuiltinAssets;
use engine_core::Scene;
use engine_render::offscreen::{self, Image};
use engine_render::Gpu;

const SIZE: u32 = 256;

fn gpu_available() -> bool {
    let available = pollster::block_on(Gpu::new(Gpu::default_instance(), None)).is_ok();
    if !available {
        eprintln!("skipping: no usable GPU on this machine");
    }
    available
}

/// Load a scene from source and render it through the real screenshot path.
fn render(source: &str) -> Image {
    let scene = Scene::from_source(source, "test.json").expect("test scene should be valid");
    let (camera, camera_transform) = scene.camera(None).expect("test scene needs a camera");
    let items = scene
        .render_items(&BuiltinAssets)
        .expect("test scenes use builtins only");
    offscreen::render(
        &items,
        &scene.water_items(),
        &scene.road_items(),
        &[],
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

/// The sRGB transfer function, mapping a linear value to its encoded byte —
/// the exact curve `Rgba8UnormSrgb` applies on write.
fn srgb_encode(linear: f32) -> u8 {
    let clamped = linear.clamp(0.0, 1.0);
    let encoded = if clamped <= 0.003_130_8 {
        clamped * 12.92
    } else {
        1.055 * clamped.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}

/// Perceived brightness of a pixel, for ordering assertions.
fn luma(pixel: [u8; 4]) -> u32 {
    pixel[0] as u32 + pixel[1] as u32 + pixel[2] as u32
}

/// A cube yawed 45° so the camera sees two faces, one toward +X+Z and one
/// toward -X+Z. `sun_yaw: 45` lights the right face head-on and leaves the
/// left face grazing; `-45` is the mirror image.
fn two_face_scene(sun_yaw: f32) -> String {
    format!(
        r#"{{"name": "two_faces", "entities": [
            {{"name": "Cube", "components": [
                {{"type": "Transform", "rotation": [0.0, 45.0, 0.0]}},
                {{"type": "Mesh", "asset": "builtin:cube"}},
                {{"type": "Material", "albedo": [0.8, 0.8, 0.8], "roughness": 0.9}}
            ]}},
            {{"name": "Sun", "components": [
                {{"type": "Transform", "rotation": [0.0, {sun_yaw}, 0.0]}},
                {{"type": "DirectionalLight"}}
            ]}},
            {{"name": "Fill", "components": [{{"type": "AmbientLight", "intensity": 0.05}}]}},
            {{"name": "Eye", "components": [
                {{"type": "Transform", "position": [0.0, 0.0, 4.0]}},
                {{"type": "Camera", "active": true}}
            ]}}
        ]}}"#
    )
}

// Sample points that land on the cube's left and right faces: the yawed unit
// cube's silhouette spans ±0.707 world units, ≈±39 px at this camera.
const LEFT: (u32, u32) = (SIZE / 2 - 20, SIZE / 2);
const RIGHT: (u32, u32) = (SIZE / 2 + 20, SIZE / 2);

#[test]
fn sun_lights_the_facing_side_and_ambient_fills_the_rest() {
    if !gpu_available() {
        return;
    }
    let image = render(&two_face_scene(45.0));

    let lit = luma(image.pixel(RIGHT.0, RIGHT.1));
    let unlit = luma(image.pixel(LEFT.0, LEFT.1));

    assert!(
        lit > unlit + 150,
        "the sun-facing face must be clearly brighter: lit {lit} vs unlit {unlit}"
    );
    // The unlit face shows albedo * 0.05 ambient — dark, but not black.
    let floor = 3 * u32::from(srgb_encode(0.8 * 0.05)) / 2;
    assert!(
        unlit > floor,
        "the unlit face should be ambient-filled, not black: {unlit} <= {floor}"
    );
}

#[test]
fn rotating_the_sun_flips_the_lit_side() {
    // Step 3 of the acceptance loop as a regression test: move the sun, see
    // the shading move.
    if !gpu_available() {
        return;
    }
    let sun_right = render(&two_face_scene(45.0));
    let sun_left = render(&two_face_scene(-45.0));

    assert!(
        luma(sun_right.pixel(RIGHT.0, RIGHT.1)) > luma(sun_right.pixel(LEFT.0, LEFT.1)),
        "yaw 45: right face lit"
    );
    assert!(
        luma(sun_left.pixel(LEFT.0, LEFT.1)) > luma(sun_left.pixel(RIGHT.0, RIGHT.1)),
        "yaw -45: the ordering must flip"
    );
}

#[test]
fn ambient_only_lights_uniformly() {
    if !gpu_available() {
        return;
    }
    let image = render(
        r#"{"name": "ambient_only", "entities": [
            {"name": "Cube", "components": [
                {"type": "Transform", "rotation": [0.0, 45.0, 0.0]},
                {"type": "Mesh", "asset": "builtin:cube"},
                {"type": "Material", "albedo": [0.8, 0.8, 0.8], "roughness": 0.9}
            ]},
            {"name": "Fill", "components": [{"type": "AmbientLight", "intensity": 0.5}]},
            {"name": "Eye", "components": [
                {"type": "Transform", "position": [0.0, 0.0, 4.0]},
                {"type": "Camera", "active": true}
            ]}
        ]}"#,
    );

    // Writing an ambient light disables the fallback sun, so both faces show
    // exactly albedo * 0.5 regardless of orientation.
    let expected = srgb_encode(0.8 * 0.5);
    for (x, y) in [LEFT, RIGHT] {
        let [r, g, b, _] = image.pixel(x, y);
        for channel in [r, g, b] {
            assert!(
                channel.abs_diff(expected) <= 2,
                "ambient-only face at ({x}, {y}) should be {expected} per channel, got {:?}",
                [r, g, b]
            );
        }
    }
}

#[test]
fn emissive_bypasses_lighting() {
    if !gpu_available() {
        return;
    }
    // A zero-intensity ambient is still a light component, so the fallback
    // rig is off and the only output is the emissive term.
    let image = render(
        r#"{"name": "emissive", "entities": [
            {"name": "Beacon", "components": [
                {"type": "Mesh", "asset": "builtin:cube"},
                {"type": "Material", "albedo": [0.0, 0.0, 0.0], "emissive": [0.0, 1.0, 0.2]}
            ]},
            {"name": "Off", "components": [{"type": "AmbientLight", "intensity": 0.0}]},
            {"name": "Eye", "components": [
                {"type": "Transform", "position": [0.0, 0.0, 4.0]},
                {"type": "Camera", "active": true}
            ]}
        ]}"#,
    );

    let [r, g, b, _] = image.pixel(SIZE / 2, SIZE / 2);
    let expected = [srgb_encode(0.0), srgb_encode(1.0), srgb_encode(0.2)];
    assert!(
        r.abs_diff(expected[0]) <= 2 && g.abs_diff(expected[1]) <= 2 && b.abs_diff(expected[2]) <= 2,
        "emissive should encode straight to the target: got {:?}, expected ≈{expected:?}",
        [r, g, b]
    );
}

#[test]
fn lower_roughness_gives_a_hotter_highlight() {
    if !gpu_available() {
        return;
    }
    let sphere_scene = |roughness: f32| {
        format!(
            r#"{{"name": "probe", "entities": [
                {{"name": "Probe", "components": [
                    {{"type": "Mesh", "asset": "builtin:sphere"}},
                    {{"type": "Material", "albedo": [0.5, 0.5, 0.5], "metallic": 0.0,
                      "roughness": {roughness}}}
                ]}},
                {{"name": "Sun", "components": [{{"type": "DirectionalLight"}}]}},
                {{"name": "Fill", "components": [{{"type": "AmbientLight", "intensity": 0.05}}]}},
                {{"name": "Eye", "components": [
                    {{"type": "Transform", "position": [0.0, 0.0, 4.0]}},
                    {{"type": "Camera", "active": true}}
                ]}}
            ]}}"#
        )
    };

    // The sun has no Transform, so its light travels -Z: straight at the
    // sphere from the camera's side, putting the highlight dead center.
    let brightest = |image: &Image| {
        (0..SIZE)
            .flat_map(|y| (0..SIZE).map(move |x| (x, y)))
            .map(|(x, y)| u32::from(image.pixel(x, y)[0]))
            .max()
            .unwrap()
    };

    let smooth = brightest(&render(&sphere_scene(0.1)));
    let rough = brightest(&render(&sphere_scene(0.9)));

    // Diffuse alone peaks at srgb(0.5 + ambient); the tight GGX highlight on
    // the smooth sphere must push well past it (in practice to clamp).
    assert!(
        smooth > rough + 20,
        "roughness 0.1 should have a hotter peak than 0.9: {smooth} vs {rough}"
    );
}
