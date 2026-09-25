//! WormForge's lobby packets.
//!
//! Every packet starts with WA's own two-byte packet type, [`PACKET_ID`], so it
//! travels through WA's lobby connections like any other packet. WA 3.8.1
//! ignores a type it does not know on both sides (the host's lobby handler
//! passes it to 0x4AC0D0, the client's to 0x4BB1F0; both return 0), so a peer
//! without WormForge drops these silently. wkTerrainSync uses 0x6666; this is
//! a different id so neither module has to pass the other's packets on.
//!
//! After the type: a protocol version byte, a message kind byte, then the
//! message, little-endian. A peer refuses a version it does not speak rather
//! than guessing at the layout.

/// "WF" in the first two bytes on the wire.
pub const PACKET_ID: u16 = 0x4657;
pub const PROTOCOL: u8 = 1;
/// Largest `Data` payload. WA's send copies into a buffer sized by a u16, and
/// wkTerrainSync's 5000-byte chunks are known to cross the lobby intact.
pub const CHUNK: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Object {
    /// The generation's manifest text; its SHA-256 is the offer's digest.
    Manifest,
    /// A pack file, by the SHA-256 of its bytes.
    Blob([u8; 32]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Still fetching: `have` of `total` bytes.
    Working,
    /// Every file verified and the session directory published.
    Ready,
    /// Gave up; `text` says why.
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// "I run WormForge": either direction, so each side knows the other's
    /// build. A client sends it on joining; the host answers with its own and
    /// the current offer.
    Hello { build: String },
    /// Host: generation `generation` is frozen; its manifest is `size` bytes
    /// with SHA-256 `digest`.
    Offer { generation: u32, digest: [u8; 32], size: u32 },
    /// Host: the generation is gone (the bulb went out).
    Withdraw { generation: u32 },
    /// Client: send `len` bytes of `object` from `offset`.
    Get { generation: u32, object: Object, offset: u32, len: u32 },
    /// Host: `bytes` of `object` from `offset`; the object is `total` bytes.
    Data { generation: u32, object: Object, offset: u32, total: u32, bytes: Vec<u8> },
    /// Host: cannot serve `object` for `generation` (unknown or withdrawn).
    Missing { generation: u32, object: Object },
    /// Client: where it stands with `generation`.
    Status { generation: u32, digest: [u8; 32], state: State, have: u64, total: u64, text: String },
}

const HELLO: u8 = 1;
const OFFER: u8 = 2;
const WITHDRAW: u8 = 3;
const GET: u8 = 4;
const DATA: u8 = 5;
const MISSING: u8 = 6;
const STATUS: u8 = 7;

/// True when `packet` is ours, whatever its version.
pub fn is_ours(packet: &[u8]) -> bool {
    packet.len() >= 2 && u16::from_le_bytes([packet[0], packet[1]]) == PACKET_ID
}

pub fn encode(message: &Message) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&PACKET_ID.to_le_bytes());
    out.push(PROTOCOL);
    match message {
        Message::Hello { build } => {
            out.push(HELLO);
            put_str(&mut out, build);
        }
        Message::Offer { generation, digest, size } => {
            out.push(OFFER);
            put_u32(&mut out, *generation);
            out.extend_from_slice(digest);
            put_u32(&mut out, *size);
        }
        Message::Withdraw { generation } => {
            out.push(WITHDRAW);
            put_u32(&mut out, *generation);
        }
        Message::Get { generation, object, offset, len } => {
            out.push(GET);
            put_u32(&mut out, *generation);
            put_object(&mut out, object);
            put_u32(&mut out, *offset);
            put_u32(&mut out, *len);
        }
        Message::Data { generation, object, offset, total, bytes } => {
            out.push(DATA);
            put_u32(&mut out, *generation);
            put_object(&mut out, object);
            put_u32(&mut out, *offset);
            put_u32(&mut out, *total);
            put_u32(&mut out, bytes.len() as u32);
            out.extend_from_slice(bytes);
        }
        Message::Missing { generation, object } => {
            out.push(MISSING);
            put_u32(&mut out, *generation);
            put_object(&mut out, object);
        }
        Message::Status { generation, digest, state, have, total, text } => {
            out.push(STATUS);
            put_u32(&mut out, *generation);
            out.extend_from_slice(digest);
            out.push(match state {
                State::Working => 0,
                State::Ready => 1,
                State::Failed => 2,
            });
            out.extend_from_slice(&have.to_le_bytes());
            out.extend_from_slice(&total.to_le_bytes());
            put_str(&mut out, text);
        }
    }
    out
}

