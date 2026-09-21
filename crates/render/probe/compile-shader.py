#!/usr/bin/env python3
"""Compile crates/render/shaders/*.hlsl to embedded ps_2_0 bytecode headers.

Wine's runtime HLSL compiler is not dependable enough to ship as a module
dependency (wkFOW hit this and says so in src/ShaderRenderer.cpp), so every
shader is compiled offline and embedded as bytecode. The source hash is
stamped into the header and re-checked by build.rs, so an edited .hlsl that
was never recompiled fails the build instead of shipping stale bytecode.

Run: crates/render/probe/compile-shader.py [--fxc path/to/fxc-or-wrapper]
Default --fxc is wkFOW's Wine shim, which is what works on this machine.
"""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

SHIM = '/Users/eule/worms_work/wkFOW/toolchain/fxc/run-fxc.sh'


def fnv1a(data: bytes) -> int:
    value = 0xcbf29ce484222325
    for byte in data:
        value = ((value ^ byte) * 0x100000001b3) & 0xffffffffffffffff
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fxc', default=os.environ.get('FXC')
                        or (SHIM if Path(SHIM).exists() else None)
                        or shutil.which('fxc') or shutil.which('fxc.exe'))
    parser.add_argument('--wine-paths', action='store_true', default=True,
                        help='pass Z: paths (needed for the Wine shim)')
    parser.add_argument('shader', nargs='*', help='defaults to every .hlsl')
    args = parser.parse_args()
    if not args.fxc:
        parser.error('no fxc: pass --fxc or set FXC')

    root = Path(__file__).resolve().parent.parent
    sources = [Path(s) for s in args.shader] or sorted(
        (root / 'shaders').glob('*.hlsl'))

    def arg(path):
        s = str(Path(path).resolve())
        return 'Z:' + s.replace('/', '\\') if args.wine_paths else s

    for source in sources:
        target = source.with_suffix('.h')
        symbol = 'WF_' + source.stem.upper() + '_SHADER'
        # A failed compile must never replace a known-good header.
        with tempfile.TemporaryDirectory(prefix='wfgpu-') as temp:
            header = Path(temp) / 'out.h'
            subprocess.run([args.fxc, '/nologo', '/T', 'ps_2_0', '/E', 'main',
                            '/O3', '/WX', '/Fh', arg(header), '/Vn', symbol,
                            arg(source)], check=True)
            text = header.read_text().replace(
                f'const BYTE {symbol}[]', f'alignas(4) const BYTE {symbol}[]')
            target.write_text(
                f'// HLSL-FNV1A: {fnv1a(source.read_bytes()):016x}\n' + text)
        print(f'compiled {source.name} -> {target.name}')


if __name__ == '__main__':
    main()
