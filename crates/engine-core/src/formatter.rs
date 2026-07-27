//! Format-preserving scene file edits.
//!
//! The editor's writes — and any future CLI convenience command — go through
//! this module, so a gizmo drag and an `engine edit-entity` produce
//! byte-identical output for the same logical change. The contract is
//! principle #5 of the editor design: **a write touches only the bytes of
//! the value it changes.** No reformatting, no key reordering, no churn in
//! entities the edit didn't touch. Diff noise is corruption of the agent's
//! medium; `git diff` after a one-field edit must show one hunk, one line.
//!
//! Mechanically this is a splice, not a serialize: a byte-span index over
//! the raw source (the same lexing approach as [`crate::lineindex`], keyed
//! by the same JSON-Pointer paths) locates the value, and the new value's
//! text replaces exactly that range. `load → save` of untouched content is
//! byte-identical by construction, because there is no save step — only
//! edits.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use crate::codes;
use crate::error::{EngineError, Result};

/// Byte range of a JSON value in the source, end-exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// Map from JSON-Pointer-like paths (the [`crate::lineindex`] convention) to
/// the byte span of the value at that path.
pub struct SpanIndex {
    spans: HashMap<String, Span>,
}

enum Frame {
    Object {
        key: Option<String>,
        expecting_key: bool,
        /// The path of the object itself, captured at `{` — reconstructing
        /// it at `}` is impossible because sibling keys have moved on.
        path: String,
        start: usize,
    },
    Array {
        index: usize,
        path: String,
        start: usize,
    },
}

