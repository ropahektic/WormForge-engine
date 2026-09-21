//! Install a PNG strip or GIF the same way PX `LoadSprite` / stock `LoadSprite` do.
//!
//! PX (`pxlog.txt`): after `Loading sprites.dat`, `Loading custom sprites` →
//! `InitGraphic@` → `LoadSprite(png, columns, rows)` → `CSprite->Index`.
//! That is `GfxHandler__LoadSprites` (0x570B50) returning in the world ctor.
//! Stock later flushes colour with `SetLayerVisibility(3, 1)` → `UpdatePalette`.
//!
//! In-match colour is the **map palette** in `PaletteContext.rgb_table` (256
//! slots, usually all in use by the time sprites.dat finishes). Frontend
//! `Graphics/*.pal` RIFF files are a different table. ProcessSprite's MapColor
//! only exact-matches the cache or allocates; when `free_count==0` we rewrite
//! the .spr RGB to the nearest live slot (3|dR|+5|dG|+2|dB|, same as
//! FindNearestCached 0x541340) and put that slot in the cache so MapColor
//! hits instead of returning 0xFFFFFFFF (white).

use crate::log_error;
#[cfg(debug_assertions)]
use crate::log_line;
use crate::rebase::rb;
use game::offsets::{
    CONSTRUCT_SPRITE, CUSTOM_SPRITE_SLOT_MAX, CUSTOM_SPRITE_SLOT_MIN, DISPLAY_GAME_LAYER,
    DISPLAY_LAYER_CONTEXTS, DISPLAY_LAYER_VISIBILITY, DISPLAY_SPRITE_BANKS, DISPLAY_SPRITE_CACHE,
    DISPLAY_SPRITE_LAYERS, DISPLAY_SPRITE_PTRS, MISSILE_ANIM_PHASE, MISSILE_RENDER_SPRITE,
    PROCESS_SPRITE,
};
use gfx::{encode_spr, FeetPlatform, SpriteStrip};
use std::sync::Mutex;

#[repr(C, align(8))]
struct SpriteObj([u8; 0x70]);

struct LoadedSpr {
    display: *mut u8,
    id: i32,
    layer: u32,
    _sprite: Box<SpriteObj>,
    _spr: Box<[u8]>,
}

unsafe impl Send for LoadedSpr {}

static LOADED: Mutex<Vec<LoadedSpr>> = Mutex::new(Vec::new());
/// Custom looping sheets: sprite id → animation_phase delta per FrameFinish.
static LOOP_DELTA: Mutex<Vec<(i32, u32)>> = Mutex::new(Vec::new());
static SPRITE_KEYS: Mutex<Vec<(String, SpriteRef)>> = Mutex::new(Vec::new());
/// Bottom opaque row of an installed GIF/PNG — the collision platform, like
/// stock donkey's horizontal rectangle in `masks.bmp`.
static SPRITE_FEET: Mutex<Vec<(i32, FeetPlatform)>> = Mutex::new(Vec::new());
static RGBA_SHEETS: Mutex<Vec<RgbaSheet>> = Mutex::new(Vec::new());

pub struct RgbaSheet {
    pub id: i32,
    pub w: u32,
    pub h: u32,
    #[allow(dead_code)]
    pub looping: bool,
    pub frames: Vec<Vec<u8>>,
}

#[derive(Clone, Copy, Debug)]
pub struct SpriteRef {
    pub id: i32,
    pub delta: u32,
    pub w: i32,
    pub h: i32,
}

const PAL_RGB: usize = 0x04;
const PAL_IN_USE: usize = 0x404;
const PAL_FREE_COUNT: usize = 0x504;
const PAL_CACHE_COUNT: usize = 0x606;
const PAL_CACHE: usize = 0x608;

pub fn clear() {
    #[cfg(debug_assertions)]
    PALETTE_DUMPED.store(false, std::sync::atomic::Ordering::SeqCst);
    if let Ok(mut g) = LOADED.lock() {
        for s in g.drain(..) {
            if s.display.is_null() {
                continue;
            }
            unsafe {
                let ptrs = (s.display as usize + DISPLAY_SPRITE_PTRS) as *mut *mut u8;
                if (*ptrs.add(s.id as usize)) == s._sprite.0.as_ptr() as *mut u8 {
                    *ptrs.add(s.id as usize) = std::ptr::null_mut();
                }
            }
        }
    }
    if let Ok(mut d) = LOOP_DELTA.lock() {
        d.clear();
    }
    if let Ok(mut k) = SPRITE_KEYS.lock() {
        k.clear();
    }
    if let Ok(mut f) = SPRITE_FEET.lock() {
        f.clear();
    }
    if let Ok(mut r) = RGBA_SHEETS.lock() {
        r.clear();
    }
    // The map's PaletteContext is rebuilt per match, so the slots go with it.
    // Only the bookkeeping has to be dropped, and doing it without touching the
    // old context avoids dereferencing one that has already been torn down.
    if let Ok(mut r) = RESERVED.lock() {
        r.clear();
    }
    clear_gpu_sheets();
}

