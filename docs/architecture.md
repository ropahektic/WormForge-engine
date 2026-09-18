# Architecture

wkLua is an **in-process object runtime** for stock WA 3.8.1. User mods are Lua. Native code wraps a few stock entry points and dispatches to the pack that **owns** the weapon or actor. It is not Project X, PXS, or vtable stacking.

PX’s power was: spawn objects, run every tick, collide, draw, put guns on the panel. That does **not** require `override CWorm::FireFinal` in user scripts.

## Dispatcher (not VFT stacking)

```
stock fire / HandleMessage / gravity / HUD
        │
        ▼
  wkLua trampoline (wrap, then original)
        │
        ▼
  dispatch by namespaced id
        │
        ▼
  only the pack that owns that id
```

Authors register `example.ice.ice_bazooka`, not a global `FireFinal` override. Two packs never silently chain (`^` / `^^` / `^^^`). First loaded `id` wins a stock slot; the rest of the later pack still loads. See [mod-ownership.md](mod-ownership.md).

Lua sees **handles and tables**, not WA C++ layouts. Native maps those to 3.8.1 offsets (`re/`). Pack sprites are `mod_id:relative/path`. A projectile PNG strip or GIF is encoded as a stock `.spr` and installed the way PX does in `InitGraphic`: ConstructSprite + ProcessSprite right after `LoadSprites` (`sprites.dat`), quantized onto the **map palette** already in `PaletteContext`. GIF frames loop via `animation_phase` on stock missiles, or on the LuaActor when `a:advance()` / `loop` is on. Stock `SetLayerVisibility(3, 1)` commits that table.

## Three object kinds

### 1. LuaActor (the entity)

This is the weapon. One native WorldEntity type; Lua is the brain. You spawn it from click (`on_click` function or GUI `on_click.spawn`), from fire (`on_fire` function or `on_fire.spawn`), or from Lua (`wa.actors.spawn` / `a:spawn`). You write `x`/`y`/`vx`/`vy`, call stock verbs, spawn children, and see worms, crates, mines, missiles, and the solid map.

Statuses (`gravity`, `loop`, `collide_*`, `persist`, `drown`, `explode`) are GUI checkboxes — native runs them when there is no brain. Verbs (`a:gravity`, `a:move`, `a:explode`, `a:advance`, `a:despawn`, `a:spawn`, `a:sound`) are the Lua textbox. `on_click` / `on_fire` as functions own spawn. `sprite` on `replace()` installs the sheet.

### 2. Scripted stock missiles (leftover bus)

Packs that only overlay `WeaponEntry` params still ride a stock `MissileEntity`. Global `wa.on.gravity_before` / `gravity_after` / `missile_frame` run for **every** missile. Prefer `on_fire` + a LuaActor so the slot owns the body.

### 3. Extra panel entries (later)

PX added weapon **pages**. That is UI + ammo, not a requirement to grow `WeaponEntry[71]` first. Options: unused slots, a Lua panel that fires through a dummy stock slot, or grow the table after InitWeaponTable allocation is fully RE’d. Behaviour is (1)+(2); the panel is separate.

## Native wraps

**Wrap, never trap** `FireWeapon` / `InitWeaponTable`.

| Status | Stock site | Role |
|--------|------------|------|
| wired | `InitWeaponTable` | overlay claims |
| wired | `apply_projectile_gravity` | Lua scales then velocity |
| wired | missile / worm `HandleMessage` | frames / lockstep keys |
| wired | GameRender wait CALL | HUD only |
| wired | fire path (`FireWeapon__StrikeFire`) | Lua `on_click` skips AirStrikeEntity |
| wired | fire path (`FireWeapon__CreateWeaponProjectile`) | Lua `on_fire` skips MissileEntity and spawns a LuaActor |
| wired | LuaActor vtable + `register_entity` | body Lua configured (click, fire, `wa.actors.spawn`) |
| wired | `Landscape+0x910` / entity tree / `PlaySoundLocal` | `wa.land`, crates/mines/missiles, `a:sound` (read/wrap, no VFT stack) |
| standby | custom lockstep packets | extra buttons; Jump/JumpUp already replicate |

Call stock `WorldEntity__Constructor` when spawning actors. Do not reimplement `CollisionManager__rope`.

## Lockstep

Anything that moves or deals damage runs on the **game thread** and on messages both peers already see. HUD must not write physics. `math.random` is stripped; WA RNG is not wired yet. Jump/JumpUp (Enter/Backspace) already ride lockstep; eating them is optional and only safe on an anchored scheme.

## What we refuse from PX

- Opaque `.pxl` / `.pxs` and a `Libs/` dumping ground
- Dumping `CWorm*` field offsets into scripts
- Stacking overrides on stock classes
- Global numeric sprite IDs
- Per-author `utils_*` forks as the composition model
