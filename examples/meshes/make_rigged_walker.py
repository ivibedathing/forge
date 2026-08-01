#!/usr/bin/env python3
"""Generate `rigged_walker.gltf`: the showcase tour's rigged character.

Text glTF with a base64-embedded buffer, like `rigged_arm.gltf` beside it and
for the same reason — a binary blob nobody can diff is what invariant 1 exists
to keep out of the repo, and a generated asset can be regenerated rather than
trusted.

`rigged_arm.gltf` is the *fixture*: three joints in a chain, the smallest thing
that can prove a palette composes. This is the *character*: thirteen joints in a
tree with two branches per side, a locomotion clip that loops, and a mesh whose
limbs pass each other — which is the case a single chain cannot pose wrong.

What it carries, and why each piece is here:

* **A skeleton with branches.** `Hips` roots a spine to the head and two legs;
  the chest roots two arms. A chain resolves parents in whatever order it is
  written in; a tree does not, which is what exercises `joint_globals`'
  parents-before-children resolution on real data.
* **Limbs that cross.** In mid-stride the forward thigh passes the rear one.
  Nothing in a one-armed fixture can tell a correct palette from one indexing
  the wrong joint; two limbs swinging out of phase can.
* **`Walk`, a one-second loop whose last keyframe is its first**, so the clip
  seams invisibly under `looping: true` and `t = period` equals `t = 0`
  byte-for-byte — M9's property, on a skinned mesh.
* **`Idle`**, a slower breathing sway, so `path#Clip` has a choice to make and
  the tour can put a second character on the same file without a second file.
* **A UV per vertex**, so the character can take the M26 material system's maps
  like anything else. Cylindrical around each limb, along its length — the
  well-behaved layout `Tree` tubes use, and for the same reason: a texture that
  varies across `u` wraps a limb rather than striping it lengthwise.

    python3 examples/meshes/make_rigged_walker.py
"""

import base64
import json
import math
import pathlib
import struct

HERE = pathlib.Path(__file__).resolve().parent

# ── The skeleton ──────────────────────────────────────────────────────────
#
# Bind pose: standing, facing -Z (the engine's forward, the direction a camera
# and a light look down). Positions are world-space in the skin's own space,
# which is what the inverse bind matrices are built from; the glTF nodes carry
# the *relative* translations derived from them further down.
#
# Order is the skin's `joints` order and therefore the order `JOINTS_0` indexes.
# It is not sorted and must not be.
JOINTS = [
    # name,      parent,     bind position
    ("Hips", None, (0.00, 0.92, 0.00)),
    ("Chest", "Hips", (0.00, 1.24, 0.00)),
    ("Head", "Chest", (0.00, 1.58, 0.00)),
    ("ArmL", "Chest", (0.20, 1.36, 0.00)),
    ("HandL", "ArmL", (0.28, 0.98, 0.00)),
    ("ArmR", "Chest", (-0.20, 1.36, 0.00)),
    ("HandR", "ArmR", (-0.28, 0.98, 0.00)),
    ("LegL", "Hips", (0.11, 0.90, 0.00)),
    ("KneeL", "LegL", (0.11, 0.48, 0.00)),
    ("FootL", "KneeL", (0.11, 0.06, 0.00)),
    ("LegR", "Hips", (-0.11, 0.90, 0.00)),
    ("KneeR", "LegR", (-0.11, 0.48, 0.00)),
    ("FootR", "KneeR", (-0.11, 0.06, 0.00)),
]

NAMES = [name for name, _, _ in JOINTS]
INDEX = {name: i for i, name in enumerate(NAMES)}
BIND = {name: position for name, _, position in JOINTS}
PARENT = {name: parent for name, parent, _ in JOINTS}

# ── The mesh ──────────────────────────────────────────────────────────────
#
# One tapered box per bone, swept from the parent joint to the child, plus a
# head block. Each bone's vertices blend the two joints it spans, which is what
# makes an elbow bend instead of shearing: at the joint itself the weights are
# half and half, so the surface folds rather than creasing at a hard boundary.
#
# `(from, to, half-width at from, half-width at to)`.
BONES = [
    ("Hips", "Chest", 0.17, 0.19),
    ("Chest", "Head", 0.19, 0.11),
    ("ArmL", "HandL", 0.070, 0.048),
    ("ArmR", "HandR", 0.070, 0.048),
    ("LegL", "KneeL", 0.095, 0.075),
    ("KneeL", "FootL", 0.075, 0.058),
    ("LegR", "KneeR", 0.095, 0.075),
    ("KneeR", "FootR", 0.075, 0.058),
]

