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

use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use weapons::LoadedMod;

pub use assets::sha256::digest;

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
    if dest.exists() {
        return Err(format!("session directory already exists: {}", dest.display()));
    }
    let staging = staging_path(dest)?;
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    }
    let result = build(mods, &staging).and_then(|manifest| {
        fs::rename(&staging, dest)
            .map_err(|e| format!("publish {}: {e}", dest.display()))?;
        Ok(Session { dir: dest.to_path_buf(), manifest })
    });
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

fn build(mods: &[LoadedMod], staging: &Path) -> Result<Manifest, String> {
    let mut sorted: Vec<&LoadedMod> = mods.iter().collect();
    sorted.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    let mut manifest = Manifest::default();
    for m in sorted {
        let id = &m.manifest.id;
        check_id(id)?;
        check_field("version", &m.manifest.version)?;
        if manifest.packs.iter().any(|p| &p.id == id) {
            return Err(format!("pack id {id} appears twice"));
        }
        manifest.packs.push(FrozenPack { id: id.clone(), version: m.manifest.version.clone() });

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
            check_field("path", &joined)?;
            out.push((joined, path));
        }
    }
    Ok(())
}

/// Ids name folders in the session directory and fields in the manifest.
fn check_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty()
        && !id.starts_with('.')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(format!("pack id {id:?} cannot be synced: use letters, digits, '.', '_' or '-'"))
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
        let toml = format!("id = \"{id}\"\nversion = \"1.0\"\napi_version = 1\nentry = \"weapons.lua\"\n");
        fs::write(dir.join("mod.toml"), &toml).unwrap();
        for (path, bytes) in files {
            let target = dir.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, bytes).unwrap();
        }
        LoadedMod { manifest: parse_manifest(&toml).unwrap(), root: dir }
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
