//! The `.WFAssets` file: a content-addressed, append-only store of the raw
//! assets pack files are usually copied from.
//!
//! ```text
//! "WFAS", u16 format, u32 collection_version, u32 count
//! index, sorted by sha256, one entry per blob:
//!     sha256[32], u64 offset, u32 len, u8 method, u16 path_len, path (UTF-8)
//! blobs, at the offsets the index names
//! footer: sha256(header + index)
//! ```
//!
//! All integers little-endian. `offset` is absolute in the file. `method` 0 is
//! stored raw (PNG and WAV do not deflate); other values are refused until a
//! build knows them. `path` is where the blob was first added from: for
//! listing and diagnostics only, never part of its identity.
//!
//! Opening reads the header and index only (about 250 KB for v1); a blob is
//! read on demand and re-hashed on every fetch, so an edited blob counts as
//! missing rather than resolving to something else.

use crate::{sha256, Error};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Mutex;

pub const MAGIC: &[u8; 4] = b"WFAS";
pub const FORMAT_VERSION: u16 = 1;
pub const METHOD_RAW: u8 = 0;
const HEADER_LEN: usize = 14;
const ENTRY_FIXED_LEN: usize = 32 + 8 + 4 + 1 + 2;
const FOOTER_LEN: u64 = 32;
/// Bounds on what a reader will allocate from an untrusted header.
const MAX_ENTRIES: u32 = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub sha256: [u8; 32],
    pub offset: u64,
    pub len: u32,
    pub method: u8,
    pub path: String,
}

/// An open collection: the index in memory, blobs read on demand.
pub struct Collection<R> {
    version: u32,
    index: Vec<Entry>,
    source: Mutex<R>,
}

impl Collection<std::fs::File> {
    pub fn open(path: &std::path::Path) -> Result<Self, Error> {
        Collection::read(std::fs::File::open(path)?)
    }
}

impl<R: Read + Seek> Collection<R> {
    /// Read and verify the header and index; blobs stay on disk.
    pub fn read(mut source: R) -> Result<Self, Error> {
        let file_len = source.seek(SeekFrom::End(0))?;
        source.seek(SeekFrom::Start(0))?;
        let mut raw = vec![0u8; HEADER_LEN];
        source.read_exact(&mut raw)?;
        if &raw[0..4] != MAGIC {
            return Err(Error::NotACollection);
        }
        let format = u16::from_le_bytes([raw[4], raw[5]]);
        if format != FORMAT_VERSION {
            return Err(Error::FormatVersion { found: format, supported: FORMAT_VERSION });
        }
        let version = u32::from_le_bytes(raw[6..10].try_into().unwrap());
        let count = u32::from_le_bytes(raw[10..14].try_into().unwrap());
        if count > MAX_ENTRIES {
            return Err(Error::Malformed(format!("{count} entries")));
        }

        let mut index = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let start = raw.len();
            raw.resize(start + ENTRY_FIXED_LEN, 0);
            source.read_exact(&mut raw[start..])?;
            let e = &raw[start..];
            let path_len = u16::from_le_bytes([e[45], e[46]]) as usize;
            let entry = Entry {
                sha256: e[0..32].try_into().unwrap(),
                offset: u64::from_le_bytes(e[32..40].try_into().unwrap()),
                len: u32::from_le_bytes(e[40..44].try_into().unwrap()),
                method: e[44],
                path: String::new(),
            };
            let path_start = raw.len();
            raw.resize(path_start + path_len, 0);
            source.read_exact(&mut raw[path_start..])?;
            let path = std::str::from_utf8(&raw[path_start..])
                .map_err(|_| Error::Malformed("path is not UTF-8".into()))?;
            index.push(Entry { path: path.to_string(), ..entry });
        }
        let index_end = raw.len() as u64;

        if file_len < index_end + FOOTER_LEN {
            return Err(Error::Truncated);
        }
        source.seek(SeekFrom::Start(file_len - FOOTER_LEN))?;
        let mut footer = [0u8; 32];
        source.read_exact(&mut footer)?;
        if footer != sha256::digest(&raw) {
            return Err(Error::IndexHash);
        }

