# Clouds (M20)

The showcase tour's sky is three colors and a sun disc. It is named as faked in
`showcase-tour.md`, and it is the last large surface in the engine that nothing
is generated onto: the forest became `Tree` components in M19, the pond became a
`Water` component in M18, and the sky above both is still a gradient with
nothing in it.

This milestone adds a `Cloud` component, built on the same premise M19
established: **a recipe, not a mesh reference**. Seeded, so two clouds with the
same parameters and different seeds are different clouds; deterministic, so a
sky can sit under a `diff-render` baseline.

## 1. Why a sphere is not a cloud

The naive version of this feature is a white `builtin:sphere` at altitude, and
it is worth being precise about why that fails, because every decision below is
aimed at one of these:

1. **A cloud has no single surface.** Its silhouette is lobed at every scale —
   a cauliflower, not a ball. One convex hull reads as a boulder however it is
   shaded.
2. **A cloud is not a PBR surface.** Lambert plus GGX puts a hard terminator
   across it and turns the far half black. Light inside a cloud scatters many
   times before it leaves, so the lit side is nearly flat and the shadowed side
   is bright, blue, and lit *from the sky*, not from the sun.
3. **A cloud has no edge.** Its silhouette is where it thins out, not where the
   geometry stops. A hard rim against the sky is the single loudest tell.
4. **A cumulus has a flat bottom.** Condensation begins at one altitude, so the
   base of every cloud in a field is the same plane. It is the cheapest of the
   four cues and one of the most legible — the exact role root flare plays on a
   tree.

So the component is built out of exactly those four things, plus `jitter` to
make every instance an individual.

## 2. A cloud is one entity with one component

```json
{ "name": "Cumulus", "components": [
  { "type": "Transform", "position": [-34.0, 40.0, -26.0], "scale": [34.0, 19.0, 34.0] },
  { "type": "Cloud", "seed": 7, "lobes": 7, "levels": 2, "children": 3,
    "flatten": 0.85, "density": 0.9, "feather": 3.0 }
]}
```

`Transform.scale` is the box the cloud is grown inside, in metres — the `Water`
rule, for the `Water` reason: two ways to say how big something is means the
editor's scale gizmo drives the wrong one. The entity carries **no `Mesh`** (the
component *is* its geometry) and **no `Material`** (a cloud is not a GGX
surface, so a `Material` beside it would be a field set nothing reads). Both are
`cloud_with_mesh`.

Non-uniform scale is the normal case, not an edge case: clouds are wider than
they are tall, and `scale: [34, 19, 34]` is what makes the lobes oblate. The
cloud pass carries an inverse-transpose `normal_matrix` for exactly this.

Rejected alternatives:

- **A `ParticleEmitter` of soft billboards.** This is how most engines do it,
  and this engine already has the emitter. It fails on a rule M13 settled:
  particle state is *simulation* state — created only by `--steps`, never baked,
  never traced, and a `--steps 0` render draws none of it. A sky has to be there
  at step 0, in the editor viewport at rest, and under `--time`. Clouds are
  scene content, like trees; smoke is simulation.
- **A scene-level `environment.clouds` layer**, noise in the sky gradient. Cheap
  and it would ride into the water reflection for free through
  `sky_common.wgsl`. But nothing about it is seeded per instance, placeable,
  passable, or lit individually. It remains the right answer for *high* cloud —
  cirrus and overcast — and is named in §9 as the follow-up.
- **A `Cloud` material on any mesh.** Same objection water raised: `builtin:
  sphere` is one convex blob, so every author would have to supply pre-authored
  lobe geometry before seeing a single cloud. The component generating its own
  geometry means `{"type": "Cloud"}` works.
- **A raymarched density field.** Real volumetrics, and deterministic, so
  nothing about the baseline discipline forbids it. It is a different renderer:
  per-pixel marching, its own depth reconciliation, and no way to sort it into
  the back-to-front list water and transparent meshes already share.

## 3. The model

A cloud is a **cluster of lobes**, and each lobe grows smaller lobes on itself.

```
place `lobes` lobes on a golden-angle spiral over the footprint
    radius falls off toward the rim
    height follows a profile: middle lobes ride high, rim lobes rest low
for each of `levels` generations:
    for each lobe, attach `children` lobes on its surface
        radius ×= `lobe_ratio`
        direction is random, biased toward +Y by `rise`
        buried by 45% of the child's radius, so the two interpenetrate
displace every lobe's vertices radially by `wobble`
fold everything below the base plane onto it by `flatten`
```

Lobes are icospheres (`detail` subdivisions: 12, 42, 162 or 642 vertices), not
UV spheres. A UV sphere pinches at its poles, and a cloud lobe is seen from
every angle with nothing to hide the pinch — `builtin:sphere` is the lighting
probe, and its pole layout is right for that job and wrong for this one.

