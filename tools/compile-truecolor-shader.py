#!/usr/bin/env python3
"""Compile the HLSL with fxc (or a compatible wrapper) and stamp its source hash."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fxc', default=os.environ.get('FXC', shutil.which('fxc') or shutil.which('fxc.exe')))
    parser.add_argument('--wine-paths', action='store_true', help='use Wine Z: paths with a local fxc wrapper')
    args = parser.parse_args()
    if not args.fxc:
        parser.error('provide --fxc or FXC pointing at fxc.exe/a compatible wrapper')
    root = Path(__file__).resolve().parent.parent
    source = root / 'crates/engine/shaders/truecolor.hlsl'
    target = source.with_suffix('.h')
    def arg(path):
        s = str(path)
        return 'Z:' + s.replace('/', '\\') if args.wine_paths else s
    # A failed compiler must not replace a known-good header.
    with tempfile.TemporaryDirectory(prefix='truecolor-', dir=root / 'target') as temp:
        header = Path(temp) / 'truecolor.h'
        subprocess.run([args.fxc, '/nologo', '/T', 'ps_2_0', '/E', 'main', '/O3', '/WX',
                        '/Fh', arg(header), '/Vn', 'WF_TRUECOLOR_SHADER', arg(source)], check=True)
        value = 0xcbf29ce484222325
        for byte in source.read_bytes():
            value = ((value ^ byte) * 0x100000001b3) & 0xffffffffffffffff
        text = header.read_text().replace('const BYTE WF_TRUECOLOR_SHADER[]',
                                          'alignas(4) const BYTE WF_TRUECOLOR_SHADER[]')
        target.write_text(f'// HLSL-FNV1A: {value:016x}\n' + text)
    print(f'Compiled {target}')


if __name__ == '__main__':
    main()
