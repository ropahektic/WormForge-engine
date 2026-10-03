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
//!
//! One engine call that runs SPIKE_NS or longer is a spike, and is kept on
//! its own for the engine to log: what the call was for, and the Lua memory
//! before and after (a drop means a GC cycle ran in it). With `spikes` on,
//! also the Lua lines its time went to. Those come from a sampler, a count
//! hook that notes where Lua is every SAMPLE_EVERY instructions once a call
//! has run a while; with a count hook set, Lua checks it on every
//! instruction, which is why it is opt-in.
//! A native's time (a sound or sprite load) goes to the line Lua is on when
//! the sampler next fires, which is at or just after the call to it.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use mlua::{HookTriggers, Lua, VmState};

/// A single engine call into Lua this long is a spike.
const SPIKE_NS: u64 = 5_000_000;
/// The sampler notes where Lua is this often, in VM instructions.
const SAMPLE_EVERY: u32 = 1000;
/// Before a call has run this long, the sampler only reads the clock.
const SAMPLE_AFTER: Duration = Duration::from_millis(1);
/// Lines kept per call; the time of any further ones goes to `rest`.
const MAX_PLACES: usize = 32;
/// Lines a spike reports.
const SPIKE_PLACES: usize = 4;

static COUNTER: AtomicBool = AtomicBool::new(false);
static PROFILE: AtomicBool = AtomicBool::new(false);
static PACKS: AtomicBool = AtomicBool::new(false);
static SPIKES_ON: AtomicBool = AtomicBool::new(false);
static LUA_NANOS: AtomicU64 = AtomicU64::new(0);
static LUA_CALLS: AtomicU32 = AtomicU32::new(0);

thread_local! {
    /// Engine calls into Lua can nest (a Lua callback reaching a native that
    /// calls back into Lua); only the outermost is timed.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    /// Time and calls per label, and per pack, since the last take.
    static BY_LABEL: RefCell<BTreeMap<&'static str, (u64, u32)>> = const { RefCell::new(BTreeMap::new()) };
    static BY_PACK: RefCell<BTreeMap<String, (u64, u32)>> = const { RefCell::new(BTreeMap::new()) };
    /// The outermost call running now, for the sampler.
    static CALL: RefCell<Option<Call>> = const { RefCell::new(None) };
    /// Spikes since the last take.
    static SPIKES: RefCell<Vec<Spike>> = const { RefCell::new(Vec::new()) };
}

struct Call {
    start: Instant,
    /// When the sampler last noted a line.
    last: Instant,
    /// (short_src, line, ns)
    places: Vec<(String, i32, u64)>,
}

/// One engine call into Lua that took SPIKE_NS or longer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spike {
    pub label: &'static str,
    pub ns: u64,
    /// Lua's memory in bytes as the call began and as it ended.
    pub mem_before: usize,
    pub mem_after: usize,
    /// The lines the sampler found it on, longest first: ("file:line", ns).
    /// Empty without `spikes`.
    pub places: Vec<(String, u64)>,
    /// The time not put on a line in `places`.
    pub rest_ns: u64,
}

