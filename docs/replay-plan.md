# WormForge replay: `.WF_WAgame`

> **Status: W0 complete and passed (2026-09-22). W1 complete: items 1, 3, 4,
> 5, 6 and 7 done; item 2 shipped as detection only because the underlying
> gap needs lockstep packets. W2 recording and W3 playback are complete and
> live-accepted on 2026-09-22. W4 live packet exchange remains optional.**
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
| prefix object | `G_REPLAY_PREFIX 0x87D3F8`, the `push imm32` operand at `ReferencesDDMain` (`0x4E3B40`) + `0x24`; wkFOW names `prefix + 0x40` DDMain |
| playback active | `prefix + 0xDB48` |
| input filename | `prefix + 0xDB60` |
| output filename | `prefix + 0xDF60` |
| map seed / saved seed | `G_MAP_SEED 0x87D430`, `G_SAVED_RANDOM_SEED 0x88ABAC` |

wkFOW's note is worth repeating: the frontend singleton is not
`GameRuntime+0x4D0` (`DisplayGfx`). Using that graphics pointer loses the filename.
`GameWorld+0x24` is useful for comparison, but wkFOW deliberately selects the
frontend singleton because the two filename buffers need not agree.

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

4. ~~**No RNG for packs.**~~ **RESOLVED in W1 — `wa.random()`.** Calls
   `GameWorld__AdvanceRNG` (0x53F320), byte-verified. `math.random` stays nil.
   Follow-up still open: `sentry_gun`'s `SPREAD` and `CASING_JITTER` fixed
   tables exist only because there was no RNG, and can now be retired.

5. **Lua 5.4.7 randomises its string-hash seed per process.**
   `lua-src-547.0.0/lua-5.4.7/lstate.c:71` mixes `time(NULL)`, the `lua_State`
   address, a stack address and `&lua_newstate`. So `pairs()` over **string-keyed**
   tables varies run to run — and `sentry_gun/weapons.lua:726` already does
   `for _, s in pairs(state)`. That loop body happens to be order-independent, but
   the pattern is in the flagship example. `lua-src`'s `Build` exposes no `define()`,
   so pinning the seed means a `CFLAGS_*` override or dropping mlua's `vendored`
   feature — both upstream-hostile. Prefer: detect it with the hash, give packs
   `wa.sorted()`.

6. ~~**Actor ids are heap addresses.**~~ **RESOLVED in W1.** Actors carry a
   spawn-order sequence (`ACTOR_SEQ`); worms use WA's own `team * 16 + index`.
   Tables keyed by `a.id` now iterate reproducibly, because Lua hashes integers
   without a per-process seed — which removes the sharp edge of gap 5 too.

7a. ~~**`wa.log` never worked from a runtime callback.**~~ **FIXED in W1.**
   *(found while verifying `wa.random`)* It pushed into a Vec snapshotted once
   after packs load, so anything logged from a hook, brain or fire callback
   went into a buffer nobody read — `mods/examples/hello` logs from a
   `wa.on.hurt` hook and had zero occurrences in the log. Not a determinism
   bug, but it made every other bug harder to find, this one included.

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
2. ⚠️ **Make gap 1 visible — DONE. It is NOT fixed, and cannot be yet.**

   The earlier claim here, that the latch was its fix, was wrong. Latching makes
   a peer consistent *with itself* inside a frame; it does nothing about two
   peers pressing different keys. `pump_space_fire` still calls `start_firing`,
   `release_shot` and `spend_gift` off this machine's keyboard, and none of it
   goes on the wire. The real answer is a synchronised input channel — Project
   X's `keySync`, 1,123 uses across its scripts — which needs lockstep packets
   WormForge does not have. That is a larger piece of work than all of W1.

   What shipped instead is detection, because a desync that announces itself is
   a bug report and one that does not is a mystery:

   - `world::is_networked()` reads `GameWorld+0x28` (the network game object).
     Verified `0x0` in an offline replay; the networked half is assumed. The
     asymmetry is safe by design — a wrong reading can only produce a missed
     warning, which is today's behaviour, never a false one.
   - `pump_space_fire` and the Lua-facing `keys()` / `pointer()` each log once,
     loudly, when they act during a networked game.
   - **`wa.world.networked()`** is exposed so packs can guard themselves. The
     three packs using local input today — `pooz`, `q_bazooka`, `worm_toss` —
     now have a way to opt out of desyncing instead of no option at all.
