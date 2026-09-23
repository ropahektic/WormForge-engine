//! One native LuaActor type: WorldEntity body, our vtable, CollisionManager register.
//!
//! Lua names the steps (cursor, spawn, sprite, loop, origin, gravity, explode).
//! Native code runs those primitives. It does not ship a hidden drop weapon.

use crate::log::{log_error, log_line, panic_text};
use crate::rebase::rb;
use crate::sprite;
use game::entity::{read_i32, write_i32};
use game::offsets::*;
use lua_host::SpawnArgs;
use minhook::MinHook;
use re_catalog::Catalog;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use weapons::{ActorSpawn, AttachedSpawn, ClusterHoming, ClusterSpec, ScreenBlit, SpawnFrom, SpriteOrigin};

type StrikeFireFn = unsafe extern "system" fn(*mut u8, *mut u8, *mut u8);
type CreateProjFn = unsafe extern "thiscall" fn(*mut u8, *mut u8, *mut u8);
type WorldCtorFn = unsafe extern "system" fn(*mut u8, *mut u8, u32, u32);
type ApplyGravityFn = unsafe extern "thiscall" fn(*mut u8, *mut u8);
type WorldFreeFn = unsafe extern "thiscall" fn(*mut u8, u32) -> *mut u8;
type WorldMsgFn = unsafe extern "thiscall" fn(*mut u8, u32, u32, u32, *mut u8);
type TeamMsgFn = unsafe extern "thiscall" fn(*mut u8, u32, u32, u32, *mut i32);
type CrtMallocFn = unsafe extern "C" fn(usize) -> *mut u8;

static ORIG_STRIKE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_TEAM_MSG: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_FIRE_DISPATCH: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_CREATE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static VTABLE: OnceLock<[usize; WORLD_ENTITY_VTABLE_SLOTS]> = OnceLock::new();
static ORIG_FREE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_MSG: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

#[derive(Clone)]
struct DropClaim {
    slot: usize,
    sprite: i32,
    delta: u32,
    spawn: Option<ActorSpawn>,
    click_id: u32,
}

#[derive(Clone)]
struct FireClaim {
    slot: usize,
    sprite: i32,
    delta: u32,
    spawn: Option<ActorSpawn>,
    fire_id: u32,
}

struct SelectClaim {
    slot: usize,
    select_id: u32,
    /// Swallow stock FireWeapon for this slot (no axe swing / no strike).
    eat_fire: bool,
}

static DROPS: Mutex<Vec<DropClaim>> = Mutex::new(Vec::new());
static FIRES: Mutex<Vec<FireClaim>> = Mutex::new(Vec::new());
static SELECTS: Mutex<Vec<SelectClaim>> = Mutex::new(Vec::new());
static LAST_SLOT: Mutex<Vec<(u32, i32)>> = Mutex::new(Vec::new());
static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static CHILD_EMITS: Mutex<Vec<(u32, AttachedSpawn)>> = Mutex::new(Vec::new());
static TRAILS: Mutex<Vec<(u32, i32)>> = Mutex::new(Vec::new());
static CLUSTERS: Mutex<Vec<(u32, ClusterLive)>> = Mutex::new(Vec::new());
static PUFFS: Mutex<Vec<SmokePuff>> = Mutex::new(Vec::new());
const SPRITE_SMOKE: i32 = 609;
const PUFF_LIFE: u8 = 18;

struct SmokePuff {
    x: i32,
    y: i32,
    sprite: i32,
    age: u8,
}

#[derive(Clone, Copy)]
struct ClusterLive {
    bounce_pct: i32,
    /// Remaining terrain hits before force-boom; 0 = unlimited.
    bounces_left: i32,
    /// Frames until auto-boom; 0 = no fuse.
    fuse_frames: i32,
    /// Worm proximity radius (px); 0 = off.
    proximity: i32,
    homing: ClusterHoming,
    #[allow(dead_code)]
    impact: bool,
    /// Homing waits until the blast/launch apex (vy >= 0, Y+ down).
    seek: bool,
}
static DEFERRED: Mutex<Vec<DeferredEmit>> = Mutex::new(Vec::new());
static EMIT_DEPTH: AtomicU32 = AtomicU32::new(0);
static PUFF_DRAWN: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
struct DeferredEmit {
    owner: u32,
    x: i32,
    y: i32,
    inherit: i32,
    emit: AttachedSpawn,
    wait: u8,
}
/// Actor ids, handed out in spawn order. Starts at 1 so 0 can mean "none".
static NEXT_SEQ: AtomicU32 = AtomicU32::new(1);

pub fn install(catalog: &Catalog) -> Result<(), String> {
    build_vtable();
    let va = catalog.function_va("FireWeapon__StrikeFire").unwrap_or(STRIKE_FIRE);
    let trampoline = unsafe {
        MinHook::create_hook(rb(va) as *mut c_void, strike_fire_hook as *mut c_void)
            .map_err(|e| format!("StrikeFire hook: {e:?}"))?
    };
    ORIG_STRIKE.store(trampoline, Ordering::SeqCst);

    let dispatch_tr = unsafe {
        MinHook::create_hook(
            rb(FIRE_WEAPON_DISPATCH) as *mut c_void,
            fire_weapon_dispatch_hook as *mut c_void,
        )
        .map_err(|e| format!("FireWeapon dispatch hook: {e:?}"))?
    };
    ORIG_FIRE_DISPATCH.store(dispatch_tr, Ordering::SeqCst);

    let create_va = catalog
        .function_va("FireWeapon__CreateWeaponProjectile")
        .unwrap_or(CREATE_WEAPON_PROJECTILE);
    let create_tr = unsafe {
        MinHook::create_hook(
            rb(create_va) as *mut c_void,
            create_proj_hook as *mut c_void,
        )
        .map_err(|e| format!("CreateWeaponProjectile hook: {e:?}"))?
    };
    ORIG_CREATE.store(create_tr, Ordering::SeqCst);

    // Land-cursor click (team msg 0x32) is eaten at its source for eat_fire
    // slots: no planted X, no reset_cursor, no worm FireWeapon / turn end.
    let team_tr = unsafe {
        MinHook::create_hook(
            rb(TEAM_MSG_HANDLER) as *mut c_void,
            team_msg_hook as *mut c_void,
        )
        .map_err(|e| format!("TeamEntity message hook: {e:?}"))?
    };
    ORIG_TEAM_MSG.store(team_tr, Ordering::SeqCst);

    lua_host::bind_actor_native(lua_host::ActorNative {
        explode: native_explode,
        despawn: native_despawn,
        in_water: native_in_water,
        on_feet: native_on_feet,
        gravity: native_gravity,
        move_body: native_move,
        advance: native_advance,
        set_frame: native_set_frame,
        spawn: native_spawn,
        list_actors: native_list_actors,
        list_worms: native_list_worms,
        resolve_sprite: native_resolve_sprite,
        land_solid: crate::world::solid_fixed,
        land_trace: crate::world::trace_fixed,
        map_size: crate::world::map_size_px,
        view_cam: crate::world::view_cam,
        water_y: crate::world::water_fixed,
        list_kind: crate::world::list_kind,
        play_sound: crate::world::play_sound,
        resolve_sound: crate::world::resolve_sound,
        pointer: crate::world::pointer,
        carry: crate::physics::carry,
        drop_carry: crate::physics::drop_carry,
        look: native_look,
        keys: crate::world::keys,
        networked: crate::world::is_networked,
        random: crate::world::draw_random,
        log: |m| crate::log_line(m),
        hide_worm: crate::physics::hide_worm,
        worm_hidden: crate::physics::worm_hidden,
        equip: crate::world::equip_weapon,
        hp: crate::world::worm_hp,
        ammo: crate::world::ammo,
        hurt: crate::physics::hit_worm,
    });
    log_line("LuaActor: FireWeapon dispatch + StrikeFire + CreateWeaponProjectile + TeamEntity msg wrap");
    Ok(())
}

/// True when `slot` was claimed with `eat_fire = true`.
pub fn slot_eats_fire(slot: usize) -> bool {
    SELECTS
        .lock()
        .ok()
        .map(|g| g.iter().any(|c| c.slot == slot && c.eat_fire))
        .unwrap_or(false)
}

/// The turn worm has an `eat_fire` (land-cursor) pack selected: WA's own
/// cursor / planted-X draws are skipped so only the pack's actor shows.
pub fn hide_team_cursor() -> bool {
    for h in list_worms_pub(0) {
        if h == 0 {
            continue;
        }
        let w = h as *mut u8;
        unsafe {
            if read_i32(w, WORM_TURN_ACTIVE) != 0 {
                return slot_eats_fire(read_i32(w, WORM_SELECTED_WEAPON) as usize);
            }
        }
    }
    false
}

fn turn_worm() -> *mut u8 {
    for h in list_worms_pub(0) {
        if h == 0 {
            continue;
        }
        let w = h as *mut u8;
        if unsafe { read_i32(w, WORM_TURN_ACTIVE) } != 0 {
            return w;
        }
    }
    std::ptr::null_mut()
}

/// TeamEntity message handler. Msg 0x32 (lockstep cursor click) for an
/// `eat_fire` slot runs the pack's `on_click` and is NOT forwarded: WA never
/// plants the X, never resets the cursor (MOVE stays → cursor keeps moving),
/// and the worm never fires (no launch anim, no turn end).
unsafe extern "thiscall" fn team_msg_hook(
    this: *mut u8,
    a: u32,
    msg: u32,
    b: u32,
    data: *mut i32,
) {
    if msg == TEAM_MSG_CURSOR_CLICK && !this.is_null() && !data.is_null() {
        let team = unsafe { read_i32(this, TEAM_INDEX) };
        if unsafe { *data } == team {
            let slot = unsafe { read_i32(this, TEAM_SELECTED_WEAPON) } as usize;
            if slot_eats_fire(slot) {
                let packed = unsafe { *data.add(2) };
                let x = ((packed & 0xFFFF) as i16 as i32) << 16;
                let y = ((packed >> 16) as i16 as i32) << 16;
                crate::log::log_line(&format!(
                    "TeamEntity click EAT slot={slot} at {x:#x},{y:#x}"
                ));
                if let Some(claim) = drop_for_slot(slot) {
                    if claim.click_id != 0 {
                        let worm = turn_worm();
                        if !worm.is_null() {
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                if let Err(e) = lua_host::weapon_click(
                                    claim.click_id,
                                    worm as u32,
                                    slot as i32,
                                    x,
                                    y,
                                    DROP_SKY_Y,
                                ) {
                                    log_error(&format!("on_click: {e}"));
                                }
                            }));
                        }
                    }
                }
                return;
            }
        }
    }
    let orig = ORIG_TEAM_MSG.load(Ordering::SeqCst);
    if !orig.is_null() {
        let f: TeamMsgFn = unsafe { std::mem::transmute(orig) };
        unsafe { f(this, a, msg, b, data) };
    }
}

