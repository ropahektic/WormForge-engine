//! Editing a component profile, as the loader window does: pick a scheme, add,
//! move and remove weapons, toggle features and set their options, then write
//! the result back as the `[profile]` table.
//!
//! The editor keeps the same [`Profile`] that [`resolve`] reads, so what it
//! shows is what a match will run. A weapon is placed exactly as `resolve`
//! places it: the scheme's preset, minus what the player removed, plus what
//! the player added or moved. Moves that would put two weapons on one position
//! are refused with the occupant, so the window can ask before replacing it.

use crate::components::{
    resolve, row_name, BundleManifest, Component, ComponentKind, OptionDecl, OptionValue, Profile,
    ResolvedProfile,
};
use crate::panel::PanelPosition;
use std::collections::BTreeMap;

/// A position another weapon already holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clash {
    pub position: PanelPosition,
    pub occupant: String,
}

/// A weapon in the loadout, where it sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub id: String,
    pub position: PanelPosition,
    /// Placed by the scheme's preset (and maybe moved), rather than added.
    pub from_preset: bool,
}

/// A component and the bundle that declares it.
#[derive(Debug, Clone, Copy)]
pub struct Entry<'a> {
    pub bundle: &'a BundleManifest,
    pub component: &'a Component,
}

impl Entry<'_> {
    /// Its name, or its id when it has none.
    pub fn name(&self) -> &str {
        if self.component.name.is_empty() {
            &self.component.id
        } else {
            &self.component.name
        }
    }
}

#[derive(Debug, Clone)]
pub struct Editor {
    pub bundles: Vec<BundleManifest>,
    pub profile: Profile,
}

impl Editor {
    pub fn new(bundles: Vec<BundleManifest>, profile: Profile) -> Editor {
        Editor { bundles, profile }
    }

