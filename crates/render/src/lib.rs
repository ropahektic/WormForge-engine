//! GPU sprite renderer: geometry, atlas packing and mask building.
//!
//! Deliberately knows nothing about WA. Its inputs are a device pointer, RGBA
//! sheets, a draw list and a coverage mask; everything that must speak COM
//! lives in `native/wf_gpu.cpp`. That split keeps the arithmetic here testable
//! on any host, which matters because this is the part that is easy to get
//! subtly wrong and hard to see wrong at 60 fps.
//!
//! What this buys over compositing into WA's framebuffer (see
//! `docs/gpu-renderer-s0.md` for the measurements that justify it):
//!
//! * **Rotation and scale** are baked into the four corner positions, so one
//!   sheet replaces the sentry gun's 120 baked angles.
//! * **Tint** is a per-vertex colour, so one greyscale sheet serves all six
//!   team colours instead of six baked copies.
//! * **Additive** is premultiplied alpha 0 under `ONE`/`INVSRCALPHA`, so it
//!   shares a pipeline state with opaque sprites and needs no second pass.
//! * Sprites never occupy WA sprite slots, which is the 223-slot global
//!   budget this whole effort exists to escape.

pub mod atlas;
pub mod geom;

pub use atlas::{Atlas, SheetId, SheetRect};
pub use geom::{Quad, Sprite, Vertex, VERTICES_PER_QUAD};

/// Colour written into a vertex, premultiplied BGRA in D3D's `D3DCOLOR` order.
///
/// Alpha 0 is not "invisible": under the renderer's blend state it is exactly
/// additive, which is how PX drew its muzzle flash. Fully transparent means
/// not emitting the quad at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Tint(pub u32);

impl Tint {
    pub const OPAQUE: Tint = Tint(0xFFFF_FFFF);
    /// White at alpha 0: the sampled texel is added to the scene.
    pub const ADDITIVE: Tint = Tint(0x00FF_FFFF);

    pub fn rgb(r: u8, g: u8, b: u8) -> Tint {
        Tint(0xFF00_0000 | ((r as u32) << 16) | ((g as u32) << 8) | b as u32)
    }

    /// Same colour, drawn additively rather than over.
    pub fn additive(self) -> Tint {
        Tint(self.0 & 0x00FF_FFFF)
    }

    pub fn alpha(self) -> u8 {
        (self.0 >> 24) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn additive_is_alpha_zero_and_keeps_its_colour() {
        let orange = Tint::rgb(255, 128, 0);
        assert_eq!(orange.alpha(), 255);
        assert_eq!(orange.additive().alpha(), 0);
        // The colour must survive: additive draws the texel's own colour.
        assert_eq!(orange.additive().0 & 0x00FF_FFFF, orange.0 & 0x00FF_FFFF);
        assert_eq!(Tint::ADDITIVE.alpha(), 0);
        assert_eq!(Tint::OPAQUE.alpha(), 255);
    }
}
