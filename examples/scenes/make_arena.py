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

CLOUDS = [
    (-60.0, 46.0, -70.0, 1),
    (70.0, 52.0, -40.0, 4),
    (10.0, 58.0, 90.0, 8),
]

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


def t(position=None, rotation=None, scale=None):
    c = {"type": "Transform"}
    if position is not None:
        c["position"] = [round(float(v), 4) for v in position]
    if rotation is not None:
        c["rotation"] = [round(float(v), 4) for v in rotation]
    if scale is not None:
        c["scale"] = [round(float(v), 4) for v in scale]
    return c


def mat(albedo, roughness=0.85, metallic=0.0, emissive=None):
    c = {"type": "Material", "albedo": albedo, "roughness": roughness}
    if metallic:
        c["metallic"] = metallic
    if emissive is not None:
        c["emissive"] = emissive
    return c


def box(name, x, y, z, sx, sy, sz, albedo, roughness=0.85, friction=0.9):
    """A static cuboid: mesh and collider are the same box, so what you see is
    what you bump into."""
    return {
        "name": name,
        "components": [
            t(position=(x, y, z), scale=(sx, sy, sz)),
            {"type": "Mesh", "asset": "builtin:cube"},
            mat(albedo, roughness),
            {
                "type": "Collider",
                "shape": "cuboid",
                "half_extents": [0.5, 0.5, 0.5],
                "friction": friction,
            },
        ],
    }


def decal(name, x, z, sx, sz, albedo):
    """A painted floor marking: a 6 cm slab, no collider, nothing to trip on."""
    return {
        "name": name,
        "components": [
            t(position=(x, 0.03, z), scale=(sx, 0.06, sz)),
            {"type": "Mesh", "asset": "builtin:cube"},
            mat(albedo, 0.9),
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
                mat(FLOOR, 0.88),
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
    edge = HALF + WALL_T / 2
    long_side = 2 * HALF + 2 * WALL_T
    out.append(box("WallNorth", 0.0, WALL_H / 2, -edge, long_side, WALL_H, WALL_T, WALL))
    out.append(box("WallSouth", 0.0, WALL_H / 2, edge, long_side, WALL_H, WALL_T, WALL))
    out.append(box("WallWest", -edge, WALL_H / 2, 0.0, WALL_T, WALL_H, long_side, WALL))
    out.append(box("WallEast", edge, WALL_H / 2, 0.0, WALL_T, WALL_H, long_side, WALL))

    # --- cover --------------------------------------------------------------
    for i, (x, z, sx, sy, sz) in enumerate(CRATES):
        out.append(box(f"Crate{i + 1:02d}", x, sy / 2, z, sx, sy, sz, CRATE, 0.9))
    for i, (x, z, sx, sy, sz) in enumerate(BARRIERS):
        out.append(box(f"Barrier{i + 1}", x, sy / 2, z, sx, sy, sz, CONCRETE, 0.8))

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
                    mat(BARREL, 0.45, 0.25, [0.22, 0.02, 0.0]),
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
                    mat([0.09, 0.10, 0.11], 0.5, 0.6),
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
                mat([0.09, 0.32, 0.62], 0.6, 0.1),
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
                mat([0.045, 0.048, 0.055], 0.35, 0.85),
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
                    mat(DRONE, 0.4, 0.35, DRONE_GLOW),
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
                    mat([0.115, 0.085, 0.062], 0.92),
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

    # --- HUD ----------------------------------------------------------------
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
                }
            ],
        },
        {
            "name": "HealthLabel",
            "components": [
                {
                    "type": "HudText",
                    "text": "HEALTH",
                    "anchor": "bottom_left",
                    "offset": [18.0, 48.0],
                    "size": 16.0,
                    "color": [0.85, 0.9, 1.0],
                }
            ],
        },
        {
            "name": "HealthBack",
            "components": [
                {
                    "type": "HudRect",
                    "anchor": "bottom_left",
                    "offset": [18.0, 18.0],
                    "size": [224.0, 20.0],
                    "color": [0.04, 0.05, 0.07],
                    "opacity": 0.65,
                }
            ],
        },
        {
            "name": "HealthFill",
            "components": [
                {
                    "type": "HudRect",
                    "anchor": "bottom_left",
                    "offset": [21.0, 21.0],
                    "size": [218.0, 14.0],
                    "color": [0.25, 0.85, 0.42],
                    "opacity": 0.95,
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
                }
            ],
        },
        {
            "name": "CenterText",
            "components": [
                {
                    "type": "HudText",
                    "text": "",
                    "anchor": "center",
                    "offset": [0.0, -60.0],
                    "size": 40.0,
                    "color": [1.0, 0.85, 0.3],
                }
            ],
        },
        {
            "name": "HintText",
            "components": [
                {
                    "type": "HudText",
                    "text": "WASD MOVE   ARROWS AIM   SPACE FIRE   R RELOAD",
                    "anchor": "center",
                    "offset": [0.0, 120.0],
                    "size": 16.0,
                    "color": [0.75, 0.82, 0.9],
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
    for w in (1, 2, 3):
        first = min(i for i, d in enumerate(DRONES) if d[2] == w) + 1
        last = max(i for i, d in enumerate(DRONES) if d[2] == w) + 1
        print(f"  wave {w}: Drone{first:02d}..Drone{last:02d}")


if __name__ == "__main__":
    main()
