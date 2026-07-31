//! Keyboard and mouse input (M11, M28): held state and replayable timelines.
//!
//! Input is sampled per fixed step — "is this key held during step N" — on
//! the same integer clock as physics and scripting; there is no event queue.
//! Live input exists only in the windowed viewer. Everywhere else input is an
//! `*.input.jsonl` timeline: sparse keyframes of the complete held set, so a
//! recorded play session replays deterministically through `--input` and the
//! result is traceable, screenshotable, and diff-renderable like everything
//! else in the engine.
//!
//! The mouse (M28, `designs/mouse-input-design.md`) rides the same timeline:
//! its buttons are names in the same `held` set, and its cursor is a
//! `"cursor": [x, y]` fraction of the frame rather than a pixel, because a
//! timeline outlives the window it was recorded in.

use std::collections::BTreeSet;

use glam::{Mat4, Vec2, Vec3};

use crate::components::Camera;
use crate::error::closest_match;
use crate::{codes, EngineError};

/// The curated key allowlist: W3C `KeyboardEvent.code` names, which are also
/// winit's `KeyCode` names. Layout-independent physical codes — WASD is WASD
/// on AZERTY hardware too. Keys outside this list do not exist, in timeline
/// files or in the viewer.
pub const KNOWN_KEYS: &[&str] = &[
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "KeyA",
    "KeyB",
    "KeyC",
    "KeyD",
    "KeyE",
    "KeyF",
    "KeyG",
    "KeyH",
    "KeyI",
    "KeyJ",
    "KeyK",
    "KeyL",
    "KeyM",
    "KeyN",
    "KeyO",
    "KeyP",
    "KeyQ",
    "KeyR",
    "KeyS",
    "KeyT",
    "KeyU",
    "KeyV",
    "KeyW",
    "KeyX",
    "KeyY",
    "KeyZ",
    "Digit0",
    "Digit1",
    "Digit2",
    "Digit3",
    "Digit4",
    "Digit5",
    "Digit6",
    "Digit7",
    "Digit8",
    "Digit9",
    "Space",
    "Enter",
    "Escape",
    "ShiftLeft",
    "ShiftRight",
    "ControlLeft",
    "ControlRight",
];

/// The mouse buttons (M28). They live in the same held set as the keys — a
/// timeline keyframe is one complete snapshot of what the player is doing —
/// but in their own allowlist, so `world.key` and `world.mouse` can each
/// reject the other kind with a suggestion instead of reading `false`
/// forever.
pub const KNOWN_BUTTONS: &[&str] = &["MouseLeft", "MouseRight", "MouseMiddle"];

/// Is `name` in the key allowlist?
pub fn is_known_key(name: &str) -> bool {
    KNOWN_KEYS.contains(&name)
}

/// Is `name` in the mouse-button allowlist?
pub fn is_known_button(name: &str) -> bool {
    KNOWN_BUTTONS.contains(&name)
}

/// Is `name` anything the held set can carry — a key or a button? This is
/// what the timeline parser and the viewer check; the split between the two
/// kinds happens at the script query, not in the file.
pub fn is_known_input(name: &str) -> bool {
    is_known_key(name) || is_known_button(name)
}

/// The closest known key to a typo, if any is close enough to suggest.
pub fn closest_key(name: &str) -> Option<String> {
    closest_match(name, KNOWN_KEYS.iter().copied())
}

/// The closest known button to a typo, if any is close enough to suggest.
pub fn closest_button(name: &str) -> Option<String> {
    closest_match(name, KNOWN_BUTTONS.iter().copied())
}

/// The closest known key *or* button — what a timeline error suggests, since
/// a `held` entry may be either.
pub fn closest_input(name: &str) -> Option<String> {
    closest_match(
        name,
        KNOWN_KEYS
            .iter()
            .copied()
            .chain(KNOWN_BUTTONS.iter().copied()),
    )
}

/// Where the cursor sits when nothing says otherwise: the centre of the
/// frame. "No input means no keys held" (M11) extends to "and the cursor at
/// the centre" — a fixed, documented point, so a run without `--input` is as
/// reproducible as one with it.
pub const CURSOR_CENTRE: Vec2 = Vec2::splat(0.5);

