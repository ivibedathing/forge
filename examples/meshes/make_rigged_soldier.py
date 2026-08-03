#!/usr/bin/env python3
"""Generate `rigged_soldier.gltf`: the arena shooter's player character.

Text glTF with a base64-embedded buffer, like `rigged_walker.gltf` and
`rigged_arm.gltf` beside it and for the same reason — a binary blob nobody can
diff is what invariant 1 exists to keep out of the repo, and a generated asset
can be regenerated rather than trusted.

This is the third rig in the repo and it is here because the other two cannot do
the job. `rigged_arm.gltf` is the M30 fixture (three joints, the smallest thing
that proves a palette composes). `rigged_walker.gltf` is the tour's character,
and its arms *swing* — which is exactly wrong for someone carrying a gun.

What this one adds, and why:

* **A weapon hand that holds still.** `HandR` is forward of the chest in the
  bind pose and stays there through `Run`: the arms are a rigid brace and the
  legs do the running. A weapon hung off that joint (M30's "hanging a prop off
  a hand is an ordinary `set_position`") therefore does not wave about, and the
  aim the script computes is the aim the picture shows.
* **Seventeen joints with a spine and a neck**, so the torso can counter-rotate
  against the hips without snapping the head round with it.
* **Two clips whose *shapes* differ, not just their rates** — `Idle` is a
  breathing sway, `Run` is a stride. That is what makes M36's
  `world.set_animation_clip` a hard cut worth having: M9 rejected blending, so a
  gait change here is a different clip, and two clips that were the same pose at
  two speeds would not demonstrate anything.
* **A UV per vertex**, cylindrical around each limb and along its length — the
  well-behaved layout `Tree` tubes use, so the character takes M26's maps like
  anything else.

    python3 examples/meshes/make_rigged_soldier.py
"""

import json
import math
import pathlib

from gltf_build import (
    ARRAY_BUFFER,
    BOX_FACES,
    ELEMENT_ARRAY_BUFFER,
    FLOAT,
    UNSIGNED_SHORT,
    Buffer,
    bounds,
    flat,
    floats,
    lerp,
    quat_x,
    quat_y,
    quat_z,
    shorts,
)

HERE = pathlib.Path(__file__).resolve().parent

# ── The skeleton ──────────────────────────────────────────────────────────
#
# Bind pose: standing at a low ready, facing -Z (the engine's forward, the
# direction a camera and a light look down). Positions are in the skin's own
# space, which is what the inverse bind matrices are built from; the glTF nodes
# carry the *relative* translations derived from them further down.
#
# Order is the skin's `joints` order and therefore the order `JOINTS_0` indexes.
# It is not sorted and must not be.
#
# The arms are already forward here rather than hanging at the sides, and that
# is the whole point of a separate rig: a bind pose is what every clip is a
# departure from, so a weapon stance authored into the bind is a weapon stance
# every clip inherits for free.
JOINTS = [
    # name,      parent,       bind position
    ("Hips", None, (0.00, 0.92, 0.00)),
    ("Spine", "Hips", (0.00, 1.10, 0.00)),
    ("Chest", "Spine", (0.00, 1.30, 0.00)),
    ("Neck", "Chest", (0.00, 1.48, 0.00)),
    ("Head", "Neck", (0.00, 1.60, 0.00)),
    ("ShoulderL", "Chest", (0.19, 1.42, 0.00)),
    ("ElbowL", "ShoulderL", (0.23, 1.27, -0.14)),
    ("HandL", "ElbowL", (0.15, 1.20, -0.34)),
    ("ShoulderR", "Chest", (-0.19, 1.42, 0.00)),
    ("ElbowR", "ShoulderR", (-0.23, 1.27, -0.13)),
    ("HandR", "ElbowR", (-0.12, 1.20, -0.33)),
    ("LegL", "Hips", (0.10, 0.90, 0.00)),
    ("KneeL", "LegL", (0.10, 0.49, 0.00)),
    ("FootL", "KneeL", (0.10, 0.07, 0.00)),
    ("LegR", "Hips", (-0.10, 0.90, 0.00)),
    ("KneeR", "LegR", (-0.10, 0.49, 0.00)),
    ("FootR", "KneeR", (-0.10, 0.07, 0.00)),
]

NAMES = [name for name, _, _ in JOINTS]
INDEX = {name: i for i, name in enumerate(NAMES)}
BIND = {name: position for name, _, position in JOINTS}
PARENT = {name: parent for name, parent, _ in JOINTS}

