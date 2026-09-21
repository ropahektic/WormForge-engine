-- Colour Test -- mortar slot, thrown like a grenade.
--
-- A demo pack with no tactical ambition: every visible thing it does is
-- something WA's indexed blitter cannot do, drawn by the GPU renderer. Throw
-- it at a wall and watch what the palette path could not have shown you:
--
--   exact rotation     the orb spins by a fractional number of degrees per
--                      frame off ONE 32x32 sheet. Baking those angles as
--                      sprites is what cost the sentry gun 72 of the 223 WA
--                      sprite slots every pack shares.
--   true colour        sprites/prism.png holds ~850 distinct colours in a
--                      single frame -- more than a 256-entry palette has
--                      room for if it gave up every other colour in the map.
--                      Nothing tints it, so what you see is the file.
--   real alpha         the halo and the sparks are smooth radial ramps. The
--                      old path hard-thresholded alpha (below 16 dropped,
--                      everything else forced opaque), which turns a
--                      gradient into a hard-edged disc.
--   additive blending  halo, trail and burst all add to what is behind them
--                      instead of covering it, so they brighten terrain and
--                      worms rather than punching holes in the scene.
--   runtime tint       one greyscale spark sheet covers the entire spectrum.
--                      Six baked team copies of the sentry's turret existed
--                      only because this was impossible.
--
-- Total cost in WA sprite slots: zero. `gpu_prefix` sends all three sheets to
-- the texture atlas instead, and none of them touches the map palette.

local PX = 0x10000

local PRISM = "sprites/prism.png"
local HALO = "sprites/halo.png"
local SPARK = "sprites/spark.png"
local SPRITES = { PRISM, HALO, SPARK }

-- Terrain's own queue depth is 0x180000; anything lower draws in front of it
-- and is occluded only by the HUD. The orb is a thrown object in the air, so
-- terrain should never bury it.
local IN_FRONT_OF_TERRAIN = 0x10000

local FUSE = 150 -- frames; 3 seconds at WA's 50 fps
local DAMAGE = 45
local BOOM_ID = 100

local SPIN = 6.5 -- degrees per frame -- deliberately not a whole number
local HUE_STEP = 5 -- degrees of hue per frame
local RADIUS = 5 -- collision radius, pixels
local BOUNCE = 0.55 -- speed kept across a bounce
local MAX_STEPS = 6 -- collision sub-steps per frame, at most
local TRAIL_EVERY = 2
local TRAIL_LIFE = 24
local BURST = 30 -- sparks in the detonation ring
local BURST_LIFE = 40
local BURST_SPEED = 2.6

local state = {}

-- HSV to 0xRRGGBB. The renderer multiplies this into the texel, so a
-- greyscale sheet plus this function is the whole of the colour pipeline --
-- no baked copies, no palette entries, no reserved indices.
local function hue(h, v)
  h = h % 360
  local c = v
  local x = c * (1 - math.abs((h / 60) % 2 - 1))
  local r, g, b
  if h < 60 then
    r, g, b = c, x, 0
  elseif h < 120 then
    r, g, b = x, c, 0
  elseif h < 180 then
    r, g, b = 0, c, x
  elseif h < 240 then
    r, g, b = 0, x, c
  elseif h < 300 then
    r, g, b = x, 0, c
  else
    r, g, b = c, 0, x
  end
  return (math.floor(r * 255) << 16) | (math.floor(g * 255) << 8) | math.floor(b * 255)
end

