//! The error-code registry.
//!
//! Every code an `EngineError` can carry is declared here, once, alongside its
//! exit-code class and a one-line description. Call sites name codes through
//! these consts rather than string literals, so a rename or a near-duplicate
//! (`asset_not_found` vs. `asset_missing`) is a compile error or a review-time
//! diff in exactly one file, never a silent wire change.
//!
//! **Codes are API.** An agent's scripts branch on them. Adding a code is a
//! feature; renaming or removing one is a breaking change to every consumer in
//! the wild. `docs/error-codes.md` is the human-facing copy of this table, and
//! a repo-contract test asserts the two match one-to-one in both directions.

/// Who is at fault, and therefore which exit code the process ends with.
///
/// The split an agent branches on without parsing anything: 1 means "edit your
/// files and retry," 2 means "fix your command or your machine."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitClass {
    /// The input files are at fault (exit code 1).
    Input,
    /// The invocation or environment is at fault (exit code 2).
    Environment,
}

impl ExitClass {
    pub fn code(self) -> i32 {
        match self {
            ExitClass::Input => 1,
            ExitClass::Environment => 2,
        }
    }
}

/// One registered error code.
pub struct ErrorCode {
    pub code: &'static str,
    pub class: ExitClass,
    pub description: &'static str,
}

/// Declares the consts and the registry table from one list, so a code cannot
/// exist without a class and a description or vice versa.
macro_rules! registry {
    ($($konst:ident = $code:literal, $class:ident, $description:literal;)*) => {
        $(pub const $konst: &str = $code;)*

        /// Every registered code. `docs/error-codes.md` mirrors this table.
        pub const REGISTRY: &[ErrorCode] = &[
            $(ErrorCode {
                code: $code,
                class: ExitClass::$class,
                description: $description,
            },)*
        ];
    };
}

