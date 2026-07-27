//! End-to-end tests for the CLI contract (`docs/cli-contract.md`).
//!
//! These pin the stream and exit-code behavior an agent scripts against:
//! stdout carries exactly one JSON object on success and nothing on failure;
//! stderr is NDJSON, one complete object per line; exit codes split 0/1/2 by
//! who is at fault. No GPU is needed — every test drives validate/build-side
//! paths, which is exactly where the contract lives.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn engine() -> Command {
    Command::new(env!("CARGO_BIN_EXE_engine"))
}

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(relative)
}

/// Write a scene into a per-test temp dir and return its path.
fn scene_file(test: &str, contents: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("engine-cli-{}-{test}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("scene.json");
    std::fs::write(&path, contents).unwrap();
    path
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout must be UTF-8")
}

fn stderr_lines(output: &Output) -> Vec<serde_json::Value> {
    let stderr = String::from_utf8(output.stderr.clone()).expect("stderr must be UTF-8");
    stderr
        .lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|e| {
                panic!("stderr line is not a JSON object ({e}): {line:?}")
            })
        })
        .collect()
}

fn codes_of(lines: &[serde_json::Value]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l["error"].as_str().unwrap_or("<missing>").to_string())
        .collect()
}

const VALID: &str = r#"{"name":"ok","entities":[
    {"name":"Cam","components":[{"type":"Camera","active":true}]},
    {"name":"Cube","components":[{"type":"Mesh","asset":"builtin:cube"}]}
]}"#;

const BROKEN: &str = r#"{"name":"bad","entities":[
    {"name":"A","components":[{"type":"Meterial"}]},
    {"name":"A"}
]}"#;

/// Valid, but with a Material on a mesh-less entity: exactly one warning.
const WARNED: &str = r#"{"name":"warned","entities":[
    {"name":"Cam","components":[{"type":"Camera","active":true}]},
    {"name":"Oops","components":[{"type":"Material"}]}
]}"#;

#[test]
fn validate_success_prints_one_json_object_and_nothing_on_stderr() {
    let scene = scene_file("valid", VALID);
    let output = engine().arg("validate").arg(&scene).output().unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty(), "stderr must be silent on success");

    let result: serde_json::Value =
        serde_json::from_str(stdout_of(&output).trim()).expect("stdout must be one JSON object");
    assert_eq!(result["valid"], true);
    assert_eq!(result["files"], 1);
    assert_eq!(result["errors"], 0);
    assert_eq!(result["warnings"], 0);
}

#[test]
fn validate_failure_exits_one_with_empty_stdout_and_ndjson_stderr() {
    let scene = scene_file("broken", BROKEN);
    let output = engine().arg("validate").arg(&scene).output().unwrap();

    assert_eq!(output.status.code(), Some(1), "input files at fault → 1");
    assert!(
        output.stdout.is_empty(),
        "stdout must be empty on failure, got: {}",
        stdout_of(&output)
    );

    let lines = stderr_lines(&output);
    let codes = codes_of(&lines);
    assert!(codes.contains(&"unknown_component".to_string()), "{codes:?}");
    assert!(codes.contains(&"duplicate_entity_name".to_string()), "{codes:?}");
    assert_eq!(
        codes.last().map(String::as_str),
        Some("validation_failed"),
        "the summary error is the final line"
    );
}

#[test]
fn validate_takes_multiple_files_and_aggregates() {
    let valid = scene_file("multi-valid", VALID);
    let broken = scene_file("multi-broken", BROKEN);
    let output = engine()
        .arg("validate")
        .arg(&valid)
        .arg(&broken)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let lines = stderr_lines(&output);
    // Both of the broken file's errors, then the aggregate summary.
    let summary = lines.last().unwrap();
    assert_eq!(summary["error"], "validation_failed");
    assert!(
        summary["message"].as_str().unwrap().contains("2 file(s)"),
        "{summary}"
    );

    // Diagnostics carry the file they belong to.
    assert!(lines
        .iter()
        .any(|l| l["file"].as_str().is_some_and(|f| f.contains("multi-broken"))));
}

#[test]
fn warnings_ride_stderr_but_do_not_fail_the_run() {
    let scene = scene_file("warned", WARNED);
    let output = engine().arg("validate").arg(&scene).output().unwrap();

    assert_eq!(output.status.code(), Some(0), "warnings alone never fail");

    let lines = stderr_lines(&output);
    assert_eq!(codes_of(&lines), ["unused_material"]);
    assert_eq!(lines[0]["severity"], "warning");

    let result: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
    assert_eq!(result["valid"], true);
    assert_eq!(result["warnings"], 1);
}

