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

/// A script-driven emission rate is scene state: it bakes under the same
/// change-based rule as a velocity, and the baked file revalidates. The
/// particles themselves stay out of the file — they are disposable
/// simulation state, so only the authored `rate` field moves.
#[test]
fn a_script_driven_particle_rate_bakes_and_revalidates() {
    let dir = std::env::temp_dir().join(format!("engine-m13-rate-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(
        dir.join("scripts/gate.rhai"),
        // Emission answers to gameplay: off, then hard on at step 30.
        "fn step(world, step) {\n\
             world.set_particle_rate(\"Puff\", if step >= 30 { 90.0 } else { 0.0 });\n\
         }\n",
    )
    .unwrap();
    let scene_path = dir.join("scene.json");
    std::fs::write(
        &scene_path,
        r#"{"name":"rate","entities":[
            {"name":"Puff","components":[
                {"type":"Transform", "rotation": [90.0, 0.0, 0.0]},
                {"type":"ParticleEmitter","rate":12.0,"seed":3},
                {"type":"Script","source":"scripts/gate.rhai"}
            ]},
            {"name":"Cam","components":[
                {"type":"Transform","position":[0.0,1.0,5.0]},
                {"type":"Camera","active":true}]}
        ]}"#,
    )
    .unwrap();

    let out = dir.join("baked.json");
    let output = engine()
        .arg("simulate")
        .arg(&scene_path)
        .args(["--steps", "60"])
        .arg("--bake")
        .arg(&out)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");

    let validate = engine().arg("validate").arg(&out).output().unwrap();
    assert_eq!(validate.status.code(), Some(0), "{validate:?}");

    let baked: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let emitter = baked["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "Puff")
        .unwrap()["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "ParticleEmitter")
        .unwrap()
        .clone();
    assert_eq!(emitter["rate"], 90.0, "the live rate must bake: {emitter}");
    // Particle state is not scene state — no particle array leaks in.
    assert!(emitter.get("particles").is_none(), "{emitter}");

    // A rate the schema forbids is a located script error, not a file that
    // bakes and then fails to validate.
    std::fs::write(
        dir.join("scripts/gate.rhai"),
        "fn step(world, step) { world.set_particle_rate(\"Puff\", -5.0); }\n",
    )
    .unwrap();
    let bad = engine()
        .arg("simulate")
        .arg(&scene_path)
        .args(["--steps", "1"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(1));
    let codes = codes_of(&stderr_lines(&bad));
    assert!(codes.contains(&"script_runtime_error".to_string()), "{codes:?}");
    std::fs::remove_dir_all(&dir).ok();
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
/// is only the driver) three laps around the generated Spa-in-miniature
/// circuit — climbing and dropping through nearly eight meters on the way —
/// and parks it just past the start line. This is the M11/M12 verification
/// fixture: interactive gameplay, verified headlessly from text alone.
///
/// Scene and timeline are both generated (`examples/scenes/make_car_track.py`
/// and `make_car_track_lap.py`). Regenerating either moves every number
/// below, and `verify/baselines/m11_lap.png` with them.
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
        let report: serde_json::Value = serde_json::from_str(&stdout_of(&output)).unwrap();
        (path, report)
    };

    // Mid-drive, high side: out at the far east of the circuit, up near the
    // crest at the end of the Kemmel climb.
    let (high_bake, _) = bake_at("2000", "high.json");
    let high = baked_position(&high_bake, "Car");
    assert!(high[0] > 60.0, "mid-drive the car is on the far side: {high:?}");
    assert!(high[1] > 7.0, "and up on the high part of the circuit: {high:?}");

    // Mid-drive, low side: down at Stavelot, the bottom of the map. The same
    // recording reaching both is what makes the elevation real rather than
    // decorative — the car drove up there and back down on its suspension.
    let (low_bake, _) = bake_at("7200", "low.json");
    let low = baked_position(&low_bake, "Car");
    assert!(low[1] < 2.5, "the drive descends to the low point too: {low:?}");
    assert!(
        high[1] - low[1] > 5.0,
        "the circuit's elevation is driven, not flat: {high:?} vs {low:?}"
    );

    // After three laps and the braking phase: stopped on the pit straight,
    // a few meters past the start line it just crossed.
    let (end_bake, report) = bake_at("11988", "end.json");
    let end = baked_position(&end_bake, "Car");
    let (dx, dz) = (end[0] - -65.80, end[2] - -37.74);
    let distance = (dx * dx + dz * dz).sqrt();
    assert!(distance < 8.0, "the drive must park by the start line: {end:?}");
    assert!(end[2] < -37.74, "having crossed it, not stopped short: {end:?}");

    // The script's HUD is part of the pinned record: parked (speed 0), just
    // across the line onto lap 4, with three completed timed laps behind it
    // (last 64.37 s, best 64.15 s — a lap of this circuit is over a minute).
    // These strings are golden the way traces are: a drivetrain, geometry or
    // timing change shows up here first.
    assert_eq!(
        report["hud"],
        serde_json::json!([
            "SPEED 0 KM/H",
            "LAP 4   TIME 3.25",
            "LAST 64.37   BEST 64.15"
        ]),
        "{report}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A scene whose script counts steps through `world.state` and shows the
/// count through `world.hud` — the smallest HUD-bearing fixture.
fn hud_fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("engine-hud-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(
        dir.join("scripts/counter.rhai"),
        r#"fn step(world, step) {
            let n = world.state("n", 0);
            world.set_state("n", n + 1.0);
            world.hud("COUNT " + n.to_int());
        }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("scene.json"),
        r#"{"name":"hud","entities":[
            {"name":"Mover","components":[
                {"type":"Transform"},
                {"type":"Mesh","asset":"builtin:cube"},
                {"type":"Script","source":"scripts/counter.rhai"}
            ]},
            {"name":"Cam","components":[{"type":"Camera","active":true}]}
        ]}"#,
    )
    .unwrap();
    dir
}

/// `world.hud` + `world.state`: the final step's HUD rides the simulate
/// report, every change rides the trace, and state persists across steps —
/// all headless, no GPU required.
#[test]
fn the_hud_rides_the_simulate_report_and_the_trace() {
    let dir = hud_fixture("trace");
    let trace = dir.join("t.jsonl");
    let output = engine()
        .arg("simulate")
        .arg(dir.join("scene.json"))
        .args(["--steps", "3"])
        .arg("--trace")
        .arg(&trace)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");

    // The report carries the *final* step's HUD (state made it count).
    let report: serde_json::Value = serde_json::from_str(&stdout_of(&output)).unwrap();
    assert_eq!(report["hud"], serde_json::json!(["COUNT 2"]), "{report}");

    // The trace records each change as its own greppable event.
    let traced: Vec<serde_json::Value> = std::fs::read_to_string(&trace)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .filter(|v: &serde_json::Value| v.get("hud").is_some())
        .collect();
    assert_eq!(traced.len(), 3, "{traced:?}");
    assert_eq!(traced[0], serde_json::json!({"step": 1, "hud": ["COUNT 0"]}));
    assert_eq!(traced[2], serde_json::json!({"step": 3, "hud": ["COUNT 2"]}));
    std::fs::remove_dir_all(&dir).ok();
}

