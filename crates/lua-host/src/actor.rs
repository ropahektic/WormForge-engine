//! Per-actor Lua brains. Callbacks run on WorldEntity FRAME_FINISH — the same
//! lockstep pump wkGolf uses for fuse freeze, idle-arm, and worm teleports.
//! HUD must not call these verbs.

use game::entity::{read_i32, write_i32};
use game::offsets::{
    ACTOR_BOOM_ID, ACTOR_DAMAGE, ACTOR_DEAD, ACTOR_DELTA, ACTOR_FLAG_MIRROR_X, ACTOR_FLAGS,
    ACTOR_MAGIC, ACTOR_OWNER, ACTOR_SEQ, ACTOR_SPR_H, ACTOR_SPR_W, ACTOR_SPRITE, CLASS_CRATE,
    CLASS_MINE, WORM_INDEX,
    CLASS_LAND, CLASS_MISSILE, CLASS_OIL, CLASS_WORM, ENT_CLASS_TYPE, ENT_POS_X, ENT_POS_Y, ENT_SPEED_X,
    ENT_SPEED_Y, ENT_WORLD, ENT_WORM_TEAM, LUA_ACTOR_MAGIC, MAX_TEAMS, WORLD_TEAM_AMMO_GROUP,
    WORLD_TEAM_STRIDE,
    ENT_GRAVITY_SCALE, ENT_WIND_SCALE, WORM_AIM_ANGLE, WORM_FACING, WORM_JET_FUEL,
    WORM_SHOTS_FIRED, WORM_SHOTS_TO_FIRE, WORM_KEY_DOWN, WORM_KEY_LEFT, WORM_KEY_RIGHT,
    WORM_KEY_UP, WORM_SELECTED_WEAPON, WORM_STATE, WORM_STATE_AIRBORNE, WORM_STATE_DRILLING,
    WORM_STATE_FROZEN, WORM_STATE_JETPACKING, WORM_STATE_PARACHUTING, WORM_STATE_PREFIRING,
    WORM_STATE_PUNCHING, WORM_STATE_STANDING, WORM_STATE_TELEPORTING, WORM_TURN_ACTIVE,
    WORM_TURN_SIDE, WORM_STATE_BUNGEE, WORM_STATE_TUMBLING, ENT_COLLISION_MASK,
    WORM_STATE_KAMIKAZE,
};
use mlua::{Function, Lua, Table, UserData, UserDataFields, UserDataMethods, Value};
use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use weapons::{parse_stock_weapon, ActorSpawn, StockWeapon};

use crate::PARKED_LUA;

#[derive(Clone, Copy, Debug, Default)]
pub struct ActorEvent {
    pub land: bool,
    pub left_ground: bool,
    pub hit_worm: bool,
    pub in_water: bool,
    /// Falloff-adjusted blast damage accumulated by stock WorldEntity logic.
    pub damage: i32,
}

/// `wa.fire.spawn`: one of WA's fires, started as the Petrol Bomb starts its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FireArgs {
    /// The worm or actor it is started for: its world's fire store is the
    /// fire's parent, and a worm's team owns it.
    pub owner: u32,
    /// Position and speed, 16.16 (PX's CFireDesc X, Y, SpX, SpY).
    pub x: i32,
    pub y: i32,
    pub vx: i32,
    pub vy: i32,
    /// PX's CFireDesc Count, Duration, UpPower and Damage.
    pub count: i32,
    pub duration: i32,
    pub up_power: i32,
    pub damage: i32,
    /// How much harder this fire's hits on worms are, 16.16 (0x10000 as WA's):
    /// their damage and their knockback.
    pub damage_scale: i32,
    pub knock_scale: i32,
    /// Drawn added to the scene, its colours multiplied by this 0xRRGGBB
    /// (PX's SetColorMod mode 1), on the GPU renderer.
    pub tint: Option<u32>,
}

/// `wa.fire.spawn` defaults: what WA's Petrol Bomb passes (CreateFire1 with
/// the stock record), so a bare spawn is a petrol fire.
pub const FIRE_DEFAULTS: FireArgs = FireArgs {
    owner: 0, x: 0, y: 0, vx: 0, vy: 0x4_0000, count: 8, duration: 4000, up_power: 100, damage: 20,
    damage_scale: 0x1_0000, knock_scale: 0x1_0000, tint: None,
};

/// What `wa.world.trace` / `wa.world.collides` test against (`with`).
pub const SHAPE_TERRAIN: u32 = 1;
pub const SHAPE_WORMS: u32 = 2;
pub const SHAPE_CRATES: u32 = 4;
pub const SHAPE_OIL: u32 = 8;
pub const SHAPE_MINES: u32 = 16;
pub const SHAPE_GRAVES: u32 = 32;

/// A circle, swept from (x0, y0) to (x1, y1) or tested at (x0, y0): 16.16,
/// radius in pixels, against the `with` bits, without `ignore`'s objects.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShapeQuery {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub radius: i32,
    pub with: u32,
    pub ignore: Vec<u32>,
}

/// A sweep's result. `hit`: it touched something on the way; (x, y) is the
/// last free centre before, (nx, ny) the unit normal away from what it
/// touched, `obj` that object (0 terrain). `starts_solid`: it overlapped at
/// the start; `left` is where it first came free, if it did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShapeHit {
    pub hit: bool,
    pub x: i32,
    pub y: i32,
    pub nx: f64,
    pub ny: f64,
    pub obj: u32,
    pub starts_solid: bool,
    pub left: Option<(i32, i32)>,
}

/// `wa.crates.spawn`: one of WA's crates, its contents WA's own random pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrateArgs {
    /// Where, 16.16.
    pub x: i32,
    pub y: i32,
    /// WA's kind (PX's ECrateType): 1 weapon, 2 health, 4 utility.
    pub kind: i32,
    /// A health crate's health.
    pub health: i32,
    /// How it arrives (PX's EDropType): 0 where it is, 1 teleported in,
    /// 2 on a parachute.
    pub drop: i32,
}

/// `wa.weapons.launch`: a real WA missile, fired from a weapon slot's fire
/// params, as `FireWeapon__CreateWeaponProjectile` fires one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LaunchArgs {
    /// The stock slot whose fire params the missile flies with.
    pub slot: u32,
    /// The worm it is fired for: its team and worm number.
    pub owner: u32,
    /// Position and speed, 16.16.
    pub x: i32,
    pub y: i32,
    pub vx: i32,
    pub vy: i32,
    /// Changes to this missile only; the weapon's record is untouched.
    pub damage: Option<i32>,
    pub sprite: Option<i32>,
    /// Fuse in ms.
    pub fuse: Option<i32>,
    /// The sound it plays in flight (0 for none).
    pub sound: Option<i32>,
    /// What it bursts into: 0 nothing, 1 clustlets, 2 fire.
    pub explode_into: Option<i32>,
    /// PX's `countdownFrom`, ms.
    pub countdown: Option<i32>,
    /// Whether the fire key sets it off in flight.
    pub explode_on_space: Option<bool>,
}

pub struct SpawnArgs {
    pub owner: u32,
    pub x: i32,
    pub y: i32,
    pub vx: i32,
    pub vy: i32,
    pub sprite: i32,
    pub delta: u32,
    pub spawn: ActorSpawn,
}

/// Which presentation fields a `set_gfx` call is setting. Anything not named
/// keeps its current value, so the single-field setters do not clobber each
/// other.
pub mod gfx_field {
    pub const ANGLE: u32 = 1 << 0;
    pub const SCALE: u32 = 1 << 1;
    /// The tint word's colour bits only. Its alpha byte is `BLEND`.
    pub const TINT: u32 = 1 << 2;
    /// The tint word's alpha byte only.
    pub const BLEND: u32 = 1 << 3;
    pub const DEPTH: u32 = 1 << 4;
}

/// Alpha byte meaning ordinary alpha blending.
const OPAQUE: u32 = 0xFF00_0000;

/// Blend mode as the tint word's alpha byte. Premultiplied alpha 0 IS additive
/// under the renderer's blend state, so "add" needs no second pipeline and can
/// share a batch with opaque sprites.
fn blend_alpha(mode: &str) -> mlua::Result<u32> {
    match mode {
        "add" | "additive" => Ok(0),
        "normal" | "alpha" => Ok(OPAQUE),
        other => Err(mlua::Error::external(format!(
            "blend: expected \"normal\" or \"add\", got {other:?}"
        ))),
    }
}

/// Decode an `a:gfx{...}` table into the native call's arguments. Split out
/// from the method so the mask can be tested without a live actor.
fn gfx_args(spec: &Table) -> mlua::Result<(u32, f32, f32, u32, u32)> {
    let angle = spec.get::<Option<f32>>("angle")?;
    let scale = spec.get::<Option<f32>>("scale")?;
    let rgb = spec.get::<Option<u32>>("tint")?;
    let blend = spec.get::<Option<String>>("blend")?;
    let depth = spec.get::<Option<u32>>("depth")?;
    let mut mask = 0;
    for (present, bit) in [
        (angle.is_some(), gfx_field::ANGLE),
        (scale.is_some(), gfx_field::SCALE),
        (rgb.is_some(), gfx_field::TINT),
        (blend.is_some(), gfx_field::BLEND),
        (depth.is_some(), gfx_field::DEPTH),
    ] {
        if present {
            mask |= bit;
        }
    }
    let alpha = match blend.as_deref() {
        Some(mode) => blend_alpha(mode)?,
        None => OPAQUE,
    };
    Ok((
        mask,
        angle.unwrap_or(0.0),
        scale.unwrap_or(1.0),
        alpha | (rgb.unwrap_or(0x00FF_FFFF) & 0x00FF_FFFF),
        depth.unwrap_or(0),
    ))
}

/// Shared by `a:gfx` and the single-field setters.
fn apply_gfx(
    handle: u32,
    mask: u32,
    angle: f32,
    scale: f32,
    tint: u32,
    depth: u32,
) -> mlua::Result<()> {
    if handle == 0 || actor_dead(handle) {
        return Ok(());
    }
    if let Some(set) = native_set_gfx() {
        set(handle, mask, angle, scale, tint, depth);
    }
    Ok(())
}

/// Atomic transport; omitted speeds keep the body's current momentum.
#[derive(Clone, Copy, Debug)]
pub struct WarpArgs {
    pub x: i32, pub y: i32,
    pub vx: Option<i32>, pub vy: Option<i32>, pub facing: Option<i32>,
}

