#!/usr/bin/env python3
"""Bake the standalone sentry pack from an extracted weapon_sentry_gun library.

Each team has 30-frame chunks: 120 idle rotations followed by 120 rotations for
each of the two firing frames. Truecolor art is the default; --palette-art
reproduces the older posterized ramp. Layer order is base, barrel/muzzle, housing.
Worm equip and put-away sheets use the source wsgnlnk/wsgnbak sequences.
Rotations use nearest-neighbour pixel sampling.

Usage: bake_sentry.py <extracted-lib-dir> <out-dir> [--steps 120]
Requires Pillow. No source scripts or game binaries are copied into the pack.
"""
import argparse
import math
import os
import sys

from PIL import Image

# team index -> RGB multiplier, in WA's colour order. The original multiplied
# by muted factors (0.65/0.15) which render dark; these are the pure channels,
# so a greyscale housing recolours to full-brightness team colour.

# Sprites are snapped to the map's palette, which holds only ~106 colours in a
# live match and is mostly earth tones. A smooth ramp produced ~254 colours per
# sheet and every one was remapped (quantized=254 in the log); a dark saturated
# blue has no neighbour there and its nearest match is black, which is why the
# blue turret came out almost unlit.
#
# So: keep the ramp short, and keep it off the floor. SHADES posterises each
# housing to a handful of steps, and FLOOR stops the darkest step from sitting
# where black is the closest match.
SHADES = 8
FLOOR = 0.34

TINT_GAIN = 2.0
# Secondary channels, measured off PX's own render rather than taken from the
# script. Sampling its red housing gives G/R 0.39 -> 0.47 and B/R 0.28 -> 0.39
# from shadow to highlight; the script's 0.15 against 0.65 would give a flat
# 0.23, which reads far more saturated than PX looks. The ratio RISING with
# brightness is a roll toward white, so the highlight knee stays.
# Secondary channels carry a warm bias -- PX's red holds more green than blue
# at every brightness (112 vs 91 at the top, 60 vs 43 at the bottom), which is
# what stops the housing reading as a flat primary. Values solved from those
# samples: G/R 0.26 and B/R 0.18 before the highlight roll.
MINOR = (0.22, 0.26, 0.18)
# Two-dominant teams (yellow/magenta/cyan) sit lower so the pair still reads.
MINOR_DUAL = 0.18
HIGHLIGHT_KNEE = 0.55
# Gentle. Solving PX's brightest sample (236,112,91) backwards gives a roll of
# about 0.08; at 0.45 the highlights washed out to G/R 0.75, nowhere near it.
HIGHLIGHT_STRENGTH = 0.08


def _tint(dominant):
    """Team factor from which channels are dominant, per PX's colour order."""
    solo = len(dominant) == 1
    return tuple(
        (0.65 if solo else 0.55) if i in dominant
        else (MINOR[i] if solo else MINOR_DUAL)
        for i in range(3)
    )


TEAM_TINTS = {
    1: _tint({0}),        # red
    2: _tint({2}),        # blue
    3: _tint({1}),        # green
    4: _tint({0, 1}),     # yellow
    5: _tint({0, 2}),     # magenta
    6: _tint({1, 2}),     # cyan
}

# PX applies these through a doubling colour mod -- SetColorMod(rgb, 6), which
# multiplies the texture by the tint and then by two. That is why its factors
# look dark on paper and bright on screen: red is 2*0.65 = 1.3x in R, clipping
# to white in the highlights, while G and B keep 2*0.15 = 0.3x of the source
# grey. Those surviving channels are the shading -- they are what makes PX's
# housing read as tinted metal rather than a flat red shape.
#
# The pure-channel tints this replaced were a palette-era workaround: they
# zeroed G and B so the ramp lived on one axis and stood the best chance of
# surviving quantisation. With truecolor there is nothing to survive, and
# zeroing those channels just throws the shading away.



