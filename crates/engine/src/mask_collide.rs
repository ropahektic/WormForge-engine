//! First-frame opaque GIF/PNG masks vs land, worms, weapons, and other actors.
//!
//! `mask_worms` / `mask_weapons` stamp the silhouette into the landscape walk
//! bitgrid so stock worm locomotion and hitscan see it as dirt. Custom probes
//! handle actor↔land (mask_land) and actor↔actor (mask_actors).

use crate::sprite;
use game::entity::{read_i32, write_i32};
use game::offsets::*;
use gfx::OpaqueMask;
use std::sync::Mutex;

/// Pixels we forced solid; `was_solid` is the bit before our stamp.
struct StampPixel {
    x: i32,
    y: i32,
    was_solid: bool,
}

static STAMPS: Mutex<Vec<StampPixel>> = Mutex::new(Vec::new());

/// Call at the start of FRAME_FINISH (before stock physics) so worms / hitscan
/// / stock missile bounce see the silhouette as landscape.
pub unsafe fn prepare_frame(_from: *mut u8) {
    unstamp_all();
    let handles = crate::actor::live_handles();
    for h in handles {
        let this = h as *mut u8;
        if this.is_null() || !is_actor(this) {
            continue;
        }
        if read_i32(this, ACTOR_DEAD) != 0 {
            continue;
        }
        let flags = read_i32(this, ACTOR_FLAGS);
        if (flags & ACTOR_FLAG_MASK) == 0 {
            continue;
        }
        // Worms + weapons share the walk grid (hitscan + walk + CM bounce).
        if (flags & (ACTOR_FLAG_MASK_WORMS | ACTOR_FLAG_MASK_WEAPONS)) == 0 {
            continue;
        }
        let sid = read_i32(this, ACTOR_SPRITE);
        let Some(mask) = sprite::mask_for_id(sid) else {
            continue;
        };
        let Some(geom) = sprite_geom(this) else {
            continue;
        };
        stamp_mask(&mask, &geom);
    }
}

/// After stock physics: land push for masked actors, pairwise actor separation.
pub unsafe fn resolve_all(_from: *mut u8) {
    let handles = crate::actor::live_handles();
    if handles.is_empty() {
        return;
    }
    let mut masked: Vec<Masked> = Vec::new();
    for h in handles {
        let this = h as *mut u8;
        if this.is_null() || !is_actor(this) {
            continue;
        }
        if read_i32(this, ACTOR_DEAD) != 0 {
            continue;
        }
        let flags = read_i32(this, ACTOR_FLAGS);
        if (flags & ACTOR_FLAG_MASK) == 0 {
            continue;
        }
        let sid = read_i32(this, ACTOR_SPRITE);
        let Some(mask) = sprite::mask_for_id(sid) else {
            continue;
        };
        let Some(geom) = sprite_geom(this) else {
            continue;
        };
        masked.push(Masked {
            ptr: this,
            flags,
            mask,
            geom,
        });
    }
    if masked.is_empty() {
        return;
    }

    for m in &masked {
        if (m.flags & ACTOR_FLAG_MASK_LAND) != 0 {
            push_actor_out_of_land(m);
        }
    }

    for i in 0..masked.len() {
        if (masked[i].flags & ACTOR_FLAG_MASK_ACTORS) == 0 {
            continue;
        }
        for j in (i + 1)..masked.len() {
            if (masked[j].flags & ACTOR_FLAG_MASK_ACTORS) == 0 {
                continue;
            }
            separate_actors(&masked[i], &masked[j]);
        }
    }
}

/// Drop every stamp (round end / unload). Restores prior bits.
pub fn clear() {
    unstamp_all();
}

fn unstamp_all() {
    let Ok(mut g) = STAMPS.lock() else {
        return;
    };
    // Restore in reverse so overlapping stamps unwind correctly.
    while let Some(p) = g.pop() {
        let _ = crate::world::set_collision_px(p.x, p.y, p.was_solid);
    }
}

