//! Cluster debris as particles, outside the lockstep.
//!
//! WA makes a weapon's cluster bits as real missiles (`CreateClusters`,
//! 0x5096A0), even when they can do nothing but fly and time out: Project
//! X's Pigeon Strike bursts every pigeon into 18 feathers, the Pogo Sheep
//! into 48 drops of blood. Each is a `MissileEntity` in the simulation: two
//! draws from WA's RNG to launch it, two more every frame to make it flutter,
//! and every message it gets goes through our missile hooks too. Hundreds of
//! them dropped the frame rate.
//!
//! When a cluster's pellets are inert -- no blast on their fuse or on contact,
//! nothing they can touch but crates and oil drums, no homing, digging or
//! animal behaviour, no detonation by the fire key -- WA is left to make none
//! (its pellet count is zeroed around the call) and they are drawn here
//! instead: launched with WA's own formula, moved with their own gravity,
//! wind, damping and flutter, animated as WA animates them, gone when their
//! fuse runs out. Nothing here reads WA's RNG or writes WA's state, so each
//! machine may draw them as it likes without the games drifting apart.
//!
//! The pellet parameters are the fire params' cluster half, which the missile
//! carries in its copy of the weapon data (+0x15C): `rd[n]` below is the
//! n-th dword of that half, at +0x22C, as WA's constructor copies it into a
//! pellet's active block (+0x2D4) and OpenWA numbers it (`render_data`).

use crate::rebase::rb;
use game::entity::read_i32;
use minhook::MinHook;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

static ORIG_CLUSTERS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Hook `MissileEntity__CreateClusters` (0x5096A0).
pub fn install() -> Result<(), String> {
    let target = rb(game::offsets::MISSILE_CREATE_CLUSTERS) as *mut c_void;
    let tr = unsafe {
        MinHook::create_hook(target, clusters_hook as *mut c_void)
            .map_err(|e| format!("MinHook CreateClusters: {e:?}"))?
    };
    ORIG_CLUSTERS.store(tr, Ordering::SeqCst);
    Ok(())
}

/// `CreateClusters`: the missile in ESI, the position on the stack
/// (`ret 8`). Decide first, call WA's with the same arguments, then put the
/// count back. ESI is callee-saved, so it still holds the missile after.
#[unsafe(naked)]
unsafe extern "C" fn clusters_hook() {
    core::arch::naked_asm!(
        "push dword ptr [esp+8]",
        "push dword ptr [esp+8]",
        "push esi",
        "call {before}",
        "add esp, 12",
        "push dword ptr [esp+8]",
        "push dword ptr [esp+8]",
        "mov eax, dword ptr [{orig}]",
        "call eax",
        "push esi",
        "call {after}",
        "add esp, 4",
        "ret 8",
        before = sym before_clusters_c,
        after = sym after_clusters_c,
        orig = sym ORIG_CLUSTERS,
    );
}

unsafe extern "C" fn before_clusters_c(missile: *mut u8, x: i32, y: i32) {
    unsafe { before_clusters(missile, x, y) };
}

unsafe extern "C" fn after_clusters_c(missile: *mut u8) {
    unsafe { after_clusters(missile) };
}

/// The missile's copy of the fire params' cluster half.
const PELLET_BLOCK: usize = 0x22C;
/// The cluster header, in the missile's copy of the fire params (entry
/// +0xF8.. less 0x3C, plus 0x15C): how many, how fast, the angle.
const CLUSTER_COUNT: usize = 0x218;
const CLUSTER_SPEED_PCT: usize = 0x21C;
const CLUSTER_SPEED_VAR_PCT: usize = 0x220;
const CLUSTER_ANGLE: usize = 0x224;
const CLUSTER_SPREAD: usize = 0x228;

// Pellet fields, by render_data index.
const RD_CONTACT_MASK: usize = 0x00;
const RD_FUSE_BLAST_BASE: usize = 0x02;
const RD_FUSE_BLAST_PCT: usize = 0x03;
const RD_SPRITE: usize = 0x06;
const RD_ANIM_KIND: usize = 0x07;
const RD_TRAIL_SPRITE: usize = 0x08;
const RD_TRAIL_POWER: usize = 0x09;
const RD_TRAIL_SPEED: usize = 0x0A;
const RD_DAMPING_PCT: usize = 0x0C;
const RD_WIND_PCT: usize = 0x0D;
const RD_FLUTTER_PCT: usize = 0x0E;
const RD_GRAVITY_PCT: usize = 0x0F;
const RD_FUSE_MS: usize = 0x11;
const RD_EXPLODE_ON_SPACE: usize = 0x16;
const RD_MISSILE_TYPE: usize = 0x17;
const RD_COLLISION_MASK: usize = 0x18;
const RD_CONTACT_BLAST: usize = 0x20;

