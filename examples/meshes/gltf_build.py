"""The glTF emission every generated mesh in this directory shares.

Text glTF with a base64-embedded buffer, which is invariant 1 applied to
assets: a mesh this repo generates is diffable, and the script that wrote it is
in the tree next to it.

**What lives here is what more than one generator needs and cannot afford to
disagree about.** The box winding table is the clearest case — backface culling
is on, so a box wound the wrong way renders *nothing at all*, and the failure
looks like a missing entity rather than a wrong one. Two independently
editable copies of that table is the worst place in this directory for a copy.
The buffer packer is the second: byte offsets, four-byte alignment padding and
accessor indices have to agree with each other, and every generator gets them
right the same way.

What deliberately does *not* live here is anything that owns a generator's own
vertex arrays — `push_quad`, `sweep`, `block` and the rig tables stay with the
mesh they build, because they are the part that differs.

"""

import base64
import math
import struct

# glTF component types and buffer targets, by their numbers in the spec.
FLOAT, UNSIGNED_SHORT = 5126, 5123
ARRAY_BUFFER, ELEMENT_ARRAY_BUFFER = 34962, 34963

# Faces of a unit box: the outward normal, then its four corners in signed
# units. Counter-clockwise seen from outside, matching wgpu's default front
# face — see the module docstring for why this table is shared rather than
# copied.
BOX_FACES = [
    ((0, 1, 0), [(-1, 1, -1), (1, 1, -1), (1, 1, 1), (-1, 1, 1)]),
    ((0, -1, 0), [(-1, -1, 1), (1, -1, 1), (1, -1, -1), (-1, -1, -1)]),
    ((0, 0, 1), [(-1, -1, 1), (-1, 1, 1), (1, 1, 1), (1, -1, 1)]),
    ((0, 0, -1), [(1, -1, -1), (1, 1, -1), (-1, 1, -1), (-1, -1, -1)]),
    ((1, 0, 0), [(1, -1, 1), (1, 1, 1), (1, 1, -1), (1, -1, -1)]),
    ((-1, 0, 0), [(-1, -1, -1), (-1, 1, -1), (-1, 1, 1), (-1, -1, 1)]),
]

# The UV each of those four corners takes, in order.
FACE_UVS = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]


def floats(values):
    return struct.pack(f"<{len(values)}f", *values)


def shorts(values):
    return struct.pack(f"<{len(values)}H", *values)


def flat(rows):
    return [component for row in rows for component in row]


def bounds(rows):
    """Per-column min and max — the `min`/`max` a POSITION accessor must
    carry, since a viewer is entitled to cull on them without reading the
    buffer."""
    columns = list(zip(*rows))
    return [min(c) for c in columns], [max(c) for c in columns]


def lerp(a, b, t):
    return tuple(x + (y - x) * t for x, y in zip(a, b))


def quat_x(degrees):
    half = math.radians(degrees) / 2
    return (math.sin(half), 0.0, 0.0, math.cos(half))


def quat_y(degrees):
    half = math.radians(degrees) / 2
    return (0.0, math.sin(half), 0.0, math.cos(half))


def quat_z(degrees):
    half = math.radians(degrees) / 2
    return (0.0, 0.0, math.sin(half), math.cos(half))


class Buffer:
    """The blob, its bufferViews and its accessors, which are one thing.

    Kept together because they are only ever correct together: an accessor
    names a view by index, a view names a byte range, and the range is only
    right if every earlier append padded itself to four bytes. Appending
    through one object is what makes that unstateable rather than merely
    documented.
    """

    def __init__(self):
        self.blob = bytearray()
        self.views = []
        self.accessors = []

    def view(self, data, target=None):
        """Append bytes as a bufferView, padded so the next one starts
        aligned."""
        while len(self.blob) % 4:
            self.blob.append(0)
        entry = {"buffer": 0, "byteOffset": len(self.blob), "byteLength": len(data)}
        if target is not None:
            entry["target"] = target
        self.blob.extend(data)
        self.views.append(entry)
        return len(self.views) - 1

    def accessor(self, entry):
        self.accessors.append(entry)
        return len(self.accessors) - 1

    def buffers(self):
        """The document's `buffers` array: one buffer, embedded as a data
        URI."""
        return [
            {
                "byteLength": len(self.blob),
                "uri": "data:application/octet-stream;base64,"
                + base64.b64encode(bytes(self.blob)).decode("ascii"),
            }
        ]