pub struct ActorNative {
    pub warp: fn(u32, &WarpArgs) -> bool,
    pub body_uid: fn(u32) -> u32,
    pub body_live: fn(u32, u32) -> bool,
    pub air_fire: fn(u32, bool) -> bool,
    pub fire_power: fn(u32) -> i32,
    pub explode: fn(handle: u32, damage: i32, explosion_id: i32, below: bool),
    /// Hold the actor on one frame of its current sheet.
    pub set_frame: fn(handle: u32, frame: i32),
    /// GPU renderer presentation: angle in degrees, scale, premultiplied tint.
    /// `mask` says which of the fields the caller actually supplied; the rest
    /// keep whatever the actor already has. Without it every setter would be
    /// a full overwrite, so `a:angle(30)` would silently reset scale and tint.
    pub set_gfx: fn(handle: u32, mask: u32, angle: f32, scale: f32, tint: u32, depth: u32),
    pub despawn: fn(handle: u32),
    pub in_water: fn(handle: u32) -> bool,
    pub on_feet: fn(handle: u32) -> bool,
    pub gravity: fn(handle: u32),
    /// bits: 1 terrain, 2 worm, 4 land edge, 8 left ground
    pub move_body: fn(handle: u32, terrain: bool, worms: bool) -> i32,
    pub advance: fn(handle: u32),
    pub spawn: fn(&SpawnArgs) -> Result<u32, String>,
    pub list_actors: fn() -> Vec<u32>,
    pub list_worms: fn(from: u32) -> Vec<u32>,
    pub resolve_sprite: fn(key: &str) -> Option<(i32, u32, i32, i32)>,
    pub land_solid: fn(x: i32, y: i32) -> bool,
    /// WA landscape cut, with 16.16 coordinates and pixel-sized radius.
    pub land_dig: fn(x: i32, y: i32, radius: i32) -> Result<(), String>,
    /// WA landscape mask cut; shape is a validated stock mask index.
    pub land_stamp: fn(sprite: i32, frame: u32, x: i32, y: i32, buried: Option<StampBuried>) -> Result<(), String>,
    pub land_cut: fn(shape: i32, x: i32, y: i32) -> Result<(), String>,
    pub land_trace: fn(x0: i32, y0: i32, x1: i32, y1: i32) -> Option<(i32, i32)>,
    pub map_size: fn() -> (i32, i32),
    pub view_cam: fn() -> (i32, i32, i32, i32),
    pub water_y: fn() -> i32,
    pub list_kind: fn(class: i32, from: u32) -> Vec<u32>,
    pub play_sound: fn(handle: u32, id: i32, flags: i32),
    pub resolve_sound: fn(key: &str) -> i32,
    /// Screen pointer in world 16.16 plus left-button down.
    pub pointer: fn() -> (i32, i32, bool),
    /// Pin a worm at (x,y) after stock HandleMessage this tick.
    pub carry: fn(handle: u32, x: i32, y: i32),
    pub drop_carry: fn(handle: u32),
    /// `worm:take{ hide, turn }`: (handle, hide, freeze the turn).
    pub take: fn(handle: u32, hide: bool, freeze: bool),
    /// `worm:release(vx, vy, { tumble, suppress_auto_parachute })`.
    pub release: fn(handle: u32, vx: i32, vy: i32, tumble: bool, suppress_auto_parachute: bool),
    /// `worm.taken`: the engine still holds it for the pack.
    pub is_taken: fn(handle: u32) -> bool,
    /// `worm:fits(x, y)`: WA's own collision test of the worm there.
    pub fits: fn(handle: u32, x: i32, y: i32) -> bool,
    /// `a:fits(x, y)`: the actor's own shape against the land.
    pub actor_fits: fn(handle: u32, x: i32, y: i32) -> bool,
    /// `worm:jetpack_steer(on)`.
    pub jetpack_steer: fn(handle: u32, on: bool),
    /// `worm.windup = frames | nil`: frames left before the worm releases
    /// its weapon, counted by the engine instead of WA's wind-up.
    pub set_windup: fn(handle: u32, frames: Option<i32>),
    pub windup: fn(handle: u32) -> Option<i32>,
    /// Switch this actor's installed sheet and reset the animation phase.
    pub look: fn(handle: u32, sprite: i32, delta: u32, w: i32, h: i32),
    /// Keyboard bits: 1 left, 2 right, 4 up, 8 down, 16 W, 32 sprint, 64 Q, 128 Space, 4096 Ctrl,
    /// 8192 the + key.
    pub keys: fn() -> u32,
    /// True when peers exist. Packs reading local input need this to guard
    /// themselves: `keys()` and `pointer()` are local-only, so acting on them
    /// in a networked game desyncs the peers.
    pub networked: fn() -> bool,
    /// One draw from WA's shared RNG, or None if it is unavailable.
    pub random: fn() -> Option<u32>,
    /// Write a line to wkLua.log now. `wa.log` collects into a Vec that is
    /// snapshotted once after loading, so without this a pack logging from a
    /// hook or a brain writes into a buffer nobody ever reads.
    pub log: fn(&str),
    pub hide_worm: fn(handle: u32, hide: bool),
    pub worm_hidden: fn(handle: u32) -> bool,
    /// Hide the body sprite only, leaving WA's labels (fuel, name, health).
    pub hide_body: fn(handle: u32, hide: bool),
    pub body_hidden: fn(handle: u32) -> bool,
    /// Stock SelectWeapon (F1 path). Returns ammo after equip, 0 if not this worm's turn.
    pub equip: fn(handle: u32, slot: u32) -> i32,
    pub hp: fn(handle: u32) -> i32,
    pub heal: fn(handle: u32, amount: i32) -> i32,
    pub arm_mine: fn(handle: u32, fuse_ms: i32) -> bool,
    pub mine_fuse: fn(handle: u32) -> i32,
    /// Add 16.16 velocity to a live stock mine or missile and wake it.
    pub impulse_object: fn(handle: u32, dx: i32, dy: i32) -> bool,
    /// The team (1-6) that fired a stock missile or laid a stock mine; 0 if none.
    pub object_team: fn(handle: u32) -> i32,
    /// Free a live stock missile or mine without it bursting.
    pub remove_object: fn(handle: u32) -> bool,
    pub hit_object: fn(handle: u32, from: u32, damage: i32, vx: i32, vy: i32, projectile: bool) -> bool,
    /// Rebuild the dead worm associated by the pack with a live grave.
    pub revive_grave: fn(grave: u32, worm_id: u32, health: i32) -> u32,
    pub grave_identity: fn(grave: u32) -> u32,
    pub electrical_nearby: fn(x: i32, y: i32, radius_px: i32, graves: bool) -> Vec<u32>,
    pub electrical_trace: fn(x0: i32, y0: i32, x1: i32, y1: i32) -> Option<(i32, i32, u32)>,
    /// Team ammo. `None` reads. `Add` leaves infinite (-1) alone; `Set`
    /// overwrites it. Returns the ammo afterwards.
    pub ammo: fn(handle: u32, slot: u32, change: Option<AmmoChange>) -> i32,
    /// WA's own cavern flag for the current match.
    pub cavern: fn() -> bool,
    /// WA's gravity and wind, (gravity, wind), 16.16: the collision
    /// manager's values, which WA's physics scale per object.
    pub forces: fn() -> (i32, i32),
    /// Set the match's gravity, 16.16 (GameInfo +0xD9A8), which WA scales by
    /// its low-gravity percentage into the collision manager every frame.
    pub set_gravity: fn(i32),
    /// Set WA's wind, 16.16, as WA's own wind change does: message 0x54.
    pub set_wind: fn(i32),
    /// Stock SpecialImpact (msg 0x4B). No explosion. Without a knock (vx, vy,
    /// 16.16) the pose is restored so the worm does not move; with one WA
    /// throws it.
    pub hurt: fn(target: u32, from: u32, amount: i32, vx: i32, vy: i32),
    /// Rewrite a stock weapon slot's body sprite (and optional cluster bit) on
    /// the live table. `cluster = 0` means leave the cluster sprite alone.
    pub retarget_weapon_sprites: fn(slot: u32, body: i32, cluster: i32) -> Result<(), String>,
    /// Fire a real WA missile from a weapon slot's fire params. Returns the
    /// missile's handle, the id `wa.on.missile_frame` gives it.
    pub launch: fn(&LaunchArgs) -> Result<u32, String>,
    /// `wa.fire.spawn{...}`: one of WA's fires. Returns its number.
    pub spawn_fire: fn(&FireArgs) -> Result<u32, String>,
    /// `wa.worms.spawn{...}`: a new worm for a team (team, x, y, name,
    /// health). Returns its handle.
    pub spawn_worm: fn(u32, i32, i32, &str, i32) -> Result<u32, String>,
    /// `wa.world.trace{...}`: a circle swept against terrain and objects.
    pub shape_trace: fn(&ShapeQuery) -> ShapeHit,
    /// The click handler (`wa._clicks` id) of the pack claim on a stock
    /// slot, 0 for none: what `wa.weapons.click` runs.
    pub slot_click: fn(u32) -> u32,
    /// `wa.world.collides{...}`: what a circle there overlaps: None nothing,
    /// Some(0) terrain, Some(handle) an object.
    pub shape_collides: fn(&ShapeQuery) -> Option<u32>,
    /// `wa.crates.spawn{...}`: one of WA's crates. Returns its handle.
    pub spawn_crate: fn(&CrateArgs) -> Result<u32, String>,
    /// WA's own WormEntity::SetState, so the state's setup runs.
    pub set_worm_state: fn(handle: u32, state: u32),
    /// Enter WA's frozen worm state when the worm is alive and eligible.
    pub freeze_worm: fn(handle: u32) -> bool,
    /// The retreat countdown in ms, 0 when retreat has not started.
    pub retreat_ms: fn() -> i32,
    /// The turn clock in ms left, as WA counts it (it stands still while WA
    /// waits for everything to settle after a shot).
    pub turn_clock_ms: fn() -> i32,
}

/// A write to team ammo.
///
/// `Add` is the original verb: relative, and it never touches an infinite
/// (-1) stock, which is what a pack gifting a shot wants. `Set` is absolute,
/// the equivalent of PX's `SetWeaponEx`. It is what a rules pack needs to own
/// a slot outright -- Highlander gives the holder exactly one and everyone else
/// none, whatever the scheme said, including a scheme's infinite bazooka.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AmmoChange {
    Add(i32),
    /// `-1` is infinite.
    Set(i32),
}

impl Default for ActorNative {
    fn default() -> Self {
        Self {
            warp: |_, _| false,
            body_uid: |_| 0,
            body_live: |_, _| false,
            air_fire: |_, _| false,
            fire_power: |_| 65536,
            explode: |_, _, _, _| {},
            despawn: |_| {},
            networked: || false,
            set_worm_state: |_, _| {},
            freeze_worm: |_| false,
            retreat_ms: || 0,
            turn_clock_ms: || 0,
            random: || None,
            log: |_| {},
            in_water: |_| false,
            on_feet: |_| false,
            gravity: |_| {},
            move_body: |_, _, _| 0,
            advance: |_| {},
            set_frame: |_, _| {},
            set_gfx: |_, _, _, _, _, _| {},
            spawn: |_| Err("spawn not bound".into()),
            list_actors: Vec::new,
            list_worms: |_| Vec::new(),
            resolve_sprite: |_| None,
            land_solid: |_, _| false,
            land_dig: |_, _, _| Err("land dig not bound".into()),
            land_stamp: |_, _, _, _, _| Err("land stamp not bound".into()),
            land_cut: |_, _, _| Err("land cut not bound".into()),
            land_trace: |_, _, _, _| None,
            map_size: || (0, 0),
            view_cam: || (0, 0, 0, 0),
            water_y: || 0,
            list_kind: |_, _| Vec::new(),
            play_sound: |_, _, _| {},
            resolve_sound: |_| 0,
            pointer: || (0, 0, false),
            carry: |_, _, _| {},
            drop_carry: |_| {},
            take: |_, _, _| {},
            release: |_, _, _, _, _| {},
            is_taken: |_| false,
            fits: |_, _, _| true,
            actor_fits: |_, _, _| true,
            jetpack_steer: |_, _| {},
            set_windup: |_, _| {},
            windup: |_| None,
            look: |_, _, _, _, _| {},
            keys: || 0,
            hide_worm: |_, _| {},
            worm_hidden: |_| false,
            hide_body: |_, _| {},
            body_hidden: |_| false,
            equip: |_, _| 0,
            hp: |_| 0,
            heal: |_, _| 0,
            arm_mine: |_, _| false,
            mine_fuse: |_| 0,
            impulse_object: |_, _, _| false,
            object_team: |_| 0,
            remove_object: |_| false,
            hit_object: |_, _, _, _, _, _| false,
            revive_grave: |_, _, _| 0,
            grave_identity: |_| 0,
            electrical_nearby: |_, _, _, _| Vec::new(),
            electrical_trace: |_, _, _, _| None,
            ammo: |_, _, _| 0,
            cavern: || false,
            forces: || (0, 0),
            set_gravity: |_| {},
            set_wind: |_| {},
            hurt: |_, _, _, _, _| {},
            retarget_weapon_sprites: |_, _, _| Err("retarget not bound".into()),
            launch: |_| Err("launch not bound".into()),
            spawn_fire: |_| Err("fire.spawn not bound".into()),
            spawn_worm: |_, _, _, _, _| Err("worms.spawn not bound".into()),
            shape_trace: |_| ShapeHit::default(),
            slot_click: |_| 0,
            shape_collides: |_| None,
            spawn_crate: |_| Err("crates.spawn not bound".into()),
        }
    }
}

static NEXT_BRAIN: AtomicU32 = AtomicU32::new(1);
static NATIVE: Mutex<Option<ActorNative>> = Mutex::new(None);

thread_local! {
    static SPAWN_OWNER: Cell<u32> = const { Cell::new(0) };
}

/// Firing worm for `wa.actors.spawn` during `on_click` / `on_fire`.
pub fn spawn_owner() -> u32 {
    SPAWN_OWNER.with(|c| c.get())
}

fn with_spawn_owner<T>(owner: u32, f: impl FnOnce() -> T) -> T {
    SPAWN_OWNER.with(|c| {
        let prev = c.replace(owner);
        struct Restore(u32);
        impl Drop for Restore {
            fn drop(&mut self) {
                SPAWN_OWNER.with(|c| c.set(self.0));
            }
        }
        let _restore = Restore(prev);
        f()
    })
}

pub fn reset() {
    NEXT_BRAIN.store(1, Ordering::SeqCst);
    if let Ok(mut g) = LIVE_ACTORS.lock() {
        g.clear();
    }
}

pub fn bind_actor_native(native: ActorNative) {
    if let Ok(mut slot) = NATIVE.lock() {
        *slot = Some(native);
    }
}

pub fn register_brain(
    lua: &Lua,
    frame: Option<Function>,
    land: Option<Function>,
    hit: Option<Function>,
) -> mlua::Result<u32> {
    if frame.is_none() && land.is_none() && hit.is_none() {
        return Ok(0);
    }
    let id = NEXT_BRAIN.fetch_add(1, Ordering::SeqCst).max(1);
    let wa: Table = lua.globals().get("wa")?;
    let brains: Table = wa.get("_brains")?;
    let spec = lua.create_table()?;
    let wa_mod: String = wa.get("_mod").unwrap_or_default();
    spec.set("mod", wa_mod)?;
    if let Some(f) = frame {
        spec.set("frame", f)?;
    }
    if let Some(f) = land {
        spec.set("land", f)?;
    }
    if let Some(f) = hit {
        spec.set("hit", f)?;
    }
    brains.set(id, spec)?;
    Ok(id)
}

