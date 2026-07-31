//! Loads real files from disk — the checked-in example pyramid, plus
//! fixtures generated into a temp directory per test.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine_core::mesh::{BuiltinMesh, MeshSource};
use engine_core::texture::{ColorSpace, TextureSource};

fn example(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(relative)
}

/// A scratch directory, torn down on drop.
struct Fixture(PathBuf);

impl Fixture {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("engine-assets-{test}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, name: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One triangle at z=0, positions only — no indices, normals, or UVs, so the
/// loader's reconstruction paths all run.
fn bare_triangle_gltf(extra_primitive_keys: &str) -> String {
    format!(
        r#"{{
  "asset": {{"version": "2.0"}},
  "scene": 0,
  "scenes": [{{"nodes": [0]}}],
  "nodes": [{{"mesh": 0}}],
  "meshes": [{{"primitives": [{{"attributes": {{"POSITION": 0}}{extra_primitive_keys}}}]}}],
  "accessors": [
    {{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
      "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]}}
  ],
  "bufferViews": [{{"buffer": 0, "byteOffset": 0, "byteLength": 36}}],
  "buffers": [{{"byteLength": 36, "uri": "tri.bin"}}]
}}"#
    )
}

fn triangle_bin() -> Vec<u8> {
    [
        [0.0f32, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
    ]
    .iter()
    .flatten()
    .flat_map(|f| f.to_le_bytes())
    .collect()
}

#[test]
fn loads_the_checked_in_pyramid() {
    let mesh = engine_assets::load_gltf(&example("meshes/pyramid.gltf")).unwrap();

    assert_eq!(mesh.vertex_count(), 16, "flat-shaded: 4 sides x3 + base x4");
    assert_eq!(mesh.triangle_count(), 6, "4 sides + 2 base");
    assert_eq!(mesh.normals.len(), 16);
    assert_eq!(mesh.uvs.len(), 16);

    // The apex is at (0, 1, 0) and the base spans the unit square on y=0.
    let max_y = mesh.positions.iter().map(|p| p[1]).fold(f32::MIN, f32::max);
    let min_y = mesh.positions.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
    assert_eq!((min_y, max_y), (0.0, 1.0));

    // Every triangle winds outward from the pyramid's interior, or backface
    // culling would eat it (same check the builtin cube gets).
    let center = glam::Vec3::new(0.0, 0.35, 0.0);
    for triangle in mesh.indices.chunks(3) {
        let [a, b, c] = [
            glam::Vec3::from_array(mesh.positions[triangle[0] as usize]),
            glam::Vec3::from_array(mesh.positions[triangle[1] as usize]),
            glam::Vec3::from_array(mesh.positions[triangle[2] as usize]),
        ];
        let geometric = (b - a).cross(c - a);
        let outward = (a + b + c) / 3.0 - center;
        assert!(
            geometric.dot(outward) > 0.0,
            "a pyramid face winds inward: {a:?} {b:?} {c:?}"
        );
    }
}

#[test]
fn reconstructs_missing_normals_and_indices() {
    let fixture = Fixture::new("bare");
    fixture.write("tri.bin", triangle_bin());
    let path = fixture.write("tri.gltf", bare_triangle_gltf(""));

    let mesh = engine_assets::load_gltf(&path).unwrap();
    assert_eq!(mesh.indices, vec![0, 1, 2], "unindexed becomes sequential");
    assert_eq!(mesh.uvs, vec![[0.0, 0.0]; 3], "absent UVs default to zero");
    for normal in &mesh.normals {
        let n = glam::Vec3::from_array(*normal);
        assert!(
            (n - glam::Vec3::Z).length() < 1e-5,
            "CCW triangle in the XY plane faces +Z, got {n:?}"
        );
    }
}

#[test]
fn bakes_node_transforms_into_vertices() {
    let fixture = Fixture::new("transform");
    fixture.write("tri.bin", triangle_bin());
    let source = bare_triangle_gltf("").replace(
        r#""nodes": [{"mesh": 0}]"#,
        r#""nodes": [{"mesh": 0, "translation": [10.0, 0.0, 0.0]}]"#,
    );
    let path = fixture.write("tri.gltf", source);

    let mesh = engine_assets::load_gltf(&path).unwrap();
    assert_eq!(mesh.positions[0], [10.0, 0.0, 0.0]);
    assert_eq!(mesh.positions[1], [11.0, 0.0, 0.0]);
}

#[test]
fn loads_a_glb_container() {
    // Minimal GLB: 12-byte header, JSON chunk (space-padded), BIN chunk.
    let json = bare_triangle_gltf("").replace(r#", "uri": "tri.bin""#, "");
    let mut json_bytes = json.into_bytes();
    while json_bytes.len() % 4 != 0 {
        json_bytes.push(b' ');
    }
    let mut bin = triangle_bin();
    while bin.len() % 4 != 0 {
        bin.push(0);
    }

    let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
    let mut glb = Vec::with_capacity(total);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total as u32).to_le_bytes());
    glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&json_bytes);
    glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"BIN\0");
    glb.extend_from_slice(&bin);

