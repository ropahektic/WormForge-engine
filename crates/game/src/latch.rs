//! Sample a value at most once per simulation frame.
//!
//! Local input is the motivating case. `GetAsyncKeyState` and the OS cursor
//! answer whatever is true at the instant they are called, so two reads inside
//! one frame can disagree — a pack that checks `keys().space` twice can see it
//! both pressed and not pressed while the simulation has not advanced at all.
//! That is a bug on its own, and it also makes the value impossible to record:
//! there is no single "the input this frame was X" to write down.
//!
//! Latching fixes both. The first reader in a frame samples, everyone else that
//! frame gets the same answer, and a recorder has exactly one value per frame
//! to store.
//!
//! Keyed on the frame number rather than refreshed from a hook, deliberately:
//! it does not matter which hook runs first, or whether anything reads at all
//! in a given frame. A frame nobody asks about costs nothing.
//!
//! Lives in `game` rather than beside its caller because `engine` cannot be
//! compiled on darwin, so tests placed there would never run.

use std::sync::Mutex;

/// A value re-sampled only when the frame number changes.
pub struct FrameLatch<T> {
    held: Mutex<Option<(i64, T)>>,
}

impl<T: Copy> Default for FrameLatch<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy> FrameLatch<T> {
    pub const fn new() -> Self {
        Self {
            held: Mutex::new(None),
        }
    }

    /// The value for `frame`, sampling only if this frame has not been sampled.
    ///
    /// Any frame number different from the last one re-samples, including one
    /// that moves backwards: a new match restarts WA's counter at zero, and
    /// treating that as "already sampled" would serve input from the previous
    /// game.
    pub fn get(&self, frame: i64, sample: impl FnOnce() -> T) -> T {
        let Ok(mut held) = self.held.lock() else {
            // Poisoned: a stale value is worse than an extra poll, because the
            // caller would act on input from some earlier frame.
            return sample();
        };
        if let Some((at, value)) = held.as_ref() {
            if *at == frame {
                return *value;
            }
        }
        let value = sample();
        *held = Some((frame, value));
        value
    }

    /// Forget the held sample. Called between matches so the first frame of a
    /// new game cannot inherit the last frame of the old one.
    pub fn clear(&self) {
        if let Ok(mut held) = self.held.lock() {
            *held = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn counting() -> (FrameLatch<u32>, AtomicU32) {
        (FrameLatch::new(), AtomicU32::new(0))
    }

    /// The whole point: repeated reads inside one frame must not re-sample,
    /// or two calls can still disagree.
    #[test]
    fn samples_once_per_frame() {
        let (latch, calls) = counting();
        let sample = || {
            calls.fetch_add(1, Ordering::SeqCst);
            7
        };
        assert_eq!(latch.get(10, sample), 7);
        assert_eq!(latch.get(10, sample), 7);
        assert_eq!(latch.get(10, sample), 7);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "sampled more than once");
    }

    /// ...but it must still follow the input, not freeze on the first value.
    #[test]
    fn a_new_frame_resamples() {
        let (latch, calls) = counting();
        let next = || calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(latch.get(1, next), 0);
        assert_eq!(latch.get(2, next), 1);
        assert_eq!(latch.get(3, next), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    /// A new match restarts WA's frame counter at zero. Treating a lower frame
    /// as already-sampled would hand the new game the old game's input.
    #[test]
    fn a_frame_number_going_backwards_resamples() {
        let (latch, calls) = counting();
        let next = || calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(latch.get(5000, next), 0);
        assert_eq!(latch.get(0, next), 1, "counter restart must re-sample");
        assert_eq!(latch.get(0, next), 1, "and then latch as normal");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn clear_forces_the_next_read_to_sample() {
        let (latch, calls) = counting();
        let next = || calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(latch.get(4, next), 0);
        latch.clear();
        assert_eq!(latch.get(4, next), 1, "same frame, but cleared");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// Tuples are the real payload (keys plus cursor), so make sure a
    /// non-scalar round-trips unchanged.
    #[test]
    fn holds_composite_values() {
        let latch: FrameLatch<(u32, i32, i32, bool)> = FrameLatch::new();
        let v = (0b1000_0000, -1_234, 5_678, true);
        assert_eq!(latch.get(1, || v), v);
        assert_eq!(latch.get(1, || panic!("must not re-sample")), v);
    }
}
