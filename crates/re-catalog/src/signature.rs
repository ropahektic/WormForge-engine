//! Masked signatures against the unmodified PE file, never a live detour's bytes.

use serde::Deserialize;
use std::collections::HashSet;

#[derive(Debug, Clone, Deserialize)]
pub struct Locator {
    pub name: String,
    #[serde(default = "required_by_default")]
    pub required: bool,
    /// Ghidra VA for documentation and offline audits. Resolution never uses it.
    pub reference_va: u32,
    pub section: String,
    pub pattern: String,
    #[serde(default)]
    pub derive: Derive,
    #[serde(default)]
    pub addend: i32,
    pub operand: Option<usize>,
    pub target_section: String,
}

fn required_by_default() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Derive {
    #[default]
    Match,
    Abs32,
    Rel32,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub name: String,
    pub rva: u32,
    pub reference_va: u32,
}

#[derive(Debug, Deserialize)]
struct SignatureFile {
    locator: Vec<Locator>,
}

pub fn builtin() -> Result<Vec<Locator>, String> {
    parse(include_str!("../../../re/Signatures.toml"))
}

pub fn parse(src: &str) -> Result<Vec<Locator>, String> {
    let parsed: SignatureFile = toml::from_str(src).map_err(|e| e.to_string())?;
    let mut names = HashSet::new();
    for locator in &parsed.locator {
        if !names.insert(locator.name.as_str()) {
            return Err(format!("duplicate WA locator {}", locator.name));
        }
        parse_pattern(&locator.pattern)?;
        if !matches!(locator.derive, Derive::Match) && locator.operand.is_none() {
            return Err(format!("{} needs an operand offset", locator.name));
        }
    }
    Ok(parsed.locator)
}

pub fn parse_pattern(src: &str) -> Result<Vec<Option<u8>>, String> {
    let mut pattern = Vec::new();
    for token in src.split_ascii_whitespace() {
        let byte = match token {
            "??" => None,
            _ if token.len() == 2 => Some(
                u8::from_str_radix(token, 16)
                    .map_err(|_| format!("invalid signature byte {token:?}"))?,
            ),
            _ => return Err(format!("invalid signature byte {token:?}")),
        };
        pattern.push(byte);
    }
    if pattern.len() < 8 || pattern.iter().filter(|b| b.is_some()).count() < 8 {
        return Err("signature needs at least eight fixed bytes".into());
    }
    Ok(pattern)
}

#[derive(Debug, Clone)]
struct Section {
    name: String,
    rva: u32,
    virtual_size: u32,
    raw_offset: usize,
    raw_size: usize,
}

