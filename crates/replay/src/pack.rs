//! `[pack]`: every pack the round actually ran with -- `mod.toml`, Lua,
//! assets -- as one deflate stream.
//!
//! Packs are *copied*, not fingerprinted. The plan's decisive PX recon found
//! that Project X archives its `.pxl` libraries and reloads them from the
//! replay: "a replay stays playable after you edit or delete the pack". That
//! is the model here, and it is why `[pack]` carries file bytes rather than a
//! hash a future install might no longer match.
//!
//! Except where a file's exact bytes are in the `.WFAssets` collection: then
//! it is recorded as its SHA-256, 32 bytes instead of the file
//! (`encode_dedup`), and playback resolves it from the collection
//! (`resolve`). The collection is append-only across releases, so a
//! reference recorded on release N resolves on every release after it. Lua
//! and `mod.toml` are always embedded, whatever the collection holds.
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

/// One file inside a pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackFile {
    /// Pack-relative path, as the pack declared it (`"weapons.lua"`).
    pub path: String,
    pub content: Content,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    /// The file's bytes, verbatim.
    Raw(Vec<u8>),
    /// A `.WFAssets` reference: the SHA-256 of the file's bytes.
    Asset([u8; 32]),
}

const WHAT: &str = "pack table";
const KIND_RAW: u8 = 0;
const KIND_ASSET: u8 = 1;

/// Files that are never replaced by a reference: the pack's behaviour, which
/// must replay exactly as it was, however the collection evolves.
pub fn always_embedded(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.eq_ignore_ascii_case("mod.toml")
        || name
            .rsplit_once('.')
            .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("lua"))
}

/// [`encode`], with every raw file the collection holds (`has`) recorded as
/// a reference instead -- except Lua and `mod.toml`.
pub fn encode_dedup(packs: &[Pack], has: impl Fn(&[u8; 32]) -> bool) -> Result<Vec<u8>, Error> {
    let deduped: Vec<Pack> = packs
        .iter()
        .map(|p| Pack {
            id: p.id.clone(),
            version: p.version.clone(),
            files: p
                .files
                .iter()
                .map(|f| {
                    let content = match &f.content {
                        Content::Raw(bytes) if !always_embedded(&f.path) => {
                            let sha = assets::sha256::digest(bytes);
                            if has(&sha) {
                                Content::Asset(sha)
                            } else {
                                f.content.clone()
                            }
                        }
                        other => other.clone(),
                    };
                    PackFile {
                        path: f.path.clone(),
                        content,
                    }
                })
                .collect(),
        })
        .collect();
    encode(&deduped)
}

/// Replace every reference with the bytes `get` returns for it. Refuses,
/// naming every hash, if any is missing -- or if `get` returns bytes that do
/// not hash to the reference, which counts as missing.
pub fn resolve(
    packs: Vec<Pack>,
    get: impl Fn(&[u8; 32]) -> Option<Vec<u8>>,
) -> Result<Vec<Pack>, Error> {
    let mut missing = Vec::new();
    let mut out = Vec::with_capacity(packs.len());
    for mut pack in packs {
        for file in &mut pack.files {
            if let Content::Asset(sha) = file.content {
                match get(&sha) {
                    Some(bytes) if assets::sha256::digest(&bytes) == sha => {
                        file.content = Content::Raw(bytes)
                    }
                    _ => {
                        if !missing.contains(&sha) {
                            missing.push(sha);
                        }
                    }
                }
            }
        }
        out.push(pack);
    }
    if missing.is_empty() {
        Ok(out)
    } else {
        Err(Error::MissingAssets { missing })
    }
}

/// All references in the table, deduplicated, in first-seen order.
pub fn asset_refs(packs: &[Pack]) -> Vec<[u8; 32]> {
    let mut out = Vec::new();
    for f in packs.iter().flat_map(|p| &p.files) {
        if let Content::Asset(sha) = f.content {
            if !out.contains(&sha) {
                out.push(sha);
            }
        }
    }
    out
}

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
            match &f.content {
                Content::Raw(raw) => {
                    plain.push(KIND_RAW);
                    plain.extend_from_slice(&(raw.len() as u32).to_le_bytes());
                    plain.extend_from_slice(raw);
                }
                Content::Asset(sha) => {
                    plain.push(KIND_ASSET);
                    plain.extend_from_slice(sha);
                }
            }
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
            let content = match r.u8()? {
                KIND_RAW => {
                    let len = r.u32()? as usize;
                    Content::Raw(r.take(len)?.to_vec())
                }
                KIND_ASSET => Content::Asset(r.take(32)?.try_into().unwrap()),
                kind => {
                    return Err(Error::Malformed {
                        what: WHAT,
                        detail: format!("file {path} has content kind {kind}"),
                    })
                }
            };
            files.push(PackFile { path, content });
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

    fn raw(path: &str, bytes: &[u8]) -> PackFile {
        PackFile {
            path: path.into(),
            content: Content::Raw(bytes.to_vec()),
        }
    }

    fn pack(files: Vec<PackFile>) -> Vec<Pack> {
        vec![Pack {
            id: "p".into(),
            version: "1".into(),
            files,
        }]
    }

    #[test]
    fn collection_files_become_references_and_resolve_back() {
        let png = b"a PX sprite, byte for byte".to_vec();
        let packs = pack(vec![raw("gfx/a.png", &png), raw("gfx/own.png", b"baked here")]);
        let collection = [assets::sha256::digest(&png)];
        let bytes = encode_dedup(&packs, |sha| collection.contains(sha)).unwrap();
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded[0].files[0].content, Content::Asset(collection[0]));
        assert_eq!(decoded[0].files[1].content, Content::Raw(b"baked here".to_vec()));
        assert_eq!(asset_refs(&decoded), collection.to_vec());
        let resolved = resolve(decoded, |sha| (*sha == collection[0]).then(|| png.clone())).unwrap();
        assert_eq!(resolved, packs);
    }

    #[test]
    fn lua_and_mod_toml_are_never_deduped() {
        let packs = pack(vec![
            raw("mod.toml", b"id = 'p'"),
            raw("weapons.lua", b"return {}"),
            raw("sub/Init.LUA", b"return {}"),
        ]);
        // A collection that claims to hold everything.
        let bytes = encode_dedup(&packs, |_| true).unwrap();
        assert_eq!(decode(&bytes).unwrap(), packs);
        assert!(always_embedded("a\\B\\MOD.TOML"));
        assert!(!always_embedded("lua.png"));
    }

    #[test]
    fn every_missing_asset_is_named() {
        let (a, b) = (b"asset a".to_vec(), b"asset b".to_vec());
        let packs = pack(vec![raw("a.png", &a), raw("b.wav", &b), raw("a2.png", &a)]);
        let decoded = decode(&encode_dedup(&packs, |_| true).unwrap()).unwrap();
        let (sa, sb) = (assets::sha256::digest(&a), assets::sha256::digest(&b));
        match resolve(decoded.clone(), |_| None) {
            Err(Error::MissingAssets { missing }) => assert_eq!(missing, vec![sa, sb]),
            other => panic!("{other:?}"),
        }
        // Bytes that do not hash to the reference count as missing.
        match resolve(decoded, |sha| Some(if *sha == sa { a.clone() } else { b"forged".to_vec() })) {
            Err(Error::MissingAssets { missing }) => assert_eq!(missing, vec![sb]),
            other => panic!("{other:?}"),
        }
    }
}
