//! Textures: the reference side of `Material.albedo_map` and friends, the CPU
//! mip chain, and the colour space that is a property of the *slot* (M26).
//!
//! This module is to a texture what [`crate::mesh`] is to a mesh: everything
//! about a reference that can be decided without opening the file, plus the
//! CPU-side data the renderer uploads. Decoding a PNG needs the `image` crate
//! and so lives in `engine-assets`; nothing here opens a file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{EngineError, Result};

/// File extensions the texture loader reads, lowercase.
///
/// One entry, deliberately: the workspace builds `image` with PNG support only,
/// and a scene that references a `.jpg` should be told so at validate time
/// rather than at decode time.
pub const TEXTURE_EXTENSIONS: &[&str] = &["png"];

/// The largest texture the engine will load, on a side.
///
/// This is `wgpu::Limits::downlevel_defaults().max_texture_dimension_2d`, read
/// from the registry rather than remembered. It is not a number the engine
/// chose: `Limits::default()` is the same 2048, and so is WebGPU's own floor.
/// Refusing at validate time rather than at upload time follows
/// `tree_too_complex`'s precedent — a device-limit panic mid-render is the
/// worst thing an agent loop can be handed.
pub const MAX_TEXTURE_SIZE: u32 = 2048;

/// Which space a texture's bytes are in, decided by the **slot** that reads it
/// and never by the file or by a field.
///
/// A PNG does not record whether its bytes are an sRGB-encoded colour or linear
/// data, so something has to say. Making it the slot is what makes the single
/// most common texture bug in any engine unrepresentable: `albedo_map` and
/// `emissive_map` are colours and decode; `orm_map` and `normal_map` are data
/// and do not. There is nothing to configure.
///
/// It also decides how the mip chain is filtered — averaging sRGB-encoded bytes
/// darkens every level below 0 — which is why the chain is generated per space
/// rather than once per file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorSpace {
    /// An sRGB-encoded colour: uploaded `Rgba8UnormSrgb` so the sampler
    /// decodes, and mip-filtered in linear space.
    Srgb,
    /// Data: uploaded `Rgba8Unorm` and mip-filtered on the raw bytes.
    Linear,
}

/// Decoded pixels with their mip chain, tightly packed RGBA8, row-major from
/// the top-left.
///
/// Level 0 is the source image; each level after it is the previous one box-
/// filtered 2×2, with an odd dimension rounding *up* and its last column or row
/// reading the single texel that remains. The chain always ends at 1×1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureData {
    pub width: u32,
    pub height: u32,
    pub space: ColorSpace,
    /// Level 0 first. Never empty.
    pub mips: Vec<Vec<u8>>,
}

impl TextureData {
    /// Build the mip chain for a decoded image.
    ///
    /// Mips are not optional and they are generated here rather than on the
    /// GPU. Not optional, because this engine has already learned twice — in
    /// water's detail normals and terrain's per-pixel bump — that sub-pixel
    /// detail without a fade aliases into sparkle that reads as *broken*
    /// rather than as low quality, and a minified texture with no mip chain is
    /// that bug in its original form. On the CPU, because a render sits under a
    /// committed baseline, which makes the filter a format contract: "the
    /// `image` crate changed its resampler" must not be able to surface as a
    /// renderer regression. It also keeps the whole path GPU-free and testable
    /// on CI, which proves only the GPU-free half.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>, space: ColorSpace) -> Self {
        let mut mips = vec![rgba];
        let (mut w, mut h) = (width.max(1), height.max(1));
        while w > 1 || h > 1 {
            let (nw, nh) = (w.div_ceil(2).max(1), h.div_ceil(2).max(1));
            let previous = mips.last().expect("never empty");
            mips.push(downsample(previous, w, h, nw, nh, space));
            w = nw;
            h = nh;
        }
        Self {
            width: width.max(1),
            height: height.max(1),
            space,
            mips,
        }
    }

    /// The full-resolution pixels.
    pub fn rgba(&self) -> &[u8] {
        self.mips.first().expect("never empty")
    }

    /// The dimensions of mip `level`, halving and rounding up like the chain.
    pub fn level_size(&self, level: usize) -> (u32, u32) {
        let mut size = (self.width, self.height);
        for _ in 0..level {
            size = (size.0.div_ceil(2).max(1), size.1.div_ceil(2).max(1));
        }
        size
    }
}

