//! The open scene document: the file, live on disk, and the derived state
//! the panels render from.
//!
//! Principle #1: the file is the document. Everything here is reconstructed
//! from disk on every reload; the only editor-held state is which entity is
//! selected and the in-flight gesture. External edits win instantly
//! (principle #4): a poll cheap enough to run continuously re-reads the file
//! and rebuilds when the bytes changed, whoever changed them.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine_core::formatter::{self, AddComponent, AddEntity, RemoveComponent, SetComponentField};
use engine_core::scene::{RenderItem, ResolvedLights};
use engine_core::{EngineError, Scene, SceneFile};

/// How often the file is re-read. Well under the "reflects within a second"
/// success criterion, and reading a few-KB JSON file at 4 Hz is nothing.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

pub struct SceneDoc {
    pub path: PathBuf,
    pub display: String,
    /// The exact bytes on disk at last reload.
    pub source: String,
    /// Parsed file — present even when semantic validation failed, as long
    /// as the JSON parses, so the hierarchy stays useful mid-edit.
    pub file: Option<SceneFile>,
    /// The same parse as raw JSON — what the inspector and gizmo read, so
    /// they see exactly the file's fields (absent keys and all).
    pub raw: Option<serde_json::Value>,
    /// Draw list + lights, rebuilt per reload; empty when invalid.
    pub items: Vec<RenderItem>,
    pub lights: ResolvedLights,
    /// The scene's own sky, fog and shadow settings, so the viewport shows
    /// what the file says rather than a house style — the editor is a view
    /// onto the text (invariant 8), and lighting is most of what there is to
    /// look at.
    pub environment: engine_core::scene::EnvironmentSettings,
    /// Full `engine validate` output — same path, same codes (principle #6).
    pub diagnostics: Vec<EngineError>,
    pub last_reload: Option<Instant>,
    /// Status-bar notice (conflict drops, write errors). Sticky until the
    /// next notice replaces it.
    pub notice: Option<String>,
    last_poll: Instant,
}

impl SceneDoc {
    pub fn open(path: PathBuf) -> Self {
        let display = path.display().to_string();
        let mut doc = Self {
            path,
            display,
            source: String::new(),
            file: None,
            raw: None,
            items: Vec::new(),
            lights: engine_core::scene::LightRig {
                sun: None,
                ambient: None,
            }
            .resolved(),
            environment: Default::default(),
            diagnostics: Vec::new(),
            last_reload: None,
            notice: None,
            last_poll: Instant::now() - POLL_INTERVAL,
        };
        doc.reload();
        doc
    }

    /// True when the diagnostics contain no errors (warnings are fine).
    pub fn is_valid(&self) -> bool {
        !self.diagnostics.iter().any(|d| !d.is_warning())
    }

    /// Re-read the file if the poll interval elapsed; rebuild on change.
    /// Returns true when a reload happened.
    pub fn poll(&mut self) -> bool {
        if self.last_poll.elapsed() < POLL_INTERVAL {
            return false;
        }
        self.last_poll = Instant::now();

        match std::fs::read_to_string(&self.path) {
            Ok(fresh) if fresh != self.source => {
                self.reload_from(fresh);
                true
            }
            Ok(_) => false,
            Err(e) => {
                // A vanished file is a fact about the document, not a crash.
                self.notice = Some(format!("cannot read {}: {e}", self.display));
                false
            }
        }
    }

    pub fn reload(&mut self) {
        match std::fs::read_to_string(&self.path) {
            Ok(fresh) => self.reload_from(fresh),
            Err(e) => {
                self.diagnostics = vec![EngineError::new(
                    engine_core::codes::SCENE_UNREADABLE,
                    format!("could not read scene: {e}"),
                )
                .file(&self.display)];
                self.file = None;
                self.items.clear();
            }
        }
    }

    fn reload_from(&mut self, source: String) {
        self.source = source;
        self.last_reload = Some(Instant::now());

        // The exact validation the CLI runs: structural pass, then the asset
        // pass only on structurally clean scenes.
        let mut diagnostics =
            engine_core::validate::validate_source(&self.source, &self.display);
        if diagnostics.iter().all(EngineError::is_warning) {
            diagnostics
                .extend(engine_assets::validate_scene_assets(&self.source, &self.display));
        }
        self.diagnostics = diagnostics;

        // Hierarchy wants the parse even when semantics failed.
        self.file = serde_json::from_str(&self.source).ok();
        self.raw = serde_json::from_str(&self.source).ok();

        self.items.clear();
        if self.is_valid() {
            if let Ok(scene) = Scene::from_source(&self.source, &self.display) {
                let assets = engine_assets::AssetServer::for_scene(&self.path);
                match scene.render_items(&assets) {
                    Ok(items) => {
                        self.items = items;
                        self.lights = scene.lights().resolved();
                        // MSAA is the viewport's own business, not the
                        // scene's: the sample count is baked into the
                        // renderer's pipelines and this one is built once.
                        self.environment = engine_core::scene::EnvironmentSettings {
                            samples: 1,
                            ..scene.environment
                        };
                    }
                    Err(e) => self.diagnostics.push(e),
                }
            }
        }
    }

