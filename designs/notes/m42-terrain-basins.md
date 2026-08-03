# Terrain basins (M42)

The design doc is `designs/terrain-basins-design.md`, and it holds the rejected alternatives —
mounds, ellipses, per-basin rim noise, and a basin authored on the `Water` entity. This note is
what building it taught.

**One field, and every consumer of the height field followed for free.** `Terrain.basins` is a list
of circular depressions in **world** XZ — `center`, `radius` (flat floor), `depth`, `falloff` (the
wall) — subtracted inside `terrain::height_at`. Because M22 kept exactly one height implementation,
adding the subtraction there gave the drawn surface, its normals, the `trimesh` collider, a `Road`
that follows the patch, a `Meadow` growing on it, `FootPlant`, `world.terrain_height` and
`engine terrain-height` all the same answer with no second call site touched. **No shader was
edited**, which is the strongest part of the result: the layer painting reads the world height and
slope the vertex already carries, so a basin's floor picks up a low-altitude layer and its wall
picks up a steep-slope layer with no new uniform, no new branch, and none of `mesh.wgsl`'s
ULP-sensitive lines in the blast radius.

- **The empty list is a branch back to M22's expression, not a subtraction of zero.** The two are
  numerically identical; the branch is so that a scene predating this milestone provably takes the
  code path its baseline was blessed under. `no_basins_is_the_untouched_field` compares bit
  patterns, and the A/B agreed: **38 of 38** comparable artifacts byte-identical between a `main`
  binary and this one. The seven excluded are the two scenes that author basins — the `main` binary
  rejects `basins` as an unknown field, so the tour's six frames and the new fixture have no base
  render to compare against. The tour is excluded on the stronger ground anyway: its *scene*
  changed, so the A/B would say nothing about it either way.
- **Overlapping basins take the deepest, never the sum.** Overlapping circles are how anything that
  is not a circle gets authored — the fixture's oblong pond is two of them — and a sum digs every
  overlap to twice the depth, which turns a lake into a ring of pits. The price is a gradient
  discontinuity where two basins are equally deep; it is under the water in every use this was
  built for, and the fixture shows it costs nothing visible.
- **`depth` is metres before `Transform.scale.y`**, exactly like `height`. A patch at `scale.y: 2`
  gets a basin twice as deep, because a basin is relief and `scale.y` multiplies relief. The
  alternative makes one component carry two vertical conventions.
- **The surface cache key had to grow.** `GridKey` names every field that changes the geometry;
  two patches differing only in their basins are different ground, and without the new entry the
  second one is handed the first one's hole out of the `Arc` cache. `basins_are_in_the_surface_cache_key`
  is the pin, and it is the failure that would have looked like a renderer bug.

## The trap this milestone is really about

**A water plane is a rectangle and a basin is a circle, and the rectangle is what you author.**
Every boundary point of a `Water` patch must land on ground *above* its own surface, or the sheet
ends in a straight cut hanging over the terrain — which is the M18 look this milestone exists to
fix. The constraint is on the rectangle's **edge midpoints**, not its corners: they are the
boundary points closest to the basin's centre, so they are where the wall has risen least.

The consequence is that **the water rectangle must be wider than the basin's shoreline, not
narrower** — the instinct is to shrink the sheet to fit the pool, and shrinking it is what exposes
the edge. In the tour: a basin of `radius 2.8 + falloff 3.2` (a 6.0 m footprint) under a 12.4 m
water patch, so the plane's nearest boundary point is 6.2 m out, past the wall, on untouched
ground. The shoreline the eye reads sits at ~4.8 m, where the wall climbs through the water level.

`engine terrain-height` on the rectangle's boundary is how to check this without rendering, and it
is worth doing: the clearance in the tour is **0.20 m at the worst corner**, which no render would
have told you was thin. A first pass at 0.12 m left less headroom than the pond's own wave
amplitude.

## Where the showcase pond went

Station 2's pond was a sheet on flat grass — ground at −0.24 m, water at −0.04 m, a 20 cm puddle
with a straight edge. It is now `basins: [{ center: [15.0, 5.5], radius: 2.8, depth: 1.7,
falloff: 3.2 }]` with the water at −0.80: a 1.13 m pool with a bank all the way round, the
waterfall plunging into it, and the raft floating where it always did (`Buoyancy` needed no
change — it reads the water, not the ground).

The basin centre is 0.5 m north of the pond's centre so the plunge point is properly in the water.
Two props moved with the waterline — `Spray` to −0.65 and `PondRaft`'s start to −0.72 — and
**nothing else in the station needed touching**: the ice shelf's base at −0.24 is still buried
(the wall has dropped the ground under its front face by 7 cm at that radius), and all three ice
blocks sit outside the 6.0 m footprint. The `RingRoad` passes 11.8 m from the basin centre, well
clear; had it passed closer, the road follows the terrain and would have dipped into the pond.

**Five of the six tour frames re-blessed, including `showcase_90` — the forest, at the other end of
the arena.** That is the rapier collider-set rule again (CLAUDE.md's trap): the terrain trimesh is
an input to the broad phase, so changing the ground under the pond moved bodies 40 m away by
sub-millimetres. The diff is in a place the change is not, and it is not a bug.

## Deliberately absent

Mounds (a negative `depth`), ellipses, rim noise, and a pond that digs its own hole — the last one
being M40's road-carving question with M40's answer, since `Terrain` owns its grid and a second
entity writing into it makes the ground a function of which other entities exist. What M42 does
establish is that an authored subtraction inside `height_at` is a workable form, which is one of
the two things carving would need.

A vertical wall (`falloff: 0`) is honest but **stair-steps on the grid** — the fixture's dry crater
is 4.4 m across on 0.25 m quads and the circle is visibly polygonal. That is the grid, not the
basin, and the fix is `segments`, not a smoother formula.

Verified by eight unit tests in `terrain.rs`, two validation tests, `verify/m42_basins.json` and
its bit-exact baseline (three renders, one image — `samples: 1` and no horizon in frame), and
`terrain_height_reports_the_basin_floor`, which pins the query rather than the picture.
