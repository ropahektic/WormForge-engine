//! WA-side integration for the fog-of-war visibility model.
//!
//! The model itself lives in the pure `fog` crate: it knows nothing about
//! WA's memory layout, only a solidity closure and a list of vision sources.
//! This module owns the parts that need WA: walking the live entity tree each
//! render frame to build that source list, sampling the collision grid for
//! the closure, and turning the resulting light grid into a screen-space L8
//! mask for the renderer.
//!
//! Lifecycle mirrors `gpu.rs`: a per-match `Fog` behind a `Mutex`, an
//! `is_active`/`set_active` gate, `start_match` when a mod enables fog, and
//! `clear` on the same match-teardown path as `gpu::clear`.

use crate::{log_error, log_line, world};
use fog::{Fog, Scheme, SourceKind, VisionSource};
use game::entity::read_i32;
use game::offsets::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

/// The per-match fog engine. `None` until a mod enables fog for this match.
static FOG: Mutex<Option<EngineFog>> = Mutex::new(None);
/// The scheme a mod asked for, held until a live map exists to build the
/// engine from. Mod entry scripts run at gfx init, before any round -- so
/// `start_match` almost always runs too early to read a map size, and the real
/// construction is deferred to the first render frame that has one.
static PENDING_SCHEME: Mutex<Option<Scheme>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static ANNOUNCED: AtomicBool = AtomicBool::new(false);
static OWNERSHIP_LOGGED: AtomicBool = AtomicBool::new(false);
static NO_GRID_LOGGED: AtomicBool = AtomicBool::new(false);

struct EngineFog {
    fog: Fog,
    /// This frame's vision sources, rebuilt every render frame. Reused across
    /// frames so a steady frame does not allocate.
    sources: Vec<VisionSource>,
    /// The most recent screen-space mask, cached so the render pass can take
    /// it without rebuilding. Sized to the surface it was built for.
    mask: Vec<u8>,
    mask_w: usize,
    mask_h: usize,
}

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

pub fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::SeqCst);
    if active && !ANNOUNCED.swap(true, Ordering::SeqCst) {
        log_line("fog of war active: vision sources drive an L8 visibility mask");
    }
}

/// True when fog is wanted for this match: either the engine is built and
/// running, or a scheme is parked waiting for the first map to appear. This is
/// what the Lua `wa.fog.enabled()` reports, so a mod that enabled fog at gfx
/// init reads true even before the first round frame constructs the engine.
pub fn enabled() -> bool {
    if !is_active() {
        return false;
    }
    let built = FOG.lock().map(|g| g.is_some()).unwrap_or(false);
    let pending = PENDING_SCHEME.lock().map(|p| p.is_some()).unwrap_or(false);
    built || pending
}

/// Ask for fog this match. Mod entry scripts run at gfx init, before a round
/// exists, so the engine is usually NOT built here: the scheme is parked and
/// constructed on the first render frame that has a live map (`on_render_frame`).
/// If a map already exists (late enable from a running round), build it now.
pub fn start_match(scheme: Scheme) {
    let (w, h) = world::map_size_px();
    if w > 0 && h > 0 {
        let fog = Fog::new(w as usize, h as usize, scheme);
        if let Ok(mut g) = FOG.lock() {
            *g = Some(EngineFog {
                fog,
                sources: Vec::new(),
                mask: Vec::new(),
                mask_w: 0,
                mask_h: 0,
            });
        }
        if let Ok(mut p) = PENDING_SCHEME.lock() {
            *p = None;
        }
    } else {
        // No map yet: park the scheme and let the first render frame build it.
        if let Ok(mut p) = PENDING_SCHEME.lock() {
            *p = Some(scheme);
        }
    }
    set_active(true);
}

/// Drop everything owned per match. Called alongside `gpu::clear`.
pub fn clear() {
    ACTIVE.store(false, Ordering::SeqCst);
    ANNOUNCED.store(false, Ordering::SeqCst);
    OWNERSHIP_LOGGED.store(false, Ordering::SeqCst);
    NO_GRID_LOGGED.store(false, Ordering::SeqCst);
    if let Ok(mut g) = FOG.lock() {
        *g = None;
    }
    if let Ok(mut p) = PENDING_SCHEME.lock() {
        *p = None;
    }
}

/// Replace the scheme (Lua `wa.fog.set`). When the engine is not built yet
/// (the usual case: a mod sets its scheme at gfx init, before `enable` and
/// before any round frame) the scheme is parked so `start_match` builds from
/// it; otherwise the live engine is updated in place.
pub fn set_scheme(scheme: Scheme) {
    let built = FOG.lock().map(|g| g.is_some()).unwrap_or(false);
    if built {
        if let Ok(mut g) = FOG.lock() {
            if let Some(engine) = g.as_mut() {
                engine.fog.set_scheme(scheme);
            }
        }
    } else if let Ok(mut p) = PENDING_SCHEME.lock() {
        *p = Some(scheme);
    }
}