pub fn register_fire(lua: &Lua, cb: Function) -> mlua::Result<u32> {
    register_cb(lua, "_fires", cb)
}

pub fn register_click(lua: &Lua, cb: Function) -> mlua::Result<u32> {
    register_cb(lua, "_clicks", cb)
}

pub fn register_select(lua: &Lua, cb: Function) -> mlua::Result<u32> {
    register_cb(lua, "_selects", cb)
}

pub fn register_explode(lua: &Lua, cb: Function) -> mlua::Result<u32> {
    register_cb(lua, "_explodes", cb)
}

pub fn register_proximity(lua: &Lua, cb: Function) -> mlua::Result<u32> {
    register_cb(lua, "_proximity", cb)
}

pub fn register_place(lua: &Lua, cb: Function) -> mlua::Result<u32> {
    register_cb(lua, "_places", cb)
}

fn register_cb(lua: &Lua, table: &str, cb: Function) -> mlua::Result<u32> {
    let id = NEXT_BRAIN.fetch_add(1, Ordering::SeqCst).max(1);
    let wa: Table = lua.globals().get("wa")?;
    let fires: Table = wa.get(table)?;
    let spec = lua.create_table()?;
    let wa_mod: String = wa.get("_mod").unwrap_or_default();
    spec.set("mod", wa_mod)?;
    spec.set("on", cb)?;
    fires.set(id, spec)?;
    Ok(id)
}

pub fn weapon_fire(
    fire_id: u32,
    worm: u32,
    slot: i32,
    x: i32,
    y: i32,
    vx: i32,
    vy: i32,
) -> Result<(), String> {
    if fire_id == 0 {
        return Ok(());
    }
    let power = NATIVE.lock().ok().and_then(|g| g.as_ref().map(|n| (n.fire_power)(worm))).unwrap_or(65536);
    with_lua("weapon_fire", |lua| {
        let extra = lua.create_table().map_err(|e| e.to_string())?;
        extra.set("power", power as f64 / 65536.0).map_err(|e| e.to_string())?;
        call_fire(lua, fire_id, worm, slot, (x, y, vx, vy), Some(&extra))
    })
}

/// Run on_fire `fire_id` with the event a shot gives it: (x, y, vx, vy), the
/// slot and the worm, plus `extra`'s fields (power or airborne dispatch).
/// The running pack (`wa._mod`) is the fire's while it
/// runs, and the caller's again after.
pub(crate) fn call_fire(
    lua: &Lua,
    fire_id: u32,
    worm: u32,
    slot: i32,
    (x, y, vx, vy): (i32, i32, i32, i32),
    extra: Option<&Table>,
) -> Result<(), String> {
    let wa: Table = lua.globals().get("wa").map_err(|e| e.to_string())?;
    let fires: Table = wa.get("_fires").map_err(|e| e.to_string())?;
    let spec = match fires.get::<Value>(fire_id).map_err(|e| e.to_string())? {
        Value::Table(t) => t,
        _ => return Err(format!("on_fire {fire_id} missing")),
    };
    let cb: Function = match spec.get::<Value>("on").map_err(|e| e.to_string())? {
        Value::Function(f) => f,
        _ => return Err(format!("on_fire {fire_id} has no function")),
    };
    let caller: Value = wa.get("_mod").map_err(|e| e.to_string())?;
    if let Ok(m) = spec.get::<String>("mod") {
        let _ = wa.set("_mod", m);
    }
    let fire = lua.create_table().map_err(|e| e.to_string())?;
    if let Some(extra) = extra {
        for pair in extra.pairs::<Value, Value>() {
            let (k, v) = pair.map_err(|e| e.to_string())?;
            fire.set(k, v).map_err(|e| e.to_string())?;
        }
    }
    fire.set("x", x).map_err(|e| e.to_string())?;
    fire.set("y", y).map_err(|e| e.to_string())?;
    fire.set("vx", vx).map_err(|e| e.to_string())?;
    fire.set("vy", vy).map_err(|e| e.to_string())?;
    fire.set("slot", slot).map_err(|e| e.to_string())?;
    fire.set("worm", WormHandle(worm))
        .map_err(|e| e.to_string())?;
    let result = with_spawn_owner(worm, || cb.call::<()>(fire).map_err(|e| e.to_string()));
    let _ = wa.set("_mod", caller);
    result
}

/// An `on_click`: the click at (x, y), and `speed`, the shot's speed in WA's
/// fire context (16.16; for a strike, the sign of its x is the direction WA's
/// arrow keys set: Project X's PxAirstrike reads it so), (0, 0) without one.
pub fn weapon_click(
    click_id: u32,
    worm: u32,
    slot: i32,
    x: i32,
    y: i32,
    sky: i32,
    speed: (i32, i32),
) -> Result<(), String> {
    if click_id == 0 {
        return Ok(());
    }
    with_lua("weapon_click", |lua| call_click(lua, click_id, worm, slot, x, y, sky, speed))
}

/// Run on_click `click_id` with the event a click gives it. The running
/// pack (`wa._mod`) is the click's while it runs, and the caller's again
/// after: `wa.weapons.click` runs one from inside another pack's callback.
#[allow(clippy::too_many_arguments)]
pub(crate) fn call_click(
    lua: &Lua,
    click_id: u32,
    worm: u32,
    slot: i32,
    x: i32,
    y: i32,
    sky: i32,
    speed: (i32, i32),
) -> Result<(), String> {
    let wa: Table = lua.globals().get("wa").map_err(|e| e.to_string())?;
    let clicks: Table = wa.get("_clicks").map_err(|e| e.to_string())?;
    let spec = match clicks.get::<Value>(click_id).map_err(|e| e.to_string())? {
        Value::Table(t) => t,
        _ => return Err(format!("on_click {click_id} missing")),
    };
    let cb: Function = match spec.get::<Value>("on").map_err(|e| e.to_string())? {
        Value::Function(f) => f,
        _ => return Err(format!("on_click {click_id} has no function")),
    };
    let caller: Value = wa.get("_mod").map_err(|e| e.to_string())?;
    if let Ok(m) = spec.get::<String>("mod") {
        let _ = wa.set("_mod", m);
    }
    let click = lua.create_table().map_err(|e| e.to_string())?;
    click.set("x", x).map_err(|e| e.to_string())?;
    click.set("y", y).map_err(|e| e.to_string())?;
    click.set("sky", sky).map_err(|e| e.to_string())?;
    click.set("slot", slot).map_err(|e| e.to_string())?;
    click.set("vx", speed.0).map_err(|e| e.to_string())?;
    click.set("vy", speed.1).map_err(|e| e.to_string())?;
    click
        .set("worm", WormHandle(worm))
        .map_err(|e| e.to_string())?;
    let result = with_spawn_owner(worm, || cb.call::<()>(click).map_err(|e| e.to_string()));
    let _ = wa.set("_mod", caller);
    result
}

pub fn weapon_select(select_id: u32, worm: u32, slot: i32) -> Result<(), String> {
    if select_id == 0 {
        return Ok(());
    }
    let (x, y, _) = native_pointer().map(|f| f()).unwrap_or((0, 0, false));
    with_lua("weapon_select", |lua| {
        let wa: Table = lua.globals().get("wa").map_err(|e| e.to_string())?;
        let selects: Table = wa.get("_selects").map_err(|e| e.to_string())?;
        let spec = match selects.get::<Value>(select_id).map_err(|e| e.to_string())? {
            Value::Table(t) => t,
            _ => return Err(format!("on_select {select_id} missing")),
        };
        let cb: Function = match spec.get::<Value>("on").map_err(|e| e.to_string())? {
            Value::Function(f) => f,
            _ => return Err(format!("on_select {select_id} has no function")),
        };
        let caller: Value = wa.get("_mod").map_err(|e| e.to_string())?;
        if let Ok(m) = spec.get::<String>("mod") {
            let _ = wa.set("_mod", m);
        }
        let sel = lua.create_table().map_err(|e| e.to_string())?;
        sel.set("x", x).map_err(|e| e.to_string())?;
        sel.set("y", y).map_err(|e| e.to_string())?;
        sel.set("slot", slot).map_err(|e| e.to_string())?;
        sel.set("worm", WormHandle(worm))
            .map_err(|e| e.to_string())?;
        let result = with_spawn_owner(worm, || cb.call::<()>(sel).map_err(|e| e.to_string()));
        let _ = wa.set("_mod", caller);
        result
    })
}

/// `missile`: the missile the step is for, its `wa.on.missile_frame` id (0
/// for a mine).
#[allow(clippy::too_many_arguments)]
pub fn weapon_emit(
    table: &'static str,
    emit_id: u32,
    label: &str,
    worm: u32,
    slot: i32,
    x: i32,
    y: i32,
    missile: u32,
) -> Result<(), String> {
    if emit_id == 0 {
        return Ok(());
    }
    with_lua("weapon_emit", |lua| {
        let wa: Table = lua.globals().get("wa").map_err(|e| e.to_string())?;
        let fns: Table = wa.get(table).map_err(|e| e.to_string())?;
        let spec = match fns.get::<Value>(emit_id).map_err(|e| e.to_string())? {
            Value::Table(t) => t,
            _ => return Err(format!("{label} {emit_id} missing")),
        };
        let cb: Function = match spec.get::<Value>("on").map_err(|e| e.to_string())? {
            Value::Function(f) => f,
            _ => return Err(format!("{label} {emit_id} has no function")),
        };
        // The step runs as its pack and leaves the caller's running: a
        // missile can burst inside another pack's wa.weapons.launch.
        let caller: Value = wa.get("_mod").map_err(|e| e.to_string())?;
        if let Ok(m) = spec.get::<String>("mod") {
            let _ = wa.set("_mod", m);
        }
        let ev = lua.create_table().map_err(|e| e.to_string())?;
        ev.set("x", x).map_err(|e| e.to_string())?;
        ev.set("y", y).map_err(|e| e.to_string())?;
        ev.set("slot", slot).map_err(|e| e.to_string())?;
        if missile != 0 {
            ev.set("missile", missile).map_err(|e| e.to_string())?;
        }
        ev.set("worm", WormHandle(worm))
            .map_err(|e| e.to_string())?;
        let result = with_spawn_owner(worm, || cb.call::<()>(ev).map_err(|e| e.to_string()));
        let _ = wa.set("_mod", caller);
        result
    })
}

/// The actors alive now, by address: the engine reports each spawn and free.
/// A pack may keep an actor's handle after the engine freed it (drowned, or
/// despawned); WA then reuses the memory, so nothing may be read or written
/// through such a handle. Every ActorHandle access goes through `live`.
static LIVE_ACTORS: Mutex<std::collections::BTreeSet<u32>> = Mutex::new(std::collections::BTreeSet::new());

pub fn actor_spawned(handle: u32, _brain_id: u32) {
    if handle != 0 {
        if let Ok(mut g) = LIVE_ACTORS.lock() {
            g.insert(handle);
        }
    }
}

fn actor_live(handle: u32) -> bool {
    handle != 0 && LIVE_ACTORS.lock().map(|g| g.contains(&handle)).unwrap_or(false)
}

/// The handle while its actor is alive, else 0, which every read and write
/// through it treats as no actor.
fn live(handle: u32) -> u32 {
    if actor_live(handle) { handle } else { 0 }
}

pub fn actor_freed(handle: u32) {
    if let Ok(mut g) = LIVE_ACTORS.lock() {
        g.remove(&handle);
    }
}

/// What hit a LuaActor whose brain has a `hit` handler: WA's blast (0x1C)
/// or hit (0x4B, Project X's M_GUNEXP), which the engine then leaves to the
/// handler instead of applying to the actor's speed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActorHit {
    /// A blast, else a hit.
    pub blast: bool,
    /// A hit's kind, WA's first field: 1 is a fire's.
    pub kind: i32,
    /// Where it came from, 16.16.
    pub x: i32,
    pub y: i32,
    /// The push WA would have given the actor, 16.16 speed.
    pub vx: i32,
    pub vy: i32,
    pub damage: i32,
    pub team: i32,
}

/// Whether brain `brain_id` takes its hits itself (`on = { hit = fn }`).
pub fn actor_takes_hits(brain_id: u32) -> bool {
    if brain_id == 0 {
        return false;
    }
    PARKED_LUA.with(|slot| {
        let guard = slot.borrow();
        let Some(lua) = guard.as_ref() else { return false };
        (|| -> mlua::Result<bool> {
            let wa: Table = lua.globals().get("wa")?;
            let brains: Table = wa.get("_brains")?;
            Ok(match brains.get::<Value>(brain_id)? {
                Value::Table(spec) => matches!(spec.get::<Value>("hit")?, Value::Function(_)),
                _ => false,
            })
        })()
        .unwrap_or(false)
    })
}

