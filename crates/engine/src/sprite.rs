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

use crate::log_line;
use crate::rebase::rb;
use game::offsets::{
    CONSTRUCT_SPRITE, CUSTOM_SPRITE_SENTINEL, CUSTOM_SPRITE_SLOT_MAX, CUSTOM_SPRITE_SLOT_MIN,
    DISPLAY_GAME_LAYER, DISPLAY_LAYER_CONTEXTS, DISPLAY_LAYER_VISIBILITY, DISPLAY_SPRITE_BANKS,
    DISPLAY_SPRITE_CACHE, DISPLAY_SPRITE_LAYERS, DISPLAY_SPRITE_PTRS, MISSILE_ANIM_PHASE,
    MISSILE_RENDER_SPRITE, PROCESS_SPRITE,
};
use gfx::{encode_spr, spr_key, FeetPlatform, SpriteStrip};
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
    let cache_n = unsafe { *(pal_ctx.add(PAL_CACHE_COUNT) as *const i16) };

    let mut sprite = Box::new(SpriteObj([0u8; 0x70]));
    let mut spr = spr.into_boxed_slice();
    unsafe {
        let snapped = quantize_to_map_palette(pal_ctx, &mut spr)?;
        log_line(&format!(
            "LoadSprite equivalent: slot {id} layer {layer} free={free_before} cache={cache_n} quantized={snapped}"
        ));
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
        let free_after = *(pal_ctx.add(PAL_FREE_COUNT) as *const i16);
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
    remember_rgba(id, strip);
    Ok(id)
}

pub fn frame_count(id: i32) -> Option<usize> {
    let g = RGBA_SHEETS.lock().ok()?;
    g.iter().find(|s| s.id == id).map(|s| s.frames.len())
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

unsafe fn nearest_in_use(ctx: *mut u8, rgb: u32) -> Option<u32> {
    let tr = (rgb & 0xff) as i32;
    let tg = ((rgb >> 8) & 0xff) as i32;
    let tb = ((rgb >> 16) & 0xff) as i32;
    let mut best = i32::MAX;
    let mut slot = 0u32;
    unsafe {
        let in_use = ctx.add(PAL_IN_USE);
        for i in 1..=255u32 {
            if *in_use.add(i as usize) == 0 {
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
        Some(slot)
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
