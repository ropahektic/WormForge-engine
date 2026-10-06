//! `worm.windup`: how many frames a worm's wind-up (prefiring, state 0x69)
//! has left before it releases its weapon, when a pack sets it.
//!
//! WA's own wind-up (`handle_PreFiring`) waits for the wind-up animation (a
//! click weapon, fire type 3) and then counts `worm+0x2A4` down. A worm with a
//! Lua value skips both: the engine counts that value down instead, one per
//! handler call, and at 0 enters WA's own release block, which releases the
//! weapon (`release_weapon_0` -> `WeaponRelease` -> `FireWeapon`) and ends the
//! prefiring state. The shot, the turn and manual placement go on as WA's.
//!
//! Only simulation callbacks set a value, so every peer and replay agrees.
//! Values are cleared at every turn start and every match.

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use std::sync::Mutex;

static ORIG_PREFIRING: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
/// WA's release block inside `handle_PreFiring`, entered with the handler's
/// frame set up and ESI = the worm.
static RELEASE: AtomicU32 = AtomicU32::new(0);
/// (worm, frames left).
static WINDUPS: Mutex<Vec<(u32, i32)>> = Mutex::new(Vec::new());

/// `worm.windup = frames` (None: WA's own wind-up again).
pub fn set(worm: u32, frames: Option<i32>) {
    let Ok(mut g) = WINDUPS.lock() else {
        return;
    };
    g.retain(|(w, _)| *w != worm);
    if let Some(frames) = frames {
        g.push((worm, frames.max(0)));
    }
}

/// The frames a pack's wind-up has left, if it set one.
pub fn get(worm: u32) -> Option<i32> {
    WINDUPS.lock().ok()?.iter().find(|(w, _)| *w == worm).map(|(_, f)| *f)
}

/// At a turn start and a new match: values never outlive the fire they were
/// set for.
pub fn clear() {
    if let Ok(mut g) = WINDUPS.lock() {
        g.clear();
    }
}

const STOCK: u32 = 0;
const RELEASE_NOW: u32 = 1;
const WAIT: u32 = 2;

/// What the handler does for `worm` this frame.
fn decide(worm: u32) -> u32 {
    let Ok(mut g) = WINDUPS.lock() else {
        return STOCK;
    };
    let Some(i) = g.iter().position(|(w, _)| *w == worm) else {
        return STOCK;
    };
    if g[i].1 <= 0 {
        g.remove(i);
        return RELEASE_NOW;
    }
    g[i].1 -= 1;
    WAIT
}

extern "C" fn decide_c(worm: *mut u8) -> u32 {
    if worm.is_null() || RELEASE.load(Ordering::Relaxed) == 0 {
        return STOCK;
    }
    decide(worm as u32)
}

/// `handle_PreFiring` (usercall EAX = worm, plain `ret`). WA's own handler,
/// nothing this frame, or its prologue replayed and a jump to its release
/// block (`sub esp,8; push ebx; push esi; mov esi,eax; push edi`, which the
/// block's exit pops).
#[unsafe(naked)]
unsafe extern "C" fn prefiring_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push eax",
        "call {decide}",
        "add esp, 4",
        "mov ecx, eax",
        "pop eax",
        "test ecx, ecx",
        "jz 2f",
        "cmp ecx, 1",
        "je 3f",
        "ret",
        "2:",
        "jmp dword ptr [{orig}]",
        "3:",
        "sub esp, 8",
        "push ebx",
        "push esi",
        "mov esi, eax",
        "push edi",
        "jmp dword ptr [{release}]",
        decide = sym decide_c,
        orig = sym ORIG_PREFIRING,
        release = sym RELEASE,
    );
}

pub fn install() -> Result<(), String> {
    let (Some(handler), Some(release)) = (
        crate::wa::try_address("WORM_HANDLE_PREFIRING"),
        crate::wa::try_address("WORM_PREFIRING_RELEASE"),
    ) else {
        return Err("WORM_HANDLE_PREFIRING / WORM_PREFIRING_RELEASE not located".into());
    };
    let trampoline =
        unsafe { crate::wa::create_hook(handler as *mut c_void, prefiring_hook as *mut c_void) }?;
    ORIG_PREFIRING.store(trampoline, Ordering::SeqCst);
    RELEASE.store(release, Ordering::SeqCst);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_wind_up_waits_its_frames_then_releases_once() {
        clear();
        assert_eq!(decide(7), STOCK, "no value: WA's own wind-up");
        set(7, Some(2));
        assert_eq!(get(7), Some(2));
        assert_eq!(decide(7), WAIT);
        assert_eq!(decide(7), WAIT);
        assert_eq!(get(7), Some(0));
        assert_eq!(decide(7), RELEASE_NOW);
        assert_eq!(get(7), None, "used up by the release");
        assert_eq!(decide(7), STOCK);
        set(8, Some(-3));
        assert_eq!(decide(8), RELEASE_NOW, "a negative value releases at once");
        set(9, Some(5));
        set(9, None);
        assert_eq!(decide(9), STOCK, "nil gives the wind-up back to WA");
        set(10, Some(1));
        clear();
        assert_eq!(get(10), None);
    }
}