def glow_alpha(im):
    """Approximate PX's additive muzzle blend with an alpha ramp.

    The flash was drawn additively (AddSpriteQ with q.blend = 1), so its black
    surround added nothing to the scene. A baked sprite cannot blend
    additively, and hard-keying only true black leaves the dark glow opaque --
    a visible box around the flash. Luminance becomes alpha instead, so the
    halo fades out the way the additive draw did.
    """
    im = im.convert('RGBA')
    px = im.load()
    for y in range(im.height):
        for x in range(im.width):
            r, g, b, _ = px[x, y]
            px[x, y] = (r, g, b, max(r, g, b))
    return im


def grey_ramp(shades=SHADES, floor=FLOOR):
    """The greys posterize() produces, as an explicit set."""
    out = []
    for k in range(shades):
        lum = floor + (k / (shades - 1)) * (1.0 - floor)
        v = min(255, int(round(255.0 * lum)))
        out.append((v, v, v))
    return out


def snap_to(im, allowed):
    """Force a composed frame back onto its allowed colours.

    Compositing blends: the art's anti-aliased edges mix with what is beneath
    them, so a housing built from 8 tints came out with 42. Every distinct
    colour asks the engine for a palette slot, so the frame is snapped back to
    the ramp once it is assembled.
    """
    px = im.load()
    cache = {}
    for y in range(im.height):
        for x in range(im.width):
            r, g, b, a = px[x, y]
            if a < 16:
                px[x, y] = (0, 0, 0, 0)
                continue
            key = (r, g, b)
            hit = cache.get(key)
            if hit is None:
                hit = min(allowed, key=lambda c:
                          3 * abs(c[0] - r) + 5 * abs(c[1] - g) + 2 * abs(c[2] - b))
                cache[key] = hit
            px[x, y] = hit + (a,)
    return im


def posterize(im, shades=SHADES, floor=FLOOR):
    """Shorten a greyscale part's ramp the same way tint() shortens the body.

    The tripod and barrel are untinted, so they skipped the tint path and kept
    their full ramp -- around a hundred greys per sheet, all of them remapped
    against a ~106 colour map palette. Fewer, better separated steps survive
    that far more predictably.
    """
    im = im.convert('RGBA')
    px = im.load()
    for y in range(im.height):
        for x in range(im.width):
            r, g, b, a = px[x, y]
            if not a:
                continue
            lum = (r + g + b) / 765.0
            lum = round(lum * (shades - 1)) / (shades - 1)
            lum = floor + lum * (1.0 - floor)
            v = min(255, int(round(255.0 * lum)))
            px[x, y] = (v, v, v, a)
    return im


def key_black(im, thresh=12):
    """Make near-black transparent: these sprites key on black, not alpha."""
    im = im.convert('RGBA')
    px = im.load()
    for y in range(im.height):
        for x in range(im.width):
            r, g, b, a = px[x, y]
            if r <= thresh and g <= thresh and b <= thresh:
                px[x, y] = (r, g, b, 0)
    return im


def team_ramp(factor):
    """The exact colours a team's housing may use: SHADES of them, no more.

    Every distinct colour in an opted-in sheet asks the engine for a palette
    slot, and the map only has so many to give. A smooth ramp produced dozens
    of near-identical tints per team; a fixed ramp costs SHADES slots and is
    reused by every frame, pose and sheet of that team.
    """
    fr, fg, fb = factor
    ramp = []
    for k in range(SHADES):
        t = k / (SHADES - 1)
        lum = FLOOR + t * (1.0 - FLOOR)
        hi = max(0.0, (t - HIGHLIGHT_KNEE) / (1.0 - HIGHLIGHT_KNEE))
        _ = hi
        ramp.append(tuple(
            min(255, int(round(255.0 * TINT_GAIN * lum * f)))
            for f in (fr, fg, fb)
        ))
    return ramp


