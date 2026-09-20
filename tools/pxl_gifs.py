#!/usr/bin/env python3
"""Render sprite sheets extracted from .pxl libraries into animated GIFs.

Project X sprite sheets are vertical (occasionally horizontal) strips of equal
frames. The frame count comes from two sources, in order of preference:

  1. The library's own scripts, which pair an attachment with its frame count:
         f = GetAttachment("drone.png");  Drone = LoadSprite(f, 4, 1);
     This is authoritative and catches non-square frames, e.g. drone.png is
     60x160 -> 4 frames of 60x40, which a square-frame guess would miss.
  2. A square-frame guess: a strip H tall and W wide holds H/W frames when H is
     a multiple of W. Used when no script declares the sheet, and when a script
     declares a single frame for what is plainly a strip of squares (worm "aim"
     sheets are loaded whole and sliced by the engine at runtime).

Anything that resolves to one frame is written as a static GIF unless
--multi-only is given.

Usage: pxl_gifs.py <extracted-dir> <out-dir> [options]
Options:
  --fps N        frames per second (default 12)
  --scale N      integer nearest-neighbour upscale (default 1)
  --min-px N     auto-upscale frames smaller than N px on their long side
  --bg COLOR     #rrggbb background, or "none" to keep alpha (default none)
  --multi-only   skip sheets that resolve to a single frame
"""
import os
import re
import sys

from PIL import Image, ImageFile

# A few source assets have damaged image data (e.g. wclnbak.png has a corrupt
# IDAT); decode what is there rather than losing the sprite entirely.
ImageFile.LOAD_TRUNCATED_IMAGES = True

IMG_EXT = ('.png', '.bmp', '.gif', '.tga')
# f = GetAttachment("name.png");  <assignment>  LoadSprite(f, <frames>, <flag>)
DECL_RE = re.compile(
    r'GetAttachment\s*\(\s*"([^"]+)"\s*\)\s*;(?:(?!GetAttachment).){0,400}?'
    r'LoadSprite\s*\(\s*\w+\s*,\s*(\d+)', re.S)


def script_frame_counts(lib_dir):
    """Map lowercased attachment name -> declared frame count for one library."""
    out = {}
    sdir = os.path.join(lib_dir, 'scripts')
    if not os.path.isdir(sdir):
        return out
    for fn in sorted(os.listdir(sdir)):
        path = os.path.join(sdir, fn)
        if not os.path.isfile(path):
            continue
        with open(path, encoding='latin1') as f:
            txt = f.read()
        for m in DECL_RE.finditer(txt):
            out[m.group(1).lower()] = int(m.group(2))
    return out


def frame_layout(size, declared):
    """Return (frame_count, horizontal?) for a sheet of the given size."""
    w, h = size
    v_square = h // w if w and h % w == 0 and h > w else 1
    h_square = w // h if h and w % h == 0 and w > h else 1
    if declared and declared > 1:
        if h % declared == 0 and h // declared <= w * 4:
            return declared, False
        if w % declared == 0:
            return declared, True
    # A script may declare 1 frame for a sheet the engine slices itself.
    if v_square > 1:
        return v_square, False
    if h_square > 1:
        return h_square, True
    return 1, False


def cut(im, count, horizontal):
    w, h = im.size
    if count <= 1:
        return [im]
    fw, fh = (w // count, h) if horizontal else (w, h // count)
    if horizontal:
        return [im.crop((i * fw, 0, (i + 1) * fw, h)) for i in range(count)]
    return [im.crop((0, i * fh, w, (i + 1) * fh)) for i in range(count)]


def prepare(frames, bg, scale, min_px):
    out = []
    if scale == 1 and min_px:
        long_side = max(frames[0].size)
        if long_side < min_px:
            scale = max(1, min_px // max(1, long_side))
    for fr in frames:
        if bg is not None:
            flat = Image.new('RGB', fr.size, bg)
            flat.paste(fr, mask=fr.getchannel('A'))
            fr = flat
        if scale > 1:
            fr = fr.resize((fr.width * scale, fr.height * scale), Image.NEAREST)
        out.append(fr)
    return out


def to_palette(frame, transparent):
    """Quantize a frame, reserving index 255 for alpha when keeping it."""
    if not transparent:
        return frame.convert('P', palette=Image.ADAPTIVE, colors=256)
    alpha = frame.getchannel('A')
    p = frame.convert('RGB').quantize(colors=255)
    p.paste(255, alpha.point(lambda a: 255 if a < 128 else 0))
    p.info['transparency'] = 255
    return p


def render(path, out_path, declared, fps, scale, min_px, bg, multi_only):
    im = Image.open(path)
    n_frames_in_file = getattr(im, 'n_frames', 1)
    if n_frames_in_file > 1:      # already animated; pass through
        frames, count = [f.copy().convert('RGBA') for f in _iter(im)], n_frames_in_file
        horizontal = False
    else:
        im = im.convert('RGBA')
        count, horizontal = frame_layout(im.size, declared)
        if count == 1 and multi_only:
            return None
        frames = cut(im, count, horizontal)
    frames = prepare(frames, bg, scale, min_px)
    pal = [to_palette(f, bg is None) for f in frames]
    pal[0].save(out_path, save_all=True, append_images=pal[1:],
                duration=max(20, int(1000 / fps)), loop=0, disposal=2,
                optimize=False)
    return count


def _iter(im):
    for i in range(im.n_frames):
        im.seek(i)
        yield im


def parse_bg(s):
    if s is None or s.lower() == 'none':
        return None
    s = s.lstrip('#')
    return tuple(int(s[i:i + 2], 16) for i in (0, 2, 4))


def main(argv):
    args = [a for a in argv[1:] if not a.startswith('--')]
    if len(args) < 2:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    src, dst = args[0], args[1]

    def opt(name, default=None):
        flag = '--' + name
        for i, a in enumerate(argv):
            if a == flag and i + 1 < len(argv):
                return argv[i + 1]
            if a.startswith(flag + '='):
                return a.split('=', 1)[1]
        return default

    fps = float(opt('fps', 12))
    scale = int(opt('scale', 1))
    min_px = int(opt('min-px', 0))
    bg = parse_bg(opt('bg'))
    multi_only = '--multi-only' in argv
    os.makedirs(dst, exist_ok=True)

    libs = sorted(d for d in os.listdir(src) if os.path.isdir(os.path.join(src, d)))
    made = skipped = failed = animated = 0
    for lib in libs:
        lib_dir = os.path.join(src, lib)
        files_dir = os.path.join(lib_dir, 'files')
        if not os.path.isdir(files_dir):
            continue
        decls = script_frame_counts(lib_dir)
        for fn in sorted(os.listdir(files_dir)):
            if not fn.lower().endswith(IMG_EXT):
                continue
            out_name = '%s__%s.gif' % (lib, os.path.splitext(fn)[0])
            out_path = os.path.join(dst, out_name.replace('/', '_'))
            try:
                n = render(os.path.join(files_dir, fn), out_path,
                           decls.get(fn.lower()), fps, scale, min_px, bg, multi_only)
            except Exception as e:                       # unreadable/odd asset
                failed += 1
                print('error: %s/%s: %s' % (lib, fn, e), file=sys.stderr)
                continue
            if n is None:
                skipped += 1
            else:
                made += 1
                animated += n > 1
    print('wrote %d gifs (%d animated, %d single-frame) to %s'
          % (made, animated, made - animated, dst))
    if skipped:
        print('skipped %d single-frame sheets' % skipped)
    if failed:
        print('failed on %d files' % failed)
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
