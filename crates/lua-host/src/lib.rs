//! Lua 5.4 host. One interpreter per match, created at gfx InitGraphic
//! (`SetLayerVisibility(3, 1)`), before InitWeaponTable overlays slots.
//!
//! `wa.*` is a dispatcher: named ids, first owner wins. Scripts do not
//! override stock C++ methods. See docs/architecture.md.

mod actor;
mod api;
mod dispatch;
mod sandbox;
mod sim_context;
mod spawn;

pub use actor::{
    gfx_field, actor_frame, actor_freed, actor_spawned, bind_actor_native, weapon_click, weapon_emit,
    weapon_fire, weapon_select, ActorEvent, ActorNative, AmmoChange, SpawnArgs,
};
pub use dispatch::{
    death, gravity_after, gravity_before, hurt, missile_frame, run_hud, turn_start, worm_message,
    Death, Hurt,
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
            allowed_slots: pack
                .manifest
                .weapons
                .allowed_slots()?
                .into_iter()
                .collect(),
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
        let err = lua
            .load("return wa.actors.spawn({ sprite = 'grenade', owner = 12345 })")
            .eval::<mlua::Value>()
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

    fn lock_host() -> std::sync::MutexGuard<'static, ()> {
        HOST.lock().unwrap_or_else(|e| e.into_inner())
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

    #[test]
    fn land_verbs_reach_native_only_during_simulation() {
        let _guard = lock_host();
        thread_local! {
            static CUT: std::cell::Cell<Option<(i32, i32, i32)>> = const { std::cell::Cell::new(None) };
            static DIG: std::cell::Cell<Option<(i32, i32, i32)>> = const { std::cell::Cell::new(None) };
        }
        actor::bind_actor_native(actor::ActorNative {
            land_cut: |shape, x, y| { CUT.with(|c| c.set(Some((shape, x, y)))); Ok(()) },
            land_dig: |x, y, radius| { DIG.with(|c| c.set(Some((x, y, radius)))); Ok(()) },
            ..Default::default()
        });
        let lua = sandbox::new_lua().unwrap();
        api::install(&lua, &test_ctx()).unwrap();
        let cut = "wa.land.cut{ x = 0x12340000, y = 0x56780000, shape = 'torch', facing = 1, angle = 0 }";
        assert!(lua.load(cut).exec().is_err());
        sim_context::with_sim(|| {
            lua.load(cut).exec().unwrap();
            lua.load("wa.land.dig{ x = 0x10000, y = 0x20000, radius = 12 }").exec().unwrap();
        });
        CUT.with(|c| assert_eq!(c.get(), Some((10, 0x12340000, 0x56780000))));
        DIG.with(|c| assert_eq!(c.get(), Some((0x10000, 0x20000, 12))));
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
        assert_eq!(claim.slot, StockWeapon::Bazooka);
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
        let five = pack(&root, "t.repeat", manifest, r#"wa.weapons.replace({ weapon = "shotgun", fire_repeat = 5 })"#);
        let scripts = run_mods(&[five]).unwrap();
        assert_eq!(scripts.resolved.applied[0].params.fire_repeat, 5);
        let stock = pack(&root, "t.repeat", manifest, r#"wa.weapons.replace({ weapon = "shotgun" })"#);
        assert_eq!(run_mods(&[stock]).unwrap().resolved.applied[0].params.fire_repeat, 1);
        let bad = pack(&root, "t.repeat", manifest, r#"wa.weapons.replace({ weapon = "shotgun", fire_repeat = 0 })"#);
        let err = run_mods(&[bad]).err().expect("no shot at all is refused");
        assert!(err.contains("fire_repeat is 1 to 20"), "{err}");
            root.join("plane.png"),
        )
        .unwrap();
        let scripts = run_mods(&[own]).unwrap();
        let params = &scripts.resolved.applied[0].params;
        assert_eq!(params.plane_sprite_path.as_deref(), Some("plane.png"));
        assert!(params.extra_sheets.iter().any(|(p, _)| p == "plane.png"));
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
end)
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        park_lua(scripts.lua);
        let m = game::entity::Motion {
            id: 1, class_type: 0, damp: 0, wind_scale: 0, gravity_scale: 0, moved: 0,
            x: 0, y: 0, vx: 0, vy: 0, cm_gravity: 0, cm_wind: 0,
        };
        assert_eq!(missile_frame(m, 2, "pre", Some(15)).unwrap().vx, 1, "slot 15 is the longbow");
        assert_eq!(missile_frame(m, 2, "pre", None).unwrap().vx, 2, "no slot is nil");
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
seen = {}
wa.on.turn_start(function(event)
  seen.turn = event.worm ~= nil and event.active ~= nil
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
end)
"#,
        );
        let scripts = run_mods(&[pack]).unwrap();
        park_lua(scripts.lua);

        turn_start(1).unwrap();
        hurt(Hurt {
            worm: 1,
            from: 2,
            by: Some(2),
            attacker: 2,
            slot: Some(1),
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
            assert!(seen.get::<bool>("hurt").unwrap());
            assert!(seen.get::<bool>("death").unwrap());
        });
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
        crate::actor_frame(
            handle,
            spawn.brain_id,
            crate::ActorEvent::default(),
        )
        .unwrap();
        assert_eq!(SPAWNED.with(|c| c.get()), 0x11);
    }
}
