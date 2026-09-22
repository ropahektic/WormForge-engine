# WormForge replay: `.WF_WAgame`

> **Status: W0 complete and passed (2026-09-22). W1 is unblocked.**
>
> This is the canonical plan. `docs/replay.md` is its companion: what W0
> measures, how to run the gate, and the results. Sections below are annotated
> where W0 changed them — struck-through items were superseded by something
> the work turned up, and the annotation says what replaced them.
>
> - **Test B** — two runs of `girder.WAgame`, 93 samples, identical across every
>   one; 7 samples caught live actors and all 7 matched.
> - **Test A** — `Mods/` populated vs emptied: the `rng` column is identical
>   frame for frame. Our packs do not perturb WA's RNG.

## Context

WA already records every game losslessly. A `.WAgame` is an **input** recording —
WA is a deterministic lockstep engine, and playback is re-simulation, not a movie.
wkFOW can therefore keep its replay data in a tiny sidecar and leave the `.WAgame`
vanilla-playable, because wkFOW never touches the simulation.

WormForge does. A Lua actor craters terrain and damages worms. A `.WAgame` recorded
with packs loaded, opened in vanilla WA, diverges the moment a WormForge weapon
would have gone off. So the recording has to carry the packs with it, and it is no
longer a vanilla file.

The harder half is that **WormForge's own layer is not deterministic yet**, and this
is already documented, not a surprise:

> Lockstep: if it moves or damages, it runs on messages both peers already see.
> No `math.random`, no clock, no `GetAsyncKeyState` as sim.
> `wa.world.keys()` / `pointer()` are **local until extra lockstep packets exist**.
> — `README.md:45`

`crates/lua-host/src/sandbox.rs:24` says the same from the other side:

> `// Lockstep: no host RNG. WA RNG is not wired yet; forbid math.random.`

So this is two jobs, in order: **close the determinism gap, then package it.** The
packaging is the easy half. Everything below is sequenced so the feasibility
question gets answered before any file format is written.

## Ground truth (researched; this is what makes it cheap)

**WA already sync-checks itself.** The `.WAgame` stream carries event `0x08` =
`frame:u32, checksum:u32`, and WA logs "Checksum Mismatch" on divergence. We do not
invent WA-side sync checking — we *listen* to it and add a WormForge layer beside it.

**WA's RNG is a single shared LCG**, reversed in
`~/worms_work/wkFOW/clones/openwa/crates/openwa-core/src/rng.rs`:

```rust
pub fn wa_lcg(state: u32) -> u32 { state.wrapping_mul(0x19660D).wrapping_add(0x3C6EF35F) }
```

State lives at `GameWorld + 0x45EC`. The game RNG adds the frame counter before the
multiply. This is the canary: divergence shows up here first.

**The load-bearing warning**, from `openwa/docs/re-notes/desync-investigation.md`:

> There is no "visual-only" code in a deterministic game engine. `debris.spr` seems
> purely decorative, but Worms uses a single shared RNG for everything. Skipping one
> sprite load shifts the entire RNG sequence.

That cost openwa seven bugs and a journal. It applies directly to us: the GPU
renderer *removes* sprites from WA's queue, and anything that reaches
`GenerateDebrisParticles` (0x546F70) ticks the RNG.

**Replay plumbing is already catalogued** in openwa (`re/Replay.toml`,
`re/ReplayGlobals.toml`) and used in wkFOW (`src/WaReplayInfo.h`):

| What | Where |
|---|---|
| prefix object | `DDMain - 0x40` — i.e. `GameRuntime+0x4D0` (we have this as `GAME_RUNTIME_DISPLAY`) minus `0x40` |
| playback active | `prefix + 0xDB48` |
| input filename | `prefix + 0xDB60` |
| output filename | `prefix + 0xDF60` |
| map seed / saved seed | `G_MAP_SEED 0x87D430`, `G_SAVED_RANDOM_SEED 0x88ABAC` |

wkFOW's note is worth repeating: using `GameWorld + 0x24` instead of the prefix reads
64 bytes late and loses the filename.

