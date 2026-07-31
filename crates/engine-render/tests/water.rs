//! Pixel-level tests for M18 water.
//!
//! Same shape as `environment.rs`: render a small scene offscreen through the
//! real screenshot path, assert on the bytes, and skip cleanly on a machine
//! with no usable GPU.
//!
//! Water is mostly a *look*, and a look is not a thing to write an assertion
//! about. What is testable is the set of claims the design makes — waves move
//! the surface, deep water hides its bed and shallow water does not, the
//! shoreline foams, a surface exists when seen from below, the same time gives
//! the same pixels, and a scene with no water is untouched by any of it. Those
//! are the tests, and they are the ones that fail loudly if the pass structure,
//! the depth copy, or the uniform packing breaks.

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

fn render_at(source: &str, time: f32) -> Image {
    let scene = Scene::from_source(source, "test.json").expect("test scene should be valid");
    let (camera, camera_transform) = scene.camera(None).expect("test scene needs a camera");
    let items = scene
        .render_items(&BuiltinAssets)
        .expect("test scenes use builtins only");
    offscreen::render(
        &items,
        &scene.water_items(),
        &scene.cloud_items(),
        &scene.road_items(),
        &scene.meadow_items(),
        &[],
        &camera,
        camera_transform.matrix(),
        scene.lights().resolved(),
        scene.environment,
        time,
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

/// A pond over a bright bed, seen from above at a steep angle so what the test
/// reads is the water *body* rather than the reflected sky.
///
/// `WATER` and `BED_Y` are spliced per test: one field changes at a time.
fn pond(water: &str, bed_y: f32) -> String {
    format!(
        r#"{{
  "name": "pond",
  "environment": {{ "sky": true, "shadows": false, "samples": 1 }},
  "entities": [
    {{ "name": "Camera", "components": [
      {{ "type": "Transform", "position": [0.0, 6.0, 6.0], "rotation": [-45.0, 0.0, 0.0] }},
      {{ "type": "Camera", "fov": 60.0, "near": 0.1, "far": 100.0, "active": true }} ] }},
    {{ "name": "Sun", "components": [
      {{ "type": "Transform", "rotation": [-60.0, 0.0, 0.0] }},
      {{ "type": "DirectionalLight", "color": [1.0, 1.0, 1.0], "intensity": 1.0 }} ] }},
    {{ "name": "Fill", "components": [
      {{ "type": "AmbientLight", "color": [1.0, 1.0, 1.0], "intensity": 0.3 }} ] }},
    {{ "name": "Bed", "components": [
      {{ "type": "Transform", "position": [0.0, {bed_y}, 0.0], "scale": [40.0, 1.0, 40.0] }},
      {{ "type": "Mesh", "asset": "builtin:plane" }},
      {{ "type": "Material", "albedo": [0.9, 0.9, 0.9], "roughness": 0.9 }} ] }},
    {{ "name": "Lake", "components": [
      {{ "type": "Transform", "position": [0.0, 0.0, 0.0], "scale": [30.0, 1.0, 30.0] }},
      {water} ] }}
  ]
}}"#
    )
}

/// The centre of the frame, which every scene here fills with water.
fn centre(image: &Image) -> [u8; 4] {
    image.pixel(SIZE / 2, SIZE / 2)
}

#[test]
fn waves_move_the_surface_over_time() {
    if !gpu_available() {
        return;
    }
    // No detail ripples, so the *only* thing that can differ between two times
    // is the wave geometry itself.
    let source = pond(
        r#"{ "type": "Water", "segments": 64, "detail": 0.0,
             "waves": [ { "direction": 0.0, "wavelength": 4.0, "amplitude": 0.4,
                          "steepness": 0.5, "speed": 1.0 } ] }"#,
        -1.0,
    );

    let rest = render_at(&source, 0.0);
    let later = render_at(&source, 1.3);
    assert_ne!(
        rest.pixels, later.pixels,
        "a travelling wave must change the picture between t=0 and t=1.3"
    );

    // And a wave with no speed must not: the surface is a static shape, which
    // is what makes `--steps 0` and the editor viewport well-defined.
    let frozen = pond(
        r#"{ "type": "Water", "segments": 64, "detail": 0.0,
             "waves": [ { "direction": 0.0, "wavelength": 4.0, "amplitude": 0.4,
                          "steepness": 0.5, "speed": 0.0 } ] }"#,
        -1.0,
    );
    assert_eq!(
        render_at(&frozen, 0.0).pixels,
        render_at(&frozen, 9.0).pixels,
        "a wave with speed 0 is static geometry, at any time"
    );
}

#[test]
fn the_same_time_gives_the_same_pixels() {
    if !gpu_available() {
        return;
    }
    // The whole reproducibility claim in one assertion: water is a pure
    // function of (file, time), so a baseline can pin it.
    let source = pond(
        r#"{ "type": "Water", "segments": 96, "detail": 0.8, "crest_foam": 0.4,
             "shore_foam": 0.5,
             "waves": [ { "direction": 20.0, "wavelength": 3.0, "amplitude": 0.25,
                          "steepness": 0.4, "speed": 1.7 },
                        { "direction": -70.0, "wavelength": 1.2, "amplitude": 0.08,
                          "steepness": 0.3, "speed": 1.1 } ] }"#,
        -1.5,
    );
    assert_eq!(
        render_at(&source, 2.5).pixels,
        render_at(&source, 2.5).pixels,
        "same file, same time, same bytes"
    );
}

