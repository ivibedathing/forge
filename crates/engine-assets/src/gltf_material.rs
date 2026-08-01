//! glTF material import (M26).
//!
//! `gltf::import` already returns `(document, buffers, images)` and the mesh
//! loader discards the third with a `_images` binding — so the material data
//! this needs is *already being parsed and thrown away*, which makes import much
//! closer to plumbing than to a feature.
//!
//! Two rules keep the result inside the engine's invariants:
//!
//! - **Embedded images are written out as PNG files.** A GLB's images live in a
//!   binary buffer, and leaving them there would be a binary asset referenced by
//!   index — invariants 1 and 3 both. Writing them out is what makes an import
//!   an ordinary, diffable, hand-editable scene.
//! - **One `materials/*.json` per glTF material, referenced rather than
//!   inlined.** A glTF model routinely has several primitives sharing one
//!   material, which is precisely the case `Material.asset` exists for.

use std::collections::HashMap;
use std::path::Path;

use engine_core::components::Material;
use engine_core::error::{EngineError, Result};
use glam::{Vec2, Vec3};

/// What one import produced.
#[derive(Debug, Default)]
pub struct Imported {
    /// The material files written, in glTF order, as scene-relative paths.
    pub materials: Vec<String>,
    /// The texture files written, as scene-relative paths. Deduped by content,
    /// so two materials sharing a map share the file.
    pub textures: Vec<String>,
    /// Things worth saying but not worth failing over — an occlusion texture
    /// that had to be repacked, most of all.
    pub warnings: Vec<String>,
}

