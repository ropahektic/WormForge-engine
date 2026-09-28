# Contributing with a coding agent



Open this repo in the agent, point it at this file and [README.md](README.md), then describe the weapon or behaviour you want. You do not need to paste the rules into chat — the agent should read them here.

Before opening a pull request, follow [docs/pull-request-code-of-conduct.md](docs/pull-request-code-of-conduct.md): keep `main` current, resolve conflicts on your branch, prefer one feature per PR, and treat [`.github/workflows/rust.yml`](.github/workflows/rust.yml) as the build gate (required for non-Windows contributors).

New power is a **Lua pack** or one **verb**. Not a dump of `WA.exe` into scripts.

Ghidra and Ghidra MCP might be required if user wants to attempt out-of-the-box implementations, this needs manual setup, inform user/author if it's needed and guide him through installation. But do not assume it is required, most of the RE is available.

## Packs

A pack is a folder (`mod.toml` + Lua + optional sprites/sounds). Copy it to `Mods/` next to `WA.exe`. No compiler.

```
mods/examples/my_thing/
  mod.toml
  weapons.lua          (or init.lua)
  sprites/…
  sounds/…
```

v1 overlays a stock panel slot. It cannot add a 72nd icon. Several packs may overlay the same slot: they all load; **first loaded `id` keeps the slot**, later claims for that slot are skipped. `gif_donkey` and `gif_donkey_lua` both overlay grenade on purpose — statuses vs a Lua brain — and that skip is the system working. You can also call weapons arbitrarily without them existing at all in panel/scheme, see q_bazooka example in /mods.

The engine (`crates/`, `wkLua.dll`) is only for verbs Lua cannot call yet. Prefer a pack.

Needs: WA **3.8.1** + WormKit, this repo, `wkLua.dll` next to `WA.exe`. Test in a match; logs are `wkLua.log` and `wkluaerror.txt`, both beside `WA.exe`. Then open a PR — only after [docs/pull-request-code-of-conduct.md](docs/pull-request-code-of-conduct.md).

| You want… | Start from |
|---|---|
| Log a line, no weapon | `hello` |
| Same bazooka, new sprite / gravity / wind | `circle_bazooka` |
| Click the map, a body falls and explodes | `gif_donkey_lua` (Lua) or `gif_donkey` (statuses) |
| Cut-in, extra sprites, sounds on fire | `wormcraft` |
| Click a worm, drag, throw | `worm_toss` |
| Selecting the slot *is* a mini-game | `pooz` |
| Hotkey that stock-selects an existing gun | `q_bazooka` |
| Hurt layer (`worm:hurt` + `wa.on.hurt`) | `hurt_lab` |
| Hitscan sentry (`a:frame`, `land.trace`, `worm:hurt`) | `turret` |

## Pack contract

Lua sees `wa` and handles (`a`, `worm`). Not C++ names, not `unknownC`, not pointers.

A weapon is usually a **LuaActor**: `wa.weapons.replace` then `on_click` / `on_fire` spawn a body with a brain. Status tables (`gravity = true`, …) are the GUI path (`gif_donkey`). Overlay a stock missile (`params`, `wa.on.gravity_*`) only when you still want stock flight (`circle_bazooka`).

- Positions / speeds are WA **16.16** integers (`0x10000` = 1 pixel).
- Space is shoot (hold to charge bazooka-type, release to fire). Enter / Backspace are `wa.msg.JUMP` / `JUMP_UP`. `wa.world.keys().jump` is W (POOZ), not Space.
- Sim on messages both peers already see. HUD does not write physics. `keys()` / `pointer()` are synchronized in networked games; every peer must run the same WormForge build.
- No `math.random`, no clock, no filesystem from Lua. Assets stay inside the pack.
- Iterate a table's keys with `wa.sorted(t)` when the order reaches the sim: `for _, k in ipairs(wa.sorted(t)) do ... t[k] ... end`. Plain `pairs()` order over string-keyed tables differs per process — keys that are actor/worm `id`s are fine, they are integers.
- `api_version = 1`. Renaming the Lua API is a breaking change: bump it.

`register` does not add a panel row; it still occupies `based_on`.

