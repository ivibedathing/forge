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

use crate::gizmo::AXES;

/// One committed inspector action. Field edits carry their value; structure
/// edits (add/remove component) carry the component type — the doc resolves
/// both against fresh file contents.
#[derive(Debug, Clone, PartialEq)]
pub enum InspectorEdit {
    Set(SetComponentField),
    AddComponent(String),
    RemoveComponent(String),
}

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
) -> Vec<InspectorEdit> {
    let mut commits = Vec::new();

    let Some(entity_value) = raw["entities"]
        .as_array()
        .and_then(|es| es.iter().find(|e| e["name"] == entity))
    else {
        ui.label("entity not present in the file");
        return commits;
    };

    let components = entity_value["components"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    for component in &components {
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
        ui.horizontal(|ui| {
            ui.strong(type_name);
            if !read_only {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let remove = ui
                        .small_button("❌")
                        .on_hover_text(format!("remove {type_name}"));
                    if remove.clicked() {
                        commits.push(InspectorEdit::RemoveComponent(type_name.to_string()));
                    }
                });
            }
        });

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
                commits.push(InspectorEdit::Set(SetComponentField {
                    entity: entity.to_string(),
                    component: type_name.to_string(),
                    field: field.clone(),
                    value,
                }));
            }
        }
    }

    if !read_only {
        ui.separator();
        let present: Vec<&str> = components
            .iter()
            .filter_map(|c| c["type"].as_str())
            .collect();
        ui.menu_button("+ add component", |ui| {
            for variant in schema["oneOf"].as_array().into_iter().flatten() {
                let Some(name) = variant["properties"]["type"]["const"].as_str() else {
                    continue;
                };
                if present.contains(&name) {
                    continue;
                }
                if ui.button(name).clicked() {
                    commits.push(InspectorEdit::AddComponent(name.to_string()));
                    ui.close();
                }
            }
        });
    }

    commits
}

/// Row labels for a vec3 field: R/G/B for color-like fields, X/Y/Z for
/// spatial ones. Tints come from [`AXES`] so the inspector letters match
/// the viewport gizmo arms.
fn axis_labels(field: &str) -> [(&'static str, egui::Color32); 3] {
    let letters = if matches!(field, "albedo" | "emissive" | "color") {
        ["R", "G", "B"]
    } else {
        ["X", "Y", "Z"]
    };
    [
        (letters[0], AXES[0].1),
        (letters[1], AXES[1].1),
        (letters[2], AXES[2].1),
    ]
}

/// Drag speed, displayed decimals, and unit suffix for a vec3 field. Angles
/// (the file convention is Euler degrees) scrub in half-degree steps; length
/// -like values in hundredths, three decimals shown like Blender's sidebar.
fn vec3_style(field: &str) -> (f64, usize, &'static str) {
    match field {
        "rotation" => (0.5, 1, "°"),
        "angular_velocity" => (0.5, 1, "°/s"),
        _ => (0.01, 3, ""),
    }
}

/// A `[f32; 3]` whose components live in `[0, 1]` is a color — that range
/// on a triple means linear RGB everywhere in the schema (`albedo`,
/// `emissive`, light `color`), and nothing else uses it.
fn is_color(property: &Value) -> bool {
    bounds_of(&property["items"]) == (0.0, 1.0)
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

    if property["type"].as_str() == Some("array") {
        ui.add_enabled_ui(!read_only, |ui| {
            committed = vec3_widget(ui, state, key, field, property, current);
        });
        return committed;
    }

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

            _ => {
                ui.label(format!("{current}"));
            }
        });
    });

    committed
}

