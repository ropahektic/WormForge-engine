//! Lua 5.4 host. One interpreter per match, created at gfx InitGraphic
//! (`SetLayerVisibility(3, 1)`), before InitWeaponTable overlays slots.
//!
//! `wa.*` is a dispatcher: named ids, first owner wins. Scripts do not
//! override stock C++ methods. See docs/architecture.md.

mod actor;
mod api;
mod dispatch;
mod sandbox;
mod spawn;

pub use actor::{
    actor_frame, actor_freed, actor_spawned, bind_actor_native, gfx_field, weapon_click,
    weapon_fire, weapon_select, ActorEvent, ActorNative, SpawnArgs,
};
pub use dispatch::{
    gravity_after, gravity_before, hurt, missile_frame, run_hud, worm_message, Hurt,
};

use crate::api::BindContext;
use gfx::SpriteStrip;
use mlua::Lua;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use weapons::{
    parse_stock_weapon, resolve_claims, LoadedMod, OverlayClaim, OverlayParams, ResolveResult,
    StockWeapon,
};

thread_local! {
    pub(crate) static PARKED_LUA: RefCell<Option<Lua>> = const { RefCell::new(None) };
}

pub fn park_lua(lua: Lua) {
    PARKED_LUA.with(|slot| {
        *slot.borrow_mut() = Some(lua);
    });
}

pub struct MatchScripts {
    pub lua: Lua,
    pub resolved: ResolveResult,
    pub logs: Vec<String>,
}

pub fn run_mods(mods: &[LoadedMod]) -> Result<MatchScripts, String> {
    actor::reset();
    let lua = sandbox::new_lua()?;
    let logs = Rc::new(RefCell::new(Vec::new()));
    let claims = Rc::new(RefCell::new(Vec::new()));

    for pack in mods {
        let ctx = BindContext {
            mod_id: pack.manifest.id.clone(),
            root: pack.root.clone(),
            allowed_slots: pack.manifest.weapons.allowed_slots()?.into_iter().collect(),
            allowed_register: pack.manifest.weapons.register.iter().cloned().collect(),
            claims: Rc::clone(&claims),
            logs: Rc::clone(&logs),
        };
        api::install(&lua, &ctx)?;
        let entry = pack.root.join(&pack.manifest.entry);
        let src = std::fs::read_to_string(&entry)
            .map_err(|e| format!("{}: cannot read {}: {e}", pack.manifest.id, entry.display()))?;
        sandbox::run_script(&lua, &src, &pack.manifest.entry)
            .map_err(|e| format!("{}: {e}", pack.manifest.id))?;
    }

    let resolved = resolve_claims(claims.borrow().clone());
    let log_lines = logs.borrow().clone();
    Ok(MatchScripts {
        lua,
        resolved,
        logs: log_lines,
    })
}

pub fn parse_overlay_table(
    mod_id: &str,
    slot: StockWeapon,
    logical_id: Option<String>,
    name: Option<String>,
    damage: Option<i32>,
    bounce_pct: Option<i32>,
    gravity_pct: Option<i32>,
    wind_pct: Option<i32>,
    sprite_id: Option<i32>,
    sprite_sheet: Option<SpriteStrip>,
    copy_from: Option<StockWeapon>,
    actor: Option<String>,
) -> OverlayClaim {
    OverlayClaim {
        mod_id: mod_id.to_string(),
        logical_id,
        slot,
        params: OverlayParams {
            name,
            damage,
            bounce_pct,
            gravity_pct,
            wind_pct,
            sprite_id,
            sprite_sheet,
            copy_from,
            actor,
            ..OverlayParams::default()
        },
    }
}

pub fn slot_from_based_on(based_on: &str) -> Result<StockWeapon, String> {
    parse_stock_weapon(based_on)
}

