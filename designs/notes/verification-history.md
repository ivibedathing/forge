# Verification history and the housekeeping record

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. `CLAUDE.md`
§Verification keeps the rituals and the current rule; this file keeps the measurements
that produced them — the flake-rate probes, the baseline-pinning history, and the
clippy cleanup's findings.*

*Why keep it: every number here is the evidence for a rule that otherwise reads as
superstition. When a sweep fails and you are deciding whether to debug or re-run, this
is the record that says which.*

## Verification (as written before the compaction)

**Run the CLI as `bin/engine`, not `cargo run -p engine-cli --`.** The shim checks whether any source
is newer than the binary (a find, ~0.02s warm), rebuilds only then, and execs; cargo's freshness walk
over this workspace costs ~8s *warm* on every call, which is the difference between a loop worth
running and one worth avoiding. Arguments pass through untouched and stdout stays clean; a rebuild
that fails comes back as one `cargo_error` line on stderr, exit 2. Default profile is **debug**,
matching how baselines are blessed. `ENGINE_PROFILE=release bin/engine …` when you want speed over
comparability.

**`bin/verify-baselines` is "look at the PNGs" as one command.** Every committed baseline is listed in
`examples/scenes/verify/baselines.json` with the scene and flags that reproduce it, and
`repo_contracts.rs::every_committed_baseline_is_listed_in_the_manifest` fails on any baseline missing
from it. NDJSON out, exit 1 on drift, `--filter` to scope, `--bless` to re-bless (from the debug
binary), `--diff-dir` to write diff PNGs, and `--render-to DIR` + `ENGINE=<other binary>` to run the
A/B bit-exactness check as a loop rather than a reconstruction. Both golden traces are checked too,
GPU-free.

