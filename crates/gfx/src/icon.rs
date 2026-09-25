//! Weapon panel icons: a pack PNG turned into the 8bpp tiles WA's panel draws.
//!
//! The panel copies a fixed window out of each weapon's icon image (24x24 at
//! (4,4) on 3.8.1, see `docs/weapon-panel.md`) with index 0 transparent. A
//! custom icon is therefore just palette indices in that window. To look native
//! and take no palette entries, it may only use indices the stock icons already
//! use -- `quantize` maps each pixel to the nearest of those. The greyed frame
//! is derived with `learn_disabled_map`, learned from how the stock icons grey
//! out, so a custom icon greys exactly like its neighbours.
//!
//! Pure functions over byte buffers: tested here, installed by the engine.

/// A palette index and its colour.
pub type PaletteColour = (u8, [u8; 3]);

/// Alpha below this is transparent (index 0).
const ALPHA_CUTOFF: u8 = 128;

/// Quantize an RGBA image into a `win_w` x `win_h` tile of palette indices.
///
/// Every opaque pixel becomes the nearest colour in `allowed` (by squared RGB
/// distance; ties go to the lower index, so the result is deterministic).
/// Pixels below the alpha cutoff, and the margin around a smaller image,
/// become index 0, which the panel skips. An image larger than the window is
/// refused, naming the size it must fit.
pub fn quantize(
    rgba: &[u8],
    w: u32,
    h: u32,
    allowed: &[PaletteColour],
    win_w: u32,
    win_h: u32,
) -> Result<Vec<u8>, String> {
    if rgba.len() != (w * h * 4) as usize {
        return Err(format!("icon: {w}x{h} image has {} bytes", rgba.len()));
    }
    if w > win_w || h > win_h {
        return Err(format!(
            "icon is {w}x{h}; the weapon panel shows {win_w}x{win_h} pixels, so it must fit that"
        ));
    }
    if allowed.is_empty() {
        return Err("icon: no stock icon colours to quantize to".into());
    }
    let mut out = vec![0u8; (win_w * win_h) as usize];
    let (ox, oy) = ((win_w - w) / 2, (win_h - h) / 2);
    for y in 0..h {
        for x in 0..w {
            let p = ((y * w + x) * 4) as usize;
            if rgba[p + 3] < ALPHA_CUTOFF {
                continue;
            }
            let rgb = [rgba[p], rgba[p + 1], rgba[p + 2]];
            out[((y + oy) * win_w + x + ox) as usize] = nearest(allowed, rgb);
        }
    }
    Ok(out)
}

fn nearest(allowed: &[PaletteColour], rgb: [u8; 3]) -> u8 {
    let mut best = (u32::MAX, 0u8);
    for &(index, c) in allowed {
        let d = distance(c, rgb);
        if d < best.0 || (d == best.0 && index < best.1) {
            best = (d, index);
        }
    }
    best.1
}

fn distance(a: [u8; 3], b: [u8; 3]) -> u32 {
    a.iter()
        .zip(b)
        .map(|(&x, y)| {
            let d = x as i32 - y as i32;
            (d * d) as u32
        })
        .sum()
}

/// Learn enabled -> disabled from stock icon pairs of equal size.
///
/// For each palette index, the disabled index it most often becomes at the
/// same pixel across all pairs (ties to the lower index). Index 0 stays 0.
/// An index never seen in an enabled frame takes the mapping of the seen
/// index nearest to it in colour (`rgb`), so every colour `quantize` can
/// produce has a greyed counterpart.
pub fn learn_disabled_map(pairs: &[(&[u8], &[u8])], rgb: &[[u8; 3]; 256]) -> [u8; 256] {
    let mut counts = vec![[0u32; 256]; 256];
    for (enabled, disabled) in pairs {
        for (&e, &d) in enabled.iter().zip(disabled.iter()) {
            if e != 0 {
                counts[e as usize][d as usize] += 1;
            }
        }
    }
    let mut map = [0u8; 256];
    let mut learned = [false; 256];
    for i in 1..256 {
        let row = &counts[i];
        let (mut best, mut n) = (0usize, 0u32);
        for (d, &c) in row.iter().enumerate() {
            if c > n {
                best = d;
                n = c;
            }
        }
        if n > 0 {
            map[i] = best as u8;
            learned[i] = true;
        }
    }
    let seen: Vec<PaletteColour> = (1..256)
        .filter(|&i| learned[i])
        .map(|i| (i as u8, rgb[i]))
        .collect();
    if !seen.is_empty() {
        for i in 1..256 {
            if !learned[i] {
                map[i] = map[nearest(&seen, rgb[i]) as usize];
            }
        }
    }
    map
}

