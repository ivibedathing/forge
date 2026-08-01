# Error codes

Every error the `engine` CLI emits carries one of the codes below in its
`error` field. **Codes are API**: agent scripts branch on them. Adding a code
is a feature; renaming or removing one is a breaking change to every consumer
in the wild.

The **exit** column is the process exit code when an error of this code is the
command's final result: `1` means the input files are at fault — edit them and
retry; `2` means the invocation or environment is at fault — fix the command
or the machine. Warnings (rows whose description starts with "warning:") ride
the same stderr stream with `"severity": "warning"` and do not affect the exit
code unless promoted by `engine validate --strict`.

This table is the human-facing copy of `engine-core/src/codes.rs`, and a
repo-contract test keeps the two in one-to-one agreement. Edit both or the
build fails.

| Code | Exit | Description |
|---|---|---|
| `invalid_json` | 1 | the file is not syntactically valid JSON |
| `scene_root_not_object` | 1 | the top-level JSON value is not an object |
| `missing_field` | 1 | a required field is absent |
| `unknown_field` | 1 | a field name is not part of the schema |
| `invalid_field_type` | 1 | a field holds a value of the wrong JSON type |
| `entity_not_object` | 1 | an entry in "entities" is not a JSON object |
| `missing_entity_name` | 1 | an entity has no "name" field |
| `empty_entity_name` | 1 | an entity's "name" is the empty string |
| `duplicate_entity_name` | 1 | two entities share a name |
| `component_not_object` | 1 | an entry in "components" is not a JSON object |
| `component_missing_type` | 1 | a component has no "type" field |
| `unknown_component` | 1 | a component's "type" names no known component |
| `duplicate_component` | 1 | the same component type appears twice on one entity |
| `value_out_of_range` | 1 | a numeric field is outside its documented range |
| `multiple_active_cameras` | 1 | more than one camera is marked active |
| `multiple_directional_lights` | 1 | more than one DirectionalLight in the scene |
| `multiple_ambient_lights` | 1 | more than one AmbientLight in the scene |
| `too_many_point_lights` | 1 | a scene may carry at most 8 PointLight components |
| `validation_failed` | 1 | summary error after per-diagnostic reports; see the preceding lines |
| `unknown_shape` | 1 | a Collider names no known shape kind |
| `unknown_body_kind` | 1 | a RigidBody names no known body kind |
| `invalid_shape_dimension` | 1 | a collider dimension must be strictly positive |
| `shape_field_mismatch` | 1 | a Collider field does not apply to its shape |
| `nonuniform_scale_on_round_collider` | 1 | sphere and capsule colliders cannot take a nonuniform Transform.scale |
| `invalid_physics_value` | 1 | a physics setting is outside its meaningful range |
| `missing_collider` | 1 | a dynamic RigidBody has no Collider and would fall through everything |
| `missing_transform` | 1 | a RigidBody or Collider needs a Transform on the same entity |
| `trimesh_on_dynamic_body` | 1 | a dynamic RigidBody cannot use a trimesh Collider; use convex_hull |
| `collider_missing_mesh` | 1 | a mesh-shaped Collider has no asset and its entity has no Mesh |
| `too_many_collision_layers` | 1 | a scene may name at most 32 distinct collision layers |
| `empty_collision_layers` | 1 | an empty layers/collides_with array; omit the field to mean everything |
| `unknown_entity` | 1 | an animation track targets an entity name not in the scene |
| `unknown_property` | 1 | an animation track targets no known Component.field |
| `type_mismatch` | 1 | a key value's shape does not match the animated field |
| `unsorted_keys` | 1 | key times must be strictly increasing |
| `conflicting_tracks` | 1 | two active clips animate the same property of the same entity |
| `animation_on_dynamic_body` | 1 | a clip animates the Transform of a dynamic RigidBody; make it kinematic |
| `clip_needs_fragment` | 1 | a glTF clip reference must name a clip: path#ClipName |
| `unknown_clip` | 1 | a #ClipName fragment names no animation in that glTF file |
| `mesh_has_no_skin` | 1 | a skeletal AnimationPlayer's glTF file carries no skin |
| `skeletal_player_mesh_mismatch` | 1 | a skeletal AnimationPlayer and its entity's Mesh name different files |
| `too_many_joints` | 1 | a skin has more joints than the fixed-size palette holds |
| `animation_stride_without_transform` | 1 | an AnimationPlayer sets stride but its entity has no Transform to measure |
| `foot_plant_without_skin` | 1 | a FootPlant is on an entity whose Mesh carries no skin |
| `foot_plant_ground_not_found` | 1 | a FootPlant's ground names no entity in the scene |
| `foot_plant_ground_not_terrain` | 1 | a FootPlant's ground names an entity with no Terrain |
| `foot_plant_non_uniform_scale` | 1 | a planted character's Transform.scale must be uniform |
| `foot_plant_chain_too_long` | 1 | a foot's chain reaches past the rig's root |
| `too_many_planted_feet` | 1 | a FootPlant lists more feet than the solver runs |
| `unknown_joint` | 1 | a joint name is not in the entity's rig |
| `skinned_collider_without_skin` | 1 | a SkinnedCollider is on an entity whose Mesh carries no skin |
| `skinned_collider_non_uniform_scale` | 1 | a proxied character's Transform.scale must be uniform |
| `duplicate_collider_part` | 1 | two parts of one SkinnedCollider report under the same name |
| `too_many_collider_parts` | 1 | a SkinnedCollider lists more parts than the physics world builds |
| `collider_part_shape_unsupported` | 1 | a SkinnedCollider part names a mesh shape, which a proxy cannot be |
| `ragdoll_without_proxies` | 1 | a Ragdoll is on an entity with no SkinnedCollider; the bodies are the proxies |
| `ragdoll_disconnected_parts` | 1 | a Ragdoll's parts form more than one tree, which is a ragdoll in pieces |
| `ragdoll_unknown_joint` | 1 | a Ragdoll joint override names a joint no part of the SkinnedCollider rides |
| `ragdoll_duplicate_joint` | 1 | two Ragdoll joint overrides name the same joint |
| `ragdoll_bad_hinge` | 1 | a Ragdoll hinge axis is zero-length, or its range runs backwards |
| `collider_part_fit_unsupported` | 1 | a sphere part asks to fit the bone, and a sphere has no length to solve |
| `breakable_without_collider` | 1 | a Breakable sets impulse_threshold but the entity has no Collider to be hit on |
| `invalid_environment_value` | 1 | an environment setting is outside its meaningful range |
| `invalid_daylight_value` | 1 | a daylight setting is outside its meaningful range |
| `daylight_palette_invalid` | 1 | a daylight palette needs at least two keyframes with strictly increasing hours |
| `daylight_and_directional_light` | 1 | daylight drives the sun, so the scene may not also author a DirectionalLight |
| `water_with_mesh` | 1 | a Water entity owns its own surface; it may not also have a Mesh or a Material |
| `water_waves_self_intersect` | 1 | the sum of Water wave steepness exceeds 1, which folds the surface through itself |
| `terrain_with_mesh` | 1 | a Terrain entity owns its own surface; it may not also have a Mesh or a Material |
| `terrain_layer_range_inverted` | 1 | a Terrain layer's height or slope range runs backwards, so it covers nothing |
| `road_with_mesh` | 1 | a Road entity owns its own surface; it may not also have a Mesh or a Material |
| `road_too_few_points` | 1 | a Road needs at least two centerline points, or three to close |
| `road_corner_does_not_fit` | 1 | two corner radii need more of the edge between them than it has |
| `road_corner_needs_radius` | 1 | a sharp corner turns too far to mitre; give it a radius |
| `too_many_road_kerbs` | 1 | a Road kerbs more corners than the shader's span array holds |
| `script_parse_error` | 1 | a script file does not compile |
| `script_missing_step_fn` | 1 | a script defines no `fn step(world, step)` |
| `script_runtime_error` | 1 | a script failed while running |
| `wheel_vehicle_not_found` | 1 | a Wheel's "vehicle" names no entity in the scene |
| `wheel_vehicle_invalid` | 1 | a Wheel's "vehicle" must be a different entity with a dynamic RigidBody |
| `wheel_with_physics` | 1 | a Wheel entity may not have its own RigidBody or Collider; the chassis owns all collision |
| `tree_with_mesh` | 1 | an entity may not have both a Tree and a Mesh; a Tree is the entity's geometry |
| `tree_too_complex` | 1 | a Tree's parameters would generate more vertices than the engine will grow |
| `cloud_with_mesh` | 1 | a Cloud entity owns its own geometry; it may not also have a Mesh or a Material |
| `cloud_too_complex` | 1 | a Cloud's parameters would generate more vertices than the engine will grow |
| `meadow_with_mesh` | 1 | a Meadow entity owns its own geometry; it may not also have a Mesh or a Material |
| `meadow_too_complex` | 1 | a Meadow's density and footprint would grow more triangles than the engine will draw |
| `meadow_terrain_not_found` | 1 | a Meadow's "terrain" names no entity in the scene |
| `meadow_terrain_invalid` | 1 | a Meadow's "terrain" must name an entity that has a Terrain component |
| `meadow_stages_invalid` | 1 | a Meadow needs at least two life-cycle stages with strictly increasing "at" |
| `too_many_growth_stages` | 1 | a Meadow has more life-cycle stages than the shader's table holds |
| `buoyancy_water_missing` | 1 | a Buoyancy must name the Water entity it floats on |
| `buoyancy_water_not_found` | 1 | a Buoyancy's "water" names no entity in the scene |
| `buoyancy_water_invalid` | 1 | a Buoyancy's "water" must name an entity that has a Water component |
| `buoyancy_without_body` | 1 | a Buoyancy needs a dynamic RigidBody and a Collider on the same entity |
| `hud_parent_not_found` | 1 | a HUD element's "parent" names no entity in the scene |
| `hud_parent_not_panel` | 1 | a HUD element's "parent" must name an entity that has a HudPanel |
| `hud_parent_cycle` | 1 | a chain of HUD "parent" references loops back on itself |
| `hud_nesting_too_deep` | 1 | a HUD element nests deeper than the layout engine will resolve |
| `hud_interact_without_element` | 1 | a HudInteract needs a HudPanel, HudRect, HudImage or HudText on the same entity to be the hit box |
| `hud_image_slice_too_large` | 1 | a HudImage's nine-slice insets are larger than the source image |
| `input_unreadable` | 1 | the input timeline file could not be read |
| `input_parse_error` | 1 | an input timeline line is not a valid {"step", "held"} object |
| `unknown_key` | 1 | an input timeline holds a name that is no known key |
| `unsorted_input_steps` | 1 | input timeline steps must be strictly increasing |
| `unused_material` | 1 | warning: a Material on an entity with no Mesh does nothing |
| `zero_scale` | 1 | warning: a Transform.scale axis of 0 renders invisibly or degenerate |
| `unknown_collision_layer` | 1 | warning: collides_with names a layer no collider is a member of |
| `daylight_overrides_sky` | 1 | warning: daylight computes the sky and ambient, so the authored ones are never read |
| `collider_mesh_size_mismatch` | 1 | warning: a Collider is a very different size from the builtin mesh it sits on |
| `scene_unreadable` | 1 | the scene file could not be read |
| `scene_parse_desync` | 2 | internal bug: the scene passed validation but failed to parse |
| `entity_not_found` | 1 | no entity has the requested name |
| `missing_component` | 1 | the entity exists but lacks the required component |
| `no_active_camera` | 1 | no camera is marked active and none was named |
| `edit_target_missing` | 1 | the entity, component, or field an edit targets is not in the file |
| `scene_write_failed` | 2 | the scene file could not be written |
| `formatter_desync` | 2 | internal bug: a formatted edit produced invalid JSON; nothing was written |
| `editor_failed` | 2 | the editor window could not start or run |
| `import_failed` | 1 | a dropped file could not be converted or copied into the scene's assets |
| `blender_not_found` | 2 | no Blender executable found to convert a .blend file; install Blender or set $BLENDER |
| `asset_not_found` | 1 | an asset reference names no builtin and no existing file |
| `asset_unsupported` | 1 | the asset's format or feature is not one the engine reads |
| `asset_path_not_relative` | 1 | asset paths must be relative to the scene file |
| `asset_load_failed` | 1 | the asset file exists but could not be parsed |
| `texture_too_large` | 1 | a texture is larger on a side than the engine's device limit |
| `material_asset_with_fields` | 1 | a Material that names an asset may not also set fields inline |
| `render_mismatch` | 1 | the rendered scene differs from its baseline beyond tolerance |
| `baseline_not_found` | 1 | the baseline PNG path does not exist |
| `baseline_invalid` | 1 | the baseline file is not a decodable, non-empty PNG |
| `dimension_mismatch` | 1 | the two images being compared have different dimensions |
| `compile_error` | 1 | a rustc error, re-emitted with its file/line |
| `compile_warning` | 1 | warning: a rustc warning, re-emitted with its file/line |
| `build_failed` | 1 | summary error when cargo reports compile errors |
| `cargo_error` | 2 | cargo itself failed with no compiler diagnostics |
| `cargo_not_found` | 2 | the cargo executable could not be run |
| `no_gpu_adapter` | 2 | no usable GPU adapter was found |
| `device_request_failed` | 2 | the GPU adapter refused a device |
| `gpu_poll_failed` | 2 | waiting on the GPU failed |
| `readback_failed` | 2 | copying the rendered image back from the GPU failed |
| `surface_creation_failed` | 2 | a window surface could not be created |
| `surface_unsupported` | 2 | the surface configuration is not supported here |
| `surface_validation_error` | 2 | the surface rejected a frame |
| `window_creation_failed` | 2 | a window could not be created |
| `event_loop_creation_failed` | 2 | an event loop could not be created |
| `event_loop_failed` | 2 | the event loop failed while running |
| `png_write_failed` | 2 | the output PNG could not be written |
| `init_target_not_empty` | 2 | the directory to scaffold into already holds files; pass --force to write anyway |
| `init_write_failed` | 2 | a scaffolded file or directory could not be written |
| `unknown_component_query` | 1 | engine list-components --component names no known component |
| `invalid_invocation` | 2 | the command line itself could not be parsed |
| `internal_panic` | 2 | internal bug: the process panicked; the protocol still held |
| `output_serialization_failed` | 2 | internal bug: a result object failed to serialize |
| `error_serialization_failed` | 2 | internal bug: an error object failed to serialize |