**32 of the 38 baselines are pinned by a test** (M33's fixture arrived with its own), and the six
that are not are the six `showcase_*` frames — deliberately. They are not byte-reproducible on this adapter (measured repeatedly at four
to six distinct images from six renders of an *unchanged* scene, on any binary), so a test
asserting them would fail at random, which is worse than no test. They keep their `diff_args`
tolerance in the manifest and stay the sweep's job; `cli.rs` says so where someone would go to add
them.

This was 25-of-37 unpinned until the pins landed, and getting worse rather than better — 16 of 35
when first counted, with M26 and M28 each adding fixtures nothing asserted. The nineteen that were
missing now cost **3.6 s** in the CLI suite for the lot, `m11_lap`'s eleven thousand steps of
vehicle physics included, which is the answer to why it had not been done. Two of them looked
pinned and were not: `m27_water_refraction.png` was named by a test only in the **negative**
direction (with `ior` back at 1.0 the baseline must *not* match), which pins refraction as
load-bearing and says nothing about the render; and `m11_lap.png` was named only in a comment, by
a test that pins the *drive* — positions, elevation, the parked HUD strings — and never rendered
it. Both now have the positive half too. A sweep failure that will not reproduce twice in a row is worth suspecting before it
is worth debugging: since M29 **all six `showcase_*` frames** carry a `diff_args` tolerance of
`--threshold 24 --max-diff-percent 0.02`, because a meadow at `samples: 4` is not byte-reproducible
on this adapter and the tour has one in every frame (M22 had already given `showcase_646` a
threshold for its own reason). The pixel *allowance* is there rather than a wider threshold because
the residual is one or two pixels well outside it, not a haze just over it — 24/0.02 held for eight
consecutive full sweeps where `--threshold 40` alone would have been a looser claim. The other 30
entries carry no `diff_args` at all — they are bit-exact, and a failure there is real.

**M31 measured the tour's flake rate directly**, which is the cheap way to settle one of these: with
the *unchanged* pre-M31 scene, `showcase_585` came back as **5 distinct images from 6 renders** on
the M31 binary and **3 from 6** on `main`'s. A frame that disagrees with itself on both sides of a
change is the adapter; `cmp`-ing one render against one render would have called that a regression.

The clippy cleanup re-measured it on **two** frames and got the same answer, which is worth knowing
before anyone reads an A/B result as a regression: `showcase_585` came back **6 distinct of 6** on
the new binary and **5 of 6** on `main`'s, and `showcase_646` **3 of 6** and **4 of 6**. Its A/B
found exactly those two frames differing out of 36 artifacts — and neither binary agrees with
itself on either, so the difference is the adapter and not the change. **This is the reason the
`md5`-it-N-times step is not optional**: a two-artifact A/B failure looks damning and here meant
nothing.

The `draw` split then found **three** — 585, 646 and **`showcase_810`** — and the probe settled 810
the same way: **3 distinct of 6 on both binaries**, `main`'s included. That retires the older note
that 810 had "been seen to flake once"; it is measurably in the class, not a one-off, which is what
the section above already predicted when it said the whole tour is. Three sweeps have now each
picked a different subset of the six tour frames, so **which** of them differ carries no
information — only whether the differing frame is stable under repetition does.

**Blessing gotcha that cost a sweep here: `--filter` is a substring match, not a regex.**
`--filter "m28|showcase"` matches nothing and blesses nothing, reporting success — run one filter
per artifact family and check the `checked` count in the summary line.

The three repeated rituals are skills in `.claude/skills/`: `verify-baselines`, `ab-check`,
`milestone`.

`cargo test --workspace` is the real check, not `cargo build`.
`crates/engine-render/tests/headless_render.rs` renders offscreen and asserts on pixel values,
because "the window opened and did not crash" does not distinguish a working renderer from a culled
triangle or a shader that writes nothing. Those tests skip cleanly (rather than fail) when no GPU is
available.

Backface culling is **on**, and the M0 triangle is wound counter-clockwise in clip space to match
wgpu's default front face. A wrongly-wound triangle renders nothing at all — if geometry is
invisible, suspect winding before suspecting the pipeline.

## Build order and remaining work (as written before the compaction)

M0 window+triangle → M1 CLI skeleton + JSON error convention → M2 JSON scenes + ECS → M3 glTF/texture
assets → M4 materials + lighting → M5 validation hardening → M6 diff-render → M7 GUI editor (E0–E2) →
M8 physics → M9 animation (A0–A1) → M10 scripting — **the roadmap is complete.** Each milestone from
M4 on ends by running its fixture from `designs/milestone-verification-scenes.md`.

Deferred follow-ups: editor E3 (structure edits) / E4 (undo), the
M5-era deferrals (`--fix`, watch mode), and — after M16–M20 — planar reflections, shadow cascades (which
is also what cloud shadows need), shadows from point lights, spot lights, a CPU wave evaluator and
buoyancy, a light on the tour's explosion, a sky-dome cloud layer for cirrus and overcast, and
tree LOD and wind. (Refraction and texture-mapped materials landed in M26, and the showcase tour's
bark is authored from them. **Alpha-cut leaves are still a missing feature**, not an authoring job:
`Tree::leaf_material` synthesizes a `Material` from `leaf_color`/`leaf_roughness` alone, so leaf maps
and an `alpha_cutoff` mean new `Tree` fields, a schema regeneration, and a validation pass.) After M23: road junctions (two roads crossing wants a patch primitive, not a ribbon), banked
cross-sections, per-point road width, roads that follow a `Terrain` instead of carrying their own
heights, and textures for asphalt grain (analytic markings beat a texture for anything periodic, but
grain is not periodic). After M30: editor picking against the posed mesh (foot IK and
stride-driven locomotion landed in M32, skinned collider proxies in M33). After M33: ragdolls
(physics writing the skeleton, which is the one-way rule reversed and wants its own answer to where
the pose then comes from), proxies that resize with the posed bone, and generating a proxy set from
the skin's vertex weights. After M32: planting against
arbitrary colliders rather than only a `Terrain` (which wants an answer to the purity question M32
declined to give), arm and hand IK with authored pole targets, toe joints, and a locomotion rule
richer than one clip per gait. **Blending stays rejected**, not deferred — see the design's §1. After M31: a
bitmap-font atlas (the sanctioned path to better text — a PNG plus an in-repo JSON of glyph cells,
sampled nearest, no new dependency and no float, arriving as a `font` field whose absence is the 8×8
font), pointer lock and scroll, text input and focus, per-side padding, and world-space UI (a health
bar over an enemy's head is a *projection* question and wants `world.project(x, y, z)`).

**The M31 audit's housekeeping is done**: `scene_renderer.rs` and `validate.rs` are split, the
clippy warnings are cleared and their CI step is blocking, every reproducible baseline has a test,
and both `docs/` files the design doc sketched now exist. What is left of that list is one standing
rule rather than a task: **a new fixture arrives with the CLI test that diff-renders it**, in the
same commit, unless it is in the tour's nondeterministic class — in which case say so where the
test would have gone.

**The clippy warnings are cleared and CI's clippy step is blocking.** Six of the twenty-eight were
not bugs to fix but the lint being wrong, and they carry a local `#[allow]` with the reason —
**read it before deleting one**. Five are the `!(a > b)` comparisons in `validate/component.rs` and
`engine-script`, written negated *precisely so NaN fails*; clippy's suggested `a <= b` is false for
NaN, so "fixing" them would let a NaN far plane, collider dimension, meadow stage or explosion
radius validate clean. The sixth is `drop(write_object)` in `scene_renderer/mod.rs`, which releases the
closure's mutable borrow of `object_bytes` — deleting it does not compile. Four
`too_many_arguments` allows carry their own rationale (a nine-field keyframe constructor, a
recursive validation walk threading a JSON location, a collider builder naming four geometry
sources, and eleven index-aligned slices on the blended draw path). One genuine defect fell out of
it: the editor cloned `ResolvedLights` under a comment claiming M17 had made it non-`Copy`, when
M17 had deliberately kept it `Copy` with a fixed-size point-light array — the comment asserted the
opposite of the design it cited.
