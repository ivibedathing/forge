# Foliage sway (M46)

*The wind, in the vertex stage. Design doc: `designs/foliage-sway-design.md`.*

Four fields on `Tree` — `wind` (degrees the outermost twigs lean at full gust),
`wind_speed` (m/s a gust travels), `wind_direction` (degrees, `0` toward −Z) and
`flutter` (degrees a single leaf beats about its own attachment) — turn M19's
static recipe into something that moves. Three of the four are `Meadow`'s
fields, spelled the same and meaning the same, so a scene authors one breeze and
both systems obey it.

## The one place the house rule is broken on purpose

**This is the only milestone whose new behaviour defaults to on.** Every other
one — M16 loudest — bought its "no baseline moved" claim by making absence mean
the old behaviour. Here the user's call was that a tree standing perfectly still
is *wrong* rather than plain, so `wind: 2.5` and `flutter: 9.0` are the defaults
and thirteen committed baselines re-blessed in the milestone commit: `m19_trees`,
five `m21_daylight` hours, `m11_lap` (the car circuit is lined with trees), and
all six tour frames. Five of the six exceed the tour's own tolerance; the sixth
is inside it and re-blessed anyway, because the A/B says its bytes moved and a
baseline that is only *nearly* current is the kind of drift this repo has been
bitten by before.

What survives is the **opt-out**, and it is exact rather than approximate:
`wind: 0` with `flutter: 0` makes `Tree::sways()` false, which emits no sway
channel, which fails `MeshData::is_foliage`, which routes the draw onto the
pipelines that compile `mesh.wgsl` as it sits on disk. Two tests hold it —
`a_windless_tree_carries_no_sway_channel` and
`a_windless_tree_renders_the_same_at_every_moment` — and the A/B measured it
against a `main` binary.

## Where the numbers live, and why there

Three constants in `tree.rs` describe the compliance of a whole tree, and they
are shaped by what each part does in a breeze rather than fitted to anything:

| Constant | Value | What it says |
|---|---|---|
| `TRUNK_SHARE` | 0.12 | The trunk's top drifts; its foot is exactly pinned. |
| `BRANCH_SHARE` | 0.55 | Each branch generation gives back half the distance left to fully compliant, so **depth** makes twigs the loosest thing in the tree. |
| `BEND_CURVE` | 1.7 | The weight ramps along a branch on a curve, because a cantilever's deflection piles up toward its free end. A straight ramp swings the base of every branch and reads as rubber. |

A branch entering at weight `w0` reaches `w0 + (1 - w0) * share` at its tip and
interpolates on `t^BEND_CURVE`; a child starts at *exactly* the weight its
parent had where it attaches, so continuity across a join is by construction and
the canopy bends as one surface. `sample()` interpolates `Node.sway` for that
reason — leaves and children both read it, and two derivations would be two
things to disagree.

**A leaf takes one weight and one phase over all of its vertices**, so the bend
carries it rigidly instead of stretching it. Its own motion is the flutter.

## The two things that would have quietly broken every tree in the repo

- **The flutter phase is `roll / τ`** — a number `emit_leaves` had already drawn
  for the leaf's spin about its midrib. Drawing a fresh random number would have
  shifted every subsequent draw in the generator, and **the draw sequence is what
  a `seed` means here**: every tree in every committed scene would have changed
  shape to gain a flutter. `the_wind_fields_move_no_vertex_of_the_geometry`
  pins it.
- **`TreeKey` grew one bit, not four words.** The geometry depends only on
  *whether* the channel is emitted, so trees differing in `wind` share a mesh
  and one upload — and animating `wind` (which `animation.rs` now allows) costs
  a uniform rather than a regrown tree every step.

## Why it is a producer at the M27 seam

`mesh.wgsl`'s four lighting lines are ULP-sensitive, so the wind is spliced,
never branched: `foliage_producer()` sets `VertexContribution::position`, the
field skinning uses, which at most one producer may claim. That is not a
conflict deferred — a tree is a recipe with no skin and a character is a skin
with no recipe — but `vertex_stage`'s assertion is what will say so out loud if
either ever changes.

