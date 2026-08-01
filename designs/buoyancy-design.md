# M38 — A CPU wave evaluator, and therefore buoyancy

**The fourth structural hole** (`designs/structural-holes.md` §4). Water is the only geometry
recipe in the engine that nothing can stand on, float in, or ask a question about, because its
surface exists **only on the GPU**: the Gerstner sum runs in `water.wgsl`'s vertex stage and no
Rust code can answer *where is the water at (x, z)*.

Terrain is the shape of the answer. `terrain::world_height_at` is one function, and
`world.terrain_height`, `Scene::terrain_height` and `engine terrain-height` all resolve through it
— which is why the ground is a thing props sit on, feet plant against and an agent can query
without reading a picture. Water has no equivalent, so it is scenery.

This milestone builds the mirror, holds it to the shader with a real GPU agreement test, and spends
it on the consumer the hole asks for: something that floats.

---

## 1. Scope

| | |
|---|---|
| **W0** | `engine_core::water::sample_at` — the Gerstner surface, on the CPU, as the shader computes it |
| **W1** | The query surface: `Scene::water_height`, `world.water_height`, `engine water-height` |
| **W2** | The agreement test: the GPU's surface read back out of a render and compared to W0 |
| **W3** | `Buoyancy` — a component that makes a `RigidBody` float on a named `Water` |

Not in scope, and each is named again in §8: wave-driven drift, water drag on a swimmer,
buoyancy against anything but a `Water` patch, and a boat that steers.

---

## 2. The forward evaluation, and why it is not the hard part

The shader displaces each grid vertex from its **undisturbed** world position `b`:

```
displaced = b + Σᵢ ( Qᵢ Aᵢ dᵢ.x cos φᵢ ,  Aᵢ sin φᵢ ,  Qᵢ Aᵢ dᵢ.y cos φᵢ )
φᵢ       = kᵢ (dᵢ · b.xz) − ωᵢ t
```

with `k = 2π/λ`, `ω = speed·k` and `Q = steepness/(k·A)` — the packing `water_uniform` already
performs, and the packing this evaluator must reproduce **exactly**, including
`engine_core::water::wave_direction`, which the two already share.

Transcribing that into Rust is mechanical. The interesting part is that it answers the wrong
question.

---

## 3. The inverse problem, and the one line that makes it well-posed

The shader is told a base point and computes where it lands. Every caller here has the opposite:
a world `(x, z)` — a boat's hull, a script's query, a CLI argument — and needs the surface *above
that column*. Because Gerstner waves displace **horizontally** as well as vertically, the surface
point standing over `(x, z)` did not start at `(x, z)`.

So the evaluator must invert `b ↦ b.xz + H(b.xz)`, where `H` is the horizontal gather. Fixed
point:

```
b₀ = query
bₙ₊₁ = query − H(bₙ)
```

**This converges exactly when the scene validates.** `H`'s Jacobian has spectral radius bounded by
`Σ steepness` — the same sum, by the same algebra, that `water_uniform` documents as making
"total steepness ≤ 1 precisely the point where the surface starts folding through itself", and
that `water_waves_self_intersect` already refuses to let a scene exceed. A surface that folds is a
surface with no single answer to "how high is the water here", and the validation rule that keeps
the render from curling into loops is *the same rule* that keeps this query single-valued. That is
not a coincidence to be grateful for; it is the reason the query can exist at all, and it is why
`Q` being packed as `steepness/(k·A)` rather than divided by the wave count (M18's deliberate
departure from most references) is load-bearing twice over.

Iteration is capped at `MAX_SOLVE_STEPS` and stops early when an update falls under
`SOLVE_TOLERANCE` — a fixed number of identical operations for identical inputs, so the answer is
a pure function of `(waves, x, z, time)` like everything else the engine promises to reproduce.

**Rejected:** solving analytically (there is no closed form for a sum of Gerstner waves), Newton on
the 2×2 Jacobian (converges in fewer steps, costs a matrix inverse per step, and is *less* robust
near `Σ steepness → 1` where the Jacobian is singular — the fixed point degrades gracefully into
"slightly wrong" where Newton degrades into "wrong"), and ignoring the gather entirely (at
`steepness 0.4` and `λ 4 m` the crest is displaced ~0.25 m horizontally, which is a quarter of a
boat).

---

## 4. Placing the patch: the rest plane, not the entity Y

`terrain::world_height_at` composes "the field says this much relief" with "the patch sits here".
The water equivalent has one extra wrinkle: the waves are evaluated in **world space** (M18, so
that scaling never stretches them and two surfaces at one height join seamlessly), while the
*undisturbed* surface is the entity's `Transform` applied to the unit grid.

