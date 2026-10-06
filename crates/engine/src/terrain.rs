//! `wa.land.stamp`: draw a pack sheet's frame into the land through WA's own
//! Landscape::plot (vtable slot 5, 0x57CE30), the call that places a girder.
//!
//! plot(x, y, image, soil) centres both grids on (x, y), records the rectangle
//! for rollback, blits `image` into the collision grid (+0x914) and the terrain
//! picture (+0x910), `soil` into the soil layer (+0x91C), and queues a redraw.
//! Every 8-bit blit skips index 0, so a frame's transparent pixels leave the
//! land as it was. For a girder WA passes, as `soil`, the same shape loaded
//! from a second archive; the stamp passes its own picture there too.
//!
//! Both grids are BitGrid headers over Rust buffers: WA reads them during the
//! call and never keeps or frees them.
//!
//! Colours: every machine must write the same pixels, since the landscape
//! checksum covers the picture or soil layer on older logic versions. The
//! palette's in-use flags differ between machines (a CPU-installed sprite
//! allocates entries, a GPU sheet does not), so the stamp only uses the
//! indices the terrain picture already holds, which the game keeps identical.

use game::entity::read_i32;
use game::offsets::{DISPLAY_GAME_LAYER, DISPLAY_LAYER_CONTEXTS, LANDSCAPE_TERRAIN_IMAGE, LANDSCAPE_DISPLAY};

/// Quantize to `palette` without allocating or changing palette entries.
/// Index 0 stays transparent, including for opaque black source pixels.
fn pixels(frame: &gfx::RgbaFrame, palette: &[(u8, [u8; 3])]) -> Result<(usize, Vec<u8>), String> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    if !(1..=256).contains(&w) || !(1..=256).contains(&h) || frame.rgba.len() != w * h * 4 {
        return Err("land.stamp: expected a complete 1..256 pixel RGBA frame".into());
    }
    if palette.is_empty() || palette.iter().any(|(i, _)| *i == 0) {
        return Err("land.stamp: no usable terrain palette".into());
    }
    let stride = (w + 3) & !3;
    let mut out = vec![0; stride * h];
    for y in 0..h {
        for x in 0..w {
            let p = &frame.rgba[(y * w + x) * 4..][..4];
            if p[3] < 128 { continue; }
            let &(index, _) = palette.iter().min_by_key(|(_, rgb)| {
                (0..3).map(|c| (p[c] as i32 - rgb[c] as i32).pow(2)).sum::<i32>()
            }).unwrap();
            out[y * stride + x] = index;
        }
    }
    Ok((stride, out))
}

/// The nonzero indices the terrain picture holds, in index order.
fn terrain_indices(rows: &[u8], stride: usize, width: usize, height: usize) -> Vec<u8> {
    let mut seen = [false; 256];
    for y in 0..height {
        for &p in &rows[y * stride..][..width] {
            seen[p as usize] = true;
        }
    }
    (1..=255u8).filter(|&i| seen[i as usize]).collect()
}

/// Each pixel's distance in whole pixels to the frame's outline: 0 where the
/// frame is transparent (alpha below 128, as `pixels` skips), 1 on the
/// outline, growing inwards. Outside the frame counts as transparent. A 3-4
/// chamfer pass pair, so the distance is close to the straight-line one in
/// every direction. Capped at 255.
pub fn edge_depths(frame: &gfx::RgbaFrame) -> Vec<u8> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    if frame.rgba.len() != w * h * 4 {
        return Vec::new();
    }
    const FAR: u32 = u32::MAX / 2;
    let mut d: Vec<u32> = (0..w * h).map(|i| if frame.rgba[i * 4 + 3] >= 128 { FAR } else { 0 }).collect();
    let at = |d: &[u32], x: isize, y: isize| -> u32 {
        if x < 0 || y < 0 || x >= w as isize || y >= h as isize { 0 } else { d[y as usize * w + x as usize] }
    };
    for y in 0..h as isize {
        for x in 0..w as isize {
            let i = y as usize * w + x as usize;
            if d[i] == 0 { continue; }
            let best = [(at(&d, x - 1, y), 3), (at(&d, x - 1, y - 1), 4), (at(&d, x, y - 1), 3), (at(&d, x + 1, y - 1), 4)]
                .iter().map(|(v, c)| v + c).min().unwrap();
            d[i] = d[i].min(best);
        }
    }
    for y in (0..h as isize).rev() {
        for x in (0..w as isize).rev() {
            let i = y as usize * w + x as usize;
            if d[i] == 0 { continue; }
            let best = [(at(&d, x + 1, y), 3), (at(&d, x + 1, y + 1), 4), (at(&d, x, y + 1), 3), (at(&d, x - 1, y + 1), 4)]
                .iter().map(|(v, c)| v + c).min().unwrap();
            d[i] = d[i].min(best);
        }
    }
    d.into_iter().map(|v| ((v + 2) / 3).min(255) as u8).collect()
}