/// Import every material of a glTF file.
///
/// `root` is the directory paths come out relative to — the scene's directory
/// when importing into a scene, the model's own otherwise. `textures_dir` and
/// `materials_dir` are relative to it.
pub fn import_materials(
    path: &Path,
    root: &Path,
    textures_dir: &str,
    materials_dir: &str,
) -> Result<Imported> {
    let display = path.display().to_string();
    let (document, _buffers, images) = gltf::import(path).map_err(|e| {
        EngineError::new(
            engine_core::codes::ASSET_LOAD_FAILED,
            format!("could not load glTF file {display}: {e}"),
        )
        .file(&display)
    })?;

    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "model".to_string());

    let mut out = Imported::default();
    // Content hash → the scene-relative path already written for it. Two
    // materials sharing a map therefore share one file rather than writing the
    // same bytes twice under two names.
    let mut written: HashMap<u64, String> = HashMap::new();

    for (index, source) in document.materials().enumerate() {
        let name = sanitize(
            source
                .name()
                .map(str::to_string)
                .unwrap_or_else(|| format!("material_{index}")),
        );
        let pbr = source.pbr_metallic_roughness();
        let base = pbr.base_color_factor();

        let mut material = Material {
            albedo: Vec3::new(base[0], base[1], base[2]),
            metallic: pbr.metallic_factor(),
            roughness: pbr.roughness_factor(),
            emissive: Vec3::from_array(source.emissive_factor()),
            uv_scale: Vec2::ONE,
            uv_offset: Vec2::ZERO,
            ..Material::default()
        };

        match source.alpha_mode() {
            gltf::material::AlphaMode::Mask => {
                material.alpha_cutoff = source.alpha_cutoff().unwrap_or(0.5);
            }
            gltf::material::AlphaMode::Blend => material.alpha = base[3],
            gltf::material::AlphaMode::Opaque => {}
        }

        // The three volume extensions, which map one for one onto what M26
        // added — this is why the features are enabled in the workspace.
        if let Some(transmission) = source.transmission() {
            material.transmission = transmission.transmission_factor();
        }
        if let Some(ior) = source.ior() {
            material.ior = ior.clamp(1.0, 3.0);
        }
        if let Some(volume) = source.volume() {
            material.thickness = volume.thickness_factor();
            let distance = volume.attenuation_distance();
            let colour = Vec3::from_array(volume.attenuation_color());
            // glTF gives a colour *and* a distance; the engine's `attenuation`
            // is per-metre survival, so a short distance means a strong tint.
            // An infinite distance (the default) absorbs nothing.
            material.attenuation = if distance.is_finite() && distance > 1e-4 {
                (Vec3::ONE - (Vec3::ONE - colour) / distance).clamp(Vec3::ZERO, Vec3::ONE)
            } else {
                Vec3::ONE
            };
        }

        let mut save = |slot: &str, image: Option<Image>| -> Result<Option<String>> {
            let Some(image) = image else { return Ok(None) };
            let hash = hash_bytes(&image.rgba, image.width, image.height);
            if let Some(existing) = written.get(&hash) {
                return Ok(Some(existing.clone()));
            }
            let relative = format!("{textures_dir}/{stem}_{name}_{slot}.png");
            write_png(&root.join(&relative), &image)?;
            written.insert(hash, relative.clone());
            out.textures.push(relative.clone());
            Ok(Some(relative))
        };

        material.albedo_map = save(
            "albedo",
            texture_image(&images, pbr.base_color_texture().map(|t| t.texture())),
        )?;
        material.emissive_map = save(
            "emissive",
            texture_image(&images, source.emissive_texture().map(|t| t.texture())),
        )?;
        material.normal_map = save(
            "normal",
            texture_image(&images, source.normal_texture().map(|t| t.texture())),
        )?;
        if let Some(normal) = source.normal_texture() {
            material.normal_strength = normal.scale();
        }

        // ORM is the one lossy spot: glTF allows the occlusion texture to be a
        // different image from the metallic-roughness one, while `orm_map`
        // packs them. When they differ, R comes from one and GB from the other,
        // and this says so rather than quietly picking a winner.
        let mr = texture_image(
            &images,
            pbr.metallic_roughness_texture().map(|t| t.texture()),
        );
        let occlusion = texture_image(&images, source.occlusion_texture().map(|t| t.texture()));
        let packed = match (mr, occlusion) {
            (None, None) => None,
            (Some(mr), None) => Some(pack_orm(&mr, None)),
            (None, Some(occlusion)) => Some(pack_orm(&occlusion, Some(&occlusion))),
            (Some(mr), Some(occlusion)) => {
                if mr.rgba != occlusion.rgba || mr.width != occlusion.width {
                    out.warnings.push(format!(
                        "material {name:?} has separate occlusion and metallic-roughness \
                         textures; they were repacked into one orm_map (R from occlusion, \
                         GB from metallic-roughness)"
                    ));
                    Some(pack_orm(&mr, Some(&occlusion)))
                } else {
                    Some(pack_orm(&mr, Some(&occlusion)))
                }
            }
        };
        material.orm_map = save("orm", packed)?;

        // §11's wrinkle, and the accepted answer to it: `albedo` defaults to
        // 0.8, so a texture multiplied by the default factor is 20% darker than
        // the artist's file. The importer knows there is a map, so it writes the
        // factor out explicitly and the file then says so.
        if material.albedo_map.is_some() && base[..3] == [1.0, 1.0, 1.0] {
            material.albedo = Vec3::ONE;
        }

        let relative = format!("{materials_dir}/{stem}_{name}.json");
        write_material(
            &root.join(&relative),
            &material,
            textures_dir,
            materials_dir,
        )?;
        out.materials.push(relative);
    }

    Ok(out)
}