-- Twelve probe directions, used both to test for ground and to estimate the
-- surface normal from it. Built once: lockstep forbids math.random, and this
-- wants to be identical on every machine anyway.
local RING = {}
for i = 0, 11 do
  local t = math.rad(i * 30)
  RING[#RING + 1] = { math.cos(t), math.sin(t) }
end

local function probe(x, y, d)
  return wa.land.solid(x + math.floor(d[1] * RADIUS * PX), y + math.floor(d[2] * RADIUS * PX))
end

local function blocked(x, y)
  if wa.land.solid(x, y) then
    return true
  end
  for _, d in ipairs(RING) do
    if probe(x, y, d) then
      return true
    end
  end
  return false
end

-- The normal points away from the solid mass: sum the directions that hit
-- ground and negate. Good enough to bounce off a slope convincingly, and it
-- needs nothing from WA beyond wa.land.solid.
local function normal_at(x, y)
  local nx, ny = 0.0, 0.0
  for _, d in ipairs(RING) do
    if probe(x, y, d) then
      nx = nx - d[1]
      ny = ny - d[2]
    end
  end
  local len = math.sqrt(nx * nx + ny * ny)
  if len < 0.001 then
    return nil
  end
  return nx / len, ny / len
end

-- Sparks are owned by the thrower, never by the orb.
--
-- WA links a spawned entity to its owner, so a spark that outlived the orb
-- that spawned it would be holding a freed parent -- the same shape as the
-- wormcraft crash. The worm outlives the burst. If the thrower died during
-- the fuse there is nothing safe to hang them on, so the explosion still
-- happens and only the decoration is skipped.
local function alive(w)
  if not w then
    return false
  end
  for _, o in ipairs(wa.worms.all()) do
    if o.id == w.id then
      return true
    end
  end
  return false
end

-- Flight, in sub-steps.
--
-- Two reasons this is not a:move(). First, a:move's terrain flag reports
-- contact but does not stop anything, so integrating first would bury the orb
-- in the ground before we could react to it. Second, a full-power throw
-- covers more than the orb's own radius in one frame, and a single test at
-- the destination would let it pass clean through a thin wall -- the first
-- thing anyone testing a bouncing weapon will try.
--
-- On a bounce the remaining sub-steps are dropped: the frame's motion has
-- been spent on the impact.
local function fly(a)
  local speed = math.max(math.abs(a.vx), math.abs(a.vy))
  local steps = math.min(MAX_STEPS, speed // (RADIUS * PX) + 1)
  for _ = 1, steps do
    local nx = a.x + a.vx // steps
    local ny = a.y + a.vy // steps
    if blocked(nx, ny) then
      local n1, n2 = normal_at(nx, ny)
      if n1 then
        local vx, vy = a.vx / PX, a.vy / PX
        local dot = vx * n1 + vy * n2
        a.vx = math.floor((vx - 2 * dot * n1) * BOUNCE * PX)
        a.vy = math.floor((vy - 2 * dot * n2) * BOUNCE * PX)
      else
        -- Ground on every side: reverse and damp hard rather than reflecting
        -- off a normal that does not exist.
        a.vx = -(a.vx // 2)
        a.vy = -(a.vy // 2)
      end
      return
    end
    a.x, a.y = nx, ny
  end
end

local function spark(owner, x, y, vx, vy, h, life, size, falls)
  wa.actors.spawn({
    sprite = SPARK,
    x = x,
    y = y,
    vx = vx,
    vy = vy,
    gravity = false, -- ticked by hand below, like the sentry's casings
    collide = false,
    origin = "center",
    owner = owner,
    kind = "colourtest_spark",
    on = (function()
      local t = 0
      return function(p)
        t = t + 1
        if t >= life then
          p:despawn()
          return
        end
        if falls then
          p:gravity()
        end
        p:move({ terrain = false, worms = false })
        local k = 1 - t / life
        -- Additive has no alpha to lower -- premultiplied alpha 0 IS the
        -- blend mode -- so fading means walking the tint down to black.
        -- tint() keeps the blend mode that blend() set at spawn, and this is
        -- the whole reason those two are separate calls.
        p:tint(hue(h, k * k))
        p:scale(size * (0.35 + 0.65 * k))
      end
    end)(),
  })
  return true
end

local function detonate(a, s)
  local x, y = a.x, a.y
  -- One full turn of hue across the ring: thirty simultaneous colours, every
  -- one of them additive, all inside a single batched draw call.
  if alive(s.owner) then
    for i = 0, BURST - 1 do
      local ang = math.rad(i * 360 / BURST)
      local speed = BURST_SPEED * (0.55 + 0.45 * ((i % 5) / 4))
      spark(
        s.owner,
        x,
        y,
        math.floor(math.cos(ang) * speed * PX),
        math.floor(math.sin(ang) * speed * PX),
        i * 360 // BURST,
        BURST_LIFE,
        1.4,
        true
      )
    end
  end
  if s.halo then
    s.halo:despawn()
    s.halo = nil
  end
  state[a.id] = nil
  a:explode({ damage = DAMAGE, id = BOOM_ID, below = false })
  a:despawn()
end

local function brain(a)
  local s = state[a.id]
  if not s then
    a:despawn()
    return
  end

  s.t = s.t + 1
  s.hue = (s.hue + HUE_STEP) % 360

  a:gravity()
  fly(a)

  -- Exact rotation, off one sheet. SPIN is 6.5 degrees precisely because the
  -- palette path could only have done 3-degree steps, and only by spending a
  -- sprite slot on each one.
  s.spin = (s.spin + SPIN) % 360
  a:angle(s.spin)

  -- The halo rides the orb: additive, hue-cycled, breathing. One greyscale
  -- 64x64 file, every colour it ever shows.
  if s.halo then
    s.halo.x, s.halo.y = a.x, a.y
    s.halo:tint(hue(s.hue + 180, 1.0))
    s.halo:scale(0.85 + 0.22 * math.sin(s.t * 0.19))
  end

  if s.t % TRAIL_EVERY == 0 and alive(s.owner) then
    spark(s.owner, a.x, a.y, a.vx // 6, a.vy // 6, s.hue, TRAIL_LIFE, 0.9, false)
  end

  if a:in_water() then
    if s.halo then
      s.halo:despawn()
      s.halo = nil
    end
    state[a.id] = nil
    a:despawn()
    return
  end

  s.fuse = s.fuse - 1
  if s.fuse <= 0 then
    detonate(a, s)
  end
end

wa.weapons.replace({
  weapon = "mortar",
  copy_from = "grenade",
  name = "Colour Test",
  is_weapon = true,

  params = {
    -- Single-frame sheets, not strips: the default of 8 would slice each one
    -- into eight slivers. Every frame of motion here is angle, scale or
    -- tint, so there is nothing to slice.
    sprite_columns = 1,
    -- All three sheets go to the GPU atlas: no WA sprite slots, no palette
    -- quantisation, full 8-bit alpha kept.
    gpu_prefix = "sprites/",
  },
  sprites = SPRITES,

  -- The stock grenade supplies the throw -- aim, power charge, release -- and
  -- fire_ev carries the resulting launch vector. The projectile from here on
  -- is ours, which is what lets it be drawn by the GPU renderer at all.
  --
  -- The fuse is fixed at FUSE rather than following the 1-5 the grenade slot
  -- lets you pick: that selection is not exposed to packs yet.
  on_fire = function(fire_ev)
    local worm = fire_ev.worm
    local orb = wa.actors.spawn({
      sprite = PRISM,
      x = fire_ev.x,
      y = fire_ev.y,
      vx = fire_ev.vx,
      vy = fire_ev.vy,
      gravity = false,
      collide = false,
      loop = false,
      origin = "center",
      owner = worm,
      kind = "colourtest",
      persist = true,
      on = brain,
    })

    -- Drawn in front of terrain and left untinted: the prism sheet's own
    -- colours are the point, and a tint would multiply them away.
    orb:gfx({ depth = IN_FRONT_OF_TERRAIN })

    local halo = wa.actors.spawn({
      sprite = HALO,
      x = fire_ev.x,
      y = fire_ev.y,
      vx = 0,
      vy = 0,
      gravity = false,
      collide = false,
      loop = false,
      origin = "center",
      owner = worm,
      kind = "colourtest_halo",
      persist = true,
      on = function() end, -- placed by the orb's brain every frame
    })
    -- Set once. Every later frame calls tint() and scale() only, and neither
    -- disturbs this: each setter writes its own field and nothing else.
    halo:gfx({ blend = "add", depth = IN_FRONT_OF_TERRAIN })

    state[orb.id] = {
      owner = worm,
      halo = halo,
      fuse = FUSE,
      spin = 0,
      hue = 0,
      t = 0,
    }
  end,
})

wa.on.hud(function()
  wa.hud.line("colour test  spin 6.5deg/frame  additive  0 sprite slots")
end)
