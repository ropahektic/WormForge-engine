//! `wa.fire.spawn`: one of WA's fires (FireEntity), started as the Petrol
//! Bomb's MissileEntity::CreateFire1 (0x509D70) starts its own: WA's
//! allocator, the first FIRE_ENTITY_ZEROED bytes zeroed, then the constructor
//! with the fire store for parent, a 12-dword setup (PX's CFireDesc) and no
//! source fire. The constructor draws from WA's RNG, so it runs only from
//! simulation callbacks: every peer and replay starts the same fires.
//!
//! A fire hits a worm with message 0x4B, kind 1, the fire its sender (Project
//! X's M_GUNEXP). A fire spawned with `damage_scale` / `knock_scale` has those
//! hits scaled before WA (and `wa.on.hurt`) see them, as Project X's Valyrian
//! Fire does. Such fires are known by address and pool slot together, so a
//! later fire at a freed one's address is not taken for it, and forgotten as
//! WA frees them (FireEntity::Free).
//!
//! A fire spawned with `tint` is drawn as Project X's SetColorMod(tint, 1)
//! draws its Valyrian Fire: additively, its colours multiplied by the tint
//! (PX's BlendType 1: render additive, diffuse by modulation), and twice on
//! every tenth game frame. A fire queues its flame (WA's petrol sprites) at
//! render-queue depth FIRE_DRAW_DEPTH + its pool slot; for a tinted fire's
//! slot the GPU renderer draws a full-colour copy of that sprite instead, at
//! that depth. The copies are made at match start from WA's own loaded
//! sprites, through WA's own frame getter, coloured by the sprite's layer
//! palette.

use game::entity::{read_i32, write_i32};
use game::offsets::{
    BITGRID_BITS, BITGRID_STRIDE, CLASS_WORM, DISPLAY_LAYER_CONTEXTS, DISPLAY_SLOT_GET_SPRITE_FRAME,
    DISPLAY_SPRITE_LAYERS, DISPLAY_SPRITE_PTRS, ENT_CLASS_TYPE, ENT_WORM_TEAM, FIRE_DRAW_DEPTH,
    FIRE_ENTITY_POOL_SLOT, FIRE_ENTITY_SIZE, FIRE_ENTITY_ZEROED, FIRE_SPRITE_FIRST,
    FIRE_SPRITE_LAST, HIT_AMOUNT, HIT_FLAG, HIT_KIND_FIRE, HIT_KNOCK_X, HIT_KNOCK_Y, MSG_HIT,
    SHARED_FIRE_STORE, SPRITE_MAX_FRAMES,
};
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use std::sync::Mutex;

/// Tinted fires: (address, pool slot, ARGB tint).
static TINTED: Mutex<Vec<(u32, i32, u32)>> = Mutex::new(Vec::new());
/// WA's fire sprites' full-colour copies: (stock sprite, GPU sheet).
static COPIES: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
static ORIG_FREE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// A fire whose hits are scaled: (address, pool slot, damage, knock), the
/// scales 16.16.
static SCALED: Mutex<Vec<(u32, i32, i32, i32)>> = Mutex::new(Vec::new());

const ONE: i32 = 0x1_0000;

type FireCtorFn =
    unsafe extern "stdcall" fn(this: *mut u8, parent: *mut u8, init: *const i32, source: *mut u8) -> *mut u8;

/// Fires started this match, for their ids.
static STARTED: AtomicU32 = AtomicU32::new(0);

pub fn clear() {
    STARTED.store(0, Ordering::SeqCst);
    if let Ok(mut g) = SCALED.lock() {
        g.clear();
    }
    if let Ok(mut g) = TINTED.lock() {
        g.clear();
    }
    if let Ok(mut g) = COPIES.lock() {
        g.clear();
    }
}

/// WA frees `fire`: it is no longer one of ours, whatever comes to its
/// address or its pool slot next.
fn forget(fire: u32) {
    if let Ok(mut g) = SCALED.lock() {
        g.retain(|(f, _, _, _)| *f != fire);
    }
    if let Ok(mut g) = TINTED.lock() {
        g.retain(|(f, _, _)| *f != fire);
    }
}

type FreeFn = unsafe extern "thiscall" fn(*mut u8, u32) -> *mut u8;

unsafe extern "thiscall" fn free_hook(this: *mut u8, flags: u32) -> *mut u8 {
    forget(this as u32);
    let orig: FreeFn = unsafe { std::mem::transmute(ORIG_FREE.load(Ordering::SeqCst)) };
    unsafe { orig(this, flags) }
}

