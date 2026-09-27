# Project X: reverse-engineering notes

What we know about how Project X (PX) and the WA code it drives actually
behave, so a port can match them. Each fact gives its evidence. PX's scripts
say what a weapon asks for; these notes say what WA then does with it.

Addresses are WA 3.8.1 (the GOG `WA.exe`). Function names follow OpenWA's
`re/entity/WormEntity.toml`.

## Sources and tools

- **PX script text:** `docs/px-scripts/` in the parent folder (one folder per
  library). See the Highlander port plan for the rest of the extracts.
- **PX's symbol table:** `Editors/PX Editors/pxdata.pxd` in the PX build. It
  lists every class, member, method and enum PX scripts can use, with member
  offsets. `tools/px_symbols.py` lists it. (`hMain.pxh` and `hUser.pxh` next
  to it are empty 8-byte stubs.)
- **PX's compiler:** Delphi code in `PXLEdit.exe` and, at run time, `wkPX.dll`.
- **WA:** disassembled with Homebrew LLVM, e.g.

      /opt/homebrew/opt/llvm/bin/llvm-objdump -d --no-show-raw-insn \
        --x86-asm-syntax=intel --start-address=0x518340 \
        --stop-address=0x5186A0 WA.exe

- **OpenWA** (`~/worms_work/clones/openwa`): names and field layouts. Its
  `crates/openwa-game/src/game/weapon_fire.rs` and `entity/worm.rs` are the
  most useful. Many handlers are named there but not decompiled.
- **wkJellyWorm** (`~/worms_work/clones/wkJellyWorm`): its renderer knows
  only the blit flags (0x1 transparent, 0x10000 flip). Nothing on sprite
  animation flags.
- **A trace build:** when the code does not settle a question, log the
  values in game. The put-away and sprite flags below came from logging
  every worm pose and every queued sprite around the event.

## PX's script language

- **Undeclared names become members of the class they are used in.** Inside
  `override void CWorm::Message(...)`, a name no one declared (the Fire Jump's
  `FJw`, `FJt`) is a new field of that worm, so each worm has its own.
  Evidence: `golf_ball/golfball_script.txt` writes `ballOwner->HaveBall`,
  `ballOwner->haveBalls` and `ballOwner->golf_teleported` on a `CWorm`, and
  none of them is declared anywhere in PX. The compiler rejects only
  undeclared *methods* ("TRZ : ProcessGetter : Undeclarated method").
  Undeclared names in a free function (`res` in `utils/RandomsGens.txt`) are
  presumably globals.
- **Bare member names mean `this`.** In a `CWorm` override, `SpY` and
  `TurnSide` are the worm that received the message, not
  `GetCurrentWorm()`.
- **`float` is WA's 16.16 fixed point**, so `SpY = -9` is -9 pixels a frame.
  In our Lua that is `worm.vy = -9 * 0x10000`.
- **`M_FRAME` is message 2, WA's FrameFinish,** which every worm gets every
  frame. (`EMType` in pxdata.pxd: `M_FRAME` = 2, `M_DRAWQUEUE` = 3.)
- **Script code runs before WA's.** An override does its work and then calls
  `super`, which is WA's handler. Our `wa.on.worm_message` runs at the same
  point: before WA's handler (`worm_hook` in `crates/engine/src/physics.rs`).
- **`CWorm::FireFinal` is the moment the weapon fires,** after any wind-up.
  It is not when fire is pressed. See the fire punch below.

## PX's member offsets against WA's

| PX member | PX offset | WA field | WA offset |
|---|---|---|---|
| `PosX`, `PosY` | +0x8C, +0x90 | position | +0x84, +0x88 |
| `SpX`, `SpY` | +0x98, +0x9C | speed | +0x90, +0x94 |
| `RopeAttached` | +0xC4 | rope attached | +0xBC |
| `Anim` (CWorm) | +0x368 | sprite animation | +0x368 |
| `CurWeapon` (CWorm) | +0x36C | weapon table entry | +0x36C |
| `TurnSide` (CWorm) | +0x1B8 | turn side, -1 / +1 | +0x1A8 |
| `PressedUp` / `Down` / `Left` / `Right` (CWorm) | +0x1E4 .. +0x1F0 | the worm's move requests | +0x1D4 .. +0x1E0 |
| `JetFuel` (CWorm) | +0x230 | jet pack fuel, 16.16 | +0x220 |
| `FirePower`, `FireAngle` (CWorm) | +0x258, +0x25C | power, the real aim | +0x248, +0x24C |

PX's `CGObject` members sit 8 bytes above WA's. Its `CWorm` members from
`TurnSide` to `FireAngle` sit 0x10 above WA's. That is confirmed by WA's
`control_jetpack` (0x513D90), which reads the move requests at +0x1D4,
+0x1DC and +0x1E0, and by `handle_JetPacking` (0x513C50), which drains the
fuel at +0x220. `Anim` and `CurWeapon` match WA exactly. So no single rule
maps PX offsets to WA: check each one against WA's code.

