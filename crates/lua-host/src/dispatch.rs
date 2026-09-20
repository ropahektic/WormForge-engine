//! Game-thread Lua callbacks. Physics writes stay on these paths.
//!
//! v1 gravity/missile lists run for every missile (a shared bus). The
//! target is per-weapon dispatch once fire wrap tags a missile with its
//! owner. Do not add a second global FireFinal-style chain here.

use game::entity::Motion;
use mlua::{Function, Lua, Table, Value};
use std::cell::RefCell;

use crate::PARKED_LUA;

#[derive(Clone, Debug)]
pub struct HudLine {
    pub text: String,
    pub x: i32,
    pub y: i32,
}

thread_local! {
    static HUD_LINES: RefCell<Vec<HudLine>> = const { RefCell::new(Vec::new()) };
}

pub fn push_hud_line(line: HudLine) {
    HUD_LINES.with(|s| s.borrow_mut().push(line));
}

pub fn take_hud_lines() -> Vec<HudLine> {
    HUD_LINES.with(|s| s.replace(Vec::new()))
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

fn hook_list<'a>(lua: &'a Lua, name: &str) -> Result<Table, String> {
    let wa: Table = lua.globals().get("wa").map_err(|e| e.to_string())?;
    let hooks: Table = wa.get("_hooks").map_err(|e| e.to_string())?;
    hooks.get(name).map_err(|e| e.to_string())
}

