# Building with Forge

Forge is a 3D engine whose primary user is a coding agent rather than a human in
a GUI editor. Scenes are JSON, errors are structured JSON on stderr, and
rendering a frame is one headless command that writes a PNG. You author a scene
by editing a text file, validating it, rendering it, and **looking at the
image** — using ordinary file edits and bash, with no integration layer.

This file is the whole orientation. Everything in it is reachable from the
`engine` binary; nothing here needs the engine's source checked out.

## The loop

1. Edit a `.json` scene file.
2. `engine validate first.json` — every error at once, with file, line,
   and a JSON Pointer into the offending file.
3. `engine screenshot first.json --out /tmp/check.png`
4. **Read the PNG.** You can view images directly. This step is the point of
   the engine; skipping it means you are authoring blind.
5. Iterate.

`engine screenshot` is the most important command here. When a scene has
physics, scripts, or particles, add `--steps N` to advance the fixed clock
before the frame is drawn — `--steps 0` renders the scene at rest.

## Commands

```
engine validate <scene.json>... [--strict]        # every error at once; --strict promotes warnings
engine screenshot <scene.json> --out x.png [--steps N] [--time T] [--camera Name]
                  [--width W --height H] [--input f.input.jsonl]
engine diff-render <scene.json> <baseline.png> [--steps N] [--out diff.png]
                   [--threshold N] [--max-diff-percent P]
engine filmstrip <scene.json> --out strip.png [--start S --end E --frames N --columns C]
engine simulate <scene.json> --steps N [--entity Name]... [--bake out.json] [--trace t.jsonl]
engine raycast <scene.json> --from x,y,z --dir x,y,z [--steps N]
engine road-centerline <scene.json> [--entity Name]
engine list-colliders <scene.json> [--entity Name] [--steps N]  # every collider, and where
engine ui-layout <scene.json> [--width W --height H] [--entity Name] [--steps N]  # where the UI landed
engine terrain-height <scene.json> --at x,z [--entity Name]   # where the ground is
engine inspect <scene.json> [--entity Name]       # every field, with the defaults filled in
engine list-components [--component Name]         # the scene + component JSON Schemas
engine list-animations <scene-or-clip-or-gltf> [--schema]
engine list-joints <scene-or-gltf> [--entity Name] [--time T] [--steps N]  # where every joint is
engine import <model.glb> [--into scene.json]     # a model's materials, as files
engine run-scene <scene.json> [--record-input f]  # windowed viewer; keyboard reaches scripts
engine edit <scene.json> [--watch]                # GUI editor: a live view onto the file
engine info                                       # the selected GPU adapter
engine agent-guide                                # this document
```

`engine --help` and `engine <command> --help` are authoritative for flags.

## How the CLI talks to you

- **stdout** is exactly one JSON object on success, and **nothing** on failure.
- **stderr** is NDJSON: one complete error or warning object per line, always.
- **Exit 0** success, **1 your files are at fault** (fix the scene and retry),
  **2 your invocation or environment is at fault** (bad flag, no GPU adapter).

So `engine validate scene.json 2>errors.ndjson; echo $?` and a `jq` over the
result is a complete integration. Every error line carries a stable `error`
code — branch on that, never on `message`. Useful fields: `file`, `line`,
`path` (a JSON Pointer such as `/entities/3/components/0/asset`), `entity`,
`component`, `field`, and `did_you_mean` when your name is a near miss.

Warnings ride the same stream with `"severity": "warning"` and do not change
the exit code unless you pass `--strict`.

**Every command reports every error.** `validate`, `screenshot`, and
`run-scene` run the same validation pipeline, so which command you ran never
changes what you learn about a broken scene.

## The scene file

```json
{
  "name": "first",
  "environment": { "sky": true, "shadows": true, "samples": 4 },
  "entities": [
    {
      "name": "Camera",
      "components": [
        { "type": "Transform", "position": [0.0, 3.0, 12.0], "rotation": [-8.0, 0.0, 0.0] },
        { "type": "Camera", "fov": 55.0, "near": 0.1, "far": 500.0, "active": true }
      ]
    }
  ]
}
```

- Entities have a **unique `name`**, and names are the addressing scheme: the
  CLI, scripts, and animation clips all target entities by name.
- Components are plain data, internally tagged with `"type"`. One component of
  a given type per entity.
- **An absent field is its documented default.** You never have to write a
  field to get the default behaviour, and adding `{ "type": "Material" }` with
  nothing else is legal.
- Asset paths are **relative to the scene file**. Absolute paths are rejected.
  A `materials/*.json` is the exception that proves it: its own texture
  references are relative to *it*, which is what lets two scenes share one.
