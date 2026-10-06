use crate::actor;
use crate::log_line;
use crate::log::{log_error, panic_text, wa_directory};
use crate::sprite::{self, SpriteRef};
use crate::world;
use game::offsets::{
    GAME_RUNTIME_WORLD, GAME_SESSION_RUNTIME, GAME_WORLD_DISPLAY, GAME_WORLD_PANEL_IMG,
    GAME_WORLD_WEAPON_TABLE,
};
use game::weapon::{snapshot_table, write_entry, WeaponEntry};
use lua_host::{mods_root_from, run_match};
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::Mutex;
use weapons::{apply_params, PanelIcon, StockWeapon, discover_mods, filter_requirements, modset_fingerprint, ResolveResult};

static NAMES: Mutex<Vec<CString>> = Mutex::new(Vec::new());
/// Panel slots `copy_panel_icon` overwrote: (world, slot, original hi, original
/// lo, copied hi, copied lo). See `restore_panel_icons`.
static PANEL_ORIGINALS: Mutex<Vec<(usize, usize, u32, u32, u32, u32)>> = Mutex::new(Vec::new());

#[derive(Clone)]
struct GfxPass {
    display: usize,
    resolved: ResolveResult,
    sprite_ids: HashMap<usize, i32>,
}

static GFX: Mutex<Option<GfxPass>> = Mutex::new(None);

