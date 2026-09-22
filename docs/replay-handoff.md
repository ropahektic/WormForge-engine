# Replay work: handoff for the next agent

> Session bridge written 2026-09-22. Read `contributing-with-an-agent.md`,
> `README.md` and `docs/replay-plan.md` first — this file assumes them. Once
> W2 starts, fold anything still useful into `replay-plan.md` and delete this.

## Where things stand

Branch `replays`, HEAD `5991425`. The **working tree holds uncommitted
changes**: W1 item 4 (gap 7) in `crates/lua-host/src/api.rs` (+125/−2, built,
tested, live-verified) plus the doc/comment updates in `docs/replay-plan.md`
and `mods/examples/synctest/weapons.lua` — and, new since this file was
written, **W1 items 6 and 7**, also uncommitted and described below, together
with the pack-author `pairs()` note in `README.md` /
`contributing-with-an-agent.md` / `sentry_gun/weapons.lua`. The DLL carrying
all three was built with `tools/build-win32.sh` and installed **after** this
file's first draft — see "Verification" below.
**Repo policy: do not commit or push unless the user asks.**

W1 checklist state (details in `replay-plan.md`):

- ✅ 1 input latch · ✅ 3 `wa.random()` · ✅ 4 hook pack context · ✅ 5 stable
  ids · ✅ 6 `wa.sorted(t)` · ✅ 7 `coroutine` stripped. **W1 is complete.**
- ⚠️ 2 shipped as **detection only**: networked local input logs loudly and
  `wa.world.networked()` exists; closing the gap needs lockstep packets
  (keySync-scale — deliberately out of W1).
- Next: **W2 — recorder**.

## What item 4 did, and the decision behind it

`bind_on` remembers each hook's owning pack by reading `wa._mod` **at
registration time** (the same convention as brains and fire/click specs in
`actor.rs`), and a wrapper re-establishes it before each dispatch, passing
arguments and return values through untouched (`MultiValue`) so no dispatcher
changed.

The instructive dead end: the first attempt captured `ctx.mod_id` at bind
time, but `bind_on` lives inside `ensure_wa`, which only runs for the *first*
pack — every later pack's hooks would have been attributed to pack 1. The
two-pack test (mirroring `run_mods` load order) caught it. Bind-time capture
also misattributes runtime registrations; registration-time reading fixes
both. If you ever touch hooks again, keep that shape.

Live verification (the pattern to reuse for any determinism-neutral change):
a temporary probe pack spawning a **bare** `sprites/dot.png` from a
`worm_message` hook, played across the `girder` corpus replay — 0/4 spawns on
the pre-fix DLL (each with "sprite is not installed", `wa._mod` stuck on the
last-installed pack), 4/4 on the fixed one. The `rng`/`worms` sync columns
were byte-identical across both runs, which is why the sync-gate hash **can't
gate** changes like this: use a log probe, not the hash, and remove the pack
afterwards.

## Next steps, in order

1. Commit W1 items 4, 6 and 7 when the user says so (message style: `W1:
   ...`, see `git log`; item 4 and items 6+7 may want separate commits since
   item 4 is already built/live-verified and 6+7 are not yet DLL-verified).
2. W2 recorder per the plan. Hard constraints from the plan: `crates/replay`
   must NOT depend on `engine` (pure format, host-testable like
   `crates/render`); freeze the schema as explicit versioned arrays from day
   one; two-phase `complete` flag; never copy `log.rs`'s try-lock-and-drop
   writer — a dropped frame is a corrupt recording that still looks valid.
5. Leftover from item 3: retire `sentry_gun`'s fixed-pattern `SPREAD` /
   `CASING_JITTER` tables now that `wa.random()` exists.

## What W1 items 6 and 7 did, and the decisions behind them

