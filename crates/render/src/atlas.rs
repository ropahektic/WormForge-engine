//! Atlas packing. Every opted-in sheet goes into one texture so a whole frame
//! is a single `DrawPrimitive` with no texture changes between sprites.
//!
//! A shelf packer is enough here and is worth preferring over anything
//! cleverer: a predictable layout is far easier to debug from a screenshot
//! than an optimal one.
//!
//! Each frame is stored trimmed to its visible pixels (alpha above 0), and
//! identical trimmed frames once: across our packs' sheets that is about half
//! the space of storing every whole cell. A frame's rect remembers where its
//! box sits in its cell, so a sprite still draws as the whole cell would.

/// Identifies a sheet within the atlas. Distinct from WA's sprite ids, which
/// these deliberately never become — escaping the 223-slot table is the point.
pub type SheetId = u32;

/// Where one frame lives in the atlas, in texels: its visible box at (x, y),
/// w by h, and where that box sits in the frame's whole cell (ox, oy in a
/// cw by ch cell).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SheetRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub atlas_w: u32,
    pub atlas_h: u32,
    pub ox: u32,
    pub oy: u32,
    pub cw: u32,
    pub ch: u32,
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

    /// A box covering its whole cell, as every frame was before trimming.
    pub fn whole(x: u32, y: u32, w: u32, h: u32, atlas_w: u32, atlas_h: u32) -> SheetRect {
        SheetRect { x, y, w, h, atlas_w, atlas_h, ox: 0, oy: 0, cw: w, ch: h }
    }

    /// The visible box in cell coordinates, 0..1: (u0, v0, u1, v1).
    pub fn box_in_cell(&self) -> (f32, f32, f32, f32) {
        let (cw, ch) = (self.cw.max(1) as f32, self.ch.max(1) as f32);
        (
            self.ox as f32 / cw,
            self.oy as f32 / ch,
            (self.ox + self.w) as f32 / cw,
            (self.oy + self.h) as f32 / ch,
        )
    }

    /// A point of the cell, 0..1 across it, as a texture coordinate: across
    /// the box exactly as `uv` maps it (texel centres, half-texel inset).
    pub fn cell_uv(&self, u: f32, v: f32) -> (f32, f32) {
        let texel = |t: f32, cell: u32, off: u32, size: u32| {
            let size = size.max(1) as f32;
            0.5 + (t * cell as f32 - off as f32) * (size - 1.0) / size
        };
        (
            (self.x as f32 + texel(u, self.cw, self.ox, self.w)) / self.atlas_w as f32,
            (self.y as f32 + texel(v, self.ch, self.oy, self.h)) / self.atlas_h as f32,
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
    /// Texels the stored pictures cover, and how many frames shared a
    /// picture with an earlier one.
    pub used: u64,
    pub shared: usize,
}

/// One sheet offered to the packer: equal-sized RGBA frames.
pub struct Source<'a> {
    pub id: SheetId,
    pub width: u32,
    pub height: u32,
    /// Each frame is `width * height * 4` bytes of straight (not
    /// premultiplied) RGBA, exactly as `gfx::RgbaFrame` stores it.
    pub frames: Vec<&'a [u8]>,
}

/// A frame cut to its visible box, and the picture stored for it.
struct Cut {
    id: SheetId,
    frame: usize,
    ox: u32,
    oy: u32,
    cw: u32,
    ch: u32,
    picture: usize,
}