/// Load the packs and install their sprites, once per match, from
/// `InitWeaponTable`: by then WA has loaded its own sprites and placed every
/// colour of the palette, the terrain's included, so the packs' 8-bit sprites
/// take what is left (or the nearest colours) and never the terrain's.
///
/// It used to run earlier, where Project X's `InitGraphic` did, after WA's
/// `GfxHandler::LoadSprites` (0x570B50). WA calls that from the world's
/// constructor only when it cannot load `colours.img` from its graphics
/// archive (and in headless replays). On an install that took that path the
/// packs' sprites took every free palette entry before the terrain was
/// loaded, and the terrain was drawn in one colour; on every other install
/// the sprites went in here anyway.
pub fn on_gfx_ready(display: *mut u8) {
    if display.is_null() {
        return;
    }
    let key = display as usize;
    if let Ok(g) = GFX.lock() {
        if g.as_ref().is_some_and(|p| p.display == key) {
            return;
        }
    }

    log_line("InitGraphic: installing custom sprites at InitWeaponTable");
    // A network round loads the synced copy of the host's packs, decided
    // before the lobby state is dropped; replay playback still comes first.
    let synced = crate::transfer::round_packs();
    crate::transfer::match_started();
    sprite::clear();
    world::clear_sounds();
    crate::hurt::reset();
    crate::arrow_source::clear();
    crate::physics::clear_handoffs();
    actor::clear_claims();
    crate::mask_collide::clear();
    crate::worm_sprite::clear();
    crate::worm_hold::clear();
    crate::mine::clear();
    crate::missile_boom::clear();
    crate::particles::clear();
    crate::physics::clear_homing();
    // Flush the previous match's diagnostics before their counters are reused.
    crate::queue_census::finish();
    crate::sync::finish();
    crate::replay::finish_recording();
    if let Err(e) = crate::replay::recover_playback_at_mod_load() {
        log_error(&format!("WormForge replay refused at InitGraphic: {e}"));
        return;
    }
    world::clear_input_latch();
    crate::gpu::clear();
    crate::truecolor::clear();
    crate::render_queue::clear_depths();

    let mods_dir = crate::replay::playback_mods_root()
        .or(synced)
        .unwrap_or_else(|| mods_root_from(&wa_directory()));
    log_line(&format!("loading mods from {}", mods_dir.display()));
    let discovered = match discover_mods(&mods_dir) {
        Ok(d) => d,
        Err(e) => {
            log_error(&format!("InitGraphic discover: {e}"));
            crate::replay::playback_failed(format!("cannot discover archived packs: {e}"));
            return;
        }
    };
    let disabled = weapons::LoaderSettings::load(&mods_dir).disabled;
    if !disabled.is_empty() {
        log_line(&format!(
            "disabled by {}: {}",
            weapons::SETTINGS_FILE,
            disabled.join(", ")
        ));
    }
    let (mods, req_skip) = filter_requirements(&discovered);
    for s in req_skip {
        log_line(&s);
    }
    // The selection: frozen with the packs in a network round or a replay,
    // else the loader settings'. Its bundles and the selection itself are
    // archived with the packs, so the replay runs the same components.
    let selection = crate::selection::load(&mods_dir, &crate::log::wormforge_dir().join("selection-stage"));
    let plan = selection.plan;
    let mut fingerprint = modset_fingerprint(&mods);
    if let Some(plan) = &plan {
        // Peers with a different selection see a modset mismatch.
        fingerprint.push(';');
        fingerprint.push_str(&plan.resolved.modset_part());
    }
    log_line(&format!("modset {fingerprint}"));
    crate::sync::set_local_modset(fingerprint);
    crate::modloader::match_loaded(&mods);
    if !crate::replay::prepare(&mods, &selection.folders) {
        return;
    }

    let scripts = match run_match(&mods, plan.as_ref()) {
        Ok(s) => s,
        Err(e) => {
            log_error(&format!("InitGraphic lua: {e}"));
            crate::replay::playback_failed(format!("cannot load archived packs: {e}"));
            return;
        }
    };
    // wa.log writes straight to the log now, so re-printing the load-time
    // transcript here would duplicate every line.
    for skip in &scripts.resolved.skipped {
        log_line(&format!(
            "slot {} kept by {}; skipped claim from {}",
            skip.claim.slot.name(),
            skip.winner_mod,
            skip.claim.mod_id
        ));
    }

    let mut sprite_ids = HashMap::new();
    let mut gpu_sheets: Vec<(String, gfx::SpriteStrip)> = Vec::new();
    // `sprite_overrides`, resolved once the GPU atlas has given their sheets
    // an id: (mod id, stock sprite, pack path).
    let mut overrides: Vec<(String, i32, String)> = Vec::new();
    // `params.truecolor` (default on): a GPU copy of each sheet WA holds as a
    // real sprite, drawn in its place: (WA sprite id, atlas key, sheet).
    let mut truecolor: Vec<(i32, String, gfx::SpriteStrip)> = Vec::new();
    for claim in &scripts.resolved.applied {
        sprite::set_force_layer(claim.params.layer);
        let mut install = |path: Option<String>, sheet: &gfx::SpriteStrip, as_weapon: bool| {
            let reserve = match (&claim.params.reserve_prefix, path.as_ref()) {
                (Some(prefix), Some(p)) => p.starts_with(prefix.as_str()),
                _ => false,
            };
            if let Some(p) = path.as_ref() {
                let by_prefix = claim
                    .params
                    .gpu_prefix
                    .as_ref()
                    .is_some_and(|prefix| p.starts_with(prefix.as_str()));
                // Worm aim/draw sheets are only ever blitted through the RQ
                // intercept, so they default to truecolor. The weapon body
                // (as_weapon) must stay a real WA slot for MissileEntity.
                let worm_sheet = !as_weapon
                    && claim.params.gpu_worm_sprites
                    && claim.params.worm_sprites.iter().any(|(_, _, wp)| wp == p);
                let hold_sheet = !as_weapon && claim.params.hold.as_ref().is_some_and(|hold| {
                    hold.sheet == *p || hold.hand.as_ref() == Some(p)
                        || hold.flash.as_ref().is_some_and(|flash| flash.sheet == *p)
                });
                let mole_dig_sheet = claim
                    .params
                    .mole_dig_rotation
                    .iter()
                    .any(|(_, wp)| wp == p);
                // A stock sprite a pack overrides draws on the GPU, in true
                // colour: a palette copy picks WA's nearest colours, which can
                // be team-colour entries WA then tints with the team's colour.
                let override_sheet = !as_weapon
                    && claim.params.sprite_overrides.iter().any(|(_, wp)| wp == p);
                if (by_prefix || worm_sheet || hold_sheet || override_sheet) && !mole_dig_sheet {
                    gpu_sheets.push((gfx::sprite_key(&claim.mod_id, p), sheet.clone()));
                    return;
                }
            }
            sprite::set_reserve_colours(reserve);
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                sprite::install_sheet(display, sheet)
            })) {
                Ok(Ok(id)) => {
                    log_line(&format!("{} sprite slot {id}", claim.mod_id));
                    if claim.params.truecolor && !truecolor.iter().any(|(t, ..)| *t == id) {
                        truecolor.push((id, format!("truecolor:{id}"), sheet.clone()));
                    }
                    if as_weapon {
                        sprite_ids.insert(claim.slot.index(), id);
                    }
                    if let Some(path) = path {
                        let (w, h) = sheet
                            .frames
                            .first()
                            .map(|f| (f.width as i32, f.height as i32))
                            .unwrap_or((0, 0));
                        sprite::remember_key(
                            gfx::sprite_key(&claim.mod_id, &path),
                            SpriteRef {
                                id,
                                delta: sprite::loop_delta(id),
                                w,
                                h,
                            },
                        );
                    }
                }
                Ok(Err(e)) => log_error(&format!("custom sprite {}: {e}", claim.mod_id)),
                Err(p) => log_error(&format!(
                    "custom sprite {} panicked: {}",
                    claim.mod_id,
                    panic_text(p)
                )),
            }
        };
        if let Some(sheet) = &claim.params.sprite_sheet {
            install(claim.params.sprite_path.clone(), sheet, true);
        }
        for (path, sheet) in &claim.params.extra_sheets {
            install(Some(path.clone()), sheet, false);
        }
        for (target, path) in &claim.params.sprite_overrides {
            overrides.push((claim.mod_id.clone(), *target, path.clone()));
        }
        for (target, path) in &claim.params.mole_dig_rotation {
            let key = gfx::sprite_key(&claim.mod_id, path);
            let sheet = claim.params.extra_sheets.iter().find(|(p, _)| p == path);
            match (sprite::lookup_key(&key), sheet) {
                (Some(source), Some((_, sheet))) => {
                    match sprite::alias_stock(display, *target, source.id, sheet) {
                        Ok(true) => {}
                        Ok(false) => log_line(&format!(
                            "{} stock sprite {target} already claimed; skipped",
                            claim.mod_id
                        )),
                        Err(e) => log_error(&format!(
                            "{} stock sprite {target}: {e}", claim.mod_id
                        )),
                    }
                }
                _ => log_error(&format!(
                    "{} stock sprite {target}: sheet {path} not installed",
                    claim.mod_id
                )),
            }
        }
        for (rel, path) in &claim.params.extra_sounds {
            let key = gfx::sprite_key(&claim.mod_id, rel);
            let slot = world::remember_wav(key, path.clone());
            world::alias_sound(rel.clone(), slot, path.clone());
            log_line(&format!("{} sound slot {slot} {rel}", claim.mod_id));
        }
    }

    // Most wanted first: what does not fit the atlas is left out from the end.
    let copies = truecolor.iter().map(|(_, key, sheet)| (key.clone(), sheet.clone())).collect();
    install_gpu_atlas(gpu_sheets, copies);
    for (id, key, _) in &truecolor {
        if let Some(copy) = sprite::lookup_key(key).filter(|s| sprite::is_virtual(s.id)) {
            sprite::alias_gpu(*id, copy.id);
        }
    }
    for (mod_id, target, path) in &overrides {
        let claimed = match sprite::lookup_key(&gfx::sprite_key(mod_id, path)) {
            Some(source) if sprite::is_virtual(source.id) => Ok(sprite::alias_gpu(*target, source.id)),
            // Also the pack's weapon sprite, so a real WA sprite: swap it in.
            Some(source) => scripts
                .resolved
                .applied
                .iter()
                .filter(|c| &c.mod_id == mod_id)
                .find_map(|c| c.params.extra_sheets.iter().find(|(p, _)| p == path))
                .ok_or_else(|| "sheet not loaded".to_string())
                .and_then(|(_, sheet)| sprite::alias_stock(display, *target, source.id, sheet)),
            None => Err(format!("sheet {path} not installed")),
        };
        match claimed {
            Ok(true) => {}
            Ok(false) => log_line(&format!("{mod_id} stock sprite {target} already claimed; skipped")),
            Err(e) => log_error(&format!("{mod_id} stock sprite {target}: {e}")),
        }
    }

    lua_host::park_lua(scripts.lua);
    if let Ok(mut g) = GFX.lock() {
        *g = Some(GfxPass {
            display: key,
            resolved: scripts.resolved,
            sprite_ids,
        });
    }
}