3. ✅ **`wa.random()` — DONE.** Calls `GameWorld__AdvanceRNG` (0x53F320),
   byte-verified before the first call the way `render_queue` verifies its
   dispatcher. Calling rather than reimplementing: the six instructions are
   known, but a copy that ever drifted from WA's by one detail would diverge
   every draw afterwards with no visible cause.

   Its disassembly independently confirmed `GAME_WORLD_FRAME_COUNTER` (0x5CC)
   and `GAME_WORLD_GAME_RNG` (0x45EC), which until then were taken from
   OpenWA's catalog on trust — both appear in it as literal displacements, and
   a test asserts the prologue bytes still encode the frame-counter offset.

   Forms mirror `math.random`'s integer ones; no float form, because 0..1
   invites float arithmetic in simulation code. Modulo bias is accepted
   deliberately: rejection sampling consumes a *variable* number of draws, and
   the draw count is exactly what must match between peers. `math.random` stays
   nil, with a test saying so.

   Measured consequence: the `rng` stream diverges from the first draw and
   never re-converges — which is the proof it reaches WA's real RNG — but in a
   girder replay `worms` and `actors` stayed identical across all 93 samples.
   The shift is latent, surfacing only where WA next consumes a draw. See
   `docs/replay.md`.

   Still open: retire the fixed-pattern tables in `sentry_gun`
   (`SPREAD`, `CASING_JITTER`), which now have a real RNG to use.
