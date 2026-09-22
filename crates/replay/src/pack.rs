//! `[pack]`: every pack the round actually ran with -- `mod.toml`, Lua,
//! assets -- as one deflate stream.
//!
//! Packs are *copied*, not fingerprinted. The plan's decisive PX recon found
//! that Project X archives its `.pxl` libraries and reloads them from the
//! replay: "a replay stays playable after you edit or delete the pack". That
//! is the model here, and it is why `[pack]` carries file bytes rather than a
//! hash a future install might no longer match.
//!
//! The table is written by walking fixed structs (never a live registry),
//! then compressed as a whole so the codec choice is invisible to the layout.

use crate::error::Error;
use crate::format::{put_str, Body};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::{Read, Write};

pub const MAX_PACKS: usize = 4096;
pub const MAX_PACK_FILES: usize = 100_000;
pub const MAX_PACK_BYTES: usize = 256 * 1024 * 1024;

/// One installed pack, as loaded for the round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pack {
    pub id: String,
    pub version: String,
    pub files: Vec<PackFile>,
}

/// One file inside a pack, bytes verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackFile {
    /// Pack-relative path, as the pack declared it (`"weapons.lua"`).
    pub path: String,
    pub raw: Vec<u8>,
}

const WHAT: &str = "pack table";

/// Serialise the table, then gzip it. Gzip over raw deflate deliberately:
/// its CRC catches a torn or bit-rotted payload as `PackCodec` at read time
/// instead of handing half a pack script to the loader.
pub fn encode(packs: &[Pack]) -> Result<Vec<u8>, Error> {
    if packs.len() > MAX_PACKS {
        return Err(Error::PackLimit(format!(
            "{} packs, maximum is {MAX_PACKS}",
            packs.len()
        )));
    }
    let mut plain = Vec::new();
    let mut file_count = 0usize;
    plain.extend_from_slice(&(packs.len() as u32).to_le_bytes());
    for p in packs {
        file_count = file_count
            .checked_add(p.files.len())
            .ok_or_else(|| Error::PackLimit("file count overflow".into()))?;
        if file_count > MAX_PACK_FILES {
            return Err(Error::PackLimit(format!(
                "{file_count} files, maximum is {MAX_PACK_FILES}"
            )));
        }
        put_str(&mut plain, &p.id);
        put_str(&mut plain, &p.version);
        plain.extend_from_slice(&(p.files.len() as u32).to_le_bytes());
        for f in &p.files {
            put_str(&mut plain, &f.path);
            plain.extend_from_slice(&(f.raw.len() as u32).to_le_bytes());
            plain.extend_from_slice(&f.raw);
            if plain.len() > MAX_PACK_BYTES {
                return Err(Error::PackLimit(format!(
                    "{} inflated bytes, maximum is {MAX_PACK_BYTES}",
                    plain.len()
                )));
            }
        }
    }
    if plain.len() > MAX_PACK_BYTES {
        return Err(Error::PackLimit(format!(
            "{} inflated bytes, maximum is {MAX_PACK_BYTES}",
            plain.len()
        )));
    }
    let mut e = GzEncoder::new(Vec::new(), Compression::default());
    e.write_all(&plain)
        .map_err(|e| Error::PackCodec(e.to_string()))?;
    e.finish().map_err(|e| Error::PackCodec(e.to_string()))
}

/// Inflate, then walk the table. Counts are read one entry at a time against
/// the remaining bytes, so a forged `u32::MAX` count fails on the first
/// entry instead of reserving gigabytes.
pub fn decode(bytes: &[u8]) -> Result<Vec<Pack>, Error> {
    let mut plain = Vec::new();
    GzDecoder::new(bytes)
        .take((MAX_PACK_BYTES + 1) as u64)
        .read_to_end(&mut plain)
        .map_err(|e| Error::PackCodec(e.to_string()))?;
    if plain.len() > MAX_PACK_BYTES {
        return Err(Error::PackLimit(format!(
            "more than {MAX_PACK_BYTES} inflated bytes"
        )));
    }
    let mut r = Body::new(&plain, WHAT);
    let count = r.u32()? as usize;
    if count > MAX_PACKS {
        return Err(Error::PackLimit(format!(
            "{count} packs, maximum is {MAX_PACKS}"
        )));
    }
    let mut packs = Vec::new();
    let mut file_count = 0usize;
    for _ in 0..count {
        let id = r.string()?;
        let version = r.string()?;
        let files_len = r.u32()? as usize;
        file_count = file_count
            .checked_add(files_len)
            .ok_or_else(|| Error::PackLimit("file count overflow".into()))?;
        if file_count > MAX_PACK_FILES {
            return Err(Error::PackLimit(format!(
                "{file_count} files, maximum is {MAX_PACK_FILES}"
            )));
        }
        let mut files = Vec::new();
        for _ in 0..files_len {
            let path = r.string()?;
            let len = r.u32()? as usize;
            let raw = r.take(len)?.to_vec();
            files.push(PackFile { path, raw });
        }
        packs.push(Pack { id, version, files });
    }
    r.done()?;
    Ok(packs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excessive_pack_count_is_refused_before_allocation() {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&((MAX_PACKS as u32) + 1).to_le_bytes())
            .unwrap();
        let bytes = encoder.finish().unwrap();
        assert!(matches!(decode(&bytes), Err(Error::PackLimit(_))));
    }
}
