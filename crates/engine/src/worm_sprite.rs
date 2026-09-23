//! Slot-scoped worm art. Stock calculates animation, slope, position and flip;
//! this wrap changes only the resulting sprite id before the stock blit.
//!
//! `weaponlnk` / `u` / `d` are custom aim-hold sheets. Stock may output:
//! - sprite id 0 while states 0x67..=0x69 (generic custom-weapon path), or
//! - a donor fire-path sheet (`wbaz*`, `wbaz2*`, …, `wthrgrn*`, …) when
//!   `copy_from` is bazooka / homing / fuse / cluster.
//! We map all of those to the claimed link sheets and keep stock's aim phase.
//!
//! `wthrow`* and the donor draw/appear sheets (`wbazlnk*`, `wgrnlnk*`, …) are
//! draw/undraw. Unset throw claims copy the aim sheet and freeze; real draw
//! GIFs play via `drive_phase`.
//!
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
/// Used for draw/undraw-style sheets where stock only hands 0x0 / 0x8000.
/// Aiming (`weaponlnk`*) keeps stock's aim-angle phase instead.
static ANIM: Mutex<Vec<(usize, i32, u32, i32)>> = Mutex::new(Vec::new());

const LINK: i32 = -1;
const LINK_U: i32 = -2;
const LINK_D: i32 = -3;
const THROW: i32 = 591;
const THROW_U: i32 = 592;
const THROW_D: i32 = 593;

/// Stock aim-hold triads (flat, up, down) remapped to `weaponlnk*`.
/// Covers bazooka / HM / pigeon / MB barrels plus fuse/cluster throw-holds.
const AIM_HOLD: &[(i32, i32, i32)] = &[
    (525, 526, 527), // wbaz
    (531, 532, 533), // wbaz2
    (534, 535, 536), // wbaz3
    (685, 686, 687), // wbaz4
    (688, 689, 690), // wbaz5
    (528, 529, 530), // wflm
    (576, 577, 578), // wthrgrn
    (579, 580, 581), // wthrcls
    (582, 583, 584), // wthrban
    (585, 586, 587), // wthrpbm
    (588, 589, 590), // wthrhgr
];

/// Stock draw/appear triads remapped to `wthrow*`.
const AIM_DRAW: &[(i32, i32, i32)] = &[
    (591, 592, 593), // wthrow
    (436, 437, 438), // wbazlnk
    (442, 443, 444), // wbz2lnk
    (445, 446, 447), // wbz3lnk
    (691, 692, 693), // wbz4lnk
    (694, 695, 696), // wbz5lnk
    (439, 440, 441), // wflmlnk
    (313, 314, 315), // wgrnlnk
    (316, 317, 318), // wclslnk
    (319, 320, 321), // wbanlnk
    (322, 323, 324), // wpbmlnk
    (325, 326, 327), // whgrlnk
];

fn is_link(pose: i32) -> bool {
    matches!(pose, LINK | LINK_U | LINK_D)
}

fn is_throw(pose: i32) -> bool {
    matches!(pose, THROW | THROW_U | THROW_D)
}

fn slope_link(slope: i32) -> i32 {
    match slope.cmp(&0) {
        std::cmp::Ordering::Less => LINK_U,
        std::cmp::Ordering::Greater => LINK_D,
        std::cmp::Ordering::Equal => LINK,
    }
}

fn triad_to(
    stock: i32,
    flat: i32,
    up: i32,
    down: i32,
    to_flat: i32,
    to_up: i32,
    to_down: i32,
) -> Option<i32> {
    if stock == flat {
        Some(to_flat)
    } else if stock == up {
        Some(to_up)
    } else if stock == down {
        Some(to_down)
    } else {
        None
    }
}

