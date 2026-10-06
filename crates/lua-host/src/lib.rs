//! Lua 5.4 host. One interpreter per match, created at gfx InitGraphic
//! (`SetLayerVisibility(3, 1)`), before InitWeaponTable overlays slots.
//!
//! `wa.*` is a dispatcher: named ids, first owner wins. Scripts do not
//! override stock C++ methods. See docs/architecture.md.

mod actor;
mod grants;
mod api;
mod bundles;
mod dispatch;
mod modules;
mod perf;
mod sandbox;
mod sim_context;
mod spawn;

pub use bundles::ComponentPlan;
pub use grants::{bind_grant_native, queue_weapon_grant, flush_weapon_grants, GrantNative, GrantOrigin, GrantItem, WeaponGrant, WeaponVisual};
pub use perf::{perf_flags, take_lua_breakdown, take_lua_spikes, take_lua_time, LuaBreakdown, PerfFlags, Spike};
pub use actor::{
    gfx_field, actor_frame, actor_freed, actor_spawned, bind_actor_native, weapon_click,
    weapon_emit, weapon_fire, weapon_select, ActorEvent, ActorNative, AmmoChange, FireArgs,
    LaunchArgs, SpawnArgs, FIRE_DEFAULTS, ShapeHit, ShapeQuery, SHAPE_CRATES, SHAPE_GRAVES,
    SHAPE_MINES, SHAPE_OIL, SHAPE_TERRAIN, SHAPE_WORMS, actor_hit, actor_takes_hits, ActorHit,
    StampBuried, CrateArgs, weapon_air_fire, WarpArgs,
};
pub use dispatch::{
    body_move, body_move_wanted, contact, Contact,
    death, drawing, gravity_after, gravity_before, hurt, missile_frame, run_draw, run_hud,
    turn_cleanup, turn_end, turn_retreat, turn_start, worm_message,
    Death, DrawQuad, Hurt,
};

use crate::api::BindContext;
use mlua::Lua;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use weapons::{
    LoadedMod, OverlayClaim, OverlayParams, ResolveResult, StockWeapon, parse_stock_weapon,
    resolve_claims,
};
use gfx::SpriteStrip;

thread_local! {
    pub(crate) static PARKED_LUA: RefCell<Option<Lua>> = const { RefCell::new(None) };
}

pub fn park_lua(lua: Lua) {
    dispatch::body_move_rebuild(&lua);
    dispatch::worm_message_rebuild(&lua);
    dispatch::fast_rebuild(&lua);
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
    run_match(mods, None)
}

