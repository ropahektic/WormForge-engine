//! Compact, order-independent input records for WormForge's lockstep carrier.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputSample {
    pub keys: u32,
    pub px: i32,
    pub py: i32,
    pub down: bool,
}

fn fixed_to_wire_pixel(value: i32) -> i16 {
    (value >> 16).clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// `keys` bits 8..11: the digit key held (0 none, 1..10 for 0..9), which the
/// wire carries in bits 9..12 of the controls word. Bit 12 is Ctrl, carried
/// in wire bit 13.
pub const DIGIT_SHIFT: u32 = 8;
pub const CTRL: u32 = 1 << 12;
const WIRE_DIGIT_SHIFT: u32 = 9;
const WIRE_DIGIT_MASK: u32 = 0xf << WIRE_DIGIT_SHIFT;
const WIRE_CTRL: u32 = 1 << 13;

pub fn pack(sample: InputSample) -> (u32, u32) {
    let digit = (sample.keys >> DIGIT_SHIFT) & 0xf;
    let controls = (sample.keys & 0xff)
        | ((sample.down as u32) << 8)
        | (digit << WIRE_DIGIT_SHIFT)
        | (if sample.keys & CTRL != 0 { WIRE_CTRL } else { 0 });
    let x = fixed_to_wire_pixel(sample.px) as u16 as u32;
    let y = fixed_to_wire_pixel(sample.py) as u16 as u32;
    (controls, x | (y << 16))
}

pub fn unpack(controls: u32, packed_pointer: u32) -> Option<InputSample> {
    if controls & !(0x1ff | WIRE_DIGIT_MASK | WIRE_CTRL) != 0 {
        return None;
    }
    let digit = (controls & WIRE_DIGIT_MASK) >> WIRE_DIGIT_SHIFT;
    Some(InputSample {
        keys: (controls & 0xff)
            | (digit << DIGIT_SHIFT)
            | (if controls & WIRE_CTRL != 0 { CTRL } else { 0 }),
        px: (packed_pointer as u16 as i16 as i32) << 16,
        py: ((packed_pointer >> 16) as u16 as i16 as i32) << 16,
        down: controls & 0x100 != 0,
    })
}

/// Bits 16..23 of the controls word: the sending machine's index plus one
/// (0: a record from a build that did not say).
const SENDER_SHIFT: u32 = 16;
const SENDER_MASK: u32 = 0xff << SENDER_SHIFT;

/// `pack`, stamped with the sending machine's index (WA's
/// `GameInfo.local_team_index`: 0 the host, 1.. the clients), so every peer
/// can tell whose input a record is.
pub fn pack_from(sample: InputSample, sender: u8) -> (u32, u32) {
    let (controls, pointer) = pack(sample);
    (controls | ((sender as u32 + 1) & 0xff) << SENDER_SHIFT, pointer)
}

/// `unpack` for a stamped record: the sample, and the sender if it said.
pub fn unpack_from(controls: u32, packed_pointer: u32) -> Option<(InputSample, Option<u8>)> {
    if controls & !(0x1ff | WIRE_DIGIT_MASK | WIRE_CTRL | SENDER_MASK) != 0 {
        return None;
    }
    let sender = (controls & SENDER_MASK) >> SENDER_SHIFT;
    let sample = unpack(controls & !SENDER_MASK, packed_pointer)?;
    Some((sample, (sender != 0).then(|| (sender - 1) as u8)))
}

/// Whether the authoritative peer needs to publish a new state record.
///
/// Controls are edge-sensitive and go out immediately. Pointer motion is
/// dense, so it is sent while dragging, at a modest rate while a pack is
/// reading it, and otherwise only with the one-second heartbeat.
pub fn should_send(
    previous: Option<InputSample>,
    current: InputSample,
    frame: u32,
    last_sent_frame: u32,
    pointer_requested: bool,
) -> bool {
    let Some(previous) = previous else {
        return true;
    };
    if previous.keys != current.keys || previous.down != current.down {
        return true;
    }
    let pointer_changed = previous.px != current.px || previous.py != current.py;
    let since_last = frame.wrapping_sub(last_sent_frame);
    if current.down && pointer_changed {
        return true;
    }
    // While a pack is reading the pointer, publish motion quickly so cursor
    // actors (worm toss, etc.) track the mouse under lockstep.
    if pointer_requested && pointer_changed && since_last >= 1 {
        return true;
    }
    since_last >= 50
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(keys: u32, x: i32, y: i32, down: bool) -> InputSample {
        InputSample {
            keys,
            px: x << 16,
            py: y << 16,
            down,
        }
    }

    #[test]
    fn wire_round_trip_keeps_controls_and_signed_pixels() {
        let original = sample(0xa5, -123, 4567, true);
        let (controls, pointer) = pack(original);
        assert_eq!(unpack(controls, pointer), Some(original));
        assert!(unpack(controls | 0x4000, pointer).is_none());
    }

    #[test]
    fn the_digit_held_travels_with_the_keys() {
        // Shift and the 7 key (digit 7, stored as 8).
        let seven = sample(0x20 | (8 << DIGIT_SHIFT), 1, 2, false);
        let (controls, pointer) = pack(seven);
        assert_eq!(unpack(controls, pointer), Some(seven));
        let (controls, pointer) = pack_from(seven, 1);
        assert_eq!(unpack_from(controls, pointer), Some((seven, Some(1))));
        // A digit change is a key change: sent at once.
        assert!(should_send(Some(sample(0x20, 1, 2, false)), seven, 2, 1, false));
    }

    #[test]
    fn ctrl_is_sent_for_the_firing_frame() {
        let ctrl = sample(CTRL | 0x80, 1, 2, false);
        let (controls, pointer) = pack_from(ctrl, 1);
        assert_eq!(unpack_from(controls, pointer), Some((ctrl, Some(1))));
        assert!(should_send(Some(sample(0x80, 1, 2, false)), ctrl, 2, 1, false));
    }

    #[test]
    fn records_say_which_machine_sent_them() {
        let shift = sample(0x20, 10, 20, false);
        for sender in [0u8, 1, 5] {
            let (controls, pointer) = pack_from(shift, sender);
            assert_eq!(unpack_from(controls, pointer), Some((shift, Some(sender))));
        }
        // An unstamped record: no sender.
        let (controls, pointer) = pack(shift);
        assert_eq!(unpack_from(controls, pointer), Some((shift, None)));
        // Anything else set is still malformed.
        assert!(unpack_from(controls | 0x4000, pointer).is_none());
        assert!(unpack_from(controls | 0x0100_0000, pointer).is_none());
    }

    #[test]
    fn wire_clamps_coordinates_and_masks_unknown_keys() {
        let original = InputSample {
            keys: 0xffff,
            px: i32::MAX,
            py: i32::MIN,
            down: false,
        };
        let (controls, pointer) = pack(original);
        assert_eq!(
            unpack(controls, pointer),
            Some(sample(0x1fff, i16::MAX as i32, i16::MIN as i32, false))
        );
    }

    #[test]
    fn control_edges_send_immediately() {
        let idle = sample(0, 300, 400, false);
        assert!(should_send(
            Some(idle),
            sample(64, 300, 400, false),
            2,
            1,
            false
        ));
        assert!(should_send(
            Some(idle),
            sample(0, 300, 400, true),
            2,
            1,
            false
        ));
    }

    #[test]
    fn pointer_is_throttled_until_used_or_dragged() {
        let previous = sample(0, 300, 400, false);
        let moved = sample(0, 301, 400, false);
        assert!(!should_send(Some(previous), moved, 2, 1, false));
        assert!(should_send(Some(previous), moved, 2, 1, true));
        assert!(should_send(
            Some(sample(0, 300, 400, true)),
            sample(0, 301, 400, true),
            2,
            1,
            false
        ));
    }

    #[test]
    fn unchanged_state_gets_a_heartbeat() {
        let idle = sample(0, 300, 400, false);
        assert!(!should_send(Some(idle), idle, 49, 0, false));
        assert!(should_send(Some(idle), idle, 50, 0, false));
    }
}