def tint(im, factor, palette_limited=False):
    """Recolour a greyscale part to a team colour, keeping its shading.

    A flat multiply (grey * pure channel) produces a ramp that lives on one
    axis of colour space. Sprites are snapped to the map's 256-entry palette
    (quantize_to_map_palette), which holds few such colours, so the shades
    collapse onto one entry and the housing reads as a solid block.

    Instead the midtones carry the team hue while the highlights roll toward
    white, which spans a region the palette actually covers and keeps the
    distinct shades distinct after quantization.
    """
    ramp = team_ramp(factor)
    im = im.convert('RGBA')
    px = im.load()
    for y in range(im.height):
        for x in range(im.width):
            r, g, b, a = px[x, y]
            if not a:
                continue
            lum = (r + g + b) / 765.0
            if palette_limited:
                rgb = ramp[min(SHADES - 1, int(round(lum * (SHADES - 1))))]
            else:
                # PX's doubling colour mod, 2 * texture * tint, with the
                # highlights rolling toward white so the ratio climbs the way
                # the measurement does instead of staying flat.
                hi = HIGHLIGHT_STRENGTH * max(
                    0.0, (lum - HIGHLIGHT_KNEE) / (1.0 - HIGHLIGHT_KNEE))
                rgb = tuple(min(255, round(255 * TINT_GAIN * lum * (f + (1 - f) * hi)))
                            for f in factor)
            px[x, y] = rgb + (a,)
    return im


# Draw offsets taken from the original's Draw(): base sits slightly low, the
# turret pivots above it, and the barrel and muzzle ride out along the bore.
TURRET_SCALE = 0.8
BASE_OFFSET = (0, 2)
PIVOT = (0, 2 + int(round(TURRET_SCALE * -15)))
BARREL_OFFSET = (TURRET_SCALE * 17, 0)
# PX's flash is a dramatic tongue of flame well beyond the barrel; scaling it
# down with the housing (TURRET_SCALE) left it smaller than the source art.
MUZZLE_SCALE = 1.15
# sentry_muzzle.png is not just the bright taper: it carries a soft round
# halo trailing back from its own centre (out to roughly -24px at
# MUZZLE_SCALE, versus the bright core starting around -14px). The old
# TURRET_SCALE*14 offset placed that centre almost on the barrel's tip, so
# the halo washed back over the entire visible barrel instead of starting
# there. This places the core's leading edge at the tip (BARREL_OFFSET plus
# the barrel art's own ~10.4px half-length) and lets the halo trail onto the
# barrel from there, the way PX's does.
MUZZLE_OFFSET = (24.0, 0)

# A 64x64 canvas only has ~32px of headroom from its own centre, but the
# flare reaches roughly BARREL_OFFSET + MUZZLE_OFFSET (~25px) plus its own
# scaled half-width (~20px) beyond a pivot that already sits 10px off centre
# -- around 55px in the worst-case direction. That clipped the flare's own
# tip off the top of the sprite. Firing frames get a bigger canvas; the
# other poses have no flare and keep the compact one.
FRAME_SIZE = (64, 64)
FIRE_FRAME_SIZE = (144, 144)


def scale(im, f=TURRET_SCALE, resample=Image.NEAREST):
    return im.resize((max(1, int(round(im.width * f))),
                      max(1, int(round(im.height * f)))), resample)


def over(canvas, img, dx, dy):
    """Composite img onto canvas at an offset, centring same-size layers."""
    layer = Image.new('RGBA', canvas.size, (0, 0, 0, 0))
    ox = (canvas.width - img.width) // 2 + int(round(dx))
    oy = (canvas.height - img.height) // 2 + int(round(dy))
    layer.paste(img, (ox, oy))
    return Image.alpha_composite(canvas, layer)


def swing(offset, angle):
    """Rotate a bore-relative offset into world space for a turret angle."""
    ox, oy = offset
    r = math.radians(angle - 90.0)
    return (ox * math.cos(r) + oy * math.sin(r),
            -ox * math.sin(r) + oy * math.cos(r))


def rot(im, angle, resample=Image.NEAREST):
    """Rotate to a turret angle. Art faces right, which is the 90 reference."""
    return im.rotate(angle - 90.0, resample=resample, expand=False)


