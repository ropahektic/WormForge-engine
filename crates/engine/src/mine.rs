//! Stock MineEntity keeps placement, proximity, knock, drown, and rest-arm.
//! Table `sprite_id` is unused: pick_sprite hardcodes mineon/mineoff. Wrap that
//! blit, ctor bounce/wind, and explode damage so overlays actually stick.
//! `on_place` / `on_proximity` / `on_explode` spawn extra LuaActors without
//! stealing this path.

use crate::log::{log_error, log_line};
use crate::rebase::rb;
use game::entity::{read_i32, write_i32};
use game::offsets::{
    CREATE_EXPLOSION, ENT_GRAVITY_SCALE, GAME_WORLD_WEAPON_TABLE, MINE_BOUNCE_X, MINE_BOUNCE_Y,
    MINE_ENTITY_CTOR, MINE_ENTITY_DTOR, MINE_EXPLODE, MINE_PICK_SPRITE, MINE_PROXIMITY_SCAN,
    MINE_SPRITE_OFF, MINE_SPRITE_ON, MINE_WIND, STOCK_WEAPON_COUNT, WEAPON_ENTRY_SIZE,
    WEAPON_FIRE_PARAMS,
};
use minhook::MinHook;
use re_catalog::Catalog;
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Mutex;
use weapons::AttachedSpawn;

#[derive(Clone, Debug, Default)]
pub struct MineTune {
    pub sprite: i32,
    pub damage: i32,
    pub bounce_pct: Option<i32>,
    pub gravity_pct: Option<i32>,
    pub wind_pct: Option<i32>,
    pub explode: Option<AttachedSpawn>,
    pub explode_id: u32,
    pub proximity: Option<AttachedSpawn>,
    pub proximity_id: u32,
    pub place: Option<AttachedSpawn>,
    pub place_id: u32,
}

#[derive(Clone, Debug)]
struct LiveMine {
    handle: u32,
    owner: u32,
    slot: usize,
    tune: MineTune,
    prox_emitted: bool,
}

static SLOTS: Mutex<Vec<Option<MineTune>>> = Mutex::new(Vec::new());
static LIVE: Mutex<Vec<LiveMine>> = Mutex::new(Vec::new());
static ORIG_CTOR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_DTOR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_PICK: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
#[allow(dead_code)]
static ORIG_BOOM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_SCAN: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static EXPLODE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

type CtorFn = unsafe extern "stdcall" fn(*mut u8, u32, *const i32, *const i32, i32, i32) -> *mut u8;
type DtorFn = unsafe extern "fastcall" fn(*mut u8);

