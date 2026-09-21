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
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, OnceLock};
use weapons::{ActorSpawn, ScreenBlit, SpawnFrom, SpriteOrigin};

type StrikeFireFn = unsafe extern "system" fn(*mut u8, *mut u8, *mut u8);
type CreateProjFn = unsafe extern "thiscall" fn(*mut u8, *mut u8, *mut u8);
type WorldCtorFn = unsafe extern "system" fn(*mut u8, *mut u8, u32, u32);
type ApplyGravityFn = unsafe extern "thiscall" fn(*mut u8, *mut u8);
type WorldFreeFn = unsafe extern "thiscall" fn(*mut u8, u32) -> *mut u8;
type WorldMsgFn = unsafe extern "thiscall" fn(*mut u8, u32, u32, u32, *mut u8);
type CrtMallocFn = unsafe extern "C" fn(usize) -> *mut u8;

static ORIG_STRIKE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
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
}

static DROPS: Mutex<Vec<DropClaim>> = Mutex::new(Vec::new());
static FIRES: Mutex<Vec<FireClaim>> = Mutex::new(Vec::new());
static SELECTS: Mutex<Vec<SelectClaim>> = Mutex::new(Vec::new());
static LAST_SLOT: Mutex<Vec<(u32, i32)>> = Mutex::new(Vec::new());
static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

pub fn install(catalog: &Catalog) -> Result<(), String> {
    build_vtable();
    let va = catalog
        .function_va("FireWeapon__StrikeFire")
        .unwrap_or(STRIKE_FIRE);
    let trampoline = unsafe {
        MinHook::create_hook(rb(va) as *mut c_void, strike_fire_hook as *mut c_void)
            .map_err(|e| format!("StrikeFire hook: {e:?}"))?
    };
    ORIG_STRIKE.store(trampoline, Ordering::SeqCst);

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

    lua_host::bind_actor_native(lua_host::ActorNative {
        explode: native_explode,
        despawn: native_despawn,
        in_water: native_in_water,
        on_feet: native_on_feet,
        gravity: native_gravity,
        move_body: native_move,
        advance: native_advance,
        set_frame: native_set_frame,
        set_gfx: native_set_gfx,
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
        hide_worm: crate::physics::hide_worm,
        worm_hidden: crate::physics::worm_hidden,
        equip: crate::world::equip_weapon,
        hp: crate::world::worm_hp,
        ammo: crate::world::ammo,
        hurt: crate::physics::hit_worm,
    });
    log_line("LuaActor: StrikeFire + CreateWeaponProjectile wrap; land/objects/sound");
    Ok(())
}

pub fn claim_drop(slot: usize, sprite: i32, delta: u32, spawn: Option<ActorSpawn>, click_id: u32) {
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

pub fn claim_fire(slot: usize, sprite: i32, delta: u32, spawn: Option<ActorSpawn>, fire_id: u32) {
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

pub fn claim_select(slot: usize, select_id: u32) {
    if select_id == 0 {
        return;
    }
    if let Ok(mut g) = SELECTS.lock() {
        g.retain(|c| c.slot != slot);
        g.push(SelectClaim { slot, select_id });
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
    let spawned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        try_spawn_drop(worm, ctx)
    }));
    match spawned {
        Ok(true) => return,
        Ok(false) => {}
        Err(p) => log_error(&format!("drop spawn panic: {}", panic_text(p))),
    }
    let orig = ORIG_STRIKE.load(Ordering::SeqCst);
    if !orig.is_null() {
        let f: StrikeFireFn = unsafe { std::mem::transmute(orig) };
        unsafe { f(worm, fire_params, ctx) };
    }
}