fn stamp_mask(mask: &OpaqueMask, geom: &SpriteGeom) {
    let Ok(mut g) = STAMPS.lock() else {
        return;
    };
    for ly in 0..geom.h {
        for lx in 0..geom.w {
            let sample_x = if geom.mirror {
                geom.w - 1 - lx
            } else {
                lx
            };
            if !mask.test(sample_x, ly) {
                continue;
            }
            let wx = geom.tl_x + lx;
            let wy = geom.tl_y + ly;
            let Some(was) = crate::world::set_collision_px(wx, wy, true) else {
                continue;
            };
            // Only record pixels we flipped from empty → solid so restore is exact.
            if !was {
                g.push(StampPixel {
                    x: wx,
                    y: wy,
                    was_solid: false,
                });
            }
        }
    }
}

struct Masked {
    ptr: *mut u8,
    flags: i32,
    mask: OpaqueMask,
    geom: SpriteGeom,
}

#[derive(Clone, Copy)]
struct SpriteGeom {
    tl_x: i32,
    tl_y: i32,
    w: i32,
    h: i32,
    mirror: bool,
    cx: i32,
    cy: i32,
}

fn is_actor(this: *mut u8) -> bool {
    if this.is_null() {
        return false;
    }
    unsafe { read_i32(this, ACTOR_MAGIC) as u32 == LUA_ACTOR_MAGIC }
}

unsafe fn sprite_geom(this: *mut u8) -> Option<SpriteGeom> {
    let x = read_i32(this, ENT_POS_X) >> 16;
    let y = read_i32(this, ENT_POS_Y) >> 16;
    let w = read_i32(this, ACTOR_SPR_W);
    let h = read_i32(this, ACTOR_SPR_H);
    if w <= 0 || h <= 0 {
        return None;
    }
    let feet_x0 = read_i32(this, ACTOR_FEET_X0);
    let feet_x1 = read_i32(this, ACTOR_FEET_X1);
    let feet_y = read_i32(this, ACTOR_FEET_Y);
    let origin = read_i32(this, ACTOR_ORIGIN);
    let flags = read_i32(this, ACTOR_FLAGS);
    // Pin the sheet from the same anchors blit uses — for bottom+feet, entity
    // sits on the feet row, so top-left is (feet_center - offsets), not a
    // second guess at DrawSpriteLocal centering (that shifted masks down and
    // left the ears unsolid).
    let (tl_x, tl_y, cx, cy) = if origin == ACTOR_ORIGIN_BOTTOM && feet_x1 >= feet_x0 && feet_y >= 0 {
        let fcx = (feet_x0 + feet_x1) / 2;
        let tl_x = x - fcx;
        let tl_y = y - feet_y;
        (tl_x, tl_y, tl_x + w / 2, tl_y + h / 2)
    } else if origin == ACTOR_ORIGIN_CENTER {
        (x - w / 2, y - h / 2, x, y)
    } else if origin == ACTOR_ORIGIN_BOTTOM {
        (x - w / 2, y - h, x, y - h / 2)
    } else {
        (x - w / 2, y - h / 2, x, y)
    };
    Some(SpriteGeom {
        tl_x,
        tl_y,
        w,
        h,
        mirror: (flags & ACTOR_FLAG_MIRROR_X) != 0,
        cx,
        cy,
    })
}

fn world_in_mask(m: &Masked, wx: i32, wy: i32) -> bool {
    let mut lx = wx - m.geom.tl_x;
    let ly = wy - m.geom.tl_y;
    if m.geom.mirror {
        lx = m.geom.w - 1 - lx;
    }
    m.mask.test(lx, ly)
}

/// Land support: opaque bottom-contour pixels with solid dirt one pixel below.
pub unsafe fn mask_land_support(this: *mut u8) -> bool {
    let flags = read_i32(this, ACTOR_FLAGS);
    if (flags & ACTOR_FLAG_MASK) == 0 || (flags & ACTOR_FLAG_MASK_LAND) == 0 {
        return false;
    }
    let sid = read_i32(this, ACTOR_SPRITE);
    let Some(mask) = sprite::mask_for_id(sid) else {
        return false;
    };
    let Some(geom) = sprite_geom(this) else {
        return false;
    };
    let m = Masked {
        ptr: this,
        flags,
        mask,
        geom,
    };
    for ly in 0..m.geom.h {
        for lx in 0..m.geom.w {
            let sample_x = if m.geom.mirror {
                m.geom.w - 1 - lx
            } else {
                lx
            };
            if !m.mask.test(sample_x, ly) {
                continue;
            }
            let below_clear = ly + 1 >= m.geom.h || !m.mask.test(sample_x, ly + 1);
            if !below_clear {
                continue;
            }
            let wx = m.geom.tl_x + lx;
            let wy = m.geom.tl_y + ly + 1;
            if crate::world::solid_fixed(wx << 16, wy << 16) {
                return true;
            }
        }
    }
    false
}

