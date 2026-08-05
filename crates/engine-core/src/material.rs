//! Shareable materials: `Material.asset` and the `materials/*.json` files it
//! names (M26).
//!
//! A material file holds the component's fields minus the `"type"`, so the
//! schema `engine list-components --component Material` publishes describes
//! both forms and there is nothing new to learn. The reference is relative to
//! the scene file, like every other asset (invariant 3), and `engine validate`
//! accepts a material file directly the way M9 made it accept clip files.

use std::path::{Path, PathBuf};

use crate::components::Material;
use crate::error::{EngineError, Result};

/// File extension a material reference must carry.
pub const MATERIAL_EXTENSION: &str = "json";

/// Resolve a `Material.asset` reference against the scene file's directory.
///
/// Checks what can be decided without parsing: relative, `.json`, present.
pub fn resolve_material(asset: &str, base_dir: &Path) -> Result<PathBuf> {
    if Path::new(asset).is_absolute() {
        return Err(EngineError::new(
            crate::codes::ASSET_PATH_NOT_RELATIVE,
            format!(
                "material {asset:?} is an absolute path; assets are referenced \
                 by path relative to the scene file, so scenes stay portable"
            ),
        ));
    }

    let resolved = base_dir.join(asset);
    match resolved.extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case(MATERIAL_EXTENSION) => {}
        _ => {
            return Err(EngineError::new(
                crate::codes::ASSET_UNSUPPORTED,
                format!("material {asset:?} must be a .{MATERIAL_EXTENSION} file"),
            ));
        }
    }

    if !resolved.is_file() {
        return Err(EngineError::new(
            crate::codes::ASSET_NOT_FOUND,
            format!(
                "no material file at {} (asset paths resolve relative to the scene file)",
                resolved.display()
            ),
        ));
    }

    Ok(resolved)
}

/// Rewrite a material file's texture reference to be relative to the scene
/// instead of to the material file.
///
/// **A material file's own references are relative to itself**, which is what
/// makes a material shareable at all: `materials/asphalt.json` saying
/// `"albedo_map": "../textures/asphalt.png"` means the same thing to every
/// scene that names it, from whatever directory. Everything downstream still
/// resolves against the scene, so the join happens once, here, lexically —
/// `..` is popped rather than followed, because the path may not exist yet
/// (`engine import` writes one before anything reads it).
/// `pub(crate)` since M47: a tileset's palette is materials living in *its*
/// directory, so the same join has a second caller (`tileset::rebase_tileset`).
pub(crate) fn rebase(material_asset: &str, reference: &str) -> String {
    let Some(dir) = Path::new(material_asset).parent() else {
        return reference.to_string();
    };
    if dir.as_os_str().is_empty() {
        return reference.to_string();
    }

    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in dir.join(reference).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir if parts.last().is_some_and(|last| last != "..") => {
                parts.pop();
            }
            other => parts.push(other.as_os_str().to_owned()),
        }
    }
    parts
        .iter()
        .map(|p| p.to_string_lossy())
        .collect::<Vec<_>>()
        .join(std::path::MAIN_SEPARATOR_STR)
}

/// Rebase every texture reference a material carries, in place.
///
/// The four-map loop factored out of [`resolve_scene_materials`] when M47's
/// tilesets became its second caller — a palette entry's maps are relative to
/// whichever file spelled them, exactly as a scene's are.
pub(crate) fn rebase_maps(material: &mut Material, asset: &str) {
    for reference in [
        &mut material.albedo_map,
        &mut material.orm_map,
        &mut material.normal_map,
        &mut material.emissive_map,
    ]
    .into_iter()
    .flatten()
    {
        *reference = rebase(asset, reference);
    }
}

/// Read and parse a material file.
///
/// The returned material carries `asset: None` — it *is* the file's contents,
/// and a material file naming another material file is a chain this design does
/// not have (`material_asset_with_fields` forbids the half of it that would be
/// useful, and one owner per material is the point).
pub fn load_material(path: &Path) -> Result<Material> {
    let display = path.display().to_string();
    let source = std::fs::read_to_string(path).map_err(|e| {
        EngineError::new(
            crate::codes::ASSET_NOT_FOUND,
            format!("could not read material {display}: {e}"),
        )
        .file(&display)
    })?;
    let mut material: Material = serde_json::from_str(&source).map_err(|e| {
        EngineError::new(
            crate::codes::ASSET_LOAD_FAILED,
            format!("material {display} does not parse: {e}"),
        )
        .file(&display)
        .line(e.line() as u32)
    })?;
    material.asset = None;
    Ok(material)
}

/// Fill in a scene's file-backed materials, in place.
///
/// Called once at load with the scene's own directory, so everything
/// downstream — the renderer, the editor, a fragment inheriting its parent's
/// material — sees a complete material and never has to know where it came
/// from. The reference stays on the component: it is what serializes (see
/// `Material`'s `Serialize` impl), so a baked scene still points at the file
/// rather than inlining a copy of it.
///
/// Errors are collected rather than returned on the first one, like every other
/// pass here; in practice validation has already reported them and this is the
/// backstop for a `SceneFile` built by hand.
pub fn resolve_scene_materials(file: &mut crate::SceneFile, base_dir: &Path) -> Vec<EngineError> {
    let mut errors = Vec::new();
    let mut cache: std::collections::HashMap<String, Material> = std::collections::HashMap::new();

    for entity in &mut file.entities {
        for component in &mut entity.components {
            let crate::components::ComponentData::Material(material) = component else {
                continue;
            };
            let Some(asset) = material.asset.clone() else {
                continue;
            };
            if let Some(hit) = cache.get(&asset) {
                let mut resolved = hit.clone();
                resolved.asset = Some(asset);
                *material = resolved;
                continue;
            }
            match resolve_material(&asset, base_dir).and_then(|path| load_material(&path)) {
                Ok(mut loaded) => {
                    rebase_maps(&mut loaded, &asset);
                    cache.insert(asset.clone(), loaded.clone());
                    let mut resolved = loaded;
                    resolved.asset = Some(asset);
                    *material = resolved;
                }
                Err(e) => errors.push(e.entity(entity.name.clone()).component("Material")),
            }
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A material file's texture references are relative to *it*, so one
    /// material means the same thing to every scene that names it.
    #[test]
    fn a_material_files_references_rebase_onto_the_scene() {
        assert_eq!(
            rebase("materials/asphalt.json", "../textures/grain.png"),
            "textures/grain.png"
        );
        assert_eq!(
            rebase("materials/asphalt.json", "grain.png"),
            "materials/grain.png"
        );
        assert_eq!(
            rebase("asphalt.json", "textures/grain.png"),
            "textures/grain.png"
        );
        assert_eq!(
            rebase("a/b/m.json", "../../../shared/t.png"),
            "../shared/t.png",
            "a `..` that escapes the scene directory survives rather than being dropped"
        );
    }

    #[test]
    fn a_material_reference_is_relative_and_json() {
        let dir = Path::new("examples");
        assert_eq!(
            resolve_material("/tmp/m.json", dir).unwrap_err().error,
            "asset_path_not_relative"
        );
        assert_eq!(
            resolve_material("m.mtl", dir).unwrap_err().error,
            "asset_unsupported"
        );
        assert_eq!(
            resolve_material("materials/nope.json", dir)
                .unwrap_err()
                .error,
            "asset_not_found"
        );
    }
}
