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
    AmbientLight, Camera, ComponentData, DirectionalLight, HudImage, HudInteract, HudPanel,
    HudRect, HudText, Name, PointLight, Transform, MAX_POINT_LIGHTS,
};
use crate::daylight::DaylightSettings;
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

    /// Scene-level rendering settings; absent means the defaults, which are
    /// the pre-M16 renderer exactly (M16).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<EnvironmentSettings>,

    /// The day/night block (M21) — a sibling of `physics` and `environment`,
    /// not a field inside either. Absent means the pre-M21 engine exactly:
    /// nothing computes a sky, and the lights are whatever the entities say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daylight: Option<DaylightSettings>,

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

/// The scene-level `environment` block (M16): how this scene is *rendered*,
/// as opposed to what is in it.
///
/// Every field defaults to the pre-M16 renderer — no sky, no fog, no shadows,
/// one sample — so a scene that says nothing about its environment renders
/// byte-identically to before the block existed. That is deliberate and is
/// what let M16 land without re-blessing baselines it had no business
/// touching: the features are opt-in per scene, in the scene file, because a
/// screenshot has to be reproducible from the file alone (invariant 2).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct EnvironmentSettings {
    /// Draw a gradient sky behind the scene instead of the flat clear color.
    pub sky: bool,

    /// Sky color straight overhead. Linear RGB, like every other color in the
    /// engine — and unclamped above 1, because a sky is a light source and
    /// clamping it to reflectance range makes noon look like dusk.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0)))]
    pub sky_zenith: Vec3,

    /// Sky color at the horizon. This is also the fog color: fog that does
    /// not match the sky it fades into reads as a gray wall, and having one
    /// field means it cannot be set wrong.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0)))]
    pub sky_horizon: Vec3,

    /// Color below the horizon — what a ground plane fades into at distance.
    /// Usually a darker, less saturated version of the terrain.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0)))]
    pub sky_ground: Vec3,

    /// Exponential-squared distance fog: `1 - exp(-(d * density)^2)`. `0`
    /// disables it. Around `0.008` is a light haze at 100 m; `0.05` is thick.
    /// `>= 0`.
    #[schemars(range(min = 0.0))]
    pub fog_density: f32,

    /// Cast shadows from the scene's `DirectionalLight`.
    pub shadows: bool,

    /// How far from the camera the shadow map covers, in meters. The map is a
    /// fixed resolution, so this trades area against sharpness: a 40 m box is
    /// crisp, a 200 m box is blocky. Strictly positive.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub shadow_distance: f32,

    /// Multisample count for the scene pass: `1` (off) or `4`. The HUD is
    /// composited after the resolve and is never multisampled, so text stays
    /// pixel-exact either way.
    #[schemars(range(min = 1, max = 4))]
    pub samples: u32,
}

impl Default for EnvironmentSettings {
    fn default() -> Self {
        Self {
            sky: false,
            // A clear-day rig, used only when `sky` is turned on.
            sky_zenith: Vec3::new(0.19, 0.34, 0.62),
            sky_horizon: Vec3::new(0.62, 0.71, 0.82),
            sky_ground: Vec3::new(0.16, 0.16, 0.17),
            fog_density: 0.0,
            shadows: false,
            shadow_distance: 60.0,
            samples: 1,
        }
    }
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
    /// The material's maps, resolved to shared pixels (M26). Empty for a
    /// material with none, which is what keeps such a draw on the pipeline that
    /// compiles `mesh.wgsl` as it sits on disk.
    pub textures: crate::texture::MaterialTextures,
    /// Set when this item is a [`Terrain`](crate::components::Terrain) patch
    /// (M22), which shades itself per pixel from its layers rather than from
    /// `material`.
    ///
    /// Terrain rides the mesh draw list instead of getting its own the way water
    /// does, and the difference between the two is the reason: water *shades*
    /// differently — reflection, absorption, foam — while terrain is an ordinary
    /// opaque lit surface that happens to compute its own albedo and roughness.
    /// Sharing the list gives it shadows, fog, MSAA, the hemispheric sky ambient
    /// and point lights (the tour's campfire has to light the ground it stands
    /// on) with no second copy of any of them, and hands the editor's picking
    /// and selection a surface they already know how to handle.
    pub terrain: Option<crate::components::Terrain>,
    /// The joint palette this draw is skinned by (M30), computed on the CPU by
    /// [`crate::skeleton::palette`] — **empty for everything that is not a
    /// skinned mesh**, which routes the draw onto the pipelines that compile
    /// `mesh.wgsl` as it sits on disk.
    ///
    /// The entity's own `model` is *not* folded in: the vertex stage computes
    /// `model · Σ wᵢ · palette[jᵢ] · position`, because glTF says the transform
    /// of the node referencing a skinned mesh is ignored and the engine's
    /// `Transform` is what places the character.
    pub joints: Vec<glam::Mat4>,
}

