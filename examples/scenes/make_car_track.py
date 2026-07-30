"""Emit examples/scenes/car_track.json: a Spa-Francorchamps in miniature.

The circuit is authored as a closed polygon: one CORNER per turn, each holding
a plan-view position, a corner radius, and a height above the ground plane.
Since M23 that polygon *is* the scene's `Road` component — the engine rounds
the corners, sweeps the cross-section, paints the markings and hands physics
the same triangles it draws. This script's remaining job is to say where the
corners are, and to place everything that stands beside the road.

Authoring the loop as a polygon is what makes it a circuit rather than a
ribbon. A closed polygon returns to its own first vertex by construction, and
its exterior angles sum to exactly one turn, so position and heading close
without solving anything; the elevation profile closes for the same reason,
being heights carried on the vertices themselves.

What this script no longer does, and it is worth knowing that it once did: it
emitted 207 `builtin:cube` plates — a deep earth box per segment whose top
face was the drivable surface, a thin colliderless asphalt slab 9 cm proud of
it, a kerb cube on the inside of the tight corners, and a painted start line.
Two constants existed only to hide the seams that arrangement left (`SKIN`,
`ASPHALT_OVERLAP`), the plates had to overlap by 35 cm so a suspension ray
could not drop through the crack between two of them, and markings beyond the
kerbs were not really possible. One `Road` component replaces all of it.

The layout follows Spa's sequence at roughly 1/15 scale: La Source, the plunge
to Eau Rouge, the Raidillon climb onto the Kemmel straight, Les Combes at the
high point, Rivage and the descent through Pouhon, Fagnes, Stavelot, the long
Blanchimont run back uphill, and the Bus Stop chicane onto the start line.

Usage:
    python3 examples/scenes/make_car_track.py [--centerline path.json]

The barriers are placed along the road's *own* sampled centerline, which the
engine publishes with `engine road-centerline` — so this script writes the
scene, asks the engine where the road actually went, and writes it again with
the guardrail in place. Re-deriving those samples here is how two
implementations of one curve start disagreeing about where the road is.

`--centerline` dumps that same centerline beside the scene: the autopilot that
authors car_track_lap.input.jsonl steers along it, and it is regenerated
rather than committed.
"""

import argparse
import json
import math
import os
import shlex
import subprocess
import sys

# ---------------------------------------------------------------------------
# Track description
# ---------------------------------------------------------------------------

ROAD_WIDTH = 7.0        # meters of asphalt, edge to edge
VERGE = 1.6             # meters of drivable shoulder each side of the asphalt
# How far the embankment reaches below the road surface. Deeper than the
# circuit ever climbs, so at every low point it is simply buried.
SKIRT = 12.0

# The circuit, one corner per entry, in racing order. Positions are plan-view
# meters (x east, z south); `radius` rounds the corner; `height` is the road
# surface above the ground plane there, and the grade between corners follows.
#
# The loop runs clockwise seen from above, so most corners turn right; the
# concave ones (Eau Rouge, the second half of each chicane) turn left, and the
# turn angle is whatever the polygon makes it — nothing here is a heading.
#
# Radii are sized for the car, not for the map. This is Spa shrunk to about a
# fifteenth, but the car driving it is full size, so corners scaled down with
# the rest of the layout would be too tight to get round at all. Nothing here
# is under MIN_RADIUS, and the hairpin is a wide one.
CORNERS = [
    # name             x      z   radius  height
    ("la_source_a",  -72.0, -52.0,  14.0,   5.6),  # the hairpin, both halves:
    ("la_source_b",  -34.0, -52.0,  14.0,   5.2),  # right, right, and you come
                                                   # out pointed down the hill
    ("eau_rouge",    -24.0,  12.0,  18.0,   0.8),  # the bottom of the plunge
    ("raidillon",     26.0,   6.0,  22.0,   4.0),  # the flick at the top of it
    ("les_combes_a",  76.0, -34.0,  16.0,   8.4),  # the crest, end of Kemmel
    ("les_combes_b",  92.0,  -6.0,  16.0,   7.8),
    ("rivage",        96.0,  34.0,  14.0,   5.4),  # slow right, dropping away
    ("pouhon",        44.0,  66.0,  26.0,   2.8),  # long downhill sweeper
    ("fagnes_a",       2.0,  52.0,  14.0,   2.0),
    ("fagnes_b",     -24.0,  68.0,  14.0,   1.4),
    ("stavelot",     -64.0,  52.0,  16.0,   0.8),  # the low point
    ("blanchimont",  -84.0,  12.0,  32.0,   3.0),  # fast, climbing all the way
    ("bus_stop_a",   -78.0, -10.0,  15.0,   4.2),  # the chicane onto the line
    ("bus_stop_b",   -60.0, -20.0,  15.0,   4.6),
]