/// Run brain `brain_id`'s `hit` handler: `hit(actor, { kind = "blast" |
/// "hit", hit_kind, x, y, vx, vy, damage, team })`.
pub fn actor_hit(handle: u32, brain_id: u32, hit: ActorHit) -> Result<(), String> {
    if handle == 0 || brain_id == 0 {
        return Ok(());
    }
    with_lua("actor hit", |lua| {
        let wa: Table = lua.globals().get("wa").map_err(|e| e.to_string())?;
        let brains: Table = wa.get("_brains").map_err(|e| e.to_string())?;
        let spec = match brains.get::<Value>(brain_id).map_err(|e| e.to_string())? {
            Value::Table(t) => t,
            _ => return Ok(()),
        };
        let f = match spec.get::<Value>("hit").map_err(|e| e.to_string())? {
            Value::Function(f) => f,
            _ => return Ok(()),
        };
        let caller: Value = wa.get("_mod").map_err(|e| e.to_string())?;
        if let Ok(m) = spec.get::<String>("mod") {
            let _ = wa.set("_mod", m);
        }
        let t = lua.create_table().map_err(|e| e.to_string())?;
        (|| -> mlua::Result<()> {
            t.set("kind", if hit.blast { "blast" } else { "hit" })?;
            t.set("hit_kind", hit.kind)?;
            t.set("x", hit.x)?;
            t.set("y", hit.y)?;
            t.set("vx", hit.vx)?;
            t.set("vy", hit.vy)?;
            t.set("damage", hit.damage)?;
            t.set("team", hit.team)?;
            Ok(())
        })()
        .map_err(|e| e.to_string())?;
        let actor = lua.create_userdata(ActorHandle(handle)).map_err(|e| e.to_string())?;
        let owner = read_handle(handle, ACTOR_OWNER) as u32;
        let result = with_spawn_owner(owner, || f.call::<()>((actor, t)).map_err(|e| e.to_string()));
        let _ = wa.set("_mod", caller);
        result
    })
}

pub fn actor_frame(handle: u32, brain_id: u32, ev: ActorEvent) -> Result<(), String> {
    if handle == 0 || brain_id == 0 {
        return Ok(());
    }
    with_lua("actor", |lua| {
        let wa: Table = lua.globals().get("wa").map_err(|e| e.to_string())?;
        let brains: Table = wa.get("_brains").map_err(|e| e.to_string())?;
        let spec = match brains.get::<Value>(brain_id).map_err(|e| e.to_string())? {
            Value::Table(t) => t,
            _ => return Err(format!("LuaActor brain {brain_id} missing")),
        };
        if let Ok(m) = spec.get::<String>("mod") {
            let _ = wa.set("_mod", m);
        }
        let frame: Option<Function> = match spec.get::<Value>("frame").map_err(|e| e.to_string())? {
            Value::Function(f) => Some(f),
            _ => None,
        };
        let land: Option<Function> = match spec.get::<Value>("land").map_err(|e| e.to_string())? {
            Value::Function(f) => Some(f),
            _ => None,
        };
        let actor = lua
            .create_userdata(ActorHandle(handle))
            .map_err(|e| e.to_string())?;
        let t = event_table(lua, &ev)?;
        if ev.land {
            if let Some(f) = land {
                f.call::<()>((actor.clone(), t.clone()))
                    .map_err(|e| e.to_string())?;
            }
        }
        if let Some(f) = frame {
            let owner = read_handle(handle, ACTOR_OWNER) as u32;
            with_spawn_owner(owner, || f.call::<()>((actor, t)).map_err(|e| e.to_string()))?;
        }
        Ok(())
    })
}

fn event_table(lua: &Lua, ev: &ActorEvent) -> Result<Table, String> {
    let t = lua.create_table().map_err(|e| e.to_string())?;
    t.set("land", ev.land).map_err(|e| e.to_string())?;
    t.set("left_ground", ev.left_ground)
        .map_err(|e| e.to_string())?;
    t.set("hit_worm", ev.hit_worm).map_err(|e| e.to_string())?;
    t.set("in_water", ev.in_water).map_err(|e| e.to_string())?;
    t.set("damage", ev.damage).map_err(|e| e.to_string())?;
    t.set("msg", 2u32).map_err(|e| e.to_string())?;
    Ok(t)
}

fn with_lua<T>(label: &'static str, f: impl FnOnce(&Lua) -> Result<T, String>) -> Result<T, String> {
    PARKED_LUA.with(|slot| {
        let guard = slot.borrow();
        let Some(lua) = guard.as_ref() else {
            return Err("no lua state".into());
        };
        crate::perf::time(label, lua, || crate::sim_context::with_sim(|| f(lua)))
    })
}

#[derive(Clone, Copy)]
pub struct ActorHandle(pub u32);

#[derive(Clone, Copy)]
pub struct WormHandle(pub u32);