/// One water surface, with its geometry resolved and its transform flattened
/// (M18) — [`RenderItem`]'s counterpart for entities that own their surface
/// instead of referencing a mesh.
///
/// Kept a separate list rather than a variant of `RenderItem` because water is
/// a separate pipeline with its own uniforms: folding it in would put a
/// `Material` on entities that have none and hand every existing consumer of
/// the draw list (picking, the editor's selection, the shadow pass) a case it
/// does not want.
#[derive(Debug, Clone)]
pub struct WaterItem {
    /// The source entity's stable name (invariant 4).
    pub entity: String,
    /// The tessellated unit grid, shared across frames — see
    /// [`crate::water::surface_grid`].
    pub mesh: std::sync::Arc<crate::mesh::MeshData>,
    pub model: glam::Mat4,
    pub water: crate::components::Water,
}

/// One cloud, with its geometry grown and its transform flattened (M20) — a
/// [`WaterItem`] for the sky, and separate from [`RenderItem`] for the same
/// reason: a cloud has no `Material`, and its own pipeline reads fields no mesh
/// has.
#[derive(Debug, Clone)]
pub struct CloudItem {
    /// The source entity's stable name (invariant 4).
    pub entity: String,
    /// The grown lobe cluster, shared across frames — see
    /// [`crate::cloud::mesh_for`].
    pub mesh: std::sync::Arc<crate::mesh::MeshData>,
    pub model: glam::Mat4,
    pub cloud: crate::components::Cloud,
}

/// One road, with its geometry generated and its transform flattened (M23) —
/// [`WaterItem`]'s sibling, and separate from [`RenderItem`] for the same
/// reasons: a road has no `Material`, its shading is its own pipeline, and
/// folding it into the draw list would hand every consumer of that list
/// (picking, selection, the shadow pass) a case it does not want.
#[derive(Debug, Clone)]
pub struct RoadItem {
    /// The source entity's stable name (invariant 4).
    pub entity: String,
    /// The generated ribbon, plus the marking layout the shader cannot derive
    /// per pixel. Shared across frames — see [`crate::road::surface`].
    pub surface: std::sync::Arc<crate::road::RoadSurface>,
    pub model: glam::Mat4,
    pub road: crate::components::Road,
}

/// One meadow, with its plants grown and placed (M29) — [`WaterItem`]'s
/// sibling, separate from [`RenderItem`] for the same reasons and one more: a
/// meadow is drawn **instanced**, so it is the only draw list whose geometry is
/// a template plus a placement, rather than a mesh.
///
/// There is no `model` matrix. `patch.instances` are already in world space,
/// because placement had to consult the terrain the meadow stands on and a
/// plant whose height came off the ground cannot then be pushed around by a
/// transform without leaving it.
#[derive(Debug, Clone)]
pub struct MeadowItem {
    /// The source entity's stable name (invariant 4).
    pub entity: String,
    /// The plant template and every copy of it, shared across frames — see
    /// [`crate::meadow::patch_for`].
    pub patch: std::sync::Arc<crate::meadow::MeadowPatch>,
    pub meadow: crate::components::Meadow,
}

/// The scene's screen-space overlay. M12's `HudItems { rects, texts }` became
/// [`crate::ui::HudTree`] in M31, because the hierarchy is a `parent` name in a
/// flat list rather than two lists split by type.
pub use crate::ui::HudTree;

/// The scene's light entities, extracted as plain data.
///
/// Extraction takes what the (already validated) world contains — validation,
/// not extraction, is what rejects multiple suns. `sun` pairs the component
/// with the world-space direction the light *travels* (the entity's
/// `transform.quat() * -Z`).
#[derive(Debug, Clone, PartialEq)]
pub struct LightRig {
    pub sun: Option<(DirectionalLight, Vec3)>,
    pub ambient: Option<AmbientLight>,
    /// Every [`PointLight`] paired with its world position, in entity-name
    /// order — the uniform array is built from this, and a light's index in it
    /// must not depend on archetype iteration. Validation caps the length at
    /// [`MAX_POINT_LIGHTS`].
    pub points: Vec<(PointLight, Vec3)>,
}

