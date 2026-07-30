//! The `engine` binary.
//!
//! Every command exits non-zero on failure and prints structured JSON errors
//! to stderr, one per line. Machine-facing success output goes to stdout as
//! JSON too. The full contract — streams, exit codes, the wire format — is
//! `docs/cli-contract.md`; the short version is that an agent can operate
//! every command with `jq` and `$?` alone. Nothing in this binary should ever
//! `panic!` on a user-reachable path, and if something does anyway, the panic
//! hook keeps even that inside the protocol.

mod app;
mod build;
mod simulate;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use engine_core::{codes, EngineError, Result, Scene};
use engine_render::Gpu;
use winit::event_loop::{ControlFlow, EventLoop};

use crate::app::{Content, ViewerApp};

#[derive(Parser)]
#[command(
    name = "engine",
    about = "Agent-native 3D engine",
    version,
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Open a window and render the M0 triangle (no scene; stack proof).
    Run {
        #[arg(long, default_value_t = 1280)]
        width: u32,
        #[arg(long, default_value_t = 720)]
        height: u32,
    },

    /// Open a window and render a scene. The keyboard drives scripts that
    /// call world.key(); --record-input turns a play session into a
    /// replayable timeline.
    RunScene {
        scene: PathBuf,
        /// Render from this entity's camera instead of the active one.
        #[arg(long)]
        camera: Option<String>,
        #[arg(long, default_value_t = 1280)]
        width: u32,
        #[arg(long, default_value_t = 720)]
        height: u32,
        /// Record the held-key timeline to this .input.jsonl file — replay
        /// it headlessly with --input on simulate/screenshot/diff-render.
        #[arg(long)]
        record_input: Option<PathBuf>,
    },

    /// Render a scene headlessly to a PNG — the agent's eyes.
    Screenshot {
        scene: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Simulate this many physics steps first — edit, simulate, LOOK.
        #[arg(long, default_value_t = 0)]
        steps: u32,
        /// Replay this .input.jsonl timeline while stepping.
        #[arg(long)]
        input: Option<PathBuf>,
        /// Render the animated pose at this scene time (seconds).
        #[arg(long, default_value_t = 0.0)]
        time: f32,
        /// Render from this entity's camera instead of the active one.
        #[arg(long)]
        camera: Option<String>,
        #[arg(long, default_value_t = 1280)]
        width: u32,
        #[arg(long, default_value_t = 720)]
        height: u32,
    },

    /// Render a scene and compare it against a baseline PNG.
    ///
    /// The scene renders at exactly the baseline's dimensions. Defaults are
    /// bit-exact — right for same-machine regression checks; cross-adapter
    /// comparisons start at --threshold 3 --max-diff-percent 0.1 and tighten
    /// using the report's numbers. Bless baselines with `engine screenshot`.
    DiffRender {
        scene: PathBuf,
        baseline: PathBuf,
        /// Simulate this many physics steps before rendering — how baked
        /// moments of simulation get visual regression coverage.
        #[arg(long, default_value_t = 0)]
        steps: u32,
        /// Replay this .input.jsonl timeline while stepping.
        #[arg(long)]
        input: Option<PathBuf>,
        /// Compare the animated pose at this scene time (seconds).
        #[arg(long, default_value_t = 0.0)]
        time: f32,
        /// Write the visual diff here (red: violation, yellow: within
        /// threshold, faded gray: identical). Written on pass and fail.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Render from this entity's camera instead of the active one.
        #[arg(long)]
        camera: Option<String>,
        /// Per-channel byte tolerance before a pixel counts as differing.
        #[arg(long, default_value_t = 0)]
        threshold: u8,
        /// Allowed percentage of differing pixels.
        #[arg(long, default_value_t = 0.0)]
        max_diff_percent: f64,
    },

    /// Open the GUI editor: a live, writable view onto the scene file.
    Edit {
        scene: PathBuf,
        /// Read-only supervision mode: full viewport and inspection, writes
        /// disabled.
        #[arg(long)]
        watch: bool,
        /// Write one screenshot of the editor and exit (agent verification).
        #[arg(long, hide = true)]
        self_screenshot: Option<PathBuf>,
        /// Delay before the self-screenshot, in milliseconds.
        #[arg(long, hide = true, default_value_t = 1500)]
        self_screenshot_after_ms: u64,
    },

    /// Contact sheet: N animation frames sampled evenly over a time range,
    /// tiled into one PNG — how motion becomes visible in a single image.
    Filmstrip {
        scene: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 0.0)]
        start: f32,
        /// Defaults to the longest clip duration in the scene.
        #[arg(long)]
        end: Option<f32>,
        #[arg(long, default_value_t = 8)]
        frames: u32,
        #[arg(long, default_value_t = 4)]
        columns: u32,
        /// Per-tile size.
        #[arg(long, default_value_t = 320)]
        width: u32,
        #[arg(long, default_value_t = 180)]
        height: u32,
        /// Render from this entity's camera instead of the active one.
        #[arg(long)]
        camera: Option<String>,
    },

    /// Every animation clip reachable from a scene or clip file, as JSON.
    ListAnimations {
        /// A scene file or a .anim.json clip file.
        path: Option<PathBuf>,
        /// Print the clip-file JSON Schema instead.
        #[arg(long)]
        schema: bool,
    },

    /// Step physics headlessly: build the world, advance N fixed steps.
    ///
    /// --bake writes the settled state back out as a valid scene file with
    /// every untouched byte preserved; --trace writes JSONL (one line per
    /// dynamic body per step, plus contact events) — the greppable,
    /// committable record of what happened.
    Simulate {
        scene: PathBuf,
        #[arg(long)]
        steps: u32,
        /// Replay this .input.jsonl timeline while stepping.
        #[arg(long)]
        input: Option<PathBuf>,
        #[arg(long)]
        bake: Option<PathBuf>,
        #[arg(long)]
        trace: Option<PathBuf>,
    },

    /// Cast a ray into the (optionally pre-simulated) scene; JSON result.
    Raycast {
        scene: PathBuf,
        /// Ray origin as x,y,z
        #[arg(long)]
        from: String,
        /// Ray direction as x,y,z (need not be normalized)
        #[arg(long)]
        dir: String,
        /// Simulate this many steps before casting.
        #[arg(long, default_value_t = 0)]
        steps: u32,
        /// Replay this .input.jsonl timeline while stepping.
        #[arg(long)]
        input: Option<PathBuf>,
    },

    /// Check scenes against the component schemas; report every error.
    Validate {
        #[arg(required = true, num_args = 1..)]
        scenes: Vec<PathBuf>,
        /// Treat warnings as errors (the CI mode).
        #[arg(long)]
        strict: bool,
    },

    /// Print a Road's sampled centerline: where the road is, which way it
    /// faces, and how far along that is.
    ///
    /// What anything *furnishing* a road needs — guardrails, signs, start
    /// lights. Publishing it is what stops a generator from re-implementing
    /// the sampler and drifting out of agreement with the ribbon it decorates.
    RoadCenterline {
        scene: PathBuf,
        /// Which road, when the scene has more than one. Defaults to the only
        /// one; with several, naming one is required.
        #[arg(long)]
        entity: Option<String>,
    },

    /// Print the component and scene JSON Schemas.
    ListComponents,

    /// Compile the workspace, re-emitting rustc diagnostics as engine errors.
    Build {
        /// Type-check only (cargo check): the same errors in half the time.
        #[arg(long)]
        check: bool,
    },

    /// Print the selected GPU adapter as JSON.
    Info,

    /// Panic on purpose — exists so tests can prove the panic hook keeps
    /// even a crash inside the JSON protocol. Debug builds only.
    #[cfg(debug_assertions)]
    #[command(hide = true)]
    DebugPanic,
}

