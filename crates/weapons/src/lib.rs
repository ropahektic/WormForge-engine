//! Stock 71-slot overlay registry and pack manifests.
//!
//! v1 cannot grow the weapon table. Every Lua claim occupies an existing
//! stock slot. If two mods overlay the same slot, the first in load order
//! (sorted mod id) wins and the later claim is skipped. That skip is the
//! dispatcher: handlers for a slot belong to the winner, they do not stack.

pub mod components;
mod overlay;
pub mod panel;
mod stock;
mod templates;
pub mod wep;

pub use overlay::{
    PanelIcon, HoldFlash, HoldSpec,
    ActorSpawn, AttachedSpawn, ClusterHoming, ClusterSpec, HomingMode, OverlayClaim, OverlayParams,
    ResolveResult, ScreenBlit, SkippedClaim, SpawnExplode, SpawnFrom, SpriteOrigin, WormTeamTint,
    apply_params, resolve_claims,
};
pub use stock::{parse_panel_icon, parse_stock_weapon, StockWeapon, STOCK_NAMES};
pub use templates::{
    template_by_id, BodyKind, FireStyle, FireTemplate, SpriteUse, FIRE_TEMPLATES,
};

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const API_VERSION: u32 = 2;

/// What the pack primarily does. Weapons packs overlay stock slots; gameplay
/// packs change match rules without claiming a panel weapon (Highlander, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackKind {
    Weapons,
    Gameplay,
}

impl Default for PackKind {
    fn default() -> Self {
        PackKind::Weapons
    }
}

impl PackKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PackKind::Weapons => "weapons",
            PackKind::Gameplay => "gameplay",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PackKind::Weapons => "Weapons",
            PackKind::Gameplay => "Gameplay",
        }
    }
}

fn default_kind() -> PackKind {
    PackKind::Weapons
}

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
    /// `weapons` (default) or `gameplay`. Omitted packs that claim no slots
    /// are treated as gameplay so rule packs do not look like empty weapons.
    #[serde(default = "default_kind")]
    pub kind: PackKind,
    #[serde(default)]
    pub weapons: WeaponClaims,
    #[serde(default)]
    pub actors: ActorClaims,
    /// `debug_worm_state = true`: the pack may call `worm:set_state`, a
    /// debugging tool on its way out (packs hand worms over with the engine's
    /// verbs instead). Every call is logged.
    #[serde(default)]
    pub debug_worm_state: bool,
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

/// Why a component bundle's folder is listed but not loaded, for now.
pub const BUNDLE_NOT_LOADED: &str = "this is a component bundle (format_version 2); bundles are not loaded yet";

/// A manifest's `id` alone, when the TOML parses even if the rest of the
/// manifest is refused.
fn manifest_id(src: &str) -> Option<String> {
    let table: toml::Table = toml::from_str(src).ok()?;
    table.get("id")?.as_str().map(str::to_owned)
}

pub fn parse_manifest(src: &str) -> Result<ModManifest, String> {
    // A component bundle would otherwise read as a pack with no claims and
    // the default entry, and be run as one.
    if components::declares_components(src) {
        return Err(BUNDLE_NOT_LOADED.into());
    }
    let mut m: ModManifest = toml::from_str(src).map_err(|e| e.to_string())?;
    if m.id.trim().is_empty() {
        return Err("mod.toml id is empty".into());
    }
    if m.api_version != API_VERSION {
        return Err(format!(
            "mod {} api_version {} is not supported (need {API_VERSION})",
            m.id, m.api_version
        ));
    }
    // Rule packs often omit `kind`. Empty weapon claims ⇒ gameplay so the
    // loader can put them on the Gameplay tab without requiring a new field
    // on every such pack — unless the author set kind explicitly to weapons.
    if m.kind == PackKind::Weapons
        && m.weapons.replace.is_empty()
        && m.weapons.overlay.is_empty()
        && m.weapons.register.is_empty()
        && !src.contains("kind")
    {
        m.kind = PackKind::Gameplay;
    }
    Ok(m)
}