# ── The mesh ──────────────────────────────────────────────────────────────
#
# One tapered box per bone, swept from the parent joint to the child. Each
# bone's vertices blend the two joints it spans, which is what makes an elbow
# bend instead of shearing.
#
# `(from, to, half-width at from, half-width at to)`.
BONES = [
    ("Hips", "Spine", 0.155, 0.170),
    ("Spine", "Chest", 0.170, 0.185),
    ("Chest", "Neck", 0.185, 0.070),
    ("ShoulderL", "ElbowL", 0.068, 0.052),
    ("ElbowL", "HandL", 0.052, 0.042),
    ("ShoulderR", "ElbowR", 0.068, 0.052),
    ("ElbowR", "HandR", 0.052, 0.042),
    ("LegL", "KneeL", 0.095, 0.072),
    ("KneeL", "FootL", 0.072, 0.056),
    ("LegR", "KneeR", 0.095, 0.072),
    ("KneeR", "FootR", 0.072, 0.056),
]

# Blocks rigid to one joint. A skull and a boot do not deform, and a
# single-joint span is also the case that proves a weight of exactly 1 in slot
# 0 with three zeroes behind it round-trips.
#
# `(joint, offset from the joint, half extents)`.
BLOCKS = [
    ("Head", (0.00, 0.075, -0.005), (0.105, 0.115, 0.100)),
    # A boot reaches forward of its ankle, which is what stops the legs ending
    # in flat stumps when the camera looks down at 20 m — the only angle this
    # character is ever seen from.
    ("FootL", (0.00, -0.035, -0.075), (0.062, 0.040, 0.115)),
    ("FootR", (0.00, -0.035, -0.075), (0.062, 0.040, 0.115)),
    # A chest rig: the silhouette that says "soldier" from directly above,
    # where the head hides most of the torso.
    ("Chest", (0.00, 0.055, -0.070), (0.150, 0.110, 0.055)),
]

SEGMENTS = 3  # rings along each bone; enough that a bend reads as a bend.

positions = []
normals = []
uvs = []
joints = []
weights = []
indices = []

# The four corners of a cross-section, counter-clockwise seen from the bone's
# own +axis.
CORNERS = [(-1, -1), (1, -1), (1, 1), (-1, 1)]


def basis(direction):
    """An orthonormal frame whose third axis is `direction`.

    The perpendicular is built from whichever world axis the bone is least
    aligned with — M19's parallel-transport lesson in miniature: derive it from
    a fixed axis and a bone that happens to point along it spins its
    cross-section into a degenerate frame.
    """
    length = math.sqrt(sum(c * c for c in direction))
    forward = tuple(c / length for c in direction)
    up = (0.0, 0.0, 1.0) if abs(forward[1]) > 0.9 else (0.0, 1.0, 0.0)
    right = (
        up[1] * forward[2] - up[2] * forward[1],
        up[2] * forward[0] - up[0] * forward[2],
        up[0] * forward[1] - up[1] * forward[0],
    )
    r = math.sqrt(sum(c * c for c in right))
    right = tuple(c / r for c in right)
    real_up = (
        forward[1] * right[2] - forward[2] * right[1],
        forward[2] * right[0] - forward[0] * right[2],
        forward[0] * right[1] - forward[1] * right[0],
    )
    return right, real_up, forward


def push_quad(quad, normal, influences, uv_span):
    """One quad as two triangles, with its own copies of the four vertices.

    Flat-shaded — every face owns its vertices — so the limbs keep crisp edges
    instead of averaging into a rounded tube.
    """
    base = len(positions)
    corners_uv = [
        (uv_span[0], 0.0),
        (uv_span[1], 0.0),
        (uv_span[1], 1.0),
        (uv_span[0], 1.0),
    ]
    for point, (u, v), influence in zip(quad, corners_uv, influences):
        positions.append(point)
        normals.append(normal)
        uvs.append((u, v))
        joints.append(influence[0])
        weights.append(influence[1])
    indices.extend([base, base + 1, base + 2, base, base + 2, base + 3])


def blend(a, b, t):
    """The four-slot influence for a point `t` of the way from joint `a` to
    joint `b`. Two influences; the unused slots are zeroed, which is how a
    vertex says "fewer than four"."""
    return [INDEX[a], INDEX[b], 0, 0], [1.0 - t, t, 0.0, 0.0]


