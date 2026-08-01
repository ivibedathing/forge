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

/// One component's schema, lifted out of the `oneOf` (M24).
///
/// `engine list-components` publishes the whole vocabulary as a `oneOf`
/// discriminated by `properties.type.const`, which is the right shape for a
/// validator and a two-attempt `jq` selector for a reader who wants one
/// component. This does that selection once, in the engine, so the answer to
/// "what fields does a `Water` have" is a command rather than a query someone
/// has to get right.
///
/// The variant is returned as a **standalone document**: the `$defs` it
/// references (transitively — `Road` reaches `RoadMarkings`, which reaches
/// nothing, while `Terrain` reaches `TerrainLayer`) are carried along, so every
/// `#/$defs/...` pointer inside it still resolves. Dropping them would print a
/// schema that reads fine and cannot be resolved by any validator, which is a
/// worse failure than the `jq` it replaces.
///
/// `None` when no component has that name; the caller turns that into
/// `unknown_component_query` with a `did_you_mean`, since it knows the file and
/// the invocation.
pub fn component_schema_named(name: &str) -> Option<Value> {
    let schema = component_schema();
    let variant = schema
        .get("oneOf")?
        .as_array()?
        .iter()
        .find(|variant| variant.pointer("/properties/type/const") == Some(&Value::from(name)))?
        .clone();

    let mut document = variant;
    let mut needed = std::collections::BTreeMap::new();
    collect_defs(&document, schema.get("$defs"), &mut needed);

    let object = document.as_object_mut()?;
    if !needed.is_empty() {
        object.insert(
            "$defs".to_string(),
            Value::Object(needed.into_iter().collect()),
        );
    }
    if let Some(dialect) = schema.get("$schema") {
        object.insert("$schema".to_string(), dialect.clone());
    }
    object.insert("title".to_string(), Value::from(name));
    Some(document)
}

/// Walk a schema fragment for `#/$defs/<name>` references, pulling each
/// definition (and everything *it* references) out of `defs`.
fn collect_defs(
    fragment: &Value,
    defs: Option<&Value>,
    out: &mut std::collections::BTreeMap<String, Value>,
) {
    match fragment {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get("$ref") {
                if let Some(name) = reference.strip_prefix("#/$defs/") {
                    if !out.contains_key(name) {
                        if let Some(definition) = defs.and_then(|d| d.get(name)) {
                            // Insert before recursing: a self-referential
                            // definition would otherwise loop forever.
                            out.insert(name.to_string(), definition.clone());
                            collect_defs(definition, defs, out);
                        }
                    }
                }
            }
            for value in map.values() {
                collect_defs(value, defs, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_defs(item, defs, out);
            }
        }
        _ => {}
    }
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
    fn names_a_single_component_out_of_the_one_of() {
        for name in ComponentData::NAMES {
            let one = component_schema_named(name)
                .unwrap_or_else(|| panic!("no single-component schema for {name}"));
            assert_eq!(one["properties"]["type"]["const"], *name);
            // Field-for-field the same object the `oneOf` carries: this is a
            // selection, not a second rendering of the vocabulary.
            assert_eq!(one["properties"], variant(name)["properties"]);
        }
        assert!(component_schema_named("Meterial").is_none());
    }

    #[test]
    fn a_single_component_schema_resolves_its_own_refs() {
        // A variant that reaches a `$defs` entry has to carry it, or the
        // printed document has pointers into a document that is not there.
        let road = component_schema_named("Road").expect("Road is a component");
        let text = road.to_string();
        for reference in text.split("\"$ref\":\"").skip(1) {
            let name = reference
                .split('"')
                .next()
                .and_then(|r| r.strip_prefix("#/$defs/").map(str::to_string))
                .expect("refs point into $defs");
            assert!(
                road["$defs"].get(&name).is_some(),
                "Road's schema references {name} without carrying it"
            );
        }
        assert!(road["$defs"]["RoadPoint"].is_object());
        assert!(road["$defs"]["RoadMarkings"].is_object());
        // …and only what it needs: Terrain's layer type is a different
        // component's business.
        assert!(road["$defs"].get("TerrainLayer").is_none());
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
        // Doc comments are what `engine list-components` publishes as each
        // field's description, and what the editor generates its inspector
        // labels from — so they have to survive schema generation. (Design
        // doc §4 also sketches a `docs/component-reference.md` built from
        // them; that file was never written.)
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
