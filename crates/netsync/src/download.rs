//! A client's download of one host generation.
//!
//! The client pulls; the host only answers. First the manifest, whose SHA-256
//! must equal the offer's digest; then every file the client cannot verify
//! locally, each checked against its hash on arrival. Up to [`WINDOW`] chunk
//! requests are in flight at once, across files, so many small files do not
//! each cost a round trip. A request unanswered for [`TIMEOUT`] is sent again,
//! at most [`TRIES`] times; an answer is the acknowledgement. Stray, late or
//! malformed data is ignored, never trusted.
//!
//! Pure state: the caller moves packets and does the disk work.

use crate::proto::{Message, Object, CHUNK};
use crate::{digest, Manifest};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const WINDOW: usize = 16;
pub const TIMEOUT: Duration = Duration::from_secs(3);
pub const TRIES: u32 = 8;
/// A file that keeps arriving corrupt is given up on after this many fetches.
pub const REFETCHES: u32 = 3;
/// Refuse offers no pack set needs, rather than fill a client's memory.
pub const MAX_MANIFEST: u32 = 8 << 20;
pub const MAX_TOTAL: u64 = 512 << 20;

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// The manifest arrived and matches the offer. Decide which files are
    /// already here and call [`Download::need`] with the rest.
    Manifest(Manifest),
    /// Every needed file arrived and verified: bytes by SHA-256.
    Complete(HashMap<[u8; 32], Vec<u8>>),
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Manifest,
    /// Waiting for [`Download::need`].
    Resolving,
    Files,
    Done,
}

struct Chunk {
    object: Object,
    offset: u32,
    len: u32,
    sent: Option<Instant>,
    tries: u32,
    done: bool,
}

pub struct Download {
    pub generation: u32,
    pub digest: [u8; 32],
    phase: Phase,
    chunks: Vec<Chunk>,
    buffers: HashMap<Object, Vec<u8>>,
    /// Chunks still outstanding per object.
    remaining: HashMap<Object, usize>,
    finished: HashMap<[u8; 32], Vec<u8>>,
    refetched: HashMap<[u8; 32], u32>,
    have: u64,
    total: u64,
}

impl Download {
    pub fn new(generation: u32, digest: [u8; 32], manifest_size: u32) -> Result<Self, String> {
        if manifest_size == 0 || manifest_size > MAX_MANIFEST {
            return Err(format!("offered manifest of {manifest_size} bytes"));
        }
        let mut d = Download {
            generation,
            digest,
            phase: Phase::Manifest,
            chunks: Vec::new(),
            buffers: HashMap::new(),
            remaining: HashMap::new(),
            finished: HashMap::new(),
            refetched: HashMap::new(),
            have: 0,
            total: manifest_size as u64,
        };
        d.add(Object::Manifest, manifest_size as u64);
        Ok(d)
    }

    fn add(&mut self, object: Object, size: u64) {
        let mut offset = 0u64;
        let mut count = 0;
        while offset < size {
            let len = (size - offset).min(CHUNK as u64);
            self.chunks.push(Chunk {
                object,
                offset: offset as u32,
                len: len as u32,
                sent: None,
                tries: 0,
                done: false,
            });
            offset += len;
            count += 1;
        }
        self.buffers.insert(object, vec![0; size as usize]);
        self.remaining.insert(object, count);
    }

    /// Bytes received and verified so far, of the bytes still to fetch.
    pub fn progress(&self) -> (u64, u64) {
        (self.have, self.total)
    }

    /// The files the client has no verified copy of, as (SHA-256, size).
    /// Duplicates are fetched once. Returns `Complete` at once when nothing
    /// is missing.
    pub fn need(&mut self, files: &[([u8; 32], u64)]) -> Option<Event> {
        if self.phase != Phase::Resolving {
            return None;
        }
        let mut total = 0u64;
        for &(sha, size) in files {
            if self.buffers.contains_key(&Object::Blob(sha)) {
                continue;
            }
            if size > u32::MAX as u64 {
                return Some(self.fail(format!("a {size}-byte file is too large to sync")));
            }
            total += size;
            if total > MAX_TOTAL {
                return Some(self.fail(format!("the missing files exceed {MAX_TOTAL} bytes")));
            }
            if size == 0 {
                self.finished.insert(sha, Vec::new());
                continue;
            }
            self.add(Object::Blob(sha), size);
        }
        self.have = 0;
        self.total = total;
        self.phase = Phase::Files;
        self.finish_if_done()
    }

