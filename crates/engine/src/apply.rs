use crate::actor;
use crate::log::{log_error, panic_text, wa_directory};
use crate::log_line;
use crate::sprite::{self, SpriteRef};
use crate::world;
use game::offsets::{GAME_RUNTIME_WORLD, GAME_WORLD_DISPLAY, GAME_WORLD_WEAPON_TABLE};
use game::weapon::{snapshot_table, write_entry, WeaponEntry};
use lua_host::{mods_root_from, run_mods};
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::Mutex;
use weapons::{
    apply_params, discover_mods, filter_requirements, modset_fingerprint, ResolveResult,
};

static NAMES: Mutex<Vec<CString>> = Mutex::new(Vec::new());

#[derive(Clone)]
struct GfxPass {
    display: usize,
    resolved: ResolveResult,
    sprite_ids: HashMap<usize, i32>,
}

static GFX: Mutex<Option<GfxPass>> = Mutex::new(None);

/// PX `InitGraphic`: custom PNG/GIF sprites immediately after `LoadSprites`
/// (`sprites.dat`). In-match colour is the map's 256-slot PaletteContext
/// (usually already full). We quantize the PNG onto that table; stock
/// `SetLayerVisibility(3, 1)` later commits it.
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

    log_line("InitGraphic: installing custom sprites after LoadSprites");
    #[cfg(debug_assertions)]
    sprite::report_layers(display);
    sprite::clear();
    world::clear_sounds();
    actor::clear_claims();
    crate::worm_sprite::clear();
    crate::render_queue::clear_depths();
    // Flush the previous match's queue census before its counters are reused.
    crate::queue_census::finish();
    crate::truecolor::clear();

    let mods_dir = mods_root_from(&wa_directory());
    log_line(&format!("loading mods from {}", mods_dir.display()));
    let discovered = match discover_mods(&mods_dir) {
        Ok(d) => d,
        Err(e) => {
            log_error(&format!("InitGraphic discover: {e}"));
            return;
        }
    };
    let (mods, req_skip) = filter_requirements(&discovered);
    for s in req_skip {
        log_line(&s);
    }
    log_line(&format!("modset {}", modset_fingerprint(&mods)));

    let scripts = match run_mods(&mods) {
        Ok(s) => s,
        Err(e) => {
            log_error(&format!("InitGraphic lua: {e}"));
            return;
        }
    };
    for line in &scripts.logs {
        log_line(line);
    }
    for skip in &scripts.resolved.skipped {
        log_line(&format!(
            "slot {} kept by {}; skipped claim from {}",
            skip.claim.slot.name(),
            skip.winner_mod,
            skip.claim.mod_id
        ));
    }

    let mut sprite_ids = HashMap::new();
    // Sheets bound for the GPU atlas. Packed once, after every pack has been
    // seen, so all of them share a single texture and therefore a single draw
    // call per frame.
    let mut gpu_sheets: Vec<(String, gfx::SpriteStrip)> = Vec::new();
    for claim in &scripts.resolved.applied {
        // A pack may ask for a lower display layer so its sprites draw behind
        // the landscape instead of over it.
        sprite::set_force_layer(claim.params.layer);
        let mut install = |path: Option<String>, sheet: &gfx::SpriteStrip, as_weapon: bool| {
            // Exact palette slots are per sheet, not per pack: the turret needs
            // them, the worm animation is ordinary worm art the palette already
            // holds, and every slot taken is one the map no longer has.
            let reserve = match (&claim.params.reserve_prefix, path.as_ref()) {
                (Some(prefix), Some(p)) => p.starts_with(prefix.as_str()),
                _ => false,
            };
            // A GPU sheet never reaches install_sheet: no .spr is encoded, no
            // palette is touched, no WA slot is consumed. That is the whole
            // reason the path exists.
            if let (Some(prefix), Some(p)) = (&claim.params.gpu_prefix, path.as_ref()) {
                if p.starts_with(prefix.as_str()) {
                    gpu_sheets.push((
                        gfx::sprite_key(&claim.mod_id, p),
                        sheet.clone(),
                    ));
                    return;
                }
            }
            sprite::set_reserve_colours(reserve);
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                sprite::install_sheet(display, sheet)
            })) {
                Ok(Ok(id)) => {
                    if path.as_ref().is_some_and(|p| {
                        claim
                            .params
                            .truecolor_prefix
                            .as_ref()
                            .is_some_and(|prefix| p.starts_with(prefix))
                    }) {
                        crate::truecolor::register(id as u32, sheet.clone());
                    }
                    log_line(&format!("{} sprite slot {id}", claim.mod_id));
                    if as_weapon {
                        sprite_ids.insert(claim.slot.index(), id);
                    }
                    if let (Some(depth), Some(p)) = (claim.params.depth, path.as_ref()) {
                        let matches = claim
                            .params
                            .depth_prefix
                            .as_ref()
                            .is_none_or(|prefix| p.starts_with(prefix.as_str()));
                        if matches {
                            crate::render_queue::set_depth(id as u32, depth);
                        }
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
        for (rel, path) in &claim.params.extra_sounds {
            let key = gfx::sprite_key(&claim.mod_id, rel);
            let slot = world::remember_wav(key, path.clone());
            world::alias_sound(rel.clone(), slot, path.clone());
            log_line(&format!("{} sound slot {slot} {rel}", claim.mod_id));
        }
    }

    // One atlas for every GPU sheet in every pack, packed after all of them
    // are known so the whole frame stays a single draw call. `gpu_active` is
    // decided here and never per frame: an actor that skipped its queue
    // command has already committed to being drawn by us, so a later bail
    // would make it vanish rather than fall back.
    install_gpu_atlas(gpu_sheets);

    lua_host::park_lua(scripts.lua);
    if let Ok(mut g) = GFX.lock() {
        *g = Some(GfxPass {
            display: key,
            resolved: scripts.resolved,
            sprite_ids,
        });
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
        let installed = GFX
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|p| p.display == display as usize))
            .unwrap_or(false);
        if !installed {
            log_error(
                "InitGraphic miss: sprites installed after palette commit (colours may be wrong)",
            );
        }
    }

    // Runs per round. Sprites are installed once per match, so this only puts
    // back any slots the round change cleared rather than rebuilding them.
    let restored = sprite::reassert();
    if restored > 0 {
        log_line(&format!(
            "re-registered {restored} custom sprite slots for this round"
        ));
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
        apply_params(&mut entry, &claim.params);
        crate::worm_sprite::claim(idx, &claim.mod_id, &claim.params.worm_sprites);
        if let Some(spawn) = spawn {
            let id = pass
                .sprite_ids
                .get(&idx)
                .copied()
                .or(claim.params.sprite_id)
                .unwrap_or(0);
            let delta = if spawn.loop_anim {
                sprite::loop_delta(id)
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
        if claim.params.on_select_id != 0 {
            actor::claim_select(idx, claim.params.on_select_id);
            log_line(&format!(
                "{} LuaActor on_select={} on slot {idx}",
                claim.mod_id, claim.params.on_select_id
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
                .map(|_| sprite::loop_delta(id))
                .unwrap_or(0);
            actor::claim_fire(idx, id, delta, spawn, claim.params.on_fire_id);
            log_line(&format!(
                "{} LuaActor fire sprite {id} on slot {idx} on_fire={}",
                claim.mod_id, claim.params.on_fire_id
            ));
        }
        if let Some(&id) = pass.sprite_ids.get(&idx) {
            if claim.params.actor_spawn().is_none() && claim.params.on_click_id == 0 {
                let old = entry.sprite_id();
                entry.retarget_sprite_id(old, id);
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
        if let Some(name) = &claim.params.name {
            match CString::new(name.as_str()) {
                Ok(c) => {
                    entry.set_name1(c.as_ptr());
                    names.push(c);
                }
                Err(_) => log_error(&format!("invalid name for {}", claim.mod_id)),
            }
        }
        unsafe {
            write_entry(table, idx, &entry);
        }
        log_line(&format!(
            "applied {} -> slot {} ({}){}",
            claim.mod_id,
            idx,
            claim.slot.name(),
            claim
                .params
                .spawn
                .as_ref()
                .map(|_| " spawn".to_string())
                .or_else(|| { (claim.params.on_click_id != 0).then(|| " on_click".to_string()) })
                .or_else(|| { claim.params.actor.as_ref().map(|a| format!(" actor {a}")) })
                .or_else(|| {
                    claim
                        .params
                        .copy_from
                        .map(|s| format!(" copy_from {}", s.name()))
                })
                .unwrap_or_default()
        ));
    }

    *NAMES.lock().unwrap() = names;
    Ok(())
}

/// Pack every GPU sheet into one atlas, register the sheets, and enable the
/// GPU path for this match.
///
/// Packed once, after every pack has been seen, so the whole frame stays a
/// single draw call. `gpu::set_active` is decided here and never per frame:
/// an actor that skipped its queue command has already committed to being
/// drawn by us, so a later bail would make it vanish rather than fall back.
/// Any failure therefore leaves the path inactive, which is a complete
/// fallback because nothing has skipped a command yet.
fn install_gpu_atlas(sheets: Vec<(String, gfx::SpriteStrip)>) {
    crate::gpu::set_active(false);
    if sheets.is_empty() {
        return;
    }
    // Frames must outlive the Source borrows, so gather them first.
    let framesets: Vec<Vec<Vec<u8>>> = sheets
        .iter()
        .map(|(_, strip)| strip.frames.iter().map(|f| f.rgba.clone()).collect())
        .collect();
    let sources: Vec<render::atlas::Source> = sheets
        .iter()
        .zip(&framesets)
        .enumerate()
        .map(|(i, ((_, strip), frames))| render::atlas::Source {
            id: i as u32,
            width: strip.frames.first().map(|f| f.width).unwrap_or(0),
            height: strip.frames.first().map(|f| f.height).unwrap_or(0),
            frames,
        })
        .collect();

    let Some(atlas) = render::Atlas::pack(&sources, 4096) else {
        log_error("gpu atlas: sheets do not fit 4096x4096; stock rendering retained");
        return;
    };
    if !crate::gpu::upload_atlas(&atlas) {
        log_error("gpu atlas: upload failed; stock rendering retained");
        return;
    }
    let mut installed = 0;
    for (i, (key, strip)) in sheets.iter().enumerate() {
        let Some(packed) = atlas.sheets.iter().find(|s| s.id == i as u32) else {
            continue;
        };
        let id = sprite::install_virtual(strip, packed.frames.clone());
        if id != 0 {
            sprite::remember_key(
                key.clone(),
                sprite::SpriteRef {
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
        "gpu atlas: {installed} sheets in {}x{}, 0 WA sprite slots used",
        atlas.width, atlas.height
    ));
    crate::gpu::set_active(true);
}
