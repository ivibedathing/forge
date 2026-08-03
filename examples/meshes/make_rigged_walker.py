#!/usr/bin/env python3
"""Generate `rigged_walker.gltf`: the showcase tour's rigged character.

Text glTF with a base64-embedded buffer, like `rigged_arm.gltf` beside it and
for the same reason — a binary blob nobody can diff is what invariant 1 exists
to keep out of the repo, and a generated asset can be regenerated rather than
trusted.

`rigged_arm.gltf` is the *fixture*: three joints in a chain, the smallest thing
that can prove a palette composes. This is the *character*: sixteen joints in a
tree with two branches per side, a locomotion clip that loops, and a mesh whose
limbs pass each other — which is the case a single chain cannot pose wrong.

What it carries, and why each piece is here:

* **A skeleton with branches.** `Hips` roots a spine to the head and two legs;
  the chest roots two arms, each with an elbow. A chain resolves parents in
  whatever order it is written in; a tree does not, which is what exercises
  `joint_globals`' parents-before-children resolution on real data.
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

The gait is built from the walk cycle's *phases* rather than from one sine per
joint. The first version of this file bent the knee on a single offset sine,
which put peak flexion in mid-stance — the planted leg buckled and the swinging
leg stayed straight, the exact opposite of a walk. The curves below name the
events (heel strike, mid-stance, toe-off, mid-swing) and place each bend
against them, sampled densely enough that the shapes survive linear
interpolation.

    python3 examples/meshes/make_rigged_walker.py
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
    ("Chest", "Hips", (0.00, 1.22, 0.00)),
    ("Neck", "Chest", (0.00, 1.47, 0.00)),
    ("Head", "Neck", (0.00, 1.58, 0.00)),
    ("ArmL", "Chest", (0.23, 1.41, 0.00)),
    ("ElbowL", "ArmL", (0.27, 1.14, -0.01)),
    ("HandL", "ElbowL", (0.29, 0.89, -0.05)),
    ("ArmR", "Chest", (-0.23, 1.41, 0.00)),
    ("ElbowR", "ArmR", (-0.27, 1.14, -0.01)),
    ("HandR", "ElbowR", (-0.29, 0.89, -0.05)),
    ("LegL", "Hips", (0.10, 0.90, 0.00)),
    ("KneeL", "LegL", (0.10, 0.48, 0.00)),
    ("FootL", "KneeL", (0.10, 0.07, 0.00)),
    ("LegR", "Hips", (-0.10, 0.90, 0.00)),
    ("KneeR", "LegR", (-0.10, 0.48, 0.00)),
    ("FootR", "KneeR", (-0.10, 0.07, 0.00)),
]

NAMES = [name for name, _, _ in JOINTS]
INDEX = {name: i for i, name in enumerate(NAMES)}
BIND = {name: position for name, _, position in JOINTS}
PARENT = {name: parent for name, parent, _ in JOINTS}

# ── The mesh ──────────────────────────────────────────────────────────────
#
# One tapered box per bone, swept from the parent joint to the child, plus
# blocks for the parts that do not deform. Each bone's vertices blend the two
# joints it spans, which is what makes an elbow bend instead of shearing: at
# the joint itself the weights are half and half, so the surface folds rather
# than creasing at a hard boundary.
#
# Cross-sections are rectangles, not squares — `(half-width, half-depth)` per
# end — because a torso as deep as it is wide reads as a crate with legs. The
# first version of this mesh was exactly that crate.
#
# `(from, to, (w, d) at from, (w, d) at to)`.
BONES = [
    ("Hips", "Chest", (0.150, 0.095), (0.180, 0.105)),
    ("Chest", "Neck", (0.180, 0.105), (0.110, 0.080)),
    ("Neck", "Head", (0.048, 0.048), (0.052, 0.052)),
    ("ArmL", "ElbowL", (0.052, 0.052), (0.043, 0.043)),
    ("ElbowL", "HandL", (0.041, 0.041), (0.033, 0.033)),
    ("ArmR", "ElbowR", (0.052, 0.052), (0.043, 0.043)),
    ("ElbowR", "HandR", (0.041, 0.041), (0.033, 0.033)),
    ("LegL", "KneeL", (0.078, 0.082), (0.060, 0.064)),
    ("KneeL", "FootL", (0.056, 0.060), (0.044, 0.048)),
    ("LegR", "KneeR", (0.078, 0.082), (0.060, 0.064)),
    ("KneeR", "FootR", (0.056, 0.060), (0.044, 0.048)),
]

# Blocks rigid to one joint. A skull, a boot and a hand do not deform, and a
# single-joint span is also the case that proves a weight of exactly 1 in slot
# 0 with three zeroes behind it round-trips.
#
# `(joint, offset from the joint, half extents)`.
BLOCKS = [
    ("Head", (0.00, 0.085, 0.010), (0.095, 0.105, 0.090)),
    # A boot reaches forward of its ankle — legs that end in flat stumps at
    # the ground are the single biggest reason the old walker read as a figure
    # on stilts.
    ("FootL", (0.00, -0.035, -0.060), (0.055, 0.038, 0.105)),
    ("FootR", (0.00, -0.035, -0.060), (0.055, 0.038, 0.105)),
    ("HandL", (0.005, -0.040, -0.010), (0.035, 0.050, 0.035)),
    ("HandR", (-0.005, -0.040, -0.010), (0.035, 0.050, 0.035)),
    # Shoulder caps, bridging the gap between the tapering chest and the arm
    # sweeps that start outboard of it.
    ("Chest", (0.195, 0.185, 0.00), (0.065, 0.055, 0.062)),
    ("Chest", (-0.195, 0.185, 0.00), (0.065, 0.055, 0.062)),
    # A pelvis under the torso sweep's floor, so the hip yaw never opens a
    # seam of daylight between the waist and the thigh tops.
    ("Hips", (0.00, -0.030, 0.00), (0.152, 0.075, 0.098)),
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
        w, d = half
        centre = lerp(origin, tip, t)
        return [
            tuple(
                centre[axis] + x * w * right[axis] + y * d * up[axis]
                for axis in range(3)
            )
            for x, y in CORNERS
        ]

    def half_at(t):
        return (
            half_start[0] + (half_end[0] - half_start[0]) * t,
            half_start[1] + (half_end[1] - half_start[1]) * t,
        )

    for segment in range(SEGMENTS):
        t0 = segment / SEGMENTS
        t1 = (segment + 1) / SEGMENTS
        r0 = ring(t0, half_at(t0))
        r1 = ring(t1, half_at(t1))
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
# Sampled rather than key-posed, and the last sample is the first, computed
# rather than copied, so the loop seams exactly and nothing drifts if the
# amplitudes are retuned.
#
# 24 frames rather than 16: the knee's swing bump is a fifth of the cycle
# wide, and 16 linear segments visibly facet it.

FRAMES = 24
WALK_PERIOD = 1.0
IDLE_PERIOD = 4.0


def qmul(a, b):
    """Compose two quaternions (apply `b`, then `a`), as (x, y, z, w)."""
    ax, ay, az, aw = a
    bx, by, bz, bw = b
    return (
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    )


def bump(s, centre, width):
    """A smooth compact pulse on the unit circle: 1 at `centre`, 0 outside
    `centre ± width`, cosine-squared between. The building block that lets a
    gait event (a push-off, a heel strike) happen *at a phase* and nowhere
    else — a plain sine leaks its bend into the whole cycle, which is how the
    old walker's planted knee came to buckle."""
    d = (s - centre + 0.5) % 1.0 - 0.5
    if abs(d) >= width:
        return 0.0
    return math.cos(math.pi * d / (2.0 * width)) ** 2