/// The current scheme (Lua `wa.fog.get`). Reads the parked scheme when the
/// engine is not built yet, then the live engine's, then registry defaults --
/// so a mod can set and read back its scheme before any round exists.
pub fn scheme() -> Scheme {
    if let Ok(p) = PENDING_SCHEME.lock() {
        if let Some(pending) = p.as_ref() {
            return pending.clone();
        }
    }
    FOG.lock()
        .ok()
        .and_then(|g| g.as_ref().map(|e| e.fog.scheme().clone()))
        .unwrap_or_default()
}

/// Forward an explosion to the fog engine (crater centre + radius, landscape
/// pixels). Called from the terrain-write hook.
///
/// Not yet called: the WA land-write detour that would invoke this is a
/// follow-up. Until it lands, craters re-open sight only for sources that move
/// or whose scheme changes. Public API awaiting that wiring.
#[allow(dead_code)]
pub fn notify_explosion(x: i32, y: i32, radius: i32) {
    if let Ok(mut g) = FOG.lock() {
        if let Some(engine) = g.as_mut() {
            engine.fog.notify_explosion(x, y, radius);
        }
    }
}

/// Forward a terrain mutation rect so overlapping source masks re-cast.
///
/// Not yet called: same land-write detour follow-up as `notify_explosion`.
#[allow(dead_code)]
pub fn notify_terrain_changed(left: i32, top: i32, right: i32, bottom: i32) {
    if let Ok(mut g) = FOG.lock() {
        if let Some(engine) = g.as_mut() {
            engine.fog.notify_terrain_changed(left, top, right, bottom);
        }
    }
}

/// Light value (0/96/192/255 plus the discovery floor) at a 16.16 world
/// coordinate, for the Lua `wa.fog.visible` query. 0 when fog is not running.
pub fn visible(x_fixed: i32, y_fixed: i32) -> u8 {
    let Ok(g) = FOG.lock() else {
        return 0;
    };
    let Some(engine) = g.as_ref() else {
        return 0;
    };
    let x = x_fixed >> 16;
    let y = y_fixed >> 16;
    if x < 0 || y < 0 {
        return 0;
    }
    engine.fog.light_at(x as usize, y as usize)
}

/// One render frame: rebuild the source list from the entity tree, run the
/// cast, and build the screen-space mask.
///
/// Driven from the render dispatch (`render_queue::dispatch`), which runs once
/// per `RQ_RenderDrawingQueue` with `display` and `clip` in hand -- the same
/// place `gpu::capture` and `truecolor::capture` hook the frame. The mask is
/// cached on the engine; the render pass takes it with `build_mask`.
pub unsafe fn on_render_frame(display: usize, clip: usize) {
    if !is_active() {
        return;
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Build the engine on the first frame that has a live map. Mod entry
        // scripts enable fog at gfx init, long before a round exists, so the
        // scheme sat parked until now.
        ensure_built();
        update_sources_and_cast();
        if let Some((w, h)) = surface_size(display) {
            build_mask_into(display, clip, w, h);
        }
    }));
    if result.is_err() {
        log_error("fog: render-frame update panicked; frame skipped");
    }
}

/// Construct the fog engine from the parked scheme once a map exists. No-op
/// when already built, when no scheme was requested, or while there is still
/// no live map to size the light grid from.
fn ensure_built() {
    if FOG.lock().map(|g| g.is_some()).unwrap_or(false) {
        return;
    }
    let scheme = PENDING_SCHEME.lock().ok().and_then(|p| p.clone());
    let Some(scheme) = scheme else {
        return;
    };
    let (w, h) = world::map_size_px();
    if w <= 0 || h <= 0 {
        return;
    }
    let fog = Fog::new(w as usize, h as usize, scheme);
    if let Ok(mut g) = FOG.lock() {
        *g = Some(EngineFog {
            fog,
            sources: Vec::new(),
            mask: Vec::new(),
            mask_w: 0,
            mask_h: 0,
        });
    }
    if let Ok(mut p) = PENDING_SCHEME.lock() {
        *p = None;
    }
    log_line(&format!("fog: engine built for {w}x{h} map"));
}

