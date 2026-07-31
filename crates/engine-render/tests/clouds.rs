//! Pixel-level tests for M20 clouds.
//!
//! Same shape as `water.rs` and `environment.rs`: render a small scene offscreen
//! through the real screenshot path, assert on the bytes, and skip cleanly on a
//! machine with no usable GPU.
//!
//! A cloud is mostly a *look*, and a look is not a thing to write an assertion
//! about. What is testable is the set of claims the design makes — a cloud is
//! visible against the sky, its sunlit side is brighter than its shaded side,
//! `density` and `feather` do what they say, `drift` moves it on the scene
//! clock, a cloud is still there when the camera is inside it, and a scene with
//! no clouds is untouched by any of it.

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

/// One entity filling the middle of the frame against a deliberately *dark*
/// sky, lit from straight above so "top" and "bottom" mean something.
///
/// The sky is dark on purpose. Against the default clear-day horizon a white
/// cloud is only a little brighter than what is behind it, so every "is it
/// there" assertion would be measuring a handful of levels; against this one
/// the difference is unmistakable and a threshold means something.
///
/// `COMPONENTS` is the whole component list, Transform included, so a test can
/// ask for a scene with no cloud in it at all.
fn sky(components: &str) -> String {
    format!(
        r#"{{
  "name": "sky",
  "environment": {{ "sky": true, "shadows": false, "samples": 1,
    "sky_zenith": [0.04, 0.10, 0.30], "sky_horizon": [0.12, 0.20, 0.40],
    "sky_ground": [0.05, 0.05, 0.05] }},
  "entities": [
    {{ "name": "Camera", "components": [
      {{ "type": "Transform", "position": [0.0, 0.0, 26.0] }},
      {{ "type": "Camera", "fov": 60.0, "near": 0.1, "far": 400.0, "active": true }} ] }},
    {{ "name": "Sun", "components": [
      {{ "type": "Transform", "rotation": [-90.0, 0.0, 0.0] }},
      {{ "type": "DirectionalLight", "color": [1.0, 1.0, 1.0], "intensity": 1.0 }} ] }},
    {{ "name": "Fill", "components": [
      {{ "type": "AmbientLight", "color": [1.0, 1.0, 1.0], "intensity": 0.3 }} ] }},
    {{ "name": "Subject", "components": [ {components} ] }}
  ]
}}"#
    )
}

fn luma(pixel: [u8; 4]) -> u32 {
    pixel[0] as u32 + pixel[1] as u32 + pixel[2] as u32
}

fn centre(image: &Image) -> [u8; 4] {
    image.pixel(SIZE / 2, SIZE / 2)
}

/// Where every subject sits: filling the middle of the frame.
const PLACE: &str = r#"{ "type": "Transform", "scale": [20.0, 20.0, 20.0] }"#;

/// A cloud filling the frame, one lobe so the geometry under the test is a
/// sphere and every claim is about the *shading*. `CLOUD_FIELDS` is where a
/// test splices the one field it is varying.
fn one_lobe(fields: &str) -> String {
    format!(
        r#"{PLACE}, {{ "type": "Cloud", "lobes": 1, "levels": 0, "children": 0,
        "lobe_size": 1.0, "wobble": 0.0, "jitter": 0.0, "flatten": 0.0 {fields} }}"#
    )
}

/// The same frame with nothing in it: the sky alone.
const NOTHING: &str = PLACE;

#[test]
fn a_cloud_is_visible_against_the_sky() {
    if !gpu_available() {
        return;
    }
    let with = render_at(&sky(&one_lobe("")), 0.0);
    let without = render_at(&sky(&one_lobe(", \"density\": 0.0")), 0.0);

    // A white cloud in front of a blue sky is brighter than the sky, and at
    // `density: 0` it must be exactly the sky — that second half is what keeps
    // "the pass ran" from passing for "the pass drew something".
    assert!(
        luma(centre(&with)) > luma(centre(&without)) + 30,
        "cloud {:?} did not stand out against sky {:?}",
        centre(&with),
        centre(&without)
    );
    let sky_only = render_at(&sky(NOTHING), 0.0);
    assert_eq!(
        centre(&without),
        centre(&sky_only),
        "a zero-density cloud must leave the sky exactly as it found it"
    );
}

#[test]
fn the_sunlit_side_is_brighter_than_the_shaded_side() {
    if !gpu_available() {
        return;
    }
    // The sun points straight down, so the top of the lobe is lit and the
    // bottom is not. This is the wrapped-diffuse claim, and it is the one that
    // breaks if the normals or the through-scatter term are wired wrong.
    let image = render_at(&sky(&one_lobe("")), 0.0);
    let top = luma(image.pixel(SIZE / 2, SIZE / 4));
    let bottom = luma(image.pixel(SIZE / 2, SIZE * 3 / 4));
    assert!(
        top > bottom + 40,
        "the lit top ({top}) should be clearly brighter than the shaded base ({bottom})"
    );
}

