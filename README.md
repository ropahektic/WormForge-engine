![description](https://imgur.com/1ef65a0f-c945-43ff-8a2c-9c8ec983dc69)

# Contributing

WormForge is meant to stay compact. New power is either a **Lua pack** or one **verb**. It is not a dump of `WA.exe` into scripts.

The Lua names are a first cut. Help curating them is welcome. Changing a name is a breaking change: bump `api_version` in `mod.toml` and say so in the PR.

## Two lanes

### Pack (no compiler)

Folder in `mods/examples/` or a player’s `Mods/`. `mod.toml` + Lua + PNG/GIF/WAV. Copy the closest example. First loaded `id` wins a stock slot

### Engine (Rust, `i686-pc-windows-msvc`)

Wrap a stock function, bind it, document it, show it in one example. Addresses live in `re/`.

If Lua can already do it with actors + `wa.land` + sprites, do not add a native “library.”

## Adding a verb

1. Confirm the site in Ghidra on stock **3.8.1**. Write `re/*.toml`.
2. **Wrap, then call original.** Never trap `FireWeapon` / `InitWeaponTable`. Never stack a vtable on a stock class.
3. Bind a **handle + table** API in `crates/lua-host`. No raw pointers, no `unknownC`, no `CWorm*` fields in Lua.
4. Lockstep: if it moves or damages, it runs on messages both peers already see.
   - HUD must not write physics.
   - No `math.random`, no clock, no `GetAsyncKeyState` as sim.
   - `wa.world.keys()` / `pointer()` are local until extra packets exist.
5. `cargo test --workspace` and a 3.8.1 launch that logs `wkLua build …`.

Draw primitives (line, blit-without-actor) belong in the DLL. Named effects (lightning, fire) belong in Lua that call those primitives.

## Lua names (until someone curates better)

- Everything user-facing is under `wa` or a handle (`a`, `worm`).
- Methods are short English verbs: `a:move`, `a:look`, `worm:equip`. Not `HandleMessage`, not `TryMove`.
- Reads that can fail return `nil` or a table (`wa.land.trace`), not thrown C++.
- Positions and speeds are WA **16.16** integers. Do not silently switch to floats.
- Stock messages are `wa.msg.JUMP`, not magic numbers in packs.
- `wa.weapons.register` does **not** add a 72nd panel slot in v1. It still occupies `based_on`. Do not “fix” that by growing the table in a drive-by PR.

PRs that only rename for consistency are useful. PRs that expose a new C++ layout “for completeness” are not.

## Please

- DO NOT vendor wa.engine, collisionmanager, wkaimbot or the "re" folder (`Resources/` is gitignored on purpose).
- DO NOT add OpenGL or a second renderer because FX is hard. Wrap stock blit first.


## Layout

- `crates/wk-lua` — DllMain
- `crates/engine` — ASLR rebase, 3.8.1 check, wraps 
- `crates/lua-host` — Lua 5.4 sandbox, `wa.*` dispatcher
- `crates/weapons` — stock slots, first-loaded overlay ownership
- `crates/actors` — namespaced LuaActor kinds (`mod_id.kind`); spawn steps come from Lua
- `crates/gfx` — namespaced sprite keys (`mod_id:path`); no global sprite ids
- `crates/game` — 32-bit layouts
- `crates/re-catalog` — `re/*.toml` + `hooks/*.toml`

## Build (engine)

```
rustup target add i686-pc-windows-msvc
cargo test --workspace
cargo build --release -p wk-lua --target i686-pc-windows-msvc
```

The cdylib is `target/i686-pc-windows-msvc/release/wkLua.dll`.



## License

MIT. Do not vendor GPL game-replacement sources into this tree.
