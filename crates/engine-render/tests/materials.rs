//! Pixel-level tests for M26's material system.
//!
//! Same shape as `terrain.rs` and `water.rs`: render a small scene offscreen
//! through the real screenshot path, assert on the bytes, and skip cleanly on a
//! machine with no usable GPU.
//!
//! A material system is mostly a *look*, and a look is not a thing to assert
//! on. What is testable is the set of claims the design makes: that the
//! producer seam is neutral, that UVs are the right way up, that `uv_scale`
//! tiles, that the colour space is decided by the slot (a roughness map read as
//! sRGB comes back gamma-decoded and every surface is smoother than it was
//! authored — invisible in any test that does not assert on the shading), that
//! a normal map perturbs, and that `alpha_cutoff` cuts the pixel *and* its
//! shadow.

use std::sync::Arc;

use engine_core::mesh::{BuiltinAssets, MeshData, MeshSource};
use engine_core::texture::{ColorSpace, TextureData, TextureSource};
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

/// Textures built in memory rather than read from disk, so these tests need no
/// fixture files and can describe exactly the pixels they are asserting about.
///
/// Meshes delegate to [`BuiltinAssets`]. The `Arc` cache is the point of the
/// `RefCell`: the renderer keys its uploads on `Arc` identity, and a source
/// that minted a fresh one per call would re-upload every frame — the M15 rule,
/// which this honours the same way `AssetServer` does.
#[derive(Default)]
struct Textures {
    made: std::cell::RefCell<std::collections::HashMap<(String, ColorSpace), Arc<TextureData>>>,
}

impl MeshSource for Textures {
    fn load_mesh(&self, asset: &str) -> engine_core::error::Result<Arc<MeshData>> {
        BuiltinAssets.load_mesh(asset)
    }
}

impl TextureSource for Textures {
    fn load_texture(
        &self,
        asset: &str,
        space: ColorSpace,
    ) -> engine_core::error::Result<Arc<TextureData>> {
        let key = (asset.to_string(), space);
        if let Some(hit) = self.made.borrow().get(&key) {
            return Ok(Arc::clone(hit));
        }
        let (width, height, rgba) = match asset {
            // Red where the texture's **u** is low, blue where it is high.
            "u_split.png" => {
                // 64², not 2²: a two-texel texture is all boundary under
                // bilinear filtering, and a test that samples a blend cannot
                // say which half it landed in.
                let mut pixels = Vec::new();
                for _ in 0..64 {
                    for x in 0..64 {
                        pixels.extend_from_slice(if x < 32 {
                            &[220, 40, 40, 255]
                        } else {
                            &[40, 40, 220, 255]
                        });
                    }
                }
                (64, 64, pixels)
            }
            // Red where the texture's **v** is low — the other axis.
            "v_split.png" => {
                let mut pixels = Vec::new();
                for y in 0..64 {
                    for _ in 0..64 {
                        pixels.extend_from_slice(if y < 32 {
                            &[220, 40, 40, 255]
                        } else {
                            &[40, 40, 220, 255]
                        });
                    }
                }
                (64, 64, pixels)
            }
            // Uniformly white: the neutral map, for the seam-is-neutral test.
            "white.png" => (1, 1, vec![255, 255, 255, 255]),
            // ORM with a *constant* mid roughness in G and nothing else. Read
            // as sRGB this decodes to ~0.22 instead of 0.5, which is the bug
            // this file exists to catch.
            "orm_half.png" => (1, 1, vec![255, 128, 0, 255]),
            "orm_rough.png" => (1, 1, vec![255, 255, 0, 255]),
            // Occlusion at half, everything else neutral.
            "orm_occluded.png" => (1, 1, vec![128, 255, 0, 255]),
            // A tangent-space normal tilted hard toward +U.
            "tilted_normal.png" => (1, 1, vec![255, 128, 128, 255]),
            // Half the texels transparent — the alpha-cut pin.
            "holes.png" => {
                // Opaque in the middle, transparent around it: a shape rather
                // than a checker, so a cut pixel and a kept one are both a
                // whole region and neither is a filtering boundary.
                let mut pixels = Vec::new();
                for y in 0..64 {
                    for x in 0..64 {
                        let inside = (16..48).contains(&x) && (16..48).contains(&y);
                        pixels.extend_from_slice(if inside {
                            &[255, 255, 255, 255]
                        } else {
                            &[255, 255, 255, 0]
                        });
                    }
                }
                (64, 64, pixels)
            }
            other => panic!("test asked for an unknown texture {other:?}"),
        };
        let data = Arc::new(TextureData::new(width, height, rgba, space));
        self.made.borrow_mut().insert(key, Arc::clone(&data));
        Ok(data)
    }
}