/// Rebuild this frame's vision sources and run the shadowcast.
fn update_sources_and_cast() {
    // The fog crate's parallel cast calls the closure from worker threads, so
    // it must be Sync; `solid_sampler_sync` asserts that at the type level.
    let Some(solid) = world::solid_sampler_sync() else {
        if !NO_GRID_LOGGED.swap(true, Ordering::SeqCst) {
            log_error("fog: collision grid unavailable; vision not updated");
        }
        return;
    };

    let Ok(mut g) = FOG.lock() else {
        return;
    };
    let Some(engine) = g.as_mut() else {
        return;
    };
    engine.sources.clear();
    unsafe {
        collect_sources(&mut engine.sources);
    }
    engine.fog.update(&engine.sources, solid);
}

/// Walk the entity tree and append this frame's vision sources.
///
/// view_ownership is handled here rather than in the fog crate, because it is
/// about whose entities count, not about the cast. wkFOW resolves "mine"
/// through DDMain's team table and the local machine id; that plumbing is not
/// mapped in WormForge's offsets yet, so v1 behaves as view_ownership 3
/// (everyone): every worm, animal, projectile and crate lights the map. That
/// is always *correct* -- it never hides something that should be visible --
/// just not narrowed to one alliance. Logged once; narrowing is a follow-up.
unsafe fn collect_sources(out: &mut Vec<VisionSource>) {
    if !OWNERSHIP_LOGGED.swap(true, Ordering::SeqCst) {
        log_line(
            "fog: view_ownership narrowing not yet wired (local machine id \
             unmapped); behaving as view_ownership=3 (everyone)",
        );
    }

    // Worms. Position is 16.16 fixed; shift to landscape pixels. The aim
    // sweep and facing build the cone direction.
    for handle in world::list_kind(CLASS_WORM, 0) {
        let worm = handle as *mut u8;
        let x = read_i32(worm, ENT_POS_X) >> 16;
        let y = read_i32(worm, ENT_POS_Y) >> 16;
        let facing = read_i32(worm, WORM_FACING);
        let active = read_i32(worm, WORM_TURN_ACTIVE) != 0;
        let aim_angle = read_i32(worm, WORM_AIM_ANGLE);
        let aimable = worm_weapon_requires_aiming(worm);
        let (dir_x, dir_y) = aim_to_direction(aim_angle, facing);
        out.push(VisionSource {
            kind: SourceKind::Worm,
            team_number: read_i32(worm, ENT_WORM_TEAM),
            worm_number: read_i32(worm, WORM_INDEX),
            id: 0,
            x,
            y,
            aim_angle,
            dir_x,
            dir_y,
            facing,
            active,
            circle_radius: -1,
            aimable,
        });
    }

    // Projectiles and animals share CLASS_MISSILE. wkFOW tells them apart by a
    // type discriminator on the missile entity; that offset is not mapped in
    // WormForge yet, so v1 treats every CLASS_MISSILE as a Projectile and
    // animal vision is a follow-up. Oil drums (CLASS_OIL) are persistent
    // hazards -- wkFOW's force_hide_mines means they emit no projectile
    // vision, so they are skipped entirely (not walked here).
    for handle in world::list_kind(CLASS_MISSILE, 0) {
        let m = handle as *mut u8;
        let x = read_i32(m, ENT_POS_X) >> 16;
        let y = read_i32(m, ENT_POS_Y) >> 16;
        let vx = read_i32(m, ENT_SPEED_X);
        let vy = read_i32(m, ENT_SPEED_Y);
        // Heading is the velocity, as a 1024-scaled unit vector. A stationary
        // projectile (a placed mine) keeps a zero heading, which the shape
        // resolves to "pointing right".
        let (dir_x, dir_y) = normalize_1024(vx, vy);
        out.push(VisionSource {
            kind: SourceKind::Projectile,
            team_number: 0,
            worm_number: 0,
            id: handle,
            x,
            y,
            aim_angle: 0,
            dir_x,
            dir_y,
            facing: if vx < 0 { -1 } else { 1 },
            active: true,
            // Live sprite bound + proj_circle pad. The sprite bound is not
            // mapped per-entity here, so use the kind-wide proj_circle as the
            // override base: -1 would ALSO use the kind-wide value, but the
            // crate treats a negative as "no override", which is what we want
            // until the per-sprite bound is plumbed.
            circle_radius: -1,
            aimable: true,
        });
    }

    // Supply crates: public objectives, terrain-blocked circle, no cone.
    for handle in world::list_kind(CLASS_CRATE, 0) {
        let c = handle as *mut u8;
        let x = read_i32(c, ENT_POS_X) >> 16;
        let y = read_i32(c, ENT_POS_Y) >> 16;
        out.push(VisionSource {
            kind: SourceKind::Crate,
            team_number: 0,
            worm_number: 0,
            id: handle,
            x,
            y,
            aim_angle: 0,
            dir_x: 0,
            dir_y: 0,
            facing: 1,
            active: true,
            circle_radius: -1,
            aimable: true,
        });
    }
}