    /// Commit one logical mutation: re-read the file (the gesture may be
    /// older than the newest external write), rebase the edit onto the fresh
    /// contents by name/type, write atomically, reload. On a vanished
    /// target: drop the edit and say so (editor design §5) — never guess.
    pub fn apply(&mut self, edit: &SetComponentField) {
        let fresh = match std::fs::read_to_string(&self.path) {
            Ok(fresh) => fresh,
            Err(e) => {
                self.notice = Some(format!("edit dropped — cannot read file: {e}"));
                return;
            }
        };

        match formatter::apply_set_component_field(&fresh, edit) {
            Ok(edited) => match formatter::write_atomic(&self.path, &edited) {
                Ok(()) => {
                    self.notice = Some(format!(
                        "wrote {}.{}.{}",
                        edit.entity, edit.component, edit.field
                    ));
                    self.reload_from(edited);
                }
                Err(e) => self.notice = Some(format!("write failed: {}", e.message)),
            },
            Err(e) => {
                self.notice = Some(format!("edit dropped — {}", e.message));
            }
        }
    }

    /// Add a component (all fields at their documented defaults) to an
    /// entity — same commit shape as [`apply`](Self::apply): fresh read,
    /// rebase by name, atomic write, reload.
    pub fn add_component(&mut self, entity: &str, component: &str) {
        let fresh = match std::fs::read_to_string(&self.path) {
            Ok(fresh) => fresh,
            Err(e) => {
                self.notice = Some(format!("edit dropped — cannot read file: {e}"));
                return;
            }
        };
        let edit = AddComponent {
            entity: entity.to_string(),
            component: component.to_string(),
            fields: vec![],
        };
        match formatter::apply_add_component(&fresh, &edit) {
            Ok(edited) => match formatter::write_atomic(&self.path, &edited) {
                Ok(()) => {
                    self.notice = Some(format!("added {component} to {entity}"));
                    self.reload_from(edited);
                }
                Err(e) => self.notice = Some(format!("write failed: {}", e.message)),
            },
            Err(e) => self.notice = Some(format!("edit dropped — {}", e.message)),
        }
    }

    /// Remove a component from an entity; a target that vanished under a
    /// concurrent writer drops the edit with a notice, never guesses.
    pub fn remove_component(&mut self, entity: &str, component: &str) {
        let fresh = match std::fs::read_to_string(&self.path) {
            Ok(fresh) => fresh,
            Err(e) => {
                self.notice = Some(format!("edit dropped — cannot read file: {e}"));
                return;
            }
        };
        let edit = RemoveComponent {
            entity: entity.to_string(),
            component: component.to_string(),
        };
        match formatter::apply_remove_component(&fresh, &edit) {
            Ok(edited) => match formatter::write_atomic(&self.path, &edited) {
                Ok(()) => {
                    self.notice = Some(format!("removed {component} from {entity}"));
                    self.reload_from(edited);
                }
                Err(e) => self.notice = Some(format!("write failed: {}", e.message)),
            },
            Err(e) => self.notice = Some(format!("edit dropped — {}", e.message)),
        }
    }

    /// Append a new entity named `base_name` (deduplicated against the
    /// fresh file contents: `pyramid`, `pyramid-2`, …). Same commit shape as
    /// [`apply`](Self::apply): fresh read, splice, atomic write, reload.
    /// Returns the name actually written, for selection.
    pub fn add_entity(
        &mut self,
        base_name: &str,
        components: Vec<(String, Vec<(String, serde_json::Value)>)>,
    ) -> Option<String> {
        let fresh = match std::fs::read_to_string(&self.path) {
            Ok(fresh) => fresh,
            Err(e) => {
                self.notice = Some(format!("import dropped — cannot read file: {e}"));
                return None;
            }
        };

        let taken: Vec<String> = serde_json::from_str::<serde_json::Value>(&fresh)
            .ok()
            .and_then(|root| {
                root["entities"].as_array().map(|entities| {
                    entities
                        .iter()
                        .filter_map(|e| e["name"].as_str().map(str::to_string))
                        .collect()
                })
            })
            .unwrap_or_default();
        let name = crate::import::unique_name(base_name, &taken);

        let edit = AddEntity {
            name: name.clone(),
            components,
        };
        match formatter::apply_add_entity(&fresh, &edit) {
            Ok(edited) => match formatter::write_atomic(&self.path, &edited) {
                Ok(()) => {
                    self.notice = Some(format!("added entity {name}"));
                    self.reload_from(edited);
                    Some(name)
                }
                Err(e) => {
                    self.notice = Some(format!("write failed: {}", e.message));
                    None
                }
            },
            Err(e) => {
                self.notice = Some(format!("import dropped — {}", e.message));
                None
            }
        }
    }

    /// Seconds since the last reload, for the status bar.
    pub fn reload_age(&self) -> Option<f32> {
        self.last_reload.map(|t| t.elapsed().as_secs_f32())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The "+ add" menu's commit, minus the click: a primitive entity splices
    /// into the file, renders, and a second add of the same primitive dedupes.
    #[test]
    fn adding_a_builtin_primitive_writes_and_dedupes() {
        let dir = std::env::temp_dir().join(format!("engine-doc-add-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scene.json");
        std::fs::write(&path, "{\n  \"name\": \"t\",\n  \"entities\": []\n}").unwrap();

        let mut doc = SceneDoc::open(path);
        let components = || {
            vec![
                (
                    "Transform".to_string(),
                    vec![("position".to_string(), serde_json::json!([0.0, 0.0, 0.0]))],
                ),
                (
                    "Mesh".to_string(),
                    vec![(
                        "asset".to_string(),
                        serde_json::Value::String("builtin:cylinder".into()),
                    )],
                ),
            ]
        };

        assert_eq!(doc.add_entity("cylinder", components()).as_deref(), Some("cylinder"));
        assert_eq!(doc.add_entity("cylinder", components()).as_deref(), Some("cylinder-2"));

        assert!(doc.is_valid(), "{:?}", doc.diagnostics);
        assert_eq!(doc.items.len(), 2, "both primitives produce draw items");
        assert!(doc.source.contains("\"builtin:cylinder\""));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