pub fn install() -> Result<(), String> {
    let Some(target) = crate::wa::try_address("FIRE_ENTITY_FREE") else {
        return Err("FIRE_ENTITY_FREE not located".into());
    };
    let trampoline = unsafe { crate::wa::create_hook(target as *mut c_void, free_hook as *mut c_void) }?;
    ORIG_FREE.store(trampoline, Ordering::SeqCst);
    Ok(())
}

/// The atlas key of stock sprite `id`'s copy.
fn copy_key(id: u32) -> String {
    format!("wa-fire:{id}")
}

type FrameFn = unsafe extern "thiscall" fn(
    display: *mut u8, sprite: u32, anim: u32, w: *mut i32, h: *mut i32,
    left: *mut i32, top: *mut i32, right: *mut i32, bottom: *mut i32, frac: *mut u32,
) -> *mut u8;

/// The animation value WA turns into `frame` of `frames`, rounding or not
/// (`(frames * anim) >> 16`, +0x8000 when the sprite rounds).
fn anim_for(frame: u32, frames: u32) -> u32 {
    ((frame << 16) + frames - 1) / frames
}

/// Full-colour copies of WA's fire sprites, for the GPU atlas: each frame
/// through WA's own GetSpriteFrameForBlit, its palette indices through the
/// sprite's layer palette, index 0 clear. A sprite WA keeps elsewhere (a
/// sprite bank) or cannot give a frame of is left out.
pub unsafe fn copies(display: *mut u8) -> Vec<(String, gfx::SpriteStrip)> {
    let mut out = Vec::new();
    if display.is_null() {
        return out;
    }
    unsafe {
        let vtable = *(display as *const *const usize);
        let get: FrameFn = std::mem::transmute(*vtable.add(DISPLAY_SLOT_GET_SPRITE_FRAME));
        for id in FIRE_SPRITE_FIRST..=FIRE_SPRITE_LAST {
            let sprite = *(display.add(DISPLAY_SPRITE_PTRS) as *const *const u8).add(id as usize);
            if sprite.is_null() {
                continue;
            }
            let frames = (sprite.add(SPRITE_MAX_FRAMES) as *const u16).read_unaligned() as u32;
            let layer = *(display.add(DISPLAY_SPRITE_LAYERS) as *const u32).add(id as usize);
            let ctx = read_i32(display, DISPLAY_LAYER_CONTEXTS + layer as usize * 4) as *const u8;
            if frames == 0 || frames > 256 || ctx.is_null() {
                continue;
            }
            let mut strip = gfx::SpriteStrip { frames: Vec::new(), fps: 10, looping: false };
            for f in 0..frames {
                let (mut w, mut h, mut l, mut t, mut r, mut b, mut frac) = (0, 0, 0, 0, 0, 0, 0u32);
                let grid = get(display, id, anim_for(f, frames), &mut w, &mut h, &mut l, &mut t, &mut r, &mut b, &mut frac);
                if grid.is_null() || w <= 0 || h <= 0 || w > 512 || h > 512 {
                    break;
                }
                let data = read_i32(grid, BITGRID_BITS) as *const u8;
                let stride = read_i32(grid, BITGRID_STRIDE) as usize;
                let (w, h) = (w as usize, h as usize);
                let mut rgba = vec![0u8; w * h * 4];
                if !data.is_null() {
                    for y in 0..(b - t).max(0) as usize {
                        for x in 0..(r - l).max(0) as usize {
                            let (cx, cy) = (l as usize + x, t as usize + y);
                            if cx >= w || cy >= h {
                                continue;
                            }
                            let index = *data.add(y * stride + x) as usize;
                            if index == 0 {
                                continue;
                            }
                            let o = (cy * w + cx) * 4;
                            rgba[o] = *ctx.add(4 + index * 4);
                            rgba[o + 1] = *ctx.add(5 + index * 4);
                            rgba[o + 2] = *ctx.add(6 + index * 4);
                            rgba[o + 3] = 255;
                        }
                    }
                }
                strip.frames.push(gfx::RgbaFrame { width: w as u32, height: h as u32, rgba });
            }
            if strip.frames.len() as u32 == frames {
                out.push((copy_key(id), strip));
            }
        }
    }
    out
}

