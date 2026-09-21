# Colour Test

A demo pack for the GPU renderer. It occupies the **mortar** slot and throws
like a grenade: charge the power bar, it arcs, bounces off slopes, and
detonates on a three-second fuse.

Nothing about it is tactically interesting. Everything about it is something
WA's indexed blitter cannot do.

## What to look for

| on screen | why it was impossible before |
|---|---|
| the orb spins smoothly | rotation was baked, one WA sprite slot per angle — the sentry gun spent **72 of 223** shared slots on exactly this |
| the orb's colours | `sprites/prism.png` carries ~850 distinct colours in one 32×32 frame; an indexed palette has 256 entries *total*, shared with the terrain |
| the halo and trail have soft edges | the palette path thresholded alpha — below 16 dropped, everything else forced opaque — so a gradient became a hard-edged disc |
| they brighten what is behind them | additive blending. The old muzzle flash faked it with a luminance→alpha trick that only worked against dark backgrounds |
| the trail and burst run through the spectrum | one greyscale 16×16 sheet, tinted per draw. Six baked copies of every sentry frame existed only because this was not available |

Cost in WA sprite slots: **zero**. `gpu_prefix` routes all three sheets into
the texture atlas, and none of them touches the map palette.

## Sheets

Generated, not drawn:

```sh
mods/examples/colourtest/bake_sprites.py
```

Writes `sprites/{prism,halo,spark}.png` and a preview to `tmp/`.

## Known gaps

- **Wind does not affect it.** A stock grenade drifts; this projectile is a
  Lua actor with `ENT_WIND_SCALE` at zero.
- **The fuse is fixed at 3 seconds.** The grenade slot's 1–5 selection is not
  exposed to packs yet, so the number you pick is ignored.
- **Bouncing is approximate.** The surface normal is estimated by probing
  `wa.land.solid` in twelve directions, not read from WA's own collision.
  Good enough to read as a grenade; not bit-identical to one.
