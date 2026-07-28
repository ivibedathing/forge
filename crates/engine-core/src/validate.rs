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
        if key != "name" && key != "entities" && key != "physics" {
            errors.push(
                cx.err(
                    codes::UNKNOWN_FIELD,
                    format!("unknown top-level field {key:?}"),
                    &format!("/{key}"),
                )
                .field(key)
                .suggest_from(key, ["name", "entities", "physics"]),
            );
        }
    }

    if let Some(physics) = object.get("physics") {
        check_physics_block(&cx, physics, &mut errors);
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
        let mut material_paths: Vec<String> = Vec::new();
        let mut has_transform = false;
        let mut scale = glam::Vec3::ONE;
        let mut rigid_body: Option<(crate::components::BodyKind, String)> = None;
        let mut collider: Option<(crate::components::ColliderShapeKind, String)> = None;
        let mut wheel_path: Option<String> = None;

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
            if type_name == "Mesh" {
                has_mesh = true;
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
                    collider = Some((c.shape, component_path));
                }
                Some(ComponentData::Wheel(w)) => {
                    wheel_path = Some(component_path.clone());
                    wheels.push((name.to_string(), w, component_path));
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
        if let Some((shape, path)) = &collider {
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
                shape,
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

        // Legal but almost certainly wrong: dead data from editing the wrong
        // entity. A warning, because rendering it is well-defined.
        if !has_mesh {
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
        (&ambient_lights, "AmbientLight", codes::MULTIPLE_AMBIENT_LIGHTS),
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
        ComponentData::Material(_) => {}

        // The flat Collider struct keeps the file walkable; which fields each
        // shape requires and forbids is semantic, checked here (design §5).
        ComponentData::Collider(collider) => {
            use crate::components::ColliderShapeKind::{Capsule, Cuboid, Sphere};

            let shape_name = match collider.shape {
                Cuboid => "cuboid",
                Sphere => "sphere",
                Capsule => "capsule",
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
            check_value(cx, property, value, type_name, entity, key, &field_path, errors);
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
                let expected = match (min_items, max_items) {
                    (Some(a), Some(b)) if a == b => format!("exactly {a}"),
                    (Some(a), Some(b)) => format!("between {a} and {b}"),
                    (Some(a), None) => format!("at least {a}"),
                    (None, Some(b)) => format!("at most {b}"),
                    (None, None) => unreachable!("guarded above"),
                };
                errors.push(
                    cx.err(
                        codes::INVALID_FIELD_TYPE,
                        format!("{field:?} must be an array of {expected} elements, found {len}"),
                        json_path,
                    )
                    .entity(entity)
                    .component(component)
                    .field(field),
                );
                return false;
            }

            let item_schema = &schema["items"];
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
                }
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
}
