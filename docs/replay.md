# Replay

W0 and W1 established deterministic simulation and stable pack APIs. W2 recording
and W3 playback of `.WF_WAgame` containers are live-accepted.

During a recorded round, WormForge writes an incomplete container beside WA's
ordinary replay plus capped temporary input and checkpoint streams. After WA's
shutdown closes its replay writer, WormForge embeds the original `.WAgame`
bytes and the exact loaded pack files, marks the container complete, and
publishes it atomically. A crash or write failure leaves `complete = false`,
which playback will refuse instead of trying to simulate a partial match.

The first live round-trip passed on 2026-09-22. A 1,057-frame round produced a
complete container with 22 checkpoints and 9 archived packs containing 367
files. Extracting `[wa]` returned all 8,313 bytes with the same SHA-256 as WA's
original, and finalization removed both temporary streams.

After finishing a local round, inspect the file written beside WA's `.WAgame`:

```sh
cargo run -p replay --bin wf-replay -- inspect "/path/to/round.WF_WAgame"
cargo run -p replay --bin wf-replay -- extract "/path/to/round.WF_WAgame" /tmp/round.WAgame
cmp "/path/to/round.WAgame" /tmp/round.WAgame
```

`inspect` validates the directory, complete flag, pack archive, input stream
and checkpoints before printing their counts and frame range. `extract` writes
the embedded vanilla section unchanged.

Playback hooks WA's mode-1 `ReplayLoader` before it opens the selected file. For
a `.WF_WAgame`, WormForge validates the container, extracts `[wa]`, redirects WA
to that ordinary replay, and loads the archived packs from a private temporary
directory. Installed packs are ignored. Recorded key/pointer samples replace OS
input at their matching simulation frame; each checkpoint compares RNG, worms,
and actors separately and records the first differing component as an error.

The W3 acceptance replay completed all 1,057 frames and all 22 checkpoints with
no mismatch. Run it directly with WA's command line, for example:

```sh
tools/sync-run.sh replay-check "/path/to/round.WF_WAgame"
```

Playback refuses incomplete containers, unsupported format or WA versions,
unsafe archived paths, oversized pack tables, and malformed input/checkpoint
streams before starting the custom simulation.

The final W3a build was then used for another live recording: 1,173 frames and
24 checkpoints, with all nine packs archived. Its extracted 5,951-byte vanilla
replay was byte-identical to WA's original.

## W0: the determinism probe

WA is a lockstep engine: a `.WAgame` records *inputs*, and playback is
re-simulation, not a movie. A WormForge replay is therefore only possible if
WormForge itself is deterministic. Nothing verified that before this.

W0 builds the instrument and nothing else. It answers one question — *do two
runs of the same input produce the same simulation?* — and everything else in
the replay plan is gated on the answer.

It is worth having on its own. WormForge ships actors that crater terrain and
damage worms; until now a desynced multiplayer game would have surfaced as a
rendering bug, or as nothing at all.

## What it measures

Every sim frame, at the end of the world root's `FRAME_FINISH`, three hashes:

| component | covers | a divergence means |
|---|---|---|
| `rng` | gameplay RNG, client effects RNG, and network RNG | same-machine simulation or presentation changed; live peer comparison splits these words |
| `worms` | every live worm's position, velocity and HP | stock simulation drifted |
| `actors` | every live Lua actor's position, velocity, sprite, phase | only our own layer drifted; WA is still in step |

They are separate numbers on purpose: *which* one moved is most of the
diagnosis.

**The gameplay RNG is the canary.** WA's `GameWorld+0x45EC` RNG is shared by
gameplay and some visual effects, so seemingly decorative code can still alter
the simulation. OpenWA's desync journal traced a whole-match divergence to a
skipped `debris.spr` load that changed how often `GenerateDebrisParticles`
ticked that word — 15,470 writes against 17,106 — and the simulation cascaded
apart from there. WA also has `fxrandom` at `+0x72EC` for client presentation
entities and `netrandom` at `+0x72F0`; W0 hashes all three for same-machine
replay verification, while W4 compares them separately across peers.

## Running the gate

```sh
tools/sync-run.sh  a  "/path/to/some.WAgame"   # drives WA with /play
tools/sync-run.sh  b  "/path/to/some.WAgame"   # same replay, again
tools/sync-diff.sh a b
```

`sync-run.sh` arms the probe, truncates `wkLua.log`, launches WA, and saves the
run to `tmp/sync-<label>.log`. `sync-diff.sh` reports the first divergent frame
and which component moved. Omit the replay path to play a round by hand
instead.

