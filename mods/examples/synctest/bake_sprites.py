#!/usr/bin/env python3
"""Generate the synctest pack's one sprite.

Deliberately trivial: a flat 8x8 disc. This pack is a measurement control, so
the art carries no information and should cost as little as possible to load.
Generated rather than committed as an opaque blob so it is obvious there is
nothing hiding in it.

    mods/examples/synctest/bake_sprites.py
"""

import math
import pathlib

from PIL import Image

OUT = pathlib.Path(__file__).resolve().parent / "sprites"
SIZE = 8


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    px = img.load()
    r = SIZE / 2 - 0.5
    for y in range(SIZE):
        for x in range(SIZE):
            if math.hypot(x - r, y - r) <= r:
                px[x, y] = (255, 240, 120, 255)
    path = OUT / "dot.png"
    img.save(path)
    print(f"{path.relative_to(OUT.parent)}  {SIZE}x{SIZE}")


if __name__ == "__main__":
    main()