    let fixture = Fixture::new("glb");
    let path = fixture.write("tri.glb", glb);

    let mesh = engine_assets::load_gltf(&path).unwrap();
    assert_eq!(mesh.vertex_count(), 3);
    assert_eq!(mesh.triangle_count(), 1);
}

#[test]
fn rejects_non_triangle_primitives() {
    let fixture = Fixture::new("lines");
    fixture.write("tri.bin", triangle_bin());
    let path = fixture.write("lines.gltf", bare_triangle_gltf(r#", "mode": 1"#));

    let err = engine_assets::load_gltf(&path).unwrap_err();
    assert_eq!(err.error, "asset_unsupported");
    assert!(err.message.contains("Lines"), "{}", err.message);
}

#[test]
fn rejects_a_file_that_is_not_gltf() {
    let fixture = Fixture::new("corrupt");
    let path = fixture.write("bad.gltf", "this is not json");

    let err = engine_assets::load_gltf(&path).unwrap_err();
    assert_eq!(err.error, "asset_load_failed");
    assert!(
        err.context().unwrap().file.is_some(),
        "load errors must name the file"
    );
}

#[test]
fn server_loads_builtins_files_and_caches() {
    let fixture = Fixture::new("server");
    fixture.write("tri.bin", triangle_bin());
    fixture.write("tri.gltf", bare_triangle_gltf(""));

    let server = engine_assets::AssetServer::new(&fixture.0);
    assert_eq!(
        *server.load_mesh("builtin:cube").unwrap(),
        BuiltinMesh::Cube.data()
    );

    let first = server.load_mesh("tri.gltf").unwrap();
    // Delete the backing file: a second load must come from the cache.
    std::fs::remove_file(fixture.0.join("tri.gltf")).unwrap();
    let again = server.load_mesh("tri.gltf").unwrap();
    assert_eq!(*again, *first);
    // And the hit is the *same* allocation, not a copy of it — what lets a
    // viewer rebuild its draw list every frame without copying geometry, and
    // what the renderer keys its uploaded buffers on.
    assert!(std::sync::Arc::ptr_eq(&first, &again));
}

#[test]
fn server_reports_a_missing_file_with_suggestions() {
    let fixture = Fixture::new("server-missing");
    fixture.write("tri.bin", triangle_bin());
    fixture.write("tri.gltf", bare_triangle_gltf(""));

    let server = engine_assets::AssetServer::new(&fixture.0);
    let err = server.load_mesh("trii.gltf").unwrap_err();
    assert_eq!(err.error, "asset_not_found");
    assert_eq!(
        err.context().unwrap().did_you_mean.as_deref(),
        Some("tri.gltf")
    );
}

#[test]
fn validate_pass_accepts_the_checked_in_example_scene() {
    let path = example("scenes/mesh_import.json");
    let source = std::fs::read_to_string(&path).unwrap();
    let display = path.display().to_string();

    let structural = engine_core::validate::validate_source(&source, &display);
    assert!(structural.is_empty(), "{structural:?}");

    let assets = engine_assets::validate_scene_assets(&source, &display);
    assert!(assets.is_empty(), "{assets:?}");
}

#[test]
fn validate_pass_reports_a_corrupt_mesh_with_scene_location() {
    let fixture = Fixture::new("validate");
    fixture.write("broken.gltf", "not gltf at all");
    let scene_path = fixture.write(
        "scene.json",
        "{\n  \"name\": \"s\",\n  \"entities\": [\n    { \"name\": \"Thing\", \"components\": [\n      { \"type\": \"Mesh\", \"asset\": \"broken.gltf\" }\n    ] }\n  ]\n}\n",
    );
    let source = std::fs::read_to_string(&scene_path).unwrap();
    let display = scene_path.display().to_string();

    // The structural pass passes — the file exists with a known extension...
    assert!(engine_core::validate::validate_source(&source, &display).is_empty());

    // ...and the asset pass is what catches that it does not parse.
    let errors = engine_assets::validate_scene_assets(&source, &display);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].error, "asset_load_failed");

    let context = errors[0].context().unwrap();
    assert_eq!(context.entity.as_deref(), Some("Thing"));
    assert_eq!(context.component.as_deref(), Some("Mesh"));
    assert_eq!(context.file.as_deref(), Some(display.as_str()));
    assert_eq!(context.line, Some(5), "points at the asset reference");
}

