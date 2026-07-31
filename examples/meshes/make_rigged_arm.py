#!/usr/bin/env python3
"""Generate `rigged_arm.gltf`: the fixture M30's skeletal animation is tested against.

Text glTF with a base64-embedded buffer, exactly like `pyramid.gltf` and
`textured_quad.gltf` beside it and for the same reason — a binary blob nobody
can diff is what invariant 1 exists to keep out of the repo, and a generated
fixture can be regenerated rather than trusted.

What it carries, and why each piece is here:

* **Three joints in a chain** — `Shoulder` at the origin, `Elbow` one metre up,
  `Hand` one metre above that — so a pose has a hierarchy to compose and
  `list-joints` has something with a shape to report.
* **A tapered column skinned across them**, each ring's weights blending the
  two joints it sits between. Four influences per vertex is what the engine
  supports; this file uses two, which is what a well-behaved export looks like.
* **A clip named `Wave`** that bends the elbow and back over one second. The
  hand is somewhere different at t=0.5 than at t=0, which is the milestone
  proving its own claim without a pixel.
* **A channel targeting a node the skin does not use** (`Marker`). glTF allows
  it, sampling ignores it, and `engine list-animations` names it — an ignored
  channel nothing reports is invisible.
* **A `Sway` clip on the shoulder**, so `path#Clip` has more than one clip to
  choose between and `unknown_clip` has something to suggest from.

    python3 examples/meshes/make_rigged_arm.py
"""

import base64
import json
import math
import pathlib
import struct

HERE = pathlib.Path(__file__).resolve().parent

# The chain, bottom to top. Positions are the joints' bind-pose world Y.
JOINTS = [("Shoulder", 0.0), ("Elbow", 1.0), ("Hand", 2.0)]

HEIGHT = 2.0
SEGMENTS = 8
BASE_HALF = 0.18
TIP_HALF = 0.10


def half_width(y):
    return BASE_HALF + (TIP_HALF - BASE_HALF) * (y / HEIGHT)


def weights_at(y):
    """The two joints a height sits between, and how much of each.

    Returned as the four-slot form glTF stores, with the unused slots zeroed —
    a zero weight is how a vertex says "fewer than four influences".
    """
    if y <= JOINTS[1][1]:
        upper = y / JOINTS[1][1]
        return [0, 1, 0, 0], [1.0 - upper, upper, 0.0, 0.0]
    upper = (y - JOINTS[1][1]) / (JOINTS[2][1] - JOINTS[1][1])
    return [1, 2, 0, 0], [1.0 - upper, upper, 0.0, 0.0]


# ── Geometry ──────────────────────────────────────────────────────────────
# A square column swept up +Y, flat-shaded: every face owns its vertices, so
# the four sides keep their own normals instead of averaging into a rounded
# corner. Counter-clockwise winding, matching the engine's front face.

positions = []
normals = []
joints = []
weights = []
indices = []

# The four corners of a ring, counter-clockwise seen from +Y.
CORNERS = [(-1, -1), (1, -1), (1, 1), (-1, 1)]


def corner(index, y):
    h = half_width(y)
    x, z = CORNERS[index]
    return (x * h, y, z * h)


def push_face(quad, normal):
    """One quad as two triangles, with its own copies of the four vertices."""
    base = len(positions)
    for point in quad:
        positions.append(point)
        normals.append(normal)
        j, w = weights_at(point[1])
        joints.append(j)
        weights.append(w)
    indices.extend([base, base + 1, base + 2, base, base + 2, base + 3])


for side in range(4):
    a, b = side, (side + 1) % 4
    # The outward normal of this side, from the untapered cross-section — the
    # taper tilts the face a few degrees and flat shading does not care.
    mid_x = (CORNERS[a][0] + CORNERS[b][0]) / 2
    mid_z = (CORNERS[a][1] + CORNERS[b][1]) / 2
    length = math.hypot(mid_x, mid_z)
    normal = (mid_x / length, 0.0, mid_z / length)

    for segment in range(SEGMENTS):
        y0 = HEIGHT * segment / SEGMENTS
        y1 = HEIGHT * (segment + 1) / SEGMENTS
        push_face(
            [corner(a, y0), corner(b, y0), corner(b, y1), corner(a, y1)],
            normal,
        )