The wind rides the **object** uniform (`foliage_wind`, `foliage_clock`), not the
frame's. That is worth keeping: `water.wgsl`, `road.wgsl` and `meadow.wgsl` each
carry a hand-maintained near-copy of the frame uniform, so appending a field
there is a four-file change with a positional-offset trap in it (CLAUDE.md,
Traps). Nothing else in the mesh path wanted a clock, and per-draw is where the
flutter amplitude has to be anyway — it is the difference between the bark draw
and the leaf draw of the same tree.

## Traps this milestone found

- **The bend is computed in the entity's local frame, so the CPU packs the wind
  direction in that frame** (`item.model.inverse().transform_vector3`). Packing
  the world direction instead is invisible on an unrotated tree and wrong on
  every yawed one: a scene that rotates its trees for variety would have them
  leaning in different directions in the same gust. The gust *travel*
  coordinate still dots world position against that local direction, which is a
  phase offset for a yawed tree and nothing more — noted in `foliage.wgsl`
  rather than fixed, because fixing it costs a third uniform lane pair to
  correct *when* a gust arrives, never which way it blows.
- **The casters must apply the same displacement, flutter included, or the
  leaves acne.** Which is why `foliage_shadow()` gives the solid caster a
  **normal attribute it otherwise has no use for** — the flutter displaces along
  it. A caster that agreed about the bend and disagreed about the beat produces
  crawling self-shadow noise on the foliage only, which is as far from its cause
  as this engine gets.
- **A caster must declare the object uniform out past the fields it reads.**
  `shadow.wgsl` stops at `surface` and the wind sits past terrain's table and the
  material maps; uniform offsets are positional, so the splice extends the whole
  struct. `shadow_cutout.wgsl` needed the *other* treatment — it declares its own
  `TerrainLayer`, so replacing its struct wholesale would redeclare the type, and
  the splice appends `map_volume` (**declared, never read**) plus the two wind
  fields instead.
- **`Tree` is a recipe whose `Material` is its bark, and the bark can be
  transparent.** The foliage set covers the opaque colour passes and the two
  casters — leaves are opaque by construction, so a transparent *bark* is the
  only way to reach the blended pass, and such a tree would render still while
  the rest of the scene moved. That is the "feature renders as if absent" trap,
  so it is a warning: `tree_sway_needs_opaque_bark`.

## What is deliberately absent

No normal rotation under the bend (a couple of degrees, under a canopy's own
shading noise — a sharper wind is where that stops being true), no separate
flutter-frequency field (it is `0.6 + 0.45 · wind_speed` Hz, so a stiffer breeze
beats leaves faster), no scene-level wind block, and nothing physical: no force
on a body, no drag, nothing a script can query. Motion is a pure function of
(files, time), which is what keeps a moving tree inside a `diff-render` baseline
— `m46_foliage_sway.png` is pinned at `--time 2.4`, bit-stable over five renders.

## The A/B

Against a `main` binary (`8ffef7d`), rendering every manifest entry with both:

- **35 of 35 comparable artifacts byte-identical.** Every scene with no tree in it is untouched —
  which is what a change that adds four lazily-built pipelines and two uniform lanes should be.
- **7 differ on purpose**: `m19_trees`, five `m21_daylight` hours and `m11_lap`, the fixtures whose
  trees now move.
- **6 tour frames excluded**, for the reason they are always excluded — this adapter does not
  reproduce them against themselves — and they contain trees besides.
- **1 unrenderable by the base binary** (`m46_foliage_sway.json` names fields it does not have),
  which is the expected shape of a new fixture in an A/B.

And the claim the whole default-on departure rests on, measured rather than assumed: `m19_trees`
with every tree stilled, rendered by **this** binary, is byte-identical to `m19_trees` rendered by
the **`main`** binary. `wind: 0` is not a very slow sway; it is the M19 tree.