/// Put installed sprites back into the display's sprite table.
///
/// Sprites are installed once per match, but WA repopulates its sprite pointer
/// table when a round begins, which drops every custom slot: the turret stops
/// drawing, and a worm sprite override then names an empty slot, so the worm
/// itself disappears. The sprite objects are still alive in LOADED for the
/// whole match, so this only has to put the pointers back.
///
/// Returns how many slots had been lost, which is 0 when nothing cleared them.
pub fn reassert() -> usize {
    let mut restored = 0;
    if let Ok(g) = LOADED.lock() {
        for s in g.iter() {
            if s.display.is_null() {
                continue;
            }
            unsafe {
                let ptrs = (s.display as usize + DISPLAY_SPRITE_PTRS) as *mut *mut u8;
                let slot = ptrs.add(s.id as usize);
                if (*slot).is_null() {
                    *slot = s._sprite.0.as_ptr() as *mut u8;
                    let layers = (s.display as usize + DISPLAY_SPRITE_LAYERS) as *mut u32;
                    *layers.add(s.id as usize) = s.layer;
                    restored += 1;
                }
            }
        }
    }
    restored
}

/// Stock missile render uses `animation_phase` as blit anim_value. Donkey's
/// anim_kind is 0x64 so FrameFinish does not advance it; we do, using GIF fps.
pub fn advance_loop(entity: *mut u8) {
    if entity.is_null() {
        return;
    }
    let sid = unsafe { *(entity.add(MISSILE_RENDER_SPRITE) as *const i32) };
    advance_loop_id(entity, sid, MISSILE_ANIM_PHASE);
}

pub fn loop_delta(id: i32) -> u32 {
    let Ok(g) = LOOP_DELTA.lock() else {
        return 0;
    };
    g.iter()
        .find(|(sid, _)| *sid == id)
        .map(|(_, d)| *d)
        .unwrap_or(0)
}

pub fn remember_key(key: String, sprite: SpriteRef) {
    if key.is_empty() || sprite.id == 0 {
        return;
    }
    if let Ok(mut g) = SPRITE_KEYS.lock() {
        g.retain(|(k, _)| k != &key);
        g.push((key, sprite));
    }
}

pub fn lookup_key(key: &str) -> Option<SpriteRef> {
    let g = SPRITE_KEYS.lock().ok()?;
    g.iter().find(|(k, _)| k == key).map(|(_, s)| *s)
}

pub fn advance_loop_id(entity: *mut u8, sid: i32, phase_off: usize) {
    if entity.is_null() {
        return;
    }
    let delta = loop_delta(sid);
    if delta == 0 {
        return;
    }
    unsafe {
        let phase = entity.add(phase_off) as *mut u32;
        *phase = (*phase).wrapping_add(delta);
    }
}

/// Display layer for sheets installed next, when set.
///
/// Sprites normally land on DISPLAY_GAME_LAYER (3), which draws over terrain.
/// Layers 1 and 2 also carry a PaletteContext, so a sheet can be installed
/// below the game layer to sit behind the landscape.
static FORCE_LAYER: Mutex<Option<u32>> = Mutex::new(None);
/// When set, sheets installed next take exact palette slots instead of snapping.
static FORCE_RESERVE: Mutex<bool> = Mutex::new(false);

pub fn set_reserve_colours(on: bool) {
    if let Ok(mut g) = FORCE_RESERVE.lock() {
        *g = on;
    }
}
#[cfg(debug_assertions)]
static PALETTE_DUMPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Palette slots this pack owns: (rgb, index).
///
/// A pack whose art the map palette cannot express -- a saturated team colour
/// against an earth-toned map -- puts its colours in unused slots instead of
/// snapping to whatever is nearest, which for pure blue is black.
///
/// The earlier attempt at this corrupted every pack that loaded later. Marking
/// a slot in use makes it a CANDIDATE for the nearest-colour search, so
/// wormcraft and worm_toss (alphabetically after sentry_gun) began snapping
/// their art onto our team colours. Reserved slots are therefore excluded from
/// that search: ours by exact match, invisible to everyone else.
static RESERVED: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
/// Ceiling on reserved slots, leaving the map most of its palette.
const RESERVE_BUDGET: usize = 64;
/// Weighted distance below which the map's own colour is good enough.
///
/// A slot spent on a colour the palette already has is a slot the map loses for
/// nothing. The tripod's greys sit among 24 greys and match closely; a
/// saturated team colour scores in the hundreds against an earth-toned palette,
/// and that is what deserves a slot.
const RESERVE_KEEP: i32 = 90;