/// The match is over: the next one must load from scratch.
///
/// `on_gfx_ready` runs once per display, keyed by its address, as a guard
/// against reaching it twice in one match. WA frees the display at match end and can
/// allocate the next match's at the same address; keyed on a stale entry, the
/// new match then skipped everything -- no Lua, no rule packs, no recorder --
/// and a player who stayed in WA between rounds desynced at frame 0.
pub fn match_ended() {
    if let Ok(mut g) = GFX.lock() {
        *g = None;
    }
}

pub fn after_init_weapon_table(runtime: *mut u8) -> Result<(), String> {
    if runtime.is_null() {
        return Err("InitWeaponTable runtime is null".into());
    }
    let world = unsafe { *(runtime.add(GAME_RUNTIME_WORLD) as *const *mut u8) };
    if world.is_null() {
        return Err("GameRuntime.world is null".into());
    }
    let table = unsafe { *(world.add(GAME_WORLD_WEAPON_TABLE) as *const *mut WeaponEntry) };
    if table.is_null() {
        return Err("GameWorld.weapon_table is null".into());
    }

    let display = unsafe { *(world.add(GAME_WORLD_DISPLAY) as *const *mut u8) };
    if display.is_null() {
        log_error("GameWorld.display is null; custom sprites skipped");
    } else {
        on_gfx_ready(display);
    }

    let restored = sprite::reassert();
    if restored > 0 {
        log_line(&format!("re-registered {restored} custom sprite slots for this round"));
    }

    let snapshot = unsafe { snapshot_table(table) };
    let pass = match GFX.lock() {
        Ok(g) => g.clone(),
        Err(_) => None,
    };
    let Some(pass) = pass else {
        return Err("no overlay (InitGraphic did not run)".into());
    };

    let mut names = Vec::new();
    for claim in &pass.resolved.applied {
        let idx = claim.slot.index();
        let mut entry = snapshot.entries[idx];
        let spawn = claim.params.actor_spawn();
        if let Some(src) = claim.params.copy_from {
            if spawn.is_none() && claim.params.on_click_id == 0 {
                entry.copy_fire_from(&snapshot.entries[src.index()]);
            }
        }
        if let Some(wep) = &claim.params.wep {
            if spawn.is_none() && claim.params.on_click_id == 0 {
                weapons::wep::import(&mut entry, wep);
                log_line(&format!("{} fires as {} ({})", claim.mod_id, wep.name, wep.path));
            }
        }
        if !claim.params.patch.is_empty() && spawn.is_none() && claim.params.on_click_id == 0 {
            weapons::wep::patch(&mut entry, &claim.params.patch);
            log_line(&format!(
                "{} patches {} weapon field{}",
                claim.mod_id,
                claim.params.patch.len(),
                if claim.params.patch.len() == 1 { "" } else { "s" }
            ));
        }
        apply_params(&mut entry, &claim.params);
        if let Some(src) = claim.params.stock_fire_choice {
            let mut alternate = entry;
            alternate.copy_fire_from(&snapshot.entries[src.index()]);
            actor::claim_stock_fire_choice(idx, alternate);
            if idx == StockWeapon::SkipGo.index() && src == StockWeapon::Surrender {
                crate::worm_sprite::claim_skip_go_surrender_pose();
            }
            log_line(&format!(
                "{} slot {idx} fires as pristine {} while Ctrl is held",
                claim.mod_id,
                src.name()
            ));
        }
        match &claim.params.panel_icon {
            Some(PanelIcon::Stock(icon)) => {
                unsafe {
                    copy_panel_icon(world, icon.index(), idx);
                }
                log_line(&format!("{} panel icon {}", claim.mod_id, icon.name()));
            }
            Some(PanelIcon::File { path, width, height, rgba }) => {
                match unsafe { crate::panel_icon::install(world, idx, rgba, *width, *height) } {
                    Ok(colours) => log_line(&format!(
                        "{} panel icon {path} (quantized, {colours} colours)",
                        claim.mod_id
                    )),
                    Err(e) => log_error(&format!("{} panel icon {path}: {e}", claim.mod_id)),
                }
            }
            None => {}
        }
        crate::worm_sprite::claim(
            idx,
            &claim.mod_id,
            &claim.params.worm_sprites,
            claim.params.worm_sprites_animate,
        );
        if let Some(hold) = &claim.params.hold {
            crate::worm_hold::claim(idx, &claim.mod_id, hold);
        }
        if let Some(spawn) = spawn {
            let id = pass
                .sprite_ids
                .get(&idx)
                .copied()
                .or(claim.params.sprite_id)
                .unwrap_or(0);
            let delta = if spawn.loop_anim {
                sprite::actor_loop_delta(id)
            } else {
                0
            };
            actor::claim_drop(idx, id, delta, Some(spawn), claim.params.on_click_id);
            log_line(&format!(
                "{} LuaActor spawn sprite {id} on slot {idx}",
                claim.mod_id
            ));
        } else if claim.params.on_click_id != 0 {
            let id = pass
                .sprite_ids
                .get(&idx)
                .copied()
                .or(claim.params.sprite_id)
                .unwrap_or(0);
            actor::claim_drop(idx, id, 0, None, claim.params.on_click_id);
            log_line(&format!(
                "{} LuaActor on_click={} on slot {idx}",
                claim.mod_id, claim.params.on_click_id
            ));
        }
        if claim.params.fire_repeat > 1 {
            actor::claim_fire_repeat(idx, claim.params.fire_repeat);
            log_line(&format!(
                "{} fires {} times per shot on slot {idx}",
                claim.mod_id, claim.params.fire_repeat
            ));
        }
        if claim.params.on_select_id != 0 || claim.params.eat_fire {
            actor::claim_select(idx, claim.params.on_select_id, claim.params.eat_fire);
            log_line(&format!(
                "{} LuaActor on_select={} eat_fire={} on slot {idx}",
                claim.mod_id, claim.params.on_select_id, claim.params.eat_fire
            ));
        }
        if claim.params.uses_lua_projectile() {
            let id = pass
                .sprite_ids
                .get(&idx)
                .copied()
                .or(claim.params.sprite_id)
                .unwrap_or(0);
            let spawn = claim.params.projectile_spawn();
            let delta = spawn
                .as_ref()
                .filter(|s| s.loop_anim)
                .map(|_| sprite::actor_loop_delta(id))
                .unwrap_or(0);
            actor::claim_fire(idx, id, delta, spawn, claim.params.on_fire_id);
            log_line(&format!(
                "{} LuaActor fire sprite {id} on slot {idx} on_fire={}",
                claim.mod_id, claim.params.on_fire_id
            ));
        }
        if let Some(&id) = pass.sprite_ids.get(&idx) {
            if claim.params.actor_spawn().is_none() && claim.params.on_click_id == 0 {
                let old = if entry.is_strike() { entry.read_i32(0x74) } else { entry.sprite_id() };
                // A placed weapon's held sheet follows its flight sprite;
                // keep the stock one unless the pack draws the worm itself.
                if entry.fire_type() == 2
                    && entry.fire_method() == 2
                    && claim.params.worm_sprites.is_empty()
                    && crate::worm_sprite::hold_as(idx, old)
                {
                    log_line(&format!("{} worm holds slot {idx} as stock sprite {old}", claim.mod_id));
                }
                if entry.is_strike() {
                    entry.retarget_strike_sprite_id(id);
                } else {
                    entry.retarget_sprite_id(old, id);
                    crate::missile_boom::set_pack_body(idx, id);
                }
                log_line(&format!(
                    "{} fire sprite {old} -> {id} (defined={})",
                    claim.mod_id,
                    entry.defined(),
                ));
            }
        } else if claim.params.sprite_sheet.is_some() {
            log_error(&format!(
                "{} has a sprite sheet but no installed id for slot {idx}",
                claim.mod_id
            ));
        }
        if let Some(path) = &claim.params.cluster_sprite_path {
            match sprite::lookup_key(&gfx::sprite_key(&claim.mod_id, path)) {
                Some(s) => {
                    if entry.is_strike() {
                        entry.set_strike_cluster_sprite_id(s.id);
                    } else {
                        entry.set_cluster_sprite_id(s.id);
                    }
                    // A homing cluster draws its render timer, which
                    // apply_params set from the cluster sprite before this
                    // sheet had an id.
                    if claim.params.cluster_homing.is_some() {
                        entry.set_cluster_render_timer(s.id);
                    }
                    log_line(&format!("{} cluster sprite {path} -> {}", claim.mod_id, s.id));
                }
                None => log_error(&format!(
                    "{} cluster sprite {path} was not installed",
                    claim.mod_id
                )),
            }
        }
        if let Some(path) = &claim.params.cluster_homing_sprite_path {
            match sprite::lookup_key(&gfx::sprite_key(&claim.mod_id, path)) {
                Some(s) => {
                    entry.set_cluster_render_timer(s.id);
                    log_line(&format!("{} homing cluster sprite {path} -> {}", claim.mod_id, s.id));
                }
                None => log_error(&format!(
                    "{} homing cluster sprite {path} was not installed",
                    claim.mod_id
                )),
            }
        }
        if let Some(path) = &claim.params.plane_sprite_path {
            match sprite::lookup_key(&gfx::sprite_key(&claim.mod_id, path)) {
                Some(s) if entry.fire_type() == 3 => {
                    entry.set_strike_plane_sprite(s.id);
                    log_line(&format!("{} strike plane {path} -> {}", claim.mod_id, s.id));
                }
                Some(_) => log_error(&format!(
                    "{} plane {path}: slot {idx} fires no strike",
                    claim.mod_id
                )),
                None => log_error(&format!("{} plane {path} was not installed", claim.mod_id)),
            }
        }
        if let Some(name) = &claim.params.name {
            match CString::new(name.as_str()) {
                Ok(c) => {
                    // Both names: the weapon panel shows `name2`, so setting
                    // only `name1` left a renamed weapon under its stock
                    // name in the panel.
                    entry.set_name1(c.as_ptr());
                    entry.set_name2(c.as_ptr());
                    names.push(c);
                }
                Err(_) => log_error(&format!("invalid name for {}", claim.mod_id)),
            }
        }
        unsafe {
            write_entry(table, idx, &entry);
        }
        if entry.is_mine() && !claim.params.uses_lua_projectile() {
            let sprite = pass
                .sprite_ids
                .get(&idx)
                .copied()
                .or(claim.params.sprite_id)
                .unwrap_or(0);
            crate::mine::claim(
                idx,
                crate::mine::MineTune {
                    sprite,
                    damage: claim.params.damage.unwrap_or(0),
                    bounce_pct: claim.params.bounce_pct,
                    gravity_pct: claim.params.gravity_pct,
                    wind_pct: claim.params.wind_pct,
                    explode: claim.params.explode_spawn.clone(),
                    explode_id: claim.params.on_explode_id,
                    proximity: claim.params.proximity_spawn.clone(),
                    proximity_id: claim.params.on_proximity_id,
                    place: claim.params.place_spawn.clone(),
                    place_id: claim.params.on_place_id,
                },
            );
            log_line(&format!(
                "{} MineEntity slot {idx} sprite {sprite} damage {}",
                claim.mod_id,
                claim.params.damage.unwrap_or(100)
            ));
        } else if claim.params.explodes_as_missile() {
            // Same attach pattern as mines: stock MissileEntity (HM/bazooka/…)
            // keeps its fire path; Lua bits spawn on detonate.
            let sprite = pass
                .sprite_ids
                .get(&idx)
                .copied()
                .or(claim.params.sprite_id)
                .unwrap_or(0);
            crate::missile_boom::claim(
                idx,
                crate::missile_boom::BoomTune {
                    sprite,
                    explode: claim.params.explode_spawn.clone(),
                    explode_id: claim.params.on_explode_id,
                },
            );
            log_line(&format!(
                "{} MissileEntity on_explode slot {idx}",
                claim.mod_id
            ));
        }
        let mut tags = String::new();
        if claim.params.cursor_on_select {
            tags.push_str(" cursor");
        }
        if claim.params.eat_fire {
            tags.push_str(" eat_fire");
        }
        if claim.params.on_click_id != 0 {
            tags.push_str(" on_click");
        }
        if claim.params.on_fire_id != 0 {
            tags.push_str(" on_fire");
        }
        if claim.params.spawn.is_some() {
            tags.push_str(" spawn");
        }
        log_line(&format!(
            "applied {} -> slot {} ({}){tags}",
            claim.mod_id,
            idx,
            claim.slot.name(),
        ));
    }

    *NAMES.lock().unwrap() = names;
    Ok(())
}

