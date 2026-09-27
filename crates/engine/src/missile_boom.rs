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
    MISSILE_WEAPON_ALT_SPRITE, MISSILE_WEAPON_DATA, MISSILE_WEAPON_SPRITE, STOCK_WEAPON_COUNT,
    WEAPON_ENTRY_SIZE, ENT_WORLD, ENT_WORM_TEAM, FIRE_DAMAGE, FIRE_EXPLODE_INTO, FIRE_FUSE,
    FIRE_PARAMS_SIZE, FIRE_SOUND,
    FIRE_SPRITE, GAME_WORLD_OBJECT_POOL, GAME_WORLD_WEAPON_TABLE, MISSILE_ENTITY_SIZE,
    MISSILE_ENTITY_ZEROED, OBJECT_POOL_LIMIT, OBJECT_POOL_MISSILE, SHOOT_DESC_SIZE, SHOOT_FUSE,
    SHOOT_SETTLE, SHOOT_TARGET_X, SHOOT_TARGET_Y, SHOOT_TEAM, SHOOT_VX, SHOOT_VY, SHOOT_WORM,
    SHOOT_X, SHOOT_Y, WA_MALLOC, WEAPON_FIRE_PARAMS, WORM_INDEX,
};
use std::collections::HashMap;
use minhook::MinHook;
use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};
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
/// Every live missile's weapon slot, by entity address, for `wa.on.missile_frame`.
static WEAPON_OF: Mutex<Option<HashMap<u32, usize>>> = Mutex::new(None);
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
    remember_weapon(p, fire_params);
    let Some(slot) = mine::slot_from_fire_params(p, fire_params).or_else(launching) else {
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

/// The slot whose weapon table entry holds `ptr`. A projectile's fire params
/// start at +0x3C of its entry; Armageddon's meteors pass +0x38.
fn slot_in_table(table: usize, ptr: usize) -> Option<usize> {
    let off = ptr.checked_sub(table)?;
    let slot = off / WEAPON_ENTRY_SIZE;
    (table != 0 && slot < STOCK_WEAPON_COUNT).then_some(slot)
}

/// Record which weapon a new missile belongs to. WA's missile constructor
/// callers (3.8.1) pass as `fire_params`:
/// - CreateWeaponProjectile: the weapon table entry + 0x3C;
/// - update_armageddon: the Armageddon entry + 0x38;
/// - CreateClusters: the parent missile's own copy (+0x15C), so the bits
///   take their parent's slot;
/// - AirStrikeEntity::release_missile: the strike's own copy, and
///   CrateEntity::detonate: a world buffer. These get no slot.
unsafe fn remember_weapon(missile: *mut u8, fire_params: *const i32) {
    let world = unsafe { read_i32(missile, game::offsets::ENT_WORLD) } as usize;
    let table = if world == 0 {
        0
    } else {
        unsafe { *((world + game::offsets::GAME_WORLD_WEAPON_TABLE) as *const usize) }
    };
    let Ok(mut g) = WEAPON_OF.lock() else {
        return;
    };
    let map = g.get_or_insert_with(HashMap::new);
    let key = missile as u32;
    map.remove(&key);
    let fp = fire_params as usize;
    let slot = slot_in_table(table, fp)
        .or_else(|| {
            let parent = fp.checked_sub(MISSILE_WEAPON_DATA)? as u32;
            map.get(&parent).copied()
        })
        .or_else(launching);
    if let Some(slot) = slot {
        map.insert(key, slot);
    }
}

/// The weapon slot a live missile was fired from, if WA's constructor call
/// named one (not strike bombs or crate bombs).
pub fn weapon_of(missile: *mut u8) -> Option<usize> {
    let g = WEAPON_OF.lock().ok()?;
    g.as_ref()?.get(&(missile as u32)).copied()
}

/// Called from the missile free hook: the address may be reused.
pub fn forget_weapon(missile: usize) {
    if let Ok(mut g) = WEAPON_OF.lock() {
        if let Some(map) = g.as_mut() {
            map.remove(&(missile as u32));
        }
    }
    if let Ok(mut g) = LAUNCHED.lock() {
        g.retain(|(m, _)| *m != missile as u32);
    }
}

/// Called at match start.
pub fn clear_weapons() {
    if let Ok(mut g) = WEAPON_OF.lock() {
        *g = None;
    }
    if let Ok(mut g) = LAUNCHED.lock() {
        g.clear();
    }
}

// ------------------------------------------------------ wa.weapons.launch

/// The slot a missile being launched belongs to, while its constructor runs
/// (-1 otherwise). With per-shot changes it flies with a copy of the slot's
/// fire params, whose address no longer names the slot.
static LAUNCH_SLOT: AtomicI32 = AtomicI32::new(-1);
/// The fire params copies launched missiles fly with, kept until WA frees
/// the missile.
static LAUNCHED: Mutex<Vec<(u32, Box<[u8]>)>> = Mutex::new(Vec::new());

fn launching() -> Option<usize> {
    usize::try_from(LAUNCH_SLOT.load(Ordering::SeqCst)).ok()
}

/// A shoot description for the missile constructor, as WA's release writes
/// one: the owner's team and worm number, position and speed, the target at
/// the launch point, WA's default bounce settle (30 frames) and an optional
/// fallback fuse.
fn shoot_desc(team: i32, worm: i32, args: &lua_host::LaunchArgs) -> [u8; SHOOT_DESC_SIZE] {
    let mut d = [0u8; SHOOT_DESC_SIZE];
    let mut put = |off: usize, v: i32| d[off..off + 4].copy_from_slice(&v.to_le_bytes());
    put(SHOOT_TEAM, team);
    put(SHOOT_WORM, worm);
    put(SHOOT_X, args.x);
    put(SHOOT_Y, args.y);
    put(SHOOT_VX, args.vx);
    put(SHOOT_VY, args.vy);
    put(SHOOT_TARGET_X, args.x);
    put(SHOOT_TARGET_Y, args.y);
    put(SHOOT_SETTLE, 30);
    put(SHOOT_FUSE, args.fuse.unwrap_or(0));
    d
}

/// A copy of a slot's fire params with this missile's changes, or None when
/// it has none and can fly with the slot's own, as WA's missiles do.
fn tuned_fire_params(src: &[u8], args: &lua_host::LaunchArgs) -> Option<Box<[u8]>> {
    if args.damage.is_none()
        && args.sprite.is_none()
        && args.fuse.is_none()
        && args.sound.is_none()
        && args.explode_into.is_none()
    {
        return None;
    }
    let mut p: Box<[u8]> = src.into();
    let mut put = |off: usize, v: Option<i32>| {
        if let Some(v) = v {
            p[off..off + 4].copy_from_slice(&v.to_le_bytes());
        }
    };
    put(FIRE_DAMAGE, args.damage);
    put(FIRE_SPRITE, args.sprite);
    put(FIRE_FUSE, args.fuse);
    put(FIRE_SOUND, args.sound);
    put(FIRE_EXPLODE_INTO, args.explode_into);
    Some(p)
}

/// `wa.weapons.launch`: a real WA missile from a slot's fire params, made
/// as `FireWeapon__CreateWeaponProjectile` (0x51E0F0) makes one, so WA
/// flies, draws, trails and explodes it as its own. The constructor runs
/// through our hook, so the missile gets its slot's pack changes too.
pub fn launch(args: &lua_host::LaunchArgs) -> Result<u32, String> {
    let slot = args.slot as usize;
    if slot >= STOCK_WEAPON_COUNT {
        return Err(format!("weapons.launch: slot {slot} out of range"));
    }
    let owner = args.owner as *mut u8;
    unsafe {
        let world = read_i32(owner, ENT_WORLD) as *mut u8;
        if world.is_null() {
            return Err("weapons.launch: the owner has no world".into());
        }
        let pool = world.add(GAME_WORLD_OBJECT_POOL) as *const i32;
        if *pool + OBJECT_POOL_MISSILE > OBJECT_POOL_LIMIT {
            return Err("weapons.launch: WA's object pool is full".into());
        }
        let table = *(world.add(GAME_WORLD_WEAPON_TABLE) as *const usize);
        if table == 0 {
            return Err("weapons.launch: no weapon table".into());
        }
        let own = (table + slot * WEAPON_ENTRY_SIZE + WEAPON_FIRE_PARAMS) as *const u8;
        let tuned = tuned_fire_params(std::slice::from_raw_parts(own, FIRE_PARAMS_SIZE), args);
        let fire_params = tuned.as_ref().map_or(own, |p| p.as_ptr()) as *const i32;
        let desc = shoot_desc(read_i32(owner, ENT_WORM_TEAM), read_i32(owner, WORM_INDEX), args);
        let parent = crate::actor::physics_parent(owner).ok_or("weapons.launch: no physics parent")?;

        let malloc: unsafe extern "C" fn(usize) -> *mut u8 =
            std::mem::transmute(rb(WA_MALLOC) as usize);
        let missile = malloc(MISSILE_ENTITY_SIZE);
        if missile.is_null() {
            return Err("weapons.launch: malloc failed".into());
        }
        std::ptr::write_bytes(missile, 0, MISSILE_ENTITY_ZEROED);
        let ctor: CtorFn = std::mem::transmute(rb(MISSILE_CTOR) as usize);
        LAUNCH_SLOT.store(slot as i32, Ordering::SeqCst);
        let p = ctor(missile, parent as u32, fire_params, desc.as_ptr() as *mut u8);
        LAUNCH_SLOT.store(-1, Ordering::SeqCst);
        if p.is_null() {
            return Err("weapons.launch: the missile constructor failed".into());
        }
        if let Some(tuned) = tuned {
            if let Ok(mut g) = LAUNCHED.lock() {
                g.push((p as u32, tuned));
            }
        }
        log_line(&format!(
            "weapons.launch: slot {slot} missile {:#x} at ({:#x},{:#x}) speed ({:#x},{:#x})",
            p as u32, args.x, args.y, args.vx, args.vy
        ));
        Ok(p as u32)
    }
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
        if let Err(e) = lua_host::weapon_emit("_explodes", cb, "on_explode", worm, slot, x, y, this as u32) {
            log_error(&format!("missile on_explode: {e}"));
        }
    }
    if let Some(spawn) = spawn {
        let body = crate::actor::stock_owner(this, owner);
        crate::actor::emit_attached(body, x, y, spawn, inherit_sprite);
    }
}

