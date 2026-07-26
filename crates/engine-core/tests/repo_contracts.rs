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
fn demo_scene_is_valid() {
    let source = repo_file("examples/scenes/demo_scene.json");
    let errors = engine_core::validate::validate_source(&source, "examples/scenes/demo_scene.json");
    assert!(
        errors.is_empty(),
        "the checked-in demo scene no longer validates:\n{}",
        errors
            .iter()
            .map(|e| e.to_json())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn demo_scene_loads_and_draws_everything() {
    let source = repo_file("examples/scenes/demo_scene.json");
    let scene = engine_core::Scene::from_source(&source, "demo_scene.json").unwrap();

    assert_eq!(scene.entity_count(), 4);
    scene.camera(None).expect("demo scene has an active camera");

    let items = scene.render_items().expect("all demo assets resolve");
    assert_eq!(
        items.len(),
        3,
        "Ground, Cube1, Cube2 — the camera draws nothing"
    );
}
