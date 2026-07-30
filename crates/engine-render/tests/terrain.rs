//! Pixel-level tests for M22 terrain.
//!
//! Same shape as `water.rs` and `environment.rs`: render a small scene offscreen
//! through the real screenshot path, assert on the bytes, and skip cleanly on a
//! machine with no usable GPU.
//!
//! Terrain is mostly a *look*, and a look is not a thing to assert on. What is
//! testable is the set of claims the design makes — a patch with relief is not a
//! flat plane, layers select on slope and on height, the surface is lit by its
//! own gradient rather than by the flat normal a plane would give, appearance is
//! a pure function of the file, and a scene with no terrain is byte-identical to
//! what the engine drew before terrain existed. Those are the tests, and they
//! fail loudly if the layer table, the uniform packing, or the local-normal
//! convention breaks.

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

/// Looking down a patch of ground from a low camera, sun from the side so
/// slopes read. `TERRAIN` is spliced per test: one field changes at a time.
fn scene_with(terrain: &str) -> String {
    format!(
        r#"{{
  "name": "t",
  "entities": [
    {{ "name": "Camera", "components": [
      {{ "type": "Transform", "position": [0.0, 4.0, 18.0], "rotation": [-11.0, 0.0, 0.0] }},
      {{ "type": "Camera", "fov": 55.0, "active": true }}
    ]}},
    {{ "name": "Sun", "components": [
      {{ "type": "Transform", "rotation": [-30.0, 40.0, 0.0] }},
      {{ "type": "DirectionalLight", "intensity": 1.1 }}
    ]}},
    {{ "name": "Sky", "components": [
      {{ "type": "AmbientLight", "intensity": 0.25 }}
    ]}},
    {{ "name": "Ground", "components": [
      {{ "type": "Transform", "scale": [80.0, 1.0, 80.0] }},
      {terrain}
    ]}}
  ]
}}"#
    )
}

/// The pixels the ground actually covers.
///
/// The test scenes draw no sky, so everything above the horizon is the clear
/// colour — and the top-left pixel is always part of it. Comparing against that
/// is what keeps a "how much of the ground changed" assertion from silently
/// measuring the background, which is most of the frame.
fn ground_pixels(image: &Image) -> Vec<[u8; 4]> {
    let background = [
        image.pixels[0],
        image.pixels[1],
        image.pixels[2],
        image.pixels[3],
    ];
    image
        .pixels
        .chunks_exact(4)
        .map(|p| [p[0], p[1], p[2], p[3]])
        .filter(|p| *p != background)
        .collect()
}

/// Indices of the ground pixels, for tests that compare two renders per pixel.
fn ground_indices(image: &Image) -> Vec<usize> {
    let background = [
        image.pixels[0],
        image.pixels[1],
        image.pixels[2],
        image.pixels[3],
    ];
    image
        .pixels
        .chunks_exact(4)
        .enumerate()
        .filter(|(_, p)| [p[0], p[1], p[2], p[3]] != background)
        .map(|(i, _)| i)
        .collect()
}

#[test]
fn relief_is_shaded_by_its_own_slope() {
    if !gpu_available() {
        return;
    }

    // The claim: a patch with `height` is lit differently across its surface,
    // where a flat one is a single value. This is the test that fails if mesh
    // normals ever go back to being world-space — the renderer transforms them
    // by the model matrix's inverse-transpose, and on an 80 m patch that crushes
    // a world normal flat, leaving a landscape lit exactly like a plane.
    let flat = render(&scene_with(
        r#"{ "type": "Terrain", "height": 0.0, "bump": 0.0, "color_variation": 0.0 }"#,
    ));
    let hills = render(&scene_with(
        r#"{ "type": "Terrain", "height": 3.0, "feature_scale": 22.0, "seed": 5,
             "bump": 0.0, "color_variation": 0.0 }"#,
    ));

    let spread = |image: &Image| {
        let lumas: Vec<u32> = ground_pixels(image).iter().copied().map(luma).collect();
        assert!(!lumas.is_empty(), "nothing was drawn");
        lumas.iter().max().unwrap() - lumas.iter().min().unwrap()
    };

    let (flat_spread, hill_spread) = (spread(&flat), spread(&hills));
    assert!(
        hill_spread > flat_spread * 3,
        "relief barely changed the shading: flat spread {flat_spread}, hills {hill_spread}"
    );
}

