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

compile() {  # compile <source> <object>
  "$LLVM/clang-cl" --target=i686-pc-windows-msvc /std:c++17 /O2 /MT /W3 \
    "${INC[@]}" /c "$1" /Fo"$2"
}
link() {     # link <exe> <object>...
  local out=$1; shift
  "$LLD/lld-link" /out:"$out" /subsystem:console \
    "/libpath:$XWIN/crt/lib/x86" \
    "/libpath:$XWIN/sdk/lib/um/x86" \
    "/libpath:$XWIN/sdk/lib/ucrt/x86" \
    "$@" d3d9.lib user32.lib kernel32.lib
}

# S0: can this machine's D3D9 do what the design needs? Inline test code.
compile crates/render/probe/gpu_probe.cpp "$OUT/gpu_probe.obj"
link "$OUT/gpu_probe.exe" "$OUT/gpu_probe.obj"

# S1: is the production renderer correct? Links native/wf_gpu.cpp itself, so
# what passes here is the code that will run inside WA.
compile crates/render/native/wf_gpu.cpp "$OUT/wf_gpu.obj"
compile crates/render/probe/gpu_render_probe.cpp "$OUT/gpu_render_probe.obj"
link "$OUT/gpu_render_probe.exe" "$OUT/wf_gpu.obj" "$OUT/gpu_render_probe.obj"

echo "built $OUT/gpu_probe.exe and $OUT/gpu_render_probe.exe"