/// Collision layers (`WorldEntity__Constructor`'s argument, one bit each in
/// a mask): what a pellet must not be able to touch to be debris. Crates
/// (11) and oil drums (17) are allowed: feathers brush them in WA.
const LAYER_WORM: u32 = 2;
const LAYER_WORM_STRIKE: u32 = 3;
const LAYER_CANISTER: u32 = 9;
const LAYER_MINE: u32 = 10;
const LAYER_GRAVE: u32 = 13;
const LAYER_MISSILE: u32 = 15;
const LAYER_ARROW: u32 = 16;
const BLOCKING_LAYERS: u32 = (1 << LAYER_WORM)
    | (1 << LAYER_WORM_STRIKE)
    | (1 << LAYER_CANISTER)
    | (1 << LAYER_MINE)
    | (1 << LAYER_GRAVE)
    | (1 << LAYER_MISSILE)
    | (1 << LAYER_ARROW);
/// The landscape's bit in a collision mask.
const LANDSCAPE_BIT: u32 = 0x40_0000;

/// Missile types (`rd[0x17]`) WA gives behaviour of its own: homing, the
/// animals, digging.
fn plain_missile(kind: i32) -> bool {
    !matches!(kind, 1 | 3 | 4)
}

/// WA's missiles tick 50 times a second; fuses count milliseconds.
const MS_PER_FRAME: i32 = 20;
/// Too many to draw is a bug somewhere; keep the newest.
const MAX_PARTICLES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Terrain {
    /// Flies through the landscape, as WA's debris without the landscape bit.
    Pass,
    /// Comes to rest where it meets the landscape.
    Stop,
}

/// One inert pellet's parameters, read once per burst.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pellet {
    pub sprite: i32,
    pub anim_kind: i32,
    pub damping: f32,
    pub wind: f32,
    pub gravity: f32,
    /// Largest random nudge per frame on each axis, in pixels.
    pub flutter: f32,
    pub life: i32,
    pub terrain: Terrain,
    /// Its trail: a sprite WA leaves behind it, how often (per frame, 1.0 =
    /// every frame) and how fast each plays (share of its animation a frame).
    pub trail_sprite: i32,
    pub trail_rate: f32,
    pub trail_anim: f32,
}

/// The pellet a cluster block describes, if it can only be debris.
pub fn inert_pellet(rd: &[i32]) -> Option<Pellet> {
    let at = |i: usize| rd.get(i).copied().unwrap_or(0);
    let touches = (at(RD_CONTACT_MASK) | at(RD_COLLISION_MASK)) as u32;
    let inert = at(RD_FUSE_BLAST_BASE) == 0
        && at(RD_FUSE_BLAST_PCT) == 0
        && at(RD_CONTACT_BLAST) == 0
        && at(RD_EXPLODE_ON_SPACE) == 0
        && plain_missile(at(RD_MISSILE_TYPE))
        && touches & BLOCKING_LAYERS == 0
        && at(RD_FUSE_MS) > 0;
    if !inert {
        return None;
    }
    Some(Pellet {
        sprite: at(RD_SPRITE),
        anim_kind: at(RD_ANIM_KIND),
        damping: at(RD_DAMPING_PCT) as f32 / 100.0,
        wind: at(RD_WIND_PCT) as f32 / 100.0,
        gravity: at(RD_GRAVITY_PCT) as f32 / 100.0,
        flutter: at(RD_FLUTTER_PCT) as f32 / 200.0,
        life: (at(RD_FUSE_MS) + MS_PER_FRAME - 1) / MS_PER_FRAME,
        terrain: if at(RD_COLLISION_MASK) as u32 & LANDSCAPE_BIT != 0 {
            Terrain::Stop
        } else {
            Terrain::Pass
        },
        // The missile frame (0x50BAB4): the emit accumulator grows by
        // power / 25 a frame; each puff's animation runs at
        // speed * 0x147A / 100 (16.16) a frame.
        trail_sprite: at(RD_TRAIL_SPRITE),
        trail_rate: at(RD_TRAIL_POWER) as f32 / 25.0,
        trail_anim: at(RD_TRAIL_SPEED) as f32 * 0x147A as f32 / 100.0 / 65536.0,
    })
}