pub fn claim_drop(
    slot: usize,
    sprite: i32,
    delta: u32,
    spawn: Option<ActorSpawn>,
    click_id: u32,
) {
    if let Ok(mut g) = DROPS.lock() {
        g.retain(|c| c.slot != slot);
        g.push(DropClaim {
            slot,
            sprite,
            delta,
            spawn,
            click_id,
        });
    }
}

pub fn claim_fire(
    slot: usize,
    sprite: i32,
    delta: u32,
    spawn: Option<ActorSpawn>,
    fire_id: u32,
) {
    if let Ok(mut g) = FIRES.lock() {
        g.retain(|c| c.slot != slot);
        g.push(FireClaim {
            slot,
            sprite,
            delta,
            spawn,
            fire_id,
        });
    }
}

pub fn claim_select(slot: usize, select_id: u32, eat_fire: bool) {
    if select_id == 0 && !eat_fire {
        return;
    }
    if let Ok(mut g) = SELECTS.lock() {
        g.retain(|c| c.slot != slot);
        g.push(SelectClaim {
            slot,
            select_id,
            eat_fire,
        });
    }
}

pub fn on_weapon_slot(worm: *mut u8) {
    if worm.is_null() {
        return;
    }
    let slot = unsafe { read_i32(worm, WORM_SELECTED_WEAPON) };
    let handle = worm as u32;
    let mut changed = false;
    if let Ok(mut g) = LAST_SLOT.lock() {
        if let Some(entry) = g.iter_mut().find(|e| e.0 == handle) {
            if entry.1 == slot {
                return;
            }
            entry.1 = slot;
            changed = true;
        } else {
            g.push((handle, slot));
            changed = true;
        }
    }
    if !changed {
        return;
    }
    let select_id = SELECTS
        .lock()
        .ok()
        .and_then(|g| {
            g.iter()
                .find(|c| c.slot == slot as usize)
                .map(|c| c.select_id)
        })
        .unwrap_or(0);
    if select_id == 0 {
        return;
    }
    if let Err(e) = lua_host::weapon_select(select_id, handle, slot) {
        log_error(&format!("on_select: {e}"));
    }
}

pub fn clear_claims() {
    if let Ok(mut g) = DROPS.lock() {
        g.clear();
    }
    if let Ok(mut g) = FIRES.lock() {
        g.clear();
    }
    if let Ok(mut g) = SELECTS.lock() {
        g.clear();
    }
    if let Ok(mut g) = LAST_SLOT.lock() {
        g.clear();
    }
    if let Ok(mut g) = LIVE.lock() {
        g.clear();
    }
    NEXT_SEQ.store(1, Ordering::SeqCst);
}

fn drop_for_slot(slot: usize) -> Option<DropClaim> {
    DROPS.lock().ok()?.iter().cloned().find(|c| c.slot == slot)
}

fn fire_for_slot(slot: usize) -> Option<FireClaim> {
    FIRES.lock().ok()?.iter().cloned().find(|c| c.slot == slot)
}

fn build_vtable() {
    VTABLE.get_or_init(|| {
        let src = rb(WORLD_ENTITY_VTABLE) as *const usize;
        let mut vt = [0usize; WORLD_ENTITY_VTABLE_SLOTS];
        unsafe {
            std::ptr::copy_nonoverlapping(src, vt.as_mut_ptr(), 20);
        }
        let ret0 = rb(STUB_RETURNS0) as usize;
        let ret1 = rb(STUB_RETURNS1) as usize;
        let noop = rb(STUB_NOOP) as usize;
        let s16 = vt[16];
        let s19 = vt[19];
        for slot in vt.iter_mut() {
            if *slot == 0 {
                *slot = ret0;
            }
        }
        if s16 == 0 {
            vt[16] = ret1;
        }
        if s19 == 0 {
            vt[19] = noop;
        }
        ORIG_FREE.store(vt[1] as *mut c_void, Ordering::SeqCst);
        ORIG_MSG.store(vt[2] as *mut c_void, Ordering::SeqCst);
        vt[1] = actor_free as *const () as usize;
        vt[2] = actor_message as *const () as usize;
        vt
    });
}

unsafe extern "system" fn strike_fire_hook(worm: *mut u8, fire_params: *mut u8, ctx: *mut u8) {
    // Spawn-style drop claims still eat here (caller sets ENT_ACTIVE afterward).
    // Click-only claims are eaten in fire_weapon_dispatch_hook so ENT_ACTIVE
    // is never set (that bit starts the stock "launch attack" anim).
    let slot = if worm.is_null() {
        usize::MAX
    } else {
        unsafe { read_i32(worm, WORM_SELECTED_WEAPON) as usize }
    };
    if let Some(claim) = drop_for_slot(slot) {
        if claim.click_id != 0 && claim.spawn.is_none() {
            crate::log::log_line(&format!("StrikeFire EAT(click) slot={slot}"));
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                let _ = try_spawn_drop(worm, ctx);
            }));
            crate::world::reset_strike_cursors();
            unsafe {
                write_i32(worm, ENT_ACTIVE, 0);
            }
            return;
        }
        crate::log::log_line(&format!("StrikeFire EAT slot={slot}"));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            let _ = try_spawn_drop(worm, ctx);
        }));
        crate::world::reset_strike_cursors();
        return;
    }
    let orig = ORIG_STRIKE.load(Ordering::SeqCst);
    if !orig.is_null() {
        let f: StrikeFireFn = unsafe { std::mem::transmute(orig) };
        unsafe { f(worm, fire_params, ctx) };
    }
}

/// EAX=weapon entry, ECX=ctx, `[esp+4]`=worm, `RET 4`.
/// Click-only cursor packs: run Lua and return without setting `ENT_ACTIVE`.
#[unsafe(naked)]
unsafe extern "C" fn fire_weapon_dispatch_hook() {
    core::arch::naked_asm!(
        "push ecx",
        "push eax",
        "push dword ptr [esp + 12]",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "pop eax",
        "pop ecx",
        "jnz 2f",
        "jmp dword ptr [{orig}]",
        "2:",
        "xor eax, eax",
        "ret 4",
        check = sym fire_weapon_click_only_eat,
        orig = sym ORIG_FIRE_DISPATCH,
    );
}

/// Args: worm, weapon, ctx (cdecl push order). Returns 1 when dispatch is eaten.
unsafe extern "C" fn fire_weapon_click_only_eat(
    worm: *mut u8,
    weapon: *mut u8,
    ctx: *mut u8,
) -> u8 {
    if worm.is_null() || weapon.is_null() {
        return 0;
    }
    let slot = unsafe { read_i32(worm, WORM_SELECTED_WEAPON) as usize };
    let eat_fire = SELECTS
        .lock()
        .ok()
        .and_then(|g| g.iter().find(|c| c.slot == slot).map(|c| c.eat_fire))
        .unwrap_or(false);
    if eat_fire {
        crate::log::log_line(&format!("FireWeapon EAT(eat_fire) slot={slot}"));
        unsafe {
            write_i32(worm, ENT_ACTIVE, 0);
        }
        return 1;
    }
    let fire_type = unsafe { read_i32(weapon, WEAPON_FIRE_TYPE) };
    if fire_type != 3 {
        return 0;
    }
    let Some(claim) = drop_for_slot(slot) else {
        return 0;
    };
    if claim.click_id == 0 || claim.spawn.is_some() {
        return 0;
    }
    crate::log::log_line(&format!("FireWeapon EAT(click-only) slot={slot}"));
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let _ = try_spawn_drop(worm, ctx);
    }));
    crate::world::reset_strike_cursors();
    unsafe {
        write_i32(worm, ENT_ACTIVE, 0);
    }
    1
}

unsafe extern "thiscall" fn create_proj_hook(
    worm: *mut u8,
    fire_params: *mut u8,
    ctx: *mut u8,
) {
    let spawned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        try_lua_projectile(worm, ctx)
    }));
    match spawned {
        Ok(true) => return,
        Ok(false) => {}
        Err(p) => log_error(&format!("lua projectile panic: {}", panic_text(p))),
    }
    let orig = ORIG_CREATE.load(Ordering::SeqCst);
    if !orig.is_null() {
        let f: CreateProjFn = unsafe { std::mem::transmute(orig) };
        unsafe { f(worm, fire_params, ctx) };
    }
}

unsafe fn try_spawn_drop(worm: *mut u8, ctx: *mut u8) -> bool {
    if worm.is_null() {
        return false;
    }
    let slot = read_i32(worm, WORM_SELECTED_WEAPON) as usize;
    let Some(claim) = drop_for_slot(slot) else {
        return false;
    };
    let (cursor_x, cursor_y) = cursor_xy(worm, ctx);
    if claim.click_id != 0 {
        crate::log::log_line(&format!(
            "on_click dispatch slot={slot} at {cursor_x:#x},{cursor_y:#x}"
        ));
        if let Err(e) = lua_host::weapon_click(
            claim.click_id,
            worm as u32,
            slot as i32,
            cursor_x,
            cursor_y,
            DROP_SKY_Y,
        ) {
            log_error(&format!("on_click: {e}"));
            return false;
        }
        return true;
    }
    let Some(spawn) = claim.spawn else {
        return false;
    };
    let sprite = claim.sprite;
    let x = cursor_x;
    let y = match spawn.from {
        SpawnFrom::Sky => DROP_SKY_Y,
        SpawnFrom::Cursor => cursor_y,
    };
    match spawn_configured(SpawnBody {
        owner: worm,
        x,
        y,
        vx: 0,
        vy: 0,
        sprite: claim.sprite,
        delta: claim.delta,
        spawn,
    }) {
        Ok(_) => {
            log_line(&format!(
                "LuaActor spawn slot {slot} sprite {sprite} at x={x:#x} y={y:#x}"
            ));
            true
        }
        Err(e) => {
            log_error(&format!("LuaActor spawn: {e}"));
            false
        }
    }
}