/// The HUD is pixels, not just JSON: a stepped screenshot of a HUD-bearing
/// scene differs from the unstepped one exactly because the overlay drew
/// (nothing in the fixture scene moves), and it reports the lines it drew.
#[test]
fn the_hud_lands_in_screenshot_pixels() {
    let dir = hud_fixture("pixels");
    let shot = |steps: &str, out: &str| {
        let path = dir.join(out);
        let output = engine()
            .arg("screenshot")
            .arg(dir.join("scene.json"))
            .args(["--steps", steps])
            .args(["--width", "320", "--height", "180"])
            .arg("--out")
            .arg(&path)
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
        Some((path, serde_json::from_str::<serde_json::Value>(&stdout_of(&output)).unwrap()))
    };

    let Some((with_hud, report)) = shot("1", "hud.png") else {
        eprintln!("skipping: no usable GPU on this machine");
        return;
    };
    let (without_hud, plain_report) = shot("0", "plain.png").expect("GPU worked a moment ago");

    assert_eq!(report["hud"], serde_json::json!(["COUNT 0"]), "{report}");
    assert!(plain_report.get("hud").is_none(), "no steps, no HUD: {plain_report}");
    assert_ne!(
        std::fs::read(&with_hud).unwrap(),
        std::fs::read(&without_hud).unwrap(),
        "the overlay must change the pixels"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ── HUD components (M12) ───────────────────────────────────────────────

/// The M16 fixture: sky, fog, shadows, 4× MSAA, `Material.alpha` and
/// `Material.transmission` all in one frame, pinned bit-exactly.
///
/// Also the regression test for the property the whole milestone rests on.
/// Every feature is opt-in through the scene's `environment` block, and a
/// scene that opts into none of it has to render exactly as it did before the
/// block existed — which is why none of the eleven pre-M16 baselines had to be
/// re-blessed. Those baselines are pinned by their own tests above; this one
/// pins the other side of the contract, that the features do something when
/// they *are* asked for.
#[test]
fn the_m16_environment_fixture_pins_sky_fog_shadows_and_glass() {
    let scene = repo_path("examples/scenes/verify/m16_environment.json");
    let baseline = repo_path("examples/scenes/verify/baselines/m16_environment.png");

    let validate = engine().arg("validate").arg(&scene).output().unwrap();
    assert_eq!(validate.status.code(), Some(0), "{validate:?}");

    let diff = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .output()
        .unwrap();
    if !diff.status.success() {
        let stderr = String::from_utf8_lossy(&diff.stderr);
        assert!(
            stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
            "diff-render failed for a non-GPU reason: {stderr}"
        );
        eprintln!("skipping render pin: no usable GPU on this machine");
        return;
    }
    let report: serde_json::Value = serde_json::from_str(stdout_of(&diff).trim()).unwrap();
    assert_eq!(report["pass"], true, "{report}");
    assert_eq!(report["diff_pixels"], 0, "{report}");
}

/// The M12 fixture end to end: the component overlay (all five anchors,
/// draw order, opacity, glyph coverage) plus the script-driven HudText and
/// HudRect render bit-exactly against the committed baseline, and the
/// script's HUD writes land in the baked scene under the change-based rule.
#[test]
fn the_m12_hud_fixture_pins_the_component_overlay() {
    let scene = repo_path("examples/scenes/verify/m12_hud.json");
    let baseline = repo_path("examples/scenes/verify/baselines/m12_hud.png");
    let dir = std::env::temp_dir().join(format!("engine-m12-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let diff = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .args(["--steps", "60"])
        .output()
        .unwrap();
    if !diff.status.success() {
        let stderr = String::from_utf8_lossy(&diff.stderr);
        assert!(
            stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
            "diff-render failed for a non-GPU reason: {stderr}"
        );
        eprintln!("skipping render pin: no usable GPU on this machine");
    } else {
        let report: serde_json::Value = serde_json::from_str(stdout_of(&diff).trim()).unwrap();
        assert_eq!(report["pass"], true, "{report}");
        assert_eq!(report["diff_pixels"], 0, "{report}");
    }

    // The bake half needs no GPU: after 60 steps the script has written
    // "STEP 60" and stretched the bar to 40 + 60 = 100 px.
    let baked_path = dir.join("baked.json");
    let bake = engine()
        .arg("simulate")
        .arg(&scene)
        .args(["--steps", "60"])
        .arg("--bake")
        .arg(&baked_path)
        .output()
        .unwrap();
    assert_eq!(bake.status.code(), Some(0), "{bake:?}");

    let baked: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&baked_path).unwrap()).unwrap();
    let component = |entity: &str, kind: &str| -> serde_json::Value {
        baked["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == entity)
            .and_then(|e| {
                e["components"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|c| c["type"] == kind)
            })
            .cloned()
            .unwrap()
    };
    assert_eq!(component("StepCounter", "HudText")["text"], "STEP 60");
    assert_eq!(
        component("GrowBar", "HudRect")["size"],
        serde_json::json!([100.0, 10.0])
    );
    // The backdrop bar was never written; its bytes are untouched.
    assert_eq!(
        component("GrowBarBack", "HudRect")["size"],
        serde_json::json!([160.0, 10.0])
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ── fire and point lights (M17) ────────────────────────────────────────

/// The M17 fixture: a campfire that is additive layered flame, turbulent
/// smoke, streaked embers, and — the new half — a `PointLight` a script
/// flickers, all of which has to be reproducible enough to sit under a pinned
/// PNG.
///
/// The render half proves determinism through the whole new stack at once: disc
/// emission, three jitter draws, the noise field, two blend pipelines, stretched
/// billboards, and the point-light branch in the mesh shader. The bake half
/// proves a script-driven light is scene state, like a velocity or a gauge
/// width, and lands back in a file that still validates.
#[test]
fn the_m17_fire_fixture_pins_additive_flame_and_firelight() {
    let scene = repo_path("examples/scenes/verify/m17_fire.json");
    let baseline = repo_path("examples/scenes/verify/baselines/m17_fire.png");

    let diff = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .args(["--steps", "240"])
        .output()
        .unwrap();
    if !diff.status.success() {
        let stderr = String::from_utf8_lossy(&diff.stderr);
        assert!(
            stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
            "diff-render failed for a non-GPU reason: {stderr}"
        );
        eprintln!("skipping render pin: no usable GPU on this machine");
    } else {
        let report: serde_json::Value = serde_json::from_str(stdout_of(&diff).trim()).unwrap();
        assert_eq!(report["pass"], true, "{report}");
        assert_eq!(report["diff_pixels"], 0, "{report}");
    }

    // The bake half needs no GPU.
    let dir = std::env::temp_dir().join(format!("engine-m17-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let baked_path = repo_path("examples/scenes/verify/m17_baked.json");
    let bake = engine()
        .arg("simulate")
        .arg(&scene)
        .args(["--steps", "240"])
        .arg("--bake")
        .arg(&baked_path)
        .output()
        .unwrap();
    assert_eq!(bake.status.code(), Some(0), "{bake:?}");

    let baked: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&baked_path).unwrap()).unwrap();
    let component = |entity: &str, kind: &str| -> serde_json::Value {
        baked["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == entity)
            .and_then(|e| {
                e["components"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|c| c["type"] == kind)
            })
            .cloned()
            .unwrap()
    };

    // The script writes intensity, color, and emission rates every step, so all
    // of them differ from the file's rest values and all of them bake.
    let light = component("FireLight", "PointLight");
    let intensity = light["intensity"].as_f64().unwrap();
    assert!(
        intensity > 0.0 && (intensity - 2.6).abs() > 1e-6,
        "the flickering light's intensity should have been written, got {intensity}"
    );
    assert!(
        light["color"].is_array(),
        "a script-written light color must bake as an array, got {light}"
    );
    let rate = component("Fire", "ParticleEmitter")["rate"].as_f64().unwrap();
    assert!(
        (rate - 210.0).abs() > 1e-6,
        "the flame's driven rate should have been written, got {rate}"
    );

    // The point of baking is that the result is a scene again.
    let revalidate = engine().arg("validate").arg(&baked_path).output().unwrap();
    assert_eq!(
        revalidate.status.code(),
        Some(0),
        "a baked fire must still validate: {}",
        String::from_utf8_lossy(&revalidate.stderr)
    );

    std::fs::remove_file(&baked_path).ok();
    std::fs::remove_dir_all(&dir).ok();
}

// ── trees (M19) ────────────────────────────────────────────────────────

/// The M19 fixture: six procedural trees — two broadleaves differing only in
/// `seed`, a whorled conifer, a leafless snag, a scrub, and the no-randomness
/// diagram tree — pinned bit-exactly.
///
/// Randomness and a pinned PNG are the two halves of the same promise: a tree
/// is *varied* (the twins are visibly different trees) and *reproducible* (the
/// same file grows the same mesh, on this machine, forever). The generator's
/// RNG is spelled out in-repo for exactly this reason, so no dependency
/// upgrade can reshape a forest.
///
/// The second half needs no GPU and pins the two ways a Tree can be authored
/// wrong: geometry it does not own, and geometry too big to grow.
#[test]
fn the_m19_tree_fixture_pins_seeded_procedural_growth() {
    let scene = repo_path("examples/scenes/verify/m19_trees.json");
    let baseline = repo_path("examples/scenes/verify/baselines/m19_trees.png");

    let validate = engine().arg("validate").arg(&scene).output().unwrap();
    assert_eq!(validate.status.code(), Some(0), "{validate:?}");

    let diff = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .output()
        .unwrap();
    if !diff.status.success() {
        let stderr = String::from_utf8_lossy(&diff.stderr);
        assert!(
            stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
            "diff-render failed for a non-GPU reason: {stderr}"
        );
        eprintln!("skipping render pin: no usable GPU on this machine");
    } else {
        let report: serde_json::Value = serde_json::from_str(stdout_of(&diff).trim()).unwrap();
        assert_eq!(report["pass"], true, "{report}");
        assert_eq!(report["diff_pixels"], 0, "{report}");
    }

    // A Tree *is* the entity's geometry, so a Mesh beside it would be a second
    // opinion about what the entity looks like.
    let clash = scene_file(
        "tree-with-mesh",
        r#"{"name":"clash","entities":[
            {"name":"Cam","components":[{"type":"Camera","active":true}]},
            {"name":"Oak","components":[
                {"type":"Tree"},
                {"type":"Mesh","asset":"builtin:cube"}
            ]}
        ]}"#,
    );
    let output = engine().arg("validate").arg(&clash).output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        codes_of(&stderr_lines(&output)),
        ["tree_with_mesh", "validation_failed"]
    );

    // Branching is exponential, so a plausible-looking edit can ask for a
    // billion vertices. It gets a located error rather than a hung render.
    let huge = scene_file(
        "tree-too-complex",
        r#"{"name":"huge","entities":[
            {"name":"Cam","components":[{"type":"Camera","active":true}]},
            {"name":"Oak","components":[
                {"type":"Tree","levels":4,"branches":12,"sides":16,"segments":12}
            ]}
        ]}"#,
    );
    let output = engine().arg("validate").arg(&huge).output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        codes_of(&stderr_lines(&output)),
        ["tree_too_complex", "validation_failed"]
    );
}

