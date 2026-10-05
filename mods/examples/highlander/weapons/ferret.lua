-- Highlander: Ferret (PX weapon_ferrets: ferret_s.txt, rafa``2's CFerret).
local shared = require("lib/shared")
local fires = shared.fires

-- Thrown, a Ferret falls until the land is 3 pixels under it, then follows
-- the terrain's outline and never falls off it: PX's square contour tracing,
-- 3-pixel steps, up to 12 traced a frame and 2 walked. It explodes on a worm
-- (the turn's worm is spared for its first 60 frames), on an oil drum, at the
-- water, or when its trace finds no land 4 steps running. It flicks mines
-- and Bowling Balls up (PX's ball is a mine) and walks on. At most 10 walk:
-- a new one makes the oldest vanish. PX's IsMoving says no, so WA doesn't
-- wait for a Ferret, and Ferrets walk on through later turns.
local P = 0x10000
local STEP = 3 * P          -- frt_maxdist
local MAX_FAILS = 4         -- frt_maxfails
local MAX_OPS = 12          -- frt_maxoper
local GRACE = 60            -- frt_framesdam
local TURN_EVERY = 6        -- frt_pixrotat: steps between turns of the sprite
local MAX_ALIVE = 10        -- frt_maxfrtalw
-- PX draws the 64-pixel frame centred on the Ferret, its feet 16 below the
-- centre: the outline is traced with a circle that size, so they meet the land.
local RADIUS = 15
-- What it runs into is a smaller matter: its body, about 8 pixels about its
-- centre. With the outline's circle it went off on worms and drums 21 and
-- 24 pixels away, a worm beside the one that placed it on its first frame.
local BODY = 8
local SCALE = 0.95          -- frt_sprscale
local DEPTH = 0xE0000       -- in front of the terrain, as the Bowling Ball
local WALK = "sprites/ferret/walk"
local FRAMES = 10
local SOUND = "sounds/ferret/walk.wav"
-- The scheme record's explosion (ExplodeAt): damage 100, blast 50.
local BOOM = { damage = 100, id = 50 }
-- A worm it runs into is thrown first (SpX = TurnSide, SpY = -5), a mine or a
-- ball flicked (SpX = TurnSide, SpY = -7).
local WORM_UP, MINE_UP = -5, -7

local UP, RIGHT, DOWN, LEFT = 1, 2, 4, 8
-- frt_moveto: from where it looks, a turn left or right is a step and a new
-- direction to look in.
local MOVES = {
  [DOWN] = { [LEFT] = { 1, 0, RIGHT }, [RIGHT] = { -1, 0, LEFT } },
  [UP] = { [LEFT] = { -1, 0, LEFT }, [RIGHT] = { 1, 0, RIGHT } },
  [LEFT] = { [LEFT] = { 0, 1, DOWN }, [RIGHT] = { 0, -1, UP } },
  [RIGHT] = { [LEFT] = { 0, -1, UP }, [RIGHT] = { 0, 1, DOWN } },
}

local alive = {} -- the Ferrets walking, oldest first
-- WA places a Ferret at its worm, where its circle overlaps the ground:
-- traced from inside the land, it would turn on the spot for ever. It rises
-- until its circle is clear of the land, at most this far, and walks from
-- the surface.
local MAX_RISE = 64

local function land_at(x, y)
  return (wa.world.collides({ x = x, y = y, radius = RADIUS }))
end

local function rise_clear(x, y)
  for _ = 1, MAX_RISE do
    if not land_at(x, y) then return y end
    y = y - P
  end
  return y
end

local function move(f, dir)
  local m = MOVES[f.looking][dir]
  f.ax, f.ay, f.looking = f.ax + m[1] * STEP, f.ay + m[2] * STEP, m[3]
end

-- frt_square_tracing: from the last outline point, turn towards the land
-- while there is land, away from it while there is none, adding each new
-- land point to the outline to walk.
local function trace(f)
  f.ax, f.ay = f.px, f.py
  f.looking = f.looking_bk
  while true do
    if f.ops <= 0 then
      f.tracing, f.walking, f.fails, f.ops = false, true, 0, MAX_OPS
      return
    end
    if f.fails >= MAX_FAILS then
      f.tracing, f.walking, f.boom, f.fails, f.why = false, false, true, 0, "a dead end"
      return
    end
    if land_at(f.ax, f.ay) then
      f.fails = 0
      if f.ax ~= f.px or f.ay ~= f.py then
        f.looking_bk = f.looking
        f.px, f.py = f.ax, f.ay
        f.outline[#f.outline + 1] = { f.ax, f.ay }
      end
      move(f, f.side < 0 and RIGHT or LEFT)
    else
      f.fails = f.fails + 1
      move(f, f.side < 0 and LEFT or RIGHT)
    end
    f.ops = f.ops - 1
  end
end

-- frt_checkinteract, at a point of its way.
local function interact(f, x, y)
  local _, obj = wa.world.collides({ x = x, y = y, radius = BODY, with = { "worms", "mines", "oil" } })
  if obj then
    if obj.kind == "worm" then
      local current = shared.current_worm()
      if f.grace > 0 and current and current.id == obj.id then return end
      obj.vx, obj.vy = f.side * P, WORM_UP * P
      f.boom, f.walking, f.tracing, f.why = true, false, false, "a worm"
    elseif obj.kind == "oil" then
      f.boom, f.walking, f.tracing, f.why = true, false, false, "an oil drum"
    elseif obj.kind == "mine" then
      obj.vx, obj.vy = f.side * P, MINE_UP * P
    end
  end
  if shared.kick_bowling_balls then
    shared.kick_bowling_balls(x, y, BODY, f.side, MINE_UP)
  end
end

-- frt_calc_rotation: every few steps, the sprite turns to the way walked
-- since its last turn. Looks only.
local function turn(f, x, y)
  f.steps = f.steps + 1
  if f.steps % TURN_EVERY ~= 0 then return end
  local dx, dy = (x - f.rx) / P, (y - f.ry) / P
  f.angle = math.atan(dx, -dy) - math.pi / 2 * f.side
  f.rx, f.ry = x, y
end

-- frt_perform_trace: one step along the outline; at its end, trace on.
local function walk(f)
  if f.boom then return false end
  local p = f.outline[f.next]
  if not p then
    f.walking, f.tracing = false, true
    f.outline, f.next = {}, 1
    f.x, f.y = f.lx, f.ly
    return false
  end
  f.next = f.next + 1
  f.x, f.y = p[1], p[2]
  interact(f, f.x, f.y)
  turn(f, f.x, f.y)
  f.lx, f.ly = f.x, f.y
  return true
end

-- Before it lands, it flies as thrown, falling, and stops where it touches.
local function fly(f)
  f.vy = f.vy + wa.world.gravity()
  local hit = wa.world.trace({ x0 = f.x, y0 = f.y, x1 = f.x + f.vx, y1 = f.y + f.vy, radius = RADIUS })
  if hit and (hit.hit or hit.starts_solid) then
    if hit.hit then f.x, f.y = hit.x, hit.y end
    f.vx, f.vy = 0, 0
  else
    f.x, f.y = f.x + f.vx, f.y + f.vy
  end
end

-- frt_firstcollision: land 3 pixels under it plants it, and its outline
-- starts there.
local function try_plant(f, a)
  local x, y = f.x, f.y + STEP
  if land_at(x, y) then
    f.planted, f.tracing = true, true
    f.px, f.py, f.ax, f.ay, f.lx, f.ly, f.rx, f.ry = x, y, x, y, x, y, x, y
    wa.sound.play(a, SOUND)
  end
  interact(f, f.x, f.y)
end

local function forget(f)
  for i, other in ipairs(alive) do
    if other == f then table.remove(alive, i); return end
  end
end

local function explode(f, a)
  wa.log(string.format("Ferret explodes on %s at (%d, %d), %d frames after it was placed",
    f.why or "?", f.x // P, f.y // P, f.age))
  f.dead = true
  forget(f)
  a:explode(BOOM)
  if a.id ~= 0 then a:despawn() end
end

local function frame(f, a)
  if f.dead then return end
  f.age = f.age + 1
  if f.gone then
    f.dead = true
    a:despawn()
    return
  end
  if not f.planted then
    fly(f)
    try_plant(f, a)
  end
  if f.y >= wa.world.water() - STEP then f.boom, f.why = true, f.why or "the water" end
  if f.boom then
    explode(f, a)
    return
  end
  if f.tracing then trace(f) end
  if f.walking and walk(f) then walk(f) end
  if f.boom then
    explode(f, a)
    return
  end
  f.grace = f.grace - 1
  a.x, a.y, a.vx, a.vy = f.x, f.y, 0, 0
  a:frame((f.steps // 2) % FRAMES)
  a:gfx({ angle = math.deg(f.angle) % 360, depth = DEPTH, scale = SCALE })
end

fires.ferret = function(fire)
  local worm = fire.worm
  -- PX lets at most 10 walk: the oldest goes, without a bang.
  while #alive >= MAX_ALIVE do
    local oldest = table.remove(alive, 1)
    oldest.gone = true
  end
  local f = {
    x = fire.x, y = rise_clear(fire.x, fire.y), vx = fire.vx, vy = fire.vy,
    side = (worm.facing or 1) < 0 and -1 or 1,
    looking = DOWN, looking_bk = DOWN, ops = MAX_OPS, fails = 0,
    outline = {}, next = 1, steps = 0, angle = 0, grace = GRACE, age = 0,
    planted = false, tracing = false, walking = false, boom = false,
  }
  alive[#alive + 1] = f
  local a = wa.actors.spawn({
    sprite = WALK, x = f.x, y = f.y, gravity = false, collide = false, owner = worm,
    on = function(actor) frame(f, actor) end,
  })
  a.facing = f.side
  a:frame(0)
end

shared.ferrets = alive
