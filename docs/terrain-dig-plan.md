# Terrain digging from Lua

> **Status (2026-09-24): static RE and S1–S3 implemented on `digging`;
> in-game S0 and sync/replay gates still pending.** Do not merge the native
> call path until those runtime gates pass on WA 3.8.1.

## Context

Lua can't dig. What exists today:

- **Cloning a stock digger.** `copy_from = "pneumatic_drill" | "blow_torch" |
  "mole_bomb"` copies a stock weapon's fire behaviour onto a slot. You can
  tune its parameters and sprite, but not how it digs.
- **Craters.** `a:explode{ damage, id, at }` calls WA's `CreateExplosion`,
  which always comes with damage, knockback, sound and effects. That makes a
  bomb, not a drill.
- **`wa.land` is read-only:** `solid(x, y)` and `trace(...)`.
- **The engine's `set_collision_px`** (used by `mask_collide`) only touches
  the walk grid. It never touches the drawn terrain and is undone every
  frame, so it can't make a hole.

This blocks PX-style scripted diggers. The Highlander pools alone name Super
Drill, Super Gravedigger and Remote Roller.

**Goal:** a small, well-shaped verb family that cuts terrain the way WA itself
does. That means the drawn terrain, the collision grid, the exposed soil and
WA's per-frame landscape checksum all stay consistent, and the result is
deterministic for lockstep and replays.

## What WA does (researched)

The drill and blow torch don't explode. Each frame they call a method on the
`Landscape` object (`GameWorld+0x20`) through its vtable (`0x66B208`):

| Slot | VA | Name (OpenWA) | What we know |
|---|---|---|---|
| 2 | 0x57C820 | `ApplyExplosion` | `(size, x, y)`. `size` is clamped to 0–100, or 0–200 when `game_info+0xD778 >= 6`. Cuts a crater with no damage: damage is `CreateExplosion`'s job. |
| 3 | 0x57CCF0 | `mask` | `(shape, x_px, y_px)`. Takes a **preloaded cut shape** from `Landscape+0x88+shape*4` (the struct has width, height and hotspot at +0x8..+0x14), marks dirty rects (`ClipAndMerge` 0x57D2B0), then cuts (0x57C300). The drill uses shape **2**; the torch uses **6–11**, picked from facing and angle. |
| 4 | 0x57CD90 | `test` | Collision test with a shape, likely "would this fit". |
| 5 | 0x57CE30 | `plot` | Adds land with a shape (girders). |
| 18 | 0x57D540 | `GetFrameChecksum` | The landscape is part of WA's own sync checksum. |

Both `ApplyExplosion` and `mask` return early when `[landscape+0xB3C] != 0`.
OpenWA names the setter `Landscape__SetControlFlag` and associates it with
indestructible terrain. Its live values still need observing in S0.

The stock drill attempts a cut on 5 of 16 RNG outcomes per frame, then draws
more shared RNG for small X/Y jitter before calling `mask(2, ...)`. The cut
itself hasn't been seen to use the RNG. PX's Super Drill adds a separate
un-jittered `mask(2, worm.x, worm.y)` on every worm frame while drilling.

WA 3.8.1 enters drilling through SetState case `0x6E` (`0x50F2D5`), which
calls `WormEntity__setup_Drilling` (`0x5186A0`). Setup derives the drill timer
at worm `+0x27C` from the active weapon entry at worm `+0x36C`, entry `+0x44`.
`WormEntity__handle_Drilling` (`0x518770`) subtracts 20 from that timer per
update (`0x518795`) and leaves drilling through SetState `0x65` when it reaches
zero (`0x518A26..0x518A38`). Other state changes can also end the drill. PX's
packaged Super Drill script has no separate distance or time counter: it
clears its extra-cut flag when `ObjState != WS_DRILLING`. Our example likewise
stops its watcher when `worm.drilling` becomes false. It inherits the stock
Pneumatic Drill duration, so extra cuts do not themselves extend travel.