/// The launch of one pellet: `CreateClusters`' formula, with draws from our
/// own generator in place of WA's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Launch {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
}

/// `CreateClusters`: the base angle is the record's, or with -1, back the way
/// the missile came (up if it fell, down if it rose, else against its
/// horizontal motion); the spread is +/- half the record's; the speed is the
/// record's less up to its variance; the pellet leaves 8 px out and 20 px up
/// along its angle. Angles in degrees, 0 straight up, clockwise.
#[allow(clippy::too_many_arguments)]
pub fn launch(
    x: f32,
    y: f32,
    parent_vx: f32,
    parent_vy: f32,
    angle: i32,
    spread: i32,
    speed_pct: i32,
    speed_var_pct: i32,
    u_angle: f32,
    u_speed: f32,
) -> Launch {
    let base = if angle >= 0 {
        angle as f32
    } else if parent_vy.abs() > parent_vx.abs() {
        if parent_vy >= 0.0 { 0.0 } else { 180.0 }
    } else if parent_vx >= 0.0 {
        270.0
    } else {
        90.0
    };
    let degrees = base + spread as f32 * (u_angle - 0.5);
    let a = degrees.to_radians();
    let speed = speed_pct as f32 / 100.0;
    let speed = speed - speed * speed_var_pct as f32 / 100.0 * (1.0 - u_speed);
    Launch {
        x: x + 8.0 * a.sin(),
        y: y - 20.0 * a.cos(),
        vx: 24.0 * speed * a.sin(),
        vy: -24.0 * speed * a.cos(),
    }
}

#[derive(Clone, Copy, Debug)]
struct Particle {
    pellet: Pellet,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    age: i32,
    resting: bool,
    /// The trail's emit accumulator: a puff each time it reaches 1.
    trail: f32,
}

/// One trail sprite: drifts where it was left and plays its animation once.
#[derive(Clone, Copy, Debug)]
struct Puff {
    sprite: i32,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    phase: f32,
    rate: f32,
}

struct State {
    particles: Vec<Particle>,
    puffs: Vec<Puff>,
    rng: u32,
}

static STATE: Mutex<State> =
    Mutex::new(State { particles: Vec::new(), puffs: Vec::new(), rng: 0x9E37_79B9 });
/// The count `before_clusters` zeroed, for `after_clusters` to put back.
static SAVED_COUNT: Mutex<Option<(usize, i32)>> = Mutex::new(None);

impl State {
    /// For looks only: never WA's generator.
    fn random(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }
}

pub fn clear() {
    if let Ok(mut s) = STATE.lock() {
        s.particles.clear();
        s.puffs.clear();
    }
}

/// Before WA's `CreateClusters(missile, x, y)`: if the pellets are debris,
/// launch them as particles and zero the count, so WA makes none but runs
/// the rest of the function as before.
pub unsafe fn before_clusters(missile: *mut u8, x: i32, y: i32) {
    if missile.is_null() {
        return;
    }
    let count = unsafe { read_i32(missile, CLUSTER_COUNT) };
    if count <= 0 {
        return;
    }
    let mut rd = [0i32; 0x2A];
    for (i, v) in rd.iter_mut().enumerate() {
        *v = unsafe { read_i32(missile, PELLET_BLOCK + 4 * i) };
    }
    let Some(pellet) = inert_pellet(&rd) else {
        return;
    };
    let fx = |v: i32| v as f32 / 65536.0;
    let parent_vx = fx(unsafe { read_i32(missile, game::offsets::ENT_SPEED_X) });
    let parent_vy = fx(unsafe { read_i32(missile, game::offsets::ENT_SPEED_Y) });
    let angle = unsafe { read_i32(missile, CLUSTER_ANGLE) };
    let spread = unsafe { read_i32(missile, CLUSTER_SPREAD) };
    let speed_pct = unsafe { read_i32(missile, CLUSTER_SPEED_PCT) };
    let speed_var = unsafe { read_i32(missile, CLUSTER_SPEED_VAR_PCT) };
    if let Ok(mut s) = STATE.lock() {
        for _ in 0..count {
            let (u_speed, u_angle) = (s.random(), s.random());
            let l = launch(
                fx(x), fx(y), parent_vx, parent_vy, angle, spread, speed_pct, speed_var,
                u_angle, u_speed,
            );
            if s.particles.len() >= MAX_PARTICLES {
                s.particles.remove(0);
            }
            s.particles.push(Particle {
                pellet,
                x: l.x,
                y: l.y,
                vx: l.vx,
                vy: l.vy,
                age: 0,
                resting: false,
                trail: 0.0,
            });
        }
    }
    unsafe { game::entity::write_i32(missile, CLUSTER_COUNT, 0) };
    if let Ok(mut g) = SAVED_COUNT.lock() {
        *g = Some((missile as usize, count));
    }
}