/// A 4x4 ordered-dither threshold, 0..15, anchored to the map so neighbouring
/// stamps share one pattern.
const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Redraw the quantized stamp's pixels that fall on existing land, as
/// `buried` asks. `land(mx, my)` is the land pixel's colour on the map there,
/// None where there is none (or off the map). Each such pixel keeps
/// `min(depth, soften) / soften` of its strength: dithered, it is kept or
/// dropped (left as the land) against `BAYER`; mixed, its colour is blended
/// with the land's and quantized again. Pixels over air are untouched.
#[allow(clippy::too_many_arguments)]
fn bury(
    data: &mut [u8],
    stride: usize,
    frame: &gfx::RgbaFrame,
    depth: &[u8],
    left: i32,
    top: i32,
    buried: lua_host::StampBuried,
    palette: &[(u8, [u8; 3])],
    land: impl Fn(i32, i32) -> Option<[u8; 3]>,
) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let soften = buried.soften.max(1) as u32;
    for y in 0..h {
        for x in 0..w {
            let at = y * stride + x;
            if data[at] == 0 { continue; }
            let (mx, my) = (left + x as i32, top + y as i32);
            let Some(ground) = land(mx, my) else { continue };
            let kept = (depth.get(y * w + x).copied().unwrap_or(0) as u32).min(soften);
            if kept >= soften { continue; }
            if buried.mix {
                let p = &frame.rgba[(y * w + x) * 4..][..3];
                let rgb: [i32; 3] = std::array::from_fn(|c| {
                    ((p[c] as u32 * kept + ground[c] as u32 * (soften - kept)) / soften) as i32
                });
                let &(index, _) = palette.iter().min_by_key(|(_, q)| {
                    (0..3).map(|c| (rgb[c] - q[c] as i32).pow(2)).sum::<i32>()
                }).unwrap();
                data[at] = index;
            } else {
                // Kept while its strength beats the threshold's midpoint:
                // kept/soften > (t + 0.5)/16.
                let t = BAYER[(my & 3) as usize][(mx & 3) as usize] as u32;
                if 32 * kept <= (2 * t + 1) * soften {
                    data[at] = 0;
                }
            }
        }
    }
}

/// A BitGrid header (0x2C bytes, as WA's 0x4F6370 lays it out) over `data`:
/// vtable, flags, pixels, bpp, row stride, size, clip rectangle.
fn grid(vtable: u32, data: &[u8], stride: usize, w: u32, h: u32) -> [u32; 11] {
    [vtable, 0, data.as_ptr() as u32, 8, stride as u32, w, h, 0, 0, w, h]
}

