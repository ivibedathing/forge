#!/usr/bin/env python3
"""Generate `textured_quad.gltf`: the fixture `engine import` is tested against.

Text glTF with a base64-embedded buffer, exactly like `pyramid.gltf` beside it
and for the same reason — a binary blob nobody can diff is what invariant 1
exists to keep out of the repo, and a generated fixture can be regenerated
rather than trusted.

The file carries what the importer has to handle: a base-colour texture and a
metallic-roughness texture embedded in the buffer as PNGs, an occlusion texture
that is a *different* image from the metallic-roughness one (the lossy case, so
the repack warning is exercised), a normal texture with a scale, `alphaMode:
MASK` with a cutoff, and the three volume extensions.

    python3 examples/meshes/make_textured_quad.py
"""

import base64
import json
import pathlib
import struct
import zlib

HERE = pathlib.Path(__file__).resolve().parent


def png(width, height, texel):
    """A `width`×`height` PNG whose pixel at (x, y) is `texel(x, y)`."""
    raw = bytearray()
    for y in range(height):
        raw.append(0)
        for x in range(width):
            raw.extend(texel(x, y))

    def chunk(kind, data):
        return (
            struct.pack(">I", len(data))
            + kind
            + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)
        )

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


# One unit quad in the XY plane, facing +Z, wound counter-clockwise.
POSITIONS = [(-0.5, -0.5, 0.0), (0.5, -0.5, 0.0), (0.5, 0.5, 0.0), (-0.5, 0.5, 0.0)]
NORMALS = [(0.0, 0.0, 1.0)] * 4
UVS = [(0.0, 1.0), (1.0, 1.0), (1.0, 0.0), (0.0, 0.0)]
INDICES = [0, 1, 2, 0, 2, 3]

IMAGES = {
    # Half opaque orange, half transparent — so alphaMode MASK has something
    # to cut.
    "base": png(8, 8, lambda x, y: (230, 140, 60, 255 if x < 4 else 0)),
    # Occlusion in R, roughness in G, metallic in B, glTF's own packing.
    "mr": png(8, 8, lambda x, y: (255, 40 + y * 24, 0, 255)),
    # A *different* image for occlusion: the case `orm_map` cannot represent
    # without a repack, and the one the importer warns about.
    "occlusion": png(8, 8, lambda x, y: (60 + x * 20, 0, 0, 255)),
    "normal": png(8, 8, lambda x, y: (128, 128, 255, 255)),
}


def build():
    blob = bytearray()
    views = []

    def add(data, target=None):
        # 4-byte alignment, which glTF requires of every accessor's view.
        while len(blob) % 4:
            blob.append(0)
        offset = len(blob)
        blob.extend(data)
        view = {"buffer": 0, "byteOffset": offset, "byteLength": len(data)}
        if target is not None:
            view["target"] = target
        views.append(view)
        return len(views) - 1

    positions = add(b"".join(struct.pack("<3f", *p) for p in POSITIONS), 34962)
    normals = add(b"".join(struct.pack("<3f", *n) for n in NORMALS), 34962)
    uvs = add(b"".join(struct.pack("<2f", *uv) for uv in UVS), 34962)
    indices = add(b"".join(struct.pack("<H", i) for i in INDICES), 34963)
    image_views = {name: add(data) for name, data in IMAGES.items()}

    accessors = [
        {
            "bufferView": positions,
            "componentType": 5126,
            "count": 4,
            "type": "VEC3",
            "min": [-0.5, -0.5, 0.0],
            "max": [0.5, 0.5, 0.0],
        },
        {"bufferView": normals, "componentType": 5126, "count": 4, "type": "VEC3"},
        {"bufferView": uvs, "componentType": 5126, "count": 4, "type": "VEC2"},
        {"bufferView": indices, "componentType": 5123, "count": 6, "type": "SCALAR"},
    ]

    document = {
        "asset": {"version": "2.0", "generator": "forge make_textured_quad.py"},
        "extensionsUsed": [
            "KHR_materials_transmission",
            "KHR_materials_ior",
            "KHR_materials_volume",
        ],
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0, "name": "Panel"}],
        "meshes": [
            {
                "name": "Panel",
                "primitives": [
                    {
                        "attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2},
                        "indices": 3,
                        "material": 0,
                        "mode": 4,
                    }
                ],
            }
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
        "images": [
            {"bufferView": image_views["base"], "mimeType": "image/png"},
            {"bufferView": image_views["mr"], "mimeType": "image/png"},
            {"bufferView": image_views["occlusion"], "mimeType": "image/png"},
            {"bufferView": image_views["normal"], "mimeType": "image/png"},
        ],
        "textures": [{"source": i} for i in range(4)],
        "materials": [
            {
                "name": "Stained Glass",
                "alphaMode": "MASK",
                "alphaCutoff": 0.4,
                "pbrMetallicRoughness": {
                    "baseColorFactor": [1.0, 1.0, 1.0, 1.0],
                    "baseColorTexture": {"index": 0},
                    "metallicFactor": 1.0,
                    "roughnessFactor": 1.0,
                    "metallicRoughnessTexture": {"index": 1},
                },
                "occlusionTexture": {"index": 2},
                "normalTexture": {"index": 3, "scale": 0.6},
                "emissiveFactor": [0.05, 0.02, 0.0],
                "extensions": {
                    "KHR_materials_transmission": {"transmissionFactor": 0.8},
                    "KHR_materials_ior": {"ior": 1.5},
                    "KHR_materials_volume": {
                        "thicknessFactor": 0.4,
                        "attenuationDistance": 2.0,
                        "attenuationColor": [0.4, 0.9, 0.6],
                    },
                },
            }
        ],
    }
    return document


if __name__ == "__main__":
    out = HERE / "textured_quad.gltf"
    out.write_text(json.dumps(build(), indent=2) + "\n")
    print(f"wrote {out.relative_to(HERE.parent.parent)}")