**Project X solved the same problem by archiving, not fingerprinting.** `strings
wkPX.dll` shows `Libs\Replay\`, `PXSchemes\repl.pxs`, `PX Replay Detected :`,
`PX Replay Library :`, `Invalid PX replay version :`, plus units `ReplayWriter`,
`DesyncData`, `NetComparing` and an INI flag `DesyncLogging`. PX copies the scheme
and the `.pxl` libraries the round used into `Libs\Replay\`, versions the replay, and
reloads them on playback. **That is the design decision to copy** — a replay that
carries its packs still plays after you edit or delete the pack.

PX also answers the `keys()` problem at script level: `keySync->AddKey(vk)` /
`KeyPressed(vk)` (1,123 uses across the extracted scripts) is a *synchronised* input
channel, not a raw poll.

## What WormForge already has that this reuses

| Thing | Where | Why it matters |
|---|---|---|
| `modset_fingerprint()` | `crates/weapons/src/lib.rs:149` | Already produces `id@version;…`. The container header field, written for us. |
| Discovery sorted by id | `crates/weapons/src/lib.rs:128` | `brain_id` assignment order is already deterministic. |
| `LIVE: Mutex<Vec<u32>>` | `crates/engine/src/actor.rs:60` | Actor iteration is insertion-ordered, not hash-ordered. |
| `MSG_FRAME_FINISH` on world root | `crates/engine/src/physics.rs:336-344` | The sim tick, distinct from `MSG_RENDER_SCENE`. The replay clock. |
| Sandbox strips | `crates/lua-host/src/sandbox.rs:9-28` | `io os debug package dofile loadfile load require collectgarbage`, `math.random/randomseed` already nil. |
| `serde` + `toml` | `Cargo.toml:13-14` | Present, but **deserialize-only today** — a header needs `Serialize` derives, not a new crate. |
| Marker-gated per-frame instrumentation | `crates/engine/src/queue_census.rs:24-53` | The exact shape W0 needs: marker file, cached `AtomicU32`, zero cost when off. |
| `wormforge_dir()` / `wa_directory()` | `crates/engine/src/log.rs:27,34` | Where files go (both `pub(crate)`). |

## Known determinism gaps (found, not speculative)

Ordered by severity. The first is the one that matters most, and it is not a pack
problem.

1. **WormForge's own engine polls local async input and writes real game state.**
   `world::pump_space_fire` (`crates/engine/src/world.rs:347-395`) runs from
   `worm_hook` on **every worm `MSG_FRAME_FINISH`** (`physics.rs:373`), does
   `key_down(0x20)` — a raw `GetAsyncKeyState` — and on the rising edge calls
   `start_firing(worm)`, on the falling edge `release_shot(worm)` (posting
   `MSG_RELEASE_WEAPON 0x27` straight into `WormEntity__HandleMessage`) plus
   `spend_gift` (decrements team ammo). `world::equip_weapon` (`:306+`) likewise
   writes `WEAPON_DEFINED/ENABLED/AVAILABILITY/PANEL_STATE` and can add ammo, and
   `q_bazooka` drives it from a local `Q` poll.

   This bypasses the path WA normally uses to get that message on the wire. It is a
   **live multiplayer desync today**, not just a replay obstacle, and it is in the
   engine rather than in a pack. These latch transitions are the primary thing a
   recorder must capture.

2. **`wa.world.keys()` / `wa.world.pointer()` are per-*call*, not per-frame.** No
   cache, no latch, no frame boundary — two calls in one frame can disagree.
   `pointer()` additionally scales by window size and DPI and offsets by the camera,
   so it depends on *local presentation*. Used today by `pooz`, `q_bazooka`,
   `worm_toss`. Recording needs a per-frame latch before it can record anything
   meaningful.

3. ~~**No frame counter exists anywhere.**~~ **RESOLVED in W0 — WA has one.**
   `GameWorld+0x5CC`, incremented at the end of each frame in
   `GameFrameDispatcher`. Nothing needed inventing: WA's own counter is the number
   a replay stream is keyed to, and the probe reads it at `root_hook`'s
   `MSG_FRAME_FINISH` (`physics.rs:336`). Bonus from the same catalog:
   `GameWorld+0x7EF0` is `-1` during playback — WA's own `IsReplay()`, confirmed
   live (the probe logged `REPLAY PLAYBACK` on its first run).

4. **No RNG for packs.** `math.random` is nil and nothing replaces it, so packs fake
   it with fixed tables (`SPREAD`, `CASING_JITTER` in `sentry_gun/weapons.lua:33,47`).

5. **Lua 5.4.7 randomises its string-hash seed per process.**
   `lua-src-547.0.0/lua-5.4.7/lstate.c:71` mixes `time(NULL)`, the `lua_State`
   address, a stack address and `&lua_newstate`. So `pairs()` over **string-keyed**
   tables varies run to run — and `sentry_gun/weapons.lua:726` already does
   `for _, s in pairs(state)`. That loop body happens to be order-independent, but
   the pattern is in the flagship example. `lua-src`'s `Build` exposes no `define()`,
   so pinning the seed means a `CFLAGS_*` override or dropping mlua's `vendored`
   feature — both upstream-hostile. Prefer: detect it with the hash, give packs
   `wa.sorted()`.

6. **Actor ids are heap addresses** (`LIVE` holds raw handles; `a.id` returns the
   entity pointer). Stable *order*, but not stable *values* across runs or peers —
   and `state[a.id]` tables are therefore address-ordered under `pairs()` even with a
   fixed hash seed. Synthetic sequential ids are needed for both replay identity and
   determinism.

7. **Sprite paths do not resolve inside non-weapon hooks.** *(found during W0)*
   `resolve_sprite_name` (`lua-host/src/spawn.rs:348`) namespaces a relative path
   like `"sprites/dot.png"` against `wa._mod`, but only the weapon-event
   dispatchers set that (`actor.rs:331,369,401,430`). Hooks registered through
   `dispatch.rs` — `worm_message`, `hurt`, `gravity_*`, `missile_frame` — never do,
   so with several packs installed the path resolves against whichever pack last
   fired a weapon and the spawn fails with "sprite is not installed" while the
   sheet sits in the atlas. Packs can work around it with `wa.sprites.key()` at
   load time (`synctest` does). The engine fix needs hook registration to record
   the owning pack. Not a determinism bug, but it cost a debugging session and it
   will cost every pack author one.

8. **`coroutine` is not stripped, and `__gc` is reachable** via `setmetatable` even
   though `collectgarbage` is nil. GC timing is allocation- and address-dependent, so
   a `__gc` side effect is nondeterministic.

## Approach

Three layers in one file, written at round end, replayed by pointing WA at the
extracted `.WAgame` and feeding our stream back:

```
2026-09-22 20.14.03 [Online] @eule.WF_WAgame
  [hdr]  magic "WFRP", format version, WA version, wkLua build hash,
         modset fingerprint, container flags
  [pack] every loaded pack: mod.toml + Lua + assets (deflate)
  [wa]   WA's own .WAgame bytes, verbatim
  [wf]   WormForge frame stream, keyed to world-root FRAME_FINISH
  [chk]  sync checkpoints
