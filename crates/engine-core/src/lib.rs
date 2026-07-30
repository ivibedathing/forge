//! Core engine types: structured errors, components, scenes, and the ECS world
//! they instantiate.
//!
//! This crate must not depend on the renderer or on any windowing library —
//! headless tooling (`engine validate`, `engine list-components`) links only
//! this, and must stay usable on a machine with no GPU.

pub mod animation;
pub mod codes;
pub mod components;
pub mod contact;
pub mod error;
pub mod formatter;
pub mod input;
pub mod lineindex;
pub mod mesh;
pub mod particles;
pub mod scene;
pub mod schema;
pub mod tree;
pub mod validate;

pub use error::{EngineError, Result};
pub use scene::{Scene, SceneFile};

/// The ECS, re-exported so downstream crates share one `hecs` version.
pub use hecs;

/// Math types, re-exported so downstream crates share one `glam` version.
pub mod math {
    pub use glam::{Mat3, Mat4, Quat, Vec2, Vec3, Vec4};
}
