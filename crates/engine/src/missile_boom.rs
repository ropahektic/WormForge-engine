//! Stock MissileEntity `on_explode` attachments — same idea as mine boom bits.
//! Keep the stock fire path (HM / bazooka / grenade / …); spawn Lua clusters
//! when the missile detonates.

use crate::log::{log_error, log_line};
use crate::mine;
use crate::rebase::rb;
use game::entity::{read_i32, write_i32};
use game::offsets::{
    MISSILE_ALT_BANK_SPRITE, MISSILE_HANDLE_MESSAGE, MISSILE_HOMING_SPRITE_A,
    MISSILE_HOMING_SPRITE_B, MISSILE_RENDER_SPRITE, MISSILE_RENDER_SPRITE_ALT,
    MISSILE_WEAPON_ALT_SPRITE, MISSILE_WEAPON_SPRITE, STOCK_WEAPON_COUNT,
};
use minhook::MinHook;
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Mutex;
use weapons::AttachedSpawn;

const MISSILE_CTOR: u32 = 0x0050_7D10;
const MISSILE_DETONATE: u32 = 0x0050_9AC0;

/// Body graphic banks the missile ctor seeds from the weapon table. Rewriting
/// these after ctor changes flight without touching WeaponEntry+0x60, which
/// throw-aim poses still need (grenade/cluster/banana stay held correctly).
const BODY_GFX_OFFS: &[usize] = &[
    MISSILE_WEAPON_SPRITE,
    MISSILE_WEAPON_ALT_SPRITE,
    MISSILE_RENDER_SPRITE,
    MISSILE_ALT_BANK_SPRITE,
    MISSILE_RENDER_SPRITE_ALT,
    MISSILE_HOMING_SPRITE_A,
    MISSILE_HOMING_SPRITE_B,
];

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
/// Per-slot in-flight body override (table +0x60 left stock for aim).
static FLIGHT: Mutex<Vec<Option<i32>>> = Mutex::new(Vec::new());
static LIVE: Mutex<Vec<Live>> = Mutex::new(Vec::new());
static ORIG_CTOR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_BOOM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

type CtorFn = unsafe extern "thiscall" fn(*mut u8, u32, *const i32, *mut u8) -> *mut u8;

fn ensure_slots(g: &mut Vec<Option<BoomTune>>) {
    if g.len() < STOCK_WEAPON_COUNT {
        g.resize(STOCK_WEAPON_COUNT, None);
    }
}

fn ensure_flight(g: &mut Vec<Option<i32>>) {
    if g.len() < STOCK_WEAPON_COUNT {
        g.resize(STOCK_WEAPON_COUNT, None);
    }
}

pub fn clear() {
    if let Ok(mut g) = SLOTS.lock() {
        g.clear();
        g.resize(STOCK_WEAPON_COUNT, None);
    }
    if let Ok(mut g) = FLIGHT.lock() {
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

/// Flight-only body sprite for this slot. Does not rewrite WeaponEntry+0x60.
pub fn set_flight_sprite(slot: usize, sprite: i32) {
    if slot >= STOCK_WEAPON_COUNT || sprite == 0 {
        return;
    }
    if let Ok(mut g) = FLIGHT.lock() {
        ensure_flight(&mut g);
        g[slot] = Some(sprite);
    }
}

fn tune_for_slot(slot: usize) -> Option<BoomTune> {
    let g = SLOTS.lock().ok()?;
    g.get(slot)?.clone()
}

fn flight_for_slot(slot: usize) -> Option<i32> {
    let g = FLIGHT.lock().ok()?;
    g.get(slot).copied().flatten()
}

fn apply_flight_sprite(entity: *mut u8, sprite: i32) {
    if entity.is_null() || sprite == 0 {
        return;
    }
    unsafe {
        // Only rewrite banks that already had a sprite. Sheep / super-sheep /
        // HM leave some body slots 0 until a mode switch; filling those made
        // a plain sheep take the cape/flight path and clip terrain.
        write_i32(entity, MISSILE_WEAPON_SPRITE, sprite);
        write_i32(entity, MISSILE_RENDER_SPRITE, sprite);
        for &off in BODY_GFX_OFFS {
            if off == MISSILE_WEAPON_SPRITE || off == MISSILE_RENDER_SPRITE {
                continue;
            }
            if read_i32(entity, off) != 0 {
                write_i32(entity, off, sprite);
            }
        }
    }
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
    log_line("missile on_explode + flight-sprite wrap installed");
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
    if let Some(sprite) = flight_for_slot(slot) {
        apply_flight_sprite(p, sprite);
    }
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
