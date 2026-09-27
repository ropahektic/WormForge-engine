#!/usr/bin/env python3
"""Cut a Project X scheme's weapon records out as .wep files.

A PX scheme (.pxs) embeds one weapon record per stock slot, in WA's slot
order. Each is a whole PX .wep: the 4-byte tag, WA's 0x1D0-byte weapon table
entry and three more dwords, then 20 strings, each a u32 length and its bytes,
then 4 flag bytes. (Some scheme records carry a fifth byte after those; it is
not part of the .wep and is left behind.)

The scheme's records are the ones its weapons play with; they are often tuned
away from the .wep in the weapon's own library.

    python3 tools/px_scheme_weps.py SCHEME.pxs                  # list the slots
    python3 tools/px_scheme_weps.py SCHEME.pxs OUT girder dynamite ...

Each named slot is written to OUT/<slot>.wep.
"""

import struct
import sys
from pathlib import Path

TAG = bytes.fromhex("06ddbb5a")
STRINGS_AT = 0x1E4
STRING_COUNT = 20
FLAGS = 4

# crates/weapons/src/stock.rs STOCK_NAMES.
STOCK_NAMES = """none bazooka homing_missile mortar homing_pigeon sheep_launcher
grenade cluster_bomb banana_bomb battle_axe earthquake shotgun handgun uzi
minigun longbow fire_punch dragon_ball kamikaze suicide_bomber prod dynamite
mine sheep super_sheep aqua_sheep mole_bomb air_strike napalm_strike
mail_strike mine_strike mole_squadron blow_torch pneumatic_drill girder
baseball_bat girder_pack ninja_rope bungee parachute teleport
scales_of_justice super_banana holy_grenade flame_thrower salvation_army
mb_bomb petrol_bomb skunk ming_vase sheep_strike carpet_bomb mad_cow
old_woman donkey nuclear_test armageddon skip_go surrender select_worm freeze
magic_bullet jet_pack low_gravity fast_walk laser_sight invisibility
damage_x2 crate_spy double_turn_time crate_shower""".split()


def records(scheme: bytes):
    """Every embedded .wep, in slot order: (bytes, name)."""
    out = []
    at = scheme.find(TAG)
    while at != -1:
        end = at + STRINGS_AT
        strings = []
        for _ in range(STRING_COUNT):
            (n,) = struct.unpack_from("<I", scheme, end)
            strings.append(scheme[end + 4 : end + 4 + n].decode("latin-1"))
            end += 4 + n
        end += FLAGS
        out.append((scheme[at:end], strings[0]))
        at = scheme.find(TAG, end)
    if len(out) != len(STOCK_NAMES):
        sys.exit(f"expected {len(STOCK_NAMES)} weapon records, found {len(out)}")
    return out


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    recs = records(Path(sys.argv[1]).read_bytes())
    if len(sys.argv) == 2:
        for i, (_, name) in enumerate(recs):
            print(f"{i:2} {STOCK_NAMES[i]:18} {name}")
        return
    out = Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    for slot in sys.argv[3:]:
        if slot not in STOCK_NAMES:
            sys.exit(f"unknown stock slot '{slot}'")
        data, name = recs[STOCK_NAMES.index(slot)]
        (out / f"{slot}.wep").write_bytes(data)
        print(f"{slot}.wep  {name}")


if __name__ == "__main__":
    main()