MIN_RADIUS = 12.0       # below this the car cannot make the turn at all
MAX_GRADE = 0.16        # steeper than this and the car cannot climb it

# Where the start line sits: this far along the edge that runs from the last
# corner to the first — the pit straight, climbing to La Source.
#
# It is a *position*, not a polygon vertex. Splitting the straight with an
# extra corner to put `v = 0` there is the obvious move and it does not work:
# La Source is a 110-degree turn on a 14 m radius, so its arc reaches 20 m back
# down a 34 m straight, and a sharp vertex 19 m along would sit inside it. The
# road refuses that (`road_corner_does_not_fit`), correctly. So the line is
# placed by arc length instead, which is what `markings.start_line_at` is for
# — and the arc length is whatever the engine says it is.
START_EDGE_FRACTION = 0.55

# How finely the engine cuts the centerline. Straights every couple of meters
# and corners every few degrees is smooth to drive and to look at, and the cost
# is linear in the circuit's length rather than quadratic like a grid's.
SEGMENT_LENGTH = 2.5
SEGMENT_ANGLE = 3.0

# Kerbs go on the inside of corners this tight. The engine picks which corners
# those are, and which side of each is the inside.
KERB_MAX_RADIUS = 16.0

# Barriers are a continuous guardrail, not a dashed line: each post is a
# little longer than the gap it is spaced by, so a car sliding off cannot slip
# between two of them and fall off the elevated road.
BARRIER_SPACING = 5.0   # meters between barrier posts, per side
BARRIER_LENGTH = 5.4

# Materials, all linear RGB.
ASPHALT = [0.09, 0.09, 0.10]
PAINT = [0.90, 0.90, 0.88]
KERB_RED = [0.80, 0.10, 0.08]
BARRIER_RED = [0.85, 0.08, 0.06]
EARTH = [0.20, 0.17, 0.13]
VERGE_COLOR = [0.18, 0.19, 0.13]
GRASS = [0.16, 0.22, 0.14]


# ---------------------------------------------------------------------------
# Geometry
# ---------------------------------------------------------------------------

def heading_vec(yaw_deg):
    """World XZ direction an entity with this Y rotation faces (its local -Z)."""
    t = math.radians(yaw_deg)
    return (-math.sin(t), -math.cos(t))


def right_vec(yaw_deg):
    """The direction to the driver's right: forward x up."""
    dx, dz = heading_vec(yaw_deg)
    return (-dz, dx)


def yaw_of(direction):
    """The Y rotation whose local -Z points along this XZ direction."""
    return math.degrees(math.atan2(-direction[0], -direction[1]))


def start_line_position():
    """Where the start line goes, in plan view: partway up the pit straight."""
    first, last = CORNERS[0], CORNERS[-1]
    f = START_EDGE_FRACTION
    return (
        last[1] + (first[1] - last[1]) * f,
        last[2] + (first[2] - last[2]) * f,
    )


def start_line_v(centerline):
    """The arc length of the centerline sample nearest that position.

    The engine's own samples, so the line lands on the road rather than near
    it, and the car parked on the line is parked on the line.
    """
    target = start_line_position()
    nearest = min(centerline, key=lambda p: math.dist((p[0], p[2]), target))
    return nearest[5], nearest