/// [`run_mods`], then the selected components of `plan`, if any.
pub fn run_match(mods: &[LoadedMod], plan: Option<&ComponentPlan>) -> Result<MatchScripts, String> {
    actor::reset();
    grants::reset();
    perf::reset();
    let lua = sandbox::new_lua()?;
    let logs = Rc::new(RefCell::new(Vec::new()));
    let claims = Rc::new(RefCell::new(Vec::new()));

    for pack in mods {
        let ctx = BindContext {
            mod_id: pack.manifest.id.clone(),
            root: pack.root.clone(),
            allowed_slots: pack
                .manifest
                .weapons
                .allowed_slots()?
                .into_iter()
                .collect(),
            allowed_register: pack.manifest.weapons.register.iter().cloned().collect(),
            claims: Rc::clone(&claims),
            logs: Rc::clone(&logs),
            debug_worm_state: pack.manifest.debug_worm_state,
        };
        api::install(&lua, &ctx)?;
        let entry = pack.root.join(&pack.manifest.entry);
        let src = std::fs::read_to_string(&entry)
            .map_err(|e| format!("{}: cannot read {}: {e}", pack.manifest.id, entry.display()))?;
        modules::run_entry(&lua, &pack.manifest.id, &pack.root, &pack.manifest.entry, &src)
            .map_err(|e| format!("{}: {e}", pack.manifest.id))?;
    }

    if let Some(plan) = plan {
        bundles::run(&lua, plan, &claims, &logs)?;
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
    use weapons::{LoadedMod, parse_manifest};

    static HOST: Mutex<()> = Mutex::new(());

    /// `.id` must be an identity, never a handle. Accepting a number where an
    /// actor belongs used to be survivable only because ids WERE pointers;
    /// now a number there would be dereferenced as an address.
    #[test]
    fn a_number_is_refused_where_an_actor_or_worm_belongs() {
        let _guard = lock_host();
        actor::bind_actor_native(actor::ActorNative::default());
        let lua = sandbox::new_lua().unwrap();
        let ctx = test_ctx();
        api::install(&lua, &ctx).unwrap();
        let err = sim_context::with_sim(|| {
            lua.load("return wa.actors.spawn({ sprite = 'grenade', owner = 12345 })")
                .eval::<mlua::Value>()
        })
        .expect_err("a raw number must not be taken for an entity");
        let text = err.to_string();
        assert!(
            text.contains("not a handle") || text.contains("not a number"),
            "the error should explain that an id is not a handle: {text}"
        );
    }

    /// A minimal bind context: these tests exercise `wa.*` directly rather
    /// than loading a pack, so nothing here needs to be a real mod.
    fn test_ctx() -> BindContext {
        BindContext {
            mod_id: "test.random".into(),
            root: std::env::temp_dir(),
            allowed_slots: Default::default(),
            allowed_register: Default::default(),
            claims: Rc::new(RefCell::new(Vec::new())),
            logs: Rc::new(RefCell::new(Vec::new())),
            debug_worm_state: false,
        }
    }

    /// `wa.random` is the only randomness a pack may have, so its range
    /// arithmetic is worth pinning down: it is pure logic, and getting it
    /// wrong would be a gameplay bug that looks like bad luck.
    #[test]
    fn wa_random_maps_a_draw_onto_the_requested_range() {
        let _guard = lock_host();
        // A fixed draw, so the mapping is the only variable.
        const DRAW: u32 = 0x1234_5678;
        actor::bind_actor_native(actor::ActorNative {
            random: || Some(DRAW),
            ..Default::default()
        });
        let lua = sandbox::new_lua().unwrap();
        let ctx = test_ctx();
        api::install(&lua, &ctx).unwrap();

        let raw: i64 = lua.load("return wa.random()").eval().unwrap();
        assert_eq!(raw, DRAW as i64, "no args gives the raw draw");

        // 1..n, matching math.random's form.
        let n: i64 = lua.load("return wa.random(6)").eval().unwrap();
        assert!((1..=6).contains(&n), "got {n}, want 1..=6");
        assert_eq!(n, 1 + (DRAW as i64 % 6));

        // a..b inclusive at both ends.
        let v: i64 = lua.load("return wa.random(10, 20)").eval().unwrap();
        assert!((10..=20).contains(&v), "got {v}, want 10..=20");

        // A single-value range is legal and must return that value.
        let one: i64 = lua.load("return wa.random(7, 7)").eval().unwrap();
        assert_eq!(one, 7);

        // Negative ranges are ordinary (offsets, deltas).
        let neg: i64 = lua.load("return wa.random(-3, -1)").eval().unwrap();
        assert!((-3..=-1).contains(&neg), "got {neg}");

        // An inverted range is a pack bug; say so rather than returning
        // something arbitrary.
        assert!(lua.load("return wa.random(5, 2)").eval::<i64>().is_err());
    }

    /// `wa.math` is WA's own sine table and a portable atan, with
    /// `math.sin`'s signature: radians in, the same bits on every machine out.
    #[test]
    fn wa_math_is_was_sine_table_and_a_portable_atan() {
        let _guard = lock_host();
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        lua.load(r#"
            local m = wa.math
            assert(m.sin(0) == 0 and m.cos(0) == 1)
            assert(m.sin(math.pi / 2) == 1 and m.sin(-math.pi / 2) == -1 and m.cos(math.pi) == -1)
            assert(m.sin(math.pi / 4) == 46340 / 65536, "WA's table, not the C runtime's")
            for i = -50, 50 do
              local x = i * 0.137
              assert(math.abs(m.sin(x) - math.sin(x)) < 1e-4, x)
              assert(math.abs(m.cos(x) - math.cos(x)) < 1e-4, x)
              assert(m.sin(x) * 65536 == math.floor(m.sin(x) * 65536), "a whole 16.16 value")
              assert(math.abs(m.atan(x, 1.5) - math.atan(x, 1.5)) < 1e-14, x)
            end
            assert(m.atan(1) == m.atan(1, 1), "x defaults to 1, as math.atan")
            assert(not pcall(m.sin, 0 / 0) and not pcall(m.cos, math.huge) and not pcall(m.atan, 1, -math.huge))
        "#).exec().unwrap();
    }

    /// With no RNG available the call must fail loudly. Returning nil would
    /// surface as an error somewhere later and further from the cause, and
    /// substituting a host RNG would desync every peer.
    #[test]
    fn wa_random_errors_rather_than_substituting_when_unavailable() {
        let _guard = lock_host();
        actor::bind_actor_native(actor::ActorNative::default());
        let lua = sandbox::new_lua().unwrap();
        let ctx = test_ctx();
        api::install(&lua, &ctx).unwrap();
        let err = lua
            .load("return wa.random(6)")
            .eval::<i64>()
            .expect_err("must not silently succeed");
        assert!(
            err.to_string().contains("desync"),
            "the error should say why it cannot be substituted: {err}"
        );
    }

    /// `wa.random` exists, but Lua's own RNG must stay unavailable: it is
    /// seeded per process, so two peers would diverge on the first call.
    #[test]
    fn lua_own_rng_stays_nil() {
        let _guard = lock_host();
        let lua = sandbox::new_lua().unwrap();
        for expr in ["math.random", "math.randomseed"] {
            let is_nil: bool = lua.load(format!("return {expr} == nil")).eval().unwrap();
            assert!(is_nil, "{expr} must stay nil");
        }
    }

    /// W1 item 7: `coroutine` stays out of the sandbox. Suspend/resume points
    /// are driven by whoever yields rather than by the sim, so a pack using
    /// them would desync; assert the strip so a future edit to the list is
    /// caught rather than silently reopened.
    #[test]
    fn coroutine_stays_nil() {
        let _guard = lock_host();
        let lua = sandbox::new_lua().unwrap();
        let is_nil: bool = lua.load("return coroutine == nil").eval().unwrap();
        assert!(is_nil, "coroutine must stay nil");
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

    pub(crate) fn lock_host() -> std::sync::MutexGuard<'static, ()> {
        HOST.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn collect_viewer_loads_as_a_utility_without_weapon_claims() {
        let _guard = lock_host();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/examples/collect_viewer");
        let pack = LoadedMod {
            manifest: parse_manifest(&std::fs::read_to_string(root.join("mod.toml")).unwrap()).unwrap(),
            root,
        };
        let scripts = run_mods(&[pack]).unwrap();
        assert!(scripts.resolved.applied.is_empty());
        park_lua(scripts.lua);
        PARKED_LUA.with(|slot| slot.borrow().as_ref().unwrap().load(
            "assert(#wa._hooks.weapon_grant==1 and #wa._hooks.draw==1)"
        ).exec().unwrap());
        run_draw().unwrap();
    }

    #[test]
    fn super_drill_example_claims_only_the_drill_slot() {
        let _guard = lock_host();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/examples/super_drill");
        let pack = LoadedMod {
            manifest: parse_manifest(&std::fs::read_to_string(root.join("mod.toml")).unwrap())
                .unwrap(),
            root,
        };
        let scripts = run_mods(&[pack]).unwrap();
        assert_eq!(scripts.resolved.applied.len(), 1);
        let claim = &scripts.resolved.applied[0];
        assert_eq!(claim.slot, StockWeapon::PneumaticDrill);
        assert_eq!(claim.params.copy_from, Some(StockWeapon::PneumaticDrill));
        assert!(!claim.params.eat_fire);
        assert_eq!(claim.params.on_fire_id, 0);
    }

    #[test]
    fn gravler_strike_uses_stock_moles_from_the_donkey_slot() {
        let _guard = lock_host();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/examples/gravler_strike");
        let pack = LoadedMod {
            manifest: parse_manifest(&std::fs::read_to_string(root.join("mod.toml")).unwrap())
                .unwrap(),
            root,
        };
        let scripts = run_mods(&[pack]).unwrap();
        assert_eq!(scripts.resolved.applied.len(), 1);
        let claim = &scripts.resolved.applied[0];
        assert_eq!(claim.slot, StockWeapon::Donkey);
        assert_eq!(claim.params.copy_from, Some(StockWeapon::MoleSquadron));
        assert_eq!(
            claim.params.panel_icon,
            Some(weapons::PanelIcon::Stock(StockWeapon::MoleSquadron))
        );
        assert_eq!(claim.params.on_click_id, 0);
        assert!(claim.params.actor_spawn().is_none());
        assert_eq!(
            claim
                .params
                .mole_dig_rotation
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            vec![158, 159, 160, 161, 663],
            "the four dig frames, plus the dive frame's underwater variant"
        );
        assert_eq!(claim.params.extra_sheets.len(), 5);
        assert!(claim.params.extra_sheets.iter().all(|(_, s)| s.frames.len() == 17));
        // PX's Gravler has no underwater art, so the dive sheet sinks under
        // PX's sinking tint instead of turning back into WA's mole.
        let sheet = |key: &str| {
            &claim.params.extra_sheets.iter().find(|(p, _)| p == key).unwrap().1
        };
        let dive = sheet("sprites/gravlerdive.png");
        assert_eq!(
            sheet("sprites/gravlerdive.png#underwater"),
            &dive.multiply([90, 90, 240])
        );
    }

    #[test]
    fn mole_dig_rotation_takes_the_packs_own_underwater_frame() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-mole-underwater");
        let _ = std::fs::remove_dir_all(&root);
        let gravler = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../mods/examples/gravler_strike/sprites");
        let own = pack(
            &root,
            "t.mole",
            r#"
id = "t.mole"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["donkey"]
"#,
            r#"
wa.weapons.replace({
  weapon = "donkey",
  copy_from = "mole_squadron",
  mole_dig_rotation = { moledive = "dive.png", mole = "sink.png" },
})
"#,
        );
        std::fs::copy(gravler.join("gravlerdive.png"), root.join("dive.png")).unwrap();
        std::fs::copy(gravler.join("gravlerdiga.png"), root.join("sink.png")).unwrap();
        let scripts = run_mods(&[own]).unwrap();
        let params = &scripts.resolved.applied[0].params;
        assert_eq!(
            params.mole_dig_rotation,
            vec![(158, "dive.png".to_string()), (663, "sink.png".to_string())],
            "a pack's own underwater frame is used as is"
        );
        assert!(!params.extra_sheets.iter().any(|(p, _)| p.ends_with("#underwater")));
    }

    #[test]
    fn sprite_overrides_take_stock_names_and_ids() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-sprite-overrides");
        let _ = std::fs::remove_dir_all(&root);
        let manifest = r#"
id = "t.over"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["mortar"]
"#;
        let own = pack(
            &root,
            "t.over",
            manifest,
            r#"
wa.weapons.replace({
  weapon = "mortar",
  sprite_overrides = { wsvbd = "trail.png", [466] = "trail.png" },
})
"#,
        );
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../mods/examples/gravler_strike/sprites/gravlerdive.png"),
            root.join("trail.png"),
        )
        .unwrap();
        let scripts = run_mods(&[own]).unwrap();
        let params = &scripts.resolved.applied[0].params;
        assert_eq!(
            params.sprite_overrides,
            vec![(250, "trail.png".to_string()), (466, "trail.png".to_string())]
        );
        assert_eq!(params.extra_sheets.iter().filter(|(p, _)| p == "trail.png").count(), 1);

        let bad = pack(
            &root,
            "t.over",
            manifest,
            r#"wa.weapons.replace({ weapon = "mortar", sprite_overrides = { [900] = "trail.png" } })"#,
        );
        let err = run_mods(&[bad]).err().expect("an id past the stock sprites is refused");
        assert!(err.contains("sprite_overrides keys are stock sprite names or ids"), "{err}");
    }

    #[test]
    fn mole_dig_rotation_refuses_other_sprites() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-mole-dig-rotation");
        let _ = std::fs::remove_dir_all(&root);
        let bad = pack(
            &root,
            "t.mole",
            r#"
id = "t.mole"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["donkey"]
"#,
            r#"
wa.weapons.replace({
  weapon = "donkey",
  copy_from = "mole_squadron",
  mole_dig_rotation = { bazooka = "sprites/x.png" },
})
"#,
        );
        let err = run_mods(&[bad]).err().expect("a non-mole sprite is refused");
        assert!(err.contains("mole_dig_rotation keys are moledive"), "{err}");
    }

    /// The handoff verbs are lockstep writes: refused outside a simulation
    /// callback, and inside one they reach the engine with the parsed options.
    #[test]
    fn handoff_verbs_reach_native_only_during_simulation() {
        let _guard = lock_host();
        thread_local! {
            static CALLS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        actor::bind_actor_native(actor::ActorNative {
            list_worms: |_| vec![0x1234],
            take: |h, hide, freeze| CALLS.with(|c| c.borrow_mut().push(format!("take {h:x} {hide} {freeze}"))),
            release: |h, vx, vy, t, suppress| CALLS.with(|c| c.borrow_mut().push(format!("release {h:x} {vx} {vy} {t} {suppress}"))),
            is_taken: |h| h == 0x1234,
            fits: |h, x, y| h == 0x1234 && x > 0 && y == 7,
            jetpack_steer: |h, on| CALLS.with(|c| c.borrow_mut().push(format!("steer {h:x} {on}"))),
            ..Default::default()
        });
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        for src in [
            "wa.worms.all()[1]:take()",
            "wa.worms.all()[1]:release(1, 2)",
            "wa.worms.all()[1]:jetpack_steer(true)",
        ] {
            assert!(lua.load(src).exec().is_err(), "{src} outside a simulation callback");
        }
        CALLS.with(|c| assert!(c.borrow().is_empty()));
        sim_context::with_sim(|| {
            lua.load(
                "local w = wa.worms.all()[1]
                 w:take()
                 w:take{ hide = true, turn = 'freeze' }
                 w:take{ turn = 'run' }
                 w:release(0x30000, -0x20000)
                 w:release()
                 w:release(1, 2, { tumble = true })
                 w:release(3, 4, { suppress_auto_parachute = true })
                 assert(w.taken == true)
                 assert(w:fits(1, 7) == true and w:fits(-1, 7) == false)
                 w:jetpack_steer(true)
                 w:jetpack_steer(false)",
            )
            .exec()
            .unwrap();
            let err = lua.load("wa.worms.all()[1]:take{ turn = 'pause' }").exec().unwrap_err();
            assert!(err.to_string().contains("\"run\" or \"freeze\""), "{err}");
        });
        CALLS.with(|c| {
            assert_eq!(
                c.borrow().as_slice(),
                [
                    "take 1234 false false",
                    "take 1234 true true",
                    "take 1234 false false",
                    "release 1234 196608 -131072 false false",
                    "release 1234 0 0 false false",
                    "release 1234 1 2 true false",
                    "release 1234 3 4 false true",
                    "steer 1234 true",
                    "steer 1234 false",
                ]
            )
        });
    }

    /// `worm:set_state` is a debugging tool a pack opts into in its mod.toml;
    /// without the flag it is refused even inside a simulation callback.
    #[test]
    fn set_state_needs_the_debug_opt_in() {
        let _guard = lock_host();
        thread_local! {
            static SET: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        actor::bind_actor_native(actor::ActorNative {
            list_worms: |_| vec![0x1234],
            set_worm_state: |_, s| SET.with(|v| v.borrow_mut().push(s)),
            ..Default::default()
        });
        let set = "wa.worms.all()[1]:set_state('standing')";
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        let err = sim_context::with_sim(|| lua.load(set).exec().unwrap_err());
        assert!(err.to_string().contains("debug_worm_state"), "{err}");
        SET.with(|v| assert!(v.borrow().is_empty()));

        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &BindContext { debug_worm_state: true, ..test_ctx() }).unwrap();
        assert!(lua.load(set).exec().is_err(), "outside a simulation callback");
        sim_context::with_sim(|| lua.load(set).exec().unwrap());
        SET.with(|v| assert_eq!(v.borrow().as_slice(), [game::offsets::WORM_STATE_STANDING]));
    }

    #[test]
    fn freeze_is_a_simulation_verb_without_debug_opt_in() {
        let _guard = lock_host();
        thread_local! {
            static FROZEN: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }
        actor::bind_actor_native(actor::ActorNative {
            list_worms: |_| vec![0x1234],
            freeze_worm: |handle| {
                FROZEN.with(|v| v.set(handle));
                true
            },
            ..Default::default()
        });
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        let call = "return wa.worms.all()[1]:freeze()";
        assert!(lua.load(call).eval::<bool>().is_err());
        assert!(sim_context::with_sim(|| lua.load(call).eval::<bool>().unwrap()));
        FROZEN.with(|v| assert_eq!(v.get(), 0x1234));
    }

    #[test]
    fn fire_spawn_reaches_native_only_during_simulation() {
        let _guard = lock_host();
        thread_local! {
            static STARTED: std::cell::RefCell<Vec<actor::FireArgs>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        actor::bind_actor_native(actor::ActorNative {
            list_worms: |_| vec![0x1234],
            spawn_fire: |a| { STARTED.with(|l| l.borrow_mut().push(*a)); Ok(7) },
            ..Default::default()
        });
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        let bare = "return wa.fire.spawn{ owner = wa.worms.all()[1], x = 0x100000, y = 0x200000 }";
        assert!(lua.load(bare).exec().is_err(), "outside a simulation callback");
        let n: u32 = sim_context::with_sim(|| lua.load(bare).eval().unwrap());
        assert_eq!(n, 7);
        sim_context::with_sim(|| {
            lua.load("wa.fire.spawn{ owner = wa.worms.all()[1], x = 1, y = 2, vx = 3, vy = 0, \
                      count = 0, duration = 6000, up_power = 0, damage = 0, \
                      damage_scale = 1.25, knock_scale = 2, tint = 0x4BE14B }")
                .exec()
                .unwrap();
            let err = |src: &str| lua.load(src).exec().unwrap_err().to_string();
            assert!(err("wa.fire.spawn{ x = 0, y = 0 }").contains("needs owner"));
            assert!(err("wa.fire.spawn{ owner = wa.worms.all()[1], x = 0 }").contains("needs y"));
            assert!(err("wa.fire.spawn{ owner = wa.worms.all()[1], x = 0, y = 0, duration = 0 }")
                .contains("duration"));
            assert!(err("wa.fire.spawn{ owner = wa.worms.all()[1], x = 0, y = 0, knock_scale = -1 }")
                .contains("knock_scale"));
            assert!(err("wa.fire.spawn{ owner = wa.worms.all()[1], x = 0, y = 0, tint = 0x1000000 }")
                .contains("tint"));
        });
        STARTED.with(|l| {
            let d = actor::FIRE_DEFAULTS;
            assert_eq!(
                l.borrow().as_slice(),
                &[
                    actor::FireArgs { owner: 0x1234, x: 0x100000, y: 0x200000, ..d },
                    actor::FireArgs {
                        owner: 0x1234, x: 1, y: 2, vx: 3, vy: 0, count: 0, duration: 6000, up_power: 0, damage: 0,
                        damage_scale: 0x1_4000, knock_scale: 0x2_0000, tint: Some(0x4B_E14B),
                    },
                ],
                "a bare spawn is WA's Petrol Bomb fire; every field can be given"
            )
        });
    }

    #[test]
    fn worms_spawn_reaches_native_only_during_simulation() {
        let _guard = lock_host();
        thread_local! {
            static MADE: std::cell::RefCell<Vec<(u32, i32, i32, String, i32)>> =
                const { std::cell::RefCell::new(Vec::new()) };
        }
        actor::bind_actor_native(actor::ActorNative {
            spawn_worm: |team, x, y, name, health| {
                MADE.with(|l| l.borrow_mut().push((team, x, y, name.to_string(), health)));
                if team == 6 { Err("team 6 already has 8 worms".into()) } else { Ok(0x3030) }
            },
            ..Default::default()
        });
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        let one = "return wa.worms.spawn{ team = 2, x = 0x100000, y = 0x200000, name = 'Reinforcement', health = 200 }";
        assert!(lua.load(one).exec().is_err(), "outside a simulation callback");
        sim_context::with_sim(|| {
            let w: mlua::AnyUserData = lua.load(one).eval().unwrap();
            assert!(w.is::<actor::WormHandle>(), "a worm as wa.worms.all() gives one");
            lua.load("wa.worms.spawn{ team = 1, x = 1, y = 2 }").exec().unwrap();
            let err = |src: &str| lua.load(src).exec().unwrap_err().to_string();
            assert!(err("wa.worms.spawn{ x = 0, y = 0 }").contains("needs team"));
            assert!(err("wa.worms.spawn{ team = 1, x = 0 }").contains("needs y"));
            assert!(err("wa.worms.spawn{ team = 1, x = 0, y = 0, health = 0 }").contains("health"));
            assert!(err("wa.worms.spawn{ team = 6, x = 0, y = 0 }").contains("already has 8"));
        });
        MADE.with(|l| assert_eq!(
            l.borrow().as_slice(),
            &[
                (2, 0x100000, 0x200000, "Reinforcement".to_string(), 200),
                (1, 1, 2, "Worm".to_string(), 100),
                (6, 0, 0, "Worm".to_string(), 100),
            ],
            "name Worm and health 100 by default"
        ));
    }

    #[test]
    fn world_trace_and_collides_pass_the_query_and_shape_the_answer() {
        let _guard = lock_host();
        thread_local! {
            static ASKED: std::cell::RefCell<Vec<actor::ShapeQuery>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        actor::bind_actor_native(actor::ActorNative {
            list_worms: |_| vec![0x1234],
            shape_trace: |q| {
                ASKED.with(|a| a.borrow_mut().push(q.clone()));
                if q.x1 == 0 {
                    return actor::ShapeHit::default();
                }
                actor::ShapeHit { hit: true, x: 10, y: 20, nx: 0.6, ny: -0.8, obj: 0, starts_solid: true, left: Some((3, 4)) }
            },
            shape_collides: |q| {
                ASKED.with(|a| a.borrow_mut().push(q.clone()));
                (q.x0 == 1).then_some(0)
            },
            ..Default::default()
        });
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        lua.load(r#"
            local h = wa.world.trace{ x0 = 1, y0 = 2, x1 = 5, y1 = 6, radius = 8,
              with = { "terrain", "worms", "oil" }, ignore = { wa.worms.all()[1] } }
            assert(h.hit and h.x == 10 and h.y == 20 and h.nx == 0.6 and h.ny == -0.8)
            assert(h.tx == 0.8 and h.ty == 0.6, "the tangent, the normal turned")
            assert(h.starts_solid and h.left_x == 3 and h.left_y == 4 and h.obj == nil)
            assert(wa.world.trace{ x0 = 1, y0 = 2, x1 = 0, y1 = 0 } == nil, "nothing touched")
            assert(wa.world.collides{ x = 1, y = 2, radius = 4 } == true)
            assert(wa.world.collides{ x = 2, y = 2 } == false)
            assert(not pcall(wa.world.trace, { x0 = 0, y0 = 0, x1 = 0 }))
            assert(not pcall(wa.world.collides, { x = 0, y = 0, radius = 65 }))
            assert(not pcall(wa.world.collides, { x = 0, y = 0, with = { "clouds" } }))
        "#).exec().unwrap();
        ASKED.with(|a| {
            let a = a.borrow();
            assert_eq!(a[0], actor::ShapeQuery {
                x0: 1, y0: 2, x1: 5, y1: 6, radius: 8,
                with: actor::SHAPE_TERRAIN | actor::SHAPE_WORMS | actor::SHAPE_OIL, ignore: vec![0x1234],
            });
            assert_eq!(a[2], actor::ShapeQuery { x0: 1, y0: 2, x1: 1, y1: 2, radius: 4, with: actor::SHAPE_TERRAIN, ignore: vec![] },
                "a point test, terrain by default");
        });
    }

    #[test]
    fn worm_hurt_carries_a_knock() {
        let _guard = lock_host();
        thread_local! {
            static HITS: std::cell::RefCell<Vec<(u32, i32, i32, i32)>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        actor::bind_actor_native(actor::ActorNative {
            list_worms: |_| vec![0x1234],
            hurt: |w, _, amount, vx, vy| HITS.with(|l| l.borrow_mut().push((w, amount, vx, vy))),
            ..Default::default()
        });
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        assert!(lua.load("wa.worms.all()[1]:hurt(5)").exec().is_err(), "outside a simulation callback");
        sim_context::with_sim(|| {
            lua.load(r#"
                local w = wa.worms.all()[1]
                w:hurt(10)
                w:hurt({ amount = 12, vx = 0x30000, vy = -0x50000 })
                w:hurt({ vx = 0x10000 })
                w:hurt({ amount = 0 })
                w:hurt(-3)
            "#).exec().unwrap();
            assert!(lua.load("wa.worms.all()[1]:hurt('x')").exec().is_err());
        });
        HITS.with(|l| assert_eq!(
            l.borrow().as_slice(),
            &[(0x1234, 10, 0, 0), (0x1234, 12, 0x30000, -0x50000), (0x1234, 0, 0x10000, 0)],
            "a number hurts in place; a table may knock, and with a knock no damage is a push"
        ));
    }

    #[test]
    fn crates_spawn_reaches_native_only_during_simulation() {
        let _guard = lock_host();
        thread_local! {
            static MADE: std::cell::RefCell<Vec<actor::CrateArgs>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        actor::bind_actor_native(actor::ActorNative {
            spawn_crate: |a| { MADE.with(|l| l.borrow_mut().push(*a)); Ok(0xC4A7E) },
            ..Default::default()
        });
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        let one = "return wa.crates.spawn{ x = 0x100000, y = -0x320000, kind = 'weapon' }";
        assert!(lua.load(one).exec().is_err(), "outside a simulation callback");
        sim_context::with_sim(|| {
            let c: mlua::AnyUserData = lua.load(one).eval().unwrap();
            assert!(c.is::<actor::WorldHandle>(), "a crate as wa.crates.all() gives one");
            lua.load("wa.crates.spawn{ x = 1, y = 2, kind = 'health', health = 60, drop = 'teleport' }; \
                      wa.crates.spawn{ x = 3, y = 4, kind = 'utility', health = 99, drop = 'none' }")
                .exec()
                .unwrap();
            let err = |src: &str| lua.load(src).exec().unwrap_err().to_string();
            assert!(err("wa.crates.spawn{ x = 0, y = 0 }").contains("kind"));
            assert!(err("wa.crates.spawn{ x = 0, kind = 'weapon' }").contains("needs y"));
            assert!(err("wa.crates.spawn{ x = 0, y = 0, kind = 'health', health = 0 }").contains("health"));
            assert!(err("wa.crates.spawn{ x = 0, y = 0, kind = 'weapon', drop = 'cannon' }").contains("drop"));
        });
        MADE.with(|l| assert_eq!(
            l.borrow().as_slice(),
            &[
                actor::CrateArgs { x: 0x100000, y: -0x320000, kind: 1, health: 0, drop: 2 },
                actor::CrateArgs { x: 1, y: 2, kind: 2, health: 60, drop: 1 },
                actor::CrateArgs { x: 3, y: 4, kind: 4, health: 0, drop: 0 },
            ],
            "WA's kinds, health for a health crate only, a parachute by default"
        ));
    }

    #[test]
    fn a_missiles_team_and_its_removal_reach_native() {
        let _guard = lock_host();
        thread_local! {
            static REMOVED: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        actor::bind_actor_native(actor::ActorNative {
            list_kind: |class, _| if class == game::offsets::CLASS_MISSILE { vec![0xA1, 0xA2] } else { Vec::new() },
            object_team: |h| if h == 0xA1 { 4 } else { 0 },
            remove_object: |h| { REMOVED.with(|l| l.borrow_mut().push(h)); true },
            ..Default::default()
        });
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        let teams: (i32, i32, Option<i32>) = lua
            .load("local m = wa.missiles.all(); return m[1].team, m[2].team, m[2].alliance")
            .eval()
            .unwrap();
        assert_eq!(teams, (4, 0, None), "a team, none, and no colour without one");
        let remove = "return wa.missiles.all()[2]:remove()";
        assert!(lua.load(remove).exec().is_err(), "outside a simulation callback");
        sim_context::with_sim(|| {
            assert!(lua.load(remove).eval::<bool>().unwrap());
        });
        REMOVED.with(|l| assert_eq!(l.borrow().as_slice(), &[0xA2]));
    }

    #[test]
    fn launch_reaches_native_only_during_simulation() {
        let _guard = lock_host();
        thread_local! {
            static LAUNCHED: std::cell::RefCell<Vec<actor::LaunchArgs>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        actor::bind_actor_native(actor::ActorNative {
            list_worms: |_| vec![0x1234],
            launch: |a| { LAUNCHED.with(|l| l.borrow_mut().push(a.clone())); Ok(0x5000) },
            ..Default::default()
        });
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        let shot = "return wa.weapons.launch{ weapon = 'bazooka', owner = wa.worms.all()[1], \
                    x = 0x100000, y = 0x200000, vx = 0x30000, vy = -0x20000, \
                    damage = 40, sprite = 54, fuse = 5000 }";
        assert!(lua.load(shot).exec().is_err(), "outside a simulation callback");
        let id: u32 = sim_context::with_sim(|| lua.load(shot).eval().unwrap());
        assert_eq!(id, 0x5000, "the missile's id");
        LAUNCHED.with(|l| {
            assert_eq!(
                l.borrow().as_slice(),
                &[actor::LaunchArgs {
                    slot: StockWeapon::Bazooka.index() as u32,
                    owner: 0x1234,
                    x: 0x100000,
                    y: 0x200000,
                    vx: 0x30000,
                    vy: -0x20000,
                    damage: Some(40),
                    sprite: Some(54),
                    fuse: Some(5000),
                    sound: None,
                    explode_into: None,
                    countdown: None,
                    explode_on_space: None,
                }]
            )
        });
        sim_context::with_sim(|| {
            let err = |src: &str| lua.load(src).exec().unwrap_err().to_string();
            assert!(err("wa.weapons.launch{ weapon = 'bazooka', x = 0, y = 0 }").contains("needs owner"));
            assert!(err("wa.weapons.launch{ weapon = 'bazooka', owner = wa.worms.all()[1], y = 0 }")
                .contains("needs x"));
            assert!(err("wa.weapons.launch{ weapon = 'nope', owner = wa.worms.all()[1], x = 0, y = 0 }")
                .contains("nope"));
            assert!(err("wa.weapons.launch{ weapon = 'mine', owner = wa.worms.all()[1], x = 0, y = 0, explode_into = 'confetti' }")
                .contains("clustlets"));
            lua.load("wa.weapons.launch{ weapon = 'mine', owner = wa.worms.all()[1], x = 0, y = 0, sound = 0, explode_into = 'clustlets' }")
                .exec()
                .unwrap();
            assert!(err("wa.weapons.launch{ weapon = 'mine', owner = wa.worms.all()[1], x = 0, y = 0, explode_on_space = 'yes' }")
                .contains("true or false"));
            lua.load("wa.weapons.launch{ weapon = 'super_banana', owner = wa.worms.all()[1], x = 0, y = 0, countdown = 0, explode_on_space = true }")
                .exec()
                .unwrap();
        });
        LAUNCHED.with(|l| {
            let l = l.borrow();
            let nuke = &l[l.len() - 2];
            assert_eq!((nuke.slot, nuke.sound, nuke.explode_into), (StockWeapon::Mine.index() as u32, Some(0), Some(1)));
            let chilli = l.last().unwrap();
            assert_eq!((chilli.countdown, chilli.explode_on_space), (Some(0), Some(true)));
        });
    }

    #[test]
    fn land_verbs_reach_native_only_during_simulation() {
        let _guard = lock_host();
        thread_local! {
            static STAMP: std::cell::Cell<Option<(i32, u32, i32, i32)>> = const { std::cell::Cell::new(None) };
            static CUT: std::cell::Cell<Option<(i32, i32, i32)>> = const { std::cell::Cell::new(None) };
            static DIG: std::cell::Cell<Option<(i32, i32, i32)>> = const { std::cell::Cell::new(None) };
            static BURIED: std::cell::Cell<Option<actor::StampBuried>> = const { std::cell::Cell::new(None) };
        }
        actor::bind_actor_native(actor::ActorNative {
            resolve_sprite: |_| Some((10001, 0, 60, 60)),
            land_stamp: |id, frame, x, y, buried| {
                STAMP.with(|c| c.set(Some((id, frame, x, y))));
                BURIED.with(|c| c.set(buried));
                Ok(())
            },
            land_cut: |shape, x, y| { CUT.with(|c| c.set(Some((shape, x, y)))); Ok(()) },
            land_dig: |x, y, radius| { DIG.with(|c| c.set(Some((x, y, radius)))); Ok(()) },
            ..Default::default()
        });
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        let cut = "wa.land.cut{ x = 0x12340000, y = 0x56780000, shape = 'torch', facing = 1, angle = 0 }";
        let stamp = "wa.land.stamp{ sprite = 'sprites/javelin/stamp', frame = 31, x = -65536, y = 131072 }";
        assert!(lua.load(stamp).exec().is_err());
        assert!(lua.load(cut).exec().is_err());
        sim_context::with_sim(|| {
            lua.load(stamp).exec().unwrap();
            assert!(lua.load(stamp.replace("frame = 31", "frame = -1")).exec().is_err());
            lua.load(cut).exec().unwrap();
            lua.load("wa.land.dig{ x = 0x10000, y = 0x20000, radius = 12 }").exec().unwrap();
        });
        STAMP.with(|c| assert_eq!(c.get(), Some((10001, 31, -65536, 131072))));
        BURIED.with(|c| assert_eq!(c.get(), None, "no buried table: drawn over the land as before"));
        let buried = |table: &str| stamp.replace(" }", &format!(", buried = {table} }}"));
        sim_context::with_sim(|| {
            lua.load(buried("{}")).exec().unwrap();
            BURIED.with(|c| assert_eq!(c.get(), Some(actor::StampBuried { soften: 2, mix: false })));
            lua.load(buried("{ soften = 5, blend = 'mix' }")).exec().unwrap();
            BURIED.with(|c| assert_eq!(c.get(), Some(actor::StampBuried { soften: 5, mix: true })));
            assert!(lua.load(buried("{ soften = 0 }")).exec().is_err());
            assert!(lua.load(buried("{ soften = 33 }")).exec().is_err());
            assert!(lua.load(buried("{ blend = 'smear' }")).exec().is_err());
        });
        CUT.with(|c| assert_eq!(c.get(), Some((10, 0x12340000, 0x56780000))));
        DIG.with(|c| assert_eq!(c.get(), Some((0x10000, 0x20000, 12))));
    }

    #[test]
    fn hellish_grenade_brings_px_art_and_firing_to_the_holy_grenade() {
        let _guard = lock_host();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/examples/hellish_grenade");
        let pack = LoadedMod {
            manifest: parse_manifest(&std::fs::read_to_string(root.join("mod.toml")).unwrap())
                .unwrap(),
            root,
        };
        let scripts = run_mods(&[pack]).unwrap();
        assert_eq!(scripts.resolved.applied.len(), 1);
        let claim = &scripts.resolved.applied[0];
        assert_eq!(claim.slot, StockWeapon::HolyGrenade);
        assert_eq!(claim.params.name.as_deref(), Some("Hellish Hand-Grenade"));
        assert_eq!(claim.params.copy_from, Some(StockWeapon::HolyGrenade));
        assert!(claim.params.wep.is_none(), "firing is copy_from + patch, not a .wep file");
        assert!(
            claim.params.patch.iter().any(|&(off, v)| off == 0x50 && v == 70),
            "damage 70 from the hellish patch"
        );
        assert!(
            claim.params.patch.iter().any(|&(off, v)| off == 0xF0 && v == 2),
            "CreateFire1 on detonation"
        );
        match &claim.params.panel_icon {
            Some(weapons::PanelIcon::File { path, width, height, .. }) => {
                assert_eq!(path, "sprites/hhh_ico.png");
                assert_eq!((*width, *height), (32, 32));
            }
            other => panic!("expected a PNG icon, got {other:?}"),
        }
        let grenade = claim.params.sprite_sheet.as_ref().expect("the thrown grenade's sheet");
        assert_eq!(grenade.frames.len(), 32, "PX's 32 spin frames");
        assert_eq!((grenade.frames[0].width, grenade.frames[0].height), (45, 45));
        // Aim and draw, each on flat ground and both slopes.
        assert_eq!(claim.params.worm_sprites.len(), 6);
        for (_, _, path) in &claim.params.worm_sprites {
            let frames = claim
                .params
                .extra_sheets
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, s)| s.frames.len())
                .unwrap();
            let want = if path.contains("aim") { 32 } else { 10 };
            assert_eq!(frames, want, "{path}");
        }
    }

    #[test]
    fn highlander_puts_its_ported_weapons_on_the_schemes_slots() {
        let _guard = lock_host();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/examples/highlander");
        let pack = LoadedMod {
            manifest: parse_manifest(&std::fs::read_to_string(root.join("mod.toml")).unwrap())
                .unwrap(),
            root,
        };
        let scripts = run_mods(&[pack]).unwrap();
        let applied = &scripts.resolved.applied;
        assert_eq!(applied.len(), 67);
        let claim = |slot| applied.iter().find(|c| c.slot == slot).expect("claimed");
        let skip_go = claim(StockWeapon::SkipGo);
        assert_eq!(skip_go.params.name.as_deref(), Some("Skip Go/Surrender"));
        assert_eq!(skip_go.params.stock_fire_choice, Some(StockWeapon::Surrender));
        let ice_arrow = claim(StockWeapon::Armageddon);
        assert_eq!(ice_arrow.params.name.as_deref(), Some("Ice Arrow"));
        assert!(ice_arrow.params.wep.is_some());
        let valyrian = claim(StockWeapon::PetrolBomb);
        assert_eq!(valyrian.params.name.as_deref(), Some("Valyrian Fire"));
        assert!(valyrian.params.wep.is_some());
        assert!(valyrian.params.on_fire_id != 0, "thrown through on_fire");
        assert_eq!(valyrian.params.hold.as_ref().unwrap().sheet, "sprites/px_hold/valyrian_fire.png");
        let reinforcement = claim(StockWeapon::NuclearTest);
        assert_eq!(reinforcement.params.name.as_deref(), Some("Reinforcement"));
        assert!(reinforcement.params.on_click_id != 0, "a click claim");
        let ferret = claim(StockWeapon::SalvationArmy);
        assert_eq!(ferret.params.name.as_deref(), Some("Ferret"));
        assert!(ferret.params.on_fire_id != 0, "thrown through on_fire");
        let portal = claim(StockWeapon::ScalesOfJustice);
        assert_eq!(portal.params.name.as_deref(), Some("Portal Gun"));
        assert!(portal.params.wep.is_some());
        assert!(portal.params.on_fire_id != 0);
        let magnet = claim(StockWeapon::BaseballBat);
        assert_eq!(magnet.params.name.as_deref(), Some("Electromagnet"));
        assert!(magnet.params.wep.is_some());
        assert!(magnet.params.on_fire_id != 0);
        let crate_strike = claim(StockWeapon::Donkey);
        assert_eq!(crate_strike.params.name.as_deref(), Some("Crate Strike"));
        assert!(crate_strike.params.on_click_id != 0, "a click claim");
        let sentry = claim(StockWeapon::Kamikaze);
        assert_eq!(sentry.params.name.as_deref(), Some("Sentry Gun"));
        assert!(sentry.params.on_fire_id != 0, "placed through on_fire");
        assert_eq!(sentry.params.worm_sprites_team_tint.as_ref().map(|t| t.colours.len()), Some(6));
        let laser = claim(StockWeapon::DragonBall);
        assert_eq!(laser.params.name.as_deref(), Some("Sentry Laser"));
        assert!(laser.params.on_fire_id != 0, "placed through on_fire");
        assert_eq!(laser.params.worm_sprites_team_tint.as_ref().map(|t| t.colours.len()), Some(6));
        let javelin = claim(StockWeapon::Prod);
        assert_eq!(javelin.params.name.as_deref(), Some("Javelin"));
        assert!(javelin.params.wep.is_some());
        assert!(javelin.params.on_explode_id != 0);
        let held: Vec<_> = applied.iter().filter(|c| c.params.hold.is_some()).collect();
        assert_eq!(held.len(), 10, "each PX hold is a replacement claim");
        let rifle = claim(StockWeapon::Handgun).params.hold.as_ref().unwrap();
        assert_eq!(rifle.sheet, "sprites/px_hold/sniper_rifle");
        assert!(rifle.hand.is_none());
        assert_eq!(rifle.flash.as_ref().unwrap().sheet, "sprites/sniper_rifle/flash");
        assert!(claim(StockWeapon::Handgun).params.extra_sheets.iter().any(|(path, _)|
            path == "sprites/sniper_rifle/flash"));

        // The scheme's record for the girder slot is the Baseball Bat:
        // a special weapon, subtype 2, as WA's own bat.
        let bat = claim(StockWeapon::Girder);
        assert_eq!(bat.params.copy_from, Some(StockWeapon::Girder));
        assert!(bat.params.wep.is_none());
        assert_eq!(bat.params.name.as_deref(), Some("Baseball Bat"));
        // Subtype 2, as WA's own bat (stock girder is 17).
        assert!(
            bat.params.patch.iter().any(|&(off, v)| off == 0x34 && v == 2),
            "subtype_param baseball bat"
        );        assert_eq!(
            bat.params.panel_icon,
            Some(weapons::PanelIcon::Stock(StockWeapon::BaseballBat))
        );
        // One on its own slot keeps its own icon.
        let torch = claim(StockWeapon::BlowTorch);
        assert_eq!(torch.params.name.as_deref(), Some("BlowTorch"));
        assert_eq!(torch.params.panel_icon, None);
        for (slot, name) in [
            (StockWeapon::MagicBullet, "Cluster Bomb"),
            (StockWeapon::Invisibility, "Teleport"),
            (StockWeapon::DamageX2, "Skunk"),
            (StockWeapon::DoubleTurnTime, "Mole Bomb"),
            (StockWeapon::GirderPack, "3 Girder Pack"),
        ] {
            assert_eq!(claim(slot).params.name.as_deref(), Some(name));
        }
        // PX's own art: the Super Gravedigger's flight strip and worm sheets.
        let digger = claim(StockWeapon::BananaBomb);
        assert_eq!(digger.params.name.as_deref(), Some("Super Gravedigger"));
        assert_eq!(digger.params.sprite_sheet.as_ref().unwrap().frames.len(), 32);
        assert_eq!(digger.params.worm_sprites.len(), 6);
        assert_eq!(
            digger.params.cluster_sprite_path.as_deref(),
            Some("sprites/super_gravedigger/cluster")
        );
        assert_eq!(
            claim(StockWeapon::ClusterBomb).params.cluster_sprite_path.as_deref(),
            Some("sprites/windy_cluster/cluster")
        );
        // The Burst Rifle's gun poses, fire included.
        assert_eq!(claim(StockWeapon::Minigun).params.worm_sprites.len(), 9);
        // Trails on the stock sprites PX repurposes, and the S.M.A.R.T
        // Bomb's homing bits.
        let smart = claim(StockWeapon::Mortar);
        assert_eq!(smart.params.sprite_overrides, vec![(250, "sprites/smart_bomb/trail".to_string())]);
        assert_eq!(
            smart.params.cluster_homing_sprite_path.as_deref(),
            Some("sprites/smart_bomb/cluster_homing")
        );
        let thumper = claim(StockWeapon::BattleAxe);
        assert_eq!(
            thumper.params.sprite_overrides.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![251, 252]
        );
        assert!(matches!(
            claim(StockWeapon::MailStrike).params.panel_icon,
            Some(weapons::PanelIcon::File { width: 32, height: 32, .. })
        ));
    }

    #[test]
    fn panel_icon_pack_loads_a_png_icon() {
        let _guard = lock_host();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/examples/panel_icon");
        let pack = LoadedMod {
            manifest: parse_manifest(&std::fs::read_to_string(root.join("mod.toml")).unwrap())
                .unwrap(),
            root,
        };
        let scripts = run_mods(&[pack]).unwrap();
        assert_eq!(scripts.resolved.applied.len(), 1);
        let claim = &scripts.resolved.applied[0];
        assert_eq!(claim.slot, StockWeapon::HolyGrenade);
        match &claim.params.panel_icon {
            Some(weapons::PanelIcon::File { path, width, height, rgba }) => {
                assert_eq!(path, "sprites/hhh_ico.png");
                assert_eq!((*width, *height), (32, 32), "a full PX icon image");
                assert_eq!(rgba.len(), 32 * 32 * 4);
            }
            other => panic!("expected a PNG icon, got {other:?}"),
        }
    }

    #[test]
    fn panel_icon_path_must_stay_in_the_pack() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-panel-icon-escape");
        let _ = std::fs::remove_dir_all(&root);
        let bad = pack(
            &root,
            "t.icon",
            r#"
id = "t.icon"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["holy_grenade"]
"#,
            r#"wa.weapons.replace({ weapon = "holy_grenade", panel_icon = "../x.png" })"#,
        );
        let err = run_mods(&[bad]).err().expect("an escaping path is refused");
        assert!(err.contains("inside the pack"), "{err}");
    }

    #[test]
    fn sentry_gun_worm_sheets_come_per_team_colour() {
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
        assert_eq!(claim.slot, StockWeapon::Donkey);
        match &claim.params.panel_icon {
            Some(weapons::PanelIcon::File { path, width, height, .. }) => {
                assert_eq!(path, "sprites/ico_sentrygun.png");
                assert_eq!((*width, *height), (32, 32), "a full PX icon image");
            }
            other => panic!("expected a PNG icon, got {other:?}"),
        }
        // Six worm poses, each baked once per team colour, in alliance order.
        let sheets = &claim.params.worm_sprites;
        assert_eq!(sheets.len(), 6 * 6);
        let link = gfx::stock_worm_sprite_id("weaponlnk").unwrap();
        let teams: Vec<_> = sheets
            .iter()
            .filter(|(id, _, _)| *id == link)
            .map(|(_, team, path)| (team.unwrap(), path.as_str()))
            .collect();
        assert_eq!(teams[0], (0, "sprites/teams/red/equip"));
        assert_eq!(teams[1], (1, "sprites/teams/blue/equip"));
        assert_eq!(teams[5], (5, "sprites/teams/cyan/equip"));
        assert!(sheets.iter().all(|(_, _, path)| claim
            .params
            .extra_sheets
            .iter()
            .any(|(p, s)| p == path && s.frames.len() == 10)));
    }

    #[test]
    fn worm_sprites_team_list_holds_at_most_six() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-worm-sprites-teams");
        let _ = std::fs::remove_dir_all(&root);
        let bad = pack(
            &root,
            "t.teams",
            r#"
id = "t.teams"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["bazooka"]
"#,
            r#"
local seven = {}
for i = 1, 7 do seven[i] = "sprites/x.png" end
wa.weapons.replace({ weapon = "bazooka", worm_sprites = { weaponlnk = seven } })
"#,
        );
        let err = run_mods(&[bad]).err().expect("a seventh team is refused");
        assert!(err.contains("1 to 6 paths"), "{err}");
    }

    #[test]
    fn turret_pack_overlays_dynamite_with_mine_placement() {
        let _guard = lock_host();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/examples/turret");
        let pack = LoadedMod {
            manifest: parse_manifest(&std::fs::read_to_string(root.join("mod.toml")).unwrap())
                .unwrap(),
            root,
        };
        let scripts = run_mods(&[pack]).unwrap();
        assert_eq!(scripts.resolved.applied.len(), 1);
        let claim = &scripts.resolved.applied[0];
        assert_eq!(claim.slot, StockWeapon::Dynamite);
        assert_eq!(claim.params.copy_from, Some(StockWeapon::Mine));
        assert_ne!(claim.params.on_fire_id, 0);
        assert_eq!(claim.params.worm_sprites.len(), 6);
        let mut turret_sheets = 0;
        for (path, sheet) in &claim.params.extra_sheets {
            if path.contains("turret_t") {
                assert_eq!(sheet.frames.len(), 120);
                turret_sheets += 1;
            } else if path.contains("uzihit") {
                assert_eq!(sheet.frames.len(), 10);
            } else if path.contains("equip") {
                assert_eq!(sheet.frames.len(), 10);
                assert!(!sheet.looping);
            }
        }
        assert_eq!(turret_sheets, 18);
    }

    #[test]
    fn cluster_bit_sprite_and_homing_and_panel_icon() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-cluster-bit");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("c"),
            "user.hello_world",
            r#"
id = "user.hello_world"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["cluster_bomb"]
"#,
            r#"
wa.weapons.replace({
  weapon = "cluster_bomb",
  name = "hello world",
  copy_from = "cluster_bomb",
  panel_icon = "banana",
  sprite = "crshairr",
  contact = true,
  fuse = true,
  cluster = {
    sprite = "targetw",
    contact = true,
    homing = "lock",
  },
})
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        let c = &scripts.resolved.applied[0];
        assert_eq!(c.params.sprite_id, Some(2));
        assert_eq!(c.params.cluster_sprite_id, Some(94));
        assert_eq!(c.params.cluster_homing, Some(weapons::HomingMode::Lock));
        assert_eq!(c.params.panel_icon, Some(weapons::PanelIcon::Stock(StockWeapon::BananaBomb)));
    }

    #[test]
    fn cluster_takes_a_pack_sheet() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-cluster-sheet");
        let _ = std::fs::remove_dir_all(&root);
        let own = pack(
            &root,
            "t.cluster",
            r#"
id = "t.cluster"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["cluster_bomb"]
"#,
            r#"
wa.weapons.replace({
  weapon = "cluster_bomb",
  cluster = { sprite = "bit.png", homing_sprite = "homing.png" },
})
"#,
        );
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../mods/examples/gravler_strike/sprites/gravlerdive.png"),
            root.join("bit.png"),
        )
        .unwrap();
        std::fs::copy(root.join("bit.png"), root.join("homing.png")).unwrap();
        let scripts = run_mods(&[own]).unwrap();
        let params = &scripts.resolved.applied[0].params;
        assert_eq!(params.cluster_sprite_path.as_deref(), Some("bit.png"));
        assert_eq!(params.cluster_sprite_id, None, "the id comes when the sheet is installed");
        assert!(params.extra_sheets.iter().any(|(p, _)| p == "bit.png"));
        assert_eq!(params.cluster_homing_sprite_path.as_deref(), Some("homing.png"));
        assert!(params.extra_sheets.iter().any(|(p, _)| p == "homing.png"));
    }

    #[test]
    fn wa_sprites_get_a_true_colour_copy_unless_a_pack_says_not() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-truecolor");
        let _ = std::fs::remove_dir_all(&root);
        let claim = |dir: &str, id: &str, params: &str| {
            pack(
                &root.join(dir),
                id,
                &format!("id = \"{id}\"\napi_version = 1\nentry = \"weapons.lua\"\n[weapons]\nreplace = [\"cluster_bomb\"]\n"),
                &format!("wa.weapons.replace({{ weapon = \"cluster_bomb\", {params} }})"),
            )
        };
        let default = claim("a", "t.default", "");
        let off = claim("b", "t.off", "params = { truecolor = false }");
        let on = run_mods(&[default]).unwrap();
        assert!(on.resolved.applied[0].params.truecolor);
        let off = run_mods(&[off]).unwrap();
        assert!(!off.resolved.applied[0].params.truecolor);
    }

    #[test]
    fn a_strike_takes_its_own_plane() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-strike-plane");
        let _ = std::fs::remove_dir_all(&root);
        let own = pack(
            &root,
            "t.plane",
            r#"
id = "t.plane"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["mb_bomb"]
"#,
            r#"wa.weapons.replace({ weapon = "mb_bomb", plane = "plane.png" })"#,
        );
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../mods/examples/gravler_strike/sprites/gravlerdive.png"),
            root.join("plane.png"),
        )
        .unwrap();
        let scripts = run_mods(&[own]).unwrap();
        let params = &scripts.resolved.applied[0].params;
        assert_eq!(params.plane_sprite_path.as_deref(), Some("plane.png"));
        assert!(params.extra_sheets.iter().any(|(p, _)| p == "plane.png"));
    }

    #[test]
    fn fire_repeat_is_one_to_twenty_shots() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-fire-repeat");
        let _ = std::fs::remove_dir_all(&root);
        let manifest = r#"
