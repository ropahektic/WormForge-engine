-- Highlander: the slot claims. Split from rules.lua by tools/split_highlander.py.
local shared = require("lib/shared")
local PORTED = shared.PORTED
local PX_ART = shared.PX_ART
local PX_HOLDS = shared.PX_HOLDS
local WEP_NAME = shared.WEP_NAME
local WEP_PATCH = shared.WEP_PATCH
local fires = shared.fires

-- WA's muzzle can be inside a wall when a worm fires from beside it. A stock
-- Old Woman constructed there explodes before her first frame. Dispatch the
-- shot to Lua so she can be launched from a clear point on the muzzle-to-worm
-- segment, keeping WA's own Old Woman movement and five-second fuse.
local OLD_WOMAN_LAUNCHER_PATCH = {
  uses_turn = 0,
  subtype_param = 3, -- bazooka aim sheet, not the Sheep Launcher hold
  fire_type = 1,
  fire_method = 3, -- CreateWeaponProjectile, intercepted by on_fire
}

-- Our own changes over the generated record patches (tools/embed_weps.py),
-- kept here so WEP_PATCH can be generated again.
local PATCH_FIXES = {
  -- S.M.A.R.T Bomb: homing bits leave the bomb's red trail circles (stock 250,
  -- drawn as sprites/smart_bomb/trail), not WA's green gas cloud (608) the
  -- record named.
  mortar = { c_homing_sprite = 250 },
}

for _, list in ipairs({ PORTED, PX_ART }) do
  for _, w in ipairs(list) do
    -- copy_from + name + named patch (stock base); was weps/<slot>.wep
    local spec = w.wep and { wep = w.wep } or { copy_from = w.weapon, name = WEP_NAME[w.weapon] }
    if w.weapon == "old_woman" then
      spec.patch = OLD_WOMAN_LAUNCHER_PATCH
      spec.on_fire = function(fire) fires.old_woman(fire) end
    elseif not w.wep and WEP_PATCH[w.weapon] then
      spec.patch = WEP_PATCH[w.weapon]
    end
    if PATCH_FIXES[w.weapon] then
      local patch = {}
      for k, v in pairs(spec.patch or {}) do patch[k] = v end
      for k, v in pairs(PATCH_FIXES[w.weapon]) do patch[k] = v end
      spec.patch = patch
    end
    for key, value in pairs(w) do
      spec[key] = value
    end
    spec.hold = PX_HOLDS[w.weapon]
    wa.weapons.replace(spec)
  end
end

do
  local P = 0x10000
  local CLEAR = 3 * P
  local function clear(x, y)
    return not wa.land.solid(x, y)
      and not wa.land.solid(x - CLEAR, y)
      and not wa.land.solid(x + CLEAR, y)
      and not wa.land.solid(x, y - CLEAR)
      and not wa.land.solid(x, y + CLEAR)
  end

  fires.old_woman = function(fire)
    local w = fire.worm
    local x, y = fire.x, fire.y
    -- Check the actual muzzle first; only move the start inward if it is
    -- blocked. Fixed steps give every peer the same answer in lockstep.
    if not clear(x, y) then
      for step = 1, 16 do
        local nx = x + ((w.x - x) * step) // 16
        local ny = y + ((w.y - y) * step) // 16
        if clear(nx, ny) then x, y = nx, ny; break end
      end
    end
    wa.weapons.launch({ weapon = "old_woman", owner = w,
      x = x, y = y, vx = fire.vx, vy = fire.vy, fuse = 5000 })
  end
end
