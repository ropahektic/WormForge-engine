//! Game-thread Lua callbacks. Physics writes stay on these paths.
//!
//! v1 gravity/missile lists run for every missile (a shared bus). The
//! target is per-weapon dispatch once fire wrap tags a missile with its
//! owner. Do not add a second global FireFinal-style chain here.

use game::entity::Motion;
use mlua::{Function, Lua, Table, Value};
use std::cell::RefCell;

use crate::PARKED_LUA;
use weapons::StockWeapon;

/// One `wa.draw.quad`, in world pixels. The engine turns it into screen
/// space with the camera, exactly as it does the GPU sprites.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawQuad {
    /// Clockwise from the corner that takes the sheet's top-left.
    pub corners: [(f32, f32); 4],
    /// Per corner, 0xAARRGGBB, not premultiplied.
    pub colours: [u32; 4],
    pub sprite: i32,
    pub frame: u32,
    /// Added to the scene rather than drawn over it.
    pub additive: bool,
    /// Render-queue depth: at or behind terrain (0x180000) terrain hides it.
    pub depth: u32,
}

thread_local! {
    static DRAW_QUADS: RefCell<Vec<DrawQuad>> = const { RefCell::new(Vec::new()) };
    static DRAWING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// True while `wa.on.draw` handlers run: drawing is allowed, and anything
/// that would reach the lockstep simulation (`wa.random`) is refused.
pub fn drawing() -> bool {
    DRAWING.with(|d| d.get())
}

pub fn push_draw_quad(q: DrawQuad) {
    DRAW_QUADS.with(|s| s.borrow_mut().push(q));
}

/// Run every `wa.on.draw` handler for one rendered frame and return what it
/// drew. Render time, not simulation: it runs as often as WA renders, which
/// is not once per sim frame, and differently on every peer.
pub fn run_draw() -> Result<Vec<DrawQuad>, String> {
    DRAW_QUADS.with(|s| s.borrow_mut().clear());
    let ran = with_lua_impl(false, |lua| {
        let list = hook_list(lua, "draw")?;
        if list.len().map_err(|e| e.to_string())? == 0 {
            return Ok(());
        }
        DRAWING.with(|d| d.set(true));
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                DRAWING.with(|d| d.set(false));
            }
        }
        let _done = Done;
        call_each(&list, ())
    });
    let quads = DRAW_QUADS.with(|s| s.replace(Vec::new()));
    ran.map(|_| quads)
}

/// One contact WA reported: `worm` touched `other`. `vx` / `vy` are the
/// worm's speed before WA exchanged velocities; `flags` is WA's own argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contact {
    pub worm: u32,
    pub other: u32,
    pub flags: u32,
    pub vx: i32,
    pub vy: i32,
}

/// `wa.on.contact(function(worm, other, c) ... end)`: a simulation callback,
/// once per contact, after WA's frame.
pub fn contact(c: Contact) -> Result<(), String> {
    with_lua(|lua| {
        let list = hook_list(lua, "contact")?;
        if list.len().map_err(|e| e.to_string())? == 0 {
            return Ok(());
        }
        let worm = crate::actor::worm_userdata(lua, c.worm)?;
        let other = crate::actor::entity_value(lua, c.other)?;
        let t = lua.create_table().map_err(|e| e.to_string())?;
        t.set("vx", c.vx).map_err(|e| e.to_string())?;
        t.set("vy", c.vy).map_err(|e| e.to_string())?;
        t.set("flags", c.flags).map_err(|e| e.to_string())?;
        call_each(&list, (worm, other, t))
    })
}

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

fn with_lua_impl<T>(
    simulation: bool,
    f: impl FnOnce(&Lua) -> Result<T, String>,
) -> Result<T, String> {
    PARKED_LUA.with(|slot| {
        let guard = slot.borrow();
        let Some(lua) = guard.as_ref() else {
            return Err("no lua state".into());
        };
        if simulation {
            crate::sim_context::with_sim(|| f(lua))
        } else {
            f(lua)
        }
    })
}