id = "t.repeat"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["shotgun"]
"#;
        let five = pack(
            &root,
            "t.repeat",
            manifest,
            r#"wa.weapons.replace({ weapon = "shotgun", fire_repeat = 5 })"#,
        );
        let scripts = run_mods(&[five]).unwrap();
        assert_eq!(scripts.resolved.applied[0].params.fire_repeat, 5);
        let stock = pack(
            &root,
            "t.repeat",
            manifest,
            r#"wa.weapons.replace({ weapon = "shotgun" })"#,
        );
        assert_eq!(run_mods(&[stock]).unwrap().resolved.applied[0].params.fire_repeat, 1);
        let bad = pack(
            &root,
            "t.repeat",
            manifest,
            r#"wa.weapons.replace({ weapon = "shotgun", fire_repeat = 0 })"#,
        );
        let err = run_mods(&[bad]).err().expect("no shot at all is refused");
        assert!(err.contains("fire_repeat is 1 to 20"), "{err}");
    }

    #[test]
    fn patch_lists_weapon_fields_by_offset() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-patch");
        let _ = std::fs::remove_dir_all(&root);
        let manifest = r#"
id = "t.patch"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["bazooka"]
"#;
        let mixed = pack(
            &root,
            "t.patch",
            manifest,
            r#"wa.weapons.replace({
  weapon = "bazooka",
  copy_from = "bazooka",
  patch = {
    cluster_count = 5,
    damage = 70,
    detonation = 1,
    requires_aiming = 0,
  },
})"#,
        );
        let scripts = run_mods(&[mixed]).unwrap();
        let p = &scripts.resolved.applied[0].params;
        assert_eq!(p.copy_from, Some(StockWeapon::Bazooka));
        assert_eq!(p.patch, vec![(0x0C, 0), (0x50, 70), (0xF0, 1), (0xF8, 5)], "sorted by offset");

        let legacy = pack(
            &root,
            "t.patch",
            manifest,
            r#"wa.weapons.replace({
  weapon = "bazooka",
  copy_from = "bazooka",
  patch = { [0x50] = 55 },
})"#,
        );
        let scripts = run_mods(&[legacy]).unwrap();
        assert_eq!(scripts.resolved.applied[0].params.patch, vec![(0x50, 55)]);

        let bad_offset = pack(
            &root,
            "t.patch",
            manifest,
            r#"wa.weapons.replace({ weapon = "bazooka", patch = { [0x10] = 1 } })"#,
        );
        let err = run_mods(&[bad_offset]).err().expect("the defined flag is the slot's");
        assert!(err.contains("not a weapon field"), "{err}");

        let bad_fire = pack(
            &root,
            "t.patch",
            manifest,
            r#"wa.weapons.replace({ weapon = "bazooka", patch = { fire_type = 7 } })"#,
        );
        let err = run_mods(&[bad_fire]).err().expect("fire type 7 is not WA's");
        assert!(err.contains("fire type 7"), "{err}");

        let bad_key = pack(
            &root,
            "t.patch",
            manifest,
            r#"wa.weapons.replace({ weapon = "bazooka", patch = { no_such_field = 70 } })"#,
        );
        let err = run_mods(&[bad_key]).err().expect("unknown names are refused");
        assert!(err.contains("not a known weapon field"), "{err}");
    }

    #[test]
    fn patch_names_shift_by_the_fire_type_the_weapon_ends_with() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-patch-strike");
        let _ = std::fs::remove_dir_all(&root);
        let manifest = r#"
