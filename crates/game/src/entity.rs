//! WorldEntity motion fields. Values are WA Fixed 16.16.

use crate::offsets::*;

#[inline]
pub unsafe fn read_i32(ptr: *mut u8, off: usize) -> i32 {
    unsafe { ptr.add(off).cast::<i32>().read_unaligned() }
}

#[inline]
pub unsafe fn write_i32(ptr: *mut u8, off: usize, v: i32) {
    unsafe { ptr.add(off).cast::<i32>().write_unaligned(v) }
}

pub unsafe fn is_missile(ptr: *mut u8, missile_vt: u32) -> bool {
    if ptr.is_null() {
        return false;
    }
    unsafe { read_i32(ptr, ENT_VTABLE) as u32 == missile_vt }
}

/// Snapshot of the projectile-gravity leaf. `cm_*` fields are CollisionManager
/// globals (read-only for Lua). `damp` / `wind_scale` / `gravity_scale` are
/// entity+0x58 / +0x5C / +0x60 — the multipliers stock uses this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Motion {
    pub id: u32,
    pub class_type: i32,
    pub damp: i32,
    pub wind_scale: i32,
    pub gravity_scale: i32,
    pub moved: i32,
    pub x: i32,
    pub y: i32,
    pub vx: i32,
    pub vy: i32,
    pub cm_gravity: i32,
    pub cm_wind: i32,
}

pub unsafe fn read_cm_forces(cm: *mut u8) -> (i32, i32) {
    if cm.is_null() {
        return (0, 0);
    }
    unsafe {
        (
            read_i32(cm, CM_PARTICLE_GRAVITY_Y),
            read_i32(cm, CM_WIND_BIAS),
        )
    }
}

pub unsafe fn read_motion(ptr: *mut u8) -> Option<Motion> {
    if ptr.is_null() {
        return None;
    }
    Some(unsafe {
        Motion {
            id: ptr as u32,
            class_type: read_i32(ptr, ENT_CLASS_TYPE),
            damp: read_i32(ptr, ENT_DAMP),
            wind_scale: read_i32(ptr, ENT_WIND_SCALE),
            gravity_scale: read_i32(ptr, ENT_GRAVITY_SCALE),
            moved: read_i32(ptr, ENT_MOVED),
            x: read_i32(ptr, ENT_POS_X),
            y: read_i32(ptr, ENT_POS_Y),
            vx: read_i32(ptr, ENT_SPEED_X),
            vy: read_i32(ptr, ENT_SPEED_Y),
            cm_gravity: 0,
            cm_wind: 0,
        }
    })
}

pub unsafe fn write_scales(ptr: *mut u8, m: &Motion) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        write_i32(ptr, ENT_DAMP, m.damp);
        write_i32(ptr, ENT_WIND_SCALE, m.wind_scale);
        write_i32(ptr, ENT_GRAVITY_SCALE, m.gravity_scale);
    }
}

pub unsafe fn write_motion(ptr: *mut u8, m: &Motion) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        write_scales(ptr, m);
        write_i32(ptr, ENT_POS_X, m.x);
        write_i32(ptr, ENT_POS_Y, m.y);
        write_i32(ptr, ENT_SPEED_X, m.vx);
        write_i32(ptr, ENT_SPEED_Y, m.vy);
        write_i32(ptr, ENT_MOVED, m.moved);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missile_vtable_constant() {
        assert_eq!(MISSILE_VTABLE, 0x00664438);
    }

    #[test]
    fn parent_defaults_slots() {
        assert_eq!(ENT_DAMP, 0x58);
        assert_eq!(ENT_WIND_SCALE, 0x5C);
        assert_eq!(ENT_GRAVITY_SCALE, 0x60);
    }
}
