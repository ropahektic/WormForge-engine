//! WA's weapon panel as `(row, column)` positions, one per stock slot.
//!
//! WA's `AssignWeaponPanelSlots` (0x537130) gives every weapon table entry its
//! panel row (entry +0x08): F1..F12 hold five consecutive slots each (F1 =
//! slots 1..=5, F2 = 6..=10, ...), except F5, which holds six (21..=26); slot 0
//! and the utilities (62..=70) get row 0, which has no F key.
//!
//! A position here names exactly one stock slot: the column is the slot's
//! place within its row, in slot order, and the utilities are row `utility`.
//! So F5 has six columns although WA draws Super Sheep (column 4) and Aqua
//! Sheep (column 5) in one cell, whichever the scheme enables. The order of
//! the utilities on screen is not checked; their columns follow slot order.

use crate::StockWeapon;
use serde::de::{self, Deserializer};
use serde::Deserialize;
use std::fmt;

/// A row of the weapon panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PanelRow {
    /// F1..F12.
    F(u8),
    /// WA's row 0: Jet Pack .. Crate Shower.
    Utility,
}

/// One panel position, `{ row = "F1", column = 1 }` in a manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PanelPosition {
    pub row: PanelRow,
    /// 1-based.
    pub column: u8,
}

/// The first slot of each F row, and how many slots it holds.
const F_ROWS: [(u32, u8); 12] = [
    (1, 5), (6, 5), (11, 5), (16, 5), (21, 6), (27, 5),
    (32, 5), (37, 5), (42, 5), (47, 5), (52, 5), (57, 5),
];
const UTILITY_FIRST: u32 = 62;
const UTILITY_COUNT: u8 = 9;

impl PanelPosition {
    /// The stock slot at this position, or None if the panel has no such cell.
    pub fn slot(self) -> Option<StockWeapon> {
        let (first, count) = match self.row {
            PanelRow::F(n) if (1..=12).contains(&n) => F_ROWS[n as usize - 1],
            PanelRow::F(_) => return None,
            PanelRow::Utility => (UTILITY_FIRST, UTILITY_COUNT),
        };
        if self.column == 0 || self.column > count {
            return None;
        }
        StockWeapon::from_index(first + self.column as u32 - 1)
    }

    /// The position of a stock slot; None for slot 0, which is no weapon.
    pub fn of(slot: StockWeapon) -> Option<PanelPosition> {
        let index = slot.index() as u32;
        if index == 0 {
            return None;
        }
        if index >= UTILITY_FIRST {
            return Some(PanelPosition { row: PanelRow::Utility, column: (index - UTILITY_FIRST + 1) as u8 });
        }
        F_ROWS.iter().enumerate().find_map(|(i, &(first, count))| {
            (first..first + count as u32).contains(&index).then(|| PanelPosition {
                row: PanelRow::F(i as u8 + 1),
                column: (index - first + 1) as u8,
            })
        })
    }
}

impl fmt::Display for PanelPosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.row {
            PanelRow::F(n) => write!(f, "(F{n}, {})", self.column),
            PanelRow::Utility => write!(f, "(utility, {})", self.column),
        }
    }
}

fn parse_row(s: &str) -> Option<PanelRow> {
    if s.eq_ignore_ascii_case("utility") {
        return Some(PanelRow::Utility);
    }
    let n: u8 = s.strip_prefix('F').or_else(|| s.strip_prefix('f'))?.parse().ok()?;
    (1..=12).contains(&n).then_some(PanelRow::F(n))
}

impl<'de> Deserialize<'de> for PanelPosition {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            row: String,
            column: u8,
        }
        let raw = Raw::deserialize(d)?;
        let row = parse_row(&raw.row)
            .ok_or_else(|| de::Error::custom(format!("panel row {:?}: use F1..F12 or utility", raw.row)))?;
        let pos = PanelPosition { row, column: raw.column };
        if pos.slot().is_none() {
            return Err(de::Error::custom(format!("the panel has no position {pos}")));
        }
        Ok(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_weapon_slot_has_one_position_and_back() {
        let mut seen = std::collections::HashSet::new();
        for index in 1..=70u32 {
            let slot = StockWeapon::from_index(index).unwrap();
            let pos = PanelPosition::of(slot).unwrap();
            assert_eq!(pos.slot(), Some(slot), "{slot:?} at {pos}");
            assert!(seen.insert(pos), "{pos} named twice");
        }
        assert_eq!(PanelPosition::of(StockWeapon::None), None);
    }

    #[test]
    fn rows_follow_was_panel_assignment() {
        let at = |row, column| PanelPosition { row, column }.slot();
        assert_eq!(at(PanelRow::F(1), 1), Some(StockWeapon::Bazooka));
        assert_eq!(at(PanelRow::F(5), 4), Some(StockWeapon::SuperSheep));
        assert_eq!(at(PanelRow::F(5), 5), Some(StockWeapon::AquaSheep));
        assert_eq!(at(PanelRow::F(5), 6), Some(StockWeapon::MoleBomb));
        assert_eq!(at(PanelRow::F(6), 1), Some(StockWeapon::AirStrike));
        assert_eq!(at(PanelRow::F(12), 5), Some(StockWeapon::MagicBullet));
        assert_eq!(at(PanelRow::Utility, 1), Some(StockWeapon::JetPack));
        assert_eq!(at(PanelRow::Utility, 9), Some(StockWeapon::CrateShower));
        assert_eq!(at(PanelRow::F(1), 6), None);
        assert_eq!(at(PanelRow::F(13), 1), None);
        assert_eq!(at(PanelRow::Utility, 0), None);
    }

    #[test]
    fn positions_parse_from_toml() {
        #[derive(Deserialize)]
        struct T {
            p: PanelPosition,
        }
        let p: T = toml::from_str("p = { row = \"F2\", column = 3 }").unwrap();
        assert_eq!(p.p.slot(), Some(StockWeapon::BananaBomb));
        assert!(toml::from_str::<T>("p = { row = \"F1\", column = 6 }").is_err());
        assert!(toml::from_str::<T>("p = { row = \"F0\", column = 1 }").is_err());
        assert!(toml::from_str::<T>("p = { row = \"F1\", column = 1, x = 2 }").is_err());
    }
}