/// Snapshot of reserved slots as a lookup table.
///
/// Taken once per nearest-colour search: testing the mutex per candidate would
/// take it roughly a million times over a pack's sheets.
fn reserved_mask() -> [bool; 256] {
    let mut mask = [false; 256];
    if let Ok(g) = RESERVED.lock() {
        for (_, slot) in g.iter() {
            if (*slot as usize) < mask.len() {
                mask[*slot as usize] = true;
            }
        }
    }
    mask
}

/// Give a sheet's own colours exact palette slots.
///
/// Called instead of quantize_to_map_palette for opted-in sheets. Writes each
/// colour into an unused slot so ProcessSprite's MapColor finds it exactly,
/// leaving the .spr palette untouched. Colours past the budget fall back to
/// snapping, so a pack cannot exhaust the palette.
unsafe fn reserve_palette_for(ctx: *mut u8, spr: &mut [u8]) -> Result<(u32, u32), String> {
    if spr.len() < 12 {
        return Err("truncated .spr".into());
    }
    let pal_n = u16::from_le_bytes([spr[10], spr[11]]) as usize;
    let mut exact = 0u32;
    let mut snapped = 0u32;
    for i in 0..pal_n {
        let o = 12 + i * 3;
        if o + 2 >= spr.len() {
            break;
        }
        let rgb = spr[o] as u32 | ((spr[o + 1] as u32) << 8) | ((spr[o + 2] as u32) << 16);
        let near = unsafe { nearest_with_cost(ctx, rgb) };
        let good_enough = near.is_some_and(|(_, cost)| cost <= RESERVE_KEEP);
        if !good_enough && unsafe { reserve_slot(ctx, rgb) }.is_some() {
            exact += 1;
            continue;
        }
        // Close enough already, or budget spent: snap, never onto a reserved slot.
        if let Some((slot, _)) = near {
            let [r, g, b] = unsafe { rgb_at(ctx, slot) };
            spr[o] = r;
            spr[o + 1] = g;
            spr[o + 2] = b;
            snapped += 1;
        }
    }
    Ok((exact, snapped))
}

/// Find or create the reserved slot holding `rgb`.
unsafe fn reserve_slot(ctx: *mut u8, rgb: u32) -> Option<u32> {
    let Ok(mut reserved) = RESERVED.lock() else {
        return None;
    };
    if let Some((_, slot)) = reserved.iter().find(|(c, _)| *c == rgb) {
        return Some(*slot);
    }
    if reserved.len() >= RESERVE_BUDGET {
        return None;
    }
    unsafe {
        let in_use = ctx.add(PAL_IN_USE);
        for i in 1..=255u32 {
            if *in_use.add(i as usize) != 0 {
                continue;
            }
            let p = ctx.add(PAL_RGB + i as usize * 4);
            *p = (rgb & 0xff) as u8;
            *p.add(1) = ((rgb >> 8) & 0xff) as u8;
            *p.add(2) = ((rgb >> 16) & 0xff) as u8;
            *in_use.add(i as usize) = 1;
            // MapColor exact-matches the CACHE, not the rgb table, and returns
            // 0xFFFFFFFF -- white -- when it misses with free_count at 0. A
            // reserved slot that is not cached is therefore an invisible slot:
            // the first attempt at this turned the blue turret white.
            ensure_cached(ctx, i);
            reserved.push((rgb, i));
            return Some(i);
        }
    }
    None
}

// A map palette holds only what the map needed -- one measured live had 106
// colours in use and exactly two blues, both desaturated slate -- so a
// saturated tint has no neighbour and snaps to whatever the weighted metric
// calls closest, which for pure blue is black.
//
// Writing colours into unreferenced slots to fix that was wrong: `free` (6) is
// WA's allocatable pool, not a count of spare indices, and the ~149 slots that
// looked free were merely unreferenced. Overwriting their RGB changed what
// those indices mean for every other sprite drawn from this PaletteContext.
//
// Colour for our own sheets is instead resolved on the GPU, where WA already
// uploads the frame as indices plus a palette texture. See crates/engine/src/
// shader.rs and the plan in .claude/plans/.

#[cfg(debug_assertions)]
unsafe fn dump_palette_once(ctx: *mut u8) {
    if PALETTE_DUMPED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    unsafe { dump_palette(ctx) };
}