/// Panel tiles read two frontend .img pointers per stock weapon from
/// GameWorld+0x144. Copy the source slot's hi/lo pair onto the overlay slot.
///
/// The table OWNS those pointers: WA's teardown deletes every slot's images.
/// After a copy two slots own the same objects, so they would be deleted
/// twice -- the second virtual destructor call goes through a freed vtable
/// and WA crashes (EIP 0) at the end of every match that used `panel_icon`.
/// So the overwritten originals are remembered and put back by
/// `restore_panel_icons` before WA tears the world down.
unsafe fn copy_panel_icon(world: *mut u8, src: usize, dst: usize) {
    if world.is_null() || src == dst || src > 70 || dst > 70 {
        return;
    }
    let base = world.add(GAME_WORLD_PANEL_IMG) as *mut u32;
    let src_hi = *base.add(src * 2);
    let src_lo = *base.add(src * 2 + 1);
    if src_hi == 0 && src_lo == 0 {
        return;
    }
    let mut originals = PANEL_ORIGINALS.lock().unwrap_or_else(|p| p.into_inner());
    // A different world is a new match; the old records describe freed memory.
    originals.retain(|r| r.0 == world as usize);
    let (orig_hi, orig_lo) = (*base.add(dst * 2), *base.add(dst * 2 + 1));
    match originals.iter_mut().find(|r| r.1 == dst) {
        // Copied onto twice: keep the first original, track the latest copy.
        Some(r) => (r.4, r.5) = (src_hi, src_lo),
        None => originals.push((world as usize, dst, orig_hi, orig_lo, src_hi, src_lo)),
    }
    *base.add(dst * 2) = src_hi;
    *base.add(dst * 2 + 1) = src_lo;
}

