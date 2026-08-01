//! The validation corpus that lives with the code: every check in this
//! module's siblings has at least one scene here that trips it.

use super::*;

fn codes_of(source: &str) -> Vec<&'static str> {
    validate_source(source, "test.json")
        .into_iter()
        .map(|e| e.error)
        .collect()
}

const VALID: &str = r#"{
  "name": "demo",
  "entities": [
    { "name": "Player", "components": [ { "type": "Camera", "active": true } ] },
    { "name": "Cube1",  "components": [ { "type": "Mesh", "asset": "builtin:cube" } ] }
  ]
}"#;

#[test]
fn accepts_a_valid_scene() {
    assert!(validate_source(VALID, "test.json").is_empty());
}

/// A menu built the way the design intends must validate clean — the
/// counterpart to every rejection test below, and the thing that would
/// break if a new rule were too eager.
#[test]
fn accepts_a_parented_hud_tree() {
    let source = r#"{
      "name": "s",
      "entities": [
        { "name": "Menu", "components": [
            { "type": "HudPanel", "layout": "column", "padding": 12, "gap": 8,
              "anchor": "center", "opacity": 0.9 } ] },
        { "name": "Title", "components": [
            { "type": "HudText", "text": "PAUSED", "parent": "Menu", "size": 24 } ] },
        { "name": "Resume", "components": [
            { "type": "HudRect", "size": [160, 32], "parent": "Menu", "stretch": [true, false] },
            { "type": "HudInteract", "hover_tint": [1.3, 1.3, 1.3] } ] }
      ]
    }"#;
    assert_eq!(codes_of(source), Vec::<&str>::new());
}

#[test]
fn a_parent_must_name_an_entity_that_has_a_panel() {
    let missing = r#"{
      "name": "s",
      "entities": [
        { "name": "Label", "components": [
            { "type": "HudText", "text": "x", "parent": "Menyu" } ] },
        { "name": "Menu", "components": [ { "type": "HudPanel" } ] }
      ]
    }"#;
    let errors = validate_source(missing, "test.json");
    assert_eq!(errors[0].error, codes::HUD_PARENT_NOT_FOUND);
    assert_eq!(
        errors[0].context().unwrap().did_you_mean.as_deref(),
        Some("Menu"),
        "a near-miss parent name suggests the real one"
    );

    // An entity that exists but is not a container: only a panel lays
    // children out, so this would silently do nothing.
    let not_a_panel = r#"{
      "name": "s",
      "entities": [
        { "name": "Label", "components": [
            { "type": "HudText", "text": "x", "parent": "Bar" } ] },
        { "name": "Bar", "components": [ { "type": "HudRect", "size": [4, 4] } ] }
      ]
    }"#;
    assert_eq!(codes_of(not_a_panel), [codes::HUD_PARENT_NOT_PANEL]);
}

/// A cycle would be an infinite walk at layout time, so it is refused
/// here. The message must name the ring — "A's parent is B" does not
/// locate a five-element loop.
#[test]
fn a_parent_cycle_is_refused_and_names_its_ring() {
    let source = r#"{
      "name": "s",
      "entities": [
        { "name": "A", "components": [ { "type": "HudPanel", "parent": "B" } ] },
        { "name": "B", "components": [ { "type": "HudPanel", "parent": "C" } ] },
        { "name": "C", "components": [ { "type": "HudPanel", "parent": "A" } ] }
      ]
    }"#;
    let errors = validate_source(source, "test.json");
    assert!(errors.iter().all(|e| e.error == codes::HUD_PARENT_CYCLE));
    assert!(
        errors[0].message.contains("A → B → C"),
        "the ring should be spelled out, got: {}",
        errors[0].message
    );
}

#[test]
fn nesting_past_the_cap_is_refused() {
    let mut entities = Vec::new();
    for level in 0..=(crate::ui::MAX_HUD_DEPTH + 2) {
        let parent = if level == 0 {
            String::new()
        } else {
            format!(r#", "parent": "P{}""#, level - 1)
        };
        entities.push(format!(
            r#"{{ "name": "P{level}", "components": [ {{ "type": "HudPanel"{parent} }} ] }}"#
        ));
    }
    let source = format!(
        r#"{{ "name": "s", "entities": [{}] }}"#,
        entities.join(",\n")
    );
    let codes = codes_of(&source);
    assert!(
        codes.contains(&codes::HUD_NESTING_TOO_DEEP),
        "expected a depth error, got {codes:?}"
    );
}

/// A `HudInteract` with no element has no rectangle to be, so it is a
/// button nobody can ever click — silent, and indistinguishable from a
/// broken hit test.
#[test]
fn an_interact_needs_an_element_to_be_the_hit_box() {
    let source = r#"{
      "name": "s",
      "entities": [
        { "name": "Ghost", "components": [ { "type": "HudInteract" } ] }
      ]
    }"#;
    assert_eq!(codes_of(source), [codes::HUD_INTERACT_WITHOUT_ELEMENT]);

    // On any of the four elements it is fine — the rule is about having a
    // rectangle, not about which kind of rectangle.
    for element in [
        r#"{ "type": "HudPanel" }"#,
        r#"{ "type": "HudRect", "size": [1, 1] }"#,
        r#"{ "type": "HudText", "text": "x" }"#,
    ] {
        let source = format!(
            r#"{{ "name": "s", "entities": [
                {{ "name": "B", "components": [ {element}, {{ "type": "HudInteract" }} ] }} ] }}"#
        );
        assert_eq!(codes_of(&source), Vec::<&str>::new(), "with {element}");
    }
}

#[test]
fn reports_syntax_errors_with_a_line() {
    let errors = validate_source("{\n  \"name\": \"x\",\n  oops\n}", "test.json");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].error, "invalid_json");
    assert_eq!(errors[0].context().unwrap().line, Some(3));
}

#[test]
fn every_semantic_error_carries_file_line_and_path() {
    // Invariant 6 as restated in CLAUDE.md: an error an agent cannot
    // locate from the payload alone is a bug. One scene, many distinct
    // errors — all of them must know where they are, both for humans
    // (line) and for jq (path).
    let source = r#"{
      "name": "s",
      "entities": [
        { "name": "A", "components": [ { "type": "Meterial" } ] },
        { "name": "A", "components": [ { "type": "Transform", "postion": [0, 1, 0] } ] },
        { "name": "C", "components": [ { "type": "Mesh" } ] },
        { "name": "D", "components": [ { "type": "Mesh", "asset": "meshes/x.glb" } ] },
        { "name": "E", "components": [ { "type": "Camera", "active": true } ] },
        { "name": "F", "components": [ { "type": "Camera", "active": true } ] }
      ]
    }"#;
    let errors = validate_source(source, "scene.json");
    assert!(errors.len() >= 5, "expected a pile of errors");
    for error in &errors {
        let context = error
            .context()
            .unwrap_or_else(|| panic!("{} has no context at all", error.error));
        assert_eq!(
            context.file.as_deref(),
            Some("scene.json"),
            "{}",
            error.error
        );
        assert!(
            context.line.is_some(),
            "{} carries no line: {}",
            error.error,
            error.to_json()
        );
        assert!(
            context.path.is_some(),
            "{} carries no JSON pointer: {}",
            error.error,
            error.to_json()
        );
    }
}

#[test]
fn paths_are_jq_addressable_json_pointers() {
    let source = r#"{"name":"s","entities":[
        {"name":"Cube1","components":[{"type":"Transform","postion":[0,1,0]}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(
        errors[0].context().unwrap().path.as_deref(),
        Some("/entities/0/components/0/postion")
    );
}

#[test]
fn locates_the_error_on_the_right_line() {
    let source = "{\n\"name\": \"s\",\n\"entities\": [\n{ \"name\": \"A\",\n  \"components\": [\n    { \"type\": \"Meterial\" }\n  ] }\n]\n}";
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].context().unwrap().line, Some(6));
}

