//! The WormForge trailer on a `.WAgame`, and the `[wf]` / `[chk]` streams.
//!
//! ```text
//! <name>.WAgame
//!   WA's own bytes, untouched            wa_len bytes
//!   trailer:
//!     u16 section count, u16 reserved (0)
//!     count x ( tag[4], u32 offset, u32 len )   offsets from trailer start
//!     section bodies: hdr, pack, wf, chk
//!   footer, 32 bytes:
//!     "WFTR", u16 format, u16 flags, u64 trailer_len, u32 crc32(trailer),
//!     u32 wa_len, 8 reserved bytes (0)
//! ```
//!
//! WA reads a `.WAgame` up to its own end-of-stream and ignores what follows
//! (the S0 spike in `docs/replay-wagame-plan.md`), so one file carries both
//! the vanilla replay and everything WormForge needs to re-simulate it.
//!
//! Detection reads only the footer. No magic, or reserved bytes that are not
//! zero, means a vanilla replay: returned as `None`, never an error. Once a
//! footer is recognised, everything else must hold -- format version, lengths
//! that add up to the file, the CRC -- or the file is **refused**: a damaged
//! WormForge replay played as vanilla would be a different match.
//!
//! The trailer is written once, whole, when the round has ended, so having one
//! means the recording is complete. There is no two-phase flag any more.
//!
//! Every reader here **refuses rather than degrades**: an unknown tag, an
//! out-of-bounds or overlapping entry, a frame stream past its cap, frames
//! out of order -- each is its own `Error`, never a best-effort parse.
//! Dropping a frame silently would play back a different match than the one
//! recorded, which is worse than not recording at all.

use crate::error::{Error, Offset};

/// Footer magic: the first four of the file's last 32 bytes.
pub const MAGIC: [u8; 4] = *b"WFTR";
/// Bump whenever any on-disk shape changes -- readers refuse other values
/// with [`Error::FormatVersion`] rather than guessing (gap D6). 1 was the
/// separate `.WF_WAgame` container, which is no longer read.
pub const FORMAT_VERSION: u16 = 2;
/// The fixed footer at the very end of the file.
pub const FOOTER_LEN: usize = 32;

pub const TAG_HDR: [u8; 4] = *b"hdr ";
pub const TAG_PACK: [u8; 4] = *b"pack";
pub const TAG_WF: [u8; 4] = *b"wf  ";
pub const TAG_CHK: [u8; 4] = *b"chk ";

/// Tags this build understands. Anything else is refused (`UnknownSection`):
/// an unknown tag means the writer knew something we do not.
pub const KNOWN_TAGS: [[u8; 4]; 4] = [TAG_HDR, TAG_PACK, TAG_WF, TAG_CHK];

/// The frozen field order of `[hdr]`. Changing this list changes the bytes,
/// so it changes [`FORMAT_VERSION`] with it -- the schema is what a reader
/// of a future build compares against to refuse cleanly.
pub const HDR_SCHEMA: [&str; 4] = ["wa_version", "build_hash", "modset", "collection_version"];

/// Hard ceiling on the encoded `[wf]` stream: 16 MiB is ~1.7M samples, over
/// nine hours at 50 fps -- orders of magnitude past any real match (~1 MB
/// for an hour). The recorder refuses *and stops* at this size rather than
/// producing a file whose stream a reader would then refuse; both sides use
/// the same constant so the two refusals can never disagree.
pub const MAX_WF_BYTES: u32 = 16 * 1024 * 1024;

const WHAT_WF: &str = "[wf] stream";
const WHAT_CHK: &str = "[chk] stream";

/// `[hdr]` -- identity of the recording.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    /// WA's own version string, e.g. `"3.8.1.0"`.
    pub wa_version: String,
    /// The wkLua build that recorded this. Refusing a mismatched build is
    /// W3's job; the field exists so it can.
    pub build_hash: String,
    /// `modset_fingerprint()` at load: `id@version;...`.
    pub modset: String,
    /// The `.WFAssets` release the recorder had loaded (0: none). A replay
    /// whose assets a player's collection lacks names this version, so the
    /// refusal can say which release is needed.
    pub collection_version: u32,
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

const DIRECTORY_HEADER: usize = 4;
const DIRECTORY_ENTRY: usize = 12;

#[derive(Clone, Copy, Debug)]
struct Section {
    tag: [u8; 4],
    offset: Offset,
    len: u32,
}

/// A validated WormForge trailer on a `.WAgame`. Construction validates the
/// footer, the CRC and the whole directory before any section is exposed, so
/// later accessors are only slice lookups.
pub struct Trailer<'a> {
    file: &'a [u8],
    wa_len: usize,
    trailer: &'a [u8],
    sections: Vec<Section>,
    header: Header,
}