fn main() {
    install_panic_hook();

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => exit_with_clap_error(error),
    };

    let result = match cli.command {
        Command::Run { width, height } => run_triangle(width, height),
        Command::RunScene {
            scene,
            camera,
            width,
            height,
            record_input,
        } => run_scene(scene, camera.as_deref(), width, height, record_input),
        Command::Screenshot {
            scene,
            out,
            steps,
            input,
            time,
            camera,
            width,
            height,
        } => screenshot(scene, out, steps, input, time, camera.as_deref(), width, height),
        Command::DiffRender {
            scene,
            baseline,
            steps,
            input,
            time,
            out,
            camera,
            threshold,
            max_diff_percent,
        } => diff_render(
            scene,
            baseline,
            steps,
            input,
            time,
            out,
            camera.as_deref(),
            threshold,
            max_diff_percent,
        ),
        Command::Filmstrip {
            scene,
            out,
            start,
            end,
            frames,
            columns,
            width,
            height,
            camera,
        } => filmstrip(
            scene,
            out,
            start,
            end,
            frames,
            columns,
            width,
            height,
            camera.as_deref(),
        ),
        Command::ListAnimations { path, schema } => list_animations(path, schema),
        Command::Edit {
            scene,
            watch,
            self_screenshot,
            self_screenshot_after_ms,
        } => engine_editor::run(engine_editor::EditorOptions {
            scene,
            watch_only: watch,
            screenshot: self_screenshot,
            screenshot_after_ms: self_screenshot_after_ms,
        }),
        Command::Simulate {
            scene,
            steps,
            input,
            bake,
            trace,
        } => simulate::simulate_command(scene, steps, input, bake, trace),
        Command::Raycast {
            scene,
            from,
            dir,
            steps,
            input,
        } => simulate::raycast_command(scene, from, dir, steps, input),
        Command::Validate { scenes, strict } => validate(&scenes, strict),
        Command::RoadCenterline { scene, entity } => road_centerline(scene, entity),
        Command::ListComponents => {
            print!("{}", engine_core::schema::canonical_json());
            Ok(())
        }
        Command::Build { check } => build::build(check),
        Command::Info => info(),
        #[cfg(debug_assertions)]
        Command::DebugPanic => panic!("deliberate panic from the hidden debug-panic subcommand"),
    };

    if let Err(error) = result {
        error.emit();
        std::process::exit(error.exit_code());
    }
}

