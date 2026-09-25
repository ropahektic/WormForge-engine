-- Executed by the Lua 5.4 host tests, with the real pack as load_pack.
return function(load_pack)
  local PX = 0x10000
  local spec, turn, bodies, rounds = nil, nil, {}, 0
  local guns, flares = {}, {}
  local owner = { id = 1, team = 1, x = 100 * PX, y = 100 * PX,
                  vx = 0, vy = 0, facing = 1 }
  -- Planted anchor sinks 8px; head is 25px above it.
  local enemy = { id = 2, team = 2, x = 200 * PX, y = 83 * PX,
                  vx = 0, vy = 0, visible = true }
  local blocked, terrain_hits = false, 0
  -- A wall in front of the worm, for placement: its x, or nil for none.
  local wall
  -- Stands in for wa.random, which draws from WA's shared RNG in the real
  -- engine. A fixed sequence here on purpose: this test asserts the sentry is
  -- deterministic, so its randomness must be too. The mapping onto the
  -- requested range matches the engine's -- lo + draw % span -- so a change on
  -- either side shows up as a different shot pattern.
  local draw_at = 0
  local DRAWS = { 7, 23, 41, 4, 88, 15, 62, 30, 51, 9, 77, 36 }
  wa = {
    random = function(a, b)
      draw_at = draw_at + 1
      local d = DRAWS[((draw_at - 1) % #DRAWS) + 1]
      if a == nil then return d end
      local lo, hi = 1, a
      if b ~= nil then lo, hi = a, b end
      return lo + d % (hi - lo + 1)
    end,
    weapons = { replace = function(s) spec = s end },
    msg = { START_TURN = 1 },
    on = { worm_message = function(cb) turn = cb end },
    worms = { all = function() return { owner, enemy } end },
    land = { trace = function(x0, _, x1)
      if wall and (wall - x0) * (x1 - x0) > 0 and math.abs(wall - x0) <= math.abs(x1 - x0) then
        return { x = wall, y = owner.y, hit = true }
      end
      if blocked then return { x = 101 * PX, y = 75 * PX, hit = true } end
      -- Sight to the worm is clear, but a bullet ray reaches terrain behind it.
      if x1 > enemy.x then
        terrain_hits = terrain_hits + 1
        return { x = 300 * PX, y = 75 * PX, hit = true }
      end
    end },
    actors = { spawn = function(s)
      if s.kind == "casing" then
        return {}          -- cosmetic shell ejection, not a fired round
      end
      if s.kind == "sentry_flare" then
        -- Additive muzzle flash: its own quad so the team tint cannot reach
        -- it. Display only, like the gun.
        local fl = s
        function fl:frame(i) self.selected_frame = i end
        function fl:gfx(spec) self.tint, self.blend = spec.tint, spec.blend end
        function fl:despawn() self.dead = true end
        flares[#flares + 1] = fl
        return fl
      end
      if s.kind == "sentry_gun" then
        -- The rotating gun: display only, no physics. It records what the
        -- brain asks of it so the test can assert on aim and team colour.
        local g = s
        function g:frame(i) self.selected_frame = i end
        function g:gfx(spec)
          self.angle = spec.angle
          self.tint = spec.tint
        end
        function g:despawn() self.dead = true end
        guns[#guns + 1] = g
        return g
      end
      if s.kind ~= "sentry" then
        assert(type(s.x) == "number" and type(s.y) == "number")
        assert(s.x < 300 * PX, "worm is hit before terrain behind it")
        rounds = rounds + 1
        return {}
      end
      local a = s
      a.id = #bodies + 1
      function a:look(path) self.sprite = path end
      function a:frame(i) self.selected_frame = i end
      function a:gfx() end
      function a:gravity() end
      function a:move() return { land = not self.water } end
      function a:advance() end
      function a:in_water() return self.water end
      function a:despawn() self.dead = true end
      function a:explode(e) self.destroyed_damage = e.damage end
      function a:sound() end
      bodies[#bodies + 1] = a
      return a
    end },
  }
  load_pack()
  assert(spec.weapon == "bazooka" and spec.copy_from == "mine")
  turn(owner, wa.msg.START_TURN)
  -- Huge launch velocity and a distant launch position must not affect placement.
  spec.on_fire({ worm = owner, x = 999 * PX, y = 999 * PX, vx = 80 * PX, vy = -80 * PX })
  local a = bodies[1]
  assert(a.x == owner.x + 5 * PX and a.y == owner.y,
         "planted a little in front of the worm, not on it")
  assert(a.vx == 19661 and a.vy == -9830 and a.loop == false)
  local gun, flare = guns[1], flares[1]
  assert(gun, "the turret spawns its rotating gun")
  assert(flare, "and a separate additive muzzle flash")
  assert(a.sprite == nil or a.sprite:find("base"), "the turret actor is the tripod")
  assert(gun.angle == 0, "facing right is 0 degrees of rotation")
  a.on(a) -- plants
  for _ = 1, 100 do a.on(a) end
  assert(rounds == 0, "PX acquisition gate ignores stationary worms")
  enemy.vx = PX
  turn(enemy, wa.msg.START_TURN)
  for _ = 1, 59 do a.on(a) end
  assert(rounds == 0, "activation delay")
  a.on(a)
  assert(rounds == 1 and gun.selected_frame == 1, "first firing frame")
  assert(flare.selected_frame == 1, "the flare lights on the first firing pose")
  assert(flare.blend == "add" and flare.tint == 0xFFFFFF,
         "the flare is additive and untinted, so a blue turret still flashes yellow")
  assert(terrain_hits == 1, "firing exercised the terrain-hit table")
  for _ = 1, 3 do a.on(a) end
  assert(gun.selected_frame == 2, "second firing frame")
  blocked = true
  for _ = 1, 8 do a.on(a) end
  assert(rounds == 1 and gun.selected_frame == 0, "lost sight returns to idle art")
  assert(flare.selected_frame == 0, "and the flare goes dark")
  blocked = false
  for _ = 1, 300 do a.on(a) end
  assert(rounds == 20, "exactly 20 rounds per turn")
  assert(gun.selected_frame == 0, "empty turret stops firing animation")
  assert(a.x == owner.x + 5 * PX and a.y == owner.y + 8 * PX and a.vx == 0 and a.vy == 0)
  turn(enemy, wa.msg.START_TURN)
  for _ = 1, 100 do a.on(a) end
  assert(rounds > 20, "reload next turn")
  owner.facing = -1
  spec.on_fire({ worm = owner, vx = 80 * PX, vy = -80 * PX })
  local left = bodies[2]
  assert(left.vx == -19661, "left placement")
  assert(left.x == owner.x - 5 * PX, "in front of a worm facing left is to its left")
  assert(guns[2].angle == 180, "facing left is a half turn")
  left.water = true
  left.on(left)
  assert(left.dead, "unplanted sentry drowns")

  -- A wall within reach leaves the turret at the worm; one past it does not
  -- matter.
  owner.facing = 1
  wall = owner.x + 15 * PX
  spec.on_fire({ worm = owner })
  assert(bodies[3].x == owner.x + 5 * PX, "a wall past the spot is no obstacle")
  wall = owner.x + 3 * PX
  spec.on_fire({ worm = owner })
  assert(bodies[4].x == owner.x, "a wall in the way leaves it at the worm")
  wall = nil

  spec.on_fire({ worm = owner })
  local damaged = bodies[5]
  damaged.on(damaged, { damage = 50 })
  assert(damaged.dead and damaged.destroyed_damage == 50,
         "50 health and PX destruction explosion")
end