/// Loader settings next to the packs: `Mods/wormforge.toml`. A disabled pack
/// stays on disk (so an updater can still diff it) but is never scanned in.
pub const SETTINGS_FILE: &str = "wormforge.toml";

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct LoaderSettings {
    /// Mod ids (not folder names) the player switched off in the loader.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// id → sha256 of the pack archive the loader installed. Lets the
    /// loader tell "up to date" from "update available" without version bumps.
    #[serde(default)]
    pub installed: std::collections::BTreeMap<String, String>,
    /// The component selection (`[profile]`), kept as written so a save by the
    /// loader window never loses it; read with `components::ProfileSpec`.
    #[serde(default)]
    pub profile: Option<toml::Table>,
}

impl LoaderSettings {
    pub fn load(mods_dir: &Path) -> LoaderSettings {
        let path = mods_dir.join(SETTINGS_FILE);
        let Ok(src) = std::fs::read_to_string(&path) else {
            return LoaderSettings::default();
        };
        toml::from_str(&src).unwrap_or_default()
    }

    pub fn save(&self, mods_dir: &Path) -> Result<(), String> {
        let mut out = String::from(
            "# WormForge mod loader. Packs listed here stay installed but are not loaded.\n\
             # Edited by the in-game loader window; safe to edit by hand.\n",
        );
        let mut ids = self.disabled.clone();
        ids.sort();
        ids.dedup();
        out.push_str("disabled = [");
        for (i, id) in ids.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{id:?}"));
        }
        out.push_str("]\n");
        if !self.installed.is_empty() {
            out.push_str("\n# Archive hashes of packs installed by the loader (id = sha256).\n[installed]\n");
            for (id, sha) in &self.installed {
                out.push_str(&format!("{id:?} = {sha:?}\n"));
            }
        }
        if let Some(profile) = &self.profile {
            let mut wrapper = toml::Table::new();
            wrapper.insert("profile".into(), toml::Value::Table(profile.clone()));
            let text = toml::to_string(&wrapper).map_err(|e| e.to_string())?;
            out.push_str("\n# The component selection: scheme, weapons, features, options.\n");
            out.push_str(&text);
        }
        std::fs::create_dir_all(mods_dir).map_err(|e| e.to_string())?;
        // Not std::fs::write: that truncates the existing file in place, which
        // the Wine in the macOS WA bundles does not implement. See
        // game::overwrite.
        game::overwrite::write(&mods_dir.join(SETTINGS_FILE), out.as_bytes())
            .map_err(|e| e.to_string())
    }

    pub fn is_disabled(&self, id: &str) -> bool {
        self.disabled.iter().any(|d| d == id)
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) {
        self.disabled.retain(|d| d != id);
        if !enabled {
            self.disabled.push(id.to_string());
        }
    }
}

/// A pack folder as the loader window sees it: parsed or broken, on or off.
#[derive(Debug, Clone)]
pub struct ModEntry {
    pub folder: String,
    pub root: PathBuf,
    /// `Err` keeps the manifest error so the loader can show it instead of
    /// silently hiding a pack with a typo in `mod.toml`.
    pub manifest: Result<ModManifest, String>,
    pub enabled: bool,
}

/// Folders the scanner never looks at: `_name`, `.name`. A nested folder such
/// as `Mods/ModsOFF/<pack>` is already ignored because the scan is not
/// recursive; the prefix rule is for people who rename in place.
pub fn is_ignored_folder(name: &str) -> bool {
    name.starts_with('_') || name.starts_with('.')
}

