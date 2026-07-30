---
name: verify-baselines
description: Re-diff every committed render baseline against the scenes that produce them, or re-bless them after an intended visual change. Use when checking that a change moved no pixels, when a milestone's "look at the PNGs" step comes due, when a diff-render fails and you need to know what else moved, or when blessing new/updated baselines.
---

# Verifying the committed baselines

`examples/scenes/verify/baselines.json` lists every committed baseline with the
scene and flags that reproduce it. `bin/verify-baselines` loops over it.

## Check everything

```bash
bin/verify-baselines
```

NDJSON on stdout, one object per artifact plus a summary; exit 1 if anything
differs. Both golden traces are checked too (no GPU needed for those).

Read failures with the numbers, not by eye:

```bash
bin/verify-baselines | jq -c 'select(.pass == false)'
```

`max_channel_delta: 1` is precision noise — suspect the adapter, the build
profile, or FMA contraction. A large delta means something actually moved.
`diff_bounds` says where.

To *see* a failure, write the diff images and look at one:

```bash
bin/verify-baselines --filter m17 --diff-dir /tmp/diffs
```

Red = violation, yellow = within threshold, faded gray = identical.

## Scope it

```bash
bin/verify-baselines --filter m19        # one milestone
bin/verify-baselines --filter showcase   # the tour's six frames
```

The full sweep renders ~12k physics steps for `m11_lap` and six 900-step tour
frames — filter while iterating, run it whole before committing.

## Re-bless

Only when the visual change is **intended**, and never as a way to make a
failure go away:

```bash
bin/verify-baselines --bless --filter m19
```

Blessing is `engine screenshot`, the same offscreen path `diff-render` uses.

Baselines are per-adapter **and** per-build-profile artifacts: bless from the
debug binary, which is what `bin/engine` builds by default and what
`cargo test` runs. A release build's `sin_cos` moves three pixels of
`m19_trees.png` (CLAUDE.md, M19).

## After adding a baseline

Add it to `baselines.json` in the same commit.
`repo_contracts.rs::every_committed_baseline_is_listed_in_the_manifest` fails
otherwise — a baseline missing from the manifest is a baseline nothing
re-diffs.

## What this does not answer

Whether a *renderer* change moved a pixel. A baseline is one binary's output,
so comparing against it cannot separate "the renderer changed" from "the
baseline was blessed by a different binary." That is the `ab-check` skill.
