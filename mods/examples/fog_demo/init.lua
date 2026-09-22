-- Fog Demo -- fog of war with no weapon attached.
--
-- A minimal pack that turns the fog engine on and sets a scheme, to show the
-- wa.fog API without touching the weapon table. Throw a grenade and watch the
-- blast open a hole in the dark; walk the map and watch explored ground stay
-- dimly remembered behind you.
--
-- Everything the fog does is presentation-only: it never touches WA's
-- deterministic simulation, so it cannot desync. A vanilla client in the same
-- match simply sees no fog.

-- The scheme, in one call. Every key is validated against the registry
-- (wa.fog.defaults() lists them); a bad key or out-of-range value raises a
-- Lua error naming the setting and its valid range, and leaves the running
-- fog untouched.
wa.fog.set({
  circle_radius = 250,   -- awareness circle around each worm, landscape pixels
  los = true,            -- terrain blocks vision
  discover = 1,          -- terrain memory: 0 off, 1 stale, 2 live, 3 premapped
  view_ownership = 3,    -- whose vision lights the map (3 = everyone, v1 default)
  explosion_circle = 40, -- temporary public light past a crater's rim
  fog_floor = 0,         -- brightness floor percent; 0 is full darkness
})

-- Master switch. Gated on the [fog] table in mod.toml; enabling reads the
-- live map size, so it only succeeds once a round is underway.
wa.fog.enable(true)

wa.on.hud(function()
  if wa.fog.enabled() then
    wa.hud.line("fog demo  circle " .. wa.fog.get("circle_radius")
      .. "  discover " .. wa.fog.get("discover"))
  else
    wa.hud.line("fog demo  (fog not running)")
  end
end)
