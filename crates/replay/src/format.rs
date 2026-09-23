//! The section directory, `[hdr]`, and the `[wf]` / `[chk]` streams.
//!
//! ```text
//!  0  "WFRP"            magic
//!  4  u16 format version
//!  6  u16 section count
//!  8  count x ( tag[4], u32 offset, u32 len )   the directory
//! ..  section bodies at their offsets, absolute from byte 0
//! ```
//!
//! Offsets are absolute and 32-bit because a replay is a file, not a stream
//! (`error::Offset`). The directory comes first so a reader knows every
//! section before touching any body, and so `[hdr]` -- including the
//! two-phase `complete` flag -- sits at a discoverable offset that phase two
//! can rewrite once, when the round ends.
//!
//! Every reader here **refuses rather than degrades**: an unknown tag, an
//! out-of-bounds or overlapping entry, a frame stream past its cap, frames
//! out of order -- each is its own `Error`, never a best-effort parse.
//! Dropping a frame silently would play back a different match than the one
//! recorded, which is worse than not recording at all.

use crate::error::{Error, Offset};

/// File magic. First four bytes, checked before anything else.
pub const MAGIC: [u8; 4] = *b"WFRP";
/// Bump whenever any on-disk shape changes -- readers refuse other values
/// with [`Error::FormatVersion`] rather than guessing (gap D6).
pub const FORMAT_VERSION: u16 = 1;

pub const TAG_HDR: [u8; 4] = *b"hdr ";
pub const TAG_PACK: [u8; 4] = *b"pack";
pub const TAG_WA: [u8; 4] = *b"wa  ";
pub const TAG_WF: [u8; 4] = *b"wf  ";
pub const TAG_CHK: [u8; 4] = *b"chk ";

/// Tags this build understands. Anything else is refused (`UnknownSection`):
/// an unknown tag means the writer knew something we do not.
pub const KNOWN_TAGS: [[u8; 4]; 5] = [TAG_HDR, TAG_PACK, TAG_WA, TAG_WF, TAG_CHK];

/// The frozen field order of `[hdr]`. Changing this list changes the bytes,
/// so it changes [`FORMAT_VERSION`] with it -- the schema is what a reader
/// of a future build compares against to refuse cleanly.
pub const HDR_SCHEMA: [&str; 5] = ["wa_version", "build_hash", "modset", "flags", "complete"];

/// Hard ceiling on the encoded `[wf]` stream: 16 MiB is ~1.7M samples, over
/// nine hours at 50 fps -- orders of magnitude past any real match (~1 MB
/// for an hour). The recorder refuses *and stops* at this size rather than
/// producing a file whose stream a reader would then refuse; both sides use
/// the same constant so the two refusals can never disagree.
pub const MAX_WF_BYTES: u32 = 16 * 1024 * 1024;

const WHAT_WF: &str = "[wf] stream";
const WHAT_CHK: &str = "[chk] stream";

/// `[hdr]` -- identity of the recording. `complete` is the two-phase flag:
/// `false` while the round runs, rewritten to `true` when it ends cleanly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    /// WA's own version string, e.g. `"3.8.1.0"`.
    pub wa_version: String,
    /// The wkLua build that recorded this. Refusing a mismatched build is
    /// W3's job; the field exists so it can.
    pub build_hash: String,
    /// `modset_fingerprint()` at load: `id@version;...`.
    pub modset: String,
    /// Container flags, reserved, currently 0.
    pub flags: u16,
    /// Phase two of the two-phase close. Playback refuses `false`.
    pub complete: bool,
}

/// One frame's local input, as latched by `game::latch::FrameLatch` -- the
/// first reader in a frame samples, and this is that one sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputSample {
    /// World-root FRAME_FINISH frame number. Matches run strictly upward.
    pub frame: i64,
    /// Key bitmask, same layout as `wa.world.keys()`.
    pub keys: u32,
    pub px: i32,
    pub py: i32,
    pub down: bool,
}

/// One sync checkpoint: the three component hashes the probe prints, kept
/// split so a divergence names *what* moved (see `docs/replay.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub frame: i64,
    pub rng: u32,
    pub worms: u32,
    pub actors: u32,
}