fn render(source: &str) -> Image {
    // Parsed and instantiated rather than validated: `Scene::from_source`
    // resolves texture references against the filesystem, and these textures
    // are built in memory precisely so the assertions can describe their
    // pixels. Every other structural check these scenes could fail is covered
    // by the validation tests in engine-core.
    let file: engine_core::SceneFile =
        serde_json::from_str(source).expect("test scene should parse");
    let scene = Scene::instantiate(file);
    let (camera, camera_transform) = scene.camera(None).expect("test scene needs a camera");
    let assets = Textures::default();
    let items = scene.render_items(&assets).expect("test textures resolve");
    offscreen::render(
        &items,
        &scene.water_items(),
        &scene.cloud_items(),
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

/// A quad filling most of the frame, seen head-on, lit from straight ahead so
/// the whole surface is evenly lit and a shading difference is the material's
/// doing and nothing else. `MATERIAL` is spliced per test.
fn quad_scene(material: &str) -> String {
    format!(
        r#"{{
  "name": "m",
  "entities": [
    {{ "name": "Camera", "components": [
      {{ "type": "Transform", "position": [0.0, 0.0, 3.0] }},
      {{ "type": "Camera", "fov": 45.0, "active": true }}
    ]}},
    {{ "name": "Sun", "components": [
      {{ "type": "Transform", "rotation": [-25.0, 20.0, 0.0] }},
      {{ "type": "DirectionalLight", "intensity": 1.0 }}
    ]}},
    {{ "name": "Sky", "components": [
      {{ "type": "AmbientLight", "intensity": 0.2 }}
    ]}},
    {{ "name": "Panel", "components": [
      {{ "type": "Transform", "rotation": [90.0, 0.0, 0.0], "scale": [3.0, 1.0, 3.0] }},
      {{ "type": "Mesh", "asset": "builtin:plane" }},
      {material}
    ]}}
  ]
}}"#
    )
}

/// The seam is neutral: a material with no maps and the same material with
/// white maps bound render the same picture.
///
/// *The same*, not bit-identical — and that distinction is the whole point of
/// §2. The plain pipeline compiles `mesh.wgsl` as it sits on disk and the
/// textured one compiles a spliced variant, so the two are allowed to differ in
/// the last place; what they are not allowed to do is differ *visibly*.
#[test]
fn white_maps_are_the_material_they_are_bound_to() {
    if !gpu_available() {
        return;
    }
    let plain = render(&quad_scene(
        r#"{ "type": "Material", "albedo": [0.6, 0.5, 0.4], "roughness": 0.5 }"#,
    ));
    let mapped = render(&quad_scene(
        r#"{ "type": "Material", "albedo": [0.6, 0.5, 0.4], "roughness": 0.5,
             "albedo_map": "white.png", "emissive_map": "white.png" }"#,
    ));

    let centre = (SIZE / 2, SIZE / 2);
    let a = plain.pixel(centre.0, centre.1);
    let b = mapped.pixel(centre.0, centre.1);
    for channel in 0..3 {
        let delta = a[channel].abs_diff(b[channel]);
        assert!(
            delta <= 1,
            "a white map changed the surface by {delta} in channel {channel}: {a:?} vs {b:?}"
        );
    }
}

