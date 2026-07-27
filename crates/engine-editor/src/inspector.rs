//! The inspector: component widgets generated from the published component
//! schema, never hand-built per component — a new component is editable the
//! day it exists (invariant #7 doing double duty).
//!
//! Commit semantics (principle #3): drag-stop or field blur commits exactly
//! one `SetComponentField` through the shared formatter. While a widget is
//! active the value is staged in memory; disk is written once per gesture.

use std::collections::HashMap;

use egui::{DragValue, Ui};
use engine_core::formatter::SetComponentField;
use serde_json::Value;

/// A number that came from an f32 widget, serialized shortest ("0.3", not
/// "0.30000001192092896").
pub fn number_from_f32(v: f32) -> Value {
    let shortest: f64 = v.to_string().parse().unwrap_or(f64::from(v));
    serde_json::Number::from_f64(shortest)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// Numeric bounds for a DragValue, from the property schema. Exclusive
/// bounds get a hair of margin so the widget cannot land exactly on an
/// invalid value.
fn bounds_of(property: &Value) -> (f64, f64) {
    let min = property["minimum"]
        .as_f64()
        .or_else(|| property["exclusiveMinimum"].as_f64().map(|m| m + 1e-3))
        .unwrap_or(f64::NEG_INFINITY);
    let max = property["maximum"]
        .as_f64()
        .or_else(|| property["exclusiveMaximum"].as_f64().map(|m| m - 1e-3))
        .unwrap_or(f64::INFINITY);
    (min, max)
}

/// Per-frame inspector state owned by the app: staged (uncommitted) values
/// and text-edit buffers. Cleared on external reload so a concurrent writer
/// can never fight a stale buffer.
#[derive(Default)]
pub struct InspectorState {
    staged: HashMap<String, Value>,
    text_buffers: HashMap<String, String>,
}

impl InspectorState {
    pub fn clear(&mut self) {
        self.staged.clear();
        self.text_buffers.clear();
    }
}

/// Draw the inspector for `entity` and return any edits committed this
/// frame. `raw` is the parsed scene JSON (the file's truth, not the typed
/// mirror), `schema` the `component_schema()` value.
pub fn ui(
    ui: &mut Ui,
    state: &mut InspectorState,
    schema: &Value,
    raw: &Value,
    entity: &str,
    read_only: bool,
) -> Vec<SetComponentField> {
    let mut commits = Vec::new();

    let Some(entity_value) = raw["entities"]
        .as_array()
        .and_then(|es| es.iter().find(|e| e["name"] == entity))
    else {
        ui.label("entity not present in the file");
        return commits;
    };

    let components = entity_value["components"].as_array();
    let Some(components) = components else {
        ui.label("no components");
        return commits;
    };

    for component in components {
        let Some(type_name) = component["type"].as_str() else {
            ui.label("component without a \"type\"");
            continue;
        };

        let variant = schema["oneOf"].as_array().and_then(|variants| {
            variants
                .iter()
                .find(|v| v["properties"]["type"]["const"] == type_name)
        });

        ui.separator();
        ui.strong(type_name);

        let Some(variant) = variant else {
            ui.colored_label(
                egui::Color32::from_rgb(220, 120, 60),
                "unknown component — see the validation panel",
            );
            continue;
        };

        let Some(properties) = variant["properties"].as_object() else {
            continue;
        };

        for (field, property) in properties {
            if field == "type" {
                continue;
            }
            let current = component
                .get(field)
                .cloned()
                .unwrap_or_else(|| property["default"].clone());
            let key = format!("{entity}/{type_name}/{field}");

            let commit = field_widget(
                ui,
                state,
                &key,
                field,
                property,
                &current,
                read_only,
            );
            if let Some(value) = commit {
                commits.push(SetComponentField {
                    entity: entity.to_string(),
                    component: type_name.to_string(),
                    field: field.clone(),
                    value,
                });
            }
        }
    }

    commits
}

/// One field's widget row. Returns the committed value when this frame ended
/// a gesture (drag stop / blur), never mid-gesture.
fn field_widget(
    ui: &mut Ui,
    state: &mut InspectorState,
    key: &str,
    field: &str,
    property: &Value,
    current: &Value,
    read_only: bool,
) -> Option<Value> {
    let mut committed = None;

    ui.horizontal(|ui| {
        ui.label(field);
        ui.add_enabled_ui(!read_only, |ui| match property["type"].as_str() {
            Some("number") => {
                let staged = state.staged.get(key).cloned();
                let mut value = staged
                    .as_ref()
                    .or(Some(current))
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                let (min, max) = bounds_of(property);
                let response = ui.add(DragValue::new(&mut value).speed(0.01).range(min..=max));
                if response.changed() {
                    state
                        .staged
                        .insert(key.to_string(), number_from_f32(value as f32));
                }
                if response.drag_stopped() || response.lost_focus() {
                    if let Some(staged) = state.staged.remove(key) {
                        if &staged != current {
                            committed = Some(staged);
                        }
                    }
                }
            }

            Some("boolean") => {
                let mut value = current.as_bool().unwrap_or(false);
                if ui.checkbox(&mut value, "").changed() {
                    // A click is a completed action; commit immediately.
                    committed = Some(Value::Bool(value));
                }
            }

            Some("string") => {
                let buffer = state
                    .text_buffers
                    .entry(key.to_string())
                    .or_insert_with(|| current.as_str().unwrap_or_default().to_string());
                let response = ui.text_edit_singleline(buffer);
                let submitted =
                    response.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter));
                if submitted {
                    let text = buffer.clone();
                    state.text_buffers.remove(key);
                    if current.as_str() != Some(text.as_str()) {
                        committed = Some(Value::String(text));
                    }
                }
            }

            Some("array") => {
                let staged = state.staged.get(key).cloned();
                let mut values: Vec<f64> = staged
                    .as_ref()
                    .or(Some(current))
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_f64).collect())
                    .unwrap_or_default();
                if values.len() != 3 {
                    values = vec![0.0; 3];
                }
                let (min, max) = bounds_of(&property["items"]);

                let mut changed = false;
                let mut gesture_ended = false;
                for value in values.iter_mut() {
                    let response =
                        ui.add(DragValue::new(value).speed(0.01).range(min..=max));
                    changed |= response.changed();
                    gesture_ended |= response.drag_stopped() || response.lost_focus();
                }
                if changed {
                    let array: Vec<Value> = values
                        .iter()
                        .map(|v| number_from_f32(*v as f32))
                        .collect();
                    state.staged.insert(key.to_string(), Value::Array(array));
                }
                if gesture_ended {
                    if let Some(staged) = state.staged.remove(key) {
                        if &staged != current {
                            committed = Some(staged);
                        }
                    }
                }
            }

            _ => {
                ui.label(format!("{current}"));
            }
        });
    });

    committed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_numbers_serialize_shortest() {
        assert_eq!(number_from_f32(0.3), serde_json::json!(0.3));
        assert_eq!(number_from_f32(45.0), serde_json::json!(45.0));
        assert_eq!(
            engine_core::formatter::format_value(&number_from_f32(0.3)),
            "0.3"
        );
    }

    #[test]
    fn exclusive_bounds_get_margin() {
        let property = serde_json::json!({
            "type": "number",
            "exclusiveMinimum": 0.0,
            "exclusiveMaximum": 180.0
        });
        let (min, max) = bounds_of(&property);
        assert!(min > 0.0 && min < 0.01);
        assert!(max < 180.0 && max > 179.9);
    }
}