fn call_each(list: &Table, args: impl Clone + mlua::IntoLuaMulti) -> Result<(), String> {
    let n = list.len().map_err(|e| e.to_string())?;
    for i in 1..=n {
        let f: Function = list.get(i).map_err(|e| e.to_string())?;
        f.call::<()>(args.clone()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn motion_to_table(lua: &Lua, m: &Motion) -> Result<Table, String> {
    let t = lua.create_table().map_err(|e| e.to_string())?;
    t.set("id", m.id).map_err(|e| e.to_string())?;
    t.set("class_type", m.class_type).map_err(|e| e.to_string())?;
    t.set("damp", m.damp).map_err(|e| e.to_string())?;
    t.set("wind_scale", m.wind_scale).map_err(|e| e.to_string())?;
    t.set("gravity_scale", m.gravity_scale)
        .map_err(|e| e.to_string())?;
    t.set("moved", m.moved).map_err(|e| e.to_string())?;
    t.set("x", m.x).map_err(|e| e.to_string())?;
    t.set("y", m.y).map_err(|e| e.to_string())?;
    t.set("vx", m.vx).map_err(|e| e.to_string())?;
    t.set("vy", m.vy).map_err(|e| e.to_string())?;
    // CollisionManager globals this tick. Writing these in Lua does not
    // change the manager — use wind_scale / gravity_scale instead.
    t.set("cm_gravity", m.cm_gravity)
        .map_err(|e| e.to_string())?;
    t.set("cm_wind", m.cm_wind).map_err(|e| e.to_string())?;
    Ok(t)
}

pub fn table_to_motion(t: &Table, base: Motion) -> Result<Motion, String> {
    Ok(Motion {
        id: base.id,
        class_type: base.class_type,
        damp: t.get("damp").unwrap_or(base.damp),
        wind_scale: t.get("wind_scale").unwrap_or(base.wind_scale),
        gravity_scale: t.get("gravity_scale").unwrap_or(base.gravity_scale),
        moved: t.get("moved").unwrap_or(base.moved),
        x: t.get("x").unwrap_or(base.x),
        y: t.get("y").unwrap_or(base.y),
        vx: t.get("vx").unwrap_or(base.vx),
        vy: t.get("vy").unwrap_or(base.vy),
        cm_gravity: base.cm_gravity,
        cm_wind: base.cm_wind,
    })
}

fn run_motion_hook(name: &str, motion: Motion) -> Result<Motion, String> {
    with_lua(|lua| {
        let list = hook_list(lua, name)?;
        if list.len().map_err(|e| e.to_string())? == 0 {
            return Ok(motion);
        }
        let t = motion_to_table(lua, &motion)?;
        call_each(&list, t.clone())?;
        table_to_motion(&t, motion)
    })
}

/// Before stock gravity+wind+damp. Write `wind_scale` / `gravity_scale` / `damp`.
pub fn gravity_before(motion: Motion) -> Result<Motion, String> {
    run_motion_hook("gravity_before", motion)
}

/// After stock gravity+wind+damp. Write `vx` / `vy` / pos for bounce, stick, spin.
pub fn gravity_after(motion: Motion) -> Result<Motion, String> {
    run_motion_hook("gravity_after", motion)
}

pub fn missile_frame(motion: Motion, msg: u32, phase: &str) -> Result<Motion, String> {
    with_lua(|lua| {
        let list = hook_list(lua, "missile_frame")?;
        if list.len().map_err(|e| e.to_string())? == 0 {
            return Ok(motion);
        }
        let t = motion_to_table(lua, &motion)?;
        t.set("msg", msg).map_err(|e| e.to_string())?;
        t.set("phase", phase).map_err(|e| e.to_string())?;
        call_each(&list, t.clone())?;
        table_to_motion(&t, motion)
    })
}

/// Returns true if any handler returned the string "eat".
///
/// Jump/JumpUp already ride the lockstep stream (stock Enter/Backspace).
/// Swallowing them is extra and only safe when the scheme already ignores
/// jump (anchored). Prefer observing the message and leaving stock alone.
pub fn worm_message(worm_id: u32, team: u32, msg: u32) -> Result<bool, String> {
    with_lua(|lua| {
        let list = hook_list(lua, "worm_message")?;
        let n = list.len().map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(false);
        }
        let _ = team;
        let worm = crate::actor::worm_userdata(lua, worm_id)?;
        let mut eat = false;
        for i in 1..=n {
            let f: Function = list.get(i).map_err(|e| e.to_string())?;
            let ret: Value = f.call((worm.clone(), msg)).map_err(|e| e.to_string())?;
            if let Value::String(s) = ret {
                if s.to_str().map_err(|e| e.to_string())? == "eat" {
                    eat = true;
                }
            }
        }
        Ok(eat)
    })
}

pub fn run_hud() -> Result<Vec<HudLine>, String> {
    HUD_LINES.with(|s| s.borrow_mut().clear());
    let _ = with_lua(|lua| {
        let list = hook_list(lua, "hud")?;
        call_each(&list, ())?;
        Ok(())
    });
    Ok(take_hud_lines())
}

#[derive(Clone, Debug)]
pub struct Hurt {
    pub worm: u32,
    pub from: u32,
    pub by: Option<i32>,
    pub kind: &'static str,
    pub amount: i32,
    pub lost: i32,
    pub hp: i32,
    pub phase: &'static str,
}

/// Blast / hit / percent / nonlethal / poison. `pre` may `eat` or rewrite `amount`. `post` has `lost`.
pub fn hurt(hit: Hurt) -> Result<(bool, i32), String> {
    with_lua(|lua| {
        let list = hook_list(lua, "hurt")?;
        let n = list.len().map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok((false, hit.amount));
        }
        let t = lua.create_table().map_err(|e| e.to_string())?;
        t.set("worm", crate::actor::worm_userdata(lua, hit.worm)?)
            .map_err(|e| e.to_string())?;
        if hit.from != 0 {
            t.set("from", crate::actor::worm_userdata(lua, hit.from)?)
                .map_err(|e| e.to_string())?;
        }
        if let Some(by) = hit.by {
            t.set("by", by).map_err(|e| e.to_string())?;
        }
        t.set("kind", hit.kind).map_err(|e| e.to_string())?;
        t.set("amount", hit.amount).map_err(|e| e.to_string())?;
        t.set("lost", hit.lost).map_err(|e| e.to_string())?;
        t.set("hp", hit.hp).map_err(|e| e.to_string())?;
        t.set("phase", hit.phase).map_err(|e| e.to_string())?;
        let mut eat = false;
        for i in 1..=n {
            let f: Function = list.get(i).map_err(|e| e.to_string())?;
            let ret: Value = f.call(t.clone()).map_err(|e| e.to_string())?;
            if hit.phase == "pre" {
                if let Value::String(s) = ret {
                    if s.to_str().map_err(|e| e.to_string())? == "eat" {
                        eat = true;
                    }
                }
            }
        }
        let amount = t.get("amount").unwrap_or(hit.amount);
        Ok((eat, amount))
    })
}