**Item 6 — `wa.sorted(t)`** (`crates/lua-host/src/api.rs`, beside `wa.hud`;
three host tests). Collects a table's keys, returns them as a sorted array:
`for _, k in ipairs(wa.sorted(t)) do ... t[k] ... end`. Total order is
type-rank then by value (integers numerically, floats by `total_cmp` so NaN
sorts last deterministically, strings byte-wise, booleans
false-before-true); integers always precede floats even for equal numeric
values, because `t[1]` and `t[1.0]` are distinct keys and the order must not
depend on `pairs()` visit order. Unsortable keys (tables, functions,
threads, userdata) are refused with an error — sorting them by address
would reintroduce the nondeterminism under a new name. Two traps avoided:
(1) fail-before-sort, not during — `sort_by` takes an infallible
comparator, and `unwrap_or(Equal)` inside it would silently emit an
address-influenced order, the bug this exists to prevent; the pre-sort
`cmp_key(k, k)` validation loop makes the error loud. (2) Array return, not
a `__pairs` metamethod or in-place sort — no new Lua-side protocol to keep
deterministic, and a `"sorted"` key in the input table stays inert data.
Determinism-neutral like item 4: host-tested, not sync-gate hash (the hash
would pass either way).

**Item 7 — `coroutine` stripped** (`crates/lua-host/src/sandbox.rs` nil-list
+ `coroutine_stays_nil` test in `lib.rs` beside `lua_own_rng_stays_nil`).
Reason recorded in place: yield/resume points are driven by whoever yields,
not by the sim, and a yield across a frame boundary would strand Rust-held
references across a per-peer GC cycle. Checked first: no example pack uses
`coroutine` (nor `setmetatable`/`__gc` — the `__gc` half of gap 8). That
half is deliberately left open: stripping `setmetatable` would break
legitimate metatable use, so it stands as a known-accepted risk the probe
will surface if it ever bites.

## Instruments and traps (all learned the hard way)

- **Host tests:** `cargo test -p lua-host --lib --target
  aarch64-apple-darwin`. `cargo` lives in `/opt/homebrew/opt/rustup/libexec/bin`
  (not on PATH). **Three tests segfault on clean HEAD, pre-existing, not
  yours:** `actor_brain_explodes_on_land`,
  `actors_spawn_from_lua_calls_native`, `worm_lockstep_message_can_write_pos`
  — all in the fake-memory actor harness; skip them. 25/25 others pass.
- **fmt:** `actor.rs`, `lib.rs` and `bind_fog` in `api.rs` carry pre-existing
  fmt drift. Format only what you touch.
- **Windows DLL:** `./tools/build-win32.sh` (clang-cl + lld-link against the
  xwin SDK). The `CC_*/CFLAGS_*/AR_*` env it exports is **required** — plain
  `cargo build` fails with `string.h not found` in mlua-sys. The DLL's
  `wkLua build …` log stamp is a static string, not a build date.
- **Live runs:** `tools/sync-run.sh <label> [replay|name]` plays a corpus
  replay through wine and captures `tmp/sync-<label>.log`;
  `tools/sync-gate.sh` runs tests A+B. `wkLua.log` lives at
  `~/Applications/Worms Armageddon.app/Contents/SharedSupport/prefix/drive_c/users/Wineskin/Local
  Settings/Application Data/WormForge/wkLua.log`. The `[example.x]` prefix on
  `wa.log` lines is the log function's install-time capture (the last-loaded
  pack binds it) — **not** the calling pack; read the message body.
- **Log-probe pattern:** temp pack in the game's `Mods/` (`
  .../drive_c/GOG Games/Worms Armageddon/Mods`), folder/id named to control
  load order (`aaa_…` loads first), claim a slot no corpus replay fires
  (`scales_of_justice` is synctest's, `crate_spy` was the last probe's),
  `wa.log` the result, grep the log, **delete the pack after**. Only
  synctest ships `dot.png`, which makes bare-path probes self-checking.
- **Game packs are copies:** `install-local.py` installs only `wkLua.dll` +
  `sentry_gun`; other packs in the game's `Mods/` are manual copies of
  `mods/examples/` — sync by hand if the live game must see an edit.
- **Determinism:** nothing you draw, spawn or log may reach WA's shared RNG
  (`GenerateDebrisParticles` 0x546F70). synctest's three rules (no input, no
  randomness, no `a:gravity()`) are the template for anything that runs
  during a replay. **Do not change synctest's behaviour** — it is the
  control.
- **Housekeeping:** `install-local.py` SHA-verifies and backs up; remind the
  user to restart WA after a DLL change; never close or restart a running
  game yourself.