#[test]
fn strict_promotes_warnings_to_exit_one() {
    let scene = scene_file("strict", WARNED);
    let output = engine()
        .arg("validate")
        .arg("--strict")
        .arg(&scene)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let codes = codes_of(&stderr_lines(&output));
    assert!(codes.contains(&"unused_material".to_string()));
    assert!(codes.contains(&"validation_failed".to_string()));
}

#[test]
fn unreadable_scene_is_a_structured_error() {
    let output = engine()
        .arg("validate")
        .arg("does/not/exist.json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let codes = codes_of(&stderr_lines(&output));
    assert!(codes.contains(&"scene_unreadable".to_string()), "{codes:?}");
}

#[test]
fn screenshot_reports_every_validation_error_like_validate_does() {
    // M5 §7: which command you ran never changes what you learn. No GPU is
    // touched — the scene fails validation first.
    let scene = scene_file("screenshot-broken", BROKEN);
    let out = scene.with_file_name("out.png");
    let output = engine()
        .arg("screenshot")
        .arg(&scene)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());

    let codes = codes_of(&stderr_lines(&output));
    assert!(codes.contains(&"unknown_component".to_string()), "{codes:?}");
    assert!(
        codes.contains(&"duplicate_entity_name".to_string()),
        "screenshot must not drip-feed one error per run: {codes:?}"
    );
}

#[test]
fn unknown_subcommand_is_structured_json_exit_two() {
    let output = engine().arg("screenshoot").output().unwrap();

    assert_eq!(output.status.code(), Some(2), "invocation at fault → 2");
    assert!(output.stdout.is_empty());

    let lines = stderr_lines(&output);
    assert_eq!(lines.len(), 1, "exactly one error object");
    assert_eq!(lines[0]["error"], "invalid_invocation");
    assert_eq!(
        lines[0]["did_you_mean"], "screenshot",
        "clap's suggestion must survive the re-rendering: {}",
        lines[0]
    );
}

#[test]
fn unknown_flag_is_structured_json_exit_two() {
    let scene = scene_file("flag", VALID);
    let output = engine()
        .arg("validate")
        .arg("--strcit")
        .arg(&scene)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let lines = stderr_lines(&output);
    assert_eq!(lines[0]["error"], "invalid_invocation");
}

#[test]
fn help_and_version_stay_human_readable() {
    let help = engine().arg("--help").output().unwrap();
    assert_eq!(help.status.code(), Some(0));
    assert!(
        stdout_of(&help).contains("Agent-native 3D engine"),
        "--help is documentation, not an error"
    );

    let version = engine().arg("--version").output().unwrap();
    assert_eq!(version.status.code(), Some(0));
}

#[test]
fn panic_hook_keeps_a_crash_inside_the_protocol() {
    let output = engine().arg("debug-panic").output().unwrap();

    assert_eq!(output.status.code(), Some(2), "internal fault → 2");
    let lines = stderr_lines(&output);
    assert_eq!(lines.len(), 1, "one JSON line, even for a panic");
    assert_eq!(lines[0]["error"], "internal_panic");
    assert!(lines[0]["file"].is_string(), "panic location survives");
}

#[test]
fn panic_hook_embeds_a_backtrace_without_breaking_ndjson() {
    let output = engine()
        .arg("debug-panic")
        .env("RUST_BACKTRACE", "1")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let lines = stderr_lines(&output);
    assert_eq!(lines.len(), 1, "the backtrace must ride escaped inside the JSON");
    assert!(lines[0]["message"].as_str().unwrap().contains("backtrace"));
}

#[test]
fn m5_broken_fixture_reports_all_planted_errors_in_one_run() {
    let output = engine()
        .arg("validate")
        .arg(repo_path("examples/scenes/verify/m5_broken.json"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let lines = stderr_lines(&output);
    assert!(lines.len() >= 7, "expected all errors at once, got {lines:?}");

    let codes = codes_of(&lines);
    for expected in [
        "unknown_component",
        "value_out_of_range",
        "asset_not_found",
        "unknown_field",
        "multiple_active_cameras",
    ] {
        assert!(codes.contains(&expected.to_string()), "missing {expected}: {codes:?}");
    }
    for line in &lines {
        if line["error"] != "validation_failed" {
            assert!(line["line"].is_u64(), "diagnostic without a line: {line}");
        }
    }
}

#[test]
fn m4_lighting_fixture_still_validates_clean() {
    // The positive twin from milestone-verification-scenes.md.
    let output = engine()
        .arg("validate")
        .arg(repo_path("examples/scenes/verify/m4_lighting.json"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stderr.is_empty(),
        "no errors and no warnings expected: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