fn pose_key(original: u32, state: i32, slope: i32) -> i32 {
    let stock = (original & 0xffff) as i32;
    if stock == 0 && matches!(state, 0x67..=0x69) {
        return slope_link(slope);
    }
    for &(flat, up, down) in AIM_HOLD {
        if let Some(pose) = triad_to(stock, flat, up, down, LINK, LINK_U, LINK_D) {
            return pose;
        }
    }
    for &(flat, up, down) in AIM_DRAW {
        if let Some(pose) = triad_to(stock, flat, up, down, THROW, THROW_U, THROW_D) {
            return pose;
        }
    }
    stock
}

pub fn clear() {
    if let Ok(mut anim) = ANIM.lock() {
        anim.clear();
    }
    if let Ok(mut claims) = CLAIMS.lock() {
        claims.clear();
    }
}

/// Fill missing slope variants from the first present link sheet, then eat
/// draw/undraw (`wthrow`*) with the matching link sheet when unset.
fn expand_aim_claims(poses: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let link = poses.iter().find(|(p, _)| *p == LINK).map(|(_, id)| *id);
    let link_u = poses.iter().find(|(p, _)| *p == LINK_U).map(|(_, id)| *id);
    let link_d = poses.iter().find(|(p, _)| *p == LINK_D).map(|(_, id)| *id);
    let any = link.or(link_u).or(link_d);
    let Some(fallback) = any else {
        return poses.to_vec();
    };
    let p = link.unwrap_or(fallback);
    let u = link_u.unwrap_or(fallback);
    let d = link_d.unwrap_or(fallback);
    let mut out: Vec<(i32, i32)> = poses
        .iter()
        .copied()
        .filter(|(pose, _)| !is_link(*pose) && !is_throw(*pose))
        .collect();
    out.push((LINK, p));
    out.push((LINK_U, u));
    out.push((LINK_D, d));
    let throw = poses.iter().find(|(pose, _)| *pose == THROW).map(|(_, id)| *id);
    let throw_u = poses.iter().find(|(pose, _)| *pose == THROW_U).map(|(_, id)| *id);
    let throw_d = poses.iter().find(|(pose, _)| *pose == THROW_D).map(|(_, id)| *id);
    out.push((THROW, throw.unwrap_or(p)));
    out.push((THROW_U, throw_u.unwrap_or(u)));
    out.push((THROW_D, throw_d.unwrap_or(d)));
    out
}

pub fn claim(slot: usize, mod_id: &str, poses: &[(i32, String)]) {
    if let Ok(mut claims) = CLAIMS.lock() {
        claims.retain(|(s, _, _)| *s != slot);
        let mut resolved = Vec::new();
        for (stock, path) in poses {
            if let Some(sheet) = sprite::lookup_key(&gfx::sprite_key(mod_id, path)) {
                resolved.push((*stock, sheet.id));
                log_line(&format!(
                    "worm sprite claim slot={slot} stock={stock} custom={} path={path}",
                    sheet.id
                ));
            }
        }
        for (stock, id) in expand_aim_claims(&resolved) {
            if !claims.iter().any(|(s, p, _)| *s == slot && *p == stock) {
                claims.push((slot, stock, id));
            }
        }
    }
}

