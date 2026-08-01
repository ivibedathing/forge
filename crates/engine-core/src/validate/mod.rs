//! Scene validation.
//!
//! Deliberately does *not* just call `serde_json::from_str::<SceneFile>` and
//! forward the error. serde stops at the first problem and describes it in
//! prose; an agent wants every problem at once, each as a structured record it
//! can act on without parsing English.
//!
//! So this walks the JSON tree itself, collecting errors. Per-component field
//! checking is driven by the schemars-generated component schema — the same
//! schema `engine list-components` publishes — so validation and publication
//! cannot disagree (invariant 7, upgraded: the schema is derived *and*
//! enforced). serde then parses each already-clean component as a final gate;
//! if the walk passes and serde still rejects, that is a `scene_parse_desync`
//! bug, not a scene problem. A [`LineIndex`] built from the raw source
//! supplies the line numbers that `serde_json::Value` discards, and every
//! error also carries its JSON Pointer in `path` (invariant 6: every error
//! names its file and line — and now where `jq` can reach it).
//!
//! The returned vector interleaves errors and warnings in file order; filter
//! with [`EngineError::is_warning`] to judge validity. Warnings mark scenes
//! that are legal but almost certainly wrong (a `Material` with no `Mesh`, a
//! zero scale axis) — the cases where the screenshot just looks subtly off
//! and the agent burns iterations discovering why.

use std::path::Path;

use serde_json::Value;

use crate::codes;
use crate::components::ComponentData;
use crate::error::EngineError;
use crate::lineindex::LineIndex;

mod blocks;
mod component;
mod entity;
mod passes;
mod walk;

use blocks::{check_daylight_block, check_environment_block, check_physics_block};
use component::check_component;
use walk::walk_component;