def sweep(start, end, half_start, half_end):
    origin = BIND[start]
    tip = BIND[end]
    direction = tuple(t - o for o, t in zip(tip, origin))
    right, up, _ = basis(direction)

    def ring(t, half):
        centre = lerp(origin, tip, t)
        return [
            tuple(
                centre[axis] + x * half * right[axis] + y * half * up[axis]
                for axis in range(3)
            )
            for x, y in CORNERS
        ]

    for segment in range(SEGMENTS):
        t0 = segment / SEGMENTS
        t1 = (segment + 1) / SEGMENTS
        r0 = ring(t0, half_start + (half_end - half_start) * t0)
        r1 = ring(t1, half_start + (half_end - half_start) * t1)
        influence0 = blend(start, end, t0)
        influence1 = blend(start, end, t1)
        for side in range(4):
            a, b = side, (side + 1) % 4
            mid_x = (CORNERS[a][0] + CORNERS[b][0]) / 2
            mid_y = (CORNERS[a][1] + CORNERS[b][1]) / 2
            length = math.hypot(mid_x, mid_y)
            normal = tuple(
                (mid_x / length) * right[axis] + (mid_y / length) * up[axis]
                for axis in range(3)
            )
            # `u` runs around the limb (a quarter per side), `v` along it.
            push_quad(
                [r0[a], r0[b], r1[b], r1[a]],
                normal,
                [influence0, influence0, influence1, influence1],
                (side / 4.0, (side + 1) / 4.0),
            )

    # Caps, so a limb is a closed solid: an open tube shows its inside through
    # the far wall the moment the camera catches it end-on.
    _, _, forward = basis(direction)
    influence_start = blend(start, end, 0.0)
    influence_end = blend(start, end, 1.0)
    cap0 = ring(0.0, half_start)
    cap1 = ring(1.0, half_end)
    push_quad(
        [cap0[3], cap0[2], cap0[1], cap0[0]],
        tuple(-c for c in forward),
        [influence_start] * 4,
        (0.0, 1.0),
    )
    push_quad(cap1, forward, [influence_end] * 4, (0.0, 1.0))


for bone in BONES:
    sweep(*bone)


def block(centre, half, joint):
    """An axis-aligned box rigid to one joint."""
    influence = ([INDEX[joint], 0, 0, 0], [1.0, 0.0, 0.0, 0.0])
    for normal, corners in BOX_FACES:
        quad = [
            tuple(centre[axis] + corners[i][axis] * half[axis] for axis in range(3))
            for i in range(4)
        ]
        push_quad(quad, normal, [influence] * 4, (0.0, 1.0))


for joint, offset, half in BLOCKS:
    block(tuple(b + o for b, o in zip(BIND[joint], offset)), half, joint)


# ── The clips ─────────────────────────────────────────────────────────────
#
# Sampled rather than key-posed: a stride is a pair of sine waves out of phase,
# and writing it as one is what makes the loop seam exact — the last sample is
# the first, computed rather than copied, so nothing drifts if the amplitudes
# are retuned.

FRAMES = 16
RUN_PERIOD = 0.62
IDLE_PERIOD = 3.6

# How far the thigh swings from vertical at the extremes of a stride. This is
# the number the stride length falls out of, and `list-joints` measures the
# result rather than trusting the arithmetic — see the module docstring of
# `make_arena.py` for where that number is used.
THIGH_SWING = 46.0