#[test]
fn deep_water_hides_its_bed_and_shallow_water_does_not() {
    if !gpu_available() {
        return;
    }
    // The absorption claim. Same water, same white bed, two depths: the deeper
    // one must come back darker, because more of the view ray ran through the
    // water body and less of the bed survived.
    let water = r#"{ "type": "Water", "segments": 32, "detail": 0.0, "waves": [],
                     "shallow_color": [0.05, 0.15, 0.15],
                     "deep_color": [0.0, 0.02, 0.04],
                     "depth_fade": 1.0, "opacity": 1.0 }"#;

    let shallow = luma(centre(&render_at(&pond(water, -0.15), 0.0)));
    let deep = luma(centre(&render_at(&pond(water, -8.0), 0.0)));
    assert!(
        deep + 30 < shallow,
        "deep water should be much darker than shallow water over the same bed \
         (deep {deep}, shallow {shallow})"
    );

    // And absorption has to be what did it, not the geometry: perfectly clear
    // water over the deep bed keeps far more of it.
    let clear = water.replace(r#""depth_fade": 1.0"#, r#""depth_fade": 1000.0"#);
    let clear_deep = luma(centre(&render_at(&pond(&clear, -8.0), 0.0)));
    assert!(
        clear_deep > deep,
        "raising depth_fade must let more of the bed through (clear {clear_deep}, absorbed {deep})"
    );
}

#[test]
fn shore_foam_brightens_where_the_bed_is_close() {
    if !gpu_available() {
        return;
    }
    // A bed 4 cm under the surface is inside any sensible foam width, so the
    // same scene with `shore_foam` on must come back brighter — foam is
    // scattered light, and it is opaque where it appears.
    let base = r#"{ "type": "Water", "segments": 32, "detail": 0.0, "waves": [],
                    "shallow_color": [0.02, 0.06, 0.06], "deep_color": [0.0, 0.01, 0.02],
                    "depth_fade": 0.2, "opacity": 1.0, "foam_color": [1.0, 1.0, 1.0],
                    "shore_foam": SHORE }"#;

    let dry = luma(centre(&render_at(
        &pond(&base.replace("SHORE", "0.0"), -0.04),
        0.0,
    )));
    let foamy = luma(centre(&render_at(
        &pond(&base.replace("SHORE", "1.5"), -0.04),
        0.0,
    )));
    assert!(
        foamy > dry + 40,
        "shore foam should visibly brighten water lying just over its bed \
         (foam {foamy}, none {dry})"
    );
}

#[test]
fn a_surface_is_visible_from_underneath() {
    if !gpu_available() {
        return;
    }
    // Back-face culling is on globally, and a water grid is a single sheet: if
    // the pipeline culled, water would silently vanish for any camera below the
    // waterline. Look up at it from below and require *something* to be there.
    let source = format!(
        r#"{{
  "name": "under",
  "environment": {{ "sky": true, "samples": 1 }},
  "entities": [
    {{ "name": "Camera", "components": [
      {{ "type": "Transform", "position": [0.0, -2.0, 0.0], "rotation": [80.0, 0.0, 0.0] }},
      {{ "type": "Camera", "fov": 70.0, "near": 0.1, "far": 100.0, "active": true }} ] }},
    {{ "name": "Sun", "components": [
      {{ "type": "Transform", "rotation": [-70.0, 0.0, 0.0] }},
      {{ "type": "DirectionalLight", "intensity": 1.0 }} ] }},
    {{ "name": "Lake", "components": [
      {{ "type": "Transform", "scale": [40.0, 1.0, 40.0] }},
      {{ "type": "Water", "segments": 16, "detail": 0.0, "waves": [],
         "shallow_color": [0.9, 0.1, 0.1], "deep_color": [0.9, 0.1, 0.1],
         "depth_fade": 0.5, "opacity": 1.0 }} ] }}
  ]
}}"#
    );
    let image = render_at(&source, 0.0);
    let pixel = centre(&image);
    assert!(
        pixel[0] > pixel[2] + 20,
        "the underside of the surface should show its (red) water body, got {pixel:?}"
    );
}

#[test]
fn a_scene_with_no_water_is_untouched_by_the_water_pass() {
    if !gpu_available() {
        return;
    }
    // The load-bearing test, and the reason the pass structure branches on
    // whether any water exists at all: M18 arrived after seventeen milestones
    // of committed baselines, and a scene with no `Water` component in it has
    // to render byte for byte as it did before. The way that breaks is silent
    // — an extra pass, a store op, a resolve moved one pass later — so it is
    // worth asserting rather than assuming.
    //
    // `time` is varied deliberately: nothing but water may read the clock.
    let dry = pond(
        r#"{ "type": "Mesh", "asset": "builtin:cube" }, { "type": "Material" }"#,
        -1.0,
    );
    assert_eq!(
        render_at(&dry, 0.0).pixels,
        render_at(&dry, 7.25).pixels,
        "with no water in the scene, scene time must change nothing"
    );
}