Three of the rules are the tree's, transposed:

- **Children are seated inside their parent**, exactly as a branch is seated
  inside the branch that carries it. There is no CSG union in this engine and
  interpenetration costs nothing.
- **Lobe size falls off per generation.** Uniform lobes read as popcorn; what
  separates cauliflower from a bag of golf balls is detail at more than one
  scale.
- **The whole thing is grown in a unit box** and sized by `Transform.scale`,
  like a water surface, so there is one way to say how big a cloud is.

And one is deliberately *not* transposed: the tree's uprighting term. A trunk
needs one because it is a random walk that compounds. A cloud's lobes are each
placed independently from the same origin, so nothing here drifts — which is
also why **the height of a base lobe is a profile rather than a draw**. A random
vertical scatter would make the difference between a cloud and a smear a lottery
the author cannot see they are playing, which is M19's lesson stated in the
positive.

## 4. Four things the renders changed

Each of these came out of a PNG that was wrong in a way the unit tests were
happy with. They are the reason this section exists, and three of them are now
pinned by tests.

### A pile of lobes shades as a pile of lobes

The first render's clusters read as bags of marbles: every lobe drew its own
terminator, because every lobe's normals were radial from its own centre. The
fix is in the geometry and it is a *shading* fix — each vertex normal is bent
55% of the way from its lobe's centre toward the **cloud's** centre
(`BODY_NORMAL`). Light entering a cloud scatters through the whole body before
it leaves, so the underside of a lobe on top of the cloud is not lit like the
underside of a lobe sitting on its own. Bending all the way is worse: the cloud
then shades as a smooth blob and the cauliflower silhouette stops being legible.

### A height profile that reaches the top of the box tears the cluster in half

With the middle lobe placed at the top of its box and the ring around it on the
floor, a seven-lobe cumulus rendered as a ball hovering over a wreath. The rise
is now capped at `DOME_STACK` (0.8) lobe *diameters*, so consecutive rings
always overlap. The consequence is worth stating plainly: **how far a cloud
fills a tall box is set by `lobe_size`, not by stretching a fixed number of
lobes to reach.** A six-lobe cloud in a very tall box is a squat cluster with
headroom, and that is the honest answer.

### A proportional edge fade makes a cloud transparent all over

Alpha was `density · facing^feather`, which fades *proportionally*: a surface
tilted 60° from the camera is already two thirds transparent. Seen from below —
where every underside is tilted — the cloud went translucent everywhere and its
own interior lobes showed through it as pale outlines. The curve is now
`density · (1 - (1 - facing)^feather)`, which keeps the body opaque and spends
its whole range in the last few degrees before the silhouette, where a real
cloud actually thins out. **`feather` inverted its sense with that change**:
higher is now crisper, not wispier.

### The sun cannot be applied in full, and cannot be withheld either

A white cloud under `intensity: 2.4` saturates everywhere and the shading
vanishes; that was the second render, and it is why the first fix attempted was
sharpening the wrapped-diffuse curve — which is the wrong knob, because the same
term also scales how much light reaches the shadowed side, and squaring it
turned the storm cloud into grey rock. What is there now is a
`THROUGH_SCATTER` fraction (0.3) of the sunlight reaching the shadowed side
having scattered through the body, with the diffuse curve left linear. Both
extremes were rendered; this is between them.

## 5. Shading: the part that is not geometry

`clouds.wgsl` is a new pipeline, not a `Material` variant. That follows the
`water.wgsl` precedent and its reasoning exactly: it duplicates `FrameUniform`
and the fog term rather than sharing them with `mesh.wgsl`, because M16 declared
four lines of `mesh.wgsl` untouchable and "equal on paper" is not enough when
FMA contraction depends on surrounding code. `sky_common.wgsl` is prepended, as
it is to `sky.wgsl` and `water.wgsl` — a cloud's underside is lit by the sky, and
that must be the same sky drawn behind it.

Three terms, each answering one item in §1:

- **Wrapped diffuse**, `dot(N, L) · 0.5 + 0.5`, mixing `shade_color` →
  `color`, and separately gating how much sun reaches the point (§4). Without
  the wrap, half of every cloud is black. `shade_color` defaults blue-grey, not
  grey: the shadowed side of a cloud is lit by the sky above it.
- **Forward scattering.** `pow(max(dot(-V, L), 0), 8)` brightens the cloud where
  it is thin and the camera is looking toward the sun — the silver lining. It is
  the reason a backlit cloud is the most legible cloud there is, and it costs one
  dot product.
- **Grazing feather**, the alpha curve of §4. It does two jobs: it gives the
  cloud the soft edge of §1.3, *and* it hides the boundaries between two
  interpenetrating lobes, since each of them vanishes exactly where its surface
  turns away from the camera.