#[test]
fn loads_a_png_texture_as_rgba8() {
    let fixture = Fixture::new("texture");
    let path = fixture.0.join("tex.png");
    let mut png = image::RgbaImage::new(2, 2);
    png.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
    png.put_pixel(1, 1, image::Rgba([0, 0, 255, 128]));
    png.save(&path).unwrap();

    let texture = engine_assets::load_texture(&path, ColorSpace::Srgb).unwrap();
    assert_eq!((texture.width, texture.height), (2, 2));
    assert_eq!(texture.rgba().len(), 16);
    assert_eq!(&texture.rgba()[0..4], &[255, 0, 0, 255]);
    assert_eq!(&texture.rgba()[12..16], &[0, 0, 255, 128]);
    // And the mip chain the renderer needs, generated at load (M26).
    assert_eq!(texture.mips.len(), 2, "2×2 chains down to 1×1");
}

#[test]
fn texture_errors_are_structured() {
    let fixture = Fixture::new("texture-errors");
    let missing =
        engine_assets::load_texture(&fixture.0.join("nope.png"), ColorSpace::Srgb).unwrap_err();
    assert_eq!(missing.error, "asset_not_found");

    let bad = fixture.write("bad.png", "not a png");
    let corrupt = engine_assets::load_texture(&bad, ColorSpace::Srgb).unwrap_err();
    assert_eq!(corrupt.error, "asset_load_failed");
}

/// The device limit is refused before the chain is built, not at upload —
/// `tree_too_complex`'s precedent, for `tree_too_complex`'s reason.
#[test]
fn a_texture_over_the_device_limit_fails_to_load() {
    let fixture = Fixture::new("texture-too-large");
    let path = fixture.0.join("huge.png");
    image::RgbaImage::new(4096, 8).save(&path).unwrap();

    let error = engine_assets::load_texture(&path, ColorSpace::Srgb).unwrap_err();
    assert_eq!(error.error, "texture_too_large");
    assert!(error.message.contains("4096"), "{}", error.message);
}

/// The M15 rule, asserted rather than assumed: the renderer keys its uploaded
/// GPU textures on this `Arc`'s identity, and a fresh one per call would
/// re-upload every texture every frame.
#[test]
fn one_texture_asset_is_one_arc() {
    let fixture = Fixture::new("texture-cache");
    let path = fixture.0.join("tex.png");
    image::RgbaImage::new(4, 4).save(&path).unwrap();

    let server = engine_assets::AssetServer::new(&fixture.0);
    let first = server.load_texture("tex.png", ColorSpace::Srgb).unwrap();
    let second = server.load_texture("tex.png", ColorSpace::Srgb).unwrap();
    assert!(Arc::ptr_eq(&first, &second), "repeat loads share one Arc");

    // A different colour space is a different decode — the mip chain is
    // filtered in it — so it is a different entry rather than the same one.
    let linear = server.load_texture("tex.png", ColorSpace::Linear).unwrap();
    assert!(!Arc::ptr_eq(&first, &linear));
    assert_eq!(linear.space, ColorSpace::Linear);
}

