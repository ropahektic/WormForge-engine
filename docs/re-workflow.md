# Reverse-engineering workflow

Ghidra on stock **WA.exe 3.8.1** is the source of truth. `re/*.toml` stores Ghidra virtual addresses at image base `0x00400000`. Runtime addresses are `va + (GetModuleHandle(NULL) - 0x400000)`.

We **wrap** stock functions and dispatch to Lua. We do not trap `FireWeapon` / `InitWeaponTable`, and we do not stack VFTs the way Project X did.

## Adding an address

1. Search the local openWA `re/` catalog (not shipped) to see if the function or field is already named.
2. Confirm in Ghidra: decompile, then check `RET imm16` in disassembly. Do not trust the decompiler for calling convention.
3. Write or update a file under this repo’s `re/` (one class or unit per file).
4. If we hook it, add a `[[hook]]` in `hooks/` with `action = "wrap"` / `wrap_after` / `call_site_after`. **Never trap** stock `FireWeapon` / `InitWeaponTable`.

## Already confirmed for v1

| Name | VA | Notes |
|------|----|--------|
| `InitWeaponTable` | `0x0053CAB0` | stdcall `GameRuntime*`, `RET 0x4`. `runtime+0x488` → world, `world+0x510` → table, memset `0x80B0`. |
| `STR_VERSION_381` | `0x00641C60` | `"3.8.1"` |
| `WeaponEntry` | size `0x1D0` | 71 entries, no table header |
| Missile damage | entry `+0x50` | |
| `sprite_id` | entry `+0x60` | Bazooka=48 (`Missile`), Grenade=50, Donkey=100. Custom PNG/GIF → uncompressed `.spr`, ConstructSprite `0x4FAA30` + ProcessSprite `0x4FAB80`. Install after `GfxHandler__LoadSprites` `0x570B50`. GIF `.spr` flags bit 0 wraps blit; FrameFinish advances `MissileEntity+0x3E8` for looping sheets (donkey anim_kind `0x64` does not). File RGB is nearest-matched onto the live map `PaletteContext` (256 slots, usually full) so MapColor exact-matches; frontend `Graphics/*.pal` is not this table. Later `SetLayerVisibility(3,1)` flushes. Slot 800..=1022. |
| `gravity_pct` | entry `+0x70` | |
| `wind_pct` | entry `+0x74` | Bazooka=50. Was previously mislabeled bounce. |
| `bounce_pct` | entry `+0x78` | Bazooka=100. |
| `CollisionManager` | `GameWorld+0x528` | size `0x264`, vtable `0x664194`. `particle_gravity_y` `+0x22C`, `wind_bias` `+0x230`. |
| `apply_projectile_gravity` | `0x004FCB30` | thiscall `(cm, entity)`, `RET 0x4`. Gravity+wind+damp leaf. Bounce is `rope` `0x004FBDC0`. |
| `MissileEntity__HandleMessage` | `0x0050B400` | FrameFinish = msg 2. |
| `WormEntity__HandleMessage` | `0x00510B40` | Jump `0x24`, JumpUp `0x25`, StartTurn `0x34`. |
| HUD CALL | `0x0053448B` | `E8 90 0A 00 00` → `RenderWaitingForPeersTextbox` `0x00534F20`. Presentation only. |

Entity parent-defaults (overlay at `+0x38`): damp `+0x58`, wind factor `+0x5C`, gravity factor `+0x60`. Speed `+0x90/+0x94`. Worm team `+0xFC`.

## Fire path (planned wrap, not installed)

Functions exist in Ghidra at these VAs (names from openWA; still FUN_* in the database). Wrap after confirm calling convention / `RET imm16`. Do not trap.

| Name | VA |
|------|-----|
| `WormEntity__TryFireWeapon` | `0x0051B2B0` |
| `FireWeapon__ProjectileFire` | `0x0051DFB0` |
| `FireWeapon__CreateWeaponProjectile` | `0x0051E0F0` |

Purpose: per-weapon Lua `on_fire` and tagging the new missile with the owning slot/id so gravity/frame hooks can dispatch.

LuaActor uses `WorldEntity::Constructor` + `CollisionManager::register_entity` (layer lists at `cm+0x028`). Click-spawn wraps `FireWeapon__StrikeFire` (`0x0051E2C0`) — Lua `on_click` or a GUI spawn table. Launch-spawn wraps `FireWeapon__CreateWeaponProjectile` (`0x0051E0F0`). `wa.actors.spawn` uses the same native body. `wa.land` reads `Landscape+0x910` (Blas). Crates/mines/missiles walk `BaseEntity` children (Jelly CTask tree, class_type only). `PlaySoundLocal` `0x4FDFE0`.

Ghidra MCP is read-only. Do not push types from the agent; edit TOML and import with a Ghidra script if you use one.
