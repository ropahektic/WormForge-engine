//! Stock MissileEntity `on_explode` attachments — same idea as mine boom bits.
//! Keep the stock fire path (HM / bazooka / grenade / …); spawn Lua clusters
//! when the missile detonates.

use crate::log::{log_error, log_line};
use crate::mine;
use crate::rebase::rb;
use game::entity::read_i32;
use game::offsets::{MISSILE_HANDLE_MESSAGE, STOCK_WEAPON_COUNT};
use minhook::MinHook;
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Mutex;
use weapons::AttachedSpawn;

const MISSILE_CTOR: u32 = 0x0050_7D10;
const MISSILE_DETONATE: u32 = 0x0050_9AC0;

#[derive(Clone, Debug, Default)]
pub struct BoomTune {
    pub sprite: i32,
    pub explode: Option<AttachedSpawn>,
    pub explode_id: u32,
}

#[derive(Clone, Debug)]
struct Live {
    handle: u32,
    owner: u32,
    slot: usize,
    tune: BoomTune,
}

static SLOTS: Mutex<Vec<Option<BoomTune>>> = Mutex::new(Vec::new());
static LIVE: Mutex<Vec<Live>> = Mutex::new(Vec::new());
static ORIG_CTOR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_BOOM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

type CtorFn = unsafe extern "thiscall" fn(*mut u8, u32, *const i32, *mut u8) -> *mut u8;

fn ensure_slots(g: &mut Vec<Option<BoomTune>>) {
    if g.len() < STOCK_WEAPON_COUNT {
        g.resize(STOCK_WEAPON_COUNT, None);
    }
}

pub fn clear() {
    if let Ok(mut g) = SLOTS.lock() {
        g.clear();
        g.resize(STOCK_WEAPON_COUNT, None);
    }
    if let Ok(mut g) = LIVE.lock() {
        g.clear();
    }
}

pub fn claim(slot: usize, tune: BoomTune) {
    if slot >= STOCK_WEAPON_COUNT {
        return;
    }
    if tune.explode.is_none() && tune.explode_id == 0 {
        return;
    }
    if let Ok(mut g) = SLOTS.lock() {
        ensure_slots(&mut g);
        g[slot] = Some(tune);
    }
}

fn tune_for_slot(slot: usize) -> Option<BoomTune> {
    let g = SLOTS.lock().ok()?;
    g.get(slot)?.clone()
}

pub fn install(_catalog: &re_catalog::Catalog) -> Result<(), String> {
    unsafe {
        ORIG_CTOR.store(
            MinHook::create_hook(rb(MISSILE_CTOR) as *mut c_void, ctor_hook as *mut c_void)
                .map_err(|e| format!("missile ctor hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_BOOM.store(
            MinHook::create_hook(
                rb(MISSILE_DETONATE) as *mut c_void,
                boom_hook as *mut c_void,
            )
            .map_err(|e| format!("missile detonate hook: {e:?}"))?,
            Ordering::SeqCst,
        );
    }
    let _ = MISSILE_HANDLE_MESSAGE;
    log_line("missile on_explode wrap installed");
    Ok(())
}

unsafe extern "thiscall" fn ctor_hook(
    this: *mut u8,
    owner: u32,
    fire_params: *const i32,
    ctx: *mut u8,
) -> *mut u8 {
    let orig = ORIG_CTOR.load(Ordering::SeqCst);
    let p = if orig.is_null() {
        this
    } else {
        let f: CtorFn = unsafe { std::mem::transmute(orig) };
        unsafe { f(this, owner, fire_params, ctx) }
    };
    if p.is_null() {
        return p;
    }
    let Some(slot) = mine::slot_from_fire_params(p, fire_params) else {
        return p;
    };
    let Some(tune) = tune_for_slot(slot) else {
        return p;
    };
    if let Ok(mut live) = LIVE.lock() {
        live.retain(|m| m.handle != p as u32);
        live.push(Live {
            handle: p as u32,
            owner,
            slot,
            tune,
        });
    }
    p
}

fn live_of(this: *mut u8) -> Option<Live> {
    let g = LIVE.lock().ok()?;
    g.iter().find(|m| m.handle == this as u32).cloned()
}

unsafe extern "C" fn before_boom(this: *mut u8) {
    if this.is_null() {
        return;
    }
    let Some(live) = live_of(this) else {
        return;
    };
    let x = unsafe { read_i32(this, 0x84) };
    let y = unsafe { read_i32(this, 0x88) };
    fire_step(
        this,
        live.owner,
        live.slot as i32,
        live.tune.explode_id,
        live.tune.explode.as_ref(),
        live.tune.sprite,
        x,
        y,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn boom_hook() {
    // FUN_00509ac0: usercall EAX=this, stack (x, y). Emit bits then stock boom.
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push eax",
        "call {before}",
        "add esp, 4",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        before = sym before_boom,
        orig = sym ORIG_BOOM,
    );
}

fn fire_step(
    this: *mut u8,
    owner: u32,
    slot: i32,
    cb: u32,
    spawn: Option<&AttachedSpawn>,
    inherit_sprite: i32,
    x: i32,
    y: i32,
) {
    if cb == 0 && spawn.is_none() {
        return;
    }
    let worm = if owner != 0 { owner } else { this as u32 };
    if cb != 0 {
        if let Err(e) = lua_host::weapon_emit("_explodes", cb, "on_explode", worm, slot, x, y) {
            log_error(&format!("missile on_explode: {e}"));
        }
    }
    if let Some(spawn) = spawn {
        let body = crate::actor::stock_owner(this, owner);
        crate::actor::emit_attached(body, x, y, spawn, inherit_sprite);
    }
}
