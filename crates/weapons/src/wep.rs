//! Project X weapon files (`.wep`).
//!
//! A `.wep` is a 4-byte PX tag, then WA's own 0x1D0-byte weapon table entry
//! exactly as it sits in memory, then PX's strings (names, icon, sprite
//! files), each a u32 length and its bytes. Every field lines up with WA
//! 3.8.1's entry at the same offset: PX's Hellish Hand-Grenade reproduces the
//! stock Holy Hand Grenade's defaults field for field, down to its render size
//! (0x4042185E), and differs only where PX changed the weapon.
//!
//! [`import`] copies how the weapon fires onto a slot -- the same fields
//! `copy_from` copies from a stock weapon -- so a pack can take a PX weapon's
//! exact numbers and still override single params or supply its own art.

use game::offsets::{
    WEAPON_CLUSTER_RENDER_TIMER, WEAPON_CLUSTER_SPRITE, WEAPON_ENTRY_SIZE, WEAPON_FIRE_TYPE,
    WEAPON_SPRITE_ID,
};
use game::weapon::WeaponEntry;

/// Every PX `.wep` starts with these bytes.
pub const WEP_TAG: [u8; 4] = [0x06, 0xDD, 0xBB, 0x5A];
/// Where PX's string list starts: after the entry and three more dwords.
const STRINGS_AT: usize = 0x1E4;
/// PX numbers its own sprites from here; WA has no such sprite.
const PX_SPRITE_BASE: i32 = 100_000;
/// Fields that hold a sprite id: the flight sprite and its siblings, as
/// `WeaponEntry::retarget_sprite_id` lists them, the cluster's, and what a
/// homing cluster draws while it homes (its render timer).
const SPRITE_FIELDS: [usize; 8] = [
    WEAPON_SPRITE_ID,
    0x64,
    0x68,
    0xDC,
    0xE0,
    0xE4,
    WEAPON_CLUSTER_SPRITE,
    WEAPON_CLUSTER_RENDER_TIMER,
];
/// StrikeFire reads its bomb and cluster art from a slab 0x14 bytes after
/// the ordinary projectile fields. These offsets are parameters on other
/// fire types, so only inspect them for a strike record.
const STRIKE_SPRITE_FIELDS: [usize; 4] = [0x74, 0x7C, 0x138, 0x140];
/// `fire_params.missile_type`: nonzero for anything that flies.
const MISSILE_TYPE: usize = 0xA4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wep {
    /// Pack-relative path, for logs.
    pub path: String,
    /// PX's full weapon name, the first string.
    pub name: String,
    /// WA's weapon table entry, `WEAPON_ENTRY_SIZE` bytes.
    pub record: Vec<u8>,
}

impl Wep {
    fn read_i32(&self, offset: usize) -> i32 {
        i32::from_le_bytes(self.record[offset..offset + 4].try_into().unwrap())
    }

    pub fn fire_type(&self) -> i32 {
        self.read_i32(WEAPON_FIRE_TYPE)
    }
}

pub fn parse_wep(path: &str, bytes: &[u8]) -> Result<Wep, String> {
    if bytes.len() < STRINGS_AT || bytes[..4] != WEP_TAG {
        return Err(format!("{path} is not a Project X weapon file"));
    }
    let record = bytes[4..4 + WEAPON_ENTRY_SIZE].to_vec();
    let mut at = STRINGS_AT;
    let mut name = None;
    if at + 4 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        at += 4;
        if len <= 256 && at + len <= bytes.len() {
            name = Some(bytes[at..at + len].iter().map(|&b| b as char).collect::<String>());
        }
    }
    let wep = Wep {
        path: path.to_string(),
        name: name.ok_or_else(|| format!("{path} has no weapon name"))?,
        record,
    };
    // WA dispatches on fire type 0..=4 (OpenWA's stock table); anything else
    // is not a WA weapon and must not reach WA's fire code.
    if !(0..=4).contains(&wep.fire_type()) {
        return Err(format!("{path}: fire type {} is not one WA knows", wep.fire_type()));
    }
    Ok(wep)
}

