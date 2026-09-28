//! Worm state trace, for finding what leaves a worm stuck (a softlock).
//!
//! Logs, per worm:
//! - every `SetState` call (WA's or ours), with the state asked for and the
//!   one the worm ends in (SetState can redirect: 0x85 becomes 0x65), the
//!   worm message it ran in and who asked (`cause`: our own callers tag
//!   themselves; untagged is WA);
//! - every change of `+0x44` made without SetState (a direct write, WA's or
//!   ours), found when a worm message returns;
//! - a worm that keeps one state other than standing for 250, 1000 and 5000
//!   frames, with its position, speed and the raw fields around the state,
//!   which is what a frozen worm looks like in the log.
//!
//! Logging only: nothing here writes to the game, so a replay traces the same
//! on every machine and plays the same with or without it.
//!
//! State names are the ones known for sure; any other prints as `?` next to
//! its number. SetState (0x50E850) has a case per state 0x64..=0x8B (jump
//! table 0x50F484); the case's setup routine is the best lead for naming the
//! rest: 0x65 0x516CA0 (Standing, also 0x88 and 0x8B), 0x67 0x517100, 0x69
//! 0x517850 (PreFiring, also 0x8A), 0x6A 0x517F00, 0x6B 0x5181D0, 0x6C
//! 0x518340 (Punching), 0x6D 0x518A50, 0x6E 0x5186A0 (Drilling), 0x6F
//! 0x5196D0 (Teleporting), 0x70 0x519D80, 0x71 0x51A7B0, 0x74 0x51A750, 0x78
//! 0x513980 (JetPacking), 0x7B 0x519E50, 0x7C 0x51A3B0 (Bungee), 0x7D
//! 0x51A710 (Parachuting), 0x86 0x516B40. The others have no setup call.

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

/// Frames in one state (other than standing) after which it is logged again.
const STALL_FRAMES: [i32; 3] = [250, 1000, 5000];

thread_local! {
    /// Who is calling SetState, when it is us.
    static CAUSE: Cell<&'static str> = const { Cell::new("") };
    /// The worm message being handled (innermost), 0 outside one.
    static MSG: Cell<u32> = const { Cell::new(0) };
}

/// Names we are sure of. `?` for the rest: the number is what to go by.
fn name(state: u32) -> &'static str {
    match state {
        0x65 => "standing",
        0x69 => "prefiring",
        0x6C => "punching",
        0x6E => "drilling",
        0x6F => "teleporting",
        0x73 => "charging",
        0x78 => "jetpacking",
        0x7B => "aiming",
        0x7C => "bungee",
        0x7D => "parachuting",
        0x7E => "sliding",
        0x7F => "sinking",
        0x80 => "hurt",
        _ => "?",
    }
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
            if state != 0x65 && STALL_FRAMES.contains(&held) {
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
