use crate::dispatch::{self, HudLine};
use crate::spawn::{self, get_bool, get_i32, get_string};
use actors::kind_id;
use gfx::{is_sheet_path, load_sprite_path, sprite_key, stock_sprite_id, SpriteStrip};
use mlua::{Function, Lua, Table, Value};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use weapons::{parse_stock_weapon, OverlayClaim, OverlayParams, StockWeapon};

pub struct BindContext {
    pub mod_id: String,
    pub root: PathBuf,
    pub allowed_slots: HashSet<StockWeapon>,
    pub allowed_register: HashSet<String>,
    pub claims: Rc<RefCell<Vec<OverlayClaim>>>,
    pub logs: Rc<RefCell<Vec<String>>>,
}

pub fn install(lua: &Lua, ctx: &BindContext) -> Result<(), String> {
    let wa = ensure_wa(lua)?;
    wa.set("_mod", ctx.mod_id.clone())
        .map_err(|e| e.to_string())?;

    let logs = Rc::clone(&ctx.logs);
    let mod_id_log = ctx.mod_id.clone();
    let log_fn = lua
        .create_function(move |_, msg: String| {
            logs.borrow_mut().push(format!("[{mod_id_log}] {msg}"));
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    wa.set("log", log_fn).map_err(|e| e.to_string())?;

    let weapons: Table = wa.get("weapons").map_err(|e| e.to_string())?;
    let push_claim = make_push(ctx);

    {
        let push_claim = Rc::clone(&push_claim);
        let mod_id = ctx.mod_id.clone();
        let root = ctx.root.clone();
        let f = lua
            .create_function(move |lua, spec: Table| {
                let slot = spec_slot(&spec)?;
                let claim = table_to_claim(lua, &mod_id, &root, slot, spec)?;
                push_claim(claim, "replace")
            })
            .map_err(|e| e.to_string())?;
        weapons.set("replace", f).map_err(|e| e.to_string())?;
    }

    {
        let push_claim = Rc::clone(&push_claim);
        let mod_id = ctx.mod_id.clone();
        let root = ctx.root.clone();
        let f = lua
            .create_function(move |lua, spec: Table| {
                let based_on: Option<String> = spec.get("based_on")?;
                let based_on = based_on.ok_or_else(|| {
                    mlua::Error::external(
                        "register() occupies a stock slot in this version; set based_on",
                    )
                })?;
                let slot = parse_stock_weapon(&based_on).map_err(mlua::Error::external)?;
                let claim = table_to_claim(lua, &mod_id, &root, slot, spec)?;
                push_claim(claim, "register")
            })
            .map_err(|e| e.to_string())?;
        weapons.set("register", f).map_err(|e| e.to_string())?;
    }

    let mod_id_gfx = ctx.mod_id.clone();
    let sprites: Table = wa.get("sprites").map_err(|e| e.to_string())?;
    let key_fn = lua
        .create_function(move |_, relative: String| Ok(sprite_key(&mod_id_gfx, &relative)))
        .map_err(|e| e.to_string())?;
    sprites.set("key", key_fn).map_err(|e| e.to_string())?;

    let mod_id_actor = ctx.mod_id.clone();
    let actors_tbl: Table = wa.get("actors").map_err(|e| e.to_string())?;
    let kind_fn = lua
        .create_function(move |_, kind: String| Ok(kind_id(&mod_id_actor, &kind)))
        .map_err(|e| e.to_string())?;
    actors_tbl.set("kind", kind_fn).map_err(|e| e.to_string())?;
    spawn::bind_world(lua, &actors_tbl, &wa)?;
    Ok(())
}

fn ensure_wa(lua: &Lua) -> Result<Table, String> {
    let globals = lua.globals();
    if let Ok(wa) = globals.get::<Table>("wa") {
        return Ok(wa);
    }
    let wa = lua.create_table().map_err(|e| e.to_string())?;
    wa.set("weapons", lua.create_table().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    wa.set("sprites", lua.create_table().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    wa.set("actors", lua.create_table().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;

    let msg = lua.create_table().map_err(|e| e.to_string())?;
    msg.set("FRAME_FINISH", 2).map_err(|e| e.to_string())?;
    msg.set("EXPLOSION", 28).map_err(|e| e.to_string())?;
    msg.set("JUMP", 0x24).map_err(|e| e.to_string())?;
    msg.set("JUMP_UP", 0x25).map_err(|e| e.to_string())?;
    msg.set("FIRE_WEAPON", 0x26).map_err(|e| e.to_string())?;
    msg.set("RELEASE_WEAPON", 0x27).map_err(|e| e.to_string())?;
    msg.set("START_TURN", 0x34).map_err(|e| e.to_string())?;
    msg.set("NONLETHAL", 0x3B).map_err(|e| e.to_string())?;
    msg.set("POISON", 0x3E).map_err(|e| e.to_string())?;
    msg.set("HIT", 0x4B).map_err(|e| e.to_string())?;
    msg.set("IMPACT", 0x76).map_err(|e| e.to_string())?;
    wa.set("msg", msg).map_err(|e| e.to_string())?;

    let hooks = lua.create_table().map_err(|e| e.to_string())?;
    for name in [
        "gravity_before",
        "gravity_after",
        "missile_frame",
        "worm_message",
        "hurt",
        "hud",
    ] {
        hooks
            .set(name, lua.create_table().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    wa.set("_hooks", hooks).map_err(|e| e.to_string())?;
    wa.set("_brains", lua.create_table().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    wa.set("_fires", lua.create_table().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    wa.set("_clicks", lua.create_table().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    wa.set("_selects", lua.create_table().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;

    let on = lua.create_table().map_err(|e| e.to_string())?;
    bind_on(lua, &on, "gravity_before")?;
    bind_on(lua, &on, "gravity_after")?;
    bind_on(lua, &on, "missile_frame")?;
    bind_on(lua, &on, "worm_message")?;
    bind_on(lua, &on, "hurt")?;
    bind_on(lua, &on, "hud")?;
    wa.set("on", on).map_err(|e| e.to_string())?;

    let hud = lua.create_table().map_err(|e| e.to_string())?;
    let line = lua
        .create_function(|_, (text, x, y): (String, Option<i32>, Option<i32>)| {
            dispatch::push_hud_line(HudLine {
                text,
                x: x.unwrap_or(14),
                y: y.unwrap_or(-1),
            });
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    hud.set("line", line).map_err(|e| e.to_string())?;
    wa.set("hud", hud).map_err(|e| e.to_string())?;

    spawn::bind_world(lua, &wa.get("actors").map_err(|e| e.to_string())?, &wa)?;

    globals.set("wa", wa.clone()).map_err(|e| e.to_string())?;
    Ok(wa)
}

fn bind_on(lua: &Lua, on: &Table, name: &'static str) -> Result<(), String> {
    let f = lua
        .create_function(move |lua, cb: Function| {
            let wa: Table = lua.globals().get("wa")?;
            let hooks: Table = wa.get("_hooks")?;
            let list: Table = hooks.get(name)?;
            let n = list.len()?;
            list.set(n + 1, cb)?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    on.set(name, f).map_err(|e| e.to_string())
}

fn make_push(ctx: &BindContext) -> Rc<dyn Fn(OverlayClaim, &str) -> mlua::Result<()>> {
    let allowed_slots = ctx.allowed_slots.clone();
    let allowed_register = ctx.allowed_register.clone();
    let claims = Rc::clone(&ctx.claims);
    let logs_skip = Rc::clone(&ctx.logs);
    Rc::new(move |claim: OverlayClaim, kind: &str| {
        if !allowed_slots.contains(&claim.slot) {
            logs_skip.borrow_mut().push(format!(
                "[{}] skipped {kind} of {} — not declared in mod.toml replace/overlay",
                claim.mod_id,
                claim.slot.name()
            ));
            return Ok(());
        }
        if let Some(id) = &claim.logical_id {
            if !allowed_register.is_empty() && !allowed_register.contains(id) {
                logs_skip.borrow_mut().push(format!(
                    "[{}] skipped register id '{id}' — not declared in mod.toml register",
                    claim.mod_id
                ));
                return Ok(());
            }
        }
        claims.borrow_mut().push(claim);
        Ok(())
    })
}

fn spec_slot(spec: &Table) -> mlua::Result<StockWeapon> {
    let weapon: Option<String> = spec.get("weapon")?;
    let slot: Option<String> = spec.get("slot")?;
    let name = weapon
        .or(slot)
        .ok_or_else(|| mlua::Error::external("replace() needs weapon = \"bazooka\""))?;
    parse_stock_weapon(&name).map_err(mlua::Error::external)
}

fn table_to_claim(
    lua: &Lua,
    mod_id: &str,
    root: &std::path::Path,
    slot: StockWeapon,
    spec: Table,
) -> mlua::Result<OverlayClaim> {
    let logical_id: Option<String> = spec.get("id")?;
    let name: Option<String> = spec.get("name")?;
    let copy_from_name: Option<String> = spec.get("copy_from")?;
    let copy_from = match copy_from_name {
        Some(s) => Some(parse_stock_weapon(&s).map_err(mlua::Error::external)?),
        None => None,
    };
    let actor: Option<String> = spec.get("actor")?;
    let is_weapon = get_bool(&spec, "is_weapon")?.unwrap_or(true);
    let (cursor_on_select, on_select_id) = spawn::parse_select(lua, &spec)?;

    let mut damage = None;
    let mut bounce_pct = None;
    let mut gravity_pct = None;
    let mut wind_pct = None;
    let mut sprite_id = None;
    let mut sprite_sheet: Option<SpriteStrip> = None;
    let mut sprite_columns = 8i32;
    let mut layer: Option<u32> = None;
    let mut depth: Option<u32> = None;
    let mut depth_prefix: Option<String> = None;
    let mut truecolor_prefix: Option<String> = None;
    let mut reserve_prefix: Option<String> = None;

    if let Some(p) = spec.get::<Option<Table>>("params")? {
        damage = get_i32(&p, "damage")?;
        bounce_pct = get_i32(&p, "bounce_pct")?;
        gravity_pct = get_i32(&p, "gravity_pct")?;
        wind_pct = get_i32(&p, "wind_pct")?;
        sprite_columns = get_i32(&p, "sprite_columns")?.unwrap_or(8).max(1);
        layer = get_i32(&p, "layer")?.map(|v| v.clamp(1, 3) as u32);
        depth = get_i32(&p, "depth")?.map(|v| v as u32);
        depth_prefix = get_string(&p, "depth_prefix")?;
        truecolor_prefix = get_string(&p, "truecolor_prefix")?;
        reserve_prefix = get_string(&p, "reserve_prefix")?;
        load_sprite_field(
            root,
            &p,
            "sprite",
            sprite_columns as u32,
            &mut sprite_id,
            &mut sprite_sheet,
            &mut None,
        )?;
    }

    let mut sprite_path = None;
    let (on_click_id, spawn) = spawn::parse_click(lua, &spec)?;
    match spec.get::<Value>("on_click")? {
        Value::Nil | Value::Function(_) => {}
        Value::Table(click) => {
            if let Ok(s) = click.get::<Table>("spawn") {
                load_sprite_field(
                    root,
                    &s,
                    "sprite",
                    sprite_columns as u32,
                    &mut sprite_id,
                    &mut sprite_sheet,
                    &mut sprite_path,
                )?;
            }
        }
        other => {
            return Err(mlua::Error::external(format!(
                "on_click must be a function or table, got {other:?}"
            )));
        }
    }

    load_sprite_field(
        root,
        &spec,
        "sprite",
        sprite_columns as u32,
        &mut sprite_id,
        &mut sprite_sheet,
        &mut sprite_path,
    )?;

    let mut extra_sheets = Vec::new();
    match spec.get::<Value>("sprites")? {
        Value::Nil => {}
        Value::Table(list) => {
            let n = list.len()?;
            for i in 1..=n {
                let rel: String = list.get(i)?;
                extra_sheets.push(load_pack_sheet(root, &rel, sprite_columns as u32)?);
            }
        }
        other => {
            return Err(mlua::Error::external(format!(
                "sprites must be a list of pack paths, got {other:?}"
            )));
        }
    }

    let mut worm_sprites = Vec::new();
    if let Some(poses) = spec.get::<Option<Table>>("worm_sprites")? {
        for pair in poses.pairs::<String, String>() {
            let (name, path) = pair?;
            let id = gfx::stock_worm_sprite_id(&name).ok_or_else(|| {
                mlua::Error::external(format!("unknown stock worm sprite '{name}'"))
            })?;
            worm_sprites.push((id, path.replace('\\', "/")));
        }
        // Lua hash iteration must not change custom sprite slot allocation.
        worm_sprites.sort_by_key(|(id, _)| *id);
        for (_, path) in &worm_sprites {
            let (path, mut sheet) = load_pack_sheet(root, path, 1)?;
            sheet.looping = false; // stock worm animator owns the phase
            extra_sheets.push((path, sheet));
        }
    }

    let mut extra_sounds = Vec::new();
    match spec.get::<Value>("sounds")? {
        Value::Nil => {}
        Value::Table(list) => {
            let n = list.len()?;
            for i in 1..=n {
                let rel: String = list.get(i)?;
                extra_sounds.push(load_pack_sound(root, &rel)?);
            }
        }
        other => {
            return Err(mlua::Error::external(format!(
                "sounds must be a list of pack WAV paths, got {other:?}"
            )));
        }
    }

    let (on_fire_id, fire_spawn) = spawn::parse_fire(lua, &spec)?;
    if let Value::Table(fire) = spec.get::<Value>("on_fire")? {
        if let Ok(s) = fire.get::<Table>("spawn") {
            load_sprite_field(
                root,
                &s,
                "sprite",
                sprite_columns as u32,
                &mut sprite_id,
                &mut sprite_sheet,
                &mut sprite_path,
            )?;
        }
    }

    Ok(OverlayClaim {
        mod_id: mod_id.to_string(),
        logical_id,
        slot,
        params: OverlayParams {
            name,
            damage,
            bounce_pct,
            gravity_pct,
            wind_pct,
            sprite_id,
            sprite_sheet,
            copy_from,
            actor,
            is_weapon,
            cursor_on_select,
            on_select_id,
            spawn,
            on_click_id,
            on_fire_id,
            fire_spawn,
            sprite_path,
            extra_sheets,
            worm_sprites,
            layer,
            depth,
            depth_prefix,
            truecolor_prefix,
            reserve_prefix,
            extra_sounds,
        },
    })
}

fn load_pack_sheet(
    root: &std::path::Path,
    relative: &str,
    _columns: u32,
) -> mlua::Result<(String, SpriteStrip)> {
    if relative.contains("..") {
        return Err(mlua::Error::external(
            "sprite path must stay inside the pack",
        ));
    }
    if !is_sheet_path(relative) && !root.join(relative).is_dir() {
        return Err(mlua::Error::external(format!(
            "sprites entry '{relative}' must be a pack PNG/GIF or frame folder"
        )));
    }
    let path = root.join(relative);
    let sheet = load_sprite_path(&path, 1).map_err(mlua::Error::external)?;
    Ok((relative.replace('\\', "/"), sheet))
}

fn load_pack_sound(
    root: &std::path::Path,
    relative: &str,
) -> mlua::Result<(String, std::path::PathBuf)> {
    if relative.contains("..") {
        return Err(mlua::Error::external(
            "sound path must stay inside the pack",
        ));
    }
    let rel = relative.replace('\\', "/");
    let lower = rel.to_ascii_lowercase();
    if !lower.ends_with(".wav") {
        return Err(mlua::Error::external(format!(
            "sounds entry '{relative}' must be a pack WAV (convert MP3 first)"
        )));
    }
    let path = root.join(&rel);
    if !path.is_file() {
        return Err(mlua::Error::external(format!(
            "sounds entry '{relative}' is missing"
        )));
    }
    Ok((rel, path))
}

fn load_sprite_field(
    root: &std::path::Path,
    table: &Table,
    key: &str,
    columns: u32,
    sprite_id: &mut Option<i32>,
    sprite_sheet: &mut Option<SpriteStrip>,
    sprite_path: &mut Option<String>,
) -> mlua::Result<()> {
    let Some(sprite) = get_string(table, key)? else {
        return Ok(());
    };
    if is_sheet_path(&sprite) {
        if sprite.contains("..") {
            return Err(mlua::Error::external(
                "sprite path must stay inside the pack",
            ));
        }
        let path = root.join(&sprite);
        *sprite_sheet = Some(load_sprite_path(&path, columns).map_err(mlua::Error::external)?);
        *sprite_path = Some(sprite);
    } else {
        *sprite_id = Some(stock_sprite_id(&sprite).ok_or_else(|| {
            mlua::Error::external(format!(
                "unknown sprite '{sprite}' (png/gif path or grenade/bazooka/mine/holy)"
            ))
        })?);
    }
    Ok(())
}
