![description](https://i.imgur.com/6Yfe1p6.png)

[Public Front + Weapon editor](https://ropahektic.github.io/WormForge/)

**What the engine can do** (dated, filterable): [Capabilities matrix](https://ropahektic.github.io/WormForge/capabilities.html)

Coding agent? Start with [contributing-with-an-agent.md](contributing-with-an-agent.md) and the matrix — do not treat this README as a feature list.

# Contributing

WormForge stays compact. New power is a **Lua pack** or one **verb** — not a dump of `WA.exe` into scripts.

**Pull requests:** [docs/pull-request-code-of-conduct.md](docs/pull-request-code-of-conduct.md) (keep `main` current, resolve conflicts first, one feature when practical, CI via [`.github/workflows/rust.yml`](.github/workflows/rust.yml)).

Lua names are a first cut. Changing a name is a breaking change: bump `api_version` in `mod.toml` and say so in the PR.

## Two lanes

### Pack (no compiler)

Folder in `mods/examples/` or a player’s `Mods/`. `mod.toml` + Lua + optional PNG/GIF/WAV. Copy the closest example. Prefer a pack whenever the [matrix](https://ropahektic.github.io/WormForge/capabilities.html) already covers the behaviour.

### Engine (Rust, `i686-pc-windows-msvc`)

Wrap a stock **3.8.1** function, bind a handle API, document it, show it in one example. Addresses live in `re/`. Only for verbs Lua cannot call yet.

1. Confirm the site in Ghidra. Write `re/*.toml`.
2. **Wrap, then call original.** Never trap `FireWeapon` / `InitWeaponTable`. Never stack a vtable on a stock class.
3. Bind under `wa` / handles in `crates/lua-host`. No raw pointers, no `unknownC`, no `CWorm*` fields in Lua.
4. Lockstep: if it moves or damages, it runs on messages both peers already see. No `math.random`, wall clock, or local-only input as simulation. Details: matrix (Lockstep & Sync) and the agent guide.
5. `cargo test --workspace` and a 3.8.1 launch that logs `wkLua build …`.

## Hard limits

- **71 stock panel slots.** Overlays / `register` occupy a stock `based_on` slot. Do not grow the weapon table in a drive-by PR.
- **One truecolor path.** Do not add OpenGL or a second full renderer. Use the existing D3D9 atlas (see matrix: Truecolor & GPU).
- **No vendoring** wa-engine.js, collisionmanager.rs, wkaim.dll, external `re` dumps, or GPL game-replacement sources (`REsources/` → gitignore).
- Positions / speeds stay WA **16.16** integers. Stock messages use `wa.msg.*`, not magic numbers.
- Methods are short English verbs on handles (`a:move`, `worm:equip`). PRs that only rename for consistency are useful; PRs that expose a new C++ layout “for completeness” are not.

## Layout

- `crates/wk-lua` — DllMain
- `crates/engine` — ASLR rebase, 3.8.1 check, wraps
- `crates/lua-host` — Lua 5.4 sandbox, `wa.*` dispatcher
- `crates/weapons` — stock slots, first-loaded overlay ownership
- `crates/actors` — namespaced LuaActor kinds (`mod_id.kind`)
- `crates/gfx` — namespaced sprite keys (`mod_id:path`)
- `crates/game` — 32-bit layouts
- `crates/re-catalog` — `re/*.toml` + `hooks/*.toml`

## Build (engine)

```
rustup target add i686-pc-windows-msvc
cargo test --workspace
cargo build --release -p wk-lua --target i686-pc-windows-msvc
```

The cdylib is `target/i686-pc-windows-msvc/release/wkLua.dll`. Off Windows, treat CI’s Windows job in `rust.yml` as the DLL gate.

## Contributors

- **[EdLud](https://github.com/EdLud)** — GPU sprite renderer (D3D9 atlas / truecolor path).

## License

MIT.
