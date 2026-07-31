# Agent ergonomics: M24 and M25

## 0. Status

Plan, not implementation. Two milestones, split by shape:

- **M24 — the CLI answers questions it already knows the answers to.** Four
  small questions an agent asks constantly and cannot currently ask directly.
  New query surface and one parsing fix; no existing output changes.
- **M25 — reports carry what the command already computed.** Two commands
  compute the answer to the most common follow-up question and then throw it
  away. Both are enrichments of an existing stdout object.

Item 5 of the original list (`engine inspect`) is placed in **M24** rather
than M25, because it is the same shape as the other three — a question the CLI
cannot answer — while M25 is strictly about existing reports underselling what
the command already has in memory. Moving it is a one-line change to this plan
if that reads wrong.

## 1. The thesis

The README now says: *discover by looking, verify by querying.* Everything
below is the querying half catching up to that claim.

The pattern in every item is the same. The engine already knows the answer —
it has the resolved scene, the stepped world, the framebuffer — and the agent
has to reconstruct it from an artifact: parse a 17.8 KB trace to learn one
position, bake a whole scene to read one Transform, spend a 1–2k-token image
read to find out a frame is black. Each reconstruction is a place where the
agent's model of the scene can drift from the engine's, which is the same
failure invariant 2 exists to prevent, one level up.

Nothing here adds a daemon, a session, or state between invocations. That
property is what lets agents run in parallel, in CI, and in headless sandboxes,
and no ergonomic win is worth it.

---

# M24 — the CLI answers questions it already knows

## 2.1 Negative coordinates parse as flags

Reproduced:

```
$ engine raycast scene.json --from -6,20,6 --dir 0,-1,0
{"error":"invalid_invocation","message":"unexpected argument '-6' found"}
```

`--from=-6,20,6` works. Roughly half of all coordinates in any centered scene
are negative, so an agent hits this constantly, and the error names the
argument rather than the cause — there is no `did_you_mean` that could help,
because nothing is misspelled.

**The change.** `allow_hyphen_values = true` on every argument taking a
vector or a signed scalar: `raycast --from` / `--dir` today, plus the same
attribute on `terrain-height --at` (§2.3) as it is added. Audit the rest of the
CLI for signed arguments at the same time and fix the class, not the instance.

**Rejected:** teaching the guide to write `--from=`. The workaround documents
a defect instead of removing it, and every agent that has not read that
sentence still hits it.

**Verification.** A CLI test casting from a negative origin, asserting the
returned hit point — a test that fails today.

## 2.2 One component's schema, without `jq` gymnastics

`engine list-components` returns `.components` as an array of *names* and the
schemas under `.component.oneOf[]`, discriminated by `.properties.type.const`.
Writing that `jq` correctly is a two-attempt operation for a reader who has not
seen the shape before; it was, in this session, for the author of the guide.

**The change.** `engine list-components [--component NAME]`.

- With `--component`, print that one component's schema as the stdout object.
- An unknown name is a new error code, `unknown_component_query`
  (`Input`, exit 1), carrying `did_you_mean` from the same Levenshtein
  suggester every other name error uses.
- Without the flag, output is **byte-identical to today's**. The checked-in
  `schemas/component-schema.json` is enforced by a repo-contract test, and the
  editor and validator read the same document.

**Rejected:** reshaping the top-level output so schemas are keyed by name.
It would be the nicer shape, and it breaks the schema file, the validation
walk, the editor's widget generation, and any agent script in the wild — a
large blast radius to save one `jq` selector.

**Also considered, deferred:** a `--fields NAME` mode printing a compact
name/type/range/default table rather than a JSON Schema. This is probably what
an agent actually wants most of the time, but it invents a second
representation of the component vocabulary, and the doc comments in the schema
carry rationale a table would drop. Revisit if `--component` output proves too
heavy in practice.

## 2.3 Ground height without a raycast trick

`world.terrain_height(name, x, z)` exists for scripts. From outside a script
the only route is a downward `raycast`, which requires knowing the trick and
requires the terrain to carry a `Collider` — a terrain authored for looks, with
no collider, cannot be queried at all.

Placement is the most common operation on terrain: it is what keeps a tree from
floating, a prop from being buried, and a particle emitter from firing out of a
hillside.

**The change.** `engine terrain-height <scene.json> --at x,z [--entity NAME]`,
reporting `{entity, x, z, height}`. `--entity` picks among several patches;
absent, it defaults to the only one, and a scene with several and no `--entity`
is an error naming the candidates — the exact convention `road-centerline`
already uses.

**It must call the same sampler the script API calls.** M22's central claim is
that terrain has one implementation and therefore nothing to keep in agreement;
a second height evaluator in the CLI would give that away for a convenience.

**Rejected:** extending `raycast` with a `--down-from x,z` shorthand. It
answers a different question — where the *collider* is, which is the displaced
grid, not the height field — and silently returns nothing for a colliderless
patch.

## 2.4 `engine inspect` — what is this entity actually set to

There is no way to ask the engine about an entity. Reading the JSON is not the
same thing: absent fields *are* the documented defaults, so the file
under-specifies the entity by design, and an agent reasoning about a
`Material` that writes only `albedo` is guessing at four other values.

**The change.** `engine inspect <scene.json> [--entity NAME]` printing, per
entity, its resolved components — every field, defaults filled in — plus the
resolved world transform. Absent `--entity`, every entity, name-sorted.

**Resolution must go through the ordinary parse path**, serializing the
components the engine actually built. Re-deriving defaults in the CLI is how
`inspect` starts lying about the scene the renderer sees, which is worse than
not having it.