- Scene-level blocks beside `entities`: `physics`, `environment`, `daylight`.

There are no comments in JSON, so anything a scene needs to say about itself
has to be a real field.

## Discovering what exists

`engine list-components` prints the scene schema and every component schema as
one JSON object, generated from the engine's own types — it cannot drift from
what the engine accepts. It is the authoritative field list, including ranges.

Its shape: `.components` is the list of type names, `.component` is a `oneOf`
over every component schema (discriminated by `.properties.type.const`), and
`.scene` is the schema for the file as a whole. `--component <Name>` does that
selection for you and prints one schema.

```bash
engine list-components --component Terrain                     # one component, whole
engine list-components --component Terrain | jq -r '.properties | keys[]'
engine list-components | jq -r '.components[]'                 # every component type
engine list-components | jq -r '.scene.properties | keys[]'    # the scene-level blocks
```

Do this rather than guessing a field name. A guessed field comes back as
`unknown_field` with a `did_you_mean`, which is a fine way to find out, but the
schema is faster.

## Asking about a scene you have

The schema says what a component *can* hold; these say what yours *does*.

```bash
engine inspect scene.json --entity Ground        # every field, defaults filled in
engine terrain-height scene.json --at -12,8      # the world Y of the ground there
engine road-centerline scene.json                # where a Road actually goes
engine list-joints scene.json --entity Robot --time 0.7   # where every joint is
engine ui-layout scene.json --width 1280 --height 720     # every HUD element's rectangle
engine list-colliders scene.json --steps 90               # every collider, and where it is
engine raycast scene.json --from -6,20,6 --dir 0,-1,0
```

`engine inspect` matters more than it sounds: **an absent field in the file is
the documented default, not an unset value**, so a `Material` that writes only
`albedo` is four values you cannot see by reading the JSON. It reports the scene
at rest — for what a scene *does*, `simulate --steps N`.

`engine terrain-height` is the height field, not a raycast: it needs no
`Collider`, and it is the same sampler `world.terrain_height` answers with in a
script, so a prop you place from the shell lands where a script would put it.

`engine ui-layout` is the same idea for the screen. A UI is laid out from
anchors, hug sizing and a `parent` tree, so **where a button ends up is not
something you can read off the file** — and you cannot click one you cannot
locate. It reports every element's pixel rectangle at a given frame size, which
is how you write the cursor that hits it: a timeline's cursor is a *fraction*
of the frame, so the centre of a reported `[x, y, w, h]` is
`[(x + w/2) / width, (y + h/2) / height]`. Layout is a pure function of the
file and the viewport, so the answer is stable and needs no GPU.

`engine list-colliders` answers the question physics never used to: where the
collision geometry actually is, shape and size included, read back out of the
built world rather than re-derived from the file. It matters most for a
`SkinnedCollider` — the hitboxes that ride a character's joints, which appear in
no render at all and whose placement comes from a pose. `--steps N` when the
pose is one the simulation reached. A proxy's row carries a `part`, and so does
a `raycast` that hits one: `entity` stays the character, `part` says where you
shot it.

`engine list-joints` is the same idea for a rigged mesh, and it is how you check
an animation without reading pixels: a filmstrip shows that *something* moved
and never that the hand reached the doorknob. Without `--time` it reports the
rig — name, parent, index, rest transform; with it, each joint's posed world
transform at that moment. It needs no GPU. Use `--steps N` instead when the
clip is driven by the *simulation* rather than by the clock — a walk cycle whose
`AnimationPlayer.stride` is set advances with the ground its entity covers, so
its phase is something the run reached rather than something the file says. On
an entity that also has a `FootPlant`, the report carries the `stride` the clip
itself measures, which is the number that field wants. Scripts ask the same
question with
`world.joint_position(entity, joint)`, which is how you hang a prop off a hand:
there is no way to *move* a joint, deliberately, so a character's pose stays a
function of its files and the clock.

Negative coordinates are ordinary arguments — `--from -6,20,6` needs no `=`.

## Built-in meshes

`Mesh.asset` is either a `builtin:` primitive or a `.gltf` / `.glb` path
relative to the scene:

```
builtin:cube  builtin:sphere  builtin:cylinder  builtin:plane  builtin:triangle
```

**Each of them is one metre across at scale 1**, centred on the origin — a cube
spans −0.5..0.5, a sphere is 0.5 in radius, a cylinder is 1 m tall and 1 m
wide, a plane is a 1 m square. So `Transform.scale` reads directly as a size in
metres: `"scale": [1.7, 0.7, 3.6]` on a cube is a car-sized box. (`builtin:triangle`
is the original stack-proof triangle and is the one shape that is not on this
grid.)

