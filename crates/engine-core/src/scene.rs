//! Scene files and the world they instantiate.
//!
//! A scene file is the whole truth about a scene (invariant 2): parsing one
//! and spawning it reconstructs the world exactly, with nothing carried in
//! memory from a previous session.

use std::collections::HashMap;

use hecs::{Entity, EntityBuilder, World};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use glam::Vec3;

use crate::components::{
    AmbientLight, Camera, ComponentData, DirectionalLight, HudRect, HudText, Name, Transform,
};
use crate::error::{EngineError, Result};
use crate::validate;

/// A scene file, exactly as it appears on disk.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SceneFile {
    pub name: String,

    /// Scene-level physics settings; absent means the defaults, so scenes
    /// without physics don't change (M8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physics: Option<PhysicsSettings>,

    pub entities: Vec<EntityDef>,
}

/// The scene-level `physics` block (M8).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct PhysicsSettings {
    #[schemars(with = "[f32; 3]")]
    pub gravity: Vec3,

    /// Fixed steps per simulated second. An **integer** deliberately: `1/60`
    /// has no exact JSON representation, and an integer keeps scene files
    /// free of float-precision noise. `dt = 1.0 / hz`, computed once,
    /// identically everywhere. `>= 1`.
    #[schemars(range(min = 1))]
    pub timestep_hz: u32,
}

impl Default for PhysicsSettings {
    fn default() -> Self {
        Self {
            gravity: Vec3::new(0.0, -9.81, 0.0),
            timestep_hz: 60,
        }
    }
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
    /// The source entity's stable name (invariant 4) — how the editor's
    /// picking and selection resolve a drawn thing back to the file.
    pub entity: String,
    /// Shared, never copied: a viewer rebuilds this list every frame, and the
    /// renderer caches uploaded GPU buffers against this `Arc`'s identity.
    pub mesh: std::sync::Arc<crate::mesh::MeshData>,
    pub model: glam::Mat4,
    pub material: crate::components::Material,
}

/// The scene's screen-space overlay, extracted as plain data in draw order
/// (M12): `rects` under `texts`, each in scene-file order. An empty overlay
/// means no HUD pass runs at all, so pre-M12 scenes render byte-identically.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HudItems {
    pub rects: Vec<HudRect>,
    pub texts: Vec<HudText>,
}

impl HudItems {
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty() && self.texts.is_empty()
    }
}

/// The scene's light entities, extracted as plain data.
///
/// Extraction takes what the (already validated) world contains — validation,
/// not extraction, is what rejects multiple suns. `sun` pairs the component
/// with the world-space direction the light *travels* (the entity's
/// `transform.quat() * -Z`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LightRig {
    pub sun: Option<(DirectionalLight, Vec3)>,
    pub ambient: Option<AmbientLight>,
}

/// Concrete lighting values, ready for a uniform buffer: colors are
/// premultiplied by intensity, the direction is normalized travel direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedLights {
    pub sun_direction: Vec3,
    pub sun_color: Vec3,
    pub ambient: Vec3,
}

impl LightRig {
    /// The travel direction of the fallback sun: arriving from
    /// `normalize((0.4, 1.0, 0.6))`, the same bearing as M2's hardcoded
    /// placeholder light, so scenes written before lighting existed keep
    /// their look.
    pub fn fallback_sun_direction() -> Vec3 {
        -Vec3::new(0.4, 1.0, 0.6).normalize()
    }

    /// Apply the no-lights fallback rule and return concrete values.
    ///
    /// The rule is all-or-nothing (design `materials-lighting-design.md` §3):
    /// zero light components → the documented fallback rig (white sun 1.0 +
    /// white ambient 0.15); at least one light component → exactly what the
    /// scene wrote, absent means off.
    pub fn resolved(&self) -> ResolvedLights {
        if self.sun.is_none() && self.ambient.is_none() {
            return ResolvedLights {
                sun_direction: Self::fallback_sun_direction(),
                sun_color: Vec3::ONE,
                ambient: Vec3::splat(0.15),
            };
        }

        let (sun_color, sun_direction) = match self.sun {
            Some((sun, direction)) => (sun.color * sun.intensity, direction),
            None => (Vec3::ZERO, Vec3::NEG_Z),
        };

        ResolvedLights {
            sun_direction,
            sun_color,
            ambient: self
                .ambient
                .map(|a| a.color * a.intensity)
                .unwrap_or(Vec3::ZERO),
        }
    }
}

