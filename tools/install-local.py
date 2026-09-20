#!/usr/bin/env python3
"""Install the built DLL and sentry pack, retaining a backup of the previous install."""

import argparse
import hashlib
from pathlib import Path
import shutil
import tempfile


DEFAULT_GAME = Path(
    "/Users/eule/Applications/Worms Armageddon.app/Contents/SharedSupport/"
    "prefix/drive_c/GOG Games/Worms Armageddon"
)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def manifest(root):
    return {str(p.relative_to(root)): digest(p)
            for p in root.rglob("*") if p.is_file()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--game", type=Path, default=DEFAULT_GAME)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    dll = repo / "target/i686-pc-windows-msvc/release/wkLua.dll"
    pack = repo / "mods/examples/sentry_gun"
    game = args.game
    for required in (dll, pack / "mod.toml", pack / "weapons.lua", game / "WA.exe"):
        if not required.is_file():
            parser.error(f"missing required file: {required}")

    expected = manifest(pack)
    expected_dll = digest(dll)
    live_dll = game / "wkLua.dll"
    live_pack = game / "Mods/sentry_gun"
    if live_dll.is_file() and live_pack.is_dir():
        if digest(live_dll) == expected_dll and manifest(live_pack) == expected:
            print("The DLL and sentry pack are already up to date.")
            return

    backups = repo / "target/local-install-backups"
    backups.mkdir(parents=True, exist_ok=True)
    backup = Path(tempfile.mkdtemp(prefix="sentry-", dir=backups))
    if live_dll.exists():
        shutil.copy2(live_dll, backup / "wkLua.dll")
    if live_pack.exists():
        shutil.copytree(live_pack, backup / "sentry_gun")

    # Stage outside Mods, then swap the whole pack so stale frames cannot remain.
    with tempfile.TemporaryDirectory(prefix=".wormforge-install-", dir=game) as temp:
        stage = Path(temp)
        shutil.copy2(dll, stage / "wkLua.dll")
        shutil.copytree(pack, stage / "sentry_gun")
        live_pack.parent.mkdir(parents=True, exist_ok=True)
        old_pack = stage / "previous-sentry"
        old_dll = stage / "previous-wkLua.dll"
        try:
            if live_pack.exists():
                live_pack.rename(old_pack)
            (stage / "sentry_gun").rename(live_pack)
            if live_dll.exists():
                live_dll.rename(old_dll)
            (stage / "wkLua.dll").rename(live_dll)
            if manifest(live_pack) != expected or digest(live_dll) != expected_dll:
                raise RuntimeError("installed file verification failed")
        except Exception:
            # Restore only components whose swap started; retain disk backups too.
            if old_pack.exists():
                if live_pack.exists():
                    shutil.rmtree(live_pack)
                old_pack.rename(live_pack)
            if old_dll.exists():
                if live_dll.exists():
                    live_dll.unlink()
                old_dll.rename(live_dll)
            raise

    print(f"Installed DLL and sentry pack ({len(expected)} files) into {game}")
    print(f"SHA-256 verified. Previous installation backed up to {backup}")
    print("Restart WA to load the new DLL.")


if __name__ == "__main__":
    main()
