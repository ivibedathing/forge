//! Scene files and the world they instantiate.
//!
//! A scene file is the whole truth about a scene (invariant 2): parsing one
//! and spawning it reconstructs the world exactly, with nothing carried in
//! memory from a previous session.

use std::collections::HashMap;
use std::path::Path;

use hecs::{Entity, EntityBuilder, World};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::components::{Camera, ComponentData, Name, Transform};
use crate::error::{EngineError, Result};
use crate::validate;

/// A scene file, exactly as it appears on disk.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SceneFile {
    pub name: String,
    pub entities: Vec<EntityDef>,
}

/// One entity in a scene file.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityDef {
    /// Unique within the scene; this is what CLI commands target (invariant 4).
    pub name: String,

    #[serde(default)]
    pub components: Vec<ComponentData>,
}

/// One thing to draw, with its geometry resolved and its transform flattened.
#[derive(Debug, Clone)]
pub struct RenderItem {
    pub mesh: crate::mesh::MeshData,
    pub model: glam::Mat4,
    pub albedo: glam::Vec3,
}

/// A scene loaded into an ECS world.
pub struct Scene {
    pub name: String,
    pub world: World,

    /// Name lookup, mirroring the [`Name`] component so targeting an entity by
    /// name does not require scanning the world.
    by_name: HashMap<String, Entity>,
}

impl Scene {
    /// Read, validate, and instantiate a scene file.
    ///
    /// Returns the first validation error. `engine validate` reports all of
    /// them at once — use [`validate::validate_source`] for that.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let display = path.display().to_string();

        let source = std::fs::read_to_string(path).map_err(|e| {
            EngineError::new("scene_unreadable", format!("could not read scene: {e}"))
                .file(&display)
        })?;