unsafe fn try_lua_projectile(worm: *mut u8, ctx: *mut u8) -> bool {
    if worm.is_null() {
        return false;
    }
    let slot = read_i32(worm, WORM_SELECTED_WEAPON) as usize;
    let Some(claim) = fire_for_slot(slot) else {
        return false;
    };
    let x = if ctx.is_null() {
        read_i32(worm, ENT_POS_X)
    } else {
        read_i32(ctx, CTX_SPAWN_X)
    };
    let y = if ctx.is_null() {
        read_i32(worm, ENT_POS_Y)
    } else {
        read_i32(ctx, CTX_SPAWN_Y)
    };
    let vx = if ctx.is_null() {
        0
    } else {
        read_i32(ctx, CTX_VEL_X)
    };
    let vy = if ctx.is_null() {
        0
    } else {
        read_i32(ctx, CTX_VEL_Y)
    };
    if claim.fire_id != 0 {
        let world = read_i32(worm, ENT_WORLD) as *mut u8;
        let frame = if world.is_null() {
            -1
        } else {
            read_i32(world, GAME_WORLD_FRAME_COUNTER)
        };
        log_line(&format!(
            "LuaActor on_fire frame={frame} slot={slot} worm={:#x} ctx={:#x}",
            worm as usize, ctx as usize
        ));
        if let Err(e) = lua_host::weapon_fire(claim.fire_id, worm as u32, slot as i32, x, y, vx, vy)
        {
            log_error(&format!("on_fire: {e}"));
            return false;
        }
        return true;
    }
    let Some(spawn) = claim.spawn else {
        return false;
    };
    match spawn_configured(SpawnBody {
        owner: worm,
        x,
        y,
        vx,
        vy,
        sprite: claim.sprite,
        delta: claim.delta,
        spawn,
    }) {
        Ok(_) => {
            log_line(&format!(
                "LuaActor fire slot {slot} sprite {} at {x:#x},{y:#x}",
                claim.sprite
            ));
            true
        }
        Err(e) => {
            log_error(&format!("LuaActor fire spawn: {e}"));
            false
        }
    }
}

fn cursor_xy(worm: *mut u8, ctx: *mut u8) -> (i32, i32) {
    // Land cursor is the live Teleport/strike aim point. Worm +0x2E0/+0x2E4
    // stay stale (wkAimbot CURSOR.md) — do not prefer them.
    let (px, py, _) = crate::world::pointer();
    if px != 0 || py != 0 {
        return (px, py);
    }
    if !ctx.is_null() {
        return (
            unsafe { read_i32(ctx, CTX_CURSOR_X) },
            unsafe { read_i32(ctx, CTX_CURSOR_Y) },
        );
    }
    if !worm.is_null() {
        return (
            unsafe { read_i32(worm, WORM_CURSOR_X) },
            unsafe { read_i32(worm, WORM_CURSOR_Y) },
        );
    }
    (0, 0)
}

struct SpawnBody {
    owner: *mut u8,
    x: i32,
    y: i32,
    vx: i32,
    vy: i32,
    sprite: i32,
    delta: u32,
    spawn: ActorSpawn,
}

unsafe fn spawn_lua_actor(body: SpawnBody) -> Result<u32, String> {
    let spawn = &body.spawn;
    let owner = body.owner;
    if owner.is_null() {
        return Err("spawn needs an owner worm or actor".into());
    }
    log_line(&format!(
        "LuaActor spawn sprite {} x={:#x} y={:#x} vx={:#x} vy={:#x} origin={:?} explode={:?}",
        body.sprite, body.x, body.y, body.vx, body.vy, spawn.origin, spawn.explode
    ));
    let malloc: CrtMallocFn = std::mem::transmute(rb(WA_MALLOC) as usize);
    let this = malloc(LUA_ACTOR_SIZE);
    if this.is_null() {
        return Err("malloc failed".into());
    }
    std::ptr::write_bytes(this, 0, LUA_ACTOR_SIZE);

    let parent = physics_parent(owner).unwrap_or(owner);
    let ctor: WorldCtorFn = std::mem::transmute(rb(WORLD_ENTITY_CTOR) as usize);
    ctor(this, parent, COLLISION_LAYER_MISSILE, 0);

    let vt = VTABLE.get().ok_or("vtable missing")?;
    *(this as *mut u32) = vt.as_ptr() as u32;
    write_i32(this, ENT_ACTIVE, 1);
    write_i32(
        this,
        ENT_COLLISION_MASK,
        if spawn.collide_worms { -1 } else { 0 },
    );
    write_i32(this, ENT_WIND_SCALE, 0);
    // Missile-layer objects still get stock CM gravity from +0x60.
    // `gravity = false` must zero the scale or the body falls through dirt.
    write_i32(
        this,
        ENT_GRAVITY_SCALE,
        if spawn.gravity { 0x10000 } else { 0 },
    );
    write_i32(this, ENT_DAMP, 0x10000);
    write_i32(this, ENT_BLAST_MASS, 0x10000);
    write_i32(this, ENT_POS_X, body.x);
    write_i32(this, ENT_POS_Y, body.y);
    write_i32(this, ENT_SPEED_X, body.vx);
    write_i32(this, ENT_SPEED_Y, body.vy);
    if read_i32(this, ENT_HELPER) == 0 {
        let helper = read_i32(owner, ENT_HELPER);
        if helper != 0 {
            write_i32(this, ENT_HELPER, helper);
        }
    }
    write_i32(this, ACTOR_MAGIC, LUA_ACTOR_MAGIC as i32);
    let mut sprite_id = body.sprite;
    if spawn.cursor && sprite_id == SPRITE_CURSOR_R && !owner.is_null() {
        // `sprite = "cursor"` means the stock land cursor, which WA draws in
        // the owner's team colour: TeamEntity+0x50 + 0x20 (cursorr..cursorc).
        let team = crate::world::team_entity(read_i32(owner, ENT_WORM_TEAM));
        if !team.is_null() {
            let colour = read_i32(team, TEAM_COLOUR);
            if (0..6).contains(&colour) {
                sprite_id = SPRITE_CURSOR_R + colour;
            }
        }
    }
    write_i32(
        this,
        ACTOR_SPRITE,
        if spawn.visible {
            sprite_id
        } else {
            CUSTOM_SPRITE_SENTINEL
        },
    );
    write_i32(this, ACTOR_PHASE, 0);
    write_i32(
        this,
        ACTOR_DELTA,
        if spawn.loop_anim {
            body.delta as i32
        } else {
            0
        },
    );
    write_i32(this, ACTOR_DEAD, 0);
    // Identity that survives being looked at from another machine. Assigned
    // in spawn order, which is reproducible; the address is not.
    write_i32(this, ACTOR_SEQ, NEXT_SEQ.fetch_add(1, Ordering::SeqCst) as i32);
    write_i32(this, ACTOR_OWNER, owner as i32);
    let (dmg, boom) = spawn
        .explode
        .map(|e| (e.damage, e.id))
        .unwrap_or((0, 0));
    write_i32(this, ACTOR_DAMAGE, dmg);
    write_i32(this, ACTOR_BOOM_ID, boom);
    write_i32(
        this,
        ACTOR_ORIGIN,
        match spawn.origin {
            SpriteOrigin::Center => ACTOR_ORIGIN_CENTER,
            SpriteOrigin::Bottom => ACTOR_ORIGIN_BOTTOM,
        },
    );
    write_i32(this, ACTOR_SPR_W, spawn.sprite_w);
    write_i32(this, ACTOR_SPR_H, spawn.sprite_h);
    let feet = match spawn.origin {
        SpriteOrigin::Bottom => spawn.feet.or_else(|| sprite::feet_for_id(body.sprite)),
        SpriteOrigin::Center => spawn.feet,
    };
    if let Some(feet) = feet {
        write_i32(this, ACTOR_FEET_X0, feet.x0);
        write_i32(this, ACTOR_FEET_X1, feet.x1);
        write_i32(this, ACTOR_FEET_Y, feet.y);
        write_i32(this, ACTOR_SPR_W, feet.width);
        write_i32(this, ACTOR_SPR_H, feet.height);
        log_line(&format!(
            "LuaActor platform x0={} x1={} y={}",
            feet.x0, feet.x1, feet.y
        ));
    } else {
        write_i32(this, ACTOR_FEET_X0, 0);
        write_i32(this, ACTOR_FEET_X1, spawn.sprite_w.saturating_sub(1).max(0));
        write_i32(this, ACTOR_FEET_Y, spawn.sprite_h.saturating_sub(1).max(0));
    }
    let mut flags = 0;
    let impact = spawn.cluster.as_ref().map(|c| c.impact).unwrap_or(true);
    if spawn.explode.is_some() && impact {
        flags |= ACTOR_FLAG_EXPLODE;
    }
    if spawn.gravity {
        flags |= ACTOR_FLAG_GRAVITY;
    }
    if spawn.loop_anim {
        flags |= ACTOR_FLAG_LOOP;
    }
    if spawn.collide_terrain {
        flags |= ACTOR_FLAG_COLLIDE_TERRAIN;
    }
    if spawn.collide_worms {
        flags |= ACTOR_FLAG_COLLIDE_WORMS;
    }
    if spawn.damage_terrain {
        flags |= ACTOR_FLAG_DAMAGE_TERRAIN;
    }
    if spawn.damage_worms {
        flags |= ACTOR_FLAG_DAMAGE_WORMS;
    }
    if spawn.persist {
        flags |= ACTOR_FLAG_PERSIST;
    }
    if spawn.drown {
        flags |= ACTOR_FLAG_DROWN;
    }
    if spawn.mask {
        flags |= ACTOR_FLAG_MASK;
        if spawn.mask_worms {
            flags |= ACTOR_FLAG_MASK_WORMS;
        }
        if spawn.mask_land {
            flags |= ACTOR_FLAG_MASK_LAND;
        }
        if spawn.mask_weapons {
            flags |= ACTOR_FLAG_MASK_WEAPONS;
        }
        if spawn.mask_actors {
            flags |= ACTOR_FLAG_MASK_ACTORS;
        }
        if sprite::mask_for_id(body.sprite).is_none() {
            log_line(&format!(
                "LuaActor mask=true but sprite {} has no opaque mask (stock?)",
                body.sprite
            ));
        }
    }
    if spawn.explode.is_some_and(|e| e.below) {
        flags |= ACTOR_FLAG_EXPLODE_BELOW;
    }
    if spawn.cursor {
        flags |= ACTOR_FLAG_CURSOR;
    }
    match spawn.screen {
        ScreenBlit::Fill => flags |= ACTOR_FLAG_SCREEN_FILL,
        ScreenBlit::Front => flags |= ACTOR_FLAG_SCREEN_FRONT,
        ScreenBlit::World => {}
    }
    write_i32(this, ACTOR_FLAGS, flags);
    write_i32(this, ACTOR_CONTACT, 0);
    write_i32(this, ACTOR_BRAIN, spawn.brain_id as i32);
    // GPU presentation defaults (entity path). Scale 0 would hide the quad.
    write_i32(this, ACTOR_ANGLE, 0);
    write_i32(this, ACTOR_SCALE, 0x10000);
    write_i32(this, ACTOR_TINT, -1); // 0xFFFFFFFF
    write_i32(this, ACTOR_DEPTH, 0x000D_0000); // in front of terrain
    lua_host::actor_spawned(this as u32, spawn.brain_id);
    remember_live(this as u32);
    if let Some(c) = spawn.cluster.as_ref() {
        remember_cluster(this as u32, c);
        if c.trail != 0 {
            remember_trail(this as u32, c.trail);
            drop_puff(this);
        }
    }
    if let Some(child) = spawn.on_explode.as_ref() {
        remember_child(this as u32, (**child).clone());
    }

    let world = read_i32(this, ENT_WORLD) as *mut u8;
    if !world.is_null() {
        let w = world.add(GAME_WORLD_MISSILE_WEIGHT) as *mut i32;
        *w = (*w).saturating_add(7);
    }
    // Mine / crater origin is inside the mask. First-tick land would detonate immediately.
    if spawn.collide_terrain && terrain_under_feet(this) {
        write_i32(this, ACTOR_CONTACT, 1);
        log_line("LuaActor spawn still in mask, seed contact so land waits");
    }
    log_line(&format!(
        "LuaActor ready helper={:#x} owner={:?} y={:#x}",
        read_i32(this, ENT_HELPER) as u32,
        owner,
        read_i32(this, ENT_POS_Y)
    ));
    if spawn.explode.is_some_and(|e| e.now) {
        explode(this);
    }
    Ok(this as u32)
}