/// Every pack folder under `Mods/`, including disabled and broken ones,
/// sorted by folder name. Does not apply `requires` or id dedupe.
pub fn list_mods(mods_dir: &Path) -> Result<Vec<ModEntry>, String> {
    let settings = LoaderSettings::load(mods_dir);
    let mut out = Vec::new();
    if !mods_dir.is_dir() {
        return Ok(out);
    }
    let entries = std::fs::read_dir(mods_dir).map_err(|e| e.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let folder = entry.file_name().to_string_lossy().into_owned();
        if is_ignored_folder(&folder) {
            continue;
        }
        let manifest_path = path.join("mod.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest = std::fs::read_to_string(&manifest_path)
            .map_err(|e| e.to_string())
            .and_then(|src| parse_manifest(&src));
        let enabled = match &manifest {
            Ok(m) => !settings.is_disabled(&m.id),
            Err(_) => false,
        };
        out.push(ModEntry {
            folder,
            root: path,
            manifest,
            enabled,
        });
    }
    out.sort_by(|a, b| a.folder.to_lowercase().cmp(&b.folder.to_lowercase()));
    Ok(out)
}

/// Online catalogue (`index.toml` next to one zip per pack). Produced by
/// `tools/publish-packs.ps1`; consumed by the loader's "Check online".
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PackIndex {
    #[serde(default)]
    pub schema: u32,
    #[serde(default)]
    pub generated: String,
    /// Absolute URL prefix each `file` is relative to.
    pub base: String,
    #[serde(default)]
    pub packs: Vec<RemotePack>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct RemotePack {
    pub id: String,
    pub folder: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default = "default_kind")]
    pub kind: PackKind,
    #[serde(default)]
    pub slots: Vec<String>,
    pub file: String,
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
}

pub const INDEX_SCHEMA: u32 = 1;

pub fn parse_index(src: &str) -> Result<PackIndex, String> {
    let idx: PackIndex = toml::from_str(src).map_err(|e| e.to_string())?;
    if idx.schema != INDEX_SCHEMA {
        return Err(format!(
            "pack index schema {} is not supported (need {INDEX_SCHEMA})",
            idx.schema
        ));
    }
    if idx.base.is_empty() {
        return Err("pack index has no base URL".into());
    }
    Ok(idx)
}

impl RemotePack {
    pub fn url(&self, index: &PackIndex) -> String {
        if self.file.contains("://") {
            return self.file.clone();
        }
        let base = index.base.trim_end_matches('/');
        format!("{base}/{}", self.file)
    }
}

/// Two or more enabled packs declaring the same stock slot. The first id in
/// `claimants` (sorted, i.e. load order) is the one whose claim wins.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotConflict {
    pub slot: String,
    pub claimants: Vec<String>,
}

/// Slot conflicts among the enabled, parseable packs. Manifest `replace` /
/// `overlay` lists are authoritative: Lua claims outside them are rejected at
/// load time, so this static check matches what a match would do.
pub fn slot_conflicts(entries: &[ModEntry]) -> Vec<SlotConflict> {
    let mut by_slot: Vec<(String, Vec<String>)> = Vec::new();
    for e in entries.iter().filter(|e| e.enabled) {
        let Ok(m) = &e.manifest else {
            continue;
        };
        let names = m.weapons.replace.iter().chain(m.weapons.overlay.iter());
        for name in names {
            let slot = parse_stock_weapon(name)
                .map(|s| s.name().to_string())
                .unwrap_or_else(|_| name.clone());
            match by_slot.iter_mut().find(|(s, _)| *s == slot) {
                Some((_, ids)) => {
                    if !ids.contains(&m.id) {
                        ids.push(m.id.clone());
                    }
                }
                None => by_slot.push((slot, vec![m.id.clone()])),
            }
        }
    }
    let mut out: Vec<SlotConflict> = by_slot
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(slot, mut ids)| {
            ids.sort();
            SlotConflict { slot, claimants: ids }
        })
        .collect();
    out.sort_by(|a, b| a.slot.cmp(&b.slot));
    out
}

/// One-line conflict summary for a given pack, or empty. Names every other
/// pack on each contested slot so the owner of a hundred packs can act on it.
pub fn conflict_text(conflicts: &[SlotConflict], id: &str) -> String {
    let mut parts = Vec::new();
    for c in conflicts.iter().filter(|c| c.claimants.iter().any(|x| x == id)) {
        let others: Vec<&str> = c
            .claimants
            .iter()
            .filter(|x| *x != id)
            .map(String::as_str)
            .collect();
        if c.claimants[0] == id {
            parts.push(format!("wins {} over {}", c.slot, others.join(", ")));
        } else {
            parts.push(format!("loses {} to {}", c.slot, c.claimants[0]));
        }
    }
    parts.join("; ")
}

