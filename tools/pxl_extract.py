#!/usr/bin/env python3
"""Extract the contents of Project X .pxl library files.

Layout: magic 02 01 CD 1B, u8 version, u32 item count, then per item
u8 type, u32 name length, name, and type-specific data:

  0x02 file   -> u32 length + raw bytes (PNG/WAV/BMP/... assets)
  0x04 script -> u32 length + ASCII source
  0x08 weapon -> an opaque weapon block whose length is not stored

Weapon blocks are ~600-750 bytes but variable, so their length is recovered by
resyncing on the next plausible item header and requiring the remaining items
to consume the file exactly to EOF.

Usage: pxl_extract.py <lib-dir-or-file>... <out-dir>
       pxl_extract.py --list <file.pxl>...
"""
import os
import re
import struct
import sys

MAGIC = b'\x02\x01\xcd\x1b'
FILE, SCRIPT, WEAPON = 0x02, 0x04, 0x08
SUBDIR = {FILE: 'files', SCRIPT: 'scripts', WEAPON: 'weapons'}
NAME_RE = re.compile(rb'^[\x20-\x7e]+$')


class Item:
    def __init__(self, kind, name, data):
        self.kind, self.name, self.data = kind, name, data


def _u32(b, o):
    return struct.unpack_from('<I', b, o)[0]


def _read_head(b, o):
    """Return (kind, name, offset past name), or None if o is not a header."""
    if o + 5 > len(b):
        return None
    kind = b[o]
    if kind not in SUBDIR:
        return None
    nl = _u32(b, o + 1)
    if not 1 <= nl <= 255 or o + 5 + nl > len(b):
        return None
    name = b[o + 5:o + 5 + nl]
    if not NAME_RE.match(name):
        return None
    return kind, name.decode('ascii'), o + 5 + nl


def _parse_from(b, o, remaining):
    """Parse `remaining` items at o, consuming the buffer exactly to EOF."""
    if remaining == 0:
        return [] if o == len(b) else None
    head = _read_head(b, o)
    if head is None:
        return None
    kind, name, p = head
    if kind in (FILE, SCRIPT):
        if p + 4 > len(b):
            return None
        dl = _u32(b, p)
        if p + 4 + dl > len(b):
            return None
        rest = _parse_from(b, p + 4 + dl, remaining - 1)
        if rest is None:
            return None
        return [Item(kind, name, b[p + 4:p + 4 + dl])] + rest
    for end in range(p, len(b) + 1):
        if remaining == 1:
            if end != len(b):
                continue
        elif _read_head(b, end) is None:
            continue
        rest = _parse_from(b, end, remaining - 1)
        if rest is not None:
            return [Item(kind, name, b[p:end])] + rest
    return None


def parse(path):
    """Return (version, [Item]) for a .pxl library."""
    with open(path, 'rb') as f:
        b = f.read()
    if b[:4] != MAGIC:
        raise ValueError('%s: not a .pxl library (magic %r)' % (path, b[:4]))
    items = _parse_from(b, 9, _u32(b, 5))
    if items is None:
        raise ValueError('%s: could not parse %d items' % (path, _u32(b, 5)))
    return b[4], items


def _safe(name):
    return ''.join('_' if c in '/\\:<>|?*"' else c for c in name).strip() or 'unnamed'


def extract(path, out_dir):
    _, items = parse(path)
    base = os.path.join(out_dir, os.path.splitext(os.path.basename(path))[0])
    for i, it in enumerate(items):
        d = os.path.join(base, SUBDIR[it.kind])
        os.makedirs(d, exist_ok=True)
        name = _safe(it.name)
        if it.kind == SCRIPT and not name.lower().endswith('.txt'):
            name += '.txt'
        elif it.kind == WEAPON and not name.lower().endswith('.wep'):
            name += '.wep'
        out = os.path.join(d, name)
        if os.path.exists(out):
            root, ext = os.path.splitext(name)
            out = os.path.join(d, '%s_%d%s' % (root, i, ext))
        with open(out, 'wb') as f:
            f.write(it.data)
    return items


def _libs(paths):
    for p in paths:
        if os.path.isdir(p):
            for fn in sorted(os.listdir(p)):
                if fn.lower().endswith('.pxl'):
                    yield os.path.join(p, fn)
        else:
            yield p


def main(argv):
    sys.setrecursionlimit(50000)
    if len(argv) >= 3 and argv[1] == '--list':
        for lib in _libs(argv[2:]):
            _, items = parse(lib)
            print('%s: %d items' % (os.path.basename(lib), len(items)))
            for it in items:
                print('  %-6s %-44s %8d bytes' % (SUBDIR[it.kind], it.name, len(it.data)))
        return 0
    if len(argv) < 3:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    out_dir, total = argv[-1], 0
    for lib in _libs(argv[1:-1]):
        try:
            total += len(extract(lib, out_dir))
        except ValueError as e:
            print('error: %s' % e, file=sys.stderr)
    print('extracted %d items into %s' % (total, out_dir))
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
