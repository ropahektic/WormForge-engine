//! Stock weapon ids 0..70. Slot index == table index.

use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u32)]
pub enum StockWeapon {
    None = 0,
    Bazooka = 1,
    HomingMissile = 2,
    Mortar = 3,
    HomingPigeon = 4,
    SheepLauncher = 5,
    Grenade = 6,
    ClusterBomb = 7,
    BananaBomb = 8,
    BattleAxe = 9,
    Earthquake = 10,
    Shotgun = 11,
    Handgun = 12,
    Uzi = 13,
    Minigun = 14,
    Longbow = 15,
    FirePunch = 16,
    DragonBall = 17,
    Kamikaze = 18,
    SuicideBomber = 19,
    Prod = 20,
    Dynamite = 21,
    Mine = 22,
    Sheep = 23,
    SuperSheep = 24,
    AquaSheep = 25,
    MoleBomb = 26,
    AirStrike = 27,
    NapalmStrike = 28,
    MailStrike = 29,
    MineStrike = 30,
    MoleSquadron = 31,
    BlowTorch = 32,
    PneumaticDrill = 33,
    Girder = 34,
    BaseballBat = 35,
    GirderPack = 36,
    NinjaRope = 37,
    Bungee = 38,
    Parachute = 39,
    Teleport = 40,
    ScalesOfJustice = 41,
    SuperBanana = 42,
    HolyGrenade = 43,
    FlameThrower = 44,
    SalvationArmy = 45,
    MbBomb = 46,
    PetrolBomb = 47,
    Skunk = 48,
    MingVase = 49,
    SheepStrike = 50,
    CarpetBomb = 51,
    MadCow = 52,
    OldWoman = 53,
    Donkey = 54,
    NuclearTest = 55,
    Armageddon = 56,
    SkipGo = 57,
    Surrender = 58,
    SelectWorm = 59,
    Freeze = 60,
    MagicBullet = 61,
    JetPack = 62,
    LowGravity = 63,
    FastWalk = 64,
    LaserSight = 65,
    Invisibility = 66,
    DamageX2 = 67,
    CrateSpy = 68,
    DoubleTurnTime = 69,
    CrateShower = 70,
}

pub const STOCK_NAMES: &[&str] = &[
    "none",
    "bazooka",
    "homing_missile",
    "mortar",
    "homing_pigeon",
    "sheep_launcher",
    "grenade",
    "cluster_bomb",
    "banana_bomb",
    "battle_axe",
    "earthquake",
    "shotgun",
    "handgun",
    "uzi",
    "minigun",
    "longbow",
    "fire_punch",
    "dragon_ball",
    "kamikaze",
    "suicide_bomber",
    "prod",
    "dynamite",
    "mine",
    "sheep",
    "super_sheep",
    "aqua_sheep",
    "mole_bomb",
    "air_strike",
    "napalm_strike",
    "mail_strike",
    "mine_strike",
    "mole_squadron",
    "blow_torch",
    "pneumatic_drill",
    "girder",
    "baseball_bat",
    "girder_pack",
    "ninja_rope",
    "bungee",
    "parachute",
    "teleport",
    "scales_of_justice",
    "super_banana",
    "holy_grenade",
    "flame_thrower",
    "salvation_army",
    "mb_bomb",
    "petrol_bomb",
    "skunk",
    "ming_vase",
    "sheep_strike",
    "carpet_bomb",
    "mad_cow",
    "old_woman",
    "donkey",
    "nuclear_test",
    "armageddon",
    "skip_go",
    "surrender",
    "select_worm",
    "freeze",
    "magic_bullet",
    "jet_pack",
    "low_gravity",
    "fast_walk",
    "laser_sight",
    "invisibility",
    "damage_x2",
    "crate_spy",
    "double_turn_time",
    "crate_shower",
];

impl StockWeapon {
    pub fn from_index(index: u32) -> Option<Self> {
        if index > 70 {
            return None;
        }
        Some(unsafe { std::mem::transmute::<u32, StockWeapon>(index) })
    }

    pub fn index(self) -> usize {
        self as u32 as usize
    }

    pub fn name(self) -> &'static str {
        STOCK_NAMES[self.index()]
    }
}

impl FromStr for StockWeapon {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_stock_weapon(s)
    }
}

pub fn parse_stock_weapon(s: &str) -> Result<StockWeapon, String> {
    let key = s.trim().to_ascii_lowercase().replace('-', "_").replace(' ', "_");
    STOCK_NAMES
        .iter()
        .position(|n| *n == key)
        .and_then(|i| StockWeapon::from_index(i as u32))
        .ok_or_else(|| format!("unknown stock weapon '{s}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bazooka_is_slot_1() {
        assert_eq!(parse_stock_weapon("bazooka").unwrap(), StockWeapon::Bazooka);
        assert_eq!(StockWeapon::Bazooka.index(), 1);
        assert_eq!(STOCK_NAMES.len(), 71);
    }
}
