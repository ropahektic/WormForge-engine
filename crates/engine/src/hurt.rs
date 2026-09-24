//! Lockstep hurt: wrap stock worm damage messages, then original.
//!
//! HP is subtracted inside `WormEntity__HandleMessage` (Ghidra 0x00510B40).
//! Blast 0x1C, hit 0x4B (flag 10 = percent of current HP), nonlethal 0x3B,
//! poison 0x3E. `0x76` does not write HP. `ApplyDamage` is AdvanceWorm.
//! `worm:hurt` posts 0x4B (pending 0x188 / sim HP 0x40ec); 0x44 commits the
//! floating number when the worm is standing.

use crate::world;
use game::entity::read_i32;
use game::offsets::{
    ACTOR_MAGIC, ACTOR_OWNER, BLAST_AMOUNT, BLAST_TEAM, CLASS_WORM, ENT_CLASS_TYPE, ENT_WORM_TEAM,
    HIT_AMOUNT, HIT_FLAG, HIT_PERCENT, HIT_TEAM, LUA_ACTOR_MAGIC, MSG_EXPLOSION, MSG_HIT,
    MSG_NONLETHAL, MSG_POISON, WORM_INDEX, WORM_POISON, WORM_SELECTED_WEAPON, WORM_TURN_ACTIVE,
};
use lua_host::Hurt;
use std::cell::Cell;
use std::sync::Mutex;

#[derive(Clone, Copy)]
struct LastHit {
    worm_id: i32,
    attacker: u32,
    attacker_team: Option<i32>,
    slot: Option<i32>,
    kind: &'static str,
}

static LAST_HITS: Mutex<Vec<LastHit>> = Mutex::new(Vec::new());
static DEAD_WORMS: Mutex<Vec<i32>> = Mutex::new(Vec::new());
thread_local! {
    static SYNTHETIC_HURT: Cell<bool> = const { Cell::new(false) };
}

pub fn reset() {
    if let Ok(mut hits) = LAST_HITS.lock() {
        hits.clear();
    }
    if let Ok(mut dead) = DEAD_WORMS.lock() {
        dead.clear();
    }
}

pub fn synthetic<T>(f: impl FnOnce() -> T) -> T {
    SYNTHETIC_HURT.with(|flag| {
        let previous = flag.replace(true);
        let result = f();
        flag.set(previous);
        result
    })
}

pub fn is_hurt(msg: u32) -> bool {
    matches!(msg, MSG_EXPLOSION | MSG_HIT | MSG_NONLETHAL | MSG_POISON)
}

pub fn kind(msg: u32, data: *mut u8) -> Option<&'static str> {
    match msg {
        MSG_EXPLOSION => Some("blast"),
        MSG_HIT if is_percent(data) => Some("percent"),
        MSG_HIT => Some("hit"),
        MSG_NONLETHAL => Some("nonlethal"),
        MSG_POISON => Some("poison"),
        _ => None,
    }
}

fn is_percent(data: *mut u8) -> bool {
    !data.is_null() && unsafe { read_i32(data, HIT_FLAG) == HIT_PERCENT }
}

/// Lua `pre`: scale `amount` or `eat` (skip stock for this worm). Then original.
/// On `percent`, `amount` is 0–100 (axe stock is 50), not HP.
pub fn before(worm: *mut u8, sender: u32, msg: u32, _size: u32, data: *mut u8) -> bool {
    let Some(hit) = snapshot(worm, sender, msg, data, "pre", 0) else {
        return false;
    };
    match lua_host::hurt(hit) {
        Ok((eat, amount)) => {
            write_amount(worm, msg, data, amount.max(0));
            eat
        }
        Err(_) => false,
    }
}

pub fn after(worm: *mut u8, sender: u32, msg: u32, _size: u32, data: *mut u8, hp_before: i32) {
    let hp = world::worm_hp(worm as u32);
    let lost = (hp_before - hp).max(0);
    let Some(hit) = snapshot(worm, sender, msg, data, "post", lost) else {
        return;
    };
    if lost > 0 {
        remember_hit(&hit);
    }
    let _ = lua_host::hurt(hit);
}

