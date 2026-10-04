-- Highlander: Bowling Ball Strike (PX weapon_bowlingball: bbstrike.txt and
-- bowlingball.txt's CBowlingBall).
local shared = require("lib/shared")
local fires = shared.fires

-- The strike: PX's plane drops three balls 48 pixels apart from 128 above
-- the map, moving 5 a frame the way it flies. Here they drop together, the
-- way WA's strike arrow points: the sign of the shot's x speed (ev.vx), which
-- PX's PxAirstrike reads, from the left unless the arrow keys turned it. As
-- PX's plane drops them early by how far they drift on the way down (its
-- SpeedCompensation), the drop line is set back by the middle ball's own
-- drift down to the click, so it lands on the click. PX's plane is not drawn.
local STRIKE_BALLS = 3
local STRIKE_SPACING = 48
local STRIKE_SPEED = 5
local STRIKE_Y = -128         -- PX's PlaneAppearYOffset

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
local FAST = 4.0              -- MinDamageSpeed: faster, it rolls through worms and hurts
local SPIN_X, SPIN_Y, MAX_SPIN = 0.06, 0.02, 0.6
local SHEET = "sprites/bowling_ball/ball.png"

-- PX's damage (ProcessBallOnOtherCollision): faster than FAST, whatever the
-- ball overlaps takes 5 at that speed up to 60 at 15 and more; crates and oil
-- drums 100. A worm then takes nothing more for 12 frames (PX's
-- DamageRecoverTime). Balls push each other apart (ProcessBallOnBallCollision)
-- by 0.6 * sqrt(speed), of which the pushed ball keeps 0.7 (GunForceMultiplier).
local MAX_DAMAGE_SPEED = 15.0
local MIN_DAMAGE, MAX_DAMAGE = 5.0, 60.0
local OBJECT_DAMAGE = 100
local DAMAGE_FORCE = 0.7      -- DamageForceMultiplier
local RECOVER_FRAMES = 12
local PUSH_SPEED = 4.0        -- MinPushSpeed
local PUSH_FORCE, PUSH_KEEP = 0.6, 0.7
local SOUND_CRUSH, SOUND_HIT = 33, 101
-- A ball counts as hitting what it touches: PX traces it with a smaller
-- mask than the 16-pixel circle it hits with, so it overlaps a drum or
-- crate it stops against; ours traces with the circle, which stops it a
-- touch away from them.
local TOUCH = 2

-- PX's Zap(0), the Lightning Strike's "overcharge": the ball wakes and is
-- thrown up, 5 to 10 a frame within 0.1 of straight up, and is electrified
-- for good (PX never counts its ZapTurnsRemaining down): awake and not slow,
-- its friction is negative, so it speeds up along what it rolls on. PX's
-- strike reaches it within its 12-pixel hit circle of the ball's 16.
local ZAPPED_FRICTION = -0.05
local ZAP_REACH = 12
-- The ball is drawn this far below where its circle is, in pixels: at the
-- circle's own place it looked to float over the ground. In front of the
-- terrain (actors are behind it by default), behind the worms (0xD0000).
local SPRITE_DROP = 4
local LIFT = 0x8000           -- half a pixel, 16.16
local BALL_DEPTH = 0x000E0000
local P = 0x10000

-- What the ball rolls on: PX's DefaultColGroup, and without worms when fast
-- (NoWormsColGroup).
local SOLID = { "terrain", "worms", "crates", "oil" }
local SOLID_FAST = { "terrain", "crates", "oil" }

local function trunc(v) return v >= 0 and math.floor(v) or -math.floor(-v) end

-- The circles PX's masks give what the ball can hit (as wa.world.trace's).
local function target_radius(obj)
  if obj.kind == "worm" then return 6 end
  if obj.kind == "mine" then return 5 end
  return 9
end

local function sleep(b)
  if b.asleep then return end
  b.asleep, b.low = true, false
  b.sleep_x, b.sleep_y = b.x, b.y
  b.vx, b.vy, b.lvx, b.lvy = 0, 0, 0, 0
  b.rvx, b.rvy = 0, 0