/// A Blender-style vec3 block: the field name above three full-width rows,
/// one per axis, each row a scrub-draggable value (click to type). Values
/// stage as one array and commit whole on gesture end, so a drag on X is
/// still exactly one write.
fn vec3_widget(
    ui: &mut Ui,
    state: &mut InspectorState,
    key: &str,
    field: &str,
    property: &Value,
    current: &Value,
) -> Option<Value> {
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
    let (speed, decimals, suffix) = vec3_style(field);

    let mut changed = false;
    let mut gesture_ended = false;
    let mut mid_gesture = false;

    ui.horizontal(|ui| {
        ui.label(field);
        if is_color(property) {
            // Color values are linear RGB — exactly what the egui picker
            // edits — so no conversion on either side.
            let mut rgb = [values[0] as f32, values[1] as f32, values[2] as f32];
            let response = ui.color_edit_button_rgb(&mut rgb);
            if response.changed() {
                values = rgb.iter().map(|&v| f64::from(v)).collect();
                changed = true;
            }
            // The picker lives in a popup with no gesture-end signal of its
            // own; the write happens when the popup closes.
            mid_gesture |= egui::Popup::is_any_open(ui.ctx());
        }
    });

    let row_height = ui.spacing().interact_size.y;
    for (value, (letter, color)) in values.iter_mut().zip(axis_labels(field)) {
        ui.horizontal(|ui| {
            ui.add_sized(
                [14.0, row_height],
                egui::Label::new(egui::RichText::new(letter).color(color).strong()),
            );
            let response = ui.add_sized(
                [ui.available_width(), row_height],
                DragValue::new(value)
                    .speed(speed)
                    .range(min..=max)
                    .fixed_decimals(decimals)
                    .suffix(suffix),
            );
            changed |= response.changed();
            gesture_ended |= response.drag_stopped() || response.lost_focus();
            mid_gesture |= response.dragged() || response.has_focus();
        });
    }

    if changed {
        let array: Vec<Value> = values.iter().map(|v| number_from_f32(*v as f32)).collect();
        state.staged.insert(key.to_string(), Value::Array(array));
    }
    if gesture_ended || (state.staged.contains_key(key) && !mid_gesture && !changed) {
        if let Some(staged) = state.staged.remove(key) {
            if &staged != current {
                return Some(staged);
            }
        }
    }
    None
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

    /// Renders the inspector headlessly and writes a PNG — the agent's way
    /// to *look at* the inspector, which `--self-screenshot` cannot show
    /// (it reads back only the viewport texture). Ignored by default; run
    /// with `cargo test -p engine-editor inspector_preview -- --ignored`.
    /// Output: `$INSPECTOR_PREVIEW_OUT` or `<temp>/inspector_preview.png`.
    #[test]
    #[ignore = "writes a preview image; for interactive/agent verification"]
    fn inspector_preview() {
        let schema = engine_core::schema::component_schema();
        let raw = serde_json::json!({
            "entities": [{
                "name": "Cube1",
                "components": [
                    { "type": "Transform",
                      "position": [0.0, 3.0, 0.0],
                      "rotation": [0.0, 45.0, 0.0],
                      "scale": [1.0, 1.0, 1.0] },
                    { "type": "Material", "albedo": [0.8, 0.2, 0.2] }
                ]
            }]
        });
        let mut state = InspectorState::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(320.0, 720.0))
            .build_ui(|ui| {
                ui.heading("inspector");
                ui.strong("Cube1");
                super::ui(ui, &mut state, &schema, &raw, "Cube1", false);
            });
        harness.run();
        let image = harness.render().expect("kittest render");
        let out = std::env::var_os("INSPECTOR_PREVIEW_OUT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("inspector_preview.png"));
        image.save(&out).expect("save preview png");
        eprintln!("[inspector-preview] wrote {}", out.display());
    }

    #[test]
    fn color_fields_get_rgb_letters_spatial_get_xyz() {
        assert_eq!(axis_labels("albedo").map(|(l, _)| l), ["R", "G", "B"]);
        assert_eq!(axis_labels("emissive").map(|(l, _)| l), ["R", "G", "B"]);
        assert_eq!(axis_labels("color").map(|(l, _)| l), ["R", "G", "B"]);
        assert_eq!(axis_labels("position").map(|(l, _)| l), ["X", "Y", "Z"]);
        assert_eq!(axis_labels("half_extents").map(|(l, _)| l), ["X", "Y", "Z"]);
        // The letter tints are the gizmo arm colors, not a second palette.
        assert_eq!(axis_labels("position").map(|(_, c)| c), AXES.map(|(_, c)| c));
    }

    #[test]
    fn angle_fields_scrub_in_degrees() {
        assert_eq!(vec3_style("rotation"), (0.5, 1, "°"));
        assert_eq!(vec3_style("angular_velocity"), (0.5, 1, "°/s"));
        assert_eq!(vec3_style("position"), (0.01, 3, ""));
    }

    /// Drives the real widgets: opening "+ add component" and choosing an
    /// absent type emits `AddComponent`; the header ❌ emits
    /// `RemoveComponent`. The formatter tests own what the splices do to
    /// the file; this pins that the UI actually asks for them.
    #[test]
    fn add_menu_and_remove_button_emit_structure_edits() {
        use std::cell::RefCell;
        use std::rc::Rc;

        use egui_kittest::kittest::Queryable;

        let schema = engine_core::schema::component_schema();
        let raw = serde_json::json!({
            "entities": [{ "name": "Cube1", "components": [ { "type": "Material" } ] }]
        });
        let commits: Rc<RefCell<Vec<InspectorEdit>>> = Rc::default();
        let sink = commits.clone();
        let state = Rc::new(RefCell::new(InspectorState::default()));
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(320.0, 720.0))
            .build_ui(move |ui| {
                let mut state = state.borrow_mut();
                let edits = super::ui(ui, &mut state, &schema, &raw, "Cube1", false);
                sink.borrow_mut().extend(edits);
            });
        harness.run();

        harness.get_by_label("+ add component").click();
        harness.run();
        harness.get_by_label("Mesh").click();
        harness.run();
        harness.get_by_label("❌").click();
        harness.run();

        let commits = commits.borrow();
        assert!(
            commits.contains(&InspectorEdit::AddComponent("Mesh".into())),
            "{commits:?}"
        );
        assert!(
            commits.contains(&InspectorEdit::RemoveComponent("Material".into())),
            "{commits:?}"
        );
    }

    #[test]
    fn color_detection_is_exactly_the_unit_range_triples() {
        let schema = engine_core::schema::component_schema();
        let property_of = |component: &str, field: &str| {
            schema["oneOf"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["properties"]["type"]["const"] == component)
                .unwrap()["properties"][field]
                .clone()
        };
        assert!(is_color(&property_of("Material", "albedo")));
        assert!(is_color(&property_of("Material", "emissive")));
        assert!(is_color(&property_of("DirectionalLight", "color")));
        assert!(is_color(&property_of("AmbientLight", "color")));
        assert!(!is_color(&property_of("Transform", "position")));
        assert!(!is_color(&property_of("Transform", "scale")));
        assert!(!is_color(&property_of("Collider", "offset")));
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
