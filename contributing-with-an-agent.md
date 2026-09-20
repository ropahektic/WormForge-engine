# Contributing with a coding agent

>>> Feed this document to your coding agent <<<<

Open this repo in the agent, point it at this file and [README.md](README.md), then describe the weapon or behaviour you want. You do not need to paste the rules into chat — the agent should read them here.

New power is a **Lua pack** or one **verb**. Not a dump of `WA.exe` into scripts.

## Packs

A pack is a folder (`mod.toml` + Lua + optional sprites/sounds). Copy it to `Mods/` next to `WA.exe`. No compiler.

```
mods/examples/my_thing/
  mod.toml
  weapons.lua          (or init.lua)
  sprites/…
  sounds/…
```

v1 overlays a stock panel slot. It cannot add a 72nd icon. Several packs may overlay the same slot: they all load; **first loaded `id` keeps the slot**, later claims for that slot are skipped. `gif_donkey` and `gif_donkey_lua` both overlay grenade on purpose — statuses vs a Lua brain — and that skip is the system working.

The engine (`crates/`, `wkLua.dll`) is only for verbs Lua cannot call yet. Prefer a pack.

Needs: WA **3.8.1** + WormKit, this repo, `wkLua.dll` next to `WA.exe`. Test in a match; logs are `%LOCALAPPDATA%\NewPX\wkLua.log` and `wkluaerror.txt`. Then open a PR.

| You want… | Start from |
|---|---|
| Log a line, no weapon | `hello` |
| Same bazooka, new sprite / gravity / wind | `circle_bazooka` |
| Click the map, a body falls and explodes | `gif_donkey_lua` (Lua) or `gif_donkey` (statuses) |
| Cut-in, extra sprites, sounds on fire | `wormcraft` |
| Click a worm, drag, throw | `worm_toss` |
| Selecting the slot *is* a mini-game | `pooz` |
| Hotkey that stock-selects an existing gun | `q_bazooka` |

## Pack contract

Lua sees `wa` and handles (`a`, `worm`). Not C++ names, not `unknownC`, not pointers.

A weapon is usually a **LuaActor**: `wa.weapons.replace` then `on_click` / `on_fire` spawn a body with a brain. Status tables (`gravity = true`, …) are the GUI path (`gif_donkey`). Overlay a stock missile (`params`, `wa.on.gravity_*`) only when you still want stock flight (`circle_bazooka`).

- Positions / speeds are WA **16.16** integers (`0x10000` = 1 pixel).
- Space is shoot (hold to charge bazooka-type, release to fire). Enter / Backspace are `wa.msg.JUMP` / `JUMP_UP`. `wa.world.keys().jump` is W (POOZ), not Space.
- Sim on messages both peers already see. HUD does not write physics. `keys()` / `pointer()` are local until extra lockstep packets exist.
- No `math.random`, no clock, no filesystem from Lua. Assets stay inside the pack.
- `api_version = 1`. Renaming the Lua API is a breaking change: bump it.

`register` does not add a panel row; it still occupies `based_on`.

## Verbs (engine)

If the current `wa.*` surface cannot do it: wrap stock 3.8.1, bind a handle API, show it in one example. Target `i686-pc-windows-msvc`. Wrap, then original — never trap `FireWeapon` / `InitWeaponTable`, never stack VFTs, never grow the 71-slot table. Details: README.

Draw primitives belong in the DLL. Named FX (lightning, fire) belong in Lua. No second renderer. Do not vendor `Resources/` or GPL game-replacement sources. Do not drive-by rename the whole API in the same PR as a pack.

## Lua surface

- `wa.log`, `wa.weapons.replace` / `register`, `wa.actors.spawn` / `all` / `kind`
- Actor: `x y vx vy facing owner kind`, `a:gravity` `a:move` `a:advance` `a:look` `a:explode` `a:despawn` `a:spawn` `a:sound` `a:in_water` `a:on_feet`
- Worm: `id team slot weapon kind visible turn x y vx vy`, `worm:carry` `worm:drop` `worm:equip`
- `wa.worms.all` / `nearest`, `wa.crates` / `mines` / `missiles` / `oil` `.all()` (pos + `kind`)
- `wa.land.solid` `trace`, `wa.world.sky` `water` `size` `view` `pointer` `keys`
- `wa.sound.play`, `wa.on.gravity_before` / `gravity_after` / `missile_frame` / `worm_message` / `hud`, `wa.hud.line`
- `wa.msg.FRAME_FINISH` `EXPLOSION` `JUMP` `JUMP_UP` `FIRE_WEAPON` `RELEASE_WEAPON` `START_TURN`

Sprites: pack PNG strip (`sprite_columns`), GIF, frame folder, or stock names (`grenade`, `bazooka`, `mine`, `holy`, `cursor` …). Map palette applies. Sounds: pack WAV on the overlay, or a stock index.

Leave the root README alone unless asked. List a new example in `mods/examples/README.md`.