/// How finely a recorded cursor is quantized: three decimals, about one pixel
/// across a 960-wide frame. Without it every tremor of a hand on a mouse is a
/// keyframe; with it a still hand records nothing. The quantized value is
/// what the file says and therefore what replays, so this is a format
/// decision rather than a display one.
/// Written as a scale rather than as a step of 0.001, because rounding
/// through a multiply and a divide by 1000 lands on the f32 that *prints* as
/// three decimals; `(v / 0.001).round() * 0.001` does not, and writes
/// `0.41300002` into the file.
pub const CURSOR_SCALE: f32 = 1000.0;

/// The keys and buttons held during one fixed step, plus where the cursor
/// was. Ordered so that serialization and recording are deterministic.
///
/// Not `Eq`: the cursor is a pair of floats. `PartialEq` is what the recorder
/// compares, and it compares quantized values.
#[derive(Debug, Clone, PartialEq)]
pub struct InputState {
    held: BTreeSet<String>,
    cursor: Vec2,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            held: BTreeSet::new(),
            cursor: CURSOR_CENTRE,
        }
    }
}

impl InputState {
    /// True if `key` (or button) is held. Callers validate the name; an
    /// unknown one is simply never held here.
    pub fn is_held(&self, key: &str) -> bool {
        self.held.contains(key)
    }

    /// Hold a key or button. Unknown names are ignored — the viewer feeds raw
    /// hardware codes through this, and names outside the allowlists don't
    /// exist.
    pub fn press(&mut self, key: &str) {
        if is_known_input(key) {
            self.held.insert(key.to_string());
        }
    }

    pub fn release(&mut self, key: &str) {
        self.held.remove(key);
    }

    /// Where the cursor is, as a fraction of the frame with the origin at the
    /// top-left corner — the same corner HUD pixels are measured from.
    pub fn cursor(&self) -> Vec2 {
        self.cursor
    }

    /// Move the cursor, clamped to the frame. Off-window positions read as
    /// the nearest edge: a pointer that left the window still points
    /// somewhere, and a ray through `1.4` is a ray into nothing.
    pub fn set_cursor(&mut self, cursor: Vec2) {
        let cursor = if cursor.is_finite() {
            cursor
        } else {
            CURSOR_CENTRE
        };
        self.cursor = cursor.clamp(Vec2::ZERO, Vec2::ONE);
    }

