//! `wa.perf.enable{ counter, profile }`: a pack turns on WormForge's FPS
//! counter and profiler for this machine. The engine reads the flags; Lua
//! never reads a timing back, so nothing measured here can reach the
//! simulation.
//!
//! With `profile` on, the time spent inside Lua is summed here, at the two
//! places every engine call into the Lua state goes through.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

static COUNTER: AtomicBool = AtomicBool::new(false);
static PROFILE: AtomicBool = AtomicBool::new(false);
static LUA_NANOS: AtomicU64 = AtomicU64::new(0);
static LUA_CALLS: AtomicU32 = AtomicU32::new(0);

thread_local! {
    /// Engine calls into Lua can nest (a Lua callback reaching a native that
    /// calls back into Lua); only the outermost is timed.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PerfFlags {
    pub counter: bool,
    pub profile: bool,
}

/// What the loaded packs asked for.
pub fn perf_flags() -> PerfFlags {
    PerfFlags {
        counter: COUNTER.load(Ordering::Relaxed),
        profile: PROFILE.load(Ordering::Relaxed),
    }
}

/// Time spent in Lua since the last call, in ns, and how many engine calls
/// into Lua that was.
pub fn take_lua_time() -> (u64, u32) {
    (
        LUA_NANOS.swap(0, Ordering::Relaxed),
        LUA_CALLS.swap(0, Ordering::Relaxed),
    )
}

pub(crate) fn enable(flags: PerfFlags) {
    if flags.counter {
        COUNTER.store(true, Ordering::Relaxed);
    }
    if flags.profile {
        PROFILE.store(true, Ordering::Relaxed);
    }
}

/// A new match's packs decide afresh.
pub(crate) fn reset() {
    COUNTER.store(false, Ordering::Relaxed);
    PROFILE.store(false, Ordering::Relaxed);
    take_lua_time();
}

/// Run `f`, adding its time to the Lua total when profiling.
pub(crate) fn time<T>(f: impl FnOnce() -> T) -> T {
    if !PROFILE.load(Ordering::Relaxed) {
        return f();
    }
    let depth = DEPTH.with(|d| {
        let v = d.get();
        d.set(v + 1);
        v
    });
    let start = (depth == 0).then(Instant::now);
    let out = f();
    DEPTH.with(|d| d.set(depth));
    if let Some(start) = start {
        LUA_NANOS.fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        LUA_CALLS.fetch_add(1, Ordering::Relaxed);
    }
    out
}