push_face([corner(i, HEIGHT) for i in (0, 1, 2, 3)], (0.0, 1.0, 0.0))
push_face([corner(i, 0.0) for i in (3, 2, 1, 0)], (0.0, -1.0, 0.0))


# ── The clips ─────────────────────────────────────────────────────────────


def quat_x(degrees):
    half = math.radians(degrees) / 2
    return (math.sin(half), 0.0, 0.0, math.cos(half))


def quat_z(degrees):
    half = math.radians(degrees) / 2
    return (0.0, 0.0, math.sin(half), math.cos(half))


IDENTITY_QUAT = (0.0, 0.0, 0.0, 1.0)

WAVE_TIMES = [0.0, 0.5, 1.0]
WAVE_ROTATIONS = [IDENTITY_QUAT, quat_x(-60.0), IDENTITY_QUAT]

# The channel nothing samples: a node in the scene that is in no skin.
MARKER_TIMES = [0.0, 1.0]
MARKER_TRANSLATIONS = [(0.0, 0.0, 0.0), (0.5, 0.0, 0.0)]

SWAY_TIMES = [0.0, 1.0, 2.0]
SWAY_ROTATIONS = [IDENTITY_QUAT, quat_z(20.0), IDENTITY_QUAT]


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

position_view = view(floats(flat(positions)), ARRAY_BUFFER)
normal_view = view(floats(flat(normals)), ARRAY_BUFFER)
joint_view = view(
    struct.pack(f"<{len(joints) * 4}H", *flat(joints)),
    ARRAY_BUFFER,
)
weight_view = view(floats(flat(weights)), ARRAY_BUFFER)
index_view = view(struct.pack(f"<{len(indices)}H", *indices), ELEMENT_ARRAY_BUFFER)

# Inverse bind matrices: skin space → each joint's bind space. The chain is a
# pure translation up +Y, so each is the inverse of that translation. Column
# major, as glTF stores matrices.
inverse_binds = []
for _, y in JOINTS:
    inverse_binds.extend(
        [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0.0, -y, 0.0, 1]
    )
inverse_bind_view = view(floats([float(v) for v in inverse_binds]))

wave_time_view = view(floats(WAVE_TIMES))
wave_rotation_view = view(floats(flat(WAVE_ROTATIONS)))
marker_time_view = view(floats(MARKER_TIMES))
marker_translation_view = view(floats(flat(MARKER_TRANSLATIONS)))
sway_time_view = view(floats(SWAY_TIMES))
sway_rotation_view = view(floats(flat(SWAY_ROTATIONS)))

FLOAT, UNSIGNED_SHORT = 5126, 5123


def bounds(rows):
    columns = list(zip(*rows))
    return [min(c) for c in columns], [max(c) for c in columns]


position_min, position_max = bounds(positions)

accessors = [
    {
        "bufferView": position_view,
        "componentType": FLOAT,
        "count": len(positions),
        "type": "VEC3",
        "min": list(position_min),
        "max": list(position_max),
    },
    {
        "bufferView": normal_view,
        "componentType": FLOAT,
        "count": len(normals),
        "type": "VEC3",
    },
    {
        "bufferView": joint_view,
        "componentType": UNSIGNED_SHORT,
        "count": len(joints),
        "type": "VEC4",
    },
    {
        "bufferView": weight_view,
        "componentType": FLOAT,
        "count": len(weights),
        "type": "VEC4",
    },
    {
        "bufferView": index_view,
        "componentType": UNSIGNED_SHORT,
        "count": len(indices),
        "type": "SCALAR",
    },
    {
        "bufferView": inverse_bind_view,
        "componentType": FLOAT,
        "count": len(JOINTS),
        "type": "MAT4",
    },
    {
        "bufferView": wave_time_view,
        "componentType": FLOAT,
        "count": len(WAVE_TIMES),
        "type": "SCALAR",
        "min": [min(WAVE_TIMES)],
        "max": [max(WAVE_TIMES)],
    },
    {
        "bufferView": wave_rotation_view,
        "componentType": FLOAT,
        "count": len(WAVE_ROTATIONS),
        "type": "VEC4",
    },
    {
        "bufferView": marker_time_view,
        "componentType": FLOAT,
        "count": len(MARKER_TIMES),
        "type": "SCALAR",
        "min": [min(MARKER_TIMES)],
        "max": [max(MARKER_TIMES)],
    },
    {
        "bufferView": marker_translation_view,
        "componentType": FLOAT,
        "count": len(MARKER_TRANSLATIONS),
        "type": "VEC3",
    },
    {
        "bufferView": sway_time_view,
        "componentType": FLOAT,
        "count": len(SWAY_TIMES),
        "type": "SCALAR",
        "min": [min(SWAY_TIMES)],
        "max": [max(SWAY_TIMES)],
    },
    {
        "bufferView": sway_rotation_view,
        "componentType": FLOAT,
        "count": len(SWAY_ROTATIONS),
        "type": "VEC4",
    },
]