`wa.on.hurt` runs on blast / hit / percent / nonlethal / poison (both peers). `percent` is axe-style: `amount` is 50 meaning half current HP. `pre` may write `amount` or return `"eat"`. `post` has `lost` (HP that actually left). `worm:hurt(10)` posts stock hit (0x4B) with no knockback; pending HP still waits until momentum ends. `worm:ammo("mine")` reads team ammo; `worm:ammo("mine", 1)` adds, and leaves infinite (`-1`) alone; `worm:ammo("mine", { set = 1 })` sets it outright, overwriting infinite too (`set = -1` makes it infinite). Ammo is shared per alliance (`worm.alliance`, so allied teams share it). Granting keeps the weapon in its own panel row.

Per-worm loadouts are virtual because WA has no per-worm ammo row. `wa.inventory.create({ id = "classes", slots = {...} })` creates a store scoped to the registering pack. It supports `get`, `set`, `add`, `remove`, `clear`, `list`, deterministic `deal`, `transfer`, and `apply`. `apply(worm, { clear = true, each = 1 })` clears the store's slots on every live alliance row and materializes only that worm's inventory. `transfer(from, to, { weapons = "all", amount = n, mode = "move"|"copy" })` takes explicit worm targets; put conditions in normal Lua callbacks.

`panel_icon = "sprites/icon.png"` on `replace` puts a pack PNG in the stock weapon panel: 32×32 (WA's and every PX icon's full size; the panel shows the centre 24×24) or up to 24×24, quantized to WA's icon colours with an automatic greyed frame. `panel_icon = "banana"` still borrows a stock icon. See `docs/weapon-panel.md`.

`wa.on.turn_start(function(event) ...)` receives `event.worm` / `event.active`. `wa.on.death` fires once per worm with `victim`, current `active`, and optional `attacker`, `attacker_team`, `weapon`, `slot`, and `kind`. `wa.on.hurt` keeps its old fields and also carries those optional attribution fields. Missing attribution is `nil`: delayed stock projectiles expose their team but are never guessed to belong to the currently selected worm/weapon. Compare worms by stable `.id`, not userdata identity.

`a:frame(i)` holds a LuaActor on one frame of its current sheet (zeroes the loop delta). `worm.facing` is read-only (−1 left, +1 right). `cluster = { sprite = "sprites/bits", homing_sprite = "sprites/bits_homing" }` takes pack sheets for the cluster bits and for what homing bits draw while they home (their render timer). `fire_repeat = 5` runs stock FireWeapon that many times per shot, in the same frame (PX’s Real Shotgun: five pellets). `plane = "sprites/plane"` on a strike slot gives the strike its own plane (the strike’s `+0x38`, where the stock jets are 86 and 87). `sprite_overrides = { wsvbd = "sprites/trail", [466] = "sprites/x" }` on `replace` swaps any stock sprite (Gfx.dir name or id 1–799) for a pack sheet for the whole match, as PX’s `SetSpriteOverride` does: every user of that sprite draws it, so only override sprites nothing else in the match needs (PX repurposes the Salvation Army’s `wsv*` frames as trails). `worm_sprites` on `replace` remaps that slot’s worm poses only (`weaponlnk` / `wthrow` and slope variants; on the uzi, minigun, shotgun and handgun also `weaponfire*`, the gun firing, which falls back to the `weaponlnk*` sheet). `ev.damage` on an actor brain is stock blast falloff this tick. Overlaying a mine’s fire (`copy_from = "mine"` plus `on_fire`) keeps placement/retreat and routes the body through the existing projectile wrap. `wep = "x.wep"` on `replace` gives the slot a Project X weapon file’s firing (a PX `.wep` is WA’s weapon table entry behind a 4-byte tag), exclusive with `copy_from`; its name is the default name, explicit params and pack art still win, and a sprite field naming a PX-only sprite keeps the slot’s own (see `hellish_grenade`). `patch = { damage = 70, detonation = 1 }` on `replace` writes named weapon fields after `copy_from` / `wep` laid the base down (legacy `[offset]` keys still work) — the web editor’s readable diff; fire type must stay 0–4, and PX sprite ids keep the slot’s stock art as with `wep`.

`mole_dig_rotation = { moledive = "sprites/dive.png", moledig1 = ..., moledig2 = ..., moledig3 = ... }` replaces the mole strike's digging frames globally for the match, as PX's `SetSpriteOverride` does; any subset of the keys works, and no other sprite is accepted. A fifth key, `mole`, is WA's underwater variant of the dive frame (a diving mole that enters water switches 158 → 663; the dig frames have none). If a pack overrides `moledive` but not `mole`, the dive sheet sinks under PX's sinking tint, `RGB(90, 90, 240)` multiplied in, instead of turning back into WA's mole. The PNG/GIF must live in the pack; the first loaded claim for a frame wins, and every mole in the match uses it.

