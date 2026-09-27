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
        (
            self.rect.w as f32 * 0.5 * self.scale,
            self.rect.h as f32 * 0.5 * self.scale,
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
        let (hx, hy) = self.half();
        let r = self.angle.to_radians();
        let (sin, cos) = (r.sin(), r.cos());

        // Corner offsets, clockwise from top-left.
        let corners = [(-hx, -hy), (hx, -hy), (hx, hy), (-hx, hy)];
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

    /// Append this quad's six vertices, as `Sprite::emit` does.
    pub fn emit(&self, width: u32, height: u32, out: &mut Vec<Vertex>) {
        let (u0, v0, u1, v1) = self.rect.uv();
        let uvs = [(u0, v0), (u1, v0), (u1, v1), (u0, v1)];
        let vertex = |i: usize| {
            let (cx, cy) = self.corners[i];
            let (x, y) = (cx - 0.5, cy - 0.5);
            Vertex {
                x,
                y,
                z: 0.0,
                rhw: 1.0,
                colour: self.colours[i],
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
        SheetRect { x: 0, y: 0, w: 32, h: 32, atlas_w: 64, atlas_h: 64 }
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