/// One 2×2 box-filter step, in the space the slot reads the texture in.
///
/// Written out here rather than delegated for the reason every generator in
/// this repo is: it is a format contract. The odd-dimension case takes the one
/// texel that remains rather than wrapping or clamping to a neighbour's column,
/// which is what every box-filter chain does and what keeps a 3-wide image from
/// smearing its right edge across the level below it.
fn downsample(
    source: &[u8],
    width: u32,
    height: u32,
    new_width: u32,
    new_height: u32,
    space: ColorSpace,
) -> Vec<u8> {
    let mut out = vec![0u8; (new_width * new_height * 4) as usize];
    for y in 0..new_height {
        for x in 0..new_width {
            // The 2×2 block, with the far side clamped back onto the near one
            // when the source dimension is odd — so the block degenerates to
            // one texel rather than reading past the row.
            let x0 = (x * 2).min(width - 1);
            let x1 = (x * 2 + 1).min(width - 1);
            let y0 = (y * 2).min(height - 1);
            let y1 = (y * 2 + 1).min(height - 1);
            let taps = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)];

            for channel in 0..4 {
                // Alpha is linear data in both spaces — it is a coverage
                // fraction, never a colour — so only rgb decodes.
                let decode = space == ColorSpace::Srgb && channel < 3;
                let mut sum = 0.0f32;
                for (tx, ty) in taps {
                    let byte = source[((ty * width + tx) * 4 + channel) as usize];
                    sum += if decode {
                        srgb_to_linear(byte)
                    } else {
                        byte as f32
                    };
                }
                let mean = sum / 4.0;
                out[((y * new_width + x) * 4 + channel) as usize] = if decode {
                    linear_to_srgb(mean)
                } else {
                    // Round rather than truncate: truncation biases every mip
                    // level down by half a step, which over eleven levels of a
                    // 2048² texture is a visible darkening.
                    (mean + 0.5) as u8
                };
            }
        }
    }
    out
}

/// The sRGB electro-optical transfer function, on a byte.
///
/// Spelled out in-repo like every other numeric contract here: the mip chain it
/// filters sits under a committed baseline.
fn srgb_to_linear(byte: u8) -> f32 {
    let v = byte as f32 / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Its inverse, back to a byte.
fn linear_to_srgb(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let encoded = if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0 + 0.5) as u8
}

/// A texture reference, resolved as far as it can be without opening the file.
///
/// The mesh seam transposed: `engine validate` calls this to reject a bad
/// reference before render time, and `engine-assets` calls it to decide what to
/// open. Unlike a mesh reference there is no `builtin:` form — a texture is
/// always a file.
pub fn resolve_texture(asset: &str, base_dir: &Path) -> Result<PathBuf> {
    if Path::new(asset).is_absolute() {
        return Err(EngineError::new(
            crate::codes::ASSET_PATH_NOT_RELATIVE,
            format!(
                "texture {asset:?} is an absolute path; assets are referenced \
                 by path relative to the scene file, so scenes stay portable"
            ),
        ));
    }

    let resolved = base_dir.join(asset);

    match resolved.extension().and_then(|e| e.to_str()) {
        Some(ext) if TEXTURE_EXTENSIONS.contains(&ext.to_lowercase().as_str()) => {}
        _ => {
            return Err(EngineError::new(
                crate::codes::ASSET_UNSUPPORTED,
                format!(
                    "texture {asset:?} is not a format the engine reads; use a {} file",
                    TEXTURE_EXTENSIONS.join(" or .")
                ),
            ));
        }
    }

    if !resolved.is_file() {
        return Err(EngineError::new(
            crate::codes::ASSET_NOT_FOUND,
            format!(
                "no texture file at {} (asset paths resolve relative to the scene file)",
                resolved.display()
            ),
        )
        .suggest_from(asset, sibling_textures(asset, &resolved).iter().map(String::as_str)));
    }

    Ok(resolved)
}