/// Even a bug must speak the protocol: any panic prints one `EngineError`
/// line and exits 2. With `RUST_BACKTRACE` set, the backtrace rides inside
/// the JSON string — escaped, so the one-object-per-line guarantee holds
/// even then.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload: &str = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s
        } else {
            "non-string panic payload"
        };

        let mut message = format!(
            "internal panic: {payload}; this is an engine bug — the scene and \
             invocation may be fine"
        );
        if std::env::var("RUST_BACKTRACE").is_ok_and(|v| v != "0") {
            message = format!(
                "{message}\nbacktrace:\n{}",
                std::backtrace::Backtrace::force_capture()
            );
        }

        let mut error = EngineError::new(codes::INTERNAL_PANIC, message);
        if let Some(location) = info.location() {
            error = error
                .file(location.file())
                .line(location.line())
                .column(location.column());
        }
        error.emit();
        std::process::exit(2);
    }));
}

/// clap parse failures are the agent path too — a typo'd flag is exactly the
/// mistake an agent makes — so they get JSON like everything else, exit 2.
/// `--help`/`--version` are documentation, not errors, and stay prose.
fn exit_with_clap_error(error: clap::Error) -> ! {
    use clap::error::{ContextKind, ContextValue, ErrorKind};

    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        let _ = error.print();
        std::process::exit(0);
    }

    // The first line of clap's rendering ("error: unrecognized subcommand
    // 'scrnshot'") is the summary; the rest is usage prose.
    let rendered = error.render().to_string();
    let message = rendered
        .lines()
        .next()
        .unwrap_or("invalid invocation")
        .trim_start_matches("error: ")
        .to_string();

    let mut engine_error = EngineError::new(codes::INVALID_INVOCATION, message);

    // clap already computed the near-miss; surface it the same way scene
    // validation does.
    let suggestion = [
        ContextKind::SuggestedSubcommand,
        ContextKind::SuggestedArg,
        ContextKind::ValidValue,
    ]
    .iter()
    .find_map(|kind| match error.get(*kind) {
        Some(ContextValue::String(s)) => Some(s.clone()),
        Some(ContextValue::Strings(s)) => s.first().cloned(),
        _ => None,
    });
    if let Some(suggestion) = suggestion {
        engine_error = engine_error.did_you_mean(suggestion);
    }

    engine_error.emit();
    std::process::exit(2);
}

/// One scene's full validation report: every structural, semantic, and asset
/// diagnostic, emitted to stderr in file order.
pub(crate) struct SceneReport {
    pub(crate) source: Option<String>,
    pub(crate) errors: usize,
    pub(crate) warnings: usize,
}

