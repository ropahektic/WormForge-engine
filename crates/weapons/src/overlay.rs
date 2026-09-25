use crate::stock::StockWeapon;

/// What a weapon shows in the stock weapon panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelIcon {
    /// Borrow a stock weapon's icon (`panel_icon = "banana"`).
    Stock(StockWeapon),
    /// A pack PNG, quantized to the stock icons' colours at install
    /// (`panel_icon = "sprites/icon.png"`).
    File {
        path: String,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
}
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
pub enum HomingMode {
    Lock,
    Avoid,
    Dodge,
}

/// LuaActor cluster spray. Not the stock cluster-bomb slab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClusterHoming {
    #[default]
    Off,
    /// Steer toward the owner's aim cursor (or world pointer).
    Aim,
    /// Steer toward the nearest other worm.
    Worm,
}

impl ClusterHoming {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "" | "false" => Some(Self::Off),
            "lock" | "aim" | "cursor" | "true" => Some(Self::Aim),
            "worm" | "target" => Some(Self::Worm),
            "avoid" | "dodge" => Some(Self::Aim),
            _ => None,
        }
    }
}

/// One native cluster body: N bits, trail, arc, impact / bounce / homing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterSpec {
    pub count: i32,
    pub spread: i32,
    pub power: i32,
    /// 0 = use the spawn sprite.
    pub sprite: i32,
    pub trail: i32,
    /// Pack GIF/PNG for puffs; resolved after gfx init.
    pub trail_path: Option<String>,
    /// 0 = no bounce. 100 = grenade-like.
    pub bounce_pct: i32,
    /// 0 = bounce forever (until fuse/prox/rest). N = explode after N terrain hits.
    pub bounces: i32,
    /// 0 = no fuse. Else ms until boom (grenade timer).
    pub fuse_ms: i32,
    /// 0 = no proximity. Else px radius — boom when a worm enters (mine-like).
    pub proximity: i32,
    pub homing: ClusterHoming,
    /// Explode on first land (ignores bounce).
    pub impact: bool,
}

impl Default for ClusterSpec {
    fn default() -> Self {
        Self {
            count: 1,
            spread: 0,
            power: 0,
            sprite: 0,
            trail: 0,
            trail_path: None,
            bounce_pct: 0,
            bounces: 0,
            fuse_ms: 0,
            proximity: 0,
            homing: ClusterHoming::Off,
            impact: false,
        }
    }
}

impl ClusterSpec {
    /// Stock CreateClusters: 30% base × 0x18 px/tick, as 16.16.
    pub const STOCK_POWER: i32 = ((0x1e << 16) / 100) * 0x18;

    /// `kind = "cluster"` with no extra keys.
    pub fn bouquet(impact: bool) -> Self {
        Self {
            count: 5,
            spread: 45,
            power: Self::STOCK_POWER,
            sprite: 0,
            trail: 609,
            trail_path: None,
            bounce_pct: 0,
            bounces: 0,
            fuse_ms: 0,
            proximity: 0,
            homing: ClusterHoming::Off,
            impact,
        }
    }
}

impl HomingMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lock" => Some(Self::Lock),
            "avoid" => Some(Self::Avoid),
            "dodge" => Some(Self::Dodge),
            "off" | "" => None,
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnExplode {
    pub damage: i32,
    pub id: i32,
    /// Detonate at the feet / just below, not the sprite centre.
    pub below: bool,
    /// Blast during spawn, before the next frame. Invisible push/cluster seeds use this.
    pub now: bool,
}

impl Default for SpawnExplode {
    fn default() -> Self {
        Self {
            damage: 50,
            id: 100,
            below: true,
            now: false,
        }
    }
}