## Verbs (engine)

If the current `wa.*` surface cannot do it: wrap stock 3.8.1, bind a handle API, show it in one example. Target `i686-pc-windows-msvc`. Wrap, then original — never trap `FireWeapon` / `InitWeaponTable`, never stack VFTs, never grow the 71-slot table. Details: README.

GPU drawing has one hard requirement: WA's **Direct3D 9 (shader)** renderer (`Renderer` = 4 in WA's Options; WA's default where supported). It is what draws GPU sheets (`gpu_prefix`, GPU worm sheets, `sprite_overrides`). On any other renderer, or with another WormKit module hooking the same D3D9 sites (wkFOW), they are not drawn at all: there is no palette fallback. `wkLua.log` names WA's renderer setting at each match and says which of the two it is.

Draw primitives belong in the DLL. Named FX (lightning, fire) belong in Lua. No second renderer. Do not vendor `Resources/` or GPL game-replacement sources. Do not drive-by rename the whole API in the same PR as a pack.

## Lua surface

- `wa.log`, `wa.weapons.replace` / `register`, `wa.actors.spawn` / `all` / `kind`
- Actor: `x y vx vy facing owner kind`, `a:gravity` `a:move` `a:advance` `a:look` `a:frame` `a:explode` `a:dig{radius}` `a:despawn` `a:spawn` `a:sound` `a:in_water` `a:on_feet`
- Weapon steps (do not steal MineEntity): `on_place` / `on_proximity` / `on_explode` as a function or `{ spawn = { sprite = false, explode = { damage = 0, id = 40, now = true }, damage_worms = false } }`. Same `spawn` table as `on_fire` / `on_click`. `sprite = false` is invisible (blit id 1023). `explode.now` blasts at spawn. Nested `on_explode` on a spawned actor fires when that actor blasts. Stock `CreateClusters` is not this path yet.
- Worm: `id team alliance slot weapon kind visible turn hp x y vx vy facing aim aiming drilling`, `worm:carry` `worm:drop` `worm:equip` `worm:ammo` `worm:hurt`. `worm.drilling` reads WA's stock drill state and is read-only. `worm.aim` (read-only) is the aim WA draws, 0 straight down to 1 straight up (the sweep at +0x270; `facing` gives the side). `worm.aiming` (read-only) is true while WA draws the worm with its weapon out: standing with it or charging a throw. `worm.facing` is writable (WA's turn side). `worm.body_visible = false` hides only the body sprite (Project X's RenderWorm): WA still draws the worm's name, health, jet pack fuel and cursor, which `worm.visible = false` hides too. `worm.fuel` is the jet pack's fuel, 16.16 (0 is unlimited). `worm.gravity` / `worm.wind` are how strongly gravity and wind act on the worm, 16.16 (65536 is WA's normal). `worm.state` names WA's state where it is known (`standing` `prefiring` `punching` `drilling` `teleporting` `jetpacking` `parachuting` `sliding`), else nil; `worm:set_state("sliding" | "standing")` enters one through WA's own SetState, from simulation callbacks only. `worm.keys` is the worm's own move requests `{ up, down, left, right }` (WA's, from lockstep input; `wa.world.keys()` is this machine's keyboard), and `worm:clear_keys()` consumes them before WA's handler sees them (simulation callbacks only).
- Inventory: `wa.inventory.create`, then `get` `set` `add` `remove` `clear` `list` `deal` `transfer` `apply`
- `wa.worms.all` / `nearest`, `wa.crates` / `mines` / `missiles` / `oil` `.all()` (pos + `kind`)
- `wa.land.solid` `trace` `dig{ x, y, radius }` `cut{ x, y, shape = "drill" | "torch", facing, angle }`, `wa.world.sky` `water` `size` `view` `pointer` `keys` `cavern` (`wa.world.cavern()` is WA's own cavern flag for the match; `wa.world.retreat()` is the retreat countdown in ms, 0 until retreat starts). `wa.world.gravity()` / `wa.world.wind()` are WA's gravity and wind, 16.16 speed per frame (Project X's `Env->Gravity` / `Env->Wind`), before WA scales them by an object's own factors (`worm.gravity` / `worm.wind`); the wind changes each turn. Terrain cuts require a simulation callback; coordinates are 16.16, radius is 1..200 pixels. Torch requires facing ±1 and angle in 16.16 from −65536 to 65536.
- `wa.perf.enable{ counter = true, profile = true }` (no table: both) turns on this machine's FPS counter (top right: the live frame rate, the 5 s average, the longest frame in ms) and profiler (a `perf:` line in `wkLua.log` every 10 s with `sim` `scene` `draw` `present` `hud`, the `lua` time within them, and `other`). Presentation only: Lua cannot read a timing back. See `mods/examples/perf`.
- `wa.sound.play`, `wa.on.gravity_before` / `gravity_after` / `missile_frame` / `worm_message` / `hurt` / `turn_start` / `death` / `hud`, `wa.hud.line`
- `wa.on.missile_frame(function(m) ... end)` runs for every WA missile each frame (`m.phase` "pre" / "post" around stock). `m.x y vx vy` are writable. `m.weapon` is the stock slot name the missile was fired from, as `worm.weapon` gives it (cluster bits take their parent's; strike bombs and crate bombs have none), and `m.id` identifies the missile. `m.sprite` is the sprite WA draws it with (MissileEntity+0x2EC, a copy of its fire params' sprite made when it was created) and is writable: 0 draws nothing, as Project X hides a missile it draws itself, while the weapon's record, which a thrown weapon's worm posture is picked by, keeps its sprite.
- `wa.weapons.launch{ weapon = "bazooka", owner = worm, x, y, vx = 0, vy = 0, damage, sprite, fuse }` fires a real WA missile from a slot's fire params, as WA's `CreateWeaponProjectile` does and Project X's `new CMissile(...)`: WA flies, draws, trails and explodes it as its own, and it is that slot's missile to every hook (`m.weapon`, `on_explode`). `owner` is the worm it is fired for (its team and worm number); positions and speeds are 16.16. `damage`, `sprite` (stock name or id), `fuse` (ms), `sound` (its flight sound, 0 for none), `explode_into` (`"nothing"`, `"clustlets"` or `"fire"`: what it bursts into, from the record's own cluster params), `countdown` (ms; Project X's `countdownFrom`) and `explode_on_space` (the fire key sets it off in flight) change this missile only, in a copy of the fire params. Simulation callbacks only. It returns the missile's id, the `m.id` of `wa.on.missile_frame`; a missile's `on_explode` event carries its id as `ev.missile`.
- `wa.on.draw(function() ... end)` runs at render time, once per rendered frame (not per sim frame, and differently on each peer): read state and draw, nothing else. `wa.draw.quad{ points = { {x, y} x4 }, color = 0xAARRGGBB | colors = { four }, sprite = "sprites/sheet", frame = 0, blend = "alpha" | "add", depth = 0x10000 }` draws a textured quad (corners in 16.16 world coordinates, clockwise from the one that takes the sheet's top-left; the sheet must be a GPU sheet, e.g. under `gpu_prefix`), what Project X's `AddSpriteQ` draws. `wa.random` refuses to run there; `wa.draw.random()` gives 0 <= r < 1 for looks only.
- wa.on.contact(function(worm, other, c) ... end): WA's own worm-vs-entity contact (WormEntity__OnWormPush, Project X's CWorm::Collide), delivered after WA's frame in the order WA made them. other is a worm, an actor or a world object (.kind); c.vx / c.vy are the worm's speed before WA exchanged velocities, c.flags WA's own argument. Touching the landscape is a contact too, with other.kind == "land" (Project X's Collide gets it as an object too).
- `wa.msg.FRAME_FINISH` `EXPLOSION` `IMPACT` `HIT` `NONLETHAL` `POISON` `JUMP` `JUMP_UP` `FIRE_WEAPON` `RELEASE_WEAPON` `START_TURN`

Sprites: pack PNG strip (`sprite_columns`), GIF, frame folder, or a Gfx.dir stem (`missile`, `grenade`, `uzihit`, …) plus aliases `bazooka`/`holy`/`mine`/`cursor`. Map palette applies. Sounds: pack WAV on the overlay, or a stock index.

Leave the root README alone unless asked. List a new example in `mods/examples/README.md`.