unsafe extern "thiscall" fn create_proj_hook(worm: *mut u8, fire_params: *mut u8, ctx: *mut u8) {
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
    match spawn_lua_actor(SpawnBody {
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
    match spawn_lua_actor(SpawnBody {
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
    let wx = unsafe { read_i32(worm, WORM_CURSOR_X) };
    let wy = unsafe { read_i32(worm, WORM_CURSOR_Y) };
    if (wx != 0 || wy != 0) || ctx.is_null() {
        (wx, wy)
    } else {
        (unsafe { read_i32(ctx, CTX_CURSOR_X) }, unsafe {
            read_i32(ctx, CTX_CURSOR_Y)
        })
    }
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
    // A zeroed allocation would otherwise mean "scale 0", i.e. invisible.
    write_i32(this, ACTOR_SCALE, 0x1_0000);
    write_i32(this, ACTOR_TINT, 0xFFFF_FFFFu32 as i32);
    // Behind terrain by default: the conservative choice, matching how a
    // stock sprite at this depth would behave.
    write_i32(this, ACTOR_DEPTH, 0x0019_0000);

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
    // Stock gravity leaf is `vy += cm_gravity * scale`. The GUI `gravity`
    // flag only auto-ticks; `a:gravity()` must still see a real scale.
    write_i32(this, ENT_GRAVITY_SCALE, 0x10000);
    write_i32(this, ENT_DAMP, 0x10000);
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
    write_i32(this, ACTOR_SPRITE, body.sprite);
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
    write_i32(this, ACTOR_OWNER, owner as i32);
    let (dmg, boom) = spawn.explode.map(|e| (e.damage, e.id)).unwrap_or((0, 0));
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
    if spawn.explode.is_some() {
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
    if spawn.explode.is_some_and(|e| e.below) {
        flags |= ACTOR_FLAG_EXPLODE_BELOW;
    }
    match spawn.screen {
        ScreenBlit::Fill => flags |= ACTOR_FLAG_SCREEN_FILL,
        ScreenBlit::Front => flags |= ACTOR_FLAG_SCREEN_FRONT,
        ScreenBlit::World => {}
    }
    write_i32(this, ACTOR_FLAGS, flags);
    write_i32(this, ACTOR_CONTACT, 0);
    write_i32(this, ACTOR_BRAIN, spawn.brain_id as i32);
    lua_host::actor_spawned(this as u32, spawn.brain_id);
    remember_live(this as u32);

    let world = read_i32(this, ENT_WORLD) as *mut u8;
    if !world.is_null() {
        let w = world.add(GAME_WORLD_MISSILE_WEIGHT) as *mut i32;
        *w = (*w).saturating_add(7);
    }
    log_line(&format!(
        "LuaActor ready helper={:#x} owner={:?}",
        read_i32(this, ENT_HELPER) as u32,
        owner
    ));
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
            MSG_EXPLOSION => {
                forward_message(this, sender, msg, size, data);
            }
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
    let auto_phys =
        (flags & (ACTOR_FLAG_GRAVITY | ACTOR_FLAG_COLLIDE_TERRAIN | ACTOR_FLAG_COLLIDE_WORMS)) != 0;
    if (flags & ACTOR_FLAG_GRAVITY) != 0 {
        apply_gravity(this);
    }
    let mut worm_hit = false;
    let mut land_now = false;
    let mut left_ground = false;
    if auto_phys {
        let hit = step_move(
            this,
            (flags & ACTOR_FLAG_COLLIDE_TERRAIN) != 0,
            (flags & ACTOR_FLAG_COLLIDE_WORMS) != 0,
        );
        worm_hit = hit & 2 != 0;
        land_now = hit & 4 != 0;
        left_ground = hit & 8 != 0;
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
    if y > 0x1800_0000 || y < -0x2000_0000 {
        die(this);
        return;
    }
    if (flags & ACTOR_FLAG_LOOP) != 0 {
        advance_anim(this);
    }
}

unsafe fn blit(this: *mut u8) {
    let flags = read_i32(this, ACTOR_FLAGS);
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
}

#[allow(dead_code)]
pub(crate) unsafe fn enqueue_sprite(world: *mut u8, x: i32, y: i32, sprite: u32, anim: u32) {
    if world.is_null() {
        return;
    }
    let rq = *(world.add(GAME_WORLD_RENDER_QUEUE) as *const *mut u8);
    if rq.is_null() {
        return;
    }
    // No actor: a bare enqueue carries no presentation state.
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
unsafe fn step_move(this: *mut u8, collide_terrain: bool, collide_worms: bool) -> i32 {
    let x = read_i32(this, ENT_POS_X).wrapping_add(read_i32(this, ENT_SPEED_X));
    let y = read_i32(this, ENT_POS_Y).wrapping_add(read_i32(this, ENT_SPEED_Y));
    let helper = read_i32(this, ENT_HELPER);
    let mut worm_hit = false;
    if collide_worms && helper != 0 {
        if try_move(this, x, y) == 0 {
            worm_hit = true;
        }
    } else {
        write_i32(this, ENT_POS_X, x);
        write_i32(this, ENT_POS_Y, y);
    }
    let on_terrain = collide_terrain && terrain_under_feet(this);
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
    if worm_hit {
        bits |= 2;
    }
    if land_now {
        bits |= 4;
    }
    if left_ground {
        bits |= 8;
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

/// Hold an actor on one frame of its sheet.
///
/// Disable the actor's phase delta so even frame folders hold their pose.
/// GetSpriteFrameForBlit reads anim_value as
/// `(phase * frames) >> 16`, which is the mapping overlay_frame uses, so this
/// inverts it. Lets one multi-frame sheet stand in for many sprites -- baked
/// rotations cost a slot each, frames of one sheet cost nothing.
/// Presentation state for the GPU renderer: rotation, scale and tint.
///
/// Writes only into our own actor struct and is read only by the renderer, so
/// it cannot reach the simulation and cannot desync a lockstep game. An actor
/// on the palette path ignores all of it.
fn native_set_gfx(handle: u32, angle: f32, scale: f32, tint: u32, depth: u32) {
    let this = handle as *mut u8;
    unsafe {
        if !is_actor(this) || read_i32(this, ACTOR_DEAD) != 0 {
            return;
        }
        let fixed = |v: f32| (v * 65536.0) as i32;
        write_i32(this, ACTOR_ANGLE, fixed(angle));
        // Guard against a pack asking for an invisible or absurd sprite.
        write_i32(this, ACTOR_SCALE, fixed(scale.clamp(0.01, 64.0)));
        write_i32(this, ACTOR_TINT, tint as i32);
        if depth != 0 {
            write_i32(this, ACTOR_DEPTH, depth as i32);
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
            // Explicit selection holds even when the loaded folder loops.
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
}

fn native_spawn(args: &SpawnArgs) -> Result<u32, String> {
    unsafe {
        spawn_lua_actor(SpawnBody {
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
        explode(this);
    } else if (read_i32(this, ACTOR_FLAGS) & ACTOR_FLAG_PERSIST) == 0 {
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
    log_line(&format!(
        "LuaActor explode damage={dmg} id={boom} below={below} at {x:#x},{y:#x}"
    ));
    create_explosion(this, x, y, boom, dmg, 0, 0);
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
    // The GPU renderer draws its own sprites as quads after WA's composite, so
    // they must not also be queued: a queued copy would be drawn by WA in
    // palette colour underneath ours, and would keep occupying one of the 223
    // shared sprite slots the GPU path exists to escape. Intercepting here
    // rather than in `blit` catches every producer -- blit, enqueue_sprite,
    // and anything added later -- in one place.
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
