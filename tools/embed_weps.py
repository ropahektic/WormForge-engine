#!/usr/bin/env python3
"""Diff pack .wep files against stock_weps.json → named patch tables for Lua.

The Highlander no longer ships most of its .wep files; regenerate from the
last commit that did:  python3 tools/embed_weps.py --rev 78974f9
"""
from __future__ import annotations

import base64
import glob
import json
import os
import struct
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
STOCK_PATH = ROOT / "tools" / "tmp" / "stock_weps.json"
WEAPON_ENTRY_SIZE = 0x1D0
WEAPON_FIRE_TYPE = 0x30
STRIKE_SHIFT = 0x14
WEP_TAG = bytes([0x06, 0xDD, 0xBB, 0x5A])
STRINGS_AT = 0x1E4

# First name wins per absolute offset (from crates/weapons/src/wep.rs).
PATCH_NAMES: list[tuple[str, int, bool]] = [
    ("requires_aiming", 0x0C, False),
    ("shots", 0x14, False),
    ("uses_turn", 0x18, False),
    ("retreat", 0x1C, False),
    ("creates_projectile", 0x20, False),
    ("fire_type", 0x30, False),
    ("subtype_param", 0x34, False),
    ("fire_method", 0x38, False),
    ("power_pct", 0x1C8, False),
    ("weight2", 0x1CC, False),
    ("pellets", 0x3C, True),
    ("spread", 0x40, True),
    ("grenade_style", 0x44, True),
    ("collision_radius", 0x48, True),
    ("bias", 0x4C, True),
    ("damage", 0x50, True),
    ("blast", 0x54, True),
    ("sprite", 0x60, True),
    ("impact", 0x64, True),
    ("flight_flags", 0x68, True),
    ("trail", 0x6C, True),
    ("gravity_pct", 0x70, True),
    ("wind_pct", 0x74, True),
    ("bounce_pct", 0x78, True),
    ("bounce_extra", 0x7C, True),
    ("friction_pct", 0x84, True),
    ("fuse_default", 0x88, True),
    ("fuse", 0x8C, True),
    ("alt_sprite", 0x90, True),
    ("glow_enable", 0x94, True),
    ("glow_sprite", 0x98, True),
    ("glow_time", 0x9C, True),
    ("space_control", 0xA0, True),
    ("missile_type", 0xA4, True),
    ("render_size", 0xA8, True),
    ("burn_sprite", 0xAC, True),
    ("homing_strength", 0xB0, True),
    ("homing_sprite", 0xB4, True),
    ("homing_turn", 0xB8, True),
    ("homing_accel", 0xBC, True),
    ("homing_speed", 0xC0, True),
    ("homing_kind", 0xC4, True),
    ("homing_arm", 0xC8, True),
    ("homing_time", 0xCC, True),
    ("detonation", 0xF0, True),
    ("cluster_count", 0xF8, True),
    ("cluster_speed", 0xFC, True),
    ("cluster_power", 0x100, True),
    ("cluster_angle", 0x104, True),
    ("cluster_spread", 0x108, True),
    ("c_collision_radius", 0x10C, True),
    ("c_bias", 0x110, True),
    ("c_damage", 0x114, True),
    ("c_blast", 0x118, True),
    ("c_sprite_count", 0x120, True),
    ("c_sprite", 0x124, True),
    ("c_impact", 0x128, True),
    ("c_flags", 0x12C, True),
    ("c_trail", 0x130, True),
    ("c_gravity_pct", 0x134, True),
    ("c_wind_pct", 0x138, True),
    ("c_bounce_pct", 0x13C, True),
    ("c_friction_pct", 0x148, True),
    ("c_fuse_default", 0x14C, True),
    ("c_fuse", 0x150, True),
    ("c_space_control", 0x164, True),
    ("c_missile_type", 0x168, True),
    ("c_render_size", 0x16C, True),
    ("c_burn_sprite", 0x170, True),
    ("c_homing_strength", 0x174, True),
    ("c_homing_sprite", 0x178, True),
    ("c_homing_turn", 0x17C, True),
    ("c_homing_accel", 0x180, True),
    ("c_homing_speed", 0x184, True),
    ("c_homing_kind", 0x188, True),
    ("c_homing_arm", 0x18C, True),
    ("c_homing_time", 0x190, True),
    # Hitscan / arrow / spray / mine / strike headers share the 0x3C slab with
    # projectile fields; pick by fire_type (+ fire_method where needed).
    ("h_shots", 0x3C, False),
    ("h_spread", 0x40, False),
    ("h_delay", 0x44, False),
    ("h_flag", 0x48, False),
    ("h_range", 0x50, False),
    ("h_radius", 0x54, False),
    ("h_damage", 0x5C, False),
    ("h_kind", 0x64, False),
    ("h_range_px", 0x68, False),
    ("a_param0", 0x3C, False),
    ("s_count", 0x3C, False),
    ("s_spread", 0x40, False),
    ("s_life", 0x44, False),
    ("s_speed", 0x48, False),
    ("m_proximity", 0x3C, False),
    ("m_fuse", 0x40, False),
    ("m_mask", 0x44, False),
    ("m_trigger", 0x48, False),
    ("m_damage", 0x50, False),
    ("m_blast", 0x54, False),
    ("k_count", 0x3C, False),
    ("k_spacing", 0x40, False),
    ("k_munition", 0x4C, False),
]

