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

/// A BitGrid header (0x2C bytes, as WA's 0x4F6370 lays it out) over `data`:
/// vtable, flags, pixels, bpp, row stride, size, clip rectangle.
fn grid(vtable: u32, data: &[u8], stride: usize, w: u32, h: u32) -> [u32; 11] {
    [vtable, 0, data.as_ptr() as u32, 8, stride as u32, w, h, 0, 0, w, h]
}

pub fn stamp(sprite: i32, frame: u32, x: i32, y: i32) -> Result<(), String> {
    let source = crate::sprite::terrain_frame(sprite, frame)?;
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
        let (stride, data) = pixels(&source, &palette)?;
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