fn snapshot(
    worm: *mut u8,
    sender: u32,
    msg: u32,
    data: *mut u8,
    phase: &'static str,
    lost: i32,
) -> Option<Hurt> {
    let kind = kind(msg, data)?;
    if worm.is_null() {
        return None;
    }
    let amount = read_amount(worm, msg, data)?;
    let from = worm_sender(sender);
    let attacker = attacking_worm(sender);
    let by = attacking_team(msg, data, attacker);
    // A selected slot is reliable only when the damage sender is the worm
    // itself. Delayed missiles and actors may outlive a later weapon switch,
    // so they intentionally expose no weapon rather than a false one.
    let synthetic = SYNTHETIC_HURT.with(Cell::get);
    let slot = if from != 0 && !synthetic {
        Some(unsafe { read_i32(from as *mut u8, WORM_SELECTED_WEAPON) })
    } else {
        None
    };
    Some(Hurt {
        worm: worm as u32,
        from,
        by,
        attacker,
        slot,
        kind,
        amount,
        lost,
        hp: world::worm_hp(worm as u32),
        phase,
    })
}

fn attacking_worm(sender: u32) -> u32 {
    if sender == 0 {
        return 0;
    }
    let p = sender as *mut u8;
    unsafe {
        if read_i32(p, ENT_CLASS_TYPE) == CLASS_WORM {
            return sender;
        }
        if read_i32(p, ACTOR_MAGIC) as u32 == LUA_ACTOR_MAGIC {
            let owner = read_i32(p, ACTOR_OWNER) as u32;
            if owner != 0 && read_i32(owner as *mut u8, ENT_CLASS_TYPE) == CLASS_WORM {
                return owner;
            }
        }
    }
    0
}

fn worm_sender(sender: u32) -> u32 {
    if sender == 0 {
        return 0;
    }
    let p = sender as *mut u8;
    unsafe {
        if read_i32(p, ENT_CLASS_TYPE) == CLASS_WORM {
            sender
        } else {
            0
        }
    }
}

fn attacking_team(msg: u32, data: *mut u8, from: u32) -> Option<i32> {
    match msg {
        MSG_EXPLOSION if !data.is_null() => unsafe { Some(read_i32(data, BLAST_TEAM)) },
        MSG_HIT if !data.is_null() => unsafe { Some(read_i32(data, HIT_TEAM)) },
        _ if from != 0 => unsafe { Some(read_i32(from as *mut u8, ENT_WORM_TEAM)) },
        _ => None,
    }
}

fn stable_worm_id(worm: *mut u8) -> i32 {
    if worm.is_null() {
        return 0;
    }
    unsafe {
        read_i32(worm, ENT_WORM_TEAM)
            .wrapping_mul(16)
            .wrapping_add(read_i32(worm, WORM_INDEX))
    }
}

fn remember_hit(hit: &Hurt) {
    let id = stable_worm_id(hit.worm as *mut u8);
    if id == 0 {
        return;
    }
    if let Ok(mut hits) = LAST_HITS.lock() {
        let next = LastHit {
            worm_id: id,
            attacker: hit.attacker,
            attacker_team: hit.by,
            slot: hit.slot,
            kind: hit.kind,
        };
        if let Some(old) = hits.iter_mut().find(|h| h.worm_id == id) {
            *old = next;
        } else {
            hits.push(next);
        }
    }
}

fn active_worm(victim: *mut u8) -> u32 {
    if !victim.is_null() && unsafe { read_i32(victim, WORM_TURN_ACTIVE) } != 0 {
        return victim as u32;
    }
    for handle in crate::actor::list_worms_pub(0) {
        if handle != 0 && unsafe { read_i32(handle as *mut u8, WORM_TURN_ACTIVE) } != 0 {
            return handle;
        }
    }
    0
}

