//! The error-code registry.
//!
//! Every code an `EngineError` can carry is declared here, once, alongside its
//! exit-code class and a one-line description. Call sites name codes through
//! these consts rather than string literals, so a rename or a near-duplicate
//! (`asset_not_found` vs. `asset_missing`) is a compile error or a review-time
//! diff in exactly one file, never a silent wire change.
//!
//! **Codes are API.** An agent's scripts branch on them. Adding a code is a
//! feature; renaming or removing one is a breaking change to every consumer in
//! the wild. `docs/error-codes.md` is the human-facing copy of this table, and
//! a repo-contract test asserts the two match one-to-one in both directions.

/// Who is at fault, and therefore which exit code the process ends with.
///
/// The split an agent branches on without parsing anything: 1 means "edit your
/// files and retry," 2 means "fix your command or your machine."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitClass {
    /// The input files are at fault (exit code 1).
    Input,
    /// The invocation or environment is at fault (exit code 2).
    Environment,
}

impl ExitClass {
    pub fn code(self) -> i32 {
        match self {
            ExitClass::Input => 1,
            ExitClass::Environment => 2,
        }
    }
}

/// One registered error code.
pub struct ErrorCode {
    pub code: &'static str,
    pub class: ExitClass,
    pub description: &'static str,
}

/// Declares the consts and the registry table from one list, so a code cannot
/// exist without a class and a description or vice versa.
macro_rules! registry {
    ($($konst:ident = $code:literal, $class:ident, $description:literal;)*) => {
        $(pub const $konst: &str = $code;)*

        /// Every registered code. `docs/error-codes.md` mirrors this table.
        pub const REGISTRY: &[ErrorCode] = &[
            $(ErrorCode {
                code: $code,
                class: ExitClass::$class,
                description: $description,
            },)*
        ];
    };
}