/// Discover `Mods/*/mod.toml`, drop packs disabled in `wormforge.toml`, then
/// sort by mod id (not directory order). A broken manifest is still an error
/// here: a pack the player left enabled must not vanish silently. One the
/// player disabled is skipped, though, when its id can still be read (say,
/// a pack for another api_version): it was never going to run.
pub fn discover_mods(mods_dir: &Path) -> Result<Vec<LoadedMod>, String> {
    let settings = LoaderSettings::load(mods_dir);
    let mut mods = Vec::new();
    for entry in list_mods(mods_dir)? {
        let manifest = match entry.manifest {
            Ok(m) => m,
            // Listed for the loader to show; not a pack the match runs.
            Err(e) if e == BUNDLE_NOT_LOADED => continue,
            Err(e) => {
                let disabled = std::fs::read_to_string(entry.root.join("mod.toml"))
                    .ok()
                    .and_then(|src| manifest_id(&src))
                    .is_some_and(|id| settings.is_disabled(&id));
                if disabled {
                    continue;
                }
                return Err(e);
            }
        };
        if !entry.enabled {
            continue;
        }
        mods.push(LoadedMod {
            manifest,
            root: entry.root,
        });
    }
    mods.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(dedupe_mod_ids(mods).0)
}

/// One `id@version` per enabled pack, sorted. The wire form of the modset
/// exchanged between peers (see engine `sync`).
pub fn modset_parts(mods: &[LoadedMod]) -> Vec<String> {
    let mut parts: Vec<String> = mods
        .iter()
        .map(|m| format!("{}@{}", m.manifest.id, m.manifest.version))
        .collect();
    parts.sort();
    parts
}

