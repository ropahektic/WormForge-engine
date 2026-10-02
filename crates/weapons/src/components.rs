//! Component bundles (`format_version = 2`) and profile resolution.
//!
//! A bundle is one installed pack folder whose `mod.toml` declares
//! components instead of one entry: **weapons** (behaviour plus default
//! weapon data, placeable at the panel positions their author checked),
//! **schemes** (choose and place weapons through a preset, and run the
//! round's rules) and **features** (independent behaviour with options).
//!
//! Everything here is read from TOML, so the loader can list, validate and
//! resolve a selection without running any Lua. [`resolve`] turns the
//! installed bundles and a proposed [`Profile`] into a [`ResolvedProfile`]
//! (every component in its load order, every weapon on its stock slot, every
//! option with a value) or the complete list of what is wrong with it.
//!
//! Not here yet: running the selected components (their Lua returns weapon
//! defaults and scheme overrides in a declaration pass), and freezing the
//! resolved profile for the network and replays.

use crate::panel::PanelPosition;
use crate::StockWeapon;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// The manifest format that declares components.
pub const COMPONENTS_FORMAT: u32 = 2;

/// A bundle's `mod.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleManifest {
    pub id: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub author: String,
    pub api_version: u32,
    pub format_version: u32,
    #[serde(default)]
    pub components: Vec<Component>,
    /// Scheme presets by name: which weapon goes where.
    #[serde(default)]
    pub presets: BTreeMap<String, Preset>,
}

fn default_version() -> String {
    "0.1.0".into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComponentKind {
    Weapon,
    Scheme,
    Feature,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    /// Unique across every installed bundle, e.g. `core.double_bazooka`.
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ComponentKind,
    /// The component's Lua file, relative to the bundle.
    pub entry: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Weapons: where a scheme may place it, as its author checked.
    #[serde(default)]
    pub compatible_positions: Vec<PanelPosition>,
    /// Schemes: the preset (in this bundle) that places their weapons.
    #[serde(default)]
    pub preset: Option<String>,
    /// Features: their settings.
    #[serde(default)]
    pub options: Vec<OptionDecl>,
    /// Component ids that must be selected with this one.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Component ids that must not be selected with this one.
    #[serde(default)]
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    /// Weapon component id -> its position.
    #[serde(default)]
    pub weapons: BTreeMap<String, PanelPosition>,
}

/// One setting of a feature; the loader draws a checkbox, a number control or
/// a dropdown from it.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RawOption")]
pub struct OptionDecl {
    pub key: String,
    pub kind: OptionKind,
    pub label: String,
    pub description: String,
}

/// An option as written: every field any type uses, so a field that does not
/// belong (a `min` on a bool, a typo) is an error rather than ignored.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOption {
    key: String,
    #[serde(rename = "type")]
    kind: String,
    label: String,
    #[serde(default)]
    description: String,
    default: toml::Value,
    min: Option<i64>,
    max: Option<i64>,
    choices: Option<Vec<String>>,
}

impl TryFrom<RawOption> for OptionDecl {
    type Error = String;

    fn try_from(r: RawOption) -> Result<Self, String> {
        let key = r.key.clone();
        let wrong = |what: &str| format!("option {key}: {what}");
        let kind = match r.kind.as_str() {
            "bool" => {
                if r.min.is_some() || r.max.is_some() || r.choices.is_some() {
                    return Err(wrong("a bool has no min, max or choices"));
                }
                let default = r.default.as_bool().ok_or_else(|| wrong("default must be true or false"))?;
                OptionKind::Bool { default }
            }
            "int" => {
                if r.choices.is_some() {
                    return Err(wrong("an int has no choices"));
                }
                let (Some(min), Some(max)) = (r.min, r.max) else {
                    return Err(wrong("an int needs min and max"));
                };
                let default = r.default.as_integer().ok_or_else(|| wrong("default must be a whole number"))?;
                OptionKind::Int { min, max, default }
            }
            "choice" => {
                if r.min.is_some() || r.max.is_some() {
                    return Err(wrong("a choice has no min or max"));
                }
                let choices = r.choices.ok_or_else(|| wrong("a choice needs choices"))?;
                let default = r.default.as_str().ok_or_else(|| wrong("default must be one of its choices"))?.to_string();
                OptionKind::Choice { choices, default }
            }
            other => return Err(wrong(&format!("type {other:?}: use bool, int or choice"))),
        };
        Ok(OptionDecl { key: r.key, kind, label: r.label, description: r.description })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OptionKind {
    Bool { default: bool },
    Int { min: i64, max: i64, default: i64 },
    Choice { choices: Vec<String>, default: String },
}

/// A setting's value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum OptionValue {
    Bool(bool),
    Int(i64),
    Choice(String),
}

impl OptionKind {
    pub fn default_value(&self) -> OptionValue {
        match self {
            OptionKind::Bool { default } => OptionValue::Bool(*default),
            OptionKind::Int { default, .. } => OptionValue::Int(*default),
            OptionKind::Choice { default, .. } => OptionValue::Choice(default.clone()),
        }
    }