/// One point light resolved for upload: color premultiplied by intensity.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ResolvedPointLight {
    pub position: Vec3,
    pub color: Vec3,
    pub range: f32,
}

/// Concrete lighting values, ready for a uniform buffer: colors are
/// premultiplied by intensity, the direction is normalized travel direction.
///
/// Point lights ride in a fixed-size array with a count rather than a `Vec`,
/// which keeps this **`Copy`**. That matters: the viewer resolves lights every
/// frame, and the shader's array is fixed-size regardless, so a heap allocation
/// per frame would buy nothing but a per-frame allocation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedLights {
    pub sun_direction: Vec3,
    pub sun_color: Vec3,
    pub ambient: Vec3,
    /// Only the first `point_count` entries are meaningful.
    pub points: [ResolvedPointLight; MAX_POINT_LIGHTS],
    pub point_count: usize,
}

impl ResolvedLights {
    /// The live point lights — the slice every consumer should read.
    pub fn live_points(&self) -> &[ResolvedPointLight] {
        &self.points[..self.point_count.min(MAX_POINT_LIGHTS)]
    }
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
    /// The rule is all-or-nothing (M4's design, §3):
    /// zero light components → the documented fallback rig (white sun 1.0 +
    /// white ambient 0.15); at least one light component → exactly what the
    /// scene wrote, absent means off.
    pub fn resolved(&self) -> ResolvedLights {
        // A `PointLight` counts as lighting the scene, so a campfire-only scene
        // stays dark outside its firelight instead of getting a free sun.
        if self.sun.is_none() && self.ambient.is_none() && self.points.is_empty() {
            return ResolvedLights {
                sun_direction: Self::fallback_sun_direction(),
                sun_color: Vec3::ONE,
                ambient: Vec3::splat(0.15),
                points: [ResolvedPointLight::default(); MAX_POINT_LIGHTS],
                point_count: 0,
            };
        }

        let (sun_color, sun_direction) = match self.sun {
            Some((sun, direction)) => (sun.color * sun.intensity, direction),
            None => (Vec3::ZERO, Vec3::NEG_Z),
        };

        // Validation caps the count, so the surplus a `zip` would drop here can
        // only exist on a scene that never loaded.
        let mut points = [ResolvedPointLight::default(); MAX_POINT_LIGHTS];
        for (slot, (light, position)) in points.iter_mut().zip(self.points.iter()) {
            *slot = ResolvedPointLight {
                position: *position,
                color: light.color * light.intensity,
                range: light.range,
            };
        }

        ResolvedLights {
            sun_direction,
            sun_color,
            ambient: self
                .ambient
                .map(|a| a.color * a.intensity)
                .unwrap_or(Vec3::ZERO),
            points,
            point_count: self.points.len().min(MAX_POINT_LIGHTS),
        }
    }
}

/// Fold a day into a scene's lights and environment.
///
/// A free function rather than a method because the viewer does not keep a
/// `Scene` in its render content — it holds the resolved values and re-folds
/// them every frame against the fixed-step clock. Both paths must agree
/// exactly, so there is one implementation and no second copy to drift.
///
/// `settings` of `None` returns its inputs untouched. That is not a
/// convenience: it is what makes a scene with no `daylight` block render
/// byte-identically to the pre-M21 engine, because it is literally the same
/// values flowing on through the same code.
pub fn apply_daylight(
    settings: Option<&DaylightSettings>,
    time: f32,
    lights: ResolvedLights,
    environment: EnvironmentSettings,
) -> (ResolvedLights, EnvironmentSettings) {
    let Some(settings) = settings else {
        return (lights, environment);
    };

    let day = settings.evaluate(time);
    let mut lights = lights;
    let mut environment = environment;

    if settings.drives_sun {
        // Validation rejects an authored `DirectionalLight` alongside
        // `drives_sun`, so there is no second owner to reconcile with: the sun
        // is whatever the day says it is.
        lights.sun_direction = day.light_direction;
        lights.sun_color = day.light_color;
    }

    if settings.drives_sky {
        environment.sky_zenith = day.sky_zenith;
        environment.sky_horizon = day.sky_horizon;
        environment.sky_ground = day.sky_ground;
        // Ambient rides with the sky rather than with the sun: it *is* the
        // sky's contribution, which is why M16 gates hemispheric ambient on
        // `sky` in the first place. A scene keeping its own `AmbientLight`
        // sets `drives_sky: false`.
        lights.ambient = day.ambient;
    }

    // The palette scales the scene's authored density rather than replacing
    // it, so a scene with `fog_density: 0` stays clear all day however misty
    // the palette's dawn is.
    environment.fog_density *= day.fog_scale;

    (lights, environment)
}

