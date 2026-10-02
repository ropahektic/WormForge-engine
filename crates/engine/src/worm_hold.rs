//! Presentation-only sheets held over WA's empty-handed worm poses.
//!
//! WA owns the worm state. The 0x67..=0x69 aim states show the hold;
//! 0x66 walking drops it immediately. No Lua actors or lockstep writes are
//! involved. A shot may keep the hold through WA's 0x6A/0x6B fire poses.

use crate::{gpu, log_error, sprite, world};
use game::{
    entity::read_i32,
    offsets::{
        CLASS_WORM, ENT_POS_X, ENT_POS_Y, WORM_AIM_ANGLE, WORM_FACING, WORM_SELECTED_WEAPON,
        WORM_STATE,
    },
};
use std::{f32::consts::PI, sync::Mutex};
use weapons::HoldSpec;

const DEPTH: u32 = 0x10000;

#[derive(Clone)]
struct ResolvedHold {
    params: HoldSpec,
    sheet: u32,
    hand: Option<u32>,
    flash: Option<u32>,
}

#[derive(Clone)]
struct Held {
    worm: u32,
    slot: usize,
    counter: u8,
    phase: f32,
    aim: f32,
    firing: bool,
    shot_out: bool,
    flash_left: u32,
    waiting: bool,
    putting_away: bool,
}

impl Held {
    fn new(worm: u32, slot: usize, aim: f32) -> Self {
        Self {
            worm,
            slot,
            counter: 0,
            phase: 0.0,
            aim,
            firing: false,
            shot_out: false,
            flash_left: 0,
            waiting: false,
            putting_away: false,
        }
    }
}

static CLAIMS: Mutex<Vec<(usize, ResolvedHold)>> = Mutex::new(Vec::new());
static HELD: Mutex<Vec<Held>> = Mutex::new(Vec::new());

pub fn clear() {
    if let Ok(mut claims) = CLAIMS.lock() {
        claims.clear();
    }
    if let Ok(mut held) = HELD.lock() {
        held.clear();
    }
}

pub fn claim(slot: usize, mod_id: &str, params: &HoldSpec) {
    let lookup = |path: &str| {
        sprite::lookup_key(&gfx::sprite_key(mod_id, path))
            .map(|sheet| sheet.id as u32)
            .filter(|id| sprite::is_gpu_sheet(*id))
    };
    let Some(sheet) = lookup(&params.sheet) else {
        log_error(&format!(
            "hold slot={slot}: missing GPU sheet {}",
            params.sheet
        ));
        return;
    };
    let hand = params.hand.as_deref().and_then(lookup);
    let flash = params.flash.as_ref().and_then(|f| lookup(&f.sheet));
    if params.hand.is_some() && hand.is_none() || params.flash.is_some() && flash.is_none() {
        log_error(&format!(
            "hold slot={slot}: missing hand or flash GPU sheet"
        ));
        return;
    }
    if let Ok(mut claims) = CLAIMS.lock() {
        claims.retain(|(s, _)| *s != slot);
        claims.push((
            slot,
            ResolvedHold {
                params: params.clone(),
                sheet,
                hand,
                flash,
            },
        ));
    }
}

fn aiming(state: i32) -> bool {
    matches!(state, 0x67..=0x69)
}
fn firing(state: i32) -> bool {
    matches!(state, 0x6A | 0x6B)
}

fn next_phase(phase: f32, speed: f32) -> f32 {
    if phase > 0.99 {
        0.0
    } else {
        phase + speed * 0.03
    }
}

fn grow(counter: u8) -> f32 {
    let mut size = 0.0;
    for c in 0..=counter.min(20) {
        size = if c >= 20 {
            1.0
        } else if c > 17 {
            c as f32 / 20.0
        } else if c > 9 {
            size - 0.025
        } else {
            2.7 * c as f32 / 20.0
        };
    }
    size
}

/// The weapon-use message arrives before WA's handler. Remember it so a
/// firing pose can retain its held art and light a muzzle flash exactly once.
pub fn on_fire(worm: u32) {
    if let Ok(mut held) = HELD.lock() {
        if let Some(h) = held.iter_mut().find(|h| h.worm == worm) {
            h.firing = true;
        }
    }
}

