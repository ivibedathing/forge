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

// ── diff-render ────────────────────────────────────────────────────────

#[test]
fn missing_baseline_is_baseline_not_found() {
    let scene = scene_file("diff-nobase", VALID);
    let output = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg("does/not/exist.png")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(codes_of(&stderr_lines(&output)), ["baseline_not_found"]);
}

#[test]
fn undecodable_baseline_is_baseline_invalid() {
    let scene = scene_file("diff-badbase", VALID);
    let fake = scene.with_file_name("baseline.png");
    std::fs::write(&fake, "this is not a png").unwrap();

    let output = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&fake)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(codes_of(&stderr_lines(&output)), ["baseline_invalid"]);
}

#[test]
fn broken_scene_reports_all_errors_before_any_render() {
    let scene = scene_file("diff-broken", BROKEN);
    let baseline = scene.with_file_name("baseline.png");
    // A real 1x1 PNG so the baseline stage passes without a GPU.
    image::RgbaImage::from_raw(1, 1, vec![0, 0, 0, 255])
        .unwrap()
        .save(&baseline)
        .unwrap();

    let output = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let codes = codes_of(&stderr_lines(&output));
    assert!(codes.contains(&"unknown_component".to_string()), "{codes:?}");
    assert!(codes.contains(&"duplicate_entity_name".to_string()), "{codes:?}");
}

