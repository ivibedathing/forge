# Road design (M23)

Circuits, streets and mountain passes: what a road *is* in a scene file, how it becomes a
continuous drivable surface, and how its markings learn the shape of the track.

The starting point was `examples/scenes/car_track.json`, and it is worth being precise about what
was wrong with it, because every decision below is aimed at one of these.

The circuit was **207 `builtin:cube` plates**. Each segment of centerline emitted a deep earth
box, cut to that segment's grade, whose top face was the drivable surface; a thin colliderless
asphalt slab was laid 9 cm proud of it so the road read as asphalt rather than as dirt; kerbs were
more cubes on top of that; and the start line was one more. Consecutive rectangles cannot tile a
curve, so every corner joint left a wedge of verge showing through, papered over by overlapping
the slabs 1.1 m (`ASPHALT_OVERLAP`) and lifting them clear of their neighbours' boxes (`SKIN`).
Both constants exist only to hide the fact that the road is not a surface. Worse, the *drivable*
top faces of two adjacent boxes meet at an angle with a discontinuity in the normal at every
joint, and the emitter had to overlap the plates by 35 cm so a suspension ray could not drop
through the crack between them.

And there were no markings at all beyond the kerbs and one painted start line. There could not
really be: a marking on a plate road is more plates, sized and rotated per segment, z-fighting
with the asphalt they sit on.

## 1. A road is one entity with one component

```json
{ "name": "Circuit", "components": [
  { "type": "Road",
    "closed": true,
    "width": 7.0,
    "shoulder": 1.6,
    "points": [
      { "position": [-72.0, 5.6, -52.0], "radius": 14.0 },
      { "position": [-34.0, 5.2, -52.0], "radius": 14.0 },
      …
    ],
    "markings": { "edge_width": 0.14, "kerb_max_radius": 16.0, "start_line": true } }
]}
```

`Road` owns its surface geometry, exactly as `Water` does (M18): the component generates the mesh,
so the entity carries **no** `Mesh` and **no** `Material`, and having either is `road_with_mesh`.
One road, one source of truth (invariant 2).

**The centerline is authored as a polygon with corner radii, not as a spline.** This is inherited
from `make_car_track.py`, where it earned its place: a closed polygon returns to its own first
vertex by construction and its exterior angles sum to exactly one turn, so position *and* heading
close without solving anything. Nothing in the file carries a heading — a heading is a derived
quantity, and a format that stores one lets an edit make it disagree with the positions on either
side of it. A corner is `(position, radius)`; `radius: 0` is a sharp vertex, mitred — which is how a road
carries a point that is not really a turn.

Rejected alternatives:

- **A spline through the points** (Catmull-Rom in plan view). Smooth everywhere, but a constant
  radius through a corner is what a race track actually is, and a spline gives no radius to check
  against `MIN_RADIUS`-style rules or to hang kerbs off. The `radius` field is load-bearing
  downstream, not decoration.
- **A pre-sampled centerline** (`[[x, y, z, heading, width], …]`) written by a generator. That is
  a mesh in JSON: unreadable, uneditable by hand, and it puts heading in the file.
- **A scene-level `road` block.** A scene can hold a circuit and a pit lane, so this is per entity.

Elevation rides on the points (`position.y`) and is interpolated **by arc length, with a monotone
cubic (Fritsch–Carlson) through the corner heights** — not linearly, and not with a plain
Catmull-Rom. Linear ramps between corners put a discontinuity in the *grade* at every corner, a
kink the car feels as a bump at exactly the moment it is loaded up mid-corner. Catmull-Rom removes
the kink and introduces a worse problem: it overshoots, so a road authored to reach 6 m crests at
6.4 and the file stops predicting the scene. Monotone cubic is smooth and provably stays inside the
authored range.

## 2. The cross-section, and why the collider is one surface

Each sample along the centerline emits a cross-section, and consecutive cross-sections are
stitched into a ribbon:

```
        u = 0
          │
  ────────┼────────      asphalt, `width` wide
 /        │        \     shoulder, `shoulder` wide each side
│                   │    skirt, dropping `skirt` metres
```

The skirt is what makes an elevated road sit on the ground rather than float over it; it drops
below the ground plane at the low points, where it is invisible, and stands as an embankment wall
at the high ones, which is what an elevated section looks like.

**One collider, and it is the whole ribbon.** This is the same lesson the plate road learned the
hard way and stated in `make_car_track.py`: road and shoulder as two colliders at different
heights builds a ledge at the asphalt edge, and a wheel that drops off it wedges against the step
and stops the car dead. Here the rule is structural rather than remembered — asphalt and shoulder
are the same triangles, so there is no edge between them to catch on, and there is no seam between
segments because consecutive cross-sections share their vertices.

Geometry comes from the component, so a `Collider` on a road entity needs no `asset` and no
`Mesh`:

```json
{ "type": "Collider", "shape": "trimesh", "friction": 0.9 }
```

