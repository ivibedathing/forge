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
| `validation_failed` | 1 | summary error after per-diagnostic reports; see the preceding lines |
| `unused_material` | 1 | warning: a Material on an entity with no Mesh does nothing |
| `zero_scale` | 1 | warning: a Transform.scale axis of 0 renders invisibly or degenerate |
| `scene_unreadable` | 1 | the scene file could not be read |
| `scene_parse_desync` | 2 | internal bug: the scene passed validation but failed to parse |
| `entity_not_found` | 1 | no entity has the requested name |
| `missing_component` | 1 | the entity exists but lacks the required component |
| `no_active_camera` | 1 | no camera is marked active and none was named |
| `edit_target_missing` | 1 | the entity, component, or field an edit targets is not in the file |
| `scene_write_failed` | 2 | the scene file could not be written |
| `formatter_desync` | 2 | internal bug: a formatted edit produced invalid JSON; nothing was written |
| `editor_failed` | 2 | the editor window could not start or run |
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