def check(centerline):
    """Everything neither the polygon nor the engine can guarantee.

    The engine refuses a corner whose radius does not fit the edges feeding it.
    What it has no opinion about is whether a *car* can drive the result, which
    is what these two are for.
    """
    problems = []
    for corner in CORNERS:
        if corner[3] < MIN_RADIUS:
            problems.append(f"{corner[0]}: radius {corner[3]:.1f}m is below the "
                            f"{MIN_RADIUS:.0f}m the car needs")

    steepest = 0.0
    for a, b in zip(centerline, centerline[1:]):
        run = math.dist((a[0], a[2]), (b[0], b[2]))
        if run > 0.1:
            steepest = max(steepest, abs(b[1] - a[1]) / run)
    if steepest > MAX_GRADE:
        problems.append(f"steepest grade {steepest * 100:.0f}% is too steep to climb")
    return problems


# ---------------------------------------------------------------------------
# Scene emission
# ---------------------------------------------------------------------------

def transform(position, rotation, scale):
    return {
        "type": "Transform",
        "position": [round(v, 4) for v in position],
        "rotation": [round(v, 4) for v in rotation],
        "scale": [round(v, 4) for v in scale],
    }


def material(albedo, roughness=0.9, metallic=0.0):
    return {
        "type": "Material",
        "albedo": [round(c, 4) for c in albedo],
        "metallic": metallic,
        "roughness": roughness,
    }


def box_collider(friction, restitution=None):
    collider = {
        "type": "Collider",
        "shape": "cuboid",
        "half_extents": [0.5, 0.5, 0.5],
        "friction": friction,
    }
    if restitution is not None:
        collider["restitution"] = restitution
    return collider


def entity(name, components):
    return {"name": name, "components": components}


