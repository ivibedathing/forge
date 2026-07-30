//! Scene validation.
//!
//! Deliberately does *not* just call `serde_json::from_str::<SceneFile>` and
//! forward the error. serde stops at the first problem and describes it in
//! prose; an agent wants every problem at once, each as a structured record it
//! can act on without parsing English.
//!
//! So this walks the JSON tree itself, collecting errors. Per-component field
//! checking is driven by the schemars-generated component schema — the same
//! schema `engine list-components` publishes — so validation and publication
//! cannot disagree (invariant 7, upgraded: the schema is derived *and*
//! enforced). serde then parses each already-clean component as a final gate;
//! if the walk passes and serde still rejects, that is a `scene_parse_desync`
//! bug, not a scene problem. A [`LineIndex`] built from the raw source
//! supplies the line numbers that `serde_json::Value` discards, and every
//! error also carries its JSON Pointer in `path` (invariant 6: every error
//! names its file and line — and now where `jq` can reach it).
//!
//! The returned vector interleaves errors and warnings in file order; filter
//! with [`EngineError::is_warning`] to judge validity. Warnings mark scenes
//! that are legal but almost certainly wrong (a `Material` with no `Mesh`, a
//! zero scale axis) — the cases where the screenshot just looks subtly off
//! and the agent burns iterations discovering why.

use std::path::Path;

use serde_json::{Map, Value};

use crate::codes;
use crate::components::ComponentData;
use crate::error::EngineError;
use crate::lineindex::LineIndex;
use crate::mesh::MeshAsset;

