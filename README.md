# WormCraft

A WormKit module (`wkLua.dll`) that lets people mod **Worms Armageddon 3.8.1** with Lua.

Native code wraps a few stock functions and **dispatches** to the pack that owns a weapon or actor. Mods are text folders (manifest + Lua + PNG/GIF).

PX-level power (custom fire, physics, sprites, later extra panel entries) is the goal. A weapon is a **LuaActor** you spawn and tick; overlaying a stock missile is only for packs that still want stock flight and for later GUI mode. Extra weapon pages later.

Custom PNG/GIF colours are nearest-matched onto the **current map palette** (in-match `PaletteContext`, not frontend `Graphics/*.pal`).

## Two ways to contribute

**Engine (Rust).** Addresses, hooks, and the Lua bindings live in `crates/`. Target is always `i686-pc-windows-msvc`. 

**Mods (Lua).** A pack is a folder with `mod.toml`, a Lua entry script, and optional assets. You do not need a compiler. 

## Install (player)

1. Own Worms Armageddon **3.8.1** with WormKit
2. Build `wkLua.dll` (below) and copy it next to `WA.exe`.
3. Create "Mods" folder in WA Installation Folder (next to WA.exe), additionally copy any mods from the "mods/examples" folder in this repository (Example: "/Worms Armageddon/Mods/wormcraft")
4. Launch the game. 

## Build (engine)

```
rustup target add i686-pc-windows-msvc
cargo test --workspace
cargo build --release -p wk-lua --target i686-pc-windows-msvc
```

The cdylib is `target/i686-pc-windows-msvc/release/wkLua.dll`.

## Layout

- `crates/wk-lua` — DllMain
- `crates/engine` — ASLR rebase, 3.8.1 check, wraps (never trap FireWeapon / InitWeaponTable)
- `crates/lua-host` — Lua 5.4 sandbox, `wa.*` dispatcher
- `crates/weapons` — stock slots, first-loaded overlay ownership
- `crates/actors` — namespaced LuaActor kinds (`mod_id.kind`); spawn steps come from Lua
- `crates/gfx` — namespaced sprite keys (`mod_id:path`); no global sprite ids
- `crates/game` — 32-bit layouts
- `crates/re-catalog` — `re/*.toml` + `hooks/*.toml`


## License

MIT. Do not vendor GPL game-replacement sources into this tree.
