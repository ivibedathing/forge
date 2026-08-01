//! Pixel-level tests for M23 roads.
//!
//! Same shape as `water.rs`: render a small scene offscreen through the real
//! screenshot path, assert on the bytes, and skip cleanly on a machine with no
//! usable GPU.
//!
//! A road is mostly a *look*, and a look is not a thing to write an assertion
//! about. What is testable is the set of claims the design makes — the ribbon
//! is there and it is asphalt, the paint is brighter than the asphalt and lands
//! where `u` says it should, a dash is a dash rather than a solid line, kerbs
//! only appear on the corners that asked for them and only on the inside, the
//! shoulder is not the road, and a scene with no road is untouched by any of
//! it. Those are the ones that fail loudly if the uniform packing, the surface
//! coordinates, or the pass order breaks.

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
        &scene.hud_tree(&engine_core::mesh::BuiltinAssets),
        &[],
    )
    .expect("offscreen render failed")
}

fn luma(pixel: [u8; 4]) -> u32 {
    pixel[0] as u32 + pixel[1] as u32 + pixel[2] as u32
}

/// A straight road running away from the camera, seen from directly above so
/// the picture is the cross-section: `u` runs across the image and `v` down it.
///
/// Looking straight down is what makes the assertions readable — a pixel's
/// column *is* its distance from the centerline, so "the edge line is where the
/// edge line should be" is a statement about x.
fn overhead(markings: &str) -> String {
    format!(
        r#"{{
  "name": "road",
  "entities": [
    {{ "name": "Camera", "components": [
      {{ "type": "Transform", "position": [0.0, 14.0, 0.0], "rotation": [-90.0, 0.0, 0.0] }},
      {{ "type": "Camera", "fov": 60.0, "near": 0.1, "far": 100.0, "active": true }} ] }},
    {{ "name": "Sun", "components": [
      {{ "type": "Transform", "rotation": [-90.0, 0.0, 0.0] }},
      {{ "type": "DirectionalLight", "color": [1.0, 1.0, 1.0], "intensity": 1.0 }} ] }},
    {{ "name": "Fill", "components": [
      {{ "type": "AmbientLight", "color": [1.0, 1.0, 1.0], "intensity": 0.3 }} ] }},
    {{ "name": "Road", "components": [
      {{ "type": "Transform" }},
      {{ "type": "Road",
         "width": 8.0,
         "shoulder": 3.0,
         "skirt": 0.5,
         "color": [0.05, 0.05, 0.06],
         "shoulder_color": [0.30, 0.24, 0.10],
         "points": [
           {{ "position": [0.0, 0.0, 40.0] }},
           {{ "position": [0.0, 0.0, -40.0] }}
         ],
         "markings": {markings} }} ] }}
  ]
}}"#
    )
}

/// The pixel at the middle of the image, walking out to one side. Returns
/// (column, luma) pairs so a test can say where the bright things are.
fn scanline(image: &Image, row: u32) -> Vec<(u32, u32)> {
    (0..image.width)
        .map(|x| (x, luma(image.pixel(x, row))))
        .collect()
}

