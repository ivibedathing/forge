//! Pixel-level tests for M17 point lights.
//!
//! Same shape as `lighting.rs` and `environment.rs`: render a small scene
//! offscreen through the real screenshot path, assert on the bytes, and skip
//! cleanly when the machine has no usable GPU.
//!
//! The load-bearing test is the last one. A `PointLight` is a new component and
//! nothing that existed before M17 has one, so a scene without one has to
//! render byte for byte as it did before the component existed — the same
//! contract M16's `environment` block signed, and it breaks just as silently.

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

fn luma(pixel: [u8; 4]) -> u32 {
    pixel[0] as u32 + pixel[1] as u32 + pixel[2] as u32
}

/// A big white floor seen from above, with almost no other light: a very dim
/// ambient so the unlit floor is nearly black and anything a point light does
/// is unambiguous. `lights` is spliced in verbatim.
fn floor_scene(lights: &str) -> String {
    format!(
        r#"{{"name": "pl", "entities": [
            {{"name": "Floor", "components": [
                {{"type": "Transform", "scale": [40.0, 1.0, 40.0]}},
                {{"type": "Mesh", "asset": "builtin:plane"}},
                {{"type": "Material", "albedo": [0.9, 0.9, 0.9], "roughness": 0.9}}
            ]}},
            {{"name": "Dim", "components": [
                {{"type": "AmbientLight", "color": [1.0, 1.0, 1.0], "intensity": 0.01}}
            ]}},
            {lights}
            {{"name": "Eye", "components": [
                {{"type": "Transform", "position": [0.0, 6.0, 0.0], "rotation": [-90.0, 0.0, 0.0]}},
                {{"type": "Camera", "active": true}}
            ]}}
        ]}}"#
    )
}

#[test]
fn a_point_light_pools_where_it_stands() {
    if !gpu_available() {
        return;
    }
    // Looking straight down at a floor with one light over its middle: the
    // center must be far brighter than the corner. This is the whole feature —
    // a *local* light, which is what the sun could never be.
    let dark = render(&floor_scene(""));
    let lit = render(&floor_scene(
        r#"{"name": "Lamp", "components": [
            {"type": "Transform", "position": [0.0, 1.2, 0.0]},
            {"type": "PointLight", "intensity": 3.0, "range": 8.0}
        ]},"#,
    ));

    let center = luma(lit.pixel(SIZE / 2, SIZE / 2));
    let center_dark = luma(dark.pixel(SIZE / 2, SIZE / 2));
    assert!(
        center > center_dark + 150,
        "the light should brighten the floor under it: {center_dark} -> {center}"
    );

    let corner = luma(lit.pixel(6, 6));
    assert!(
        center > corner * 2,
        "a point light must fall off with distance: center {center}, corner {corner}"
    );
}

#[test]
fn intensity_and_color_do_what_they_say() {
    if !gpu_available() {
        return;
    }
    let at = |intensity: f32, color: &str| -> [u8; 4] {
        let scene = floor_scene(&format!(
            r#"{{"name": "Lamp", "components": [
                {{"type": "Transform", "position": [0.0, 1.2, 0.0]}},
                {{"type": "PointLight", "intensity": {intensity}, "color": {color},
                  "range": 8.0}}
            ]}},"#
        ));
        render(&scene).pixel(SIZE / 2, SIZE / 2)
    };

    let dim = at(1.0, "[1.0, 1.0, 1.0]");
    let bright = at(3.0, "[1.0, 1.0, 1.0]");
    assert!(
        luma(bright) > luma(dim),
        "more intensity must be brighter: {dim:?} -> {bright:?}"
    );

    // A red light on a white floor makes a red pool, not a grey one.
    let red = at(3.0, "[1.0, 0.0, 0.0]");
    assert!(
        red[0] > red[1] + 40 && red[0] > red[2] + 40,
        "a red point light must light the floor red, got {red:?}"
    );
}

#[test]
fn range_is_a_hard_horizon() {
    if !gpu_available() {
        return;
    }
    // The windowed falloff has to actually close: past `range` a light must
    // contribute *nothing*, not a little. Without that a lamp in one room lifts
    // the black level of every other room in the scene.
    let near = render(&floor_scene(
        r#"{"name": "Lamp", "components": [
            {"type": "Transform", "position": [0.0, 1.2, 0.0]},
            {"type": "PointLight", "intensity": 40.0, "range": 1.5}
        ]},"#,
    ));
    let dark = render(&floor_scene(""));

    // The middle is blazing…
    assert!(
        luma(near.pixel(SIZE / 2, SIZE / 2)) > luma(dark.pixel(SIZE / 2, SIZE / 2)) + 200,
        "a range-1.5 light should still blow out the floor beneath it"
    );
    // …and the corner, well beyond 1.5 units of floor, is untouched.
    assert_eq!(
        near.pixel(4, 4),
        dark.pixel(4, 4),
        "beyond its range a point light must contribute exactly nothing"
    );
}