pub fn set_force_layer(layer: Option<u32>) {
    if let Ok(mut g) = FORCE_LAYER.lock() {
        *g = layer;
    }
}

pub fn install_sheet(display: *mut u8, strip: &SpriteStrip) -> Result<i32, String> {
    if display.is_null() {
        return Err("display is null".into());
    }
    let mut spr = encode_spr(strip)?;
    spr.resize(spr.len() + 16, 0);

    let id = unsafe { free_slot(display) }.ok_or("no free sprite slot in 800..=1022")?;
    let forced = FORCE_LAYER.lock().ok().and_then(|g| *g);
    let (pal_ctx, layer) = match forced {
        Some(want) => {
            let base = display as usize + DISPLAY_LAYER_CONTEXTS;
            let ctx = unsafe { *((base + (want as usize) * 4) as *const usize) };
            if ctx != 0 {
                (ctx as *mut u8, want)
            } else {
                log_error(&format!(
                    "layer {want} has no PaletteContext; using default"
                ));
                unsafe { palette_ctx(display) }
            }
        }
        None => unsafe { palette_ctx(display) },
    };
    if pal_ctx.is_null() {
        return Err("no PaletteContext on display layers 1-3".into());
    }
    let cache = unsafe { *((display as usize + DISPLAY_SPRITE_CACHE) as *const *mut u8) };
    if cache.is_null() {
        return Err("DisplayBase.sprite_cache is null".into());
    }

    let free_before = unsafe { *(pal_ctx.add(PAL_FREE_COUNT) as *const i16) };
    let cache_n = unsafe { *(pal_ctx.add(PAL_CACHE_COUNT) as *const i16) };
    #[cfg(not(debug_assertions))]
    let _ = (free_before, cache_n);

    let mut sprite = Box::new(SpriteObj([0u8; 0x70]));
    let mut spr = spr.into_boxed_slice();
    unsafe {
        #[cfg(debug_assertions)]
        dump_palette_once(pal_ctx);
        let reserve = FORCE_RESERVE.lock().map(|g| *g).unwrap_or(false);
        if reserve {
            let (exact, snapped) = reserve_palette_for(pal_ctx, &mut spr)?;
            #[cfg(debug_assertions)]
            {
                let held = RESERVED.lock().map(|g| g.len()).unwrap_or(0);
                log_line(&format!(
                    "LoadSprite equivalent: slot {id} layer {layer} free={free_before} cache={cache_n} exact={exact} snapped={snapped} reserved_total={held}"
                ));
            }
            #[cfg(not(debug_assertions))]
            let _ = (exact, snapped);
        } else {
            let snapped = quantize_to_map_palette(pal_ctx, &mut spr)?;
            #[cfg(debug_assertions)]
            log_line(&format!(
                "LoadSprite equivalent: slot {id} layer {layer} free={free_before} cache={cache_n} quantized={snapped}"
            ));
            #[cfg(not(debug_assertions))]
            let _ = snapped;
        }
        construct_sprite(sprite.0.as_mut_ptr(), cache);
        let ok = process_sprite(sprite.0.as_mut_ptr(), pal_ctx, spr.as_mut_ptr());
        if ok == 0 {
            return Err("ProcessSprite returned 0".into());
        }
        let ptrs = (display as usize + DISPLAY_SPRITE_PTRS) as *mut *mut u8;
        let layers = (display as usize + DISPLAY_SPRITE_LAYERS) as *mut u32;
        let vis = (display as usize + DISPLAY_LAYER_VISIBILITY) as *mut u32;
        *ptrs.add(id as usize) = sprite.0.as_mut_ptr();
        *layers.add(id as usize) = layer;
        *vis.add(layer as usize) = (*vis.add(layer as usize)).wrapping_add(1);
        #[cfg(debug_assertions)]
        {
            let free_after = *(pal_ctx.add(PAL_FREE_COUNT) as *const i16);
            log_line(&format!(
                "installed .spr at sprite id {id} layer {layer} ({} frames loop={} delta={} free={})",
                strip.frames.len(),
                strip.looping,
                strip.phase_delta(),
                free_after
            ));
        }
    }

    if let Ok(mut g) = LOADED.lock() {
        g.push(LoadedSpr {
            display,
            id,
            layer,
            _sprite: sprite,
            _spr: spr,
        });
    }
    let delta = strip.phase_delta();
    if delta != 0 {
        if let Ok(mut d) = LOOP_DELTA.lock() {
            d.retain(|(existing, _)| *existing != id);
            d.push((id, delta));
        }
        #[cfg(debug_assertions)]
        log_line(&format!("sprite {id} GIF loop delta {delta}"));
    }
    if let Some(feet) = strip.feet_platform() {
        if let Ok(mut g) = SPRITE_FEET.lock() {
            g.retain(|(sid, _)| *sid != id);
            g.push((id, feet));
        }
        #[cfg(debug_assertions)]
        log_line(&format!(
            "sprite {id} platform x0={} x1={} y={} (sheet {}x{})",
            feet.x0, feet.x1, feet.y, feet.width, feet.height
        ));
    }
    remember_rgba(id, strip);
    Ok(id)
}