        let blob_end = file_len - FOOTER_LEN;
        for pair in index.windows(2) {
            if pair[0].sha256 >= pair[1].sha256 {
                return Err(Error::IndexOrder);
            }
        }
        for e in &index {
            if e.method != METHOD_RAW {
                return Err(Error::Malformed(format!("unknown method {}", e.method)));
            }
            let end = e.offset.checked_add(e.len as u64);
            if e.offset < index_end || end.is_none_or(|end| end > blob_end) {
                return Err(Error::Malformed(format!(
                    "blob {} at {}+{} outside {index_end}..{blob_end}",
                    e.path, e.offset, e.len
                )));
            }
        }
        Ok(Collection { version, index, source: Mutex::new(source) })
    }

    /// The release number this collection was cut for.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Every entry, sorted by hash.
    pub fn entries(&self) -> &[Entry] {
        &self.index
    }

    /// Binary search on the sorted index.
    pub fn find(&self, sha256: &[u8; 32]) -> Option<&Entry> {
        self.index
            .binary_search_by(|e| e.sha256.cmp(sha256))
            .ok()
            .map(|i| &self.index[i])
    }

    pub fn contains(&self, sha256: &[u8; 32]) -> bool {
        self.find(sha256).is_some()
    }

    /// Fetch a blob by hash, re-hashed on every read. `Ok(None)` when absent;
    /// `Err(Tampered)` when present but not what its key says.
    pub fn get(&self, sha256: &[u8; 32]) -> Result<Option<Vec<u8>>, Error> {
        let Some(entry) = self.find(sha256) else {
            return Ok(None);
        };
        let mut bytes = vec![0u8; entry.len as usize];
        {
            let mut source = self.source.lock().map_err(|_| {
                Error::Io(std::io::Error::other("collection reader poisoned"))
            })?;
            source.seek(SeekFrom::Start(entry.offset))?;
            source.read_exact(&mut bytes)?;
        }
        if sha256::digest(&bytes) != *sha256 {
            return Err(Error::Tampered { sha256: *sha256 });
        }
        Ok(Some(bytes))
    }

    /// Re-hash every blob. `wf-assets verify`.
    pub fn verify_all(&self) -> Result<(), Error> {
        for e in &self.index {
            self.get(&e.sha256)?;
        }
        Ok(())
    }
}

/// Assembles a new collection. Deduplicates by content: adding bytes already
/// present keeps the first path and stores nothing new.
#[derive(Default)]
pub struct Builder {
    blobs: BTreeMap<[u8; 32], (String, Vec<u8>)>,
    previous_version: Option<u32>,
}

impl Builder {
    pub fn new() -> Builder {
        Builder::default()
    }

    /// Start from a shipped collection, carrying every entry forward. Each
    /// blob is re-hashed on the way, so a damaged previous file cannot seed a
    /// damaged successor.
    pub fn from_previous<R: Read + Seek>(previous: &Collection<R>) -> Result<Builder, Error> {
        let mut builder = Builder { previous_version: Some(previous.version()), ..Builder::default() };
        for e in previous.entries() {
            let bytes = previous.get(&e.sha256)?.expect("entry listed by its own index");
            builder.blobs.insert(e.sha256, (e.path.clone(), bytes));
        }
        Ok(builder)
    }

    /// Add one file. Returns its hash and whether it was new.
    pub fn add(&mut self, path: &str, bytes: Vec<u8>) -> ([u8; 32], bool) {
        let sha = sha256::digest(&bytes);
        let new = !self.blobs.contains_key(&sha);
        if new {
            self.blobs.insert(sha, (path.to_string(), bytes));
        }
        (sha, new)
    }

    pub fn len(&self) -> usize {
        self.blobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty()
    }

    pub fn contains(&self, sha256: &[u8; 32]) -> bool {
        self.blobs.contains_key(sha256)
    }

