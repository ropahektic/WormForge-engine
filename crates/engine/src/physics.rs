//! Game-thread wraps: projectile gravity leaf, missile frames, worm lockstep messages.
//!
//! Scripted stock missiles: Lua is the brain, stock MissileEntity is the body.
//!
//! `CollisionManager__apply_projectile_gravity` (0x004FCB30, thiscall RET 0x4):
//! `speed_y += particle_gravity_y * entity+0x60`, then
//! `speed_x += wind_bias * entity+0x5C` if a 1px wind probe is clear, then
//! both components × entity+0x58 (damp). Bounce/land is *not* this function —
//! that is `CollisionManager__rope` from `update_0`. Lua therefore:
//! - `gravity_before` writes the three scales so stock wind/gravity see them
//! - `gravity_after` edits velocity (extra bounce, stickiness, spin kicks)

use crate::log_line;
use crate::rebase::rb;
use game::entity::{is_missile, read_cm_forces, read_i32, read_motion, write_i32, write_motion, write_scales};
use game::offsets::{
    APPLY_PROJECTILE_GRAVITY, ENT_MOVED, ENT_POS_X, ENT_POS_Y, ENT_SPEED_X, ENT_SPEED_Y,
    ENT_WORM_TEAM, MISSILE_HANDLE_MESSAGE, MISSILE_VTABLE, MSG_FRAME_FINISH, MSG_RENDER_SCENE,
    WORM_FACING, WORM_HANDLE_MESSAGE, WORM_SET_STATE, WORM_STATE_AIRBORNE, WORLD_ROOT_HANDLE_MESSAGE,
    WORLD_ROOT_TURN_ENDED,
};
use minhook::MinHook;
use re_catalog::Catalog;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

type ApplyGravityFn = unsafe extern "thiscall" fn(*mut u8, *mut u8);
type HandleMessageFn = unsafe extern "thiscall" fn(*mut u8, u32, u32, u32, *mut u8);

static ORIG_GRAVITY: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_MISSILE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_WORM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_ROOT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

struct Carry {
    handle: u32,
    x: i32,
    y: i32,
    facing: i32,
}

static CARRY: Mutex<Vec<Carry>> = Mutex::new(Vec::new());

fn holding() -> bool {
    CARRY
        .lock()
        .ok()
        .map(|g| !g.is_empty())
        .unwrap_or(false)
}

pub fn drop_all() {
    let handles = CARRY
        .lock()
        .ok()
        .map(|g| g.iter().map(|c| c.handle).collect::<Vec<_>>())
        .unwrap_or_default();
    for h in handles {
        drop_carry(h);
    }
}

type SetStateFn = unsafe extern "thiscall" fn(*mut u8, u32);

pub fn carry(handle: u32, x: i32, y: i32) {
    if handle == 0 {
        return;
    }
    if let Ok(mut g) = CARRY.lock() {
        if let Some(slot) = g.iter_mut().find(|c| c.handle == handle) {
            slot.x = x;
            slot.y = y;
        } else {
            let facing = unsafe { read_i32(handle as *mut u8, WORM_FACING) };
            g.push(Carry {
                handle,
                x,
                y,
                facing,
            });
            crate::world::freeze_turn();
        }
    }
    crate::world::hold_turn_timer();
}

pub fn drop_carry(handle: u32) {
    if handle != 0 {
        unsafe {
            write_i32(handle as *mut u8, ENT_MOVED, 1);
            let f: SetStateFn = std::mem::transmute(rb(WORM_SET_STATE) as usize);
            f(handle as *mut u8, WORM_STATE_AIRBORNE);
        }
    }
    if let Ok(mut g) = CARRY.lock() {
        g.retain(|c| c.handle != handle);
        if g.is_empty() {
            crate::world::thaw_turn();
        }
    }
}

