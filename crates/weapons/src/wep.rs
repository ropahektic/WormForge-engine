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
/// A strike's plane sprite, where other fire types keep their fire method.
const STRIKE_PLANE: usize = 0x38;
/// WA 3.8.1's last stock sprite. PX numbers some sprites it adds itself just
/// past it -- the Banana Express's plane is 704 -- which WA does not have.
const LAST_STOCK_SPRITE: i32 = 696;
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
    keep_stock_sprites(entry, &before);
}

/// The record offsets a pack may set one by one (`patch = { [0x50] = 100 }`):
/// the same ranges `copy_from` and [`import`] copy. Everything else is the
/// slot's identity (names, panel place, availability) or padding.
pub fn patchable(offset: usize) -> bool {
    offset % 4 == 0
        && ((0x0C..0x10).contains(&offset)
            || (0x14..0x24).contains(&offset)
            || (WEAPON_FIRE_TYPE..WEAPON_ENTRY_SIZE).contains(&offset))
}

/// Check one `patch` entry the way [`parse_wep`] checks a file: an offset the
/// slot may take, and a fire type WA dispatches on.
pub fn check_patch(offset: i64, value: i32) -> Result<usize, String> {
    let off = usize::try_from(offset).ok().filter(|&o| patchable(o)).ok_or_else(|| {
        format!(
            "patch offset {offset:#x} is not a weapon field; use dword offsets 0x0C, 0x14-0x20 or 0x30-0x1CC"
        )
    })?;
    if off == WEAPON_FIRE_TYPE && !(0..=4).contains(&value) {
        return Err(format!("patch fire type {value} is not one WA knows (0-4)"));
    }
    Ok(off)
}

/// Write single fields onto `entry` -- the web editor's diff against a stock
/// base, `copy_from` having laid that base down first. A field naming a PX
/// sprite keeps the slot's stock one, as [`import`] does.
pub fn patch(entry: &mut WeaponEntry, fields: &[(usize, i32)]) {
    let before = *entry;
    for &(off, v) in fields {
        if patchable(off) {
            entry.write_i32(off, v);
        }
    }
    keep_stock_sprites(entry, &before);
}

/// Strike munitions reuse the missile slab 0x14 bytes later in the entry.
const STRIKE_SHIFT: usize = 0x14;

/// One named weapon field the web editor may put in `patch = { damage = 70 }`.
/// `slab` fields shift by [`STRIKE_SHIFT`] when the weapon's fire type is 3.
#[derive(Clone, Copy, Debug)]
struct PatchName {
    name: &'static str,
    off: usize,
    slab: bool,
}

