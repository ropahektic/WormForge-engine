//! Minimal ZIP reader for pack downloads: stored and deflate entries, no
//! zip64, no encryption. About 150 lines on top of the `flate2` inflater the
//! replay crate already links, instead of a full archive crate in `wkLua.dll`.

use flate2::read::DeflateDecoder;
use std::io::Read;
use std::path::{Path, PathBuf};

const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_LOCAL: u32 = 0x0403_4b50;
const METHOD_STORED: u16 = 0;
const METHOD_DEFLATE: u16 = 8;

#[derive(Debug, Clone)]
pub struct Entry {
    /// Forward-slash path as stored, before any safety filtering.
    pub name: String,
    method: u16,
    crc: u32,
    compressed: usize,
    uncompressed: usize,
    local_offset: usize,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }
}

fn u16_at(d: &[u8], o: usize) -> Result<u16, String> {
    d.get(o..o + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| "zip: truncated".into())
}

fn u32_at(d: &[u8], o: usize) -> Result<u32, String> {
    d.get(o..o + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| "zip: truncated".into())
}

/// Central-directory listing. Windows-made archives use `\` separators; they
/// are normalised to `/` here.
pub fn entries(data: &[u8]) -> Result<Vec<Entry>, String> {
    if data.len() < 22 {
        return Err("zip: too small".into());
    }
    let floor = data.len().saturating_sub(22 + 65535);
    let mut eocd = None;
    let mut pos = data.len() - 22;
    loop {
        if u32_at(data, pos)? == SIG_EOCD {
            eocd = Some(pos);
            break;
        }
        if pos == floor {
            break;
        }
        pos -= 1;
    }
    let eocd = eocd.ok_or("zip: no end-of-central-directory record")?;
    let count = u16_at(data, eocd + 10)? as usize;
    let cd_offset = u32_at(data, eocd + 16)? as usize;
    if count == 0xFFFF || cd_offset == 0xFFFF_FFFF {
        return Err("zip: zip64 archives are not supported".into());
    }

    let mut out = Vec::with_capacity(count);
    let mut p = cd_offset;
    for _ in 0..count {
        if u32_at(data, p)? != SIG_CENTRAL {
            return Err("zip: bad central directory entry".into());
        }
        let method = u16_at(data, p + 10)?;
        let crc = u32_at(data, p + 16)?;
        let compressed = u32_at(data, p + 20)? as usize;
        let uncompressed = u32_at(data, p + 24)? as usize;
        let name_len = u16_at(data, p + 28)? as usize;
        let extra_len = u16_at(data, p + 30)? as usize;
        let comment_len = u16_at(data, p + 32)? as usize;
        let local_offset = u32_at(data, p + 42)? as usize;
        let name_bytes = data
            .get(p + 46..p + 46 + name_len)
            .ok_or("zip: truncated name")?;
        let name = String::from_utf8_lossy(name_bytes).replace('\\', "/");
        out.push(Entry {
            name,
            method,
            crc,
            compressed,
            uncompressed,
            local_offset,
        });
        p += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

/// Inflate one entry and verify its CRC-32.
pub fn read(data: &[u8], e: &Entry) -> Result<Vec<u8>, String> {
    let lo = e.local_offset;
    if u32_at(data, lo)? != SIG_LOCAL {
        return Err(format!("zip: bad local header for {}", e.name));
    }
    let name_len = u16_at(data, lo + 26)? as usize;
    let extra_len = u16_at(data, lo + 28)? as usize;
    let start = lo + 30 + name_len + extra_len;
    let raw = data
        .get(start..start + e.compressed)
        .ok_or_else(|| format!("zip: truncated data for {}", e.name))?;
    let bytes = match e.method {
        METHOD_STORED => raw.to_vec(),
        METHOD_DEFLATE => {
            let mut out = Vec::with_capacity(e.uncompressed);
            DeflateDecoder::new(raw)
                .read_to_end(&mut out)
                .map_err(|err| format!("zip: inflate {}: {err}", e.name))?;
            out
        }
        m => return Err(format!("zip: {} uses unsupported method {m}", e.name)),
    };
    if bytes.len() != e.uncompressed {
        return Err(format!("zip: {} size mismatch", e.name));
    }
    let mut crc = flate2::Crc::new();
    crc.update(&bytes);
    if crc.sum() != e.crc {
        return Err(format!("zip: {} CRC mismatch", e.name));
    }
    Ok(bytes)
}

/// Reject anything that could escape `dest`: absolute paths, drive letters,
/// `..` components.
pub fn safe_relative(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in name.split('/') {
        match part {
            "" | "." => continue,
            ".." => return None,
            p if p.contains(':') => return None,
            p => out.push(p),
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Extract every entry under `dest`. Returns the number of files written.
pub fn extract_all(data: &[u8], dest: &Path) -> Result<usize, String> {
    let list = entries(data)?;
    let mut files = 0;
    for e in &list {
        let Some(rel) = safe_relative(&e.name) else {
            continue;
        };
        let target = dest.join(rel);
        if e.is_dir() {
            std::fs::create_dir_all(&target).map_err(|err| err.to_string())?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        let bytes = read(data, e)?;
        // Updating a pack overwrites its files; std::fs::write cannot under
        // Wine. See game::overwrite.
        game::overwrite::write(&target, &bytes)
            .map_err(|err| format!("write {}: {err}", target.display()))?;
        files += 1;
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::DeflateEncoder;
    use std::io::Write;

    /// Tiny writer so the test does not depend on an external archive.
    fn build_zip(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, body, deflate) in files {
            let mut crc = flate2::Crc::new();
            crc.update(body);
            let (method, data) = if *deflate {
                let mut enc = DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                enc.write_all(body).unwrap();
                (8u16, enc.finish().unwrap())
            } else {
                (0u16, body.to_vec())
            };
            let local_offset = out.len() as u32;
            let mut hdr = Vec::new();
            hdr.extend_from_slice(&SIG_LOCAL.to_le_bytes());
            hdr.extend_from_slice(&[20, 0, 0, 0]);
            hdr.extend_from_slice(&method.to_le_bytes());
            hdr.extend_from_slice(&[0; 4]);
            hdr.extend_from_slice(&crc.sum().to_le_bytes());
            hdr.extend_from_slice(&(data.len() as u32).to_le_bytes());
            hdr.extend_from_slice(&(body.len() as u32).to_le_bytes());
            hdr.extend_from_slice(&(name.len() as u16).to_le_bytes());
            hdr.extend_from_slice(&[0, 0]);
            hdr.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&hdr);
            out.extend_from_slice(&data);

            central.extend_from_slice(&SIG_CENTRAL.to_le_bytes());
            central.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
            central.extend_from_slice(&method.to_le_bytes());
            central.extend_from_slice(&[0; 4]);
            central.extend_from_slice(&crc.sum().to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(body.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0; 12]);
            central.extend_from_slice(&local_offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let cd_offset = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&SIG_EOCD.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    #[test]
    fn extracts_stored_and_deflated_with_backslash_names() {
        let big = vec![b'x'; 5000];
        let zip = build_zip(&[
            ("pack\\mod.toml", b"id = \"a\"", false),
            ("pack/weapons.lua", &big, true),
            ("pack/sub/", b"", false),
            ("../escape.txt", b"nope", false),
        ]);
        let list = entries(&zip).unwrap();
        assert_eq!(list.len(), 4);
        assert_eq!(list[0].name, "pack/mod.toml");
        assert_eq!(read(&zip, &list[1]).unwrap(), big);

        let dest = std::env::temp_dir().join(format!("wf-zip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        assert_eq!(extract_all(&zip, &dest).unwrap(), 2);
        assert_eq!(std::fs::read_to_string(dest.join("pack/mod.toml")).unwrap(), "id = \"a\"");
        assert!(dest.join("pack/sub").is_dir());
        assert!(!dest.parent().unwrap().join("escape.txt").exists());
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn corrupt_data_is_refused() {
        let mut zip = build_zip(&[("f.txt", b"hello world", true)]);
        let list = entries(&zip).unwrap();
        zip[list[0].local_offset + 30 + 5] ^= 0xff;
        assert!(read(&zip, &list[0]).is_err());
        assert!(entries(b"not a zip at all, definitely not").is_err());
        assert_eq!(safe_relative("C:/x"), None);
        assert_eq!(safe_relative("/a/b"), Some(PathBuf::from("a/b")));
    }
}