registry! {
    // ── Scene structure ────────────────────────────────────────────────
    INVALID_JSON = "invalid_json", Input,
        "the file is not syntactically valid JSON";
    SCENE_ROOT_NOT_OBJECT = "scene_root_not_object", Input,
        "the top-level JSON value is not an object";
    MISSING_FIELD = "missing_field", Input,
        "a required field is absent";
    UNKNOWN_FIELD = "unknown_field", Input,
        "a field name is not part of the schema";
    INVALID_FIELD_TYPE = "invalid_field_type", Input,
        "a field holds a value of the wrong JSON type";
    ENTITY_NOT_OBJECT = "entity_not_object", Input,
        "an entry in \"entities\" is not a JSON object";
    MISSING_ENTITY_NAME = "missing_entity_name", Input,
        "an entity has no \"name\" field";
    EMPTY_ENTITY_NAME = "empty_entity_name", Input,
        "an entity's \"name\" is the empty string";
    DUPLICATE_ENTITY_NAME = "duplicate_entity_name", Input,
        "two entities share a name";
    COMPONENT_NOT_OBJECT = "component_not_object", Input,
        "an entry in \"components\" is not a JSON object";
    COMPONENT_MISSING_TYPE = "component_missing_type", Input,
        "a component has no \"type\" field";
    UNKNOWN_COMPONENT = "unknown_component", Input,
        "a component's \"type\" names no known component";
    DUPLICATE_COMPONENT = "duplicate_component", Input,
        "the same component type appears twice on one entity";
    VALUE_OUT_OF_RANGE = "value_out_of_range", Input,
        "a numeric field is outside its documented range";
    MULTIPLE_ACTIVE_CAMERAS = "multiple_active_cameras", Input,
        "more than one camera is marked active";
    MULTIPLE_DIRECTIONAL_LIGHTS = "multiple_directional_lights", Input,
        "more than one DirectionalLight in the scene";
    MULTIPLE_AMBIENT_LIGHTS = "multiple_ambient_lights", Input,
        "more than one AmbientLight in the scene";
    TOO_MANY_POINT_LIGHTS = "too_many_point_lights", Input,
        "a scene may carry at most 8 PointLight components";
    VALIDATION_FAILED = "validation_failed", Input,
        "summary error after per-diagnostic reports; see the preceding lines";

    // ── Physics (M8) ───────────────────────────────────────────────────
    UNKNOWN_SHAPE = "unknown_shape", Input,
        "a Collider names no known shape kind";
    UNKNOWN_BODY_KIND = "unknown_body_kind", Input,
        "a RigidBody names no known body kind";
    INVALID_SHAPE_DIMENSION = "invalid_shape_dimension", Input,
        "a collider dimension must be strictly positive";
    SHAPE_FIELD_MISMATCH = "shape_field_mismatch", Input,
        "a Collider field does not apply to its shape";
    NONUNIFORM_SCALE_ON_ROUND_COLLIDER = "nonuniform_scale_on_round_collider", Input,
        "sphere and capsule colliders cannot take a nonuniform Transform.scale";
    INVALID_PHYSICS_VALUE = "invalid_physics_value", Input,
        "a physics setting is outside its meaningful range";
    MISSING_COLLIDER = "missing_collider", Input,
        "a dynamic RigidBody has no Collider and would fall through everything";
    MISSING_TRANSFORM = "missing_transform", Input,
        "a RigidBody or Collider needs a Transform on the same entity";

    // ── Collision (M12) ────────────────────────────────────────────────
    TRIMESH_ON_DYNAMIC_BODY = "trimesh_on_dynamic_body", Input,
        "a dynamic RigidBody cannot use a trimesh Collider; use convex_hull";
    COLLIDER_MISSING_MESH = "collider_missing_mesh", Input,
        "a mesh-shaped Collider has no asset and its entity has no Mesh";
    TOO_MANY_COLLISION_LAYERS = "too_many_collision_layers", Input,
        "a scene may name at most 32 distinct collision layers";
    EMPTY_COLLISION_LAYERS = "empty_collision_layers", Input,
        "an empty layers/collides_with array; omit the field to mean everything";

    // ── Animation (M9) ─────────────────────────────────────────────────
    UNKNOWN_ENTITY = "unknown_entity", Input,
        "an animation track targets an entity name not in the scene";
    UNKNOWN_PROPERTY = "unknown_property", Input,
        "an animation track targets no known Component.field";
    TYPE_MISMATCH = "type_mismatch", Input,
        "a key value's shape does not match the animated field";
    UNSORTED_KEYS = "unsorted_keys", Input,
        "key times must be strictly increasing";
    CONFLICTING_TRACKS = "conflicting_tracks", Input,
        "two active clips animate the same property of the same entity";
    ANIMATION_ON_DYNAMIC_BODY = "animation_on_dynamic_body", Input,
        "a clip animates the Transform of a dynamic RigidBody; make it kinematic";

    // ── Skeletal animation (M30) ───────────────────────────────────────
    CLIP_NEEDS_FRAGMENT = "clip_needs_fragment", Input,
        "a glTF clip reference must name a clip: path#ClipName";
    UNKNOWN_CLIP = "unknown_clip", Input,
        "a #ClipName fragment names no animation in that glTF file";
    MESH_HAS_NO_SKIN = "mesh_has_no_skin", Input,
        "a skeletal AnimationPlayer's glTF file carries no skin";
    SKELETAL_PLAYER_MESH_MISMATCH = "skeletal_player_mesh_mismatch", Input,
        "a skeletal AnimationPlayer and its entity's Mesh name different files";
    TOO_MANY_JOINTS = "too_many_joints", Input,
        "a skin has more joints than the fixed-size palette holds";

    // ── Locomotion and foot planting (M32) ─────────────────────────────
    ANIMATION_STRIDE_WITHOUT_TRANSFORM = "animation_stride_without_transform", Input,
        "an AnimationPlayer sets stride but its entity has no Transform to measure";
    FOOT_PLANT_WITHOUT_SKIN = "foot_plant_without_skin", Input,
        "a FootPlant is on an entity whose Mesh carries no skin";
    FOOT_PLANT_GROUND_NOT_FOUND = "foot_plant_ground_not_found", Input,
        "a FootPlant's ground names no entity in the scene";
    FOOT_PLANT_GROUND_NOT_TERRAIN = "foot_plant_ground_not_terrain", Input,
        "a FootPlant's ground names an entity with no Terrain";
    FOOT_PLANT_NON_UNIFORM_SCALE = "foot_plant_non_uniform_scale", Input,
        "a planted character's Transform.scale must be uniform";
    FOOT_PLANT_CHAIN_TOO_LONG = "foot_plant_chain_too_long", Input,
        "a foot's chain reaches past the rig's root";
    TOO_MANY_PLANTED_FEET = "too_many_planted_feet", Input,
        "a FootPlant lists more feet than the solver runs";
    UNKNOWN_JOINT = "unknown_joint", Input,
        "a joint name is not in the entity's rig";

    // ── Skinned collider proxies (M33) ─────────────────────────────────
    SKINNED_COLLIDER_WITHOUT_SKIN = "skinned_collider_without_skin", Input,
        "a SkinnedCollider is on an entity whose Mesh carries no skin";
    SKINNED_COLLIDER_NON_UNIFORM_SCALE = "skinned_collider_non_uniform_scale", Input,
        "a proxied character's Transform.scale must be uniform";
    DUPLICATE_COLLIDER_PART = "duplicate_collider_part", Input,
        "two parts of one SkinnedCollider report under the same name";
    TOO_MANY_COLLIDER_PARTS = "too_many_collider_parts", Input,
        "a SkinnedCollider lists more parts than the physics world builds";
    COLLIDER_PART_SHAPE_UNSUPPORTED = "collider_part_shape_unsupported", Input,
        "a SkinnedCollider part names a mesh shape, which a proxy cannot be";

    // ── Ragdolls (M39) ─────────────────────────────────────────────────
    RAGDOLL_WITHOUT_PROXIES = "ragdoll_without_proxies", Input,
        "a Ragdoll is on an entity with no SkinnedCollider; the bodies are the proxies";
    RAGDOLL_DISCONNECTED_PARTS = "ragdoll_disconnected_parts", Input,
        "a Ragdoll's parts form more than one tree, which is a ragdoll in pieces";
    RAGDOLL_UNKNOWN_JOINT = "ragdoll_unknown_joint", Input,
        "a Ragdoll joint override names a joint no part of the SkinnedCollider rides";
    RAGDOLL_DUPLICATE_JOINT = "ragdoll_duplicate_joint", Input,
        "two Ragdoll joint overrides name the same joint";
    RAGDOLL_BAD_HINGE = "ragdoll_bad_hinge", Input,
        "a Ragdoll hinge axis is zero-length, or its range runs backwards";
    COLLIDER_PART_FIT_UNSUPPORTED = "collider_part_fit_unsupported", Input,
        "a sphere part asks to fit the bone, and a sphere has no length to solve";

    // ── Breaking (M14) ─────────────────────────────────────────────────
    BREAKABLE_WITHOUT_COLLIDER = "breakable_without_collider", Input,
        "a Breakable sets impulse_threshold but the entity has no Collider to be hit on";

    // ── Fracture (M43) ─────────────────────────────────────────────────
    FRAGMENT_GEOMETRY = "fragment_geometry", Input,
        "a fragment is a mesh reference or a shard's points, never both and never neither";
    SHARD_DEGENERATE = "shard_degenerate", Input,
        "a shard's points do not bound a volume: fewer than four, or all coplanar";
    SHARD_WITH_MESH = "shard_with_mesh", Input,
        "a Shard owns its geometry, so the entity may not also carry a Mesh";
    FRACTURE_FAILED = "fracture_failed", Input,
        "engine fracture could not break the volume into the pieces asked for";

    // ── Emitter lifetime (M44) ─────────────────────────────────────────
    EMITTER_NEVER_FINISHES = "emitter_never_finishes", Input,
        "a ParticleEmitter sets despawn_when_done but has no duration to finish";

    // ── Environment (M16) ──────────────────────────────────────────────
    INVALID_ENVIRONMENT_VALUE = "invalid_environment_value", Input,
        "an environment setting is outside its meaningful range";

    // ── Daylight (M21) ─────────────────────────────────────────────────
    INVALID_DAYLIGHT_VALUE = "invalid_daylight_value", Input,
        "a daylight setting is outside its meaningful range";
    DAYLIGHT_PALETTE_INVALID = "daylight_palette_invalid", Input,
        "a daylight palette needs at least two keyframes with strictly increasing hours";
    DAYLIGHT_AND_DIRECTIONAL_LIGHT = "daylight_and_directional_light", Input,
        "daylight drives the sun, so the scene may not also author a DirectionalLight";

    // ── Water (M18) ────────────────────────────────────────────────────
    WATER_WITH_MESH = "water_with_mesh", Input,
        "a Water entity owns its own surface; it may not also have a Mesh or a Material";
    WATER_WAVES_SELF_INTERSECT = "water_waves_self_intersect", Input,
        "the sum of Water wave steepness exceeds 1, which folds the surface through itself";

    // ── Terrain (M22) ──────────────────────────────────────────────────
    TERRAIN_WITH_MESH = "terrain_with_mesh", Input,
        "a Terrain entity owns its own surface; it may not also have a Mesh or a Material";
    TERRAIN_LAYER_RANGE_INVERTED = "terrain_layer_range_inverted", Input,
        "a Terrain layer's height or slope range runs backwards, so it covers nothing";
    // ── Terrain basins (M42) ───────────────────────────────────────────
    TERRAIN_BASIN_NO_EFFECT = "terrain_basin_no_effect", Input,
        "a Terrain basin has no depth or no footprint, so it cuts nothing (warning)";
    TERRAIN_BASIN_OUTSIDE_PATCH = "terrain_basin_outside_patch", Input,
        "a Terrain basin's footprint misses the patch entirely, usually a center \
         written in local rather than world XZ (warning)";
    // ── Roads (M23) ────────────────────────────────────────────────────
    ROAD_WITH_MESH = "road_with_mesh", Input,
        "a Road entity owns its own surface; it may not also have a Mesh or a Material";
    ROAD_TOO_FEW_POINTS = "road_too_few_points", Input,
        "a Road needs at least two centerline points, or three to close";
    ROAD_CORNER_DOES_NOT_FIT = "road_corner_does_not_fit", Input,
        "two corner radii need more of the edge between them than it has";
    ROAD_CORNER_NEEDS_RADIUS = "road_corner_needs_radius", Input,
        "a sharp corner turns too far to mitre; give it a radius";
    TOO_MANY_ROAD_KERBS = "too_many_road_kerbs", Input,
        "a Road kerbs more corners than the shader's span array holds";
    ROAD_TERRAIN_NOT_FOUND = "road_terrain_not_found", Input,
        "a Road's \"follow_terrain\" names no entity in the scene";
    ROAD_TERRAIN_INVALID = "road_terrain_invalid", Input,
        "a Road's \"follow_terrain\" must name an entity that has a Terrain component";

    // ── Junctions (M40) ────────────────────────────────────────────────
    JUNCTION_WITH_MESH = "junction_with_mesh", Input,
        "a Junction entity owns its own surface; it may not also have a Mesh or a Material";
    JUNCTION_TOO_FEW_ARMS = "junction_too_few_arms", Input,
        "a Junction needs at least two arms to bound a patch";
    JUNCTION_ROAD_NOT_FOUND = "junction_road_not_found", Input,
        "a Junction arm's \"road\" names no entity in the scene";
    JUNCTION_ROAD_INVALID = "junction_road_invalid", Input,
        "a Junction arm's \"road\" must name an entity that has a Road component";
    JUNCTION_ARM_CLOSED = "junction_arm_closed", Input,
        "a Junction arm names a closed road, which has no free end to meet";
    JUNCTION_DUPLICATE_ARM = "junction_duplicate_arm", Input,
        "two Junction arms name the same end of the same road";

    // ── Scripting (M10) ────────────────────────────────────────────────
    SCRIPT_PARSE_ERROR = "script_parse_error", Input,
        "a script file does not compile";
    SCRIPT_MISSING_STEP_FN = "script_missing_step_fn", Input,
        "a script defines no `fn step(world, step)`";
    SCRIPT_RUNTIME_ERROR = "script_runtime_error", Input,
        "a script failed while running";

    // ── Entity spawning (M37) ──────────────────────────────────────────
    TEMPLATE_NOT_OBJECT = "template_not_object", Input,
        "an entry in \"templates\" is not a JSON object";
    MISSING_TEMPLATE_NAME = "missing_template_name", Input,
        "a template has no \"name\" field";
    EMPTY_TEMPLATE_NAME = "empty_template_name", Input,
        "a template's \"name\" is the empty string";
    DUPLICATE_TEMPLATE_NAME = "duplicate_template_name", Input,
        "a template's name is shared with another template or with an entity";
    TEMPLATE_FORBIDDEN_COMPONENT = "template_forbidden_component", Input,
        "a component whose scene-level budget a spawn could violate may not appear in a template";

    // ── Vehicles (M12) ─────────────────────────────────────────────────
    WHEEL_VEHICLE_NOT_FOUND = "wheel_vehicle_not_found", Input,
        "a Wheel's \"vehicle\" names no entity in the scene";
    WHEEL_VEHICLE_INVALID = "wheel_vehicle_invalid", Input,
        "a Wheel's \"vehicle\" must be a different entity with a dynamic RigidBody";
    WHEEL_WITH_PHYSICS = "wheel_with_physics", Input,
        "a Wheel entity may not have its own RigidBody or Collider; the chassis owns all collision";

    // ── Trees (M19) ────────────────────────────────────────────────────
    TREE_WITH_MESH = "tree_with_mesh", Input,
        "an entity may not have both a Tree and a Mesh; a Tree is the entity's geometry";
    TREE_TOO_COMPLEX = "tree_too_complex", Input,
        "a Tree's parameters would generate more vertices than the engine will grow";

    // ── Clouds (M20) ───────────────────────────────────────────────────
    CLOUD_WITH_MESH = "cloud_with_mesh", Input,
        "a Cloud entity owns its own geometry; it may not also have a Mesh or a Material";
    CLOUD_TOO_COMPLEX = "cloud_too_complex", Input,
        "a Cloud's parameters would generate more vertices than the engine will grow";

    // ── Meadows (M29) ──────────────────────────────────────────────────
    MEADOW_WITH_MESH = "meadow_with_mesh", Input,
        "a Meadow entity owns its own geometry; it may not also have a Mesh or a Material";
    MEADOW_TOO_COMPLEX = "meadow_too_complex", Input,
        "a Meadow's density and footprint would grow more triangles than the engine will draw";
    MEADOW_TERRAIN_NOT_FOUND = "meadow_terrain_not_found", Input,
        "a Meadow's \"terrain\" names no entity in the scene";
    MEADOW_TERRAIN_INVALID = "meadow_terrain_invalid", Input,
        "a Meadow's \"terrain\" must name an entity that has a Terrain component";
    MEADOW_STAGES_INVALID = "meadow_stages_invalid", Input,
        "a Meadow needs at least two life-cycle stages with strictly increasing \"at\"";
    TOO_MANY_GROWTH_STAGES = "too_many_growth_stages", Input,
        "a Meadow has more life-cycle stages than the shader's table holds";

    // ── Buoyancy (M41) ─────────────────────────────────────────────────
    BUOYANCY_WATER_MISSING = "buoyancy_water_missing", Input,
        "a Buoyancy must name the Water entity it floats on";
    BUOYANCY_WATER_NOT_FOUND = "buoyancy_water_not_found", Input,
        "a Buoyancy's \"water\" names no entity in the scene";
    BUOYANCY_WATER_INVALID = "buoyancy_water_invalid", Input,
        "a Buoyancy's \"water\" must name an entity that has a Water component";
    BUOYANCY_WITHOUT_BODY = "buoyancy_without_body", Input,
        "a Buoyancy needs a dynamic RigidBody and a Collider on the same entity";

    // ── Global illumination (M35) ──────────────────────────────────────
    LIGHT_PROBE_VOLUME_WITH_MESH = "light_probe_volume_with_mesh", Input,
        "a LightProbeVolume entity is a region of space, not geometry; it may not also have a Mesh or a Material";
    LIGHT_PROBE_VOLUME_WITHOUT_TRANSFORM = "light_probe_volume_without_transform", Input,
        "a LightProbeVolume takes its bounds from its Transform, so it needs one";
    GI_BAKE_MISSING = "gi_bake_missing", Input,
        "a LightProbeVolume's \"bake\" names a file that is not there; run `engine bake-gi`";
    GI_BAKE_STALE = "gi_bake_stale", Input,
        "a GI bake was taken from a different scene than the one loading it; re-run `engine bake-gi`";
    GI_BAKE_MALFORMED = "gi_bake_malformed", Input,
        "a GI bake file parses but its version, grid or basis disagrees with the component";
    TOO_MANY_GI_PROBES = "too_many_gi_probes", Input,
        "a LightProbeVolume's bounds and spacing would place more probes than the engine will bake";
    MULTIPLE_LIGHT_PROBE_VOLUMES = "multiple_light_probe_volumes", Input,
        "a scene may have at most one LightProbeVolume; the renderer holds one field";

    // ── Tile synthesis (M47) ───────────────────────────────────────────
    TILE_GRID_WITH_MESH = "tile_grid_with_mesh", Input,
        "a TileGrid grows its own geometry from its tileset's palette; it may not also have a Mesh or a Material";
    TILESET_NOT_FOUND = "tileset_not_found", Input,
        "a TileGrid's \"tileset\" names a file that is not there";
    TILESET_MALFORMED = "tileset_malformed", Input,
        "a tileset file does not parse, or a tile in it is structurally invalid";
    TILESET_TOO_COMPLEX = "tileset_too_complex", Input,
        "a tileset expands to more tiles, or a tile carries more parts, than the engine indexes";
    UNKNOWN_SOCKET_FORM = "unknown_socket_form", Input,
        "a socket string is not \"0\", a plain name, or a name suffixed _l/_r/_i";
    UNKNOWN_PALETTE_KEY = "unknown_palette_key", Input,
        "a tile part names a material that is not in the tileset's palette";
    UNKNOWN_TILE = "unknown_tile", Input,
        "a tile name resolves to nothing in the tileset";
    TILE_FILL_NOT_SELF_COMPATIBLE = "tile_fill_not_self_compatible", Input,
        "a TileGrid's fill tiles cannot sit beside themselves, so the solver's fallback is itself illegal";
    TILE_LAYOUT_MISSING = "tile_layout_missing", Input,
        "a TileGrid's \"layout\" names a file that is not there; run `engine synthesize`";
    TILE_LAYOUT_MALFORMED = "tile_layout_malformed", Input,
        "a tile layout does not parse, or its rows are not the grid the header declares";
    TILE_LAYOUT_MISMATCH = "tile_layout_mismatch", Input,
        "a tile layout's size or tileset disagrees with the component that names it";
    TILE_LAYOUT_STALE = "tile_layout_stale", Input,
        "a tile layout was solved from different inputs than the scene now holds; re-run `engine synthesize`";
    TILE_LAYOUT_ILLEGAL = "tile_layout_illegal", Input,
        "an unlocked cell in a tile layout violates the tileset's adjacency rules";
    TILE_GRID_TOO_COMPLEX = "tile_grid_too_complex", Input,
        "a TileGrid's cells and tiles would grow more vertices than the engine builds";
    TILE_GRID_GROUND_NOT_FOUND = "tile_grid_ground_not_found", Input,
        "a TileGrid's \"ground\" names no entity in the scene";
    TILE_GRID_GROUND_INVALID = "tile_grid_ground_invalid", Input,
        "a TileGrid's \"ground\" must name an entity that has a Terrain component";

    // ── UI system (M31) ────────────────────────────────────────────────
    HUD_PARENT_NOT_FOUND = "hud_parent_not_found", Input,
        "a HUD element's \"parent\" names no entity in the scene";
    HUD_PARENT_NOT_PANEL = "hud_parent_not_panel", Input,
        "a HUD element's \"parent\" must name an entity that has a HudPanel";
    HUD_PARENT_CYCLE = "hud_parent_cycle", Input,
        "a chain of HUD \"parent\" references loops back on itself";
    HUD_NESTING_TOO_DEEP = "hud_nesting_too_deep", Input,
        "a HUD element nests deeper than the layout engine will resolve";
    HUD_INTERACT_WITHOUT_ELEMENT = "hud_interact_without_element", Input,
        "a HudInteract needs a HudPanel, HudRect, HudImage or HudText on the same entity to be the hit box";
    HUD_IMAGE_SLICE_TOO_LARGE = "hud_image_slice_too_large", Input,
        "a HudImage's nine-slice insets are larger than the source image";

    // ── Input (M11) ────────────────────────────────────────────────────
    INPUT_UNREADABLE = "input_unreadable", Input,
        "the input timeline file could not be read";
    INPUT_PARSE_ERROR = "input_parse_error", Input,
        "an input timeline line is not a valid {\"step\", \"held\"} object";
    UNKNOWN_KEY = "unknown_key", Input,
        "an input timeline holds a name that is no known key";
    UNSORTED_INPUT_STEPS = "unsorted_input_steps", Input,
        "input timeline steps must be strictly increasing";

    // ── Warnings (severity: "warning"; do not affect the exit code
    //    unless promoted by --strict) ────────────────────────────────────
    UNUSED_MATERIAL = "unused_material", Input,
        "warning: a Material on an entity with no Mesh does nothing";
    ZERO_SCALE = "zero_scale", Input,
        "warning: a Transform.scale axis of 0 renders invisibly or degenerate";
    UNKNOWN_COLLISION_LAYER = "unknown_collision_layer", Input,
        "warning: collides_with names a layer no collider is a member of";
    DAYLIGHT_OVERRIDES_SKY = "daylight_overrides_sky", Input,
        "warning: daylight computes the sky and ambient, so the authored ones are never read";
    COLLIDER_MESH_SIZE_MISMATCH = "collider_mesh_size_mismatch", Input,
        "warning: a Collider is a very different size from the builtin mesh it sits on";
    GI_SUN_SAMPLES_UNUSED = "gi_sun_samples_unused", Input,
        "warning: LightProbeVolume.sun_samples asks for an arc, but this scene's sun does not move";
    ROAD_PINS_OVERLAP = "road_pins_overlap", Input,
        "warning: two pinned Road heights are closer together than follow_blend, so neither is reached exactly";
    ROAD_FOLLOW_ROTATED = "road_follow_rotated", Input,
        "warning: a Road following a Terrain is rolled or pitched by its own Transform, so its heights are skewed";
    TREE_SWAY_NEEDS_OPAQUE_BARK = "tree_sway_needs_opaque_bark", Input,
        "warning: a Tree asks for wind but its Material is transparent, and only the opaque pipelines carry the wind";
    TILE_SOCKET_ORPHANED = "tile_socket_orphaned", Input,
        "warning: a tile face carries a socket no other tile mates, so the tile can never be placed there";
    TILE_LAYOUT_FORCED = "tile_layout_forced", Input,
        "warning: a locked cell in a tile layout violates adjacency; the author asserted it and the engine draws it";
    TILE_GRID_SCALED = "tile_grid_scaled", Input,
        "warning: a non-unit Transform.scale rescales the cell metres a TileGrid's tileset declared";

    // ── Scene semantics at command time ───────────────────────────────
    SCENE_UNREADABLE = "scene_unreadable", Input,
        "the scene file could not be read";
    SCENE_PARSE_DESYNC = "scene_parse_desync", Environment,
        "internal bug: the scene passed validation but failed to parse";
    ENTITY_NOT_FOUND = "entity_not_found", Input,
        "no entity has the requested name";
    MISSING_COMPONENT = "missing_component", Input,
        "the entity exists but lacks the required component";
    NO_ACTIVE_CAMERA = "no_active_camera", Input,
        "no camera is marked active and none was named";

    // ── Editing (engine edit, formatter) ──────────────────────────────
    EDIT_TARGET_MISSING = "edit_target_missing", Input,
        "the entity, component, or field an edit targets is not in the file";
    SCENE_WRITE_FAILED = "scene_write_failed", Environment,
        "the scene file could not be written";
    FORMATTER_DESYNC = "formatter_desync", Environment,
        "internal bug: a formatted edit produced invalid JSON; nothing was written";
    EDITOR_FAILED = "editor_failed", Environment,
        "the editor window could not start or run";
    IMPORT_FAILED = "import_failed", Input,
        "a dropped file could not be converted or copied into the scene's assets";
    BLENDER_NOT_FOUND = "blender_not_found", Environment,
        "no Blender executable found to convert a .blend file; install Blender or set $BLENDER";

    // ── Assets ─────────────────────────────────────────────────────────
    ASSET_NOT_FOUND = "asset_not_found", Input,
        "an asset reference names no builtin and no existing file";
    ASSET_UNSUPPORTED = "asset_unsupported", Input,
        "the asset's format or feature is not one the engine reads";
    ASSET_PATH_NOT_RELATIVE = "asset_path_not_relative", Input,
        "asset paths must be relative to the scene file";
    ASSET_LOAD_FAILED = "asset_load_failed", Input,
        "the asset file exists but could not be parsed";
    TEXTURE_TOO_LARGE = "texture_too_large", Input,
        "a texture is larger on a side than the engine's device limit";
    MATERIAL_ASSET_WITH_FIELDS = "material_asset_with_fields", Input,
        "a Material that names an asset may not also set fields inline";

    // ── diff-render ────────────────────────────────────────────────────
    RENDER_MISMATCH = "render_mismatch", Input,
        "the rendered scene differs from its baseline beyond tolerance";
    BASELINE_NOT_FOUND = "baseline_not_found", Input,
        "the baseline PNG path does not exist";
    BASELINE_INVALID = "baseline_invalid", Input,
        "the baseline file is not a decodable, non-empty PNG";
    DIMENSION_MISMATCH = "dimension_mismatch", Input,
        "the two images being compared have different dimensions";

    // ── engine build ───────────────────────────────────────────────────
    COMPILE_ERROR = "compile_error", Input,
        "a rustc error, re-emitted with its file/line";
    COMPILE_WARNING = "compile_warning", Input,
        "warning: a rustc warning, re-emitted with its file/line";
    BUILD_FAILED = "build_failed", Input,
        "summary error when cargo reports compile errors";
    CARGO_ERROR = "cargo_error", Environment,
        "cargo itself failed with no compiler diagnostics";
    CARGO_NOT_FOUND = "cargo_not_found", Environment,
        "the cargo executable could not be run";

    // ── Rendering & GPU environment ────────────────────────────────────
    NO_GPU_ADAPTER = "no_gpu_adapter", Environment,
        "no usable GPU adapter was found";
    DEVICE_REQUEST_FAILED = "device_request_failed", Environment,
        "the GPU adapter refused a device";
    GPU_POLL_FAILED = "gpu_poll_failed", Environment,
        "waiting on the GPU failed";
    READBACK_FAILED = "readback_failed", Environment,
        "copying the rendered image back from the GPU failed";
    SURFACE_CREATION_FAILED = "surface_creation_failed", Environment,
        "a window surface could not be created";
    SURFACE_UNSUPPORTED = "surface_unsupported", Environment,
        "the surface configuration is not supported here";
    SURFACE_VALIDATION_ERROR = "surface_validation_error", Environment,
        "the surface rejected a frame";
    WINDOW_CREATION_FAILED = "window_creation_failed", Environment,
        "a window could not be created";
    EVENT_LOOP_CREATION_FAILED = "event_loop_creation_failed", Environment,
        "an event loop could not be created";
    EVENT_LOOP_FAILED = "event_loop_failed", Environment,
        "the event loop failed while running";
    PNG_WRITE_FAILED = "png_write_failed", Environment,
        "the output PNG could not be written";

    // ── engine init ────────────────────────────────────────────────────
    INIT_TARGET_NOT_EMPTY = "init_target_not_empty", Environment,
        "the directory to scaffold into already holds files; pass --force to write anyway";
    INIT_WRITE_FAILED = "init_write_failed", Environment,
        "a scaffolded file or directory could not be written";

    // ── Introspection queries (M24) ────────────────────────────────────
    UNKNOWN_COMPONENT_QUERY = "unknown_component_query", Input,
        "engine list-components --component names no known component";

    // ── Process-level ──────────────────────────────────────────────────
    INVALID_INVOCATION = "invalid_invocation", Environment,
        "the command line itself could not be parsed";
    INTERNAL_PANIC = "internal_panic", Environment,
        "internal bug: the process panicked; the protocol still held";
    OUTPUT_SERIALIZATION_FAILED = "output_serialization_failed", Environment,
        "internal bug: a result object failed to serialize";
    ERROR_SERIALIZATION_FAILED = "error_serialization_failed", Environment,
        "internal bug: an error object failed to serialize";
}

/// The exit code for an error code. Unregistered codes land on 2: an
/// unregistered code is itself an engine bug, which is class Environment.
pub fn exit_code(code: &str) -> i32 {
    REGISTRY
        .iter()
        .find(|entry| entry.code == code)
        .map(|entry| entry.class.code())
        .unwrap_or(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for entry in REGISTRY {
            assert!(seen.insert(entry.code), "{} registered twice", entry.code);
        }
    }

    #[test]
    fn codes_are_snake_case() {
        for entry in REGISTRY {
            assert!(
                entry
                    .code
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_'),
                "{} is not snake_case",
                entry.code
            );
        }
    }

    #[test]
    fn exit_codes_follow_the_class() {
        assert_eq!(exit_code(VALIDATION_FAILED), 1);
        assert_eq!(exit_code(NO_GPU_ADAPTER), 2);
        assert_eq!(exit_code("some_future_unregistered_code"), 2);
    }
}