/// Live retarget of a stock slot's body (and optional cluster bit) sprite.
/// Used by packs that must draw `wa.random` during the match (Highlander-style),
/// not at InitGraphic when the RNG stream is still a fixed default.
pub fn retarget_sprites(slot: usize, body: i32, cluster: Option<i32>) -> Result<(), String> {
    use game::offsets::STOCK_WEAPON_COUNT;
    if slot >= STOCK_WEAPON_COUNT {
        return Err(format!("retarget: slot {slot} out of range"));
    }
    if body == 0 {
        return Err("retarget: body sprite id must be non-zero".into());
    }
    let world = unsafe { runtime_world() };
    if world.is_null() {
        return Err("retarget: no GameWorld".into());
    }
    let table = unsafe { *(world.add(GAME_WORLD_WEAPON_TABLE) as *const *mut WeaponEntry) };
    if table.is_null() {
        return Err("retarget: no weapon table".into());
    }
    unsafe {
        let mut entry = *table.add(slot);
        // Leave WeaponEntry+0x60 stock: throw-aim (wthrgrn / wthrcls / …) and
        // bazooka hold still resolve from the stock body. Flight is rewritten
        // on the MissileEntity after ctor (missile_boom::set_flight_sprite).
        if let Some(bit) = cluster {
            entry.set_cluster_sprite_id(bit);
        }
        write_entry(table, slot, &entry);
        if entry.is_mine() {
            crate::mine::set_sprite(slot, body);
        } else {
            crate::missile_boom::set_flight_sprite(slot, body);
        }
    }
    log_line(&format!(
        "retarget slot {slot} flight {body}{}",
        cluster
            .map(|c| format!(" cluster {c}"))
            .unwrap_or_default()
    ));
    Ok(())
}