A corpus of 18 curated single-feature replays already exists at
`~/worms_work/wkFOW/clones/openwa/testdata/replays/` (girder, napalm, kamikaze,
cluster, freeze_nuke_armageddon, …).

Measured behaviour of `/play`:

- WA **does** exit on its own when the replay ends, and the exit itself takes
  about ten seconds. Playback runs at roughly wall-clock speed, so the run
  lasts about as long as the recorded game: `girder` (93 s of gameplay,
  4,649 frames) takes a little under two minutes end to end.
- WA writes **no** `User/Logs` entry for a `/play` run, and prints nothing
  about checksums to stdout or stderr on a clean run. The corpus's
  `*_expected.log` baselines therefore cannot be reproduced this way — they
  come from OpenWA's own Rust test runner, not from stock WA.

### If WA hangs on an unreadable error

Symptom: WA starts, prints *"Wine cannot find the FreeType font library"*, and
then hangs on a dialog you cannot read. `ERRORLOG.TXT` records

```
WA caused an Integer Divide by Zero (0xc0000094)
in module comctl32.dll
```

That is the Wineskin environment missing, not a WA or WormForge fault: with no
fonts loaded, text metrics come back zero and a Windows common control divides
by one of them. `tools/sync-run.sh` exports what the bundle's launcher exports,
which is the fix. If you launch WA by hand for a probe run, export the same
block or expect this crash.

### The two tests, which answer different questions

**A — do our packs perturb WA?** Same replay twice, once with `Mods/`
populated and once with it emptied, comparing the **`rng`** column. WA has one
shared RNG for gameplay and effects, so anything of ours that reaches it shows
up here. This is the `debris.spr` class of bug and is worth fixing whether or
not replay ever ships.

(The original plan was to compare WA's own checksums with the DLL present and
absent. That is not observable from outside: WA reports nothing on a clean
run, and with the DLL absent there is no probe to produce a stream. Comparing
packs-on against packs-off keeps the probe in both runs and still isolates
what we contribute.)

**B — is our own layer reproducible?** Same replay twice with
`mods/examples/synctest` installed, diffing the **`actors`** column.

That pack exists only for this. A corpus replay fires only stock weapons, so
without it `actors` stays at `811c9dc5` — the FNV basis, i.e. the empty-list
hash — for the whole run and test B measures nothing. `synctest` spawns six
kinematic dots on every `START_TURN`, which is a trigger a recorded replay
actually reaches.

Its constraints are what make it a control rather than a variable: no input
reads, no randomness, and nothing that can touch WA's shared RNG — no
explosion, no damage, no collision, and gravity integrated in Lua rather than
through `a:gravity()`, which would enter WA's collision manager. A test in
`crates/lua-host` enforces each of those against the pack source, and that
test was itself checked against deliberately broken copies to confirm it
fails when it should.

Neither test needs a recorder to exist, which is why they come first.

## W0 result: both tests pass

Measured on this machine, `girder.WAgame`, wkLua loaded, nine packs installed
including `synctest`.

**Test B — our own layer is reproducible.** Two runs, 93 samples each,
**identical across every one**. Seven of those samples caught live actors, and
all seven matched:

```
sync f=49   rng=6af22799 worms=a35e6047 actors=57bd1fde
sync f=599  rng=b8bf2786 worms=bed8440b actors=98e0b688
sync f=649  rng=03f89e55 worms=0a8ac294 actors=4d6f391f
sync f=1449 rng=402ea97a worms=c1d5f9e2 actors=59825d6a
...
```

**Test A — our packs do not perturb WA's RNG.** Same replay with `Mods/`
emptied: the `rng` column is identical to the packs-on run, frame for frame.
`synctest` spawned six actors per turn that arced and expired, and WA's shared
RNG never moved because of it. That is the `debris.spr` class of bug, and we
do not have it.

Read together: WormForge re-simulates identically from the same input, and it
does so without disturbing the simulation it sits inside. **W1 is unblocked.**

### Caveat worth keeping

Only 7 of 93 samples caught live actors. The control pack's dots live 90
frames while the probe samples every 50, so each wave is caught once or twice.
Seven matching samples is real signal, not noise — but it is thin, and a
longer actor lifetime would make test B sharper if it is ever in doubt.

## Earlier result

Measured on this machine, wkLua loaded, all packs installed.

**Positive — `girder.WAgame` played twice: byte-identical**, 93 samples
covering 4,649 frames. Every `rng`, `worms` and `actors` value matched.

```
A a: 93 frames   B b: 93 frames
RESULT: IDENTICAL across all 93 sampled frames.
```