So the query resolves in this order:

1. Invert the horizontal gather → base XZ `b` (§3).
2. Solve `model · (u, 0, v) = (b.x, ?, b.z)` for `(u, v)` — a 2×2 solve against the model matrix's
   X and Z columns. This is what makes a **rotated** water entity answerable rather than a special
   case, and a degenerate (edge-on) surface return `None` rather than a number nobody can defend.
3. `|u| ≤ 0.5 && |v| ≤ 0.5` or **`None`** — outside the patch there is no water, which is a
   different answer from "the water is at 0.0" and the difference is exactly what a boat drifting
   off the edge of a pond needs.
4. Rest height = `(model · (u, 0, v)).y`; surface height = rest height + the vertical wave sum at
   `b`.

**Rejected:** treating the surface as an infinite plane at the entity's Y. It is one line shorter
and it makes `water-height` answer confidently about a point 400 m from any water.

---

## 5. The agreement test

Two implementations of one curve is the pattern `CLAUDE.md` warns about under the query commands,
and here the duplication is unavoidable — one side must run on the GPU. The M28 precedent
(`engine-render/tests/pointer.rs`) holds engine-core's longhand inverse projection to the
renderer's real matrix; this is the same idea against a shader instead of a transform, and it has
to actually reach the GPU.

**The trick is `shore_foam`.** The water shader already computes `thickness` — how far the view ray
travels through the body before it hits what is behind — and paints foam as
`(1 − smoothstep(0, shore_foam, thickness))²`. Set a scene up so that:

- the camera looks **straight down** and the frame is an **odd** number of pixels, so the ray
  through the exact centre pixel is vertical and hits the surface over a known `(x, z)` whatever
  height the surface has there;
- a flat bed lies under the water, so `thickness` at that pixel *is* `surface_y − bed_y`;
- the sun's intensity is 0, the sky is off and `shallow_color == deep_color == black`, so every
  other term in the fragment shader is zero and `foam_color` is white;

and the centre pixel's luminance becomes **exactly** the foam ramp. Inverting the smoothstep turns
one pixel back into a surface height in metres, which is then compared against
`water::sample_at` at several `(x, z)` and several times.

This is a genuinely strong test rather than a ceremonial one: it exercises the horizontal
inversion (the pixel sees whichever surface point *displaced* over the camera's column, which is
precisely what §3 solves for), the packing, the direction convention, and the clock. A sign error,
a missing `Q`, a `wavelength`-for-`k` slip or a wrong `ω` all move the answer by tens of
centimetres against a tolerance in the low centimetres.

Its two known error terms are stated in the test rather than hidden: the rasterizer interpolates
the surface **linearly between grid vertices** while the CPU evaluates it continuously (bounded by
`A·k²·d²/8`, which the fixture keeps under a centimetre by tessellating finely), and an 8-bit
channel quantizes the foam ramp. The tolerance is set from those two, not tuned until green.

**Rejected:** a depth-buffer readback (water is depth-read-only — `depth_write_enabled: false` —
so the surface leaves no depth to sample); asserting on the water's *colour* (entangles the normal
with the thickness, and inverting it needs the Fresnel term); a silhouette test against the
background (reads the max along a ray, not a point sample); and parsing `water.wgsl` in a test,
which pins the text rather than the arithmetic and would pass on a shader that no longer compiles
to the same thing.

---

## 6. `Buoyancy`

```json
{ "type": "Buoyancy", "water": "Pond", "samples": 2, "drag": 1.2, "angular_drag": 2.0 }
```

- **`water`** — the `Water` entity, by name. Required, and required to exist: the `Meadow.terrain`
  precedent, for the same reason (one implementation of "where is the surface", named rather than
  guessed). A scene with two ponds must say which one.
- **`samples`** — columns per axis over the body's footprint, `[1, 4]`, default `2`. This is the
  only knob that is really about *fidelity*: at 1 there is a single upward force through the centre
  and a raft cannot right itself; at 2 the four corners of a hull each feel their own wave, which
  is what makes a boat pitch and roll with the water instead of hovering over it.
- **`drag`** / **`angular_drag`** — damping in 1/s that applies **only in proportion to how
  submerged the body is**, added on top of `RigidBody.linear_damping` / `angular_damping`. Water
  drag is not a property of the boat, which is why it cannot just be authored on the `RigidBody`:
  a hull thrown out of the pond has to stop being damped the moment it leaves.

