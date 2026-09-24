//! Refusals, as distinct variants so tests assert on the reason and callers
//! can name it.

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// The first four bytes are not `WFAS`.
    NotACollection,
    /// Written by a newer (or older) format.
    FormatVersion { found: u16, supported: u16 },
    /// The file ended before the header, index or footer did.
    Truncated,
    /// The footer's hash does not match the header and index: the index was
    /// edited, or the file is torn.
    IndexHash,
    /// The index is not strictly sorted by hash, so lookup cannot trust it.
    IndexOrder,
    /// An index field is out of range: a blob outside the blob area, an
    /// unknown storage method, a path that is not UTF-8.
    Malformed(String),
    /// A fetched blob does not hash to its key. Users cannot change what a
    /// replay resolves to: a tampered entry is treated as missing.
    Tampered { sha256: [u8; 32] },
    /// The collection lacks a hash the lock file says was shipped. Collections
    /// are append-only across releases; writing this one would break every
    /// replay that refers to the dropped asset.
    MissingLocked { missing: Vec<[u8; 32]> },
    /// A new collection must carry a higher release number than the one it
    /// was built from.
    VersionNotIncreasing { previous: u32, requested: u32 },
    /// A lock line did not parse.
    Lock { line: usize, detail: String },
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use crate::sha256::to_hex;
        match self {
            Error::NotACollection => write!(f, "not a .WFAssets collection (no WFAS magic)"),
            Error::FormatVersion { found, supported } => {
                write!(f, "collection format {found}, this build reads {supported}")
            }
            Error::Truncated => write!(f, "collection is truncated"),
            Error::IndexHash => write!(f, "collection index does not match its footer hash"),
            Error::IndexOrder => write!(f, "collection index is not sorted by hash"),
            Error::Malformed(detail) => write!(f, "collection index malformed: {detail}"),
            Error::Tampered { sha256 } => {
                write!(f, "collection blob {} does not match its hash", to_hex(sha256))
            }
            Error::MissingLocked { missing } => {
                write!(f, "collection is missing {} locked hash(es):", missing.len())?;
                for sha in missing {
                    write!(f, " {}", to_hex(sha))?;
                }
                Ok(())
            }
            Error::VersionNotIncreasing { previous, requested } => write!(
                f,
                "collection version {requested} is not above the previous collection's {previous}"
            ),
            Error::Lock { line, detail } => write!(f, "lock line {line}: {detail}"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::Truncated
        } else {
            Error::Io(e)
        }
    }
}
