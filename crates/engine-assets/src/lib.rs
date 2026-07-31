//! Asset loading: glTF meshes and image textures (M3).
//!
//! This crate is the only place the engine opens asset files. `engine-core`
//! decides what an asset *reference* means ([`engine_core::mesh::MeshAsset`]);
//! this crate turns the referenced files into plain CPU data. It links neither
//! wgpu nor winit, so everything here is testable on a machine with no GPU.
//!
//! Errors follow the engine convention (structured, coded, with context):
//! - `asset_not_found` — the reference resolves to nothing on disk
//! - `asset_load_failed` — the file exists but cannot be read or parsed
//! - `asset_unsupported` — the file parses but uses something the engine
//!   does not implement (non-triangle primitives, meshless files, …)

mod gltf_material;
mod gltf_mesh;
mod gltf_skin;
mod server;
mod texture;
mod validate;

pub use gltf_material::{import_materials, Imported};
pub use gltf_mesh::load_gltf;
pub use gltf_skin::load_rig;
pub use server::AssetServer;
pub use texture::load_texture;
pub use validate::validate_scene_assets;