fn physics_parent(entity: *mut u8) -> Option<*mut u8> {
    if entity.is_null() {
        return None;
    }
    unsafe { lookup_shared(entity, 0, SHARED_FILTER_PHYSICS) }
}

unsafe fn lookup_shared(entity: *mut u8, key_esi: u32, key_edi: u32) -> Option<*mut u8> {
    if entity.is_null() {
        return None;
    }
    let table = read_i32(entity, ENT_SHARED_DATA) as *mut u8;
    lookup_in_table(table, key_esi, key_edi)
}

unsafe fn lookup_in_table(table: *mut u8, key_esi: u32, key_edi: u32) -> Option<*mut u8> {
    if table.is_null() {
        return None;
    }
    let bucket = (key_esi.wrapping_mul(0x11).wrapping_add(key_edi) & 0xFF) as usize;
    let mut node = *(table.add(bucket * 4) as *const *mut u8);
    while !node.is_null() {
        let n_edi = *(node.add(4) as *const u32);
        let n_esi = *(node.add(8) as *const u32);
        if n_edi == key_edi && n_esi == key_esi {
            let found = *(node.add(12) as *const *mut u8);
            if !found.is_null() {
                return Some(found);
            }
        }
        node = *(node as *const *mut u8);
    }
    None
}

fn is_actor(this: *mut u8) -> bool {
    if this.is_null() {
        return false;
    }
    unsafe { read_i32(this, ACTOR_MAGIC) as u32 == LUA_ACTOR_MAGIC }
}

unsafe extern "thiscall" fn actor_message(
    this: *mut u8,
    sender: u32,
    msg: u32,
    size: u32,
    data: *mut u8,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        if !is_actor(this) {
            forward_message(this, sender, msg, size, data);
            return;
        }
        if read_i32(this, ACTOR_DEAD) != 0 {
            return;
        }
        match msg {
            MSG_FRAME_FINISH => tick(this),
            MSG_RENDER_SCENE => blit(this),
            MSG_EXPLOSION => apply_explosion_knock(this, data),
            MSG_IMPACT => {}
            _ => forward_message(this, sender, msg, size, data),
        }
    }));
}

fn forward_message(this: *mut u8, sender: u32, msg: u32, size: u32, data: *mut u8) {
    let orig = ORIG_MSG.load(Ordering::SeqCst);
    if orig.is_null() {
        return;
    }
    let f: WorldMsgFn = unsafe { std::mem::transmute(orig) };
    unsafe {
        f(this, sender, msg, size, data);
    }
}

unsafe fn tick(this: *mut u8) {
    let world = read_i32(this, ENT_WORLD) as *mut u8;
    if world.is_null() {
        return;
    }
    let flags = read_i32(this, ACTOR_FLAGS);
    let auto_phys = (flags
        & (ACTOR_FLAG_GRAVITY | ACTOR_FLAG_COLLIDE_TERRAIN | ACTOR_FLAG_COLLIDE_WORMS))
        != 0;
    // CM message 1 already ran update_0 (stock gravity + rope) on this body.
    // Homemade pos+=vel / a second gravity leaf made the arc twice as fast.
    apply_cluster_homing(this);
    apply_cluster_timers(this);
    if read_i32(this, ACTOR_DEAD) != 0 {
        return;
    }
    if (flags & ACTOR_FLAG_CURSOR) != 0 {
        // The body *is* the world cursor: land cursor in land-cursor mode
        // (Teleport-style select), lockstep-synced pointer otherwise. Done
        // before the brain so Lua sees a.x/a.y already on the pointer.
        let (px, py, _) = crate::world::pointer();
        write_i32(this, ENT_POS_X, px);
        write_i32(this, ENT_POS_Y, py);
    }
    let worm_hit = false;
    let mut land_now = false;
    let mut left_ground = false;
    if auto_phys {
        let collide_t = (flags & ACTOR_FLAG_COLLIDE_TERRAIN) != 0;
        let was_contact = read_i32(this, ACTOR_CONTACT) != 0;
        let on_terrain =
            collide_t && (terrain_under_feet(this) || read_i32(this, ENT_SETTLE) > 0);
        land_now = on_terrain && !was_contact;
        left_ground = !on_terrain && was_contact;
        write_i32(this, ACTOR_CONTACT, if on_terrain { 1 } else { 0 });
        drop_puff(this);
    }
    let brain = read_i32(this, ACTOR_BRAIN) as u32;
    if brain != 0 {
        let damage = read_i32(this, ENT_DAMAGE_ACCUM).max(0);
        write_i32(this, ENT_DAMAGE_ACCUM, 0);
        let ev = lua_host::ActorEvent {
            land: land_now,
            left_ground,
            hit_worm: worm_hit,
            in_water: drowned(this, world),
            damage,
        };
        if let Err(e) = lua_host::actor_frame(this as u32, brain, ev) {
            log_error(&format!("LuaActor brain: {e}"));
        }
        if read_i32(this, ACTOR_DEAD) != 0 {
            return;
        }
    } else if land_now {
        land(this);
        if read_i32(this, ACTOR_DEAD) != 0 {
            return;
        }
    }
    if brain == 0 && (flags & ACTOR_FLAG_DROWN) != 0 && drowned(this, world) {
        die(this);
        return;
    }
    let y = read_i32(this, ENT_POS_Y);
    // Invisible tick actors (sprite sentinel) may sit on the land cursor
    // anywhere on the map — do not cull them by Y.
    let invisible = read_i32(this, ACTOR_SPRITE) == CUSTOM_SPRITE_SENTINEL;
    if !invisible && (y > 0x1800_0000 || y < -0x2000_0000) {
        die(this);
        return;
    }
    let sprite_id = read_i32(this, ACTOR_SPRITE);
    if (flags & ACTOR_FLAG_CURSOR) != 0 && (SPRITE_CURSOR_R..SPRITE_CURSOR_R + 6).contains(&sprite_id) {
        ease_cursor_phase(this);
    } else if (flags & ACTOR_FLAG_LOOP) != 0 {
        advance_anim(this);
    }
}

/// Stock cursor sheet ease-in (TeamEntity msg 5 on `+0x3D4`): close 20 % of
/// the gap to `TEAM_CURSOR_PHASE_END` per frame, at least 0x400, then hold.
unsafe fn ease_cursor_phase(this: *mut u8) {
    let phase = read_i32(this, ACTOR_PHASE);
    let gap = TEAM_CURSOR_PHASE_END - phase;
    if gap == 0 {
        return;
    }
    let step = ((gap as i64 * 0x3333) >> 16) as i32;
    let next = if step.abs() >= 0x400 {
        phase + step
    } else if gap.abs() < 0x400 {
        TEAM_CURSOR_PHASE_END
    } else {
        phase + if gap > 0 { 0x400 } else { -0x400 }
    };
    write_i32(this, ACTOR_PHASE, next);
}

unsafe fn blit(this: *mut u8) {
    let flags = read_i32(this, ACTOR_FLAGS);
    let sprite_id = read_i32(this, ACTOR_SPRITE);
    if sprite_id == 0 || sprite_id == CUSTOM_SPRITE_SENTINEL {
        return;
    }
    if (flags & (ACTOR_FLAG_SCREEN_FILL | ACTOR_FLAG_SCREEN_FRONT)) != 0 {
        crate::overlay::push(
            read_i32(this, ACTOR_SPRITE),
            read_i32(this, ACTOR_PHASE) as u32,
            (flags & ACTOR_FLAG_SCREEN_FILL) != 0,
        );
        if (flags & ACTOR_FLAG_SCREEN_FRONT) != 0 {
            let world = read_i32(this, ENT_WORLD) as *mut u8;
            if !world.is_null() {
                let display = *(world.add(GAME_WORLD_DISPLAY) as *const *mut u8);
                crate::overlay::present(display);
            }
        }
        return;
    }
    let world = read_i32(this, ENT_WORLD) as *mut u8;
    if world.is_null() {
        return;
    }
    let rq = *(world.add(GAME_WORLD_RENDER_QUEUE) as *const *mut u8);
    if rq.is_null() {
        return;
    }
    let x = read_i32(this, ENT_POS_X);
    let y = read_i32(this, ENT_POS_Y);
    let w = read_i32(this, ACTOR_SPR_W);
    let h = read_i32(this, ACTOR_SPR_H);
    let feet_x0 = read_i32(this, ACTOR_FEET_X0);
    let feet_x1 = read_i32(this, ACTOR_FEET_X1);
    let feet_y = read_i32(this, ACTOR_FEET_Y);
    let origin = read_i32(this, ACTOR_ORIGIN);
    let (blit_x, blit_y) = if origin == ACTOR_ORIGIN_CENTER {
        (x, y)
    } else if w > 0 && h > 0 && feet_x1 >= feet_x0 {
        let cx = (feet_x0 + feet_x1) / 2;
        (
            x.wrapping_sub((cx - w / 2) << 16),
            y.wrapping_sub((feet_y - h / 2) << 16),
        )
    } else if origin == ACTOR_ORIGIN_BOTTOM && h > 0 {
        (x, y.wrapping_sub((h / 2) << 16))
    } else {
        (x, y)
    };
    let sprite = read_i32(this, ACTOR_SPRITE) as u32
        | if (flags & ACTOR_FLAG_MIRROR_X) != 0 {
            SPRITE_MIRROR_X
        } else {
            0
        };
    let anim = read_i32(this, ACTOR_PHASE) as u32;
    draw_sprite_local(rq, 0x0005_0001, blit_x, blit_y, sprite, anim, this);
    draw_puffs(this);
}

#[allow(dead_code)]
pub(crate) unsafe fn enqueue_sprite(
    world: *mut u8,
    x: i32,
    y: i32,
    sprite: u32,
    anim: u32,
) {
    if world.is_null() {
        return;
    }
    let rq = *(world.add(GAME_WORLD_RENDER_QUEUE) as *const *mut u8);
    if rq.is_null() {
        return;
    }
    draw_sprite_local(rq, 0x0005_0001, x, y, sprite, anim, std::ptr::null_mut());
}

