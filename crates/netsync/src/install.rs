//! After a peer finishes assembling a lobby sync session, copy those packs
//! into the WA `Mods/` tree so they show up in the ModLoader for later
//! offline / host games. Network rounds still load from the session freeze;
//! this is a permanent install of the bytes that arrived.

use crate::PACKS_DIR;
use std::fs;
use std::path::{Path, PathBuf};
use weapons::{is_ignored_folder, list_mods, parse_manifest, LoaderSettings};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct InstallReport {
    pub installed: Vec<String>,
    pub updated: Vec<String>,
    pub unchanged: Vec<String>,
    pub errors: Vec<(String, String)>,
}

/// Copy every pack under `session_dir/packs/<id>/` into `mods_dir`.
///
/// - New pack ids land in `Mods/<id>/` and are added to `disabled` in
///   `wormforge.toml` (off until the player enables them).
/// - An existing folder with the same id is updated in place when any file
///   differs; its enabled/disabled toggle is left alone.
/// - Identical content is left untouched.
pub fn install_session_into_mods(session_dir: &Path, mods_dir: &Path) -> Result<InstallReport, String> {
    let packs_root = session_dir.join(PACKS_DIR);
    if !packs_root.is_dir() {
        return Ok(InstallReport::default());
    }
    fs::create_dir_all(mods_dir).map_err(|e| format!("{}: {e}", mods_dir.display()))?;

    let existing = list_mods(mods_dir)?;
    let mut settings = LoaderSettings::load(mods_dir);
    let mut report = InstallReport::default();

    let mut pack_dirs = Vec::new();
    for entry in fs::read_dir(&packs_root).map_err(|e| format!("{}: {e}", packs_root.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            pack_dirs.push(path);
        }
    }
    pack_dirs.sort();

    for src in pack_dirs {
        let folder_name = src
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if folder_name.is_empty() || is_ignored_folder(&folder_name) {
            continue;
        }
        let toml = match fs::read_to_string(src.join("mod.toml")) {
            Ok(s) => s,
            Err(e) => {
                report.errors.push((folder_name, format!("read mod.toml: {e}")));
                continue;
            }
        };
        let manifest = match parse_manifest(&toml) {
            Ok(m) => m,
            Err(e) => {
                report.errors.push((folder_name, e));
                continue;
            }
        };
        let id = manifest.id.clone();

        let prior = existing.iter().find(|e| {
            e.manifest
                .as_ref()
                .ok()
                .is_some_and(|m| m.id == id)
        });

        if let Some(entry) = prior {
            if packs_match(&src, &entry.root) {
                report.unchanged.push(id);
                continue;
            }
            match replace_pack(&src, &entry.root, mods_dir, &id) {
                Ok(()) => report.updated.push(id),
                Err(e) => report.errors.push((id, e)),
            }
            continue;
        }

        let dest = free_folder(mods_dir, &id);
        match replace_pack(&src, &dest, mods_dir, &id) {
            Ok(()) => {
                settings.set_enabled(&id, false);
                report.installed.push(id);
            }
            Err(e) => report.errors.push((id, e)),
        }
    }

    if !report.installed.is_empty() || !report.updated.is_empty() {
        settings.save(mods_dir)?;
    }
    Ok(report)
}

fn free_folder(mods_dir: &Path, id: &str) -> PathBuf {
    let base = mods_dir.join(id);
    if !base.exists() {
        return base;
    }
    // Same folder name, different pack id already lives here — pick a free name.
    for n in 2..1000 {
        let candidate = mods_dir.join(format!("{id}-{n}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    mods_dir.join(format!("{id}-sync"))
}

fn packs_match(a: &Path, b: &Path) -> bool {
    let Ok(fa) = collect_files(a) else {
        return false;
    };
    let Ok(fb) = collect_files(b) else {
        return false;
    };
    if fa.len() != fb.len() {
        return false;
    }
    for (rel, path_a) in &fa {
        let Some(path_b) = fb.get(rel) else {
            return false;
        };
        let Ok(ba) = fs::read(path_a) else {
            return false;
        };
        let Ok(bb) = fs::read(path_b) else {
            return false;
        };
        if ba != bb {
            return false;
        }
    }
    true
}

fn collect_files(root: &Path) -> Result<std::collections::BTreeMap<String, PathBuf>, String> {
    let mut out = std::collections::BTreeMap::new();
    collect_walk(root, root, &mut out)?;
    Ok(out)
}

fn collect_walk(
    root: &Path,
    dir: &Path,
    out: &mut std::collections::BTreeMap<String, PathBuf>,
) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let ty = entry.file_type().map_err(|e| format!("{}: {e}", path.display()))?;
        if ty.is_dir() {
            collect_walk(root, &path, out)?;
        } else if ty.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.insert(rel, path);
        }
    }
    Ok(())
}

