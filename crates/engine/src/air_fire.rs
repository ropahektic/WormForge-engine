//! Opt-in Lua firing while WA keeps its own airborne physics.
use game::{entity::read_i32, offsets::*};
use std::{collections::HashMap, sync::Mutex};
#[derive(Clone, Copy)]
struct Armed { slot: u32, held: bool }
static ARMED: Mutex<Option<HashMap<u32, Armed>>> = Mutex::new(None);
pub fn clear() { if let Ok(mut g) = ARMED.lock() { *g = None; } }
pub fn forget(h: u32) { if let Ok(mut g) = ARMED.lock() { if let Some(g) = g.as_mut() { g.remove(&h); } } }
pub fn enable(h: u32, on: bool) -> bool {
    if !on { forget(h); return true; }
    if crate::body::uid(h) == 0 { return false; }
    let worm = h as *mut u8;
    let slot = unsafe { read_i32(worm, WORM_SELECTED_WEAPON) as u32 };
    if unsafe { read_i32(worm, ENT_CLASS_TYPE) } != CLASS_WORM || crate::actor::slot_fire(slot) == 0 { return false; }
    if let Ok(mut g) = ARMED.lock() {
        let map = g.get_or_insert_with(HashMap::new);
        map.entry(h).and_modify(|armed| { if armed.slot != slot { *armed = Armed { slot, held: true }; } })
            .or_insert(Armed { slot, held: false });
    }
    true
}
fn airborne(state: u32) -> bool { matches!(state, 0x77 | 0x79 | 0x7A | 0x7E | 0x7F | 0x80 | 0x81) }
pub fn message(worm: *mut u8, msg: u32) -> bool {
    if worm.is_null() { return false; }
    let h = worm as u32;
    let Some(mut armed) = ARMED.lock().ok().and_then(|g| g.as_ref().and_then(|g| g.get(&h).copied())) else { return false; };
    let (slot, active, state) = unsafe { (read_i32(worm, WORM_SELECTED_WEAPON) as u32,
        read_i32(worm, WORM_TURN_ACTIVE) != 0, read_i32(worm, WORM_STATE) as u32) };
    if slot != armed.slot || !active || crate::world::worm_hp(h) <= 0 || matches!(msg, MSG_RETREAT_FINISHED | MSG_FINISH_TURN) {
        forget(h); return false;
    }
    // Eat only WA's rejected airborne Space messages. Ground fire keeps its
    // usual charging/release, ammo and retreat path.
    if matches!(msg, MSG_FIRE_WEAPON | MSG_RELEASE_WEAPON) && airborne(state) { return true; }
    if msg != MSG_FRAME_FINISH { return false; }
    let pressed = crate::world::keys() & 128 != 0;
    let fire = pressed && !armed.held && airborne(state) && crate::world::ammo(h, slot, None) != 0;
    armed.held = pressed;
    if let Ok(mut g) = ARMED.lock() { if let Some(g) = g.as_mut() { g.insert(h, armed); } }
    if fire {
        let (x, y) = unsafe { (read_i32(worm, ENT_POS_X), read_i32(worm, ENT_POS_Y)) };
        if let Err(e) = lua_host::weapon_air_fire(crate::actor::slot_fire(slot), h, slot as i32, x, y) {
            crate::log_error(&format!("air_fire: {e}"));
        }
    }
    false
}