**Open, decide during implementation:** whether `inspect` reports the entity's
state *after* `--steps N`. It would be useful and it overlaps M25's simulate
report; the current inclination is to keep `inspect` a pure function of the
file at rest and let M25 own everything post-simulation, so the two commands
answer "what did you author" and "what happened" rather than blurring.

## 2.5 M24 scope and risk

New surface only, plus one parse fix. No component changes, so no schema
regeneration and no showcase-tour entry. No renderer, geometry, or physics code
is touched, so **no baseline can move** — and `bin/verify-baselines` reporting
30 of 30 unchanged is the claim, not an A/B, since no pixel path is involved.

New error codes: `unknown_component_query`, plus whatever `terrain-height` and
`inspect` need for "no such entity" and "entity is not a Terrain" — each
declared in `codes.rs` with its exit class and mirrored into
`docs/error-codes.md`, which a repo-contract test enforces in both directions.

---

# M25 — reports carry what the command already computed

## 3.1 `simulate` does not say where anything ended up

Reproduced — the whole report:

```json
{"contacts": ..., "simulated_steps": 120, "timestep_hz": 60}
```

To learn where a body ended up, the agent must either write a trace (125 lines
/ 17.8 KB for a 120-step run) and parse its tail, or `--bake` an entire scene
file and read a Transform back out. The M22 CLI test does the latter to assert
one number.

The data is already there. The trace's final line is exactly:

```json
{"entity":"Dropped","position":[...],"rotation":[...],"linear_velocity":[...],"step":120}
```

**The change.** The `simulate` report gains an `entities` array: the same
per-entity state the trace's last row carries, name-sorted, for the dynamic
bodies the trace already enumerates. `--entity NAME` (repeatable) narrows it,
and also reaches entities the trace does not enumerate — a scripted kinematic
platform, a camera a chase script is driving — which is the case `--trace`
cannot serve at all today.

**Additive, and the ordering is a contract.** Existing keys keep their
meaning, so an agent parsing `simulated_steps` is unaffected. Name-sorting is
not cosmetic: it is the same rule the trace follows so that an unchanged scene
reports identically, and it must not depend on archetype iteration order.

**Not changed:** the trace format, the bake format, and the golden traces
`m8_drop.trace.jsonl` and `m14_break.trace.jsonl`. Those are pinned artifacts;
this milestone adds a report field and touches neither.

**Rejected:** making `--trace` cheaper, or adding `--trace-last`. The problem
is not that the trace is expensive, it is that answering a question about the
end state should not require producing a file about every step.

## 3.2 A render digest in the screenshot report

`screenshot` reports `entities_drawn`, which catches "nothing loaded" but not
"everything is black" — the most common bad render, and the one whose diagnosis
currently costs a 1–2k-token image read. A scene with one light and no ambient
is the classic case, and the render is *correct*; only looking reveals it.

**The change.** The `screenshot` (and `filmstrip`) report gains a compact
digest computed from the framebuffer that is already in memory before PNG
encode: mean luminance, and the fraction of pixels differing from the frame's
own background/clear value. An agent can then read the image when the digest
says something is there, and skip it when the digest says the frame is empty.

**Determinism is the trap here, and it is a real one.** M22 records that a
terrain patch under MSAA renders ~24 pixels differently run to run on this
adapter. A full-precision mean over the frame would therefore differ in its
low digits between two runs of an unchanged scene — turning a diagnostic into
a source of phantom diffs, exactly the failure mode CLAUDE.md warns about with
checks that always fail.

So the digest is **quantized** — rounded to a fixed small number of decimals,
chosen so that adapter noise cannot move it — and it is documented as a
diagnostic, never a pin. `diff-render` remains the only thing that pins a
render, and it stays bit-exact by default.

**Rejected:** a perceptual hash. It invites exactly the "compare two renders by
number" use that `diff-render` already does properly and with a diff PNG to
show where.

**Rejected:** making the digest optional behind a flag. It costs one pass over
a buffer that is already resident; a flag would mean an agent has to know to
ask, which is the whole problem being fixed.

## 3.3 M25 scope and risk

**The pixel-path claim needs care.** Nothing here changes what is drawn, but
the digest reads the framebuffer between render and PNG encode. That is a read,
so `bin/verify-baselines` showing 30 of 30 unchanged is sufficient evidence and
an `ab-check` is not required — unless implementation ends up touching
`offscreen::render`'s structure, in which case the A/B is mandatory and the
`ab-check` skill is the procedure.

Both changes are additive keys on existing stdout objects. `docs/cli-contract.md`
gains their description; the CLI tests that assert report shape need updating,
and that update is the deliberate moment to confirm nothing else parsed those
objects positionally.

---

## 4. Order, and what "done" means

M24 first: it is smaller, it is pure addition, and `inspect` plus
`terrain-height` are what an agent needs while authoring, which is the more
common activity than simulating.

Each milestone is done when:

1. `bin/engine validate examples/scenes/*.json --strict` passes.
2. `cargo test --workspace` passes, with a new CLI test per item — each written
   so it **fails against today's binary**, since a test that passes before the
   change tests nothing.
3. `bin/verify-baselines` reports 30 of 30, proving the reports moved and the
   pixels did not.
4. `docs/cli-contract.md`, `docs/error-codes.md`, and the `AGENTS.md` the
   scaffold ships are updated together — the last one matters most, since the
   guide is what a new user's agent reads instead of this document.
5. CLAUDE.md records the decisions a future session would otherwise re-derive:
   the one-sampler rule for `terrain-height`, the name-sort contract on the
   simulate report, and the digest's quantization and why.

No new fixture or baseline: neither milestone adds a visual feature, and a
fixture that pins no new pixels is a baseline to maintain for nothing.
