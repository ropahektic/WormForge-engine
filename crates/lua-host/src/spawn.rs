//! Parse spawn tables (GUI statuses + Lua `on`) and run `wa.actors.spawn`.

use crate::actor::{
    native_land_solid, native_land_trace, native_list_actors, native_list_kind, native_list_worms,
    native_map_size, native_resolve_sprite, native_spawn, native_view_cam, native_water_y,
    ActorHandle, SpawnArgs, WorldHandle, WormHandle,
};
use game::offsets::{CLASS_CRATE, CLASS_MINE, CLASS_MISSILE, CLASS_OIL};
use gfx::{is_sheet_path, sprite_key, stock_sprite_id};
use mlua::{Function, Lua, Table, Value};
use weapons::{ActorSpawn, ScreenBlit, SpawnExplode, SpawnFrom, SpriteOrigin};

pub fn parse_spawn(lua: &Lua, s: &Table) -> mlua::Result<ActorSpawn> {
    let mut spawn = ActorSpawn::default();
    spawn.from = match get_string(s, "from")?.as_deref() {
        None | Some("sky") => SpawnFrom::Sky,
        Some("cursor") => SpawnFrom::Cursor,
        Some("owner") => SpawnFrom::Cursor,
        Some(other) => {
            return Err(mlua::Error::external(format!(
                "spawn.from must be \"sky\", \"cursor\", or \"owner\", got {other}"
            )))
        }
    };
    spawn.origin = match get_string(s, "origin")?.as_deref() {
        None | Some("center") => SpriteOrigin::Center,
        Some("bottom") => SpriteOrigin::Bottom,
        Some(other) => {
            return Err(mlua::Error::external(format!(
                "spawn.origin must be \"center\" or \"bottom\", got {other}"
            )))
        }
    };
    spawn.brain_id = parse_brain(lua, s)?;
    let auto = spawn.brain_id == 0;
    spawn.gravity = get_bool(s, "gravity")?.unwrap_or(auto);
    spawn.loop_anim = get_bool(s, "loop")?.unwrap_or(auto);
    spawn.collide = get_bool(s, "collide")?.unwrap_or(auto);
    spawn.collide_terrain = get_bool(s, "collide_terrain")?.unwrap_or(spawn.collide);
    spawn.collide_worms = get_bool(s, "collide_worms")?.unwrap_or(spawn.collide);
    spawn.explode = parse_explode(s)?;
    let explode_on = spawn.explode.is_some();
    spawn.damage_terrain = get_bool(s, "damage_terrain")?.unwrap_or(explode_on);
    spawn.damage_worms = get_bool(s, "damage_worms")?.unwrap_or(explode_on);
    spawn.persist = get_bool(s, "persist")?.unwrap_or(explode_on && auto);
    spawn.drown = get_bool(s, "drown")?.unwrap_or(auto);
    spawn.screen = match get_string(s, "screen")?.as_deref() {
        None => {
            if get_bool(s, "screen")?.unwrap_or(false) {
                ScreenBlit::Front
            } else {
                ScreenBlit::World
            }
        }
        Some("fill") => ScreenBlit::Fill,
        Some("front") => ScreenBlit::Front,
        Some("world") => ScreenBlit::World,
        Some(other) => {
            return Err(mlua::Error::external(format!(
                "spawn.screen must be \"fill\", \"front\", or \"world\", got {other}"
            )))
        }
    };
    Ok(spawn)
}

pub fn parse_brain(lua: &Lua, s: &Table) -> mlua::Result<u32> {
    match s.get::<Value>("on")? {
        Value::Nil => Ok(0),
        Value::Function(frame) => crate::actor::register_brain(lua, Some(frame), None),
        Value::Table(on) => {
            let frame = get_fn(&on, "frame")?;
            let land = get_fn(&on, "land")?;
            crate::actor::register_brain(lua, frame, land)
        }
        other => Err(mlua::Error::external(format!(
            "spawn.on must be a function or table, got {other:?}"
        ))),
    }
}

