-- Sync Test -- an input-free, kinematic pack for the W0 determinism gate.
--
-- Not a weapon and not a demo. It exists so that the sync probe's `actors`
-- column is non-empty while a *recorded replay* plays back, which is the only
-- way to ask whether OUR layer re-simulates identically. Corpus replays fire
-- only stock weapons, so without a pack like this `actors` stays at the
-- empty-list hash for the whole run and test B measures nothing.
--
-- Three rules, and breaking any one of them makes it measure the wrong thing:
--
--   1. NO INPUT. Never wa.world.keys(), never wa.world.pointer(). Those are
--      per-call polls of live OS state -- two calls inside one frame can
--      disagree -- so a pack that reads them cannot be reproducible by
--      construction, and a divergence would say nothing about the engine.
--
--   2. NO RANDOMNESS. math.random is nil in the sandbox anyway; the spread
--      below is a fixed table indexed by a counter, so it is identical on
--      every machine and every run.
--
--   3. NOTHING THAT REACHES WA's RNG. These actors never explode, never
--      damage terrain or worms, never collide, and do not call a:gravity().
--      WA has ONE shared RNG for gameplay and visual effects, so an actor
--      that craters ground would shift it -- and then the probe's `rng`
--      column would stop being a clean control for test A. Gravity is
--      integrated here, in Lua, with a constant.
--
-- Expected result when it works: `actors` varies frame to frame while `rng`
-- and `worms` stay bit-identical to a run with Mods/ emptied.

local PX = 0x10000

local DOT = "sprites/dot.png"
local SPRITES = { DOT }

-- Resolved to a namespaced key HERE, at load time, and never by relative path
-- at spawn time.
--
-- wa.actors.spawn resolves a bare path like "sprites/dot.png" against
-- `wa._mod`, the current pack context. That used to be wrong inside hooks:
-- wa.on.* hooks ran with whatever pack id the last load or weapon event left
-- behind, so a bare path resolved against the wrong pack and the spawn
-- failed with "sprite is not installed" even though the sheet was loaded
-- (replay-plan gap 7). Fixed in W1: hook registration remembers the owning
-- pack and re-establishes it at dispatch, so a bare path would now resolve
-- correctly here too.
--
-- The pre-resolved key stays regardless: one lookup at load instead of one
-- per spawn, and correct wherever it is used because it closes over this
-- pack's own id.
local DOT_KEY = wa.sprites.key(DOT)

local COUNT = 6 -- actors spawned per turn
local LIFE = 90 -- frames each one lives
local GRAVITY = 0x0600 -- per-frame velocity delta, 16.16
local UP = -3 * PX -- initial upward velocity

-- Stands in for randomness. Fixed, ordered, and small enough to read.
local SPREAD = { -140, -84, -28, 28, 84, 140 }

-- Terrain's own queue depth is 0x180000; below that is in front of it. The
-- dots are cosmetic, so keep them visible rather than buried.
local IN_FRONT_OF_TERRAIN = 0x10000

-- Counts turns, not actors. Actor handles are heap addresses and so differ
-- between runs; a turn index does not.
local turns = 0

local function drift(index)
  local t = 0
  return function(a)
    t = t + 1
    if t >= LIFE then
      a:despawn()
      return
    end
    -- Integrated here rather than through a:gravity(), which calls into WA's
    -- collision manager. Nothing in this pack should enter WA code that could
    -- touch the shared RNG.
    a.vy = a.vy + GRAVITY
    -- terrain=false, worms=false: this only integrates position. With them
    -- true it would report contact, and contact is a step towards the kind of
    -- interaction rule 3 forbids.
    a:move({ terrain = false, worms = false })
    -- Exercise the presentation path too, deterministically: angle is a pure
    -- function of the frame counter and the actor's index.
    a:angle((t * 4 + index * 60) % 360)
  end
end

local function spawn_wave(worm)
  turns = turns + 1
  for i = 1, COUNT do
    local dot = wa.actors.spawn({
      sprite = DOT_KEY,
      x = worm.x + SPREAD[i] * (PX // 100),
      y = worm.y - 20 * PX,
      vx = SPREAD[i] * (PX // 200),
      vy = UP,
      gravity = false, -- integrated in the brain, see rule 3
      collide = false,
      loop = false,
      origin = "center",
      owner = worm,
      kind = "synctest_dot",
      on = drift(i),
    })
    if dot then
      dot:gfx({ depth = IN_FRONT_OF_TERRAIN })
    end
  end
end

-- START_TURN already rides the lockstep stream -- both peers see it, and a
-- replay replays it -- so it is a legitimate trigger. Firing a weapon is not
-- available to us here: a recorded replay only presses the buttons it
-- recorded, and it never chose this pack.
wa.on.worm_message(function(worm, msg)
  if msg == wa.msg.START_TURN then
    spawn_wave(worm)
  end
end)

-- The claim exists only to carry the sprite: pack sprites are installed per
-- weapon claim, and without one the actors would have no sheet. The slot is
-- deliberately an obscure utility that no corpus replay fires, so claiming it
-- changes nothing that a recorded game goes on to read.
wa.weapons.replace({
  weapon = "scales_of_justice",
  name = "Sync Test",
  params = {
    sprite_columns = 1,
    gpu_prefix = "sprites/",
  },
  sprites = SPRITES,
})

wa.on.hud(function()
  wa.hud.line("sync test  " .. turns .. " waves  (determinism control)")
end)
