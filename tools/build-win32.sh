#!/usr/bin/env bash
# Cross-compile wkLua.dll (i686-pc-windows-msvc) from macOS.
#
# Ported from wkFOW's toolchain/toolchain-win32.cmake, which does the same job
# for C++: clang-cl and lld-link against the real Microsoft CRT and Windows SDK
# that xwin downloads. Rust needs the same two things, just wired through cargo
# instead of CMake:
#
#   * a linker for the MSVC target          -> lld-link + /libpath to xwin libs
#   * a C compiler for the crates with C    -> clang-cl + /imsvc to xwin headers
#
# mlua (vendored Lua 5.4) and minhook both compile C through the cc crate, so
# the CC_/CFLAGS_/AR_ variables are required, not optional.
#
# The SDK is large (~800MB), so this reuses wkFOW's copy by default rather than
# fetching a second one. Point XWIN elsewhere to override.
set -euo pipefail

XWIN="${XWIN:-/Users/eule/worms_work/wkFOW/toolchain/msvc}"
LLVM="${LLVM:-/opt/homebrew/opt/llvm/bin}"
LLD="${LLD:-/opt/homebrew/opt/lld/bin}"
CARGO_BIN="${CARGO_BIN:-/opt/homebrew/opt/rustup/libexec/bin}"
TARGET=i686-pc-windows-msvc

for f in "$XWIN/crt/include" "$XWIN/sdk/include/ucrt" "$LLVM/clang-cl" "$LLD/lld-link"; do
  [ -e "$f" ] || { echo "missing: $f" >&2; exit 1; }
done

export PATH="$CARGO_BIN:$LLD:$LLVM:$PATH"

# Headers: the SDK ships mixed-case paths on a case-sensitive volume, so each
# include root is named explicitly rather than resolved case-insensitively.
INC="/imsvc $XWIN/crt/include"
INC="$INC /imsvc $XWIN/sdk/include/ucrt"
INC="$INC /imsvc $XWIN/sdk/include/um"
INC="$INC /imsvc $XWIN/sdk/include/shared"
INC="$INC /imsvc $XWIN/sdk/include/winrt"

export CC_i686_pc_windows_msvc="$LLVM/clang-cl"
export CXX_i686_pc_windows_msvc="$LLVM/clang-cl"
export AR_i686_pc_windows_msvc="$LLVM/llvm-lib"
export CFLAGS_i686_pc_windows_msvc="--target=$TARGET $INC"
export CXXFLAGS_i686_pc_windows_msvc="--target=$TARGET $INC"

export CARGO_TARGET_I686_PC_WINDOWS_MSVC_LINKER="$LLD/lld-link"
export CARGO_TARGET_I686_PC_WINDOWS_MSVC_RUSTFLAGS="\
-Lnative=$XWIN/crt/lib/x86 \
-Lnative=$XWIN/sdk/lib/um/x86 \
-Lnative=$XWIN/sdk/lib/ucrt/x86"

cd "$(dirname "$0")/.."
echo "building wkLua.dll for $TARGET"
cargo build --release -p wk-lua --target "$TARGET" "$@"

DLL="target/$TARGET/release/wkLua.dll"
if [ -f "$DLL" ]; then
  echo
  echo "built $DLL"
  ls -la "$DLL"
else
  echo "build reported success but $DLL is missing" >&2
  exit 1
fi