id = "t.strike"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["air_strike"]
"#;
        let damage_at = |src: &str| {
            let scripts = run_mods(&[pack(&root, "t.strike", manifest, src)]).unwrap();
            let p = &scripts.resolved.applied[0].params;
            p.patch.iter().find(|&&(_, v)| v == 70).map(|&(off, _)| off)
        };
        // A strike's munition slab sits 0x14 later, with or without copy_from.
        assert_eq!(
            damage_at(r#"wa.weapons.replace({ weapon = "air_strike", patch = { damage = 70 } })"#),
            Some(0x64)
        );
        // A strike made a projectile takes the projectile's offsets.
        assert_eq!(
            damage_at(
                r#"wa.weapons.replace({ weapon = "air_strike", copy_from = "air_strike",
                     patch = { fire_type = 1, damage = 70 } })"#
            ),
            Some(0x50)
        );
    }

    #[test]
    fn mine_brain_and_sprite() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-mine-brain");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("m"),
            "user.mine",
            r#"
id = "user.mine"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["bazooka"]
"#,
            r#"
wa.weapons.replace({
  weapon = "bazooka",
  name = "My Weapon",
  copy_from = "mine",
  sprite = "crshairb",
  params = { damage = 80, bounce_pct = 100 },
  mine = { proximity = 40, fuse = 3000, trigger = 1000 },
})
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        let c = &scripts.resolved.applied[0];
        assert_eq!(c.params.sprite_id, Some(3));
        assert_eq!(c.params.damage, Some(80));
        assert_eq!(c.params.bounce_pct, Some(100));
        assert_eq!(c.params.mine_proximity, Some(40));
        assert_eq!(c.params.mine_fuse, Some(3000));
        assert_eq!(c.params.mine_trigger, Some(1000));
    }

    #[test]
    fn mine_on_explode_invisible_now_keeps_mine_entity() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-mine-emit");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("m"),
            "user.mine_emit",
            r#"
id = "user.mine_emit"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["bazooka"]
"#,
            r#"
wa.weapons.replace({
  weapon = "bazooka",
  copy_from = "mine",
  sprite = "crshairb",
  on_explode = {
    spawn = {
      sprite = false,
      explode = { damage = 0, id = 40, now = true },
      damage_worms = false,
    },
  },
  on_proximity = {
    spawn = {
      sprite = false,
      explode = { damage = 0, id = 20, now = true },
      damage_worms = false,
      damage_terrain = false,
    },
  },
})
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        let c = &scripts.resolved.applied[0];
        assert!(!c.params.uses_lua_projectile());
        let boom = c.params.explode_spawn.as_ref().expect("explode spawn");
        assert!(!boom.spawn.visible);
        assert_eq!(boom.sprite, game::offsets::CUSTOM_SPRITE_SENTINEL);
        assert_eq!(boom.spawn.explode.unwrap().now, true);
        assert!(!boom.spawn.damage_worms);
        assert!(!boom.spawn.persist);
        assert_eq!(boom.count, 1);
        let prox = c.params.proximity_spawn.as_ref().expect("proximity spawn");
        assert!(!prox.spawn.visible);
        assert!(!prox.spawn.damage_terrain);
    }

    #[test]
    fn fire_spawn_can_chain_on_explode() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-fire-emit");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("f"),
            "user.fire_emit",
            r#"
id = "user.fire_emit"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["bazooka"]
"#,
            r#"