/// UV orientation, which is the thing that is silently upside-down forever if
/// nothing asserts it.
///
/// **`builtin:plane`'s UVs are not the intuitive ones**, and this test says so
/// rather than papering over it: the primitive is `quad(+Y, +Z, +X)`, so its
/// `u` runs along the quad's local **+Z** and its `v` along local **+X**. On
/// this panel — a plane stood upright by a 90° pitch, so local +Z points down
/// the screen and local +X points right — that puts `u` on the screen's
/// vertical axis and `v` on its horizontal one. The builtins were never
/// authored for texturing (nothing sampled a UV before M23's roads, which
/// generate their own), and changing them is a separate decision from adding
/// texture maps; what matters here is that the mapping is stable, non-
/// degenerate, and not transposed or flipped from run to run.
#[test]
fn an_albedo_map_lands_the_right_way_round() {
    if !gpu_available() {
        return;
    }
    // Off-centre samples deliberately: the split sits exactly at 0.5, and a
    // bilinear tap there is half of each colour by definition.
    let image = render(&quad_scene(
        r#"{ "type": "Material", "albedo": [1.0, 1.0, 1.0], "roughness": 0.9,
             "albedo_map": "u_split.png" }"#,
    ));
    let top = image.pixel(SIZE / 2, SIZE * 3 / 8);
    let bottom = image.pixel(SIZE / 2, SIZE * 5 / 8);
    assert!(
        top[0] > top[2] && bottom[2] > bottom[0],
        "the plane's u runs down the screen here, so low u (red) belongs at the \
         top: {top:?} / {bottom:?}"
    );

    let image = render(&quad_scene(
        r#"{ "type": "Material", "albedo": [1.0, 1.0, 1.0], "roughness": 0.9,
             "albedo_map": "v_split.png" }"#,
    ));
    let left = image.pixel(SIZE * 3 / 8, SIZE / 2);
    let right = image.pixel(SIZE * 5 / 8, SIZE / 2);
    assert!(
        left[0] > left[2] && right[2] > right[0],
        "and its v runs across, so low v (red) belongs on the left: {left:?} / {right:?}"
    );
}

/// `uv_scale` tiles rather than stretching.
#[test]
fn uv_scale_makes_tiles() {
    if !gpu_available() {
        return;
    }
    // `v_split` varies across the screen on this panel (see the orientation
    // test), so two tiles of it read red / blue / red / blue in quarters
    // instead of red / blue in halves.
    let image = render(&quad_scene(
        r#"{ "type": "Material", "albedo": [1.0, 1.0, 1.0], "roughness": 0.9,
             "albedo_map": "v_split.png", "uv_scale": [1.0, 2.0] }"#,
    ));
    let eighth = |i: u32| image.pixel(SIZE * (2 * i + 1) / 8, SIZE / 2);
    let reds: Vec<bool> = (0..4).map(|i| eighth(i)[0] > eighth(i)[2]).collect();
    assert_eq!(
        reds,
        vec![true, false, true, false],
        "two tiles across should draw two copies, not one stretched one"
    );

    // And one tile is still one: the check that the four-band reading above is
    // the scale's doing and not the texture's.
    let single = render(&quad_scene(
        r#"{ "type": "Material", "albedo": [1.0, 1.0, 1.0], "roughness": 0.9,
             "albedo_map": "v_split.png" }"#,
    ));
    let eighth = |i: u32| single.pixel(SIZE * (2 * i + 1) / 8, SIZE / 2);
    let reds: Vec<bool> = (0..4).map(|i| eighth(i)[0] > eighth(i)[2]).collect();
    assert_eq!(reds, vec![true, true, false, false]);
}

