//! Scene validation.
//!
//! Deliberately does *not* just call `serde_json::from_str::<SceneFile>` and
//! forward the error. serde stops at the first problem and describes it in
//! prose; an agent wants every problem at once, each as a structured record it
//! can act on without parsing English.
//!
//! So this walks the JSON tree itself, collecting errors, and only falls back
//! to serde for per-component field checking — where serde's knowledge of the
//! target type is exactly what is needed. A [`LineIndex`] built from the raw
//! source supplies the line numbers that `serde_json::Value` discards
//! (invariant 6: every error names its file and line).

use std::path::Path;

use serde_json::Value;

use crate::components::ComponentData;
use crate::error::EngineError;
use crate::lineindex::LineIndex;
use crate::mesh::MeshAsset;

/// Validate a scene file's contents. An empty result means the scene is valid.
pub fn validate_source(source: &str, path: &str) -> Vec<EngineError> {
    let root: Value = match serde_json::from_str(source) {
        Ok(value) => value,
        Err(e) => {
            // Syntax errors never reach the LineIndex; serde_json itself knows
            // the position here.
            return vec![EngineError::new("invalid_json", e.to_string())
                .file(path)
                .line(e.line() as u32)
                .column(e.column() as u32)];
        }
    };

    let cx = Cx {
        file: path,
        index: LineIndex::new(source),
    };

    let mut errors = Vec::new();

    let Some(object) = root.as_object() else {
        errors.push(cx.err(
            "scene_root_not_object",
            format!("a scene must be a JSON object, found {}", kind_of(&root)),
            "",
        ));
        return errors;
    };

    match object.get("name") {
        None => errors.push(cx.missing_field("name", "")),
        Some(v) if !v.is_string() => errors.push(cx.wrong_type("name", "string", v, "/name")),
        Some(_) => {}
    }

    for key in object.keys() {
        if key != "name" && key != "entities" {
            errors.push(
                cx.err(
                    "unknown_field",
                    format!("unknown top-level field {key:?}"),
                    &format!("/{key}"),
                )
                .field(key)
                .suggest_from(key, ["name", "entities"]),
            );
        }
    }

    let entities = match object.get("entities") {
        None => {
            errors.push(cx.missing_field("entities", ""));
            return errors;
        }
        Some(Value::Array(entities)) => entities,
        Some(other) => {
            errors.push(cx.wrong_type("entities", "array", other, "/entities"));
            return errors;
        }
    };

    let mut seen_names: Vec<&str> = Vec::with_capacity(entities.len());
    // (entity name, component path) per active camera, so the surplus-camera
    // error can point at a concrete line and list candidates.
    let mut active_cameras: Vec<(String, String)> = Vec::new();

    for (entity_index, entity) in entities.iter().enumerate() {
        let entity_path = format!("/entities/{entity_index}");

        let Some(entity) = entity.as_object() else {
            errors.push(cx.err(
                "entity_not_object",
                format!(
                    "entity at index {entity_index} must be an object, found {}",
                    kind_of(entity)
                ),
                &entity_path,
            ));
            continue;
        };

        // The entity name is load-bearing for every later error message, so
        // resolve it before anything else.
        let name = match entity.get("name") {
            Some(Value::String(name)) if !name.is_empty() => name.as_str(),
            Some(Value::String(_)) => {
                errors.push(
                    cx.err(
                        "empty_entity_name",
                        format!("entity at index {entity_index} has an empty name"),
                        &format!("{entity_path}/name"),
                    )
                    .field("name"),
                );
                continue;
            }
            Some(other) => {
                errors.push(
                    cx.wrong_type("name", "string", other, &format!("{entity_path}/name"))
                        .entity(format!("<entity at index {entity_index}>")),
                );
                continue;
            }
            None => {
                errors.push(
                    cx.err(
                        "missing_entity_name",
                        format!(
                            "entity at index {entity_index} has no name; \
                             names are how the CLI and agent edits target entities"
                        ),
                        &entity_path,
                    )
                    .field("name"),
                );
                continue;
            }
        };

        if seen_names.contains(&name) {
            errors.push(
                cx.err(
                    "duplicate_entity_name",
                    format!(
                        "more than one entity is named {name:?}; names must be unique \
                         because they are how entities are targeted"
                    ),
                    &format!("{entity_path}/name"),
                )
                .entity(name),
            );
        }
        seen_names.push(name);

        for key in entity.keys() {
            if key != "name" && key != "components" {
                errors.push(
                    cx.err(
                        "unknown_field",
                        format!("unknown entity field {key:?}"),
                        &format!("{entity_path}/{key}"),
                    )
                    .entity(name)
                    .field(key)
                    .suggest_from(key, ["name", "components"]),
                );
            }
        }

        let components = match entity.get("components") {
            None => continue,
            Some(Value::Array(components)) => components,
            Some(other) => {
                errors.push(
                    cx.wrong_type(
                        "components",
                        "array",
                        other,
                        &format!("{entity_path}/components"),
                    )
                    .entity(name),
                );
                continue;
            }
        };

        for (component_index, component) in components.iter().enumerate() {
            let component_path = format!("{entity_path}/components/{component_index}");
            match check_component(&cx, component, name, &component_path) {
                Ok(Checked {
                    active_camera: true,
                }) => {
                    active_cameras.push((name.to_string(), component_path));
                }
                Ok(_) => {}
                Err(e) => errors.push(e),
            }
        }
    }

    if active_cameras.len() > 1 {
        let names: Vec<&str> = active_cameras.iter().map(|(n, _)| n.as_str()).collect();
        // Point at the first surplus camera — the one that made it ambiguous.
        let (_, surplus_path) = &active_cameras[1];
        errors.push(
            cx.err(
                "multiple_active_cameras",
                format!(
                    "{} cameras are marked active ({}); exactly one may be, \
                     otherwise which one renders is arbitrary",
                    active_cameras.len(),
                    names.join(", ")
                ),
                surplus_path,
            )
            .component("Camera")
            .candidates(names),
        );
    }

    errors
}

