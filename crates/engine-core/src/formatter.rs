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

/// Rewrite every number in a value as the shortest text that round-trips
/// through `f32` — `0.1276797`, not `0.12767969071865082`.
///
/// `serde_json` widens an `f32` to `f64` on the way in, and the widened value
/// prints all seventeen digits of a number the engine only ever had seven of.
/// Anything generated (a fracture's shards, a fitted collider set) goes
/// through this before it is spliced, or the scene file fills with precision
/// nobody wrote and no reader can use.
pub fn shorten_floats(value: &mut Value) {
    match value {
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                *value = number_from_f32(f as f32);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(shorten_floats),
        Value::Object(fields) => fields.values_mut().for_each(shorten_floats),
        _ => {}
    }
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
        // An array of scalar arrays — a shard's points (M43) — in the same
        // style one level up, rather than serde's spaceless compact form.
        Value::Array(items)
            if items.iter().all(|v| {
                v.as_array()
                    .is_some_and(|inner| inner.iter().all(|x| !x.is_array() && !x.is_object()))
            }) =>
        {
            let inner: Vec<String> = items.iter().map(format_value).collect();
            format!("[{}]", inner.join(", "))
        }
        // Strings, bools, null, and (rare) nested containers: serde's
        // compact form is already correct.
        other => serde_json::to_string(other).unwrap_or_else(|_| "null".to_string()),
    }
}

/// Whether a value is one this module breaks across lines rather than writing
/// inline: an array holding arrays or objects.
///
/// The line length of an inline one is unbounded, and *that* is the problem —
/// a fourteen-shard `Breakable` (M43) came out as a single six-thousand
/// character line, which is a JSON scene format that is no longer
/// git-diffable by construction (invariant 1). Arrays of scalars stay inline:
/// a `[0.5, 0.5, 0.5]` is one value and reads like one.
fn is_block(value: &Value) -> bool {
    matches!(value, Value::Array(items) if items.iter().any(|v| v.is_array() || v.is_object()))
}

/// [`format_value`] for a value that has to break across lines, with `indent`
/// the indentation of the line the value *starts* on.
///
/// One element per line, each element itself inline — so a point list reads
/// one point per line and a fragment list one fragment per line, which is the
/// granularity a diff is useful at.
fn format_block(value: &Value, indent: &str) -> String {
    let Value::Array(items) = value else {
        return format_value(value);
    };
    if items.is_empty() {
        return "[]".to_string();
    }
    let inner = format!("{indent}  ");
    let lines: Vec<String> = items
        .iter()
        .map(|item| match item {
            Value::Object(fields) => {
                let pairs: Vec<String> = fields
                    .iter()
                    .map(|(k, v)| format!("{k:?}: {}", format_value(v)))
                    .collect();
                format!("{inner}{{ {} }}", pairs.join(", "))
            }
            other => format!("{inner}{}", format_value(other)),
        })
        .collect();
    format!("[\n{}\n{indent}]", lines.join(",\n"))
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
        + inner.rfind(|c: char| !c.is_whitespace()).map_or(0, |i| {
            i + inner[i..].chars().next().map_or(1, char::len_utf8)
        });

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

    let component_pointer = format!("/entities/{entity_index}/components/{component_index}");
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

    append_array_item(
        source,
        "/entities",
        entities.len(),
        &entity_text_inline(edit),
        &|indent| entity_text(edit, indent),
    )
}

