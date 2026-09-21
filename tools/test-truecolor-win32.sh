#!/usr/bin/env bash
# Compile the standalone D3D9 regression using the same local SDK as the DLL.
set -euo pipefail
cd "$(dirname "$0")/.."
XWIN="${XWIN:-/Users/eule/worms_work/wkFOW/toolchain/msvc}"
LLVM="${LLVM:-/opt/homebrew/opt/llvm/bin}"
LLD="${LLD:-/opt/homebrew/opt/lld/bin}"
mkdir -p target/truecolor-test
"$LLVM/clang-cl" --target=i686-pc-windows-msvc /std:c++17 /O2 /MT \
  /imsvc "$XWIN/crt/include" /imsvc "$XWIN/sdk/include/ucrt" \
  /imsvc "$XWIN/sdk/include/um" /imsvc "$XWIN/sdk/include/shared" \
  /c crates/engine/native/truecolor.cpp /Fotarget/truecolor-test/renderer.obj
"$LLVM/clang-cl" --target=i686-pc-windows-msvc /std:c++17 /O2 /MT \
  /imsvc "$XWIN/crt/include" /imsvc "$XWIN/sdk/include/ucrt" \
  /imsvc "$XWIN/sdk/include/um" /imsvc "$XWIN/sdk/include/shared" \
  /c tools/test-truecolor-win32.cpp /Fotarget/truecolor-test/test.obj
"$LLD/lld-link" /out:target/truecolor-test/test.exe /subsystem:console \
  "/libpath:$XWIN/crt/lib/x86" "/libpath:$XWIN/sdk/lib/um/x86" \
  "/libpath:$XWIN/sdk/lib/ucrt/x86" target/truecolor-test/renderer.obj \
  target/truecolor-test/test.obj d3d9.lib user32.lib kernel32.lib