/// Validate a scene file's contents. An empty result means the scene is valid
/// with nothing to warn about; a result with only warnings is still valid.
pub fn validate_source(source: &str, path: &str) -> Vec<EngineError> {
    let root: Value = match serde_json::from_str(source) {
        Ok(value) => value,
        Err(e) => {
            // Syntax errors never reach the LineIndex; serde_json itself knows
            // the position here.
            return vec![EngineError::new(codes::INVALID_JSON, e.to_string())
                .file(path)
                .line(e.line() as u32)
                .column(e.column() as u32)];
        }
    };

    let cx = Cx {
        file: path,
        index: LineIndex::new(source),
    };
    let schemas = ComponentSchemas::new();

    let mut errors = Vec::new();

    let Some(object) = root.as_object() else {
        errors.push(cx.err(
            codes::SCENE_ROOT_NOT_OBJECT,
            format!("a scene must be a JSON object, found {}", kind_of(&root)),
            "",
        ));
        return errors;
    };

    match object.get("name") {
        None => errors.push(cx.missing_field("name", "")),
        Some(v) if !v.is_string() => errors.push(cx.wrong_type("name", "string", v, "/name")),
        Some(_) => {}
    }

    for key in object.keys() {
        if key != "name"
            && key != "entities"
            && key != "physics"
            && key != "environment"
            && key != "daylight"
        {
            errors.push(
                cx.err(
                    codes::UNKNOWN_FIELD,
                    format!("unknown top-level field {key:?}"),
                    &format!("/{key}"),
                )
                .field(key)
                .suggest_from(
                    key,
                    ["name", "entities", "physics", "environment", "daylight"],
                ),
            );
        }
    }

    if let Some(physics) = object.get("physics") {
        check_physics_block(&cx, physics, &mut errors);
    }

    if let Some(environment) = object.get("environment") {
        check_environment_block(&cx, environment, &mut errors);
    }

    if let Some(daylight) = object.get("daylight") {
        check_daylight_block(&cx, daylight, object.get("environment"), &mut errors);
    }

    let entities = match object.get("entities") {
        None => {
            errors.push(cx.missing_field("entities", ""));
            return errors;
        }
        Some(Value::Array(entities)) => entities,
        Some(other) => {
            errors.push(cx.wrong_type("entities", "array", other, "/entities"));
            return errors;
        }
    };

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

        for (component_index, component) in components.iter().enumerate() {
            let component_path = format!("{entity_path}/components/{component_index}");
            let checked = check_component(&cx, &schemas, component, name, &component_path, &mut errors);

            let Some(type_name) = checked.type_name else {
                continue;
            };

            if seen_types.iter().any(|t| *t == type_name) {
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
                    players.push((name.to_string(), player, component_path));
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
                _ => {}
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
            if mesh_shape && collider_data.asset.is_none() && !has_mesh {
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

        // Legal but almost certainly wrong: dead data from editing the wrong
        // entity. A warning, because rendering it is well-defined. A `Tree`
        // counts as geometry here — the entity's Material is its bark. A Water
        // or Cloud entity's Material is already a hard error above, so it does
        // not also collect this: one mistake, one diagnostic.
        if !has_mesh && tree_path.is_none() && water.is_none() && cloud_path.is_none() {
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

    if active_cameras.len() > 1 {
        let names: Vec<&str> = active_cameras.iter().map(|(n, _)| n.as_str()).collect();
        // Point at the first surplus camera — the one that made it ambiguous.
        let (_, surplus_path) = &active_cameras[1];
        errors.push(
            cx.err(
                codes::MULTIPLE_ACTIVE_CAMERAS,
                format!(
                    "{} cameras are marked active ({}); exactly one may be, \
                     otherwise which one renders is arbitrary",
                    active_cameras.len(),
                    names.join(", ")
                ),
                surplus_path,
            )
            .component("Camera")
            .candidates(names),
        );
    }

    // Lights follow the camera precedent: a deterministic failure over a
    // silent pick of whichever entity the world iterates first.
    for (list, component, code) in [
        (
            &directional_lights,
            "DirectionalLight",
            codes::MULTIPLE_DIRECTIONAL_LIGHTS,
        ),
        (
            &ambient_lights,
            "AmbientLight",
            codes::MULTIPLE_AMBIENT_LIGHTS,
        ),
    ] {
        if list.len() > 1 {
            let names: Vec<&str> = list.iter().map(|(n, _)| n.as_str()).collect();
            let (_, surplus_path) = &list[1];
            errors.push(
                cx.err(
                    code,
                    format!(
                        "{} {component} components in one scene ({}); at most one is \
                         allowed, otherwise which applies is arbitrary",
                        list.len(),
                        names.join(", ")
                    ),
                    surplus_path,
                )
                .component(component)
                .candidates(names),
            );
        }
    }

    // ── Daylight ownership (M21) ───────────────────────────────────────
    // Two owners of one sun is what invariant 8 exists to prevent: a rotation
    // in a text file that is silently ignored, or silently overwritten, is a
    // value that does not mean what it says.
    if let Some(daylight) = object.get("daylight").and_then(Value::as_object) {
        let drives_sun = daylight
            .get("drives_sun")
            .map_or(true, |v| v.as_bool().unwrap_or(true));

        if drives_sun {
            for (name, path) in &directional_lights {
                errors.push(
                    cx.err(
                        codes::DAYLIGHT_AND_DIRECTIONAL_LIGHT,
                        format!(
                            "entity {name:?} has a DirectionalLight, but the scene's daylight \
                             block drives the sun; remove the light, or set \
                             daylight.drives_sun to false to keep aiming it by hand"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("DirectionalLight"),
                );
            }
        }

        let drives_sky = daylight
            .get("drives_sky")
            .map_or(true, |v| v.as_bool().unwrap_or(true));

        // The other half of `daylight_overrides_sky`: ambient rides with the
        // sky, so an authored AmbientLight is unread for the same reason the
        // authored band colors are.
        if drives_sky {
            for (name, path) in &ambient_lights {
                errors.push(
                    cx.err(
                        codes::DAYLIGHT_OVERRIDES_SKY,
                        format!(
                            "entity {name:?} has an AmbientLight, but daylight computes the \
                             ambient term from its palette; set daylight.drives_sky to false \
                             to keep this one"
                        ),
                        path,
                    )
                    .entity(name)
                    .component("AmbientLight")
                    .warning(),
                );
            }
        }
    }

    // Point lights are plural by design, but the shader array is fixed-size.
    // Refusing the scene beats dropping the ninth light silently — an agent
    // that placed nine and sees eight has no way to tell which was ignored.
    if point_lights.len() > crate::components::MAX_POINT_LIGHTS {
        let names: Vec<&str> = point_lights.iter().map(|(n, _)| n.as_str()).collect();
        let (_, surplus_path) = &point_lights[crate::components::MAX_POINT_LIGHTS];
        errors.push(
            cx.err(
                codes::TOO_MANY_POINT_LIGHTS,
                format!(
                    "{} PointLight components in one scene ({}); at most {} are allowed, \
                     because the shader carries them in a fixed-size array",
                    point_lights.len(),
                    names.join(", "),
                    crate::components::MAX_POINT_LIGHTS,
                ),
                surplus_path,
            )
            .component("PointLight")
            .candidates(names),
        );
    }

    // ── Collision layers (M12): 32-bit budget, reference checks ────────
    if distinct_layers.len() > 32 {
        let (surplus_name, surplus_path) = &distinct_layers[32];
        errors.push(
            cx.err(
                codes::TOO_MANY_COLLISION_LAYERS,
                format!(
                    "this scene names {} distinct collision layers; rapier's \
                     interaction groups hold 32 bits, so at most 32 names may \
                     exist ({surplus_name:?} is the 33rd)",
                    distinct_layers.len()
                ),
                surplus_path,
            )
            .component("Collider"),
        );
    }
    for (layer, entity, path) in &layer_refs {
        if !layer_memberships.contains(layer) {
            // A warning: colliders that declare no `layers` are members of
            // everything, so the reference still matches them — but naming a
            // layer nobody declares is almost always a typo.
            errors.push(
                cx.err(
                    codes::UNKNOWN_COLLISION_LAYER,
                    format!(
                        "collides_with names layer {layer:?}, but no collider \
                         in this scene is a member of it"
                    ),
                    path,
                )
                .entity(entity)
                .component("Collider")
                .field("collides_with")
                .suggest_from(layer, layer_memberships.iter().map(String::as_str))
                .warning(),
            );
        }
    }

    // ── Wheel pass (M12): every wheel's chassis must exist and be a
    //    different entity with a dynamic RigidBody ─────────────────────
    for (owner, wheel, wheel_component_path) in &wheels {
        let vehicle_path = format!("{wheel_component_path}/vehicle");
        if !seen_names.contains(&wheel.vehicle.as_str()) {
            errors.push(
                cx.err(
                    codes::WHEEL_VEHICLE_NOT_FOUND,
                    format!(
                        "the Wheel on {owner:?} names vehicle {:?}, which is not \
                         an entity in this scene",
                        wheel.vehicle
                    ),
                    &vehicle_path,
                )
                .entity(owner)
                .component("Wheel")
                .field("vehicle")
                .suggest_from(&wheel.vehicle, seen_names.iter().copied()),
            );
        } else if wheel.vehicle == *owner
            || body_kinds.get(&wheel.vehicle) != Some(&crate::components::BodyKind::Dynamic)
        {
            let why = if wheel.vehicle == *owner {
                "a wheel cannot be its own chassis".to_string()
            } else {
                format!("{:?} has no dynamic RigidBody to suspend", wheel.vehicle)
            };
            errors.push(
                cx.err(
                    codes::WHEEL_VEHICLE_INVALID,
                    format!(
                        "the Wheel on {owner:?} names vehicle {:?}, but {why}; \
                         the vehicle must be a different entity with a dynamic \
                         RigidBody and a Collider",
                        wheel.vehicle
                    ),
                    &vehicle_path,
                )
                .entity(owner)
                .component("Wheel")
                .field("vehicle"),
            );
        }
    }

    // ── Animation pass (M9): clip contents, target entities, conflicts ─
    // Runs against the same scene the players sit in; clip-content errors
    // carry the *clip's* file/line via its own LineIndex.
    let base_dir = Path::new(cx.file).parent().unwrap_or(Path::new(""));
    let mut claimed: std::collections::HashMap<(String, String), String> =
        std::collections::HashMap::new();
    for (player_entity, player, player_path) in &players {
        if player.clip.contains('#')
            || Path::new(&player.clip).is_absolute()
        {
            continue; // Already reported by the component check.
        }
        let clip_path = base_dir.join(&player.clip);
        let Ok(clip_source) = std::fs::read_to_string(&clip_path) else {
            continue; // asset_not_found already reported.
        };
        let clip_display = clip_path.display().to_string();

        let clip_errors = crate::animation::validate_clip_source(&clip_source, &clip_display);
        if !clip_errors.is_empty() {
            errors.extend(clip_errors);
            continue;
        }
        let Ok(clip) = serde_json::from_str::<crate::animation::ClipFile>(&clip_source) else {
            continue;
        };
        let clip_index = LineIndex::new(&clip_source);

        for (track_index, track) in clip.tracks.iter().enumerate() {
            let entity_pointer = format!("/tracks/{track_index}/entity");
            if !seen_names.contains(&track.entity.as_str()) {
                let mut error = EngineError::new(
                    codes::UNKNOWN_ENTITY,
                    format!(
                        "track {track_index} of clip {:?} targets {:?}, which is                          not an entity in this scene",
                        clip.name, track.entity
                    ),
                )
                .file(&clip_display)
                .path(&entity_pointer)
                .entity(&track.entity)
                .suggest_from(&track.entity, seen_names.iter().copied());
                if let Some(line) = clip_index.line_of_or_parent(&entity_pointer) {
                    error = error.line(line);
                }
                errors.push(error);
                continue;
            }

            // Two active clips on one entity.property: deterministic failure
            // over silent last-writer-wins (the active-camera rationale).
            let key = (track.entity.clone(), track.property.clone());
            match claimed.get(&key) {
                Some(other_player) if other_player != player_entity => {
                    errors.push(
                        cx.err(
                            codes::CONFLICTING_TRACKS,
                            format!(
                                "players on {other_player:?} and {player_entity:?} both                                  animate {}.{}; at most one clip may drive a property",
                                track.entity, track.property
                            ),
                            player_path,
                        )
                        .component("AnimationPlayer")
                        .candidates([other_player.as_str(), player_entity.as_str()]),
                    );
                }
                Some(_) => {}
                None => {
                    claimed.insert(key, player_entity.clone());
                }
            }

            // M8 ownership rule, settled here: an animated Transform on a
            // dynamic body is a contradiction (who wins?); kinematic bodies
            // are exactly the "animation drives, physics follows" case.
            if track.property.starts_with("Transform.")
                && body_kinds.get(&track.entity)
                    == Some(&crate::components::BodyKind::Dynamic)
            {
                errors.push(
                    cx.err(
                        codes::ANIMATION_ON_DYNAMIC_BODY,
                        format!(
                            "clip {:?} animates {}.{} but {:?} has a dynamic                              RigidBody; make the body kinematic if animation                              should drive it",
                            clip.name, track.entity, track.property, track.entity
                        ),
                        player_path,
                    )
                    .entity(&track.entity)
                    .component("AnimationPlayer"),
                );
            }
        }
    }

    errors
}

/// What a checked component tells the caller.
#[derive(Default)]
struct Checked {
    /// The component's known type name — set even when its fields have
    /// errors, so duplicate detection still sees it.
    type_name: Option<String>,
    active_camera: bool,
    directional_light: bool,
    ambient_light: bool,
    point_light: bool,
    /// The parsed component when the shape was clean — what the entity-level
    /// cross-component checks (physics, M8) read.
    parsed: Option<ComponentData>,
}

impl Checked {
    fn named(type_name: &str) -> Self {
        Self {
            type_name: Some(type_name.to_string()),
            ..Self::default()
        }
    }
}

/// The schemars-generated component schema, the driver for per-component
/// field checking. Built once per validated file.
struct ComponentSchemas {
    schema: Value,
}

impl ComponentSchemas {
    fn new() -> Self {
        Self {
            schema: crate::schema::component_schema(),
        }
    }

    /// Look through a `$ref` to the schema's `$defs` (schemars refs shared
    /// types like enums). Non-ref schemas come back unchanged.
    fn resolve<'a>(&'a self, property: &'a Value) -> &'a Value {
        match property["$ref"].as_str().and_then(|r| r.strip_prefix("#/$defs/")) {
            Some(name) => &self.schema["$defs"][name],
            None => property,
        }
    }

    /// The `oneOf` variant for a component name. The discrimination shape is
    /// pinned by tests in `schema.rs`, so a known name always resolves.
    fn variant(&self, name: &str) -> Option<&Value> {
        self.schema["oneOf"]
            .as_array()?
            .iter()
            .find(|v| v["properties"]["type"]["const"] == name)
    }
}

/// Shared validation context: the file name and the line lookup.
struct Cx<'a> {
    file: &'a str,
    index: LineIndex,
}

impl Cx<'_> {
    /// A structured error at `json_path`, with file, line, and JSON Pointer
    /// attached. The pointer is what the validator walked to get here; keeping
    /// it costs nothing and makes the fix `jq`-addressable.
    fn err(&self, code: &'static str, message: String, json_path: &str) -> EngineError {
        let error = EngineError::new(code, message).file(self.file).path(json_path);
        match self.index.line_of_or_parent(json_path) {
            Some(line) => error.line(line),
            None => error,
        }
    }

    fn missing_field(&self, field: &str, parent_path: &str) -> EngineError {
        self.err(
            codes::MISSING_FIELD,
            format!("a scene requires a {field:?} field"),
            parent_path,
        )
        .field(field)
    }

    fn wrong_type(
        &self,
        field: &str,
        expected: &str,
        found: &Value,
        json_path: &str,
    ) -> EngineError {
        self.err(
            codes::INVALID_FIELD_TYPE,
            format!(
                "{field:?} must be {} {expected}, found {}",
                article(expected),
                kind_of(found)
            ),
            json_path,
        )
        .field(field)
    }
}

fn check_component(
    cx: &Cx<'_>,
    schemas: &ComponentSchemas,
    component: &Value,
    entity: &str,
    component_path: &str,
    errors: &mut Vec<EngineError>,
) -> Checked {
    let Some(object) = component.as_object() else {
        errors.push(
            cx.err(
                codes::COMPONENT_NOT_OBJECT,
                format!("components must be objects, found {}", kind_of(component)),
                component_path,
            )
            .entity(entity),
        );
        return Checked::default();
    };

    let type_name = match object.get("type") {
        Some(Value::String(name)) => name.as_str(),
        Some(other) => {
            errors.push(
                cx.wrong_type("type", "string", other, &format!("{component_path}/type"))
                    .entity(entity),
            );
            return Checked::default();
        }
        None => {
            errors.push(
                cx.err(
                    codes::COMPONENT_MISSING_TYPE,
                    "every component needs a \"type\" field naming which component it is"
                        .to_string(),
                    component_path,
                )
                .entity(entity)
                .field("type"),
            );
            return Checked::default();
        }
    };

    if !ComponentData::NAMES.contains(&type_name) {
        errors.push(
            cx.err(
                codes::UNKNOWN_COMPONENT,
                format!("no component named {type_name:?}"),
                &format!("{component_path}/type"),
            )
            .entity(entity)
            .component(type_name)
            .suggest_from(type_name, ComponentData::NAMES.iter().copied()),
        );
        return Checked::default();
    }

    // The name is known; the schema variant drives the field checks from here.
    // `variant` cannot miss for a known name (schema.rs pins the shape), but a
    // validator must degrade rather than panic, so the impossible branch just
    // skips field checking.
    let Some(variant) = schemas.variant(type_name) else {
        return Checked::named(type_name);
    };

    let shape_clean =
        walk_component(cx, schemas, variant, object, type_name, entity, component_path, errors);
    if !shape_clean {
        // Field names or JSON types are wrong; serde would reject this
        // component, so parsing and the semantic checks below are moot.
        return Checked::named(type_name);
    }

    // The walk passed, so serde must accept — it is the final gate proving the
    // walk and the parser agree. A rejection here is an engine bug, and the
    // error code says so rather than blaming the scene.
    let parsed = match serde_json::from_value::<ComponentData>(component.clone()) {
        Ok(parsed) => parsed,
        Err(e) => {
            errors.push(
                cx.err(
                    codes::SCENE_PARSE_DESYNC,
                    format!(
                        "component {type_name:?} passed the schema walk but failed to \
                         parse ({e}); this is an engine bug, not a scene problem"
                    ),
                    component_path,
                )
                .entity(entity)
                .component(type_name),
            );
            return Checked::named(type_name);
        }
    };

    let mut checked = Checked::named(type_name);
    checked.parsed = Some(parsed.clone());

    match parsed {
        // An unresolvable mesh asset is a validation error, never a silent
        // fallback (design doc §5). Resolution is against the scene file's own
        // directory, because that is what relative asset paths mean. This
        // checks the reference (existence, extension); whether the file
        // *parses* is checked by `engine validate` through engine-assets.
        ComponentData::Mesh(mesh) => {
            let base_dir = Path::new(cx.file).parent().unwrap_or(Path::new(""));
            if let Err(resolve) = MeshAsset::resolve(&mesh.asset, base_dir) {
                let mut error = cx
                    .err(
                        resolve.error,
                        resolve.message.clone(),
                        &format!("{component_path}/asset"),
                    )
                    .entity(entity)
                    .component("Mesh")
                    .field("asset");
                if let Some(suggestion) = resolve.context().and_then(|c| c.did_you_mean.clone()) {
                    error = error.did_you_mean(suggestion);
                }
                errors.push(error);
            }
        }

        ComponentData::Camera(camera) => {
            // JSON Schema cannot express `far > near`, so this one check is
            // hand-written. Written as `!(far > near)` so NaN also fails.
            if !(camera.far > camera.near) {
                errors.push(
                    cx.err(
                        codes::VALUE_OUT_OF_RANGE,
                        format!(
                            "Camera.far is {}; it must be greater than near ({})",
                            camera.far, camera.near
                        ),
                        &format!("{component_path}/far"),
                    )
                    .entity(entity)
                    .component("Camera")
                    .field("far"),
                );
            }
            checked.active_camera = camera.active;
        }

        ComponentData::Transform(transform) => {
            // Legal but almost certainly wrong: renders invisibly or as
            // degenerate geometry with no error — the classic "I edited the
            // file and nothing changed".
            let scale = transform.scale.to_array();
            if scale.contains(&0.0) {
                errors.push(
                    cx.err(
                        codes::ZERO_SCALE,
                        format!(
                            "Transform.scale is [{}, {}, {}]; a zero axis renders the \
                             entity invisibly or as degenerate geometry",
                            scale[0], scale[1], scale[2]
                        ),
                        &format!("{component_path}/scale"),
                    )
                    .entity(entity)
                    .component("Transform")
                    .field("scale")
                    .warning(),
                );
            }
        }

        ComponentData::DirectionalLight(_) => checked.directional_light = true,
        ComponentData::AmbientLight(_) => checked.ambient_light = true,
        ComponentData::PointLight(_) => checked.point_light = true,
        ComponentData::Material(_) => {}

        // Water's own fields are fully covered by the schema walk (ranges,
        // `maxItems` on the wave list); what is left is cross-component and
        // cross-wave, and lives with the entity checks that can see the whole
        // entity.
        ComponentData::Water(_) => {}

        // The flat Collider struct keeps the file walkable; which fields each
        // shape requires and forbids is semantic, checked here (design §5).
        ComponentData::Collider(ref collider) => {
            use crate::components::ColliderShapeKind::{
                Capsule, ConvexHull, Cuboid, Sphere, Trimesh,
            };

            let shape_name = match collider.shape {
                Cuboid => "cuboid",
                Sphere => "sphere",
                Capsule => "capsule",
                Trimesh => "trimesh",
                ConvexHull => "convex_hull",
            };
            let fields: [(&str, bool, bool); 3] = [
                // (field, present, wanted-by-this-shape)
                (
                    "half_extents",
                    collider.half_extents.is_some(),
                    collider.shape == Cuboid,
                ),
                (
                    "radius",
                    collider.radius.is_some(),
                    matches!(collider.shape, Sphere | Capsule),
                ),
                (
                    "half_height",
                    collider.half_height.is_some(),
                    collider.shape == Capsule,
                ),
            ];
            for (field, present, wanted) in fields {
                if wanted && !present {
                    errors.push(
                        cx.err(
                            codes::MISSING_FIELD,
                            format!("{shape_name} colliders require the field {field:?}"),
                            component_path,
                        )
                        .entity(entity)
                        .component("Collider")
                        .field(field),
                    );
                }
                if !wanted && present {
                    errors.push(
                        cx.err(
                            codes::SHAPE_FIELD_MISMATCH,
                            format!("{shape_name} colliders have no field {field:?}"),
                            &format!("{component_path}/{field}"),
                        )
                        .entity(entity)
                        .component("Collider")
                        .field(field),
                    );
                }
            }

            // `asset` belongs to the mesh shapes only — and unlike the
            // dimension fields it is optional there (absent means "borrow
            // the entity's Mesh", checked at entity level).
            let mesh_shape = matches!(collider.shape, Trimesh | ConvexHull);
            if let Some(asset) = &collider.asset {
                if !mesh_shape {
                    errors.push(
                        cx.err(
                            codes::SHAPE_FIELD_MISMATCH,
                            format!("{shape_name} colliders have no field \"asset\""),
                            &format!("{component_path}/asset"),
                        )
                        .entity(entity)
                        .component("Collider")
                        .field("asset"),
                    );
                } else {
                    // Same reference checks as Mesh.asset: existence,
                    // extension, relative path. Parsing is the asset pass's.
                    let base_dir = Path::new(cx.file).parent().unwrap_or(Path::new(""));
                    if let Err(resolve) = MeshAsset::resolve(asset, base_dir) {
                        let mut error = cx
                            .err(
                                resolve.error,
                                resolve.message.clone(),
                                &format!("{component_path}/asset"),
                            )
                            .entity(entity)
                            .component("Collider")
                            .field("asset");
                        if let Some(suggestion) =
                            resolve.context().and_then(|c| c.did_you_mean.clone())
                        {
                            error = error.did_you_mean(suggestion);
                        }
                        errors.push(error);
                    }
                }
            }

            // An empty layer array reads as "member of/collides with
            // nothing" — a trap when the author meant "everything". The
            // field being absent is how "everything" is spelled.
            for (field, list) in [
                ("layers", &collider.layers),
                ("collides_with", &collider.collides_with),
            ] {
                if list.as_ref().is_some_and(Vec::is_empty) {
                    errors.push(
                        cx.err(
                            codes::EMPTY_COLLISION_LAYERS,
                            format!(
                                "Collider.{field} is an empty array, which would mean \
                                 \"nothing\"; omit the field to mean \"everything\""
                            ),
                            &format!("{component_path}/{field}"),
                        )
                        .entity(entity)
                        .component("Collider")
                        .field(field),
                    );
                }
            }

            // Dimensions are strictly positive; NaN fails too via !(v > 0).
            let mut dimension = |field: &str, label: String, v: f32| {
                if !(v > 0.0) {
                    errors.push(
                        cx.err(
                            codes::INVALID_SHAPE_DIMENSION,
                            format!("Collider.{label} is {v}; it must be greater than 0"),
                            &format!("{component_path}/{field}"),
                        )
                        .entity(entity)
                        .component("Collider")
                        .field(field),
                    );
                }
            };
            if let Some(half_extents) = collider.half_extents {
                for (i, v) in half_extents.to_array().into_iter().enumerate() {
                    dimension("half_extents", format!("half_extents[{i}]"), v);
                }
            }
            if let Some(radius) = collider.radius {
                dimension("radius", "radius".into(), radius);
            }
            if let Some(half_height) = collider.half_height {
                dimension("half_height", "half_height".into(), half_height);
            }
        }

        // RigidBody's numeric ranges live in the published schema; the
        // cross-component requirements (Transform, Collider) are entity-level.
        ComponentData::RigidBody(_) => {}

        // Wheel's numeric ranges live in the schema; its entity-level rules
        // (Transform required, no own physics) and its cross-entity vehicle
        // reference are checked by the scene-level wheel pass.
        ComponentData::Wheel(_) => {}

        // HUD elements are fully described by the schema: anchor is a schema
        // enum, sizes/colors/opacity are schema ranges, and they reference no
        // files and need no Transform.
        ComponentData::HudText(_) | ComponentData::HudRect(_) => {}

        // Every emitter constraint is a schema range; the simulation reads
        // whatever validated, so there is nothing semantic left to check.
        ComponentData::ParticleEmitter(_) => {}

        // Every individual tree field is in range by the time we get here, and
        // the combination can still be absurd: branching is exponential, so
        // one more level is the whole scene's geometry again. Refusing to grow
        // it names a number the author can act on, where growing it means a
        // command that looks like it hung.
        ComponentData::Tree(ref tree) => {
            let vertices = crate::tree::vertex_count(tree);
            if vertices > crate::tree::MAX_TREE_VERTICES {
                errors.push(
                    cx.err(
                        codes::TREE_TOO_COMPLEX,
                        format!(
                            "this Tree would generate {vertices} vertices, over the \
                             {} the engine will grow; lower \"levels\", \"branches\", \
                             \"whorl\", \"sides\", or \"segments\"",
                            crate::tree::MAX_TREE_VERTICES
                        ),
                        component_path,
                    )
                    .entity(entity)
                    .component("Tree")
                    .field("levels"),
                );
            }
        }

        // Lobes are exponential in `levels` exactly as branches are, and the
        // ceiling is reachable by one keystroke: `levels: 3, children: 8` at 32
        // base lobes is 18,720 lobes. Refusing names a number the author can
        // act on; growing it is a command that looks like it hung.
        ComponentData::Cloud(ref cloud) => {
            let vertices = crate::cloud::vertex_count(cloud);
            if vertices > crate::cloud::MAX_CLOUD_VERTICES {
                errors.push(
                    cx.err(
                        codes::CLOUD_TOO_COMPLEX,
                        format!(
                            "this Cloud would generate {vertices} vertices ({} lobes), \
                             over the {} the engine will grow; lower \"levels\", \
                             \"children\", \"lobes\", or \"detail\"",
                            crate::cloud::lobe_count(cloud),
                            crate::cloud::MAX_CLOUD_VERTICES
                        ),
                        component_path,
                    )
                    .entity(entity)
                    .component("Cloud")
                    .field("levels"),
                );
            }
        }

        // Fragment mesh references resolve like `Mesh.asset` (existence,
        // extension, relative path); fragment collider dimensions are
        // strictly positive like a Collider's.
        ComponentData::Breakable(ref breakable) => {
            let base_dir = Path::new(cx.file).parent().unwrap_or(Path::new(""));
            for (i, fragment) in breakable.fragments.iter().enumerate() {
                let fragment_path = format!("{component_path}/fragments/{i}");
                if let Err(resolve) = MeshAsset::resolve(&fragment.mesh, base_dir) {
                    let mut error = cx
                        .err(
                            resolve.error,
                            resolve.message.clone(),
                            &format!("{fragment_path}/mesh"),
                        )
                        .entity(entity)
                        .component("Breakable")
                        .field("mesh");
                    if let Some(suggestion) =
                        resolve.context().and_then(|c| c.did_you_mean.clone())
                    {
                        error = error.did_you_mean(suggestion);
                    }
                    errors.push(error);
                }
                for (axis, v) in fragment.half_extents.to_array().into_iter().enumerate() {
                    if !(v > 0.0) {
                        errors.push(
                            cx.err(
                                codes::INVALID_SHAPE_DIMENSION,
                                format!(
                                    "Breakable.fragments[{i}].half_extents[{axis}] is {v}; \
                                     it must be greater than 0"
                                ),
                                &format!("{fragment_path}/half_extents/{axis}"),
                            )
                            .entity(entity)
                            .component("Breakable")
                            .field("half_extents"),
                        );
                    }
                }
            }
        }

        // Script references: relative, existing, .rhai. Compilation is the
        // script pass's job (engine-script), like glTF parsing is the asset
        // pass's.
        ComponentData::Script(script) => {
            let json_path = &format!("{component_path}/source");
            if Path::new(&script.source).is_absolute() {
                errors.push(
                    cx.err(
                        codes::ASSET_PATH_NOT_RELATIVE,
                        format!(
                            "script {:?} is an absolute path; scripts are referenced                              relative to the scene file",
                            script.source
                        ),
                        json_path,
                    )
                    .entity(entity)
                    .component("Script")
                    .field("source"),
                );
            } else if !script.source.ends_with(".rhai") {
                errors.push(
                    cx.err(
                        codes::ASSET_UNSUPPORTED,
                        format!("script {:?} is not a .rhai file", script.source),
                        json_path,
                    )
                    .entity(entity)
                    .component("Script")
                    .field("source"),
                );
            } else {
                let base_dir = Path::new(cx.file).parent().unwrap_or(Path::new(""));
                if !base_dir.join(&script.source).is_file() {
                    errors.push(
                        cx.err(
                            codes::ASSET_NOT_FOUND,
                            format!(
                                "no script file at {:?} (script paths resolve relative                                  to the scene file)",
                                script.source
                            ),
                            json_path,
                        )
                        .entity(entity)
                        .component("Script")
                        .field("source"),
                    );
                }
            }
        }

        // Clip references resolve like mesh assets: relative to the scene
        // file, existence checked here; clip *content* is validated by the
        // scene-level animation pass so its errors carry the clip's own
        // file/line.
        ComponentData::AnimationPlayer(player) => {
            let path = &format!("{component_path}/clip");
            if player.clip.contains('#') {
                errors.push(
                    cx.err(
                        codes::ASSET_UNSUPPORTED,
                        format!(
                            "clip {:?} is a glTF fragment reference; skeletal                              clips are not yet supported — use a .anim.json                              property clip",
                            player.clip
                        ),
                        path,
                    )
                    .entity(entity)
                    .component("AnimationPlayer")
                    .field("clip"),
                );
            } else if Path::new(&player.clip).is_absolute() {
                errors.push(
                    cx.err(
                        codes::ASSET_PATH_NOT_RELATIVE,
                        format!(
                            "clip {:?} is an absolute path; clips are referenced                              relative to the scene file",
                            player.clip
                        ),
                        path,
                    )
                    .entity(entity)
                    .component("AnimationPlayer")
                    .field("clip"),
                );
            } else {
                let base_dir = Path::new(cx.file).parent().unwrap_or(Path::new(""));
                if !base_dir.join(&player.clip).is_file() {
                    errors.push(
                        cx.err(
                            codes::ASSET_NOT_FOUND,
                            format!(
                                "no clip file at {:?} (clip paths resolve relative                                  to the scene file)",
                                player.clip
                            ),
                            path,
                        )
                        .entity(entity)
                        .component("AnimationPlayer")
                        .field("clip"),
                    );
                }
            }
        }
    }

    checked
}

/// Check one component object against its schema variant: unknown keys,
/// missing required fields, JSON types, and numeric ranges. Returns whether
/// the component's *shape* is clean — range violations report errors but do
/// not make the shape unparseable, so they leave the return value true.
fn walk_component(
    cx: &Cx<'_>,
    schemas: &ComponentSchemas,
    variant: &Value,
    object: &Map<String, Value>,
    type_name: &str,
    entity: &str,
    component_path: &str,
    errors: &mut Vec<EngineError>,
) -> bool {
    let mut shape_clean = true;
    let empty = Map::new();
    let properties = variant["properties"].as_object().unwrap_or(&empty);

    for key in object.keys() {
        if key == "type" || properties.contains_key(key.as_str()) {
            continue;
        }
        shape_clean = false;
        errors.push(
            cx.err(
                codes::UNKNOWN_FIELD,
                format!("component {type_name:?} has no field {key:?}"),
                &format!("{component_path}/{key}"),
            )
            .entity(entity)
            .component(type_name)
            .field(key)
            .suggest_from(
                key,
                properties.keys().map(String::as_str).filter(|k| *k != "type"),
            ),
        );
    }

    if let Some(required) = variant["required"].as_array() {
        for field in required.iter().filter_map(Value::as_str) {
            if field != "type" && !object.contains_key(field) {
                shape_clean = false;
                errors.push(
                    cx.err(
                        codes::MISSING_FIELD,
                        format!("component {type_name:?} requires the field {field:?}"),
                        component_path,
                    )
                    .entity(entity)
                    .component(type_name)
                    .field(field),
                );
            }
        }
    }

    for (key, value) in object {
        if key == "type" {
            continue;
        }
        let Some(property) = properties.get(key.as_str()) else {
            continue; // already reported as unknown
        };
        let property = schemas.resolve(property);
        let field_path = format!("{component_path}/{key}");
        shape_clean &=
            check_value(cx, schemas, property, value, type_name, entity, key, &field_path, errors);
    }

    shape_clean
}

/// The JSON type a property schema names, looking through nullability:
/// `Option<T>` fields publish `"type": ["<T>", "null"]`. Returns the
/// non-null type and whether null is allowed.
fn schema_type(schema: &Value) -> (Option<&str>, bool) {
    if let Some(t) = schema["type"].as_str() {
        return (Some(t), false);
    }
    if let Some(types) = schema["type"].as_array() {
        let nullable = types.iter().any(|t| t == "null");
        let concrete = types
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null");
        return (concrete, nullable);
    }
    (None, false)
}

/// The closed set of strings a property schema accepts, if it is an enum.
/// schemars writes plain enums as `"enum": [...]` and doc-commented ones as
/// a `oneOf` of `const` entries; both are the same contract.
fn enum_values(schema: &Value) -> Option<Vec<&str>> {
    if let Some(values) = schema["enum"].as_array() {
        return Some(values.iter().filter_map(Value::as_str).collect());
    }
    if let Some(variants) = schema["oneOf"].as_array() {
        let consts: Vec<&str> = variants.iter().filter_map(|v| v["const"].as_str()).collect();
        if !consts.is_empty() && consts.len() == variants.len() {
            return Some(consts);
        }
    }
    None
}

/// Check one field value against its property schema. Returns whether the
/// value's shape (JSON type, array arity) is clean.
#[allow(clippy::too_many_arguments)]
fn check_value(
    cx: &Cx<'_>,
    schemas: &ComponentSchemas,
    schema: &Value,
    value: &Value,
    component: &str,
    entity: &str,
    field: &str,
    json_path: &str,
    errors: &mut Vec<EngineError>,
) -> bool {
    let (type_name, nullable) = schema_type(schema);
    if nullable && value.is_null() {
        return true;
    }
    // An enum-of-strings schema may carry no top-level "type"; it is a
    // string field with a closed vocabulary either way.
    let type_name = type_name.or_else(|| enum_values(schema).map(|_| "string"));
    match type_name {
        Some("number") => {
            let Some(number) = value.as_number() else {
                errors.push(
                    cx.wrong_type(field, "number", value, json_path)
                        .entity(entity)
                        .component(component),
                );
                return false;
            };
            check_bounds(cx, schema, number, component, entity, field, field, json_path, errors);
            true
        }

        Some("integer") => {
            // Integer fields (u32 in the component structs) are stricter than
            // "number": serde rejects fractions and out-of-format values, so
            // the walk must too — reporting them as shape errors, or the
            // final serde gate would fire `scene_parse_desync` on them.
            let integral = value
                .as_number()
                .is_some_and(|n| n.is_u64() || n.is_i64());
            if !integral {
                errors.push(
                    cx.wrong_type(field, "integer", value, json_path)
                        .entity(entity)
                        .component(component),
                );
                return false;
            }
            if schema["format"].as_str() == Some("uint32")
                && value.as_u64().is_none_or(|n| n > u64::from(u32::MAX))
            {
                errors.push(
                    cx.err(
                        codes::INVALID_FIELD_TYPE,
                        format!(
                            "{field:?} must be an unsigned 32-bit integer, found {}",
                            value
                        ),
                        json_path,
                    )
                    .entity(entity)
                    .component(component)
                    .field(field),
                );
                return false;
            }
            let number = value.as_number().expect("checked above");
            check_bounds(cx, schema, number, component, entity, field, field, json_path, errors);
            true
        }

        Some("boolean") => {
            if !value.is_boolean() {
                errors.push(
                    cx.wrong_type(field, "boolean", value, json_path)
                        .entity(entity)
                        .component(component),
                );
                return false;
            }
            true
        }

        Some("string") => {
            let Some(text) = value.as_str() else {
                errors.push(
                    cx.wrong_type(field, "string", value, json_path)
                        .entity(entity)
                        .component(component),
                );
                return false;
            };

            // Closed string enums (RigidBody.body, Collider.shape): the walk
            // must reject unknown variants itself, or serde's rejection would
            // masquerade as a desync bug. Typos get did_you_mean.
            if let Some(allowed) = enum_values(schema) {
                if !allowed.contains(&text) {
                    let (code, what) = match field {
                        "shape" => (codes::UNKNOWN_SHAPE, "collider shape"),
                        "body" => (codes::UNKNOWN_BODY_KIND, "body kind"),
                        _ => (codes::INVALID_FIELD_TYPE, "value"),
                    };
                    errors.push(
                        cx.err(
                            code,
                            format!(
                                "no {what} named {text:?}; expected one of {}",
                                allowed.join(", ")
                            ),
                            json_path,
                        )
                        .entity(entity)
                        .component(component)
                        .field(field)
                        .suggest_from(text, allowed.iter().copied()),
                    );
                    return false;
                }
            }
            true
        }

        Some("array") => {
            let Some(items) = value.as_array() else {
                errors.push(
                    cx.wrong_type(field, "array", value, json_path)
                        .entity(entity)
                        .component(component),
                );
                return false;
            };

            let len = items.len() as u64;
            let min_items = schema["minItems"].as_u64();
            let max_items = schema["maxItems"].as_u64();
            if min_items.is_some_and(|n| len < n) || max_items.is_some_and(|n| len > n) {
                // Fixed arity ([f32; 3] fields) is a shape error — serde
                // rejects the wrong length too. An open-ended bound (a Vec
                // with a minimum, like Breakable.fragments) still parses, so
                // it reports as a range violation and checking continues —
                // the walk must never be stricter than the loader about what
                // *parses* (the corpus agreement property).
                if min_items.is_some() && min_items == max_items {
                    errors.push(
                        cx.err(
                            codes::INVALID_FIELD_TYPE,
                            format!(
                                "{field:?} must be an array of exactly {} elements, found {len}",
                                min_items.unwrap_or(0)
                            ),
                            json_path,
                        )
                        .entity(entity)
                        .component(component)
                        .field(field),
                    );
                    return false;
                }
                let expected = match (min_items, max_items) {
                    (Some(a), Some(b)) => format!("between {a} and {b}"),
                    (Some(a), None) => format!("at least {a}"),
                    (None, Some(b)) => format!("at most {b}"),
                    (None, None) => unreachable!("guarded above"),
                };
                errors.push(
                    cx.err(
                        codes::VALUE_OUT_OF_RANGE,
                        format!("{field:?} must have {expected} elements, found {len}"),
                        json_path,
                    )
                    .entity(entity)
                    .component(component)
                    .field(field),
                );
            }

            let item_schema = schemas.resolve(&schema["items"]);
            let mut clean = true;
            for (i, item) in items.iter().enumerate() {
                let item_path = format!("{json_path}/{i}");
                if item_schema["type"].as_str() == Some("number") {
                    let Some(number) = item.as_number() else {
                        clean = false;
                        errors.push(
                            cx.err(
                                codes::INVALID_FIELD_TYPE,
                                format!("{field}[{i}] must be a number, found {}", kind_of(item)),
                                &item_path,
                            )
                            .entity(entity)
                            .component(component)
                            .field(field),
                        );
                        continue;
                    };
                    let label = format!("{field}[{i}]");
                    check_bounds(
                        cx, item_schema, number, component, entity, field, &label, &item_path,
                        errors,
                    );
                } else if item_schema["type"].as_str() == Some("object") {
                    // Arrays of objects (Breakable.fragments): recurse, so a
                    // bad fragment is a located walk error rather than a
                    // serde rejection masquerading as scene_parse_desync.
                    clean &= check_value(
                        cx, schemas, item_schema, item, component, entity, field, &item_path,
                        errors,
                    );
                }
            }
            clean
        }

        Some("object") => {
            let Some(map) = value.as_object() else {
                errors.push(
                    cx.wrong_type(field, "object", value, json_path)
                        .entity(entity)
                        .component(component),
                );
                return false;
            };

            let empty = Map::new();
            let properties = schema["properties"].as_object().unwrap_or(&empty);
            let mut clean = true;

            for key in map.keys() {
                if !properties.contains_key(key.as_str()) {
                    clean = false;
                    errors.push(
                        cx.err(
                            codes::UNKNOWN_FIELD,
                            format!("{field:?} entries have no field {key:?}"),
                            &format!("{json_path}/{key}"),
                        )
                        .entity(entity)
                        .component(component)
                        .field(key)
                        .suggest_from(key, properties.keys().map(String::as_str)),
                    );
                }
            }

            if let Some(required) = schema["required"].as_array() {
                for req in required.iter().filter_map(Value::as_str) {
                    if !map.contains_key(req) {
                        clean = false;
                        errors.push(
                            cx.err(
                                codes::MISSING_FIELD,
                                format!("each {field:?} entry requires the field {req:?}"),
                                json_path,
                            )
                            .entity(entity)
                            .component(component)
                            .field(req),
                        );
                    }
                }
            }

            for (key, item) in map {
                let Some(property) = properties.get(key.as_str()) else {
                    continue; // already reported as unknown
                };
                let property = schemas.resolve(property);
                clean &= check_value(
                    cx,
                    schemas,
                    property,
                    item,
                    component,
                    entity,
                    key,
                    &format!("{json_path}/{key}"),
                    errors,
                );
            }

            clean
        }

        // A property kind the walk does not know how to check — leave it to
        // the serde gate rather than guessing.
        _ => true,
    }
}

/// Emit `value_out_of_range` when `number` violates the bounds its property
/// schema declares (`minimum`/`maximum`/`exclusiveMinimum`/`exclusiveMaximum`).
/// One error per offending value — an agent fixing `albedo: [1.5, 2.0, 0.5]`
/// should learn about both bad channels in one run.
#[allow(clippy::too_many_arguments)]
fn check_bounds(
    cx: &Cx<'_>,
    schema: &Value,
    number: &serde_json::Number,
    component: &str,
    entity: &str,
    field: &str,
    label: &str,
    json_path: &str,
    errors: &mut Vec<EngineError>,
) {
    let Some(v) = number.as_f64() else { return };
    let min = schema["minimum"].as_f64();
    let max = schema["maximum"].as_f64();
    let emin = schema["exclusiveMinimum"].as_f64();
    let emax = schema["exclusiveMaximum"].as_f64();

    let violated = min.is_some_and(|b| v < b)
        || max.is_some_and(|b| v > b)
        || emin.is_some_and(|b| v <= b)
        || emax.is_some_and(|b| v >= b);
    if !violated {
        return;
    }

    let requirement = match (min, max, emin, emax) {
        (Some(lo), Some(hi), None, None) => {
            format!("the allowed range is [{}, {}]", fmt_num(lo), fmt_num(hi))
        }
        (None, None, Some(lo), Some(hi)) => format!(
            "it must be greater than {} and less than {}",
            fmt_num(lo),
            fmt_num(hi)
        ),
        _ => {
            let mut clauses = Vec::new();
            if let Some(lo) = min {
                clauses.push(format!("it must be at least {}", fmt_num(lo)));
            }
            if let Some(lo) = emin {
                clauses.push(format!("it must be greater than {}", fmt_num(lo)));
            }
            if let Some(hi) = max {
                clauses.push(format!("it must be at most {}", fmt_num(hi)));
            }
            if let Some(hi) = emax {
                clauses.push(format!("it must be less than {}", fmt_num(hi)));
            }
            clauses.join(" and ")
        }
    };

    errors.push(
        cx.err(
            codes::VALUE_OUT_OF_RANGE,
            format!("{component}.{label} is {}; {requirement}", fmt_num(v)),
            json_path,
        )
        .entity(entity)
        .component(component)
        .field(field),
    );
}

/// Format a bound or value the way `{}` formats an `f32`: integral values
/// without a trailing `.0`, so messages read "at least 0", not "at least 0.0".
fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.is_finite() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Validate the scene-level `physics` block by hand (the top-level walk is
/// hand-written; the block is small enough to keep it that way).
fn check_physics_block(cx: &Cx<'_>, physics: &Value, errors: &mut Vec<EngineError>) {
    let Some(object) = physics.as_object() else {
        errors.push(cx.wrong_type("physics", "object", physics, "/physics"));
        return;
    };

    for key in object.keys() {
        if key != "gravity" && key != "timestep_hz" {
            errors.push(
                cx.err(
                    codes::UNKNOWN_FIELD,
                    format!("the physics block has no field {key:?}"),
                    &format!("/physics/{key}"),
                )
                .field(key)
                .suggest_from(key, ["gravity", "timestep_hz"]),
            );
        }
    }

    if let Some(gravity) = object.get("gravity") {
        match gravity.as_array() {
            Some(items) if items.len() == 3 && items.iter().all(Value::is_number) => {}
            _ => errors.push(
                cx.wrong_type("gravity", "array", gravity, "/physics/gravity")
                    .field("gravity"),
            ),
        }
    }

    if let Some(hz) = object.get("timestep_hz") {
        let valid = hz.as_u64().is_some_and(|v| v >= 1);
        if !valid {
            errors.push(
                cx.err(
                    codes::INVALID_PHYSICS_VALUE,
                    format!(
                        "physics.timestep_hz is {hz}; it must be an integer of at least 1"
                    ),
                    "/physics/timestep_hz",
                )
                .field("timestep_hz"),
            );
        }
    }
}

/// Validate the scene-level `environment` block (M16), hand-written like
/// [`check_physics_block`] and for the same reason.
fn check_environment_block(cx: &Cx<'_>, environment: &Value, errors: &mut Vec<EngineError>) {
    const COLORS: [&str; 3] = ["sky_zenith", "sky_horizon", "sky_ground"];
    const FLAGS: [&str; 2] = ["sky", "shadows"];
    const KNOWN: [&str; 8] = [
        "sky",
        "sky_zenith",
        "sky_horizon",
        "sky_ground",
        "fog_density",
        "shadows",
        "shadow_distance",
        "samples",
    ];

    let Some(object) = environment.as_object() else {
        errors.push(cx.wrong_type("environment", "object", environment, "/environment"));
        return;
    };

    for key in object.keys() {
        if !KNOWN.contains(&key.as_str()) {
            errors.push(
                cx.err(
                    codes::UNKNOWN_FIELD,
                    format!("the environment block has no field {key:?}"),
                    &format!("/environment/{key}"),
                )
                .field(key)
                .suggest_from(key, KNOWN),
            );
        }
    }

    for name in FLAGS {
        if let Some(value) = object.get(name) {
            if !value.is_boolean() {
                errors.push(
                    cx.wrong_type(name, "boolean", value, &format!("/environment/{name}"))
                        .field(name),
                );
            }
        }
    }

    for name in COLORS {
        if let Some(value) = object.get(name) {
            let ok = value
                .as_array()
                .is_some_and(|items| items.len() == 3 && items.iter().all(Value::is_number));
            if !ok {
                errors.push(
                    cx.wrong_type(name, "array", value, &format!("/environment/{name}"))
                        .field(name),
                );
            }
        }
    }

    if let Some(density) = object.get("fog_density") {
        let valid = density.as_f64().is_some_and(|v| v >= 0.0 && v.is_finite());
        if !valid {
            errors.push(
                cx.err(
                    codes::INVALID_ENVIRONMENT_VALUE,
                    format!("environment.fog_density is {density}; it must be a number >= 0"),
                    "/environment/fog_density",
                )
                .field("fog_density"),
            );
        }
    }

    if let Some(distance) = object.get("shadow_distance") {
        let valid = distance.as_f64().is_some_and(|v| v > 0.0 && v.is_finite());
        if !valid {
            errors.push(
                cx.err(
                    codes::INVALID_ENVIRONMENT_VALUE,
                    format!(
                        "environment.shadow_distance is {distance}; it must be a number greater than 0"
                    ),
                    "/environment/shadow_distance",
                )
                .field("shadow_distance"),
            );
        }
    }

    // 1 or 4 and nothing between: every other count would need its own set of
    // pipelines, and a scene asking for 2 should be told so rather than
    // silently rounded to something it did not write.
    if let Some(samples) = object.get("samples") {
        let valid = samples.as_u64().is_some_and(|v| v == 1 || v == 4);
        if !valid {
            errors.push(
                cx.err(
                    codes::INVALID_ENVIRONMENT_VALUE,
                    format!("environment.samples is {samples}; it must be 1 or 4"),
                    "/environment/samples",
                )
                .field("samples"),
            );
        }
    }
}

/// The scene-level `daylight` block (M21), hand-validated like `physics` and
/// `environment` rather than walked from the schema.
///
/// `environment` comes in so the `daylight_overrides_sky` warning can see
/// whether the scene also authored sky colors that nothing will read.
fn check_daylight_block(
    cx: &Cx<'_>,
    daylight: &Value,
    environment: Option<&Value>,
    errors: &mut Vec<EngineError>,
) {
    const FLAGS: [&str; 2] = ["drives_sun", "drives_sky"];
    const KNOWN: [&str; 10] = [
        "time_of_day",
        "day_length",
        "sun_elevation",
        "sun_azimuth",
        "moon_elevation",
        "moon_color",
        "moon_intensity",
        "drives_sun",
        "drives_sky",
        "palette",
    ];

    let Some(object) = daylight.as_object() else {
        errors.push(cx.wrong_type("daylight", "object", daylight, "/daylight"));
        return;
    };

    for key in object.keys() {
        if !KNOWN.contains(&key.as_str()) {
            errors.push(
                cx.err(
                    codes::UNKNOWN_FIELD,
                    format!("the daylight block has no field {key:?}"),
                    &format!("/daylight/{key}"),
                )
                .field(key)
                .suggest_from(key, KNOWN),
            );
        }
    }

    for name in FLAGS {
        if let Some(value) = object.get(name) {
            if !value.is_boolean() {
                errors.push(
                    cx.wrong_type(name, "boolean", value, &format!("/daylight/{name}"))
                        .field(name),
                );
            }
        }
    }

    // (field, low, high, low is exclusive, high is exclusive, prose)
    let ranges: [(&str, f64, f64, bool, bool, &str); 6] = [
        ("time_of_day", 0.0, 24.0, false, true, "hours in [0, 24)"),
        ("day_length", 0.0, f64::INFINITY, false, false, "a number >= 0"),
        ("sun_elevation", 0.0, 90.0, true, false, "degrees in (0, 90]"),
        ("sun_azimuth", f64::NEG_INFINITY, f64::INFINITY, false, false, "a finite number of degrees"),
        ("moon_elevation", 0.0, 90.0, true, false, "degrees in (0, 90]"),
        ("moon_intensity", 0.0, f64::INFINITY, false, false, "a number >= 0"),
    ];

    for (name, low, high, low_exclusive, high_exclusive, prose) in ranges {
        let Some(value) = object.get(name) else {
            continue;
        };
        let ok = value.as_f64().is_some_and(|v| {
            v.is_finite()
                && (if low_exclusive { v > low } else { v >= low })
                && (if high_exclusive { v < high } else { v <= high })
        });
        if !ok {
            errors.push(
                cx.err(
                    codes::INVALID_DAYLIGHT_VALUE,
                    format!("daylight.{name} is {value}; it must be {prose}"),
                    &format!("/daylight/{name}"),
                )
                .field(name),
            );
        }
    }

    if let Some(color) = object.get("moon_color") {
        check_daylight_color(cx, color, "moon_color", "/daylight/moon_color", true, errors);
    }

    if let Some(palette) = object.get("palette") {
        check_daylight_palette(cx, palette, errors);
    }

    // A scene that authored sky bands and left `drives_sky` on has written
    // values nothing will ever read — the `unused_material` precedent, and the
    // fix (`drives_sky: false`) goes in the message.
    let drives_sky = object
        .get("drives_sky")
        .map_or(true, |v| v.as_bool().unwrap_or(true));
    if drives_sky {
        let authored: Vec<&str> = environment
            .and_then(Value::as_object)
            .map(|env| {
                ["sky_zenith", "sky_horizon", "sky_ground"]
                    .into_iter()
                    .filter(|band| env.contains_key(*band))
                    .collect()
            })
            .unwrap_or_default();

        if !authored.is_empty() {
            errors.push(
                cx.err(
                    codes::DAYLIGHT_OVERRIDES_SKY,
                    format!(
                        "daylight computes the sky, so environment.{} {} never read; \
                         set daylight.drives_sky to false to keep the authored colors",
                        authored.join(", environment."),
                        if authored.len() == 1 { "is" } else { "are" },
                    ),
                    "/daylight/drives_sky",
                )
                .field("drives_sky")
                .candidates(authored)
                .warning(),
            );
        }
    }
}

/// A linear-RGB triple in a hand-validated block. `clamped` distinguishes a
/// chromaticity (`[0, 1]`) from a sky band, which is a light source and is
/// deliberately unbounded above.
fn check_daylight_color(
    cx: &Cx<'_>,
    value: &Value,
    field: &str,
    path: &str,
    clamped: bool,
    errors: &mut Vec<EngineError>,
) {
    let Some(items) = value.as_array().filter(|items| items.len() == 3) else {
        errors.push(cx.wrong_type(field, "array", value, path).field(field));
        return;
    };

    for (channel, item) in items.iter().enumerate() {
        let ok = item
            .as_f64()
            .is_some_and(|v| v.is_finite() && v >= 0.0 && (!clamped || v <= 1.0));
        if !ok {
            errors.push(
                cx.err(
                    codes::INVALID_DAYLIGHT_VALUE,
                    format!(
                        "{field}[{channel}] is {item}; it must be a number in {}",
                        if clamped { "[0, 1]" } else { "[0, ∞)" }
                    ),
                    &format!("{path}/{channel}"),
                )
                .field(field),
            );
        }
    }
}

/// The palette table: at least two keyframes, strictly increasing hours, and
/// every field of every keyframe present.
///
/// Requiring all nine fields is deliberate — a half-specified keyframe
/// silently interpolating toward black is a worse failure than being told to
/// finish it.
fn check_daylight_palette(cx: &Cx<'_>, palette: &Value, errors: &mut Vec<EngineError>) {
    const COLORS: [&str; 4] = ["sun_color", "ambient_color", "sky_zenith", "sky_ground"];
    const REQUIRED: [&str; 9] = [
        "hour",
        "sun_color",
        "sun_intensity",
        "ambient_color",
        "ambient_intensity",
        "sky_zenith",
        "sky_horizon",
        "sky_ground",
        "fog_scale",
    ];

    let Some(keys) = palette.as_array() else {
        errors.push(cx.wrong_type("palette", "array", palette, "/daylight/palette"));
        return;
    };

    if keys.len() < 2 {
        errors.push(
            cx.err(
                codes::DAYLIGHT_PALETTE_INVALID,
                format!(
                    "daylight.palette holds {} keyframe(s); it needs at least 2, \
                     because a day is interpolated between them",
                    keys.len()
                ),
                "/daylight/palette",
            )
            .field("palette"),
        );
        return;
    }

    let mut previous_hour: Option<f64> = None;

    for (index, key) in keys.iter().enumerate() {
        let key_path = format!("/daylight/palette/{index}");

        let Some(object) = key.as_object() else {
            errors.push(cx.wrong_type("palette", "object", key, &key_path));
            continue;
        };

        for name in REQUIRED {
            if !object.contains_key(name) {
                errors.push(
                    cx.err(
                        codes::MISSING_FIELD,
                        format!("daylight palette keyframe {index} has no {name:?}"),
                        &key_path,
                    )
                    .field(name),
                );
            }
        }

        for key_name in object.keys() {
            if !REQUIRED.contains(&key_name.as_str()) {
                errors.push(
                    cx.err(
                        codes::UNKNOWN_FIELD,
                        format!("a daylight palette keyframe has no field {key_name:?}"),
                        &format!("{key_path}/{key_name}"),
                    )
                    .field(key_name)
                    .suggest_from(key_name, REQUIRED),
                );
            }
        }

        for name in COLORS.into_iter().chain(["sky_horizon"]) {
            if let Some(color) = object.get(name) {
                // Sky bands are light sources and are unbounded above; the
                // sun and ambient carry their magnitude in an intensity, so
                // their colors are chromaticities in [0, 1].
                let clamped = name == "sun_color" || name == "ambient_color";
                check_daylight_color(
                    cx,
                    color,
                    name,
                    &format!("{key_path}/{name}"),
                    clamped,
                    errors,
                );
            }
        }

        for name in ["sun_intensity", "ambient_intensity", "fog_scale"] {
            if let Some(value) = object.get(name) {
                let ok = value.as_f64().is_some_and(|v| v.is_finite() && v >= 0.0);
                if !ok {
                    errors.push(
                        cx.err(
                            codes::INVALID_DAYLIGHT_VALUE,
                            format!("palette keyframe {index}: {name} is {value}; it must be >= 0"),
                            &format!("{key_path}/{name}"),
                        )
                        .field(name),
                    );
                }
            }
        }

        let Some(hour) = object.get("hour") else {
            continue;
        };
        let Some(hour) = hour
            .as_f64()
            .filter(|v| v.is_finite() && (0.0..24.0).contains(v))
        else {
            errors.push(
                cx.err(
                    codes::INVALID_DAYLIGHT_VALUE,
                    format!("palette keyframe {index}: hour is {hour}; it must be in [0, 24)"),
                    &format!("{key_path}/hour"),
                )
                .field("hour"),
            );
            continue;
        };

        // Sorted, because the table wraps: an unsorted palette has no
        // well-defined "next keyframe" and would interpolate backwards
        // through the day rather than failing.
        if let Some(previous) = previous_hour {
            if hour <= previous {
                errors.push(
                    cx.err(
                        codes::DAYLIGHT_PALETTE_INVALID,
                        format!(
                            "palette keyframe {index} is at hour {hour}, not after the \
                             previous keyframe's {previous}; hours must strictly increase"
                        ),
                        &format!("{key_path}/hour"),
                    )
                    .field("hour"),
                );
            }
        }
        previous_hour = Some(hour);
    }
}

fn article(word: &str) -> &'static str {
    match word.chars().next() {
        Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
        _ => "a",
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes_of(source: &str) -> Vec<&'static str> {
        validate_source(source, "test.json")
            .into_iter()
            .map(|e| e.error)
            .collect()
    }

    const VALID: &str = r#"{
      "name": "demo",
      "entities": [
        { "name": "Player", "components": [ { "type": "Camera", "active": true } ] },
        { "name": "Cube1",  "components": [ { "type": "Mesh", "asset": "builtin:cube" } ] }
      ]
    }"#;

    #[test]
    fn accepts_a_valid_scene() {
        assert!(validate_source(VALID, "test.json").is_empty());
    }

    #[test]
    fn reports_syntax_errors_with_a_line() {
        let errors = validate_source("{\n  \"name\": \"x\",\n  oops\n}", "test.json");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error, "invalid_json");
        assert_eq!(errors[0].context().unwrap().line, Some(3));
    }

    #[test]
    fn every_semantic_error_carries_file_line_and_path() {
        // Invariant 6 as restated in CLAUDE.md: an error an agent cannot
        // locate from the payload alone is a bug. One scene, many distinct
        // errors — all of them must know where they are, both for humans
        // (line) and for jq (path).
        let source = r#"{
          "name": "s",
          "entities": [
            { "name": "A", "components": [ { "type": "Meterial" } ] },
            { "name": "A", "components": [ { "type": "Transform", "postion": [0, 1, 0] } ] },
            { "name": "C", "components": [ { "type": "Mesh" } ] },
            { "name": "D", "components": [ { "type": "Mesh", "asset": "meshes/x.glb" } ] },
            { "name": "E", "components": [ { "type": "Camera", "active": true } ] },
            { "name": "F", "components": [ { "type": "Camera", "active": true } ] }
          ]
        }"#;
        let errors = validate_source(source, "scene.json");
        assert!(errors.len() >= 5, "expected a pile of errors");
        for error in &errors {
            let context = error
                .context()
                .unwrap_or_else(|| panic!("{} has no context at all", error.error));
            assert_eq!(
                context.file.as_deref(),
                Some("scene.json"),
                "{}",
                error.error
            );
            assert!(
                context.line.is_some(),
                "{} carries no line: {}",
                error.error,
                error.to_json()
            );
            assert!(
                context.path.is_some(),
                "{} carries no JSON pointer: {}",
                error.error,
                error.to_json()
            );
        }
    }

    #[test]
    fn paths_are_jq_addressable_json_pointers() {
        let source = r#"{"name":"s","entities":[
            {"name":"Cube1","components":[{"type":"Transform","postion":[0,1,0]}]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(
            errors[0].context().unwrap().path.as_deref(),
            Some("/entities/0/components/0/postion")
        );
    }

    #[test]
    fn locates_the_error_on_the_right_line() {
        let source = "{\n\"name\": \"s\",\n\"entities\": [\n{ \"name\": \"A\",\n  \"components\": [\n    { \"type\": \"Meterial\" }\n  ] }\n]\n}";
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].context().unwrap().line, Some(6));
    }

    #[test]
    fn suggests_the_right_component_name() {
        let source = r#"{"name":"s","entities":[
            {"name":"Cube1","components":[{"type":"Meterial","albedo":[1,0,0]}]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1);

        let context = errors[0].context().unwrap();
        assert_eq!(errors[0].error, "unknown_component");
        assert_eq!(context.entity.as_deref(), Some("Cube1"));
        assert_eq!(context.did_you_mean.as_deref(), Some("Material"));
    }

    #[test]
    fn suggests_the_right_field_name() {
        let source = r#"{"name":"s","entities":[
            {"name":"Cube1","components":[{"type":"Transform","postion":[0,1,0]}]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1);

        let context = errors[0].context().unwrap();
        assert_eq!(errors[0].error, "unknown_field");
        assert_eq!(context.field.as_deref(), Some("postion"));
        assert_eq!(context.did_you_mean.as_deref(), Some("position"));
    }

    #[test]
    fn reports_a_required_field_that_is_absent() {
        let source = r#"{"name":"s","entities":[
            {"name":"Cube1","components":[{"type":"Mesh"}]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors[0].error, "missing_field");
        assert_eq!(errors[0].context().unwrap().field.as_deref(), Some("asset"));
    }

    #[test]
    fn reports_a_field_of_the_wrong_json_type() {
        let source = r#"{"name":"s","entities":[
            {"name":"Cube1","components":[{"type":"Mesh","asset":42}]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].error, "invalid_field_type");
        assert_eq!(errors[0].context().unwrap().field.as_deref(), Some("asset"));
    }

    #[test]
    fn reports_a_wrong_arity_vector() {
        let source = r#"{"name":"s","entities":[
            {"name":"Cube1","components":[{"type":"Transform","position":[0,1]}]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].error, "invalid_field_type");
        assert!(errors[0].message.contains("exactly 3"), "{}", errors[0].message);
    }

    #[test]
    fn reports_a_non_numeric_vector_element() {
        let source = r#"{"name":"s","entities":[
            {"name":"Cube1","components":[{"type":"Transform","position":[0,"one",2]}]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].error, "invalid_field_type");
        assert!(
            errors[0].message.contains("position[1]"),
            "{}",
            errors[0].message
        );
        assert_eq!(
            errors[0].context().unwrap().path.as_deref(),
            Some("/entities/0/components/0/position/1")
        );
    }

    #[test]
    fn rejects_an_unresolvable_mesh_asset_at_validation_time() {
        // Design doc §5: never a silent fallback. This mesh file does not
        // exist next to the scene, so validation must say so.
        let source = r#"{"name":"s","entities":[
            {"name":"Cube1","components":[{"type":"Mesh","asset":"meshes/cube.glb"}]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error, "asset_not_found");

        let context = errors[0].context().unwrap();
        assert_eq!(context.entity.as_deref(), Some("Cube1"));
        assert_eq!(context.field.as_deref(), Some("asset"));
    }

    #[test]
    fn accepts_a_mesh_file_that_exists_next_to_the_scene() {
        // Asset paths resolve relative to the scene file, so validation of the
        // same source succeeds or fails with the scene's location.
        let dir = std::env::temp_dir().join(format!("engine-validate-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("meshes")).unwrap();
        std::fs::write(dir.join("meshes/thing.gltf"), b"{}").unwrap();

        let source = r#"{"name":"s","entities":[
            {"name":"Thing","components":[{"type":"Mesh","asset":"meshes/thing.gltf"}]}
        ]}"#;
        let scene_path = dir.join("scene.json").display().to_string();
        let errors = validate_source(source, &scene_path);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(errors.is_empty(), "{:?}", errors);
    }

    #[test]
    fn rejects_a_mesh_format_the_loader_does_not_read() {
        let source = r#"{"name":"s","entities":[
            {"name":"Cube1","components":[{"type":"Mesh","asset":"meshes/cube.obj"}]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error, "asset_unsupported");
    }

    #[test]
    fn suggests_a_near_miss_builtin_asset() {
        let source = r#"{"name":"s","entities":[
            {"name":"Cube1","components":[{"type":"Mesh","asset":"builtin:cuve"}]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors[0].error, "asset_not_found");
        assert_eq!(
            errors[0].context().unwrap().did_you_mean.as_deref(),
            Some("builtin:cube")
        );
    }

    #[test]
    fn rejects_more_than_one_directional_or_ambient_light() {
        let source = r#"{"name":"s","entities":[
            {"name":"SunA","components":[{"type":"DirectionalLight"}]},
            {"name":"SunB","components":[{"type":"DirectionalLight"}]},
            {"name":"FillA","components":[{"type":"AmbientLight"}]},
            {"name":"FillB","components":[{"type":"AmbientLight"}]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 2, "{errors:?}");

        let sun = errors
            .iter()
            .find(|e| e.error == "multiple_directional_lights")
            .expect("surplus sun should be flagged");
        assert_eq!(
            sun.context().unwrap().candidates,
            Some(vec!["SunA".to_string(), "SunB".to_string()])
        );
        assert!(
            sun.context().unwrap().line.is_some(),
            "must point at the surplus component"
        );

        assert!(errors.iter().any(|e| e.error == "multiple_ambient_lights"));
    }

    #[test]
    fn allows_several_point_lights_but_not_more_than_the_shader_holds() {
        // Point lights are the one light component that is plural, so the
        // "more than one" rule must NOT apply to them…
        let mut entities: Vec<String> = (0..crate::components::MAX_POINT_LIGHTS)
            .map(|i| {
                format!(
                    r#"{{"name":"Lamp{i}","components":[
                        {{"type":"Transform","position":[{i}.0,1.0,0.0]}},
                        {{"type":"PointLight","intensity":2.0}}
                    ]}}"#
                )
            })
            .collect();
        let source = format!(r#"{{"name":"s","entities":[{}]}}"#, entities.join(","));
        let errors = validate_source(&source, "s.json");
        assert!(
            errors.is_empty(),
            "{} point lights must be fine, got {errors:?}",
            crate::components::MAX_POINT_LIGHTS
        );

        // …but the shader's array is fixed-size, and one past it is an error
        // rather than a light that silently never shines.
        entities.push(
            r#"{"name":"Surplus","components":[
                {"type":"Transform"},
                {"type":"PointLight"}
            ]}"#
            .to_string(),
        );
        let source = format!(r#"{{"name":"s","entities":[{}]}}"#, entities.join(","));
        let errors = validate_source(&source, "s.json");
        let surplus = errors
            .iter()
            .find(|e| e.error == "too_many_point_lights")
            .expect("the ninth point light must be reported");
        assert!(
            surplus.context().unwrap().line.is_some(),
            "must point at the surplus component"
        );
    }

    #[test]
    fn rejects_out_of_range_point_light_values() {
        let source = r#"{"name":"s","entities":[
            {"name":"Lamp","components":[
                {"type":"Transform"},
                {"type":"PointLight","intensity":-1.0,"range":0.0,"color":[2.0,0.0,0.0]}
            ]}
        ]}"#;
        let errors = validate_source(source, "s.json");
        let fields: Vec<&str> = errors
            .iter()
            .filter(|e| e.error == "value_out_of_range")
            .filter_map(|e| e.context().and_then(|c| c.field.as_deref()))
            .collect();
        // All three at once, the M5 rule: which command you ran must never
        // change what you learn about a broken scene.
        for expected in ["intensity", "range", "color"] {
            assert!(
                fields.contains(&expected),
                "expected {expected} out of range, got {fields:?}"
            );
        }
    }

    #[test]
    fn rejects_an_unknown_particle_blend_with_a_suggestion() {
        // `blend` is the ParticleEmitter's first closed-vocabulary field, so it
        // rides the same enum path `RigidBody.body` and `Collider.shape` do —
        // including the typo suggestion, which only works while
        // `ParticleBlend`'s variants stay undocumented (see components.rs).
        let source = r#"{"name":"s","entities":[
            {"name":"Puff","components":[
                {"type":"Transform"},
                {"type":"ParticleEmitter","blend":"addative"}
            ]}
        ]}"#;
        let errors = validate_source(source, "s.json");
        let bad = errors
            .iter()
            .find(|e| e.context().and_then(|c| c.field.as_deref()) == Some("blend"))
            .expect("an unknown blend mode must be reported");
        assert_eq!(
            bad.context().unwrap().did_you_mean.as_deref(),
            Some("additive"),
            "a near-miss blend mode should be suggested"
        );
    }

    #[test]
    fn accepts_one_sun_and_one_ambient() {
        let source = r#"{"name":"s","entities":[
            {"name":"Sun","components":[
                {"type":"Transform","rotation":[-50.0,30.0,0.0]},
                {"type":"DirectionalLight","color":[1.0,1.0,1.0],"intensity":1.0}
            ]},
            {"name":"Fill","components":[{"type":"AmbientLight","intensity":0.05}]}
        ]}"#;
        assert!(codes_of(source).is_empty());
    }

    #[test]
    fn rejects_out_of_range_material_and_light_values() {
        // One run reports every violation: both bad albedo channels, the bad
        // roughness, and the negative intensity.
        let source = r#"{"name":"s","entities":[
            {"name":"Bad","components":[
                {"type":"Mesh","asset":"builtin:cube"},
                {"type":"Material","albedo":[1.5,-0.25,0.5],"roughness":1.5},
                {"type":"DirectionalLight","intensity":-2.0}
            ]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        let out_of_range: Vec<_> = errors
            .iter()
            .filter(|e| e.error == "value_out_of_range")
            .collect();
        assert_eq!(out_of_range.len(), 4, "{errors:?}");

        for error in &out_of_range {
            let context = error.context().unwrap();
            assert_eq!(context.entity.as_deref(), Some("Bad"));
            assert!(context.line.is_some(), "{}", error.to_json());
        }

        let albedo_messages: Vec<&str> = out_of_range
            .iter()
            .filter(|e| e.context().unwrap().field.as_deref() == Some("albedo"))
            .map(|e| e.message.as_str())
            .collect();
        assert_eq!(albedo_messages.len(), 2, "both bad channels reported");
        assert!(albedo_messages.iter().any(|m| m.contains("albedo[0] is 1.5")));
        assert!(
            albedo_messages
                .iter()
                .any(|m| m.contains("the allowed range is [0, 1]")),
            "{albedo_messages:?}"
        );

        assert!(out_of_range
            .iter()
            .any(|e| e.context().unwrap().field.as_deref() == Some("intensity")
                && e.message.contains("at least 0")));
    }

    #[test]
    fn rejects_camera_values_the_projection_cannot_survive() {
        // fov 0, negative near, far below near: all validate today upstream
        // of M5 and render garbage or nothing. Gap 2 closed.
        let source = r#"{"name":"s","entities":[
            {"name":"EyeA","components":[{"type":"Camera","fov":0.0,"active":true}]},
            {"name":"EyeB","components":[{"type":"Camera","near":-1.0}]},
            {"name":"EyeC","components":[{"type":"Camera","near":0.1,"far":0.05}]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        let ranges: Vec<_> = errors
            .iter()
            .filter(|e| e.error == "value_out_of_range")
            .collect();
        assert_eq!(ranges.len(), 3, "{errors:?}");

        assert!(ranges
            .iter()
            .any(|e| e.message.contains("fov is 0")
                && e.message.contains("greater than 0 and less than 180")));
        assert!(ranges
            .iter()
            .any(|e| e.message.contains("near is -1") && e.message.contains("greater than 0")));
        assert!(ranges
            .iter()
            .any(|e| e.message.contains("far is 0.05")
                && e.message.contains("greater than near (0.1)")));
    }

    #[test]
    fn rejects_a_duplicate_component() {
        let source = r#"{"name":"s","entities":[
            {"name":"Cube1","components":[
                {"type":"Mesh","asset":"builtin:cube"},
                {"type":"Transform","position":[0,1,0]},
                {"type":"Transform","position":[0,2,0]}
            ]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].error, "duplicate_component");

        let context = errors[0].context().unwrap();
        assert_eq!(context.entity.as_deref(), Some("Cube1"));
        assert_eq!(context.component.as_deref(), Some("Transform"));
        assert_eq!(
            context.path.as_deref(),
            Some("/entities/0/components/2"),
            "must point at the surplus copy"
        );
    }

    #[test]
    fn warns_about_a_material_with_no_mesh() {
        let source = r#"{"name":"s","entities":[
            {"name":"Oops","components":[{"type":"Material","albedo":[1,0,0]}]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].error, "unused_material");
        assert!(errors[0].is_warning(), "must carry severity: warning");
        assert!(
            errors[0].context().unwrap().line.is_some(),
            "warnings carry full context too"
        );
    }

    #[test]
    fn warns_about_a_zero_scale_axis() {
        let source = r#"{"name":"s","entities":[
            {"name":"Flat","components":[
                {"type":"Transform","scale":[1.0,0.0,1.0]},
                {"type":"Mesh","asset":"builtin:cube"}
            ]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].error, "zero_scale");
        assert!(errors[0].is_warning());
        assert_eq!(
            errors[0].context().unwrap().path.as_deref(),
            Some("/entities/0/components/0/scale")
        );
    }

    #[test]
    fn warnings_do_not_hide_errors_and_vice_versa() {
        let source = r#"{"name":"s","entities":[
            {"name":"Oops","components":[{"type":"Material","metallic":2.0}]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        let codes: Vec<&str> = errors.iter().map(|e| e.error).collect();
        assert!(codes.contains(&"value_out_of_range"), "{codes:?}");
        assert!(codes.contains(&"unused_material"), "{codes:?}");
    }

    #[test]
    fn range_violations_do_not_mask_other_checks() {
        // A component with a bad value AND a surplus camera both report; a
        // range violation must also not stop the camera flags from being
        // collected, or two active cameras with bad fovs would sneak through.
        let source = r#"{"name":"s","entities":[
            {"name":"A","components":[{"type":"Camera","fov":200.0,"active":true}]},
            {"name":"B","components":[{"type":"Camera","active":true}]}
        ]}"#;
        let codes = codes_of(source);
        assert!(codes.contains(&"value_out_of_range"), "{codes:?}");
        assert!(codes.contains(&"multiple_active_cameras"), "{codes:?}");
    }

    #[test]
    fn suggests_the_right_light_component_name() {
        // The m4 verification scene's error path: a misspelled light.
        let source = r#"{"name":"s","entities":[
            {"name":"Sun","components":[{"type":"DirectionelLight"}]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error, "unknown_component");
        assert_eq!(
            errors[0].context().unwrap().did_you_mean.as_deref(),
            Some("DirectionalLight")
        );
    }

    const PHYSICS_VALID: &str = r#"{"name":"p","physics":{"gravity":[0.0,-9.81,0.0],"timestep_hz":60},"entities":[
        {"name":"Ground","components":[
            {"type":"Transform","scale":[10.0,1.0,10.0]},
            {"type":"Collider","shape":"cuboid","half_extents":[5.0,0.05,5.0]}
        ]},
        {"name":"Cube","components":[
            {"type":"Transform","position":[0.0,5.0,0.0]},
            {"type":"RigidBody","body":"dynamic"},
            {"type":"Collider","shape":"cuboid","half_extents":[0.5,0.5,0.5]}
        ]}
    ]}"#;

    #[test]
    fn accepts_a_valid_physics_scene() {
        assert!(codes_of(PHYSICS_VALID).is_empty(), "{:?}", validate_source(PHYSICS_VALID, "t"));
    }

    #[test]
    fn suggests_shape_and_body_kind_typos() {
        let source = r#"{"name":"s","entities":[
            {"name":"A","components":[
                {"type":"Transform"},
                {"type":"RigidBody","body":"dynmaic"},
                {"type":"Collider","shape":"cubiod","half_extents":[0.5,0.5,0.5]}
            ]}
        ]}"#;
        let errors = validate_source(source, "test.json");

        let body = errors.iter().find(|e| e.error == "unknown_body_kind").unwrap();
        assert_eq!(body.context().unwrap().did_you_mean.as_deref(), Some("dynamic"));

        let shape = errors.iter().find(|e| e.error == "unknown_shape").unwrap();
        assert_eq!(shape.context().unwrap().did_you_mean.as_deref(), Some("cuboid"));
    }

    #[test]
    fn hud_components_validate_through_the_schema() {
        // Anchor typo: the generic enum path, with did_you_mean. Ranges:
        // size >= 4, colors in [0, 1], opacity in [0, 1], rect size >= 0 —
        // all authored as schemars attributes, all caught by the walk.
        let source = r#"{"name":"s","entities":[
            {"name":"Label","components":[
                {"type":"HudText","text":"HI","anchor":"top_lft","size":2.0,"color":[2.0,0.0,0.0]}
            ]},
            {"name":"Bar","components":[
                {"type":"HudRect","size":[-1.0,5.0],"opacity":1.5}
            ]}
        ]}"#;
        let errors = validate_source(source, "test.json");

        let anchor = errors
            .iter()
            .find(|e| e.context().and_then(|c| c.field.as_deref()) == Some("anchor"))
            .unwrap();
        assert_eq!(anchor.error, "invalid_field_type");
        assert_eq!(anchor.context().unwrap().did_you_mean.as_deref(), Some("top_left"));

        let range_fields: Vec<&str> = errors
            .iter()
            .filter(|e| e.error == "value_out_of_range")
            .filter_map(|e| e.context().and_then(|c| c.path.as_deref()))
            .collect();
        for expected in ["size", "color/0", "size/0", "opacity"] {
            assert!(
                range_fields.iter().any(|p| p.ends_with(expected)),
                "missing range error for {expected}: {range_fields:?}"
            );
        }

        // A well-formed pair of HUD components validates clean.
        let valid = r#"{"name":"s","entities":[
            {"name":"Label","components":[{"type":"HudText","text":"HI","anchor":"bottom_right"}]},
            {"name":"Bar","components":[{"type":"HudRect","size":[0.0,0.0]}]}
        ]}"#;
        assert!(validate_source(valid, "t").is_empty(), "{:?}", validate_source(valid, "t"));
    }

    #[test]
    fn rejects_a_dynamic_body_without_a_collider() {
        let source = r#"{"name":"s","entities":[
            {"name":"Faller","components":[
                {"type":"Transform"},{"type":"RigidBody","body":"dynamic"}
            ]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].error, "missing_collider");
        assert_eq!(errors[0].context().unwrap().entity.as_deref(), Some("Faller"));
    }

    #[test]
    fn fixed_and_kinematic_bodies_need_no_collider() {
        let source = r#"{"name":"s","entities":[
            {"name":"A","components":[{"type":"Transform"},{"type":"RigidBody","body":"fixed"}]},
            {"name":"B","components":[{"type":"Transform"},{"type":"RigidBody","body":"kinematic"}]}
        ]}"#;
        assert!(codes_of(source).is_empty());
    }

    #[test]
    fn rejects_physics_components_without_a_transform() {
        let source = r#"{"name":"s","entities":[
            {"name":"A","components":[{"type":"RigidBody","body":"fixed"}]},
            {"name":"B","components":[{"type":"Collider","shape":"sphere","radius":0.5}]}
        ]}"#;
        let codes = codes_of(source);
        assert_eq!(
            codes.iter().filter(|c| **c == "missing_transform").count(),
            2,
            "{codes:?}"
        );
    }

    #[test]
    fn rejects_nonuniform_scale_on_round_colliders() {
        let source = r#"{"name":"s","entities":[
            {"name":"Squished","components":[
                {"type":"Transform","scale":[1.0,2.0,1.0]},
                {"type":"Collider","shape":"sphere","radius":0.5}
            ]},
            {"name":"FineCuboid","components":[
                {"type":"Transform","scale":[1.0,2.0,1.0]},
                {"type":"Collider","shape":"cuboid","half_extents":[0.5,0.5,0.5]}
            ]}
        ]}"#;
        assert_eq!(codes_of(source), ["nonuniform_scale_on_round_collider"]);
    }

    #[test]
    fn wheels_need_a_real_chassis_and_no_physics_of_their_own() {
        // A correct vehicle: chassis + one wheel — no errors.
        let good = r#"{"name":"s","entities":[
            {"name":"Car","components":[
                {"type":"Transform"},
                {"type":"RigidBody","body":"dynamic"},
                {"type":"Collider","shape":"cuboid","half_extents":[1.0,0.5,2.0]}
            ]},
            {"name":"WheelFL","components":[
                {"type":"Transform"},
                {"type":"Wheel","vehicle":"Car","offset":[-0.8,0.0,-1.2]}
            ]}
        ]}"#;
        assert_eq!(codes_of(good), Vec::<&str>::new());

        // Typo'd chassis name: not found, with a suggestion.
        let typo = good.replace(r#""vehicle":"Car","#, r#""vehicle":"Carr","#);
        let errors = validate_source(&typo, "test.json");
        assert_eq!(errors[0].error, "wheel_vehicle_not_found");
        assert_eq!(
            errors[0].context().unwrap().did_you_mean.as_deref(),
            Some("Car")
        );

        // Chassis without a dynamic body is invalid.
        let fixed = good.replace(r#""body":"dynamic""#, r#""body":"fixed""#);
        assert_eq!(codes_of(&fixed), ["wheel_vehicle_invalid"]);

        // A wheel cannot be its own chassis.
        let own = good.replace(r#""vehicle":"Car","#, r#""vehicle":"WheelFL","#);
        assert_eq!(codes_of(&own), ["wheel_vehicle_invalid"]);

        // A wheel entity with its own collider: the chassis owns collision.
        let armored = good.replace(
            r#"{"type":"Wheel","vehicle":"Car","#,
            r#"{"type":"Collider","shape":"sphere","radius":0.3},
               {"type":"Wheel","vehicle":"Car","#,
        );
        assert_eq!(codes_of(&armored), ["wheel_with_physics"]);

        // And it needs a Transform for physics to write the pose into.
        let bare = good.replace(r#"{"type":"Transform"},
                {"type":"Wheel""#, r#"{"type":"Wheel""#);
        assert_eq!(codes_of(&bare), ["missing_transform"]);
    }

    #[test]
    fn a_tree_is_the_entitys_geometry_and_carries_its_own_material() {
        // The happy path, and the thing that makes a Tree different from every
        // other component: a Material with no Mesh next to it is *not* the
        // unused_material warning here, because the Material is the bark.
        let good = r#"{"name":"s","entities":[
            {"name":"Oak","components":[
                {"type":"Transform","position":[0.0,0.0,0.0]},
                {"type":"Tree","seed":3,"levels":2},
                {"type":"Material","albedo":[0.2,0.14,0.09]}
            ]}
        ]}"#;
        assert_eq!(codes_of(good), Vec::<&str>::new());

        // A Tree and a Mesh on one entity would draw both at one transform.
        let doubled = good.replace(
            r#"{"type":"Tree""#,
            r#"{"type":"Mesh","asset":"builtin:cube"},{"type":"Tree""#,
        );
        assert_eq!(codes_of(&doubled), ["tree_with_mesh"]);

        // Branching is exponential, so the combination of in-range fields can
        // still be absurd; refusing names a number rather than hanging.
        let huge = good.replace(
            r#""seed":3,"levels":2"#,
            r#""seed":3,"levels":4,"branches":12,"whorl":6,"sides":12,"segments":8"#,
        );
        let errors = validate_source(&huge, "test.json");
        assert_eq!(errors[0].error, "tree_too_complex");
        assert!(
            errors[0].message.contains("vertices"),
            "the error should name the number: {}",
            errors[0].message
        );

        // Typos in the closed leaf vocabulary get a suggestion like any other.
        let typo = good.replace(r#""levels":2"#, r#""levels":2,"leaf":"cluser""#);
        let errors = validate_source(&typo, "test.json");
        assert_eq!(errors[0].error, "invalid_field_type");
        assert_eq!(
            errors[0].context().unwrap().did_you_mean.as_deref(),
            Some("cluster")
        );
    }

    #[test]
    fn enforces_per_shape_collider_fields() {
        let source = r#"{"name":"s","entities":[
            {"name":"A","components":[
                {"type":"Transform"},
                {"type":"Collider","shape":"sphere","half_extents":[0.5,0.5,0.5]}
            ]},
            {"name":"B","components":[
                {"type":"Transform"},
                {"type":"Collider","shape":"cuboid"}
            ]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        let codes: Vec<&str> = errors.iter().map(|e| e.error).collect();
        // Sphere: missing radius AND stray half_extents; cuboid: missing half_extents.
        assert!(codes.iter().filter(|c| **c == "missing_field").count() == 2, "{errors:?}");
        assert!(codes.contains(&"shape_field_mismatch"), "{errors:?}");
    }

    #[test]
    fn rejects_non_positive_shape_dimensions() {
        let source = r#"{"name":"s","entities":[
            {"name":"A","components":[
                {"type":"Transform"},
                {"type":"Collider","shape":"cuboid","half_extents":[0.5,0.0,0.5]}
            ]},
            {"name":"B","components":[
                {"type":"Transform"},
                {"type":"Collider","shape":"sphere","radius":-1.0}
            ]}
        ]}"#;
        let codes = codes_of(source);
        assert_eq!(
            codes.iter().filter(|c| **c == "invalid_shape_dimension").count(),
            2,
            "{codes:?}"
        );
    }

    #[test]
    fn accepts_a_valid_breakable() {
        let source = r#"{"name":"s","entities":[
            {"name":"Crate","components":[
                {"type":"Transform"},
                {"type":"Mesh","asset":"builtin:cube"},
                {"type":"Collider","shape":"cuboid","half_extents":[0.5,0.5,0.5]},
                {"type":"Breakable","impulse_threshold":8.0,"fragments":[
                    {"mesh":"builtin:cube","offset":[-0.25,0.0,0.0],"scale":[0.5,0.5,0.5]},
                    {"mesh":"builtin:sphere","rotation":[0.0,30.0,0.0],
                     "half_extents":[0.2,0.2,0.2],"density":2.0}
                ]}
            ]}
        ]}"#;
        assert!(validate_source(source, "test.json").is_empty());
    }

    #[test]
    fn rejects_an_empty_fragments_list() {
        // An empty Vec still *parses* — the minimum is a range check, not a
        // shape error, per the corpus agreement property.
        let source = r#"{"name":"s","entities":[
            {"name":"Crate","components":[
                {"type":"Transform"},
                {"type":"Breakable","fragments":[]}
            ]}
        ]}"#;
        assert_eq!(codes_of(source), ["value_out_of_range"]);
    }

    #[test]
    fn rejects_unknown_fragment_fields_with_a_suggestion() {
        let source = r#"{"name":"s","entities":[
            {"name":"Crate","components":[
                {"type":"Transform"},
                {"type":"Breakable","fragments":[
                    {"mesh":"builtin:cube","ofset":[0.1,0.0,0.0]}
                ]}
            ]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].error, "unknown_field");
        assert_eq!(errors[0].context().unwrap().did_you_mean.as_deref(), Some("offset"));
        assert_eq!(
            errors[0].context().unwrap().path.as_deref(),
            Some("/entities/0/components/1/fragments/0/ofset")
        );
    }

    #[test]
    fn rejects_a_fragment_missing_its_mesh() {
        let source = r#"{"name":"s","entities":[
            {"name":"Crate","components":[
                {"type":"Transform"},
                {"type":"Breakable","fragments":[{"offset":[0.1,0.0,0.0]}]}
            ]}
        ]}"#;
        assert_eq!(codes_of(source), ["missing_field"]);
    }

    #[test]
    fn suggests_a_near_miss_fragment_mesh() {
        let source = r#"{"name":"s","entities":[
            {"name":"Crate","components":[
                {"type":"Transform"},
                {"type":"Breakable","fragments":[{"mesh":"builtin:cubee"}]}
            ]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].error, "asset_not_found");
        assert_eq!(errors[0].context().unwrap().did_you_mean.as_deref(), Some("builtin:cube"));
        assert_eq!(
            errors[0].context().unwrap().path.as_deref(),
            Some("/entities/0/components/1/fragments/0/mesh")
        );
    }

    #[test]
    fn rejects_non_positive_fragment_half_extents() {
        let source = r#"{"name":"s","entities":[
            {"name":"Crate","components":[
                {"type":"Transform"},
                {"type":"Breakable","fragments":[
                    {"mesh":"builtin:cube","half_extents":[0.2,0.0,0.2]}
                ]}
            ]}
        ]}"#;
        assert_eq!(codes_of(source), ["invalid_shape_dimension"]);
    }

    #[test]
    fn rejects_a_thresholded_breakable_without_a_collider() {
        let source = r#"{"name":"s","entities":[
            {"name":"Crate","components":[
                {"type":"Transform"},
                {"type":"Breakable","impulse_threshold":5.0,
                 "fragments":[{"mesh":"builtin:cube"}]}
            ]}
        ]}"#;
        assert_eq!(codes_of(source), ["breakable_without_collider"]);

        // Omitting the threshold makes it script/explosion-only: no collider
        // needed.
        let source = r#"{"name":"s","entities":[
            {"name":"Crate","components":[
                {"type":"Transform"},
                {"type":"Breakable","fragments":[{"mesh":"builtin:cube"}]}
            ]}
        ]}"#;
        assert!(validate_source(source, "test.json").is_empty());
    }

    #[test]
    fn a_water_entity_owns_its_surface_alone() {
        // A Mesh or a Material beside Water is a second answer to what this
        // surface is, and the render only ever honours one of them.
        let with_mesh = r#"{"name":"s","entities":[
            {"name":"Pond","components":[
                {"type":"Transform"},
                {"type":"Water"},
                {"type":"Mesh","asset":"builtin:plane"}
            ]}
        ]}"#;
        assert_eq!(codes_of(with_mesh), ["water_with_mesh"]);

        // A Material alone is the same error — and *not* the `unused_material`
        // warning, which would be a confusing second thing to read about one
        // mistake.
        let with_material = r#"{"name":"s","entities":[
            {"name":"Pond","components":[
                {"type":"Transform"},
                {"type":"Water"},
                {"type":"Material","albedo":[0.1,0.2,0.3]}
            ]}
        ]}"#;
        assert_eq!(codes_of(with_material), ["water_with_mesh"]);

        // Water on its own, with nothing else to say: valid, and a flat
        // reflective surface is exactly what it means.
        let alone = r#"{"name":"s","entities":[
            {"name":"Pond","components":[
                {"type":"Transform","scale":[10.0,1.0,10.0]},
                {"type":"Water"}
            ]}
        ]}"#;
        assert!(validate_source(alone, "test.json").is_empty());
    }

    // ── Daylight (M21) ─────────────────────────────────────────────

    #[test]
    fn daylight_and_an_authored_sun_are_two_owners_of_one_thing() {
        // Invariant 8: a rotation in a text file that is silently ignored, or
        // silently overwritten, is a value that does not mean what it says.
        let both = r#"{"name":"s","daylight":{},"entities":[
            {"name":"Sun","components":[
                {"type":"Transform","rotation":[-40.0,0.0,0.0]},
                {"type":"DirectionalLight"}
            ]}
        ]}"#;
        assert_eq!(codes_of(both), ["daylight_and_directional_light"]);

        // `drives_sun: false` is the escape hatch, and it makes the same
        // scene legal — daylight then paints the sky and leaves the sun alone.
        let hand_aimed = r#"{"name":"s","daylight":{"drives_sun":false},"entities":[
            {"name":"Sun","components":[
                {"type":"Transform","rotation":[-40.0,0.0,0.0]},
                {"type":"DirectionalLight"}
            ]}
        ]}"#;
        assert!(validate_source(hand_aimed, "test.json").is_empty());

        // And daylight with no light entities at all is the ordinary case:
        // the block *is* the sun.
        let alone = r#"{"name":"s","daylight":{"time_of_day":7.25},"entities":[]}"#;
        assert!(validate_source(alone, "test.json").is_empty());
    }

    #[test]
    fn authored_sky_under_daylight_warns_rather_than_failing() {
        // The `unused_material` precedent: a value nothing reads is worth
        // saying out loud, but it is not a broken scene.
        let source = r#"{"name":"s",
            "daylight":{},
            "environment":{"sky":true,"sky_zenith":[0.1,0.2,0.3]},
            "entities":[]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error, "daylight_overrides_sky");
        assert!(errors[0].is_warning(), "this must not fail the scene");

        // An AmbientLight is unread for the same reason, and says so.
        let ambient = r#"{"name":"s","daylight":{},"entities":[
            {"name":"Sky","components":[{"type":"AmbientLight"}]}
        ]}"#;
        let errors = validate_source(ambient, "test.json");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error, "daylight_overrides_sky");
        assert!(errors[0].is_warning());

        // `drives_sky: false` silences both, because then they are read.
        let kept = r#"{"name":"s",
            "daylight":{"drives_sky":false},
            "environment":{"sky":true,"sky_zenith":[0.1,0.2,0.3]},
            "entities":[
                {"name":"Sky","components":[{"type":"AmbientLight"}]}
            ]}"#;
        assert!(validate_source(kept, "test.json").is_empty());
    }

    #[test]
    fn daylight_values_are_range_checked() {
        for (field, value) in [
            ("time_of_day", "24.0"),   // the hour is [0, 24), open at the top
            ("time_of_day", "-1.0"),
            ("day_length", "-5.0"),
            ("sun_elevation", "0.0"),  // (0, 90]: a sun that never rises
            ("sun_elevation", "91.0"),
            ("moon_elevation", "0.0"),
            ("moon_intensity", "-0.1"),
        ] {
            let source = format!(
                r#"{{"name":"s","daylight":{{"{field}":{value}}},"entities":[]}}"#
            );
            assert_eq!(
                codes_of(&source),
                ["invalid_daylight_value"],
                "daylight.{field} = {value} should have been rejected"
            );
        }

        // A typo'd field gets a suggestion, not a silent default.
        let typo = r#"{"name":"s","daylight":{"time_of_dey":6.0},"entities":[]}"#;
        let errors = validate_source(typo, "test.json");
        assert_eq!(errors[0].error, "unknown_field");
        assert_eq!(
            errors[0].context().unwrap().did_you_mean.as_deref(),
            Some("time_of_day")
        );
    }

    #[test]
    fn a_palette_must_be_sorted_and_complete() {
        // Nine fields, all required: a half-specified keyframe silently
        // interpolating toward black is a worse failure than being told to
        // finish it.
        let full = |hour: &str| {
            format!(
                r#"{{"hour":{hour},"sun_color":[1,1,1],"sun_intensity":1.0,
                     "ambient_color":[1,1,1],"ambient_intensity":0.2,
                     "sky_zenith":[0.2,0.3,0.6],"sky_horizon":[0.6,0.7,0.8],
                     "sky_ground":[0.1,0.1,0.1],"fog_scale":1.0}}"#
            )
        };

        let sorted = format!(
            r#"{{"name":"s","daylight":{{"palette":[{},{}]}},"entities":[]}}"#,
            full("6.0"),
            full("18.0")
        );
        assert!(validate_source(&sorted, "test.json").is_empty());

        // Out of order: the table wraps, so an unsorted one has no
        // well-defined "next keyframe" and would run backwards through the day.
        let unsorted = format!(
            r#"{{"name":"s","daylight":{{"palette":[{},{}]}},"entities":[]}}"#,
            full("18.0"),
            full("6.0")
        );
        assert_eq!(codes_of(&unsorted), ["daylight_palette_invalid"]);

        // One keyframe is not a day.
        let lonely = format!(
            r#"{{"name":"s","daylight":{{"palette":[{}]}},"entities":[]}}"#,
            full("12.0")
        );
        assert_eq!(codes_of(&lonely), ["daylight_palette_invalid"]);

        // A missing field is located, not defaulted.
        let partial = format!(
            r#"{{"name":"s","daylight":{{"palette":[{{"hour":6.0}},{}]}},"entities":[]}}"#,
            full("18.0")
        );
        let errors = validate_source(&partial, "test.json");
        assert!(errors.iter().all(|e| e.error == "missing_field"));
        assert_eq!(errors.len(), 8, "eight of the nine fields are absent");
    }

    #[test]
    fn rejects_waves_that_would_fold_the_surface() {
        // Gerstner's constraint, and the one number an author cannot infer from
        // a single wave: each is legal, the sum is not.
        let source = r#"{"name":"s","entities":[
            {"name":"Sea","components":[
                {"type":"Transform"},
                {"type":"Water","waves":[
                    {"wavelength":6.0,"amplitude":0.4,"steepness":0.7},
                    {"wavelength":2.0,"amplitude":0.2,"steepness":0.5}
                ]}
            ]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(
            errors.iter().map(|e| e.error).collect::<Vec<_>>(),
            ["water_waves_self_intersect"]
        );
        // The message has to carry the arithmetic: the author needs to know
        // which numbers to scale, not merely that something is too steep.
        assert!(errors[0].message.contains("1.2"), "{}", errors[0].message);
        assert!(
            errors[0].message.contains("0.7 + 0.5"),
            "{}",
            errors[0].message
        );

        // Exactly 1 is the boundary and is allowed: it is the point where the
        // surface first has a vertical tangent, not where it folds.
        let boundary = source.replace("0.5}", "0.3}");
        assert!(validate_source(&boundary, "test.json").is_empty());
    }

    #[test]
    fn the_wave_list_is_capped_by_the_schema() {
        // The cap lives in two places — `water::MAX_WAVES` sizes the shader's
        // uniform array, `#[schemars(length(max = ...))]` rejects the scene —
        // and they have to be the same number, or a scene would validate and
        // then silently lose waves at the pipeline.
        let schema = crate::schema::component_schema();
        let water = schema["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["properties"]["type"]["const"] == "Water")
            .expect("Water is a published component");
        assert_eq!(
            water["properties"]["waves"]["maxItems"].as_u64(),
            Some(crate::water::MAX_WAVES as u64),
            "the published cap must match the one the renderer packs for"
        );

        let one = r#"{"wavelength":2.0,"amplitude":0.1,"steepness":0.05}"#;
        let waves = vec![one; crate::water::MAX_WAVES + 1].join(",");
        let source = format!(
            r#"{{"name":"s","entities":[
                {{"name":"Sea","components":[
                    {{"type":"Transform"}},
                    {{"type":"Water","waves":[{waves}]}}
                ]}}
            ]}}"#
        );
        assert_eq!(codes_of(&source), ["value_out_of_range"]);
    }

    #[test]
    fn rejects_a_bad_timestep() {
        let source = r#"{"name":"s","physics":{"timestep_hz":0},"entities":[]}"#;
        assert_eq!(codes_of(source), ["invalid_physics_value"]);
        let source = r#"{"name":"s","physics":{"timestep_hz":60.5},"entities":[]}"#;
        assert_eq!(codes_of(source), ["invalid_physics_value"]);
    }

    #[test]
    fn rejects_unknown_physics_block_fields() {
        let source = r#"{"name":"s","physics":{"gravty":[0,-9.81,0]},"entities":[]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors[0].error, "unknown_field");
        assert_eq!(errors[0].context().unwrap().did_you_mean.as_deref(), Some("gravity"));
    }

    #[test]
    fn accepts_a_full_environment_block() {
        let source = r#"{"name":"s","environment":{
            "sky":true,"sky_zenith":[0.2,0.3,0.6],"sky_horizon":[0.6,0.7,0.8],
            "sky_ground":[0.1,0.1,0.1],"fog_density":0.01,
            "shadows":true,"shadow_distance":80.0,"samples":4},"entities":[]}"#;
        assert!(codes_of(source).is_empty(), "{:?}", codes_of(source));
    }

    #[test]
    fn rejects_unknown_environment_block_fields() {
        let source = r#"{"name":"s","environment":{"shadow":true},"entities":[]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors[0].error, "unknown_field");
        assert_eq!(
            errors[0].context().unwrap().did_you_mean.as_deref(),
            Some("shadows")
        );
    }

    #[test]
    fn rejects_environment_values_outside_their_range() {
        // Only 1 and 4 are real sample counts; 2 is told so rather than rounded.
        let source = r#"{"name":"s","environment":{"samples":2},"entities":[]}"#;
        assert_eq!(codes_of(source), ["invalid_environment_value"]);

        let source = r#"{"name":"s","environment":{"fog_density":-1.0},"entities":[]}"#;
        assert_eq!(codes_of(source), ["invalid_environment_value"]);

        let source = r#"{"name":"s","environment":{"shadow_distance":0.0},"entities":[]}"#;
        assert_eq!(codes_of(source), ["invalid_environment_value"]);
    }

    #[test]
    fn rejects_mistyped_environment_fields() {
        let source = r#"{"name":"s","environment":{"sky":"yes"},"entities":[]}"#;
        assert_eq!(codes_of(source), ["invalid_field_type"]);

        let source = r#"{"name":"s","environment":{"sky_zenith":[0.2,0.3]},"entities":[]}"#;
        assert_eq!(codes_of(source), ["invalid_field_type"]);
    }

    /// The block reports *every* problem at once, like the rest of validation.
    #[test]
    fn reports_all_environment_problems_together() {
        let source =
            r#"{"name":"s","environment":{"samples":3,"fog_density":-1,"nope":1},"entities":[]}"#;
        let mut codes = codes_of(source);
        codes.sort();
        assert_eq!(
            codes,
            [
                "invalid_environment_value",
                "invalid_environment_value",
                "unknown_field"
            ]
        );
    }

    #[test]
    fn rejects_duplicate_entity_names() {
        let source = r#"{"name":"s","entities":[{"name":"A"},{"name":"A"}]}"#;
        assert_eq!(codes_of(source), ["duplicate_entity_name"]);
    }

    #[test]
    fn rejects_more_than_one_active_camera() {
        let source = r#"{"name":"s","entities":[
            {"name":"A","components":[{"type":"Camera","active":true}]},
            {"name":"B","components":[{"type":"Camera","active":true}]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error, "multiple_active_cameras");
        assert_eq!(
            errors[0].context().unwrap().candidates,
            Some(vec!["A".to_string(), "B".to_string()])
        );
    }

    #[test]
    fn accepts_exactly_one_active_camera_among_several() {
        let source = r#"{"name":"s","entities":[
            {"name":"A","components":[{"type":"Camera","active":true}]},
            {"name":"B","components":[{"type":"Camera","active":false}]}
        ]}"#;
        assert!(codes_of(source).is_empty());
    }

    #[test]
    fn collects_every_error_rather_than_stopping_at_the_first() {
        // The whole reason this does not just defer to serde: an agent should
        // need one validate run, not four.
        let source = r#"{"name":"s","entities":[
            {"name":"A","components":[{"type":"Meterial"}]},
            {"name":"A","components":[{"type":"Transform","postion":[0,0,0]}]},
            {"name":"C","components":[{"type":"Mesh"}]}
        ]}"#;
        let codes = codes_of(source);
        assert_eq!(
            codes.len(),
            4,
            "expected four distinct errors, got {codes:?}"
        );
        assert!(codes.contains(&"unknown_component"));
        assert!(codes.contains(&"duplicate_entity_name"));
        assert!(codes.contains(&"unknown_field"));
        assert!(codes.contains(&"missing_field"));
    }

    #[test]
    fn rejects_a_misspelled_top_level_field() {
        let source = r#"{"name":"s","entites":[]}"#;
        let errors = validate_source(source, "test.json");
        let unknown = errors
            .iter()
            .find(|e| e.error == "unknown_field")
            .expect("should flag the misspelled key");
        assert_eq!(
            unknown.context().unwrap().did_you_mean.as_deref(),
            Some("entities")
        );
    }

    #[test]
    fn requires_entity_names() {
        let source = r#"{"name":"s","entities":[{"components":[]}]}"#;
        assert_eq!(codes_of(source), ["missing_entity_name"]);
    }

    // ── Collision (M12): mesh shapes and layers ────────────────────────

    #[test]
    fn mesh_colliders_borrow_the_entity_mesh_or_take_an_asset() {
        // Trimesh borrowing the entity's Mesh: valid.
        let borrowing = r#"{"name":"s","entities":[
            {"name":"Track","components":[
                {"type":"Transform"},
                {"type":"Mesh","asset":"builtin:plane"},
                {"type":"Collider","shape":"trimesh"}
            ]}
        ]}"#;
        assert!(codes_of(borrowing).is_empty(), "{:?}", validate_source(borrowing, "t"));

        // Trimesh with an explicit asset and no Mesh: also valid.
        let explicit = r#"{"name":"s","entities":[
            {"name":"Track","components":[
                {"type":"Transform"},
                {"type":"Collider","shape":"trimesh","asset":"builtin:plane"}
            ]}
        ]}"#;
        assert!(codes_of(explicit).is_empty(), "{:?}", validate_source(explicit, "t"));

        // Neither: there is no geometry to collide.
        let neither = r#"{"name":"s","entities":[
            {"name":"Track","components":[
                {"type":"Transform"},
                {"type":"Collider","shape":"trimesh"}
            ]}
        ]}"#;
        assert_eq!(codes_of(neither), ["collider_missing_mesh"]);
    }

    #[test]
    fn rejects_a_trimesh_on_a_dynamic_body() {
        let source = r#"{"name":"s","entities":[
            {"name":"Rock","components":[
                {"type":"Transform"},
                {"type":"RigidBody","body":"dynamic"},
                {"type":"Collider","shape":"trimesh","asset":"builtin:cube"}
            ]}
        ]}"#;
        assert_eq!(codes_of(source), ["trimesh_on_dynamic_body"]);

        // convex_hull is the supported dynamic mesh shape.
        let hull = source.replace("trimesh", "convex_hull");
        assert!(codes_of(&hull).is_empty(), "{:?}", validate_source(&hull, "t"));
    }

    #[test]
    fn asset_is_a_mesh_shape_field_and_gets_reference_checks() {
        let source = r#"{"name":"s","entities":[
            {"name":"A","components":[
                {"type":"Transform"},
                {"type":"Collider","shape":"cuboid","half_extents":[1.0,1.0,1.0],
                 "asset":"builtin:cube"}
            ]},
            {"name":"B","components":[
                {"type":"Transform"},
                {"type":"Collider","shape":"trimesh","asset":"builtin:cubee"}
            ]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        let codes: Vec<&str> = errors.iter().map(|e| e.error).collect();
        assert!(codes.contains(&"shape_field_mismatch"), "{errors:?}");
        let bad_ref = errors.iter().find(|e| e.error == "asset_not_found").unwrap();
        assert_eq!(
            bad_ref.context().unwrap().did_you_mean.as_deref(),
            Some("builtin:cube")
        );
    }

    #[test]
    fn warns_on_a_collides_with_layer_nobody_declares() {
        let source = r#"{"name":"s","entities":[
            {"name":"Ground","components":[
                {"type":"Transform"},
                {"type":"Collider","shape":"cuboid","half_extents":[5.0,0.1,5.0],
                 "layers":["ground"]}
            ]},
            {"name":"Sensor","components":[
                {"type":"Transform"},
                {"type":"Collider","shape":"cuboid","half_extents":[1.0,1.0,1.0],
                 "sensor":true,"collides_with":["gorund"]}
            ]}
        ]}"#;
        let errors = validate_source(source, "test.json");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].error, "unknown_collision_layer");
        assert!(errors[0].is_warning(), "a typo'd reference still simulates, so: warning");
        assert_eq!(errors[0].context().unwrap().did_you_mean.as_deref(), Some("ground"));
    }

    #[test]
    fn rejects_empty_layer_arrays_and_more_than_32_layers() {
        let empty = r#"{"name":"s","entities":[
            {"name":"A","components":[
                {"type":"Transform"},
                {"type":"Collider","shape":"cuboid","half_extents":[1.0,1.0,1.0],
                 "layers":[]}
            ]}
        ]}"#;
        assert_eq!(codes_of(empty), ["empty_collision_layers"]);

        let names: Vec<String> = (0..33).map(|i| format!("\"layer{i:02}\"")).collect();
        let crowded = format!(
            r#"{{"name":"s","entities":[
                {{"name":"A","components":[
                    {{"type":"Transform"}},
                    {{"type":"Collider","shape":"cuboid","half_extents":[1.0,1.0,1.0],
                     "layers":[{}]}}
                ]}}
            ]}}"#,
            names.join(",")
        );
        assert_eq!(codes_of(&crowded), ["too_many_collision_layers"]);
    }

    #[test]
    fn matching_layers_validate_clean() {
        let source = r#"{"name":"s","entities":[
            {"name":"Ground","components":[
                {"type":"Transform"},
                {"type":"Collider","shape":"cuboid","half_extents":[5.0,0.1,5.0],
                 "layers":["ground"]}
            ]},
            {"name":"Player","components":[
                {"type":"Transform"},
                {"type":"RigidBody","body":"dynamic"},
                {"type":"Collider","shape":"sphere","radius":0.5,
                 "layers":["player"],"collides_with":["ground"]}
            ]}
        ]}"#;
        assert!(codes_of(source).is_empty(), "{:?}", validate_source(source, "t"));
    }
}
