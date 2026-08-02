# Bounced sunlight (M45)

**The postcard case.** M35 gave the engine a fill light that knows what is above
a surface, and the effect a viewer names when they see GI working — *a red wall
reddening the white one beside it* — is not in it. M35 bounces the **sky**. The
sun, which is where nearly all the light in every scene in this repo comes from,
is a direct light with a shadow map and contributes no bounce at all. So a wall
in full sun throws nothing onto its neighbour, and the ground under a red truck
at noon is grey.

`global-illumination-design.md` §5.3 wrote this milestone's mechanism down at the
time it was deferred, so that the choice would not have to be made twice. This
document takes that section, checks it against what the build actually learned,
and settles the four things it left open.

## 1. Scope

**In:** a `sun` basis in the bake, sampled along the scene's own daylight arc;
per-probe transfer for it in the bake file; the fold that turns it into the same
four planes the GPU already reads; `sun_samples` on `LightProbeVolume`;
`gi-probe` reporting the sun's share separately; a fixture that is the postcard.

**Out, each with a reason:** specular GI, `PointLight` and emissive bounce
(transfer is linear in intensity, so those are a *per-light basis vector* and a
different milestone), dynamic occluders, bounced **moonlight** (§4), and anything
that runs per frame on the GPU.

**Nothing on the GPU changes.** This is the property that makes the milestone
small, and it is worth stating before the mechanism: evaluation already folds
*N* basis sources into exactly four `Rgba16Float` planes on the CPU. A fifth,
tenth or twentieth basis source adds terms to a CPU sum. **No shader is edited,
no binding is added, `with_surface` is not touched.** After M35's two-claimant
anchor incident, a GI feature that cannot reach the WGSL is the shape to prefer.

## 2. The spine: the sun basis is *bounced-only*

The tempting formulation is "add the sun to the basis set the way the sky is in
it". That is wrong here, and the reason is the one thing §5.3 did not say.

A probe's sky transfer counts a ray that **escapes** — the sky is what you see
when nothing is in the way. If the sun basis worked the same way, a ray that
escaped toward the sun would deposit the sun's radiance into the probe, and the
shader would then add that to the direct sun term it already computes with the
shadow map. **Every lit surface would be lit twice.**

So the sun basis counts only rays that **hit**:

```
T_sun[k][c] = (1/S) · Σ_d  SH_c(d) · albedo(hit_d) · max(0, n_hit · −L_k) · vis(hit_d → L_k)
```

— for each of the *S* sample directions `d` from the probe, the surface it hits,
how much sun that surface receives, and its colour. A ray that escapes
contributes nothing.

Two consequences fall out, and both are load-bearing:

- **An unoccluded probe has zero sun transfer.** No rays hit, so the sum is
  empty. Design §3.1's guarantee — an unoccluded probe reconstructs
  `sky_ambient(n)` *exactly*, so turning GI on cannot change the brightness of an
  open scene — survives this milestone **untouched**, not merely approximately.
  Open ground stays byte-identical; only surfaces with geometry near them move.
  That is also the sharpest test available, and §7 pins it.
- **The units are the sky basis's units by construction.** The accumulation is
  the same shape with `sky_reaching(hit)` replaced by `NdotL · vis(hit → sun)`,
  the same `1/S` normalization, and the same SH-L1 projection reconstructed with
  the same `LINEAR_GAIN`. Nothing has to be reconciled between the two bases
  because neither one was ever in physical units — both are in *the engine's*
  units, which is what makes the fill they produce comparable to the fill they
  replace.

The marginal cost is **one shadow ray per hit point per sun direction**. The
primary rays are already cast and their hit points already found; a sun direction
adds a visibility query at each. This is why *N* can be 8 rather than 2: the
expensive half of the bake is shared.

## 3. The basis is the scene's own arc, not an ambient cube

§5.3 offered two candidates and preferred the second. The build confirms it:

- **Six axis directions** (an ambient cube of sun positions). Scene-independent,
  and it spends a sixth of its budget on light arriving from directly below.
