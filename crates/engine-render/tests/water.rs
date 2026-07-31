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
        &scene.hud_tree(&engine_core::mesh::BuiltinAssets),
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

// ── Refraction (M27) ─────────────────────────────────────────────────────────
//
// The claims `water-refraction-design.md` makes, in the order it makes them:
// the field defaults to off, it moves what is behind the surface, it moves it
// *without* changing how much of it survives, and it does not drag geometry
// standing in the water sideways across the frame.

/// A pond whose bed is a bright sheet under dark bars, seen steeply enough
/// that the frame is water over bed rather than reflected sky.
///
/// A *pattern* is the point: refraction is a displacement, and a displacement
/// of a uniform field is invisible by construction. The bars run across the
/// view direction rather than along it, which is the whole reason there are
/// bars: the refracted ray's horizontal component points the same way the view
/// ray's does — away from the camera — so the displacement is along Z, and a
/// boundary parallel to Z barely moves under it. The first version of this test
/// split the bed left/right and saw 236 pixels change where this one sees
/// thousands.
///
/// The bed is emissive and its albedo is black, so what the tests read is the
/// bed's *pattern* and not the sun's angle on it.
fn patterned_pond(water: &str) -> String {
    format!(
        r#"{{
  "name": "refracting_pond",
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
      {{ "type": "Transform", "position": [0.0, -1.02, 0.0], "scale": [80.0, 1.0, 80.0] }},
      {{ "type": "Mesh", "asset": "builtin:plane" }},
      {{ "type": "Material", "albedo": [0.0, 0.0, 0.0], "emissive": [0.95, 0.95, 0.95] }} ] }},
    {{ "name": "BarA", "components": [
      {{ "type": "Transform", "position": [0.0, -1.0, -12.0], "scale": [60.0, 1.0, 2.0] }},
      {{ "type": "Mesh", "asset": "builtin:plane" }},
      {{ "type": "Material", "albedo": [0.0, 0.0, 0.0], "emissive": [0.02, 0.02, 0.02] }} ] }},
    {{ "name": "BarB", "components": [
      {{ "type": "Transform", "position": [0.0, -1.0, -8.0], "scale": [60.0, 1.0, 2.0] }},
      {{ "type": "Mesh", "asset": "builtin:plane" }},
      {{ "type": "Material", "albedo": [0.0, 0.0, 0.0], "emissive": [0.02, 0.02, 0.02] }} ] }},
    {{ "name": "BarC", "components": [
      {{ "type": "Transform", "position": [0.0, -1.0, -4.0], "scale": [60.0, 1.0, 2.0] }},
      {{ "type": "Mesh", "asset": "builtin:plane" }},
      {{ "type": "Material", "albedo": [0.0, 0.0, 0.0], "emissive": [0.02, 0.02, 0.02] }} ] }},
    {{ "name": "BarD", "components": [
      {{ "type": "Transform", "position": [0.0, -1.0, 0.0], "scale": [60.0, 1.0, 2.0] }},
      {{ "type": "Mesh", "asset": "builtin:plane" }},
      {{ "type": "Material", "albedo": [0.0, 0.0, 0.0], "emissive": [0.02, 0.02, 0.02] }} ] }},
    {{ "name": "BarE", "components": [
      {{ "type": "Transform", "position": [0.0, -1.0, 4.0], "scale": [60.0, 1.0, 2.0] }},
      {{ "type": "Mesh", "asset": "builtin:plane" }},
      {{ "type": "Material", "albedo": [0.0, 0.0, 0.0], "emissive": [0.02, 0.02, 0.02] }} ] }},
    {{ "name": "Lake", "components": [
      {{ "type": "Transform", "position": [0.0, 0.0, 0.0], "scale": [30.0, 1.0, 30.0] }},
      {water} ] }}
  ]
}}"#
    )
}

/// The clear-water body every refraction test uses: absorption turned right
/// down, because a bed the water has already hidden cannot be seen to bend.
const CLEAR: &str = r#""segments": 64,
      "waves": [ {{ "direction": 20.0, "wavelength": 4.0, "amplitude": 0.09, "steepness": 0.4, "speed": 1.5 }} ],
      "detail": 0.0, "roughness": 0.08, "depth_fade": 40.0, "opacity": 0.35"#;

fn clear_water(extra: &str) -> String {
    format!(
        r#"{{ "type": "Water", {}{} }}"#,
        CLEAR.replace("{{", "{").replace("}}", "}"),
        extra
    )
}

