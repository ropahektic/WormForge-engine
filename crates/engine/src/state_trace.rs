//! Worm state trace, for finding what leaves a worm stuck (a softlock).
//!
//! Logs, per worm:
//! - every `SetState` call (WA's or ours), with the state asked for and the
//!   one the worm ends in (SetState can redirect: 0x85 becomes 0x65), the
//!   worm message it ran in and who asked (`cause`: our own callers tag
//!   themselves; untagged is WA);
//! - every change of `+0x44` made without SetState (a direct write, WA's or
//!   ours), seen at the end of the worm's frame;
//! - a worm that keeps one state other than idle (standing, or the turn's
//!   worm ready) for 250, 1000 and 5000 frames, with its position, speed and the raw fields around the state,
//!   which is what a frozen worm looks like in the log.
//!
//! Logging only: nothing here writes to the game, so a replay traces the same
//! on every machine and plays the same with or without it.
//!
//! States print by number and by their `game::worm_state` label, which
//! carries the evidence for each and marks a provisional one with `?`.

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Mutex;

use game::entity::read_i32;
use game::offsets::{
    ENT_ACTIVE, ENT_MOVED, ENT_POS_X, ENT_POS_Y, ENT_SPEED_X, ENT_SPEED_Y, ENT_WORLD,
    ENT_WORM_TEAM, GAME_WORLD_FRAME_COUNTER, MSG_FRAME_FINISH, WORM_INDEX, WORM_STATE,
    WORM_TURN_ACTIVE,
};

use crate::log_line;

type SetStateFn = unsafe extern "thiscall" fn(*mut u8, u32);

static ORIG: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// A worm's last traced state, and the frame it entered it.
#[derive(Clone, Copy)]
struct Track {
    state: u32,
    since: i32,
}

static TRACKS: Mutex<Option<HashMap<u32, Track>>> = Mutex::new(None);

/// Frames in one state after which it is logged again.
const STALL_FRAMES: [i32; 3] = [250, 1000, 5000];
/// States a worm rests in for as long as it likes: standing, and the turn's
/// worm ready to act (`game::worm_state`).
const IDLE: [u32; 2] = [0x65, 0x67];

thread_local! {
    /// Who is calling SetState, when it is us.
    static CAUSE: Cell<&'static str> = const { Cell::new("") };
    /// The worm message being handled (innermost), 0 outside one.
    static MSG: Cell<u32> = const { Cell::new(0) };
}

/// The state's debug label (`game::worm_state`), `?` for one not seen yet.
fn name(state: u32) -> &'static str {
    game::worm_state::label(state).unwrap_or("?")
}

pub fn install() -> Result<(), String> {
    unsafe {
        ORIG.store(
            crate::wa::create_hook(
                crate::wa::address("WORM_SET_STATE") as *mut c_void,
                set_state_hook as *mut c_void,
            )
            .map_err(|e| format!("worm SetState trace hook: {e:?}"))?,
            Ordering::SeqCst,
        );
    }
    log_line("worm state trace installed (SetState, direct writes, stalls)");
    Ok(())
}

/// Run `f` with SetState calls in it attributed to `cause`.
pub fn with_cause<R>(cause: &'static str, f: impl FnOnce() -> R) -> R {
    let prev = CAUSE.with(|c| c.replace(cause));
    let r = f();
    CAUSE.with(|c| c.set(prev));
    r
}

/// Around WA's handling of one worm message: remembers the message for the
/// SetState lines. After the frame's own message (every live worm gets one a
/// frame), reports a state changed without SetState and a stalled worm; not
/// after any other, which may be the one a dying worm is freed in.
pub fn message<R>(this: *mut u8, msg: u32, f: impl FnOnce() -> R) -> R {
    let prev = MSG.with(|m| m.replace(msg));
    let r = f();
    MSG.with(|m| m.set(prev));
    if !this.is_null() && msg == MSG_FRAME_FINISH {
        unsafe { check(this) };
    }
    r
}

/// Our own direct write of a worm's state (not through SetState).
pub fn wrote(this: *mut u8, from: u32, to: u32, cause: &str) {
    if this.is_null() || from == to {
        return;
    }
    unsafe {
        log_line(&format!(
            "state {} {:#x} {} -> {:#x} {} (direct write by {cause}){}",
            who(this),
            from,
            name(from),
            to,
            name(to),
            place(this)
        ));
        remember(this, to);
    }
}

