//! The selected components' weapon data, written into WA's scheme for the
//! round.
//!
//! WA turns the lobby's scheme into two places long before the world exists
//! (`GameInfo__ConvertScheme`, 0x45D640): GameInfo's per-alliance ammo and
//! delay rows, and the bytes `InitWeaponTable` lays over the weapon table.
//! Both are written here from the `INIT_WEAPON_TABLE` hook, after WA's
//! overlay and before `GameWorld__InitTeamsFromSetup` copies the rows into
//! the team arena, so the round starts with the component's values and every
//! machine (and every replay playback) writes the same ones.

use crate::log_line;
use game::offsets::{
    GAME_INFO_ALLIANCE_COUNT, GAME_INFO_WEAPON_ROWS, GAME_INFO_WEAPON_ROW_DELAY,
    GAME_INFO_WEAPON_ROW_STRIDE, GAME_WORLD_GAME_INFO, MAX_TEAMS, STOCK_WEAPON_COUNT,
    WEAPON_AVAILABILITY, WEAPON_ENDS_TURN,
};
use game::weapon::WeaponEntry;
use lua_host::WeaponSettings;

/// Write `settings` into the weapon table and GameInfo's ammo rows.
///
/// # Safety
/// `world` is the live GameWorld and `table` its weapon table, during
/// `InitWeaponTable`.
pub unsafe fn apply(world: *mut u8, table: *mut WeaponEntry, settings: &[WeaponSettings]) {
    if settings.is_empty() || world.is_null() || table.is_null() {
        return;
    }
    let game_info = *(world.add(GAME_WORLD_GAME_INFO) as *const *mut u8);
    if game_info.is_null() {
        crate::log::log_error("component weapon data skipped: no GameInfo");
        return;
    }
    let alliances = (*game_info.add(GAME_INFO_ALLIANCE_COUNT) as usize).min(MAX_TEAMS as usize);
    for s in settings {
        let idx = s.slot.index();
        if idx >= STOCK_WEAPON_COUNT {
            continue;
        }
        let entry = &mut *table.add(idx);
        if let Some(weight) = s.crate_weight {
            entry.write_i32(WEAPON_AVAILABILITY, weight);
        }
        if let Some(ends) = s.ends_turn {
            entry.write_i32(WEAPON_ENDS_TURN, ends as i32);
        }
        for a in 0..alliances {
            let row = game_info.add(GAME_INFO_WEAPON_ROWS + a * GAME_INFO_WEAPON_ROW_STRIDE);
            if let Some(ammo) = s.round_start {
                *(row.add(idx * 2) as *mut i16) = ammo as i16;
            }
            if let Some(delay) = s.delay {
                *(row.add(GAME_INFO_WEAPON_ROW_DELAY + idx * 2) as *mut i16) = delay as i16;
            }
        }
        log_line(&format!("{}: {}", s.component, describe(s)));
    }
}

/// The log line for what `s` set on its slot.
fn describe(s: &WeaponSettings) -> String {
    let mut parts = Vec::new();
    match s.round_start {
        Some(-1) => parts.push("ammo infinite".to_string()),
        Some(n) => parts.push(format!("ammo {n}")),
        None => {}
    }
    if let Some(d) = s.delay {
        parts.push(format!("delay {d}"));
    }
    if let Some(w) = s.crate_weight {
        parts.push(format!("crate weight {w}"));
    }
    if let Some(e) = s.ends_turn {
        parts.push(format!("ends turn {e}"));
    }
    if parts.is_empty() {
        parts.push("scheme values kept".into());
    }
    format!("{} on {}", parts.join(", "), s.slot.name())
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::offsets::{WEAPON_ENTRY_SIZE, WEAPON_TABLE_BYTES};
    use weapons::StockWeapon;

    fn settings(slot: StockWeapon) -> WeaponSettings {
        WeaponSettings {
            component: "t.w".into(),
            slot,
            round_start: Some(-1),
            delay: Some(2),
            crate_weight: Some(60),
            ends_turn: Some(false),
            unapplied: Vec::new(),
        }
    }

    #[test]
    fn settings_land_in_every_alliance_row_and_the_slot_entry() {
        let mut world = vec![0u8; 0x100];
        let mut game_info = vec![0u8; GAME_INFO_WEAPON_ROWS + 7 * GAME_INFO_WEAPON_ROW_STRIDE];
        let mut table = vec![0u8; WEAPON_TABLE_BYTES];
        game_info[GAME_INFO_ALLIANCE_COUNT] = 2;
        let gi = game_info.as_mut_ptr() as u32;
        world[GAME_WORLD_GAME_INFO..GAME_WORLD_GAME_INFO + 4].copy_from_slice(&gi.to_le_bytes());
        let slot = StockWeapon::Bazooka;
        let idx = slot.index();
        // Teleport stays the scheme's: a row other than the setting's slot.
        let teleport = StockWeapon::Teleport.index();
        let row = |a: usize| GAME_INFO_WEAPON_ROWS + a * GAME_INFO_WEAPON_ROW_STRIDE;
        game_info[row(0) + teleport * 2] = 3;
        game_info[row(1) + teleport * 2] = 3;
        // The third alliance doesn't play this round.
        game_info[row(2) + idx * 2] = 5;
        let i16_at = |b: &[u8], o: usize| i16::from_le_bytes([b[o], b[o + 1]]);
        unsafe {
            apply(world.as_mut_ptr(), table.as_mut_ptr() as *mut WeaponEntry, &[settings(slot)]);
        }
        for a in 0..2 {
            assert_eq!(i16_at(&game_info, row(a) + idx * 2), -1);
            assert_eq!(i16_at(&game_info, row(a) + GAME_INFO_WEAPON_ROW_DELAY + idx * 2), 2);
            assert_eq!(i16_at(&game_info, row(a) + teleport * 2), 3);
        }
        assert_eq!(i16_at(&game_info, row(2) + idx * 2), 5);
        let entry = &table[idx * WEAPON_ENTRY_SIZE..(idx + 1) * WEAPON_ENTRY_SIZE];
        let i32_at = |o: usize| i32::from_le_bytes(entry[o..o + 4].try_into().unwrap());
        assert_eq!((i32_at(WEAPON_AVAILABILITY), i32_at(WEAPON_ENDS_TURN)), (60, 0));
    }

    #[test]
    fn unset_fields_keep_the_scheme() {
        let s = WeaponSettings { round_start: None, delay: None, crate_weight: None, ends_turn: None, ..settings(StockWeapon::Bazooka) };
        assert_eq!(describe(&s), "scheme values kept on bazooka");
        assert_eq!(
            describe(&settings(StockWeapon::Bazooka)),
            "ammo infinite, delay 2, crate weight 60, ends turn false on bazooka"
        );
    }
}
