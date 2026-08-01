# A CPU wave evaluator, and buoyancy (M38)

*Design in `designs/buoyancy-design.md`, which holds the rejected alternatives. This note holds what
building it taught.* Closes the fourth of `designs/structural-holes.md`'s four.

**Water was the only recipe nothing could ask a question about.** The Gerstner sum lived only in
`water.wgsl`'s vertex stage, so no Rust code could answer *how high is the water at (x, z)* — which
is why terrain has `FootPlant`, props that sit on it and `engine terrain-height`, and water had
scenery. M38 writes the Rust mirror, holds it to the shader with a GPU agreement test, and spends it
on `Buoyancy`.

## The inverse problem is the whole milestone

The shader is told a base point and computes where it lands. Every caller wants the opposite: a
world `(x, z)` and the surface above it. Gerstner waves displace **horizontally** as well as
vertically, so the surface point standing over `(x, z)` did not start there — at `steepness 0.4` on a
4 m wave the crest has travelled a quarter of a metre sideways.

`Surface::base_under` inverts it by fixed-point iteration, and **the iteration converges exactly when
the scene validates**. The gather's Jacobian has spectral radius bounded by `Σ steepness`, which is
the same sum `water_waves_self_intersect` already refuses to let exceed 1. That is not luck and it is
worth stating plainly: a surface that folds has *two* answers to "how high is the water here", so the
rule that keeps the render from curling into loops is the rule that makes the query well-posed. M18's
decision to pack `Q` as `steepness/(k·A)` rather than dividing by the wave count is therefore
load-bearing twice over — it is what makes the validation bound and the convergence bound the same
number.

`MAX_SOLVE_STEPS = 32` with an early exit at `SOLVE_TOLERANCE = 1e-5` m. A typical lake at `Σ ≈ 0.5`
is under a micrometre in 20 steps; only a scene at the validator's limit reaches the cap, and at
exactly 1.0 the surface is folding anyway.

## The agreement test, and why it is `shore_foam`

Two implementations of one curve is the pattern `CLAUDE.md` warns about, unavoidable here because one
side must run on the GPU. `engine-render/tests/water.rs` holds them to each other by **reading the
drawn surface back out of a render**:

- a camera looking straight down through an **odd**-sized frame, so the ray through the exact centre
  pixel is vertical and hits the surface over a known `(x, z)` whatever height it has there;
- a flat bed under the water, so the shader's `thickness` at that pixel *is* `surface_y − bed_y`;
- sun intensity 0, sky off, and both water colours and the bed black — so reflection, sun specular
  and the lit water body are all exactly zero and the only surviving term is
  `mix(0, foam_color, foam_amount)` over a black destination.

The centre pixel is then *literally* the foam ramp, and inverting
`(1 − smoothstep(0, shore_foam, thickness))²` turns one pixel back into a height in metres.
`0.5 − sin(asin(1 − 2y)/3)` is the closed-form inverse of `3u² − 2u³`.

**Measured agreement is 1.4 mm**, against a 30 mm assertion. The two known error terms are the
rasterizer interpolating the surface linearly between grid vertices (about 1 mm at `segments: 256`
over 40 m) and 8-bit quantization of the ramp — so the tolerance is 20× the floor and still tens of
centimetres below what any real formula error would produce. **Do not widen it**; it drifting is the
signal the test exists for.

`the_probe_reads_a_flat_surface_where_the_evaluator_puts_it` is the calibration half: with no waves
the answer is known without either implementation, so it fails first if the *readback* is wrong
(sRGB decode, smoothstep inverse, off-centre pixel) rather than the wave arithmetic. Without it, a
broken probe and a broken evaluator that happened to cancel would both look like success.

Rejected: a depth readback (water is `depth_write_enabled: false` and leaves no depth to sample),
asserting on the water's colour (entangles the normal with the thickness), a silhouette test (reads
the max along a ray, not a point sample), and parsing the WGSL (pins the text, not the arithmetic).

## Buoyancy: what the shape comes from, and the two corrections the renders forced

Displaced **volume** is rapier's exact figure for the collider at density 1, so a sphere displaces a
sphere and not its bounding box. The *distribution* is `samples²` columns, each pushing up at its own
position — which is where the righting moment comes from, with nothing modelling pitch or roll
separately. No authored volume or hull size: a second shape description drifts from the first the day
either is edited.

