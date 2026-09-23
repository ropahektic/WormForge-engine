#!/usr/bin/env python3
"""Compare stock-state snapshots in two peers' latest recorded rounds/launches."""

import argparse
from pathlib import Path
import re


HEADER = re.compile(r"stock state f=(-?\d+) bytes=(\d+) stream-hash=([0-9a-f]{8})$")
DATA = re.compile(r"stock state data f=(-?\d+) off=([0-9a-f]+) hex=([0-9a-f]+)$")
END = re.compile(r"stock state end f=(-?\d+)$")


def checksum(data):
    result = 0
    for byte in data:
        result = (((result << 3) | (result >> 29)) + byte) & 0xffffffff
    return result


def snapshots(text):
    # Logs append across launches. Never compare a new round to a stale one.
    text = text.rsplit("wkLua build ", 1)[-1]
    text = text.rsplit("replay recorder: pending ", 1)[-1]
    complete, pending = {}, {}
    for line in text.splitlines():
        if match := HEADER.fullmatch(line):
            frame, size, digest = match.groups()
            pending[int(frame)] = (int(size), int(digest, 16), bytearray())
        elif match := DATA.fullmatch(line):
            frame, offset, data = match.groups()
            if int(frame) not in pending:
                raise ValueError(f"snapshot data without header at frame {frame}")
            size, _, raw = pending[int(frame)]
            if int(offset, 16) != len(raw):
                raise ValueError(f"missing or reordered data at frame {frame}, offset {offset}")
            raw.extend(bytes.fromhex(data))
            if len(raw) > size:
                raise ValueError(f"oversized snapshot at frame {frame}")
        elif match := END.fullmatch(line):
            frame = int(match[1])
            if frame not in pending:
                raise ValueError(f"snapshot end without header at frame {frame}")
            size, digest, raw = pending.pop(frame)
            if len(raw) != size or checksum(raw) != digest:
                raise ValueError(f"incomplete or corrupt snapshot at frame {frame}")
            complete[frame] = bytes(raw)
    if pending:
        raise ValueError(f"unfinished snapshots at frames {sorted(pending)}")
    return complete


def compare(left, right):
    frames = sorted(left.keys() & right.keys())
    if not frames:
        raise ValueError("no common snapshot frames in the latest sessions")
    for frame in sorted(left.keys() ^ right.keys()):
        print(f"frame {frame}: snapshot only in {'left' if frame in left else 'right'} log")
    different = False
    for frame in frames:
        a, b = left[frame], right[frame]
        if a == b:
            print(f"frame {frame}: identical serialized state ({len(a)} bytes)")
            continue
        different = True
        offsets = [i for i in range(max(len(a), len(b))) if a[i:i+1] != b[i:i+1]]
        print(f"frame {frame}: {len(offsets)} differing bytes; sizes {len(a)} / {len(b)}")
        # Show aligned words around the first differences, without flooding output.
        words = sorted({offset & ~3 for offset in offsets})
        for offset in words[:32]:
            print(f"  +0x{offset:08x}: {a[offset:offset+4].hex() or '<absent>'} / "
                  f"{b[offset:offset+4].hex() or '<absent>'}")
        if len(words) > 32:
            print(f"  ... {len(words) - 32} more differing words")
    return different


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("left", type=Path)
    parser.add_argument("right", type=Path)
    args = parser.parse_args()
    try:
        left, right = (snapshots(path.read_text(errors="replace"))
                       for path in (args.left, args.right))
        return int(compare(left, right))
    except (OSError, ValueError) as error:
        parser.error(str(error))


if __name__ == "__main__":
    raise SystemExit(main())
