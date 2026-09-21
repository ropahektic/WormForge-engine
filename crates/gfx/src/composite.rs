//! Palette-independent sprite compositing. Output is premultiplied BGRA8.

use crate::RgbaFrame;

pub fn frame_index(phase: u32, count: usize, looping: bool) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let phase = if looping {
        phase & 0xffff
    } else {
        phase.min(0xffff)
    };
    Some(((phase as u64 * count as u64) >> 16) as usize)
}

/// Draw a centered sprite, clipped to both the canvas and WA's clip rectangle.
pub fn over(
    dst: &mut [u8],
    width: usize,
    height: usize,
    clip: [usize; 4],
    frame: &RgbaFrame,
    center: [i32; 2],
    mirror_x: bool,
) {
    let sw = frame.width as usize;
    let sh = frame.height as usize;
    if dst.len() != width.saturating_mul(height).saturating_mul(4)
        || frame.rgba.len() != sw.saturating_mul(sh).saturating_mul(4)
    {
        return;
    }
    let left = center[0] as i64 - (sw / 2) as i64;
    let top = center[1] as i64 - (sh / 2) as i64;
    let x0 = left.max(clip[0] as i64).max(0).min(width as i64) as usize;
    let y0 = top.max(clip[1] as i64).max(0).min(height as i64) as usize;
    let x1 = (left + sw as i64).max(0).min(clip[2].min(width) as i64) as usize;
    let y1 = (top + sh as i64).max(0).min(clip[3].min(height) as i64) as usize;
    for y in y0..y1 {
        for x in x0..x1 {
            let sx = (x as i64 - left) as usize;
            let sx = if mirror_x { sw - 1 - sx } else { sx };
            let s = ((y as i64 - top) as usize * sw + sx) * 4;
            let d = (y * width + x) * 4;
            let a = frame.rgba[s + 3] as u32;
            for (channel, source) in [(0, 2), (1, 1), (2, 0)] {
                dst[d + channel] = ((frame.rgba[s + source] as u32 * a
                    + dst[d + channel] as u32 * (255 - a)
                    + 127)
                    / 255) as u8;
            }
            dst[d + 3] = (a + (dst[d + 3] as u32 * (255 - a) + 127) / 255) as u8;
        }
    }
}

/// Two different clears detect opaque writes even when they match one clear.
pub fn occlude(dst: &mut [u8], a: &[u8], b: &[u8]) {
    if dst.len() / 4 != a.len() || a.len() != b.len() {
        return;
    }
    for ((pixel, a), b) in dst.chunks_exact_mut(4).zip(a).zip(b) {
        if *a != 0x55 || *b != 0xaa {
            pixel.fill(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(rgba: Vec<u8>) -> RgbaFrame {
        RgbaFrame {
            width: (rgba.len() / 4) as u32,
            height: 1,
            rgba,
        }
    }
    #[test]
    fn alpha_black_and_colors_are_preserved_without_a_palette() {
        let mut dst = vec![0; 12];
        over(
            &mut dst,
            3,
            1,
            [0, 0, 3, 1],
            &frame(vec![0, 0, 0, 255, 255, 0, 0, 128, 0, 0, 255, 1]),
            [1, 0],
            false,
        );
        assert_eq!(dst, [0, 0, 0, 255, 0, 0, 128, 128, 1, 0, 0, 1]);
    }
    #[test]
    fn foreground_terrain_covers_sentry_even_when_it_matches_clear_color() {
        let mut dst = vec![0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255];
        occlude(&mut dst, &[0x55, 0xaa, 0x55], &[0x55, 0xaa, 0xaa]);
        assert_eq!(dst, [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255]);
        // A nearer translucent sprite still blends over the remaining far one.
        over(
            &mut dst,
            3,
            1,
            [0, 0, 3, 1],
            &frame(vec![0, 0, 255, 128]),
            [2, 0],
            false,
        );
        assert_eq!(&dst[8..], &[128, 0, 127, 255]);
    }
    #[test]
    fn mirror_clip_and_negative_positions_keep_source_alignment() {
        let mut dst = vec![0; 8];
        over(
            &mut dst,
            2,
            1,
            [0, 0, 1, 1],
            &frame(vec![255, 0, 0, 255, 0, 255, 0, 255]),
            [0, 0],
            true,
        );
        assert_eq!(dst, [0, 0, 255, 255, 0, 0, 0, 0]);
    }
    #[test]
    fn animation_matches_phase_selection_at_wrap_and_end() {
        assert_eq!(frame_index(0x10000, 30, true), Some(0));
        assert_eq!(frame_index(0x10000, 30, false), Some(29));
        for i in 0..30 {
            assert_eq!(
                frame_index(crate::frame_phase(i, 30).unwrap(), 30, false),
                Some(i as usize)
            );
        }
    }
}