// ── Rigs (M27) ────────────────────────────────────────────────────────────

#[test]
fn a_skinned_primitive_carries_its_influences_and_stays_in_skin_space() {
    let mesh = engine_assets::load_gltf(&example("meshes/rigged_arm.gltf")).unwrap();

    assert!(mesh.is_skinned(), "the arm has JOINTS_0 and WEIGHTS_0");
    assert_eq!(mesh.joint_indices.len(), mesh.positions.len());
    assert_eq!(mesh.joint_weights.len(), mesh.positions.len());
    for weights in &mesh.joint_weights {
        let sum: f32 = weights.iter().sum();
        assert!(
            (sum - 1.0).abs() < 1e-5,
            "glTF weights are normalized per vertex, got {weights:?}"
        );
    }
    for indices in &mesh.joint_indices {
        assert!(
            indices.iter().all(|&i| (i as usize) < 3),
            "the arm has three joints, got {indices:?}"
        );
    }
}

#[test]
fn a_skinned_primitives_node_transform_is_not_baked_into_its_vertices() {
    // glTF says the transform of the node referencing a skinned mesh is
    // ignored: the palette already speaks skin space. Baking it is the bug
    // this test exists to catch, and its symptom — a character posed correctly
    // in the wrong place — looks like a scene-authoring mistake rather than a
    // loader one.
    let source = std::fs::read_to_string(example("meshes/rigged_arm.gltf")).unwrap();
    let mut document: serde_json::Value = serde_json::from_str(&source).unwrap();
    let unmoved = engine_assets::load_gltf(&example("meshes/rigged_arm.gltf")).unwrap();

    document["nodes"][0]["translation"] = serde_json::json!([100.0, 0.0, 0.0]);
    let fixture = Fixture::new("skinned-node-transform");
    let path = fixture.write("arm.gltf", serde_json::to_string(&document).unwrap());
    let moved = engine_assets::load_gltf(&path).unwrap();

    assert_eq!(
        unmoved.positions, moved.positions,
        "moving the skinned node must not move a single vertex"
    );
}

#[test]
fn an_unskinned_file_carries_no_influences_at_all() {
    // Which is what keeps every mesh committed before M27 uploading exactly
    // the vertex buffers it always did.
    let mesh = engine_assets::load_gltf(&example("meshes/pyramid.gltf")).unwrap();
    assert!(!mesh.is_skinned());
    assert!(mesh.joint_indices.is_empty());
    assert!(mesh.joint_weights.is_empty());
}

#[test]
fn a_fifth_influence_per_vertex_is_refused_rather_than_dropped() {
    let source = std::fs::read_to_string(example("meshes/rigged_arm.gltf")).unwrap();
    let mut document: serde_json::Value = serde_json::from_str(&source).unwrap();
    // Point JOINTS_1 at the accessor JOINTS_0 already uses: the values are
    // irrelevant, the attribute's presence is the whole claim.
    let joints_0 = document["meshes"][0]["primitives"][0]["attributes"]["JOINTS_0"].clone();
    document["meshes"][0]["primitives"][0]["attributes"]["JOINTS_1"] = joints_0;

    let fixture = Fixture::new("joints-1");
    let path = fixture.write("arm.gltf", serde_json::to_string(&document).unwrap());
    let error = engine_assets::load_gltf(&path).unwrap_err();

    assert_eq!(error.error, engine_core::codes::ASSET_UNSUPPORTED);
    assert!(
        error.message.contains("JOINTS_1"),
        "the message has to name the attribute: {}",
        error.message
    );
}