impl<'a> Trailer<'a> {
    /// `Ok(None)` for a vanilla `.WAgame`; `Err` for a WormForge replay that
    /// is damaged or from another format version.
    pub fn find(file: &'a [u8]) -> Result<Option<Self>, Error> {
        let Some(footer) = file.len().checked_sub(FOOTER_LEN).map(|at| &file[at..]) else {
            return Ok(None);
        };
        if footer[0..4] != MAGIC || footer[24..32] != [0; 8] {
            return Ok(None);
        }
        let format = u16::from_le_bytes([footer[4], footer[5]]);
        if format != FORMAT_VERSION {
            return Err(Error::FormatVersion {
                found: format,
                supported: FORMAT_VERSION,
            });
        }
        let trailer_len = u64::from_le_bytes(footer[8..16].try_into().unwrap());
        let crc = u32::from_le_bytes(footer[16..20].try_into().unwrap());
        let wa_len = u32::from_le_bytes(footer[20..24].try_into().unwrap()) as u64;
        let body_len = (file.len() - FOOTER_LEN) as u64;
        if wa_len.checked_add(trailer_len) != Some(body_len) {
            return Err(Error::TrailerLength {
                wa_len,
                trailer_len,
                file_len: file.len() as u64,
            });
        }
        let wa_len = wa_len as usize;
        let trailer = &file[wa_len..file.len() - FOOTER_LEN];
        let found = crc32(trailer);
        if found != crc {
            return Err(Error::TrailerCrc {
                expected: crc,
                found,
            });
        }
        let (sections, header) = parse_directory(trailer)?;
        Ok(Some(Self {
            file,
            wa_len,
            trailer,
            sections,
            header,
        }))
    }

    /// WA's own bytes: the whole file up to the trailer, byte-identical to
    /// what WA wrote.
    pub fn wa_prefix(&self) -> &'a [u8] {
        &self.file[..self.wa_len]
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

    pub fn pack(&self) -> &'a [u8] {
        self.section(TAG_PACK)
    }

    pub fn wf(&self) -> &'a [u8] {
        self.section(TAG_WF)
    }

    pub fn chk(&self) -> &'a [u8] {
        self.section(TAG_CHK)
    }

    fn section(&self, tag: [u8; 4]) -> &'a [u8] {
        // Every known tag was proven present and in bounds by `find`.
        section_bytes(self.trailer, &self.sections, tag).unwrap()
    }
}

