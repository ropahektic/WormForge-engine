//! `assets/wfassets.lock`: every hash any shipped collection has contained.
//!
//! ```text
//! # comment
//! <sha256 hex> <size> <first_release> <path>
//! ```
//!
//! One line per entry, committed to git. The builder appends a line for each
//! new blob and refuses to write a collection missing any locked hash: that
//! is what makes the collection append-only across releases, so a replay
//! recorded on release N resolves on every release after it. The path is the
//! rest of the line and may contain spaces.

use crate::{sha256, Error};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockEntry {
    pub sha256: [u8; 32],
    pub size: u64,
    pub first_release: u32,
    pub path: String,
}

impl LockEntry {
    pub fn line(&self) -> String {
        format!(
            "{} {} {} {}",
            sha256::to_hex(&self.sha256),
            self.size,
            self.first_release,
            self.path
        )
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Lock {
    pub entries: Vec<LockEntry>,
}

impl Lock {
    pub fn parse(text: &str) -> Result<Lock, Error> {
        let mut entries = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim_end_matches('\r');
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let bad = |detail: &str| Error::Lock { line: i + 1, detail: detail.to_string() };
            let mut fields = line.splitn(4, ' ');
            let (Some(sha), Some(size), Some(release), Some(path)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                return Err(bad("expected: sha256 size first_release path"));
            };
            entries.push(LockEntry {
                sha256: sha256::from_hex(sha).ok_or_else(|| bad("bad sha256"))?,
                size: size.parse().map_err(|_| bad("bad size"))?,
                first_release: release.parse().map_err(|_| bad("bad first_release"))?,
                path: path.to_string(),
            });
        }
        Ok(Lock { entries })
    }

    pub fn contains(&self, sha256: &[u8; 32]) -> bool {
        self.entries.iter().any(|e| e.sha256 == *sha256)
    }

    /// Every locked hash `has` does not know, in lock order.
    pub fn missing(&self, has: impl Fn(&[u8; 32]) -> bool) -> Vec<[u8; 32]> {
        self.entries.iter().filter(|e| !has(&e.sha256)).map(|e| e.sha256).collect()
    }

    /// The append-only rule: refuse unless every locked hash is present.
    pub fn check(&self, has: impl Fn(&[u8; 32]) -> bool) -> Result<(), Error> {
        let missing = self.missing(has);
        if missing.is_empty() {
            Ok(())
        } else {
            Err(Error::MissingLocked { missing })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lines_with_spaces_in_paths() {
        let a = sha256::digest(b"a");
        let text = format!(
            "# header comment\n\n{} 12 1 sprites/Worm Sheep.png\r\n",
            sha256::to_hex(&a)
        );
        let lock = Lock::parse(&text).unwrap();
        assert_eq!(lock.entries.len(), 1);
        assert_eq!(lock.entries[0].path, "sprites/Worm Sheep.png");
        assert_eq!(lock.entries[0].size, 12);
        assert_eq!(Lock::parse(&lock.entries[0].line()).unwrap(), lock);
    }

    #[test]
    fn a_bad_line_is_named() {
        let err = Lock::parse("# ok\nnot-a-hash 1 1 x\n").unwrap_err();
        assert!(matches!(err, Error::Lock { line: 2, .. }));
    }

    #[test]
    fn a_missing_locked_hash_is_refused() {
        let (a, b) = (sha256::digest(b"a"), sha256::digest(b"b"));
        let lock = Lock {
            entries: [a, b]
                .iter()
                .map(|&sha256| LockEntry { sha256, size: 1, first_release: 1, path: "x".into() })
                .collect(),
        };
        lock.check(|_| true).unwrap();
        match lock.check(|sha| *sha == a) {
            Err(Error::MissingLocked { missing }) => assert_eq!(missing, vec![b]),
            other => panic!("{other:?}"),
        }
    }
}