/// After WA's `CreateClusters`: the count back as the record has it.
pub unsafe fn after_clusters(missile: *mut u8) {
    let saved = SAVED_COUNT.lock().ok().and_then(|mut g| g.take());
    if let Some((at, count)) = saved {
        if at == missile as usize {
            unsafe { game::entity::write_i32(missile, CLUSTER_COUNT, count) };
        }
    }
}

/// One game frame: gravity, wind, damping, flutter; the fuse; the terrain.
/// Called at the end of WA's frame, so debris moves at the game's pace, but
/// it only ever touches this list.
pub fn tick(gravity: f32, wind: f32, water_y: f32, solid: impl Fn(i32, i32) -> bool) {
    let Ok(mut s) = STATE.lock() else {
        return;
    };
    let s = &mut *s;
    s.puffs.retain_mut(|p| {
        p.x += p.vx;
        p.y += p.vy;
        p.phase += p.rate;
        p.phase < 1.0
    });
    let mut i = 0;
    while i < s.particles.len() {
        let (nx, ny) = {
            let p = s.particles[i];
            if p.pellet.flutter > 0.0 && !p.resting {
                (s.random(), s.random())
            } else {
                (0.5, 0.5)
            }
        };
        let p = &mut s.particles[i];
        p.age += 1;
        if p.age >= p.pellet.life || p.y > water_y {
            s.particles.swap_remove(i);
            continue;
        }
        if !p.resting {
            p.vx = p.vx * p.pellet.damping + wind * p.pellet.wind;
            p.vy = p.vy * p.pellet.damping + gravity * p.pellet.gravity;
            p.vx += (nx - 0.5) * 2.0 * p.pellet.flutter;
            p.vy += (ny - 0.5) * 2.0 * p.pellet.flutter;
            let (x, y) = (p.x + p.vx, p.y + p.vy);
            if p.pellet.terrain == Terrain::Stop && solid(x as i32, y as i32) {
                p.resting = true;
                p.vx = 0.0;
                p.vy = 0.0;
            } else {
                p.x = x;
                p.y = y;
            }
        }
        // Its trail, above the water, as WA leaves one behind a missile:
        // each puff drifting up to half a pixel a frame each way.
        let p = s.particles[i];
        if p.pellet.trail_sprite != 0 && p.pellet.trail_rate > 0.0 && !p.resting {
            let mut acc = p.trail + p.pellet.trail_rate;
            while acc >= 1.0 {
                acc -= 1.0;
                let (a, b) = (s.random(), s.random());
                if s.puffs.len() >= MAX_PARTICLES {
                    s.puffs.remove(0);
                }
                s.puffs.push(Puff {
                    sprite: p.pellet.trail_sprite,
                    x: p.x,
                    y: p.y,
                    vx: a - 0.5,
                    vy: b - 0.5,
                    phase: 0.0,
                    rate: p.pellet.trail_anim.max(1.0 / 500.0),
                });
            }
            s.particles[i].trail = acc;
        }
        i += 1;
    }
}

