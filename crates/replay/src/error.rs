//! Refusals a reader must be able to tell apart.
//!
//! Playback has to refuse cleanly -- wrong format version, wrong WA version, a
//! damaged trailer, assets the local collection lacks -- and each wants its
//! own message, not one `String` that says "bad file". So they are distinct
//! variants, and the tests assert on the variant rather than on wording.

use std::fmt;

/// A byte offset into the container. `u32` because a replay is a file, not a
/// stream, and the section directory stores offsets in 32 bits.
pub type Offset = u32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The file, or a section inside it, ended early.
    Truncated {
        at: Offset,
        wanted: usize,
        got: usize,
    },
    /// D6. A container written by a newer (or older) build.
    FormatVersion { found: u16, supported: u16 },
    /// The container is structurally understood but its embedded vanilla
    /// replay targets a different WA executable layout.
    WaVersion { found: String, supported: String },
    /// The footer is ours, but the lengths it records do not add up to the
    /// file: bytes were cut out of, or added to, the replay.
    TrailerLength {
        wa_len: u64,
        trailer_len: u64,
        file_len: u64,
    },
    /// The footer is ours, but the trailer's CRC does not match: the file was
    /// damaged after it was written.
    TrailerCrc { expected: u32, found: u32 },
    /// Pack files recorded as `.WFAssets` references that the local
    /// collection does not have (or has, but tampered). Every one is named.
    MissingAssets { missing: Vec<[u8; 32]> },
    /// A section the reader needs is absent.
    MissingSection { tag: &'static str },
    /// A section tag this build has never heard of. Refused rather than
    /// skipped: an unknown tag means the writer knew something we do not.
    UnknownSection { tag: [u8; 4] },
    /// A directory entry points outside the file.
    SectionBounds {
        tag: [u8; 4],
        offset: Offset,
        len: u32,
        file_len: u32,
    },
    /// Two sections claim overlapping bytes, so at least one of them is not
    /// what it says it is.
    SectionsOverlap { a: [u8; 4], b: [u8; 4] },
    /// A body did not decode: bad varint, bad UTF-8, unknown tag, torn record.
    Malformed { what: &'static str, detail: String },
    /// The frame stream is larger than the reader will accept. Refused at the
    /// point of writing as well, so a recording stops rather than producing a
    /// file that would be silently truncated on playback.
    StreamFull { cap: u32 },
    /// The `[pack]` payload is not a deflate stream this build can inflate.
    PackCodec(String),
    /// The compressed pack table claims more data or entries than playback
    /// will materialise. This also bounds gzip expansion from untrusted files.
    PackLimit(String),
    /// The container would not fit inside the 32-bit directory offsets.
    /// The writer refuses at this size rather than emitting offsets that
    /// wrap; nothing this large has ever been a real recording.
    ContainerTooLarge { bytes: u64 },
    /// Filesystem failure, message already formatted.
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated { at, wanted, got } => write!(
                f,
                "truncated at byte {at}: wanted {wanted} more bytes, {got} available"
            ),
            Error::FormatVersion { found, supported } => write!(
                f,
                "replay format version {found} is not supported by this build (reads {supported})"
            ),
            Error::WaVersion { found, supported } => write!(
                f,
                "replay was recorded for WA {found}; this build supports {supported}"
            ),
            Error::TrailerLength {
                wa_len,
                trailer_len,
                file_len,
            } => write!(
                f,
                "replay trailer is damaged: {wa_len} WA bytes + {trailer_len} trailer bytes \
                 do not fit a {file_len}-byte file"
            ),
            Error::TrailerCrc { expected, found } => write!(
                f,
                "replay trailer is damaged: CRC {found:08x}, expected {expected:08x}"
            ),
            Error::MissingAssets { missing } => {
                write!(f, "{} asset(s) missing from the .WFAssets collection:", missing.len())?;
                for sha in missing {
                    write!(f, " {}", assets::sha256::to_hex(sha))?;
                }
                Ok(())
            }
            Error::MissingSection { tag } => {
                write!(f, "replay is missing its {} section", tag.trim())
            }
            Error::UnknownSection { tag } => write!(
                f,
                "replay has an unknown section tag {:?}",
                String::from_utf8_lossy(tag)
            ),
            Error::SectionBounds {
                tag,
                offset,
                len,
                file_len,
            } => write!(
                f,
                "section {} claims bytes {offset}..{} of a {file_len}-byte file",
                tag_str(tag),
                *offset as u64 + *len as u64
            ),
            Error::SectionsOverlap { a, b } => {
                write!(f, "sections {} and {} overlap", tag_str(a), tag_str(b))
            }
            Error::Malformed { what, detail } => write!(f, "{what} is malformed: {detail}"),
            Error::StreamFull { cap } => {
                write!(f, "frame stream exceeded its {cap}-byte cap")
            }
            Error::PackCodec(d) => write!(f, "pack payload did not inflate: {d}"),
            Error::PackLimit(d) => write!(f, "pack payload exceeds its limit: {d}"),
            Error::ContainerTooLarge { bytes } => write!(
                f,
                "container would be {bytes} bytes, past the 4 GiB the 32-bit directory can address"
            ),
            Error::Io(d) => write!(f, "{d}"),
        }
    }
}

impl std::error::Error for Error {}

/// Render a 4-byte tag for a human, trimming the padding spaces.
pub fn tag_str(tag: &[u8; 4]) -> String {
    String::from_utf8_lossy(tag).trim().to_string()
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}
