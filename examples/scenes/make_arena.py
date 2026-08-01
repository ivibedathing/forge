#!/usr/bin/env python3
"""Emit `arena_shooter.json` — the top-down shooter's arena.

The scene is generated for the same reason `make_car_track.py` is: most of it
is repetition the engine has no way to express. A bullet pool is fourteen
identical entities differing only in a name, a drone is nine lines of JSON
repeated ten times, and the arena's cover is a table of positions. Hand-writing
those is how a scene file stops being editable — one number moves and forty
lines have to follow.

Everything a human would actually want to tune lives at the top of this file as
a table. Run it and re-render:

    python3 examples/scenes/make_arena.py
    bin/engine screenshot examples/scenes/arena_shooter.json --out /tmp/a.png

The emitted JSON is the scene — the engine never runs this script, and the file
it writes is a normal, hand-editable scene. This is a convenience, not a second
source of truth.
"""

import json
import os

# ---------------------------------------------------------------------------
# The arena
# ---------------------------------------------------------------------------

HALF = 28.0  # inner half-width of the fighting floor, metres
WALL_H = 3.2
WALL_T = 1.2

PLAYER_START = (0.0, 0.9, 16.0)

# Cover: (x, z, size_x, size_y, size_z). Static, no rigid body — the player and
# the drones collide with it, and it is what makes the arena a place to move
# through rather than an empty square.
CRATES = [
    (-12.0, -16.0, 1.8, 1.8, 1.8),
    (-12.0, -16.0 - 1.9, 1.8, 1.8, 1.8),
    (12.0, -16.0, 1.8, 1.8, 1.8),
    (12.0, -14.1, 1.8, 3.6, 1.8),
    (-18.5, -4.0, 1.8, 1.8, 1.8),
    (18.5, -4.0, 1.8, 3.6, 1.8),
    (-6.5, 8.0, 1.8, 1.8, 1.8),
    (6.5, 8.0, 1.8, 1.8, 1.8),
    (-16.0, 18.5, 1.8, 3.6, 1.8),
    (16.0, 18.5, 1.8, 1.8, 1.8),
    (0.0, 25.0, 1.8, 1.8, 1.8),
]

# Long concrete barriers: (x, z, size_x, size_y, size_z).
BARRIERS = [
    (0.0, -10.0, 12.0, 1.5, 1.3),
    (-15.0, 6.0, 1.3, 1.5, 11.0),
    (15.0, 6.0, 1.3, 1.5, 11.0),
    (0.0, 21.0, 9.0, 1.5, 1.3),
    (-22.0, -12.0, 1.3, 1.5, 8.0),
    (22.0, -12.0, 1.3, 1.5, 8.0),
]

# Explosive barrels. Shooting one kills every drone inside BLAST_RADIUS.
BARRELS = [
    (-9.0, -2.0),
    (9.0, -2.0),
    (-3.5, 15.0),
    (20.0, -21.0),
]

# Floodlights: (x, z). Post + globe + PointLight.
LAMPS = [
    (-21.0, -21.0),
    (21.0, -21.0),
    (-21.0, 21.0),
    (21.0, 21.0),
]

# Drones, in the order they are numbered. The wave a drone belongs to is its
# index // DRONES_PER_WAVE-ish — spelled out here so the layout stays readable.
# Each entry is (x, z, wave). Dormant drones hover at HOVER_PARK metres up and
# drop in when their wave starts, which is why they have no gravity.
DRONES = [
    (-23.0, -23.0, 1),
    (23.0, -23.0, 1),
    (-23.0, 23.0, 1),
    (23.0, 23.0, 1),
    (0.0, -25.0, 2),
    (-25.0, 0.0, 2),
    (25.0, 0.0, 2),
    (-13.0, -25.0, 3),
    (13.0, -25.0, 3),
    (0.0, 25.0, 3),
]

BULLETS = 14  # the pool the script recycles; see scripts/topdown_shooter.rhai

# Trees outside the walls, for something to see past the arena: (x, z, seed).
TREES = [
    (-44.0, -38.0, 3),
    (-52.0, 6.0, 11),
    (-38.0, 46.0, 5),
    (40.0, -44.0, 7),
    (54.0, 2.0, 2),
    (36.0, 44.0, 9),
    (-8.0, -56.0, 4),
    (14.0, 58.0, 6),
]

# Grass at the foot of the plateau: four strips ringing the arena, each one a
# `Meadow` standing on `Landscape`. It is a ring rather than a field because
# the camera looks *down* at the arena from 20 m and the only ground it ever
# sees past the walls is the band immediately outside them — grass further out
# would be plants nobody renders. Entries are (x, z, size_x, size_z, seed).
#
# Density is the number that decides whether this is scenery or a stall: the
# component counts plants per square metre of footprint, so a strip is
# `size_x * size_z * density` plants, and M29's budget (`MAX_MEADOW_TRIANGLES`,
# 8M) is per entity and counted in triangles. Measured at these settings: a
# plant is 64 triangles, a long strip ~20k plants (1.3M triangles) and a short
# one ~12k (0.8M), so each sits well inside its own budget and the four
# together are about 4M — which costs the frame nothing measurable, because
# every one of them is two static buffers and a vertex shader.
MEADOW_DENSITY = 11.0
MEADOWS = [
    (0.0, -40.0, 108.0, 17.0, 21),
    (0.0, 40.0, 108.0, 17.0, 34),
    (-40.0, 0.0, 17.0, 63.0, 47),
    (40.0, 0.0, 17.0, 63.0, 58),
]

