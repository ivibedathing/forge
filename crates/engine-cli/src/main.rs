//! The `engine` binary.
//!
//! Every command exits non-zero on failure and prints a single line of JSON to
//! stderr. Human-facing prose goes to stdout; machine-facing errors go to
//! stderr. Nothing in this binary should ever `panic!` on a user-reachable
//! path — a panic is an unparseable error, which defeats the point.

mod app;

use clap::{Parser, Subcommand};
use engine_core::{EngineError, Result};
use engine_render::Gpu;
use winit::event_loop::{ControlFlow, EventLoop};

use crate::app::ViewerApp;

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
    /// Open a window and render the M0 triangle.
    ///
    /// Takes no scene yet — scene loading arrives at M2. This exists to prove
    /// the wgpu/winit stack works end to end.
    Run {
        #[arg(long, default_value_t = 1280)]
        width: u32,
        #[arg(long, default_value_t = 720)]
        height: u32,
    },

    /// Print the selected GPU adapter as JSON.
    Info,
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Run { width, height } => run(width, height),
        Command::Info => info(),
    };

    if let Err(error) = result {
        error.emit();
        std::process::exit(1);
    }
}

fn run(width: u32, height: u32) -> Result<()> {
    let event_loop = EventLoop::new().map_err(|e| {
        EngineError::new(
            "event_loop_creation_failed",
            format!("could not create an event loop: {e}"),
        )
    })?;

    // Poll rather than Wait: M0 redraws continuously, and this is the mode a
    // real-time loop will want anyway.
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = ViewerApp::new("engine — M0", width, height);

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
