#!/usr/bin/env python3
"""List the symbols in Project X's pxdata.pxd.

pxdata.pxd (next to PXLEdit.exe) is PX's symbol table: the classes, members,
methods and enums its script language knows. Each member record is a u32 name
length, the name, then three u32s: a type (8 = float, which is WA's 16.16
fixed point; 0 = int; 1 = bool; 7 = void method), a second word, and the
member's byte offset in the object. Class and enum records use other layouts,
so their three words print as noise.

    python3 tools/px_symbols.py PXDATA.pxd            # every record
    python3 tools/px_symbols.py PXDATA.pxd SpX TurnSide

Each line is: file offset, name, type, second word, member offset. See
docs/px-reverse-engineering.md for what the offsets mean against WA's own.
"""

import re
import struct
import sys

NAME = re.compile(rb"[A-Za-z_][A-Za-z0-9_@]*")


def symbols(data: bytes):
    at = 0
    while at < len(data) - 4:
        (n,) = struct.unpack_from("<I", data, at)
        if 2 <= n <= 64 and at + 4 + n + 12 <= len(data):
            name = data[at + 4 : at + 4 + n]
            if NAME.fullmatch(name):
                words = struct.unpack_from("<III", data, at + 4 + n)
                yield at, name.decode(), words
                at += 4 + n
                continue
        at += 1


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    data = open(sys.argv[1], "rb").read()
    wanted = set(sys.argv[2:])
    for at, name, (kind, second, offset) in symbols(data):
        if not wanted or name in wanted:
            print(f"{at:06x} {name:40s} {kind:08x} {second:08x} {offset:08x}")


if __name__ == "__main__":
    main()