**Negative — `girder` against `kamikaze`: diverges at the first sample**, and
the tool names both components that moved.

```
RESULT: DIVERGED.
  first divergence at f=49
    -> rng differs (6af22799 vs 774b761c)
    -> worms differs (a35e6047 vs b1c402f4)
```

The negative control matters as much as the positive one: a gate that cannot
fail proves nothing. Together they say the probe reads live state, varies with
it, and reports a real difference as a difference.

What this does **not** yet establish: the packs were loaded but inert
throughout, because a corpus replay only fires stock weapons. So this is
"WA plus wkLua with idle packs is reproducible", which is the right first
gate but not the whole question. Tests A and B below close the rest.

## What `wa.random()` costs, measured

Drawing advances WA's shared RNG, so a run with a drawing pack has a different
RNG stream from a run without one. Measured on `girder.WAgame`, a pack drawing
three times per turn:

```
without draws: sync f=49 rng=6af22799 worms=a35e6047 actors=57bd1fde
with draws:    sync f=49 rng=47bb6c6c worms=a35e6047 actors=57bd1fde
```

The `rng` column diverges from the first sample after the first draw and never
re-converges. **That is the proof the call reaches WA's real RNG** rather than
a private copy — a reimplementation would have left WA's own state untouched.

But `worms` is identical in **all 93 samples**, and so is `actors`. The shift
is real and latent: it changes the game only where WA actually consumes a
draw — damage jitter, crate placement, barrel spread, debris — and a
girder-only replay consumes none of them for worm state.

So the honest statement is narrower than "a game with a drawing pack is a
different game". It is: **the RNG sequence is different from the first draw
onward, and that surfaces wherever WA next consumes it.** In a match with
explosions or crates it would surface quickly; in this replay it never did.

This is also a small vindication of splitting the hash by component. One
number would have said "diverged" and left the interesting part — that the
simulation did not — invisible.

## Cost

