//! Property-style corpora for the validator (M5 §10).
//!
//! Two properties replace the old serde-message-parsing tests:
//!
//! 1. **Agreement**: the schema-driven walk and serde agree — a scene with no
//!    shape errors parses, and a scene with shape errors does not. Drift in
//!    either direction is exactly the `scene_parse_desync` bug class.
//! 2. **Robustness**: garbage input never panics the validator, and every
//!    error it returns still carries file + line.

use engine_core::validate::validate_source;
use engine_core::SceneFile;

/// Codes that make a component or scene unparseable by serde. Range
/// violations and semantic errors (duplicates, surplus cameras, missing
/// assets) deliberately do not — serde parses those scenes fine.
const SHAPE_CODES: &[&str] = &[
    "invalid_json",
    "scene_root_not_object",
    "missing_field",
    "unknown_field",
    "invalid_field_type",
    "entity_not_object",
    "missing_entity_name",
    "component_not_object",
    "component_missing_type",
    "unknown_component",
    "unknown_shape",
    "unknown_body_kind",
];

/// Valid and broken scenes, mixed. Builtin assets only, so validity depends
/// on nothing outside the source text.
const AGREEMENT_CORPUS: &[&str] = &[
    // Valid.
    r#"{"name":"s","entities":[]}"#,
    r#"{"name":"s","entities":[{"name":"A"}]}"#,
    r#"{"name":"s","entities":[{"name":"A","components":[]}]}"#,
    r#"{"name":"s","entities":[
        {"name":"Cam","components":[{"type":"Camera","fov":90.0,"active":true}]},
        {"name":"Cube","components":[
            {"type":"Transform","position":[0,1,0],"rotation":[0,45,0],"scale":[2,2,2]},
            {"type":"Mesh","asset":"builtin:cube"},
            {"type":"Material","albedo":[1,0,0],"metallic":1.0,"roughness":0.1}
        ]},
        {"name":"Sun","components":[{"type":"DirectionalLight","color":[1,1,1],"intensity":2.5}]},
        {"name":"Fill","components":[{"type":"AmbientLight"}]}
    ]}"#,
    // Valid but warned (warnings must not affect parseability).
    r#"{"name":"s","entities":[{"name":"A","components":[{"type":"Material"}]}]}"#,
    r#"{"name":"s","entities":[{"name":"A","components":[
        {"type":"Transform","scale":[0,0,0]},{"type":"Mesh","asset":"builtin:cube"}]}]}"#,
    // Range/semantic errors only — serde still parses these.
    r#"{"name":"s","entities":[{"name":"A","components":[
        {"type":"Mesh","asset":"builtin:cube"},
        {"type":"Material","albedo":[9,9,9],"metallic":-1.0}]}]}"#,
    r#"{"name":"s","entities":[{"name":"A","components":[{"type":"Camera","fov":0.0,"near":-1.0}]}]}"#,
    r#"{"name":"s","entities":[{"name":"A"},{"name":"A"}]}"#,
    r#"{"name":"s","entities":[
        {"name":"A","components":[{"type":"Camera","active":true}]},
        {"name":"B","components":[{"type":"Camera","active":true}]}
    ]}"#,
    // Physics (M8): valid, semantically broken (serde parses), shape-broken.
    r#"{"name":"s","physics":{"gravity":[0.0,-9.81,0.0],"timestep_hz":60},"entities":[
        {"name":"A","components":[{"type":"Transform"},{"type":"RigidBody","body":"dynamic"},
         {"type":"Collider","shape":"capsule","radius":0.3,"half_height":0.5}]}]}"#,
    r#"{"name":"s","entities":[{"name":"A","components":[{"type":"Transform"},{"type":"RigidBody","body":"dynamic"}]}]}"#,
    r#"{"name":"s","entities":[{"name":"A","components":[{"type":"Transform"},
        {"type":"Collider","shape":"sphere","radius":-2.0,"half_extents":[1,1,1]}]}]}"#,
    r#"{"name":"s","entities":[{"name":"A","components":[{"type":"RigidBody","body":"dynmaic"}]}]}"#,
    r#"{"name":"s","entities":[{"name":"A","components":[{"type":"Collider","shape":"donut"}]}]}"#,
    // Shape-broken — serde must reject every one of these.
    r#"{"entities":[]}"#,
    r#"{"name":"s"}"#,
    r#"{"name":42,"entities":[]}"#,
    r#"{"name":"s","entities":[42]}"#,
    r#"{"name":"s","entities":[{"components":[]}]}"#,
    r#"{"name":"s","entities":[{"name":"A","components":[7]}]}"#,
    r#"{"name":"s","entities":[{"name":"A","components":[{}]}]}"#,
    r#"{"name":"s","entities":[{"name":"A","components":[{"type":"Rigidbody"}]}]}"#,
    r#"{"name":"s","entities":[{"name":"A","components":[{"type":"Mesh"}]}]}"#,
    r#"{"name":"s","entities":[{"name":"A","components":[{"type":"Mesh","asset":42}]}]}"#,
    r#"{"name":"s","entities":[{"name":"A","components":[{"type":"Transform","postion":[0,0,0]}]}]}"#,
    r#"{"name":"s","entities":[{"name":"A","components":[{"type":"Transform","position":[0,1]}]}]}"#,
    r#"{"name":"s","entities":[{"name":"A","components":[{"type":"Transform","position":[0,"x",2]}]}]}"#,
    r#"{"name":"s","entities":[{"name":"A","components":[{"type":"Camera","active":"yes"}]}]}"#,
];