#[test]
fn suggests_the_right_component_name() {
    let source = r#"{"name":"s","entities":[
        {"name":"Cube1","components":[{"type":"Meterial","albedo":[1,0,0]}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1);

    let context = errors[0].context().unwrap();
    assert_eq!(errors[0].error, "unknown_component");
    assert_eq!(context.entity.as_deref(), Some("Cube1"));
    assert_eq!(context.did_you_mean.as_deref(), Some("Material"));
}

#[test]
fn suggests_the_right_field_name() {
    let source = r#"{"name":"s","entities":[
        {"name":"Cube1","components":[{"type":"Transform","postion":[0,1,0]}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1);

    let context = errors[0].context().unwrap();
    assert_eq!(errors[0].error, "unknown_field");
    assert_eq!(context.field.as_deref(), Some("postion"));
    assert_eq!(context.did_you_mean.as_deref(), Some("position"));
}

#[test]
fn reports_a_required_field_that_is_absent() {
    let source = r#"{"name":"s","entities":[
        {"name":"Cube1","components":[{"type":"Mesh"}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors[0].error, "missing_field");
    assert_eq!(errors[0].context().unwrap().field.as_deref(), Some("asset"));
}

#[test]
fn reports_a_field_of_the_wrong_json_type() {
    let source = r#"{"name":"s","entities":[
        {"name":"Cube1","components":[{"type":"Mesh","asset":42}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].error, "invalid_field_type");
    assert_eq!(errors[0].context().unwrap().field.as_deref(), Some("asset"));
}

#[test]
fn reports_a_wrong_arity_vector() {
    let source = r#"{"name":"s","entities":[
        {"name":"Cube1","components":[{"type":"Transform","position":[0,1]}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].error, "invalid_field_type");
    assert!(
        errors[0].message.contains("exactly 3"),
        "{}",
        errors[0].message
    );
}

#[test]
fn reports_a_non_numeric_vector_element() {
    let source = r#"{"name":"s","entities":[
        {"name":"Cube1","components":[{"type":"Transform","position":[0,"one",2]}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].error, "invalid_field_type");
    assert!(
        errors[0].message.contains("position[1]"),
        "{}",
        errors[0].message
    );
    assert_eq!(
        errors[0].context().unwrap().path.as_deref(),
        Some("/entities/0/components/0/position/1")
    );
}

#[test]
fn rejects_an_unresolvable_mesh_asset_at_validation_time() {
    // Design doc §5: never a silent fallback. This mesh file does not
    // exist next to the scene, so validation must say so.
    let source = r#"{"name":"s","entities":[
        {"name":"Cube1","components":[{"type":"Mesh","asset":"meshes/cube.glb"}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].error, "asset_not_found");

    let context = errors[0].context().unwrap();
    assert_eq!(context.entity.as_deref(), Some("Cube1"));
    assert_eq!(context.field.as_deref(), Some("asset"));
}

#[test]
fn accepts_a_mesh_file_that_exists_next_to_the_scene() {
    // Asset paths resolve relative to the scene file, so validation of the
    // same source succeeds or fails with the scene's location.
    let dir = std::env::temp_dir().join(format!("engine-validate-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("meshes")).unwrap();
    std::fs::write(dir.join("meshes/thing.gltf"), b"{}").unwrap();

    let source = r#"{"name":"s","entities":[
        {"name":"Thing","components":[{"type":"Mesh","asset":"meshes/thing.gltf"}]}
    ]}"#;
    let scene_path = dir.join("scene.json").display().to_string();
    let errors = validate_source(source, &scene_path);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(errors.is_empty(), "{:?}", errors);
}

/// §5's rule, and the reason it is checked against the raw JSON: the
/// parsed component cannot tell an override from a default.
#[test]
fn a_material_is_a_file_or_a_set_of_fields_never_both() {
    let dir = std::env::temp_dir().join(format!("engine-material-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("materials")).unwrap();
    std::fs::write(
        dir.join("materials/asphalt.json"),
        br#"{"albedo": [0.2, 0.2, 0.2], "roughness": 0.7}"#,
    )
    .unwrap();
    let scene_path = dir.join("scene.json").display().to_string();

    let reference = r#"{"name":"s","entities":[
        {"name":"Road1","components":[
            {"type":"Mesh","asset":"builtin:cube"},
            {"type":"Material","asset":"materials/asphalt.json"}]}
    ]}"#;
    assert!(
        validate_source(reference, &scene_path).is_empty(),
        "a reference alone is the supported form"
    );

    // Even a field written at exactly the file's own value is refused:
    // the point is that the *resolved* material must not depend on
    // information the file does not carry.
    let both = r#"{"name":"s","entities":[
        {"name":"Road1","components":[
            {"type":"Mesh","asset":"builtin:cube"},
            {"type":"Material","asset":"materials/asphalt.json","roughness":0.7}]}
    ]}"#;
    let errors = validate_source(both, &scene_path);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].error, "material_asset_with_fields");
    assert!(
        errors[0].message.contains("roughness"),
        "{}",
        errors[0].message
    );

    // A missing file is reported at the *scene's* line; a malformed one
    // carries the material file's own (M9's clip precedent).
    let missing = r#"{"name":"s","entities":[
        {"name":"Road1","components":[
            {"type":"Mesh","asset":"builtin:cube"},
            {"type":"Material","asset":"materials/gone.json"}]}
    ]}"#;
    let errors = validate_source(missing, &scene_path);
    assert_eq!(errors[0].error, "asset_not_found");
    assert_eq!(
        errors[0].context().unwrap().file.as_deref(),
        Some(scene_path.as_str())
    );

    std::fs::write(
        dir.join("materials/broken.json"),
        b"{\n  \"roughness\": 4.0\n}",
    )
    .unwrap();
    let broken = r#"{"name":"s","entities":[
        {"name":"Road1","components":[
            {"type":"Mesh","asset":"builtin:cube"},
            {"type":"Material","asset":"materials/broken.json"}]}
    ]}"#;
    let errors = validate_source(broken, &scene_path);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(errors[0].error, "value_out_of_range", "{errors:?}");
    assert!(
        errors[0]
            .context()
            .unwrap()
            .file
            .as_deref()
            .is_some_and(|f| f.ends_with("broken.json")),
        "a material file's errors carry its own file, not the scene's"
    );
    assert_eq!(errors[0].context().unwrap().line, Some(2));
}

/// A material file is validated with the same walk the component gets, so
/// a typo in one is caught with a suggestion rather than ignored.
#[test]
fn a_material_file_is_walked_like_the_component() {
    let errors = validate_material_source(r#"{"roughnes": 0.5}"#, "m.json");
    assert_eq!(errors[0].error, "unknown_field");
    assert_eq!(
        errors[0].context().unwrap().did_you_mean.as_deref(),
        Some("roughness")
    );

    // It is not a component and it may not chain to another file.
    let errors = validate_material_source(r#"{"type": "Material"}"#, "m.json");
    assert_eq!(errors[0].error, "unknown_field");
    assert_eq!(errors[0].context().unwrap().field.as_deref(), Some("type"));
    let errors = validate_material_source(r#"{"asset": "other.json"}"#, "m.json");
    assert_eq!(errors[0].context().unwrap().field.as_deref(), Some("asset"));

    assert!(validate_material_source(r#"{"metallic": 1.0}"#, "m.json").is_empty());
}

#[test]
fn rejects_an_unresolvable_texture_map() {
    let source = r#"{"name":"s","entities":[
        {"name":"Cube1","components":[
            {"type":"Mesh","asset":"builtin:cube"},
            {"type":"Material","albedo_map":"textures/bark.png"}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].error, "asset_not_found");
    assert_eq!(
        errors[0].context().unwrap().field.as_deref(),
        Some("albedo_map")
    );

    let wrong_format = r#"{"name":"s","entities":[
        {"name":"Cube1","components":[
            {"type":"Mesh","asset":"builtin:cube"},
            {"type":"Material","normal_map":"textures/bark.tga"}]}
    ]}"#;
    let errors = validate_source(wrong_format, "test.json");
    assert_eq!(errors[0].error, "asset_unsupported");
}

#[test]
fn rejects_a_mesh_format_the_loader_does_not_read() {
    let source = r#"{"name":"s","entities":[
        {"name":"Cube1","components":[{"type":"Mesh","asset":"meshes/cube.obj"}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].error, "asset_unsupported");
}

#[test]
fn suggests_a_near_miss_builtin_asset() {
    let source = r#"{"name":"s","entities":[
        {"name":"Cube1","components":[{"type":"Mesh","asset":"builtin:cuve"}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors[0].error, "asset_not_found");
    assert_eq!(
        errors[0].context().unwrap().did_you_mean.as_deref(),
        Some("builtin:cube")
    );
}

#[test]
fn rejects_more_than_one_directional_or_ambient_light() {
    let source = r#"{"name":"s","entities":[
        {"name":"SunA","components":[{"type":"DirectionalLight"}]},
        {"name":"SunB","components":[{"type":"DirectionalLight"}]},
        {"name":"FillA","components":[{"type":"AmbientLight"}]},
        {"name":"FillB","components":[{"type":"AmbientLight"}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 2, "{errors:?}");

    let sun = errors
        .iter()
        .find(|e| e.error == "multiple_directional_lights")
        .expect("surplus sun should be flagged");
    assert_eq!(
        sun.context().unwrap().candidates,
        Some(vec!["SunA".to_string(), "SunB".to_string()])
    );
    assert!(
        sun.context().unwrap().line.is_some(),
        "must point at the surplus component"
    );

    assert!(errors.iter().any(|e| e.error == "multiple_ambient_lights"));
}

#[test]
fn allows_several_point_lights_but_not_more_than_the_shader_holds() {
    // Point lights are the one light component that is plural, so the
    // "more than one" rule must NOT apply to them…
    let mut entities: Vec<String> = (0..crate::components::MAX_POINT_LIGHTS)
        .map(|i| {
            format!(
                r#"{{"name":"Lamp{i}","components":[
                    {{"type":"Transform","position":[{i}.0,1.0,0.0]}},
                    {{"type":"PointLight","intensity":2.0}}
                ]}}"#
            )
        })
        .collect();
    let source = format!(r#"{{"name":"s","entities":[{}]}}"#, entities.join(","));
    let errors = validate_source(&source, "s.json");
    assert!(
        errors.is_empty(),
        "{} point lights must be fine, got {errors:?}",
        crate::components::MAX_POINT_LIGHTS
    );

    // …but the shader's array is fixed-size, and one past it is an error
    // rather than a light that silently never shines.
    entities.push(
        r#"{"name":"Surplus","components":[
            {"type":"Transform"},
            {"type":"PointLight"}
        ]}"#
        .to_string(),
    );
    let source = format!(r#"{{"name":"s","entities":[{}]}}"#, entities.join(","));
    let errors = validate_source(&source, "s.json");
    let surplus = errors
        .iter()
        .find(|e| e.error == "too_many_point_lights")
        .expect("the ninth point light must be reported");
    assert!(
        surplus.context().unwrap().line.is_some(),
        "must point at the surplus component"
    );
}

#[test]
fn rejects_out_of_range_point_light_values() {
    let source = r#"{"name":"s","entities":[
        {"name":"Lamp","components":[
            {"type":"Transform"},
            {"type":"PointLight","intensity":-1.0,"range":0.0,"color":[2.0,0.0,0.0]}
        ]}
    ]}"#;
    let errors = validate_source(source, "s.json");
    let fields: Vec<&str> = errors
        .iter()
        .filter(|e| e.error == "value_out_of_range")
        .filter_map(|e| e.context().and_then(|c| c.field.as_deref()))
        .collect();
    // All three at once, the M5 rule: which command you ran must never
    // change what you learn about a broken scene.
    for expected in ["intensity", "range", "color"] {
        assert!(
            fields.contains(&expected),
            "expected {expected} out of range, got {fields:?}"
        );
    }
}

