use crate::dispatch::{self, HudLine};
use crate::spawn::{self, get_bool, get_i32, get_string};
use actors::kind_id;
use gfx::{is_sheet_path, load_sprite_path, sprite_key, stock_sprite_id, SpriteStrip};
use mlua::{Function, Lua, MultiValue, Table, Value};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use weapons::{
    parse_panel_icon, parse_stock_weapon, HomingMode, OverlayClaim, OverlayParams, PanelIcon,
    StockWeapon,
};

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
            let line = format!("[{mod_id_log}] {msg}");
            // Straight to wkLua.log, so a pack logging from a hook or a brain
            // is visible. The Vec below is only the load-time transcript, and
            // it is snapshotted once after loading -- anything pushed to it
            // later is never read by anyone.
            if let Some(write) = crate::actor::native_log() {
                write(&line);
            }
            logs.borrow_mut().push(line);
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
        let f = lua
            .create_function(|_, spec: Table| {
                let name: String = spec.get("weapon").map_err(|_| {
                    mlua::Error::external("weapons.retarget needs weapon = \"bazooka\"")
                })?;
                let slot = parse_stock_weapon(&name).map_err(mlua::Error::external)?;
                let body_name: String = spec.get("sprite").map_err(|_| {
                    mlua::Error::external("weapons.retarget needs sprite = \"banana\"")
                })?;
                let body = stock_sprite_id(&body_name).ok_or_else(|| {
                    mlua::Error::external(format!("unknown stock sprite '{body_name}'"))
                })?;
                let cluster = match spec.get::<Option<String>>("cluster")? {
                    None => 0,
                    Some(n) => stock_sprite_id(&n).ok_or_else(|| {
                        mlua::Error::external(format!("unknown stock cluster sprite '{n}'"))
                    })?,
                };
                let retarget = crate::actor::native_retarget_weapon_sprites().ok_or_else(|| {
                    mlua::Error::external("weapons.retarget: native not bound")
                })?;
                retarget(slot.index() as u32, body, cluster).map_err(mlua::Error::external)?;
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        weapons.set("retarget", f).map_err(|e| e.to_string())?;
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

/// Build the `wa` table if this Lua state has none yet.
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
        "turn_start",
        "death",
        "hud",
        "draw",
        "contact",
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
    wa.set("_explodes", lua.create_table().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    wa.set("_proximity", lua.create_table().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    wa.set("_places", lua.create_table().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;

    let on = lua.create_table().map_err(|e| e.to_string())?;
    bind_on(lua, &on, "gravity_before")?;
    bind_on(lua, &on, "gravity_after")?;
    bind_on(lua, &on, "missile_frame")?;
    bind_on(lua, &on, "worm_message")?;
    bind_on(lua, &on, "hurt")?;
    bind_on(lua, &on, "turn_start")?;
    bind_on(lua, &on, "death")?;
    bind_on(lua, &on, "hud")?;
    bind_on(lua, &on, "draw")?;
    bind_on(lua, &on, "contact")?;
    wa.set("on", on).map_err(|e| e.to_string())?;

    let draw = lua.create_table().map_err(|e| e.to_string())?;
    draw.set("quad", draw_quad_fn(lua)?).map_err(|e| e.to_string())?;
    draw.set("random", draw_random_fn(lua)?).map_err(|e| e.to_string())?;
    wa.set("draw", draw).map_err(|e| e.to_string())?;

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

    // Lockstep: `pairs()` order over string-keyed tables varies per process
    // (Lua 5.4 randomises its string-hash seed), so packs get `wa.sorted(t)`
    // -- a deterministic key order to iterate instead. Array return, so the
    // call site is `for _, k in ipairs(wa.sorted(t)) do ... t[k] ... end`.
    let sorted = lua
        .create_function(|lua, t: Table| {
            let mut keys: Vec<Value> = t
                .pairs::<Value, Value>()
                .map(|kv| kv.map(|(k, _)| k))
                .collect::<Result<_, _>>()?;
            // Fail before sorting, not during: `sort_by` takes an infallible
            // comparator, and an `unwrap_or(Equal)` there would silently emit
            // an address-influenced order -- the bug this exists to prevent.
            for k in &keys {
                cmp_key(k, k).map_err(mlua::Error::external)?;
            }
            keys.sort_by(|a, b| cmp_key(a, b).unwrap_or(std::cmp::Ordering::Equal));
            let out = lua.create_table()?;
            for (i, k) in keys.into_iter().enumerate() {
                out.set((i + 1) as i64, k)?;
            }
            Ok(out)
        })
        .map_err(|e| e.to_string())?;
    wa.set("sorted", sorted).map_err(|e| e.to_string())?;

    spawn::bind_world(lua, &wa.get("actors").map_err(|e| e.to_string())?, &wa)?;

    globals.set("wa", wa.clone()).map_err(|e| e.to_string())?;
    install_inventory(lua, &wa)?;
    Ok(wa)
}

/// Pack-scoped virtual worm inventories.
///
/// WA's real ammo remains alliance-scoped. These stores are deliberately
/// keyed by stable worm id and only touch the live TeamArena row when
/// `apply()` is called. Keeping the state in this match's Lua interpreter
/// also gives it the same lifetime as pack state and avoids process-global
/// state leaking into the next round.
fn install_inventory(lua: &Lua, wa: &Table) -> Result<(), String> {
    wa.set("inventory", lua.create_table().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    wa.set(
        "_inventory_stores",
        lua.create_table().map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    lua.load(
        r#"
local inventory_api = wa.inventory
local stores = wa._inventory_stores

local function whole_nonnegative(n, what)
  if type(n) ~= "number" or n < 0 or n % 1 ~= 0 then
    error(what .. " must be a non-negative integer", 3)
  end
  return n
end

local function worm_id(worm)
  if worm == nil or type(worm.id) ~= "number" then
    error("inventory expects a worm", 3)
  end
  return worm.id
end

local function requested_weapons(value, slots, allowed)
  if value == nil or value == "all" then
    return slots
  end
  if type(value) == "string" then
    if not allowed[value] then
      error("weapon '" .. value .. "' is outside this inventory", 4)
    end
    return { value }
  end
  if type(value) ~= "table" then
    error("weapons must be \"all\", a weapon name, or an array", 4)
  end
  local out = {}
  for i, name in ipairs(value) do
    if type(name) ~= "string" or not allowed[name] then
      error("invalid inventory weapon at index " .. tostring(i), 4)
    end
    out[#out + 1] = name
  end
  return out
end

function inventory_api.create(spec)
  if type(spec) ~= "table" then
    error("wa.inventory.create expects a table", 2)
  end
  local id = spec.id
  if type(id) ~= "string" or id == "" then
    error("inventory id must be a non-empty string", 2)
  end
  if type(spec.slots) ~= "table" or #spec.slots == 0 then
    error("inventory slots must be a non-empty array", 2)
  end

  local owner = wa._mod or ""
  local key = owner .. "\0" .. id
  if stores[key] ~= nil then
    error("inventory '" .. id .. "' already exists for " .. owner, 2)
  end

  local slots, allowed = {}, {}
  for i, name in ipairs(spec.slots) do
    if type(name) ~= "string" or name == "" then
      error("inventory slot " .. tostring(i) .. " must be a weapon name", 2)
    end
    if allowed[name] then
      error("duplicate inventory slot '" .. name .. "'", 2)
    end
    slots[#slots + 1] = name
    allowed[name] = true
  end

  local values = {}
  local inv = { id = id, owner = owner }

  local function row(worm, create)
    local id = worm_id(worm)
    local r = values[id]
    if r == nil and create then
      r = {}
      values[id] = r
    end
    return r
  end

  local function require_weapon(name)
    if type(name) ~= "string" or not allowed[name] then
      error("weapon '" .. tostring(name) .. "' is outside inventory '" .. id .. "'", 3)
    end
  end

  function inv:get(worm, name)
    require_weapon(name)
    local r = row(worm, false)
    return r and (r[name] or 0) or 0
  end

  function inv:set(worm, name, amount)
    require_weapon(name)
    amount = whole_nonnegative(amount, "inventory amount")
    local r = row(worm, true)
    if amount == 0 then r[name] = nil else r[name] = amount end
    return amount
  end

  function inv:add(worm, name, amount)
    amount = amount == nil and 1 or whole_nonnegative(amount, "inventory amount")
    return self:set(worm, name, self:get(worm, name) + amount)
  end

  function inv:remove(worm, name, amount)
    amount = amount == nil and 1 or whole_nonnegative(amount, "inventory amount")
    return self:set(worm, name, math.max(0, self:get(worm, name) - amount))
  end

  function inv:clear(worm, weapons)
    local r = row(worm, false)
    if r == nil then return end
    for _, name in ipairs(requested_weapons(weapons, slots, allowed)) do
      r[name] = nil
    end
  end

  function inv:list(worm)
    local out, r = {}, row(worm, false)
    if r == nil then return out end
    for _, name in ipairs(slots) do
      for _ = 1, r[name] or 0 do out[#out + 1] = name end
    end
    return out
  end

  function inv:deal(worm, pool, count)
    if type(pool) ~= "table" or #pool == 0 then
      error("inventory deal pool must be a non-empty array", 2)
    end
    count = count == nil and 1 or whole_nonnegative(count, "deal count")
    for i, name in ipairs(pool) do
      if type(name) ~= "string" or not allowed[name] then
        error("invalid deal weapon at index " .. tostring(i), 2)
      end
    end
    local dealt = {}
    for _ = 1, count do
      local name = pool[wa.random(1, #pool)]
      self:add(worm, name, 1)
      dealt[#dealt + 1] = name
    end
    return dealt
  end

  function inv:transfer(from, to, opts)
    opts = opts or {}
    if type(opts) ~= "table" then error("transfer options must be a table", 2) end
    local mode = opts.mode or "move"
    if mode ~= "move" and mode ~= "copy" then
      error("transfer mode must be \"move\" or \"copy\"", 2)
    end
    local limit = opts.amount
    if limit ~= nil then limit = whole_nonnegative(limit, "transfer amount") end
    local moved = {}
    for _, name in ipairs(requested_weapons(opts.weapons, slots, allowed)) do
      local have = self:get(from, name)
      local n = limit == nil and have or math.min(have, limit)
      if n > 0 then
        self:add(to, name, n)
        if mode == "move" then self:remove(from, name, n) end
        moved[name] = n
      end
    end
    return moved
  end

  function inv:apply(worm, opts)
    opts = opts or {}
    if type(opts) ~= "table" then error("apply options must be a table", 2) end
    if opts.clear then
      local seen = {}
      for _, live in ipairs(wa.worms.all()) do
        if not seen[live.alliance] then
          seen[live.alliance] = true
          for _, name in ipairs(slots) do live:ammo(name, { set = 0 }) end
        end
      end
    end
    local each = opts.each
    if each ~= nil then each = whole_nonnegative(each, "apply each") end
    local apply_slots = requested_weapons(opts.weapons, slots, allowed)
    local r = row(worm, false)
    if r ~= nil then
      for _, name in ipairs(apply_slots) do
        local owned = r[name] or 0
        if owned > 0 then worm:ammo(name, { set = each or owned }) end
      end
    end
  end

  stores[key] = inv
  return inv
end
"#,
    )
    .set_name("wa.inventory")
    .exec()
    .map_err(|e| e.to_string())
}

/// Register a `wa.on.<name>` hook, remembering which pack registered it.
///
/// The remembering is the point. `wa.actors.spawn` resolves a relative sprite
/// path against `wa._mod`, and only the weapon-event dispatchers used to set
/// that. A hook registered here used to run with whatever `_mod` the last
/// weapon event left behind, so with several packs installed a path resolved
/// against the wrong pack and the spawn failed with "sprite is not installed"
/// while the sheet sat in the atlas.
///
/// The owner is read from `wa._mod` at registration time -- the same way
/// brains and fire/click specs pick up their pack (`actor.rs`). During a
/// pack's load script `install` has just set `_mod` to that pack, and from a
/// weapon-event context the dispatcher has set it to the running pack, so a
/// hook registered at runtime is attributed correctly too. Wrapping at
/// registration rather than fixing each dispatcher keeps it to one place and
/// covers hooks added later for free.
fn bind_on(lua: &Lua, on: &Table, name: &'static str) -> Result<(), String> {
    let f = lua
        .create_function(move |lua, cb: Function| {
            let wa: Table = lua.globals().get("wa")?;
            let hooks: Table = wa.get("_hooks")?;
            let list: Table = hooks.get(name)?;
            // The pack in whose name this hook is registered. Empty means
            // nobody had set `_mod` yet; such a hook keeps the old
            // stale-field behaviour rather than guessing a pack.
            let owner: String = wa.get("_mod").unwrap_or_default();
            // MultiValue so every hook shape passes through untouched --
            // arguments in, and return values (worm_message's "eat", hurt's
            // rewritten amount) back out.
            let wrapped = lua.create_function(move |lua, args: MultiValue| {
                let wa: Table = lua.globals().get("wa")?;
                wa.set("_mod", owner.clone())?;
                cb.call::<MultiValue>(args)
            })?;
            let n = list.len()?;
            list.set(n + 1, wrapped)?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    on.set(name, f).map_err(|e| e.to_string())
}

/// Total order over Lua keys so `wa.sorted` is deterministic.
///
/// Rank first by a coarse type order both peers compute identically, then
/// within a rank by value: integers numerically, floats by `total_cmp`
/// (NaN sorts after everything, deterministically), strings byte-wise,
/// booleans with false before true. Integers always sort before floats,
/// even across the same numeric value -- `t[1]` and `t[1.0]` are distinct
/// keys in Lua 5.4, so the order must not depend on which one `pairs()`
/// happened to visit first.
///
/// Anything without a same-process-independent order (tables, functions,
/// threads, userdata, lightuserdata, nil) is refused with an error rather
/// than sorted by address: addresses are exactly what made `pairs()` vary,
/// so sorting by them would reintroduce the nondeterminism under a new name.
fn cmp_key(a: &Value, b: &Value) -> Result<std::cmp::Ordering, String> {
    use std::cmp::Ordering;
    fn rank(v: &Value) -> Result<u8, String> {
        match v {
            Value::Integer(_) => Ok(0),
            Value::Number(_) => Ok(1),
            Value::String(_) => Ok(2),
            Value::Boolean(_) => Ok(3),
            other => Err(format!(
                "wa.sorted cannot order {} keys deterministically; use string, integer, float or boolean keys",
                other.type_name()
            )),
        }
    }
    Ok(rank(a)?.cmp(&rank(b)?).then_with(|| match (a, b) {
        (Value::Integer(x), Value::Integer(y)) => x.cmp(y),
        (Value::Number(x), Value::Number(y)) => x.total_cmp(y),
        (Value::String(x), Value::String(y)) => x.as_bytes().as_ref().cmp(y.as_bytes().as_ref()),
        (Value::Boolean(x), Value::Boolean(y)) => x.cmp(y),
        // Different ranks already decided; same-type pairs are covered.
        _ => Ordering::Equal,
    }))
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

/// WA's team colours: alliance 0 (red) to 5 (cyan).
const WORM_SPRITE_TEAMS: usize = 6;

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
    let panel_icon = match get_string(&spec, "panel_icon")?.or(get_string(&spec, "icon")?) {
        Some(s) if s.to_ascii_lowercase().ends_with(".png") => Some(load_panel_icon(root, &s)?),
        Some(s) => Some(PanelIcon::Stock(
            parse_panel_icon(&s).map_err(mlua::Error::external)?,
        )),
        None => None,
    };
    let actor: Option<String> = spec.get("actor")?;
    let is_weapon = get_bool(&spec, "is_weapon")?.unwrap_or(true);
    let eat_fire = get_bool(&spec, "eat_fire")?.unwrap_or(false);
    let fire_repeat = match get_i32(&spec, "fire_repeat")? {
        None => 1,
        Some(n) if (1..=20).contains(&n) => n as u32,
        Some(n) => {
            return Err(mlua::Error::external(format!("fire_repeat is 1 to 20 shots at once; got {n}")))
        }
    };
    // `wep = "hellish.wep"`: a Project X weapon file whose firing the slot
    // takes (weapons::wep). Its name is the default name.
    if copy_from.is_some() && spec.contains_key("wep")? {
        return Err(mlua::Error::external("use copy_from or wep, not both: each sets how the weapon fires"));
    }
    let wep = match get_string(&spec, "wep")? {
        Some(rel) => {
            if rel.contains("..") {
                return Err(mlua::Error::external("wep path must stay inside the pack"));
            }
            let bytes = std::fs::read(root.join(&rel))
                .map_err(|e| mlua::Error::external(format!("wep {rel}: {e}")))?;
            Some(weapons::wep::parse_wep(&rel.replace('\\', "/"), &bytes).map_err(mlua::Error::external)?)
        }
        None => None,
    };

    let (cursor_on_select, on_select_id) = spawn::parse_select(lua, &spec)?;

    let mut damage = None;
    let mut bounce_pct = None;
    let mut gravity_pct = None;
    let mut wind_pct = None;
    let mut sprite_id = None;
    let mut sprite_sheet: Option<SpriteStrip> = None;
    let mut sprite_columns = 8i32;

    if let Some(p) = spec.get::<Option<Table>>("params")? {
        damage = get_i32(&p, "damage")?;
        bounce_pct = get_i32(&p, "bounce_pct")?;
        gravity_pct = get_i32(&p, "gravity_pct")?;
        wind_pct = get_i32(&p, "wind_pct")?;
        sprite_columns = get_i32(&p, "sprite_columns")?.unwrap_or(8).max(1);
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
            )))
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
            )))
        }
    }

    // The mole strike's digging frames: dive, then the three-frame dig
    // cycle, plus `mole` -- WA's name for the dive frame's underwater
    // variant (`Sprite__GetUnderwaterVariant` maps 158 to 663; the dig
    // frames have none). Only these stock sprites can be replaced here.
    const MOLE_DIG_ROTATION: [&str; 5] = ["moledive", "moledig1", "moledig2", "moledig3", "mole"];
    let mut mole_dig_rotation = Vec::new();
    if let Some(frames) = spec.get::<Option<Table>>("mole_dig_rotation")? {
        for pair in frames.pairs::<String, String>() {
            let (name, path) = pair?;
            let name = name.to_ascii_lowercase();
            if !MOLE_DIG_ROTATION.contains(&name.as_str()) {
                return Err(mlua::Error::external(format!(
                    "mole_dig_rotation keys are {}; got {name:?}",
                    MOLE_DIG_ROTATION.join(", ")
                )));
            }
            let id = gfx::stock_sprite_id(&name).expect("mole dig sprites are stock names");
            let (path, sheet) = load_pack_sheet(root, &path, 1)?;
            if !extra_sheets.iter().any(|(existing, _)| existing == &path) {
                extra_sheets.push((path.clone(), sheet));
            }
            mole_dig_rotation.push((id, path));
        }
        // A dive frame with no underwater art of its own sinks as that same
        // frame under PX's sinking tint -- how PX draws every object it owns
        // under water. Without this, WA swapped a sinking custom mole back
        // to its stock underwater mole.
        let dive = gfx::stock_sprite_id("moledive").expect("stock name");
        let underwater = gfx::stock_sprite_id("mole").expect("stock name");
        let dive_path = mole_dig_rotation
            .iter()
            .find(|(id, _)| *id == dive)
            .map(|(_, p)| p.clone());
        if let Some(dive_path) = dive_path {
            if !mole_dig_rotation.iter().any(|(id, _)| *id == underwater) {
                let sheet = extra_sheets
                    .iter()
                    .find(|(p, _)| *p == dive_path)
                    .map(|(_, s)| s.multiply(PX_SINKING_TINT))
                    .expect("the dive sheet was loaded above");
                let key = format!("{dive_path}#underwater");
                extra_sheets.push((key.clone(), sheet));
                mole_dig_rotation.push((underwater, key));
            }
        }
        mole_dig_rotation.sort_by_key(|(id, _)| *id);
    }

    // `sprite_overrides = { wsvbd = "sprites/trail", [466] = "sprites/x" }`:
    // any stock sprite, by Gfx.dir name or id, replaced for the whole match.
    // Project X weapons point their records at stock sprites some library
    // repurposes this way, e.g. the Salvation Army's `wsv*` as trails.
    let mut sprite_overrides = Vec::new();
    if let Some(list) = spec.get::<Option<Table>>("sprite_overrides")? {
        for pair in list.pairs::<Value, String>() {
            let (key, path) = pair?;
            let id = match &key {
                Value::Integer(n) => i32::try_from(*n).ok(),
                Value::String(name) => stock_sprite_id(&name.to_str()?),
                _ => None,
            };
            let id = id.filter(|id| (1..800).contains(id)).ok_or_else(|| {
                mlua::Error::external(format!(
                    "sprite_overrides keys are stock sprite names or ids 1-799; got {key:?}"
                ))
            })?;
            let (path, sheet) = load_pack_sheet(root, &path, 1)?;
            if !extra_sheets.iter().any(|(existing, _)| existing == &path) {
                extra_sheets.push((path.clone(), sheet));
            }
            sprite_overrides.push((id, path));
        }
        // pairs() order is per process; the install order must not be.
        sprite_overrides.sort_by_key(|(id, _)| *id);
    }

    // `weaponlnk = "sprites/equip"`, or one sheet per team colour:
    // `weaponlnk = { red, blue, green, yellow, magenta, cyan }`, indexed by
    // alliance + 1 like the rest of the Lua surface.
    let mut worm_sprites = Vec::new();
    if let Some(poses) = spec.get::<Option<Table>>("worm_sprites")? {
        for pair in poses.pairs::<String, Value>() {
            let (name, value) = pair?;
            let id = gfx::stock_worm_sprite_id(&name).ok_or_else(|| {
                mlua::Error::external(format!("unknown stock worm sprite '{name}'"))
            })?;
            match value {
                Value::String(path) => {
                    worm_sprites.push((id, None, path.to_str()?.replace('\\', "/")));
                }
                Value::Table(teams) => {
                    let n = teams.raw_len();
                    if n == 0 || n > WORM_SPRITE_TEAMS {
                        return Err(mlua::Error::external(format!(
                            "worm_sprites.{name}: a team list holds 1 to {WORM_SPRITE_TEAMS} \
                             paths, one per team colour; got {n}"
                        )));
                    }
                    for i in 1..=n {
                        let path: String = teams.get(i)?;
                        worm_sprites.push((id, Some((i - 1) as u8), path.replace('\\', "/")));
                    }
                }
                other => {
                    return Err(mlua::Error::external(format!(
                        "worm_sprites.{name} must be a path or a list of paths, got {other:?}"
                    )))
                }
            }
        }
        worm_sprites.sort_by_key(|(id, team, _)| (*id, *team));
        for (_, _, path) in &worm_sprites {
            if extra_sheets.iter().any(|(existing, _)| existing == path) {
                continue;
            }
            let (path, mut sheet) = load_pack_sheet(root, path, 1)?;
            sheet.looping = false;
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
            )))
        }
    }

    let mut layer = None;
    let mut depth = None;
    let mut depth_prefix = None;
    let mut gpu_prefix = None;
    let mut truecolor_prefix = None;
    let mut reserve_prefix = None;
    let mut gpu_worm_sprites = true;
    let mut worm_sprites_animate = false;
    if let Some(p) = spec.get::<Option<Table>>("params")? {
        layer = get_i32(&p, "layer")?.map(|v| v as u32);
        depth = get_i32(&p, "depth")?.map(|v| v as u32);
        depth_prefix = get_string(&p, "depth_prefix")?;
        gpu_prefix = get_string(&p, "gpu_prefix")?;
        truecolor_prefix = get_string(&p, "truecolor_prefix")?;
        reserve_prefix = get_string(&p, "reserve_prefix")?;
        gpu_worm_sprites = get_bool(&p, "gpu_worm_sprites")?.unwrap_or(true);
        worm_sprites_animate = get_bool(&p, "worm_sprites_animate")?.unwrap_or(false);
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

    let mut plane_sprite_path = None;
    if let Some(rel) = get_string(&spec, "plane")? {
        let (path, sheet) = load_pack_sheet(root, &rel, 1)?;
        if !extra_sheets.iter().any(|(existing, _)| existing == &path) {
            extra_sheets.push((path.clone(), sheet));
        }
        plane_sprite_path = Some(path);
    }

    let mut cluster_sprite_id = None;
    let mut cluster_sprite_path = None;
    let mut cluster_homing_sprite_path = None;
    let mut cluster_damage = None;
    let mut cluster_bounce_pct = None;
    let mut cluster_gravity_pct = None;
    let mut cluster_wind_pct = None;
    let mut cluster_homing = None;
    if let Some(cluster) = spec.get::<Option<Table>>("cluster")? {
        cluster_damage = get_i32(&cluster, "damage")?;
        cluster_bounce_pct = get_i32(&cluster, "bounce_pct")?;
        cluster_gravity_pct = get_i32(&cluster, "gravity_pct")?;
        cluster_wind_pct = get_i32(&cluster, "wind_pct")?;
        if let Some(h) = get_string(&cluster, "homing")? {
            cluster_homing = HomingMode::parse(&h);
            if cluster_homing.is_none() && h != "off" {
                return Err(mlua::Error::external(format!(
                    "cluster.homing must be lock/avoid/dodge/off, got '{h}'"
                )));
            }
        }
        let mut cluster_sheet = None;
        let mut cluster_path = None;
        load_sprite_field(
            root,
            &cluster,
            "sprite",
            sprite_columns as u32,
            &mut cluster_sprite_id,
            &mut cluster_sheet,
            &mut cluster_path,
        )?;
        if let (Some(path), Some(sheet)) = (cluster_path.take(), cluster_sheet.take()) {
            cluster_sprite_path = Some(path.clone());
            extra_sheets.push((path, sheet));
        }
        if let Some(rel) = get_string(&cluster, "homing_sprite")? {
            let (path, sheet) = load_pack_sheet(root, &rel, 1)?;
            if !extra_sheets.iter().any(|(existing, _)| existing == &path) {
                extra_sheets.push((path.clone(), sheet));
            }
            cluster_homing_sprite_path = Some(path);
        }
        if let Some(p) = cluster.get::<Option<Table>>("params")? {
            if cluster_damage.is_none() {
                cluster_damage = get_i32(&p, "damage")?;
            }
            if cluster_bounce_pct.is_none() {
                cluster_bounce_pct = get_i32(&p, "bounce_pct")?;
            }
            if cluster_gravity_pct.is_none() {
                cluster_gravity_pct = get_i32(&p, "gravity_pct")?;
            }
            if cluster_wind_pct.is_none() {
                cluster_wind_pct = get_i32(&p, "wind_pct")?;
            }
        }
    }

    let mut mine_proximity = None;
    let mut mine_fuse = None;
    let mut mine_mask = None;
    let mut mine_trigger = None;
    let mut mine_blast = None;
    if let Some(mine) = spec.get::<Option<Table>>("mine")? {
        mine_proximity = get_i32(&mine, "proximity")?;
        mine_fuse = get_i32(&mine, "fuse")?;
        mine_mask = get_i32(&mine, "mask")?;
        mine_trigger = get_i32(&mine, "trigger")?;
        mine_blast = get_i32(&mine, "blast")?;
    }

    let (on_explode_id, explode_spawn) =
        spawn::parse_step(lua, &spec, "on_explode", crate::actor::register_explode)?;
    let (on_proximity_id, proximity_spawn) =
        spawn::parse_step(lua, &spec, "on_proximity", crate::actor::register_proximity)?;
    let (on_place_id, place_spawn) =
        spawn::parse_step(lua, &spec, "on_place", crate::actor::register_place)?;
    install_attached_sheet(
        root,
        sprite_columns as u32,
        &explode_spawn,
        &mut extra_sheets,
    )?;
    install_attached_sheet(
        root,
        sprite_columns as u32,
        &proximity_spawn,
        &mut extra_sheets,
    )?;
    install_attached_sheet(root, sprite_columns as u32, &place_spawn, &mut extra_sheets)?;
    if let Some(child) = fire_spawn.as_ref().and_then(|s| s.on_explode.as_ref()) {
        install_attached_sheet(
            root,
            sprite_columns as u32,
            &Some((**child).clone()),
            &mut extra_sheets,
        )?;
    }
    if let Some(child) = spawn.as_ref().and_then(|s| s.on_explode.as_ref()) {
        install_attached_sheet(
            root,
            sprite_columns as u32,
            &Some((**child).clone()),
            &mut extra_sheets,
        )?;
    }

    Ok(OverlayClaim {
        mod_id: mod_id.to_string(),
        logical_id,
        slot,
        params: OverlayParams {
            name: name.or_else(|| wep.as_ref().map(|w| w.name.clone())),
            damage,
            bounce_pct,
            gravity_pct,
            wind_pct,
            sprite_id,
            sprite_sheet,
            copy_from,
            wep,
            actor,
            is_weapon,
            cursor_on_select,
            eat_fire,
            fire_repeat,
            on_select_id,
            spawn,
            on_click_id,
            on_fire_id,
            fire_spawn,
            sprite_path,
            extra_sheets,
            mole_dig_rotation,
            sprite_overrides,
            worm_sprites,
            layer,
            depth,
            depth_prefix,
            reserve_prefix,
            gpu_prefix,
            truecolor_prefix,
            gpu_worm_sprites,
            worm_sprites_animate,
            extra_sounds,
            cluster_sprite_id,
            cluster_sprite_path,
            cluster_homing_sprite_path,
            plane_sprite_path,
            cluster_damage,
            cluster_bounce_pct,
            cluster_gravity_pct,
            cluster_wind_pct,
            cluster_homing,
            panel_icon,
            mine_proximity,
            mine_fuse,
            mine_mask,
            mine_trigger,
            mine_blast,
            on_explode_id,
            explode_spawn,
            on_proximity_id,
            proximity_spawn,
            on_place_id,
            place_spawn,
        },
    })
}

/// PX's colour for anything it draws while it sinks: every PX-scripted
/// object wraps its draw in `SetColorMod(RGB(90, 90, 240), 6)` while
/// `Sinking` (sentry gun, magnet, rock, ...).
const PX_SINKING_TINT: [u8; 3] = [90, 90, 240];

/// `panel_icon = "sprites/icon.png"`: a pack PNG for the weapon panel. Its
/// size is checked against the panel window at install, where the window is
/// read from the live icons; only the path rule is enforced here.
fn load_panel_icon(root: &std::path::Path, relative: &str) -> mlua::Result<PanelIcon> {
    if relative.contains("..") {
        return Err(mlua::Error::external("panel_icon path must stay inside the pack"));
    }
    let bytes = std::fs::read(root.join(relative))
        .map_err(|e| mlua::Error::external(format!("panel_icon {relative}: {e}")))?;
    let (width, height, rgba) = gfx::load_png_rgba(&bytes)
        .map_err(|e| mlua::Error::external(format!("panel_icon {relative}: {e}")))?;
    Ok(PanelIcon::File {
        path: relative.replace('\\', "/"),
        width,
        height,
        rgba,
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

fn install_attached_sheet(
    root: &std::path::Path,
    columns: u32,
    attached: &Option<weapons::AttachedSpawn>,
    extra: &mut Vec<(String, gfx::SpriteStrip)>,
) -> mlua::Result<()> {
    let Some(a) = attached else {
        return Ok(());
    };
    if let Some(path) = &a.path {
        push_extra_sheet(root, columns, path, extra)?;
    }
    if let Some(path) = &a.trail_path {
        push_extra_sheet(root, columns, path, extra)?;
    }
    if let Some(path) = a.spawn.cluster.as_ref().and_then(|c| c.trail_path.as_ref()) {
        push_extra_sheet(root, columns, path, extra)?;
    }
    if let Some(child) = &a.spawn.on_explode {
        install_attached_sheet(root, columns, &Some((**child).clone()), extra)?;
    }
    Ok(())
}

fn push_extra_sheet(
    root: &std::path::Path,
    columns: u32,
    path: &str,
    extra: &mut Vec<(String, gfx::SpriteStrip)>,
) -> mlua::Result<()> {
    if extra.iter().any(|(p, _)| p == path) {
        return Ok(());
    }
    extra.push(load_pack_sheet(root, path, columns)?);
    Ok(())
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
                "unknown sprite '{sprite}' (png/gif path or a Gfx.dir name, e.g. missile/grenade/uzihit)"
            ))
        })?);
    }
    Ok(())
}

/// `wa.draw.quad{ points, color | colors, sprite, frame, blend, depth }`,
/// only inside `wa.on.draw`. `points` are four `{ x, y }` in 16.16 world
/// coordinates, clockwise from the one that takes the sheet's top-left;
/// colours are 0xAARRGGBB, one for all corners or four; `blend` is "alpha"
/// (default) or "add"; `depth` defaults to 0x10000, in front of terrain.
fn draw_quad_fn(lua: &Lua) -> Result<Function, String> {
    lua.create_function(|lua, spec: Table| {
        if !dispatch::drawing() {
            return Err(mlua::Error::external(
                "wa.draw.quad: only inside wa.on.draw",
            ));
        }
        let points: Table = spec.get("points")?;
        let mut corners = [(0f32, 0f32); 4];
        for (i, c) in corners.iter_mut().enumerate() {
            let p: Table = points.get(i + 1).map_err(|_| {
                mlua::Error::external("wa.draw.quad: points needs four { x, y }")
            })?;
            let x: f64 = p.get(1).or_else(|_| p.get("x"))?;
            let y: f64 = p.get(2).or_else(|_| p.get("y"))?;
            *c = ((x / 65536.0) as f32, (y / 65536.0) as f32);
        }
        let colours = match spec.get::<Option<Table>>("colors")? {
            Some(t) => {
                let mut c = [0u32; 4];
                for (i, v) in c.iter_mut().enumerate() {
                    *v = t.get::<i64>(i + 1)? as u32;
                }
                c
            }
            None => [spec.get::<Option<i64>>("color")?.unwrap_or(0xFFFF_FFFF) as u32; 4],
        };
        let name: String = spec.get("sprite")?;
        let (sprite, ..) = crate::spawn::resolve_sprite_name(lua, &name)?;
        let additive = match spec.get::<Option<String>>("blend")?.as_deref() {
            None | Some("alpha") => false,
            Some("add") => true,
            Some(other) => {
                return Err(mlua::Error::external(format!(
                    "wa.draw.quad: blend {other:?}; valid: alpha, add"
                )))
            }
        };
        dispatch::push_draw_quad(dispatch::DrawQuad {
            corners,
            colours,
            sprite,
            frame: spec.get::<Option<u32>>("frame")?.unwrap_or(0),
            additive,
            depth: spec.get::<Option<u32>>("depth")?.unwrap_or(0x1_0000),
        });
        Ok(())
    })
    .map_err(|e| e.to_string())
}

/// `wa.draw.random()`: 0 <= r < 1, for looks only (flicker, jitter). A
/// local generator, never WA's: it differs between peers by design.
fn draw_random_fn(lua: &Lua) -> Result<Function, String> {
    lua.create_function(|_, ()| {
        thread_local! {
            static STATE: std::cell::Cell<u64> = const { std::cell::Cell::new(0x9E37_79B9_7F4A_7C15) };
        }
        let r = STATE.with(|s| {
            // xorshift64*
            let mut x = s.get();
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            s.set(x);
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        });
        Ok((r >> 11) as f64 / (1u64 << 53) as f64)
    })
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(mod_id: &str) -> BindContext {
        BindContext {
            mod_id: mod_id.to_string(),
            root: PathBuf::new(),
            allowed_slots: HashSet::new(),
            allowed_register: HashSet::new(),
            claims: Rc::new(RefCell::new(Vec::new())),
            logs: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// Gap 7: hooks that no weapon dispatcher drives (`hud` here, and the same
    /// path serves worm_message / hurt / gravity_*) used to run with whatever
    /// `wa._mod` the last weapon event left behind. With two packs installed
    /// that resolves spawn sprite paths against the wrong pack. Registration
    /// must remember the owner, and the wrapper must re-establish it.
    #[test]
    fn non_weapon_hooks_run_with_their_own_pack_id() {
        let lua = Lua::new();
        // The load order run_mods produces: install a, run a's script, install
        // b, run b's script. install sets wa._mod, so each registration reads
        // its own pack -- and the wrapper must keep that alive at dispatch.
        install(&lua, &ctx("packa")).unwrap();
        lua.load(
            r#"
            hits = {}
            wa.on.hud(function() hits[#hits + 1] = wa._mod end)  -- pack a's script
        "#,
        )
        .exec()
        .unwrap();
        install(&lua, &ctx("packb")).unwrap();
        lua.load(
            r#"
            wa.on.hud(function() hits[#hits + 1] = wa._mod end)  -- pack b's script
        "#,
        )
        .exec()
        .unwrap();

        // Poison the field the way a pack-a weapon event would have, then
        // dispatch both hooks the way dispatch.rs does: walk the list, call
        // each entry, read back whatever the handler saw.
        lua.load(r#"wa._mod = "packa""#).exec().unwrap();
        let wa: Table = lua.globals().get("wa").unwrap();
        let hooks: Table = wa.get("_hooks").unwrap();
        let list: Table = hooks.get("hud").unwrap();
        for i in 1..=list.len().unwrap() {
            let f: mlua::Function = list.get(i).unwrap();
            f.call::<()>(()).unwrap();
        }

        let hits: Table = lua.globals().get("hits").unwrap();
        assert_eq!(hits.get::<String>(1).unwrap(), "packa");
        assert_eq!(hits.get::<String>(2).unwrap(), "packb");
        assert_eq!(hits.len().unwrap(), 2);
    }

    /// The wrapper must stay transparent: worm_message dispatch reads the
    /// callback's return to spot "eat", hurt's pre phase does the same for a
    /// rewritten amount. Wrap-and-call must hand return values through.
    #[test]
    fn hook_wrapper_passes_return_values_through() {
        let lua = Lua::new();
        install(&lua, &ctx("packa")).unwrap();
        lua.load(
            r#"
            seen = nil
            wa.on.worm_message(function() seen = wa._mod return "eat" end)
        "#,
        )
        .exec()
        .unwrap();
        let wa: Table = lua.globals().get("wa").unwrap();
        let list: Table = wa
            .get::<Table>("_hooks")
            .unwrap()
            .get("worm_message")
            .unwrap();
        let f: mlua::Function = list.get(1).unwrap();
        lua.load(r#"wa._mod = "stale""#).exec().unwrap();
        let ret: String = f
            .call::<String>((lua.create_table().unwrap(), 0x24u32))
            .unwrap();
        assert_eq!(ret, "eat");
        assert_eq!(lua.globals().get::<String>("seen").unwrap(), "packa");
    }

    /// Gap 5 remainder: `pairs()` over string-keyed tables varies per process
    /// (Lua 5.4 randomises its string-hash seed), so packs iterating one with
    /// gameplay effects would desync. `wa.sorted` returns the same key order
    /// on both peers for the orderable key types.
    #[test]
    fn sorted_returns_string_keys_in_byte_order() {
        let lua = Lua::new();
        install(&lua, &ctx("sort")).unwrap();
        let order: Vec<String> = lua
            .load(r#"return wa.sorted({ zebra = 1, apple = 2, Mango = 3 })"#)
            .eval::<Table>()
            .unwrap()
            .sequence_values::<String>()
            .collect::<mlua::Result<_>>()
            .unwrap();
        assert_eq!(order, vec!["Mango", "apple", "zebra"]);
    }

    /// Integers sort among themselves numerically, and always before floats
    /// even for equal numeric values -- `t[1]` and `t[1.0]` are distinct keys
    /// in Lua 5.4, so the order must not depend on visit order. Booleans sort
    /// false before true, after strings.
    #[test]
    fn sorted_orders_mixed_scalar_keys_deterministically() {
        let lua = Lua::new();
        install(&lua, &ctx("sort")).unwrap();
        let order: Vec<String> = lua
            .load(
                r#"
                local t = { [10] = 1, [2] = 2, [1.5] = 3, [1] = 4, b = 5, [true] = 6, [false] = 7 }
                local names = {}
                for _, k in ipairs(wa.sorted(t)) do names[#names + 1] = type(k) .. ":" .. tostring(k) end
                return names
                "#,
            )
            .eval::<Table>()
            .unwrap()
            .sequence_values::<String>()
            .collect::<mlua::Result<_>>()
            .unwrap();
        assert_eq!(
            order,
            vec![
                "number:1",
                "number:2",
                "number:10",
                "number:1.5",
                "string:b",
                "boolean:false",
                "boolean:true",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
        );
    }

    /// Keys with no same-process-independent order are refused: sorting table
    /// keys by address would reintroduce the nondeterminism under a new name.
    #[test]
    fn sorted_refuses_unsortable_keys() {
        let lua = Lua::new();
        install(&lua, &ctx("sort")).unwrap();
        let err = lua
            .load(r#"return wa.sorted({ [{}] = 1 })"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("wa.sorted"), "unexpected error: {err}");
    }

    #[test]
    fn virtual_inventory_is_pack_scoped_and_composes_verbs() {
        let lua = Lua::new();
        install(&lua, &ctx("pack.a")).unwrap();
        lua.load(
            r#"
            local draw = 0
            wa.random = function(lo, hi)
              draw = draw + 1
              return lo + ((draw - 1) % (hi - lo + 1))
            end
            local live = {}
            local function worm(id, alliance)
              local w = { id = id, alliance = alliance, live = {} }
              function w:ammo(name, change)
                if change ~= nil then self.live[name] = change.set end
                return self.live[name] or 0
              end
              return w
            end
            a, b, c = worm(16, 0), worm(32, 1), worm(33, 1)
            wa.worms.all = function() return { a, b, c } end

            inv_a = wa.inventory.create({
              id = "loot", slots = { "bazooka", "grenade", "rope" }
            })
            local dealt = inv_a:deal(a, { "bazooka", "grenade" }, 2)
            assert(dealt[1] == "bazooka" and dealt[2] == "grenade")
            assert(inv_a:get(a, "bazooka") == 1 and inv_a:get(a, "grenade") == 1)
            inv_a:transfer(a, b, {
              weapons = { "bazooka" }, amount = 1, mode = "move"
            })
            assert(inv_a:get(a, "bazooka") == 0 and inv_a:get(b, "bazooka") == 1)
            inv_a:transfer(a, b, { weapons = "grenade", mode = "copy" })
            assert(inv_a:get(a, "grenade") == 1 and inv_a:get(b, "grenade") == 1)
            inv_a:apply(b, { clear = true, each = 1 })
            assert(a.live.bazooka == 0 and a.live.grenade == 0)
            assert(b.live.bazooka == 1 and b.live.grenade == 1)
            -- b/c share an alliance: clear visits that physical row once.
            assert(c.live.bazooka == nil)
        "#,
        )
        .exec()
        .unwrap();

        // The same public id in another pack is a distinct store.
        install(&lua, &ctx("pack.b")).unwrap();
        lua.load(
            r#"
            inv_b = wa.inventory.create({
              id = "loot", slots = { "bazooka", "grenade", "rope" }
            })
            assert(inv_b:get(b, "bazooka") == 0)
            assert(inv_a:get(b, "bazooka") == 1)
        "#,
        )
        .exec()
        .unwrap();
    }
}