The collider is still an ordinary component, because friction, restitution and collision layers are
ordinary data and a road that owned them would be a second place to look. It is built with parry's
`FIX_INTERNAL_EDGES`, which is what stops a body resting on the ribbon from eventually catching an
edge between two coplanar triangles and being flung sideways — and it is applied to road geometry
only, because switching it on for every trimesh in the engine moves an existing terrain baseline. `trimesh` on a **fixed**
body is the supported case (M12's `trimesh_on_dynamic_body` still applies — a road is not a falling
object).

**Vertex normals are averaged along the road and flat across it**, so the surface shades as one
continuous ribbon. The collider keeps the faceted triangles, which is correct: the shading normal
is a lie told to the eye, and the wheel should ride on the geometry that is actually there.

## 3. Markings are drawn, not built

Every marking — edge lines, centre line, kerbs, the start line — is computed per pixel in
`road.wgsl` from two numbers interpolated across the surface:

- **`u`**: signed distance from the centerline, measured *along the cross-section*, in metres.
  Positive to the driver's right. Because it is cross-section arc length rather than a lateral
  offset, `|u| > width/2 + shoulder` is exactly "on the skirt", whatever the profile does.
- **`v`**: distance travelled along the centerline, in metres.

They ride in the mesh's UVs, which the renderer had never uploaded before (M15's cache packs
positions and normals only) and now does.

This is the decision that answers "markings adjusted to the track shape". A painted line is a band
in `u`, so it follows every curve and every grade automatically and can never z-fight, because it
is not a surface sitting on another surface — it is the same pixel, shaded differently. Dashes are
periodic in `v`, so a dash is the same length in metres on a straight and through a hairpin,
where geometry stretched around the outside of a corner would smear it.

Two things the CPU decides and hands to the shader, because per-pixel code cannot know them:

- **Kerb spans.** The generator knows each corner's radius and its `v` range, so corners tighter
  than `markings.kerb_max_radius` contribute a span `(v_start, v_end, side, stripe)` to a
  fixed-size uniform array (`MAX_ROAD_KERBS`). The side is the *inside* of the turn, which is where
  a kerb belongs and which only the plan-view geometry knows.
- **Period fitting.** On a closed road the dash period is snapped to `total / round(total / period)`
  and each kerb's stripe width to `span / round(span / stripe)`, so the pattern closes on itself
  instead of leaving a short dash at the seam. This is markings adjusting to the track in the most
  literal sense.
- **Where the start line goes.** `markings.start_line_at` is an arc length, not "wherever the
  polygon begins". The obvious alternative — split the straight with a `radius: 0` point so the road
  *starts* at the line — is what the car demo tried first, and the circuit refused it: La Source is
  a 110° turn on a 14 m radius, so its arc reaches 20 m back down a 34 m straight and a sharp vertex
  19 m along would sit inside the arc. `road_corner_does_not_fit`, correctly. A polygon's first
  point is a corner, chosen by the shape; a start line belongs partway down a straight; they are
  different jobs and now have different fields.

Anti-aliasing is `fwidth` on the marking coordinate, clamped: a road seen at a grazing angle from
200 m has enormous derivatives, and unclamped the paint dissolves into grey.

**Kerbs are painted, not raised.** A real kerb is a step, and a step is exactly the thing §2 says
must not exist on the drivable surface. A painted rumble strip reads as a kerb at any distance the
camera will ever see one from and costs the car nothing.

## 4. The shader duplicates the mesh shader's lighting, deliberately

`road.wgsl` re-derives the GGX/shadow/fog/sky code that `mesh.wgsl` already contains, following the
precedent `water.wgsl` set and for the same reason: M16 established that the four lines computing
`direct`/`ambient`/`base_color` in `mesh.wgsl` are pinned byte-for-byte against committed
baselines, and that *restructuring arithmetic which is equal on paper* has already moved a baseline
by one ULP, because FMA contraction depends on surrounding code. Sharing a function between the two
shaders means editing those lines. Only `sky_common.wgsl` is shared — prepended at pipeline build —
so the sky a road reflects cannot drift from the sky drawn behind it.

## 5. Everything is off unless a scene has a road

A scene with no `Road` component uploads no road uniforms, builds no road draws, and issues the
exact pass structure it did before this milestone. That is the same contract M16, M17 and M18 kept,
and it is why the eighteen committed baselines did not move.

## 6. Not here, deliberately

- **Banking.** A `bank` on each point would rotate the cross-section about the tangent; it is
  cheap in the geometry and not cheap in the elevation profile, the collider, or the kerb-side
  logic. Deferred until a scene wants a banked oval.
- **Junctions.** Two roads crossing is two entities and a visible seam. Intersections need a
  different primitive (a patch, not a ribbon) and should not be smuggled in as a special case here.
- **Width variation.** One `width` per road. A pit lane merging out of a straight wants a per-point
  width; it is a small change to the sampler when something needs it.
- **Textures.** The engine has no texture-mapped materials yet (`engine-assets` loads PNGs and
  nothing consumes them). Analytic markings are not a stopgap for that — they are better for
  anything periodic — but asphalt grain will want a texture eventually.
- **A CPU query for road height.** The mirror of water's missing wave evaluator: nothing yet needs
  "how high is the road at (x, z)" outside the physics engine, which has the trimesh.