impl UserData for ActorHandle {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // The spawn-order id, not the entity pointer. An address is not an
        // identity: it differs between peers and between runs, so comparing
        // ids or iterating a table keyed by them gave a different answer on
        // every machine.
        fields.add_field_method_get("id", |_, this| Ok(read_handle(live(this.0), ACTOR_SEQ)));
        fields.add_field_method_get("kind", |_, _| Ok("actor"));
        bind_pos_fields(fields, |h| live(h.0));
        fields.add_field_method_get("owner", |lua, this| {
            let owner = read_handle(live(this.0), ACTOR_OWNER);
            lua.create_userdata(WormHandle(owner as u32))
        });
        fields.add_field_method_get("facing", |_, this| {
            let flags = read_handle(live(this.0), ACTOR_FLAGS);
            Ok(if flags & ACTOR_FLAG_MIRROR_X != 0 {
                -1i32
            } else {
                1i32
            })
        });
        fields.add_field_method_set("facing", |_, this, v: i32| {
            crate::sim_context::require("worm.facing (write)")?;
            let mut flags = read_handle(live(this.0), ACTOR_FLAGS);
            if v < 0 {
                flags |= ACTOR_FLAG_MIRROR_X;
            } else {
                flags &= !ACTOR_FLAG_MIRROR_X;
            }
            write_handle(live(this.0), ACTOR_FLAGS, flags);
            Ok(())
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("warp", |_, this, spec: Table| {
            crate::sim_context::require("a:warp")?;
            let args = warp_args(&spec)?;
            let h = this.0;
            if !actor_live(h) { return Ok(false); }
            let warp = NATIVE.lock().ok().and_then(|g| g.as_ref().map(|n| n.warp));
            Ok(warp.map(|f| f(h, &args)).unwrap_or(false))
        });
        methods.add_method("dig", |_, this, spec: Table| {
            if live(this.0) == 0 || actor_dead(live(this.0)) {
                return Ok(());
            }
            let radius: i32 = spec.get("radius")?;
            crate::spawn::dig_at(
                read_handle(live(this.0), ENT_POS_X),
                read_handle(live(this.0), ENT_POS_Y),
                radius,
            )
        });
        // `a:fits(x, y)`: whether the actor's collision shape at (x, y),
        // 16.16, touches no land, by WA's own test. A read.
        methods.add_method("fits", |_, this, (x, y, mode): (i32, i32, Option<String>)| {
            fits_terrain_mode(mode.as_deref())?;
            let h = live(this.0);
            if h == 0 {
                return Ok(true);
            }
            Ok(native_actor_fits().map(|f| f(h, x, y)).unwrap_or(true))
        });
        methods.add_method("explode", |_, this, opts: Option<Table>| {
            crate::sim_context::require("actor:explode")?;
            if live(this.0) == 0 || actor_dead(live(this.0)) {
                return Ok(());
            }
            let mut damage = read_handle(live(this.0), ACTOR_DAMAGE);
            let mut id = read_handle(live(this.0), ACTOR_BOOM_ID);
            let mut below = true;
            if let Some(t) = opts {
                if let Ok(Some(v)) = opt_i32(&t, "damage") {
                    damage = v;
                }
                if let Ok(Some(v)) = opt_i32(&t, "id") {
                    id = v;
                }
                if let Ok(Some(v)) = opt_bool(&t, "below") {
                    below = v;
                }
                if let Ok(Value::String(s)) = t.get::<Value>("at") {
                    below = s.to_str()?.as_ref() == "below";
                }
            }
            let explode = native_explode().ok_or_else(|| {
                mlua::Error::external("explode: CreateExplosion not bound")
            })?;
            explode(live(this.0), damage, id, below);
            Ok(())
        });
        methods.add_method("despawn", |_, this, ()| {
            crate::sim_context::require("actor:despawn")?;
            if live(this.0) == 0 || actor_dead(live(this.0)) {
                return Ok(());
            }
            let despawn = native_despawn_fn().ok_or_else(|| {
                mlua::Error::external("despawn: native free not bound")
            })?;
            despawn(live(this.0));
            Ok(())
        });
        methods.add_method("in_water", |_, this, ()| {
            if live(this.0) == 0 {
                return Ok(false);
            }
            Ok(native_in_water_fn().map(|f| f(live(this.0))).unwrap_or(false))
        });
        methods.add_method("on_feet", |_, this, ()| {
            if live(this.0) == 0 {
                return Ok(false);
            }
            Ok(native_on_feet_fn().map(|f| f(live(this.0))).unwrap_or(false))
        });
        methods.add_method("gravity", |_, this, ()| {
            if live(this.0) == 0 || actor_dead(live(this.0)) {
                return Ok(());
            }
            let gravity = native_gravity().ok_or_else(|| {
                mlua::Error::external("gravity: stock leaf not bound")
            })?;
            gravity(live(this.0));
            Ok(())
        });
        methods.add_method("move", |lua, this, opts: Option<Table>| {
            if live(this.0) == 0 || actor_dead(live(this.0)) {
                return empty_hit(lua);
            }
            let mut terrain = true;
            let mut worms = true;
            if let Some(t) = opts {
                if let Ok(Some(v)) = opt_bool(&t, "terrain") {
                    terrain = v;
                }
                if let Ok(Some(v)) = opt_bool(&t, "worms") {
                    worms = v;
                }
                if let Ok(Some(v)) = opt_bool(&t, "collide_terrain") {
                    terrain = v;
                }
                if let Ok(Some(v)) = opt_bool(&t, "collide_worms") {
                    worms = v;
                }
            }
            let move_body = native_move().ok_or_else(|| {
                mlua::Error::external("move: native step not bound")
            })?;
            hit_table(lua, move_body(live(this.0), terrain, worms))
        });
        methods.add_method("advance", |_, this, ()| {
            if live(this.0) == 0 || actor_dead(live(this.0)) {
                return Ok(());
            }
            let advance = native_advance().ok_or_else(|| {
                mlua::Error::external("advance: sprite loop not bound")
            })?;
            advance(live(this.0));
            Ok(())
        });
        // Set several presentation fields at once. Only the keys actually
        // present are written -- `a:gfx{angle = 30}` leaves scale, tint and
        // depth exactly as they were.
        methods.add_method("gfx", |_, this, spec: Table| {
            let (mask, angle, scale, tint, depth) = gfx_args(&spec)?;
            apply_gfx(live(this.0), mask, angle, scale, tint, depth)
        });
        // Single-field setters. They forward to the same native call with a
        // one-bit mask, so each is genuinely independent of the others.
        methods.add_method("angle", |_, this, degrees: f32| {
            apply_gfx(live(this.0), gfx_field::ANGLE, degrees, 1.0, 0, 0)
        });
        methods.add_method("scale", |_, this, factor: f32| {
            apply_gfx(live(this.0), gfx_field::SCALE, 0.0, factor, 0, 0)
        });
        methods.add_method("tint", |_, this, rgb: u32| {
            apply_gfx(live(this.0), gfx_field::TINT, 0.0, 1.0, rgb & 0x00FF_FFFF, 0)
        });
        methods.add_method("blend", |_, this, mode: String| {
            apply_gfx(live(this.0), gfx_field::BLEND, 0.0, 1.0, blend_alpha(&mode)?, 0)
        });
        methods.add_method("frame", |_, this, frame: i32| {
            if live(this.0) == 0 || actor_dead(live(this.0)) {
                return Ok(());
            }
            if let Some(set) = native_set_frame() {
                set(live(this.0), frame);
            }
            Ok(())
        });
        methods.add_method("look", |lua, this, name: String| {
            if live(this.0) == 0 || actor_dead(live(this.0)) {
                return Ok(());
            }
            let (id, delta, w, h) = crate::spawn::resolve_sprite_name(lua, &name)?;
            if let Some(look) = native_look() {
                look(live(this.0), id, delta, w, h);
            }
            Ok(())
        });
        methods.add_method("spawn", |lua, this, spec: Table| {
            crate::spawn::runtime_spawn(lua, spec, Some(live(this.0)))
        });
        methods.add_method("sound", |_, this, (slot, flags): (Value, Option<i32>)| {
            if live(this.0) == 0 || actor_dead(live(this.0)) {
                return Ok(());
            }
            let id = sound_slot(slot)?;
            if id == 0 {
                return Ok(());
            }
            let play = native_play_sound().ok_or_else(|| {
                mlua::Error::external("sound: PlaySoundLocal not bound")
            })?;
            play(live(this.0), id, flags.unwrap_or(8));
            Ok(())
        });
    }
}

impl UserData for WormHandle {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // `team * 16 + index` -- not invented here: it is the key WA itself
        // uses to find a worm in its shared-data table, so it is stable across
        // peers and runs where the pointer is not.
        fields.add_field_method_get("id", |_, this| {
            Ok(stable_worm_id(this.0))
        });
        fields.add_field_method_get("facing", |_, this| {
            Ok(if this.0 == 0 {
                1
            } else {
                let direction = unsafe { read_i32(this.0 as *mut u8, WORM_TURN_SIDE) };
                if direction < 0 {
                    -1
                } else {
                    1
                }
            })
        });
        // Facing: WA's turn side (+0x1A8), and its copy (+0x3DC) so the
        // worm's sprite follows at once rather than at the next aim update.
        fields.add_field_method_set("facing", |_, this, v: i32| {
            crate::sim_context::require("worm.facing (write)")?;
            if this.0 != 0 {
                let side = if v < 0 { -1 } else { 1 };
                unsafe {
                    write_i32(this.0 as *mut u8, WORM_TURN_SIDE, side);
                    write_i32(this.0 as *mut u8, WORM_FACING, side);
                }
            }
            Ok(())
        });
        // A repeat-fire weapon's shots, fired and to fire (Project X's
        // nShooted and nShoots). While fired is below to-fire WA fires again
        // after the weapon's delay, so a pack that holds them (0 and 1 each
        // frame) keeps the worm firing. Writes are lockstep: simulation
        // callbacks only.
        for (name, off) in [("shots_fired", WORM_SHOTS_FIRED), ("shots_to_fire", WORM_SHOTS_TO_FIRE)] {
            fields.add_field_method_get(name, move |_, this| {
                Ok(if this.0 == 0 { 0 } else { unsafe { read_i32(this.0 as *mut u8, off) } })
            });
            fields.add_field_method_set(name, move |_, this, v: i32| {
                if !crate::sim_context::active() {
                    return Err(mlua::Error::external(format!(
                        "worm.{name}: only from a simulation callback"
                    )));
                }
                if this.0 != 0 {
                    unsafe { write_i32(this.0 as *mut u8, off, v) };
                }
                Ok(())
            });
        }
        // Jet pack fuel, 16.16 (Project X's JetFuel); 0 is unlimited.
        fields.add_field_method_get("fuel", |_, this| {
            Ok(if this.0 == 0 { 0 } else { unsafe { read_i32(this.0 as *mut u8, WORM_JET_FUEL) } })
        });
        fields.add_field_method_set("fuel", |_, this, v: i32| {
            crate::sim_context::require("worm.fuel (write)")?;
            if this.0 != 0 {
                unsafe { write_i32(this.0 as *mut u8, WORM_JET_FUEL, v) };
            }
            Ok(())
        });
        // How strongly gravity and wind act on the worm, 16.16 (1.0 = 65536;
        // Project X's GravityFactor / WindFactor).
        for (name, off) in [("gravity", ENT_GRAVITY_SCALE), ("wind", ENT_WIND_SCALE)] {
            fields.add_field_method_get(name, move |_, this| {
                Ok(if this.0 == 0 { 0 } else { unsafe { read_i32(this.0 as *mut u8, off) } })
            });
            fields.add_field_method_set(name, move |_, this, v: i32| {
                crate::sim_context::require(&format!("worm.{name} (write)"))?;
                if this.0 != 0 {
                    unsafe { write_i32(this.0 as *mut u8, off, v) };
                }
                Ok(())
            });
        }
        // Which collision layers the worm collides with (WA's +0x34, Project
        // X's ColGroup): a bit per layer. Bit 22 (0x400000), the landscape's,
        // is also what WA's water acts on (0x4FDE20): without it a worm in the
        // water neither sinks nor drowns.
        fields.add_field_method_get("collision_mask", |_, this| {
            Ok(if this.0 == 0 { 0 } else { unsafe { read_i32(this.0 as *mut u8, ENT_COLLISION_MASK) } })
        });
        fields.add_field_method_set("collision_mask", |_, this, v: i64| {
            crate::sim_context::require("worm.collision_mask (write)")?;
            if this.0 != 0 {
                unsafe { write_i32(this.0 as *mut u8, ENT_COLLISION_MASK, v as u32 as i32) };
            }
            Ok(())
        });
        // The worm's own move requests (WA's, from lockstep input), unlike
        // wa.world.keys(), the keyboard of the player whose turn it is.
        fields.add_field_method_get("keys", |lua, this| {
            let t = lua.create_table()?;
            for (name, off) in WORM_KEYS {
                let down = this.0 != 0 && unsafe { read_i32(this.0 as *mut u8, off) } != 0;
                t.set(name, down)?;
            }
            Ok(t)
        });
        fields.add_field_method_get("state", |_, this| {
            if this.0 == 0 {
                return Ok(None);
            }
            Ok(worm_state_name(unsafe { read_i32(this.0 as *mut u8, WORM_STATE) } as u32))
        });
        // `worm.taken`: still the pack's since `worm:take`. The engine lets
        // go at the turn's end and when WA knocks the worm flying.
        fields.add_field_method_get("taken", |_, this| {
            Ok(this.0 != 0 && native_is_taken().map(|f| f(this.0)).unwrap_or(false))
        });
        // Diagnostics: keep the raw value even when `state` has no name yet.
        fields.add_field_method_get("state_id", |_, this| {
            if this.0 == 0 {
                return Ok(None);
            }
            Ok(Some(unsafe { read_i32(this.0 as *mut u8, WORM_STATE) } as u32))
        });
        // Diagnostics: WA's state by its debug label (game::worm_state), `?`
        // marking a provisional one. For logs, never for game logic.
        fields.add_field_method_get("state_label", |_, this| {
            if this.0 == 0 {
                return Ok(None);
            }
            let state = unsafe { read_i32(this.0 as *mut u8, WORM_STATE) } as u32;
            Ok(Some(
                game::worm_state::label(state)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{state:#x}?")),
            ))
        });
        fields.add_field_method_get("team", |_, this| {
            if this.0 == 0 {
                return Ok(0i32);
            }
            Ok(unsafe { read_i32(this.0 as *mut u8, ENT_WORM_TEAM) })
        });
        fields.add_field_method_get("slot", |_, this| {
            if this.0 == 0 {
                return Ok(0i32);
            }
            Ok(unsafe { read_i32(this.0 as *mut u8, WORM_SELECTED_WEAPON) })
        });
        fields.add_field_method_get("weapon", |_, this| {
            if this.0 == 0 {
                return Ok(None);
            }
            let slot = unsafe { read_i32(this.0 as *mut u8, WORM_SELECTED_WEAPON) };
            Ok(StockWeapon::from_index(slot as u32).map(|s| s.name().to_string()))
        });
        // The aim WA draws: its 180-degree sweep, 0 straight down, 0.5 level,
        // 1 straight up; `facing` says which side. Project X's FireAngle.
        fields.add_field_method_get("aim", |_, this| {
            Ok(if this.0 == 0 {
                0.0
            } else {
                aim_fraction(unsafe { read_i32(this.0 as *mut u8, WORM_AIM_ANGLE) })
            })
        });
        // WA is drawing the worm with its weapon out: standing with it, or
        // charging a throw (Project X's WS_AIMING / WS_SETPOWER).
        fields.add_field_method_get("aiming", |_, this| {
            Ok(this.0 != 0
                && holds_weapon(unsafe { read_i32(this.0 as *mut u8, WORM_STATE) as u32 }))
        });
        fields.add_field_method_get("drilling", |_, this| {
            Ok(this.0 != 0
                && unsafe { read_i32(this.0 as *mut u8, WORM_STATE) as u32 }
                    == WORM_STATE_DRILLING)
        });
        fields.add_field_method_get("kind", |_, _| Ok("worm"));
        fields.add_field_method_get("visible", |_, this| {
            Ok(!native_worm_hidden()
                .map(|f| f(this.0))
                .unwrap_or(false))
        });
        fields.add_field_method_set("visible", |_, this, v: bool| {
            crate::sim_context::require("worm.visible (write)")?;
            if this.0 == 0 {
                return Ok(());
            }
            if let Some(hide) = native_hide_worm() {
                hide(this.0, !v);
            }
            Ok(())
        });
        // Only the body sprite (Project X's RenderWorm): WA still draws the
        // worm's labels -- name, health, jet pack fuel -- and its cursor.
        fields.add_field_method_get("body_visible", |_, this| {
            Ok(!native_hide_body().map(|(_, hidden)| hidden(this.0)).unwrap_or(false))
        });
        fields.add_field_method_set("body_visible", |_, this, v: bool| {
            crate::sim_context::require("worm.body_visible (write)")?;
            if this.0 != 0 {
                if let Some((hide, _)) = native_hide_body() {
                    hide(this.0, !v);
                }
            }
            Ok(())
        });
        // The frames left before the worm releases its weapon, as a pack set
        // them; nil while WA runs its own wind-up (see `state`). Written,
        // the engine counts them down instead of WA's wind-up and releases
        // through WA's own path: 0 on the worm's next frame, nil gives the
        // wind-up back to WA. Cleared at every turn start.
        fields.add_field_method_get("windup", |_, this| {
            Ok(native_windup().and_then(|(_, get)| get(this.0)))
        });
        fields.add_field_method_set("windup", |_, this, v: Option<i32>| {
            crate::sim_context::require("worm.windup (write)")?;
            if v.is_some_and(|f| f < 0) {
                return Err(mlua::Error::external("worm.windup: frames are 0 or more, or nil"));
            }
            if this.0 != 0 {
                if let Some((set, _)) = native_windup() {
                    set(this.0, v);
                }
            }
            Ok(())
        });
        fields.add_field_method_get("turn", |_, this| {
            if this.0 == 0 {
                return Ok(false);
            }
            Ok(unsafe { read_i32(this.0 as *mut u8, WORM_TURN_ACTIVE) } != 0)
        });
        fields.add_field_method_get("hp", |_, this| {
            Ok(native_hp().map(|f| f(this.0)).unwrap_or(0))
        });
        fields.add_field_method_get("alliance", |_, this| {
            Ok(unsafe { game::team::worm_team_colour(this.0 as *mut u8) }
                .unwrap_or(0) as i32)
        });
        // WA indexes physical ammo by this separate owner/player row. It can
        // differ from the visible coalition colour exposed as `alliance`.
        fields.add_field_method_get("ammo_group", |_, this| {
            if this.0 == 0 {
                return Ok(0i32);
            }
            unsafe {
                let worm = this.0 as *mut u8;
                let world = read_i32(worm, ENT_WORLD) as *mut u8;
                if world.is_null() {
                    return Ok(0);
                }
                let team = read_i32(worm, ENT_WORM_TEAM);
                if !(1..=MAX_TEAMS as i32).contains(&team) {
                    return Ok(0);
                }
                Ok(read_i32(
                    world,
                    WORLD_TEAM_AMMO_GROUP + (team as usize) * WORLD_TEAM_STRIDE,
                ))
            }
        });
        bind_pos_fields(fields, |h| h.0);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // A semantic transition for Ice Arrow and other pack weapons. WA's
        // SetState runs its own frozen-state setup; no timer is held in Lua.
        methods.add_method("freeze", |_, this, ()| {
            crate::sim_context::require("worm:freeze")?;
            Ok(native_freeze_worm().map(|freeze| freeze(this.0)).unwrap_or(false))
        });
        methods.add_method("warp", |_, this, spec: Table| {
            crate::sim_context::require("worm:warp")?;
            let args = warp_args(&spec)?;
            let warp = NATIVE.lock().ok().and_then(|g| g.as_ref().map(|n| n.warp));
            Ok(warp.map(|f| f(this.0, &args)).unwrap_or(false))
        });
        methods.add_method("air_fire", |_, this, on: Option<bool>| {
            crate::sim_context::require("worm:air_fire")?;
            let air_fire = NATIVE.lock().ok().and_then(|g| g.as_ref().map(|n| n.air_fire));
            Ok(air_fire.map(|f| f(this.0, on.unwrap_or(true))).unwrap_or(false))
        });
        // `worm:set_state(...)`, through WA's own SetState. A debugging tool
        // on its way out: only for a pack whose mod.toml declares
        // `debug_worm_state = true`, every call logged. A lockstep write:
        // only from a simulation callback.
        methods.add_method("set_state", |lua, this, name: String| {
            let wa: Table = lua.globals().get("wa")?;
            let caller: String = wa.get("_mod").unwrap_or_default();
            let allowed = match wa.get::<Value>("_debug_worm_state")? {
                Value::Table(t) => t.get::<Option<bool>>(caller.as_str())?.unwrap_or(false),
                _ => false,
            };
            if !allowed {
                return Err(mlua::Error::external(format!(
                    "worm:set_state is a debugging tool: {caller:?} must declare \
                     debug_worm_state = true in its mod.toml. Hand a worm over with \
                     worm:take / worm:release / worm:carry / worm:drop instead."
                )));
            }
            let state = settable_worm_state(&name).ok_or_else(|| {
                mlua::Error::external(format!(
                    "worm:set_state: {name:?} is not a state a script may enter; valid: sliding, standing, bungee, tumbling, teleporting"
                ))
            })?;
            if !crate::sim_context::active() {
                return Err(mlua::Error::external(
                    "worm:set_state: only from a simulation callback (worm_message, a brain, ...)",
                ));
            }
            if let Some(write) = native_log() {
                write(&format!("[{caller}] worm:set_state({name:?}) (debug_worm_state)"));
            }
            if this.0 != 0 {
                if let Some(set) = native_set_worm_state() {
                    set(this.0, state);
                }
            }
            Ok(())
        });
        // `worm:clear_keys()`: consume this frame's move requests before WA's
        // handler sees them, as Project X's Helicopter does so WA's own jet
        // pack neither thrusts nor burns fuel. A lockstep write.
        methods.add_method("clear_keys", |_, this, ()| {
            if !crate::sim_context::active() {
                return Err(mlua::Error::external(
                    "worm:clear_keys: only from a simulation callback",
                ));
            }
            if this.0 != 0 {
                for (_, off) in WORM_KEYS {
                    unsafe { write_i32(this.0 as *mut u8, off, 0) };
                }
            }
            Ok(())
        });
        methods.add_method("carry", |_, this, (x, y): (i32, i32)| {
            crate::sim_context::require("worm:carry")?;
            if this.0 == 0 {
                return Ok(());
            }
            if let Some(carry) = native_carry() {
                carry(this.0, x, y);
            }
            Ok(())
        });
        methods.add_method("drop", |_, this, ()| {
            crate::sim_context::require("worm:drop")?;
            if this.0 == 0 {
                return Ok(());
            }
            if let Some(drop) = native_drop_carry() {
                drop(this.0);
            }
            Ok(())
        });
        // `worm:take{ hide = false, turn = "run" }`: the pack owns the worm
        // until `worm:release` (or the turn's end). It stays where it is until
        // the pack moves it with `worm:carry(x, y)`; the engine consumes its
        // move keys and jumps (after `worm_message` hooks have read them) and
        // turns its water off, so a vehicle may hold it in water. `hide`
        // hides it as `worm.visible = false`; `turn = "freeze"` holds the
        // turn's timer as `worm:carry` does, `"run"` lets it run.
        methods.add_method("take", |_, this, opts: Option<Table>| {
            crate::sim_context::require("worm:take")?;
            let (hide, freeze) = match &opts {
                None => (false, false),
                Some(t) => {
                    let hide = t.get::<Option<bool>>("hide")?.unwrap_or(false);
                    let freeze = match t.get::<Option<String>>("turn")?.as_deref() {
                        None | Some("run") => false,
                        Some("freeze") => true,
                        Some(other) => {
                            return Err(mlua::Error::external(format!(
                                "worm:take: turn is \"run\" or \"freeze\", got {other:?}"
                            )))
                        }
                    };
                    (hide, freeze)
                }
            };
            if this.0 != 0 {
                if let Some(take) = native_take() {
                    take(this.0, hide, freeze);
                }
            }
            Ok(())
        });
        // `worm:release(vx, vy, { tumble })`: give the worm back to WA moving
        // at (vx, vy), 16.16. A held worm is shown again and let go; any worm
        // then falls under WA's control (as from a closed parachute), and WA
        // flies and lands it with its own states. `tumble = true`: out of
        // control instead, as after a hit; in water WA drowns it. A hard
        // vehicle landing can suppress automatic parachute reopening until
        // WA moves the worm out of the falling or tumbling state.
        methods.add_method(
            "release",
            |_, this, (vx, vy, opts): (Option<i32>, Option<i32>, Option<Table>)| {
                crate::sim_context::require("worm:release")?;
                let tumble = match &opts {
                    Some(t) => t.get::<Option<bool>>("tumble")?.unwrap_or(false),
                    None => false,
                };
                let suppress_auto_parachute = match &opts {
                    Some(t) => t.get::<Option<bool>>("suppress_auto_parachute")?.unwrap_or(false),
                    None => false,
                };
                if this.0 != 0 {
                    if let Some(release) = native_release() {
                        release(this.0, vx.unwrap_or(0), vy.unwrap_or(0), tumble, suppress_auto_parachute);
                    }
                }
                Ok(())
            },
        );
        // `worm:fits(x, y)`: whether the worm would touch nothing at (x, y),
        // 16.16, by WA's own collision test and the worm's own shape (for a
        // taken worm, with the mask it had before it was taken). The optional
        // "terrain" mode checks land alone. A read.
        methods.add_method("fits", |_, this, (x, y, mode): (i32, i32, Option<String>)| {
            let terrain = fits_terrain_mode(mode.as_deref())?;
            if this.0 == 0 {
                return Ok(true);
            }
            let fits = if terrain { native_actor_fits() } else { native_fits() };
            Ok(fits.map(|f| f(this.0, x, y)).unwrap_or(true))
        });
        // `worm:jetpack_steer(on)`: while WA's jet pack runs, the pack steers
        // it (speed, fuel) and WA neither thrusts nor burns fuel from the keys;
        // WA keeps its collisions, fuel HUD and landing. Lapses when the jet
        // pack ends.
        methods.add_method("jetpack_steer", |_, this, on: Option<bool>| {
            crate::sim_context::require("worm:jetpack_steer")?;
            if this.0 != 0 {
                if let Some(steer) = native_jetpack_steer() {
                    steer(this.0, on.unwrap_or(true));
                }
            }
            Ok(())
        });
        methods.add_method("equip", |_, this, name: String| {
            crate::sim_context::require("worm:equip")?;
            if this.0 == 0 {
                return Ok(0i32);
            }
            let slot = parse_stock_weapon(&name).map_err(mlua::Error::external)?;
            let ammo = native_equip()
                .map(|f| f(this.0, slot as u32))
                .unwrap_or(0);
            Ok(ammo)
        });
        methods.add_method("ammo", |_, this, (name, change): (String, Value)| {
            if this.0 == 0 {
                return Ok(0i32);
            }
            let slot = parse_stock_weapon(&name).map_err(mlua::Error::external)?;
            let change = ammo_change(change)?;
            if change.is_some() {
                crate::sim_context::require("worm:ammo (a change)")?;
            }
            Ok(native_ammo()
                .map(|f| f(this.0, slot as u32, change))
                .unwrap_or(0))
        });
        methods.add_method("hurt", |_, this, arg: Value| {
            crate::sim_context::require("worm:hurt")?;
            if this.0 == 0 {
                return Ok(());
            }
            let (amount, vx, vy) = match arg {
                Value::Integer(n) => (n as i32, 0, 0),
                Value::Number(n) => (n as i32, 0, 0),
                Value::Table(t) => (
                    t.get::<Option<i32>>("amount")?.unwrap_or(0),
                    t.get::<Option<i32>>("vx")?.unwrap_or(0),
                    t.get::<Option<i32>>("vy")?.unwrap_or(0),
                ),
                other => {
                    return Err(mlua::Error::external(format!(
                        "hurt expects a number or {{ amount = n, vx, vy }}, got {other:?}"
                    )))
                }
            };
            if amount < 0 || (amount == 0 && vx == 0 && vy == 0) {
                return Ok(());
            }
            if let Some(hurt) = native_hurt() {
                hurt(this.0, spawn_owner(), amount, vx, vy);
            }
            Ok(())
        });
        methods.add_method("heal", |_, this, amount: i32| {
            if !crate::sim_context::active() {
                return Err(mlua::Error::external("worm:heal: only from a simulation callback"));
            }
            Ok(native_heal().map(|f| f(this.0, amount)).unwrap_or(0))
        });
    }
}

/// Stock world object (crate / mine / missile / oil). Pos only — no CTask offsets.
#[derive(Clone, Copy)]
pub struct WorldHandle(pub u32, pub u32);
impl WorldHandle {
    pub(crate) fn new(handle: u32) -> Self {
        Self(handle, NATIVE.lock().ok().and_then(|g| g.as_ref().map(|n| (n.body_uid)(handle))).unwrap_or(0))
    }
    fn live(self) -> bool {
        NATIVE.lock().ok().and_then(|g| g.as_ref().map(|n| (n.body_live)(self.0, self.1))).unwrap_or(false)
    }
}

impl UserData for WorldHandle {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("id", |_, this| Ok(this.0));
        fields.add_field_method_get("uid", |_, this| Ok(this.1));
        fields.add_field_method_get("kind", |_, this| Ok(kind_name(this.0)));
        fields.add_field_method_get("worm_id", |_, this| {
            Ok(native_grave_identity().map(|f| f(this.0)).unwrap_or(0))
        });
        fields.add_field_method_get("fuse", |_, this| {
            Ok(native_mine_fuse().map(|f| f(this.0)).unwrap_or(0))
        });
        // A stock missile's or mine's team (1-6, 0 none) and its colour, as
        // `worm.alliance` (nil without a team).
        fields.add_field_method_get("team", |_, this| {
            Ok(native_object_team().map(|f| f(this.0)).unwrap_or(0))
        });
        fields.add_field_method_get("alliance", |_, this| {
            let team = native_object_team().map(|f| f(this.0)).unwrap_or(0);
            if team == 0 {
                return Ok(None);
            }
            Ok(unsafe { game::team::entity_team_colour(this.0 as *mut u8, team) }.map(|c| c as i32))
        });
        bind_pos_fields(fields, |h| h.0);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("warp", |_, this, spec: Table| {
            crate::sim_context::require("object:warp")?;
            let args = warp_args(&spec)?;
            if !this.live() { return Ok(false); }
            let warp = NATIVE.lock().ok().and_then(|g| g.as_ref().map(|n| n.warp));
            Ok(warp.map(|f| f(this.0, &args)).unwrap_or(false))
        });
        methods.add_method("fits", |_, this, (x, y, mode): (i32, i32, Option<String>)| {
            fits_terrain_mode(mode.as_deref())?;
            if !this.live() { return Ok(false); }
            Ok(native_actor_fits().map(|f| f(this.0, x, y)).unwrap_or(false))
        });
        methods.add_method("arm", |_, this, fuse_ms: i32| {
            if !crate::sim_context::active() {
                return Err(mlua::Error::external("mine:arm: only from a simulation callback"));
            }
            Ok(native_arm_mine().map(|f| f(this.0, fuse_ms)).unwrap_or(false))
        });
        methods.add_method("impulse", |_, this, (dx, dy): (i32, i32)| {
            crate::sim_context::require("object:impulse")?;
            Ok(native_impulse_object().map(|f| f(this.0, dx, dy)).unwrap_or(false))
        });
        methods.add_method("remove", |_, this, ()| {
            crate::sim_context::require("object:remove")?;
            Ok(native_remove_object().map(|f| f(this.0)).unwrap_or(false))
        });
        methods.add_method("hit", |_, this, (damage, vx, vy, kind): (i32, i32, i32, Option<String>)| {
            if !crate::sim_context::active() {
                return Err(mlua::Error::external("object:hit: only from a simulation callback"));
            }
            let projectile = match kind.as_deref().unwrap_or("direct") {
                "direct" => false,
                "projectile" => true,
                _ => return Err(mlua::Error::external("object:hit: kind must be direct or projectile")),
            };
            Ok(native_hit_object().map(|f| f(this.0, spawn_owner(), damage, vx, vy, projectile)).unwrap_or(false))
        });
        methods.add_method("revive", |lua, this, (worm_id, health): (u32, i32)| {
            if !crate::sim_context::active() {
                return Err(mlua::Error::external("grave:revive: only from a simulation callback"));
            }
            let h = native_revive_grave().map(|f| f(this.0, worm_id, health)).unwrap_or(0);
            if h == 0 { Ok(None) } else { Ok(Some(worm_userdata(lua, h).map_err(mlua::Error::external)?)) }
        });
    }
}

