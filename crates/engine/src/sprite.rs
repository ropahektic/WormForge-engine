//! Install a PNG strip or GIF as a WA `.spr` via ConstructSprite + ProcessSprite.
//!
//! On palette overflow, rewrite the sheet onto existing live colours (read-only
//! nearest) so MapColor does not land on index 255 (solid white). Washed, but
//! usable — real fix waits on Edlud's render PR. Never steal map slots.

use crate::log_line;
use game::offsets::{
    CUSTOM_SPRITE_SENTINEL, CUSTOM_SPRITE_SLOT_MAX, CUSTOM_SPRITE_SLOT_MIN, DISPLAY_GAME_LAYER,
    DISPLAY_LAYER_CONTEXTS, DISPLAY_LAYER_VISIBILITY, DISPLAY_SPRITE_BANKS, DISPLAY_SPRITE_CACHE,
    DISPLAY_SPRITE_LAYERS, DISPLAY_SPRITE_PTRS, MISSILE_ANIM_PHASE, MISSILE_RENDER_SPRITE,
};
use gfx::{encode_spr, spr_key, FeetPlatform, OpaqueMask, SpriteStrip};
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
struct StockAlias {
    display: usize,
    target: usize,
    custom_ptr: usize,
    custom_layer: u32,
    original_ptr: usize,
    original_layer: u32,
}

static STOCK_ALIASES: Mutex<Vec<StockAlias>> = Mutex::new(Vec::new());
/// Custom looping sheets: sprite id → animation_phase delta per FrameFinish.
static LOOP_DELTA: Mutex<Vec<(i32, u32)>> = Mutex::new(Vec::new());
static SPRITE_KEYS: Mutex<Vec<(String, SpriteRef)>> = Mutex::new(Vec::new());
/// Bottom opaque row of an installed GIF/PNG — the collision platform, like
/// stock donkey's horizontal rectangle in `masks.bmp`.
static SPRITE_FEET: Mutex<Vec<(i32, FeetPlatform)>> = Mutex::new(Vec::new());
/// First-frame opaque silhouette for `mask = true` actors.
/// Multi-frame sheets keep one mask per GIF frame (phase picks which) —
/// experimental animated collision.
struct MaskSheet {
    looping: bool,
    frames: Vec<OpaqueMask>,
}

static SPRITE_MASKS: Mutex<Vec<(i32, MaskSheet)>> = Mutex::new(Vec::new());
static RGBA_SHEETS: Mutex<Vec<RgbaSheet>> = Mutex::new(Vec::new());