pub fn feet_for_id(id: i32) -> Option<FeetPlatform> {
    let g = SPRITE_FEET.lock().ok()?;
    g.iter().find(|(sid, _)| *sid == id).map(|(_, f)| *f)
}

fn remember_rgba(id: i32, strip: &SpriteStrip) {
    let Some(first) = strip.frames.first() else {
        return;
    };
    let w = first.width;
    let h = first.height;
    let mut frames = Vec::with_capacity(strip.frames.len());
    for frame in &strip.frames {
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            let o = i * 4;
            if o + 3 >= frame.rgba.len() {
                continue;
            }
            let r = frame.rgba[o];
            let g = frame.rgba[o + 1];
            let b = frame.rgba[o + 2];
            let a = frame.rgba[o + 3];
            if a < 16 {
                continue;
            }
            bgra[o] = b;
            bgra[o + 1] = g;
            bgra[o + 2] = r;
            bgra[o + 3] = 255;
        }
        punch_letterbox(&mut bgra, w, h);
        frames.push(bgra);
    }
    if let Ok(mut g) = RGBA_SHEETS.lock() {
        g.retain(|s| s.id != id);
        g.push(RgbaSheet {
            id,
            w,
            h,
            looping: strip.looping,
            frames,
        });
    }
}

/// Letterbox / backdrop black connected to the top or bottom edge becomes
/// transparent. Interior blacks (goggles, outlines) stay opaque so the
/// portrait does not punch through to the shader GIF.
fn punch_letterbox(bgra: &mut [u8], w: u32, h: u32) {
    let w = w as usize;
    let h = h as usize;
    if w == 0 || h == 0 {
        return;
    }
    let key = |bgra: &[u8], i: usize| {
        let o = i * 4;
        let b = bgra[o] as u16;
        let g = bgra[o + 1] as u16;
        let r = bgra[o + 2] as u16;
        bgra[o + 3] < 16 || r + g + b < 18
    };
    let mut seen = vec![false; w * h];
    let mut stack = Vec::new();
    for x in 0..w {
        let top = x;
        let bot = (h - 1) * w + x;
        if key(bgra, top) {
            stack.push(top);
        }
        if key(bgra, bot) {
            stack.push(bot);
        }
    }
    while let Some(i) = stack.pop() {
        if seen[i] {
            continue;
        }
        seen[i] = true;
        let x = i % w;
        let y = i / w;
        if x > 0 {
            let n = i - 1;
            if !seen[n] && key(bgra, n) {
                stack.push(n);
            }
        }
        if x + 1 < w {
            let n = i + 1;
            if !seen[n] && key(bgra, n) {
                stack.push(n);
            }
        }
        if y > 0 {
            let n = i - w;
            if !seen[n] && key(bgra, n) {
                stack.push(n);
            }
        }
        if y + 1 < h {
            let n = i + w;
            if !seen[n] && key(bgra, n) {
                stack.push(n);
            }
        }
    }
    for (i, punch) in seen.iter().enumerate() {
        if !*punch {
            continue;
        }
        let o = i * 4;
        bgra[o] = 0;
        bgra[o + 1] = 0;
        bgra[o + 2] = 0;
        bgra[o + 3] = 0;
    }
}

/// Frames in an installed sheet, for turning a frame index into an anim phase.
pub fn frame_count(id: i32) -> Option<usize> {
    let g = RGBA_SHEETS.lock().ok()?;
    g.iter().find(|s| s.id == id).map(|s| s.frames.len())
}

pub fn overlay_frame(id: i32, phase: u32) -> Option<(u32, u32, Vec<u8>)> {
    let g = RGBA_SHEETS.lock().ok()?;
    let sheet = g.iter().find(|s| s.id == id)?;
    let n = sheet.frames.len();
    if n == 0 {
        return None;
    }
    let idx = ((phase as u64).saturating_mul(n as u64) >> 16) as usize % n;
    Some((sheet.w, sheet.h, sheet.frames[idx].clone()))
}