#[test]
fn density_is_how_much_of_the_sky_a_cloud_hides() {
    if !gpu_available() {
        return;
    }
    let sky_only = luma(centre(&render_at(&sky(NOTHING), 0.0)));
    let thin = luma(centre(&render_at(
        &sky(&one_lobe(", \"density\": 0.25")),
        0.0,
    )));
    let thick = luma(centre(&render_at(
        &sky(&one_lobe(", \"density\": 1.0")),
        0.0,
    )));
    assert!(
        thin > sky_only && thick > thin,
        "density should grade the sky ({sky_only}) through thin ({thin}) to thick ({thick})"
    );
}

#[test]
fn a_lower_feather_thins_the_cloud_toward_its_silhouette() {
    if !gpu_available() {
        return;
    }
    // Near the rim the surface is nearly edge-on. `feather` is the exponent on
    // how fast that thins, and the sign of its effect is the thing to pin:
    // higher is crisper, which is the opposite of the first version of this
    // field and exactly the kind of thing a rename would silently invert.
    let at = |feather: f32| {
        let image = render_at(&sky(&one_lobe(&format!(", \"feather\": {feather}"))), 0.0);
        // Three quarters of the way out toward the silhouette of a lobe that
        // fills the middle half of the frame.
        luma(image.pixel(SIZE / 2 + SIZE * 7 / 32, SIZE / 2))
    };
    assert!(
        at(4.0) > at(1.0),
        "a crisper feather ({}) should be more opaque at the rim than a wispy one ({})",
        at(4.0),
        at(1.0)
    );
}

#[test]
fn drift_moves_a_cloud_on_the_scene_clock() {
    if !gpu_available() {
        return;
    }
    // A cloud small enough to leave the frame, drifting hard to one side. The
    // claim is that `time` alone moves it — no simulation, no script — which is
    // what lets a drifting sky sit under a `--time` baseline.
    let drifting = one_lobe(", \"drift\": [12.0, 0.0, 0.0]");
    let start = render_at(&sky(&drifting), 0.0);
    let later = render_at(&sky(&drifting), 2.0);
    assert_ne!(
        centre(&start),
        centre(&later),
        "a drifting cloud must not render identically two seconds apart"
    );

    // And the same instant twice is the same bytes, which is the property the
    // baseline depends on.
    assert_eq!(render_at(&sky(&drifting), 2.0).pixels, later.pixels);

    // Wrapping brings it back: one full wrap period is where it started.
    let wrapping = one_lobe(", \"drift\": [12.0, 0.0, 0.0], \"drift_wrap\": 24.0");
    assert_eq!(
        render_at(&sky(&wrapping), 0.0).pixels,
        render_at(&sky(&wrapping), 2.0).pixels,
        "a cloud drifting 12 m/s with a 24 m wrap is back where it started at t = 2"
    );
}

#[test]
fn a_cloud_is_visible_from_inside_it() {
    if !gpu_available() {
        return;
    }
    // Culling is off for the cloud pipeline, and this is why: with it on, a
    // cloud would vanish the instant the camera entered one. The camera sits at
    // the origin, inside a lobe that spans 10 m in every direction.
    let move_in = |s: String| {
        s.replace(
            "\"position\": [0.0, 0.0, 26.0]",
            "\"position\": [0.0, 0.0, 0.0]",
        )
    };
    let image = render_at(&move_in(sky(&one_lobe(""))), 0.0);
    let sky_only = render_at(&move_in(sky(NOTHING)), 0.0);
    assert_ne!(
        centre(&image),
        centre(&sky_only),
        "the far wall of the lobe should still be drawn from inside it"
    );
}

#[test]
fn a_scene_with_no_clouds_is_untouched_by_the_cloud_pass() {
    if !gpu_available() {
        return;
    }
    // The invariant every milestone since M16 has had to hold: nineteen
    // committed baselines predate this component, and a scene that does not use
    // it must render exactly as it did. Two different times, because `time` is
    // the only new input reaching the frame.
    let source = sky(&format!(
        r#"{PLACE}, {{ "type": "Mesh", "asset": "builtin:sphere" }},
        {{ "type": "Material", "albedo": [0.8, 0.2, 0.2] }}"#
    ));
    let first = render_at(&source, 0.0);
    let second = render_at(&source, 7.5);
    assert_eq!(
        first.pixels, second.pixels,
        "a cloudless scene must not depend on the clock"
    );
}
