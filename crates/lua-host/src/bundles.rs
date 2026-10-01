//! Running the selected components of a resolved profile.
//!
//! Two passes, so nothing runs before the whole selection is known to be valid:
//!
//! 1. **Declaration.** Each selected component's file runs once, as a module of
//!    its bundle (its own environment, the bundle's `require`), with `wa`
//!    unavailable: it may only return data. A weapon returns
//!    `{ defaults = {...}, install = function(ctx) ... end }`, a scheme
//!    `{ weapons = { [id] = overrides }, install = ... }`, a feature its install
//!    function (or `{ install = ... }`). The data is checked against
//!    [`WEAPON_DATA`], and each placed weapon's data is the scheme's overrides
//!    laid over the weapon's defaults, field by field: an omitted field
//!    inherits, an explicit 0 is a value.
//! 2. **Install.** In the profile's load order (weapons by slot, the scheme,
//!    the features), each `install(ctx)` runs with `wa` bound to that
//!    component, as a pack's entry is: its log lines and claims carry its id,
//!    and a weapon may claim only the stock slot it was placed on.
//!
//! Components that are not selected never run: they register no claims or
//! callbacks and load no sprites.

use crate::api::{self, BindContext};
use crate::modules;
use mlua::{Function, Lua, Table, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use weapons::components::{row_name, ResolvedProfile, Selected};
use weapons::{OverlayClaim, StockWeapon};

/// What to run: the resolved selection and where each bundle is.
#[derive(Debug, Clone)]
pub struct ComponentPlan {
    pub resolved: ResolvedProfile,
    /// Bundle id -> its folder.
    pub roots: BTreeMap<String, PathBuf>,
}

/// One field of a weapon's data.
enum Field {
    Int { min: i64, max: i64 },
    Bool,
    Choice(&'static [&'static str]),
    Table(&'static [(&'static str, Field)]),
}

const TURN_START: &[(&str, Field)] = &[
    ("amount", Field::Int { min: -1, max: 99 }),
    ("mode", Field::Choice(&["add", "replace"])),
];
const AMMO: &[(&str, Field)] = &[
    // -1 is infinite, as in the ammo verbs.
    ("round_start", Field::Int { min: -1, max: 99 }),
    ("turn_start", Field::Table(TURN_START)),
    ("crate_collect", Field::Int { min: -1, max: 99 }),
];
const CRATE: &[(&str, Field)] = &[
    ("chance_pct", Field::Int { min: 0, max: 100 }),
    ("type", Field::Choice(&["weapon", "utility", "health", "custom"])),
];
/// The weapon data a component may declare and a scheme may override. Not
/// yet written into WA's scheme: install(ctx) receives it as `ctx.data`.
const WEAPON_DATA: &[(&str, Field)] = &[
    ("ammo", Field::Table(AMMO)),
    ("delay", Field::Int { min: 0, max: 99 }),
    ("crate", Field::Table(CRATE)),
    ("ends_turn", Field::Bool),
    ("power", Field::Int { min: 0, max: 100 }),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Weapon,
    Scheme,
    Feature,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Weapon => "weapon",
            Kind::Scheme => "scheme",
            Kind::Feature => "feature",
        }
    }
}

struct Declared {
    kind: Kind,
    component: Selected,
    slot: Option<StockWeapon>,
    install: Option<Function>,
    /// Weapons: defaults. Schemes: overrides by weapon id.
    data: Option<Table>,
}