/// Append an item to the array at `array_pointer`, matching the array's own
/// layout: multi-line arrays gain an indented block (via `block_text`, which
/// receives the indentation of the array's items), inline arrays gain
/// `, inline_text`. The bytes before and after the insertion point stay
/// untouched.
fn append_array_item(
    source: &str,
    array_pointer: &str,
    item_count: usize,
    inline_text: &str,
    block_text: &dyn Fn(&str) -> String,
) -> Result<String> {
    let index = SpanIndex::new(source);
    let span = index.span_of(array_pointer).ok_or_else(|| {
        EngineError::new(
            codes::EDIT_TARGET_MISSING,
            format!("no array at {array_pointer:?} to append to"),
        )
        .path(array_pointer)
    })?;
    let inner = &source[span.start + 1..span.end - 1];

    // Directly after the last item's end (or after `[` when empty), so the
    // bytes before and after the insertion stay untouched.
    let insert_at = span.start
        + 1
        + inner.rfind(|c: char| !c.is_whitespace()).map_or(0, |i| {
            i + inner[i..].chars().next().map_or(1, char::len_utf8)
        });

    let insertion = if inner.contains('\n') {
        // Multi-line array: match the first item's indentation, or step
        // once in from the array's own line when it is empty.
        let indent = match index.span_of(&format!("{array_pointer}/0")) {
            Some(first) => line_indent(source, first.start),
            None => format!("{}  ", line_indent(source, span.start)),
        };
        let text = block_text(&indent);
        if item_count == 0 {
            format!("\n{text}")
        } else {
            format!(",\n{text}")
        }
    } else if item_count == 0 {
        if inner.is_empty() {
            format!(" {inline_text} ")
        } else {
            inline_text.to_string()
        }
    } else {
        format!(", {inline_text}")
    };

    let mut edited = String::with_capacity(source.len() + insertion.len());
    edited.push_str(&source[..insert_at]);
    edited.push_str(&insertion);
    edited.push_str(&source[insert_at..]);
    check_still_parses(&edited, array_pointer)?;
    Ok(edited)
}

/// Add a component to an existing entity — the inspector's "+ add
/// component". Addressed by entity `name`, like every other mutation, so it
/// rebases onto fresh file contents. A type the entity already has is
/// `duplicate_component` (the same code the validator would raise); the
/// usual authoring shape is an empty `fields` — absent fields *are* the
/// documented defaults, and the inspector shows them editable.
#[derive(Debug, Clone, PartialEq)]
pub struct AddComponent {
    pub entity: String,
    pub component: String,
    /// Fields in authoring order, usually empty.
    pub fields: Vec<(String, Value)>,
}

/// Apply an [`AddComponent`]: splice `{ "type": ..., ... }` into the
/// entity's `components` array, matching its layout. An entity with no
/// `components` key at all gains one holding just the new component.
pub fn apply_add_component(source: &str, edit: &AddComponent) -> Result<String> {
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

    let components = entities[entity_index]["components"].as_array();
    if components.is_some_and(|cs| cs.iter().any(|c| c["type"] == edit.component.as_str())) {
        return Err(EngineError::new(
            codes::DUPLICATE_COMPONENT,
            format!(
                "entity {:?} already has a {} component",
                edit.entity, edit.component
            ),
        )
        .entity(&edit.entity)
        .component(&edit.component));
    }

    let text = component_text(&edit.component, &edit.fields);
    match components {
        Some(existing) => append_array_item(
            source,
            &format!("/entities/{entity_index}/components"),
            existing.len(),
            &text,
            // Re-rendered at the indentation the array turned out to have,
            // which is what lets a block-shaped field line up under it.
            &|indent| {
                format!(
                    "{indent}{}",
                    component_text_at(&edit.component, &edit.fields, indent)
                )
            },
        ),
        None => {
            // No components array at all: the new key's value is authored as
            // text (via a placeholder splice) so it lands in scene style, not
            // serde's compact form.
            let edited = insert_key(
                source,
                &format!("/entities/{entity_index}"),
                "components",
                &Value::Array(vec![]),
            )?;
            append_array_item(
                &edited,
                &format!("/entities/{entity_index}/components"),
                0,
                &text,
                &|indent| {
                    format!(
                        "{indent}{}",
                        component_text_at(&edit.component, &edit.fields, indent)
                    )
                },
            )
        }
    }
}

/// Remove one component from an entity — the inspector's per-component "✕".
/// Addressed by entity `name` and component `type`; a vanished target is
/// `edit_target_missing`, and the caller drops the edit.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoveComponent {
    pub entity: String,
    pub component: String,
}

