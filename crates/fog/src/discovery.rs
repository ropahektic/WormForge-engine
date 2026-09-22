//! Terrain memory ("what have I seen before"), ported from wkFOW's
//! `Discovery.cpp` and adapted to this renderer's single L8 visibility mask.
//!
//! wkFOW paints remembered ground with the land's own colour through a
//! depth-gradient texture. Our fog pass darkens by one scalar per pixel, so the
//! colour gradient is out of scope; what this module keeps is the *model* —
//! which pixels are remembered, and how brightly each mode re-shows them.
//!
//! Two 1-bit planes cover the landscape rect:
//!
//!   seen        every pixel that ever rose above the outer feather ring
//!   remembered  the subset that was collision terrain when first seen
//!
//! Only worm/owned-animal light records memory. Projectile light is public but
//! transient — recording it would leave a permanent survey of wherever a shell
//! happened to fly, which is exactly the leak discovery exists to prevent.
//!
//! Hotseat is knowingly wrong, for the same reason wkFOW documents: memory is
//! permanent and cannot follow the turn-active alliance, so two alliances
//! sharing one machine see each other's discoveries. Vision itself is live and
//! unaffected.

/// A pixel counts as seen once its light rises above the outer feather ring.
/// Feather is a cosmetic ramp painted past the true vision boundary, so
/// remembering it would creep the discovered area outward a little every frame
/// and eventually reveal the map through a keyhole.
pub const SEEN_LIGHT_THRESHOLD: u8 = 200;

/// Brightness (percent) at which remembered terrain is re-shown, per mode.
/// Mirrors `Config::discoverFloor` / `discoverLiveFloor`.
pub const DISCOVER_FLOOR: u8 = 30;
pub const DISCOVER_LIVE_FLOOR: u8 = 70;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Off: no memory, unseen ground is fully fogged.
    Off = 0,
    /// Stale terrain: remembered ground shown dimmed at DISCOVER_FLOOR.
    Texture = 1,
    /// Live terrain: remembered ground kept bright at DISCOVER_LIVE_FLOOR.
    Live = 2,
    /// Premapped: the whole map starts remembered (like Texture, pre-seen).
    Premapped = 3,
}

impl Mode {
    pub fn from_i32(v: i32) -> Self {
        match v {
            2 => Mode::Live,
            3 => Mode::Premapped,
            1 => Mode::Texture,
            _ => Mode::Off,
        }
    }
}

/// One bit per landscape pixel, packed LSB-first like WA's own BitGrid.
struct Bitplane {
    bits: Vec<u8>,
    width: usize,
    height: usize,
}

impl Bitplane {
    fn new(width: usize, height: usize) -> Self {
        Bitplane {
            bits: vec![0; (width * height).div_ceil(8)],
            width,
            height,
        }
    }

    #[inline]
    fn get(&self, x: usize, y: usize) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let i = y * self.width + x;
        (self.bits[i >> 3] >> (x & 7)) & 1 != 0
    }

    #[inline]
    fn set(&mut self, x: usize, y: usize) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = y * self.width + x;
        self.bits[i >> 3] |= 1 << (x & 7);
    }

    fn fill(&mut self, value: bool) {
        self.bits.fill(if value { 0xFF } else { 0 });
    }
}

pub struct Discovery {
    width: usize,
    height: usize,
    seen: Bitplane,
    remembered: Bitplane,
    mode: Mode,
}

impl Discovery {
    pub fn new(width: usize, height: usize, mode: Mode) -> Self {
        let mut d = Discovery {
            width,
            height,
            seen: Bitplane::new(width, height),
            remembered: Bitplane::new(width, height),
            mode,
        };
        // Premapped starts with the whole map remembered. We do not know which
        // pixels are terrain yet, so `remembered` is filled wholesale; the
        // brightness test below treats remembered-open and remembered-terrain
        // alike in this port, both lifted to the floor.
        if mode == Mode::Premapped {
            d.seen.fill(true);
            d.remembered.fill(true);
        }
        d
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: Mode) {
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        if mode == Mode::Premapped {
            self.seen.fill(true);
            self.remembered.fill(true);
        }
    }