#[test]
fn rejects_an_unknown_particle_blend_with_a_suggestion() {
    // `blend` is the ParticleEmitter's first closed-vocabulary field, so it
    // rides the same enum path `RigidBody.body` and `Collider.shape` do —
    // including the typo suggestion, which only works while
    // `ParticleBlend`'s variants stay undocumented (see components.rs).
    let source = r#"{"name":"s","entities":[
        {"name":"Puff","components":[
            {"type":"Transform"},
            {"type":"ParticleEmitter","blend":"addative"}
        ]}
    ]}"#;
    let errors = validate_source(source, "s.json");
    let bad = errors
        .iter()
        .find(|e| e.context().and_then(|c| c.field.as_deref()) == Some("blend"))
        .expect("an unknown blend mode must be reported");
    assert_eq!(
        bad.context().unwrap().did_you_mean.as_deref(),
        Some("additive"),
        "a near-miss blend mode should be suggested"
    );
}

#[test]
fn accepts_one_sun_and_one_ambient() {
    let source = r#"{"name":"s","entities":[
        {"name":"Sun","components":[
            {"type":"Transform","rotation":[-50.0,30.0,0.0]},
            {"type":"DirectionalLight","color":[1.0,1.0,1.0],"intensity":1.0}
        ]},
        {"name":"Fill","components":[{"type":"AmbientLight","intensity":0.05}]}
    ]}"#;
    assert!(codes_of(source).is_empty());
}

#[test]
fn rejects_out_of_range_material_and_light_values() {
    // One run reports every violation: both bad albedo channels, the bad
    // roughness, and the negative intensity.
    let source = r#"{"name":"s","entities":[
        {"name":"Bad","components":[
            {"type":"Mesh","asset":"builtin:cube"},
            {"type":"Material","albedo":[1.5,-0.25,0.5],"roughness":1.5},
            {"type":"DirectionalLight","intensity":-2.0}
        ]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    let out_of_range: Vec<_> = errors
        .iter()
        .filter(|e| e.error == "value_out_of_range")
        .collect();
    assert_eq!(out_of_range.len(), 4, "{errors:?}");

    for error in &out_of_range {
        let context = error.context().unwrap();
        assert_eq!(context.entity.as_deref(), Some("Bad"));
        assert!(context.line.is_some(), "{}", error.to_json());
    }

    let albedo_messages: Vec<&str> = out_of_range
        .iter()
        .filter(|e| e.context().unwrap().field.as_deref() == Some("albedo"))
        .map(|e| e.message.as_str())
        .collect();
    assert_eq!(albedo_messages.len(), 2, "both bad channels reported");
    assert!(albedo_messages
        .iter()
        .any(|m| m.contains("albedo[0] is 1.5")));
    assert!(
        albedo_messages
            .iter()
            .any(|m| m.contains("the allowed range is [0, 1]")),
        "{albedo_messages:?}"
    );

    assert!(out_of_range
        .iter()
        .any(
            |e| e.context().unwrap().field.as_deref() == Some("intensity")
                && e.message.contains("at least 0")
        ));
}

#[test]
fn rejects_camera_values_the_projection_cannot_survive() {
    // fov 0, negative near, far below near: all validate today upstream
    // of M5 and render garbage or nothing. Gap 2 closed.
    let source = r#"{"name":"s","entities":[
        {"name":"EyeA","components":[{"type":"Camera","fov":0.0,"active":true}]},
        {"name":"EyeB","components":[{"type":"Camera","near":-1.0}]},
        {"name":"EyeC","components":[{"type":"Camera","near":0.1,"far":0.05}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    let ranges: Vec<_> = errors
        .iter()
        .filter(|e| e.error == "value_out_of_range")
        .collect();
    assert_eq!(ranges.len(), 3, "{errors:?}");

    assert!(ranges.iter().any(|e| e.message.contains("fov is 0")
        && e.message.contains("greater than 0 and less than 180")));
    assert!(ranges
        .iter()
        .any(|e| e.message.contains("near is -1") && e.message.contains("greater than 0")));
    assert!(ranges.iter().any(
        |e| e.message.contains("far is 0.05") && e.message.contains("greater than near (0.1)")
    ));
}

#[test]
fn rejects_a_duplicate_component() {
    let source = r#"{"name":"s","entities":[
        {"name":"Cube1","components":[
            {"type":"Mesh","asset":"builtin:cube"},
            {"type":"Transform","position":[0,1,0]},
            {"type":"Transform","position":[0,2,0]}
        ]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].error, "duplicate_component");

    let context = errors[0].context().unwrap();
    assert_eq!(context.entity.as_deref(), Some("Cube1"));
    assert_eq!(context.component.as_deref(), Some("Transform"));
    assert_eq!(
        context.path.as_deref(),
        Some("/entities/0/components/2"),
        "must point at the surplus copy"
    );
}

#[test]
fn warns_about_a_material_with_no_mesh() {
    let source = r#"{"name":"s","entities":[
        {"name":"Oops","components":[{"type":"Material","albedo":[1,0,0]}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].error, "unused_material");
    assert!(errors[0].is_warning(), "must carry severity: warning");
    assert!(
        errors[0].context().unwrap().line.is_some(),
        "warnings carry full context too"
    );
}

#[test]
fn warns_about_a_zero_scale_axis() {
    let source = r#"{"name":"s","entities":[
        {"name":"Flat","components":[
            {"type":"Transform","scale":[1.0,0.0,1.0]},
            {"type":"Mesh","asset":"builtin:cube"}
        ]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].error, "zero_scale");
    assert!(errors[0].is_warning());
    assert_eq!(
        errors[0].context().unwrap().path.as_deref(),
        Some("/entities/0/components/0/scale")
    );
}

#[test]
fn warnings_do_not_hide_errors_and_vice_versa() {
    let source = r#"{"name":"s","entities":[
        {"name":"Oops","components":[{"type":"Material","metallic":2.0}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    let codes: Vec<&str> = errors.iter().map(|e| e.error).collect();
    assert!(codes.contains(&"value_out_of_range"), "{codes:?}");
    assert!(codes.contains(&"unused_material"), "{codes:?}");
}

#[test]
fn range_violations_do_not_mask_other_checks() {
    // A component with a bad value AND a surplus camera both report; a
    // range violation must also not stop the camera flags from being
    // collected, or two active cameras with bad fovs would sneak through.
    let source = r#"{"name":"s","entities":[
        {"name":"A","components":[{"type":"Camera","fov":200.0,"active":true}]},
        {"name":"B","components":[{"type":"Camera","active":true}]}
    ]}"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"value_out_of_range"), "{codes:?}");
    assert!(codes.contains(&"multiple_active_cameras"), "{codes:?}");
}

#[test]
fn suggests_the_right_light_component_name() {
    // The m4 verification scene's error path: a misspelled light.
    let source = r#"{"name":"s","entities":[
        {"name":"Sun","components":[{"type":"DirectionelLight"}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].error, "unknown_component");
    assert_eq!(
        errors[0].context().unwrap().did_you_mean.as_deref(),
        Some("DirectionalLight")
    );
}

const PHYSICS_VALID: &str = r#"{"name":"p","physics":{"gravity":[0.0,-9.81,0.0],"timestep_hz":60},"entities":[
    {"name":"Ground","components":[
        {"type":"Transform","scale":[10.0,1.0,10.0]},
        {"type":"Collider","shape":"cuboid","half_extents":[5.0,0.05,5.0]}
    ]},
    {"name":"Cube","components":[
        {"type":"Transform","position":[0.0,5.0,0.0]},
        {"type":"RigidBody","body":"dynamic"},
        {"type":"Collider","shape":"cuboid","half_extents":[0.5,0.5,0.5]}
    ]}
]}"#;

