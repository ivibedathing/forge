# designs/notes/

**What each system's build actually taught** — one note per system, the tier below `CLAUDE.md`.

These files were relocated verbatim out of `CLAUDE.md` when it was compacted from ~157k characters
to an index. Nothing was rewritten in the move: each note's body is the prose that section held,
and `CLAUDE.md` now carries a digest of it plus a pointer here. The reason for the split is that
`CLAUDE.md` is loaded into context every session, and a 157k-character file spends that budget on
detail most sessions never need — while the detail is exactly what you want the moment you touch
the system it describes.

**Read the note before changing the system it covers.** Nearly every one records a constant that is
load-bearing, a "simplification" that is a bug, or a measurement that settled a question — the
things that are invisible in the code and expensive to rediscover.

## How this relates to the other two tiers

| Tier | Holds | Applies to |
|---|---|---|
| `CLAUDE.md` | invariants, CLI surface, cross-cutting traps, digests | everything |
| `designs/notes/*.md` | what the build learned; the traps in one system | every system |
| `designs/*.md` | the *rejected alternatives* — why the shape is the shape | M26+ and cross-cutting |

**For M4–M25 the note is the only rationale in the working tree.** Those eighteen design docs were
deleted once their milestones were built (see `../README.md` for the list and the two commands that
recover one from git history). If a change is about to reverse one of their decisions, read the
original out of history first — that is the case the longer prose was written for.

For M26 and later the design doc survives alongside the note, and each such note links to it. The
division: the doc has the alternatives weighed *before* building, the note has what building it
taught.

## The notes

**Foundations** — `assets.md`, `m04-lighting.md`, `m05-validation.md`, `m06-diff-render.md`,
`m07-editor.md`

**Simulation** — `m08-physics.md` (collision too), `m09-animation.md`, `m10-scripting.md`,
`m13-particles-and-m17-fire.md`, `m14-breaking.md`, `m33-skinned-colliders.md`

**Geometry recipes** — `m18-water.md`, `m27-water-refraction.md`, `m19-trees.md`, `m20-clouds.md`,
`m22-terrain.md`, `m23-roads.md`, `m29-meadows.md`

**Environment and time** — `m16-environment.md`, `m17-point-lights.md`, `m21-daylight.md`,
`m15-frame-cost.md`

**Materials** — `m26-materials.md`

**Characters** — `m30-skeletal-animation.md`, `m32-locomotion.md`

**Input and UI** — `m11-input.md`, `m28-mouse.md`, `m11_6-hud.md`, `m31-ui-system.md`

**Vehicles** — `m11_5-vehicles-and-wheels.md`, `car-demo.md`

**Ergonomics and units** — `m24-m25-agent-ergonomics.md`, `m34-one-unit-is-one-metre.md`

**Demos and shipping** — `showcase-tour-notes.md`, `distribution-notes.md`

**Cross-cutting** — `verification-history.md` (the measurements behind the verification rules)

## Adding one

A milestone that earns a `CLAUDE.md` section earns a note here: the section becomes the digest and
the detail lands in `designs/notes/mNN-topic.md`. Keep the digest to the shape the others have —
what the system is, and the one or two things that will bite someone who changes it.
