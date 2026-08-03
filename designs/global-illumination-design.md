# M35 — Global Illumination: Design

*Status: **built** (M35). §12 records what was settled with the user before the
build and what the build itself settled; §13 is the build order it followed.
Where the code and this document disagree, the code is right and §12 says why —
the three places that happened are the sky basis (§3, two bands not three),
multi-volume rendering (§12.6), and the fixture's ceiling (§12.7).
`designs/notes/m35-global-illumination.md` holds what building it taught. Drafted as M33 and renumbered to
M35 at merge, since skinned collider proxies took M33 and a units pass took M34
while this was open — numbers are claimed at merge time here, not at branch
time.*

`CLAUDE.md` lists "advanced rendering (GI, ray tracing)" as out of scope for v1.
**This reverses the GI half of that one item and nothing else** — ray tracing
stays out, and §2 rejects the ray-traced approach on its own merits. The
precedent for reading a scope line as reversible when the user asks for the thing
is M28, which reversed M11's input design §7, "no mouse".

---

## 0. The lie this removes

`sky_ambient` in `mesh.wgsl` gives every surface in the scene the whole sky
hemisphere. It is a good lie — it is most of what makes untextured geometry read
as lit rather than painted, and M16's comment says so. But it is the same lie
everywhere: the inside of the tour's forest, the underside of the truck, the
corner where the arena's walls meet, and a bare patch of open terrain all receive
identical fill light, because nothing in the engine knows that geometry stands
between a surface and the sky.

Two consequences are visible in every render this repo has committed. Contact is
missing — objects sit *on* the ground with no darkening where they meet it, which
is why a screenshot of the tour reads as a collection of models rather than a
place. And colour never travels: a red wall beside a white one leaves the white
one white, and the campfire lights the air but not the ash around it.

**GI here means one thing: the fill term stops being a constant and becomes a
function of where you are standing and which way you are facing.** Everything
else — mirror reflections, caustics, a second shadow map — is out of scope and
§14 says so.

## 1. Scope

In:

- An **irradiance probe volume**: a component, one entity, sized by its
  `Transform`, holding a grid of probes.
- A **CPU bake** producing a text file next to the scene, deterministic and
  byte-reproducible, verified against a hash of its inputs.
- **Sky occlusion and one bounce** of sky light, evaluated against the *live*
  sky — so `daylight` moves the fill light through the day for free.
- Receivers: the mesh family (including `Terrain`, textured and skinned
  variants), `Road`, and `Meadow`.
- A **volume in the showcase tour**, so the feature is visible in the demo and
  not only in its fixture.

Out, and each has a reason in §14: **bounced sunlight** (§5.3 — the decided
scope is sky only, and it is the largest deferral), specular GI, `Water` and
`Cloud` receivers, dynamic occluders, probe relighting from `PointLight`s, and
anything that runs per frame on the GPU.

**Everything defaults to off.** A scene with no volume component renders
byte-identically to M32, and the mechanism that guarantees that is the shader
seam (§7), not a runtime branch.

## 2. Why not the four obvious alternatives

**Screen-space GI/AO.** Cheap, no bake, no storage, no invariant trouble — and
wrong for this engine on its own terms. It is a function of the camera, so
`inspect`, `simulate` and every headless query can say nothing about it; it
misses every occluder off screen, so the tour's forest darkens as trees leave
frame; and it is the one lighting feature whose result changes when you move the
camera without moving anything. This repo's culture is that a render is a pure
function of files and a clock. Screen space breaks that in the one place it is
most visible.

**Lightmaps.** The standard answer, and it needs a UV unwrapper. Every recipe
component — `Terrain`, `Road`, `Tree`, `Cloud`, `Meadow`, `Water` — generates its
own geometry, so an unwrapper would have to serve six generators, and a meadow's
geometry changes shape every frame by design. A texel-space bake also fixes
resolution to surface area, which is exactly backwards for a 546 m circuit beside
a 2 m crate. Probes decouple GI resolution from geometry, which is what lets one
volume serve terrain, a truck and a character.

**Voxel cone tracing / SDF tracing.** Real-time, dynamic, and a whole renderer.
It wants compute passes, a mip pyramid rebuilt per frame, and a large 3D texture,
against `downlevel_defaults`' 4 bind groups and this repo's hard preference for
things that are testable without a GPU.