#[test]
fn accepts_a_valid_physics_scene() {
    assert!(
        codes_of(PHYSICS_VALID).is_empty(),
        "{:?}",
        validate_source(PHYSICS_VALID, "t")
    );
}

#[test]
fn suggests_shape_and_body_kind_typos() {
    let source = r#"{"name":"s","entities":[
        {"name":"A","components":[
            {"type":"Transform"},
            {"type":"RigidBody","body":"dynmaic"},
            {"type":"Collider","shape":"cubiod","half_extents":[0.5,0.5,0.5]}
        ]}
    ]}"#;
    let errors = validate_source(source, "test.json");

    let body = errors
        .iter()
        .find(|e| e.error == "unknown_body_kind")
        .unwrap();
    assert_eq!(
        body.context().unwrap().did_you_mean.as_deref(),
        Some("dynamic")
    );

    let shape = errors.iter().find(|e| e.error == "unknown_shape").unwrap();
    assert_eq!(
        shape.context().unwrap().did_you_mean.as_deref(),
        Some("cuboid")
    );
}

#[test]
fn hud_components_validate_through_the_schema() {
    // Anchor typo: the generic enum path, with did_you_mean. Ranges:
    // size >= 4, colors in [0, 1], opacity in [0, 1], rect size >= 0 —
    // all authored as schemars attributes, all caught by the walk.
    let source = r#"{"name":"s","entities":[
        {"name":"Label","components":[
            {"type":"HudText","text":"HI","anchor":"top_lft","size":2.0,"color":[2.0,0.0,0.0]}
        ]},
        {"name":"Bar","components":[
            {"type":"HudRect","size":[-1.0,5.0],"opacity":1.5}
        ]}
    ]}"#;
    let errors = validate_source(source, "test.json");

    let anchor = errors
        .iter()
        .find(|e| e.context().and_then(|c| c.field.as_deref()) == Some("anchor"))
        .unwrap();
    assert_eq!(anchor.error, "invalid_field_type");
    assert_eq!(
        anchor.context().unwrap().did_you_mean.as_deref(),
        Some("top_left")
    );

    let range_fields: Vec<&str> = errors
        .iter()
        .filter(|e| e.error == "value_out_of_range")
        .filter_map(|e| e.context().and_then(|c| c.path.as_deref()))
        .collect();
    for expected in ["size", "color/0", "size/0", "opacity"] {
        assert!(
            range_fields.iter().any(|p| p.ends_with(expected)),
            "missing range error for {expected}: {range_fields:?}"
        );
    }

    // A well-formed pair of HUD components validates clean.
    let valid = r#"{"name":"s","entities":[
        {"name":"Label","components":[{"type":"HudText","text":"HI","anchor":"bottom_right"}]},
        {"name":"Bar","components":[{"type":"HudRect","size":[0.0,0.0]}]}
    ]}"#;
    assert!(
        validate_source(valid, "t").is_empty(),
        "{:?}",
        validate_source(valid, "t")
    );
}

#[test]
fn rejects_a_dynamic_body_without_a_collider() {
    let source = r#"{"name":"s","entities":[
        {"name":"Faller","components":[
            {"type":"Transform"},{"type":"RigidBody","body":"dynamic"}
        ]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].error, "missing_collider");
    assert_eq!(
        errors[0].context().unwrap().entity.as_deref(),
        Some("Faller")
    );
}

#[test]
fn fixed_and_kinematic_bodies_need_no_collider() {
    let source = r#"{"name":"s","entities":[
        {"name":"A","components":[{"type":"Transform"},{"type":"RigidBody","body":"fixed"}]},
        {"name":"B","components":[{"type":"Transform"},{"type":"RigidBody","body":"kinematic"}]}
    ]}"#;
    assert!(codes_of(source).is_empty());
}

#[test]
fn rejects_physics_components_without_a_transform() {
    let source = r#"{"name":"s","entities":[
        {"name":"A","components":[{"type":"RigidBody","body":"fixed"}]},
        {"name":"B","components":[{"type":"Collider","shape":"sphere","radius":0.5}]}
    ]}"#;
    let codes = codes_of(source);
    assert_eq!(
        codes.iter().filter(|c| **c == "missing_transform").count(),
        2,
        "{codes:?}"
    );
}

#[test]
fn rejects_nonuniform_scale_on_round_colliders() {
    let source = r#"{"name":"s","entities":[
        {"name":"Squished","components":[
            {"type":"Transform","scale":[1.0,2.0,1.0]},
            {"type":"Collider","shape":"sphere","radius":0.5}
        ]},
        {"name":"FineCuboid","components":[
            {"type":"Transform","scale":[1.0,2.0,1.0]},
            {"type":"Collider","shape":"cuboid","half_extents":[0.5,0.5,0.5]}
        ]}
    ]}"#;
    assert_eq!(codes_of(source), ["nonuniform_scale_on_round_collider"]);
}

#[test]
fn wheels_need_a_real_chassis_and_no_physics_of_their_own() {
    // A correct vehicle: chassis + one wheel — no errors.
    let good = r#"{"name":"s","entities":[
        {"name":"Car","components":[
            {"type":"Transform"},
            {"type":"RigidBody","body":"dynamic"},
            {"type":"Collider","shape":"cuboid","half_extents":[1.0,0.5,2.0]}
        ]},
        {"name":"WheelFL","components":[
            {"type":"Transform"},
            {"type":"Wheel","vehicle":"Car","offset":[-0.8,0.0,-1.2]}
        ]}
    ]}"#;
    assert_eq!(codes_of(good), Vec::<&str>::new());

    // Typo'd chassis name: not found, with a suggestion.
    let typo = good.replace(r#""vehicle":"Car","#, r#""vehicle":"Carr","#);
    let errors = validate_source(&typo, "test.json");
    assert_eq!(errors[0].error, "wheel_vehicle_not_found");
    assert_eq!(
        errors[0].context().unwrap().did_you_mean.as_deref(),
        Some("Car")
    );

    // Chassis without a dynamic body is invalid.
    let fixed = good.replace(r#""body":"dynamic""#, r#""body":"fixed""#);
    assert_eq!(codes_of(&fixed), ["wheel_vehicle_invalid"]);

    // A wheel cannot be its own chassis.
    let own = good.replace(r#""vehicle":"Car","#, r#""vehicle":"WheelFL","#);
    assert_eq!(codes_of(&own), ["wheel_vehicle_invalid"]);

    // A wheel entity with its own collider: the chassis owns collision.
    let armored = good.replace(
        r#"{"type":"Wheel","vehicle":"Car","#,
        r#"{"type":"Collider","shape":"sphere","radius":0.3},
           {"type":"Wheel","vehicle":"Car","#,
    );
    assert_eq!(codes_of(&armored), ["wheel_with_physics"]);

    // And it needs a Transform for physics to write the pose into.
    let bare = good.replace(
        r#"{"type":"Transform"},
            {"type":"Wheel""#,
        r#"{"type":"Wheel""#,
    );
    assert_eq!(codes_of(&bare), ["missing_transform"]);
}

#[test]
fn a_tree_is_the_entitys_geometry_and_carries_its_own_material() {
    // The happy path, and the thing that makes a Tree different from every
    // other component: a Material with no Mesh next to it is *not* the
    // unused_material warning here, because the Material is the bark.
    let good = r#"{"name":"s","entities":[
        {"name":"Oak","components":[
            {"type":"Transform","position":[0.0,0.0,0.0]},
            {"type":"Tree","seed":3,"levels":2},
            {"type":"Material","albedo":[0.2,0.14,0.09]}
        ]}
    ]}"#;
    assert_eq!(codes_of(good), Vec::<&str>::new());

    // A Tree and a Mesh on one entity would draw both at one transform.
    let doubled = good.replace(
        r#"{"type":"Tree""#,
        r#"{"type":"Mesh","asset":"builtin:cube"},{"type":"Tree""#,
    );
    assert_eq!(codes_of(&doubled), ["tree_with_mesh"]);

    // Branching is exponential, so the combination of in-range fields can
    // still be absurd; refusing names a number rather than hanging.
    let huge = good.replace(
        r#""seed":3,"levels":2"#,
        r#""seed":3,"levels":4,"branches":12,"whorl":6,"sides":12,"segments":8"#,
    );
    let errors = validate_source(&huge, "test.json");
    assert_eq!(errors[0].error, "tree_too_complex");
    assert!(
        errors[0].message.contains("vertices"),
        "the error should name the number: {}",
        errors[0].message
    );

    // Typos in the closed leaf vocabulary get a suggestion like any other.
    let typo = good.replace(r#""levels":2"#, r#""levels":2,"leaf":"cluser""#);
    let errors = validate_source(&typo, "test.json");
    assert_eq!(errors[0].error, "invalid_field_type");
    assert_eq!(
        errors[0].context().unwrap().did_you_mean.as_deref(),
        Some("cluster")
    );
}

#[test]
fn enforces_per_shape_collider_fields() {
    let source = r#"{"name":"s","entities":[
        {"name":"A","components":[
            {"type":"Transform"},
            {"type":"Collider","shape":"sphere","half_extents":[0.5,0.5,0.5]}
        ]},
        {"name":"B","components":[
            {"type":"Transform"},
            {"type":"Collider","shape":"cuboid"}
        ]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    let codes: Vec<&str> = errors.iter().map(|e| e.error).collect();
    // Sphere: missing radius AND stray half_extents; cuboid: missing half_extents.
    assert!(
        codes.iter().filter(|c| **c == "missing_field").count() == 2,
        "{errors:?}"
    );
    assert!(codes.contains(&"shape_field_mismatch"), "{errors:?}");
}

#[test]
fn rejects_non_positive_shape_dimensions() {
    let source = r#"{"name":"s","entities":[
        {"name":"A","components":[
            {"type":"Transform"},
            {"type":"Collider","shape":"cuboid","half_extents":[0.5,0.0,0.5]}
        ]},
        {"name":"B","components":[
            {"type":"Transform"},
            {"type":"Collider","shape":"sphere","radius":-1.0}
        ]}
    ]}"#;
    let codes = codes_of(source);
    assert_eq!(
        codes
            .iter()
            .filter(|c| **c == "invalid_shape_dimension")
            .count(),
        2,
        "{codes:?}"
    );
}

#[test]
fn accepts_a_valid_breakable() {
    let source = r#"{"name":"s","entities":[
        {"name":"Crate","components":[
            {"type":"Transform"},
            {"type":"Mesh","asset":"builtin:cube"},
            {"type":"Collider","shape":"cuboid","half_extents":[0.5,0.5,0.5]},
            {"type":"Breakable","impulse_threshold":8.0,"fragments":[
                {"mesh":"builtin:cube","offset":[-0.25,0.0,0.0],"scale":[0.5,0.5,0.5]},
                {"mesh":"builtin:sphere","rotation":[0.0,30.0,0.0],
                 "half_extents":[0.2,0.2,0.2],"density":2.0}
            ]}
        ]}
    ]}"#;
    assert!(validate_source(source, "test.json").is_empty());
}