/// Run `plan`'s components: declare, check, merge, then install.
pub(crate) fn run(
    lua: &Lua,
    plan: &ComponentPlan,
    claims: &Rc<RefCell<Vec<OverlayClaim>>>,
    logs: &Rc<RefCell<Vec<String>>>,
) -> Result<(), String> {
    let r = &plan.resolved;
    let mut envs: BTreeMap<String, Table> = BTreeMap::new();
    for (bundle, root) in &plan.roots {
        let env = modules::bundle_env(lua, bundle, root).map_err(|e| e.to_string())?;
        env.raw_set("wa", wa_while_declaring(lua).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        envs.insert(bundle.clone(), env);
    }

    let mut order: Vec<(Kind, &Selected, Option<StockWeapon>)> = Vec::new();
    order.extend(r.weapons.iter().map(|w| (Kind::Weapon, &w.component, Some(w.slot))));
    order.extend(r.scheme.iter().map(|s| (Kind::Scheme, s, None)));
    order.extend(r.features.iter().map(|f| (Kind::Feature, f, None)));

    // 1. Declaration.
    let mut errors = Vec::new();
    let mut declared = Vec::new();
    for (kind, c, slot) in order {
        match declare(lua, &envs, kind, c) {
            Ok((install, data)) => declared.push(Declared { kind, component: c.clone(), slot, install, data }),
            Err(e) => errors.push(format!("{} {}: {e}", kind.name(), c.id)),
        }
    }
    for env in envs.values() {
        env.raw_set("wa", Value::Nil).map_err(|e| e.to_string())?;
    }
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }

    // 2. Weapon data: defaults, then the scheme's overrides.
    let overrides = declared.iter().find(|d| d.kind == Kind::Scheme).and_then(|d| d.data.clone());
    let placed: HashSet<&str> = r.weapons.iter().map(|w| w.component.id.as_str()).collect();
    if let Some(over) = &overrides {
        for pair in over.pairs::<String, Value>() {
            let (id, _) = pair.map_err(|e| e.to_string())?;
            if !placed.contains(id.as_str()) {
                log(logs, &format!("scheme override for {id} skipped: not in the loadout"));
            }
        }
    }
    let mut data: BTreeMap<String, Table> = BTreeMap::new();
    for d in declared.iter().filter(|d| d.kind == Kind::Weapon) {
        let id = &d.component.id;
        let over = match &overrides {
            Some(t) => t.get::<Option<Table>>(id.as_str()).map_err(|e| format!("scheme override for {id}: {e}"))?,
            None => None,
        };
        let merged = merge(lua, d.data.as_ref(), over.as_ref(), WEAPON_DATA).map_err(|e| e.to_string())?;
        data.insert(id.clone(), merged);
    }

    // 3. Install, in load order.
    let weapons_table = weapons_table(lua, r, &data).map_err(|e| e.to_string())?;
    for d in &declared {
        let ctx = BindContext {
            mod_id: d.component.id.clone(),
            root: plan.roots[&d.component.bundle].clone(),
            allowed_slots: d.slot.into_iter().collect(),
            allowed_register: HashSet::new(),
            claims: Rc::clone(claims),
            logs: Rc::clone(logs),
        };
        api::install(lua, &ctx)?;
        let Some(install) = &d.install else {
            continue;
        };
        let ctx_table = context(lua, r, d, &data, &weapons_table).map_err(|e| e.to_string())?;
        install
            .call::<()>(ctx_table)
            .map_err(|e| format!("{} {}: install: {e}", d.kind.name(), d.component.id))?;
    }
    Ok(())
}

/// A `wa` that refuses every use: the declaration pass only returns data.
fn wa_while_declaring(lua: &Lua) -> mlua::Result<Table> {
    let stub = lua.create_table()?;
    let meta = lua.create_table()?;
    meta.raw_set(
        "__index",
        lua.create_function(|_, (_, key): (Value, Value)| -> mlua::Result<Value> {
            Err(mlua::Error::external(format!(
                "wa.{} while declaring: a component's file only returns its data; use wa in install(ctx)",
                key.to_string().unwrap_or_default()
            )))
        })?,
    )?;
    stub.set_metatable(Some(meta));
    Ok(stub)
}

