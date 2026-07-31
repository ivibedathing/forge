//! Drag-and-drop import: a mesh file dropped on the editor window becomes an
//! asset next to the scene and a new entity referencing it by relative path
//! (invariant #3).
//!
//! `.glb`/`.gltf` files already inside the scene's directory tree are
//! referenced in place; ones outside it are copied into `meshes/` beside the
//! scene. `.blend` files are converted to `.glb` in `meshes/` by running
//! Blender headlessly (its bundled glTF exporter) — Blender is found via
//! `$BLENDER`, `PATH`, or the macOS app bundle, and its absence is a
//! structured `blender_not_found`. Conversion runs on a worker thread; the
//! entity splice happens back on the UI thread through the formatter.

use std::path::{Path, PathBuf};
use std::process::Command;

use engine_core::{codes, EngineError};

type Result<T> = std::result::Result<T, EngineError>;

/// A successful import: the asset reference to write into the scene
/// (relative, forward slashes) and the entity base name (the file stem —
/// deduplicated against the live file at commit time, not here).
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedAsset {
    pub asset: String,
    pub base_name: String,
    /// The first `materials/*.json` the model's own materials were written to
    /// (M26), scene-relative. `None` for a model that carries none.
    pub material: Option<String>,
}

/// Whether a dropped path is something [`import`] can handle.
pub fn supported(path: &Path) -> bool {
    matches!(extension(path).as_deref(), Some("blend" | "gltf" | "glb"))
}

fn extension(path: &Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().to_lowercase())
}

/// Import `dropped` for the scene at `scene_path`. Blocking (file copies,
/// possibly a Blender run) — call from a worker thread.
pub fn import(scene_path: &Path, dropped: &Path) -> Result<ImportedAsset> {
    let scene_dir = scene_path.parent().unwrap_or(Path::new("."));
    let stem = dropped
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            EngineError::new(
                codes::IMPORT_FAILED,
                format!("cannot derive a name from {}", dropped.display()),
            )
        })?;

    // The materials are imported through the *same* code path `engine import`
    // uses, deliberately: an import reachable only by dropping a file on a GUI
    // is exactly the bespoke integration layer this project exists to avoid.
    // A model with no materials, or one whose materials fail to parse, still
    // imports as geometry — the mesh is what was dropped.
    let material = |source: &Path| {
        engine_assets::import_materials(source, scene_dir, "textures", "materials")
            .ok()
            .and_then(|imported| imported.materials.into_iter().next())
    };

    match extension(dropped).as_deref() {
        Some("blend") => {
            let blender = find_blender().ok_or_else(|| {
                EngineError::new(
                    codes::BLENDER_NOT_FOUND,
                    "converting .blend needs Blender; install it or point $BLENDER at the executable",
                )
            })?;
            let out = meshes_dir(scene_dir)?.join(format!("{stem}.glb"));
            convert_blend(&blender, dropped, &out)?;
            Ok(ImportedAsset {
                asset: format!("meshes/{stem}.glb"),
                base_name: stem,
                material: material(&out),
            })
        }
        Some(ext @ ("gltf" | "glb")) => {
            // Already under the scene's tree: reference it where it lies.
            if let Some(relative) = relative_within(scene_dir, dropped) {
                return Ok(ImportedAsset {
                    asset: relative,
                    base_name: stem,
                    material: material(dropped),
                });
            }
            // Outside: copy beside the scene. (A .gltf with external buffer
            // files copies alone; a dangling buffer URI then shows up in the
            // validation panel rather than silently — prefer .glb for drops.)
            let out = meshes_dir(scene_dir)?.join(format!("{stem}.{ext}"));
            std::fs::copy(dropped, &out).map_err(|e| {
                EngineError::new(
                    codes::IMPORT_FAILED,
                    format!("could not copy into {}: {e}", out.display()),
                )
                .file(dropped.display().to_string())
            })?;
            Ok(ImportedAsset {
                asset: format!("meshes/{stem}.{ext}"),
                base_name: stem,
                material: material(&out),
            })
        }
        _ => Err(EngineError::new(
            codes::IMPORT_FAILED,
            format!(
                "{} is not an importable mesh file (.blend, .gltf, .glb)",
                dropped.display()
            ),
        )),
    }
}