#[cfg(test)]
mod launch_tests {
    use super::{shoot_desc, tuned_fire_params};
    use game::offsets::{
        FIRE_DAMAGE, FIRE_EXPLODE_INTO, FIRE_FUSE, FIRE_PARAMS_SIZE, FIRE_SOUND, FIRE_SPRITE,
    };

    fn at(b: &[u8], off: usize) -> i32 {
        i32::from_le_bytes(b[off..off + 4].try_into().unwrap())
    }

    #[test]
    fn the_shoot_description_is_pxs_cshootdesc() {
        let args = lua_host::LaunchArgs {
            slot: 1, owner: 1, x: 0x0010_0000, y: 0x0020_0000, vx: 0x3_0000, vy: -0x2_0000,
            fuse: Some(5000), ..Default::default()
        };
        let d = shoot_desc(2, 3, &args);
        // Team, Worm, X, Y, SpX, SpY, AddX, AddY, vo32, vo36, Delay.
        let want = [2, 3, 0x0010_0000, 0x0020_0000, 0x3_0000, -0x2_0000, 0x0010_0000, 0x0020_0000, 0, 30, 5000];
        for (i, v) in want.iter().enumerate() {
            assert_eq!(at(&d, i * 4), *v, "dword {i}");
        }
    }

    #[test]
    fn per_shot_changes_go_to_a_copy() {
        let own = vec![0x11u8; FIRE_PARAMS_SIZE];
        let none = lua_host::LaunchArgs::default();
        assert!(tuned_fire_params(&own, &none).is_none(), "no changes: the slot's own");
        // The Double Bazooka's second shell, as PX's script launches it.
        let args = lua_host::LaunchArgs {
            damage: Some(40), sprite: Some(54), fuse: Some(5000), ..Default::default()
        };
        let p = tuned_fire_params(&own, &args).unwrap();
        assert_eq!((at(&p, FIRE_DAMAGE), at(&p, FIRE_SPRITE), at(&p, FIRE_FUSE)), (40, 54, 5000));
        assert_eq!(at(&p, FIRE_SOUND), 0x1111_1111, "untouched unless asked");
        // The Nuke Rocket's flying nuke: silent, bursting into clustlets.
        let nuke = lua_host::LaunchArgs {
            sound: Some(0), explode_into: Some(1), ..Default::default()
        };
        let p = tuned_fire_params(&own, &nuke).unwrap();
        assert_eq!((at(&p, FIRE_SOUND), at(&p, FIRE_EXPLODE_INTO)), (0, 1));
        assert_eq!(p.len(), FIRE_PARAMS_SIZE);
        assert!(own.iter().all(|b| *b == 0x11), "the record is untouched");
        assert_eq!(p[0], 0x11);
    }
}

#[cfg(test)]
mod tests {
    use super::slot_in_table;
    use game::offsets::{STOCK_WEAPON_COUNT, WEAPON_ENTRY_SIZE};

    #[test]
    fn a_pointer_inside_an_entry_is_that_entrys_slot() {
        let table = 0x0100_0000;
        let entry = |slot: usize| table + slot * WEAPON_ENTRY_SIZE;
        assert_eq!(slot_in_table(table, entry(15) + 0x3C), Some(15), "a projectile's fire params");
        assert_eq!(slot_in_table(table, entry(58) + 0x38), Some(58), "Armageddon's meteors");
        assert_eq!(slot_in_table(table, entry(STOCK_WEAPON_COUNT)), None, "past the table");
        assert_eq!(slot_in_table(table, table - 4), None, "before the table");
        assert_eq!(slot_in_table(0, 0x3C), None, "no table");
    }
}
