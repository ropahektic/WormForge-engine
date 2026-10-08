//! Real crate awards, scoped to WorldEntity's stock collection operation.
//! WA 3.8.1 evidence and signatures: re/GameWorld.toml / Signatures.toml.
use game::{entity::read_i32, offsets::*};
use lua_host::{GrantItem, GrantNative, GrantOrigin, WeaponGrant};
use std::{
    cell::RefCell,
    ffi::c_void,
    sync::{
        atomic::{AtomicPtr, Ordering},
        Mutex,
    },
};

static PICKUP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static DESCRIBE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ADD: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
#[derive(Default)]
struct Pickup {
    origin: Option<GrantOrigin>,
    crates: Vec<(i32, i32, Vec<GrantItem>)>,
}
thread_local! { static SCOPES: RefCell<Vec<Pickup>> = const { RefCell::new(Vec::new()) }; }
static LAST_ACTIVE: Mutex<Option<u8>> = Mutex::new(None);
thread_local! {
    static PICKUP_LOGGED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static VISIBILITY_LOGGED: RefCell<[Option<(bool, Option<u8>, bool)>; 6]> = const { RefCell::new([None; 6]) };
}

pub fn install() -> Result<(), String> {
    lua_host::bind_grant_native(GrantNative {
        origin,
        frame: crate::world::sim_frame,
        local_alliance,
        visual: crate::weapon_visual::visual,
    });
    for (name, detour, original) in [
        ("WORLD_COLLECT_CRATE", pickup as *mut c_void, &PICKUP),
        ("CRATE_GET_DESCRIPTION", describe as *mut c_void, &DESCRIBE),
        ("TEAM_ADD_AMMO", ammo_hook as *mut c_void, &ADD),
    ] {
        original.store(
            unsafe { crate::wa::create_hook(crate::wa::address(name) as *mut c_void, detour) }?,
            Ordering::SeqCst,
        );
    }
    crate::log_line("weapon grant: crate collection, description and ammo hooks installed");
    Ok(())
}
pub fn clear() {
    *LAST_ACTIVE.lock().unwrap() = None;
    SCOPES.with(|s| s.borrow_mut().clear());
    PICKUP_LOGGED.with(|s| s.set(false));
    VISIBILITY_LOGGED.with(|s| *s.borrow_mut() = [None; 6]);
}
pub fn active_alliance() -> Option<u8> {
    crate::actor::list_worms_pub(0).into_iter().find_map(|h| {
        let w = h as *mut u8;
        if h != 0 && unsafe { read_i32(w, WORM_TURN_ACTIVE) } != 0 {
            unsafe { game::team::worm_team_colour(w) }
        } else {
            None
        }
    })
}
pub fn remember_active(worm: u32) {
    *LAST_ACTIVE.lock().unwrap() = unsafe { game::team::worm_team_colour(worm as *mut u8) };
}
pub fn origin(worm: u32) -> Option<GrantOrigin> {
    if worm == 0
        || unsafe { read_i32(worm as *mut u8, ENT_CLASS_TYPE) } != CLASS_WORM
        || crate::world::worm_hp(worm) <= 0
    {
        return None;
    }
    let w = worm as *mut u8;
    Some(GrantOrigin {
        worm_id: unsafe { read_i32(w, ENT_WORM_TEAM) as u32 * 16 + read_i32(w, WORM_INDEX) as u32 },
        x: unsafe { read_i32(w, ENT_POS_X) },
        y: unsafe { read_i32(w, ENT_POS_Y) },
        alliance: active_alliance().or_else(|| *LAST_ACTIVE.lock().unwrap()),
        frame: crate::world::sim_frame()?,
    })
}
/// Owner and colour are independent of ammo rows. Keep defeated players'
/// coalition membership by reading lobby team records, not live worms.
pub fn local_alliance(alliance: u8) -> bool {
    if alliance > 5 {
        return false;
    }
    let world = crate::world::session_world();
    if world.is_null() {
        return false;
    }
    let info = unsafe { read_i32(world, GAME_WORLD_GAME_INFO) } as *const u8;
    if info.is_null() {
        return false;
    }
    let local = unsafe { *info.add(GAME_INFO_LOCAL_MACHINE) };
    let records: Vec<(i32, u8)> = (0..MAX_TEAMS as usize)
        .filter_map(|i| {
            let r = unsafe { info.add(GAME_INFO_TEAM_RECORDS + i * GAME_INFO_TEAM_RECORD_STRIDE) };
            // The setup worm count survives deaths; empty lobby records have none.
            if unsafe { *r.add(GAME_INFO_TEAM_WORM_COUNT) } == 0 {
                return None;
            }
            Some((unsafe { *r as i8 as i32 }, unsafe {
                *r.add(GAME_INFO_TEAM_COLOUR)
            }))
        })
        .collect();
    let active = active_alliance().or_else(|| *LAST_ACTIVE.lock().unwrap());
    let online = crate::world::is_networked();
    let result = visible(alliance, local as i32, &records, online, active);
    VISIBILITY_LOGGED.with(|s| {
        let mut s = s.borrow_mut();
        let decision = (online, active, result);
        if s[alliance as usize] != Some(decision) {
            crate::log_line(&format!("weapon grant visibility: coalition={alliance} active={active:?} online={online} local_machine={local} teams={records:?} visible={result}"));
            s[alliance as usize] = Some(decision);
        }
    });
    result
}
fn visible(
    alliance: u8,
    local: i32,
    records: &[(i32, u8)],
    online: bool,
    active: Option<u8>,
) -> bool {
    if !online {
        return active == Some(alliance);
    }
    records
        .iter()
        .any(|&(owner, colour)| owner == local && owner >= 0 && colour == alliance)
}

