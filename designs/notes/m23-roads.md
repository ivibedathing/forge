# Roads (M23)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Roads.*

The car demo's circuit was **207 `builtin:cube` plates** whose overlapping slabs and constants existed
only to hide the fact that the road was not a surface. `Road` replaces all of it with one entity, and
the entity carries **no** `Mesh` and **no** `Material` (`road_with_mesh`).

The centerline is authored as a **polygon with corner radii** (`points`, each a `position` and a
`radius`), not a spline: a closed polygon returns to its own first vertex and its exterior angles sum
to one turn, so position *and* heading close without solving anything, and nothing in the file
carries a heading. `radius: 0` is a sharp vertex, mitred with the standard `1 / cos(turn/2)`
widening; past `MAX_SHARP_TURN_DEGREES` the mitre folds and validation says so
(`road_corner_needs_radius`), as it does when two arcs need more of the edge between them than it has
(`road_corner_does_not_fit`). Elevation rides on the points and is interpolated by arc length with a
**monotone cubic** (Fritsch–Carlson), not linearly and not Catmull-Rom: linear ramps break the grade
at every corner — a bump the car feels exactly where it is loaded up — and plain Catmull-Rom
overshoots, so a road authored to reach 6 m crests at 6.4 and the file stops predicting the scene.

- **One collider, and it is the whole ribbon.** Asphalt, shoulders and the embankment skirt are the
  same triangles, so the ledge that stopped the car dead on the plate road is now structurally
  impossible. A `Collider` with `"shape": "trimesh"` on a road entity needs no `asset` and no `Mesh`
  — the road *is* the geometry — while friction and layers stay on the `Collider`.
- **`FIX_INTERNAL_EDGES` on a road's own trimesh, and only there.** Without it a body resting on a
  triangle mesh eventually contacts an edge *between* two coplanar triangles, takes a contact normal
  along that edge instead of off the surface, and is flung sideways: a ball parked on the M23 fixture
  sat still for two seconds and then left the road at 4.8 m/s. Switching it on for *every* trimesh
  moves `verify/baselines/m22_terrain.png` by 1339 pixels. **Terrain has the same latent bug** and
  should take the same flag as its own change, with its own re-blessed baseline.
- **Markings are drawn, not built.** Every marking is computed per pixel in `shaders/road.wgsl` from
  two surface coordinates the vertex stage carries in the mesh's UVs (which the renderer had never
  uploaded before and now does for every mesh): `u`, signed metres from the centerline *along the
  cross-section*, so `|u| > width/2 + shoulder` is exactly "on the skirt"; and `v`, metres along the
  centerline. A line is a band in `u`, so it follows every curve and grade for free; a dash is
  periodic in `v`, so it is the same length in metres through a hairpin as on a straight. Paint
  cannot z-fight, because it is the same pixel shaded differently. Anti-aliasing is a **clamped**
  `fwidth`; unclamped, a road seen at a grazing angle from 200 m dissolves into grey.
- **Two things the CPU decides**, because per-pixel code cannot: **kerb spans** (which corners are
  under `markings.kerb_max_radius`, and which side is the *inside*) ride in a fixed-size uniform
  array, `MAX_ROAD_KERBS` of them, beyond which `too_many_road_kerbs`; and **period fitting** — on a
  closed road the dash period is snapped to `total / round(total / period)` and each kerb's stripe to
  its span, so patterns close on themselves. Kerbs are *painted*, not raised: a real kerb is a step,
  and a step is the thing the whole design says must not exist on the drivable surface.
- **`markings.start_line_at`** places the start line by arc length rather than at `v = 0`. The obvious
  alternative — split the straight with a radius-0 point — fails on the demo circuit: La Source is a
  110° turn on a 14 m radius, its arc reaches 20 m back down a 34 m straight, and a sharp vertex 19 m
  along would sit inside it. The road refuses that, correctly.
- **`road.wgsl` duplicates `mesh.wgsl`'s lighting**, following the `water.wgsl` precedent and for the
  same reason; only `sky_common.wgsl` is prepended.
- **`engine road-centerline`** publishes the samples the ribbon was built from — world position,
  heading and `v` per point. Anything placed *along* a road needs them, and a generator re-deriving
  them is how two implementations of one curve start disagreeing about where the road is.
  `make_car_track.py` is the worked example: write the road, ask the engine where it went, write the
  scene again with the guardrail and the car on it.
- **Roads draw last in the opaque pass**, after the terrain run M22 moved to the front. That puts a
  road where M22 measured this adapter to be unreliable, so it was checked rather than assumed: five
  consecutive sweeps of the six tour frames came back with **zero** differing pixels every time,
  `showcase_646` included. A road ribbon is a few thousand triangles against terrain's 200k; if a
  future road scene starts flaking, M22's fix is to give the pass something to draw afterwards.

Fixture `verify/m23_road.json` at `--steps 180`, pinned by a CLI test that also drops a ball on the
road and requires it to *stay where it lands*.
