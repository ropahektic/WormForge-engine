//! Quad geometry. Rotation and scale are applied on the CPU because WA's
//! device is `D3DCREATE_SOFTWARE_VERTEXPROCESSING` — there is no hardware
//! vertex stage to offload to, and pretransformed `XYZRHW` vertices are also
//! the most conservative thing to ask of Wine's D3D9 translation.

use crate::atlas::SheetRect;
use crate::Tint;

/// Two triangles, listed rather than fanned so every sprite in the frame can
/// share one `DrawPrimitive` call. The S0 probe measured per-sprite draws at
/// 296x the cost of one batched call, so batching is not optional.
pub const VERTICES_PER_QUAD: usize = 6;

/// `D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX2`, matching `wf_gpu.cpp`.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Vertex {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub rhw: f32,
    pub colour: u32,
    /// Atlas coordinates.
    pub u: f32,
    pub v: f32,
    /// Screen coordinates, used to sample the occlusion mask at this pixel.
    pub su: f32,
    pub sv: f32,
}

/// One sprite to draw this frame, in screen pixels.
#[derive(Clone, Copy, Debug)]
pub struct Sprite {
    /// Centre, in screen pixels.
    pub x: f32,
    pub y: f32,
    /// Source rectangle within the atlas.
    pub rect: SheetRect,
    /// Degrees clockwise. WA's own art faces right at 90 degrees; callers
    /// convert, this is plain screen rotation.
    pub angle: f32,
    /// 1.0 draws at the sheet's native size.
    pub scale: f32,
    pub tint: Tint,
    /// Mirror horizontally, preserving rotation — WA's `SPRITE_MIRROR_X`.
    pub mirror_x: bool,
}

impl Sprite {
    /// Half-extent of the unrotated quad, in screen pixels.
    fn half(&self) -> (f32, f32) {
        // The whole cell's, as WA would draw it; the stored box is within.
        (
            self.rect.cw as f32 * 0.5 * self.scale,
            self.rect.ch as f32 * 0.5 * self.scale,
        )
    }

    /// Axis-aligned screen bounds after rotation, as inclusive pixel extents.
    ///
    /// The renderer needs this to build the coverage mask over only the
    /// pixels sprites actually touch: uploading the full surface every frame
    /// is the dominant per-frame memcpy, and at ~10% coverage this is an
    /// order of magnitude less work.
    pub fn bounds(&self) -> (i32, i32, i32, i32) {
        let (hx, hy) = self.half();
        let r = self.angle.to_radians();
        let (s, c) = (r.sin().abs(), r.cos().abs());
        // Extent of a rotated rectangle projected onto each axis.
        let ex = hx * c + hy * s;
        let ey = hx * s + hy * c;
        (
            (self.x - ex).floor() as i32,
            (self.y - ey).floor() as i32,
            (self.x + ex).ceil() as i32,
            (self.y + ey).ceil() as i32,
        )
    }

    /// True when the sprite cannot touch a `width` x `height` surface.
    ///
    /// WA dispatches render messages to entities regardless of visibility and
    /// `RQ_DrawSpriteLocal` does not cull, so once sprites leave the queue the
    /// culling becomes ours to do.
    pub fn offscreen(&self, width: u32, height: u32) -> bool {
        let (x0, y0, x1, y1) = self.bounds();
        x1 < 0 || y1 < 0 || x0 >= width as i32 || y0 >= height as i32
    }

