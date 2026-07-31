//! Pixel-level tests for the M16 environment: sky, fog, shadows, and the
//! blended pass.
//!
//! Same shape as `lighting.rs` — render a small scene offscreen through the
//! real screenshot path and assert on the bytes that came back — and the same
//! clean skip when the machine has no usable GPU.
//!
//! The load-bearing test here is the last one. Every feature below is opt-in
//! through the scene's `environment` block, and the reason it is opt-in is
//! that a dozen committed baselines were blessed before any of it existed; a
//! scene that asks for none of it has to render byte for byte as it did
//! before. That property is worth a test of its own, because the way it
//! breaks is silent.

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
        &scene.cloud_items(),
        &scene.road_items(),
        &scene.meadow_items(),
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

/// A ground plane with a cube floating above it, lit by a sun coming from
/// straight overhead so the cube's shadow lands directly beneath it. The
/// camera looks down at a shallow angle from +Z.
///
/// `environment` is spliced in verbatim, which is what lets each test below
/// change exactly one thing.
fn shadow_scene(environment: &str) -> String {
    format!(
        r#"{{"name": "env", {environment} "entities": [
            {{"name": "Ground", "components": [
                {{"type": "Transform", "scale": [40.0, 1.0, 40.0]}},
                {{"type": "Mesh", "asset": "builtin:plane"}},
                {{"type": "Material", "albedo": [0.7, 0.7, 0.7], "roughness": 0.9}}
            ]}},
            {{"name": "Caster", "components": [
                {{"type": "Transform", "position": [0.0, 3.0, 0.0], "scale": [2.0, 2.0, 2.0]}},
                {{"type": "Mesh", "asset": "builtin:cube"}},
                {{"type": "Material", "albedo": [0.8, 0.2, 0.2], "roughness": 0.9}}
            ]}},
            {{"name": "Sun", "components": [
                {{"type": "Transform", "rotation": [-90.0, 0.0, 0.0]}},
                {{"type": "DirectionalLight", "intensity": 1.0}}
            ]}},
            {{"name": "Fill", "components": [
                {{"type": "AmbientLight", "intensity": 0.15}}
            ]}},
            {{"name": "Camera", "components": [
                {{"type": "Transform", "position": [0.0, 6.0, 14.0], "rotation": [-20.0, 0.0, 0.0]}},
                {{"type": "Camera", "active": true, "fov": 60.0}}
            ]}}
        ]}}"#
    )
}

#[test]
fn a_caster_darkens_the_ground_beneath_it() {
    if !gpu_available() {
        return;
    }

    let lit = render(&shadow_scene(""));
    let shadowed = render(&shadow_scene(r#""environment": {"shadows": true},"#));

    // Asserted over the whole frame rather than at a hand-computed pixel:
    // where the shadow lands depends on the camera, the sun angle and the
    // projection, and a test that encodes all three breaks the first time the
    // fixture is nudged. What must hold regardless is the *sign* — a shadow
    // pass can only ever remove sunlight.
    let mut darker = 0usize;
    let mut brighter = 0usize;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let before = luma(lit.pixel(x, y));
            let after = luma(shadowed.pixel(x, y));
            if after < before {
                darker += 1;
            } else if after > before {
                brighter += 1;
            }
        }
    }

    assert!(
        darker > 100,
        "a cube over a plane should shadow a visible patch of it, got {darker} px",
    );
    assert_eq!(
        brighter, 0,
        "shadowing may only ever darken; {brighter} pixels got brighter",
    );

    // Shadowed, not black: the ambient term still reaches it. Taken as the
    // darkest ground pixel, wherever it turned out to be.
    let darkest = (0..SIZE)
        .flat_map(|y| (0..SIZE).map(move |x| (x, y)))
        .filter(|&(x, y)| luma(shadowed.pixel(x, y)) < luma(lit.pixel(x, y)))
        .map(|(x, y)| luma(shadowed.pixel(x, y)))
        .min()
        .expect("some pixel darkened");
    assert!(
        darkest > 0,
        "a shadow is an absence of sun, not an absence of light",
    );
}

#[test]
fn the_sky_replaces_the_clear_color_and_runs_blue_upward() {
    if !gpu_available() {
        return;
    }

    let flat = render(&shadow_scene(""));
    let sky = render(&shadow_scene(
        r#""environment": {"sky": true, "sky_zenith": [0.05, 0.15, 0.6],
            "sky_horizon": [0.6, 0.7, 0.85], "sky_ground": [0.1, 0.1, 0.1]},"#,
    ));

    // Top of frame is above the horizon in this camera, and empty of geometry.
    let top = sky.pixel(SIZE / 2, 2);
    assert_ne!(
        top,
        flat.pixel(SIZE / 2, 2),
        "the sky pass must replace the flat clear color",
    );
    assert!(
        top[2] > top[0],
        "sky should be bluer than it is red, got {top:?}",
    );

    // Higher in the frame is nearer the zenith, which is the more saturated
    // end of the gradient.
    let lower = sky.pixel(SIZE / 2, SIZE / 3);
    assert!(
        (top[2] as i32 - top[0] as i32) > (lower[2] as i32 - lower[0] as i32),
        "blue should deepen with height: top {top:?}, lower {lower:?}",
    );
}

