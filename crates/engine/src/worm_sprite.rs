//! Slot-scoped worm art. Stock calculates animation, slope, position and flip;
//! this wrap changes only the resulting sprite id before the stock blit.
//!
//! `weaponlnk` / `u` / `d` are the custom-weapon aiming poses (stock sprite id 0
//! while states 0x67..=0x69). Stock already writes the aim-angle phase — keep it.
//! `wthrow`* is draw/undraw; when aiming is claimed we remap those to the aim
//! sheets and freeze phase so stock draw of another weapon never plays.

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

fn is_link(pose: i32) -> bool {
    matches!(pose, LINK | LINK_U | LINK_D)
}

fn is_throw(pose: i32) -> bool {
    matches!(pose, THROW | THROW_U | THROW_D)
}

fn pose_key(original: u32, state: i32, slope: i32) -> i32 {
    let stock = (original & 0xffff) as i32;
    if stock == 0 && matches!(state, 0x67..=0x69) {
        match slope.cmp(&0) {
            std::cmp::Ordering::Less => LINK_U,
            std::cmp::Ordering::Greater => LINK_D,
            std::cmp::Ordering::Equal => LINK,
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
    let trampoline = unsafe {
        MinHook::create_hook(rb(va) as *mut c_void, calc_sprite_hook as *mut c_void)
            .map_err(|e| format!("worm sprite hook: {e:?}"))?
    };
    ORIGINAL.store(trampoline, Ordering::SeqCst);
    Ok(())
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
            // Eat draw/undraw: hold a mid frame of the aim sheet, no play-through.
            if !phase_out.is_null() {
                *phase_out = 0x8000;
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
