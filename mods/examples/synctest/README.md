# Sync Test

A measurement control, not a weapon. It makes the determinism probe's `actors`
column non-empty while a recorded replay plays back, so
[W0's test B](../../../docs/replay.md) can ask whether WormForge's own layer
re-simulates identically run to run.

Without it, `actors` sits at the empty-list hash for a whole corpus replay —
those replays fire only stock weapons, so no Lua actor ever spawns and test B
measures nothing.

## Why it looks the way it does

On every `START_TURN` it spawns six dots that arc and expire. Each constraint
is load-bearing:

| rule | why |
|---|---|
| never reads `keys()` / `pointer()` | those are per-**call** polls of live OS state, so a pack that reads them cannot be reproducible by construction |
| no randomness | `math.random` is nil in the sandbox; spread comes from a fixed table indexed by a counter |
| never explodes, damages, or collides | WA has **one** shared RNG for gameplay and effects — an actor that craters ground shifts it, and `rng` stops being a clean control for test A |
| gravity integrated in Lua, not `a:gravity()` | that call enters WA's collision manager; nothing here should run WA code that might touch the RNG |
| counts turns, not actors | actor handles are heap addresses and differ between runs |

`START_TURN` is the trigger because it already rides the lockstep stream. Firing
a weapon is not an option: a recorded replay only presses the buttons it
recorded, and it never chose this pack.

The weapon claim exists solely to carry the sprite — pack sprites are installed
per claim. The slot is an obscure utility no corpus replay fires, so claiming it
changes nothing a recorded game goes on to read.

## Expected result

`actors` varies frame to frame, while `rng` and `worms` stay bit-identical to a
run with `Mods/` emptied.

## Sheet

```sh
mods/examples/synctest/bake_sprites.py
```