CLOUDS = [
    (-60.0, 46.0, -70.0, 1),
    (70.0, 52.0, -40.0, 4),
    (10.0, 58.0, 90.0, 8),
]

# ---------------------------------------------------------------------------
# The HUD
# ---------------------------------------------------------------------------
#
# Four numbers and a string the script has to agree with. Everything else about
# the layout — where the menu sits, how wide the button is, where the health
# label goes — is the engine's business since M31, which is the point of the
# component tree emitted at the bottom of this file.

HEALTH_BAR = (224.0, 20.0)  # the gauge panel
HEALTH_PAD = 3.0  # its padding, which *is* the bezel around the fill
HEALTH_FILL = HEALTH_BAR[0] - 2 * HEALTH_PAD  # a full gauge, in pixels
HEALTH_GOOD = [0.25, 0.85, 0.42]  # the fill at full health; the script fades it
# The reticle is `ui_icon.png` drawn at its own 16 px, and that is not a
# preference: a `HudImage` with no `slice` is all "middle band", and the middle
# band **tiles**. Asking for 32 px of a 16 px icon does not scale it up, it
# draws four of it — which is what the first render showed, a 2x2 of rings.
CROSSHAIR = 16.0

# The menu column's width, which is its widest unwrapped child: thirteen glyphs
# of a 32-pixel title, the 8x8 font advancing one glyph height per character.
MENU_WIDTH = 13 * 32.0
CONTROLS = "WASD MOVE   MOUSE AIM   CLICK FIRE   R RELOAD   ESC PAUSE"


# ---------------------------------------------------------------------------
# Palette (linear RGB, like everything else in this engine)
# ---------------------------------------------------------------------------

FLOOR = [0.150, 0.155, 0.172]
FLOOR_LINE = [0.30, 0.31, 0.33]
FLOOR_PAD = [0.105, 0.115, 0.132]
WALL = [0.275, 0.285, 0.305]
CRATE = [0.300, 0.225, 0.135]
CONCRETE = [0.300, 0.305, 0.325]
DRONE = [0.075, 0.085, 0.105]
DRONE_GLOW = [1.0, 0.12, 0.08]
BARREL = [0.430, 0.095, 0.055]
# The barrels are the one surface whose map carries its own hue, so their tint
# is near-white: it must not repaint the hazard band.
BARREL_TINT = [0.95, 0.93, 0.91]


# ---------------------------------------------------------------------------
# Surfaces
# ---------------------------------------------------------------------------
#
# Every map here is generated by `examples/textures/make_textures.py`. They are
# near-neutral and bright on purpose: `albedo_map` is **multiplied** by
# `albedo`, so a map carrying its own strong colour could only ever be tinted
# down toward black. The map carries the relief and the palette above carries
# the hue. `barrel.png` is the one deliberate exception, and says why in its own
# docstring — a hazard stripe is a colour contrast, not a brightness one.

DECK = {
    "albedo_map": "../textures/deck.png",
    "normal_map": "../textures/deck_normal.png",
    "orm_map": "../textures/deck_orm.png",
}
CONCRETE_MAPS = {
    "albedo_map": "../textures/concrete.png",
    "normal_map": "../textures/concrete_normal.png",
}
PANEL_MAPS = {
    "normal_map": "../textures/plate_normal.png",
    "orm_map": "../textures/plate_orm.png",
}
BARREL_MAPS = {
    "albedo_map": "../textures/barrel.png",
    "normal_map": "../textures/barrel_normal.png",
}

SLAB = 3.1  # metres of floor per tile of `deck.png`
PANEL = 1.6  # metres of wall per pressed panel
AGGREGATE = 1.6  # metres of barrier per tile of `concrete.png`


def top_uv(sx, sz, metres=SLAB):
    """Tiling for a box's **top** face, in tiles of `metres`.

    The arguments swap, and that is not a typo: `builtin:cube`'s +Y face is
    `quad(+Y, +Z, +X)`, so `u` runs along local +Z and `v` along +X. Passing
    `(sx, sz)` straight through tiles a 56 × 2.6 m lane the wrong way round and
    draws stretched bands where the slabs should be.
    """
    return [round(sz / metres, 3), round(sx / metres, 3)]


def side_uv(u_metres, v_metres, metres=PANEL):
    """Tiling for one named *side* face, given how many metres each of its own
    axes spans.

    A cube's six faces do not agree on which way `u` runs — it is vertical on
    ±X and horizontal on ±Z, and the two faces *within* each pair are
    transposed against each other as well. So a box's tiling is a property of
    the face you care about, not of the box: the four perimeter walls each get
    their own value here, chosen for the face that points into the arena, and
    the outward faces nobody plays against are left to stretch.
    """
    return [round(u_metres / metres, 3), round(v_metres / metres, 3)]


def t(position=None, rotation=None, scale=None):
    c = {"type": "Transform"}
    if position is not None:
        c["position"] = [round(float(v), 4) for v in position]
    if rotation is not None:
        c["rotation"] = [round(float(v), 4) for v in rotation]
    if scale is not None:
        c["scale"] = [round(float(v), 4) for v in scale]
    return c