Clouds join the existing back-to-front `Blended` list beside water and
transparent meshes, depth-tested, **not** depth-writing, so a lobe's alpha
accumulates through the lobes behind it — more overlap is more density, which is
a poor man's optical depth and roughly the right poor man's. The rejected
alternative was a depth-only prepass per cloud plus an `Equal`-tested shading
draw, which collapses each pixel to the nearest lobe and cannot double-blend; it
is a strictly larger change (a second pipeline, a second draw per cloud) and the
feather turned out to be enough.

Culling is **off** for this pipeline alone. A cloud has no inside: with backface
culling on it would vanish the instant the camera entered it (pinned by
`a_cloud_is_visible_from_inside_it`), and the far wall of each lobe would stop
contributing to the accumulation standing in for thickness.

Fog applies — a cloud at 400 m fading into `sky_horizon` is aerial perspective,
and it is free.

Clouds do **not** cast shadows, which is M16's rule for transparent geometry and
also just true of the current shadow map: one cascade fitted to
`shadow_distance` (60 m by default) does not reach a cloud at 200 m altitude.
Cloud shadows want a second, far cascade — named in §9, not smuggled in here.

## 6. Drift: clouds translate, they do not evolve

`drift` is metres per second in world space, evaluated against the same
reproducible clock water uses — `ScenePass.time`, which is `--time` when a
command was given one and `steps / timestep_hz` otherwise. The offset is applied
**in the vertex stage**, not to the model matrix, so `Scene::cloud_items` stays
a pure function of the file, and the generated geometry keeps its `Arc` identity
across frames.

The shape does not change with time, and that is deliberate. Regenerating lobes
per frame would mint a new `Arc<MeshData>` every frame, which defeats M15's
upload cache twice over — a re-upload per frame and one cache entry per frame
until eviction. It is the same reason `tree-design.md` gives for having no wind:
in this engine, generated geometry is a thing you make once.

`drift_wrap`, in metres, recycles a drifting cloud around a box of that size so
a sky does not empty out over a long take. `0` disables it. Wrapping *teleports*
a cloud, so the wrap wants to be larger than the view, or far enough out that
fog has already eaten the cloud before it jumps. That is an authoring caveat
rather than a mechanism, and saying so is cheaper than a fade nobody asked for.

## 7. Determinism, cost, caching

All three follow M19 exactly, which is most of the argument for doing it this
way:

- **One private xorshift32**, seeded from `Cloud::seed` through the same
  splitmix finaliser, written out in this repo rather than pulled from a
  dependency — the draw sequence is part of what a scene file *means*, so an
  upgrade may not be able to reshape a sky. Draw order is fixed: each base lobe
  draws its radius, offset and height jitters, then its wobble phases, then
  recurses into its children in index order. Jitter helpers always consume a
  draw, even at `jitter: 0`; no cloud baseline predates any cloud field, so the
  simpler contract is the one to hold.
- **`cloud::vertex_count` is exact**, not an estimate: `lobes ×
  Σ(children^i, i = 0..levels) × (10 · 4^detail + 2)`. Validation refuses
  anything over `MAX_CLOUD_VERTICES` (100,000) with `cloud_too_complex` before
  a single allocation, because a hung render with no output is the worst
  failure an agent loop can hit. The ceiling is reachable by accident:
  `lobes: 32, levels: 3, children: 8` is 18,720 lobes.
