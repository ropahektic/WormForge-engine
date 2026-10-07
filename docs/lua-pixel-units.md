# Lua units (API version 2)

Use `api_version = 2` in every pack/component manifest. Version 1 is refused,
including packs embedded in old replays; those replays no longer load their
scripts. Every peer must use the same engine and packs.

## Numbers

Positions and draw corners are **pixels**, speeds **pixels/frame**, and world
gravity/wind **pixels/frame²** (50 simulation frames per second). Lua integers
are pixels too: `worm.x = 5` means five pixels. Normals, facing, aim, screen
sizes, radii, ribbon widths and hold placement keep their existing units.
Terrain masks and circular query radii still take whole pixel dimensions.

```lua
-- v1: x = click.x + 50 * 0x10000, vy = -0x30000
wa.crates.spawn{ x = click.x + 50, y = click.sky, kind = "weapon" }
worm.vy = -3
worm.gravity = 1
```

The shared Lua-host converter reads native signed 16.16 exactly as `v / 65536`.
Writes use `round(pixels * 65536)`, with ties away from zero, then check the
signed i32 range. NaN, infinity and overflow error with the field's name;
values never silently saturate. Reading a written field returns its rounded
1/65536-pixel value. Keep accumulating float state in your own Lua table
rather than reading the rounded field back every frame.

Plain factors (previously 65536 = normal): `worm.gravity`, `worm.wind`, and
motion events' `damp`, `wind_scale`, `gravity_scale`. Jetpack `worm.fuel` is
also a plain number rather than its native 16.16 encoding (zero is unlimited).
Fire `damage_scale` and `knock_scale` already accepted plain factors and retain
that interface, now using the same converter. Spawn/cluster `power` is a launch
speed in pixels/frame. `fire.power` remains its existing 0–1 charge factor.

IDs, frames, milliseconds, HP, damage, ammunition, percentages, collision
bitmasks, palette colours and sound/sprite indices keep their existing units.
Fire `up_power` retains its native integer parameter (default 100), not its
position/speed encoding. `.wep` files and raw weapon-record `patch` values are
native record data, not positions on the Lua surface; their format is unchanged.

## Simulation maths

Use ordinary float arithmetic and `math.sqrt`. For simulation trigonometry use
`wa.math.sin`, `wa.math.cos`, `wa.math.atan`: WA's table and portable arithmetic.
Do not use CRT `math.sin/cos/tan/asin/acos/atan/exp/log/pow` or the exponentiation
operator `^` in simulation. Write `x * x` for a square. SSE2 arithmetic and the
single explicit boundary rounding rule give peers the same native values.
Draw/HUD callbacks may use CRT maths because their results stay local.

Run `python3 tools/check_sim_math.py` to check packs. The checker ignores strings,
comments and host test mocks, follows local render helpers, and treats unknown
functions as simulation. It is conservative for dynamic calls and exported
helpers; it cannot prove arbitrary Lua data flow.

## Draw bands

Lower depths are nearer. Names are plain numbers, so `wa.depth.OBJECTS - 1`
draws just in front of the object band. Numeric depths are still accepted.

| Name | Value | Position |
|---|---:|---|
| `FRONT` | `0x10000` | In front of world bodies |
| `EFFECTS` | `0xC0000` | In front of worms |
| `WORMS` | `0xD0000` | Worm band |
| `OBJECTS` | `0xE0000` | Behind worms, in front of terrain |
| `TERRAIN` | `0x180000` | Terrain occlusion boundary |
| `BEHIND_TERRAIN` | `0x190000` | Behind terrain |

Colours remain `0xRRGGBB` / `0xAARRGGBB`. Use existing `wa.msg` names and stock
sprite stems where available. Stock sounds have no named engine table yet:
remaining numeric sound IDs are kept (often as pack-local named constants).
Legacy sprite override IDs without known Gfx.dir stems also remain numeric.

## Pack migration details

Whole-pixel floor operations retained for terrain/grid sampling, Reinforcement's
chosen column, turret beam distances, laser seeds, log formatting and sweep
step counts, and diagonal portal exit height. Fractional motion subdivisions and damping use ordinary division.
POOZ movement constants retain their exact native values in pixels. Crate Strike uses the precise click and stock `click.sky`, as approved, instead
of flooring the click and hardcoding -50. Chilli's and Nuke Rocket's velocity lotteries retain their
original integer RNG ranges and draw count, then converts that random velocity
to pixels; changing the lottery to seven integer pixel speeds would change play.
Valyrian's precomputed ring now contains exact pixel values. The glider's hard
landing cutoff retains its one-native-unit margin below eight pixels/frame.