/// Run the whole validation pipeline on one file and emit every diagnostic.
/// Every scene-consuming command calls this, so which command you ran never
/// changes what you learn about a broken scene — the diagnostics are
/// byte-identical to `engine validate`'s.
pub(crate) fn report_scene_diagnostics(path: &PathBuf) -> SceneReport {
    let display = path.display().to_string();
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(e) => {
            EngineError::new(codes::SCENE_UNREADABLE, format!("could not read scene: {e}"))
                .file(&display)
                .emit();
            return SceneReport {
                source: None,
                errors: 1,
                warnings: 0,
            };
        }
    };

    // Structural pass first; the asset pass assumes a well-formed scene, and
    // mixing "your JSON is wrong" with "your glTF is corrupt" in one report
    // would double-report every reference the structural pass rejected.
    let mut diagnostics = engine_core::validate::validate_source(&source, &display);
    if diagnostics.iter().all(EngineError::is_warning) {
        diagnostics.extend(engine_assets::validate_scene_assets(&source, &display));
    }
    if diagnostics.iter().all(EngineError::is_warning) {
        diagnostics.extend(engine_script::validate_scene_scripts(&source, &display));
    }

    let mut errors = 0;
    let mut warnings = 0;
    for diagnostic in &diagnostics {
        if diagnostic.is_warning() {
            warnings += 1;
        } else {
            errors += 1;
        }
        diagnostic.emit();
    }

    SceneReport {
        source: Some(source),
        errors,
        warnings,
    }
}

/// `engine validate` — every diagnostic for every file, then an aggregate
/// verdict. `--strict` promotes warnings to errors, for CI.
fn validate(scenes: &[PathBuf], strict: bool) -> Result<()> {
    let mut errors = 0usize;
    let mut warnings = 0usize;

    for path in scenes {
        // A clip file validates as a clip ("tracks", no "entities"): the
        // same all-at-once contract, structural checks only — entity-name
        // resolution needs a scene.
        let is_clip = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .is_some_and(|v| v.get("tracks").is_some() && v.get("entities").is_none());
        if is_clip {
            let display = path.display().to_string();
            let source = std::fs::read_to_string(path).unwrap_or_default();
            let diagnostics =
                engine_core::animation::validate_clip_source(&source, &display);
            for diagnostic in &diagnostics {
                diagnostic.emit();
            }
            errors += diagnostics.len();
            continue;
        }
        let report = report_scene_diagnostics(path);
        errors += report.errors;
        warnings += report.warnings;
    }

    let files = scenes.len();
    if errors > 0 || (strict && warnings > 0) {
        let strict_note = if errors == 0 {
            "; --strict treats warnings as errors"
        } else {
            ""
        };
        return Err(EngineError::new(
            codes::VALIDATION_FAILED,
            format!("{errors} error(s) and {warnings} warning(s) across {files} file(s){strict_note}"),
        ));
    }

    println!(
        "{}",
        serde_json::json!({
            "valid": true,
            "files": files,
            "errors": 0,
            "warnings": warnings,
        })
    );
    Ok(())
}

/// Load a scene for rendering, reporting *all* validation errors first.
fn load_scene(path: &PathBuf) -> Result<Scene> {
    let report = report_scene_diagnostics(path);
    let display = path.display().to_string();

    if report.errors > 0 {
        return Err(EngineError::new(
            codes::VALIDATION_FAILED,
            format!("{} error(s) in {display}", report.errors),
        )
        .file(&display));
    }

    let source = report.source.unwrap_or_default();
    Scene::from_source(&source, &display).map_err(|mut errors| {
        // Validation was clean, so this is the desync backstop; emit any
        // surplus records and surface the last as the command's result.
        let last = errors.pop().unwrap_or_else(|| {
            EngineError::new(
                codes::SCENE_PARSE_DESYNC,
                "scene failed to load after clean validation",
            )
            .file(&display)
        });
        for error in errors {
            error.emit();
        }
        last
    })
}