/// Human-readable difference between two fingerprints (`modset_fingerprint`
/// strings). `None` when they match.
pub fn modset_diff(local: &str, remote: &str) -> Option<String> {
    if local == remote {
        return None;
    }
    let split = |s: &str| -> Vec<(String, String)> {
        s.split(';')
            .filter(|p| !p.is_empty())
            .map(|p| match p.split_once('@') {
                Some((id, v)) => (id.to_string(), v.to_string()),
                None => (p.to_string(), String::new()),
            })
            .collect()
    };
    let l = split(local);
    let r = split(remote);
    let mut only_local = Vec::new();
    let mut only_remote = Vec::new();
    let mut versions = Vec::new();
    for (id, v) in &l {
        match r.iter().find(|(rid, _)| rid == id) {
            None => only_local.push(id.clone()),
            Some((_, rv)) if rv != v => versions.push(format!("{id} (you {v}, peer {rv})")),
            _ => {}
        }
    }
    for (id, _) in &r {
        if !l.iter().any(|(lid, _)| lid == id) {
            only_remote.push(id.clone());
        }
    }
    let mut out = Vec::new();
    if !only_local.is_empty() {
        out.push(format!("only you: {}", only_local.join(", ")));
    }
    if !only_remote.is_empty() {
        out.push(format!("only peer: {}", only_remote.join(", ")));
    }
    if !versions.is_empty() {
        out.push(format!("version: {}", versions.join(", ")));
    }
    Some(out.join("; "))
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
    modset_parts(mods).join(";")
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
    #[test]
    fn api_v1_is_refused_after_the_pixel_migration() {
        let err = parse_manifest("id = \"old.pack\"\napi_version = 1\n").unwrap_err();
        assert!(err.contains("api_version 1 is not supported (need 2)"));
    }

    #[test]
    fn a_save_keeps_the_profile() {
        let root = std::env::temp_dir().join("wf-weapons-settings-profile");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(SETTINGS_FILE),
            "disabled = []\n[profile]\nscheme = \"hl.highlander\"\n[profile.weapons]\n\"core.teleport\" = { row = \"F8\", column = 4 }\n",
        )
        .unwrap();
        let mut settings = LoaderSettings::load(&root);
        settings.set_enabled("some.pack", false);
        settings.save(&root).unwrap();
        let again = LoaderSettings::load(&root);
        assert_eq!(again.disabled, ["some.pack"]);
        assert_eq!(again.profile, settings.profile);
        let spec = components::ProfileSpec::from_table(again.profile.as_ref().unwrap()).unwrap();
        assert_eq!(spec.scheme.as_deref(), Some("hl.highlander"));
        assert_eq!(spec.weapons.len(), 1);
    }

    #[test]
    fn a_component_bundle_is_listed_but_not_loaded_as_a_pack() {
        let root = std::env::temp_dir().join("wf-weapons-bundle-skip");
        let _ = std::fs::remove_dir_all(&root);
        for (folder, toml) in [
            ("old", "id = \"old.pack\"\napi_version = 2\nentry = \"init.lua\"\n"),
            ("new", "id = \"new.bundle\"\napi_version = 2\nformat_version = 2\n[[components]]\nid = \"a.b\"\ntype = \"feature\"\nentry = \"a.lua\"\n"),
        ] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
            std::fs::write(root.join(folder).join("mod.toml"), toml).unwrap();
        }
        let listed = list_mods(&root).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|e| e.manifest.as_ref().err().map(String::as_str) == Some(BUNDLE_NOT_LOADED)));
        let loaded = discover_mods(&root).unwrap();
        assert_eq!(loaded.iter().map(|m| m.manifest.id.as_str()).collect::<Vec<_>>(), ["old.pack"]);
    }

    use super::*;

    #[test]
    fn manifest_parses_overlay_slots() {
        let src = r#"
id = "example.ice_bazooka"
api_version = 2
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
api_version = 2
entry = "weapons.lua"
[actors]
register = ["example.boss.mole"]
"#;
        let m = parse_manifest(src).unwrap();
        assert_eq!(m.actors.register, vec!["example.boss.mole"]);
    }

    fn write_pack(dir: &Path, folder: &str, id: &str) {
        let root = dir.join(folder);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("mod.toml"),
            format!("id = \"{id}\"\napi_version = 2\nentry = \"weapons.lua\"\n"),
        )
        .unwrap();
    }

    fn temp_mods_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wf-weapons-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn disabled_settings_and_ignored_folders_are_skipped() {
        let dir = temp_mods_dir("disabled");
        write_pack(&dir, "alpha", "x.alpha");
        write_pack(&dir, "beta", "x.beta");
        write_pack(&dir, "_parked", "x.parked");
        write_pack(&dir, "ModsOFF", "x.off");
        std::fs::write(dir.join("ModsOFF").join("mod.toml"), "not toml = [").unwrap();

        let mut settings = LoaderSettings::load(&dir);
        assert!(settings.disabled.is_empty());
        settings.set_enabled("x.beta", false);
        settings.set_enabled("x.off", false);
        settings.installed.insert("x.alpha".into(), "ab".repeat(32));
        settings.save(&dir).unwrap();
        assert_eq!(LoaderSettings::load(&dir), settings);

        let all = list_mods(&dir).unwrap();
        let folders: Vec<_> = all.iter().map(|e| e.folder.as_str()).collect();
        assert_eq!(folders, vec!["alpha", "beta", "ModsOFF"]);
        assert!(all[0].enabled);
        assert!(!all[1].enabled);
        assert!(all[2].manifest.is_err() && !all[2].enabled);

        // A broken manifest still fails discovery loudly …
        assert!(discover_mods(&dir).is_err());
        // … unless its folder is parked out of the way.
        std::fs::rename(dir.join("ModsOFF"), dir.join("_ModsOFF")).unwrap();
        let ids: Vec<_> = discover_mods(&dir)
            .unwrap()
            .into_iter()
            .map(|m| m.manifest.id)
            .collect();
        assert_eq!(ids, vec!["x.alpha"]);

        settings.set_enabled("x.beta", true);
        settings.save(&dir).unwrap();
        assert_eq!(discover_mods(&dir).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_disabled_pack_with_a_refused_manifest_does_not_stop_discovery() {
        let dir = temp_mods_dir("refused");
        write_pack(&dir, "alpha", "x.alpha");
        let newer = dir.join("newer");
        std::fs::create_dir_all(&newer).unwrap();
        std::fs::write(newer.join("mod.toml"), "id = \"x.newer\"\napi_version = 99\nentry = \"init.lua\"\n")
            .unwrap();
        // Enabled, it still stops discovery, naming why.
        let err = discover_mods(&dir).unwrap_err();
        assert!(err.contains("x.newer") && err.contains("api_version 99"), "{err}");
        // Disabled, it is skipped and the others load.
        let mut settings = LoaderSettings::load(&dir);
        settings.set_enabled("x.newer", false);
        settings.save(&dir).unwrap();
        let ids: Vec<_> = discover_mods(&dir).unwrap().into_iter().map(|m| m.manifest.id).collect();
        assert_eq!(ids, vec!["x.alpha"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn slot_conflicts_name_every_claimant() {
        let entry = |id: &str, replace: &str, enabled: bool| ModEntry {
            folder: id.to_string(),
            root: PathBuf::new(),
            manifest: parse_manifest(&format!(
                "id = \"{id}\"\napi_version = 2\n[weapons]\nreplace = [{replace}]\n"
            )),
            enabled,
        };
        let entries = vec![
            entry("z.late", "\"bazooka\", \"grenade\"", true),
            entry("a.first", "\"bazooka\"", true),
            entry("m.mid", "\"Bazooka\"", true),
            entry("x.off", "\"bazooka\"", false),
            entry("g.only", "\"grenade\"", true),
        ];
        let c = slot_conflicts(&entries);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].slot, "bazooka");
        assert_eq!(c[0].claimants, vec!["a.first", "m.mid", "z.late"]);
        assert_eq!(c[1].claimants, vec!["g.only", "z.late"]);
        assert_eq!(conflict_text(&c, "a.first"), "wins bazooka over m.mid, z.late");
        assert_eq!(
            conflict_text(&c, "z.late"),
            "loses bazooka to a.first; loses grenade to g.only"
        );
        assert_eq!(conflict_text(&c, "x.off"), "");
    }

    #[test]
    fn pack_index_parses_and_resolves_urls() {
        let src = r#"
schema = 1
generated = "2026-09-23T22:00:00Z"
base = "https://example.org/downloads/mods/"

[[packs]]
id = "example.worm_toss"
folder = "worm_toss"
version = "0.1.0"
author = "wkLua"
slots = ["battle_axe"]
file = "worm_toss.zip"
sha256 = "00"
size = 10

[[packs]]
id = "x.abs"
folder = "abs"
kind = "gameplay"
file = "https://cdn.example.org/abs.zip"
sha256 = "11"
"#;
        let idx = parse_index(src).unwrap();
        assert_eq!(idx.packs.len(), 2);
        assert_eq!(idx.packs[0].kind, PackKind::Weapons);
        assert_eq!(idx.packs[1].kind, PackKind::Gameplay);
        assert_eq!(idx.packs[0].url(&idx), "https://example.org/downloads/mods/worm_toss.zip");
        assert_eq!(idx.packs[1].url(&idx), "https://cdn.example.org/abs.zip");
        assert_eq!(idx.packs[1].version, "0.1.0");
        assert!(parse_index("schema = 2\nbase = \"x\"\n").is_err());
    }

    #[test]
    fn empty_claims_without_kind_are_gameplay() {
        let m = parse_manifest(
            "id = \"example.highlander\"\napi_version = 2\nentry = \"rules.lua\"\n[weapons]\nregister = []\nreplace = []\n",
        )
        .unwrap();
        assert_eq!(m.kind, PackKind::Gameplay);
        let m = parse_manifest(
            "id = \"example.keep\"\napi_version = 2\nkind = \"weapons\"\n[weapons]\nregister = []\n",
        )
        .unwrap();
        assert_eq!(m.kind, PackKind::Weapons);
        let m = parse_manifest(
            "id = \"example.gun\"\napi_version = 2\n[weapons]\nreplace = [\"bazooka\"]\n",
        )
        .unwrap();
        assert_eq!(m.kind, PackKind::Weapons);
    }

    #[test]
    fn modset_diff_names_the_difference() {
        assert_eq!(modset_diff("a@1;b@2", "a@1;b@2"), None);
        let d = modset_diff("a@1;b@2;c@1", "b@3;d@1").unwrap();
        assert!(d.contains("only you: a, c"), "{d}");
        assert!(d.contains("only peer: d"), "{d}");
        assert!(d.contains("b (you 2, peer 3)"), "{d}");
        assert_eq!(modset_diff("", "a@1").unwrap(), "only peer: a");
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