fn ensure_slots(g: &mut Vec<Option<MineTune>>) {
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

pub fn claim(slot: usize, tune: MineTune) {
    if slot >= STOCK_WEAPON_COUNT {
        return;
    }
    if let Ok(mut g) = SLOTS.lock() {
        ensure_slots(&mut g);
        g[slot] = Some(tune);
    }
}

pub fn install(catalog: &Catalog) -> Result<(), String> {
    let ctor = catalog
        .function_va("MineEntity__ctor")
        .unwrap_or(MINE_ENTITY_CTOR);
    let dtor = catalog
        .function_va("MineEntity__dtor_inner")
        .unwrap_or(MINE_ENTITY_DTOR);
    let pick = catalog
        .function_va("MineEntity__pick_sprite")
        .unwrap_or(MINE_PICK_SPRITE);
    let boom = catalog
        .function_va("MineEntity__explode")
        .unwrap_or(MINE_EXPLODE);
    let scan = catalog
        .function_va("MineEntity__proximity_scan")
        .unwrap_or(MINE_PROXIMITY_SCAN);
    unsafe {
        ORIG_CTOR.store(
            MinHook::create_hook(rb(ctor) as *mut c_void, ctor_hook as *mut c_void)
                .map_err(|e| format!("mine ctor hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_DTOR.store(
            MinHook::create_hook(rb(dtor) as *mut c_void, dtor_hook as *mut c_void)
                .map_err(|e| format!("mine dtor hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_PICK.store(
            MinHook::create_hook(rb(pick) as *mut c_void, pick_hook as *mut c_void)
                .map_err(|e| format!("mine pick hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_BOOM.store(
            MinHook::create_hook(rb(boom) as *mut c_void, boom_hook as *mut c_void)
                .map_err(|e| format!("mine explode hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_SCAN.store(
            MinHook::create_hook(rb(scan) as *mut c_void, scan_hook as *mut c_void)
                .map_err(|e| format!("mine scan hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        EXPLODE.store(rb(CREATE_EXPLOSION) as *mut c_void, Ordering::SeqCst);
    }
    log_line("mine wraps: ctor, dtor, pick_sprite, explode, proximity");
    Ok(())
}

fn tune_for_slot(slot: usize) -> Option<MineTune> {
    let g = SLOTS.lock().ok()?;
    g.get(slot).cloned().flatten()
}

fn live_mine(this: *mut u8) -> Option<LiveMine> {
    let key = this as u32;
    LIVE.lock()
        .ok()?
        .iter()
        .find(|m| m.handle == key)
        .cloned()
}

fn live_tune(this: *mut u8) -> Option<MineTune> {
    live_mine(this).map(|m| m.tune)
}

fn slot_from_fire_params(this: *mut u8, fire_params: *const i32) -> Option<usize> {
    if this.is_null() || fire_params.is_null() {
        return None;
    }
    unsafe {
        let world = read_i32(this, game::offsets::ENT_WORLD) as *mut u8;
        if world.is_null() {
            return None;
        }
        let table = *(world.add(GAME_WORLD_WEAPON_TABLE) as *const usize);
        if table == 0 {
            return None;
        }
        let fp = fire_params as usize;
        let base = table + WEAPON_FIRE_PARAMS;
        if fp < base {
            return None;
        }
        let off = fp - base;
        if off % WEAPON_ENTRY_SIZE != 0 {
            return None;
        }
        let slot = off / WEAPON_ENTRY_SIZE;
        (slot < STOCK_WEAPON_COUNT).then_some(slot)
    }
}

fn pct_fixed(pct: i32) -> i32 {
    ((pct as i64) * 0x10000 / 100) as i32
}

unsafe extern "stdcall" fn ctor_hook(
    this: *mut u8,
    owner: u32,
    fire_params: *const i32,
    spawn: *const i32,
    ground: i32,
    placed: i32,
) -> *mut u8 {
    let orig = ORIG_CTOR.load(Ordering::SeqCst);
    let p = if orig.is_null() {
        this
    } else {
        let f: CtorFn = unsafe { std::mem::transmute(orig) };
        unsafe { f(this, owner, fire_params, spawn, ground, placed) }
    };
    if p.is_null() {
        return p;
    }
    let Some(slot) = slot_from_fire_params(p, fire_params) else {
        return p;
    };
    let Some(tune) = tune_for_slot(slot) else {
        return p;
    };
    unsafe {
        if let Some(b) = tune.bounce_pct {
            let v = pct_fixed(b);
            write_i32(p, MINE_BOUNCE_X, v);
            write_i32(p, MINE_BOUNCE_Y, v);
        }
        if let Some(g) = tune.gravity_pct {
            write_i32(p, ENT_GRAVITY_SCALE, pct_fixed(g));
        }
        if let Some(w) = tune.wind_pct {
            write_i32(p, MINE_WIND, pct_fixed(w));
        }
    }
    if let Ok(mut live) = LIVE.lock() {
        live.retain(|m| m.handle != p as u32);
        live.push(LiveMine {
            handle: p as u32,
            owner,
            slot,
            tune: tune.clone(),
            prox_emitted: false,
        });
    }
    fire_step(
        p,
        owner,
        slot as i32,
        tune.place_id,
        tune.place.as_ref(),
        tune.sprite,
        "on_place",
        read_i32(p, 0x84),
        read_i32(p, 0x88),
    );
    p
}

unsafe extern "fastcall" fn dtor_hook(this: *mut u8) {
    if let Ok(mut live) = LIVE.lock() {
        live.retain(|m| m.handle != this as u32);
    }
    let orig = ORIG_DTOR.load(Ordering::SeqCst);
    if !orig.is_null() {
        let f: DtorFn = unsafe { std::mem::transmute(orig) };
        unsafe { f(this) };
    }
}

#[unsafe(naked)]
unsafe extern "C" fn pick_hook() {
    core::arch::naked_asm!(
        "push edi",
        "push ecx",
        "push dword ptr [esp+12]",
        "mov ecx, dword ptr [esp+4]",
        "mov eax, [{orig}]",
        "call eax",
        "push dword ptr [esp+4]",
        "push dword ptr [esp+4]",
        "call {after}",
        "add esp, 8",
        "pop ecx",
        "pop edi",
        "ret 4",
        orig = sym ORIG_PICK,
        after = sym after_pick,
    );
}

unsafe extern "C" fn after_pick(this: *mut u8, sprite_out: *mut i32) {
    if this.is_null() || sprite_out.is_null() {
        return;
    }
    let Some(tune) = live_tune(this) else {
        return;
    };
    if tune.sprite == 0 {
        return;
    }
    unsafe {
        let cur = *sprite_out;
        if cur == MINE_SPRITE_ON || cur == MINE_SPRITE_OFF {
            *sprite_out = tune.sprite;
        }
    }
}

#[unsafe(naked)]
unsafe extern "C" fn boom_hook() {
    core::arch::naked_asm!(
        "push eax",
        "call {boom}",
        "add esp, 4",
        "ret",
        boom = sym explode_with_tune,
    );
}

unsafe extern "C" fn explode_with_tune(this: *mut u8) {
    if this.is_null() {
        return;
    }
    let damage = live_tune(this)
        .map(|t| t.damage)
        .filter(|d| *d > 0)
        .unwrap_or(100);
    let explode = EXPLODE.load(Ordering::SeqCst);
    if explode.is_null() {
        return;
    }
    // Bits must exist before CreateExplosion so the stock blast walk can throw them.
    if let Some(live) = live_mine(this) {
        let x = read_i32(this, 0x84);
        let y = read_i32(this, 0x88);
        fire_step(
            this,
            live.owner,
            live.slot as i32,
            live.tune.explode_id,
            live.tune.explode.as_ref(),
            live.tune.sprite,
            "on_explode",
            x,
            y,
        );
    }
    // Stock FUN_00507110: ESI=this, EAX=x, ECX=y+1.0, stdcall CreateExplosion(damage, blast, 0, team).
    unsafe {
        let x = read_i32(this, 0x84);
        let y = read_i32(this, 0x88).wrapping_add(0x100000);
        let blast = read_i32(this, 0x124);
        let team = read_i32(this, 0x144);
        let args = [
            this as u32,
            blast as u32,
            damage as u32,
            team as u32,
            explode as u32,
            x as u32,
            y as u32,
        ];
        core::arch::asm!(
            "push esi",
            "mov esi, dword ptr [ebx]",
            "mov eax, dword ptr [ebx + 20]",
            "mov ecx, dword ptr [ebx + 24]",
            "push dword ptr [ebx + 12]",
            "push 0",
            "push dword ptr [ebx + 4]",
            "push dword ptr [ebx + 8]",
            "call dword ptr [ebx + 16]",
            "pop esi",
            in("ebx") args.as_ptr(),
            out("eax") _,
            out("ecx") _,
            out("edx") _,
        );
    }
}

#[unsafe(naked)]
unsafe extern "C" fn scan_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push dword ptr [esp+8]",
        "mov eax, dword ptr [esp+4]",
        "mov edx, [{orig}]",
        "call edx",
        "push eax",
        "push dword ptr [esp+4]",
        "call {after}",
        "add esp, 8",
        "pop ecx",
        "ret 4",
        orig = sym ORIG_SCAN,
        after = sym after_scan,
    );
}

unsafe extern "C" fn after_scan(this: *mut u8, hit: *mut u8) -> *mut u8 {
    if this.is_null() || hit.is_null() {
        return hit;
    }
    let Some(live) = live_mine(this) else {
        return hit;
    };
    if live.prox_emitted {
        return hit;
    }
    if let Ok(mut g) = LIVE.lock() {
        if let Some(m) = g.iter_mut().find(|m| m.handle == this as u32) {
            m.prox_emitted = true;
        }
    }
    let x = read_i32(this, 0x84);
    let y = read_i32(this, 0x88);
    fire_step(
        this,
        live.owner,
        live.slot as i32,
        live.tune.proximity_id,
        live.tune.proximity.as_ref(),
        live.tune.sprite,
        "on_proximity",
        x,
        y,
    );
    hit
}

fn fire_step(
    this: *mut u8,
    owner: u32,
    slot: i32,
    cb: u32,
    spawn: Option<&AttachedSpawn>,
    inherit_sprite: i32,
    label: &str,
    x: i32,
    y: i32,
) {
    if cb == 0 && spawn.is_none() {
        return;
    }
    let worm = if owner != 0 { owner } else { this as u32 };
    if cb != 0 {
        if let Err(e) = lua_host::weapon_emit(
            match label {
                "on_explode" => "_explodes",
                "on_proximity" => "_proximity",
                _ => "_places",
            },
            cb,
            label,
            worm,
            slot,
            x,
            y,
        ) {
            log_error(&format!("{label}: {e}"));
        }
    }
    if let Some(spawn) = spawn {
        let body = crate::actor::stock_owner(this, owner);
        if label == "on_explode" {
            crate::actor::emit_attached(body, x, y, spawn, inherit_sprite);
        } else {
            crate::actor::emit_attached_later(body, x, y, spawn, inherit_sprite);
        }
    }
}