/// Length-prefixed string, 32-bit little-endian byte length -- shared with
/// the `[pack]` table so both codecs agree on what "string" means.
pub fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// LEB128.
fn put_uvar(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Zigzag, so the negative screen coordinates a pointer can carry do not
/// cost 65536 bytes each as a sign-extended u64.
fn put_svar(out: &mut Vec<u8>, v: i64) {
    put_uvar(out, ((v as u64) << 1) ^ ((v >> 63) as u64));
}

/// Encode the frame stream. Frames must strictly increase -- the latch
/// guarantees one sample per frame, so a repeat or a step backwards means
/// the recorder miscounted, and a miscounted stream is refused here rather
/// than played back as input from the wrong frame.
pub fn encode_inputs(samples: &[InputSample]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut prev: Option<i64> = None;
    for s in samples {
        if let Some(p) = prev {
            if s.frame <= p {
                return Err(Error::Malformed {
                    what: WHAT_WF,
                    detail: format!("frame {} does not strictly follow {p}", s.frame),
                });
            }
        }
        put_svar(&mut out, s.frame);
        put_uvar(&mut out, s.keys as u64);
        put_svar(&mut out, s.px as i64);
        put_svar(&mut out, s.py as i64);
        out.push(s.down as u8);
        if out.len() > MAX_WF_BYTES as usize {
            return Err(Error::StreamFull { cap: MAX_WF_BYTES });
        }
        prev = Some(s.frame);
    }
    Ok(out)
}

/// The reader half of [`encode_inputs`]: the same cap, the same
/// monotonic-frame rule, plus range checks only a forged file could trip.
/// A record the decoder does not understand is refused, never skipped --
/// skipping one would shift every later frame onto the wrong input.
pub fn decode_inputs(bytes: &[u8]) -> Result<Vec<InputSample>, Error> {
    check_cap(bytes)?;
    let mut r = Body::new(bytes, WHAT_WF);
    let mut out = Vec::new();
    let mut prev: Option<i64> = None;
    while !r.is_empty() {
        let frame = r.svar()?;
        if let Some(p) = prev {
            if frame <= p {
                return Err(Error::Malformed {
                    what: WHAT_WF,
                    detail: format!("frame {frame} does not strictly follow {p}"),
                });
            }
        }
        let keys = r.u32_var()?;
        let px = r.s32()?;
        let py = r.s32()?;
        let down = match r.u8()? {
            0 => false,
            1 => true,
            b => {
                return Err(Error::Malformed {
                    what: WHAT_WF,
                    detail: format!("down flag is {b}, not 0 or 1"),
                })
            }
        };
        out.push(InputSample {
            frame,
            keys,
            px,
            py,
            down,
        });
        prev = Some(frame);
    }
    Ok(out)
}

/// Where "full" is, defined once so the writer's refusal and the reader's
/// can never disagree about the boundary -- two sides computing the cap
/// differently is how a silently truncated file starts.
fn check_cap(bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() as u64 > MAX_WF_BYTES as u64 {
        Err(Error::StreamFull { cap: MAX_WF_BYTES })
    } else {
        Ok(())
    }
}

/// `[chk]` encoder. Shares `[wf]`'s monotonic-frame rule (a checkpoint may
/// never repeat or rewind a frame the input stream has already passed) and
/// its cap, for the reason on [`check_cap`].
pub fn encode_checkpoints(cps: &[Checkpoint]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut prev: Option<i64> = None;
    for c in cps {
        if let Some(p) = prev {
            if c.frame <= p {
                return Err(Error::Malformed {
                    what: WHAT_CHK,
                    detail: format!("frame {} does not strictly follow {p}", c.frame),
                });
            }
        }
        put_svar(&mut out, c.frame);
        put_uvar(&mut out, c.rng as u64);
        put_uvar(&mut out, c.worms as u64);
        put_uvar(&mut out, c.actors as u64);
        check_cap(&out)?;
        prev = Some(c.frame);
    }
    Ok(out)
}

/// The three hashes are u32 on the wire as everywhere else, so a wider
/// varint means the file is not what it claims to be.
pub fn decode_checkpoints(bytes: &[u8]) -> Result<Vec<Checkpoint>, Error> {
    check_cap(bytes)?;
    let mut r = Body::new(bytes, WHAT_CHK);
    let mut out = Vec::new();
    let mut prev: Option<i64> = None;
    while !r.is_empty() {
        let frame = r.svar()?;
        if let Some(p) = prev {
            if frame <= p {
                return Err(Error::Malformed {
                    what: WHAT_CHK,
                    detail: format!("frame {frame} does not strictly follow {p}"),
                });
            }
        }
        out.push(Checkpoint {
            frame,
            rng: r.u32_var()?,
            worms: r.u32_var()?,
            actors: r.u32_var()?,
        });
        prev = Some(frame);
    }
    Ok(out)
}

const DIRECTORY_HEADER: usize = 8;
const DIRECTORY_ENTRY: usize = 12;

#[derive(Clone, Copy, Debug)]
struct Section {
    tag: [u8; 4],
    offset: Offset,
    len: u32,
}

/// A validated view of a replay container. Construction validates the whole
/// directory before any section is exposed, so later accessors are only slice
/// lookups and cannot discover a bounds error halfway through playback.
pub struct Container<'a> {
    bytes: &'a [u8],
    sections: Vec<Section>,
    header: Header,
}

