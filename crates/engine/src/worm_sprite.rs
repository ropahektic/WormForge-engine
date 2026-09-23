//! Slot-scoped worm art. Stock calculates animation, slope, position and flip;
//! this wrap changes only the resulting sprite id before the stock blit.
//! Empty sprite metadata is initialized separately: the stock animator saves
//! these outputs even when GetSpriteInfo(0) fails, so stack garbage can enter
//! its serialized simulation state.

use crate::{log_line, rebase::rb, sprite};
use game::{
    entity::read_i32,
    offsets::{WORM_SELECTED_WEAPON, WORM_SLOPE_DIR, WORM_STATE},
};
use minhook::MinHook;
use re_catalog::Catalog;
use std::{
    ffi::c_void,
    sync::{
        Mutex,
        atomic::{AtomicPtr, Ordering},
    },
};

static ORIGINAL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_SPRITE_INFO: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

type SpriteInfoFn = unsafe extern "thiscall" fn(*mut u8, u32, *mut u32, *mut u32, *mut u32) -> u32;

static CLAIMS: Mutex<Vec<(usize, i32, i32)>> = Mutex::new(Vec::new());
/// Per worm: which claimed pose is playing, and how far through it.
///
/// WA animates the poses it owns by changing state and swapping sprite id, not
/// by advancing a phase within one sheet: for a claimed pose it hands us only
/// 0x0 or 0x8000, so an overridden sheet sits on its first frame forever. A
/// custom sheet is one strip per pose, so the phase has to come from here.
static ANIM: Mutex<Vec<(usize, i32, u32, i32)>> = Mutex::new(Vec::new());

fn pose_key(original: u32, state: i32, slope: i32) -> i32 {
    let stock = (original & 0xffff) as i32;
    if stock == 0 && matches!(state, 0x67..=0x69) {
        match slope.cmp(&0) {
            std::cmp::Ordering::Less => -2,
            std::cmp::Ordering::Greater => -3,
            std::cmp::Ordering::Equal => -1,
        }
    } else {
        stock
    }
}

pub fn clear() {
    if let Ok(mut anim) = ANIM.lock() {
        anim.clear();
    }
    if let Ok(mut claims) = CLAIMS.lock() {
        claims.clear();
    }
}

pub fn claim(slot: usize, mod_id: &str, poses: &[(i32, String)]) {
    if let Ok(mut claims) = CLAIMS.lock() {
        claims.retain(|(s, _, _)| *s != slot);
        for (stock, path) in poses {
            if let Some(sheet) = sprite::lookup_key(&gfx::sprite_key(mod_id, path)) {
                claims.push((slot, *stock, sheet.id));
                log_line(&format!(
                    "worm sprite claim slot={slot} stock={stock} custom={} path={path}",
                    sheet.id
                ));
            }
        }
    }
}

pub fn install(catalog: &Catalog) -> Result<(), String> {
    let va = catalog
        .function_va("WormEntity__calc_sprite")
        .ok_or("missing WormEntity__calc_sprite catalog entry")?;
    let trampoline = unsafe {
        MinHook::create_hook(rb(va) as *mut c_void, calc_sprite_hook as *mut c_void)
            .map_err(|e| format!("worm sprite hook: {e:?}"))?
    };
    ORIGINAL.store(trampoline, Ordering::SeqCst);
    let info_va = catalog
        .function_va("DisplayGfx__GetSpriteInfo")
        .ok_or("missing DisplayGfx__GetSpriteInfo catalog entry")?;
    let info = rb(info_va) as *mut c_void;
    // 3.8.1: MOV EAX,[ESP+4]; LEA EDX,[EAX-1]. The unsigned range
    // check immediately following rejects ID 0 without touching outputs.
    let expected = [0x8b, 0x44, 0x24, 0x04, 0x8d, 0x50, 0xff];
    unsafe {
        if std::slice::from_raw_parts(info as *const u8, expected.len()) != expected {
            return Err(format!("sprite info: WA bytes differ at {info_va:#010x}"));
        }
        let trampoline = MinHook::create_hook(info, sprite_info_hook as *mut c_void)
            .map_err(|e| format!("sprite info hook: {e:?}"))?;
        ORIGINAL_SPRITE_INFO.store(trampoline, Ordering::SeqCst);
    }
    log_line("empty sprite metadata initialized for deterministic animation state");
    Ok(())
}

/// SpriteAnimation__play_1 (0x4F5BD0) passes its two uninitialized stack locals
/// to GetSpriteInfo, ignores failure, then copies them into animator+0x14/0x18
/// and its queue at +0x30/+0x34. Sentry's empty stock equip pose uses ID 0.
/// Supply zero rate/flags and one frame. A nonzero divisor also covers an
/// empty sprite with high flag bits: advance checks the whole sprite word
/// before dividing by frame count. Keep stock failure and real-sprite lookup.
unsafe extern "thiscall" fn sprite_info_hook(
    display: *mut u8,
    sprite_id: u32,
    rate_out: *mut u32,
    flags_out: *mut u32,
    frames_out: *mut u32,
) -> u32 {
    if sprite_id == 0 {
        for out in [rate_out, flags_out] {
            if !out.is_null() {
                *out = 0;
            }
        }
        if !frames_out.is_null() {
            *frames_out = 1;
        }
    }
    let original: SpriteInfoFn = std::mem::transmute(ORIGINAL_SPRITE_INFO.load(Ordering::Relaxed));
    original(display, sprite_id, rate_out, flags_out, frames_out)
}