unsafe fn push_actor_out_of_land(m: &Masked) {
    let mut embedded = false;
    'scan: for ly in 0..m.geom.h {
        for lx in 0..m.geom.w {
            let sample_x = if m.geom.mirror {
                m.geom.w - 1 - lx
            } else {
                lx
            };
            if !m.mask.test(sample_x, ly) {
                continue;
            }
            let wx = m.geom.tl_x + lx;
            let wy = m.geom.tl_y + ly;
            // Our own stamp counts as solid — skip those so the actor does not
            // float itself out of its silhouette.
            if crate::world::solid_fixed(wx << 16, wy << 16) {
                // If this pixel is ours, ignore.
                if world_in_mask(m, wx, wy) {
                    continue;
                }
                embedded = true;
                break 'scan;
            }
        }
    }
    if !embedded {
        // Also: contour resting on real dirt (not our stamp).
        return;
    }
    let mut y = read_i32(m.ptr, ENT_POS_Y);
    for _ in 0..48 {
        y = y.wrapping_sub(0x10000);
        write_i32(m.ptr, ENT_POS_Y, y);
        let Some(geom) = sprite_geom(m.ptr) else {
            return;
        };
        let mut still = false;
        for ly in 0..geom.h {
            for lx in 0..geom.w {
                let sample_x = if geom.mirror { geom.w - 1 - lx } else { lx };
                if !m.mask.test(sample_x, ly) {
                    continue;
                }
                let wx = geom.tl_x + lx;
                let wy = geom.tl_y + ly;
                if world_in_mask(m, wx, wy) {
                    continue;
                }
                if crate::world::solid_fixed(wx << 16, wy << 16) {
                    still = true;
                    break;
                }
            }
            if still {
                break;
            }
        }
        if !still {
            write_i32(m.ptr, ENT_SPEED_Y, 0);
            write_i32(m.ptr, ACTOR_CONTACT, 1);
            return;
        }
    }
}

unsafe fn separate_actors(a: &Masked, b: &Masked) {
    let ax0 = a.geom.tl_x;
    let ay0 = a.geom.tl_y;
    let ax1 = ax0 + a.geom.w;
    let ay1 = ay0 + a.geom.h;
    let bx0 = b.geom.tl_x;
    let by0 = b.geom.tl_y;
    let bx1 = bx0 + b.geom.w;
    let by1 = by0 + b.geom.h;
    if ax1 < bx0 || bx1 < ax0 || ay1 < by0 || by1 < ay0 {
        return;
    }
    let a_in_b = world_in_mask(b, a.geom.cx, a.geom.cy);
    let b_in_a = world_in_mask(a, b.geom.cx, b.geom.cy);
    if !a_in_b && !b_in_a {
        let mx = (a.geom.cx + b.geom.cx) / 2;
        let my = (a.geom.cy + b.geom.cy) / 2;
        if !world_in_mask(a, mx, my) && !world_in_mask(b, mx, my) {
            return;
        }
    }
    let dx = (b.geom.cx - a.geom.cx) as f32;
    let dy = (b.geom.cy - a.geom.cy) as f32;
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let nx = ((dx / len) * 32768.0) as i32;
    let ny = ((dy / len) * 32768.0) as i32;
    let ax = read_i32(a.ptr, ENT_POS_X).wrapping_sub(nx);
    let ay = read_i32(a.ptr, ENT_POS_Y).wrapping_sub(ny);
    let bx = read_i32(b.ptr, ENT_POS_X).wrapping_add(nx);
    let by = read_i32(b.ptr, ENT_POS_Y).wrapping_add(ny);
    write_i32(a.ptr, ENT_POS_X, ax);
    write_i32(a.ptr, ENT_POS_Y, ay);
    write_i32(b.ptr, ENT_POS_X, bx);
    write_i32(b.ptr, ENT_POS_Y, by);
}
