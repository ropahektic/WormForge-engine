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
    BLAST_AMOUNT, BLAST_TEAM, CLASS_WORM, ENT_CLASS_TYPE, ENT_WORM_TEAM, HIT_AMOUNT, HIT_FLAG,
    HIT_PERCENT, HIT_TEAM, MSG_EXPLOSION, MSG_HIT, MSG_NONLETHAL, MSG_POISON, WORM_POISON,
};
use lua_host::Hurt;

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
    let by = attacking_team(msg, data, from);
    Some(Hurt {
        worm: worm as u32,
        from,
        by,
        kind,
        amount,
        lost,
        hp: world::worm_hp(worm as u32),
        phase,
    })
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
        assert_eq!(
            kind(MSG_HIT, axe.as_mut_ptr() as *mut u8),
            Some("percent")
        );
        assert_eq!(kind(MSG_NONLETHAL, std::ptr::null_mut()), Some("nonlethal"));
        assert_eq!(kind(MSG_POISON, std::ptr::null_mut()), Some("poison"));
        assert_eq!(kind(0x24, std::ptr::null_mut()), None);
    }
}