unsafe fn apply_gravity(this: *mut u8) {
    let world = read_i32(this, ENT_WORLD) as *mut u8;
    if world.is_null() {
        return;
    }
    let cm = *(world.add(GAME_WORLD_COLLISION_MANAGER) as *const *mut u8);
    if cm.is_null() {
        return;
    }
    let g: ApplyGravityFn = std::mem::transmute(rb(APPLY_PROJECTILE_GRAVITY) as usize);
    g(cm, this);
}

/// bits: 1 terrain, 2 worm, 4 land edge, 8 left ground
unsafe fn step_move(this: *mut u8, collide_terrain: bool, _collide_worms: bool) -> i32 {
    let x = read_i32(this, ENT_POS_X).wrapping_add(read_i32(this, ENT_SPEED_X));
    let y = read_i32(this, ENT_POS_Y).wrapping_add(read_i32(this, ENT_SPEED_Y));
    write_i32(this, ENT_POS_X, x);
    write_i32(this, ENT_POS_Y, y);
    let bounce_pct = cluster_get(this as u32)
        .map(|c| c.bounce_pct)
        .unwrap_or(0);
    let on_terrain = collide_terrain && terrain_under_feet(this);
    if on_terrain && bounce_pct > 0 {
        return bounce_step(this, bounce_pct);
    }
    let was_contact = read_i32(this, ACTOR_CONTACT) != 0;
    let land_now = on_terrain && !was_contact;
    let left_ground = !on_terrain && was_contact;
    if on_terrain {
        if land_now {
            write_i32(this, ACTOR_CONTACT, 1);
        }
    } else {
        write_i32(this, ACTOR_CONTACT, 0);
    }
    let mut bits = 0;
    if on_terrain {
        bits |= 1;
    }
    if land_now {
        bits |= 4;
    }
    if left_ground {
        bits |= 8;
    }
    bits
}

unsafe fn bounce_step(this: *mut u8, bounce_pct: i32) -> i32 {
    let vx = read_i32(this, ENT_SPEED_X);
    let vy = read_i32(this, ENT_SPEED_Y);
    let mag_y = vy.unsigned_abs() as i32;
    let bounced = mag_y.saturating_mul(bounce_pct.clamp(0, 200)) / 100;
    let bounced = bounced.saturating_mul(7) / 8;
    let nx = vx.saturating_mul(bounce_pct.clamp(0, 200)) / 100;
    let ny = -bounced.max(0x8_000);
    write_i32(this, ENT_SPEED_X, nx);
    write_i32(this, ENT_SPEED_Y, ny);
    let x = read_i32(this, ENT_POS_X);
    let y = lift_clear(x, read_i32(this, ENT_POS_Y));
    write_i32(this, ENT_POS_Y, y);
    let rest = nx.unsigned_abs() < 0xC000 && bounced < 0x1_4000;
    write_i32(this, ACTOR_CONTACT, if rest { 1 } else { 0 });
    let mut bits = 1;
    if rest {
        bits |= 4;
    }
    bits
}

unsafe fn advance_anim(this: *mut u8) {
    let delta = read_i32(this, ACTOR_DELTA) as u32;
    if delta != 0 {
        let phase = this.add(ACTOR_PHASE) as *mut u32;
        *phase = (*phase).wrapping_add(delta);
    } else {
        sprite::advance_loop_id(this, read_i32(this, ACTOR_SPRITE), ACTOR_PHASE);
    }
}

fn native_gravity(handle: u32) {
    let this = handle as *mut u8;
    unsafe {
        if is_actor(this) {
            write_i32(this, ENT_GRAVITY_SCALE, 0x10000);
            apply_gravity(this);
        }
    }
}

fn native_move(handle: u32, terrain: bool, worms: bool) -> i32 {
    let this = handle as *mut u8;
    unsafe {
        if !is_actor(this) || read_i32(this, ACTOR_DEAD) != 0 {
            return 0;
        }
        step_move(this, terrain, worms)
    }
}

fn native_advance(handle: u32) {
    let this = handle as *mut u8;
    unsafe {
        if is_actor(this) {
            advance_anim(this);
        }
    }
}

fn native_look(handle: u32, id: i32, delta: u32, w: i32, h: i32) {
    let this = handle as *mut u8;
    unsafe {
        if !is_actor(this) || read_i32(this, ACTOR_DEAD) != 0 {
            return;
        }
        write_i32(this, ACTOR_SPRITE, id);
        write_i32(this, ACTOR_DELTA, delta as i32);
        write_i32(this, ACTOR_SPR_W, w);
        write_i32(this, ACTOR_SPR_H, h);
        write_i32(this, ACTOR_PHASE, 0);
        if let Some(feet) = sprite::feet_for_id(id) {
            write_i32(this, ACTOR_FEET_X0, feet.x0);
            write_i32(this, ACTOR_FEET_X1, feet.x1);
            write_i32(this, ACTOR_FEET_Y, feet.y);
        }
    }
}

fn native_set_frame(handle: u32, frame: i32) {
    let this = handle as *mut u8;
    unsafe {
        if !is_actor(this) || read_i32(this, ACTOR_DEAD) != 0 {
            return;
        }
        let id = read_i32(this, ACTOR_SPRITE);
        let Some(n) = sprite::frame_count(id) else {
            return;
        };
        if let Some(phase) = gfx::frame_phase(frame, n) {
            write_i32(this, ACTOR_PHASE, phase as i32);
            write_i32(this, ACTOR_DELTA, 0);
        }
    }
}

fn remember_live(handle: u32) {
    if handle == 0 {
        return;
    }
    if let Ok(mut g) = LIVE.lock() {
        if !g.contains(&handle) {
            g.push(handle);
        }
    }
}

fn forget_live(handle: u32) {
    if let Ok(mut g) = LIVE.lock() {
        g.retain(|h| *h != handle);
    }
    if let Ok(mut g) = CHILD_EMITS.lock() {
        g.retain(|(h, _)| *h != handle);
    }
    if let Ok(mut g) = TRAILS.lock() {
        g.retain(|(h, _)| *h != handle);
    }
    if let Ok(mut g) = CLUSTERS.lock() {
        g.retain(|(h, _)| *h != handle);
    }
}

pub(crate) fn live_handles() -> Vec<u32> {
    LIVE.lock().map(|g| g.clone()).unwrap_or_default()
}

pub(crate) fn list_worms_pub(from: u32) -> Vec<u32> {
    native_list_worms(from)
}

fn remember_child(handle: u32, emit: AttachedSpawn) {
    if handle == 0 {
        return;
    }
    if let Ok(mut g) = CHILD_EMITS.lock() {
        g.retain(|(h, _)| *h != handle);
        g.push((handle, emit));
    }
}

fn take_child(handle: u32) -> Option<AttachedSpawn> {
    let mut g = CHILD_EMITS.lock().ok()?;
    let i = g.iter().position(|(h, _)| *h == handle)?;
    Some(g.remove(i).1)
}

/// Spawn a LuaActor from an explode / proximity / place attachment.
pub fn emit_attached(
    owner: *mut u8,
    x: i32,
    y: i32,
    emit: &AttachedSpawn,
    inherit_sprite: i32,
) -> Option<u32> {
    let depth = EMIT_DEPTH.fetch_add(1, Ordering::SeqCst);
    if depth >= 8 {
        EMIT_DEPTH.fetch_sub(1, Ordering::SeqCst);
        log_error("attached spawn depth 8, stopping");
        return None;
    }
    let cluster = cluster_of(emit);
    let visible = emit.spawn.visible;
    let sprite = if !visible || emit.sprite == CUSTOM_SPRITE_SENTINEL {
        CUSTOM_SPRITE_SENTINEL
    } else if cluster.sprite != 0 {
        cluster.sprite
    } else if emit.sprite != 0 {
        emit.sprite
    } else if let Some(path) = &emit.path {
        sprite::lookup_key(path)
            .map(|r| r.id)
            .unwrap_or(inherit_sprite)
    } else if inherit_sprite != 0 {
        inherit_sprite
    } else {
        CUSTOM_SPRITE_SENTINEL
    };
    let delta = if emit.delta != 0 {
        emit.delta
    } else {
        sprite::actor_loop_delta(sprite)
    };
    let n = cluster.count.clamp(1, 16);
    let trail = trail_sprite(&cluster, n);
    // Stack every bit above the blast (WA Y+ is down). Side offsets made the
    // explosion shove outer bits flat horizontally — keep X shared so knock
    // lifts the whole bouquet into arcs.
    let base_y = {
        let before = y;
        let lifted = lift_clear(x, y);
        if lifted != before {
            log_line(&format!("attached lift y {before:#x} -> {lifted:#x}"));
        }
        lifted
    };
    let mut bit_spawn = emit.spawn.clone();
    if let Some(c) = bit_spawn.cluster.as_mut() {
        c.count = 1;
        c.trail = trail;
        c.trail_path = cluster.trail_path.clone();
        c.sprite = sprite;
        if !c.impact && c.bounce_pct == 0 {
            c.bounce_pct = 100;
        }
    } else if cluster_needs_live(&cluster) {
        let mut c = cluster.clone();
        c.count = 1;
        c.sprite = sprite;
        c.trail = trail;
        c.trail_path = cluster.trail_path.clone();
        bit_spawn.cluster = Some(c);
    }
    let mut first = None;
    let mut last_err = None;
    for i in 0..n {
        // Stack upward (2px) with a tiny X jitter so the blast fans arcs
        // left/right — never park bits out on the flanks (±9px max at n=10).
        let ox = (i * 2 - (n - 1)) * 0x10000;
        let by = base_y.wrapping_sub((i + 1) * 0x2_0000);
        // Rest: CreateExplosion's blast walk supplies the upward fan.
        let (vx, vy) = (0, 0);
        let result = unsafe {
            spawn_lua_actor(SpawnBody {
                owner,
                x: x.wrapping_add(ox),
                y: by,
                vx,
                vy,
                sprite,
                delta,
                spawn: bit_spawn.clone(),
            })
        };
        match result {
            Ok(handle) => unsafe {
                if first.is_none() {
                    first = Some(handle);
                }
                let p = handle as *mut u8;
                write_i32(p, ENT_WIND_SCALE, 0);
                if trail != 0 {
                    remember_trail(handle, trail);
                    drop_puff(p);
                }
            },
            Err(e) => last_err = Some(e),
        }
    }
    EMIT_DEPTH.fetch_sub(1, Ordering::SeqCst);
    if let Some(e) = last_err {
        log_error(&format!("attached spawn: {e}"));
    }
    first
}

