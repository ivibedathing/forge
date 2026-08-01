# Component reference

**Generated from the component schema — do not edit by hand.**
Regenerate with `engine list-components --markdown > docs/component-reference.md`;
`cargo test -p engine-core --test repo_contracts` fails when this file is stale.

Every component is a JSON object tagged with its `type`, and every field below
is optional — an absent field *is* its documented default. `engine list-components`
prints the same information as JSON Schema, and `engine inspect <scene>` prints a
scene's components with the defaults filled in.

| Component | Summary |
|---|---|
| [`AmbientLight`](#ambientlight) | A flat, non-directional fill: `albedo * color * intensity`, added to the |
| [`AnimationPlayer`](#animationplayer) | Plays an animation clip against scene time (M9), or against ground |
| [`Breakable`](#breakable) | Breaks into pre-authored fragments (M14) — on a hard enough collision, |
| [`Camera`](#camera) | A viewpoint. `engine screenshot --camera <name>` selects one by entity name. |
| [`Cloud`](#cloud) | A cloud: a cumulus, a raft of stratocumulus, a storm anvil, a torn wisp. |
| [`Collider`](#collider) | Collision geometry (M8). Requires a `Transform`. With no `RigidBody` on |
| [`DirectionalLight`](#directionallight) | A sun: parallel light with no falloff. |
| [`FootPlant`](#footplant) | Puts a skinned character's feet on the terrain under them (M32). |
| [`HudImage`](#hudimage) | A screen-space textured rectangle (M31): icons, logos, framed panels. |
| [`HudInteract`](#hudinteract) | Makes the HUD element on its own entity clickable (M31). |
| [`HudPanel`](#hudpanel) | A screen-space container that lays its children out (M31). |
| [`HudRect`](#hudrect) | A screen-space solid rectangle (M12): the primitive behind health bars, |
| [`HudText`](#hudtext) | A screen-space text label (M12): lines of the built-in 8×8 pixel font, |
| [`Material`](#material) | Surface appearance, in the metallic/roughness parameterization every |
| [`Meadow`](#meadow) | Ground cover that grows, seeds and dies on a loop: grass, weeds, wildflowers |
| [`Mesh`](#mesh) | Renderable geometry. |
| [`ParticleEmitter`](#particleemitter) | A deterministic particle emitter (M13): smoke, sparks, dust — and, with the |
| [`PointLight`](#pointlight) | A local light that shines in every direction from its entity's position, |
| [`RigidBody`](#rigidbody) | A simulated rigid body (M8). Requires a `Transform`; a **dynamic** body |
| [`Road`](#road) | A road: a circuit, a street, a mountain pass. |
| [`Script`](#script) | Gameplay logic as data (M10): a Rhai script run once per fixed step. |
| [`Terrain`](#terrain) | A patch of ground: displaced terrain with a procedurally shaded surface |
| [`Transform`](#transform) | Position, orientation, and scale. |
| [`Tree`](#tree) | A procedurally generated tree (M19): trunk, recursive branches, and leaves, |
| [`Water`](#water) | A body of water: an ocean, a lake, a pond, a canal. |
| [`Wheel`](#wheel) | One raycast-suspension wheel of a vehicle (M12). |

## AmbientLight

A flat, non-directional fill: `albedo * color * intensity`, added to the
lit result. Exists because a sun-only scene renders back faces pure black,
and a black region in a screenshot tells an agent nothing.

At most one per scene (`multiple_ambient_lights`).

| Field | Type | Default | Notes |
|---|---|---|---|
| `color` | `[number; 3]` | `[1, 1, 1]` | Linear RGB, each component in `[0, 1]`. |
| `intensity` | `number` | `0.05` | `>= 0`. (at least 0) |

## AnimationPlayer

Plays an animation clip against scene time (M9), or against ground
covered (M32).

`clip` is a relative path to a property clip (`*.anim.json`), or a
`path#ClipName` glTF fragment naming a skeletal clip in the entity's own
mesh file (M30). A player in the file is playing — there is no play/pause
runtime state.

The clock is scene time by default, which keeps the pose a pure function
of (files, time). Setting `stride` swaps that clock for **distance
travelled**, and `phase` is where the clip has got to: still a field in
the file, so nothing moves into hidden state.

| Field | Type | Default | Notes |
|---|---|---|---|
| `clip` | `string` | — |  |
| `looping` | `boolean` | `true` | Wrap by clip duration; when false, clamp to the final pose. |
| `phase` | `number` | `0` | How far this player has got, in **cycles** of its clip, when `stride` drives it (M32). Ignored otherwise.  Cycles rather than seconds so the locomotion system needs no clip duration to advance it: one step covering `d` metres adds `d / stride`, and nothing has to open the clip file to know that. The engine writes it back every fixed step and the change-based bake splices it, so where a character is in its stride survives a bake the same way where it is standing does. A looping player's phase is reduced into `[0, 1)` as it is stored. (at least 0) |
| `speed` | `number` | `1` | Time multiplier; local time = `t * speed + start_offset`, or `phase * speed + start_offset` when `stride` is set. |
| `start_offset` | `number` | `0` |  |
| `stride` | `number` | `0` | Metres of ground one **cycle** of this clip covers (M32).  `0` — the default — is the M9 behaviour: scene time drives the clip. Above zero the clip is driven by the entity's horizontal displacement instead, advancing `distance / stride` cycles per fixed step, which is what stops a walk cycle sliding when the character's speed changes. `engine list-joints` measures the right value off the clip itself. (at least 0) |

## Breakable

Breaks into pre-authored fragments (M14) — on a hard enough collision,
inside an explosion, or when a script calls `world.break_entity`.

On break the entity is replaced, after that step's physics, by one
dynamic-body entity per fragment (`Parent.frag0`, `Parent.frag1`, …),
each inheriting the parent's `Material` and motion. Fragments are
ordinary entities afterwards: they render, trace, and bake like anything
else.

| Field | Type | Default | Notes |
|---|---|---|---|
| `fragments` | `object[]` | — | What the entity becomes. At least one. |
| `impulse_threshold` | `number` | — | Contact impulse, in kg·m/s (≈ mass x closing speed), at or above which a collision breaks this entity. **Absent means collisions never break it** — only scripts and explosions do. Impulse rather than force so the number survives a `timestep_hz` change. `> 0`. (greater than 0) |

## Camera

A viewpoint. `engine screenshot --camera <name>` selects one by entity name.

| Field | Type | Default | Notes |
|---|---|---|---|
| `active` | `boolean` | `false` | Marks the camera used when none is named explicitly.  Exactly one camera in a scene may set this. Zero or several is a validation error rather than a warning-plus-guess: a deterministic failure is cheaper for an agent than a nondeterministic success. |
| `far` | `number` | `1000` | Far clip distance. Strictly positive, and must exceed `near` (checked cross-field by `engine validate`). (greater than 0) |
| `fov` | `number` | `60` | Vertical field of view, in degrees. Strictly between 0 and 180. (greater than 0, less than 180) |
| `near` | `number` | `0.1` | Near clip distance. Strictly positive. (greater than 0) |

## Cloud

A cloud: a cumulus, a raft of stratocumulus, a storm anvil, a torn wisp.

A recipe rather than a mesh reference, like [`Tree`] — the engine grows it
into a cluster of lobes, each of which grows smaller lobes on itself, seeded
so two clouds with the same parameters and different seeds are different
clouds. The entity owns that geometry, sized by `Transform.scale` like a
water surface, so a `Cloud` entity carries **no** `Mesh` and **no**
`Material` (`cloud_with_mesh`). A cloud is not a GGX surface: what a
`Material` describes, `color` / `shade_color` / `density` / `feather`
describe instead.

Non-uniform scale is the normal case, not an edge case — `scale: [24, 12,
24]` is what makes a cumulus wider than it is tall, and it oblates the lobes
with it.

Shading is three cheap stand-ins for multiple scattering, none of which is
volumetric: wrapped diffuse between `shade_color` and `color`, a forward-
scattering silver lining when the camera looks toward the sun, and an alpha
that fades toward each lobe's own silhouette. Clouds do not cast shadows
(the engine has one shadow cascade and it is fitted to the camera, not to a
cloud at altitude) and are not lit by `PointLight`s.

| Field | Type | Default | Notes |
|---|---|---|---|
| `children` | `integer` | `3` | Lobes grown on each lobe of the previous generation. `[0, 8]`. (at least 0, at most 8) |
| `color` | `[number; 3]` | `[1, 0.98, 0.95]` | Linear RGB of the sunlit side. Each component `[0, 1]`. |
| `density` | `number` | `0.9` | How opaque the cloud is where it is thickest, `[0, 1]`. Lobes do not write depth, so overlapping ones accumulate — which is a cheap stand-in for optical depth, and why a wisp wants a much lower value than a storm. (at least 0, at most 1) |
| `detail` | `integer` | `2` | Icosphere subdivisions per lobe: 12, 42, 162 or 642 vertices. `[0, 3]`. This is the quality dial, and `2` is plenty for anything at cloud distance. (at least 0, at most 3) |
| `drift` | `[number; 3]` | `[0, 0, 0]` | World-space metres per second the cloud travels, evaluated against the scene clock (`--time`, else `steps / timestep_hz`) — so a drifting sky is as reproducible as a wave, and a script is not needed to move it.  The cloud's *shape* never changes with time. Regenerating lobes per frame would mint a new mesh every frame and defeat the renderer's upload cache; in this engine, generated geometry is made once. |
| `drift_wrap` | `number` | `0` | Metres after which a drifting cloud recycles to where it started. `>= 0`; `0` (the default) lets it drift away for good.  Wrapping *teleports* the cloud, so it wants to be wider than the view or far enough out that fog has already eaten it before it jumps. (at least 0) |
| `feather` | `number` | `3` | How crisp the cloud's edges are, `[0, 8]`. Alpha follows `1 - (1 - facing)^feather` as the surface turns away from the camera, so **higher is crisper** and low values are wispy: 1 fades the whole surface proportionally, 3 keeps the body opaque and thins only the last few degrees before the silhouette.  It is doing two jobs. A real cloud's silhouette is where it thins out, not where its geometry stops — and the same fade is what hides the boundaries *between* two interpenetrating lobes, since each of them vanishes exactly where its surface turns away. (at least 0, at most 8) |
| `flatten` | `number` | `0` | How much the cloud sits on a flat base, `[0, 1]`. `0` is a puffball with lobes scattered through its whole box; `1` seats every lobe on the base plane and folds what hangs below onto it.  A cumulus has a flat bottom because condensation begins at one altitude, which is why every fair-weather cloud in a field shares a base — it is the cheapest of this component's cues and one of the most legible. (at least 0, at most 1) |
| `jitter` | `number` | `0.3` | How much every jittered quantity — lobe radius, placement, child size — varies, as a fraction. `[0, 1)`; `0` is a diagram. (at least 0, at most 0.99) |
| `levels` | `integer` | `2` | Generations of smaller lobes piled on the base ones. `[0, 3]`: `0` is a cluster of plain spheres, `2` reads as cauliflower, `3` is expensive. (at least 0, at most 3) |
| `lobe_ratio` | `number` | `0.55` | Child lobe radius as a fraction of its parent's, `(0, 1]`. This is the dial that makes the silhouette detailed at more than one scale — at 1 every lobe is the same size and the cloud reads as popcorn. (greater than 0, at most 1) |
| `lobe_size` | `number` | `0.42` | Diameter of a base lobe as a fraction of the cloud's own size, `(0, 1]`. Large values give a few fat billows, small ones a curdled texture. (greater than 0, at most 1) |
| `lobes` | `integer` | `6` | Lobes in the base cluster, spread over the footprint on a golden-angle spiral. `[1, 32]`; a handful reads as one cumulus, a dozen or more as a raft. (at least 1, at most 32) |
| `rise` | `number` | `0.35` | How strongly child lobes are biased toward the sky, `[0, 1]`. A cumulus is a convection cell: its detail is on top, where the air is still rising, and its underside is smooth. At `0` children scatter in every direction and the cloud reads as a sea urchin. (at least 0, at most 1) |
| `seed` | `integer` | `0` | Seeds every random draw. Two clouds with the same parameters and different seeds are different clouds; the same seed always regrows the same cloud. (at least 0) |
| `shade_color` | `[number; 3]` | `[0.42, 0.46, 0.58]` | Linear RGB of the self-shadowed side, each component `[0, 1]`.  Blue-grey rather than grey by default, and that is the point: the underside of a cloud is lit by the sky above it, not by the sun it is hiding from. Darkening this toward slate is most of what makes a storm. |
| `wobble` | `number` | `0.12` | Smooth radial distortion of each lobe, as a fraction of its radius. `[0, 1)`; a little is what stops a lobe from reading as a ball. (at least 0, at most 0.99) |

## Collider

Collision geometry (M8). Requires a `Transform`. With no `RigidBody` on
the entity, this is static collision geometry — the common case for
ground planes and walls.

One flat object discriminated by `shape` (the shape `jq` and an LLM
handle best): `cuboid` uses `half_extents`, `sphere` uses `radius`,
`capsule` (Y-axis) uses `half_height` + `radius`; `trimesh` and
`convex_hull` take their geometry from `asset`, or from the entity's own
`Mesh` when `asset` is absent — the collider matches what the screenshot
shows, by construction. Validation enforces which fields each shape
requires and forbids — the file format is the contract, the flat Rust
struct is how it stays walkable by the schema-driven validator and the
editor's generated inspector.

`Transform.scale` scales the shape when the physics world is built — a
cube scaled 2x collides 2x big, which is what the screenshot shows.
Nonuniform scale on a round shape has no physics representation and is a
validation error, never a silent approximation (mesh shapes scale
per-vertex, so nonuniform is fine there).

Layers: `layers` names the collision layers this collider belongs to,
`collides_with` restricts which layers it interacts with. Both absent
means "collide with everything" — exactly the pre-layer behavior. Two
colliders interact only if each one's `collides_with` (or absence)
admits a layer the other belongs to. Layer names are scene-local
strings; a scene may use at most 32 distinct names.

| Field | Type | Default | Notes |
|---|---|---|---|
| `asset` | `string` | — | `trimesh` and `convex_hull` only: the mesh whose geometry to collide as (`builtin:` or a `.gltf`/`.glb` path relative to the scene file). Absent, the entity's own `Mesh.asset` is used — the common case; a mesh shape with neither is `collider_missing_mesh`. |
| `collides_with` | `string[]` | — | Only interact with colliders belonging to these layers. Absent = interact with everything. Empty is an error — omit the field instead. |
| `density` | `number` | `1` | Mass comes from `density` x shape volume. `> 0`. (greater than 0) |
| `friction` | `number` | `0.5` | `>= 0`. (at least 0) |
| `half_extents` | `[number; 3]` | — | `cuboid` only. Each component `> 0`. |
| `half_height` | `number` | — | `capsule` only: half the cylindrical section's height. `> 0`. |
| `layers` | `string[]` | — | Collision layers this collider is a member of. Absent = member of every layer. Empty is an error — omit the field instead. |
| `offset` | `[number; 3]` | `[0, 0, 0]` | Local offset of the shape from the entity's transform origin. |
| `radius` | `number` | — | `sphere` and `capsule`. `> 0`. |
| `restitution` | `number` | `0` | Bounciness, `[0, 1]`. When two colliders touch, the **larger** of the two restitutions applies (max-combine), so a bouncy ball bounces on a plain floor exactly as its own value says. (at least 0, at most 1) |
| `sensor` | `boolean` | `false` | Sensors detect overlaps (trace events) but exert no forces. |
| `shape` | `"cuboid"` \| `"sphere"` \| `"capsule"` \| `"trimesh"` \| `"convex_hull"` | — | The collision shape kinds `Collider.shape` may name. |

## DirectionalLight

A sun: parallel light with no falloff.

The light shines down the entity's local **−Z**, taken from its
`Transform` — the same convention the camera uses, so aiming a light is
aiming a camera. With no `Transform` the light travels toward −Z
(horizontally); a noon sun is `"rotation": [-90, 0, 0]`.

At most one per scene (`multiple_directional_lights`). A scene with **no**
light components at all gets a documented fallback rig (sun + ambient); a
scene with any light component gets exactly what it wrote — absent means
off.

| Field | Type | Default | Notes |
|---|---|---|---|
| `color` | `[number; 3]` | `[1, 1, 1]` | Linear RGB chromaticity, each component in `[0, 1]`. Magnitude lives in `intensity`. |
| `intensity` | `number` | `1` | Unitless multiplier, `>= 0`, unbounded above: intensity 2 is twice as bright, and a white light at 1.0 on a white surface head-on reads white. (at least 0) |

## FootPlant

Puts a skinned character's feet on the terrain under them (M32).

A post-pass over the posed skeleton: each named ankle is moved to the
ground beneath where the clip put it, the joints above it rotate to follow
(two-bone IK), the hips drop when a leg cannot reach, and the sole tilts to
the slope. It runs wherever the pose does, so `engine list-joints --time`
reports the planted rig and the render draws it — one answer, not two.

The ground is a `Terrain` entity named in `ground`, and that is a purity
decision rather than a convenience: planting against the *physics* world
would make the pose a function of the simulation, and the pose being a pure
function of (files, time) is what lets `list-joints` answer at all. The cost
is that a character cannot stand on a crate — see
`designs/locomotion-design.md` §5.

| Field | Type | Default | Notes |
|---|---|---|---|
| `align` | `number` | `30` | Degrees the sole may tilt to meet the ground's normal. `0` leaves the foot's animated orientation alone, which is right for a character that only ever walks on the flat. (at least 0, at most 90) |
| `feet` | `object[]` | — | The feet to plant, at most [`MAX_PLANTED_FEET`]. |
| `ground` | `string` | — | The entity carrying the `Terrain` these feet stand on. |
| `hips` | `string` | — | The joint lowered when a leg cannot reach its target — the pelvis, in a humanoid. Absent, the deficit is simply clamped: one foot plants and the other stretches, which is bounded but reads as wrong. |
| `max_drop` | `number` | `0.5` | How far below the animated ankle a target may be, in metres. This is what keeps a foot in mid-swing from being dragged to the floor: planting is a *correction*, and a correction with no ceiling is a different animation. (at least 0) |
| `max_lift` | `number` | `0.5` | And how far above it. (at least 0) |

## HudImage

A screen-space textured rectangle (M31): icons, logos, framed panels.

`texture` is a PNG relative to the scene file, loaded through the same
`TextureSource` and `(asset, space)` cache the material system uses, in
sRGB — so `texture_too_large` fires from `validate`, before a device
exists. Only the base level is read: the overlay draws at most one
destination pixel per texel band and never minifies below it, so a mip
level selection would have no correct answer at this scale.

Sampling is **nearest-neighbour**, written out in `engine-core` like every
other generator here — a render sits under a baseline, so the filter is a
format contract, and nearest is exactly reproducible where a bilinear
filter is a float-rounding question.

| Field | Type | Default | Notes |
|---|---|---|---|
| `anchor` | `"top_left"` \| `"top_right"` \| `"bottom_left"` \| `"bottom_right"` \| `"center"` | — | Where a HUD element attaches on screen (M12).  `offset` is measured **inward** from the anchor: from a right anchor, `offset[0]` runs leftward; from a bottom anchor, `offset[1]` runs upward; from `center` it is the usual +x-right / +y-down applied to the element's center. The anchored point is the element's matching corner (its center for `center`), so `offset: [0, 0]` puts the element flush against its corner at any resolution. |
| `offset` | `[number; 2]` | `[0, 0]` | Pixels inward from `anchor` (see [`HudAnchor`]). |
| `opacity` | `number` | `1` | `[0, 1]`, multiplied onto the texture's own alpha. (at least 0, at most 1) |
| `parent` | `string` | — | The name of an entity carrying a [`HudPanel`] to place this inside. |
| `size` | `[number; 2]` | — | `[width, height]` in destination pixels. Ignored on an axis where `stretch` is true. |
| `slice` | `[number; 4]` | `[0, 0, 0, 0]` | Nine-slice insets in **source** pixels, `[left, top, right, bottom]`. The default `[0, 0, 0, 0]` is a plain stretch. Corners are copied 1:1, edges tile along their axis and the centre tiles both ways — tiling rather than stretching, because tiling at nearest is exact where stretching at nearest is a moiré pattern. |
| `stretch` | `[boolean; 2]` | `[false, false]` | Fill the parent's content box on `[x, y]`, ignoring `size`. |
| `texture` | `string` | — | A `.png` path relative to the scene file (invariant 3). |
| `tint` | `[number; 3]` | `[1, 1, 1]` | Multiplies the decoded texel in linear space, so one grey frame texture serves a red panel and a blue one — the material system's authoring rule for `albedo_map`, here for the same reason. |
| `visible` | `boolean` | `true` | Drawn and hit-testable when true (the default). |

## HudInteract

Makes the HUD element on its own entity clickable (M31).

Carries no geometry: the hit box is that element's laid-out rectangle. An
entity with a `HudInteract` and no `HudPanel`/`HudRect`/`HudImage`/
`HudText` is `hud_interact_without_element`.

A separate component rather than an `interactive: true` flag on each of
four components, because the flag would be four fields that must stay in
agreement, and because the tints belong next to it.

The tints multiply the element's own colour (clamped to `[0, 1]` after
multiplying) and default to `[1, 1, 1]` — no change — so adding a
`HudInteract` moves no pixel until a cursor arrives. They exist so the
ordinary case, a button that lights up under the pointer, needs no script
at all; anything richer is a script writing colours.

| Field | Type | Default | Notes |
|---|---|---|---|
| `disabled` | `boolean` | `false` | Excluded from hit-testing when true, so it never hovers, presses or clicks — and never blocks what is under it either. |
| `hover_tint` | `[number; 3]` | `[1, 1, 1]` | Colour multiplier while the cursor is over this element. Unbounded above — a hover tint brightens. |
| `press_tint` | `[number; 3]` | `[1, 1, 1]` | Colour multiplier while a button is held down on this element. |

## HudPanel

A screen-space container that lays its children out (M31).

This is the component that removes hand-computed pixel offsets. Children
name it in their `parent`; `layout` decides whether they are stacked in a
`row`, a `column`, or placed `free` by their own anchors relative to this
panel's content box.

**Absent `width`/`height` means hug contents** — the panel is exactly its
children's extent plus `padding`. That is the default because it is the
case that makes a dialog authorable: the box follows the text instead of
the text being fitted to a box someone solved by hand.

`opacity` defaults to **0**, so a bare `HudPanel` is an invisible layout
group; set it and the same component is the dialog's backdrop. One
component rather than a container plus a rect whose size would have to be
kept in agreement with it.

| Field | Type | Default | Notes |
|---|---|---|---|
| `align` | `"start"` \| `"center"` \| `"end"` | — | Cross-axis alignment of a [`HudPanel`]'s children (M31). See [`HudLayout`] for why the variants are undocumented. |
| `anchor` | `"top_left"` \| `"top_right"` \| `"bottom_left"` \| `"bottom_right"` \| `"center"` | — | Where a HUD element attaches on screen (M12).  `offset` is measured **inward** from the anchor: from a right anchor, `offset[0]` runs leftward; from a bottom anchor, `offset[1]` runs upward; from `center` it is the usual +x-right / +y-down applied to the element's center. The anchored point is the element's matching corner (its center for `center`), so `offset: [0, 0]` puts the element flush against its corner at any resolution. |
| `color` | `[number; 3]` | `[1, 1, 1]` | Background colour, linear RGB in `[0, 1]`. Only visible at `opacity > 0`. |
| `gap` | `number` | `0` | Pixels between children along the main axis of a `row`/`column`. (at least 0) |
| `height` | `number` | — | Fixed height in pixels. Absent hugs the children. (at least 0) |
| `layout` | `"free"` \| `"row"` \| `"column"` | — | How a [`HudPanel`] arranges its children (M31).  NOTE (schemars): variants carry no doc comments on purpose. A doc comment on a *variant* turns the generated schema from a flat `"enum": [...]` into oneOf/const, which blinds the validation walk's closed-vocabulary check — the same trap `ColliderShapeKind` documents. |
| `offset` | `[number; 2]` | `[0, 0]` | Pixels inward from `anchor` (see [`HudAnchor`]). |
| `opacity` | `number` | `0` | `[0, 1]`, defaulting to **0** — an invisible layout group. (at least 0, at most 1) |
| `padding` | `number` | `0` | Uniform inset in pixels between this panel's edge and its content box. Per-side padding is the obvious next field and costs nothing to add later; M12's "no z field until something needs it" applies. (at least 0) |
| `parent` | `string` | — | The name of another [`HudPanel`] entity to nest inside. |
| `stretch` | `[boolean; 2]` | `[false, false]` | Fill the parent's content box on `[x, y]`, ignoring `width`/`height` and hug sizing on that axis. |
| `visible` | `boolean` | `true` | Drawn and hit-testable when true (the default). A hidden panel hides its whole subtree. |
| `width` | `number` | — | Fixed width in pixels. Absent hugs the children. (at least 0) |

## HudRect

A screen-space solid rectangle (M12): the primitive behind health bars,
speed bars, and backdrops. Drawn with the panels and images, before all
`HudText`, file order within the class.

M31 adds only the shared `parent`/`visible`/`stretch`; a rect stays the
flat script-driven bar it has always been.

| Field | Type | Default | Notes |
|---|---|---|---|
| `anchor` | `"top_left"` \| `"top_right"` \| `"bottom_left"` \| `"bottom_right"` \| `"center"` | — | Where a HUD element attaches on screen (M12).  `offset` is measured **inward** from the anchor: from a right anchor, `offset[0]` runs leftward; from a bottom anchor, `offset[1]` runs upward; from `center` it is the usual +x-right / +y-down applied to the element's center. The anchored point is the element's matching corner (its center for `center`), so `offset: [0, 0]` puts the element flush against its corner at any resolution. |
| `color` | `[number; 3]` | `[1, 1, 1]` | Linear RGB in `[0, 1]`. |
| `offset` | `[number; 2]` | `[0, 0]` | Pixels inward from `anchor` (see [`HudAnchor`]). Inside a `row` or `column` parent this is a nudge on top of the computed position. |
| `opacity` | `number` | `1` | `[0, 1]`; `1` (the default) replaces the pixel exactly, fractions alpha-blend on the GPU (deterministic per adapter, like every baseline). (at least 0, at most 1) |
| `parent` | `string` | — | The name of an entity carrying a [`HudPanel`] to place this inside. Absent (the default) means a child of the viewport. |
| `size` | `[number; 2]` | — | `[width, height]` in pixels, each `>= 0` — zero is legal so a script-driven bar can be empty. Scripts resize via `world.set_hud_rect_size` or `world.set_hud_size`. Ignored on an axis where `stretch` is true. |
| `stretch` | `[boolean; 2]` | `[false, false]` | Fill the parent's content box on `[x, y]`, ignoring `size` on that axis — the full-screen dim backdrop, and the bar spanning a column. |
| `visible` | `boolean` | `true` | Drawn and hit-testable when true (the default). |

## HudText

A screen-space text label (M12): lines of the built-in 8×8 pixel font,
drawn over the 3D scene after lighting, independent of any camera.

Needs no `Transform` — placement is `anchor` + `offset` in framebuffer
pixels, which is what the agent sees in the PNG. Text is always opaque
and never anti-aliased, so a HUD glyph is bit-exact in baselines. Glyphs
outside the font's coverage render as a filled box: visibly wrong in the
screenshot, never a panic.

M31 adds `parent`, `visible` and `stretch` (shared by every element in the
family), plus `align`, `wrap` and `line_gap`. Every one defaults to the M12
behaviour: no parent means a child of the viewport placed by exactly the
M12 anchor arithmetic, and `wrap: 0` means the single unwrapped line it has
always been.

| Field | Type | Default | Notes |
|---|---|---|---|
| `align` | `"left"` \| `"center"` \| `"right"` | — | Horizontal alignment of text within its own box (M31). See [`HudLayout`] for why the variants are undocumented. |
| `anchor` | `"top_left"` \| `"top_right"` \| `"bottom_left"` \| `"bottom_right"` \| `"center"` | — | Where a HUD element attaches on screen (M12).  `offset` is measured **inward** from the anchor: from a right anchor, `offset[0]` runs leftward; from a bottom anchor, `offset[1]` runs upward; from `center` it is the usual +x-right / +y-down applied to the element's center. The anchored point is the element's matching corner (its center for `center`), so `offset: [0, 0]` puts the element flush against its corner at any resolution. |
| `color` | `[number; 3]` | `[1, 1, 1]` | Linear RGB in `[0, 1]`, like every color in the engine; encoded to sRGB when the overlay is rasterized. |
| `line_gap` | `number` | `0` | Extra pixels between lines, on top of the glyph cell. (at least 0) |
| `offset` | `[number; 2]` | `[0, 0]` | Pixels inward from `anchor` (see [`HudAnchor`]). Inside a `row` or `column` parent this is a nudge on top of the computed position rather than the position itself. |
| `parent` | `string` | — | The name of an entity carrying a [`HudPanel`] to place this inside. Absent (the default) means a child of the viewport. |
| `size` | `number` | `16` | Glyph height in pixels, `>= 4`. The 8×8 font renders at integer scale `max(1, round(size / 8))`, so `16` means exactly 2× glyphs. (at least 4) |
| `stretch` | `[boolean; 2]` | `[false, false]` | Fill the parent's content box on `[x, y]`, ignoring this element's own size on that axis. Two booleans rather than a `"fill"` string in a numeric field, which would break the schema-driven walk. |
| `text` | `string` | — | The label. `\n` breaks a line explicitly; `wrap` breaks it automatically. Scripts may rewrite it via `world.set_hud_text` — an empty string is a legal rest value for a script-driven readout. |
| `visible` | `boolean` | `true` | Drawn and hit-testable when true (the default). Hiding a panel hides its whole subtree — one boolean is how a menu opens and closes. |
| `wrap` | `number` | `0` | Wrap width in pixels; `0` (the default) is no wrapping. Breaks on spaces — a word longer than `wrap` overflows rather than splitting, since a mid-word break in a fixed-width font reads as corruption. (at least 0) |

## Material

Surface appearance, in the metallic/roughness parameterization every
mainstream engine and glTF file uses.

All color fields are **linear** RGB in `[0, 1]` — physical reflectance, not
sRGB-encoded screen values. The engine never silently decodes an authored
color; the PNG pixel is the lit, sRGB-encoded result, so `albedo: [0.5,
0.5, 0.5]` under full light reads back ≈188, not 128.

Since M26 a material may instead be a **file**: `{"type": "Material",
"asset": "materials/asphalt.json"}` names a JSON document holding these same
fields minus the `"type"`. `asset` is exclusive with every other field —
setting both is `material_asset_with_fields` — because serde cannot tell an
absent field from one written at its default, so a partial override would
resolve to something the file does not say. A variant is a second file.

| Field | Type | Default | Notes |
|---|---|---|---|
| `albedo` | `[number; 3]` | `[0.8, 0.8, 0.8]` |  |
| `albedo_map` | `string` | — | An sRGB colour texture, **multiplied** by `albedo` (M26).  A tint over the map, not a replacement — which means the default `[0.8, 0.8, 0.8]` darkens an imported texture by 20% unless the file says `"albedo": [1, 1, 1]` beside the map. `engine import` writes that explicitly; a hand-authored material has to. |
| `alpha` | `number` | `1` | Uniform opacity: `1` = opaque, `0` = invisible. Range `[0, 1]`.  A flat blend with no view dependence — the "ghost this object" knob. Anything below 1 moves the entity out of the opaque pass and into the sorted blended one, where it tests depth but does not write it. (at least 0, at most 1) |
| `alpha_cutoff` | `number` | `0` | Above 0, a pixel whose `albedo_map` alpha falls below this is discarded — and so is its shadow, through a second caster pipeline with a fragment stage. This is what an alpha-cut leaf needs. Range `[0, 1]`; `0` (the default) cuts nothing, so the depth-only caster pass every current scene uses is untouched. (at least 0, at most 1) |
| `asset` | `string` | — | A `materials/*.json` file holding this material, relative to the scene file (invariant 3). Exclusive with every other field. |
| `attenuation` | `[number; 3]` | `[1, 1, 1]` | What survives that path, per linear-RGB channel: transmitted colour is scaled by `exp(-(1 - attenuation) * thickness)`. `[1, 1, 1]` absorbs nothing. |
| `emissive` | `[number; 3]` | `[0, 0, 0]` | Added after lighting, unaffected by any light — "make this visible regardless of lighting" is a debugging move worth having. Range `[0, 1]` per component. |
| `emissive_map` | `string` | — | An sRGB colour texture multiplied by `emissive`. |
| `ior` | `number` | `1` | Index of refraction, `1.0` (the default) being no bending at all. Read only by a transmissive surface, where it refracts the view vector against the shading normal and offsets the scene-colour sample. Range `[1, 3]` — glass is 1.5, water 1.33, diamond 2.4. (at least 1, at most 3) |
| `metallic` | `number` | `0` | `0` = dielectric, `1` = metal. Metals have no diffuse; their specular is tinted by `albedo`. Range `[0, 1]`. (at least 0, at most 1) |
| `normal_map` | `string` | — | A tangent-space normal map, linear data. The tangent frame is derived per pixel from screen-space derivatives rather than stored per vertex, so this works unmodified on `Water`, `Terrain`, `Road`, `Tree` and `Cloud` geometry, none of which carries tangents. |
| `normal_strength` | `number` | `1` | Scales the normal map's tangent-space XY. The first thing anyone does with a normal map is discover it is too strong, and the alternative is re-authoring the texture. (at least 0, at most 8) |
| `orm_map` | `string` | — | Occlusion in R, roughness in G, metallic in B — glTF's packing, so an import is a file copy rather than a channel re-pack. Linear data, never colour. R multiplies the ambient and sky terms only, never the direct sun: that is what makes it *ambient* occlusion rather than a second shadow. G and B multiply `roughness` and `metallic`. |
| `roughness` | `number` | `0.9` | Perceptual roughness: `0` = mirror-tight highlight, `1` = matte. Range `[0, 1]`. (at least 0, at most 1) |
| `thickness` | `number` | `0` | How far light travels inside the surface, in metres. Both the scale of the refraction offset and the Beer–Lambert path length, so a thick block of ice is finally greener than a thin one. `0` is the pre-M26 behaviour. (at least 0) |
| `transmission` | `number` | `0` | How much light passes *through* the surface instead of scattering off it: `0` = opaque, `1` = clear glass. Range `[0, 1]`.  Unlike [`Material::alpha`] this is view-dependent and keeps the specular lobe, which is the whole difference between a transparent object and a faded one: a water surface seen edge-on reflects the sky and hides its bottom, and seen from overhead it does neither. The approximation is a Fresnel lerp back toward opaque at grazing angles, with the diffuse term scaled by `1 - transmission` (light that went through did not come back). There is no refraction and no scene-color sampling, so what is behind the surface is not bent or tinted by its thickness. (at least 0, at most 1) |
| `uv_offset` | `[number; 2]` | `[0, 0]` |  |
| `uv_scale` | `[number; 2]` | `[1, 1]` | Tiling for every map on this material: UV × `uv_scale` + `uv_offset`. Sampling repeats on both axes, so `[20, 20]` is twenty tiles. |

## Meadow

Ground cover that grows, seeds and dies on a loop: grass, weeds, wildflowers
and the dry stand they turn into.

A recipe rather than a mesh reference, like [`Tree`] and [`Cloud`] — the
engine grows one plant and scatters copies of it over the footprint
`Transform.scale` gives, so a `Meadow` entity carries **no** `Mesh` and
**no** `Material` (`meadow_with_mesh`).

**It is the first recipe in this engine whose subject changes shape over
time.** A sprout is not a small blade of grass. The resolution is that the
geometry is static and the *life cycle lives in the vertex stage*: the plant
is built once carrying every organ any stage will need, and each organ scales
to nothing outside the phase window it belongs to. See
`designs/meadow-design.md`.

The cycle runs on the scene clock, so it can be sped up:
[`cycle_length`](Meadow::cycle_length) `: 3.0` runs a whole generation in
three seconds. **`0` — the default — freezes the field** at
[`phase`](Meadow::phase), the way `daylight.day_length: 0` freezes the day:
most scenes want a dial, not motion, and a frozen field is reproducible with
no `--time` at all.

Every generation **reseeds** rather than regrowing: a plant's position within
its own cell, its height and its lean all shift a little each time round, so
the dead stalk and the sprout that replaces it are not collinear. That costs
one integer hash in the shader and no state anywhere.

A meadow is scenery: no `Collider`, no shadow cast (a 2048² map cannot
resolve a blade of grass, and what it would record is noise that crawls),
and no `PointLight` contribution.

| Field | Type | Default | Notes |
|---|---|---|---|
| `blade_width` | `number` | `0.007` | Width of a blade at its base, in metres, `> 0`. (greater than 0) |
| `blades` | `integer` | `5` | Blades in one plant. `[1, 12]`; `3`–`5` reads as a tuft. (at least 1, at most 12) |
| `cycle_length` | `number` | `0` | Seconds one full life cycle takes, `>= 0`.  **`0` freezes the field** at [`phase`](Meadow::phase) — the default, and `daylight.day_length: 0`'s reasoning exactly. (at least 0) |
| `density` | `number` | `24` | Plants per square metre of footprint, `>= 0`.  The footprint is `Transform.scale` in XZ, so the plant count is `density × area`, rounded up to a square grid. `0` is an empty field, which is a legitimate thing to animate toward. (at least 0) |
| `flower_color` | `[number; 3]` | `[0.4, 0.37, 0.17]` | Linear RGB of the flower head, each component `[0, 1]`. What stops the weed stage from being nothing but taller grass. |
| `head_size` | `number` | `0.018` | Size of the flower and seed heads, in metres, `>= 0`. `0` grows a plant that never flowers. (at least 0) |
| `height` | `number` | `0.45` | Height of a fully grown plant in metres, `> 0`. `Transform.scale.y` multiplies it, the way it multiplies a [`Terrain`]'s relief. (greater than 0) |
| `max_slope` | `number` | `38` | Steepest ground grass will grow on, in degrees, `[0, 90]`. Plants on steeper ground are dropped; `90` keeps everything. (at least 0, at most 90) |
| `phase` | `number` | `0.42` | Where the cycle starts, `[0, 1)` — and where a frozen field sits. The default is mature green, so `{"type": "Meadow"}` alone puts a working field of grass in a scene. (at least 0, at most 0.999) |
| `seed` | `integer` | `0` | Seeds placement and the template's blades. Two meadows with the same parameters and different seeds are different fields.  The generator's xorshift and the shader's reseed hash are both written out in this repo, so a given seed means the same field across dependency upgrades — a meadow render sits under a `diff-render` baseline, which makes both a format contract. (at least 0) |
| `segments` | `integer` | `4` | Lengthwise segments per blade — how finely a blade can curve as it leans and bends in the wind. `[1, 8]`.  Together with [`blades`](Meadow::blades) this sets the per-plant triangle count, and the product with the plant count is what `meadow_too_complex` bounds. (at least 1, at most 8) |
| `size_jitter` | `number` | `0.35` | How much plant height varies between plants, as a fraction, `[0, 1)`. `0` is a lawn. (at least 0, at most 0.99) |
| `splay` | `number` | `54` | Degrees the outermost blades splay from vertical, `[0, 90]`. Blade 0 stays near upright, which is what gives a tuft a centre rather than a hole. (at least 0, at most 90) |
| `stages` | `object[]` | `6 entries` | The life cycle, as keyframes over `phase`. At least two, at most [`MAX_GROWTH_STAGES`](crate::meadow::MAX_GROWTH_STAGES), strictly increasing in `at`.  The default is the full seed → sprout → grass → weeds → dry → collapse cycle, so a meadow is worth looking at before anything is authored. |
| `stagger` | `number` | `0.25` | How far plants desync from each other, `[0, 1]`.  `0` marches the whole field in lockstep; `1` spreads offsets across the whole cycle, so every stage is present at every moment and the field never appears to change. A real meadow browns together with variation, which is why the default is near the low end. (at least 0, at most 1) |
| `terrain` | `string` | — | The [`Terrain`] entity this meadow stands on, by name.  Each plant's altitude is sampled from that patch through the same function `world.terrain_height` and `engine terrain-height` call, so there is one implementation of "where is the ground" and nothing to keep in agreement. Absent, the field is flat at the entity's own Y. |
| `wind` | `number` | `9` | How far the wind bends a plant at full sway, in degrees, `>= 0`. `0` is still air. (at least 0, at most 90) |
| `wind_direction` | `number` | `0` | Which way the wind blows, in degrees — `0` toward −Z, the engine's forward convention, shared with `Water`'s wave directions. |
| `wind_speed` | `number` | `3.5` | How fast gusts travel across the field, in metres per second, `>= 0`.  Gusts are a travelling wave, not a per-plant shimmer: sampling the noise against a moving coordinate is what makes wind cross a meadow visibly. (at least 0) |

## Mesh

Renderable geometry.

`asset` is either a `builtin:` primitive (`builtin:cube`, `builtin:cylinder`,
`builtin:plane`, `builtin:sphere`, `builtin:triangle`) or a `.gltf`/`.glb`
file's relative path, resolved
against the directory of the scene file that references it (invariant 3).
`engine validate` checks the reference resolves; a file that exists but
fails to parse is reported by validation's asset pass and again at render
time.

| Field | Type | Default | Notes |
|---|---|---|---|
| `asset` | `string` | — |  |

## ParticleEmitter

A deterministic particle emitter (M13): smoke, sparks, dust — and, with the
M17 fields, fire.

Particles spray from the entity's position into a cone around its local
**−Z** — the same aiming convention the camera and lights use, so a rising
smoke plume is `"rotation": [90, 0, 0]`. Each particle is born with
`start_*` values and reaches its `end_*` values as it dies; the billboard
renders as a soft unlit disc, alpha-blended over the scene.

Particles are simulation state on the fixed step clock, advanced by
`--steps` exactly like physics (`--time` poses animations, it does not
advance particles). The state is derived and disposable — never baked,
never traced — and the `seed` makes it reproducible: same file, same
steps, same particles, so screenshots of smoke diff-render bit-exactly.

# The fire fields (M17)

A cone of identical sprites reads as a sparkler, not a flame. Five fields
fix that, and **every one of them defaults to the M13 behaviour** — a
pre-M17 emitter is byte-identical, down to which random numbers it draws:
`radius` spreads emission over a fire bed, the three `*_jitter` fields
break the lockstep of a population born identical, `turbulence` makes
flames lick, `stretch` turns round puffs into motion-aligned tongues and
ember streaks, and `blend: "additive"` makes overlapping flame get
*brighter* instead of merely more opaque, which is the single biggest
difference between fire and orange smoke.

**Random draws are skipped, not defaulted, when a field is off.** Every
jitter field consumes the emitter's RNG only when it is non-zero, in the
documented order (direction → disc → speed → size → lifetime →
turbulence). That is what keeps every baseline blessed before M17 exact,
and it is the same discipline as not consuming the RNG on a capped spawn.

| Field | Type | Default | Notes |
|---|---|---|---|
| `acceleration` | `[number; 3]` | `[0, 0, 0]` | World-space acceleration applied to every live particle, units/s². Particles ignore physics; this is where buoyancy (`[0, 1, 0]` for smoke) or gravity (`[0, -9.81, 0]` for sparks) comes from. |
| `blend` | `"alpha"` \| `"additive"` | — | How a particle's color combines with what is already on screen.  NOTE: leave these variants undocumented. A doc comment on an enum *variant* makes schemars emit oneOf/const instead of a flat `"enum": [...]`, which blinds the validation walk's closed-vocabulary check — the same trap [`ColliderShapeKind`] carries a note about. |
| `drag` | `number` | `0` | Velocity damping per second. `>= 0`; 0 means none. (at least 0) |
| `end_alpha` | `number` | `0` | Opacity at death, `[0, 1]`. The default 0 fades particles out. (at least 0, at most 1) |
| `end_color` | `[number; 3]` | `[0.7, 0.7, 0.7]` | Linear RGB at death, each component in `[0, 1]`. |
| `end_size` | `number` | `0.2` | Billboard half-size at death. `> 0`; larger than `start_size` grows (smoke), smaller shrinks (sparks). (greater than 0) |
| `lifetime` | `number` | `2` | Seconds each particle lives. `> 0`. (greater than 0) |
| `lifetime_jitter` | `number` | `0` | Fraction by which each particle's lifespan varies from `lifetime`, `[0, 1)`. The most valuable of the three for fire: identical lifespans put the whole population's fade at one height, which draws a flat top on the flame, and varying them is what makes it ragged.  A particle's lifespan is fixed **at birth**, so animating `lifetime` mid-run retimes new particles and leaves live ones alone. (at least 0, at most 0.99) |
| `max_particles` | `integer` | `1024` | Cap on live particles; spawns beyond it are dropped (deterministically). `[1, 65536]`. (at least 1, at most 65536) |
| `radius` | `number` | `0` | Radius of the disc particles are born on, in world units, centred on the entity and lying in the plane **perpendicular to the aim** (local XY). `>= 0`; 0 (default) emits from the single point.  A campfire is a bed of coals, not a nozzle: emitting a flame from one point gives a cone with a visible apex, and no amount of `spread` hides it. Like `Wheel.offset`, this is rotated by the entity's rotation but **not** scaled by its `Transform.scale`. (at least 0) |
| `rate` | `number` | `10` | Particles spawned per second. `>= 0`; 0 stops emission (existing particles live out their lifetime). (at least 0) |
| `seed` | `integer` | `0` | Seed for the emitter's random spray directions. Same seed, same steps → identical particles; give two otherwise-identical emitters different seeds so they don't emit in lockstep. (at least 0) |
| `size_jitter` | `number` | `0` | Fraction by which each particle's size varies from `start_size` / `end_size`, `[0, 1]`. One multiplier scales both, so a particle keeps its authored growth curve and only its scale changes. (at least 0, at most 1) |
| `speed` | `number` | `1` | Initial speed in units/second along the sampled cone direction. `>= 0`. (at least 0) |
| `speed_jitter` | `number` | `0` | Fraction by which each particle's launch speed varies from `speed`, `[0, 1]`: 0.3 means every particle draws uniformly from ±30%. (at least 0, at most 1) |
| `spread` | `number` | `15` | Cone half-angle in degrees around local −Z. `[0, 180]`: 0 is a beam, 90 a hemisphere, 180 a full sphere. (at least 0, at most 180) |
| `start_alpha` | `number` | `1` | Opacity at birth, `[0, 1]`. (at least 0, at most 1) |
| `start_color` | `[number; 3]` | `[0.7, 0.7, 0.7]` | Linear RGB at birth, each component in `[0, 1]`. Unlit. |
| `start_size` | `number` | `0.2` | Billboard half-size in world units at birth. `> 0`. (greater than 0) |
| `stretch` | `number` | `0` | Seconds of travel to stretch the billboard along its own velocity. `>= 0`; 0 (default) keeps sprites round.  The sprite grows along its direction of motion by the distance the particle covers in this many seconds, so the same value stretches a fast ember into a streak and leaves slow smoke nearly circular — which is what a camera does. Flame tongues want a little (~0.05), sparks want a lot (~0.2). (at least 0) |
| `turbulence` | `number` | `0` | Strength of a swirling world-space acceleration field, units/s². `>= 0`; 0 (default) means none.  This is what separates a flame from a jet of orange dots: hot gas is unstable and curls. The field is smooth value noise sampled along each particle's own path (see `turbulence_scale`), so a particle wanders coherently rather than vibrating — and it is generated by an integer hash specified in this repo, so it is exactly as reproducible as the spray directions. (at least 0) |
| `turbulence_scale` | `number` | `1` | Size in world units of one cell of the turbulence field. `> 0`.  Small values curl the flame tightly (candle), large ones sway the whole plume (bonfire in wind). Roughly: a particle's direction changes over this distance travelled. (greater than 0) |

## PointLight

A local light that shines in every direction from its entity's position,
falling off with distance (M17).

Unlike the sun, there may be **many** per scene — up to
[`MAX_POINT_LIGHTS`], beyond which the scene is invalid
(`too_many_point_lights`) rather than silently missing the extras. It has no
orientation, so its `Transform.rotation` and `scale` are ignored; only
`position` is read. Point lights do not cast shadows (the engine has one
shadow map, and it belongs to the sun) and are not attenuated by fog.

This is the component that lets a fire light the ground around it: its
`intensity` and `color` are in the curated script API
(`world.light_intensity` / `set_light_intensity` / `light_color` /
`set_light_color`), so the same flicker signal that drives a flame's
emission rate can drive the light it casts.

Presence counts as "the scene lit itself": a scene whose only light is a
`PointLight` gets no fallback sun, same as with any other light component.

| Field | Type | Default | Notes |
|---|---|---|---|
| `color` | `[number; 3]` | `[1, 1, 1]` | Linear RGB, each component in `[0, 1]`. |
| `intensity` | `number` | `1` | Brightness at one unit of distance. `>= 0`.  Falloff is inverse-square, so this is the value the surface of a sphere one metre away receives — which makes `intensity` comparable to `DirectionalLight.intensity` at exactly that distance and four times dimmer at two metres. A campfire is a few units; a candle is a fraction. (at least 0) |
| `range` | `number` | `10` | Distance in world units at which the light reaches exactly zero. `> 0`.  Inverse-square falloff never truly reaches zero, so a range is what keeps a light local: the physical curve is multiplied by a window that smoothly closes at `range`. Without it, every light in a scene would contribute a little to every surface, and a lantern in one room would lift the black level of the next. (greater than 0) |

## RigidBody

A simulated rigid body (M8). Requires a `Transform`; a **dynamic** body
also requires a `Collider` (`missing_collider` — it would fall forever
through everything).

Simulation state is derived, never authoritative: this component is the
initial conditions, and `engine simulate --bake` writes the evolved values
back as ordinary scene text.

| Field | Type | Default | Notes |
|---|---|---|---|
| `angular_damping` | `number` | `0` | `>= 0`. (at least 0) |
| `angular_velocity` | `[number; 3]` | `[0, 0, 0]` | **Degrees per second**, `[x, y, z]` — the same units and axis order as `Transform.rotation`, for the same reason: the agent that writes `"rotation": [0, 45, 0]` writes `[0, 90, 0]` for a half-turn per second. Converted to rad/s only at the physics-backend boundary. |
| `body` | `"dynamic"` \| `"kinematic"` \| `"fixed"` | — | How a rigid body participates in simulation. |
| `can_sleep` | `boolean` | `true` | Allow the solver to put this body to sleep once it settles. |
| `ccd` | `boolean` | `false` | Continuous collision detection, for small fast bodies that would otherwise tunnel. |
| `gravity_scale` | `number` | `1` | Multiplier on scene gravity. `>= 0`. (at least 0) |
| `linear_damping` | `number` | `0` | `>= 0`. (at least 0) |
| `linear_velocity` | `[number; 3]` | `[0, 0, 0]` |  |
| `locked_rotations` | `[boolean; 3]` | `[false, false, false]` | Lock rotation around the `[x, y, z]` world axes. A vehicle locks `[true, false, true]`: yaw stays free for steering while contacts can no longer pitch or roll it over. |

## Road

A road: a circuit, a street, a mountain pass.

The entity owns its surface geometry — one continuous ribbon generated from
the centerline — so a `Road` entity carries **no** `Mesh` and no `Material`
(`road_with_mesh`), the same rule [`Water`] follows and for the same reason.

Asphalt, shoulders and the embankment skirt are all the **same** triangles.
That is not a saving, it is the point: road and shoulder as two surfaces at
slightly different heights build a ledge along the asphalt edge, and a wheel
that drops off it wedges against the step and stops the car dead. There is
no seam between segments either, because consecutive cross-sections share
their vertices.

Physics reads the same mesh: a `Collider` with `"shape": "trimesh"` on a
road entity needs no `asset` and no `Mesh`, because the road is the
geometry. Friction and collision layers stay on the `Collider`, where every
other surface in the engine keeps them.

| Field | Type | Default | Notes |
|---|---|---|---|
| `bank_color` | `[number; 3]` | `[0.2, 0.17, 0.13]` | Linear RGB of the embankment below the shoulder. Each component `[0, 1]`. |
| `closed` | `boolean` | `false` | Join the last point back to the first. A closed road is a circuit: the polygon's exterior angles sum to one turn, so it shuts without a solver. |
| `color` | `[number; 3]` | `[0.09, 0.09, 0.1]` | Linear RGB of the asphalt. Each component `[0, 1]`. |
| `markings` | `object` | — | What is painted on a road, and where.  Every marking is computed per pixel from the road's surface coordinates — `u`, metres from the centerline across the road, and `v`, metres along it — rather than built as geometry laid on the asphalt. That is what makes a line follow every curve and grade for free, keeps a dash the same length in metres through a hairpin as on a straight, and means paint can never z-fight: it is not a surface on a surface, it is the same pixel shaded differently. |
| `points` | `object[]` | `[{"position": [0, 0, 0], "radius": 0}, {"position": [0, 0, -20], "radius": 0}]` | The centerline, corner by corner, in the order they are driven. At least two points; a closed road needs at least three. |
| `roughness` | `number` | `0.92` | Surface roughness, `[0, 1]`, meaning what `Material.roughness` means. Asphalt is nearly matte; wet asphalt is not. (at least 0, at most 1) |
| `segment_angle` | `number` | `5` | Most degrees of arc one segment may cover through a corner. `>= 0.5`.  This is the resolution knob that matters: a corner cut every 5° is smooth to drive and to look at, and the cost is linear in the road's length rather than quadratic like a grid's. (at least 0.5) |
| `segment_length` | `number` | `2` | Longest a straight segment may be before the road is cut again, in metres. `>= 0.25`. (at least 0.25) |
| `shoulder` | `number` | `1.5` | Drivable shoulder each side of the asphalt, in metres. `>= 0`.  Part of the same surface, not a second one — see the note above about ledges. (at least 0) |
| `shoulder_color` | `[number; 3]` | `[0.17, 0.2, 0.14]` | Linear RGB of the shoulder each side of the asphalt. Each component `[0, 1]`. |
| `skirt` | `number` | `0.6` | How far the embankment drops below the road's outer edge, in metres. `>= 0`.  This is what stops an elevated road from floating. Set it deeper than the road ever climbs and it simply disappears under the ground plane wherever the road is low. (at least 0) |
| `width` | `number` | `7` | Width of the asphalt, edge to edge, in metres. `> 0`. (greater than 0) |

## Script

Gameplay logic as data (M10): a Rhai script run once per fixed step.

`source` is a relative `.rhai` path defining `fn step(world, step)`.
Scripts mutate the world through a small registered API and never invent
state of their own — baked output after a scripted run is an ordinary
scene file.

| Field | Type | Default | Notes |
|---|---|---|---|
| `source` | `string` | — |  |

## Terrain

A patch of ground: displaced terrain with a procedurally shaded surface
(M22).

The entity carries **no** [`Mesh`] and **no** [`Material`] — `Terrain` owns
both, like [`Water`] — and having either is `terrain_with_mesh`. Geometry is
a tessellated unit grid sized by `Transform.scale`, displaced by an fBm
height field; `Transform.scale.y` multiplies that displacement, so [`height`]
is what you get at scale 1.

Heights are sampled in **world** XZ, so two patches with the same fields meet
seamlessly and moving one moves it *through* the field rather than dragging
its hills along.

Unlike water's waves, the height field is evaluated on the **CPU**: terrain
does not animate, so the surface is generated once and cached, and there is
exactly one implementation for the renderer, the collider (a `trimesh`
`Collider` with no asset uses this surface) and
`world.terrain_height(name, x, z)` to share. Appearance is the opposite —
per-pixel, in the shader, mirrored by nothing, which is what licenses detail
far finer than the grid.

[`height`]: Terrain::height

| Field | Type | Default | Notes |
|---|---|---|---|
| `bump` | `number` | `0.3` | Per-pixel normal perturbation from the detail noise, `[0, 1]`.  Bumpiness with no displacement behind it — nothing physical may depend on it, which is what allows detail far finer than the grid or the collider. It fades with view distance, because sub-pixel normal variation aliases into sparkle that reads as broken rather than as low quality (the lesson water's detail ripples already paid for). (at least 0, at most 1) |
| `color_variation` | `number` | `0.25` | How strongly the detail noise modulates the blended albedo, `[0, 1]`.  The cure for the one-flat-colour look: even a single-layer terrain stops being a sheet of paint. Past ~0.5 the ground reads as camouflage. (at least 0, at most 1) |
| `feature_scale` | `number` | `40` | Metres across one cell of the largest noise octave. `> 0`.  The size of the big rolling features, and the field that decides whether a patch reads as dunes, as pasture or as foothills. Well under `segments`-worth of quads and the grid cannot resolve it; far larger than the patch and the ground reads as a tilted plane. (greater than 0) |
| `height` | `number` | `2` | Metres of displacement at full amplitude, `>= 0`.  The field is normalised to `[-1, 1]` before scaling, so this is the peak above (and below) the entity's own Y, and **adding octaves adds detail rather than altitude**. 0 is a flat patch, which is a legitimate thing to ask for and is still shaded by the layer system. (at least 0) |
| `layers` | `object[]` | `[]` | The materials the surface is painted with, at most [`MAX_TERRAIN_LAYERS`](crate::terrain::MAX_TERRAIN_LAYERS), blended by height and slope.  Empty (the default) paints the whole surface with [`TerrainLayer`]'s own defaults, so a bare `{"type": "Terrain"}` is a plausible grassy patch rather than an error or a blank. |
| `octaves` | `integer` | `4` | How many octaves of noise are summed. `[1, 8]`.  Each octave halves the feature size and scales its amplitude by [`persistence`](Terrain::persistence). Past about 5 the added detail is finer than the grid can carry and only costs generation time. (at least 1, at most 8) |
| `persistence` | `number` | `0.5` | Amplitude multiplier per octave, `[0, 1]`.  Low values give smooth swells; near 1 gives a rough, noisy surface with no clear large-scale shape. 0.5 is the usual landscape. (at least 0, at most 1) |
| `seed` | `integer` | `0` | Chooses the landscape. Any change reshapes every hill.  The noise hash is written out in this crate rather than pulled from a dependency, so a given seed means the same terrain across upgrades — a terrain render sits under a `diff-render` baseline, which makes this a format contract. (at least 0) |
| `segments` | `integer` | `128` | Quads per axis across the patch. `[1, 512]`.  The resolution the *relief* is drawn and collided at. What matters is metres per quad against [`feature_scale`](Terrain::feature_scale): a 200 m patch at 192 has one vertex per metre, which resolves a 40 m hill comfortably and a 3 m hummock not at all. Surface *detail* is per pixel and does not care. (at least 1, at most 512) |
| `texture_scale` | `number` | `4` | Metres across one cell of the surface-detail noise. `> 0`.  The scale of the mottling within a layer and of the fingers along the boundary between two — the *texture*, as opposed to the relief. Around a few metres reads as ground cover seen from standing height. (greater than 0) |
| `warp` | `number` | `0` | Domain warp: how far the field is dragged sideways before it is summed, as a fraction of [`feature_scale`](Terrain::feature_scale). `[0, 2]`; 0 (the default) is off.  Two lines of arithmetic, and the largest single difference between "fBm" and "landscape". Unwarped fBm is isotropic blobs; warping shears them into ridges and valleys that read as though water once ran over them. Past ~1 the surface starts to look smeared. (at least 0, at most 2) |

## Transform

Position, orientation, and scale.

All three fields are optional in JSON; omitting one gives the identity
value, so a scene can say `{"type": "Transform", "position": [0, 3, 0]}`
without restating a rotation and scale it does not care about.

**One world unit is one metre**, and that is the convention every other
number in the format is quoted against: gravity is `-9.81` because a body
falls 9.81 m/s², `Tree.height: 6.0` is a six-metre tree, and a `Wheel`
0.35 m in radius belongs under a car 1.7 m wide. Time is seconds and mass
is kilograms, so `Collider.density` is kg/m³ — note its default of `1.0`
is *not* a plausible material, and a body meant to be pushed by forces
wants a real one (the demo car's box chassis carries `350`, which is how
4.3 m³ becomes 1.5 t).

| Field | Type | Default | Notes |
|---|---|---|---|
| `position` | `[number; 3]` | `[0, 0, 0]` | World-space `[x, y, z]` **in metres**. +Y is up. |
| `rotation` | `[number; 3]` | `[0, 0, 0]` | Euler angles in **degrees**, `[x, y, z]`, applied in X→Y→Z intrinsic order (glam `EulerRot::XYZ`). Identity is `[0, 0, 0]`.  Degrees rather than a quaternion is a settled decision (design doc §9): an agent told to "rotate 45° about Y" writes `[0.0, 45.0, 0.0]` directly, whereas an unlabeled `[x, y, z, w]` array invites silent ordering bugs. |
| `scale` | `[number; 3]` | `[1, 1, 1]` | Multiplier on the entity's own geometry, per axis. Identity is `[1, 1, 1]`.  **Every `builtin:` primitive is one metre across at scale 1**, so on those this field reads directly as a size in metres: a `builtin:cube` at `[1.7, 0.7, 3.6]` is a car-sized box, and a `builtin:sphere` at `[0.9, 0.9, 0.9]` is 0.9 m across — *not* 1.8. The recipes size the same way (`Terrain`, `Water` and `Meadow` take their footprint from `scale` in XZ), and it also multiplies `Collider` dimensions, which is why a cuboid collider matching a builtin cube is authored as `half_extents: [0.5, 0.5, 0.5]` in *local* units rather than in metres. |

## Tree

A procedurally generated tree (M19): trunk, recursive branches, and leaves,
grown from a `seed` into geometry the renderer draws like any other mesh.

The tree is built in entity-local space with its **base at the origin,
growing along +Y**, so `"position": [x, 0, z]` plants it on flat ground and
`Transform.scale` sizes the whole thing. It replaces the entity's `Mesh`
rather than accompanying one (`tree_with_mesh`): the component *is* the
geometry.

# Two draws, two materials

A tree needs bark and foliage, and one `Material` cannot be both. The
entity's own `Material` is the **bark**; the leaves get [`Tree::leaf_color`]
and [`Tree::leaf_roughness`], and are always opaque and non-metallic. That
last part is a constraint, not an oversight: every leaf of one tree is a
single mesh, and the blended pass sorts whole draws, so translucent leaves
could not be sorted against each other and would visibly z-fight.

# Randomness

Everything jittered is drawn from a private xorshift seeded by `seed`,
consumed in a fixed order (a branch draws its own wander, then recurses into
its children in index order, then scatters its leaves), so two trees that
differ only in `seed` are different trees and the *same* tree is the same
mesh forever — which is what lets a forest sit under a pinned baseline.
`jitter` is the master dial for how much variation there is at all; `0`
grows a rigid diagram of a tree.

# Cost

Vertices scale as `(branches · whorl)^levels · segments · sides`, and a
tree that would blow past the budget is a validation error
(`tree_too_complex`) rather than a hang. `levels: 2` with the default
branching is ~1k vertices; `levels: 3` is ~6k. Generation happens once per
distinct parameter set and is cached, so a forest of nine trees is nine
meshes no matter how many frames render.

| Field | Type | Default | Notes |
|---|---|---|---|
| `branch_angle` | `number` | `48` | Angle in degrees between a child branch and its parent at the attachment. Small angles sweep up (poplar), large ones reach out (oak) or hang down (spruce, with negative `tropism`). `[0, 180]`. (at least 0, at most 180) |
| `branch_start` | `number` | `0.35` | Fraction of a branch's length that carries no children — the bare trunk under the crown. `[0, 1)`. (at least 0, at most 0.99) |
| `branch_twist` | `number` | `137.5` | Degrees of rotation around the parent between successive attachment points. The default is the golden angle, which is what real phyllotaxis converges on and what stops branches from stacking into visible rows. |
| `branches` | `integer` | `5` | Attachment points spaced along each parent branch. `[0, 16]`. (at least 0, at most 16) |
| `crook` | `number` | `8` | Degrees of random wander per meter of branch. This is the gnarl: `0` grows perfectly straight poles, `8` is a healthy tree, `25` is an old olive. `>= 0`. (at least 0) |
| `flare` | `number` | `0.4` | Extra radius at the very foot of the trunk, as a fraction: `0.4` makes the base 40% wider than the trunk above it. Root flare is most of what says "grown" rather than "placed", and it is at eye level. `>= 0`. (at least 0) |
| `height` | `number` | `6` | Trunk length in meters, before `Transform.scale`. `> 0`. (greater than 0) |
| `jitter` | `number` | `0.25` | How much every jittered quantity — branch length, radius, angle, leaf placement — varies, as a fraction. `[0, 1)`; `0` is a diagram. (at least 0, at most 0.99) |
| `leaf` | `"blade"` \| `"cluster"` \| `"none"` | — | What hangs on a tree's outermost branches.  NOTE: leave these variants undocumented, like [`ParticleBlend`]'s — a doc comment on a variant turns the schema into oneOf/const and blinds the validation walk's closed-vocabulary check. |
| `leaf_color` | `[number; 3]` | `[0.09, 0.26, 0.08]` | Foliage albedo, linear RGB in `[0, 1]` — the leaves' half of the tree's appearance, the entity's `Material` being the bark's. |
| `leaf_roughness` | `number` | `0.75` | Foliage roughness, `[0, 1]`. Leaves are waxy, not matte: a little specular is what separates them from felt. (at least 0, at most 1) |
| `leaf_size` | `number` | `0.3` | Length of one leaf, or diameter of one cluster, in meters. `> 0`. (greater than 0) |
| `leaves_per_branch` | `integer` | `6` | Leaves scattered along each outermost branch. `[0, 64]`; `0` is equivalent to `"leaf": "none"`. (at least 0, at most 64) |
| `length_falloff` | `number` | `0.35` | How much shorter children get toward the parent's tip, `[0, 1]`: `0` makes every child the same length (a round crown), `0.8` makes the top ones stubs (the cone of a conifer). (at least 0, at most 1) |
| `length_ratio` | `number` | `0.62` | Child length as a fraction of its parent's. `(0, 2]`. (greater than 0, at most 2) |
| `levels` | `integer` | `2` | How many generations of branches hang off the trunk. `0` is a bare pole; `2` reads as a tree; `3` is a good hero tree; `4` is expensive. `[0, 4]`. (at least 0, at most 4) |
| `radius_ratio` | `number` | `0.6` | Child radius as a fraction of the parent's radius where it attaches. `(0, 1]`. (greater than 0, at most 1) |
| `seed` | `integer` | `0` | Seeds every random draw. Two trees with the same parameters and different seeds are different trees; the same seed always regrows the same tree. (at least 0) |
| `segments` | `integer` | `5` | Lengthwise segments per branch — how finely a branch can curve. `[1, 16]`. (at least 1, at most 16) |
| `sides` | `integer` | `6` | Radial sides of a branch tube. `[3, 16]`; `5`–`6` is plenty, since bark silhouettes read from the branching, not the cross-section. (at least 3, at most 16) |
| `taper` | `number` | `0.12` | Radius at a branch's tip as a fraction of its base. `[0, 1]`; small values give the sharp taper of a young shoot. (at least 0, at most 1) |
| `tropism` | `number` | `8` | Degrees per meter that a branch curves toward the sky as it grows. Positive lifts tips toward the light (most trees), negative lets gravity droop them (spruce, willow). This is what makes branches curve rather than point.  It does not apply to the trunk, whose line is [`Tree::crook`] alone: a negative tropism on the trunk is unstable — a degree of crook tips it off vertical and the bend then compounds until the tree grows sideways. |
| `trunk_radius` | `number` | `0.16` | Trunk radius at the ground, in meters, before `Transform.scale`. `> 0`. (greater than 0) |
| `whorl` | `integer` | `1` | Branches emitted at each attachment point **on the trunk**, spread evenly around it. `1` (default) gives the alternate, spiralling arrangement of most broadleaf trees; `4`–`5` gives the whorls of a conifer, where a ring of limbs leaves the trunk at one height.  Deeper levels are always alternate, which is both what a real conifer's limbs carry and what keeps the geometry finite — a whorl compounding at every level multiplies the tree by itself. `[1, 8]`. (at least 1, at most 8) |

## Water

A body of water: an ocean, a lake, a pond, a canal.

The entity owns its own surface geometry — a tessellated unit grid sized by
`Transform.scale`, exactly like a scaled `builtin:plane` — so a `Water`
entity carries **no** `Mesh` and no `Material` (`water_with_mesh`). One
surface is one entity: sixteen tiles pretending to be a pond is what this
component exists to delete, and their seams are visible in any screenshot.

Waves are evaluated in **world space** in the vertex stage, which has two
consequences worth knowing: scaling a surface never stretches its waves, and
two adjacent water entities at the same height share one continuous surface
for free.

Shading is water-specific rather than a `Material`: sky reflection with a
Fresnel-weighted view term, absorption of what is behind the surface with
depth (`shallow_color` → `deep_color`), and foam where the water meets
geometry or folds at a crest. What it does *not* do is refract — the bed of
a pond is not displaced by the ripples above it.

| Field | Type | Default | Notes |
|---|---|---|---|
| `crest_foam` | `number` | `0` | Foam on the crests, `[0, 1]`; 0 (the default) is off.  Driven by the Gerstner Jacobian — where the surface pinches toward folding, which is exactly where a real wave breaks — so it appears only on steep waves and needs no second noise field to place it. (at least 0, at most 1) |
| `deep_color` | `[number; 3]` | `[0.01, 0.05, 0.08]` | Linear RGB of water far deeper than `depth_fade`. Each component `[0, 1]`. |
| `depth_fade` | `number` | `2.5` | Metres of water the view has to pass through to reach `deep_color` and full `opacity`. `> 0`.  Beer-Lambert absorption against the depth of whatever is behind the surface, so the same water is clear at the edge of a pond and opaque in the middle — which is most of how a surface reads as *deep* rather than as a coloured pane. A clear alpine lake is 6 or more; a silty pond is under 1. (greater than 0) |
| `detail` | `number` | `0.5` | Strength of the per-pixel ripple normals, `[0, 1]`.  Small-scale roughness the grid is far too coarse to carry as geometry, perturbing the normal and nothing else. Per line of code this is the biggest single difference between "blue glass" and "water", because it is what breaks the sun and the sky into glitter between the vertices. Nothing physical may depend on it — no buoyancy, no collision. (at least 0, at most 1) |
| `detail_scale` | `number` | `0.6` | Size of one ripple cell in metres. `> 0`. Around 0.5 reads as wind texture on a lake; 3 or more as a swell the grid did not resolve. (greater than 0) |
| `foam_color` | `[number; 3]` | `[0.86, 0.9, 0.92]` | Linear RGB of both foam kinds, each component `[0, 1]`. Foam is scattered light, so it is opaque where it appears. |
| `ior` | `number` | `1` | Index of refraction, `1.0` (the default) being no bending at all — the pre-M27 surface, which absorbs and tints what is behind it but never moves it. Range `[1, 3]`; water is 1.33.  Unlike [`Material::ior`] this needs no companion `thickness`: the shader already measures how far the view ray travels through the body to reach the bed, so the bend scales with the water's own depth and a pond bends its bed most where it is deepest. It also cannot change how *deep* the water looks — absorption stays [`Water::depth_fade`]'s job — so it can be turned on in a tuned scene without re-tuning it. (at least 1, at most 3) |
| `opacity` | `number` | `0.94` | How opaque deep water becomes, `[0, 1]`. 1 hides its bed completely however clear the shallows are. (at least 0, at most 1) |
| `roughness` | `number` | `0.06` | Surface roughness, `[0, 1]`, meaning what `Material.roughness` means.  Water is smooth, but not 0: a mirror-tight sun highlight is a single blown-out pixel that aliases as the camera moves, and the reflected sky carries most of the look anyway. (at least 0, at most 1) |
| `segments` | `integer` | `64` | Quads per axis across the surface. `[1, 512]`.  This is the resolution the *waves* are drawn at, and it is the one field worth thinking about: a wave needs roughly eight quads per wavelength to look like a wave rather than a fold, so a 14 m pond carrying 2 m chop wants ~64, and a 200 m ocean carrying 3 m chop cannot be drawn by any grid this component will generate. Detail *normals* are per pixel and cost nothing, which is why the glitter survives a coarse grid even though the silhouette does not. (at least 1, at most 512) |
| `shallow_color` | `[number; 3]` | `[0.09, 0.2, 0.21]` | Linear RGB of water one `depth_fade` deep or less — the colour at the shoreline. Each component `[0, 1]`. |
| `shore_foam` | `number` | `0` | Width in metres of the foam line where the surface meets geometry. `>= 0`; 0 (the default) is off.  This is the shoreline, and it is also the waterline on anything standing in the water: it comes from the depth behind the surface, so a boat, an ice block and the bank all get one without being marked up. (at least 0) |
| `waves` | `object[]` | `[]` | The waves summed to shape the surface, at most [`MAX_WAVES`](crate::water::MAX_WAVES) of them. Empty (the default) leaves the surface flat: a mirror, which is what a sheltered pond at dawn actually looks like. |

## Wheel

One raycast-suspension wheel of a vehicle (M12).

Sits on its own entity — the wheel's visual — and names its chassis in
`vehicle` (an entity with a **dynamic** `RigidBody` + `Collider`). The
engine groups every wheel naming the same chassis into one raycast
vehicle: each wheel casts a ray down its suspension, a spring/damper
pushes the chassis, and a tire friction model drives and grips at the
contact point. The wheel entity's own `Transform` is **written by
physics** every step — suspension compression, steering yaw, and axle
spin — so screenshots show wheels that steer, roll, and bounce. Author
its rest pose; simulation owns it afterwards. A wheel entity must not
carry its own `RigidBody` or `Collider` (`wheel_with_physics`) — the
chassis owns all collision.

Control fields (`engine_force`, `brake`, `steering`) are runtime inputs
the same way `RigidBody.linear_velocity` is: scripts write them via
`world.set_engine_force(...)` and friends, and physics reads them each
step.

| Field | Type | Default | Notes |
|---|---|---|---|
| `brake` | `number` | `0` | Braking strength, `>= 0`. A runtime input scripts write each step (`world.set_brake`). (at least 0) |
| `engine_force` | `number` | `0` | Drive force along the wheel's rolling direction, in newtons. Positive is the chassis's forward (−Z); negative reverses. A runtime input scripts write each step (`world.set_engine_force`). |
| `friction_slip` | `number` | `10.5` | Tire traction: how much forward/braking impulse the contact patch transmits before it slips, as a multiple of the suspension load. Larger = grippier; too large flips the vehicle under hard braking. `>= 0`. (at least 0) |
| `max_suspension_force` | `number` | `30000` | Hard cap on the suspension force, in newtons (not mass-scaled). `>= 0`. (at least 0) |
| `offset` | `[number; 3]` | `[0, 0, 0]` | Suspension attachment point in the chassis's local frame, in meters (rotated with the chassis, **not** multiplied by its `Transform.scale`). The suspension ray starts here and points down the chassis's local −Y. |
| `radius` | `number` | `0.3` | Wheel radius in meters. `> 0`. (greater than 0) |
| `side_friction_stiffness` | `number` | `1` | Multiplier on the tire's sideways grip. `1.0` = full lateral friction; lower values let the vehicle drift. `>= 0`. (at least 0) |
| `steering` | `number` | `0` | Steering angle in **degrees** about the chassis's up axis; positive steers left. A runtime input scripts write each step (`world.set_steering`). |
| `suspension_compression` | `number` | `2.5` | Damping while the spring compresses, per kilogram of chassis mass. `sqrt(stiffness)` is critical damping; ~0.5× that is a comfortable car. `>= 0`. (at least 0) |
| `suspension_damping` | `number` | `3.5` | Damping while the spring extends (rebound), per kilogram of chassis mass. Usually a little higher than `suspension_compression`. `>= 0`. (at least 0) |
| `suspension_rest_length` | `number` | `0.3` | Suspension spring rest length in meters: how far below `offset` the wheel center hangs when the spring is neither compressed nor stretched. `>= 0`. (at least 0) |
| `suspension_stiffness` | `number` | `24` | Spring stiffness **per kilogram of chassis mass** (the backend multiplies by mass, so the same value suspends a light or heavy chassis identically). Higher = stiffer. `> 0`. Static sag per wheel is roughly `9.81 / (4 * stiffness)` meters — keep `suspension_travel` above that. (greater than 0) |
| `suspension_travel` | `number` | `0.25` | Maximum travel from rest length, in meters, both directions. `>= 0`. (at least 0) |
| `vehicle` | `string` | — | Name of the chassis entity this wheel belongs to. Must be a *different* entity with a dynamic `RigidBody` and a `Collider`. |