def run_curves():
    """Every animated channel of `Run`.

    The legs do all the work. The arms hold the weapon and only *shake* with
    the impacts — which is the difference between this rig and the tour
    walker's, whose arms counter-swing. A gun on the end of a swinging arm
    points somewhere new every frame, and the script's aim would stop
    describing the picture.

    The knee only ever bends backwards (`min(0, …)`), because a knee that
    hinges the other way is the single most obvious wrongness a character can
    have.
    """
    times = [RUN_PERIOD * i / FRAMES for i in range(FRAMES + 1)]

    def phase(t, offset):
        return 2 * math.pi * (t / RUN_PERIOD) + offset

    hips = []
    for t in times:
        # Two bobs per cycle: the body drops onto each planted foot. A runner
        # also leans into the run, but leaning is a rotation of the whole body
        # and the script owns that — the model faces where the player aims.
        drop = 0.055 * abs(math.sin(phase(t, 0.0)))
        hips.append((0.0, BIND["Hips"][1] - 0.020 - drop, 0.0))

    def leg(offset):
        return [quat_x(THIGH_SWING * math.sin(phase(t, offset))) for t in times]

    def knee(offset):
        return [
            quat_x(min(0.0, -68.0 * math.sin(phase(t, offset) - 0.85))) for t in times
        ]

    def foot(offset):
        return [quat_x(17.0 * math.sin(phase(t, offset) + 1.1)) for t in times]

    return times, [
        ("Hips", "translation", hips),
        # The hips twist with the stride and the chest gives most of it back,
        # so the shoulders — and the weapon on them — stay near the heading.
        ("Hips", "rotation", [quat_y(7.0 * math.sin(phase(t, 0.0))) for t in times]),
        ("Chest", "rotation", [quat_y(-5.0 * math.sin(phase(t, 0.0))) for t in times]),
        # The head stays level against the bob, which is what real runners do
        # and what stops the camera's subject bouncing.
        ("Neck", "rotation", [quat_x(3.0 * abs(math.sin(phase(t, 0.0)))) for t in times]),
        ("LegL", "rotation", leg(0.0)),
        ("KneeL", "rotation", knee(0.0)),
        ("FootL", "rotation", foot(0.0)),
        ("LegR", "rotation", leg(math.pi)),
        ("KneeR", "rotation", knee(math.pi)),
        ("FootR", "rotation", foot(math.pi)),
        # Two degrees of shake at twice the stride rate — the footfalls, not a
        # swing. Enough to read as carried weight, small enough that the muzzle
        # stays on target.
        (
            "ShoulderL",
            "rotation",
            [quat_x(2.2 * math.sin(2 * phase(t, 0.0))) for t in times],
        ),
        (
            "ShoulderR",
            "rotation",
            [quat_x(2.2 * math.sin(2 * phase(t, 0.0))) for t in times],
        ),
    ]


def idle_curves():
    """A slow breathing sway at the ready.

    Deliberately a different *shape* from `Run` rather than a slower version of
    it: the whole point of M36's `set_animation_clip` is that a gait change is a
    cut between two clips, and two clips that differ only in rate would prove
    nothing about the cut.
    """
    times = [IDLE_PERIOD * i / FRAMES for i in range(FRAMES + 1)]

    def phase(t):
        return 2 * math.pi * (t / IDLE_PERIOD)

    return times, [
        (
            "Hips",
            "translation",
            [(0.0, BIND["Hips"][1] + 0.011 * math.sin(phase(t)), 0.0) for t in times],
        ),
        ("Spine", "rotation", [quat_x(-1.8 * math.sin(phase(t))) for t in times]),
        # The head scans, slowly, at half the breathing rate — the one thing
        # that keeps a standing figure from reading as a statue.
        ("Neck", "rotation", [quat_y(8.0 * math.sin(phase(t) / 2)) for t in times]),
        ("ShoulderL", "rotation", [quat_x(2.4 * math.sin(phase(t))) for t in times]),
        ("ShoulderR", "rotation", [quat_x(2.4 * math.sin(phase(t) + 0.3)) for t in times]),
        # The weapon hand settles a touch as the chest rises, so the muzzle
        # drifts rather than hanging frozen.
        ("ElbowR", "rotation", [quat_z(1.6 * math.sin(phase(t) + 0.6)) for t in times]),
    ]


# ── Buffer assembly ───────────────────────────────────────────────────────

buffer = Buffer()
view, accessor = buffer.view, buffer.accessor

position_min, position_max = bounds(positions)
POSITION_A = accessor(
    {
        "bufferView": view(floats(flat(positions)), ARRAY_BUFFER),
        "componentType": FLOAT,
        "count": len(positions),
        "type": "VEC3",
        "min": list(position_min),
        "max": list(position_max),
    }
)
NORMAL_A = accessor(
    {
        "bufferView": view(floats(flat(normals)), ARRAY_BUFFER),
        "componentType": FLOAT,
        "count": len(normals),
        "type": "VEC3",
    }
)
UV_A = accessor(
    {
        "bufferView": view(floats(flat(uvs)), ARRAY_BUFFER),
        "componentType": FLOAT,
        "count": len(uvs),
        "type": "VEC2",
    }
)
JOINTS_A = accessor(
    {
        "bufferView": view(shorts(flat(joints)), ARRAY_BUFFER),
        "componentType": UNSIGNED_SHORT,
        "count": len(joints),
        "type": "VEC4",
    }
)
WEIGHTS_A = accessor(
    {
        "bufferView": view(floats(flat(weights)), ARRAY_BUFFER),
        "componentType": FLOAT,
        "count": len(weights),
        "type": "VEC4",
    }
)
INDEX_A = accessor(
    {
        "bufferView": view(shorts(indices), ELEMENT_ARRAY_BUFFER),
        "componentType": UNSIGNED_SHORT,
        "count": len(indices),
        "type": "SCALAR",
    }
)

