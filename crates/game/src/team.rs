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

use crate::offsets::{MAX_TEAMS, MAX_WORMS_PER_TEAM};
use core::ops::RangeInclusive;

/// Every possible team number: 1 to 6.
pub fn team_numbers() -> RangeInclusive<u32> {
    1..=MAX_TEAMS
}

/// Every possible worm number within a team: 1 to 8.
pub fn worm_numbers() -> RangeInclusive<u32> {
    1..=MAX_WORMS_PER_TEAM
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
}