impl SpanIndex {
    /// Lex `source` (assumed to be valid JSON — callers validate first) and
    /// record every value's byte span.
    pub fn new(source: &str) -> Self {
        let mut spans = HashMap::new();
        let mut stack: Vec<Frame> = Vec::new();
        let mut chars = source.char_indices().peekable();

        while let Some((at, c)) = chars.next() {
            match c {
                c if c.is_whitespace() => {}

                '"' => {
                    let mut end = at + 1;
                    let mut text = String::new();
                    while let Some((i, c)) = chars.next() {
                        end = i + c.len_utf8();
                        match c {
                            '\\' => {
                                text.push(c);
                                // Consume the escaped character.
                                if let Some((j, escaped)) = chars.next() {
                                    text.push(escaped);
                                    end = j + escaped.len_utf8();
                                }
                            }
                            '"' => break,
                            c => text.push(c),
                        }
                    }

                    match stack.last_mut() {
                        Some(Frame::Object {
                            key,
                            expecting_key: expecting @ true,
                            ..
                        }) => {
                            *key = Some(text);
                            *expecting = false;
                        }
                        _ => {
                            if let Some(path) = path_of(&stack) {
                                spans.insert(path, Span { start: at, end });
                            }
                        }
                    }
                }

                '{' => {
                    let path = path_of(&stack);
                    stack.push(Frame::Object {
                        key: None,
                        expecting_key: true,
                        path: path.unwrap_or_default(),
                        start: at,
                    });
                    // A container at an unaddressable position still needs a
                    // frame (for nesting), but records no span — mark it by
                    // storing an impossible path. Simpler: always store; the
                    // root path is "" which is addressable.
                }
                '[' => {
                    let path = path_of(&stack);
                    stack.push(Frame::Array {
                        index: 0,
                        path: path.unwrap_or_default(),
                        start: at,
                    });
                }
                '}' | ']' => {
                    if let Some(frame) = stack.pop() {
                        let (path, start) = match frame {
                            Frame::Object { path, start, .. } => (path, start),
                            Frame::Array { path, start, .. } => (path, start),
                        };
                        spans.insert(path, Span { start, end: at + 1 });
                    }
                }

                ',' => match stack.last_mut() {
                    Some(Frame::Object {
                        key, expecting_key, ..
                    }) => {
                        *key = None;
                        *expecting_key = true;
                    }
                    Some(Frame::Array { index, .. }) => *index += 1,
                    None => {}
                },

                ':' => {}

                // Numbers, true/false/null.
                c if c.is_ascii_digit() || c == '-' || c == 't' || c == 'f' || c == 'n' => {
                    let mut end = at + c.len_utf8();
                    while let Some(&(i, next)) = chars.peek() {
                        if next.is_ascii_alphanumeric() || next == '.' || next == '+' || next == '-'
                        {
                            end = i + next.len_utf8();
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if let Some(path) = path_of(&stack) {
                        spans.insert(path, Span { start: at, end });
                    }
                }

                _ => {}
            }
        }

        Self { spans }
    }

    pub fn span_of(&self, path: &str) -> Option<Span> {
        self.spans.get(path).copied()
    }
}

/// The addressable path of the value about to start, given the open
/// containers. `None` when between object entries (no current key).
fn path_of(stack: &[Frame]) -> Option<String> {
    let mut path = String::new();
    for frame in stack {
        match frame {
            Frame::Object { key: Some(key), .. } => {
                path.push('/');
                path.push_str(key);
            }
            Frame::Object { key: None, .. } => return None,
            Frame::Array { index, .. } => {
                path.push('/');
                path.push_str(&index.to_string());
            }
        }
    }
    Some(path)
}

/// A number that came from an f32, serialized shortest ("0.3", never
/// "0.30000001192092896") — the representation a human would have typed.
pub fn number_from_f32(v: f32) -> Value {
    let shortest: f64 = v.to_string().parse().unwrap_or(f64::from(v));
    serde_json::Number::from_f64(shortest)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// Serialize a JSON value in scene-file style: floats keep a decimal point
/// (`3.0`, not `3`), arrays of scalars go on one line with `", "` separators
/// — the style every example scene and the M-fixtures use.
pub fn format_value(value: &Value) -> String {
    match value {
        Value::Number(n) => format_number(n),
        Value::Array(items) if items.iter().all(|v| !v.is_array() && !v.is_object()) => {
            let inner: Vec<String> = items.iter().map(format_value).collect();
            format!("[{}]", inner.join(", "))
        }
        // Strings, bools, null, and (rare) nested containers: serde's
        // compact form is already correct.
        other => serde_json::to_string(other)
            .unwrap_or_else(|_| "null".to_string()),
    }
}

fn format_number(n: &serde_json::Number) -> String {
    if let Some(f) = n.as_f64() {
        // Scene numbers are f32 fields; write integral floats as "3.0" so a
        // number edited by the editor looks like a number typed by a human.
        if n.is_f64() || f.fract() != 0.0 {
            if f.fract() == 0.0 && f.is_finite() && f.abs() < 1e15 {
                return format!("{:.1}", f);
            }
            return n.to_string();
        }
        // Integers written as integers (entity counts, future int fields).
        return n.to_string();
    }
    n.to_string()
}

/// Replace the value at `pointer` with `value`, preserving every other byte.
pub fn set_value(source: &str, pointer: &str, value: &Value) -> Result<String> {
    let index = SpanIndex::new(source);
    let span = index.span_of(pointer).ok_or_else(|| {
        EngineError::new(
            codes::EDIT_TARGET_MISSING,
            format!("nothing at {pointer:?} to edit"),
        )
        .path(pointer)
    })?;

    let mut edited = String::with_capacity(source.len() + 32);
    edited.push_str(&source[..span.start]);
    edited.push_str(&format_value(value));
    edited.push_str(&source[span.end..]);
    check_still_parses(&edited, pointer)?;
    Ok(edited)
}

/// Insert `"key": value` into the object at `object_pointer` (which must not
/// already contain `key` — use [`set_value`] for that). Matches the object's
/// own layout: inline objects gain `, "key": value` before the brace;
/// multi-line objects gain a correctly indented new line.
pub fn insert_key(source: &str, object_pointer: &str, key: &str, value: &Value) -> Result<String> {
    let index = SpanIndex::new(source);
    let span = index.span_of(object_pointer).ok_or_else(|| {
        EngineError::new(
            codes::EDIT_TARGET_MISSING,
            format!("no object at {object_pointer:?} to insert into"),
        )
        .path(object_pointer)
    })?;

    let object_text = &source[span.start..span.end];
    let inner = &object_text[1..object_text.len() - 1];
    let is_empty = inner.trim().is_empty();

    // Where the insertion goes: directly after the last non-whitespace
    // character inside the braces (the previous property's end), so trailing
    // whitespace and the closing brace keep their exact bytes.
    let insert_at = span.start
        + 1
        + inner
            .rfind(|c: char| !c.is_whitespace())
            .map_or(0, |i| i + inner[i..].chars().next().map_or(1, char::len_utf8));

    let entry = format!("{:?}: {}", key, format_value(value));
    let insertion = if is_empty {
        // "{}" or "{ }": keep the padding style of the empty object.
        if inner.is_empty() {
            format!(" {entry} ")
        } else {
            entry
        }
    } else if inner.contains('\n') {
        // Multi-line object: match the indentation of its properties.
        let last_line_start = source[..insert_at].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let indent: String = source[last_line_start..]
            .chars()
            .take_while(|c| c.is_whitespace())
            .collect();
        format!(",\n{indent}{entry}")
    } else {
        format!(", {entry}")
    };

    let mut edited = String::with_capacity(source.len() + insertion.len());
    edited.push_str(&source[..insert_at]);
    edited.push_str(&insertion);
    edited.push_str(&source[insert_at..]);
    check_still_parses(&edited, object_pointer)?;
    Ok(edited)
}

/// The one logical mutation the editor commits: set one field of one
/// component on one entity. Addressed by stable `name`/`type` — never by
/// index — so it can be re-applied ("rebased") onto a file another writer
/// changed mid-gesture.
#[derive(Debug, Clone, PartialEq)]
pub struct SetComponentField {
    pub entity: String,
    pub component: String,
    pub field: String,
    pub value: Value,
}

/// Apply a [`SetComponentField`] to `source`. Resolves the entity by name
/// and the component by type in *this* source, so applying to freshly
/// re-read contents is exactly the conflict policy of editor design §5.
/// A missing field is inserted; a missing entity or component is
/// `edit_target_missing` — the caller drops the edit and says so.
pub fn apply_set_component_field(source: &str, edit: &SetComponentField) -> Result<String> {
    let root: Value = serde_json::from_str(source).map_err(|e| {
        EngineError::new(
            codes::EDIT_TARGET_MISSING,
            format!("cannot edit a file that no longer parses: {e}"),
        )
    })?;

    let entities = root["entities"].as_array().ok_or_else(|| {
        EngineError::new(codes::EDIT_TARGET_MISSING, "the file has no entities array")
    })?;

    let entity_index = entities
        .iter()
        .position(|e| e["name"] == edit.entity.as_str())
        .ok_or_else(|| {
            EngineError::new(
                codes::EDIT_TARGET_MISSING,
                format!("entity {:?} is no longer in the file", edit.entity),
            )
            .entity(&edit.entity)
        })?;

    let components = entities[entity_index]["components"]
        .as_array()
        .ok_or_else(|| {
            EngineError::new(
                codes::EDIT_TARGET_MISSING,
                format!("entity {:?} has no components array", edit.entity),
            )
            .entity(&edit.entity)
        })?;

    let component_index = components
        .iter()
        .position(|c| c["type"] == edit.component.as_str())
        .ok_or_else(|| {
            EngineError::new(
                codes::EDIT_TARGET_MISSING,
                format!(
                    "entity {:?} no longer has a {} component",
                    edit.entity, edit.component
                ),
            )
            .entity(&edit.entity)
            .component(&edit.component)
        })?;

    let component_pointer =
        format!("/entities/{entity_index}/components/{component_index}");
    let field_pointer = format!("{component_pointer}/{}", edit.field);

    if components[component_index]
        .as_object()
        .is_some_and(|c| c.contains_key(&edit.field))
    {
        set_value(source, &field_pointer, &edit.value)
    } else {
        insert_key(source, &component_pointer, &edit.field, &edit.value)
    }
}

/// A new entity appended to the scene's `entities` array — the first
/// structure edit (editor drag-and-drop import). Components are ordered
/// pairs rather than a `Value` object so the authored key order survives
/// into the file (`"type"` first, then fields as given).
#[derive(Debug, Clone, PartialEq)]
pub struct AddEntity {
    pub name: String,
    /// `(component type, fields in authoring order)`.
    pub components: Vec<(String, Vec<(String, Value)>)>,
}

/// Append `edit` to the `entities` array, preserving every existing byte and
/// matching the file's layout (multi-line arrays gain an indented block,
/// inline arrays an inline object). A name collision is an error — the
/// caller picks a free name against fresh file contents; this is the
/// backstop for the rebase race.
pub fn apply_add_entity(source: &str, edit: &AddEntity) -> Result<String> {
    let root: Value = serde_json::from_str(source).map_err(|e| {
        EngineError::new(
            codes::EDIT_TARGET_MISSING,
            format!("cannot edit a file that no longer parses: {e}"),
        )
    })?;
    let entities = root["entities"].as_array().ok_or_else(|| {
        EngineError::new(codes::EDIT_TARGET_MISSING, "the file has no entities array")
    })?;
    if entities.iter().any(|e| e["name"] == edit.name.as_str()) {
        return Err(EngineError::new(
            codes::DUPLICATE_ENTITY_NAME,
            format!("an entity named {:?} already exists", edit.name),
        )
        .entity(&edit.name));
    }

    let index = SpanIndex::new(source);
    let span = index.span_of("/entities").ok_or_else(|| {
        EngineError::new(codes::EDIT_TARGET_MISSING, "no entities array to append to")
            .path("/entities")
    })?;
    let inner = &source[span.start + 1..span.end - 1];

    // Directly after the last entity's closing brace (or after `[` when
    // empty), so the bytes before and after the insertion stay untouched.
    let insert_at = span.start
        + 1
        + inner
            .rfind(|c: char| !c.is_whitespace())
            .map_or(0, |i| i + inner[i..].chars().next().map_or(1, char::len_utf8));

    let insertion = if inner.contains('\n') {
        // Multi-line array: match the first entity's indentation, or step
        // once in from the array's own line when it is empty.
        let indent = match index.span_of("/entities/0") {
            Some(first) => line_indent(source, first.start),
            None => format!("{}  ", line_indent(source, span.start)),
        };
        let text = entity_text(edit, &indent);
        if entities.is_empty() {
            format!("\n{text}")
        } else {
            format!(",\n{text}")
        }
    } else if entities.is_empty() {
        if inner.is_empty() {
            format!(" {} ", entity_text_inline(edit))
        } else {
            entity_text_inline(edit)
        }
    } else {
        format!(", {}", entity_text_inline(edit))
    };

    let mut edited = String::with_capacity(source.len() + insertion.len());
    edited.push_str(&source[..insert_at]);
    edited.push_str(&insertion);
    edited.push_str(&source[insert_at..]);
    check_still_parses(&edited, "/entities")?;
    Ok(edited)
}

/// Leading whitespace of the line containing byte `at`.
fn line_indent(source: &str, at: usize) -> String {
    let line_start = source[..at].rfind('\n').map_or(0, |i| i + 1);
    source[line_start..at]
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect()
}

/// One component in scene style: `{ "type": "Mesh", "asset": "x.glb" }`.
fn component_text(kind: &str, fields: &[(String, Value)]) -> String {
    let mut parts = vec![format!("\"type\": {kind:?}")];
    parts.extend(fields.iter().map(|(k, v)| format!("{k:?}: {}", format_value(v))));
    format!("{{ {} }}", parts.join(", "))
}

/// The entity as an indented block, `indent` being its opening brace's
/// indentation. Two-space steps — the style of every example scene.
fn entity_text(edit: &AddEntity, indent: &str) -> String {
    let mut out = format!("{indent}{{\n{indent}  \"name\": {:?},\n", edit.name);
    if edit.components.is_empty() {
        out.push_str(&format!("{indent}  \"components\": []\n"));
    } else {
        out.push_str(&format!("{indent}  \"components\": [\n"));
        for (i, (kind, fields)) in edit.components.iter().enumerate() {
            let comma = if i + 1 < edit.components.len() { "," } else { "" };
            out.push_str(&format!("{indent}    {}{comma}\n", component_text(kind, fields)));
        }
        out.push_str(&format!("{indent}  ]\n"));
    }
    out.push_str(&format!("{indent}}}"));
    out
}

fn entity_text_inline(edit: &AddEntity) -> String {
    let components: Vec<String> = edit
        .components
        .iter()
        .map(|(kind, fields)| component_text(kind, fields))
        .collect();
    format!(
        "{{ \"name\": {:?}, \"components\": [ {} ] }}",
        edit.name,
        components.join(", ")
    )
}

/// Write atomically: temp file in the same directory, then rename, so a
/// concurrently reading agent never sees a half-written scene.
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let display = path.display().to_string();
    let io_error = |e: std::io::Error| {
        EngineError::new(
            codes::SCENE_WRITE_FAILED,
            format!("could not write scene: {e}"),
        )
        .file(&display)
    };

    let directory = path.parent().unwrap_or(Path::new("."));
    let temp = directory.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("scene"),
        std::process::id()
    ));

