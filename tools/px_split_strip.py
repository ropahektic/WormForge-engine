#!/usr/bin/env python3
"""Split a Project X sprite strip into a WormForge frame folder.

PX draws animations as one vertical strip of square frames, each as wide as
the strip. A pack takes a folder of frames instead:

    python3 tools/px_split_strip.py STRIP.png OUT_DIR

writes OUT_DIR/frame000.png, frame001.png, ... top to bottom.
"""

import sys
from pathlib import Path

from PIL import Image


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    strip = Image.open(sys.argv[1]).convert("RGBA")
    w, h = strip.size
    if h % w:
        sys.exit(f"{sys.argv[1]}: {w}x{h} is not a strip of {w}x{w} frames")
    out = Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    for old in out.glob("frame*.png"):
        old.unlink()
    for i in range(h // w):
        strip.crop((0, i * w, w, (i + 1) * w)).save(out / f"frame{i:03}.png")
    print(f"{out}: {h // w} frames of {w}x{w}")


if __name__ == "__main__":
    main()