/// A trimmed picture as it goes into the atlas: w by h, straight RGBA.
struct Picture {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

/// The box of `frame` (w by h RGBA) with any alpha, or a 1x1 clear corner
/// when it has none (a frame must still have a rect).
fn visible_box(frame: &[u8], w: u32, h: u32) -> (u32, u32, u32, u32) {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            let a = frame.get(((y * w + x) * 4 + 3) as usize).copied().unwrap_or(0);
            if a != 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x1 <= x0 || y1 <= y0 {
        (0, 0, 1, 1)
    } else {
        (x0, y0, x1 - x0, y1 - y0)
    }
}

/// Every frame trimmed, identical pictures shared.
fn cut(sources: &[Source]) -> (Vec<Cut>, Vec<Picture>) {
    use std::collections::hash_map::DefaultHasher;
    use std::collections::HashMap;
    use std::hash::{Hash, Hasher};
    let mut cuts = Vec::new();
    let mut pictures: Vec<Picture> = Vec::new();
    let mut by_hash: HashMap<u64, Vec<usize>> = HashMap::new();
    for source in sources {
        let (cw, ch) = (source.width, source.height);
        if cw == 0 || ch == 0 {
            continue;
        }
        for (index, frame) in source.frames.iter().enumerate() {
            let (ox, oy, w, h) = visible_box(frame, cw, ch);
            let mut rgba = Vec::with_capacity((w * h * 4) as usize);
            for y in oy..oy + h {
                let row = ((y * cw + ox) * 4) as usize;
                let end = row + (w * 4) as usize;
                match frame.get(row..end) {
                    Some(bytes) => rgba.extend_from_slice(bytes),
                    None => rgba.resize(rgba.len() + (w * 4) as usize, 0),
                }
            }
            let mut hasher = DefaultHasher::new();
            (w, h, &rgba).hash(&mut hasher);
            let same = by_hash.entry(hasher.finish()).or_default();
            let picture = match same.iter().copied().find(|&p| {
                let q = &pictures[p];
                q.w == w && q.h == h && q.rgba == rgba
            }) {
                Some(p) => p,
                None => {
                    pictures.push(Picture { w, h, rgba });
                    same.push(pictures.len() - 1);
                    pictures.len() - 1
                }
            };
            cuts.push(Cut { id: source.id, frame: index, ox, oy, cw, ch, picture });
        }
    }
    (cuts, pictures)
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
        let (cuts, pictures) = cut(sources);
        let entries: Vec<(u32, u32)> = pictures.iter().map(|p| (p.w, p.h)).collect();
        if entries.is_empty() {
            return None;
        }

        // Tallest first: shelves then waste far less vertical space than they
        // do in source order, where one tall frame can set a whole shelf's
        // height for a row of short ones.
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by(|&a, &b| {
            entries[b].1
                .cmp(&entries[a].1)
                .then(entries[b].0.cmp(&entries[a].0))
                .then(a.cmp(&b))
        });

        // Start at the widest frame (rounded up to a power of two) and double
        // until everything fits.
        let widest = entries.iter().map(|e| e.0 + PAD * 2).max()?;
        let mut side = 64u32.max(widest.next_power_of_two());
        loop {
            if side > max_side {
                return None;
            }
            if let Some(placed) = shelf_pack(&entries, &order, side) {
                return Some(assemble(&cuts, &pictures, &placed, side));
            }
            side *= 2;
        }
    }
}

