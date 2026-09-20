//! WA's render queue: the depth every world draw is ordered by.
//!
//! Layout from wkFOW's RenderQueue.h, itself taken from openwa's
//! render/queue.rs. The queue lives at GameWorld+0x524. Each entry is a dword
//! array: cmd[0] is the command type, cmd[1] is its LAYER. The dispatcher
//! sorts ascending by layer and walks from the highest index down, so
//!
//!     HIGH layer == drawn FIRST == furthest back
//!     LOW  layer == drawn LAST  == on top
//!
//! Putting the sentry behind terrain but in front of the soil therefore means
//! giving its sprite command a layer between the two:
//!
//!     terrain layer  <  sentry layer  <  soil layer
//!
//! This module only reads, to establish what those values actually are.

use crate::log_line;
use game::offsets::GAME_WORLD_RENDER_QUEUE;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

const OFF_ENTRY_COUNT: usize = 0x10004;
const OFF_ENTRY_PTRS: usize = 0x10008;
const MAX_ENTRIES: usize = 0x800;

pub const CMD_SPRITE_GLOBAL: u32 = 0x4;
pub const CMD_TILED_BITMAP: u32 = 0xD;
pub const CMD_TILED_TERRAIN: u32 = 0xE;

static DUMPED: AtomicBool = AtomicBool::new(false);
/// Every (type, layer, sprite) combination reported so far.
///
/// A one-shot dump kept catching the scene before the turret was on screen --
/// first before it was placed, then while the worm was still equipping. This
/// keeps watching and reports each new combination as it appears, so whatever
/// frame the turret finally draws in is the frame it gets logged.
static SEEN: Mutex<Vec<(u32, u32, u32)>> = Mutex::new(Vec::new());
const REPORT_CAP: usize = 120;
/// sprite id -> render-queue depth requested by its pack.
static DEPTHS: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());

pub fn set_depth(sprite: u32, depth: u32) {
    if let Ok(mut g) = DEPTHS.lock() {
        g.retain(|(id, _)| *id != sprite);
        g.push((sprite, depth));
    }
}

pub fn clear_depths() {
    if let Ok(mut g) = DEPTHS.lock() {
        g.clear();
    }
    if let Ok(mut g) = SEEN.lock() {
        g.clear();
    }
}

/// Re-depth our sprites in the queue before it is dispatched.
///
/// The dispatcher sorts by cmd[1], so rewriting it moves the draw without
/// touching the sprite, its palette or its position. Depths are registered per
/// sprite id, so only the sheets a pack asked for move: the turret goes behind
/// the landscape while the worm holding it stays in front.
pub fn apply_depths(world: usize) {
    if world == 0 {
        return;
    }
    let Ok(depths) = DEPTHS.lock() else {
        return;
    };
    if depths.is_empty() {
        return;
    }
    let Some((base, count)) = queue_base(world) else {
        return;
    };
    let ptrs = (base + OFF_ENTRY_PTRS) as *const usize;
    for i in 0..count {
        let entry = unsafe { *ptrs.add(i) };
        if entry == 0 {
            continue;
        }
        let kind = unsafe { *(entry as *const u32) };
        if kind != CMD_SPRITE_GLOBAL && kind != 0x5 && kind != 0x6 {
            continue;
        }
        let sprite = (unsafe { *((entry + 16) as *const u32) }) & 0xffff;
        if let Some((_, depth)) = depths.iter().find(|(id, _)| *id == sprite) {
            unsafe {
                *((entry + 4) as *mut u32) = *depth;
            }
        }
    }
}

fn describe(kind: u32) -> &'static str {
    match kind {
        0x0 => "fill_rect",
        0x1 => "bitmap_global",
        0x2 => "textbox_local",
        0x3 => "via_callback",
        0x4 => "sprite_global",
        0x5 => "sprite_local",
        0x6 => "sprite_offset",
        0x7 => "polyline",
        0x8 => "line_strip",
        0x9 => "polygon",
        0xA => "pixel_strip",
        0xB => "crosshair",
        0xC => "outlined_pixel",
        0xD => "tiled_bitmap",
        0xE => "tiled_terrain",
        _ => "?",
    }
}