4. ✅ **Set `wa._mod` for non-weapon hooks — DONE.** `bind_on`
   (`lua-host/src/api.rs`) records the owning pack by reading `wa._mod` at
   registration time — the same way brains and fire/click specs pick up their
   pack — and a wrapper around every registered hook re-establishes it at
   dispatch. Arguments and return values (`"eat"`, hurt's rewritten amount)
   pass through untouched, so no dispatcher changed. Fixes "sprite is not
   installed" for any pack spawning from `worm_message` / `hurt` /
   `gravity_*`.

   Two host tests cover the mechanism: two-pack attribution in `run_mods`
   load order, and return-value transparency. The first attempt captured
   `ctx.mod_id` at bind time instead — `ensure_wa` only runs for the first
   pack, so every later pack's hooks would have been attributed to whoever
   installed first; the two-pack test caught it, which is why the capture is
   registration-time now.

   Verified live with a temporary probe pack spawning a bare `sprites/dot.png`
   from a `worm_message` hook across a girder replay: 0/4 spawns on the
   pre-fix DLL (each failing with "sprite is not installed", `wa._mod` stuck
   on the last-installed pack), 4/4 on the fixed one with `wa._mod` restored
   to the owning pack. The fix is determinism-neutral — `rng` and `worms`
   columns byte-identical across both runs — which is exactly why the
   sync-gate hash cannot gate it; the live probe is the instrument for
   changes of this shape.

   synctest's `wa.sprites.key` pre-resolution is now belt-and-braces rather
   than required; its comment says so.
5. ✅ **Stable ids — DONE, for worms as well as actors.** Actors carry a
   spawn-order sequence in a new struct field (`ACTOR_SEQ`, 0x150), reset each
   match. Worms use `team * 16 + index`, which is not invented: it is the key
   WA itself builds to find a worm in its shared-data table, and `WORM_INDEX`
   already existed at 0x100.

   **This also covers most of item 6.** Lua hashes integers without a
   per-process seed, so a table keyed by `a.id` now iterates reproducibly —
   and `state[a.id]` is the dominant pattern across `sentry_gun`,
   `colourtest`, `pooz` and `circle_bazooka`. The addresses were what made
   `pairs()` order vary; ids fix it without touching Lua's hash seed.

   It also opened and closed a foot-gun: `parse_handle` accepted a bare number
   as a raw entity pointer, survivable only while `.id` WAS that pointer. Now
   refused with an explaining error. Checked first — no example pack ever
   passed `.id` as a handle.

   Verified: the gate passes and the hashes are bit-identical to the pre-change
   baseline, as they must be, since handles were deliberately never hashed.
6. ✅ **`wa.sorted(t)` — DONE.** `api.rs` exposes it beside `wa.hud`: it collects
   a table's keys and returns them sorted as an array, so the call site is
   `for _, k in ipairs(wa.sorted(t)) do ... t[k] ... end`. Order is
   type-ranked (integer, float, string, boolean) then by value — integers
   numerically, floats by `total_cmp` (NaN last, deterministically), strings
   byte-wise, booleans false-before-true. Integers always precede floats even
   for equal numeric values, because `t[1]` and `t[1.0]` are distinct keys
   and the order must not depend on visit order. Keys with no
   process-independent order (tables, functions, threads, userdata) are
   refused with an error: sorting them by address would reintroduce the
   nondeterminism under a new name. Three host tests cover string order,
   mixed-scalar determinism and the refusal.

   Determinism-neutral like item 4, gated the same way: host tests, not the
   sync-gate hash (the order it returns never reaches the sim unless a pack
   acts on it; the hash would pass either way).

   The hazard is now documented where pack authors will meet it: the lockstep
   bullet in `README.md`, the same bullet in `contributing-with-an-agent.md`,
   and a comment on the one live `pairs()` in the examples
   (`sentry_gun/weapons.lua`, the START_TURN reset). That loop body is
   order-independent so it stays as `pairs()` — the comment records why that
   is safe and what would invalidate it. Integer keys are unaffected either
   way: Lua hashes integers without a per-process seed, which is why item 5
   settled the common `state[a.id]` pattern without a `CFLAGS` hack.
7. ✅ **`coroutine` stripped — DONE.** Added to the nil-list in
   `sandbox.rs` with the reason in place: suspend/resume points are driven
   by whoever yields rather than by the sim, and a yield across a frame
   boundary would strand Rust-held references across a per-peer GC cycle
   (gap 8's `__gc` half). No example pack uses coroutines — checked before
   stripping. A `coroutine_stays_nil` host test beside
   `lua_own_rng_stays_nil` keeps a future edit from silently reopening it.

   The `__gc` half of gap 8 (reachable via `setmetatable`, `collectgarbage`
   already nil) is deliberately left open: no pack uses it, and stripping
   `setmetatable` would break legitimate metatable use. Documented as a
   known-accepted risk; the probe will surface it if it ever bites.

Record the latch into the `[wf]` stream as part of W2, not here — W1 is about
making the inputs *exist* in one place, W2 about writing them down.

### W2 — Recorder

- ✅ Read WA's output replay path from `prefix + 0xDF60` (prefix = the singleton
  operand at `ReferencesDDMain + 0x24`).
  If WA supplies none, record nothing and say so — wkFOW's rule, and correct.
- ✅ Write `[wf]` + `[chk]` during the round to temp files. The
  `GameEngine::Shutdown` wrapper finalizes only after WA has destroyed its
  replay writer, then embeds the finished `.WAgame` and publishes with
  `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`.
- ✅ **Freeze the schema as explicit versioned arrays from day one.** wkFOW's
  `ReplayFormat.h` carries a comment about a real outage caused by serialising the
  live settings registry while the reader compared against a frozen list: *"Every
  sidecar written since the explosion setting landed was unloadable, which is a worse
  failure than a version bump would have been."*
- ✅ Two-phase `complete` flag, as wkFOW does: an aborted round leaves an unplayable file
  rather than a subtly wrong one. Cap the stream below the reader's limit.

The pure format crate has host tests for full round-trip, byte-identical `[wa]`
extraction, version refusal, directory truncation, section bounds and torn input.
The live acceptance round produced a complete 9,034,237-byte container with all
1,057 frames (`0..=1056`), 22 checkpoints and 9 packs/367 files. Its embedded
8,313-byte `[wa]` section has SHA-256
`c22e7266d3b346649ef2c4eaa785f5b6594866d9b47463491d07dcb2ae904e61`, byte-for-byte
identical to WA's replay. No temporary files remained after finalization.

### W3 — Playback

- ✅ Detect `.WF_WAgame`, extract `[wa]` to temp, point `prefix + 0xDB60` at it, set the
  playback flag, load `[pack]` in place of the installed packs.
- ✅ Feed recorded local input back at the matching frame.
- ✅ Verify `[chk]` as it goes. **Stop loudly at the first mismatch, naming the frame and
  which component diverged** — a replay that quietly stops matching is the expensive
  failure mode.
- ✅ Refuse cleanly on: wrong format version, wrong WA version, missing `complete`.

The live W3 acceptance used WA's `/play` path on the W2 container. ReplayLoader
accepted the extracted vanilla bytes, all nine archived packs loaded from the
private playback directory, and the run verified all 1,057 frame samples and all
22 checkpoints. The 21 marker-gated sync lines also match the recording. Playback
then removed its extracted `.WAgame` and archived-pack directory. The player rejects
unsafe archive paths and bounds pack count, file count, and gzip expansion before
materialising untrusted replay contents.

A second round recorded by the final W3a build also passed container inspection:
1,173 frames (`0..=1172`), 24 checkpoints, the same 9 packs/367 files, and a
byte-identical 5,951-byte `[wa]` section (SHA-256
`510f4e48e5c45c76a9803d1ea4ea78cf2ce889ddff4aed55885e04e0de4b560e`).

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
crates/engine/src/replay.rs   ✅ recorder/player state machine (W2/W3)
crates/replay/                ✅ container format, host-testable, no WA
  src/{lib.rs,format.rs,pack.rs}
re/GameWorld.toml             ✅ probe fields added (frame counter, RNG, replay flag)
re/Replay.toml                ✅ prefix offsets, ported from openwa + wkFOW
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
