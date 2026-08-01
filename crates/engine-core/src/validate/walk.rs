//! The schema-driven field walk: the half of validation that reads the same
//! schema `engine list-components` publishes, so the two cannot disagree.

use serde_json::{Map, Value};

use crate::codes;
use crate::error::EngineError;

use super::{kind_of, ComponentSchemas, Cx};

/// Check one component object against its schema variant: unknown keys,
/// missing required fields, JSON types, and numeric ranges. Returns whether
/// the component's *shape* is clean — range violations report errors but do
/// not make the shape unparseable, so they leave the return value true.
///
/// Eight parameters, and clippy would rather they were a struct. They are not:
/// six of them are the *location* being reported (entity, component, JSON
/// pointer) threaded down a recursive walk, and `Cx` is already the bundle for
/// everything that does not change between nodes. A second bundle whose fields
/// change at every level would only move the argument list somewhere less
/// visible.
#[allow(clippy::too_many_arguments)]
pub(super) fn walk_component(
    cx: &Cx<'_>,
    schemas: &ComponentSchemas,
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
                properties
                    .keys()
                    .map(String::as_str)
                    .filter(|k| *k != "type"),
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
        let property = schemas.resolve(property);
        let field_path = format!("{component_path}/{key}");
        shape_clean &= check_value(
            cx,
            schemas,
            property,
            value,
            type_name,
            entity,
            key,
            &field_path,
            errors,
        );
    }

    shape_clean
}

/// The JSON type a property schema names, looking through nullability:
/// `Option<T>` fields publish `"type": ["<T>", "null"]`. Returns the
/// non-null type and whether null is allowed.
pub(super) fn schema_type(schema: &Value) -> (Option<&str>, bool) {
    if let Some(t) = schema["type"].as_str() {
        return (Some(t), false);
    }
    if let Some(types) = schema["type"].as_array() {
        let nullable = types.iter().any(|t| t == "null");
        let concrete = types
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null");
        return (concrete, nullable);
    }
    (None, false)
}

/// The closed set of strings a property schema accepts, if it is an enum.
/// schemars writes plain enums as `"enum": [...]` and doc-commented ones as
/// a `oneOf` of `const` entries; both are the same contract.
pub(super) fn enum_values(schema: &Value) -> Option<Vec<&str>> {
    if let Some(values) = schema["enum"].as_array() {
        return Some(values.iter().filter_map(Value::as_str).collect());
    }
    if let Some(variants) = schema["oneOf"].as_array() {
        let consts: Vec<&str> = variants
            .iter()
            .filter_map(|v| v["const"].as_str())
            .collect();
        if !consts.is_empty() && consts.len() == variants.len() {
            return Some(consts);
        }
    }
    None
}