/// The live GameWorld, read the way `after_init_weapon_table` got the one
/// `copy_panel_icon` wrote into: `GameRuntime.world`.
unsafe fn runtime_world() -> *mut u8 {
    let slot = crate::wa::address("G_GAME_SESSION") as *const *mut u8;
    if slot.is_null() || (*slot).is_null() {
        return std::ptr::null_mut();
    }
    let runtime = *((*slot).add(GAME_SESSION_RUNTIME) as *const *mut u8);
    if runtime.is_null() {
        return std::ptr::null_mut();
    }
    *(runtime.add(GAME_RUNTIME_WORLD) as *const *mut u8)
}

/// Put every slot `copy_panel_icon` overwrote back to its own images, so each
/// image has exactly one owner when WA's teardown deletes them. Called by the
/// `GameEngine::Shutdown` hook BEFORE the original runs.
///
/// Only writes into the live session world, and only where the slot still
/// holds the pair we copied there -- anything else means the table changed
/// under us, and writing would be a guess.
pub fn restore_panel_icons() {
    let mut originals = PANEL_ORIGINALS.lock().unwrap_or_else(|p| p.into_inner());
    let live = unsafe { runtime_world() } as usize;
    for (w, dst, orig_hi, orig_lo, hi, lo) in originals.drain(..) {
        if w != live {
            continue;
        }
        unsafe {
            let base = (w as *mut u8).add(GAME_WORLD_PANEL_IMG) as *mut u32;
            if *base.add(dst * 2) == hi && *base.add(dst * 2 + 1) == lo {
                *base.add(dst * 2) = orig_hi;
                *base.add(dst * 2 + 1) = orig_lo;
            } else {
                log_error(&format!(
                    "panel icon slot {dst} changed after the copy; left as it is"
                ));
            }
        }
    }
}