fn replace_pack(src: &Path, dest: &Path, mods_dir: &Path, id: &str) -> Result<(), String> {
    let staging = mods_dir.join(format!("_sync_{id}"));
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    }
    copy_dir(src, &staging)?;
    if dest.exists() {
        fs::remove_dir_all(dest).map_err(|e| format!("replace {}: {e}", dest.display()))?;
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::rename(&staging, dest).map_err(|e| {
        let _ = fs::remove_dir_all(&staging);
        format!("install {}: {e}", dest.display())
    })?;
    Ok(())
}

fn copy_dir(src: &Path, dest: &Path) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    for entry in fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let ty = entry.file_type().map_err(|e| format!("{}: {e}", from.display()))?;
        if ty.is_dir() {
            copy_dir(&from, &to)?;
        } else if ty.is_file() {
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            fs::copy(&from, &to).map_err(|e| format!("{} -> {}: {e}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{assemble, freeze};
    use weapons::{parse_manifest, LoadedMod};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wf-sync-install-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn pack(root: &Path, folder: &str, id: &str, files: &[(&str, &[u8])]) -> LoadedMod {
        let dir = root.join(folder);
        fs::create_dir_all(&dir).unwrap();
        let toml = format!(
            "id = \"{id}\"\nversion = \"1\"\napi_version = 1\nentry = \"w.lua\"\n"
        );
        fs::write(dir.join("mod.toml"), &toml).unwrap();
        for (path, bytes) in files {
            let target = dir.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, bytes).unwrap();
        }
        LoadedMod {
            manifest: parse_manifest(&toml).unwrap(),
            root: dir,
        }
    }

    #[test]
    fn new_packs_land_disabled_under_mods() {
        let root = scratch("new");
        let host = pack(&root.join("host"), "cool", "t.cool", &[("w.lua", b"print(1)")]);
        let session = freeze(&[host], &root.join("session")).unwrap();
        let mods = root.join("Mods");
        let report = install_session_into_mods(&session.dir, &mods).unwrap();
        assert_eq!(report.installed, ["t.cool"]);
        assert!(mods.join("t.cool/w.lua").is_file());
        let settings = LoaderSettings::load(&mods);
        assert!(settings.is_disabled("t.cool"));
    }

    #[test]
    fn identical_existing_pack_is_left_alone() {
        let root = scratch("same");
        let mods = root.join("Mods");
        let _ = pack(&mods, "cool", "t.cool", &[("w.lua", b"print(1)")]);
        let host = pack(&root.join("host"), "cool", "t.cool", &[("w.lua", b"print(1)")]);
        let session = freeze(&[host], &root.join("session")).unwrap();
        let before = LoaderSettings::load(&mods);
        let report = install_session_into_mods(&session.dir, &mods).unwrap();
        assert_eq!(report.unchanged, ["t.cool"]);
        assert!(report.installed.is_empty());
        assert_eq!(LoaderSettings::load(&mods), before);
    }

    #[test]
    fn differing_pack_updates_in_place_and_keeps_enabled() {
        let root = scratch("upd");
        let mods = root.join("Mods");
        let _ = pack(&mods, "cool-folder", "t.cool", &[("w.lua", b"old")]);
        let mut settings = LoaderSettings::load(&mods);
        settings.set_enabled("t.cool", true);
        settings.save(&mods).unwrap();

        let host = pack(&root.join("host"), "cool", "t.cool", &[("w.lua", b"new")]);
        let session = freeze(&[host], &root.join("session")).unwrap();
        let report = install_session_into_mods(&session.dir, &mods).unwrap();
        assert_eq!(report.updated, ["t.cool"]);
        assert_eq!(fs::read(mods.join("cool-folder/w.lua")).unwrap(), b"new");
        assert!(!LoaderSettings::load(&mods).is_disabled("t.cool"));
    }

    #[test]
    fn assemble_then_install_matches_freeze_path() {
        let root = scratch("assemble");
        let host = pack(
            &root.join("host"),
            "a",
            "t.a",
            &[("sprites/x.png", b"png"), ("w.lua", b"lua")],
        );
        let frozen = freeze(&[host], &root.join("host-session")).unwrap();
        let client = assemble(&frozen.manifest, &root.join("client-session"), |f| {
            fs::read(
                frozen
                    .dir
                    .join(PACKS_DIR)
                    .join(&f.pack)
                    .join(&f.path),
            )
            .map_err(|e| e.to_string())
        })
        .unwrap();
        let mods = root.join("Mods");
        let report = install_session_into_mods(&client.dir, &mods).unwrap();
        assert_eq!(report.installed, ["t.a"]);
        assert_eq!(fs::read(mods.join("t.a/sprites/x.png")).unwrap(), b"png");
    }
}
