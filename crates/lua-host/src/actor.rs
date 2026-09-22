//! Per-actor Lua brains. Callbacks run on WorldEntity FRAME_FINISH — the same
//! lockstep pump wkGolf uses for fuse freeze, idle-arm, and worm teleports.
//! HUD must not call these verbs.

use game::entity::{read_i32, write_i32};
use game::offsets::{
    ACTOR_SEQ, WORM_INDEX,
    ACTOR_BOOM_ID, ACTOR_DAMAGE, ACTOR_DEAD, ACTOR_DELTA, ACTOR_FLAGS, ACTOR_FLAG_MIRROR_X,
    ACTOR_MAGIC, ACTOR_OWNER, ACTOR_SPRITE, ACTOR_SPR_H, ACTOR_SPR_W, CLASS_CRATE, CLASS_MINE,
    CLASS_MISSILE, CLASS_OIL, CLASS_WORM, ENT_CLASS_TYPE, ENT_POS_X, ENT_POS_Y, ENT_SPEED_X,
    ENT_SPEED_Y, ENT_WORLD, ENT_WORM_TEAM, LUA_ACTOR_MAGIC, WORLD_TEAM_ALLIANCE, WORLD_TEAM_STRIDE,
    WORM_FACING, WORM_SELECTED_WEAPON, WORM_TURN_ACTIVE,
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

pub struct ActorNative {
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
    /// Switch this actor's installed sheet and reset the animation phase.
    pub look: fn(handle: u32, sprite: i32, delta: u32, w: i32, h: i32),
    /// Keyboard bits: 1 left, 2 right, 4 up, 8 down, 16 W, 32 sprint, 64 Q, 128 Space.
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
    /// Stock SelectWeapon (F1 path). Returns ammo after equip, 0 if not this worm's turn.
    pub equip: fn(handle: u32, slot: u32) -> i32,
    pub hp: fn(handle: u32) -> i32,
    /// Team ammo. `delta = None` reads. Infinite stays -1.
    pub ammo: fn(handle: u32, slot: u32, delta: Option<i32>) -> i32,
    /// Stock SpecialImpact (msg 0x4B). No explosion. Pose restored so the worm does not move.
    pub hurt: fn(target: u32, from: u32, amount: i32),
    /// Fog-of-war master switch for the match. Enabling creates the engine
    /// instance from the current scheme; disabling drops it.
    pub fog_enable: fn(enable: bool),
    pub fog_enabled: fn() -> bool,
    /// Replace the active scheme (already validated by `fog::settings`).
    pub fog_set_scheme: fn(fog::Scheme),
    pub fog_scheme: fn() -> fog::Scheme,
    /// Light level (0..=255) at a world 16.16 position, for `wa.fog.visible`.
    pub fog_visible: fn(x: i32, y: i32) -> u8,
}

impl Default for ActorNative {
    fn default() -> Self {
        Self {
            explode: |_, _, _, _| {},
            despawn: |_| {},
            set_gfx: |_, _, _, _, _, _| {},
            networked: || false,
            random: || None,
            log: |_| {},
            in_water: |_| false,
            on_feet: |_| false,
            gravity: |_| {},
            move_body: |_, _, _| 0,
            advance: |_| {},
            set_frame: |_, _| {},
            spawn: |_| Err("spawn not bound".into()),
            list_actors: Vec::new,
            list_worms: |_| Vec::new(),
            resolve_sprite: |_| None,
            land_solid: |_, _| false,
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
            look: |_, _, _, _, _| {},
            keys: || 0,
            hide_worm: |_, _| {},
            worm_hidden: |_| false,
            equip: |_, _| 0,
            hp: |_| 0,
            ammo: |_, _, _| 0,
            hurt: |_, _, _| {},
            fog_enable: |_| {},
            fog_enabled: || false,
            fog_set_scheme: |_| {},
            fog_scheme: fog::Scheme::default,
            fog_visible: |_, _| 255,
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
) -> mlua::Result<u32> {
    if frame.is_none() && land.is_none() {
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
    with_lua(|lua| {
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
        if let Ok(m) = spec.get::<String>("mod") {
            let _ = wa.set("_mod", m);
        }
        let fire = lua.create_table().map_err(|e| e.to_string())?;
        fire.set("x", x).map_err(|e| e.to_string())?;
        fire.set("y", y).map_err(|e| e.to_string())?;
        fire.set("vx", vx).map_err(|e| e.to_string())?;
        fire.set("vy", vy).map_err(|e| e.to_string())?;
        fire.set("slot", slot).map_err(|e| e.to_string())?;
        fire.set("worm", WormHandle(worm))
            .map_err(|e| e.to_string())?;
        with_spawn_owner(worm, || cb.call::<()>(fire).map_err(|e| e.to_string()))?;
        Ok(())
    })
}

pub fn weapon_click(
    click_id: u32,
    worm: u32,
    slot: i32,
    x: i32,
    y: i32,
    sky: i32,
) -> Result<(), String> {
    if click_id == 0 {
        return Ok(());
    }
    with_lua(|lua| {
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
        if let Ok(m) = spec.get::<String>("mod") {
            let _ = wa.set("_mod", m);
        }
        let click = lua.create_table().map_err(|e| e.to_string())?;
        click.set("x", x).map_err(|e| e.to_string())?;
        click.set("y", y).map_err(|e| e.to_string())?;
        click.set("sky", sky).map_err(|e| e.to_string())?;
        click.set("slot", slot).map_err(|e| e.to_string())?;
        click
            .set("worm", WormHandle(worm))
            .map_err(|e| e.to_string())?;
        with_spawn_owner(worm, || cb.call::<()>(click).map_err(|e| e.to_string()))?;
        Ok(())
    })
}

pub fn weapon_select(select_id: u32, worm: u32, slot: i32) -> Result<(), String> {
    if select_id == 0 {
        return Ok(());
    }
    let (x, y, _) = native_pointer().map(|f| f()).unwrap_or((0, 0, false));
    with_lua(|lua| {
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
        if let Ok(m) = spec.get::<String>("mod") {
            let _ = wa.set("_mod", m);
        }
        let sel = lua.create_table().map_err(|e| e.to_string())?;
        sel.set("x", x).map_err(|e| e.to_string())?;
        sel.set("y", y).map_err(|e| e.to_string())?;
        sel.set("slot", slot).map_err(|e| e.to_string())?;
        sel.set("worm", WormHandle(worm))
            .map_err(|e| e.to_string())?;
        with_spawn_owner(worm, || cb.call::<()>(sel).map_err(|e| e.to_string()))?;
        Ok(())
    })
}

pub fn actor_spawned(_handle: u32, _brain_id: u32) {}

pub fn actor_freed(_handle: u32) {}

pub fn actor_frame(handle: u32, brain_id: u32, ev: ActorEvent) -> Result<(), String> {
    if handle == 0 || brain_id == 0 {
        return Ok(());
    }
    with_lua(|lua| {
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
            f.call::<()>((actor, t)).map_err(|e| e.to_string())?;
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

fn with_lua<T>(f: impl FnOnce(&Lua) -> Result<T, String>) -> Result<T, String> {
    PARKED_LUA.with(|slot| {
        let guard = slot.borrow();
        let Some(lua) = guard.as_ref() else {
            return Err("no lua state".into());
        };
        f(lua)
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
        fields.add_field_method_get("id", |_, this| Ok(read_handle(this.0, ACTOR_SEQ)));
        fields.add_field_method_get("kind", |_, _| Ok("actor"));
        bind_pos_fields(fields, |h| h.0);
        fields.add_field_method_get("owner", |lua, this| {
            let owner = read_handle(this.0, ACTOR_OWNER);
            lua.create_userdata(WormHandle(owner as u32))
        });
        fields.add_field_method_get("facing", |_, this| {
            let flags = read_handle(this.0, ACTOR_FLAGS);
            Ok(if flags & ACTOR_FLAG_MIRROR_X != 0 {
                -1i32
            } else {
                1i32
            })
        });
        fields.add_field_method_set("facing", |_, this, v: i32| {
            let mut flags = read_handle(this.0, ACTOR_FLAGS);
            if v < 0 {
                flags |= ACTOR_FLAG_MIRROR_X;
            } else {
                flags &= !ACTOR_FLAG_MIRROR_X;
            }
            write_handle(this.0, ACTOR_FLAGS, flags);
            Ok(())
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("explode", |_, this, opts: Option<Table>| {
            if this.0 == 0 || actor_dead(this.0) {
                return Ok(());
            }
            let mut damage = read_handle(this.0, ACTOR_DAMAGE);
            let mut id = read_handle(this.0, ACTOR_BOOM_ID);
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
            let explode = native_explode()
                .ok_or_else(|| mlua::Error::external("explode: CreateExplosion not bound"))?;
            explode(this.0, damage, id, below);
            Ok(())
        });
        methods.add_method("despawn", |_, this, ()| {
            if this.0 == 0 || actor_dead(this.0) {
                return Ok(());
            }
            let despawn = native_despawn_fn()
                .ok_or_else(|| mlua::Error::external("despawn: native free not bound"))?;
            despawn(this.0);
            Ok(())
        });
        methods.add_method("in_water", |_, this, ()| {
            if this.0 == 0 {
                return Ok(false);
            }
            Ok(native_in_water_fn().map(|f| f(this.0)).unwrap_or(false))
        });
        methods.add_method("on_feet", |_, this, ()| {
            if this.0 == 0 {
                return Ok(false);
            }
            Ok(native_on_feet_fn().map(|f| f(this.0)).unwrap_or(false))
        });
        methods.add_method("gravity", |_, this, ()| {
            if this.0 == 0 || actor_dead(this.0) {
                return Ok(());
            }
            let gravity = native_gravity()
                .ok_or_else(|| mlua::Error::external("gravity: stock leaf not bound"))?;
            gravity(this.0);
            Ok(())
        });
        methods.add_method("move", |lua, this, opts: Option<Table>| {
            if this.0 == 0 || actor_dead(this.0) {
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
            let move_body = native_move()
                .ok_or_else(|| mlua::Error::external("move: native step not bound"))?;
            hit_table(lua, move_body(this.0, terrain, worms))
        });
        methods.add_method("advance", |_, this, ()| {
            if this.0 == 0 || actor_dead(this.0) {
                return Ok(());
            }
            let advance = native_advance()
                .ok_or_else(|| mlua::Error::external("advance: sprite loop not bound"))?;
            advance(this.0);
            Ok(())
        });
        // Set several presentation fields at once. Only the keys actually
        // present are written -- `a:gfx{angle = 30}` leaves scale, tint and
        // depth exactly as they were.
        methods.add_method("gfx", |_, this, spec: Table| {
            let (mask, angle, scale, tint, depth) = gfx_args(&spec)?;
            apply_gfx(this.0, mask, angle, scale, tint, depth)
        });
        // Single-field setters. They forward to the same native call with a
        // one-bit mask, so each is genuinely independent of the others.
        methods.add_method("angle", |_, this, degrees: f32| {
            apply_gfx(this.0, gfx_field::ANGLE, degrees, 1.0, 0, 0)
        });
        methods.add_method("scale", |_, this, factor: f32| {
            apply_gfx(this.0, gfx_field::SCALE, 0.0, factor, 0, 0)
        });
        methods.add_method("tint", |_, this, rgb: u32| {
            apply_gfx(this.0, gfx_field::TINT, 0.0, 1.0, rgb & 0x00FF_FFFF, 0)
        });
        methods.add_method("blend", |_, this, mode: String| {
            apply_gfx(this.0, gfx_field::BLEND, 0.0, 1.0, blend_alpha(&mode)?, 0)
        });
        methods.add_method("frame", |_, this, frame: i32| {
            if this.0 == 0 || actor_dead(this.0) {
                return Ok(());
            }
            if let Some(set) = native_set_frame() {
                set(this.0, frame);
            }
            Ok(())
        });
        methods.add_method("look", |lua, this, name: String| {
            if this.0 == 0 || actor_dead(this.0) {
                return Ok(());
            }
            let (id, delta, w, h) = crate::spawn::resolve_sprite_name(lua, &name)?;
            if let Some(look) = native_look() {
                look(this.0, id, delta, w, h);
            }
            Ok(())
        });
        methods.add_method("spawn", |lua, this, spec: Table| {
            crate::spawn::runtime_spawn(lua, spec, Some(this.0))
        });
        methods.add_method("sound", |_, this, (slot, flags): (Value, Option<i32>)| {
            if this.0 == 0 || actor_dead(this.0) {
                return Ok(());
            }
            let id = sound_slot(slot)?;
            if id == 0 {
                return Ok(());
            }
            let play = native_play_sound()
                .ok_or_else(|| mlua::Error::external("sound: PlaySoundLocal not bound"))?;
            play(this.0, id, flags.unwrap_or(8));
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
                let direction = unsafe { read_i32(this.0 as *mut u8, WORM_FACING) };
                if direction < 0 {
                    -1
                } else {
                    1
                }
            })
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
        fields.add_field_method_get("kind", |_, _| Ok("worm"));
        fields.add_field_method_get("visible", |_, this| {
            Ok(!native_worm_hidden().map(|f| f(this.0)).unwrap_or(false))
        });
        fields.add_field_method_set("visible", |_, this, v: bool| {
            if this.0 == 0 {
                return Ok(());
            }
            if let Some(hide) = native_hide_worm() {
                hide(this.0, !v);
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
                if team < 0 {
                    return Ok(0);
                }
                Ok(read_i32(
                    world,
                    WORLD_TEAM_ALLIANCE + (team as usize) * WORLD_TEAM_STRIDE,
                ))
            }
        });
        bind_pos_fields(fields, |h| h.0);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("carry", |_, this, (x, y): (i32, i32)| {
            if this.0 == 0 {
                return Ok(());
            }
            if let Some(carry) = native_carry() {
                carry(this.0, x, y);
            }
            Ok(())
        });
        methods.add_method("drop", |_, this, ()| {
            if this.0 == 0 {
                return Ok(());
            }
            if let Some(drop) = native_drop_carry() {
                drop(this.0);
            }
            Ok(())
        });
        methods.add_method("equip", |_, this, name: String| {
            if this.0 == 0 {
                return Ok(0i32);
            }
            let slot = parse_stock_weapon(&name).map_err(mlua::Error::external)?;
            let ammo = native_equip().map(|f| f(this.0, slot as u32)).unwrap_or(0);
            Ok(ammo)
        });
        methods.add_method("ammo", |_, this, (name, delta): (String, Option<i32>)| {
            if this.0 == 0 {
                return Ok(0i32);
            }
            let slot = parse_stock_weapon(&name).map_err(mlua::Error::external)?;
            Ok(native_ammo()
                .map(|f| f(this.0, slot as u32, delta))
                .unwrap_or(0))
        });
        methods.add_method("hurt", |_, this, arg: Value| {
            if this.0 == 0 {
                return Ok(());
            }
            let amount = match arg {
                Value::Integer(n) => n as i32,
                Value::Number(n) => n as i32,
                Value::Table(t) => t.get::<Option<i32>>("amount")?.unwrap_or(0),
                other => {
                    return Err(mlua::Error::external(format!(
                        "hurt expects a number or {{ amount = n }}, got {other:?}"
                    )))
                }
            };
            if amount <= 0 {
                return Ok(());
            }
            if let Some(hurt) = native_hurt() {
                hurt(this.0, spawn_owner(), amount);
            }
            Ok(())
        });
    }
}

/// Stock world object (crate / mine / missile / oil). Pos only — no CTask offsets.
#[derive(Clone, Copy)]
pub struct WorldHandle(pub u32);

impl UserData for WorldHandle {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("id", |_, this| Ok(this.0));
        fields.add_field_method_get("kind", |_, this| Ok(kind_name(this.0)));
        bind_pos_fields(fields, |h| h.0);
    }
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
        write_handle(handle(this), ENT_POS_X, v);
        Ok(())
    });
    fields.add_field_method_get("y", move |_, this| Ok(read_handle(handle(this), ENT_POS_Y)));
    fields.add_field_method_set("y", move |_, this, v: i32| {
        write_handle(handle(this), ENT_POS_Y, v);
        Ok(())
    });
    fields.add_field_method_get("vx", move |_, this| {
        Ok(read_handle(handle(this), ENT_SPEED_X))
    });
    fields.add_field_method_set("vx", move |_, this, v: i32| {
        write_handle(handle(this), ENT_SPEED_X, v);
        Ok(())
    });
    fields.add_field_method_get("vy", move |_, this| {
        Ok(read_handle(handle(this), ENT_SPEED_Y))
    });
    fields.add_field_method_set("vy", move |_, this, v: i32| {
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

pub fn native_set_gfx() -> Option<fn(u32, u32, f32, f32, u32, u32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.set_gfx)
}

pub fn native_log() -> Option<fn(&str)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.log)
}

pub fn native_random() -> Option<fn() -> Option<u32>> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.random)
}

pub fn native_networked() -> Option<fn() -> bool> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.networked)
}

pub fn native_set_frame() -> Option<fn(u32, i32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.set_frame)
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

pub fn native_keys() -> Option<fn() -> u32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.keys)
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

pub fn native_ammo() -> Option<fn(u32, u32, Option<i32>) -> i32> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.ammo)
}

fn native_hurt() -> Option<fn(u32, u32, i32)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.hurt)
}

pub fn native_fog_enable() -> Option<fn(bool)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.fog_enable)
}

pub fn native_fog_enabled() -> bool {
    NATIVE
        .lock()
        .ok()
        .and_then(|n| n.as_ref().map(|n| (n.fog_enabled)()))
        .unwrap_or(false)
}

pub fn native_fog_set_scheme() -> Option<fn(fog::Scheme)> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.fog_set_scheme)
}

pub fn native_fog_scheme() -> fog::Scheme {
    NATIVE
        .lock()
        .ok()
        .and_then(|n| n.as_ref().map(|n| (n.fog_scheme)()))
        .unwrap_or_default()
}

pub fn native_fog_visible() -> Option<fn(i32, i32) -> u8> {
    NATIVE.lock().ok()?.as_ref().map(|n| n.fog_visible)
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
    if handle == 0 {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(src: &str) -> mlua::Result<(u32, f32, f32, u32, u32)> {
        let lua = Lua::new();
        gfx_args(&lua.load(src).eval::<Table>()?)
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
