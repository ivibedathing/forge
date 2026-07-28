"""Emit examples/scenes/car_track.json: a Spa-Francorchamps in miniature.

The circuit is authored as a closed polygon: one CORNER per turn, each holding
a plan-view position, a corner radius, and a height above the ground plane. The
centerline is that polygon with its corners rounded — straight along each edge,
constant-radius arc through each corner — and this script bakes the result into
the scene's entities: road plates (pitched to the grade, each with a cuboid
collider, so the car really drives up and down), embankments filling the gap to
the ground plane, kerbs on the inside of the tight corners, and barrier posts
down both edges.

Authoring the loop as a polygon is what makes it a circuit rather than a
ribbon. A closed polygon returns to its own first vertex by construction, and
its exterior angles sum to exactly one turn, so position and heading close
without solving anything; the elevation profile closes for the same reason,
being heights carried on the vertices themselves. The two things that can still
be wrong are local and reported by --report: a corner radius too large for the
edges feeding it, and a grade too steep to climb.

The layout follows Spa's sequence at roughly 1/15 scale: La Source, the plunge
to Eau Rouge, the Raidillon climb onto the Kemmel straight, Les Combes at the
high point, Rivage and the descent through Pouhon, Fagnes, Stavelot, the long
Blanchimont run back uphill, and the Bus Stop chicane onto the start line.

Usage:
    python3 examples/scenes/make_car_track.py [--centerline path.json]

`--centerline` dumps the sampled centerline (position, heading, width) beside
the scene: the autopilot that authors car_track_lap.input.jsonl steers along
it, and it is regenerated rather than committed.
"""

import argparse
import json
import math

# ---------------------------------------------------------------------------
# Track description
# ---------------------------------------------------------------------------

ROAD_WIDTH = 7.0        # meters of asphalt, edge to edge
ROAD_THICK = 0.5        # plate thickness; the collider is the same box

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

# Where the start line sits: this far along the edge that runs from the last
# corner to the first — the pit straight, climbing to La Source.
START_EDGE_FRACTION = 0.55

# Sampling: how finely the centerline is cut into road plates.
STRAIGHT_STEP = 6.0     # meters per plate on a straight
ARC_STEP_DEG = 10.0     # degrees of arc per plate
ARC_STEP_MIN = 1.6      # but never shorter than this

# Barriers are a continuous guardrail, not a dashed line: each post is a
# little longer than the gap it is spaced by, so a car sliding off cannot slip
# between two of them and fall off the elevated road.
BARRIER_SPACING = 5.0   # meters between barrier posts, per side
BARRIER_LENGTH = 5.4
VERGE = 1.6             # meters of drivable shoulder each side of the asphalt
# How far the asphalt slab stands proud of the verge it is laid on. It has to
# clear the *neighbouring* segments' verge boxes, which are wider than the
# asphalt and swing across it at every corner joint: a few centimeters short
# and the road shows through in tan stripes from above. Purely cosmetic — the
# slab carries no collider, so this number never moves the car.
SKIN = 0.09
# Consecutive rectangles cannot tile a curve: on the outside of every corner
# joint they leave a wedge that shows the verge through. The asphalt slabs
# overlap far more than the verge boxes need to, to cover it. Cosmetic too —
# the slabs carry no collider.
ASPHALT_OVERLAP = 1.1
KERB_MAX_RADIUS = 16.0  # kerbs go on the inside of corners this tight

# Materials, all linear RGB.
ASPHALT = [0.09, 0.09, 0.10]
PAINT = [0.92, 0.92, 0.92]
KERB_RED = [0.80, 0.10, 0.08]
BARRIER_RED = [0.85, 0.08, 0.06]
EARTH = [0.20, 0.17, 0.13]
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


def normalize(v):
    length = math.hypot(v[0], v[1])
    return (v[0] / length, v[1] / length)


