//! The asset source that actually reads files.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine_core::error::Result;
use engine_core::mesh::{MeshAsset, MeshData, MeshSource};
use engine_core::skeleton::{Rig, RigSource};
use engine_core::texture::{resolve_texture, ColorSpace, TextureData, TextureSource};

/// Resolves and loads mesh assets relative to a root directory — in practice,
/// the directory of the scene file being rendered.
///
/// Caches by asset string, so a scene with forty entities sharing one `.glb`
/// parses it once. Hits hand back the *same* `Arc`, per the [`MeshSource`]
/// contract: a viewer that rebuilds its draw list every frame then copies no
/// geometry, and the renderer can key its uploaded GPU buffers on that shared
/// identity. The cache is per-server and a server is per-scene-load; nothing
/// here outlives the command that created it (invariant 2: no hidden state).
pub struct AssetServer {
    root: PathBuf,
    cache: RefCell<HashMap<String, Arc<MeshData>>>,
    /// The same arrangement for textures, keyed on the reference **and** the
    /// colour space it was decoded for — the space decides how the mip chain
    /// was filtered, so one file read as albedo and as ORM really is two
    /// different sets of pixels.
    textures: RefCell<HashMap<(String, ColorSpace), Arc<TextureData>>>,
    /// Rigs (M27), cached for the same reason as geometry: a palette is
    /// recomputed every frame and must not re-parse the `.glb` behind it.
    rigs: RefCell<HashMap<String, Arc<Rig>>>,
}

impl AssetServer {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cache: RefCell::new(HashMap::new()),
            textures: RefCell::new(HashMap::new()),
            rigs: RefCell::new(HashMap::new()),
        }
    }

    /// A server rooted at the scene file's directory — the base against which
    /// the scene's relative asset paths are defined.
    pub fn for_scene(scene_path: &Path) -> Self {
        Self::new(scene_path.parent().unwrap_or(Path::new("")))
    }
}

impl MeshSource for AssetServer {
    fn load_mesh(&self, asset: &str) -> Result<Arc<MeshData>> {
        if let Some(hit) = self.cache.borrow().get(asset) {
            return Ok(Arc::clone(hit));
        }

        let data = Arc::new(match MeshAsset::resolve(asset, &self.root)? {
            MeshAsset::Builtin(builtin) => builtin.data(),
            MeshAsset::File(path) => crate::gltf_mesh::load_gltf(&path)?,
        });

        self.cache
            .borrow_mut()
            .insert(asset.to_string(), Arc::clone(&data));
        Ok(data)
    }
}

impl RigSource for AssetServer {
    fn load_rig(&self, asset: &str) -> Result<Arc<Rig>> {
        if let Some(hit) = self.rigs.borrow().get(asset) {
            return Ok(Arc::clone(hit));
        }

        let rig = Arc::new(match MeshAsset::resolve(asset, &self.root)? {
            // A `builtin:` primitive is generated geometry with no file behind
            // it, so it has no rig — an empty one, not an error.
            MeshAsset::Builtin(_) => Rig::default(),
            MeshAsset::File(path) => crate::gltf_skin::load_rig(&path)?,
        });

        self.rigs
            .borrow_mut()
            .insert(asset.to_string(), Arc::clone(&rig));
        Ok(rig)
    }
}

impl TextureSource for AssetServer {
    fn load_texture(&self, asset: &str, space: ColorSpace) -> Result<Arc<TextureData>> {
        let key = (asset.to_string(), space);
        if let Some(hit) = self.textures.borrow().get(&key) {
            return Ok(Arc::clone(hit));
        }

        let path = resolve_texture(asset, &self.root)?;
        let data = Arc::new(crate::texture::load_texture(&path, space)?);

        self.textures.borrow_mut().insert(key, Arc::clone(&data));
        Ok(data)
    }
}
