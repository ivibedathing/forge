//! The winit application handler for windowed viewing.

use std::sync::Arc;

use engine_core::{EngineError, Result};
use engine_render::WindowTarget;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

pub struct ViewerApp {
    title: String,
    width: u32,
    height: u32,

    // Populated on `resumed`, which is the only point at which winit guarantees
    // a window can be created. On desktop this fires once at startup.
    window: Option<Arc<Window>>,
    target: Option<WindowTarget>,

    /// Set when a frame fails unrecoverably; drained by `into_result` so the
    /// process can exit non-zero with structured JSON.
    error: Option<EngineError>,
}

impl ViewerApp {
    pub fn new(title: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            title: title.into(),
            width,
            height,
            window: None,
            target: None,
            error: None,
        }
    }

    /// The error that ended the loop, if any.
    pub fn into_result(self) -> Result<()> {
        match self.error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Record a fatal error and ask the loop to unwind.
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: EngineError) {
        self.error = Some(error);
        event_loop.exit();
    }
}

impl ApplicationHandler for ViewerApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(winit::dpi::LogicalSize::new(self.width, self.height));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                return self.fail(
                    event_loop,
                    EngineError::new(
                        "window_creation_failed",
                        format!("could not create a window: {e}"),
                    ),
                );
            }
        };

        let size = window.inner_size();
        match WindowTarget::new(window.clone(), size.width, size.height) {
            Ok(target) => {
                self.window = Some(window);
                self.target = Some(target);
            }
            Err(e) => self.fail(event_loop, e),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(target) = self.target.as_mut() else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                target.resize(size.width, size.height);
            }

            WindowEvent::RedrawRequested => {
                if let Err(e) = target.render() {
                    return self.fail(event_loop, e);
                }
                // Keep drawing. M0 has no simulation to step, so this is just a
                // continuous redraw; a real frame pacer arrives with the
                // runtime loop.
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }

            _ => {}
        }
    }
}
