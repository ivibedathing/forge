//! Loads real files from disk — the checked-in example pyramid, plus
//! fixtures generated into a temp directory per test.

use std::path::{Path, PathBuf};

use engine_core::mesh::{BuiltinMesh, MeshSource};

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
        server.load_mesh("builtin:cube").unwrap(),
        BuiltinMesh::Cube.data()
    );

    let first = server.load_mesh("tri.gltf").unwrap();
    // Delete the backing file: a second load must come from the cache.
    std::fs::remove_file(fixture.0.join("tri.gltf")).unwrap();
    assert_eq!(server.load_mesh("tri.gltf").unwrap(), first);
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

    let texture = engine_assets::load_texture(&path).unwrap();
    assert_eq!((texture.width, texture.height), (2, 2));
    assert_eq!(texture.rgba.len(), 16);
    assert_eq!(&texture.rgba[0..4], &[255, 0, 0, 255]);
    assert_eq!(&texture.rgba[12..16], &[0, 0, 255, 128]);
}

#[test]
fn texture_errors_are_structured() {
    let fixture = Fixture::new("texture-errors");
    let missing = engine_assets::load_texture(&fixture.0.join("nope.png")).unwrap_err();
    assert_eq!(missing.error, "asset_not_found");

    let bad = fixture.write("bad.png", "not a png");
    let corrupt = engine_assets::load_texture(&bad).unwrap_err();
    assert_eq!(corrupt.error, "asset_load_failed");
}