pub fn decode(packet: &[u8]) -> Result<Message, String> {
    if !is_ours(packet) {
        return Err("not a WormForge packet".into());
    }
    let mut r = Reader { bytes: packet, at: 2 };
    let version = r.u8()?;
    if version != PROTOCOL {
        return Err(format!("protocol version {version}; this build speaks {PROTOCOL}"));
    }
    let message = match r.u8()? {
        HELLO => Message::Hello { build: r.str()? },
        OFFER => Message::Offer { generation: r.u32()?, digest: r.hash()?, size: r.u32()? },
        WITHDRAW => Message::Withdraw { generation: r.u32()? },
        GET => Message::Get {
            generation: r.u32()?,
            object: r.object()?,
            offset: r.u32()?,
            len: r.u32()?,
        },
        DATA => {
            let generation = r.u32()?;
            let object = r.object()?;
            let offset = r.u32()?;
            let total = r.u32()?;
            let len = r.u32()? as usize;
            if len > CHUNK {
                return Err(format!("data chunk of {len} bytes exceeds {CHUNK}"));
            }
            Message::Data { generation, object, offset, total, bytes: r.take(len)?.to_vec() }
        }
        MISSING => Message::Missing { generation: r.u32()?, object: r.object()? },
        STATUS => Message::Status {
            generation: r.u32()?,
            digest: r.hash()?,
            state: match r.u8()? {
                0 => State::Working,
                1 => State::Ready,
                2 => State::Failed,
                s => return Err(format!("unknown status {s}")),
            },
            have: r.u64()?,
            total: r.u64()?,
            text: r.str()?,
        },
        kind => return Err(format!("unknown message kind {kind}")),
    };
    if r.at != packet.len() {
        return Err(format!("{} trailing bytes", packet.len() - r.at));
    }
    Ok(message)
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Strings are short (a build id, a failure reason): u16 length, UTF-8.
fn put_str(out: &mut Vec<u8>, s: &str) {
    let bytes = &s.as_bytes()[..s.len().min(1024)];
    out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn put_object(out: &mut Vec<u8>, object: &Object) {
    match object {
        Object::Manifest => out.push(0),
        Object::Blob(sha) => {
            out.push(1);
            out.extend_from_slice(sha);
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.bytes.len());
        let end = end.ok_or_else(|| format!("packet ends early at byte {}", self.at))?;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn hash(&mut self) -> Result<[u8; 32], String> {
        Ok(self.take(32)?.try_into().unwrap())
    }

    fn str(&mut self) -> Result<String, String> {
        let len = u16::from_le_bytes(self.take(2)?.try_into().unwrap()) as usize;
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| "string is not UTF-8".into())
    }

    fn object(&mut self) -> Result<Object, String> {
        match self.u8()? {
            0 => Ok(Object::Manifest),
            1 => Ok(Object::Blob(self.hash()?)),
            t => Err(format!("unknown object tag {t}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_round_trips() {
        let sha = [7u8; 32];
        let all = [
            Message::Hello { build: "20260925-test".into() },
            Message::Offer { generation: 3, digest: sha, size: 1234 },
            Message::Withdraw { generation: 3 },
            Message::Get { generation: 3, object: Object::Manifest, offset: 0, len: 4096 },
            Message::Get { generation: 3, object: Object::Blob(sha), offset: 8192, len: 10 },
            Message::Data {
                generation: 3,
                object: Object::Blob(sha),
                offset: 4096,
                total: 9000,
                bytes: vec![1, 2, 3],
            },
            Message::Missing { generation: 3, object: Object::Blob(sha) },
            Message::Status {
                generation: 3,
                digest: sha,
                state: State::Failed,
                have: 5,
                total: 9,
                text: "disk full".into(),
            },
        ];
        for m in all {
            let wire = encode(&m);
            assert!(is_ours(&wire));
            assert_eq!(&wire[..2], b"WF");
            assert_eq!(decode(&wire).unwrap(), m);
        }
    }

    #[test]
    fn damage_is_refused_not_misread() {
        let wire = encode(&Message::Offer { generation: 1, digest: [0; 32], size: 1 });
        assert!(decode(&wire[..wire.len() - 1]).unwrap_err().contains("ends early"));
        let mut longer = wire.clone();
        longer.push(0);
        assert!(decode(&longer).unwrap_err().contains("trailing"));
        let mut newer = wire.clone();
        newer[2] = PROTOCOL + 1;
        assert!(decode(&newer).unwrap_err().contains("protocol version"));
        assert!(!is_ours(&[0x66, 0x66, 1]), "wkTerrainSync's packets are not ours");
    }
}
