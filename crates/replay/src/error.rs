//! Refusals a reader must be able to tell apart.
//!
//! W3 has to "refuse cleanly on: wrong format version, wrong WA version,
//! missing `complete`", and those three want three different messages, not one
//! `String` that says "bad file". So they are distinct variants, and the tests
//! assert on the variant rather than on wording.

use std::fmt;

/// A byte offset into the container. `u32` because a replay is a file, not a
/// stream, and the section directory stores offsets in 32 bits.
pub type Offset = u32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The first four bytes are not `WFRP`. Almost certainly a plain
    /// `.WAgame` handed to the container reader.
    NotAContainer,
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
    /// The two-phase flag never reached phase two: the round was aborted
    /// between opening the recorder and finishing the file. An unplayable
    /// file is the intended outcome, and it must not read as a valid one.
    Incomplete,
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
            Error::NotAContainer => write!(f, "not a WormForge replay: magic is not \"WFRP\""),
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
            Error::Incomplete => write!(
                f,
                "replay is marked incomplete: recording did not reach the end of the round"
            ),
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
