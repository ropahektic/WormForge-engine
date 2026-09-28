//! Entity contacts, from WA's own collision callbacks, for `wa.on.contact`.
//!
//! WA tells a worm it touched another entity through the worm's vtable slot
//! 8, `WormEntity__OnWormPush` (0x50D9A0, thiscall `(other, flags)`, RET 8):
//! Project X's `CWorm::Collide`. We only record the contact there, with the
//! worm's speed before WA exchanges velocities (what PX's scripts check), and
//! hand the frame's contacts to Lua after WA's frame, from the world root's
//! FRAME_FINISH. Calling Lua inside the collision itself would let a pack
//! explode or move things in the middle of WA's contact resolution.
//!
//! Actors are meant to join the same queue (their brain's `hit_worm`), so
//! there stays one contact path.

use crate::log::{log_error, log_line};
use game::entity::read_i32;
use game::offsets::{ENT_SPEED_X, ENT_SPEED_Y};
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Mutex;

type OnContactFn = unsafe extern "thiscall" fn(*mut u8, *mut u8, u32) -> u32;

static ORIG: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static PENDING: Mutex<Vec<lua_host::Contact>> = Mutex::new(Vec::new());

pub fn install() -> Result<(), String> {
    let target = crate::wa::address("WORM_ON_CONTACT") as *mut c_void;
    let trampoline = unsafe { crate::wa::create_hook(target, on_contact_hook as *mut c_void) }?;
    ORIG.store(trampoline, Ordering::SeqCst);
    log_line("worm contact hook installed (wa.on.contact)");
    Ok(())
}

unsafe extern "thiscall" fn on_contact_hook(worm: *mut u8, other: *mut u8, flags: u32) -> u32 {
    if !worm.is_null() && !other.is_null() {
        let contact = lua_host::Contact {
            worm: worm as u32,
            other: other as u32,
            flags,
            vx: unsafe { read_i32(worm, ENT_SPEED_X) },
            vy: unsafe { read_i32(worm, ENT_SPEED_Y) },
        };
        if let Ok(mut g) = PENDING.lock() {
            g.push(contact);
        }
    }
    let orig = ORIG.load(Ordering::SeqCst);
    if orig.is_null() {
        return 0;
    }
    let f: OnContactFn = unsafe { std::mem::transmute(orig) };
    unsafe { f(worm, other, flags) }
}

/// Hand this frame's contacts to Lua, in the order WA made them. Called
/// after the world root's stock FRAME_FINISH, the same point on every peer.
pub fn flush() {
    let contacts = match PENDING.lock() {
        Ok(mut g) if !g.is_empty() => std::mem::take(&mut *g),
        _ => return,
    };
    for c in contacts {
        if let Err(e) = lua_host::contact(c) {
            log_error(&format!("wa.on.contact: {e}"));
        }
    }
}

/// Drop anything recorded but not yet delivered (match start and end).
pub fn clear() {
    if let Ok(mut g) = PENDING.lock() {
        g.clear();
    }
}