pub fn mods_root_from(base: &Path) -> std::path::PathBuf {
    base.join("Mods")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use weapons::{parse_manifest, LoadedMod};

    static HOST: Mutex<()> = Mutex::new(());

    #[test]
    fn land_trace_returns_a_hit_table_or_nil() {
        let _guard = lock_host();
        crate::bind_actor_native(crate::ActorNative {
            land_trace: |_, _, x, y| (x > 0).then_some((x, y)),
            ..Default::default()
        });
        let lua = Lua::new();
        let wa = lua.create_table().unwrap();
        let actors = lua.create_table().unwrap();
        spawn::bind_world(&lua, &actors, &wa).unwrap();
        lua.globals().set("wa", wa).unwrap();
        let result = lua
            .load(
                r#"
            local hit = wa.land.trace(0, 0, 300 * 0x10000, 75 * 0x10000)
            assert(type(hit) == "table" and hit.hit)
            assert(hit.x == 300 * 0x10000 and hit.y == 75 * 0x10000)
            assert(wa.land.trace(0, 0, 0, 0) == nil)
        "#,
            )
            .exec();
        crate::bind_actor_native(crate::ActorNative::default());
        result.unwrap();
    }

    #[test]
    fn sentry_placement_firing_and_reload_are_deterministic() {
        let lua = Lua::new();
        let test: mlua::Function = lua
            .load(include_str!("../tests/fixtures/test_sentry.lua"))
            .eval()
            .unwrap();
        let pack = lua
            .load(include_str!(
                "../../../mods/examples/sentry_gun/weapons.lua"
            ))
            .into_function()
            .unwrap();
        test.call::<()>(pack).unwrap();
    }

    #[test]
    fn sentry_pack_is_gpu_rendered_with_slot_scoped_worm_poses() {
        let _guard = lock_host();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/examples/sentry_gun");
        let pack = LoadedMod {
            manifest: parse_manifest(&std::fs::read_to_string(root.join("mod.toml")).unwrap())
                .unwrap(),
            root,
        };
        let scripts = run_mods(&[pack]).unwrap();
        assert_eq!(scripts.resolved.applied.len(), 1);
        let claim = &scripts.resolved.applied[0];
        assert_eq!(claim.slot, StockWeapon::Bazooka);
        assert_eq!(claim.params.copy_from, Some(StockWeapon::Mine));
        assert_ne!(claim.params.on_fire_id, 0);
        // The headline of the GPU renderer: the turret's 6 teams x 3 poses x
        // 120 baked angles collapse to two sheets, rotated and tinted at draw
        // time. 10 sheets total, none of them consuming a WA sprite slot.
        assert_eq!(claim.params.extra_sheets.len(), 11);
        assert_eq!(claim.params.gpu_prefix.as_deref(), Some("sprites/turret_"));
        assert_eq!(claim.params.depth, Some(0x190000));
        assert_eq!(
            claim.params.depth_prefix.as_deref(),
            Some("sprites/turret_")
        );
        assert_eq!(claim.params.worm_sprites.len(), 6);
        let turret: Vec<&String> = claim
            .params
            .extra_sheets
            .iter()
            .map(|(p, _)| p)
            .filter(|p| p.contains("turret_"))
            .collect();
        assert_eq!(
            turret.len(),
            3,
            "base, gun and a separate additive flare: {turret:?}"
        );
        for (path, sheet) in &claim.params.extra_sheets {
            if path.contains("turret_base") {
                assert_eq!(sheet.frames.len(), 1, "the tripod never animates");
            } else if path.contains("turret_flare") {
                // Empty and lit: the flare is always present and selects
                // nothing while idle.
                assert_eq!(sheet.frames.len(), 2);
            } else if path.contains("turret_gun") {
                // Idle plus the two firing frames; every angle is a rotation.
                assert_eq!(sheet.frames.len(), 3);
            } else if path.contains("equip") {
                assert_eq!(sheet.frames.len(), 10);
                assert!(!sheet.looping);
            }
        }
    }

    /// The colourtest pack exists to make the renderer's new capabilities
    /// visible, so the properties that carry that meaning are asserted here
    /// rather than left to a screenshot. Each one is something the palette
    /// path could not represent.
    #[test]
    fn colourtest_pack_is_truecolour_and_costs_no_sprite_slots() {
        let _guard = lock_host();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/examples/colourtest");
        let pack = LoadedMod {
            manifest: parse_manifest(&std::fs::read_to_string(root.join("mod.toml")).unwrap())
                .unwrap(),
            root,
        };
        let scripts = run_mods(&[pack]).unwrap();
        assert_eq!(scripts.resolved.applied.len(), 1);
        let claim = &scripts.resolved.applied[0];
        // Sits in the mortar slot, throws like a grenade.
        assert_eq!(claim.slot, StockWeapon::Mortar);
        assert_eq!(claim.params.copy_from, Some(StockWeapon::Grenade));
        assert_ne!(claim.params.on_fire_id, 0);

        // Every sheet goes to the GPU atlas, so the pack spends none of the
        // 223 WA sprite slots that all packs share.
        assert_eq!(claim.params.gpu_prefix.as_deref(), Some("sprites/"));
        assert_eq!(claim.params.extra_sheets.len(), 3);
        for (path, sheet) in &claim.params.extra_sheets {
            assert!(
                path.contains("sprites/"),
                "{path} would miss gpu_prefix and take a real slot"
            );
            assert_eq!(sheet.frames.len(), 1, "{path}: motion is angle/scale/tint");
        }

        let sheet = |name: &str| {
            &claim
                .params
                .extra_sheets
                .iter()
                .find(|(p, _)| p.contains(name))
                .unwrap_or_else(|| panic!("{name} missing"))
                .1
                .frames[0]
        };

        // More colours in one 32x32 sprite than an indexed palette has
        // entries. This is the claim the pack is named for.
        let prism = sheet("prism");
        let colours: std::collections::HashSet<[u8; 3]> = prism
            .rgba
            .chunks_exact(4)
            .filter(|p| p[3] > 0)
            .map(|p| [p[0], p[1], p[2]])
            .collect();
        assert!(
            colours.len() > 256,
            "prism has {} distinct colours; the point is that it exceeds a \
             256-entry palette",
            colours.len()
        );

        // Partial alpha survives. The palette path thresholded it away --
        // below 16 dropped, everything else forced to 255 -- which turns
        // every one of these ramps into a hard-edged disc.
        for name in ["halo", "spark"] {
            let partial = sheet(name)
                .rgba
                .chunks_exact(4)
                .filter(|p| p[3] > 0 && p[3] < 255)
                .count();
            assert!(
                partial > 64,
                "{name} has only {partial} partially transparent pixels; its \
                 additive falloff would read as a hard circle"
            );
        }
    }

    fn lock_host() -> std::sync::MutexGuard<'static, ()> {
        HOST.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn pack(dir: &std::path::Path, _id: &str, toml: &str, lua: &str) -> LoadedMod {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("mod.toml"), toml).unwrap();
        std::fs::write(dir.join("weapons.lua"), lua).unwrap();
        LoadedMod {
            manifest: parse_manifest(toml).unwrap(),
            root: dir.to_path_buf(),
        }
    }

    #[test]
    fn two_bazooka_overlays_first_wins() {
        let root = std::env::temp_dir().join("wklua-lua-host-conflict");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let a = pack(
            &root.join("a"),
            "a.ice",
            r#"
id = "a.ice"
api_version = 1
entry = "weapons.lua"
[weapons]
register = ["a.ice.ice_bazooka"]
replace = ["bazooka"]
"#,
            r#"
wa.weapons.register({
  id = "a.ice.ice_bazooka",
  based_on = "bazooka",
  name = "Ice Bazooka",
  params = { damage = 45 },
})
"#,
        );
        let b = pack(
            &root.join("b"),
            "b.nuke",
            r#"
id = "b.nuke"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["bazooka", "grenade"]
"#,
            r#"
wa.weapons.replace({ weapon = "bazooka", params = { damage = 1 } })
wa.weapons.replace({ weapon = "grenade", params = { damage = 9 } })
"#,
        );
        let scripts = run_mods(&[a, b]).unwrap();
        assert_eq!(scripts.resolved.applied.len(), 2);
        assert_eq!(scripts.resolved.applied[0].mod_id, "a.ice");
        assert_eq!(scripts.resolved.applied[0].params.damage, Some(45));
        assert_eq!(scripts.resolved.applied[1].mod_id, "b.nuke");
        assert_eq!(scripts.resolved.skipped.len(), 1);
        assert_eq!(scripts.resolved.skipped[0].claim.mod_id, "b.nuke");
        assert_eq!(scripts.resolved.skipped[0].winner_mod, "a.ice");
    }

    fn sample_motion() -> game::entity::Motion {
        game::entity::Motion {
            id: 1,
            class_type: 0,
            damp: 0x10000,
            wind_scale: 0,
            gravity_scale: 0x10000,
            moved: 0,
            x: 0,
            y: 0,
            vx: 10,
            vy: 20,
            cm_gravity: 0x10000,
            cm_wind: 0,
        }
    }

    #[test]
    fn gravity_hooks_mutate_scales_and_velocity() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-gravity");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("g"),
            "g.bounce",
            r#"