That matters most where a `Collider` sits on the same entity, because collider
dimensions are in the entity's **own** units and `Transform.scale` multiplies
them too. A cuboid matching a builtin mesh is always `"half_extents": [0.5,
0.5, 0.5]` and a sphere is always `"radius": 0.5`, whatever the scale — write
the world measurement into either and you get a shape scaled twice, which draws
at one size and collides at another. `engine validate` warns
(`collider_mesh_size_mismatch`) when the two disagree by more than a quarter.

Several components own their geometry instead of referencing a mesh, and having
both is a validation error: `Terrain`, `Water`, `Road`, `Tree`, and `Cloud` are
recipes the engine grows on load. A `Terrain` or `Road` entity carries no `Mesh`
and no `Material`; a `Collider` with `"shape": "trimesh"` and no `asset` borrows
that generated surface, which is how ground becomes collidable without a mesh
file.

## Texture maps and shared materials

A `Material` can carry `albedo_map`, `orm_map` (occlusion / roughness /
metallic, glTF's packing), `normal_map` and `emissive_map` — `.png` paths
relative to the scene — with `uv_scale` / `uv_offset` for tiling. Each map
**multiplies** its factor rather than replacing it, so `albedo` is a tint over
`albedo_map`: write `"albedo": [1, 1, 1]` beside a map unless you want the 0.8
default darkening it. `alpha_cutoff` above 0 discards transparent pixels and
their shadow, which is how a foliage card works. `ior` / `thickness` /
`attenuation` bend and absorb what is behind a transmissive surface.

You never say which colour space a texture is in: the **slot** decides. Albedo
and emissive are colours and are decoded; ORM and normal are data and are not.

A material can also *be* a file:

```json
{ "type": "Material", "asset": "materials/asphalt.json" }
```

holding the same fields minus the `"type"`. `asset` is exclusive with every
other field — setting both is `material_asset_with_fields` — because an absent
field and a field written at its default are the same thing to the parser, so a
partial override would resolve to something the file does not say. A variant is
a second file. A material file's own texture paths are relative to **it**, and
`engine validate materials/asphalt.json` checks one directly.

`engine import model.glb --into scene.json` writes a model's materials out as
those files, with any embedded textures as PNGs beside them, and splices an
entity that references them.

## Conventions that will trip you up

- **Cameras and lights aim down their entity's local −Z.** A light with no
  rotation shines toward −Z, not down. Rising smoke is `"rotation": [90, 0, 0]`.
- **Colors are linear RGB in `[0, 1]`**, not sRGB bytes. Render targets encode
  to sRGB on write, so a `0.5` albedo is not a `128` pixel.
- **`rotation` is XYZ Euler degrees.** The middle angle is clamped to ±90°, so a
  yaw integrated past that comes back as the `(±180, θ, ±180)` twin and
  `rotation[1]` stops being "the yaw". In scripts, use `world.forward(name)` for
  heading math instead of reading `rotation[1]`.
- **Angular velocity is degrees per second** in the file, everywhere.
- A scene with **zero light components** gets a documented fallback rig. As soon
  as any light exists, absent means off — which is the usual reason a first
  scene renders black.
- `--steps` advances the fixed simulation clock (physics, scripts, particles).
  `--time` poses animations, water, and clouds without simulating. **Particles
  exist only under `--steps`**; a `--steps 0` render draws none.

## Physics

Add a scene-level `"physics"` block for gravity and timestep, a `RigidBody`
(`dynamic` / `kinematic` / `fixed`) and a `Collider` to an entity. A dynamic body
with no collider is an error — it would fall through everything.

`engine simulate --steps N` runs the world without drawing it and its report
carries **`entities`** — where each dynamic body ended up, name-sorted, with
`position`, `rotation`, and `linear_velocity`:

```bash
engine simulate scene.json --steps 120 | jq '.entities[] | select(.entity=="Ball").position'
engine simulate scene.json --steps 120 --entity Platform   # narrows; reaches
                                                           # non-dynamic entities too
```

Reach for that before `--trace t.jsonl` (every step, as JSONL — for *how* it got
there) or `--bake out.json` (the settled state as an ordinary scene file).
Simulation is deterministic: the same file and the same step count give
byte-identical results.

## Scripting

A `Script` component (`{ "type": "Script", "source": "scripts/spin.rhai" }`)
runs `fn step(world, step)` once per fixed step. The curated `world` API is the
entire universe available to a script — no clock, no file system, no randomness,
so runs stay reproducible.

```rhai
fn step(world, step) {
    let p = world.position("Ball");          // [x, y, z]
    world.set_position("Ball", p[0], p[1] + 0.01, p[2]);
    world.hud("y = " + p[1]);                // debug line, cleared each step
}
```

Reads and writes by entity name: `position` / `set_position`, `rotation` /
`set_rotation`, `scale` / `set_scale`, `forward`, `linear_velocity` /
`set_linear_velocity`, `angular_velocity` / `set_angular_velocity`,
`look_at`, `key` (keyboard, in the viewer or from a replayed timeline),
`touching` / `contacts_started`, `terrain_height`, `light_intensity` /
`set_light_intensity`, `particle_rate` / `set_particle_rate`, `hud`, and
`state` / `set_state` for numeric memory between steps.

A game also needs a shell, which is four more calls. `save(slot)` /
`load(slot)` / `has_save(slot)` write and read the whole `state` map as sorted
JSON in `saves/slot<N>.json` beside the scene — a save is that map and nothing
else, so score, level and every setting persist without being enumerated
anywhere; slots are `0..9`, and an empty one reads as `false` rather than
failing, because "is there a save?" is a menu's first question. `quit()` closes
the viewer's window, and headlessly stops the run and reports `quit_at_step`.
The `environment` block is writable through `shadows` / `set_shadows`,
`sky` / `set_sky`, `fog` / `set_fog`, `samples` / `set_samples` and
`shadow_cascades` / `set_shadow_cascades`, which is what a graphics-settings
screen drives; `set_samples` takes 1 or 4 and `set_shadow_cascades` takes 1 to
4, both erroring at the call on anything else. Those two rebuild every pipeline
when they change, so they are a settings screen's deliberate actions rather
than sliders. And `animation_clip` / `set_animation_clip`
switches a skinned character between clips as a **hard cut** — blending is a
standing non-goal, so a change of gait is a change of clip, and the cut resets
`phase` because two clips do not share a cycle.

The mouse is the same shape: `mouse("MouseLeft")` for the buttons,
`cursor_x()` / `cursor_y()` for where the pointer is as a fraction of the
frame, `viewport_width()` / `viewport_height()` to put that in HUD pixels, and
`cursor_ground(y)` for the world point under the cursor — the call a top-down
game aims with. `hud_offset` / `set_hud_offset` moves a `HudText` or `HudRect`,
which is how a crosshair follows the pointer and how a menu lays itself out.

```rhai
fn step(world, step) {
    let g = world.cursor_ground(0.0);        // where the pointer meets y = 0
    if world.mouse("MouseLeft") {
        world.set_position("Marker", g[0], 0.05, g[2]);
    }
}
```

Input lives in an `*.input.jsonl` timeline headlessly — keys and buttons in
one `held` array, plus an optional `"cursor": [x, y]` as a fraction of the
frame — so a mouse-driven game screenshots and diff-renders like anything
else. Note that the cursor's *ray* depends on the frame's aspect: render a
mouse-driven scene at the size its timeline was recorded at.

System order per step: animations → scripts → physics → particles → render.

## Rendering the same frame twice

Renders are reproducible on one machine and one GPU adapter, which is what makes
`engine diff-render` a regression test: render a scene, keep the PNG as a
baseline, and a later diff-render fails if a pixel moved.

Two limits worth knowing before you build a workflow on it:

- **A baseline is an artifact of the machine that blessed it.** Another GPU, and
  often another build profile, renders slightly different bytes. Bless your own
  baselines locally; do not expect one committed from elsewhere to match.
  Cross-machine comparisons start around `--threshold 3 --max-diff-percent 0.1`.
- Bless a baseline with `engine screenshot` — there is no separate bless flag.

## When something looks wrong

- **Nothing rendered.** Read the `digest` in the `screenshot` report before you
  read the image: `coverage: 0.0` means nothing but background reached the
  frame, and `mean_luminance` near 0 means it rendered dark. `entities_drawn`
  only tells you geometry was *submitted*. Then check the camera is
  `"active": true` and pointed at the scene (it looks down its own local −Z),
  and that lights exist.
- **Geometry is invisible from one side.** Backface culling is on and front
  faces are counter-clockwise. A wrongly wound triangle renders nothing.
- **A scene validates but looks off.** Read the warnings on stderr — `zero_scale`
  and `unused_material` exist precisely for scenes that are legal and wrong.
- **A field was ignored.** It was probably rejected: check the exit code, and
  read the schema with `engine list-components --component <Name>`.
- **An entity is not where you think.** `engine inspect scene.json --entity X`
  prints what the engine actually built, defaults included — the gap between
  that and what you meant is usually the bug.