/// One decoded image, normalized to RGBA8 like everything else the engine
/// loads.
struct Image {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn texture_image(
    images: &[gltf::image::Data],
    texture: Option<gltf::Texture<'_>>,
) -> Option<Image> {
    let data = images.get(texture?.source().index())?;
    let pixels = data.pixels.as_slice();
    let channels = match data.format {
        gltf::image::Format::R8 => 1,
        gltf::image::Format::R8G8 => 2,
        gltf::image::Format::R8G8B8 => 3,
        gltf::image::Format::R8G8B8A8 => 4,
        // 16-bit and float formats are rare in the wild and would need a
        // conversion table each; refusing to guess is better than importing a
        // texture whose values are silently wrong by 256×.
        _ => return None,
    };
    let count = (data.width * data.height) as usize;
    let mut rgba = Vec::with_capacity(count * 4);
    for texel in 0..count {
        let at = texel * channels;
        let (r, g, b, a) = match channels {
            1 => (pixels[at], pixels[at], pixels[at], 255),
            2 => (pixels[at], pixels[at], pixels[at], pixels[at + 1]),
            3 => (pixels[at], pixels[at + 1], pixels[at + 2], 255),
            _ => (pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]),
        };
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    Some(Image {
        width: data.width,
        height: data.height,
        rgba,
    })
}

/// Occlusion in R, roughness in G, metallic in B — glTF's own packing, which is
/// why `orm_map` adopted it: when the two source textures are the same image
/// this is a copy.
fn pack_orm(metallic_roughness: &Image, occlusion: Option<&Image>) -> Image {
    let mut rgba = metallic_roughness.rgba.clone();
    for texel in 0..(metallic_roughness.width * metallic_roughness.height) as usize {
        let at = texel * 4;
        rgba[at] = match occlusion {
            // Sampled by relative position, so a differently-sized occlusion
            // map still lands in the right place.
            Some(occlusion) => {
                let x = texel as u32 % metallic_roughness.width;
                let y = texel as u32 / metallic_roughness.width;
                let ox = x * occlusion.width / metallic_roughness.width.max(1);
                let oy = y * occlusion.height / metallic_roughness.height.max(1);
                let source = ((oy.min(occlusion.height.saturating_sub(1)) * occlusion.width
                    + ox.min(occlusion.width.saturating_sub(1)))
                    * 4) as usize;
                occlusion.rgba.get(source).copied().unwrap_or(255)
            }
            // Absent occlusion is "nothing is occluded", not "everything is".
            None => 255,
        };
        rgba[at + 3] = 255;
    }
    Image {
        width: metallic_roughness.width,
        height: metallic_roughness.height,
        rgba,
    }
}

fn write_png(path: &Path, image: &Image) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| write_failed(path, e))?;
    }
    let buffer = image::RgbaImage::from_raw(image.width, image.height, image.rgba.clone())
        .ok_or_else(|| {
            EngineError::new(
                engine_core::codes::ASSET_LOAD_FAILED,
                format!(
                    "glTF image for {} has the wrong pixel count",
                    path.display()
                ),
            )
        })?;
    buffer
        .save(path)
        .map_err(|e| write_failed(path, std::io::Error::other(e)))
}

/// Write a material file, with its texture references made relative to **it**
/// rather than to the scene — the rule that makes a material shareable.
fn write_material(
    path: &Path,
    material: &Material,
    textures_dir: &str,
    materials_dir: &str,
) -> Result<()> {
    let mut material = material.clone();
    for reference in [
        &mut material.albedo_map,
        &mut material.orm_map,
        &mut material.normal_map,
        &mut material.emissive_map,
    ]
    .into_iter()
    .flatten()
    {
        *reference = reference.replacen(
            &format!("{textures_dir}/"),
            &format!(
                "{}{textures_dir}/",
                "../".repeat(materials_dir.split('/').count())
            ),
            1,
        );
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| write_failed(path, e))?;
    }
    let json = serde_json::to_string_pretty(&material)
        .map_err(|e| write_failed(path, std::io::Error::other(e)))?;
    std::fs::write(path, format!("{json}\n")).map_err(|e| write_failed(path, e))
}

fn write_failed(path: &Path, e: std::io::Error) -> EngineError {
    EngineError::new(
        engine_core::codes::IMPORT_FAILED,
        format!("could not write {}: {e}", path.display()),
    )
    .file(path.display().to_string())
}

/// A file name that is safe on every platform and stable across imports.
fn sanitize(name: String) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let trimmed = cleaned.trim_matches('_').to_lowercase();
    if trimmed.is_empty() {
        "material".to_string()
    } else {
        trimmed
    }
}

/// FNV-1a over the pixels and dimensions. Written out rather than pulled in:
/// it decides *file names*, so it is a format contract like every other hash in
/// this repo, and a dependency changing its hash would rename every texture an
/// import writes.
fn hash_bytes(bytes: &[u8], width: u32, height: u32) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in width
        .to_le_bytes()
        .iter()
        .chain(height.to_le_bytes().iter())
        .chain(bytes.iter())
    {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
