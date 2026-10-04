-- Highlander: Bowling Ball Strike (PX weapon_bowlingball: bbstrike.txt and
-- bowlingball.txt's CBowlingBall).
local shared = require("lib/shared")
local fires = shared.fires

-- The strike: PX's plane drops three balls 48 pixels apart, moving 5 a frame
-- the way it flies. Here they drop together from the sky over the click,
-- the way the worm faces; PX's plane is not drawn.
local STRIKE_BALLS = 3
local STRIKE_SPACING = 48
local STRIKE_SPEED = 5

-- PX's CBowlingBall numbers.
local RADIUS = 16             -- MakeCircleMask(16): the ball's art is 32 across
local BOUNCE, FRICTION = 0.03, 0.02
local INERTIA_X, INERTIA_Y = 0.8, 0.4
local LOW_SPEED = 4.0         -- below this for LOW_FRAMES frames: "low speed"
local LOW_FRAMES = 100
local LOW_BOUNCE, LOW_FRICTION = 0.03, 0.03
local LOW_INERTIA_X, LOW_INERTIA_Y = 0.0, 0.0
local SLEEP_DISTANCE = 2      -- moved less than this over SLEEP_PERIOD frames: asleep
local SLEEP_PERIOD = 80
local FAST = 4.0              -- MinDamageSpeed: faster, it rolls through worms
local SPIN_X, SPIN_Y, MAX_SPIN = 0.06, 0.02, 0.6
local SHEET = "sprites/bowling_ball/ball.png"
local P = 0x10000

-- What the ball rolls on: PX's DefaultColGroup, and without worms when fast
-- (NoWormsColGroup).
local SOLID = { "terrain", "worms", "crates", "oil" }
local SOLID_FAST = { "terrain", "crates", "oil" }

local function trunc(v) return v >= 0 and math.floor(v) or -math.floor(-v) end

local function sleep(b, a)
  if b.asleep then return end
  b.asleep, b.low = true, false
  b.sleep_x, b.sleep_y = a.x, a.y
  b.vx, b.vy, b.lvx, b.lvy = 0, 0, 0, 0
end

-- PX's UpdateRotation: spin from the ball's real motion, towards it.
local function spin(b)
  local rot = math.min(MAX_SPIN, SPIN_X * math.abs(b.rvx) + SPIN_Y * math.abs(b.rvy))
  if b.rvx < 0 then rot = -rot end
  b.rot = b.rot + rot
  while b.rot > math.pi do b.rot = b.rot - 2 * math.pi end
  while b.rot <= -math.pi do b.rot = b.rot + 2 * math.pi end
end

-- PX's PhysicsThink, a frame: the move traced with the ball's circle, a hit
-- turned into bounce along the surface's normal and friction along it, then
-- PX's inertia (a share of the speed's last change again), the low-speed
-- state and sleep.
local function think(a, b)
  if b.asleep then
    a.x, a.y = b.sleep_x, b.sleep_y
    return
  end
  local speed = math.sqrt(b.rvx * b.rvx + b.rvy * b.rvy)
  local vx2, vy2 = b.vx, b.vy + wa.world.gravity() / P
  local bounce, friction, ix, iy = BOUNCE, FRICTION, INERTIA_X, INERTIA_Y
  if b.low then
    bounce, friction, ix, iy = LOW_BOUNCE, LOW_FRICTION, LOW_INERTIA_X, LOW_INERTIA_Y
  end
  if vx2 == 0 and vy2 == 0 then vy2 = b.vy + 0.1 end
  local with = speed > FAST and SOLID_FAST or SOLID
  local x, y = a.x, a.y
  local function trace_from(sx, sy)
    return wa.world.trace({
      x0 = sx, y0 = sy, x1 = sx + trunc(vx2 * P), y1 = sy + trunc(vy2 * P),
      radius = RADIUS, with = with,
    })
  end
  local res = trace_from(x, y)
  if res and res.starts_solid then
    -- Stuck in something: carry on from where the path comes free, or sleep
    -- where it is (PX's unblocking falls back to Sleep too).
    if not res.left_x then
      sleep(b, a)
      return
    end
    x, y = res.left_x, res.left_y
    res = trace_from(x, y)
  end
  if res and res.hit then
    local spt = vx2 * res.tx + vy2 * res.ty
    local spn = vx2 * res.nx + vy2 * res.ny
    b.vx = vx2 - spn * res.nx * (bounce + 1) - spt * res.tx * friction
    b.vy = vy2 - spn * res.ny * (bounce + 1) - spt * res.ty * friction
    x, y = res.x, res.y
  else
    b.vx, b.vy = vx2, vy2
    x, y = x + trunc(vx2 * P), y + trunc(vy2 * P)
  end
  b.vx = b.vx + ix * (b.vx - b.lvx)
  b.vy = b.vy + iy * (b.vy - b.lvy)
  b.lvx, b.lvy = b.vx, b.vy
  b.rvx, b.rvy = (x - b.lx) / P, (y - b.ly) / P
  b.lx, b.ly = x, y
  a.x, a.y, a.vx, a.vy = x, y, 0, 0
  -- Slow for long enough: low speed, no inertia, so it settles.
  if speed < LOW_SPEED then
    b.low_timer = b.low_timer - 1
    if b.low_timer <= 0 then b.low, b.low_timer = true, LOW_FRAMES end
  else
    b.low, b.low_timer = false, LOW_FRAMES
  end
  b.sleep_timer = b.sleep_timer - 1
  if b.sleep_timer <= 0 then
    local dx, dy = (x - b.rec_x) / P, (y - b.rec_y) / P
    if math.sqrt(dx * dx + dy * dy) < SLEEP_DISTANCE then sleep(b, a) end
    b.sleep_timer, b.rec_x, b.rec_y = SLEEP_PERIOD, x, y
  end
end

shared.bowling_balls = {} -- actor id -> ball

local function spawn_ball(owner, x, y, vx, vy)
  local b = {
    vx = vx, vy = vy, lvx = vx, lvy = vy, rvx = 0, rvy = 0, lx = x, ly = y,
    rot = 0, low = false, low_timer = 200, sleep_timer = 200, rec_x = x, rec_y = y,
    asleep = false,
  }
  local a = wa.actors.spawn({
    sprite = SHEET, x = x, y = y, gravity = false, collide = false, owner = owner,
    on = function(a)
      if a.y >= wa.world.water() then
        shared.bowling_balls[a.id] = nil
        a:despawn()
        return
      end
      spin(b)
      think(a, b)
      a:gfx({ angle = math.deg(b.rot) % 360 })
    end,
  })
  shared.bowling_balls[a.id] = b
  return a, b
end
shared.spawn_bowling_ball = spawn_ball

fires.bowling_strike = function(ev)
  local dir = ev.worm.facing < 0 and -1 or 1
  for i = 1, STRIKE_BALLS do
    local offset = (i - (STRIKE_BALLS + 1) / 2) * STRIKE_SPACING
    spawn_ball(ev.worm, ev.x + math.floor(offset * P), wa.world.sky, STRIKE_SPEED * dir, 0)
  end
end
