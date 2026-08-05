//! The cross-entity passes.
//!
//! Each runs after the entity walk because each checks a *reference between*
//! entities, and a name may be authored after its use. They take
//! [`SceneFacts`] and destructure it by name — see the note on that struct for
//! why the fields are not passed positionally.

use std::path::Path;

use serde_json::{Map, Value};

use crate::codes;
use crate::error::EngineError;
use crate::lineindex::LineIndex;

use super::entity::SceneFacts;
use super::Cx;

/// More than one active camera: a deterministic failure over a silent pick.
pub(super) fn camera(cx: &Cx<'_>, facts: &SceneFacts<'_>, errors: &mut Vec<EngineError>) {
    let SceneFacts { active_cameras, .. } = facts;

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
}

/// At most one directional light, one ambient light and one probe volume — the
/// camera's precedent, applied to every component the engine can hold only one
/// of.
pub(super) fn lights(cx: &Cx<'_>, facts: &SceneFacts<'_>, errors: &mut Vec<EngineError>) {
    let SceneFacts {
        directional_lights,
        ambient_lights,
        probe_volumes,
        ..
    } = facts;

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
        // A probe volume joined them in M35 for the same reason and one more:
        // the renderer holds a single field, so a second volume would bake,
        // validate, and then silently light nothing.
        (
            &probe_volumes,
            "LightProbeVolume",
            codes::MULTIPLE_LIGHT_PROBE_VOLUMES,
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
}

/// Daylight ownership (M21): two owners of one sun is what invariant 8 prevents.
pub(super) fn daylight(
    cx: &Cx<'_>,
    object: &Map<String, Value>,
    facts: &SceneFacts<'_>,
    errors: &mut Vec<EngineError>,
) {
    let SceneFacts {
        directional_lights,
        ambient_lights,
        ..
    } = facts;

    // ── Daylight ownership (M21) ───────────────────────────────────────
    // Two owners of one sun is what invariant 8 exists to prevent: a rotation
    // in a text file that is silently ignored, or silently overwritten, is a
    // value that does not mean what it says.
    if let Some(daylight) = object.get("daylight").and_then(Value::as_object) {
        let drives_sun = daylight
            .get("drives_sun")
            .is_none_or(|v| v.as_bool().unwrap_or(true));

        if drives_sun {
            for (name, path) in directional_lights {
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
            .is_none_or(|v| v.as_bool().unwrap_or(true));

        // The other half of `daylight_overrides_sky`: ambient rides with the
        // sky, so an authored AmbientLight is unread for the same reason the
        // authored band colors are.
        if drives_sky {
            for (name, path) in ambient_lights {
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
}

/// The fixed-size point-light array (M17): refuse rather than drop the ninth.
pub(super) fn point_light_budget(
    cx: &Cx<'_>,
    facts: &SceneFacts<'_>,
    errors: &mut Vec<EngineError>,
) {
    let SceneFacts { point_lights, .. } = facts;

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
}

/// Collision layers (M12): the 32-bit budget and reference checks.
pub(super) fn collision_layers(cx: &Cx<'_>, facts: &SceneFacts<'_>, errors: &mut Vec<EngineError>) {
    let SceneFacts {
        distinct_layers,
        layer_memberships,
        layer_refs,
        ..
    } = facts;

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
    for (layer, entity, path) in layer_refs {
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
}

/// Wheels (M12): every chassis must exist and be a different dynamic body.
pub(super) fn wheel(cx: &Cx<'_>, facts: &SceneFacts<'_>, errors: &mut Vec<EngineError>) {
    let SceneFacts {
        wheels,
        body_kinds,
        seen_names,
        ..
    } = facts;

    // ── Wheel pass (M12): every wheel's chassis must exist and be a
    //    different entity with a dynamic RigidBody ─────────────────────
    for (owner, wheel, wheel_component_path) in wheels {
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
}

/// One cross-entity name reference: the two-arm check every "this component
/// names an entity carrying X" pass shares. Arm one — the name resolves to
/// nothing — reports `not_found` with a suggestion drawn from every entity;
/// arm two — it resolves, but the target has no X — reports `invalid` with a
/// suggestion drawn from the entities that *do* carry X. Six passes were
/// line-for-line copies of this shape before it was factored (the wheel pass
/// keeps its own second arm: its predicate is "dynamic body, not itself",
/// not "carries a component").
pub(super) struct Reference<'a> {
    /// The phrase both messages open with, ending at the quoted name —
    /// "the Meadow on \"Lawn\" names terrain \"Hill\"".
    pub(super) subject: String,
    /// The name being resolved.
    pub(super) name: &'a str,
    pub(super) owner: &'a str,
    pub(super) component: &'a str,
    pub(super) field: &'static str,
    /// JSON pointer of the naming field.
    pub(super) path: String,
    /// What the target must carry, as the message says it — "Terrain
    /// component", "HudPanel".
    pub(super) target: &'static str,
    /// Why the target must be one; follows "has no {target}; ".
    pub(super) why: &'static str,
    pub(super) not_found: &'static str,
    pub(super) invalid: &'static str,
}

/// Check one [`Reference`], returning whether it resolved to a valid target —
/// the junction pass chains its closed-road check on the answer.
pub(super) fn check_reference(
    cx: &Cx<'_>,
    reference: Reference<'_>,
    seen_names: &[&str],
    target_names: &std::collections::BTreeSet<String>,
    errors: &mut Vec<EngineError>,
) -> bool {
    if !seen_names.contains(&reference.name) {
        errors.push(
            cx.err(
                reference.not_found,
                format!(
                    "{}, which is not an entity in this scene",
                    reference.subject
                ),
                &reference.path,
            )
            .entity(reference.owner)
            .component(reference.component)
            .field(reference.field)
            .suggest_from(reference.name, seen_names.iter().copied()),
        );
        return false;
    }
    if !target_names.contains(reference.name) {
        errors.push(
            cx.err(
                reference.invalid,
                format!(
                    "{}, but that entity has no {}; {}",
                    reference.subject, reference.target, reference.why
                ),
                &reference.path,
            )
            .entity(reference.owner)
            .component(reference.component)
            .field(reference.field)
            .suggest_from(reference.name, target_names.iter().map(String::as_str)),
        );
        return false;
    }
    true
}

/// Meadows (M29): the ground a meadow names must be a `Terrain`.
pub(super) fn meadow(cx: &Cx<'_>, facts: &SceneFacts<'_>, errors: &mut Vec<EngineError>) {
    let SceneFacts {
        meadows,
        terrain_names,
        seen_names,
        ..
    } = facts;

    // ── Meadow pass (M29): the ground a meadow names must be a Terrain ──
    //
    // A name that resolves to nothing, or to an entity with no `Terrain`, would
    // otherwise silently fall back to flat ground at the meadow's own Y — grass
    // hovering over a hillside, with nothing in the file or the render saying
    // why. The wheel pass's shape, and its reasoning.
    for (owner, meadow, meadow_component_path) in meadows {
        let Some(ground) = &meadow.terrain else {
            continue;
        };
        check_reference(
            cx,
            Reference {
                subject: format!("the Meadow on {owner:?} names terrain {ground:?}"),
                name: ground,
                owner,
                component: "Meadow",
                field: "terrain",
                path: format!("{meadow_component_path}/terrain"),
                target: "Terrain component",
                why: "a meadow samples its ground height from a Terrain patch, \
                      so the name must be one",
                not_found: codes::MEADOW_TERRAIN_NOT_FOUND,
                invalid: codes::MEADOW_TERRAIN_INVALID,
            },
            seen_names,
            terrain_names,
            errors,
        );
    }
}

/// Roads that follow a terrain (M40): the ground a road rides on.
///
/// The meadow pass's shape, and the same failure it prevents — a
/// `follow_terrain` that resolves to nothing falls back to the road's authored
/// heights, which is a road hanging in the air over a hillside with nothing in
/// the file or the render saying why.
pub(super) fn road_ground(cx: &Cx<'_>, facts: &SceneFacts<'_>, errors: &mut Vec<EngineError>) {
    let SceneFacts {
        roads,
        terrain_names,
        seen_names,
        ..
    } = facts;

    for (owner, road, road_component_path) in roads {
        let Some(ground) = &road.follow_terrain else {
            continue;
        };
        check_reference(
            cx,
            Reference {
                subject: format!("the Road on {owner:?} follows terrain {ground:?}"),
                name: ground,
                owner,
                component: "Road",
                field: "follow_terrain",
                path: format!("{road_component_path}/follow_terrain"),
                target: "Terrain component",
                why: "a road samples its ground height from a Terrain patch, \
                      so the name must be one",
                not_found: codes::ROAD_TERRAIN_NOT_FOUND,
                invalid: codes::ROAD_TERRAIN_INVALID,
            },
            seen_names,
            terrain_names,
            errors,
        );
    }
}

/// Junctions (M40): the roads whose mouths bound the patch.
///
/// An arm that resolves to nothing is silently dropped by the generator, and
/// what comes back is a patch with one side missing — a hole exactly where the
/// junction was supposed to close one. Every reason the meadow pass exists.
pub(super) fn junction(cx: &Cx<'_>, facts: &SceneFacts<'_>, errors: &mut Vec<EngineError>) {
    let SceneFacts {
        junctions,
        road_names,
        closed_road_names,
        seen_names,
        ..
    } = facts;

    for (owner, junction, component_path) in junctions {
        for (index, arm) in junction.arms.iter().enumerate() {
            let arm_path = format!("{component_path}/arms/{index}/road");
            let resolved = check_reference(
                cx,
                Reference {
                    subject: format!(
                        "arm {index} of the Junction on {owner:?} names road {:?}",
                        arm.road
                    ),
                    name: &arm.road,
                    owner,
                    component: "Junction",
                    field: "arms",
                    path: arm_path.clone(),
                    target: "Road component",
                    why: "a junction is bounded by the mouths of roads, so \
                          every arm must name one",
                    not_found: codes::JUNCTION_ROAD_NOT_FOUND,
                    invalid: codes::JUNCTION_ROAD_INVALID,
                },
                seen_names,
                road_names,
                errors,
            );
            // The third arm is this pass's own: a *valid* road that happens
            // to be a loop has no free end for a junction to meet.
            if resolved && closed_road_names.contains(arm.road.as_str()) {
                errors.push(
                    cx.err(
                        codes::JUNCTION_ARM_CLOSED,
                        format!(
                            "arm {index} of the Junction on {owner:?} names road \
                             {:?}, which is closed; a closed road is a loop with no \
                             free end for a junction to meet — split it into two \
                             open roads that both end here",
                            arm.road
                        ),
                        &arm_path,
                    )
                    .entity(owner)
                    .component("Junction")
                    .field("arms"),
                );
            }
        }
    }
}

/// Buoyancy (M41): the water a body floats on, and the body itself.
pub(super) fn buoyancy(cx: &Cx<'_>, facts: &SceneFacts<'_>, errors: &mut Vec<EngineError>) {
    let SceneFacts {
        buoyancies,
        water_names,
        collider_names,
        body_kinds,
        seen_names,
        ..
    } = facts;

    // ── Buoyancy pass (M41) ────────────────────────────────────────────
    //
    // Two halves. The meadow pass's name check, because the same silent failure
    // is available — a `water` that resolves to nothing would leave a boat
    // sinking with nothing in the file saying why. And a check the meadow pass
    // has no equivalent of: buoyancy is *a force*, so an entity with no dynamic
    // body has nothing for it to act on. Authoring one there is a component
    // that cannot do anything, which is exactly the class of mistake a render
    // cannot show you.
    for (owner, buoyancy, component_path) in buoyancies {
        if !buoyancy.water.trim().is_empty() {
            check_reference(
                cx,
                Reference {
                    subject: format!("the Buoyancy on {owner:?} names water {:?}", buoyancy.water),
                    name: &buoyancy.water,
                    owner,
                    component: "Buoyancy",
                    field: "water",
                    path: format!("{component_path}/water"),
                    target: "Water component",
                    why: "a body floats on a Water surface, so the name must \
                          be one",
                    not_found: codes::BUOYANCY_WATER_NOT_FOUND,
                    invalid: codes::BUOYANCY_WATER_INVALID,
                },
                seen_names,
                water_names,
                errors,
            );
        }

        let dynamic = body_kinds.get(owner.as_str()) == Some(&crate::components::BodyKind::Dynamic);
        let has_collider = collider_names.contains(owner.as_str());
        if !dynamic || !has_collider {
            let missing = match (dynamic, has_collider) {
                (false, false) => "has neither a dynamic RigidBody nor a Collider",
                (false, true) => "has no dynamic RigidBody",
                (true, false) => "has no Collider",
                (true, true) => unreachable!("guarded by the condition above"),
            };
            errors.push(
                cx.err(
                    codes::BUOYANCY_WITHOUT_BODY,
                    format!(
                        "the Buoyancy on {owner:?} {missing}; buoyancy is a force applied \
                         to a dynamic body, and the shape it displaces water with is the \
                         entity's own Collider, so without both the component can have no \
                         effect at all"
                    ),
                    component_path,
                )
                .entity(owner)
                .component("Buoyancy"),
            );
        }
    }
}

/// Foot planting (M32): the ground a character stands on.
pub(super) fn foot_planting(cx: &Cx<'_>, facts: &SceneFacts<'_>, errors: &mut Vec<EngineError>) {
    let SceneFacts {
        foot_plants,
        terrain_names,
        seen_names,
        ..
    } = facts;

    // ── Foot planting pass (M32): the ground a character stands on ─────
    //
    // The meadow pass's shape again, and the same failure it prevents: a
    // `ground` that resolves to nothing would leave the feet posed exactly as
    // the clip put them, with nothing in the file or the render saying why the
    // component appears to do nothing.
    for (owner, plant, plant_component_path) in foot_plants {
        check_reference(
            cx,
            Reference {
                subject: format!("the FootPlant on {owner:?} names ground {:?}", plant.ground),
                name: &plant.ground,
                owner,
                component: "FootPlant",
                field: "ground",
                path: format!("{plant_component_path}/ground"),
                target: "Terrain component",
                why: "feet are planted against a Terrain and deliberately not \
                      against the physics world, so that the pose stays a \
                      pure function of the files",
                not_found: codes::FOOT_PLANT_GROUND_NOT_FOUND,
                invalid: codes::FOOT_PLANT_GROUND_NOT_TERRAIN,
            },
            seen_names,
            terrain_names,
            errors,
        );

        // A bounded budget an agent can be told about, rather than a solver
        // that plants four feet and silently ignores the fifth.
        if plant.feet.len() > crate::components::MAX_PLANTED_FEET {
            errors.push(
                cx.err(
                    codes::TOO_MANY_PLANTED_FEET,
                    format!(
                        "the FootPlant on {owner:?} lists {} feet; at most {} are \
                         planted",
                        plant.feet.len(),
                        crate::components::MAX_PLANTED_FEET
                    ),
                    &format!("{plant_component_path}/feet"),
                )
                .entity(owner)
                .component("FootPlant")
                .field("feet"),
            );
        }
    }
}

/// HUD parentage (M31): the tree the layout engine will walk.
pub(super) fn hud_parent(cx: &Cx<'_>, facts: &SceneFacts<'_>, errors: &mut Vec<EngineError>) {
    let SceneFacts {
        hud_elements,
        hud_panel_names,
        seen_names,
        ..
    } = facts;

    // ── HUD parent pass (M31): the tree the layout engine will walk ────
    //
    // The wheel pass's shape, for the wheel pass's reason. `parent` is a name
    // in a flat file (the `Wheel.vehicle` precedent), so nothing about the
    // file's *structure* can guarantee it resolves, that its target is a
    // container, or that the chain terminates. All three are checked here
    // rather than guarded at layout time, because a hung layout with no output
    // is the worst failure an agent loop can hit — `tree_too_complex`'s
    // argument.
    for (owner, component, parent, component_path) in hud_elements {
        let Some(parent) = parent else {
            continue;
        };
        check_reference(
            cx,
            Reference {
                subject: format!("the {component} on {owner:?} names parent {parent:?}"),
                name: parent,
                owner,
                component,
                field: "parent",
                path: format!("{component_path}/parent"),
                target: "HudPanel",
                why: "only a panel lays children out, so the name must be one",
                not_found: codes::HUD_PARENT_NOT_FOUND,
                invalid: codes::HUD_PARENT_NOT_PANEL,
            },
            seen_names,
            hud_panel_names,
            errors,
        );
    }

    // Cycles and depth, walked over the resolved graph. Both are reported with
    // the whole chain, because "A's parent is B" is not enough to find the ring
    // when the ring is five elements long.
    {
        let parent_of: std::collections::BTreeMap<&str, &str> = hud_elements
            .iter()
            .filter_map(|(owner, _, parent, _)| {
                let parent = parent.as_deref()?;
                hud_panel_names
                    .contains(parent)
                    .then_some((owner.as_str(), parent))
            })
            .collect();

        for (owner, component, parent, component_path) in hud_elements {
            if parent.is_none() {
                continue;
            }
            let parent_path = format!("{component_path}/parent");
            let mut chain = vec![owner.as_str()];
            let mut current = owner.as_str();
            while let Some(&next) = parent_of.get(current) {
                if let Some(at) = chain.iter().position(|seen| *seen == next) {
                    let ring: Vec<&str> = chain[at..].to_vec();
                    errors.push(
                        cx.err(
                            codes::HUD_PARENT_CYCLE,
                            format!(
                                "the {component} on {owner:?} is inside a parent cycle: {} → {next}; \
                                 a HUD element cannot be its own ancestor",
                                ring.join(" → ")
                            ),
                            &parent_path,
                        )
                        .entity(owner)
                        .component(*component)
                        .field("parent"),
                    );
                    break;
                }
                chain.push(next);
                if chain.len() > crate::ui::MAX_HUD_DEPTH {
                    errors.push(
                        cx.err(
                            codes::HUD_NESTING_TOO_DEEP,
                            format!(
                                "the {component} on {owner:?} nests {} levels deep, past the \
                                 limit of {}; flatten the chain {}",
                                chain.len() - 1,
                                crate::ui::MAX_HUD_DEPTH,
                                chain.join(" → ")
                            ),
                            &parent_path,
                        )
                        .entity(owner)
                        .component(*component)
                        .field("parent"),
                    );
                    break;
                }
                current = next;
            }
        }
    }
}

/// Animation (M9): clip contents, target entities, and conflicts.
pub(super) fn animation(cx: &Cx<'_>, facts: &SceneFacts<'_>, errors: &mut Vec<EngineError>) {
    let SceneFacts {
        players,
        body_kinds,
        seen_names,
        ..
    } = facts;

    // ── Animation pass (M9): clip contents, target entities, conflicts ─
    // Runs against the same scene the players sit in; clip-content errors
    // carry the *clip's* file/line via its own LineIndex.
    let base_dir = Path::new(cx.file).parent().unwrap_or(Path::new(""));
    let mut claimed: std::collections::HashMap<(String, String), String> =
        std::collections::HashMap::new();
    for (player_entity, player, player_path) in players {
        // Skeletal references (M30) are the asset pass's business; a glTF
        // path with no fragment already has its own error. Either way, this
        // pass would only try to read a binary as JSON.
        if player.clip.contains('#')
            || crate::skeleton::is_gltf_path(&player.clip)
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
                && body_kinds.get(&track.entity) == Some(&crate::components::BodyKind::Dynamic)
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
}