fn meshes_dir(scene_dir: &Path) -> Result<PathBuf> {
    let dir = scene_dir.join("meshes");
    std::fs::create_dir_all(&dir).map_err(|e| {
        EngineError::new(
            codes::IMPORT_FAILED,
            format!("could not create {}: {e}", dir.display()),
        )
    })?;
    Ok(dir)
}

/// `file` relative to `base` with forward slashes, if `file` is inside
/// `base`'s tree. Symlink-proof via canonicalization (both must exist).
fn relative_within(base: &Path, file: &Path) -> Option<String> {
    let base = base.canonicalize().ok()?;
    let file = file.canonicalize().ok()?;
    let relative = file.strip_prefix(&base).ok()?;
    let parts: Vec<String> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    Some(parts.join("/"))
}

/// Find a Blender executable: `$BLENDER` beats `PATH` beats the macOS app
/// bundle.
pub fn find_blender() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("BLENDER") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("blender");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let bundle = PathBuf::from("/Applications/Blender.app/Contents/MacOS/Blender");
    bundle.is_file().then_some(bundle)
}

/// The Python expression handed to `blender --python-expr`. The output path
/// rides as a JSON string literal — valid Python, quoting handled.
fn export_expr(out: &Path) -> String {
    let filepath = serde_json::to_string(&out.to_string_lossy())
        .unwrap_or_else(|_| "\"export.glb\"".into());
    format!(
        "import bpy; bpy.ops.export_scene.gltf(filepath={filepath}, export_format='GLB')"
    )
}

fn convert_blend(blender: &Path, blend: &Path, out: &Path) -> Result<()> {
    let output = Command::new(blender)
        .arg("--background")
        .arg("--factory-startup")
        .arg(blend)
        .arg("--python-exit-code")
        .arg("2")
        .arg("--python-expr")
        .arg(export_expr(out))
        .output()
        .map_err(|e| {
            EngineError::new(
                codes::BLENDER_NOT_FOUND,
                format!("could not run {}: {e}", blender.display()),
            )
        })?;

    if !output.status.success() || !out.is_file() {
        // Blender is chatty on stdout; the exporter's own failure usually
        // sits in the last lines of either stream.
        let mut noise = String::from_utf8_lossy(&output.stdout).to_string();
        noise.push_str(&String::from_utf8_lossy(&output.stderr));
        let tail: Vec<&str> = noise
            .lines()
            .filter(|l| !l.trim().is_empty())
            .rev()
            .take(3)
            .collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        return Err(EngineError::new(
            codes::IMPORT_FAILED,
            format!("Blender export failed: {}", tail.join(" | ")),
        )
        .file(blend.display().to_string()));
    }
    Ok(())
}

