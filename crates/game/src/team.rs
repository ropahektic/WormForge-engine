//! How WA numbers teams and worms.
//!
//! **Both start at 1.** The first team is team 1 and its first worm is worm 1,
//! in the worm entity (`ENT_WORM_TEAM`, `WORM_INDEX`), in WA's own
//! `TeamArena` lookups, and in wkFOW's team records. The stable worm id
//! `team * 16 + worm` is therefore 17 for the first worm of the first team.
//!
//! Loops over "every team" or "every worm" must use these ranges. Walking
//! `0..MAX_TEAMS` and `0..MAX_WORMS_PER_TEAM` looks right and skips the
//! last of each: `wa.worms.all()` once never listed team 6 or any team's
//! eighth worm, so Highlander never took the scheme's weapons from the cyan
//! team and sentries could not see those worms at all.

use crate::entity::read_i32;
use crate::offsets::{
    ENT_WORLD, ENT_WORM_TEAM, GAME_INFO_TEAM_COLOUR, GAME_INFO_TEAM_RECORDS,
    GAME_INFO_TEAM_RECORD_STRIDE, GAME_WORLD_GAME_INFO, MAX_TEAMS, MAX_WORMS_PER_TEAM,
};
use core::ops::RangeInclusive;

/// Every possible team number: 1 to 6.
pub fn team_numbers() -> RangeInclusive<u32> {
    1..=MAX_TEAMS
}

/// Every possible worm number within a team: 1 to 8.
pub fn worm_numbers() -> RangeInclusive<u32> {
    1..=MAX_WORMS_PER_TEAM
}

/// WA's visible coalition colour, 0 red through 5 cyan. This comes from the
/// lobby's GameInfo team record, not the TeamArena ammo row or player slot.
pub unsafe fn worm_team_colour(worm: *mut u8) -> Option<u8> {
    if worm.is_null() {
        return None;
    }
    let world = unsafe { read_i32(worm, ENT_WORLD) } as *mut u8;
    if world.is_null() {
        return None;
    }
    let info = unsafe { read_i32(world, GAME_WORLD_GAME_INFO) } as *const u8;
    let team = unsafe { read_i32(worm, ENT_WORM_TEAM) };
    unsafe { colour_from_game_info(info, team) }
}

unsafe fn colour_from_game_info(info: *const u8, team: i32) -> Option<u8> {
    if info.is_null() || !(1..=MAX_TEAMS as i32).contains(&team) {
        return None;
    }
    let offset = GAME_INFO_TEAM_RECORDS
        + (team as usize - 1) * GAME_INFO_TEAM_RECORD_STRIDE
        + GAME_INFO_TEAM_COLOUR;
    let colour = unsafe { *info.add(offset) };
    (colour < MAX_TEAMS as u8).then_some(colour)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two cases that were missed.
    #[test]
    fn the_last_team_and_the_last_worm_are_included() {
        assert!(team_numbers().contains(&6));
        assert!(worm_numbers().contains(&8));
    }

    /// And there is no team 0 or worm 0 to find.
    #[test]
    fn numbering_starts_at_one() {
        assert_eq!(*team_numbers().start(), 1);
        assert_eq!(*worm_numbers().start(), 1);
        assert_eq!(team_numbers().count(), 6);
        assert_eq!(worm_numbers().count(), 8);
    }

    /// Matches the ids the Highlander log showed for a real two-team match:
    /// the first team's worms were 17..24 and the second's 33..40.
    #[test]
    fn stable_ids_of_a_real_match() {
        let ids: Vec<u32> = team_numbers()
            .take(2)
            .flat_map(|t| worm_numbers().map(move |w| t * 16 + w))
            .collect();
        assert_eq!(&ids[..8], &[17, 18, 19, 20, 21, 22, 23, 24]);
        assert_eq!(&ids[8..], &[33, 34, 35, 36, 37, 38, 39, 40]);
    }

    #[test]
    fn team_colour_uses_lobby_colour_not_record_order() {
        let mut info = vec![0u8; GAME_INFO_TEAM_RECORDS + 2 * GAME_INFO_TEAM_RECORD_STRIDE];
        info[GAME_INFO_TEAM_RECORDS] = 4; // owner/player slot is separate
        info[GAME_INFO_TEAM_RECORDS + GAME_INFO_TEAM_COLOUR] = 1; // blue
        info[GAME_INFO_TEAM_RECORDS + GAME_INFO_TEAM_RECORD_STRIDE + GAME_INFO_TEAM_COLOUR] = 0;
        assert_eq!(unsafe { colour_from_game_info(info.as_ptr(), 1) }, Some(1));
        assert_eq!(unsafe { colour_from_game_info(info.as_ptr(), 2) }, Some(0));
        assert_eq!(unsafe { colour_from_game_info(info.as_ptr(), 0) }, None);
    }
}
