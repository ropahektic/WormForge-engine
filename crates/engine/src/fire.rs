//! `wa.fire.spawn`: one of WA's fires (FireEntity), started as the Petrol
//! Bomb's MissileEntity::CreateFire1 (0x509D70) starts its own: WA's
//! allocator, the first FIRE_ENTITY_ZEROED bytes zeroed, then the constructor
//! with the fire store for parent, a 12-dword setup (PX's CFireDesc) and no
//! source fire. The constructor draws from WA's RNG, so it runs only from
//! simulation callbacks: every peer and replay starts the same fires.
//!
//! A fire hits a worm with message 0x4B, kind 1, the fire its sender (Project
//! X's M_GUNEXP). A fire spawned with `damage_scale` / `knock_scale` has those
//! hits scaled before WA (and `wa.on.hurt`) see them, as Project X's Valyrian
//! Fire does. Such fires are known by address and pool slot together, so a
//! later fire at a freed one's address is not taken for it.

use game::entity::{read_i32, write_i32};
use game::offsets::{
    CLASS_WORM, ENT_CLASS_TYPE, ENT_WORM_TEAM, FIRE_ENTITY_POOL_SLOT, FIRE_ENTITY_SIZE,
    FIRE_ENTITY_ZEROED, HIT_AMOUNT, HIT_FLAG, HIT_KIND_FIRE, HIT_KNOCK_X, HIT_KNOCK_Y, MSG_HIT,
    SHARED_FIRE_STORE,
};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// A fire whose hits are scaled: (address, pool slot, damage, knock), the
/// scales 16.16.
static SCALED: Mutex<Vec<(u32, i32, i32, i32)>> = Mutex::new(Vec::new());

const ONE: i32 = 0x1_0000;

type FireCtorFn =
    unsafe extern "stdcall" fn(this: *mut u8, parent: *mut u8, init: *const i32, source: *mut u8) -> *mut u8;

/// Fires started this match, for their ids.
static STARTED: AtomicU32 = AtomicU32::new(0);

pub fn clear() {
    STARTED.store(0, Ordering::SeqCst);
    if let Ok(mut g) = SCALED.lock() {
        g.clear();
    }
}

/// `v` times a 16.16 scale, in integers, the same on every machine.
fn scaled(v: i32, scale: i32) -> i32 {
    ((v as i64 * scale as i64) >> 16) as i32
}

/// Before a worm handles `msg` from `sender`: a scaled fire's hit gets its
/// damage and knockback scaled.
pub fn before_worm_message(sender: u32, msg: u32, data: *mut u8) {
    if msg != MSG_HIT || sender == 0 || data.is_null() {
        return;
    }
    let Some((damage, knock)) = SCALED.lock().ok().and_then(|g| {
        g.iter()
            .find(|(fire, _, _, _)| *fire == sender)
            .filter(|(_, slot, _, _)| unsafe { read_i32(sender as *mut u8, FIRE_ENTITY_POOL_SLOT) } == *slot)
            .map(|&(_, _, d, k)| (d, k))
    }) else {
        return;
    };
    unsafe {
        if read_i32(data, HIT_FLAG) != HIT_KIND_FIRE {
            return;
        }
        write_i32(data, HIT_AMOUNT, scaled(read_i32(data, HIT_AMOUNT), damage));
        write_i32(data, HIT_KNOCK_X, scaled(read_i32(data, HIT_KNOCK_X), knock));
        write_i32(data, HIT_KNOCK_Y, scaled(read_i32(data, HIT_KNOCK_Y), knock));
    }
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
        let p = ctor(fire, parent, init.as_ptr(), std::ptr::null_mut());
        if p.is_null() {
            return Err("wa.fire.spawn: the fire constructor failed".into());
        }
        if args.damage_scale != ONE || args.knock_scale != ONE {
            let slot = read_i32(p, FIRE_ENTITY_POOL_SLOT);
            if let Ok(mut g) = SCALED.lock() {
                g.retain(|(fire, _, _, _)| *fire != p as u32);
                g.push((p as u32, slot, args.damage_scale, args.knock_scale));
            }
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
            x: 10, y: 20, vx: 3, vy: 4, count: 8, duration: 4000, up_power: 100, damage: 20,
            owner: 1, ..lua_host::FIRE_DEFAULTS
        };
        assert_eq!(setup(&args, 2), [10, 20, 3, 4, 0, 8, 1, 4000, 100, 20, 1, 2]);
    }

    #[test]
    fn a_scaled_fires_hit_is_scaled_and_others_are_not() {
        clear();
        // A fake fire: its pool slot is all the hit check reads of it.
        let mut fire = vec![0u8; FIRE_ENTITY_SIZE];
        unsafe { write_i32(fire.as_mut_ptr(), FIRE_ENTITY_POOL_SLOT, 7) };
        let addr = fire.as_mut_ptr() as u32;
        SCALED.lock().unwrap().push((addr, 7, 0x1_4000, 0x2_0000)); // 1.25, 2
        let hit = |flag: i32| {
            let mut d = [0i32; 8];
            d[0] = flag;
            d[3] = -0x3_0000; // knock x, 16.16
            d[4] = 0x1_8000;
            d[5] = 20; // damage
            d
        };
        let mut d = hit(HIT_KIND_FIRE);
        before_worm_message(addr, MSG_HIT, d.as_mut_ptr() as *mut u8);
        assert_eq!((d[5], d[3], d[4]), (25, -0x6_0000, 0x3_0000));
        let mut other = hit(10); // the axe's percent hit, not a fire's
        before_worm_message(addr, MSG_HIT, other.as_mut_ptr() as *mut u8);
        assert_eq!(other[5], 20);
        unsafe { write_i32(fire.as_mut_ptr(), FIRE_ENTITY_POOL_SLOT, 8) };
        let mut reused = hit(HIT_KIND_FIRE);
        before_worm_message(addr, MSG_HIT, reused.as_mut_ptr() as *mut u8);
        assert_eq!(reused[5], 20, "another fire at a freed one's address");
        clear();
    }
}