/// `engine road-centerline` — publish a road's sampled centerline (M19).
///
/// The road's geometry is generated from its polygon of corners, and anything
/// placed *along* it — a guardrail, a sign, a start light — needs the same
/// samples the ribbon was built from. Re-deriving them in a generator is how
/// two implementations of one curve start disagreeing about where the road is,
/// so the engine publishes the one it actually used.
///
/// The transform is applied, so the positions are world space.
fn road_centerline(scene_path: PathBuf, entity: Option<String>) -> Result<()> {
    let scene = load_scene(&scene_path)?;
    let roads = scene.road_items();

    let road = match (&entity, roads.len()) {
        (Some(name), _) => roads
            .iter()
            .find(|item| item.entity == *name)
            .ok_or_else(|| {
                EngineError::new(
                    codes::ENTITY_NOT_FOUND,
                    format!("no entity named {name:?} with a Road component"),
                )
                .entity(name)
                .file(scene_path.display().to_string())
                .suggest_from(name, roads.iter().map(|item| item.entity.as_str()))
            })?,
        (None, 1) => &roads[0],
        (None, 0) => {
            return Err(EngineError::new(
                codes::MISSING_COMPONENT,
                "scene has no entity with a Road component",
            )
            .file(scene_path.display().to_string()))
        }
        (None, _) => {
            return Err(EngineError::new(
                codes::MISSING_COMPONENT,
                format!(
                    "scene has {} roads ({}); name one with --entity",
                    roads.len(),
                    roads
                        .iter()
                        .map(|item| item.entity.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
            .file(scene_path.display().to_string()))
        }
    };

    let points: Vec<serde_json::Value> = road
        .surface
        .centerline
        .iter()
        .map(|point| {
            let world = road.model.transform_point3(point.position);
            serde_json::json!({
                "position": [world.x, world.y, world.z],
                "forward": [point.direction.x, point.direction.y],
                "v": point.v,
            })
        })
        .collect();

    println!(
        "{}",
        serde_json::json!({
            "entity": road.entity,
            "length": road.surface.length,
            "width": road.road.width,
            "shoulder": road.road.shoulder,
            "closed": road.road.closed,
            "points": points,
        })
    );
    Ok(())
}

/// `engine diff-render` — render the scene at the baseline's dimensions and
/// compare. The report goes to stdout on *both* pass and fail (a documented
/// exception to "nothing on stdout on failure": a failing run still tells
/// the agent exactly how much differs and where); on mismatch, additionally
/// `render_mismatch` on stderr and exit 1.
#[allow(clippy::too_many_arguments)]
fn diff_render(
    scene_path: PathBuf,
    baseline_path: PathBuf,
    steps: u32,
    input_path: Option<PathBuf>,
    time: f32,
    out: Option<PathBuf>,
    camera_name: Option<&str>,
    threshold: u8,
    max_diff_percent: f64,
) -> Result<()> {
    // Baseline first: it is cheap, needs no GPU, and defines the render size.
    let baseline = load_baseline(&baseline_path)?;
    let input = simulate::load_input(input_path.as_deref())?;

    let mut scene = load_scene(&scene_path)?;
    let players = engine_core::animation::load_players(&scene, &scene_path)?;
    if !players.is_empty() {
        engine_core::animation::apply_all(&mut scene, &players, time);
    }
    let (particles, hud) = if steps > 0 {
        let outcome = simulate::run(&mut scene, &scene_path, steps, input.as_ref(), None)?;
        (outcome.particles.instances(&scene.world), outcome.hud)
    } else {
        (Vec::new(), Vec::new())
    };
    let (camera, camera_transform) = scene.camera(camera_name)?;
    let assets = engine_assets::AssetServer::for_scene(&scene_path);
    let items = scene.render_items(&assets)?;

    let (actual, adapter) = engine_render::offscreen::render_with_adapter(
        &items,
        &scene.water_items(),
        &scene.road_items(),
        &particles,
        &camera,
        camera_transform.matrix(),
        scene.lights().resolved(),
        scene.environment,
        scene_time(time, steps, &scene),
        baseline.width,
        baseline.height,
        &scene.hud_items(),
        &hud,
    )?;

    let (stats, diff_image) = engine_render::diff::diff(&actual, &baseline, threshold)?;

    if let Some(out) = &out {
        // Written on pass too: an all-faded image is itself legible
        // confirmation that nothing moved.
        let png =
            image::RgbaImage::from_raw(diff_image.width, diff_image.height, diff_image.pixels)
                .expect("diff() returns exactly width*height*4 bytes");
        png.save(out).map_err(|e| {
            EngineError::new(codes::PNG_WRITE_FAILED, format!("could not write PNG: {e}"))
                .file(out.display().to_string())
        })?;
    }

    let pass = stats.passes(max_diff_percent);
    let mut report = serde_json::json!({
        "pass": pass,
        "width": actual.width,
        "height": actual.height,
        "diff_pixels": stats.diff_pixels,
        "diff_percent": stats.diff_percent(),
        "max_channel_delta": stats.max_channel_delta,
        "threshold": threshold,
        "max_diff_percent": max_diff_percent,
        "adapter": adapter,
    });
    if let Some(bounds) = stats.bounds {
        report["diff_bounds"] = serde_json::json!({
            "min_x": bounds.min_x,
            "min_y": bounds.min_y,
            "max_x": bounds.max_x,
            "max_y": bounds.max_y,
        });
    }
    if let Some(out) = &out {
        report["diff_image"] = serde_json::json!(out.display().to_string());
    }
    println!("{report}");

    if pass {
        Ok(())
    } else {
        Err(EngineError::new(
            codes::RENDER_MISMATCH,
            format!(
                "{} of {} pixels ({:.3}%) differ from {} (threshold {threshold}, max allowed {max_diff_percent}%)",
                stats.diff_pixels,
                stats.total_pixels,
                stats.diff_percent(),
                baseline_path.display(),
            ),
        )
        .file(baseline_path.display().to_string()))
    }
}

/// Decode a baseline PNG into the comparison image format.
/// The scene clock in seconds, as water reads it (M18).
///
/// Water is the first thing in the engine that is a pure function of *time* and
/// yet belongs to the rendered frame rather than to a clip, so it needs one rule
/// covering both ways a command can say when "now" is:
///
/// - `--time T` names an instant directly, the way it does for animation. It
///   wins when given, so `screenshot --time 2.5` and `diff-render --time 2.5`
///   pin the same wave state down to identical bytes, and a `filmstrip` walking
///   time shows the waves moving.
/// - otherwise `--steps N` has advanced the fixed clock by `N/hz` seconds, and
///   the water is where that much simulated time put it — the same clock the
///   physics, the scripts and the particles in that frame ran on.
///
/// Neither is wall clock, and nothing else in the frame is: this is what keeps
/// a water render reproducible from the file plus its flags (invariant 2). A
/// command that passes both flags gets the explicit `--time`, which is the only
/// combination where the two answers differ.
fn scene_time(time: f32, steps: u32, scene: &engine_core::Scene) -> f32 {
    if time > 0.0 {
        return time;
    }
    steps as f32 / scene.physics.timestep_hz.max(1) as f32
}

fn load_baseline(path: &std::path::Path) -> Result<engine_render::Image> {
    let display = path.display().to_string();

    let bytes = std::fs::read(path).map_err(|e| {
        // Not `asset_not_found`: baselines are not scene assets, and
        // overloading that code would muddy what it means to validation.
        let code = if e.kind() == std::io::ErrorKind::NotFound {
            codes::BASELINE_NOT_FOUND
        } else {
            codes::BASELINE_INVALID
        };
        EngineError::new(code, format!("could not read baseline {display}: {e}")).file(&display)
    })?;

    let decoded = image::load_from_memory(&bytes).map_err(|e| {
        EngineError::new(
            codes::BASELINE_INVALID,
            format!("could not decode baseline {display} as an image: {e}"),
        )
        .file(&display)
    })?;

    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 {
        return Err(EngineError::new(
            codes::BASELINE_INVALID,
            format!("baseline {display} has zero dimensions"),
        )
        .file(&display));
    }

    Ok(engine_render::Image {
        width,
        height,
        pixels: rgba.into_raw(),
    })
}

/// `engine filmstrip` — one PNG, many moments.
#[allow(clippy::too_many_arguments)]
fn filmstrip(
    scene_path: PathBuf,
    out: PathBuf,
    start: f32,
    end: Option<f32>,
    frames: u32,
    columns: u32,
    tile_width: u32,
    tile_height: u32,
    camera_name: Option<&str>,
) -> Result<()> {
    let mut scene = load_scene(&scene_path)?;
    let players = engine_core::animation::load_players(&scene, &scene_path)?;
    let end = end.unwrap_or_else(|| {
        start + engine_core::animation::longest_duration(&players).max(0.001)
    });

    let frames = frames.max(1);
    let columns = columns.max(1).min(frames);
    let rows = frames.div_ceil(columns);
    let (camera, camera_transform) = scene.camera(camera_name)?;
    let assets = engine_assets::AssetServer::for_scene(&scene_path);

    let mut sheet = image::RgbaImage::new(tile_width * columns, tile_height * rows);
    for frame in 0..frames {
        let t = if frames == 1 {
            start
        } else {
            start + (end - start) * frame as f32 / (frames - 1) as f32
        };
        engine_core::animation::apply_all(&mut scene, &players, t);
        let items = scene.render_items(&assets)?;
        // Filmstrip samples animation time only; particles advance with
        // --steps, which filmstrip does not take, so none are drawn. Water is
        // a pure function of time rather than of stepping, so it *does* move
        // across the strip — a filmstrip of a lake is a filmstrip of its waves.
        let rendered = engine_render::offscreen::render(
            &items,
            &scene.water_items(),
            &scene.road_items(),
            &[],
            &camera,
            camera_transform.matrix(),
            scene.lights().resolved(),
            scene.environment,
            t,
            tile_width,
            tile_height,
            &scene.hud_items(),
            &[],
        )?;
        let tile =
            image::RgbaImage::from_raw(rendered.width, rendered.height, rendered.pixels)
                .expect("offscreen render returns exactly width*height*4 bytes");
        let x = (frame % columns) * tile_width;
        let y = (frame / columns) * tile_height;
        image::imageops::replace(&mut sheet, &tile, i64::from(x), i64::from(y));
    }

    sheet.save(&out).map_err(|e| {
        EngineError::new(codes::PNG_WRITE_FAILED, format!("could not write PNG: {e}"))
            .file(out.display().to_string())
    })?;

    println!(
        "{}",
        serde_json::json!({
            "written": out.display().to_string(),
            "frames": frames,
            "start": start,
            "end": end,
            "columns": columns,
            "tile_width": tile_width,
            "tile_height": tile_height,
        })
    );
    Ok(())
}

/// `engine list-animations` — the introspection window (design principle 5).
fn list_animations(path: Option<PathBuf>, schema: bool) -> Result<()> {
    if schema {
        print!("{}", engine_core::schema::canonical_animation_json());
        return Ok(());
    }
    let Some(path) = path else {
        return Err(EngineError::new(
            codes::INVALID_INVOCATION,
            "list-animations needs a scene or clip file (or --schema)",
        ));
    };

    let display = path.display().to_string();
    let source = std::fs::read_to_string(&path).map_err(|e| {
        EngineError::new(codes::SCENE_UNREADABLE, format!("could not read: {e}"))
            .file(&display)
    })?;
    let sniff: serde_json::Value = serde_json::from_str(&source).unwrap_or_default();

    let clip_report = |clip: &engine_core::animation::ClipFile, source_path: &str| {
        serde_json::json!({
            "name": clip.name,
            "source": source_path,
            "duration": engine_core::animation::duration(clip) as f64,
            "tracks": clip.tracks.iter().map(|t| serde_json::json!({
                "entity": t.entity,
                "property": t.property,
                "interpolation": format!("{:?}", t.interpolation).to_lowercase(),
                "keys": t.keys.len(),
            })).collect::<Vec<_>>(),
        })
    };

    let clips: Vec<serde_json::Value> = if sniff.get("tracks").is_some() {
        // A clip file directly.
        let clip: engine_core::animation::ClipFile =
            serde_json::from_str(&source).map_err(|e| {
                EngineError::new(
                    codes::ASSET_LOAD_FAILED,
                    format!("clip does not parse: {e}"),
                )
                .file(&display)
            })?;
        vec![clip_report(&clip, &display)]
    } else {
        // A scene: every player's clip.
        let scene = load_scene(&path)?;
        let players = engine_core::animation::load_players(&scene, &path)?;
        players
            .iter()
            .map(|p| clip_report(&p.clip, &p.player.clip))
            .collect()
    };

    println!("{}", serde_json::json!({ "clips": clips }));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn screenshot(
    scene_path: PathBuf,
    out: PathBuf,
    steps: u32,
    input_path: Option<PathBuf>,
    time: f32,
    camera_name: Option<&str>,
    width: u32,
    height: u32,
) -> Result<()> {
    let input = simulate::load_input(input_path.as_deref())?;
    let mut scene = load_scene(&scene_path)?;
    // System order: sample animations, then physics and particles, then render.
    let players = engine_core::animation::load_players(&scene, &scene_path)?;
    if !players.is_empty() {
        engine_core::animation::apply_all(&mut scene, &players, time);
    }
    let (particles, hud) = if steps > 0 {
        let outcome = simulate::run(&mut scene, &scene_path, steps, input.as_ref(), None)?;
        (outcome.particles.instances(&scene.world), outcome.hud)
    } else {
        (Vec::new(), Vec::new())
    };
    let (camera, camera_transform) = scene.camera(camera_name)?;
    let assets = engine_assets::AssetServer::for_scene(&scene_path);
    let items = scene.render_items(&assets)?;
    let drawn = items.len();

    let image = engine_render::offscreen::render(
        &items,
        &scene.water_items(),
        &scene.road_items(),
        &particles,
        &camera,
        camera_transform.matrix(),
        scene.lights().resolved(),
        scene.environment,
        scene_time(time, steps, &scene),
        width,
        height,
        &scene.hud_items(),
        &hud,
    )?;

    let png = image::RgbaImage::from_raw(image.width, image.height, image.pixels)
        .expect("offscreen::render returns exactly width*height*4 bytes");
    png.save(&out).map_err(|e| {
        EngineError::new(codes::PNG_WRITE_FAILED, format!("could not write PNG: {e}"))
            .file(out.display().to_string())
    })?;

    let mut report = serde_json::json!({
        "written": out.display().to_string(),
        "width": image.width,
        "height": image.height,
        "scene": scene.name,
        "entities_drawn": drawn,
    });
    if !hud.is_empty() {
        report["hud"] = serde_json::json!(hud);
    }
    println!("{report}");
    Ok(())
}

fn run_scene(
    scene_path: PathBuf,
    camera_name: Option<&str>,
    width: u32,
    height: u32,
    record_input: Option<PathBuf>,
) -> Result<()> {
    let scene = load_scene(&scene_path)?;
    let (camera, camera_transform) = scene.camera(camera_name)?;
    let assets = engine_assets::AssetServer::for_scene(&scene_path);
    let items = scene.render_items(&assets)?;
    let water = scene.water_items();
    let roads = scene.road_items();
    let lights = scene.lights().resolved();
    let environment = scene.environment;
    let hud_items = scene.hud_items();
    let title = format!("engine — {}", scene.name);

    // Physics and animated scenes come alive in the viewer; static scenes
    // stay static (unless a recording was asked for, which needs the step
    // clock running to have steps to record against).
    let players = engine_core::animation::load_players(&scene, &scene_path)?;
    let scripts =
        engine_script::ScriptHost::build(&scene.world, &scene_path, scene.physics.timestep_hz)?;
    let has_physics = engine_physics::PhysicsWorld::scene_has_physics(&scene.world);
    let has_emitters =
        engine_core::particles::ParticleSystem::scene_has_emitters(&scene.world);
    let simulation = if has_physics
        || has_emitters
        || !players.is_empty()
        || scripts.is_some()
        || record_input.is_some()
    {
        let physics = if has_physics {
            Some(engine_physics::PhysicsWorld::build(&scene.world, &scene.physics, &assets)?)
        } else {
            None
        };
        let recorder = record_input
            .as_deref()
            .map(crate::app::InputRecorder::create)
            .transpose()?;
        let particles = engine_core::particles::ParticleSystem::build(&scene.world);
        Some(crate::app::Simulation {
            scene,
            physics,
            particles,
            players,
            scripts,
            assets,
            camera_name: camera_name.map(String::from),
            held: engine_core::input::InputState::default(),
            contacts: engine_core::contact::ContactState::default(),
            recorder,
            accumulator: 0.0,
            t: 0.0,
            step_index: 0,
            last: None,
            hud_lines: Vec::new(),
        })
    } else {
        None
    };

    run_app(ViewerApp::new(
        title,
        width,
        height,
        Content::Scene {
            items,
            water,
            roads,
            camera,
            camera_model: camera_transform.matrix(),
            lights,
            environment,
            hud_items,
            simulation,
        },
    ))
}

fn run_triangle(width: u32, height: u32) -> Result<()> {
    run_app(ViewerApp::new(
        "engine — M0",
        width,
        height,
        Content::Triangle,
    ))
}

fn run_app(mut app: ViewerApp) -> Result<()> {
    let event_loop = EventLoop::new().map_err(|e| {
        EngineError::new(
            codes::EVENT_LOOP_CREATION_FAILED,
            format!("could not create an event loop: {e}"),
        )
    })?;

    // Poll rather than Wait: the viewer redraws continuously, and this is the
    // mode a real-time loop will want anyway.
    event_loop.set_control_flow(ControlFlow::Poll);

    event_loop.run_app(&mut app).map_err(|e| {
        EngineError::new(codes::EVENT_LOOP_FAILED, format!("the event loop failed: {e}"))
    })?;

    // A render error inside the loop exits it cleanly; surface it here.
    app.into_result()
}

fn info() -> Result<()> {
    let instance = Gpu::default_instance();
    let gpu = pollster::block_on(Gpu::new(instance, None))?;
    let adapter = gpu.adapter_info();

    let json = serde_json::json!({
        "name": adapter.name,
        "backend": adapter.backend.to_string(),
        "device_type": format!("{:?}", adapter.device_type),
        "driver": adapter.driver,
        "driver_info": adapter.driver_info,
    });

    println!(
        "{}",
        serde_json::to_string_pretty(&json).map_err(|e| {
            EngineError::new(
                codes::OUTPUT_SERIALIZATION_FAILED,
                format!("could not serialize adapter info: {e}"),
            )
        })?
    );

    Ok(())
}