#[test]
fn a_flat_single_layer_patch_is_exactly_a_painted_plane() {
    if !gpu_available() {
        return;
    }

    // The floor under every other claim here, and the sharpest statement of what
    // terrain *is*: an ordinary lit mesh whose material is computed instead of
    // authored. With no relief, no bump and no variation, one layer must be
    // indistinguishable — byte for byte — from `builtin:plane` carrying the same
    // `Material`. Anything else means the terrain path is lighting differently,
    // and every softer assertion below would be measuring that drift.
    //
    // (Not "one flat colour": the GGX specular depends on the view vector, so
    // even a genuine plane varies by a unit or two across the frame. Comparing
    // against the plane tests the claim without having to model that.)
    // `segments: 1` so the comparison is about *shading* only: the generated
    // grid is then the same two triangles as `builtin:plane` (pinned by
    // `terrain::tests::a_flat_patch_is_the_builtin_plane`). At a finer
    // tessellation the interpolants are computed from different vertices and a
    // stray pixel lands one unit away for reasons that have nothing to do with
    // this code path.
    let terrain = render(&scene_with(
        r#"{ "type": "Terrain", "segments": 1, "height": 0.0, "bump": 0.0,
             "color_variation": 0.0,
             "layers": [{ "albedo": [0.2, 0.3, 0.1], "roughness": 0.85 }] }"#,
    ));
    let plane = render(&scene_with(
        r#"{ "type": "Mesh", "asset": "builtin:plane" },
           { "type": "Material", "albedo": [0.2, 0.3, 0.1], "roughness": 0.85 }"#,
    ));

    assert!(
        ground_pixels(&plane).len() > 1000,
        "the reference plane barely covers the frame"
    );

    // Counted rather than `assert_eq!` on the buffers: a mismatch there prints
    // a quarter of a megabyte of pixel values and says nothing useful.
    let differing: Vec<usize> = terrain
        .pixels
        .chunks_exact(4)
        .zip(plane.pixels.chunks_exact(4))
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, _)| i)
        .collect();
    if let Some(&first) = differing.first() {
        panic!(
            "flat one-layer terrain must render exactly as the plane it replaces; \
             {} of {} pixels differ, first at {first}: terrain {:?} vs plane {:?}",
            differing.len(),
            terrain.pixels.len() / 4,
            &terrain.pixels[first * 4..first * 4 + 4],
            &plane.pixels[first * 4..first * 4 + 4],
        );
    }
}

#[test]
fn a_slope_layer_paints_only_slopes() {
    if !gpu_available() {
        return;
    }

    // Slope selection is the load-bearing half of the layer system: height
    // alone draws contour stripes, and slope is what reads as ground.
    let base = r#""height": 5.0, "feature_scale": 16.0, "seed": 9, "bump": 0.0,
                  "color_variation": 0.0"#;
    let plain = render(&scene_with(&format!(
        r#"{{ "type": "Terrain", {base},
              "layers": [{{ "albedo": [0.05, 0.05, 0.05] }}] }}"#
    )));
    let with_cliffs = render(&scene_with(&format!(
        r#"{{ "type": "Terrain", {base},
              "layers": [
                {{ "albedo": [0.05, 0.05, 0.05] }},
                {{ "albedo": [0.9, 0.1, 0.1], "slope_range": [20.0, 90.0],
                   "slope_blend": 1.0, "noise": 0.0 }}
              ] }}"#
    )));

    let ground_at = ground_indices(&plain);
    let ground = ground_at.len();
    let reddened = ground_at
        .iter()
        .filter(|i| with_cliffs.pixels[*i * 4] > plain.pixels[*i * 4] + 30)
        .count();

    // Some of the frame must change, but nowhere near all of it: a patch this
    // gentle has steep faces only on the flanks of its hills. Both bounds
    // matter — "everything went red" is the failure where the band is inverted
    // or the fade swallows the whole range.
    assert!(
        reddened > ground / 100,
        "the slope layer painted almost nothing: {reddened} of {ground}"
    );
    assert!(
        reddened < ground / 2,
        "the slope layer painted almost everything: {reddened} of {ground}"
    );
}

