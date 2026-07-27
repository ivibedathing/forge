"""Emit examples/meshes/pyramid.gltf: a flat-shaded unit-base pyramid.

Base half-extent 0.5 on y=0, apex at (0, 1, 0). Flat normals, so side faces
do not share vertices. All faces wound counter-clockwise seen from outside.
"""
import base64, json, struct, sys

A = (-0.5, 0.0,  0.5)   # front-left
B = ( 0.5, 0.0,  0.5)   # front-right
C = ( 0.5, 0.0, -0.5)   # back-right
D = (-0.5, 0.0, -0.5)   # back-left
E = ( 0.0, 1.0,  0.0)   # apex

S = 0.4472135955, 0.894427191  # normalized (1, 0.5) side-slope components

faces = [
    # (verts, normal, uvs)
    ([A, B, E], (0.0,  S[0],  S[1]), [(0, 0), (1, 0), (0.5, 1)]),   # front
    ([B, C, E], ( S[1], S[0], 0.0),  [(0, 0), (1, 0), (0.5, 1)]),   # right
    ([C, D, E], (0.0,  S[0], -S[1]), [(0, 0), (1, 0), (0.5, 1)]),   # back
    ([D, A, E], (-S[1], S[0], 0.0),  [(0, 0), (1, 0), (0.5, 1)]),   # left
    ([A, D, C, B], (0.0, -1.0, 0.0), [(0, 0), (0, 1), (1, 1), (1, 0)]),  # base
]

positions, normals, uvs, indices = [], [], [], []
for verts, normal, face_uvs in faces:
    base = len(positions)
    positions += verts
    normals += [normal] * len(verts)
    uvs += face_uvs
    for i in range(1, len(verts) - 1):  # fan
        indices += [base, base + i, base + i + 1]

def floats(values, n):
    return b"".join(struct.pack("<" + "f" * n, *v) for v in values)

pos_bytes = floats(positions, 3)
norm_bytes = floats(normals, 3)
uv_bytes = floats(uvs, 2)
idx_bytes = struct.pack("<" + "H" * len(indices), *indices)
if len(idx_bytes) % 4:
    idx_bytes += b"\x00\x00"

buffer = pos_bytes + norm_bytes + uv_bytes + idx_bytes
mins = [min(p[i] for p in positions) for i in range(3)]
maxs = [max(p[i] for p in positions) for i in range(3)]

gltf = {
    "asset": {"version": "2.0", "generator": "examples/meshes/make_pyramid.py"},
    "scene": 0,
    "scenes": [{"nodes": [0]}],
    "nodes": [{"mesh": 0}],
    "meshes": [{
        "primitives": [{
            "attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2},
            "indices": 3,
            "mode": 4,
        }],
    }],
    "accessors": [
        {"bufferView": 0, "componentType": 5126, "count": len(positions),
         "type": "VEC3", "min": mins, "max": maxs},
        {"bufferView": 1, "componentType": 5126, "count": len(normals), "type": "VEC3"},
        {"bufferView": 2, "componentType": 5126, "count": len(uvs), "type": "VEC2"},
        {"bufferView": 3, "componentType": 5123, "count": len(indices), "type": "SCALAR"},
    ],
    "bufferViews": [
        {"buffer": 0, "byteOffset": 0, "byteLength": len(pos_bytes)},
        {"buffer": 0, "byteOffset": len(pos_bytes), "byteLength": len(norm_bytes)},
        {"buffer": 0, "byteOffset": len(pos_bytes) + len(norm_bytes), "byteLength": len(uv_bytes)},
        {"buffer": 0, "byteOffset": len(pos_bytes) + len(norm_bytes) + len(uv_bytes),
         "byteLength": len(idx_bytes)},
    ],
    "buffers": [{
        "byteLength": len(buffer),
        "uri": "data:application/octet-stream;base64," + base64.b64encode(buffer).decode(),
    }],
}

with open(sys.argv[1], "w") as f:
    json.dump(gltf, f, indent=2)
    f.write("\n")
print(f"wrote {sys.argv[1]}: {len(positions)} verts, {len(indices)//3} tris")