**The shape comes from the `Collider`, not from new fields.** Displaced volume is the collider's
own volume — rapier computes it exactly for every shape the engine has, cuboid through trimesh, so
a sphere displaces a sphere and not its bounding box. The *distribution* of that volume comes from
the collider's world AABB, divided into `samples²` columns: each column's submerged fraction is
how much of the AABB's height at that column sits under the surface, and the force
`ρ · |g| · (V/N) · f` is applied **at that column**, so the torque that rights a boat falls out of
the same sum. One shape description, no second one to keep in agreement (invariant 5 in spirit:
the collider is the data, this is the system).

**`Water.density`** (kg/m³, default 1000, ignored by the renderer) carries the fluid. It belongs to
the lake and not to the boat: two hulls in one pond disagreeing about how dense the water is, is
not a knob, it is a bug waiting to be filed. The authoring knob for "floats higher" already exists
and is `Collider.density`, in the same unit.

**The force is vertical** — along `−gravity`, not along the surface normal. Aiming it up the normal
is the common game approximation and it looks better for about ten seconds, after which the moored
buoy has drifted out of frame, because a normal-aligned force integrates into net transport across
a wave train. Wave-driven drift is a real feature and is deferred as one (§8).

**Default off, like everything else.** No `Buoyancy` component, no force, no read of the water, and
a scene without one steps bit-for-bit as it did.

**Rejected:** buoyancy applied automatically to any dynamic body whose collider overlaps a water
patch (implicit, un-inspectable, and it would silently change every existing scene with a pond);
`volume` as an authored field (a second shape description, drifting from the collider the first
time either is edited); and a `Buoyancy` on a fixed or kinematic body — a validation error, because
the component's whole effect is a force and neither body kind takes one.

---

## 7. The query surface

```
engine water-height <scene.json> --at x,z [--entity Name] [--time T] [--steps N]
```

`terrain-height`'s twin, plus a clock, because water is the first query surface in the engine that
**moves**. Time resolves the way every other water consumer already resolves it (`--time` when
given, otherwise `steps / timestep_hz`, `scene_time` in the CLI), so the number this prints is the
number the render at that frame drew and the number the physics step used.

It reports the surface normal beside the height. The normal is what a script needs to sit a boat
*on* a wave rather than in it, it comes free from the same derivatives, and asking for it later
would mean a second command.

`world.water_height(name, x, z)` is the script call, read-only for `world.terrain_height`'s reason:
a surface's shape is a function of its authored fields and the clock, and a script-settable one is
hidden state (invariant 2).

---

## 8. Deliberately absent

- **Wave-driven drift.** The orbital velocity of a Gerstner wave would push a floating body along
  with it. It is a real force, it wants its own field and its own answer to "does a raft eventually
  cross the pond", and it is not needed to make something float.
- **Drag on a submerged swimmer**, separate from a floating hull.
- **Buoyancy against anything else.** No floating on terrain, no gas, no "fluid" abstraction. There
  is one fluid in the engine and it is spelled `Water`.
- **Waves that respond to the body.** Nothing the boat does disturbs the surface. The surface stays
  a pure function of (file, time), which is what keeps `--time` renders reproducible and what lets
  the CPU and the GPU agree at all.
- **A CPU normal for the *ripples*.** M18 is explicit that the detail slope field has no height
  behind it and that nothing physical may depend on it. This evaluator reads `waves` and ignores
  `detail` entirely — the same surface the geometry has, not the one the pixels have.

---

## 9. Determinism and what re-blesses

The evaluator is pure arithmetic over authored fields and an explicit clock, so nothing here reads
a wall clock, an iteration order or a hash.

What does move: **the showcase tour gains a floating body**, and `CLAUDE.md`'s trap says a physics
scene is not stable under the addition of a collider anywhere in it — the collider set is an input
to the broad phase. The tour's six frames therefore re-bless, which is the documented cost of a
scene gaining a body and is why they are the six that already carry a tolerance rather than a hard
pin. Every other baseline must not move, and the A/B between binaries is what proves it — nothing
in this milestone touches a shader, so the claim is falsifiable and expected to hold.

---

## 10. Build order

1. `water::sample_at` + the inversion, with unit tests (flat water, a known sine at `steepness 0`,
   the inversion round-tripping against the forward map).
2. `Scene::water_height`, `engine water-height`, `world.water_height`.
3. The GPU agreement test (§5) — **before** buoyancy, because everything downstream is only as
   trustworthy as this.
4. `Water.density`, `Buoyancy`, validation, schema regeneration.
5. The physics integration.
6. `verify/m38_buoyancy.json` + baseline + manifest entry + CLI test; the tour's floating body.
7. The full sweep, the A/B, the note, `CLAUDE.md`, and `structural-holes.md` losing its §4.
