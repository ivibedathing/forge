//! Basic texture loading: an image file to RGBA8 pixels.
//!
//! M3 provides the loading; nothing samples these until M4 wires textures
//! into materials. Format detection is delegated to the `image` crate — the
//! workspace currently builds it with PNG support only — and everything is
//! normalized to RGBA8 so the GPU upload path has exactly one case.

use std::path::Path;

use engine_core::error::{EngineError, Result};

/// Decoded pixels, tightly packed RGBA8, row-major from the top-left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Load an image file as RGBA8.
pub fn load_texture(path: &Path) -> Result<TextureData> {
    let display = path.display().to_string();

    let bytes = std::fs::read(path).map_err(|e| {
        let code = if e.kind() == std::io::ErrorKind::NotFound {
            "asset_not_found"
        } else {
            "asset_load_failed"
        };
        EngineError::new(code, format!("could not read texture {display}: {e}")).file(&display)
    })?;

    let image = image::load_from_memory(&bytes)
        .map_err(|e| {
            EngineError::new(
                "asset_load_failed",
                format!("could not decode texture {display}: {e}"),
            )
            .file(&display)
        })?
        .to_rgba8();

    Ok(TextureData {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}
