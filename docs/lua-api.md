# Lua API

Mods are folders. A later UI only has to generate this layout.

```
Mods/my_pack/
  mod.toml
  weapons.lua
  sprites/icon.png     # optional; blit not fully wired
```

Read [architecture.md](architecture.md) first: dispatcher, scripted missiles, one LuaActor type, extra panel later.

## `mod.toml`

```toml
id = "author.pack"
version = "0.1.0"
author = "you"
api_version = 1
entry = "weapons.lua"

[weapons]
register = ["author.pack.ice_bazooka"]
replace = ["bazooka"]
requires = []
conflicts = []

[actors]
register = ["author.pack.boss_mole"]   # LuaActor kinds
```

`replace` / `overlay` list **stock slot names**. v1 cannot add a 72nd `WeaponEntry`. Extra panel entries are later work.

Enabled packs: every `Mods/*/mod.toml`, sorted by `id`.

## `wa.log(message)`

Writes to `%LOCALAPPDATA%\NewPX\wkLua.log`. Failures also go to `%LOCALAPPDATA%\NewPX\wkluaerror.txt`.

## Weapons (slot overlay)

### `wa.weapons.replace({ weapon = "bazooka", ... })`

Overlay that **panel slot**. `weapon = "grenade"` is where it appears — that is the only slot choice. Optional `name`. Optional `copy_from` (fire/aim/strike taken from another stock weapon; panel row, `defined`, and scheme ammo stay on `weapon` so it does not disappear). `copy_from` is ignored when the pack spawns its own actor. `params.damage` / `bounce_pct` / `gravity_pct` / `wind_pct` / `sprite` / `sprite_columns`.

Two layers, same primitives. A later GUI emits the **status** table (checkboxes / dropdowns). The Lua textbox is `on_click` / `on_fire` / `on.frame` functions that call stock verbs. `sprite` on `replace()` is installed at gfx init so `wa.actors.spawn` can use that path later.

The weapon **is** a LuaActor. You spawn it from Lua on click or fire. Status tables (`on_click.spawn`) are the GUI shortcut; functions are the sandbox.

**Statuses** (`mods/examples/gif_donkey`) — native tick reads the flags:

```lua
on_click = {
  spawn = {
    sprite = "sprites/200w.gif",
    loop = true,
    from = "sky",
    origin = "bottom",
    gravity = true,
    collide_terrain = true,
    collide_worms = true,
    persist = true,
    drown = true,
    explode = { damage = 50, id = 100, below = true },
  },
}
```

**Verbs** (`mods/examples/gif_donkey_lua`) — click and tick are Lua. `sprite` on `replace()` installs the GIF; `on_click` spawns the body. Do not enable both packs (same grenade slot).

```lua
local function brain(a)
  a.vx = 0
  a:gravity()
  local hit = a:move({ terrain = true, worms = true })
  if hit.land then
    a:explode({ damage = 50, id = 100, at = "below" })
  end
  a.vx = 0
  a:advance()
  if a:in_water() then
    a:despawn()
  end
end

wa.weapons.replace({
  weapon = "grenade",
  sprite = "sprites/200w.gif",
  on_select = { cursor = true },
  on_click = function(click)
    wa.actors.spawn({
      sprite = "sprites/200w.gif",
      x = click.x,
      y = click.sky,
      origin = "bottom",
      owner = click.worm,
      on = brain,
    })
  end,
})
```

`click` is `x` `y` (cursor, 16.16), `sky` (stock strike Y), `slot`, `worm`. `wa.actors.spawn` during `on_click` / `on_fire` / `on_select` uses that worm as owner if you omit `owner`. Hybrid is allowed: keep `gravity = true` on a spawn table and only write `on.frame` for explode. If `on.*` is set on a spawn table, omitted auto flags default **off**.

`on_select = { cursor = true }` is the stock strike targeting cursor (airstrike X on click). Prefer `on_select = function(sel)` and spawn a LuaActor with a stock cursor sprite (`"cursor"` / `"cursorr"` … `"cursorc"`) if you want a cursor you own. `sel` is `x` `y` (pointer), `slot`, `worm`.

