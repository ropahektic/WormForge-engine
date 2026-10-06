//! Remember which stock slot created a WA ArrowEntity for `wa.on.hurt`.
//! The arrow's hit message supplies only its sender pointer. Its selected
//! weapon may change while it flies, so snapshot the slot at construction.

use game::entity::read_i32;
use game::offsets::{CLASS_ARROW, ENT_CLASS_TYPE, ENT_WORLD};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

static ORIGINAL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static WEAPONS: Mutex<Option<HashMap<u32, usize>>> = Mutex::new(None);

type Ctor = unsafe extern "stdcall" fn(*mut u8, *mut u8, *const i32, *mut u8) -> *mut u8;

pub fn install() -> Result<(), String> {
    let target = crate::wa::address("ARROW_CTOR") as *mut c_void;
    let trampoline = unsafe { crate::wa::create_hook(target, ctor_hook as *mut c_void) }?;
    ORIGINAL.store(trampoline, Ordering::SeqCst);
    Ok(())
}

pub fn clear() {
    if let Ok(mut g) = WEAPONS.lock() {
        *g = None;
    }
}

pub fn weapon_of(sender: u32) -> Option<i32> {
    if sender == 0 || unsafe { read_i32(sender as *mut u8, ENT_CLASS_TYPE) } != CLASS_ARROW {
        return None;
    }
    WEAPONS
        .lock()
        .ok()?
        .as_ref()?
        .get(&sender)
        .copied()
        .map(|slot| slot as i32)
}

unsafe extern "stdcall" fn ctor_hook(
    this: *mut u8,
    parent: *mut u8,
    fire_params: *const i32,
    ctx: *mut u8,
) -> *mut u8 {
    // CreateArrow gives the ArrowEntity the shared physics filter as parent;
    // like any entity, it points back at this match's GameWorld.
    let world = if parent.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { read_i32(parent, ENT_WORLD) as *mut u8 }
    };
    let slot = crate::mine::slot_in_world(world, fire_params);
    if let Ok(mut g) = WEAPONS.lock() {
        let map = g.get_or_insert_with(HashMap::new);
        map.remove(&(this as u32));
        if let Some(slot) = slot {
            map.insert(this as u32, slot);
        }
    }
    let original: Ctor = unsafe { std::mem::transmute(ORIGINAL.load(Ordering::SeqCst)) };
    unsafe { original(this, parent, fire_params, ctx) }
}