/// What a valid component tells the caller.
struct Checked {
    active_camera: bool,
}

/// Shared validation context: the file name and the line lookup.
struct Cx<'a> {
    file: &'a str,
    index: LineIndex,
}

impl Cx<'_> {
    /// A structured error at `json_path`, with file and line attached.
    fn err(&self, code: &'static str, message: String, json_path: &str) -> EngineError {
        let error = EngineError::new(code, message).file(self.file);
        match self.index.line_of_or_parent(json_path) {
            Some(line) => error.line(line),
            None => error,
        }
    }

    fn missing_field(&self, field: &str, parent_path: &str) -> EngineError {
        self.err(
            "missing_field",
            format!("a scene requires a {field:?} field"),
            parent_path,
        )
        .field(field)
    }

    fn wrong_type(
        &self,
        field: &str,
        expected: &str,
        found: &Value,
        json_path: &str,
    ) -> EngineError {
        self.err(
            "invalid_field_type",
            format!(
                "{field:?} must be {} {expected}, found {}",
                article(expected),
                kind_of(found)
            ),
            json_path,
        )
        .field(field)
    }
}

fn check_component(
    cx: &Cx<'_>,
    component: &Value,
    entity: &str,
    component_path: &str,
) -> std::result::Result<Checked, EngineError> {
    let Some(object) = component.as_object() else {
        return Err(cx
            .err(
                "component_not_object",
                format!("components must be objects, found {}", kind_of(component)),
                component_path,
            )
            .entity(entity));
    };

    let type_name = match object.get("type") {
        Some(Value::String(name)) => name.as_str(),
        Some(other) => {
            return Err(cx
                .wrong_type("type", "string", other, &format!("{component_path}/type"))
                .entity(entity));
        }
        None => {
            return Err(cx
                .err(
                    "component_missing_type",
                    "every component needs a \"type\" field naming which component it is"
                        .to_string(),
                    component_path,
                )
                .entity(entity)
                .field("type"));
        }
    };

    if !ComponentData::NAMES.contains(&type_name) {
        return Err(cx
            .err(
                "unknown_component",
                format!("no component named {type_name:?}"),
                &format!("{component_path}/type"),
            )
            .entity(entity)
            .component(type_name)
            .suggest_from(type_name, ComponentData::NAMES.iter().copied()));
    }

    // The name is known, so serde can now check the fields against the real
    // type. This is the one place serde's error is better than anything a
    // hand-written walk would produce.
    let parsed = serde_json::from_value::<ComponentData>(component.clone())
        .map_err(|e| component_field_error(cx, e, type_name, entity, component_path))?;

    match parsed {
        // An unresolvable mesh asset is a validation error, never a silent
        // fallback (design doc §5). Resolution is against the scene file's own
        // directory, because that is what relative asset paths mean. This
        // checks the reference (existence, extension); whether the file
        // *parses* is checked by `engine validate` through engine-assets.
        ComponentData::Mesh(mesh) => {
            let base_dir = Path::new(cx.file).parent().unwrap_or(Path::new(""));
            if let Err(resolve) = MeshAsset::resolve(&mesh.asset, base_dir) {
                let mut error = cx
                    .err(
                        resolve.error,
                        resolve.message.clone(),
                        &format!("{component_path}/asset"),
                    )
                    .entity(entity)
                    .component("Mesh")
                    .field("asset");
                if let Some(suggestion) = resolve.context().and_then(|c| c.did_you_mean.clone()) {
                    error = error.did_you_mean(suggestion);
                }
                return Err(error);
            }
            Ok(Checked {
                active_camera: false,
            })
        }
        ComponentData::Camera(camera) => Ok(Checked {
            active_camera: camera.active,
        }),
        _ => Ok(Checked {
            active_camera: false,
        }),
    }
}

