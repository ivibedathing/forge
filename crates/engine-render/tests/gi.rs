//! Pixel-level tests for M35 global illumination.
//!
//! These are the GPU half of the milestone's claim. The CPU half —
//! `an_unoccluded_probe_reproduces_sky_ambient` and its neighbours — proves the
//! *fold* is right; nothing there touches a shader. What is only checkable here
//! is that `gi.wgsl` computes the same thing the fold intended, and that the
//! variant carrying it still lights everything else exactly as before.
//!
//! Like the other render tests, each skips cleanly when the machine has no
//! usable GPU.

use engine_core::gi::{self, bake};
use engine_core::math::Vec3;
use engine_core::mesh::BuiltinAssets;
use engine_core::Scene;
use engine_render::offscreen::{self, Image};
use engine_render::Gpu;

const SIZE: u32 = 192;

fn gpu_available() -> bool {
    let available = pollster::block_on(Gpu::new(Gpu::default_instance(), None)).is_ok();
    if !available {
        eprintln!("skipping: no usable GPU on this machine");
    }
    available
}

/// A white floor, a strongly red wall beside it, and a white slab under an
/// overhang — the smallest scene in which GI has something to say. No terrain
/// and no ground cover, so the render is reproducible on this adapter (M22's
/// rule), and `samples: 1` for the same reason.
fn source(intensity: f32) -> String {
    format!(
        r#"{{
  "name": "gi test",
  "environment": {{
    "sky": true,
    "sky_zenith": [0.2, 0.4, 0.9],
    "sky_horizon": [0.7, 0.8, 0.95],
    "sky_ground": [0.2, 0.18, 0.15],
    "samples": 1
  }},
  "entities": [
    {{ "name": "Camera", "components": [
      {{ "type": "Transform", "position": [0.0, 2.0, 7.0], "rotation": [-10.0, 0.0, 0.0] }},
      {{ "type": "Camera", "active": true }} ] }},
    {{ "name": "Sun", "components": [
      {{ "type": "Transform", "rotation": [-60.0, 20.0, 0.0] }},
      {{ "type": "DirectionalLight", "color": [1.0, 1.0, 1.0], "intensity": 1.2 }} ] }},
    {{ "name": "Fill", "components": [
      {{ "type": "AmbientLight", "color": [1.0, 1.0, 1.0], "intensity": 0.4 }} ] }},
    {{ "name": "Floor", "components": [
      {{ "type": "Transform", "position": [0.0, -0.1, 0.0], "scale": [12.0, 0.2, 12.0] }},
      {{ "type": "Mesh", "asset": "builtin:cube" }},
      {{ "type": "Material", "albedo": [0.8, 0.8, 0.8], "roughness": 0.9 }} ] }},
    {{ "name": "RedWall", "components": [
      {{ "type": "Transform", "position": [-2.5, 2.0, 0.0], "scale": [0.4, 4.0, 8.0] }},
      {{ "type": "Mesh", "asset": "builtin:cube" }},
      {{ "type": "Material", "albedo": [0.9, 0.03, 0.03], "roughness": 0.9 }} ] }},
    {{ "name": "Lighting", "components": [
      {{ "type": "Transform", "position": [0.0, 2.0, 0.0], "scale": [8.0, 4.0, 8.0] }},
      {{ "type": "LightProbeVolume", "spacing": 1.0, "bake": "gi/t.gi.json",
         "bounces": 1, "intensity": {intensity}, "blend": 0.0 }} ] }}
  ]
}}"#
    )
}

/// Load, bake in memory, fold, and render.
///
/// The bake goes through the same `bake::bake` the CLI calls, so what this
/// renders is what `engine bake-gi` would have written — no second code path
/// for the tests to agree with instead of the product.
fn render(intensity: f32, with_gi: bool) -> Image {
    // `gi_bake_missing` is expected here: the bake lives in memory, not on
    // disk, which is the whole point of building it in the test.
    let scene = Scene::from_source_ignoring(&source(intensity), "test.json", &["gi_bake_missing"])
        .expect("test scene should be valid apart from the absent bake file");
    let (camera, camera_transform) = scene.camera(None).expect("test scene needs a camera");
    let items = scene
        .render_items(&BuiltinAssets)
        .expect("test scenes use builtins only");

    let field = with_gi.then(|| {
        let (name, volume, _) = gi::evaluate::rendered_volume(&scene).expect("the scene has one");
        let tris = bake::collect_occluders(&scene, &BuiltinAssets).expect("builtins only");
        let params = bake::BakeParams {
            samples: 128,
            bounces: 1,
        };
        let (baked, _) = bake::bake(
            "test.json",
            &name,
            tris,
            Vec3::new(0.0, 2.0, 0.0),
            Vec3::new(8.0, 4.0, 8.0),
            &volume,
            &params,
        );
        gi::evaluate(
            &baked,
            &volume,
            &scene.lights().resolved(),
            &scene.environment,
        )
    });

    offscreen::render_with_adapter(
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
        &scene.hud_tree(&BuiltinAssets),
        &[],
        field.as_ref(),
    )
    .expect("offscreen render failed")
    .0
}