// ── clouds (M20) ───────────────────────────────────────────────────────

/// The M20 fixture: seven procedural clouds — two cumulus differing only in
/// `seed`, a stratocumulus raft, a storm anvil, a torn wisp, a drifting cloud,
/// and the no-randomness diagram cloud — pinned bit-exactly at `--steps 120`.
///
/// The baseline pins three things at once that are easy to break separately: a
/// cloud is *varied* (the twins are visibly different clouds) and *reproducible*
/// (the same file grows the same lobes forever, since the RNG is spelled out
/// in-repo), and `drift` runs on the reproducible clock rather than on wall
/// time, which is what lets a moving sky sit under a baseline at all. The clock
/// is pinned from both directions below, exactly as M18 pins water's.
///
/// The rest needs no GPU and pins the two ways a Cloud can be authored wrong:
/// geometry it does not own, and geometry too big to grow.
#[test]
fn the_m20_cloud_fixture_pins_seeded_clouds_and_their_clock() {
    let scene = repo_path("examples/scenes/verify/m20_clouds.json");
    let baseline = repo_path("examples/scenes/verify/baselines/m20_clouds.png");

    let validate = engine().arg("validate").arg(&scene).output().unwrap();
    assert_eq!(validate.status.code(), Some(0), "{validate:?}");

    let diff = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .args(["--steps", "120"])
        .output()
        .unwrap();
    if !diff.status.success() {
        let stderr = String::from_utf8_lossy(&diff.stderr);
        assert!(
            stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
            "diff-render failed for a non-GPU reason: {stderr}"
        );
        eprintln!("skipping render pin: no usable GPU on this machine");
    } else {
        let report: serde_json::Value = serde_json::from_str(stdout_of(&diff).trim()).unwrap();
        assert_eq!(report["pass"], true, "{report}");
        assert_eq!(report["diff_pixels"], 0, "{report}");

        // 120 steps at the scene's 60 Hz *is* two seconds: the two flags name
        // the same instant, and the renderer has one clock.
        let by_time = engine()
            .arg("diff-render")
            .arg(&scene)
            .arg(&baseline)
            .args(["--time", "2.0"])
            .output()
            .unwrap();
        assert_eq!(by_time.status.code(), Some(0), "{by_time:?}");
        let report: serde_json::Value =
            serde_json::from_str(stdout_of(&by_time).trim()).unwrap();
        assert_eq!(
            report["diff_pixels"], 0,
            "--time 2.0 must render the same sky as --steps 120 at 60 Hz: {report}"
        );

        // And a scene that says nothing about time renders its clouds where the
        // file put them, which is a *different* picture — otherwise the two
        // assertions above would pass for the trivial reason that nothing drifts.
        let at_rest = engine()
            .arg("diff-render")
            .arg(&scene)
            .arg(&baseline)
            .output()
            .unwrap();
        let report: serde_json::Value =
            serde_json::from_str(stdout_of(&at_rest).trim()).unwrap();
        assert_eq!(
            report["pass"], false,
            "a cloud at t=0 should not match a baseline blessed at t=2: {report}"
        );
    }

    // A Cloud *is* the entity's geometry and carries its own colours, so a Mesh
    // or a Material beside it is a second, silently ignored opinion.
    let clash = scene_file(
        "cloud-with-mesh",
        r#"{"name":"clash","entities":[
            {"name":"Cam","components":[{"type":"Camera","active":true}]},
            {"name":"Cumulus","components":[
                {"type":"Cloud"},
                {"type":"Material"}
            ]}
        ]}"#,
    );
    let output = engine().arg("validate").arg(&clash).output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        codes_of(&stderr_lines(&output)),
        ["cloud_with_mesh", "validation_failed"]
    );

    // Lobes are exponential in `levels`, so a plausible-looking edit can ask
    // for millions of vertices. It gets a located error naming a real number,
    // rather than a render that looks like it hung.
    let huge = scene_file(
        "cloud-too-complex",
        r#"{"name":"huge","entities":[
            {"name":"Cam","components":[{"type":"Camera","active":true}]},
            {"name":"Storm","components":[
                {"type":"Cloud","lobes":32,"levels":3,"children":8,"detail":3}
            ]}
        ]}"#,
    );
    let output = engine().arg("validate").arg(&huge).output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        codes_of(&stderr_lines(&output)),
        ["cloud_too_complex", "validation_failed"]
    );
}

