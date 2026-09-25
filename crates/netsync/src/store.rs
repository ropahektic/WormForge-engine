//! The host's side of a generation: its files in memory, answering requests.
//!
//! Loaded once on a worker thread after the freeze, so the lobby thread only
//! copies bytes out of memory when a request arrives. Every file is re-hashed
//! on load; a session directory someone edited is refused rather than served.

use crate::proto::{Message, Object, CHUNK};
use crate::{digest, Session, PACKS_DIR};
use std::collections::HashMap;

pub struct Served {
    pub generation: u32,
    pub digest: [u8; 32],
    /// Packs in the generation; none means there is nothing to sync.
    pub packs: usize,
    manifest: Vec<u8>,
    blobs: HashMap<[u8; 32], Vec<u8>>,
}

impl Served {
    pub fn load(generation: u32, session: &Session) -> Result<Served, String> {
        let manifest = session.manifest.to_text().into_bytes();
        let mut blobs = HashMap::new();
        for f in &session.manifest.files {
            if blobs.contains_key(&f.sha256) {
                continue;
            }
            let path = session.dir.join(PACKS_DIR).join(&f.pack).join(&f.path);
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if digest(&bytes) != f.sha256 {
                return Err(format!("{} changed since it was frozen", path.display()));
            }
            blobs.insert(f.sha256, bytes);
        }
        Ok(Served {
            generation,
            digest: digest(&manifest),
            packs: session.manifest.packs.len(),
            manifest,
            blobs,
        })
    }

    pub fn offer(&self) -> Message {
        Message::Offer {
            generation: self.generation,
            digest: self.digest,
            size: self.manifest.len() as u32,
        }
    }

    /// The answer to a client's `Get`, or `None` for any other message.
    /// A request for another generation, an unknown file or bytes past the
    /// end gets `Missing`, so the client stops asking.
    pub fn answer(&self, request: &Message) -> Option<Message> {
        let Message::Get { generation, object, offset, len } = request else {
            return None;
        };
        let missing = Message::Missing { generation: *generation, object: *object };
        if *generation != self.generation {
            return Some(missing);
        }
        let whole = match object {
            Object::Manifest => &self.manifest,
            Object::Blob(sha) => match self.blobs.get(sha) {
                Some(b) => b,
                None => return Some(missing),
            },
        };
        let start = *offset as usize;
        let end = start.saturating_add((*len as usize).min(CHUNK));
        if start >= whole.len() || end > whole.len() {
            return Some(missing);
        }
        Some(Message::Data {
            generation: self.generation,
            object: *object,
            offset: *offset,
            total: whole.len() as u32,
            bytes: whole[start..end].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::{Download, Event};
    use crate::{assemble, freeze, proto};
    use std::path::Path;
    use std::time::Instant;
    use weapons::{parse_manifest, LoadedMod};

    fn pack(root: &Path, id: &str, files: &[(&str, Vec<u8>)]) -> LoadedMod {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let toml = format!("id = \"{id}\"\nversion = \"1\"\napi_version = 1\nentry = \"w.lua\"\n");
        std::fs::write(dir.join("mod.toml"), &toml).unwrap();
        for (path, bytes) in files {
            let target = dir.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, bytes).unwrap();
        }
        LoadedMod { manifest: parse_manifest(&toml).unwrap(), root: dir }
    }

    /// Host freezes, client downloads over the wire encoding with a lossy
    /// link, then assembles the same session from what it had plus what came.
    #[test]
    fn a_client_rebuilds_the_hosts_session_over_a_lossy_link() {
        let root = std::env::temp_dir().join(format!("wf-netsync-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let big: Vec<u8> = (0..20_000u32).map(|i| (i * 7 % 256) as u8).collect();
        let shared = b"the client already has this".to_vec();
        let a = pack(&root.join("host"), "t.a", &[("sprites/big.png", big), ("x.lua", shared.clone())]);
        let session = freeze(&[a], &root.join("host-session")).unwrap();
        let served = Served::load(2, &session).unwrap();

        let proto::Message::Offer { generation, digest, size } = served.offer() else { panic!() };
        let mut d = Download::new(generation, digest, size).unwrap();
        let t0 = Instant::now();
        let mut done = None;
        let mut sent = 0;
        let mut manifest = None;
        for round in 0..40u32 {
            let now = t0 + crate::download::TIMEOUT * round;
            for get in d.poll(now).unwrap() {
                sent += 1;
                if sent % 3 == 0 {
                    continue; // every third packet is lost
                }
                let wire = proto::encode(&get);
                let answer = served.answer(&proto::decode(&wire).unwrap()).unwrap();
                let answer = proto::decode(&proto::encode(&answer)).unwrap();
                match d.receive(&answer) {
                    Some(Event::Manifest(m)) => {
                        // The client has x.lua already; it needs the rest.
                        let need: Vec<_> = m
                            .files
                            .iter()
                            .filter(|f| f.sha256 != crate::digest(&shared))
                            .map(|f| (f.sha256, f.size))
                            .collect();
                        manifest = Some(m);
                        if let Some(e) = d.need(&need) {
                            done = Some(e);
                        }
                    }
                    Some(e) => done = Some(e),
                    None => {}
                }
            }
            if done.is_some() {
                break;
            }
        }
        let Some(Event::Complete(received)) = done else { panic!("{done:?}") };
        let manifest = manifest.unwrap();
        let client = assemble(&manifest, &root.join("client-session"), |f| {
            if f.sha256 == crate::digest(&shared) {
                return Ok(shared.clone());
            }
            received.get(&f.sha256).cloned().ok_or_else(|| "not received".into())
        })
        .unwrap();
        assert_eq!(client.manifest.digest(), session.manifest.digest());
        for f in &manifest.files {
            let rel = Path::new(PACKS_DIR).join(&f.pack).join(&f.path);
            assert_eq!(
                std::fs::read(root.join("host-session").join(&rel)).unwrap(),
                std::fs::read(root.join("client-session").join(&rel)).unwrap()
            );
        }
    }

    #[test]
    fn requests_outside_the_generation_get_missing() {
        let root = std::env::temp_dir().join(format!("wf-netsync-served-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let a = pack(&root, "t.a", &[("x", b"abc".to_vec())]);
        let session = freeze(&[a], &root.join("s")).unwrap();
        let served = Served::load(5, &session).unwrap();
        let get = |generation, object, offset, len| Message::Get { generation, object, offset, len };
        let blob = Object::Blob(crate::digest(b"abc"));
        assert!(matches!(served.answer(&get(4, blob, 0, 3)), Some(Message::Missing { .. })));
        assert!(matches!(served.answer(&get(5, Object::Blob([1; 32]), 0, 3)), Some(Message::Missing { .. })));
        assert!(matches!(served.answer(&get(5, blob, 3, 1)), Some(Message::Missing { .. })));
        assert!(matches!(served.answer(&get(5, blob, 1, 2)), Some(Message::Data { .. })));
        assert_eq!(served.answer(&Message::Withdraw { generation: 5 }), None);

        std::fs::write(session.dir.join("packs/t.a/x"), b"abd").unwrap();
        assert!(Served::load(5, &session).err().unwrap().contains("changed since"));
    }
}