    /// Append this sprite's six vertices.
    pub fn emit(&self, width: u32, height: u32, out: &mut Vec<Vertex>) {
        let r = self.angle.to_radians();
        let (sin, cos) = (r.sin(), r.cos());

        // Corner offsets, clockwise from top-left: the stored box where it
        // sits in the cell, the cell centred on (x, y). Mirrored, the box
        // sits on the other side of the centre.
        let (cw, ch) = (self.rect.cw as f32, self.rect.ch as f32);
        let mut left = self.rect.ox as f32 - cw * 0.5;
        let mut right = left + self.rect.w as f32;
        if self.mirror_x {
            (left, right) = (-right, -left);
        }
        let top = self.rect.oy as f32 - ch * 0.5;
        let bottom = top + self.rect.h as f32;
        let k = self.scale;
        let corners = [(left * k, top * k), (right * k, top * k), (right * k, bottom * k), (left * k, bottom * k)];
        let (u0, v0, u1, v1) = self.rect.uv();
        // Mirroring swaps the source columns rather than the geometry, so a
        // mirrored sprite keeps its rotation instead of reflecting it.
        let (ul, ur) = if self.mirror_x { (u1, u0) } else { (u0, u1) };
        let uvs = [(ul, v0), (ur, v0), (ur, v1), (ul, v1)];

        let vertex = |i: usize| {
            let (ox, oy) = corners[i];
            // D3D9 samples texels at pixel centres; the -0.5 bias is the same
            // one WA applies to its own fullscreen quad.
            let x = self.x + ox * cos - oy * sin - 0.5;
            let y = self.y + ox * sin + oy * cos - 0.5;
            Vertex {
                x,
                y,
                z: 0.0,
                rhw: 1.0,
                colour: self.tint.0,
                u: uvs[i].0,
                v: uvs[i].1,
                su: (x + 0.5) / width as f32,
                sv: (y + 0.5) / height as f32,
            }
        };

        let (a, b, c, d) = (vertex(0), vertex(1), vertex(2), vertex(3));
        out.extend_from_slice(&[a, b, c, a, c, d]);
    }
}

/// A free-form textured quad, in screen pixels: four corners, clockwise from
/// the one that takes the sheet's top-left, each with its own colour. What
/// Project X's `AddSpriteQ` draws (its laser ray is a strip of these).
#[derive(Clone, Copy, Debug)]
pub struct Quad {
    pub corners: [(f32, f32); 4],
    /// Per corner, as `Tint`: premultiplied, alpha 0 additive.
    pub colours: [u32; 4],
    /// Normalized coordinates inside `rect`, in corner order.
    pub uvs: [(f32, f32); 4],
    pub rect: SheetRect,
}

impl Quad {
    /// Axis-aligned screen bounds, as inclusive pixel extents.
    pub fn bounds(&self) -> (i32, i32, i32, i32) {
        let xs = self.corners.map(|c| c.0);
        let ys = self.corners.map(|c| c.1);
        let min = |v: [f32; 4]| v.iter().copied().fold(f32::INFINITY, f32::min);
        let max = |v: [f32; 4]| v.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        (
            min(xs).floor() as i32,
            min(ys).floor() as i32,
            max(xs).ceil() as i32,
            max(ys).ceil() as i32,
        )
    }

    pub fn offscreen(&self, width: u32, height: u32) -> bool {
        let (x0, y0, x1, y1) = self.bounds();
        x1 < 0 || y1 < 0 || x0 >= width as i32 || y0 >= height as i32
    }

    /// The part of the quad over its frame's stored box: corners, colours
    /// and cell coordinates, or None when no part is. The quad's `uvs` span
    /// an axis-aligned rectangle of the cell (as `wa.draw.quad` and ribbons
    /// give them): it is clipped to the box, and corners and colours follow
    /// by bilinear interpolation. A quad over a whole cell is unchanged.
    fn on_box(&self) -> Option<([(f32, f32); 4], [u32; 4], [(f32, f32); 4])> {
        let (b0, c0, b1, c1) = self.rect.box_in_cell();
        let (ua, va) = self.uvs[0];
        let (ub, vb) = self.uvs[2];
        // [a, b] clipped to [lo, hi], whichever way it runs: the clipped ends
        // and their fractions along the quad's edge.
        let clip = |a: f32, b: f32, lo: f32, hi: f32| -> Option<(f32, f32, f32, f32)> {
            let (cmin, cmax) = (a.min(b).max(lo), a.max(b).min(hi));
            if cmin >= cmax {
                return None;
            }
            let at = |t: f32| if b == a { 0.0 } else { (t - a) / (b - a) };
            let (from, to) = if b >= a { (cmin, cmax) } else { (cmax, cmin) };
            Some((from, to, at(from), at(to)))
        };
        let (u_from, u_to, s0, s1) = clip(ua, ub, b0, b1)?;
        let (v_from, v_to, t0, t1) = clip(va, vb, c0, c1)?;
        let [p0, p1, p2, p3] = self.corners;
        let lerp = |a: (f32, f32), b: (f32, f32), t: f32| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        let at = |s: f32, t: f32| lerp(lerp(p0, p1, s), lerp(p3, p2, s), t);
        let [k0, k1, k2, k3] = self.colours;
        let colour_at = |s: f32, t: f32| mix(mix(k0, k1, s), mix(k3, k2, s), t);
        Some((
            [at(s0, t0), at(s1, t0), at(s1, t1), at(s0, t1)],
            [colour_at(s0, t0), colour_at(s1, t0), colour_at(s1, t1), colour_at(s0, t1)],
            [(u_from, v_from), (u_to, v_from), (u_to, v_to), (u_from, v_to)],
        ))
    }

