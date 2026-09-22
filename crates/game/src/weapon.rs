use core::ffi::c_char;

use crate::offsets::{
    STOCK_WEAPON_COUNT, WEAPON_AVAILABILITY, WEAPON_BOUNCE_PCT, WEAPON_CLUSTER_BOUNCE,
    WEAPON_CLUSTER_DAMAGE, WEAPON_CLUSTER_GRAVITY, WEAPON_CLUSTER_HOMING, WEAPON_CLUSTER_HOMING_KIND,
    WEAPON_CLUSTER_MISSILE_TYPE, WEAPON_CLUSTER_RENDER_TIMER, WEAPON_CLUSTER_SPRITE, WEAPON_CLUSTER_WIND,
    WEAPON_CREATES_PROJECTILE, WEAPON_DAMAGE, WEAPON_DEFINED, WEAPON_ENTRY_SIZE, WEAPON_FIRE_METHOD,
    WEAPON_FIRE_TYPE, WEAPON_GRAVITY_PCT, WEAPON_MINE_BLAST, WEAPON_MINE_FUSE, WEAPON_MINE_MASK,
    WEAPON_MINE_PROXIMITY, WEAPON_MINE_TRIGGER, WEAPON_NAME1, WEAPON_REQUIRES_AIMING, WEAPON_SPECIAL_SUBTYPE,
    WEAPON_SPRITE_ID, WEAPON_TABLE_BYTES, WEAPON_UNKNOWN_18, WEAPON_WIND_PCT,
};

/// One stock table slot. Layout is a fixed 0x1D0 blob so host tests can
/// round-trip fields without depending on pointer width of `name1`.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct WeaponEntry {
    pub raw: [u8; WEAPON_ENTRY_SIZE],
}

const _: () = assert!(core::mem::size_of::<WeaponEntry>() == WEAPON_ENTRY_SIZE);

#[derive(Clone, Copy)]
#[repr(C)]
pub struct WeaponTable {
    pub entries: [WeaponEntry; STOCK_WEAPON_COUNT],
}

const _: () = assert!(core::mem::size_of::<WeaponTable>() == WEAPON_TABLE_BYTES);

impl Default for WeaponEntry {
    fn default() -> Self {
        Self {
            raw: [0; WEAPON_ENTRY_SIZE],
        }
    }
}

impl WeaponEntry {
    pub fn read_i32(&self, offset: usize) -> i32 {
        let b = self.raw[offset..offset + 4].try_into().unwrap();
        i32::from_le_bytes(b)
    }

