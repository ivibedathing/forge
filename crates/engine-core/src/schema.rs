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

/// Schema for a tileset file (M47).
///
/// A tileset is a file kind of its own rather than a component's contents, so
/// it has no `oneOf` variant to be lifted out of — but it drives the same
/// `walk_component` field check every component does, which is what gives it
/// unknown-field `did_you_mean`, closed-vocabulary part kinds and numeric
/// ranges without a line of bespoke checking.
pub fn tileset_schema() -> Value {
    to_value(schemars::schema_for!(crate::tileset::Tileset))
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

/// The component reference, as markdown, generated from the same schema
/// `engine list-components` publishes.
///
/// Invariant 7 says component schemas are derived from the Rust structs and
/// never maintained by hand; a prose reference maintained beside them would be
/// a second source of truth that drifts the first time someone adds a field.
/// So this renders the schema — every doc comment here was written on the
/// struct — and `repo_contracts.rs` fails when the committed file disagrees.
/// Regenerate with `engine list-components --markdown > docs/component-reference.md`.
pub fn component_reference() -> String {
    let schema = component_schema();
    let defs = schema["$defs"].clone();
    let variants = schema["oneOf"].as_array().cloned().unwrap_or_default();

    let mut out = String::new();
    out.push_str("# Component reference\n\n");
    out.push_str(
        "**Generated from the component schema — do not edit by hand.**\n\
         Regenerate with `engine list-components --markdown > docs/component-reference.md`;\n\
         `cargo test -p engine-core --test repo_contracts` fails when this file is stale.\n\n",
    );
    out.push_str(
        "Every component is a JSON object tagged with its `type`, and every field below\n\
         is optional — an absent field *is* its documented default. `engine list-components`\n\
         prints the same information as JSON Schema, and `engine inspect <scene>` prints a\n\
         scene's components with the defaults filled in.\n\n",
    );

    let mut names: Vec<(String, &Value)> = variants
        .iter()
        .filter_map(|v| {
            v["properties"]["type"]["const"]
                .as_str()
                .map(|n| (n.to_string(), v))
        })
        .collect();
    names.sort_by(|a, b| a.0.cmp(&b.0));

    out.push_str("| Component | Summary |\n|---|---|\n");
    for (name, variant) in &names {
        let summary = variant["description"]
            .as_str()
            .and_then(|d| d.lines().next())
            .unwrap_or("");
        out.push_str(&format!(
            "| [`{name}`](#{}) | {summary} |\n",
            name.to_lowercase()
        ));
    }
    out.push('\n');

    for (name, variant) in &names {
        out.push_str(&format!("## {name}\n\n"));
        if let Some(description) = variant["description"].as_str() {
            out.push_str(description.trim_end());
            out.push_str("\n\n");
        }
        let properties = variant["properties"].as_object();
        let fields: Vec<(&String, &Value)> = properties
            .map(|p| p.iter().filter(|(k, _)| k.as_str() != "type").collect())
            .unwrap_or_default();
        if fields.is_empty() {
            out.push_str("No fields; the `type` tag is the whole component.\n\n");
            continue;
        }
        out.push_str("| Field | Type | Default | Notes |\n|---|---|---|---|\n");
        for (field, spec) in fields {
            let spec = &resolve(spec, &defs);
            let ty = type_label(spec, &defs);
            let default = spec
                .get("default")
                .map(|d| format!("`{}`", compact_default(d)))
                .unwrap_or_else(|| "—".to_string());
            let mut notes = spec["description"]
                .as_str()
                .unwrap_or("")
                .replace('\n', " ")
                .replace('|', "\\|");
            if let Some(range) = range_label(spec) {
                if notes.is_empty() {
                    notes = range;
                } else {
                    notes = format!("{notes} ({range})");
                }
            }
            out.push_str(&format!("| `{field}` | {ty} | {default} | {notes} |\n"));
        }
        out.push('\n');
    }
    out
}

/// A default as an author would type it.
///
/// Every float in the schema is an `f32` widened to `f64` by serialization, so
/// `Camera.near`'s `0.1` arrives as `0.10000000149011612`. Printing that would
/// invite someone to copy it into a scene, where it means the same thing and
/// reads like a mistake.
fn compact_default(value: &Value) -> String {
    match value {
        Value::Number(n) => match n.as_f64() {
            Some(v) if v.fract() != 0.0 || v.abs() >= 1e16 => format!("{}", v as f32),
            Some(v) => format!("{v}"),
            None => n.to_string(),
        },
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(compact_default).collect();
            let joined = format!("[{}]", inner.join(", "));
            // A table cell is not the place for `Meadow.stages`' six fully
            // spelled-out keyframes. Past a screen's width the count is the
            // useful part; the prose beside it says what they are.
            if joined.len() > DEFAULT_CELL_LIMIT {
                let noun = if items.len() == 1 { "entry" } else { "entries" };
                format!("{} {noun}", items.len())
            } else {
                joined
            }
        }
        Value::Object(fields) => {
            let inner: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("\"{k}\": {}", compact_default(v)))
                .collect();
            let joined = format!("{{{}}}", inner.join(", "));
            if joined.len() > DEFAULT_CELL_LIMIT {
                "{…}".to_string()
            } else {
                joined
            }
        }
        other => other.to_string(),
    }
}