unsafe fn free_slot(display: *mut u8) -> Option<i32> {
    unsafe {
        let ptrs = (display as usize + DISPLAY_SPRITE_PTRS) as *const *mut u8;
        let banks = (display as usize + DISPLAY_SPRITE_BANKS) as *const *mut u8;
        for id in CUSTOM_SPRITE_SLOT_MIN..=CUSTOM_SPRITE_SLOT_MAX {
            if (*ptrs.add(id as usize)).is_null() && (*banks.add(id as usize)).is_null() {
                return Some(id);
            }
        }
        None
    }
}

/// Which display layers carry a PaletteContext.
///
/// Sprites install on the first one found, which is always the game layer (3).
/// Drawing behind terrain would mean installing on a lower layer instead, so
/// this records which are even present before that can be attempted.
#[cfg(debug_assertions)]
pub fn report_layers(display: *mut u8) {
    if display.is_null() {
        return;
    }
    let base = display as usize + DISPLAY_LAYER_CONTEXTS;
    let mut found = Vec::new();
    for layer in 0..4u32 {
        let ctx = unsafe { *((base + (layer as usize) * 4) as *const usize) };
        found.push(format!("{layer}={}", if ctx != 0 { "ctx" } else { "none" }));
    }
    log_line(&format!("display layer contexts: {}", found.join(" ")));
}

unsafe fn palette_ctx(display: *mut u8) -> (*mut u8, u32) {
    unsafe {
        let base = display as usize + DISPLAY_LAYER_CONTEXTS;
        for layer in [DISPLAY_GAME_LAYER, 1, 2] {
            let ctx = *((base + (layer as usize) * 4) as *const usize);
            if ctx != 0 {
                return (ctx as *mut u8, layer);
            }
        }
        (std::ptr::null_mut(), 0)
    }
}

/// Rewrite .spr RGB to the live map palette so ProcessSprite MapColor exact-matches
/// instead of allocating (or returning 0xFF when the table is full).
unsafe fn quantize_to_map_palette(pal_ctx: *mut u8, spr: &mut [u8]) -> Result<u32, String> {
    if spr.len() < 12 {
        return Err("truncated .spr".into());
    }
    let pal_n = u16::from_le_bytes([spr[10], spr[11]]) as usize;
    if pal_n == 0 {
        return Ok(0);
    }
    let cache_n = unsafe { *(pal_ctx.add(PAL_CACHE_COUNT) as *const i16) };
    let use_cache_only = cache_n >= 256;
    let mut snapped = 0u32;
    for i in 0..pal_n {
        let o = 12 + i * 3;
        if o + 2 >= spr.len() {
            break;
        }
        let rgb = spr[o] as u32 | ((spr[o + 1] as u32) << 8) | ((spr[o + 2] as u32) << 16);
        let Some(slot) = (unsafe {
            if use_cache_only {
                nearest_in_cache(pal_ctx, rgb)
            } else {
                nearest_in_use(pal_ctx, rgb)
            }
        }) else {
            return Err("map palette has no in-use colours".into());
        };
        let [r, g, b] = unsafe { rgb_at(pal_ctx, slot) };
        if [r, g, b] != [spr[o], spr[o + 1], spr[o + 2]] {
            spr[o] = r;
            spr[o + 1] = g;
            spr[o + 2] = b;
            snapped += 1;
        }
        unsafe {
            ensure_cached(pal_ctx, slot);
        }
    }
    Ok(snapped)
}

unsafe fn ensure_cached(ctx: *mut u8, slot: u32) {
    if slot == 0 || slot > 255 {
        return;
    }
    unsafe {
        let n = *(ctx.add(PAL_CACHE_COUNT) as *const i16);
        let search = (n as usize).min(256);
        for i in 0..search {
            if *ctx.add(PAL_CACHE + i) as u32 == slot {
                return;
            }
        }
        if n >= 256 {
            return;
        }
        *ctx.add(PAL_CACHE + n as usize) = slot as u8;
        *(ctx.add(PAL_CACHE_COUNT) as *mut i16) = n + 1;
    }
}

unsafe fn nearest_in_cache(ctx: *mut u8, rgb: u32) -> Option<u32> {
    let tr = (rgb & 0xff) as i32;
    let tg = ((rgb >> 8) & 0xff) as i32;
    let tb = ((rgb >> 16) & 0xff) as i32;
    let mut best = i32::MAX;
    let mut slot = 0u32;
    unsafe {
        let n = (*(ctx.add(PAL_CACHE_COUNT) as *const i16) as usize).min(256);
        for i in 0..n {
            let s = *ctx.add(PAL_CACHE + i) as u32;
            if s == 0 {
                continue;
            }
            let [r, g, b] = rgb_at(ctx, s);
            let dist =
                (r as i32 - tr).abs() * 3 + (g as i32 - tg).abs() * 5 + (b as i32 - tb).abs() * 2;
            if dist < best {
                best = dist;
                slot = s;
            }
        }
    }
    if slot == 0 {
        None
    } else {
        Some(slot)
    }
}

