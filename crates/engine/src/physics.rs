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
    WORM_DRAW_AIM_CURSOR, WORM_DRAW_ARROW, WORM_DRAW_CROSSHAIR, WORM_DRAW_CURSOR,
    WORM_DRAW_HEALTH, WORM_DRAW_HUD, WORM_DRAW_NAME, WORM_DRAW_OFFMAP, WORM_DRAW_SPRITE,
    WORM_DRAW_TURN, WORM_SHOW_CURSOR,
    WORM_FACING, WORM_HANDLE_MESSAGE, WORM_SET_STATE, WORM_STATE_AIRBORNE,
    WORLD_ROOT_HANDLE_MESSAGE, WORLD_ROOT_TURN_ENDED,
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
static ORIG_DRAW: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_NAME: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_HUD: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_HEALTH: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_CURSOR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_OFFMAP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_TURN: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_ARROW: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_AIM_CURSOR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_CROSSHAIR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

struct Carry {
    handle: u32,
    x: i32,
    y: i32,
    facing: i32,
}

static CARRY: Mutex<Vec<Carry>> = Mutex::new(Vec::new());
static HIDDEN: Mutex<Vec<u32>> = Mutex::new(Vec::new());

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
    hide_worm(handle, false);
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

pub fn hide_worm(handle: u32, hide: bool) {
    if handle == 0 {
        return;
    }
    if let Ok(mut g) = HIDDEN.lock() {
        if hide {
            if !g.contains(&handle) {
                g.push(handle);
            }
        } else {
            g.retain(|h| *h != handle);
        }
    }
    if hide {
        unsafe {
            write_i32(handle as *mut u8, WORM_SHOW_CURSOR, 0);
        }
    }
}

pub fn worm_hidden(handle: u32) -> bool {
    HIDDEN
        .lock()
        .ok()
        .map(|g| g.contains(&handle))
        .unwrap_or(false)
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
        ORIG_DRAW.store(
            MinHook::create_hook(
                rb(WORM_DRAW_SPRITE) as *mut c_void,
                draw_hook as *mut c_void,
            )
            .map_err(|e| format!("draw-sprite hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_NAME.store(
            MinHook::create_hook(rb(WORM_DRAW_NAME) as *mut c_void, name_hook as *mut c_void)
                .map_err(|e| format!("draw-name hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_HUD.store(
            MinHook::create_hook(rb(WORM_DRAW_HUD) as *mut c_void, hud_hook as *mut c_void)
                .map_err(|e| format!("draw-hud hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_HEALTH.store(
            MinHook::create_hook(
                rb(WORM_DRAW_HEALTH) as *mut c_void,
                health_hook as *mut c_void,
            )
            .map_err(|e| format!("draw-health hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_CURSOR.store(
            MinHook::create_hook(
                rb(WORM_DRAW_CURSOR) as *mut c_void,
                cursor_hook as *mut c_void,
            )
            .map_err(|e| format!("draw-cursor hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_OFFMAP.store(
            MinHook::create_hook(
                rb(WORM_DRAW_OFFMAP) as *mut c_void,
                offmap_hook as *mut c_void,
            )
            .map_err(|e| format!("draw-offmap hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_TURN.store(
            MinHook::create_hook(rb(WORM_DRAW_TURN) as *mut c_void, turn_hook as *mut c_void)
                .map_err(|e| format!("draw-turn hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_ARROW.store(
            MinHook::create_hook(
                rb(WORM_DRAW_ARROW) as *mut c_void,
                arrow_hook as *mut c_void,
            )
            .map_err(|e| format!("draw-arrow hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_AIM_CURSOR.store(
            MinHook::create_hook(
                rb(WORM_DRAW_AIM_CURSOR) as *mut c_void,
                aim_cursor_hook as *mut c_void,
            )
            .map_err(|e| format!("draw-aim-cursor hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_CROSSHAIR.store(
            MinHook::create_hook(
                rb(WORM_DRAW_CROSSHAIR) as *mut c_void,
                crosshair_hook as *mut c_void,
            )
            .map_err(|e| format!("draw-crosshair hook: {e:?}"))?,
            Ordering::SeqCst,
        );
    }
    log_line("physics wraps: gravity leaf, missile HM, worm HM, world root, draw skip");
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
        crate::world::pump_space_fire(this);
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
        if worm_hidden(handle) {
            write_i32(this, WORM_SHOW_CURSOR, 0);
        }
        crate::world::hold_turn_timer();
    }
}

/// `WormEntity__DrawSprite_Maybe` is usercall EAX=this. Skip stock blit when hidden.
/// Must stay naked: an `extern "C"` trampoline clobbers EAX (prior match-start AV).
unsafe extern "C" fn draw_hidden(this: *mut u8) -> u8 {
    if this.is_null() {
        return 0;
    }
    worm_hidden(this as u32) as u8
}

#[unsafe(naked)]
unsafe extern "C" fn draw_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push eax",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_DRAW,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn cursor_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push eax",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_CURSOR,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn name_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push esi",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_NAME,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn hud_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push esi",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_HUD,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn health_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push esi",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_HEALTH,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn offmap_hook() {
    core::arch::naked_asm!(
        "push ecx",
        "push edx",
        "push dword ptr [esp+12]",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "ret 4",
        check = sym draw_hidden,
        orig = sym ORIG_OFFMAP,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn turn_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push ecx",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_TURN,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn arrow_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push esi",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_ARROW,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn aim_cursor_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push esi",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_AIM_CURSOR,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn crosshair_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push edi",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_CROSSHAIR,
    );
}
