//! Generic transport and identity for collision bodies. See
//! re/CollisionManager.toml: WA 3.8.1 ctor, destructor and per-body update.
use game::entity::{read_i32, write_i32};
use game::offsets::*;
use lua_host::WarpArgs;
use std::{collections::HashMap, ffi::c_void, sync::{Mutex, atomic::{AtomicPtr, Ordering}}};

static CTOR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static DTOR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static UPDATE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
#[derive(Default)]
struct Ids {
    bodies: HashMap<u32, (u32, u32)>, // address -> (world, uid)
    worlds: HashMap<u32, (u32, usize)>, // next uid, live count
}
impl Ids {
    fn remove(&mut self, h: u32) {
        if let Some((world, _)) = self.bodies.remove(&h) {
            if let Some((_, count)) = self.worlds.get_mut(&world) {
                *count -= 1;
                if *count == 0 { self.worlds.remove(&world); }
            }
        }
    }
    fn insert(&mut self, h: u32, world: u32) {
        self.remove(h);
        let (next, count) = self.worlds.entry(world).or_insert((1, 0));
        self.bodies.insert(h, (world, *next));
        *next = next.checked_add(1).expect("body uid exhausted");
        *count += 1;
    }
}
static IDS: Mutex<Option<Ids>> = Mutex::new(None);
type Ctor = unsafe extern "stdcall" fn(*mut u8, *mut u8, u32, u32) -> *mut u8;
type Dtor = unsafe extern "thiscall" fn(*mut u8);
type Update = unsafe extern "stdcall" fn(*mut u8, *mut u8, u32);

pub fn install() -> Result<(), String> {
    for (name, detour, original) in [
        ("WORLD_ENTITY_CTOR", ctor as *mut c_void, &CTOR),
        ("WORLD_ENTITY_DTOR", dtor as *mut c_void, &DTOR),
        ("COLLISION_BODY_UPDATE", update as *mut c_void, &UPDATE),
    ] {
        original.store(unsafe { crate::wa::create_hook(crate::wa::address(name) as *mut c_void, detour) }?, Ordering::SeqCst);
    }
    Ok(())
}
unsafe extern "stdcall" fn ctor(this: *mut u8, parent: *mut u8, layer: u32, shape: u32) -> *mut u8 {
    let f: Ctor = unsafe { std::mem::transmute(CTOR.load(Ordering::SeqCst)) };
    let result = unsafe { f(this, parent, layer, shape) };
    if !this.is_null() {
        let world = unsafe { read_i32(this, ENT_WORLD) as u32 };
        if let Ok(mut ids) = IDS.lock() { ids.get_or_insert_with(Ids::default).insert(this as u32, world); }
    }
    result
}
unsafe extern "thiscall" fn dtor(this: *mut u8) {
    if let Ok(mut ids) = IDS.lock() { if let Some(ids) = ids.as_mut() { ids.remove(this as u32); } }
    crate::air_fire::forget(this as u32);
    let f: Dtor = unsafe { std::mem::transmute(DTOR.load(Ordering::SeqCst)) };
    unsafe { f(this) };
}
unsafe extern "stdcall" fn update(cm: *mut u8, this: *mut u8, index: u32) {
    let before_uid = uid(this as u32);
    if !this.is_null() {
        let class = unsafe { read_i32(this, ENT_CLASS_TYPE) };
        if lua_host::body_move_wanted(class) && matches!(class, CLASS_WORM | CLASS_MISSILE | CLASS_MINE | CLASS_CRATE | CLASS_OIL | CLASS_GRAVE | CLASS_ARROW) {
            if let Err(e) = lua_host::body_move(this as u32, class) {
                if e != "no lua state" { crate::log_error(&format!("body_move: {e}")); }
            }
        }
    }
    if before_uid != 0 && !live(this as u32, before_uid) { return; }
    let f: Update = unsafe { std::mem::transmute(UPDATE.load(Ordering::SeqCst)) };
    unsafe { f(cm, this, index) };
}
pub fn uid(h: u32) -> u32 {
    IDS.lock().ok().and_then(|ids| ids.as_ref().and_then(|ids| ids.bodies.get(&h).map(|(_, id)| *id))).unwrap_or(0)
}
pub fn live(h: u32, id: u32) -> bool { id != 0 && uid(h) == id }