// ── collision (M12) ────────────────────────────────────────────────────

/// End to end: a dynamic box drops onto a trimesh ground (geometry borrowed
/// from the entity's own builtin plane, layer-filtered), and a script sees
/// the contact through `world.touching` and reacts by moving a marker —
/// gameplay reacting to a hit, verified from text files alone.
#[test]
fn scripts_react_to_contacts_end_to_end() {
    let dir = std::env::temp_dir().join(format!("engine-m12-contact-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(
        dir.join("scripts/alarm.rhai"),
        r#"fn step(world, step) {
            if world.touching("Box").len() > 0 {
                world.set_position("Marker", 0.0, 9.0, 0.0);
            }
        }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("scene.json"),
        r#"{"name":"m12","entities":[
            {"name":"Ground","components":[
                {"type":"Transform","scale":[10.0,1.0,10.0]},
                {"type":"Mesh","asset":"builtin:plane"},
                {"type":"RigidBody","body":"fixed"},
                {"type":"Collider","shape":"trimesh","layers":["world"]}
            ]},
            {"name":"Box","components":[
                {"type":"Transform","position":[0.0,1.5,0.0]},
                {"type":"Mesh","asset":"builtin:cube"},
                {"type":"RigidBody","body":"dynamic"},
                {"type":"Collider","shape":"cuboid","half_extents":[0.5,0.5,0.5],
                 "collides_with":["world"]}
            ]},
            {"name":"Marker","components":[
                {"type":"Transform","position":[0.0,0.0,0.0]},
                {"type":"Script","source":"scripts/alarm.rhai"}
            ]},
            {"name":"Cam","components":[{"type":"Camera","active":true}]}
        ]}"#,
    )
    .unwrap();

    let validate = engine().arg("validate").arg(dir.join("scene.json")).output().unwrap();
    assert_eq!(validate.status.code(), Some(0), "{validate:?}");

    let bake = dir.join("baked.json");
    let output = engine()
        .arg("simulate")
        .arg(dir.join("scene.json"))
        .args(["--steps", "90"])
        .arg("--bake")
        .arg(&bake)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        baked_position(&bake, "Marker"),
        vec![0.0, 9.0, 0.0],
        "the script must have seen the Box↔Ground contact"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ── particles (M13) ────────────────────────────────────────────────────

/// A minimal emitter scene: sprays up from the origin, visible from the
/// fixture camera after a second of stepping.
const PARTICLE_SCENE: &str = r#"{"name":"puff","entities":[
    {"name":"Fountain","components":[
        {"type":"Transform","rotation":[90.0,0.0,0.0]},
        {"type":"ParticleEmitter","rate":30.0,"lifetime":2.0,"speed":1.0,
         "start_size":0.3,"end_size":0.3,"start_alpha":1.0,"end_alpha":0.5,
         "seed":11}]},
    {"name":"Cam","components":[
        {"type":"Transform","position":[0.0,1.0,5.0]},
        {"type":"Camera","active":true}]}
]}"#;