/// The `cw` x `ch` rectangle at (`x`, `y`) of an RGBA image. For a full-size
/// icon image (a PX icon is WA's whole 32x32 image), this is the window the
/// panel actually shows.
pub fn crop(rgba: &[u8], w: u32, x: u32, y: u32, cw: u32, ch: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((cw * ch * 4) as usize);
    for row in y..y + ch {
        let at = ((row * w + x) * 4) as usize;
        out.extend_from_slice(&rgba[at..at + (cw * 4) as usize]);
    }
    out
}

/// Apply a learned map to a tile.
pub fn disable(tile: &[u8], map: &[u8; 256]) -> Vec<u8> {
    tile.iter().map(|&i| map[i as usize]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [u8; 3] = [200, 30, 30];
    const GREEN: [u8; 3] = [30, 200, 30];
    const GREY: [u8; 3] = [90, 90, 90];

    fn px(c: [u8; 3], a: u8) -> [u8; 4] {
        [c[0], c[1], c[2], a]
    }

    fn image(pixels: &[[u8; 4]]) -> Vec<u8> {
        pixels.iter().flatten().copied().collect()
    }

    #[test]
    fn exact_colours_round_trip() {
        let allowed = [(7, RED), (9, GREEN)];
        let img = image(&[px(RED, 255), px(GREEN, 255)]);
        assert_eq!(quantize(&img, 2, 1, &allowed, 2, 1).unwrap(), vec![7, 9]);
    }

    #[test]
    fn nearest_colour_and_transparency() {
        let allowed = [(7, RED), (9, GREEN)];
        let img = image(&[px([180, 60, 40], 255), px(GREEN, 10)]);
        assert_eq!(quantize(&img, 2, 1, &allowed, 2, 1).unwrap(), vec![7, 0]);
    }

    #[test]
    fn a_smaller_icon_is_centred() {
        let allowed = [(5, RED)];
        let img = image(&[px(RED, 255)]);
        let tile = quantize(&img, 1, 1, &allowed, 3, 3).unwrap();
        assert_eq!(tile, vec![0, 0, 0, 0, 5, 0, 0, 0, 0]);
    }

    #[test]
    fn a_larger_icon_is_refused_naming_the_size() {
        let allowed = [(5, RED)];
        let img = vec![255; 25 * 24 * 4];
        let err = quantize(&img, 25, 24, &allowed, 24, 24).unwrap_err();
        assert!(err.contains("24x24"), "{err}");
    }

    #[test]
    fn crop_takes_the_window() {
        // 3x3 image, pixel value = its index; take the centre 1x1.
        let img: Vec<u8> = (0..9u8).flat_map(|i| [i, i, i, 255]).collect();
        assert_eq!(crop(&img, 3, 1, 1, 1, 1), vec![4, 4, 4, 255]);
    }

    #[test]
    fn disabled_map_learns_the_majority_and_fills_gaps_by_colour() {
        let mut rgb = [[0u8; 3]; 256];
        rgb[1] = RED;
        rgb[2] = GREEN;
        rgb[3] = [190, 40, 40]; // never seen enabled; nearest seen is 1
        rgb[10] = GREY;
        rgb[11] = [60, 60, 60];
        // 1 becomes 10 twice and 11 once; 2 becomes 11.
        let enabled: &[u8] = &[1, 1, 1, 2, 0];
        let disabled: &[u8] = &[10, 10, 11, 11, 0];
        let map = learn_disabled_map(&[(enabled, disabled)], &rgb);
        assert_eq!(map[0], 0);
        assert_eq!(map[1], 10);
        assert_eq!(map[2], 11);
        assert_eq!(map[3], 10, "unseen index borrows its nearest colour's mapping");
        assert_eq!(disable(&[1, 2, 0], &map), vec![10, 11, 0]);
    }
}
