//! The per-entity walk: one pass over `entities`, checking each component and
//! collecting what the cross-entity passes in [`super::passes`] will need.
//!
//! The collecting is why this cannot simply check-and-forget. A wheel's chassis
//! may be authored after the wheel, a HUD element's parent after the child, so
//! anything naming another entity has to wait for every name to exist. Those
//! deferred inputs are what [`SceneFacts`] carries out.

use serde_json::Value;

use crate::codes;
use crate::components::{Collider, ColliderShapeKind, ComponentData};
use crate::error::EngineError;
use crate::mesh::BuiltinMesh;
use glam::Vec3;

use super::{check_component, kind_of, ComponentSchemas, Cx};

type HudRef = (String, &'static str, Option<String>, String);

/// What the entity walk collected for the cross-entity passes.
///
/// Every field is built here and read in exactly one pass. It is a struct
/// rather than sixteen returned values because four of them are
/// `Vec<(String, String)>` and three are `BTreeSet<String>`: passing them
/// positionally would let a swapped pair type-check and validate the wrong
/// thing. Construction is field-init shorthand and each pass destructures by
/// name, so the mapping is name-identity end to end.
pub(super) struct SceneFacts<'a> {
    pub(super) seen_names: Vec<&'a str>,
    pub(super) active_cameras: Vec<(String, String)>,
    pub(super) players: Vec<(String, crate::components::AnimationPlayer, String)>,
    pub(super) body_kinds: std::collections::HashMap<String, crate::components::BodyKind>,
    pub(super) directional_lights: Vec<(String, String)>,
    pub(super) ambient_lights: Vec<(String, String)>,
    pub(super) point_lights: Vec<(String, String)>,
    pub(super) layer_memberships: std::collections::BTreeSet<String>,
    pub(super) layer_refs: Vec<(String, String, String)>,
    pub(super) distinct_layers: Vec<(String, String)>,
    pub(super) wheels: Vec<(String, crate::components::Wheel, String)>,
    pub(super) meadows: Vec<(String, crate::components::Meadow, String)>,
    /// Roads that name a `Terrain` to follow (M40), and junctions that name
    /// roads — both are cross-entity references, so both wait for the pass
    /// where every name is known. The meadow pass's shape.
    pub(super) roads: Vec<(String, crate::components::Road, String)>,
    pub(super) junctions: Vec<(String, crate::components::Junction, String)>,
    pub(super) road_names: std::collections::BTreeSet<String>,
    pub(super) closed_road_names: std::collections::BTreeSet<String>,
    pub(super) terrain_names: std::collections::BTreeSet<String>,
    pub(super) foot_plants: Vec<(String, crate::components::FootPlant, String)>,
    pub(super) buoyancies: Vec<(String, crate::components::Buoyancy, String)>,
    pub(super) water_names: std::collections::BTreeSet<String>,
    pub(super) collider_names: std::collections::BTreeSet<String>,
    pub(super) hud_elements: Vec<HudRef>,
    pub(super) hud_panel_names: std::collections::BTreeSet<String>,
    /// Every `LightProbeVolume`, with its `spacing` and the path that declared
    /// it (M35). Collected rather than counted because the multi-volume warning
    /// has to name which one the renderer will actually draw.
    pub(super) probe_volumes: Vec<(String, String)>,
}

/// Which list is being walked (M37).
///
/// `entities` and `templates` hold the same shape — a name and a component
/// list — and take the same per-component checks, so they take the same walk.
/// What differs is the JSON pointer, the word in the top-level messages, the
/// codes those messages carry, and whether some components are forbidden. One
/// walk with a `Kind` rather than two walks, because two walks is how the
/// entity rules and the template rules start disagreeing about what a
/// `Collider` may say.
#[derive(Clone, Copy)]
pub(super) struct Kind {
    /// The JSON pointer segment and the top-level field name.
    segment: &'static str,
    /// The word in messages: "entity" or "template".
    word: &'static str,
    /// The keys an entry may carry, for the unknown-field check.
    keys: &'static [&'static str],
    not_object: &'static str,
    missing_name: &'static str,
    empty_name: &'static str,
    duplicate_name: &'static str,
    /// Components a spawn could use to violate a validated scene-level budget
    /// — empty for entities, five long for templates. See
    /// `designs/entity-spawning-design.md` §4.
    forbidden: &'static [(&'static str, &'static str)],
}

impl Kind {
    pub(super) const ENTITY: Self = Self {
        segment: "entities",
        word: "entity",
        keys: &["name", "components"],
        not_object: codes::ENTITY_NOT_OBJECT,
        missing_name: codes::MISSING_ENTITY_NAME,
        empty_name: codes::EMPTY_ENTITY_NAME,
        duplicate_name: codes::DUPLICATE_ENTITY_NAME,
        forbidden: &[],
    };

    pub(super) const TEMPLATE: Self = Self {
        segment: "templates",
        word: "template",
        keys: &["name", "limit", "components"],
        not_object: codes::TEMPLATE_NOT_OBJECT,
        missing_name: codes::MISSING_TEMPLATE_NAME,
        empty_name: codes::EMPTY_TEMPLATE_NAME,
        duplicate_name: codes::DUPLICATE_TEMPLATE_NAME,
        // Each of these is refused because spawning one could make a valid
        // scene invalid at step 40, and the point of validation is that it
        // cannot. The reason travels with the name so the error can say it.
        forbidden: &[
            (
                "Script",
                "scripts are compiled once when the run starts, so a spawned one \
                 would never run",
            ),
            (
                "Camera",
                "a scene may mark at most one camera active, and a spawn cannot be \
                 allowed to break a rule validation has already checked",
            ),
            (
                "DirectionalLight",
                "a scene may have at most one DirectionalLight",
            ),
            ("AmbientLight", "a scene may have at most one AmbientLight"),
            (
                "PointLight",
                "a scene may carry at most 8 PointLight components, and overflowing \
                 that budget drops lights silently rather than failing",
            ),
        ],
    };

    fn is_template(&self) -> bool {
        !self.forbidden.is_empty()
    }
}

