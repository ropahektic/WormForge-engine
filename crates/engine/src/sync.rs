//! Determinism probe: hash the simulation every frame and say when it drifts.
//!
//! WA is a lockstep engine. Two peers, or a replay and the game it recorded,
//! must step identical state from identical input. This module makes that
//! claim *checkable* instead of assumed, and it is the gate on whether a
//! re-simulating replay is possible at all: if two runs of the same recorded
//! input do not produce the same hash stream, nothing built on top can work.
//!
//! It is worth having even if replay is never built. WormForge adds actors
//! that crater terrain and damage worms, and today nothing would tell you if
//! that had quietly desynced a networked game.
//!
//! **The canary is WA's RNG.** There is one shared RNG for gameplay and for
//! visual effects, so "visual-only" is not a category that exists here.
//! OpenWA's desync journal traced a whole-match divergence to a skipped
//! `debris.spr` load that changed how often `GenerateDebrisParticles` ticked
//! that word — 15,470 writes against 17,106. If our renderer perturbs WA at
//! all, this is where it shows.
//!
//! Hashes are split by component rather than rolled into one number, because
//! "which part drifted" is most of the diagnosis: RNG alone means we
//! perturbed WA, actors alone means the fault is ours.
//!
//! Gated behind a `sync.txt` marker next to `wkLua.log`, exactly like
//! `queue_census`, so a normal install pays one relaxed atomic load per frame
//! and writes nothing.

use crate::log_line;
use game::entity::read_i32;
use game::hash::Fnv;
use game::offsets::{
    ACTOR_PHASE, ACTOR_SPRITE, ENT_POS_X, ENT_POS_Y, ENT_SPEED_X, ENT_SPEED_Y,
    GAME_WORLD_FRAME_COUNTER, GAME_WORLD_GAME_RNG, GAME_WORLD_RECORDED_FRAME,
    GAME_WORLD_RNG_STATE_1, GAME_WORLD_RNG_STATE_2,
};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// 0 unknown, 1 off, 2 on. Resolved once from the marker file.
static ENABLED: AtomicU32 = AtomicU32::new(0);
/// Last frame we hashed, so a repeated or rewound counter is visible.
static LAST_FRAME: AtomicU64 = AtomicU64::new(u64::MAX);
static FRAMES: AtomicU64 = AtomicU64::new(0);
static ANNOUNCED: AtomicU32 = AtomicU32::new(0);

/// Log a line this often. 50 frames is one second of WA at 50 fps, which is
/// dense enough to bisect a divergence and still readable by eye.
const REPORT_EVERY: u64 = 50;

fn enabled() -> bool {
    match ENABLED.load(Ordering::Relaxed) {
        0 => {
            let on = crate::log::wormforge_dir().join("sync.txt").exists();
            ENABLED.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            if on {
                log_line(
                    "sync probe enabled; hashing simulation state every frame \
                     (rng / worms / actors)",
                );
            }
            on
        }
        v => v == 2,
    }
}

/// The three RNG words. Divergence here means **we perturbed WA itself**,
/// which is a different and more serious bug than our own actors drifting.
unsafe fn hash_rng(world: *mut u8) -> u32 {
    let mut h = Fnv::new();
    h.write_i32(read_i32(world, GAME_WORLD_GAME_RNG));
    h.write_i32(read_i32(world, GAME_WORLD_RNG_STATE_1));
    h.write_i32(read_i32(world, GAME_WORLD_RNG_STATE_2));
    h.finish()
}

/// Every live worm, in team/index order rather than address order — the walk
/// in `list_worms` is keyed by (team, index), so it is already reproducible.
///
/// As with actors, the handle is not hashed: it is an entity pointer, so it
/// differs between runs even when the worm is in exactly the same state. The
/// (team, index) walk order is the identity that matters.
unsafe fn hash_worms() -> u32 {
    let mut h = Fnv::new();
    for handle in crate::actor::live_worms() {
        let worm = handle as *mut u8;
        h.write_i32(read_i32(worm, ENT_POS_X));
        h.write_i32(read_i32(worm, ENT_POS_Y));
        h.write_i32(read_i32(worm, ENT_SPEED_X));
        h.write_i32(read_i32(worm, ENT_SPEED_Y));
        h.write_i32(crate::world::worm_hp(handle));
    }
    h.finish()
}

/// Our own actors. `LIVE` is a `Vec` and `retain` preserves order, so the walk
/// is insertion-ordered and reproducible.
///
/// The handle itself is deliberately NOT hashed: it is a heap address, so it
/// differs between runs even when the simulation agrees. Hashing it would make
/// every run look divergent. That the *count* and the *order* still have to
/// match is what this actually tests.
unsafe fn hash_actors() -> u32 {
    let mut h = Fnv::new();
    for handle in crate::actor::live_actors() {
        let a = handle as *mut u8;
        h.write_i32(read_i32(a, ENT_POS_X));
        h.write_i32(read_i32(a, ENT_POS_Y));
        h.write_i32(read_i32(a, ENT_SPEED_X));
        h.write_i32(read_i32(a, ENT_SPEED_Y));
        h.write_i32(read_i32(a, ACTOR_SPRITE));
        h.write_i32(read_i32(a, ACTOR_PHASE));
    }
    h.finish()
}

/// Called at the end of every world-root `FRAME_FINISH`, after WA's own
/// handler has run — so this observes end-of-frame state, which is the point
/// at which WA increments its frame counter.
pub unsafe fn observe(world: *mut u8) {
    if !enabled() || world.is_null() {
        return;
    }
    let frame = read_i32(world, GAME_WORLD_FRAME_COUNTER);
    if frame < 0 {
        return;
    }
    let frame = frame as u64;

    // WA's counter, not one of ours. A repeat means this hook ran twice for
    // one sim frame, which would silently double-count and is worth knowing.
    let previous = LAST_FRAME.swap(frame, Ordering::Relaxed);
    if previous == frame {
        return;
    }

    if ANNOUNCED.swap(1, Ordering::Relaxed) == 0 {
        // -1 here means WA is playing a replay back rather than recording one.
        let recorded = read_i32(world, GAME_WORLD_RECORDED_FRAME);
        let net = read_i32(world, game::offsets::GAME_WORLD_NET_GAME);
        log_line(&format!(
            "sync probe: world {world:?} first frame {frame}, \
             recorded_frame_counter {recorded} ({}), net_game {net:#x} ({})",
            if recorded < 0 { "REPLAY PLAYBACK" } else { "live or recording" },
            if net != 0 { "NETWORKED" } else { "local" }
        ));
    }

    let rng = hash_rng(world);
    let worms = hash_worms();
    let actors = hash_actors();

    let n = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    if n % REPORT_EVERY == 0 {
        // One line per report, fixed field order, so two runs diff cleanly
        // with `diff <(grep "^sync f=" a.log) <(grep "^sync f=" b.log)`.
        log_line(&format!(
            "sync f={frame} rng={rng:08x} worms={worms:08x} actors={actors:08x}"
        ));
    }
}

/// Flush a final line at match end, so a short session still yields a tail to
/// compare even if it did not reach a report boundary.
pub fn finish() {
    if !enabled() {
        return;
    }
    let frames = FRAMES.load(Ordering::Relaxed);
    if frames > 0 {
        log_line(&format!("sync probe: {frames} frames hashed this match"));
    }
    FRAMES.store(0, Ordering::Relaxed);
    LAST_FRAME.store(u64::MAX, Ordering::Relaxed);
    ANNOUNCED.store(0, Ordering::Relaxed);
}
