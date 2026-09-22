//! Atlas packing. Every opted-in sheet goes into one texture so a whole frame
//! is a single `DrawPrimitive` with no texture changes between sprites.
//!
//! A shelf packer is enough here and is worth preferring over anything
//! cleverer: sprite sheets are frames of equal size, so they pack into rows
//! near-perfectly, and a predictable layout is far easier to debug from a
//! screenshot than an optimal one.

/// Identifies a sheet within the atlas. Distinct from WA's sprite ids, which
/// these deliberately never become — escaping the 223-slot table is the point.
pub type SheetId = u32;

/// Where one frame lives in the atlas, in texels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SheetRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub atlas_w: u32,
    pub atlas_h: u32,
}

impl SheetRect {
    /// Normalised texture coordinates, half-texel inset.
    ///
    /// The inset matters: without it a point-sampled edge texel can bleed in
    /// from the neighbouring frame, which shows up as a one-pixel seam along
    /// a rotated sprite and is miserable to diagnose from a screenshot.
    pub fn uv(&self) -> (f32, f32, f32, f32) {
        let (aw, ah) = (self.atlas_w as f32, self.atlas_h as f32);
        (
            (self.x as f32 + 0.5) / aw,
            (self.y as f32 + 0.5) / ah,
            (self.x as f32 + self.w as f32 - 0.5) / aw,
            (self.y as f32 + self.h as f32 - 0.5) / ah,
        )
    }
}

/// One sheet's frames, packed.
#[derive(Clone, Debug)]
pub struct Packed {
    pub id: SheetId,
    pub frames: Vec<SheetRect>,
}

/// A packed atlas plus the BGRA image to upload.
#[derive(Clone, Debug)]
pub struct Atlas {
    pub width: u32,
    pub height: u32,
    /// Premultiplied BGRA, `width * height * 4` bytes.
    pub pixels: Vec<u8>,
    pub sheets: Vec<Packed>,
}

/// One sheet offered to the packer: equal-sized RGBA frames.
pub struct Source<'a> {
    pub id: SheetId,
    pub width: u32,
    pub height: u32,
    /// Each frame is `width * height * 4` bytes of straight (not
    /// premultiplied) RGBA, exactly as `gfx::RgbaFrame` stores it.
    pub frames: &'a [Vec<u8>],
}

const PAD: u32 = 1;

impl Atlas {
    pub fn rect(&self, id: SheetId, frame: usize) -> Option<SheetRect> {
        let sheet = self.sheets.iter().find(|s| s.id == id)?;
        sheet.frames.get(frame).copied()
    }

    /// Shelf-pack every frame into an atlas at most `max_side` on a side.
    ///
    /// Returns `None` only when the frames genuinely cannot fit, which the
    /// caller must treat as "fall back to the palette path" rather than as a
    /// reason to drop sprites. This machine reports a 16384 limit, so in
    /// practice the cap is memory, not the device.
    pub fn pack(sources: &[Source], max_side: u32) -> Option<Atlas> {
        let mut entries: Vec<(SheetId, usize, u32, u32)> = Vec::new();
        for source in sources {
            if source.width == 0 || source.height == 0 {
                continue;
            }
            for index in 0..source.frames.len() {
                entries.push((source.id, index, source.width, source.height));
            }
        }
        if entries.is_empty() {
            return None;
        }

        // Tallest first: shelves then waste far less vertical space than they
        // do in source order, where one tall frame can set a whole shelf's
        // height for a row of short ones.
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by(|&a, &b| {
            entries[b].3
                .cmp(&entries[a].3)
                .then(entries[b].2.cmp(&entries[a].2))
                .then(a.cmp(&b))
        });

        // Start at the widest frame (rounded up to a power of two) and double
        // until everything fits.
        let widest = entries.iter().map(|e| e.2 + PAD * 2).max()?;
        let mut side = 64u32.max(widest.next_power_of_two());
        loop {
            if side > max_side {
                return None;
            }
            if let Some(placed) = shelf_pack(&entries, &order, side) {
                return Some(assemble(sources, entries, placed, side));
            }
            side *= 2;
        }
    }
}

/// Place every entry, or report that this side length is too small.
fn shelf_pack(
    entries: &[(SheetId, usize, u32, u32)],
    order: &[usize],
    side: u32,
) -> Option<Vec<(u32, u32)>> {
    let mut placed = vec![(0u32, 0u32); entries.len()];
    let (mut x, mut y, mut shelf_h) = (PAD, PAD, 0u32);
    for &i in order {
        let (_, _, w, h) = entries[i];
        let (fw, fh) = (w + PAD, h + PAD);
        if x + fw > side {
            // Next shelf.
            x = PAD;
            y += shelf_h;
            shelf_h = 0;
        }
        if y + fh > side {
            return None;
        }
        placed[i] = (x, y);
        x += fw;
        shelf_h = shelf_h.max(fh);
    }
    Some(placed)
}