id = "g.bounce"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["grenade"]
"#,
            r#"
wa.on.gravity_before(function(p)
  p.wind_scale = 0x10000
end)
wa.on.gravity_after(function(p)
  p.vx = p.vx + 1
end)
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        park_lua(scripts.lua);
        let before = gravity_before(sample_motion()).unwrap();
        assert_eq!(before.wind_scale, 0x10000);
        let after = gravity_after(sample_motion()).unwrap();
        assert_eq!(after.vx, 11);
    }

    #[test]
    fn worm_message_eat_is_opt_in() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-worm");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("w"),
            "w.obs",
            r#"
id = "w.obs"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = []
"#,
            r#"
wa.on.worm_message(function(_w, msg)
  if msg == wa.msg.JUMP then
    return "eat"
  end
end)
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        park_lua(scripts.lua);
        assert!(worm_message(1, 0, 0x24).unwrap());
        assert!(!worm_message(1, 0, 0x34).unwrap());
    }

    #[test]
    fn hurt_pre_scales_and_can_eat() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-hurt");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("h"),
            "h.hurt",
            r#"
id = "h.hurt"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = []
"#,
            r#"
wa.on.hurt(function(hit)
  if hit.phase == "pre" then
    hit.amount = 4
    if hit.kind == "blast" then
      return "eat"
    end
  end
end)
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        park_lua(scripts.lua);
        let (eat, amt) = hurt(Hurt {
            worm: 1,
            from: 0,
            by: Some(0),
            kind: "blast",
            amount: 50,
            lost: 0,
            hp: 100,
            phase: "pre",
        })
        .unwrap();
        assert!(eat);
        assert_eq!(amt, 4);
        let (eat_post, _) = hurt(Hurt {
            worm: 1,
            from: 0,
            by: Some(0),
            kind: "blast",
            amount: 50,
            lost: 12,
            hp: 88,
            phase: "post",
        })
        .unwrap();
        assert!(!eat_post);
    }

    #[test]
    fn actors_kind_is_namespaced() {
        let root = std::env::temp_dir().join("wklua-lua-host-actors");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("a"),
            "a.pack",
            r#"
id = "a.pack"
api_version = 1
entry = "weapons.lua"
[actors]
register = ["a.pack.mole"]
"#,
            r#"
wa.log(wa.actors.kind("mole"))
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        assert!(scripts.logs.iter().any(|l| l.contains("a.pack.mole")));
    }

    #[test]
    fn png_strip_loads_from_pack() {
        let root = std::env::temp_dir().join("wklua-lua-host-circle");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("c"),
            "c.circle",
            r#"
id = "c.circle"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["bazooka"]
"#,
            r#"
wa.weapons.replace({
  weapon = "bazooka",
  name = "Circle Bazooka",
  params = {
    sprite = "sprites/ball.png",
    sprite_columns = 8,
    gravity_pct = 25,
    wind_pct = 200,
    bounce_pct = 100,
  },
})
"#,
        );
        let png = include_bytes!("../../../mods/examples/circle_bazooka/sprites/ball.png");
        std::fs::create_dir_all(pack.root.join("sprites")).unwrap();
        std::fs::write(pack.root.join("sprites/ball.png"), png).unwrap();
        let scripts = run_mods(&[pack]).unwrap();
        assert_eq!(scripts.resolved.applied.len(), 1);
        let p = &scripts.resolved.applied[0].params;
        let sheet = p.sprite_sheet.as_ref().expect("png strip");
        assert_eq!(sheet.frames.len(), 8);
        assert_eq!(p.gravity_pct, Some(25));
        assert_eq!(p.wind_pct, Some(200));
        assert_eq!(p.bounce_pct, Some(100));
    }

    #[test]
    fn drop_actor_on_grenade_slot() {
        let root = std::env::temp_dir().join("wklua-lua-host-drop-actor");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("g"),
            "g.hormones",
            r#"
id = "g.hormones"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["grenade"]
"#,
            r#"
wa.weapons.replace({
  weapon = "grenade",
  name = "Growth Hormones",
  is_weapon = true,
  on_select = { cursor = true },
  on_click = {
    spawn = {
      loop = true,
      from = "sky",
      origin = "bottom",
      gravity = true,
      collide_terrain = true,
      damage_terrain = true,
      collide_worms = true,
      damage_worms = true,
      persist = true,
      drown = true,
      explode = { damage = 50, id = 100, below = true },
    },
  },
})
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        assert_eq!(scripts.resolved.applied.len(), 1);
        let c = &scripts.resolved.applied[0];
        assert_eq!(c.slot, StockWeapon::Grenade);
        assert_eq!(c.params.name.as_deref(), Some("Growth Hormones"));
        assert!(c.params.is_weapon);
        assert!(c.params.cursor_on_select);
        let spawn = c.params.spawn.as_ref().expect("spawn");
        assert!(spawn.loop_anim);
        assert!(spawn.collide_terrain);
        assert!(spawn.collide_worms);
        assert!(spawn.damage_terrain);
        assert!(spawn.damage_worms);
        assert!(spawn.persist);
        assert!(spawn.drown);
        assert_eq!(spawn.explode.unwrap().damage, 50);
        assert!(spawn.explode.unwrap().below);
        assert!(c.params.copy_from.is_none());
    }

    #[test]
    fn actor_brain_explodes_on_land() {
        let _host = lock_host();
        use game::entity::write_i32;
        use game::offsets::{
            ACTOR_BOOM_ID, ACTOR_DAMAGE, ACTOR_DEAD, ACTOR_MAGIC, ENT_POS_X, LUA_ACTOR_MAGIC,
            LUA_ACTOR_SIZE,
        };
        use std::cell::Cell;

        thread_local! {
            static BOOM: Cell<Option<(u32, i32, i32, bool)>> = const { Cell::new(None) };
            static DEAD: Cell<bool> = const { Cell::new(false) };
        }

        let root = std::env::temp_dir().join("wklua-lua-host-actor-brain");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("g"),
            "g.hormones",
            r#"
id = "g.hormones"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["grenade"]
"#,
            r#"
wa.weapons.replace({
  weapon = "grenade",
  on_click = {
    spawn = {
      explode = { damage = 50, id = 100 },
      on = {
        frame = function(a, ev)
          if ev.land then
            a:explode({ at = "below" })
          end
          if a:in_water() then
            a:despawn()
          end
        end,
      },
    },
  },
})
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        let spawn = scripts.resolved.applied[0]
            .params
            .spawn
            .as_ref()
            .expect("spawn");
        assert!(spawn.brain_id != 0);
        assert!(!spawn.gravity);
        assert!(!spawn.loop_anim);
        assert!(!spawn.collide_terrain);
        park_lua(scripts.lua);
        crate::bind_actor_native(crate::ActorNative {
            explode: |h, d, id, below| BOOM.with(|c| c.set(Some((h, d, id, below)))),
            despawn: |_| DEAD.with(|c| c.set(true)),
            in_water: |_| false,
            ..Default::default()
        });
        let mut body = vec![0u8; LUA_ACTOR_SIZE];
        let handle = body.as_mut_ptr() as u32;
        unsafe {
            write_i32(body.as_mut_ptr(), ACTOR_MAGIC, LUA_ACTOR_MAGIC as i32);
            write_i32(body.as_mut_ptr(), ACTOR_DEAD, 0);
            write_i32(body.as_mut_ptr(), ACTOR_DAMAGE, 50);
            write_i32(body.as_mut_ptr(), ACTOR_BOOM_ID, 100);
            write_i32(body.as_mut_ptr(), ENT_POS_X, 0x10_0000);
        }
        crate::actor_frame(
            handle,
            spawn.brain_id,
            crate::ActorEvent {
                land: true,
                left_ground: false,
                hit_worm: false,
                in_water: false,
                damage: 0,
            },
        )
        .unwrap();
        let boom = BOOM.with(|c| c.get()).expect("explode");
        assert_eq!(boom.0, handle);
        assert_eq!(boom.1, 50);
        assert_eq!(boom.2, 100);
        assert!(boom.3);
        assert!(!DEAD.with(|c| c.get()));
    }

    #[test]
    fn worm_lockstep_message_can_write_pos() {
        let _host = lock_host();
        use game::entity::{read_i32, write_i32};
        use game::offsets::{ENT_POS_X, ENT_POS_Y};

        let root = std::env::temp_dir().join("wklua-lua-host-worm-pos");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("w"),
            "w.tp",
            r#"
id = "w.tp"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = []
"#,
            r#"
wa.on.worm_message(function(worm, msg)
  if msg == wa.msg.JUMP then
    worm.x = 0x12340000
    worm.y = 0x00AB0000
  end
end)
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        park_lua(scripts.lua);
        let mut worm = vec![0u8; 0x100];
        unsafe {
            write_i32(worm.as_mut_ptr(), ENT_POS_X, 1);
            write_i32(worm.as_mut_ptr(), ENT_POS_Y, 2);
        }
        assert!(!worm_message(worm.as_mut_ptr() as u32, 0, 0x24).unwrap());
        unsafe {
            assert_eq!(read_i32(worm.as_mut_ptr(), ENT_POS_X), 0x1234_0000);
            assert_eq!(read_i32(worm.as_mut_ptr(), ENT_POS_Y), 0x00AB_0000);
        }
    }

    #[test]
    fn on_fire_spawn_table_is_lua_projectile() {
        let root = std::env::temp_dir().join("wklua-lua-host-on-fire");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("i"),
            "i.ice",
            r#"
id = "i.ice"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["bazooka"]
"#,
            r#"
local function brain(a)
  a:gravity()
end
wa.weapons.replace({
  weapon = "bazooka",
  on_fire = {
    spawn = {
      sprite = "bazooka",
      origin = "center",
      on = brain,
    },
  },
})
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        let p = &scripts.resolved.applied[0].params;
        assert!(p.uses_lua_projectile());
        assert!(!p.uses_click_actor());
        let spawn = p.fire_spawn.as_ref().expect("fire_spawn");
        assert!(spawn.brain_id != 0);
        assert!(!spawn.gravity);
    }

    #[test]
    fn on_click_function_owns_spawn() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-on-click-fn");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("g"),
            "g.hormones",
            r#"