- **N samples along the scene's own arc.** `daylight::arc(hours, elevation,
  azimuth)` already maps an hour to a sun direction, and that arc is a single
  great circle. Sampling it covers exactly the directions the scene can produce.

Take the arc. It is more accurate per stored byte, and it **degenerates
correctly**: a scene with an authored `DirectionalLight` and no `daylight` block
has one sun direction, so *N* collapses to 1 and the sun basis costs one vector
per probe. That is the common case in this repo — the M35 fixture, the arena, and
the new fixture below are all static-sun scenes — so the common case pays 12
extra numbers per probe, not 96.

The sample times are **centred in their bands**, matching `sample_direction`'s
`+0.5` convention:

```
hours(k) = 6 + 12 · (k + 0.5) / N        for k in 0..N
```

Sunrise is 06:00 and sunset 18:00 at every elevation — M21 fixed that
deliberately, and it is what makes "the lit half" a constant rather than a
function of `sun_elevation`.

**The coupling this introduces is honest and was accepted in advance:** the bake
reads the `daylight` block, so editing `sun_elevation` or `sun_azimuth`
invalidates it. The bake is a function of the scene file and the arc is in the
scene file. §6's `inputs_hash` covers it, and since the M35 decisions
`bake-gi --check` is what reads that hash back.

## 4. Moonlight does not bounce

The arc is sampled over the **lit half only**, and outside it the sun basis
contributes zero. `daylight` swings one directional light between the sun and the
moon, so at night the live "sun" is the moon, travelling the mirror of the arc —
directions no sample covers.

The alternative is to sample all 24 hours, which doubles the file to describe a
bounce whose source is `moon_intensity`, two orders of magnitude below the sun's
and below the file's own quantization once it has bounced off a surface. **Stated
as a limitation rather than hidden:** a moonlit scene gets M35's sky bounce and
no sun bounce, which is what it got before this milestone.

The gate falls out of §6's angular interpolation and needs no extra field: the
moon travels the mirror of the sun's arc, so whenever it is the dominant light it
is 150° or more from the nearest baked sun direction, and the 90° cut-off already
returns zero there. Rejected: reading `Daylight::sun_is_dominant`, which would be
a more direct statement of the intent but would put a `daylight`-shaped
dependency into a fold that otherwise works for any sun, static or not.

## 5. `sun_samples` defaults to 0, and that is not timidity

A `LightProbeVolume` gains one field:

```json
{ "type": "LightProbeVolume", "spacing": 1.0, "bake": "gi/x.gi.json", "sun_samples": 8 }
```

`0` means no sun basis — **M35 exactly, byte for byte**. Range `[0, 16]`.

Defaulting it off follows the house rule that made M16 possible, and here the
rule has teeth beyond baselines: the file is the cost. A daylight scene at
*N* = 8 stores 96 numbers per probe against M35's 24, so the showcase tour's bake
goes from 210 KB to roughly 1 MB. That is a permanent repo cost paid per scene,
and the author is the one who should choose it.

**It follows M17's precedent exactly**, which is why the field can default to 0
without invalidating a single committed bake: `sun_samples` enters `inputs_hash`
**only when it is non-zero**. M17 established the rule for the particle RNG — skip
the draw entirely when a jitter field is zero, never default it, because a
defaulted draw is arithmetically equivalent and still shifts every subsequent
one. The same discipline applied to a digest means every bake in the repo stays
valid across this milestone, and `bake-gi --check` stays green until an author
opts in.

**Rejected: defaulting to 8.** It would make the effect arrive for free with a
re-bake, and it would also silently quintuple every future bake in the repo for
scenes whose sun never moves off one direction. A scene that wants it says so.

## 6. The file, and what a reader that does not know about it does

`basis` in the header is already a named map, and §6 wrote it that way for
exactly this arrival:

```json
{"format":"forge-gi/1", …, "basis":{"sky":2,"sun":8},
 "sun_dirs":[[-0.83,0.29,-0.47], …]}