/// Apply a [`RemoveComponent`]: delete exactly the component's bytes plus
/// the one separating comma, leaving every other byte in place. Removing the
/// only component leaves `[]`.
pub fn apply_remove_component(source: &str, edit: &RemoveComponent) -> Result<String> {
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

    let base = format!("/entities/{entity_index}/components");
    let index = SpanIndex::new(source);
    let span_of = |pointer: &str| {
        index.span_of(pointer).ok_or_else(|| {
            EngineError::new(
                codes::EDIT_TARGET_MISSING,
                format!("nothing at {pointer:?} to edit"),
            )
            .path(pointer)
        })
    };

    let (cut_start, cut_end, replacement) = if components.len() == 1 {
        // The only component: the array collapses to `[]`.
        let array = span_of(&base)?;
        (array.start, array.end, "[]")
    } else if component_index + 1 < components.len() {
        // Not the last: cut from this component's start to the next one's,
        // which eats the separating comma and keeps this one's indentation
        // bytes for its successor.
        let this = span_of(&format!("{base}/{component_index}"))?;
        let next = span_of(&format!("{base}/{}", component_index + 1))?;
        (this.start, next.start, "")
    } else {
        // The last: cut from the previous component's end through this one's,
        // eating the comma and the line break before it.
        let previous = span_of(&format!("{base}/{}", component_index - 1))?;
        let this = span_of(&format!("{base}/{component_index}"))?;
        (previous.end, this.end, "")
    };

    let mut edited = String::with_capacity(source.len());
    edited.push_str(&source[..cut_start]);
    edited.push_str(replacement);
    edited.push_str(&source[cut_end..]);
    check_still_parses(&edited, &base)?;
    Ok(edited)
}

/// Remove one entity from the scene — bake's structural splice for an entity
/// that no longer exists in the world (it broke into fragments). Addressed
/// by `name`; a vanished target is `edit_target_missing`.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoveEntity {
    pub entity: String,
}

/// Apply a [`RemoveEntity`]: delete exactly the entity's bytes plus the one
/// separating comma, leaving every other byte in place. Removing the only
/// entity leaves `[]`.
pub fn apply_remove_entity(source: &str, edit: &RemoveEntity) -> Result<String> {
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

    let index = SpanIndex::new(source);
    let span_of = |pointer: &str| {
        index.span_of(pointer).ok_or_else(|| {
            EngineError::new(
                codes::EDIT_TARGET_MISSING,
                format!("nothing at {pointer:?} to edit"),
            )
            .path(pointer)
        })
    };

    let (cut_start, cut_end, replacement) = if entities.len() == 1 {
        // The only entity: the array collapses to `[]`.
        let array = span_of("/entities")?;
        (array.start, array.end, "[]")
    } else if entity_index + 1 < entities.len() {
        // Not the last: cut from this entity's start to the next one's,
        // which eats the separating comma and keeps this one's indentation
        // bytes for its successor.
        let this = span_of(&format!("/entities/{entity_index}"))?;
        let next = span_of(&format!("/entities/{}", entity_index + 1))?;
        (this.start, next.start, "")
    } else {
        // The last: cut from the previous entity's end through this one's,
        // eating the comma and the line break before it.
        let previous = span_of(&format!("/entities/{}", entity_index - 1))?;
        let this = span_of(&format!("/entities/{entity_index}"))?;
        (previous.end, this.end, "")
    };

    let mut edited = String::with_capacity(source.len());
    edited.push_str(&source[..cut_start]);
    edited.push_str(replacement);
    edited.push_str(&source[cut_end..]);
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
    component_text_at(kind, fields, "")
}