end

-- PX's Wake: moving again, its timers restarted.
local function wake(b)
  if not b.asleep and not b.low then return end
  b.asleep, b.low = false, false
  b.low_timer, b.sleep_timer = 200, 200
  b.rec_x, b.rec_y = b.x, b.y
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
-- state and sleep. The ball's place is b.x, b.y; the actor only draws it.
-- Returns the speed the frame started with, which the hits below go by.
local function think(b)
  if b.asleep then
    -- No real speed asleep, as PX's: it stops turning too.
    b.x, b.y = b.sleep_x, b.sleep_y
    b.rvx, b.rvy = 0, 0
    return 0
  end
  local speed = math.sqrt(b.rvx * b.rvx + b.rvy * b.rvy)
  local vx2, vy2 = b.vx, b.vy + wa.world.gravity() / P
  local bounce, friction, ix, iy = BOUNCE, FRICTION, INERTIA_X, INERTIA_Y
  if b.low then
    bounce, friction, ix, iy = LOW_BOUNCE, LOW_FRICTION, LOW_INERTIA_X, LOW_INERTIA_Y
  elseif b.zapped then
    friction = ZAPPED_FRICTION
  end
  if vx2 == 0 and vy2 == 0 then vy2 = b.vy + 0.1 end
  local with = speed > FAST and SOLID_FAST or SOLID
  local x, y = b.x, b.y
  local dx, dy = trunc(vx2 * P), trunc(vy2 * P)
  local function trace_from(sx, sy, k)
    return wa.world.trace({
      x0 = sx, y0 = sy, x1 = sx + k * dx, y1 = sy + k * dy, radius = RADIUS, with = with,
    })
  end
  local res = trace_from(x, y, 1)
  if res and res.starts_solid then
    -- Stuck in something (another ball can push it in): carry on from where
    -- the path comes free; else, as PX's unblocking, back out along the move
    -- eight times its length and come forward from there; else sleep where
    -- it is, as PX does.
    if not res.left_x then
      res = trace_from(x, y, -8)
      if not (res and res.left_x) then
        sleep(b)
        return 0
      end
    end
    x, y = res.left_x, res.left_y
    res = trace_from(x, y, 1)
  end
  if res and res.hit then
    local spt = vx2 * res.tx + vy2 * res.ty
    local spn = vx2 * res.nx + vy2 * res.ny
    b.vx = vx2 - spn * res.nx * (bounce + 1) - spt * res.tx * friction
    b.vy = vy2 - spn * res.ny * (bounce + 1) - spt * res.ty * friction
    x, y = res.x, res.y
    -- Rest half a pixel off the surface, as PX's ball does (it stops a
    -- step short, up to a pixel): touching it, a ball gliding along would
    -- hit it at once every frame and stand still at speed.
    local lx, ly = x + trunc(res.nx * LIFT), y + trunc(res.ny * LIFT)
    local lift = wa.world.trace({ x0 = x, y0 = y, x1 = lx, y1 = ly, radius = RADIUS, with = with })
    if not (lift and (lift.hit or lift.starts_solid)) then x, y = lx, ly end
  else
    b.vx, b.vy = vx2, vy2
    x, y = x + trunc(vx2 * P), y + trunc(vy2 * P)
  end
  b.vx = b.vx + ix * (b.vx - b.lvx)
  b.vy = b.vy + iy * (b.vy - b.lvy)
  b.lvx, b.lvy = b.vx, b.vy
  b.rvx, b.rvy = (x - b.lx) / P, (y - b.ly) / P
  b.lx, b.ly = x, y
  b.x, b.y = x, y
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
    if math.sqrt(dx * dx + dy * dy) < SLEEP_DISTANCE then sleep(b) end
    b.sleep_timer, b.rec_x, b.rec_y = SLEEP_PERIOD, x, y
  end
  return speed
end

