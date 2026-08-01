# Particles (M13) and fire (M17)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Particles.*

The `ParticleEmitter` component is a seeded deterministic emitter — cone spray around the entity's
local **−Z** (the camera/light aiming convention: rising smoke is `"rotation": [90, 0, 0]`), spawn
rate via a credit accumulator, per-particle world-space acceleration/drag, and start→end
interpolation of half-size, linear-RGB color, and alpha over each particle's lifetime.

Simulation is GPU-free in `engine-core/src/particles.rs`: a private per-emitter xorshift32 RNG
**fully specified in-repo so dependency upgrades can't change sequences** (splitmix-finalizer
seeding, RNG *not* consumed on capped spawns), emitters stepped in name order — same file +
`--steps` → byte-identical pixels, which is what lets smoke live under a diff-render baseline
(`verify/m13_smoke.json`). Particle state is simulation state: created only by `--steps` (never
`--time`), never baked or traced, and a `--steps 0` render draws nothing. System order: animations →
scripts → physics → **particles** → render (an emitter riding a dynamic body trails where the body
actually went). Rendering is `shaders/particles.wgsl`: camera-facing instanced quads with a `(1−d)²`
soft-disc falloff, alpha-blended (depth-tested against meshes, depth-write off), CPU-sorted
back-to-front by camera distance with `total_cmp`.

`rate` is the one emitter parameter scripts drive — `world.particle_rate` / `set_particle_rate` — and
the setter rejects negative/NaN/f32-overflowing values **at the call** so a bad rate is a located
script error rather than a baked file that fails `validate`. It bakes change-based. Rate 0 pauses
emission without touching live particles, which is what makes gating cheap: `car.rhai` runs
`SkidLeft`/`SkidRight` at the rear contact patches off chassis sideslip (1 m/s deadband so
suspension jitter is not a skid) plus a braking-lockup term, and parks an `Exhaust` emitter at the
tailpipe each step (particles are world-space once spawned, so a moving car leaves a trail behind it
rather than dragging a plume along). All three follow the car's *height* — a contact patch pinned to
a fixed altitude smokes from inside the hill on a circuit that climbs.

**M17's five fire fields**, each fixing one reason a particle
cone does not read as flame: `blend: "additive"` (overlapping flame *brightens*; alpha blending can
only render orange smoke), `radius` (a disc of coals instead of a single apex),
`speed_jitter`/`size_jitter`/`lifetime_jitter` (a population born identical dies at one height,
drawing a flat top), `turbulence`+`turbulence_scale`, and `stretch`. **Every default is the M13
behaviour, down to which random numbers the emitter draws**: the draw order is a format contract —
direction → disc → speed → size → lifetime → turbulence — and each step is *skipped*, not defaulted,
when its field is zero, since a defaulted draw would shift every subsequent one and move every
particle baseline (`defaulted_fire_fields_consume_no_randomness` pins it by construction).
Turbulence is smooth value noise sampled along each particle's own path plus a per-particle offset
drawn at birth — smooth because per-step randomness makes a particle *vibrate* rather than arc,
per-particle because otherwise every particle follows one shared braid; the integer hash is spelled
out in-repo like the xorshift. A particle's lifespan and size scale are fixed **at birth**. Additive
is a **second pipeline** over the same shader and instance buffer (the sorted list is
stable-partitioned on the CPU), *not* one premultiplied pipeline — that alternative moves the
multiply by alpha into the shader for every particle including the ones under existing baselines.
Additive sprites draw after *all* alpha ones regardless of depth, which is what firelight scattering
in smoke looks like. `stretch` is in **seconds** of travel and elongates along the velocity's
*screen-space* projection, so a particle flying at the camera stays round.