unsafe extern "thiscall" fn set_state_hook(this: *mut u8, state: u32) {
    let orig = ORIG.load(Ordering::SeqCst);
    if orig.is_null() {
        return;
    }
    let f: SetStateFn = unsafe { std::mem::transmute(orig) };
    if this.is_null() {
        unsafe { f(this, state) };
        return;
    }
    let before = unsafe { read_i32(this, WORM_STATE) as u32 };
    unsafe { f(this, state) };
    let after = unsafe { read_i32(this, WORM_STATE) as u32 };
    let cause = CAUSE.with(|c| c.get());
    let msg = MSG.with(|m| m.get());
    let asked = if after == state {
        String::new()
    } else {
        format!(" (asked {state:#x} {})", name(state))
    };
    unsafe {
        log_line(&format!(
            "state {} {:#x} {} -> {:#x} {}{asked} by {} in msg {msg:#x}{}",
            who(this),
            before,
            name(before),
            after,
            name(after),
            if cause.is_empty() { "WA" } else { cause },
            place(this)
        ));
        remember(this, after);
    }
}

/// At the end of a worm's frame: a state that moved without SetState since
/// the last check, and a worm that has held one state too long.
unsafe fn check(this: *mut u8) {
    let state = unsafe { read_i32(this, WORM_STATE) as u32 };
    let now = unsafe { frame(this) };
    let Ok(mut g) = TRACKS.lock() else {
        return;
    };
    let tracks = g.get_or_insert_with(HashMap::new);
    let key = this as u32;
    match tracks.get(&key).copied() {
        None => {
            tracks.insert(key, Track { state, since: now });
        }
        Some(t) if t.state != state => {
            tracks.insert(key, Track { state, since: now });
            drop(g);
            unsafe {
                log_line(&format!(
                    "state {} {:#x} {} -> {:#x} {} (direct write, seen at frame end){}",
                    who(this),
                    t.state,
                    name(t.state),
                    state,
                    name(state),
                    place(this)
                ));
            }
        }
        Some(t) => {
            let held = now - t.since;
            if !IDLE.contains(&state) && STALL_FRAMES.contains(&held) {
                drop(g);
                unsafe { stall(this, state, held) };
            }
        }
    }
}

unsafe fn stall(this: *mut u8, state: u32, held: i32) {
    let raw: Vec<String> = (0x3C..=0x5C)
        .step_by(4)
        .map(|off| format!("{:x}", unsafe { read_i32(this, off) }))
        .collect();
    unsafe {
        log_line(&format!(
            "state {} still {:#x} {} after {held} frames{} active={} moved={} turn={} raw+3c=[{}]",
            who(this),
            state,
            name(state),
            place(this),
            read_i32(this, ENT_ACTIVE),
            read_i32(this, ENT_MOVED),
            read_i32(this, WORM_TURN_ACTIVE),
            raw.join(" ")
        ));
    }
}

/// The worm's state now; its clock restarts only if the state changed, so a
/// worm set to the same state again and again still shows up as stalled.
unsafe fn remember(this: *mut u8, state: u32) {
    let now = unsafe { frame(this) };
    if let Ok(mut g) = TRACKS.lock() {
        let tracks = g.get_or_insert_with(HashMap::new);
        let key = this as u32;
        if tracks.get(&key).map(|t| t.state) != Some(state) {
            tracks.insert(key, Track { state, since: now });
        }
    }
}

unsafe fn frame(this: *mut u8) -> i32 {
    let world = unsafe { read_i32(this, ENT_WORLD) } as *mut u8;
    if world.is_null() {
        -1
    } else {
        unsafe { read_i32(world, GAME_WORLD_FRAME_COUNTER) }
    }
}

/// `w=<address> t<team>#<index> f=<frame>`.
unsafe fn who(this: *mut u8) -> String {
    unsafe {
        format!(
            "w={:#x} t{}#{} f={}",
            this as u32,
            read_i32(this, ENT_WORM_TEAM),
            read_i32(this, WORM_INDEX),
            frame(this)
        )
    }
}

/// ` at (x,y) v(vx,vy)` in pixels, speeds to 1/100 px a frame.
unsafe fn place(this: *mut u8) -> String {
    let px = |v: i32| v >> 16;
    let sp = |v: i32| (v as i64 * 100 / 0x10000) as i32;
    unsafe {
        format!(
            " at ({},{}) v({},{})/100",
            px(read_i32(this, ENT_POS_X)),
            px(read_i32(this, ENT_POS_Y)),
            sp(read_i32(this, ENT_SPEED_X)),
            sp(read_i32(this, ENT_SPEED_Y))
        )
    }
}
