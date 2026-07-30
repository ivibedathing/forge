---
name: milestone
description: The end-to-end loop this repo builds milestones with — worktree, design doc, implementation, fixture plus baseline, the full verification sweep, and the merge. Use when starting or finishing a milestone (M20, M22, …), or any change large enough to earn its own verification fixture and CLAUDE.md entry.
---

# Running a milestone

## Before writing code

1. **Ask which branch this merges into.** Several sessions run against this
   repo at once and parallel branches land in different places. Ask while the
   choice is still free — moving finished work is expensive.
2. **Take a worktree.** `git worktree add .claude/worktrees/<name> -b <name>`.
   Sessions sharing one working tree overwrite each other's edits.
3. **Read the design doc for what you are touching** — `designs/water-design.md`,
   `designs/tree-design.md`, `designs/fire-and-lights-design.md`, and so on, plus
   `designs/agent-native-engine-design.md` for anything structural. Several §9
   decisions are still open: surface them, do not pick silently.
4. **Write the design doc first** when the milestone is new. Every milestone
   here has one, and it is where the rejected alternatives live.

## While building

- **Check dependency APIs in the registry, not from memory.** wgpu 30 differs
  sharply from what training data describes; CLAUDE.md lists the traps.
- **Default new behaviour to off.** The reason M16 could add sky, fog, shadows,
  MSAA and transparency without re-blessing a single one of eleven baselines is
  that a scene omitting the block renders byte for byte as before. This is the
  house style, not a convenience.
- **Determinism is a format contract.** Seeded RNGs are written out in-repo so
  a dependency upgrade cannot reshape a forest; draw order is part of the
  contract; a defaulted draw that shifts every subsequent one moves every
  baseline.
- **Regenerate the schema** after touching any component:
  `bin/engine list-components > schemas/component-schema.json`.

## Verifying — in this order

```bash
bin/engine validate examples/scenes/*.json --strict   # fast structural gate
cargo test --workspace                                # the real check
bin/verify-baselines                                  # every committed baseline
```

Then, depending on what the change touched:

- **Renderer, shaders, or generated geometry** → the `ab-check` skill. A
  baseline diff cannot prove "no pixel moved"; only an A/B between binaries
  can.
- **A new component** → add it to `examples/scenes/showcase_tour.json`.
  `repo_contracts.rs::showcase_tour_uses_every_component_the_engine_has` fails
  otherwise, deliberately, with no allowlist.
- **A new fixture** → `examples/scenes/verify/<name>.json` plus its baseline,
  listed in `verify/baselines.json`, all in the same commit. Bless from the
  **debug** binary.
- **Look at the render.** `bin/engine screenshot <scene> --out /tmp/f.png
  --width 640 --height 360` and read the PNG. Every model rule in the tree
  system came out of looking at renders, not out of tests passing.

## Finishing

- Update **CLAUDE.md**: what the milestone added, and the decisions a future
  session would otherwise re-derive — especially anything that cost a
  debugging session.
- Commit the fixture, the baseline, the manifest entry, the schema, and the
  docs together.
- Merge only into the branch you were told, and say the worktree is ready.

## The one-line version

Edit text → `validate` → `cargo test --workspace` → render a PNG → **look at
it** → `verify-baselines` → merge. Everything else is in service of that loop
staying fast enough to run.
