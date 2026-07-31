//! Rendering: GPU acquisition, render pipelines, and presentation.
//!
//! The module split here encodes a design constraint from the design doc:
//! headless rendering is a first-class path, not a special case. `gpu`,
//! `renderer`, `scene_renderer`, and `offscreen` know nothing about windows, so
//! `engine screenshot` uses them directly.

pub mod diff;
pub mod digest;
pub mod gpu;
pub mod hud;
pub mod offscreen;
pub mod renderer;
pub mod scene_renderer;
pub mod window;

pub use gpu::Gpu;
pub use offscreen::Image;
pub use renderer::{Frame, Renderer};
pub use scene_renderer::SceneRenderer;
pub use window::WindowTarget;
