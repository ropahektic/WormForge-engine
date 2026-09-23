# The GPU sprite renderer

Custom sprites are packed into a texture atlas and drawn as rotated, scaled,
tinted quads after WA's own fullscreen composite — instead of being installed
as WA sprites and blitted by WA. They occupy **none** of the 223 sprite slots
every loaded pack shares, and never touch the map palette.

What that buys, measured on the sentry gun:

| | palette path | GPU path |
|---|---|---|
| turret sheets | 6 teams × 3 poses × 120 angles | `base` 1 + `gun` 3 + `flare` 2 |
| WA sprite slots | **72 of 223** | **0** |
| rotation | baked, 3° steps | exact, per frame |
| team colour | 6 baked copies | vertex tint over greyscale |
| muzzle flash | luminance→alpha fake | real additive blend |

## Using it

```lua
params = { gpu_prefix = "sprites/turret_" }   -- which sheets go to the atlas

a:gfx({
  angle = 37,          -- degrees clockwise; art faces right at 0
  scale = 1.5,         -- 1.0 is the sheet's native size
  tint  = 0xFF8000,    -- multiplies the texel; greyscale art recolours cleanly
  blend = "add",       -- premultiplied alpha 0: adds instead of covering
  depth = 0x10000,     -- occlusion band, see below
})
```

### What is on the GPU path without asking

`worm_sprites` (the `weaponlnk*` / `wthrow*` aim-hold and draw sheets) go to
the atlas **by default**. They are only ever blitted through the stock
`RQ_DrawSpriteLocal` intercept — never handed to a `MissileEntity` — so the
8-bit palette wash bought nothing but washed-out colours. The web editor's
"generate custom" aiming GIFs therefore render truecolor with no extra line
in the pack. Opt out per pack with:

```lua
params = { gpu_worm_sprites = false }   -- keep the palette path
```

The weapon body sprite (`sprite = ...`) stays a real WA slot regardless of
prefix matching: `MissileEntity` indexes WA's sprite table with it.

### Stock cursor as an actor

`wa.cursor.spawn{ owner = worm, on = fn }` spawns an actor that *is* the world
cursor: native code snaps it to `wa.world.pointer()` every frame before `on`
runs, and with the default `sprite = "cursor"` it wears the stock land cursor
in the owner's team colour, playing WA's ease-in (Teleport's look). Pass any
pack sheet as `sprite` for a custom cursor (add it to `gpu_prefix` for
truecolor). Pair with `on_select = { cursor = true }` (land-cursor input mode)
and `eat_fire = true` (the click reaches `on_click` and nothing else); WA's own
cursor draw is skipped for such a slot so exactly one cursor is on screen.

Each field has a single-purpose setter too, for when only one is changing:

```lua
a:angle(37)          -- degrees clockwise
a:scale(1.5)
a:tint(0xFF8000)     -- keeps the current blend mode
a:blend("add")       -- keeps the current colour; "normal" is the other mode
```

**Only the fields you name are written.** `a:gfx{angle = 37}` leaves scale,
tint and depth exactly as they were, and the setters above are independent of
one another — `a:blend("add")` does not discard the team colour. Without that
rule the setters could not exist at all: each would be a full overwrite of the
other three.

A blend mode that is not `"normal"`/`"alpha"` or `"add"`/`"additive"` is an
error rather than a silent fallback, because a typo would otherwise surface
only as a missing glow in a screenshot.

These are Rust methods, not Lua sugar over `a:gfx`. A fork-side Lua prelude
was the original plan — it would have kept the footprint in
`crates/lua-host/src/actor.rs` smaller, which matters for merging upstream —
but mlua protects userdata metatables with `__metatable = false`, so a script
cannot reach the method table to extend it. `Lua::register_userdata_type` does
not help either: the `impl UserData` registration wins over it.

`a:gfx` is presentation only. It is stored in the actor struct, read solely by
the renderer, and can never reach the simulation — lockstep safety is
structural rather than a rule to remember.

## Occlusion

The renderer draws *after* WA's composite, so without a mask its sprites would
sit on top of everything. The mask is built per pixel from two sources,
because the two occluders differ in kind:

**The HUD is its own draw**, so the dispatch is split around it and a
framebuffer diff identifies it exactly.

**Terrain is not.** Terrain and the soil a crater exposes arrive as a single
`tiled_terrain` command, so no split of the render queue can separate them —
and they must behave differently, since soil is background and a sprite
standing in a crater has to draw *over* it. The collision mask can tell them
apart: solid ground is solid, exposed soil is not. wkFOW reached the same
conclusion for its own sprite occlusion; `Vision::occludesSprites` documents
it, including why matching soil's palette indices is the wrong answer (a table
per level theme, broken on custom levels).

Sampling is confined to the pixels sprites actually cover. A full-surface
sweep would be a million lookups a frame to answer a question about a few
thousand pixels.

### Two bands

`depth` selects what may hide a sprite:

- **at or behind `0x180000`** — solid ground and the HUD hide it (the sentry's
  tripod, whose sunk feet should disappear into the ground)
- **in front of `0x180000`** — only the HUD hides it (the gun and its muzzle
  flash, mounted above the landscape)

**Worms never occlude either band**, and that is not expressible as a
render-queue depth at all. WA draws terrain (`0x180000`) behind worms
(`0xd0xxx`), so "in front of worms and behind terrain" needs
`0x180000 < d < 0xd0000` — an empty interval. A painter's algorithm can only
express a transitive order. Deciding per pixel by source escapes that.

## Costs, measured on this machine

At 1280×800 in the live GOG prefix (`docs/gpu-renderer-s0.md`):

- 500 sprites in one batched draw: **0.15 ms**; the same 500 as individual
  `DrawPrimitiveUP` calls: **45.6 ms**. Batching is the design, not an
  optimisation.
- Full-surface mask upload: **0.30 ms**, so dirty-rect *uploads* were dropped
  as pointless. Dirty rects matter for CPU collision sampling instead.
- A second band costs one more mask upload and draw call.

## Requirements and failure

The GPU path needs WA's D3D9 shader renderer. A GPU sheet has **no palette
fallback** — if that renderer is unavailable the sprites are absent, not
degraded. The renderer says so loudly once per match, naming the two real
causes: the software renderer is in use, or another module owns the three
D3D9 hook sites.

`docs/gpu-renderer-s0.md` records the standalone probes that established
viability before any of this was wired into WA, and the measurements above.