/// `base` if free, else `base-2`, `base-3`, … — how a second drop of the
/// same file gets its own entity.
pub fn unique_name(base: &str, taken: &[String]) -> String {
    if !taken.iter().any(|t| t == base) {
        return base.to_string();
    }
    (2..)
        .map(|i| format!("{base}-{i}"))
        .find(|candidate| !taken.iter().any(|t| t == candidate))
        .expect("the integers do not run out")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("engine-import-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn unique_name_dedupes() {
        let taken = vec!["pyramid".to_string(), "pyramid-2".to_string()];
        assert_eq!(unique_name("cube", &taken), "cube");
        assert_eq!(unique_name("pyramid", &taken), "pyramid-3");
    }

    #[test]
    fn supported_extensions_are_case_insensitive() {
        assert!(supported(Path::new("a.blend")));
        assert!(supported(Path::new("a.GLB")));
        assert!(supported(Path::new("a.gltf")));
        assert!(!supported(Path::new("a.fbx")));
        assert!(!supported(Path::new("blend")));
    }

    #[test]
    fn export_expr_quotes_the_path_as_a_python_literal() {
        let expr = export_expr(Path::new("/tmp/o'brien scene.glb"));
        assert!(expr.contains(r#"filepath="/tmp/o'brien scene.glb""#), "{expr}");
        assert!(expr.contains("export_format='GLB'"));
    }

    #[test]
    fn gltf_inside_the_scene_tree_is_referenced_in_place() {
        let dir = temp_dir("inplace");
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        let scene = dir.join("scene.json");
        std::fs::write(&scene, "{}").unwrap();
        let mesh = dir.join("assets/rock.glb");
        std::fs::write(&mesh, "not really glb").unwrap();

        let imported = import(&scene, &mesh).unwrap();
        assert_eq!(imported.asset, "assets/rock.glb");
        assert_eq!(imported.base_name, "rock");
        // Nothing was copied.
        assert!(!dir.join("meshes").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn glb_outside_the_scene_tree_is_copied_into_meshes() {
        let scene_dir = temp_dir("copy-scene");
        let elsewhere = temp_dir("copy-src");
        let scene = scene_dir.join("scene.json");
        std::fs::write(&scene, "{}").unwrap();
        let mesh = elsewhere.join("rock.glb");
        std::fs::write(&mesh, "bytes").unwrap();

        let imported = import(&scene, &mesh).unwrap();
        assert_eq!(imported.asset, "meshes/rock.glb");
        assert_eq!(
            std::fs::read_to_string(scene_dir.join("meshes/rock.glb")).unwrap(),
            "bytes"
        );
        std::fs::remove_dir_all(&scene_dir).unwrap();
        std::fs::remove_dir_all(&elsewhere).unwrap();
    }

    #[test]
    fn unsupported_drop_is_import_failed() {
        let err = import(Path::new("scene.json"), Path::new("model.fbx")).unwrap_err();
        assert_eq!(err.error, "import_failed");
    }

    /// The whole chain — .blend → Blender → .glb → entity splice → asset
    /// server — against a real Blender. Skips cleanly (like the GPU tests)
    /// when none is installed.
    #[test]
    fn blend_drop_end_to_end_when_blender_is_installed() {
        let Some(blender) = find_blender() else {
            eprintln!("skipping blend_drop_end_to_end: no Blender installed");
            return;
        };

        let dir = temp_dir("blend-e2e");
        let blend = dir.join("thing.blend");
        // Author the fixture with Blender itself: factory startup is the
        // default cube scene.
        let filepath = serde_json::to_string(&blend.to_string_lossy()).unwrap();
        let status = Command::new(&blender)
            .args(["--background", "--factory-startup", "--python-exit-code", "2"])
            .arg("--python-expr")
            .arg(format!(
                "import bpy; bpy.ops.wm.save_as_mainfile(filepath={filepath})"
            ))
            .status()
            .expect("blender runs");
        assert!(status.success(), "authoring the .blend fixture failed");

        let scene = dir.join("scene.json");
        std::fs::write(&scene, "{\n  \"name\": \"t\",\n  \"entities\": []\n}").unwrap();

        let imported = import(&scene, &blend).unwrap();
        assert_eq!(imported.asset, "meshes/thing.glb");
        let glb = std::fs::read(dir.join("meshes/thing.glb")).unwrap();
        assert_eq!(&glb[..4], b"glTF", "output is a binary glTF");

        // The spliced entity renders through the real asset server.
        let edit = engine_core::formatter::AddEntity {
            name: imported.base_name.clone(),
            components: vec![
                ("Transform".into(), vec![]),
                (
                    "Mesh".into(),
                    vec![(
                        "asset".into(),
                        serde_json::Value::String(imported.asset.clone()),
                    )],
                ),
            ],
        };
        let source = engine_core::formatter::apply_add_entity(
            &std::fs::read_to_string(&scene).unwrap(),
            &edit,
        )
        .unwrap();
        std::fs::write(&scene, &source).unwrap();

        let parsed =
            engine_core::Scene::from_source(&source, &scene.display().to_string()).unwrap();
        let assets = engine_assets::AssetServer::for_scene(&scene);
        let items = parsed.render_items(&assets).unwrap();
        assert!(!items.is_empty(), "the imported mesh produces draw items");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