/// The §7 same-machine determinism promise as an executable claim, plus the
/// full agent loop: bless → diff (pass) → edit → diff (fail with located
/// damage). Skips cleanly when this machine has no GPU, the same policy as
/// headless_render.rs.
#[test]
fn bless_then_diff_round_trip() {
    const SCENE: &str = r#"{"name":"diffscene","entities":[
        {"name":"Cam","components":[
            {"type":"Transform","position":[0.0,1.5,5.0]},
            {"type":"Camera","active":true}]},
        {"name":"Cube","components":[
            {"type":"Transform","position":[0.0,1.0,0.0]},
            {"type":"Mesh","asset":"builtin:cube"},
            {"type":"Material","albedo":[0.8,0.1,0.1]}]}
    ]}"#;

    let scene = scene_file("diff-roundtrip", SCENE);
    let baseline = scene.with_file_name("baseline.png");

    // Bless via screenshot — the identical offscreen path, so a screenshot
    // is a valid baseline by construction.
    let bless = engine()
        .arg("screenshot")
        .arg(&scene)
        .arg("--out")
        .arg(&baseline)
        .arg("--width")
        .arg("96")
        .arg("--height")
        .arg("64")
        .output()
        .unwrap();
    if !bless.status.success() {
        let stderr = String::from_utf8_lossy(&bless.stderr);
        assert!(
            stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
            "screenshot failed for a non-GPU reason: {stderr}"
        );
        eprintln!("skipping: no usable GPU on this machine");
        return;
    }

    // Self-diff at bit-exact defaults must pass with zero difference.
    let diff_out = scene.with_file_name("selfdiff.png");
    let self_diff = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .arg("--out")
        .arg(&diff_out)
        .output()
        .unwrap();

    assert_eq!(
        self_diff.status.code(),
        Some(0),
        "self-diff must be deterministic: {}",
        String::from_utf8_lossy(&self_diff.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_str(stdout_of(&self_diff).trim()).unwrap();
    assert_eq!(report["pass"], true);
    assert_eq!(report["diff_pixels"], 0);
    assert_eq!(report["width"], 96, "render size comes from the baseline");
    assert_eq!(report["height"], 64);
    assert!(report["diff_bounds"].is_null(), "no bounds when nothing differs");
    assert!(report["adapter"].is_string());
    assert!(diff_out.exists(), "diff image is written on pass too");

    // A real change must fail loudly, with the damage located.
    let changed = scene.with_file_name("changed.json");
    std::fs::write(&changed, SCENE.replace("[0.8,0.1,0.1]", "[0.1,0.1,0.9]")).unwrap();

    let fail_out = scene.with_file_name("faildiff.png");
    let changed_diff = engine()
        .arg("diff-render")
        .arg(&changed)
        .arg(&baseline)
        .arg("--out")
        .arg(&fail_out)
        .output()
        .unwrap();

    assert_eq!(changed_diff.status.code(), Some(1));
    assert_eq!(
        codes_of(&stderr_lines(&changed_diff)),
        ["render_mismatch"],
        "one structured error on stderr"
    );

    // The report still prints on failure — the documented exception.
    let report: serde_json::Value =
        serde_json::from_str(stdout_of(&changed_diff).trim()).unwrap();
    assert_eq!(report["pass"], false);
    assert!(report["diff_pixels"].as_u64().unwrap() > 0);
    let bounds = &report["diff_bounds"];
    assert!(bounds["max_x"].as_u64().unwrap() >= bounds["min_x"].as_u64().unwrap());

    // The diff PNG classifies: red violations inside the bounds, none outside.
    let diff_png = image::open(&fail_out).unwrap().to_rgba8();
    let (min_x, min_y) = (
        bounds["min_x"].as_u64().unwrap() as u32,
        bounds["min_y"].as_u64().unwrap() as u32,
    );
    let (max_x, max_y) = (
        bounds["max_x"].as_u64().unwrap() as u32,
        bounds["max_y"].as_u64().unwrap() as u32,
    );
    let mut red_inside = 0u32;
    for (x, y, pixel) in diff_png.enumerate_pixels() {
        let inside = x >= min_x && x <= max_x && y >= min_y && y <= max_y;
        if pixel.0 == [255, 0, 0, 255] {
            assert!(inside, "red pixel outside diff_bounds at ({x}, {y})");
            red_inside += 1;
        }
    }
    assert!(red_inside > 0, "the recolored cube must show as red");
}

// ── physics (M8) ───────────────────────────────────────────────────────

/// Determinism is the contract: the same scene and step count produce
/// byte-identical traces, run to run and against the committed golden.
/// A golden mismatch on a rapier upgrade is a breaking change to review,
/// never noise to regenerate blindly.
#[test]
fn simulation_traces_are_deterministic_and_match_the_golden() {
    let scene = repo_path("examples/scenes/verify/m8_drop.json");
    let trace = |name: &str| {
        let path = std::env::temp_dir().join(format!(
            "engine-m8-{}-{name}.jsonl",
            std::process::id()
        ));
        let output = engine()
            .arg("simulate")
            .arg(&scene)
            .arg("--steps")
            .arg("300")
            .arg("--trace")
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        std::fs::read(&path).unwrap()
    };

    let first = trace("a");
    let second = trace("b");
    assert_eq!(first, second, "twice-run traces must be byte-identical");

    let golden =
        std::fs::read(repo_path("examples/scenes/verify/baselines/m8_drop.trace.jsonl")).unwrap();
    assert_eq!(
        first, golden,
        "trace drifted from the committed golden — if a rapier upgrade \
         caused this, review the diff as a breaking change"
    );
}

/// Baking is a representation checkpoint, not a bit-perfect solver
/// snapshot: quantizing to Euler-degree f32 text at the bake boundary (and
/// dropping solver caches, which are deliberately disposable) shifts the
/// resumed trajectory by float ulps. The pinned property is agreement
/// within solver noise, not byte equality.
#[test]
fn bake_round_trip_agrees_within_solver_noise() {
    let scene = repo_path("examples/scenes/verify/m8_drop.json");
    let dir = std::env::temp_dir().join(format!("engine-m8-bake-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let simulate = |input: &std::path::Path, steps: &str, out: &std::path::Path| {
        let output = engine()
            .arg("simulate")
            .arg(input)
            .arg("--steps")
            .arg(steps)
            .arg("--bake")
            .arg(out)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
    };

    let mid = dir.join("mid.json");
    let resumed = dir.join("resumed.json");
    let straight = dir.join("straight.json");
    simulate(&scene, "150", &mid);
    simulate(&mid, "150", &resumed);
    simulate(&scene, "300", &straight);

    let position = |path: &std::path::Path, entity: &str| -> Vec<f64> {
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let e = root["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == entity)
            .unwrap();
        e["components"][0]["position"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect()
    };

    for entity in ["DropCube", "BouncyBall"] {
        let a = position(&resumed, entity);
        let b = position(&straight, entity);
        for axis in 0..3 {
            assert!(
                (a[axis] - b[axis]).abs() < 1e-4,
                "{entity} axis {axis}: resumed {a:?} vs straight {b:?}"
            );
        }
    }

    // And a bake must always be a valid scene file.
    let output = engine().arg("validate").arg(&resumed).output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn raycast_reports_hits_and_misses_as_json() {
    let scene = repo_path("examples/scenes/verify/m8_drop.json");
    let output = engine()
        .arg("raycast")
        .arg(&scene)
        .arg("--from")
        .arg("0,10,0")
        .arg("--dir")
        .arg("0,-1,0")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let result: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
    // At step 0 the cube is airborne at x=0, so the ray hits it.
    assert_eq!(result["hit"]["entity"], "DropCube", "{result}");

    let output = engine()
        .arg("raycast")
        .arg(&scene)
        .arg("--from")
        .arg("50,10,0")
        .arg("--dir")
        .arg("0,-1,0")
        .output()
        .unwrap();
    let result: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
    assert!(result["hit"].is_null(), "{result}");
}

#[test]
fn physics_validation_errors_surface_end_to_end() {
    let scene = scene_file(
        "physics-broken",
        r#"{"name":"pb","entities":[
            {"name":"Faller","components":[
                {"type":"Transform"},{"type":"RigidBody","body":"dynmaic"}
            ]},
            {"name":"Egg","components":[
                {"type":"Transform","scale":[1.0,2.0,1.0]},
                {"type":"Collider","shape":"cubiod","radius":0.5}
            ]}
        ]}"#,
    );
    let output = engine().arg("validate").arg(&scene).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let codes = codes_of(&stderr_lines(&output));
    for expected in ["unknown_body_kind", "unknown_shape"] {
        assert!(codes.contains(&expected.to_string()), "missing {expected}: {codes:?}");
    }
    let lines = stderr_lines(&output);
    let body = lines.iter().find(|l| l["error"] == "unknown_body_kind").unwrap();
    assert_eq!(body["did_you_mean"], "dynamic");
}

// ── animation (M9) ─────────────────────────────────────────────────────

/// The strictest determinism check in the verification doc: the same scene
/// at the same --time renders byte-identical PNGs, and the loop period
/// lands exactly back on the t=0 pose.
#[test]
fn animated_screenshots_are_time_deterministic() {
    let scene = repo_path("examples/scenes/verify/m9_spin.json");
    let dir = std::env::temp_dir().join(format!("engine-m9-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let shot = |name: &str, time: &str| -> Option<Vec<u8>> {
        let out = dir.join(format!("{name}.png"));
        let output = engine()
            .arg("screenshot")
            .arg(&scene)
            .arg("--time")
            .arg(time)
            .arg("--out")
            .arg(&out)
            .arg("--width")
            .arg("128")
            .arg("--height")
            .arg("72")
            .output()
            .unwrap();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
                "screenshot failed for a non-GPU reason: {stderr}"
            );
            return None;
        }
        Some(std::fs::read(&out).unwrap())
    };

    let Some(t0) = shot("t0", "0.0") else {
        eprintln!("skipping: no usable GPU on this machine");
        return;
    };
    let quarter = shot("t025", "0.25").unwrap();
    let period = shot("t2", "2.0").unwrap();

    assert_ne!(t0, quarter, "the cube must visibly move by t=0.25");
    assert_eq!(t0, period, "t=2.0 is the loop period; pose and pixels must match t=0");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn list_animations_reports_the_spin_clip() {
    let output = engine()
        .arg("list-animations")
        .arg(repo_path("examples/scenes/verify/m9_spin.json"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
    assert_eq!(report["clips"][0]["name"], "spin");
    assert_eq!(report["clips"][0]["duration"], 2.0);
    assert_eq!(report["clips"][0]["tracks"][0]["entity"], "SpinCube");
    assert_eq!(report["clips"][0]["tracks"][0]["property"], "Transform.rotation");
}

#[test]
fn clip_files_validate_directly() {
    let output = engine()
        .arg("validate")
        .arg(repo_path("examples/scenes/verify/animations/spin.anim.json"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
}

/// Scene-context animation errors, end to end: a typo'd target entity, two
/// clips fighting over one property, and a clip driving a dynamic body.
#[test]
fn animation_scene_errors_fire_with_suggestions() {
    let dir = std::env::temp_dir().join(format!("engine-m9-errors-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("animations")).unwrap();
    std::fs::write(
        dir.join("animations/spin.anim.json"),
        r#"{"name":"spin","tracks":[{"entity":"SpinCube","property":"Transform.rotation",
            "keys":[{"time":0.0,"value":[0,0,0]},{"time":2.0,"value":[0,360,0]}]}]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("animations/typo.anim.json"),
        r#"{"name":"typo","tracks":[{"entity":"SpinCub","property":"Transform.rotation",
            "keys":[{"time":0.0,"value":[0,0,0]},{"time":1.0,"value":[0,90,0]}]}]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("scene.json"),
        r#"{"name":"err","entities":[
            {"name":"SpinCube","components":[
                {"type":"Transform"},{"type":"Mesh","asset":"builtin:cube"},
                {"type":"RigidBody","body":"dynamic"},
                {"type":"Collider","shape":"cuboid","half_extents":[0.5,0.5,0.5]},
                {"type":"AnimationPlayer","clip":"animations/spin.anim.json"}]},
            {"name":"Rival","components":[
                {"type":"AnimationPlayer","clip":"animations/spin.anim.json"}]},
            {"name":"Typo","components":[
                {"type":"AnimationPlayer","clip":"animations/typo.anim.json"}]},
            {"name":"Cam","components":[{"type":"Camera","active":true}]}
        ]}"#,
    )
    .unwrap();

    let output = engine().arg("validate").arg(dir.join("scene.json")).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let lines = stderr_lines(&output);
    let codes = codes_of(&lines);
    for expected in ["unknown_entity", "conflicting_tracks", "animation_on_dynamic_body"] {
        assert!(codes.contains(&expected.to_string()), "missing {expected}: {codes:?}");
    }
    let typo = lines.iter().find(|l| l["error"] == "unknown_entity").unwrap();
    assert_eq!(typo["did_you_mean"], "SpinCube", "{typo}");
    assert!(
        typo["file"].as_str().unwrap().contains("typo.anim.json"),
        "unknown_entity points at the clip file: {typo}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ── scripting (M10) ────────────────────────────────────────────────────

/// Scripted motion is deterministic and trace-observable: twice-run traces
/// are byte-identical with scripts running, and the kinematic elevator's
/// crossing into the static sensor appears as a contact event.
#[test]
fn scripted_simulation_is_deterministic_and_sensor_observable() {
    let scene = repo_path("examples/scenes/verify/m10_script.json");
    let trace = |name: &str| {
        let path = std::env::temp_dir()
            .join(format!("engine-m10-{}-{name}.jsonl", std::process::id()));
        let output = engine()
            .arg("simulate")
            .arg(&scene)
            .arg("--steps")
            .arg("150")
            .arg("--trace")
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        std::fs::read_to_string(&path).unwrap()
    };

    let first = trace("a");
    assert_eq!(first, trace("b"), "determinism must hold with scripts running");
    assert!(
        first.contains(r#""contact":["Elevator","TopSensor"],"started":true"#),
        "the sensor crossing must be trace-visible"
    );
}

/// A bake after a scripted run captures script-driven state (the kinematic
/// elevator's risen Transform) and is itself a valid scene file.
#[test]
fn scripted_bake_is_a_valid_scene_with_the_moved_state() {
    let scene = repo_path("examples/scenes/verify/m10_script.json");
    // Bake next to the scene so relative script/asset paths keep resolving.
    let out = repo_path(&format!(
        "examples/scenes/verify/.m10-bake-test-{}.json",
        std::process::id()
    ));
    let output = engine()
        .arg("simulate")
        .arg(&scene)
        .arg("--steps")
        .arg("120")
        .arg("--bake")
        .arg(&out)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");

    let validate = engine().arg("validate").arg(&out).output().unwrap();
    assert_eq!(validate.status.code(), Some(0));

    let baked: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let y = baked["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "Elevator")
        .unwrap()["components"][0]["position"][1]
        .as_f64()
        .unwrap();
    assert!((y - 2.25).abs() < 1e-3, "elevator baked at {y}, expected ~2.25");
    std::fs::remove_file(&out).ok();
}

/// A script runtime error surfaces as structured JSON naming the script
/// file and line, exit 1 — never a panic, never a silent no-op.
#[test]
fn script_runtime_errors_are_structured() {
    let dir = std::env::temp_dir().join(format!("engine-m10-err-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(
        dir.join("scripts/bad.rhai"),
        "fn step(world, step) { world.position(\"Ghost\"); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("scene.json"),
        r#"{"name":"e","entities":[
            {"name":"Box","components":[{"type":"Transform"},
                {"type":"Script","source":"scripts/bad.rhai"}]},
            {"name":"Cam","components":[{"type":"Camera","active":true}]}
        ]}"#,
    )
    .unwrap();

    let output = engine()
        .arg("simulate")
        .arg(dir.join("scene.json"))
        .arg("--steps")
        .arg("3")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let lines = stderr_lines(&output);
    let error = lines.iter().find(|l| l["error"] == "script_runtime_error").unwrap();
    assert!(error["file"].as_str().unwrap().ends_with("bad.rhai"));
    assert!(error["line"].is_u64());
    assert_eq!(error["entity"], "Box");
    std::fs::remove_dir_all(&dir).ok();
}

/// A script that does not compile fails `engine validate` with the script's
/// own file and line.
#[test]
fn script_parse_errors_fail_validation() {
    let dir = std::env::temp_dir().join(format!("engine-m10-parse-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(dir.join("scripts/bad.rhai"), "fn step(world step) {}\n").unwrap();
    std::fs::write(
        dir.join("scene.json"),
        r#"{"name":"e","entities":[
            {"name":"Box","components":[{"type":"Transform"},
                {"type":"Script","source":"scripts/bad.rhai"}]}
        ]}"#,
    )
    .unwrap();

    let output = engine().arg("validate").arg(dir.join("scene.json")).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let codes = codes_of(&stderr_lines(&output));
    assert!(codes.contains(&"script_parse_error".to_string()), "{codes:?}");
    std::fs::remove_dir_all(&dir).ok();
}

// ── Input (M11) ────────────────────────────────────────────────────────

/// A minimal input-driven scene: the script moves Mover +1 in x on every
/// step where ArrowUp is held.
fn input_fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("engine-m11-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(
        dir.join("scripts/move.rhai"),
        r#"fn step(world, step) {
            if world.key("ArrowUp") {
                let p = world.position("Mover");
                world.set_position("Mover", p[0] + 1.0, p[1], p[2]);
            }
        }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("scene.json"),
        r#"{"name":"m11","entities":[
            {"name":"Mover","components":[
                {"type":"Transform","position":[0.0,0.0,0.0]},
                {"type":"Mesh","asset":"builtin:cube"},
                {"type":"Script","source":"scripts/move.rhai"}
            ]},
            {"name":"Cam","components":[{"type":"Camera","active":true}]}
        ]}"#,
    )
    .unwrap();
    dir
}

fn baked_position(baked: &Path, entity: &str) -> Vec<f64> {
    let scene: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(baked).unwrap()).unwrap();
    scene["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == entity)
        .and_then(|e| {
            e["components"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["type"] == "Transform")
        })
        .and_then(|t| t["position"].as_array())
        .map(|p| p.iter().map(|v| v.as_f64().unwrap()).collect())
        .unwrap()
}

/// `--input` reaches scripts, keyframes hold until replaced, and replaying
/// the same timeline twice is byte-identical — the record-once,
/// regression-test-forever contract.
#[test]
fn input_replay_reaches_scripts_and_is_deterministic() {
    let dir = input_fixture("replay");
    // Held during steps 3..6 → exactly 3 moves.
    std::fs::write(
        dir.join("lap.input.jsonl"),
        "{\"step\": 3, \"held\": [\"ArrowUp\"]}\n{\"step\": 6, \"held\": []}\n",
    )
    .unwrap();

    let bake = |out: &str| {
        let path = dir.join(out);
        let output = engine()
            .arg("simulate")
            .arg(dir.join("scene.json"))
            .args(["--steps", "10"])
            .arg("--input")
            .arg(dir.join("lap.input.jsonl"))
            .arg("--bake")
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        path
    };
    let first = bake("a.json");
    let second = bake("b.json");

    assert_eq!(baked_position(&first, "Mover"), vec![3.0, 0.0, 0.0]);
    assert_eq!(
        std::fs::read(&first).unwrap(),
        std::fs::read(&second).unwrap(),
        "same timeline, same bytes"
    );

    // No timeline means no keys held: nothing moves, and the bake preserves
    // the file byte-for-byte.
    let output = engine()
        .arg("simulate")
        .arg(dir.join("scene.json"))
        .args(["--steps", "10"])
        .arg("--bake")
        .arg(dir.join("none.json"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        std::fs::read(dir.join("none.json")).unwrap(),
        std::fs::read(dir.join("scene.json")).unwrap(),
        "no input, no change"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A broken timeline reports every error at once — unknown keys with
/// did_you_mean, junk lines, non-increasing steps — and exits 1 with an
/// empty stdout.
#[test]
fn a_broken_input_timeline_reports_every_error_at_once() {
    let dir = input_fixture("badinput");
    std::fs::write(
        dir.join("bad.input.jsonl"),
        concat!(
            "{\"step\": 0, \"held\": [\"ArowUp\"]}\n",
            "junk\n",
            "{\"step\": 0, \"held\": [\"Space\"]}\n",
        ),
    )
    .unwrap();

    let output = engine()
        .arg("simulate")
        .arg(dir.join("scene.json"))
        .args(["--steps", "1"])
        .arg("--input")
        .arg(dir.join("bad.input.jsonl"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout_of(&output).is_empty(), "stdout must be silent on failure");

    let lines = stderr_lines(&output);
    let codes = codes_of(&lines);
    assert!(codes.contains(&"unknown_key".to_string()), "{codes:?}");
    assert!(codes.contains(&"input_parse_error".to_string()), "{codes:?}");
    assert!(codes.contains(&"unsorted_input_steps".to_string()), "{codes:?}");

    let typo = lines.iter().find(|l| l["error"] == "unknown_key").unwrap();
    assert_eq!(typo["did_you_mean"], "ArrowUp");
    assert_eq!(typo["line"], 1);
    std::fs::remove_dir_all(&dir).ok();
}

/// The committed demo: replaying the recorded session drives the physical
/// car (dynamic box chassis on four raycast-suspension Wheels; the script
/// is only the driver) three laps around the track and parks it on the
/// start line. This is the M11/M12 verification fixture — interactive
/// gameplay, verified headlessly from text files alone.
#[test]
fn the_committed_lap_timeline_drives_the_car_around_the_track() {
    let scene = repo_path("examples/scenes/car_track.json");
    let timeline = repo_path("examples/scenes/car_track_lap.input.jsonl");
    let dir = std::env::temp_dir().join(format!("engine-m11-lap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // The baked copy lands in a temp dir; that's fine because the test only
    // reads its JSON, never loads its (scene-relative) assets.
    let bake_at = |steps: &str, out: &str| {
        let path = dir.join(out);
        let output = engine()
            .arg("simulate")
            .arg(&scene)
            .args(["--steps", steps])
            .arg("--input")
            .arg(&timeline)
            .arg("--bake")
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        path
    };

    // Mid-drive: far side of the circuit (the north straight, z ≈ -9).
    let mid = baked_position(&bake_at("480", "mid.json"), "Car");
    assert!(mid[2] < -5.0, "mid-drive the car is on the far straight: {mid:?}");

    // After three laps and the braking phase: parked on the start line.
    let end = baked_position(&bake_at("2880", "end.json"), "Car");
    let (dx, dz) = (end[0], end[2] - 9.0);
    let distance = (dx * dx + dz * dz).sqrt();
    assert!(distance < 1.5, "the drive must park on the start line: {end:?}");
    std::fs::remove_dir_all(&dir).ok();
}
