#!/usr/bin/env python3
"""Generate the committed test textures for the M26 material fixture.

Generated rather than downloaded, for the reason every generator in this repo
is written out: the renders sit under committed baselines, so the pixels are a
format contract. A downloaded asset would also be a binary blob nobody can
diff, and these are small enough to describe in a paragraph each.

Pure Python — PNG encoding is thirty lines and a dependency here would be a
dependency on the *baselines*. Run from anywhere:

    python3 examples/textures/make_textures.py
"""

import math
import pathlib
import struct
import zlib

HERE = pathlib.Path(__file__).resolve().parent


def write_png(path, width, height, pixels):
    """Write RGBA8 `pixels` (a flat bytes-like, row-major) as a PNG."""
    raw = bytearray()
    stride = width * 4
    for y in range(height):
        raw.append(0)  # filter type 0: none, so the bytes are the pixels
        raw.extend(pixels[y * stride : (y + 1) * stride])

    def chunk(kind, data):
        out = struct.pack(">I", len(data)) + kind + data
        return out + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )
    print(f"wrote {path.relative_to(HERE.parent.parent)} ({width}×{height})")


def checker(size=256, cells=8):
    """An sRGB albedo checker.

    Two jobs: it makes UV orientation and `uv_scale` visible at a glance (a
    tiling bug draws stretched bands instead of squares), and its high-contrast
    edges are what a missing mip chain aliases on, so a floor of this seen at a
    grazing angle is the mip test by eye.
    """
    pixels = bytearray(size * size * 4)
    cell = size // cells
    for y in range(size):
        for x in range(size):
            light = ((x // cell) + (y // cell)) % 2 == 0
            # Not black-and-white: a mid-grey pair keeps the lit result inside
            # the range where a shading difference is still legible.
            value = (205, 200, 190) if light else (60, 62, 70)
            at = (y * size + x) * 4
            pixels[at : at + 4] = bytes(value) + b"\xff"
    return size, size, pixels


def orm(size=256):
    """Occlusion / roughness / metallic, glTF's packing, as linear data.

    Laid out to be legible on the builtin sphere, where u runs around the
    equator and v pole to pole, so the hemisphere facing the camera shows all
    three at once.

    G (roughness) sweeps around the equator, drawing a highlight that tightens
    across the surface — the check that fails loudly if this file were ever
    uploaded as sRGB, since a gamma-decoded roughness is glossy everywhere.
    B (metallic) turns one side to metal. R (occlusion) is a dark band low down,
    and it must darken *only* the ambient and sky terms: a band that also cut
    the sun would be a second shadow map, not ambient occlusion.
    """
    pixels = bytearray(size * size * 4)
    for y in range(size):
        for x in range(size):
            u = x / (size - 1)
            v = y / (size - 1)
            # A dark band low down, so occlusion reads as a stripe that the
            # sun's side must ignore.
            band = math.exp(-(((v - 0.72) / 0.06) ** 2))
            occlusion = int(255 * (1.0 - 0.85 * band))
            roughness = int(255 * (0.04 + 0.92 * u))
            metallic = 255 if u < 0.5 else 0
            at = (y * size + x) * 4
            pixels[at : at + 4] = bytes((occlusion, roughness, metallic, 255))
    return size, size, pixels


def bumps(size=256, period=64, depth=0.6):
    """A tangent-space normal map: a grid of round dents, linear data.

    Encoded the standard way — xyz in [-1, 1] mapped onto [0, 255], so a flat
    texel is (128, 128, 255). The dents are round rather than square so the
    perturbed shading reads as geometry rather than as a seam.
    """
    pixels = bytearray(size * size * 4)
    for y in range(size):
        for x in range(size):
            # Distance from the nearest dent centre, in [-1, 1] per axis.
            dx = ((x % period) / period) * 2.0 - 1.0
            dy = ((y % period) / period) * 2.0 - 1.0
            r = math.hypot(dx, dy)
            if r < 1e-6 or r > 1.0:
                nx, ny = 0.0, 0.0
            else:
                # A sphere-cap dent: slope grows toward the rim, then stops.
                slope = depth * math.sin(r * math.pi)
                nx, ny = -dx / r * slope, -dy / r * slope
            nz = math.sqrt(max(1.0 - nx * nx - ny * ny, 1e-6))
            at = (y * size + x) * 4
            pixels[at : at + 4] = bytes(
                (
                    int(round((nx * 0.5 + 0.5) * 255)),
                    int(round((ny * 0.5 + 0.5) * 255)),
                    int(round((nz * 0.5 + 0.5) * 255)),
                    255,
                )
            )
    return size, size, pixels


def leaf(size=128):
    """An alpha-cut foliage card: a leaf shape with hard transparent corners.

    The point is the *alpha*, which `alpha_cutoff` tests — and which the
    cut-out shadow pipeline tests too, since a leaf that cuts its pixels and
    not its shadow casts the silhouette of the quad it was drawn on.
    """
    pixels = bytearray(size * size * 4)
    for y in range(size):
        for x in range(size):
            u = x / (size - 1) * 2.0 - 1.0
            v = y / (size - 1)
            # A pointed blade: widest a third of the way up, closing to a tip.
            width = math.sin(v * math.pi) ** 1.6 * (1.0 - 0.45 * v)
            inside = abs(u) < width * 0.85
            # The midrib reads darker, so the cut-out is legibly a leaf and not
            # merely a green blob.
            rib = abs(u) < 0.06 and inside
            if not inside:
                colour = (0, 0, 0, 0)
            elif rib:
                colour = (70, 92, 40, 255)
            else:
                shade = 0.75 + 0.25 * (1.0 - abs(u))
                colour = (int(96 * shade), int(150 * shade), int(58 * shade), 255)
            at = (y * size + x) * 4
            pixels[at : at + 4] = bytes(colour)
    return size, size, pixels


if __name__ == "__main__":
    for name, make in [
        ("checker.png", checker),
        ("panel_orm.png", orm),
        ("bumps_normal.png", bumps),
        ("leaf.png", leaf),
    ]:
        width, height, pixels = make()
        write_png(HERE / name, width, height, pixels)