    pub fn write_i32(&mut self, offset: usize, value: i32) {
        self.raw[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    pub fn name1(&self) -> *const c_char {
        u32::from_le_bytes(self.raw[WEAPON_NAME1..WEAPON_NAME1 + 4].try_into().unwrap()) as *const c_char
    }

    pub fn set_name1(&mut self, ptr: *const c_char) {
        let bits = ptr as u32;
        self.raw[WEAPON_NAME1..WEAPON_NAME1 + 4].copy_from_slice(&bits.to_le_bytes());
    }

    pub fn defined(&self) -> i32 {
        self.read_i32(WEAPON_DEFINED)
    }

    pub fn set_defined(&mut self, value: i32) {
        self.write_i32(WEAPON_DEFINED, value);
    }

    pub fn requires_aiming(&self) -> i32 {
        self.read_i32(WEAPON_REQUIRES_AIMING)
    }

    pub fn set_requires_aiming(&mut self, value: i32) {
        self.write_i32(WEAPON_REQUIRES_AIMING, value);
    }

    pub fn fire_type(&self) -> i32 {
        self.read_i32(WEAPON_FIRE_TYPE)
    }

    pub fn set_fire_type(&mut self, value: i32) {
        self.write_i32(WEAPON_FIRE_TYPE, value);
    }

    pub fn special_subtype(&self) -> i32 {
        self.read_i32(WEAPON_SPECIAL_SUBTYPE)
    }

    pub fn set_special_subtype(&mut self, value: i32) {
        self.write_i32(WEAPON_SPECIAL_SUBTYPE, value);
    }

    pub fn fire_method(&self) -> i32 {
        self.read_i32(WEAPON_FIRE_METHOD)
    }

    pub fn set_fire_method(&mut self, value: i32) {
        self.write_i32(WEAPON_FIRE_METHOD, value);
    }

    pub fn creates_projectile(&self) -> i32 {
        self.read_i32(WEAPON_CREATES_PROJECTILE)
    }

    pub fn set_creates_projectile(&mut self, value: i32) {
        self.write_i32(WEAPON_CREATES_PROJECTILE, value);
    }

    pub fn set_unknown_18(&mut self, value: i32) {
        self.write_i32(WEAPON_UNKNOWN_18, value);
    }

    /// `weapon` is the panel slot. This copies only how the weapon fires
    /// (aim, strike/projectile, fire_params). Panel row, defined, and
    /// scheme availability stay on this entry so it does not vanish.
    pub fn copy_fire_from(&mut self, src: &WeaponEntry) {
        self.raw[WEAPON_REQUIRES_AIMING..WEAPON_DEFINED]
            .copy_from_slice(&src.raw[WEAPON_REQUIRES_AIMING..WEAPON_DEFINED]);
        self.raw[WEAPON_DEFINED + 4..WEAPON_AVAILABILITY]
            .copy_from_slice(&src.raw[WEAPON_DEFINED + 4..WEAPON_AVAILABILITY]);
        self.raw[WEAPON_FIRE_TYPE..].copy_from_slice(&src.raw[WEAPON_FIRE_TYPE..]);
    }

    pub fn damage(&self) -> i32 {
        self.read_i32(WEAPON_DAMAGE)
    }

    pub fn set_damage(&mut self, value: i32) {
        self.write_i32(WEAPON_DAMAGE, value);
    }

    pub fn gravity_pct(&self) -> i32 {
        self.read_i32(WEAPON_GRAVITY_PCT)
    }

    pub fn set_gravity_pct(&mut self, value: i32) {
        self.write_i32(WEAPON_GRAVITY_PCT, value);
    }

    pub fn bounce_pct(&self) -> i32 {
        self.read_i32(WEAPON_BOUNCE_PCT)
    }

    pub fn set_bounce_pct(&mut self, value: i32) {
        self.write_i32(WEAPON_BOUNCE_PCT, value);
    }

    pub fn sprite_id(&self) -> i32 {
        self.read_i32(WEAPON_SPRITE_ID)
    }

    pub fn set_sprite_id(&mut self, value: i32) {
        self.write_i32(WEAPON_SPRITE_ID, value);
    }

    /// StrikeFire copies from `special_subtype` (+0x34). Missile ctor then
    /// reads `AirStrike+0x60` as weapon_data, which is entry `+0x74` —
    /// the field named wind_influence. Stock donkey stores KnownSpriteId
    /// Donkey (82) there, not fire-params sprite_id (100).
    pub fn retarget_sprite_id(&mut self, old: i32, new: i32) {
        if old == new {
            return;
        }
        self.set_sprite_id(new);
        const DONKEY_GFX: i32 = 82;
        for off in [0x64usize, 0x68, WEAPON_WIND_PCT, 0x110, 0x114, 0x118] {
            let v = self.read_i32(off);
            if v == old || v == DONKEY_GFX {
                self.write_i32(off, new);
            }
        }
    }

    pub fn wind_pct(&self) -> i32 {
        self.read_i32(WEAPON_WIND_PCT)
    }

    pub fn set_wind_pct(&mut self, value: i32) {
        self.write_i32(WEAPON_WIND_PCT, value);
    }

    pub fn set_cluster_sprite_id(&mut self, value: i32) {
        self.write_i32(WEAPON_CLUSTER_SPRITE, value);
    }

    pub fn cluster_sprite_id(&self) -> i32 {
        self.read_i32(WEAPON_CLUSTER_SPRITE)
    }

    pub fn set_cluster_damage(&mut self, value: i32) {
        self.write_i32(WEAPON_CLUSTER_DAMAGE, value);
    }

    pub fn set_cluster_gravity_pct(&mut self, value: i32) {
        self.write_i32(WEAPON_CLUSTER_GRAVITY, value);
    }

    pub fn set_cluster_wind_pct(&mut self, value: i32) {
        self.write_i32(WEAPON_CLUSTER_WIND, value);
    }

    pub fn set_cluster_bounce_pct(&mut self, value: i32) {
        self.write_i32(WEAPON_CLUSTER_BOUNCE, value);
    }

    pub fn set_cluster_missile_type(&mut self, value: i32) {
        self.write_i32(WEAPON_CLUSTER_MISSILE_TYPE, value);
    }

    pub fn set_cluster_render_timer(&mut self, value: i32) {
        self.write_i32(WEAPON_CLUSTER_RENDER_TIMER, value);
    }

    pub fn set_cluster_homing_params(&mut self, values: [i32; 5]) {
        for (i, v) in values.iter().enumerate() {
            self.write_i32(WEAPON_CLUSTER_HOMING + i * 4, *v);
        }
    }

    pub fn set_cluster_homing_kind(&mut self, kind: i32, p1: i32, p2: i32) {
        self.write_i32(WEAPON_CLUSTER_HOMING_KIND, kind);
        self.write_i32(WEAPON_CLUSTER_HOMING_KIND + 4, p1);
        self.write_i32(WEAPON_CLUSTER_HOMING_KIND + 8, p2);
    }

    pub fn is_mine(&self) -> bool {
        self.fire_type() == 2 && self.fire_method() == 1
    }

    pub fn set_mine_proximity(&mut self, value: i32) {
        self.write_i32(WEAPON_MINE_PROXIMITY, value);
    }

    pub fn set_mine_fuse(&mut self, value: i32) {
        self.write_i32(WEAPON_MINE_FUSE, value);
    }

    pub fn set_mine_mask(&mut self, value: i32) {
        self.write_i32(WEAPON_MINE_MASK, value);
    }

    pub fn set_mine_trigger(&mut self, value: i32) {
        self.write_i32(WEAPON_MINE_TRIGGER, value);
    }

    pub fn set_mine_blast(&mut self, value: i32) {
        self.write_i32(WEAPON_MINE_BLAST, value);
    }
}

/// Copy 71 entries from a live WA table pointer.
///
/// # Safety
/// `ptr` must point at a 0x80B0-byte weapon table for the current match.
pub unsafe fn snapshot_table(ptr: *const WeaponEntry) -> WeaponTable {
    let mut table = WeaponTable {
        entries: [WeaponEntry::default(); STOCK_WEAPON_COUNT],
    };
    if ptr.is_null() {
        return table;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(ptr, table.entries.as_mut_ptr(), STOCK_WEAPON_COUNT);
    }
    table
}

/// Write one entry back to the live table.
///
/// # Safety
/// `ptr` must be the live table; `index` in 0..71.
pub unsafe fn write_entry(ptr: *mut WeaponEntry, index: usize, entry: &WeaponEntry) {
    assert!(index < STOCK_WEAPON_COUNT);
    unsafe {
        *ptr.add(index) = *entry;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offsets::{WEAPON_AVAILABILITY, WEAPON_FIRE_TYPE, WEAPON_PANEL_STATE, WEAPON_REQUIRES_AIMING};

    #[test]
    fn damage_roundtrip() {
        let mut e = WeaponEntry::default();
        e.set_damage(45);
        e.set_bounce_pct(100);
        e.set_sprite_id(50);
        e.set_wind_pct(200);
        assert_eq!(e.damage(), 45);
        assert_eq!(e.bounce_pct(), 100);
        assert_eq!(e.sprite_id(), 50);
        assert_eq!(e.wind_pct(), 200);
    }

    #[test]
    fn copy_fire_keeps_panel_slot_identity() {
        let mut dest = WeaponEntry::default();
        dest.write_i32(WEAPON_PANEL_STATE, 1);
        dest.set_defined(1);
        dest.write_i32(WEAPON_AVAILABILITY, 1);
        dest.write_i32(WEAPON_FIRE_TYPE, 1);
        dest.write_i32(WEAPON_REQUIRES_AIMING, 1);
        dest.set_sprite_id(50);

        let mut src = WeaponEntry::default();
        src.write_i32(WEAPON_PANEL_STATE, 5);
        src.set_defined(0);
        src.write_i32(WEAPON_AVAILABILITY, 0);
        src.write_i32(WEAPON_FIRE_TYPE, 3);
        src.write_i32(WEAPON_REQUIRES_AIMING, 0);
        src.set_sprite_id(100);
        src.write_i32(0x64, 100);
        src.write_i32(0x68, 100);
        src.set_wind_pct(82);

        dest.copy_fire_from(&src);
        assert_eq!(dest.read_i32(WEAPON_PANEL_STATE), 1);
        assert_eq!(dest.defined(), 1);
        assert_eq!(dest.read_i32(WEAPON_AVAILABILITY), 1);
        assert_eq!(dest.read_i32(WEAPON_FIRE_TYPE), 3);
        assert_eq!(dest.read_i32(WEAPON_REQUIRES_AIMING), 0);
        assert_eq!(dest.sprite_id(), 100);
        dest.retarget_sprite_id(100, 801);
        assert_eq!(dest.sprite_id(), 801);
        assert_eq!(dest.read_i32(0x64), 801);
        assert_eq!(dest.read_i32(0x68), 801);
        assert_eq!(dest.wind_pct(), 801);
    }
}