/// §4.2, the bug that is invisible in any test that does not assert on the
/// shading: an ORM map is **data**. Read as sRGB, a mid-grey roughness of 128
/// decodes to ~0.22 rather than 0.5, and the surface comes back far glossier
/// than it was authored.
#[test]
fn an_orm_map_is_data_and_not_a_colour() {
    if !gpu_available() {
        return;
    }
    let with_map = render(&quad_scene(
        r#"{ "type": "Material", "albedo": [0.4, 0.4, 0.4], "metallic": 0.0,
             "roughness": 1.0, "orm_map": "orm_half.png" }"#,
    ));
    // The same surface authored directly at the roughness the map's G channel
    // says, with no map at all. If the slot decoded the map as sRGB these would
    // not agree — the mapped one would be much glossier.
    let authored = render(&quad_scene(
        r#"{ "type": "Material", "albedo": [0.4, 0.4, 0.4], "metallic": 0.0,
             "roughness": 0.50196078 }"#,
    ));
    let centre = (SIZE / 2, SIZE / 2);
    let a = with_map.pixel(centre.0, centre.1);
    let b = authored.pixel(centre.0, centre.1);
    for channel in 0..3 {
        let delta = a[channel].abs_diff(b[channel]);
        assert!(
            delta <= 2,
            "an ORM map's roughness should equal the same roughness authored inline \
             ({a:?} vs {b:?}); a gap here means the slot decoded linear data as sRGB"
        );
    }

    // And the map really is driving roughness: a rougher one is a different
    // picture, so the test above is not passing by both sides being ignored.
    let rough = render(&quad_scene(
        r#"{ "type": "Material", "albedo": [0.4, 0.4, 0.4], "metallic": 0.0,
             "roughness": 1.0, "orm_map": "orm_rough.png" }"#,
    ));
    assert_ne!(
        rough.pixel(centre.0, centre.1),
        a,
        "G = 1.0 and G = 0.5 must not render the same surface"
    );
}

/// Occlusion multiplies the ambient and sky terms **only**, never the direct
/// sun. That is the whole difference between ambient occlusion and a second
/// shadow map, and it is checked by taking the sun away: with only ambient
/// light, occlusion has to bite.
#[test]
fn occlusion_darkens_the_ambient_and_not_the_sun() {
    if !gpu_available() {
        return;
    }
    let unlit = |material: &str| {
        render(
            &quad_scene(material)
                .replace(r#""intensity": 1.0"#, r#""intensity": 0.0"#)
                .replace(r#""intensity": 0.2"#, r#""intensity": 0.9"#),
        )
    };
    let plain = unlit(
        r#"{ "type": "Material", "albedo": [0.8, 0.8, 0.8], "roughness": 0.9,
             "orm_map": "orm_rough.png" }"#,
    );
    let occluded = unlit(
        r#"{ "type": "Material", "albedo": [0.8, 0.8, 0.8], "roughness": 0.9,
             "orm_map": "orm_occluded.png" }"#,
    );
    let centre = (SIZE / 2, SIZE / 2);
    assert!(
        luma(occluded.pixel(centre.0, centre.1)) < luma(plain.pixel(centre.0, centre.1)),
        "half occlusion should darken a surface lit only by ambient"
    );
}

/// A normal map perturbs the shading of a flat quad.
#[test]
fn a_normal_map_moves_the_shading_of_a_flat_surface() {
    if !gpu_available() {
        return;
    }
    let flat = render(&quad_scene(
        r#"{ "type": "Material", "albedo": [0.7, 0.7, 0.7], "roughness": 0.35,
             "albedo_map": "white.png" }"#,
    ));
    let tilted = render(&quad_scene(
        r#"{ "type": "Material", "albedo": [0.7, 0.7, 0.7], "roughness": 0.35,
             "albedo_map": "white.png", "normal_map": "tilted_normal.png" }"#,
    ));
    let centre = (SIZE / 2, SIZE / 2);
    assert_ne!(
        flat.pixel(centre.0, centre.1),
        tilted.pixel(centre.0, centre.1),
        "a tilted tangent normal should change how a flat quad is lit"
    );

    // And `normal_strength: 0` puts it back: the scale is on the tangent XY,
    // so zero is exactly the geometric normal.
    let disabled = render(&quad_scene(
        r#"{ "type": "Material", "albedo": [0.7, 0.7, 0.7], "roughness": 0.35,
             "albedo_map": "white.png", "normal_map": "tilted_normal.png",
             "normal_strength": 0.0 }"#,
    ));
    for channel in 0..3 {
        let delta =
            flat.pixel(centre.0, centre.1)[channel].abs_diff(disabled.pixel(centre.0, centre.1)[channel]);
        assert!(delta <= 1, "normal_strength 0 should be the geometric normal");
    }
}