fn with_lua<T>(f: impl FnOnce(&Lua) -> Result<T, String>) -> Result<T, String> {
    with_lua_impl(true, f)
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
    t.set("class_type", m.class_type)
        .map_err(|e| e.to_string())?;
    t.set("damp", m.damp).map_err(|e| e.to_string())?;
    t.set("wind_scale", m.wind_scale)
        .map_err(|e| e.to_string())?;
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

/// `slot`: the weapon the missile was fired from, when the engine knows it
/// (projectiles, their cluster bits, Armageddon; not strike bombs). Lua sees
/// it as `m.weapon`, the same stock slot name as `worm.weapon`.
///
/// `sprite`: the sprite WA draws the missile with (MissileEntity+0x2EC). Lua
/// sees it as `m.sprite` and may write it; 0 draws nothing, as Project X's
/// records and `red_spb` hide a missile it draws itself.
///
/// `mask`: the layers the missile collides with (+0x34, Project X's
/// `ColGroup`), as `m.collision_mask`, writable: Project X's Swim Suit lets a
/// missile in the water hit a swimming worm.
pub fn missile_frame(
    motion: Motion,
    msg: u32,
    phase: &str,
    slot: Option<usize>,
    sprite: &mut i32,
    mask: &mut i32,
) -> Result<Motion, String> {
    with_lua(|lua| {
        let list = hook_list(lua, "missile_frame")?;
        if list.len().map_err(|e| e.to_string())? == 0 {
            return Ok(motion);
        }
        let t = motion_to_table(lua, &motion)?;
        t.set("msg", msg).map_err(|e| e.to_string())?;
        t.set("phase", phase).map_err(|e| e.to_string())?;
        let weapon = slot
            .and_then(|s| StockWeapon::from_index(s as u32))
            .map(|w| w.name());
        t.set("weapon", weapon).map_err(|e| e.to_string())?;
        t.set("sprite", *sprite).map_err(|e| e.to_string())?;
        t.set("collision_mask", *mask as u32).map_err(|e| e.to_string())?;
        call_each(&list, t.clone())?;
        *sprite = t.get("sprite").unwrap_or(*sprite);
        *mask = t.get::<i64>("collision_mask").map(|v| v as u32 as i32).unwrap_or(*mask);
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
    let _ = with_lua_impl(false, |lua| {
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
    pub attacker: u32,
    pub slot: Option<i32>,
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
            t.set("attacker_team", by).map_err(|e| e.to_string())?;
        }
        if hit.attacker != 0 {
            t.set("attacker", crate::actor::worm_userdata(lua, hit.attacker)?)
                .map_err(|e| e.to_string())?;
        }
        if let Some(slot) = hit.slot {
            t.set("slot", slot).map_err(|e| e.to_string())?;
            if let Some(weapon) = StockWeapon::from_index(slot as u32) {
                t.set("weapon", weapon.name()).map_err(|e| e.to_string())?;
            }
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

#[derive(Clone, Debug)]
pub struct Death {
    pub victim: u32,
    pub active: u32,
    pub attacker: u32,
    pub attacker_team: Option<i32>,
    pub slot: Option<i32>,
    pub kind: Option<&'static str>,
}

pub fn turn_start(worm: u32) -> Result<(), String> {
    with_lua(|lua| {
        let list = hook_list(lua, "turn_start")?;
        if list.len().map_err(|e| e.to_string())? == 0 {
            return Ok(());
        }
        let event = lua.create_table().map_err(|e| e.to_string())?;
        let worm = crate::actor::worm_userdata(lua, worm)?;
        event.set("worm", worm.clone()).map_err(|e| e.to_string())?;
        event.set("active", worm).map_err(|e| e.to_string())?;
        call_each(&list, event)
    })
}

pub fn death(death: Death) -> Result<(), String> {
    with_lua(|lua| {
        let list = hook_list(lua, "death")?;
        if list.len().map_err(|e| e.to_string())? == 0 {
            return Ok(());
        }
        let event = lua.create_table().map_err(|e| e.to_string())?;
        event
            .set("victim", crate::actor::worm_userdata(lua, death.victim)?)
            .map_err(|e| e.to_string())?;
        if death.active != 0 {
            event
                .set("active", crate::actor::worm_userdata(lua, death.active)?)
                .map_err(|e| e.to_string())?;
        }
        if death.attacker != 0 {
            event
                .set(
                    "attacker",
                    crate::actor::worm_userdata(lua, death.attacker)?,
                )
                .map_err(|e| e.to_string())?;
        }
        if let Some(team) = death.attacker_team {
            event
                .set("attacker_team", team)
                .map_err(|e| e.to_string())?;
        }
        if let Some(slot) = death.slot {
            event.set("slot", slot).map_err(|e| e.to_string())?;
            if let Some(weapon) = StockWeapon::from_index(slot as u32) {
                event
                    .set("weapon", weapon.name())
                    .map_err(|e| e.to_string())?;
            }
        }
        if let Some(kind) = death.kind {
            event.set("kind", kind).map_err(|e| e.to_string())?;
        }
        call_each(&list, event)
    })
}