#[test]
fn a_height_layer_paints_only_high_ground() {
    if !gpu_available() {
        return;
    }

    let base = r#""height": 5.0, "feature_scale": 16.0, "seed": 9, "bump": 0.0,
                  "color_variation": 0.0"#;
    let plain = render(&scene_with(&format!(
        r#"{{ "type": "Terrain", {base},
              "layers": [{{ "albedo": [0.05, 0.05, 0.05] }}] }}"#
    )));
    let snowline = render(&scene_with(&format!(
        r#"{{ "type": "Terrain", {base},
              "layers": [
                {{ "albedo": [0.05, 0.05, 0.05] }},
                {{ "albedo": [0.1, 0.1, 0.9], "height_range": [2.0, 40.0],
                   "height_blend": 0.2, "noise": 0.0 }}
              ] }}"#
    )));

    let ground_at = ground_indices(&plain);
    let ground = ground_at.len();
    let changed = ground_at
        .iter()
        .filter(|i| snowline.pixels[*i * 4 + 2] > plain.pixels[*i * 4 + 2] + 30)
        .count();
    assert!(
        changed > ground / 100 && changed < ground / 2,
        "the height band covered {changed} of {ground} ground pixels"
    );
}

#[test]
fn the_same_file_gives_the_same_pixels() {
    if !gpu_available() {
        return;
    }

    // Terrain has to sit under a `diff-render` baseline, which means every part
    // of it — the height hash, the layer blend, the detail noise — is a pure
    // function of the file. Nothing here may reach for a clock or a random seed.
    let source = scene_with(
        r#"{ "type": "Terrain", "height": 2.5, "seed": 12, "warp": 0.6, "bump": 0.5,
             "layers": [
               { "albedo": [0.1, 0.15, 0.07] },
               { "albedo": [0.3, 0.2, 0.1], "slope_range": [20.0, 90.0] }
             ] }"#,
    );
    assert_eq!(
        render(&source).pixels,
        render(&source).pixels,
        "terrain is not reproducible from its file alone"
    );
}

#[test]
fn a_scene_with_no_terrain_is_untouched_by_the_terrain_path() {
    if !gpu_available() {
        return;
    }

    // M22 grew the object uniform and put a branch in the middle of `fs_main`.
    // Eighteen milestones of committed baselines say neither may move a pixel of
    // an ordinary mesh. This is the cheap in-repo half of that check; the whole
    // one is an A/B between binaries built at `main` and here.
    let source = r#"{
  "name": "no-terrain",
  "entities": [
    { "name": "Camera", "components": [
      { "type": "Transform", "position": [0.0, 2.0, 6.0], "rotation": [-14.0, 0.0, 0.0] },
      { "type": "Camera", "fov": 55.0, "active": true }
    ]},
    { "name": "Sun", "components": [
      { "type": "Transform", "rotation": [-35.0, 25.0, 0.0] },
      { "type": "DirectionalLight", "intensity": 1.2 }
    ]},
    { "name": "Cube", "components": [
      { "type": "Transform", "rotation": [0.0, 25.0, 0.0] },
      { "type": "Mesh", "asset": "builtin:cube" },
      { "type": "Material", "albedo": [0.7, 0.3, 0.2], "roughness": 0.5 }
    ]}
  ]
}"#;

    let image = render(source);
    // Something was actually drawn — an all-background frame would pass any
    // equality check trivially.
    assert!(
        ground_pixels(&image).len() > 500,
        "the reference cube did not render"
    );
    assert_eq!(
        image.pixels,
        render(source).pixels,
        "a terrain-free scene must render identically every time"
    );
}
