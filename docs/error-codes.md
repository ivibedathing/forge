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
| `breakable_without_collider` | 1 | a Breakable sets impulse_threshold but the entity has no Collider to be hit on |
| `invalid_environment_value` | 1 | an environment setting is outside its meaningful range |
| `script_parse_error` | 1 | a script file does not compile |
| `script_missing_step_fn` | 1 | a script defines no `fn step(world, step)` |
| `script_runtime_error` | 1 | a script failed while running |
| `wheel_vehicle_not_found` | 1 | a Wheel's "vehicle" names no entity in the scene |
| `wheel_vehicle_invalid` | 1 | a Wheel's "vehicle" must be a different entity with a dynamic RigidBody |
| `wheel_with_physics` | 1 | a Wheel entity may not have its own RigidBody or Collider; the chassis owns all collision |
| `input_unreadable` | 1 | the input timeline file could not be read |
| `input_parse_error` | 1 | an input timeline line is not a valid {"step", "held"} object |
| `unknown_key` | 1 | an input timeline holds a name that is no known key |
| `unsorted_input_steps` | 1 | input timeline steps must be strictly increasing |
| `unused_material` | 1 | warning: a Material on an entity with no Mesh does nothing |
| `zero_scale` | 1 | warning: a Transform.scale axis of 0 renders invisibly or degenerate |
| `unknown_collision_layer` | 1 | warning: collides_with names a layer no collider is a member of |
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
| `invalid_invocation` | 2 | the command line itself could not be parsed |
| `internal_panic` | 2 | internal bug: the process panicked; the protocol still held |
| `output_serialization_failed` | 2 | internal bug: a result object failed to serialize |
| `error_serialization_failed` | 2 | internal bug: an error object failed to serialize |
