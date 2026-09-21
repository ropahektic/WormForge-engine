-- Executed by the Lua 5.4 host tests, with the real pack as load_pack.
return function(load_pack)
  local PX = 0x10000
  local spec, turn, bodies, rounds = nil, nil, {}, 0
  local owner = { id = 1, team = 1, x = 100 * PX, y = 100 * PX,
                  vx = 0, vy = 0, facing = 1 }
  -- Planted anchor sinks 8px; head is 25px above it.
  local enemy = { id = 2, team = 2, x = 200 * PX, y = 83 * PX,
                  vx = 0, vy = 0, visible = true }
  local blocked, terrain_hits = false, 0
  wa = {
    weapons = { replace = function(s) spec = s end },
    msg = { START_TURN = 1 },
    on = { worm_message = function(cb) turn = cb end },
    worms = { all = function() return { owner, enemy } end },
    land = { trace = function(_, _, x1)
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
      if s.kind ~= "sentry" then
        assert(type(s.x) == "number" and type(s.y) == "number")
        assert(s.x < 300 * PX, "worm is hit before terrain behind it")
        rounds = rounds + 1
        return {}
      end
      local a = s
      a.id = #bodies + 1
      function a:look(path) self.sprite = path end
      function a:frame(i)
        assert(i >= 0 and i < 30, "frame stays within its sheet chunk")
        local pose, chunk = self.sprite:match("_p(%d+)_s(%d+)$")
        assert(pose and chunk, "turret sheet names encode pose and chunk")
        self.selected_frame = tonumber(pose) * 120 + tonumber(chunk) * 30 + i
      end
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
  assert(a.x == owner.x and a.y == owner.y)
  assert(a.vx == 19661 and a.vy == -9830 and a.loop == false)
  assert(a.selected_frame == 30, "initial right-facing frame")
  a.on(a) -- plants
  for _ = 1, 100 do a.on(a) end
  assert(rounds == 0, "PX acquisition gate ignores stationary worms")
  enemy.vx = PX
  turn(enemy, wa.msg.START_TURN)
  for _ = 1, 59 do a.on(a) end
  assert(rounds == 0, "activation delay")
  a.on(a)
  assert(rounds == 1 and a.selected_frame == 150, "first firing frame")
  assert(terrain_hits == 1, "firing exercised the terrain-hit table")
  for _ = 1, 3 do a.on(a) end
  assert(a.selected_frame == 270, "second firing frame")
  blocked = true
  for _ = 1, 8 do a.on(a) end
  assert(rounds == 1 and a.selected_frame < 120, "lost sight returns to idle art")
  blocked = false
  for _ = 1, 300 do a.on(a) end
  assert(rounds == 20, "exactly 20 rounds per turn")
  assert(a.selected_frame < 120, "empty turret stops firing animation")
  assert(a.x == owner.x and a.y == owner.y + 8 * PX and a.vx == 0 and a.vy == 0)
  turn(enemy, wa.msg.START_TURN)
  for _ = 1, 100 do a.on(a) end
  assert(rounds > 20, "reload next turn")
  owner.facing = -1
  spec.on_fire({ worm = owner, vx = 80 * PX, vy = -80 * PX })
  local left = bodies[2]
  assert(left.vx == -19661 and left.selected_frame == 90, "left placement")
  left.water = true
  left.on(left)
  assert(left.dead, "unplanted sentry drowns")

  spec.on_fire({ worm = owner })
  local damaged = bodies[3]
  damaged.on(damaged, { damage = 50 })
  assert(damaged.dead and damaged.destroyed_damage == 50,
         "50 health and PX destruction explosion")
end