pub fn parse_fire(lua: &Lua, spec: &Table) -> mlua::Result<(u32, Option<ActorSpawn>)> {
    parse_event(lua, spec, "on_fire", crate::actor::register_fire)
}

pub fn parse_click(lua: &Lua, spec: &Table) -> mlua::Result<(u32, Option<ActorSpawn>)> {
    parse_event(lua, spec, "on_click", crate::actor::register_click)
}

pub fn parse_select(lua: &Lua, spec: &Table) -> mlua::Result<(bool, u32)> {
    match spec.get::<Value>("on_select")? {
        Value::Nil => Ok((false, 0)),
        Value::Function(f) => Ok((false, crate::actor::register_select(lua, f)?)),
        Value::Table(t) => Ok((get_bool(&t, "cursor")?.unwrap_or(false), 0)),
        other => Err(mlua::Error::external(format!(
            "on_select must be a function or table, got {other:?}"
        ))),
    }
}

fn parse_event(
    lua: &Lua,
    spec: &Table,
    key: &str,
    register: fn(&Lua, Function) -> mlua::Result<u32>,
) -> mlua::Result<(u32, Option<ActorSpawn>)> {
    match spec.get::<Value>(key)? {
        Value::Nil => Ok((0, None)),
        Value::Function(f) => Ok((register(lua, f)?, None)),
        Value::Table(t) => {
            let id = match t.get::<Value>("on")? {
                Value::Function(f) => register(lua, f)?,
                Value::Nil => 0,
                other => {
                    return Err(mlua::Error::external(format!(
                        "{key}.on must be a function, got {other:?}"
                    )))
                }
            };
            let spawn = match t.get::<Value>("spawn")? {
                Value::Nil => None,
                Value::Table(s) => Some(parse_spawn(lua, &s)?),
                other => {
                    return Err(mlua::Error::external(format!(
                        "{key}.spawn must be a table, got {other:?}"
                    )))
                }
            };
            Ok((id, spawn))
        }
        other => Err(mlua::Error::external(format!(
            "{key} must be a function or table, got {other:?}"
        ))),
    }
}

pub fn bind_world(lua: &Lua, actors: &Table, wa: &Table) -> Result<(), String> {
    let spawn_fn = lua
        .create_function(|lua, spec: Table| runtime_spawn(lua, spec, None))
        .map_err(|e| e.to_string())?;
    actors.set("spawn", spawn_fn).map_err(|e| e.to_string())?;

    let all = lua
        .create_function(|lua, ()| handles_table(lua, native_list_actors(), true))
        .map_err(|e| e.to_string())?;
    actors.set("all", all).map_err(|e| e.to_string())?;

    let worms = lua.create_table().map_err(|e| e.to_string())?;
    let worms_all = lua
        .create_function(|lua, from: Option<Value>| {
            let from_h = optional_handle(&from)?;
            handles_table(lua, native_list_worms(from_h), false)
        })
        .map_err(|e| e.to_string())?;
    worms.set("all", worms_all).map_err(|e| e.to_string())?;
    let nearest = lua
        .create_function(|lua, from: Value| worms_nearest(lua, from))
        .map_err(|e| e.to_string())?;
    worms.set("nearest", nearest).map_err(|e| e.to_string())?;
    wa.set("worms", worms).map_err(|e| e.to_string())?;
    bind_kind_list(lua, wa, "crates", CLASS_CRATE)?;
    bind_kind_list(lua, wa, "mines", CLASS_MINE)?;
    bind_kind_list(lua, wa, "missiles", CLASS_MISSILE)?;
    bind_kind_list(lua, wa, "oil", CLASS_OIL)?;
    bind_land(lua, wa)?;
    bind_world_info(lua, wa)?;
    bind_sound(lua, wa)?;
    Ok(())
}