/// Blit the sources into the packed layout, premultiplying as we go.
fn assemble(
    sources: &[Source],
    entries: Vec<(SheetId, usize, u32, u32)>,
    placed: Vec<(u32, u32)>,
    side: u32,
) -> Atlas {
    let mut pixels = vec![0u8; (side as usize) * (side as usize) * 4];
    let mut sheets: Vec<Packed> = Vec::new();

    for (entry, &(px, py)) in entries.iter().zip(&placed) {
        let (id, frame_index, w, h) = *entry;
        let Some(source) = sources.iter().find(|s| s.id == id) else {
            continue;
        };
        let Some(frame) = source.frames.get(frame_index) else {
            continue;
        };
        for row in 0..h as usize {
            for col in 0..w as usize {
                let s = (row * w as usize + col) * 4;
                if s + 3 >= frame.len() {
                    continue;
                }
                let (r, g, b, a) = (
                    frame[s] as u32,
                    frame[s + 1] as u32,
                    frame[s + 2] as u32,
                    frame[s + 3] as u32,
                );
                let d = ((py as usize + row) * side as usize + px as usize + col) * 4;
                // D3DFMT_A8R8G8B8 on little-endian is B,G,R,A in memory.
                // Premultiplied so opaque and additive share one blend state.
                pixels[d] = ((b * a + 127) / 255) as u8;
                pixels[d + 1] = ((g * a + 127) / 255) as u8;
                pixels[d + 2] = ((r * a + 127) / 255) as u8;
                pixels[d + 3] = a as u8;
            }
        }

        let rect = SheetRect { x: px, y: py, w, h, atlas_w: side, atlas_h: side };
        match sheets.iter_mut().find(|s| s.id == id) {
            Some(sheet) => {
                if sheet.frames.len() <= frame_index {
                    sheet.frames.resize(frame_index + 1, rect);
                }
                sheet.frames[frame_index] = rect;
            }
            None => {
                let mut frames = vec![rect; frame_index + 1];
                frames[frame_index] = rect;
                sheets.push(Packed { id, frames });
            }
        }
    }

    Atlas { width: side, height: side, pixels, sheets }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(n: usize, w: u32, h: u32, colour: [u8; 4]) -> Vec<Vec<u8>> {
        (0..n)
            .map(|_| colour.iter().copied().cycle().take((w * h * 4) as usize).collect())
            .collect()
    }

    #[test]
    fn packs_every_frame_without_overlapping() {
        let a = frames(4, 32, 32, [255, 0, 0, 255]);
        let b = frames(2, 16, 48, [0, 255, 0, 255]);
        let atlas = Atlas::pack(
            &[
                Source { id: 1, width: 32, height: 32, frames: &a },
                Source { id: 2, width: 16, height: 48, frames: &b },
            ],
            4096,
        )
        .expect("packs");

        let mut rects = Vec::new();
        for sheet in &atlas.sheets {
            for frame in &sheet.frames {
                rects.push(*frame);
            }
        }
        assert_eq!(rects.len(), 6);
        for (i, p) in rects.iter().enumerate() {
            assert!(p.x + p.w <= atlas.width && p.y + p.h <= atlas.height,
                    "frame {i} escapes the atlas");
            for (j, q) in rects.iter().enumerate() {
                if i >= j {
                    continue;
                }
                let disjoint = p.x + p.w <= q.x
                    || q.x + q.w <= p.x
                    || p.y + p.h <= q.y
                    || q.y + q.h <= p.y;
                assert!(disjoint, "frames {i} and {j} overlap");
            }
        }
    }

    #[test]
    fn premultiplies_and_writes_bgra() {
        // Half-transparent pure red, straight RGBA in.
        let f = frames(1, 2, 2, [255, 0, 0, 128]);
        let atlas =
            Atlas::pack(&[Source { id: 7, width: 2, height: 2, frames: &f }], 4096).unwrap();
        let r = atlas.rect(7, 0).unwrap();
        let d = ((r.y as usize) * atlas.width as usize + r.x as usize) * 4;
        assert_eq!(atlas.pixels[d], 0, "blue");
        assert_eq!(atlas.pixels[d + 1], 0, "green");
        assert_eq!(atlas.pixels[d + 2], 128, "red premultiplied by 128/255");
        assert_eq!(atlas.pixels[d + 3], 128, "alpha preserved");
    }

    #[test]
    fn uv_is_half_texel_inset_so_neighbours_cannot_bleed() {
        let rect = SheetRect { x: 0, y: 0, w: 32, h: 32, atlas_w: 64, atlas_h: 64 };
        let (u0, v0, u1, v1) = rect.uv();
        assert!(u0 > 0.0 && v0 > 0.0, "inset away from the frame edge");
        assert!(u1 < 32.0 / 64.0, "stops short of the neighbour");
        assert!((u0 - 0.5 / 64.0).abs() < 1e-6);
        assert!((v1 - 31.5 / 64.0).abs() < 1e-6);
    }

    #[test]
    fn refuses_rather_than_dropping_frames_when_it_cannot_fit() {
        let big = frames(1, 512, 512, [1, 2, 3, 255]);
        assert!(Atlas::pack(&[Source { id: 1, width: 512, height: 512, frames: &big }], 256)
            .is_none());
    }

    #[test]
    fn the_sentry_gun_fits_in_one_atlas() {
        // The whole point of the fork: the sentry's 120 baked rotations x 3
        // poses x 6 teams collapse to 3 poses of one greyscale 64x64 sheet,
        // tinted and rotated at draw time.
        let poses = frames(3, 64, 64, [200, 200, 200, 255]);
        let atlas =
            Atlas::pack(&[Source { id: 1, width: 64, height: 64, frames: &poses }], 4096)
                .expect("packs");
        assert!(atlas.width <= 256, "3 frames need a tiny atlas, got {}", atlas.width);
        assert_eq!(atlas.sheets[0].frames.len(), 3);
    }
}
