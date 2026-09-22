# Fog of war

wkFOW's fog-of-war, integrated into WormForge as a first-class engine feature
with all gameplay settings in Lua. The TOML scheme files and lobby chat
commands of the original collapse into a single `wa.fog.set` call.

Scope: vision circles/cones, line of sight, and terrain memory (`discover`).
CTF, decorative bonfires/streetlights, replay recording, and lobby sync are not
ported. Fog is **presentation-only** — it never touches WA's deterministic
simulation, so it cannot desync; a vanilla client in the same match simply sees
no fog.

## Architecture

The work is split the same way as the GPU sprite renderer: a pure, host-testable
crate, and a thin WA-facing glue.

```
crates/fog            pure visibility model — no WA offsets, no COM, no mlua
  src/settings.rs     the settings registry (port of wkFOW's Settings::Def)
  src/vision.rs       shadowcast + two-tier LOD + parallel CastPool + discovery
  src/discovery.rs    terrain memory (seen/remembered bitmaps, three modes)
crates/engine/src/fog.rs   WA glue: reads vision sources from the entity tree,
                      builds the screen-space L8 mask each render frame
crates/render         the fog multiply pass (fog.hlsl + wf_gpu.cpp fog texture)
crates/lua-host       the wa.fog Lua namespace
```

The parallel shadowcast and the two-tier LOD are ported at full fidelity because
profiling showed they are what meet WA's frame budget on real maps — they are
load-bearing, not optional. The fork-join `CastPool` casts the 8 octants of a
dirty source in parallel and joins within the frame, so near and far light are
always built from the same source position (no tier seam).

## Rendering

The fog is one fullscreen multiply-blend quad drawn in the second scene the GPU
renderer already runs after WA's composite (`SRCBLEND=ZERO, DESTBLEND=SRCCOLOR`
scales the framebuffer by the visibility value). It is drawn **after** the GPU
sprites so world and sprites are darkened alike, and the HUD is exempted by
compositing the HUD region (learned from the same framebuffer diff the sprite
occlusion uses) into the mask as fully-visible before upload. If WA's D3D9
shader renderer is unavailable, fog is off with a single logged warning — never
a black screen.

## Lua API

A pack declares it uses fog in `mod.toml`:

```toml
[fog]
enable = true
```

Then drives it from its entry script:

```lua
-- Set the scheme. Starts from the current scheme, validates every key against
-- the registry, and only then replaces the live scheme — a bad key raises a
-- Lua error naming the setting and its range, and leaves the fog untouched.
wa.fog.set({
  circle_radius = 250,   -- awareness circle around each worm, landscape px
  cone_width = 45,       -- aim cone width, degrees (360 = full circle)
  cone_length = 0,       -- aim cone reach, px (0 = no cone)
  los = true,            -- terrain blocks vision
  discover = 1,          -- terrain memory: 0 off, 1 stale, 2 live, 3 premapped
  view_ownership = 3,    -- 1 my alliance, 2 active worm only, 3 everyone
  explosion_circle = 40, -- temporary public light past a crater's rim
  fog_floor = 0,         -- brightness floor percent (0 = full darkness)
})

wa.fog.enable(true)            -- master switch (needs [fog] in mod.toml)
wa.fog.enabled()               -- -> boolean
wa.fog.get("circle_radius")    -- -> current value, or nil if unknown
wa.fog.defaults()              -- -> { name = default, ... } from the registry
wa.fog.visible(x, y)           -- -> 0..255 light at a world 16.16 position
```

`wa.fog.enable` is gated on the `[fog]` declaration (an undeclared pack logs a
skip, like an undeclared weapon slot). The read verbs — `get`, `defaults`,
`enabled`, `visible` — are open to any pack.

The full settings list with defaults and ranges lives in
`crates/fog/src/settings.rs` (`SETTINGS`), the single source of truth the Lua
validation reads. See `mods/examples/fog_demo/` for a working pack.

## Follow-ups (not yet wired)

- **Terrain-mutation hooks**: `engine::fog::notify_explosion` and
  `notify_terrain_changed` are public but not yet called — the WA land-write
  detour that would invoke them is not installed. Until then, a terrain change
  under a *stationary* source does not re-open its sight lines (moving sources
  and scheme changes re-cast normally).
- **view_ownership narrowing**: v1 behaves as `view_ownership = 3` (everyone
  lights the map). Alliance/active-worm narrowing needs the DDMain team table
  and local machine-id offsets mapped.
- **Animal vs projectile**: all `CLASS_MISSILE` entities are treated as
  projectiles; the animal discriminator offset is not mapped.