/// A scene loaded into an ECS world.
pub struct Scene {
    pub name: String,
    pub world: World,

    /// Scene-level physics settings (defaults when the file has no
    /// `physics` block) — carried so simulation commands need no re-parse.
    pub physics: PhysicsSettings,

    /// Scene-level rendering settings (defaults when the file has no
    /// `environment` block), carried for the same reason.
    ///
    /// These are what the scene *authored*. When a `daylight` block is
    /// present it computes some of them, so the values a frame actually
    /// renders with come from [`Scene::resolved_at`] — not from here.
    pub environment: EnvironmentSettings,

    /// The day/night block (M21), or `None` for a scene that has no opinion
    /// about the time of day. `None` is the pre-M21 engine exactly.
    pub daylight: Option<DaylightSettings>,

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
        let mut file: SceneFile = serde_json::from_str(source).map_err(|e| {
            vec![EngineError::new(
                crate::codes::SCENE_PARSE_DESYNC,
                format!("scene passed validation but failed to parse: {e}"),
            )
            .file(path)]
        })?;

        // File-backed materials are filled in here rather than at every point
        // of use, so nothing downstream has to know a material can live in
        // another file. Validation has already proved each one resolves, so a
        // failure at this point is a file that changed underneath us.
        let base_dir = std::path::Path::new(path)
            .parent()
            .unwrap_or(std::path::Path::new(""));
        let errors = crate::material::resolve_scene_materials(&mut file, base_dir);
        if !errors.is_empty() {
            return Err(errors.into_iter().map(|e| e.file(path)).collect());
        }