fn bind_kind_list(lua: &Lua, wa: &Table, name: &str, class: i32) -> Result<(), String> {
    let tbl = lua.create_table().map_err(|e| e.to_string())?;
    let all = lua
        .create_function(move |lua, from: Option<Value>| {
            let from_h = optional_handle(&from)?;
            objects_table(lua, native_list_kind(class, from_h))
        })
        .map_err(|e| e.to_string())?;
    tbl.set("all", all).map_err(|e| e.to_string())?;
    wa.set(name, tbl).map_err(|e| e.to_string())?;
    Ok(())
}

fn bind_land(lua: &Lua, wa: &Table) -> Result<(), String> {
    let land = lua.create_table().map_err(|e| e.to_string())?;
    let solid = lua
        .create_function(|_, (x, y): (i32, i32)| {
            Ok(native_land_solid().map(|f| f(x, y)).unwrap_or(false))
        })
        .map_err(|e| e.to_string())?;
    land.set("solid", solid).map_err(|e| e.to_string())?;
    let trace = lua
        .create_function(|lua, (x0, y0, x1, y1): (i32, i32, i32, i32)| {
            match native_land_trace().and_then(|f| f(x0, y0, x1, y1)) {
                Some((x, y)) => {
                    let t = lua.create_table()?;
                    t.set("x", x)?;
                    t.set("y", y)?;
                    t.set("hit", true)?;
                    Ok(Value::Table(t))
                }
                None => Ok(Value::Nil),
            }
        })
        .map_err(|e| e.to_string())?;
    land.set("trace", trace).map_err(|e| e.to_string())?;
    wa.set("land", land).map_err(|e| e.to_string())?;
    Ok(())
}

/// `wa.random()` — the only randomness a pack may use.
///
/// Mirrors `math.random`'s integer forms, which is what pack authors already
/// know: `wa.random()` gives the raw 32-bit draw, `wa.random(n)` gives `1..n`,
/// `wa.random(a, b)` gives `a..b`. There is deliberately no float form —
/// `math.random()`'s 0..1 invites float arithmetic in simulation code, and
/// nothing has needed it.
///
/// Errors rather than returns nil when the draw is unavailable. A pack that
/// silently got `nil` here would fail somewhere later and further away.
fn random_fn(lua: &Lua) -> Result<Function, String> {
    lua.create_function(|_, (a, b): (Option<i64>, Option<i64>)| {
        let draw = crate::actor::native_random()
            .and_then(|f| f())
            .ok_or_else(|| {
                mlua::Error::external(
                    "wa.random: WA's RNG is unavailable (no world yet, or the \
                     engine refused to call it). It cannot be substituted -- a \
                     host RNG would desync every peer.",
                )
            })?;
        let (lo, hi) = match (a, b) {
            (None, _) => return Ok(draw as i64),
            (Some(n), None) => (1, n),
            (Some(lo), Some(hi)) => (lo, hi),
        };
        if hi < lo {
            return Err(mlua::Error::external(format!(
                "wa.random: empty range {lo}..{hi}"
            )));
        }
        // Span is computed in i128 so that a full-i64 range cannot overflow.
        let span = (hi as i128 - lo as i128 + 1) as u128;
        // Modulo, biased for spans that do not divide 2^32 -- the same thing
        // WA itself does (`GS->GetRandom() % N` throughout Project X's
        // scripts). Rejection sampling would be less biased but would consume
        // a variable number of draws, and the count of draws is exactly what
        // has to match between peers and replays. Determinism wins.
        Ok(lo + (draw as u128 % span) as i64)
    })
    .map_err(|e| e.to_string())
}

