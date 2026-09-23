#!/usr/bin/env python3
"""Check the complete two-way synchronized-input matrix in two wkLua logs."""

import argparse
from dataclasses import dataclass
from pathlib import Path
import re


BUILD = re.compile(r"wkLua build (\S+)")
INPUT = re.compile(
    r"sync input (tx|rx) f=(-?\d+) keys=0x([0-9a-fA-F]+) "
    r"pointer=(-?\d+),(-?\d+) down=(true|false)$"
)
STOCK = re.compile(
    r"stock dispatch f=(-?\d+) kind=0x08 .*"
    r"expected-frame=(-?\d+) expected-checksum=([0-9a-fA-F]{8}).*$"
)
CHECKPOINT = re.compile(
    r"sync rx f=(-?\d+) (game-rng|net-rng|worms|actors)="
    r"[0-9a-fA-F]{8} local=[0-9a-fA-F]{8} OK$"
)

KEYS = (
    ("A/Left", 0x01),
    ("D/Right", 0x02),
    ("Up", 0x04),
    ("S/Down", 0x08),
    ("W", 0x10),
    ("Shift", 0x20),
    ("Q", 0x40),
    ("Space", 0x80),
)
COMPONENTS = ("game-rng", "net-rng", "worms", "actors")


@dataclass(frozen=True)
class Sample:
    frame: int
    keys: int
    x: int
    y: int
    down: bool


@dataclass
class Log:
    build: str
    tx: list[Sample]
    rx: list[Sample]
    stock: list[tuple[int, str]]
    checkpoints: set[str]
    q_equips: int
    space_charges: int
    space_shots: int
    errors: list[str]
    completed: bool


def latest_round(text: str) -> tuple[str, str]:
    launches = list(BUILD.finditer(text))
    if not launches:
        raise ValueError("no wkLua build line")
    launch = launches[-1]
    session = text[launch.start():]
    marker = "replay recorder: pending "
    if marker not in session:
        raise ValueError("latest launch has no recorded round")
    return launch.group(1), session.rsplit(marker, 1)[-1]


def parse(text: str) -> Log:
    build, text = latest_round(text)
    streams = {"tx": [], "rx": []}
    stock = []
    checkpoints = set()
    errors = []
    for line in text.splitlines():
        if match := INPUT.fullmatch(line):
            direction, frame, keys, x, y, down = match.groups()
            streams[direction].append(Sample(
                int(frame), int(keys, 16), int(x), int(y), down == "true"))
        elif match := STOCK.fullmatch(line):
            _, frame, checksum = match.groups()
            stock.append((int(frame), checksum.lower()))
        elif match := CHECKPOINT.fullmatch(line):
            checkpoints.add(match.group(2))
        if "ERROR" in line or "sync DESYNC" in line:
            errors.append(line)
    return Log(
        build=build,
        tx=streams["tx"],
        rx=streams["rx"],
        stock=stock,
        checkpoints=checkpoints,
        q_equips=text.count("Q equip bazooka"),
        space_charges=text.count("space charge worm="),
        space_shots=text.count("space shoot worm="),
        errors=errors,
        completed="replay recorder: completed " in text,
    )


def key_coverage(samples: list[Sample], bit: int) -> bool:
    pressed = next((i for i, sample in enumerate(samples) if sample.keys & bit), None)
    return pressed is not None and any(
        not (sample.keys & bit) for sample in samples[pressed + 1:]
    )


def pointer_coverage(samples: list[Sample]) -> tuple[bool, bool, bool]:
    pressed = next((i for i, sample in enumerate(samples) if sample.down), None)
    if pressed is None:
        return False, False, False
    release = next(
        (i for i in range(pressed + 1, len(samples)) if not samples[i].down),
        None,
    )
    end = release if release is not None else len(samples)
    held = [sample for sample in samples[pressed:end] if sample.down]
    moved = any(
        (left.x, left.y) != (right.x, right.y)
        for left, right in zip(held, held[1:])
    )
    return True, moved, release is not None


def inspect(label: str, log: Log) -> list[str]:
    failures = []
    print(f"{label}: build {log.build}")
    for direction, samples in (("tx", log.tx), ("rx", log.rx)):
        missing = [name for name, bit in KEYS if not key_coverage(samples, bit)]
        pointer = pointer_coverage(samples)
        pointer_names = [name for name, ok in zip(("press", "held motion", "release"), pointer)
                         if not ok]
        print(f"  {direction}: {len(samples)} samples; "
              f"keys={'PASS' if not missing else 'missing ' + ', '.join(missing)}; "
              f"pointer={'PASS' if not pointer_names else 'missing ' + ', '.join(pointer_names)}")
        if missing:
            failures.append(f"{label} {direction} missing key transitions: {', '.join(missing)}")
        if pointer_names:
            failures.append(f"{label} {direction} missing pointer: {', '.join(pointer_names)}")
    actions = (log.q_equips, log.space_charges, log.space_shots)
    print(f"  actions: Q equips={actions[0]}, Space charge={actions[1]}, "
          f"Space release={actions[2]}")
    if any(count < 2 for count in actions):
        failures.append(
            f"{label} needs at least two Q equips, Space charges, and Space releases"
        )
    missing_components = sorted(set(COMPONENTS) - log.checkpoints)
    if missing_components:
        failures.append(f"{label} missing checkpoint components: {', '.join(missing_components)}")
    if log.errors:
        failures.append(f"{label} logged {len(log.errors)} error/desync line(s)")
    if not log.completed:
        failures.append(f"{label} replay recorder did not complete")
    return failures


def compare(left: Log, right: Log) -> list[str]:
    failures = []
    if left.build != right.build:
        failures.append(f"builds differ: {left.build} / {right.build}")
    if not left.stock or not right.stock:
        failures.append("no stock kind-8 checks in one or both logs")
    elif left.stock != right.stock:
        failures.append(
            f"stock kind-8 sequences differ ({len(left.stock)} / {len(right.stock)} checks)"
        )
    else:
        print(f"stock kind-8: PASS ({len(left.stock)} matching frame/checksum pairs)")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("left", type=Path)
    parser.add_argument("right", type=Path)
    args = parser.parse_args()
    try:
        left = parse(args.left.read_text(errors="replace"))
        right = parse(args.right.read_text(errors="replace"))
    except (OSError, ValueError) as error:
        parser.error(str(error))
    failures = inspect("left", left) + inspect("right", right) + compare(left, right)
    if failures:
        print("input matrix: FAIL")
        for failure in failures:
            print(f"  - {failure}")
        return 1
    print("input matrix: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