(Found by disassembling `WormEntity__handle_Drilling` 0x518770 and
`handle_BlowTorching` 0x51A8F0, with OpenWA's `re/Landscape.toml` for names.)

## Principles

1. **Call WA's own methods through the object's vtable,** after checking that
   the vtable pointer is `0x66B208`, the same kind of guard used elsewhere.
   Never write terrain bitmaps ourselves: that would skip soil, redraw and the
   checksum.
2. **Simulation only.** The verbs are only valid inside simulation callbacks
   (`worm_message`, actor brains, weapon events). Calling one from `wa.on.hud`
   is an error, not a silent desync.
3. **No hidden randomness.** A verb never draws from the RNG. If a pack wants
   jitter, it calls `wa.random()` itself, so the draws are visible in its code.
4. **Pixels at the API edge.** Positions are 16.16 like everything else in
   `wa.*`; sizes are in pixels.

## Proposed API

```lua
-- Circular cut, no damage, no knockback. WA's ApplyExplosion.
wa.land.dig{ x = a.x, y = a.y, radius = 12 }

-- WA's own drill and torch shapes. Named, never raw indices.
wa.land.cut{ x = a.x, y = a.y, shape = "drill" }         -- index 2
wa.land.cut{ x = a.x, y = a.y, shape = "torch", facing = a.facing, angle = a.angle }
                                                         -- 6..11, as the stock torch does

-- Convenience at an actor's position.
a:dig{ radius = 12 }
```

**Left out on purpose, candidates for later:**

- **`wa.land.fill` / `plot`** (adding land). It's the mirror image and WA has
  it (slot 5), but it's a different design question: how packs should create
  terrain, and whether that goes in the same verb family.
- **Custom PNG cut shapes.** This would mean building WA-format mask objects,
  much like custom sprites, and deciding where they live. Worth doing only
  once circles and the stock shapes have proved too limiting.
- **`wa.land.test`.** Useful for "would this fit", but not needed to dig.

## Stages

### S0: spike (gate)

With a throwaway build, call slots 2 and 3 from a debug pack's actor brain and
confirm:

1. The argument order and units for `ApplyExplosion(size, x, y)`, and how
   `size` maps to the visible radius.
2. What the shape table holds: dump `Landscape+0x88` entries (count, width,
   height, hotspot) behind a marker file, and map indices to shapes.
3. No RNG use: the `rng` column of the sync probe is unchanged by a cut.
   Debris is the risk here: `GenerateDebrisParticles` ticks the RNG. If a cut
   does spawn debris, that's still lockstep-safe, but it must be documented.
4. Cavern borders stay uncut.
5. The `+0xB3C` indestructible-terrain control: observe when cuts are skipped.
6. The same cut sequence played twice through `tools/sync-run.sh` gives
   identical checksums.

### S1: engine natives

- `crates/engine/src/world.rs`: `land_dig(x, y, radius)` and
  `land_cut(shape, x, y)`, going through the landscape vtable with the
  pointer guard.
- Offsets and RE entries: `LANDSCAPE_VTABLE`, the slot indices, the shape
  table offset. Add `re/Landscape.toml` entries and tests in
  `game::offsets`.
- A `sim_context` flag set around every simulation dispatch in `lua-host`, so
  the verbs can refuse outside the simulation.

### S2: Lua API

- `crates/lua-host/src/spawn.rs` `bind_land`: `dig` and `cut`, validated.
  `radius` must be in range. `shape` must be a known name, and a typo is an
  error that lists the valid names.
- `a:dig` in `actor.rs`.
- Document the verbs in `contributing-with-an-agent.md`.

### S3: an example that uses it

`mods/examples/super_drill`: keep WA's stock Pneumatic Drill fire/state and
attach an invisible watcher that calls `wa.land.cut{ shape = "drill" }` at the
worm's live position each frame while `worm.drilling` is true. This follows
PX's packaged `weapon_super_drill.pxl` script. It also doubles as a sync test
(record, play back, compare).

## Critical files

- `crates/engine/src/world.rs`, `crates/engine/src/actor.rs` (natives)
- `crates/lua-host/src/spawn.rs`, `crates/lua-host/src/actor.rs`,
  `crates/lua-host/src/dispatch.rs` (API, simulation-context flag)
- `crates/game/src/offsets.rs`, `re/Landscape.toml`
- `contributing-with-an-agent.md`, `mods/examples/super_drill/`

## Risks

- **Per-frame cost.** A digger cuts every frame, and each cut dirties and
  redraws rows. The stock drill does the same, so it's affordable at stock
  rates. A pack cutting dozens of shapes a frame might not be. Measure it in
  S0.
- **Redraw paths.** The GPU renderer and fog of war read terrain. WA's own
  redraw path covers the stock renderer. The GPU renderer's occlusion samples
  the collision grid, which the cut updates. Check both in S0.
- **The `+0xB3C` control** makes cuts no-ops on indestructible terrain. Confirm
  its live behavior in the S0 round.

## Verification

- `cargo test -p game`: offsets and shape-name mapping.
- In game, the `super_drill` example:
  - the tunnel is drawn and walkable
  - soil shows in the tunnel
  - borders on caverns survive
- The sync gate (`tools/sync-run.sh`, `sync-gate.sh`): two runs of the same
  recorded match give identical checksums, and WA's own checksum logs no
  mismatch.
- A two-peer network match with the drill: no desync.

### Implementation checkpoint (2026-09-24)

The `digging` worktree is based on local `main` (`12e5938`) and leaves the
other in-flight PR branches untouched. Static WA 3.8.1 disassembly confirmed
vtable slots 2/3 and the six torch masks; it does not replace the S0 live gate.
The example pack claims only the Pneumatic Drill slot.

Host `game` tests (27), focused `lua-host` terrain tests, the example's pack
load, `luac -p`, and a mocked first fire/tick pass. The first Windows build
attempt lacked the C cross headers. A subsequent release build succeeded with
`wkFOW/toolchain/msvc` headers and libraries plus LLVM `clang-cl`/`lld-link`.
The resulting DLL and `super_drill` pack were installed locally with backup
and SHA-256 verification. WA must be restarted to load the new DLL. Run S0
and the replay/network checks before treating the native call path as accepted.

The first local game test confirmed that the native mask cuts terrain. The
original example actor moved horizontally and cut a wider-looking tunnel. A
downward-only actor revision still did not reproduce PX's method. Targeted
inspection of PX's packaged `Libs/weapon_super_drill.pxl` shows that PX leaves
the worm in stock `WS_DRILLING`, calls `ApplyMask(2, PosX, PosY)` additionally
on each worm frame, and clears its flag after the state ends. WA 3.8.1's
normal `handle_Drilling` randomly calls landscape mask slot 3 with shape 2
near the worm at `0x518770..0x518882`. The current example preserves that
stock route, follows `worm.drilling`, and adds the PX cut. It was rebuilt and
installed with SHA-256 verification. It still needs a follow-up game check
for appearance and replay/network sync.
