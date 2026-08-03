#!/usr/bin/env python3
"""Generate the arena shooter's three weapon meshes.

Text glTF with a base64-embedded buffer, like every other generated asset in
this directory and for invariant 1's reason. Static, not rigged: a weapon is a
*prop*, hung off the player rig's `HandR` through `world.joint_position` — which
is M30's own sanctioned pattern ("hanging a prop off a hand is then an ordinary
`set_position`"), and M36 is the first thing in the repo that actually does it.

Three conventions, and all three are load-bearing:

* **The origin is the grip**, because that is the point the hand holds and
  therefore the point the script places.
* **The barrel runs along -Z**, the engine's forward. `world.look_at` aims an
  entity's local -Z, so a weapon authored this way is aimed by the same call
  that aims a camera and a light, with no per-weapon offset anywhere.
* **One primitive, one material.** The scene's own `Material` colours the whole
  weapon; the shapes do the reading. Two primitives would need two materials on
  one entity, which the component model does not have — and a stand-in that
  needs a texture atlas to be legible is the wrong stand-in.

Each weapon is a list of boxes. That is not laziness: at the arena camera's
20 m the whole weapon is about forty pixels long, and a silhouette made of
rectangles is exactly as readable there as a modelled one — while staying
something an agent can retune by editing four numbers.

    python3 examples/meshes/make_weapons.py
"""

import json
import pathlib

from gltf_build import (
    ARRAY_BUFFER,
    BOX_FACES,
    ELEMENT_ARRAY_BUFFER,
    FACE_UVS,
    FLOAT,
    UNSIGNED_SHORT,
    Buffer,
    bounds,
    flat,
    floats,
    shorts,
)

HERE = pathlib.Path(__file__).resolve().parent

# Each entry: (name, [(centre, half extents), ...]).
#
# Measured against the player rig: `HandR` sits 1.20 m up and 0.33 m forward of
# the chest, so a weapon whose grip is at the origin puts its muzzle roughly a
# metre ahead of the player — which is what `MUZZLE_FORWARD` in the game script
# has to agree with.
WEAPONS = {
    # A sidearm: slide, grip, and nothing else to say. Short enough that the
    # silhouette reads as "small" from directly above, which is the only
    # distinction the top-down camera can make between three weapons.
    "pistol": [
        ((0.000, 0.030, -0.110), (0.022, 0.038, 0.115)),  # slide
        ((0.000, -0.055, 0.010), (0.020, 0.062, 0.032)),  # grip
        ((0.000, -0.012, -0.020), (0.012, 0.016, 0.040)),  # trigger guard
    ],
    # A rifle: long, with a stock behind the hand — the reason the grip is the
    # origin rather than the receiver's centre is that a stock has to extend
    # *backwards*, and negative -Z is where it goes.
    "rifle": [
        ((0.000, 0.028, -0.300), (0.017, 0.024, 0.300)),  # barrel
        ((0.000, 0.020, -0.030), (0.030, 0.046, 0.150)),  # receiver
        ((0.000, 0.010, 0.190), (0.024, 0.040, 0.120)),  # stock
        ((0.000, -0.070, -0.020), (0.018, 0.055, 0.038)),  # magazine
        ((0.000, -0.055, 0.055), (0.019, 0.055, 0.030)),  # grip
        ((0.000, -0.020, -0.280), (0.021, 0.026, 0.090)),  # foregrip
        ((0.000, 0.062, -0.120), (0.008, 0.014, 0.055)),  # optic
    ],
    # A shotgun: shorter than the rifle, fatter everywhere, with a pump under
    # the barrel. Bulk is what has to read at forty pixels, not detail.
    "shotgun": [
        ((0.000, 0.030, -0.250), (0.024, 0.030, 0.250)),  # barrel
        ((0.000, 0.014, -0.030), (0.036, 0.050, 0.140)),  # receiver
        ((0.000, 0.000, 0.185), (0.028, 0.048, 0.115)),  # stock
        ((0.000, -0.030, -0.230), (0.030, 0.032, 0.110)),  # pump
        ((0.000, -0.050, 0.050), (0.021, 0.050, 0.032)),  # grip
    ],
}

def build(boxes):
    """Flat-shaded geometry for a list of boxes: every face owns its vertices,
    so the edges stay crisp instead of averaging into a rounded lump."""
    positions, normals, uvs, indices = [], [], [], []
    for centre, half in boxes:
        for normal, corners in BOX_FACES:
            base = len(positions)
            for corner, uv in zip(corners, FACE_UVS):
                positions.append(
                    tuple(centre[a] + corner[a] * half[a] for a in range(3))
                )
                normals.append(normal)
                uvs.append(uv)
            indices.extend([base, base + 1, base + 2, base, base + 2, base + 3])
    return positions, normals, uvs, indices


def document_for(name, boxes):
    positions, normals, uvs, indices = build(boxes)

    buffer = Buffer()
    view, accessor = buffer.view, buffer.accessor

    position_min, position_max = bounds(positions)
    position_a = accessor(
        {
            "bufferView": view(floats(flat(positions)), ARRAY_BUFFER),
            "componentType": FLOAT,
            "count": len(positions),
            "type": "VEC3",
            "min": position_min,
            "max": position_max,
        }
    )
    normal_a = accessor(
        {
            "bufferView": view(floats(flat(normals)), ARRAY_BUFFER),
            "componentType": FLOAT,
            "count": len(normals),
            "type": "VEC3",
        }
    )
    uv_a = accessor(
        {
            "bufferView": view(floats(flat(uvs)), ARRAY_BUFFER),
            "componentType": FLOAT,
            "count": len(uvs),
            "type": "VEC2",
        }
    )
    index_a = accessor(
        {
            "bufferView": view(shorts(indices), ELEMENT_ARRAY_BUFFER),
            "componentType": UNSIGNED_SHORT,
            "count": len(indices),
            "type": "SCALAR",
        }
    )

    return (
        {
            "asset": {
                "version": "2.0",
                "generator": "forge examples/meshes/make_weapons.py",
            },
            "scene": 0,
            "scenes": [{"nodes": [0]}],
            # The node transform is the identity: a static primitive's node
            # transform *is* baked by the loader, so anything here would be a
            # second placement fighting the scene's own `Transform`.
            "nodes": [{"name": name.capitalize(), "mesh": 0}],
            "meshes": [
                {
                    "name": f"{name.capitalize()}Mesh",
                    "primitives": [
                        {
                            "attributes": {
                                "POSITION": position_a,
                                "NORMAL": normal_a,
                                "TEXCOORD_0": uv_a,
                            },
                            "indices": index_a,
                            "mode": 4,
                        }
                    ],
                }
            ],
            "accessors": buffer.accessors,
            "bufferViews": buffer.views,
            "buffers": buffer.buffers(),
        },
        positions,
        indices,
    )


for name, boxes in WEAPONS.items():
    document, positions, indices = document_for(name, boxes)
    out = HERE / f"weapon_{name}.gltf"
    out.write_text(json.dumps(document, indent=2) + "\n")
    reach = min(p[2] for p in positions)
    back = max(p[2] for p in positions)
    print(
        f"wrote {out.name} "
        f"({len(positions)} vertices, {len(indices) // 3} triangles, "
        f"muzzle {-reach:.3f} m forward of the grip, {back:.3f} m behind)"
    )