/// What to draw for each particle: position (16.16), sprite, animation phase.
pub fn frames() -> Vec<(i32, i32, u32, u32)> {
    let Ok(s) = STATE.lock() else {
        return Vec::new();
    };
    let fixed = |v: f32| (v * 65536.0) as i32;
    // Trails first, so the debris draws over them.
    let puffs = s.puffs.iter().map(|p| {
        (fixed(p.x), fixed(p.y), p.sprite as u32, ((p.phase * 65536.0) as u32).min(0xFFFF))
    });
    puffs.chain(s.particles
        .iter()
        .map(|p| {
            let mut sprite = p.pellet.sprite as u32;
            let phase = match p.pellet.anim_kind {
                // The fuse's progress.
                0 => ((p.age as i64 * 0x10000) / p.pellet.life.max(1) as i64).clamp(0, 0xFFFF) as u32,
                // The flight direction (atan2 of the speed, 0x10000 a turn).
                2 if p.vx != 0.0 || p.vy != 0.0 => {
                    let a = p.vx.atan2(-p.vy) / std::f32::consts::TAU;
                    ((a.rem_euclid(1.0)) * 65536.0) as u32 & 0xFFFF
                }
                // Up to three frames on by horizontal speed, facing it.
                3 => {
                    let step = (p.vx.abs() / 2.0).min(3.0) as u32;
                    sprite = sprite.wrapping_add(step);
                    if p.vx < 0.0 {
                        sprite |= game::offsets::SPRITE_MIRROR_X;
                    }
                    0
                }
                // 1 is the object's spin, which nothing gives debris.
                _ => 0,
            };
            (fixed(p.x), fixed(p.y), sprite, phase)
        }))
        .collect()
}