/// After the atlas is built: which GPU sheet each copied sprite became.
pub fn bind_copies() {
    let Ok(mut g) = COPIES.lock() else {
        return;
    };
    g.clear();
    let sheet = |key: String| {
        crate::sprite::lookup_key(&key).filter(|s| crate::sprite::is_virtual(s.id)).map(|s| s.id as u32)
    };
    for id in FIRE_SPRITE_FIRST..=FIRE_SPRITE_LAST {
        if let Some(copy) = sheet(copy_key(id)) {
            g.push((id, copy));
        }
    }
}

/// A draw WA queues at `depth` with `sprite`: a tinted fire's flame gives
/// the copy to draw instead and the tint.
pub fn tinted_draw(depth: u32, sprite: u32) -> Option<(u32, u32)> {
    if depth & 0xFFFF_0000 != FIRE_DRAW_DEPTH {
        return None;
    }
    let slot = (depth & 0xFFFF) as i32;
    let tint = TINTED.lock().ok()?.iter().find(|(_, s, _)| *s == slot).map(|(_, _, t)| *t)?;
    let id = crate::sprite::sheet_id(sprite);
    let copy = COPIES.lock().ok()?.iter().find(|(s, _)| *s == id).map(|&(_, c)| c)?;
    Some((copy, tint))
}

/// `v` times a 16.16 scale, in integers, the same on every machine.
fn scaled(v: i32, scale: i32) -> i32 {
    ((v as i64 * scale as i64) >> 16) as i32
}

/// Before a worm handles `msg` from `sender`: a scaled fire's hit gets its
/// damage and knockback scaled.
pub fn before_worm_message(sender: u32, msg: u32, data: *mut u8) {
    if msg != MSG_HIT || sender == 0 || data.is_null() {
        return;
    }
    let Some((damage, knock)) = SCALED.lock().ok().and_then(|g| {
        g.iter()
            .find(|(fire, _, _, _)| *fire == sender)
            .filter(|(_, slot, _, _)| unsafe { read_i32(sender as *mut u8, FIRE_ENTITY_POOL_SLOT) } == *slot)
            .map(|&(_, _, d, k)| (d, k))
    }) else {
        return;
    };
    unsafe {
        if read_i32(data, HIT_FLAG) != HIT_KIND_FIRE {
            return;
        }
        write_i32(data, HIT_AMOUNT, scaled(read_i32(data, HIT_AMOUNT), damage));
        write_i32(data, HIT_KNOCK_X, scaled(read_i32(data, HIT_KNOCK_X), knock));
        write_i32(data, HIT_KNOCK_Y, scaled(read_i32(data, HIT_KNOCK_Y), knock));
    }
}

/// The 12 dwords FireEntity's constructor copies (PX's CFireDesc): position,
/// speed, 0x10 (0, as WA and PX pass it), count, 0x18 (1), duration,
/// upward power, damage, 0x28 (1), owner team.
fn setup(args: &lua_host::FireArgs, team: i32) -> [i32; 12] {
    [
        args.x, args.y, args.vx, args.vy, 0, args.count, 1, args.duration, args.up_power,
        args.damage, 1, team,
    ]
}

