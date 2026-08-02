# M45 — bounced sunlight

**What it adds.** A `sun` basis in the GI bake: per probe, how much light
arrives having bounced off a surface the *sun* is lighting. M35 bounced the sky
alone, so a red wall in full sun threw nothing onto its neighbour. This is the
effect a viewer names when they see GI working, and the design that was written
down at the moment it was deferred (`global-illumination-design.md` §5.3) is the
one that got built — with two corrections, both below.

Design doc: `designs/sun-bounce-design.md`. **The component field is
`LightProbeVolume.sun_samples`, and it defaults to `0`, which is M35 exactly.**

## The three things that made this a small milestone

- **No shader was edited.** Evaluation already folds N basis sources into the
  four `Rgba16Float` planes the GPU reads; a new basis adds terms to a CPU sum.
  No binding, no `with_surface` splice, no anchor with a second claimant. After
  M35 found two of those the hard way, a GI feature that cannot reach the WGSL
  is the shape to prefer.
- **The expensive half of the bake is shared.** The primary rays are already
  cast and their hit points already found; a sun direction costs *one shadow ray
  per hit*. Measured on the showcase tour: 17.3 s at zero sun directions,
  20.0 s at four. Storage is the real cost, not time — see below.
- **The sun basis is bounced-only**, which is the design's §2 and the one idea
  the whole milestone rests on. A ray from the probe that *escapes* contributes
  nothing: the direct sun is the shader's own term with its own shadow map, and
  counting it here as well would light every lit surface twice. Only rays that
  **hit** contribute, weighted by the surface's albedo, its `N·L`, and whether
  the sun reaches it.

That asymmetry pays a second dividend that was not obvious until it was written
down: **an unoccluded probe has no hits, so its sun transfer is all zeros.**
Design §3.1's guarantee — an unoccluded probe reconstructs `sky_ambient(n)`
exactly, so turning GI on cannot change an open scene's brightness — survives
this milestone *exactly*, not approximately. Open ground is untouched; only
surfaces with geometry near them move.

## The bug: bounced sunlight made pixels darker

The first working render was **darker** than the one without it. Mean luminance
fell from 0.617 to 0.605 and 37% of the frame's pixels lost light, on a change
that can only ever add a non-negative quantity.

**It is SH-L1 ringing, and the gain that causes it is the one M35 derived.**
Reconstruction is `c0 + 3·(c1·n)`, and that 3 is `LINEAR_GAIN` — not a tuning
knob but the exact value that makes an unoccluded probe reproduce
`sky_ambient(n)`. It is safe for the *sky* basis because sky transfer is spread
over the whole sphere, so `c0` dominates every linear term. **A sun bounce is
not spread.** It arrives from one wall. For a lobe concentrated in a single
direction `u`, `c1 = c0·u`, and the reconstruction at `n = −u` is
`c0·(1 − 3) = −2·c0` — negative light, subtracted from the sky's fill, on every
surface facing away from the bounce.

The fix is `SUN_BAND_GAIN` in `evaluate.rs`: the sun basis's three linear bands
are pre-scaled by `1/3` as they are folded, so the shader's one gain reconstructs
them at an effective gain of 1. `c0·(1 + u·n)` is non-negative for every normal.
The sky basis is untouched, which is why §3.1 still holds.

**The cost is stated rather than hidden**: the bounce is *less* directional than
the geometry says, so a wall's colour spreads a little further around a sphere
than it should. A basis that could hold the sharp lobe is SH-L2 — nine
coefficients per source and five more texture planes.

The general lesson is worth more than the fix: **a derived constant is derived
against the thing it was derived for.** `LINEAR_GAIN`'s doc comment carries a
proof, the proof is correct, and it is a proof about the sky. Reusing it for a
basis with a different angular distribution was not a judgement call anyone
made — the coefficient array was simply already there.

`bounced_sunlight_never_darkens_a_surface` in `cli.rs` is that bug as a test: 24
position/normal pairs, and the assertion is only that the sun's share is never
negative. Nothing else in the suite would have said so — the baseline moved,
which is what a lighting milestone is supposed to do.

## Two places the design was reversed by writing the code

