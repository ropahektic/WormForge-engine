//! Host-to-client asset sync (`docs/network-asset-sync-plan.md`).
//!
//! When the host lights their ready bulb, the enabled pack set is frozen into
//! a session directory: a copy of every pack file plus a canonical manifest of
//! pack ids, paths, sizes and SHA-256 hashes. Later steps send that manifest
//! to clients and load network rounds from the frozen copy, so what the host
//! edits in `Mods/` afterwards cannot change a round already agreed on.
//!
//! A pack's files are every regular file under its folder, as the replay pack
//! table archives them; a symlink is refused rather than followed.

pub mod download;
pub mod gate;
pub mod install;
pub mod proto;
pub mod store;

use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use weapons::components::PackFolder;
use weapons::LoadedMod;

pub use assets::sha256::digest;
pub use install::{install_session_into_mods, InstallReport};

/// First line of every manifest. Bump it when the format changes.
pub const MANIFEST_HEADER: &str = "wormforge-sync-manifest 1";
/// The manifest's name inside a session directory.
pub const MANIFEST_FILE: &str = "manifest.txt";
/// Where the pack copies live inside a session directory, one folder per id.
pub const PACKS_DIR: &str = "packs";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrozenPack {
    pub id: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrozenFile {
    pub pack: String,
    /// Pack-relative, `/`-separated.
    pub path: String,
    pub size: u64,
    pub sha256: [u8; 32],
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    /// Sorted by id.
    pub packs: Vec<FrozenPack>,
    /// Sorted by pack, then path.
    pub files: Vec<FrozenFile>,
}

impl Manifest {
    /// The canonical text. Two machines holding the same bytes produce the
    /// same text, so its digest names the whole pack set.
    ///
    /// ```text
    /// wormforge-sync-manifest 1
    /// pack <id> <version>
    /// file <pack> <size> <sha256> <path>
    /// ```
    ///
    /// The free-form field is always last on its line, so it may hold spaces.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str(MANIFEST_HEADER);
        out.push('\n');
        for p in &self.packs {
            let _ = writeln!(out, "pack {} {}", p.id, p.version);
        }
        for f in &self.files {
            let _ = writeln!(out, "file {} {} {} {}", f.pack, f.size, hex(&f.sha256), f.path);
        }
        out
    }

    pub fn digest(&self) -> [u8; 32] {
        digest(self.to_text().as_bytes())
    }

    /// Read a manifest another machine sent. Only the canonical form is
    /// accepted -- sorted, no duplicates, every path a plain relative one --
    /// so a manifest that parses names exactly the bytes its digest covers.
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let mut lines = text.split_inclusive('\n');
        if lines.next() != Some(&format!("{MANIFEST_HEADER}\n")) {
            return Err("not a WormForge sync manifest, or a newer format".into());
        }
        let mut manifest = Manifest::default();
        for line in lines {
            let line = line
                .strip_suffix('\n')
                .ok_or("manifest does not end with a newline")?;
            if let Some(rest) = line.strip_prefix("pack ") {
                let (id, version) = rest.split_once(' ').ok_or("pack line without a version")?;
                check_id(id)?;
                check_field("version", version)?;
                manifest.packs.push(FrozenPack { id: id.into(), version: version.into() });
            } else if let Some(rest) = line.strip_prefix("file ") {
                let mut parts = rest.splitn(4, ' ');
                let (Some(pack), Some(size), Some(sha), Some(path)) =
                    (parts.next(), parts.next(), parts.next(), parts.next())
                else {
                    return Err(format!("short file line: {line:?}"));
                };
                check_path(path)?;
                manifest.files.push(FrozenFile {
                    pack: pack.into(),
                    path: path.into(),
                    size: size.parse().map_err(|_| format!("bad size in {line:?}"))?,
                    sha256: parse_hex(sha).ok_or_else(|| format!("bad hash in {line:?}"))?,
                });
            } else {
                return Err(format!("unknown manifest line: {line:?}"));
            }
        }
        for f in &manifest.files {
            if !manifest.packs.iter().any(|p| p.id == f.pack) {
                return Err(format!("file {} names undeclared pack {}", f.path, f.pack));
            }
        }
        if manifest.to_text() != text {
            return Err("manifest is not in canonical order".into());
        }
        let sorted_unique = |a: &[(&str, &str)]| a.windows(2).all(|w| w[0] < w[1]);
        let packs: Vec<_> = manifest.packs.iter().map(|p| (p.id.as_str(), "")).collect();
        let files: Vec<_> =
            manifest.files.iter().map(|f| (f.pack.as_str(), f.path.as_str())).collect();
        if !sorted_unique(&packs) || !sorted_unique(&files) {
            return Err("manifest lists a pack or file out of order or twice".into());
        }
        Ok(manifest)
    }

    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// A published session directory.