pub struct PeImage<'a> {
    bytes: &'a [u8],
    image_base: u32,
    image_size: u32,
    timestamp: u32,
    sections: Vec<Section>,
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, String> {
    let b = bytes.get(at..at + 2).ok_or("truncated PE u16")?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    let b = bytes.get(at..at + 4).ok_or("truncated PE u32")?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

impl<'a> PeImage<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.get(..2) != Some(b"MZ") {
            return Err("WA image is not MZ".into());
        }
        let pe = u32_at(bytes, 0x3c)? as usize;
        if bytes.get(pe..pe + 4) != Some(b"PE\0\0") || u16_at(bytes, pe + 4)? != 0x14c {
            return Err("WA image is not PE32 x86".into());
        }
        let count = u16_at(bytes, pe + 6)? as usize;
        let optional_len = u16_at(bytes, pe + 20)? as usize;
        let opt = pe + 24;
        if optional_len < 60 || u16_at(bytes, opt)? != 0x10b || count == 0 || count > 96 {
            return Err("invalid PE32 optional header or section count".into());
        }
        let image_base = u32_at(bytes, opt + 28)?;
        let image_size = u32_at(bytes, opt + 56)?;
        let timestamp = u32_at(bytes, pe + 8)?;
        let table = opt.checked_add(optional_len).ok_or("PE header overflow")?;
        let mut sections = Vec::with_capacity(count);
        for i in 0..count {
            let at = table.checked_add(i * 40).ok_or("PE section overflow")?;
            let name_bytes = bytes.get(at..at + 8).ok_or("truncated section table")?;
            let end = name_bytes.iter().position(|&b| b == 0).unwrap_or(8);
            let name = String::from_utf8_lossy(&name_bytes[..end]).into_owned();
            let virtual_size = u32_at(bytes, at + 8)?;
            let rva = u32_at(bytes, at + 12)?;
            let raw_size = u32_at(bytes, at + 16)? as usize;
            let raw_offset = u32_at(bytes, at + 20)? as usize;
            if rva
                .checked_add(virtual_size)
                .is_none_or(|end| end > image_size)
                || raw_offset
                    .checked_add(raw_size)
                    .is_none_or(|end| end > bytes.len())
            {
                return Err(format!("invalid PE section {name}"));
            }
            sections.push(Section {
                name,
                rva,
                virtual_size,
                raw_offset,
                raw_size,
            });
        }
        Ok(Self {
            bytes,
            image_base,
            image_size,
            timestamp,
            sections,
        })
    }

    pub fn image_size(&self) -> u32 {
        self.image_size
    }

    pub fn timestamp(&self) -> u32 {
        self.timestamp
    }

    fn section(&self, name: &str) -> Result<&Section, String> {
        self.sections
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("missing PE section {name}"))
    }

    pub fn bytes_at_rva(&self, rva: u32, len: usize) -> Option<&'a [u8]> {
        let section = self
            .sections
            .iter()
            .find(|s| rva >= s.rva && rva < s.rva + s.virtual_size)?;
        let offset = (rva - section.rva) as usize;
        if offset.checked_add(len)? > section.raw_size {
            return None;
        }
        self.bytes
            .get(section.raw_offset + offset..section.raw_offset + offset + len)
    }

    pub fn resolve(&self, locator: &Locator) -> Result<Resolved, String> {
        let section = self.section(&locator.section)?;
        let data_len = section.raw_size.min(section.virtual_size as usize);
        let data = &self.bytes[section.raw_offset..section.raw_offset + data_len];
        let pattern = parse_pattern(&locator.pattern)?;
        if pattern.len() > data.len() {
            return Err(format!(
                "{}: signature longer than {}",
                locator.name, section.name
            ));
        }
        let mut found = None;
        for at in 0..=data.len() - pattern.len() {
            if pattern
                .iter()
                .enumerate()
                .all(|(i, b)| b.is_none_or(|v| data[at + i] == v))
            {
                if found.replace(at).is_some() {
                    return Err(format!("{}: ambiguous signature", locator.name));
                }
            }
        }
        let at = found.ok_or_else(|| format!("{}: signature not found", locator.name))?;
        let match_rva = section.rva + at as u32;
        let base = match locator.derive {
            Derive::Match => match_rva,
            Derive::Abs32 => {
                let op = locator.operand.unwrap();
                let pos = at.checked_add(op).ok_or("operand overflow")?;
                if pos.checked_add(4).is_none_or(|end| end > data.len()) {
                    return Err(format!("{}: operand outside section", locator.name));
                }
                u32_at(data, pos)?
                    .checked_sub(self.image_base)
                    .ok_or_else(|| format!("{}: absolute operand below image base", locator.name))?
            }
            Derive::Rel32 => {
                let op = locator.operand.unwrap();
                let pos = at.checked_add(op).ok_or("operand overflow")?;
                if pos.checked_add(4).is_none_or(|end| end > data.len()) {
                    return Err(format!("{}: operand outside section", locator.name));
                }
                let displacement = u32_at(data, pos)? as i32 as i64;
                let target = match_rva as i64 + op as i64 + 4 + displacement;
                u32::try_from(target)
                    .map_err(|_| format!("{}: relative operand overflow", locator.name))?
            }
        };
        let target = base as i64 + locator.addend as i64;
        let rva =
            u32::try_from(target).map_err(|_| format!("{}: result overflow", locator.name))?;
        let wanted = self.section(&locator.target_section)?;
        if rva < wanted.rva || rva >= wanted.rva + wanted.virtual_size {
            return Err(format!("{}: result outside {}", locator.name, wanted.name));
        }
        Ok(Resolved {
            name: locator.name.clone(),
            rva,
            reference_va: locator.reference_va,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        let mut pe = vec![0u8; 0x800];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        pe[0x84..0x86].copy_from_slice(&0x14cu16.to_le_bytes());
        pe[0x86..0x88].copy_from_slice(&2u16.to_le_bytes());
        pe[0x94..0x96].copy_from_slice(&0xe0u16.to_le_bytes());
        pe[0x98..0x9a].copy_from_slice(&0x10bu16.to_le_bytes());
        pe[0xb4..0xb8].copy_from_slice(&0x400000u32.to_le_bytes());
        pe[0xd0..0xd4].copy_from_slice(&0x3000u32.to_le_bytes());
        for (index, (name, rva, raw)) in [
            (b".text\0\0\0" as &[u8; 8], 0x1000u32, 0x200u32),
            (b".data\0\0\0", 0x2000u32, 0x400u32),
        ]
        .iter()
        .enumerate()
        {
            let at = 0x178 + index * 40;
            pe[at..at + 8].copy_from_slice(*name);
            pe[at + 8..at + 12].copy_from_slice(&0x100u32.to_le_bytes());
            pe[at + 12..at + 16].copy_from_slice(&rva.to_le_bytes());
            pe[at + 16..at + 20].copy_from_slice(&0x100u32.to_le_bytes());
            pe[at + 20..at + 24].copy_from_slice(&raw.to_le_bytes());
        }
        pe[0x210..0x21c].copy_from_slice(&[
            0x55, 0x8b, 0xec, 0xe8, 0x10, 0, 0, 0, 0x8b, 0xc0, 0xc3, 0x90,
        ]);
        pe[0x230..0x23d].copy_from_slice(&[
            0xa1, 0x20, 0x20, 0x40, 0, 0x85, 0xc0, 0x74, 0x03, 0x33, 0xc0, 0xc3, 0x90,
        ]);
        pe
    }

    fn locator(pattern: &str, derive: Derive, operand: Option<usize>, target: &str) -> Locator {
        Locator {
            name: "test".into(),
            required: true,
            reference_va: 0,
            section: ".text".into(),
            pattern: pattern.into(),
            derive,
            addend: 0,
            operand,
            target_section: target.into(),
        }
    }

    #[test]
    fn resolves_masked_match_and_relative_and_absolute_operands() {
        let data = fixture();
        let pe = PeImage::parse(&data).unwrap();
        let direct = locator(
            "55 8B EC E8 ?? ?? ?? ?? 8B C0 C3 90",
            Derive::Match,
            None,
            ".text",
        );
        assert_eq!(pe.resolve(&direct).unwrap().rva, 0x1010);
        let rel = locator(
            "55 8B EC E8 ?? ?? ?? ?? 8B C0 C3 90",
            Derive::Rel32,
            Some(4),
            ".text",
        );
        assert_eq!(pe.resolve(&rel).unwrap().rva, 0x1028);
        let abs = locator(
            "A1 ?? ?? ?? ?? 85 C0 74 ?? 33 C0 C3 90",
            Derive::Abs32,
            Some(1),
            ".data",
        );
        assert_eq!(pe.resolve(&abs).unwrap().rva, 0x2020);
    }

    #[test]
    fn refuses_missing_ambiguous_and_out_of_section() {
        let mut data = fixture();
        let pe = PeImage::parse(&data).unwrap();
        let l = locator(
            "55 8B EC E8 ?? ?? ?? ?? 8B C0 C3 90",
            Derive::Match,
            None,
            ".text",
        );
        let mut missing = l.clone();
        missing.pattern = "55 8B EC E8 ?? ?? ?? ?? 8B C1 C3 90".into();
        assert!(pe.resolve(&missing).unwrap_err().contains("not found"));
        let mut wrong_section = l.clone();
        wrong_section.target_section = ".data".into();
        assert!(pe.resolve(&wrong_section).unwrap_err().contains("outside"));
        let duplicate = data[0x210..0x21c].to_vec();
        data[0x250..0x25c].copy_from_slice(&duplicate);
        assert!(PeImage::parse(&data)
            .unwrap()
            .resolve(&l)
            .unwrap_err()
            .contains("ambiguous"));
    }

    #[test]
    fn audit_installed_wa_when_provided() {
        let Ok(path) = std::env::var("WF_WA_EXE") else {
            return;
        };
        let data = std::fs::read(path).unwrap();
        let pe = PeImage::parse(&data).unwrap();
        let locators = builtin().unwrap();
        for locator in &locators {
            let result = pe.resolve(locator).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                result.rva + 0x400000,
                locator.reference_va,
                "{}",
                locator.name
            );
        }
        assert!(locators.len() >= 70);
    }

    #[test]
    fn every_runtime_address_lookup_has_a_locator() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../engine/src");
        let names: HashSet<String> = builtin().unwrap().into_iter().map(|l| l.name).collect();
        for file in std::fs::read_dir(root).unwrap() {
            let path = file.unwrap().path();
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            if path.file_name().unwrap() != "wa.rs" {
                assert!(
                    !source.contains("MinHook::create_hook("),
                    "{} bypasses hook validation",
                    path.display()
                );
                assert!(
                    !source.contains("rb("),
                    "{} uses a fixed WA VA",
                    path.display()
                );
            }
            for marker in [
                "wa::address(\"",
                "wa::try_address(\"",
                "wa::check_callable(\"",
            ] {
                for tail in source.split(marker).skip(1) {
                    let name = tail.split('"').next().unwrap();
                    assert!(
                        names.contains(name),
                        "{} uses missing locator {name}",
                        path.display()
                    );
                }
            }
        }
    }
}