    /// Serialize as release `version`. Refuses a version that does not rise
    /// above the collection this one was built from.
    pub fn write(&self, version: u32, out: &mut impl Write) -> Result<(), Error> {
        if let Some(previous) = self.previous_version {
            if version <= previous {
                return Err(Error::VersionNotIncreasing { previous, requested: version });
            }
        }
        let mut head = Vec::new();
        head.extend_from_slice(MAGIC);
        head.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        head.extend_from_slice(&version.to_le_bytes());
        head.extend_from_slice(&(self.blobs.len() as u32).to_le_bytes());
        let index_len: usize = self
            .blobs
            .values()
            .map(|(path, _)| ENTRY_FIXED_LEN + path.len())
            .sum();
        let mut offset = (HEADER_LEN + index_len) as u64;
        // BTreeMap iterates in key order: the index comes out sorted.
        for (sha, (path, bytes)) in &self.blobs {
            let path_len = u16::try_from(path.len())
                .map_err(|_| Error::Malformed(format!("path too long: {path}")))?;
            let len = u32::try_from(bytes.len())
                .map_err(|_| Error::Malformed(format!("blob too large: {path}")))?;
            head.extend_from_slice(sha);
            head.extend_from_slice(&offset.to_le_bytes());
            head.extend_from_slice(&len.to_le_bytes());
            head.push(METHOD_RAW);
            head.extend_from_slice(&path_len.to_le_bytes());
            head.extend_from_slice(path.as_bytes());
            offset += len as u64;
        }
        out.write_all(&head)?;
        for (_, bytes) in self.blobs.values() {
            out.write_all(bytes)?;
        }
        out.write_all(&sha256::digest(&head))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn build(files: &[(&str, &[u8])], version: u32) -> Vec<u8> {
        let mut b = Builder::new();
        for (path, bytes) in files {
            b.add(path, bytes.to_vec());
        }
        let mut out = Vec::new();
        b.write(version, &mut out).unwrap();
        out
    }

    const FILES: [(&str, &[u8]); 3] = [
        ("sprites/a.png", b"first asset"),
        ("sounds/b.wav", b"second asset, a little longer"),
        ("sprites/c.png", b"third"),
    ];

    #[test]
    fn round_trip() {
        let c = Collection::read(Cursor::new(build(&FILES, 1))).unwrap();
        assert_eq!(c.version(), 1);
        assert_eq!(c.entries().len(), 3);
        for (path, bytes) in FILES {
            let sha = sha256::digest(bytes);
            assert_eq!(c.get(&sha).unwrap().as_deref(), Some(bytes));
            assert_eq!(c.find(&sha).unwrap().path, path);
        }
        assert_eq!(c.get(&sha256::digest(b"not in it")).unwrap(), None);
        c.verify_all().unwrap();
    }

    #[test]
    fn identical_bytes_are_stored_once() {
        let mut b = Builder::new();
        assert!(b.add("a.png", b"same".to_vec()).1);
        assert!(!b.add("copy/a.png", b"same".to_vec()).1);
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn index_is_sorted_and_found_by_binary_search() {
        let files: Vec<(String, Vec<u8>)> =
            (0..200).map(|i| (format!("f{i}.png"), format!("blob {i}").into_bytes())).collect();
        let refs: Vec<(&str, &[u8])> =
            files.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
        let c = Collection::read(Cursor::new(build(&refs, 1))).unwrap();
        assert!(c.entries().windows(2).all(|w| w[0].sha256 < w[1].sha256));
        for (_, bytes) in &files {
            assert!(c.contains(&sha256::digest(bytes)));
        }
    }

    #[test]
    fn a_tampered_blob_is_refused() {
        let mut bytes = build(&FILES, 1);
        let c = Collection::read(Cursor::new(bytes.clone())).unwrap();
        let victim = c.find(&sha256::digest(FILES[1].1)).unwrap().clone();
        bytes[victim.offset as usize] ^= 0xFF;
        // The index is intact, so the file still opens...
        let c = Collection::read(Cursor::new(bytes)).unwrap();
        // ...but the edited blob never resolves.
        assert!(matches!(c.get(&victim.sha256), Err(Error::Tampered { .. })));
        assert!(c.verify_all().is_err());
        // Untouched blobs still do.
        assert!(c.get(&sha256::digest(FILES[0].1)).unwrap().is_some());
    }

    #[test]
    fn a_tampered_index_is_refused() {
        let mut bytes = build(&FILES, 1);
        bytes[HEADER_LEN + 40] ^= 1; // first entry's length
        assert!(matches!(Collection::read(Cursor::new(bytes)), Err(Error::IndexHash)));
    }

    #[test]
    fn truncation_and_foreign_files_are_refused() {
        let bytes = build(&FILES, 1);
        for cut in [0, 10, HEADER_LEN + 5, bytes.len() - 1] {
            assert!(Collection::read(Cursor::new(bytes[..cut].to_vec())).is_err(), "cut {cut}");
        }
        let mut foreign = bytes.clone();
        foreign[0] = b'X';
        assert!(matches!(Collection::read(Cursor::new(foreign)), Err(Error::NotACollection)));
    }

    #[test]
    fn carry_forward_keeps_every_old_entry() {
        let v1 = Collection::read(Cursor::new(build(&FILES, 1))).unwrap();
        let mut b = Builder::from_previous(&v1).unwrap();
        b.add("new/d.png", b"added in release 2".to_vec());
        let mut out = Vec::new();
        b.write(2, &mut out).unwrap();
        let v2 = Collection::read(Cursor::new(out)).unwrap();
        assert_eq!(v2.version(), 2);
        for e in v1.entries() {
            assert_eq!(v2.get(&e.sha256).unwrap(), v1.get(&e.sha256).unwrap());
        }
        assert_eq!(v2.entries().len(), v1.entries().len() + 1);
    }

    #[test]
    fn the_version_must_rise() {
        let v3 = Collection::read(Cursor::new(build(&FILES, 3))).unwrap();
        let b = Builder::from_previous(&v3).unwrap();
        for v in [2, 3] {
            assert!(matches!(
                b.write(v, &mut Vec::new()),
                Err(Error::VersionNotIncreasing { previous: 3, .. })
            ));
        }
        b.write(4, &mut Vec::new()).unwrap();
    }
}
