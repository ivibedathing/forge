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
fn showcase_tour_scene_is_valid() {
    assert_scene_validates("examples/scenes/showcase_tour.json");
}

/// The showcase tour's growth contract (`showcase-tour.md`): it exists to be
/// the one scene where *every* system is on screen at once, so a component
/// the engine has but the tour does not use is a hole in the demo — and, more
/// to the point, a hole in what the FPS and render checks actually measure.
///
/// This test is deliberately the thing that fails on the commit that adds a
/// component. Adding an entity that uses it to `showcase_tour.json` is the
/// fix; there is no allowlist to append to, because an allowlist is how a
/// contract like this quietly stops meaning anything.
///
/// **One exception exists, and it is derived rather than declared.** M21's
/// `daylight` block *synthesizes* the sun and the ambient term, and the engine
/// refuses a scene that also authors them (`daylight_and_directional_light`,
/// and the `daylight_overrides_sky` warning). So `DirectionalLight` and
/// `AmbientLight` became the first components that **cannot** share a scene
/// with another feature — which is a hole in this contract's premise, not in
/// the tour. The exemption below is computed from the same rule validation
/// enforces, so it disappears by itself if `drives_sun` / `drives_sky` are
/// ever turned off in the tour. It is not a list of components someone could
/// not be bothered to add.
#[test]
fn showcase_tour_uses_every_component_the_engine_has() {
    let schema: serde_json::Value =
        serde_json::from_str(&engine_core::schema::canonical_json()).unwrap();
    let known: Vec<&str> = schema["components"]
        .as_array()
        .expect("the schema publishes its component list")
        .iter()
        .map(|name| name.as_str().expect("component names are strings"))
        .collect();

    let scene: serde_json::Value =
        serde_json::from_str(&repo_file("examples/scenes/showcase_tour.json")).unwrap();
    let used: std::collections::BTreeSet<&str> = scene["entities"]
        .as_array()
        .expect("entities")
        .iter()
        .flat_map(|entity| entity["components"].as_array().expect("components"))
        .filter_map(|component| component["type"].as_str())
        .collect();

    // Components the scene is *forbidden* to carry, because its `daylight`
    // block already owns what they mean.
    let daylight = &scene["daylight"];
    let drives = |field: &str| {
        !daylight.is_null() && daylight[field].as_bool().unwrap_or(true)
    };
    let mut owned_by_daylight: Vec<&str> = Vec::new();
    if drives("drives_sun") {
        owned_by_daylight.push("DirectionalLight");
    }
    if drives("drives_sky") {
        owned_by_daylight.push("AmbientLight");
    }

    let missing: Vec<&str> = known
        .iter()
        .copied()
        .filter(|name| !used.contains(name) && !owned_by_daylight.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "examples/scenes/showcase_tour.json shows off every engine system, and \
         these components are not in it: {missing:?}\n\
         Add an entity that uses each one — see showcase-tour.md."
    );

    // The exemption cuts both ways: if the tour stops driving the sun, it owes
    // the demo a real `DirectionalLight` again.
    for name in owned_by_daylight {
        assert!(
            !used.contains(name),
            "the tour's daylight block owns {name}, so the scene must not also \
             author one — validation rejects that combination outright"
        );
    }
}