pub struct RgbaSheet {
    pub id: i32,
    pub w: u32,
    pub h: u32,
    #[allow(dead_code)]
    pub looping: bool,
    pub frames: Vec<Vec<u8>>,
    // Source alpha is gameplay data for land.stamp, independent of the GPU atlas.
    pub terrain_frames: Vec<gfx::RgbaFrame>,
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
const PAL_FREE_LIST: usize = 0x506;
const PAL_CACHE_COUNT: usize = 0x606;
const PAL_CACHE: usize = 0x608;
const PAL_DIRTY: usize = 0x708;

pub fn clear() {
    clear_gpu_sheets();
    // Restore aliases before dropping the custom sprites they point to.
    if let Ok(mut aliases) = STOCK_ALIASES.lock() {
        for a in aliases.drain(..) {
            unsafe {
                let ptrs = (a.display + DISPLAY_SPRITE_PTRS) as *mut *mut u8;
                let layers = (a.display + DISPLAY_SPRITE_LAYERS) as *mut u32;
                if *ptrs.add(a.target) as usize == a.custom_ptr {
                    *ptrs.add(a.target) = a.original_ptr as *mut u8;
                    *layers.add(a.target) = a.original_layer;
                }
            }
        }
    }
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
    if let Ok(mut m) = SPRITE_MASKS.lock() {
        m.clear();
    }
    if let Ok(mut r) = RGBA_SHEETS.lock() {
        r.clear();
    }
}

/// Take our sprites out of WA's sprite table before WA tears the display down.
///
/// `DisplayBase::Destructor` walks slots 1..0x3FF and deletes every sprite it
/// finds (scalar deleting destructor, then WA's `free`). Ours are Rust
/// allocations: WA must never delete them. Called by the `GameEngine::Shutdown`
/// hook BEFORE the original. The objects themselves stay alive until the next
/// match's `clear`, and forgetting the display keeps `clear` and `reassert`
/// from ever writing into the freed one.
pub fn release_before_teardown() {
    // Stock aliases first: a mole slot pointing at our sprite would have WA
    // delete our object -- twice, since the custom slot holds it as well --
    // and leak its own. Put each original back so WA frees what it owns.
    if let Ok(mut aliases) = STOCK_ALIASES.lock() {
        for a in aliases.drain(..) {
            unsafe {
                let ptrs = (a.display + DISPLAY_SPRITE_PTRS) as *mut *mut u8;
                let layers = (a.display + DISPLAY_SPRITE_LAYERS) as *mut u32;
                if *ptrs.add(a.target) as usize == a.custom_ptr {
                    *ptrs.add(a.target) = a.original_ptr as *mut u8;
                    *layers.add(a.target) = a.original_layer;
                }
            }
        }
    }
    if let Ok(mut g) = LOADED.lock() {
        for s in g.iter_mut() {
            if s.display.is_null() {
                continue;
            }
            unsafe {
                let ptrs = (s.display as usize + DISPLAY_SPRITE_PTRS) as *mut *mut u8;
                if *ptrs.add(s.id as usize) == s._sprite.0.as_ptr() as *mut u8 {
                    *ptrs.add(s.id as usize) = std::ptr::null_mut();
                }
            }
            s.display = std::ptr::null_mut();
        }
    }
}

/// Put installed sprites back into the display's sprite table.
///
/// Sprites are installed once per match, but WA repopulates its sprite pointer
/// table when a round begins, which drops every custom slot.
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
    if let Ok(mut aliases) = STOCK_ALIASES.lock() {
        for a in aliases.iter_mut() {
            unsafe {
                let ptrs = (a.display + DISPLAY_SPRITE_PTRS) as *mut *mut u8;
                let layers = (a.display + DISPLAY_SPRITE_LAYERS) as *mut u32;
                let slot = ptrs.add(a.target);
                if *slot as usize != a.custom_ptr {
                    if !(*slot).is_null() {
                        a.original_ptr = *slot as usize;
                        a.original_layer = *layers.add(a.target);
                    }
                    *slot = a.custom_ptr as *mut u8;
                    *layers.add(a.target) = a.custom_layer;
                    restored += 1;
                }
            }
        }
    }
    restored
}