#[test]
fn rejects_an_empty_fragments_list() {
    // An empty Vec still *parses* — the minimum is a range check, not a
    // shape error, per the corpus agreement property.
    let source = r#"{"name":"s","entities":[
        {"name":"Crate","components":[
            {"type":"Transform"},
            {"type":"Breakable","fragments":[]}
        ]}
    ]}"#;
    assert_eq!(codes_of(source), ["value_out_of_range"]);
}

#[test]
fn rejects_unknown_fragment_fields_with_a_suggestion() {
    let source = r#"{"name":"s","entities":[
        {"name":"Crate","components":[
            {"type":"Transform"},
            {"type":"Breakable","fragments":[
                {"mesh":"builtin:cube","ofset":[0.1,0.0,0.0]}
            ]}
        ]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].error, "unknown_field");
    assert_eq!(
        errors[0].context().unwrap().did_you_mean.as_deref(),
        Some("offset")
    );
    assert_eq!(
        errors[0].context().unwrap().path.as_deref(),
        Some("/entities/0/components/1/fragments/0/ofset")
    );
}

#[test]
fn rejects_a_fragment_missing_its_mesh() {
    let source = r#"{"name":"s","entities":[
        {"name":"Crate","components":[
            {"type":"Transform"},
            {"type":"Breakable","fragments":[{"offset":[0.1,0.0,0.0]}]}
        ]}
    ]}"#;
    assert_eq!(codes_of(source), ["missing_field"]);
}

#[test]
fn suggests_a_near_miss_fragment_mesh() {
    let source = r#"{"name":"s","entities":[
        {"name":"Crate","components":[
            {"type":"Transform"},
            {"type":"Breakable","fragments":[{"mesh":"builtin:cubee"}]}
        ]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].error, "asset_not_found");
    assert_eq!(
        errors[0].context().unwrap().did_you_mean.as_deref(),
        Some("builtin:cube")
    );
    assert_eq!(
        errors[0].context().unwrap().path.as_deref(),
        Some("/entities/0/components/1/fragments/0/mesh")
    );
}

#[test]
fn rejects_non_positive_fragment_half_extents() {
    let source = r#"{"name":"s","entities":[
        {"name":"Crate","components":[
            {"type":"Transform"},
            {"type":"Breakable","fragments":[
                {"mesh":"builtin:cube","half_extents":[0.2,0.0,0.2]}
            ]}
        ]}
    ]}"#;
    assert_eq!(codes_of(source), ["invalid_shape_dimension"]);
}

#[test]
fn rejects_a_thresholded_breakable_without_a_collider() {
    let source = r#"{"name":"s","entities":[
        {"name":"Crate","components":[
            {"type":"Transform"},
            {"type":"Breakable","impulse_threshold":5.0,
             "fragments":[{"mesh":"builtin:cube"}]}
        ]}
    ]}"#;
    assert_eq!(codes_of(source), ["breakable_without_collider"]);

    // Omitting the threshold makes it script/explosion-only: no collider
    // needed.
    let source = r#"{"name":"s","entities":[
        {"name":"Crate","components":[
            {"type":"Transform"},
            {"type":"Breakable","fragments":[{"mesh":"builtin:cube"}]}
        ]}
    ]}"#;
    assert!(validate_source(source, "test.json").is_empty());
}

/// The road's half of the "a generated surface is one source of truth"
/// rule, matching water's.
#[test]
fn a_road_entity_owns_its_surface_alone() {
    let bare = r#"{"name":"s","entities":[
        {"name":"Circuit","components":[
            {"type":"Transform"},
            {"type":"Road","points":[
                {"position":[0.0,0.0,0.0]},{"position":[0.0,0.0,-20.0]}]}
        ]}
    ]}"#;
    assert!(validate_source(bare, "test.json").is_empty());

    let with_mesh = r#"{"name":"s","entities":[
        {"name":"Circuit","components":[
            {"type":"Transform"},
            {"type":"Road","points":[
                {"position":[0.0,0.0,0.0]},{"position":[0.0,0.0,-20.0]}]},
            {"type":"Mesh","asset":"builtin:cube"}
        ]}
    ]}"#;
    assert_eq!(codes_of(with_mesh), ["road_with_mesh"]);

    let with_material = r#"{"name":"s","entities":[
        {"name":"Circuit","components":[
            {"type":"Transform"},
            {"type":"Road","points":[
                {"position":[0.0,0.0,0.0]},{"position":[0.0,0.0,-20.0]}]},
            {"type":"Material"}
        ]}
    ]}"#;
    assert_eq!(
        codes_of(with_material),
        ["road_with_mesh"],
        "a Material on a road is an error, not the unused_material warning"
    );
}

/// A bare `{"type": "Road"}` is a road: 20 m of straight, so adding the
/// component in the editor shows one instead of invalidating the scene.
#[test]
fn a_bare_road_is_a_road() {
    let source = r#"{"name":"s","entities":[
        {"name":"Street","components":[
            {"type":"Transform"},
            {"type":"Road"}
        ]}
    ]}"#;
    assert!(validate_source(source, "test.json").is_empty());
}

#[test]
fn a_road_needs_enough_points_to_be_a_road() {
    let source = r#"{"name":"s","entities":[
        {"name":"Circuit","components":[
            {"type":"Transform"},
            {"type":"Road","points":[{"position":[0.0,0.0,0.0]}]}
        ]}
    ]}"#;
    assert_eq!(codes_of(source), ["road_too_few_points"]);

    // Two points make a road but not a circuit.
    let closed = r#"{"name":"s","entities":[
        {"name":"Circuit","components":[
            {"type":"Transform"},
            {"type":"Road","closed":true,"points":[
                {"position":[0.0,0.0,0.0]},{"position":[0.0,0.0,-20.0]}]}
        ]}
    ]}"#;
    assert_eq!(codes_of(closed), ["road_too_few_points"]);
}