shared.bowling_balls = {} -- actor id -> ball

-- Frames counted by the oldest ball still rolling: the worms' 12-frame
-- recovery needs a clock, and only balls hit.
local clock, clock_ball = 0, nil
local recover = {} -- worm id -> clock at which it can be hurt again

local function tick(id)
  if clock_ball == nil or not shared.bowling_balls[clock_ball] then clock_ball = id end
  if clock_ball == id then clock = clock + 1 end
end

local function overlaps(b, obj)
  local dx, dy = (obj.x - b.x) / P, (obj.y - b.y) / P
  local r = RADIUS + target_radius(obj) + TOUCH
  return dx * dx + dy * dy <= r * r
end

-- PX's ProcessBallOnBallCollision: every other ball this one overlaps is
-- pushed away from it, and wakes.
local function push_balls(id, b, speed)
  local push = PUSH_FORCE * math.sqrt(speed)
  for other_id, o in pairs(shared.bowling_balls) do
    if other_id ~= id then
      local dx, dy = (o.x - b.x) / P, (o.y - b.y) / P
      local d2 = dx * dx + dy * dy
      if d2 > 0 and d2 <= (2 * RADIUS) * (2 * RADIUS) then
        local d = math.sqrt(d2)
        wake(o)
        o.vx = o.vx + PUSH_KEEP * push * dx / d
        o.vy = o.vy + PUSH_KEEP * push * dy / d
      end
    end
  end
end

-- A standing worm hit from above, straight down, is crushed (PX's
-- "crushable" states, the ones a worm stands in).
local CRUSHABLE = { standing = true, firing = true, fired = true, prefiring = true }

-- PX's ProcessBallOnOtherCollision: the hit's damage from the ball's speed,
-- and its push from how the ball really moved this frame.
local function hit_others(a, b, speed)
  local dmg = MIN_DAMAGE + (MAX_DAMAGE - MIN_DAMAGE)
    * (math.min(speed, MAX_DAMAGE_SPEED) - FAST) / (MAX_DAMAGE_SPEED - FAST)
  local function hit(obj)
    if not overlaps(b, obj) then return end
    local damage = 0
    if obj.kind == "worm" then
      if obj.hp <= 0 or (recover[obj.id] or 0) > clock then return end
      damage = dmg
      recover[obj.id] = clock + RECOVER_FRAMES
    elseif obj.kind == "crate" or obj.kind == "oil" then
      damage = OBJECT_DAMAGE
    else
      damage = dmg
    end
    damage = trunc(damage)
    if damage <= 0 then return end
    local fx, fy = b.rvx, b.rvy
    local ax = math.abs(fx)
    local crushed = obj.kind == "worm" and CRUSHABLE[obj.state] and fy > FAST and ax / fy < 0.2
    if crushed then
      fx, fy = 0, 10
    else
      fy = fy < 0 and fy - ax or fy - 2 * ax
      fx, fy = fx * DAMAGE_FORCE, fy * DAMAGE_FORCE
    end
    if obj.kind == "worm" then
      obj:hurt({ amount = damage, vx = trunc(fx * P), vy = trunc(fy * P) })
    else
      obj:hit(damage, trunc(fx * P), trunc(fy * P))
    end
    a:sound(crushed and SOUND_CRUSH or SOUND_HIT)
  end
  for _, group in ipairs({ wa.worms, wa.mines, wa.crates, wa.oil }) do
    for _, obj in ipairs(group.all() or {}) do hit(obj) end
  end
end