fn cluster_of(emit: &AttachedSpawn) -> ClusterSpec {
    let mut c = emit.spawn.cluster.clone().unwrap_or(ClusterSpec {
        count: emit.count,
        spread: emit.spread,
        power: emit.power,
        sprite: emit.sprite,
        trail: emit.trail,
        trail_path: emit.trail_path.clone(),
        bounce_pct: 0,
        bounces: 0,
        fuse_ms: 0,
        proximity: 0,
        homing: ClusterHoming::Off,
        impact: emit.spawn.explode.is_some(),
    });
    if c.count <= 1 {
        c.count = emit.count.max(1);
    }
    if c.spread == 0 {
        c.spread = emit.spread;
    }
    if c.power == 0 {
        c.power = emit.power;
    }
    if c.trail == 0 {
        c.trail = emit.trail;
    }
    if c.trail_path.is_none() {
        c.trail_path = emit.trail_path.clone();
    }
    if c.sprite == 0 {
        c.sprite = emit.sprite;
    }
    // Bounce bits without an explicit percent get grenade-like restitution.
    if !c.impact && c.bounce_pct == 0 {
        c.bounce_pct = 100;
    }
    c
}

fn cluster_needs_live(c: &ClusterSpec) -> bool {
    c.count > 1
        || c.trail != 0
        || c.trail_path.is_some()
        || c.bounce_pct > 0
        || c.bounces > 0
        || c.fuse_ms > 0
        || c.proximity > 0
        || c.homing != ClusterHoming::Off
        || !c.impact
}

fn bit_vel(i: i32, n: i32, emit: &AttachedSpawn, cluster: &ClusterSpec) -> (i32, i32) {
    if n <= 1 {
        return (emit.vx, emit.vy);
    }
    let mid = (n - 1).max(1);
    let k = i * 2 - (n - 1);
    let spread = if cluster.spread != 0 {
        cluster.spread
    } else {
        45
    };
    let power = if cluster.power > 0 {
        cluster.power
    } else if emit.vy < 0 {
        -emit.vy
    } else if n > 1 {
        ClusterSpec::STOCK_POWER
    } else {
        0
    };
    if power == 0 {
        return (0, 0);
    }
    // Stock CreateClusters: fixed up ± spread, speed along the ray (not straight up + x kick).
    let deg = 90.0 + (spread as f32) * (k as f32) / (mid as f32);
    let rad = deg.to_radians();
    let vx = (power as f32 * rad.cos()).round() as i32;
    let vy = -(power as f32 * rad.sin()).round() as i32;
    (vx, vy)
}

unsafe fn apply_explosion_knock(this: *mut u8, data: *mut u8) {
    if this.is_null() || data.is_null() {
        return;
    }
    let args = [
        this as u32,
        read_i32(data, 0xC) as u32,
        read_i32(data, 0x10) as u32,
        read_i32(data, 4) as u32,
        read_i32(data, 8) as u32,
        rb(APPLY_EXPLOSION_KNOCK),
    ];
    core::arch::asm!(
        "push edi",
        "mov edi, dword ptr [{args}]",
        "push dword ptr [{args} + 16]",
        "push dword ptr [{args} + 12]",
        "push dword ptr [{args} + 8]",
        "push dword ptr [{args} + 4]",
        "call dword ptr [{args} + 20]",
        "pop edi",
        args = in(reg) args.as_ptr(),
        out("eax") _,
        out("ecx") _,
        out("edx") _,
    );
}

fn trail_sprite(cluster: &ClusterSpec, n: i32) -> i32 {
    if let Some(path) = cluster.trail_path.as_deref() {
        if let Some(s) = sprite::lookup_key(path) {
            return s.id;
        }
    }
    if cluster.trail == 130 || (cluster.trail == 0 && n > 1) {
        SPRITE_SMOKE
    } else {
        cluster.trail
    }
}

fn remember_trail(handle: u32, sprite: i32) {
    if handle == 0 || sprite == 0 {
        return;
    }
    if let Ok(mut g) = TRAILS.lock() {
        g.retain(|(h, _)| *h != handle);
        g.push((handle, sprite));
    }
}

fn remember_cluster(handle: u32, spec: &ClusterSpec) {
    if handle == 0 {
        return;
    }
    let bounce_pct = if spec.bounce_pct > 0 {
        spec.bounce_pct
    } else if !spec.impact {
        100
    } else {
        0
    };
    let fuse_frames = if spec.fuse_ms > 0 {
        // WA logic is 50 fps.
        (spec.fuse_ms.max(1) * 50 + 999) / 1000
    } else {
        0
    };
    if let Ok(mut g) = CLUSTERS.lock() {
        g.retain(|(h, _)| *h != handle);
        g.push((
            handle,
            ClusterLive {
                bounce_pct,
                bounces_left: spec.bounces.max(0),
                fuse_frames,
                proximity: spec.proximity.max(0),
                homing: spec.homing,
                impact: spec.impact,
                seek: false,
            },
        ));
    }
}

fn cluster_get(handle: u32) -> Option<ClusterLive> {
    let g = CLUSTERS.lock().ok()?;
    g.iter().find(|(h, _)| *h == handle).map(|(_, c)| *c)
}

fn cluster_update(handle: u32, f: impl FnOnce(&mut ClusterLive)) {
    if let Ok(mut g) = CLUSTERS.lock() {
        if let Some((_, c)) = g.iter_mut().find(|(h, _)| *h == handle) {
            f(c);
        }
    }
}

unsafe fn apply_cluster_timers(this: *mut u8) {
    let Some(live) = cluster_get(this as u32) else {
        return;
    };
    if live.fuse_frames > 0 {
        let left = live.fuse_frames - 1;
        cluster_update(this as u32, |c| c.fuse_frames = left);
        if left == 0 {
            explode(this);
            return;
        }
    }
    if live.proximity > 0 && worm_in_proximity(this, live.proximity) {
        explode(this);
    }
}

unsafe fn worm_in_proximity(this: *mut u8, radius_px: i32) -> bool {
    let owner = read_i32(this, ACTOR_OWNER) as u32;
    let x = read_i32(this, ENT_POS_X);
    let y = read_i32(this, ENT_POS_Y);
    let r = (radius_px as i64) << 16;
    let r2 = r.saturating_mul(r);
    for w in list_worms(this) {
        if w == 0 || w == owner || w == this as u32 {
            continue;
        }
        let p = w as *mut u8;
        let dx = (read_i32(p, ENT_POS_X) as i64) - (x as i64);
        let dy = (read_i32(p, ENT_POS_Y) as i64) - (y as i64);
        if dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy)) <= r2 {
            return true;
        }
    }
    false
}

unsafe fn apply_cluster_homing(this: *mut u8) {
    let Some(live) = cluster_get(this as u32) else {
        return;
    };
    if live.homing == ClusterHoming::Off {
        return;
    }
    if !live.seek {
        // Still climbing from the mine blast / launch. Don't steer until the apex.
        if read_i32(this, ENT_SPEED_Y) < 0 {
            return;
        }
        cluster_arm_seek(this as u32);
    }
    let target = match live.homing {
        ClusterHoming::Off => return,
        ClusterHoming::Aim => cluster_aim_xy(this),
        ClusterHoming::Worm => cluster_worm_xy(this),
    };
    let Some((tx, ty)) = target else {
        return;
    };
    steer_toward(this, tx, ty);
}

fn cluster_arm_seek(handle: u32) {
    if let Ok(mut g) = CLUSTERS.lock() {
        if let Some((_, c)) = g.iter_mut().find(|(h, _)| *h == handle) {
            c.seek = true;
        }
    }
}

unsafe fn cluster_aim_xy(this: *mut u8) -> Option<(i32, i32)> {
    let owner = read_i32(this, ACTOR_OWNER) as *mut u8;
    if !owner.is_null() && read_i32(owner, ENT_CLASS_TYPE) == CLASS_WORM {
        let cx = read_i32(owner, WORM_CURSOR_X);
        let cy = read_i32(owner, WORM_CURSOR_Y);
        if cx != 0 || cy != 0 {
            return Some((cx, cy));
        }
    }
    let (px, py, _) = crate::world::pointer();
    if px != 0 || py != 0 {
        Some((px, py))
    } else {
        None
    }
}

unsafe fn cluster_worm_xy(this: *mut u8) -> Option<(i32, i32)> {
    let owner = read_i32(this, ACTOR_OWNER) as u32;
    let x = read_i32(this, ENT_POS_X);
    let y = read_i32(this, ENT_POS_Y);
    let mut best = None;
    let mut best_d = i64::MAX;
    for w in list_worms(this) {
        if w == 0 || w == owner || w == this as u32 {
            continue;
        }
        let p = w as *mut u8;
        let wx = read_i32(p, ENT_POS_X);
        let wy = read_i32(p, ENT_POS_Y);
        let dx = (wx as i64) - (x as i64);
        let dy = (wy as i64) - (y as i64);
        let d = dx.abs() + dy.abs();
        if d < best_d {
            best_d = d;
            best = Some((wx, wy));
        }
    }
    best
}

unsafe fn steer_toward(this: *mut u8, tx: i32, ty: i32) {
    let x = read_i32(this, ENT_POS_X);
    let y = read_i32(this, ENT_POS_Y);
    let vx = read_i32(this, ENT_SPEED_X);
    let vy = read_i32(this, ENT_SPEED_Y);
    let dx = (tx as i64) - (x as i64);
    let dy = (ty as i64) - (y as i64);
    let mag = dx.abs() + dy.abs();
    if mag == 0 {
        return;
    }
    let speed = (vx.unsigned_abs() as i64) + (vy.unsigned_abs() as i64);
    let speed = speed.max(0x1_0000);
    let want_vx = (speed * dx / mag) as i32;
    let want_vy = (speed * dy / mag) as i32;
    write_i32(this, ENT_SPEED_X, vx + (want_vx - vx) / 6);
    write_i32(this, ENT_SPEED_Y, vy + (want_vy - vy) / 6);
}

unsafe fn spawn_configured(body: SpawnBody) -> Result<u32, String> {
    if let Some(c) = body.spawn.cluster.as_ref() {
        if c.count > 1 {
            let emit = AttachedSpawn {
                sprite: if c.sprite != 0 { c.sprite } else { body.sprite },
                delta: body.delta,
                vx: body.vx,
                vy: body.vy,
                count: c.count,
                spread: c.spread,
                power: c.power,
                trail: c.trail,
                trail_path: c.trail_path.clone(),
                path: None,
                spawn: body.spawn.clone(),
            };
            return emit_fan(body.owner, body.x, body.y, &emit, 0)
                .ok_or_else(|| "cluster spawn failed".into());
        }
    }
    spawn_lua_actor(body)
}