# WA 3.8.1's last stock sprite (crates/weapons/src/wep.rs LAST_STOCK_SPRITE).
LAST_STOCK_SPRITE = 696
# Where PX numbers the sprites it adds (crates/weapons/src/wep.rs PX_SPRITE_BASE).
PX_SPRITE_BASE = 100_000

# import()'s keep_stock_sprites (crates/weapons/src/wep.rs): the fields whose
# PX-only ids leave the slot's own, the strike's extra ones, and its plane.
SPRITE_FIELDS = (0x60, 0x64, 0x68, 0xDC, 0xE0, 0xE4, 0x124, 0x170)
STRIKE_SPRITE_FIELDS = (0x74, 0x7C, 0x138, 0x140)
STRIKE_PLANE = 0x38
MISSILE_TYPE = 0xA4

# Where .wep files are read from: the working tree, or a commit (--rev).
WEP_REV: str | None = None


def read_wep(path: Path) -> bytes:
    if WEP_REV is None:
        return path.read_bytes()
    rel = path.resolve().relative_to(ROOT).as_posix()
    return subprocess.run(
        ["git", "-C", str(ROOT), "show", f"{WEP_REV}:{rel}"],
        check=True, capture_output=True,
    ).stdout


def list_weps(folder: Path) -> list[Path]:
    if WEP_REV is None:
        return sorted(folder.glob("*.wep"))
    rel = folder.resolve().relative_to(ROOT).as_posix()
    names = subprocess.run(
        ["git", "-C", str(ROOT), "ls-tree", "--name-only", f"{WEP_REV}:{rel}"],
        check=True, capture_output=True, text=True,
    ).stdout.split()
    return sorted(folder / n for n in names if n.endswith(".wep"))


def patchable(off: int) -> bool:
    return off % 4 == 0 and (
        (0x0C <= off < 0x10)
        or (0x14 <= off < 0x24)
        or (WEAPON_FIRE_TYPE <= off < WEAPON_ENTRY_SIZE)
    )


def i32(buf: bytes, off: int) -> int:
    return struct.unpack_from("<i", buf, off)[0]


def parse_wep(path: Path) -> bytes:
    b = read_wep(path)
    if b[:4] != WEP_TAG or len(b) < 4 + WEAPON_ENTRY_SIZE:
        raise SystemExit(f"{path} is not a PX .wep")
    return bytes(b[4 : 4 + WEAPON_ENTRY_SIZE])


def wep_display_name(path: Path) -> str:
    b = read_wep(path)
    if b[:4] != WEP_TAG or len(b) < STRINGS_AT + 4:
        raise SystemExit(f"{path}: no PX name string")
    ln = struct.unpack_from("<I", b, STRINGS_AT)[0]
    at = STRINGS_AT + 4
    return b[at : at + ln].decode("latin-1", "replace")


def load_stocks() -> dict[str, bytes]:
    data = json.loads(STOCK_PATH.read_text(encoding="utf-8"))
    out = {}
    for e in data:
        slot = e["slot"]
        if slot == "none":
            continue
        raw = base64.b64decode(e["record"])[:WEAPON_ENTRY_SIZE]
        if len(raw) < WEAPON_ENTRY_SIZE:
            raw = raw + bytes(WEAPON_ENTRY_SIZE - len(raw))
        out[slot] = raw
    return out