/// Called once per worm game frame, after WA has advanced its state.
pub fn tick(worm: u32) {
    if worm == 0 {
        return;
    }
    let ptr = worm as *mut u8;
    let (state, selected, aim) = unsafe {
        (
            read_i32(ptr, WORM_STATE),
            read_i32(ptr, WORM_SELECTED_WEAPON) as usize,
            (read_i32(ptr, WORM_AIM_ANGLE) as u32 & 0xffff) as f32 / 65535.0,
        )
    };
    let Ok(claims) = CLAIMS.lock() else {
        return;
    };
    let selected_hold = claims.iter().find(|(slot, _)| *slot == selected);
    let Ok(mut held) = HELD.lock() else {
        return;
    };
    let at = held.iter().position(|h| h.worm == worm);

    // Walking has its own WA body pose and never shows a pack weapon.
    if state == 0x66 {
        if let Some(i) = at {
            held.remove(i);
        }
        return;
    }
    if aiming(state) {
        if let Some((_, config)) = selected_hold {
            let h = match at {
                Some(i) if held[i].slot == selected && !held[i].putting_away => &mut held[i],
                Some(i) => {
                    held[i] = Held::new(worm, selected, aim);
                    &mut held[i]
                }
                None => {
                    held.push(Held::new(worm, selected, aim));
                    held.last_mut().unwrap()
                }
            };
            if h.shot_out {
                h.firing = false;
                h.shot_out = false;
            }
            h.waiting = false;
            h.aim = aim;
            h.counter = (h.counter + 1).min(20);
            if config.params.frames > 1 {
                h.phase = next_phase(h.phase, config.params.anim_speed);
            }
        } else if let Some(i) = at {
            held.remove(i);
        }
        return;
    }
    let Some(i) = at else {
        return;
    };
    let Some((_, config)) = claims.iter().find(|(slot, _)| *slot == held[i].slot) else {
        held.remove(i);
        return;
    };
    let h = &mut held[i];
    if firing(state) && h.firing && h.slot == selected && config.params.keep_while_firing {
        if !h.shot_out {
            // The first firing tick also counts as a lit frame.
            h.flash_left = config
                .params
                .flash
                .as_ref()
                .map_or(0, |f| f.frames.saturating_add(1));
            h.shot_out = true;
        }
        h.waiting = false;
        if config.params.frames > 1 {
            h.phase = next_phase(h.phase, config.params.anim_speed);
        }
        h.flash_left = h.flash_left.saturating_sub(1);
        return;
    }
    if state == 0x65 && selected != h.slot && !config.params.thrown {
        if !h.putting_away {
            h.counter = 20;
            h.putting_away = true;
        } else if h.counter == 0 {
            held.remove(i);
            return;
        } else {
            h.counter -= 1;
        }
        if config.params.frames > 1 {
            h.phase = next_phase(h.phase, config.params.anim_speed);
        }
        return;
    }
    if firing(state) && !config.params.thrown {
        h.waiting = true;
        return;
    }
    held.remove(i);
}

fn visible(h: &Held, state: i32, selected: usize, config: &ResolvedHold) -> bool {
    if state == 0x66 {
        return false;
    }
    if aiming(state) {
        return selected == h.slot && !h.waiting && !h.putting_away;
    }
    if firing(state) {
        return selected == h.slot
            && h.firing
            && h.shot_out
            && config.params.keep_while_firing
            && !h.waiting;
    }
    state == 0x65 && h.putting_away && h.counter > 0
}

fn place(
    x: i32,
    y: i32,
    facing: i32,
    sheet: u32,
    dx: f32,
    dy: f32,
    radians: f32,
    scale: f32,
    frame: u32,
    tint: u32,
    depth: u32,
) {
    let px = (facing as f32 * dx * 65536.0 + 0.5).floor() as i32;
    let py = (dy * 65536.0 + 0.5).floor() as i32;
    gpu::enqueue_hold(
        x.saturating_add(px),
        y.saturating_add(py),
        sheet,
        frame,
        facing,
        (radians * facing as f32).to_degrees().rem_euclid(360.0),
        scale,
        tint,
        depth,
    );
}