def mat(albedo, roughness=0.85, metallic=0.0, emissive=None, maps=None, uv=None, **extra):
    c = {"type": "Material", "albedo": albedo, "roughness": roughness}
    if metallic:
        c["metallic"] = metallic
    if emissive is not None:
        c["emissive"] = emissive
    if maps:
        c.update(maps)
    if uv is not None:
        c["uv_scale"] = uv
    c.update(extra)
    return c


def box(name, x, y, z, sx, sy, sz, albedo, roughness=0.85, friction=0.9, material=None):
    """A static cuboid: mesh and collider are the same box, so what you see is
    what you bump into."""
    return {
        "name": name,
        "components": [
            t(position=(x, y, z), scale=(sx, sy, sz)),
            {"type": "Mesh", "asset": "builtin:cube"},
            material if material is not None else mat(albedo, roughness),
            {
                "type": "Collider",
                "shape": "cuboid",
                "half_extents": [0.5, 0.5, 0.5],
                "friction": friction,
            },
        ],
    }


def decal(name, x, z, sx, sz, albedo):
    """A painted floor marking: a 6 cm slab, no collider, nothing to trip on.

    The paint takes the deck's *normal* map and not its albedo — paint follows
    the slab it was rolled onto, and lining a second copy of the deck's grime up
    with the one underneath is not possible when the two boxes have different
    sizes. Its `uv_scale` is the floor's, computed from this slab's own extent,
    so the relief runs at the same 3.1 m as the concrete around it.
    """
    return {
        "name": name,
        "components": [
            t(position=(x, 0.03, z), scale=(sx, 0.06, sz)),
            {"type": "Mesh", "asset": "builtin:cube"},
            mat(
                albedo,
                0.72,
                maps={"normal_map": "../textures/deck_normal.png"},
                uv=top_uv(sx, sz),
                normal_strength=0.55,
            ),
        ],
    }


