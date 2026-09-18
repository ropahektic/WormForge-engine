# WormForge

A WormKit module (`wkLua.dll`) that lets people mod **Worms Armageddon 3.8.1** with Lua.

Native code wraps a few stock functions and **dispatches** to the pack that owns a weapon or actor. Mods are text folders (manifest + Lua + PNG/GIF), not Project X `.pxl` files and not vtable stacking.

PX-level power (custom fire, physics, sprites, later extra panel entries) is the goal. The shape is [docs/architecture.md](docs/architecture.md): a weapon is a **LuaActor** you spawn and tick; overlaying a stock missile is only for packs that still want stock flight. Extra weapon pages later.

## Two ways to contribute

**Engine (Rust).** Addresses, hooks, and the Lua bindings live in `crates/`. Target is always `i686-pc-windows-msvc`. Reverse-engineering notes go in [`re/`](re/) — see [docs/re-workflow.md](docs/re-workflow.md).

**Mods (Lua).** A pack is a folder with `mod.toml`, a Lua entry script, and optional assets. You do not need a compiler. API: [docs/lua-api.md](docs/lua-api.md). Ownership: [docs/mod-ownership.md](docs/mod-ownership.md). Lockstep: [docs/determinism.md](docs/determinism.md).

## Install (player)

1. Own Worms Armageddon **3.8.1** with WormKit (`dsound.dll` that loads `wk*.dll`).
2. Build `wkLua.dll` (below) and copy it next to `WA.exe`.
3. Copy a pack from `mods/examples/` into `Mods/` next to `WA.exe` (so `WA.exe` / `Mods/circle_bazooka/mod.toml` or `Mods/gif_donkey/mod.toml`). First loaded `id` wins a stock slot — do not enable two overlays of the same weapon at once.
4. Launch the game. Recopy `Releases/wkLua.dll` next to `WA.exe` each time (overwrite). A **WormForge** console opens with the game — that is the live log. Confirm `wkLua build 20260918-ptr`. Custom PNG/GIF colours are nearest-matched onto the **current map palette** (in-match `PaletteContext`, not frontend `Graphics/*.pal`).

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
- `docs/architecture.md` — object model vs Project X

## License

MIT. Do not vendor GPL game-replacement sources into this tree.
