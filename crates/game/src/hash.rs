//! Stable hashing for determinism checks.
//!
//! Lives here rather than in `engine` for one practical reason: `engine`
//! cannot be compiled on a developer's Mac (minhook's build script rejects
//! darwin), so tests placed beside the caller would never run. This is pure
//! arithmetic with no WA dependency, so it belongs where it can be tested.
//!
//! FNV-1a, 32-bit, deliberately not cryptographic. The job is to compare two
//! runs of the *same build* and answer "did these diverge, and at which
//! frame" — so it must be stable, cheap enough to run every frame, and
//! sensitive to both value and ordering. Nothing here defends against an
//! adversary choosing inputs.
//!
//! Rust's own `DefaultHasher` is explicitly not usable: it is randomly seeded
//! per process, which is exactly the property that would make every run look
//! divergent.

const OFFSET_BASIS: u32 = 0x811C_9DC5;
const PRIME: u32 = 0x0100_0193;

/// Running FNV-1a hash over a sequence of 32-bit words.
#[derive(Clone, Copy, Debug)]
pub struct Fnv(u32);

impl Default for Fnv {
    fn default() -> Self {
        Self::new()
    }
}

impl Fnv {
    pub fn new() -> Self {
        Fnv(OFFSET_BASIS)
    }

    /// Feed one word, little-endian, a byte at a time.
    pub fn write(&mut self, value: u32) {
        for byte in value.to_le_bytes() {
            self.0 ^= byte as u32;
            self.0 = self.0.wrapping_mul(PRIME);
        }
    }

    /// WA stores nearly everything as a signed 16.16 fixed-point word, and
    /// negative coordinates are ordinary (a worm blown off the map), so the
    /// sign has to survive into the hash rather than being clamped away.
    pub fn write_i32(&mut self, value: i32) {
        self.write(value as u32);
    }

    pub fn finish(&self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(words: &[u32]) -> u32 {
        let mut h = Fnv::new();
        for w in words {
            h.write(*w);
        }
        h.finish()
    }

    /// The whole point: same input, same answer. If this ever fails the probe
    /// reports divergence on every run and is worse than useless.
    #[test]
    fn identical_input_hashes_identically() {
        assert_eq!(hash(&[1, 2, 3]), hash(&[1, 2, 3]));
        assert_eq!(Fnv::new().finish(), OFFSET_BASIS, "empty state is the basis");
    }

    /// Two actors that swapped places are a divergence, not a match, so order
    /// has to change the hash.
    #[test]
    fn ordering_changes_the_hash() {
        assert_ne!(hash(&[1, 2]), hash(&[2, 1]));
    }

    /// A single changed word must move it — including in the low byte, which
    /// a careless implementation can swallow.
    #[test]
    fn a_one_bit_change_moves_the_hash() {
        assert_ne!(hash(&[0x1234_5678]), hash(&[0x1234_5679]));
        assert_ne!(hash(&[0x1234_5678]), hash(&[0x9234_5678]));
    }

    /// Negative fixed-point values are normal in WA. `-1` and `1` must not
    /// collapse onto each other.
    #[test]
    fn sign_survives() {
        let mut a = Fnv::new();
        a.write_i32(-1);
        let mut b = Fnv::new();
        b.write_i32(1);
        assert_ne!(a.finish(), b.finish());

        let mut c = Fnv::new();
        c.write_i32(-1);
        assert_eq!(a.finish(), c.finish());
    }

    /// Feeding N zeros must not be the same as feeding N+1 zeros — otherwise
    /// an actor list that gained an all-zero entry would hash unchanged.
    #[test]
    fn length_is_part_of_the_hash() {
        assert_ne!(hash(&[0]), hash(&[0, 0]));
        assert_ne!(hash(&[]), hash(&[0]));
    }
}