        Ok(Self::instantiate(file))
    }

    /// Spawn a parsed scene file into a fresh world.
    pub fn instantiate(file: SceneFile) -> Self {
        let physics = file.physics.unwrap_or_default();
        let environment = file.environment.unwrap_or_default();
        let daylight = file.daylight;
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
            environment,
            daylight,
            world,
            by_name,
        }
    }

    /// The lights and environment to render with at `time` seconds of scene
    /// time — the pair every render path should ask for instead of reading
    /// `lights().resolved()` and `environment` separately.
    ///
    /// With no `daylight` block this returns exactly those two values,
    /// untouched, which is what makes the absent-block path byte-identical to
    /// the pre-M21 engine: same values, same types, same code downstream.
    ///
    /// With one, the day is evaluated and folded in according to `drives_sun`
    /// and `drives_sky` (design §6).
    pub fn resolved_at(&self, time: f32) -> (ResolvedLights, EnvironmentSettings) {
        apply_daylight(
            self.daylight.as_ref(),
            time,
            self.lights().resolved(),
            self.environment,
        )
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
                    EngineError::new(
                        crate::codes::ENTITY_NOT_FOUND,
                        format!("no entity named {name:?}"),
                    )
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
    pub fn render_items(
        &self,
        assets: &dyn crate::texture::AssetSource,
    ) -> Result<Vec<RenderItem>> {
        self.render_items_at(assets, None)
    }

    /// The draw list with every skinned mesh posed at scene time `time` (M30).
    ///
    /// `None` is the **rest pose** — a skinned mesh drawn as its file authored
    /// it, which is what the editor's viewport shows (it shows scenes at rest)
    /// and what an entity with a skin and no `AnimationPlayer` renders at any
    /// time. `Some(t)` walks each player's clip through the same
    /// speed/offset/looping arithmetic property clips use, so one clock drives
    /// both kinds of animation.
    ///
    /// It is a separate entry point rather than a parameter on `render_items`
    /// because the palette is the *only* thing in this list that depends on
    /// time: everything else was already posed by the caller before it got
    /// here, so twenty call sites that cannot have a skin would be threading a
    /// number nothing reads.
    pub fn render_items_at(
        &self,
        assets: &dyn crate::texture::AssetSource,
        time: Option<f32>,
    ) -> Result<Vec<RenderItem>> {
        let mut items = Vec::new();

        for (entity, mesh) in self
            .world
            .query::<(Entity, &crate::components::Mesh)>()
            .iter()
        {
            let named = |e: crate::error::EngineError| match self.world.get::<&Name>(entity) {
                // Name the entity so the agent knows which one to fix.
                Ok(name) => e.entity(name.0.clone()),
                Err(_) => e,
            };
            let data = assets.load_mesh(&mesh.asset).map_err(named)?;
            let joints = if data.is_skinned() {
                self.palette_for(entity, &mesh.asset, assets, time)
                    .map_err(named)?
            } else {
                // The overwhelmingly common case, and the one that must cost
                // nothing: no rig is even asked for.
                Vec::new()
            };

            let transform = self.transform_of(entity);
            let material = self
                .world
                .get::<&crate::components::Material>(entity)
                .map(|m| (*m).clone())
                .unwrap_or_default();

            let name = self
                .world
                .get::<&Name>(entity)
                .map(|n| n.0.clone())
                .unwrap_or_default();

            let textures = crate::texture::MaterialTextures::resolve(&material, assets)
                .map_err(|e| e.entity(name.clone()))?;

            items.push(RenderItem {
                entity: name,
                mesh: data,
                model: transform.matrix(),
                material,
                textures,
                terrain: None,
                joints,
            });
        }

        // Trees carry their geometry instead of referencing it (M19), and one
        // tree is two draws: bark under the entity's own `Material`, leaves
        // under the tree's foliage fields. Both items keep the entity's name,
        // so picking and selection resolve a tree back to one place in the
        // file like anything else.
        for (entity, tree) in self
            .world
            .query::<(Entity, &crate::components::Tree)>()
            .iter()
        {
            let grown = crate::tree::meshes_for(tree);
            let model = self.transform_of(entity).matrix();
            let name = self
                .world
                .get::<&Name>(entity)
                .map(|n| n.0.clone())
                .unwrap_or_default();
            let bark = self
                .world
                .get::<&crate::components::Material>(entity)
                .map(|m| (*m).clone())
                .unwrap_or_default();

            let bark_textures = crate::texture::MaterialTextures::resolve(&bark, assets)
                .map_err(|e| e.entity(name.clone()))?;
            let leaf_material = tree.leaf_material();
            let leaf_textures = crate::texture::MaterialTextures::resolve(&leaf_material, assets)
                .map_err(|e| e.entity(name.clone()))?;

            items.push(RenderItem {
                entity: name.clone(),
                mesh: grown.bark,
                model,
                material: bark,
                textures: bark_textures,
                terrain: None,
                joints: Vec::new(),
            });
            if let Some(leaves) = grown.leaves {
                items.push(RenderItem {
                    entity: name,
                    mesh: leaves,
                    model,
                    material: leaf_material,
                    textures: leaf_textures,
                    terrain: None,
                    joints: Vec::new(),
                });
            }
        }
        items.extend(self.terrain_items());

        Ok(items)
    }

    /// The joint palette for one skinned entity at scene time `time` (M30), or
    /// an empty vector when the mesh turns out to carry no skin after all.
    ///
    /// A mesh with `JOINTS_0` and a file with no `skin` is a malformed export
    /// rather than a scene error: it renders unskinned — in bind space, where
    /// its vertices already are — instead of failing a render that validation
    /// let through.
    fn palette_for(
        &self,
        entity: Entity,
        asset: &str,
        assets: &dyn crate::texture::AssetSource,
        time: Option<f32>,
    ) -> Result<Vec<glam::Mat4>> {
        let rig = assets.load_rig(asset)?;
        let Some(skin) = &rig.skin else {
            return Ok(Vec::new());
        };

        let player = self
            .world
            .get::<&crate::components::AnimationPlayer>(entity)
            .ok()
            .map(|player| (*player).clone());
        // A property clip on a skinned entity is legal: it animates components,
        // not joints, and the rig stays at rest.
        let clip = player.as_ref().and_then(|player| {
            match crate::skeleton::ClipRef::parse(&player.clip) {
                crate::skeleton::ClipRef::Skeletal { clip, .. } => rig.clip_named(clip),
                crate::skeleton::ClipRef::Property(_) => None,
            }
        });

        let local = match (time, &player, clip) {
            (Some(t), Some(player), Some(clip)) => {
                crate::animation::local_time(player, crate::skeleton::duration(clip), t)
            }
            // No clock, no player, or a player whose clip this file does not
            // have: the rest pose. It still needs a palette — the vertices are
            // in skin space, so `global · inverse_bind` is what puts them back
            // in bind space, and an identity palette would collapse any rig
            // whose rest pose is not exactly its bind pose.
            _ => 0.0,
        };
        let globals = self.posed_globals(entity, skin, time.and(clip), local);
        Ok(globals
            .into_iter()
            .zip(&skin.joints)
            .map(|(global, joint)| global * joint.inverse_bind)
            .collect())
    }

    /// One skinned entity's joints in skin space at clip time `local`, with
    /// M32's foot planting applied when the entity asks for it.
    pub fn posed_globals(
        &self,
        entity: Entity,
        skin: &crate::skeleton::SkinData,
        clip: Option<&crate::skeleton::SkeletalClip>,
        local: f32,
    ) -> Vec<glam::Mat4> {
        crate::locomotion::posed_globals(&self.world, entity, skin, clip, local)
    }

    /// An entity's `FootPlant`, for callers that need to know which joints are
    /// feet — `engine list-joints` measuring a clip's stride, chiefly.
    pub fn foot_plant_of(&self, entity: Entity) -> Option<crate::components::FootPlant> {
        self.world
            .get::<&crate::components::FootPlant>(entity)
            .ok()
            .map(|p| (*p).clone())
    }

    /// Terrain patches, as draw items with their surfaces generated (M22).
    ///
    /// Takes no [`MeshSource`](crate::mesh::MeshSource) and cannot fail — a
    /// patch's geometry is generated from its own fields, like water's — and the
    /// surface comes back as a cached `Arc`, so the renderer uploads each patch
    /// once for the life of the run however many frames pass.
    ///
    /// Sorted by entity name, so the draw list does not depend on how hecs
    /// happened to lay out archetypes.
    pub fn terrain_items(&self) -> Vec<RenderItem> {
        let mut items: Vec<RenderItem> = self
            .world
            .query::<(Entity, &crate::components::Terrain)>()
            .iter()
            .map(|(entity, terrain)| {
                let transform = self.transform_of(entity);
                RenderItem {
                    entity: self
                        .world
                        .get::<&Name>(entity)
                        .map(|n| n.0.clone())
                        .unwrap_or_default(),
                    mesh: crate::terrain::surface_grid(
                        terrain,
                        glam::Vec2::new(transform.position.x, transform.position.z),
                        glam::Vec2::new(transform.scale.x, transform.scale.z),
                    ),
                    model: transform.matrix(),
                    // Opaque, non-metallic, unlit-by-emission: everything the
                    // layers do not decide. `albedo` and `roughness` here are
                    // what a terrain with no layers at all would use, and the
                    // shader replaces both the moment there is one.
                    material: crate::components::Material::default(),
                    // Terrain does not sample a material's maps in M26: it
                    // shades itself from its layers, and a textured × terrain
                    // producer is the variant the material design defers.
                    textures: crate::texture::MaterialTextures::default(),
                    terrain: Some(terrain.clone()),
                    joints: Vec::new(),
                }
            })
            .collect();
        items.sort_by(|a, b| a.entity.cmp(&b.entity));
        items
    }

    /// The height of a terrain patch at a world XZ position, in world metres
    /// (M22) — what `world.terrain_height` and prop placement resolve through.
    ///
    /// `None` when the entity does not exist or has no `Terrain`. Applies the
    /// patch's own Y and `Transform.scale.y`, so the answer is a world
    /// coordinate a caller can assign to a position directly.
    pub fn terrain_height(&self, name: &str, x: f32, z: f32) -> Option<f32> {
        let entity = self.entity(name)?;
        let terrain = self.world.get::<&crate::components::Terrain>(entity).ok()?;
        let transform = self.transform_of(entity);
        Some(crate::terrain::world_height_at(&terrain, &transform, x, z))
    }

    /// Flatten the world's water surfaces into a draw list (M18).
    ///
    /// Takes no [`MeshSource`](crate::mesh::MeshSource): a water entity's
    /// geometry is generated, not loaded, so this cannot fail — and the grid
    /// comes back as a cached `Arc`, so the renderer uploads one surface per
    /// tessellation for the life of the run however many frames pass.
    ///
    /// Sorted by entity name, for the same reason the lights are: a fixed
    /// order that does not depend on how hecs happened to lay out archetypes.
    /// (Drawing order is decided later, back-to-front from the camera.)
    pub fn water_items(&self) -> Vec<WaterItem> {
        let mut items: Vec<WaterItem> = self
            .world
            .query::<(Entity, &crate::components::Water)>()
            .iter()
            .map(|(entity, water)| WaterItem {
                entity: self
                    .world
                    .get::<&Name>(entity)
                    .map(|n| n.0.clone())
                    .unwrap_or_default(),
                mesh: crate::water::surface_grid(water.segments),
                model: self.transform_of(entity).matrix(),
                water: water.clone(),
            })
            .collect();
        items.sort_by(|a, b| a.entity.cmp(&b.entity));
        items
    }

    /// Flatten the world's meadows into a draw list (M29).
    ///
    /// Takes no [`MeshSource`](crate::mesh::MeshSource) and cannot fail, for
    /// water's reason: the geometry is grown from the component, not loaded.
    ///
    /// Time is deliberately absent, as it is for clouds — the life cycle is
    /// evaluated in the vertex stage from the frame's own clock. That is what
    /// keeps this a pure function of the file and keeps the patch's `Arc`
    /// identity stable across frames, which the renderer's upload cache depends
    /// on. A meadow whose plants were regrown on the CPU each frame would defeat
    /// M15's geometry cache exactly as CPU wave displacement would have.
    ///
    /// A `terrain` naming an entity that is not a `Terrain` falls back to flat
    /// ground here rather than failing: validation has already refused that file
    /// (`meadow_terrain_invalid`), and the render path does not re-litigate what
    /// `validate` owns.
    ///
    /// Sorted by entity name, for the reason every other draw list is.
    pub fn meadow_items(&self) -> Vec<MeadowItem> {
        let mut items: Vec<MeadowItem> = self
            .world
            .query::<(Entity, &crate::components::Meadow)>()
            .iter()
            .map(|(entity, meadow)| {
                let model = self.transform_of(entity).matrix();
                // Resolve the ground first: the borrows have to outlive the
                // `patch_for` call that reads through them.
                let ground_entity = meadow.terrain.as_deref().and_then(|name| self.entity(name));
                let ground_terrain = ground_entity
                    .and_then(|e| self.world.get::<&crate::components::Terrain>(e).ok());
                let ground_transform = ground_entity.map(|e| self.transform_of(e));
                let ground = match (&ground_terrain, &ground_transform) {
                    (Some(terrain), Some(transform)) => {
                        Some(crate::meadow::Ground { terrain, transform })
                    }
                    _ => None,
                };

                MeadowItem {
                    entity: self
                        .world
                        .get::<&Name>(entity)
                        .map(|n| n.0.clone())
                        .unwrap_or_default(),
                    patch: crate::meadow::patch_for(meadow, model, ground),
                    meadow: meadow.clone(),
                }
            })
            .collect();
        items.sort_by(|a, b| a.entity.cmp(&b.entity));
        items
    }

    /// Flatten the world's clouds into a draw list (M20).
    ///
    /// Takes no [`MeshSource`](crate::mesh::MeshSource) and cannot fail, like
    /// [`water_items`](Self::water_items): a cloud's geometry is grown, not
    /// loaded, and comes back as a cached `Arc` so the renderer uploads each
    /// distinct cloud once for the life of the run.
    ///
    /// Time is deliberately absent. `drift` is applied in the vertex stage from
    /// the frame's own clock, which is what keeps this a pure function of the
    /// file — and keeps the grown mesh's `Arc` identity stable across frames,
    /// which the renderer's upload cache depends on.
    ///
    /// Sorted by entity name, for the reason the lights and the water surfaces
    /// are: a fixed order that does not depend on hecs' archetype layout.
    pub fn cloud_items(&self) -> Vec<CloudItem> {
        let mut items: Vec<CloudItem> = self
            .world
            .query::<(Entity, &crate::components::Cloud)>()
            .iter()
            .map(|(entity, cloud)| CloudItem {
                entity: self
                    .world
                    .get::<&Name>(entity)
                    .map(|n| n.0.clone())
                    .unwrap_or_default(),
                mesh: crate::cloud::mesh_for(cloud),
                model: self.transform_of(entity).matrix(),
                cloud: cloud.clone(),
            })
            .collect();
        items.sort_by(|a, b| a.entity.cmp(&b.entity));
        items
    }

    /// Flatten the world's roads into a draw list (M23).
    ///
    /// Takes no [`MeshSource`](crate::mesh::MeshSource) for water's reason: a
    /// road's geometry is generated from its own component, not loaded, so this
    /// cannot fail — and the ribbon comes back as a cached `Arc`, so a road
    /// that is not being edited uploads once for the life of the run.
    ///
    /// Sorted by entity name, so a road's place in the frame does not depend on
    /// how hecs happened to lay out archetypes.
    pub fn road_items(&self) -> Vec<RoadItem> {
        let mut items: Vec<RoadItem> = self
            .world
            .query::<(Entity, &crate::components::Road)>()
            .iter()
            .map(|(entity, road)| RoadItem {
                entity: self
                    .world
                    .get::<&Name>(entity)
                    .map(|n| n.0.clone())
                    .unwrap_or_default(),
                surface: crate::road::surface(road),
                model: self.transform_of(entity).matrix(),
                road: road.clone(),
            })
            .collect();
        items.sort_by(|a, b| a.entity.cmp(&b.entity));
        items
    }

    /// Extract the screen-space overlay as plain data, in **scene-file order**
    /// (M12, generalized in M31).
    ///
    /// The list is not in draw order any more — `ui::layout` decides that, from
    /// the tree the `parent` names describe — but file order is still what
    /// breaks ties among siblings, so producing it in file order is a contract
    /// rather than a convenience.
    ///
    /// File order is recovered by sorting on entity id: `instantiate` spawns
    /// definitions sequentially into a fresh world and nothing ever despawns,
    /// so hecs ids are monotone in file order even though archetype iteration
    /// is not.
    ///
    /// `textures` resolves each `HudImage`'s PNG. A failure is *not* an error
    /// here: the reference was already checked by `validate`, and a render path
    /// with no asset directory (a GPU-less test) must still lay the overlay
    /// out. An unresolved image lays out at its authored size and draws
    /// nothing.
    pub fn hud_tree(&self, textures: &dyn crate::texture::TextureSource) -> HudTree {
        use crate::ui::{HudKind, HudNode};

        fn in_file_order<C: Clone + hecs::Component>(world: &World) -> Vec<(u64, Entity, C)> {
            let mut found: Vec<(u64, Entity, C)> = world
                .query::<(Entity, &C)>()
                .iter()
                .map(|(entity, c)| (entity.to_bits().get(), entity, c.clone()))
                .collect();
            found.sort_by_key(|(bits, _, _)| *bits);
            found
        }

        let name_of = |entity: Entity| {
            self.world
                .get::<&Name>(entity)
                .map(|n| n.0.clone())
                .unwrap_or_default()
        };

        let mut nodes: Vec<(u64, HudNode)> = Vec::new();
        let mut push = |bits: u64, entity: Entity, kind: HudKind| {
            nodes.push((
                bits,
                HudNode {
                    entity: name_of(entity),
                    kind,
                    interact: self
                        .world
                        .get::<&HudInteract>(entity)
                        .map(|i| (*i).clone())
                        .ok(),
                },
            ));
        };

        for (bits, entity, panel) in in_file_order::<HudPanel>(&self.world) {
            push(bits, entity, HudKind::Panel(panel));
        }
        for (bits, entity, rect) in in_file_order::<HudRect>(&self.world) {
            push(bits, entity, HudKind::Rect(rect));
        }
        for (bits, entity, image) in in_file_order::<HudImage>(&self.world) {
            let pixels = textures
                .load_texture(&image.texture, crate::texture::ColorSpace::Srgb)
                .ok();
            push(bits, entity, HudKind::Image(image, pixels));
        }
        for (bits, entity, text) in in_file_order::<HudText>(&self.world) {
            push(bits, entity, HudKind::Text(text));
        }

        // One flat list, back in file order across all four kinds.
        nodes.sort_by_key(|(bits, _)| *bits);
        HudTree {
            nodes: nodes.into_iter().map(|(_, node)| node).collect(),
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

        let ambient = self.world.query::<&AmbientLight>().iter().next().copied();

        // Point lights are plural, so unlike the sun their order is visible in
        // the uniform array — sort by entity name, the same determinism rule
        // wheels and collision layers follow.
        let mut points: Vec<(String, (PointLight, Vec3))> = self
            .world
            .query::<(Entity, &PointLight, &Name)>()
            .iter()
            .map(|(entity, light, name)| {
                (name.0.clone(), (*light, self.transform_of(entity).position))
            })
            .collect();
        points.sort_by(|a, b| a.0.cmp(&b.0));

        LightRig {
            sun,
            ambient,
            points: points.into_iter().map(|(_, light)| light).collect(),
        }
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

        let err = scene.render_items(&crate::mesh::BuiltinAssets).unwrap_err();
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