/// Give `entry` the imported weapon's firing, as `copy_from` does for a
/// stock weapon. The slot keeps its panel place, availability and name.
///
/// A sprite field naming one of PX's own sprites, or a flying weapon's
/// sprite left 0 for a PX script to supply, keeps the slot's stock sprite:
/// WA has neither, and the pack's own art replaces it afterwards.
pub fn import(entry: &mut WeaponEntry, wep: &Wep) {
    let before = *entry;
    let mut source = before;
    source.raw.copy_from_slice(&wep.record);
    entry.copy_fire_from(&source);
    let flies = entry.read_i32(MISSILE_TYPE) != 0;
    let strike = entry.is_strike();
    for off in SPRITE_FIELDS.into_iter().chain(STRIKE_SPRITE_FIELDS.into_iter().filter(|_| strike)) {
        let v = entry.read_i32(off);
        if v >= PX_SPRITE_BASE || (off == WEAPON_SPRITE_ID && v == 0 && flies && !strike) {
            entry.write_i32(off, before.read_i32(off));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::offsets::{WEAPON_DAMAGE, WEAPON_DEFINED, WEAPON_GRAVITY_PCT};

    fn hellish() -> Wep {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../mods/examples/hellish_grenade/hellish.wep");
        parse_wep("hellish.wep", &std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn px_hellish_grenade_is_a_holy_grenade_that_burns() {
        let w = hellish();
        assert_eq!(w.name, "Hellish Hand-Grenade");
        assert_eq!(w.record.len(), WEAPON_ENTRY_SIZE);
        // The stock Holy Hand Grenade's shape (OpenWA init_weapon_defaults)...
        assert_eq!((w.fire_type(), w.read_i32(0x34), w.read_i32(0x38)), (1, 2, 3));
        assert_eq!(w.read_i32(0x88), 3000, "fixed 3 s fuse");
        assert_eq!(w.read_i32(0xA8), 0x4042185E, "the Holy Grenade's render size");
        // ...with PX's changes: a weaker blast that bursts into fire.
        assert_eq!(w.read_i32(WEAPON_DAMAGE), 70);
        assert_eq!(w.read_i32(0x54), 80);
        assert_eq!(w.read_i32(0xF0), 2, "CreateFire1 on detonation, as the Petrol Bomb");
        assert_eq!(w.read_i32(0x1C), 3000, "3 s retreat");
    }

    #[test]
    fn import_takes_the_firing_and_keeps_the_slot() {
        let w = hellish();
        let mut slot = WeaponEntry { raw: [0; WEAPON_ENTRY_SIZE] };
        slot.write_i32(WEAPON_DEFINED, 7);
        slot.write_i32(WEAPON_SPRITE_ID, 0x38);
        slot.write_i32(0x24, 5); // availability stays the slot's
        import(&mut slot, &w);
        assert_eq!(slot.read_i32(WEAPON_DAMAGE), 70);
        assert_eq!(slot.read_i32(WEAPON_GRAVITY_PCT), 100);
        assert_eq!(slot.read_i32(WEAPON_DEFINED), 7);
        assert_eq!(slot.read_i32(0x24), 5);
        assert_eq!(
            slot.read_i32(WEAPON_SPRITE_ID),
            0x38,
            "PX's own sprite (100000) is not a WA sprite; the slot keeps its own"
        );
    }

    #[test]
    fn a_homing_clusters_px_sprite_keeps_the_slots_own() {
        let mut w = hellish();
        w.record[WEAPON_CLUSTER_RENDER_TIMER..WEAPON_CLUSTER_RENDER_TIMER + 4]
            .copy_from_slice(&PX_SPRITE_BASE.to_le_bytes());
        let mut slot = WeaponEntry { raw: [0; WEAPON_ENTRY_SIZE] };
        slot.write_i32(WEAPON_CLUSTER_RENDER_TIMER, 53);
        import(&mut slot, &w);
        assert_eq!(slot.read_i32(WEAPON_CLUSTER_RENDER_TIMER), 53);
    }

    #[test]
    fn a_strikes_px_sprites_keep_the_slots_own() {
        let mut w = hellish();
        let set = |w: &mut Wep, off: usize, v: i32| w.record[off..off + 4].copy_from_slice(&v.to_le_bytes());
        set(&mut w, WEAPON_FIRE_TYPE, 3);
        set(&mut w, 0x34, 0);
        set(&mut w, 0x74, PX_SPRITE_BASE);
        set(&mut w, 0x7C, 251); // a stock id a pack overrides stays as is
        set(&mut w, 0x138, PX_SPRITE_BASE);
        let mut slot = WeaponEntry { raw: [0; WEAPON_ENTRY_SIZE] };
        slot.write_i32(0x74, 52);
        slot.write_i32(0x138, 53);
        import(&mut slot, &w);
        assert_eq!((slot.read_i32(0x74), slot.read_i32(0x7C), slot.read_i32(0x138)), (52, 251, 53));
    }

    #[test]
    fn other_files_are_refused() {
        assert!(parse_wep("x.wep", b"WEPFILE\0").unwrap_err().contains("not a Project X"));
        let mut bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../mods/examples/hellish_grenade/hellish.wep"
        ))
        .unwrap();
        bytes[4 + WEAPON_FIRE_TYPE] = 9;
        assert!(parse_wep("x.wep", &bytes).unwrap_err().contains("fire type 9"));
    }
}