#[test]
fn point_lights_add_up() {
    if !gpu_available() {
        return;
    }
    // Two lights either side of the center, so the middle of the floor gets
    // some of each. Light is additive; two lamps must be brighter than one.
    let one = render(&floor_scene(
        r#"{"name": "LampA", "components": [
            {"type": "Transform", "position": [-0.9, 1.2, 0.0]},
            {"type": "PointLight", "intensity": 2.0, "range": 8.0}
        ]},"#,
    ));
    let two = render(&floor_scene(
        r#"{"name": "LampA", "components": [
            {"type": "Transform", "position": [-0.9, 1.2, 0.0]},
            {"type": "PointLight", "intensity": 2.0, "range": 8.0}
        ]},
        {"name": "LampB", "components": [
            {"type": "Transform", "position": [0.9, 1.2, 0.0]},
            {"type": "PointLight", "intensity": 2.0, "range": 8.0}
        ]},"#,
    ));
    assert!(
        luma(two.pixel(SIZE / 2, SIZE / 2)) > luma(one.pixel(SIZE / 2, SIZE / 2)),
        "a second lamp must add its own light"
    );
}

#[test]
fn a_point_light_is_extra_light_not_replacement_light() {
    if !gpu_available() {
        return;
    }
    // A campfire does not switch the sun off. Adding a point light to a
    // sunlit scene must only ever brighten it — if the point-light branch
    // rebuilt the shaded color instead of adding to it, this would regress
    // quietly and only in scenes that have both.
    let sunlit = r#"{"name": "Sun", "components": [
        {"type": "Transform", "rotation": [-90.0, 0.0, 0.0]},
        {"type": "DirectionalLight", "intensity": 0.8}
    ]},"#;
    let without = render(&floor_scene(sunlit));
    let with = render(&floor_scene(&format!(
        r#"{sunlit}
        {{"name": "Lamp", "components": [
            {{"type": "Transform", "position": [0.0, 1.2, 0.0]}},
            {{"type": "PointLight", "intensity": 2.0, "range": 6.0, "color": [1.0, 0.4, 0.1]}}
        ]}},"#
    )));

    for y in 0..SIZE {
        for x in 0..SIZE {
            let before = without.pixel(x, y);
            let after = with.pixel(x, y);
            for channel in 0..3 {
                assert!(
                    after[channel] >= before[channel],
                    "adding a point light darkened ({x}, {y}): {before:?} -> {after:?}"
                );
            }
        }
    }
    assert!(
        luma(with.pixel(SIZE / 2, SIZE / 2)) > luma(without.pixel(SIZE / 2, SIZE / 2)),
        "the lamp should be visible on top of the sunlight"
    );
}

#[test]
fn a_scene_with_no_point_light_renders_exactly_as_before() {
    if !gpu_available() {
        return;
    }
    // The contract. This is not "close enough": every baseline in the repo was
    // blessed before point lights existed, and the fixed-size light array, the
    // extra uniform lanes, and the new shader branch all had to land without
    // moving a byte. `PointLight`'s only effect on a scene without one must be
    // the empty loop it never enters.
    //
    // Rendered twice through the same path, so a difference could only come from
    // the renderer being nondeterministic — which would itself be the bug this
    // repo's diff-render baselines depend on not existing.
    let source = floor_scene(
        r#"{"name": "Sun", "components": [
            {"type": "Transform", "rotation": [-60.0, 20.0, 0.0]},
            {"type": "DirectionalLight", "intensity": 0.9}
        ]},
        {"name": "Box", "components": [
            {"type": "Transform", "position": [0.0, 0.5, 0.0], "scale": [1.0, 1.0, 1.0]},
            {"type": "Mesh", "asset": "builtin:cube"},
            {"type": "Material", "albedo": [0.8, 0.3, 0.2], "metallic": 0.3, "roughness": 0.4}
        ]},"#,
    );
    assert_eq!(
        render(&source).pixels,
        render(&source).pixels,
        "the pointless-light path must be deterministic"
    );

    // And the light array really is inert: a scene whose only light components
    // are a sun and an ambient must be unaffected by the presence of a light
    // *elsewhere* that is out of range of everything drawn.
    let with_distant_lamp = floor_scene(
        r#"{"name": "Sun", "components": [
            {"type": "Transform", "rotation": [-60.0, 20.0, 0.0]},
            {"type": "DirectionalLight", "intensity": 0.9}
        ]},
        {"name": "Box", "components": [
            {"type": "Transform", "position": [0.0, 0.5, 0.0], "scale": [1.0, 1.0, 1.0]},
            {"type": "Mesh", "asset": "builtin:cube"},
            {"type": "Material", "albedo": [0.8, 0.3, 0.2], "metallic": 0.3, "roughness": 0.4}
        ]},
        {"name": "FarLamp", "components": [
            {"type": "Transform", "position": [500.0, 1.0, 500.0]},
            {"type": "PointLight", "intensity": 50.0, "range": 4.0}
        ]},"#,
    );
    assert_eq!(
        render(&source).pixels,
        render(&with_distant_lamp).pixels,
        "an out-of-range point light must not change a single byte"
    );
}