**Ray-traced / DDGI.** Needs hardware ray tracing, which wgpu 30 exposes only
behind experimental features and which not every target adapter has. The
baselines are per-adapter already; making the *lighting model* per-adapter is a
different thing entirely.

What is left is precomputation, and the whole design below is about **what**
to precompute so that the answer still moves when the sun does.

## 3. The spine: bake transfer, not radiance

The naive bake stores, per probe, the irradiance that arrived when the bake ran.
It is one number set, it is simple, and it is dead on arrival here: `daylight`
is a flagship system, and a scene baked at noon would carry noon's fill light
through midnight. A GI system whose output contradicts the sky the engine is
drawing is worse than no GI system.

So the bake stores **transfer**: how much light reaches this probe *per unit of
light emitted by each basis source*, which is a property of the scene's geometry
and albedo alone. Transfer is linear in source radiance, so evaluation is a
scaled sum:

```
irradiance(probe) = Σ_basis  transfer[probe][basis] · live_radiance[basis]
```

**The choice of basis is the whole design.** It has to be small enough to store
and to fold per frame, and it has to span everything the scene's lighting can
actually do. The engine hands us that set almost directly:

| Basis source | Count | Live radiance comes from | Moves with the clock? |
|---|---|---|---|
| Sky zenith band | 1 | `EnvironmentSettings.sky_zenith` | yes, via `daylight` |
| Sky ground band | 1 | `sky_ground` | yes |
| ~~Sky horizon band~~ | ~~1~~ | ~~`sky_horizon`~~ | **not a basis source — see below** |
| ~~Sun, direction *k*~~ | ~~N~~ | ~~`ResolvedLights.sun_color`~~ | deferred, §5.3 |

**Two basis sources, not the three this section was drafted with**, and the
correction matters enough to leave the third struck through rather than deleted.
The distinction the draft missed: `sky_gradient` draws the sky *dome* and does
interpolate three bands, but `sky_ambient` — **the fill term GI actually
replaces** — mixes only `sky_ground` and `sky_zenith`, linearly in
`n.y * 0.5 + 0.5`. `sky_horizon` never appears in it.

Baking a third band would let GI produce fill light the pre-M35 engine cannot,
so an open-sky probe would match `sky_ambient(n)` in total energy but not in
shape — and §3.1 pins exactly that equality. The stated cost: a sunset's horizon
colour does not tint GI. It does not tint the ambient fill today either, so
nothing regresses, and widening `sky_ambient` to three bands would edit one of
the four ULP-sensitive lighting lines. That is a different milestone.

The sun is the one source whose *direction* moves, which is what makes it cost N
vectors instead of one; §5.3 records the mechanism and §14 carries it forward.

The sky bands are not an approximation chosen for GI — they are M16's actual
fill model, the same colours `apply_daylight` rewrites every frame. **Baking
against the palette means GI tracks day and night exactly, with no extra
machinery**, in the same way M21 got fog recolouring for free because fog *is*
`sky_horizon`. That is the single most load-bearing sentence in this document.

### 3.1 The open-sky probe is exactly M16's hemispheric ambient

A probe with nothing above it must evaluate to `sky_ambient(n)` — byte-for-byte
the expression `mesh.wgsl` already computes — scaled by the authored
`AmbientLight` exactly as today. The bake normalizes to make that true, so:

- turning GI on **cannot** change the overall brightness of an open scene, only
  redistribute it;
- `AmbientLight.color` and `.intensity` keep predicting what they predict, which
  is the property M16 fought for with its per-channel normalization;
- every difference GI makes is attributable to geometry — occlusion darkens,
  bounce tints — which is the only way an agent looking at two renders can tell
  what the feature did.

This is M21's "the noon keyframe *is* the M16 clear-day defaults" applied to
space instead of time: pin the model to the hand-authored system at the one point
where both can be checked.

## 4. The volume is one entity with one component

Following `Water`, `Terrain` and `Meadow` exactly. The entity carries a
`Transform` and a `LightProbeVolume` and **no `Mesh` and no `Material`**
(`light_probe_volume_with_mesh`), and the `Transform` is the volume's bounds — a
unit box scaled and positioned, non-uniform scale being the normal case.