    /// Why `value` is not a value of this option, if it is not.
    fn check(&self, value: &OptionValue) -> Option<String> {
        match (self, value) {
            (OptionKind::Bool { .. }, OptionValue::Bool(_)) => None,
            (OptionKind::Int { min, max, .. }, OptionValue::Int(v)) => {
                (v < min || v > max).then(|| format!("{v} is outside {min}..={max}"))
            }
            (OptionKind::Choice { choices, .. }, OptionValue::Choice(v)) => {
                (!choices.contains(v)).then(|| format!("{v:?} is not one of {}", choices.join(", ")))
            }
            (OptionKind::Bool { .. }, _) => Some("expected true or false".into()),
            (OptionKind::Int { .. }, _) => Some("expected a whole number".into()),
            (OptionKind::Choice { .. }, _) => Some("expected one of its choices".into()),
        }
    }
}

/// Whether a manifest declares the component format, without parsing it.
pub fn declares_components(src: &str) -> bool {
    #[derive(Deserialize)]
    struct Probe {
        #[serde(default)]
        format_version: Option<u32>,
    }
    toml::from_str::<Probe>(src).ok().and_then(|p| p.format_version).unwrap_or(1) >= COMPONENTS_FORMAT
}

/// Parse and check a bundle's `mod.toml`. Every problem is reported, one per
/// line, so an author sees them all at once.
pub fn parse_bundle(src: &str) -> Result<BundleManifest, String> {
    let m: BundleManifest = toml::from_str(src).map_err(|e| e.to_string())?;
    let mut errors = Vec::new();
    let mut err = |e: String| errors.push(format!("{}: {e}", m.id));
    if m.id.trim().is_empty() {
        return Err("mod.toml id is empty".into());
    }
    if m.api_version != crate::API_VERSION {
        err(format!("api_version {} is not supported (need {})", m.api_version, crate::API_VERSION));
    }
    if m.format_version != COMPONENTS_FORMAT {
        err(format!("format_version {} is not supported (need {COMPONENTS_FORMAT})", m.format_version));
    }
    if m.components.is_empty() {
        err("declares no components".into());
    }
    let mut ids = BTreeSet::new();
    for c in &m.components {
        if !valid_id(&c.id) {
            err(format!("component id {:?}: use lowercase letters, digits, '_', '-' and '.'", c.id));
        }
        if !ids.insert(c.id.as_str()) {
            err(format!("component {} is declared twice", c.id));
        }
        if !valid_entry(&c.entry) {
            err(format!("{}: entry {:?} must be a .lua file inside the bundle", c.id, c.entry));
        }
        match c.kind {
            ComponentKind::Weapon => {
                if c.compatible_positions.is_empty() {
                    err(format!("{}: a weapon names at least one compatible position", c.id));
                }
            }
            ComponentKind::Scheme => match &c.preset {
                Some(p) if !m.presets.contains_key(p) => err(format!("{}: no preset {p:?} in this bundle", c.id)),
                _ => {}
            },
            ComponentKind::Feature => {}
        }
        if c.kind != ComponentKind::Weapon && !c.compatible_positions.is_empty() {
            err(format!("{}: only a weapon has compatible positions", c.id));
        }
        if c.kind != ComponentKind::Scheme && c.preset.is_some() {
            err(format!("{}: only a scheme has a preset", c.id));
        }
        if c.kind != ComponentKind::Feature && !c.options.is_empty() {
            err(format!("{}: only a feature has options", c.id));
        }
        let mut keys = BTreeSet::new();
        for o in &c.options {
            if !valid_key(&o.key) {
                err(format!("{}: option key {:?}: use lowercase letters, digits and '_'", c.id, o.key));
            }
            if !keys.insert(o.key.as_str()) {
                err(format!("{}: option {} is declared twice", c.id, o.key));
            }
            if o.label.trim().is_empty() {
                err(format!("{}: option {} needs a label", c.id, o.key));
            }
            match &o.kind {
                OptionKind::Int { min, max, .. } if min > max => {
                    err(format!("{}: option {}: min {min} is above max {max}", c.id, o.key));
                }
                OptionKind::Choice { choices, .. } if choices.is_empty() => {
                    err(format!("{}: option {} has no choices", c.id, o.key));
                }
                kind => {
                    if let Some(why) = kind.check(&kind.default_value()) {
                        err(format!("{}: option {} default: {why}", c.id, o.key));
                    }
                }
            }
        }
        for other in c.requires.iter().chain(&c.conflicts) {
            if other == &c.id {
                err(format!("{}: names itself in requires or conflicts", c.id));
            }
        }
    }
    for (name, preset) in &m.presets {
        if !m.components.iter().any(|c| c.preset.as_deref() == Some(name.as_str())) {
            err(format!("preset {name:?} is used by no scheme"));
        }
        let mut taken = BTreeMap::new();
        for (weapon, pos) in &preset.weapons {
            if let Some(first) = taken.insert(*pos, weapon) {
                err(format!("preset {name:?} puts {first} and {weapon} at {pos}"));
            }
        }
    }
    if errors.is_empty() {
        Ok(m)
    } else {
        Err(errors.join("\n"))
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.'))
}

fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 64 && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A relative `.lua` path of plain segments, as `require` names files.
fn valid_entry(entry: &str) -> bool {
    let Some(stem) = entry.strip_suffix(".lua") else {
        return false;
    };
    !stem.is_empty()
        && stem.split('/').all(|s| {
            !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

/// What a player picked: the loader's saved selection, before resolution.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    /// At most one scheme. Its preset places its weapons.
    pub scheme: Option<String>,
    /// Weapons by component id, each with the position the player chose;
    /// `None` keeps the preset's (or, for a weapon outside the preset, its
    /// first compatible position).
    pub weapons: BTreeMap<String, Option<PanelPosition>>,
    /// Weapons of the scheme's preset the player took out.
    pub removed: BTreeSet<String>,
    pub features: BTreeSet<String>,
    /// Component id -> option key -> value; options left out take their default.
    pub options: BTreeMap<String, BTreeMap<String, OptionValue>>,
}

/// A selection with nothing left to decide.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProfile {
    pub scheme: Option<Selected>,
    /// One per stock slot, in slot order.
    pub weapons: Vec<PlacedWeapon>,
    /// In component id order.
    pub features: Vec<Selected>,
    /// Every option of every selected feature, defaults filled in.
    pub options: BTreeMap<String, BTreeMap<String, OptionValue>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    pub bundle: String,
    pub id: String,
    pub entry: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedWeapon {
    pub component: Selected,
    pub position: PanelPosition,
    pub slot: StockWeapon,
}

impl ResolvedProfile {
    /// The order the selected components run in: weapons by slot (their
    /// defaults first), then the scheme (its overrides and rules), then the
    /// features by id. Fixed, so every peer and every replay runs them alike.
    pub fn load_order(&self) -> Vec<&Selected> {
        self.weapons
            .iter()
            .map(|w| &w.component)
            .chain(self.scheme.iter())
            .chain(self.features.iter())
            .collect()
    }
}

/// Resolve `profile` against the installed `bundles`: every problem is
/// returned, not just the first.
pub fn resolve(bundles: &[BundleManifest], profile: &Profile) -> Result<ResolvedProfile, Vec<String>> {
    let mut errors = Vec::new();
    let mut by_id: BTreeMap<&str, (&BundleManifest, &Component)> = BTreeMap::new();
    for b in bundles {
        for c in &b.components {
            if let Some((first, _)) = by_id.insert(c.id.as_str(), (b, c)) {
                errors.push(format!("component {} is declared by both {} and {}", c.id, first.id, b.id));
            }
        }
    }
    let selected = |b: &BundleManifest, c: &Component| Selected {
        bundle: b.id.clone(),
        id: c.id.clone(),
        entry: c.entry.clone(),
    };
    let lookup = |id: &str, kind: ComponentKind, errors: &mut Vec<String>| match by_id.get(id) {
        Some(&(b, c)) if c.kind == kind => Some((b, c)),
        Some(&(_, c)) => {
            errors.push(format!("{id} is a {:?}, not a {kind:?}", c.kind).to_lowercase());
            None
        }
        None => {
            errors.push(format!("{id} is not installed"));
            None
        }
    };

    // The scheme, and the weapons its preset places.
    let mut wanted: BTreeMap<String, Option<PanelPosition>> = BTreeMap::new();
    let scheme = profile.scheme.as_deref().and_then(|id| {
        let (b, c) = lookup(id, ComponentKind::Scheme, &mut errors)?;
        if let Some(preset) = c.preset.as_ref().and_then(|p| b.presets.get(p)) {
            for (weapon, pos) in &preset.weapons {
                if !profile.removed.contains(weapon) {
                    wanted.insert(weapon.clone(), Some(*pos));
                }
            }
        }
        Some(selected(b, c))
    });
    for (weapon, pos) in &profile.weapons {
        match pos {
            Some(p) => {
                wanted.insert(weapon.clone(), Some(*p));
            }
            None => {
                wanted.entry(weapon.clone()).or_insert(None);
            }
        }
    }

    let mut weapons = Vec::new();
    let mut at: BTreeMap<PanelPosition, String> = BTreeMap::new();
    for (id, pos) in &wanted {
        let Some((b, c)) = lookup(id, ComponentKind::Weapon, &mut errors) else {
            continue;
        };
        let position = match pos {
            Some(p) if c.compatible_positions.contains(p) => *p,
            Some(p) => {
                errors.push(format!("{id} cannot go at {p}: its author checked it only at {}", list(&c.compatible_positions)));
                continue;
            }
            None => c.compatible_positions[0],
        };
        if let Some(first) = at.insert(position, id.clone()) {
            errors.push(format!("{first} and {id} both want {position}"));
            continue;
        }
        let slot = position.slot().expect("positions are checked when parsed");
        weapons.push(PlacedWeapon { component: selected(b, c), position, slot });
    }
    weapons.sort_by_key(|w| w.slot);

    let mut features = Vec::new();
    for id in &profile.features {
        if let Some((b, c)) = lookup(id, ComponentKind::Feature, &mut errors) {
            features.push(selected(b, c));
        }
    }

    // Dependencies, among everything selected.
    let chosen: BTreeSet<&str> = weapons
        .iter()
        .map(|w| w.component.id.as_str())
        .chain(scheme.iter().map(|s| s.id.as_str()))
        .chain(features.iter().map(|f| f.id.as_str()))
        .collect();
    for id in &chosen {
        let (_, c) = by_id[id];
        for need in &c.requires {
            if !chosen.contains(need.as_str()) {
                errors.push(format!("{id} requires {need}, which is not selected"));
            }
        }
        for clash in &c.conflicts {
            if chosen.contains(clash.as_str()) {
                errors.push(format!("{id} conflicts with {clash}"));
            }
        }
    }

    // Options: every option of every selected feature, checked.
    let mut options = BTreeMap::new();
    for id in profile.options.keys() {
        if !features.iter().any(|f| &f.id == id) {
            errors.push(format!("options given for {id}, which is not a selected feature"));
        }
    }
    for f in &features {
        let (_, c) = by_id[f.id.as_str()];
        let given = profile.options.get(&f.id);
        let mut resolved = BTreeMap::new();
        for o in &c.options {
            let value = match given.and_then(|g| g.get(&o.key)) {
                Some(v) => {
                    if let Some(why) = o.kind.check(v) {
                        errors.push(format!("{}: option {}: {why}", f.id, o.key));
                        continue;
                    }
                    v.clone()
                }
                None => o.kind.default_value(),
            };
            resolved.insert(o.key.clone(), value);
        }
        for key in given.into_iter().flat_map(|g| g.keys()) {
            if !c.options.iter().any(|o| &o.key == key) {
                errors.push(format!("{}: has no option {key}", f.id));
            }
        }
        options.insert(f.id.clone(), resolved);
    }

    if errors.is_empty() {
        Ok(ResolvedProfile { scheme, weapons, features, options })
    } else {
        Err(errors)
    }
}

fn list(positions: &[PanelPosition]) -> String {
    positions.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::PanelRow;

    const WEAPONS: &str = r#"
id = "catalog.weapons"
api_version = 1
format_version = 2

[[components]]
id = "core.double_bazooka"
type = "weapon"
entry = "double_bazooka.lua"
compatible_positions = [{ row = "F1", column = 1 }]

[[components]]
id = "core.teleport"
type = "weapon"
entry = "weapons/teleport.lua"
compatible_positions = [{ row = "F8", column = 4 }, { row = "utility", column = 1 }]

[[components]]
id = "core.mortar"
type = "weapon"
entry = "mortar.lua"
compatible_positions = [{ row = "F1", column = 3 }, { row = "F1", column = 1 }]
"#;

    const SCHEMES: &str = r#"
id = "catalog.schemes"
api_version = 1
format_version = 2

[[components]]
id = "hl.highlander"
type = "scheme"
entry = "highlander.lua"
preset = "highlander"

[presets.highlander.weapons]
"core.double_bazooka" = { row = "F1", column = 1 }
"core.teleport" = { row = "F8", column = 4 }
"#;

    const FEATURES: &str = r#"
id = "catalog.features"
api_version = 1
format_version = 2

[[components]]
id = "core.utilities"
type = "feature"
entry = "utilities.lua"
conflicts = ["core.fow"]

[[components.options]]
key = "always_fast_walk"
type = "bool"
default = false
label = "Always fast walk"

[[components.options]]
key = "fow_strength"
type = "int"
min = 0
max = 10
default = 5
label = "Fog strength"

[[components.options]]
key = "crate_mode"
type = "choice"
choices = ["off", "weapons", "all"]
default = "weapons"
label = "Crates"

[[components]]
id = "core.fow"
type = "feature"
entry = "fow.lua"
requires = ["hl.highlander"]
"#;

    fn bundles() -> Vec<BundleManifest> {
        [WEAPONS, SCHEMES, FEATURES].iter().map(|s| parse_bundle(s).unwrap()).collect()
    }

    fn pos(row: u8, column: u8) -> PanelPosition {
        PanelPosition { row: PanelRow::F(row), column }
    }

    #[test]
    fn the_plans_example_bundles_parse() {
        let b = bundles();
        assert_eq!(b[0].components.len(), 3);
        assert_eq!(b[1].presets["highlander"].weapons.len(), 2);
        assert_eq!(b[2].components[0].options[2].kind.default_value(), OptionValue::Choice("weapons".into()));
        assert!(declares_components(WEAPONS));
        assert!(!declares_components("id = \"old\"\napi_version = 1\n"));
    }

    #[test]
    fn a_scheme_places_its_preset_and_features_get_their_defaults() {
        let profile = Profile {
            scheme: Some("hl.highlander".into()),
            features: ["core.utilities".to_string()].into(),
            ..Profile::default()
        };
        let r = resolve(&bundles(), &profile).unwrap();
        let placed: Vec<_> = r.weapons.iter().map(|w| (w.component.id.as_str(), w.slot)).collect();
        assert_eq!(placed, [("core.double_bazooka", StockWeapon::Bazooka), ("core.teleport", StockWeapon::Teleport)]);
        assert_eq!(r.options["core.utilities"]["fow_strength"], OptionValue::Int(5));
        let order: Vec<_> = r.load_order().iter().map(|s| s.id.as_str()).collect();
        assert_eq!(order, ["core.double_bazooka", "core.teleport", "hl.highlander", "core.utilities"]);
        assert_eq!(r.weapons[1].component.entry, "weapons/teleport.lua");
    }

    #[test]
    fn the_player_moves_adds_and_removes_weapons() {
        let profile = Profile {
            scheme: Some("hl.highlander".into()),
            weapons: [
                ("core.teleport".to_string(), Some(PanelPosition { row: PanelRow::Utility, column: 1 })),
                ("core.mortar".to_string(), None),
            ]
            .into(),
            removed: ["core.double_bazooka".to_string()].into(),
            ..Profile::default()
        };
        let r = resolve(&bundles(), &profile).unwrap();
        let placed: Vec<_> = r.weapons.iter().map(|w| (w.component.id.as_str(), w.slot)).collect();
        assert_eq!(placed, [("core.mortar", StockWeapon::Mortar), ("core.teleport", StockWeapon::JetPack)]);
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let profile = Profile {
            scheme: Some("hl.highlander".into()),
            weapons: [
                ("core.mortar".to_string(), Some(pos(1, 1))),
                ("core.teleport".to_string(), Some(pos(2, 1))),
                ("core.missing".to_string(), None),
                ("core.utilities".to_string(), None),
            ]
            .into(),
            features: ["core.utilities".to_string(), "core.fow".to_string()].into(),
            options: [
                ("core.utilities".to_string(), [
                    ("fow_strength".to_string(), OptionValue::Int(11)),
                    ("crate_mode".to_string(), OptionValue::Choice("some".into())),
                    ("always_fast_walk".to_string(), OptionValue::Int(1)),
                    ("nope".to_string(), OptionValue::Bool(true)),
                ].into()),
                ("core.mortar".to_string(), BTreeMap::new()),
            ]
            .into(),
            ..Profile::default()
        };
        let errors = resolve(&bundles(), &profile).unwrap_err();
        let expect = [
            "core.double_bazooka and core.mortar both want (F1, 1)",
            "core.teleport cannot go at (F2, 1)",
            "core.missing is not installed",
            "core.utilities is a feature, not a weapon",
            "core.utilities conflicts with core.fow",
            "options given for core.mortar, which is not a selected feature",
            "core.utilities: option fow_strength: 11 is outside 0..=10",
            "core.utilities: option crate_mode: \"some\" is not one of off, weapons, all",
            "core.utilities: option always_fast_walk: expected true or false",
            "core.utilities: has no option nope",
        ];
        for e in expect {
            assert!(errors.iter().any(|x| x.starts_with(e)), "missing {e:?} in {errors:#?}");
        }
        assert_eq!(errors.len(), expect.len(), "{errors:#?}");
    }

    #[test]
    fn requires_and_one_install_per_component_id() {
        let profile = Profile { features: ["core.fow".to_string()].into(), ..Profile::default() };
        let errors = resolve(&bundles(), &profile).unwrap_err();
        assert_eq!(errors, ["core.fow requires hl.highlander, which is not selected"]);

        let mut twice = bundles();
        twice.push(parse_bundle(&WEAPONS.replace("catalog.weapons", "other.weapons")).unwrap());
        let errors = resolve(&twice, &Profile::default()).unwrap_err();
        assert!(errors.iter().all(|e| e.contains("is declared by both catalog.weapons and other.weapons")));
        assert_eq!(errors.len(), 3);
    }

    #[test]
    fn bad_bundles_list_every_mistake() {
        let src = r#"
id = "bad"
api_version = 1
format_version = 2

[[components]]
id = "Bad Id"
type = "weapon"
entry = "../x.lua"

[[components]]
id = "s"
type = "scheme"
entry = "s.lua"
preset = "missing"
compatible_positions = [{ row = "F1", column = 1 }]

[[components]]
id = "f"
type = "feature"
entry = "f.lua"
requires = ["f"]

[[components.options]]
key = "n"
type = "int"
min = 5
max = 1
default = 3
label = ""

[[components.options]]
key = "c"
type = "choice"
choices = ["a"]
default = "b"
label = "C"

[presets.orphan.weapons]
"a" = { row = "F1", column = 1 }
"b" = { row = "F1", column = 1 }
"#;
        let err = parse_bundle(src).unwrap_err();
        for e in [
            "component id \"Bad Id\"",
            "Bad Id: entry \"../x.lua\"",
            "Bad Id: a weapon names at least one compatible position",
            "s: no preset \"missing\"",
            "s: only a weapon has compatible positions",
            "f: option n needs a label",
            "f: option n: min 5 is above max 1",
            "f: option c default: \"b\" is not one of a",
            "f: names itself",
            "preset \"orphan\" is used by no scheme",
            "preset \"orphan\" puts a and b at (F1, 1)",
        ] {
            assert!(err.contains(e), "missing {e:?} in\n{err}");
        }
        assert!(parse_bundle("id = \"x\"\napi_version = 1\nformat_version = 2\nentry = \"init.lua\"\n").is_err(),
            "a v1 field in a bundle is refused, not ignored");
        let option = |body: &str| {
            parse_bundle(&format!(
                "id = \"o\"\napi_version = 1\nformat_version = 2\n[[components]]\nid = \"f\"\ntype = \"feature\"\nentry = \"f.lua\"\n[[components.options]]\nkey = \"k\"\nlabel = \"K\"\n{body}"
            ))
        };
        assert!(option("type = \"bool\"\ndefault = true\n").is_ok());
        for (body, why) in [
            ("type = \"bool\"\ndefault = true\nmin = 1\n", "a bool has no min"),
            ("type = \"int\"\ndefault = 1\n", "an int needs min and max"),
            ("type = \"int\"\nmin = 0\nmax = 2\ndefault = \"1\"\n", "default must be a whole number"),
            ("type = \"choice\"\ndefault = \"a\"\n", "a choice needs choices"),
            ("type = \"float\"\ndefault = 1.0\n", "use bool, int or choice"),
            ("type = \"bool\"\ndefault = true\ndefualt = false\n", "unknown field"),
        ] {
            let e = option(body).unwrap_err();
            assert!(e.contains(why), "{body}: {e}");
        }
    }
}