/// Spikes since the last call, oldest first.
pub fn take_lua_spikes() -> Vec<Spike> {
    SPIKES.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// Start the sampler on `lua`, for `wa.perf.enable{ spikes = true }`.
pub(crate) fn install_sampler(lua: &Lua) {
    lua.set_hook(HookTriggers::new().every_nth_instruction(SAMPLE_EVERY), |_, debug| {
        CALL.with(|c| {
            let Ok(mut c) = c.try_borrow_mut() else { return };
            let Some(call) = c.as_mut() else { return };
            let now = Instant::now();
            if now.duration_since(call.start) < SAMPLE_AFTER {
                return;
            }
            let ns = now.duration_since(call.last).as_nanos() as u64;
            call.last = now;
            let line = debug.curr_line();
            let source = debug.source();
            let src = source.short_src.as_deref().unwrap_or("?");
            if let Some(p) = call.places.iter_mut().find(|p| p.1 == line && p.0 == src) {
                p.2 += ns;
            } else if call.places.len() < MAX_PLACES {
                call.places.push((src.to_string(), line, ns));
            }
        });
        Ok(VmState::Continue)
    });
}

fn spike(label: &'static str, ns: u64, mem_before: usize, mem_after: usize, call: Call) -> Spike {
    let mut places: Vec<(String, u64)> =
        call.places.into_iter().map(|(src, line, ns)| (format!("{src}:{line}"), ns)).collect();
    places.sort_by(|a, b| b.1.cmp(&a.1));
    places.truncate(SPIKE_PLACES);
    let placed: u64 = places.iter().map(|p| p.1).sum();
    Spike { label, ns, mem_before, mem_after, places, rest_ns: ns.saturating_sub(placed) }
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

/// Whether hook handlers are timed one by one, for the per-pack breakdown.
pub(crate) fn profiling_packs() -> bool {
    PROFILE.load(Ordering::Relaxed) && PACKS.load(Ordering::Relaxed)
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
    /// With `profile`: time every hook handler on its own, for the per-pack
    /// breakdown. It costs a clock read and a lookup per handler, so hook
    /// lists then run handler by handler from the engine, slower.
    pub packs: bool,
    /// With `profile`: sample which Lua lines long calls (spikes) spend their
    /// time on. Lua runs slower while a sampler is set.
    pub spikes: bool,
}

/// What the loaded packs asked for.
pub fn perf_flags() -> PerfFlags {
    PerfFlags {
        counter: COUNTER.load(Ordering::Relaxed),
        profile: PROFILE.load(Ordering::Relaxed),
        packs: PACKS.load(Ordering::Relaxed),
        spikes: SPIKES_ON.load(Ordering::Relaxed),
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
    if flags.packs {
        PACKS.store(true, Ordering::Relaxed);
    }
    if flags.spikes {
        SPIKES_ON.store(true, Ordering::Relaxed);
    }
}

/// A new match's packs decide afresh.
pub(crate) fn reset() {
    COUNTER.store(false, Ordering::Relaxed);
    PROFILE.store(false, Ordering::Relaxed);
    PACKS.store(false, Ordering::Relaxed);
    SPIKES_ON.store(false, Ordering::Relaxed);
    take_lua_time();
    take_lua_breakdown();
    take_lua_spikes();
}

/// Run `f`, adding its time to the Lua total and to `label` when profiling,
/// and keeping it as a spike when it is long.
pub(crate) fn time<T>(label: &'static str, lua: &Lua, f: impl FnOnce() -> T) -> T {
    if !PROFILE.load(Ordering::Relaxed) {
        return f();
    }
    let depth = DEPTH.with(|d| {
        let v = d.get();
        d.set(v + 1);
        v
    });
    let start = (depth == 0).then(|| {
        let now = Instant::now();
        CALL.with(|c| *c.borrow_mut() = Some(Call { start: now, last: now, places: Vec::new() }));
        (now, lua.used_memory())
    });
    let out = f();
    DEPTH.with(|d| d.set(depth));
    if let Some((start, mem_before)) = start {
        let ns = start.elapsed().as_nanos() as u64;
        if let Some(call) = CALL.with(|c| c.borrow_mut().take()) {
            if ns >= SPIKE_NS {
                let s = spike(label, ns, mem_before, lua.used_memory(), call);
                SPIKES.with(|v| v.borrow_mut().push(s));
            }
        }
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
        let lua = Lua::new();
        enable(PerfFlags { counter: false, profile: true, packs: true, spikes: false });
        take_lua_breakdown();
        time("worm_message", &lua, || {
            // A nested call into Lua is part of the outer one.
            time("actor", &lua, || ());
        });
        time("actor", &lua, || ());
        add_pack_time("example.highlander".into(), 500);
        add_pack_time("example.highlander".into(), 700);
        let b = take_lua_breakdown();
        let labels: Vec<_> = b.by_label.iter().map(|(n, _, c)| (n.as_str(), *c)).collect();
        assert_eq!(labels, [("actor", 1), ("worm_message", 1)]);
        assert_eq!(b.by_pack, vec![("example.highlander".to_string(), 1200, 2)]);
        assert_eq!(take_lua_breakdown(), LuaBreakdown::default(), "a take empties it");
        assert!(take_lua_spikes().is_empty(), "short calls are no spikes");
        reset();
    }

    #[test]
    fn a_long_call_is_a_spike_with_the_lines_it_spent_its_time_on() {
        let lua = Lua::new();
        enable(PerfFlags { counter: false, profile: true, packs: false, spikes: false });
        install_sampler(&lua);
        take_lua_spikes();
        lua.globals()
            .set("wait", lua.create_function(|_, ms: u64| {
                std::thread::sleep(Duration::from_millis(ms));
                Ok(())
            }).unwrap())
            .unwrap();
        let f: mlua::Function = lua.load("return function()\n  wait(12)\n  for i = 1, 5000 do end\n  junk = {}\n  for i = 1, 3000 do junk[i] = {} end\nend").set_name("=slow.lua").eval().unwrap();
        time("turn_start", &lua, || f.call::<()>(()).unwrap());
        let spikes = take_lua_spikes();
        assert_eq!(spikes.len(), 1, "{spikes:?}");
        let s = &spikes[0];
        assert_eq!(s.label, "turn_start");
        assert!(s.ns >= 12_000_000, "{s:?}");
        assert!(s.mem_after > s.mem_before, "{s:?}");
        // The native's time lands on the loop right after it.
        let (place, ns) = &s.places[0];
        assert!(place.starts_with("slow.lua:"), "{s:?}");
        assert!(*ns >= 11_000_000, "{s:?}");
        time("actor", &lua, || ());
        assert!(take_lua_spikes().is_empty());
        lua.remove_hook();
        reset();
    }
}