/// The two things a polygon of corners cannot guarantee about itself, both
/// of which render as a road that has crossed through itself — a bad thing
/// to have to diagnose from a screenshot.
#[test]
fn a_corner_that_cannot_be_built_is_refused() {
    let overlapping = r#"{"name":"s","entities":[
        {"name":"Circuit","components":[
            {"type":"Transform"},
            {"type":"Road","closed":true,"points":[
                {"position":[-20.0,0.0,-20.0],"radius":40.0},
                {"position":[20.0,0.0,-20.0],"radius":40.0},
                {"position":[0.0,0.0,20.0],"radius":40.0}]}
        ]}
    ]}"#;
    let codes = codes_of(overlapping);
    assert!(
        codes.iter().all(|c| *c == "road_corner_does_not_fit"),
        "{codes:?}"
    );
    assert!(!codes.is_empty(), "40 m radii do not fit 40 m edges");

    let folded = r#"{"name":"s","entities":[
        {"name":"Circuit","components":[
            {"type":"Transform"},
            {"type":"Road","closed":true,"points":[
                {"position":[-20.0,0.0,-20.0]},
                {"position":[20.0,0.0,-20.0]},
                {"position":[0.0,0.0,20.0]}]}
        ]}
    ]}"#;
    let codes = codes_of(folded);
    assert!(
        codes.contains(&"road_corner_needs_radius"),
        "a 120-degree sharp vertex cannot be mitred: {codes:?}"
    );
}

/// A road with a collider is physics geometry, and physics only sees
/// entities that have a Transform — so a road without one is an error
/// (the collider check\'s, not a second road-shaped copy of it) rather
/// than a road the car falls straight through.
#[test]
fn a_road_that_collides_needs_a_transform() {
    let source = r#"{"name":"s","entities":[
        {"name":"Circuit","components":[
            {"type":"Road","points":[
                {"position":[0.0,0.0,0.0]},{"position":[0.0,0.0,-20.0]}]},
            {"type":"Collider","shape":"trimesh"}
        ]}
    ]}"#;
    assert_eq!(codes_of(source), ["missing_transform"]);
}

/// The other half of that: a trimesh collider on a road needs no asset and
/// no Mesh, because the road *is* the geometry.
#[test]
fn a_road_supplies_its_own_collider_geometry() {
    let source = r#"{"name":"s","entities":[
        {"name":"Circuit","components":[
            {"type":"Transform"},
            {"type":"Road","points":[
                {"position":[0.0,0.0,0.0]},{"position":[0.0,0.0,-20.0]}]},
            {"type":"Collider","shape":"trimesh","friction":0.9}
        ]}
    ]}"#;
    assert!(validate_source(source, "test.json").is_empty());

    // Without the road it is the error it always was.
    let orphan = r#"{"name":"s","entities":[
        {"name":"Nothing","components":[
            {"type":"Transform"},
            {"type":"Collider","shape":"trimesh"}
        ]}
    ]}"#;
    assert_eq!(codes_of(orphan), ["collider_missing_mesh"]);
}

#[test]
fn a_water_entity_owns_its_surface_alone() {
    // A Mesh or a Material beside Water is a second answer to what this
    // surface is, and the render only ever honours one of them.
    let with_mesh = r#"{"name":"s","entities":[
        {"name":"Pond","components":[
            {"type":"Transform"},
            {"type":"Water"},
            {"type":"Mesh","asset":"builtin:plane"}
        ]}
    ]}"#;
    assert_eq!(codes_of(with_mesh), ["water_with_mesh"]);

    // A Material alone is the same error — and *not* the `unused_material`
    // warning, which would be a confusing second thing to read about one
    // mistake.
    let with_material = r#"{"name":"s","entities":[
        {"name":"Pond","components":[
            {"type":"Transform"},
            {"type":"Water"},
            {"type":"Material","albedo":[0.1,0.2,0.3]}
        ]}
    ]}"#;
    assert_eq!(codes_of(with_material), ["water_with_mesh"]);

    // Water on its own, with nothing else to say: valid, and a flat
    // reflective surface is exactly what it means.
    let alone = r#"{"name":"s","entities":[
        {"name":"Pond","components":[
            {"type":"Transform","scale":[10.0,1.0,10.0]},
            {"type":"Water"}
        ]}
    ]}"#;
    assert!(validate_source(alone, "test.json").is_empty());
}

// ── Daylight (M21) ─────────────────────────────────────────────

#[test]
fn daylight_and_an_authored_sun_are_two_owners_of_one_thing() {
    // Invariant 8: a rotation in a text file that is silently ignored, or
    // silently overwritten, is a value that does not mean what it says.
    let both = r#"{"name":"s","daylight":{},"entities":[
        {"name":"Sun","components":[
            {"type":"Transform","rotation":[-40.0,0.0,0.0]},
            {"type":"DirectionalLight"}
        ]}
    ]}"#;
    assert_eq!(codes_of(both), ["daylight_and_directional_light"]);

    // `drives_sun: false` is the escape hatch, and it makes the same
    // scene legal — daylight then paints the sky and leaves the sun alone.
    let hand_aimed = r#"{"name":"s","daylight":{"drives_sun":false},"entities":[
        {"name":"Sun","components":[
            {"type":"Transform","rotation":[-40.0,0.0,0.0]},
            {"type":"DirectionalLight"}
        ]}
    ]}"#;
    assert!(validate_source(hand_aimed, "test.json").is_empty());

    // And daylight with no light entities at all is the ordinary case:
    // the block *is* the sun.
    let alone = r#"{"name":"s","daylight":{"time_of_day":7.25},"entities":[]}"#;
    assert!(validate_source(alone, "test.json").is_empty());
}

#[test]
fn authored_sky_under_daylight_warns_rather_than_failing() {
    // The `unused_material` precedent: a value nothing reads is worth
    // saying out loud, but it is not a broken scene.
    let source = r#"{"name":"s",
        "daylight":{},
        "environment":{"sky":true,"sky_zenith":[0.1,0.2,0.3]},
        "entities":[]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].error, "daylight_overrides_sky");
    assert!(errors[0].is_warning(), "this must not fail the scene");

    // An AmbientLight is unread for the same reason, and says so.
    let ambient = r#"{"name":"s","daylight":{},"entities":[
        {"name":"Sky","components":[{"type":"AmbientLight"}]}
    ]}"#;
    let errors = validate_source(ambient, "test.json");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].error, "daylight_overrides_sky");
    assert!(errors[0].is_warning());

    // `drives_sky: false` silences both, because then they are read.
    let kept = r#"{"name":"s",
        "daylight":{"drives_sky":false},
        "environment":{"sky":true,"sky_zenith":[0.1,0.2,0.3]},
        "entities":[
            {"name":"Sky","components":[{"type":"AmbientLight"}]}
        ]}"#;
    assert!(validate_source(kept, "test.json").is_empty());
}

#[test]
fn daylight_values_are_range_checked() {
    for (field, value) in [
        ("time_of_day", "24.0"), // the hour is [0, 24), open at the top
        ("time_of_day", "-1.0"),
        ("day_length", "-5.0"),
        ("sun_elevation", "0.0"), // (0, 90]: a sun that never rises
        ("sun_elevation", "91.0"),
        ("moon_elevation", "0.0"),
        ("moon_intensity", "-0.1"),
    ] {
        let source = format!(r#"{{"name":"s","daylight":{{"{field}":{value}}},"entities":[]}}"#);
        assert_eq!(
            codes_of(&source),
            ["invalid_daylight_value"],
            "daylight.{field} = {value} should have been rejected"
        );
    }

    // A typo'd field gets a suggestion, not a silent default.
    let typo = r#"{"name":"s","daylight":{"time_of_dey":6.0},"entities":[]}"#;
    let errors = validate_source(typo, "test.json");
    assert_eq!(errors[0].error, "unknown_field");
    assert_eq!(
        errors[0].context().unwrap().did_you_mean.as_deref(),
        Some("time_of_day")
    );
}

#[test]
fn a_palette_must_be_sorted_and_complete() {
    // Nine fields, all required: a half-specified keyframe silently
    // interpolating toward black is a worse failure than being told to
    // finish it.
    let full = |hour: &str| {
        format!(
            r#"{{"hour":{hour},"sun_color":[1,1,1],"sun_intensity":1.0,
                 "ambient_color":[1,1,1],"ambient_intensity":0.2,
                 "sky_zenith":[0.2,0.3,0.6],"sky_horizon":[0.6,0.7,0.8],
                 "sky_ground":[0.1,0.1,0.1],"fog_scale":1.0}}"#
        )
    };

    let sorted = format!(
        r#"{{"name":"s","daylight":{{"palette":[{},{}]}},"entities":[]}}"#,
        full("6.0"),
        full("18.0")
    );
    assert!(validate_source(&sorted, "test.json").is_empty());

    // Out of order: the table wraps, so an unsorted one has no
    // well-defined "next keyframe" and would run backwards through the day.
    let unsorted = format!(
        r#"{{"name":"s","daylight":{{"palette":[{},{}]}},"entities":[]}}"#,
        full("18.0"),
        full("6.0")
    );
    assert_eq!(codes_of(&unsorted), ["daylight_palette_invalid"]);

    // One keyframe is not a day.
    let lonely = format!(
        r#"{{"name":"s","daylight":{{"palette":[{}]}},"entities":[]}}"#,
        full("12.0")
    );
    assert_eq!(codes_of(&lonely), ["daylight_palette_invalid"]);

    // A missing field is located, not defaulted.
    let partial = format!(
        r#"{{"name":"s","daylight":{{"palette":[{{"hour":6.0}},{}]}},"entities":[]}}"#,
        full("18.0")
    );
    let errors = validate_source(&partial, "test.json");
    assert!(errors.iter().all(|e| e.error == "missing_field"));
    assert_eq!(errors.len(), 8, "eight of the nine fields are absent");
}

