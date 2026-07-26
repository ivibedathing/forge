//! The `engine` binary.
//!
//! Every command exits non-zero on failure and prints structured JSON errors
//! to stderr, one per line. Machine-facing success output goes to stdout as
//! JSON too. Nothing in this binary should ever `panic!` on a user-reachable
//! path — a panic is an unparseable error, which defeats the point.

mod app;
mod build;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use engine_core::{EngineError, Result, Scene};
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
        /// Render from this entity's camera instead of the active one.
        #[arg(long)]
        camera: Option<String>,
        #[arg(long, default_value_t = 1280)]
        width: u32,
        #[arg(long, default_value_t = 720)]
        height: u32,
    },

    /// Check a scene against the component schemas; report every error.
    Validate { scene: PathBuf },

    /// Print the component and scene JSON Schemas.
    ListComponents,

    /// Compile the workspace, re-emitting rustc diagnostics as engine errors.
    Build,

    /// Print the selected GPU adapter as JSON.
    Info,
}

fn main() {
    let cli = Cli::parse();

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
            camera,
            width,
            height,
        } => screenshot(scene, out, camera.as_deref(), width, height),
        Command::Validate { scene } => validate(scene),
        Command::ListComponents => {
            print!("{}", engine_core::schema::canonical_json());
            Ok(())
        }
        Command::Build => build::build(),
        Command::Info => info(),
    };

    if let Err(error) = result {
        error.emit();
        std::process::exit(1);
    }
}

/// `engine validate` — the one command that reports *all* errors, not the
/// first. Success prints a summary an agent can assert on.
fn validate(path: PathBuf) -> Result<()> {
    let display = path.display().to_string();
    let source = std::fs::read_to_string(&path).map_err(|e| {
        EngineError::new("scene_unreadable", format!("could not read scene: {e}")).file(&display)
    })?;

    let errors = engine_core::validate::validate_source(&source, &display);
    if !errors.is_empty() {
        let count = errors.len();
        for error in errors {
            error.emit();
        }
        return Err(EngineError::new(
            "validation_failed",
            format!("{count} error(s) in {display}"),
        )
        .file(&display));
    }

    // Parse is guaranteed to succeed after clean validation.
    let scene = Scene::from_source(&source, &display)?;
    println!(
        "{}",
        serde_json::json!({
            "valid": true,
            "scene": scene.name,
            "entities": scene.entity_count(),
        })
    );
    Ok(())
}

fn screenshot(
    scene_path: PathBuf,
    out: PathBuf,
    camera_name: Option<&str>,
    width: u32,
    height: u32,
) -> Result<()> {
    let scene = Scene::load(&scene_path)?;
    let (camera, camera_transform) = scene.camera(camera_name)?;
    let items = scene.render_items()?;
    let drawn = items.len();

    let image = engine_render::offscreen::render(
        &items,
        &camera,
        camera_transform.matrix(),
        width,
        height,
    )?;

    let png = image::RgbaImage::from_raw(image.width, image.height, image.pixels)
        .expect("offscreen::render returns exactly width*height*4 bytes");
    png.save(&out).map_err(|e| {
        EngineError::new("png_write_failed", format!("could not write PNG: {e}"))
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
    let scene = Scene::load(&scene_path)?;
    let (camera, camera_transform) = scene.camera(camera_name)?;
    let items = scene.render_items()?;
    let title = format!("engine — {}", scene.name);

    run_app(ViewerApp::new(
        title,
        width,
        height,
        Content::Scene {
            items,
            camera,
            camera_model: camera_transform.matrix(),
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
            "event_loop_creation_failed",
            format!("could not create an event loop: {e}"),
        )
    })?;

    // Poll rather than Wait: the viewer redraws continuously, and this is the
    // mode a real-time loop will want anyway.
    event_loop.set_control_flow(ControlFlow::Poll);

    event_loop.run_app(&mut app).map_err(|e| {
        EngineError::new("event_loop_failed", format!("the event loop failed: {e}"))
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
                "output_serialization_failed",
                format!("could not serialize adapter info: {e}"),
            )
        })?
    );

    Ok(())
}
