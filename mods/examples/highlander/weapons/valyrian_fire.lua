-- Highlander: Valyrian Fire (PX weapon_valyrianfire, rafa``2's valyrianfire_s).
local shared = require("lib/shared")
local fires = shared.fires

-- PX's FireFinal: from a worm in the water nothing is thrown and the bottle
-- goes back into the team's ammo; otherwise the bottle starts 15 pixels
-- further along the aim. WA's own projectile from there, so WA flies, burns
-- and explodes it as its Petrol Bomb.
local THROW_OUT = 15

fires.valyrian_fire = function(fire)
  local w = fire.worm
  if w.y >= wa.world.water() then
    -- Set, as the pack always writes ammo; infinite (-1) stays infinite.
    local n = w:ammo("petrol_bomb")
    if n >= 0 then w:ammo("petrol_bomb", { set = n + 1 }) end
    return
  end
  -- Along the aim, which is the throw's direction. Not PX's cos and sin of
  -- the aim angle: the start is simulation, and only sqrt is exact on
  -- every machine.
  local x, y = fire.x, fire.y
  local speed = math.sqrt(fire.vx * fire.vx + fire.vy * fire.vy)
  if speed > 0 then
    x = x + math.floor(THROW_OUT * 0x10000 * fire.vx / speed)
    y = y + math.floor(THROW_OUT * 0x10000 * fire.vy / speed)
  end
  wa.weapons.launch({ weapon = "petrol_bomb", owner = w, x = x, y = y, vx = fire.vx, vy = fire.vy })
end
