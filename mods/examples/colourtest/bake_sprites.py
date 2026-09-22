#!/usr/bin/env python3
"""Generate the colourtest pack's sheets.

Three sheets, all drawn by the GPU renderer, all costing zero of the 223 WA
sprite slots every pack shares.

Each one exists to make a specific renderer capability visible:

  prism  a full-spectrum sphere. Its hue sweeps the whole wheel across one
         32x32 sprite, which is thousands of distinct colours in a single
         frame -- more than a 256-entry indexed palette has room for even if
         it gave up every other colour in the map. Drawn with a white tint so
         nothing modulates it: what you see is the file.

  halo   white, with a smooth alpha falloff. The palette path could not use
         this at all: it hard-thresholded alpha (< 16 dropped, everything
         else forced opaque), so a gradient became a hard disc. Drawn
         additively and tinted at runtime, so one greyscale file covers every
         hue instead of one baked copy per colour.

  spark  the same idea at burst size.

Antialiasing is done by supersampling 4x and box-filtering in premultiplied
space -- averaging straight RGBA across the transparent edge would drag the
colour of undefined pixels into the rim.

    mods/examples/colourtest/bake_sprites.py    # sheets, plus a tmp/ preview
"""

import colorsys
import math
import pathlib

import numpy as np
from PIL import Image

PACK = pathlib.Path(__file__).resolve().parent
ROOT = PACK.parent.parent.parent
OUT = PACK / "sprites"
PREVIEW = ROOT / "tmp"

SS = 4  # supersampling factor

PRISM = 32
HALO = 64
SPARK = 16


def downsample(rgba: np.ndarray) -> Image.Image:
    """Box-filter SSxSS blocks in premultiplied space, then unpremultiply."""
    h, w, _ = rgba.shape
    rgb = rgba[..., :3].astype(np.float64)
    a = rgba[..., 3:].astype(np.float64)
    premul = rgb * a
    stack = np.concatenate([premul, a], axis=2)
    stack = stack.reshape(h // SS, SS, w // SS, SS, 4).mean(axis=(1, 3))
    a_out = stack[..., 3:]
    # Fully transparent pixels have no colour to recover; leave them black.
    rgb_out = np.divide(stack[..., :3], a_out, out=np.zeros_like(stack[..., :3]),
                        where=a_out > 1e-9)
    out = np.concatenate([rgb_out, a_out], axis=2)
    return Image.fromarray(np.clip(out * 255.0 + 0.5, 0, 255).astype(np.uint8), "RGBA")


def prism() -> Image.Image:
    """A lit sphere whose hue runs all the way round it.

    Hue by angle and saturation by radius: a white specular core bleeding out
    into fully saturated colour at the rim. The gradient is continuous, which
    is exactly what an indexed palette cannot represent -- it would band.
    """
    n = PRISM * SS
    out = np.zeros((n, n, 4))
    light = np.array([-0.45, -0.55, 0.70])
    light /= np.linalg.norm(light)
    for py in range(n):
        for px in range(n):
            dx = (px + 0.5) / n * 2 - 1
            dy = (py + 0.5) / n * 2 - 1
            r = math.hypot(dx, dy)
            if r > 1.0:
                continue
            nz = math.sqrt(max(0.0, 1.0 - r * r))
            normal = np.array([dx, dy, nz])
            lambert = max(0.0, float(normal @ light))
            # Blinn-Phong highlight, so the sphere reads as a solid object
            # rather than a flat disc of colour.
            half = light + np.array([0.0, 0.0, 1.0])
            half /= np.linalg.norm(half)
            spec = max(0.0, float(normal @ half)) ** 64 * 0.70
            hue = (math.degrees(math.atan2(dy, dx)) + 360.0) % 360.0
            # Saturate quickly: a slow ramp washes the whole sphere towards
            # white and the point of the sheet is the colour, not the shading.
            sat = min(1.0, r * 1.7)
            val = 0.40 + 0.60 * lambert
            red, green, blue = colorsys.hsv_to_rgb(hue / 360.0, sat, val)
            out[py, px] = [min(1.0, red + spec), min(1.0, green + spec),
                           min(1.0, blue + spec), 1.0]
    return downsample(out)


def falloff(size: int, power: float, core: float) -> Image.Image:
    """White with a smooth radial alpha ramp -- the additive glow sheets."""
    n = size * SS
    out = np.zeros((n, n, 4))
    for py in range(n):
        for px in range(n):
            dx = (px + 0.5) / n * 2 - 1
            dy = (py + 0.5) / n * 2 - 1
            r = math.hypot(dx, dy)
            if r >= 1.0:
                continue
            a = (1.0 - r) ** power
            a = min(1.0, a + core * max(0.0, 1.0 - r * 3.0) ** 2)
            out[py, px] = [1.0, 1.0, 1.0, a]
    return downsample(out)


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    PREVIEW.mkdir(parents=True, exist_ok=True)
    sheets = {
        "prism": prism(),
        "halo": falloff(HALO, 2.6, 0.25),
        "spark": falloff(SPARK, 1.8, 0.55),
    }
    for name, image in sheets.items():
        path = OUT / f"{name}.png"
        image.save(path)
        colours = len({tuple(p) for p in np.array(image).reshape(-1, 4) if p[3] > 0})
        print(f"{path.relative_to(ROOT)}  {image.size[0]}x{image.size[1]}  "
              f"{colours} distinct RGBA values")

    # Contact sheet on a mid grey, plus the same sheets composited additively,
    # so the previews show what the blend mode actually does rather than only
    # what the files contain.
    pad, scale = 12, 3
    width = sum(s.size[0] * scale + pad for s in sheets.values()) + pad
    height = max(s.size[1] for s in sheets.values()) * scale + 2 * pad
    sheet = Image.new("RGB", (width, height), (58, 58, 64))
    x = pad
    for image in sheets.values():
        big = image.resize((image.size[0] * scale, image.size[1] * scale), Image.NEAREST)
        under = np.array(sheet.crop((x, pad, x + big.size[0], pad + big.size[1])),
                         dtype=np.float64)
        top = np.array(big, dtype=np.float64)
        a = top[..., 3:] / 255.0
        # Premultiplied additive: exactly what the renderer does for a quad
        # whose tint alpha is 0.
        mixed = np.clip(under + top[..., :3] * a, 0, 255).astype(np.uint8)
        sheet.paste(Image.fromarray(mixed, "RGB"), (x, pad))
        x += big.size[0] + pad
    out = PREVIEW / "colourtest_sheets.png"
    sheet.save(out)
    print(f"preview {out.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