pub fn warp(h: u32, args: &WarpArgs) -> bool {
    if uid(h) == 0 { return false; }
    let this = h as *mut u8;
    unsafe {
        let class = read_i32(this, ENT_CLASS_TYPE);
        let worm = class == CLASS_WORM;
        if worm && crate::world::worm_hp(h) <= 0 { return false; }
        let actor = crate::actor::live_actors().contains(&h);
        if !actor && !matches!(class, CLASS_WORM | CLASS_MISSILE | CLASS_MINE | CLASS_CRATE | CLASS_OIL | CLASS_GRAVE | CLASS_ARROW) { return false; }
        if args.facing.is_some() && !worm && !actor { return false; }
        if actor {
            if let Some(side) = args.facing {
                let flags = read_i32(this, ACTOR_FLAGS);
                write_i32(this, ACTOR_FLAGS, if side < 0 { flags | ACTOR_FLAG_MIRROR_X } else { flags & !ACTOR_FLAG_MIRROR_X });
            }
        }
        // A fits check here would prevent Lua's deliberate blocked-exit
        // handling (PX cuts a small hole). The caller decides clearance.
        let vx = args.vx.unwrap_or_else(|| read_i32(this, ENT_SPEED_X));
        let vy = args.vy.unwrap_or_else(|| read_i32(this, ENT_SPEED_Y));
        if worm {
            let state = read_i32(this, WORM_STATE) as u32;
            // Grounded or attached to a rope: detach through WA's own setup.
            // Keep airborne/tumbling/frozen states and all weapon/fuse fields.
            if matches!(state, 0x65 | 0x66 | 0x67 | 0x7B | 0x7C) && (vx != 0 || vy != 0) {
                crate::physics::set_worm_state(h, WORM_STATE_AIRBORNE);
            }
            if let Some(side) = args.facing {
                write_i32(this, WORM_TURN_SIDE, side);
                write_i32(this, WORM_FACING, side);
            }
        }
        write_i32(this, ENT_POS_X, args.x);
        write_i32(this, ENT_POS_Y, args.y);
        write_i32(this, ENT_SPEED_X, vx);
        write_i32(this, ENT_SPEED_Y, vy);
        // update_0 skips a settled body (4FCE0F); B8 is the moved latch.
        write_i32(this, ENT_SETTLE, 0);
        write_i32(this, ENT_MOVED, 1);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identities_follow_construction_not_addresses_and_end_with_world() {
        let mut ids = Ids::default();
        ids.insert(100, 10); ids.insert(50, 10);
        assert_eq!(ids.bodies[&100].1, 1); assert_eq!(ids.bodies[&50].1, 2);
        ids.remove(100); ids.insert(100, 10);
        assert_eq!(ids.bodies[&100].1, 3);
        ids.insert(900, 20); assert_eq!(ids.bodies[&900].1, 1);
        ids.remove(100); ids.remove(50); ids.insert(70, 10);
        assert_eq!(ids.bodies[&70].1, 1);
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    #[test]
    fn warp_preserves_frozen_state_health_owner_and_unrelated_fields() {
        let mut world = vec![0i32; (ARENA_WORM_HP + 0x100) / 4];
        world[ARENA_WORM_HP / 4] = 100;
        let mut body = vec![0x13579i32; 0x600 / 4];
        let p = body.as_mut_ptr() as *mut u8;
        let h = p as u32;
        unsafe {
            write_i32(p, ENT_CLASS_TYPE, CLASS_WORM);
            write_i32(p, ENT_WORLD, world.as_mut_ptr() as i32);
            write_i32(p, ENT_WORM_TEAM, 0);
            write_i32(p, WORM_INDEX, 0);
            write_i32(p, WORM_STATE, 0x86);
            write_i32(p, ENT_SPEED_X, 5 * 65536);
            write_i32(p, ENT_SPEED_Y, -7 * 65536);
        }
        IDS.lock().unwrap().get_or_insert_with(Ids::default).insert(h, 123);
        let old_uid = uid(h);
        let before = body.clone();
        assert!(warp(h, &WarpArgs { x: 100 * 65536, y: 200 * 65536,
            vx: Some(13 * 65536), vy: None, facing: Some(-1) }));
        let allowed = [ENT_POS_X, ENT_POS_Y, ENT_SPEED_X, ENT_SETTLE, ENT_MOVED, WORM_TURN_SIDE, WORM_FACING];
        for (i, (&old, &new)) in before.iter().zip(&body).enumerate() {
            if !allowed.contains(&(i * 4)) { assert_eq!(old, new, "changed +{:x}", i * 4); }
        }
        assert_eq!(body[WORM_STATE / 4], 0x86);
        assert_eq!(body[ENT_SPEED_Y / 4], -7 * 65536);
        assert_eq!(crate::world::worm_hp(h), 100);
        assert_eq!(body[ENT_SETTLE / 4], 0);
        assert_eq!(body[ENT_MOVED / 4], 1);
        IDS.lock().unwrap().as_mut().unwrap().remove(h);
        assert!(!live(h, old_uid));
        assert!(!warp(h, &WarpArgs { x: 0, y: 0, vx: None, vy: None, facing: None }));
    }
}