/// Field names match the web editor's `wep_schema.js` (no `unk_*` unknowns).
const PATCH_NAMES: &[PatchName] = &[
    // Header / handling
    PatchName { name: "requires_aiming", off: 0x0C, slab: false },
    PatchName { name: "shots", off: 0x14, slab: false },
    PatchName { name: "uses_turn", off: 0x18, slab: false },
    PatchName { name: "retreat", off: 0x1C, slab: false },
    PatchName { name: "creates_projectile", off: 0x20, slab: false },
    PatchName { name: "fire_type", off: 0x30, slab: false },
    PatchName { name: "hold_pose", off: 0x34, slab: false },
    PatchName { name: "special_subtype", off: 0x34, slab: false },
    PatchName { name: "subtype_param", off: 0x34, slab: false },
    PatchName { name: "fire_method", off: 0x38, slab: false },
    PatchName { name: "plane_sprite", off: 0x38, slab: false },
    PatchName { name: "special_param", off: 0x38, slab: false },
    PatchName { name: "power_pct", off: 0x1C8, slab: false },
    PatchName { name: "weight2", off: 0x1CC, slab: false },
    // Missile / strike body (slab)
    PatchName { name: "pellets", off: 0x3C, slab: true },
    PatchName { name: "spread", off: 0x40, slab: true },
    PatchName { name: "grenade_style", off: 0x44, slab: true },
    PatchName { name: "collision_radius", off: 0x48, slab: true },
    PatchName { name: "bias", off: 0x4C, slab: true },
    PatchName { name: "damage", off: 0x50, slab: true },
    PatchName { name: "blast", off: 0x54, slab: true },
    PatchName { name: "sprite", off: 0x60, slab: true },
    PatchName { name: "impact", off: 0x64, slab: true },
    PatchName { name: "flight_flags", off: 0x68, slab: true },
    PatchName { name: "trail", off: 0x6C, slab: true },
    PatchName { name: "gravity_pct", off: 0x70, slab: true },
    PatchName { name: "wind_pct", off: 0x74, slab: true },
    PatchName { name: "bounce_pct", off: 0x78, slab: true },
    PatchName { name: "bounce_extra", off: 0x7C, slab: true },
    PatchName { name: "friction_pct", off: 0x84, slab: true },
    PatchName { name: "fuse_default", off: 0x88, slab: true },
    PatchName { name: "fuse", off: 0x8C, slab: true },
    PatchName { name: "alt_sprite", off: 0x90, slab: true },
    PatchName { name: "glow_enable", off: 0x94, slab: true },
    PatchName { name: "glow_sprite", off: 0x98, slab: true },
    PatchName { name: "glow_time", off: 0x9C, slab: true },
    PatchName { name: "space_control", off: 0xA0, slab: true },
    PatchName { name: "missile_type", off: 0xA4, slab: true },
    PatchName { name: "render_size", off: 0xA8, slab: true },
    PatchName { name: "burn_sprite", off: 0xAC, slab: true },
    PatchName { name: "homing_strength", off: 0xB0, slab: true },
    PatchName { name: "homing_sprite", off: 0xB4, slab: true },
    PatchName { name: "homing_turn", off: 0xB8, slab: true },
    PatchName { name: "homing_accel", off: 0xBC, slab: true },
    PatchName { name: "homing_speed", off: 0xC0, slab: true },
    PatchName { name: "homing_kind", off: 0xC4, slab: true },
    PatchName { name: "homing_arm", off: 0xC8, slab: true },
    PatchName { name: "homing_time", off: 0xCC, slab: true },
    PatchName { name: "detonation", off: 0xF0, slab: true },
    PatchName { name: "cluster_count", off: 0xF8, slab: true },
    PatchName { name: "cluster_speed", off: 0xFC, slab: true },
    PatchName { name: "cluster_power", off: 0x100, slab: true },
    PatchName { name: "cluster_angle", off: 0x104, slab: true },
    PatchName { name: "cluster_spread", off: 0x108, slab: true },
    PatchName { name: "c_collision_radius", off: 0x10C, slab: true },
    PatchName { name: "c_bias", off: 0x110, slab: true },
    PatchName { name: "c_damage", off: 0x114, slab: true },
    PatchName { name: "c_blast", off: 0x118, slab: true },
    // How many sprites from `c_sprite` the bits take in turn: a bit's is
    // c_sprite + (its index - 1) % c_sprite_count (the MissileEntity
    // constructor, 0x507E70). 0 on a slot whose stock weapon has no bits, so
    // a record that makes bits must set it, or WA divides by zero.
    PatchName { name: "c_sprite_count", off: 0x120, slab: true },
    PatchName { name: "c_sprite", off: 0x124, slab: true },
    PatchName { name: "c_impact", off: 0x128, slab: true },
    PatchName { name: "c_flags", off: 0x12C, slab: true },
    PatchName { name: "c_trail", off: 0x130, slab: true },
    PatchName { name: "c_gravity_pct", off: 0x134, slab: true },
    PatchName { name: "c_wind_pct", off: 0x138, slab: true },
    PatchName { name: "c_bounce_pct", off: 0x13C, slab: true },
    PatchName { name: "c_friction_pct", off: 0x148, slab: true },
    PatchName { name: "c_fuse_default", off: 0x14C, slab: true },
    PatchName { name: "c_fuse", off: 0x150, slab: true },
    PatchName { name: "c_space_control", off: 0x164, slab: true },
    PatchName { name: "c_missile_type", off: 0x168, slab: true },
    PatchName { name: "c_render_size", off: 0x16C, slab: true },
    PatchName { name: "c_burn_sprite", off: 0x170, slab: true },
    PatchName { name: "c_homing_strength", off: 0x174, slab: true },
    PatchName { name: "c_homing_sprite", off: 0x178, slab: true },
    PatchName { name: "c_homing_turn", off: 0x17C, slab: true },
    PatchName { name: "c_homing_accel", off: 0x180, slab: true },
    PatchName { name: "c_homing_speed", off: 0x184, slab: true },
    PatchName { name: "c_homing_kind", off: 0x188, slab: true },
    PatchName { name: "c_homing_arm", off: 0x18C, slab: true },
    PatchName { name: "c_homing_time", off: 0x190, slab: true },
    // Hitscan
    PatchName { name: "h_shots", off: 0x3C, slab: false },
    PatchName { name: "h_spread", off: 0x40, slab: false },
    PatchName { name: "h_delay", off: 0x44, slab: false },
    PatchName { name: "h_flag", off: 0x48, slab: false },
    PatchName { name: "h_range", off: 0x50, slab: false },
    PatchName { name: "h_radius", off: 0x54, slab: false },
    PatchName { name: "h_damage", off: 0x5C, slab: false },
    PatchName { name: "h_kind", off: 0x64, slab: false },
    PatchName { name: "h_range_px", off: 0x68, slab: false },
    // Arrow / spray
    PatchName { name: "a_param0", off: 0x3C, slab: false },
    PatchName { name: "s_count", off: 0x3C, slab: false },
    PatchName { name: "s_spread", off: 0x40, slab: false },
    PatchName { name: "s_life", off: 0x44, slab: false },
    PatchName { name: "s_speed", off: 0x48, slab: false },
    // Mine
    PatchName { name: "m_proximity", off: 0x3C, slab: false },
    PatchName { name: "m_fuse", off: 0x40, slab: false },
    PatchName { name: "m_mask", off: 0x44, slab: false },
    PatchName { name: "m_trigger", off: 0x48, slab: false },
    PatchName { name: "m_damage", off: 0x50, slab: false },
    PatchName { name: "m_blast", off: 0x54, slab: false },
    // Strike header
    PatchName { name: "k_count", off: 0x3C, slab: false },
    PatchName { name: "k_spacing", off: 0x40, slab: false },
    PatchName { name: "k_munition", off: 0x4C, slab: false },
];

