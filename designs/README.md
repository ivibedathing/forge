# designs/

Two kinds of document live here.

**Cross-cutting, and never pruned:**

- `agent-native-engine-design.md` — the source of truth for layout, formats and
  build order. Read it before any structural decision; several §9 questions are
  **still open**.
- `milestone-verification-scenes.md` — the fixture registry, M4 through M32. The
  standard check's "look at the PNGs" step is defined here.
- `showcase-tour.md` and `arena-shooter.md` — the two demo scenes, both live.
- `distribution-design.md` — release workflow, `install.sh`, `engine init`.

**Per-milestone designs**, which are where the *rejected alternatives* live —
the reason a thing is shaped the way it is, rather than what shape it has.

## The M4–M25 prune

The design docs for **M4 through M25** were deleted once their milestones were
built and their conclusions had been folded into `CLAUDE.md`. The eighteen:

| Milestone | Doc |
|---|---|
| M4 materials + lighting | `materials-lighting-design.md` |
| M5 validation | `validation-design.md` |
| M6 diff-render | `diff-render-design.md` |
| M7 GUI editor | `gui-editor-design.md` |
| M8 physics | `physics-design.md` |
| M9 animation | `animation-system-design.md` |
| M10 scripting | `scripting-design.md` |
| M11 input | `input-design.md` |
| M11.6/M12 HUD | `hud-design.md` |
| M14 breaking | `breaking-design.md` |
| M17 fire + point lights | `fire-and-lights-design.md` |
| M18 water | `water-design.md` |
| M19 trees | `tree-design.md` |
| M20 clouds | `cloud-design.md` |
| M21 day/night | `daylight-design.md` |
| M22 terrain | `terrain-design.md` |
| M23 roads | `road-design.md` |
| M24/M25 agent ergonomics | `agent-ergonomics-design.md` |

M12 wheels/collision, M13 particles, M15 frame cost and M16 environment never
had one; their record has always been `CLAUDE.md` alone.

**Where the content went.** `CLAUDE.md` carries a section per system with the
decisions that cost time — it is the index and the survival guide, and it was
written to be sufficient. The full text of every deleted doc is in git history:

```sh
git log --diff-filter=D --oneline -- designs/water-design.md   # the deleting commit
git show <commit>^:designs/water-design.md                     # the file itself
```

**What was lost, stated plainly.** `CLAUDE.md` records what each system *does*
and the traps in it; the deleted docs also recorded the alternatives that were
weighed and rejected, at more length than the summary keeps. If a change is
about to reverse one of those decisions, read the original out of history first
— that is exactly the case the prose was written for. Two examples still cited
by surviving docs: M9's §8 rejection of animation blending (which M30 and M32
both lean on as still standing) and M11's §7 "no mouse" (which M28 reversed).

A milestone from M26 on keeps its doc. Whether it is pruned later is the same
call this one was, and it should not be made until the milestone's conclusions
are in `CLAUDE.md`.
