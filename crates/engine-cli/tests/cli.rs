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
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
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
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("stderr line is not a JSON object ({e}): {line:?}"))
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
    assert!(
        codes.contains(&"unknown_component".to_string()),
        "{codes:?}"
    );
    assert!(
        codes.contains(&"duplicate_entity_name".to_string()),
        "{codes:?}"
    );
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
    assert!(lines.iter().any(|l| l["file"]
        .as_str()
        .is_some_and(|f| f.contains("multi-broken"))));
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
    assert!(
        codes.contains(&"unknown_component".to_string()),
        "{codes:?}"
    );
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
    assert_eq!(
        lines.len(),
        1,
        "the backtrace must ride escaped inside the JSON"
    );
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
    assert!(
        lines.len() >= 7,
        "expected all errors at once, got {lines:?}"
    );

    let codes = codes_of(&lines);
    for expected in [
        "unknown_component",
        "value_out_of_range",
        "asset_not_found",
        "unknown_field",
        "multiple_active_cameras",
    ] {
        assert!(
            codes.contains(&expected.to_string()),
            "missing {expected}: {codes:?}"
        );
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
    assert!(
        codes.contains(&"unknown_component".to_string()),
        "{codes:?}"
    );
    assert!(
        codes.contains(&"duplicate_entity_name".to_string()),
        "{codes:?}"
    );
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
    let report: serde_json::Value = serde_json::from_str(stdout_of(&self_diff).trim()).unwrap();
    assert_eq!(report["pass"], true);
    assert_eq!(report["diff_pixels"], 0);
    assert_eq!(report["width"], 96, "render size comes from the baseline");
    assert_eq!(report["height"], 64);
    assert!(
        report["diff_bounds"].is_null(),
        "no bounds when nothing differs"
    );
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
    let report: serde_json::Value = serde_json::from_str(stdout_of(&changed_diff).trim()).unwrap();
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
        let path =
            std::env::temp_dir().join(format!("engine-m8-{}-{name}.jsonl", std::process::id()));
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

    let golden = std::fs::read(repo_path(
        "examples/scenes/verify/baselines/m8_drop.trace.jsonl",
    ))
    .unwrap();
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
        assert!(
            codes.contains(&expected.to_string()),
            "missing {expected}: {codes:?}"
        );
    }
    let lines = stderr_lines(&output);
    let body = lines
        .iter()
        .find(|l| l["error"] == "unknown_body_kind")
        .unwrap();
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
    assert_eq!(
        t0, period,
        "t=2.0 is the loop period; pose and pixels must match t=0"
    );
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
    assert_eq!(
        report["clips"][0]["tracks"][0]["property"],
        "Transform.rotation"
    );
}