(
    POSITION_A,
    NORMAL_A,
    JOINTS_A,
    WEIGHTS_A,
    INDEX_A,
    INVERSE_BIND_A,
    WAVE_TIME_A,
    WAVE_ROTATION_A,
    MARKER_TIME_A,
    MARKER_TRANSLATION_A,
    SWAY_TIME_A,
    SWAY_ROTATION_A,
) = range(len(accessors))

# Node 0 is the skinned mesh. glTF says its transform is ignored for skinning,
# so it is the identity here and the scene's own Transform places the arm.
NODE_ARM, NODE_SHOULDER, NODE_ELBOW, NODE_HAND, NODE_MARKER = range(5)

document = {
    "asset": {
        "version": "2.0",
        "generator": "forge examples/meshes/make_rigged_arm.py",
    },
    "scene": 0,
    "scenes": [{"nodes": [NODE_ARM, NODE_SHOULDER, NODE_MARKER]}],
    "nodes": [
        {"name": "Arm", "mesh": 0, "skin": 0},
        {"name": "Shoulder", "children": [NODE_ELBOW]},
        {"name": "Elbow", "translation": [0.0, 1.0, 0.0], "children": [NODE_HAND]},
        {"name": "Hand", "translation": [0.0, 1.0, 0.0]},
        {"name": "Marker", "translation": [0.0, 0.0, 0.0]},
    ],
    "skins": [
        {
            "name": "ArmRig",
            "inverseBindMatrices": INVERSE_BIND_A,
            "skeleton": NODE_SHOULDER,
            "joints": [NODE_SHOULDER, NODE_ELBOW, NODE_HAND],
        }
    ],
    "meshes": [
        {
            "name": "ArmMesh",
            "primitives": [
                {
                    "attributes": {
                        "POSITION": POSITION_A,
                        "NORMAL": NORMAL_A,
                        "JOINTS_0": JOINTS_A,
                        "WEIGHTS_0": WEIGHTS_A,
                    },
                    "indices": INDEX_A,
                    "mode": 4,
                }
            ],
        }
    ],
    "animations": [
        {
            "name": "Wave",
            "samplers": [
                {
                    "input": WAVE_TIME_A,
                    "output": WAVE_ROTATION_A,
                    "interpolation": "LINEAR",
                },
                {
                    "input": MARKER_TIME_A,
                    "output": MARKER_TRANSLATION_A,
                    "interpolation": "LINEAR",
                },
            ],
            "channels": [
                {"sampler": 0, "target": {"node": NODE_ELBOW, "path": "rotation"}},
                {"sampler": 1, "target": {"node": NODE_MARKER, "path": "translation"}},
            ],
        },
        {
            "name": "Sway",
            "samplers": [
                {
                    "input": SWAY_TIME_A,
                    "output": SWAY_ROTATION_A,
                    "interpolation": "LINEAR",
                }
            ],
            "channels": [
                {"sampler": 0, "target": {"node": NODE_SHOULDER, "path": "rotation"}}
            ],
        },
    ],
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

out = HERE / "rigged_arm.gltf"
out.write_text(json.dumps(document, indent=2) + "\n")
print(
    f"wrote {out} "
    f"({len(positions)} vertices, {len(indices) // 3} triangles, "
    f"{len(JOINTS)} joints, {len(document['animations'])} clips)"
)
