//! The asset pass of `engine validate`: do the referenced mesh files parse?
//!
//! `engine-core`'s validation checks every asset *reference* (existence,
//! extension) but cannot open a glTF file without this crate. This pass loads
//! each distinct file-backed mesh once and reports every failure, so an agent
//! learns about a corrupt asset from `engine validate` rather than from a
//! failed screenshot three commands later.

use std::collections::HashMap;
use std::path::Path;

use engine_core::components::ComponentData;
use engine_core::error::EngineError;
use engine_core::lineindex::LineIndex;
use engine_core::mesh::MeshAsset;
use engine_core::SceneFile;

/// Check every mesh asset a scene references. Empty result: all load.
///
/// Expects a scene that already passed structural validation; a source that
/// does not even parse returns no errors here, because the structural pass has
/// already reported it.
pub fn validate_scene_assets(source: &str, path: &str) -> Vec<EngineError> {
    let Ok(file) = serde_json::from_str::<SceneFile>(source) else {
        return Vec::new();
    };

    let index = LineIndex::new(source);
    let base_dir = Path::new(path).parent().unwrap_or(Path::new(""));

    // Load each distinct asset string once; a shared broken file is still
    // reported at every reference, since each is a place to fix it.
    let mut verdicts: HashMap<&str, Option<EngineError>> = HashMap::new();
    let mut errors = Vec::new();

    for (entity_index, entity) in file.entities.iter().enumerate() {
        for (component_index, component) in entity.components.iter().enumerate() {
            let ComponentData::Mesh(mesh) = component else {
                continue;
            };

            let verdict = verdicts
                .entry(mesh.asset.as_str())
                .or_insert_with(|| check_asset(&mesh.asset, base_dir));

            if let Some(template) = verdict {
                let json_path = format!("/entities/{entity_index}/components/{component_index}/asset");
                let mut error = template
                    .clone()
                    .file(path)
                    .entity(entity.name.clone())
                    .component("Mesh")
                    .field("asset");
                if let Some(line) = index.line_of_or_parent(&json_path) {
                    error = error.line(line);
                }
                errors.push(error.path(json_path));
            }
        }
    }

    errors
}

/// Load one asset to the point of proving it usable. The returned error, if
/// any, is a template — location context is attached per reference.
fn check_asset(asset: &str, base_dir: &Path) -> Option<EngineError> {
    match MeshAsset::resolve(asset, base_dir) {
        Ok(MeshAsset::Builtin(_)) => None,
        Ok(MeshAsset::File(path)) => crate::gltf_mesh::load_gltf(&path).err(),
        Err(e) => Some(e),
    }
}