#[derive(Clone, Debug)]
pub struct Session {
    pub dir: PathBuf,
    pub manifest: Manifest,
}

pub fn parse_hex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Copy `mods` into a new session directory at `dest` and write its manifest.
///
/// The copy is built beside `dest` and renamed into place only once complete,
/// so a session directory that exists is always whole. `dest` must not exist.
pub fn freeze(mods: &[LoadedMod], dest: &Path) -> Result<Session, String> {
    freeze_with(mods, &[], dest)
}

/// [`freeze`] `mods` and `extra` folders together: component bundles and the
/// frozen selection, which are no loaded packs but travel as packs do.
pub fn freeze_with(mods: &[LoadedMod], extra: &[PackFolder], dest: &Path) -> Result<Session, String> {
    if dest.exists() {
        return Err(format!("session directory already exists: {}", dest.display()));
    }
    let staging = staging_path(dest)?;
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    }
    let mut folders: Vec<PackFolder> = mods
        .iter()
        .map(|m| PackFolder { id: m.manifest.id.clone(), version: m.manifest.version.clone(), root: m.root.clone() })
        .collect();
    folders.extend(extra.iter().cloned());
    let result = build(&folders, &staging).and_then(|manifest| {
        fs::rename(&staging, dest)
            .map_err(|e| format!("publish {}: {e}", dest.display()))?;
        Ok(Session { dir: dest.to_path_buf(), manifest })
    });
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

/// Build a session directory at `dest` holding exactly `manifest`'s files,
/// taking each file's bytes from `fetch`. Every file is checked against its
/// size and hash before it is written; like [`freeze`], the directory is
/// staged and appears only once whole.
pub fn assemble(
    manifest: &Manifest,
    dest: &Path,
    mut fetch: impl FnMut(&FrozenFile) -> Result<Vec<u8>, String>,
) -> Result<Session, String> {
    if dest.exists() {
        return Err(format!("session directory already exists: {}", dest.display()));
    }
    let staging = staging_path(dest)?;
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    }
    let result = (|| {
        fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
        for f in &manifest.files {
            check_id(&f.pack)?;
            check_path(&f.path)?;
            let bytes = fetch(f)?;
            if bytes.len() as u64 != f.size || digest(&bytes) != f.sha256 {
                return Err(format!("{}:{} does not match the manifest", f.pack, f.path));
            }
            let target = staging.join(PACKS_DIR).join(&f.pack).join(&f.path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            fs::write(&target, &bytes).map_err(|e| format!("{}: {e}", target.display()))?;
        }
        let manifest_path = staging.join(MANIFEST_FILE);
        fs::write(&manifest_path, manifest.to_text())
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        fs::rename(&staging, dest).map_err(|e| format!("publish {}: {e}", dest.display()))?;
        Ok(Session { dir: dest.to_path_buf(), manifest: manifest.clone() })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn staging_path(dest: &Path) -> Result<PathBuf, String> {
    let name = dest
        .file_name()
        .ok_or_else(|| format!("session directory has no name: {}", dest.display()))?;
    let mut staging = name.to_os_string();
    staging.push(".partial");
    Ok(dest.with_file_name(staging))
}

fn build(folders: &[PackFolder], staging: &Path) -> Result<Manifest, String> {
    let mut sorted: Vec<&PackFolder> = folders.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    let mut manifest = Manifest::default();
    for m in sorted {
        let id = &m.id;
        check_id(id)?;
        check_field("version", &m.version)?;
        if manifest.packs.iter().any(|p| &p.id == id) {
            return Err(format!("pack id {id} appears twice"));
        }
        manifest.packs.push(FrozenPack { id: id.clone(), version: m.version.clone() });

        let mut files = Vec::new();
        collect(&m.root, &m.root, &mut files)?;
        files.sort();
        let pack_dir = staging.join(PACKS_DIR).join(id);
        for (path, source) in files {
            let bytes = fs::read(&source).map_err(|e| format!("{}: {e}", source.display()))?;
            let target = pack_dir.join(&path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            fs::write(&target, &bytes).map_err(|e| format!("{}: {e}", target.display()))?;
            manifest.files.push(FrozenFile {
                pack: id.clone(),
                path,
                size: bytes.len() as u64,
                sha256: digest(&bytes),
            });
        }
    }
    fs::create_dir_all(staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let manifest_path = staging.join(MANIFEST_FILE);
    fs::write(&manifest_path, manifest.to_text())
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    Ok(manifest)
}

/// Every regular file under `dir`, as (pack-relative `/` path, source path).
fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let ty = entry.file_type().map_err(|e| format!("{}: {e}", path.display()))?;
        if ty.is_symlink() {
            return Err(format!("pack contains a symlink: {}", path.display()));
        }
        if ty.is_dir() {
            collect(root, &path, out)?;
        } else if ty.is_file() {
            let relative = path.strip_prefix(root).map_err(|e| e.to_string())?;
            let mut parts = Vec::new();
            for component in relative.components() {
                let Component::Normal(part) = component else {
                    return Err(format!("invalid pack path: {}", relative.display()));
                };
                let part = part
                    .to_str()
                    .ok_or_else(|| format!("pack path is not UTF-8: {}", relative.display()))?;
                parts.push(part.to_string());
            }
            let joined = parts.join("/");
            check_path(&joined)?;
            out.push((joined, path));
        }
    }
    Ok(())
}

/// Ids name folders in the session directory and fields in the manifest. A
/// leading '.' or '_' would make the loader skip the folder (`is_ignored_folder`).
fn check_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty()
        && !id.starts_with(['.', '_'])
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(format!("pack id {id:?} cannot be synced: use letters, digits, '.', '_' or '-'"))
    }
}

/// A path from another machine becomes a path on this one: plain relative
/// components only, nothing Windows would read as a drive or a separator.
pub fn check_path(path: &str) -> Result<(), String> {
    check_field("path", path)?;
    let ok = !path.is_empty()
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.contains(['\\', ':'])
                && !part.ends_with([' ', '.'])
        });
    if ok {
        Ok(())
    } else {
        Err(format!("unsafe pack path {path:?}"))
    }
}