/// PX-style SetSpriteOverride for stock Gfx.dir IDs. First loaded pack wins.
/// The original pointer is restored on clear; reassert handles WA rebuilding
/// the sprite table at the start of a round.
pub fn alias_stock(
    display: *mut u8,
    target: i32,
    source: i32,
    strip: &SpriteStrip,
) -> Result<bool, String> {
    if display.is_null() || !(1..800).contains(&target) {
        return Err("invalid stock sprite override target".into());
    }
    let (custom_ptr, custom_layer) = {
        let loaded = LOADED.lock().map_err(|_| "sprite table lock poisoned")?;
        let sprite = loaded
            .iter()
            .find(|s| s.display == display && s.id == source)
            .ok_or("stock sprite override source was not installed")?;
        (sprite._sprite.0.as_ptr() as usize, sprite.layer)
    };
    let mut aliases = STOCK_ALIASES.lock().map_err(|_| "sprite alias lock poisoned")?;
    if aliases
        .iter()
        .any(|a| a.display == display as usize && a.target == target as usize)
    {
        return Ok(false);
    }
    unsafe {
        let ptrs = (display as usize + DISPLAY_SPRITE_PTRS) as *mut *mut u8;
        let layers = (display as usize + DISPLAY_SPRITE_LAYERS) as *mut u32;
        let original_ptr = *ptrs.add(target as usize) as usize;
        if original_ptr == 0 {
            return Err(format!("stock sprite {target} is unavailable"));
        }
        let original_layer = *layers.add(target as usize);
        *ptrs.add(target as usize) = custom_ptr as *mut u8;
        *layers.add(target as usize) = custom_layer;
        aliases.push(StockAlias {
            display: display as usize,
            target: target as usize,
            custom_ptr,
            custom_layer,
            original_ptr,
            original_layer,
        });
    }
    remember_rgba(target, strip);
    log_line(&format!(
        "stock sprite {target} overridden by custom sprite {source}"
    ));
    Ok(true)
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

/// Phase step for LuaActor strip playback.
///
/// Custom GIF/PNG sheets use their installed fps. Stock multi-frame sheets
/// (banana, grenade, clustlet, …) are not in `LOOP_DELTA`, so without a
/// fallback `ACTOR_PHASE` stays 0 and bits look frozen — stock MissileEntity
/// tumbles those sheets via its own anim_kind path.
pub fn actor_loop_delta(id: i32) -> u32 {
    let d = loop_delta(id);
    if d != 0 {
        return d;
    }
    if id <= 0 || id >= CUSTOM_SPRITE_SENTINEL as i32 {
        return 0;
    }
    // Known stock sheet lengths; everything else assumes the common 32-frame tumble.
    let n = match id {
        54 => 6u64, // clustlet
        _ => 32u64,
    };
    let fps = 10u64;
    ((0x10000 * fps) / (n * 50)).max(1) as u32
}

pub fn remember_key(key: String, sprite: SpriteRef) {
    if key.is_empty() || sprite.id == 0 {
        return;
    }
    if let Ok(mut g) = SPRITE_KEYS.lock() {
        g.retain(|(k, _)| k != &key);
        g.push((key.clone(), sprite));
        if let Some((_, rel)) = key.split_once(':') {
            if !rel.is_empty() && rel != key {
                g.retain(|(k, _)| k != rel);
                g.push((rel.to_string(), sprite));
            }
        }
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

pub fn install_sheet(display: *mut u8, strip: &SpriteStrip) -> Result<i32, String> {
    if display.is_null() {
        return Err("display is null".into());
    }
    let mut spr = encode_spr(strip)?;
    spr.resize(spr.len() + 16, 0);

    let id = unsafe { free_slot(display) }.ok_or("no free sprite slot in 800..=1022")?;
    let (pal_ctx, layer) = unsafe { palette_ctx(display) };
    if pal_ctx.is_null() {
        return Err("no PaletteContext on display layers 1-3".into());
    }
    let cache = unsafe { *((display as usize + DISPLAY_SPRITE_CACHE) as *const *mut u8) };
    if cache.is_null() {
        return Err("DisplayBase.sprite_cache is null".into());
    }

    let free_before = unsafe { *(pal_ctx.add(PAL_FREE_COUNT) as *const i16) };

    let mut sprite = Box::new(SpriteObj([0u8; 0x70]));
    let mut spr = spr.into_boxed_slice();
    unsafe {
        // Snap overflow colours onto live slots before ProcessSprite so MapColor
        // exact-hits them (raw overflow → index 255 = solid white).
        let (kept, allocated, nearest) = wash_spr_palette(pal_ctx, &mut spr)?;
        construct_sprite(sprite.0.as_mut_ptr(), cache);
        let ok = process_sprite(sprite.0.as_mut_ptr(), pal_ctx, spr.as_mut_ptr());
        if ok == 0 {
            return Err("ProcessSprite returned 0".into());
        }
        let free_after = *(pal_ctx.add(PAL_FREE_COUNT) as *const i16);
        log_line(&format!(
            "LoadSprite equivalent: slot {id} layer {layer} free={free_before}->{free_after} kept={kept} alloc={allocated} nearest={nearest}"
        ));
        let ptrs = (display as usize + DISPLAY_SPRITE_PTRS) as *mut *mut u8;
        let layers = (display as usize + DISPLAY_SPRITE_LAYERS) as *mut u32;
        let vis = (display as usize + DISPLAY_LAYER_VISIBILITY) as *mut u32;
        *ptrs.add(id as usize) = sprite.0.as_mut_ptr();
        *layers.add(id as usize) = layer;
        *vis.add(layer as usize) = (*vis.add(layer as usize)).wrapping_add(1);
        log_line(&format!(
            "installed .spr at sprite id {id} layer {layer} ({} frames loop={} delta={} free={})",
            strip.frames.len(),
            strip.looping,
            strip.phase_delta(),
            free_after
        ));
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
        log_line(&format!("sprite {id} GIF loop delta {delta}"));
    }
    if let Some(feet) = strip.feet_platform() {
        if let Ok(mut g) = SPRITE_FEET.lock() {
            g.retain(|(sid, _)| *sid != id);
            g.push((id, feet));
        }
        log_line(&format!(
            "sprite {id} platform x0={} x1={} y={} (sheet {}x{})",
            feet.x0, feet.x1, feet.y, feet.width, feet.height
        ));
    }
    remember_mask(id, strip);
    remember_rgba(id, strip);
    Ok(id)
}

pub fn frame_count(id: i32) -> Option<usize> {
    if let Ok(g) = GPU_SHEETS.lock() {
        if let Some(s) = g.iter().find(|s| s.id == id) {
            return Some(s.frames.len());
        }
    }
    let g = RGBA_SHEETS.lock().ok()?;
    g.iter().find(|s| s.id == id).map(|s| s.frames.len())
}

pub fn feet_for_id(id: i32) -> Option<FeetPlatform> {
    let g = SPRITE_FEET.lock().ok()?;
    g.iter().find(|(sid, _)| *sid == id).map(|(_, f)| *f)
}

pub fn mask_for_id(id: i32) -> Option<OpaqueMask> {
    mask_for_phase(id, 0)
}

/// Mask for the same frame the blit shows (`animation_phase` / `ACTOR_PHASE`).
pub fn mask_for_phase(id: i32, phase: u32) -> Option<OpaqueMask> {
    let g = SPRITE_MASKS.lock().ok()?;
    let sheet = g.iter().find(|(sid, _)| *sid == id).map(|(_, s)| s)?;
    if sheet.frames.is_empty() {
        return None;
    }
    let idx = gfx::composite::frame_index(phase, sheet.frames.len(), sheet.looping)
        .unwrap_or(0)
        .min(sheet.frames.len() - 1);
    Some(sheet.frames[idx].clone())
}

fn remember_mask(id: i32, strip: &SpriteStrip) {
    let frames = strip.opaque_masks();
    if frames.is_empty() {
        return;
    }
    let n = frames.len();
    let bits = frames[0].width.saturating_mul(frames[0].height);
    if let Ok(mut g) = SPRITE_MASKS.lock() {
        g.retain(|(sid, _)| *sid != id);
        g.push((
            id,
            MaskSheet {
                looping: strip.looping,
                frames,
            },
        ));
    }
    log_line(&format!(
        "sprite {id} mask frames={n} bits/frame={bits} looping={}",
        strip.looping
    ));
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
            if spr_key(r, g, b, a) {
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
            terrain_frames: terrain_frames(strip),
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

/// Exact / free-list / nearest onto live colours. Rewrites `.spr` RGB only so
/// ProcessSprite's MapColor hits; never overwrites map palette slots.
unsafe fn wash_spr_palette(pal_ctx: *mut u8, spr: &mut [u8]) -> Result<(u32, u32, u32), String> {
    if spr.len() < 12 {
        return Err("truncated .spr".into());
    }
    let pal_n = u16::from_le_bytes([spr[10], spr[11]]) as usize;
    if pal_n == 0 {
        return Ok((0, 0, 0));
    }
    let mut kept = 0u32;
    let mut allocated = 0u32;
    let mut nearest = 0u32;
    let mut overflow_logged = false;
    for i in 0..pal_n {
        let o = 12 + i * 3;
        if o + 2 >= spr.len() {
            break;
        }
        let rgb = spr[o] as u32 | ((spr[o + 1] as u32) << 8) | ((spr[o + 2] as u32) << 16);
        unsafe {
            resolve_colour(
                pal_ctx,
                rgb,
                &mut spr[o..o + 3],
                &mut kept,
                &mut allocated,
                &mut nearest,
                &mut overflow_logged,
            )?;
        }
    }
    Ok((kept, allocated, nearest))
}

unsafe fn resolve_colour(
    ctx: *mut u8,
    rgb: u32,
    spr_rgb: &mut [u8],
    kept: &mut u32,
    allocated: &mut u32,
    nearest: &mut u32,
    overflow_logged: &mut bool,
) -> Result<u32, String> {
    unsafe {
        if let Some(slot) = find_exact_cached(ctx, rgb) {
            *kept += 1;
            return Ok(slot);
        }
        if let Some(slot) = find_exact_table(ctx, rgb) {
            cache_ensure(ctx, slot);
            *kept += 1;
            return Ok(slot);
        }
        let free = *(ctx.add(PAL_FREE_COUNT) as *const i16);
        if free > 0 {
            let next = free - 1;
            let slot = *ctx.add(PAL_FREE_LIST + next as usize) as u32;
            if slot != 0 && slot <= 255 && *ctx.add(PAL_IN_USE + slot as usize) == 0 {
                *(ctx.add(PAL_FREE_COUNT) as *mut i16) = next;
                write_free_slot(ctx, slot, rgb);
                mark_dirty(ctx);
                *allocated += 1;
                return Ok(slot);
            }
        }
        if !*overflow_logged {
            log_line("Palette Overflow Detected! (washing onto live colours)");
            *overflow_logged = true;
        }
        let slot = nearest_live_colour(ctx, rgb).ok_or("palette empty")?;
        let [r, g, b] = rgb_at(ctx, slot);
        spr_rgb[0] = r;
        spr_rgb[1] = g;
        spr_rgb[2] = b;
        cache_ensure(ctx, slot);
        *nearest += 1;
        Ok(slot)
    }
}

unsafe fn cache_ensure(ctx: *mut u8, slot: u32) {
    if slot == 0 || slot > 255 {
        return;
    }
    unsafe {
        let n = (*(ctx.add(PAL_CACHE_COUNT) as *const i16) as usize).min(256);
        for i in 0..n {
            if *ctx.add(PAL_CACHE + i) as u32 == slot {
                return;
            }
        }
        if n >= 256 {
            return;
        }
        *ctx.add(PAL_CACHE + n) = slot as u8;
        *(ctx.add(PAL_CACHE_COUNT) as *mut i16) = (n + 1) as i16;
    }
}

unsafe fn nearest_live_colour(ctx: *mut u8, rgb: u32) -> Option<u32> {
    unsafe {
        let tr = (rgb & 0xff) as i32;
        let tg = ((rgb >> 8) & 0xff) as i32;
        let tb = ((rgb >> 16) & 0xff) as i32;
        let in_use = ctx.add(PAL_IN_USE);
        let mut best_dist = i32::MAX;
        let mut best_slot = 0u32;
        for slot in 1..=255u32 {
            if *in_use.add(slot as usize) == 0 {
                continue;
            }
            let [r, g, b] = rgb_at(ctx, slot);
            let dist =
                (g as i32 - tg).abs() * 5 + (b as i32 - tb).abs() * 2 + (r as i32 - tr).abs() * 3;
            if dist == 0 {
                return Some(slot);
            }
            if dist < best_dist {
                best_dist = dist;
                best_slot = slot;
            }
        }
        if best_slot == 0 {
            None
        } else {
            Some(best_slot)
        }
    }
}

unsafe fn find_exact_cached(ctx: *mut u8, rgb: u32) -> Option<u32> {
    unsafe {
        let n = (*(ctx.add(PAL_CACHE_COUNT) as *const i16) as usize).min(256);
        let want = rgb & 0xffffff;
        for i in 0..n {
            let slot = *ctx.add(PAL_CACHE + i) as u32;
            if slot == 0 {
                continue;
            }
            let [r, g, b] = rgb_at(ctx, slot);
            let live = r as u32 | ((g as u32) << 8) | ((b as u32) << 16);
            if live == want {
                return Some(slot);
            }
        }
        None
    }
}

unsafe fn find_exact_table(ctx: *mut u8, rgb: u32) -> Option<u32> {
    unsafe {
        let want = rgb & 0xffffff;
        let in_use = ctx.add(PAL_IN_USE);
        for i in 1..=255u32 {
            if *in_use.add(i as usize) == 0 {
                continue;
            }
            let [r, g, b] = rgb_at(ctx, i);
            let live = r as u32 | ((g as u32) << 8) | ((b as u32) << 16);
            if live == want {
                return Some(i);
            }
        }
        None
    }
}

unsafe fn write_free_slot(ctx: *mut u8, slot: u32, rgb: u32) {
    if slot == 0 || slot > 255 {
        return;
    }
    unsafe {
        let p = ctx.add(PAL_RGB + slot as usize * 4) as *mut u32;
        let prev = *p;
        *p = (prev & 0xff000000) | (rgb & 0xffffff);
        *ctx.add(PAL_IN_USE + slot as usize) = 1;
        cache_ensure(ctx, slot);
    }
}

unsafe fn mark_dirty(ctx: *mut u8) {
    unsafe {
        *(ctx.add(PAL_DIRTY) as *mut u16) = 1;
    }
}

unsafe fn rgb_at(ctx: *mut u8, slot: u32) -> [u8; 3] {
    unsafe {
        let p = ctx.add(PAL_RGB + slot as usize * 4);
        [*p, *p.add(1), *p.add(2)]
    }
}

unsafe fn construct_sprite(sprite: *mut u8, cache: *mut u8) {
    let f = crate::wa::address("CONSTRUCT_SPRITE") as usize;
    unsafe {
        core::arch::asm!(
            "call {f}",
            f = in(reg) f,
            inout("eax") sprite => _,
            // ConstructSprite may clobber ECX; do not assume cache survives.
            inout("ecx") cache => _,
            out("edx") _,
            options(nostack),
        );
    }
}

unsafe fn process_sprite(sprite: *mut u8, pal_ctx: *mut u8, raw: *mut u8) -> u32 {
    let f = crate::wa::address("PROCESS_SPRITE") as usize;
    let ret: u32;
    unsafe {
        core::arch::asm!(
            "push {raw}",
            "call {f}",
            raw = in(reg) raw,
            f = in(reg) f,
            inout("eax") sprite => ret,
            // ProcessSprite destroys ECX; `in` would wrongly keep pal_ctx in ECX
            // for free_after → AV on display+0x311C.
            inout("ecx") pal_ctx => _,
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

/// The sheet a drawn sprite value names. WA packs flags next to the id --
/// the mirror bit, the reverse bit a weapon being put away gets, and others
/// -- which a lookup of our sheets must ignore. Otherwise the pack art is
/// not recognised and WA draws `id & 0xffff`, some unrelated stock sprite.
pub fn sheet_id(sprite: u32) -> u32 {
    sprite & (VIRTUAL_SPRITE_BASE as u32 | 0xffff)
}

/// The phase a sprite is drawn at. As `DisplayGfx__BlitSprite` (0x56B080)
/// does, a sprite with WA's reverse flag plays backwards: `0x10000 - phase`.
pub fn draw_phase(sprite: u32, phase: u32) -> u32 {
    if sprite & game::offsets::SPRITE_REVERSE != 0 {
        0x1_0000u32.wrapping_sub(phase)
    } else {
        phase
    }
}

pub fn is_virtual(id: i32) -> bool {
    id >= VIRTUAL_SPRITE_BASE
}

/// Stock sprite id -> the GPU sheet drawn in its place (`sprite_overrides`).
/// WA keeps its own sprite at that id, so every size, frame-count and phase
/// lookup still answers as before; only the draw is ours, in true colour.
static GPU_ALIASES: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());

/// Draw `sheet`, a GPU sheet, wherever WA draws stock sprite `target`.
/// The first claim wins; false when `target` is already drawn as another.
pub fn alias_gpu(target: i32, sheet: i32) -> bool {
    let Ok(mut g) = GPU_ALIASES.lock() else {
        return false;
    };
    if g.iter().any(|(t, _)| *t == target as u32) {
        return false;
    }
    g.push((target as u32, sheet as u32));
    log_line(&format!("stock sprite {target} drawn as GPU sheet {sheet:#x}"));
    true
}

/// The GPU sheet drawn in place of `id`, or `id` itself.
pub fn gpu_alias(id: u32) -> u32 {
    GPU_ALIASES
        .lock()
        .ok()
        .and_then(|g| g.iter().find(|(t, _)| *t == id).map(|(_, s)| *s))
        .unwrap_or(id)
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
pub fn gpu_frame(id: u32) -> Option<(impl Fn(usize) -> Option<render::SheetRect>, usize, bool)> {
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
        g.push(GpuSheet {
            id,
            frames: rects,
            looping: strip.looping,
        });
    }
    if let Some(feet) = strip.feet_platform() {
        if let Ok(mut g) = SPRITE_FEET.lock() {
            g.retain(|(s, _)| *s != id);
            g.push((id, feet));
        }
    }
    remember_mask(id, strip);
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
            // The atlas holds the pixels; this entry exists only so lookups
            // keyed on RGBA_SHEETS still find the sheet. Anything that needs
            // a frame COUNT must use frame_count(), which checks GPU_SHEETS
            // first for exactly this reason.
            terrain_frames: terrain_frames(strip),
            frames: Vec::new(),
        });
    }
    id
}

/// Drop every GPU sheet. Called with the rest of the per-match sprite state.
pub fn clear_gpu_sheets() {
    if let Ok(mut g) = GPU_SHEETS.lock() {
        g.clear();
    }
    if let Ok(mut g) = GPU_ALIASES.lock() {
        g.clear();
    }
    if let Ok(mut next) = NEXT_VIRTUAL.lock() {
        *next = VIRTUAL_SPRITE_BASE;
    }
}

/// How many of the 223 shared custom slots are currently occupied.
///
/// The ceiling is global: every loaded pack draws from the same range, so one
/// pack baking rotations costs every other pack. GPU sheets are deliberately
/// absent from this count -- that is the point of them.
pub fn slots_used() -> usize {
    LOADED.lock().map(|g| g.len()).unwrap_or(0)
}

pub fn set_force_layer(_layer: Option<u32>) {}

pub fn set_reserve_colours(_on: bool) {}

#[cfg(test)]
mod sheet_id_tests {
    use super::{draw_phase, sheet_id};
    use game::offsets::{SPRITE_MIRROR_X, SPRITE_REVERSE};

    #[test]
    fn flags_next_to_the_id_do_not_change_the_sheet() {
        let sheet = 0x0100_0063;
        assert_eq!(sheet_id(sheet), sheet);
        assert_eq!(sheet_id(sheet | SPRITE_MIRROR_X), sheet);
        // A weapon being put away, as seen in WA: this drew stock 99, a crate.
        assert_eq!(sheet_id(sheet | SPRITE_REVERSE | SPRITE_MIRROR_X), sheet);
        // The body flags WA's worm draw can add (0x50FCD0).
        assert_eq!(sheet_id(sheet | 0x0020_0000 | SPRITE_MIRROR_X), sheet);
        assert_eq!(sheet_id(sheet | 0x0040_0000 | SPRITE_MIRROR_X), sheet);
        assert_eq!(sheet_id(99 | 0x0040_0000), 99, "a stock sprite stays stock");
    }

    #[test]
    fn a_reversed_sprite_plays_backwards() {
        let sheet = 0x0100_0061;
        assert_eq!(draw_phase(sheet, 0x4000), 0x4000);
        assert_eq!(draw_phase(sheet | SPRITE_MIRROR_X, 0x4000), 0x4000);
        assert_eq!(draw_phase(sheet | SPRITE_REVERSE, 0x4000), 0xC000);
        assert_eq!(draw_phase(sheet | SPRITE_REVERSE, 0), 0x1_0000, "a one-shot sheet starts on its last frame");
        assert_eq!(draw_phase(sheet | SPRITE_REVERSE, 0x1_0000), 0);
    }
}

// Only bounded frames can be stamped. Keep their source pixels even for GPU
// sheets; a device reset or render callback must never change terrain writes.
fn terrain_frames(strip: &SpriteStrip) -> Vec<gfx::RgbaFrame> {
    if strip.frames.iter().all(|f| (1..=256).contains(&f.width) && (1..=256).contains(&f.height)) {
        strip.frames.clone()
    } else { Vec::new() }
}

pub fn terrain_frame(id: i32, frame: u32) -> Result<gfx::RgbaFrame, String> {
    let sheets = RGBA_SHEETS.lock().map_err(|_| "land.stamp: sprite store unavailable")?;
    let sheet = sheets.iter().find(|s| s.id == id).ok_or("land.stamp: unknown sheet")?;
    sheet.terrain_frames.get(frame as usize).cloned()
        .ok_or_else(|| "land.stamp: frame out of range or sheet exceeds 256x256".into())
}