/// How wide a rendered default may get before it is summarized instead.
const DEFAULT_CELL_LIMIT: usize = 120;

/// Follow a `$ref` into `$defs`, so a field typed as one of the shared enums
/// (`Collider.shape`, `HudPanel.layout`, …) shows its actual vocabulary rather
/// than the word "object". The referenced definition carries the doc comment
/// too, which is why this replaces the spec rather than only its type.
fn resolve(spec: &Value, defs: &Value) -> Value {
    let Some(reference) = spec["$ref"].as_str() else {
        return spec.clone();
    };
    let Some(name) = reference.strip_prefix("#/$defs/") else {
        return spec.clone();
    };
    match defs.get(name) {
        Some(target) => target.clone(),
        None => spec.clone(),
    }
}

/// A field's type as the reference shows it: the JSON type, or `[T; n]` for
/// the fixed-length arrays this format uses for vectors and colours.
fn type_label(spec: &Value, defs: &Value) -> String {
    if let Some(items) = spec.get("items") {
        let items = resolve(items, defs);
        let inner = items["type"].as_str().unwrap_or("object");
        return match spec.get("minItems").and_then(Value::as_u64) {
            Some(n) if spec.get("maxItems").and_then(Value::as_u64) == Some(n) => {
                format!("`[{inner}; {n}]`")
            }
            _ => format!("`{inner}[]`"),
        };
    }
    if let Some(values) = spec.get("enum").and_then(Value::as_array) {
        let names: Vec<String> = values
            .iter()
            .filter_map(Value::as_str)
            .map(|v| format!("`\"{v}\"`"))
            .collect();
        if !names.is_empty() {
            return names.join(" \\| ");
        }
    }
    // A documented enum reaches the schema as oneOf/const rather than a flat
    // `enum` — the schemars gotcha `ColliderShapeKind` is kept undocumented to
    // avoid. Both forms mean the same closed vocabulary, so both render as one.
    if let Some(variants) = spec.get("oneOf").and_then(Value::as_array) {
        let names: Vec<String> = variants
            .iter()
            .filter_map(|v| v["const"].as_str())
            .map(|v| format!("`\"{v}\"`"))
            .collect();
        if !names.is_empty() {
            return names.join(" \\| ");
        }
    }
    // An `Option<T>` field is typed `["T", "null"]`; the null is what "absent"
    // already means everywhere in this format, so only `T` is worth printing.
    if let Some(types) = spec["type"].as_array() {
        let named: Vec<&str> = types
            .iter()
            .filter_map(Value::as_str)
            .filter(|t| *t != "null")
            .collect();
        if named.len() == 1 {
            return format!("`{}`", named[0]);
        }
        if !named.is_empty() {
            return format!("`{}`", named.join(" | "));
        }
    }
    match spec["type"].as_str() {
        Some(t) => format!("`{t}`"),
        None => "`object`".to_string(),
    }
}

/// The range constraint a field carries, if any — the `#[schemars(...)]`
/// attributes validation enforces, spelled out so the reference and the
/// error message agree.
fn range_label(spec: &Value) -> Option<String> {
    let bound = |key: &str, text: &str| {
        spec.get(key)
            .and_then(Value::as_f64)
            .map(|v| format!("{text} {v}"))
    };
    let parts: Vec<String> = [
        bound("minimum", "at least"),
        bound("exclusiveMinimum", "greater than"),
        bound("maximum", "at most"),
        bound("exclusiveMaximum", "less than"),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
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