#[test]
fn a_terrain_entity_owns_its_surface_alone() {
    // Same rule as water: the patch generates its geometry and paints it
    // from its layers, so a Mesh or a Material beside it is a second,
    // silently ignored answer to what this ground is.
    let with_mesh = r#"{"name":"s","entities":[
        {"name":"Ground","components":[
            {"type":"Transform","scale":[100.0,1.0,100.0]},
            {"type":"Terrain"},
            {"type":"Mesh","asset":"builtin:plane"}
        ]}
    ]}"#;
    assert_eq!(codes_of(with_mesh), ["terrain_with_mesh"]);

    let with_material = r#"{"name":"s","entities":[
        {"name":"Ground","components":[
            {"type":"Transform","scale":[100.0,1.0,100.0]},
            {"type":"Terrain"},
            {"type":"Material","albedo":[0.2,0.3,0.1]}
        ]}
    ]}"#;
    // And not *also* `unused_material`: one complaint per mistake.
    assert_eq!(codes_of(with_material), ["terrain_with_mesh"]);

    // Terrain on its own: valid, and a bare `{"type": "Terrain"}` is a
    // plausible grassy patch rather than a blank.
    let alone = r#"{"name":"s","entities":[
        {"name":"Ground","components":[
            {"type":"Transform","scale":[100.0,1.0,100.0]},
            {"type":"Terrain"}
        ]}
    ]}"#;
    assert!(validate_source(alone, "test.json").is_empty());
}

#[test]
fn a_terrain_collider_may_borrow_the_generated_surface() {
    // A mesh-shaped collider with no asset normally has nothing to work
    // from; a Terrain is geometry in reach, and this is how ground becomes
    // collidable without a mesh file duplicating what the renderer draws.
    let source = r#"{"name":"s","entities":[
        {"name":"Ground","components":[
            {"type":"Transform","scale":[100.0,1.0,100.0]},
            {"type":"Terrain"},
            {"type":"Collider","shape":"trimesh"}
        ]}
    ]}"#;
    assert!(validate_source(source, "test.json").is_empty());

    // Without either, the old error still stands.
    let bare = r#"{"name":"s","entities":[
        {"name":"Nothing","components":[
            {"type":"Transform"},
            {"type":"Collider","shape":"trimesh"}
        ]}
    ]}"#;
    assert_eq!(codes_of(bare), ["collider_missing_mesh"]);
}

#[test]
fn rejects_a_terrain_layer_whose_band_runs_backwards() {
    // Silent otherwise: the layer's weight is zero everywhere, so it never
    // appears and the author goes looking in the shader for a material that
    // was never asked for.
    let source = r#"{"name":"s","entities":[
        {"name":"Ground","components":[
            {"type":"Transform"},
            {"type":"Terrain","layers":[
                {"albedo":[0.1,0.2,0.1]},
                {"albedo":[0.3,0.3,0.3],"slope_range":[70.0,30.0]}
            ]}
        ]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(codes_of(source), ["terrain_layer_range_inverted"]);
    assert_eq!(
        errors[0].context().unwrap().path.as_deref(),
        Some("/entities/0/components/1/layers/1/slope_range")
    );

    // Both bands are checked, and both are reported at once.
    let both = r#"{"name":"s","entities":[
        {"name":"Ground","components":[
            {"type":"Transform"},
            {"type":"Terrain","layers":[
                {"albedo":[0.1,0.2,0.1]},
                {"albedo":[0.3,0.3,0.3],"height_range":[4.0,1.0],
                 "slope_range":[70.0,30.0]}
            ]}
        ]}
    ]}"#;
    assert_eq!(
        codes_of(both),
        [
            "terrain_layer_range_inverted",
            "terrain_layer_range_inverted"
        ]
    );
}

#[test]
fn rejects_more_terrain_layers_than_the_shader_can_hold() {
    // The layer table is fixed-size, so a fifth layer would be dropped
    // silently — the schema's `maxItems` catches it before it can be.
    let source = r#"{"name":"s","entities":[
        {"name":"Ground","components":[
            {"type":"Transform"},
            {"type":"Terrain","layers":[
                {"albedo":[0.1,0.1,0.1]},
                {"albedo":[0.2,0.2,0.2]},
                {"albedo":[0.3,0.3,0.3]},
                {"albedo":[0.4,0.4,0.4]},
                {"albedo":[0.5,0.5,0.5]}
            ]}
        ]}
    ]}"#;
    assert_eq!(codes_of(source), ["value_out_of_range"]);
}

#[test]
fn rejects_waves_that_would_fold_the_surface() {
    // Gerstner's constraint, and the one number an author cannot infer from
    // a single wave: each is legal, the sum is not.
    let source = r#"{"name":"s","entities":[
        {"name":"Sea","components":[
            {"type":"Transform"},
            {"type":"Water","waves":[
                {"wavelength":6.0,"amplitude":0.4,"steepness":0.7},
                {"wavelength":2.0,"amplitude":0.2,"steepness":0.5}
            ]}
        ]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(
        errors.iter().map(|e| e.error).collect::<Vec<_>>(),
        ["water_waves_self_intersect"]
    );
    // The message has to carry the arithmetic: the author needs to know
    // which numbers to scale, not merely that something is too steep.
    assert!(errors[0].message.contains("1.2"), "{}", errors[0].message);
    assert!(
        errors[0].message.contains("0.7 + 0.5"),
        "{}",
        errors[0].message
    );

    // Exactly 1 is the boundary and is allowed: it is the point where the
    // surface first has a vertical tangent, not where it folds.
    let boundary = source.replace("0.5}", "0.3}");
    assert!(validate_source(&boundary, "test.json").is_empty());
}

#[test]
fn the_wave_list_is_capped_by_the_schema() {
    // The cap lives in two places — `water::MAX_WAVES` sizes the shader's
    // uniform array, `#[schemars(length(max = ...))]` rejects the scene —
    // and they have to be the same number, or a scene would validate and
    // then silently lose waves at the pipeline.
    let schema = crate::schema::component_schema();
    let water = schema["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["properties"]["type"]["const"] == "Water")
        .expect("Water is a published component");
    assert_eq!(
        water["properties"]["waves"]["maxItems"].as_u64(),
        Some(crate::water::MAX_WAVES as u64),
        "the published cap must match the one the renderer packs for"
    );

    let one = r#"{"wavelength":2.0,"amplitude":0.1,"steepness":0.05}"#;
    let waves = [one; crate::water::MAX_WAVES + 1].join(",");
    let source = format!(
        r#"{{"name":"s","entities":[
            {{"name":"Sea","components":[
                {{"type":"Transform"}},
                {{"type":"Water","waves":[{waves}]}}
            ]}}
        ]}}"#
    );
    assert_eq!(codes_of(&source), ["value_out_of_range"]);
}

#[test]
fn rejects_a_bad_timestep() {
    let source = r#"{"name":"s","physics":{"timestep_hz":0},"entities":[]}"#;
    assert_eq!(codes_of(source), ["invalid_physics_value"]);
    let source = r#"{"name":"s","physics":{"timestep_hz":60.5},"entities":[]}"#;
    assert_eq!(codes_of(source), ["invalid_physics_value"]);
}

#[test]
fn rejects_unknown_physics_block_fields() {
    let source = r#"{"name":"s","physics":{"gravty":[0,-9.81,0]},"entities":[]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors[0].error, "unknown_field");
    assert_eq!(
        errors[0].context().unwrap().did_you_mean.as_deref(),
        Some("gravity")
    );
}

#[test]
fn accepts_a_full_environment_block() {
    let source = r#"{"name":"s","environment":{
        "sky":true,"sky_zenith":[0.2,0.3,0.6],"sky_horizon":[0.6,0.7,0.8],
        "sky_ground":[0.1,0.1,0.1],"fog_density":0.01,
        "shadows":true,"shadow_distance":80.0,"samples":4},"entities":[]}"#;
    assert!(codes_of(source).is_empty(), "{:?}", codes_of(source));
}

#[test]
fn rejects_unknown_environment_block_fields() {
    let source = r#"{"name":"s","environment":{"shadow":true},"entities":[]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors[0].error, "unknown_field");
    assert_eq!(
        errors[0].context().unwrap().did_you_mean.as_deref(),
        Some("shadows")
    );
}