unsafe fn rgb_at(ctx: *mut u8, slot: u32) -> [u8; 3] {
    unsafe {
        let p = ctx.add(PAL_RGB + slot as usize * 4);
        [*p, *p.add(1), *p.add(2)]
    }
}

/// Log the map palette's in-use colours, grouped by hue family.
///
/// Sprites are snapped to this set, so a tint with no neighbour here cannot
/// survive: a saturated blue in an earth-toned palette lands on whatever is
/// closest, which was black. Knowing what the palette actually holds is the
/// difference between choosing team colours and guessing them.
/// What the live map palette actually holds, one line per match.
///
/// Colour-team choices need to know whether blues exist before picking team
/// colours, and which families saturate the table. Gated like its only caller:
/// a Release build neither runs nor compiles it.
#[cfg(debug_assertions)]
pub unsafe fn dump_palette(ctx: *mut u8) {
    if ctx.is_null() {
        return;
    }
    let mut families = [0u32; 8];
    let mut blues: Vec<String> = Vec::new();
    let mut n = 0;
    unsafe {
        let in_use = ctx.add(PAL_IN_USE);
        for i in 1..=255u32 {
            if *in_use.add(i as usize) == 0 {
                continue;
            }
            n += 1;
            let [r, g, b] = rgb_at(ctx, i);
            let (r, g, b) = (r as i32, g as i32, b as i32);
            let mx = r.max(g).max(b);
            let mn = r.min(g).min(b);
            let idx = if mx - mn < 24 {
                0 // grey
            } else if r >= g && r >= b {
                if g > b {
                    1
                } else {
                    2
                } // red/orange, red/magenta
            } else if g >= r && g >= b {
                if r > b {
                    3
                } else {
                    4
                } // yellow/green, green/cyan
            } else if r > g {
                5 // magenta/violet
            } else {
                6 // blue/cyan
            };
            families[idx] += 1;
            if idx == 6 && blues.len() < 12 {
                blues.push(format!("{r},{g},{b}"));
            }
        }
    }
    log_line(&format!(
        "map palette: {n} in use -- grey {} red {} redmag {} yelgrn {} grncyn {} magvio {} blue {}",
        families[0], families[1], families[2], families[3], families[4], families[5], families[6]
    ));
    log_line(&format!("map palette blues: {}", blues.join("  ")));
}

unsafe fn nearest_in_use(ctx: *mut u8, rgb: u32) -> Option<u32> {
    unsafe { nearest_with_cost(ctx, rgb) }.map(|(slot, _)| slot)
}

/// Nearest in-use slot and how poor the match is.
unsafe fn nearest_with_cost(ctx: *mut u8, rgb: u32) -> Option<(u32, i32)> {
    let reserved = reserved_mask();
    let tr = (rgb & 0xff) as i32;
    let tg = ((rgb >> 8) & 0xff) as i32;
    let tb = ((rgb >> 16) & 0xff) as i32;
    let mut best = i32::MAX;
    let mut slot = 0u32;
    unsafe {
        let in_use = ctx.add(PAL_IN_USE);
        for i in 1..=255u32 {
            if *in_use.add(i as usize) == 0 || reserved[i as usize] {
                continue;
            }
            let [r, g, b] = rgb_at(ctx, i);
            let dist =
                (r as i32 - tr).abs() * 3 + (g as i32 - tg).abs() * 5 + (b as i32 - tb).abs() * 2;
            if dist < best {
                best = dist;
                slot = i;
            }
        }
    }
    if slot == 0 {
        None
    } else {
        Some((slot, best))
    }
}

unsafe fn construct_sprite(sprite: *mut u8, cache: *mut u8) {
    let f = rb(CONSTRUCT_SPRITE) as usize;
    unsafe {
        core::arch::asm!(
            "call {f}",
            f = in(reg) f,
            inout("eax") sprite => _,
            inout("ecx") cache => _,
            out("edx") _,
            options(nostack),
        );
    }
}

unsafe fn process_sprite(sprite: *mut u8, pal_ctx: *mut u8, raw: *mut u8) -> u32 {
    let f = rb(PROCESS_SPRITE) as usize;
    let ret: u32;
    unsafe {
        core::arch::asm!(
            "push {raw}",
            "call {f}",
            raw = in(reg) raw,
            f = in(reg) f,
            inout("eax") sprite => ret,
            in("ecx") pal_ctx,
            out("edx") _,
            options(nostack),
        );
    }
    ret
}