- **`cloud::mesh_for` caches on the component's exact field bits**, compared
  not hashed, and must return the same `Arc` — M15's geometry cache keys on
  `Arc` identity, so a fresh copy per frame re-uploads every cloud in the sky
  every frame. The key covers the **eleven geometry fields only**: `color`,
  `density`, `feather`, `drift` and the rest are uniforms and cannot move a
  vertex, so a white cloud and a storm-grey one of the same shape share one
  upload. (`Tree`'s key includes its foliage colours; it did not have to.)

And, inherited from M19 whether we like it or not: **cloud baselines are per
build profile as well as per adapter.** Every rotation and every icosphere
vertex goes through transcendentals, and release and debug builds reach
different libm routines. Bless from the debug binary, the profile `cargo test`
runs.

## 8. Species recipes

There is no species enum — a species is a set of parameters. These are the five
the fixture is built from; copy one and change the seed.

| | Cumulus | Stratocumulus raft | Storm anvil | Fractus wisp | Diagram |
|---|---|---|---|---|---|
| `scale` | `[34, 19, 34]` | `[120, 26, 60]` | `[52, 52, 52]` | `[18, 5, 11]` | `[15, 15, 15]` |
| `lobes` | 7 | 16 | 9 | 3 | 6 |
| `levels` | 2 | 1 | 2 | 1 | 0 |
| `children` | 3 | 3 | 4 | 2 | 0 |
| `lobe_size` | 0.46 | 0.26 | 0.38 | 0.55 | 0.5 |
| `lobe_ratio` | 0.56 | 0.6 | 0.5 | 0.7 | — |
| `flatten` | 0.85 | 0.95 | 0.9 | 0.0 | 0.0 |
| `rise` | 0.4 | 0.15 | 0.55 | 0.1 | — |
| `wobble` | 0.14 | 0.12 | 0.15 | 0.2 | 0.0 |
| `jitter` | 0.3 | 0.35 | 0.3 | 0.4 | 0.0 |
| `density` | 0.9 | 0.78 | 1.0 | 0.5 | 1.0 |
| `feather` | 3.0 | 2.6 | 3.5 | 1.3 | 3.0 |
| `shade_color` | default | `[0.46, 0.50, 0.60]` | `[0.30, 0.33, 0.42]` | `[0.55, 0.60, 0.70]` | default |

- **The anvil** is the cumulus with a darker `shade_color`, a taller box and
  enough `rise` that the detail piles on top. Nothing in the model knows what an
  anvil is; `rise` and vertical `scale` draw it, the way `length_falloff` draws
  a conifer.
- **The raft** is flat and wide with many small lobes and one generation: a
  stratocumulus deck is a texture, not a shape.
- **The wisp** turns `flatten` off, drops `density`, and is the one species that
  wants a *low* `feather` — torn cloud is thin everywhere, not just at its rim.
- **The diagram** is the authoring reference: `jitter` and `wobble` zeroed and no
  children, which is the configuration where the seed stops mattering entirely
  (`no_jitter_and_no_wobble_leaves_only_the_children_to_the_seed`). Where a child
  lobe attaches is a direction draw with no parameter to turn off, so a diagram
  cloud is a diagram of the *base cluster*.

A sky is more entities than a hand wants to author. That is a solved problem in
this repo and does not need a new mechanism: `examples/scenes/make_car_track.py`
generates a circuit, and a `make_sky.py` would scatter a cloud field the same
way — a generator emitting entities into a scene file that stays the source of
truth.

## 9. What this is not

- **No volumetrics.** No raymarching, no density field, no light marching. The
  shading in §5 is three cheap terms standing in for multiple scattering.
- **No high cloud.** Cirrus and overcast are a sky-dome property, not objects,
  and they belong in `environment` / `sky_common.wgsl` where the water
  reflection would pick them up for free. That is the natural M20.5, and it is
  what the showcase tour would benefit from most — see below.
- **No cloud shadows.** Wants a second far shadow cascade, which the engine does
  not have — the same "shadow cascades" entry the roadmap already carries.
- **No lighting *from* clouds**, and no effect on `AmbientLight`. An overcast
  sky in this engine is still authored by darkening `sky_zenith`.
- **No collision, no LOD, no growth.** A `Cloud` has no `Collider`, its geometry
  is not reachable as a `MeshSource` asset, and `detail` / `lobes` / `levels`
  are the manual quality dial.
- **No precipitation.** Rain under a storm cloud is a `ParticleEmitter` and
  always was.

## 10. Verification

- `verify/m20_clouds.json` + `verify/baselines/m20_clouds.png`, blessed from the
  debug binary at `--steps 120`, carrying all five species including two cumulus
  differing only in `seed` and a cloud with `drift`.
- The clock is pinned from both directions, as M18's is: `--steps 120` at 60 Hz
  and `--time 2.0` are the same bytes, and a run with neither must *not* match a
  baseline blessed on a drifting sky.
- Seven GPU-skipping pixel tests in `engine-render/tests/clouds.rs`: a cloud
  stands out against the sky and a zero-density one leaves it byte-identical,
  the sunlit side beats the shaded side, `density` grades, `feather`'s sense is
  the one §4 settled on, `drift` moves with the clock and wraps back, a cloud is
  visible from inside itself, and a scene with no clouds does not depend on the
  clock at all.
- Eleven generator tests in `cloud.rs`, several swept over seeds: the icosphere
  counts match `vertex_count` exactly, every lobe triangle winds outward, a
  flattened cloud rests exactly on its base plane without collapsing, no seed
  sends a child chain outside the envelope, `rise` piles children upward, and
  `mesh_for` shares one `Arc` per distinct *shape*.
- The showcase tour gains four clouds — `repo_contracts.rs::
  showcase_tour_uses_every_component_the_engine_has` requires it — which cost
  all six showcase baselines a re-bless and no other baseline anything.
- And the check this repo has learned actually settles a bit-exactness question:
  an **A/B between binaries** built at `main` and here, rendering fifteen
  scene/step combinations and `cmp`-ing the PNGs. All fifteen byte-identical.
