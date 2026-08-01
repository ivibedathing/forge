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
    pub(super) terrain_names: std::collections::BTreeSet<String>,
    pub(super) foot_plants: Vec<(String, crate::components::FootPlant, String)>,
    pub(super) hud_elements: Vec<HudRef>,
    pub(super) hud_panel_names: std::collections::BTreeSet<String>,
}

/// Walk every entity, pushing per-entity errors and collecting the rest.
pub(super) fn walk<'a>(
    cx: &Cx<'_>,
    schemas: &ComponentSchemas,
    entities: &'a [Value],
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
    let mut terrain_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // Foot planting pass inputs (M32): a `FootPlant` names the Terrain its
    // feet stand on, so it waits for every name too — the meadow pass's shape,
    // for the meadow pass's reason.
    let mut foot_plants: Vec<(String, crate::components::FootPlant, String)> = Vec::new();
    // One HUD element awaiting the cross-entity pass (M31): its owner, the
    // component that owns the `parent` reference, the reference itself, and
    // the component-s JSON path. Collected across all four kinds because
    // `parent` means the same on every one of them, so one pass checks them
    // all -- four near-identical passes is how two of them start disagreeing.
    let mut hud_elements: Vec<HudRef> = Vec::new();
    let mut hud_panel_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for (entity_index, entity) in entities.iter().enumerate() {
        let entity_path = format!("/entities/{entity_index}");

        let Some(entity) = entity.as_object() else {
            errors.push(cx.err(
                codes::ENTITY_NOT_OBJECT,
                format!(
                    "entity at index {entity_index} must be an object, found {}",
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
                        codes::EMPTY_ENTITY_NAME,
                        format!("entity at index {entity_index} has an empty name"),
                        &format!("{entity_path}/name"),
                    )
                    .field("name"),
                );
                continue;
            }
            Some(other) => {
                errors.push(
                    cx.wrong_type("name", "string", other, &format!("{entity_path}/name"))
                        .entity(format!("<entity at index {entity_index}>")),
                );
                continue;
            }
            None => {
                errors.push(
                    cx.err(
                        codes::MISSING_ENTITY_NAME,
                        format!(
                            "entity at index {entity_index} has no name; \
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
                    codes::DUPLICATE_ENTITY_NAME,
                    format!(
                        "more than one entity is named {name:?}; names must be unique \
                         because they are how entities are targeted"
                    ),
                    &format!("{entity_path}/name"),
                )
                .entity(name),
            );
        }
        seen_names.push(name);

        for key in entity.keys() {
            if key != "name" && key != "components" {
                errors.push(
                    cx.err(
                        codes::UNKNOWN_FIELD,
                        format!("unknown entity field {key:?}"),
                        &format!("{entity_path}/{key}"),
                    )
                    .entity(name)
                    .field(key)
                    .suggest_from(key, ["name", "components"]),
                );
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
        let mut tree_path: Option<String> = None;
        let mut cloud_path: Option<String> = None;
        let mut material_paths: Vec<String> = Vec::new();
        let mut has_transform = false;
        let mut scale = glam::Vec3::ONE;
        let mut rigid_body: Option<(crate::components::BodyKind, String)> = None;
        let mut collider: Option<(crate::components::Collider, String)> = None;
        let mut wheel_path: Option<String> = None;
        let mut breakable_threshold: Option<String> = None;
        let mut water: Option<(crate::components::Water, String)> = None;
        let mut terrain: Option<(crate::components::Terrain, String)> = None;
        let mut road: Option<(crate::components::Road, String)> = None;
        let mut meadow: Option<(crate::components::Meadow, String)> = None;
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
            if type_name == "Material" {
                material_paths.push(component_path.clone());
            }
            match checked.parsed {
                Some(ComponentData::Transform(t)) => {
                    has_transform = true;
                    scale = t.scale;
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
                    road = Some((r, component_path));
                }
                Some(ComponentData::Meadow(m)) => {
                    meadows.push((name.to_string(), m.clone(), component_path.clone()));
                    meadow = Some((m, component_path));
                }
                Some(ComponentData::FootPlant(p)) => {
                    foot_plant = Some(component_path.clone());
                    foot_plants.push((name.to_string(), p, component_path));
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
            if mesh_shape
                && collider_data.asset.is_none()
                && !has_mesh
                && terrain.is_none()
                && road.is_none()
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
            && water.is_none()
            && cloud_path.is_none()
            && terrain.is_none()
            && road.is_none()
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
        hud_elements,
        hud_panel_names,
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