#[test]
fn the_ior_default_is_no_refraction_at_all() {
    if !gpu_available() {
        return;
    }
    // The house rule, and the reason M27 re-blessed no committed baseline: a
    // `Water` that does not mention `ior` must be the M18 surface exactly, down
    // to the bytes — which is also the assertion that it takes the plain
    // pipeline rather than the spliced one.
    let absent = patterned_pond(&clear_water(""));
    let explicit = patterned_pond(&clear_water(r#", "ior": 1.0"#));
    assert_eq!(
        render_at(&absent, 2.0).pixels,
        render_at(&explicit, 2.0).pixels,
        "absent `ior` must render byte for byte as `ior: 1.0`"
    );
}

#[test]
fn refraction_displaces_what_is_behind_the_surface() {
    if !gpu_available() {
        return;
    }
    let straight = render_at(&patterned_pond(&clear_water(r#", "ior": 1.0"#)), 2.0);
    let bent = render_at(&patterned_pond(&clear_water(r#", "ior": 1.5"#)), 2.0);

    // The bed's light/dark boundary runs under the middle of the water, and
    // bending the view ray moves where each pixel reads it from. Counted rather
    // than located: the waves make the boundary a ragged line, so the honest
    // assertion is that a great many pixels changed, not that one particular
    // pixel did.
    let moved = straight
        .pixels
        .chunks_exact(4)
        .zip(bent.pixels.chunks_exact(4))
        .filter(|(a, b)| a[0].abs_diff(b[0]) > 8)
        .count();
    assert!(
        moved > 500,
        "refraction should displace the bed across many pixels, but only {moved} changed"
    );
}

#[test]
fn refraction_moves_the_bed_without_changing_how_much_of_it_survives() {
    if !gpu_available() {
        return;
    }
    // `water-refraction-design.md` §3: water keeps its own absorption model and
    // gains no `attenuation`, so turning `ior` on changes *where* the bed is
    // read from and not how much of it comes back. Over a bed of one flat
    // emissive colour a displacement has nothing to displace, so the two
    // renders have to agree — and if refraction had brought its own absorption
    // curve, or dropped the `1 - out_alpha` weighting, this is where it would
    // show as an overall brightness shift.
    let flat = |water: String| {
        patterned_pond(&water).replace(
            r#""emissive": [0.02, 0.02, 0.02]"#,
            r#""emissive": [0.95, 0.95, 0.95]"#,
        )
    };
    let straight = render_at(&flat(clear_water(r#", "ior": 1.0"#)), 2.0);
    let bent = render_at(&flat(clear_water(r#", "ior": 1.5"#)), 2.0);

    let mean = |image: &Image| {
        image
            .pixels
            .chunks_exact(4)
            .map(|p| luma(p.try_into().unwrap()) as u64)
            .sum::<u64>() as f64
            / (SIZE * SIZE) as f64
    };
    let (a, b) = (mean(&straight), mean(&bent));
    assert!(
        (a - b).abs() < 2.0,
        "refraction must not change the water's absorption: mean {a:.2} vs {b:.2}"
    );
}

#[test]
fn geometry_standing_in_the_water_does_not_smear_into_it() {
    if !gpu_available() {
        return;
    }
    // The one place water's refraction is *more* careful than the mesh path
    // (`water-refraction-design.md` §4). A screen-space offset can reach a
    // pixel nearer than the refracting surface; water behind an object standing
    // in it refracts *upward on screen*, straight into that object, and without
    // the depth check the object's colour is dragged out across the water
    // around it. Measured on the M27 fixture, dropping the check moves ~22k
    // pixels by up to 99 — so a smear would be loud here.
    //
    // The pillar is pure red and nothing else in the scene is, which makes
    // "how much red is in the frame" the whole assertion: it must not grow when
    // the water starts bending, because the pillar is *in front of* the water
    // that would otherwise sample it.
    let grazing = |water: String| {
        format!(
            r#"{{
  "name": "pillar_in_water",
  "environment": {{ "sky": true, "shadows": false, "samples": 1 }},
  "entities": [
    {{ "name": "Camera", "components": [
      {{ "type": "Transform", "position": [0.0, 1.1, 9.0], "rotation": [-7.0, 0.0, 0.0] }},
      {{ "type": "Camera", "fov": 60.0, "near": 0.1, "far": 200.0, "active": true }} ] }},
    {{ "name": "Sun", "components": [
      {{ "type": "Transform", "rotation": [-50.0, 0.0, 0.0] }},
      {{ "type": "DirectionalLight", "color": [1.0, 1.0, 1.0], "intensity": 1.0 }} ] }},
    {{ "name": "Fill", "components": [
      {{ "type": "AmbientLight", "color": [1.0, 1.0, 1.0], "intensity": 0.3 }} ] }},
    {{ "name": "Bed", "components": [
      {{ "type": "Transform", "position": [0.0, -1.0, 0.0], "scale": [80.0, 1.0, 80.0] }},
      {{ "type": "Mesh", "asset": "builtin:plane" }},
      {{ "type": "Material", "albedo": [0.0, 0.0, 0.0], "emissive": [0.55, 0.55, 0.55] }} ] }},
    {{ "name": "Pillar", "components": [
      {{ "type": "Transform", "position": [0.0, 0.0, 0.0], "scale": [1.2, 1.6, 1.2] }},
      {{ "type": "Mesh", "asset": "builtin:cube" }},
      {{ "type": "Material", "albedo": [0.0, 0.0, 0.0], "emissive": [0.9, 0.0, 0.0] }} ] }},
    {{ "name": "Lake", "components": [
      {{ "type": "Transform", "position": [0.0, 0.0, -10.0], "scale": [80.0, 1.0, 60.0] }},
      {water} ] }}
  ]
}}"#
        )
    };

    let red_pixels = |image: &Image| {
        image
            .pixels
            .chunks_exact(4)
            .filter(|p| p[0] > 90 && p[0] as u32 > p[1] as u32 * 2 + 40)
            .count()
    };
    let straight = red_pixels(&render_at(&grazing(clear_water(r#", "ior": 1.0"#)), 2.0));
    let bent = red_pixels(&render_at(&grazing(clear_water(r#", "ior": 1.4"#)), 2.0));

    assert!(
        straight > 200,
        "the pillar should be plainly in frame, saw {straight} red pixels"
    );
    assert!(
        bent <= straight + straight / 10,
        "refraction dragged the pillar across the water: {straight} red pixels became {bent}"
    );
}