wa.weapons.replace({
  weapon = "bazooka",
  on_fire = {
    spawn = {
      sprite = "missile",
      explode = { damage = 50 },
      on_explode = {
        sprite = false,
        explode = { damage = 0, id = 40, now = true },
        damage_worms = false,
      },
    },
  },
})
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        let c = &scripts.resolved.applied[0];
        assert!(c.params.uses_lua_projectile());
        let child = c
            .params
            .fire_spawn
            .as_ref()
            .expect("fire spawn")
            .on_explode
            .as_ref()
            .expect("nested explode");
        assert!(!child.spawn.visible);
        assert_eq!(child.spawn.explode.unwrap().now, true);
    }

    #[test]
    fn cluster_actor_parses_data_values() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-cluster-actor");
        let _ = std::fs::remove_dir_all(&root);
        let mine = pack(
            &root.join("m"),
            "user.cluster_mine",
            r#"
id = "user.cluster_mine"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["bazooka"]
"#,
            r#"
wa.weapons.replace({
  weapon = "bazooka",
  copy_from = "mine",
  on_explode = {
    spawn = {
      kind = "cluster",
      sprite = "banana",
      trail = "smklt25",
      count = 5,
      spread = 45,
      power = 0xA0000,
      impact = true,
      bounce = true,
      homing = "worm",
      explode = { damage = 30, id = 100 },
    },
  },
})
"#,
        );
        let scripts = run_mods(&[mine]).unwrap();
        let c = &scripts.resolved.applied[0];
        assert!(!c.params.uses_lua_projectile());
        let boom = c.params.explode_spawn.as_ref().expect("explode cluster");
        let spec = boom.spawn.cluster.as_ref().expect("cluster spec");
        assert_eq!(spec.count, 5);
        assert_eq!(spec.spread, 45);
        assert_eq!(spec.power, 0xA0000);
        assert_eq!(spec.trail, 609);
        assert_eq!(spec.sprite, 51);
        assert_eq!(spec.bounce_pct, 100);
        assert_eq!(spec.homing, weapons::ClusterHoming::Worm);
        assert!(spec.impact);
        assert_eq!(boom.count, 5);

        let fire = pack(
            &root.join("f"),
            "user.cluster_fire",
            r#"
id = "user.cluster_fire"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["bazooka"]
"#,
            r#"
wa.weapons.replace({
  weapon = "bazooka",
  on_fire = {
    spawn = {
      kind = "cluster",
      sprite = "banana",
      count = 8,
      spread = 90,
      impact = false,
      bounce = false,
      homing = false,
      collide = false,
      explode = false,
    },
  },
})
"#,
        );
        let scripts = run_mods(&[fire]).unwrap();
        let c = &scripts.resolved.applied[0];
        assert!(c.params.uses_lua_projectile());
        let spawn = c.params.fire_spawn.as_ref().expect("fire cluster");
        let fx = spawn.cluster.as_ref().expect("fx spec");
        assert_eq!(fx.count, 8);
        assert_eq!(fx.spread, 90);
        assert!(!fx.impact);
        assert_eq!(fx.bounce_pct, 0);
        assert_eq!(fx.homing, weapons::ClusterHoming::Off);
        assert!(!spawn.collide);
        assert!(spawn.explode.is_none());

        let bounce = pack(
            &root.join("b"),
            "user.cluster_bounce",
            r#"
id = "user.cluster_bounce"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["bazooka"]
"#,
            r#"
wa.weapons.replace({
  weapon = "bazooka",
  copy_from = "mine",
  on_explode = {
    spawn = {
      kind = "cluster",
      sprite = "banana",
      count = 5,
      impact = false,
      bounces = 3,
      fuse = 2000,
      proximity = 40,
      explode = { damage = 30, id = 100 },
    },
  },
})
"#,
        );
        let scripts = run_mods(&[bounce]).unwrap();
        let c = &scripts.resolved.applied[0];
        let boom = c.params.explode_spawn.as_ref().expect("bounce cluster");
        let spec = boom.spawn.cluster.as_ref().expect("bounce spec");
        assert!(!spec.impact);
        assert_eq!(spec.bounce_pct, 100); // default grenade bounce when omitted
        assert_eq!(spec.bounces, 3);
        assert_eq!(spec.fuse_ms, 2000);
        assert_eq!(spec.proximity, 40);
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

    /// A pack whose entry is `entry` and whose other files are `files`.
    fn pack_with(dir: &std::path::Path, id: &str, entry: &str, files: &[(&str, &str)]) -> LoadedMod {
        let toml = format!("id = \"{id}\"\napi_version = 1\nentry = \"weapons.lua\"\n");
        let pack = pack(dir, id, &toml, entry);
        for (path, src) in files {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, src).unwrap();
        }
        pack
    }

    #[test]
    fn a_pack_requires_its_own_modules_once_each_in_private() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-require-own");
        let _ = std::fs::remove_dir_all(&root);
        let p = pack_with(
            &root.join("p"),
            "p.mods",
            r#"
GREETING = "hi"
local shared = require("lib/shared")
assert(require("lib/shared") == shared, "one value per module")
assert(shared.answer == 42, "nested require")
assert(shared.greeting == "hi", "modules read the shared globals")
assert(require("lib/silent") == true, "a module returning nothing gives true")
ENTRY_SAW = shared.answer
"#,
            &[
                ("lib/shared.lua", "LEAKED = true\nlocal two = require('lib/two')\nreturn { answer = 40 + two, greeting = GREETING, wa_seen = wa ~= nil }"),
                ("lib/two.lua", "LOADS = (LOADS or 0) + 1\nreturn 2"),
                ("lib/silent.lua", "local x = 1"),
            ],
        );
        let scripts = run_mods(&[p]).unwrap();
        let g = scripts.lua.globals();
        assert_eq!(g.get::<i64>("ENTRY_SAW").unwrap(), 42, "the entry still writes the shared globals");
        assert!(g.get::<Option<bool>>("LEAKED").unwrap().is_none(), "a module's globals stay its own");
        assert!(g.get::<Option<i64>>("LOADS").unwrap().is_none());
        assert!(g.get::<Option<mlua::Value>>("require").unwrap().is_none(), "no global require");
    }

    #[test]
    fn two_packs_each_load_their_own_module_of_the_same_name() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-require-two");
        let _ = std::fs::remove_dir_all(&root);
        let a = pack_with(&root.join("a"), "a.p", "A_UTIL = require('lib/util')", &[("lib/util.lua", "return 'a'")]);
        let b = pack_with(&root.join("b"), "b.p", "B_UTIL = require('lib/util')", &[("lib/util.lua", "return 'b'")]);
        let scripts = run_mods(&[a, b]).unwrap();
        let g = scripts.lua.globals();
        assert_eq!(g.get::<String>("A_UTIL").unwrap(), "a");
        assert_eq!(g.get::<String>("B_UTIL").unwrap(), "b");
    }

    #[test]
    fn require_refuses_cycles_the_entry_and_anything_outside_the_pack() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-require-refused");
        let _ = std::fs::remove_dir_all(&root);
        let cases = [
            ("require('lib/a')", "require cycle in t.p: lib/a -> lib/b -> lib/a"),
            ("require('weapons')", "weapons.lua is the pack's entry file"),
            ("require('../other/x')", "with '/' between folders"),
            ("require('lib/missing')", "no lib/missing.lua in the pack"),
            ("require('lib')", "lib.lua"),
        ];
        for (i, (entry, expected)) in cases.iter().enumerate() {
            let p = pack_with(
                &root.join(format!("p{i}")),
                "t.p",
                entry,
                &[("lib/a.lua", "return require('lib/b')"), ("lib/b.lua", "return require('lib/a')")],
            );
            let err = run_mods(&[p]).err().unwrap_or_default();
            assert!(err.contains(expected), "{entry}: {err}");
        }
    }

    /// A bundle on disk: its mod.toml and Lua files.
    fn bundle(dir: &std::path::Path, toml: &str, files: &[(&str, &str)]) -> weapons::components::LoadedBundle {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("mod.toml"), toml).unwrap();
        for (path, src) in files {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, src).unwrap();
        }
        weapons::components::LoadedBundle {
            manifest: weapons::components::parse_bundle(toml).unwrap(),
            root: dir.to_path_buf(),
        }
    }

    fn plan(bundles: &[weapons::components::LoadedBundle], profile: &weapons::components::Profile) -> ComponentPlan {
        let manifests: Vec<_> = bundles.iter().map(|b| b.manifest.clone()).collect();
        ComponentPlan {
            resolved: weapons::components::resolve(&manifests, profile).unwrap(),
            roots: bundles.iter().map(|b| (b.manifest.id.clone(), b.root.clone())).collect(),
        }
    }

    const CATALOG: &str = r#"
id = "t.weapons"
api_version = 1
format_version = 2
[[components]]
id = "t.double_bazooka"
type = "weapon"
entry = "weapons/double_bazooka.lua"
compatible_positions = [{ row = "F1", column = 1 }]
[[components]]
id = "t.unused"
type = "weapon"
entry = "weapons/unused.lua"
compatible_positions = [{ row = "F1", column = 2 }]
"#;

    const SCHEMES: &str = r#"
