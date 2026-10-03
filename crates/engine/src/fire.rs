//! `wa.fire.spawn`: one of WA's fires (FireEntity), started as the Petrol
//! Bomb's MissileEntity::CreateFire1 (0x509D70) starts its own: WA's
//! allocator, the first FIRE_ENTITY_ZEROED bytes zeroed, then the constructor
//! with the fire store for parent, a 12-dword setup (PX's CFireDesc) and no
//! source fire. The constructor draws from WA's RNG, so it runs only from
//! simulation callbacks: every peer and replay starts the same fires.

use game::entity::read_i32;
use game::offsets::{
    CLASS_WORM, ENT_CLASS_TYPE, ENT_WORM_TEAM, FIRE_ENTITY_SIZE, FIRE_ENTITY_ZEROED,
    SHARED_FIRE_STORE,
};
use std::sync::atomic::{AtomicU32, Ordering};

type FireCtorFn =
    unsafe extern "stdcall" fn(this: *mut u8, parent: *mut u8, init: *const i32, source: *mut u8) -> *mut u8;

/// Fires started this match, for their ids.
static STARTED: AtomicU32 = AtomicU32::new(0);

pub fn clear() {
    STARTED.store(0, Ordering::SeqCst);
}

/// The 12 dwords FireEntity's constructor copies (PX's CFireDesc): position,
/// speed, 0x10 (0, as WA and PX pass it), count, 0x18 (1), duration,
/// upward power, damage, 0x28 (1), owner team.
fn setup(args: &lua_host::FireArgs, team: i32) -> [i32; 12] {
    [
        args.x, args.y, args.vx, args.vy, 0, args.count, 1, args.duration, args.up_power,
        args.damage, 1, team,
    ]
}

pub fn spawn(args: &lua_host::FireArgs) -> Result<u32, String> {
    let Some(ctor) = crate::wa::try_address("FIRE_ENTITY_CTOR") else {
        return Err("wa.fire.spawn: WA's fire constructor was not located".into());
    };
    let owner = args.owner as *mut u8;
    unsafe {
        let parent = crate::actor::shared_object(owner, SHARED_FIRE_STORE)
            .ok_or("wa.fire.spawn: no fire store")?;
        // A worm's team owns the fire, as the shot's team owns a Petrol
        // Bomb's; anything else's is no team's.
        let team = if read_i32(owner, ENT_CLASS_TYPE) == CLASS_WORM {
            read_i32(owner, ENT_WORM_TEAM)
        } else {
            0
        };
        let init = setup(args, team);
        let malloc: unsafe extern "C" fn(usize) -> *mut u8 =
            std::mem::transmute(crate::wa::address("WA_MALLOC") as usize);
        let fire = malloc(FIRE_ENTITY_SIZE);
        if fire.is_null() {
            return Err("wa.fire.spawn: malloc failed".into());
        }
        std::ptr::write_bytes(fire, 0, FIRE_ENTITY_ZEROED);
        let ctor: FireCtorFn = std::mem::transmute(ctor as usize);
        if ctor(fire, parent, init.as_ptr(), std::ptr::null_mut()).is_null() {
            return Err("wa.fire.spawn: the fire constructor failed".into());
        }
    }
    Ok(STARTED.fetch_add(1, Ordering::SeqCst) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setup_is_pxs_fire_desc_in_order() {
        let args = lua_host::FireArgs {
            owner: 1, x: 10, y: 20, vx: 3, vy: 4, count: 8, duration: 4000, up_power: 100, damage: 20,
        };
        assert_eq!(setup(&args, 2), [10, 20, 3, 4, 0, 8, 1, 4000, 100, 20, 1, 2]);
    }
}
