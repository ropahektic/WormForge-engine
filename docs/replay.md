# Replay — W0: the determinism probe

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
| `rng` | `GameWorld+0x45EC` and the two secondary RNG words | **we perturbed WA itself** — the most serious result |
| `worms` | every live worm's position, velocity and HP | stock simulation drifted |
| `actors` | every live Lua actor's position, velocity, sprite, phase | only our own layer drifted; WA is still in step |

They are separate numbers on purpose: *which* one moved is most of the
diagnosis.

**The RNG is the canary.** WA has exactly one RNG, shared by gameplay and
visual effects, so "visual-only" is not a category that exists here. OpenWA's
desync journal traced a whole-match divergence to a skipped `debris.spr` load
that changed how often `GenerateDebrisParticles` ticked that word — 15,470
writes against 17,106 — and the simulation cascaded apart from there.

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

Off unless a `sync.txt` marker sits beside `wkLua.log`
(`%LOCALAPPDATA%\WormForge\`). Off, it costs one relaxed atomic load per frame
and writes nothing, following `queue_census`.

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
