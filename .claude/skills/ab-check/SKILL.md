---
name: ab-check
description: Prove a renderer or engine change moves no pixel, by rendering the same scenes with two binaries and cmp-ing the PNGs. Use whenever a change touches a shader, the render path, mesh/particle/water/tree geometry generation, or anything CLAUDE.md flags as ULP-sensitive — and before claiming "no baseline had to be re-blessed."
---

# The A/B bit-exactness check

**A diff against a baseline cannot answer this question.** A baseline is one
binary's output, blessed at some past moment on some adapter and profile. When
it differs you cannot tell whether your change moved a pixel or the baseline
was already drifting. The check that settles it is an A/B between two
binaries over the same scenes.

This repo has been bitten by the difference: the M17 A/B found a 1-pixel
`m14_break.png` diff that turned out to be **pre-existing drift on main**, not
the change under test. (It is still there — `bin/verify-baselines` reports it.)

## The procedure

1. **Build the reference binary** from the merge base, in a scratch worktree so
   the current tree is untouched. Its own `target/` means a cold build —
   several minutes, so start it in the background.

   ```bash
   base=$(git merge-base HEAD main)
   git worktree add /tmp/ab-base "$base"
   (cd /tmp/ab-base && cargo build -p engine-cli)
   ```

2. **Render every manifest entry with both binaries, from *this* worktree's
   scenes.** Same scenes, two binaries — that is what isolates the binary as
   the variable.

   ```bash
   bin/verify-baselines --render-to /tmp/ab/new
   ENGINE=/tmp/ab-base/target/debug/engine bin/verify-baselines --render-to /tmp/ab/base
   ```

3. **Compare bytes.**

   ```bash
   for f in /tmp/ab/new/*.png; do
     n=$(basename "$f")
     cmp -s "$f" "/tmp/ab/base/$n" || echo "DIFFERS $n"
   done
   ```

4. **Clean up:** `git worktree remove /tmp/ab-base`.

## Reading the result

- **Nothing differs** — the change moves no pixel on any committed scene. Say
  how many combinations that covered; it is the number that makes the claim
  mean something.
- **Something differs** — either intended (say which scenes and why) or a bug.
  Get the numbers from `bin/verify-baselines --filter <name> --diff-dir /tmp/d`
  and look at the diff PNG.

## Caveats that matter

- **A fixture using a component the base binary does not have will fail to
  render under it.** That is expected — exclude those entries and say which.
  The claim the A/B supports is that *pre-existing* scenes are untouched.
- **If this branch edited a scene file, the A/B says nothing about that scene**
  — the input changed, not just the binary. Note it rather than counting it.
- Same machine, same adapter, same build profile on both sides. Debug, unless
  you have a reason: that is what `bin/engine` builds and what `cargo test`
  runs.