    /// Record one frame's discovering light. `light` is the native-resolution
    /// light grid (byte per landscape pixel, 0/96/192/255), `solid` reports
    /// collision terrain. Only pixels over the feather threshold are remembered.
    pub fn record(&mut self, light: &[u8], solid: impl Fn(usize, usize) -> bool) {
        if self.mode == Mode::Off {
            return;
        }
        for y in 0..self.height {
            let row = y * self.width;
            for x in 0..self.width {
                if light[row + x] >= SEEN_LIGHT_THRESHOLD && !self.seen.get(x, y) {
                    self.seen.set(x, y);
                    if solid(x, y) {
                        self.remembered.set(x, y);
                    }
                }
            }
        }
    }

    /// Permanently record an explosion crater: the blast exposed ground that is
    /// now public knowledge even after its light fades.
    pub fn notify_crater(
        &mut self,
        cx: i32,
        cy: i32,
        radius: i32,
        solid: impl Fn(usize, usize) -> bool,
    ) {
        if self.mode == Mode::Off {
            return;
        }
        let r2 = radius * radius;
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dy * dy > r2 {
                    continue;
                }
                let x = cx + dx;
                let y = cy + dy;
                if x < 0 || y < 0 {
                    continue;
                }
                let (x, y) = (x as usize, y as usize);
                if x >= self.width || y >= self.height {
                    continue;
                }
                if !self.seen.get(x, y) {
                    self.seen.set(x, y);
                    if solid(x, y) {
                        self.remembered.set(x, y);
                    }
                }
            }
        }
    }

    /// The brightness floor this mode grants a remembered pixel, as a 0..=255
    /// light value the compositor unions into the fog mask. Unremembered pixels
    /// get 0 (fully fogged).
    pub fn floor_at(&self, x: usize, y: usize) -> u8 {
        if !self.remembered.get(x, y) {
            return 0;
        }
        let pct = match self.mode {
            Mode::Off => 0,
            Mode::Texture | Mode::Premapped => DISCOVER_FLOOR,
            Mode::Live => DISCOVER_LIVE_FLOOR,
        };
        (pct as u32 * 255 / 100) as u8
    }

    pub fn is_remembered(&self, x: usize, y: usize) -> bool {
        self.remembered.get(x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_open(_x: usize, _y: usize) -> bool {
        false
    }

    #[test]
    fn off_mode_remembers_nothing() {
        let mut d = Discovery::new(8, 8, Mode::Off);
        d.record(&[255; 64], all_open);
        assert!(!d.is_remembered(4, 4));
        assert_eq!(d.floor_at(4, 4), 0);
    }

    #[test]
    fn records_only_over_threshold() {
        let mut d = Discovery::new(8, 8, Mode::Texture);
        let mut light = [0u8; 64];
        light[0] = 255; // seen
        light[1] = 150; // feather, below threshold
        d.record(&light, all_open);
        assert!(d.seen.get(0, 0));
        assert!(!d.seen.get(1, 0));
    }

    #[test]
    fn remembers_terrain_not_open_air() {
        let mut d = Discovery::new(8, 8, Mode::Texture);
        let light = [255u8; 64];
        // (0,0) is solid ground, everything else open.
        d.record(&light, |x, y| x == 0 && y == 0);
        assert!(d.is_remembered(0, 0));
        assert!(!d.is_remembered(1, 0));
    }

    #[test]
    fn texture_floor_is_dimmer_than_live() {
        let mut t = Discovery::new(4, 4, Mode::Texture);
        t.record(&[255u8; 16], |_x, _y| true);
        let mut l = Discovery::new(4, 4, Mode::Live);
        l.record(&[255u8; 16], |_x, _y| true);
        assert!(t.floor_at(0, 0) < l.floor_at(0, 0));
        assert_eq!(u32::from(t.floor_at(0, 0)), DISCOVER_FLOOR as u32 * 255 / 100);
        assert_eq!(u32::from(l.floor_at(0, 0)), DISCOVER_LIVE_FLOOR as u32 * 255 / 100);
    }

    #[test]
    fn premapped_starts_remembered() {
        let d = Discovery::new(4, 4, Mode::Premapped);
        assert!(d.is_remembered(2, 2));
        assert_eq!(u32::from(d.floor_at(2, 2)), DISCOVER_FLOOR as u32 * 255 / 100);
    }

    #[test]
    fn crater_marks_permanent_knowledge() {
        let mut d = Discovery::new(16, 16, Mode::Texture);
        d.notify_crater(8, 8, 3, |_x, _y| true);
        assert!(d.is_remembered(8, 8));
        assert!(d.is_remembered(10, 8));
        assert!(!d.is_remembered(15, 15));
    }
}