/// Particle state is created by `--steps` and by nothing else — and it is
/// deterministic: the same file and step count produce byte-identical
/// pixels, seeded RNG included, which is what lets an emitter live under a
/// committed diff-render baseline like `verify/m13_smoke.json` does.
#[test]
fn particles_advance_with_steps_and_render_deterministically() {
    let scene = scene_file("particles-steps", PARTICLE_SCENE);
    let shot = |steps: &str, out: &str| {
        let path = scene.with_file_name(out);
        let output = engine()
            .arg("screenshot")
            .arg(&scene)
            .args(["--steps", steps])
            .args(["--width", "128", "--height", "96"])
            .arg("--out")
            .arg(&path)
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
        Some(std::fs::read(&path).unwrap())
    };

    let Some(stepped) = shot("60", "stepped.png") else {
        eprintln!("skipping: no usable GPU on this machine");
        return;
    };
    let unstepped = shot("0", "unstepped.png").expect("GPU worked a moment ago");
    let again = shot("60", "again.png").expect("GPU worked a moment ago");

    assert_ne!(stepped, unstepped, "60 steps of particles must draw something");
    assert_eq!(stepped, again, "same file + steps must be byte-identical");
}

/// The first integer component fields (`seed`, `max_particles`) go through
/// the same walk as everything else: a float or negative where a u32
/// belongs is a shape error, a zero below the documented minimum is a range
/// error — both located, neither a `scene_parse_desync`.
#[test]
fn emitter_integer_fields_validate_like_everything_else() {
    let scene = scene_file(
        "particles-ints",
        r#"{"name":"bad","entities":[{"name":"A","components":[
            {"type":"ParticleEmitter","seed":1.5,"max_particles":0}]}]}"#,
    );
    let output = engine().arg("validate").arg(&scene).output().unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout_of(&output).is_empty(), "stdout must be silent on failure");
    let lines = stderr_lines(&output);
    let codes = codes_of(&lines);
    assert!(codes.contains(&"invalid_field_type".to_string()), "{codes:?}");
    assert!(codes.contains(&"value_out_of_range".to_string()), "{codes:?}");
    for line in lines.iter().filter(|l| l["error"] != "validation_failed") {
        assert!(line["line"].is_u64(), "diagnostic without a line: {line}");
    }
}