# Inverse bind matrices: skin space → each joint's bind space. Every bind pose
# here is a pure translation, so each is the inverse of that translation.
# Column major, as glTF stores matrices.
inverse_binds = []
for name in NAMES:
    x, y, z = BIND[name]
    inverse_binds.extend([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, -x, -y, -z, 1])
INVERSE_BIND_A = accessor(
    {
        "bufferView": view(floats([float(v) for v in inverse_binds])),
        "componentType": FLOAT,
        "count": len(NAMES),
        "type": "MAT4",
    }
)


# ── Nodes ─────────────────────────────────────────────────────────────────
#
# Node 0 is the skinned mesh. glTF says its transform is ignored for skinning,
# so it is the identity here and the scene's own `Transform` places the player.
NODE_MESH = 0
NODE_OF = {name: 1 + i for i, name in enumerate(NAMES)}


def clip(name, curves):
    """One animation, with one sampler per channel."""
    times, channels = curves
    time_a = accessor(
        {
            "bufferView": view(floats(times)),
            "componentType": FLOAT,
            "count": len(times),
            "type": "SCALAR",
            "min": [min(times)],
            "max": [max(times)],
        }
    )
    samplers = []
    wired = []
    for joint, path, values in channels:
        value_a = accessor(
            {
                "bufferView": view(floats(flat(values))),
                "componentType": FLOAT,
                "count": len(values),
                "type": "VEC3" if path == "translation" else "VEC4",
            }
        )
        wired.append(
            {"sampler": len(samplers), "target": {"node": NODE_OF[joint], "path": path}}
        )
        samplers.append({"input": time_a, "output": value_a, "interpolation": "LINEAR"})
    return {"name": name, "samplers": samplers, "channels": wired}


nodes = [{"name": "Soldier", "mesh": 0, "skin": 0}]
for name in NAMES:
    parent = PARENT[name]
    origin = BIND[parent] if parent else (0.0, 0.0, 0.0)
    local = tuple(round(b - o, 6) for b, o in zip(BIND[name], origin))
    node = {"name": name, "translation": list(local)}
    children = [NODE_OF[other] for other in NAMES if PARENT[other] == name]
    if children:
        node["children"] = children
    nodes.append(node)

document = {
    "asset": {
        "version": "2.0",
        "generator": "forge examples/meshes/make_rigged_soldier.py",
    },
    "scene": 0,
    "scenes": [{"nodes": [NODE_MESH, NODE_OF["Hips"]]}],
    "nodes": nodes,
    "skins": [
        {
            "name": "SoldierRig",
            "inverseBindMatrices": INVERSE_BIND_A,
            "skeleton": NODE_OF["Hips"],
            "joints": [NODE_OF[name] for name in NAMES],
        }
    ],
    "meshes": [
        {
            "name": "SoldierMesh",
            "primitives": [
                {
                    "attributes": {
                        "POSITION": POSITION_A,
                        "NORMAL": NORMAL_A,
                        "TEXCOORD_0": UV_A,
                        "JOINTS_0": JOINTS_A,
                        "WEIGHTS_0": WEIGHTS_A,
                    },
                    "indices": INDEX_A,
                    "mode": 4,
                }
            ],
        }
    ],
    "animations": [clip("Idle", idle_curves()), clip("Run", run_curves())],
    "accessors": buffer.accessors,
    "bufferViews": buffer.views,
    "buffers": buffer.buffers(),
}

out = HERE / "rigged_soldier.gltf"
out.write_text(json.dumps(document, indent=2) + "\n")
print(
    f"wrote {out} "
    f"({len(positions)} vertices, {len(indices) // 3} triangles, "
    f"{len(NAMES)} joints, {len(document['animations'])} clips)"
)
print(f"  stands {position_max[1]:.3f} m tall; weapon hand at {BIND['HandR']}")
print("  measure Run's stride with:  bin/engine list-joints <scene> --entity Player")