`params.sprite` is a pack-relative PNG strip (`sprites/ball.png`, 1 row, `sprite_columns` frames, default 8), a pack-relative GIF (`sprites/foo.gif`, native frames, delays kept), a **folder of PNG frames** (`sprites/absorb/` with `frame0000.png` …), or a named stock sprite (`grenade`, `bazooka`, `mine`, `holy`, `cursor` / `cursorr`–`cursorc`). `sprites = { "sprites/burst", "sprites/W.png", ... }` installs extra sheets on the same overlay so `on_fire` can spawn more than one look. Top-level `sprite` on `replace()` is still installed at gfx init. `on_click.spawn.sprite` still works for the GUI table. `sprite_columns` is ignored for GIFs and frame folders. A PNG/GIF/sequence is encoded as a WA `.spr` and installed into `sprite_ptrs` the way PX `LoadSprite` does in `InitGraphic` — immediately after `GfxHandler__LoadSprites` (`sprites.dat`). GIFs and frame folders loop when you call `a:advance()` (or `loop = true` on a status table). In a match the 256-colour table is the **current map palette** (`PaletteContext`), usually already full; frontend `Graphics/*.pal` RIFF files are not that table. File RGB is nearest-matched onto live slots (same 3|dR|+5|dG|+2|dB| metric as stock FindNearestCached) so ProcessSprite can MapColor without allocating. A green ball stays green only if the map has a green; on a desert map it becomes the nearest sand/brown. For exact colours, author the PNG/GIF with colours that already exist on WA maps.

### `wa.weapons.register({ id = "...", based_on = "bazooka", ... })`

Same overlay, `based_on` is the slot. **Does not create a new panel entry.** `id` must be listed in `mod.toml` `register` when that list is non-empty.

Per-weapon `on_fire` belongs to the **slot owner**. It is a Lua function or `{ spawn = { ... } }`. It does not chain with other packs.

```lua
-- Function: you spawn whatever you want at launch.
wa.weapons.replace({
  weapon = "bazooka",
  on_fire = function(fire)
    wa.actors.spawn({
      sprite = "sprites/proj.png",
      x = fire.x, y = fire.y, vx = fire.vx, vy = fire.vy,
      owner = fire.worm,
      on = function(a)
        a:gravity()
        local hit = a:move({ terrain = true, worms = true })
        if hit.land or hit.worm then
          a:explode({ damage = 45, id = 100, at = "center" })
          a:despawn()
        end
      end,
    })
  end,
})

-- Table: native spawns one LuaActor at the stock launch pos/vel.
on_fire = { spawn = { sprite = "sprites/proj.png", origin = "center", on = brain } }
```

`fire` is `x` `y` `vx` `vy` (16.16), `slot`, `worm`. Skipping stock `MissileEntity` is the point: the LuaActor *is* the projectile.

Pack sprite keys (`wa.sprites.key`) are namespaced `mod_id:relative/path`. Projectile `params.sprite` may be a PNG strip or a GIF. Named stock sprites remain a fallback.

## Actors (LuaActor)

One native body type. Lua is the brain: `on.frame` runs on `wa.msg.FRAME_FINISH` (same message pump as stock missiles). Native code still does gravity, feet vs landscape, and blit when you call those verbs (or when GUI statuses are on).

The actor is a world entity. You can spawn more of them, list them, list worms, and query the map.

```lua
local child = wa.actors.spawn({
  x = a.x, y = a.y, vx = 0, vy = 0,
  owner = a.owner,
  sprite = "sprites/200w.gif", -- already installed at gfx init, or a stock name
  origin = "bottom",
  on = function(b)
    b:gravity()
    b:move({ terrain = true, worms = true })
  end,
})

local worms = wa.worms.all(a)
local near = wa.worms.nearest(a)
for _, crate in ipairs(wa.crates.all()) do
  -- crate.x / crate.y / crate.kind
end
if wa.land.solid(a.x, a.y + 0x10000) then
  -- dirt under this body (16.16)
end
```

`a:spawn({ ... })` is the same, inheriting this actor’s owner, pos, and sprite when omitted.

`actor = "drop"` is still accepted as sugar for `on_select.cursor` + sky spawn + gravity + explode. Prefer `on_click = function` so a later UI generates callbacks, not more C flags.

### Actor handle (`a` in `on.frame`)

| Field / method | Meaning |
|---|---|
| `a.x` `a.y` `a.vx` `a.vy` | Position / velocity, WA 16.16. Writable. |
| `a:gravity()` | Stock gravity leaf (scale 1.0). Spawn-table `gravity = true` only auto-ticks; a brain that omits the checkbox still gets gravity when it calls the verb. |
| `a:move({ terrain, worms })` | Integrate velocity. `land` is rising-edge dirt under the sprite’s bottom opaque row (the platform, like stock donkey `masks.bmp`). `worm` is a separate TryMove hit. `a:explode` is stock `CreateExplosion`; the body keeps native blast impulse. |
| `a:advance()` | Next GIF / strip frame. |
| `a:explode({ damage, id, at = "below" })` | Stock `CreateExplosion`. Does not despawn. |
| `a:despawn()` | Free the body. |
| `a:spawn({ ... })` | Another LuaActor. Inherits owner / pos / sprite if omitted. |
| `a:sound(id [, flags])` | Stock `PlaySoundLocal`, or a pack WAV path from `sounds = { ... }`. |
| `a:in_water()` `a:on_feet()` | Water line / feet vs landscape. |
| `a.owner` | Firing worm handle (`x`/`y`/`team`/`weapon`/`slot`). |
| `a.kind` | `"actor"`. |
| `worm:carry(x, y)` | Keep this worm at `(x, y)` after stock physics this tick. Facing stays as at grab. Stock draws the body. |
| `worm:drop()` | Stop carrying. `vx` / `vy` you write next are the throw; stock then uses `wroll.spr`. |

