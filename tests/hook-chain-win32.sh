#!/usr/bin/env bash
# Build an isolated x86 test against the same MinHook source used by wkLua.
set -euo pipefail
cd "$(dirname "$0")/.."
XWIN="${XWIN:-/Users/eule/worms_work/wkFOW/toolchain/msvc}"
LLVM="${LLVM:-/opt/homebrew/opt/llvm/bin}"
LLD="${LLD:-/opt/homebrew/opt/lld/bin}"
MINHOOK="$(find "$HOME/.cargo/registry/src" -path '*/minhook-0.8.0/minhook/include/MinHook.h' -print -quit)"
test -n "$MINHOOK" || { echo 'MinHook 0.8 source missing' >&2; exit 1; }
MINHOOK="$(dirname "$(dirname "$MINHOOK")")"
mkdir -p target/hook-chain-test
objects=()
for src in "$MINHOOK/src/hook.c" "$MINHOOK/src/trampoline.c" \
           "$MINHOOK/src/buffer.c" "$MINHOOK/src/hde/hde32.c" \
           tests/hook-chain-win32.cpp; do
  stem="$(basename "${src%.*}")"
  "$LLVM/clang-cl" --target=i686-pc-windows-msvc /O2 /MT \
    /I "$MINHOOK/include" /I "$MINHOOK/src" \
    /imsvc "$XWIN/crt/include" /imsvc "$XWIN/sdk/include/ucrt" \
    /imsvc "$XWIN/sdk/include/um" /imsvc "$XWIN/sdk/include/shared" \
    /c "/Fotarget/hook-chain-test/$stem.obj" -- "$src"
  objects+=("target/hook-chain-test/$stem.obj")
done
"$LLD/lld-link" /out:target/hook-chain-test/test.exe /subsystem:console \
  "/libpath:$XWIN/crt/lib/x86" "/libpath:$XWIN/sdk/lib/um/x86" \
  "/libpath:$XWIN/sdk/lib/ucrt/x86" "${objects[@]}" \
  kernel32.lib
echo 'built target/hook-chain-test/test.exe'