    pub fn entry(&self, id: &str) -> Option<Entry<'_>> {
        self.bundles.iter().find_map(|b| {
            b.components.iter().find(|c| c.id == id).map(|component| Entry { bundle: b, component })
        })
    }

    /// Every installed component of `kind`, by name.
    pub fn catalog(&self, kind: ComponentKind) -> Vec<Entry<'_>> {
        let mut out: Vec<Entry<'_>> = self
            .bundles
            .iter()
            .flat_map(|b| b.components.iter().map(move |component| Entry { bundle: b, component }))
            .filter(|e| e.component.kind == kind)
            .collect();
        out.sort_by(|a, b| a.name().to_lowercase().cmp(&b.name().to_lowercase()).then(a.component.id.cmp(&b.component.id)));
        out
    }

    /// The selected scheme's preset: weapon id -> position.
    pub fn preset(&self) -> BTreeMap<String, PanelPosition> {
        let Some(scheme) = self.profile.scheme.as_deref().and_then(|id| self.entry(id)) else {
            return BTreeMap::new();
        };
        scheme
            .component
            .preset
            .as_ref()
            .and_then(|p| scheme.bundle.presets.get(p))
            .map(|p| p.weapons.clone())
            .unwrap_or_default()
    }

    /// The loadout in panel order, placed as `resolve` places it. A weapon
    /// that isn't installed, or has no position, is left out (`errors` says
    /// why); of two on one position, both are listed.
    pub fn loadout(&self) -> Vec<Placed> {
        let preset = self.preset();
        let mut out = Vec::new();
        let mut ids: Vec<&String> = preset.keys().filter(|id| !self.profile.removed.contains(*id)).collect();
        ids.extend(self.profile.weapons.keys().filter(|id| !preset.contains_key(*id)));
        for id in ids {
            let Some(e) = self.entry(id).filter(|e| e.component.kind == ComponentKind::Weapon) else {
                continue;
            };
            let position = match (self.profile.weapons.get(id), preset.get(id)) {
                (Some(Some(p)), _) => Some(*p),
                (_, Some(p)) => Some(*p),
                _ => e.component.compatible_positions.first().copied(),
            };
            if let Some(position) = position {
                out.push(Placed { id: id.clone(), position, from_preset: preset.contains_key(id) });
            }
        }
        out.sort_by_key(|p| (p.position.slot(), p.id.clone()));
        out
    }

    pub fn in_loadout(&self, id: &str) -> bool {
        self.loadout().iter().any(|p| p.id == id)
    }

    /// The weapon at `position` other than `except`.
    pub fn occupant(&self, position: PanelPosition, except: &str) -> Option<String> {
        self.loadout().into_iter().find(|p| p.position == position && p.id != except).map(|p| p.id)
    }

    /// Choose the scheme (`None` for none). The loadout edits made to the old
    /// scheme's preset are dropped: they described that preset.
    pub fn set_scheme(&mut self, scheme: Option<String>) {
        if self.profile.scheme == scheme {
            return;
        }
        self.profile.scheme = scheme;
        self.profile.weapons.clear();
        self.profile.removed.clear();
    }

    /// How many loadout edits `set_scheme` would drop.
    pub fn loadout_edits(&self) -> usize {
        self.profile.weapons.len() + self.profile.removed.len()
    }

    /// Put `id` in the loadout: at its preset position if the scheme has one
    /// for it, else its first compatible position that is free, else its first.
    pub fn add_weapon(&mut self, id: &str) -> Result<(), Clash> {
        if self.in_loadout(id) {
            return Ok(());
        }
        let preset = self.preset();
        let position = match preset.get(id) {
            Some(p) => *p,
            None => {
                let Some(e) = self.entry(id) else {
                    return Ok(());
                };
                let positions = e.component.compatible_positions.clone();
                let Some(first) = positions.first().copied() else {
                    return Ok(());
                };
                positions.iter().copied().find(|p| self.occupant(*p, id).is_none()).unwrap_or(first)
            }
        };
        if let Some(occupant) = self.occupant(position, id) {
            return Err(Clash { position, occupant });
        }
        self.place(id, position);
        Ok(())
    }

    /// Move `id` (in the loadout or not) to `position`.
    pub fn move_weapon(&mut self, id: &str, position: PanelPosition) -> Result<(), Clash> {
        if let Some(occupant) = self.occupant(position, id) {
            return Err(Clash { position, occupant });
        }
        self.place(id, position);
        Ok(())
    }

    /// Move `id` to `position`, taking out the weapon there.
    pub fn replace(&mut self, id: &str, position: PanelPosition) {
        if let Some(occupant) = self.occupant(position, id) {
            self.remove_weapon(&occupant);
        }
        self.place(id, position);
    }

    pub fn remove_weapon(&mut self, id: &str) {
        self.profile.weapons.remove(id);
        if self.preset().contains_key(id) {
            self.profile.removed.insert(id.to_string());
        }
    }

    fn place(&mut self, id: &str, position: PanelPosition) {
        self.profile.removed.remove(id);
        let preset = self.preset();
        if preset.get(id) == Some(&position) {
            // Back where the preset puts it: no edit to keep.
            self.profile.weapons.remove(id);
        } else {
            self.profile.weapons.insert(id.to_string(), Some(position));
        }
    }

    pub fn set_feature(&mut self, id: &str, on: bool) {
        if on {
            self.profile.features.insert(id.to_string());
        } else {
            self.profile.features.remove(id);
            // resolve refuses options for a feature that isn't selected.
            self.profile.options.remove(id);
        }
    }

    /// A feature option's value: the profile's, else its default.
    pub fn option(&self, feature: &str, decl: &OptionDecl) -> OptionValue {
        self.profile
            .options
            .get(feature)
            .and_then(|o| o.get(&decl.key))
            .cloned()
            .unwrap_or_else(|| decl.kind.default_value())
    }

    /// Set an option; the default is stored as no value, so the profile only
    /// records what the player changed.
    pub fn set_option(&mut self, feature: &str, key: &str, value: OptionValue) -> Result<(), String> {
        let decl = self
            .entry(feature)
            .and_then(|e| e.component.options.iter().find(|o| o.key == key).cloned())
            .ok_or_else(|| format!("{feature} has no option {key}"))?;
        if let Some(why) = decl.kind.check(&value) {
            return Err(format!("{}: {why}", decl.label));
        }
        let values = self.profile.options.entry(feature.to_string()).or_default();
        if value == decl.kind.default_value() {
            values.remove(key);
        } else {
            values.insert(key.to_string(), value);
        }
        if values.is_empty() {
            self.profile.options.remove(feature);
        }
        Ok(())
    }

    pub fn resolve(&self) -> Result<ResolvedProfile, Vec<String>> {
        resolve(&self.bundles, &self.profile)
    }

    pub fn is_empty(&self) -> bool {
        self.profile == Profile::default()
    }

    /// The `[profile]` table for `wormforge.toml`, as `ProfileSpec` reads it.
    pub fn to_table(&self) -> toml::Table {
        let p = &self.profile;
        let mut t = toml::Table::new();
        if let Some(s) = &p.scheme {
            t.insert("scheme".into(), s.clone().into());
        }
        if !p.features.is_empty() {
            t.insert("features".into(), p.features.iter().cloned().collect::<Vec<_>>().into());
        }
        if !p.removed.is_empty() {
            t.insert("removed".into(), p.removed.iter().cloned().collect::<Vec<_>>().into());
        }
        if !p.weapons.is_empty() {
            let mut w = toml::Table::new();
            for (id, pos) in &p.weapons {
                let v = match pos {
                    None => toml::Value::Boolean(true),
                    Some(pos) => {
                        let mut at = toml::Table::new();
                        at.insert("row".into(), row_name(pos.row).into());
                        at.insert("column".into(), (pos.column as i64).into());
                        toml::Value::Table(at)
                    }
                };
                w.insert(id.clone(), v);
            }
            t.insert("weapons".into(), w.into());
        }
        if !p.options.is_empty() {
            let mut o = toml::Table::new();
            for (id, values) in &p.options {
                let mut f = toml::Table::new();
                for (key, value) in values {
                    let v = match value {
                        OptionValue::Bool(b) => toml::Value::Boolean(*b),
                        OptionValue::Int(i) => toml::Value::Integer(*i),
                        OptionValue::Choice(c) => toml::Value::String(c.clone()),
                    };
                    f.insert(key.clone(), v);
                }
                o.insert(id.clone(), f.into());
            }
            t.insert("options".into(), o.into());
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{parse_bundle, ProfileSpec};
    use crate::panel::PanelRow;

    const CATALOG: &str = r#"
id = "t.catalog"
api_version = 1
format_version = 2
[[components]]
id = "t.big"
type = "weapon"
name = "Big Bazooka"
entry = "big.lua"
compatible_positions = [{ row = "F1", column = 1 }, { row = "F2", column = 3 }]
[[components]]
id = "t.mortar"
type = "weapon"
name = "Mortar"
entry = "mortar.lua"
compatible_positions = [{ row = "F1", column = 3 }, { row = "F1", column = 1 }]
[[components]]
id = "t.tele"
type = "weapon"
name = "Teleport"
entry = "tele.lua"
compatible_positions = [{ row = "utility", column = 1 }]
[[components]]
id = "t.scheme"
type = "scheme"
name = "Test scheme"
entry = "scheme.lua"
preset = "p"
[[components]]
id = "t.other"
type = "scheme"
entry = "other.lua"
[[components]]
id = "t.note"
type = "feature"
name = "Turn note"
entry = "note.lua"
[[components.options]]
key = "show"
type = "bool"
default = true
label = "Show"
[[components.options]]
key = "lines"
type = "int"
default = 1
min = 1
max = 3
label = "Lines"
[presets.p.weapons]
"t.big" = { row = "F1", column = 1 }
"t.tele" = { row = "utility", column = 1 }
"#;

    fn at(row: PanelRow, column: u8) -> PanelPosition {
        PanelPosition { row, column }
    }

    fn editor() -> Editor {
        let mut e = Editor::new(vec![parse_bundle(CATALOG).unwrap()], Profile::default());
        e.set_scheme(Some("t.scheme".into()));
        e
    }

    fn ids(e: &Editor) -> Vec<(String, String)> {
        e.loadout().into_iter().map(|p| (p.id, p.position.to_string())).collect()
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn the_scheme_preset_is_the_loadout_and_catalogs_are_by_name() {
        let e = editor();
        assert_eq!(ids(&e), pairs(&[("t.big", "(F1, 1)"), ("t.tele", "(utility, 1)")]));
        let weapons: Vec<String> = e.catalog(ComponentKind::Weapon).iter().map(|w| w.name().to_string()).collect();
        assert_eq!(weapons, ["Big Bazooka", "Mortar", "Teleport"]);
        let schemes: Vec<String> = e.catalog(ComponentKind::Scheme).iter().map(|w| w.name().to_string()).collect();
        assert_eq!(schemes, ["t.other", "Test scheme"]);
        assert!(e.resolve().is_ok());
    }

    #[test]
    fn adding_takes_a_free_compatible_position_and_reports_a_clash() {
        let mut e = editor();
        // Mortar's first position (F1, 3) is free.
        e.add_weapon("t.mortar").unwrap();
        assert_eq!(e.profile.weapons.get("t.mortar"), Some(&Some(at(PanelRow::F(1), 3))));
        // Moving it onto Big Bazooka is refused with the occupant...
        assert_eq!(
            e.move_weapon("t.mortar", at(PanelRow::F(1), 1)),
            Err(Clash { position: at(PanelRow::F(1), 1), occupant: "t.big".into() })
        );
        // ... and replacing takes Big Bazooka out of the preset.
        e.replace("t.mortar", at(PanelRow::F(1), 1));
        assert_eq!(ids(&e), pairs(&[("t.mortar", "(F1, 1)"), ("t.tele", "(utility, 1)")]));
        assert!(e.profile.removed.contains("t.big"));
        // Adding Big Bazooka back wants its preset position, now Mortar's.
        assert_eq!(e.add_weapon("t.big"), Err(Clash { position: at(PanelRow::F(1), 1), occupant: "t.mortar".into() }));
        assert!(e.resolve().is_ok());
    }

    #[test]
    fn moving_back_to_the_preset_position_leaves_no_edit() {
        let mut e = editor();
        e.move_weapon("t.big", at(PanelRow::F(2), 3)).unwrap();
        assert_eq!(e.loadout_edits(), 1);
        e.move_weapon("t.big", at(PanelRow::F(1), 1)).unwrap();
        assert_eq!(e.loadout_edits(), 0);
        e.remove_weapon("t.big");
        assert_eq!(ids(&e), pairs(&[("t.tele", "(utility, 1)")]));
        e.add_weapon("t.big").unwrap();
        assert_eq!(e.loadout_edits(), 0);
    }

    #[test]
    fn switching_scheme_drops_the_old_loadout_edits() {
        let mut e = editor();
        e.add_weapon("t.mortar").unwrap();
        e.remove_weapon("t.tele");
        assert_eq!(e.loadout_edits(), 2);
        e.set_scheme(Some("t.other".into()));
        assert_eq!(e.loadout_edits(), 0);
        assert!(e.loadout().is_empty());
    }

    #[test]
    fn options_store_only_changes_and_go_with_their_feature() {
        let mut e = editor();
        e.set_feature("t.note", true);
        let lines = e.entry("t.note").unwrap().component.options[1].clone();
        assert_eq!(e.option("t.note", &lines), OptionValue::Int(1));
        e.set_option("t.note", "lines", OptionValue::Int(3)).unwrap();
        assert_eq!(e.option("t.note", &lines), OptionValue::Int(3));
        assert_eq!(e.set_option("t.note", "lines", OptionValue::Int(4)), Err("Lines: 4 is outside 1..=3".into()));
        e.set_option("t.note", "lines", OptionValue::Int(1)).unwrap();
        assert!(e.profile.options.is_empty());
        e.set_option("t.note", "show", OptionValue::Bool(false)).unwrap();
        e.set_feature("t.note", false);
        assert!(e.profile.options.is_empty() && e.profile.features.is_empty());
    }

    #[test]
    fn a_catalog_of_hundreds_of_weapons_keeps_the_loadout_to_what_is_placed() {
        // 420 weapons over 6 bundles, each checked at its own stock position
        // and at Bazooka's, as a large catalog would be.
        let mut bundles = Vec::new();
        for b in 0..6 {
            let mut src = format!("id = \"big.b{b}\"\napi_version = 1\nformat_version = 2\n");
            for w in 0..70 {
                let slot = crate::StockWeapon::from_index(w as u32 + 1).unwrap();
                let p = PanelPosition::of(slot).unwrap();
                src.push_str(&format!(
                    "[[components]]\nid = \"big.b{b}.w{w}\"\ntype = \"weapon\"\nentry = \"w.lua\"\ncompatible_positions = [{{ row = \"{}\", column = {} }}, {{ row = \"F1\", column = 1 }}]\n",
                    row_name(p.row),
                    p.column
                ));
            }
            bundles.push(parse_bundle(&src).unwrap());
        }
        let mut e = Editor::new(bundles, Profile::default());
        assert_eq!(e.catalog(ComponentKind::Weapon).len(), 420);
        assert!(e.loadout().is_empty());
        let started = std::time::Instant::now();
        for w in 0..70 {
            e.add_weapon(&format!("big.b{}.w{w}", w % 6)).unwrap();
        }
        // The panel is full: a 71st weapon clashes on its first position.
        assert_eq!(e.loadout().len(), 70);
        assert_eq!(e.add_weapon("big.b5.w0").unwrap_err().occupant, "big.b0.w0");
        assert!(e.resolve().is_ok());
        assert!(started.elapsed() < std::time::Duration::from_secs(2), "{:?}", started.elapsed());
    }

    #[test]
    fn the_table_reads_back_as_the_same_profile() {
        let mut e = editor();
        e.set_feature("t.note", true);
        e.set_option("t.note", "lines", OptionValue::Int(2)).unwrap();
        e.add_weapon("t.mortar").unwrap();
        e.move_weapon("t.big", at(PanelRow::F(2), 3)).unwrap();
        e.remove_weapon("t.tele");
        let table = e.to_table();
        let back = ProfileSpec::from_table(&table).unwrap().to_profile().unwrap();
        assert_eq!(back, e.profile);
        let text = toml::to_string(&table).unwrap();
        assert!(text.contains("scheme = \"t.scheme\""), "{text}");
        assert!(Editor::new(Vec::new(), Profile::default()).to_table().is_empty());
    }
}
