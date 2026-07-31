//! Texture loading: an image file to RGBA8 pixels and their mip chain.
//!
//! The split is [`engine_core::texture`]'s: that module owns everything about a
//! texture reference and about the pixels once decoded — the colour space, the
//! mip filter, the size limit — and this one owns opening the file. Format
//! detection is delegated to the `image` crate (the workspace builds it with
//! PNG support only) and everything is normalized to RGBA8, so the upload path
//! has exactly one case.

use std::path::Path;

use engine_core::error::{EngineError, Result};
use engine_core::texture::{check_texture_size, ColorSpace, TextureData};

/// Load an image file as RGBA8 with its mip chain, filtered for `space`.
pub fn load_texture(path: &Path, space: ColorSpace) -> Result<TextureData> {
    let display = path.display().to_string();

    let bytes = std::fs::read(path).map_err(|e| {
        let code = if e.kind() == std::io::ErrorKind::NotFound {
            engine_core::codes::ASSET_NOT_FOUND
        } else {
            engine_core::codes::ASSET_LOAD_FAILED
        };
        EngineError::new(code, format!("could not read texture {display}: {e}")).file(&display)
    })?;

    let image = image::load_from_memory(&bytes)
        .map_err(|e| {
            EngineError::new(
                engine_core::codes::ASSET_LOAD_FAILED,
                format!("could not decode texture {display}: {e}"),
            )
            .file(&display)
        })?
        .to_rgba8();

    // Before the chain is built, not after: the point of the limit is to refuse
    // ahead of the allocation, and a 8192² source would spend a second
    // box-filtering eleven levels it can never upload.
    check_texture_size(&display, image.width(), image.height()).map_err(|e| e.file(&display))?;

    Ok(TextureData::new(
        image.width(),
        image.height(),
        image.into_raw(),
        space,
    ))
}