/// Walk every entity — or every template (M37) — pushing per-entry errors and
/// collecting the rest.
pub(super) fn walk<'a>(
    cx: &Cx<'_>,
    schemas: &ComponentSchemas,
    entities: &'a [Value],
    kind: Kind,
    errors: &mut Vec<EngineError>,
) -> SceneFacts<'a> {
    let mut seen_names: Vec<&str> = Vec::with_capacity(entities.len());
    // Animation pass inputs, collected across all entities (M9).
    let mut players: Vec<(String, crate::components::AnimationPlayer, String)> = Vec::new();
    let mut body_kinds: std::collections::HashMap<String, crate::components::BodyKind> =
        std::collections::HashMap::new();
    // (entity name, component path) per at-most-one component, so each
    // surplus error can point at a concrete line and list candidates.
    let mut active_cameras: Vec<(String, String)> = Vec::new();
    let mut directional_lights: Vec<(String, String)> = Vec::new();
    let mut ambient_lights: Vec<(String, String)> = Vec::new();
    let mut point_lights: Vec<(String, String)> = Vec::new();
    let mut probe_volumes: Vec<(String, String)> = Vec::new();
    // Collision layers (M12): membership names declared anywhere, every
    // `collides_with` reference (for the unknown-layer warning), and each
    // distinct name with the path that introduced it (for the 32-bit budget).
    let mut layer_memberships: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    let mut layer_refs: Vec<(String, String, String)> = Vec::new();
    let mut distinct_layers: Vec<(String, String)> = Vec::new();
    // Wheel pass inputs (M12): checked cross-entity once names are known.
    let mut wheels: Vec<(String, crate::components::Wheel, String)> = Vec::new();
    // Meadow pass inputs (M29): a meadow names the Terrain it stands on, which
    // is another entity, so the check waits until every name is known — the
    // wheel pass's shape.
    let mut meadows: Vec<(String, crate::components::Meadow, String)> = Vec::new();
    // Road and junction pass inputs (M40), for the meadow pass's reason: a road
    // names the terrain it rides on and a junction names the roads that reach
    // it, and neither name can be checked until every entity has been seen.
    let mut roads: Vec<(String, crate::components::Road, String)> = Vec::new();
    let mut junctions: Vec<(String, crate::components::Junction, String)> = Vec::new();
    let mut road_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut closed_road_names: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    let mut terrain_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // Foot planting pass inputs (M32): a `FootPlant` names the Terrain its
    // feet stand on, so it waits for every name too — the meadow pass's shape,
    // for the meadow pass's reason.
    let mut foot_plants: Vec<(String, crate::components::FootPlant, String)> = Vec::new();
    // Buoyancy pass inputs (M41): a `Buoyancy` names the Water it floats on
    // and needs a body and a shape on its own entity, so it waits for every
    // name too — the meadow pass's shape a third time.
    let mut buoyancies: Vec<(String, crate::components::Buoyancy, String)> = Vec::new();
    let mut water_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut collider_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // One HUD element awaiting the cross-entity pass (M31): its owner, the
    // component that owns the `parent` reference, the reference itself, and
    // the component-s JSON path. Collected across all four kinds because
    // `parent` means the same on every one of them, so one pass checks them
    // all -- four near-identical passes is how two of them start disagreeing.
    let mut hud_elements: Vec<HudRef> = Vec::new();
    let mut hud_panel_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for (entity_index, entity) in entities.iter().enumerate() {
        let entity_path = format!("/{}/{entity_index}", kind.segment);
        let word = kind.word;

        let Some(entity) = entity.as_object() else {
            errors.push(cx.err(
                kind.not_object,
                format!(
                    "{word} at index {entity_index} must be an object, found {}",
                    kind_of(entity)
                ),
                &entity_path,
            ));
            continue;
        };

        // The entity name is load-bearing for every later error message, so
        // resolve it before anything else.
        let name = match entity.get("name") {
            Some(Value::String(name)) if !name.is_empty() => name.as_str(),
            Some(Value::String(_)) => {
                errors.push(
                    cx.err(
                        kind.empty_name,
                        format!("{word} at index {entity_index} has an empty name"),
                        &format!("{entity_path}/name"),
                    )
                    .field("name"),
                );
                continue;
            }
            Some(other) => {
                errors.push(
                    cx.wrong_type("name", "string", other, &format!("{entity_path}/name"))
                        .entity(format!("<{word} at index {entity_index}>")),
                );
                continue;
            }
            None => {
                errors.push(
                    cx.err(
                        kind.missing_name,
                        format!(
                            "{word} at index {entity_index} has no name; \
                             names are how the CLI and agent edits target entities"
                        ),
                        &entity_path,
                    )
                    .field("name"),
                );
                continue;
            }
        };

        if seen_names.contains(&name) {
            errors.push(
                cx.err(
                    kind.duplicate_name,
                    format!(
                        "more than one {word} is named {name:?}; names must be unique \
                         because they are how entities are targeted"
                    ),
                    &format!("{entity_path}/name"),
                )
                .entity(name),
            );
        }
        seen_names.push(name);

        for key in entity.keys() {
            if !kind.keys.contains(&key.as_str()) {
                errors.push(
                    cx.err(
                        codes::UNKNOWN_FIELD,
                        format!("unknown {word} field {key:?}"),
                        &format!("{entity_path}/{key}"),
                    )
                    .entity(name)
                    .field(key)
                    .suggest_from(key, kind.keys.iter().copied()),
                );
            }
        }

        // A template's `limit` is the one field an entity does not have, and
        // it is checked here rather than by the component schema walk because
        // it is not a component. `>= 1`: a limit of zero is a template that
        // can never spawn, which is a mistake with no legitimate reading.
        if kind.is_template() {
            match entity.get("limit") {
                None => {}
                Some(Value::Number(n)) if n.is_u64() && n.as_u64() != Some(0) => {}
                Some(Value::Number(n)) if n.is_u64() => errors.push(
                    cx.err(
                        codes::VALUE_OUT_OF_RANGE,
                        format!(
                            "template {name:?} has a limit of 0, so nothing could ever \
                             spawn from it; the smallest useful limit is 1"
                        ),
                        &format!("{entity_path}/limit"),
                    )
                    .entity(name)
                    .field("limit"),
                ),
                Some(other) => errors.push(
                    cx.wrong_type(
                        "limit",
                        "non-negative integer",
                        other,
                        &format!("{entity_path}/limit"),
                    )
                    .entity(name),
                ),
            }
        }

        let components = match entity.get("components") {
            None => continue,
            Some(Value::Array(components)) => components,
            Some(other) => {
                errors.push(
                    cx.wrong_type(
                        "components",
                        "array",
                        other,
                        &format!("{entity_path}/components"),
                    )
                    .entity(name),
                );
                continue;
            }
        };

        let mut seen_types: Vec<String> = Vec::new();
        let mut has_mesh = false;
        let mut mesh_path: Option<String> = None;
        // The `Mesh.asset` string, kept beside the path so a skeletal player
        // can be checked against the file its skin has to live in (M30).
        let mut mesh_asset: Option<String> = None;
        // A skeletal `AnimationPlayer`: (glTF asset, clip name, JSON path).
        let mut skeletal_player: Option<(String, String, String)> = None;
        // A stride-driven `AnimationPlayer`'s JSON path (M32): the clock is
        // then the entity's own displacement, so it needs somewhere to move.
        let mut stride_player: Option<String> = None;
        // A `FootPlant`'s JSON path (M32), for the checks that need to know
        // what else is on this entity.
        let mut foot_plant: Option<String> = None;
        // A `SkinnedCollider` (M33): its proxies ride the entity's rig, so the
        // checks need the entity's `Mesh` and its scale.
        let mut skinned_collider: Option<(crate::components::SkinnedCollider, String)> = None;
        // A `Ragdoll` (M39): its bodies *are* the proxies, so every check it
        // has needs the `SkinnedCollider` beside it.
        let mut ragdoll: Option<(crate::components::Ragdoll, String)> = None;
        let mut tree_path: Option<String> = None;
        let mut shard_path: Option<String> = None;
        let mut cloud_path: Option<String> = None;
        let mut material_paths: Vec<String> = Vec::new();
        let mut has_transform = false;
        let mut scale = glam::Vec3::ONE;
        let mut rotation = glam::Vec3::ZERO;
        // Only the terrain-basin check reads this, and it reads it in world XZ:
        // a basin is authored in world space, so the question "does it land on
        // this patch" is asked about the patch's world footprint (M42).
        let mut position = glam::Vec3::ZERO;
        let mut rigid_body: Option<(crate::components::BodyKind, String)> = None;
        let mut collider: Option<(crate::components::Collider, String)> = None;
        let mut wheel_path: Option<String> = None;
        let mut breakable_threshold: Option<String> = None;
        let mut water: Option<(crate::components::Water, String)> = None;
        let mut terrain: Option<(crate::components::Terrain, String)> = None;
        let mut road: Option<(crate::components::Road, String)> = None;
        let mut junction: Option<(crate::components::Junction, String)> = None;
        let mut meadow: Option<(crate::components::Meadow, String)> = None;
        let mut light_probe_volume: Option<(crate::components::LightProbeVolume, String)> = None;
        // Whether this entity carries anything a `HudInteract` could use as
        // its hit box (M31).
        let mut has_hud_element = false;
        let mut hud_interact_path: Option<String> = None;
        let mut hud_image: Option<(crate::components::HudImage, String)> = None;

        for (component_index, component) in components.iter().enumerate() {
            let component_path = format!("{entity_path}/components/{component_index}");
            let checked = check_component(cx, schemas, component, name, &component_path, errors);

            let Some(type_name) = checked.type_name else {
                continue;
            };

            if seen_types.contains(&type_name) {
                // An error, not a warning: hecs keeps only the last, so the
                // file and the world would disagree — hidden state, which
                // invariant 2 exists to prevent. Points at the surplus copy.
                errors.push(
                    cx.err(
                        codes::DUPLICATE_COMPONENT,
                        format!(
                            "entity {name:?} has more than one {type_name} component; \
                             only one would survive, so the file and the world would disagree"
                        ),
                        &component_path,
                    )
                    .entity(name)
                    .component(&type_name),
                );
                continue;
            }
            seen_types.push(type_name.clone());

            // §4 of the spawning design: a template may not carry a component
            // whose scene-level budget a spawn could then violate. Refused at
            // validation rather than at the spawn, because "this scene is
            // valid" has to keep meaning something after step 0.
            if let Some((_, why)) = kind.forbidden.iter().find(|(c, _)| *c == type_name) {
                errors.push(
                    cx.err(
                        codes::TEMPLATE_FORBIDDEN_COMPONENT,
                        format!(
                            "template {name:?} carries a {type_name}, which a template \
                             may not: {why}"
                        ),
                        &component_path,
                    )
                    .entity(name)
                    .component(&type_name),
                );
                continue;
            }

            if checked.active_camera {
                active_cameras.push((name.to_string(), component_path.clone()));
            }
            if checked.directional_light {
                directional_lights.push((name.to_string(), component_path.clone()));
            }
            if checked.ambient_light {
                ambient_lights.push((name.to_string(), component_path.clone()));
            }
            if checked.point_light {
                point_lights.push((name.to_string(), component_path.clone()));
            }
            if type_name == "Mesh" {
                has_mesh = true;
                mesh_path = Some(component_path.clone());
            }
            if type_name == "Tree" {
                tree_path = Some(component_path.clone());
            }
            if type_name == "Shard" {
                shard_path = Some(component_path.clone());
            }
            if type_name == "Material" {
                material_paths.push(component_path.clone());
            }
            match checked.parsed {
                Some(ComponentData::Transform(t)) => {
                    has_transform = true;
                    scale = t.scale;
                    rotation = t.rotation;
                    position = t.position;
                }
                Some(ComponentData::RigidBody(rb)) => {
                    body_kinds.insert(name.to_string(), rb.body);
                    rigid_body = Some((rb.body, component_path));
                }
                Some(ComponentData::AnimationPlayer(player)) => {
                    if let crate::skeleton::ClipRef::Skeletal { asset, clip } =
                        crate::skeleton::ClipRef::parse(&player.clip)
                    {
                        skeletal_player =
                            Some((asset.to_string(), clip.to_string(), component_path.clone()));
                    }
                    if player.stride > 0.0 {
                        stride_player = Some(component_path.clone());
                    }
                    players.push((name.to_string(), player, component_path));
                }
                Some(ComponentData::Mesh(mesh)) => {
                    mesh_asset = Some(mesh.asset.clone());
                }
                Some(ComponentData::Collider(c)) => {
                    collider_names.insert(name.to_string());
                    collider = Some((c, component_path));
                }
                Some(ComponentData::Wheel(w)) => {
                    wheel_path = Some(component_path.clone());
                    wheels.push((name.to_string(), w, component_path));
                }
                Some(ComponentData::Breakable(b)) => {
                    if b.impulse_threshold.is_some() {
                        breakable_threshold = Some(component_path);
                    }
                }
                Some(ComponentData::Water(w)) => {
                    water_names.insert(name.to_string());
                    water = Some((w, component_path));
                }
                Some(ComponentData::Cloud(_)) => {
                    cloud_path = Some(component_path);
                }
                Some(ComponentData::Terrain(t)) => {
                    terrain_names.insert(name.to_string());
                    terrain = Some((t, component_path));
                }
                Some(ComponentData::Road(r)) => {
                    road_names.insert(name.to_string());
                    if r.closed {
                        closed_road_names.insert(name.to_string());
                    }
                    roads.push((name.to_string(), r.clone(), component_path.clone()));
                    road = Some((r, component_path));
                }
                Some(ComponentData::Junction(j)) => {
                    junctions.push((name.to_string(), j.clone(), component_path.clone()));
                    junction = Some((j, component_path));
                }
                Some(ComponentData::Meadow(m)) => {
                    meadows.push((name.to_string(), m.clone(), component_path.clone()));
                    meadow = Some((m, component_path));
                }
                Some(ComponentData::LightProbeVolume(v)) => {
                    probe_volumes.push((name.to_string(), component_path.clone()));
                    light_probe_volume = Some((v, component_path));
                }
                Some(ComponentData::FootPlant(p)) => {
                    foot_plant = Some(component_path.clone());
                    foot_plants.push((name.to_string(), p, component_path));
                }
                Some(ComponentData::Buoyancy(b)) => {
                    buoyancies.push((name.to_string(), b, component_path));
                }
                Some(ComponentData::SkinnedCollider(s)) => {
                    skinned_collider = Some((s, component_path));
                }
                Some(ComponentData::Ragdoll(r)) => {
                    ragdoll = Some((r, component_path));
                }
                Some(ComponentData::HudPanel(p)) => {
                    has_hud_element = true;
                    hud_panel_names.insert(name.to_string());
                    hud_elements.push((name.to_string(), "HudPanel", p.parent, component_path));
                }
                Some(ComponentData::HudRect(r)) => {
                    has_hud_element = true;
                    hud_elements.push((name.to_string(), "HudRect", r.parent, component_path));
                }
                Some(ComponentData::HudImage(i)) => {
                    has_hud_element = true;
                    hud_elements.push((
                        name.to_string(),
                        "HudImage",
                        i.parent.clone(),
                        component_path.clone(),
                    ));
                    hud_image = Some((i, component_path));
                }
                Some(ComponentData::HudText(t)) => {
                    has_hud_element = true;
                    hud_elements.push((name.to_string(), "HudText", t.parent, component_path));
                }
                Some(ComponentData::HudInteract(_)) => {
                    hud_interact_path = Some(component_path);
                }
                _ => {}
            }
        }

        // ── Skeletal ownership (M30) ──────────────────────────────────
        //
        // The skin lives inside the mesh file, so a player pointing anywhere
        // else is describing a rig that will never be applied. Checking the
        // reference rather than the file keeps this in engine-core, where no
        // glTF can be opened; `mesh_has_no_skin` is the asset pass's half.
        if let Some((asset, _, player_path)) = &skeletal_player {
            match mesh_asset.as_deref() {
                Some(mesh) if mesh == asset => {}
                Some(mesh) => {
                    errors.push(
                        cx.err(
                            codes::SKELETAL_PLAYER_MESH_MISMATCH,
                            format!(
                                "entity {name:?} plays a skeletal clip from {asset:?} but \
                                 its Mesh is {mesh:?}; the skin lives in the mesh file, \
                                 so the rig would never be applied"
                            ),
                            player_path,
                        )
                        .entity(name)
                        .component("AnimationPlayer")
                        .field("clip"),
                    );
                }
                None => {
                    errors.push(
                        cx.err(
                            codes::SKELETAL_PLAYER_MESH_MISMATCH,
                            format!(
                                "entity {name:?} plays a skeletal clip from {asset:?} but \
                                 has no Mesh; a skeletal player belongs on the entity \
                                 whose Mesh owns the skin"
                            ),
                            player_path,
                        )
                        .entity(name)
                        .component("AnimationPlayer")
                        .field("clip"),
                    );
                }
            }
        }

        // ── Locomotion ownership (M32) ────────────────────────────────
        //
        // A stride-driven clip's clock *is* the entity's horizontal
        // displacement, so an entity with no Transform has no clock at all and
        // its clip would stand frozen at phase 0 forever — a stillness that
        // looks exactly like a missing clip and is very hard to place.
        if let Some(path) = &stride_player {
            if !has_transform {
                errors.push(
                    cx.err(
                        codes::ANIMATION_STRIDE_WITHOUT_TRANSFORM,
                        format!(
                            "entity {name:?} sets AnimationPlayer.stride but has no Transform; \
                             a stride-driven clip is advanced by how far the entity moves, \
                             so with nothing to move it would never leave phase 0"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("AnimationPlayer")
                    .field("stride"),
                );
            }
        }

        // ── Foot planting ownership (M32) ─────────────────────────────
        //
        // Whether the mesh's file actually carries a *skin* is engine-assets'
        // half (`foot_plant_without_skin`), the M30 division: engine-core
        // checks the reference, the asset pass opens the file.
        if let Some(path) = &foot_plant {
            if mesh_asset.is_none() {
                errors.push(
                    cx.err(
                        codes::FOOT_PLANT_WITHOUT_SKIN,
                        format!(
                            "entity {name:?} has a FootPlant but no Mesh; the joints it \
                             plants live in a skinned mesh file, so a FootPlant belongs \
                             on the entity whose Mesh owns the skin"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("FootPlant"),
                );
            }
            // The solve runs in skin space and maps the target back through
            // the model's inverse. A non-uniform scale would arrive there as a
            // stretch on every bone length, so the leg would solve to the
            // wrong bend — refused rather than subtly wrong.
            let uniform = (scale.x - scale.y).abs() < 1e-4 && (scale.y - scale.z).abs() < 1e-4;
            if !uniform {
                errors.push(
                    cx.err(
                        codes::FOOT_PLANT_NON_UNIFORM_SCALE,
                        format!(
                            "entity {name:?} has a FootPlant and a non-uniform \
                             Transform.scale [{}, {}, {}]; the IK solves in the skin's \
                             own space, where a non-uniform scale is a different bone \
                             length along every axis",
                            scale.x, scale.y, scale.z
                        ),
                        path,
                    )
                    .entity(name)
                    .component("FootPlant"),
                );
            }
        }

        // ── Cross-component physics checks (design §9) ────────────────
        if let Some((body, path)) = &rigid_body {
            if !has_transform {
                errors.push(
                    cx.err(
                        codes::MISSING_TRANSFORM,
                        format!("entity {name:?} has a RigidBody but no Transform to move"),
                        path,
                    )
                    .entity(name)
                    .component("RigidBody"),
                );
            }
            if *body == crate::components::BodyKind::Dynamic && collider.is_none() {
                // An error, not a warning: it would fall forever through
                // everything, which is a mistake essentially always.
                errors.push(
                    cx.err(
                        codes::MISSING_COLLIDER,
                        format!(
                            "entity {name:?} has a dynamic RigidBody but no Collider; \
                             it would fall forever through everything"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("RigidBody"),
                );
            }
        }
        if let Some((collider_data, path)) = &collider {
            if !has_transform && rigid_body.is_none() {
                errors.push(
                    cx.err(
                        codes::MISSING_TRANSFORM,
                        format!("entity {name:?} has a Collider but no Transform to place it"),
                        path,
                    )
                    .entity(name)
                    .component("Collider"),
                );
            }
            let round = matches!(
                collider_data.shape,
                crate::components::ColliderShapeKind::Sphere
                    | crate::components::ColliderShapeKind::Capsule
            );
            if round && !(scale.x == scale.y && scale.y == scale.z) {
                errors.push(
                    cx.err(
                        codes::NONUNIFORM_SCALE_ON_ROUND_COLLIDER,
                        format!(
                            "entity {name:?} scales a round collider by [{}, {}, {}]; \
                             spheres and capsules only take uniform scale",
                            scale.x, scale.y, scale.z
                        ),
                        path,
                    )
                    .entity(name)
                    .component("Collider")
                    .field("shape"),
                );
            }

            // Mesh shapes borrow the entity's own Mesh when `asset` is
            // absent; neither present means there is no geometry to collide.
            let mesh_shape = matches!(
                collider_data.shape,
                crate::components::ColliderShapeKind::Trimesh
                    | crate::components::ColliderShapeKind::ConvexHull
            );
            // Terrain (M22) and Road (M23) both count as geometry to borrow: a
            // `trimesh` collider on either, with no asset, takes the surface the
            // component generates. That is how ground and asphalt are collidable
            // without a mesh file duplicating what the renderer already draws —
            // and for a road it is the whole point, since the surface driven and
            // the surface drawn then cannot be authored apart.
            // A `Shard` (M43) is the third: a `convex_hull` collider on one,
            // with no asset, collides with the hull the shard draws — which is
            // what makes a broken piece's drawn shape and collided shape
            // impossible to author apart.
            if mesh_shape
                && collider_data.asset.is_none()
                && !has_mesh
                && terrain.is_none()
                && road.is_none()
                && junction.is_none()
                && shard_path.is_none()
            {
                errors.push(
                    cx.err(
                        codes::COLLIDER_MISSING_MESH,
                        format!(
                            "entity {name:?} has a mesh-shaped Collider but no \"asset\" \
                             field and no Mesh component to borrow geometry from"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("Collider")
                    .field("asset"),
                );
            }
            // A dynamic trimesh has no well-defined interior, so rapier
            // cannot give it mass — the deterministic answer is an error
            // naming the working alternative (the animation_on_dynamic_body
            // precedent).
            if collider_data.shape == crate::components::ColliderShapeKind::Trimesh
                && rigid_body
                    .as_ref()
                    .is_some_and(|(kind, _)| *kind == crate::components::BodyKind::Dynamic)
            {
                errors.push(
                    cx.err(
                        codes::TRIMESH_ON_DYNAMIC_BODY,
                        format!(
                            "entity {name:?} puts a trimesh Collider on a dynamic \
                             RigidBody; a trimesh has no mass — use shape \
                             \"convex_hull\", or make the body fixed or kinematic"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("Collider")
                    .field("shape"),
                );
            }

            // A builtin primitive is one metre across at scale 1, so both the
            // drawn size and the simulated size are readable straight out of
            // the file — and nothing else reports it when they disagree. The
            // symptom is a ball resting half-buried in the floor, or a crate
            // whose corner is hit before it is reached, and both look like
            // engine bugs.
            if let Some(Ok(builtin)) = mesh_asset.as_deref().and_then(BuiltinMesh::parse) {
                errors.extend(check_collider_matches_mesh(
                    cx,
                    name,
                    builtin,
                    scale,
                    collider_data,
                    path,
                ));
            }

            // Collect layer names for the scene-level checks below.
            let mut note_distinct = |layer: &str, path: &str| {
                if !distinct_layers.iter().any(|(l, _)| l == layer) {
                    distinct_layers.push((layer.to_string(), path.to_string()));
                }
            };
            for layer in collider_data.layers.iter().flatten() {
                layer_memberships.insert(layer.clone());
                note_distinct(layer, path);
            }
            for layer in collider_data.collides_with.iter().flatten() {
                layer_refs.push((layer.clone(), name.to_string(), path.clone()));
                note_distinct(layer, path);
            }
        }

        // ── Skinned collider proxies (M33) ────────────────────────────
        //
        // A proxy rides a joint, and the joints live in the mesh file — so a
        // component on an entity with no `Mesh` describes a rig that will
        // never exist. Whether the file it names carries a *skin* is
        // engine-assets' half, the M30/M32 division.
        if let Some((proxies, path)) = &skinned_collider {
            if mesh_asset.is_none() {
                errors.push(
                    cx.err(
                        codes::SKINNED_COLLIDER_WITHOUT_SKIN,
                        format!(
                            "entity {name:?} has a SkinnedCollider but no Mesh; its \
                             proxies ride joints, and the joints live in a skinned \
                             mesh file"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("SkinnedCollider"),
                );
            }
            // `FootPlant`'s reason, arriving at the same refusal from the
            // other side: a sphere through a non-uniform scale is not a
            // sphere, and rapier has no shape for what it becomes.
            let uniform = (scale.x - scale.y).abs() < 1e-4 && (scale.y - scale.z).abs() < 1e-4;
            if !uniform {
                errors.push(
                    cx.err(
                        codes::SKINNED_COLLIDER_NON_UNIFORM_SCALE,
                        format!(
                            "entity {name:?} has a SkinnedCollider and a non-uniform \
                             Transform.scale [{}, {}, {}]; a proxy is scaled by it, and \
                             a non-uniformly scaled sphere or capsule is not a shape \
                             rapier has",
                            scale.x, scale.y, scale.z
                        ),
                        path,
                    )
                    .entity(name)
                    .component("SkinnedCollider"),
                );
            }

            // The layer budget and the reference check are M12's, and they
            // count every collider in the scene — proxies included, or a
            // character's own layer names would be invisible to both.
            let mut note_distinct = |layer: &str, path: &str| {
                if !distinct_layers.iter().any(|(l, _)| l == layer) {
                    distinct_layers.push((layer.to_string(), path.to_string()));
                }
            };
            for layer in proxies.layers.iter().flatten() {
                layer_memberships.insert(layer.clone());
                note_distinct(layer, path);
            }
            for layer in proxies.collides_with.iter().flatten() {
                layer_refs.push((layer.clone(), name.to_string(), path.clone()));
                note_distinct(layer, path);
            }
        }

        // ── Ragdolls (M39) ────────────────────────────────────────────
        //
        // The bodies are the proxies (design §4), so a `Ragdoll` without a
        // `SkinnedCollider` has nothing to fall. Whether the parts form one
        // tree needs the rig's ancestry and is engine-assets' half, beside the
        // joint-name checks — the M30/M32 division, for the third time.
        if let Some((ragdoll, path)) = &ragdoll {
            match skinned_collider.as_ref() {
                None => {
                    errors.push(
                        cx.err(
                            codes::RAGDOLL_WITHOUT_PROXIES,
                            format!(
                                "entity {name:?} has a Ragdoll but no SkinnedCollider; a \
                                 ragdoll's bodies *are* its proxies, so the hitbox that \
                                 was shot is the body that falls, and a character with no \
                                 hitboxes has nothing to fall"
                            ),
                            path,
                        )
                        .entity(name)
                        .component("Ragdoll"),
                    );
                }
                Some((proxies, _)) => {
                    // An override for a joint no part rides constrains nothing,
                    // and reads in the file as a knee that bends when it does
                    // not — the failure mode a typo deserves a name for.
                    let ridden: Vec<&str> =
                        proxies.parts.iter().map(|p| p.joint.as_str()).collect();
                    for (i, joint) in ragdoll.joints.iter().enumerate() {
                        if ridden.contains(&joint.joint.as_str()) {
                            continue;
                        }
                        errors.push(
                            cx.err(
                                codes::RAGDOLL_UNKNOWN_JOINT,
                                format!(
                                    "the Ragdoll override for {:?} names a joint no part \
                                     of this SkinnedCollider rides, so it would constrain \
                                     nothing",
                                    joint.joint
                                ),
                                &format!("{path}/joints/{i}/joint"),
                            )
                            .entity(name)
                            .component("Ragdoll")
                            .field(format!("joints/{i}/joint"))
                            .suggest_from(&joint.joint, ridden.iter().copied()),
                        );
                    }
                }
            }
        }

        // ── Wheel entity checks (M12) ─────────────────────────────────
        if let Some(path) = &wheel_path {
            if !has_transform {
                errors.push(
                    cx.err(
                        codes::MISSING_TRANSFORM,
                        format!(
                            "entity {name:?} has a Wheel but no Transform; \
                             physics writes the wheel's pose into it every step"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("Wheel"),
                );
            }
            if rigid_body.is_some() || collider.is_some() {
                errors.push(
                    cx.err(
                        codes::WHEEL_WITH_PHYSICS,
                        format!(
                            "entity {name:?} has a Wheel and its own RigidBody or \
                             Collider; the chassis owns all collision — the wheel \
                             touches the road through its suspension ray"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("Wheel"),
                );
            }
        }

        // A collision can only break what a collider can hit: a threshold
        // with nothing to receive the impact is dead data. Script- or
        // explosion-only breakables just omit the threshold.
        if let Some(path) = &breakable_threshold {
            if collider.is_none() {
                errors.push(
                    cx.err(
                        codes::BREAKABLE_WITHOUT_COLLIDER,
                        format!(
                            "entity {name:?} sets Breakable.impulse_threshold but has no \
                             Collider; no collision can ever reach the threshold — add a \
                             Collider or drop the threshold"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("Breakable")
                    .field("impulse_threshold"),
                );
            }
        }

        // ── Tree entity checks (M19) ──────────────────────────────────
        //
        // A `Tree` *is* the entity's geometry. Carrying a `Mesh` as well would
        // draw both at one transform, which is never what an author means and
        // has no defined winner — the same reasoning as `wheel_with_physics`.
        if let (Some(path), true) = (&tree_path, has_mesh) {
            errors.push(
                cx.err(
                    codes::TREE_WITH_MESH,
                    format!(
                        "entity {name:?} has both a Tree and a Mesh; a Tree generates \
                         the entity's geometry, so the two would draw on top of each \
                         other — split them into two entities"
                    ),
                    mesh_path.as_deref().unwrap_or(path),
                )
                .entity(name)
                .component("Tree"),
            );
        }

        // ── Shard entity checks (M43) ─────────────────────────────────
        //
        // A `Shard` *is* the entity's geometry, for `tree_with_mesh`'s reason:
        // two geometries at one transform draw on top of each other and
        // nothing says which one the author meant. Its `Material` is fine —
        // that is the shard's surface, the same exception a Tree's bark is.
        if let (Some(path), true) = (&shard_path, has_mesh) {
            errors.push(
                cx.err(
                    codes::SHARD_WITH_MESH,
                    format!(
                        "entity {name:?} has both a Shard and a Mesh; a Shard is the \
                         convex hull of its own points, so the two would draw on top \
                         of each other — split them into two entities"
                    ),
                    mesh_path.as_deref().unwrap_or(path),
                )
                .entity(name)
                .component("Shard"),
            );
        }

        // ── Cloud entity checks (M20) ─────────────────────────────────
        //
        // A `Cloud` grows the entity's geometry and shades it with its own
        // fields, so a `Mesh` or a `Material` beside it is a second, silently
        // ignored answer to what this entity is — `water_with_mesh`'s reasoning,
        // and an error for the same reason: the two authorings look identical in
        // the file and nothing in the render says which one lost.
        if let Some(path) = &cloud_path {
            if has_mesh || !material_paths.is_empty() {
                let extras = match (has_mesh, material_paths.is_empty()) {
                    (true, false) => "a Mesh and a Material",
                    (true, true) => "a Mesh",
                    _ => "a Material",
                };
                errors.push(
                    cx.err(
                        codes::CLOUD_WITH_MESH,
                        format!(
                            "entity {name:?} has a Cloud component and also {extras}; \
                             a Cloud grows its own geometry (sized by Transform.scale) \
                             and carries its own colours — drop the extra component"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("Cloud"),
                );
            }
        }

        // ── Road surface checks (M23) ─────────────────────────────────
        //
        // Same rule as water and cloud: a `Road` generates its own ribbon and
        // carries its own colours, so a `Mesh` or `Material` beside it is a
        // second, silently ignored answer to what this surface is.
        //
        // (A road with a collider and no Transform is a road the car falls
        // through — already `missing_transform` from the collider check above.
        // One problem, one error.)
        if let Some((_, path)) = &road {
            if has_mesh || !material_paths.is_empty() {
                let extras = match (has_mesh, material_paths.is_empty()) {
                    (true, false) => "a Mesh and a Material",
                    (true, true) => "a Mesh",
                    _ => "a Material",
                };
                errors.push(
                    cx.err(
                        codes::ROAD_WITH_MESH,
                        format!(
                            "entity {name:?} has a Road component and also {extras}; \
                             a road generates its own surface from its centerline and \
                             carries its own colours — drop the extra component"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("Road"),
                );
            }
        }

        // A road that follows a terrain samples the ground in world space and
        // brings the answer back into its own local space (M40). That mapping
        // is exact for the translation, scale and yaw a road is actually placed
        // with, and *not* for a roll or a pitch — local `y` stops being world
        // "up", and the road comes out skewed against the ground it is meant to
        // be lying on. Rare, silent, and impossible to diagnose from the render.
        if let Some((road, path)) = &road {
            if road.follow_terrain.is_some() && (rotation.x != 0.0 || rotation.z != 0.0) {
                errors.push(
                    cx.err(
                        codes::ROAD_FOLLOW_ROTATED,
                        format!(
                            "the Road on {name:?} follows a Terrain, but its Transform \
                             rotates {:.1}° about X and {:.1}° about Z; ground heights \
                             are sampled in world space and brought back into the \
                             road's local space, which only lines up when the road is \
                             level — place it with a yaw and a translation, and put the \
                             tilt in the terrain",
                            rotation.x, rotation.z
                        ),
                        path,
                    )
                    .entity(name)
                    .component("Road")
                    .field("follow_terrain")
                    .warning(),
                );
            }
        }

        // ── Junction surface checks (M40) ─────────────────────────────
        //
        // The road rule, one primitive over: a `Junction` generates its own
        // patch and carries its own colours, so a `Mesh` or `Material` beside
        // it is a second, silently ignored answer to what this surface is.
        if let Some((_, path)) = &junction {
            if has_mesh || !material_paths.is_empty() {
                let extras = match (has_mesh, material_paths.is_empty()) {
                    (true, false) => "a Mesh and a Material",
                    (true, true) => "a Mesh",
                    _ => "a Material",
                };
                errors.push(
                    cx.err(
                        codes::JUNCTION_WITH_MESH,
                        format!(
                            "entity {name:?} has a Junction component and also {extras}; \
                             a junction generates its own patch from the roads that reach \
                             it and carries its own colours — drop the extra component"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("Junction"),
                );
            }
        }

        // ── Water surface checks (M18) ────────────────────────────────
        if let Some((water, path)) = &water {
            // A `Water` entity generates its own grid and shades it with its
            // own fields, so a `Mesh` or `Material` beside it is a second,
            // silently ignored answer to what this surface is — invariant 2's
            // hidden state in miniature. An error rather than a warning
            // because the two authorings look identical in the file and
            // nothing in the render tells you which one lost.
            if has_mesh || !material_paths.is_empty() {
                let extras = match (has_mesh, material_paths.is_empty()) {
                    (true, false) => "a Mesh and a Material",
                    (true, true) => "a Mesh",
                    _ => "a Material",
                };
                errors.push(
                    cx.err(
                        codes::WATER_WITH_MESH,
                        format!(
                            "entity {name:?} has a Water component and also {extras}; \
                             water generates its own surface (sized by Transform.scale) \
                             and carries its own colours — drop the extra component"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("Water"),
                );
            }

            // The Gerstner constraint. Past a total steepness of 1 the surface
            // folds through itself: crests curl into loops, normals invert, and
            // the render looks like a shader bug rather than like the number
            // being slightly too high. Refusing is the `make_car_track.py`
            // move — check what the formula cannot represent, and say so with
            // the arithmetic in the message.
            let total: f32 = water.waves.iter().map(|w| w.steepness).sum();
            if total > 1.0 {
                let parts: Vec<String> = water
                    .waves
                    .iter()
                    .map(|w| format!("{}", w.steepness))
                    .collect();
                errors.push(
                    cx.err(
                        codes::WATER_WAVES_SELF_INTERSECT,
                        format!(
                            "entity {name:?} sums wave steepness to {total} ({}); \
                             at more than 1 the surface folds through itself and the \
                             crests curl into loops — scale the steepness values down",
                            parts.join(" + ")
                        ),
                        path,
                    )
                    .entity(name)
                    .component("Water")
                    .field("waves"),
                );
            }
        }

        // ── Terrain checks (M22) ─────────────────────────────────────────
        if let Some((terrain, path)) = &terrain {
            // Same rule as water, for the same reason: the patch generates its
            // own surface and paints it from its own layers, so a Mesh or a
            // Material beside it is a second, silently ignored answer to what
            // this ground is.
            if has_mesh || !material_paths.is_empty() {
                let extras = match (has_mesh, material_paths.is_empty()) {
                    (true, false) => "a Mesh and a Material",
                    (true, true) => "a Mesh",
                    _ => "a Material",
                };
                errors.push(
                    cx.err(
                        codes::TERRAIN_WITH_MESH,
                        format!(
                            "entity {name:?} has a Terrain component and also {extras}; \
                             terrain generates its own surface (sized by Transform.scale) \
                             and paints it from its layers — drop the extra component"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("Terrain"),
                );
            }

            // A backwards band is silent otherwise: the layer's weight is zero
            // everywhere, so it simply never appears and the author is left
            // looking at the shader for a missing material that was never asked
            // for. Cheaper to refuse, with the numbers in the message.
            for (index, layer) in terrain.layers.iter().enumerate() {
                let mut inverted = |field: &str, range: [f32; 2], unit: &str| {
                    errors.push(
                        cx.err(
                            codes::TERRAIN_LAYER_RANGE_INVERTED,
                            format!(
                                "entity {name:?} layer {index} has {field} \
                                 [{}, {}]{unit}, which runs backwards and so covers \
                                 nothing; swap the two values",
                                range[0], range[1]
                            ),
                            &format!("{path}/layers/{index}/{field}"),
                        )
                        .entity(name)
                        .component("Terrain")
                        .field(field),
                    );
                };

                if layer.height_range[0] > layer.height_range[1] {
                    inverted("height_range", layer.height_range, " m");
                }
                if layer.slope_range[0] > layer.slope_range[1] {
                    inverted("slope_range", layer.slope_range, "°");
                }
            }

            // ── Basins (M42) ─────────────────────────────────────────────
            //
            // Both of these are warnings rather than errors: a basin that cuts
            // nothing is legal, and a basin that misses this patch is legal too
            // — M22 promises world-space sampling precisely so that one terrain
            // description can be shared across several patches, and a shared
            // description names basins some of its patches do not contain.
            // What is *not* fine is either happening silently, because the
            // symptom in both cases is identical: the ground is exactly what it
            // was and nothing says why.
            let centre = glam::Vec2::new(position.x, position.z);
            let half = (glam::Vec2::new(scale.x, scale.z) * 0.5).abs();
            let (patch_min, patch_max) = (centre - half, centre + half);

            for (index, basin) in terrain.basins.iter().enumerate() {
                let path = |field: &str| format!("{path}/basins/{index}/{field}");
                let footprint = basin.radius + basin.falloff;

                if basin.depth == 0.0 || footprint == 0.0 {
                    let why = if basin.depth == 0.0 {
                        "depth 0".to_string()
                    } else {
                        "radius 0 and falloff 0, so no footprint".to_string()
                    };
                    errors.push(
                        cx.err(
                            codes::TERRAIN_BASIN_NO_EFFECT,
                            format!(
                                "entity {name:?} basin {index} has {why}, so it cuts \
                                 nothing and the ground there is the plain noise"
                            ),
                            &path(if basin.depth == 0.0 {
                                "depth"
                            } else {
                                "radius"
                            }),
                        )
                        .entity(name)
                        .component("Terrain")
                        .field("basins")
                        .warning(),
                    );
                    continue;
                }

                // Rectangle against rectangle rather than circle against
                // rectangle: the answer only has to be right about *missing
                // entirely*, and the bounding box of a basin that clips a
                // corner is the case where the two disagree — a warning that
                // fires on a basin which does reach the patch is worse than one
                // that stays quiet on a basin which barely does not.
                let (basin_min, basin_max) = (
                    glam::Vec2::new(basin.center[0], basin.center[1]) - footprint,
                    glam::Vec2::new(basin.center[0], basin.center[1]) + footprint,
                );
                let overlaps = basin_min.x <= patch_max.x
                    && basin_max.x >= patch_min.x
                    && basin_min.y <= patch_max.y
                    && basin_max.y >= patch_min.y;

                if !overlaps {
                    errors.push(
                        cx.err(
                            codes::TERRAIN_BASIN_OUTSIDE_PATCH,
                            format!(
                                "entity {name:?} basin {index} is centred at \
                                 ({}, {}) with a footprint of {footprint} m, which \
                                 misses the patch's own extent \
                                 x [{}, {}], z [{}, {}] — basin centres are in \
                                 **world** XZ, like every other terrain sample, not \
                                 in the patch's local space",
                                basin.center[0],
                                basin.center[1],
                                patch_min.x,
                                patch_max.x,
                                patch_min.y,
                                patch_max.y
                            ),
                            &path("center"),
                        )
                        .entity(name)
                        .component("Terrain")
                        .field("basins")
                        .warning(),
                    );
                }
            }
        }

        // ── Meadow checks (M29) ──────────────────────────────────────────
        if let Some((meadow, path)) = &meadow {
            // Same rule as every other recipe: the component grows the entity's
            // geometry and carries its own colours, so a Mesh or Material beside
            // it is a second, silently ignored answer to what this entity is.
            if has_mesh || !material_paths.is_empty() {
                let extras = match (has_mesh, material_paths.is_empty()) {
                    (true, false) => "a Mesh and a Material",
                    (true, true) => "a Mesh",
                    _ => "a Material",
                };
                errors.push(
                    cx.err(
                        codes::MEADOW_WITH_MESH,
                        format!(
                            "entity {name:?} has a Meadow component and also {extras}; \
                             a meadow grows its own plants (over the footprint \
                             Transform.scale gives) and carries its own colours in its \
                             stages — drop the extra component"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("Meadow"),
                );
            }

            // Refused before anything is allocated, because a hung render with
            // no output is the worst failure an agent loop can hit — M19's rule,
            // counted here in triangles because a meadow's cost is the *product*
            // of its plant count and its template (see `MAX_MEADOW_TRIANGLES`).
            let triangles = crate::meadow::triangle_count(meadow, scale.x, scale.z);
            if triangles > crate::meadow::MAX_MEADOW_TRIANGLES {
                let plants = crate::meadow::plant_count(meadow, scale.x, scale.z);
                errors.push(
                    cx.err(
                        codes::MEADOW_TOO_COMPLEX,
                        format!(
                            "entity {name:?} would grow {plants} plants of \
                             {} triangles each ({triangles} total), over the limit of {}; \
                             reduce density, the footprint, blades or segments",
                            triangles / plants.max(1),
                            crate::meadow::MAX_MEADOW_TRIANGLES
                        ),
                        path,
                    )
                    .entity(name)
                    .component("Meadow")
                    .field("density"),
                );
            }
        }

        // ── LightProbeVolume checks (M35) ────────────────────────────────
        if let Some((volume, path)) = &light_probe_volume {
            // The recipe rule, for a component that grows no geometry at all:
            // this entity is a *region of space*, and a Mesh beside it is a
            // second, silently ignored answer to what the entity is. Stated
            // separately from the other recipes because the reason differs —
            // the others own geometry, this one owns none.
            if has_mesh || !material_paths.is_empty() {
                let extras = match (has_mesh, material_paths.is_empty()) {
                    (true, false) => "a Mesh and a Material",
                    (true, true) => "a Mesh",
                    _ => "a Material",
                };
                errors.push(
                    cx.err(
                        codes::LIGHT_PROBE_VOLUME_WITH_MESH,
                        format!(
                            "entity {name:?} has a LightProbeVolume and also {extras}; \
                             a probe volume is a region of space that lights other \
                             surfaces and draws nothing itself — drop the extra component"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("LightProbeVolume"),
                );
            }

            // The Transform *is* the bounds — there is no other source for
            // them, so without one the volume covers nothing and every probe
            // would land on top of every other.
            if !has_transform {
                errors.push(
                    cx.err(
                        codes::LIGHT_PROBE_VOLUME_WITHOUT_TRANSFORM,
                        format!(
                            "entity {name:?} has a LightProbeVolume but no Transform; \
                             the Transform is the volume's bounds — a unit box scaled \
                             and positioned"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("LightProbeVolume"),
                );
            }

            // A bake taken before the volume was resized or re-tuned describes
            // a grid that no longer exists, so the renderer would index it with
            // coordinates it was never baked for. Cheap to catch: the header
            // records the grid, the spacing and the bounces it was taken with,
            // and all three are derivable from the component in front of us.
            //
            // This is the *component* half of staleness. The geometry half —
            // "somebody moved a wall after baking" — is what `inputs_hash`
            // exists for, and it is not checked here: reproducing that digest
            // means collecting every occluder in the scene, which for the tour
            // is around a million triangles, and `validate` is the ~0.02s gate
            // the whole agent loop leans on. See the note for the open question.
            let base_dir = std::path::Path::new(cx.file)
                .parent()
                .unwrap_or(std::path::Path::new(""));
            let bake_file = base_dir.join(&volume.bake);
            if let Ok(text) = std::fs::read_to_string(&bake_file) {
                if let Ok(baked) = crate::gi::BakedGi::parse(&text) {
                    if !baked.matches(volume, scale) {
                        let want = crate::gi::grid_counts(scale, volume.spacing);
                        errors.push(
                            cx.err(
                                codes::GI_BAKE_STALE,
                                format!(
                                    "entity {name:?} needs a {}×{}×{} grid at spacing {} \
                                     with {} bounce(s), but {:?} was baked as {}×{}×{} at \
                                     spacing {} with {}; re-run `engine bake-gi`",
                                    want[0],
                                    want[1],
                                    want[2],
                                    volume.spacing,
                                    volume.bounces,
                                    volume.bake,
                                    baked.header.grid[0],
                                    baked.header.grid[1],
                                    baked.header.grid[2],
                                    baked.header.spacing,
                                    baked.header.bounces,
                                ),
                                path,
                            )
                            .entity(name)
                            .component("LightProbeVolume")
                            .field("bake"),
                        );
                    }
                }
            }

            // Refused before anything is allocated — `tree_too_complex`'s rule.
            // A hung bake that produces no output is the worst failure an agent
            // loop can hit, and the arithmetic predicting the count is exact.
            let probes = crate::gi::probe_count_for(volume, scale);
            if probes > crate::gi::MAX_GI_PROBES {
                let grid = crate::gi::grid_counts(scale, volume.spacing);
                errors.push(
                    cx.err(
                        codes::TOO_MANY_GI_PROBES,
                        format!(
                            "entity {name:?} would place a {}×{}×{} grid ({probes} probes) \
                             over the limit of {}; raise spacing or shrink the volume",
                            grid[0],
                            grid[1],
                            grid[2],
                            crate::gi::MAX_GI_PROBES
                        ),
                        path,
                    )
                    .entity(name)
                    .component("LightProbeVolume")
                    .field("spacing"),
                );
            }
        }

        // ── HudInteract needs something to hit (M31) ──────────────────
        //
        // A `HudInteract` carries no geometry: the hit box *is* the laid-out
        // rectangle of the element on its own entity. With no element there is
        // no rectangle, so the component could only ever be a button nobody
        // can click — which is silent, and looks exactly like a broken hit
        // test.
        if let Some(path) = &hud_interact_path {
            if !has_hud_element {
                errors.push(
                    cx.err(
                        codes::HUD_INTERACT_WITHOUT_ELEMENT,
                        format!(
                            "entity {name:?} has a HudInteract but no HudPanel, HudRect, \
                             HudImage or HudText; the hit box is the element's own \
                             laid-out rectangle, so there is nothing here to click"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("HudInteract"),
                );
            }
        }

        // `hud_image_slice_too_large` is deliberately *not* here: comparing an
        // inset against the source needs the PNG's dimensions, and engine-core
        // cannot decode one. It lives in the engine-assets pass beside
        // `texture_too_large`, which is where every check that has to open a
        // file lives.
        let _ = &hud_image;

        // Legal but almost certainly wrong: dead data from editing the wrong
        // entity. A warning, because rendering it is well-defined. A `Tree`
        // counts as geometry here — the entity's Material is its bark. A Water,
        // Cloud, Road or Meadow entity's Material is already a hard error above,
        // so it does not also collect this: one mistake, one diagnostic.
        if !has_mesh
            && tree_path.is_none()
            && shard_path.is_none()
            && water.is_none()
            && cloud_path.is_none()
            && terrain.is_none()
            && road.is_none()
            && junction.is_none()
            && meadow.is_none()
        {
            for material_path in material_paths {
                errors.push(
                    cx.err(
                        codes::UNUSED_MATERIAL,
                        format!(
                            "entity {name:?} has a Material but no Mesh; \
                             the material affects nothing"
                        ),
                        &material_path,
                    )
                    .entity(name)
                    .component("Material")
                    .warning(),
                );
            }
        }
    }

    SceneFacts {
        roads,
        junctions,
        road_names,
        closed_road_names,
        seen_names,
        active_cameras,
        players,
        body_kinds,
        directional_lights,
        ambient_lights,
        point_lights,
        layer_memberships,
        layer_refs,
        distinct_layers,
        wheels,
        meadows,
        terrain_names,
        foot_plants,
        buoyancies,
        water_names,
        collider_names,
        hud_elements,
        hud_panel_names,
        probe_volumes,
    }
}

/// How far a collider may differ from the builtin mesh it sits on before the
/// difference is more likely a mistake than a decision.
///
/// A proxy collider is legitimate — a box around a barrel, a slightly inset
/// hull — so this is deliberately loose. What it is sized to catch is the
/// error class that has no other symptom: a factor of two (a `builtin:sphere`
/// was radius 1 until M34, so a collider matching the drawn ball was authored
/// at twice its visible size) and a `Transform.scale` applied twice (a radius
/// written as a world measurement, then scaled again by the engine).
const COLLIDER_SIZE_TOLERANCE: f32 = 1.25;

/// Compare a `sphere` or `cuboid` collider against the builtin mesh on the
/// same entity, both in world units.
///
/// Only the two shapes whose extent is a plain function of their fields: a
/// `capsule`'s half-height means something else, and the mesh shapes *are* the
/// mesh and cannot disagree with it. Axes the mesh is flat in are skipped —
/// a `builtin:plane` floor needs a collider with thickness, and that is the
/// normal authoring, not a mistake.
fn check_collider_matches_mesh(
    cx: &Cx,
    entity: &str,
    builtin: BuiltinMesh,
    scale: Vec3,
    collider: &Collider,
    path: &str,
) -> Option<EngineError> {
    let drawn = builtin.half_extents() * scale.abs();
    let (simulated, field) = match collider.shape {
        // Round shapes take their radius from x; validation elsewhere refuses
        // a non-uniform scale on them, so this is the whole story.
        ColliderShapeKind::Sphere => (Vec3::splat(collider.radius?) * scale.x.abs(), "radius"),
        ColliderShapeKind::Cuboid => (collider.half_extents? * scale.abs(), "half_extents"),
        _ => return None,
    };

    let off = (0..3).find(|&axis| {
        let (a, b) = (drawn[axis], simulated[axis]);
        a > 1e-6 && b > 1e-6 && (a / b > COLLIDER_SIZE_TOLERANCE || b / a > COLLIDER_SIZE_TOLERANCE)
    })?;

    let axis = ["x", "y", "z"][off];
    Some(
        cx.err(
            codes::COLLIDER_MESH_SIZE_MISMATCH,
            format!(
                "entity {entity:?} draws {asset} {drawn_m:.3} m across on {axis} but \
                 collides as {sim_m:.3} m; a builtin mesh is 1 m across at scale 1, so \
                 Collider.{field} is in the entity's own units and Transform.scale \
                 multiplies it too",
                asset = builtin.asset(),
                drawn_m = drawn[off] * 2.0,
                sim_m = simulated[off] * 2.0,
            ),
            path,
        )
        .entity(entity)
        .component("Collider")
        .field(field)
        .warning(),
    )
}
