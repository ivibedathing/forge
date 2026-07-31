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


def stone_noise(x, y, size, salt):
    """A hash-based value in [0, 1) — the same discipline as every other
    generator here: written out, so a Python upgrade cannot reshuffle a
    committed texture and surface as a renderer regression."""
    h = (x * 374761393 + y * 668265263 + salt * 2246822519) & 0xFFFFFFFF
    h = (h ^ (h >> 13)) * 1274126177 & 0xFFFFFFFF
    return ((h ^ (h >> 16)) & 0xFFFF) / 65536.0


def smooth(x, y, size, cell, salt):
    """Bilinear value noise on a `cell`-sized lattice, wrapping at `size` so
    the texture tiles."""
    fx, fy = x / cell, y / cell
    x0, y0 = int(fx), int(fy)
    tx, ty = fx - x0, fy - y0
    # Smoothstep, so the lattice does not show as a grid of creases.
    tx, ty = tx * tx * (3 - 2 * tx), ty * ty * (3 - 2 * ty)
    wrap = max(size // cell, 1)
    def at(ix, iy):
        return stone_noise(ix % wrap, iy % wrap, size, salt)
    top = at(x0, y0) * (1 - tx) + at(x0 + 1, y0) * tx
    bottom = at(x0, y0 + 1) * (1 - tx) + at(x0 + 1, y0 + 1) * tx
    return top * (1 - ty) + bottom * ty


def granite(size=256):
    """Weathered grey stone, for the showcase tour's monolith.

    Three octaves of wrapping value noise plus a per-texel speckle, which is
    what reads as mineral grain rather than as fog. Tiles, because the monolith
    is 4 m wide and 5.5 m tall and a texture that seams would announce itself.
    """
    pixels = bytearray(size * size * 4)
    for y in range(size):
        for x in range(size):
            value = (
                0.55 * smooth(x, y, size, 64, 1)
                + 0.30 * smooth(x, y, size, 16, 2)
                + 0.15 * smooth(x, y, size, 4, 3)
            )
            speckle = stone_noise(x, y, size, 7) * 0.14 - 0.07
            tone = max(0.0, min(1.0, 0.42 + 0.34 * (value - 0.5) + speckle))
            # A faint warm/cool split so the stone is not a grey ramp.
            r = int(255 * tone * 1.02)
            g = int(255 * tone)
            b = int(255 * tone * 0.97)
            at = (y * size + x) * 4
            pixels[at : at + 4] = bytes((min(r, 255), min(g, 255), min(b, 255), 255))
    return size, size, pixels


def granite_normal(size=256, strength=2.2):
    """The same field's *gradient*, as a tangent-space normal map.

    Derived from the albedo's own noise rather than authored separately, so the
    bumps line up with the mottling — which is most of what makes stone read as
    stone instead of as a photograph glued to a box.
    """
    def height(x, y):
        return (
            0.55 * smooth(x, y, size, 64, 1)
            + 0.30 * smooth(x, y, size, 16, 2)
            + 0.15 * smooth(x, y, size, 4, 3)
        )

    pixels = bytearray(size * size * 4)
    for y in range(size):
        for x in range(size):
            dx = (height((x + 1) % size, y) - height((x - 1) % size, y)) * strength
            dy = (height(x, (y + 1) % size) - height(x, (y - 1) % size)) * strength
            nx, ny = -dx, -dy
            nz = math.sqrt(max(1.0 - min(nx * nx + ny * ny, 0.98), 1e-4))
            at = (y * size + x) * 4
            pixels[at : at + 4] = bytes(
                (
                    int(round(max(0.0, min(1.0, nx * 0.5 + 0.5)) * 255)),
                    int(round(max(0.0, min(1.0, ny * 0.5 + 0.5)) * 255)),
                    int(round(max(0.0, min(1.0, nz * 0.5 + 0.5)) * 255)),
                    255,
                )
            )
    return size, size, pixels


if __name__ == "__main__":
    for name, make in [
        ("checker.png", checker),
        ("panel_orm.png", orm),
        ("bumps_normal.png", bumps),
        ("leaf.png", leaf),
        ("granite.png", granite),
        ("granite_normal.png", granite_normal),
    ]:
        width, height, pixels = make()
        write_png(HERE / name, width, height, pixels)