def bake_gpu(src, dst):
    """Bake for the GPU renderer.

    The palette bake spends 6 teams x 3 poses x 120 angles = 72 sprite slots,
    out of the 223 that every loaded pack shares, purely because WA's blitter
    cannot rotate or tint. The GPU renderer does both at draw time, so the
    same turret becomes:

        turret_base   1 frame   the tripod, which never rotates
        turret_gun    3 frames  housing + barrel, greyscale, upright

    Two sheets, zero WA sprite slots. The housing stays greyscale because the
    team colour is a vertex tint now; keeping the source's own shading is what
    makes it read as tinted metal rather than a flat colour, which the palette
    bake had to reconstruct with TINT_GAIN/MINOR/HIGHLIGHT_KNEE.
    """
    files = os.path.join(src, 'files')
    def load(n):
        return Image.open(os.path.join(files, n)).convert('RGBA')

    sprites = os.path.join(dst, 'sprites')
    os.makedirs(sprites, exist_ok=True)

    # The tripod, alone and upright.
    base = Image.new('RGBA', FRAME_SIZE, (0, 0, 0, 0))
    base = over(base, load('sentry_base.png'), *BASE_OFFSET)
    folder = os.path.join(sprites, 'turret_base')
    os.makedirs(folder, exist_ok=True)
    base.save(os.path.join(folder, 'frame000.png'))

    # Housing + barrel, upright, greyscale. Drawn about the pivot, so the
    # renderer's rotation (which is about the quad centre) matches PX's.
    body = load('sentry_body.png')
    barrel = load('sentry_gun.png')
    firing = load('sentry_gun_firing.png')
    muzzle = glow_alpha(load('sentry_muzzle.png'))
    barrels = [barrel] + [firing.crop((0, y, 64, y + 64)) for y in (0, 64)]

    folder = os.path.join(sprites, 'turret_gun')
    os.makedirs(folder, exist_ok=True)
    # Every frame of a sheet must share one canvas: the atlas packer sizes a
    # sheet from its first frame, so a larger later frame would be packed as
    # if it were the first frame's size and render clipped. The flare needs
    # the bigger canvas, so all three use it.
    for pose, barrel_frame in enumerate(barrels):
        frame = Image.new('RGBA', FIRE_FRAME_SIZE, (0, 0, 0, 0))
        frame = over(frame, scale(barrel_frame), BARREL_OFFSET[0], 0)
        frame = over(frame, scale(body), 0, 0)
        if pose == 1:
            # Additive at draw time now, so the luminance-to-alpha hack that
            # faked it for a baked sprite is not needed -- the muzzle keeps
            # its own colour and the renderer adds it.
            flare = rot(scale(muzzle, MUZZLE_SCALE, resample=Image.BICUBIC),
                        90.0, resample=Image.BICUBIC)
            frame = over(frame, flare, BARREL_OFFSET[0] + MUZZLE_OFFSET[0], 0)
        frame.save(os.path.join(folder, 'frame%03d.png' % pose))

    print('baked into %s' % sprites)
    print('  turret_base 1 frame, turret_gun %d frames' % len(barrels))
    print('  2 GPU sheets, 0 of 223 WA sprite slots '
          '(the palette bake used 72 for rotation alone)')
    return 0


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source')
    parser.add_argument('output')
    parser.add_argument('--steps', type=int, default=120)
    # WA's ProcessSprite crashed constructing a single 360-frame (~1.5MB)
    # sheet. The largest that load cleanly here are around 110KB (pooz's
    # 14 x 96x84). Slots are cheap now that angles are frames, so sheets are
    # split to stay in that range.
    parser.add_argument('--per-sheet', type=int, default=30)
    parser.add_argument('--gpu', action='store_true',
                        help='bake for the GPU renderer: one greyscale gun '
                             'sheet plus a static base, rotated and tinted at '
                             'draw time instead of baked per angle and team')
    parser.add_argument('--palette-art', action='store_true',
                        help='reproduce the earlier posterized palette artwork')
    args = parser.parse_args(argv[1:])
    src, dst, steps = args.source, args.output, args.steps
    per_sheet = args.per_sheet
    if steps != 120:
        parser.error('the Lua pack expects 120 rotation steps')

    files = os.path.join(src, 'files')
    def load(n):
        return Image.open(os.path.join(files, n)).convert('RGBA')

    if args.gpu:
        return bake_gpu(src, dst)
    prepare = posterize if args.palette_art else lambda im: im
    base = prepare(load('sentry_base.png'))
    body = load('sentry_body.png')
    barrel = prepare(load('sentry_gun.png'))
    muzzle = glow_alpha(load('sentry_muzzle.png'))
    firing = prepare(load('sentry_gun_firing.png'))
    barrels = [barrel] + [firing.crop((0, y, 64, y + 64)) for y in (0, 64)]

    # The anchor is the lowest opaque row (origin="bottom"). The muzzle glow
    # hangs below the tripod, so a firing pose would anchor ~12px lower than an
    # idle one and the turret visibly jumps each time the barrel animates.
    # Clipping every frame at the tripod's footline keeps that row constant;
    # the clipped glow would have been under the ground anyway.
    #
    # over() centres a part using the CANVAS's own size, so the same
    # BASE_OFFSET lands on a different absolute row once firing frames get a
    # bigger canvas (FIRE_FRAME_SIZE) than idle ones (FRAME_SIZE). The
    # footline has to be recomputed per canvas size rather than shared.
    def build_footline(size):
        footline = Image.new('RGBA', size, (0, 0, 0, 0))
        footline = over(footline, base, *BASE_OFFSET)
        floor_y = footline.getbbox()[3] - 1
        # The contact row's x-extent matters as much as its y. WA derives the
        # platform from the lowest opaque row (x0, x1, y), so if a downward-
        # pointing barrel or the glow lands on that row it widens x0..x1 and
        # the turret shifts sideways between frames -- the horizontal
        # counterpart of the vertical jump. Forcing that row to be exactly
        # the tripod's own feet makes the platform identical in every frame.
        feet_row = {}
        for x in range(footline.width):
            px = footline.getpixel((x, floor_y))
            if px[3]:
                feet_row[x] = px
        return floor_y, feet_row

    footlines = {size: build_footline(size) for size in (FRAME_SIZE, FIRE_FRAME_SIZE)}

    def clip_below_feet(img, floor_y, feet_row):
        px = img.load()
        for y in range(floor_y + 1, img.height):
            for x in range(img.width):
                r, g, b, a = px[x, y]
                if a:
                    px[x, y] = (r, g, b, 0)
        for x in range(img.width):
            if x in feet_row:
                px[x, floor_y] = feet_row[x]
            else:
                r, g, b, a = px[x, floor_y]
                if a:
                    px[x, floor_y] = (r, g, b, 0)
        return img

    sprites = os.path.join(dst, 'sprites')
    os.makedirs(sprites, exist_ok=True)
    made = 0

    # One frame folder per team: a multi-frame sheet costs a single sprite
    # slot however many frames it holds, and a:frame() holds the actor on one
    # of them. Baked rotations used to cost a slot each, which capped the
    # turret at 28 angles; as frames they are practically free.
    for team in range(1, 7):
        tinted = tint(body, TEAM_TINTS[team], args.palette_art)
        for pose, barrel_frame in enumerate(barrels):
            for i in range(steps):
                chunk = i // per_sheet
                folder = os.path.join(
                    sprites, 'turret_t%d_p%d_s%d' % (team, pose, chunk))
                os.makedirs(folder, exist_ok=True)
                angle = i * 360.0 / steps
                canvas_size = FIRE_FRAME_SIZE if pose == 1 else FRAME_SIZE
                floor_y, feet_row = footlines[canvas_size]
                frame = Image.new('RGBA', canvas_size, (0, 0, 0, 0))
                frame = over(frame, base, *BASE_OFFSET)
                bx, by = swing(BARREL_OFFSET, angle)
                frame = over(frame, rot(scale(barrel_frame), angle),
                             PIVOT[0] + bx, PIVOT[1] + by)
                # Mirror in the housing's LOCAL coordinates before rotating.
                # Mirroring the rotated result reverses its aim on diagonals.
                housing = scale(tinted)
                if angle < 180.0:
                    housing = rot(housing.transpose(Image.FLIP_LEFT_RIGHT),
                                  angle + 180.0)
                else:
                    housing = rot(housing, angle)
                frame = over(frame, housing, *PIVOT)
                # PX shows the separate additive muzzle during the first half
                # of its two-frame firing cycle, at the end of the bore, drawn
                # on top of the housing -- drawing it before left the housing
                # painted back over most of the flare, hiding all but the
                # sliver that poked out past its silhouette. It is also soft,
                # blurred art rather than pixel art, so it is scaled and
                # rotated with bicubic resampling instead of the nearest-
                # neighbour used everywhere else -- nearest turned its smooth
                # gradient into visible blocky rings.
                if pose == 1:
                    mx, my = swing((BARREL_OFFSET[0] + MUZZLE_OFFSET[0], 0), angle)
                    flare = rot(scale(muzzle, MUZZLE_SCALE, resample=Image.BICUBIC),
                                angle, resample=Image.BICUBIC)
                    frame = over(frame, flare, PIVOT[0] + mx, PIVOT[1] + my)
                if args.palette_art:
                    snap_to(frame, team_ramp(TEAM_TINTS[team]) + grey_ramp())
                clip_below_feet(frame, floor_y, feet_row)
                frame.save(os.path.join(folder, 'frame%03d.png' % (i % per_sheet)))
    made += 6 * len(barrels) * ((steps + per_sheet - 1) // per_sheet)

    # The damage carrier should not be seen -- the muzzle belongs at the
    # barrel, and drawing it at the ray impact was the old implementation's
    # bug. It cannot be blank, though: a fully transparent 1x1 sprite gave
    # encode_spr an empty palette (it falls back to one colour) and crashed
    # WA's ProcessSprite on load. One nearly-black pixel in an 8x8 frame keeps
    # the sprite well-formed and is invisible at game scale for the single
    # frame it lives.
    carrier = Image.new('RGBA', (8, 8), (0, 0, 0, 0))
    carrier.putpixel((4, 4), (16, 16, 16, 255))
    carrier.save(os.path.join(sprites, 'impact.png'))
    made += 1

    for suffix in ('', 'u', 'd'):
        for action, source in (('equip', 'wsgnlnk'), ('unequip', 'wsgnbak')):
            strip = load(source + suffix + '.png')
            frames = [strip.crop((0, y * 60, 60, y * 60 + 60)) for y in range(10)]
            folder = os.path.join(sprites, action + suffix)
            os.makedirs(folder, exist_ok=True)
            for i, frame in enumerate(frames):
                frame.save(os.path.join(folder, 'frame%03d.png' % i))
        made += 2

    # sounds the brain actually plays, copied verbatim into the pack
    snd_out = os.path.join(dst, 'sounds')
    os.makedirs(snd_out, exist_ok=True)
    for wav in ('sentryplace.wav', 'sentrybeeps2.wav',
                'sentryfireloop.wav', 'sentryfireend.wav',
                'sentryservostart.wav', 'sentryservoloop.wav',
                'sentryservoend.wav'):
        src_wav = os.path.join(files, wav)
        if os.path.exists(src_wav):
            with open(src_wav, 'rb') as f_in, open(os.path.join(snd_out, wav), 'wb') as f_out:
                f_out.write(f_in.read())
    print('baked into %s' % sprites)
    print('  turret 6 teams x %d frames (%.2f deg step)'
          % (steps * 3, 360.0 / steps))
    chunks = (steps + per_sheet - 1) // per_sheet
    print('  split into 6 teams x 3 poses x %d sheets of <=%d frames = %d slots'
          % (chunks, per_sheet, 6 * 3 * chunks))
    print('  invisible impact carrier    = 1 slot')
    print('  total %d of 223 custom sprite slots' % made)
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