`wa.actors.spawn(spec)` — same table as `a:spawn`. `wa.actors.all()` — live LuaActors. `wa.worms.all([from])` — HP>0 worms (SharedData). `wa.worms.nearest(from)` — closest other body. `wa.crates.all()` / `wa.mines.all()` / `wa.missiles.all()` / `wa.oil.all()` — stock objects from the entity tree (pos + `kind` only).

### Map (`wa.land` / `wa.world`)

Live landscape solid at `Landscape+0x910` (same grid Blas reads). Coordinates are WA **16.16** like `a.x`.

| Call | Meaning |
|---|---|
| `wa.land.solid(x, y)` | Dirt at this point. Out of bounds is air. |
| `wa.land.trace(x0, y0, x1, y1)` | First solid pixel on the segment, or `nil`. Hit is `{ x, y, hit = true }`. |
| `wa.world.sky` | Stock strike spawn Y (`DROP_SKY_Y`). |
| `wa.world.water()` | Water line as 16.16. |
| `wa.world.size()` | `{ w, h }` map pixels. |
| `wa.world.view()` | Camera `{ cx, cy }` in 16.16, `{ w, h }` viewport pixels. |
| `wa.world.pointer()` | Live OS mouse `{ x, y }` world 16.16, `{ down }` left button. Game thread only. |

### Sound

`a:sound(id)` or `a:sound("sounds/foo.wav")` or `wa.sound.play(handle, id [, flags])`. A number is a stock sound index. A pack WAV path must also be listed in `sounds = { ... }` on `replace()`; wkLua loads it into DSSound slot 451+ the way Jelly `registerCustomSound` does. Convert MP3 to PCM WAV first. Lockstep: only from `on.frame` / `on_click` / `on_fire`, not HUD.

Writes from `on.frame` / `on_click` / `on_fire` are lockstep-safe: both peers tick the same actor. Do not write these from `wa.on.hud`.

### `wa.actors.kind("boss_mole")`

Returns `mod_id.boss_mole`. Register the kind in `[actors] register`. Spawn instances with `wa.actors.spawn` / `on_fire` / `on_click`.

## Physics — scripted stock missiles

Behaviours (bounce, stickiness, wind, spin kick) are **Lua**. The engine does not ship those as native options.

`CollisionManager__apply_projectile_gravity` (`0x004FCB30`) is the gravity **leaf**:

- `speed_y += cm_gravity * gravity_scale` (`entity+0x60`)
- `speed_x += cm_wind * wind_scale` (`entity+0x5C`) if a 1px wind probe is clear
- both × `damp` (`entity+0x58`)

Land/bounce is `CollisionManager__rope` later in the tick. A floor bounce shows up **next** gravity call as `vy` reversing from a downward fall.

Values are WA **16.16** (`0x10000` = 1.0). Y grows downward.

v1 gravity/missile hooks run for **every missile**. Per-weapon filtering needs a missile → owner map from the fire wrap.

### `wa.on.gravity_before(function(p) ... end)`

Before stock gravity+wind+damp. Write `wind_scale` / `gravity_scale` / `damp`. `cm_wind` / `cm_gravity` are read-only CollisionManager globals.

### `wa.on.gravity_after(function(p) ... end)`

After stock. Write `vx` / `vy` / pos. Floor bounce: previous `vy > 2*0x10000` and current `vy < 0`.

### `wa.on.missile_frame(function(p) ... end)`

Missile `HandleMessage` around `wa.msg.FRAME_FINISH` (2). `p.phase` is `"pre"` or `"post"`. Count this message for rest timers; do not arm from wall-clock.

## Lockstep input

### `wa.on.worm_message(function(worm, msg) ... end)`

Worm `HandleMessage`. `worm` is a handle: `id`, `team`, writable `x` / `y` / `vx` / `vy` (16.16). Writes here run on the lockstep pump (both peers). Do not write worm pos from HUD.

Enter = `wa.msg.JUMP` (`0x24`), Backspace = `wa.msg.JUMP_UP` (`0x25`). Those messages already replicate. The worm stays put if the scheme is **anchored**. Returning `"eat"` skips stock; only do that when the scheme already ignores the action.

Custom lockstep packets: standby. See [determinism.md](determinism.md).

## HUD (presentation only)

### `wa.on.hud(function() ... end)` / `wa.hud.line(text [, x, y])`

After the stock GameRender wait CALL. Must not write physics. `y < 0` stacks from the top-left.