Two things were wrong on the first working version, and **both were found by looking at the render,
not by a test**:

1. **Sample points laid over the body's *world* AABB tumble the body.** A tilted hull has a bigger
   world AABB than an upright one, so its corner columns acquire lever arms the hull does not have,
   and each one pushes the tilt further. A raft dropped into a pond went end over end within a few
   hundred steps. The fix is that the points ride in the **body's own frame** — each is then a real
   quarter of a real hull, and the torque it makes is the torque the shape has. Local XZ is treated
   as the deck plane, which is the engine's usual "the body's own axes mean something" convention.

2. **The submersion ramp must be the body's *local* draft, not its world height.** Taking it from the
   world AABB — "how tall does it stand right now" — sounds more physical and floats a tilted hull
   too high: a plank at 20° has a world AABB three times its thickness, so the same submerged
   fraction puts its centre three times further above the water. The raft visibly hovered. Equilibrium
   draft is a property of the shape and its density, not of which way it is leaning.

**`Water.density`** (kg/m³, default 1000) is the fluid, and it lives on the lake rather than on the
boat: two hulls in one pond disagreeing about how dense the water is is not a knob. It is the only
`Water` field nothing renders. The authoring knob for "floats higher" is `Collider.density`, which
already existed in the same unit.

**The force is vertical**, along `−gravity`, never along the surface normal. Aiming it up the normal
looks better for about ten seconds, after which the moored buoy has drifted out of frame — a
normal-aligned force integrates into net transport across a wave train. Wave-driven drift is deferred
as its own feature.

`drag` and `angular_drag` are added **on top of** `RigidBody`'s damping and scaled by submersion.
That is what makes them water drag rather than a second body property: a hull thrown clear of the
pond stops being dragged the moment it leaves, and a half-submerged one is dragged half as hard.

## The trap this milestone exposed: the script clock is one step behind

A script runs at the time its step **begins** at (`step_index · dt`, 0-based); physics and the render
are handed the time it **ends** at (`step · dt`). That offset predates M38 — `simulate.rs` documents
it where it passes the two — but **water is the first thing in the script API where it is visible**,
because it is the first surface that moves. Terrain never had to care: a height field has no clock.

Consequences worth knowing before debugging a disagreement:

- `world.water_height` at step N and `engine water-height --steps N` are **not** the same instant.
  Comparing them wants `--steps N-1` after `--steps N`, which is what
  `water_height_is_the_evaluator_scripts_ask` does, spelled out rather than absorbed into a tolerance.
- A script placing a prop on the water is one step behind the buoyancy in the same frame. At 60 Hz on
  a lake that is under a millimetre; on a fast swell it is not.

This cost a confusing 2.4 mm test failure that looked exactly like an evaluator bug.

## Verification

`bin/verify-baselines`: **42 of 42**, both golden traces included. The four tour frames that moved
(`showcase_450/585/646/810`) re-blessed, and the diff image confirms the change is **entirely on the
pond** — the new raft plus the broad-phase perturbation `CLAUDE.md` documents (a scene that gains a
body re-blesses). The other two tour frames did not move at all and were left alone.

`verify/m38_buoyancy.json` at `--steps 480` is **bit-exact** — five renders came back as one image —
because the camera holds no terrain and the scene renders at `samples: 1`, which is exactly the rule
M22 and M29 arrived at.

The fixture's three densities are the assertion: they share a pond, a clock and an evaluator, so
anything that broke buoyancy as a whole would move all three together. Only a working force law puts
the raft at the waterline, the buoy half out of it, and the stone on the bottom.

## Deliberately absent

Wave-driven drift; drag on a submerged swimmer as distinct from a floating hull; buoyancy against
anything but a `Water` (there is one fluid in the engine and it is spelled `Water`); waves that
respond to the body — the surface stays a pure function of (file, time), which is what keeps `--time`
renders reproducible and what lets the CPU and the GPU agree at all.

And **no CPU ripples**: this evaluator reads `waves` and ignores `detail` entirely. M18 is explicit
that the detail slope field has no height behind it and that nothing physical may depend on it. A
boat sits on the surface the waves make, not on the one the glitter suggests.