id = "g.hormones"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["grenade"]
"#,
            r#"
wa.weapons.replace({
  weapon = "grenade",
  sprite = "grenade",
  on_select = { cursor = true },
  on_click = function(click)
    wa.actors.spawn({
      sprite = "grenade",
      x = click.x,
      y = click.sky,
      on = function(a) a:gravity() end,
    })
  end,
})
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        let p = &scripts.resolved.applied[0].params;
        assert!(p.uses_click_actor());
        assert!(p.on_click_id != 0);
        assert!(p.spawn.is_none());
        park_lua(scripts.lua);
        use std::cell::Cell;
        thread_local! {
            static OWNER: Cell<u32> = const { Cell::new(0) };
        }
        crate::bind_actor_native(crate::ActorNative {
            spawn: |args| {
                OWNER.with(|c| c.set(args.owner));
                Ok(0xB0)
            },
            ..Default::default()
        });
        crate::weapon_click(p.on_click_id, 0x11, 5, 1, 2, -0x00D0_0000).unwrap();
        assert_eq!(OWNER.with(|c| c.get()), 0x11);
    }

    #[test]
    fn on_select_function_does_not_use_strike() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-on-select-fn");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("t"),
            "t.toss",
            r#"
id = "t.toss"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["battle_axe"]
"#,
            r#"