#[test]
fn walk_and_serde_agree_on_every_corpus_scene() {
    for (i, source) in AGREEMENT_CORPUS.iter().enumerate() {
        let shape_errors: Vec<&str> = validate_source(source, "corpus.json")
            .into_iter()
            .map(|e| e.error)
            .filter(|code| SHAPE_CODES.contains(code))
            .collect();
        let parses = serde_json::from_str::<SceneFile>(source).is_ok();

        match (shape_errors.is_empty(), parses) {
            (true, false) => panic!(
                "corpus[{i}]: walk found no shape errors but serde rejects — \
                 the walk is missing a check:\n{source}"
            ),
            (false, true) => panic!(
                "corpus[{i}]: walk reports {shape_errors:?} but serde parses — \
                 the walk is stricter than the loader:\n{source}"
            ),
            _ => {}
        }
    }
}

#[test]
fn desync_never_fires_on_the_corpus() {
    for source in AGREEMENT_CORPUS {
        let errors = validate_source(source, "corpus.json");
        assert!(
            errors.iter().all(|e| e.error != "scene_parse_desync"),
            "walk/serde desync on: {source}"
        );
    }
}

#[test]
fn garbage_input_never_panics_and_errors_stay_located() {
    let deep_open = "[".repeat(400);
    let deep_closed = format!("{}{}", "[".repeat(300), "]".repeat(300));
    let big_number = r#"{"name":"s","entities":[],"x":1e999}"#;

    let corpus: Vec<String> = vec![
        String::new(),                                     // empty file
        " \n\t ".to_string(),                              // whitespace only
        "\u{feff}{\"name\":\"s\",\"entities\":[]}".to_string(), // BOM
        "null".to_string(),
        "[]".to_string(),
        "\"scene\"".to_string(),
        "42".to_string(),
        "{".to_string(),
        deep_open,
        deep_closed,
        big_number.to_string(),
        r#"{"name":null,"entities":null}"#.to_string(),
        r#"{"name":{"name":"s"},"entities":{"0":{}}}"#.to_string(),
        r#"{"name":"s","entities":[{"name":["A"],"components":{"type":"Mesh"}}]}"#.to_string(),
        r#"{"name":"s","entities":[{"name":"Ünïcödé — 名前","components":[{"type":"Mesh","asset":"builtin:cube"}]}]}"#
            .to_string(),
        r#"{"name":"s","entities":[{"name":"A","components":[{"type":["Mesh"],"asset":true}]}]}"#
            .to_string(),
        r#"{"name":"s","entities":[{"name":"A","components":[{"type":"Camera","fov":true,"near":[],"far":{},"active":0}]}]}"#
            .to_string(),
    ];

    for source in &corpus {
        // The property is "returns, with located errors" — a panic fails the
        // whole test binary, which is the point.
        let errors = validate_source(source, "garbage.json");
        let valid_unicode_scene = source.contains("Ünïcödé");
        assert!(
            valid_unicode_scene || !errors.is_empty(),
            "garbage validated cleanly: {source:?}"
        );
        for error in &errors {
            let context = error
                .context()
                .unwrap_or_else(|| panic!("{} has no context on {source:?}", error.error));
            assert!(context.file.is_some(), "{} lost its file", error.error);
            assert!(
                context.line.is_some(),
                "{} carries no line on {source:?}: {}",
                error.error,
                error.to_json()
            );
        }
    }
}

/// The golden kitchen sink: one scene exercising every scene-validation
/// error code reachable from a single parseable file, its full NDJSON output
/// pinned byte-for-byte — line numbers, paths, suggestions, severities. Any
/// wire-format drift fails loudly here before an agent meets it.
#[test]
fn kitchen_sink_output_is_pinned() {
    let source = include_str!("kitchen_sink.json");
    let errors = validate_source(source, "kitchen_sink.json");

    let actual: Vec<String> = errors.iter().map(|e| e.to_json()).collect();
    let expected: Vec<&str> = include_str!("kitchen_sink.expected.ndjson")
        .lines()
        .filter(|l| !l.is_empty())
        .collect();

    assert_eq!(
        actual,
        expected,
        "wire format drifted; if the change is deliberate, re-pin with:\n\
         actual output:\n{}",
        actual.join("\n")
    );
}