/// Validate a scene file's contents. An empty result means the scene is valid
/// with nothing to warn about; a result with only warnings is still valid.
pub fn validate_source(source: &str, path: &str) -> Vec<EngineError> {
    let root: Value = match serde_json::from_str(source) {
        Ok(value) => value,
        Err(e) => {
            // Syntax errors never reach the LineIndex; serde_json itself knows
            // the position here.
            return vec![EngineError::new(codes::INVALID_JSON, e.to_string())
                .file(path)
                .line(e.line() as u32)
                .column(e.column() as u32)];
        }
    };

    let cx = Cx {
        file: path,
        index: LineIndex::new(source),
    };
    let schemas = ComponentSchemas::new();

    let mut errors = Vec::new();

    let Some(object) = root.as_object() else {
        errors.push(cx.err(
            codes::SCENE_ROOT_NOT_OBJECT,
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
        if key != "name"
            && key != "entities"
            && key != "physics"
            && key != "environment"
            && key != "daylight"
        {
            errors.push(
                cx.err(
                    codes::UNKNOWN_FIELD,
                    format!("unknown top-level field {key:?}"),
                    &format!("/{key}"),
                )
                .field(key)
                .suggest_from(
                    key,
                    ["name", "entities", "physics", "environment", "daylight"],
                ),
            );
        }
    }

    if let Some(physics) = object.get("physics") {
        check_physics_block(&cx, physics, &mut errors);
    }

    if let Some(environment) = object.get("environment") {
        check_environment_block(&cx, environment, &mut errors);
    }

    if let Some(daylight) = object.get("daylight") {
        check_daylight_block(&cx, daylight, object.get("environment"), &mut errors);
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

    let facts = entity::walk(&cx, &schemas, entities, &mut errors);

    passes::camera(&cx, &facts, &mut errors);
    passes::lights(&cx, &facts, &mut errors);
    passes::daylight(&cx, object, &facts, &mut errors);
    passes::point_light_budget(&cx, &facts, &mut errors);
    passes::collision_layers(&cx, &facts, &mut errors);
    passes::wheel(&cx, &facts, &mut errors);
    passes::meadow(&cx, &facts, &mut errors);
    passes::road_ground(&cx, &facts, &mut errors);
    passes::junction(&cx, &facts, &mut errors);
    passes::foot_planting(&cx, &facts, &mut errors);
    passes::hud_parent(&cx, &facts, &mut errors);
    passes::animation(&cx, &facts, &mut errors);

    errors
}

/// What a checked component tells the caller.
#[derive(Default)]
struct Checked {
    /// The component's known type name — set even when its fields have
    /// errors, so duplicate detection still sees it.
    type_name: Option<String>,
    active_camera: bool,
    directional_light: bool,
    ambient_light: bool,
    point_light: bool,
    /// The parsed component when the shape was clean — what the entity-level
    /// cross-component checks (physics, M8) read.
    parsed: Option<ComponentData>,
}

impl Checked {
    fn named(type_name: &str) -> Self {
        Self {
            type_name: Some(type_name.to_string()),
            ..Self::default()
        }
    }
}

/// The schemars-generated component schema, the driver for per-component
/// field checking. Built once per validated file.
struct ComponentSchemas {
    schema: Value,
}

impl ComponentSchemas {
    fn new() -> Self {
        Self {
            schema: crate::schema::component_schema(),
        }
    }

    /// Look through a `$ref` to the schema's `$defs` (schemars refs shared
    /// types like enums). Non-ref schemas come back unchanged.
    fn resolve<'a>(&'a self, property: &'a Value) -> &'a Value {
        match property["$ref"]
            .as_str()
            .and_then(|r| r.strip_prefix("#/$defs/"))
        {
            Some(name) => &self.schema["$defs"][name],
            None => property,
        }
    }

    /// The `oneOf` variant for a component name. The discrimination shape is
    /// pinned by tests in `schema.rs`, so a known name always resolves.
    fn variant(&self, name: &str) -> Option<&Value> {
        self.schema["oneOf"]
            .as_array()?
            .iter()
            .find(|v| v["properties"]["type"]["const"] == name)
    }
}

/// Shared validation context: the file name and the line lookup.
struct Cx<'a> {
    file: &'a str,
    index: LineIndex,
}

impl Cx<'_> {
    /// A structured error at `json_path`, with file, line, and JSON Pointer
    /// attached. The pointer is what the validator walked to get here; keeping
    /// it costs nothing and makes the fix `jq`-addressable.
    fn err(&self, code: &'static str, message: String, json_path: &str) -> EngineError {
        let error = EngineError::new(code, message)
            .file(self.file)
            .path(json_path);
        match self.index.line_of_or_parent(json_path) {
            Some(line) => error.line(line),
            None => error,
        }
    }

    fn missing_field(&self, field: &str, parent_path: &str) -> EngineError {
        self.err(
            codes::MISSING_FIELD,
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
            codes::INVALID_FIELD_TYPE,
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

/// Validate a standalone `materials/*.json` file (M26).
///
/// A material file is the `Material` component's fields minus the `"type"`, so
/// this is the same schema walk the component gets — unknown fields, JSON
/// types, ranges — against the same published variant, with the material file's
/// own line numbers. Nothing about it is checked twice: the scene's Material
/// component checks the *reference*, this checks the *contents*.
///
/// `engine validate materials/asphalt.json` runs exactly this, the way
/// `engine validate` accepts a clip file directly.
pub fn validate_material_source(source: &str, path: &str) -> Vec<EngineError> {
    let root: Value = match serde_json::from_str(source) {
        Ok(value) => value,
        Err(e) => {
            return vec![EngineError::new(codes::INVALID_JSON, e.to_string())
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
            codes::COMPONENT_NOT_OBJECT,
            format!(
                "a material file must be a JSON object, found {}",
                kind_of(&root)
            ),
            "",
        ));
        return errors;
    };

    let schemas = ComponentSchemas::new();
    let Some(variant) = schemas.variant("Material") else {
        return errors;
    };

    // A material file has no `"type"` — it is not a component, it is what a
    // component points at — and it may not name another material file either.
    for reserved in ["type", "asset"] {
        if object.contains_key(reserved) {
            errors.push(
                cx.err(
                    codes::UNKNOWN_FIELD,
                    format!(
                        "a material file has no {reserved:?} field; it holds the \
                         Material component's fields and nothing else"
                    ),
                    &format!("/{reserved}"),
                )
                .component("Material")
                .field(reserved),
            );
        }
    }

    let mut filtered = object.clone();
    filtered.remove("asset");
    let clean = walk_component(
        &cx,
        &schemas,
        variant,
        &filtered,
        "Material",
        "",
        "",
        &mut errors,
    );

    // Its texture references, resolved against **the material file's own
    // directory** — that is what lets one material be named by scenes in
    // different places and still find its maps.
    if clean {
        if let Ok(material) =
            serde_json::from_value::<crate::components::Material>(Value::Object(filtered))
        {
            let base_dir = Path::new(path).parent().unwrap_or(Path::new(""));
            for (field, asset, _) in material.maps() {
                if let Err(resolve) = crate::texture::resolve_texture(asset, base_dir) {
                    let mut error = cx
                        .err(resolve.error, resolve.message.clone(), &format!("/{field}"))
                        .component("Material")
                        .field(field);
                    if let Some(suggestion) = resolve.context().and_then(|c| c.did_you_mean.clone())
                    {
                        error = error.did_you_mean(suggestion);
                    }
                    errors.push(error);
                }
            }
        }
    }

    errors
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
mod tests;