        Self::from_source(&source, &display)
    }

    /// Validate and instantiate a scene already in memory.
    pub fn from_source(source: &str, path: &str) -> Result<Self> {
        let errors = validate::validate_source(source, path);
        if let Some(first) = errors.into_iter().next() {
            return Err(first);
        }

        // Validation already proved this parses; a failure here is a bug in
        // validation rather than in the scene, so it gets its own error code.
        let file: SceneFile = serde_json::from_str(source).map_err(|e| {
            EngineError::new(
                "scene_parse_desync",
                format!("scene passed validation but failed to parse: {e}"),
            )
            .file(path)
        })?;

        Ok(Self::instantiate(file))
    }

    /// Spawn a parsed scene file into a fresh world.
    pub fn instantiate(file: SceneFile) -> Self {
        let mut world = World::new();
        let mut by_name = HashMap::with_capacity(file.entities.len());

        for def in file.entities {
            let mut builder = EntityBuilder::new();
            builder.add(Name(def.name.clone()));

            for component in def.components {
                component.add_to(&mut builder);
            }

            let entity = world.spawn(builder.build());
            by_name.insert(def.name, entity);
        }

        Self {
            name: file.name,
            world,
            by_name,
        }
    }

    /// Look up an entity by its stable name.
    pub fn entity(&self, name: &str) -> Option<Entity> {
        self.by_name.get(name).copied()
    }

    pub fn entity_count(&self) -> usize {
        self.by_name.len()
    }

    /// Every entity name, in unspecified order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }

    /// Resolve the camera to render from.
    ///
    /// With `requested`, picks that entity by name and requires it to have a
    /// `Camera`. Without, picks the one marked `active`. Validation rejects
    /// scenes with more than one active camera, so the choice is unambiguous.
    ///
    /// The returned transform is the camera entity's, defaulting to the
    /// identity when it has none — a camera without a transform sits at the
    /// origin rather than failing to render.
    pub fn camera(&self, requested: Option<&str>) -> Result<(Camera, Transform)> {
        match requested {
            Some(name) => {
                let entity = self.entity(name).ok_or_else(|| {
                    EngineError::new("entity_not_found", format!("no entity named {name:?}"))
                        .entity(name)
                        .suggest_from(name, self.names())
                })?;

                let camera = self.world.get::<&Camera>(entity).map(|c| *c).map_err(|_| {
                    EngineError::new(
                        "missing_component",
                        format!("entity {name:?} exists but has no Camera component"),
                    )
                    .entity(name)
                    .component("Camera")
                })?;

                Ok((camera, self.transform_of(entity)))
            }

            None => {
                // hecs 0.11 yields only the queried components, so `Entity` is
                // requested explicitly as part of the query tuple.
                let (entity, camera) = self
                    .world
                    .query::<(Entity, &Camera)>()
                    .iter()
                    .find(|(_, camera)| camera.active)
                    .map(|(entity, camera)| (entity, *camera))
                    .ok_or_else(|| {
                        EngineError::new(
                            "no_active_camera",
                            "scene has no Camera with \"active\": true; \
                             pass --camera <entity> to choose one explicitly",
                        )
                    })?;

                Ok((camera, self.transform_of(entity)))
            }
        }
    }

    /// Flatten the world into a draw list, resolving mesh assets via `assets`.
    ///
    /// Plain data with no GPU types, so the extraction is testable headlessly
    /// and `engine-render` stays free of ECS queries. Callers with files to
    /// load pass `engine-assets`' `AssetServer`; builtin-only contexts pass
    /// [`BuiltinAssets`](crate::mesh::BuiltinAssets). Entities without a `Mesh`
    /// contribute nothing; a `Mesh` whose asset cannot be loaded is an error
    /// rather than a silent omission.
    pub fn render_items(&self, assets: &dyn crate::mesh::MeshSource) -> Result<Vec<RenderItem>> {
        let mut items = Vec::new();

        for (entity, mesh) in self
            .world
            .query::<(Entity, &crate::components::Mesh)>()
            .iter()
        {
            let data = assets.load_mesh(&mesh.asset).map_err(|e| {
                // Name the entity so the agent knows which one to fix.
                match self.world.get::<&Name>(entity) {
                    Ok(name) => e.entity(name.0.clone()),
                    Err(_) => e,
                }
            })?;

            let transform = self.transform_of(entity);
            let material = self
                .world
                .get::<&crate::components::Material>(entity)
                .map(|m| *m)
                .unwrap_or_default();

            items.push(RenderItem {
                mesh: data,
                model: transform.matrix(),
                albedo: material.albedo,
            });
        }

        Ok(items)
    }

    fn transform_of(&self, entity: Entity) -> Transform {
        self.world
            .get::<&Transform>(entity)
            .map(|t| *t)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Mesh;
    use glam::Vec3;

    const DEMO: &str = r#"{
      "name": "demo",
      "entities": [
        {
          "name": "Player",
          "components": [
            { "type": "Transform", "position": [0.0, 1.0, 0.0] },
            { "type": "Camera", "fov": 60.0, "active": true }
          ]
        },
        {
          "name": "Cube1",
          "components": [
            { "type": "Transform", "position": [0.0, 3.0, 0.0] },
            { "type": "Mesh", "asset": "builtin:cube" }
          ]
        }
      ]
    }"#;

    fn demo() -> Scene {
        Scene::from_source(DEMO, "demo.json").expect("demo scene should load")
    }

    #[test]
    fn spawns_every_entity_with_its_components() {
        let scene = demo();
        assert_eq!(scene.entity_count(), 2);

        let cube = scene.entity("Cube1").expect("Cube1 should exist");
        let transform = scene.world.get::<&Transform>(cube).unwrap();
        assert_eq!(transform.position, Vec3::new(0.0, 3.0, 0.0));

        let mesh = scene.world.get::<&Mesh>(cube).unwrap();
        assert_eq!(mesh.asset, "builtin:cube");
    }

    #[test]
    fn render_items_resolve_through_the_given_source() {
        let scene = demo();
        let items = scene
            .render_items(&crate::mesh::BuiltinAssets)
            .expect("demo scene is builtin-only");
        assert_eq!(items.len(), 1, "one Mesh entity");
        assert_eq!(items[0].mesh, crate::mesh::BuiltinMesh::Cube.data());
    }

    #[test]
    fn render_items_name_the_entity_whose_asset_failed() {
        let source = r#"{"name": "s", "entities": [
            {"name": "Broken", "components": [{"type": "Mesh", "asset": "builtin:cube"}]}
        ]}"#;
        let mut scene = Scene::from_source(source, "s.json").unwrap();
        // Corrupt the asset reference after load, so instantiation succeeds
        // but resolution fails — the render-time backstop path.
        let entity = scene.entity("Broken").unwrap();
        scene
            .world
            .insert_one(
                entity,
                Mesh {
                    asset: "builtin:nope".into(),
                },
            )
            .unwrap();

        let err = scene
            .render_items(&crate::mesh::BuiltinAssets)
            .unwrap_err();
        assert_eq!(err.error, "asset_not_found");
        assert_eq!(err.context().unwrap().entity.as_deref(), Some("Broken"));
    }

    #[test]
    fn attaches_name_as_a_component() {
        let scene = demo();
        let player = scene.entity("Player").unwrap();
        assert_eq!(
            *scene.world.get::<&Name>(player).unwrap(),
            Name("Player".into())
        );
    }

    #[test]
    fn finds_the_active_camera() {
        let scene = demo();
        let (camera, transform) = scene.camera(None).unwrap();
        assert_eq!(camera.fov, 60.0);
        assert_eq!(transform.position, Vec3::new(0.0, 1.0, 0.0));
    }

    #[test]
    fn selects_a_camera_by_name() {
        let scene = demo();
        let (camera, _) = scene.camera(Some("Player")).unwrap();
        assert!(camera.active);
    }

    #[test]
    fn suggests_a_near_miss_entity_name() {
        let scene = demo();
        let err = scene.camera(Some("Playr")).unwrap_err();
        assert_eq!(err.error, "entity_not_found");
        assert_eq!(
            err.context().unwrap().did_you_mean.as_deref(),
            Some("Player")
        );
    }

    #[test]
    fn reports_an_entity_that_is_not_a_camera() {
        let scene = demo();
        let err = scene.camera(Some("Cube1")).unwrap_err();
        assert_eq!(err.error, "missing_component");
    }

    #[test]
    fn reports_a_scene_with_no_active_camera() {
        let source = r#"{"name": "s", "entities": [
            {"name": "A", "components": [{"type": "Camera", "active": false}]}
        ]}"#;
        let scene = Scene::from_source(source, "s.json").unwrap();
        assert_eq!(scene.camera(None).unwrap_err().error, "no_active_camera");
    }

    #[test]
    fn a_camera_without_a_transform_sits_at_the_origin() {
        let source = r#"{"name": "s", "entities": [
            {"name": "Eye", "components": [{"type": "Camera", "active": true}]}
        ]}"#;
        let scene = Scene::from_source(source, "s.json").unwrap();
        let (_, transform) = scene.camera(None).unwrap();
        assert_eq!(transform.position, Vec3::ZERO);
    }
}