/// Queue one frame of hold art. Direct state checks make walking hide the
/// weapon even if WA enters 0x66 between FrameFinish and this render.
pub fn draw_holds() {
    if !gpu::is_active() {
        return;
    }
    let live = world::list_kind(CLASS_WORM, 0);
    let Ok(claims) = CLAIMS.lock() else {
        return;
    };
    let Ok(held) = HELD.lock() else {
        return;
    };
    for h in held.iter() {
        if !live.contains(&h.worm)
            || crate::physics::worm_hidden(h.worm)
            || crate::physics::body_hidden(h.worm)
        {
            continue;
        }
        let ptr = h.worm as *mut u8;
        let (state, selected, x, y, facing, live_aim) = unsafe {
            (
                read_i32(ptr, WORM_STATE),
                read_i32(ptr, WORM_SELECTED_WEAPON) as usize,
                read_i32(ptr, ENT_POS_X),
                read_i32(ptr, ENT_POS_Y),
                if read_i32(ptr, WORM_FACING) < 0 {
                    -1
                } else {
                    1
                },
                (read_i32(ptr, WORM_AIM_ANGLE) as u32 & 0xffff) as f32 / 65535.0,
            )
        };
        let Some((_, config)) = claims.iter().find(|(slot, _)| *slot == h.slot) else {
            continue;
        };
        if !visible(h, state, selected, config) {
            continue;
        }
        let p = &config.params;
        let aim = if h.putting_away {
            h.aim
        } else if aiming(state) {
            live_aim
        } else {
            h.aim
        };
        let a = if p.static_angles.is_some() { 0.5 } else { aim };
        let rot = (1.0 - a) * PI;
        let rot_w = rot + PI * p.weap_radial_rotation;
        let rot_h = rot - PI * p.hand_radial_rotation;
        // Lua's old hold placed counter 0 before advancing it on the first
        // aim frame. Keep that first invisible grow-in frame.
        let size = grow(if aiming(state) {
            h.counter.saturating_sub(1)
        } else {
            h.counter
        });
        let frame = ((h.phase * p.frames as f32).floor() as u32).min(p.frames - 1);
        place(
            x,
            y,
            facing,
            config.sheet,
            rot_w.sin() * p.weap_radius,
            -rot_w.cos() * p.weap_radius,
            p.static_angles
                .map_or(PI - PI * aim - PI * (1.0 - p.weap_rotation), |s| s.0 * PI),
            size,
            frame,
            0xffff_ffff,
            DEPTH,
        );
        if let Some(hand) = config.hand {
            place(
                x,
                y,
                facing,
                hand,
                rot_h.sin() * p.hand_radius,
                -rot_h.cos() * p.hand_radius,
                p.static_angles
                    .map_or(rot + PI * p.hand_rotation, |s| s.1 * PI),
                size * p.hand_scale,
                0,
                0xffff_ffff,
                DEPTH,
            );
        }
        if let (Some(f), Some(sheet)) = (&p.flash, config.flash) {
            if h.flash_left > 0 {
                place(
                    x,
                    y,
                    facing,
                    sheet,
                    rot.sin() * f.out - rot.cos() * f.side,
                    -rot.cos() * f.out - rot.sin() * f.side,
                    rot - PI / 2.0,
                    f.scale,
                    1,
                    0x00ff_ffff,
                    DEPTH - 0x100,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn held() -> Held {
        Held::new(1, 12, 0.5)
    }
    fn config() -> ResolvedHold {
        ResolvedHold {
            params: HoldSpec {
                sheet: "rifle".into(),
                hand: None,
                hand_radius: 1.0,
                hand_rotation: 1.0,
                hand_radial_rotation: 1.0,
                hand_scale: 1.0,
                weap_radius: 1.0,
                weap_rotation: 1.0,
                weap_radial_rotation: 1.0,
                static_angles: None,
                anim_speed: 1.0,
                frames: 1,
                keep_while_firing: true,
                thrown: false,
                flash: None,
            },
            sheet: 1,
            hand: None,
            flash: None,
        }
    }
    #[test]
    fn walk_hides_hold_and_aim_restores_it() {
        let h = held();
        let p = config();
        assert!(visible(&h, 0x67, 12, &p));
        assert!(visible(&h, 0x69, 12, &p));
        assert!(!visible(&h, 0x66, 12, &p));
        assert!(!visible(&h, 0x73, 12, &p));
        assert!(!visible(&h, 0x67, 1, &p));
    }
    #[test]
    fn firing_requires_its_actual_fire_message() {
        let mut h = held();
        let p = config();
        assert!(!visible(&h, 0x6a, 12, &p));
        h.firing = true;
        h.shot_out = true;
        assert!(visible(&h, 0x6a, 12, &p));
        assert!(!visible(&h, 0x66, 12, &p));
    }

    #[test]
    fn live_walk_transition_discards_hold_then_aim_creates_a_fresh_one() {
        use game::entity::write_i32;
        clear();
        CLAIMS.lock().unwrap().push((12, config()));
        let mut memory = [0u8; 0x400];
        let worm = memory.as_mut_ptr() as u32;
        unsafe {
            write_i32(memory.as_mut_ptr(), WORM_SELECTED_WEAPON, 12);
            write_i32(memory.as_mut_ptr(), WORM_AIM_ANGLE, 0x7fff);
            write_i32(memory.as_mut_ptr(), WORM_STATE, 0x67);
        }
        tick(worm);
        assert_eq!(
            HELD.lock()
                .unwrap()
                .iter()
                .find(|h| h.worm == worm)
                .unwrap()
                .slot,
            12
        );
        on_fire(worm);
        unsafe {
            write_i32(memory.as_mut_ptr(), WORM_STATE, 0x6a);
        }
        tick(worm);
        assert!(
            HELD.lock()
                .unwrap()
                .iter()
                .find(|h| h.worm == worm)
                .unwrap()
                .shot_out
        );
        unsafe {
            write_i32(memory.as_mut_ptr(), WORM_STATE, 0x66);
        }
        tick(worm);
        assert!(!HELD.lock().unwrap().iter().any(|h| h.worm == worm));
        unsafe {
            write_i32(memory.as_mut_ptr(), WORM_STATE, 0x67);
        }
        tick(worm);
        let held = HELD.lock().unwrap();
        let restored = held.iter().find(|h| h.worm == worm).unwrap();
        assert!(!restored.firing && !restored.shot_out);
        drop(held);
        clear();
    }
}