/// Texture files actually present in the directory the reference points into,
/// spelled the way the scene would spell them.
fn sibling_textures(asset: &str, resolved: &Path) -> Vec<String> {
    let mut candidates = Vec::new();
    let Some(dir) = resolved.parent() else {
        return candidates;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return candidates;
    };

    let prefix = Path::new(asset).parent().filter(|p| !p.as_os_str().is_empty());
    for entry in entries.flatten() {
        if !entry.path().is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_texture = Path::new(name.as_ref())
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| TEXTURE_EXTENSIONS.contains(&ext.to_lowercase().as_str()));
        if !is_texture {
            continue;
        }
        candidates.push(match prefix {
            Some(dir) => dir.join(name.as_ref()).to_string_lossy().into_owned(),
            None => name.into_owned(),
        });
    }
    candidates.sort();
    candidates
}

/// The size check, separated from the load so validation can make it without a
/// device and the loader can make it without duplicating the message.
pub fn check_texture_size(asset: &str, width: u32, height: u32) -> Result<()> {
    if width.max(height) > MAX_TEXTURE_SIZE {
        return Err(EngineError::new(
            crate::codes::TEXTURE_TOO_LARGE,
            format!(
                "texture {asset:?} is {width}×{height}; the engine's device limit is \
                 {MAX_TEXTURE_SIZE} on a side (downlevel defaults). Downscale it."
            ),
        ));
    }
    Ok(())
}

/// Anything that can turn a texture reference into pixels.
///
/// [`crate::mesh::MeshSource`]'s counterpart, and under the same rule, for the
/// same reason: implementations must hand out the **same `Arc`** for repeated
/// loads of one asset in one colour space, because the renderer keys its
/// uploaded GPU textures on that identity and a fresh `Arc` per call re-uploads
/// every texture every frame.
///
/// The colour space is part of the key, not just of the upload: it decides how
/// the mip chain was filtered (see [`ColorSpace`]).
pub trait TextureSource {
    fn load_texture(&self, asset: &str, space: ColorSpace) -> Result<Arc<TextureData>>;
}

/// Everything the draw list needs to resolve: geometry and textures.
///
/// One parameter rather than two, with a blanket impl, so every existing caller
/// of `Scene::render_items` keeps passing exactly what it passed before.
pub trait AssetSource: crate::mesh::MeshSource + TextureSource {}
impl<T: crate::mesh::MeshSource + TextureSource + ?Sized> AssetSource for T {}

/// The four maps a material can carry, resolved to shared pixels (M26).
///
/// `Arc`, and the *same* `Arc` for one asset, for M15's reason: the renderer
/// keys its uploaded GPU textures on that identity.
#[derive(Debug, Clone, Default)]
pub struct MaterialTextures {
    pub albedo: Option<Arc<TextureData>>,
    pub orm: Option<Arc<TextureData>>,
    pub normal: Option<Arc<TextureData>>,
    pub emissive: Option<Arc<TextureData>>,
}

impl MaterialTextures {
    /// Whether anything is bound — the test that routes a draw to the textured
    /// pipeline variant.
    pub fn any(&self) -> bool {
        self.albedo.is_some()
            || self.orm.is_some()
            || self.normal.is_some()
            || self.emissive.is_some()
    }

    /// Resolve a material's maps through `textures`.
    pub fn resolve(
        material: &crate::components::Material,
        textures: &dyn TextureSource,
    ) -> Result<Self> {
        let mut out = Self::default();
        for (field, asset, space) in material.maps() {
            let loaded = textures
                .load_texture(asset, space)
                .map_err(|e| e.component("Material").field(field))?;
            match field {
                "albedo_map" => out.albedo = Some(loaded),
                "orm_map" => out.orm = Some(loaded),
                "normal_map" => out.normal = Some(loaded),
                _ => out.emissive = Some(loaded),
            }
        }
        Ok(out)
    }
}

/// A [`TextureSource`] with nothing behind it — for GPU-less contexts that have
/// no asset directory. Every reference is an error, which is what a scene with
/// texture maps rendered through a mesh-only context should get.
pub struct NoTextures;

impl TextureSource for NoTextures {
    fn load_texture(&self, asset: &str, _space: ColorSpace) -> Result<Arc<TextureData>> {
        Err(EngineError::new(
            crate::codes::ASSET_NOT_FOUND,
            format!("cannot load texture {asset:?}: this context has no asset directory"),
        ))
    }
}

