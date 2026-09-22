//! Stock 71-slot overlay registry and pack manifests.
//!
//! v1 cannot grow the weapon table. Every Lua claim occupies an existing
//! stock slot. If two mods overlay the same slot, the first in load order
//! (sorted mod id) wins and the later claim is skipped. That skip is the
//! dispatcher: handlers for a slot belong to the winner, they do not stack.

mod overlay;
mod stock;
mod templates;

pub use overlay::{
    ActorSpawn, AttachedSpawn, ClusterHoming, ClusterSpec, HomingMode, OverlayClaim, OverlayParams,
    ResolveResult, ScreenBlit, SkippedClaim, SpawnExplode, SpawnFrom, SpriteOrigin, apply_params,
    resolve_claims,
};
pub use stock::{parse_panel_icon, parse_stock_weapon, StockWeapon, STOCK_NAMES};
pub use templates::{
    template_by_id, BodyKind, FireStyle, FireTemplate, SpriteUse, FIRE_TEMPLATES,
};

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const API_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
pub struct ModManifest {
    pub id: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default = "default_api")]
    pub api_version: u32,
    #[serde(default = "default_entry")]
    pub entry: String,
    #[serde(default)]
    pub weapons: WeaponClaims,
    #[serde(default)]
    pub actors: ActorClaims,
}

fn default_version() -> String {
    "0.1.0".into()
}
fn default_api() -> u32 {
    API_VERSION
}
fn default_entry() -> String {
    "init.lua".into()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WeaponClaims {
    /// Logical ids this pack may `register` (still occupy a stock slot in v1).
    #[serde(default)]
    pub register: Vec<String>,
    /// Stock slots this pack may overlay (`bazooka`, …). Alias of overlay.
    #[serde(default)]
    pub replace: Vec<String>,
    #[serde(default)]
    pub overlay: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
}

impl WeaponClaims {
    pub fn allowed_slots(&self) -> Result<Vec<StockWeapon>, String> {
        let mut names = self.replace.clone();
        names.extend(self.overlay.iter().cloned());
        let mut slots = Vec::new();
        for name in names {
            slots.push(parse_stock_weapon(&name)?);
        }
        Ok(slots)
    }
}

/// LuaActor kinds this pack may own (`mod_id.kind`). Native `drop` spawn is installed.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ActorClaims {
    #[serde(default)]
    pub register: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LoadedMod {
    pub manifest: ModManifest,
    pub root: PathBuf,
}

pub fn parse_manifest(src: &str) -> Result<ModManifest, String> {
    let m: ModManifest = toml::from_str(src).map_err(|e| e.to_string())?;
    if m.id.trim().is_empty() {
        return Err("mod.toml id is empty".into());
    }
    if m.api_version != API_VERSION {
        return Err(format!(
            "mod {} api_version {} is not supported (need {API_VERSION})",
            m.id, m.api_version
        ));
    }
    Ok(m)
}

/// Discover `Mods/*/mod.toml`, then sort by mod id (not directory order).
pub fn discover_mods(mods_dir: &Path) -> Result<Vec<LoadedMod>, String> {
    let mut mods = Vec::new();
    if !mods_dir.is_dir() {
        return Ok(mods);
    }
    let entries = std::fs::read_dir(mods_dir).map_err(|e| e.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("mod.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let src = std::fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
        let manifest = parse_manifest(&src)?;
        mods.push(LoadedMod {
            manifest,
            root: path,
        });
    }
    mods.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(dedupe_mod_ids(mods).0)
}

/// Drop later folders that reuse an id. Returns (kept, skipped ids).
pub fn dedupe_mod_ids(mods: Vec<LoadedMod>) -> (Vec<LoadedMod>, Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    let mut kept = Vec::new();
    let mut skipped = Vec::new();
    for m in mods {
        if !seen.insert(m.manifest.id.clone()) {
            skipped.push(m.manifest.id);
            continue;
        }
        kept.push(m);
    }
    (kept, skipped)
}

/// Fingerprint of the enabled set. Same mods in a different folder order
/// still hash the same because discovery sorts by id.
pub fn modset_fingerprint(mods: &[LoadedMod]) -> String {
    let mut parts: Vec<String> = mods
        .iter()
        .map(|m| format!("{}@{}", m.manifest.id, m.manifest.version))
        .collect();
    parts.sort();
    parts.join(";")
}

pub fn filter_requirements(mods: &[LoadedMod]) -> (Vec<LoadedMod>, Vec<String>) {
    let ids: std::collections::HashSet<_> = mods.iter().map(|m| m.manifest.id.clone()).collect();
    let mut kept = Vec::new();
    let mut skipped = Vec::new();
    for m in mods {
        let missing: Vec<_> = m
            .manifest
            .weapons
            .requires
            .iter()
            .filter(|r| !ids.contains(*r))
            .cloned()
            .collect();
        if !missing.is_empty() {
            skipped.push(format!(
                "{} skipped: missing requires {}",
                m.manifest.id,
                missing.join(",")
            ));
            continue;
        }
        kept.push(m.clone());
    }
    (kept, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parses_overlay_slots() {
        let src = r#"
id = "example.ice_bazooka"
api_version = 1
entry = "weapons.lua"
[weapons]
register = ["example.ice_bazooka"]
replace = ["bazooka"]
"#;
        let m = parse_manifest(src).unwrap();
        let slots = m.weapons.allowed_slots().unwrap();
        assert_eq!(slots, vec![StockWeapon::Bazooka]);
        assert!(m.actors.register.is_empty());
    }

    #[test]
    fn manifest_parses_actor_kinds() {
        let src = r#"
id = "example.boss"
api_version = 1
entry = "weapons.lua"
[actors]
register = ["example.boss.mole"]
"#;
        let m = parse_manifest(src).unwrap();
        assert_eq!(m.actors.register, vec!["example.boss.mole"]);
    }

    #[test]
    fn fire_templates_use_real_gfx_stems() {
        for t in FIRE_TEMPLATES {
            assert_eq!(STOCK_NAMES[t.copy_from as usize].is_empty(), false);
            for s in t.sprites {
                assert!(
                    gfx::stock_sprite_id(s.name).is_some(),
                    "template {} sprite {}",
                    t.id,
                    s.name
                );
            }
        }
        assert_eq!(template_by_id("power_missile").unwrap().copy_from, StockWeapon::Bazooka);
        assert!(FIRE_TEMPLATES.iter().any(|t| t.fire == FireStyle::Hitscan));
        assert!(FIRE_TEMPLATES.iter().any(|t| t.fire == FireStyle::Drop));
        assert!(FIRE_TEMPLATES.iter().any(|t| t.fire == FireStyle::Cursor));
    }
}
