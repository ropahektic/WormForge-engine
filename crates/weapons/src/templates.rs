//! Fire style × body kind → stock `copy_from` and Gfx.dir sprite stems.
//!
//! WA dispatch (`FireType` / `FireMethod` from openWA):
//! - Power: type 1 method 3 `CreateWeaponProjectile` + space power bar
//! - Hitscan: type 1 method 2 `ProjectileFire` (shotgun, handgun, uzi, minigun)
//! - Drop: type 2 (mine method 1 `MineEntity`; dynamite/animals method 2 `MissileEntity`)
//! - Cursor: type 3 Strike (airstrikes); Teleport is type 4 subtype 10
//!
//! Body on a `MissileEntity`:
//! - missile: `missile_type` 2, contact, no bounce
//! - grenade: bounce + fuse, `impact_type` 1
//! - homing: `missile_type` 1; click target then space. Pigeon / magic bullet
//!   steer around dirt; homing missile does not.
//! - cluster: `impact_spawn_kind` 1/3; sub-pellet slab is `cluster_params`
//!   (sprite at cluster_params[6] = entry+0x124). Overlay writes that slab.

use crate::stock::StockWeapon;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireStyle {
    Power,
    Hitscan,
    Drop,
    Cursor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    Missile,
    Grenade,
    Homing,
    Cluster,
    Hitscan,
    Placed,
    Strike,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteUse {
    pub role: &'static str,
    pub name: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FireTemplate {
    pub id: &'static str,
    pub fire: FireStyle,
    pub body: BodyKind,
    pub copy_from: StockWeapon,
    /// WA `WeaponEntry.fire_type`.
    pub fire_type: i32,
    /// WA `WeaponEntry.fire_method` (0 for strike/special).
    pub fire_method: i32,
    pub sprites: &'static [SpriteUse],
    pub notes: &'static str,
}

pub const FIRE_TEMPLATES: &[FireTemplate] = &[
    FireTemplate {
        id: "power_missile",
        fire: FireStyle::Power,
        body: BodyKind::Missile,
        copy_from: StockWeapon::Bazooka,
        fire_type: 1,
        fire_method: 3,
        sprites: &[SpriteUse {
            role: "body",
            name: "missile",
        }],
        notes: "Aim, hold space to power, release. Explodes on contact.",
    },
    FireTemplate {
        id: "power_grenade",
        fire: FireStyle::Power,
        body: BodyKind::Grenade,
        copy_from: StockWeapon::Grenade,
        fire_type: 1,
        fire_method: 3,
        sprites: &[SpriteUse {
            role: "body",
            name: "grenade",
        }],
        notes: "Same throw path as bazooka; fuse + bounce.",
    },
    FireTemplate {
        id: "power_homing",
        fire: FireStyle::Power,
        body: BodyKind::Homing,
        copy_from: StockWeapon::HomingMissile,
        fire_type: 1,
        fire_method: 3,
        sprites: &[
            SpriteUse {
                role: "body",
                name: "hmissil1",
            },
            SpriteUse {
                role: "alt",
                name: "hmissil2",
            },
        ],
        notes: "Click target, then space-power. Does not steer around terrain.",
    },
    FireTemplate {
        id: "power_pigeon",
        fire: FireStyle::Power,
        body: BodyKind::Homing,
        copy_from: StockWeapon::HomingPigeon,
        fire_type: 1,
        fire_method: 3,
        sprites: &[
            SpriteUse {
                role: "body",
                name: "pigeonr0",
            },
            SpriteUse {
                role: "turn",
                name: "pigeonr1",
            },
            SpriteUse {
                role: "turn",
                name: "pigeonr2",
            },
            SpriteUse {
                role: "turn",
                name: "pigeonr3",
            },
        ],
        notes: "Homing that tries to avoid dirt. Magic bullet (copy_from magic_bullet, sprite bullet) dodges better.",
    },
    FireTemplate {
        id: "power_cluster",
        fire: FireStyle::Power,
        body: BodyKind::Cluster,
        copy_from: StockWeapon::ClusterBomb,
        fire_type: 1,
        fire_method: 3,
        sprites: &[
            SpriteUse {
                role: "body",
                name: "cluster",
            },
            SpriteUse {
                role: "bit",
                name: "clustlet",
            },
        ],
        notes: "Fuse throw; explosion ejects bits upward. Banana uses sprite banana. Bit sprite is cluster_params[6] (entry+0x124).",
    },
    FireTemplate {
        id: "hitscan_uzi",
        fire: FireStyle::Hitscan,
        body: BodyKind::Hitscan,
        copy_from: StockWeapon::Uzi,
        fire_type: 1,
        fire_method: 2,
        sprites: &[
            SpriteUse {
                role: "impact",
                name: "uzihit",
            },
            SpriteUse {
                role: "case",
                name: "uzicase",
            },
        ],
        notes: "Aim + tap/hold; first solid pixel/worm. Table sprite_id is 0. Lua bullets: land.trace + hurt.",
    },
    FireTemplate {
        id: "hitscan_shotgun",
        fire: FireStyle::Hitscan,
        body: BodyKind::Hitscan,
        copy_from: StockWeapon::Shotgun,
        fire_type: 1,
        fire_method: 2,
        sprites: &[
            SpriteUse {
                role: "impact",
                name: "shothit",
            },
            SpriteUse {
                role: "case",
                name: "shotcase",
            },
        ],
        notes: "Same ProjectileFire path as uzi/minigun/handgun. Longbow is method 4 ArrowEntity.",
    },
    FireTemplate {
        id: "drop_mine",
        fire: FireStyle::Drop,
        body: BodyKind::Placed,
        copy_from: StockWeapon::Mine,
        fire_type: 2,
        fire_method: 1,
        sprites: &[
            SpriteUse {
                role: "armed",
                name: "mineon",
            },
            SpriteUse {
                role: "idle",
                name: "mineoff",
            },
        ],
        notes: "Drops a MineEntity at the worm. Lua on_fire needs fire_method 2 so CreateWeaponProjectile wrap owns the body.",
    },
    FireTemplate {
        id: "drop_dynamite",
        fire: FireStyle::Drop,
        body: BodyKind::Placed,
        copy_from: StockWeapon::Dynamite,
        fire_type: 2,
        fire_method: 2,
        sprites: &[SpriteUse {
            role: "body",
            name: "dynamite",
        }],
        notes: "Placed MissileEntity with fuse. Sheep/cow/skunk share this fire path.",
    },
    FireTemplate {
        id: "drop_sheep",
        fire: FireStyle::Drop,
        body: BodyKind::Placed,
        copy_from: StockWeapon::Sheep,
        fire_type: 2,
        fire_method: 2,
        sprites: &[
            SpriteUse {
                role: "walk",
                name: "sheepwlk",
            },
            SpriteUse {
                role: "fall",
                name: "sheepfal",
            },
        ],
        notes: "Self-driven animal at worm feet.",
    },
    FireTemplate {
        id: "cursor_strike",
        fire: FireStyle::Cursor,
        body: BodyKind::Strike,
        copy_from: StockWeapon::AirStrike,
        fire_type: 3,
        fire_method: 0,
        sprites: &[
            SpriteUse {
                role: "bomb",
                name: "airmisl",
            },
            SpriteUse {
                role: "plane",
                name: "airjetb",
            },
        ],
        notes: "Click map; plane drops munitions. Mail/napalm/mine/sheep/carpet strikes are the same family.",
    },
    FireTemplate {
        id: "cursor_teleport",
        fire: FireStyle::Cursor,
        body: BodyKind::Strike,
        copy_from: StockWeapon::Teleport,
        fire_type: 4,
        fire_method: 0,
        sprites: &[SpriteUse {
            role: "cursor",
            name: "cursorr",
        }],
        notes: "Special subtype 10, not Strike. Click is the teleport destination.",
    },
];

pub fn template_by_id(id: &str) -> Option<&'static FireTemplate> {
    FIRE_TEMPLATES.iter().find(|t| t.id == id)
}