/// Any entity as the handle Lua already knows it by: a worm, a LuaActor, or
/// a world object with a `kind`.
pub(crate) fn entity_value(lua: &Lua, handle: u32) -> Result<Value, String> {
    let v = if handle != 0 && unsafe { read_i32(handle as *mut u8, ENT_CLASS_TYPE) } == CLASS_WORM {
        Value::UserData(worm_userdata(lua, handle)?)
    } else if handle != 0 && unsafe { read_i32(handle as *mut u8, ACTOR_MAGIC) } as u32 == LUA_ACTOR_MAGIC {
        Value::UserData(lua.create_userdata(ActorHandle(handle)).map_err(|e| e.to_string())?)
    } else {
        Value::UserData(lua.create_userdata(WorldHandle::new(handle)).map_err(|e| e.to_string())?)
    };
    Ok(v)
}

fn kind_name(handle: u32) -> &'static str {
    if handle == 0 {
        return "none";
    }
    match unsafe { read_i32(handle as *mut u8, ENT_CLASS_TYPE) } {
        CLASS_MINE => "mine",
        CLASS_MISSILE => "missile",
        CLASS_CRATE => "crate",
        CLASS_WORM => "worm",
        CLASS_OIL => "oil",
        game::offsets::CLASS_GRAVE => "grave",
        CLASS_LAND => "land",
        game::offsets::CLASS_ARROW => "arrow",
        _ => "object",
    }
}