/// Run a component's file and check what it returned.
fn declare(
    lua: &Lua,
    envs: &BTreeMap<String, Table>,
    kind: Kind,
    c: &Selected,
) -> Result<(Option<Function>, Option<Table>), String> {
    let env = envs.get(&c.bundle).ok_or_else(|| format!("bundle {} is not installed", c.bundle))?;
    let require: Function = env.raw_get("require").map_err(|e| e.to_string())?;
    let module = c.entry.strip_suffix(".lua").unwrap_or(&c.entry);
    let value: Value = require.call(module).map_err(|e| e.to_string())?;
    let _ = lua;
    match (kind, value) {
        (Kind::Feature, Value::Function(f)) => Ok((Some(f), None)),
        (_, Value::Table(t)) => {
            let allowed: &[&str] = match kind {
                Kind::Weapon => &["defaults", "install"],
                Kind::Scheme => &["weapons", "install"],
                Kind::Feature => &["install"],
            };
            for pair in t.pairs::<Value, Value>() {
                let (k, _) = pair.map_err(|e| e.to_string())?;
                let name = k.to_string().unwrap_or_default();
                if !matches!(k, Value::String(_)) || !allowed.contains(&name.as_str()) {
                    return Err(format!("returns {name}: a {} returns only {}", kind.name(), allowed.join(" and ")));
                }
            }
            let install: Option<Function> = t.get("install").map_err(|_| "install must be a function".to_string())?;
            let data = match kind {
                Kind::Weapon => {
                    let defaults: Option<Table> = t.get("defaults").map_err(|_| "defaults must be a table".to_string())?;
                    if let Some(d) = &defaults {
                        check(d, WEAPON_DATA, "defaults")?;
                    }
                    defaults
                }
                Kind::Scheme => {
                    let weapons: Option<Table> = t.get("weapons").map_err(|_| "weapons must be a table".to_string())?;
                    if let Some(w) = &weapons {
                        for pair in w.pairs::<Value, Value>() {
                            let (id, over) = pair.map_err(|e| e.to_string())?;
                            let id = match id {
                                Value::String(s) => s.to_str().map_err(|e| e.to_string())?.to_string(),
                                other => return Err(format!("weapons key {other:?}: use component ids")),
                            };
                            let Value::Table(over) = over else {
                                return Err(format!("weapons.{id} must be a table"));
                            };
                            check(&over, WEAPON_DATA, &format!("weapons.{id}"))?;
                        }
                    }
                    weapons
                }
                Kind::Feature => None,
            };
            Ok((install, data))
        }
        (_, Value::Boolean(true)) => Err("returns nothing; it must return its data".into()),
        (_, other) => Err(format!("returns a {}; expected a table", other.type_name())),
    }
}

/// Check `t` against `schema`, naming the first wrong field by its path.
fn check(t: &Table, schema: &[(&str, Field)], path: &str) -> Result<(), String> {
    for pair in t.pairs::<Value, Value>() {
        let (k, v) = pair.map_err(|e| e.to_string())?;
        let name = match &k {
            Value::String(s) => s.to_str().map_err(|e| e.to_string())?.to_string(),
            other => return Err(format!("{path}: key {other:?} is not a field name")),
        };
        let at = format!("{path}.{name}");
        let Some((_, field)) = schema.iter().find(|(n, _)| *n == name) else {
            let known: Vec<&str> = schema.iter().map(|(n, _)| *n).collect();
            return Err(format!("{at} is not a field; use {}", known.join(", ")));
        };
        match (field, &v) {
            (Field::Int { min, max }, Value::Integer(i)) => {
                if i < min || i > max {
                    return Err(format!("{at} = {i} is outside {min}..={max}"));
                }
            }
            (Field::Int { .. }, _) => return Err(format!("{at} must be a whole number")),
            (Field::Bool, Value::Boolean(_)) => {}
            (Field::Bool, _) => return Err(format!("{at} must be true or false")),
            (Field::Choice(choices), Value::String(s)) => {
                let s = s.to_str().map_err(|e| e.to_string())?;
                if !choices.contains(&&*s) {
                    return Err(format!("{at} = {s:?}: use {}", choices.join(", ")));
                }
            }
            (Field::Choice(choices), _) => return Err(format!("{at} must be one of {}", choices.join(", "))),
            (Field::Table(inner), Value::Table(sub)) => check(sub, inner, &at)?,
            (Field::Table(_), _) => return Err(format!("{at} must be a table")),
        }
    }
    Ok(())
}