/// Check one field value against its property schema. Returns whether the
/// value's shape (JSON type, array arity) is clean.
#[allow(clippy::too_many_arguments)]
pub(super) fn check_value(
    cx: &Cx<'_>,
    schemas: &ComponentSchemas,
    schema: &Value,
    value: &Value,
    component: &str,
    entity: &str,
    field: &str,
    json_path: &str,
    errors: &mut Vec<EngineError>,
) -> bool {
    let (type_name, nullable) = schema_type(schema);
    if nullable && value.is_null() {
        return true;
    }
    // An enum-of-strings schema may carry no top-level "type"; it is a
    // string field with a closed vocabulary either way.
    let type_name = type_name.or_else(|| enum_values(schema).map(|_| "string"));
    match type_name {
        Some("number") => {
            let Some(number) = value.as_number() else {
                errors.push(
                    cx.wrong_type(field, "number", value, json_path)
                        .entity(entity)
                        .component(component),
                );
                return false;
            };
            check_bounds(
                cx, schema, number, component, entity, field, field, json_path, errors,
            );
            true
        }

        Some("integer") => {
            // Integer fields (u32 in the component structs) are stricter than
            // "number": serde rejects fractions and out-of-format values, so
            // the walk must too — reporting them as shape errors, or the
            // final serde gate would fire `scene_parse_desync` on them.
            let integral = value.as_number().is_some_and(|n| n.is_u64() || n.is_i64());
            if !integral {
                errors.push(
                    cx.wrong_type(field, "integer", value, json_path)
                        .entity(entity)
                        .component(component),
                );
                return false;
            }
            if schema["format"].as_str() == Some("uint32")
                && value.as_u64().is_none_or(|n| n > u64::from(u32::MAX))
            {
                errors.push(
                    cx.err(
                        codes::INVALID_FIELD_TYPE,
                        format!(
                            "{field:?} must be an unsigned 32-bit integer, found {}",
                            value
                        ),
                        json_path,
                    )
                    .entity(entity)
                    .component(component)
                    .field(field),
                );
                return false;
            }
            let number = value.as_number().expect("checked above");
            check_bounds(
                cx, schema, number, component, entity, field, field, json_path, errors,
            );
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
            let Some(text) = value.as_str() else {
                errors.push(
                    cx.wrong_type(field, "string", value, json_path)
                        .entity(entity)
                        .component(component),
                );
                return false;
            };

            // Closed string enums (RigidBody.body, Collider.shape): the walk
            // must reject unknown variants itself, or serde's rejection would
            // masquerade as a desync bug. Typos get did_you_mean.
            if let Some(allowed) = enum_values(schema) {
                if !allowed.contains(&text) {
                    let (code, what) = match field {
                        "shape" => (codes::UNKNOWN_SHAPE, "collider shape"),
                        "body" => (codes::UNKNOWN_BODY_KIND, "body kind"),
                        _ => (codes::INVALID_FIELD_TYPE, "value"),
                    };
                    errors.push(
                        cx.err(
                            code,
                            format!(
                                "no {what} named {text:?}; expected one of {}",
                                allowed.join(", ")
                            ),
                            json_path,
                        )
                        .entity(entity)
                        .component(component)
                        .field(field)
                        .suggest_from(text, allowed.iter().copied()),
                    );
                    return false;
                }
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
                // Fixed arity ([f32; 3] fields) is a shape error — serde
                // rejects the wrong length too. An open-ended bound (a Vec
                // with a minimum, like Breakable.fragments) still parses, so
                // it reports as a range violation and checking continues —
                // the walk must never be stricter than the loader about what
                // *parses* (the corpus agreement property).
                if min_items.is_some() && min_items == max_items {
                    errors.push(
                        cx.err(
                            codes::INVALID_FIELD_TYPE,
                            format!(
                                "{field:?} must be an array of exactly {} elements, found {len}",
                                min_items.unwrap_or(0)
                            ),
                            json_path,
                        )
                        .entity(entity)
                        .component(component)
                        .field(field),
                    );
                    return false;
                }
                let expected = match (min_items, max_items) {
                    (Some(a), Some(b)) => format!("between {a} and {b}"),
                    (Some(a), None) => format!("at least {a}"),
                    (None, Some(b)) => format!("at most {b}"),
                    (None, None) => unreachable!("guarded above"),
                };
                errors.push(
                    cx.err(
                        codes::VALUE_OUT_OF_RANGE,
                        format!("{field:?} must have {expected} elements, found {len}"),
                        json_path,
                    )
                    .entity(entity)
                    .component(component)
                    .field(field),
                );
            }

            let item_schema = schemas.resolve(&schema["items"]);
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
                        cx,
                        item_schema,
                        number,
                        component,
                        entity,
                        field,
                        &label,
                        &item_path,
                        errors,
                    );
                } else if item_schema["type"].as_str() == Some("object") {
                    // Arrays of objects (Breakable.fragments): recurse, so a
                    // bad fragment is a located walk error rather than a
                    // serde rejection masquerading as scene_parse_desync.
                    clean &= check_value(
                        cx,
                        schemas,
                        item_schema,
                        item,
                        component,
                        entity,
                        field,
                        &item_path,
                        errors,
                    );
                }
            }
            clean
        }

        Some("object") => {
            let Some(map) = value.as_object() else {
                errors.push(
                    cx.wrong_type(field, "object", value, json_path)
                        .entity(entity)
                        .component(component),
                );
                return false;
            };

            let empty = Map::new();
            let properties = schema["properties"].as_object().unwrap_or(&empty);
            let mut clean = true;

            for key in map.keys() {
                if !properties.contains_key(key.as_str()) {
                    clean = false;
                    errors.push(
                        cx.err(
                            codes::UNKNOWN_FIELD,
                            format!("{field:?} entries have no field {key:?}"),
                            &format!("{json_path}/{key}"),
                        )
                        .entity(entity)
                        .component(component)
                        .field(key)
                        .suggest_from(key, properties.keys().map(String::as_str)),
                    );
                }
            }

            if let Some(required) = schema["required"].as_array() {
                for req in required.iter().filter_map(Value::as_str) {
                    if !map.contains_key(req) {
                        clean = false;
                        errors.push(
                            cx.err(
                                codes::MISSING_FIELD,
                                format!("each {field:?} entry requires the field {req:?}"),
                                json_path,
                            )
                            .entity(entity)
                            .component(component)
                            .field(req),
                        );
                    }
                }
            }

            for (key, item) in map {
                let Some(property) = properties.get(key.as_str()) else {
                    continue; // already reported as unknown
                };
                let property = schemas.resolve(property);
                clean &= check_value(
                    cx,
                    schemas,
                    property,
                    item,
                    component,
                    entity,
                    key,
                    &format!("{json_path}/{key}"),
                    errors,
                );
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
pub(super) fn check_bounds(
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
pub(super) fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.is_finite() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}