# The head is a block on one joint alone — a skull does not deform, and a
# single-joint span is also the case that proves a weight of exactly 1 in slot
# 0 with three zeroes behind it round-trips.
HEAD_HALF = (0.115, 0.125, 0.105)

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


def lerp(a, b, t):
    return tuple(x + (y - x) * t for x, y in zip(a, b))


def basis(direction):
    """An orthonormal frame whose third axis is `direction`.

    The perpendicular is built from whichever world axis the bone is least
    aligned with, which is M19's parallel-transport lesson in miniature: derive
    it from a fixed axis and a bone that happens to point along it spins its
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
    instead of averaging into a rounded tube. The character is stylised and the
    faces are what make its silhouette legible at the distances the tour's
    cameras sit at.
    """
    base = len(positions)
    corners_uv = [(uv_span[0], 0.0), (uv_span[1], 0.0), (uv_span[1], 1.0), (uv_span[0], 1.0)]
    for point, (u, v), influence in zip(quad, corners_uv, influences):
        positions.append(point)
        normals.append(normal)
        uvs.append((u, v))
        joints.append(influence[0])
        weights.append(influence[1])
    indices.extend([base, base + 1, base + 2, base, base + 2, base + 3])


def blend(a, b, t):
    """The four-slot influence for a point `t` of the way from bone joint `a`
    to joint `b`. Two influences; the unused slots are zeroed, which is how a
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
    faces = [
        ((0, 1, 0), [(-1, 1, -1), (1, 1, -1), (1, 1, 1), (-1, 1, 1)]),
        ((0, -1, 0), [(-1, -1, 1), (1, -1, 1), (1, -1, -1), (-1, -1, -1)]),
        ((0, 0, 1), [(-1, -1, 1), (-1, 1, 1), (1, 1, 1), (1, -1, 1)]),
        ((0, 0, -1), [(1, -1, -1), (1, 1, -1), (-1, 1, -1), (-1, -1, -1)]),
        ((1, 0, 0), [(1, -1, 1), (1, 1, 1), (1, 1, -1), (1, -1, -1)]),
        ((-1, 0, 0), [(-1, -1, -1), (-1, 1, -1), (-1, 1, 1), (-1, -1, 1)]),
    ]
    for normal, corners in faces:
        quad = [
            tuple(centre[axis] + corners[i][axis] * half[axis] for axis in range(3))
            for i in range(4)
        ]
        push_quad(quad, normal, [influence] * 4, (0.0, 1.0))


block(
    (BIND["Head"][0], BIND["Head"][1] + 0.10, BIND["Head"][2]),
    HEAD_HALF,
    "Head",
)


# ── The clips ─────────────────────────────────────────────────────────────
#
# Sampled rather than key-posed: a walk is a pair of sine waves out of phase,
# and writing it as one is what makes the loop seam exact — the last sample is
# the first, computed rather than copied, so nothing drifts if the amplitudes
# are retuned.

FRAMES = 16
WALK_PERIOD = 1.0
IDLE_PERIOD = 4.0


def quat_x(degrees):
    half = math.radians(degrees) / 2
    return (math.sin(half), 0.0, 0.0, math.cos(half))


def quat_y(degrees):
    half = math.radians(degrees) / 2
    return (0.0, math.sin(half), 0.0, math.cos(half))


IDENTITY_QUAT = (0.0, 0.0, 0.0, 1.0)


def walk_curves():
    """Every animated channel of `Walk`, as `(joint, path, [values])`.

    Two strides per cycle — the left leg leads, the right follows half a period
    later — with the arms counter-swinging, which is what stops a walk reading
    as a shuffle. The knee only ever bends backwards (`min(0, …)`), because a
    knee that hinges the other way is the single most obvious wrongness a
    character can have.
    """
    times = [WALK_PERIOD * i / FRAMES for i in range(FRAMES + 1)]

    def phase(t, offset):
        return 2 * math.pi * (t / WALK_PERIOD) + offset

    hips = []
    for t in times:
        # Two bobs per stride: the body rises over each planted leg.
        rise = 0.030 * abs(math.sin(phase(t, 0.0)))
        hips.append((0.0, BIND["Hips"][1] - 0.030 + rise, 0.0))

    def leg(offset):
        return [quat_x(26.0 * math.sin(phase(t, offset))) for t in times]

    def knee(offset):
        return [
            quat_x(min(0.0, -42.0 * math.sin(phase(t, offset) - 0.9))) for t in times
        ]

    def foot(offset):
        return [quat_x(14.0 * math.sin(phase(t, offset) + 1.2)) for t in times]

    def arm(offset):
        return [quat_x(19.0 * math.sin(phase(t, offset))) for t in times]

    return times, [
        ("Hips", "translation", hips),
        # The torso counter-rotates a little against the hips, which is what
        # makes the shoulders swing rather than ride along.
        ("Chest", "rotation", [quat_y(6.0 * math.sin(phase(t, 0.0))) for t in times]),
        ("LegL", "rotation", leg(0.0)),
        ("KneeL", "rotation", knee(0.0)),
        ("FootL", "rotation", foot(0.0)),
        ("LegR", "rotation", leg(math.pi)),
        ("KneeR", "rotation", knee(math.pi)),
        ("FootR", "rotation", foot(math.pi)),
        # Arms swing opposite the leg on the same side.
        ("ArmL", "rotation", arm(math.pi)),
        ("ArmR", "rotation", arm(0.0)),
    ]


def idle_curves():
    """A slow breathing sway, so the file carries a second clip."""
    times = [IDLE_PERIOD * i / FRAMES for i in range(FRAMES + 1)]

    def phase(t):
        return 2 * math.pi * (t / IDLE_PERIOD)

    return times, [
        (
            "Hips",
            "translation",
            [
                (0.0, BIND["Hips"][1] + 0.012 * math.sin(phase(t)), 0.0)
                for t in times
            ],
        ),
        ("Chest", "rotation", [quat_x(-2.2 * math.sin(phase(t))) for t in times]),
        ("Head", "rotation", [quat_y(7.0 * math.sin(phase(t) / 2)) for t in times]),
        ("ArmL", "rotation", [quat_x(3.0 * math.sin(phase(t))) for t in times]),
        ("ArmR", "rotation", [quat_x(3.0 * math.sin(phase(t) + 0.4)) for t in times]),
    ]


# ── Buffer assembly ───────────────────────────────────────────────────────

blob = bytearray()
views = []


def view(data, target=None):
    """Append bytes as a bufferView, padded so the next one starts aligned."""
    while len(blob) % 4:
        blob.append(0)
    entry = {"buffer": 0, "byteOffset": len(blob), "byteLength": len(data)}
    if target is not None:
        entry["target"] = target
    blob.extend(data)
    views.append(entry)
    return len(views) - 1


def floats(values):
    return struct.pack(f"<{len(values)}f", *values)


def flat(rows):
    return [component for row in rows for component in row]


ARRAY_BUFFER, ELEMENT_ARRAY_BUFFER = 34962, 34963
FLOAT, UNSIGNED_SHORT = 5126, 5123

accessors = []


def accessor(entry):
    accessors.append(entry)
    return len(accessors) - 1


def bounds(rows):
    columns = list(zip(*rows))
    return [min(c) for c in columns], [max(c) for c in columns]


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
        "bufferView": view(
            struct.pack(f"<{len(joints) * 4}H", *flat(joints)), ARRAY_BUFFER
        ),
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
        "bufferView": view(
            struct.pack(f"<{len(indices)}H", *indices), ELEMENT_ARRAY_BUFFER
        ),
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
            {
                "sampler": len(samplers),
                "target": {"node": NODE_OF[joint], "path": path},
            }
        )
        samplers.append(
            {"input": time_a, "output": value_a, "interpolation": "LINEAR"}
        )
    return {"name": name, "samplers": samplers, "channels": wired}


# ── Nodes ─────────────────────────────────────────────────────────────────
#
# Node 0 is the skinned mesh. glTF says its transform is ignored for skinning,
# so it is the identity here and the scene's own `Transform` places the walker.
NODE_MESH = 0
NODE_OF = {name: 1 + i for i, name in enumerate(NAMES)}

nodes = [{"name": "Walker", "mesh": 0, "skin": 0}]
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
        "generator": "forge examples/meshes/make_rigged_walker.py",
    },
    "scene": 0,
    "scenes": [{"nodes": [NODE_MESH, NODE_OF["Hips"]]}],
    "nodes": nodes,
    "skins": [
        {
            "name": "WalkerRig",
            "inverseBindMatrices": INVERSE_BIND_A,
            "skeleton": NODE_OF["Hips"],
            "joints": [NODE_OF[name] for name in NAMES],
        }
    ],
    "meshes": [
        {
            "name": "WalkerMesh",
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
    "animations": [clip("Walk", walk_curves()), clip("Idle", idle_curves())],
    "accessors": accessors,
    "bufferViews": views,
    "buffers": [
        {
            "byteLength": len(blob),
            "uri": "data:application/octet-stream;base64,"
            + base64.b64encode(bytes(blob)).decode("ascii"),
        }
    ],
}

out = HERE / "rigged_walker.gltf"
out.write_text(json.dumps(document, indent=2) + "\n")
print(
    f"wrote {out} "
    f"({len(positions)} vertices, {len(indices) // 3} triangles, "
    f"{len(NAMES)} joints, {len(document['animations'])} clips)"
)