impl<'a> Container<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() < MAGIC.len() || bytes[..4] != MAGIC {
            return Err(Error::NotAContainer);
        }
        let mut root = Body::new(bytes, "container directory");
        root.take(4)?;
        let version = root.u16()?;
        if version != FORMAT_VERSION {
            return Err(Error::FormatVersion {
                found: version,
                supported: FORMAT_VERSION,
            });
        }
        let count = root.u16()? as usize;
        if count != KNOWN_TAGS.len() {
            return Err(Error::Malformed {
                what: "container directory",
                detail: format!("has {count} sections, expected {}", KNOWN_TAGS.len()),
            });
        }
        let directory_len =
            DIRECTORY_HEADER
                .checked_add(count.checked_mul(DIRECTORY_ENTRY).ok_or_else(|| {
                    Error::Malformed {
                        what: "container directory",
                        detail: "section count overflows the directory size".into(),
                    }
                })?)
                .ok_or_else(|| Error::Malformed {
                    what: "container directory",
                    detail: "directory size overflows".into(),
                })?;
        if bytes.len() < directory_len {
            return Err(Error::Truncated {
                at: root.pos as Offset,
                wanted: directory_len - root.pos,
                got: bytes.len().saturating_sub(root.pos),
            });
        }

        let file_len = u32::try_from(bytes.len()).map_err(|_| Error::ContainerTooLarge {
            bytes: bytes.len() as u64,
        })?;
        let mut sections = Vec::with_capacity(count);
        for _ in 0..count {
            let tag: [u8; 4] = root.take(4)?.try_into().unwrap();
            if !KNOWN_TAGS.contains(&tag) {
                return Err(Error::UnknownSection { tag });
            }
            if sections.iter().any(|s: &Section| s.tag == tag) {
                return Err(Error::Malformed {
                    what: "container directory",
                    detail: format!("section {} appears twice", crate::tag_str(&tag)),
                });
            }
            let offset = root.u32()?;
            let len = root.u32()?;
            let end = offset as u64 + len as u64;
            if offset < directory_len as u32 || end > file_len as u64 {
                return Err(Error::SectionBounds {
                    tag,
                    offset,
                    len,
                    file_len,
                });
            }
            sections.push(Section { tag, offset, len });
        }

        let mut ordered = sections.clone();
        ordered.sort_by_key(|s| s.offset);
        for pair in ordered.windows(2) {
            let a_end = pair[0].offset as u64 + pair[0].len as u64;
            if a_end > pair[1].offset as u64 {
                return Err(Error::SectionsOverlap {
                    a: pair[0].tag,
                    b: pair[1].tag,
                });
            }
        }
        for (tag, name) in [
            (TAG_HDR, "hdr "),
            (TAG_PACK, "pack"),
            (TAG_WA, "wa  "),
            (TAG_WF, "wf  "),
            (TAG_CHK, "chk "),
        ] {
            if !sections.iter().any(|s| s.tag == tag) {
                return Err(Error::MissingSection { tag: name });
            }
        }

        let hdr = section_bytes(bytes, &sections, TAG_HDR).unwrap();
        let header = decode_header(hdr)?;
        Ok(Self {
            bytes,
            sections,
            header,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn require_wa_version(&self, supported: &str) -> Result<(), Error> {
        if self.header.wa_version == supported {
            Ok(())
        } else {
            Err(Error::WaVersion {
                found: self.header.wa_version.clone(),
                supported: supported.to_string(),
            })
        }
    }

    pub fn pack(&self) -> Result<&'a [u8], Error> {
        self.complete_section(TAG_PACK)
    }

    pub fn wa(&self) -> Result<&'a [u8], Error> {
        self.complete_section(TAG_WA)
    }

    pub fn wf(&self) -> Result<&'a [u8], Error> {
        self.complete_section(TAG_WF)
    }

    pub fn chk(&self) -> Result<&'a [u8], Error> {
        self.complete_section(TAG_CHK)
    }

    fn complete_section(&self, tag: [u8; 4]) -> Result<&'a [u8], Error> {
        if !self.header.complete {
            return Err(Error::Incomplete);
        }
        Ok(section_bytes(self.bytes, &self.sections, tag).unwrap())
    }
}