- **The header stores sampled *directions*, not the hours they came from.**
  §5.3 assumed hours. Directions are what both kinds of scene have — a static
  `DirectionalLight` has no hour — and they let the fold interpolate by the
  angle to `ResolvedLights.sun_direction`, which is already resolved, already
  time-dependent, and already the exact quantity the direct term uses. So
  `evaluate()` needed no clock argument and the four render paths that call it
  were not touched.
- **The 90° gate replaced `sun_is_dominant`.** Beyond 90° from every baked
  direction the sun term is zero, and that one line is what excludes bounced
  moonlight without asking `daylight` which body is up: the moon rides the
  mirror of the arc, so whenever it is the dominant light it sits 150° or more
  from the nearest sample. The same line makes an *animated* `DirectionalLight`
  degrade gracefully rather than stay confidently wrong — a case `inputs_hash`
  cannot catch, because an animation is not an input to the bake.

## What it costs on disk

| Scene | Sun directions | Bake |
|---|---|---|
| `verify/m45_sun_bounce.json` (static sun) | 1 | 208 KB |
| `verify/m35_gi.json` (opted out) | 0 | 88 KB, **byte-identical to M35** |
| `showcase_tour.json` (`daylight`, 4 samples) | 4 | 210 KB → 584 KB |

A daylight scene stores 12 numbers per probe per direction against the sky
basis's 24 total, so the arc is the whole cost. **This is why `sun_samples`
defaults to 0** and why the tour asks for 4 rather than the design's 8: the file
is a permanent repo cost and the author is the one who should choose it. A scene
whose sun does not move needs 1 — anything more stores the same vector
repeatedly, which `gi_sun_samples_unused` warns about.

## Traps

- **The receiver must not be shadowed by the thing standing between it and the
  sun**, which sounds obvious and cost three iterations of the fixture. Two
  walls facing each other, the sun coming from behind one of them: the *near*
  wall shadows the courtyard, so the far wall's inner face — the surface the
  whole scene exists to light — is in shade with nothing bouncing onto it. The
  fixture's white wall is 1.4 m tall against the red wall's 3.2 m for exactly
  this reason. `digest.mean_luminance` is what says so without reading the image.
- **`sun_samples` enters `inputs_hash` only when it is non-zero.** M17's rule for
  the particle RNG, applied to a digest: skip the step entirely when the field is
  off, never feed it a defaulted value. A defaulted zero is arithmetically
  reasonable and would have made **every bake in the repo stale** on the day this
  merged. It is also what let `re_baking_the_fixture_reproduces_the_committed_file`
  keep passing untouched.
- **A `daylight` scene's arc is sampled over 06:00–18:00**, and that is a
  constant rather than a function of `sun_elevation` because M21 fixed sunrise
  and sunset at every elevation. Editing `sun_elevation` or `sun_azimuth` moves
  every sampled direction and therefore invalidates the bake —
  `bake-gi --check` is what reads that back, since M35's decisions.
- **`gi-probe`'s `sun_bounce` is a difference, not a second evaluation.** It
  folds the same bake twice, once with the sun directions struck out, and
  subtracts. A separate code path would be a second model of the fold, free to
  agree everywhere except where it matters.

## What was measured

- **The postcard, as numbers.** On the fixture, a surface 1 m from the sunlit red
  wall and facing it: `sun_bounce` = `[0.278, 0.029, 0.024]`, R/G = **9.7**. The
  far flank of the same sphere, facing away: R/G = **1.09**. The rendered
  difference on that flank is `(82, 77, 79)` → `(126, 90, 90)`.
- **Nothing darkens.** Over the whole fixture frame, 0 pixels darkened and 31%
  brightened; mean `+5.95` R against `+3.23` G and `+2.94` B. On the tour's
  `showcase_646`, 0 darkened and 58% brightened, mean `+3.32/+2.38/+0.75` — warm,
  because the tour's 16:30 sun bounces off sandy ground.
- **One of six tour frames moved past the sweep's tolerance** (`showcase_646`,
  1.08% of pixels, max channel delta 44). The other five were inside
  `--threshold 24 --max-diff-percent 0.02` — the effect is real but diffuse at
  the tour's 4 m probe spacing.
- **The fixture is bit-reproducible**: five renders, one image, so
  `m45_sun_bounce.png` takes a hard pin with no `diff_args`, like `m35_gi.png`.