unsafe extern "C" fn after_calc(
    worm: *mut u8,
    sprite_out: *mut u32,
    _x_out: *const i32,
    _y_out: *const i32,
    phase_out: *mut u32,
) {
    if worm.is_null() || sprite_out.is_null() {
        return;
    }
    let slot = read_i32(worm, WORM_SELECTED_WEAPON) as usize;
    let original = *sprite_out;
    let state = read_i32(worm, WORM_STATE);
    let slope = read_i32(worm, WORM_SLOPE_DIR);
    if let Ok(claims) = CLAIMS.lock() {
        if !claims.iter().any(|(s, _, _)| *s == slot) {
            return;
        }
        let stock = pose_key(original, state, slope);
        if let Some((_, _, id)) = claims
            .iter()
            .find(|(s, claimed, _)| *s == slot && *claimed == stock)
        {
            // Retain mirror and other stock sprite flags.
            *sprite_out = (original & !0xffff) | *id as u32;
            drive_phase(worm as usize, stock, *id, phase_out);
        } else if let Ok(mut anim) = ANIM.lock() {
            // This worm is on a claimed slot but not currently in one of its
            // claimed poses (e.g. walking between equips). drive_phase only
            // resets a worm's stored phase when the pose or sheet id differs
            // from last time, so if the next equip lands on the SAME pose
            // key as whatever this worm last held (same slope sign is common
            // -- the worm rarely moves between shots), the stale entry is
            // still sitting at its old phase, which is the final held frame.
            // The next drive_phase call then sees no change and never
            // resets, so the equip animation appears to jump straight to
            // its last frame instead of playing. Dropping the entry here,
            // the moment the worm leaves a claimed pose, guarantees the next
            // equip always starts from a fresh (missing) entry.
            anim.retain(|(w, _, _, _)| *w != worm as usize);
        }
    }
}

/// Advance a claimed pose through its own sheet, once, and hold the last frame.
///
/// GetSpriteFrameForBlit reads anim_value as `(phase * frames) >> 16`. Each
/// frame is held for HOLD draws so a short strip does not flash past, and the
/// phase stops inside the final frame instead of wrapping: equipping ends with
/// the weapon held, it does not loop.
/// Game frames each animation frame is held for.
const HOLD: u32 = 2;

/// Advance one worm's claimed pose, once per game frame.
///
/// Called from the worm's FrameFinish. The sprite calc runs several times per
/// frame, so advancing in the draw hook made the sequence play several times
/// too fast and snap to its last frame almost at once.
pub fn tick(worm: usize) {
    let Ok(mut anim) = ANIM.lock() else {
        return;
    };
    let Some(entry) = anim.iter_mut().find(|(w, _, _, _)| *w == worm) else {
        return;
    };
    let Some(frames) = sprite::frame_count(entry.3) else {
        return;
    };
    if frames < 2 {
        return;
    }
    let step = 0x1_0000u32 / (frames as u32 * HOLD);
    // Stop inside the final frame rather than wrapping: equipping ends with
    // the weapon held, it does not loop.
    let last = 0x1_0000u32 - (0x1_0000u32 / frames as u32) / 2;
    entry.2 = (entry.2 + step).min(last);
}

/// Write the pose's current phase, restarting whenever the pose changes.
unsafe fn drive_phase(worm: usize, pose: i32, sprite: i32, phase_out: *mut u32) {
    if phase_out.is_null() {
        return;
    }
    let Ok(mut anim) = ANIM.lock() else {
        return;
    };
    let phase = match anim.iter_mut().find(|(w, _, _, _)| *w == worm) {
        Some(entry) => {
            if entry.1 != pose || entry.3 != sprite {
                entry.1 = pose;
                entry.3 = sprite;
                entry.2 = 0;
            }
            entry.2
        }
        None => {
            anim.push((worm, pose, 0, sprite));
            0
        }
    };
    unsafe {
        *phase_out = phase;
    }
}

#[cfg(test)]
mod tests {
    use super::pose_key;

    #[test]
    fn custom_link_uses_state_and_slope_but_throw_keeps_stock_id() {
        assert_eq!(pose_key(0x40000, 0x67, 0), -1);
        assert_eq!(pose_key(0x40000, 0x68, -1), -2);
        assert_eq!(pose_key(0x40000, 0x69, 1), -3);
        assert_eq!(pose_key(0x4024f, 0x6a, 0), 591);
        assert_eq!(pose_key(0x40000, 0x6a, 0), 0);
    }
}

/// WA 3.8.1 usercall: ESI=worm, EAX=sprite_out, stack=x_out,y_out,phase_out.
/// Save the output pointer across the original, then remap only its result.
#[unsafe(naked)]
unsafe extern "C" fn calc_sprite_hook() {
    core::arch::naked_asm!(
        "push ebp",
        "mov ebp, esp",
        "push eax",
        "push esi",
        "push dword ptr [ebp + 16]",
        "push dword ptr [ebp + 12]",
        "push dword ptr [ebp + 8]",
        "call dword ptr [{original}]",
        "pushad",
        "push dword ptr [ebp + 16]",
        "push dword ptr [ebp + 12]",
        "push dword ptr [ebp + 8]",
        "push dword ptr [ebp - 4]",
        "push dword ptr [ebp - 8]",
        "call {after}",
        "add esp, 20",
        "popad",
        "mov esp, ebp",
        "pop ebp",
        "ret 12",
        original = sym ORIGINAL,
        after = sym after_calc,
    );
}
