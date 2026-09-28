//! Add a bounded sprite frame through WA's own Landscape::plot (vtable 5).
//! Rust owns the temporary BitGrid; WA reads it synchronously and never frees it.

use game::entity::read_i32;
use game::offsets::{DISPLAY_GAME_LAYER, DISPLAY_LAYER_CONTEXTS, LANDSCAPE_TERRAIN_IMAGE, LANDSCAPE_DISPLAY};

/// Quantize without allocating or changing palette entries. Index zero is
/// reserved for transparency, including when an opaque source pixel is black.
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

pub fn stamp(sprite: i32, frame: u32, x: i32, y: i32) -> Result<(), String> {
    let source = crate::sprite::terrain_frame(sprite, frame)?;
    let expected = crate::wa::try_address("LANDSCAPE_STAMP")
        .ok_or("land.stamp: native terrain stamping unavailable")?;
    unsafe {
        let (land, method) = crate::world::landscape_method(5, expected)?;
        let image = read_i32(land, LANDSCAPE_TERRAIN_IMAGE) as *const u32;
        let display = read_i32(land, LANDSCAPE_DISPLAY) as *mut u8;
        if image.is_null() || display.is_null() || *image.add(3) != 8 {
            return Err("land.stamp: terrain image unavailable or not 8-bit".into());
        }
        // Skip wholly off-map frames before the stock dirty-rectangle path,
        // whose rectangle storage uses 16-bit coordinates.
        let left = (x >> 16) - source.width as i32 / 2;
        let top = (y >> 16) - source.height as i32 / 2;
        if left >= *image.add(9) as i32 || top >= *image.add(10) as i32
            || left + source.width as i32 <= *image.add(7) as i32
            || top + source.height as i32 <= *image.add(8) as i32 {
            return Ok(());
        }
        // Landscape loads its image against the game's layer-3 PaletteContext.
        // Read its original colours, not the animated display palette.
        let ctx = read_i32(display, DISPLAY_LAYER_CONTEXTS + DISPLAY_GAME_LAYER as usize * 4) as *const u8;
        if ctx.is_null() { return Err("land.stamp: terrain palette unavailable".into()); }
        let palette: Vec<_> = (1..=255usize).filter(|&i| *ctx.add(0x404 + i) != 0)
            .map(|i| (i as u8, [*ctx.add(4 + i * 4), *ctx.add(5 + i * 4), *ctx.add(6 + i * 4)]))
            .collect();
        let (stride, data) = pixels(&source, &palette)?;
        if data.iter().all(|p| *p == 0) { return Ok(()); }
        // Clone only the 0x2c-byte BitGrid base. The stock blitter reads pixels
        // through its base vtable; no DisplayBitGrid ownership escapes this call.
        let header = [*image, 1, data.as_ptr() as u32, 8, stride as u32,
            source.width, source.height, 0, 0, source.width, source.height];
        // A separate alpha mask prevents palette colours becoming soil values.
        let mask: Vec<u8> = data.iter().map(|p| u8::from(*p != 0)).collect();
        let mut mask_header = header;
        mask_header[2] = mask.as_ptr() as u32;
        let plot: unsafe extern "thiscall" fn(*mut u8, i32, i32, *const u32, *const u32) -> i32 =
            std::mem::transmute(method as usize);
        // The native call clips at every destination grid's bounds and updates
        // collision, terrain, soil, rollback storage and redraw rectangles.
        plot(land, x >> 16, y >> 16, header.as_ptr(), mask_header.as_ptr());
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