wa.weapons.replace({
  weapon = "battle_axe",
  on_select = function(sel)
    wa.actors.spawn({
      sprite = "cursor",
      x = sel.x,
      y = sel.y,
      owner = sel.worm,
      on = function(a) a.x = a.x end,
    })
  end,
})
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        let p = &scripts.resolved.applied[0].params;
        assert!(!p.uses_click_actor());
        assert!(p.on_select_id != 0);
        assert!(!p.cursor_on_select);
        park_lua(scripts.lua);
        use std::cell::Cell;
        thread_local! {
            static OWNER: Cell<u32> = const { Cell::new(0) };
        }
        crate::bind_actor_native(crate::ActorNative {
            spawn: |args| {
                OWNER.with(|c| c.set(args.owner));
                Ok(0xC0)
            },
            ..Default::default()
        });
        crate::weapon_select(p.on_select_id, 0x22, 9).unwrap();
        assert_eq!(OWNER.with(|c| c.get()), 0x22);
    }

    #[test]
    fn actors_spawn_from_lua_calls_native() {
        let _host = lock_host();
        use std::cell::Cell;

        thread_local! {
            static SPAWNED: Cell<u32> = const { Cell::new(0) };
        }

        let root = std::env::temp_dir().join("wklua-lua-host-actors-spawn");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("s"),
            "s.spawn",
            r#"
id = "s.spawn"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["grenade"]
"#,
            r#"
wa.weapons.replace({
  weapon = "grenade",
  on_click = {
    spawn = {
      on = function(a)
        a:spawn({ x = a.x, y = a.y, vx = 0, vy = 0x10000 })
      end,
    },
  },
})
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        let spawn = scripts.resolved.applied[0]
            .params
            .spawn
            .as_ref()
            .expect("spawn");
        park_lua(scripts.lua);
        crate::bind_actor_native(crate::ActorNative {
            spawn: |args| {
                SPAWNED.with(|c| c.set(args.owner));
                Ok(0xBEEF)
            },
            ..Default::default()
        });
        use game::entity::write_i32;
        use game::offsets::{
            ACTOR_DEAD, ACTOR_MAGIC, ACTOR_OWNER, ENT_POS_X, ENT_POS_Y, LUA_ACTOR_MAGIC,
            LUA_ACTOR_SIZE,
        };
        let mut body = vec![0u8; LUA_ACTOR_SIZE];
        let handle = body.as_mut_ptr() as u32;
        unsafe {
            write_i32(body.as_mut_ptr(), ACTOR_MAGIC, LUA_ACTOR_MAGIC as i32);
            write_i32(body.as_mut_ptr(), ACTOR_DEAD, 0);
            write_i32(body.as_mut_ptr(), ACTOR_OWNER, 0x11);
            write_i32(body.as_mut_ptr(), ENT_POS_X, 3);
            write_i32(body.as_mut_ptr(), ENT_POS_Y, 4);
        }
        crate::actor_frame(handle, spawn.brain_id, crate::ActorEvent::default()).unwrap();
        assert_eq!(SPAWNED.with(|c| c.get()), 0x11);
    }
}