/// A component as text, at a known indentation.
///
/// One line, exactly as it always was — **unless** a field is block-shaped
/// (see [`is_block`]), in which case the component opens out and that field
/// takes a line per element. The condition matters: every pre-M43 caller
/// writes only scalars and short arrays, so their output is byte-identical
/// and the editor's committed splices did not move.
fn component_text_at(kind: &str, fields: &[(String, Value)], indent: &str) -> String {
    if !fields.iter().any(|(_, v)| is_block(v)) {
        let mut parts = vec![format!("\"type\": {kind:?}")];
        parts.extend(
            fields
                .iter()
                .map(|(k, v)| format!("{k:?}: {}", format_value(v))),
        );
        return format!("{{ {} }}", parts.join(", "));
    }

    let inner = format!("{indent}  ");
    let mut parts = vec![format!("{inner}\"type\": {kind:?}")];
    parts.extend(fields.iter().map(|(k, v)| {
        let text = if is_block(v) {
            format_block(v, &inner)
        } else {
            format_value(v)
        };
        format!("{inner}{k:?}: {text}")
    }));
    format!("{{\n{}\n{indent}}}", parts.join(",\n"))
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
            let comma = if i + 1 < edit.components.len() {
                ","
            } else {
                ""
            };
            out.push_str(&format!(
                "{indent}    {}{comma}\n",
                component_text(kind, fields)
            ));
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
    serde_json::from_str::<Value>(edited)
        .map(|_| ())
        .map_err(|e| {
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
        let edited = set_value(SCENE, "/entities/0/components/2/roughness", &json!(0.3)).unwrap();

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
        let edited = set_value(SCENE, "/entities/0/components/2/roughness", &json!(0.4)).unwrap();
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
        let source =
            "{\n  \"name\": \"s\",\n  \"entities\": [\n    {\n      \"name\": \"A\"\n    }\n  ]\n}";
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
        let edited = insert_key(
            source,
            "/entities/0/components/0",
            "type",
            &json!("Transform"),
        )
        .unwrap();
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
        assert_eq!(
            root["entities"][1]["components"][2]["roughness"],
            json!(0.3)
        );
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
        assert!(edited.contains(r#"]}, { "name": "pyramid","#), "{edited}");
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
    fn add_component_appends_in_array_style_and_touches_nothing_else() {
        let edit = AddComponent {
            entity: "Camera1".into(),
            component: "Transform".into(),
            fields: vec![],
        };
        let edited = apply_add_component(SCENE, &edit).unwrap();
        // Camera1's components array is inline; the new component joins it
        // inline, and only that line changes.
        assert!(
            edited.contains(
                "[ { \"type\": \"Camera\", \"active\": true }, { \"type\": \"Transform\" } ]"
            ),
            "{edited}"
        );
        let changed = SCENE
            .lines()
            .zip(edited.lines())
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(changed, 1);

        // The Sphere's components array is multi-line; a new component gets
        // its own correctly indented line.
        let edit = AddComponent {
            entity: "Sphere".into(),
            component: "RigidBody".into(),
            fields: vec![("body".into(), json!("dynamic"))],
        };
        let edited = apply_add_component(SCENE, &edit).unwrap();
        assert!(
            edited.contains(
                "{ \"type\": \"Material\", \"albedo\": [0.9, 0.1, 0.1], \"roughness\": 0.4 },\n        \
                 { \"type\": \"RigidBody\", \"body\": \"dynamic\" }\n"
            ),
            "{edited}"
        );
        let root: Value = serde_json::from_str(&edited).unwrap();
        assert_eq!(
            root["entities"][0]["components"][3]["body"],
            json!("dynamic")
        );
    }

    #[test]
    fn add_component_creates_a_missing_components_array() {
        let source = r#"{"name":"s","entities":[{"name":"A"}]}"#;
        let edit = AddComponent {
            entity: "A".into(),
            component: "Transform".into(),
            fields: vec![],
        };
        let edited = apply_add_component(source, &edit).unwrap();
        let root: Value = serde_json::from_str(&edited).unwrap();
        assert_eq!(
            root["entities"][0]["components"][0]["type"],
            json!("Transform")
        );
    }

    #[test]
    fn add_component_refuses_a_duplicate_type() {
        let edit = AddComponent {
            entity: "Sphere".into(),
            component: "Material".into(),
            fields: vec![],
        };
        let err = apply_add_component(SCENE, &edit).unwrap_err();
        assert_eq!(err.error, "duplicate_component");
        assert_eq!(
            err.context().unwrap().component.as_deref(),
            Some("Material")
        );
    }

    #[test]
    fn add_component_against_a_vanished_entity_reports_not_corrupts() {
        let edit = AddComponent {
            entity: "Gone".into(),
            component: "Material".into(),
            fields: vec![],
        };
        let err = apply_add_component(SCENE, &edit).unwrap_err();
        assert_eq!(err.error, "edit_target_missing");
    }

    #[test]
    fn remove_component_cuts_exactly_one_line_from_a_multiline_array() {
        // Middle component.
        let edit = RemoveComponent {
            entity: "Sphere".into(),
            component: "Mesh".into(),
        };
        let edited = apply_remove_component(SCENE, &edit).unwrap();
        assert!(!edited.contains("\"type\": \"Mesh\""), "{edited}");
        assert_eq!(SCENE.lines().count(), edited.lines().count() + 1);
        let root: Value = serde_json::from_str(&edited).unwrap();
        assert_eq!(
            root["entities"][0]["components"].as_array().unwrap().len(),
            2
        );
        assert_eq!(
            root["entities"][0]["components"][1]["type"],
            json!("Material")
        );

        // Last component: the predecessor keeps its bytes, loses its comma.
        let edit = RemoveComponent {
            entity: "Sphere".into(),
            component: "Material".into(),
        };
        let edited = apply_remove_component(SCENE, &edit).unwrap();
        assert!(
            edited.contains("{ \"type\": \"Mesh\", \"asset\": \"builtin:sphere\" }\n      ]"),
            "{edited}"
        );
        assert_eq!(SCENE.lines().count(), edited.lines().count() + 1);
    }

    #[test]
    fn remove_only_component_leaves_an_empty_array() {
        let edit = RemoveComponent {
            entity: "Camera1".into(),
            component: "Camera".into(),
        };
        let edited = apply_remove_component(SCENE, &edit).unwrap();
        assert!(
            edited.contains("{ \"name\": \"Camera1\", \"components\": [] }"),
            "{edited}"
        );
        serde_json::from_str::<Value>(&edited).unwrap();
    }

    #[test]
    fn remove_component_addresses_by_name_and_type_not_index() {
        let reordered = {
            let mut root: Value = serde_json::from_str(SCENE).unwrap();
            root["entities"].as_array_mut().unwrap().reverse();
            serde_json::to_string_pretty(&root).unwrap()
        };
        let edit = RemoveComponent {
            entity: "Sphere".into(),
            component: "Material".into(),
        };
        let edited = apply_remove_component(&reordered, &edit).unwrap();
        let root: Value = serde_json::from_str(&edited).unwrap();
        assert_eq!(
            root["entities"][1]["components"].as_array().unwrap().len(),
            2
        );
    }

    #[test]
    fn remove_component_against_a_vanished_target_reports_not_corrupts() {
        let edit = RemoveComponent {
            entity: "Sphere".into(),
            component: "Camera".into(),
        };
        let err = apply_remove_component(SCENE, &edit).unwrap_err();
        assert_eq!(err.error, "edit_target_missing");
        assert_eq!(err.context().unwrap().component.as_deref(), Some("Camera"));
    }

    #[test]
    fn remove_entity_cuts_exactly_one_block() {
        let edit = RemoveEntity {
            entity: "Sphere".into(),
        };
        let edited = apply_remove_entity(SCENE, &edit).unwrap();
        assert!(!edited.contains("Sphere"), "{edited}");
        let root: Value = serde_json::from_str(&edited).unwrap();
        assert_eq!(root["entities"].as_array().unwrap().len(), 1);
        assert_eq!(root["entities"][0]["name"], json!("Camera1"));

        // The last entity: the predecessor keeps its bytes, loses its comma.
        let edit = RemoveEntity {
            entity: "Camera1".into(),
        };
        let edited = apply_remove_entity(SCENE, &edit).unwrap();
        let root: Value = serde_json::from_str(&edited).unwrap();
        assert_eq!(root["entities"].as_array().unwrap().len(), 1);
        assert_eq!(root["entities"][0]["name"], json!("Sphere"));
    }

    #[test]
    fn remove_the_only_entity_leaves_an_empty_array() {
        let edited = apply_remove_entity(
            SCENE,
            &RemoveEntity {
                entity: "Sphere".into(),
            },
        )
        .unwrap();
        let edited = apply_remove_entity(
            &edited,
            &RemoveEntity {
                entity: "Camera1".into(),
            },
        )
        .unwrap();
        let root: Value = serde_json::from_str(&edited).unwrap();
        assert_eq!(root["entities"], json!([]));
    }

    #[test]
    fn remove_entity_against_a_vanished_target_reports_not_corrupts() {
        let err = apply_remove_entity(
            SCENE,
            &RemoveEntity {
                entity: "Gone".into(),
            },
        )
        .unwrap_err();
        assert_eq!(err.error, "edit_target_missing");
        assert_eq!(err.context().unwrap().entity.as_deref(), Some("Gone"));
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