/// Self-launched cluster fan (on_fire / at launch): stock CreateClusters rays.
pub fn emit_fan(
    owner: *mut u8,
    x: i32,
    y: i32,
    emit: &AttachedSpawn,
    inherit_sprite: i32,
) -> Option<u32> {
    let depth = EMIT_DEPTH.fetch_add(1, Ordering::SeqCst);
    if depth >= 8 {
        EMIT_DEPTH.fetch_sub(1, Ordering::SeqCst);
        log_error("attached spawn depth 8, stopping");
        return None;
    }
    let cluster = cluster_of(emit);
    let visible = emit.spawn.visible;
    let sprite = if !visible || emit.sprite == CUSTOM_SPRITE_SENTINEL {
        CUSTOM_SPRITE_SENTINEL
    } else if cluster.sprite != 0 {
        cluster.sprite
    } else if emit.sprite != 0 {
        emit.sprite
    } else if let Some(path) = &emit.path {
        sprite::lookup_key(path)
            .map(|r| r.id)
            .unwrap_or(inherit_sprite)
    } else if inherit_sprite != 0 {
        inherit_sprite
    } else {
        CUSTOM_SPRITE_SENTINEL
    };
    let delta = if emit.delta != 0 {
        emit.delta
    } else {
        sprite::actor_loop_delta(sprite)
    };
    let n = cluster.count.clamp(1, 16);
    let trail = trail_sprite(&cluster, n);
    let y = lift_clear(x, y);
    let mut bit_spawn = emit.spawn.clone();
    if let Some(c) = bit_spawn.cluster.as_mut() {
        c.count = 1;
        c.trail = trail;
        c.trail_path = cluster.trail_path.clone();
        c.sprite = sprite;
        if !c.impact && c.bounce_pct == 0 {
            c.bounce_pct = 100;
        }
    } else if cluster_needs_live(&cluster) {
        let mut c = cluster.clone();
        c.count = 1;
        c.sprite = sprite;
        c.trail = trail;
        c.trail_path = cluster.trail_path.clone();
        bit_spawn.cluster = Some(c);
    }
    let mut first = None;
    let mut last_err = None;
    for i in 0..n {
        let (vx, vy) = bit_vel(i, n, emit, &cluster);
        let result = unsafe {
            spawn_lua_actor(SpawnBody {
                owner,
                x,
                y,
                vx,
                vy,
                sprite,
                delta,
                spawn: bit_spawn.clone(),
            })
        };
        match result {
            Ok(handle) => unsafe {
                if first.is_none() {
                    first = Some(handle);
                }
                let p = handle as *mut u8;
                write_i32(p, ENT_WIND_SCALE, 0);
                if trail != 0 {
                    remember_trail(handle, trail);
                    drop_puff(p);
                }
            },
            Err(e) => last_err = Some(e),
        }
    }
    EMIT_DEPTH.fetch_sub(1, Ordering::SeqCst);
    if let Some(e) = last_err {
        log_error(&format!("fan spawn: {e}"));
    }
    first
}

fn drop_puff(this: *mut u8) {
    let h = this as u32;
    let sprite = {
        let Ok(g) = TRAILS.lock() else {
            return;
        };
        g.iter().find(|(id, _)| *id == h).map(|(_, s)| *s)
    };
    let Some(sprite) = sprite else {
        return;
    };
    let x = unsafe { read_i32(this, ENT_POS_X) };
    let y = unsafe { read_i32(this, ENT_POS_Y) };
    if let Ok(mut g) = PUFFS.lock() {
        if g.len() > 80 {
            g.remove(0);
        }
        g.push(SmokePuff {
            x,
            y,
            sprite,
            age: 0,
        });
    }
}

pub fn begin_puff_frame() {
    PUFF_DRAWN.store(false, Ordering::SeqCst);
}

pub fn tick_puffs() {
    PUFF_DRAWN.store(false, Ordering::SeqCst);
    if let Ok(mut g) = PUFFS.lock() {
        for p in g.iter_mut() {
            p.age = p.age.saturating_add(1);
        }
        g.retain(|p| p.age < PUFF_LIFE);
    }
}

pub fn draw_puffs(entity: *mut u8) {
    if entity.is_null() {
        return;
    }
    if PUFF_DRAWN.swap(true, Ordering::SeqCst) {
        return;
    }
    let world = unsafe {
        let w = read_i32(entity, ENT_WORLD) as *mut u8;
        if w.is_null() {
            entity
        } else {
            w
        }
    };
    let rq = unsafe { *(world.add(GAME_WORLD_RENDER_QUEUE) as *const *mut u8) };
    if rq.is_null() {
        PUFF_DRAWN.store(false, Ordering::SeqCst);
        return;
    }
    let Ok(g) = PUFFS.lock() else {
        PUFF_DRAWN.store(false, Ordering::SeqCst);
        return;
    };
    for p in g.iter() {
        let anim = (p.age as u32) << 12;
        unsafe {
            draw_sprite_local(rq, 0x0005_0001, p.x, p.y, p.sprite as u32, anim, std::ptr::null_mut());
        }
    }
}

/// Hold one extra FRAME_FINISH so we never spawn inside the blast walk.
pub fn emit_attached_later(
    owner: *mut u8,
    x: i32,
    y: i32,
    emit: &AttachedSpawn,
    inherit_sprite: i32,
) {
    if owner.is_null() {
        log_error("attached spawn later: no owner");
        return;
    }
    if let Ok(mut g) = DEFERRED.lock() {
        g.push(DeferredEmit {
            owner: owner as u32,
            x,
            y,
            inherit: inherit_sprite,
            emit: emit.clone(),
            wait: 1,
        });
    }
}

pub fn flush_deferred() {
    let mut ready = Vec::new();
    if let Ok(mut g) = DEFERRED.lock() {
        let mut i = 0;
        while i < g.len() {
            if g[i].wait > 0 {
                g[i].wait -= 1;
                i += 1;
            } else {
                ready.push(g.remove(i));
            }
        }
    }
    for item in ready {
        emit_attached(
            item.owner as *mut u8,
            item.x,
            item.y,
            &item.emit,
            item.inherit,
        );
    }
}

/// WA Y+ is down. Walk up until the pixel is air, then a few more so feet clear the lip.
fn lift_clear(x: i32, y: i32) -> i32 {
    const PX: i32 = 0x10000;
    let mut py = y;
    for _ in 0..64 {
        if !crate::world::solid_fixed(x, py) {
            break;
        }
        py = py.wrapping_sub(PX);
    }
    // Always pop out of the crater lip, even when the origin pixel already reads as air.
    py.wrapping_sub(PX * 16)
}

/// CreateExplosion ESI must be a stock WorldEntity (worm/mine/missile).
/// LuaActor is 0x140 with a custom vtable; the blast walk then hits a null.
pub fn stock_owner(from: *mut u8, fallback: u32) -> *mut u8 {
    unsafe {
        if let Some(&w) = list_worms(from).first() {
            return w as *mut u8;
        }
        if fallback != 0 {
            let p = fallback as *mut u8;
            if let Some(&w) = list_worms(p).first() {
                return w as *mut u8;
            }
            return p;
        }
        from
    }
}

fn stock_class(p: *mut u8) -> bool {
    if p.is_null() {
        return false;
    }
    let class = unsafe { read_i32(p, ENT_CLASS_TYPE) };
    class == CLASS_MINE || class == CLASS_MISSILE || class == CLASS_WORM
}

fn blast_sender(this: *mut u8) -> *mut u8 {
    if stock_class(this) {
        return this;
    }
    let owner = unsafe { read_i32(this, ACTOR_OWNER) } as *mut u8;
    if stock_class(owner) {
        return owner;
    }
    unsafe {
        if let Some(&w) = list_worms(this).first() {
            return w as *mut u8;
        }
        if let Some(&w) = list_worms(owner).first() {
            return w as *mut u8;
        }
    }
    if !owner.is_null() {
        owner
    } else {
        this
    }
}

fn emit_child(this: *mut u8, x: i32, y: i32) {
    let Some(emit) = take_child(this as u32) else {
        return;
    };
    let (owner, inherit) = unsafe {
        let o = read_i32(this, ACTOR_OWNER) as *mut u8;
        let owner = if o.is_null() { this } else { o };
        (owner, read_i32(this, ACTOR_SPRITE))
    };
    emit_attached_later(owner, x, y, &emit, inherit);
}

fn native_spawn(args: &SpawnArgs) -> Result<u32, String> {
    unsafe {
        spawn_configured(SpawnBody {
            owner: args.owner as *mut u8,
            x: args.x,
            y: args.y,
            vx: args.vx,
            vy: args.vy,
            sprite: args.sprite,
            delta: args.delta,
            spawn: args.spawn.clone(),
        })
    }
}

