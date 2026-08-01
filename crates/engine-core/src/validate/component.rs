//! Per-component semantic checks beyond what the schema can express.

use std::path::Path;

use serde_json::Value;

use crate::codes;
use crate::components::ComponentData;
use crate::error::EngineError;
use crate::mesh::MeshAsset;

use super::{kind_of, validate_material_source, walk_component, Checked, ComponentSchemas, Cx};

pub(super) fn check_component(
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

    let shape_clean = walk_component(
        cx,
        schemas,
        variant,
        object,
        type_name,
        entity,
        component_path,
        errors,
    );
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
            // clippy wants `far <= near`, which is a different function: it
            // is *false* for NaN, so a NaN far plane would validate clean.
            #[allow(clippy::neg_cmp_op_on_partial_ord)]
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
        // A material is one of two things and never half of each (M26): a set
        // of fields, or a reference to a file holding them.
        //
        // The exclusivity is checked against the **raw JSON**, not the parsed
        // component, and that is the whole reason the rule exists: every field
        // has a `#[serde(default)]`, so the parsed value cannot say whether
        // `"roughness": 0.9` was an override or someone spelling out the
        // default. The keys present in the file can.
        ComponentData::Material(material) => {
            let base_dir = Path::new(cx.file).parent().unwrap_or(Path::new(""));

            if material.asset.is_some() {
                let inline: Vec<&str> = object
                    .keys()
                    .map(String::as_str)
                    .filter(|k| *k != "type" && *k != "asset")
                    .collect();
                if !inline.is_empty() {
                    errors.push(
                        cx.err(
                            codes::MATERIAL_ASSET_WITH_FIELDS,
                            format!(
                                "this Material names an asset and also sets {}; a material \
                                 is a file or a set of fields, never both — an override \
                                 cannot be told apart from a field written at its default. \
                                 Make a second material file instead.",
                                inline.join(", ")
                            ),
                            component_path,
                        )
                        .entity(entity)
                        .component("Material")
                        .field("asset")
                        .candidates(inline.iter().copied()),
                    );
                }
            }

            // The reference itself, then the file's own contents with the
            // file's own line numbers — M9's clip-error precedent.
            if let Some(asset) = &material.asset {
                match crate::material::resolve_material(asset, base_dir) {
                    Err(resolve) => errors.push(
                        cx.err(
                            resolve.error,
                            resolve.message.clone(),
                            &format!("{component_path}/asset"),
                        )
                        .entity(entity)
                        .component("Material")
                        .field("asset"),
                    ),
                    Ok(path) => {
                        let display = path.display().to_string();
                        match std::fs::read_to_string(&path) {
                            Ok(source) => {
                                errors.extend(validate_material_source(&source, &display))
                            }
                            Err(e) => errors.push(
                                cx.err(
                                    codes::ASSET_LOAD_FAILED,
                                    format!("could not read material {display}: {e}"),
                                    &format!("{component_path}/asset"),
                                )
                                .entity(entity)
                                .component("Material")
                                .field("asset"),
                            ),
                        }
                    }
                }
            }

            for (field, asset, _) in material.maps() {
                if let Err(resolve) = crate::texture::resolve_texture(asset, base_dir) {
                    let mut error = cx
                        .err(
                            resolve.error,
                            resolve.message.clone(),
                            &format!("{component_path}/{field}"),
                        )
                        .entity(entity)
                        .component("Material")
                        .field(field);
                    if let Some(suggestion) = resolve.context().and_then(|c| c.did_you_mean.clone())
                    {
                        error = error.did_you_mean(suggestion);
                    }
                    errors.push(error);
                }
            }
        }

        // Water's own fields are fully covered by the schema walk (ranges,
        // `maxItems` on the wave list); what is left is cross-component and
        // cross-wave, and lives with the entity checks that can see the whole
        // entity.
        ComponentData::Water(_) => {}

        // Terrain likewise: field ranges and the layer count are schema, and
        // what is left (a Mesh beside it, a backwards band) needs the whole
        // entity in view.
        ComponentData::Terrain(_) => {}

        // A road's field ranges are covered by the schema walk. What is left is
        // what the polygon cannot guarantee about *itself*: whether the corner
        // radii fit on the edges feeding them, whether a sharp vertex turns
        // further than a mitre can cover, and whether it kerbs more corners
        // than the shader has room for. All three render as a road that has
        // crossed itself or lost a kerb, which is a bad thing to learn from a
        // screenshot.
        ComponentData::Road(ref road) => {
            let needed = if road.closed { 3 } else { 2 };
            if road.points.len() < needed {
                errors.push(
                    cx.err(
                        codes::ROAD_TOO_FEW_POINTS,
                        format!(
                            "Road has {} centerline point(s); {}a road needs at least \
                             {needed}",
                            road.points.len(),
                            if road.closed { "closed, " } else { "" },
                        ),
                        &format!("{component_path}/points"),
                    )
                    .entity(entity)
                    .component("Road")
                    .field("points"),
                );
            } else {
                for (index, kind, message) in crate::road::geometry_problems(road) {
                    let code = match kind {
                        crate::road::RoadProblem::CornerDoesNotFit => {
                            codes::ROAD_CORNER_DOES_NOT_FIT
                        }
                        crate::road::RoadProblem::CornerNeedsRadius => {
                            codes::ROAD_CORNER_NEEDS_RADIUS
                        }
                    };
                    errors.push(
                        cx.err(code, message, &format!("{component_path}/points/{index}"))
                            .entity(entity)
                            .component("Road")
                            .field("points"),
                    );
                }
            }

            let kerbs = crate::road::kerb_span_count(road);
            if kerbs > crate::road::MAX_ROAD_KERBS {
                errors.push(
                    cx.err(
                        codes::TOO_MANY_ROAD_KERBS,
                        format!(
                            "Road kerbs {kerbs} corners but the shader carries at most {}; \
                             lower markings.kerb_max_radius so it selects fewer, or split \
                             the road into two entities",
                            crate::road::MAX_ROAD_KERBS
                        ),
                        &format!("{component_path}/markings/kerb_max_radius"),
                    )
                    .entity(entity)
                    .component("Road")
                    .field("markings"),
                );
            }
        }

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
            // The negated form is load-bearing — `v <= 0.0` lets NaN through.
            #[allow(clippy::neg_cmp_op_on_partial_ord)]
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
        // The HUD family's own fields are covered by the schema walk (ranges,
        // the anchor/layout/align vocabularies). What is left is relational —
        // whether `parent` names a panel, whether the chain loops, whether a
        // `HudInteract` has anything to hit — and needs either the whole
        // entity or the whole scene in view, so it lives in the entity and
        // scene passes.
        ComponentData::HudText(_) | ComponentData::HudRect(_) | ComponentData::HudPanel(_) => {}

        ComponentData::HudInteract(_) => {}

        // An image's texture reference is checked exactly like a `Material`
        // map: existence, extension and absolute-path rejection here, size
        // (`texture_too_large`) and decodability in the engine-assets pass, so
        // a broken PNG fails `validate` rather than the screenshot.
        ComponentData::HudImage(ref image) => {
            let base_dir = Path::new(cx.file).parent().unwrap_or(Path::new(""));
            if let Err(resolve) = crate::texture::resolve_texture(&image.texture, base_dir) {
                let mut error = cx
                    .err(
                        resolve.error,
                        resolve.message.clone(),
                        &format!("{component_path}/texture"),
                    )
                    .entity(entity)
                    .component("HudImage")
                    .field("texture");
                if let Some(suggestion) = resolve.context().and_then(|c| c.did_you_mean.clone()) {
                    error = error.did_you_mean(suggestion);
                }
                errors.push(error);
            }
        }

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

        // The life-cycle table (M29). The keyframes are read by interpolating
        // between neighbours and wrapping from the last back to the first, so
        // an out-of-order or too-short table does not fail loudly at render
        // time — it silently plays the cycle backwards through part of itself,
        // which reads as a renderer bug. `daylight_palette_invalid`'s reasoning
        // and its shape.
        // A `FootPlant`'s own fields are range-checked by the schema walk, and
        // everything else about it needs either the scene's other entities
        // (the ground) or the rig itself (the joint names) — so the cross-
        // entity pass and engine-assets carry those, and there is nothing to
        // check with the component alone.
        ComponentData::FootPlant(_) => {}

        // A proxy set (M33). Everything here is answerable from the component
        // alone: which shapes a proxy may be, that each part carries the
        // dimensions its shape needs, that no two parts report under one name,
        // and the budget. Whether the joints exist is the rig's answer and
        // lives in engine-assets, beside the `FootPlant` joint check.
        ComponentData::SkinnedCollider(ref proxies) => {
            use crate::components::ColliderShapeKind::{
                Capsule, ConvexHull, Cuboid, Sphere, Trimesh,
            };

            if proxies.parts.len() > crate::components::MAX_COLLIDER_PARTS {
                errors.push(
                    cx.err(
                        codes::TOO_MANY_COLLIDER_PARTS,
                        format!(
                            "this SkinnedCollider lists {} parts; at most {} are built",
                            proxies.parts.len(),
                            crate::components::MAX_COLLIDER_PARTS
                        ),
                        &format!("{component_path}/parts"),
                    )
                    .entity(entity)
                    .component("SkinnedCollider")
                    .field("parts"),
                );
            }

            let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
            for (i, part) in proxies.parts.iter().enumerate() {
                let part_path = format!("{component_path}/parts/{i}");
                let label = part.part_name();

                // Reports address a part by name, and two parts under one name
                // would make `list-colliders` and a contact's `part` ambiguous
                // — the failure a report exists to prevent.
                if !seen.insert(label) {
                    errors.push(
                        cx.err(
                            codes::DUPLICATE_COLLIDER_PART,
                            format!(
                                "two parts of this SkinnedCollider report as {label:?}; \
                                 part names address a proxy in every report, so they \
                                 must be unique (set \"name\" on one of them)"
                            ),
                            &part_path,
                        )
                        .entity(entity)
                        .component("SkinnedCollider")
                        .field(format!("parts/{i}/name")),
                    );
                }

                let shape_name = match part.shape {
                    Cuboid => "cuboid",
                    Sphere => "sphere",
                    Capsule => "capsule",
                    Trimesh => "trimesh",
                    ConvexHull => "convex_hull",
                };
                if matches!(part.shape, Trimesh | ConvexHull) {
                    errors.push(
                        cx.err(
                            codes::COLLIDER_PART_SHAPE_UNSUPPORTED,
                            format!(
                                "part {label:?} is a {shape_name}; a proxy may only be \
                                 \"cuboid\", \"sphere\" or \"capsule\". A mesh shape \
                                 describes one specific mesh, and a skinned mesh is \
                                 posed on the GPU where physics cannot read it"
                            ),
                            &format!("{part_path}/shape"),
                        )
                        .entity(entity)
                        .component("SkinnedCollider")
                        .field(format!("parts/{i}/shape")),
                    );
                    continue;
                }

                // `Collider`'s per-shape rule, applied per part.
                let fields: [(&str, bool, bool); 3] = [
                    ("half_extents", part.half_extents.is_some(), part.shape == Cuboid),
                    (
                        "radius",
                        part.radius.is_some(),
                        matches!(part.shape, Sphere | Capsule),
                    ),
                    (
                        "half_height",
                        part.half_height.is_some(),
                        part.shape == Capsule,
                    ),
                ];
                for (field, present, wanted) in fields {
                    if wanted && !present {
                        errors.push(
                            cx.err(
                                codes::MISSING_FIELD,
                                format!(
                                    "{shape_name} parts require the field {field:?} \
                                     (part {label:?})"
                                ),
                                &part_path,
                            )
                            .entity(entity)
                            .component("SkinnedCollider")
                            .field(format!("parts/{i}/{field}")),
                        );
                    }
                    if !wanted && present {
                        errors.push(
                            cx.err(
                                codes::SHAPE_FIELD_MISMATCH,
                                format!(
                                    "{shape_name} parts have no field {field:?} \
                                     (part {label:?})"
                                ),
                                &format!("{part_path}/{field}"),
                            )
                            .entity(entity)
                            .component("SkinnedCollider")
                            .field(format!("parts/{i}/{field}")),
                        );
                    }
                }

                // Strictly positive, negated so a NaN fails — `Collider`'s
                // comparison and its reason.
                #[allow(clippy::neg_cmp_op_on_partial_ord)]
                let mut dimension = |field: &str, label: String, v: f32| {
                    if !(v > 0.0) {
                        errors.push(
                            cx.err(
                                codes::INVALID_SHAPE_DIMENSION,
                                format!(
                                    "SkinnedCollider.{label} is {v}; it must be greater \
                                     than 0"
                                ),
                                &format!("{part_path}/{field}"),
                            )
                            .entity(entity)
                            .component("SkinnedCollider")
                            .field(format!("parts/{i}/{field}")),
                        );
                    }
                };
                if let Some(half_extents) = part.half_extents {
                    for (axis, v) in half_extents.to_array().into_iter().enumerate() {
                        dimension(
                            "half_extents",
                            format!("parts[{i}].half_extents[{axis}]"),
                            v,
                        );
                    }
                }
                if let Some(radius) = part.radius {
                    dimension("radius", format!("parts[{i}].radius"), radius);
                }
                if let Some(half_height) = part.half_height {
                    dimension("half_height", format!("parts[{i}].half_height"), half_height);
                }
            }

            // M12's rule, unchanged: an empty array reads as "nothing", and
            // absence is how "everything" is spelled.
            for (field, list) in [
                ("layers", &proxies.layers),
                ("collides_with", &proxies.collides_with),
            ] {
                if list.as_ref().is_some_and(Vec::is_empty) {
                    errors.push(
                        cx.err(
                            codes::EMPTY_COLLISION_LAYERS,
                            format!(
                                "SkinnedCollider.{field} is an empty array, which would \
                                 mean \"nothing\"; omit the field to mean \"everything\""
                            ),
                            &format!("{component_path}/{field}"),
                        )
                        .entity(entity)
                        .component("SkinnedCollider")
                        .field(field),
                    );
                }
            }
        }

        ComponentData::Meadow(ref meadow) => {
            if meadow.stages.len() < 2 {
                errors.push(
                    cx.err(
                        codes::MEADOW_STAGES_INVALID,
                        format!(
                            "this Meadow has {} life-cycle stage(s); it needs at \
                             least two to interpolate between, and the table wraps \
                             from the last back round to the first",
                            meadow.stages.len()
                        ),
                        &format!("{component_path}/stages"),
                    )
                    .entity(entity)
                    .component("Meadow")
                    .field("stages"),
                );
            } else if meadow.stages.len() > crate::meadow::MAX_GROWTH_STAGES {
                errors.push(
                    cx.err(
                        codes::TOO_MANY_GROWTH_STAGES,
                        format!(
                            "this Meadow has {} life-cycle stages; the shader's table \
                             is a fixed-size uniform array holding {}",
                            meadow.stages.len(),
                            crate::meadow::MAX_GROWTH_STAGES
                        ),
                        &format!("{component_path}/stages"),
                    )
                    .entity(entity)
                    .component("Meadow")
                    .field("stages"),
                );
            }

            // Strictly increasing, and the negated form makes a NaN `at`
            // fail rather than compare false and slip through.
            #[allow(clippy::neg_cmp_op_on_partial_ord)]
            for pair in meadow.stages.windows(2) {
                if !(pair[1].at > pair[0].at) {
                    errors.push(
                        cx.err(
                            codes::MEADOW_STAGES_INVALID,
                            format!(
                                "this Meadow's stages run {} then {}; \"at\" must \
                                 strictly increase down the table",
                                pair[0].at, pair[1].at
                            ),
                            &format!("{component_path}/stages"),
                        )
                        .entity(entity)
                        .component("Meadow")
                        .field("stages"),
                    );
                    break;
                }
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
                    if let Some(suggestion) = resolve.context().and_then(|c| c.did_you_mean.clone())
                    {
                        error = error.did_you_mean(suggestion);
                    }
                    errors.push(error);
                }
                // `!(v > 0.0)` rather than `v <= 0.0`, so NaN fails too.
                #[allow(clippy::neg_cmp_op_on_partial_ord)]
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
            let reference = crate::skeleton::ClipRef::parse(&player.clip);
            let asset = reference.asset();

            if Path::new(asset).is_absolute() {
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
            } else if matches!(reference, crate::skeleton::ClipRef::Property(_))
                && crate::skeleton::is_gltf_path(asset)
            {
                // A glTF path with no `#Clip`. Defaulting to the only clip in
                // the file is friendlier right up until someone exports a
                // second one, at which point which clip plays changes
                // silently — the failure class this engine trades convenience
                // to avoid.
                errors.push(
                    cx.err(
                        codes::CLIP_NEEDS_FRAGMENT,
                        format!(
                            "clip {:?} names a glTF file but no clip inside it; write \
                             {asset}#ClipName (engine list-animations {asset} lists them)",
                            player.clip
                        ),
                        path,
                    )
                    .entity(entity)
                    .component("AnimationPlayer")
                    .field("clip"),
                );
            } else if !asset.starts_with("builtin:") {
                // A `builtin:` reference resolves to generated geometry with
                // no file behind it, so there is nothing to find on disk;
                // `mesh_has_no_skin` is what has something useful to say
                // about it, and it comes from the asset pass.
                let base_dir = Path::new(cx.file).parent().unwrap_or(Path::new(""));
                if !base_dir.join(asset).is_file() {
                    errors.push(
                        cx.err(
                            codes::ASSET_NOT_FOUND,
                            format!(
                                "no clip file at {asset:?} (clip paths resolve relative \
                                 to the scene file)"
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
