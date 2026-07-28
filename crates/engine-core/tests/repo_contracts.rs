//! Contracts between the code and the files checked into the repository.
//!
//! These tests are how "generated, not hand-written" stays true (invariant 7):
//! if a component changes and the schema file is not regenerated, or the demo
//! scene rots, the build fails rather than shipping a stale contract.

use std::path::Path;

fn repo_file(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()))
}

#[test]
fn checked_in_schema_matches_the_code() {
    let checked_in = repo_file("schemas/component-schema.json");
    let generated = engine_core::schema::canonical_json();
    assert_eq!(
        checked_in, generated,
        "schemas/component-schema.json is stale — regenerate it:\n\
         cargo run -p engine-cli -- list-components > schemas/component-schema.json"
    );
}

#[test]
fn checked_in_animation_schema_matches_the_code() {
    let checked_in = repo_file("schemas/animation-schema.json");
    let generated = engine_core::schema::canonical_animation_json();
    assert_eq!(
        checked_in, generated,
        "schemas/animation-schema.json is stale — regenerate it:\n\
         cargo run -p engine-cli -- list-animations --schema > schemas/animation-schema.json"
    );
}

#[test]
fn m9_spin_verify_scene_and_clip_are_valid() {
    assert_scene_validates("examples/scenes/verify/m9_spin.json");
    let source = repo_file("examples/scenes/verify/animations/spin.anim.json");
    let errors = engine_core::animation::validate_clip_source(&source, "spin.anim.json");
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn m10_script_verify_scene_is_valid() {
    assert_scene_validates("examples/scenes/verify/m10_script.json");
}

#[test]
fn demo_scene_is_valid() {
    // The demo scene references a mesh file (the truck), so it validates
    // through the absolute-path helper like the other file-referencing scenes.
    assert_scene_validates("examples/scenes/demo_scene.json");
}

/// Validate a scene that references real files, so validation must see the
/// scene's actual location — hence the absolute path.
fn assert_scene_validates(relative: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(relative);
    let display = path.display().to_string();
    let source = std::fs::read_to_string(&path).unwrap();
    let errors = engine_core::validate::validate_source(&source, &display);
    assert!(
        errors.is_empty(),
        "the checked-in scene {relative} no longer validates:\n{}",
        errors
            .iter()
            .map(|e| e.to_json())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn mesh_import_scene_is_valid() {
    assert_scene_validates("examples/scenes/mesh_import.json");
}

#[test]
fn m4_lighting_verify_scene_is_valid() {
    // The M4 verification fixture (milestone-verification-scenes.md): its
    // JSON is canonical and never edited casually, so it validating is a
    // repo contract like the schema file.
    assert_scene_validates("examples/scenes/verify/m4_lighting.json");
}

/// The M5 verification fixture is committed **broken** — it packs one
/// instance of every headline error class into a single file to prove the
/// all-errors-at-once contract. It *failing* validation, with exactly these
/// codes, is the pass condition (milestone-verification-scenes.md).
#[test]
fn m5_broken_verify_scene_reports_every_planted_error() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/scenes/verify/m5_broken.json");
    let display = path.display().to_string();
    let source = std::fs::read_to_string(&path).unwrap();
    let errors = engine_core::validate::validate_source(&source, &display);

    let dump = || {
        errors
            .iter()
            .map(|e| e.to_json())
            .collect::<Vec<_>>()
            .join("\n")
    };

    // One run reports all of them — never a drip-feed.
    assert_eq!(errors.len(), 7, "expected exactly the planted errors:\n{}", dump());

    for error in &errors {
        let context = error.context().expect("every error carries context");
        assert!(context.line.is_some(), "no line: {}", error.to_json());
        assert!(context.path.is_some(), "no path: {}", error.to_json());
    }

    let find = |code: &str, predicate: &dyn Fn(&engine_core::EngineError) -> bool| {
        errors
            .iter()
            .find(|e| e.error == code && predicate(e))
            .unwrap_or_else(|| panic!("missing {code}:\n{}", dump()))
    };

    let typo = find("unknown_component", &|e| {
        e.context().unwrap().component.as_deref() == Some("Meterial")
    });
    assert_eq!(
        typo.context().unwrap().did_you_mean.as_deref(),
        Some("Material")
    );

    find("value_out_of_range", &|e| e.message.contains("albedo[0] is 1.5"));
    find("value_out_of_range", &|e| e.message.contains("roughness is 1.5"));
    find("value_out_of_range", &|e| e.message.contains("intensity is -2"));
    find("asset_not_found", &|e| e.message.contains("does_not_exist.glb"));

    let colour = find("unknown_field", &|e| {
        e.context().unwrap().field.as_deref() == Some("colour")
    });
    assert_eq!(
        colour.context().unwrap().did_you_mean.as_deref(),
        Some("color")
    );

    let cameras = find("multiple_active_cameras", &|_| true);
    assert_eq!(
        cameras.context().unwrap().candidates,
        Some(vec!["CameraA".to_string(), "CameraB".to_string()])
    );
}

/// `codes.rs` ⟷ `docs/error-codes.md`, both directions: every registered
/// code appears in the doc with the same exit class and description, and the
/// doc lists nothing unregistered. Codes are API; this is the enforcement.
#[test]
fn error_code_registry_matches_the_docs() {
    let doc = repo_file("docs/error-codes.md");

    let mut documented = Vec::new();
    for line in doc.lines() {
        let Some(rest) = line.strip_prefix("| `") else {
            continue;
        };
        let mut cells = rest.split(" | ");
        let code = cells
            .next()
            .and_then(|c| c.strip_suffix('`'))
            .unwrap_or_else(|| panic!("malformed row: {line}"));
        let exit: i32 = cells
            .next()
            .and_then(|c| c.trim().parse().ok())
            .unwrap_or_else(|| panic!("malformed exit cell: {line}"));
        let description = cells
            .next()
            .and_then(|c| c.strip_suffix(" |"))
            .unwrap_or_else(|| panic!("malformed description cell: {line}"));
        documented.push((code.to_string(), exit, description.to_string()));
    }

    let registry = engine_core::codes::REGISTRY;
    for entry in registry {
        let row = documented
            .iter()
            .find(|(code, _, _)| code == entry.code)
            .unwrap_or_else(|| {
                panic!("{} is registered but missing from docs/error-codes.md", entry.code)
            });
        assert_eq!(
            row.1,
            entry.class.code(),
            "{}: doc says exit {}, registry says {}",
            entry.code,
            row.1,
            entry.class.code()
        );
        assert_eq!(
            row.2, entry.description,
            "{}: doc description differs from the registry",
            entry.code
        );
    }
    for (code, _, _) in &documented {
        assert!(
            registry.iter().any(|entry| entry.code == code),
            "{code} is documented but not registered in codes.rs"
        );
    }
    assert_eq!(documented.len(), registry.len());
}

/// A [`engine_core::mesh::MeshSource`] that substitutes a cube for file
/// assets: engine-core cannot parse glTF (that's engine-assets' job), but
/// this test only counts draw calls, so any mesh data will do for the truck.
struct StubbedFileAssets;

impl engine_core::mesh::MeshSource for StubbedFileAssets {
    fn load_mesh(
        &self,
        asset: &str,
    ) -> engine_core::error::Result<std::sync::Arc<engine_core::mesh::MeshData>> {
        match engine_core::mesh::BuiltinMesh::parse(asset) {
            Some(builtin) => Ok(std::sync::Arc::new(builtin?.data())),
            None => Ok(std::sync::Arc::new(engine_core::mesh::BuiltinMesh::Cube.data())),
        }
    }
}

#[test]
fn demo_scene_loads_and_draws_everything() {
    // The truck's mesh existence check resolves relative to the scene path,
    // so from_source must see the scene's real location.
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/scenes/demo_scene.json");
    let source = repo_file("examples/scenes/demo_scene.json");
    let scene = engine_core::Scene::from_source(&source, &path.display().to_string()).unwrap();

    assert_eq!(scene.entity_count(), 8);
    scene.camera(None).expect("demo scene has an active camera");
    assert!(
        scene.lights().sun.is_some() && scene.lights().ambient.is_some(),
        "the demo scene models the recommended sun+ambient rig"
    );

    let items = scene
        .render_items(&StubbedFileAssets)
        .expect("all demo assets resolve");
    assert_eq!(
        items.len(),
        5,
        "Ground, Cube1, Cube2, Sphere1, truck — lights and camera draw nothing"
    );
}

/// The M7 write-through contract on the real M4 fixture: an inspector-style
/// edit of one field produces exactly one changed line — no reordering, no
/// reformatting, no churn (editor principle #5). This is the scriptable
/// analog of milestone-verification-scenes.md step 2.
#[test]
fn formatter_edit_of_m4_fixture_changes_exactly_one_line() {
    let source = repo_file("examples/scenes/verify/m4_lighting.json");
    let edit = engine_core::formatter::SetComponentField {
        entity: "SphereSmooth".into(),
        component: "Material".into(),
        field: "roughness".into(),
        value: serde_json::json!(0.3),
    };
    let edited = engine_core::formatter::apply_set_component_field(&source, &edit).unwrap();

    let changed: Vec<(&str, &str)> = source
        .lines()
        .zip(edited.lines())
        .filter(|(a, b)| a != b)
        .collect();
    assert_eq!(source.lines().count(), edited.lines().count());
    assert_eq!(changed.len(), 1, "one hunk, one line: {changed:?}");
    assert!(changed[0].1.contains("0.3"), "{}", changed[0].1);

    // And the edited scene still validates — the editor can never write a
    // scene the engine rejects, for a value the schema allows.
    let errors = engine_core::validate::validate_source(&edited, "edited.json");
    assert!(
        errors.iter().all(|e| e.is_warning() || e.error == "asset_not_found"),
        "unexpected: {errors:?}"
    );
}