pub fn count() -> usize {
    STATE.lock().map(|s| s.particles.len() + s.puffs.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The particle list is global: tests that fill it take turns.
    static LIST: Mutex<()> = Mutex::new(());

    /// Project X's Pigeon Strike's feathers and Pogo Sheep's blood, and WA's
    /// Cluster Bomb bits, from the scheme's records (rd[0x00..0x29]).
    fn feathers() -> Vec<i32> {
        let mut rd = vec![0; 0x2A];
        rd[0x00] = 137282;
        rd[0x05] = 1;
        rd[0x06] = 134;
        rd[0x07] = 1;
        rd[0x0B] = 50;
        rd[0x0C] = 99;
        rd[0x0E] = 60;
        rd[0x0F] = 45;
        rd[0x11] = 10000;
        rd[0x17] = 2;
        rd[0x18] = 137282;
        rd[0x19] = 20;
        rd[0x1A] = 20;
        rd[0x1C] = 8;
        rd[0x22] = 1;
        rd
    }

    #[test]
    fn feathers_and_blood_are_debris() {
        let p = inert_pellet(&feathers()).expect("feathers are debris");
        assert_eq!((p.sprite, p.life, p.terrain), (134, 500, Terrain::Pass));
        assert!((p.damping - 0.99).abs() < 1e-6 && (p.gravity - 0.45).abs() < 1e-6);
        let mut blood = vec![0; 0x2A];
        blood[0x05] = 1;
        blood[0x06] = 815;
        blood[0x0C] = 65;
        blood[0x0E] = 650;
        blood[0x0F] = 75;
        blood[0x11] = 2000;
        blood[0x17] = 2;
        blood[0x18] = 2;
        blood[0x08] = 249;
        blood[0x09] = 1;
        blood[0x0A] = 20;
        let b = inert_pellet(&blood).expect("blood is debris");
        assert_eq!((b.life, b.terrain, b.trail_sprite), (100, Terrain::Pass, 249));
        // A puff every 25 frames, each playing for about 62.
        assert!((b.trail_rate - 0.04).abs() < 1e-6);
        assert!((1.0 / b.trail_anim - 62.4).abs() < 0.5, "{}", 1.0 / b.trail_anim);
    }

    #[test]
    fn bits_that_matter_stay_missiles() {
        // The Cluster Bomb: a fuse blast, and it touches worms.
        let mut cluster = feathers();
        cluster[0x00] = 137342;
        cluster[0x02] = 100;
        cluster[0x03] = 20;
        cluster[0x18] = 4331614;
        assert!(inert_pellet(&cluster).is_none());
        // No blast, but it can hit a worm.
        let mut pusher = feathers();
        pusher[0x00] |= 1 << LAYER_WORM;
        assert!(inert_pellet(&pusher).is_none());
        // A contact blast (the Banana Bomb's), homing, the fire key.
        for (i, v) in [(RD_CONTACT_BLAST, 15), (RD_MISSILE_TYPE, 1), (RD_EXPLODE_ON_SPACE, 1)] {
            let mut rd = feathers();
            rd[i] = v;
            assert!(inert_pellet(&rd).is_none(), "rd[{i:#x}] = {v}");
        }
        // No fuse: it would never go away.
        let mut forever = feathers();
        forever[RD_FUSE_MS] = 0;
        assert!(inert_pellet(&forever).is_none());
    }

    #[test]
    fn the_landscape_bit_makes_it_stop() {
        let mut rd = feathers();
        rd[RD_COLLISION_MASK] |= LANDSCAPE_BIT as i32;
        assert_eq!(inert_pellet(&rd).unwrap().terrain, Terrain::Stop);
    }

    #[test]
    fn launched_as_create_clusters_launches() {
        // Angle 0 is straight up: out 20 px above, rising.
        let l = launch(100.0, 50.0, 0.0, 0.0, 0, 0, 100, 0, 0.5, 0.0);
        assert!((l.x - 100.0).abs() < 1e-4 && (l.y - 30.0).abs() < 1e-4);
        assert!(l.vx.abs() < 1e-4 && (l.vy + 24.0).abs() < 1e-4);
        // -1: back against the missile's motion. Falling: up; rising: down;
        // moving right: to the left (270).
        assert!(launch(0.0, 0.0, 1.0, 5.0, -1, 0, 100, 0, 0.5, 0.0).vy < 0.0);
        assert!(launch(0.0, 0.0, 1.0, -5.0, -1, 0, 100, 0, 0.5, 0.0).vy > 0.0);
        assert!(launch(0.0, 0.0, 5.0, 1.0, -1, 0, 100, 0, 0.5, 0.0).vx < 0.0);
        // The variance takes up to its share off the speed.
        let slow = launch(0.0, 0.0, 0.0, 0.0, 0, 0, 100, 30, 0.5, 0.0);
        assert!((slow.vy + 24.0 * 0.7).abs() < 1e-4);
    }

    #[test]
    fn debris_falls_times_out_and_passes_or_stops() {
        let _list = LIST.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        let mut p = inert_pellet(&feathers()).unwrap();
        p.flutter = 0.0;
        if let Ok(mut s) = STATE.lock() {
            s.particles.push(Particle { pellet: p, x: 0.0, y: 0.0, vx: 0.0, vy: 0.0, age: 0, resting: false, trail: 0.0 });
            let mut stopper = p;
            stopper.terrain = Terrain::Stop;
            s.particles.push(Particle { pellet: stopper, x: 0.0, y: 0.0, vx: 0.0, vy: 0.0, age: 0, resting: false, trail: 0.0 });
        }
        let ground = |_: i32, y: i32| y >= 5;
        for _ in 0..40 {
            tick(1.0, 0.0, 1e9, ground);
        }
        let f = frames();
        assert_eq!(f.len(), 2);
        assert!(f[0].1 > 5 * 65536, "passes through the ground");
        assert!(f[1].1 < 5 * 65536, "comes to rest on it");
        for _ in 0..500 {
            tick(1.0, 0.0, 1e9, ground);
        }
        assert_eq!(count(), 0, "gone when its fuse runs out");
    }

    #[test]
    fn debris_leaves_its_trail() {
        let _list = LIST.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        let mut p = inert_pellet(&feathers()).unwrap();
        p.flutter = 0.0;
        p.trail_sprite = 249;
        p.trail_rate = 0.5;
        p.trail_anim = 0.1;
        if let Ok(mut s) = STATE.lock() {
            s.particles.push(Particle { pellet: p, x: 0.0, y: 0.0, vx: 1.0, vy: 0.0, age: 0, resting: false, trail: 0.0 });
        }
        for _ in 0..4 {
            tick(0.0, 0.0, 1e9, |_, _| false);
        }
        let trails = frames().iter().filter(|f| f.2 == 249).count();
        assert_eq!(trails, 2, "one every other frame");
        for _ in 0..12 {
            tick(0.0, 0.0, 1e9, |_, _| false);
        }
        let oldest = frames().iter().filter(|f| f.2 == 249).count();
        assert!(oldest <= 5, "each plays its animation once, then goes: {oldest}");
        clear();
    }
}