def entities():
    out = []

    # --- the ground the arena stands on ------------------------------------
    # A flat floor, deliberately: the player and the drones slide around it at
    # speed, and a Terrain trimesh under a sliding body is where this engine's
    # internal-edge bug lives (see CLAUDE.md, M23). The relief is scenery only,
    # sits below the floor, and carries no collider.
    # The arena is a plateau, not a plane: a 6 m block whose top face is y=0.
    # The landscape below is strictly lower than that (see `Landscape`), which
    # is the whole reason the terrain can be scenery without ever poking a hill
    # up through the fighting floor.
    out.append(
        {
            "name": "Floor",
            "components": [
                t(position=(0.0, -3.0, 0.0), scale=(2 * HALF + 6, 6.0, 2 * HALF + 6)),
                {"type": "Mesh", "asset": "builtin:cube"},
                mat(FLOOR, 0.88, maps=DECK, uv=top_uv(2 * HALF + 6, 2 * HALF + 6)),
                {
                    "type": "Collider",
                    "shape": "cuboid",
                    "half_extents": [0.5, 0.5, 0.5],
                    "friction": 0.9,
                },
            ],
        }
    )
    out.append(
        {
            "name": "Landscape",
            "components": [
                t(position=(0.0, -7.0, 0.0), scale=(520.0, 1.0, 520.0)),
                {
                    "type": "Terrain",
                    "segments": 220,
                    "seed": 12,
                    "height": 6.0,
                    "feature_scale": 70.0,
                    "octaves": 5,
                    "persistence": 0.5,
                    "warp": 0.6,
                    "texture_scale": 3.0,
                    "color_variation": 0.35,
                    "bump": 0.3,
                    "layers": [
                        {"albedo": [0.098, 0.132, 0.062], "roughness": 0.96},
                        {
                            "albedo": [0.145, 0.150, 0.078],
                            "roughness": 0.96,
                            "height_range": [2.4, 90.0],
                            "height_blend": 1.6,
                            "slope_range": [0.0, 20.0],
                            "slope_blend": 9.0,
                            "noise": 0.9,
                        },
                        {
                            "albedo": [0.108, 0.080, 0.052],
                            "roughness": 0.95,
                            "slope_range": [26.0, 90.0],
                            "slope_blend": 8.0,
                            "noise": 0.9,
                        },
                    ],
                },
            ],
        }
    )

    # --- floor markings -----------------------------------------------------
    span = 2 * HALF
    out.append(decal("PaintLaneX", 0.0, 0.0, span, 2.6, FLOOR_LINE))
    out.append(decal("PaintLaneZ", 0.0, 0.0, 2.6, span, FLOOR_LINE))
    for i, (px, pz) in enumerate(
        [(-15.0, -15.0), (15.0, -15.0), (-15.0, 15.0), (15.0, 15.0)]
    ):
        out.append(decal(f"PaintPad{i + 1}", px, pz, 13.0, 13.0, FLOOR_PAD))
    out.append(decal("PaintStart", PLAYER_START[0], PLAYER_START[2], 5.0, 5.0, FLOOR_LINE))

    # --- perimeter ----------------------------------------------------------
    # Pressed steel panelling, tiled for the face that points *inward* — that
    # face is a different one of the cube's six on each of the four walls, and
    # they disagree about which way `u` runs. See `side_uv`.
    edge = HALF + WALL_T / 2
    long_side = 2 * HALF + 2 * WALL_T
    wall_faces = {
        "WallNorth": side_uv(long_side, WALL_H),  # inner face +Z: u along X
        "WallSouth": side_uv(WALL_H, long_side),  # inner face -Z: u along Y
        "WallWest": side_uv(WALL_H, long_side),  # inner face +X: u along Y
        "WallEast": side_uv(long_side, WALL_H),  # inner face -X: u along Z
    }

    def wall(name, x, y, z, sx, sy, sz):
        return box(
            name, x, y, z, sx, sy, sz, WALL,
            material=mat(WALL, 0.7, 0.25, maps=PANEL_MAPS, uv=wall_faces[name]),
        )

    out.append(wall("WallNorth", 0.0, WALL_H / 2, -edge, long_side, WALL_H, WALL_T))
    out.append(wall("WallSouth", 0.0, WALL_H / 2, edge, long_side, WALL_H, WALL_T))
    out.append(wall("WallWest", -edge, WALL_H / 2, 0.0, WALL_T, WALL_H, long_side))
    out.append(wall("WallEast", edge, WALL_H / 2, 0.0, WALL_T, WALL_H, long_side))

    # --- cover --------------------------------------------------------------
    # Every crate is the same wood at the same tiling, so they share one
    # `materials/*.json` rather than carrying eleven copies of it. The barriers
    # cannot: `Material.asset` is exclusive with every other field, and each
    # barrier needs its own `uv_scale` because a 12 m one and an 11 m one are
    # long along different axes.
    for i, (x, z, sx, sy, sz) in enumerate(CRATES):
        out.append(
            box(
                f"Crate{i + 1:02d}", x, sy / 2, z, sx, sy, sz, CRATE, 0.9,
                material={"type": "Material", "asset": "../materials/crate_wood.json"},
            )
        )
    for i, (x, z, sx, sy, sz) in enumerate(BARRIERS):
        # The long faces are ±Z on an X-long barrier and ±X on a Z-long one, and
        # `u` is horizontal on the first pair and vertical on the second.
        uv = side_uv(sx, sy, AGGREGATE) if sx >= sz else side_uv(sy, sz, AGGREGATE)
        out.append(
            box(
                f"Barrier{i + 1}", x, sy / 2, z, sx, sy, sz, CONCRETE, 0.8,
                material=mat(CONCRETE, 0.8, maps=CONCRETE_MAPS, uv=uv),
            )
        )

    # --- explosive barrels --------------------------------------------------
    # Dynamic so a blast throws them, breakable so a bullet opens them, and the
    # script turns the break into a radial kill. The collider is a cuboid under
    # a cylinder mesh: Transform.scale scales collider shapes, and a box takes
    # a non-uniform scale without argument.
    for i, (x, z) in enumerate(BARRELS):
        out.append(
            {
                "name": f"Barrel{i + 1}",
                "components": [
                    t(position=(x, 0.62, z), scale=(0.86, 1.24, 0.86)),
                    {"type": "Mesh", "asset": "builtin:cylinder"},
                    # `barrel.png` carries its own colour, so the tint here is
                    # near-white rather than the red the untextured barrel used
                    # — the map is the paint. The emissive is dialled back for
                    # the same reason: the hazard band now does the job of
                    # reading "this one explodes" from across the arena, and a
                    # strong glow over it only washes the stripes out.
                    mat(
                        BARREL_TINT, 0.45, 0.25, [0.08, 0.012, 0.0],
                        maps=BARREL_MAPS, uv=[1.0, 1.0],
                    ),
                    {"type": "RigidBody", "body": "dynamic", "linear_damping": 0.4},
                    {
                        "type": "Collider",
                        "shape": "cuboid",
                        "half_extents": [0.42, 0.5, 0.42],
                        "density": 60.0,
                        "friction": 0.7,
                    },
                    {
                        "type": "Breakable",
                        "fragments": [
                            {
                                "mesh": "builtin:cube",
                                "offset": [ox * 0.3, oy * 0.3, oz * 0.3],
                                "scale": [0.42, 0.5, 0.42],
                                "half_extents": [0.5, 0.5, 0.5],
                                "density": 60.0,
                            }
                            for ox, oy, oz in [
                                (-1, -1, -1),
                                (1, -1, 1),
                                (-1, 1, 1),
                                (1, 1, -1),
                            ]
                        ],
                    },
                ],
            }
        )

    # --- floodlights --------------------------------------------------------
    for i, (x, z) in enumerate(LAMPS):
        out.append(
            {
                "name": f"LampPost{i + 1}",
                "components": [
                    t(position=(x, 3.0, z), scale=(0.22, 6.0, 0.22)),
                    {"type": "Mesh", "asset": "builtin:cylinder"},
                    mat([0.09, 0.10, 0.11], 0.5, 0.6, maps=PANEL_MAPS, uv=[1.0, 3.75]),
                    {
                        "type": "Collider",
                        "shape": "cuboid",
                        "half_extents": [0.5, 0.5, 0.5],
                        "friction": 0.6,
                    },
                ],
            }
        )
        out.append(
            {
                "name": f"LampGlobe{i + 1}",
                "components": [
                    t(position=(x, 6.1, z), scale=(0.3, 0.3, 0.3)),
                    {"type": "Mesh", "asset": "builtin:sphere"},
                    mat([0.9, 0.86, 0.7], 0.3, 0.0, [1.0, 0.9, 0.66]),
                ],
            }
        )
        out.append(
            {
                "name": f"Lamp{i + 1}",
                "components": [
                    t(position=(x, 6.1, z)),
                    {
                        "type": "PointLight",
                        "color": [1.0, 0.88, 0.68],
                        "intensity": 22.0,
                        "range": 22.0,
                    },
                ],
            }
        )

    # --- the player ---------------------------------------------------------
    # `Player` is the physics proxy and carries no mesh; the three visual parts
    # are placed from its position every step. That is the M12 wheel pattern,
    # and it exists because a dynamic body owns its own Transform — a script
    # cannot turn one to face the aim, so the parts that must turn are not it.
    px, py, pz = PLAYER_START
    out.append(
        {
            "name": "Player",
            "components": [
                t(position=(px, py, pz)),
                {
                    "type": "RigidBody",
                    "body": "dynamic",
                    "linear_damping": 0.2,
                    "locked_rotations": [True, True, True],
                    "can_sleep": False,
                },
                {
                    "type": "Collider",
                    "shape": "capsule",
                    "radius": 0.42,
                    "half_height": 0.3,
                    "density": 120.0,
                    "friction": 0.2,
                },
            ],
        }
    )
    out.append(
        {
            "name": "PlayerBody",
            "components": [
                t(position=(px, py - 0.12, pz), scale=(0.95, 1.15, 0.95)),
                {"type": "Mesh", "asset": "builtin:cylinder"},
                mat([0.09, 0.32, 0.62], 0.6, 0.1, maps=PANEL_MAPS, uv=[2.0, 1.0]),
            ],
        }
    )
    out.append(
        {
            "name": "PlayerHead",
            "components": [
                t(position=(px, py + 0.62, pz), scale=(0.32, 0.32, 0.32)),
                {"type": "Mesh", "asset": "builtin:sphere"},
                mat([0.62, 0.50, 0.38], 0.7),
            ],
        }
    )
    out.append(
        {
            "name": "PlayerGun",
            "components": [
                t(position=(px, py + 0.34, pz - 0.7), scale=(0.15, 0.15, 1.0)),
                {"type": "Mesh", "asset": "builtin:cube"},
                mat(
                    [0.045, 0.048, 0.055], 0.35, 0.85,
                    maps={"normal_map": "../textures/plate_normal.png"},
                    uv=[1.0, 2.0],
                ),
            ],
        }
    )

    # --- drones -------------------------------------------------------------
    for i, (x, z, wave) in enumerate(DRONES):
        y = 0.95 if wave == 1 else 46.0
        out.append(
            {
                "name": f"Drone{i + 1:02d}",
                "components": [
                    t(position=(x, y, z), scale=(0.82, 0.82, 0.82)),
                    {"type": "Mesh", "asset": "builtin:cube"},
                    # The glow used to be the whole cube. `drone_eye.png`
                    # multiplies the emissive down to a lens and four corner
                    # lamps, so a drone reads as a machine with a light on it
                    # rather than as a lit block — and the panelling underneath
                    # is what makes the hull a hull. Both maps are radially
                    # symmetric, which is what survives a cube transposing `u`
                    # and `v` between its faces.
                    mat(
                        DRONE, 0.4, 0.35, DRONE_GLOW,
                        maps=dict(PANEL_MAPS, emissive_map="../textures/drone_eye.png"),
                        uv=[1.0, 1.0],
                    ),
                    {
                        "type": "RigidBody",
                        "body": "dynamic",
                        "gravity_scale": 0.0,
                        "linear_damping": 1.6,
                        "locked_rotations": [True, True, True],
                        "can_sleep": False,
                    },
                    {
                        "type": "Collider",
                        "shape": "cuboid",
                        "half_extents": [0.5, 0.5, 0.5],
                        "density": 8.0,
                        "friction": 0.3,
                    },
                    {
                        "type": "Breakable",
                        "fragments": [
                            {
                                "mesh": "builtin:cube",
                                "offset": [ox * 0.26, oy * 0.26, oz * 0.26],
                                "scale": [0.44, 0.44, 0.44],
                                "half_extents": [0.5, 0.5, 0.5],
                                "density": 8.0,
                            }
                            for ox, oy, oz in [
                                (-1, -1, -1),
                                (1, -1, 1),
                                (-1, 1, 1),
                                (1, 1, -1),
                                (1, -1, -1),
                                (-1, 1, -1),
                            ]
                        ],
                    },
                ],
            }
        )

    # --- the bullet pool ----------------------------------------------------
    # Bullets carry no RigidBody and no Collider: the script flies them and
    # tests them against drone centres itself, as a swept segment rather than a
    # point, so a 46 m/s round cannot step over a drone between two frames.
    # Physics has no opinion about them, which is also why they never disturb
    # the arena they fly through.
    for i in range(BULLETS):
        out.append(
            {
                "name": f"Bullet{i:02d}",
                "components": [
                    t(position=(0.0, -30.0, 0.0), scale=(0.11, 0.11, 0.11)),
                    {"type": "Mesh", "asset": "builtin:sphere"},
                    mat([0.9, 0.75, 0.25], 0.3, 0.0, [1.0, 0.72, 0.18]),
                ],
            }
        )

    # --- effects ------------------------------------------------------------
    out.append(
        {
            "name": "MuzzleSmoke",
            "components": [
                t(position=(px, py + 0.34, pz - 1.0), rotation=(0.0, 0.0, 0.0)),
                {
                    "type": "ParticleEmitter",
                    "seed": 21,
                    "rate": 0.0,
                    "max_particles": 320,
                    "lifetime": 0.34,
                    "lifetime_jitter": 0.4,
                    "speed": 5.5,
                    "speed_jitter": 0.6,
                    "spread": 22.0,
                    "drag": 3.0,
                    "start_size": 0.08,
                    "end_size": 0.3,
                    "start_color": [0.95, 0.62, 0.24],
                    "end_color": [0.34, 0.33, 0.32],
                    "start_alpha": 0.55,
                    "end_alpha": 0.0,
                    "acceleration": [0.0, 1.2, 0.0],
                },
            ],
        }
    )
    out.append(
        {
            "name": "MuzzleFlash",
            "components": [
                t(position=(px, py + 0.34, pz - 1.0)),
                {
                    "type": "PointLight",
                    "color": [1.0, 0.78, 0.36],
                    "intensity": 0.0,
                    "range": 5.0,
                },
            ],
        }
    )
    out.append(
        {
            "name": "Sparks",
            "components": [
                t(position=(0.0, -30.0, 0.0), rotation=(-90.0, 0.0, 0.0)),
                {
                    "type": "ParticleEmitter",
                    "seed": 44,
                    "rate": 0.0,
                    "max_particles": 512,
                    "blend": "additive",
                    "lifetime": 0.35,
                    "lifetime_jitter": 0.5,
                    "speed": 7.0,
                    "speed_jitter": 0.7,
                    "spread": 180.0,
                    "drag": 2.0,
                    "stretch": 0.03,
                    "start_size": 0.07,
                    "end_size": 0.015,
                    "start_color": [0.9, 0.45, 0.13],
                    "end_color": [0.6, 0.1, 0.02],
                    "start_alpha": 1.0,
                    "end_alpha": 0.0,
                    "acceleration": [0.0, -8.0, 0.0],
                },
            ],
        }
    )
    out.append(
        {
            "name": "Blast",
            "components": [
                t(position=(0.0, -30.0, 0.0), rotation=(-90.0, 0.0, 0.0)),
                {
                    "type": "ParticleEmitter",
                    "seed": 77,
                    "rate": 0.0,
                    "max_particles": 768,
                    "blend": "additive",
                    "lifetime": 0.8,
                    "lifetime_jitter": 0.45,
                    "speed": 11.0,
                    "speed_jitter": 0.7,
                    "size_jitter": 0.5,
                    "spread": 180.0,
                    "radius": 0.5,
                    "drag": 2.2,
                    "turbulence": 2.5,
                    "turbulence_scale": 2.0,
                    "start_size": 0.45,
                    "end_size": 1.3,
                    "start_color": [0.85, 0.40, 0.11],
                    "end_color": [0.30, 0.07, 0.02],
                    "start_alpha": 0.75,
                    "end_alpha": 0.0,
                    "acceleration": [0.0, 2.5, 0.0],
                },
            ],
        }
    )
    out.append(
        {
            "name": "BlastLight",
            "components": [
                t(position=(0.0, -30.0, 0.0)),
                {
                    "type": "PointLight",
                    "color": [1.0, 0.62, 0.24],
                    "intensity": 0.0,
                    "range": 15.0,
                },
            ],
        }
    )

    # --- scenery ------------------------------------------------------------
    for i, (x, z, seed) in enumerate(TREES):
        out.append(
            {
                "name": f"Tree{i + 1}",
                "components": [
                    t(position=(x, -4.6, z)),
                    # The bark the showcase tour authored, shared rather than
                    # re-tinted: a `Tree`'s own `Material` is its bark, and this
                    # is what `Material.asset` is for.
                    {"type": "Material", "asset": "../materials/bark.json"},
                    {
                        "type": "Tree",
                        "seed": seed,
                        "height": 8.0 + (seed % 4) * 1.1,
                        "levels": 2,
                        "branches": 5,
                        "trunk_radius": 0.2,
                        "leaf": "blade",
                        "leaf_size": 0.34,
                        "leaf_color": [0.062, 0.185, 0.055],
                    },
                ],
            }
        )
    for i, (x, z, sx, sz, seed) in enumerate(MEADOWS):
        out.append(
            {
                "name": f"Grass{i + 1}",
                "components": [
                    t(position=(x, 0.0, z), scale=(sx, 1.0, sz)),
                    {
                        "type": "Meadow",
                        "terrain": "Landscape",
                        "seed": seed,
                        "density": MEADOW_DENSITY,
                        "height": 0.62,
                        "blades": 4,
                        "segments": 3,
                        "blade_width": 0.008,
                        "head_size": 0.015,
                        "size_jitter": 0.4,
                        "splay": 58.0,
                        "max_slope": 34.0,
                        "flower_color": [0.34, 0.30, 0.13],
                        "wind": 12.0,
                        "wind_direction": 24.0,
                        "wind_speed": 4.0,
                        "phase": 0.46,
                        "stagger": 0.3,
                    },
                ],
            }
        )
    for i, (x, y, z, seed) in enumerate(CLOUDS):
        out.append(
            {
                "name": f"Cloud{i + 1}",
                "components": [
                    t(position=(x, y, z), scale=(46.0, 14.0, 34.0)),
                    {
                        "type": "Cloud",
                        "seed": seed,
                        "lobes": 6,
                        "levels": 2,
                        "children": 3,
                        "lobe_size": 0.4,
                        "density": 0.85,
                        "feather": 3.0,
                        "drift": [0.35, 0.0, 0.0],
                    },
                ],
            }
        )

    # --- the camera the script flies ---------------------------------------
    out.append(
        {
            "name": "Eye",
            "components": [
                t(position=(px, py + 20.0, pz + 11.5), rotation=(-60.0, 0.0, 0.0)),
                {"type": "Camera", "fov": 46.0, "near": 0.5, "far": 600.0, "active": True},
            ],
        }
    )

    # --- HUD (M31) ----------------------------------------------------------
    # The play HUD and the menu are one component tree, laid out by the engine.
    # Before M31 every one of these was a top-level element the script placed
    # by hand — a menu title centred by multiplying its length by the glyph
    # advance, a button whose rectangle the script both drew and hit-tested. A
    # `HudPanel` does the arranging now, `HudInteract` does the hit test, and
    # what is left in the script is which words are on screen.
    out += [
        {
            "name": "ScoreText",
            "components": [
                {
                    "type": "HudText",
                    "text": "SCORE 0",
                    "anchor": "top_left",
                    "offset": [18.0, 16.0],
                    "size": 24.0,
                    "color": [1.0, 0.95, 0.8],
                    "visible": False,
                }
            ],
        },
        {
            "name": "WaveText",
            "components": [
                {
                    "type": "HudText",
                    "text": "WAVE 1/3",
                    "anchor": "top_right",
                    "offset": [18.0, 16.0],
                    "size": 24.0,
                    "color": [1.0, 0.62, 0.5],
                    "visible": False,
                }
            ],
        },
        # The health readout is a column: label over gauge, the gauge a panel
        # whose padding *is* the bezel. The three hand-kept numbers this
        # replaces (a 224-wide back, a fill inset 3 px into it at 218, and a
        # label offset 30 px above both) were three chances to disagree; now
        # the only authored width is the panel's, and the fill's is the health.
        {
            "name": "HealthGroup",
            "components": [
                {
                    "type": "HudPanel",
                    "anchor": "bottom_left",
                    "offset": [18.0, 18.0],
                    "layout": "column",
                    "gap": 6.0,
                    "visible": False,
                }
            ],
        },
        {
            "name": "HealthLabel",
            "components": [
                {
                    "type": "HudText",
                    "text": "HEALTH",
                    "size": 16.0,
                    "color": [0.85, 0.9, 1.0],
                    "parent": "HealthGroup",
                }
            ],
        },
        {
            "name": "HealthBar",
            "components": [
                {
                    "type": "HudPanel",
                    "width": HEALTH_BAR[0],
                    "height": HEALTH_BAR[1],
                    "padding": HEALTH_PAD,
                    "color": [0.04, 0.05, 0.07],
                    "opacity": 0.65,
                    "parent": "HealthGroup",
                }
            ],
        },
        {
            "name": "HealthFill",
            "components": [
                {
                    "type": "HudRect",
                    "size": [HEALTH_FILL, HEALTH_BAR[1] - 2 * HEALTH_PAD],
                    "color": HEALTH_GOOD,
                    "opacity": 0.95,
                    "parent": "HealthBar",
                }
            ],
        },
        {
            "name": "AmmoText",
            "components": [
                {
                    "type": "HudText",
                    "text": "AMMO 12/12",
                    "anchor": "bottom_right",
                    "offset": [18.0, 18.0],
                    "size": 24.0,
                    "color": [1.0, 0.95, 0.8],
                    "visible": False,
                }
            ],
        },
        # Wave banner: centred, hidden, shown for a second and a half when a
        # wave starts. It is one `visible` the script writes — which is the
        # cheapest thing M31 added and the one this game wanted most, since
        # before it a new wave arrived with no announcement at all.
        {
            "name": "WaveBanner",
            "components": [
                {
                    "type": "HudText",
                    "text": "WAVE 1",
                    "anchor": "center",
                    "offset": [0.0, -100.0],
                    "size": 32.0,
                    "align": "center",
                    "color": [1.0, 0.72, 0.32],
                    "visible": False,
                }
            ],
        },
        # --- the menu (M31) -------------------------------------------------
        # Authored *open*, on the title screen, so `screenshot --steps 0` shows
        # the game as it opens and the file says what the first frame is. The
        # script closes it.
        #
        # The veil is a top-level stretched rect rather than a child of the
        # menu: it covers the frame, and the menu covers its own contents.
        {
            "name": "MenuVeil",
            "components": [
                {
                    "type": "HudRect",
                    "size": [0.0, 0.0],
                    "stretch": [True, True],
                    "color": [0.015, 0.02, 0.03],
                    "opacity": 0.78,
                }
            ],
        },
        {
            "name": "MenuRoot",
            "components": [{"type": "HudPanel", "anchor": "center", "layout": "free"}],
        },
        # The frame is nine-sliced and stretched over whatever the column comes
        # out as: corners 1:1, edges tiled. It is the reason the menu no longer
        # has to be a flat rectangle sized by hand.
        {
            "name": "MenuFrame",
            "components": [
                {
                    "type": "HudImage",
                    "texture": "../textures/ui_frame.png",
                    "size": [0.0, 0.0],
                    "slice": [12.0, 12.0, 12.0, 12.0],
                    "tint": [0.52, 0.60, 0.74],
                    "parent": "MenuRoot",
                    "stretch": [True, True],
                }
            ],
        },
        {
            "name": "MenuColumn",
            "components": [
                {
                    "type": "HudPanel",
                    "layout": "column",
                    "padding": 24.0,
                    "gap": 16.0,
                    "align": "center",
                    "parent": "MenuRoot",
                }
            ],
        },
        {
            "name": "MenuTitle",
            "components": [
                {
                    "type": "HudText",
                    "text": "ARENA SHOOTER",
                    "size": 32.0,
                    "color": [1.0, 0.85, 0.3],
                    "parent": "MenuColumn",
                }
            ],
        },
        # `wrap` is what lets the controls line be one string in the script
        # instead of a hand-broken three. It is set to the title's own width so
        # the column is exactly as wide as its widest unwrapped child.
        {
            "name": "MenuLine",
            "components": [
                {
                    "type": "HudText",
                    "text": CONTROLS,
                    "size": 16.0,
                    "wrap": MENU_WIDTH,
                    "line_gap": 5.0,
                    "align": "center",
                    "color": [0.75, 0.82, 0.9],
                    "parent": "MenuColumn",
                    "stretch": [True, False],
                }
            ],
        },
        # The button. `HudInteract` is the whole of what the script used to do
        # by hand: the hit box is this panel's laid-out rectangle, and the
        # tints are the hover and press feedback the old menu faked by putting
        # brackets around the label.
        {
            "name": "MenuButton",
            "components": [
                {
                    "type": "HudPanel",
                    "layout": "column",
                    "padding": 10.0,
                    "align": "center",
                    "color": [0.10, 0.13, 0.18],
                    "opacity": 0.95,
                    "parent": "MenuColumn",
                    "stretch": [True, False],
                },
                {
                    "type": "HudInteract",
                    "hover_tint": [1.9, 1.9, 1.9],
                    "press_tint": [0.5, 0.5, 0.5],
                },
            ],
        },
        {
            "name": "MenuButtonText",
            "components": [
                {
                    "type": "HudText",
                    "text": "PLAY",
                    "size": 24.0,
                    "color": [0.9, 0.95, 1.0],
                    "parent": "MenuButton",
                }
            ],
        },
        # The crosshair is `ui_icon.png` — a ring with a dot, which is what a
        # reticle is — parked on the cursor's pixel by the script and hidden
        # with the rest of the play HUD whenever a menu is up, since a menu has
        # the window's own pointer. Two bars used to stand in for it.
        {
            "name": "Crosshair",
            "components": [
                {
                    "type": "HudImage",
                    "texture": "../textures/ui_icon.png",
                    "size": [CROSSHAIR, CROSSHAIR],
                    "tint": [1.0, 0.85, 0.25],
                    "opacity": 0.9,
                    "visible": False,
                }
            ],
        },
        {
            "name": "Game",
            "components": [{"type": "Script", "source": "scripts/topdown_shooter.rhai"}],
        },
    ]
    return out