pub fn install(catalog: &Catalog) -> Result<(), String> {
    let va = catalog
        .function_va("WormEntity__calc_sprite")
        .ok_or("missing WormEntity__calc_sprite catalog entry")?;
    let original = unsafe {
        MinHook::create_hook(rb(va) as *mut c_void, calc_sprite_hook as *mut c_void)
            .map_err(|e| format!("worm sprite hook: {e:?}"))?
    };
    ORIGINAL.store(original, Ordering::SeqCst);
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
    log_line("worm sprite wrap and deterministic empty-sprite metadata installed");
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
    let Ok(claims) = CLAIMS.lock() else {
        return;
    };
    if !claims.iter().any(|(s, _, _)| *s == slot) {
        return;
    }
    let stock = pose_key(original, state, read_i32(worm, WORM_SLOPE_DIR));
    if let Some((_, _, id)) = claims
        .iter()
        .find(|(s, claimed, _)| *s == slot && *claimed == stock)
    {
        *sprite_out = (original & !0xffff) | *id as u32;
        if is_link(stock) {
            // Stock phase is the aim angle across the 180° sheet.
        } else if is_throw(stock) {
            let link_pose = match stock {
                THROW_U => LINK_U,
                THROW_D => LINK_D,
                _ => LINK,
            };
            let eaten = claims
                .iter()
                .any(|(s, pose, sheet)| *s == slot && *pose == link_pose && *sheet == *id);
            if eaten {
                // Eat draw/undraw: hold a mid frame of the aim sheet, no play-through.
                if !phase_out.is_null() {
                    *phase_out = 0x8000;
                }
            } else {
                drive_phase(worm as usize, stock, *id, phase_out);
            }
        } else {
            drive_phase(worm as usize, stock, *id, phase_out);
        }
    }
}

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
    let last = 0x1_0000u32 - (0x1_0000u32 / frames as u32) / 2;
    entry.2 = (entry.2 + step).min(last);
}

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
    use super::{expand_aim_claims, pose_key, LINK, LINK_D, LINK_U, THROW, THROW_D, THROW_U};

    #[test]
    fn custom_link_uses_state_and_slope_but_throw_keeps_stock_id() {
        assert_eq!(pose_key(0x40000, 0x67, 0), LINK);
        assert_eq!(pose_key(0x40000, 0x68, -1), LINK_U);
        assert_eq!(pose_key(0x40000, 0x69, 1), LINK_D);
        assert_eq!(pose_key(0x4024f, 0x6a, 0), THROW);
        assert_eq!(pose_key(0x40000, 0x6a, 0), 0);
    }

    #[test]
    fn bazooka_and_homing_barrels_map_to_link() {
        assert_eq!(pose_key(525, 0x67, 0), LINK);
        assert_eq!(pose_key(526, 0x67, 0), LINK_U);
        assert_eq!(pose_key(527, 0x67, 0), LINK_D);
        assert_eq!(pose_key(531, 0x67, 0), LINK); // wbaz2 — HM family
        assert_eq!(pose_key(532, 0x67, 0), LINK_U);
        assert_eq!(pose_key(535, 0x67, 0), LINK_U); // wbaz3u
        assert_eq!(pose_key(536, 0x67, 0), LINK_D); // wbaz3d
    }

    #[test]
    fn fuse_and_cluster_holds_map_to_link() {
        assert_eq!(pose_key(576, 0x67, 0), LINK); // wthrgrn
        assert_eq!(pose_key(579, 0x67, 0), LINK); // wthrcls
        assert_eq!(pose_key(436, 0x6a, 0), THROW); // wbazlnk draw
        assert_eq!(pose_key(313, 0x6a, 0), THROW); // wgrnlnk
    }

    #[test]
    fn one_link_sheet_fills_slopes_and_eats_throw() {
        let out = expand_aim_claims(&[(LINK, 900)]);
        assert!(out.contains(&(LINK, 900)));
        assert!(out.contains(&(LINK_U, 900)));
        assert!(out.contains(&(LINK_D, 900)));
        assert!(out.contains(&(THROW, 900)));
        assert!(out.contains(&(THROW_U, 900)));
        assert!(out.contains(&(THROW_D, 900)));
    }

    #[test]
    fn three_link_sheets_keep_slopes_and_eat_throw() {
        let out = expand_aim_claims(&[(LINK, 1), (LINK_U, 2), (LINK_D, 3)]);
        assert!(out.contains(&(LINK, 1)));
        assert!(out.contains(&(LINK_U, 2)));
        assert!(out.contains(&(LINK_D, 3)));
        assert!(out.contains(&(THROW, 1)));
        assert!(out.contains(&(THROW_U, 2)));
        assert!(out.contains(&(THROW_D, 3)));
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