```

and a probe line gains a parallel array:

```json
{"p":[0,0,0],"sky":[[12 numbers],[12 numbers]],"sun":[[12 numbers], … ×N]}
```

`sun` and `sun_dirs` are **absent** on a bake with no sun basis, which is what
keeps every committed file byte-identical. The format version does not move: a
reader adds terms for each basis entry it knows, and `forge-gi/1` already
promised that.

**The header stores the sampled *directions*, not the hours they came from**, and
this reverses what §5.3 assumed. The hours are how the set is *generated* — walk
`daylight`'s arc — but they are the wrong thing to interpolate by at fold time,
for three reasons found while writing it down:

- **A static `DirectionalLight` has no hour.** Recording one would be a fiction,
  and every reader would then need to know which kind of bake it was holding.
  A direction is what both cases actually have.
- **The fold would need a clock.** `evaluate(baked, volume, lights, environment)`
  is called from four render paths; interpolating by angle to
  `lights.sun_direction` — which is already resolved, already time-dependent, and
  already the exact quantity the direct term uses — needs no new argument and
  cannot disagree with the light the shader is about to use.
- **It degrades gracefully when a light is animated.** A scene that rotates its
  `DirectionalLight` away from where it was baked gets a bounce that fades with
  the angle instead of one that stays confidently wrong. `inputs_hash` cannot
  catch that case — the animation is not an input to the bake — so the fold
  being angle-based is the only place it can be handled at all.

Interpolation is the **two nearest by angle, lerped**, with a hard gate: beyond
90° from every baked direction the term is zero. That gate is what excludes the
moon (§4) without needing to ask which body is dominant — at every hour the moon
is the dominant light, it sits 150° or more from the nearest baked sun direction.

## 7. Verification

- **The exactness test, strengthened.** An unoccluded probe still reconstructs
  `sky_ambient(n)` exactly *with a sun basis present and the sun overhead*. This
  is the one assertion that would catch the double-counting error §2 exists to
  avoid, and it is a CPU test over the numbers, not a render.
- **The postcard, as a fixture.** `verify/m45_sun_bounce.json`: a white wall, a
  saturated red wall facing it, a static low sun striking the red one. The white
  wall's lit face must come back measurably red — asserted through `gi-probe`,
  which reports the sun's share separately, rather than by eye.
- **The A/B.** Every scene without `sun_samples` must render byte-identically to
  a `main` binary. This is the claim the default of 0 exists to make, and the
  `ab-check` skill is how it is made.
- **`gi-probe --time`** across the arc on the fixture: the bounce must move as
  the sun does, and must fall to zero at night.

## 8. Build order

- **S0** — the field, the schema, the validation, and `sun_dirs` in the format.
  No bake change; a scene can say `sun_samples: 8` and nothing happens yet.
- **S1** — the bake: arc sampling, the shadow ray per hit, the `sun` array.
  `bake-gi` reports it; `gi-probe` can read it back before anything renders.
- **S2** — the fold: nearest-two angular interpolation against the live sun
  direction, gated at 90°. This is where a render first changes.
- **S3** — the fixture, its baseline, the tour opting in, docs and the note.

## 9. What the build changed

Three amendments, recorded here so the next reader is not misled by the plan.

1. **§6's header stores directions, not hours** — argued in place above; it was
   written as `sun_hours` and became `sun_dirs` before any code shipped.

2. **§4's moon gate is angular, not `sun_is_dominant`** — also argued in place.

3. **SH-L1 rings, and the ringing is negative light.** Nothing in this document
   anticipated it. Reconstruction is `c0 + LINEAR_GAIN·(c1·n)` with
   `LINEAR_GAIN = 3`, a constant M35 *derived* — and derived for the sky, whose
   transfer is spread over the whole sphere so that `c0` dominates. A sun bounce
   is concentrated on one wall: for a lobe along `u`, `c1 = c0·u`, and the
   reconstruction at `n = −u` is `−2·c0`. The first working render was
   **darker** than the one without the feature — mean luminance 0.605 against
   0.617, with 37% of pixels losing light.

   `SUN_BAND_GAIN` pre-scales the sun basis's linear bands by `1/3`, so one
   shader gain reconstructs the sky basis at 3 and the sun basis at 1, and
   `c0·(1 + u·n)` is non-negative everywhere. The stated cost is a softer,
   less directional lobe than the geometry supports; the fix that would not
   cost that is SH-L2, at nine coefficients per source and five more texture
   planes. §7's verification list gained
   `bounced_sunlight_never_darkens_a_surface` because of this.

## 10. Not in this milestone

Specular GI, point-light and emissive bounce, dynamic occluders, bounced
moonlight (§4), more than one volume (`multiple_light_probe_volumes`, settled
after M35), and per-cascade probe resolution. Each is in CLAUDE.md's GI backlog
with its own reason.