def scene():
    return {
        "name": "arena_shooter",
        "physics": {"gravity": [0.0, -19.0, 0.0], "timestep_hz": 60},
        "daylight": {
            "time_of_day": 15.2,
            "day_length": 0.0,
            "sun_elevation": 64.0,
            "sun_azimuth": 24.0,
        },
        "environment": {
            "sky": True,
            "shadows": True,
            "shadow_distance": 90.0,
            "fog_density": 0.0012,
            "samples": 4,
        },
        "entities": entities(),
    }


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    path = os.path.join(here, "arena_shooter.json")
    with open(path, "w") as f:
        json.dump(scene(), f, indent=2)
        f.write("\n")
    counts = {}
    for e in scene()["entities"]:
        for c in e["components"]:
            counts[c["type"]] = counts.get(c["type"], 0) + 1
    print(f"wrote {path}")
    print(f"  {len(scene()['entities'])} entities, {len(DRONES)} drones, {BULLETS} bullets")
    print("  components:", ", ".join(f"{k}x{v}" for k, v in sorted(counts.items())))
    print("\nthe script's constants must match this file:")
    print(f"  DRONE_COUNT  = {len(DRONES)}")
    print(f"  BULLET_COUNT = {BULLETS}")
    print(f"  BARREL_COUNT = {len(BARRELS)}")
    print(f"  ARENA_HALF   = {HALF}")
    print(f"  HEALTH_FILL  = {HEALTH_FILL}")
    print(f"  CROSSHAIR    = {CROSSHAIR}")
    for w in (1, 2, 3):
        first = min(i for i, d in enumerate(DRONES) if d[2] == w) + 1
        last = max(i for i, d in enumerate(DRONES) if d[2] == w) + 1
        print(f"  wave {w}: Drone{first:02d}..Drone{last:02d}")


if __name__ == "__main__":
    main()
