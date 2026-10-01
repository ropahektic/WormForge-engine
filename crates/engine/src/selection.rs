//! The component selection a round runs, and what to freeze with it.
//!
//! A round loading from a synced session or a replay's packs runs the
//! selection frozen there ([`weapons::components::PROFILE_PACK`]); a local
//! round runs `[profile]` from the loader settings. Either way the selection
//! is resolved against the bundles in the same folder, so every peer and
//! every playback resolves the same bundles to the same components.

use crate::log::{log_error, log_line};
use lua_host::ComponentPlan;
use std::path::Path;
use weapons::components::{list_bundles, profile_table, resolve, write_profile_pack, PackFolder, ProfileSpec};

pub struct Selection {
    /// What to run; None when there is no selection or it does not resolve.
    pub plan: Option<ComponentPlan>,
    /// What to freeze or archive with the packs: the bundles the selection
    /// uses and the selection itself, written under the stage folder.
    pub folders: Vec<PackFolder>,
}

/// Resolve the selection for a round loading from `mods_dir`, logging every
/// problem; `stage` is where the frozen selection is written.
pub fn load(mods_dir: &Path, stage: &Path) -> Selection {
    let none = Selection { plan: None, folders: Vec::new() };
    let table = match profile_table(mods_dir) {
        Ok(Some(t)) => t,
        Ok(None) => return none,
        Err(e) => {
            log_error(&format!("profile: {e}"));
            return none;
        }
    };
    let (bundles, broken) = list_bundles(mods_dir);
    for e in &broken {
        log_error(&format!("component bundle {e}"));
    }
    let profile = match ProfileSpec::from_table(&table).map_err(|e| vec![e]).and_then(|s| s.to_profile()) {
        Ok(p) => p,
        Err(errors) => {
            for e in errors {
                log_error(&format!("profile: {e}"));
            }
            return none;
        }
    };
    let manifests: Vec<_> = bundles.iter().map(|b| b.manifest.clone()).collect();
    let resolved = match resolve(&manifests, &profile) {
        Ok(r) => r,
        Err(errors) => {
            log_error("profile does not resolve; loading packs without components:");
            for e in errors {
                log_error(&format!("  {e}"));
            }
            return none;
        }
    };
    if resolved.is_empty() {
        return none;
    }
    for line in resolved.canonical().lines() {
        log_line(&format!("profile {line}"));
    }
    let used: std::collections::BTreeSet<&str> = resolved.load_order().iter().map(|c| c.bundle.as_str()).collect();
    let mut folders: Vec<PackFolder> = bundles
        .iter()
        .filter(|b| used.contains(b.manifest.id.as_str()))
        .map(|b| PackFolder { id: b.manifest.id.clone(), version: b.manifest.version.clone(), root: b.root.clone() })
        .collect();
    match write_profile_pack(stage, &table) {
        Ok(f) => folders.push(f),
        Err(e) => log_error(&format!("profile: cannot stage the selection for freezing: {e}")),
    }
    let plan = ComponentPlan {
        resolved,
        roots: bundles.into_iter().map(|b| (b.manifest.id, b.root)).collect(),
    };
    Selection { plan: Some(plan), folders }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A Mods folder holding a copy of the component_demo example bundle.
    fn mods_with_demo(name: &str) -> PathBuf {
        let mods = std::env::temp_dir().join(format!("wf-selection-{name}"));
        let _ = std::fs::remove_dir_all(&mods);
        let demo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/examples/component_demo");
        for rel in ["mod.toml", "scheme.lua", "weapons/big_bazooka.lua", "features/turn_note.lua"] {
            let to = mods.join("component_demo").join(rel);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(demo.join(rel), to).unwrap();
        }
        mods
    }

    #[test]
    fn a_local_selection_resolves_and_lists_what_to_freeze() {
        let mods = mods_with_demo("local");
        std::fs::write(mods.join("wormforge.toml"), "[profile]\nscheme = \"demo.scheme\"\n").unwrap();
        let stage = mods.join("stage");
        let s = load(&mods, &stage);
        let plan = s.plan.unwrap();
        assert_eq!(plan.resolved.weapons[0].slot, weapons::StockWeapon::Bazooka);
        let ids: Vec<_> = s.folders.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["example.components", "wormforge.profile"]);
        let staged = std::fs::read_to_string(stage.join("wormforge.profile/profile.toml")).unwrap();
        assert_eq!(staged, "scheme = \"demo.scheme\"\n");
    }

    #[test]
    fn a_frozen_selection_wins_and_a_bad_one_runs_nothing() {
        let mods = mods_with_demo("frozen");
        std::fs::write(mods.join("wormforge.toml"), "[profile]\nscheme = \"not.installed\"\n").unwrap();
        assert!(load(&mods, &mods.join("stage")).plan.is_none(), "the local selection does not resolve");
        std::fs::create_dir_all(mods.join("wormforge.profile")).unwrap();
        std::fs::write(mods.join("wormforge.profile/profile.toml"), "scheme = \"demo.scheme\"\n").unwrap();
        let s = load(&mods, &mods.join("stage"));
        assert!(s.plan.is_some(), "the frozen selection is the one run");
        std::fs::write(mods.join("wormforge.profile/profile.toml"), "features = [\"nope\"]\n").unwrap();
        let s = load(&mods, &mods.join("stage"));
        assert!(s.plan.is_none() && s.folders.is_empty());
    }
}