/// Extra LuaActor fired from a weapon or actor step (explode / proximity / place).
#[derive(Debug, Clone)]
pub struct AttachedSpawn {
    pub sprite: i32,
    pub delta: u32,
    pub vx: i32,
    pub vy: i32,
    /// How many bits to spray. 1 = a single body (old banana kick).
    pub count: i32,
    /// Fan width in degrees around straight up. Stock cluster is 45°.
    pub spread: i32,
    /// Launch speed 16.16 along each ray. 0 falls back to stock CreateClusters.
    pub power: i32,
    /// Linger smoke puffs (smklt25). 0 = none. Prefer ClusterSpec.trail.
    pub trail: i32,
    /// Pack GIF/PNG for puffs; resolved after gfx init.
    pub trail_path: Option<String>,
    /// Pack-relative sheet installed at gfx init; resolved at emit.
    pub path: Option<String>,
    pub spawn: ActorSpawn,
}

impl Default for AttachedSpawn {
    fn default() -> Self {
        Self {
            sprite: 0,
            delta: 0,
            vx: 0,
            vy: 0,
            count: 1,
            spread: 0,
            power: 0,
            trail: 0,
            trail_path: None,
            path: None,
            spawn: ActorSpawn::default(),
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
    /// Draw the body. `false` / `sprite = false` uses the 1023 blit reject.
    pub visible: bool,
    /// Bake opaque silhouette(s) from the GIF/PNG. Sub-toggles below choose
    /// what the mask collides with; when `mask` is true and a sub-flag is
    /// omitted, that sub-flag defaults to true.
    ///
    /// Multi-frame GIFs swap the walk-grid stamp each anim frame (**experimental**
    /// — works, but standing worms on a morphing outline can feel cursed).
    pub mask: bool,
    pub mask_worms: bool,
    pub mask_land: bool,
    pub mask_weapons: bool,
    pub mask_actors: bool,
    /// `wa.cursor.spawn`: the body is the world cursor. Native code moves it to
    /// `wa.world.pointer()` every frame and plays the stock cursor ease-in.
    pub cursor: bool,
    /// Non-zero: Lua `on.frame` / `on.land` owns policy. `0` keeps native sugar.
    pub brain_id: u32,
    pub explode: Option<SpawnExplode>,
    /// Spawned when this actor blasts. Does not replace the blast.
    pub on_explode: Option<Box<AttachedSpawn>>,
    /// `kind = "cluster"` / `cluster = { ... }`. Native fan, bounce, homing.
    pub cluster: Option<ClusterSpec>,
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
            visible: true,
            mask: false,
            mask_worms: false,
            mask_land: false,
            mask_weapons: false,
            mask_actors: false,
            cursor: false,
            brain_id: 0,
            explode: None,
            on_explode: None,
            cluster: None,
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
    /// Swallow stock FireWeapon for this slot (pointer-driven weapons).
    pub eat_fire: bool,
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
    /// Replacement sheets for the mole strike's digging frames (`moledive`,
    /// `moledig1..3`), as stock sprite id -> pack path. Global for the match,
    /// as PX's `SetSpriteOverride` is: every mole uses them.
    pub mole_dig_rotation: Vec<(i32, String)>,
    /// Stock worm sprite id -> pack sheet path, scoped to this weapon slot.
    /// With an alliance, the sheet is only for worms of that team colour
    /// (0 red .. 5 cyan); without one, it is for every worm.
    pub worm_sprites: Vec<(i32, Option<u8>, String)>,
    pub layer: Option<u32>,
    pub depth: Option<u32>,
    pub depth_prefix: Option<String>,
    pub reserve_prefix: Option<String>,
    pub gpu_prefix: Option<String>,
    pub truecolor_prefix: Option<String>,
    /// `worm_sprites` sheets go to the GPU atlas (truecolor) by default: they
    /// are only ever drawn through the RQ intercept, never as WA missile
    /// sprites, so the palette wash buys nothing. `params.gpu_worm_sprites =
    /// false` keeps the 8-bit path.
    pub gpu_worm_sprites: bool,
    /// `weaponlnk*` sheets are one-shot animations (equip / unfold) driven
    /// frame by frame, not aim-angle sheets. Default false = aim-wizard
    /// semantics (phase is the aim angle; draw poses hold or play).
    pub worm_sprites_animate: bool,
    /// Pack-relative WAV paths (`sounds = { ... }`) loaded into DSSound slot 451+.
    pub extra_sounds: Vec<(String, std::path::PathBuf)>,
    pub cluster_sprite_id: Option<i32>,
    pub cluster_damage: Option<i32>,
    pub cluster_bounce_pct: Option<i32>,
    pub cluster_gravity_pct: Option<i32>,
    pub cluster_wind_pct: Option<i32>,
    /// Homing discriminator written onto the cluster sub-pellet slab.
    pub cluster_homing: Option<HomingMode>,
    /// The overlay slot's weapon panel icon: a stock weapon's, or a pack PNG.
    pub panel_icon: Option<PanelIcon>,
    /// MineEntity fire_params[0] — proximity radius in pixels.
    pub mine_proximity: Option<i32>,
    /// MineEntity fire_params[1] — arm delay. Negative keeps scheme MineFuse.
    pub mine_fuse: Option<i32>,
    /// MineEntity fire_params[2] — detect bitmask (1 << entity+0x30).
    pub mine_mask: Option<i32>,
    /// MineEntity fire_params[3] — beep fuse after a worm is seen.
    pub mine_trigger: Option<i32>,
    /// MineEntity fire_params[6] — CreateExplosion id/radius arg.
    pub mine_blast: Option<i32>,
    /// Lua `on_explode` callback. Does not steal MineEntity.
    pub on_explode_id: u32,
    pub explode_spawn: Option<AttachedSpawn>,
    /// Lua `on_proximity` callback. First scan hit only.
    pub on_proximity_id: u32,
    pub proximity_spawn: Option<AttachedSpawn>,
    /// Lua `on_place` callback. MineEntity ctor / body spawn.
    pub on_place_id: u32,
    pub place_spawn: Option<AttachedSpawn>,
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
            eat_fire: false,
            on_select_id: 0,
            spawn: None,
            on_click_id: 0,
            on_fire_id: 0,
            fire_spawn: None,
            sprite_path: None,
            extra_sheets: Vec::new(),
            mole_dig_rotation: Vec::new(),
            worm_sprites: Vec::new(),
            layer: None,
            depth: None,
            depth_prefix: None,
            reserve_prefix: None,
            gpu_prefix: None,
            truecolor_prefix: None,
            gpu_worm_sprites: true,
            worm_sprites_animate: false,
            extra_sounds: Vec::new(),
            cluster_sprite_id: None,
            cluster_damage: None,
            cluster_bounce_pct: None,
            cluster_gravity_pct: None,
            cluster_wind_pct: None,
            cluster_homing: None,
            panel_icon: None,
            mine_proximity: None,
            mine_fuse: None,
            mine_mask: None,
            mine_trigger: None,
            mine_blast: None,
            on_explode_id: 0,
            explode_spawn: None,
            on_proximity_id: 0,
            proximity_spawn: None,
            on_place_id: 0,
            place_spawn: None,
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
                    now: false,
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
        // Strike/airstrike fire_type only when Lua asked for the land cursor or a
        // click-spawn. A bare `on_click` callback must NOT flip the slot to
        // fire_type 3 (that plants markerr and ends the turn).
        self.actor_spawn().is_some() || self.cursor_on_select
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
        // Replace flight + HM burn seed fields (entry +0xE0/+0xE4), not only +0x60.
        let old = entry.sprite_id();
        entry.retarget_sprite_id(old, v);
    }
    if let Some(v) = params.cluster_sprite_id {
        entry.set_cluster_sprite_id(v);
    }
    if let Some(v) = params.cluster_damage {
        entry.set_cluster_damage(v);
    }
    if let Some(v) = params.cluster_bounce_pct {
        entry.set_cluster_bounce_pct(v);
    }
    if let Some(v) = params.cluster_gravity_pct {
        entry.set_cluster_gravity_pct(v);
    }
    if let Some(v) = params.cluster_wind_pct {
        entry.set_cluster_wind_pct(v);
    }
    if let Some(mode) = params.cluster_homing {
        apply_cluster_homing(entry, mode, params.cluster_sprite_id);
    }
    if entry.is_mine() {
        if let Some(v) = params.mine_proximity {
            entry.set_mine_proximity(v);
        }
        if let Some(v) = params.mine_fuse {
            entry.set_mine_fuse(v);
        }
        if let Some(v) = params.mine_mask {
            entry.set_mine_mask(v);
        }
        if let Some(v) = params.mine_trigger {
            entry.set_mine_trigger(v);
        }
        if let Some(v) = params.mine_blast {
            entry.set_mine_blast(v);
        }
    }
    if params.uses_click_actor() {
        apply_drop_fire(entry);
    } else if params.uses_lua_projectile() && entry.fire_type() == 2 && entry.fire_method() == 1 {
        // Keep stock mine placement/retreat, but dispatch its body through
        // CreateWeaponProjectile (placed method 2), where Lua owns the spawn.
        entry.set_fire_method(2);
    }
}

fn apply_cluster_homing(entry: &mut WeaponEntry, mode: HomingMode, sprite: Option<i32>) {
    entry.set_cluster_missile_type(1);
    let (params, kind, t1, t2) = match mode {
        HomingMode::Lock => ([2, 0x83, 0x32, 0x64, 0x32], 1, 0x1F4, 0x1194),
        HomingMode::Avoid => ([3, 0x86, 0xA, 0x14, 0x32], 2, 0x1F4, 0x1194),
        HomingMode::Dodge => ([2, 0x87, 0x32, 0x64, 0x32], 2, 0x64, 0x26AC),
    };
    entry.set_cluster_homing_params(params);
    entry.set_cluster_homing_kind(kind, t1, t2);
    // Homing burn blits `render_timer`, not the flight sprite. Stock cluster
    // leaves that 0; copy the bit sprite so the body stays visible.
    let body = sprite.filter(|id| *id != 0).unwrap_or_else(|| entry.cluster_sprite_id());
    if body != 0 {
        entry.set_cluster_render_timer(body);
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
    fn apply_params_retargets_homing_burn_seeds() {
        let mut entry = WeaponEntry::default();
        entry.set_sprite_id(59);
        entry.write_i32(0xAC, 58);
        entry.write_i32(0xE0, 58);
        entry.write_i32(0xE4, 59);
        apply_params(
            &mut entry,
            &OverlayParams {
                sprite_id: Some(50), // e.g. sprite = "wroll"
                ..Default::default()
            },
        );
        assert_eq!(entry.sprite_id(), 50);
        assert_eq!(entry.read_i32(0xAC), 50);
        assert_eq!(entry.read_i32(0xE0), 50);
        assert_eq!(entry.read_i32(0xE4), 50);
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
    fn apply_params_writes_cluster_bit_sprite() {
        let mut entry = WeaponEntry::default();
        apply_params(
            &mut entry,
            &OverlayParams {
                sprite_id: Some(51),
                cluster_sprite_id: Some(54),
                cluster_bounce_pct: Some(100),
                ..Default::default()
            },
        );
        assert_eq!(entry.sprite_id(), 51);
        assert_eq!(entry.cluster_sprite_id(), 54);
        assert_eq!(entry.read_i32(0x124), 54);
        assert_eq!(entry.read_i32(0x13C), 100);
    }

    #[test]
    fn lua_cluster_bouquet_defaults() {
        let c = ClusterSpec::bouquet(true);
        assert_eq!(c.count, 5);
        assert_eq!(c.spread, 45);
        assert_eq!(c.power, ClusterSpec::STOCK_POWER);
        assert_eq!(c.trail, 609);
        assert_eq!(c.bounce_pct, 0);
        assert_eq!(c.homing, ClusterHoming::Off);
        assert!(c.impact);
        assert_eq!(ClusterHoming::parse("worm"), Some(ClusterHoming::Worm));
        assert_eq!(ClusterHoming::parse("aim"), Some(ClusterHoming::Aim));
        assert_eq!(ClusterHoming::parse("off"), Some(ClusterHoming::Off));
    }

    #[test]
    fn apply_params_writes_cluster_homing_lock() {
        let mut entry = WeaponEntry::default();
        apply_params(
            &mut entry,
            &OverlayParams {
                cluster_sprite_id: Some(94),
                cluster_homing: Some(HomingMode::Lock),
                ..Default::default()
            },
        );
        assert_eq!(entry.read_i32(0x168), 1);
        assert_eq!(entry.read_i32(0x174), 2);
        assert_eq!(entry.read_i32(0x188), 1);
        assert_eq!(entry.read_i32(0x170), 94);
    }

    #[test]
    fn apply_params_writes_mine_brain() {
        let mut entry = WeaponEntry::default();
        entry.set_fire_type(2);
        entry.set_fire_method(1);
        apply_params(
            &mut entry,
            &OverlayParams {
                sprite_id: Some(3),
                damage: Some(80),
                mine_proximity: Some(40),
                mine_fuse: Some(3000),
                mine_mask: Some(1 << 18),
                mine_trigger: Some(1000),
                mine_blast: Some(50),
                ..Default::default()
            },
        );
        assert_eq!(entry.sprite_id(), 3);
        assert_eq!(entry.damage(), 80);
        assert_eq!(entry.read_i32(0x3C), 40);
        assert_eq!(entry.read_i32(0x40), 3000);
        assert_eq!(entry.read_i32(0x44), 1 << 18);
        assert_eq!(entry.read_i32(0x48), 1000);
        assert_eq!(entry.read_i32(0x54), 50);
        assert_eq!(entry.fire_method(), 1);
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
    fn click_function_alone_does_not_force_strike() {
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
        // Bare on_click is not a land-cursor weapon — keep stock fire type.
        assert_eq!(entry.fire_type(), 1);
        assert!(!OverlayParams {
            on_click_id: 7,
            ..Default::default()
        }
        .uses_click_actor());
    }

    #[test]
    fn click_function_with_cursor_applies_strike() {
        let mut entry = WeaponEntry::default();
        entry.set_fire_type(1);
        apply_params(
            &mut entry,
            &OverlayParams {
                on_click_id: 7,
                cursor_on_select: true,
                ..Default::default()
            },
        );
        assert_eq!(entry.fire_type(), 3);
        assert!(OverlayParams {
            on_click_id: 7,
            cursor_on_select: true,
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
        apply_params(
            &mut entry,
            &OverlayParams {
                on_fire_id: 1,
                ..Default::default()
            },
        );
        assert_eq!(entry.fire_type(), 2);
        assert_eq!(entry.fire_method(), 2);
        assert_eq!(entry.requires_aiming(), 0);
        assert_eq!(entry.read_i32(0x1c), 5000);
        assert_eq!(entry.defined(), 1);
        assert_eq!(entry.read_i32(0x24), 1);
        apply_params(&mut mine, &OverlayParams::default());
        assert_eq!(mine.fire_method(), 1);
    }

    #[test]
    fn mine_on_explode_does_not_steal_placement() {
        let mut entry = WeaponEntry::default();
        entry.set_fire_type(2);
        entry.set_fire_method(1);
        apply_params(
            &mut entry,
            &OverlayParams {
                on_explode_id: 9,
                explode_spawn: Some(AttachedSpawn {
                    sprite: 1023,
                    spawn: ActorSpawn {
                        visible: false,
                        explode: Some(SpawnExplode {
                            damage: 0,
                            id: 40,
                            below: true,
                            now: true,
                        }),
                        damage_worms: false,
                        ..ActorSpawn::default()
                    },
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        assert_eq!(entry.fire_method(), 1);
        assert!(!OverlayParams {
            on_explode_id: 9,
            ..Default::default()
        }
        .uses_lua_projectile());
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