```json
{
  "name": "Lighting",
  "components": [
    { "type": "Transform", "position": [0, 8, 0], "scale": [120, 16, 120] },
    { "type": "LightProbeVolume",
      "spacing": 4.0,
      "bake": "gi/showcase.gi.json",
      "bounces": 1,
      "intensity": 1.0 }
  ]
}
```

- **`spacing`** is metres between probes, not a resolution — so a volume that is
  resized keeps its GI detail instead of stretching it, and two volumes at the
  same spacing agree where they meet. Grid counts are derived and reported.
- **`bake`** is a relative path (invariant 3), the file §6 describes.
- **`intensity`** scales the whole effect, which exists so an authoring pass can
  dial GI back without re-baking and so `0.0` is a one-field A/B against the
  pre-M35 look.
- **`bounces`** is 1 or 2 (§5.4).

**Amended after the build: a scene may hold at most one volume**, and a second is
`multiple_light_probe_volumes` — see §12 item 6. This paragraph originally
allowed several, resolving by smallest-spacing-wins, as the way to give an
interior finer spacing than the landscape around it; §7's binding arithmetic
already assumed one, so the rule was implemented everywhere except in the
shader, which is a component that validates and does nothing. The interior-at-
finer-spacing want survives as a deferral, and its shape is the multi-volume
binding question rather than a resolution rule.