```

Extraction of `[wa]` gives back a byte-identical vanilla `.WAgame`, so nothing is
lost by choosing the container over a sidecar — `wf extract` is a few lines.

The frame stream is **only what WA cannot already reproduce**: local input reads, RNG
draws if we cannot route them through WA's, and periodic hashes. Everything else
comes back for free from re-simulation. Expect single-digit KB against a 1–10 MB
`.WAgame` (wkFOW's real sidecars are 794–3,297 bytes; measured in `User/wkFOW/Games`).

## Stages

### W0 — Determinism harness + state hash. ✅ **DONE — gate passed**

Shipped (branch `replays`, 8 commits, `4ce7382`..`d8e4086`):

- `crates/engine/src/sync.rs` — per-frame hash of `rng` / `worms` / `actors`,
  keyed to **WA's own frame counter** at `GameWorld+0x5CC`. Marker-gated on
  `sync.txt` exactly like `queue_census`.
- `crates/game/src/hash.rs` — FNV-1a, placed here rather than beside its caller
  because `engine` cannot compile on darwin, so tests there would never run.
- `re/GameWorld.toml` + `offsets.rs` — the probe fields, cross-checked against
  OpenWA's catalog (both agree on size `0x98D8` and five independent fields)
  before being trusted.
- `mods/examples/synctest` — the input-free control pack test B needs, with a
  test enforcing its rules against its own source.
- `tools/sync-{run,diff,gate}.sh` — machine-local harness (gitignored).

**Results.** Test B: two runs of `girder.WAgame`, 93 samples, identical across
every one; 7 caught live actors and all 7 matched. Test A: `Mods/` populated vs
emptied, `rng` identical frame for frame. Negative control (`girder` vs
`kamikaze`) diverges at the first sample, so the gate can fail.

**Test A changed shape.** The plan called for comparing WA's own checksums with
the DLL present and absent. That is not observable: WA prints nothing about
checksums on a clean run, writes no `User/Logs` entry for `/play`, and with the
DLL absent there is no probe to produce a stream at all. Packs-on vs packs-off
keeps the probe in both runs and still isolates what we contribute.

**PX recon done**, three questions, all answered — see `docs/replay.md`. The
decisive one: `0x19660D` appears **0 times in `wkPX.dll`** against 168 in PX's
own `wa.exe`, so PX carries no RNG and defers entirely to WA's. That settles
`wa.random()`.

**Two harness traps worth remembering**, both of which produced convincing
false results before being fixed: a capture taken while a previous WA was still
appending to `wkLua.log` blends two runs into one file (fixed by waiting, plus
a monotonic-frame check); and a length-only mismatch is not a divergence, so
the diff tool no longer reports one as such.

**Caveat:** only 7 of 93 samples catch live actors — the control dots live 90
frames while the probe samples every 50. Real signal, but thin. Raise the
lifetime if test B is ever in doubt.

### W1 — Close the gaps  ← **next**

Ordered by payoff. The first two are worth doing whether or not replay ships.

1. ✅ **Per-frame input latch — DONE.** `crates/game/src/latch.rs` holds the
   frame-keyed cache (5 tests); `world.rs` routes `keys()`, `pointer()` and
   `pump_space_fire` through it. Every raw `GetAsyncKeyState` is now confined to
   two `poll_*` functions.

   Keyed on WA's frame counter rather than refreshed from a hook, so it does
   not depend on whether the world root dispatches to worms before or after —
   the first reader in a frame samples, everyone else that frame agrees. Two
   latches, not one: keys and cursor are independent, and a keys-only pack
   should not pay for the cursor's scaling maths.

   Consequence, documented rather than hidden: while the simulation is not
   advancing the sample does not change either. Correct for anything feeding
   the simulation; a pack wanting a live cursor for decoration would need an
   explicitly unlatched call, which does not exist because nothing has asked.

   **Verified behaviour-preserving in-game**: the W0 gate re-run produced hashes
   bit-identical to the pre-latch run, sample for sample. The probe proved a
   refactor safe, which is the instrument paying for itself outside replay.
2. **Retire `pump_space_fire`'s local poll** (gap 1). It posts
   `MSG_RELEASE_WEAPON` and spends ammo off a raw `GetAsyncKeyState`, bypassing
   the path WA uses to put that message on the wire — a live multiplayer desync
   today, in the engine rather than in a pack. The latch is its fix.
3. **`wa.random(n)`** reading `GameWorld+0x45EC` (gap 4). Design confirmed by PX.
   Decide deliberately and document: drawing *advances* WA's shared RNG, so a
   pack that draws shifts every subsequent stock draw. Both peers do the same
   thing so lockstep holds, but the stock game's sequence is no longer what it
   would have been without the pack. Retires the fixed-pattern tables in
   `sentry_gun`.
4. **Set `wa._mod` for non-weapon hooks** (gap 7, found in W0). Needs hook
   registration to record the owning pack. Fixes "sprite is not installed" for
   any pack spawning from `worm_message` / `hurt` / `gravity_*`.
5. **Synthetic sequential actor ids** (gap 6), so `a.id` is stable across runs
   and peers. Keep the address internally.
6. **`wa.sorted(t)`** and document the `pairs()` hazard (gap 5). The probe will
   surface it if it ever bites.
7. **Strip `coroutine`**, or keep it deliberately and say why (gap 8).

Record the latch into the `[wf]` stream as part of W2, not here — W1 is about
making the inputs *exist* in one place, W2 about writing them down.

### W2 — Recorder

- Read WA's output replay path from `prefix + 0xDF60` (prefix = `display - 0x40`).
  If WA supplies none, record nothing and say so — wkFOW's rule, and correct.
- Write `[wf]` + `[chk]` during the round to a temp file; at round end, read WA's
  finished `.WAgame`, assemble the container, atomically replace.
- **Freeze the schema as explicit versioned arrays from day one.** wkFOW's
  `ReplayFormat.h` carries a comment about a real outage caused by serialising the
  live settings registry while the reader compared against a frozen list: *"Every
  sidecar written since the explosion setting landed was unloadable, which is a worse
  failure than a version bump would have been."*
- Two-phase `complete` flag, as wkFOW does: an aborted round leaves an unplayable file
  rather than a subtly wrong one. Cap the stream below the reader's limit.

### W3 — Playback

- Detect `.WF_WAgame`, extract `[wa]` to temp, point `prefix + 0xDB60` at it, set the
  playback flag, load `[pack]` in place of the installed packs.
- Feed recorded local input back at the matching frame.
- Verify `[chk]` as it goes. **Stop loudly at the first mismatch, naming the frame and
  which component diverged** — a replay that quietly stops matching is the expensive
  failure mode.
- Refuse cleanly on: wrong format version, wrong WA version, missing `complete`.

### W4 — Live desync compare (unlocked by W0, not blocking)

The W0 hash already runs in live games. Exchanging it needs a packet, which is the
same missing capability as `keySync`. Defer until W0–W3 land; until then, log the
local hash stream so two players can diff logs after the fact.

## Files

**New (W0 items marked ✅ as built):**
```
crates/engine/src/sync.rs     ✅ state hash, keyed to WA's frame counter
crates/game/src/hash.rs       ✅ FNV-1a (here, not in engine: darwin can't build engine)
mods/examples/synctest/       ✅ the input-free control pack for test B
tools/sync-{run,diff,gate}.sh ✅ harness (gitignored: machine-local paths)
docs/replay.md                ✅ procedure + results
crates/engine/src/replay.rs      recorder/player state machine (W2/W3)
crates/replay/                   container format, host-testable, no WA
  src/{lib.rs,format.rs,pack.rs}
re/GameWorld.toml             ✅ probe fields added (frame counter, RNG, replay flag)
re/Replay.toml                   prefix offsets, ported from openwa + wkFOW
re/Random.toml                   RNG state + advance
docs/replay.md
```

**Touched (keep small — the fork's merge budget is already 595 lines over 13 upstream
files):** `crates/engine/src/lib.rs` (+1 `mod`), `physics.rs` (~+3 at the existing
FRAME_FINISH site), `apply.rs` (~+3 at `on_gfx_ready`, beside `gpu::clear()`),
`crates/lua-host/src/{api.rs,spawn.rs}` (+`wa.random`, +`wa.sorted`),
`crates/game/src/offsets.rs` (+RNG and replay offsets).

`crates/replay` must not depend on `engine` — pure format code, testable on darwin
like `crates/render`.

## Risks

- ~~**The GPU renderer may already be desyncing.**~~ **ANSWERED by W0 test A.**
  With the GPU renderer active and nine packs loaded — including `synctest`
  spawning six actors a turn — the `rng` column is identical to a run with
  `Mods/` emptied, frame for frame. Nothing we draw reaches
  `GenerateDebrisParticles` or otherwise ticks WA's shared RNG. Worth re-running
  after any renderer change; that is one command now.
- **The write side is greenfield.** There is not one `Serialize` impl or
  `to_string` call in the tree — `serde`/`toml` are used deserialize-only. A binary
  stream means hand-rolled encode (preferred, following wkFOW) or a new dependency.
- **Do not copy `log.rs`'s writer.** `write_file` (`log.rs:161-179`) uses `try_lock()`
  and **silently drops the line on contention**. Acceptable for logs, fatal for a
  frame stream — a dropped frame is a corrupt recording that still looks valid.
- `wormforge_dir()` and `wa_directory()` are `pub(crate)` (`log.rs:27,34`) and need
  widening or a sibling accessor.
- **`crates/fog` is a workspace member that nothing links.** It is also the only crate
  using `Instant` and `thread::spawn`. If it is ever wired in, it imports wall-clock
  state into the frame — check before, not after.
- **Embedding packs bloats the container.** `sentry_gun` ships PNG sequence folders.
  Embed Lua + manifest always; for assets, offer content-hash-only with a fallback to
  the installed pack, and embed fully only on request.
- **`pairs()` ordering** (gap 5) may force either a `CFLAGS` hack or an upstream
  conversation. Neither is pleasant; the hash tells us whether it is real first.
- **Recorded local input records one peer's view.** In a networked game where a pack
  reads `keys()`, the peers already disagree, so the replay is faithful to the
  recorder and not to "the game". Say this in `docs/replay.md` rather than pretending.
- **Casing:** real files on disk are `.WAgame`, not `.WAGAME`. Use `.WF_WAgame` to
  match, and match case-insensitively when loading.

## Verification

- `cargo test -p replay --target aarch64-apple-darwin` — container round-trip,
  version refusal, truncation refusal, `complete=0` refusal. Host-runnable.
- **Corpus already exists:** `~/worms_work/wkFOW/clones/openwa/testdata/replays/` has
  18 curated single-feature `.WAgame` files (girder, napalm, kamikaze, cluster,
  freeze_nuke_armageddon, …) plus ~25 TAS missions. `~/worms_work/parse_replays.py`
  is a working `.WAgame` walker with the event table — reuse it to validate that
  `[wa]` extraction is byte-identical and that our frame numbering matches WA's.
- **W0 gate, live:** same `.WAgame` twice, same packs, diff the hash streams. Then the
  same with the GPU renderer forced off, to isolate the renderer.
- **End to end:** record a round using `sentry_gun` + `colourtest` + `wormcraft`,
  replay it, confirm no checkpoint mismatch and that terrain damage lands identically.
- `./tools/build-win32.sh`, install to the GOG prefix per the usual path.
