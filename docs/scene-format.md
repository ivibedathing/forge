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

`name` and `entities` are required. `physics`, `environment`, `daylight` and
`templates` are optional scene-level blocks — siblings of `entities`, not
components, because they describe the world rather than a thing in it.

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

**`templates` are entities that do not exist yet.** A `templates` entry is an
entity definition plus a `limit`, declared but never instantiated: nothing
renders it, physics never sees it, and `simulate` never traces it. A script
brings one into the world with `world.spawn_entity("Bullet", x, y, z)`, which
returns the new entity's name — `Bullet#1`, `Bullet#2`, … — and
`world.despawn_entity(name)` takes it back out.

```json
"templates": [
  {
    "name": "Bullet",
    "limit": 48,
    "components": [
      { "type": "Transform", "scale": [0.1, 0.1, 0.1] },
      { "type": "Mesh", "asset": "builtin:sphere" },
      { "type": "RigidBody", "body": "dynamic", "gravity_scale": 0.0 },
      { "type": "Collider", "shape": "sphere", "radius": 0.5 }
    ]
  }
]
```

The block exists so that a spawn names something *the file already declares*
rather than constructing geometry from a script, which would put scene data in
a `.rhai` and stop the scene file being the whole truth about what can exist.
Four consequences worth knowing before you write one:

- **Template names share the entity name space.** A template may not take an
  entity's name, because a script addresses both by name.
- **The spawn call sets the position and nothing else.** A template's own
  `rotation` and `scale` survive; a template with no `Transform` gets one.
- **`limit` is the most instances that may be *live* at once**, default 64.
  Spawning at the limit spawns nothing and returns the empty string, which is a
  value the script checks — not an error, because a gun that fires faster than
  its bullets expire is an ordinary game.
- **Five components are refused inside a template**: `Script`, `Camera`,
  `DirectionalLight`, `AmbientLight` and `PointLight`. Each has a scene-level
  budget that validation checks, and a spawn must not be able to make a valid
  scene invalid at step 40.

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
- **Every `builtin:` primitive is one metre across at scale 1**, centred on the
  origin, so `Transform.scale` reads as a size in metres. `builtin:triangle` is
  the exception — it is the original stack proof, not a modelling shape.
- **`Collider` dimensions are in the entity's own units**, and `Transform.scale`
  multiplies them the same way it multiplies the mesh. A collider matching a
  builtin is therefore written `"half_extents": [0.5, 0.5, 0.5]` or
  `"radius": 0.5` at *every* scale. `Collider.density` is kg/m³ and its default
  of `1.0` is not a plausible material — mass is `density × shape volume`, so
  anything meant to be pushed by forces wants a real one.

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

Since M47 there are two more, both named by a `TileGrid`, and both routed by
**shape** rather than by filename the way clips and materials are:

**A tileset (`tilesets/*.json`)** holds a `cell` size in metres, a `palette` of
named materials, and a list of `tiles`. A tile is *grown, not modelled*: it
carries parametric `parts` — `box`, `wedge`, `cylinder` — in the same `at`
(centre) and `size` (full extent) metres a `Transform` uses, cell-local with the
origin at the cell's centre in X and Z and its floor in Y. Each of its six
`faces` carries a socket string saying what may sit against it:

| Written | Mates | Meaning |
|---|---|---|
| `"0"` | `"0"` | nothing here; reserved |
| `"x"` | `"x"` | symmetric — the common case, so it has no suffix |
| `"x_l"` | `"x_r"` | one half of a mirrored pair |
| `"x_i"` | ignores the turn | vertical faces only |

A tileset may also carry **`constraints`** (M49): region properties the solver
must satisfy, which face adjacency cannot state. One shape with four optional
predicates over a set of tiles named by their authored names — `count` (how many
cells), `regions` (how many connected regions; `max: 1` is connectivity),
`region_size` (how big each is) and `region_contains` (what each must hold).
They are evaluated on the ground layer, 4-connected in XZ, and a terrace step
does not split a region. Without them the village came out as one 60-cell mass
and the tour's hamlet as walls enclosing no rooms at all.

`rotations` of 1, 2 or 4 expands the tile over quarter-turns about Y before
anything else runs; a vertical socket keeps its rotation index unless suffixed
`_i`. A tileset's own references — a palette entry's `asset`, and that
material's texture maps — are relative to **the tileset file**, which is what
makes one shareable, and they are rebased onto the scene at load.

**A layout (`layouts/*.tiles.json`)** is the solved grid, written by
`engine synthesize`: NDJSON, a header object then one line per grid row, cells
`name@rotation` separated by spaces. **The line order is the layout** — x
fastest, then z, then y — and it is checked, because a permuted file parses as
valid JSON and renders a wrong world. A `!` before a token locks that cell: a
hard constraint the solver never re-picks, byte-identical after a full re-solve,
and the way an author says "the door goes *there*".

The grid's **vertical ends are closed and its horizontal edges are open** by
default: a patch is a window onto a larger world sideways, but there is no
storey below the ground or above the sky. That single rule is what keeps roofs
off the ground floor without any tile having to say so. Since M51 the
component may say `"edges": "closed"`, which constrains every free edge — the
border **and** every terrace seam — as the fill pair (street at ground, air
above), so a structure must complete inside the grid and on its own terrace.
Closed is what keeps a village's houses whole; open remains the default and is
M47 exactly.

**A locked floor cell is a building plot** (M51). Under a tileset whose floors
only mate wall interiors, a `!floor@0` in the layout forces propagation to
grow a complete house around it — which is the reliable way to *place*
buildings, since a constraint solver rarely completes a large structure by
luck. `clear_tiles` and `--reset` both keep locks, so plots survive every
rebuild.

Since M50 a script can re-solve part of a grid while the scene runs:
`world.synthesize("Village", x, z, radius)` re-solves the blocks meeting a
world-space disc — with an optional fifth argument for the seed — and
`world.clear_tiles("Village")` puts every unlocked cell back to the tileset's
fill, keeping the locks. Both are queued and applied between the scripts and
physics, like `world.spawn_entity`. **The layout file is still where a run
starts and nothing writes back**, so a runtime arrangement lives only in the
run; `engine tile-grid --steps N` is how to read one. A `TileGrid` carrying a
`Collider` is refused, because its geometry is also a physics trimesh.