fn parse_directory(trailer: &[u8]) -> Result<(Vec<Section>, Header), Error> {
    let mut root = Body::new(trailer, "trailer directory");
    let count = root.u16()? as usize;
    root.u16()?;
    if count != KNOWN_TAGS.len() {
        return Err(Error::Malformed {
            what: "trailer directory",
            detail: format!("has {count} sections, expected {}", KNOWN_TAGS.len()),
        });
    }
    let directory_len = DIRECTORY_HEADER + count * DIRECTORY_ENTRY;
    let trailer_len = u32::try_from(trailer.len()).map_err(|_| Error::ContainerTooLarge {
        bytes: trailer.len() as u64,
    })?;
    let mut sections = Vec::with_capacity(count);
    for _ in 0..count {
        let tag: [u8; 4] = root.take(4)?.try_into().unwrap();
        if !KNOWN_TAGS.contains(&tag) {
            return Err(Error::UnknownSection { tag });
        }
        if sections.iter().any(|s: &Section| s.tag == tag) {
            return Err(Error::Malformed {
                what: "trailer directory",
                detail: format!("section {} appears twice", crate::tag_str(&tag)),
            });
        }
        let offset = root.u32()?;
        let len = root.u32()?;
        let end = offset as u64 + len as u64;
        if offset < directory_len as u32 || end > trailer_len as u64 {
            return Err(Error::SectionBounds {
                tag,
                offset,
                len,
                file_len: trailer_len,
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
    // Four distinct known tags, so every one is present.
    let header = decode_header(section_bytes(trailer, &sections, TAG_HDR).unwrap())?;
    Ok((sections, header))
}

fn section_bytes<'a>(bytes: &'a [u8], sections: &[Section], tag: [u8; 4]) -> Option<&'a [u8]> {
    let s = sections.iter().find(|s| s.tag == tag)?;
    let start = s.offset as usize;
    Some(&bytes[start..start + s.len as usize])
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(bytes);
    crc.sum()
}

/// WA's bytes followed by a trailer and footer: the finished file. The
/// section order is frozen with the format version even though the reader
/// uses tags, so identical inputs give identical bytes.
pub fn append_trailer(
    wa: &[u8],
    header: &Header,
    packs: &[u8],
    wf: &[u8],
    checkpoints: &[u8],
) -> Result<Vec<u8>, Error> {
    check_cap(wf)?;
    check_cap(checkpoints)?;
    let wa_len = u32::try_from(wa.len()).map_err(|_| Error::ContainerTooLarge {
        bytes: wa.len() as u64,
    })?;
    let header = encode_header(header);
    let bodies: [([u8; 4], &[u8]); 4] = [
        (TAG_HDR, &header),
        (TAG_PACK, packs),
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

    let mut out = Vec::with_capacity(wa.len() + total as usize + FOOTER_LEN);
    out.extend_from_slice(wa);
    out.extend_from_slice(&(bodies.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    let mut offset = directory_len as u32;
    for (tag, body) in &bodies {
        let len = body.len() as u32;
        out.extend_from_slice(tag);
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        offset += len;
    }
    for (_, body) in bodies {
        out.extend_from_slice(body);
    }
    let crc = crc32(&out[wa.len()..]);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // flags
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&wa_len.to_le_bytes());
    out.extend_from_slice(&[0; 8]);
    Ok(out)
}

fn encode_header(header: &Header) -> Vec<u8> {
    let mut out = Vec::new();
    put_str(&mut out, &header.wa_version);
    put_str(&mut out, &header.build_hash);
    put_str(&mut out, &header.modset);
    out.extend_from_slice(&header.collection_version.to_le_bytes());
    out
}

fn decode_header(bytes: &[u8]) -> Result<Header, Error> {
    let mut r = Body::new(bytes, "[hdr]");
    let wa_version = r.string()?;
    let build_hash = r.string()?;
    let modset = r.string()?;
    let collection_version = r.u32()?;
    r.done()?;
    Ok(Header {
        wa_version,
        build_hash,
        modset,
        collection_version,
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

    const WA: &[u8] = b"WA\x00fake vanilla replay bytes";

    fn file() -> Vec<u8> {
        append_trailer(WA, &Header::default(), b"packs", b"wf", b"chk").unwrap()
    }

    #[test]
    fn a_vanilla_file_is_not_detected() {
        assert!(Trailer::find(WA).unwrap().is_none());
        assert!(Trailer::find(b"").unwrap().is_none());
        // A tail that happens to hold the magic where the footer would start,
        // followed by anything but eight zero bytes, is still vanilla.
        let mut tail = WA.to_vec();
        tail.extend_from_slice(&MAGIC);
        tail.extend_from_slice(&[0xA5; 28]);
        assert!(Trailer::find(&tail).unwrap().is_none());
    }

    #[test]
    fn wrong_version_is_distinct() {
        let mut bytes = file();
        let at = bytes.len() - FOOTER_LEN + 4;
        bytes[at..at + 2].copy_from_slice(&99u16.to_le_bytes());
        assert!(matches!(
            Trailer::find(&bytes),
            Err(Error::FormatVersion {
                found: 99,
                supported: FORMAT_VERSION
            })
        ));
    }

    #[test]
    fn a_truncated_trailer_is_refused() {
        let bytes = file();
        // Cut bytes out of the trailer but keep the footer.
        let mut cut = bytes[..WA.len() + 3].to_vec();
        cut.extend_from_slice(&bytes[bytes.len() - FOOTER_LEN..]);
        assert!(matches!(Trailer::find(&cut), Err(Error::TrailerLength { .. })));
    }

    #[test]
    fn a_bad_crc_is_refused() {
        let mut bytes = file();
        bytes[WA.len() + 20] ^= 1;
        assert!(matches!(Trailer::find(&bytes), Err(Error::TrailerCrc { .. })));
    }

    #[test]
    fn the_wa_prefix_comes_back_byte_identical() {
        let bytes = file();
        let t = Trailer::find(&bytes).unwrap().unwrap();
        assert_eq!(t.wa_prefix(), WA);
        assert_eq!(t.pack(), b"packs");
        assert_eq!(t.wf(), b"wf");
        assert_eq!(t.chk(), b"chk");
    }

    #[test]
    fn section_outside_trailer_is_refused() {
        // Build a directory by hand with a bad offset and a valid CRC, so the
        // bounds check is what trips.
        let mut bytes = file();
        let entry = WA.len() + DIRECTORY_HEADER + 4;
        bytes[entry..entry + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let end = bytes.len() - FOOTER_LEN;
        let crc = crc32(&bytes[WA.len()..end]);
        bytes[end + 16..end + 20].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(Trailer::find(&bytes), Err(Error::SectionBounds { .. })));
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