Synchronized input is active in networked matches. Records are sent when
controls change, while a used pointer moves, and as a one-second heartbeat.
Checkpoint generation and its five additional records are off unless a
`sync.txt` marker sits beside `wkLua.log` (`%LOCALAPPDATA%\WormForge\`) when WA
starts. The compact-codec bridge remains installed so WormForge input and
diagnostic replay records can always be decoded.

During the current W4 investigation, the first eight stock kind-8 checks also
dump WA's serialized state into `wkLua.log` automatically (up to 128 KiB of raw
state each). These bounded diagnostics do not require `sync.txt`; hex data is
written only to the file, not the live console.

## W4: live peer comparison

With `sync.txt` present, WormForge inserts five records into WA's compact network
stream every 50 frames: gameplay RNG, client effects RNG, network RNG, worms,
and actors. Put the marker on both peers and restart WA before the round. Both
peers must use this WormForge DLL; a stock peer does not understand the private
input kind and cannot safely join a marker-enabled sender.

For a live comparison, create an empty `sync.txt` beside the `wkLua.log` on
**every** peer before starting WA. The startup log prints that directory. For
example, Naja's installation reports
`C:\\users\\crossover\\Local Settings\\Application Data\\WormForge\\wkLua.log`,
so its marker is
`C:\\users\\crossover\\Local Settings\\Application Data\\WormForge\\sync.txt`.
An empty file is correct; the marker is existence-only. Each peer's startup log
must say both `sync probe enabled` and `sync transport sending enabled` before
joining the round.

Successful traffic looks like this in `wkLua.log`:

```text
sync tx f=50 game-rng=1a2b3c4d fx-rng=01020304 net-rng=a0b0c0d0 worms=55667788 actors=811c9dc5
sync rx f=50 game-rng=1a2b3c4d local=1a2b3c4d OK
sync rx f=50 fx-rng=01020304 local=01020304 OK
sync rx f=50 net-rng=a0b0c0d0 local=a0b0c0d0 OK
sync rx f=50 worms=55667788 local=55667788 OK
sync rx f=50 actors=811c9dc5 local=811c9dc5 OK
```

A differing gameplay RNG, network RNG, worm, or actor value is logged as
`sync DESYNC`, naming the frame and component. Client `fxrandom` differences
are logged as `sync fx drift`: those effects are not shared simulation state,
but the value remains useful when diagnosing renderer changes. Delivery at a
different simulation frame is always an error. The wire format uses a one-byte
private message kind plus a fixed 12-byte payload. Project X provides a useful
comparison on WA 3.6.31, where it hooks the encoder/decoder at `0x531560` /
`0x531B50` and handles kinds `0x80` and `0x81`. WormForge's 3.8.1 codec
counterparts are `0x5648B0` / `0x564EA0`. Project X's numbers do not validate
the kind or delivery path on 3.8.1.

The first 3.8.1 action-path tests failed at sentry placement. In the
`20260923-replay-w4-decoder-route` round, both peers agree on the kind-8 checksum
at frame 273 (`f281054f`). At sentry placement, frame 411, the sender has
`e160ec74` and Naja expects `923979f7`; Naja never reaches sentry `on_fire`.
The decoder callback also applies input early (Space at frame 219 instead of
272 on one peer), then dispatch applies it again at the proper frame.

Rechecking the executables disproved two earlier explanations: PX's compact
decoder copies payload bytes without applying simulation input, and WA's
kind-8 checksum covers serialized **simulation state**, not input FIFO bytes.
The stock classifier is bypassed for the accepted source at `0x53E22C`;
private records from that source already reach the input queue.

Build `20260923-replay-w4-dispatch-state` removes decoder-side and enqueue-side
input application. Both peers consume input only at `GameRuntime__DispatchInputMsg`,
whose hook now returns the defined handled status (`EAX=1`). The classifier
remains unchanged. Frame-zero sending remains suppressed, but this is not an
authority test: both peers were observed producing input during the ready
period too. The first eight kind-8 checks include serialized-state snapshots
for byte comparison if the placement mismatch persists. A new two-peer round
is still required; the timing corrections do not prove the original sentry
failure fixed. The subsequent round below confirms the failure persisted and
identifies the exact differing state.

### 2026-09-23: empty sprite metadata in the placement checksum

The `20260923-replay-w4-dispatch-state` round at `13.34.52` (Naja's
`wkLua (3).log`, paired with the local log) passes the frame-188 check:
all 41,028 serialized bytes match. At frame 321, all 42,132 bytes match except
four words at `0x8E94`, `0x8E98`, `0x8EB0`, and `0x8EB4`. The local values
are `054fe9d8,05632430`, repeated; Naja has `068e7468,06a1cd50`, repeated.
The second value in each pair is that peer's firing worm address. Naja fails
the stock check before `on_fire`.

These words are inside the firing worm's **SpriteAnimation** block, tagged
`0x20`: current flags/frame count at animator `+0x14/+0x18`, and the same
metadata in the first animation queue entry at `+0x30/+0x34`.
Disassembly establishes the producer:

- `SpriteAnimation__play_1` (`0x4F5BD0`) passes two uninitialized stack locals
  as outputs to `DisplayGfx__GetSpriteInfo` (`0x523500`).
- Sprite ID 0 is rejected without writing outputs. The sentry's empty stock
  equip pose takes this path; its serialized sprite ID and rate are both zero.
- The animator ignores the failure return and copies the untouched locals
  into the current and queued metadata. `0x4F5700` serializes these words,
  so process-local addresses reach WA's simulation-state checksum.

The serialized hashes are `2c311dd8` locally and `3b3c5388` on Naja's side;
the separate world-object contribution is `036ea604` on both peers. As a
forensic check, clearing only the four bad metadata words in the saved dumps
makes the complete byte arrays identical, yielding stream hash `9e2313f5`
and full checksum `a191b9f9`. No checksum or serialized-state masking is used
by the runtime fix.

Build `20260923-replay-w4-empty-sprite-info` supplies rate=0, flags=0, frames=1
for sprite ID 0 before calling the original lookup. The stock failure return
is preserved, and real sprite lookups pass through. A one-frame fallback
keeps the divisor valid even if the animator's sprite word has high flag bits;
its advance routine (`0x4F5AE0`) skips only a completely zero sprite word.
Dispatch-only synchronized input and automatic state dumps remain enabled.

Validation: applying the implemented defaults (flags=0, frames=1 in both
locations) to the captured states also makes every byte identical, with
stream hash `be2513f5` and reconstructed full checksum `c193b9f9` on both
sides. The 30 Rust and 5 snapshot-tool tests pass, the Windows DLL builds,
and its compiled hook preserves ECX/stack arguments while delegating to the
original lookup. The new DLL and sentry pack were installed locally with
backup and hash verification.

The subsequent two-peer acceptance round (Naja's `wkLua.log`, replay timestamps
`13.46.31` / `13.46.32`) used this build on both sides and completed several
turns. Each peer recorded the same 77 stock kind-8
frame/checksum pairs through the end of the round, including sentry placement,
64 sentry fire callbacks, and four Wormcraft callbacks. The first eight state
snapshots matched byte for byte, from frame 176 (46,648 bytes) through frame
719 (50,512 bytes). The round continued beyond frame 11,000; no `sync DESYNC`
or error was logged, and both replay recorders completed. Gameplay RNG,
network RNG, worms, and actors stayed equal. Only the expected client-effects
`fxrandom` drift appeared. This accepts the synchronized-input transport and
the empty-sprite fix for the sentry/Wormcraft path. The broader Q/WASD/Shift,
Space charge/release, and Worm Toss drag/release matrix remains a follow-up.

Compare the automatic state snapshots with
`python3 tools/sync-state-diff.py first-wkLua.log second-wkLua.log`. It selects
the latest launch and recorded round in each log, validates complete dumps,
and reports differing byte offsets. Exit status is 0 for matching common
snapshots, 1 for differing bytes, and 2 for missing/corrupt evidence. Matching
serialized bytes alone does not validate the separate world-object checksum
contribution or prove successful network play.

## Synchronized pack input

The peer whose input WA currently treats as authoritative publishes its eight
key bits, mouse button, and signed world-pixel pointer coordinates. Key and
button changes send immediately. Pointer movement sends while the button is
down or a pack is reading the pointer, throttled to avoid starving stock input;
an idle heartbeat refreshes the complete state once per second. Unchanged state
persists on every peer until a replacement arrives. `wa.world.keys()`,
`wa.world.pointer()`, and native Space charge/release read that state.

Input uses pixel precision on the wire. The OS source is also screen-pixel based,
and the sender consumes its own encoded value so there is no local/remote
precision difference. New records provisionally use custom kind `0x80`. Kind
`0xFE` from the first W4 build is accepted only when decoding old diagnostic
replays.

All network peers must run the same WormForge DLL even when `sync.txt` is absent.
Stock WA does not recognize WormForge's private message kind.

## Notes that cost time to learn

**WA already has a frame counter** at `GameWorld+0x5CC`, incremented at the end
of each frame in `GameFrameDispatcher`. The plan called for introducing one;
reading WA's is strictly better, because it is the number a replay stream is
keyed to. `GameWorld+0x7EF0` is `-1` during playback — WA's own `IsReplay()`,
for free.

**Addresses are never hashed and never used as identity.** Worm and actor
handles are heap pointers, so hashing them would make every run look divergent.
Worms are walked by `(team, index)` and actors in spawn order; that the counts
and the ordering still have to match is what the probe actually tests.

**The offsets were cross-checked before being trusted.** They come from
OpenWA's 3.8.1 catalog, and both catalogs independently agree on struct size
`0x98D8` and on `landscape 0x20`, `weapon_table 0x510`, `team_arena 0x4628`,
`water_level 0x5E0`, `level_width 0x77C0`. A wrong base would have produced a
hash that looked like it was working.

**The diff tool was validated before being pointed at real runs**, against
known-identical and known-divergent input. OpenWA's journal lost days to a diff
tool that reported ~490 false differences on a run that was actually passing.

## Project X reconnaissance

Time-boxed, three questions.

**Does PX carry its own RNG, or use WA's?** WA's, exclusively. The LCG
multiplier `0x19660D` appears **0 times in `wkPX.dll`**, against 168 in PX's
patched `wa.exe` and 163 in stock 3.8.1 `WA.exe`. So every one of the ~227
`GS->GetRandom()` calls in PX's scripts bottoms out in WA's shared RNG. This
confirms the design for `wa.random()`: read `GameWorld+0x45EC`, never generate
locally.

PX applies the same principle to input. `keySync->AddKey(vk)` /
`KeyPressed(vk)` appears 1,123 times across the extracted scripts — scripts
register the keys they care about and read them through a *synchronised*
object rather than polling. That is the answer to WormForge's `wa.world.keys()`
hole, which the README already flags as "local until extra packets exist".

**What does a PX replay archive hold?** `Libs\Replay\` plus
`PXSchemes\repl.pxs`: PX copies the `.pxl` libraries and the scheme the round
actually used, versions them (`Invalid PX replay version :`), and reloads them
from there on playback. **PX archives the scripts rather than fingerprinting
them** — a replay stays playable after you edit or delete the pack. That is the
model for `.WF_WAgame`. (The folder is empty on this install; no PX replay was
ever recorded here.)

**What does its desync machinery compare?** A `DesyncLogging` INI switch
feeding `DesyncData` / `NetComparing` units. It is absent from the shipped
`PX.ini`, so it is a developer diagnostic rather than a player feature —
consistent with what W0 builds, but with no reusable detail short of deep
Delphi RTTI work, which the settled architecture does not need.
