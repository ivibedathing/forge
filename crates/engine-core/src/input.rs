//! Keyboard input (M11): held-key state and replayable timelines.
//!
//! Input is sampled per fixed step — "is this key held during step N" — on
//! the same integer clock as physics and scripting; there is no event queue.
//! Live keys exist only in the windowed viewer. Everywhere else input is an
//! `*.input.jsonl` timeline: sparse keyframes of the complete held set, so a
//! recorded play session replays deterministically through `--input` and the
//! result is traceable, screenshotable, and diff-renderable like everything
//! else in the engine.

use std::collections::BTreeSet;

use crate::error::closest_match;
use crate::{codes, EngineError};

/// The curated key allowlist: W3C `KeyboardEvent.code` names, which are also
/// winit's `KeyCode` names. Layout-independent physical codes — WASD is WASD
/// on AZERTY hardware too. Keys outside this list do not exist, in timeline
/// files or in the viewer.
pub const KNOWN_KEYS: &[&str] = &[
    "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight",
    "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI",
    "KeyJ", "KeyK", "KeyL", "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR",
    "KeyS", "KeyT", "KeyU", "KeyV", "KeyW", "KeyX", "KeyY", "KeyZ",
    "Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6",
    "Digit7", "Digit8", "Digit9",
    "Space", "Enter",
    "ShiftLeft", "ShiftRight", "ControlLeft", "ControlRight",
];

/// Is `name` in the allowlist?
pub fn is_known_key(name: &str) -> bool {
    KNOWN_KEYS.contains(&name)
}

/// The closest known key to a typo, if any is close enough to suggest.
pub fn closest_key(name: &str) -> Option<String> {
    closest_match(name, KNOWN_KEYS.iter().copied())
}

/// The keys held during one fixed step. Ordered so that serialization and
/// recording are deterministic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputState {
    held: BTreeSet<String>,
}

impl InputState {
    /// True if `key` is held. Callers validate the name; an unknown key is
    /// simply never held here.
    pub fn is_held(&self, key: &str) -> bool {
        self.held.contains(key)
    }

    /// Hold a key. Unknown names are ignored — the viewer feeds raw hardware
    /// codes through this, and keys outside the allowlist don't exist.
    pub fn press(&mut self, key: &str) {
        if is_known_key(key) {
            self.held.insert(key.to_string());
        }
    }

    pub fn release(&mut self, key: &str) {
        self.held.remove(key);
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// The held keys, in stable order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.held.iter().map(String::as_str)
    }

    /// One timeline line for this held set at `step` — the only place the
    /// wire format is written, so recording and parsing cannot drift.
    pub fn timeline_line(&self, step: u64) -> String {
        let held: Vec<&str> = self.keys().collect();
        serde_json::json!({ "step": step, "held": held }).to_string()
    }
}

/// A parsed `*.input.jsonl` timeline: each keyframe's held set takes effect
/// at its step and holds until the next keyframe; before the first keyframe
/// nothing is held.
#[derive(Debug, Clone, Default)]
pub struct InputTimeline {
    /// Sorted by step, strictly increasing — enforced at parse.
    keyframes: Vec<(u64, InputState)>,
    /// What `held_at` returns before the first keyframe.
    empty: InputState,
}

impl InputTimeline {
    /// Parse a timeline, reporting every error at once (the M5 contract).
    /// Errors carry the timeline file and 1-based line number.
    pub fn parse(source: &str, path: &str) -> Result<Self, Vec<EngineError>> {
        let mut errors = Vec::new();
        let mut keyframes: Vec<(u64, InputState)> = Vec::new();

        for (index, line) in source.lines().enumerate() {
            let line_no = index as u32 + 1;
            if line.trim().is_empty() {
                continue;
            }
            let at = |e: EngineError| e.file(path).line(line_no);

            let value: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    errors.push(at(EngineError::new(
                        codes::INPUT_PARSE_ERROR,
                        format!("timeline line is not valid JSON: {e}"),
                    )));
                    continue;
                }
            };
            let Some(object) = value.as_object() else {
                errors.push(at(EngineError::new(
                    codes::INPUT_PARSE_ERROR,
                    "timeline line must be an object: {\"step\": N, \"held\": [..]}",
                )));
                continue;
            };
            for key in object.keys() {
                if key != "step" && key != "held" {
                    errors.push(at(EngineError::new(
                        codes::INPUT_PARSE_ERROR,
                        format!("unknown timeline field {key:?}"),
                    )
                    .field(key)
                    .suggest_from(key, ["step", "held"])));
                }
            }

            let step = match object.get("step").and_then(serde_json::Value::as_u64) {
                Some(step) => step,
                None => {
                    errors.push(at(EngineError::new(
                        codes::INPUT_PARSE_ERROR,
                        "timeline line needs an integer \"step\" >= 0",
                    )
                    .field("step")));
                    continue;
                }
            };
            let Some(held_values) = object.get("held").and_then(serde_json::Value::as_array)
            else {
                errors.push(at(EngineError::new(
                    codes::INPUT_PARSE_ERROR,
                    "timeline line needs a \"held\" array of key names",
                )
                .field("held")));
                continue;
            };