def names_for_weapon(ft: int, fm: int) -> list[tuple[str, int, bool]]:
    """Prefer the layout that matches fire_type / fire_method."""
    # Projectile / thrown (0) and strike munition slab (3): missile field names.
    # Hitscan (1, method 2), arrow (1, method 4), spray (1, method 1),
    # mine-like placed (2, method 1), strike header (3) k_*.
    prefer: set[str] = set()
    if ft == 0:
        prefer = {n for n, _, slab in PATCH_NAMES if slab or n in {
            "requires_aiming", "shots", "uses_turn", "retreat", "creates_projectile",
            "fire_type", "subtype_param", "fire_method", "power_pct", "weight2",
        }}
    elif ft == 3:
        prefer = {n for n, _, slab in PATCH_NAMES if slab or n.startswith("k_") or n in {
            "requires_aiming", "shots", "uses_turn", "retreat", "creates_projectile",
            "fire_type", "subtype_param", "fire_method", "power_pct", "weight2",
        }}
        # Strike plane lives at 0x38 as fire_method name in PATCH_NAMES.
    elif ft == 1 and fm == 2:
        prefer = {n for n, _, _ in PATCH_NAMES if n.startswith("h_") or n in {
            "requires_aiming", "shots", "uses_turn", "retreat", "creates_projectile",
            "fire_type", "subtype_param", "fire_method", "power_pct", "weight2",
        }}
    elif ft == 1 and fm == 4:
        prefer = {n for n, _, _ in PATCH_NAMES if n.startswith("a_") or n in {
            "requires_aiming", "shots", "uses_turn", "retreat", "creates_projectile",
            "fire_type", "subtype_param", "fire_method", "power_pct", "weight2",
        }}
    elif ft == 1 and fm == 1:
        prefer = {n for n, _, _ in PATCH_NAMES if n.startswith("s_") or n in {
            "requires_aiming", "shots", "uses_turn", "retreat", "creates_projectile",
            "fire_type", "subtype_param", "fire_method", "power_pct", "weight2",
        }}
    elif ft == 2:
        prefer = {n for n, _, _ in PATCH_NAMES if n.startswith("m_") or n in {
            "requires_aiming", "shots", "uses_turn", "retreat", "creates_projectile",
            "fire_type", "subtype_param", "fire_method", "power_pct", "weight2",
        }}
    else:
        prefer = {n for n, _, _ in PATCH_NAMES}

    ordered = [t for t in PATCH_NAMES if t[0] in prefer]
    # Append any remaining so we never miss a dword.
    seen = {t[0] for t in ordered}
    ordered.extend(t for t in PATCH_NAMES if t[0] not in seen)
    return ordered


def put_i32(buf: bytearray, off: int, v: int) -> None:
    struct.pack_into("<i", buf, off, v)


def imported(stock: bytes, wep: bytes) -> bytes:
    """The slot's entry after import(): copy_fire_from the record, then
    keep_stock_sprites. Byte for byte what `wep = "x.wep"` gave the slot."""
    out = bytearray(stock)
    for lo, hi in ((0x0C, 0x10), (0x14, 0x24), (WEAPON_FIRE_TYPE, WEAPON_ENTRY_SIZE)):
        out[lo:hi] = wep[lo:hi]
    flies = i32(out, MISSILE_TYPE) != 0
    strike = i32(out, WEAPON_FIRE_TYPE) == 3 and i32(out, 0x34) == 0
    fields = SPRITE_FIELDS + (STRIKE_SPRITE_FIELDS if strike else ())
    for off in fields:
        v = i32(out, off)
        if v >= PX_SPRITE_BASE or (off == 0x60 and v == 0 and flies and not strike):
            put_i32(out, off, i32(stock, off))
    if strike and i32(out, STRIKE_PLANE) > LAST_STOCK_SPRITE:
        put_i32(out, STRIKE_PLANE, i32(stock, STRIKE_PLANE))
    return bytes(out)


def field_name(abs_off: int, ft: int, fm: int, strike: bool) -> str | None:
    """The name `patch` resolves to `abs_off` (lua-host shifts slab names by
    0x14 when the weapon is a strike), preferring the fire type's own layout."""
    if strike and abs_off == STRIKE_PLANE:
        return "plane_sprite"
    for name, off, slab in names_for_weapon(ft, fm):
        if (off + STRIKE_SHIFT if (slab and strike) else off) == abs_off:
            return name
    return None


def diff_patch(stock: bytes, wep: bytes) -> list[tuple[str | int, int]]:
    """Every field import() changed on the stock slot, so copy_from + patch
    lays down the same entry. A field no name reaches (WA reads it, but no
    one has named it yet: +0xD0, the cluster copies at +0x140..+0x1AC, ...)
    goes in by offset; dropping it changes how the weapon behaves."""
    want = imported(stock, wep)
    ft = i32(want, WEAPON_FIRE_TYPE)
    fm = i32(want, 0x38)
    # As lua-host decides: fire_type in the patch if it changes, else the slot's.
    strike = ft == 3 if ft != i32(stock, WEAPON_FIRE_TYPE) else i32(stock, WEAPON_FIRE_TYPE) == 3
    out: list[tuple[str | int, int]] = []
    for off in range(0, WEAPON_ENTRY_SIZE, 4):
        if not patchable(off) or i32(want, off) == i32(stock, off):
            continue
        out.append((field_name(off, ft, fm, strike) or off, i32(want, off)))
    return out