A pixel outside the volume falls back to `sky_ambient` — which is exactly the
pre-M35 path, so the boundary of a volume is a fade, not a step (`blend`, in
metres, at the volume's edge).

Rejected: a scene-level `gi` block beside `physics`/`environment`/`daylight`.
Bounds are spatial and there can be several of them, which is what a `Transform`
is for; and the recipe-component idiom is the strongest pattern in this codebase.

## 5. The bake

`engine bake-gi <scene.json> [--entity Name] [--out path]` writes the file and
reports what it did. It is the only new command that *writes* into the project,
and like `import` it writes files rather than mutating the scene.

### 5.1 It rays against render geometry, not colliders

A tree has no `Collider` — that is a deliberate property of the tour's forest —
so a bake that asked the physics world what is in the way would find a landscape
with no trees on it. So the bake builds its own BVH over exactly what
`Scene::render_items` produces: builtin meshes, glTF meshes, and the generated
geometry of `Terrain`, `Road`, `Tree`, `Cloud` and `Meadow` (the last two are
questions in §12). Each triangle carries the albedo of the material it came from,
which is what makes bounce coloured.

### 5.2 The sampling is a format contract

A baked file sits under a render baseline, so the sequence of directions is a
format contract exactly as the particle xorshift, the terrain hash and the meadow
reseed hash are. Cosine-weighted hemisphere sampling from a **stratified,
in-repo, spelled-out** sequence — not `rand`, not a dependency, and not a
sequence that changes when the sample count changes in a way that reshuffles
existing probes. `samples` per probe is a `bake-gi` flag with a documented
default, and it is recorded in the file, because a file baked at 128 samples and
one baked at 512 are different artifacts and the render must be able to say
which it is looking at.

**The bake must be byte-reproducible across machines**, not merely
same-adapter: it is CPU-only, has no floating-point ordering ambiguity (rays are
accumulated in a fixed order), and a repo-contract test re-bakes the fixture and
compares. This is a stronger promise than any render in the repo makes, and it is
available only because the bake never touches a GPU — the `daylight.rs` dividend
again.

### 5.3 The sun is not a basis source in M35 — and how it will be

**Decided scope: sky only.** The sun stays a direct light with a shadow map and
contributes no bounce, which keeps the bake to three transfer vectors per probe,
keeps it independent of the `daylight` arc, and still delivers contact darkening
and sky-coloured fill — the majority of what §0 is about. What it does *not*
deliver is the postcard case: a red wall reddening a white one. That is M36, and
this section is its design so the decision does not have to be made twice.

The sun is the one basis source whose *direction* moves, and a transfer vector
per direction is what makes it expensive. Two candidates:

- **Six axis directions** (an ambient cube of sun positions), interpolated by the
  three nearest. Generic, scene-independent, and spends most of its budget on
  directions the sun in a given scene never occupies — including straight down.
- **N samples along the scene's own arc.** `daylight.rs` already maps time to a
  sun direction from `sun_elevation`/`sun_azimuth`, and that arc is a single great
  circle. Sampling it at N points (default 8 over the lit half) and interpolating
  by time-of-day covers exactly the directions the scene can produce. A scene with
  an authored `DirectionalLight` and no `daylight` block has **one** sun
  direction, so N = 1 and the sun basis costs one vector.

Take the second when it lands. It is more accurate per byte, it degenerates
correctly for the static case, and the coupling it introduces — the bake reads
the `daylight` block — is honest: the bake is a function of the scene file, and
the arc is in the scene file. The cost is written down: **changing
`sun_elevation` or `sun_azimuth` would invalidate the bake**, and §6's hash
catches that rather than letting it render wrong.

Nothing in M35's file format has to change to accept it: `basis` is already a
named map in the header, and a reader that finds a `sun` entry beside `sky` adds
terms to the same sum.

### 5.4 One bounce, optionally two

Bounce one is where nearly all of the visible difference is. Bounce two is a
second gather from the first bounce's result at each probe, costs another pass
over the volume, and mostly lifts the black in deep occlusion. `bounces` defaults
to 1; the fixture renders both and the design records the measured difference
rather than asserting one.

### 5.5 Probes inside geometry

A probe buried in the terrain or inside the truck gathers black and then leaks
that black into the surfaces it interpolates to. The classic fixes are a
per-probe validity flag with normal-weighted 8-tap interpolation in the shader,
or fixing it at bake time.

**Fix it at bake time.** A probe whose hemisphere is more than a threshold
occluded at close range is marked invalid, pushed toward open space along the
gradient of its own occlusion, and — if it still cannot see anything — filled
from the nearest valid neighbour by a flood fill. What reaches the shader is then
always a *plausible* field, which means the shader can use plain hardware
trilinear filtering: four texture samples, no per-tap weighting, no validity
branch, no divergence. The cost is that GI cannot express a genuinely dark
interior, and this engine has no interiors. If one ever arrives, the shader-side
fix is a known quantity and this paragraph is where to start.

## 6. The bake file, and why it is a file

**Why not compute it at load.** Because the loop is the product. The tour is
about a million triangles; a BVH build plus a few hundred thousand rays is
seconds in a debug build, and it would be paid by every `screenshot`, every
`diff-render`, every editor reload. The engine's whole premise is that the edit →
render → look loop stays fast enough to run constantly, and this is exactly the
kind of cost that quietly ends it.

**Why not a binary.** Invariant 1. It is JSON, and specifically it is one probe
per line so a diff shows which probes moved:

```json
{"format":"forge-gi/1","scene":"showcase_tour.json","entity":"Lighting",
 "inputs_hash":"…","grid":[30,4,30],"origin":[…],"spacing":4.0,
 "basis":{"sky":3},"samples":256,"bounces":1}
{"p":[0,0,0],"sky":[[…12 numbers…],[…],[…]]}
```

Each probe holds one **SH-L1 vector per basis source**: 4 coefficients × 3
channels = 12 numbers, quantized to four decimals. At the decided scope that is
36 numbers a probe, so a 30×4×30 volume is 3600 probes and roughly 600 KB of
text — large for a repo file and small beside the PNGs already committed, and it
is the size that made sky-only the cheap scope as well as the simple one. The
levers if it needs to shrink are `spacing` and quantization; a PNG probe atlas
would be several times smaller and is rejected on invariant 1.

**A stale bake is an error, never a wrong render.** `inputs_hash` covers every
input the bake read — the geometry-producing components of every entity, their
transforms and albedos, the volume's own fields, and the bake's own parameters.
Mismatch is `gi_bake_stale`, exit 1, naming the command that fixes it. Absent
file is `gi_bake_missing`. This is `scene_parse_desync`'s discipline: the failure
mode that must not exist is the one where everything runs and the picture is
quietly wrong.

**Amended after the build: `validate` does not recompute the hash — `engine
bake-gi --check` does.** Recomputing means collecting the scene's whole triangle
set, which is 0.86s on the showcase tour against `validate`'s 0.17s and grows
with the geometry while `validate` does not. So `validate` keeps the header
checks it can afford — grid, spacing, basis, probe count, and the file's
existence, which is what actually catches an edited *component* — and the
geometry-level check is an explicit command with a repo test standing over the
committed bakes (`every_committed_gi_bake_matches_its_scene`, no allowlist). The
gate that catches this in practice is CI, not the author's next `validate`; what
the decision gives up is the case where an author edits geometry, renders, and
never runs the tests.

## 7. The shader seam, and the first anchor with two claimants

Evaluation is `gi::evaluate(&Baked, &ResolvedLights, &EnvironmentSettings) ->
IrradianceField` — a CPU fold over probes × basis, run when the lights change,
producing exactly one thing the GPU sees: **an SH-L1 irradiance field in four
`Rgba16Float` 3D textures**, bound in group 2 beside the shadow map, the depth
copy and the colour copy, which is where frame-scoped textures live since M26.
Group 2 goes from five bindings to nine, well inside `downlevel_defaults`.

This is M21's architecture, not M16's: the *model* is a pure CPU function and the
GPU only ever reads its output. No new pass, no compute, and the per-frame cost
is a small upload (a few tens of KB) rather than a trace.

`Rgba16Float` because it is filterable in core WebGPU, and hardware trilinear
filtering is the entire reason to use a 3D texture rather than a storage buffer:
probe interpolation is free and continuous. Four textures because SH-L1 is four
coefficients; the fourth channel of the first carries the sun-visibility term
that `Meadow`'s root shading and the fade at a volume's edge both want.

**The producer.** GI is a `Producer` at M26's seam, claiming `anchor::AMBIENT`
and `anchor::FILL` — the two lines that decide what fill light a surface
receives, and the two the texture producer already claims for its occlusion map.

That collision is the structural finding of this milestone. `with_surface`
applies substitutions by sequential `str::replace`, so **two producers claiming
one anchor is not a merge, it is a silent no-op for whichever runs second** —
and a splice that silently did nothing renders the feature as if it were absent,
which `every_producer_actually_replaces_what_it_claims` exists to catch. This is
M30's lesson repeating on the fragment side, in the same words its comment on
`VERTEX_STAGE` uses: whole-line replacement worked while exactly one producer did
it, and does not survive two. A textured surface inside a GI volume is precisely
the case that has to compose, and the tour is full of them.

So `AMBIENT` and `FILL` stop being *replaced* and start being **reassembled from
contributions**, exactly as the vertex stage was: each producer contributes an
occlusion multiplier and/or an irradiance expression, and the seam builds the
line. The empty assembly must equal the anchor byte for byte, which is the same
assertion that keeps M16's four untouchable lines reachable.

`Road` and `Meadow` duplicate `mesh.wgsl`'s lighting — the `water`/`clouds`
precedent, for M16's reason — so each takes its own two-anchor splice against its
own file, following `with_water_refraction`'s shape. Three files gain anchors;
none of the three is edited.

## 8. What GI is added to, and what it is not

- It replaces the **hemispheric fill** and nothing else. Direct sun, its shadow,
  the specular lobe, the sky reflection and M17's point lights are all untouched.
  Occlusion multiplies ambient terms and never direct light — the texture
  producer's comment already states this rule and GI obeys it.
- **Dynamic entities receive GI and do not cast it.** The truck is lit by the
  bounce off the road it stands on; it does not darken the road under itself, and
  it does not bleed its own paint colour. Stated cost, and the reason a moving
  vehicle is the wrong thing to point at when checking whether GI is working.
- **The blast at station 04 still emits no light**, and neither does the
  campfire, into GI. Point-light bounce is §14.
- Transparency: a blended surface takes GI in its `fill` term exactly where it
  takes `sky_ambient` today, so the composite in `anchor::BLENDED` is unchanged.

## 9. Validation

| Code | When |
|---|---|
| `light_probe_volume_with_mesh` | the entity also carries a `Mesh` or a `Material` |
| `gi_bake_missing` | `bake` names a file that is not there |
| `gi_bake_stale` | `inputs_hash` disagrees with the scene |
| `gi_bake_malformed` | the file parses but its grid, basis or version disagree with the component |
| `too_many_gi_probes` | derived probe count over `MAX_GI_PROBES` |
| `gi_spacing_out_of_range` | schema range on `spacing` |
| `gi_volume_without_transform` | no `Transform`, so no bounds |

`too_many_gi_probes` is refused **before allocating**, `tree_too_complex`'s
precedent: a hung bake with no output is the worst failure an agent loop can hit.
`spacing`, `intensity`, `bounces` and `blend` carry `#[schemars(...)]` ranges so
the schema-driven walk checks them without a hand-written rule.

`gi_bake_stale` fires from `validate`, which means the standard gate catches a
scene edited after its bake — the cheapest possible place to catch it.

## 10. Querying it, because a picture is not a report

`engine gi-probe <scene.json> --at x,y,z [--normal x,y,z] [--time T]` reports the
irradiance the renderer would use at that point and normal, plus which volume
answered, the eight probes it interpolated, and how occluded they are. This is
`terrain-height`'s and `road-centerline`'s argument applied to light: anything
that asks "why is this dark" needs the number, not a PNG.

`bake-gi` reports probe counts, timings, the ray count, and how many probes were
invalid and relocated — the last being the number that says whether a volume is
badly placed.

Scripts get **`world.irradiance(x, y, z, nx, ny, nz)`**, read-only, returning
three numbers. Read-only for M21's reason: a script-settable light field is
hidden state and the render must stay a function of files and a clock.
`LightProbeVolume`'s geometry fields go in `NOT_ANIMATABLE` — a clip driving
`spacing` would invalidate the bake every frame — while `intensity` and `blend`
animate freely, which is how a scene fades GI in.

## 11. Verification

**Fixture** `verify/m35_gi.json`, aimed at its subject with no terrain in frame,
per M22's rule, so it can carry a hard bit-exact pin: a white floor and ceiling
between a strongly red wall and a strongly green one, lit by a sky, with **two
identical white spheres** — one in the open, one under an overhang. The two
spheres are the assertion, M30's and M32's fixture logic: they share a mesh, a
material and a light, so anything that made both wrong would leave them
identical. Only real GI reddens one side of each, greens the other, and darkens
the sheltered one.

Pinned without a pixel as well, which is the part every earlier fixture should
have had:

- a CLI test that `gi-probe` inside the red wall's bounce is measurably redder
  than the same probe with `intensity: 0.0`, and that the sheltered sphere's
  probe is measurably darker than the open one's;
- a repo-contract test that re-baking the fixture reproduces the committed file
  **byte for byte** (§5.2's promise);
- a diff-render assertion in the CLI suite, not just a manifest entry.

**A/B.** GI touches the shader assembly seam, which four sections of `CLAUDE.md`
flag as ULP-sensitive, so the no-pixel-moved claim is settled by the `ab-check`
skill — two binaries, `cmp` the PNGs — and not by a baseline sweep. The
prediction to test: 37 of 37 artifacts byte-identical, because every existing
scene compiles the same pipeline variants it compiled before. If the reassembly
of `AMBIENT`/`FILL` moves a pixel, that is M26's refraction lesson repeating and
the answer is the same one — a separate variant, not a shared branch.

**Reproducibility.** The bake is byte-reproducible everywhere. The render is
per-adapter as always, and the fixture avoids terrain and ground cover so it can
be pinned hard; whether it needs `samples: 1` is a measurement, not an
assumption — render it four times and count distinct images before blessing.

## 12. Decisions

Settled with the user before implementation began:

1. **The component is `LightProbeVolume`** — it says what the thing is made of,
   which is also what `bake-gi` and `gi-probe` talk about.
2. **Sky basis only in M35.** §5.3 carries the sun's design forward to M36 rather
   than deleting it, and the file format accepts it without a version bump.
3. **The tour gets a real volume**, not a token one, which re-blesses all six
   showcase baselines. They already carry M29's `--threshold 24
   --max-diff-percent 0.02` tolerance, so the re-bless is routine; what is *not*
   routine is that GI changes every frame substantially, so the six must be
   looked at rather than blessed blind.

Settled during the build, from measurements rather than from argument:

4. **Neither `Cloud` nor `Meadow` occludes; both receive.** The provisional
   answer held. `collect_occluders` walks meshes, trees, terrain and roads and
   stops there. A cloud casts no shadow today and making it darken GI would be
   the first thing it ever darkened; grass occluding grass is real but multiplies
   the ray count by the blade count, and the tour's meadow alone would have
   dominated the bake.
5. **The tour's volume is `spacing: 4.0`** over a 56 x 16 x 54 m box centred on
   the action — 1050 probes, a 208 KB file, and a 37-second bake against 504,366
   triangles. Picked from renders, as this section asked. What the renders said:
   station 01, in the forest, changes **10.9%** of its pixels; the other five
   stations change **under 1%** each. That is not a token volume failing to
   matter — it is §3.1 working. An unoccluded probe reconstructs `sky_ambient`
   exactly, so open ground *must* barely move, and the frame with a canopy over
   it is the one that does.

6. **A scene has at most one volume** — added during G2 as the one place this
   document was internally inconsistent, and settled after the M37–M43 merge.
   §4 allowed several and specified smallest-spacing-wins; §7's own arithmetic
   ("group 2 goes from five bindings to nine") assumes a single field, and a
   second volume costs either four more bindings per volume or the hardware
   trilinear filtering that is the whole reason the field is a texture rather
   than a buffer. G2 shipped the gap as a warning; **the decision made it an
   error — `multiple_light_probe_volumes` — which amends §4.** The precedent is
   `DirectionalLight` and `AmbientLight`: at-most-one, errors, and for the same
   reason. A warning was the wrong instrument because the failure it warns about
   has no symptom — the second volume validates, bakes, and lights nothing.

   What §4's rule was *for* is still a real want: an interior at finer spacing
   than the landscape it sits in. That is now a deferral with a name (§12's
   list), and the shape it would take is the multi-volume binding question, not
   a resolution rule in the CPU.

7. **The fixture has no ceiling**, against §11's wording. The light source here
   *is* the sky, and §3.1 pins GI to `sky_ambient`; a ceiling seals the sky out
   and the box renders black, which is a fixture that demonstrates nothing. The
   overhang over one sphere does the sheltering the ceiling was there for, and
   the two spheres still differ only in what is above them.

## 13. Build order

- **G0** — `LightProbeVolume` component, schema, validation, budget, and the
  bake-file format with its hash. No renderer change; `validate` and `inspect`
  work end to end and the errors are reachable before a pixel moves.
- **G1** — the bake: BVH, sampling, sky basis only, `bake-gi` and `gi-probe`. GPU
  untouched; everything in this stage is unit-testable without an adapter.
- **G2** — evaluation and upload: `gi::evaluate`, the four 3D textures, group 2,
  and the seam work — reassembling `AMBIENT`/`FILL` from contributions, which is
  the risky part and comes with its own A/B before anything else is added.
- **G3** — `Road` and `Meadow` receivers, the fixture and its baseline, the
  tour's volume and its six re-blessed frames, `CLAUDE.md`.

Each stage ends runnable, and G2 is the one to stop at if the A/B says the seam
change is not free.

## 14. Not in this milestone

- **Bounced sunlight**, §5.3 — the largest deferral and the one a viewer will
  notice, since it is what makes a coloured wall tint its neighbour under a sun
  rather than only under the sky. The design is written; M36 is the build.
- **Specular GI.** The sky reflection stays M16's roughness-lerped gradient. A
  probe volume can hold a prefiltered radiance cube, and that is what IBL means
  here; it is a milestone of its own and the deferred-work list already names it.
- **Point-light and emissive bounce.** Transfer is linear in intensity, so a
  per-light basis vector would be *exact* for a flickering campfire — the cost is
  storage, at 12 numbers per probe per light. Worth doing when a scene has one
  important local light rather than eight.
- **Dynamic occluders.** Requires either re-baking or a runtime structure, and
  both are a different design.
- **Sky visibility for `Water` and `Cloud`.** Water's fill is dominated by its
  sky reflection, which GI does not touch; a cloud is lit by the sky it is drawn
  against and has no inside.
- **Probe volumes that follow the camera**, cascaded volumes, and streaming.
  One volume per region, authored, is the agent-legible version.
- **Anything per-frame on the GPU.** The moment GI needs a compute pass it stops
  being a CPU function that `gi-probe` can answer for, and that property is worth
  more here than the fidelity it buys.