def corner_fillet(previous, corner, following):
    """Round one polygon corner: turn angle, tangent length, arc center.

    The fillet is the circle of the corner's radius tucked into the wedge
    between the two edges. It touches each edge a tangent length back from the
    vertex, which is the room the straights have to leave for it.
    """
    incoming = normalize((corner[1] - previous[1], corner[2] - previous[2]))
    outgoing = normalize((following[1] - corner[1], following[2] - corner[2]))
    cross = incoming[0] * outgoing[1] - incoming[1] * outgoing[0]
    dot = max(-1.0, min(1.0, incoming[0] * outgoing[0] + incoming[1] * outgoing[1]))
    turn = math.degrees(math.acos(dot))          # unsigned; the cross has the sign
    sign = 1.0 if cross > 0 else -1.0            # + turns right, the loop's way
    tangent = corner[3] * math.tan(math.radians(turn) / 2.0)
    # The center sits square to the incoming edge, on the inside of the turn.
    inward = (-incoming[1] * sign, incoming[0] * sign)
    entry = (corner[1] - incoming[0] * tangent, corner[2] - incoming[1] * tangent)
    return {
        "turn": turn,
        "sign": sign,
        "tangent": tangent,
        "center": (entry[0] + inward[0] * corner[3], entry[1] + inward[1] * corner[3]),
        "entry": entry,
        "exit": (corner[1] + outgoing[0] * tangent, corner[2] + outgoing[1] * tangent),
        "entry_yaw": yaw_of(incoming),
    }


def build_plan():
    """The rounded polygon as a closed ring of (x, z, yaw, corner, arc?) nodes.

    Elevation is left for later: heights live on the corners, and interpolating
    between them wants a path length that only exists once the ring is walked.
    """
    fillets = [
        corner_fillet(CORNERS[i - 1], CORNERS[i], CORNERS[(i + 1) % len(CORNERS)])
        for i in range(len(CORNERS))
    ]

    nodes = []
    for i, corner in enumerate(CORNERS):
        fillet, previous = fillets[i], fillets[i - 1]

        # Straight from the previous corner's exit to this corner's entry.
        span = (fillet["entry"][0] - previous["exit"][0],
                fillet["entry"][1] - previous["exit"][1])
        length = math.hypot(*span)
        count = max(1, int(math.ceil(length / STRAIGHT_STEP)))
        for k in range(count):
            f = k / count
            nodes.append((
                previous["exit"][0] + span[0] * f,
                previous["exit"][1] + span[1] * f,
                fillet["entry_yaw"],
                CORNERS[i - 1][0],
                False,
            ))

        # Then the arc through the corner itself.
        radius, turn, sign = corner[3], fillet["turn"], fillet["sign"]
        arc_length = math.radians(turn) * radius
        steps = max(1, int(math.ceil(max(turn / ARC_STEP_DEG,
                                         arc_length / ARC_STEP_MIN))))
        for k in range(steps):
            yaw = fillet["entry_yaw"] - sign * turn * (k / steps)
            # Every point on the arc is one radius from the center, square to
            # the heading there.
            outward = right_vec(yaw)
            nodes.append((
                fillet["center"][0] - outward[0] * radius * sign,
                fillet["center"][1] - outward[1] * radius * sign,
                yaw,
                corner[0],
                True,
            ))
    return nodes, fillets