fn section_bytes<'a>(bytes: &'a [u8], sections: &[Section], tag: [u8; 4]) -> Option<&'a [u8]> {
    let s = sections.iter().find(|s| s.tag == tag)?;
    let start = s.offset as usize;
    Some(&bytes[start..start + s.len as usize])
}

/// Assemble the five fixed sections. The order is frozen with the format
/// version even though the reader uses tags: stable bytes make fixtures and
/// extraction tools straightforward to compare.
pub fn assemble(
    header: &Header,
    packs: &[u8],
    wa: &[u8],
    wf: &[u8],
    checkpoints: &[u8],
) -> Result<Vec<u8>, Error> {
    check_cap(wf)?;
    check_cap(checkpoints)?;
    let header = encode_header(header);
    let bodies: [([u8; 4], &[u8]); 5] = [
        (TAG_HDR, &header),
        (TAG_PACK, packs),
        (TAG_WA, wa),
        (TAG_WF, wf),
        (TAG_CHK, checkpoints),
    ];
    let directory_len = DIRECTORY_HEADER + bodies.len() * DIRECTORY_ENTRY;
    let total = bodies
        .iter()
        .try_fold(directory_len as u64, |n, (_, body)| {
            n.checked_add(body.len() as u64)
        })
        .ok_or(Error::ContainerTooLarge { bytes: u64::MAX })?;
    if total > u32::MAX as u64 {
        return Err(Error::ContainerTooLarge { bytes: total });
    }

    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(bodies.len() as u16).to_le_bytes());
    let mut offset = directory_len as u32;
    for (tag, body) in &bodies {
        let len =
            u32::try_from(body.len()).map_err(|_| Error::ContainerTooLarge { bytes: total })?;
        out.extend_from_slice(tag);
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        offset = offset
            .checked_add(len)
            .ok_or(Error::ContainerTooLarge { bytes: total })?;
    }
    for (_, body) in bodies {
        out.extend_from_slice(body);
    }
    Ok(out)
}

fn encode_header(header: &Header) -> Vec<u8> {
    let mut out = Vec::new();
    put_str(&mut out, &header.wa_version);
    put_str(&mut out, &header.build_hash);
    put_str(&mut out, &header.modset);
    out.extend_from_slice(&header.flags.to_le_bytes());
    out.push(header.complete as u8);
    out
}