#[test]
fn m13_smoke_fixture_validates_clean() {
    // The positive twin from milestone-verification-scenes.md; its pixel
    // pinning is `engine diff-render` against the committed per-adapter
    // baseline, not a portable unit test.
    let output = engine()
        .arg("validate")
        .arg(repo_path("examples/scenes/verify/m13_smoke.json"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stderr.is_empty(),
        "no errors and no warnings expected: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── breaking (M14) ─────────────────────────────────────────────────────

/// Determinism extends through breaks: the fixture's crate shatters at the
/// same step into the same fragments, byte-identical run to run and against
/// the committed golden. The golden also pins the trace shape: one `broke`
/// line, and fragment rows joining from the step after the break.
#[test]
fn breaking_traces_are_deterministic_and_match_the_golden() {
    let scene = repo_path("examples/scenes/verify/m14_break.json");
    let trace = |name: &str| {
        let path = std::env::temp_dir().join(format!(
            "engine-m14-{}-{name}.jsonl",
            std::process::id()
        ));
        let output = engine()
            .arg("simulate")
            .arg(&scene)
            .args(["--steps", "300"])
            .arg("--trace")
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        std::fs::read(&path).unwrap()
    };

    let first = trace("a");
    assert_eq!(first, trace("b"), "twice-run traces must be byte-identical");

    let golden = std::fs::read(repo_path(
        "examples/scenes/verify/baselines/m14_break.trace.jsonl",
    ))
    .unwrap();
    assert_eq!(
        first, golden,
        "trace drifted from the committed golden — if a rapier upgrade \
         caused this, review the diff as a breaking change"
    );

    let lines: Vec<serde_json::Value> = String::from_utf8(golden)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let breaks: Vec<&serde_json::Value> =
        lines.iter().filter(|l| l.get("broke").is_some()).collect();
    assert_eq!(breaks.len(), 1, "exactly one break");
    assert_eq!(breaks[0]["broke"], "Crate");
    let broke_step = breaks[0]["step"].as_u64().unwrap();
    let first_fragment_row = lines
        .iter()
        .find(|l| l["entity"] == "Crate.frag0")
        .expect("fragments join the rows");
    assert_eq!(
        first_fragment_row["step"].as_u64().unwrap(),
        broke_step + 1,
        "fragment rows start the step after the break"
    );
}

/// A break is a structural change bake must survive: the broken entity is
/// spliced out of the file, its fragments spliced in with their full
/// current state, and the result is a valid scene.
#[test]
fn a_break_bakes_to_a_valid_scene_with_fragments() {
    let scene = repo_path("examples/scenes/verify/m14_break.json");
    let dir = std::env::temp_dir().join(format!("engine-m14-bake-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let baked = dir.join("baked.json");

    let output = engine()
        .arg("simulate")
        .arg(&scene)
        .args(["--steps", "300"])
        .arg("--bake")
        .arg(&baked)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");

    let validate = engine().arg("validate").arg(&baked).output().unwrap();
    assert_eq!(validate.status.code(), Some(0), "the baked scene validates: {validate:?}");

    let root: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&baked).unwrap()).unwrap();
    let names: Vec<&str> = root["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"Crate"), "the broken entity is gone: {names:?}");
    assert!(names.contains(&"Ball"), "untouched entities survive: {names:?}");
    for i in 0..4 {
        let name = format!("Crate.frag{i}");
        let entity = root["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == name.as_str())
            .unwrap_or_else(|| panic!("{name} baked in: {names:?}"));
        let body = entity["components"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["type"] == "RigidBody")
            .expect("fragments bake with their body");
        assert_eq!(body["body"], "dynamic");
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// The baked post-break scene reloads into exactly the post-break world:
/// rendering it at rest equals rendering the original scene at the bake
/// step, bit for bit.
#[test]
fn a_baked_break_renders_bit_exactly() {
    let scene = repo_path("examples/scenes/verify/m14_break.json");
    let dir = std::env::temp_dir().join(format!("engine-m14-render-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let baked = dir.join("baked.json");

    let output = engine()
        .arg("simulate")
        .arg(&scene)
        .args(["--steps", "300"])
        .arg("--bake")
        .arg(&baked)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");

    let shot = |scene: &Path, steps: &str, out: &str| {
        let path = dir.join(out);
        let output = engine()
            .arg("screenshot")
            .arg(scene)
            .args(["--steps", steps])
            .args(["--width", "320", "--height", "180"])
            .arg("--out")
            .arg(&path)
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
        Some(path)
    };

    let Some(live) = shot(&scene, "300", "live.png") else {
        eprintln!("skipping: no usable GPU on this machine");
        return;
    };
    let resumed = shot(&baked, "0", "resumed.png").expect("GPU worked a moment ago");
    assert_eq!(
        std::fs::read(&live).unwrap(),
        std::fs::read(&resumed).unwrap(),
        "the baked scene must reload into exactly the post-break world"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `world.break_entity` and `world.explode` drive breaks from scripts —
/// headless, deterministic, and observable in the trace. The script-only
/// crate has no threshold (nothing but the script can break it); the
/// thresholded one is broken by the blast.
#[test]
fn scripts_break_and_explode() {
    let dir = std::env::temp_dir().join(format!("engine-m14-script-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(
        dir.join("scripts/demolish.rhai"),
        r#"fn step(world, step) {
            if step == 5 { world.break_entity("CrateA"); }
            if step == 10 { world.explode(0.0, 0.5, 2.0, 3.0, 50.0); }
        }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("scene.json"),
        r#"{"name":"demolition","entities":[
            {"name":"Ground","components":[
                {"type":"Transform"},
                {"type":"Collider","shape":"cuboid","half_extents":[10.0,0.05,10.0]}
            ]},
            {"name":"CrateA","components":[
                {"type":"Transform","position":[0.0,0.55,0.0]},
                {"type":"Mesh","asset":"builtin:cube"},
                {"type":"Breakable","fragments":[
                    {"mesh":"builtin:cube","offset":[-0.25,0.0,0.0],
                     "scale":[0.5,0.5,0.5],"half_extents":[0.25,0.25,0.25]},
                    {"mesh":"builtin:cube","offset":[0.25,0.0,0.0],
                     "scale":[0.5,0.5,0.5],"half_extents":[0.25,0.25,0.25]}
                ]},
                {"type":"Script","source":"scripts/demolish.rhai"}
            ]},
            {"name":"CrateB","components":[
                {"type":"Transform","position":[0.0,0.55,2.0]},
                {"type":"Mesh","asset":"builtin:cube"},
                {"type":"Collider","shape":"cuboid","half_extents":[0.5,0.5,0.5]},
                {"type":"Breakable","impulse_threshold":5.0,"fragments":[
                    {"mesh":"builtin:cube","offset":[0.0,0.0,0.0],
                     "scale":[0.5,0.5,0.5],"half_extents":[0.25,0.25,0.25]}
                ]}
            ]},
            {"name":"Cam","components":[{"type":"Camera","active":true}]}
        ]}"#,
    )
    .unwrap();

    let trace_path = dir.join("t.jsonl");
    let run = || {
        let output = engine()
            .arg("simulate")
            .arg(dir.join("scene.json"))
            .args(["--steps", "60"])
            .arg("--trace")
            .arg(&trace_path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        std::fs::read(&trace_path).unwrap()
    };
    let first = run();
    assert_eq!(first, run(), "scripted breaks stay deterministic");

    let lines: Vec<serde_json::Value> = String::from_utf8(first)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let breaks: Vec<(&str, u64)> = lines
        .iter()
        .filter_map(|l| Some((l.get("broke")?.as_str()?, l["step"].as_u64()?)))
        .collect();
    // break_entity is queued at script step 5 (trace step 6); the blast is
    // queued at script step 10 and lands in trace step 11's physics.
    assert_eq!(breaks, [("CrateA", 6), ("CrateB", 11)], "{lines:?}");
    assert!(
        lines.iter().any(|l| l["entity"] == "CrateA.frag0"),
        "script-broken fragments trace as dynamic bodies"
    );
    assert!(
        lines.iter().any(|l| l["entity"] == "CrateB.frag0"),
        "blast-broken fragments trace as dynamic bodies"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The new validation rule rides the NDJSON protocol like every other:
/// a thresholded Breakable with no Collider is `breakable_without_collider`,
/// exit 1.
#[test]
fn a_thresholded_breakable_without_a_collider_fails_validation() {
    let scene = scene_file(
        "breakable-no-collider",
        r#"{"name":"s","entities":[
            {"name":"Cam","components":[{"type":"Camera","active":true}]},
            {"name":"Crate","components":[
                {"type":"Transform"},
                {"type":"Breakable","impulse_threshold":5.0,
                 "fragments":[{"mesh":"builtin:cube"}]}
            ]}
        ]}"#,
    );
    let output = engine().arg("validate").arg(&scene).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout_of(&output).is_empty(), "stdout must be empty on failure");
    let codes = codes_of(&stderr_lines(&output));
    assert!(
        codes.contains(&"breakable_without_collider".to_string()),
        "{codes:?}"
    );
}

// ── The showcase tour (showcase-tour.md) ───────────────────────────────────

/// The tour is the scene every system has to keep working in, so what it
/// pins is the *whole stack running together for 15 seconds*: animation,
/// scripts, wheels, physics, breaking and particles all advancing on one
/// fixed clock, twice, to the same bytes.
///
/// The trace itself is 2 MB and would churn on every framing tweak, so there
/// is no committed golden here — determinism against a second run plus the
/// pinned story beats below is the contract. The PNG baselines under
/// `verify/baselines/showcase_*.png` are per-adapter artifacts and are
/// checked with `engine diff-render` by hand, not from a test.
#[test]
fn the_showcase_tour_runs_fifteen_deterministic_seconds() {
    let scene = repo_path("examples/scenes/showcase_tour.json");
    let trace = |name: &str| {
        let path = std::env::temp_dir()
            .join(format!("engine-tour-{}-{name}.jsonl", std::process::id()));
        let output = engine()
            .arg("simulate")
            .arg(&scene)
            .args(["--steps", "900"])
            .arg("--trace")
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        (bytes, stdout_of(&output))
    };

    let (first, report) = trace("a");
    let (second, _) = trace("b");
    assert_eq!(first, second, "twice-run traces must be byte-identical");

    // 900 steps at 60 Hz is the advertised fifteen seconds.
    let report: serde_json::Value = serde_json::from_str(report.trim()).unwrap();
    assert_eq!(report["simulated_steps"], 900);
    assert_eq!(report["timestep_hz"], 60);
    assert_eq!(
        report["hud"][0].as_str().unwrap(),
        "TOUR 900/900  05 THE WHOLE WORLD",
        "the director's last line names the last station"
    );

    let lines: Vec<serde_json::Value> = String::from_utf8(first)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    // Station 04 demonstrates all three ways to break something, and each
    // has to land inside its 180-step window (steps 540..720) or the camera
    // is looking somewhere else when it happens.
    let breaks: Vec<(u64, &str)> = lines
        .iter()
        .filter_map(|l| {
            Some((l.get("step")?.as_u64()?, l.get("broke")?.as_str()?))
        })
        .collect();
    for (step, what) in &breaks {
        assert!(
            (540..720).contains(step),
            "{what} broke at step {step}, outside the breaking station's window"
        );
    }
    // Which crate each trigger happens to claim is a float-level detail that
    // moves between optimisation levels, so what is pinned is the trigger
    // *sequence*: the boulder's impact, then the script's named break on its
    // exact step, then the blast.
    let scripted = breaks
        .iter()
        .find(|(_, what)| *what == "IcePillar")
        .expect("world.break_entity fires on IcePillar");
    assert_eq!(scripted.0, 601, "a scripted break lands the step after the call");
    assert!(
        breaks.iter().any(|(step, _)| (580..600).contains(step)),
        "the boulder should shatter something on impact: {breaks:?}"
    );
    assert!(
        breaks.iter().any(|(step, _)| *step >= 637),
        "the explosion should still find something to break: {breaks:?}"
    );

    // Nothing may leave the world. A body that loses its ground contact
    // falls forever in silence — no error, no failed validation, just a
    // scene that renders wrong — so the tour asserts the floor holds.
    let last = lines
        .iter()
        .filter(|l| l["step"] == 900 && l.get("position").is_some());
    for row in last {
        let y = row["position"][1].as_f64().unwrap();
        assert!(
            y > -1.0,
            "{} ended up at y={y}: it fell through the world",
            row["entity"]
        );
    }
}

/// The tour drives a vehicle around a world full of resting bodies — the
/// combination that used to silently disable every other collider's
/// contacts. The crates start flush on the ground, so if the first step
/// swallows their broad-phase pairs they never touch anything again.
#[test]
fn the_showcase_tour_keeps_its_crates_on_the_ground() {
    let scene = repo_path("examples/scenes/showcase_tour.json");
    let path = std::env::temp_dir().join(format!("engine-tour-rest-{}.jsonl", std::process::id()));
    let output = engine()
        .arg("simulate")
        .arg(&scene)
        .args(["--steps", "300"])
        .arg("--trace")
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let trace = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);

    // Five seconds in, long before anything is meant to move, the stack is
    // still standing where the file put it.
    //
    // Measured against each body's *authored* height rather than a world
    // constant. Since M22 the ground is terrain, so "resting" is a different
    // number for every entity and a fixed floor would only be pinning where
    // this particular hillside happens to sit — the claim is that nothing sank
    // through whatever it was standing on.
    let authored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&scene).unwrap()).unwrap();
    let authored_y = |name: &str| -> f64 {
        authored["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == name)
            .and_then(|e| e["components"].as_array())
            .unwrap()
            .iter()
            .find(|c| c["type"] == "Transform")
            .and_then(|t| t["position"][1].as_f64())
            .unwrap_or_else(|| panic!("{name} has no authored height"))
    };

    let resting: Vec<serde_json::Value> = trace
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .filter(|l: &serde_json::Value| l["step"] == 300)
        .collect();
    for name in ["Crate1", "Crate2", "Crate3", "Boulder", "IcePillar"] {
        let row = resting
            .iter()
            .find(|l| l["entity"] == name)
            .unwrap_or_else(|| panic!("{name} left the trace entirely"));
        let y = row["position"][1].as_f64().unwrap();
        let start = authored_y(name);
        assert!(
            y > start - 0.3,
            "{name} sank to y={y} from an authored {start} — \
             a resting body lost its ground contact"
        );
    }
}

/// The M18 fixture: a lake with Gerstner waves over a sloping bed, a shoreline,
/// and three things standing in the water.
///
/// The render half pins the whole new path at once — wave displacement in the
/// vertex stage, the depth copy between the two passes, absorption, foam, and
/// the sorted blended list that puts a transmissive ice floe in the same
/// ordering as the surface it sits in.
///
/// The rest of the test is the property that makes such a pin possible at all:
/// water is a pure function of the file and the clock. `--steps 120` at 60 Hz
/// and `--time 2.0` are the same instant said two ways, so they have to produce
/// the same bytes; and the same instant asked for twice has to as well. Without
/// that, a water baseline would be a flake generator rather than a regression
/// test.
#[test]
fn the_m18_water_fixture_pins_waves_depth_and_foam() {
    let scene = repo_path("examples/scenes/verify/m18_water.json");
    let baseline = repo_path("examples/scenes/verify/baselines/m18_water.png");

    let diff = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .args(["--steps", "120"])
        .output()
        .unwrap();
    if !diff.status.success() {
        let stderr = String::from_utf8_lossy(&diff.stderr);
        assert!(
            stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
            "diff-render failed for a non-GPU reason: {stderr}"
        );
        eprintln!("skipping render pin: no usable GPU on this machine");
        return;
    }
    let report: serde_json::Value = serde_json::from_str(stdout_of(&diff).trim()).unwrap();
    assert_eq!(report["pass"], true, "{report}");
    assert_eq!(report["diff_pixels"], 0, "{report}");

    // 120 steps at the scene's 60 Hz *is* two seconds: the two flags name the
    // same instant, and the renderer has one clock.
    let by_time = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .args(["--time", "2.0"])
        .output()
        .unwrap();
    assert_eq!(by_time.status.code(), Some(0), "{by_time:?}");
    let report: serde_json::Value = serde_json::from_str(stdout_of(&by_time).trim()).unwrap();
    assert_eq!(
        report["diff_pixels"], 0,
        "--time 2.0 must render the same water as --steps 120 at 60 Hz: {report}"
    );

    // And a scene that says nothing about time renders its water at rest, which
    // is a *different* picture — otherwise the two assertions above would pass
    // for the trivial reason that the clock is ignored.
    let at_rest = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_str(stdout_of(&at_rest).trim()).unwrap();
    assert_eq!(
        report["pass"], false,
        "water at t=0 should not match a baseline blessed at t=2: {report}"
    );
}

/// M21: one scene file, five times of day.
///
/// The fixture runs a 24-second day (`day_length: 24.0`), so an hour is a
/// second and step `hour * 60` at 60 Hz is that hour — which is why the
/// baselines are named for the clock and not for a step count. Five renders
/// from *one* file is the point: the day is a pure function of the clock, so
/// there is nothing to author per time of day.
///
/// The lamp is what makes the night baselines more than a dark picture. Its
/// `PointLight` starts at intensity 0 and `scripts/m21_lamp.rhai` raises it off
/// `world.sun_altitude()`, so the two night frames are lit by something that
/// read the clock. That also means these renders need `--steps` rather than
/// `--time`: scripts run on the step loop, and a `--time` render never steps.
#[test]
fn the_m21_daylight_fixture_pins_a_whole_day_from_one_file() {
    let scene = repo_path("examples/scenes/verify/m21_daylight.json");

    let validate = engine().arg("validate").arg(&scene).output().unwrap();
    assert_eq!(validate.status.code(), Some(0), "{validate:?}");

    // (label, steps) — night, sunrise, noon, sunset, night again.
    for (label, steps) in [
        ("0200", "120"),
        ("0630", "390"),
        ("1200", "720"),
        ("1830", "1110"),
        ("2200", "1320"),
    ] {
        let baseline = repo_path(&format!(
            "examples/scenes/verify/baselines/m21_daylight_{label}.png"
        ));
        let diff = engine()
            .arg("diff-render")
            .arg(&scene)
            .arg(&baseline)
            .args(["--steps", steps])
            .output()
            .unwrap();
        if !diff.status.success() {
            let stderr = String::from_utf8_lossy(&diff.stderr);
            assert!(
                stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
                "diff-render failed for a non-GPU reason at {label}: {stderr}"
            );
            eprintln!("skipping render pin: no usable GPU on this machine");
            return;
        }
        let report: serde_json::Value = serde_json::from_str(stdout_of(&diff).trim()).unwrap();
        assert_eq!(report["pass"], true, "at {label}: {report}");
        assert_eq!(report["diff_pixels"], 0, "at {label}: {report}");
    }

    // The times have to actually differ, or the five assertions above would
    // all pass for the trivial reason that the clock is ignored.
    let noon = repo_path("examples/scenes/verify/baselines/m21_daylight_1200.png");
    let at_night = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&noon)
        .args(["--steps", "120"])
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_str(stdout_of(&at_night).trim()).unwrap();
    assert_eq!(
        report["pass"], false,
        "02:00 must not match a baseline blessed at noon: {report}"
    );
}

/// M21: the two ways a scene can disagree with its own daylight block.
///
/// GPU-free, so it runs everywhere the rest of the suite's validation tests do.
#[test]
fn daylight_owns_the_sun_and_says_so() {
    // Two owners of one sun (invariant 8).
    let clash = scene_file(
        "daylight-and-sun",
        r#"{"name":"clash","daylight":{},"entities":[
            {"name":"Cam","components":[{"type":"Camera","active":true}]},
            {"name":"Sun","components":[
                {"type":"Transform","rotation":[-40.0,0.0,0.0]},
                {"type":"DirectionalLight"}
            ]}
        ]}"#,
    );
    let output = engine().arg("validate").arg(&clash).output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        codes_of(&stderr_lines(&output)),
        ["daylight_and_directional_light", "validation_failed"]
    );

    // An authored sky under `drives_sky` is a warning, not a failure — the
    // scene still renders, it just renders the day's colors.
    let overridden = scene_file(
        "daylight-overrides-sky",
        r#"{"name":"warn",
            "daylight":{},
            "environment":{"sky":true,"sky_zenith":[0.1,0.2,0.3]},
            "entities":[
                {"name":"Cam","components":[{"type":"Camera","active":true}]}
            ]}"#,
    );
    let output = engine().arg("validate").arg(&overridden).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "a warning must not fail: {output:?}");
    assert_eq!(
        codes_of(&stderr_lines(&output)),
        ["daylight_overrides_sky"]
    );

    // ...and --strict promotes it, like every other warning.
    let strict = engine()
        .arg("validate")
        .arg(&overridden)
        .arg("--strict")
        .output()
        .unwrap();
    assert_eq!(strict.status.code(), Some(1), "{strict:?}");
}
/// M22's fixture: a landscape with relief, a generated surface material, a
/// trimesh collider taken from that same surface, and props standing on it.
///
/// The render pins the whole path at once — the CPU height field, the local
/// normals the shader lights it by, slope- and height-selected layers, the
/// detail noise, and the fact that terrain reaches all of this through the mesh
/// pipeline (so it takes the sun, the shadow map, the sky ambient and the fog
/// with it).
///
/// The rest is what makes such a pin possible. Terrain is a pure function of the
/// file: unlike water it does not move with the clock, so the *same* baseline
/// has to match whether the scene is asked for at step 0 or step 120 — every
/// pixel that differs between those two is the ball falling, not the ground
/// drifting. And the collider has to come from the surface: the dropped sphere
/// is authored in mid-air, so if terrain were invisible to physics it would fall
/// through the world and out of frame.
#[test]
fn the_m22_terrain_fixture_pins_relief_layers_and_collision() {
    let scene = repo_path("examples/scenes/verify/m22_terrain.json");
    let baseline = repo_path("examples/scenes/verify/baselines/m22_terrain.png");

    let diff = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .args(["--steps", "120"])
        .output()
        .unwrap();
    if !diff.status.success() {
        let stderr = String::from_utf8_lossy(&diff.stderr);
        assert!(
            stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
            "diff-render failed for a non-GPU reason: {stderr}"
        );
        eprintln!("skipping render pin: no usable GPU on this machine");
        return;
    }
    let report: serde_json::Value = serde_json::from_str(stdout_of(&diff).trim()).unwrap();
    assert_eq!(report["pass"], true, "{report}");
    assert_eq!(report["diff_pixels"], 0, "{report}");

    // The ball has to land *on* the ground rather than through it, which is the
    // one claim a picture of a hillside cannot make on its own: the collider is
    // generated from the same height field the renderer draws.
    let simulate = engine()
        .arg("simulate")
        .arg(&scene)
        .args(["--steps", "120"])
        .output()
        .unwrap();
    assert_eq!(simulate.status.code(), Some(0), "{simulate:?}");

    let trace_dir =
        std::env::temp_dir().join(format!("engine-m22-{}", std::process::id()));
    std::fs::create_dir_all(&trace_dir).unwrap();
    let trace_path = trace_dir.join("terrain.jsonl");
    let traced = engine()
        .arg("simulate")
        .arg(&scene)
        .args(["--steps", "120"])
        .arg("--trace")
        .arg(&trace_path)
        .output()
        .unwrap();
    assert_eq!(traced.status.code(), Some(0), "{traced:?}");

    let rows: Vec<serde_json::Value> = std::fs::read_to_string(&trace_path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let ball = rows
        .iter()
        .rev()
        .find(|row| row["entity"] == "Dropped")
        .expect("the dropped sphere should appear in the trace");
    let y = ball["position"][1].as_f64().unwrap();

    // It starts at y = 2.0 over ground about 4 m below it. Landing *on* the
    // hillside puts it near -3.8; still up at 2 means gravity never ran, and
    // far below the ground means the trimesh collider was never built from the
    // terrain and it fell through the world.
    assert!(
        y > -5.0,
        "the ball fell through the terrain — it ended at y = {y}: {ball}"
    );
    assert!(
        y < -2.0,
        "the ball never fell — it ended at y = {y}: {ball}"
    );
}