-- The balls by id, in id order: whatever draws from WA's RNG for them does
-- so in the same order on every machine.
local function balls_in_order()
  local ids = {}
  for id in pairs(shared.bowling_balls) do ids[#ids + 1] = id end
  table.sort(ids)
  return ids
end

local function zap(b)
  b.zapped = true
  wake(b)
  local ang = (wa.random(0, 2000) - 1000) / 10000
  local speed = 5 + wa.random(0, 1000) / 200
  -- sin and cos by their series: |ang| <= 0.1, so exact to 1e-7, and no
  -- trig in the simulation.
  local a2 = ang * ang
  b.vx = (ang - ang * a2 / 6) * speed
  b.vy = -(1 - a2 / 2 + a2 * a2 / 24) * speed
end

-- The Lightning Strike: every ball within reach of where it hit.
shared.zap_bowling_balls = function(x, y)
  local reach = (ZAP_REACH + RADIUS) * (ZAP_REACH + RADIUS)
  for _, id in ipairs(balls_in_order()) do
    local b = shared.bowling_balls[id]
    local dx, dy = (b.x - x) / P, (b.y - y) / P
    if dx * dx + dy * dy <= reach then zap(b) end
  end
end

-- Where a beam from (x0, y0) along the unit (dx, dy), at most len pixels,
-- first meets a ball's circle (PX's ball is an oil drum to the strike's
-- trace), or nil.
shared.bowling_ball_on_beam = function(x0, y0, dx, dy, len)
  local best
  for _, id in ipairs(balls_in_order()) do
    local b = shared.bowling_balls[id]
    local cx, cy = (b.x - x0) / P, (b.y - y0) / P
    local along = cx * dx + cy * dy
    local miss = cx * cx + cy * cy - along * along
    if miss <= RADIUS * RADIUS then
      local t = along - math.sqrt(RADIUS * RADIUS - miss)
      if t > 0 and t < len and (not best or t < best) then best = t end
    end
  end
  if best then return x0 + trunc(best * dx * P), y0 + trunc(best * dy * P) end
end

local function spawn_ball(owner, x, y, vx, vy)
  local b = {
    x = x, y = y, vx = vx, vy = vy, lvx = vx, lvy = vy, rvx = 0, rvy = 0, lx = x, ly = y,
    rot = 0, low = false, low_timer = 200, sleep_timer = 200, rec_x = x, rec_y = y,
    asleep = false,
  }
  local a = wa.actors.spawn({
    sprite = SHEET, x = x, y = y + SPRITE_DROP * P, gravity = false, collide = false, owner = owner,
    on = function(a)
      if b.y >= wa.world.water() then
        shared.bowling_balls[a.id] = nil
        a:despawn()
        return
      end
      tick(a.id)
      spin(b)
      local speed = think(b)
      if speed > PUSH_SPEED then push_balls(a.id, b, speed) end
      if speed > FAST then hit_others(a, b, speed) end
      a.x, a.y, a.vx, a.vy = b.x, b.y + SPRITE_DROP * P, 0, 0
      a:gfx({ angle = math.deg(b.rot) % 360, depth = BALL_DEPTH })
    end,
  })
  shared.bowling_balls[a.id] = b
  return a, b
end
shared.spawn_bowling_ball = spawn_ball

-- How far a ball dropped at y0 moving vx drifts sideways before its centre
-- falls to y1: think's flight through the air, gravity and PX's inertia,
-- frame by frame (no trig, the same on every machine).
local function drift(y0, y1, vx)
  local g = wa.world.gravity() / P
  if g <= 0 then return 0 end
  local x, y, vy, lvy = 0, y0, 0, 0
  for _ = 1, 3000 do
    if y >= y1 then break end
    local vy2 = vy + g
    x, y = x + trunc(vx * P), y + trunc(vy2 * P)
    vy = vy2 + INERTIA_Y * (vy2 - lvy)
    lvy = vy
  end
  return x
end

fires.bowling_strike = function(ev)
  local dir = (ev.vx or 0) < 0 and -1 or 1
  local y0 = STRIKE_Y * P
  local back = drift(y0, ev.y - RADIUS * P, STRIKE_SPEED * dir)
  for i = 1, STRIKE_BALLS do
    local offset = (i - (STRIKE_BALLS + 1) / 2) * STRIKE_SPACING
    spawn_ball(ev.worm, ev.x - back + math.floor(offset * P), y0, STRIKE_SPEED * dir, 0)
  end
end