// ---------------------------------------------------------------------------
// GPU sheets
//
// A sheet installed for the GPU renderer gets an id but no WA sprite slot: no
// `.spr` is encoded, no palette is quantised or reserved, no entry is written
// into `DISPLAY_SPRITE_PTRS`. That is the entire point. Those 223 slots
// (CUSTOM_SPRITE_SLOT_MIN..=MAX) are shared by every loaded pack, and the
// sentry gun alone spends 72 of them on baked rotations that the GPU path
// does with one float.
//
// Skipping the palette also removes the two historical corruption regressions
// at their source: `quantize_to_map_palette` and `reserve_palette_for` simply
// never run for these sheets, so nothing they do can repaint the scene. They
// keep more than 254 colours and real partial alpha as a side effect.

/// Ids are `0x0100_0000 | n`, chosen so `id & 0xffff` can never collide with
/// a real stock sprite index. Anything that treats an id as an index into
/// WA's 1023-entry sprite table must reject these -- see `is_virtual`.
pub const VIRTUAL_SPRITE_BASE: i32 = 0x0100_0000;

/// A GPU sheet: where each frame lives in the atlas, plus the animation
/// properties `a:frame` and `a:advance` need.
pub struct GpuSheet {
    pub id: i32,
    pub frames: Vec<render::SheetRect>,
    pub looping: bool,
}

static GPU_SHEETS: Mutex<Vec<GpuSheet>> = Mutex::new(Vec::new());
static NEXT_VIRTUAL: Mutex<i32> = Mutex::new(VIRTUAL_SPRITE_BASE);

pub fn is_virtual(id: i32) -> bool {
    id >= VIRTUAL_SPRITE_BASE
}

/// True when this id was installed for the GPU renderer.
pub fn is_gpu_sheet(id: u32) -> bool {
    GPU_SHEETS
        .lock()
        .map(|g| g.iter().any(|s| (s.id as u32) == id))
        .unwrap_or(false)
}

/// Atlas rectangles and animation properties for a GPU sheet.
#[allow(clippy::type_complexity)]
pub fn gpu_frame(
    id: u32,
) -> Option<(impl Fn(usize) -> Option<render::SheetRect>, usize, bool)> {
    let sheets = GPU_SHEETS.lock().ok()?;
    let sheet = sheets.iter().find(|s| (s.id as u32) == id)?;
    let frames = sheet.frames.clone();
    let count = frames.len();
    let looping = sheet.looping;
    Some((move |i: usize| frames.get(i).copied(), count, looping))
}

/// Install a sheet for the GPU renderer.
///
/// Populates exactly the WormForge-side tables the rest of the engine reads --
/// feet/collision, frame count, loop delta, name lookup -- and nothing that
/// touches WA's sprite table or the map palette. `a:move`, `a:on_feet`,
/// `a:frame`, `a:look` and explosion placement all keep working because none
/// of them ever consulted WA's table in the first place.
pub fn install_virtual(strip: &gfx::SpriteStrip, rects: Vec<render::SheetRect>) -> i32 {
    let id = {
        let Ok(mut next) = NEXT_VIRTUAL.lock() else {
            return 0;
        };
        let id = *next;
        *next += 1;
        id
    };

    if let Ok(mut g) = GPU_SHEETS.lock() {
        g.retain(|s| s.id != id);
        g.push(GpuSheet { id, frames: rects, looping: strip.looping });
    }
    if let Some(feet) = strip.feet_platform() {
        if let Ok(mut g) = SPRITE_FEET.lock() {
            g.retain(|(s, _)| *s != id);
            g.push((id, feet));
        }
    }
    let delta = strip.phase_delta();
    if delta > 0 {
        if let Ok(mut g) = LOOP_DELTA.lock() {
            g.retain(|(s, _)| *s != id);
            g.push((id, delta));
        }
    }
    // frame_count() reads RGBA_SHEETS, which a:frame needs to invert its phase.
    if let Ok(mut g) = RGBA_SHEETS.lock() {
        g.retain(|s| s.id != id);
        g.push(RgbaSheet {
            id,
            w: strip.frames.first().map(|f| f.width).unwrap_or(0),
            h: strip.frames.first().map(|f| f.height).unwrap_or(0),
            looping: strip.looping,
            frames: Vec::new(), // the atlas holds the pixels now
        });
    }
    id
}

/// Drop every GPU sheet. Called with the rest of the per-match sprite state.
pub fn clear_gpu_sheets() {
    if let Ok(mut g) = GPU_SHEETS.lock() {
        g.clear();
    }
    if let Ok(mut next) = NEXT_VIRTUAL.lock() {
        *next = VIRTUAL_SPRITE_BASE;
    }
}