/// Place every entry, or report that this side length is too small.
fn shelf_pack(entries: &[(u32, u32)], order: &[usize], side: u32) -> Option<Vec<(u32, u32)>> {
    let mut placed = vec![(0u32, 0u32); entries.len()];
    let (mut x, mut y, mut shelf_h) = (PAD, PAD, 0u32);
    for &i in order {
        let (w, h) = entries[i];
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

/// Blit the pictures into the packed layout, premultiplying as we go, and
/// give every frame its rect.
fn assemble(cuts: &[Cut], pictures: &[Picture], placed: &[(u32, u32)], side: u32) -> Atlas {
    let mut pixels = vec![0u8; (side as usize) * (side as usize) * 4];
    for (picture, &(px, py)) in pictures.iter().zip(placed) {
        let (w, h) = (picture.w as usize, picture.h as usize);
        for row in 0..h {
            for col in 0..w {
                let s = (row * w + col) * 4;
                let (r, g, b, a) = (
                    picture.rgba[s] as u32,
                    picture.rgba[s + 1] as u32,
                    picture.rgba[s + 2] as u32,
                    picture.rgba[s + 3] as u32,
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
    }

    let mut sheets: Vec<Packed> = Vec::new();
    for c in cuts {
        let (px, py) = placed[c.picture];
        let p = &pictures[c.picture];
        let rect = SheetRect {
            x: px, y: py, w: p.w, h: p.h, atlas_w: side, atlas_h: side,
            ox: c.ox, oy: c.oy, cw: c.cw, ch: c.ch,
        };
        match sheets.iter_mut().find(|s| s.id == c.id) {
            Some(sheet) => {
                if sheet.frames.len() <= c.frame {
                    sheet.frames.resize(c.frame + 1, rect);
                }
                sheet.frames[c.frame] = rect;
            }
            None => {
                let mut frames = vec![rect; c.frame + 1];
                frames[c.frame] = rect;
                sheets.push(Packed { id: c.id, frames });
            }
        }
    }

    let used = pictures.iter().map(|p| p.w as u64 * p.h as u64).sum();
    let shared = cuts.len() - pictures.len();
    Atlas { width: side, height: side, pixels, sheets, used, shared }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(n: usize, w: u32, h: u32, colour: [u8; 4]) -> Vec<Vec<u8>> {
        (0..n)
            .map(|_| colour.iter().copied().cycle().take((w * h * 4) as usize).collect())
            .collect()
    }

    /// Frames that differ: frame i's first pixel's red is i.
    fn distinct(n: usize, w: u32, h: u32) -> Vec<Vec<u8>> {
        let mut f = frames(n, w, h, [9, 9, 9, 255]);
        for (i, frame) in f.iter_mut().enumerate() {
            frame[0] = i as u8;
        }
        f
    }

    fn slices(frames: &[Vec<u8>]) -> Vec<&[u8]> {
        frames.iter().map(|f| f.as_slice()).collect()
    }

    #[test]
    fn packs_every_frame_without_overlapping() {
        let a = distinct(4, 32, 32);
        let b = distinct(2, 16, 48);
        let atlas = Atlas::pack(
            &[
                Source { id: 1, width: 32, height: 32, frames: slices(&a) },
                Source { id: 2, width: 16, height: 48, frames: slices(&b) },
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
            Atlas::pack(&[Source { id: 7, width: 2, height: 2, frames: slices(&f) }], 4096).unwrap();
        let r = atlas.rect(7, 0).unwrap();
        let d = ((r.y as usize) * atlas.width as usize + r.x as usize) * 4;
        assert_eq!(atlas.pixels[d], 0, "blue");
        assert_eq!(atlas.pixels[d + 1], 0, "green");
        assert_eq!(atlas.pixels[d + 2], 128, "red premultiplied by 128/255");
        assert_eq!(atlas.pixels[d + 3], 128, "alpha preserved");
    }

    #[test]
    fn uv_is_half_texel_inset_so_neighbours_cannot_bleed() {
        let rect = SheetRect::whole(0, 0, 32, 32, 64, 64);
        let (u0, v0, u1, v1) = rect.uv();
        assert!(u0 > 0.0 && v0 > 0.0, "inset away from the frame edge");
        assert!(u1 < 32.0 / 64.0, "stops short of the neighbour");
        assert!((u0 - 0.5 / 64.0).abs() < 1e-6);
        assert!((v1 - 31.5 / 64.0).abs() < 1e-6);
    }

    #[test]
    fn a_frame_is_stored_trimmed_and_knows_where_its_box_sits() {
        // 8x6, opaque only at x 2..5, y 1..4.
        let mut f = frames(1, 8, 6, [0, 0, 0, 0]);
        for y in 1..4 {
            for x in 2..5 {
                let i = (y * 8 + x) * 4;
                f[0][i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let atlas = Atlas::pack(&[Source { id: 1, width: 8, height: 6, frames: slices(&f) }], 4096).unwrap();
        let r = atlas.rect(1, 0).unwrap();
        assert_eq!((r.w, r.h, r.ox, r.oy, r.cw, r.ch), (3, 3, 2, 1, 8, 6));
        let d = ((r.y as usize) * atlas.width as usize + r.x as usize) * 4;
        assert_eq!(&atlas.pixels[d..d + 4], &[0, 0, 255, 255], "the box's first texel is the red one");
        assert_eq!(r.box_in_cell(), (2.0 / 8.0, 1.0 / 6.0, 5.0 / 8.0, 4.0 / 6.0));
    }

    #[test]
    fn identical_frames_share_one_picture() {
        let f = frames(5, 16, 16, [1, 2, 3, 255]);
        let atlas = Atlas::pack(&[Source { id: 1, width: 16, height: 16, frames: slices(&f) }], 4096).unwrap();
        let rects = &atlas.sheets[0].frames;
        assert_eq!(rects.len(), 5);
        assert!(rects.iter().all(|r| (r.x, r.y) == (rects[0].x, rects[0].y)));
    }

    #[test]
    fn an_empty_frame_still_has_a_rect() {
        let f = frames(1, 4, 4, [0, 0, 0, 0]);
        let atlas = Atlas::pack(&[Source { id: 1, width: 4, height: 4, frames: slices(&f) }], 4096).unwrap();
        let r = atlas.rect(1, 0).unwrap();
        assert_eq!((r.w, r.h, r.cw, r.ch), (1, 1, 4, 4));
    }

    #[test]
    fn cell_uv_maps_the_box_as_uv_does_and_a_whole_cell_as_before() {
        let whole = SheetRect::whole(10, 20, 32, 16, 256, 256);
        let (u0, v0, u1, v1) = whole.uv();
        assert_eq!(whole.cell_uv(0.0, 0.0), (u0, v0));
        assert_eq!(whole.cell_uv(1.0, 1.0), (u1, v1));
        let trimmed = SheetRect { ox: 4, oy: 2, cw: 40, ch: 20, ..whole };
        let (b0, c0, b1, c1) = trimmed.box_in_cell();
        let (t0, s0, t1, s1) = trimmed.uv();
        let close = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6;
        assert!(close(trimmed.cell_uv(b0, c0), (t0, s0)));
        assert!(close(trimmed.cell_uv(b1, c1), (t1, s1)));
    }

    #[test]
    fn refuses_rather_than_dropping_frames_when_it_cannot_fit() {
        let big = frames(1, 512, 512, [1, 2, 3, 255]);
        assert!(Atlas::pack(&[Source { id: 1, width: 512, height: 512, frames: slices(&big) }], 256)
            .is_none());
    }

    #[test]
    fn the_sentry_gun_fits_in_one_atlas() {
        // The whole point of the fork: the sentry's 120 baked rotations x 3
        // poses x 6 teams collapse to 3 poses of one greyscale 64x64 sheet,
        // tinted and rotated at draw time.
        let poses = frames(3, 64, 64, [200, 200, 200, 255]);
        let atlas =
            Atlas::pack(&[Source { id: 1, width: 64, height: 64, frames: slices(&poses) }], 4096)
                .expect("packs");
        assert!(atlas.width <= 256, "3 frames need a tiny atlas, got {}", atlas.width);
        assert_eq!(atlas.sheets[0].frames.len(), 3);
    }
}