/// `over`'s fields over `base`'s, recursively, as a new table: what the scheme
/// sets wins, what it leaves out is the weapon's.
fn merge(lua: &Lua, base: Option<&Table>, over: Option<&Table>, schema: &[(&str, Field)]) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    for (name, field) in schema {
        let b: Value = base.map(|t| t.raw_get(*name)).transpose()?.unwrap_or(Value::Nil);
        let o: Value = over.map(|t| t.raw_get(*name)).transpose()?.unwrap_or(Value::Nil);
        let value = match field {
            Field::Table(inner) => {
                let bt = match &b { Value::Table(t) => Some(t), _ => None };
                let ot = match &o { Value::Table(t) => Some(t), _ => None };
                if bt.is_none() && ot.is_none() {
                    Value::Nil
                } else {
                    Value::Table(merge(lua, bt, ot, inner)?)
                }
            }
            _ if !o.is_nil() => o,
            _ => b,
        };
        if !value.is_nil() {
            out.raw_set(*name, value)?;
        }
    }
    Ok(out)
}

/// Every placed weapon, for a scheme's ctx: id -> { stock_slot, position, data }.
fn weapons_table(lua: &Lua, r: &ResolvedProfile, data: &BTreeMap<String, Table>) -> mlua::Result<Table> {
    let all = lua.create_table()?;
    for w in &r.weapons {
        let entry = lua.create_table()?;
        entry.set("stock_slot", w.slot.name())?;
        entry.set("position", position(lua, w.position)?)?;
        entry.set("data", data[&w.component.id].clone())?;
        all.set(w.component.id.as_str(), entry)?;
    }
    Ok(all)
}

fn position(lua: &Lua, p: weapons::panel::PanelPosition) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("row", row_name(p.row))?;
    t.set("column", p.column)?;
    Ok(t)
}

/// The `ctx` an install receives.
fn context(
    lua: &Lua,
    r: &ResolvedProfile,
    d: &Declared,
    data: &BTreeMap<String, Table>,
    weapons: &Table,
) -> mlua::Result<Table> {
    let ctx = lua.create_table()?;
    ctx.set("id", d.component.id.as_str())?;
    ctx.set("bundle", d.component.bundle.as_str())?;
    ctx.set("type", d.kind.name())?;
    let chosen: HashSet<String> = r
        .weapons
        .iter()
        .map(|w| w.component.id.clone())
        .chain(r.scheme.iter().map(|s| s.id.clone()))
        .chain(r.features.iter().map(|f| f.id.clone()))
        .collect();
    ctx.set("selected", lua.create_function(move |_, id: String| Ok(chosen.contains(&id)))?)?;
    ctx.set("weapons", weapons.clone())?;
    match d.kind {
        Kind::Weapon => {
            let w = r.weapons.iter().find(|w| w.component.id == d.component.id).expect("placed");
            ctx.set("stock_slot", w.slot.name())?;
            ctx.set("position", position(lua, w.position)?)?;
            ctx.set("data", data[&d.component.id].clone())?;
        }
        Kind::Feature => {
            let values = lua.create_table()?;
            for (key, value) in r.options.get(&d.component.id).into_iter().flatten() {
                let v = match value {
                    weapons::components::OptionValue::Bool(b) => Value::Boolean(*b),
                    weapons::components::OptionValue::Int(i) => Value::Integer(*i),
                    weapons::components::OptionValue::Choice(c) => Value::String(lua.create_string(c)?),
                };
                values.set(key.as_str(), v)?;
            }
            let id = d.component.id.clone();
            let lookup = values.clone();
            ctx.set(
                "option",
                lua.create_function(move |_, key: String| {
                    let v: Value = lookup.raw_get(key.as_str())?;
                    if v.is_nil() {
                        return Err(mlua::Error::external(format!("{id} has no option {key}")));
                    }
                    Ok(v)
                })?,
            )?;
            ctx.set("options", values)?;
        }
        Kind::Scheme => {}
    }
    Ok(ctx)
}

fn log(logs: &Rc<RefCell<Vec<String>>>, line: &str) {
    let line = format!("[components] {line}");
    if let Some(write) = crate::actor::native_log() {
        write(&line);
    }
    logs.borrow_mut().push(line);
}
