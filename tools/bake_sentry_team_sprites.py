#!/usr/bin/env python3
"""Bake the sentry gun's worm sheets once per team colour.

The planted turret takes its team colour as a vertex tint on a greyscale
housing. The worm's equip and put-away sheets carry the same housing, but
there the tint cannot be a quad tint: it would colour the worm too. So each
sheet is baked six times with the same tint multiplied into the housing only,
and the pack lists the six per pose (`worm_sprites = { weaponlnk = {...} }`);
the engine draws the one matching the worm's alliance.

The housing is found by colour, frame by frame:
- grey (the worm is pink; its outline is a dark red),
- not one of the exact greys the worm itself uses (eyes, pupils, highlights),
  read off the first equip frame, where the worm holds nothing yet,
- at least FLOOR bright, and part of a connected area of at least MIN_AREA
  pixels. The tripod is dark with small highlights; both stay grey, as the
  planted tripod does.

Usage: bake_sentry_team_sprites.py <PX weapon_sentry_gun files dir> <pack dir>
"""

import os
import sys

from PIL import Image

# alliance order, and the same values as TEAM_TINT in weapons.lua
TEAMS = [
    ("red", (255, 100, 71)),
    ("blue", (71, 100, 255)),
    ("green", (71, 255, 100)),
    ("yellow", (230, 230, 71)),
    ("magenta", (230, 71, 230)),
    ("cyan", (71, 230, 230)),
]
SHEETS = [("equip", "wsgnlnk"), ("unequip", "wsgnbak")]
SLOPES = ["", "u", "d"]
FRAME = 60
GREY = 8
FLOOR = 90
MIN_AREA = 12


def frames(path):
    strip = Image.open(path).convert("RGBA")
    return [strip.crop((0, y, FRAME, y + FRAME)) for y in range(0, strip.height, FRAME)]


def is_grey(r, g, b):
    return max(r, g, b) - min(r, g, b) <= GREY


def worm_greys(frame):
    return {
        (r, g, b)
        for r, g, b, a in frame.getdata()
        if a and is_grey(r, g, b)
    }


def housing(frame, worm):
    px = frame.load()
    candidates = set()
    for y in range(frame.height):
        for x in range(frame.width):
            r, g, b, a = px[x, y]
            if a and is_grey(r, g, b) and (r, g, b) not in worm and max(r, g, b) >= FLOOR:
                candidates.add((x, y))
    keep, seen = set(), set()
    for start in sorted(candidates):
        if start in seen:
            continue
        seen.add(start)
        stack, area = [start], []
        while stack:
            x, y = stack.pop()
            area.append((x, y))
            for dx in (-1, 0, 1):
                for dy in (-1, 0, 1):
                    n = (x + dx, y + dy)
                    if n in candidates and n not in seen:
                        seen.add(n)
                        stack.append(n)
        if len(area) >= MIN_AREA:
            keep.update(area)
    return keep


def tint(frame, mask, factor):
    out = frame.copy()
    px = out.load()
    for x, y in mask:
        r, g, b, a = px[x, y]
        px[x, y] = (r * factor[0] // 255, g * factor[1] // 255, b * factor[2] // 255, a)
    return out


def main(argv):
    if len(argv) != 3:
        print(__doc__)
        return 2
    src, pack = argv[1], argv[2]
    worm = set()
    for slope in SLOPES:
        worm |= worm_greys(frames(os.path.join(src, "wsgnlnk%s.png" % slope))[0])
    made = 0
    for slope in SLOPES:
        for action, source in SHEETS:
            sheet = frames(os.path.join(src, source + slope + ".png"))
            masks = [housing(f, worm) for f in sheet]
            for team, factor in TEAMS:
                folder = os.path.join(pack, "sprites", "teams", team, action + slope)
                os.makedirs(folder, exist_ok=True)
                for i, (frame, mask) in enumerate(zip(sheet, masks)):
                    tint(frame, mask, factor).save(os.path.join(folder, "frame%03d.png" % i))
                made += 1
    print("baked %d sheets into %s" % (made, os.path.join(pack, "sprites", "teams")))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