## WA's aim fields

Two worm fields hold the aim, and they differ while the aim moves:

| Field | What | Written by |
|---|---|---|
| +0x24C | the aim, what the shot uses, quantised to 1/128 steps | `control_aim` (0x51D100) |
| +0x270 | the crosshair WA draws, `worm.aim` | `update_crosshair` (0x51CFC0), each sim frame: 0.8 × itself + 0.2 × +0x24C; and `DrawAimCursor` (0x51D680), at render time, interpolated between sim frames |

So +0x270 lags +0x24C by a few frames. Read at different moments, a sim
callback and a render hook see different values. Anything drawn with a
held weapon must use the aim the weapon was placed with: the Sniper
Rifle's laser uses its hold's aim. PX's `FireAngle` (+0x25C in its symbol
table) is +0x24C, the real aim, by the 0x10 shift of its worm members (see
above).

## WA's worm states

`SetState` (0x50E850) dispatches through a jump table at 0x50F484 (base
0x64), and each case calls the state's setup:

| State | Setup | `worm.state` |
|---|---|---|
| 0x65 | `setup_Standing` | standing |
| 0x69 | `setup_PreFiring` | prefiring (PX's `WS_FIRED`: the wind-up) |
| 0x6C | `setup_Punching` | punching |
| 0x6E | `setup_Drilling` | drilling |
| 0x6F | `setup_Teleporting` | teleporting |
| 0x78 | `setup_JetPacking` | jetpacking |
| 0x7E | none | sliding (knockback, contact, walking off a ledge) |

Some older names in our engine do not match this table: 0x7B's setup is
`setup_Batroping`, and 0x71's is `setup_Axeing`.

## WA's worm sprites

**Where a worm's sprite comes from.** `WormEntity__calc_sprite` (0x512E80)
first asks the worm's animation (+0x368; vtable 0x6640CC) for its sprite
and phase: slot 3 (0x4F5400) returns the sprite at animation +0x8 and the
phase at +0xC. The sprite is whatever the worm code passed to `play`,
flags included. `calc_sprite` then adjusts it (e.g. it flips the mirror
bit for a jet-packing worm) and returns it. The worm draw (0x50FCD0) calls
it and may OR in more flags (below).

**A sprite value** is a sheet id in the low 16 bits plus flags:

| Bit | Meaning | Evidence |
|---|---|---|
| 0x40000 | mirror (flip X) | `RQ_DrawSpriteLocal`; `calc_sprite` XORs it at 0x512EC7 |
| 0x200000 | set by the worm draw when bit 8 of the worm's team record (world + 0x4628 + team × 0x51C) is set; meaning unknown | 0x50FD07 |
| 0x400000 | set by the worm draw when worm +0x148 is non-zero; OpenWA names +0x148 "poison damage per turn", unverified | 0x50FD18 |
| 0x800000 | **play backwards** | below |

**0x800000 plays a sheet backwards.** `DisplayGfx__BlitSprite` (0x56B080)
draws a sprite with this bit at phase `0x10000 - phase` (0x56B0AB; stock
sheets 0x1D5..0x1D7 get an extra scale). The worm code plays sheets with
it: e.g. `0x80012D` at 0x510D60, and `calc_sprite` compares against
flagged ids (`0x80012D`, `0x800130`.., `0x8002B3`).

Anything matching sprite ids against its own (our GPU sheets) must mask
these flags off, or the id is not recognised and WA draws `id & 0xffff`,
some unrelated stock sprite. A Sniper Rifle pose drew as a weapon crate
(stock 99) that way. The 0x200000 and 0x400000 bits never appeared in that
trace; the flag was 0x800000.

## WA putting a weapon away

Traced with the Sniper Rifle (the handgun slot) over three shots:

1. **The last shot:** the worm fires (states 0x6A, 0x6B). By the time it
   stands again, WA has selected the next weapon (the bazooka here).
2. **One frame** standing (0x65) with the rifle's aim sheet still drawn.
3. **The put-away:** standing, WA plays the gun's *draw* sheet with
   0x800000, in the worm's slope variant (500, 501 or 502), e.g.
   `0x008401F6`. Its phase runs from 0 to about 0x10000 in steps of 0xCCB:
   20 frames.
4. Then the new weapon's own draw, not reversed.

So a put-away can use a slope variant the draw never used, and it comes
after the selection has already changed.

**PX does not animate its weapon out.** `WormSprite::Message`
(`p_sprite_builder/worms_anims_script.txt`) draws the weapon only while
the worm is `WS_AIMING` or `WS_SETPOWER` with it. On any other frame it sets
`sprite = 0` and its grow-in `counter` back to 0 (grow-in: `counterMax`
20). The rifle vanishes the frame after the last shot. The Highlander port
shrinks it out along the grow-in played backwards instead, during WA's
put-away.

## WA's contacts

WA tells a worm it touched another entity through the worm's vtable slot
8, `WormEntity__OnWormPush` (0x50D9A0, thiscall `(other, flags)`). This is
PX's `CWorm::Collide`. The landscape takes part too (ClassType 28, 0x1C),
and PX's `Collide` gets it as an object like any other, not `NullObj`: the
Helicopter crashes on landing faster than a pixel a frame, and "smooth
landing" is only that threshold (checked in PX). `wa.on.contact` gives it
as kind `"land"`.

## WA's fire punch

A fire punch goes through three worm states.

**1. PreFiring** (`setup_PreFiring` 0x517850, `handle_PreFiring` 0x517BE0).
Pressing fire (worm message 0x26, our `wa.msg.FIRE_WEAPON`) calls
`StartFiring`, which enters this state and plays the wind-up. For a special
weapon (fire type 4) whose subtype is 1, the handler (0x517D08) waits until
the wind-up animation reaches 1.0. It then counts down +0x2A4, which setup
sets to 1, and fires on the next frame. The worm stands still throughout,
with its speed at 0.

**2. Fire.** The fire calls `CWorm::FireFinal` (PX's hook) and
`fire_weapon_special`, which sets state 0x6C, FirePunch.

**3. Punching** (`setup_Punching` 0x518340, `handle_Punching` 0x5183C0).

- **Setup:** sets vx = 0 and vy = -6 * `rec[+0x44]` / 100 pixels a frame
  (-6 for both records below). It then cuts preloaded shape 3 at
  (x, y + vy).
- **Every frame:**
  1. Moves the worm (0x50D450).
  2. While vy >= 0, shows the falling pose (anim 0x11B).
  3. If vx, vy, +0xB0, +0xB4 and +0xB8 are all 0, meaning the worm has
     stopped, it leaves the state for 0x6A and ends the punch.
  4. Otherwise, while vy < 0, it throws fire particles and hits worms
     (0x5193D0). The hit is centred 8 pixels ahead of the worm (from +0x1A8),
     with:
     - damage `rec[+0x38]`;
     - knock angle `rec[+0x3C]` degrees;
     - knock power `rec[+0x40]` / 10.
  5. While vy < 0, it cuts shape 3 at (x, y + vy).

So **the punch lasts until the worm stops, not for a fixed height.** Any
speed a script gives the worm during it is still a punch: upward it cuts
terrain and burns worms. Shape 3 is the `wa.land.cut` shape `punch` on the
`land-cut-punch` branch.

The record fields (the weapon table entry; `.wep` offsets less the 4-byte
tag):

| Field | Stock Fire Punch | PX Fire Jump |
|---|---|---|
| +0x30 fire type / +0x34 subtype | 4 / 1 | 4 / 1 |
| +0x38 damage | 30 | **45** |
| +0x3C knock angle, degrees | 30 | 30 |
| +0x40 knock power, tenths | 75 | **80** |
| +0x44 launch speed, % of 6 px/frame | 100 | 100 |

The stock record is the one 16 of the PX build's 26 readable schemes carry in
slot 16. The HL 0rang3 1.1 scheme's record equals the `weapon_firejump`
library's `.wep` in every field above.

## Fire Jump (`weapon_firejump`)

`Fire_Jump_script`:

1. `FireFinal` sets the shooter's `FJw` and `FJt = 0`.
2. On each of that worm's `M_FRAME`s, before WA's punch handler, it counts
   `FJt` up while there is a current worm, and:
   - `FJt` 1: `SpY = -9`
   - `FJt` 30: `SpX = TurnSide * 4`
   - `FJt` 60: `SpY -= 1`
   - `FJt` 81 to 209: when `SpY > 5`, `SpY = 4`
   - `FJt` 211: stops

Put together with WA's punch:

- **Frame 1** is the frame after the fire, and the punch has already set
  vy = -6. So -9 replaces the punch's own launch.
- **The worm stays in the punch state** all the way up and across. WA cuts
  the way up and hits worms for 45, as for any punch. The script cuts
  nothing itself.
- **The cap is not a smooth glide.** A fall that passes 5 pixels a frame is
  set back to 4, until frame 209.

**Port consequences** (`mods/examples/highlander/rules.lua`):

- **Start the count at the punch, not at the fire key.** Counting from
  `FIRE_WEAPON` puts frame 1 in the wind-up, where setup_Punching then
  overwrites the -9. Every later step then comes early, possibly after the
  punch has ended. The port starts counting on the first frame after the
  fire key that the worm is rising (vy < 0), since it stood still until the
  punch.
- **Add no terrain cut.** WA's punch cuts as long as the worm rises.
- **One small difference remains.** PX pauses the count while there is no
  current worm (`GetCurrentWorm() == NullObj`, between turns). The port does
  not.