    /// Append this quad's six vertices, as `Sprite::emit` does.
    pub fn emit(&self, width: u32, height: u32, out: &mut Vec<Vertex>) {
        let Some((corners, colours, cell)) = self.on_box() else {
            return;
        };
        let uvs = cell.map(|(u, v)| self.rect.cell_uv(u, v));
        let vertex = |i: usize| {
            let (cx, cy) = corners[i];
            let (x, y) = (cx - 0.5, cy - 0.5);
            Vertex {
                x,
                y,
                z: 0.0,
                rhw: 1.0,
                colour: colours[i],
                u: uvs[i].0,
                v: uvs[i].1,
                su: (x + 0.5) / width as f32,
                sv: (y + 0.5) / height as f32,
            }
        };
        let (a, b, c, d) = (vertex(0), vertex(1), vertex(2), vertex(3));
        out.extend_from_slice(&[a, b, c, a, c, d]);
    }
}

/// Two vertex colours (four 8-bit channels) mixed: `t` of the way to `b`.
fn mix(a: u32, b: u32, t: f32) -> u32 {
    if a == b || t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    let mut out = 0u32;
    for shift in [0u32, 8, 16, 24] {
        let (x, y) = (((a >> shift) & 0xFF) as f32, ((b >> shift) & 0xFF) as f32);
        out |= ((x + (y - x) * t).round() as u32 & 0xFF) << shift;
    }
    out
}

