//! Replace a file's contents without truncating it in place.
//!
//! Rust's std truncates an existing file on Windows with
//! `SetFileInformationByHandle(FileAllocationInfo)` and then
//! `(FileEndOfFileInfo)`. `fs::write`, `File::create`, `truncate(true)` and
//! `set_len` all go through it. The Wine that ships in the macOS WA bundles
//! stubs both classes and fails with error 120, "Call not implemented" -- so
//! every one of those calls works the first time, while the file does not
//! exist yet, and fails on every save after that. `set_len` is the worst of
//! them: the write lands, the shrink does not, and the old file's tail is
//! left behind the new contents.
//!
//! Deleting first and creating with `create_new` never asks for a truncate,
//! and neither does a rename over the target. Both were checked under that
//! Wine, as was everything above failing.
//!
//! Use these instead of `fs::write` / `truncate(true)` for any path that may
//! already exist.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Open `path` for writing as an empty file, deleting whatever was there.
///
/// For streams that are written incrementally. Not atomic: until the caller
/// finishes writing, the file holds only what has been written so far.
pub fn create(path: &Path) -> io::Result<File> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Replace `path` with `bytes` atomically: write a sibling temp file, flush it
/// to disk, then rename it over the target. A crash leaves either the old file
/// or the new one, never a half-written one.
///
/// The temp is itself opened through `create`, so a temp left behind by an
/// earlier crash is replaced rather than tripping over the same truncate.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temp = temp_path(path);
    let result = (|| {
        let mut file = create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".wf-tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wf-overwrite-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn write_creates_a_missing_file() {
        let p = dir("missing").join("a.toml");
        write(&p, b"new").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new");
    }

    /// The case that fails under Wine with std. Shorter-over-longer is the
    /// one that matters: a replace that skipped the shrink would leave the
    /// old tail behind, and this catches it.
    #[test]
    fn write_replaces_a_longer_existing_file_completely() {
        let p = dir("longer").join("a.toml");
        fs::write(&p, b"old contents, much longer").unwrap();
        write(&p, b"new").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new");
    }

    #[test]
    fn write_survives_a_stale_temp_from_a_crash() {
        let d = dir("stale");
        let p = d.join("a.toml");
        fs::write(temp_path(&p), b"left over by a crash, and long").unwrap();
        write(&p, b"new").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new");
        assert!(!temp_path(&p).exists(), "temp is consumed by the rename");
    }

    #[test]
    fn write_leaves_no_temp_behind() {
        let d = dir("clean");
        let p = d.join("a.toml");
        write(&p, b"one").unwrap();
        write(&p, b"two").unwrap();
        let names: Vec<_> = fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("a.toml")]);
    }

    #[test]
    fn create_starts_empty_over_an_existing_file() {
        let p = dir("create").join("a.log");
        fs::write(&p, b"old contents").unwrap();
        let mut f = create(&p).unwrap();
        f.write_all(b"x").unwrap();
        drop(f);
        assert_eq!(fs::read(&p).unwrap(), b"x");
    }

    /// A failed write must report, not quietly leave the old file looking
    /// updated. A directory in the way makes the rename fail.
    #[test]
    fn write_reports_failure_and_cleans_its_temp() {
        let d = dir("fail");
        let p = d.join("a.toml");
        fs::create_dir_all(p.join("occupied")).unwrap();
        assert!(write(&p, b"new").is_err());
        assert!(!temp_path(&p).exists());
    }
}