/// The same growth contract, one level up: every **scene-level block** the
/// format has should be exercised by the tour too.
///
/// `daylight` is a block rather than a component, so the component walk above
/// would never have noticed it missing — which is exactly the kind of gap a
/// growth contract exists to close before an agent finds it.
#[test]
fn showcase_tour_uses_every_scene_block_the_format_has() {
    let schema: serde_json::Value =
        serde_json::from_str(&engine_core::schema::canonical_json()).unwrap();
    let known: Vec<&str> = schema["scene"]["properties"]
        .as_object()
        .expect("the scene schema publishes its top-level fields")
        .keys()
        .map(String::as_str)
        .collect();

    let scene: serde_json::Value =
        serde_json::from_str(&repo_file("examples/scenes/showcase_tour.json")).unwrap();
    let object = scene.as_object().expect("a scene file is an object");

    let missing: Vec<&str> = known
        .iter()
        .copied()
        .filter(|field| !object.contains_key(*field))
        .collect();
    assert!(
        missing.is_empty(),
        "examples/scenes/showcase_tour.json should exercise every scene-level \
         block, and these are absent: {missing:?}\n\
         Add each one — see showcase-tour.md."
    );
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

/// And a stub for the texture half of the draw list (M26): one white texel,
/// whatever was asked for. This test counts draw calls, and a draw call does
/// not care what is in the map.
impl engine_core::texture::TextureSource for StubbedFileAssets {
    fn load_texture(
        &self,
        _asset: &str,
        space: engine_core::texture::ColorSpace,
    ) -> engine_core::error::Result<std::sync::Arc<engine_core::texture::TextureData>> {
        Ok(std::sync::Arc::new(engine_core::texture::TextureData::new(
            1,
            1,
            vec![255, 255, 255, 255],
            space,
        )))
    }
}

/// And the rig half (M27). A stubbed cube has no skin, so the draw list this
/// test counts never asks for a palette.
impl engine_core::skeleton::RigSource for StubbedFileAssets {
    fn load_rig(
        &self,
        _asset: &str,
    ) -> engine_core::error::Result<std::sync::Arc<engine_core::skeleton::Rig>> {
        Ok(std::sync::Arc::new(engine_core::skeleton::Rig::default()))
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

/// Every committed baseline is listed in `verify/baselines.json`, with the
/// scene it comes from.
///
/// The manifest is what `bin/verify-baselines` loops over, and what makes the
/// A/B bit-exactness check between two binaries a command rather than a
/// reconstruction. A baseline missing from it is a baseline nothing re-diffs:
/// 15 of the 26 have no CLI test looking at them, so the sweep is their only
/// check, and it can only check what it can see.
#[test]
fn every_committed_baseline_is_listed_in_the_manifest() {
    let manifest: serde_json::Value =
        serde_json::from_str(&repo_file("examples/scenes/verify/baselines.json")).unwrap();

    let mut listed: Vec<String> = Vec::new();
    for key in ["baselines", "traces"] {
        for entry in manifest[key].as_array().unwrap() {
            let artifact = entry[if key == "baselines" { "baseline" } else { "trace" }]
                .as_str()
                .unwrap();
            let scene = entry["scene"].as_str().unwrap();
            for relative in [artifact, scene] {
                let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(relative);
                assert!(path.exists(), "{relative} is in the manifest but not on disk");
            }
            listed.push(artifact.rsplit('/').next().unwrap().to_string());
        }
    }

    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/scenes/verify/baselines");
    let mut missing: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if (name.ends_with(".png") || name.ends_with(".jsonl")) && !listed.contains(&name) {
            missing.push(name);
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "committed baselines missing from examples/scenes/verify/baselines.json: {missing:?}\n\
         add each one with the scene and flags that reproduce it — `bin/verify-baselines` \
         checks exactly what this file lists"
    );
}

/// Every asset a committed scene references is itself **committed** — not
/// merely present on the machine that wrote the scene.
///
/// This is the mistake M26 shipped for exactly one commit: `.gitignore` carries
/// a blanket `*.png` to keep render output out of the repo, the material
/// textures are `.png`, and they were generated into the working tree by their
/// own script. Every local check passed — the files were right there — and a
/// fresh clone failed to validate the showcase tour.
///
/// Nothing but git can tell "on disk" from "in the repo", so this shells out to
/// it and skips cleanly where it cannot (a tarball install has no repo, and the
/// distribution design ships those deliberately).
#[test]
fn every_asset_a_committed_scene_references_is_committed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let Ok(listing) = std::process::Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["ls-files", "examples"])
        .output()
    else {
        eprintln!("skipping: no git on this machine");
        return;
    };
    if !listing.status.success() {
        eprintln!("skipping: not a git checkout");
        return;
    }
    let tracked: std::collections::HashSet<String> = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .map(str::to_string)
        .collect();

    // Every reference any committed scene makes, as a repo-relative path.
    let mut missing: Vec<String> = Vec::new();
    for scene in tracked.iter().filter(|p| {
        p.starts_with("examples/scenes/")
            && p.ends_with(".json")
            && !p.contains("baselines")
            // The one scene that is committed **broken** and must stay that
            // way: a missing mesh reference is one of the seven planted errors
            // `m5_broken_stays_broken` pins it for.
            && !p.ends_with("m5_broken.json")
    }) {
        let Ok(source) = std::fs::read_to_string(root.join(scene)) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&source) else {
            continue;
        };
        let dir = Path::new(scene).parent().unwrap_or(Path::new(""));

        // Any string field that looks like a relative file reference. Blunt on
        // purpose: a new component with a new asset field is covered the day it
        // exists, with no list to keep.
        let mut stack = vec![&value];
        while let Some(node) = stack.pop() {
            match node {
                serde_json::Value::Object(map) => stack.extend(map.values()),
                serde_json::Value::Array(items) => stack.extend(items.iter()),
                serde_json::Value::String(text) => {
                    let looks_like_a_file = [".png", ".gltf", ".glb", ".json", ".rhai", ".jsonl"]
                        .iter()
                        .any(|ext| text.ends_with(ext));
                    if !looks_like_a_file || text.starts_with("builtin:") {
                        continue;
                    }
                    let joined = dir.join(text);
                    // Lexical normalization: the reference may climb out of the
                    // scene's own directory, which is ordinary.
                    let joined = joined.to_string_lossy().into_owned();
                    let mut parts: Vec<&str> = Vec::new();
                    for part in joined.split('/') {
                        match part {
                            "." | "" => {}
                            ".." => {
                                parts.pop();
                            }
                            other => parts.push(other),
                        }
                    }
                    let normalized = parts.join("/");
                    if !tracked.contains(&normalized) {
                        missing.push(format!("{scene} references untracked {normalized}"));
                    }
                }
                _ => {}
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "a committed scene references files that are not in the repo, so a fresh \
         clone cannot validate or render it:\n  {}",
        missing.join("\n  ")
    );
}