/// A scene loaded into an ECS world.
pub struct Scene {
    pub name: String,
    pub world: World,

    /// Scene-level physics settings (defaults when the file has no
    /// `physics` block) — carried so simulation commands need no re-parse.
    pub physics: PhysicsSettings,

    /// Name lookup, mirroring the [`Name`] component so targeting an entity by
    /// name does not require scanning the world.
    by_name: HashMap<String, Entity>,
}

impl Scene {
    /// Validate and instantiate a scene already in memory.
    ///
    /// The error side is *every* validation error, never just the first —
    /// which command you ran must never change what you learn about a broken
    /// scene (M5 §7). Warnings are not errors and do not appear here; run
    /// [`validate::validate_source`] directly to see them.
    pub fn from_source(source: &str, path: &str) -> std::result::Result<Self, Vec<EngineError>> {
        let errors: Vec<EngineError> = validate::validate_source(source, path)
            .into_iter()
            .filter(|e| !e.is_warning())
            .collect();
        if !errors.is_empty() {
            return Err(errors);
        }

        // Validation already proved this parses; a failure here is a bug in
        // validation rather than in the scene, so it gets its own error code.
        let file: SceneFile = serde_json::from_str(source).map_err(|e| {
            vec![EngineError::new(
                crate::codes::SCENE_PARSE_DESYNC,
                format!("scene passed validation but failed to parse: {e}"),
            )
            .file(path)]
        })?;

        Ok(Self::instantiate(file))
    }

