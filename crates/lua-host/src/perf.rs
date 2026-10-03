//! `wa.perf.enable{ counter, profile }`: a pack turns on WormForge's FPS
//! counter and profiler for this machine. The engine reads the flags; Lua
//! never reads a timing back, so nothing measured here can reach the
//! simulation.
//!
//! With `profile` on, the time spent inside Lua is summed here, at the two
//! places every engine call into the Lua state goes through, by what the
//! call was for (a hook's name, a weapon step, an actor's brain). Where the
//! engine walks a hook list, each handler's time also goes to the pack that
//! registered it.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
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
    /// Time and calls per label, and per pack, since the last take.
    static BY_LABEL: RefCell<BTreeMap<&'static str, (u64, u32)>> = const { RefCell::new(BTreeMap::new()) };
    static BY_PACK: RefCell<BTreeMap<String, (u64, u32)>> = const { RefCell::new(BTreeMap::new()) };
}

/// Lua time since the last take, by label and by pack: (name, ns, calls),
/// in name order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LuaBreakdown {
    pub by_label: Vec<(String, u64, u32)>,
    pub by_pack: Vec<(String, u64, u32)>,
}

pub fn take_lua_breakdown() -> LuaBreakdown {
    let drain = |m: &mut BTreeMap<_, (u64, u32)>| -> Vec<(String, u64, u32)> {
        std::mem::take(m).into_iter().map(|(k, (ns, n))| (String::from(k), ns, n)).collect()
    };
    LuaBreakdown {
        by_label: BY_LABEL.with(|m| drain(&mut m.borrow_mut())),
        by_pack: BY_PACK.with(|m| {
            std::mem::take(&mut *m.borrow_mut()).into_iter().map(|(k, (ns, n))| (k, ns, n)).collect()
        }),
    }
}

/// Whether the profiler is on (so callers can skip their own timing).
pub(crate) fn profiling() -> bool {
    PROFILE.load(Ordering::Relaxed)
}

/// One hook handler's time, for the pack that registered it.
pub(crate) fn add_pack_time(pack: String, ns: u64) {
    BY_PACK.with(|m| {
        let mut m = m.borrow_mut();
        let e = m.entry(pack).or_insert((0, 0));
        e.0 += ns;
        e.1 += 1;
    });
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
    take_lua_breakdown();
}

/// Run `f`, adding its time to the Lua total and to `label` when profiling.
pub(crate) fn time<T>(label: &'static str, f: impl FnOnce() -> T) -> T {
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
        let ns = start.elapsed().as_nanos() as u64;
        LUA_NANOS.fetch_add(ns, Ordering::Relaxed);
        LUA_CALLS.fetch_add(1, Ordering::Relaxed);
        BY_LABEL.with(|m| {
            let mut m = m.borrow_mut();
            let e = m.entry(label).or_insert((0, 0));
            e.0 += ns;
            e.1 += 1;
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_goes_to_the_outermost_label_and_hooks_to_their_pack() {
        enable(PerfFlags { counter: false, profile: true });
        take_lua_breakdown();
        time("worm_message", || {
            // A nested call into Lua is part of the outer one.
            time("actor", || ());
        });
        time("actor", || ());
        add_pack_time("example.highlander".into(), 500);
        add_pack_time("example.highlander".into(), 700);
        let b = take_lua_breakdown();
        let labels: Vec<_> = b.by_label.iter().map(|(n, _, c)| (n.as_str(), *c)).collect();
        assert_eq!(labels, [("actor", 1), ("worm_message", 1)]);
        assert_eq!(b.by_pack, vec![("example.highlander".to_string(), 1200, 2)]);
        assert_eq!(take_lua_breakdown(), LuaBreakdown::default(), "a take empties it");
        reset();
    }
}