/// `intensity: 0.0` is a one-field A/B against the pre-M35 look — and it is
/// **byte for byte**, not approximately.
///
/// This is the strongest statement available about the seam without running the
/// `ab-check` skill, and it says more than a shader-source assertion could: the
/// two images come from *different compiled pipelines*. The left one is the GI
/// variant, carrying `gi.wgsl`, four extra bindings and three extra uniform
/// fields; the right is `mesh.wgsl` exactly as it sits on disk. That they agree
/// to the byte means the reassembled `AMBIENT` and `FILL` lines reach the
/// compiler in a form that contracts to the same arithmetic — which is the
/// property CLAUDE.md flags as ULP-sensitive in four places, and the one that
/// has been broken three separate times by restructurings that were equal on
/// paper.
///
/// If this ever fails, the answer is M26's refraction lesson: a separate
/// variant, not a shared branch.
#[test]
fn gi_at_zero_intensity_is_byte_identical_to_no_gi_at_all() {
    if !gpu_available() {
        return;
    }
    let with_field = render(0.0, true);
    let without = render(0.0, false);
    assert_eq!(
        with_field.pixels, without.pixels,
        "a weight of zero must land on the pre-M35 expression exactly, not nearly"
    );
}

/// The shader and `IrradianceField::sample` compute the same number.
///
/// M41's arrangement — a CPU evaluator held to the shader by a test that reads
/// the drawn pixel back — applied to light instead of to water. Without it the
/// two are only *intended* to agree, and `engine gi-probe` becomes a second
/// model that drifts from the one that draws.
///
/// The scene is chosen so the prediction is exact rather than approximate. With
/// `sky: false`, no shadows, an opaque surface and a sun at zero intensity, the
/// entire `if shadowed || lit_sky || blended` branch is skipped and the fragment
/// is `direct + ambient + emissive` with the first and last at zero. So the
/// pixel is `albedo * gi_fill(...)` and nothing else: no GGX lobe, no sky
/// reflection, no fog. Anything that disagrees here is the shader disagreeing
/// with the fold, with nowhere for the difference to hide.
///
/// The tolerance is 3/255 but the measured difference on this adapter is **0**
/// — both sample points, all three channels. It is left loose rather than
/// tightened to zero because the field is `Rgba16Float` and the fetch is
/// hardware trilinear: half precision and a driver's interpolation are exactly
/// the two things this repo has repeatedly found to be per-adapter. A byte of
/// slack is the difference between a test that fails on someone else's GPU and
/// one that catches a wrong reconstruction, which would be off by far more.
#[test]
fn the_shader_and_the_cpu_evaluator_agree() {
    if !gpu_available() {
        return;
    }
    // Two thin walls forming a slot on the left, rather than a ceiling.
    //
    // A ceiling is the obvious way to occlude a probe and the wrong one here:
    // the camera looks straight down, so a ceiling's *top* is what fills those
    // pixels, and the prediction would be compared against a surface two metres
    // above the one it was computed for. Walls occlude the floor between them
    // while leaving it visible from above, which is what makes the pixel and the
    // prediction describe the same square centimetre.
    //
    // The volume's floor sits a metre *below* the ground so the sampled points
    // are strictly inside it: with `blend: 0` a point exactly on the boundary
    // has weight zero, and the test would silently be comparing the fallback.
    let source = r#"{
  "name": "gi agreement",
  "environment": { "sky": false, "samples": 1 },
  "entities": [
    { "name": "Camera", "components": [
      { "type": "Transform", "position": [0.0, 6.0, 0.0], "rotation": [-90.0, 0.0, 0.0] },
      { "type": "Camera", "active": true } ] },
    { "name": "Sun", "components": [
      { "type": "Transform", "rotation": [-90.0, 0.0, 0.0] },
      { "type": "DirectionalLight", "color": [1.0, 1.0, 1.0], "intensity": 0.0 } ] },
    { "name": "Fill", "components": [
      { "type": "AmbientLight", "color": [1.0, 1.0, 1.0], "intensity": 0.5 } ] },
    { "name": "Floor", "components": [
      { "type": "Transform", "position": [0.0, -0.1, 0.0], "scale": [10.0, 0.2, 10.0] },
      { "type": "Mesh", "asset": "builtin:cube" },
      { "type": "Material", "albedo": [0.6, 0.6, 0.6], "roughness": 1.0 } ] },
    { "name": "WallOuter", "components": [
      { "type": "Transform", "position": [-2.5, 1.0, 0.0], "scale": [0.15, 2.0, 8.0] },
      { "type": "Mesh", "asset": "builtin:cube" },
      { "type": "Material", "albedo": [0.5, 0.5, 0.5], "roughness": 1.0 } ] },
    { "name": "WallInner", "components": [
      { "type": "Transform", "position": [-0.9, 1.0, 0.0], "scale": [0.15, 2.0, 8.0] },
      { "type": "Mesh", "asset": "builtin:cube" },
      { "type": "Material", "albedo": [0.5, 0.5, 0.5], "roughness": 1.0 } ] },
    { "name": "Lighting", "components": [
      { "type": "Transform", "position": [0.0, 1.5, 0.0], "scale": [8.0, 5.0, 8.0] },
      { "type": "LightProbeVolume", "spacing": 0.5, "bake": "gi/t.gi.json",
        "bounces": 1, "intensity": 1.0, "blend": 0.0 } ] }
  ]
}"#;

    let scene = Scene::from_source_ignoring(source, "test.json", &["gi_bake_missing"])
        .expect("agreement scene should be valid apart from the absent bake");
    let (camera, camera_transform) = scene.camera(None).expect("needs a camera");
    let items = scene.render_items(&BuiltinAssets).expect("builtins only");
    let (name, volume, _) = gi::evaluate::rendered_volume(&scene).expect("has a volume");
    let tris = bake::collect_occluders(&scene, &BuiltinAssets).expect("builtins only");
    let (baked, _) = bake::bake(
        "test.json",
        &name,
        tris,
        Vec3::new(0.0, 1.5, 0.0),
        Vec3::new(8.0, 5.0, 8.0),
        &volume,
        &bake::BakeParams {
            samples: 256,
            bounces: 1,
        },
    );
    let field = gi::evaluate(
        &baked,
        &volume,
        &scene.lights().resolved(),
        &scene.environment,
    );

    let image = offscreen::render_with_adapter(
        &items,
        &[],
        &[],
        &[],
        &[],
        &[],
        &camera,
        camera_transform.matrix(),
        scene.lights().resolved(),
        scene.environment,
        0.0,
        SIZE,
        SIZE,
        &scene.hud_tree(&BuiltinAssets),
        &[],
        Some(&field),
    )
    .expect("offscreen render failed")
    .0;

    // The camera looks straight down from (0, 6, 0) at a 10 m floor, so the
    // frame's horizontal axis is world X and its vertical axis is world Z. Two
    // samples: one on the floor inside the slot, one on open floor. Both are on
    // the floor's top face, whose normal is +Y.
    // `Camera.fov` defaults to 60 degrees and is *vertical*; the frame is
    // square, so the same half-extent covers both axes. The floor's top face is
    // at y = 0 and the camera at y = 6, so the plane is exactly 6 m away.
    let half_extent = 6.0 * (60.0f32.to_radians() * 0.5).tan();
    for (px, py) in [(SIZE / 4, SIZE / 2), (3 * SIZE / 4, SIZE / 2)] {
        let ndc_x = (px as f32 + 0.5) / SIZE as f32 * 2.0 - 1.0;
        let ndc_y = 1.0 - (py as f32 + 0.5) / SIZE as f32 * 2.0;
        let at = Vec3::new(ndc_x * half_extent, 0.0, -ndc_y * half_extent);

        let expected = field.sample(at, Vec3::Y) * 0.6;
        let i = ((py * SIZE + px) * 4) as usize;
        for (channel, want) in expected.to_array().iter().enumerate() {
            let got = image.pixels[i + channel];
            let predicted = srgb_encode(*want);
            assert!(
                got.abs_diff(predicted) <= 3,
                "pixel ({px}, {py}) channel {channel}: shader gave {got}, the CPU \
                 evaluator predicts {predicted} (linear {want}) at world {at:?}"
            );
        }
    }
}