pub fn install(catalog: &Catalog) -> Result<(), String> {
    let g = catalog
        .function_va("CollisionManager__apply_projectile_gravity")
        .unwrap_or(APPLY_PROJECTILE_GRAVITY);
    let m = catalog
        .function_va("MissileEntity__HandleMessage")
        .unwrap_or(MISSILE_HANDLE_MESSAGE);
    let w = catalog
        .function_va("WormEntity__HandleMessage")
        .unwrap_or(WORM_HANDLE_MESSAGE);

    unsafe {
        ORIG_GRAVITY.store(
            MinHook::create_hook(rb(g) as *mut c_void, gravity_hook as *mut c_void)
                .map_err(|e| format!("gravity hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_MISSILE.store(
            MinHook::create_hook(rb(m) as *mut c_void, missile_hook as *mut c_void)
                .map_err(|e| format!("missile hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_WORM.store(
            MinHook::create_hook(rb(w) as *mut c_void, worm_hook as *mut c_void)
                .map_err(|e| format!("worm hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        let root = catalog
            .function_va("WorldRootEntity__HandleMessage")
            .unwrap_or(WORLD_ROOT_HANDLE_MESSAGE);
        ORIG_ROOT.store(
            MinHook::create_hook(rb(root) as *mut c_void, root_hook as *mut c_void)
                .map_err(|e| format!("world-root hook: {e:?}"))?,
            Ordering::SeqCst,
        );
    }
    log_line("physics wraps: gravity leaf, missile HM, worm HM, world root");
    Ok(())
}

fn missile_vt() -> u32 {
    rb(MISSILE_VTABLE)
}

fn with_cm_forces(cm: *mut u8, mut m: game::entity::Motion) -> game::entity::Motion {
    let (gy, wind) = unsafe { read_cm_forces(cm) };
    m.cm_gravity = gy;
    m.cm_wind = wind;
    m
}

unsafe extern "thiscall" fn gravity_hook(cm: *mut u8, entity: *mut u8) {
    let missile = unsafe { is_missile(entity, missile_vt()) };
    if missile {
        if let Some(m) = unsafe { read_motion(entity) } {
            let m = with_cm_forces(cm, m);
            if let Ok(before) = lua_host::gravity_before(m) {
                unsafe { write_scales(entity, &before) };
            }
        }
    }
    let orig = ORIG_GRAVITY.load(Ordering::SeqCst);
    if !orig.is_null() {
        let f: ApplyGravityFn = unsafe { std::mem::transmute(orig) };
        unsafe { f(cm, entity) };
    }
    if missile {
        if let Some(m) = unsafe { read_motion(entity) } {
            let m = with_cm_forces(cm, m);
            if let Ok(after) = lua_host::gravity_after(m) {
                unsafe { write_motion(entity, &after) };
            }
        }
    }
}

unsafe extern "thiscall" fn missile_hook(
    this: *mut u8,
    a: u32,
    msg: u32,
    b: u32,
    c: *mut u8,
) {
    if msg == MSG_FRAME_FINISH {
        if let Some(m) = unsafe { read_motion(this) } {
            if let Ok(after) = lua_host::missile_frame(m, msg, "pre") {
                unsafe { write_motion(this, &after) };
            }
        }
    }
    let orig = ORIG_MISSILE.load(Ordering::SeqCst);
    if !orig.is_null() {
        let f: HandleMessageFn = unsafe { std::mem::transmute(orig) };
        unsafe { f(this, a, msg, b, c) };
    }
    if msg == MSG_FRAME_FINISH {
        if let Some(m) = unsafe { read_motion(this) } {
            if let Ok(after) = lua_host::missile_frame(m, msg, "post") {
                unsafe { write_motion(this, &after) };
            }
        }
        crate::sprite::advance_loop(this);
    }
}

unsafe extern "thiscall" fn root_hook(this: *mut u8, a: u32, msg: u32, b: u32, c: *mut u8) {
    let orig = ORIG_ROOT.load(Ordering::SeqCst);
    if !orig.is_null() {
        let f: HandleMessageFn = unsafe { std::mem::transmute(orig) };
        unsafe { f(this, a, msg, b, c) };
    }
    if !holding() {
        return;
    }
    if msg == MSG_FRAME_FINISH || msg == MSG_RENDER_SCENE {
        crate::world::freeze_turn();
    }
    if msg == MSG_FRAME_FINISH && !this.is_null() {
        let ended = unsafe { read_i32(this, WORLD_ROOT_TURN_ENDED) != 0 };
        if ended {
            drop_all();
        }
    }
}

unsafe extern "thiscall" fn worm_hook(this: *mut u8, a: u32, msg: u32, b: u32, c: *mut u8) {
    let team = if this.is_null() {
        0
    } else {
        unsafe { game::entity::read_i32(this, ENT_WORM_TEAM) as u32 }
    };
    let eat = lua_host::worm_message(this as u32, team, msg).unwrap_or(false);
    if eat {
        return;
    }
    let orig = ORIG_WORM.load(Ordering::SeqCst);
    if !orig.is_null() {
        let f: HandleMessageFn = unsafe { std::mem::transmute(orig) };
        unsafe { f(this, a, msg, b, c) };
    }
    if msg == MSG_FRAME_FINISH {
        crate::actor::on_weapon_slot(this);
        apply_carry(this);
    }
}

fn apply_carry(this: *mut u8) {
    if this.is_null() {
        return;
    }
    let handle = this as u32;
    let Some((x, y, facing)) = CARRY.lock().ok().and_then(|g| {
        g.iter()
            .find(|c| c.handle == handle)
            .map(|c| (c.x, c.y, c.facing))
    }) else {
        return;
    };
    unsafe {
        write_i32(this, ENT_POS_X, x);
        write_i32(this, ENT_POS_Y, y);
        write_i32(this, ENT_SPEED_X, 0);
        write_i32(this, ENT_SPEED_Y, 0);
        write_i32(this, WORM_FACING, facing);
        crate::world::hold_turn_timer();
    }
}