type PickupFn = unsafe extern "stdcall" fn(*mut u8, u32, u32, *mut u8) -> u32;
unsafe extern "stdcall" fn pickup(worm: *mut u8, team: u32, index: u32, flag: *mut u8) -> u32 {
    // Homing projectiles can collect for a worm that has already died.
    // The native recipient arguments remain valid; crate coordinates replace
    // this placeholder before queuing, so no live worm pointer is needed.
    let snapshot = if game::team::team_numbers().contains(&team)
        && game::team::worm_numbers().contains(&index)
    {
        crate::world::sim_frame().map(|frame| GrantOrigin {
            worm_id: team * 16 + index,
            x: 0,
            y: 0,
            alliance: active_alliance().or_else(|| *LAST_ACTIVE.lock().unwrap()),
            frame,
        })
    } else {
        None
    };
    let first = PICKUP_LOGGED.with(|s| !s.replace(true));
    if first {
        crate::log_line(&format!("weapon grant pickup: first call collector={worm:p} team={team} index={index} origin={snapshot:?}"));
    }
    SCOPES.with(|s| {
        s.borrow_mut().push(Pickup {
            origin: snapshot,
            ..Pickup::default()
        })
    });
    let f: PickupFn = unsafe { std::mem::transmute(PICKUP.load(Ordering::SeqCst)) };
    let result = unsafe { f(worm, team, index, flag) };
    let scope = SCOPES.with(|s| s.borrow_mut().pop()).unwrap();
    if first || result != 0 || !scope.crates.is_empty() {
        crate::log_line(&format!("weapon grant pickup: result={result} team={team} index={index} origin={:?} crates={:?}", scope.origin, scope.crates));
    }
    if let Some(origin) = scope.origin {
        for (x, y, items) in scope.crates {
            lua_host::queue_weapon_grant(WeaponGrant {
                origin: GrantOrigin {
                    x,
                    y,
                    ..origin.clone()
                },
                items,
                source: "crate".into(),
            });
        }
    }
    result
}
type DescribeFn = unsafe extern "thiscall" fn(*mut u8, u32, u32, *mut u8) -> u32;
unsafe extern "thiscall" fn describe(this: *mut u8, msg: u32, size: u32, out: *mut u8) -> u32 {
    let position = if !this.is_null() {
        Some(unsafe { (read_i32(this, ENT_POS_X), read_i32(this, ENT_POS_Y)) })
    } else {
        None
    };
    let f: DescribeFn = unsafe { std::mem::transmute(DESCRIBE.load(Ordering::SeqCst)) };
    let result = unsafe { f(this, msg, size, out) };
    if SCOPES.with(|s| !s.borrow().is_empty()) {
        crate::log_line(&format!("weapon grant crate description: crate={this:p} msg=0x{msg:x} size=0x{size:x} result={result} position={position:?}"));
    }
    if result != 0 && msg == 0x7D8 && size == 0x394 {
        if let Some((x, y)) = position {
            SCOPES.with(|s| {
                if let Some(scope) = s.borrow_mut().last_mut() {
                    scope.crates.push((x, y, Vec::new()));
                }
            });
        }
    }
    result
}
extern "C" fn award(team: u32, amount: i32, slot: u32) {
    if amount <= 0 || slot == 0 || slot >= STOCK_WEAPON_COUNT as u32 {
        return;
    }
    SCOPES.with(|s| {
        if let Some(scope) = s.borrow_mut().last_mut() {
            crate::log_line(&format!("weapon grant award: team={team} weapon={slot} amount={amount} recipient={:?} described_crates={}", scope.origin.as_ref().map(|o| o.worm_id), scope.crates.len()));
            if scope
                .origin
                .as_ref()
                .is_some_and(|o| o.worm_id / 16 == team)
            {
                if let Some((_, _, items)) = scope.crates.last_mut() {
                    items.push(GrantItem {
                        weapon: slot,
                        amount: amount as u32,
                    });
                }
            }
        }
    });
}
/// TeamArena::AddAmmo is EAX=team, EDX=amount, stack=(arena,slot), RET 8.
/// Preserve every register/flag and tail-call the real trampoline.
#[unsafe(naked)]
unsafe extern "C" fn ammo_hook() {
    core::arch::naked_asm!(
        "pushfd", "pushad",
        "push dword ptr [esp + 44]", // slot, original stack +8
        "push dword ptr [esp + 24]", // saved EDX (+20 before first push)
        "push dword ptr [esp + 36]", // saved EAX (+28 before two pushes)
        "call {award}", "add esp, 12", "popad", "popfd",
        "jmp dword ptr [{original}]", award = sym award, original = sym ADD,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    static FORWARDED: Mutex<Vec<u32>> = Mutex::new(Vec::new());
    extern "C" fn probe(registers: *const u32) {
        let r = unsafe { std::slice::from_raw_parts(registers, 12) };
        *FORWARDED.lock().unwrap() = vec![r[7], r[5], r[6], r[8] & 1, r[10], r[11]];
    }
    #[unsafe(naked)]
    unsafe extern "C" fn fake_add_ammo() {
        core::arch::naked_asm!(
            "pushfd", "pushad", "push esp", "call {probe}", "add esp, 4",
            "popad", "popfd", "mov eax, 0x5678", "ret 8", probe = sym probe,
        );
    }
    #[test]
    fn add_ammo_detour_preserves_registers_flags_stack_and_original_return() {
        let previous = ADD.swap(fake_add_ammo as *mut c_void, Ordering::SeqCst);
        SCOPES.with(|s| {
            s.borrow_mut().push(Pickup {
                origin: Some(GrantOrigin {
                    worm_id: 17,
                    x: 0,
                    y: 0,
                    alliance: Some(4),
                    frame: 5,
                }),
                crates: vec![(123, 456, vec![])],
            })
        });
        let result: u32;
        unsafe {
            core::arch::asm!(
                "mov eax, 1", "mov edx, 3", "mov ecx, 0x1234",
                "push 6", "push 0x99", "stc", "call {hook}",
                hook = sym ammo_hook, lateout("eax") result,
                lateout("ecx") _, lateout("edx") _,
            );
        }
        ADD.store(previous, Ordering::SeqCst);
        assert_eq!(result, 0x5678);
        assert_eq!(*FORWARDED.lock().unwrap(), [1, 3, 0x1234, 1, 0x99, 6]);
        let scope = SCOPES.with(|s| s.borrow_mut().pop()).unwrap();
        assert_eq!(
            scope.crates[0].2,
            [GrantItem {
                weapon: 6,
                amount: 3
            }]
        );
    }

    #[test]
    fn membership_uses_owner_and_colour_and_handles_hotseat_and_spectators() {
        let records = [(0, 4), (2, 4), (3, 1), (-1, 2)];
        assert!(visible(4, 0, &records, true, Some(1)));
        assert!(visible(4, 2, &records, true, Some(1)));
        assert!(!visible(4, 3, &records, true, Some(4)));
        assert!(!visible(4, 5, &records, true, Some(4)));
        assert!(!visible(2, 0, &records, true, Some(2)));
        assert!(visible(4, 0, &records, false, Some(4)));
        assert!(!visible(4, 0, &records, false, Some(1)));
        assert!(visible(1, 0, &records, false, Some(1)));
    }
    #[test]
    fn only_actual_awards_in_a_pickup_are_recorded_including_infinite_ammo() {
        award(1, 3, 6);
        SCOPES.with(|s| {
            s.borrow_mut().push(Pickup {
                origin: Some(GrantOrigin {
                    worm_id: 17,
                    x: 0,
                    y: 0,
                    alliance: Some(4),
                    frame: 5,
                }),
                crates: vec![(123, 456, vec![])],
            })
        });
        award(1, 3, 6);
        award(1, -1, 6);
        award(2, 1, 7);
        award(1, 0, 7);
        let scope = SCOPES.with(|s| s.borrow_mut().pop()).unwrap();
        assert_eq!(
            scope.crates[0].2,
            [GrantItem {
                weapon: 6,
                amount: 3
            }]
        );
    }
}
