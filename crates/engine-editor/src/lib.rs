//! The GUI editor (M7): a live, writable *view* onto scene JSON files.
//!
//! The scene file on disk stays the single source of truth (invariant #8).
//! Every editor action is a text edit through `engine_core::formatter`;
//! external edits win within one poll interval; validation is the same code
//! the CLI runs. Deleting this crate leaves the engine whole — the editor is
//! a client of the engine crates, never a fork of them.

mod app;
mod camera;
mod doc;
mod gizmo;
mod inspector;
mod pick;
mod viewport;

use std::path::PathBuf;

use engine_core::{codes, EngineError, Result};

pub struct EditorOptions {
    pub scene: PathBuf,
    /// Read-only supervision mode (`engine edit --watch`).
    pub watch_only: bool,
    /// Agent-verification hook: write one screenshot here and exit.
    pub screenshot: Option<PathBuf>,
    pub screenshot_after_ms: u64,
}

/// Minimal stderr logger so egui/wgpu warnings surface during agent
/// verification runs instead of vanishing.
struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("[{}] {}", record.level(), record.args());
        }
    }
    fn flush(&self) {}
}

/// Open the editor window on a scene file. Blocks until the window closes.
pub fn run(options: EditorOptions) -> Result<()> {
    let _ = log::set_logger(&StderrLogger).map(|()| log::set_max_level(log::LevelFilter::Warn));
    let title = format!(
        "engine edit — {}{}",
        options.scene.display(),
        if options.watch_only { " (read-only)" } else { "" }
    );

    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title(&title),
        ..Default::default()
    };

    eframe::run_native(
        "engine-edit",
        native_options,
        Box::new(move |_cc| Ok(Box::new(app::EditorApp::new(options)))),
    )
    .map_err(|e| {
        EngineError::new(codes::EDITOR_FAILED, format!("the editor could not run: {e}"))
    })
}