/// Resolve a web-editor field name to an absolute weapon-entry offset.
/// `strike` is true when the weapon's fire type is (or will be) 3.
pub fn patch_offset_for_name(name: &str, strike: bool) -> Result<usize, String> {
    let field = PATCH_NAMES.iter().find(|f| f.name == name).ok_or_else(|| {
        format!("patch field `{name}` is not a known weapon field; use names like damage, fuse, cluster_count")
    })?;
    let off = if field.slab && strike {
        field.off + STRIKE_SHIFT
    } else {
        field.off
    };
    Ok(off)
}

/// Stock slots whose table entry is fire type 3 (strike plane + munition slab).
pub fn stock_is_strike(slot: crate::stock::StockWeapon) -> bool {
    use crate::stock::StockWeapon::*;
    matches!(
        slot,
        AirStrike
            | NapalmStrike
            | MailStrike
            | MineStrike
            | MoleSquadron
            | SheepStrike
            | CarpetBomb
    )
}

fn keep_stock_sprites(entry: &mut WeaponEntry, before: &WeaponEntry) {
    let flies = entry.read_i32(MISSILE_TYPE) != 0;
    let strike = entry.is_strike();
    for off in SPRITE_FIELDS.into_iter().chain(STRIKE_SPRITE_FIELDS.into_iter().filter(|_| strike)) {
        let v = entry.read_i32(off);
        if v >= PX_SPRITE_BASE || (off == WEAPON_SPRITE_ID && v == 0 && flies && !strike) {
            entry.write_i32(off, before.read_i32(off));
        }
    }
    if strike && entry.read_i32(STRIKE_PLANE) > LAST_STOCK_SPRITE {
        entry.write_i32(STRIKE_PLANE, before.read_i32(STRIKE_PLANE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::offsets::{WEAPON_DAMAGE, WEAPON_DEFINED, WEAPON_GRAVITY_PCT};

    fn hellish() -> Wep {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/hellish.wep");
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

        // PX's own plane (the Banana Express's 704) is past WA's sprites.
        set(&mut w, STRIKE_PLANE, 704);
        slot.write_i32(STRIKE_PLANE, 86);
        import(&mut slot, &w);
        assert_eq!(slot.read_i32(STRIKE_PLANE), 86);
        set(&mut w, STRIKE_PLANE, 87);
        import(&mut slot, &w);
        assert_eq!(slot.read_i32(STRIKE_PLANE), 87, "a stock jet is kept");
    }

    #[test]
    fn patch_writes_fire_fields_and_keeps_the_slots_identity_and_art() {
        let mut slot = WeaponEntry { raw: [0; WEAPON_ENTRY_SIZE] };
        slot.write_i32(WEAPON_DEFINED, 7);
        slot.write_i32(WEAPON_SPRITE_ID, 0x38);
        slot.write_i32(WEAPON_FIRE_TYPE, 1);
        slot.write_i32(MISSILE_TYPE, 2);
        patch(
            &mut slot,
            &[
                (WEAPON_DAMAGE, 70),
                (0xF0, 1),
                (0xF8, 5),
                (WEAPON_DEFINED, 1), // not patchable: silently kept
                (WEAPON_SPRITE_ID, PX_SPRITE_BASE + 3),
            ],
        );
        assert_eq!(slot.read_i32(WEAPON_DAMAGE), 70);
        assert_eq!((slot.read_i32(0xF0), slot.read_i32(0xF8)), (1, 5));
        assert_eq!(slot.read_i32(WEAPON_DEFINED), 7);
        assert_eq!(slot.read_i32(WEAPON_SPRITE_ID), 0x38, "a PX sprite id keeps the stock art");

        assert_eq!(check_patch(0x50, 100), Ok(0x50));
        assert_eq!(check_patch(0x0C, 1), Ok(0x0C));
        assert!(check_patch(0x10, 1).unwrap_err().contains("not a weapon field"), "defined");
        assert!(check_patch(0x24, 1).unwrap_err().contains("not a weapon field"), "availability");
        assert!(check_patch(0x52, 1).unwrap_err().contains("not a weapon field"), "unaligned");
        assert!(check_patch(0x1D0, 1).unwrap_err().contains("not a weapon field"), "past the entry");
        assert!(check_patch(-4, 1).is_err());
        assert!(check_patch(0x30, 9).unwrap_err().contains("fire type 9"));
    }

    #[test]
    fn a_cluster_sprite_count_is_named_and_shifts_on_a_strike() {
        // The divisor the MissileEntity constructor takes a bit's sprite by.
        assert_eq!(patch_offset_for_name("c_sprite_count", false), Ok(0x120));
        assert_eq!(patch_offset_for_name("c_sprite_count", true), Ok(0x134));
    }

    #[test]
    fn other_files_are_refused() {
        assert!(parse_wep("x.wep", b"WEPFILE\0").unwrap_err().contains("not a Project X"));
        let mut bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/hellish.wep"
        ))
        .unwrap();
        bytes[4 + WEAPON_FIRE_TYPE] = 9;
        assert!(parse_wep("x.wep", &bytes).unwrap_err().contains("fire type 9"));
    }

    #[test]
    fn highlander_full_scheme_records_are_valid() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../mods/examples/highlander/weps");
        for (slot, name, fire_type) in [
            ("earthquake", "Electric Grenade", 1),
            ("napalm_strike", "Lightning Strike", 3),
            ("teleport", "Super Tele", 3),
        ] {
            let path = root.join(format!("{slot}.wep"));
            let bytes = std::fs::read(&path).unwrap();
            let wep = parse_wep(&path.display().to_string(), &bytes).unwrap();
            assert_eq!(wep.name, name);
            assert_eq!(wep.fire_type(), fire_type);
        }
    }
}
