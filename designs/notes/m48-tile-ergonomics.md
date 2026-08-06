# Tile-synthesis ergonomics (M48)

*No design doc: three commands, each answering a question M47 made me answer by
hand. `designs/tile-synthesis-design.md` is still the design for the system
itself.*

M24's argument — **looking at a picture cannot answer where something is** —
applied to M47. Every item here comes from a specific moment of building the
M47 fixture where the engine held the answer and would not say it.

| Command | The moment |
|---|---|
| `engine tile-grid --at x,z` | Finding a flat cell to drop the ball on meant parsing the layout in Python. Twice, because the terrain changed under it. |
| `synthesize --at` / `--around` | Every region I wanted was somewhere I could point at; `--region`'s cell indices were arithmetic I did by hand. |
| `list-tiles --sheet` | The village tileset took four passes over its own render, and three of them would have been visible in a contact sheet. |

## What was substituted, and why

The first plan for this milestone had **per-block geometry caching** in it, on
the claim that a one-cell edit re-uploads the whole village every frame. That
claim was false and checking it took ten minutes:

- there is **no frustum culling** in the renderer — every `cull` in
  `scene_renderer` is backface culling;
- **picking is AABB-per-`RenderItem` returning the entity name**
  (`engine-editor/src/pick.rs`), and a grid's items all share one name, so
  splitting them changes nothing an author can select;
- geometry is grown **once** in `Scene::from_source`, not per frame, and the
  `Arc` is stable for the scene's lifetime.

So the split would have bought a slower reload path and more draw calls. It is
worth writing down because the reasoning *sounds* right — merged meshes really
are a granularity loss — and the thing that made it wrong is that the two
consumers which would care do not exist yet.

## `surface_y`, and why it is the point

`tile-grid --at` reports the height a column's geometry reaches, by growing that
one tile through the same `grow_tile` the draw list goes through. It is checked
against physics rather than against itself: the M47 ball rests at `surface_y`
plus its radius, to within a millimetre.

That millimetre is rapier's allowed contact penetration, not an error — a
resting body sits slightly *into* its contact. A test asserting exactness here
fails, and the failure looks like a bug in the query.

## Traps

- **Resolve a relative asset reference only after creating the directory it is
  relative to.** `--sheet` computed the tileset's path relative to a directory
  that did not exist yet, `canonicalize` failed, and the fallback resolved
  against the working directory instead — writing a sheet that named a tileset
  which was not there. The fallback is gone; an empty base is the working
  directory, and a missing one is an error.
- **A contact sheet needs gaps.** Packed edge to edge, ten tiles of walls read
  as one maze. The spacer is the tileset's own zero-part tile when it has one,
  which is also the only spacer that cannot introduce a shape the author did not
  author.
- **Frame from a distance and an angle, not from a position and a fitted
  pitch.** The first version placed the camera and derived the pitch to match,
  which framed the sheet at twice the distance its width needed.
- **Tiles across, rotations down.** The transpose is deeper than it is wide, and
  its rows run away from the camera and merge into a single mass.

## Verification

Nine CLI tests. No fixture and no baseline: M48 adds no component and touches no
geometry, so the sweep stays at 53 artifacts and nothing re-blesses. That is
also why no A/B is owed — the render path is not in the diff.
