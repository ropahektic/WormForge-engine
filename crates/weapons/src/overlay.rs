use crate::stock::StockWeapon;
use actors::is_drop;
use game::weapon::WeaponEntry;
use gfx::{FeetPlatform, SpriteStrip};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpawnFrom {
    #[default]
    Sky,
    Cursor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpriteOrigin {
    #[default]
    Center,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScreenBlit {
    #[default]
    World,
    /// Stretch over the whole window / display.
    Fill,
    /// Centered on top of fill overlays.
    Front,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnExplode {
    pub damage: i32,
    pub id: i32,
    /// Detonate at the feet / just below, not the sprite centre.
    pub below: bool,
}

impl Default for SpawnExplode {
    fn default() -> Self {
        Self {
            damage: 50,
            id: 100,
            below: true,
        }
    }
}

/// Lua-chosen actor spawn. Native code runs the primitives; the pack names them.
#[derive(Debug, Clone)]
pub struct ActorSpawn {
    pub from: SpawnFrom,
    pub origin: SpriteOrigin,
    pub gravity: bool,
    pub loop_anim: bool,
    pub collide: bool,
    pub collide_terrain: bool,
    pub collide_worms: bool,
    pub damage_terrain: bool,
    pub damage_worms: bool,
    /// Survive the blast (Concrete Donkey). Knock-up is explosion physics.
    pub persist: bool,
    /// Die when the body is below the water line.
    pub drown: bool,
    /// Non-zero: Lua `on.frame` / `on.land` owns policy. `0` keeps native sugar.
    pub brain_id: u32,
    pub explode: Option<SpawnExplode>,
    pub sprite_w: i32,
    pub sprite_h: i32,
    pub feet: Option<FeetPlatform>,
    pub screen: ScreenBlit,
}

impl Default for ActorSpawn {
    fn default() -> Self {
        Self {
            from: SpawnFrom::Sky,
            origin: SpriteOrigin::Center,
            gravity: true,
            loop_anim: true,
            collide: true,
            collide_terrain: true,
            collide_worms: true,
            damage_terrain: true,
            damage_worms: true,
            persist: false,
            drown: true,
            brain_id: 0,
            explode: None,
            sprite_w: 0,
            sprite_h: 0,
            feet: None,
            screen: ScreenBlit::World,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OverlayParams {
    pub name: Option<String>,
    pub damage: Option<i32>,
    pub bounce_pct: Option<i32>,
    pub gravity_pct: Option<i32>,
    pub wind_pct: Option<i32>,
    pub sprite_id: Option<i32>,
    pub sprite_sheet: Option<SpriteStrip>,
    /// Clone this stock weapon's fire onto `slot`. Panel identity stays.
    pub copy_from: Option<StockWeapon>,
    /// Legacy `actor = "drop"` string. Prefer `on_click.spawn`.
    pub actor: Option<String>,
    /// Panel weapon (selectable). `replace()` is already a weapon; Lua still names it.
    pub is_weapon: bool,
    /// On select, show the strike targeting cursor.
    pub cursor_on_select: bool,
    /// Lua `on_select` callback id (`0` = none). Runs when the slot is chosen.
    pub on_select_id: u32,
    /// On cursor click, spawn a LuaActor with these options (GUI statuses).
    pub spawn: Option<ActorSpawn>,
    /// Lua `on_click` callback id (`0` = none). Slot owner only.
    pub on_click_id: u32,
    /// Lua `on_fire` callback id (`0` = none). Slot owner only.
    pub on_fire_id: u32,
    /// Spawn a LuaActor at launch instead of a stock MissileEntity.
    pub fire_spawn: Option<ActorSpawn>,
    /// Pack-relative sprite path last loaded for this claim.
    pub sprite_path: Option<String>,
    /// Extra PNG/GIF/frame-folders installed at gfx init (`sprites = { ... }`).
    pub extra_sheets: Vec<(String, SpriteStrip)>,
    /// Stock worm sprite id -> pack sheet path, scoped to this weapon slot.
    pub worm_sprites: Vec<(i32, String)>,
    /// Display layer for this pack's sheets. Sprites default to the game layer
    /// (3), which draws over terrain; 1 and 2 sit below it. Note these are
    /// palette-bound: a sheet installed against another layer's PaletteContext
    /// renders with the wrong colours, so this is rarely what you want.
    pub layer: Option<u32>,
    /// Render-queue depth for this pack's sheets, and which paths it applies
    /// to. This is the real draw-order control: high is further back, so a
    /// value between terrain (0x180000) and soil (0x1b0000) puts an object
    /// behind the landscape. Unlike `layer` it never touches palettes.
    pub depth: Option<u32>,
    pub depth_prefix: Option<String>,
    /// Opt these sheets into the GPU renderer: packed into a texture atlas
    /// and drawn as quads, never occupying one of the 223 shared WA sprite
    /// slots and never touching the map palette. Supersedes `truecolor_prefix`
    /// for sheets it matches.
    pub gpu_prefix: Option<String>,
    /// Opt these sheets into RGBA rendering on the D3D9 shader backend.
    /// Other renderers retain the stock palette approximation.
    pub truecolor_prefix: Option<String>,
    /// Sheets whose paths start with this take exact palette slots instead of
    /// being snapped to the map's colours. For art the map palette cannot
    /// express -- a saturated team colour on an earth-toned map, where the
    /// nearest colour to blue is black.
    pub reserve_prefix: Option<String>,
    /// Pack-relative WAV paths (`sounds = { ... }`) loaded into DSSound slot 451+.
    pub extra_sounds: Vec<(String, std::path::PathBuf)>,
}

impl Default for OverlayParams {
    fn default() -> Self {
        Self {
            name: None,
            damage: None,
            bounce_pct: None,
            gravity_pct: None,
            wind_pct: None,
            sprite_id: None,
            sprite_sheet: None,
            copy_from: None,
            actor: None,
            is_weapon: true,
            cursor_on_select: false,
            on_select_id: 0,
            spawn: None,
            on_click_id: 0,
            on_fire_id: 0,
            fire_spawn: None,
            layer: None,
            depth: None,
            depth_prefix: None,
            gpu_prefix: None,
            truecolor_prefix: None,
            reserve_prefix: None,
            sprite_path: None,
            extra_sheets: Vec::new(),
            worm_sprites: Vec::new(),
            extra_sounds: Vec::new(),
        }
    }
}

impl OverlayParams {
    fn fill_sheet_dims(spawn: &mut ActorSpawn, sheet: &SpriteStrip) {
        if spawn.sprite_w == 0 {
            if let Some(frame) = sheet.frames.first() {
                spawn.sprite_w = frame.width as i32;
                spawn.sprite_h = frame.height as i32;
            }
        }
        if spawn.feet.is_none() {
            spawn.feet = sheet.feet_platform();
        }
    }

    pub fn actor_spawn(&self) -> Option<ActorSpawn> {
        if let Some(spawn) = &self.spawn {
            let mut spawn = spawn.clone();
            if let Some(sheet) = &self.sprite_sheet {
                Self::fill_sheet_dims(&mut spawn, sheet);
            }
            return Some(spawn);
        }
        if self.actor.as_deref().is_some_and(is_drop) {
            let mut spawn = ActorSpawn {
                explode: Some(SpawnExplode {
                    damage: self.damage.unwrap_or(50),
                    id: 100,
                    below: true,
                }),
                persist: true,
                drown: true,
                ..ActorSpawn::default()
            };
            if let Some(sheet) = &self.sprite_sheet {
                if let Some(frame) = sheet.frames.first() {
                    spawn.sprite_w = frame.width as i32;
                    spawn.sprite_h = frame.height as i32;
                }
                spawn.feet = sheet.feet_platform();
            }
            return Some(spawn);
        }
        None
    }

    pub fn uses_click_actor(&self) -> bool {
        self.actor_spawn().is_some() || self.cursor_on_select || self.on_click_id != 0
    }

    /// Launch-time LuaActor (bazooka-style). Not a strike click.
    pub fn projectile_spawn(&self) -> Option<ActorSpawn> {
        let mut spawn = self.fire_spawn.clone()?;
        if let Some(sheet) = &self.sprite_sheet {
            Self::fill_sheet_dims(&mut spawn, sheet);
        }
        Some(spawn)
    }

    pub fn uses_lua_projectile(&self) -> bool {
        self.on_fire_id != 0 || self.fire_spawn.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct OverlayClaim {
    pub mod_id: String,
    pub logical_id: Option<String>,
    pub slot: StockWeapon,
    pub params: OverlayParams,
}

#[derive(Debug, Clone)]
pub struct SkippedClaim {
    pub claim: OverlayClaim,
    pub winner_mod: String,
}

#[derive(Debug, Clone, Default)]
pub struct ResolveResult {
    pub applied: Vec<OverlayClaim>,
    pub skipped: Vec<SkippedClaim>,
}

/// First claim per stock slot wins. Later claims for that slot are skipped;
/// the rest of those mods are unaffected.
pub fn resolve_claims(claims: Vec<OverlayClaim>) -> ResolveResult {
    let mut occupied: Vec<(StockWeapon, String)> = Vec::new();
    let mut result = ResolveResult::default();
    for claim in claims {
        if let Some((_, winner)) = occupied.iter().find(|(slot, _)| *slot == claim.slot) {
            result.skipped.push(SkippedClaim {
                winner_mod: winner.clone(),
                claim,
            });
        } else {
            occupied.push((claim.slot, claim.mod_id.clone()));
            result.applied.push(claim);
        }
    }
    result
}

pub fn apply_params(entry: &mut WeaponEntry, params: &OverlayParams) {
    if let Some(v) = params.damage {
        entry.set_damage(v);
    }
    if let Some(v) = params.bounce_pct {
        entry.set_bounce_pct(v);
    }
    if let Some(v) = params.gravity_pct {
        entry.set_gravity_pct(v);
    }
    if let Some(v) = params.wind_pct {
        entry.set_wind_pct(v);
    }
    if let Some(v) = params.sprite_id {
        entry.set_sprite_id(v);
    }
    if params.uses_click_actor() {
        apply_drop_fire(entry);
    } else if params.uses_lua_projectile() && entry.fire_type() == 2 && entry.fire_method() == 1 {
        // Keep stock mine placement/retreat, but dispatch its body through
        // CreateWeaponProjectile (placed method 2), where Lua owns the spawn.
        entry.set_fire_method(2);
    }
}

/// Strike click-target + our falling actor. Panel/defined stay on the slot.
pub fn apply_drop_fire(entry: &mut WeaponEntry) {
    entry.set_requires_aiming(0);
    entry.set_creates_projectile(1);
    entry.set_unknown_18(1);
    entry.set_fire_type(3);
    entry.set_special_subtype(0);
    entry.set_fire_method(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stock::StockWeapon;

    fn claim(mod_id: &str, slot: StockWeapon) -> OverlayClaim {
        OverlayClaim {
            mod_id: mod_id.into(),
            logical_id: None,
            slot,
            params: OverlayParams::default(),
        }
    }

    #[test]
    fn first_loaded_keeps_bazooka_second_claim_skipped() {
        let resolved = resolve_claims(vec![
            claim("alice.ice", StockWeapon::Bazooka),
            claim("bob.nuke", StockWeapon::Bazooka),
            claim("bob.nuke", StockWeapon::Grenade),
        ]);
        assert_eq!(resolved.applied.len(), 2);
        assert_eq!(resolved.applied[0].mod_id, "alice.ice");
        assert_eq!(resolved.applied[0].slot, StockWeapon::Bazooka);
        assert_eq!(resolved.applied[1].mod_id, "bob.nuke");
        assert_eq!(resolved.applied[1].slot, StockWeapon::Grenade);
        assert_eq!(resolved.skipped.len(), 1);
        assert_eq!(resolved.skipped[0].claim.mod_id, "bob.nuke");
        assert_eq!(resolved.skipped[0].winner_mod, "alice.ice");
    }

    #[test]
    fn two_mods_can_overlay_different_slots() {
        let resolved = resolve_claims(vec![
            claim("a", StockWeapon::Bazooka),
            claim("b", StockWeapon::Grenade),
        ]);
        assert!(resolved.skipped.is_empty());
        assert_eq!(resolved.applied.len(), 2);
    }

    #[test]
    fn apply_params_writes_sprite_wind_and_bounce_slots() {
        let mut entry = WeaponEntry::default();
        apply_params(
            &mut entry,
            &OverlayParams {
                sprite_id: Some(50),
                gravity_pct: Some(25),
                wind_pct: Some(200),
                bounce_pct: Some(100),
                ..Default::default()
            },
        );
        assert_eq!(entry.sprite_id(), 50);
        assert_eq!(entry.gravity_pct(), 25);
        assert_eq!(entry.wind_pct(), 200);
        assert_eq!(entry.bounce_pct(), 100);
        assert_eq!(entry.read_i32(0x74), 200);
        assert_eq!(entry.read_i32(0x78), 100);
    }

    #[test]
    fn click_spawn_table_applies_strike_cursor() {
        let mut entry = WeaponEntry::default();
        entry.set_requires_aiming(1);
        entry.set_fire_type(1);
        apply_params(
            &mut entry,
            &OverlayParams {
                cursor_on_select: true,
                spawn: Some(ActorSpawn {
                    explode: Some(SpawnExplode::default()),
                    ..ActorSpawn::default()
                }),
                ..Default::default()
            },
        );
        assert_eq!(entry.requires_aiming(), 0);
        assert_eq!(entry.fire_type(), 3);
        assert_eq!(entry.creates_projectile(), 1);
    }

    #[test]
    fn click_function_applies_strike_cursor() {
        let mut entry = WeaponEntry::default();
        entry.set_requires_aiming(1);
        entry.set_fire_type(1);
        apply_params(
            &mut entry,
            &OverlayParams {
                on_click_id: 7,
                ..Default::default()
            },
        );
        assert_eq!(entry.requires_aiming(), 0);
        assert_eq!(entry.fire_type(), 3);
        assert!(OverlayParams {
            on_click_id: 7,
            ..Default::default()
        }
        .uses_click_actor());
    }

    #[test]
    fn drop_string_still_means_click_spawn() {
        let p = OverlayParams {
            actor: Some("drop".into()),
            damage: Some(40),
            ..Default::default()
        };
        let spawn = p.actor_spawn().expect("drop sugar");
        assert_eq!(spawn.explode.unwrap().damage, 40);
        assert!(p.uses_click_actor());
    }

    #[test]
    fn lua_projectile_keeps_stock_fire_type() {
        let mut entry = WeaponEntry::default();
        entry.set_requires_aiming(1);
        entry.set_fire_type(1);
        apply_params(
            &mut entry,
            &OverlayParams {
                fire_spawn: Some(ActorSpawn::default()),
                ..Default::default()
            },
        );
        assert_eq!(entry.requires_aiming(), 1);
        assert_eq!(entry.fire_type(), 1);
        assert!(!OverlayParams {
            fire_spawn: Some(ActorSpawn::default()),
            ..Default::default()
        }
        .uses_click_actor());
        assert!(OverlayParams {
            fire_spawn: Some(ActorSpawn::default()),
            ..Default::default()
        }
        .uses_lua_projectile());
    }

    #[test]
    fn lua_mine_keeps_placement_and_retreat_but_uses_existing_spawn_wrap() {
        let mut mine = WeaponEntry::default();
        mine.set_fire_type(2);
        mine.set_fire_method(1);
        mine.set_requires_aiming(0);
        mine.write_i32(0x1c, 5000);
        let mut entry = WeaponEntry::default();
        entry.set_defined(1);
        entry.write_i32(0x24, 1);
        entry.copy_fire_from(&mine);
        apply_params(&mut entry, &OverlayParams { on_fire_id: 1, ..Default::default() });
        assert_eq!(entry.fire_type(), 2);
        assert_eq!(entry.fire_method(), 2);
        assert_eq!(entry.requires_aiming(), 0);
        assert_eq!(entry.read_i32(0x1c), 5000);
        assert_eq!(entry.defined(), 1);
        assert_eq!(entry.read_i32(0x24), 1);
        apply_params(&mut mine, &OverlayParams::default());
        assert_eq!(mine.fire_method(), 1); // stock mine is unaffected
    }

    #[test]
    fn select_function_does_not_apply_strike() {
        let mut entry = WeaponEntry::default();
        entry.set_requires_aiming(1);
        entry.set_fire_type(1);
        apply_params(
            &mut entry,
            &OverlayParams {
                on_select_id: 3,
                ..Default::default()
            },
        );
        assert_eq!(entry.requires_aiming(), 1);
        assert_eq!(entry.fire_type(), 1);
        assert!(!OverlayParams {
            on_select_id: 3,
            ..Default::default()
        }
        .uses_click_actor());
    }

    #[test]
    fn custom_sheet_leaves_stock_sprite_until_engine_installs() {
        let mut entry = WeaponEntry::default();
        entry.set_sprite_id(48);
        apply_params(
            &mut entry,
            &OverlayParams {
                sprite_sheet: Some(gfx::SpriteStrip {
                    frames: vec![gfx::RgbaFrame {
                        width: 1,
                        height: 1,
                        rgba: vec![0, 255, 0, 255],
                    }],
                    fps: 10,
                    looping: false,
                }),
                ..Default::default()
            },
        );
        assert_eq!(entry.sprite_id(), 48);
    }
}