/// Resolve the queue's base and entry count.
///
/// wkFOW documents the queue at GameWorld+0x524 without saying whether that is
/// the structure or a pointer to it; in practice it is a pointer. Inline is
/// tried first and a bad count rejects the walk rather than following the
/// queue off its arena, which can be torn mid-rebuild.
fn queue_base(world: usize) -> Option<(usize, usize)> {
    let inline = world + GAME_WORLD_RENDER_QUEUE;
    let count = unsafe { *((inline + OFF_ENTRY_COUNT) as *const u32) } as usize;
    if count > 0 && count <= MAX_ENTRIES {
        return Some((inline, count));
    }
    let indirect = unsafe { *(inline as *const usize) };
    if indirect == 0 {
        return None;
    }
    let count = unsafe { *((indirect + OFF_ENTRY_COUNT) as *const u32) } as usize;
    (count > 0 && count <= MAX_ENTRIES).then_some((indirect, count))
}

/// Custom sprite slots start here; anything at or above is one of ours.
const CUSTOM_SPRITE_MIN: u32 = 800;

/// Log every distinct (command type, layer) in the queue, once per match.
///
/// Held back until one of our own sprites is actually in the queue: dumping on
/// the first rendered frame caught the scene before the turret existed, which
/// showed the terrain and soil depths but not the one we need to change.
///
/// Read-only and bounded: a bad count or a null entry stops the walk rather
/// than following the queue off its arena, which can be torn mid-rebuild
/// during the frontend transition.
pub fn dump_once(world: usize) {
    if world == 0 {
        return;
    }
    // wkFOW documents the queue as living at GameWorld+0x524. Whether that is
    // the structure itself or a pointer to it is not stated, so try inline
    // first and fall back to dereferencing, reporting which one read sanely.
    let inline = world + GAME_WORLD_RENDER_QUEUE;
    let mut base = inline;
    let mut count = unsafe { *((base + OFF_ENTRY_COUNT) as *const u32) } as usize;
    let mut how = "inline";
    if count == 0 || count > MAX_ENTRIES {
        let indirect = unsafe { *(inline as *const usize) };
        if indirect != 0 {
            let c = unsafe { *((indirect + OFF_ENTRY_COUNT) as *const u32) } as usize;
            if c > 0 && c <= MAX_ENTRIES {
                base = indirect;
                count = c;
                how = "pointer";
            }
        }
    }
    if count == 0 || count > MAX_ENTRIES {
            DUMPED.store(true, Ordering::SeqCst);
        log_line(&format!(
            "render queue: no sane entry count (inline {count}); base {inline:#x}"
        ));
        return;
    }
    let _ = how;
    let ptrs = (base + OFF_ENTRY_PTRS) as *const usize;
    let mut seen: Vec<(u32, u32, u32)> = Vec::new();
    for i in 0..count {
        let entry = unsafe { *ptrs.add(i) };
        if entry == 0 {
            continue;
        }
        let kind = unsafe { *(entry as *const u32) };
        let layer = unsafe { *((entry + 4) as *const u32) };
        // Sprite index sits in the low 16 bits of the op word.
        let sprite = if kind == CMD_SPRITE_GLOBAL || kind == 0x5 || kind == 0x6 {
            (unsafe { *((entry + 16) as *const u32) }) & 0xffff
        } else {
            0
        };
        if !seen.iter().any(|(k, l, s)| *k == kind && *l == layer && *s == sprite) {
            seen.push((kind, layer, sprite));
        }
    }
    let Ok(mut known) = SEEN.lock() else {
        return;
    };
    if known.len() >= REPORT_CAP {
        return;
    }
    seen.sort_by_key(|(_, l, _)| std::cmp::Reverse(*l));
    for (kind, layer, sprite) in seen {
        if known.iter().any(|e| *e == (kind, layer, sprite)) {
            continue;
        }
        known.push((kind, layer, sprite));
        if sprite >= CUSTOM_SPRITE_MIN {
            log_line(&format!(
                "queue OURS layer {layer:#x} {} sprite {sprite} ({count} entries)",
                describe(kind)
            ));
        } else if sprite != 0 {
            log_line(&format!("queue layer {layer:#x} {} sprite {sprite}", describe(kind)));
        } else {
            log_line(&format!("queue layer {layer:#x} {}", describe(kind)));
        }
        if known.len() >= REPORT_CAP {
            log_line("queue: report cap reached");
            break;
        }
    }
}

pub fn reset() {
    DUMPED.store(false, Ordering::SeqCst);
}