fn bind_world_info(lua: &Lua, wa: &Table) -> Result<(), String> {
    let world = lua.create_table().map_err(|e| e.to_string())?;
    world
        .set("sky", game::offsets::DROP_SKY_Y)
        .map_err(|e| e.to_string())?;
    let water = lua
        .create_function(|_, ()| Ok(native_water_y().map(|f| f()).unwrap_or(0)))
        .map_err(|e| e.to_string())?;
    world.set("water", water).map_err(|e| e.to_string())?;
    let size = lua
        .create_function(|lua, ()| {
            let (w, h) = native_map_size().map(|f| f()).unwrap_or((0, 0));
            let t = lua.create_table()?;
            t.set("w", w)?;
            t.set("h", h)?;
            Ok(t)
        })
        .map_err(|e| e.to_string())?;
    world.set("size", size).map_err(|e| e.to_string())?;
    let view = lua
        .create_function(|lua, ()| {
            let (cx, cy, w, h) = native_view_cam().map(|f| f()).unwrap_or((0, 0, 0, 0));
            let t = lua.create_table()?;
            t.set("cx", cx)?;
            t.set("cy", cy)?;
            t.set("w", w)?;
            t.set("h", h)?;
            Ok(t)
        })
        .map_err(|e| e.to_string())?;
    world.set("view", view).map_err(|e| e.to_string())?;
    let pointer = lua
        .create_function(|lua, ()| {
            let (x, y, down) = crate::actor::native_pointer()
                .map(|f| f())
                .unwrap_or((0, 0, false));
            let t = lua.create_table()?;
            t.set("x", x)?;
            t.set("y", y)?;
            t.set("down", down)?;
            Ok(t)
        })
        .map_err(|e| e.to_string())?;
    world.set("pointer", pointer).map_err(|e| e.to_string())?;
    let keys = lua
        .create_function(|lua, ()| {
            let m = crate::actor::native_keys().map(|f| f()).unwrap_or(0);
            let t = lua.create_table()?;
            t.set("left", m & 1 != 0)?;
            t.set("right", m & 2 != 0)?;
            t.set("up", m & 4 != 0)?;
            t.set("down", m & 8 != 0)?;
            t.set("jump", m & 16 != 0)?;
            t.set("sprint", m & 32 != 0)?;
            t.set("q", m & 64 != 0)?;
            t.set("space", m & 128 != 0)?;
            Ok(t)
        })
        .map_err(|e| e.to_string())?;
    world.set("keys", keys).map_err(|e| e.to_string())?;

    // The guard that makes keys()/pointer() usable at all. They read THIS
    // machine's input, which never goes on the wire, so acting on them in a
    // networked game parts the two simulations. A pack that wants to stay
    // lockstep-safe checks this first and does nothing when it is true.
    let networked = lua
        .create_function(|_, ()| {
            Ok(crate::actor::native_networked().map(|f| f()).unwrap_or(false))
        })
        .map_err(|e| e.to_string())?;
    world.set("networked", networked).map_err(|e| e.to_string())?;
    wa.set("random", random_fn(lua)?).map_err(|e| e.to_string())?;
    wa.set("world", world).map_err(|e| e.to_string())?;
    Ok(())
}