#[test]
fn the_draw_list_carries_a_palette_that_moves_with_the_clock() {
    let path = example("scenes/verify/m27_skeletal.json");
    let source = std::fs::read_to_string(&path).unwrap();
    let scene = engine_core::Scene::from_source(&source, &path.display().to_string()).unwrap();
    let assets = engine_assets::AssetServer::for_scene(&path);

    let at = |t: Option<f32>| {
        let items = scene.render_items_at(&assets, t).unwrap();
        let find = |name: &str| {
            items
                .iter()
                .find(|item| item.entity == name)
                .unwrap_or_else(|| panic!("{name} draws"))
                .joints
                .clone()
        };
        (find("Arm"), find("Rest"), find("Ground"))
    };

    let (rest_arm, rest_still, ground) = at(None);
    assert!(
        ground.is_empty(),
        "an unskinned mesh carries no palette, which is what keeps it on the \
         pipeline that compiles mesh.wgsl as it sits on disk"
    );
    assert_eq!(rest_arm.len(), 3, "the arm's three joints");
    assert_eq!(
        rest_arm, rest_still,
        "with no clock both arms are the rest pose"
    );

    // A quarter of the way into Wave, the played arm has moved and the one
    // with no AnimationPlayer has not.
    let (waved, still, _) = at(Some(0.25));
    assert_eq!(still, rest_still, "no player, no motion, at any time");
    assert_ne!(waved, rest_arm, "the Wave clip poses the played arm");
}

#[test]
fn the_rigged_arm_loads_its_skin_in_the_files_joint_order() {
    let rig = engine_assets::load_rig(&example("meshes/rigged_arm.gltf")).unwrap();
    let skin = rig.skin.expect("rigged_arm.gltf has a skin");

    assert_eq!(skin.name.as_deref(), Some("ArmRig"));
    // The skin's own `joints` order, not sorted — a joint's index is written
    // into the vertex data.
    let names: Vec<&str> = skin.joints.iter().map(|j| j.name.as_str()).collect();
    assert_eq!(names, ["Shoulder", "Elbow", "Hand"]);

    assert_eq!(skin.joints[0].parent, None);
    assert_eq!(skin.joints[1].parent, Some(0));
    assert_eq!(skin.joints[2].parent, Some(1));
}

#[test]
fn the_rest_pose_puts_each_joint_where_the_file_says() {
    let rig = engine_assets::load_rig(&example("meshes/rigged_arm.gltf")).unwrap();
    let skin = rig.skin.unwrap();
    let globals = engine_core::skeleton::joint_globals(&skin, None, 0.0);

    for (index, expected) in [0.0f32, 1.0, 2.0].iter().enumerate() {
        let position = globals[index].transform_point3(glam::Vec3::ZERO);
        assert!(
            (position - glam::Vec3::new(0.0, *expected, 0.0)).length() < 1e-5,
            "joint {index} is at {position}, expected y={expected}"
        );
    }

    // Rest pose == bind pose, so every palette entry is the identity: a
    // skinned mesh with no clip renders exactly where its vertices sit.
    for matrix in engine_core::skeleton::palette(&skin, None, 0.0) {
        assert!(
            (matrix - glam::Mat4::IDENTITY).abs_diff_eq(glam::Mat4::ZERO, 1e-5),
            "{matrix} is not the identity"
        );
    }
}

#[test]
fn the_wave_clip_moves_the_hand_and_returns_it() {
    let rig = engine_assets::load_rig(&example("meshes/rigged_arm.gltf")).unwrap();
    let skin = rig.skin.unwrap();
    let wave = rig.clips.iter().find(|c| c.name == "Wave").unwrap();
    assert_eq!(engine_core::skeleton::duration(wave), 1.0);

    let hand_at = |t| {
        engine_core::skeleton::joint_globals(&skin, Some(wave), t)[2]
            .transform_point3(glam::Vec3::ZERO)
    };

    let rest = hand_at(0.0);
    let bent = hand_at(0.5);
    let back = hand_at(1.0);

    // The claim the milestone makes about itself: motion is verifiable
    // without a pixel.
    assert!(
        (bent - rest).length() > 0.5,
        "the hand barely moved: {rest} -> {bent}"
    );
    // A 60° bend at the elbow swings the hand forward (-Z) and down.
    assert!(bent.z < -0.8, "the hand did not swing forward: {bent}");
    assert!(bent.y < rest.y, "the hand did not drop: {bent}");
    assert!((back - rest).length() < 1e-5, "the clip did not return: {back}");
}

