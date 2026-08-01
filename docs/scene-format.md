# The scene format

A scene is one JSON file. It is the whole state: everything needed to
reconstruct what you see is text on disk, addressed by name, with no
editor-side database and no opaque IDs. `git diff` is an exact record of what
changed.

This document covers the rules the schema cannot express. For the components
themselves — every field, its type, its default — see
[`component-reference.md`](component-reference.md), which is generated from the
same schema `engine list-components` publishes. For the CLI's wire contract,
see [`cli-contract.md`](cli-contract.md).

## The shape of a file

<!-- validated -->
```json
{
  "name": "worked_example",
  "physics": { "gravity": [0.0, -9.81, 0.0], "timestep_hz": 60 },
  "environment": { "sky": true, "shadows": true, "samples": 4 },
  "entities": [
    {
      "name": "Camera",
      "components": [
        { "type": "Transform", "position": [0.0, 3.0, 8.0], "rotation": [-12.0, 0.0, 0.0] },
        { "type": "Camera", "active": true }
      ]
    },
    {
      "name": "Sun",
      "components": [
        { "type": "Transform", "rotation": [-50.0, -30.0, 0.0] },
        { "type": "DirectionalLight", "intensity": 3.0 }
      ]
    },
    {
      "name": "Ground",
      "components": [
        { "type": "Transform", "scale": [40.0, 1.0, 40.0] },
        { "type": "Mesh", "asset": "builtin:plane" },
        { "type": "Material", "albedo": [0.35, 0.38, 0.32] },
        { "type": "Collider", "shape": "cuboid", "half_extents": [0.5, 0.001, 0.5] }
      ]
    },
    {
      "name": "Crate",
      "components": [
        { "type": "Transform", "position": [0.0, 4.0, 0.0] },
        { "type": "Mesh", "asset": "builtin:cube" },
        { "type": "Material", "albedo": [0.6, 0.4, 0.2], "roughness": 0.7 },
        { "type": "RigidBody", "body": "dynamic" },
        { "type": "Collider", "shape": "cuboid", "half_extents": [0.5, 0.5, 0.5], "density": 12.0 }
      ]
    }
  ]
}
```

That file renders, simulates and validates as it stands. A repo contract test
checks exactly this block, so it cannot rot silently.

`name` and `entities` are required. `physics`, `environment` and `daylight` are
optional scene-level blocks — siblings of `entities`, not components, because
they describe the world rather than a thing in it.

## Rules the schema cannot express

**Components are internally tagged.** Each carries a `"type"` naming its
component, and its fields sit beside the tag rather than nested under it. There
is no wrapper object and no array-of-one-key form.

**An absent field *is* the documented default.** Scenes under-specify entities
by design — `{"type": "Transform", "position": [0, 3, 0]}` is a complete
Transform with an identity rotation and unit scale. This is why deleting a
field is how you reset it, and why `engine inspect` exists: it prints an
entity with every default filled in, which the file deliberately does not.

**Entity names are addresses.** `Wheel.vehicle`, `Meadow.ground`,
`HudPanel.parent` and every CLI `--entity` flag resolve by name, so names must
be unique within a scene and renaming one is a breaking edit. Components refer
to each other by name in a flat list rather than by nesting, which is what
keeps a scene diffable: moving a child in the hierarchy changes one string, not
the indentation of a subtree.

**Assets are relative paths, never IDs.** `Mesh.asset`, `Material.albedo_map`,
`AnimationPlayer.clip` and `Script.source` resolve **relative to the scene
file**, not to the working directory or the project root. A scene one directory
down reaches its scripts through `../scripts/`. Absolute paths are rejected.
The one exception is the `builtin:` prefix — `builtin:cube`, `builtin:sphere`,
`builtin:plane`, `builtin:cylinder`, `builtin:triangle` — which names geometry
the engine generates rather than a file.

**Some components own their geometry.** `Water`, `Terrain`, `Road`, `Cloud` and
`Meadow` are recipes, not mesh references: each generates its own surface from
its fields, so the entity carries **no** `Mesh` and **no** `Material`, and
saying otherwise is a validation error. `Tree` is the near-exception — it grows
its own geometry too, but the entity's `Material` is its bark.

**JSON has no comments, and that is a real cost.** It was accepted deliberately
(the agent loop is specified as ordinary bash, and `jq` is ordinary bash while
a commented dialect is not). The consequence is a rule: anything a scene needs
to say about itself has to be a real field. If you find yourself wanting to
annotate a value, the annotation belongs in the component, in the design doc,
or in the name of the entity.

## Angles, colours and units

- **Rotations are Euler angles in degrees**, `[x, y, z]`, applied X→Y→Z. An
  agent told to rotate 45° about Y writes `[0.0, 45.0, 0.0]`; an unlabelled
  quaternion invites silent ordering bugs.
- **Cameras and lights aim down their local −Z**, the same convention for both.
  A particle emitter sprays along −Z too, which is why rising smoke is
  `"rotation": [90, 0, 0]`.
- **Colours are linear RGB** in `[0, 1]`, not sRGB bytes. The render target
  does the encoding.
- **Distances are metres, mass is kilograms, time is seconds.** Angular
  velocity is degrees per second in the file and converted at the physics
  boundary.

## Checking a file

```bash
engine validate scene.json          # every error at once, no GPU, ~0.02s
engine validate scene.json --strict # promote warnings to failures
engine inspect scene.json           # every field resolved, defaults filled in
engine list-components              # the vocabulary, as JSON Schema
```

`validate` reports **every** problem in one run rather than stopping at the
first, and each error carries the file, the line, a JSON Pointer in `path`, and
a `did_you_mean` when a name is close to a real one. Exit code 1 means the
files are at fault and 2 means the invocation or environment is — the split is
specified in [`cli-contract.md`](cli-contract.md).

Warnings ride the same stream with `"severity": "warning"` and do not fail the
run unless `--strict` is passed. They mark scenes that load but probably do not
mean what they say: a `Material` on an entity with no `Mesh`, a zero scale.

## Related files

Animation clips (`*.anim.json`) and material files (`materials/*.json`) are
separate documents with their own schemas, and `engine validate` accepts both
directly. A material file is referenced by `Material.asset`, which is exclusive
with every other field on that component — a shared material cannot be tinted
per entity, by design.