def lua_key(key: str | int) -> str:
    return key if isinstance(key, str) else f"[0x{key:03X}]"


def lua_patch(fields: list[tuple[str | int, int]], indent: str = "  ") -> str:
    if not fields:
        return ""
    lines = [f"{indent}patch = {{"]
    for key, v in fields:
        lines.append(f"{indent}  {lua_key(key)} = {v},")
    lines.append(f"{indent}}},")
    return "\n".join(lines)


def emit_highlander_table(
    patches: dict[str, list[tuple[str | int, int]]],
    names: dict[str, str],
) -> str:
    lines = [
        "-- Display names + named patches from each former weps/<slot>.wep,",
        "-- as copy_from + name + patch (web editor style). Generated by tools/embed_weps.py.",
        "-- Each patch is every field the .wep's import changed, so the slot fires as",
        "-- it did; a field with no name yet goes in by offset ([0x0D0] = 2).",
        "local WEP_NAME = {",
    ]
    for slot in sorted(names):
        n = names[slot].replace("\\", "\\\\").replace('"', '\\"')
        lines.append(f'  {slot} = "{n}",')
    lines.append("}")
    lines.append("local WEP_PATCH = {")
    for slot in sorted(patches):
        fields = patches[slot]
        if not fields:
            continue
        lines.append(f"  {slot} = {{")
        for key, v in fields:
            lines.append(f"    {lua_key(key)} = {v},")
        lines.append("  },")
    lines.append("}")
    lines.append("")
    return "\n".join(lines)


def splice_into_rules(table: str) -> None:
    # Highlander's weapon data module (tools/split_highlander.py split it
    # out of rules.lua); the block sits between its markers.
    rules = ROOT / "mods/examples/highlander/weapons/wep_data.lua"
    text = rules.read_text(encoding="utf-8")
    begin = "-- WEP_PATCH begin"
    end = "-- WEP_PATCH end"
    block = f"{begin}\n{table.rstrip()}\n{end}"
    if begin not in text or end not in text:
        raise SystemExit(f"{rules}: cannot find the WEP_PATCH markers")
    pre, rest = text.split(begin, 1)
    _, post = rest.split(end, 1)
    text = pre + block + post
    rules.write_text(text, encoding="utf-8", newline="\n")
    print(f"spliced WEP_NAME/WEP_PATCH into {rules}")


def main() -> None:
    global WEP_REV
    # The pack no longer ships most of its .wep files: --rev reads them from
    # a commit that did (78974f9 has all of them).
    args = sys.argv[1:]
    if args[:1] == ["--rev"] and len(args) == 2:
        WEP_REV = args[1]
    elif args:
        raise SystemExit("usage: embed_weps.py [--rev <commit with the .wep files>]")

    stocks = load_stocks()
    highlander: dict[str, list[tuple[str | int, int]]] = {}
    names: dict[str, str] = {}
    for path in list_weps(ROOT / "mods/examples/highlander/weps"):
        slot = path.stem
        if slot not in stocks:
            raise SystemExit(f"no stock for {slot}")
        highlander[slot] = diff_patch(stocks[slot], parse_wep(path))
        names[slot] = wep_display_name(path)

    empty = [s for s, p in highlander.items() if not p]
    print(f"highlander: {len(highlander)} weps, {len(empty)} art-only (no patch): {empty}")

    table = emit_highlander_table(highlander, names)
    splice_into_rules(table)

    hellish_src = ROOT / "mods/examples/hellish_grenade/hellish.wep"
    if WEP_REV is None and hellish_src.is_file():
        fixture = ROOT / "crates/weapons/tests/fixtures/hellish.wep"
        fixture.parent.mkdir(parents=True, exist_ok=True)
        fixture.write_bytes(hellish_src.read_bytes())
    if WEP_REV is not None or hellish_src.is_file():
        hellish = diff_patch(stocks["holy_grenade"], parse_wep(hellish_src))
        print(f"hellish: {len(hellish)} fields -> {hellish}")
        (ROOT / "tools/tmp/hellish_patch.lua").write_text(lua_patch(hellish), encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main()
