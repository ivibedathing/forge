# The break's dust, and the emitter lifetime (M44)

*The item `designs/fracture-design.md` §7 rejected, built once the objection was answered. That
section now records the reversal; this note is what building it taught.*

M43 shipped without dust, for a stated reason: an engine-spawned `ParticleEmitter` had nowhere good
to die. M44 gives it one, and then the dust is ordinary work.

## The lifetime

Two fields on `ParticleEmitter`, both defaulting to M13:

- **`duration`** — seconds of *emission*, measured from the emitter's first step. When it is up,
  emission stops exactly as `rate: 0` does: no new particles, and the live ones finish their own
  `lifetime`. Absent means forever, which is every emitter written before this.
- **`despawn_when_done`** — once the duration is up *and* the last particle has died, the entity
  goes. Requires `duration` (`emitter_never_finishes`), because without one the flag could never
  fire, and a field that cannot do anything is an error here rather than a no-op.

The despawn is the **caller's**, not the system's: `ParticleSystem::finished` answers *which*
entities are spent and the sim loop does the removal, because the world is the loop's to mutate and
a name table has to be refreshed with it. Both drivers do it — the headless loop in `simulate.rs`
and the viewer in `app.rs` — for the reason breaks are applied in both: a played run and a simulated
one may not diverge.

`age` lives in the per-emitter runtime state, not in the component, which is the same call M13 made
for particle positions. The consequence is stated rather than hidden: **a scene baked mid-puff
reloads and puffs again**, exactly as the tour's fire restarts.

## `sync`, and the M37 hole it closed

`ParticleSystem::build` ran once and the system tracked that list forever. An emitter that arrived
later never emitted at all — and one *could* arrive: a `templates` entry carrying a
`ParticleEmitter` has been spawnable since M37, and had been silently inert ever since. Nothing
noticed, because nothing had tried.

`sync` adds states for untracked emitters, drops states whose entity is gone, and re-sorts the whole
list by name so draw order stays a function of the names rather than of arrival order. Each
emitter's RNG is its own, so the sort cannot change what any existing emitter emits.

## The trap that ate an hour: a burst born inside the thing it comes off

The first version placed the emitter **at the contact point**, which is the obvious choice and is
wrong. A contact point sits *on* the object's surface, and at the moment of a break the object is
still there — the fragments have not moved yet. So every particle was born inside the silhouette of
geometry that then depth-rejected it. The simulation produced 84 particles, the renderer received
84 particles, and the frame showed nothing.

That failure mode is worth naming because **every diagnostic said it was working**: the trace had
the spawn, the system reported live particles, `instances()` handed the renderer a full list with
plausible positions, sizes and alphas. What settled it was rendering a hand-authored emitter with
the identical numbers in an empty scene — it puffed enormously — which isolated the variable to
placement rather than parameters or pipeline.

The fix is one line and is also the physical answer: push the burst out along the face normal (for
a contact, the direction from the object's centre) by a fraction of the object's radius. Dust comes
*off* an impact face; it does not start inside the rock. `the_burst_comes_off_the_struck_face`
pins it, because a refactor that "simplifies" it back to `impact.point` would look equivalent and
would silently return to an invisible feature.

Second-order version of the same thing: the metal plate's sparks were hidden **behind the hammer
ball** that broke it, since the ball sits exactly where the burst is. That one is not a bug — it is
why sparks are fast and stretched, so they leave the occluder within a frame or two.

## The numbers, and why they are what they are

`FractureMaterial::dust(size)` scales everything by the object's radius, taken from the furthest
fragment offset: a puff on a 3 m boulder cannot be the puff on a teacup. Four genuinely different
bursts rather than one recoloured:

| Material | Reads as | What makes it |
|---|---|---|
| `stone` | dust that hangs | 1.3 s life, upward acceleration, heavy drag, billboards that grow 3.5× |
| `wood` | sawdust that falls | 0.7 s, downward acceleration, small, brown |
| `glass` | glitter | 0.55 s, fast, tiny, **additive** so it catches light |
| `metal` | sparks | 0.5 s, fastest, additive, `stretch: 0.14` so each is a streak |

**Dust has to out-contrast the ground, not match the material.** The first stone puff was
`(0.46, 0.44, 0.41)` — the honest colour of rock dust, and invisible against a grey-green floor at
this exposure. Pale (`0.78, 0.76, 0.71`) reads as dust; accurate reads as nothing. `m19-trees.md`'s
lesson in a different system.

## Verification

`verify/m43_fracture.json` re-pins at **step 52** rather than 55: the bursts are denser there and
the shard patterns are indistinguishable three frames earlier, so one frame shows both halves of
M43 and all of M44. Still bit-reproducible (three renders, one image).

The showcase tour needed **no re-bless**, which is worth recording as a measurement rather than
luck: its only material-bearing breakable is the ice pillar, it shatters at step 601, its glitter
is 0.55 s long, and the nearest sampled frame is 646 — by which point the last particles are at
alpha ≈ 0. The tour's crates carry no `material`, so they are still the M14 path and still throw
nothing.