impl TextureSource for crate::mesh::BuiltinAssets {
    fn load_texture(&self, asset: &str, space: ColorSpace) -> Result<Arc<TextureData>> {
        NoTextures.load_texture(asset, space)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The filter is a format contract, so it is pinned by hand-computed
    /// values rather than by "it looks smaller".
    #[test]
    fn a_box_filter_averages_its_block() {
        // 2×2 linear data: 0, 100, 200, 255 in every channel.
        let mut rgba = Vec::new();
        for v in [0u8, 100, 200, 255] {
            rgba.extend_from_slice(&[v, v, v, v]);
        }
        let texture = TextureData::new(2, 2, rgba, ColorSpace::Linear);
        assert_eq!(texture.mips.len(), 2, "2×2 chains down to 1×1");
        // (0 + 100 + 200 + 255) / 4 = 138.75, rounded to 139.
        assert_eq!(texture.mips[1], vec![139, 139, 139, 139]);
    }

    #[test]
    fn an_odd_dimension_rounds_up_and_keeps_its_last_texel() {
        // 3×1: red, green, blue.
        let rgba = vec![
            255, 0, 0, 255, //
            0, 255, 0, 255, //
            0, 0, 255, 255,
        ];
        let texture = TextureData::new(3, 1, rgba, ColorSpace::Linear);
        assert_eq!(texture.level_size(1), (2, 1), "3 halves to 2, not 1");
        // The second output texel's block degenerates onto the blue texel
        // alone, so it stays pure blue rather than borrowing from green.
        assert_eq!(&texture.mips[1][4..8], &[0, 0, 255, 255]);
        assert_eq!(texture.mips.len(), 3, "3×1 → 2×1 → 1×1");
        assert_eq!(texture.level_size(2), (1, 1));
    }

    /// The gamma-correct half of §4.2: averaging encoded bytes darkens every
    /// level, which is the classic "my mips are too dark" bug.
    #[test]
    fn an_srgb_chain_filters_in_linear_space() {
        // 2×2, half black and half white.
        let rgba = vec![
            0, 0, 0, 255, //
            255, 255, 255, 255, //
            0, 0, 0, 255, //
            255, 255, 255, 255,
        ];
        let srgb = TextureData::new(2, 2, rgba.clone(), ColorSpace::Srgb);
        let linear = TextureData::new(2, 2, rgba, ColorSpace::Linear);
        // Half the light is 0.5 linear, which encodes to 188 — not 128, which
        // is what averaging the bytes gives.
        assert_eq!(srgb.mips[1][0], 188);
        assert_eq!(linear.mips[1][0], 128);
        // Alpha is coverage in both spaces and must not be decoded.
        assert_eq!(srgb.mips[1][3], 255);
    }

    #[test]
    fn a_chain_ends_at_one_by_one() {
        let texture = TextureData::new(8, 4, vec![7; 8 * 4 * 4], ColorSpace::Linear);
        assert_eq!(texture.mips.len(), 4, "8×4 → 4×2 → 2×1 → 1×1");
        assert_eq!(texture.level_size(3), (1, 1));
        assert_eq!(texture.mips[3].len(), 4);
        // A constant image stays constant at every level.
        assert!(texture.mips.iter().all(|level| level.iter().all(|&b| b == 7)));
    }

    #[test]
    fn a_texture_over_the_device_limit_is_refused_before_anything_allocates() {
        let error = check_texture_size("big.png", 4096, 4096).unwrap_err();
        assert_eq!(error.error, "texture_too_large");
        assert!(error.message.contains("4096"), "{}", error.message);
        assert!(error.message.contains("2048"), "{}", error.message);
        check_texture_size("fine.png", 2048, 1).expect("at the limit is allowed");
    }

    #[test]
    fn a_texture_reference_is_relative_and_a_png() {
        let dir = std::path::Path::new("examples");
        assert_eq!(
            resolve_texture("/etc/passwd.png", dir).unwrap_err().error,
            "asset_path_not_relative"
        );
        assert_eq!(
            resolve_texture("bark.jpg", dir).unwrap_err().error,
            "asset_unsupported"
        );
        assert_eq!(
            resolve_texture("nothing-here.png", dir).unwrap_err().error,
            "asset_not_found"
        );
    }
}
