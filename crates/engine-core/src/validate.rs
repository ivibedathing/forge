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

use serde_json::{Map, Value};

use crate::codes;
use crate::components::ComponentData;
use crate::error::EngineError;
use crate::lineindex::LineIndex;
use crate::mesh::MeshAsset;

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
        if key != "name" && key != "entities" {
            errors.push(
                cx.err(
                    codes::UNKNOWN_FIELD,
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
    // (entity name, component path) per at-most-one component, so each
    // surplus error can point at a concrete line and list candidates.
    let mut active_cameras: Vec<(String, String)> = Vec::new();
    let mut directional_lights: Vec<(String, String)> = Vec::new();
    let mut ambient_lights: Vec<(String, String)> = Vec::new();

    for (entity_index, entity) in entities.iter().enumerate() {
        let entity_path = format!("/entities/{entity_index}");

        let Some(entity) = entity.as_object() else {
            errors.push(cx.err(
                codes::ENTITY_NOT_OBJECT,
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
                        codes::EMPTY_ENTITY_NAME,
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
                        codes::MISSING_ENTITY_NAME,
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
                    codes::DUPLICATE_ENTITY_NAME,
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
                        codes::UNKNOWN_FIELD,
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

        let mut seen_types: Vec<String> = Vec::new();
        let mut has_mesh = false;
        let mut material_paths: Vec<String> = Vec::new();

        for (component_index, component) in components.iter().enumerate() {
            let component_path = format!("{entity_path}/components/{component_index}");
            let checked = check_component(&cx, &schemas, component, name, &component_path, &mut errors);

            let Some(type_name) = checked.type_name else {
                continue;
            };

            if seen_types.iter().any(|t| *t == type_name) {
                // An error, not a warning: hecs keeps only the last, so the
                // file and the world would disagree — hidden state, which
                // invariant 2 exists to prevent. Points at the surplus copy.
                errors.push(
                    cx.err(
                        codes::DUPLICATE_COMPONENT,
                        format!(
                            "entity {name:?} has more than one {type_name} component; \
                             only one would survive, so the file and the world would disagree"
                        ),
                        &component_path,
                    )
                    .entity(name)
                    .component(&type_name),
                );
                continue;
            }
            seen_types.push(type_name.clone());

            if checked.active_camera {
                active_cameras.push((name.to_string(), component_path.clone()));
            }
            if checked.directional_light {
                directional_lights.push((name.to_string(), component_path.clone()));
            }
            if checked.ambient_light {
                ambient_lights.push((name.to_string(), component_path.clone()));
            }
            if type_name == "Mesh" {
                has_mesh = true;
            }
            if type_name == "Material" {
                material_paths.push(component_path);
            }
        }

        // Legal but almost certainly wrong: dead data from editing the wrong
        // entity. A warning, because rendering it is well-defined.
        if !has_mesh {
            for material_path in material_paths {
                errors.push(
                    cx.err(
                        codes::UNUSED_MATERIAL,
                        format!(
                            "entity {name:?} has a Material but no Mesh; \
                             the material affects nothing"
                        ),
                        &material_path,
                    )
                    .entity(name)
                    .component("Material")
                    .warning(),
                );
            }
        }
    }

    if active_cameras.len() > 1 {
        let names: Vec<&str> = active_cameras.iter().map(|(n, _)| n.as_str()).collect();
        // Point at the first surplus camera — the one that made it ambiguous.
        let (_, surplus_path) = &active_cameras[1];
        errors.push(
            cx.err(
                codes::MULTIPLE_ACTIVE_CAMERAS,
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

    // Lights follow the camera precedent: a deterministic failure over a
    // silent pick of whichever entity the world iterates first.
    for (list, component, code) in [
        (
            &directional_lights,
            "DirectionalLight",
            codes::MULTIPLE_DIRECTIONAL_LIGHTS,
        ),
        (&ambient_lights, "AmbientLight", codes::MULTIPLE_AMBIENT_LIGHTS),
    ] {
        if list.len() > 1 {
            let names: Vec<&str> = list.iter().map(|(n, _)| n.as_str()).collect();
            let (_, surplus_path) = &list[1];
            errors.push(
                cx.err(
                    code,
                    format!(
                        "{} {component} components in one scene ({}); at most one is \
                         allowed, otherwise which applies is arbitrary",
                        list.len(),
                        names.join(", ")
                    ),
                    surplus_path,
                )
                .component(component)
                .candidates(names),
            );
        }
    }

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
        let error = EngineError::new(code, message).file(self.file).path(json_path);
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

fn check_component(
    cx: &Cx<'_>,
    schemas: &ComponentSchemas,
    component: &Value,
    entity: &str,
    component_path: &str,
    errors: &mut Vec<EngineError>,
) -> Checked {
    let Some(object) = component.as_object() else {
        errors.push(
            cx.err(
                codes::COMPONENT_NOT_OBJECT,
                format!("components must be objects, found {}", kind_of(component)),
                component_path,
            )
            .entity(entity),
        );
        return Checked::default();
    };

    let type_name = match object.get("type") {
        Some(Value::String(name)) => name.as_str(),
        Some(other) => {
            errors.push(
                cx.wrong_type("type", "string", other, &format!("{component_path}/type"))
                    .entity(entity),
            );
            return Checked::default();
        }
        None => {
            errors.push(
                cx.err(
                    codes::COMPONENT_MISSING_TYPE,
                    "every component needs a \"type\" field naming which component it is"
                        .to_string(),
                    component_path,
                )
                .entity(entity)
                .field("type"),
            );
            return Checked::default();
        }
    };

    if !ComponentData::NAMES.contains(&type_name) {
        errors.push(
            cx.err(
                codes::UNKNOWN_COMPONENT,
                format!("no component named {type_name:?}"),
                &format!("{component_path}/type"),
            )
            .entity(entity)
            .component(type_name)
            .suggest_from(type_name, ComponentData::NAMES.iter().copied()),
        );
        return Checked::default();
    }

    // The name is known; the schema variant drives the field checks from here.
    // `variant` cannot miss for a known name (schema.rs pins the shape), but a
    // validator must degrade rather than panic, so the impossible branch just
    // skips field checking.
    let Some(variant) = schemas.variant(type_name) else {
        return Checked::named(type_name);
    };

    let shape_clean = walk_component(cx, variant, object, type_name, entity, component_path, errors);
    if !shape_clean {
        // Field names or JSON types are wrong; serde would reject this
        // component, so parsing and the semantic checks below are moot.
        return Checked::named(type_name);
    }

    // The walk passed, so serde must accept — it is the final gate proving the
    // walk and the parser agree. A rejection here is an engine bug, and the
    // error code says so rather than blaming the scene.
    let parsed = match serde_json::from_value::<ComponentData>(component.clone()) {
        Ok(parsed) => parsed,
        Err(e) => {
            errors.push(
                cx.err(
                    codes::SCENE_PARSE_DESYNC,
                    format!(
                        "component {type_name:?} passed the schema walk but failed to \
                         parse ({e}); this is an engine bug, not a scene problem"
                    ),
                    component_path,
                )
                .entity(entity)
                .component(type_name),
            );
            return Checked::named(type_name);
        }
    };

    let mut checked = Checked::named(type_name);

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
                errors.push(error);
            }
        }

        ComponentData::Camera(camera) => {
            // JSON Schema cannot express `far > near`, so this one check is
            // hand-written. Written as `!(far > near)` so NaN also fails.
            if !(camera.far > camera.near) {
                errors.push(
                    cx.err(
                        codes::VALUE_OUT_OF_RANGE,
                        format!(
                            "Camera.far is {}; it must be greater than near ({})",
                            camera.far, camera.near
                        ),
                        &format!("{component_path}/far"),
                    )
                    .entity(entity)
                    .component("Camera")
                    .field("far"),
                );
            }
            checked.active_camera = camera.active;
        }

        ComponentData::Transform(transform) => {
            // Legal but almost certainly wrong: renders invisibly or as
            // degenerate geometry with no error — the classic "I edited the
            // file and nothing changed".
            let scale = transform.scale.to_array();
            if scale.contains(&0.0) {
                errors.push(
                    cx.err(
                        codes::ZERO_SCALE,
                        format!(
                            "Transform.scale is [{}, {}, {}]; a zero axis renders the \
                             entity invisibly or as degenerate geometry",
                            scale[0], scale[1], scale[2]
                        ),
                        &format!("{component_path}/scale"),
                    )
                    .entity(entity)
                    .component("Transform")
                    .field("scale")
                    .warning(),
                );
            }
        }

        ComponentData::DirectionalLight(_) => checked.directional_light = true,
        ComponentData::AmbientLight(_) => checked.ambient_light = true,
        ComponentData::Material(_) => {}
    }

    checked
}

/// Check one component object against its schema variant: unknown keys,
/// missing required fields, JSON types, and numeric ranges. Returns whether
/// the component's *shape* is clean — range violations report errors but do
/// not make the shape unparseable, so they leave the return value true.
fn walk_component(
    cx: &Cx<'_>,
    variant: &Value,
    object: &Map<String, Value>,
    type_name: &str,
    entity: &str,
    component_path: &str,
    errors: &mut Vec<EngineError>,
) -> bool {
    let mut shape_clean = true;
    let empty = Map::new();
    let properties = variant["properties"].as_object().unwrap_or(&empty);

    for key in object.keys() {
        if key == "type" || properties.contains_key(key.as_str()) {
            continue;
        }
        shape_clean = false;
        errors.push(
            cx.err(
                codes::UNKNOWN_FIELD,
                format!("component {type_name:?} has no field {key:?}"),
                &format!("{component_path}/{key}"),
            )
            .entity(entity)
            .component(type_name)
            .field(key)
            .suggest_from(
                key,
                properties.keys().map(String::as_str).filter(|k| *k != "type"),
            ),
        );
    }

    if let Some(required) = variant["required"].as_array() {
        for field in required.iter().filter_map(Value::as_str) {
            if field != "type" && !object.contains_key(field) {
                shape_clean = false;
                errors.push(
                    cx.err(
                        codes::MISSING_FIELD,
                        format!("component {type_name:?} requires the field {field:?}"),
                        component_path,
                    )
                    .entity(entity)
                    .component(type_name)
                    .field(field),
                );
            }
        }
    }

    for (key, value) in object {
        if key == "type" {
            continue;
        }
        let Some(property) = properties.get(key.as_str()) else {
            continue; // already reported as unknown
        };
        let field_path = format!("{component_path}/{key}");
        shape_clean &=
            check_value(cx, property, value, type_name, entity, key, &field_path, errors);
    }

    shape_clean
}

/// Check one field value against its property schema. Returns whether the
/// value's shape (JSON type, array arity) is clean.
#[allow(clippy::too_many_arguments)]
fn check_value(
    cx: &Cx<'_>,
    schema: &Value,
    value: &Value,
    component: &str,
    entity: &str,
    field: &str,
    json_path: &str,
    errors: &mut Vec<EngineError>,
) -> bool {
    match schema["type"].as_str() {
        Some("number") => {
            let Some(number) = value.as_number() else {
                errors.push(
                    cx.wrong_type(field, "number", value, json_path)
                        .entity(entity)
                        .component(component),
                );
                return false;
            };
            check_bounds(cx, schema, number, component, entity, field, field, json_path, errors);
            true
        }

        Some("boolean") => {
            if !value.is_boolean() {
                errors.push(
                    cx.wrong_type(field, "boolean", value, json_path)
                        .entity(entity)
                        .component(component),
                );
                return false;
            }
            true
        }

        Some("string") => {
            if !value.is_string() {
                errors.push(
                    cx.wrong_type(field, "string", value, json_path)
                        .entity(entity)
                        .component(component),
                );
                return false;
            }
            true
        }

        Some("array") => {
            let Some(items) = value.as_array() else {
                errors.push(
                    cx.wrong_type(field, "array", value, json_path)
                        .entity(entity)
                        .component(component),
                );
                return false;
            };

            let len = items.len() as u64;
            let min_items = schema["minItems"].as_u64();
            let max_items = schema["maxItems"].as_u64();
            if min_items.is_some_and(|n| len < n) || max_items.is_some_and(|n| len > n) {
                let expected = match (min_items, max_items) {
                    (Some(a), Some(b)) if a == b => format!("exactly {a}"),
                    (Some(a), Some(b)) => format!("between {a} and {b}"),
                    (Some(a), None) => format!("at least {a}"),
                    (None, Some(b)) => format!("at most {b}"),
                    (None, None) => unreachable!("guarded above"),
                };
                errors.push(
                    cx.err(
                        codes::INVALID_FIELD_TYPE,
                        format!("{field:?} must be an array of {expected} elements, found {len}"),
                        json_path,
                    )
                    .entity(entity)
                    .component(component)
                    .field(field),
                );
                return false;
            }

            let item_schema = &schema["items"];
            let mut clean = true;
            for (i, item) in items.iter().enumerate() {
                let item_path = format!("{json_path}/{i}");
                if item_schema["type"].as_str() == Some("number") {
                    let Some(number) = item.as_number() else {
                        clean = false;
                        errors.push(
                            cx.err(
                                codes::INVALID_FIELD_TYPE,
                                format!("{field}[{i}] must be a number, found {}", kind_of(item)),
                                &item_path,
                            )
                            .entity(entity)
                            .component(component)
                            .field(field),
                        );
                        continue;
                    };
                    let label = format!("{field}[{i}]");
                    check_bounds(
                        cx, item_schema, number, component, entity, field, &label, &item_path,
                        errors,
                    );
                }
            }
            clean
        }

        // A property kind the walk does not know how to check — leave it to
        // the serde gate rather than guessing.
        _ => true,
    }
}

/// Emit `value_out_of_range` when `number` violates the bounds its property
/// schema declares (`minimum`/`maximum`/`exclusiveMinimum`/`exclusiveMaximum`).
/// One error per offending value — an agent fixing `albedo: [1.5, 2.0, 0.5]`
/// should learn about both bad channels in one run.
#[allow(clippy::too_many_arguments)]
fn check_bounds(
    cx: &Cx<'_>,
    schema: &Value,
    number: &serde_json::Number,
    component: &str,
    entity: &str,
    field: &str,
    label: &str,
    json_path: &str,
    errors: &mut Vec<EngineError>,
) {
    let Some(v) = number.as_f64() else { return };
    let min = schema["minimum"].as_f64();
    let max = schema["maximum"].as_f64();
    let emin = schema["exclusiveMinimum"].as_f64();
    let emax = schema["exclusiveMaximum"].as_f64();

    let violated = min.is_some_and(|b| v < b)
        || max.is_some_and(|b| v > b)
        || emin.is_some_and(|b| v <= b)
        || emax.is_some_and(|b| v >= b);
    if !violated {
        return;
    }

    let requirement = match (min, max, emin, emax) {
        (Some(lo), Some(hi), None, None) => {
            format!("the allowed range is [{}, {}]", fmt_num(lo), fmt_num(hi))
        }
        (None, None, Some(lo), Some(hi)) => format!(
            "it must be greater than {} and less than {}",
            fmt_num(lo),
            fmt_num(hi)
        ),
        _ => {
            let mut clauses = Vec::new();
            if let Some(lo) = min {
                clauses.push(format!("it must be at least {}", fmt_num(lo)));
            }
            if let Some(lo) = emin {
                clauses.push(format!("it must be greater than {}", fmt_num(lo)));
            }
            if let Some(hi) = max {
                clauses.push(format!("it must be at most {}", fmt_num(hi)));
            }
            if let Some(hi) = emax {
                clauses.push(format!("it must be less than {}", fmt_num(hi)));
            }
            clauses.join(" and ")
        }
    };

    errors.push(
        cx.err(
            codes::VALUE_OUT_OF_RANGE,
            format!("{component}.{label} is {}; {requirement}", fmt_num(v)),
            json_path,
        )
        .entity(entity)
        .component(component)
        .field(field),
    );
}

/// Format a bound or value the way `{}` formats an `f32`: integral values
/// without a trailing `.0`, so messages read "at least 0", not "at least 0.0".
fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.is_finite() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
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
        assert!(errors[0].message.contains("exactly 3"), "{}", errors[0].message);
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
        assert!(albedo_messages.iter().any(|m| m.contains("albedo[0] is 1.5")));
        assert!(
            albedo_messages
                .iter()
                .any(|m| m.contains("the allowed range is [0, 1]")),
            "{albedo_messages:?}"
        );

        assert!(out_of_range
            .iter()
            .any(|e| e.context().unwrap().field.as_deref() == Some("intensity")
                && e.message.contains("at least 0")));
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

        assert!(ranges
            .iter()
            .any(|e| e.message.contains("fov is 0")
                && e.message.contains("greater than 0 and less than 180")));
        assert!(ranges
            .iter()
            .any(|e| e.message.contains("near is -1") && e.message.contains("greater than 0")));
        assert!(ranges
            .iter()
            .any(|e| e.message.contains("far is 0.05")
                && e.message.contains("greater than near (0.1)")));
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
}