fn bind_sound(lua: &Lua, wa: &Table) -> Result<(), String> {
    let sound = lua.create_table().map_err(|e| e.to_string())?;
    let play = lua
        .create_function(|_, (target, id, flags): (Value, Value, Option<i32>)| {
            let handle = parse_handle(&target)?;
            if handle == 0 {
                return Ok(());
            }
            let id = crate::actor::sound_slot(id)?;
            if id == 0 {
                return Ok(());
            }
            if let Some(play) = crate::actor::native_play_sound() {
                play(handle, id, flags.unwrap_or(8));
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    sound.set("play", play).map_err(|e| e.to_string())?;
    wa.set("sound", sound).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn runtime_spawn(lua: &Lua, spec: Table, inherit: Option<u32>) -> mlua::Result<ActorHandle> {
    let spawn = parse_spawn(lua, &spec)?;
    let inherit_owner = inherit
        .map(|h| crate::actor::actor_owner(h))
        .filter(|h| *h != 0)
        .or_else(|| {
            let o = crate::actor::spawn_owner();
            (o != 0).then_some(o)
        });
    let owner = match spec.get::<Value>("owner")? {
        Value::Nil => inherit_owner.unwrap_or(0),
        v => parse_handle(&v)?,
    };
    let (ix, iy, ivx, ivy) = inherit_pos(inherit);
    let x = get_i32(&spec, "x")?.unwrap_or(ix);
    let y = get_i32(&spec, "y")?.unwrap_or(iy);
    let vx = get_i32(&spec, "vx")?.unwrap_or(ivx);
    let vy = get_i32(&spec, "vy")?.unwrap_or(ivy);
    let (mut sprite, mut delta, mut w, mut h) = inherit_sprite(inherit);
    if let Some(name) = get_string(&spec, "sprite")? {
        let resolved = resolve_sprite_name(lua, &name)?;
        sprite = resolved.0;
        delta = resolved.1;
        if resolved.2 != 0 {
            w = resolved.2;
            h = resolved.3;
        }
    }
    let mut spawn = spawn;
    if spawn.sprite_w == 0 {
        spawn.sprite_w = w;
        spawn.sprite_h = h;
    }
    let spawn_fn = native_spawn().ok_or_else(|| {
        mlua::Error::external("wa.actors.spawn: native spawn not bound (game thread only)")
    })?;
    let handle = spawn_fn(&SpawnArgs {
        owner,
        x,
        y,
        vx,
        vy,
        sprite,
        delta,
        spawn,
    })
    .map_err(mlua::Error::external)?;
    Ok(ActorHandle(handle))
}

pub(crate) fn resolve_sprite_name(lua: &Lua, name: &str) -> mlua::Result<(i32, u32, i32, i32)> {
    if let Some(id) = stock_sprite_id(name) {
        return Ok((id, 0, 0, 0));
    }
    let key = if name.contains(':') {
        name.to_string()
    } else if is_sheet_path(name) {
        let wa: Table = lua.globals().get("wa")?;
        let mod_id: String = wa.get("_mod").unwrap_or_default();
        sprite_key(&mod_id, name)
    } else {
        return Err(mlua::Error::external(format!(
            "unknown sprite '{name}' (pack path, wa.sprites.key, or grenade/bazooka/mine/holy/cursor)"
        )));
    };
    native_resolve_sprite()
        .and_then(|f| f(&key))
        .ok_or_else(|| {
            mlua::Error::external(format!(
                "sprite '{name}' is not installed (pack PNG/GIF is loaded at gfx init)"
            ))
        })
}

fn inherit_pos(inherit: Option<u32>) -> (i32, i32, i32, i32) {
    let Some(h) = inherit else {
        return (0, 0, 0, 0);
    };
    crate::actor::pos_tuple(h)
}

fn inherit_sprite(inherit: Option<u32>) -> (i32, u32, i32, i32) {
    let Some(h) = inherit else {
        return (0, 0, 0, 0);
    };
    crate::actor::sprite_tuple(h)
}

fn handles_table(lua: &Lua, ids: Vec<u32>, actors: bool) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for (i, id) in ids.into_iter().enumerate() {
        if actors {
            t.set(i + 1, ActorHandle(id))?;
        } else {
            t.set(i + 1, WormHandle(id))?;
        }
    }
    Ok(t)
}

fn objects_table(lua: &Lua, ids: Vec<u32>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for (i, id) in ids.into_iter().enumerate() {
        t.set(i + 1, WorldHandle(id))?;
    }
    Ok(t)
}

fn worms_nearest(lua: &Lua, from: Value) -> mlua::Result<Option<WormHandle>> {
    let from_h = parse_handle(&from)?;
    let (x, y, _, _) = crate::actor::pos_tuple(from_h);
    let mut best: Option<(i64, u32)> = None;
    for id in native_list_worms(from_h) {
        if id == 0 || id == from_h {
            continue;
        }
        let (wx, wy, _, _) = crate::actor::pos_tuple(id);
        let dx = wx as i64 - x as i64;
        let dy = wy as i64 - y as i64;
        let d = dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy));
        if best.is_none() || best.is_some_and(|(bd, _)| d < bd) {
            best = Some((d, id));
        }
    }
    match best {
        Some((_, id)) => Ok(Some(WormHandle(id))),
        None => {
            let _ = lua;
            Ok(None)
        }
    }
}

fn optional_handle(v: &Option<Value>) -> mlua::Result<u32> {
    match v {
        None | Some(Value::Nil) => Ok(0),
        Some(v) => parse_handle(v),
    }
}

fn parse_handle(v: &Value) -> mlua::Result<u32> {
    match v {
        Value::Nil => Ok(0),
        // Deliberately refused. This used to accept a bare number as a raw
        // entity pointer, which was survivable only while `a.id` WAS that
        // pointer. Ids are now spawn-order sequence numbers, so a number
        // arriving here would be dereferenced as an address and take the game
        // down. No example pack ever did this -- every use of `.id` is as a
        // table key or a comparison -- so refusing costs nothing and closes
        // the hole.
        Value::Integer(_) | Value::Number(_) => Err(mlua::Error::external(
            "expected an actor or worm here, not a number. `a.id` is an              identity, not a handle -- pass the actor or worm itself.",
        )),
        Value::UserData(ud) => {
            if let Ok(w) = ud.borrow::<WormHandle>() {
                return Ok(w.0);
            }
            if let Ok(a) = ud.borrow::<ActorHandle>() {
                return Ok(a.0);
            }
            if let Ok(o) = ud.borrow::<WorldHandle>() {
                return Ok(o.0);
            }
            Err(mlua::Error::external(
                "owner/from must be an actor, worm, or world handle",
            ))
        }
        other => Err(mlua::Error::external(format!(
            "handle expected, got {other:?}"
        ))),
    }
}

fn parse_explode(s: &Table) -> mlua::Result<Option<SpawnExplode>> {
    match s.get::<Value>("explode")? {
        Value::Nil => Ok(None),
        Value::Boolean(false) => Ok(None),
        Value::Boolean(true) => Ok(Some(SpawnExplode::default())),
        Value::Table(t) => Ok(Some(SpawnExplode {
            damage: get_i32(&t, "damage")?.unwrap_or(50),
            id: get_i32(&t, "id")?.unwrap_or(100),
            below: get_bool(&t, "below")?.unwrap_or(true),
        })),
        other => Err(mlua::Error::external(format!(
            "spawn.explode must be a table or bool, got {other:?}"
        ))),
    }
}

fn get_fn(table: &Table, key: &str) -> mlua::Result<Option<Function>> {
    match table.get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::Function(f) => Ok(Some(f)),
        other => Err(mlua::Error::external(format!(
            "{key} must be a function, got {other:?}"
        ))),
    }
}

pub fn get_string(table: &Table, key: &str) -> mlua::Result<Option<String>> {
    match table.get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::String(s) => Ok(Some(s.to_str()?.to_string())),
        other => Err(mlua::Error::external(format!(
            "{key} must be a string, got {other:?}"
        ))),
    }
}

pub fn get_i32(table: &Table, key: &str) -> mlua::Result<Option<i32>> {
    match table.get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::Integer(i) => Ok(Some(i as i32)),
        Value::Number(n) => Ok(Some(n as i32)),
        other => Err(mlua::Error::external(format!(
            "{key} must be an integer, got {other:?}"
        ))),
    }
}

pub fn get_bool(table: &Table, key: &str) -> mlua::Result<Option<bool>> {
    match table.get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::Boolean(b) => Ok(Some(b)),
        other => Err(mlua::Error::external(format!(
            "{key} must be a boolean, got {other:?}"
        ))),
    }
}
