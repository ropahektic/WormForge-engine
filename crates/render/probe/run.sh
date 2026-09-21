#!/usr/bin/env bash
# Run the S0 GPU probe.
#
# Prefix choice matters and is the whole point of running this twice:
#
#   default            a private throwaway prefix under target/. Answers "can
#                      D3D9 do this here at all", with no game state involved.
#   WFGPU_PREFIX=...   point at the real GOG Worms prefix to answer the question
#                      that actually decides the project: can it do this in the
#                      exact prefix, with the exact graphics translation layer,
#                      that WA runs under.
#
# A pass in the private prefix and a fail in the game prefix is itself a useful
# result -- it means the translation layer, not D3D9, is the constraint.
set -euo pipefail
cd "$(dirname "$0")/../../.."

EXE=target/gpu-probe/gpu_probe.exe
if [ ! -f "$EXE" ]; then
  echo "no probe binary; run crates/render/probe/build.sh first" >&2
  exit 1
fi

GAME_PREFIX="/Users/eule/Applications/Worms Armageddon.app/Contents/SharedSupport/prefix"
export WINEPREFIX="${WFGPU_PREFIX:-$PWD/target/gpu-probe/.wine}"
export WINEDEBUG="${WINEDEBUG:--all}"
unset WINEARCH

if [ "$WINEPREFIX" = "$GAME_PREFIX" ]; then
  echo "prefix: the live GOG game prefix (the decisive run)"
else
  echo "prefix: $WINEPREFIX"
fi

# WA bundles an i386 Wine that modern macOS cannot execute, and a shell started
# from the game bundle can put it first on PATH. Resolve deliberately.
wine_bin="${WFGPU_WINE:-}"
if [ -z "$wine_bin" ] && [ -x /usr/local/bin/wine ]; then
  wine_bin=/usr/local/bin/wine
fi
if [ -z "$wine_bin" ]; then
  wine_bin=$(command -v wine || true)
fi
if [ -z "$wine_bin" ] || [ ! -x "$wine_bin" ]; then
  echo "no usable Wine found; set WFGPU_WINE" >&2
  exit 127
fi

exec "$wine_bin" "$EXE"