/// Turn a serde field error into a structured one, recovering the offending
/// field name and a suggestion where possible.
///
/// This reads serde's message text, which is not a stable interface. It
/// degrades to the raw message rather than misreporting when the format
/// changes, and the tests below pin the shapes that matter.
fn component_field_error(
    cx: &Cx<'_>,
    error: serde_json::Error,
    component: &str,
    entity: &str,
    component_path: &str,
) -> EngineError {
    let message = error.to_string();

    // "unknown field `postion`, expected one of `position`, `rotation`, ..."
    if let Some(rest) = message.strip_prefix("unknown field `") {
        if let Some((field, tail)) = rest.split_once('`') {
            let expected: Vec<&str> = tail
                .split('`')
                .skip(1)
                .step_by(2)
                .filter(|s| !s.is_empty())
                .collect();

            return cx
                .err(
                    "unknown_field",
                    format!("component {component:?} has no field {field:?}"),
                    &format!("{component_path}/{field}"),
                )
                .entity(entity)
                .component(component)
                .field(field)
                .suggest_from(field, expected);
        }
    }

    // "missing field `asset`"
    if let Some(rest) = message.strip_prefix("missing field `") {
        if let Some((field, _)) = rest.split_once('`') {
            return cx
                .err(
                    "missing_field",
                    format!("component {component:?} requires the field {field:?}"),
                    component_path,
                )
                .entity(entity)
                .component(component)
                .field(field);
        }
    }

    cx.err("invalid_component", message, component_path)
        .entity(entity)
        .component(component)
}

fn article(word: &str) -> &'static str {
    match word.chars().next() {
        Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
        _ => "a",
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(source: &str) -> Vec<&'static str> {
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

    #[test]
    fn reports_syntax_errors_with_a_line() {
        let errors = validate_source("{\n  \"name\": \"x\",\n  oops\n}", "test.json");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error, "invalid_json");
        assert_eq!(errors[0].context().unwrap().line, Some(3));
    }

    #[test]
    fn every_semantic_error_carries_file_and_line() {
        // Invariant 6 as restated in CLAUDE.md: an error an agent cannot
        // locate from the payload alone is a bug. One scene, many distinct
        // errors — all of them must know where they are.
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
        }
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
    fn rejects_duplicate_entity_names() {
        let source = r#"{"name":"s","entities":[{"name":"A"},{"name":"A"}]}"#;
        assert_eq!(codes(source), ["duplicate_entity_name"]);
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
        assert!(codes(source).is_empty());
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
        let codes = codes(source);
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
        assert_eq!(codes(source), ["missing_entity_name"]);
    }
}