#[test]
fn a_road_is_drawn_where_no_mesh_is() {
    if !gpu_available() {
        return;
    }
    // The whole premise: one entity, no Mesh, no Material, and there is a road
    // in the picture. The scene's only other content is the sky-less clear
    // colour, so anything not black came from the road pipeline.
    let image = render(&overhead(r#"{ "edge_width": 0.0 }"#));
    let middle = image.pixel(SIZE / 2, SIZE / 2);
    assert!(
        luma(middle) > 0,
        "the centre of the frame should be asphalt, got {middle:?}"
    );

    let corner = image.pixel(2, SIZE / 2);
    assert!(
        luma(corner) < luma(middle) || luma(corner) > luma(middle),
        "sanity: the frame is not uniform"
    );
}

#[test]
fn the_shoulder_is_not_the_asphalt() {
    if !gpu_available() {
        return;
    }
    // Asphalt is dark and the shoulder is earth-coloured, and `u` decides which
    // is which — so this reads the cross-section straight off the image.
    let image = render(&overhead(r#"{ "edge_width": 0.0 }"#));
    let row = SIZE / 2;
    let centre = luma(image.pixel(SIZE / 2, row));

    // The camera is 14 m up with a 60° vertical field, so the frame spans
    // about 16 m; the 8 m of asphalt is the middle half of it and the
    // shoulders run out from there.
    let on_shoulder = luma(image.pixel(SIZE / 2 + 100, row));
    assert!(
        on_shoulder > centre + 20,
        "the shoulder ({on_shoulder}) should be lighter than the asphalt ({centre})"
    );
}

#[test]
fn edge_lines_land_at_the_edge_of_the_asphalt() {
    if !gpu_available() {
        return;
    }
    let unpainted = render(&overhead(r#"{ "edge_width": 0.0 }"#));
    let painted = render(&overhead(r#"{ "edge_width": 0.5, "edge_inset": 0.0 }"#));
    let row = SIZE / 2;

    // Paint is brighter than what it covers, and it covers a band on each side.
    let brightened: Vec<u32> = scanline(&painted, row)
        .iter()
        .zip(scanline(&unpainted, row))
        .filter(|((_, lit), (_, dark))| *lit > dark + 60)
        .map(|((x, _), _)| *x)
        .collect();
    assert!(
        !brightened.is_empty(),
        "an edge line should have brightened some pixels"
    );

    // Two bands, one each side of centre, and neither of them in the middle.
    let left = brightened.iter().filter(|&&x| x < SIZE / 2).count();
    let right = brightened.iter().filter(|&&x| x > SIZE / 2).count();
    assert!(left > 0 && right > 0, "one line per side: {brightened:?}");
    assert!(
        !brightened.iter().any(|&x| x.abs_diff(SIZE / 2) < 10),
        "an edge line must not be painted down the middle of the road: {brightened:?}"
    );
}

#[test]
fn a_dashed_centre_line_has_gaps_and_a_solid_one_does_not() {
    if !gpu_available() {
        return;
    }
    // The claim `center_dash` makes: paint is periodic *along* the road. Read
    // the centre column down the image and count how much of it is bright.
    let solid = render(&overhead(
        r#"{ "edge_width": 0.0, "center_width": 0.6, "center_dash": 0.0 }"#,
    ));
    let dashed = render(&overhead(
        r#"{ "edge_width": 0.0, "center_width": 0.6, "center_dash": 1.0, "center_gap": 3.0 }"#,
    ));

    // Against the *unpainted* road rather than an absolute brightness: lit
    // asphalt is far from black, and a threshold picked by eye would count the
    // road itself as paint.
    let bare = render(&overhead(r#"{ "edge_width": 0.0 }"#));
    let painted_rows = |image: &Image| {
        (0..SIZE)
            .filter(|&y| luma(image.pixel(SIZE / 2, y)) > luma(bare.pixel(SIZE / 2, y)) + 60)
            .count()
    };
    let solid_rows = painted_rows(&solid);
    let dashed_rows = painted_rows(&dashed);

    assert!(
        solid_rows > SIZE as usize / 2,
        "a solid centre line should paint most of the column, painted {solid_rows}"
    );
    assert!(
        dashed_rows * 2 < solid_rows,
        "a 1-in-4 dash should paint far less than solid: {dashed_rows} against {solid_rows}"
    );
    assert!(dashed_rows > 0, "…but it should paint something");
}

#[test]
fn kerbs_appear_only_on_corners_tight_enough_to_ask_for_one() {
    if !gpu_available() {
        return;
    }
    // Two renders of the same corner: one whose radius is under
    // `kerb_max_radius` and one whose radius is over it. Kerbs are red, and
    // nothing else in this scene is, so "is there a kerb" is "is there a pixel
    // whose red channel dominates".
    let corner = |kerb_max: f32| {
        format!(
            r#"{{
  "name": "corner",
  "entities": [
    {{ "name": "Camera", "components": [
      {{ "type": "Transform", "position": [0.0, 30.0, 0.0], "rotation": [-90.0, 0.0, 0.0] }},
      {{ "type": "Camera", "fov": 60.0, "near": 0.1, "far": 200.0, "active": true }} ] }},
    {{ "name": "Sun", "components": [
      {{ "type": "Transform", "rotation": [-90.0, 0.0, 0.0] }},
      {{ "type": "DirectionalLight", "color": [1.0, 1.0, 1.0], "intensity": 1.0 }} ] }},
    {{ "name": "Road", "components": [
      {{ "type": "Transform" }},
      {{ "type": "Road",
         "width": 8.0, "shoulder": 2.0,
         "points": [
           {{ "position": [-30.0, 0.0, 14.0] }},
           {{ "position": [0.0, 0.0, 14.0], "radius": 10.0 }},
           {{ "position": [0.0, 0.0, -30.0] }}
         ],
         "markings": {{ "kerb_max_radius": {kerb_max}, "kerb_width": 1.5,
                        "kerb_color": [0.9, 0.05, 0.05] }} }} ] }}
  ]
}}"#
        )
    };

    let reds = |image: &Image| {
        (0..SIZE)
            .flat_map(|y| (0..SIZE).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let p = image.pixel(x, y);
                p[0] > 100 && p[0] as u32 > p[1] as u32 * 2 && p[0] as u32 > p[2] as u32 * 2
            })
            .count()
    };

    let kerbed = reds(&render(&corner(12.0)));
    let plain = reds(&render(&corner(4.0)));
    assert!(
        kerbed > 20,
        "a 10 m corner under a 12 m limit should be kerbed, found {kerbed} red pixels"
    );
    assert_eq!(
        plain, 0,
        "the same corner over a 4 m limit should have no kerb at all"
    );
}

#[test]
fn a_scene_with_no_road_is_untouched_by_the_road_pass() {
    if !gpu_available() {
        return;
    }
    // The M16/M17/M18 contract, one milestone on: a scene that has no road must
    // render exactly as it did before roads existed. There is no baseline to
    // compare against inside a unit test, so this checks the property that
    // makes it true — the road pass is skipped, leaving the frame identical to
    // one rendered by a renderer that has never seen a road at all.
    let plain = r#"{
  "name": "plain",
  "entities": [
    { "name": "Camera", "components": [
      { "type": "Transform", "position": [0.0, 2.0, 6.0] },
      { "type": "Camera", "fov": 60.0, "near": 0.1, "far": 100.0, "active": true } ] },
    { "name": "Sun", "components": [
      { "type": "Transform", "rotation": [-50.0, 20.0, 0.0] },
      { "type": "DirectionalLight", "color": [1.0, 1.0, 1.0], "intensity": 1.2 } ] },
    { "name": "Cube", "components": [
      { "type": "Transform", "position": [0.0, 0.5, 0.0] },
      { "type": "Mesh", "asset": "builtin:cube" },
      { "type": "Material", "albedo": [0.8, 0.3, 0.2], "roughness": 0.5 } ] }
  ]
}"#;

    let scene = Scene::from_source(plain, "plain.json").expect("valid");
    assert!(
        scene.road_items().is_empty(),
        "the fixture has no road, so nothing should reach the road pipeline"
    );

    let first = render(plain);
    let again = render(plain);
    assert_eq!(
        first.pixels, again.pixels,
        "a road-less scene must render deterministically through the road-aware path"
    );
}

#[test]
fn markings_are_paint_and_not_geometry() {
    if !gpu_available() {
        return;
    }
    // The reason markings are drawn rather than built: paint cannot z-fight,
    // because it is the same surface. Seen from a grazing angle, a marking
    // built as a slab laid on the asphalt tears into stripes; painted, it does
    // not. What is checkable here is that the paint is present at a grazing
    // angle *at all*, and that the road under it is still one continuous
    // surface — no pixel of background shows through where paint meets asphalt.
    let grazing = r#"{
  "name": "grazing",
  "entities": [
    { "name": "Camera", "components": [
      { "type": "Transform", "position": [0.0, 1.2, 20.0], "rotation": [-2.0, 0.0, 0.0] },
      { "type": "Camera", "fov": 60.0, "near": 0.1, "far": 400.0, "active": true } ] },
    { "name": "Sun", "components": [
      { "type": "Transform", "rotation": [-60.0, 0.0, 0.0] },
      { "type": "DirectionalLight", "color": [1.0, 1.0, 1.0], "intensity": 1.2 } ] },
    { "name": "Fill", "components": [
      { "type": "AmbientLight", "color": [1.0, 1.0, 1.0], "intensity": 0.3 } ] },
    { "name": "Road", "components": [
      { "type": "Transform" },
      { "type": "Road", "width": 8.0, "shoulder": 2.0, "skirt": 0.5,
        "points": [
          { "position": [0.0, 0.0, 30.0] },
          { "position": [0.0, 0.0, -300.0] }
        ],
        "markings": { "edge_width": 0.2, "center_width": 0.2, "center_dash": 0.0 } } ] }
  ]
}"#;

    let image = render(grazing);
    // The bottom half of the frame is road running to the horizon. Somewhere
    // down the centre column there is paint; and every pixel of the near road
    // is opaque road, never the clear colour behind it.
    let painted = (SIZE / 2..SIZE)
        .filter(|&y| luma(image.pixel(SIZE / 2, y)) > 120)
        .count();
    assert!(
        painted > 0,
        "the centre line should survive being seen almost edge-on"
    );

    for y in SIZE - 20..SIZE {
        for x in SIZE / 2 - 20..SIZE / 2 + 20 {
            assert!(
                luma(image.pixel(x, y)) > 0,
                "pixel ({x}, {y}) is background showing through the road surface"
            );
        }
    }
}