pub fn stamp(
    sprite: i32,
    frame: u32,
    x: i32,
    y: i32,
    buried: Option<lua_host::StampBuried>,
) -> Result<(), String> {
    let (source, depth) = crate::sprite::terrain_frame(sprite, frame)?;
    let expected = crate::wa::try_address("LANDSCAPE_STAMP")
        .ok_or("land.stamp: native terrain stamping unavailable")?;
    unsafe {
        let (land, method) = crate::world::landscape_method(5, expected)?;
        let image = read_i32(land, LANDSCAPE_TERRAIN_IMAGE) as *const u32;
        let display = read_i32(land, LANDSCAPE_DISPLAY) as *mut u8;
        if image.is_null() || display.is_null() || *image.add(3) != 8 || (*image.add(2)) == 0 {
            return Err("land.stamp: terrain image unavailable or not 8-bit".into());
        }
        let (map_w, map_h, map_stride) = (*image.add(5) as usize, *image.add(6) as usize, *image.add(4) as usize);
        // Skip wholly off-map frames before the stock dirty-rectangle path,
        // whose rectangle storage uses 16-bit coordinates.
        let left = (x >> 16) - source.width as i32 / 2;
        let top = (y >> 16) - source.height as i32 / 2;
        if left >= *image.add(9) as i32 || top >= *image.add(10) as i32
            || left + source.width as i32 <= *image.add(7) as i32
            || top + source.height as i32 <= *image.add(8) as i32 {
            return Ok(());
        }
        // The terrain picture's colours, looked up in the game layer's
        // PaletteContext (+4: RGBA per index), which the landscape loads with.
        let ctx = read_i32(display, DISPLAY_LAYER_CONTEXTS + DISPLAY_GAME_LAYER as usize * 4) as *const u8;
        if ctx.is_null() { return Err("land.stamp: terrain palette unavailable".into()); }
        let rows = std::slice::from_raw_parts(*image.add(2) as *const u8, map_stride * map_h);
        let palette: Vec<_> = terrain_indices(rows, map_stride, map_w, map_h).into_iter()
            .map(|i| (i, [*ctx.add(4 + i as usize * 4), *ctx.add(5 + i as usize * 4), *ctx.add(6 + i as usize * 4)]))
            .collect();
        let (stride, mut data) = pixels(&source, &palette)?;
        if let Some(buried) = buried {
            let colour = |i: u8| [*ctx.add(4 + i as usize * 4), *ctx.add(5 + i as usize * 4), *ctx.add(6 + i as usize * 4)];
            bury(&mut data, stride, &source, &depth, left, top, buried, &palette, |mx, my| {
                if mx < 0 || my < 0 || mx as usize >= map_w || my as usize >= map_h {
                    return None;
                }
                let index = rows[my as usize * map_stride + mx as usize];
                (index != 0).then(|| colour(index))
            });
        }
        if data.iter().all(|p| *p == 0) { return Ok(()); }
        let picture = grid(*image, &data, stride, source.width, source.height);
        // A girder's soil art is the same shape from another archive; the
        // javelin has one picture, so it goes into both layers.
        let soil = picture;
        let plot: unsafe extern "thiscall" fn(*mut u8, i32, i32, *const u32, *const u32) -> i32 =
            std::mem::transmute(method as usize);
        plot(land, x >> 16, y >> 16, picture.as_ptr(), soil.as_ptr());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_black_and_padding_do_not_create_a_solid_rectangle() {
        let frame = gfx::RgbaFrame { width: 3, height: 2, rgba: vec![
            255, 0, 0, 0, 0, 0, 0, 255, 255, 0, 0, 127,
            255, 0, 0, 128, 0, 0, 0, 0, 0, 0, 0, 255,
        ] };
        let palette = [(1, [0, 0, 0]), (2, [255, 0, 0])];
        assert_eq!(pixels(&frame, &palette).unwrap(), (4, vec![0, 1, 0, 0, 2, 0, 1, 0]));
    }

    #[test]
    fn only_indices_the_terrain_holds_are_candidates() {
        // Two rows of 3 pixels, stride 4: the padding byte (9) is not terrain.
        let rows = [0, 5, 7, 9, 5, 0, 200, 9];
        assert_eq!(terrain_indices(&rows, 4, 3, 2), vec![5, 7, 200]);
    }

    #[test]
    fn a_grid_header_matches_was_bitgrid_layout() {
        let data = [0u8; 8];
        let g = grid(0x664144, &data, 4, 3, 2);
        assert_eq!((g[0], g[1], g[3], g[4], g[5], g[6]), (0x664144, 0, 8, 4, 3, 2));
        assert_eq!(&g[7..], &[0, 0, 3, 2]);
    }

    fn opaque(w: u32, h: u32, rgb: [u8; 3]) -> gfx::RgbaFrame {
        gfx::RgbaFrame { width: w, height: h, rgba: (0..w * h).flat_map(|_| [rgb[0], rgb[1], rgb[2], 255]).collect() }
    }

    #[test]
    fn depth_grows_from_the_outline_inwards() {
        // A 5x5 square: the outline is 1, the ring inside it 2, the middle 3.
        let d = edge_depths(&opaque(5, 5, [9; 3]));
        assert_eq!(&d[10..15], &[1, 2, 3, 2, 1]);
        assert_eq!(&d[0..5], &[1; 5]);
        // A transparent pixel is 0, and its neighbours are outline.
        let mut f = opaque(3, 3, [9; 3]);
        f.rgba[4 * 4 + 3] = 0;
        let d = edge_depths(&f);
        assert_eq!(d[4], 0);
        assert!(d.iter().enumerate().all(|(i, &v)| i == 4 || v == 1));
    }

    #[test]
    fn buried_dither_keeps_the_thick_middle_and_thins_the_edge() {
        // A 5x5 square wholly over land, soften 2: the outline (depth 1)
        // keeps the pixels whose threshold is under the middle, depth 2 and
        // more keeps everything.
        let f = opaque(5, 5, [255, 0, 0]);
        let depth = edge_depths(&f);
        let palette = [(1, [255, 0, 0])];
        let (stride, mut data) = pixels(&f, &palette).unwrap();
        let buried = lua_host::StampBuried { soften: 2, mix: false };
        bury(&mut data, stride, &f, &depth, 0, 0, buried, &palette, |_, _| Some([0, 0, 255]));
        for y in 0..5 {
            for x in 0..5 {
                let kept = data[y * stride + x] != 0;
                if depth[y * 5 + x] >= 2 {
                    assert!(kept, "inner pixel ({x},{y}) dropped");
                } else {
                    assert_eq!(kept, BAYER[y & 3][x & 3] <= 7, "outline pixel ({x},{y})");
                }
            }
        }
    }

    #[test]
    fn buried_leaves_pixels_over_air_alone() {
        let f = opaque(4, 1, [255, 0, 0]);
        let depth = edge_depths(&f);
        let palette = [(1, [255, 0, 0])];
        let (stride, mut data) = pixels(&f, &palette).unwrap();
        let buried = lua_host::StampBuried { soften: 8, mix: false };
        // Land only right of x = 1 on the map; the stamp starts at x = 10.
        bury(&mut data, stride, &f, &depth, 10, 0, buried, &palette, |mx, _| (mx >= 12).then_some([0; 3]));
        assert_eq!(&data[..2], &[1, 1], "over air: as drawn");
    }

    #[test]
    fn buried_mix_blends_towards_the_land_and_stays_in_the_palette() {
        // One outline pixel, half strength: red over blue becomes purple.
        let f = opaque(1, 1, [255, 0, 0]);
        let depth = edge_depths(&f);
        let palette = [(1, [255, 0, 0]), (2, [0, 0, 255]), (3, [128, 0, 128])];
        let (stride, mut data) = pixels(&f, &palette).unwrap();
        let buried = lua_host::StampBuried { soften: 2, mix: true };
        bury(&mut data, stride, &f, &depth, 0, 0, buried, &palette, |_, _| Some([0, 0, 255]));
        assert_eq!(data[0], 3);
    }

    #[test]
    fn malformed_frames_and_transparent_palette_fail_before_any_write() {
        let mut f = gfx::RgbaFrame { width: 1, height: 1, rgba: vec![0; 4] };
        assert!(pixels(&f, &[]).is_err());
        assert!(pixels(&f, &[(0, [0; 3])]).is_err());
        f.width = 257;
        assert!(pixels(&f, &[(1, [0; 3])]).is_err());
        f.width = 1; f.rgba.pop();
        assert!(pixels(&f, &[(1, [0; 3])]).is_err());
    }
}
