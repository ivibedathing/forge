//! The asset pass of `engine validate`: do the referenced files parse?
//!
//! `engine-core`'s validation checks every asset *reference* (existence,
//! extension) but cannot open a glTF file or decode a PNG without this crate.
//! This pass loads each distinct file-backed mesh and texture once and reports
//! every failure, so an agent learns about a corrupt asset from
//! `engine validate` rather than from a failed screenshot three commands later.

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
    let Ok(mut file) = serde_json::from_str::<SceneFile>(source) else {
        return Vec::new();
    };

    let index = LineIndex::new(source);
    let base_dir = Path::new(path).parent().unwrap_or(Path::new(""));

    // Fill in file-backed materials first, so a shared material's maps are
    // decoded here too — they are as much a part of this scene's assets as a
    // map written inline. Reference failures are engine-core's to report.
    let _ = engine_core::material::resolve_scene_materials(&mut file, base_dir);

    // Load each distinct asset string once; a shared broken file is still
    // reported at every reference, since each is a place to fix it.
    let mut verdicts: HashMap<&str, Option<EngineError>> = HashMap::new();
    let mut errors = Vec::new();

    // Textures are keyed on (reference, colour space): the space decides how
    // the mip chain is filtered, so one file read as albedo and as ORM really
    // is two decodes, and a failure in either is worth reporting.
    let mut texture_verdicts: HashMap<
        (String, engine_core::texture::ColorSpace),
        Option<EngineError>,
    > = HashMap::new();

    for (entity_index, entity) in file.entities.iter().enumerate() {
        for (component_index, component) in entity.components.iter().enumerate() {
            let component_path = format!("/entities/{entity_index}/components/{component_index}");

            // A Material's maps (M26). A material that came from a file has
            // had its references rebased onto the scene already, and reports
            // against the scene's own line for the component — the file it
            // came from is one `engine validate` away.
            if let ComponentData::Material(material) = component {
                for (field, asset, space) in material.maps() {
                    let verdict = texture_verdicts
                        .entry((asset.to_string(), space))
                        .or_insert_with(|| check_texture(asset, base_dir, space));
                    let Some(template) = verdict else { continue };
                    let json_path = format!("{component_path}/{field}");
                    let mut error = template
                        .clone()
                        .file(path)
                        .entity(entity.name.clone())
                        .component("Material")
                        .field(field);
                    if let Some(line) = index.line_of_or_parent(&json_path) {
                        error = error.line(line);
                    }
                    errors.push(error.path(json_path));
                }
            }

            // A HudImage (M31). The reference itself is engine-core's, and so
            // is every field range; what needs this crate is the comparison
            // between the nine-slice insets and the *source* image, which
            // means decoding it. Same division as `texture_too_large`, and the
            // same reason it fires from `validate` rather than from a draw:
            // a frame that has quietly clamped its corners is a bug found too
            // late, and the fix is a number the author cannot otherwise see.
            if let ComponentData::HudImage(image) = component {
                let space = engine_core::texture::ColorSpace::Srgb;
                let verdict = texture_verdicts
                    .entry((image.texture.clone(), space))
                    .or_insert_with(|| check_texture(&image.texture, base_dir, space));
                let json_path = format!("{component_path}/texture");
                if let Some(template) = verdict {
                    let mut error = template
                        .clone()
                        .file(path)
                        .entity(entity.name.clone())
                        .component("HudImage")
                        .field("texture");
                    if let Some(line) = index.line_of_or_parent(&json_path) {
                        error = error.line(line);
                    }
                    errors.push(error.path(json_path));
                } else if let Some(error) = check_slice(image, base_dir, &entity.name) {
                    let slice_path = format!("{component_path}/slice");
                    let mut error = error.file(path);
                    if let Some(line) = index.line_of_or_parent(&slice_path) {
                        error = error.line(line);
                    }
                    errors.push(error.path(slice_path));
                }
            }

            // A skeletal AnimationPlayer (M30). The reference-level rules —
            // the fragment being required, the player and the Mesh naming one
            // file — are engine-core's, because they need no file opened.
            // These three do: the file has to have a skin, the fragment has to
            // name a clip that is in it, and the skin has to fit the palette.
            if let ComponentData::AnimationPlayer(player) = component {
                if let engine_core::skeleton::ClipRef::Skeletal { asset, clip } =
                    engine_core::skeleton::ClipRef::parse(&player.clip)
                {
                    let json_path = format!("{component_path}/clip");
                    for mut error in check_rig(asset, clip, base_dir) {
                        error = error
                            .file(path)
                            .entity(entity.name.clone())
                            .component("AnimationPlayer")
                            .field("clip");
                        if let Some(line) = index.line_of_or_parent(&json_path) {
                            error = error.line(line);
                        }
                        errors.push(error.path(json_path.clone()));
                    }
                }
            }

            // Every mesh reference this component holds: a Mesh's `asset`,
            // a Collider's mesh-collider `asset` (M12), or each fragment
            // `mesh` of a Breakable (M14).
            let references: Vec<(String, &str, &str, &str)> = match component {
                ComponentData::Mesh(mesh) => vec![(
                    format!("{component_path}/asset"),
                    mesh.asset.as_str(),
                    "Mesh",
                    "asset",
                )],
                ComponentData::Collider(collider) => match &collider.asset {
                    Some(asset) => vec![(
                        format!("{component_path}/asset"),
                        asset.as_str(),
                        "Collider",
                        "asset",
                    )],
                    None => continue,
                },
                ComponentData::Breakable(breakable) => breakable
                    .fragments
                    .iter()
                    .enumerate()
                    .map(|(i, fragment)| {
                        (
                            format!("{component_path}/fragments/{i}/mesh"),
                            fragment.mesh.as_str(),
                            "Breakable",
                            "mesh",
                        )
                    })
                    .collect(),
                _ => continue,
            };

            for (json_path, asset, component_name, field) in references {
                let verdict = verdicts
                    .entry(asset)
                    .or_insert_with(|| check_asset(asset, base_dir));

                if let Some(template) = verdict {
                    let mut error = template
                        .clone()
                        .file(path)
                        .entity(entity.name.clone())
                        .component(component_name)
                        .field(field);
                    if let Some(line) = index.line_of_or_parent(&json_path) {
                        error = error.line(line);
                    }
                    errors.push(error.path(json_path));
                }
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

/// Open a skeletal player's glTF and check what only the file can answer.
///
/// Returns templates: location context is attached by the caller, the way
/// every other verdict in this pass is.
fn check_rig(asset: &str, clip: &str, base_dir: &Path) -> Vec<EngineError> {
    let path = match MeshAsset::resolve(asset, base_dir) {
        // A `builtin:` primitive is generated geometry; it can no more carry a
        // skin than it can carry a texture, and saying so beats "no skin".
        Ok(MeshAsset::Builtin(_)) => {
            return vec![EngineError::new(
                engine_core::codes::MESH_HAS_NO_SKIN,
                format!(
                    "clip {asset}#{clip} names the builtin primitive {asset:?}, which \
                     has no skeleton; skeletal clips come from glTF files"
                ),
            )]
        }
        Ok(MeshAsset::File(path)) => path,
        // The reference itself is engine-core's to report; this pass would
        // only say it twice.
        Err(_) => return Vec::new(),
    };

    let rig = match crate::gltf_skin::load_rig(&path) {
        Ok(rig) => rig,
        // A file that will not parse is already reported against whatever
        // `Mesh` references it — and if nothing does, the mismatch rule fired.
        Err(_) => return Vec::new(),
    };

    let mut errors = Vec::new();

    match &rig.skin {
        None => errors.push(EngineError::new(
            engine_core::codes::MESH_HAS_NO_SKIN,
            format!(
                "glTF file {asset:?} carries no skin, so a skeletal player on it \
                 could only draw the rest pose forever"
            ),
        )),
        Some(skin) if skin.joints.len() > engine_core::skeleton::MAX_JOINTS => {
            // Before a device exists, rather than a character that renders
            // correctly up to joint 128 and explodes past it.
            errors.push(EngineError::new(
                engine_core::codes::TOO_MANY_JOINTS,
                format!(
                    "glTF file {asset:?} has a skin with {} joints; the joint palette \
                     holds {}",
                    skin.joints.len(),
                    engine_core::skeleton::MAX_JOINTS
                ),
            ));
        }
        Some(_) => {}
    }

    if rig.clip_named(clip).is_none() {
        errors.push(
            EngineError::new(
                engine_core::codes::UNKNOWN_CLIP,
                format!(
                    "glTF file {asset:?} has no animation named {clip:?} (engine \
                     list-animations {asset} lists them)"
                ),
            )
            .suggest_from(clip, rig.clip_names()),
        );
    }

    errors
}

/// Do a `HudImage`'s nine-slice insets fit the image they cut up (M31)?
///
/// Only called once the texture is known to decode, so the load here is the
/// cached one. Reports both dimensions in the message, because "too large" is
/// unactionable without the size it is too large for.
fn check_slice(
    image: &engine_core::components::HudImage,
    base_dir: &Path,
    entity: &str,
) -> Option<EngineError> {
    let path = engine_core::texture::resolve_texture(&image.texture, base_dir).ok()?;
    let texture =
        crate::texture::load_texture(&path, engine_core::texture::ColorSpace::Srgb).ok()?;
    let [left, top, right, bottom] = image.slice;
    let (horizontal, vertical) = (left + right, top + bottom);
    if horizontal <= texture.width as f32 && vertical <= texture.height as f32 {
        return None;
    }
    Some(
        EngineError::new(
            engine_core::codes::HUD_IMAGE_SLICE_TOO_LARGE,
            format!(
                "the HudImage on {entity:?} slices [{left}, {top}, {right}, {bottom}] out of \
                 {:?}, which is {}×{}; left+right ({horizontal}) must fit the width and \
                 top+bottom ({vertical}) the height",
                image.texture, texture.width, texture.height
            ),
        )
        .entity(entity)
        .component("HudImage")
        .field("slice"),
    )
}

/// The same for a texture: decode it, which is also where `texture_too_large`
/// fires — before any device exists, let alone any allocation.
fn check_texture(
    asset: &str,
    base_dir: &Path,
    space: engine_core::texture::ColorSpace,
) -> Option<EngineError> {
    match engine_core::texture::resolve_texture(asset, base_dir) {
        Ok(path) => crate::texture::load_texture(&path, space).err(),
        // The reference itself is engine-core's to report; this pass would
        // only say it twice.
        Err(_) => None,
    }
}