/// Emit one lockstep death event when a worm first reaches zero HP.
///
/// This runs after the stock FRAME_FINISH handler, where pending WA damage has
/// settled. A positive-HP observation removes the marker so resurrection-like
/// mods can produce a later death event for the same stable worm id.
pub fn observe_death(worm: *mut u8) {
    if worm.is_null() {
        return;
    }
    let id = stable_worm_id(worm);
    let hp = world::worm_hp(worm as u32);
    if hp > 0 {
        // A nonlethal hit must not become the credited cause of an unrelated
        // drowning/fall/poison death many frames later.
        if let Ok(mut hits) = LAST_HITS.lock() {
            hits.retain(|hit| hit.worm_id != id);
        }
    }
    if !mark_death(id, hp) {
        return;
    }
    let hit = LAST_HITS
        .lock()
        .ok()
        .and_then(|hits| hits.iter().find(|h| h.worm_id == id).copied());
    let death = lua_host::Death {
        victim: worm as u32,
        active: active_worm(worm),
        attacker: hit.map(|h| h.attacker).unwrap_or(0),
        attacker_team: hit.and_then(|h| h.attacker_team),
        slot: hit.and_then(|h| h.slot),
        kind: hit.map(|h| h.kind),
    };
    if let Err(e) = lua_host::death(death) {
        crate::log_error(&format!("death: {e}"));
    }
}

fn mark_death(id: i32, hp: i32) -> bool {
    if let Ok(mut dead) = DEAD_WORMS.lock() {
        if hp > 0 {
            dead.retain(|seen| *seen != id);
            false
        } else if dead.contains(&id) {
            false
        } else {
            dead.push(id);
            true
        }
    } else {
        false
    }
}

fn read_amount(worm: *mut u8, msg: u32, data: *mut u8) -> Option<i32> {
    match msg {
        MSG_EXPLOSION if !data.is_null() => unsafe { Some(read_i32(data, BLAST_AMOUNT)) },
        MSG_HIT if !data.is_null() => unsafe { Some(read_i32(data, HIT_AMOUNT)) },
        MSG_NONLETHAL if !data.is_null() => unsafe { Some(read_i32(data, 0)) },
        MSG_POISON if !worm.is_null() => unsafe { Some(read_i32(worm, WORM_POISON)) },
        _ => None,
    }
}

fn write_amount(worm: *mut u8, msg: u32, data: *mut u8, amount: i32) {
    match msg {
        MSG_EXPLOSION if !data.is_null() => unsafe {
            game::entity::write_i32(data, BLAST_AMOUNT, amount);
        },
        MSG_HIT if !data.is_null() => unsafe {
            game::entity::write_i32(data, HIT_AMOUNT, amount);
        },
        MSG_NONLETHAL if !data.is_null() => unsafe {
            game::entity::write_i32(data, 0, amount);
        },
        MSG_POISON if !worm.is_null() => unsafe {
            game::entity::write_i32(worm, WORM_POISON, amount);
        },
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::offsets::MSG_IMPACT;

    #[test]
    fn kinds_are_short_lockstep_names() {
        assert_eq!(kind(MSG_EXPLOSION, std::ptr::null_mut()), Some("blast"));
        assert_eq!(kind(MSG_IMPACT, std::ptr::null_mut()), None);
        assert!(!is_hurt(MSG_IMPACT));
        assert_eq!(kind(MSG_HIT, std::ptr::null_mut()), Some("hit"));
        let mut axe = [0i32; 8];
        axe[0] = HIT_PERCENT;
        axe[5] = 50;
        assert_eq!(kind(MSG_HIT, axe.as_mut_ptr() as *mut u8), Some("percent"));
        assert_eq!(kind(MSG_NONLETHAL, std::ptr::null_mut()), Some("nonlethal"));
        assert_eq!(kind(MSG_POISON, std::ptr::null_mut()), Some("poison"));
        assert_eq!(kind(0x24, std::ptr::null_mut()), None);
    }

    #[test]
    fn death_transition_is_emitted_once_and_can_rearm() {
        reset();
        assert!(!mark_death(16, 100));
        assert!(mark_death(16, 0));
        assert!(!mark_death(16, 0));
        assert!(!mark_death(16, 50));
        assert!(mark_death(16, 0));
        reset();
    }
}