fn native_list_actors() -> Vec<u32> {
    LIVE.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Live worms for the determinism probe, walked by (team, index) rather than
/// by address so two runs enumerate them in the same order. Resolves the
/// tables from the global session, so it needs no entity to start from.
pub fn live_worms() -> Vec<u32> {
    unsafe { list_worms(std::ptr::null_mut()) }
}

/// Our own live actors, in spawn order: `LIVE` is a `Vec` and `retain`
/// preserves it, so this is reproducible where an address-keyed set would not
/// be.
pub fn live_actors() -> Vec<u32> {
    native_list_actors()
}

fn native_list_worms(from: u32) -> Vec<u32> {
    unsafe { list_worms(from as *mut u8) }
}

fn native_resolve_sprite(key: &str) -> Option<(i32, u32, i32, i32)> {
    let s = sprite::lookup_key(key)?;
    Some((s.id, s.delta, s.w, s.h))
}

unsafe fn list_worms(from: *mut u8) -> Vec<u32> {
    let (shared, world) = worm_tables(from);
    if shared.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for team in 0..MAX_TEAMS {
        for idx in 0..MAX_WORMS_PER_TEAM {
            if !world.is_null() {
                let hp_off = ARENA_WORM_HP
                    + (team as usize) * WORLD_TEAM_STRIDE
                    + (idx as usize) * ARENA_WORM_STRIDE;
                if read_i32(world, hp_off) <= 0 {
                    continue;
                }
            }
            let key_esi = team.wrapping_mul(16).wrapping_add(idx);
            if let Some(ptr) = lookup_in_table(shared, key_esi, SHARED_KEY_WORM) {
                out.push(ptr as u32);
            }
        }
    }
    out
}

unsafe fn worm_tables(from: *mut u8) -> (*mut u8, *mut u8) {
    if !from.is_null() {
        let shared = read_i32(from, ENT_SHARED_DATA) as *mut u8;
        let world = read_i32(from, ENT_WORLD) as *mut u8;
        if !shared.is_null() {
            return (shared, world);
        }
    }
    session_shared_world()
}

unsafe fn session_shared_world() -> (*mut u8, *mut u8) {
    let slot = rb(G_GAME_SESSION) as *const *mut u8;
    if slot.is_null() {
        return (std::ptr::null_mut(), std::ptr::null_mut());
    }
    let session = *slot;
    if session.is_null() {
        return (std::ptr::null_mut(), std::ptr::null_mut());
    }
    let runtime = *(session.add(GAME_SESSION_RUNTIME) as *const *mut u8);
    if runtime.is_null() {
        return (std::ptr::null_mut(), std::ptr::null_mut());
    }
    let world_root = *(runtime.add(GAME_RUNTIME_WORLD_ROOT) as *const *mut u8);
    if world_root.is_null() {
        return (std::ptr::null_mut(), std::ptr::null_mut());
    }
    (
        read_i32(world_root, ENT_SHARED_DATA) as *mut u8,
        read_i32(world_root, ENT_WORLD) as *mut u8,
    )
}

unsafe fn drowned(this: *mut u8, world: *mut u8) -> bool {
    let y_px = read_i32(this, ENT_POS_Y) >> 16;
    let water = read_i32(world, GAME_WORLD_WATER_LEVEL);
    let kill = read_i32(world, GAME_WORLD_WATER_KILL_Y);
    y_px >= water || (kill != 0 && y_px >= kill)
}

unsafe fn land(this: *mut u8) {
    if (read_i32(this, ACTOR_FLAGS) & ACTOR_FLAG_EXPLODE) != 0 {
        // Donkey / bounce-boom: persist + explode. Without an upward kick the crater
        // re-lands the bit in the same impact and burns bounces=N in a few frames.
        if let Some(live) = cluster_get(this as u32) {
            if live.bounces_left > 0 {
                let left = live.bounces_left - 1;
                cluster_update(this as u32, |c| c.bounces_left = left);
                if left == 0 {
                    let flags = read_i32(this, ACTOR_FLAGS);
                    explode_at(
                        this,
                        read_i32(this, ACTOR_DAMAGE),
                        read_i32(this, ACTOR_BOOM_ID),
                        (flags & ACTOR_FLAG_EXPLODE_BELOW) != 0,
                        (flags & ACTOR_FLAG_DAMAGE_TERRAIN) != 0,
                        (flags & ACTOR_FLAG_DAMAGE_WORMS) != 0,
                    );
                    die(this);
                    return;
                }
                // Boom without self-knock, then grenade-style bounce on the
                // impact velocity (own blast was shoving bits sideways).
                explode_no_self_knock(this);
                if read_i32(this, ACTOR_DEAD) == 0 {
                    let pct = if live.bounce_pct > 0 {
                        live.bounce_pct
                    } else {
                        100
                    };
                    bounce_step(this, pct);
                }
                return;
            }
        }
        explode(this);
        return;
    }
    // Custom cluster bits: bounce / fuse / prox — never instant-despawn on dirt.
    if let Some(live) = cluster_get(this as u32) {
        if live.bounce_pct > 0 {
            if live.bounces_left > 0 {
                let left = live.bounces_left - 1;
                cluster_update(this as u32, |c| c.bounces_left = left);
                if left == 0 {
                    explode(this);
                    return;
                }
            }
            bounce_step(this, live.bounce_pct);
            return;
        }
        // Drop-and-wait (fuse / proximity mine bits): sit until timer or worm.
        if live.fuse_frames > 0 || live.proximity > 0 {
            write_i32(this, ENT_SPEED_X, 0);
            write_i32(this, ENT_SPEED_Y, 0);
            return;
        }
    }
    if (read_i32(this, ACTOR_FLAGS) & ACTOR_FLAG_PERSIST) == 0 {
        die(this);
    }
}

unsafe fn explode(this: *mut u8) {
    let flags = read_i32(this, ACTOR_FLAGS);
    let persist = (flags & ACTOR_FLAG_PERSIST) != 0;
    explode_at(
        this,
        read_i32(this, ACTOR_DAMAGE),
        read_i32(this, ACTOR_BOOM_ID),
        (flags & ACTOR_FLAG_EXPLODE_BELOW) != 0,
        (flags & ACTOR_FLAG_DAMAGE_TERRAIN) != 0,
        (flags & ACTOR_FLAG_DAMAGE_WORMS) != 0,
    );
    if !persist {
        die(this);
    }
}

/// Terrain/worm boom still fires; this body ignores its own blast mass.
unsafe fn explode_no_self_knock(this: *mut u8) {
    let mass = read_i32(this, ENT_BLAST_MASS);
    write_i32(this, ENT_BLAST_MASS, 0);
    explode(this);
    if read_i32(this, ACTOR_DEAD) == 0 {
        write_i32(this, ENT_BLAST_MASS, mass);
    }
}

unsafe fn explode_at(
    this: *mut u8,
    damage: i32,
    explosion_id: i32,
    below: bool,
    damage_terrain: bool,
    damage_worms: bool,
) {
    let x = read_i32(this, ENT_POS_X);
    let mut y = read_i32(this, ENT_POS_Y);
    if below {
        y = y.wrapping_add(0x10000);
    }
    let boom = if damage_terrain {
        explosion_id as u32
    } else {
        0
    };
    let dmg = if damage_worms { damage as u32 } else { 0 };
    let sender = blast_sender(this);
    log_line(&format!(
        "LuaActor explode damage={dmg} id={boom} below={below} at {x:#x},{y:#x} sender={sender:?}"
    ));
    create_explosion(sender, x, y, boom, dmg, 0, 0);
    emit_child(this, x, y);
}

fn native_explode(handle: u32, damage: i32, explosion_id: i32, below: bool) {
    let this = handle as *mut u8;
    unsafe {
        if !is_actor(this) || read_i32(this, ACTOR_DEAD) != 0 {
            return;
        }
        explode_at(this, damage, explosion_id, below, true, true);
    }
}

fn native_despawn(handle: u32) {
    let this = handle as *mut u8;
    unsafe {
        if !is_actor(this) {
            return;
        }
        die(this);
    }
}

fn native_in_water(handle: u32) -> bool {
    let this = handle as *mut u8;
    unsafe {
        if !is_actor(this) {
            return false;
        }
        let world = read_i32(this, ENT_WORLD) as *mut u8;
        if world.is_null() {
            return false;
        }
        drowned(this, world)
    }
}

fn native_on_feet(handle: u32) -> bool {
    let this = handle as *mut u8;
    unsafe { is_actor(this) && terrain_under_feet(this) }
}

unsafe fn die(this: *mut u8) {
    if read_i32(this, ACTOR_DEAD) != 0 {
        return;
    }
    write_i32(this, ACTOR_DEAD, 1);
    log_line("LuaActor drown/despawn");
    let free: WorldFreeFn = std::mem::transmute(actor_free as *const ());
    let _ = free(this, 1);
}

unsafe extern "thiscall" fn actor_free(this: *mut u8, flags: u32) -> *mut u8 {
    if is_actor(this) {
        forget_live(this as u32);
        lua_host::actor_freed(this as u32);
        write_i32(this, ACTOR_MAGIC, 0);
        let world = read_i32(this, ENT_WORLD) as *mut u8;
        if !world.is_null() {
            let w = world.add(GAME_WORLD_MISSILE_WEIGHT) as *mut i32;
            unsafe {
                *w = (*w).saturating_sub(7);
            }
        }
    }
    let orig = ORIG_FREE.load(Ordering::SeqCst);
    if orig.is_null() {
        return this;
    }
    let f: WorldFreeFn = unsafe { std::mem::transmute(orig) };
    unsafe { f(this, flags) }
}

/// Solid collision under the sprite's bottom opaque row.
unsafe fn terrain_under_feet(this: *mut u8) -> bool {
    let flags = read_i32(this, ACTOR_FLAGS);
    if (flags & ACTOR_FLAG_MASK) != 0 && (flags & ACTOR_FLAG_MASK_LAND) != 0 {
        if crate::mask_collide::mask_land_support(this) {
            return true;
        }
        // Fall through to feet bar when silhouette contour has no dirt contact
        // yet (e.g. still airborne) — keeps land events working on drop-in.
    }
    let world = read_i32(this, ENT_WORLD) as *mut u8;
    if world.is_null() {
        return false;
    }
    let x = read_i32(this, ENT_POS_X);
    let y = read_i32(this, ENT_POS_Y).wrapping_add(0x10000);
    let x0 = read_i32(this, ACTOR_FEET_X0);
    let x1 = read_i32(this, ACTOR_FEET_X1);
    if x1 < x0 {
        return crate::world::solid_fixed(x, y);
    }
    let cx = (x0 + x1) / 2;
    for px in x0..=x1 {
        let wx = x.wrapping_add((px - cx) << 16);
        if crate::world::solid_fixed(wx, y) {
            return true;
        }
    }
    false
}

#[allow(dead_code)]
unsafe fn try_move(this: *mut u8, x: i32, y: i32) -> u32 {
    let f = rb(WORLD_ENTITY_TRY_MOVE) as u32;
    let args = [this as u32, y as u32, x as u32, f];
    let ret: u32;
    // Pin in EBX: `in(reg)` can pick ESI, then `mov esi,[p]` makes `{p}+12` read this+12 (often 0 → EIP 0).
    core::arch::asm!(
        "push esi",
        "push edi",
        "mov esi, dword ptr [ebx]",
        "mov edi, dword ptr [ebx + 4]",
        "push dword ptr [ebx + 8]",
        "call dword ptr [ebx + 12]",
        "pop edi",
        "pop esi",
        in("ebx") args.as_ptr(),
        lateout("eax") ret,
        out("ecx") _,
        out("edx") _,
    );
    ret
}

unsafe fn draw_sprite_local(
    queue: *mut u8,
    layer: u32,
    x: i32,
    y: i32,
    sprite: u32,
    anim: u32,
    actor: *mut u8,
) {
    if crate::gpu::intercept(layer, x, y, sprite, anim, actor) {
        return;
    }
    let f = rb(RQ_DRAW_SPRITE_LOCAL) as u32;
    let args = [queue as u32, layer, x as u32, y as u32, sprite, anim, f];
    core::arch::asm!(
        "mov eax, dword ptr [ebx + 12]",
        "mov ecx, dword ptr [ebx]",
        "push dword ptr [ebx + 20]",
        "push dword ptr [ebx + 16]",
        "push dword ptr [ebx + 8]",
        "push dword ptr [ebx + 4]",
        "call dword ptr [ebx + 24]",
        in("ebx") args.as_ptr(),
        out("eax") _,
        out("ecx") _,
        out("edx") _,
    );
}

unsafe fn create_explosion(
    sender: *mut u8,
    x: i32,
    y: i32,
    explosion_id: u32,
    damage: u32,
    caller_flag: u32,
    owner_id: u32,
) {
    let f = rb(CREATE_EXPLOSION) as u32;
    let args = [
        sender as u32,
        explosion_id,
        damage,
        caller_flag,
        owner_id,
        f,
        x as u32,
        y as u32,
    ];
    core::arch::asm!(
        "push esi",
        "mov esi, dword ptr [ebx]",
        "mov eax, dword ptr [ebx + 24]",
        "mov ecx, dword ptr [ebx + 28]",
        "push dword ptr [ebx + 16]",
        "push dword ptr [ebx + 12]",
        "push dword ptr [ebx + 8]",
        "push dword ptr [ebx + 4]",
        "call dword ptr [ebx + 20]",
        "pop esi",
        in("ebx") args.as_ptr(),
        out("eax") _,
        out("ecx") _,
        out("edx") _,
    );
}