            let mut held = InputState::default();
            for value in held_values {
                let Some(name) = value.as_str() else {
                    errors.push(at(EngineError::new(
                        codes::INPUT_PARSE_ERROR,
                        format!("held entries must be key-name strings, got {value}"),
                    )
                    .field("held")));
                    continue;
                };
                if is_known_key(name) {
                    held.press(name);
                } else {
                    errors.push(at(EngineError::new(
                        codes::UNKNOWN_KEY,
                        format!("{name:?} names no known key"),
                    )
                    .field("held")
                    .suggest_from(name, KNOWN_KEYS.iter().copied())));
                }
            }

            if let Some((last_step, _)) = keyframes.last() {
                if step <= *last_step {
                    errors.push(at(EngineError::new(
                        codes::UNSORTED_INPUT_STEPS,
                        format!(
                            "timeline steps must be strictly increasing: {step} after {last_step}"
                        ),
                    )
                    .field("step")));
                    continue;
                }
            }
            keyframes.push((step, held));
        }

        if errors.is_empty() {
            Ok(Self {
                keyframes,
                empty: InputState::default(),
            })
        } else {
            Err(errors)
        }
    }

    /// Read and parse a timeline file.
    pub fn load(path: &std::path::Path) -> Result<Self, Vec<EngineError>> {
        let display = path.display().to_string();
        let source = std::fs::read_to_string(path).map_err(|e| {
            vec![EngineError::new(
                codes::INPUT_UNREADABLE,
                format!("could not read input timeline {display}: {e}"),
            )
            .file(&display)]
        })?;
        Self::parse(&source, &display)
    }

    /// The held set during `step`: the last keyframe at or before it.
    pub fn held_at(&self, step: u64) -> &InputState {
        match self.keyframes.partition_point(|(s, _)| *s <= step) {
            0 => &self.empty,
            n => &self.keyframes[n - 1].1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframes_hold_until_the_next_and_start_empty() {
        let timeline = InputTimeline::parse(
            "{\"step\": 10, \"held\": [\"ArrowUp\"]}\n\n{\"step\": 20, \"held\": []}\n",
            "t.input.jsonl",
        )
        .unwrap();
        assert!(!timeline.held_at(0).is_held("ArrowUp"));
        assert!(timeline.held_at(10).is_held("ArrowUp"));
        assert!(timeline.held_at(19).is_held("ArrowUp"));
        assert!(!timeline.held_at(20).is_held("ArrowUp"));
        assert!(!timeline.held_at(1_000_000).is_held("ArrowUp"));
    }

    #[test]
    fn every_error_reports_at_once_with_lines_and_suggestions() {
        let source = concat!(
            "{\"step\": 0, \"held\": [\"ArowUp\"]}\n",
            "not json\n",
            "{\"step\": 0, \"held\": [\"Space\"], \"presed\": 1}\n",
        );
        let errors = InputTimeline::parse(source, "t.input.jsonl").unwrap_err();
        let codes: Vec<&str> = errors.iter().map(|e| e.error).collect();
        assert!(codes.contains(&"unknown_key"), "{codes:?}");
        assert!(codes.contains(&"input_parse_error"), "{codes:?}");
        assert!(codes.contains(&"unsorted_input_steps"), "{codes:?}");

        let typo = errors.iter().find(|e| e.error == "unknown_key").unwrap();
        let context = typo.context().unwrap();
        assert_eq!(context.did_you_mean.as_deref(), Some("ArrowUp"));
        assert_eq!(context.line, Some(1));
        assert_eq!(context.file.as_deref(), Some("t.input.jsonl"));

        let extra = errors
            .iter()
            .find(|e| e.message.contains("unknown timeline field"))
            .unwrap();
        assert_eq!(extra.context().unwrap().line, Some(3));
    }

    #[test]
    fn recording_round_trips_through_parse() {
        let mut held = InputState::default();
        held.press("ArrowUp");
        held.press("ArrowLeft");
        held.press("NotAKey"); // hardware codes outside the allowlist vanish
        let line = held.timeline_line(42);
        let timeline = InputTimeline::parse(&line, "t.input.jsonl").unwrap();
        assert_eq!(timeline.held_at(42), &held);
        assert!(!timeline.held_at(42).is_held("NotAKey"));
    }

    #[test]
    fn known_keys_cover_the_arrows_and_wasd() {
        for key in ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "KeyW", "KeyA", "KeyS", "KeyD", "Space"] {
            assert!(is_known_key(key), "{key} should be known");
        }
        assert!(!is_known_key("Up"));
        assert_eq!(closest_key("Spce").as_deref(), Some("Space"));
    }
}