#[test]
fn a_channel_outside_the_skin_is_loaded_and_then_ignored() {
    // `Marker` is a node in the scene that is in no skin. glTF allows the
    // channel; sampling ignores it; `list-animations` reports it — an ignored
    // channel nothing names is invisible.
    let rig = engine_assets::load_rig(&example("meshes/rigged_arm.gltf")).unwrap();
    let wave = rig.clips.iter().find(|c| c.name == "Wave").unwrap();
    let skin = rig.skin.unwrap();

    let marker = wave
        .channels
        .iter()
        .find(|c| c.node_name.as_deref() == Some("Marker"))
        .expect("the Marker channel survived loading");
    assert!(skin.joint_of_node(marker.node).is_none());

    // Every joint that the elbow rotation does not reach is exactly at rest.
    let posed = engine_core::skeleton::joint_globals(&skin, Some(wave), 1.0);
    let rest = engine_core::skeleton::joint_globals(&skin, None, 0.0);
    assert_eq!(posed[0], rest[0]);
}

#[test]
fn every_clip_in_the_file_is_addressable_by_name() {
    let rig = engine_assets::load_rig(&example("meshes/rigged_arm.gltf")).unwrap();
    assert_eq!(rig.clip_names(), ["Wave", "Sway"]);
    assert!(rig.clip_named("Sway").is_some());
    assert!(rig.clip_named("Walk").is_none());
}

#[test]
fn an_unrigged_file_loads_an_empty_rig_rather_than_failing() {
    // Every mesh in the repo is one of these; "does this file have a rig" is
    // a question the caller asks, not a failure.
    let rig = engine_assets::load_rig(&example("meshes/pyramid.gltf")).unwrap();
    assert!(rig.skin.is_none());
    assert!(rig.clips.is_empty());
}

/// A minimal text glTF carrying a skin of `count` joints and nothing else.
///
/// No geometry: `load_rig` does not need any, and the point is the joint
/// budget rather than the mesh.
fn skin_only_gltf(count: usize) -> String {
    let nodes: Vec<String> = (0..count)
        .map(|i| {
            let children = if i + 1 < count {
                format!(", \"children\": [{}]", i + 1)
            } else {
                String::new()
            };
            format!("{{\"name\": \"j{i}\", \"translation\": [0, 1, 0]{children}}}")
        })
        .collect();
    let joints: Vec<String> = (0..count).map(|i| i.to_string()).collect();
    format!(
        r#"{{
          "asset": {{"version": "2.0"}},
          "scene": 0,
          "scenes": [{{"nodes": [0]}}],
          "nodes": [{}],
          "skins": [{{"name": "Long", "joints": [{}]}}]
        }}"#,
        nodes.join(","),
        joints.join(",")
    )
}

#[test]
fn a_skin_over_the_palette_budget_fails_validation_before_any_device_exists() {
    // The `MAX_POINT_LIGHTS` / `MAX_ROAD_KERBS` idiom: a fixed-size uniform
    // gets an error at validate time, never a character that renders
    // correctly up to joint 128 and explodes past it.
    let fixture = Fixture::new("too-many-joints");
    let over = engine_core::skeleton::MAX_JOINTS + 1;
    fixture.write("long.gltf", skin_only_gltf(over));
    fixture.write("ok.gltf", skin_only_gltf(engine_core::skeleton::MAX_JOINTS));

    let scene = |asset: &str| {
        format!(
            r#"{{"name":"s","entities":[{{"name":"A","components":[
                {{"type":"Mesh","asset":"{asset}"}},
                {{"type":"AnimationPlayer","clip":"{asset}#Nope"}}
            ]}}]}}"#
        )
    };

    let path = fixture.write("scene.json", scene("long.gltf"));
    let errors =
        engine_assets::validate_scene_assets(&scene("long.gltf"), &path.display().to_string());
    let codes: Vec<&str> = errors.iter().map(|e| e.error).collect();
    assert!(
        codes.contains(&"too_many_joints"),
        "a {over}-joint skin was accepted: {codes:?}"
    );

    // Exactly at the limit is fine — the ceiling is inclusive.
    let path = fixture.write("ok.json", scene("ok.gltf"));
    let errors =
        engine_assets::validate_scene_assets(&scene("ok.gltf"), &path.display().to_string());
    let codes: Vec<&str> = errors.iter().map(|e| e.error).collect();
    assert!(
        !codes.contains(&"too_many_joints"),
        "a {}-joint skin was refused: {codes:?}",
        engine_core::skeleton::MAX_JOINTS
    );
}
