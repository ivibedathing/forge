//! Rendering: GPU acquisition, the render pipeline, and windowed presentation.
//!
//! The module split here encodes a design constraint from the design doc:
//! headless rendering is a first-class path, not a special case. `gpu` and
//! `renderer` know nothing about windows, so `engine screenshot` can use them
//! directly.

pub mod gpu;
pub mod renderer;
pub mod window;

pub use gpu::Gpu;
pub use renderer::{Frame, Renderer};
pub use window::WindowTarget;
