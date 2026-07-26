//! Core engine types: structured errors, math re-exports, and (from M2) the
//! ECS and scene graph.
//!
//! This crate must not depend on the renderer or on any windowing library —
//! headless tooling (`engine validate`, `engine list-components`) links only
//! this, and must stay usable on a machine with no GPU.

pub mod error;

pub use error::{EngineError, Result};

/// Math types, re-exported so downstream crates share one `glam` version.
pub mod math {
    pub use glam::{Mat3, Mat4, Quat, Vec2, Vec3, Vec4};
}
