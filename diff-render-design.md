# M6 — Diff-Render / Visual Regression: Design

Milestone M6 of `agent-native-engine-design.md` §8: "Diff-render / visual regression tooling."
This document settles the comparison model, the CLI contract, the tolerance and determinism story,
and how the pieces are verified. Written 2026-07-27, against the post-M2 codebase (the design
composes with M4's sRGB switch but does not depend on it — see §6).

## 1. Scope

**In:** `engine diff-render <scene.json> <baseline.png>` — render the scene headlessly through the
existing offscreen path, compare against a baseline PNG, emit a machine-readable report on stdout,
optionally write a visual diff PNG, and exit non-zero on mismatch with a structured error. Plus
the pure comparison function as a testable library API, and the baseline workflow (§8).

**Out (deferred, see §10):** perceptual/AA-aware metrics, image-vs-image mode without a scene,
ignore-region masks, multi-camera batch comparison, automatic baseline updating.

The purpose, per design doc §7: the agent (or CI) pins "this scene should always look like
baseline.png ± tolerance" and gets a deterministic pass/fail plus a picture of what moved.

## 2. CLI contract

```
engine diff-render <scene.json> <baseline.png> [--out diff.png] [--camera <name>]
                   [--threshold N] [--max-diff-percent P]
```

- **Render size comes from the baseline.** No `--width`/`--height`: the scene is rendered at
  exactly the baseline PNG's dimensions, which eliminates the entire dimension-mismatch failure
  class from the common path. Want a different size? Re-bless the baseline (§8). The
  `COPY_BYTES_PER_ROW_ALIGNMENT` padding path in `offscreen.rs` already handles arbitrary widths.
- `--camera <name>` mirrors `engine screenshot` — same semantics, same errors.
- `--out diff.png` is optional. When given, the diff image (§4) is written on both pass and fail —
  an all-faded image is itself legible confirmation of a pass. When omitted, no image is written;
  the report and exit code carry the result.
- `--threshold N` — per-channel byte tolerance, default **0** (§5).
- `--max-diff-percent P` — allowed percentage of differing pixels, default **0.0** (§5).

Success output is the JSON report on stdout (the `main.rs` convention: machine-facing success
output on stdout, one-line JSON errors on stderr). The report is printed on *both* pass and fail —
a failing run still tells the agent exactly how much differs and where:

```json
{
  "pass": false,
  "width": 1280,
  "height": 720,
  "diff_pixels": 1834,
  "diff_percent": 0.199,
  "max_channel_delta": 214,
  "threshold": 0,
  "max_diff_percent": 0.0,
  "diff_bounds": { "min_x": 512, "min_y": 200, "max_x": 700, "max_y": 391 },
  "diff_image": "diff.png",
  "adapter": "Apple M3 Pro"
}
```

- `diff_bounds` is the bounding box of all violating pixels (absent when `diff_pixels` is 0) — it
  lets an agent crop straight to the damage instead of eyeballing a full frame.
- `adapter` is the wgpu adapter name (what `engine info` reports). Cross-machine baseline failures
  are the expected hard case (§7), and the report should carry the one fact that diagnoses them.
- `diff_image` is present only when `--out` was given.

On mismatch, additionally one `EngineError` on stderr and exit 1:

```json
{"error": "render_mismatch", "message": "1834 of 921600 pixels (0.199%) differ from baseline.png (threshold 0, max allowed 0%)", "file": "baseline.png"}
```

Distinguishing "pixels differ" from "scene is invalid" is done by parsing the `error` code, not by
inventing a second exit-code convention — every existing command exits 1 with structured stderr on
any failure, and diff-render follows suit.

## 3. Comparison model

Two knobs, both explainable in one sentence each, evaluated in plain byte space:

1. A pixel **differs** when any of its four RGBA channels deviates from the baseline by more than
   `--threshold` (absolute difference, 0–255 scale).
2. The comparison **passes** when the percentage of differing pixels is ≤ `--max-diff-percent`.

That's the whole model. `max_channel_delta` in the report is the largest single-channel deviation
found anywhere, which makes near-misses self-diagnosing: a failure with `max_channel_delta: 1`
says "precision noise, raise the threshold," while `214` says "something actually moved."

**Rejected: perceptual metrics** (pixelmatch-style YIQ distance, anti-aliasing detection,
SSIM). They exist to serve human judgments of "looks the same" and buy that with heuristics that
are hard to reason about from a report. This tool's consumer is an agent that needs *predictable*
semantics: byte thresholds compose with the pixel assertions already written in
`headless_render.rs`, can be reproduced with `jq`-level tooling, and never pass a real regression
because it happened to be smooth. The two-knob model separates the two real noise sources —
magnitude noise (precision/rounding → `--threshold`) and spatial noise (rasterization edge
snapping, which produces *large* deltas on *few* pixels → `--max-diff-percent`) — so each gets
loosened only as far as its own noise requires.

Comparison happens on the bytes an agent sees in the PNG — post-M4 that means sRGB-encoded values.
The threshold is therefore in encoded space, which is the space the baseline file is in. No
decode-to-linear round trip; the PNG is the contract.

Alpha is compared like any other channel. Today's render target is opaque (alpha always 255), so
this costs nothing, and it means a future transparent-background feature can't silently drift.

## 4. The diff image

Written when `--out` is given; designed to be *looked at* by the agent, per the engine's core
loop. Three deterministic pixel classes:

- **Unchanged** (delta 0 on all channels): the baseline pixel, faded to light grayscale —
  `gray = 191 + luma / 4` where `luma = (r + 2g + b) / 4`. Structure stays recognizable; nothing
  screams.
- **Within threshold** (nonzero delta, all channels ≤ threshold): solid yellow `[255, 200, 0]`.
  Tolerated noise stays *visible* — a baseline quietly drifting toward its threshold is exactly
  what an agent should notice before it becomes a flake.
- **Violation** (any channel > threshold): solid red `[255, 0, 0]`.

The formulas are part of the contract (pinned by tests, §11) so a diff image is itself comparable
across runs. Rejected: heatmaps and blended overlays — prettier, but "red means fail, yellow means
tolerated, gray means same" is the version that survives being described in one sentence to a
model looking at a PNG.

## 5. Defaults: strict, with visible loosening

**Default `--threshold 0 --max-diff-percent 0.0` — bit-exact.** On the same machine, same adapter,
same engine binary, the render path is deterministic in practice, so exactness is achievable and
is the honest default. Two arguments for strict-by-default:

- A tolerance the user didn't choose is hidden state in the invariant-2 sense: a silent window in
  which real regressions (a one-step albedo drift, an off-by-one in a uniform) pass unreported.
  The failure mode of strictness is a spurious failure that *explains itself* — the report says
  `max_channel_delta: 1` and the fix is an explicit, git-visible `--threshold 2` in the CI script.
  The failure mode of looseness is silence.
- Every loosening is then a deliberate, inspectable decision recorded where it's used, not a
  buried engine constant.

Documented guidance (in the command's `--help` and `docs/`): same-machine regression checks keep
the defaults; cross-adapter comparisons start at `--threshold 3 --max-diff-percent 0.1` and tighten
from there using the report's numbers.

## 6. Implementation placement

- **`engine-render/src/diff.rs`** — the comparison and diff-image generation, as pure CPU
  functions over the existing `offscreen::Image` (width/height/RGBA bytes):

  ```rust
  pub struct DiffStats { /* diff_pixels, max_channel_delta, bounds, … */ }
  pub fn diff(actual: &Image, baseline: &Image, threshold: u8) -> Result<(DiffStats, Image), EngineError>
  ```

  No GPU, no wgpu types in the signature — these unit-test everywhere, including GPU-less CI,
  unlike anything behind the adapter-availability skip in `headless_render.rs`. It lives in
  `engine-render` (not the CLI) because it consumes `Image` and because `diff` is engine
  functionality a future test harness or editor should call without shelling out.
- **`engine-cli`** — the `DiffRender` subcommand: decode the baseline PNG (the `image` crate,
  already a workspace dependency) into an `Image`, render the scene through the same
  `offscreen::render` path `screenshot` uses at the baseline's dimensions, call `diff`, write the
  diff PNG, print the report, map the result to exit code + `render_mismatch`.

Dimension mismatch can only arise if a future flag reintroduces explicit sizes, but `diff` checks
anyway and returns `dimension_mismatch` — cheap insurance that the library function is safe to
call with arbitrary images.

## 7. Determinism and CI

What is and isn't promised, stated plainly because visual regression lives or dies on it:

- **Same machine, same adapter, same binary: deterministic.** This is the supported, default-config
  use case, and the integration test (§11) pins it.
- **Across adapters/drivers/OSes: not promised.** Rasterization tie-breaking, precision, and
  driver shader compilers all legitimately differ. Baselines are therefore *per-adapter
  artifacts*: CI regenerates or stores baselines for the adapter it actually runs on, and the
  report's `adapter` field is the first thing to check when a diff fails only on one machine.
- **GPU-less environments:** `diff-render` needs a GPU exactly as much as `screenshot` does, and
  fails with the same structured no-adapter error. For hermetic CI, a software rasterizer
  (lavapipe on Linux) gives wgpu a stable, driver-update-proof adapter — worth a line in the docs,
  not worth engine code.

Baseline PNGs are binary files in a text-first repo. That's compatible with invariant 1, which
bans binary *scene and asset-metadata* formats — a baseline is a test fixture, the *output* of the
text-first pipeline, reproducible from a scene file with one command. Convention:
`tests/baselines/<scene-name>.png`, adjacent to the scenes they pin.

## 8. Baseline workflow — blessing is `screenshot`

There is no `--update-baseline` flag and no bless subcommand. The blessing operation already
exists:

```
engine screenshot examples/scenes/demo_scene.json --out tests/baselines/demo_scene.png
```

Both commands drive the identical offscreen path, so a screenshot *is* a valid baseline by
construction. One command, one job (design doc §2.7); the "update" action stays a deliberate,
diffable act (a changed PNG in git status) rather than a flag that can be reflexively appended to
a failing command until it passes — the visual-regression equivalent of `--force`.

The agent loop this enables, end to end:

1. Bless: `engine screenshot scene.json --out baseline.png` — after *looking at* the PNG.
2. Edit code or scene.
3. `engine diff-render scene.json baseline.png --out diff.png`.
4. Exit 0 → nothing visible changed. Exit 1 → read the report, look at `diff.png`, crop to
   `diff_bounds`, decide: regression (fix the code) or intended change (re-bless).

## 9. Errors

All via the existing `EngineError`, no new error machinery:

- `render_mismatch` — the comparison failed (§2). Carries the summary message and the baseline
  path in `file`.
- `baseline_not_found` — the baseline path doesn't exist. Not `asset_not_found`: baselines aren't
  scene assets, and overloading the code would muddy what `asset_not_found` means to validation.
- `baseline_invalid` — the file exists but isn't decodable as a PNG (or has zero dimensions).
- `dimension_mismatch` — library-level guard (§6), unreachable from today's CLI.
- Everything upstream (scene parse/validation errors, no adapter, bad `--camera`) surfaces exactly
  as it does for `screenshot` — same codes, same shapes.

## 10. Deferred, with reserved shapes

- **Image-vs-image mode** (compare two PNGs, no render): the library function already supports it;
  exposing it is one CLI variant away if a real workflow demands it. Deferred because bash already
  has this loop covered when no render is involved, and §2.7 says commands do one thing.
- **Ignore-region masks** (exclude a rect or a mask PNG from comparison): the reserved shape is a
  `--mask mask.png` flag where black pixels are ignored. Wanted the moment a scene contains
  anything intentionally nondeterministic (animation timestamps, future particles). Not before.
- **Perceptual / AA-aware comparison:** revisit only with evidence that the two-knob model forces
  intolerably loose thresholds in a real workflow (§3's bet is that it won't).
- **Multi-camera / multi-scene batch:** a shell loop over `diff-render` is the v1 answer; a batch
  mode is justified only when adapter-init cost (~100ms per invocation) demonstrably hurts a real
  suite.

## 11. Test plan

`engine-render` unit tests (no GPU — this is the point of §6's split):

- Identical images → `diff_pixels: 0`, pass, no bounds.
- Single pixel, single channel off by exactly `threshold` → passes; by `threshold + 1` → fails;
  at `threshold 0`, off by 1 → fails. (The boundary is the contract.)
- `max_channel_delta` reports the true maximum across a multi-pixel diff.
- `diff_bounds` is the tight bounding box for a known scatter of violating pixels.
- Percent budget: N violating pixels out of W×H passes iff within `max_diff_percent`, including
  the exact-boundary case.
- Diff image pixel classes: the three §4 formulas pinned exactly (faded gray value, yellow,
  red).
- `dimension_mismatch` from mismatched inputs.

`engine-cli` integration test (skips cleanly without a GPU, same mechanism as
`headless_render.rs`):

- `screenshot` a scene, then `diff-render` the same scene against it with defaults → exit 0,
  `pass: true`, `diff_pixels: 0`. This is the same-machine determinism promise of §7 as an
  executable claim.
- Change one entity's albedo, `diff-render` against the old baseline → exit 1, `render_mismatch`
  on stderr, report on stdout with nonzero `diff_pixels` and bounds covering the object's screen
  region; diff PNG contains red inside the bounds and none outside.
- Missing baseline path → `baseline_not_found`; a text file as baseline → `baseline_invalid`.

## 12. Build order within M6

Each step leaves the workspace green:

1. `engine-render/src/diff.rs`: `DiffStats`, `diff()`, diff-image generation, full unit-test
   suite. Pure CPU; no wgpu API risk anywhere in this step.
2. `engine-cli`: the `DiffRender` subcommand — baseline decode, render-at-baseline-size, report
   printing, error mapping. Integration tests.
3. Bless `tests/baselines/demo_scene.png` from the demo scene, wire the round-trip test to it,
   and run the §8 loop once by hand: edit the demo scene, diff, **look at diff.png**, revert.