fn bind_pos_fields<T, F>(fields: &mut F, handle: fn(&T) -> u32)
where
    T: Copy + 'static,
    F: UserDataFields<T>,
{
    fields.add_field_method_get("x", move |_, this| Ok(read_handle(handle(this), ENT_POS_X)));
    fields.add_field_method_set("x", move |_, this, v: i32| {
        crate::sim_context::require("x (write)")?;
        write_handle(handle(this), ENT_POS_X, v);
        Ok(())
    });
    fields.add_field_method_get("y", move |_, this| Ok(read_handle(handle(this), ENT_POS_Y)));
    fields.add_field_method_set("y", move |_, this, v: i32| {
        crate::sim_context::require("y (write)")?;
        write_handle(handle(this), ENT_POS_Y, v);
        Ok(())
    });
    fields.add_field_method_get("vx", move |_, this| {
        Ok(read_handle(handle(this), ENT_SPEED_X))
    });
    fields.add_field_method_set("vx", move |_, this, v: i32| {
        crate::sim_context::require("vx (write)")?;
        write_handle(handle(this), ENT_SPEED_X, v);
        Ok(())
    });
    fields.add_field_method_get("vy", move |_, this| {
        Ok(read_handle(handle(this), ENT_SPEED_Y))
    });
    fields.add_field_method_set("vy", move |_, this, v: i32| {
        crate::sim_context::require("vy (write)")?;
        write_handle(handle(this), ENT_SPEED_Y, v);
        Ok(())
    });
}

fn native_explode() -> Option<fn(u32, i32, i32, bool)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.explode)
}

fn native_despawn_fn() -> Option<fn(u32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.despawn)
}

fn native_in_water_fn() -> Option<fn(u32) -> bool> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.in_water)
}

fn native_on_feet_fn() -> Option<fn(u32) -> bool> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.on_feet)
}

fn native_gravity() -> Option<fn(u32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.gravity)
}

fn native_move() -> Option<fn(u32, bool, bool) -> i32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.move_body)
}

fn native_advance() -> Option<fn(u32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.advance)
}

pub fn native_spawn() -> Option<fn(&SpawnArgs) -> Result<u32, String>> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.spawn)
}

pub fn native_list_actors() -> Vec<u32> {
    NATIVE
        .lock()
        .ok()
        .and_then(|n| n.as_ref().map(|n| (n.list_actors)()))
        .unwrap_or_default()
}

pub fn native_list_worms(from: u32) -> Vec<u32> {
    NATIVE
        .lock()
        .ok()
        .and_then(|n| n.as_ref().map(|n| (n.list_worms)(from)))
        .unwrap_or_default()
}

pub fn native_resolve_sprite() -> Option<fn(&str) -> Option<(i32, u32, i32, i32)>> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.resolve_sprite)
}

pub fn native_land_solid() -> Option<fn(i32, i32) -> bool> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.land_solid)
}

pub fn native_land_dig() -> Option<fn(i32, i32, i32) -> Result<(), String>> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.land_dig)
}

pub fn native_land_cut() -> Option<fn(i32, i32, i32) -> Result<(), String>> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.land_cut)
}

pub fn native_log() -> Option<fn(&str)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.log)
}

pub fn native_random() -> Option<fn() -> Option<u32>> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.random)
}

fn native_take() -> Option<fn(u32, bool, bool)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.take)
}

fn native_actor_fits() -> Option<fn(u32, i32, i32) -> bool> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.actor_fits)
}

fn native_fits() -> Option<fn(u32, i32, i32) -> bool> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.fits)
}

fn native_is_taken() -> Option<fn(u32) -> bool> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.is_taken)
}

fn native_release() -> Option<fn(u32, i32, i32, bool, bool)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.release)
}

fn native_windup() -> Option<(fn(u32, Option<i32>), fn(u32) -> Option<i32>)> {
    NATIVE.lock().ok()?.as_ref().map(|n| (n.set_windup, n.windup))
}

fn native_jetpack_steer() -> Option<fn(u32, bool)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.jetpack_steer)
}

pub fn native_set_worm_state() -> Option<fn(u32, u32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.set_worm_state)
}

pub fn native_freeze_worm() -> Option<fn(u32) -> bool> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.freeze_worm)
}

pub fn native_retreat_ms() -> Option<fn() -> i32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.retreat_ms)
}

pub fn native_turn_clock_ms() -> Option<fn() -> i32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.turn_clock_ms)
}

const WORM_KEYS: [(&str, usize); 4] = [
    ("up", WORM_KEY_UP),
    ("down", WORM_KEY_DOWN),
    ("left", WORM_KEY_LEFT),
    ("right", WORM_KEY_RIGHT),
];

/// `worm.state`: the WA states a pack can name, by the setup `SetState`
/// runs for them. Anything else reads as nil rather than a guessed name.
pub(crate) fn worm_state_name(state: u32) -> Option<&'static str> {
    Some(match state {
        WORM_STATE_STANDING => "standing",
        0x6A => "firing",
        0x6B => "fired",
        WORM_STATE_PREFIRING => "prefiring",
        WORM_STATE_PUNCHING => "punching",
        WORM_STATE_DRILLING => "drilling",
        WORM_STATE_KAMIKAZE => "kamikaze",
        WORM_STATE_TELEPORTING => "teleporting",
        WORM_STATE_JETPACKING => "jetpacking",
        WORM_STATE_PARACHUTING => "parachuting",
        WORM_STATE_AIRBORNE => "sliding",
        WORM_STATE_BUNGEE => "bungee",
        WORM_STATE_TUMBLING => "tumbling",
        WORM_STATE_FROZEN => "frozen",
        _ => return None,
    })
}

/// The states `worm:set_state` may enter: the ones a script ends a move
/// with, the two Project X's Swim Suit holds a worm in, and teleporting for
/// Super Tele's stock vanishing animation. Other action states would skip
/// WA's fire path. `"sinking"` is the old name of tumbling (0x7F), which is
/// WA's hit state, not drowning: it stays as an alias until packs move off it.
pub(crate) fn settable_worm_state(name: &str) -> Option<u32> {
    match name {
        "standing" => Some(WORM_STATE_STANDING),
        "sliding" => Some(WORM_STATE_AIRBORNE),
        "bungee" => Some(WORM_STATE_BUNGEE),
        "tumbling" | "sinking" => Some(WORM_STATE_TUMBLING),
        "teleporting" => Some(WORM_STATE_TELEPORTING),
        _ => None,
    }
}

pub fn native_networked() -> Option<fn() -> bool> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.networked)
}

pub fn native_land_trace() -> Option<fn(i32, i32, i32, i32) -> Option<(i32, i32)>> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.land_trace)
}

pub fn native_map_size() -> Option<fn() -> (i32, i32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.map_size)
}

pub fn native_view_cam() -> Option<fn() -> (i32, i32, i32, i32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.view_cam)
}

pub fn native_water_y() -> Option<fn() -> i32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.water_y)
}

pub fn native_list_kind(class: i32, from: u32) -> Vec<u32> {
    NATIVE
        .lock()
        .ok()
        .and_then(|n| n.as_ref().map(|n| (n.list_kind)(class, from)))
        .unwrap_or_default()
}

pub fn native_play_sound() -> Option<fn(u32, i32, i32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.play_sound)
}

pub fn native_resolve_sound() -> Option<fn(&str) -> i32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.resolve_sound)
}

pub fn native_pointer() -> Option<fn() -> (i32, i32, bool)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.pointer)
}

pub fn native_carry() -> Option<fn(u32, i32, i32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.carry)
}

pub fn native_drop_carry() -> Option<fn(u32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.drop_carry)
}

pub fn native_look() -> Option<fn(u32, i32, u32, i32, i32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.look)
}

pub fn native_set_gfx() -> Option<fn(u32, u32, f32, f32, u32, u32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.set_gfx)
}

pub fn native_set_frame() -> Option<fn(u32, i32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.set_frame)
}

pub fn native_keys() -> Option<fn() -> u32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.keys)
}

pub fn native_hide_body() -> Option<(fn(u32, bool), fn(u32) -> bool)> {
    NATIVE.lock().ok()?.as_ref().map(|n| (n.hide_body, n.body_hidden))
}

pub fn native_hide_worm() -> Option<fn(u32, bool)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.hide_worm)
}

pub fn native_worm_hidden() -> Option<fn(u32) -> bool> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.worm_hidden)
}

pub fn native_equip() -> Option<fn(u32, u32) -> i32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.equip)
}

pub fn native_hp() -> Option<fn(u32) -> i32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.hp)
}

fn native_heal() -> Option<fn(u32, i32) -> i32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.heal)
}

fn native_arm_mine() -> Option<fn(u32, i32) -> bool> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.arm_mine)
}

fn native_mine_fuse() -> Option<fn(u32) -> i32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.mine_fuse)
}

fn native_impulse_object() -> Option<fn(u32, i32, i32) -> bool> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.impulse_object)
}

fn native_object_team() -> Option<fn(u32) -> i32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.object_team)
}

fn native_remove_object() -> Option<fn(u32) -> bool> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.remove_object)
}

fn native_hit_object() -> Option<fn(u32, u32, i32, i32, i32, bool) -> bool> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.hit_object)
}

fn native_revive_grave() -> Option<fn(u32, u32, i32) -> u32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.revive_grave)
}

fn native_grave_identity() -> Option<fn(u32) -> u32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.grave_identity)
}

pub fn native_electrical_nearby() -> Option<fn(i32, i32, i32, bool) -> Vec<u32>> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.electrical_nearby)
}

pub fn native_electrical_trace() -> Option<fn(i32, i32, i32, i32) -> Option<(i32, i32, u32)>> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.electrical_trace)
}

pub fn native_ammo() -> Option<fn(u32, u32, Option<AmmoChange>) -> i32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.ammo)
}

pub fn native_cavern() -> Option<fn() -> bool> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.cavern)
}

pub fn native_forces() -> Option<fn() -> (i32, i32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.forces)
}

pub fn native_set_forces() -> Option<(fn(i32), fn(i32))> {
    NATIVE.lock().ok()?.as_ref().map(|n| (n.set_gravity, n.set_wind))
}

/// `worm:ammo(name)` reads, `worm:ammo(name, n)` adds, and
/// `worm:ammo(name, { set = n })` sets.
///
/// A table rather than a second method, so the one verb stays the one place a
/// pack looks for ammo. A table with anything but `set` in it is refused
/// rather than read as a read: `{ add = 1 }` silently doing nothing would be
/// the worst way to find out it isn't a key.
fn ammo_change(v: Value) -> mlua::Result<Option<AmmoChange>> {
    match v {
        Value::Nil => Ok(None),
        Value::Integer(n) => Ok(Some(AmmoChange::Add(n as i32))),
        Value::Number(n) => Ok(Some(AmmoChange::Add(n as i32))),
        Value::Table(t) => {
            for pair in t.clone().pairs::<Value, Value>() {
                let (k, _) = pair?;
                if !matches!(&k, Value::String(s) if s.to_str().map(|s| s == "set").unwrap_or(false)) {
                    return Err(mlua::Error::external(format!(
                        "ammo: the only table key is `set`, got {k:?}"
                    )));
                }
            }
            let n: i32 = t.get::<Option<i32>>("set")?.ok_or_else(|| {
                mlua::Error::external("ammo: `{ set = n }` needs a number")
            })?;
            if n < -1 {
                return Err(mlua::Error::external(format!(
                    "ammo: set = {n} is out of range (-1 is infinite, 0 or more is a count)"
                )));
            }
            Ok(Some(AmmoChange::Set(n)))
        }
        other => Err(mlua::Error::external(format!(
            "ammo expects nothing, a number or {{ set = n }}, got {other:?}"
        ))),
    }
}

fn native_hurt() -> Option<fn(u32, u32, i32, i32, i32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.hurt)
}

pub(crate) fn native_spawn_fire() -> Option<fn(&FireArgs) -> Result<u32, String>> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.spawn_fire)
}

pub(crate) fn native_spawn_worm() -> Option<fn(u32, i32, i32, &str, i32) -> Result<u32, String>> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.spawn_worm)
}

pub(crate) fn native_slot_click() -> Option<fn(u32) -> u32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.slot_click)
}

pub(crate) fn native_shape() -> Option<(fn(&ShapeQuery) -> ShapeHit, fn(&ShapeQuery) -> Option<u32>)> {
    NATIVE.lock().ok()?.as_ref().map(|n| (n.shape_trace, n.shape_collides))
}

pub(crate) fn native_spawn_crate() -> Option<fn(&CrateArgs) -> Result<u32, String>> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.spawn_crate)
}

pub(crate) fn native_launch() -> Option<fn(&LaunchArgs) -> Result<u32, String>> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.launch)
}

