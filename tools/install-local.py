#!/usr/bin/env python3
"""Install the built DLL, sentry pack, and Highlander pack.

Also installs the .WFAssets collection next to wkLua.dll when one has been
built (tmp/WormForge.WFAssets, or --assets), skipping it when unchanged: it
is 150 MB and append-only, so it is never backed up.
"""

import argparse
import hashlib
from pathlib import Path
import shutil
import tempfile


DEFAULT_GAME = Path(
    "/Users/eule/Applications/Worms Armageddon.app/Contents/SharedSupport/"
    "prefix/drive_c/GOG Games/Worms Armageddon"
)
PACKS = ("sentry_gun", "highlander")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def manifest(root):
    return {str(p.relative_to(root)): digest(p)
            for p in root.rglob("*") if p.is_file()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--game", type=Path, default=DEFAULT_GAME)
    parser.add_argument("--assets", type=Path, default=None,
                        help="collection to install (default tmp/WormForge.WFAssets if built)")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    dll = repo / "target/i686-pc-windows-msvc/release/wkLua.dll"
    packs = {name: repo / "mods/examples" / name for name in PACKS}
    game = args.game
    required_files = [dll, game / "WA.exe"]
    for name, pack in packs.items():
        required_files.append(pack / "mod.toml")
        required_files.append(pack / ("weapons.lua" if name == "sentry_gun" else "rules.lua"))
    for required in required_files:
        if not required.is_file():
            parser.error(f"missing required file: {required}")

    expected = {name: manifest(pack) for name, pack in packs.items()}
    expected_dll = digest(dll)
    live_dll = game / "wkLua.dll"
    had_live_dll = live_dll.exists()
    live_packs = {name: game / "Mods" / name for name in PACKS}
    up_to_date = live_dll.is_file() and digest(live_dll) == expected_dll
    up_to_date = up_to_date and all(
        live.is_dir() and manifest(live) == expected[name]
        for name, live in live_packs.items()
    )
    if up_to_date:
        print("The DLL, sentry pack, and Highlander pack are already up to date.")
        install_assets(args.assets or repo / "tmp/WormForge.WFAssets", game,
                       explicit=args.assets is not None)
        return

    backups = repo / "target/local-install-backups"
    backups.mkdir(parents=True, exist_ok=True)
    backup = Path(tempfile.mkdtemp(prefix="highlander-", dir=backups))
    if live_dll.exists():
        shutil.copy2(live_dll, backup / "wkLua.dll")
    for name, live in live_packs.items():
        if live.exists():
            shutil.copytree(live, backup / name)

    # Stage outside Mods, then swap whole packs so stale frames cannot remain.
    with tempfile.TemporaryDirectory(prefix=".wormforge-install-", dir=game) as temp:
        stage = Path(temp)
        shutil.copy2(dll, stage / "wkLua.dll")
        for name, pack in packs.items():
            shutil.copytree(pack, stage / name)
        (game / "Mods").mkdir(parents=True, exist_ok=True)
        old_dll = stage / "previous-wkLua.dll"
        swapped = []
        try:
            for name, live in live_packs.items():
                previous = stage / ("previous-" + name)
                if live.exists():
                    live.rename(previous)
                swapped.append((live, previous))
                (stage / name).rename(live)
            if live_dll.exists():
                live_dll.rename(old_dll)
            (stage / "wkLua.dll").rename(live_dll)
            if (digest(live_dll) != expected_dll or any(
                manifest(live) != expected[name]
                for name, live in live_packs.items()
            )):
                raise RuntimeError("installed file verification failed")
        except Exception:
            # Restore only components whose swap started; retain disk backups too.
            for live, previous in reversed(swapped):
                if live.exists():
                    shutil.rmtree(live)
                if previous.exists():
                    previous.rename(live)
            if old_dll.exists():
                if live_dll.exists():
                    live_dll.unlink()
                old_dll.rename(live_dll)
            elif not had_live_dll and live_dll.exists():
                live_dll.unlink()
            raise

    install_assets(args.assets or repo / "tmp/WormForge.WFAssets", game,
                   explicit=args.assets is not None)
    counts = ", ".join(f"{name}: {len(expected[name])} files" for name in PACKS)
    print(f"Installed DLL and packs ({counts}) into {game}")
    print(f"SHA-256 verified. Previous installation backed up to {backup}")
    print("Restart WA to load the new DLL.")


def install_assets(source, game, explicit):
    """Copy the collection next to wkLua.dll unless the installed one matches."""
    if not source.is_file():
        if explicit:
            raise SystemExit(f"missing collection: {source}")
        return
    live = game / "WormForge.WFAssets"
    if live.is_file() and live.stat().st_size == source.stat().st_size \
            and digest(live) == digest(source):
        print("The .WFAssets collection is already up to date.")
        return
    temp = game / ".WormForge.WFAssets.tmp"
    shutil.copyfile(source, temp)
    if digest(temp) != digest(source):
        temp.unlink()
        raise RuntimeError("collection copy verification failed")
    temp.replace(live)
    print(f"Installed {live.name} ({source.stat().st_size / 1e6:.1f} MB)")


if __name__ == "__main__":
    main()