#[test]
fn clip_files_validate_directly() {
    let output = engine()
        .arg("validate")
        .arg(repo_path(
            "examples/scenes/verify/animations/spin.anim.json",
        ))
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

    let output = engine()
        .arg("validate")
        .arg(dir.join("scene.json"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let lines = stderr_lines(&output);
    let codes = codes_of(&lines);
    for expected in [
        "unknown_entity",
        "conflicting_tracks",
        "animation_on_dynamic_body",
    ] {
        assert!(
            codes.contains(&expected.to_string()),
            "missing {expected}: {codes:?}"
        );
    }
    let typo = lines
        .iter()
        .find(|l| l["error"] == "unknown_entity")
        .unwrap();
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
        let path =
            std::env::temp_dir().join(format!("engine-m10-{}-{name}.jsonl", std::process::id()));
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
    assert_eq!(
        first,
        trace("b"),
        "determinism must hold with scripts running"
    );
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
    assert!(
        (y - 2.25).abs() < 1e-3,
        "elevator baked at {y}, expected ~2.25"
    );
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
    assert!(
        codes.contains(&"script_runtime_error".to_string()),
        "{codes:?}"
    );
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
    let error = lines
        .iter()
        .find(|l| l["error"] == "script_runtime_error")
        .unwrap();
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

    let output = engine()
        .arg("validate")
        .arg(dir.join("scene.json"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let codes = codes_of(&stderr_lines(&output));
    assert!(
        codes.contains(&"script_parse_error".to_string()),
        "{codes:?}"
    );
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
    assert!(
        stdout_of(&output).is_empty(),
        "stdout must be silent on failure"
    );

    let lines = stderr_lines(&output);
    let codes = codes_of(&lines);
    assert!(codes.contains(&"unknown_key".to_string()), "{codes:?}");
    assert!(
        codes.contains(&"input_parse_error".to_string()),
        "{codes:?}"
    );
    assert!(
        codes.contains(&"unsorted_input_steps".to_string()),
        "{codes:?}"
    );

    let typo = lines.iter().find(|l| l["error"] == "unknown_key").unwrap();
    assert_eq!(typo["did_you_mean"], "ArrowUp");
    assert_eq!(typo["line"], 1);
    std::fs::remove_dir_all(&dir).ok();
}

// ── The mouse (M28) ────────────────────────────────────────────────────

/// The M28 fixture, end to end: the committed timeline's cursor drives a
/// marker across the ground through the engine's own inverse projection, and
/// a held button over the HUD's plate is a click.
///
/// The numbers here are *not* eyeballed — each is where the ray through that
/// cursor meets the plane, and the two baselines
/// (`m28_pointer_{aim,click}.png`) are the same run rendered. A regression in
/// the ray, the aspect, or the timeline's cursor field moves them together.
#[test]
fn the_mouse_aims_where_the_cursor_points() {
    let scene = repo_path("examples/scenes/verify/m28_pointer.json");
    let timeline = repo_path("examples/scenes/verify/m28_pointer.input.jsonl");

    let at = |steps: &str| {
        let output = engine()
            .arg("simulate")
            .arg(&scene)
            .args(["--steps", steps])
            .arg("--input")
            .arg(&timeline)
            .args(["--entity", "Marker", "--entity", "Held"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        serde_json::from_str::<serde_json::Value>(&stdout_of(&output)).unwrap()
    };
    let position = |report: &serde_json::Value, entity: &str| -> Vec<f64> {
        report["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["entity"] == entity)
            .unwrap()["position"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect()
    };

    // Step 40: the cursor is at (0.2, 0.72) — left of centre and low, so the
    // marker sits left of the camera axis and *nearer* than the origin.
    let aim = at("40");
    let marker = position(&aim, "Marker");
    assert!(marker[0] < -3.0, "left of centre: {marker:?}");
    assert!(marker[2] > 1.0, "and short of the origin: {marker:?}");
    // MouseLeft is held here, but not over the button — so nothing fired.
    // A click is a position on the HUD as much as a button state.
    assert!(
        position(&aim, "Held")[1] < 0.0,
        "a click away from the button is not a press"
    );

    // Step 80: the cursor is over the plate in the bottom-right corner with
    // MouseLeft down, and the script hit-tests it in pixels and fires.
    //
    // Through `screenshot` at the baseline's own size, and that is the point
    // rather than an inconvenience: a HUD element is *pixel*-sized, so which
    // element a cursor is over depends on the frame it is over (M28 §5).
    // `simulate` renders nothing and runs at `Viewport::DEFAULT` — 960x540,
    // the same 16:9 aspect, so it aims the ray identically and misses this
    // 132x26 plate by twelve pixels.
    let shot = std::env::temp_dir().join(format!("engine-m27-{}.png", std::process::id()));
    let output = engine()
        .arg("screenshot")
        .arg(&scene)
        .arg("--out")
        .arg(&shot)
        .args(["--steps", "80"])
        .arg("--input")
        .arg(&timeline)
        .args(["--width", "640", "--height", "360"])
        .output()
        .unwrap();
    // Everything above this line is `simulate`, which renders nothing and so
    // runs anywhere; the pixel-sized half of the claim needs a frame, and a
    // machine with no usable GPU skips it rather than failing.
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
            "screenshot failed for a non-GPU reason: {stderr}"
        );
        eprintln!("skipping the framed half: no usable GPU on this machine");
        return;
    }
    let click: serde_json::Value = serde_json::from_str(&stdout_of(&output)).unwrap();
    let line = click["hud"][0].as_str().unwrap().to_string();
    assert!(line.ends_with("FIRE"), "the press is on the HUD: {line}");

    // And the ray is the same ray at the same aspect: the ground point the
    // frame reports is the one `simulate` put the marker at.
    let marker = position(&at("80"), "Marker");
    let rounded = format!(
        "G {} {}",
        (marker[0] * 10.0).round() / 10.0,
        (marker[2] * 10.0).round() / 10.0
    );
    assert!(
        line.starts_with(&rounded),
        "the aspect is all that matters to the ray: {line} vs {rounded}"
    );
    std::fs::remove_file(&shot).ok();

    // The same timeline with the cursor field ignored would put the marker at
    // the centre of the frame; check the "no --input at all" case does
    // exactly that, since it is the M28 promise that keyboard-era files and
    // no-input runs are unchanged.
    let output = engine()
        .arg("simulate")
        .arg(&scene)
        .args(["--steps", "80", "--entity", "Marker"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: serde_json::Value = serde_json::from_str(&stdout_of(&output)).unwrap();
    let centred = position(&report, "Marker");
    assert!(
        centred[0].abs() < 1e-3,
        "no input means the cursor sits at the centre of the frame: {centred:?}"
    );
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
///
/// **The numbers are a per-platform artifact**, the way a render baseline is a
/// per-adapter one, so they are only checked on the machine class they were
/// blessed on. Eleven thousand steps of a vehicle sim is chaotic and `sin`
/// and `cos` disagree in their last bits between Apple's libm and glibc's:
/// replayed on x86-64 Linux this same timeline parks the car ~53 m from where
/// it parks here. That is not a drivetrain change to debug — it is what makes
/// tree baselines per build profile, arriving in physics. Cross-platform
/// agreement here would mean routing every trig call in the engine and in
/// Rhai through one deterministic libm, which is a milestone, not a fixup.
#[test]
fn the_committed_lap_timeline_drives_the_car_around_the_track() {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        eprintln!(
            "skipping the pinned drive: recorded on aarch64 macOS, and this is {} {}",
            std::env::consts::ARCH,
            std::env::consts::OS
        );
        return;
    }

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
    let (high_bake, _) = bake_at("1800", "high.json");
    let high = baked_position(&high_bake, "Car");
    assert!(
        high[0] > 60.0,
        "mid-drive the car is on the far side: {high:?}"
    );
    assert!(
        high[1] > 7.0,
        "and up on the high part of the circuit: {high:?}"
    );

    // Mid-drive, low side: down at Stavelot, the bottom of the map. The same
    // recording reaching both is what makes the elevation real rather than
    // decorative — the car drove up there and back down on its suspension.
    let (low_bake, _) = bake_at("6600", "low.json");
    let low = baked_position(&low_bake, "Car");
    assert!(
        low[1] < 2.5,
        "the drive descends to the low point too: {low:?}"
    );
    assert!(
        high[1] - low[1] > 5.0,
        "the circuit's elevation is driven, not flat: {high:?} vs {low:?}"
    );

    // After three laps and the braking phase: stopped on the pit straight,
    // a few meters past the start line it just crossed.
    let (end_bake, report) = bake_at("11634", "end.json");
    let end = baked_position(&end_bake, "Car");
    let (dx, dz) = (end[0] - -65.78, end[2] - -37.36);
    let distance = (dx * dx + dz * dz).sqrt();
    assert!(
        distance < 8.0,
        "the drive must park by the start line: {end:?}"
    );
    assert!(
        end[2] < -37.36,
        "having crossed it, not stopped short: {end:?}"
    );

    // The script's HUD is part of the pinned record: parked (speed 0), just
    // across the line onto lap 4, with three completed timed laps behind it
    // (last 63.70 s, best 59.47 s — a lap of this circuit is around a minute).
    // These strings are golden the way traces are: a drivetrain, geometry or
    // timing change shows up here first. They moved in M23 because the road
    // did: the car now drives a continuous ribbon instead of 207 plates, so it
    // carries speed through corners the plate road scrubbed off.
    assert_eq!(
        report["hud"],
        serde_json::json!([
            "SPEED 0 KM/H",
            "LAP 4   TIME 3.42",
            "LAST 63.70   BEST 59.47"
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
    assert_eq!(
        traced[0],
        serde_json::json!({"step": 1, "hud": ["COUNT 0"]})
    );
    assert_eq!(
        traced[2],
        serde_json::json!({"step": 3, "hud": ["COUNT 2"]})
    );
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
        Some((
            path,
            serde_json::from_str::<serde_json::Value>(&stdout_of(&output)).unwrap(),
        ))
    };

    let Some((with_hud, report)) = shot("1", "hud.png") else {
        eprintln!("skipping: no usable GPU on this machine");
        return;
    };
    let (without_hud, plain_report) = shot("0", "plain.png").expect("GPU worked a moment ago");

    assert_eq!(report["hud"], serde_json::json!(["COUNT 0"]), "{report}");
    assert!(
        plain_report.get("hud").is_none(),
        "no steps, no HUD: {plain_report}"
    );
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
    let rate = component("Fire", "ParticleEmitter")["rate"]
        .as_f64()
        .unwrap();
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
        let report: serde_json::Value = serde_json::from_str(stdout_of(&by_time).trim()).unwrap();
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
        let report: serde_json::Value = serde_json::from_str(stdout_of(&at_rest).trim()).unwrap();
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

    let validate = engine()
        .arg("validate")
        .arg(dir.join("scene.json"))
        .output()
        .unwrap();
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

    assert_ne!(
        stepped, unstepped,
        "60 steps of particles must draw something"
    );
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
    assert!(
        stdout_of(&output).is_empty(),
        "stdout must be silent on failure"
    );
    let lines = stderr_lines(&output);
    let codes = codes_of(&lines);
    assert!(
        codes.contains(&"invalid_field_type".to_string()),
        "{codes:?}"
    );
    assert!(
        codes.contains(&"value_out_of_range".to_string()),
        "{codes:?}"
    );
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
        let path =
            std::env::temp_dir().join(format!("engine-m14-{}-{name}.jsonl", std::process::id()));
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
    assert_eq!(
        validate.status.code(),
        Some(0),
        "the baked scene validates: {validate:?}"
    );

    let root: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&baked).unwrap()).unwrap();
    let names: Vec<&str> = root["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"Crate"),
        "the broken entity is gone: {names:?}"
    );
    assert!(
        names.contains(&"Ball"),
        "untouched entities survive: {names:?}"
    );
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
    assert!(
        stdout_of(&output).is_empty(),
        "stdout must be empty on failure"
    );
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
        let path =
            std::env::temp_dir().join(format!("engine-tour-{}-{name}.jsonl", std::process::id()));
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
        .filter_map(|l| Some((l.get("step")?.as_u64()?, l.get("broke")?.as_str()?)))
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
    assert_eq!(
        scripted.0, 601,
        "a scripted break lands the step after the call"
    );
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

/// The tour does not end; it comes round. The director used to clamp its
/// station index at the last one, so past step 900 the segment-local `t` swept
/// 0→1 forever and the camera replayed the finale's own three seconds over and
/// over while the fire, the truck and the daylight all went on moving. The key
/// path is a closed cycle now — six legs, the last of which flies home — so
/// one 1080-step lap puts the camera back on the key it opened with and the
/// stations come round again with the world in the state it reached.
///
/// This is checked through `simulate` rather than a baseline because the
/// second lap has no committed PNG and should not get one: what is pinned is
/// that the camera *moves on*, not what it happens to see.
#[test]
fn the_showcase_tour_keeps_touring_past_its_fifteen_seconds() {
    let scene = repo_path("examples/scenes/showcase_tour.json");
    let camera_at = |steps: &str| -> ([f64; 3], String) {
        let output = engine()
            .arg("simulate")
            .arg(&scene)
            .args(["--steps", steps])
            .args(["--entity", "TourCam"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        let report: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
        let p = &report["entities"][0]["position"];
        let axis = |i: usize| p[i].as_f64().expect("the camera reports a position");
        (
            [axis(0), axis(1), axis(2)],
            report["hud"][0].as_str().unwrap().to_string(),
        )
    };
    let apart = |a: [f64; 3], b: [f64; 3]| {
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    };

    // The opening key, and where the fifteen seconds leave the camera.
    let opening = [-27.0, 7.5, -20.5];
    let (finale, _) = camera_at("900");
    assert!(
        apart(finale, opening) > 40.0,
        "the tour should end its fifteen seconds a long way from where it began: {finale:?}"
    );

    // A lap later it is home, having travelled rather than looped in place.
    let (home, line) = camera_at("1080");
    assert!(
        apart(home, opening) < 0.25,
        "one lap should return the camera to its opening key, not to {home:?}"
    );
    assert_eq!(line, "TOUR LAP 2  06 THE WAY BACK");

    // And the stations themselves come round: 90 steps into the new lap is
    // the forest again, framed exactly as station 01 frames it.
    let (_, line) = camera_at("1170");
    assert_eq!(line, "TOUR LAP 2  01 FOREST");
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
    assert_eq!(
        output.status.code(),
        Some(0),
        "a warning must not fail: {output:?}"
    );
    assert_eq!(codes_of(&stderr_lines(&output)), ["daylight_overrides_sky"]);

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

    let trace_dir = std::env::temp_dir().join(format!("engine-m22-{}", std::process::id()));
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

/// The M23 fixture: a closed circuit as one `Road` entity — asphalt, shoulders,
/// an embankment, edge lines, a dashed centre line fitted to the lap, kerbs on
/// the two corners tight enough to ask for them, and a start line at `v = 0`.
///
/// The render half pins the whole new path at once: the generated ribbon, the
/// surface coordinates its markings are painted in, the kerb spans the CPU
/// hands the shader, and a road drawn in the opaque pass casting a shadow
/// through the unchanged shadow pipeline.
///
/// The rest is the claim that matters more than the picture: **the road is a
/// surface a thing can rest on**. The fixture drops a ball on it, and the ball
/// has to be found sitting on the asphalt — not on the grass 34 m away, which
/// is where it ended up before mesh colliders were given
/// `FIX_INTERNAL_EDGES` and a body resting on coplanar triangles was flung
/// sideways by an edge contact.
#[test]
fn the_m23_road_fixture_pins_markings_and_a_drivable_surface() {
    let scene = repo_path("examples/scenes/verify/m23_road.json");
    let baseline = repo_path("examples/scenes/verify/baselines/m23_road.png");

    // Physics first: it needs no GPU, so it runs on every machine. Bake next
    // to the scene so relative asset paths keep resolving.
    let bake = repo_path(&format!(
        "examples/scenes/verify/.m23-bake-test-{}.json",
        std::process::id()
    ));
    let simulated = engine()
        .arg("simulate")
        .arg(&scene)
        .args(["--steps", "180"])
        .arg("--bake")
        .arg(&bake)
        .output()
        .unwrap();
    assert_eq!(simulated.status.code(), Some(0), "{simulated:?}");

    let baked: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bake).unwrap()).unwrap();
    let _ = std::fs::remove_file(&bake);
    let ball = baked["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "Ball")
        .expect("the fixture has a Ball");
    let position = ball["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "Transform")
        .and_then(|t| t["position"].as_array())
        .expect("a baked Transform")
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect::<Vec<f64>>();

    // Dropped at (-34, 6, 0) onto a road whose surface is 0.2 m up there, with
    // a 0.7 m ball: resting means y ≈ 0.9, and *staying where it landed* means
    // x and z have not run away.
    assert!(
        (position[1] - 0.9).abs() < 0.05,
        "the ball should rest on the road surface at y ≈ 0.9, found {position:?}"
    );
    assert!(
        (position[0] + 34.0).abs() < 0.5 && position[2].abs() < 0.5,
        "the ball should stay where it landed; a body flung along an internal \
         edge ends up off the road entirely: {position:?}"
    );

    let diff = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .args(["--steps", "180"])
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

/// The M40 fixture: a circuit that can only be authored with all five of the
/// milestone's additions at once — banked corners the engine signed itself, a
/// pit lane that widens through `RoadPoint.width`, three roads riding an M22
/// terrain rather than carrying pasted-in heights, a `Junction` where the pit
/// lane meets the paddock road, and grain on the asphalt.
///
/// The render half pins the lot. The physics half pins the one claim a picture
/// cannot make: **the banking has the right sign.** A ball dropped on the west
/// straight, approaching a right-hand corner, has to roll toward the *inside*
/// of that turn and stay on the asphalt. A bank signed the other way rolls it
/// off the outside, which is a circuit that throws the car off at every corner
/// — the exact failure `Road::auto_bank` exists to stop an author making by
/// hand.
#[test]
fn the_m40_track_fixture_pins_banking_width_and_a_junction() {
    let scene = repo_path("examples/scenes/verify/m40_track.json");
    let baseline = repo_path("examples/scenes/verify/baselines/m40_track.png");

    // Physics first: no GPU needed, so it runs on every machine.
    let simulated = engine()
        .arg("simulate")
        .arg(&scene)
        .args(["--steps", "180"])
        .args(["--entity", "Ball"])
        .output()
        .unwrap();
    assert_eq!(simulated.status.code(), Some(0), "{simulated:?}");
    let report: serde_json::Value = serde_json::from_str(stdout_of(&simulated).trim()).unwrap();
    let position = report["entities"][0]["position"]
        .as_array()
        .expect("the ball's position")
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect::<Vec<f64>>();

    // The straight runs down x = -40 toward a corner that turns right, so the
    // inside is +x and the outside is -x.
    assert!(
        position[0] > -40.0,
        "a ball on a correctly banked road rolls toward the inside of the \
         turn (+x from -40); it went the other way, which is the bank signed \
         backwards: {position:?}"
    );
    // 4.5 m of asphalt plus 1.8 m of shoulder each side. Rolling *off* is as
    // wrong as not rolling at all.
    assert!(
        position[0] < -33.7,
        "the ball left the road entirely: {position:?}"
    );
    assert!(
        position[1] > -4.0 && position[1] < 0.0,
        "the ball should be resting on a road that follows the terrain about \
         2.5 m below the origin, not fallen through it: {position:?}"
    );

    // The junction resolved every arm it names. A dropped arm is a hole in the
    // patch, and a hole is exactly what the primitive exists to close.
    let plan = engine().arg("junction-plan").arg(&scene).output().unwrap();
    assert_eq!(plan.status.code(), Some(0), "{plan:?}");
    let plan: serde_json::Value = serde_json::from_str(stdout_of(&plan).trim()).unwrap();
    let arms = plan["arms"].as_array().expect("arms");
    assert_eq!(arms.len(), 3, "the T junction has three arms: {plan}");
    for arm in arms {
        let reach = arm["reach"].as_f64().unwrap();
        assert!(
            (2.0..12.0).contains(&reach),
            "arm {} met the junction {reach} m out, which is not a mouth: {plan}",
            arm["road"]
        );
    }

    // Per-point width reached what the file asked for and no more — the
    // monotone cubic's whole job, one quantity over from the heights.
    let centerline = engine()
        .arg("road-centerline")
        .arg(&scene)
        .args(["--entity", "PitLane"])
        .output()
        .unwrap();
    assert_eq!(centerline.status.code(), Some(0), "{centerline:?}");
    let centerline: serde_json::Value =
        serde_json::from_str(stdout_of(&centerline).trim()).unwrap();
    let widths: Vec<f64> = centerline["points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["width"].as_f64().unwrap())
        .collect();
    let widest = widths.iter().cloned().fold(f64::MIN, f64::max);
    let narrowest = widths.iter().cloned().fold(f64::MAX, f64::min);
    assert!(
        (widest - 13.0).abs() < 1e-3,
        "the pit lane is authored to reach 13 m and must not bulge past it: {widest}"
    );
    assert!(
        (narrowest - 7.0).abs() < 1e-3,
        "and must not pinch below the road's own 7 m: {narrowest}"
    );

    let diff = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .args(["--steps", "180"])
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

// ── engine init / engine agent-guide (distribution) ────────────────────────
//
// The scaffold is what someone gets who installed a binary and has no
// checkout. If it does not validate and render, the first thing a new user
// sees is a failure in code they did not write.

/// A fresh empty directory for one test, removed first so reruns are clean.
fn scratch_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("engine-init-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn init_scaffolds_a_scene_that_validates() {
    let dir = scratch_dir("validates");
    let output = engine().arg("init").arg(&dir).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", stderr_lines(&output));

    let report: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
    assert_eq!(report["created"], dir.display().to_string());
    assert!(
        report["next"].as_array().is_some_and(|n| !n.is_empty()),
        "the result should tell an agent what to run next: {report}"
    );

    for name in [
        "AGENTS.md",
        "CLAUDE.md",
        ".gitignore",
        "first.json",
        "scripts/spin.rhai",
    ] {
        assert!(dir.join(name).exists(), "{name} should have been written");
    }

    // The scene references its script relatively, so this also pins that the
    // scaffold's layout resolves — a scene one directory down would not.
    let validate = engine()
        .arg("validate")
        .arg(dir.join("first.json"))
        .arg("--strict")
        .output()
        .unwrap();
    assert_eq!(
        validate.status.code(),
        Some(0),
        "the scaffolded scene must validate with no warnings: {:?}",
        stderr_lines(&validate)
    );
}

#[test]
fn init_refuses_a_directory_that_already_holds_files() {
    let dir = scratch_dir("occupied");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("CLAUDE.md"), "mine, not the scaffold's").unwrap();

    let output = engine().arg("init").arg(&dir).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stdout_of(&output).is_empty(),
        "failure writes nothing to stdout"
    );
    assert_eq!(codes_of(&stderr_lines(&output)), ["init_target_not_empty"]);
    assert_eq!(
        std::fs::read_to_string(dir.join("CLAUDE.md")).unwrap(),
        "mine, not the scaffold's",
        "refusing must not have overwritten anything"
    );

    let forced = engine()
        .arg("init")
        .arg(&dir)
        .arg("--force")
        .output()
        .unwrap();
    assert_eq!(forced.status.code(), Some(0), "{:?}", stderr_lines(&forced));
    assert!(std::fs::read_to_string(dir.join("CLAUDE.md"))
        .unwrap()
        .contains("AGENTS.md"));
}

#[test]
fn agent_guide_is_documentation_not_a_result() {
    let output = engine().arg("agent-guide").output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stderr.is_empty(),
        "documentation writes nothing to stderr"
    );

    let guide = stdout_of(&output);
    assert!(
        guide.contains("engine screenshot"),
        "the guide must teach the loop"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(&guide).is_err(),
        "agent-guide is markdown, a documented exception to the stdout contract"
    );

    // The binary carries the guide, and `init` writes that same text — one
    // source of truth, so a fix to one cannot miss the other.
    let dir = scratch_dir("guide");
    engine().arg("init").arg(&dir).output().unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(),
        guide
    );
}

// ── M24: the CLI answers questions it already knows ───────────────────────
//
// Four questions an agent asks constantly and could not ask directly. Each of
// these fails against the pre-M24 binary — a test that passes before the change
// tests nothing.

/// A scene with a terrain patch and no collider on it: the height field exists
/// whether or not anything can be dropped onto it.
const TERRAIN: &str = r#"{"name":"ground","entities":[
    {"name":"Cam","components":[
        {"type":"Camera","active":true},
        {"type":"Transform","position":[0.0,5.0,20.0]}]},
    {"name":"Ground","components":[
        {"type":"Transform","position":[0.0,-1.0,0.0],"scale":[180.0,2.0,180.0]},
        {"type":"Terrain","height":6.0,"seed":7}]}
]}"#;

#[test]
fn raycast_takes_a_negative_origin_without_an_equals_sign() {
    // Roughly half the coordinates in any centered scene are negative, and
    // before M24 clap read `-6,20,6` as a flag: `unexpected argument '-6'`,
    // with no `did_you_mean` that could help, because nothing is misspelled.
    let scene = repo_path("examples/scenes/verify/m22_terrain.json");
    let output = engine()
        .args(["raycast"])
        .arg(&scene)
        .args(["--from", "-6,20,6", "--dir", "0,-1,0"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{:?}", stderr_lines(&output));
    let report: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
    assert_eq!(report["hit"]["entity"], "Ground");
    let point = &report["hit"]["point"];
    assert_eq!(point[0].as_f64().unwrap(), -6.0);
    assert_eq!(point[2].as_f64().unwrap(), 6.0);
}

#[test]
fn signed_scalars_parse_everywhere_they_are_taken() {
    // The class, not the instance: every argument that takes a vector or a
    // signed number accepts a leading minus.
    let scene = scene_file("signed", TERRAIN);
    let out = std::env::temp_dir().join(format!("engine-signed-{}.png", std::process::id()));

    for args in [
        vec!["--time", "-1.0"],
        vec!["--time", "-0.5", "--steps", "0"],
    ] {
        let output = engine()
            .arg("screenshot")
            .arg(&scene)
            .arg("--out")
            .arg(&out)
            .args(&args)
            .args(["--width", "64", "--height", "64"])
            .output()
            .unwrap();
        // A GPU-less machine fails later, in the renderer; what is being
        // pinned here is that it is not rejected by the *parser*.
        let codes = codes_of(&stderr_lines(&output));
        assert!(
            !codes.iter().any(|c| c == "invalid_invocation"),
            "{args:?} must parse; got {codes:?}"
        );
    }
}

#[test]
fn list_components_names_one_component() {
    let output = engine()
        .args(["list-components", "--component", "Water"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", stderr_lines(&output));
    assert!(output.stderr.is_empty());

    let schema: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
    assert_eq!(schema["properties"]["type"]["const"], "Water");
    assert!(
        schema["properties"]["segments"].is_object(),
        "a component's own fields are the point of asking"
    );
    // Standalone: the `Wave` definition its `waves` array points at rides along,
    // so the printed document resolves without the one it was lifted out of.
    assert!(schema["$defs"]["Wave"].is_object());

    // Without the flag, byte-identical to what it always printed — the
    // checked-in schema file is that output.
    let whole = engine().arg("list-components").output().unwrap();
    assert_eq!(
        stdout_of(&whole),
        std::fs::read_to_string(repo_path("schemas/component-schema.json")).unwrap(),
        "--component must not have reshaped the default output"
    );
}

#[test]
fn an_unknown_component_query_suggests_the_near_miss() {
    let output = engine()
        .args(["list-components", "--component", "Meterial"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout_of(&output).is_empty(),
        "failure writes nothing to stdout"
    );

    let lines = stderr_lines(&output);
    assert_eq!(codes_of(&lines), ["unknown_component_query"]);
    assert_eq!(lines[0]["did_you_mean"], "Material");
}

#[test]
fn terrain_height_answers_without_a_collider_or_a_raycast() {
    let scene = scene_file("terrain-height", TERRAIN);
    let output = engine()
        .arg("terrain-height")
        .arg(&scene)
        .args(["--at", "-12,8"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{:?}", stderr_lines(&output));
    let report: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
    assert_eq!(report["entity"], "Ground");
    assert_eq!(report["x"].as_f64().unwrap(), -12.0);
    assert_eq!(report["z"].as_f64().unwrap(), 8.0);
    // The patch sits at y = -1 with scale.y = 2 over ±6 m of relief, so the
    // answer is a world coordinate in that band — not the raw field value.
    let height = report["height"].as_f64().unwrap();
    assert!((-13.0..=11.0).contains(&height), "height was {height}");
}

#[test]
fn terrain_height_is_the_sampler_scripts_ask() {
    // M22's central claim is that terrain has exactly one implementation. This
    // pins it across the two ways of asking: a script's world.terrain_height
    // and the CLI must return the same f32, not merely a similar number.
    let scene = scene_file(
        "terrain-sampler",
        &TERRAIN.replace(
            r#"{"name":"Cam","components":["#,
            r#"{"name":"Probe","components":[{"type":"Script","source":"probe.rhai"}]},
    {"name":"Cam","components":["#,
        ),
    );
    std::fs::write(
        scene.parent().unwrap().join("probe.rhai"),
        "fn step(world, step) { world.hud(\"h=\" + world.terrain_height(\"Ground\", -12.0, 8.0)); }\n",
    )
    .unwrap();

    let simulated = engine()
        .arg("simulate")
        .arg(&scene)
        .args(["--steps", "1"])
        .output()
        .unwrap();
    assert_eq!(
        simulated.status.code(),
        Some(0),
        "{:?}",
        stderr_lines(&simulated)
    );
    let report: serde_json::Value = serde_json::from_str(stdout_of(&simulated).trim()).unwrap();
    let from_script: f64 = report["hud"][0]
        .as_str()
        .expect("the script pushes one HUD line")
        .trim_start_matches("h=")
        .parse()
        .unwrap();

    let queried = engine()
        .arg("terrain-height")
        .arg(&scene)
        .args(["--at", "-12,8"])
        .output()
        .unwrap();
    let from_cli: serde_json::Value = serde_json::from_str(stdout_of(&queried).trim()).unwrap();

    assert_eq!(
        from_cli["height"].as_f64().unwrap() as f32,
        from_script as f32,
        "the CLI and the script API must sample the same height field"
    );
}

#[test]
fn terrain_height_names_the_candidates_when_several_patches_exist() {
    let two = TERRAIN.replace(
        r#"{"name":"Ground","components":["#,
        r#"{"name":"Island","components":[
        {"type":"Transform","position":[400.0,0.0,0.0],"scale":[60.0,3.0,60.0]},
        {"type":"Terrain","height":4.0,"seed":2}]},
    {"name":"Ground","components":["#,
    );
    let scene = scene_file("terrain-two", &two);

    let ambiguous = engine()
        .arg("terrain-height")
        .arg(&scene)
        .args(["--at", "0,0"])
        .output()
        .unwrap();
    assert_eq!(ambiguous.status.code(), Some(1));
    let lines = stderr_lines(&ambiguous);
    assert_eq!(codes_of(&lines), ["missing_component"]);
    let message = lines[0]["message"].as_str().unwrap();
    assert!(
        message.contains("Ground") && message.contains("Island"),
        "{message}"
    );

    // Naming one that is not there suggests the near miss, like every other
    // name error in the engine.
    let typo = engine()
        .arg("terrain-height")
        .arg(&scene)
        .args(["--at", "0,0", "--entity", "Grond"])
        .output()
        .unwrap();
    assert_eq!(typo.status.code(), Some(1));
    let lines = stderr_lines(&typo);
    assert_eq!(codes_of(&lines), ["entity_not_found"]);
    assert_eq!(lines[0]["did_you_mean"], "Ground");

    // And naming one that is there answers for that patch.
    let named = engine()
        .arg("terrain-height")
        .arg(&scene)
        .args(["--at", "400,0", "--entity", "Island"])
        .output()
        .unwrap();
    assert_eq!(named.status.code(), Some(0), "{:?}", stderr_lines(&named));
    let report: serde_json::Value = serde_json::from_str(stdout_of(&named).trim()).unwrap();
    assert_eq!(report["entity"], "Island");
}

#[test]
fn inspect_fills_in_the_defaults_the_file_leaves_out() {
    // `{"type": "Material"}` in the file is five values in the engine. Reading
    // the JSON tells you one of them.
    let scene = scene_file(
        "inspect",
        r#"{"name":"defaults","entities":[
    {"name":"Cam","components":[{"type":"Camera","active":true}]},
    {"name":"Cube","components":[
        {"type":"Transform","position":[1.0,2.0,3.0]},
        {"type":"Mesh","asset":"builtin:cube"},
        {"type":"Material","albedo":[0.5,0.5,0.5]}]}
]}"#,
    );

    let output = engine()
        .arg("inspect")
        .arg(&scene)
        .args(["--entity", "Cube"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", stderr_lines(&output));

    let report: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
    let entities = report["entities"].as_array().unwrap();
    assert_eq!(entities.len(), 1, "--entity narrows to one");
    assert_eq!(entities[0]["name"], "Cube");

    let material = entities[0]["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "Material")
        .expect("the entity has a Material");
    // 0.9, not the 0.5 a reader guesses — which is the whole argument for the
    // command: the file says nothing, and everyone fills the silence wrong.
    // (Compared as f32: these are f32 fields widened to JSON doubles.)
    assert_eq!(
        material["roughness"].as_f64().unwrap() as f32,
        0.9,
        "an unwritten field is its default"
    );
    assert_eq!(material["metallic"].as_f64().unwrap(), 0.0);
    assert_eq!(material["alpha"].as_f64().unwrap(), 1.0);

    // The Transform the file omits two thirds of comes back whole, and the
    // resolved placement is reported beside the components.
    assert_eq!(
        entities[0]["transform"]["scale"],
        serde_json::json!([1.0, 1.0, 1.0])
    );
    assert_eq!(
        entities[0]["transform"]["position"],
        serde_json::json!([1.0, 2.0, 3.0])
    );
}

#[test]
fn inspect_reports_every_entity_name_sorted() {
    let scene = scene_file("inspect-all", VALID);
    let output = engine().arg("inspect").arg(&scene).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", stderr_lines(&output));

    let report: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
    let names: Vec<&str> = report["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Cam", "Cube"], "sorted, never archetype order");
    assert_eq!(report["scene"], "ok");

    // An entity with no Transform still reports one: the identity placement is
    // what everything downstream uses for it.
    let cube = &report["entities"][1];
    assert_eq!(
        cube["transform"]["scale"],
        serde_json::json!([1.0, 1.0, 1.0])
    );
}

#[test]
fn inspect_suggests_a_near_miss_entity() {
    let scene = scene_file("inspect-typo", VALID);
    let output = engine()
        .arg("inspect")
        .arg(&scene)
        .args(["--entity", "Cubee"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout_of(&output).is_empty());
    let lines = stderr_lines(&output);
    assert_eq!(codes_of(&lines), ["entity_not_found"]);
    assert_eq!(lines[0]["did_you_mean"], "Cube");
}

// ── M25: reports carry what the command already computed ──────────────────
//
// Two existing stdout objects that computed the answer to the most common
// follow-up question and threw it away. Both additions fail against the
// pre-M25 binary, which reports neither key.

/// A ball dropped onto a fixed floor: something moves, something does not.
const DROP: &str = r#"{"name":"drop","entities":[
    {"name":"Cam","components":[
        {"type":"Camera","active":true},
        {"type":"Transform","position":[0.0,2.0,8.0]}]},
    {"name":"Floor","components":[
        {"type":"Transform","position":[0.0,-1.0,0.0]},
        {"type":"RigidBody","body":"fixed"},
        {"type":"Collider","shape":"cuboid","half_extents":[8.0,0.5,8.0]}]},
    {"name":"Ball","components":[
        {"type":"Transform","position":[0.0,4.0,0.0]},
        {"type":"RigidBody","body":"dynamic"},
        {"type":"Collider","shape":"sphere","radius":0.5}]}
]}"#;

#[test]
fn simulate_says_where_everything_ended_up() {
    // Before M25 the whole report was {contacts, simulated_steps,
    // timestep_hz}: to learn a body's final position an agent wrote a trace
    // and parsed its tail, or baked a scene file and read a Transform back.
    let scene = scene_file("m25-drop", DROP);
    let output = engine()
        .arg("simulate")
        .arg(&scene)
        .args(["--steps", "120"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", stderr_lines(&output));

    let report: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
    let entities = report["entities"]
        .as_array()
        .expect("the report says where things are");

    // The trace's rule for who appears: the dynamic bodies, name-sorted. The
    // fixed floor is not one of them.
    let names: Vec<&str> = entities
        .iter()
        .map(|e| e["entity"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Ball"]);

    let ball = &entities[0];
    let y = ball["position"][1].as_f64().unwrap();
    assert!(
        (-0.6..=0.1).contains(&y),
        "the ball should be resting on the floor, not at {y}"
    );
    assert!(
        ball["linear_velocity"].is_array(),
        "a body reports its velocity"
    );
    assert!(ball["rotation"].is_array());

    // Existing keys keep their meaning: this is an addition, not a reshape.
    assert_eq!(report["simulated_steps"], 120);
    assert_eq!(report["timestep_hz"], 60);
    assert!(report["contacts"].as_u64().is_some());
}

#[test]
fn simulate_entity_reaches_what_the_trace_never_enumerates() {
    // A trace lists dynamic bodies. A fixed floor, a scripted kinematic
    // platform, a camera a chase script drives are all invisible to it — and
    // are exactly the entities an agent asks about.
    let scene = scene_file("m25-named", DROP);
    let output = engine()
        .arg("simulate")
        .arg(&scene)
        .args(["--steps", "30", "--entity", "Floor", "--entity", "Cam"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", stderr_lines(&output));

    let report: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
    let names: Vec<&str> = report["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["entity"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Cam", "Floor"], "named entities, still name-sorted");

    // The camera has no RigidBody, so it reports placement and no velocity.
    let cam = &report["entities"][0];
    assert_eq!(cam["position"], serde_json::json!([0.0, 2.0, 8.0]));
    assert!(cam["linear_velocity"].is_null());
}

#[test]
fn simulate_reports_every_unknown_entity_at_once() {
    let scene = scene_file("m25-typos", DROP);
    let output = engine()
        .arg("simulate")
        .arg(&scene)
        .args(["--steps", "1", "--entity", "Bal", "--entity", "Flor"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout_of(&output).is_empty(),
        "failure writes nothing to stdout"
    );
    let lines = stderr_lines(&output);
    assert_eq!(codes_of(&lines), ["entity_not_found", "entity_not_found"]);
    let suggestions: Vec<&str> = lines
        .iter()
        .map(|l| l["did_you_mean"].as_str().unwrap())
        .collect();
    assert!(
        suggestions.contains(&"Ball") && suggestions.contains(&"Floor"),
        "{suggestions:?}"
    );
}

#[test]
fn a_screenshot_report_says_whether_anything_is_in_the_frame() {
    // `entities_drawn` catches "nothing loaded". It cannot catch "the camera
    // is aimed at nothing", which renders a perfectly correct empty frame —
    // and diagnosing that used to cost an image read.
    let visible = scene_file(
        "m25-visible",
        r#"{"name":"visible","entities":[
    {"name":"Cam","components":[
        {"type":"Camera","active":true},
        {"type":"Transform","position":[0.0,0.0,5.0]}]},
    {"name":"Cube","components":[
        {"type":"Transform"},
        {"type":"Mesh","asset":"builtin:cube"},
        {"type":"Material","albedo":[0.8,0.2,0.2]}]}
]}"#,
    );
    let out = visible.with_file_name("visible.png");
    let output = engine()
        .arg("screenshot")
        .arg(&visible)
        .arg("--out")
        .arg(&out)
        .args(["--width", "160", "--height", "120"])
        .output()
        .unwrap();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
            "screenshot failed for a non-GPU reason: {stderr}"
        );
        eprintln!("skipping: no usable GPU on this machine");
        return;
    }

    let report: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
    let digest = &report["digest"];
    assert!(
        digest["coverage"].as_f64().unwrap() > 0.0,
        "the cube is in shot"
    );
    assert!(digest["mean_luminance"].as_f64().unwrap() > 0.0);
    assert_eq!(
        digest["background"].as_array().unwrap().len(),
        3,
        "the background is reported as sRGB bytes"
    );

    // The same scene shot from behind the camera's own back: one entity
    // drawn, nothing in the frame. The digest is the difference.
    let empty_source = std::fs::read_to_string(&visible)
        .unwrap()
        .replace("[0.0,0.0,5.0]", "[0.0,0.0,-500.0]");
    let empty = visible.with_file_name("empty.json");
    std::fs::write(&empty, empty_source).unwrap();
    let empty_out = visible.with_file_name("empty.png");
    let output = engine()
        .arg("screenshot")
        .arg(&empty)
        .arg("--out")
        .arg(&empty_out)
        .args(["--width", "160", "--height", "120"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", stderr_lines(&output));

    let empty_report: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
    assert_eq!(
        empty_report["entities_drawn"], report["entities_drawn"],
        "the same geometry was submitted both times"
    );
    assert_eq!(
        empty_report["digest"]["coverage"].as_f64().unwrap(),
        0.0,
        "nothing but background reached the frame"
    );
}

#[test]
fn the_digest_is_stable_across_runs_of_an_unchanged_scene() {
    // The trap M25 was planned around: a full-precision mean over a frame
    // would differ in its low digits run to run wherever this adapter's MSAA
    // is nondeterministic (M22), turning a diagnostic into phantom diffs.
    let scene = repo_path("examples/scenes/verify/m16_environment.json");
    let out = std::env::temp_dir().join(format!("engine-digest-{}.png", std::process::id()));

    let mut digests = Vec::new();
    for _ in 0..3 {
        let output = engine()
            .arg("screenshot")
            .arg(&scene)
            .arg("--out")
            .arg(&out)
            .args(["--width", "320", "--height", "180"])
            .output()
            .unwrap();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
                "screenshot failed for a non-GPU reason: {stderr}"
            );
            eprintln!("skipping: no usable GPU on this machine");
            return;
        }
        let report: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
        digests.push(report["digest"].clone());
    }
    assert_eq!(digests[0], digests[1]);
    assert_eq!(digests[1], digests[2]);
}

#[test]
fn a_filmstrip_reports_the_digest_of_the_sheet_it_wrote() {
    let scene = repo_path("examples/scenes/demo_scene.json");
    let out = std::env::temp_dir().join(format!("engine-strip-{}.png", std::process::id()));
    let output = engine()
        .arg("filmstrip")
        .arg(&scene)
        .arg("--out")
        .arg(&out)
        .args([
            "--frames",
            "2",
            "--columns",
            "2",
            "--width",
            "80",
            "--height",
            "60",
        ])
        .output()
        .unwrap();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
            "filmstrip failed for a non-GPU reason: {stderr}"
        );
        eprintln!("skipping: no usable GPU on this machine");
        return;
    }

    let report: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
    assert_eq!(report["frames"], 2, "existing keys keep their meaning");
    assert!(
        report["digest"]["mean_luminance"].as_f64().is_some(),
        "the strip that was written is summarized too"
    );
}

/// `engine import` (M26): a glTF model's materials become files the engine
/// reads, and the images it had embedded become PNGs on disk.
///
/// The whole point is that the result is an *ordinary* scene — so the test's
/// real assertion is that `engine validate` accepts everything the import
/// wrote, references and decodes and all.
#[test]
fn import_writes_material_files_and_the_textures_they_reference() {
    let scene = scene_file(
        "import",
        "{\n  \"name\": \"imported\",\n  \"entities\": []\n}\n",
    );
    let dir = scene.parent().unwrap().to_path_buf();
    let model = repo_path("examples/meshes/textured_quad.gltf");

    let output = engine()
        .arg("import")
        .arg(&model)
        .arg("--into")
        .arg(&scene)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("the report is one JSON object");
    assert_eq!(report["entity"], "textured_quad");
    assert_eq!(report["materials"].as_array().unwrap().len(), 1);
    // Albedo, normal, and the repacked ORM — three files for four glTF
    // textures, because occlusion and metallic-roughness pack into one.
    assert_eq!(report["textures"].as_array().unwrap().len(), 3);

    let material = dir.join("materials/textured_quad_stained_glass.json");
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&material).unwrap()).unwrap();
    // The maps are relative to the *material file*, which is what makes one
    // shareable between scenes in different directories.
    assert_eq!(
        written["albedo_map"],
        "../textures/textured_quad_stained_glass_albedo.png"
    );
    // §11: the importer knows there is a map, so it writes the tint out
    // explicitly rather than leaving the 0.8 default to darken the artist's
    // texture by 20% for a reason nobody would guess.
    assert_eq!(written["albedo"], serde_json::json!([1.0, 1.0, 1.0]));
    // The three volume extensions land on the three M26 fields.
    assert_eq!(written["ior"], 1.5);
    assert_eq!(written["thickness"], 0.4);
    assert_eq!(written["transmission"], 0.8);
    // alphaMode MASK with its cutoff, and the normal texture's scale.
    assert_eq!(written["alpha_cutoff"], 0.4);
    assert_eq!(written["normal_strength"], 0.6);

    // The occlusion repack is lossy and says so rather than picking a winner
    // quietly.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("repacked into one orm_map"), "{stderr}");

    // Everything it wrote is a file the engine reads.
    let validate = engine()
        .arg("validate")
        .arg(&scene)
        .arg(&material)
        .output()
        .unwrap();
    assert!(
        validate.status.success(),
        "the import should validate: {}",
        String::from_utf8_lossy(&validate.stderr)
    );

    // Re-importing writes the same bytes: names come from the material and the
    // model, and textures are deduped by content hash, so nothing accumulates.
    let before: Vec<(std::path::PathBuf, Vec<u8>)> = std::fs::read_dir(dir.join("textures"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    let again = engine()
        .arg("import")
        .arg(&model)
        .arg("--into")
        .arg(&scene)
        .output()
        .unwrap();
    assert!(again.status.success());
    for (path, bytes) in before {
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "re-importing rewrote {} differently",
            path.display()
        );
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

/// M27: the water refraction fixture, from two cameras in one file.
///
/// Both baselines are hard bit-exact pins with no tolerance, which is
/// deliberate and only defensible because the fixture obeys M22's rule — it
/// aims at its subject, with no terrain anywhere in frame. Four consecutive
/// sweeps came back at zero differing pixels.
///
/// The two cameras pin two different halves of the design:
///
/// - `Camera` looks down at the pool, where the bed's grid of bars is the
///   thing refraction acts on. This is the frame that would go wrong if the
///   exit point were stepped along the refracted ray by the view ray's path
///   length instead of solved to the bed's depth — the bars scramble into
///   blocks, which is what the first implementation drew.
/// - `CameraGrazing` looks across it at 8°, where the boulder and the posts
///   stand *in* the water. That is the framing the depth-validated sample
///   exists for: dropping the check moves ~22k pixels of it by up to 99, as
///   the water behind each object drags the object's colour out across itself.
#[test]
fn the_m27_water_refraction_fixture_pins_a_bent_bed_and_a_clean_waterline() {
    let scene = repo_path("examples/scenes/verify/m27_water_refraction.json");

    for (baseline, extra) in [
        ("m27_water_refraction.png", Vec::new()),
        ("m27_water_grazing.png", vec!["--camera", "CameraGrazing"]),
    ] {
        let baseline = repo_path(&format!("examples/scenes/verify/baselines/{baseline}"));
        let diff = engine()
            .arg("diff-render")
            .arg(&scene)
            .arg(&baseline)
            .args(["--steps", "120"])
            .args(&extra)
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

    // The claim the whole milestone rests on: `ior` is the only reason this
    // scene renders differently from the M18 surface. Drop it back to the
    // default and the committed baseline must stop matching — otherwise the
    // pins above would pass just as well with refraction wired to nothing,
    // which is the failure mode a splice makes easy to miss.
    let source = std::fs::read_to_string(&scene).unwrap();
    let unrefracted = source.replace(r#""ior": 1.33"#, r#""ior": 1.0"#);
    assert_ne!(
        source, unrefracted,
        "the fixture must author `ior` explicitly"
    );
    let plain = scene.with_file_name("m27_unrefracted.json");
    std::fs::write(&plain, unrefracted).unwrap();

    let report = engine()
        .arg("diff-render")
        .arg(&plain)
        .arg(repo_path(
            "examples/scenes/verify/baselines/m27_water_refraction.png",
        ))
        .args(["--steps", "120"])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout_of(&report).trim()).unwrap();
    let _ = std::fs::remove_file(&plain);
    assert_eq!(
        parsed["pass"], false,
        "with `ior` back at its default the baseline must not match: {parsed}"
    );
}
// ── Skeletal animation (M30 S0) ───────────────────────────────────────────

/// The milestone's fixture: two copies of the rigged arm, one playing `Wave`.
fn skeletal_scene() -> PathBuf {
    repo_path("examples/scenes/verify/m30_skeletal.json")
}

fn json_stdout(output: &Output) -> serde_json::Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(stdout_of(output).trim()).expect("stdout must be one JSON object")
}

#[test]
fn the_skeletal_fixture_validates() {
    let output = engine()
        .arg("validate")
        .arg(skeletal_scene())
        .arg("--strict")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", stderr_lines(&output));
}

/// The M30 fixture rendered: a rigged arm mid-`Wave` beside an identical one
/// with no player, pinned bit-exactly.
///
/// The two arms are the assertion. They share a file, a mesh and a material,
/// so anything that made *both* wrong — a palette that never reached the GPU,
/// a bind group off by a slot — would still leave them identical; only real
/// skinning makes one bend and the other stand. And the bent arm's shadow
/// bends with it, which is the skinned caster: `shadow.wgsl` reads nothing but
/// the model matrix, so without a second pipeline a walking character casts
/// its rest pose, a wrongness that reads as a renderer bug.
///
/// Aimed at its subject with no terrain in frame (M22's rule), so it carries a
/// hard pin rather than a `diff_args` tolerance. Measured, not assumed: unlike
/// tree and cloud baselines this one renders identically from the debug and
/// release binaries — three joints of slerp is not enough libm to reach a
/// pixel.
#[test]
fn the_m30_skeletal_fixture_pins_a_posed_rig_and_its_shadow() {
    let scene = skeletal_scene();
    let baseline = repo_path("examples/scenes/verify/baselines/m30_skeletal.png");

    let diff = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .arg("--time")
        .arg("0.4")
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

/// The pose is a pure function of (files, time), the M9 property — now on a
/// skinned mesh, where the joints reach the GPU as a uniform rather than the
/// components reaching it as a transform.
#[test]
fn a_skinned_render_is_a_pure_function_of_the_file_and_the_clock() {
    let dir = std::env::temp_dir().join(format!("engine-m30-skin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let shot = |name: &str, time: &str| {
        let out = dir.join(name);
        let output = engine()
            .arg("screenshot")
            .arg(skeletal_scene())
            .arg("--out")
            .arg(&out)
            .arg("--width")
            .arg("160")
            .arg("--height")
            .arg("90")
            .arg("--time")
            .arg(time)
            .output()
            .unwrap();
        if !output.status.success() {
            return None;
        }
        Some(std::fs::read(&out).unwrap())
    };

    let Some(first) = shot("a.png", "0.4") else {
        eprintln!("skipping render determinism: no usable GPU on this machine");
        std::fs::remove_dir_all(&dir).ok();
        return;
    };
    let again = shot("b.png", "0.4").unwrap();
    let elsewhere = shot("c.png", "0.9").unwrap();

    assert_eq!(first, again, "same file, same time, same bytes");
    assert_ne!(
        first, elsewhere,
        "a different time has to pose the rig differently, or the clock is \
         not reaching the palette at all"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `world.joint_position` / `world.joint_transform`: the two read-only getters
/// that make a rig reachable from gameplay (M30 §8).
///
/// The test is the design's own worked example — parent a prop to a hand — and
/// it asserts the two things a wrong implementation would still satisfy
/// separately: the prop is *not* at the rig's origin (so a palette really was
/// applied) and it *moves between steps* (so the clock reaches it).
#[test]
fn a_script_can_hang_a_prop_off_a_joint() {
    let dir = repo_path("examples/meshes");
    let script = dir.join("_m30_torch.rhai");
    std::fs::write(
        &script,
        r#"fn step(world, step) {
            let p = world.joint_position("Arm", "Hand");
            world.set_position("Torch", p[0], p[1], p[2]);
            let t = world.joint_transform("Arm", "Hand");
            world.hud("pitch " + t[3]);
        }"#,
    )
    .unwrap();
    let path = dir.join("_m30_joint_script.json");
    std::fs::write(
        &path,
        r#"{"name":"s","entities":[
            {"name":"Cam","components":[{"type":"Camera","active":true}]},
            {"name":"Arm","components":[
                {"type":"Mesh","asset":"rigged_arm.gltf"},
                {"type":"AnimationPlayer","clip":"rigged_arm.gltf#Wave","looping":true},
                {"type":"Script","source":"_m30_torch.rhai"}
            ]},
            {"name":"Torch","components":[
                {"type":"Transform"},
                {"type":"Mesh","asset":"builtin:sphere"}
            ]}
        ]}"#,
    )
    .unwrap();

    let at = |steps: &str| {
        let output = engine()
            .arg("simulate")
            .arg(&path)
            .arg("--steps")
            .arg(steps)
            .arg("--entity")
            .arg("Torch")
            .output()
            .unwrap();
        json_stdout(&output)
    };
    let early = at("6");
    let later = at("30");

    let position = |report: &serde_json::Value| -> Vec<f64> {
        report["entities"][0]["position"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect()
    };
    let (a, b) = (position(&early), position(&later));

    // The rig's root is at the origin and the hand is 2 m up it, so a prop
    // that landed at the origin means no palette was applied at all.
    assert!(a[1] > 0.5, "the hand is well above the ground, got {a:?}");
    assert_ne!(
        a, b,
        "the clip has to move the hand between two step counts"
    );
    // And `joint_transform` reports an orientation, not just a place.
    let pitch = later["hud"][0].as_str().unwrap();
    assert!(pitch.starts_with("pitch "), "got {pitch:?}");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&script);
}

/// A mistyped joint is a located runtime error with a suggestion, matching
/// `world.key` — not a silent identity transform.
#[test]
fn a_mistyped_joint_name_is_a_located_error_with_a_suggestion() {
    let dir = repo_path("examples/meshes");
    let script = dir.join("_m30_typo.rhai");
    std::fs::write(
        &script,
        "fn step(world, step) {\n    world.joint_position(\"Arm\", \"Hnad\");\n}",
    )
    .unwrap();
    let path = dir.join("_m30_joint_typo.json");
    std::fs::write(
        &path,
        r#"{"name":"s","entities":[
            {"name":"Cam","components":[{"type":"Camera","active":true}]},
            {"name":"Arm","components":[
                {"type":"Mesh","asset":"rigged_arm.gltf"},
                {"type":"Script","source":"_m30_typo.rhai"}
            ]}
        ]}"#,
    )
    .unwrap();

    let output = engine()
        .arg("simulate")
        .arg(&path)
        .arg("--steps")
        .arg("1")
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&script);

    assert_eq!(output.status.code(), Some(1));
    let lines = stderr_lines(&output);
    assert!(codes_of(&lines).contains(&"script_runtime_error".to_string()));
    let message = lines[0]["message"].as_str().unwrap();
    assert!(
        message.contains("did you mean \\\"Hand\\\"") || message.contains("did you mean \"Hand\""),
        "the error must suggest the real joint: {message}"
    );
    assert_eq!(lines[0]["line"], 2, "and point at the script line");
}

#[test]
fn list_joints_reports_a_rig_out_of_a_gltf_directly() {
    let output = engine()
        .arg("list-joints")
        .arg(repo_path("examples/meshes/rigged_arm.gltf"))
        .output()
        .unwrap();
    let report = json_stdout(&output);
    let rig = &report["rigs"][0];

    assert_eq!(rig["skin"], "ArmRig");
    assert_eq!(rig["joint_count"], 3);
    // The skin's own order, carrying its index — a joint's index is written
    // into the vertex data, so it is a fact about the asset.
    let names: Vec<&str> = rig["joints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Shoulder", "Elbow", "Hand"]);
    assert_eq!(rig["joints"][2]["index"], 2);
    assert_eq!(rig["joints"][2]["parent"], 1);
    assert_eq!(rig["joints"][2]["parent_name"], "Elbow");
    assert_eq!(rig["joints"][0]["parent"], serde_json::Value::Null);
    // No `--time`: the rest pose, and no `time` key claiming otherwise.
    assert!(rig.get("time").is_none());
}

#[test]
fn list_joints_needs_no_collider_and_no_gpu_to_say_where_a_hand_is() {
    // The claim the milestone makes about itself: motion verified without a
    // pixel. The hand is somewhere different at t=0.5 than at rest, and the
    // arm that plays no clip has not moved at all.
    let at = |time: &str| -> serde_json::Value {
        let output = engine()
            .arg("list-joints")
            .arg(skeletal_scene())
            .arg("--time")
            .arg(time)
            .output()
            .unwrap();
        json_stdout(&output)
    };

    let hand_of = |report: &serde_json::Value, entity: &str| -> [f64; 3] {
        let rig = report["rigs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["entity"] == entity)
            .unwrap_or_else(|| panic!("no rig for {entity}"));
        let joint = rig["joints"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["name"] == "Hand")
            .unwrap();
        let p = joint["world"]["position"].as_array().unwrap();
        [
            p[0].as_f64().unwrap(),
            p[1].as_f64().unwrap(),
            p[2].as_f64().unwrap(),
        ]
    };

    let rest = hand_of(&at("0"), "Arm");
    let bent = hand_of(&at("0.5"), "Arm");
    let back = hand_of(&at("1"), "Arm");

    assert!(
        (bent[2] - rest[2]).abs() > 0.5 && bent[1] < rest[1],
        "the hand did not swing: {rest:?} -> {bent:?}"
    );
    assert!(
        (back[1] - rest[1]).abs() < 1e-4 && (back[2] - rest[2]).abs() < 1e-4,
        "the clip did not return: {back:?}"
    );

    // The entity's own Transform places the rig — glTF ignores the skinned
    // mesh node's transform, so nothing out of the file competes with it.
    assert!(
        (rest[0] - -1.1).abs() < 1e-4,
        "Arm is not where the scene put it"
    );

    // The arm with no player stays at rest at every time.
    assert_eq!(hand_of(&at("0"), "Rest"), hand_of(&at("0.5"), "Rest"));
}

#[test]
fn list_joints_narrows_to_one_entity_and_suggests_on_a_typo() {
    let output = engine()
        .arg("list-joints")
        .arg(skeletal_scene())
        .arg("--entity")
        .arg("Arm")
        .output()
        .unwrap();
    let report = json_stdout(&output);
    assert_eq!(report["rigs"].as_array().unwrap().len(), 1);
    assert_eq!(report["rigs"][0]["entity"], "Arm");
    assert_eq!(report["rigs"][0]["clip"], "Wave");

    let output = engine()
        .arg("list-joints")
        .arg(skeletal_scene())
        .arg("--entity")
        .arg("Arn")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "stdout must be empty on failure");
    let lines = stderr_lines(&output);
    assert_eq!(codes_of(&lines), ["unknown_entity"]);
    assert_eq!(lines[0]["did_you_mean"], "Arm");
}

#[test]
fn list_joints_takes_a_negative_time() {
    // M24 put `allow_hyphen_values` on the class of signed arguments; a new
    // one joins it rather than teaching the guide to write `--time=`.
    let output = engine()
        .arg("list-joints")
        .arg(skeletal_scene())
        .arg("--time")
        .arg("-0.5")
        .output()
        .unwrap();
    let report = json_stdout(&output);
    assert_eq!(report["rigs"][0]["time"], -0.5);
    // The arm's player loops a one-second clip, so the pose sampled is the
    // wrap — reported beside the time asked for rather than in place of it.
    assert_eq!(report["rigs"][0]["clip_time"], 0.5);
}

#[test]
fn list_animations_reads_a_gltf_and_names_the_channel_it_ignores() {
    let output = engine()
        .arg("list-animations")
        .arg(repo_path("examples/meshes/rigged_arm.gltf"))
        .output()
        .unwrap();
    let report = json_stdout(&output);

    let names: Vec<&str> = report["clips"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Wave", "Sway"]);

    let wave = &report["clips"][0];
    assert_eq!(wave["kind"], "skeletal");
    assert_eq!(wave["duration"], 1.0);

    // `Marker` is a node in the scene that is in no skin: glTF allows the
    // channel, sampling ignores it, and the report names it. An ignored
    // channel nothing reports is invisible.
    let marker = wave["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["node_name"] == "Marker")
        .expect("the ignored channel is reported");
    assert_eq!(marker["joint"], serde_json::Value::Null);
    assert_eq!(marker["sampled"], false);

    let elbow = wave["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["node_name"] == "Elbow")
        .unwrap();
    assert_eq!(elbow["joint"], 1);
    assert_eq!(elbow["sampled"], true);
    assert_eq!(elbow["property"], "rotation");
    assert_eq!(elbow["interpolation"], "linear");
}

#[test]
fn list_animations_on_a_scene_reports_both_kinds_of_clip() {
    let output = engine()
        .arg("list-animations")
        .arg(skeletal_scene())
        .output()
        .unwrap();
    let report = json_stdout(&output);
    let clips = report["clips"].as_array().unwrap();
    assert_eq!(clips.len(), 1, "only the arm plays a clip");
    assert_eq!(clips[0]["kind"], "skeletal");
    assert_eq!(clips[0]["entity"], "Arm");

    // The M9 property-clip fixture still reports as it always did, now
    // saying which kind it is.
    let output = engine()
        .arg("list-animations")
        .arg(repo_path("examples/scenes/verify/m9_spin.json"))
        .output()
        .unwrap();
    let report = json_stdout(&output);
    assert_eq!(report["clips"][0]["kind"], "property");
    assert!(report["clips"][0]["tracks"].is_array());
}

#[test]
fn a_gltf_clip_reference_without_a_fragment_is_an_error_not_a_guess() {
    // Defaulting to the only clip in the file is friendlier right up until
    // someone exports a second one, at which point which clip plays changes
    // silently.
    let scene = format!(
        r#"{{"name":"s","entities":[
            {{"name":"Cam","components":[{{"type":"Camera","active":true}}]}},
            {{"name":"Arm","components":[
                {{"type":"Mesh","asset":"{arm}"}},
                {{"type":"AnimationPlayer","clip":"{arm}"}}
            ]}}
        ]}}"#,
        arm = repo_path("examples/meshes/rigged_arm.gltf")
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
            .replace('\\', "/")
    );
    // An absolute path is its own error, so use a relative one: the scene
    // goes next to the asset instead.
    let dir = repo_path("examples/meshes");
    let path = dir.join("_m30_fragment_check.json");
    let relative = scene.replace(
        &repo_path("examples/meshes/rigged_arm.gltf")
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
            .replace('\\', "/"),
        "rigged_arm.gltf",
    );
    std::fs::write(&path, relative).unwrap();

    let output = engine().arg("validate").arg(&path).output().unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(output.status.code(), Some(1));
    let codes = codes_of(&stderr_lines(&output));
    assert!(
        codes.contains(&"clip_needs_fragment".to_string()),
        "got {codes:?}"
    );
}

#[test]
fn a_skeletal_player_must_name_its_entitys_own_mesh() {
    let dir = repo_path("examples/meshes");
    let path = dir.join("_m30_mismatch_check.json");
    std::fs::write(
        &path,
        r#"{"name":"s","entities":[
            {"name":"Cam","components":[{"type":"Camera","active":true}]},
            {"name":"Arm","components":[
                {"type":"Mesh","asset":"pyramid.gltf"},
                {"type":"AnimationPlayer","clip":"rigged_arm.gltf#Wave"}
            ]}
        ]}"#,
    )
    .unwrap();

    let output = engine().arg("validate").arg(&path).output().unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(output.status.code(), Some(1));
    let codes = codes_of(&stderr_lines(&output));
    assert!(
        codes.contains(&"skeletal_player_mesh_mismatch".to_string()),
        "got {codes:?}"
    );
}

#[test]
fn an_unknown_clip_fragment_suggests_a_real_one() {
    let dir = repo_path("examples/meshes");
    let path = dir.join("_m30_unknown_clip_check.json");
    std::fs::write(
        &path,
        r#"{"name":"s","entities":[
            {"name":"Cam","components":[{"type":"Camera","active":true}]},
            {"name":"Arm","components":[
                {"type":"Mesh","asset":"rigged_arm.gltf"},
                {"type":"AnimationPlayer","clip":"rigged_arm.gltf#Wav"}
            ]}
        ]}"#,
    )
    .unwrap();

    let output = engine().arg("validate").arg(&path).output().unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(output.status.code(), Some(1));
    let lines = stderr_lines(&output);
    let unknown = lines
        .iter()
        .find(|l| l["error"] == "unknown_clip")
        .unwrap_or_else(|| panic!("got {:?}", codes_of(&lines)));
    assert_eq!(unknown["did_you_mean"], "Wave");
}

#[test]
fn a_skeletal_player_on_an_unrigged_file_says_so() {
    let dir = repo_path("examples/meshes");
    let path = dir.join("_m30_no_skin_check.json");
    std::fs::write(
        &path,
        r#"{"name":"s","entities":[
            {"name":"Cam","components":[{"type":"Camera","active":true}]},
            {"name":"P","components":[
                {"type":"Mesh","asset":"pyramid.gltf"},
                {"type":"AnimationPlayer","clip":"pyramid.gltf#Wave"}
            ]}
        ]}"#,
    )
    .unwrap();

    let output = engine().arg("validate").arg(&path).output().unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(output.status.code(), Some(1));
    let codes = codes_of(&stderr_lines(&output));
    assert!(
        codes.contains(&"mesh_has_no_skin".to_string()),
        "got {codes:?}"
    );
}

// ── The UI system (M31) ───────────────────────────────────────────────────

fn ui_scene() -> PathBuf {
    repo_path("examples/scenes/verify/m31_ui.json")
}

fn ui_timeline() -> PathBuf {
    repo_path("examples/scenes/verify/m31_ui.input.jsonl")
}

#[test]
fn the_ui_fixture_validates() {
    let output = engine()
        .arg("validate")
        .arg(ui_scene())
        .arg("--strict")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", stderr_lines(&output));
}

/// `engine ui-layout` publishes the rectangle the renderer draws and the hit
/// test uses, which is the whole point of the command: an agent authoring a
/// menu cannot see it move.
///
/// The assertions are about *relationships*, not coordinates, so the fixture
/// stays re-authorable: the hugging panel is exactly its column, the column's
/// children stack in file order (not draw order — the class sort would put
/// both buttons above both labels), and only the two buttons are interactive.
#[test]
fn ui_layout_reports_the_tree_it_laid_out() {
    let report = json_stdout(
        &engine()
            .arg("ui-layout")
            .arg(ui_scene())
            .arg("--width")
            .arg("640")
            .arg("--height")
            .arg("360")
            .output()
            .unwrap(),
    );
    assert_eq!(report["viewport"], serde_json::json!([640, 360]));

    let of = |name: &str| {
        report["elements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["entity"] == name)
            .unwrap_or_else(|| panic!("no element named {name}"))
            .clone()
    };
    let rect = |name: &str| {
        let e = of(name);
        let r = e["rect"].as_array().unwrap().clone();
        (
            r[0].as_i64().unwrap(),
            r[1].as_i64().unwrap(),
            r[2].as_i64().unwrap(),
            r[3].as_i64().unwrap(),
        )
    };

    // Name-sorted, the `simulate --entity` contract.
    let names: Vec<&str> = report["elements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["entity"].as_str().unwrap())
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "elements are name-sorted");

    // The backdrop stretches to the whole frame.
    assert_eq!(rect("Dim"), (0, 0, 640, 360));

    // Hug sizing: the panel *is* its column, and the nine-sliced frame
    // stretches over exactly that. Three entities, one rectangle — which is
    // the thing that cannot be expressed with hand-computed offsets.
    assert_eq!(rect("Menu"), rect("Column"));
    assert_eq!(rect("Menu"), rect("Frame"));

    // Flow order is file order: title, body, then the two buttons, top to
    // bottom. Draw order sorts panels under text, and using that ordering for
    // the flow would stack both buttons above both labels.
    let ys = |name: &str| rect(name).1;
    assert!(ys("Title") < ys("Body"), "title above body");
    assert!(ys("Body") < ys("Resume"), "body above the first button");
    assert!(ys("Resume") < ys("Quit"), "first button above the second");

    // Both buttons span the column's content width, because they stretch on
    // the cross axis; the title does not.
    assert_eq!(rect("Resume").2, rect("Quit").2);
    assert_eq!(rect("Resume").2, rect("Body").2);

    // Only the buttons are interactive, and every element is visible.
    for element in report["elements"].as_array().unwrap() {
        let name = element["entity"].as_str().unwrap();
        let expected = name == "Resume" || name == "Quit";
        assert_eq!(element["interactive"], expected, "{name}");
        assert_eq!(element["visible"], true, "{name}");
    }

    // Unknown names are reported all at once, with a suggestion.
    let bad = engine()
        .arg("ui-layout")
        .arg(ui_scene())
        .arg("--entity")
        .arg("Quitt")
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(1));
    let error = &stderr_lines(&bad)[0];
    assert_eq!(error["error"], "entity_not_found");
    assert_eq!(error["did_you_mean"], "Quit");
}

/// The loop `ui-layout` exists to close, closed in a test: take the reported
/// rectangle, turn it into the *fraction* a timeline carries, and confirm the
/// click lands.
///
/// This is the one place the pixel report and the fractional cursor have to
/// agree, and they are computed by different code — the report by the layout
/// engine, the cursor by M28's timeline parser through `Pointer`.
#[test]
fn a_cursor_derived_from_the_reported_rect_hits_that_element() {
    let (width, height) = (640u32, 360u32);
    let report = json_stdout(
        &engine()
            .arg("ui-layout")
            .arg(ui_scene())
            .arg("--width")
            .arg(width.to_string())
            .arg("--height")
            .arg(height.to_string())
            .arg("--entity")
            .arg("Quit")
            .output()
            .unwrap(),
    );
    let rect = report["elements"][0]["rect"].as_array().unwrap().clone();
    let (x, y, w, h) = (
        rect[0].as_f64().unwrap(),
        rect[1].as_f64().unwrap(),
        rect[2].as_f64().unwrap(),
        rect[3].as_f64().unwrap(),
    );

    // The centre of the button, as a fraction of the frame, quantized the way
    // a recorded timeline is.
    let fx = ((x + w / 2.0) / f64::from(width) * 1000.0).round() / 1000.0;
    let fy = ((y + h / 2.0) / f64::from(height) * 1000.0).round() / 1000.0;

    // The committed timeline must already aim there — if this fails, the
    // fixture's layout moved and the timeline needs re-deriving.
    let timeline = std::fs::read_to_string(ui_timeline()).unwrap();
    let last = timeline.lines().last().unwrap();
    let last: serde_json::Value = serde_json::from_str(last).unwrap();
    assert_eq!(
        last["cursor"],
        serde_json::json!([fx, fy]),
        "the timeline's final cursor should be the centre of Quit"
    );
    assert_eq!(last["held"], serde_json::json!(["MouseLeft"]));
}

/// The fixture rendered: a menu over a 3D scene with the second button held
/// down, pinned bit-exactly.
///
/// The pressed button is the assertion. It is the state hardest to reach and
/// the one nothing else pins: it requires the timeline's cursor to have landed
/// on the right rectangle, the press capture to have survived from the step it
/// started on, and the press tint to have been multiplied into the panel's own
/// colour before the rasterizer ever saw it.
///
/// Aimed at its subject with no terrain in frame (M22's rule), so it carries a
/// hard pin rather than a `diff_args` tolerance.
#[test]
fn the_m31_ui_fixture_pins_a_pressed_button_over_a_3d_scene() {
    let baseline = repo_path("examples/scenes/verify/baselines/m31_ui.png");
    let diff = engine()
        .arg("diff-render")
        .arg(ui_scene())
        .arg(&baseline)
        .arg("--steps")
        .arg("30")
        .arg("--input")
        .arg(ui_timeline())
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

/// Adding a `HudInteract` must move no pixel until a cursor arrives on it:
/// the tints default to `[1, 1, 1]`, so an untouched button renders exactly as
/// it would with no interaction in the scene at all.
///
/// Tested against `--steps 0`, which never runs the simulation and so never
/// touches the interaction state, versus a full run whose cursor is parked in
/// a corner. The fixture is otherwise static — no particles, no water, no
/// daylight — so the two frames may differ only in what the pointer did.
///
/// (The centre of the frame, which is where M28 puts an absent cursor, is over
/// the first button in this fixture — so "no `--input`" is emphatically *not*
/// the untouched case here, and using it as one is the mistake this comment
/// exists to stop the next person repeating.)
#[test]
fn an_untouched_button_renders_as_if_it_had_no_interact() {
    let dir = std::env::temp_dir().join(format!("engine-m31-untouched-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // A timeline that parks the cursor in the corner, clear of the menu.
    let away = dir.join("away.input.jsonl");
    std::fs::write(
        &away,
        "{\"step\": 0, \"held\": [], \"cursor\": [0.02, 0.02]}\n",
    )
    .unwrap();

    let shot = |name: &str, steps: &str, input: Option<&std::path::Path>| {
        let out = dir.join(name);
        let mut command = engine();
        command
            .arg("screenshot")
            .arg(ui_scene())
            .arg("--out")
            .arg(&out)
            .arg("--steps")
            .arg(steps)
            .arg("--width")
            .arg("640")
            .arg("--height")
            .arg("360");
        if let Some(input) = input {
            command.arg("--input").arg(input);
        }
        (command.output().unwrap(), out)
    };

    let (ran, away_png) = shot("away.png", "30", Some(&away));
    if !ran.status.success() {
        let stderr = String::from_utf8_lossy(&ran.stderr);
        assert!(
            stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
            "screenshot failed for a non-GPU reason: {stderr}"
        );
        eprintln!("skipping: no usable GPU on this machine");
        return;
    }
    let (rest, rest_png) = shot("rest.png", "0", None);
    assert!(rest.status.success());

    assert_eq!(
        std::fs::read(&away_png).unwrap(),
        std::fs::read(&rest_png).unwrap(),
        "a pointer over no interactive element tints nothing"
    );
}

/// The other half of the same claim: a pointer that *is* over a button changes
/// the frame. Without this, the test above would pass just as well if tinting
/// were never wired up at all.
#[test]
fn a_hovered_button_is_a_different_frame_from_an_untouched_one() {
    let dir = std::env::temp_dir().join(format!("engine-m31-hover-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, cursor: &str, held: &str| {
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!("{{\"step\": 0, \"held\": [{held}], \"cursor\": {cursor}}}\n"),
        )
        .unwrap();
        path
    };
    let away = write("away.jsonl", "[0.02, 0.02]", "");
    // The centre of Quit, per `engine ui-layout`.
    let over = write("over.jsonl", "[0.5, 0.653]", "");
    let down = write("down.jsonl", "[0.5, 0.653]", "\"MouseLeft\"");

    let shot = |name: &str, input: &std::path::Path| {
        let out = dir.join(name);
        let output = engine()
            .arg("screenshot")
            .arg(ui_scene())
            .arg("--out")
            .arg(&out)
            .arg("--steps")
            .arg("30")
            .arg("--input")
            .arg(input)
            .arg("--width")
            .arg("640")
            .arg("--height")
            .arg("360")
            .output()
            .unwrap();
        (output, out)
    };

    let (first, away_png) = shot("away.png", &away);
    if !first.status.success() {
        let stderr = String::from_utf8_lossy(&first.stderr);
        assert!(
            stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
            "screenshot failed for a non-GPU reason: {stderr}"
        );
        eprintln!("skipping: no usable GPU on this machine");
        return;
    }
    let (_, over_png) = shot("over.png", &over);
    let (_, down_png) = shot("down.png", &down);

    let away_bytes = std::fs::read(&away_png).unwrap();
    let over_bytes = std::fs::read(&over_png).unwrap();
    let down_bytes = std::fs::read(&down_png).unwrap();
    assert_ne!(away_bytes, over_bytes, "hover must brighten the button");
    assert_ne!(over_bytes, down_bytes, "press must differ from hover");
}

// ── Locomotion and foot planting (M32) ────────────────────────────────────

/// The milestone's fixture: two identical walkers crossing one slope, and the
/// only difference between them is a `FootPlant`.
fn locomotion_scene() -> PathBuf {
    repo_path("examples/scenes/verify/m32_locomotion.json")
}

#[test]
fn the_locomotion_fixture_validates() {
    let output = engine()
        .arg("validate")
        .arg(locomotion_scene())
        .arg("--strict")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", stderr_lines(&output));
}

/// The M32 fixture rendered: two walkers mid-stride on a hillside, one with
/// its feet planted and one without, pinned bit-exactly.
///
/// The two walkers are the assertion, M30's fixture logic reused — they share
/// a file, a mesh and a clip, so anything that made both wrong would leave
/// them identical; only real planting puts one pair of feet on the slope while
/// the other pair sinks into it.
///
/// It renders at `samples: 1`, and that is deliberate rather than incidental:
/// the fixture needs terrain in frame, which M22 measured as the end of this
/// adapter's bit-exactness at `samples: 4`. M29's meadow hit the same wall and
/// settled it the same way. Measured, not assumed — four consecutive renders
/// of this scene are one image.
#[test]
fn the_m32_locomotion_fixture_pins_planted_feet() {
    let scene = locomotion_scene();
    let baseline = repo_path("examples/scenes/verify/baselines/m32_locomotion.png");

    let diff = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .arg("--steps")
        .arg("45")
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

/// The milestone's claim, proved without a pixel — M30's half of the story,
/// which is the half this engine cares about most.
///
/// A planted ankle's world Y is the terrain height under it plus the foot's
/// `sole`, at any moment of the clip; the unplanted twin's is whatever the
/// animator left it at, which on a slope is inside the hill. Both facts come
/// out of `engine list-joints` and `engine terrain-height`, neither of which
/// renders anything.
#[test]
fn a_planted_ankle_stands_on_the_ground_and_an_unplanted_one_does_not() {
    let scene = locomotion_scene();

    let joints = |entity: &str, time: &str| -> serde_json::Value {
        let output = engine()
            .arg("list-joints")
            .arg(&scene)
            .arg("--entity")
            .arg(entity)
            .arg("--time")
            .arg(time)
            .output()
            .unwrap();
        json_stdout(&output)["rigs"][0].clone()
    };
    let ground_at = |x: f64, z: f64| -> f64 {
        let output = engine()
            .arg("terrain-height")
            .arg(&scene)
            .arg("--at")
            .arg(format!("{x},{z}"))
            .output()
            .unwrap();
        json_stdout(&output)["height"].as_f64().unwrap()
    };
    let ankle = |rig: &serde_json::Value, name: &str| -> (f64, f64, f64) {
        let joint = rig["joints"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["name"] == name)
            .unwrap_or_else(|| panic!("no joint {name} in {rig}"));
        let p = joint["world"]["position"].as_array().unwrap();
        (
            p[0].as_f64().unwrap(),
            p[1].as_f64().unwrap(),
            p[2].as_f64().unwrap(),
        )
    };

    // The sole offset the fixture authors, in metres.
    const SOLE: f64 = 0.09;
    let mut off_ground = 0;
    for time in ["0.0", "0.25", "0.5", "0.75"] {
        let planted = joints("Planted", time);
        let loose = joints("Loose", time);
        for foot in ["FootL", "FootR"] {
            let (x, y, z) = ankle(&planted, foot);
            let wanted = ground_at(x, z) + SOLE;
            assert!(
                (y - wanted).abs() < 2e-3,
                "at t={time} the planted {foot} is at y={y}, the ground under it \
                 plus its sole is {wanted}"
            );

            let (lx, ly, lz) = ankle(&loose, foot);
            if (ly - (ground_at(lx, lz) + SOLE)).abs() > 2e-2 {
                off_ground += 1;
            }
        }
    }
    assert!(
        off_ground >= 4,
        "the unplanted walker's feet should mostly *not* be on the ground — \
         if they were, the fixture would prove nothing ({off_ground} of 8 were off)"
    );
}

/// The other half: a stride-driven clip advances with the ground covered, so a
/// planted foot stays where it was put while it is the one bearing weight.
///
/// This is foot slide as a number, which is what the milestone is actually
/// about. With `stride` measured off the clip the planted ankle moves under a
/// centimetre a step through stance; with the clock driving the clip instead,
/// the same foot travels several times that.
#[test]
fn a_stride_driven_walk_does_not_slide_its_feet() {
    // Where the planted foot is after `steps` steps of the real simulation.
    // `list-joints --steps` runs the locomotion system first, which is the
    // only way to ask a stride-driven rig anything: its phase lives in the
    // world the run leaves behind, not in the file's authored pose.
    let foot_after = |steps: u32| -> Vec<(f64, f64, f64)> {
        let output = engine()
            .arg("list-joints")
            .arg(locomotion_scene())
            .arg("--entity")
            .arg("Planted")
            .arg("--steps")
            .arg(steps.to_string())
            .output()
            .unwrap();
        let rig = json_stdout(&output)["rigs"][0].clone();
        ["FootL", "FootR"]
            .iter()
            .map(|name| {
                let joint = rig["joints"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|j| &j["name"] == name)
                    .unwrap();
                let p = joint["world"]["position"].as_array().unwrap();
                (
                    p[0].as_f64().unwrap(),
                    p[1].as_f64().unwrap(),
                    p[2].as_f64().unwrap(),
                )
            })
            .collect()
    };

    // Two steps apart: the walker covers 1/30 m at its scripted 1 m/s, and the
    // foot bearing weight should have travelled essentially none of it.
    let before = foot_after(24);
    let after = foot_after(26);
    let slip = before
        .iter()
        .zip(&after)
        .map(|(a, b)| ((a.0 - b.0).powi(2) + (a.2 - b.2).powi(2)).sqrt())
        .fold(f64::INFINITY, f64::min);
    assert!(
        slip < 0.012,
        "the planted foot slid {slip} m over two steps; a stance foot should stay put"
    );
}

// ── The baselines the sweep used to be the only check on ──────────────────
//
// `bin/verify-baselines` walks every entry in `baselines.json`, but it is a
// thing a person runs, not a thing `cargo test` runs. Twenty-five artifacts
// had no test behind them at all, so a change could move any of them and stay
// green until someone remembered to sweep. These pin the nineteen that render
// reproducibly, at 3.6 s for the lot.
//
// The six `showcase_*` frames are deliberately **not** here. They are not
// byte-reproducible on this adapter — measured repeatedly at four to six
// distinct images from six renders of an unchanged scene, on any binary — so a
// test asserting them would fail at random, which is worse than no test. They
// keep their `diff_args` tolerance in the manifest and stay the sweep's job.
// See CLAUDE.md §Verification before adding them here.

/// Diff-render one committed baseline and require it bit-exact.
///
/// Skips cleanly when this machine has no GPU, the policy every render pin
/// here follows: baselines are per-adapter artifacts, so a machine that cannot
/// render them has nothing to say about them.
fn pin_baseline(scene: &str, baseline: &str, args: &[&str]) {
    let mut command = engine();
    command
        .arg("diff-render")
        .arg(repo_path(scene))
        .arg(repo_path(baseline));
    for arg in args {
        // Timeline paths are repo-relative in the manifest; the test process
        // does not promise to run from the repo root.
        if arg.ends_with(".input.jsonl") {
            command.arg(repo_path(arg));
        } else {
            command.arg(arg);
        }
    }
    let diff = command.output().unwrap();
    if !diff.status.success() {
        let stderr = String::from_utf8_lossy(&diff.stderr);
        assert!(
            stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
            "diff-render failed for a non-GPU reason: {stderr}"
        );
        eprintln!("skipping render pin for {baseline}: no usable GPU on this machine");
        return;
    }
    let report: serde_json::Value = serde_json::from_str(stdout_of(&diff).trim()).unwrap();
    assert_eq!(report["pass"], true, "{baseline}: {report}");
    assert_eq!(report["diff_pixels"], 0, "{baseline}: {report}");
}

/// M4's lighting rig: the GGX shader, the two light components, and the sRGB
/// render target that M4 settled the colour space on.
#[test]
fn m4_lighting_baseline_pins_the_pbr_rig() {
    pin_baseline(
        "examples/scenes/verify/m4_lighting.json",
        "examples/scenes/verify/baselines/m4_lighting.png",
        &[],
    );
}

/// Both ends of M8's fall. `--steps 0` is the scene at rest, which is also the
/// only thing that catches a physics build changing where bodies *start*.
#[test]
fn m8_drop_baselines_pin_both_ends_of_the_fall() {
    for (steps, baseline) in [
        ("0", "examples/scenes/verify/baselines/m8_drop_t0.png"),
        ("300", "examples/scenes/verify/baselines/m8_drop_t300.png"),
    ] {
        pin_baseline(
            "examples/scenes/verify/m8_drop.json",
            baseline,
            &["--steps", steps],
        );
    }
}

/// M9 sampled by `--time`, not `--steps`: a property clip's pose is a pure
/// function of the clock, and this is the artifact that says so in pixels.
#[test]
fn m9_spin_baseline_pins_the_sampled_pose() {
    pin_baseline(
        "examples/scenes/verify/m9_spin.json",
        "examples/scenes/verify/baselines/m9_t025.png",
        &["--time", "0.25"],
    );
}

/// M10 before and after the script has run: `--steps 0` is the authored file,
/// `--steps 120` is what `fn step` did to it.
#[test]
fn m10_script_baselines_pin_both_ends_of_the_script() {
    for (steps, baseline) in [
        ("0", "examples/scenes/verify/baselines/m10_t0.png"),
        ("120", "examples/scenes/verify/baselines/m10_t120.png"),
    ] {
        pin_baseline(
            "examples/scenes/verify/m10_script.json",
            baseline,
            &["--steps", steps],
        );
    }
}

/// The parked car at the end of three recorded laps.
///
/// The lap test above pins the *drive* — positions, elevation, the HUD strings
/// — and has named this PNG in a comment since M11 without anyone rendering
/// it. Eleven thousand steps of vehicle physics and a render, for 1.2 s.
#[test]
fn m11_lap_baseline_pins_the_parked_car() {
    pin_baseline(
        "examples/scenes/car_track.json",
        "examples/scenes/verify/baselines/m11_lap.png",
        &[
            "--steps",
            "11634",
            "--input",
            "examples/scenes/car_track_lap.input.jsonl",
        ],
    );
}

/// M13's smoke: the seeded emitter's whole point is that a particle field can
/// sit under a bit-exact baseline at all.
#[test]
fn m13_smoke_baseline_pins_the_particle_field() {
    pin_baseline(
        "examples/scenes/verify/m13_smoke.json",
        "examples/scenes/verify/baselines/m13_smoke.png",
        &["--steps", "180"],
    );
}

/// M14 after the break: fragments are ordinary entities by the time this
/// renders, which is the milestone's claim as a picture.
#[test]
fn m14_break_baseline_pins_the_debris() {
    pin_baseline(
        "examples/scenes/verify/m14_break.json",
        "examples/scenes/verify/baselines/m14_break.png",
        &["--steps", "300"],
    );
}

/// The four hours of M21 that no test covered — noon already had one.
///
/// Together they pin the sun/moon handoff from both sides: 02:00 and 22:00 are
/// moonlit, 06:30 and 18:30 sit just past the swap.
#[test]
fn m21_daylight_baselines_pin_the_other_four_hours() {
    for (steps, baseline) in [
        (
            "120",
            "examples/scenes/verify/baselines/m21_daylight_0200.png",
        ),
        (
            "390",
            "examples/scenes/verify/baselines/m21_daylight_0630.png",
        ),
        (
            "1110",
            "examples/scenes/verify/baselines/m21_daylight_1830.png",
        ),
        (
            "1320",
            "examples/scenes/verify/baselines/m21_daylight_2200.png",
        ),
    ] {
        pin_baseline(
            "examples/scenes/verify/m21_daylight.json",
            baseline,
            &["--steps", steps],
        );
    }
}

/// M26's texture maps, aimed at their subject with no terrain in frame, which
/// is what lets this one carry a hard pin at all.
#[test]
fn m26_materials_baseline_pins_the_texture_maps() {
    pin_baseline(
        "examples/scenes/verify/m26_materials.json",
        "examples/scenes/verify/baselines/m26_materials.png",
        &[],
    );
}

/// M27's two cameras on one file: the overhead one pins the bend, the grazing
/// one pins the waterline the depth-copy validation exists for.
///
/// A test already names the overhead baseline, but only to assert it must
/// *not* match with `ior` back at 1.0 — that pins refraction as load-bearing
/// and says nothing about the render. This is the positive half.
#[test]
fn m27_water_refraction_baselines_pin_both_cameras() {
    pin_baseline(
        "examples/scenes/verify/m27_water_refraction.json",
        "examples/scenes/verify/baselines/m27_water_refraction.png",
        &["--steps", "120"],
    );
    pin_baseline(
        "examples/scenes/verify/m27_water_refraction.json",
        "examples/scenes/verify/baselines/m27_water_grazing.png",
        &["--steps", "120", "--camera", "CameraGrazing"],
    );
}

/// M28's two baselines from one timeline: where the pointer aimed, and what it
/// hit when the button went down.
#[test]
fn m28_pointer_baselines_pin_aim_and_click() {
    for (steps, baseline) in [
        ("40", "examples/scenes/verify/baselines/m28_pointer_aim.png"),
        (
            "80",
            "examples/scenes/verify/baselines/m28_pointer_click.png",
        ),
    ] {
        pin_baseline(
            "examples/scenes/verify/m28_pointer.json",
            baseline,
            &[
                "--steps",
                steps,
                "--input",
                "examples/scenes/verify/m28_pointer.input.jsonl",
            ],
        );
    }
}

/// M29's field, and the reason it renders at `samples: 1`: a meadow under MSAA
/// is not byte-reproducible on this adapter, so the fixture gives up
/// antialiasing to keep a hard pin. If this starts failing, check `samples`
/// before blaming the vertex stage.
#[test]
fn m29_meadow_baseline_pins_the_field_at_samples_1() {
    pin_baseline(
        "examples/scenes/verify/m29_meadow.json",
        "examples/scenes/verify/baselines/m29_meadow.png",
        &["--time", "0.7"],
    );
}
// ── Skinned collider proxies (M33) ────────────────────────────────────────

/// The milestone's fixture: two identical walkers walk into two identical
/// crates, and the only difference between them is a `SkinnedCollider`.
fn proxy_scene() -> PathBuf {
    repo_path("examples/scenes/verify/m33_proxies.json")
}

#[test]
fn the_proxy_fixture_validates() {
    let output = engine()
        .arg("validate")
        .arg(proxy_scene())
        .arg("--strict")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", stderr_lines(&output));
}

/// The M33 fixture rendered: one walker bulldozing a crate along, and its twin
/// standing past a crate it walked straight through.
///
/// The two walkers are the assertion, M30's fixture logic for the third time —
/// they share a file, a mesh, a clip and a crate, so anything that made both
/// wrong would leave them identical.
///
/// It aims at its subject with no terrain in frame, per M22's rule, so it
/// carries a hard bit-exact pin; four consecutive renders came back as one
/// image, measured rather than assumed.
#[test]
fn the_m33_proxy_fixture_pins_a_shoved_crate() {
    let scene = proxy_scene();
    let baseline = repo_path("examples/scenes/verify/baselines/m33_proxies.png");

    let diff = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .arg("--steps")
        .arg("150")
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

/// The milestone's claim as a number, with no image read: a character whose
/// pose the physics world can see moves what it walks into, and one whose pose
/// it cannot see does not.
#[test]
fn a_proxied_walker_shoves_its_crate_and_an_unproxied_one_walks_through_its_own() {
    let output = engine()
        .arg("simulate")
        .arg(proxy_scene())
        .arg("--steps")
        .arg("150")
        .arg("--entity")
        .arg("CrateHit")
        .arg("--entity")
        .arg("CrateMissed")
        .output()
        .unwrap();
    let report = json_stdout(&output);
    let z_of = |name: &str| -> f64 {
        report["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["entity"] == name)
            .unwrap_or_else(|| panic!("no {name} in {report}"))["position"][2]
            .as_f64()
            .unwrap()
    };

    // Both crates are authored at z = -0.55 and both walkers walk into them.
    let hit = z_of("CrateHit");
    let missed = z_of("CrateMissed");
    assert!(
        hit < -1.2,
        "the proxied walker must shove its crate well past its authored z = -0.55, \
         it is at {hit}"
    );
    assert!(
        (missed + 0.55).abs() < 1e-3,
        "the unproxied walker must pass through its crate without touching it, \
         but the crate moved to {missed}"
    );
}

/// `engine list-colliders` and `engine list-joints` must agree about where a
/// part is — the report closing the loop on itself, since a hitbox riding a
/// joint is invisible in every render.
///
/// They agree to millimetres rather than exactly, and the residue is causal:
/// this walker's clip is stride-driven, so its `phase` is advanced by the
/// ground it covered, which physics cannot know until it has run. The proxy is
/// therefore posed from the previous step's phase — M12's contact latency, in
/// another place. A wrong joint, a dropped model transform or a mis-composed
/// offset would all be off by tens of centimetres, not by two millimetres.
#[test]
fn list_colliders_and_list_joints_agree_about_where_a_part_is() {
    let scene = proxy_scene();

    let colliders = engine()
        .arg("list-colliders")
        .arg(&scene)
        .arg("--entity")
        .arg("Proxied")
        .arg("--steps")
        .arg("150")
        .output()
        .unwrap();
    let colliders = json_stdout(&colliders);
    let hips = colliders["colliders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["part"] == "Hips")
        .unwrap_or_else(|| panic!("no Hips proxy in {colliders}"));

    let joints = engine()
        .arg("list-joints")
        .arg(&scene)
        .arg("--entity")
        .arg("Proxied")
        .arg("--steps")
        .arg("150")
        .output()
        .unwrap();
    let joints = json_stdout(&joints);
    let joint = joints["rigs"][0]["joints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["name"] == "Hips")
        .unwrap_or_else(|| panic!("no Hips joint in {joints}"));

    // The Hips part carries no offset, so the two should name the same point.
    for axis in 0..3 {
        let from_physics = hips["position"][axis].as_f64().unwrap();
        let from_pose = joint["world"]["position"][axis].as_f64().unwrap();
        assert!(
            (from_physics - from_pose).abs() < 5e-3,
            "axis {axis}: the proxy is at {from_physics}, the joint at {from_pose}"
        );
    }

    // Every part the component authors is built, and each is named once.
    let parts: Vec<&str> = colliders["colliders"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["part"].as_str())
        .collect();
    assert_eq!(parts.len(), 11, "{colliders}");
    assert!(
        parts.contains(&"Head") && parts.contains(&"FootR"),
        "{parts:?}"
    );
}

/// A shot to the head reports the head. The entity name stays an entity name —
/// a proxy is not one, and a report that put `Proxied/Head` where an entity
/// belongs would name something no command accepts (design §5).
#[test]
fn a_raycast_names_the_proxy_part_it_hit() {
    let scene = proxy_scene();

    let at_head = engine()
        .arg("raycast")
        .arg(&scene)
        .arg("--from")
        .arg("-6,1.55,-1.4833")
        .arg("--dir")
        .arg("1,0,0")
        .arg("--steps")
        .arg("150")
        .output()
        .unwrap();
    let hit = json_stdout(&at_head)["hit"].clone();
    assert_eq!(hit["entity"], "Proxied", "{hit}");
    assert_eq!(hit["part"], "Head", "{hit}");

    // The same ray across the unproxied walker's lane finds nothing: its pose
    // is invisible to physics, which is the whole difference between them.
    let past_loose = engine()
        .arg("raycast")
        .arg(&scene)
        .arg("--from")
        .arg("-6,1.55,1.4833")
        .arg("--dir")
        .arg("1,0,0")
        .arg("--steps")
        .arg("150")
        .output()
        .unwrap();
    assert_eq!(json_stdout(&past_loose)["hit"], serde_json::Value::Null);
}

/// `list-colliders` answers for an ordinary scene too — "where are the
/// colliders" was unanswerable before this command, and answering it only for
/// proxies would be half a report.
#[test]
fn list_colliders_reports_component_colliders_with_no_part() {
    let output = engine()
        .arg("list-colliders")
        .arg(repo_path("examples/scenes/verify/m8_drop.json"))
        .output()
        .unwrap();
    let report = json_stdout(&output);
    let rows = report["colliders"].as_array().unwrap();
    assert!(!rows.is_empty(), "{report}");
    assert!(
        rows.iter().all(|row| row["part"].is_null()),
        "a scene with no SkinnedCollider has no parts: {report}"
    );
    let cube = rows
        .iter()
        .find(|row| row["entity"] == "DropCube")
        .unwrap_or_else(|| panic!("no DropCube in {report}"));
    assert_eq!(cube["shape"], "cuboid", "{cube}");
    assert_eq!(
        cube["dimensions"],
        serde_json::json!([0.5, 0.5, 0.5]),
        "{cube}"
    );
}

/// A proxy on a joint the rig does not have is refused before a device or a
/// step exists, with the near miss named — `world.key`'s manners, and
/// `FootPlant`'s, since the failure is identical: a mistyped joint otherwise
/// builds no hitbox at all, silently, and nothing in the render says so.
#[test]
fn a_proxy_on_an_unknown_joint_is_refused_with_a_suggestion() {
    let scene = proxy_scene();
    let source = std::fs::read_to_string(&scene).unwrap();
    let typo = source.replace(r#""joint": "Chest""#, r#""joint": "Chset""#);
    assert_ne!(source, typo, "the fixture must author a Chest proxy");

    // Next to the original: asset paths resolve relative to the scene file.
    let broken = scene.with_file_name("m33_broken_joint.json");
    std::fs::write(&broken, typo).unwrap();
    let output = engine().arg("validate").arg(&broken).output().unwrap();
    let _ = std::fs::remove_file(&broken);

    assert_eq!(output.status.code(), Some(1));
    let errors = stderr_lines(&output);
    let unknown = errors
        .iter()
        .find(|e| e["error"] == "unknown_joint")
        .unwrap_or_else(|| panic!("expected unknown_joint, got {errors:?}"));
    assert_eq!(unknown["component"], "SkinnedCollider", "{unknown}");
    assert_eq!(unknown["did_you_mean"], "Chest", "{unknown}");
}

// ── M36: the game shell — saves, quit, and a writable environment ──────────
//
// Three engine additions, and only one of them has pixels behind it. The
// fixture pins that one; these pin the rest, which is the split the milestone's
// design doc asks for.

/// A scene in its own temp directory with one script, no assets but builtins.
///
/// No relative asset paths, deliberately: M10's trap is that a scene moved away
/// from its files loses them, and a save test that copied a fixture would be
/// testing the copy rather than the save.
fn shell_scene(test: &str, script: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("engine-m36-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(dir.join("scripts/shell.rhai"), script).unwrap();
    let scene = dir.join("scene.json");
    std::fs::write(
        &scene,
        r#"{"name":"shell","entities":[
            {"name":"Cam","components":[{"type":"Camera","active":true}]},
            {"name":"Mark","components":[
                {"type":"Transform"},
                {"type":"Mesh","asset":"builtin:cube"}
            ]},
            {"name":"Game","components":[{"type":"Script","source":"scripts/shell.rhai"}]}
        ]}"#,
    )
    .unwrap();
    scene
}

fn simulate_report(scene: &Path, steps: u32, entities: &[&str]) -> serde_json::Value {
    let mut command = engine();
    command
        .arg("simulate")
        .arg(scene)
        .arg("--steps")
        .arg(steps.to_string());
    for name in entities {
        command.arg("--entity").arg(name);
    }
    json_stdout(&command.output().unwrap())
}

/// The other half of the shell's contract: a script that asks for something
/// impossible fails the run, and says so as a located `script_runtime_error`.
/// Shared by the setters that validate at the call (M13's rule), so the two
/// cannot drift on what a script failure looks like — only on the sentence.
fn script_error(scene: &Path) -> serde_json::Value {
    let output = engine()
        .arg("simulate")
        .arg(scene)
        .arg("--steps")
        .arg("1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let errors = stderr_lines(&output);
    errors
        .iter()
        .find(|e| e["error"] == "script_runtime_error")
        .unwrap_or_else(|| panic!("expected script_runtime_error, got {errors:?}"))
        .clone()
}

/// The round trip is the whole promise, and it is checked *across processes*:
/// one `engine simulate` writes the slot, a second reads it. A single run would
/// return the right answer out of `world.state` even if no file existed.
#[test]
fn a_save_slot_survives_the_process_that_wrote_it() {
    let writer = shell_scene(
        "save-writer",
        r#"fn step(world, step) {
            if step == 0 { world.set_state("score", 4200.0); }
            if step == 0 { world.set_state("level", 3.0); }
            if step == 1 { let ok = world.save(1); }
        }"#,
    );
    simulate_report(&writer, 3, &["Mark"]);

    // Next to the scene, not in /tmp's root and not beside the binary: M10's
    // rule, because everything in this engine resolves relative to the file.
    let slot = writer.with_file_name("saves").join("slot1.json");
    let body = std::fs::read_to_string(&slot)
        .unwrap_or_else(|e| panic!("no save at {}: {e}", slot.display()));
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["score"], 4200.0, "{body}");
    assert_eq!(parsed["level"], 3.0, "{body}");
    // Sorted keys, so a save is git-diffable by construction (invariant 1).
    assert!(
        body.find("\"level\"").unwrap() < body.find("\"score\"").unwrap(),
        "keys must be sorted: {body}"
    );

    // A second scene in the same directory reads the slot the first wrote and
    // parks the cube at the numbers that came back.
    let reader_dir = writer.parent().unwrap();
    std::fs::write(
        reader_dir.join("scripts/shell.rhai"),
        r#"fn step(world, step) {
            if step == 0 { let ok = world.load(1); }
            if step == 1 {
                let s = world.state("score", 0.0);
                let l = world.state("level", 0.0);
                world.set_position("Mark", s, l, 0.0);
            }
        }"#,
    )
    .unwrap();
    let report = simulate_report(&writer, 3, &["Mark"]);
    let at = &report["entities"][0]["position"];
    assert_eq!(at[0], 4200.0, "{report}");
    assert_eq!(at[1], 3.0, "the save did not come back: {report}");
}

/// An empty slot reads as empty rather than failing — "is there a save?" is a
/// menu's first question. An impossible slot *does* fail, because a script
/// choosing its own path is what the sandbox exists to prevent.
#[test]
fn an_empty_slot_is_not_an_error_and_an_impossible_one_is() {
    let scene = shell_scene(
        "slots",
        r#"fn step(world, step) {
            let found = world.load(4);
            let there = world.has_save(4);
            if !found && !there { world.set_position("Mark", 7.0, 0.0, 0.0); }
        }"#,
    );
    let report = simulate_report(&scene, 2, &["Mark"]);
    assert_eq!(report["entities"][0]["position"][0], 7.0, "{report}");

    let bad = shell_scene("slot-range", r#"fn step(world, step) { world.save(11); }"#);
    let failure = script_error(&bad);
    assert!(
        failure["message"].as_str().unwrap().contains("0..9"),
        "the message must name the range: {failure}"
    );
}

/// `world.quit` stops a headless run and says where. It is **not** a failure:
/// a game that ended is not an error, and the frame the run reached is still
/// the frame to render.
#[test]
fn quitting_stops_a_headless_run_and_says_which_step() {
    let scene = shell_scene(
        "quit",
        r#"fn step(world, step) {
            world.set_position("Mark", step.to_float(), 0.0, 0.0);
            if step == 12 { world.quit(); }
        }"#,
    );
    let report = simulate_report(&scene, 500, &["Mark"]);
    assert_eq!(report["quit_at_step"], 12, "{report}");
    // `simulated_steps` stays what was *asked* for, so the two together say
    // "you asked for 500 and it ended at 12".
    assert_eq!(report["simulated_steps"], 500, "{report}");
    // And the run really stopped: the cube is where step 12 left it, not
    // where step 499 would have.
    assert_eq!(report["entities"][0]["position"][0], 12.0, "{report}");

    // A run that never quits carries no key at all, so every pre-M36 report is
    // byte-identical.
    let quiet = shell_scene(
        "no-quit",
        r#"fn step(world, step) { world.set_position("Mark", 1.0, 0.0, 0.0); }"#,
    );
    let report = simulate_report(&quiet, 5, &["Mark"]);
    assert!(report.get("quit_at_step").is_none(), "{report}");
}

/// The `environment` block is writable, and a scene that writes it renders
/// differently — checked without a GPU by reading the value back through the
/// one thing that reports it: the fixture's own diff-render below does the
/// pixels. Here the claim is that the *setters validate at the call*, M13's
/// rule, because this value ends up in a scene file.
#[test]
fn samples_must_be_one_or_four_at_the_call() {
    let scene = shell_scene(
        "samples",
        r#"fn step(world, step) { world.set_samples(2); }"#,
    );
    let failure = script_error(&scene);
    assert!(
        failure["message"]
            .as_str()
            .unwrap()
            .contains("must be 1 or 4"),
        "{failure}"
    );
}

/// `engine ui-layout --steps` reports the layout the *run* reached (M36), which
/// is M32's `list-joints --steps` argument applied to a menu: which slots a
/// screen uses is what the script painted, not what the file says.
///
/// The arena is the worked example and the assertion is the interesting half —
/// its title card is authored to be exactly what the script paints, so the two
/// reports must agree. A card that grew on the first step would move every
/// button in it, and a demo timeline aiming at the rest rect would click
/// through empty space.
#[test]
fn ui_layout_can_report_the_layout_a_run_painted() {
    let scene = repo_path("examples/scenes/arena_shooter.json");
    let rect = |steps: Option<u32>| -> serde_json::Value {
        let mut command = engine();
        command
            .arg("ui-layout")
            .arg(&scene)
            .arg("--width")
            .arg("960")
            .arg("--height")
            .arg("540")
            .arg("--entity")
            .arg("MenuBtn1");
        if let Some(steps) = steps {
            command.arg("--steps").arg(steps.to_string());
        }
        json_stdout(&command.output().unwrap())["elements"][0]["rect"].clone()
    };

    let at_rest = rect(None);
    assert_eq!(
        at_rest,
        rect(Some(4)),
        "the arena authors its title card exactly, so painting it must not move a button"
    );
    assert!(
        at_rest[3].as_f64().unwrap() > 0.0,
        "a visible button has a height: {at_rest}"
    );
}

/// The fixture: script-driven shadows and a hard clip cut, pinned bit-exactly.
///
/// **The two soldiers are the assertion** (M30's fixture logic for the fourth
/// time): they share a file, a mesh and a material, so anything that made both
/// wrong would leave them identical. Only a real clip switch makes one stride
/// and the other breathe, and only a writable `environment` puts a shadow under
/// either of them — the file authors `shadows: false`.
#[test]
fn the_shell_fixture_matches_its_baseline() {
    let scene = repo_path("examples/scenes/verify/m36_shell.json");
    let baseline = repo_path("examples/scenes/verify/baselines/m36_shell.png");

    let diff = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .arg("--steps")
        .arg("90")
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
}

// ── Shadow cascades (M38) ─────────────────────────────────────────────────

fn cascade_scene() -> PathBuf {
    repo_path("examples/scenes/verify/m38_shadow_cascades.json")
}

#[test]
fn the_cascade_fixture_validates() {
    let output = engine()
        .arg("validate")
        .arg(cascade_scene())
        .arg("--strict")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", stderr_lines(&output));
}

/// The milestone's fixture: a fence receding to 168 m under three nested
/// cascades, so one object spans all three and the sharpness gradient runs
/// along it rather than between three separate props.
///
/// It aims at flat ground with no `Terrain` in frame and renders at
/// `samples: 1`, per CLAUDE.md's reproducibility rule, so it carries a hard
/// bit-exact pin — three consecutive renders came back as one image, measured
/// rather than assumed.
#[test]
fn the_m38_cascade_fixture_matches_its_baseline() {
    let scene = cascade_scene();
    let baseline = repo_path("examples/scenes/verify/baselines/m38_shadow_cascades.png");

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

/// A cascade count outside 1–4 is refused at validate time, with the
/// environment block's own code rather than a schema type error.
#[test]
fn a_scene_asking_for_too_many_cascades_is_refused() {
    let scene = scene_file(
        "too-many-cascades",
        r#"{"name":"s","environment":{"shadows":true,"shadow_cascades":9},"entities":[]}"#,
    );

    let output = engine().arg("validate").arg(&scene).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let errors = stderr_lines(&output);
    assert!(
        errors
            .iter()
            .any(|line| line["error"] == "invalid_environment_value"
                && line["field"] == "shadow_cascades"),
        "{errors:?}"
    );
}

// ── Ragdolls (M39) ─────────────────────────────────────────────────────────

/// The milestone's fixture: two identical walkers, and the only difference
/// between them is a `Ragdoll` a script fires at step 40.
fn ragdoll_scene() -> PathBuf {
    repo_path("examples/scenes/verify/m39_ragdoll.json")
}

#[test]
fn the_ragdoll_fixture_validates() {
    let output = engine()
        .arg("validate")
        .arg(ragdoll_scene())
        .arg("--strict")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", stderr_lines(&output));
}

/// The M39 fixture rendered mid-collapse: one walker folding onto the floor
/// while its twin walks on.
///
/// The two walkers are the assertion, M30's fixture logic for the fourth time
/// — they share a file, a mesh, a clip and a proxy set, so anything that made
/// both wrong would leave them identical. Step 75 rather than the end of the
/// run because a settled corpse is a flat pile: the frame where the milestone
/// is legible is the one still falling.
///
/// No terrain in frame, per M22's rule, so this is a hard bit-exact pin — four
/// consecutive renders came back as one image, measured rather than assumed.
#[test]
fn the_m39_ragdoll_fixture_pins_a_collapsing_character() {
    let diff = engine()
        .arg("diff-render")
        .arg(ragdoll_scene())
        .arg(repo_path(
            "examples/scenes/verify/baselines/m39_ragdoll.png",
        ))
        .arg("--steps")
        .arg("75")
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

/// The milestone's claim as a number, with no image read: the character
/// physics took over falls, and its twin does not.
///
/// `Transform.position` following the root part is what makes this readable at
/// all — a ragdoll whose transform stayed put would be invisible to `simulate`,
/// to culling and to every script distance check, while being plainly
/// somewhere else on screen (design §4).
#[test]
fn the_ragdolled_walker_falls_and_its_twin_keeps_walking() {
    let output = engine()
        .arg("simulate")
        .arg(ragdoll_scene())
        .arg("--steps")
        .arg("150")
        .arg("--entity")
        .arg("Dropped")
        .arg("--entity")
        .arg("Walking")
        .output()
        .unwrap();
    let report = json_stdout(&output);
    let y_of = |name: &str| -> f64 {
        report["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["entity"] == name)
            .unwrap_or_else(|| panic!("no {name} in {report}"))["position"][1]
            .as_f64()
            .unwrap()
    };

    // Both are authored standing at y = 0 and carried by the same script line.
    let dropped = y_of("Dropped");
    assert!(
        dropped > 0.05 && dropped < 0.45,
        "the ragdoll's root should end up on the floor but above it — a hips \
         joint at {dropped} is either still standing or fell through"
    );
    assert_eq!(
        y_of("Walking"),
        0.0,
        "the walker with no Ragdoll must be exactly where the script put it"
    );
}

/// M33's agreement test with its arrow reversed: physics is now the *source* of
/// the pose, and `list-colliders` and `list-joints` still have to name the same
/// point.
///
/// This is the check that the whole design turns on. The proxy's placement is
/// read out of rapier; the joint's is read out of `Ragdoll.pose`, the component
/// field physics wrote. If the write-back's `G = M⁻¹ · B · L⁻¹` were wrong in
/// any of its three factors these would disagree by tens of centimetres.
///
/// Exactly, not to millimetres, unlike M33's: there is no stride latency here,
/// because a ragdolled character's pose is not advanced by ground covered.
#[test]
fn a_ragdolled_characters_reports_agree_about_where_its_hips_are() {
    let scene = ragdoll_scene();

    let colliders = engine()
        .arg("list-colliders")
        .arg(&scene)
        .arg("--entity")
        .arg("Dropped")
        .arg("--steps")
        .arg("75")
        .output()
        .unwrap();
    let colliders = json_stdout(&colliders);
    let hips = colliders["colliders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["part"] == "Hips")
        .unwrap_or_else(|| panic!("no Hips proxy in {colliders}"));

    let joints = engine()
        .arg("list-joints")
        .arg(&scene)
        .arg("--entity")
        .arg("Dropped")
        .arg("--steps")
        .arg("75")
        .output()
        .unwrap();
    let joints = json_stdout(&joints);
    let joint = joints["rigs"][0]["joints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["name"] == "Hips")
        .unwrap_or_else(|| panic!("no Hips joint in {joints}"));

    // The Hips part carries no offset, so the two name the same point.
    for axis in 0..3 {
        let from_physics = hips["position"][axis].as_f64().unwrap();
        let from_pose = joint["world"]["position"][axis].as_f64().unwrap();
        assert!(
            (from_physics - from_pose).abs() < 1e-3,
            "axis {axis}: the proxy is at {from_physics}, the joint at {from_pose}"
        );
    }
}

/// A corpse baked mid-fall reloads into the same heap — **bit-exactly**.
///
/// This is why `Ragdoll.pose` is a component field rather than state in the
/// physics world (design §2, and M32's rule that the bake is what settles the
/// question). A pose that lived in `PhysicsWorld` would reload as a character
/// standing up in its bind pose, and the bake's promise would be false for
/// every dead body in every scene.
#[test]
fn a_ragdoll_baked_mid_fall_reloads_into_the_same_heap() {
    let baked = repo_path("examples/scenes/verify/m39_baked_probe.json");
    let _ = std::fs::remove_file(&baked);

    let bake = engine()
        .arg("simulate")
        .arg(ragdoll_scene())
        .arg("--steps")
        .arg("75")
        .arg("--bake")
        .arg(&baked)
        .output()
        .unwrap();
    assert!(bake.status.success(), "{:?}", stderr_lines(&bake));

    // The pose is in the file, in full, and the flag with it.
    let text = std::fs::read_to_string(&baked).unwrap();
    let file: serde_json::Value = serde_json::from_str(&text).unwrap();
    let ragdoll = file["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "Dropped")
        .unwrap()["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "Ragdoll")
        .expect("the baked corpse must carry its Ragdoll")
        .clone();
    assert_eq!(ragdoll["active"], true, "{ragdoll}");
    assert_eq!(
        ragdoll["pose"].as_array().map(Vec::len),
        Some(13),
        "one entry per joint of the rig, {ragdoll}"
    );

    // And it draws as the frame it was baked from, with no steps at all.
    let diff = engine()
        .arg("diff-render")
        .arg(&baked)
        .arg(repo_path(
            "examples/scenes/verify/baselines/m39_ragdoll.png",
        ))
        .arg("--steps")
        .arg("0")
        .output()
        .unwrap();
    if !diff.status.success() {
        let stderr = String::from_utf8_lossy(&diff.stderr);
        assert!(
            stderr.contains("no_gpu_adapter") || stderr.contains("device_request_failed"),
            "diff-render failed for a non-GPU reason: {stderr}"
        );
        let _ = std::fs::remove_file(&baked);
        eprintln!("skipping bake round-trip pin: no usable GPU on this machine");
        return;
    }
    let report: serde_json::Value = serde_json::from_str(stdout_of(&diff).trim()).unwrap();
    let _ = std::fs::remove_file(&baked);
    assert_eq!(report["pass"], true, "{report}");
    assert_eq!(report["diff_pixels"], 0, "{report}");
}

/// A `fit: "bone"` part takes its length from the posed rig, and a plain one
/// does not (M39 §7).
///
/// The fixture's thigh capsules are fitted and its shin capsules are authored,
/// so one pair reports a `half_height` the rig implies and the other reports
/// the number in the file. This is the property M33 wanted when it refused
/// resizing shapes — not that they never change, but that what they *are* is
/// answerable with a command.
#[test]
fn a_fitted_part_reports_the_bone_and_an_authored_one_reports_the_file() {
    let colliders = engine()
        .arg("list-colliders")
        .arg(ragdoll_scene())
        .arg("--entity")
        .arg("Walking")
        .arg("--steps")
        .arg("30")
        .output()
        .unwrap();
    let colliders = json_stdout(&colliders);
    let half_height = |part: &str| -> f64 {
        colliders["colliders"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["part"] == part)
            .unwrap_or_else(|| panic!("no {part} in {colliders}"))["dimensions"][0]
            .as_f64()
            .unwrap()
    };

    // `KneeL` is authored at 0.15 and must stay there, exactly.
    assert!(
        (half_height("KneeL") - 0.15).abs() < 1e-6,
        "an authored part must report the file's number, got {}",
        half_height("KneeL")
    );
    // `LegL` is authored at 0.15 too and asks to fit; the walker's thigh is a
    // different length, so the reported number must have moved off it.
    let fitted = half_height("LegL");
    assert!(
        (fitted - 0.15).abs() > 1e-3,
        "a fitted part must report the bone, not the authored 0.15, got {fitted}"
    );
}

/// `engine fit-colliders` solves a proxy set and prints it as text — the
/// answer to M33's refusal of runtime generation rather than an overruling of
/// it (design §8). Without `--write` the scene file is untouched.
#[test]
fn fit_colliders_prints_a_proxy_set_and_leaves_the_file_alone() {
    let scene = ragdoll_scene();
    let before = std::fs::read_to_string(&scene).unwrap();

    let output = engine()
        .arg("fit-colliders")
        .arg(&scene)
        .arg("--entity")
        .arg("Walking")
        .output()
        .unwrap();
    let report = json_stdout(&output);
    assert_eq!(report["written"], false, "{report}");

    let parts = report["entities"][0]["component"]["parts"]
        .as_array()
        .unwrap_or_else(|| panic!("no parts in {report}"));
    // One per joint the skin actually weights, which for this rig is all
    // thirteen — and each names a joint of the rig rather than an index.
    assert_eq!(parts.len(), 13, "{report}");
    assert!(
        parts.iter().any(|p| p["joint"] == "Head"),
        "the fitted set must name joints, {report}"
    );
    // Every fitted shape has to be one a proxy may be, and a real size.
    for part in parts {
        assert_eq!(part["shape"], "cuboid", "{part}");
        for axis in 0..3 {
            assert!(
                part["half_extents"][axis].as_f64().unwrap() > 0.0,
                "a fitted extent must be positive, {part}"
            );
        }
    }

    assert_eq!(
        std::fs::read_to_string(&scene).unwrap(),
        before,
        "fit-colliders without --write must not touch the scene file"
    );
}

/// The four ragdoll-specific refusals, each reported before a device or a step
/// exists.
#[test]
fn ragdoll_validation_refuses_what_it_should() {
    let dir = scratch_dir("ragdoll-validation");
    std::fs::create_dir_all(&dir).unwrap();
    let scene = dir.join("bad.json");

    // A Ragdoll with no SkinnedCollider: the bodies *are* the proxies.
    std::fs::write(
        &scene,
        serde_json::json!({
            "name": "bad",
            "entities": [{
                "name": "Ghost",
                "components": [
                    { "type": "Transform" },
                    { "type": "Ragdoll" },
                ],
            }],
        })
        .to_string(),
    )
    .unwrap();
    let codes = codes_of(&stderr_lines(
        &engine().arg("validate").arg(&scene).output().unwrap(),
    ));
    assert!(
        codes.contains(&"ragdoll_without_proxies".to_string()),
        "{codes:?}"
    );

    // A hinge with no axis, a range that runs backwards, a range with no
    // hinge, two overrides for one joint, and an override for a joint no part
    // rides — all at once, because this validator reports every error.
    std::fs::write(
        &scene,
        serde_json::json!({
            "name": "bad",
            "entities": [{
                "name": "Walker",
                "components": [
                    { "type": "Transform" },
                    { "type": "Mesh", "asset": "builtin:cube" },
                    { "type": "SkinnedCollider", "parts": [
                        { "joint": "Hips", "shape": "sphere", "radius": 0.1 },
                    ]},
                    { "type": "Ragdoll", "joints": [
                        { "joint": "Hips", "hinge": [0.0, 0.0, 0.0] },
                        { "joint": "Hips", "limit": 20.0 },
                        { "joint": "Knee", "limit": 20.0 },
                        { "joint": "Hips", "range": [10.0, 0.0] },
                    ]},
                ],
            }],
        })
        .to_string(),
    )
    .unwrap();
    let codes = codes_of(&stderr_lines(
        &engine().arg("validate").arg(&scene).output().unwrap(),
    ));
    for wanted in [
        "ragdoll_bad_hinge",
        "ragdoll_duplicate_joint",
        "ragdoll_unknown_joint",
    ] {
        assert!(
            codes.contains(&wanted.to_string()),
            "expected {wanted} in {codes:?}"
        );
    }
}

// ── Buoyancy and the water evaluator (M41) ──────────────────────────────

/// A pond with two floats and a stone, sized so the whole pond is inside the
/// patch and nothing needs a long settle.
const POND: &str = r#"{
  "name":"pond",
  "physics":{"gravity":[0.0,-9.81,0.0],"timestep_hz":60},
  "entities":[
    {"name":"Cam","components":[
      {"type":"Transform","position":[0.0,3.0,8.0],"rotation":[-15.0,0.0,0.0]},
      {"type":"Camera","fov":50.0,"near":0.1,"far":100.0,"active":true}]},
    {"name":"Bed","components":[
      {"type":"Transform","position":[0.0,-3.0,0.0],"scale":[40.0,1.0,40.0]},
      {"type":"Mesh","asset":"builtin:plane"},
      {"type":"Material","albedo":[0.3,0.3,0.3]},
      {"type":"Collider","shape":"cuboid","half_extents":[0.5,0.05,0.5]}]},
    {"name":"Lake","components":[
      {"type":"Transform","scale":[20.0,1.0,20.0]},
      {"type":"Water","segments":64,"waves":[
        {"direction":30.0,"wavelength":6.0,"amplitude":0.12,"steepness":0.4,"speed":1.4}]}]},
    {"name":"Cork","components":[
      {"type":"Transform","position":[-1.5,0.6,0.0],"scale":[0.8,0.8,0.8]},
      {"type":"Mesh","asset":"builtin:sphere"},
      {"type":"Material","albedo":[0.8,0.4,0.1]},
      {"type":"RigidBody","body":"dynamic"},
      {"type":"Collider","shape":"sphere","radius":0.5,"density":250.0},
      {"type":"Buoyancy","water":"Lake","samples":1,"drag":2.0}]},
    {"name":"Anvil","components":[
      {"type":"Transform","position":[1.5,0.6,0.0],"scale":[0.8,0.8,0.8]},
      {"type":"Mesh","asset":"builtin:sphere"},
      {"type":"Material","albedo":[0.2,0.2,0.2]},
      {"type":"RigidBody","body":"dynamic"},
      {"type":"Collider","shape":"sphere","radius":0.5,"density":7800.0},
      {"type":"Buoyancy","water":"Lake","samples":1,"drag":2.0}]}
  ]}"#;

/// The claim buoyancy exists to make: what is lighter than water stays at the
/// surface, and what is heavier goes to the bottom.
///
/// Two bodies identical in every way but `Collider.density`, so nothing that
/// moved both — a broken evaluator, gravity, the bed — can make this pass. The
/// numbers are metres and the pond is 3 m deep, so the gap between the two
/// outcomes is far larger than any tolerance question.
#[test]
fn a_light_body_floats_and_a_dense_one_sinks() {
    let scene = scene_file("buoyancy-density", POND);
    let report = json_stdout(
        &engine()
            .arg("simulate")
            .arg(&scene)
            .args(["--steps", "420"])
            .args(["--entity", "Cork"])
            .args(["--entity", "Anvil"])
            .output()
            .unwrap(),
    );

    let height_of = |name: &str| -> f64 {
        report["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["entity"] == name)
            .unwrap_or_else(|| panic!("{name} should be in the report"))["position"][1]
            .as_f64()
            .unwrap()
    };

    let cork = height_of("Cork");
    let anvil = height_of("Anvil");
    assert!(
        cork > -0.6,
        "a body a quarter the density of water must stay at the surface, not sink to {cork}"
    );
    assert!(
        anvil < -2.0,
        "a body denser than steel must reach the bed, not hover at {anvil}"
    );
}

/// Buoyancy is opt-in, and this is what "opt-in" has to mean: the *same* light
/// body with no `Buoyancy` component sinks like any other.
///
/// Without this, a passing float test proves only that something holds bodies
/// up — it could be the water's collider, if water had one, or a bug.
#[test]
fn without_the_component_nothing_floats() {
    let sinking = POND.replace(
        r#",
      {"type":"Buoyancy","water":"Lake","samples":1,"drag":2.0}]},
    {"name":"Anvil"#,
        r#"]},
    {"name":"Anvil"#,
    );
    let scene = scene_file("buoyancy-optin", &sinking);
    let report = json_stdout(
        &engine()
            .arg("simulate")
            .arg(&scene)
            .args(["--steps", "420"])
            .args(["--entity", "Cork"])
            .output()
            .unwrap(),
    );
    let cork = report["entities"][0]["position"][1].as_f64().unwrap();
    assert!(
        cork < -2.0,
        "a cork with no Buoyancy is an ordinary body and belongs on the bed, not at {cork}"
    );
}

/// `water-height` and `world.water_height` are one evaluator, exactly as
/// `terrain-height` and `world.terrain_height` are.
///
/// The terrain twin of this test is what M22's one-implementation claim rests
/// on; water needs it more, not less, because there is a *third* copy of the
/// curve in `water.wgsl` that a render test holds separately.
#[test]
fn water_height_is_the_evaluator_scripts_ask() {
    let scene = scene_file(
        "water-sampler",
        &POND.replace(
            r#"{"name":"Cam","components":["#,
            r#"{"name":"Probe","components":[{"type":"Script","source":"probe.rhai"}]},
    {"name":"Cam","components":["#,
        ),
    );
    std::fs::write(
        scene.parent().unwrap().join("probe.rhai"),
        "fn step(world, step) { world.hud(\"h=\" + world.water_height(\"Lake\", 2.5, -1.5)); }\n",
    )
    .unwrap();

    // Ten steps, and the CLI is asked about **nine**. That is not an
    // off-by-one: a script runs at the time its step *begins* at
    // (`step_index`, 0-based) while physics and the render are handed the time
    // it *ends* at, so the last of ten script calls saw 9/60 s. Terrain never
    // had to care because a height field has no clock. Asking both at the same
    // instant is the entire point of this test, so the offset is spelled out
    // rather than absorbed into a tolerance.
    let simulated = json_stdout(
        &engine()
            .arg("simulate")
            .arg(&scene)
            .args(["--steps", "10"])
            .output()
            .unwrap(),
    );
    let from_script: f64 = simulated["hud"][0]
        .as_str()
        .expect("the script pushes one HUD line")
        .trim_start_matches("h=")
        .parse()
        .unwrap();

    let from_cli = json_stdout(
        &engine()
            .arg("water-height")
            .arg(&scene)
            .args(["--at", "2.5,-1.5"])
            .args(["--steps", "9"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        from_cli["height"].as_f64().unwrap() as f32,
        from_script as f32,
        "the CLI and the script API must sample the same surface"
    );
}

/// Water has edges, and the query says so rather than answering 0.0.
#[test]
fn water_height_reports_where_there_is_no_water() {
    let scene = scene_file("water-edges", POND);
    let inside = json_stdout(
        &engine()
            .arg("water-height")
            .arg(&scene)
            .args(["--at", "3,3"])
            .output()
            .unwrap(),
    );
    assert_eq!(inside["water"], true);
    assert!(inside["height"].is_number(), "{inside}");
    assert_eq!(
        inside["normal"].as_array().unwrap().len(),
        3,
        "the normal rides along: {inside}"
    );

    // The patch is 20 m across, so ±10 m is the edge.
    let outside = json_stdout(
        &engine()
            .arg("water-height")
            .arg(&scene)
            .args(["--at", "10.5,0"])
            .output()
            .unwrap(),
    );
    assert_eq!(outside["water"], false);
    assert!(
        outside.get("height").is_none(),
        "no water means no height to report: {outside}"
    );
}

/// The surface moves, so the query has a clock and it is load-bearing.
#[test]
fn water_height_answers_at_the_time_it_is_asked_about() {
    let scene = scene_file("water-clock", POND);
    let at = |args: [&str; 2]| -> f64 {
        json_stdout(
            &engine()
                .arg("water-height")
                .arg(&scene)
                .args(["--at", "0,0"])
                .args(args)
                .output()
                .unwrap(),
        )["height"]
            .as_f64()
            .unwrap()
    };

    let start = at(["--time", "0.0"]);
    let later = at(["--time", "1.7"]);
    assert!(
        (start - later).abs() > 1e-3,
        "a travelling wave must have moved between t=0 and t=1.7: {start} vs {later}"
    );
    // `--steps` is the same clock at `steps / timestep_hz`, which is how a
    // render and a physics step agree about where the wave is.
    assert!(
        (at(["--steps", "102"]) - later).abs() < 1e-6,
        "102 steps at 60 Hz is 1.7 s"
    );
}

/// The fixture: a raft and a buoy riding a swell, and a stone on the bed.
///
/// **The three densities are the assertion.** They share a pond, a clock and an
/// evaluator, so anything that broke buoyancy as a whole would move all three
/// together — only a working force law puts one at the waterline, one half out
/// of it, and one on the bottom. Pinned bit-exactly: five renders of this scene
/// came back as one image, because the camera holds no terrain and the scene
/// renders at `samples: 1`.
#[test]
fn the_buoyancy_fixture_matches_its_baseline() {
    let scene = repo_path("examples/scenes/verify/m41_buoyancy.json");
    let baseline = repo_path("examples/scenes/verify/baselines/m41_buoyancy.png");

    let diff = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .arg("--steps")
        .arg("480")
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
}

/// A `Buoyancy` that names nothing, names the wrong thing, or sits on something
/// that cannot be pushed — all at once, because validation reports everything at
/// once (M5) and a scene with three mistakes should need one run to find them.
#[test]
fn buoyancy_refuses_the_component_that_could_do_nothing() {
    let broken = r#"{
  "name":"broken buoyancy",
  "entities":[
    {"name":"Cam","components":[
      {"type":"Transform","position":[0.0,2.0,6.0]},
      {"type":"Camera","fov":50.0,"near":0.1,"far":100.0,"active":true}]},
    {"name":"Lake","components":[
      {"type":"Transform","scale":[20.0,1.0,20.0]},
      {"type":"Water"}]},
    {"name":"Nameless","components":[
      {"type":"Transform","position":[0.0,1.0,0.0]},
      {"type":"RigidBody","body":"dynamic"},
      {"type":"Collider","shape":"sphere","radius":0.5},
      {"type":"Buoyancy"}]},
    {"name":"WrongTarget","components":[
      {"type":"Transform","position":[2.0,1.0,0.0]},
      {"type":"RigidBody","body":"dynamic"},
      {"type":"Collider","shape":"sphere","radius":0.5},
      {"type":"Buoyancy","water":"Cam"}]},
    {"name":"Typo","components":[
      {"type":"Transform","position":[4.0,1.0,0.0]},
      {"type":"RigidBody","body":"dynamic"},
      {"type":"Collider","shape":"sphere","radius":0.5},
      {"type":"Buoyancy","water":"Laike"}]},
    {"name":"Statue","components":[
      {"type":"Transform","position":[6.0,1.0,0.0]},
      {"type":"RigidBody","body":"fixed"},
      {"type":"Collider","shape":"sphere","radius":0.5},
      {"type":"Buoyancy","water":"Lake"}]}
  ]}"#;
    let scene = scene_file("buoyancy-broken", broken);
    let output = engine().arg("validate").arg(&scene).output().unwrap();
    assert_eq!(output.status.code(), Some(1));

    let lines = stderr_lines(&output);
    let codes = codes_of(&lines);
    for expected in [
        "buoyancy_water_missing",
        "buoyancy_water_invalid",
        "buoyancy_water_not_found",
        "buoyancy_without_body",
    ] {
        assert!(
            codes.iter().any(|code| code == expected),
            "expected {expected} among {codes:?}"
        );
    }

    // A near miss suggests the real surface, like every other name error here.
    let typo = lines
        .iter()
        .find(|l| l["error"] == "buoyancy_water_not_found")
        .expect("the typo case is reported");
    assert_eq!(typo["did_you_mean"], "Lake");

    // A fixed body cannot take a force, so the component on it is inert — that
    // is the failure the render cannot show you and the reason this is an error
    // rather than a warning.
    let inert = lines
        .iter()
        .find(|l| l["error"] == "buoyancy_without_body")
        .expect("the fixed body is reported");
    assert_eq!(inert["entity"], "Statue");
}

/// The M37 fixture: a launcher firing entities that did not exist when the
/// scene loaded, pinned bit-exactly.
///
/// **The arc of five shots is the assertion.** Nothing in the file draws a
/// sphere — `Shot` is a `templates` entry, declared and not instantiated — so
/// every ball in the frame was spawned by a script, given a velocity through
/// the ordinary API on the line after its spawn, simulated by rapier, and
/// reaped by name. A spawn that silently did nothing, arrived a step late, or
/// never reached physics all render as a frame with no spheres in it.
#[test]
fn the_spawn_fixture_matches_its_baseline() {
    let scene = repo_path("examples/scenes/verify/m37_spawn.json");
    let baseline = repo_path("examples/scenes/verify/baselines/m37_spawn.png");

    let diff = engine()
        .arg("diff-render")
        .arg(&scene)
        .arg(&baseline)
        .arg("--steps")
        .arg("120")
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
}

/// The report, the trace and the bake all have to say a spawn happened — a
/// picture cannot answer "did my gun fire", which is the whole reason the
/// query commands exist (M24/M25).
#[test]
fn simulate_reports_traces_and_bakes_what_a_run_spawned() {
    let scene = repo_path("examples/scenes/verify/m37_spawn.json");
    let dir = std::env::temp_dir().join(format!("engine-m37-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let trace = dir.join("m37_spawn.jsonl");

    let output = engine()
        .arg("simulate")
        .arg(&scene)
        .args(["--steps", "120"])
        .arg("--trace")
        .arg(&trace)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", stderr_lines(&output));
    let report: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();

    // A total, not a live count: eleven were fired and five are still up.
    assert_eq!(report["spawned"], 11, "{report}");
    assert_eq!(report["hud"][0], "shots in flight 5/6", "{report}");

    // Spawned bodies are ordinary entities from the moment they exist, so they
    // are in the report's `entities` array like anything else.
    let names: Vec<String> = report["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["entity"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"Shot#11".to_string()), "{names:?}");
    assert!(
        !names.contains(&"Shot#1".to_string()),
        "the first shot was reaped: {names:?}"
    );

    // The trace records both halves as events, so a run is greppable.
    let lines = std::fs::read_to_string(&trace).unwrap();
    assert!(lines.contains(r#""spawned":"Shot#1""#), "no spawn event");
    assert!(lines.contains(r#""despawned":"Shot#1""#), "no despawn event");

    // And the names are never reused: `Shot#1` is spawned once in the whole
    // run, however many times its slot is freed.
    let spawns = lines
        .lines()
        .filter(|l| l.contains(r#""spawned":"Shot#1""#) && !l.contains("Shot#1x"))
        .count();
    assert_eq!(spawns, 1, "a name was reused");
}