id = "t.schemes"
api_version = 1
format_version = 2
[[components]]
id = "t.scheme"
type = "scheme"
entry = "scheme.lua"
preset = "p"
[presets.p.weapons]
"t.double_bazooka" = { row = "F1", column = 1 }
[[components]]
id = "t.feature"
type = "feature"
entry = "feature.lua"
[[components.options]]
key = "fast"
type = "bool"
default = false
label = "Fast"
"#;

    fn selection(scheme_src: &str, weapon_src: &str, feature_src: &str) -> (Vec<weapons::components::LoadedBundle>, weapons::components::Profile) {
        let root = std::env::temp_dir().join(format!("wklua-components-{}", weapon_src.len() + 7 * scheme_src.len() + 13 * feature_src.len()));
        let _ = std::fs::remove_dir_all(&root);
        let bundles = vec![
            bundle(&root.join("w"), CATALOG, &[
                ("weapons/double_bazooka.lua", weapon_src),
                ("weapons/unused.lua", "error('an unselected weapon ran')"),
            ]),
            bundle(&root.join("s"), SCHEMES, &[("scheme.lua", scheme_src), ("feature.lua", feature_src)]),
        ];
        let profile = weapons::components::Profile {
            scheme: Some("t.scheme".into()),
            features: ["t.feature".to_string()].into(),
            options: [("t.feature".to_string(), [("fast".to_string(), weapons::components::OptionValue::Bool(true))].into())].into(),
            ..Default::default()
        };
        (bundles, profile)
    }

    #[test]
    fn selected_components_declare_merge_and_install_in_order() {
        let _guard = lock_host();
        let (bundles, profile) = selection(
            r#"return {
                 weapons = { ["t.double_bazooka"] = { ammo = { turn_start = { amount = 1, mode = "replace" } }, delay = 0 },
                             ["t.unused"] = { delay = 3 } },
                 install = function(ctx) wa.log("scheme sees " .. ctx.weapons["t.double_bazooka"].stock_slot) end,
               }"#,
            r#"local shared = require("lib/shared")
               return {
                 defaults = { ammo = { round_start = 1, turn_start = { amount = 2, mode = "add" } }, delay = 2, ends_turn = false },
                 install = function(ctx)
                   local d = ctx.data
                   wa.log(("%s at %s %d: round %d, turn %d %s, delay %d, ends %s, %s"):format(ctx.stock_slot, ctx.position.row,
                     ctx.position.column, d.ammo.round_start, d.ammo.turn_start.amount, d.ammo.turn_start.mode, d.delay,
                     tostring(d.ends_turn), shared))
                   wa.weapons.replace({ weapon = ctx.stock_slot, name = "Double Bazooka" })
                 end,
               }"#,
            r#"return function(ctx)
                 wa.log("feature fast=" .. tostring(ctx.option("fast")) .. " scheme selected=" .. tostring(ctx.selected("t.scheme"))
                   .. " unused selected=" .. tostring(ctx.selected("t.unused")))
               end"#,
        );
        std::fs::create_dir_all(bundles[0].root.join("lib")).unwrap();
        std::fs::write(bundles[0].root.join("lib/shared.lua"), "return 'shared ok'").unwrap();
        let scripts = run_match(&[], Some(&plan(&bundles, &profile))).unwrap();
        let logs: Vec<&str> = scripts.logs.iter().map(String::as_str).collect();
        assert_eq!(logs, [
            "[components] scheme override for t.unused skipped: not in the loadout",
            "[t.double_bazooka] bazooka at F1 1: round 1, turn 1 replace, delay 0, ends false, shared ok",
            "[t.scheme] scheme sees bazooka",
            "[t.feature] feature fast=true scheme selected=true unused selected=false",
        ]);
        let claim = &scripts.resolved.applied[0];
        assert_eq!((claim.slot, claim.mod_id.as_str()), (StockWeapon::Bazooka, "t.double_bazooka"));
        assert_eq!(scripts.resolved.applied.len(), 1);
    }

    #[test]
    fn the_component_demo_example_runs_with_its_readme_profile() {
        let _guard = lock_host();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/examples/component_demo");
        let manifest = weapons::components::parse_bundle(&std::fs::read_to_string(root.join("mod.toml")).unwrap()).unwrap();
        // The README's profile.
        let profile = weapons::components::Profile {
            scheme: Some("demo.scheme".into()),
            features: ["demo.turn_note".to_string()].into(),
            options: [("demo.turn_note".to_string(), [("show".to_string(), weapons::components::OptionValue::Bool(true))].into())].into(),
            ..Default::default()
        };
        let bundles = [weapons::components::LoadedBundle { manifest, root }];
        let scripts = run_match(&[], Some(&plan(&bundles, &profile))).unwrap();
        assert_eq!(scripts.logs, [
            "[demo.big_bazooka] Big Bazooka on bazooka at F1 1: round_start -1, delay 1",
            "[demo.scheme] scheme places demo.big_bazooka on bazooka",
        ]);
        let claim = &scripts.resolved.applied[0];
        assert_eq!((claim.slot, claim.params.name.as_deref(), claim.params.damage), (StockWeapon::Bazooka, Some("Big Bazooka"), Some(80)));
    }

    #[test]
    fn components_are_refused_before_anything_installs() {
        let _guard = lock_host();
        let ok_scheme = "return {}";
        let ok_feature = "return function() end";
        let cases = [
            ("wa.log('x') return {}", ok_scheme, ok_feature, "weapon t.double_bazooka: ", "wa.log while declaring"),
            ("return { defaults = { delay = -1 } }", ok_scheme, ok_feature, "", "defaults.delay = -1 is outside 0..=99"),
            ("return { defaults = { ammo = { rounds = 1 } } }", ok_scheme, ok_feature, "", "defaults.ammo.rounds is not a field; use round_start"),
            ("return { defaults = { crate = { type = \"gold\" } } }", ok_scheme, ok_feature, "", "defaults.crate.type = \"gold\": use weapon"),
            ("return { defaults = {}, extra = 1 }", ok_scheme, ok_feature, "", "returns extra: a weapon returns only defaults and install"),
            ("return {}", "return { weapons = { [\"t.double_bazooka\"] = 5 } }", ok_feature, "scheme t.scheme: ", "weapons.t.double_bazooka must be a table"),
            ("return {}", ok_scheme, "local x = 1", "feature t.feature: ", "returns nothing"),
        ];
        for (weapon, scheme, feature, prefix, expected) in cases {
            let (bundles, profile) = selection(scheme, weapon, feature);
            let err = run_match(&[], Some(&plan(&bundles, &profile))).err().unwrap_or_default();
            assert!(err.contains(prefix) && err.contains(expected), "{weapon} / {scheme} / {feature}:\n{err}");
        }

        // A weapon claims only the slot it was placed on; another is skipped,
        // as a pack's undeclared slot is.
        let (bundles, profile) = selection(
            ok_scheme,
            "return { install = function(ctx) wa.weapons.replace({ weapon = 'mortar', name = 'x' }) end }",
            ok_feature,
        );
        let scripts = run_match(&[], Some(&plan(&bundles, &profile))).unwrap();
        assert!(scripts.resolved.applied.is_empty());
        assert!(scripts.logs.iter().any(|l| l.starts_with("[t.double_bazooka] skipped replace of mortar")), "{:?}", scripts.logs);
    }

    #[test]
    fn two_bazooka_overlays_first_wins() {
        let _guard = lock_host();
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

    fn filter_pack(root: &std::path::Path, script: &str) -> LoadedMod {
        let _ = std::fs::remove_dir_all(root);
        pack(
            &root.join("w"),
            "w.filter",
            r#"
id = "w.filter"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = []
"#,
            script,
        )
    }

    fn parked_seen() -> Vec<String> {
        PARKED_LUA.with(|slot| {
            let guard = slot.borrow();
            let lua = guard.as_ref().unwrap();
            let seen: mlua::Table = lua.globals().get("seen").unwrap();
            let out = seen.sequence_values::<String>().map(|v| v.unwrap()).collect();
            lua.load("seen = {}").exec().unwrap();
            out
        })
    }

    #[test]
    fn worm_message_filters_keep_registration_order() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-worm-filter");
        let pack = filter_pack(&root, r#"
seen = {}
local function note(tag) return function(_, m) seen[#seen + 1] = tag .. m end end
wa.on.worm_message(note("a"))
wa.on.worm_message(note("b"), { msgs = { 2 } })
wa.on.worm_message(note("c"))
wa.on.worm_message(note("d"), { msgs = { 2, 36 } })
"#);
        park_lua(run_mods(&[pack]).unwrap().lua);
        worm_message(1, 0, 2).unwrap();
        assert_eq!(parked_seen(), ["a2", "b2", "c2", "d2"]);
        worm_message(1, 0, 5).unwrap();
        assert_eq!(parked_seen(), ["a5", "c5"]);
        worm_message(1, 0, 36).unwrap();
        assert_eq!(parked_seen(), ["a36", "c36", "d36"]);
    }

    #[test]
    fn a_message_no_handler_wants_never_enters_lua() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-worm-filter-skip");
        let pack = filter_pack(&root, r#"
seen = {}
wa.on.worm_message(function(_, m) seen[#seen + 1] = "f" .. m end, { msgs = { 2 } })
"#);
        park_lua(run_mods(&[pack]).unwrap().lua);
        perf::enable(PerfFlags { counter: false, profile: true, packs: false, spikes: false });
        take_lua_time();
        assert!(!worm_message(1, 0, 5).unwrap());
        assert_eq!(take_lua_time().1, 0, "no call into Lua for an unwanted message");
        worm_message(1, 0, 2).unwrap();
        assert_eq!(take_lua_time().1, 1);
        perf::reset();
        assert_eq!(parked_seen(), ["f2"]);
    }

    #[test]
    fn another_state_built_mid_match_keeps_the_parked_filters() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-worm-filter-second");
        let pack = filter_pack(&root, r#"
seen = {}
wa.on.worm_message(function(_, m) seen[#seen + 1] = "f" .. m end, { msgs = { 2 } })
"#);
        park_lua(run_mods(&[pack]).unwrap().lua);
        // A second pack state, built and not parked (a pack check, say).
        let other = filter_pack(&root.join("other"), "-- no handlers");
        let _second = run_mods(&[other]).unwrap();
        worm_message(1, 0, 2).unwrap();
        assert_eq!(parked_seen(), ["f2"], "the parked state's handler still runs");
    }

    /// Dispatch cost per call, Highlander-shaped: 13 worm_message handlers
    /// (most of them filtered to FRAME_FINISH) and 5 missile_frame handlers
    /// that, like most real ones, look at the missile and return. Run it in
    /// release, as the DLL is built:
    /// `cargo test --release -p lua-host bench_dispatch -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench_dispatch() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-bench");
        let mut script = String::from("local W = {}\n");
        for i in 1..=13 {
            let msgs = if i % 4 == 0 { "{ 2, 0x26 }" } else { "{ 2 }" };
            script.push_str(&format!(
                "wa.on.worm_message(function(w, m) if m ~= 2 then return end W[{i}] = m end, {{ msgs = {msgs} }})\n"
            ));
        }
        for _ in 1..=5 {
            script.push_str("wa.on.missile_frame(function(m) if m.phase ~= 'post' then return end if m.weapon == 'prod' then m.vx = m.vx end end)\n");
        }
        let pack = filter_pack(&root, &script);
        park_lua(run_mods(&[pack]).unwrap().lua);
        let motion = game::entity::Motion {
            id: 0xF000_0001, class_type: 0, damp: 0x10000, wind_scale: 0x10000, gravity_scale: 0x10000,
            moved: 1, x: 100 << 16, y: 200 << 16, vx: 0x8000, vy: -0x8000, cm_gravity: 0x1000, cm_wind: 0,
        };
        let n = 20_000u32;
        let run = |label: &str| {
            let t = std::time::Instant::now();
            for _ in 0..n {
                worm_message(1, 0, 2).unwrap();
            }
            let wm = t.elapsed().as_nanos() as f64 / n as f64 / 1e3;
            let (mut sprite, mut mask) = (48, 0x7C);
            let t = std::time::Instant::now();
            for i in 0..n {
                let phase = if i % 2 == 0 { "pre" } else { "post" };
                missile_frame(motion, 2, phase, Some(6), &mut sprite, &mut mask).unwrap();
            }
            let mf = t.elapsed().as_nanos() as f64 / n as f64 / 1e3;
            println!("bench_dispatch {label}: worm_message {wm:.2} us/call, missile_frame {mf:.2} us/call");
        };
        run("profiler off");
        perf::enable(PerfFlags { counter: false, profile: true, packs: false, spikes: false });
        run("profiler on ");
        PARKED_LUA.with(|slot| perf::install_sampler(slot.borrow().as_ref().unwrap()));
        run("sampler on ");
        perf::enable(PerfFlags { counter: false, profile: true, packs: true, spikes: false });
        run("packs on    ");
        perf::reset();
    }

    #[test]
    fn missile_frame_fast_and_slow_paths_agree() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-missile-paths");
        let pack = filter_pack(&root, r#"
seen = {}
wa.on.missile_frame(function(m)
  seen[#seen + 1] = m.phase .. ":" .. tostring(m.weapon) .. ":" .. m.msg
  if m.phase ~= "post" then return end
  m.x, m.vx, m.sprite, m.collision_mask = m.x + 7, -m.vx, 0, 0x400000
  m.damp = "not a number"
end)
"#);
        park_lua(run_mods(&[pack]).unwrap().lua);
        let motion = game::entity::Motion {
            id: 0xF000_0002, class_type: 0, damp: 0x10000, wind_scale: 3, gravity_scale: 4,
            moved: 1, x: 100, y: 200, vx: 5, vy: -6, cm_gravity: 0, cm_wind: 0,
        };
        let run = || {
            let (mut sprite, mut mask) = (48, 0x7C);
            let pre = missile_frame(motion, 2, "pre", Some(6), &mut sprite, &mut mask).unwrap();
            let post = missile_frame(motion, 2, "post", Some(6), &mut sprite, &mut mask).unwrap();
            (pre, post, sprite, mask, parked_seen())
        };
        let fast = run();
        perf::enable(PerfFlags { counter: false, profile: true, packs: true, spikes: false });
        let slow = run();
        perf::reset();
        assert_eq!(fast, slow);
        let (pre, post, sprite, mask, seen) = fast;
        assert_eq!(pre, motion, "an untouched frame comes back as it went");
        assert_eq!((post.x, post.vx, post.damp, post.y), (107, -5, 0x10000, 200));
        assert_eq!((sprite, mask), (0, 0x400000));
        assert_eq!(seen, ["pre:grenade:2", "post:grenade:2"]);
    }

    #[test]
    fn worm_message_options_are_checked() {
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        let f = "local f = function() end ";
        for bad in [
            "wa.on.worm_message(f, {})",
            "wa.on.worm_message(f, { msgs = {} })",
            "wa.on.worm_message(f, { msgs = { 'jump' } })",
            "wa.on.worm_message(f, { msgs = { 2 }, also = 1 })",
            "wa.on.hud(f, { msgs = { 2 } })",
        ] {
            assert!(lua.load(format!("{f}{bad}")).exec().is_err(), "{bad}");
        }
        lua.load(format!("{f}wa.on.worm_message(f, {{ msgs = {{ 2, 2, 36 }} }})")).exec().unwrap();
    }

    #[test]
    fn missile_frame_names_the_missiles_weapon() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-missile-weapon");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("m"),
            "m.obs",
            r#"
id = "m.obs"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = []
"#,
            r#"
wa.on.missile_frame(function(m)
  if m.weapon == "longbow" then m.vx = 1
  elseif m.weapon == nil then m.vx = 2 end
  if m.weapon == "super_banana" and m.sprite == 51 then m.sprite = 0 end
  if m.weapon == "cluster_bomb" and m.collision_mask == 0x400000 then m.collision_mask = 0x40007C end
end)
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        park_lua(scripts.lua);
        let m = game::entity::Motion {
            id: 1, class_type: 0, damp: 0, wind_scale: 0, gravity_scale: 0, moved: 0,
            x: 0, y: 0, vx: 0, vy: 0, cm_gravity: 0, cm_wind: 0,
        };
        let mut mask = 0x400000;
        let mut sprite = 48;
        assert_eq!(missile_frame(m, 2, "pre", Some(15), &mut sprite, &mut mask).unwrap().vx, 1, "slot 15 is the longbow");
        assert_eq!(missile_frame(m, 2, "pre", None, &mut sprite, &mut mask).unwrap().vx, 2, "no slot is nil");
        assert_eq!(sprite, 48, "left alone");
        let mut sprite = 51;
        missile_frame(m, 2, "pre", Some(42), &mut sprite, &mut mask).unwrap();
        assert_eq!(sprite, 0, "m.sprite = 0 hides it");
        // Project X's Swim Suit: a missile in the water also hits worms.
        assert_eq!(mask, 0x400000, "left alone");
        let mut mask = 0x400000;
        let mut sprite = 1;
        missile_frame(m, 2, "pre", Some(7), &mut sprite, &mut mask).unwrap();
        assert_eq!(mask, 0x40007C);
    }

    #[test]
    fn draw_hook_collects_quads_and_keeps_out_of_the_simulation() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-draw");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("d"),
            "d.draw",
            r#"
id = "d.draw"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = []
"#,
            r#"
outside = pcall(wa.draw.quad, {})
wa.on.draw(function()
  local P = 0x10000
  wa.draw.quad({
    points = { { 10 * P, 10 * P }, { 110 * P, 10 * P }, { 110 * P, 12 * P }, { 10 * P, 12 * P } },
    colors = { 0xFFFF3030, 0xFFFF3030, 0x10FF3030, 0x10FF3030 },
    sprite = "grenade", blend = "add",
  })
  sim_random = pcall(wa.random, 1, 6)
  local r = wa.draw.random()
  looks = r >= 0 and r < 1
end)
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        park_lua(scripts.lua);
        let quads = run_draw().unwrap();
        assert_eq!(quads.len(), 1);
        let q = quads[0];
        assert_eq!(q.corners[1], (110.0, 10.0), "16.16 in, pixels out");
        assert_eq!(q.colours, [0xFFFF3030, 0xFFFF3030, 0x10FF3030, 0x10FF3030]);
        assert!(q.additive);
        assert_eq!(q.depth, 0x1_0000, "in front of terrain by default");
        assert!(!drawing(), "the draw context ends with the hook");
        PARKED_LUA.with(|slot| {
            let guard = slot.borrow();
            let g = guard.as_ref().unwrap().globals();
            assert!(!g.get::<bool>("outside").unwrap(), "wa.draw.quad outside wa.on.draw");
            assert!(!g.get::<bool>("sim_random").unwrap(), "wa.random inside wa.on.draw");
            assert!(g.get::<bool>("looks").unwrap(), "wa.draw.random in 0..1");
        });
    }

    #[test]
    fn contact_reaches_lua_with_the_speed_before_impact() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-contact");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("c"),
            "c.contact",
            r#"
id = "c.contact"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = []
"#,
            r#"
wa.on.contact(function(worm, other, c)
  seen = { vx = c.vx, vy = c.vy, flags = c.flags, kind = other.kind, worm = worm ~= nil }
end)
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        park_lua(scripts.lua);
        contact(Contact { worm: 1, other: 0, flags: 3, vx: 0x2_0000, vy: -0x1_0000 }).unwrap();
        PARKED_LUA.with(|slot| {
            let guard = slot.borrow();
            let seen: mlua::Table = guard.as_ref().unwrap().globals().get("seen").unwrap();
            assert_eq!(seen.get::<i32>("vx").unwrap(), 0x2_0000);
            assert_eq!(seen.get::<i32>("vy").unwrap(), -0x1_0000);
            assert_eq!(seen.get::<u32>("flags").unwrap(), 3);
            assert_eq!(seen.get::<String>("kind").unwrap(), "none");
            assert!(seen.get::<bool>("worm").unwrap());
        });
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
            attacker: 0,
            slot: None,
            source_weapon: None,
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
            attacker: 0,
            slot: None,
            source_weapon: None,
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
    fn rule_events_dispatch_composable_context() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-rule-events");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("rules"),
            "rules.events",
            r#"
id = "rules.events"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = []
"#,
            r#"
seen = { phases = {} }
wa.on.turn_start(function(event)
  seen.turn = event.worm ~= nil and event.active ~= nil
end)
wa.on.turn_retreat(function(event)
  if event.worm ~= nil and event.active ~= nil then
    seen.phases[#seen.phases + 1] = "retreat"
  end
end)
wa.on.turn_end(function(event)
  if event.worm ~= nil and event.active ~= nil then
    seen.phases[#seen.phases + 1] = "end"
  end
end)
wa.on.turn_cleanup(function(event)
  if event.worm ~= nil and event.active ~= nil then
    seen.phases[#seen.phases + 1] = "cleanup"
  end
end)
wa.on.death(function(event)
  seen.death = event.victim ~= nil
    and event.active ~= nil
    and event.attacker ~= nil
    and event.attacker_team == 2
    and event.weapon == "bazooka"
    and event.kind == "blast"
end)
wa.on.hurt(function(event)
  seen.hurt = event.attacker ~= nil
    and event.attacker_team == 2
    and event.by == 2
    and event.weapon == "bazooka"
    and event.source_weapon == "armageddon"
end)
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        park_lua(scripts.lua);

        turn_start(1).unwrap();
        turn_retreat(1).unwrap();
        turn_end(1).unwrap();
        turn_cleanup(1).unwrap();
        hurt(Hurt {
            worm: 1,
            from: 2,
            by: Some(2),
            attacker: 2,
            slot: Some(1),
            source_weapon: Some(56),
            kind: "blast",
            amount: 20,
            lost: 20,
            hp: 80,
            phase: "post",
        })
        .unwrap();
        death(Death {
            victim: 1,
            active: 3,
            attacker: 2,
            attacker_team: Some(2),
            slot: Some(1),
            kind: Some("blast"),
        })
        .unwrap();

        PARKED_LUA.with(|slot| {
            let guard = slot.borrow();
            let lua = guard.as_ref().unwrap();
            let seen: mlua::Table = lua.globals().get("seen").unwrap();
            assert!(seen.get::<bool>("turn").unwrap());
            let phases: mlua::Table = seen.get("phases").unwrap();
            let phases: Vec<String> = phases.sequence_values().collect::<Result<_, _>>().unwrap();
            assert_eq!(phases, ["retreat", "end", "cleanup"]);
            assert!(seen.get::<bool>("hurt").unwrap());
            assert!(seen.get::<bool>("death").unwrap());
        });
    }

    #[test]
    fn transport_verbs_validate_specs_and_require_simulation() {
        let _guard = lock_host();
        actor::bind_actor_native(ActorNative {
            warp: |_, spec| {
                // Native calls may reenter Lua: the binding must have released
                // NATIVE before calling the engine callback.
                let _keys = actor::native_keys().unwrap()();
                assert_eq!(spec.x, 123); assert_eq!(spec.y, 456);
                assert_eq!(spec.vx, Some(789)); assert_eq!(spec.vy, None);
                true
            },
            air_fire: |_, on| on,
            body_uid: |_| 7,
            body_live: |_, id| id == 7,
            actor_fits: |_, x, y| x == 100 && y == 200,
            fits: |_, x, y| x == 200 && y == 100,
            ..Default::default()
        });
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        lua.globals().set("w", actor::WormHandle(0)).unwrap();
        lua.globals().set("wf", actor::WormHandle(0x1234)).unwrap();
        lua.globals().set("o", actor::WorldHandle::new(0)).unwrap();
        lua.load(r#"
            assert(not pcall(function() w:warp{x=123,y=456} end))
            assert(not pcall(function() w:air_fire() end))
            assert(o:fits(100,200) and not o:fits(200,100))
            assert(wf:fits(200,100) and not wf:fits(100,200))
            assert(wf:fits(100,200,"terrain") and not wf:fits(200,100,"terrain"))
            assert(o:fits(100,200,"terrain"))
            assert(not pcall(function() wf:fits(100,200,"bogus") end))
            assert(o.uid==7)
        "#).exec().unwrap();
        sim_context::with_sim(|| lua.load(r#"
            assert(w:warp{x=123,y=456,vx=789,facing=-1})
            assert(o:warp{x=123,y=456,vx=789})
            assert(w:air_fire() and not w:air_fire(false))
            assert(not pcall(function() w:warp{x=1,y=2,foo=3} end))
            assert(not pcall(function() w:warp{x=1,y=2,facing=0} end))
        "#).exec()).unwrap();
    }

    #[test]
    fn body_move_filters_keep_registration_order_and_pack_context() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-body-move");
        let pack = filter_pack(&root, r#"
          seen = {}
          wa._mod = "first"
          wa.on.body_move(function(b) seen[#seen+1] = wa._mod .. ":" .. b.kind end, {kinds={"mine"}})
          wa._mod = "second"
          wa.on.body_move(function(b) seen[#seen+1] = wa._mod .. ":" .. b.kind end, {kinds={"arrow", "mine"}})
          assert(not pcall(function() wa.on.body_move(function() end, {kinds={"bogus"}}) end))
        "#);
        park_lua(run_mods(&[pack]).unwrap().lua);
        assert!(body_move_wanted(game::offsets::CLASS_MINE));
        assert!(!body_move_wanted(game::offsets::CLASS_WORM));
        let other = filter_pack(&root.join("other"), "-- no hooks");
        let _second = run_mods(&[other]).unwrap();
        assert!(body_move_wanted(game::offsets::CLASS_MINE), "unparked Lua must not replace the running filters");
        let mut raw = vec![0i32; 0x200 / 4];
        let p = raw.as_mut_ptr() as *mut u8;
        unsafe { game::entity::write_i32(p, game::offsets::ENT_CLASS_TYPE, game::offsets::CLASS_MINE) };
        body_move(p as u32, game::offsets::CLASS_MINE).unwrap();
        PARKED_LUA.with(|slot| {
            let lua = slot.borrow(); let lua = lua.as_ref().unwrap();
            let seen: mlua::Table = lua.globals().get("seen").unwrap();
            assert_eq!(seen.get::<String>(1).unwrap(), "first:mine");
            assert_eq!(seen.get::<String>(2).unwrap(), "second:mine");
        });
    }

    #[test]
    fn highlander_portals_cover_transport_controls_and_air_firing() {
        let lua = Lua::new();
        lua.globals().set("HIGHLANDER_ROOT", concat!(env!("CARGO_MANIFEST_DIR"), "/../../mods/examples/highlander/")).unwrap();
        lua.load(include_str!("../../../mods/examples/highlander/test_portals.lua"))
            .set_name("highlander/test_portals.lua").exec().unwrap();
    }

    #[test]
    fn portal_shot_moves_hits_and_draws_through_real_bindings() {
        let _host = lock_host();
        use game::{entity::{read_i32, write_i32}, offsets::*};
        use std::cell::{Cell, RefCell};
        const P: i32 = 0x10000;
        thread_local! {
            static BODIES: RefCell<Vec<(Vec<u8>, u32)>> = const { RefCell::new(Vec::new()) };
            static WORM: Cell<u32> = const { Cell::new(0) };
            static SOUNDS: Cell<usize> = const { Cell::new(0) };
        }
        BODIES.with(|v| v.borrow_mut().clear());
        SOUNDS.with(|v| v.set(0));
        let mut worm = vec![0u8; 0x1000];
        let h = worm.as_mut_ptr() as u32;
        unsafe {
            for (off, value) in [(ENT_POS_X, 100*P), (ENT_POS_Y, 100*P),
                (WORM_TURN_SIDE, 1), (WORM_AIM_ANGLE, 0x7fff),
                (WORM_SELECTED_WEAPON, weapons::StockWeapon::ScalesOfJustice.index() as i32),
                (WORM_STATE, 0x67), (WORM_TURN_ACTIVE, 1)] {
                write_i32(worm.as_mut_ptr(), off, value);
            }
        }
        WORM.with(|v| v.set(h));
        actor::bind_actor_native(ActorNative {
            map_size: || (2000, 1000),
            water_y: || 1000*P,
            list_worms: |_| WORM.with(|v| vec![v.get()]),
            resolve_sprite: |_| Some((10001, 0, 100, 100)),
            resolve_sound: |_| 7,
            fire_power: |_| P,
            ammo: |_, _, _| 3,
            play_sound: |target, id, _| {
                WORM.with(|v| assert_eq!(target, v.get()));
                assert_eq!(id, 7);
                SOUNDS.with(|v| v.set(v.get()+1));
            },
            spawn: |args| BODIES.with(|v| {
                let mut bodies = v.borrow_mut();
                let mut body = vec![0u8; LUA_ACTOR_SIZE];
                let h = body.as_mut_ptr() as u32;
                unsafe {
                    for (off, value) in [(ACTOR_MAGIC, LUA_ACTOR_MAGIC as i32),
                        (ACTOR_SEQ, bodies.len() as i32+1), (ACTOR_OWNER, args.owner as i32),
                        (ENT_POS_X, args.x), (ENT_POS_Y, args.y)] {
                        write_i32(body.as_mut_ptr(), off, value);
                    }
                }
                actor::actor_spawned(h, args.spawn.brain_id);
                bodies.push((body, args.spawn.brain_id));
                Ok(h)
            }),
            despawn: |h| unsafe { write_i32(h as *mut u8, ACTOR_DEAD, 1) },
            land_trace: |x, y, _, _| Some((x, y)),
            shape_trace: |q| {
                assert_eq!(q.with, actor::SHAPE_TERRAIN);
                assert_eq!(q.radius, 0);
                if q.x0 < 200*P && q.x1 >= 200*P {
                    actor::ShapeHit { hit: true, x: 199*P, y: q.y1, nx: -1.0,
                        ..Default::default() }
                } else { actor::ShapeHit::default() }
            },
            ..Default::default()
        });
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/examples/highlander");
        let pack = LoadedMod {
            manifest: parse_manifest(&std::fs::read_to_string(root.join("mod.toml")).unwrap()).unwrap(),
            root,
        };
        let scripts = run_mods(&[pack]).unwrap();
        let fire = scripts.resolved.applied.iter()
            .find(|c| c.slot == weapons::StockWeapon::ScalesOfJustice).unwrap().params.on_fire_id;
        park_lua(scripts.lua);
        actor::weapon_fire(fire, h, weapons::StockWeapon::ScalesOfJustice.index() as i32,
            100*P, 100*P, 0, 0).unwrap();
        let (bolt, brain) = BODIES.with(|v| {
            let mut bodies = v.borrow_mut();
            assert_eq!(bodies.len(), 2, "manager and visible bolt");
            (bodies[1].0.as_mut_ptr() as u32, bodies[1].1)
        });
        let initial_x = unsafe { read_i32(bolt as *mut u8, ENT_POS_X) };
        actor::actor_frame(bolt, brain, ActorEvent::default()).unwrap();
        let moved_x = unsafe { read_i32(bolt as *mut u8, ENT_POS_X) };
        assert!((moved_x-initial_x-35*P).abs() < 10, "bolt advances at full power");
        actor::actor_frame(bolt, brain, ActorEvent::default()).unwrap();
        actor::actor_frame(bolt, brain, ActorEvent::default()).unwrap();
        assert_eq!(unsafe { read_i32(bolt as *mut u8, ACTOR_DEAD) }, 1, "terrain impact removes bolt");
        SOUNDS.with(|v| assert_eq!(v.get(), 2, "shot and successful portal impact"));
        let quads = run_draw().unwrap();
        let endpoint = |quads: &[dispatch::DrawQuad]| quads.iter().any(|q| {
            let x = q.corners.iter().map(|p| p.0).sum::<f32>() / 4.0;
            let y = q.corners.iter().map(|p| p.1).sum::<f32>() / 4.0;
            q.colours == [0xFFFFFFFF; 4] && q.additive
                && (x-199.0).abs() < 0.01 && (y-100.0).abs() < 0.01
        });
        assert!(endpoint(&quads), "first endpoint is bright at the impact before a partner exists");
        for q in quads {
            for (x, y) in q.corners {
                assert!(x.abs() < 2000.0 && y.abs() < 1000.0, "all effects stay on the map");
            }
        }
        unsafe { write_i32(worm.as_mut_ptr(), WORM_TURN_ACTIVE, 0) };
        turn_end(h).unwrap();
        let (manager, manager_brain) = BODIES.with(|v| {
            let mut bodies = v.borrow_mut();
            (bodies[0].0.as_mut_ptr() as u32, bodies[0].1)
        });
        for _ in 0..160 {
            actor::actor_frame(manager, manager_brain, ActorEvent::default()).unwrap();
        }
        assert!(endpoint(&run_draw().unwrap()), "endpoint still draws after retreat and effect expiry");
        actor::reset();
        actor::bind_actor_native(ActorNative::default());
        PARKED_LUA.with(|slot| *slot.borrow_mut() = None);
        WORM.with(|v| v.set(0));
        BODIES.with(|v| v.borrow_mut().clear());
    }

    #[test]
    fn highlander_uses_public_inventory_and_rule_events() {
        let lua = Lua::new();
        let rules = include_str!("../../../mods/examples/highlander/rules.lua");
        let dofile = lua
            .create_function(move |lua, _: String| {
                lua.load(rules).set_name("highlander/rules.lua").exec()
            })
            .unwrap();
        lua.globals().set("dofile", dofile).unwrap();
        // The suite compares against baseline.txt, which it cannot open from
        // the crate's folder.
        lua.globals()
            .set("HIGHLANDER_BASELINE", include_str!("../../../mods/examples/highlander/baseline.txt"))
            .unwrap();
        // rules.lua requires the pack's modules, which the suite loads from
        // the pack's folder.
        lua.globals()
            .set("HIGHLANDER_ROOT", concat!(env!("CARGO_MANIFEST_DIR"), "/../../mods/examples/highlander/"))
            .unwrap();
        let os: mlua::Table = lua.globals().get("os").unwrap();
        os.set(
            "exit",
            lua.create_function(|_, code: i32| {
                Err::<(), _>(mlua::Error::external(format!(
                    "Highlander mock suite failed with exit {code}"
                )))
            })
            .unwrap(),
        )
        .unwrap();
        lua.load(include_str!(
            "../../../mods/examples/highlander/test_rules.lua"
        ))
        .set_name("highlander/test_rules.lua")
        .exec()
        .unwrap();
    }

    #[test]
    fn highlander_pure_lua_reference_still_matches() {
        let lua = Lua::new();
        let rules = include_str!("../../../mods/examples/highlander_pure_lua/rules.lua");
        let dofile = lua
            .create_function(move |lua, _: String| {
                lua.load(rules)
                    .set_name("highlander_pure_lua/rules.lua")
                    .exec()
            })
            .unwrap();
        lua.globals().set("dofile", dofile).unwrap();
        let os: mlua::Table = lua.globals().get("os").unwrap();
        os.set(
            "exit",
            lua.create_function(|_, code: i32| {
                Err::<(), _>(mlua::Error::external(format!(
                    "Pure-Lua Highlander suite failed with exit {code}"
                )))
            })
            .unwrap(),
        )
        .unwrap();
        lua.load(include_str!(
            "../../../mods/examples/highlander_pure_lua/test_rules.lua"
        ))
        .set_name("highlander_pure_lua/test_rules.lua")
        .exec()
        .unwrap();
    }

    #[test]
    fn actors_kind_is_namespaced() {
        let _guard = lock_host();
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
    fn pack_sheet_is_loaded_at_replace_not_looked_up() {
        let _guard = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-pxgif");
        let _ = std::fs::remove_dir_all(&root);
        let png = include_bytes!("../../gfx/tests/fixtures/ball-8.png");
        let pack = pack(
            &root.join("p"),
            "user.my_weapon",
            r#"
id = "user.my_weapon"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["mine"]
"#,
            r#"
wa.weapons.replace({
  weapon = "mine",
  name = "PX gif",
  copy_from = "mine",
  sprite = "sprites/cweps_gfx__swormb.png",
  on_explode = {
    spawn = {
      kind = "cluster",
      sprite = "sprites/cweps_gfx__swormb.png",
      count = 5,
      spread = 45,
      power = 655360,
      impact = true,
    },
  },
})
"#,
        );
        std::fs::create_dir_all(pack.root.join("sprites")).unwrap();
        std::fs::write(pack.root.join("sprites/cweps_gfx__swormb.png"), png).unwrap();
        let scripts = run_mods(&[pack]).unwrap();
        let c = &scripts.resolved.applied[0];
        assert!(c.params.sprite_sheet.is_some());
        assert_eq!(
            c.params.sprite_path.as_deref(),
            Some("sprites/cweps_gfx__swormb.png")
        );
        let boom = c.params.explode_spawn.as_ref().expect("explode cluster");
        assert_eq!(
            boom.path.as_deref(),
            Some("sprites/cweps_gfx__swormb.png")
        );
        assert!(c
            .params
            .extra_sheets
            .iter()
            .any(|(p, _)| p == "sprites/cweps_gfx__swormb.png"));
    }

    #[test]
    fn png_strip_loads_from_pack() {
        let _guard = lock_host();
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
        let png = include_bytes!("../../gfx/tests/fixtures/ball-8.png");
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
        let _guard = lock_host();
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
    fn mask_spawn_toggles_parse() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-mask");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("m"),
            "m.mask",
            r#"
id = "m.mask"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["prod"]
"#,
            r#"
wa.weapons.replace({
  weapon = "prod",
  on_click = {
    spawn = {
      mask = true,
      mask_worms = false,
      mask_weapons = true,
      from = "cursor",
      gravity = false,
      collide = false,
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
        assert!(spawn.mask);
        assert!(!spawn.mask_worms);
        assert!(spawn.mask_land); // defaults to mask
        assert!(spawn.mask_weapons);
        assert!(spawn.mask_actors); // defaults to mask
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
        // As the engine does for every actor it spawns.
        crate::actor_spawned(handle, spawn.brain_id);
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
    fn a_freed_actors_handle_reads_and_writes_nothing() {
        use game::entity::{read_i32, write_i32};
        use game::offsets::{ACTOR_MAGIC, ENT_POS_X, ENT_POS_Y, LUA_ACTOR_MAGIC, LUA_ACTOR_SIZE};
        let _host = lock_host();
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        let mut body = vec![0u8; LUA_ACTOR_SIZE];
        let handle = body.as_mut_ptr() as u32;
        unsafe {
            write_i32(body.as_mut_ptr(), ACTOR_MAGIC, LUA_ACTOR_MAGIC as i32);
            write_i32(body.as_mut_ptr(), ENT_POS_X, 0x50000);
        }
        crate::actor_spawned(handle, 0);
        lua.globals().set("A", lua.create_userdata(actor::ActorHandle(handle)).unwrap()).unwrap();
        sim_context::with_sim(|| {
            let x: i64 = lua.load("return A.x").eval().unwrap();
            assert_eq!(x, 0x50000, "alive, it reads through");
            crate::actor_freed(handle);
            // WA reuses the memory: the handle must leave it alone.
            unsafe { write_i32(body.as_mut_ptr(), ENT_POS_X, 0x1234) };
            lua.load("A.x = 0x70000; A.y = 0x80000; A.vx = 1").exec().unwrap();
            assert_eq!(lua.load("return A.x").eval::<i64>().unwrap(), 0, "freed, it reads nothing");
        });
        assert_eq!(unsafe { read_i32(body.as_mut_ptr(), ENT_POS_X) }, 0x1234, "and writes nothing");
        assert_eq!(unsafe { read_i32(body.as_mut_ptr(), ENT_POS_Y) }, 0);
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
        let _guard = lock_host();
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
        crate::weapon_click(p.on_click_id, 0x11, 5, 1, 2, -0x00D0_0000, (-0x5_0000, 0)).unwrap();
        assert_eq!(OWNER.with(|c| c.get()), 0x11);
    }

    #[test]
    fn an_actor_takes_the_missile_shape_and_fits_by_it() {
        let _host = lock_host();
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        let spec = |src: &str| -> mlua::Table { lua.load(src).eval().unwrap() };
        assert!(crate::spawn::parse_spawn(&lua, &spec("return { shape = 'missile' }")).unwrap().missile_shape);
        assert!(!crate::spawn::parse_spawn(&lua, &spec("return {}")).unwrap().missile_shape, "a worm's by default");
        assert!(crate::spawn::parse_spawn(&lua, &spec("return { shape = 'cube' }")).is_err());
        use std::cell::Cell;
        thread_local! {
            static ASKED: Cell<(u32, i32, i32)> = const { Cell::new((0, 0, 0)) };
        }
        crate::bind_actor_native(crate::ActorNative {
            actor_fits: |h, x, y| {
                ASKED.with(|c| c.set((h, x, y)));
                x < 100
            },
            ..Default::default()
        });
        crate::actor_spawned(0x5000, 0);
        let a = lua.create_userdata(crate::actor::ActorHandle(0x5000)).unwrap();
        lua.globals().set("A", a).unwrap();
        assert!(lua.load("return A:fits(50, 7)").eval::<bool>().unwrap());
        assert!(!lua.load("return A:fits(150, 7)").eval::<bool>().unwrap(), "land there");
        assert_eq!(ASKED.with(|c| c.get()), (0x5000, 150, 7), "WA's test, for this actor, at that place");
        crate::actor_freed(0x5000);
        assert!(lua.load("return A:fits(150, 7)").eval::<bool>().unwrap(), "a freed actor touches nothing");
    }

    #[test]
    fn an_actor_brain_takes_its_hits() {
        let _host = lock_host();
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        let spec: mlua::Table = lua
            .load(r#"return { on = {
                frame = function() end,
                hit = function(a, hit)
                    GOT = { hit.kind, hit.hit_kind, hit.x, hit.y, hit.vx, hit.vy, hit.damage, hit.team }
                end,
            } }"#)
            .eval()
            .unwrap();
        let with_hit = crate::spawn::parse_brain(&lua, &spec).unwrap();
        let plain: mlua::Table = lua.load("return { on = function() end }").eval().unwrap();
        let without = crate::spawn::parse_brain(&lua, &plain).unwrap();
        let as_object: mlua::Table = lua.load("return { as_object = true }").eval().unwrap();
        assert!(crate::spawn::parse_spawn(&lua, &as_object).unwrap().as_object);
        assert!(!crate::spawn::parse_spawn(&lua, &plain).unwrap().as_object, "missiles' layer by default");
        crate::park_lua(lua);
        assert!(crate::actor_takes_hits(with_hit));
        assert!(!crate::actor_takes_hits(without), "a brain without hit leaves hits to WA");
        assert!(!crate::actor_takes_hits(0));
        // A fake actor: the handler reads its owner field.
        let mut body = vec![0u8; 0x400];
        let handle = body.as_mut_ptr() as u32;
        let hit = crate::ActorHit { blast: false, kind: 1, x: 10, y: 20, vx: 0x8000, vy: -0x10000, damage: 7, team: 2 };
        crate::actor_hit(handle, with_hit, hit).unwrap();
        let (kind, ints): (String, Vec<i64>) = PARKED_LUA.with(|slot| {
            let guard = slot.borrow();
            let lua = guard.as_ref().unwrap();
            let got: mlua::Table = lua.globals().get("GOT").unwrap();
            let kind: String = got.get(1).unwrap();
            let ints = (2..=8).map(|i| got.get::<i64>(i).unwrap()).collect();
            (kind, ints)
        });
        assert_eq!(kind, "hit");
        assert_eq!(ints, vec![1, 10, 20, 0x8000, -0x10000, 7, 2], "a fire's hit, as WA sent it");
        drop(body);
    }

    #[test]
    fn weapons_click_runs_the_claims_on_click() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-weapons-click");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("s"),
            "s.strike",
            r#"
id = "s.strike"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["air_strike"]
"#,
            r#"
wa.weapons.replace({
  weapon = "air_strike",
  on_select = { cursor = true },
  on_click = function(ev)
    CLICKED = { ev.x, ev.y, ev.sky, ev.slot, ev.vx, ev.vy, ev.worm ~= nil, wa._mod }
  end,
})
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        let id = scripts.resolved.applied[0].params.on_click_id;
        assert!(id != 0);
        let lua = scripts.lua;
        use std::sync::atomic::{AtomicU32, Ordering};
        static CLICK: AtomicU32 = AtomicU32::new(0);
        CLICK.store(id, Ordering::SeqCst);
        crate::bind_actor_native(crate::ActorNative {
            list_worms: |_| vec![0x1234],
            slot_click: |slot| {
                if slot == weapons::StockWeapon::AirStrike.index() as u32 { CLICK.load(Ordering::SeqCst) } else { 0 }
            },
            ..Default::default()
        });
        let click = "wa.weapons.click{ weapon = 'air_strike', worm = wa.worms.all()[1], x = 0x100000, y = 0x200000, vx = -0x50000 }";
        assert!(lua.load(click).exec().is_err(), "outside a simulation callback");
        sim_context::with_sim(|| {
            lua.load("wa._mod = 'caller'").exec().unwrap();
            lua.load(click).exec().unwrap();
            let err = |src: &str| lua.load(src).exec().unwrap_err().to_string();
            assert!(err("wa.weapons.click{ weapon = 'mail_strike', worm = wa.worms.all()[1], x = 0, y = 0 }")
                .contains("no pack claims mail_strike"));
            assert!(err("wa.weapons.click{ weapon = 'air_strike', x = 0, y = 0 }").contains("needs worm"));
            assert!(err("wa.weapons.click{ weapon = 'air_strike', worm = wa.worms.all()[1], x = 0 }").contains("needs y"));
        });
        let got: Vec<mlua::Value> = lua.load("return table.unpack(CLICKED)").eval::<mlua::MultiValue>().unwrap().into_vec();
        let int = |i: usize| got[i].as_i64().unwrap();
        assert_eq!((int(0), int(1), int(2)), (0x100000, 0x200000, -0x00D0_0000), "the click and WA's sky");
        assert_eq!(int(3), weapons::StockWeapon::AirStrike.index() as i64);
        assert_eq!((int(4), int(5)), (-0x50000, 0), "the shot's speed as given, vy 0 by default");
        assert_eq!(got[6].as_boolean(), Some(true), "for the worm given");
        assert_eq!(got[7].as_string().unwrap().to_str().unwrap(), "s.strike", "it runs as its own pack");
        assert_eq!(lua.load("return wa._mod").eval::<String>().unwrap(), "caller", "and the caller's comes back");
    }

    #[test]
    fn weapons_fire_runs_the_claims_on_fire() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-weapons-fire");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("f"),
            "f.magnet",
            r#"
id = "f.magnet"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["baseball_bat"]
"#,
            r#"
wa.weapons.replace({
  weapon = "baseball_bat",
  on_fire = function(fire)
    FIRED = { fire.x, fire.y, fire.vx, fire.vy, fire.slot, fire.polarity, fire.weapon == nil, fire.worm ~= nil, wa._mod }
  end,
})
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        let id = scripts.resolved.applied[0].params.on_fire_id;
        assert!(id != 0);
        let lua = scripts.lua;
        use std::sync::atomic::{AtomicU32, Ordering};
        static FIRE: AtomicU32 = AtomicU32::new(0);
        FIRE.store(id, Ordering::SeqCst);
        crate::bind_actor_native(crate::ActorNative {
            list_worms: |_| vec![0x1234],
            slot_fire: |slot| {
                if slot == weapons::StockWeapon::BaseballBat.index() as u32 { FIRE.load(Ordering::SeqCst) } else { 0 }
            },
            ..Default::default()
        });
        let fire = "wa.weapons.fire{ weapon = 'baseball_bat', worm = wa.worms.all()[1], x = 0x100000, y = 0x200000, \
                    vx = 3, polarity = -1 }";
        assert!(lua.load(fire).exec().is_err(), "outside a simulation callback");
        sim_context::with_sim(|| {
            lua.load("wa._mod = 'caller'").exec().unwrap();
            lua.load(fire).exec().unwrap();
            let err = |src: &str| lua.load(src).exec().unwrap_err().to_string();
            assert!(err("wa.weapons.fire{ weapon = 'mail_strike', worm = wa.worms.all()[1], x = 0, y = 0 }")
                .contains("no pack claims mail_strike"));
            assert!(err("wa.weapons.fire{ weapon = 'baseball_bat', x = 0, y = 0 }").contains("needs worm"));
            assert!(err("wa.weapons.fire{ weapon = 'baseball_bat', worm = wa.worms.all()[1], x = 0 }")
                .contains("needs y"));
        });
        let got: Vec<mlua::Value> = lua.load("return table.unpack(FIRED)").eval::<mlua::MultiValue>().unwrap().into_vec();
        let int = |i: usize| got[i].as_i64().unwrap();
        assert_eq!((int(0), int(1), int(2), int(3)), (0x100000, 0x200000, 3, 0), "the shot, vy 0 by default");
        assert_eq!(int(4), weapons::StockWeapon::BaseballBat.index() as i64);
        assert_eq!(int(5), -1, "the spec's other fields reach the handler");
        assert_eq!(got[6].as_boolean(), Some(true), "but not the slot's name");
        assert_eq!(got[7].as_boolean(), Some(true), "for the worm given");
        assert_eq!(got[8].as_string().unwrap().to_str().unwrap(), "f.magnet", "it runs as its own pack");
        assert_eq!(lua.load("return wa._mod").eval::<String>().unwrap(), "caller", "and the caller's comes back");
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

    /// A missile can burst while another pack launches it (started inside
    /// the land): its on_explode runs as its own pack inside that call, and
    /// the launching pack must be the running one again after it.
    #[test]
    fn a_weapon_step_leaves_the_callers_pack_running() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-nested-step");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("n"),
            "n.burst",
            r#"
id = "n.burst"
api_version = 1
entry = "weapons.lua"
[weapons]
replace = ["grenade"]
"#,
            r#"
wa.weapons.replace({ weapon = "grenade", on_explode = function() burst_ran_as = wa._mod end })
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        let id = scripts.resolved.applied[0].params.on_explode_id;
        assert!(id != 0);
        scripts.lua.load(r#"wa._mod = "n.launcher""#).exec().unwrap();
        park_lua(scripts.lua);
        crate::weapon_emit("_explodes", id, "on_explode", 0, 5, 0, 0, 0).unwrap();
        let (ran_as, after): (String, String) = PARKED_LUA.with(|slot| {
            let guard = slot.borrow();
            let lua = guard.as_ref().unwrap();
            let wa: mlua::Table = lua.globals().get("wa").unwrap();
            (lua.globals().get("burst_ran_as").unwrap(), wa.get("_mod").unwrap())
        });
        assert_eq!(ran_as, "n.burst");
        assert_eq!(after, "n.launcher");
    }

    #[test]
    fn a_pack_turns_on_the_perf_counter_and_profiler() {
        let _host = lock_host();
        let root = std::env::temp_dir().join("wklua-lua-host-perf");
        let _ = std::fs::remove_dir_all(&root);
        let manifest = |id: &str| {
            format!("id = \"{id}\"\napi_version = 1\nentry = \"weapons.lua\"\n")
        };
        let counter = pack(
            &root.join("c"),
            "p.counter",
            &manifest("p.counter"),
            "wa.perf.enable({ counter = true })",
        );
        run_mods(&[counter]).unwrap();
        assert_eq!(perf_flags(), PerfFlags { counter: true, profile: false, packs: false, spikes: false });
        let both = pack(&root.join("b"), "p.both", &manifest("p.both"), "wa.perf.enable()");
        run_mods(&[both]).unwrap();
        assert_eq!(perf_flags(), PerfFlags { counter: true, profile: true, packs: false, spikes: false });
        let none = pack(&root.join("n"), "p.none", &manifest("p.none"), "");
        run_mods(&[none]).unwrap();
        assert_eq!(perf_flags(), PerfFlags::default(), "a new match's packs decide afresh");
        let typo = pack(
            &root.join("t"),
            "p.typo",
            &manifest("p.typo"),
            "wa.perf.enable({ countr = true })",
        );
        assert!(run_mods(&[typo]).is_err(), "an unknown option is an error");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn world_sets_gravity_and_wind_only_in_the_simulation() {
        let _host = lock_host();
        use std::sync::atomic::{AtomicI32, Ordering};
        static GRAVITY: AtomicI32 = AtomicI32::new(0);
        static WIND: AtomicI32 = AtomicI32::new(0);
        crate::bind_actor_native(crate::ActorNative {
            set_gravity: |v| GRAVITY.store(v, Ordering::SeqCst),
            set_wind: |v| WIND.store(v, Ordering::SeqCst),
            ..Default::default()
        });
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        assert!(lua.load("wa.world.set_wind(0x1000)").exec().is_err(), "outside a simulation callback");
        sim_context::with_sim(|| {
            lua.load("wa.world.set_gravity(0x3AE * 2); wa.world.set_wind(-0x81D)").exec().unwrap();
        });
        crate::bind_actor_native(crate::ActorNative::default());
        assert_eq!((GRAVITY.load(Ordering::SeqCst), WIND.load(Ordering::SeqCst)), (0x3AE * 2, -0x81D));
    }

    #[test]
    fn simulation_writes_are_refused_at_render_time() {
        let _host = lock_host();
        crate::bind_actor_native(crate::ActorNative::default());
        let root = std::env::temp_dir().join("wklua-lua-host-render-writes");
        let _ = std::fs::remove_dir_all(&root);
        // The HUD runs once per rendered frame, differently on every peer:
        // a draw from WA's RNG or a spawn there would desync the match.
        let pack = pack(
            &root.join("r"),
            "r.render",
            r#"
id = "r.render"
api_version = 1
entry = "weapons.lua"
"#,
            r#"
wa.on.hud(function()
  local _, random = pcall(wa.random, 1, 6)
  local _, spawn = pcall(wa.actors.spawn, { sprite = "grenade" })
  wa.hud.line(tostring(random), 0, 0)
  wa.hud.line(tostring(spawn), 0, 0)
end)
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        park_lua(scripts.lua);
        let lines: Vec<String> = run_hud().unwrap().into_iter().map(|l| l.text).collect();
        assert!(lines[0].contains("wa.on.hud"), "wa.random in the HUD: {}", lines[0]);
        assert!(lines[1].contains("simulation callback"), "a spawn in the HUD: {}", lines[1]);
    }

    #[test]
    fn world_reads_gravity_wind_and_the_turn_clock() {
        let _host = lock_host();
        crate::bind_actor_native(crate::ActorNative {
            forces: || (0x3AE, -0x120),
            turn_clock_ms: || 24200,
            ..Default::default()
        });
        let root = std::env::temp_dir().join("wklua-lua-host-world-forces");
        let _ = std::fs::remove_dir_all(&root);
        let pack = pack(
            &root.join("f"),
            "f.forces",
            r#"
id = "f.forces"
api_version = 1
entry = "weapons.lua"
"#,
            "GRAVITY, WIND, CLOCK = wa.world.gravity(), wa.world.wind(), wa.world.turn_clock()",
        );
        let scripts = run_mods(&[pack]).unwrap();
        let g: i32 = scripts.lua.globals().get("GRAVITY").unwrap();
        let w: i32 = scripts.lua.globals().get("WIND").unwrap();
        let c: i32 = scripts.lua.globals().get("CLOCK").unwrap();
        park_lua(scripts.lua);
        crate::bind_actor_native(crate::ActorNative::default());
        assert_eq!((g, w, c), (0x3AE, -0x120, 24200));
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
        crate::actor_frame(
            handle,
            spawn.brain_id,
            crate::ActorEvent::default(),
        )
        .unwrap();
        assert_eq!(SPAWNED.with(|c| c.get()), 0x11);
    }
}
