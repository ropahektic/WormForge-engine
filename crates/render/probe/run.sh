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

# Which probe: s0 = can this machine's D3D9 do it at all (inline test code);
# s1 = is the production renderer correct (links native/wf_gpu.cpp).
case "${1:-s1}" in
  s0) EXE=target/gpu-probe/gpu_probe.exe ;;
  s1) EXE=target/gpu-probe/gpu_render_probe.exe ;;
  *)  echo "usage: run.sh [s0|s1]" >&2; exit 2 ;;
esac
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

# The game prefix is 32-bit and Homebrew's Wine is WoW64, which refuses it, so
# running there needs the bundle's own wine32on64. The bundle's plain `wine` is
# an i386 Mach-O binary that modern macOS cannot execute at all.
BUNDLE_WINE="/Users/eule/Applications/Worms Armageddon.app/Contents/SharedSupport/wine/bin/wine32on64"
wine_bin="${WFGPU_WINE:-}"
if [ -z "$wine_bin" ] && [ -n "${WFGPU_PREFIX:-}" ] && [ -x "$BUNDLE_WINE" ]; then
  wine_bin=$BUNDLE_WINE
fi
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