/// The sRGB transfer function — the exact curve `Rgba8UnormSrgb` applies on
/// write, so a linear prediction and a PNG byte are comparable.
fn srgb_encode(linear: f32) -> u8 {
    let clamped = linear.clamp(0.0, 1.0);
    let encoded = if clamped <= 0.003_130_8 {
        clamped * 12.92
    } else {
        1.055 * clamped.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}

/// And with GI on, the picture actually changes — in the direction occlusion
/// predicts.
///
/// Paired with the test above this is the whole claim: the feature is off when
/// it says it is off, and doing something when it says it is on. A test that
/// only checked the first would pass on a producer that was spliced in and
/// silently did nothing, which is the exact failure the seam exists to prevent.
#[test]
fn gi_darkens_what_the_wall_shelters() {
    if !gpu_available() {
        return;
    }
    let lit = render(1.0, true);
    let flat = render(0.0, true);

    // Mean luminance over the lower-left quadrant: floor in the red wall's
    // pocket, where the wall blocks roughly half the sky.
    let mean = |image: &Image| -> f64 {
        let mut sum = 0u64;
        let mut count = 0u64;
        for y in (SIZE / 2)..SIZE {
            for x in 0..(SIZE / 3) {
                let i = ((y * SIZE + x) * 4) as usize;
                sum += image.pixels[i] as u64
                    + image.pixels[i + 1] as u64
                    + image.pixels[i + 2] as u64;
                count += 3;
            }
        }
        sum as f64 / count as f64
    };

    let with_gi = mean(&lit);
    let without = mean(&flat);
    assert!(
        with_gi < without,
        "a sheltered floor must be darker with GI on: {with_gi} vs {without}"
    );
}