/// A manifest line ends at a newline, so a field may not contain one.
fn check_field(what: &str, value: &str) -> Result<(), String> {
    if value.chars().any(char::is_control) {
        return Err(format!("{what} {value:?} contains a control character"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use weapons::parse_manifest;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wf-netsync-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn pack(root: &Path, folder: &str, id: &str, files: &[(&str, &[u8])]) -> LoadedMod {
        let dir = root.join(folder);
        fs::create_dir_all(&dir).unwrap();
        let toml = format!("id = \"{id}\"\nversion = \"1.0\"\napi_version = 2\nentry = \"weapons.lua\"\n");
        fs::write(dir.join("mod.toml"), &toml).unwrap();
        for (path, bytes) in files {
            let target = dir.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, bytes).unwrap();
        }
        LoadedMod { manifest: parse_manifest(&toml).unwrap(), root: dir }
    }

    #[test]
    fn bundles_and_the_frozen_selection_travel_as_packs_but_are_not_installed() {
        let root = scratch("extras");
        let a = pack(&root, "a", "a.pack", &[("weapons.lua", b"-- a")]);
        let bundle = root.join("b");
        fs::create_dir_all(&bundle).unwrap();
        fs::write(bundle.join("mod.toml"), "id = \"b.bundle\"\napi_version = 2\nformat_version = 2\n").unwrap();
        fs::write(bundle.join("w.lua"), "return {}").unwrap();
        let selection = root.join("stage").join(weapons::components::PROFILE_PACK);
        fs::create_dir_all(&selection).unwrap();
        fs::write(selection.join(weapons::components::PROFILE_FILE), "scheme = \"x\"\n").unwrap();
        let extras = [
            PackFolder { id: "b.bundle".into(), version: "1.0.0".into(), root: bundle },
            PackFolder { id: weapons::components::PROFILE_PACK.into(), version: "1".into(), root: selection },
        ];
        let session = freeze_with(&[a], &extras, &root.join("session")).unwrap();
        let ids: Vec<_> = session.manifest.packs.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["a.pack", "b.bundle", "wormforge.profile"]);
        let frozen = session.dir.join(PACKS_DIR).join("wormforge.profile").join("profile.toml");
        assert_eq!(fs::read_to_string(frozen).unwrap(), "scheme = \"x\"\n");

        let mods = root.join("Mods");
        let report = install_session_into_mods(&session.dir, &mods).unwrap();
        assert_eq!(report.installed, ["a.pack"]);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!mods.join("wormforge.profile").exists());
    }

    #[test]
    fn freezes_every_file_with_a_canonical_manifest() {
        let root = scratch("freeze");
        let b = pack(&root, "zeta", "t.b", &[("weapons.lua", b"b"), ("sprites/x y.png", b"png")]);
        let a = pack(&root, "alpha", "t.a", &[("weapons.lua", b"a")]);
        let dest = root.join("session");
        let session = freeze(&[b, a], &dest).unwrap();

        let ids: Vec<_> = session.manifest.packs.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["t.a", "t.b"], "packs sorted by id, not by folder");
        let paths: Vec<_> = session
            .manifest
            .files
            .iter()
            .map(|f| format!("{}:{}", f.pack, f.path))
            .collect();
        assert_eq!(
            paths,
            ["t.a:mod.toml", "t.a:weapons.lua", "t.b:mod.toml", "t.b:sprites/x y.png", "t.b:weapons.lua"]
        );
        assert_eq!(fs::read(dest.join("packs/t.b/sprites/x y.png")).unwrap(), b"png");
        let text = fs::read_to_string(dest.join(MANIFEST_FILE)).unwrap();
        assert_eq!(text, session.manifest.to_text());
        assert!(text.starts_with("wormforge-sync-manifest 1\npack t.a 1.0\npack t.b 1.0\n"));
        assert!(text.contains(&format!("file t.b 3 {} sprites/x y.png\n", hex(&digest(b"png")))));
        assert!(!root.join("session.partial").exists(), "staging is gone once published");
    }

    #[test]
    fn same_bytes_same_digest_and_any_change_shows() {
        let root = scratch("digest");
        let a = pack(&root, "a", "t.a", &[("weapons.lua", b"one")]);
        let first = freeze(&[a.clone()], &root.join("s1")).unwrap().manifest.digest();
        let again = freeze(&[a.clone()], &root.join("s2")).unwrap().manifest.digest();
        assert_eq!(first, again);
        // Same version in mod.toml, different bytes: the version is no proof.
        fs::write(a.root.join("weapons.lua"), b"two").unwrap();
        let changed = freeze(&[a], &root.join("s3")).unwrap().manifest.digest();
        assert_ne!(first, changed);
    }

    #[test]
    fn an_existing_session_is_never_overwritten() {
        let root = scratch("exists");
        let a = pack(&root, "a", "t.a", &[]);
        let dest = root.join("session");
        freeze(&[a.clone()], &dest).unwrap();
        let err = freeze(&[a], &dest).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn a_failed_freeze_leaves_nothing_behind() {
        let root = scratch("failed");
        let mut bad = pack(&root, "a", "t.a", &[("weapons.lua", b"a")]);
        bad.manifest.id = "../escape".into();
        let dest = root.join("session");
        let err = freeze(&[bad], &dest).unwrap_err();
        assert!(err.contains("cannot be synced"), "{err}");
        assert!(!dest.exists() && !root.join("session.partial").exists());
    }

    #[test]
    fn a_manifest_parses_back_and_forgeries_do_not() {
        let root = scratch("parse");
        let a = pack(&root, "a", "t.a", &[("sprites/x y.png", b"png"), ("w.lua", b"w")]);
        let manifest = freeze(&[a], &root.join("s")).unwrap().manifest;
        let text = manifest.to_text();
        assert_eq!(Manifest::parse(&text).unwrap(), manifest);

        let sha = hex(&digest(b"x"));
        let bad = |line: &str| {
            let forged = format!("{MANIFEST_HEADER}\npack t.a 1\n{line}\n");
            Manifest::parse(&forged).unwrap_err()
        };
        assert!(bad(&format!("file t.a 1 {sha} ../evil")).contains("unsafe"));
        assert!(bad(&format!("file t.a 1 {sha} C:/evil")).contains("unsafe"));
        assert!(bad(&format!("file t.a 1 {sha} a\\b")).contains("unsafe"));
        assert!(bad(&format!("file t.a 1 {sha} /abs")).contains("unsafe"));
        assert!(bad(&format!("file t.b 1 {sha} x")).contains("undeclared"));
        assert!(bad(&format!("file t.a 1 {} x", &sha[1..])).contains("bad hash"));
        let twice = format!("{text}{}", text.lines().last().unwrap().to_string() + "\n");
        assert!(Manifest::parse(&twice).is_err(), "a repeated file is refused");
        assert!(Manifest::parse("hello\n").is_err());
    }

    #[test]
    fn assemble_checks_every_file_it_writes() {
        let root = scratch("assemble");
        let a = pack(&root, "a", "t.a", &[("w.lua", b"w")]);
        let session = freeze(&[a], &root.join("s")).unwrap();
        let err = assemble(&session.manifest, &root.join("c1"), |_| Ok(b"x".to_vec())).unwrap_err();
        assert!(err.contains("does not match"), "{err}");
        assert!(!root.join("c1").exists() && !root.join("c1.partial").exists());
        let dir = session.dir.clone();
        let ok = assemble(&session.manifest, &root.join("c2"), |f| {
            fs::read(dir.join(PACKS_DIR).join(&f.pack).join(&f.path)).map_err(|e| e.to_string())
        })
        .unwrap();
        assert_eq!(ok.manifest, session.manifest);
        assert_eq!(fs::read_to_string(root.join("c2").join(MANIFEST_FILE)).unwrap(), session.manifest.to_text());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_refused() {
        let root = scratch("symlink");
        let a = pack(&root, "a", "t.a", &[]);
        std::os::unix::fs::symlink("/etc/hosts", a.root.join("hosts")).unwrap();
        let err = freeze(&[a], &root.join("session")).unwrap_err();
        assert!(err.contains("symlink"), "{err}");
    }
}