pub(crate) fn native_retarget_weapon_sprites() -> Option<fn(u32, i32, i32) -> Result<(), String>> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.retarget_weapon_sprites)
}

pub(crate) fn sound_slot(slot: Value) -> mlua::Result<i32> {
    match slot {
        Value::Integer(n) => Ok(n as i32),
        Value::Number(n) => Ok(n as i32),
        Value::String(s) => {
            let key = s.to_str()?.to_string();
            Ok(native_resolve_sound().map(|f| f(&key)).unwrap_or(0))
        }
        Value::Nil => Ok(0),
        other => Err(mlua::Error::external(format!(
            "sound id must be a number or pack WAV path, got {other:?}"
        ))),
    }
}

pub fn actor_owner(handle: u32) -> u32 {
    if handle == 0 {
        return 0;
    }
    read_handle(handle, ACTOR_OWNER) as u32
}

pub fn pos_tuple(handle: u32) -> (i32, i32, i32, i32) {
    (
        read_handle(handle, ENT_POS_X),
        read_handle(handle, ENT_POS_Y),
        read_handle(handle, ENT_SPEED_X),
        read_handle(handle, ENT_SPEED_Y),
    )
}

pub fn sprite_tuple(handle: u32) -> (i32, u32, i32, i32) {
    (
        read_handle(handle, ACTOR_SPRITE),
        read_handle(handle, ACTOR_DELTA) as u32,
        read_handle(handle, ACTOR_SPR_W),
        read_handle(handle, ACTOR_SPR_H),
    )
}

fn hit_table(lua: &Lua, bits: i32) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("terrain", bits & 1 != 0)?;
    t.set("worm", bits & 2 != 0)?;
    t.set("land", bits & 4 != 0)?;
    t.set("left_ground", bits & 8 != 0)?;
    Ok(t)
}

fn empty_hit(lua: &Lua) -> mlua::Result<Table> {
    hit_table(lua, 0)
}

/// A worm's identity as WA itself keys it: `team * 16 + index`.
///
/// Returns 0 for a null handle, which no live worm can be.
fn stable_worm_id(handle: u32) -> i32 {
    if handle == 0 {
        return 0;
    }
    let team = read_handle(handle, ENT_WORM_TEAM);
    let index = read_handle(handle, WORM_INDEX);
    team.wrapping_mul(16).wrapping_add(index)
}

fn actor_dead(handle: u32) -> bool {
    if !actor_live(handle) {
        return true;
    }
    unsafe {
        read_i32(handle as *mut u8, ACTOR_MAGIC) as u32 != LUA_ACTOR_MAGIC
            || read_i32(handle as *mut u8, ACTOR_DEAD) != 0
    }
}

fn read_handle(handle: u32, off: usize) -> i32 {
    if handle == 0 {
        return 0;
    }
    unsafe { read_i32(handle as *mut u8, off) }
}

fn write_handle(handle: u32, off: usize, v: i32) {
    if handle == 0 {
        return;
    }
    unsafe { write_i32(handle as *mut u8, off, v) }
}

fn opt_i32(t: &Table, key: &str) -> mlua::Result<Option<i32>> {
    match t.get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::Integer(i) => Ok(Some(i as i32)),
        Value::Number(n) => Ok(Some(n as i32)),
        other => Err(mlua::Error::external(format!(
            "{key} must be an integer, got {other:?}"
        ))),
    }
}

fn opt_bool(t: &Table, key: &str) -> mlua::Result<Option<bool>> {
    match t.get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::Boolean(b) => Ok(Some(b)),
        other => Err(mlua::Error::external(format!(
            "{key} must be a boolean, got {other:?}"
        ))),
    }
}

pub fn worm_userdata(lua: &Lua, handle: u32) -> Result<mlua::AnyUserData, String> {
    lua.create_userdata(WormHandle(handle))
        .map_err(|e| e.to_string())
}

/// WormEntity +0x270, the aim sweep WA draws: 0x0000 straight down, 0x7FFF
/// level, 0xFFFF straight up, facing choosing the side. Measured live by
/// wkFOW, whose view cone is built from it; +0x24C is the aim's target over
/// a whole turn, not what is drawn.
fn aim_fraction(raw: i32) -> f64 {
    f64::from(raw & 0xFFFF) / f64::from(0xFFFF)
}

/// The worm states in which WA draws the worm with its weapon out: 0x67
/// ready and its variants 0x68/0x69. 0x73 is rope rolling in live traces.
fn holds_weapon(state: u32) -> bool {
    matches!(state, 0x67..=0x69)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aim_is_the_drawn_sweep_from_down_to_up() {
        assert_eq!(super::aim_fraction(0x0000), 0.0);
        assert_eq!(super::aim_fraction(0xFFFF), 1.0);
        assert!((super::aim_fraction(0x7FFF) - 0.5).abs() < 1e-4);
        // Only the low 16 bits are the sweep.
        assert_eq!(super::aim_fraction(0x1_0000), 0.0);
    }

    #[test]
    fn aiming_is_ready_or_charging_the_weapon() {
        for state in [0x67, 0x68, 0x69] {
            assert!(super::holds_weapon(state), "{state:#x}");
        }
        // Idle, walking, drilling, jumping, hurt.
        for state in [0x65, 0x66, 0x6E, 0x73, 0x77, 0x80] {
            assert!(!super::holds_weapon(state), "{state:#x}");
        }
    }

    #[test]
    fn worm_states_are_named_by_their_setup_and_few_are_enterable() {
        assert_eq!(worm_state_name(0x78), Some("jetpacking"));
        assert_eq!(worm_state_name(0x6C), Some("punching"));
        assert_eq!(worm_state_name(0x7E), Some("sliding"));
        assert_eq!(worm_state_name(0x65), Some("standing"));
        assert_eq!(worm_state_name(0x69), Some("prefiring"));
        assert_eq!(worm_state_name(0x7D), Some("parachuting"));
        assert_eq!(worm_state_name(0x86), Some("frozen"));
        assert_eq!(worm_state_name(0x6D), Some("kamikaze"));
        // Not guessed: 0x7B's setup is Batroping, whatever it was called.
        assert_eq!(worm_state_name(0x7B), None);
        assert_eq!(settable_worm_state("sliding"), Some(0x7E));
        // Project X's Swim Suit: a bungee's state to swim, sinking when the
        // suit gives out; and both read back by name.
        assert_eq!(settable_worm_state("bungee"), Some(0x7C));
        assert_eq!(settable_worm_state("tumbling"), Some(0x7F));
        assert_eq!(settable_worm_state("sinking"), Some(0x7F), "the old name stays an alias");
        assert_eq!(settable_worm_state("teleporting"), Some(0x6F));
        assert_eq!((worm_state_name(0x7C), worm_state_name(0x7F)), (Some("bungee"), Some("tumbling")));
        assert_eq!(settable_worm_state("standing"), Some(0x65));
        assert_eq!(settable_worm_state("jetpacking"), None);
    }

    fn args(src: &str) -> mlua::Result<(u32, f32, f32, u32, u32)> {
        let lua = Lua::new();
        gfx_args(&lua.load(src).eval::<Table>()?)
    }

    fn change(src: &str) -> mlua::Result<Option<AmmoChange>> {
        let lua = Lua::new();
        ammo_change(lua.load(src).eval::<Value>()?)
    }

    /// The original forms keep their meaning: nothing reads, a number adds.
    #[test]
    fn ammo_read_and_add_are_unchanged() {
        assert_eq!(change("nil").unwrap(), None);
        assert_eq!(change("1").unwrap(), Some(AmmoChange::Add(1)));
        assert_eq!(change("-2").unwrap(), Some(AmmoChange::Add(-2)));
    }

    /// `-1` is how WA spells infinite, so it must be settable -- it is also the
    /// value `Set` exists to be able to overwrite.
    #[test]
    fn ammo_set_accepts_counts_and_infinite() {
        assert_eq!(change("{ set = 0 }").unwrap(), Some(AmmoChange::Set(0)));
        assert_eq!(change("{ set = 1 }").unwrap(), Some(AmmoChange::Set(1)));
        assert_eq!(change("{ set = -1 }").unwrap(), Some(AmmoChange::Set(-1)));
    }

    /// A misspelt key must fail loudly. Read as a plain read it would return
    /// the ammo and change nothing, and the pack would look like it worked.
    #[test]
    fn ammo_table_typos_and_nonsense_are_refused() {
        assert!(change("{ add = 1 }").is_err());
        assert!(change("{ set = 1, extra = 2 }").is_err());
        assert!(change("{}").is_err());
        assert!(change("{ set = -2 }").is_err());
        assert!(change("'one'").is_err());
    }

    /// The reason the mask exists: before it, `a:gfx{angle = 30}` also wrote
    /// scale 1.0 and tint white, so any single-field setter built on top of
    /// `gfx` would silently undo the others.
    #[test]
    fn only_the_keys_present_are_marked_for_writing() {
        let (mask, angle, ..) = args("return {angle = 30}").unwrap();
        assert_eq!(mask, gfx_field::ANGLE);
        assert_eq!(angle, 30.0);

        let (mask, ..) = args("return {}").unwrap();
        assert_eq!(mask, 0, "an empty table must write nothing at all");

        let (mask, _, _, tint, depth) =
            args("return {scale = 2, tint = 0x00FF00, blend = 'add', depth = 5}").unwrap();
        assert_eq!(
            mask,
            gfx_field::SCALE | gfx_field::TINT | gfx_field::BLEND | gfx_field::DEPTH
        );
        assert_eq!(tint, 0x0000_FF00, "additive is alpha 0, colour preserved");
        assert_eq!(depth, 5);
    }

    /// depth 0 used to be the "leave it alone" sentinel, which made layer 0
    /// unreachable. The mask carries that meaning now, so 0 is a real value.
    #[test]
    fn depth_zero_is_a_value_not_a_sentinel() {
        let (mask, _, _, _, depth) = args("return {depth = 0}").unwrap();
        assert_eq!(mask, gfx_field::DEPTH);
        assert_eq!(depth, 0);
    }

    #[test]
    fn blend_modes_map_to_the_tint_alpha_byte_and_typos_are_refused() {
        assert_eq!(blend_alpha("add").unwrap(), 0);
        assert_eq!(blend_alpha("additive").unwrap(), 0);
        assert_eq!(blend_alpha("normal").unwrap(), OPAQUE);
        assert_eq!(blend_alpha("alpha").unwrap(), OPAQUE);
        // Silently treating a typo as "normal" would show up as a missing
        // glow, which is a slow thing to track down from a screenshot.
        assert!(blend_alpha("addative").is_err());
        assert!(args("return {blend = 'screen'}").is_err());
    }

    #[test]
    fn a_tint_without_a_blend_is_opaque() {
        let (mask, _, _, tint, _) = args("return {tint = 0xFF8000}").unwrap();
        assert_eq!(mask, gfx_field::TINT);
        assert_eq!(tint, 0xFFFF_8000);
    }
}

/// `wa.land.stamp{ buried = { soften, blend } }`: how the stamp's pixels
/// that land on existing terrain are drawn. Each keeps `min(d, soften) /
/// soften` of its strength, `d` being its distance in pixels to the frame's
/// outline, so the sprite's thick middle stays whole and its edges fade into
/// the land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StampBuried {
    /// Pixels over which the strength grows from the outline, 1..=32.
    pub soften: u8,
    /// Blend each buried pixel's colour with the land's (`"mix"`) rather than
    /// keep or drop it against an ordered dither pattern (`"dither"`).
    pub mix: bool,
}

pub fn native_land_stamp() -> Option<fn(i32, u32, i32, i32, Option<StampBuried>) -> Result<(), String>> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.land_stamp)
}

fn fits_terrain_mode(mode: Option<&str>) -> mlua::Result<bool> {
    match mode {
        None => Ok(false),
        Some("terrain") => Ok(true),
        _ => Err(mlua::Error::external("fits: mode must be terrain or omitted")),
    }
}

fn warp_args(spec: &Table) -> mlua::Result<WarpArgs> {
    for key in spec.clone().pairs::<String, Value>() {
        let (key, _) = key?;
        if !matches!(key.as_str(), "x" | "y" | "vx" | "vy" | "facing") {
            return Err(mlua::Error::external(format!("warp: unknown field {key}")));
        }
    }
    let facing = spec.get::<Option<i32>>("facing")?;
    if facing.is_some_and(|v| v != -1 && v != 1) {
        return Err(mlua::Error::external("warp: facing must be -1 or 1"));
    }
    Ok(WarpArgs { x: spec.get("x")?, y: spec.get("y")?, vx: spec.get("vx")?, vy: spec.get("vy")?, facing })
}

pub fn weapon_air_fire(fire_id: u32, worm: u32, slot: i32, x: i32, y: i32) -> Result<(), String> {
    with_lua("weapon_air_fire", |lua| {
        let extra = lua.create_table().map_err(|e| e.to_string())?;
        extra.set("airborne", true).map_err(|e| e.to_string())?;
        call_fire(lua, fire_id, worm, slot, (x, y, 0, 0), Some(&extra))
    })
}
