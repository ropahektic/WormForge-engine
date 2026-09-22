//! Measure what WA actually puts in its render queue, so the renderer's
//! design rests on counts rather than on assumptions.
//!
//! The question that matters: **how often does a kind-3 (`draw_via_callback`)
//! command appear?** `truecolor::capture` bails on the entire frame when it
//! sees one, so every such frame silently falls back to palette sprites. The
//! GPU renderer inherits that bail, because it needs the same split of WA's
//! dispatch to build its coverage mask. If kind-3 is common the split cannot
//! be the source of occlusion and the design has to change; if it is rare or
//! absent the bail may be removable, which would improve the shipped
//! compositor too.
//!
//! Also reports the queue's own hard limits, which are easy to forget:
//! `RQ_DrawSpriteLocal` silently drops draws past 0x800 commands or a full
//! 64 KiB arena, so "my sprite vanished in a busy frame" has a second
//! explanation besides ours.
//!
//! Measurement only. Nothing here changes a pixel; delete the module and its
//! two call sites once the numbers are in.

use crate::log_line;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Enabled by a `queue_census.txt` marker next to `wkLua.log`, so a normal
/// install pays one relaxed atomic load per frame and writes nothing.
static ENABLED: AtomicU32 = AtomicU32::new(0);
static FRAMES: AtomicU64 = AtomicU64::new(0);
/// Frames containing at least one kind-3 command: the headline number.
static FRAMES_WITH_CALLBACK: AtomicU64 = AtomicU64::new(0);
/// Frames containing a command kind we do not recognise at all.
static FRAMES_WITH_UNKNOWN: AtomicU64 = AtomicU64::new(0);
/// Total commands seen per kind, 0..=15.
static BY_KIND: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];
static PEAK_COMMANDS: AtomicU32 = AtomicU32::new(0);
static REPORTED: AtomicBool = AtomicBool::new(false);

/// Report every this many frames: often enough to see it inside one match,
/// rare enough not to flood the log.
const REPORT_EVERY: u64 = 600;

fn enabled() -> bool {
    match ENABLED.load(Ordering::Relaxed) {
        0 => {
            let on = crate::log::wormforge_dir().join("queue_census.txt").exists();
            ENABLED.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            if on {
                log_line("queue census enabled; counting render-queue command kinds");
            }
            on
        }
        v => v == 2,
    }
}

pub fn observe(q: &crate::render_queue::Queue) {
    if !enabled() {
        return;
    }
    let mut callback = false;
    let mut unknown = false;
    for command in &q.commands {
        let kind = command.kind;
        if kind == 3 {
            callback = true;
        }
        if kind > 14 {
            unknown = true;
        }
        BY_KIND[(kind as usize).min(15)].fetch_add(1, Ordering::Relaxed);
    }

    let count = q.commands.len() as u32;
    PEAK_COMMANDS.fetch_max(count, Ordering::Relaxed);
    if callback {
        FRAMES_WITH_CALLBACK.fetch_add(1, Ordering::Relaxed);
    }
    if unknown {
        FRAMES_WITH_UNKNOWN.fetch_add(1, Ordering::Relaxed);
    }

    let frames = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    if frames % REPORT_EVERY == 0 {
        report(frames);
    }
}

fn name(kind: usize) -> &'static str {
    match kind {
        0 => "fill_rect",
        1 => "bitmap_global",
        2 => "textbox_local",
        3 => "VIA_CALLBACK",
        4 => "sprite_global",
        5 => "sprite_local",
        6 => "sprite_offset",
        7 => "polyline",
        8 => "line_strip",
        9 => "polygon",
        10 => "pixel_strip",
        11 => "crosshair",
        12 => "outlined_pixel",
        13 => "tiled_bitmap",
        14 => "tiled_terrain",
        _ => "UNKNOWN",
    }
}

fn report(frames: u64) {
    let callback = FRAMES_WITH_CALLBACK.load(Ordering::Relaxed);
    let unknown = FRAMES_WITH_UNKNOWN.load(Ordering::Relaxed);
    let peak = PEAK_COMMANDS.load(Ordering::Relaxed);
    let pct = |n: u64| 100.0 * n as f64 / frames as f64;

    log_line(&format!(
        "queue census: {frames} frames, peak {peak}/2048 commands \
         ({:.1}% of arena)",
        100.0 * peak as f64 / 2048.0
    ));
    // The number the renderer design turns on.
    log_line(&format!(
        "queue census: frames containing a kind-3 callback: {callback} ({:.2}%) \
         -- these are the frames truecolor currently abandons",
        pct(callback)
    ));
    if unknown > 0 {
        log_line(&format!(
            "queue census: frames containing an UNKNOWN command kind: \
             {unknown} ({:.2}%)",
            pct(unknown)
        ));
    }
    for kind in 0..16 {
        let total = BY_KIND[kind].load(Ordering::Relaxed);
        if total > 0 {
            log_line(&format!(
                "queue census:   kind {kind:>2} {:<14} {total:>8} commands \
                 ({:.1}/frame)",
                name(kind),
                total as f64 / frames as f64
            ));
        }
    }
    REPORTED.store(true, Ordering::Relaxed);
}

/// Flush a final report at match end, so a short session still yields numbers.
pub fn finish() {
    if !enabled() {
        return;
    }
    let frames = FRAMES.load(Ordering::Relaxed);
    if frames > 0 {
        log_line("queue census: final tally");
        report(frames);
    }
}
