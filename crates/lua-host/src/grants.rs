//! Semantic acquisitions, independent of WA's materialized ammo rows.
use mlua::{Lua, Table};
use std::{cell::RefCell, sync::Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantItem {
    pub weapon: u32,
    pub amount: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantOrigin {
    pub worm_id: u32,
    pub x: i32,
    pub y: i32,
    pub alliance: Option<u8>,
    pub frame: i64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WeaponGrant {
    pub origin: GrantOrigin,
    pub items: Vec<GrantItem>,
    pub source: String,
}
#[derive(Clone, Debug)]
pub struct WeaponVisual {
    pub sprite: String,
    pub frame: u32,
    pub width: u32,
    pub height: u32,
}
pub struct GrantNative {
    pub origin: fn(u32) -> Option<GrantOrigin>,
    pub frame: fn() -> Option<i64>,
    pub local_alliance: fn(u8) -> bool,
    pub visual: fn(u32) -> Option<WeaponVisual>,
}
static NATIVE: Mutex<GrantNative> = Mutex::new(GrantNative {
    origin: |_| None,
    frame: || None,
    local_alliance: |_| false,
    visual: |_| None,
});
thread_local! { static PENDING: RefCell<Vec<WeaponGrant>> = const { RefCell::new(Vec::new()) }; }
pub fn bind_grant_native(native: GrantNative) {
    *NATIVE.lock().unwrap() = native;
}
pub fn queue_weapon_grant(mut grant: WeaponGrant) {
    grant.items.retain(|i| {
        i.amount != 0
            && weapons::StockWeapon::from_index(i.weapon)
                .is_some_and(|w| w != weapons::StockWeapon::None)
    });
    if !grant.items.is_empty() {
        trace(&format!(
            "collect_viewer grant queued: source={} origin={:?} items={:?}",
            grant.source, grant.origin, grant.items
        ));
        PENDING.with(|q| q.borrow_mut().push(grant));
    }
}
pub(crate) fn reset() {
    PENDING.with(|q| q.borrow_mut().clear());
}
/// Drain before callbacks so a callback never holds the queue's borrow.
pub fn flush_weapon_grants() -> Result<(), String> {
    let grants = PENDING.with(|q| q.replace(Vec::new()));
    let mut error = None;
    for grant in grants {
        trace(&format!(
            "collect_viewer grant dispatch: source={} worm={} coalition={:?} frame={} items={:?}",
            grant.source,
            grant.origin.worm_id,
            grant.origin.alliance,
            grant.origin.frame,
            grant.items
        ));
        if let Err(e) = crate::dispatch::weapon_grant(grant) {
            error.get_or_insert(e);
        }
    }
    error.map_or(Ok(()), Err)
}
fn trace(message: &str) {
    if let Some(log) = crate::actor::native_log() {
        log(message);
    }
}
fn notify(spec: Table, source: &str) -> mlua::Result<()> {
    crate::sim_context::require("weapons.notify_grant")?;
    let worm = spec.get::<mlua::AnyUserData>("worm")?;
    let handle = worm.borrow::<crate::actor::WormHandle>()?.0;
    let items: Table = spec.get("items")?;
    let mut out = Vec::new();
    for item in items.sequence_values::<Table>() {
        let item = item?;
        let name: String = item.get("weapon")?;
        let weapon = weapons::parse_stock_weapon(&name).map_err(mlua::Error::external)?;
        let amount: u32 = item.get("amount")?;
        if weapon == weapons::StockWeapon::None {
            return Err(mlua::Error::external(
                "notify_grant: weapon must be a stock slot",
            ));
        }
        if amount > 0 {
            out.push(GrantItem {
                weapon: weapon as u32,
                amount,
            });
        }
    }
    let origin_fn = NATIVE.lock().map_err(mlua::Error::external)?.origin;
    let Some(mut origin) = origin_fn(handle) else {
        return Ok(());
    };
    if let Some(x) = spec.get::<Option<i32>>("x")? {
        origin.x = x;
    }
    if let Some(y) = spec.get::<Option<i32>>("y")? {
        origin.y = y;
    }
    queue_weapon_grant(WeaponGrant {
        origin,
        items: out,
        source: source.into(),
    });
    Ok(())
}
/// The inventory's own report of a grant. Handed to the inventory code as a
/// local only, never put in `wa`, so no pack can report one in its name: an
/// "inventory" grant is always a real inventory change.
pub(crate) fn inventory_grant(lua: &Lua) -> mlua::Result<mlua::Function> {
    lua.create_function(|_, spec: Table| {
        if crate::sim_context::active() {
            notify(spec, "inventory")
        } else {
            Ok(())
        }
    })
}

pub(crate) fn install(lua: &Lua, wa: &Table) -> Result<(), String> {
    let result = (|| -> mlua::Result<()> {
        let weapons: Table = wa.get("weapons")?;
        weapons.set(
            "notify_grant",
            lua.create_function(|_, spec: Table| notify(spec, "script"))?,
        )?;
        weapons.set(
            "visual",
            lua.create_function(|lua, name: String| {
                let slot = weapons::parse_stock_weapon(&name).map_err(mlua::Error::external)?;
                let f = NATIVE.lock().map_err(mlua::Error::external)?.visual;
                let Some(v) = f(slot as u32) else {
                    return Ok(None::<Table>);
                };
                let t = lua.create_table()?;
                t.set("sprite", v.sprite)?;
                t.set("frame", v.frame)?;
                t.set("width", v.width)?;
                t.set("height", v.height)?;
                t.set("kind", "icon")?;
                Ok(Some(t))
            })?,
        )?;
        let world: Table = wa.get("world")?;
        world.set(
            "frame",
            lua.create_function(|_, ()| {
                let f = NATIVE.lock().map_err(mlua::Error::external)?.frame;
                Ok(f())
            })?,
        )?;
        world.set("is_local_alliance", lua.create_function(|_, alliance: u8| {
            if !crate::dispatch::rendering() || crate::sim_context::active() {
                return Err(mlua::Error::external("world.is_local_alliance: only from draw or HUD callbacks, never simulation"));
            }
            if alliance > 5 { return Err(mlua::Error::external("alliance must be 0..5")); }
            let f = NATIVE.lock().map_err(mlua::Error::external)?.local_alliance; Ok(f(alliance))
        })?)?;
        Ok(())
    })();
    result.map_err(|e| e.to_string())
}
pub(crate) fn event(lua: &Lua, grant: WeaponGrant) -> Result<Table, String> {
    let make = || -> mlua::Result<Table> {
        let t = lua.create_table()?;
        t.set("worm_id", grant.origin.worm_id)?;
        t.set("x", grant.origin.x)?;
        t.set("y", grant.origin.y)?;
        t.set("alliance", grant.origin.alliance)?;
        t.set("frame", grant.origin.frame)?;
        t.set("source", grant.source)?;
        let items = lua.create_table()?;
        for (n, item) in grant.items.into_iter().enumerate() {
            let i = lua.create_table()?;
            i.set(
                "weapon",
                weapons::StockWeapon::from_index(item.weapon)
                    .unwrap()
                    .name(),
            )?;
            i.set("amount", item.amount)?;
            items.set(n + 1, i)?;
        }
        t.set("items", items)?;
        Ok(t)
    };
    make().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{
        entity::{read_i32, write_i32},
        offsets::*,
    };
    use std::{collections::HashSet, path::PathBuf, rc::Rc};

    fn origin(handle: u32) -> Option<GrantOrigin> {
        let w = handle as *mut u8;
        Some(GrantOrigin {
            worm_id: unsafe { (read_i32(w, ENT_WORM_TEAM) * 16 + read_i32(w, WORM_INDEX)) as u32 },
            x: unsafe { read_i32(w, ENT_POS_X) },
            y: unsafe { read_i32(w, ENT_POS_Y) },
            alliance: Some(4),
            frame: 42,
        })
    }
    #[test]
    fn inventory_and_explicit_grants_are_queued_complete_batches_with_fixed_origins() {
        let _guard = crate::tests::lock_host();
        reset();
        let previous = std::mem::replace(
            &mut *NATIVE.lock().unwrap(),
            GrantNative {
                origin,
                frame: || Some(42),
                local_alliance: |a| a == 4,
                visual: |_| {
                    Some(WeaponVisual {
                        sprite: "other-pack:preview".into(),
                        frame: 2,
                        width: 24,
                        height: 8,
                    })
                },
            },
        );
        crate::bind_actor_native(crate::ActorNative::default());
        let lua = Lua::new();
        let mut ctx = crate::api::BindContext {
            mod_id: "grants".into(),
            root: PathBuf::new(),
            allowed_slots: HashSet::new(),
            allowed_register: HashSet::new(),
            claims: Rc::new(RefCell::new(Vec::new())),
            logs: Rc::new(RefCell::new(Vec::new())),
            debug_worm_state: false,
        };
        crate::api::install(&lua, &ctx).unwrap();
        let mut a = vec![0u32; 0x500 / 4];
        let mut b = vec![0u32; 0x500 / 4];
        for (buf, team) in [(&mut a, 1), (&mut b, 2)] {
            let w = buf.as_mut_ptr() as *mut u8;
            unsafe {
                write_i32(w, ENT_WORM_TEAM, team);
                write_i32(w, WORM_INDEX, 1);
                write_i32(w, ENT_POS_X, 100 << 16);
                write_i32(w, ENT_POS_Y, 200 << 16);
            }
        }
        lua.globals()
            .set(
                "a",
                lua.create_userdata(crate::actor::WormHandle(a.as_mut_ptr() as u32))
                    .unwrap(),
            )
            .unwrap();
        lua.globals()
            .set(
                "b",
                lua.create_userdata(crate::actor::WormHandle(b.as_mut_ptr() as u32))
                    .unwrap(),
            )
            .unwrap();
        lua.load(r#"
            events = {}
            wa.on.weapon_grant(function(e)
              assert(not wa.world.is_local_alliance or not pcall(wa.world.is_local_alliance,4))
              events[#events+1] = e
            end)
            wa.on.draw(function() assert(wa.world.is_local_alliance(4)); assert(not wa.world.is_local_alliance(1)) end)
            wa.on.hud(function() assert(wa.world.is_local_alliance(4)) end)
            assert(not pcall(wa.world.is_local_alliance,4))
            assert(not pcall(wa.weapons.notify_grant,{worm=b,items={}}))
            local v=wa.weapons.visual('grenade')
            assert(v.sprite=='other-pack:preview' and v.frame==2 and v.width==24 and v.kind=='icon')
            assert(wa.world.frame()==42)
            wa.random=function(lo,hi) return lo end
            inv=wa.inventory.create{id='loot',slots={'grenade','bazooka'}}
            -- The inventory's stores and its grant report are its own locals.
            assert(wa._inventory_grant==nil and wa._inventory_stores==nil)
        "#).exec().unwrap();
        crate::sim_context::with_sim(|| {
            lua.load(
                r#"
            inv:deal(a,{'grenade'},3) -- setup is silent
            inv:apply(a,{each=1}) -- materialization is silent
            a:ammo('grenade',{set=5}) -- refills, refunds and direct writes are silent
            inv:add(a,'bazooka',2)
            inv:set(a,'bazooka',4) -- only the positive delta
            inv:set(a,'bazooka',4)
            inv:remove(a,'bazooka',1)
            for _, bad in ipairs({0.5, 'bad', 0x80000000, -0x80000001}) do
              assert(not pcall(inv.transfer,inv,a,b,{x=bad}))
              assert(not pcall(inv.transfer,inv,a,b,{y=bad}))
              assert(inv:get(a,'grenade')==3 and inv:get(b,'grenade')==0)
            end
            inv:transfer(a,b,{mode='copy',amount=1,x=321*0x10000,y=-12*0x10000})
            inv:transfer(a,b,{mode='move'}) -- one batch, three of each, not one usable shot
            inv:apply(b,{each=1})
            wa.weapons.notify_grant{worm=b,items={{weapon='grenade',amount=2}},x=123,y=456}
            assert(#events==0)
        "#,
            )
            .exec()
        })
        .unwrap();
        unsafe {
            write_i32(b.as_mut_ptr() as *mut u8, ENT_POS_X, 300 << 16);
        }
        // Loading a later pack rebinds world; the shared verbs must survive.
        ctx.mod_id = "grants.other".into();
        crate::api::install(&lua, &ctx).unwrap();
        lua.load("assert(wa.world.frame()==42 and wa.weapons.visual('grenade').frame==2)")
            .exec()
            .unwrap();
        crate::park_lua(lua);
        flush_weapon_grants().unwrap();
        crate::PARKED_LUA.with(|slot| {
            slot.borrow()
                .as_ref()
                .unwrap()
                .load(
                    r#"
            assert(#events==5)
            assert(events[1].items[1].amount==2 and events[2].items[1].amount==2)
            local pickup=events[3]
            assert(pickup.source=='inventory' and pickup.worm_id==33 and pickup.alliance==4)
            assert(pickup.x==321*0x10000 and pickup.y==-12*0x10000)
            assert(#pickup.items==2 and pickup.items[1].amount==1 and pickup.items[2].amount==1)
            local e=events[4]
            assert(e.source=='inventory' and e.worm_id==33 and e.frame==42 and e.alliance==4)
            assert(e.x==100*0x10000 and e.y==200*0x10000)
            assert(#e.items==2 and e.items[1].weapon=='grenade' and e.items[1].amount==3)
            assert(e.items[2].weapon=='bazooka' and e.items[2].amount==3)
            assert(events[5].source=='script' and events[5].x==123 and events[5].y==456)
        "#,
                )
                .exec()
                .unwrap()
        });
        crate::run_draw().unwrap();
        crate::run_hud().unwrap();
        flush_weapon_grants().unwrap(); // no duplicates
        crate::PARKED_LUA.with(|slot| {
            *slot.borrow_mut() = None;
        });
        bind_grant_native(previous);
        reset();
    }

    #[test]
    fn empty_health_destroyed_and_removed_batches_stay_silent() {
        reset();
        for items in [
            vec![],
            vec![GrantItem {
                weapon: 6,
                amount: 0,
            }],
            vec![GrantItem {
                weapon: 0,
                amount: 2,
            }],
        ] {
            queue_weapon_grant(WeaponGrant {
                origin: GrantOrigin {
                    worm_id: 17,
                    x: 1,
                    y: 2,
                    alliance: Some(4),
                    frame: 5,
                },
                items,
                source: "crate".into(),
            });
        }
        assert!(PENDING.with(|q| q.borrow().is_empty()));
    }
}