fn pack_atlas(sheets: &[(String, gfx::SpriteStrip)]) -> Option<render::Atlas> {
    // The frames' pixels are borrowed, not copied: a pack that does not fit
    // is tried again with fewer sheets.
    let sources: Vec<render::atlas::Source> = sheets
        .iter()
        .enumerate()
        .map(|(i, (_, strip))| render::atlas::Source {
            id: i as u32,
            width: strip.frames.first().map(|f| f.width).unwrap_or(0),
            height: strip.frames.first().map(|f| f.height).unwrap_or(0),
            frames: strip.frames.iter().map(|f| f.rgba.as_slice()).collect(),
        })
        .collect();
    render::Atlas::pack(&sources, 4096)
}

/// The most of `n` optional sheets that fit, in order: the largest `k` with
/// `fits(k)`, `fits` being monotonic (fewer always fit if more do). None when
/// not even 0 fit.
fn longest_fit(n: usize, mut fits: impl FnMut(usize) -> bool) -> Option<usize> {
    if fits(n) {
        return Some(n);
    }
    if !fits(0) {
        return None;
    }
    // fits(lo), !fits(hi).
    let (mut lo, mut hi) = (0, n);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(lo)
}

/// `optional` sheets go in too, as many as fit beside `sheets`, in the order
/// given (most wanted first); the rest are left out, and said so.
fn install_gpu_atlas(sheets: Vec<(String, gfx::SpriteStrip)>, optional: Vec<(String, gfx::SpriteStrip)>) {
    crate::gpu::set_active(false);
    let required = sheets.len();
    let wanted = optional.len();
    let mut sheets = sheets;
    sheets.extend(optional);
    if sheets.is_empty() {
        return;
    }
    let mut atlas = None;
    let kept = longest_fit(wanted, |k| {
        let fitted = pack_atlas(&sheets[..required + k]);
        let ok = fitted.is_some();
        if ok {
            atlas = fitted;
        }
        ok
    });
    if let Some(k) = kept {
        if k < wanted {
            log_error(&format!(
                "gpu atlas: {} of {wanted} true-colour copies of WA sprites do not fit; they keep their palette draw",
                wanted - k
            ));
        }
        sheets.truncate(required + k);
        if sheets.is_empty() {
            return;
        }
        // The last pack tried may have been a larger one that failed.
        if atlas.as_ref().is_none_or(|a| a.sheets.len() != sheets.len()) {
            atlas = pack_atlas(&sheets);
        }
    }
    let Some(atlas) = atlas else {
        log_error("gpu atlas: sheets do not fit 4096x4096; stock rendering retained");
        return;
    };
    let mut installed = 0;
    for (i, (key, strip)) in sheets.iter().enumerate() {
        let Some(packed) = atlas.sheets.iter().find(|s| s.id == i as u32) else {
            continue;
        };
        let id = sprite::install_virtual(strip, packed.frames.clone());
        if id != 0 {
            sprite::remember_key(
                key.clone(),
                SpriteRef {
                    id,
                    w: strip.frames.first().map(|f| f.width).unwrap_or(0) as i32,
                    h: strip.frames.first().map(|f| f.height).unwrap_or(0) as i32,
                    delta: strip.phase_delta(),
                },
            );
            installed += 1;
        }
    }
    log_line(&format!(
        "gpu atlas: {installed} sheets in {}x{} ({}% covered, {} frames sharing a picture), 0 WA sprite slots used",
        atlas.width,
        atlas.height,
        atlas.used * 100 / (atlas.width as u64 * atlas.height as u64).max(1),
        atlas.shared
    ));
    crate::gpu::set_atlas(atlas);
    crate::gpu::set_active(true);
    log_line("gpu renderer active: sprites drawn as GPU quads");
}

#[cfg(test)]
mod tests {
    use super::longest_fit;

    #[test]
    fn the_atlas_keeps_the_most_optional_sheets_that_fit() {
        assert_eq!(longest_fit(41, |k| k <= 41), Some(41), "all of them");
        assert_eq!(longest_fit(41, |k| k <= 31), Some(31), "the first 31");
        assert_eq!(longest_fit(41, |_| true), Some(41));
        assert_eq!(longest_fit(41, |k| k == 0), Some(0), "none of them");
        assert_eq!(longest_fit(41, |_| false), None, "not even the required sheets");
        assert_eq!(longest_fit(0, |_| true), Some(0));
        for limit in 0..=10 {
            assert_eq!(longest_fit(10, |k| k <= limit), Some(limit));
        }
    }
}