def emit_road(start_at=0.0):
    """The circuit itself: one entity, one component, one collider.

    Asphalt, verge and embankment are the same triangles, which is not a saving
    but the point — road and shoulder as two surfaces at different heights
    build a ledge along the asphalt edge, and a wheel that drops off it wedges
    against the step and stops the car dead. That was the plate road's worst
    failure mode and it is now structurally impossible.

    No centre line: a race circuit has none. Edge lines, kerbs on the tight
    corners, and a start line across `v = 0` are what it does have.
    """
    return entity("Circuit", [
        transform((0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (1.0, 1.0, 1.0)),
        {
            "type": "Road",
            "closed": True,
            "width": ROAD_WIDTH,
            "shoulder": VERGE,
            "skirt": SKIRT,
            "segment_length": SEGMENT_LENGTH,
            "segment_angle": SEGMENT_ANGLE,
            "color": ASPHALT,
            "roughness": 0.95,
            "shoulder_color": VERGE_COLOR,
            "bank_color": EARTH,
            "points": [
                {"position": [round(c[1], 4), round(c[4], 4), round(c[2], 4)],
                 "radius": c[3]}
                for c in CORNERS
            ],
            "markings": {
                "color": PAINT,
                "edge_width": 0.15,
                "edge_inset": 0.10,
                "kerb_max_radius": KERB_MAX_RADIUS,
                "kerb_width": 0.9,
                "kerb_stripe": 1.4,
                "kerb_color": KERB_RED,
                "start_line": True,
                "start_line_at": round(start_at, 4),
                "start_line_width": 0.7,
            },
        },
        # The road *is* the collision geometry: no asset, no Mesh, no second
        # surface to disagree with the one on screen.
        {"type": "Collider", "shape": "trimesh", "friction": 0.85},
    ])


def emit_barriers(centerline):
    """Posts at a fixed spacing down both sides, from the road's own samples."""
    entities = []
    debt = 0.0
    index = 0
    reach = ROAD_WIDTH / 2.0 + VERGE - 0.3

    for a, b in zip(centerline, centerline[1:]):
        debt += math.dist((a[0], a[2]), (b[0], b[2]))
        while debt >= BARRIER_SPACING:
            debt -= BARRIER_SPACING
            yaw = yaw_of((a[3], a[4]))
            rx, rz = right_vec(yaw)
            for side, tag in ((1.0, "R"), (-1.0, "L")):
                entities.append(entity(f"Barrier{tag}{index:03d}", [
                    transform(
                        (a[0] + rx * reach * side, a[1] + 0.28, a[2] + rz * reach * side),
                        (0.0, yaw, 0.0),
                        (0.45, 0.6, BARRIER_LENGTH),
                    ),
                    {"type": "Mesh", "asset": "builtin:cube"},
                    material(BARRIER_RED if index % 2 == 0 else PAINT, roughness=0.7),
                    # Slippery on purpose. A guardrail a car can *stick* to
                    # ends the drive: nose it at walking pace and no amount of
                    # throttle frees it again.
                    box_collider(0.05, 0.1),
                ]))
            index += 1
    return entities


def emit_ground(centerline):
    xs = [p[0] for p in centerline]
    zs = [p[2] for p in centerline]
    margin = 30.0
    size_x = (max(xs) - min(xs)) + margin * 2
    size_z = (max(zs) - min(zs)) + margin * 2
    center_x = (max(xs) + min(xs)) / 2.0
    center_z = (max(zs) + min(zs)) / 2.0
    ground = entity("Ground", [
        transform((center_x, 0.0, center_z), (0.0, 0.0, 0.0), (size_x, 1.0, size_z)),
        {"type": "Mesh", "asset": "builtin:plane"},
        material(GRASS, roughness=0.95),
        {
            "type": "Collider",
            "shape": "cuboid",
            "half_extents": [0.5, 0.5, 0.5],
            "offset": [0.0, -0.5, 0.0],
            "friction": 0.6,
        },
    ])
    return ground, (center_x, center_z, size_x, size_z)


def emit_car(start):
    """The chassis, its four wheels, and the driver script — unchanged physics.

    Everything is placed relative to the start line, facing down the straight.
    """
    x, y, z, yaw = start
    dx, dz = heading_vec(yaw)
    rx, rz = right_vec(yaw)

    def place(lateral, longitudinal, height):
        return (
            x + rx * lateral + dx * longitudinal,
            y + height,
            z + rz * lateral + dz * longitudinal,
        )

    entities = [entity("Car", [
        transform(place(0.0, 0.0, 0.9), (0.0, yaw, 0.0), (1.7, 0.7, 3.6)),
        {"type": "RigidBody", "body": "dynamic", "ccd": True},
        {
            "type": "Collider",
            "shape": "cuboid",
            "half_extents": [0.5, 0.5, 0.5],
            "density": 350.0,
            "friction": 0.3,
            "restitution": 0.1,
        },
        {"type": "Mesh", "asset": "builtin:cube"},
        material([0.72, 0.15, 0.12], roughness=0.55, metallic=0.1),
        {"type": "Script", "source": "scripts/car.rhai"},
    ])]

    wheels = [
        ("WheelFL", -0.92, 1.25, 2.0),
        ("WheelFR", 0.92, 1.25, 2.0),
        ("WheelRL", -0.92, -1.25, 1.7),
        ("WheelRR", 0.92, -1.25, 1.7),
    ]
    for name, lateral, longitudinal, side_friction in wheels:
        entities.append(entity(name, [
            transform(
                place(lateral, longitudinal, 0.35),
                (0.0, yaw, 90.0),
                (0.7, 0.26, 0.7),
            ),
            {"type": "Mesh", "asset": "builtin:cylinder"},
            material([0.05, 0.05, 0.06], roughness=0.9),
            {
                "type": "Wheel",
                "vehicle": "Car",
                # Chassis-local: +X right, -Z forward.
                "offset": [lateral, -0.2, -longitudinal],
                "radius": 0.35,
                "suspension_rest_length": 0.35,
                "suspension_stiffness": 30.0,
                "suspension_compression": 2.8,
                "suspension_damping": 3.8,
                "suspension_travel": 0.2,
                "side_friction_stiffness": side_friction,
                "friction_slip": 1.0,
            },
        ]))

    # The tailpipe emitter; car.rhai parks it behind the rear bumper each step,
    # so this placement only has to be right for the scene at rest.
    entities.append(entity("Exhaust", [
        transform(place(0.55, -2.05, -0.5), (90.0, 0.0, 0.0), (1.0, 1.0, 1.0)),
        {
            "type": "ParticleEmitter",
            "rate": 55.0,
            "lifetime": 1.4,
            "speed": 1.0,
            "spread": 16.0,
            "acceleration": [0.0, 1.1, 0.0],
            "drag": 1.2,
            "start_size": 0.09,
            "end_size": 0.4,
            "start_color": [0.3, 0.3, 0.32],
            "end_color": [0.55, 0.55, 0.58],
            "start_alpha": 0.75,
            "end_alpha": 0.0,
            "max_particles": 128,
            "seed": 20,
        },
    ]))

    # Tire smoke at each rear contact patch. car.rhai runs these at rate 0
    # until the rear end is actually sliding, so they cost nothing at rest.
    for name, lateral, seed in (("SkidLeft", -0.92, 31), ("SkidRight", 0.92, 32)):
        entities.append(entity(name, [
            transform(place(lateral, -1.25, -0.78), (90.0, 0.0, 0.0), (1.0, 1.0, 1.0)),
            {
                "type": "ParticleEmitter",
                "rate": 0.0,
                "lifetime": 0.9,
                "speed": 0.8,
                "spread": 55.0,
                "acceleration": [0.0, 0.6, 0.0],
                "drag": 2.0,
                "start_size": 0.1,
                "end_size": 0.5,
                "start_color": [0.62, 0.62, 0.62],
                "end_color": [0.78, 0.78, 0.78],
                "start_alpha": 0.5,
                "end_alpha": 0.0,
                "max_particles": 160,
                "seed": seed,
            },
        ]))
    return entities


def emit_furniture(bounds, start):
    center_x, center_z, size_x, size_z = bounds
    x, y, z, yaw = start
    # The top-down camera has to hold the whole circuit: pull back until the
    # 50 degree vertical field covers the longer axis with room to spare.
    span = max(size_x, size_z)
    height = span / (2.0 * math.tan(math.radians(25.0))) * 1.05
    dx, dz = heading_vec(yaw)
    return [
        entity("Sun", [
            transform((0.0, 0.0, 0.0), (-49.13, 28.48, 0.0), (1.0, 1.0, 1.0)),
            {"type": "DirectionalLight", "color": [1.0, 1.0, 1.0], "intensity": 1.66},
        ]),
        entity("Ambient", [
            {"type": "AmbientLight", "color": [1.0, 1.0, 1.0], "intensity": 0.06},
        ]),
        entity("ChaseCam", [
            transform(
                (x - dx * 8.0, y + 2.3, z - dz * 8.0),
                (-14.0, yaw, 0.0),
                (1.0, 1.0, 1.0),
            ),
            {"type": "Camera", "fov": 55.0, "near": 0.1, "far": 400.0, "active": True},
        ]),
        entity("TopCam", [
            transform(
                (center_x, round(height, 2), center_z),
                (-90.0, 0.0, 0.0),
                (1.0, 1.0, 1.0),
            ),
            # Near plane pulled right up to the track. From 200 m up, a 0.1 m
            # near plane spends all its depth precision on empty air.
            {"type": "Camera", "fov": 50.0, "near": round(height * 0.5, 2),
             "far": 400.0, "active": False},
        ]),
        entity("SpeedBarBack", [{
            "type": "HudRect",
            "anchor": "bottom_left",
            "offset": [16.0, 16.0],
            "size": [260.0, 12.0],
            "color": [0.05, 0.06, 0.08],
            "opacity": 0.75,
        }]),
        entity("SpeedBar", [{
            "type": "HudRect",
            "anchor": "bottom_left",
            "offset": [16.0, 16.0],
            "size": [0.0, 12.0],
            "color": [0.2, 0.9, 0.3],
        }]),
    ]


def write_scene(path, entities):
    scene = {
        "name": "car_track",
        "entities": entities,
        "physics": {"gravity": [0.0, -9.81, 0.0], "timestep_hz": 60},
    }
    with open(path, "w") as handle:
        json.dump(scene, handle, indent=2)
        handle.write("\n")


def ask_engine_for_centerline(engine, scene_path):
    """The road's own samples: (x, y, z, forward_x, forward_z, v) per point.

    Asking rather than re-deriving. The engine rounded these corners and swept
    this cross-section; a second implementation here would agree with it right
    up until one of the two changed.
    """
    command = shlex.split(engine) + ["road-centerline", scene_path]
    result = subprocess.run(command, capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit(f"`{' '.join(command)}` failed:\n{result.stderr.strip()}")
    data = json.loads(result.stdout)
    return data, [
        (p["position"][0], p["position"][1], p["position"][2],
         p["forward"][0], p["forward"][1], p["v"])
        for p in data["points"]
    ]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", default="examples/scenes/car_track.json")
    parser.add_argument("--centerline")
    parser.add_argument(
        "--engine",
        default=os.environ.get("ENGINE", "cargo run -q -p engine-cli --"),
        help="how to invoke the engine CLI (it is asked where the road went)",
    )
    args = parser.parse_args()

    # Pass one: the road alone, which is all the engine needs to answer.
    write_scene(args.out, [emit_road()])

    # Pass two: furnish the centerline the engine actually built. The start
    # line and the car both go where the road passes the pit straight, which
    # is a question only the finished centerline can answer.
    summary, centerline = ask_engine_for_centerline(args.engine, args.out)
    line_v, line_point = start_line_v(centerline)
    road = emit_road(line_v)
    start = (line_point[0], line_point[1], line_point[2],
             yaw_of((line_point[3], line_point[4])))

    ground, bounds = emit_ground(centerline)
    entities = [ground, road]
    entities.extend(emit_barriers(centerline))
    entities.extend(emit_car(start))
    entities.extend(emit_furniture(bounds, start))
    write_scene(args.out, entities)

    print("corners, in racing order:")
    for corner in CORNERS:
        print(f"  {corner[0]:14} r={corner[3]:4.1f}m  h={corner[4]:4.1f}m")
    heights = [p[1] for p in centerline]
    grades = [
        (abs(b[1] - a[1]) / max(1e-6, math.dist((a[0], a[2]), (b[0], b[2]))))
        for a, b in zip(centerline, centerline[1:])
    ]
    print(f"lap: {summary['length']:.1f}m over {len(centerline) - 1} segments")
    print(f"elevation: {min(heights):.2f} .. {max(heights):.2f}m"
          f", steepest grade {max(grades) * 100:.1f}%")
    # scripts/car.rhai times laps off the start line, so it needs these two.
    print(f"start line: ({start[0]:.2f}, {start[1]:.2f}, {start[2]:.2f}) "
          f"heading {heading_vec(start[3])[0]:+.2f},{heading_vec(start[3])[1]:+.2f}")
    print(f"  scripts/car.rhai: let line_z = {start[2]:.2f}; "
          f"let line_x = {start[0]:.2f};")
    print(f"footprint: {bounds[2]:.0f} x {bounds[3]:.0f}m, {len(entities)} entities")

    problems = check(centerline)
    if problems:
        raise SystemExit("track layout is undrivable:\n  " + "\n  ".join(problems))

    if args.centerline:
        # Rotated to begin at the start line, not at the road's first point.
        # The autopilot counts a lap when its progress along this list wraps,
        # and the car's own script times laps off the line — so the two agree
        # only if index 0 *is* the line. (The road's `v = 0` is the first
        # corner, which is a different thing and rightly so: a polygon starts
        # where the shape starts, a lap where the line is.)
        ring = [p for p in centerline[:-1]]
        at = min(range(len(ring)), key=lambda i: abs(ring[i][5] - line_v))
        ring = ring[at:] + ring[:at]
        with open(args.centerline, "w") as handle:
            json.dump({
                "width": ROAD_WIDTH,
                "start": list(start),
                "nodes": [list(p[:3]) for p in ring],
            }, handle)


if __name__ == "__main__":
    main()