    /// The requests to send now: new ones up to the window, and any whose
    /// answer is overdue. `Err` once a request has run out of tries.
    pub fn poll(&mut self, now: Instant) -> Result<Vec<Message>, String> {
        if !matches!(self.phase, Phase::Manifest | Phase::Files) {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut in_flight = self
            .chunks
            .iter()
            .filter(|c| !c.done && c.sent.is_some_and(|t| now.duration_since(t) < TIMEOUT))
            .count();
        for c in self.chunks.iter_mut().filter(|c| !c.done) {
            let overdue = c.sent.is_some_and(|t| now.duration_since(t) >= TIMEOUT);
            if !(overdue || (c.sent.is_none() && in_flight < WINDOW)) {
                continue;
            }
            if c.tries >= TRIES {
                let what = describe(&c.object);
                let msg = format!("no answer for {what} at byte {} after {TRIES} tries", c.offset);
                self.phase = Phase::Done;
                return Err(msg);
            }
            if !overdue {
                in_flight += 1;
            }
            c.sent = Some(now);
            c.tries += 1;
            out.push(Message::Get {
                generation: self.generation,
                object: c.object,
                offset: c.offset,
                len: c.len,
            });
        }
        Ok(out)
    }

    /// Take a `Data` or `Missing` from the host.
    pub fn receive(&mut self, message: &Message) -> Option<Event> {
        match message {
            Message::Missing { generation, object } if *generation == self.generation => {
                if self.remaining.contains_key(object) && self.phase != Phase::Done {
                    return Some(self.fail(format!("the host no longer has {}", describe(object))));
                }
                None
            }
            Message::Data { generation, object, offset, total, bytes }
                if *generation == self.generation =>
            {
                self.data(*object, *offset, *total, bytes)
            }
            _ => None,
        }
    }

    fn data(&mut self, object: Object, offset: u32, total: u32, bytes: &[u8]) -> Option<Event> {
        let buffer = self.buffers.get_mut(&object)?;
        if total as usize != buffer.len() {
            return None;
        }
        let chunk = self
            .chunks
            .iter_mut()
            .find(|c| !c.done && c.object == object && c.offset == offset)?;
        if bytes.len() != chunk.len as usize {
            return None;
        }
        chunk.done = true;
        buffer[offset as usize..offset as usize + bytes.len()].copy_from_slice(bytes);
        let left = self.remaining.get_mut(&object)?;
        *left -= 1;
        if *left > 0 {
            if object != Object::Manifest {
                self.have += bytes.len() as u64;
            }
            return None;
        }
        let whole = self.buffers.remove(&object)?;
        self.remaining.remove(&object);
        self.chunks.retain(|c| c.object != object);
        match object {
            Object::Manifest => {
                if digest(&whole) != self.digest {
                    return Some(self.fail("the manifest does not match its offer".into()));
                }
                let text = match String::from_utf8(whole) {
                    Ok(t) => t,
                    Err(_) => return Some(self.fail("the manifest is not UTF-8".into())),
                };
                match Manifest::parse(&text) {
                    Ok(m) => {
                        self.phase = Phase::Resolving;
                        Some(Event::Manifest(m))
                    }
                    Err(e) => Some(self.fail(format!("manifest refused: {e}"))),
                }
            }
            Object::Blob(sha) => {
                if digest(&whole) != sha {
                    // Fetch it again from scratch rather than keep bad bytes.
                    let count = {
                        let c = self.refetched.entry(sha).or_insert(0);
                        *c += 1;
                        *c
                    };
                    if count > REFETCHES {
                        let what = describe(&object);
                        return Some(self.fail(format!("{what} arrived corrupt {count} times")));
                    }
                    let size = whole.len() as u64;
                    self.have -= size - bytes.len() as u64;
                    self.add(object, size);
                    return None;
                }
                self.have += bytes.len() as u64;
                self.finished.insert(sha, whole);
                self.finish_if_done()
            }
        }
    }

    fn finish_if_done(&mut self) -> Option<Event> {
        if self.phase == Phase::Files && self.remaining.is_empty() {
            self.phase = Phase::Done;
            return Some(Event::Complete(std::mem::take(&mut self.finished)));
        }
        None
    }

    fn fail(&mut self, reason: String) -> Event {
        self.phase = Phase::Done;
        self.chunks.clear();
        Event::Failed(reason)
    }
}

fn describe(object: &Object) -> String {
    match object {
        Object::Manifest => "the manifest".into(),
        Object::Blob(sha) => format!("file {}", &crate::hex(sha)[..12]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::Object;
    use crate::{FrozenFile, FrozenPack};

    /// A host that answers from memory, optionally dropping or corrupting.
    struct Host {
        manifest: Vec<u8>,
        blobs: HashMap<[u8; 32], Vec<u8>>,
    }

    impl Host {
        fn answer(&self, get: &Message) -> Message {
            let Message::Get { generation, object, offset, len } = get else { panic!() };
            let whole = match object {
                Object::Manifest => &self.manifest,
                Object::Blob(sha) => &self.blobs[sha],
            };
            let end = (*offset + *len) as usize;
            Message::Data {
                generation: *generation,
                object: *object,
                offset: *offset,
                total: whole.len() as u32,
                bytes: whole[*offset as usize..end].to_vec(),
            }
        }
    }

    fn fixture() -> (Host, Manifest, Vec<Vec<u8>>) {
        let files: Vec<Vec<u8>> = vec![
            (0..10_000u32).map(|i| (i % 251) as u8).collect(),
            b"tiny".to_vec(),
            vec![],
        ];
        let mut manifest = Manifest {
            packs: vec![FrozenPack { id: "t.a".into(), version: "1".into() }],
            files: Vec::new(),
        };
        for (i, bytes) in files.iter().enumerate() {
            manifest.files.push(FrozenFile {
                pack: "t.a".into(),
                path: format!("f{i}"),
                size: bytes.len() as u64,
                sha256: digest(bytes),
            });
        }
        let host = Host {
            manifest: manifest.to_text().into_bytes(),
            blobs: files.iter().map(|b| (digest(b), b.clone())).collect(),
        };
        (host, manifest, files)
    }

    fn needed(m: &Manifest) -> Vec<([u8; 32], u64)> {
        m.files.iter().map(|f| (f.sha256, f.size)).collect()
    }

    #[test]
    fn pulls_the_manifest_then_every_needed_file() {
        let (host, manifest, files) = fixture();
        let offered = digest(&host.manifest);
        let mut d = Download::new(4, offered, host.manifest.len() as u32).unwrap();
        let now = Instant::now();
        let mut got = None;
        for _ in 0..50 {
            for get in d.poll(now).unwrap() {
                match d.receive(&host.answer(&get)) {
                    Some(Event::Manifest(m)) => {
                        assert_eq!(m, manifest);
                        if let Some(e) = d.need(&needed(&m)) {
                            got = Some(e);
                        }
                    }
                    Some(e) => got = Some(e),
                    None => {}
                }
            }
            if got.is_some() {
                break;
            }
        }
        let Some(Event::Complete(blobs)) = got else { panic!("{got:?}") };
        for f in &files {
            assert_eq!(&blobs[&digest(f)], f);
        }
        assert_eq!(d.progress(), (10_004, 10_004));
    }

    #[test]
    fn nothing_missing_completes_without_asking() {
        let (host, _, _) = fixture();
        let mut d = Download::new(1, digest(&host.manifest), host.manifest.len() as u32).unwrap();
        let get = d.poll(Instant::now()).unwrap().remove(0);
        let Some(Event::Manifest(_)) = d.receive(&host.answer(&get)) else { panic!() };
        assert_eq!(d.need(&[]), Some(Event::Complete(HashMap::new())));
        assert!(d.poll(Instant::now()).unwrap().is_empty());
    }

    #[test]
    fn a_lost_answer_is_asked_again_and_bad_data_is_ignored() {
        let (host, manifest, _) = fixture();
        let mut d = Download::new(1, digest(&host.manifest), host.manifest.len() as u32).unwrap();
        let t0 = Instant::now();
        let get = d.poll(t0).unwrap().remove(0);
        let Some(Event::Manifest(_)) = d.receive(&host.answer(&get)) else { panic!() };
        d.need(&needed(&manifest));

        let first = d.poll(t0).unwrap();
        assert_eq!(first.len(), 4, "3 chunks of the big file and 1 of the tiny one");
        // Every answer lost: nothing more until the timeout, then all again.
        assert!(d.poll(t0 + Duration::from_secs(1)).unwrap().is_empty());
        let again = d.poll(t0 + TIMEOUT).unwrap();
        assert_eq!(again, first);

        // Wrong generation, wrong total and wrong length are all ignored.
        let Message::Data { object, offset, total, bytes, .. } = host.answer(&again[3]) else {
            panic!()
        };
        let stale = Message::Data { generation: 9, object, offset, total, bytes: bytes.clone() };
        assert_eq!(d.receive(&stale), None);
        let short = Message::Data { generation: 1, object, offset, total, bytes: bytes[1..].to_vec() };
        assert_eq!(d.receive(&short), None);
        let good = Message::Data { generation: 1, object, offset, total, bytes };
        assert_eq!(d.receive(&good), None);
        assert_eq!(d.receive(&good), None, "a duplicate is harmless");
        assert_eq!(d.progress(), (4, 10_004));
    }

    #[test]
    fn a_corrupt_file_is_fetched_again() {
        let (host, manifest, files) = fixture();
        let mut d = Download::new(1, digest(&host.manifest), host.manifest.len() as u32).unwrap();
        let now = Instant::now();
        let get = d.poll(now).unwrap().remove(0);
        let Some(Event::Manifest(_)) = d.receive(&host.answer(&get)) else { panic!() };
        d.need(&[(manifest.files[1].sha256, 4)]);
        let get = d.poll(now).unwrap().remove(0);
        let Message::Data { generation, object, offset, total, .. } = host.answer(&get) else {
            panic!()
        };
        let bad = Message::Data { generation, object, offset, total, bytes: b"tInY".to_vec() };
        assert_eq!(d.receive(&bad), None);
        assert_eq!(d.progress(), (0, 4));
        let retry = d.poll(now).unwrap().remove(0);
        let Some(Event::Complete(blobs)) = d.receive(&host.answer(&retry)) else { panic!() };
        assert_eq!(blobs[&digest(&files[1])], files[1]);
    }

    #[test]
    fn a_forged_manifest_and_a_silent_host_fail() {
        let (host, _, _) = fixture();
        let mut d = Download::new(1, [0; 32], host.manifest.len() as u32).unwrap();
        let get = d.poll(Instant::now()).unwrap().remove(0);
        let Some(Event::Failed(why)) = d.receive(&host.answer(&get)) else { panic!() };
        assert!(why.contains("does not match"), "{why}");

        let mut d = Download::new(1, [0; 32], 10).unwrap();
        let t0 = Instant::now();
        let mut err = None;
        for i in 0..=TRIES {
            if let Err(e) = d.poll(t0 + TIMEOUT * i) {
                err = Some(e);
            }
        }
        assert!(err.unwrap().contains("after 8 tries"));
        assert!(Download::new(1, [0; 32], MAX_MANIFEST + 1).is_err());
    }

    #[test]
    fn a_withdrawn_generation_fails_cleanly() {
        let (host, _, _) = fixture();
        let mut d = Download::new(1, digest(&host.manifest), host.manifest.len() as u32).unwrap();
        d.poll(Instant::now()).unwrap();
        let gone = Message::Missing { generation: 1, object: Object::Manifest };
        assert!(matches!(d.receive(&gone), Some(Event::Failed(_))));
        assert!(d.poll(Instant::now()).unwrap().is_empty());
    }
}