fn decode_header(bytes: &[u8]) -> Result<Header, Error> {
    let mut r = Body::new(bytes, "[hdr]");
    let wa_version = r.string()?;
    let build_hash = r.string()?;
    let modset = r.string()?;
    let flags = r.u16()?;
    let complete = match r.u8()? {
        0 => false,
        1 => true,
        b => {
            return Err(Error::Malformed {
                what: "[hdr]",
                detail: format!("complete flag is {b}, not 0 or 1"),
            })
        }
    };
    r.done()?;
    Ok(Header {
        wa_version,
        build_hash,
        modset,
        flags,
        complete,
    })
}

/// Bounds-checked little-endian/varint reader shared by all section codecs.
pub(crate) struct Body<'a> {
    bytes: &'a [u8],
    pos: usize,
    what: &'static str,
}

impl<'a> Body<'a> {
    pub(crate) fn new(bytes: &'a [u8], what: &'static str) -> Self {
        Self {
            bytes,
            pos: 0,
            what,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pos == self.bytes.len()
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let available = self.bytes.len().saturating_sub(self.pos);
        if available < n {
            return Err(Error::Truncated {
                at: self.pos as Offset,
                wanted: n,
                got: available,
            });
        }
        let start = self.pos;
        self.pos += n;
        Ok(&self.bytes[start..self.pos])
    }

    pub(crate) fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub(crate) fn string(&mut self) -> Result<String, Error> {
        let len = self.u32()? as usize;
        let at = self.pos;
        String::from_utf8(self.take(len)?.to_vec()).map_err(|e| Error::Malformed {
            what: self.what,
            detail: format!("string at byte {at} is not UTF-8: {e}"),
        })
    }

    fn uvar(&mut self) -> Result<u64, Error> {
        let start = self.pos;
        let mut value = 0u64;
        for shift in (0..=63).step_by(7) {
            let b = self.u8()?;
            if shift == 63 && b > 1 {
                return Err(Error::Malformed {
                    what: self.what,
                    detail: format!("varint at byte {start} overflows u64"),
                });
            }
            value |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(Error::Malformed {
            what: self.what,
            detail: format!("varint at byte {start} is too long"),
        })
    }

    fn svar(&mut self) -> Result<i64, Error> {
        let v = self.uvar()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }

    fn u32_var(&mut self) -> Result<u32, Error> {
        let at = self.pos;
        let v = self.uvar()?;
        u32::try_from(v).map_err(|_| Error::Malformed {
            what: self.what,
            detail: format!("integer at byte {at} does not fit u32"),
        })
    }

    fn s32(&mut self) -> Result<i32, Error> {
        let at = self.pos;
        let v = self.svar()?;
        i32::try_from(v).map_err(|_| Error::Malformed {
            what: self.what,
            detail: format!("integer at byte {at} does not fit i32"),
        })
    }

    pub(crate) fn done(&self) -> Result<(), Error> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(Error::Malformed {
                what: self.what,
                detail: format!("{} trailing bytes", self.bytes.len() - self.pos),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete() -> Vec<u8> {
        assemble(
            &Header {
                complete: true,
                ..Header::default()
            },
            b"packs",
            b"wa",
            b"wf",
            b"chk",
        )
        .unwrap()
    }

    #[test]
    fn wrong_version_is_distinct() {
        let mut bytes = complete();
        bytes[4..6].copy_from_slice(&99u16.to_le_bytes());
        assert!(matches!(
            Container::parse(&bytes),
            Err(Error::FormatVersion {
                found: 99,
                supported: FORMAT_VERSION
            })
        ));
    }

    #[test]
    fn truncated_directory_is_refused() {
        let bytes = complete();
        assert!(matches!(
            Container::parse(&bytes[..20]),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn section_outside_file_is_refused() {
        let mut bytes = complete();
        // First entry's offset.
        bytes[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            Container::parse(&bytes),
            Err(Error::SectionBounds { .. })
        ));
    }

    #[test]
    fn malformed_input_record_is_refused() {
        let bytes = encode_inputs(&[InputSample {
            frame: 1,
            keys: 0,
            px: 0,
            py: 0,
            down: false,
        }])
        .unwrap();
        assert!(matches!(
            decode_inputs(&bytes[..bytes.len() - 1]),
            Err(Error::Truncated { .. })
        ));
    }
}