/// Build the frame's vertex buffer, skipping anything off-screen.
pub fn build(sprites: &[Sprite], width: u32, height: u32) -> Vec<Vertex> {
    let mut out = Vec::with_capacity(sprites.len() * VERTICES_PER_QUAD);
    for sprite in sprites {
        if !sprite.offscreen(width, height) {
            sprite.emit(width, height, &mut out);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::SheetRect;

    fn rect() -> SheetRect {
        SheetRect::whole(0, 0, 32, 32, 64, 64)
    }

    #[test]
    fn a_trimmed_sprite_draws_its_box_where_it_sits_in_the_cell() {
        // A 32x32 cell whose visible 10x8 box starts at (20, 4).
        let mut s = sprite(0.0, 1.0);
        s.rect = SheetRect { w: 10, h: 8, ox: 20, oy: 4, cw: 32, ch: 32, ..rect() };
        let mut v = Vec::new();
        s.emit(256, 128, &mut v);
        // Cell centred on (100, 50): its left edge 84, box 20 in = 104.
        assert!((v[0].x - (104.0 - 0.5)).abs() < 0.01 && (v[0].y - (38.0 - 0.5)).abs() < 0.01);
        assert!((v[2].x - (114.0 - 0.5)).abs() < 0.01 && (v[2].y - (46.0 - 0.5)).abs() < 0.01);
        // Mirrored: the box sits 20 in from the cell's right edge (116): 86..96.
        s.mirror_x = true;
        let mut m = Vec::new();
        s.emit(256, 128, &mut m);
        assert!((m[0].x - (86.0 - 0.5)).abs() < 0.01 && (m[2].x - (96.0 - 0.5)).abs() < 0.01);
        assert_eq!(s.bounds(), (84, 34, 116, 66), "bounds stay the cell's");
    }

    #[test]
    fn a_quad_is_clipped_to_its_frames_box() {
        // Box: the right half of the cell. A quad over the whole cell keeps
        // only its right half, its colours fading along.
        let q = Quad {
            corners: [(0.0, 0.0), (100.0, 0.0), (100.0, 10.0), (0.0, 10.0)],
            colours: [0xFF00_0000, 0xFFFF_FFFF, 0xFFFF_FFFF, 0xFF00_0000],
            uvs: [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
            rect: SheetRect { w: 16, ox: 16, ..rect() },
        };
        let mut v = Vec::new();
        q.emit(256, 128, &mut v);
        assert!((v[0].x - 49.5).abs() < 0.01 && (v[1].x - 99.5).abs() < 0.01);
        assert_eq!(v[0].colour, 0xFF80_8080, "half way along the fade");
        let (u0, _, u1, _) = q.rect.uv();
        assert!((v[0].u - u0).abs() < 1e-6 && (v[1].u - u1).abs() < 1e-6);
        // A ribbon segment wholly over the clear left half draws nothing.
        let left = Quad { uvs: [(0.1, 0.0), (0.4, 0.0), (0.4, 1.0), (0.1, 1.0)], ..q };
        let mut none = Vec::new();
        left.emit(256, 128, &mut none);
        assert!(none.is_empty());
    }

    fn sprite(angle: f32, scale: f32) -> Sprite {
        Sprite {
            x: 100.0,
            y: 50.0,
            rect: rect(),
            angle,
            scale,
            tint: Tint::OPAQUE,
            mirror_x: false,
        }
    }

    #[test]
    fn a_free_quad_keeps_its_corners_colours_and_sheet_corners() {
        let q = Quad {
            corners: [(10.0, 10.0), (110.0, 10.0), (110.0, 12.0), (10.0, 12.0)],
            colours: [1, 2, 3, 4],
            uvs: [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
            rect: rect(),
        };
        let mut v = Vec::new();
        q.emit(256, 128, &mut v);
        assert_eq!(v.len(), VERTICES_PER_QUAD);
        // Two triangles: a b c, a c d.
        let order: Vec<u32> = v.iter().map(|v| v.colour).collect();
        assert_eq!(order, vec![1, 2, 3, 1, 3, 4]);
        assert!((v[1].x - 109.5).abs() < 0.01 && (v[2].y - 11.5).abs() < 0.01);
        let (u0, v0, u1, v1) = rect().uv();
        assert_eq!((v[0].u, v[0].v, v[2].u, v[2].v), (u0, v0, u1, v1));
        assert_eq!(q.bounds(), (10, 10, 110, 12));
        assert!(!q.offscreen(256, 128));
        assert!(q.offscreen(5, 5));
    }

    #[test]
    fn ribbon_segment_uses_continuous_u_faded_colours_and_culls_offscreen() {
        let q = Quad {
            // A tapered horizontal segment after ribbon expansion.
            corners: [(10.0, 8.0), (50.0, 9.0), (50.0, 11.0), (10.0, 12.0)],
            colours: [0xDC78_8CFF, 0x1E78_8CFF, 0x1E78_8CFF, 0xDC78_8CFF],
            uvs: [(0.25, 0.0), (0.75, 0.0), (0.75, 1.0), (0.25, 1.0)],
            rect: rect(),
        };
        let mut out = Vec::new();
        q.emit(128, 64, &mut out);
        let (u0, v0, u1, v1) = rect().uv();
        assert_eq!(out[0].u, u0 + (u1-u0)*0.25);
        assert_eq!(out[1].u, u0 + (u1-u0)*0.75);
        assert_eq!(out[0].v, v0);
        assert_eq!(out[2].v, v1);
        assert_eq!(out[0].colour, 0xDC78_8CFF);
        assert_eq!(out[1].colour, 0x1E78_8CFF);
        assert_eq!(q.bounds(), (10, 8, 50, 12));
        assert!(!q.offscreen(128, 64));
        assert!(q.offscreen(8, 8));
    }

    #[test]
    fn unrotated_quad_is_centred_and_sized() {
        let mut v = Vec::new();
        sprite(0.0, 1.0).emit(256, 128, &mut v);
        assert_eq!(v.len(), VERTICES_PER_QUAD);
        // 32x32 at scale 1 centred on (100, 50): corners at +/-16.
        assert!((v[0].x - 83.5).abs() < 0.01, "left {}", v[0].x);
        assert!((v[0].y - 33.5).abs() < 0.01, "top {}", v[0].y);
        assert!((v[2].x - 115.5).abs() < 0.01, "right {}", v[2].x);
        assert!((v[2].y - 65.5).abs() < 0.01, "bottom {}", v[2].y);
    }

    #[test]
    fn rotation_extends_the_diagonal_past_the_unrotated_edge() {
        // At 45 degrees a square's corner reaches half*sqrt(2) along an axis,
        // which is the property that makes a baked-rotation sheet unnecessary.
        let (x0, _, x1, _) = sprite(45.0, 1.0).bounds();
        let extent = (x1 - x0) as f32 / 2.0;
        let expected = 16.0 * 2f32.sqrt();
        assert!((extent - expected).abs() < 1.5, "extent {extent} vs {expected}");
        // A 90 degree turn of a square is the same box again.
        let (a0, b0, a1, b1) = sprite(90.0, 1.0).bounds();
        assert!((a1 - a0 - 32).abs() <= 1 && (b1 - b0 - 32).abs() <= 1);
    }

    #[test]
    fn scale_multiplies_extent() {
        let (x0, _, x1, _) = sprite(0.0, 2.0).bounds();
        assert_eq!(x1 - x0, 64);
    }

    #[test]
    fn mirroring_swaps_columns_without_reflecting_the_rotation() {
        let mut plain = Vec::new();
        let mut flipped = Vec::new();
        sprite(30.0, 1.0).emit(256, 128, &mut plain);
        let mut s = sprite(30.0, 1.0);
        s.mirror_x = true;
        s.emit(256, 128, &mut flipped);
        // Geometry is identical...
        for (a, b) in plain.iter().zip(&flipped) {
            assert!((a.x - b.x).abs() < 0.001 && (a.y - b.y).abs() < 0.001);
        }
        // ...only the source columns swap.
        assert_ne!(plain[0].u, flipped[0].u);
        assert_eq!(plain[0].u, flipped[1].u);
    }

    #[test]
    fn screen_uv_tracks_position_so_the_mask_samples_the_right_pixel() {
        let mut v = Vec::new();
        sprite(0.0, 1.0).emit(200, 100, &mut v);
        for vertex in &v {
            assert!((vertex.su - (vertex.x + 0.5) / 200.0).abs() < 1e-6);
            assert!((vertex.sv - (vertex.y + 0.5) / 100.0).abs() < 1e-6);
        }
    }

    #[test]
    fn offscreen_sprites_are_culled_but_partly_visible_ones_are_not() {
        let mut far = sprite(0.0, 1.0);
        far.x = -100.0;
        assert!(far.offscreen(256, 128));

        let mut edge = sprite(0.0, 1.0);
        edge.x = -8.0; // half of it is still on screen
        assert!(!edge.offscreen(256, 128));

        let built = build(&[far, edge], 256, 128);
        assert_eq!(built.len(), VERTICES_PER_QUAD, "only the visible one");
    }

    #[test]
    fn triangle_list_winding_covers_the_quad_once() {
        let mut v = Vec::new();
        sprite(0.0, 1.0).emit(256, 128, &mut v);
        // Two triangles sharing the 0-2 diagonal: a,b,c then a,c,d.
        assert_eq!(v[0], v[3]);
        assert_eq!(v[2], v[4]);
        assert_eq!(v.len() % 3, 0, "must be a whole number of triangles");
    }
}
