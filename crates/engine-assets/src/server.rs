//! The mesh source that actually reads files.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use engine_core::error::Result;
use engine_core::mesh::{MeshAsset, MeshData, MeshSource};

/// Resolves and loads mesh assets relative to a root directory — in practice,
/// the directory of the scene file being rendered.
///
/// Caches by asset string, so a scene with forty entities sharing one `.glb`
/// parses it once. The cache is per-server and a server is per-scene-load;
/// nothing here outlives the command that created it (invariant 2: no hidden
/// state).
pub struct AssetServer {
    root: PathBuf,
    cache: RefCell<HashMap<String, MeshData>>,
}

impl AssetServer {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// A server rooted at the scene file's directory — the base against which
    /// the scene's relative asset paths are defined.
    pub fn for_scene(scene_path: &Path) -> Self {
        Self::new(scene_path.parent().unwrap_or(Path::new("")))
    }
}

impl MeshSource for AssetServer {
    fn load_mesh(&self, asset: &str) -> Result<MeshData> {
        if let Some(hit) = self.cache.borrow().get(asset) {
            return Ok(hit.clone());
        }

        let data = match MeshAsset::resolve(asset, &self.root)? {
            MeshAsset::Builtin(builtin) => builtin.data(),
            MeshAsset::File(path) => crate::gltf_mesh::load_gltf(&path)?,
        };

        self.cache
            .borrow_mut()
            .insert(asset.to_string(), data.clone());
        Ok(data)
    }
}
