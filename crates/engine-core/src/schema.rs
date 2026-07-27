//! JSON Schema export.
//!
//! Schemas are derived from the Rust types via `schemars` and never written by
//! hand (invariant 7). `engine list-components` prints this, and
//! `schemas/component-schema.json` is a checked-in copy that a test keeps in
//! sync — so a component added without regenerating the schema fails the build
//! rather than shipping a stale contract.

use serde_json::Value;

use crate::components::ComponentData;
use crate::scene::SceneFile;

/// Schema for a single component — the `oneOf` over every known component,
/// discriminated by `"type"`.
pub fn component_schema() -> Value {
    to_value(schemars::schema_for!(ComponentData))
}

/// Schema for a whole scene file.
pub fn scene_schema() -> Value {
    to_value(schemars::schema_for!(SceneFile))
}

/// Both schemas, as `engine list-components` emits them.
pub fn full_schema() -> Value {
    serde_json::json!({
        "scene": scene_schema(),
        "component": component_schema(),
        "components": ComponentData::NAMES,
    })
}

/// Schema for a property-clip file (M9).
pub fn animation_schema() -> Value {
    to_value(schemars::schema_for!(crate::animation::ClipFile))
}

/// The canonical on-disk form of the animation schema
/// (`schemas/animation-schema.json`), kept in sync by `repo_contracts.rs`.
pub fn canonical_animation_json() -> String {
    let mut s = serde_json::to_string_pretty(&animation_schema())
        .expect("schemas are plain data and cannot fail to serialize");
    s.push('\n');
    s
}

/// Render the canonical on-disk form: pretty-printed, newline-terminated.
///
/// Used by both the CLI and the drift test, so the committed file and the
/// generated one cannot differ by formatting alone.
pub fn canonical_json() -> String {
    let mut s = serde_json::to_string_pretty(&full_schema())
        .expect("schemas are plain data and cannot fail to serialize");
    s.push('\n');
    s
}

fn to_value(schema: schemars::Schema) -> Value {
    schema.to_value()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_every_component_as_a_variant() {
        let schema = component_schema();
        let variants = schema["oneOf"]
            .as_array()
            .expect("component schema should be a oneOf over the components");
        assert_eq!(variants.len(), ComponentData::NAMES.len());
    }

    #[test]
    fn discriminates_variants_on_type() {
        let schema = component_schema();
        let names: Vec<&str> = schema["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v["properties"]["type"]["const"].as_str())
            .collect();
        assert_eq!(names, ComponentData::NAMES);
    }

    /// The `oneOf` entry for one component. Variants are inlined rather than
    /// `$ref`d because `deny_unknown_fields` sets `additionalProperties: false`.
    fn variant(name: &str) -> Value {
        component_schema()["oneOf"]
            .as_array()
            .expect("component schema should be a oneOf")
            .iter()
            .find(|v| v["properties"]["type"]["const"] == name)
            .unwrap_or_else(|| panic!("no schema variant for {name}"))
            .clone()
    }

    #[test]
    fn describes_glam_vectors_as_bounded_arrays() {
        // glam types have no JsonSchema impl of their own; the components
        // annotate them with `#[schemars(with = "[f32; 3]")]`. If that
        // annotation were dropped the schema would silently lose the shape.
        let position = &variant("Transform")["properties"]["position"];
        assert_eq!(position["type"], "array");
        assert_eq!(position["minItems"], 3);
        assert_eq!(position["maxItems"], 3);

        // Rotation is Euler degrees [x, y, z] (design doc §5) — three
        // elements, not a four-element quaternion.
        let rotation = &variant("Transform")["properties"]["rotation"];
        assert_eq!(rotation["minItems"], 3);
        assert_eq!(rotation["maxItems"], 3);
    }

    #[test]
    fn closes_components_to_unknown_fields() {
        // The schema has to agree with `deny_unknown_fields`, or an agent
        // validating against the published schema would accept a typo'd field
        // that the loader then rejects.
        assert_eq!(variant("Transform")["additionalProperties"], false);
    }

    #[test]
    fn publishes_defaults_so_omitted_fields_are_discoverable() {
        // An agent reading the schema should be able to tell that omitting
        // `scale` yields 1,1,1 rather than 0,0,0.
        assert_eq!(
            variant("Transform")["properties"]["scale"]["default"],
            serde_json::json!([1.0, 1.0, 1.0])
        );
    }

    #[test]
    fn carries_doc_comments_into_the_schema() {
        // Doc comments are the source for `docs/component-reference.md`
        // (design doc §4), so they have to survive schema generation.
        let mesh = variant("Mesh");
        let description = mesh["description"]
            .as_str()
            .expect("Mesh should carry its doc comment");
        assert!(
            description.contains("relative path"),
            "unexpected description: {description}"
        );
    }

    #[test]
    fn marks_required_fields_as_required() {
        let required = &variant("Mesh")["required"];
        let required: Vec<&str> = required
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(required.contains(&"asset"), "Mesh.asset has no default");
    }

    #[test]
    fn canonical_form_is_stable() {
        assert_eq!(canonical_json(), canonical_json());
        assert!(canonical_json().ends_with("}\n"));
    }
}