#[test]
fn fog_pulls_distant_surfaces_toward_the_horizon_color() {
    if !gpu_available() {
        return;
    }

    let clear = render(&shadow_scene(
        r#""environment": {"sky": true, "sky_horizon": [0.9, 0.1, 0.1]},"#,
    ));
    let foggy = render(&shadow_scene(
        r#""environment": {"sky": true, "sky_horizon": [0.9, 0.1, 0.1], "fog_density": 0.05},"#,
    ));

    // Just below the horizon: the far end of the ground plane, tens of meters
    // out. A deliberately red fog color makes the direction of the shift
    // unambiguous — a grey one could be confused with any other brightening.
    let far = (SIZE / 2, SIZE * 5 / 12);
    let near = (SIZE / 2, SIZE - 2);

    let far_shift = foggy.pixel(far.0, far.1)[0] as i32 - clear.pixel(far.0, far.1)[0] as i32;
    let near_shift = foggy.pixel(near.0, near.1)[0] as i32 - clear.pixel(near.0, near.1)[0] as i32;

    assert!(
        far_shift > 0,
        "distant ground should take on the fog color, shifted {far_shift}",
    );
    assert!(
        far_shift > near_shift,
        "fog must grow with distance: far {far_shift}, near {near_shift}",
    );
}

#[test]
fn a_transparent_surface_shows_what_is_behind_it() {
    if !gpu_available() {
        return;
    }

    let scene = |alpha: f32| {
        format!(
            r#"{{"name": "blend", "entities": [
                {{"name": "Behind", "components": [
                    {{"type": "Transform", "position": [0.0, 0.0, -3.0], "scale": [4.0, 4.0, 0.2]}},
                    {{"type": "Mesh", "asset": "builtin:cube"}},
                    {{"type": "Material", "albedo": [0.9, 0.1, 0.1], "roughness": 0.9}}
                ]}},
                {{"name": "Front", "components": [
                    {{"type": "Transform", "position": [0.0, 0.0, 1.0], "scale": [2.0, 2.0, 0.2]}},
                    {{"type": "Mesh", "asset": "builtin:cube"}},
                    {{"type": "Material", "albedo": [0.1, 0.1, 0.9], "roughness": 0.9,
                      "alpha": {alpha}}}
                ]}},
                {{"name": "Sun", "components": [
                    {{"type": "Transform", "rotation": [0.0, 0.0, 0.0]}},
                    {{"type": "DirectionalLight", "intensity": 1.0}}
                ]}},
                {{"name": "Camera", "components": [
                    {{"type": "Transform", "position": [0.0, 0.0, 8.0]}},
                    {{"type": "Camera", "active": true, "fov": 60.0}}
                ]}}
            ]}}"#
        )
    };

    let center = (SIZE / 2, SIZE / 2);
    let opaque = render(&scene(1.0)).pixel(center.0, center.1);
    let ghost = render(&scene(0.35)).pixel(center.0, center.1);
    // Alpha 0 is the far panel on its own: the near one is still drawn, still
    // sorted, still depth-tested, and contributes nothing. It is the honest
    // reference for "what is behind", and it also checks that a fully clear
    // surface is genuinely invisible rather than merely faint.
    let behind = render(&scene(0.0)).pixel(center.0, center.1);

    // The near panel is blue and covers the far red one completely at this
    // camera, so the blend has to land strictly between the two. Stated as
    // inequalities against both ends rather than as absolute bytes: a lit
    // blue surface is not red 0 (it carries a white specular lobe), and a lit
    // red one is not blue 0.
    assert!(
        ghost[0] > opaque[0] + 20,
        "blending should let the panel behind through: opaque {opaque:?}, ghost {ghost:?}",
    );
    assert!(
        ghost[0] < behind[0],
        "the near panel should still hide some of the far one: behind {behind:?}, ghost {ghost:?}",
    );
    assert!(
        ghost[2] > behind[2] + 20,
        "the near panel is blue and must tint the result: behind {behind:?}, ghost {ghost:?}",
    );
}

/// The whole reason every M16 feature defaults to off.
///
/// A scene with no `environment` block must produce the identical bytes it
/// produced before the block existed. There is no committed PNG to compare
/// against here — that is what the `verify/` baselines and the CLI suite are
/// for — so this pins the weaker but still useful property: turning a feature
/// on changes the image, and leaving the block out is the same as writing one
/// with every default in it.
#[test]
fn an_absent_environment_block_renders_exactly_like_a_default_one() {
    if !gpu_available() {
        return;
    }

    let absent = render(&shadow_scene(""));
    let explicit = render(&shadow_scene(
        r#""environment": {"sky": false, "fog_density": 0.0, "shadows": false, "samples": 1},"#,
    ));
    assert_eq!(
        absent.pixels, explicit.pixels,
        "an absent environment block and an all-defaults one must agree",
    );

    // And the features really are doing something, so the equality above is
    // not two broken paths agreeing.
    let shadowed = render(&shadow_scene(r#""environment": {"shadows": true},"#));
    assert_ne!(
        absent.pixels, shadowed.pixels,
        "turning shadows on should change the image",
    );
}

/// MSAA must resolve to the same image where the image has no edges in it,
/// and differ where it does. Interior pixels of a flat surface have every
/// sample covered by the same triangle, so the resolve returns that color
/// exactly; a silhouette pixel is a blend.
#[test]
fn msaa_smooths_edges_without_disturbing_flat_interiors() {
    if !gpu_available() {
        return;
    }

    let aliased = render(&shadow_scene(r#""environment": {"samples": 1},"#));
    let smoothed = render(&shadow_scene(r#""environment": {"samples": 4},"#));

    // Dead center of the cube's front face: no edge within many pixels.
    assert_eq!(
        aliased.pixel(SIZE / 2, SIZE / 3),
        smoothed.pixel(SIZE / 2, SIZE / 3),
        "a fully covered interior pixel must survive the resolve unchanged",
    );

    // Somewhere along the cube's silhouette the two must disagree, or MSAA is
    // not running at all.
    let differs = (0..SIZE).any(|y| (0..SIZE).any(|x| aliased.pixel(x, y) != smoothed.pixel(x, y)));
    assert!(differs, "4x MSAA should change at least one edge pixel");
}