    std::fs::write(&temp, contents).map_err(io_error)?;
    std::fs::rename(&temp, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        io_error(e)
    })
}

/// A spliced edit must still be valid JSON; anything else is a bug in this
/// module, and the broken text must never reach the disk.
fn check_still_parses(edited: &str, pointer: &str) -> Result<()> {
    serde_json::from_str::<Value>(edited).map(|_| ()).map_err(|e| {
        EngineError::new(
            codes::FORMATTER_DESYNC,
            format!("editing {pointer:?} produced invalid JSON ({e}); nothing was written"),
        )
        .path(pointer)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SCENE: &str = r#"{
  "name": "demo",
  "entities": [
    {
      "name": "Sphere",
      "components": [
        { "type": "Transform", "position": [0.0, 1.0, 0.0] },
        { "type": "Mesh", "asset": "builtin:sphere" },
        { "type": "Material", "albedo": [0.9, 0.1, 0.1], "roughness": 0.4 }
      ]
    },
    { "name": "Camera1", "components": [ { "type": "Camera", "active": true } ] }
  ]
}"#;

    #[test]
    fn set_value_changes_exactly_one_line() {
        let edited = set_value(
            SCENE,
            "/entities/0/components/2/roughness",
            &json!(0.3),
        )
        .unwrap();

        let changed: Vec<(&str, &str)> = SCENE
            .lines()
            .zip(edited.lines())
            .filter(|(a, b)| a != b)
            .collect();
        assert_eq!(changed.len(), 1, "exactly one line may change");
        assert!(changed[0].1.contains("\"roughness\": 0.3"));
        assert_eq!(
            SCENE.lines().count(),
            edited.lines().count(),
            "no lines added or removed"
        );
    }

    #[test]
    fn set_value_preserves_every_untouched_byte() {
        let edited = set_value(
            SCENE,
            "/entities/0/components/0/position",
            &json!([1.5, 3.0, 0.0]),
        )
        .unwrap();

        assert!(edited.contains("\"position\": [1.5, 3.0, 0.0]"));
        // Everything before and after the edited component line is identical.
        let untouched_prefix: String = SCENE.lines().take(5).collect();
        let edited_prefix: String = edited.lines().take(5).collect();
        assert_eq!(untouched_prefix, edited_prefix);
        assert!(edited.ends_with("}"));
        assert_eq!(SCENE.lines().count(), edited.lines().count());
    }

    #[test]
    fn round_trip_of_untouched_content_is_byte_identical() {
        // Setting a value to itself must be a no-op on the whole file — the
        // E1 round-trip contract (load → save → byte-identical), which holds
        // by construction because there is no save step, only splices.
        let edited = set_value(
            SCENE,
            "/entities/0/components/2/roughness",
            &json!(0.4),
        )
        .unwrap();
        assert_eq!(SCENE, edited);
    }

    #[test]
    fn scene_style_number_formatting() {
        assert_eq!(format_value(&json!(0.3)), "0.3");
        assert_eq!(format_value(&json!(3.0)), "3.0");
        assert_eq!(format_value(&json!(-45.0)), "-45.0");
        assert_eq!(format_value(&json!(3)), "3");
        assert_eq!(format_value(&json!([0.0, 45.0, 0.0])), "[0.0, 45.0, 0.0]");
        assert_eq!(format_value(&json!(true)), "true");
        assert_eq!(format_value(&json!("builtin:cube")), "\"builtin:cube\"");
    }

    #[test]
    fn insert_key_into_inline_object_matches_its_style() {
        let edited = insert_key(
            SCENE,
            "/entities/0/components/0",
            "scale",
            &json!([2.0, 2.0, 2.0]),
        )
        .unwrap();
        assert!(
            edited.contains(
                "{ \"type\": \"Transform\", \"position\": [0.0, 1.0, 0.0], \"scale\": [2.0, 2.0, 2.0] }"
            ),
            "{edited}"
        );
        assert_eq!(SCENE.lines().count(), edited.lines().count());
    }

    #[test]
    fn insert_key_into_multiline_object_matches_indentation() {
        let source = "{\n  \"name\": \"s\",\n  \"entities\": [\n    {\n      \"name\": \"A\"\n    }\n  ]\n}";
        let edited = insert_key(source, "/entities/0", "components", &json!([])).unwrap();
        assert!(
            edited.contains("\"name\": \"A\",\n      \"components\": []"),
            "{edited}"
        );
    }

    #[test]
    fn insert_key_into_empty_object() {
        let source = r#"{"name":"s","entities":[{"name":"A","components":[{}]}]}"#;
        // Not a valid scene (component without type), but the formatter is a
        // text tool; validation is someone else's job.
        let edited =
            insert_key(source, "/entities/0/components/0", "type", &json!("Transform")).unwrap();
        assert!(edited.contains(r#"[{ "type": "Transform" }]"#), "{edited}");
    }

    #[test]
    fn missing_pointer_is_edit_target_missing() {
        let err = set_value(SCENE, "/entities/9/components/0", &json!(1)).unwrap_err();
        assert_eq!(err.error, "edit_target_missing");
    }

    #[test]
    fn mutation_addresses_by_name_and_type_not_index() {
        let edit = SetComponentField {
            entity: "Sphere".into(),
            component: "Material".into(),
            field: "roughness".into(),
            value: json!(0.3),
        };
        let edited = apply_set_component_field(SCENE, &edit).unwrap();
        assert!(edited.contains("\"roughness\": 0.3"));

        // The rebase property: reorder the entities (as a concurrent writer
        // might) and the same mutation still lands on the right component.
        let reordered = {
            let mut root: Value = serde_json::from_str(SCENE).unwrap();
            let entities = root["entities"].as_array_mut().unwrap();
            entities.reverse();
            serde_json::to_string_pretty(&root).unwrap()
        };
        let edited = apply_set_component_field(&reordered, &edit).unwrap();
        let root: Value = serde_json::from_str(&edited).unwrap();
        assert_eq!(root["entities"][1]["components"][2]["roughness"], json!(0.3));
    }

    #[test]
    fn mutation_inserts_an_absent_field() {
        let edit = SetComponentField {
            entity: "Sphere".into(),
            component: "Transform".into(),
            field: "rotation".into(),
            value: json!([0.0, 45.0, 0.0]),
        };
        let edited = apply_set_component_field(SCENE, &edit).unwrap();
        assert!(
            edited.contains("\"position\": [0.0, 1.0, 0.0], \"rotation\": [0.0, 45.0, 0.0]"),
            "{edited}"
        );
    }

    #[test]
    fn mutation_against_a_vanished_entity_reports_not_corrupts() {
        let edit = SetComponentField {
            entity: "Gone".into(),
            component: "Transform".into(),
            field: "position".into(),
            value: json!([0.0, 0.0, 0.0]),
        };
        let err = apply_set_component_field(SCENE, &edit).unwrap_err();
        assert_eq!(err.error, "edit_target_missing");
        assert_eq!(err.context().unwrap().entity.as_deref(), Some("Gone"));
    }

    fn pyramid() -> AddEntity {
        AddEntity {
            name: "pyramid".into(),
            components: vec![
                (
                    "Transform".into(),
                    vec![("position".into(), json!([0.0, 0.0, 0.0]))],
                ),
                (
                    "Mesh".into(),
                    vec![("asset".into(), json!("meshes/pyramid.glb"))],
                ),
            ],
        }
    }

    #[test]
    fn add_entity_appends_in_scene_style_and_touches_nothing_else() {
        let edited = apply_add_entity(SCENE, &pyramid()).unwrap();

        // Every original line except the spliced region is byte-identical:
        // the insertion lands between the last entity and the closing `]`.
        assert!(edited.starts_with(&SCENE[..SCENE.rfind("\n  ]").unwrap()]));
        assert!(edited.ends_with("\n  ]\n}"));
        assert!(
            edited.contains(
                "    {\n      \"name\": \"pyramid\",\n      \"components\": [\n        \
                 { \"type\": \"Transform\", \"position\": [0.0, 0.0, 0.0] },\n        \
                 { \"type\": \"Mesh\", \"asset\": \"meshes/pyramid.glb\" }\n      ]\n    }"
            ),
            "{edited}"
        );

        let root: Value = serde_json::from_str(&edited).unwrap();
        assert_eq!(root["entities"].as_array().unwrap().len(), 3);
        assert_eq!(root["entities"][2]["name"], json!("pyramid"));
    }

    #[test]
    fn add_entity_into_empty_and_inline_arrays() {
        let empty = r#"{"name":"s","entities":[]}"#;
        let edited = apply_add_entity(empty, &pyramid()).unwrap();
        assert!(
            edited.contains(r#""entities":[ { "name": "pyramid","#),
            "{edited}"
        );
        serde_json::from_str::<Value>(&edited).unwrap();

        let inline = r#"{"name":"s","entities":[{"name":"A","components":[]}]}"#;
        let edited = apply_add_entity(inline, &pyramid()).unwrap();
        assert!(
            edited.contains(r#"]}, { "name": "pyramid","#),
            "{edited}"
        );
        serde_json::from_str::<Value>(&edited).unwrap();
    }

    #[test]
    fn add_entity_refuses_a_taken_name() {
        let edit = AddEntity {
            name: "Sphere".into(),
            components: vec![],
        };
        let err = apply_add_entity(SCENE, &edit).unwrap_err();
        assert_eq!(err.error, "duplicate_entity_name");
        assert_eq!(err.context().unwrap().entity.as_deref(), Some("Sphere"));
    }

    #[test]
    fn atomic_write_replaces_content() {
        let dir = std::env::temp_dir().join(format!("engine-formatter-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scene.json");
        std::fs::write(&path, "old").unwrap();

        write_atomic(&path, "new contents").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new contents");

        // No temp file left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(leftovers.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn spans_cover_scalars_strings_arrays_and_objects() {
        let index = SpanIndex::new(SCENE);
        let span = index.span_of("/entities/0/components/2/roughness").unwrap();
        assert_eq!(&SCENE[span.start..span.end], "0.4");

        let span = index.span_of("/entities/0/components/1/asset").unwrap();
        assert_eq!(&SCENE[span.start..span.end], "\"builtin:sphere\"");

        let span = index.span_of("/entities/0/components/0/position").unwrap();
        assert_eq!(&SCENE[span.start..span.end], "[0.0, 1.0, 0.0]");

        let span = index.span_of("/entities/1").unwrap();
        assert!(SCENE[span.start..span.end].starts_with("{ \"name\": \"Camera1\""));
        assert!(SCENE[span.start..span.end].ends_with("] }"));

        let span = index.span_of("").unwrap();
        assert_eq!((span.start, span.end), (0, SCENE.len()));
    }
}