def plateau(s, centre, hold, fade):
    """1 across `centre ± hold`, cosine-fading to 0 by `centre ± (hold+fade)`.
    The stance window: full ankle compensation while the foot bears weight,
    released smoothly on either side."""
    d = abs((s - centre + 0.5) % 1.0 - 0.5)
    if d <= hold:
        return 1.0
    if d >= hold + fade:
        return 0.0
    return math.cos(math.pi * (d - hold) / (2.0 * fade)) ** 2


# The walk cycle, phase-normalised: `s` runs 0→1 over one full cycle (two
# steps). For the *left* leg: thigh max forward (heel strike) at s = 0.25,
# stance through s = 0.25…0.75, toe-off at 0.75, swing through 0.75…1.25.
# The right leg is the same curves at s + 0.5.

THIGH_SWING = 27.0  # degrees from vertical at the stride's extremes


def thigh(s):
    """A sine flattened toward a triangle by its third harmonic, so the thigh
    sweeps through stance at near-constant speed. The stride-driven locomotion
    system converts ground covered into phase at one fixed rate; the closer
    the planted ankle's sweep is to linear, the less the foot skates — a pure
    sine leaves ~1.3 cm of slip a step, the flattened one under half that."""
    phi = 2.0 * math.pi * s
    return THIGH_SWING * (math.sin(phi) - math.sin(3.0 * phi) / 9.0) / (10.0 / 9.0)


