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

    /// Open a window and render a scene.
    RunScene {
        scene: PathBuf,
        /// Render from this entity's camera instead of the active one.
        #[arg(long)]
        camera: Option<String>,
        #[arg(long, default_value_t = 1280)]
        width: u32,
        #[arg(long, default_value_t = 720)]
        height: u32,
    },

    /// Render a scene headlessly to a PNG — the agent's eyes.
    Screenshot {
        scene: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Simulate this many physics steps first — edit, simulate, LOOK.
        #[arg(long, default_value_t = 0)]
        steps: u32,
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
    },

    /// Check scenes against the component schemas; report every error.
    Validate {
        #[arg(required = true, num_args = 1..)]
        scenes: Vec<PathBuf>,
        /// Treat warnings as errors (the CI mode).
        #[arg(long)]
        strict: bool,
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
        } => run_scene(scene, camera.as_deref(), width, height),
        Command::Screenshot {
            scene,
            out,
            steps,
            camera,
            width,
            height,
        } => screenshot(scene, out, steps, camera.as_deref(), width, height),
        Command::DiffRender {
            scene,
            baseline,
            steps,
            out,
            camera,
            threshold,
            max_diff_percent,
        } => diff_render(
            scene,
            baseline,
            steps,
            out,
            camera.as_deref(),
            threshold,
            max_diff_percent,
        ),
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
            bake,
            trace,
        } => simulate::simulate_command(scene, steps, bake, trace),
        Command::Raycast {
            scene,
            from,
            dir,
            steps,
        } => simulate::raycast_command(scene, from, dir, steps),
        Command::Validate { scenes, strict } => validate(&scenes, strict),
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

/// `engine diff-render` — render the scene at the baseline's dimensions and
/// compare. The report goes to stdout on *both* pass and fail (a documented
/// exception to "nothing on stdout on failure": a failing run still tells
/// the agent exactly how much differs and where); on mismatch, additionally
/// `render_mismatch` on stderr and exit 1.
fn diff_render(
    scene_path: PathBuf,
    baseline_path: PathBuf,
    steps: u32,
    out: Option<PathBuf>,
    camera_name: Option<&str>,
    threshold: u8,
    max_diff_percent: f64,
) -> Result<()> {
    // Baseline first: it is cheap, needs no GPU, and defines the render size.
    let baseline = load_baseline(&baseline_path)?;

    let mut scene = load_scene(&scene_path)?;
    if steps > 0 {
        simulate::run(&mut scene, steps, None)?;
    }
    let (camera, camera_transform) = scene.camera(camera_name)?;
    let assets = engine_assets::AssetServer::for_scene(&scene_path);
    let items = scene.render_items(&assets)?;

    let (actual, adapter) = engine_render::offscreen::render_with_adapter(
        &items,
        &camera,
        camera_transform.matrix(),
        scene.lights().resolved(),
        baseline.width,
        baseline.height,
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

fn screenshot(
    scene_path: PathBuf,
    out: PathBuf,
    steps: u32,
    camera_name: Option<&str>,
    width: u32,
    height: u32,
) -> Result<()> {
    let mut scene = load_scene(&scene_path)?;
    if steps > 0 {
        simulate::run(&mut scene, steps, None)?;
    }
    let (camera, camera_transform) = scene.camera(camera_name)?;
    let assets = engine_assets::AssetServer::for_scene(&scene_path);
    let items = scene.render_items(&assets)?;
    let drawn = items.len();

    let image = engine_render::offscreen::render(
        &items,
        &camera,
        camera_transform.matrix(),
        scene.lights().resolved(),
        width,
        height,
    )?;

    let png = image::RgbaImage::from_raw(image.width, image.height, image.pixels)
        .expect("offscreen::render returns exactly width*height*4 bytes");
    png.save(&out).map_err(|e| {
        EngineError::new(codes::PNG_WRITE_FAILED, format!("could not write PNG: {e}"))
            .file(out.display().to_string())
    })?;

    println!(
        "{}",
        serde_json::json!({
            "written": out.display().to_string(),
            "width": image.width,
            "height": image.height,
            "scene": scene.name,
            "entities_drawn": drawn,
        })
    );
    Ok(())
}

fn run_scene(
    scene_path: PathBuf,
    camera_name: Option<&str>,
    width: u32,
    height: u32,
) -> Result<()> {
    let scene = load_scene(&scene_path)?;
    let (camera, camera_transform) = scene.camera(camera_name)?;
    let assets = engine_assets::AssetServer::for_scene(&scene_path);
    let items = scene.render_items(&assets)?;
    let lights = scene.lights().resolved();
    let title = format!("engine — {}", scene.name);

    // Physics scenes come alive in the viewer; static scenes stay static.
    let simulation = if engine_physics::PhysicsWorld::scene_has_physics(&scene.world) {
        let physics = engine_physics::PhysicsWorld::build(&scene.world, &scene.physics)?;
        Some(crate::app::Simulation {
            scene,
            physics,
            assets,
            accumulator: 0.0,
            last: None,
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
            camera,
            camera_model: camera_transform.matrix(),
            lights,
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