registry! {
    // ── Scene structure ────────────────────────────────────────────────
    INVALID_JSON = "invalid_json", Input,
        "the file is not syntactically valid JSON";
    SCENE_ROOT_NOT_OBJECT = "scene_root_not_object", Input,
        "the top-level JSON value is not an object";
    MISSING_FIELD = "missing_field", Input,
        "a required field is absent";
    UNKNOWN_FIELD = "unknown_field", Input,
        "a field name is not part of the schema";
    INVALID_FIELD_TYPE = "invalid_field_type", Input,
        "a field holds a value of the wrong JSON type";
    ENTITY_NOT_OBJECT = "entity_not_object", Input,
        "an entry in \"entities\" is not a JSON object";
    MISSING_ENTITY_NAME = "missing_entity_name", Input,
        "an entity has no \"name\" field";
    EMPTY_ENTITY_NAME = "empty_entity_name", Input,
        "an entity's \"name\" is the empty string";
    DUPLICATE_ENTITY_NAME = "duplicate_entity_name", Input,
        "two entities share a name";
    COMPONENT_NOT_OBJECT = "component_not_object", Input,
        "an entry in \"components\" is not a JSON object";
    COMPONENT_MISSING_TYPE = "component_missing_type", Input,
        "a component has no \"type\" field";
    UNKNOWN_COMPONENT = "unknown_component", Input,
        "a component's \"type\" names no known component";
    DUPLICATE_COMPONENT = "duplicate_component", Input,
        "the same component type appears twice on one entity";
    VALUE_OUT_OF_RANGE = "value_out_of_range", Input,
        "a numeric field is outside its documented range";
    MULTIPLE_ACTIVE_CAMERAS = "multiple_active_cameras", Input,
        "more than one camera is marked active";
    MULTIPLE_DIRECTIONAL_LIGHTS = "multiple_directional_lights", Input,
        "more than one DirectionalLight in the scene";
    MULTIPLE_AMBIENT_LIGHTS = "multiple_ambient_lights", Input,
        "more than one AmbientLight in the scene";
    VALIDATION_FAILED = "validation_failed", Input,
        "summary error after per-diagnostic reports; see the preceding lines";

    // ── Warnings (severity: "warning"; do not affect the exit code
    //    unless promoted by --strict) ────────────────────────────────────
    UNUSED_MATERIAL = "unused_material", Input,
        "warning: a Material on an entity with no Mesh does nothing";
    ZERO_SCALE = "zero_scale", Input,
        "warning: a Transform.scale axis of 0 renders invisibly or degenerate";

    // ── Scene semantics at command time ───────────────────────────────
    SCENE_UNREADABLE = "scene_unreadable", Input,
        "the scene file could not be read";
    SCENE_PARSE_DESYNC = "scene_parse_desync", Environment,
        "internal bug: the scene passed validation but failed to parse";
    ENTITY_NOT_FOUND = "entity_not_found", Input,
        "no entity has the requested name";
    MISSING_COMPONENT = "missing_component", Input,
        "the entity exists but lacks the required component";
    NO_ACTIVE_CAMERA = "no_active_camera", Input,
        "no camera is marked active and none was named";

    // ── Assets ─────────────────────────────────────────────────────────
    ASSET_NOT_FOUND = "asset_not_found", Input,
        "an asset reference names no builtin and no existing file";
    ASSET_UNSUPPORTED = "asset_unsupported", Input,
        "the asset's format or feature is not one the engine reads";
    ASSET_PATH_NOT_RELATIVE = "asset_path_not_relative", Input,
        "asset paths must be relative to the scene file";
    ASSET_LOAD_FAILED = "asset_load_failed", Input,
        "the asset file exists but could not be parsed";

    // ── engine build ───────────────────────────────────────────────────
    COMPILE_ERROR = "compile_error", Input,
        "a rustc error, re-emitted with its file/line";
    COMPILE_WARNING = "compile_warning", Input,
        "warning: a rustc warning, re-emitted with its file/line";
    BUILD_FAILED = "build_failed", Input,
        "summary error when cargo reports compile errors";
    CARGO_ERROR = "cargo_error", Environment,
        "cargo itself failed with no compiler diagnostics";
    CARGO_NOT_FOUND = "cargo_not_found", Environment,
        "the cargo executable could not be run";

    // ── Rendering & GPU environment ────────────────────────────────────
    NO_GPU_ADAPTER = "no_gpu_adapter", Environment,
        "no usable GPU adapter was found";
    DEVICE_REQUEST_FAILED = "device_request_failed", Environment,
        "the GPU adapter refused a device";
    GPU_POLL_FAILED = "gpu_poll_failed", Environment,
        "waiting on the GPU failed";
    READBACK_FAILED = "readback_failed", Environment,
        "copying the rendered image back from the GPU failed";
    SURFACE_CREATION_FAILED = "surface_creation_failed", Environment,
        "a window surface could not be created";
    SURFACE_UNSUPPORTED = "surface_unsupported", Environment,
        "the surface configuration is not supported here";
    SURFACE_VALIDATION_ERROR = "surface_validation_error", Environment,
        "the surface rejected a frame";
    WINDOW_CREATION_FAILED = "window_creation_failed", Environment,
        "a window could not be created";
    EVENT_LOOP_CREATION_FAILED = "event_loop_creation_failed", Environment,
        "an event loop could not be created";
    EVENT_LOOP_FAILED = "event_loop_failed", Environment,
        "the event loop failed while running";
    PNG_WRITE_FAILED = "png_write_failed", Environment,
        "the output PNG could not be written";

    // ── Process-level ──────────────────────────────────────────────────
    INVALID_INVOCATION = "invalid_invocation", Environment,
        "the command line itself could not be parsed";
    INTERNAL_PANIC = "internal_panic", Environment,
        "internal bug: the process panicked; the protocol still held";
    OUTPUT_SERIALIZATION_FAILED = "output_serialization_failed", Environment,
        "internal bug: a result object failed to serialize";
    ERROR_SERIALIZATION_FAILED = "error_serialization_failed", Environment,
        "internal bug: an error object failed to serialize";
}

/// The exit code for an error code. Unregistered codes land on 2: an
/// unregistered code is itself an engine bug, which is class Environment.
pub fn exit_code(code: &str) -> i32 {
    REGISTRY
        .iter()
        .find(|entry| entry.code == code)
        .map(|entry| entry.class.code())
        .unwrap_or(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for entry in REGISTRY {
            assert!(seen.insert(entry.code), "{} registered twice", entry.code);
        }
    }

    #[test]
    fn codes_are_snake_case() {
        for entry in REGISTRY {
            assert!(
                entry
                    .code
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_'),
                "{} is not snake_case",
                entry.code
            );
        }
    }

    #[test]
    fn exit_codes_follow_the_class() {
        assert_eq!(exit_code(VALIDATION_FAILED), 1);
        assert_eq!(exit_code(NO_GPU_ADAPTER), 2);
        assert_eq!(exit_code("some_future_unregistered_code"), 2);
    }
}