pub fn spawn(args: &lua_host::FireArgs) -> Result<u32, String> {
    let Some(ctor) = crate::wa::try_address("FIRE_ENTITY_CTOR") else {
        return Err("wa.fire.spawn: WA's fire constructor was not located".into());
    };
    let owner = args.owner as *mut u8;
    unsafe {
        let parent = crate::actor::shared_object(owner, SHARED_FIRE_STORE)
            .ok_or("wa.fire.spawn: no fire store")?;
        // A worm's team owns the fire, as the shot's team owns a Petrol
        // Bomb's; anything else's is no team's.
        let team = if read_i32(owner, ENT_CLASS_TYPE) == CLASS_WORM {
            read_i32(owner, ENT_WORM_TEAM)
        } else {
            0
        };
        let init = setup(args, team);
        let malloc: unsafe extern "C" fn(usize) -> *mut u8 =
            std::mem::transmute(crate::wa::address("WA_MALLOC") as usize);
        let fire = malloc(FIRE_ENTITY_SIZE);
        if fire.is_null() {
            return Err("wa.fire.spawn: malloc failed".into());
        }
        std::ptr::write_bytes(fire, 0, FIRE_ENTITY_ZEROED);
        let ctor: FireCtorFn = std::mem::transmute(ctor as usize);
        let p = ctor(fire, parent, init.as_ptr(), std::ptr::null_mut());
        if p.is_null() {
            return Err("wa.fire.spawn: the fire constructor failed".into());
        }
        let slot = read_i32(p, FIRE_ENTITY_POOL_SLOT);
        if args.damage_scale != ONE || args.knock_scale != ONE {
            if let Ok(mut g) = SCALED.lock() {
                g.retain(|(fire, _, _, _)| *fire != p as u32);
                g.push((p as u32, slot, args.damage_scale, args.knock_scale));
            }
        }
        if let Some(rgb) = args.tint {
            if let Ok(mut g) = TINTED.lock() {
                // A slot is one fire's at a time: an older fire there is gone.
                g.retain(|(fire, s, _)| *fire != p as u32 && *s != slot);
                g.push((p as u32, slot, 0xFF00_0000 | (rgb & 0x00FF_FFFF)));
            }
        }
    }
    Ok(STARTED.fetch_add(1, Ordering::SeqCst) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fire tests share the registries: one at a time.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn the_setup_is_pxs_fire_desc_in_order() {
        let args = lua_host::FireArgs {
            x: 10, y: 20, vx: 3, vy: 4, count: 8, duration: 4000, up_power: 100, damage: 20,
            owner: 1, ..lua_host::FIRE_DEFAULTS
        };
        assert_eq!(setup(&args, 2), [10, 20, 3, 4, 0, 8, 1, 4000, 100, 20, 1, 2]);
    }

    #[test]
    fn the_anim_value_for_a_frame_gives_that_frame_rounded_or_not() {
        for frames in [1u32, 4, 6, 7, 32, 255] {
            for f in 0..frames {
                let a = anim_for(f, frames);
                assert_eq!((frames * a) >> 16, f, "{f} of {frames}");
                assert_eq!((frames * a + 0x8000) >> 16, f, "{f} of {frames}, rounded");
            }
        }
    }

    #[test]
    fn a_tinted_fires_flame_draws_its_copy_and_freeing_forgets_it() {
        let _serial = serial();
        clear();
        TINTED.lock().unwrap().push((0x5000, 9, 0xFF4B_E14B));
        COPIES.lock().unwrap().push((121, 0x0100_0042));
        let depth = FIRE_DRAW_DEPTH + 9;
        assert_eq!(tinted_draw(depth, 121), Some((0x0100_0042, 0xFF4B_E14B)));
        assert_eq!(tinted_draw(depth, 121 | 0x4_0000), Some((0x0100_0042, 0xFF4B_E14B)), "mirrored");
        assert_eq!(tinted_draw(depth, 48), None, "not a fire sprite");
        assert_eq!(tinted_draw(FIRE_DRAW_DEPTH + 8, 121), None, "another fire");
        assert_eq!(tinted_draw(0xD_0009, 121), None, "not a fire's depth");
        forget(0x5000);
        assert_eq!(tinted_draw(depth, 121), None, "freed");
        clear();
    }

    #[test]
    fn a_scaled_fires_hit_is_scaled_and_others_are_not() {
        let _serial = serial();
        clear();
        // A fake fire: its pool slot is all the hit check reads of it.
        let mut fire = vec![0u8; FIRE_ENTITY_SIZE];
        unsafe { write_i32(fire.as_mut_ptr(), FIRE_ENTITY_POOL_SLOT, 7) };
        let addr = fire.as_mut_ptr() as u32;
        SCALED.lock().unwrap().push((addr, 7, 0x1_4000, 0x2_0000)); // 1.25, 2
        let hit = |flag: i32| {
            let mut d = [0i32; 8];
            d[0] = flag;
            d[3] = -0x3_0000; // knock x, 16.16
            d[4] = 0x1_8000;
            d[5] = 20; // damage
            d
        };
        let mut d = hit(HIT_KIND_FIRE);
        before_worm_message(addr, MSG_HIT, d.as_mut_ptr() as *mut u8);
        assert_eq!((d[5], d[3], d[4]), (25, -0x6_0000, 0x3_0000));
        let mut other = hit(10); // the axe's percent hit, not a fire's
        before_worm_message(addr, MSG_HIT, other.as_mut_ptr() as *mut u8);
        assert_eq!(other[5], 20);
        unsafe { write_i32(fire.as_mut_ptr(), FIRE_ENTITY_POOL_SLOT, 8) };
        let mut reused = hit(HIT_KIND_FIRE);
        before_worm_message(addr, MSG_HIT, reused.as_mut_ptr() as *mut u8);
        assert_eq!(reused[5], 20, "another fire at a freed one's address");
        clear();
    }
}
