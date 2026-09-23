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

pub fn pack(sample: InputSample) -> (u32, u32) {
    let controls = (sample.keys & 0xff) | ((sample.down as u32) << 8);
    let x = fixed_to_wire_pixel(sample.px) as u16 as u32;
    let y = fixed_to_wire_pixel(sample.py) as u16 as u32;
    (controls, x | (y << 16))
}

pub fn unpack(controls: u32, packed_pointer: u32) -> Option<InputSample> {
    if controls & !0x1ff != 0 {
        return None;
    }
    Some(InputSample {
        keys: controls & 0xff,
        px: (packed_pointer as u16 as i16 as i32) << 16,
        py: ((packed_pointer >> 16) as u16 as i16 as i32) << 16,
        down: controls & 0x100 != 0,
    })
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
    if pointer_requested && pointer_changed && since_last >= 5 {
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
        assert!(unpack(controls | 0x200, pointer).is_none());
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
            Some(sample(0xff, i16::MAX as i32, i16::MIN as i32, false))
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
        assert!(!should_send(Some(previous), moved, 4, 1, true));
        assert!(should_send(Some(previous), moved, 6, 1, true));
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