def add_elevation(plan):
    """Interpolate the corner heights along the ring by distance travelled."""
    distance = [0.0]
    for i in range(1, len(plan) + 1):
        a, b = plan[i - 1], plan[i % len(plan)]
        distance.append(distance[-1] + math.dist((a[0], a[1]), (b[0], b[1])))
    total = distance[-1]

    # Each corner's height is pinned at the middle of its arc; the road ramps
    # between those marks, so the grade is constant along a straight.
    marks = []
    for corner in CORNERS:
        indices = [k for k, node in enumerate(plan)
                   if node[3] == corner[0] and node[4]]
        marks.append((distance[indices[len(indices) // 2]], corner[4]))
    marks.sort()

    def height_at(d):
        for k in range(len(marks)):
            start, end = marks[k], marks[(k + 1) % len(marks)]
            span = (end[0] - start[0]) % total
            offset = (d - start[0]) % total
            if span > 0.0 and offset <= span:
                return start[1] + (end[1] - start[1]) * (offset / span)
        return marks[0][1]

    return [
        (node[0], height_at(distance[i]), node[1], node[2], node[3], node[4])
        for i, node in enumerate(plan)
    ]


def rotate_to_start(nodes):
    """Turn the ring into a lap: begin at the start line, end back on it.

    The line sits partway along the pit straight, so the ring is rotated to
    begin at the node nearest that point, and the first node is repeated at the
    end — the road wants a closing plate, and the lap wants somewhere to stop.
    """
    first, last = CORNERS[0], CORNERS[-1]
    target = (
        last[1] + (first[1] - last[1]) * START_EDGE_FRACTION,
        last[2] + (first[2] - last[2]) * START_EDGE_FRACTION,
    )
    best = min(range(len(nodes)),
               key=lambda i: math.dist((nodes[i][0], nodes[i][2]), target))
    ring = nodes[best:] + nodes[:best]
    return ring + [ring[0]]


def build():
    plan, fillets = build_plan()
    return rotate_to_start(add_elevation(plan)), fillets


def check(nodes, fillets):
    """Everything the polygon cannot guarantee: corner fit and drivable grades."""
    problems = []
    for corner in CORNERS:
        if corner[3] < MIN_RADIUS:
            problems.append(f"{corner[0]}: radius {corner[3]:.1f}m is below the "
                            f"{MIN_RADIUS:.0f}m the car needs")
    for i, corner in enumerate(CORNERS):
        following = CORNERS[(i + 1) % len(CORNERS)]
        edge = math.dist((corner[1], corner[2]), (following[1], following[2]))
        needed = fillets[i]["tangent"] + fillets[(i + 1) % len(CORNERS)]["tangent"]
        if needed > edge - 1.0:
            problems.append(
                f"{corner[0]} -> {following[0]}: a {edge:.1f}m edge cannot hold "
                f"{needed:.1f}m of corner radius"
            )
    steepest = 0.0
    for i in range(len(nodes) - 1):
        a, b = nodes[i], nodes[i + 1]
        run = math.dist((a[0], a[2]), (b[0], b[2]))
        if run > 0.1:
            steepest = max(steepest, abs(b[1] - a[1]) / run)
    if steepest > 0.16:
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


def plate_rotation(yaw, pitch):
    """XYZ Euler degrees for yaw-then-pitch — the file format's only order.

    The rotation wanted is Ry(yaw) * Rx(pitch): pitch in the plate's own frame,
    so a climbing segment tips about its axle rather than about world X. The
    scene format stores XYZ (Rx * Ry * Rz), so the product is decomposed back.
    """
    cy, sy = math.cos(math.radians(yaw)), math.sin(math.radians(yaw))
    cp, sp = math.cos(math.radians(pitch)), math.sin(math.radians(pitch))
    # m = Ry(yaw) @ Rx(pitch), column-major-free: m[row][col]
    m = [
        [cy, sy * sp, sy * cp],
        [0.0, cp, -sp],
        [-sy, cy * sp, cy * cp],
    ]
    b = math.asin(max(-1.0, min(1.0, m[0][2])))
    a = math.atan2(-m[1][2], m[2][2])
    c = math.atan2(-m[0][1], m[0][0])
    return [math.degrees(a), math.degrees(b), math.degrees(c)]


def plate_normal(yaw, pitch):
    cy, sy = math.cos(math.radians(yaw)), math.sin(math.radians(yaw))
    cp, sp = math.cos(math.radians(pitch)), math.sin(math.radians(pitch))
    return (sy * sp, cp, cy * sp)


def entity(name, components):
    return {"name": name, "components": components}


def segment_geometry(a, b):
    """Center, yaw, pitch and length of the road plate spanning nodes a -> b."""
    dx, dy, dz = b[0] - a[0], b[1] - a[1], b[2] - a[2]
    flat = math.hypot(dx, dz)
    length = math.hypot(flat, dy)
    yaw = math.degrees(math.atan2(-dx, -dz))
    pitch = math.degrees(math.atan2(dy, flat))
    center = ((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0, (a[2] + b[2]) / 2.0)
    return center, yaw, pitch, length


def corner_named(name):
    for corner in CORNERS:
        if corner[0] == name:
            return corner
    raise KeyError(name)


def emit_track(nodes, fillets):
    """Road plates, embankments, kerbs and barriers along the sampled line."""
    entities = []
    barrier_debt = 0.0
    barrier_index = 0
    turn_signs = {CORNERS[i][0]: fillets[i]["sign"] for i in range(len(CORNERS))}

    for i in range(len(nodes) - 1):
        a, b = nodes[i], nodes[i + 1]
        center, yaw, pitch, length = segment_geometry(a, b)
        nx, ny, nz = plate_normal(yaw, pitch)
        # The plate hangs below the surface it carries.
        road_center = (
            center[0] - nx * ROAD_THICK / 2.0,
            center[1] - ny * ROAD_THICK / 2.0,
            center[2] - nz * ROAD_THICK / 2.0,
        )
        # Overlap neighbours slightly so kinks between plates leave no seam a
        # suspension ray can drop through.
        plate_len = length + 0.35
        surface = min(a[1], b[1])

        # The embankment is one deep box per segment, cut to the same grade as
        # the road and reaching below the ground plane, which hides its
        # underside. It is the only thing here with a collider: this single
        # continuous surface, wider than the asphalt, is what the car drives
        # on. Splitting road and shoulder into two colliders at different
        # heights builds a ledge at the asphalt edge, and a wheel that drops
        # off it wedges against the step and stops the car dead.
        bank_thick = surface + 1.0
        entities.append(entity(f"Bank{i:03d}", [
            transform(
                (
                    center[0] - nx * bank_thick / 2.0,
                    center[1] - ny * bank_thick / 2.0,
                    center[2] - nz * bank_thick / 2.0,
                ),
                plate_rotation(yaw, pitch),
                (ROAD_WIDTH + VERGE * 2.0, bank_thick, plate_len),
            ),
            {"type": "Mesh", "asset": "builtin:cube"},
            material(EARTH, roughness=1.0),
            box_collider(0.85),
        ]))

        # The asphalt itself is paint over that: a thin slab laid on the verge,
        # no collider of its own, purely so the road reads as road.
        entities.append(entity(f"Road{i:03d}", [
            transform(
                (
                    road_center[0] + nx * SKIN,
                    road_center[1] + ny * SKIN,
                    road_center[2] + nz * SKIN,
                ),
                plate_rotation(yaw, pitch),
                (ROAD_WIDTH, ROAD_THICK, length + ASPHALT_OVERLAP),
            ),
            {"type": "Mesh", "asset": "builtin:cube"},
            material(ASPHALT, roughness=0.95),
        ]))

        # Kerbs: alternating red/white strips on the inside of tight corners.
        corner = corner_named(a[4])
        if a[5] and corner[3] <= KERB_MAX_RADIUS:
            inside = turn_signs[a[4]]
            rx, rz = right_vec(yaw)
            offset = inside * (ROAD_WIDTH / 2.0 - 0.45)
            entities.append(entity(f"Kerb{i:03d}", [
                transform(
                    (
                        road_center[0] + rx * offset,
                        road_center[1] + 0.04,
                        road_center[2] + rz * offset,
                    ),
                    plate_rotation(yaw, pitch),
                    (0.9, ROAD_THICK, plate_len),
                ),
                {"type": "Mesh", "asset": "builtin:cube"},
                material(KERB_RED if i % 2 == 0 else PAINT, roughness=0.7),
            ]))

        # Barriers: posts at a fixed spacing along the arc length, both sides.
        barrier_debt += length
        while barrier_debt >= BARRIER_SPACING:
            barrier_debt -= BARRIER_SPACING
            rx, rz = right_vec(yaw)
            reach = ROAD_WIDTH / 2.0 + VERGE - 0.3
            for side, tag in ((1.0, "R"), (-1.0, "L")):
                entities.append(entity(f"Barrier{tag}{barrier_index:03d}", [
                    transform(
                        (
                            center[0] + rx * reach * side,
                            surface + 0.28,
                            center[2] + rz * reach * side,
                        ),
                        (0.0, yaw, 0.0),
                        (0.45, 0.6, BARRIER_LENGTH),
                    ),
                    {"type": "Mesh", "asset": "builtin:cube"},
                    material(BARRIER_RED if barrier_index % 2 == 0 else PAINT, roughness=0.7),
                    # Slippery on purpose. A guardrail a car can *stick* to
                    # ends the drive: nose it at walking pace and no amount of
                    # throttle frees it again.
                    box_collider(0.05, 0.1),
                ]))
            barrier_index += 1

    return entities


def emit_ground(nodes):
    xs = [n[0] for n in nodes]
    zs = [n[2] for n in nodes]
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


def emit_start_line(nodes):
    """Paint across the road at the first node — flat to the grade there.

    Thin on purpose: this is paint, not a kerb. Anything with real height here
    is a wall the car is parked against at step 0.
    """
    _, yaw, pitch, _ = segment_geometry(nodes[0], nodes[1])
    return entity("StartLine", [
        transform(
            (nodes[0][0], nodes[0][1] + 0.02, nodes[0][2]),
            plate_rotation(yaw, pitch),
            (ROAD_WIDTH, 0.04, 0.7),
        ),
        {"type": "Mesh", "asset": "builtin:cube"},
        material(PAINT, roughness=0.6),
    ])


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
            # near plane spends all its depth precision on empty air and the
            # asphalt z-fights with the verge a few centimeters below it.
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


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", default="examples/scenes/car_track.json")
    parser.add_argument("--centerline")
    args = parser.parse_args()

    nodes, fillets = build()

    # The car sits on the first node, pointed down the pit straight.
    start = (nodes[0][0], nodes[0][1], nodes[0][2], nodes[0][3])

    ground, bounds = emit_ground(nodes)
    entities = [ground]
    entities.extend(emit_track(nodes, fillets))
    entities.append(emit_start_line(nodes))
    entities.extend(emit_car(start))
    entities.extend(emit_furniture(bounds, start))

    scene = {
        "name": "car_track",
        "entities": entities,
        "physics": {"gravity": [0.0, -9.81, 0.0], "timestep_hz": 60},
    }
    with open(args.out, "w") as handle:
        json.dump(scene, handle, indent=2)
        handle.write("\n")

    length = sum(math.dist((nodes[i][0], nodes[i][1], nodes[i][2]),
                           (nodes[i + 1][0], nodes[i + 1][1], nodes[i + 1][2]))
                 for i in range(len(nodes) - 1))
    grades = [(abs(nodes[i + 1][1] - nodes[i][1]) / max(1e-6, math.dist(
        (nodes[i][0], nodes[i][2]), (nodes[i + 1][0], nodes[i + 1][2]))), nodes[i][4])
        for i in range(len(nodes) - 1)]
    steepest, steepest_at = max(grades)
    print("corners, in racing order:")
    for i, corner in enumerate(CORNERS):
        turn = fillets[i]["turn"] * fillets[i]["sign"]
        way = "right" if turn > 0 else "left "
        print(f"  {corner[0]:14} {way} {abs(turn):5.1f} deg  r={corner[3]:4.1f}m"
              f"  h={corner[4]:4.1f}m")
    print(f"lap: {length:.1f}m over {len(nodes) - 1} plates")
    print(f"elevation: {min(n[1] for n in nodes):.2f} .. {max(n[1] for n in nodes):.2f}m"
          f", steepest grade {steepest * 100:.1f}% at {steepest_at}")
    # scripts/car.rhai times laps off the start line, so it needs these two.
    print(f"start line: ({start[0]:.2f}, {start[1]:.2f}, {start[2]:.2f}) "
          f"heading {heading_vec(start[3])[0]:+.2f},{heading_vec(start[3])[1]:+.2f}")
    print(f"  scripts/car.rhai: let line_z = {start[2]:.2f}; "
          f"let line_x = {start[0]:.2f};")
    print(f"footprint: {bounds[2]:.0f} x {bounds[3]:.0f}m, {len(entities)} entities")

    problems = check(nodes, fillets)
    if problems:
        raise SystemExit("track layout is unbuildable:\n  " + "\n  ".join(problems))

    if args.centerline:
        with open(args.centerline, "w") as handle:
            json.dump({
                "width": ROAD_WIDTH,
                "start": list(start),
                "nodes": [list(n[:5]) for n in nodes],
            }, handle)


if __name__ == "__main__":
    main()