/// Does the worm's selected weapon require aiming? Droppables (dynamite,
/// mines, utilities) have no aim sweep, so their dirX/dirY hold a stale angle
/// and the cone falls back to facing.
unsafe fn worm_weapon_requires_aiming(worm: *mut u8) -> bool {
    let entry = read_i32(worm, WORM_ACTIVE_WEAPON) as *mut u8;
    if entry.is_null() {
        return false;
    }
    read_i32(entry, WEAPON_REQUIRES_AIMING) != 0
}

/// Port of wkFOW's `aimToDirection` (GameState.cpp:280). The low 16 bits of
/// the aim angle encode a 180-degree sweep; facing selects the side. Returns
/// the 1024-scaled unit vector the vision shape uses.
fn aim_to_direction(aim_angle: i32, facing: i32) -> (i32, i32) {
    const DIR_SCALE: f64 = 1024.0;
    const PI: f64 = std::f64::consts::PI;
    let half_sweep = (aim_angle & 0xFFFF) as f64 / 65536.0 * PI;
    let radians = if facing >= 0 {
        PI - half_sweep
    } else {
        PI + half_sweep
    };
    (
        (radians.sin() * DIR_SCALE) as i32,
        (-radians.cos() * DIR_SCALE) as i32,
    )
}

/// Scale a 16.16 velocity to a 1024-based unit vector. Zero stays zero.
fn normalize_1024(vx: i32, vy: i32) -> (i32, i32) {
    if vx == 0 && vy == 0 {
        return (0, 0);
    }
    let len = ((vx as f64) * (vx as f64) + (vy as f64) * (vy as f64)).sqrt();
    (
        (vx as f64 / len * 1024.0) as i32,
        (vy as f64 / len * 1024.0) as i32,
    )
}

/// WA's render surface size, read from the display. `None` when unavailable.
unsafe fn surface_size(display: usize) -> Option<(usize, usize)> {
    if display == 0 {
        return None;
    }
    const SURFACE: usize = 0x3d9c;
    let address = *((display + SURFACE) as *const u32) as usize;
    if address == 0 {
        return None;
    }
    let w = *((address + 20) as *const u32) as usize;
    let h = *((address + 24) as *const u32) as usize;
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }
    Some((w, h))
}

/// Build the screen-space L8 mask into the engine's cache.
///
/// The transform mirrors `gpu.rs::resolve`: `screen = world - clip + cam`, so
/// `world = screen + clip - cam`. Each screen pixel samples the fog light at
/// its world coordinate, with the `fog_floor` scheme value as a minimum.
unsafe fn build_mask_into(display: usize, clip: usize, w: usize, h: usize) {
    if clip == 0 {
        return;
    }
    let clip_x = *(clip as *const i32) >> 16;
    let clip_y = *((clip + 4) as *const i32) >> 16;
    let cam_x = *((display + DISPLAY_VIEW_X) as *const i32);
    let cam_y = *((display + DISPLAY_VIEW_Y) as *const i32);

    let Ok(mut g) = FOG.lock() else {
        return;
    };
    let Some(engine) = g.as_mut() else {
        return;
    };
    let pixels = w * h;
    if engine.mask.len() != pixels {
        engine.mask = vec![0; pixels];
        engine.mask_w = w;
        engine.mask_h = h;
    }
    let floor_pct = engine.fog.scheme().get("fog_floor").unwrap_or(0).clamp(0, 100);
    let floor = (floor_pct as u32 * 255 / 100) as u8;
    // `light_at` takes absolute landscape pixels and applies the grid origin
    // and discovery floor itself, so the world mapping below is all it needs.
    for sy in 0..h as i32 {
        let wy = sy + clip_y - cam_y;
        for sx in 0..w as i32 {
            let wx = sx + clip_x - cam_x;
            let light = if wx >= 0 && wy >= 0 {
                engine.fog.light_at(wx as usize, wy as usize)
            } else {
                0
            };
            engine.mask[sy as usize * w + sx as usize] = light.max(floor);
        }
    }
}

/// Take the most recent screen-space mask for the render pass. Returns the
/// cached buffer and its dimensions, or `None` when fog has not produced one
/// this match. The borrow is behind the mutex; callers that need it across a
/// frame should copy.
pub fn cached_mask() -> Option<(Vec<u8>, usize, usize)> {
    let Ok(g) = FOG.lock() else {
        return None;
    };
    let engine = g.as_ref()?;
    if engine.mask.is_empty() || engine.mask_w == 0 || engine.mask_h == 0 {
        return None;
    }
    Some((engine.mask.clone(), engine.mask_w, engine.mask_h))
}