    /// True when nothing is held. The cursor is not part of this: a keyframe
    /// exists to say what is *held*, and the recorder's "an initial empty set
    /// is implicit" rule would otherwise depend on where a hand happened to
    /// leave the mouse.
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// The held keys and buttons, in stable order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.held.iter().map(String::as_str)
    }

    /// This state with its cursor snapped to [`CURSOR_SCALE`] — what the
    /// recorder compares and writes, so the file and the replay agree.
    pub fn quantized(&self) -> Self {
        let snap = |v: f32| (v * CURSOR_SCALE).round() / CURSOR_SCALE;
        Self {
            held: self.held.clone(),
            cursor: Vec2::new(snap(self.cursor.x), snap(self.cursor.y)),
        }
    }

    /// One timeline line for this held set at `step` — the only place the
    /// wire format is written, so recording and parsing cannot drift.
    ///
    /// The cursor is always written. An absent one parses as the centre, and
    /// a recording that omitted it while the pointer sat elsewhere would
    /// replay somewhere the session never pointed.
    pub fn timeline_line(&self, step: u64) -> String {
        let held: Vec<&str> = self.keys().collect();
        let cursor = self.quantized().cursor;
        // Through the shortest-f32 text path, so `0.62` writes as `0.62` and
        // not as the f64-widened `0.6200000047683716` — the same rule the
        // formatter applies to every number this engine writes.
        let number = crate::formatter::number_from_f32;
        serde_json::json!({
            "step": step,
            "held": held,
            "cursor": [number(cursor.x), number(cursor.y)],
        })
        .to_string()
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
                if key != "step" && key != "held" && key != "cursor" {
                    errors.push(at(EngineError::new(
                        codes::INPUT_PARSE_ERROR,
                        format!("unknown timeline field {key:?}"),
                    )
                    .field(key)
                    .suggest_from(key, ["step", "held", "cursor"])));
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
            let Some(held_values) = object.get("held").and_then(serde_json::Value::as_array) else {
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
                if is_known_input(name) {
                    held.press(name);
                } else {
                    errors.push(at(EngineError::new(
                        codes::UNKNOWN_KEY,
                        format!("{name:?} names no known key or mouse button"),
                    )
                    .field("held")
                    .suggest_from(
                        name,
                        KNOWN_KEYS
                            .iter()
                            .copied()
                            .chain(KNOWN_BUTTONS.iter().copied()),
                    )));
                }
            }

            // The cursor (M28). Absent is the centre of the frame, not a
            // carry-over from the previous keyframe: a keyframe is a complete
            // snapshot, and a carry-over rule makes line 40 unreadable
            // without line 0.
            match object.get("cursor") {
                None => {}
                Some(serde_json::Value::Array(pair)) if pair.len() == 2 => {
                    let parsed: Vec<Option<f64>> =
                        pair.iter().map(serde_json::Value::as_f64).collect();
                    match (parsed[0], parsed[1]) {
                        (Some(x), Some(y)) => {
                            held.set_cursor(Vec2::new(x as f32, y as f32));
                        }
                        _ => errors.push(at(EngineError::new(
                            codes::INPUT_PARSE_ERROR,
                            format!(
                                "cursor components must be numbers, got {}",
                                object["cursor"]
                            ),
                        )
                        .field("cursor"))),
                    }
                }
                Some(other) => errors.push(at(EngineError::new(
                    codes::INPUT_PARSE_ERROR,
                    format!(
                        "cursor must be two numbers as a fraction of the frame, \
                         origin top-left: [x, y] in [0, 1] — got {other}"
                    ),
                )
                .field("cursor"))),
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

/// The frame a cursor is measured in, and the camera it is measured through
/// (M28).
///
/// A cursor is a fraction of the frame; turning it into a direction needs the
/// frame's aspect and the camera's field of view. Commands that render know
/// both; the ones that render nothing use [`Viewport::DEFAULT`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    /// The `--camera` the run points through, or `None` for the scene's
    /// active camera — the same selection the render makes, so what a script
    /// aims at is what the picture shows.
    pub camera: Option<String>,
}

impl Viewport {
    /// What `simulate` and `raycast` use: they produce no image and so have
    /// no size of their own. A documented constant rather than an accident,
    /// because a mouse-driven run *is* a function of its aspect ratio.
    pub const DEFAULT: Self = Self {
        width: 960,
        height: 540,
        camera: None,
    };

    pub fn new(width: u32, height: u32, camera: Option<&str>) -> Self {
        Self {
            width,
            height,
            camera: camera.map(str::to_string),
        }
    }

    pub fn aspect(&self) -> f32 {
        self.width.max(1) as f32 / self.height.max(1) as f32
    }
}

impl Default for Viewport {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Where the pointer points during one step: the cursor, the frame it was
/// measured in, and the world-space ray through it.
///
/// Resolved by the *caller* of `ScriptHost::step` — the same code that knows
/// which camera it is about to render through — so the script host holds no
/// camera-selection policy and the viewer and the headless path provably
/// agree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pointer {
    /// `[0, 1]` across the frame, origin top-left.
    pub cursor: Vec2,
    /// The frame in pixels: what turns the cursor into HUD coordinates.
    pub viewport: [u32; 2],
    /// The ray through the cursor, or `None` when the scene has no camera to
    /// look through — in which case asking where the pointer is in the world
    /// is a runtime error rather than a made-up direction.
    pub ray: Option<(Vec3, Vec3)>,
}

impl Default for Pointer {
    fn default() -> Self {
        Self {
            cursor: CURSOR_CENTRE,
            viewport: [Viewport::DEFAULT.width, Viewport::DEFAULT.height],
            ray: None,
        }
    }
}

impl Pointer {
    /// How far along a ray that never meets the plane `ground` answers. A
    /// camera tipped at the horizon degrades to "very far away" rather than
    /// to a NaN that lands in a `Transform`.
    pub const MAX_GROUND_DISTANCE: f32 = 500.0;

    /// The pointer for this step. `camera` is the camera and its model
    /// matrix, as `Scene::camera` returns them; `None` leaves the ray unset.
    ///
    /// The direction is the inverse of `scene_renderer::view_projection`, and
    /// `engine-render` pins the two against each other — engine-core cannot
    /// depend on the renderer, so an agreement test is what keeps two
    /// spellings of one transform honest.
    pub fn resolve(
        input: &InputState,
        viewport: &Viewport,
        camera: Option<(Camera, Mat4)>,
    ) -> Self {
        let cursor = input.cursor();
        let ray = camera.map(|(camera, model)| {
            // Cursor (top-left origin, y down) to normalized device space
            // (centre origin, y up).
            let ndc = Vec2::new(cursor.x * 2.0 - 1.0, 1.0 - cursor.y * 2.0);
            let tan_half = (camera.fov.to_radians() * 0.5).tan();
            let view_dir = Vec3::new(
                ndc.x * viewport.aspect() * tan_half,
                ndc.y * tan_half,
                // The camera looks down its own local −Z, the convention
                // every camera and light in this engine follows.
                -1.0,
            );
            let direction = model.transform_vector3(view_dir).normalize_or_zero();
            (model.w_axis.truncate(), direction)
        });
        Self {
            cursor,
            viewport: [viewport.width.max(1), viewport.height.max(1)],
            ray,
        }
    }

    /// Where the ray meets the horizontal plane at height `y`, or `None` when
    /// there is no camera. A ray running parallel to the plane or away from
    /// it returns the point [`MAX_GROUND_DISTANCE`](Self::MAX_GROUND_DISTANCE)
    /// along its horizontal projection.
    pub fn ground(&self, y: f32) -> Option<Vec3> {
        let (origin, direction) = self.ray?;
        let toward = (y - origin.y) / direction.y;
        if direction.y.abs() > 1e-6 && toward > 0.0 && toward <= Self::MAX_GROUND_DISTANCE {
            return Some(origin + direction * toward);
        }
        let flat = Vec3::new(direction.x, 0.0, direction.z).normalize_or_zero();
        Some(Vec3::new(origin.x, y, origin.z) + flat * Self::MAX_GROUND_DISTANCE)
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
    fn a_timeline_without_a_cursor_reads_as_the_centre_of_the_frame() {
        // Every timeline committed before M28 says nothing about a cursor and
        // must keep meaning exactly what it meant.
        let timeline =
            InputTimeline::parse("{\"step\": 0, \"held\": [\"KeyW\"]}\n", "t.input.jsonl").unwrap();
        assert_eq!(timeline.held_at(0).cursor(), CURSOR_CENTRE);
    }

    #[test]
    fn buttons_ride_the_held_set_and_the_cursor_clamps_to_the_frame() {
        let timeline = InputTimeline::parse(
            concat!(
                "{\"step\": 0, \"held\": [\"MouseLeft\", \"KeyW\"], \"cursor\": [0.25, 0.75]}\n",
                "{\"step\": 5, \"held\": [], \"cursor\": [1.4, -0.2]}\n",
            ),
            "t.input.jsonl",
        )
        .unwrap();
        assert!(timeline.held_at(0).is_held("MouseLeft"));
        assert!(timeline.held_at(0).is_held("KeyW"));
        assert_eq!(timeline.held_at(0).cursor(), Vec2::new(0.25, 0.75));
        // Off-window is the nearest edge, not a ray into nothing.
        assert_eq!(timeline.held_at(5).cursor(), Vec2::new(1.0, 0.0));
    }

    #[test]
    fn a_misspelled_button_is_an_error_that_names_the_button_it_meant() {
        let errors =
            InputTimeline::parse("{\"step\": 0, \"held\": [\"MouseLeftt\"]}", "t.input.jsonl")
                .unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error, "unknown_key");
        assert_eq!(
            errors[0].context().unwrap().did_you_mean.as_deref(),
            Some("MouseLeft")
        );
    }

    #[test]
    fn a_malformed_cursor_is_reported_rather_than_silently_centred() {
        let errors = InputTimeline::parse(
            "{\"step\": 0, \"held\": [], \"cursor\": [0.5]}",
            "t.input.jsonl",
        )
        .unwrap_err();
        assert_eq!(errors[0].error, "input_parse_error");
        assert_eq!(
            errors[0].context().unwrap().field.as_deref(),
            Some("cursor")
        );
    }

    #[test]
    fn a_recorded_cursor_round_trips_through_its_own_quantum() {
        let mut held = InputState::default();
        held.press("MouseLeft");
        // A position with more precision than the format keeps.
        held.set_cursor(Vec2::new(0.6204999, 0.4131));
        let line = held.timeline_line(42);
        // Three decimals, written without f64 noise.
        assert!(line.contains("\"cursor\":[0.62,0.413]"), "{line}");
        let timeline = InputTimeline::parse(&line, "t.input.jsonl").unwrap();
        assert_eq!(timeline.held_at(42), &held.quantized());
    }

    #[test]
    fn the_pointer_ray_leaves_the_camera_through_the_cursor() {
        use crate::components::Camera;

        let camera = Camera {
            fov: 90.0,
            ..Camera::default()
        };
        // Sitting at the origin, looking down −Z (the identity model matrix is
        // exactly that convention).
        let viewport = Viewport::new(200, 100, None);
        let mut input = InputState::default();

        let centre = Pointer::resolve(&input, &viewport, Some((camera, Mat4::IDENTITY)));
        let (origin, direction) = centre.ray.unwrap();
        assert_eq!(origin, Vec3::ZERO);
        assert!((direction - Vec3::NEG_Z).length() < 1e-6, "{direction}");

        // Top-left of the frame is up and to the left: at a 90° vertical fov
        // the corner direction is (−aspect, +1, −1) normalized.
        input.set_cursor(Vec2::ZERO);
        let corner = Pointer::resolve(&input, &viewport, Some((camera, Mat4::IDENTITY)));
        let (_, direction) = corner.ray.unwrap();
        let expected = Vec3::new(-2.0, 1.0, -1.0).normalize();
        assert!((direction - expected).length() < 1e-6, "{direction}");
    }

    #[test]
    fn the_ground_point_is_where_the_ray_crosses_the_plane() {
        use crate::components::Camera;

        let camera = Camera::default();
        // 10 m up, looking straight down: the centre of the frame is directly
        // below the camera whatever the fov.
        let model = Mat4::from_rotation_translation(
            glam::Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
            Vec3::new(3.0, 10.0, -4.0),
        );
        let pointer = Pointer::resolve(
            &InputState::default(),
            &Viewport::DEFAULT,
            Some((camera, model)),
        );
        let ground = pointer.ground(0.0).unwrap();
        assert!(
            (ground - Vec3::new(3.0, 0.0, -4.0)).length() < 1e-4,
            "{ground}"
        );

        // A ray that never reaches the plane degrades to "far away", not NaN.
        let level = Mat4::from_translation(Vec3::new(0.0, 2.0, 0.0));
        let flat = Pointer::resolve(
            &InputState::default(),
            &Viewport::DEFAULT,
            Some((camera, level)),
        );
        let far = flat.ground(0.0).unwrap();
        assert!(far.is_finite());
        assert!((far.z + Pointer::MAX_GROUND_DISTANCE).abs() < 1e-3, "{far}");
    }

    #[test]
    fn a_scene_with_no_camera_has_no_ray_at_all() {
        let pointer = Pointer::resolve(&InputState::default(), &Viewport::DEFAULT, None);
        assert!(pointer.ray.is_none());
        assert!(pointer.ground(0.0).is_none());
        assert_eq!(pointer.cursor, CURSOR_CENTRE);
    }

    #[test]
    fn known_keys_cover_the_arrows_and_wasd() {
        for key in [
            "ArrowUp",
            "ArrowDown",
            "ArrowLeft",
            "ArrowRight",
            "KeyW",
            "KeyA",
            "KeyS",
            "KeyD",
            "Space",
        ] {
            assert!(is_known_key(key), "{key} should be known");
        }
        assert!(!is_known_key("Up"));
        assert_eq!(closest_key("Spce").as_deref(), Some("Space"));
    }
}
