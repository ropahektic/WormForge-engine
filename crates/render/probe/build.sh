#!/usr/bin/env bash
# Build the S0 GPU probe: a standalone 32-bit Windows exe that answers whether
# this machine's D3D9 can host the sprite renderer. It does not link wkLua, load
# WA, or touch the game prefix -- run it before committing to any integration.
#
#   crates/render/probe/build.sh && crates/render/probe/run.sh
#
# Uses the same clang-cl + lld-link + xwin SDK as tools/build-win32.sh.
set -euo pipefail
cd "$(dirname "$0")/../../.."

XWIN="${XWIN:-/Users/eule/worms_work/wkFOW/toolchain/msvc}"
LLVM="${LLVM:-/opt/homebrew/opt/llvm/bin}"
LLD="${LLD:-/opt/homebrew/opt/lld/bin}"
OUT=target/gpu-probe

if [ ! -d "$XWIN" ]; then
  echo "no MSVC SDK at $XWIN; set XWIN" >&2
  exit 1
fi
if [ ! -f crates/render/shaders/sprite.h ]; then
  echo "missing crates/render/shaders/sprite.h" >&2
  echo "run: crates/render/probe/compile-shader.py" >&2
  exit 1
fi

mkdir -p "$OUT"
INC=(
  /imsvc "$XWIN/crt/include"
  /imsvc "$XWIN/sdk/include/ucrt"
  /imsvc "$XWIN/sdk/include/um"
  /imsvc "$XWIN/sdk/include/shared"
)

"$LLVM/clang-cl" --target=i686-pc-windows-msvc /std:c++17 /O2 /MT /W3 \
  "${INC[@]}" /c crates/render/probe/gpu_probe.cpp /Fo"$OUT/gpu_probe.obj"

"$LLD/lld-link" /out:"$OUT/gpu_probe.exe" /subsystem:console \
  "/libpath:$XWIN/crt/lib/x86" \
  "/libpath:$XWIN/sdk/lib/um/x86" \
  "/libpath:$XWIN/sdk/lib/ucrt/x86" \
  "$OUT/gpu_probe.obj" d3d9.lib user32.lib kernel32.lib

echo "built $OUT/gpu_probe.exe"