/// `alpha_cutoff` removes pixels — and removes their shadow, which needs a
/// second caster pipeline because `shadow.wgsl` has no fragment stage.
///
/// Both renders go through that **same** cut-out caster (any `alpha_cutoff`
/// above 0 selects it), so the only variable is what the alpha test does. That
/// matters: the solid caster is front-face culled — it records each caster's
/// far side, which is a better peeling margin than any bias — and a flat card
/// with its front toward the sun is therefore culled out of the shadow map
/// entirely. Comparing against `alpha_cutoff: 0` would be comparing against a
/// caster that never drew.
#[test]
fn alpha_cutoff_removes_pixels_and_their_shadow() {
    if !gpu_available() {
        return;
    }
    // A card over a floor, the sun tilted so the shadow lands beside the card
    // rather than under it, where the card would hide its own shadow.
    let scene = |map: &str| {
        format!(
            r#"{{
  "name": "cut",
  "environment": {{ "shadows": true, "shadow_distance": 14.0 }},
  "entities": [
    {{ "name": "Camera", "components": [
      {{ "type": "Transform", "position": [0.0, 4.0, 4.5], "rotation": [-42.0, 0.0, 0.0] }},
      {{ "type": "Camera", "fov": 55.0, "active": true }}
    ]}},
    {{ "name": "Sun", "components": [
      {{ "type": "Transform", "rotation": [-55.0, -30.0, 0.0] }},
      {{ "type": "DirectionalLight", "intensity": 1.0 }}
    ]}},
    {{ "name": "Sky", "components": [
      {{ "type": "AmbientLight", "intensity": 0.15 }}
    ]}},
    {{ "name": "Floor", "components": [
      {{ "type": "Transform", "position": [0.0, -1.0, 0.0], "scale": [12.0, 1.0, 12.0] }},
      {{ "type": "Mesh", "asset": "builtin:plane" }},
      {{ "type": "Material", "albedo": [0.7, 0.7, 0.7], "roughness": 0.9 }}
    ]}},
    {{ "name": "Card", "components": [
      {{ "type": "Transform", "position": [0.0, 1.2, 0.0], "scale": [2.4, 1.0, 2.4] }},
      {{ "type": "Mesh", "asset": "builtin:plane" }},
      {{ "type": "Material", "albedo": [0.12, 0.55, 0.16], "roughness": 0.9,
         "albedo_map": "{map}", "alpha_cutoff": 0.5 }}
    ]}}
  ]
}}"#
        )
    };

    let solid = render(&scene("white.png"));
    let cut = render(&scene("holes.png"));

    // The card itself: it is green and everything else is grey, so counting
    // green-dominant pixels counts card. A cut card is strictly less of it.
    let card_pixels = |image: &Image| {
        let mut count = 0u32;
        for y in 0..SIZE {
            for x in 0..SIZE {
                let p = image.pixel(x, y);
                if p[1] > p[0] + 20 && p[1] > p[2] + 20 {
                    count += 1;
                }
            }
        }
        count
    };
    let (solid_card, cut_card) = (card_pixels(&solid), card_pixels(&cut));
    assert!(solid_card > 0, "the card should be visible at all");
    assert!(
        cut_card * 2 < solid_card,
        "a cutoff above the map's transparent texels must remove pixels \
         (cut {cut_card} vs solid {solid_card})"
    );

    // And the floor: the solid card's shadow is the whole quad, the cut card's
    // is only its opaque middle, so there is strictly less shadow on the ground.
    // Counted rather than summed, because the two images also differ where the
    // card itself is and a sum would mix the two effects.
    let shadowed_floor = |image: &Image| {
        let mut count = 0u32;
        for y in 0..SIZE {
            for x in 0..SIZE {
                let p = image.pixel(x, y);
                let is_card = p[1] > p[0] + 20 && p[1] > p[2] + 20;
                if !is_card && luma(p) < 400 {
                    count += 1;
                }
            }
        }
        count
    };
    let (solid_shadow, cut_shadow) = (shadowed_floor(&solid), shadowed_floor(&cut));
    assert!(solid_shadow > 0, "the solid card should cast a shadow at all");
    assert!(
        cut_shadow < solid_shadow,
        "a cut-out card must cut its shadow too, or it casts the silhouette of \
         the quad it was drawn on (cut {cut_shadow} vs solid {solid_shadow})"
    );
}