    /// Spawn a parsed scene file into a fresh world.
    pub fn instantiate(file: SceneFile) -> Self {
        let physics = file.physics.unwrap_or_default();
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
            physics,
            world,
            by_name,
        }
    }

    /// Look up an entity by its stable name.
    pub fn entity(&self, name: &str) -> Option<Entity> {
        self.by_name.get(name).copied()
    }

    /// Rebuild the name lookup from the world. Call after anything changes
    /// the entity set — a break despawns the parent and spawns fragments.
    pub fn refresh_names(&mut self) {
        self.by_name = self
            .world
            .query::<(Entity, &Name)>()
            .iter()
            .map(|(entity, name)| (name.0.clone(), entity))
            .collect();
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
                    EngineError::new(crate::codes::ENTITY_NOT_FOUND, format!("no entity named {name:?}"))
                        .entity(name)
                        .suggest_from(name, self.names())
                })?;

                let camera = self.world.get::<&Camera>(entity).map(|c| *c).map_err(|_| {
                    EngineError::new(
                        crate::codes::MISSING_COMPONENT,
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
                            crate::codes::NO_ACTIVE_CAMERA,
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

            let name = self
                .world
                .get::<&Name>(entity)
                .map(|n| n.0.clone())
                .unwrap_or_default();

            items.push(RenderItem {
                entity: name,
                mesh: data,
                model: transform.matrix(),
                material,
            });
        }

        Ok(items)
    }

    /// Extract the screen-space overlay as plain data, in draw order (M12):
    /// every `HudRect`, then every `HudText`, each in scene-file order — text
    /// always reads over bars, and within a class the file is the z-order.
    ///
    /// File order is recovered by sorting on entity id: `instantiate` spawns
    /// definitions sequentially into a fresh world and nothing ever despawns,
    /// so hecs ids are monotone in file order even though archetype iteration
    /// is not.
    pub fn hud_items(&self) -> HudItems {
        fn in_file_order<C: Clone + hecs::Component>(world: &World) -> Vec<C> {
            let mut found: Vec<(u64, C)> = world
                .query::<(Entity, &C)>()
                .iter()
                .map(|(entity, c)| (entity.to_bits().get(), c.clone()))
                .collect();
            found.sort_by_key(|(bits, _)| *bits);
            found.into_iter().map(|(_, c)| c).collect()
        }

        HudItems {
            rects: in_file_order::<HudRect>(&self.world),
            texts: in_file_order::<HudText>(&self.world),
        }
    }

    /// Extract the scene's lights as plain data.
    ///
    /// Takes the first of each kind found; validation rejects scenes with more
    /// than one, so on a valid scene "first" is "only". The sun's direction is
    /// where its light travels: the entity's local −Z carried to world space,
    /// identity transform meaning horizontal travel toward −Z — the same
    /// orientation convention the camera uses.
    pub fn lights(&self) -> LightRig {
        let sun = self
            .world
            .query::<(Entity, &DirectionalLight)>()
            .iter()
            .next()
            .map(|(entity, light)| {
                let direction = (self.transform_of(entity).quat() * Vec3::NEG_Z).normalize();
                (*light, direction)
            });

        let ambient = self
            .world
            .query::<&AmbientLight>()
            .iter()
            .next()
            .map(|ambient| *ambient);

        LightRig { sun, ambient }
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
        assert_eq!(*items[0].mesh, crate::mesh::BuiltinMesh::Cube.data());
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
    fn light_direction_follows_the_camera_convention() {
        // The direction-convention pin, analogous to the Euler pin test:
        // rotation [-90, 0, 0] points local -Z straight down, so the light
        // travels (0, -1, 0) — a noon sun.
        let source = r#"{"name": "s", "entities": [
            {"name": "Sun", "components": [
                {"type": "Transform", "rotation": [-90.0, 0.0, 0.0]},
                {"type": "DirectionalLight"}
            ]}
        ]}"#;
        let scene = Scene::from_source(source, "s.json").unwrap();
        let (_, direction) = scene.lights().sun.expect("sun should be extracted");
        assert!(
            (direction - Vec3::NEG_Y).length() < 1e-5,
            "a [-90, 0, 0] sun should travel straight down, got {direction:?}"
        );
    }

    #[test]
    fn zero_light_components_get_the_fallback_rig() {
        let scene = demo(); // the demo fixture has no light entities
        let rig = scene.lights();
        assert_eq!(rig.sun, None);
        assert_eq!(rig.ambient, None);

        let resolved = rig.resolved();
        assert_eq!(resolved.sun_color, Vec3::ONE, "fallback sun is white 1.0");
        assert_eq!(resolved.ambient, Vec3::splat(0.15));
        assert!(
            (resolved.sun_direction - LightRig::fallback_sun_direction()).length() < 1e-6,
            "fallback arrives from the M2 placeholder bearing"
        );
    }

    #[test]
    fn any_light_component_disables_the_fallback() {
        // The other half of the all-or-nothing rule: writing only an ambient
        // light means the sun is OFF, not defaulted.
        let source = r#"{"name": "s", "entities": [
            {"name": "Fill", "components": [{"type": "AmbientLight", "intensity": 0.5}]}
        ]}"#;
        let scene = Scene::from_source(source, "s.json").unwrap();
        let resolved = scene.lights().resolved();
        assert_eq!(resolved.sun_color, Vec3::ZERO, "absent means off");
        assert_eq!(resolved.ambient, Vec3::splat(0.5));
    }

    #[test]
    fn resolved_lights_premultiply_intensity() {
        let source = r#"{"name": "s", "entities": [
            {"name": "Sun", "components": [
                {"type": "DirectionalLight", "color": [1.0, 0.5, 0.0], "intensity": 2.0}
            ]}
        ]}"#;
        let scene = Scene::from_source(source, "s.json").unwrap();
        let resolved = scene.lights().resolved();
        assert_eq!(resolved.sun_color, Vec3::new(2.0, 1.0, 0.0));
        assert_eq!(
            resolved.sun_direction,
            Vec3::NEG_Z,
            "no Transform means horizontal travel toward -Z, documented not an error"
        );
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