def knee_flex(s):
    """Degrees of knee bend (always backwards). Nearly straight through
    stance, a small give as weight lands just after heel strike, and a big
    flexion bump in mid-swing that lifts the heel and clears the ground —
    which is *when* a knee bends in a walk; how much was never the problem."""
    return 6.0 + 58.0 * bump(s, 0.85, 0.17) + 4.0 * bump(s, 0.36, 0.08)


def ankle_pitch(s):
    """Degrees of toes-up. Through stance the ankle cancels the leg's own
    rotation so the sole stays on the ground; around it, the events: toes up
    to meet the heel strike, toes down pushing off, toes up again clearing
    the ground in swing."""
    stance = -(thigh(s) - knee_flex(s)) * plateau(s, 0.50, 0.15, 0.14)
    return (
        stance
        + 10.0 * bump(s, 0.25, 0.08)
        - 16.0 * bump(s, 0.74, 0.10)
        + 6.0 * bump(s, 0.94, 0.12)
    )


def walk_curves():
    """Every animated channel of `Walk`, as `(joint, path, [values])`.

    Two strides per cycle — the left leg leads, the right follows half a
    period later — with the arms counter-swinging over always-bent elbows,
    the pelvis yawing with the stride and rolling off the swing-side hip, and
    the chest giving the yaw back so the shoulders counter the hips. The knee
    only ever bends backwards, because a knee that hinges the other way is
    the single most obvious wrongness a character can have.
    """
    times = [WALK_PERIOD * i / FRAMES for i in range(FRAMES + 1)]
    samples = [i / FRAMES for i in range(FRAMES + 1)]

    hips_t = []
    hips_r = []
    for s in samples:
        # Two bobs per cycle: highest over each straight planted leg at
        # mid-stance, lowest at each heel strike, and a sway out over
        # whichever foot is bearing the weight.
        y = BIND["Hips"][1] - 0.022 + 0.020 * math.cos(4.0 * math.pi * s)
        hips_t.append((0.0, y, 0.0))
        # The pelvis leads the stride: the stepping side's hip swings
        # forward. Yaw only — a hip roll or sway here rides down the planted
        # leg and skates the foot the locomotion system is holding still, so
        # the weight shift lives on the chest, where only the upper body
        # inherits it.
        hips_r.append(quat_y(7.0 * math.sin(2.0 * math.pi * s)))

    def leg(offset):
        return [quat_x(thigh(s + offset)) for s in samples]

    def knee(offset):
        return [quat_x(-knee_flex(s + offset)) for s in samples]

    def foot(offset):
        return [quat_x(ankle_pitch(s + offset)) for s in samples]

    def arm(offset):
        # Counter-swing, from a shoulder that hangs a touch forward.
        return [
            quat_x(3.0 - 22.0 * math.sin(2.0 * math.pi * (s + offset)))
            for s in samples
        ]

    def elbow(offset):
        # Never straight — a locked elbow is a mannequin's — and flexing
        # further as the arm swings forward.
        return [
            quat_x(14.0 + 12.0 * max(0.0, -math.sin(2.0 * math.pi * (s + offset))))
            for s in samples
        ]

    chest = [
        # The shoulders counter the pelvis: the chest gives back the hips'
        # yaw and more, over a slight constant forward lean into the walk —
        # and carries the weight shift, rolling out over whichever leg is
        # planted, so the sway reads in the shoulders without touching the
        # feet.
        qmul(
            qmul(
                quat_x(-4.0),
                quat_y(-11.0 * math.sin(2.0 * math.pi * s)),
            ),
            quat_z(-2.5 * math.cos(2.0 * math.pi * s)),
        )
        for s in samples
    ]
    head = [
        # The head undoes what is left of the twist and the lean, so the
        # gaze holds the horizon while everything under it works.
        qmul(quat_x(4.0), quat_y(4.0 * math.sin(2.0 * math.pi * s)))
        for s in samples
    ]

    return times, [
        ("Hips", "translation", hips_t),
        ("Hips", "rotation", hips_r),
        ("Chest", "rotation", chest),
        ("Head", "rotation", head),
        ("LegL", "rotation", leg(0.0)),
        ("KneeL", "rotation", knee(0.0)),
        ("FootL", "rotation", foot(0.0)),
        ("LegR", "rotation", leg(0.5)),
        ("KneeR", "rotation", knee(0.5)),
        ("FootR", "rotation", foot(0.5)),
        # Arms swing opposite the leg on the same side.
        ("ArmL", "rotation", arm(0.5)),
        ("ElbowL", "rotation", elbow(0.5)),
        ("ArmR", "rotation", arm(0.0)),
        ("ElbowR", "rotation", elbow(0.0)),
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
                (0.0, BIND["Hips"][1] + 0.010 * math.sin(phase(t)), 0.0)
                for t in times
            ],
        ),
        (
            "Chest",
            "rotation",
            [quat_x(-2.0 - 2.0 * math.sin(phase(t))) for t in times],
        ),
        # The head scans, slowly, at half the breathing rate — the one thing
        # that keeps a standing figure from reading as a statue.
        ("Head", "rotation", [quat_y(7.0 * math.sin(phase(t) / 2)) for t in times]),
        ("ArmL", "rotation", [quat_x(2.0 + 2.5 * math.sin(phase(t))) for t in times]),
        (
            "ArmR",
            "rotation",
            [quat_x(2.0 + 2.5 * math.sin(phase(t) + 0.4)) for t in times],
        ),
        # Elbows at a resting bend, breathing a little with the arms.
        (
            "ElbowL",
            "rotation",
            [quat_x(12.0 + 1.5 * math.sin(phase(t))) for t in times],
        ),
        (
            "ElbowR",
            "rotation",
            [quat_x(12.0 + 1.5 * math.sin(phase(t) + 0.4)) for t in times],
        ),
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
# so it is the identity here and the scene's own `Transform` places the walker.
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
            {
                "sampler": len(samplers),
                "target": {"node": NODE_OF[joint], "path": path},
            }
        )
        samplers.append(
            {"input": time_a, "output": value_a, "interpolation": "LINEAR"}
        )
    return {"name": name, "samplers": samplers, "channels": wired}


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
    "accessors": buffer.accessors,
    "bufferViews": buffer.views,
    "buffers": buffer.buffers(),
}

out = HERE / "rigged_walker.gltf"
out.write_text(json.dumps(document, indent=2) + "\n")
print(
    f"wrote {out} "
    f"({len(positions)} vertices, {len(indices) // 3} triangles, "
    f"{len(NAMES)} joints, {len(document['animations'])} clips)"
)