#[test]
fn rejects_environment_values_outside_their_range() {
    // Only 1 and 4 are real sample counts; 2 is told so rather than rounded.
    let source = r#"{"name":"s","environment":{"samples":2},"entities":[]}"#;
    assert_eq!(codes_of(source), ["invalid_environment_value"]);

    let source = r#"{"name":"s","environment":{"fog_density":-1.0},"entities":[]}"#;
    assert_eq!(codes_of(source), ["invalid_environment_value"]);

    let source = r#"{"name":"s","environment":{"shadow_distance":0.0},"entities":[]}"#;
    assert_eq!(codes_of(source), ["invalid_environment_value"]);
}

#[test]
fn rejects_mistyped_environment_fields() {
    let source = r#"{"name":"s","environment":{"sky":"yes"},"entities":[]}"#;
    assert_eq!(codes_of(source), ["invalid_field_type"]);

    let source = r#"{"name":"s","environment":{"sky_zenith":[0.2,0.3]},"entities":[]}"#;
    assert_eq!(codes_of(source), ["invalid_field_type"]);
}

/// The block reports *every* problem at once, like the rest of validation.
#[test]
fn reports_all_environment_problems_together() {
    let source =
        r#"{"name":"s","environment":{"samples":3,"fog_density":-1,"nope":1},"entities":[]}"#;
    let mut codes = codes_of(source);
    codes.sort();
    assert_eq!(
        codes,
        [
            "invalid_environment_value",
            "invalid_environment_value",
            "unknown_field"
        ]
    );
}

#[test]
fn rejects_duplicate_entity_names() {
    let source = r#"{"name":"s","entities":[{"name":"A"},{"name":"A"}]}"#;
    assert_eq!(codes_of(source), ["duplicate_entity_name"]);
}

#[test]
fn rejects_more_than_one_active_camera() {
    let source = r#"{"name":"s","entities":[
        {"name":"A","components":[{"type":"Camera","active":true}]},
        {"name":"B","components":[{"type":"Camera","active":true}]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].error, "multiple_active_cameras");
    assert_eq!(
        errors[0].context().unwrap().candidates,
        Some(vec!["A".to_string(), "B".to_string()])
    );
}

#[test]
fn accepts_exactly_one_active_camera_among_several() {
    let source = r#"{"name":"s","entities":[
        {"name":"A","components":[{"type":"Camera","active":true}]},
        {"name":"B","components":[{"type":"Camera","active":false}]}
    ]}"#;
    assert!(codes_of(source).is_empty());
}

#[test]
fn collects_every_error_rather_than_stopping_at_the_first() {
    // The whole reason this does not just defer to serde: an agent should
    // need one validate run, not four.
    let source = r#"{"name":"s","entities":[
        {"name":"A","components":[{"type":"Meterial"}]},
        {"name":"A","components":[{"type":"Transform","postion":[0,0,0]}]},
        {"name":"C","components":[{"type":"Mesh"}]}
    ]}"#;
    let codes = codes_of(source);
    assert_eq!(
        codes.len(),
        4,
        "expected four distinct errors, got {codes:?}"
    );
    assert!(codes.contains(&"unknown_component"));
    assert!(codes.contains(&"duplicate_entity_name"));
    assert!(codes.contains(&"unknown_field"));
    assert!(codes.contains(&"missing_field"));
}

#[test]
fn rejects_a_misspelled_top_level_field() {
    let source = r#"{"name":"s","entites":[]}"#;
    let errors = validate_source(source, "test.json");
    let unknown = errors
        .iter()
        .find(|e| e.error == "unknown_field")
        .expect("should flag the misspelled key");
    assert_eq!(
        unknown.context().unwrap().did_you_mean.as_deref(),
        Some("entities")
    );
}

#[test]
fn requires_entity_names() {
    let source = r#"{"name":"s","entities":[{"components":[]}]}"#;
    assert_eq!(codes_of(source), ["missing_entity_name"]);
}

// ── Collision (M12): mesh shapes and layers ────────────────────────

#[test]
fn mesh_colliders_borrow_the_entity_mesh_or_take_an_asset() {
    // Trimesh borrowing the entity's Mesh: valid.
    let borrowing = r#"{"name":"s","entities":[
        {"name":"Track","components":[
            {"type":"Transform"},
            {"type":"Mesh","asset":"builtin:plane"},
            {"type":"Collider","shape":"trimesh"}
        ]}
    ]}"#;
    assert!(
        codes_of(borrowing).is_empty(),
        "{:?}",
        validate_source(borrowing, "t")
    );

    // Trimesh with an explicit asset and no Mesh: also valid.
    let explicit = r#"{"name":"s","entities":[
        {"name":"Track","components":[
            {"type":"Transform"},
            {"type":"Collider","shape":"trimesh","asset":"builtin:plane"}
        ]}
    ]}"#;
    assert!(
        codes_of(explicit).is_empty(),
        "{:?}",
        validate_source(explicit, "t")
    );

    // Neither: there is no geometry to collide.
    let neither = r#"{"name":"s","entities":[
        {"name":"Track","components":[
            {"type":"Transform"},
            {"type":"Collider","shape":"trimesh"}
        ]}
    ]}"#;
    assert_eq!(codes_of(neither), ["collider_missing_mesh"]);
}

#[test]
fn rejects_a_trimesh_on_a_dynamic_body() {
    let source = r#"{"name":"s","entities":[
        {"name":"Rock","components":[
            {"type":"Transform"},
            {"type":"RigidBody","body":"dynamic"},
            {"type":"Collider","shape":"trimesh","asset":"builtin:cube"}
        ]}
    ]}"#;
    assert_eq!(codes_of(source), ["trimesh_on_dynamic_body"]);

    // convex_hull is the supported dynamic mesh shape.
    let hull = source.replace("trimesh", "convex_hull");
    assert!(
        codes_of(&hull).is_empty(),
        "{:?}",
        validate_source(&hull, "t")
    );
}

#[test]
fn asset_is_a_mesh_shape_field_and_gets_reference_checks() {
    let source = r#"{"name":"s","entities":[
        {"name":"A","components":[
            {"type":"Transform"},
            {"type":"Collider","shape":"cuboid","half_extents":[1.0,1.0,1.0],
             "asset":"builtin:cube"}
        ]},
        {"name":"B","components":[
            {"type":"Transform"},
            {"type":"Collider","shape":"trimesh","asset":"builtin:cubee"}
        ]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    let codes: Vec<&str> = errors.iter().map(|e| e.error).collect();
    assert!(codes.contains(&"shape_field_mismatch"), "{errors:?}");
    let bad_ref = errors
        .iter()
        .find(|e| e.error == "asset_not_found")
        .unwrap();
    assert_eq!(
        bad_ref.context().unwrap().did_you_mean.as_deref(),
        Some("builtin:cube")
    );
}

#[test]
fn warns_on_a_collides_with_layer_nobody_declares() {
    let source = r#"{"name":"s","entities":[
        {"name":"Ground","components":[
            {"type":"Transform"},
            {"type":"Collider","shape":"cuboid","half_extents":[5.0,0.1,5.0],
             "layers":["ground"]}
        ]},
        {"name":"Sensor","components":[
            {"type":"Transform"},
            {"type":"Collider","shape":"cuboid","half_extents":[1.0,1.0,1.0],
             "sensor":true,"collides_with":["gorund"]}
        ]}
    ]}"#;
    let errors = validate_source(source, "test.json");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].error, "unknown_collision_layer");
    assert!(
        errors[0].is_warning(),
        "a typo'd reference still simulates, so: warning"
    );
    assert_eq!(
        errors[0].context().unwrap().did_you_mean.as_deref(),
        Some("ground")
    );
}

#[test]
fn rejects_empty_layer_arrays_and_more_than_32_layers() {
    let empty = r#"{"name":"s","entities":[
        {"name":"A","components":[
            {"type":"Transform"},
            {"type":"Collider","shape":"cuboid","half_extents":[1.0,1.0,1.0],
             "layers":[]}
        ]}
    ]}"#;
    assert_eq!(codes_of(empty), ["empty_collision_layers"]);

    let names: Vec<String> = (0..33).map(|i| format!("\"layer{i:02}\"")).collect();
    let crowded = format!(
        r#"{{"name":"s","entities":[
            {{"name":"A","components":[
                {{"type":"Transform"}},
                {{"type":"Collider","shape":"cuboid","half_extents":[1.0,1.0,1.0],
                 "layers":[{}]}}
            ]}}
        ]}}"#,
        names.join(",")
    );
    assert_eq!(codes_of(&crowded), ["too_many_collision_layers"]);
}

#[test]
fn matching_layers_validate_clean() {
    let source = r#"{"name":"s","entities":[
        {"name":"Ground","components":[
            {"type":"Transform"},
            {"type":"Collider","shape":"cuboid","half_extents":[5.0,0.1,5.0],
             "layers":["ground"]}
        ]},
        {"name":"Player","components":[
            {"type":"Transform"},
            {"type":"RigidBody","body":"dynamic"},
            {"type":"Collider","shape":"sphere","radius":0.5,
             "layers":["player"],"collides_with":["ground"]}
        ]}
    ]}"#;
    assert!(
        codes_of(source).is_empty(),
        "{:?}",
        validate_source(source, "t")
    );
}
