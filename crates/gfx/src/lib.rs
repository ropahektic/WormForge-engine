//! Sprite names are `mod_id` + relative path. Numeric WA sprite indexes
//! are not part of the public API (they collide across packs).
//!
//! Projectile look: a PNG strip (`sprites/ball.png`, 1 row × N columns)
//! or a GIF (`sprites/foo.gif`, native frames, looped). Stock names
//! (`grenade`, `missile`, `uzihit`, …) remain a fallback that borrows a WA sprite.

pub mod icon;
mod gif_decode;
mod stock_ids;
pub mod composite;

use std::io::Cursor;
use std::path::Path;

pub use gif_decode::{load_gif_bytes, load_gif_file};
pub use stock_ids::STOCK_SPRITES;

/// Gfx.dir stem (or a short alias) → Display sprite id.
///
/// Names are the `.spr` archive stems (missile=48, grenade=50, uzihit=114, …).
/// Aliases: `bazooka`→missile, `holy`→hgrenade, `mine`→mineon, `cursor`→cursorr,
/// `homing`→hmissil1, `pigeon`→pigeonr0.
fn lookup_stock(name: &str) -> Option<i32> {
    STOCK_SPRITES
        .binary_search_by_key(&name, |&(n, _)| n)
        .ok()
        .map(|i| STOCK_SPRITES[i].1)
}

pub fn stock_sprite_id(name: &str) -> Option<i32> {
    let lowered = name.trim().to_ascii_lowercase();
    let key = match lowered.as_str() {
        "bazooka" => "missile",
        "holy" | "holy_grenade" => "hgrenade",
        "mine" => "mineon",
        "cursor" => "cursorr",
        "homing" | "homing_missile" => "hmissil1",
        "pigeon" => "pigeonr0",
        other => other,
    };
    lookup_stock(key)
        .or_else(|| lookup_stock(&key.replace('-', "_")))
        .or_else(|| lookup_stock(&key.replace('_', "-")))
}

/// First non-`_2` stem for a Display id (frontend duplicates keep the `_2` suffix).
pub fn stock_sprite_name(id: i32) -> Option<&'static str> {
    STOCK_SPRITES
        .iter()
        .find(|(n, i)| *i == id && !n.ends_with("_2"))
        .or_else(|| STOCK_SPRITES.iter().find(|(_, i)| *i == id))
        .map(|(n, _)| *n)
}

pub fn sprite_key(mod_id: &str, relative: &str) -> String {
    format!("{mod_id}:{relative}")
}

/// Stock and custom-weapon worm poses, including slope variants. Used only by
/// slot-scoped `worm_sprites`, never to replace global stock sheets.
pub fn stock_worm_sprite_id(name: &str) -> Option<i32> {
    match name {
        "weaponlnk" => Some(-1),
        "weaponlnku" => Some(-2),
        "weaponlnkd" => Some(-3),
        "wbazlnk" => Some(436),
        "wbazlnku" => Some(437),
        "wbazlnkd" => Some(438),
        "wbaz" => Some(525),
        "wbazu" => Some(526),
        "wbazd" => Some(527),
        "wthrow" => Some(591),
        "wthrowu" => Some(592),
        "wthrowd" => Some(593),
        _ => None,
    }
}

/// Inverse of WA's floor(phase * frame_count / 65536). Round UP so an
/// explicit frame does not accidentally select its predecessor.
pub fn frame_phase(frame: i32, count: usize) -> Option<u32> {
    if count == 0 || count > 0x10000 {
        return None;
    }
    let index = frame.rem_euclid(count as i32) as u64;
    Some((index * 0x10000).div_ceil(count as u64) as u32)
}

/// One RGBA8888 frame (row-major, 4 bytes per pixel).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Equal-sized frames (PNG strip columns, or composited GIF frames).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpriteStrip {
    pub frames: Vec<RgbaFrame>,
    /// Written into the `.spr` header. GIFs use the file's average delay
    /// (1..=50). PNG strips stay at 10; those weapons usually drive
    /// `animation_phase` from velocity (anim_kind 2), not this fps.
    pub fps: u16,
    /// `.spr` flags bit 0: wrap `anim_value` instead of clamping at the last
    /// frame. GIF sheets set this so the loop is natural.
    pub looping: bool,
}

impl SpriteStrip {
    /// The same strip multiplied by `tint`, channel by channel (alpha kept):
    /// PX's `SetColorMod(rgb, 6)`. PX draws every object it owns this way
    /// while it sinks, with `RGB(90, 90, 240)`.
    pub fn multiply(&self, tint: [u8; 3]) -> SpriteStrip {
        let frames = self
            .frames
            .iter()
            .map(|f| RgbaFrame {
                width: f.width,
                height: f.height,
                rgba: f
                    .rgba
                    .chunks_exact(4)
                    .flat_map(|p| {
                        let m = |c: u8, t: u8| ((c as u32 * t as u32 + 127) / 255) as u8;
                        [m(p[0], tint[0]), m(p[1], tint[1]), m(p[2], tint[2]), p[3]]
                    })
                    .collect(),
            })
            .collect();
        SpriteStrip { frames, ..self.clone() }
    }

    /// `animation_phase` increment per FrameFinish tick so blit walks the
    /// strip at `fps`. Zero when the sheet is not a looping GIF.
    pub fn phase_delta(&self) -> u32 {
        if !self.looping || self.frames.len() < 2 {
            return 0;
        }
        let n = self.frames.len() as u64;
        let fps = (self.fps.max(1) as u64).min(50);
        ((0x10000 * fps) / (n * 50)).max(1) as u32
    }

    /// Horizontal platform on the lowest opaque row (union across frames).
    /// Used as the collision / explosion contact instead of a centered pixel.
    pub fn feet_platform(&self) -> Option<FeetPlatform> {
        feet_platform(&self.frames)
    }

    /// First-frame opaque bitmask (static PNG / single-frame GIF).
    pub fn opaque_mask(&self) -> Option<OpaqueMask> {
        self.frames.first().map(OpaqueMask::from_frame)
    }

    /// Per-frame opaque bitmasks. Multi-frame GIFs enable **experimental**
    /// animated collision (walk-grid stamp follows `animation_phase`).
    pub fn opaque_masks(&self) -> Vec<OpaqueMask> {
        self.frames.iter().map(OpaqueMask::from_frame).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FeetPlatform {
    pub width: i32,
    pub height: i32,
    /// Bottom-most opaque row (pixels from top).
    pub y: i32,
    pub x0: i32,
    pub x1: i32,
}

pub fn feet_platform(frames: &[RgbaFrame]) -> Option<FeetPlatform> {
    let frame = frames.first()?;
    let w = frame.width as i32;
    let h = frame.height as i32;
    if w <= 0 || h <= 0 {
        return None;
    }
    let mut y = -1i32;
    let mut x0 = w;
    let mut x1 = -1i32;
    for frame in frames {
        if frame.width as i32 != w || frame.height as i32 != h {
            continue;
        }
        let mut row = -1i32;
        let mut left = w;
        let mut right = -1i32;
        for py in (0..h).rev() {
            let mut found = false;
            for px in 0..w {
                if opaque_pixel(frame, px as u32, py as u32) {
                    found = true;
                    left = left.min(px);
                    right = right.max(px);
                }
            }
            if found {
                row = py;
                break;
            }
        }
        if row < 0 {
            continue;
        }
        y = y.max(row);
        x0 = x0.min(left);
        x1 = x1.max(right);
    }
    if y < 0 || x1 < x0 {
        return None;
    }
    Some(FeetPlatform {
        width: w,
        height: h,
        y,
        x0,
        x1,
    })
}

fn opaque_pixel(frame: &RgbaFrame, x: u32, y: u32) -> bool {
    let i = ((y * frame.width + x) * 4) as usize;
    if i + 3 >= frame.rgba.len() {
        return false;
    }
    let r = frame.rgba[i];
    let g = frame.rgba[i + 1];
    let b = frame.rgba[i + 2];
    let a = frame.rgba[i + 3];
    // Alpha is the hole test. Dark fur / hair / outlines must stay solid.
    if a < 20 {
        return false;
    }
    if a < 255 {
        return true;
    }
    // Fully opaque sources with no alpha (keyed black empty): need some chroma.
    (r as u16) + (g as u16) + (b as u16) >= 6
}

/// Packed opaque-pixel collision silhouette (1 bit per pixel, row-major).
/// Built once from the sheet's first frame — GIF anim does not swap it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpaqueMask {
    pub width: u32,
    pub height: u32,
    bits: Vec<u8>,
}

impl OpaqueMask {
    pub fn from_frame(frame: &RgbaFrame) -> Self {
        let w = frame.width;
        let h = frame.height;
        let pixels = (w as usize).saturating_mul(h as usize);
        let mut bits = vec![0u8; pixels.div_ceil(8)];
        for y in 0..h {
            for x in 0..w {
                if opaque_pixel(frame, x, y) {
                    let i = (y as usize) * (w as usize) + (x as usize);
                    bits[i / 8] |= 1 << (i % 8);
                }
            }
        }
        let mut mask = Self {
            width: w,
            height: h,
            bits,
        };
        // Thin features / AA fringes (ears, hair, antennae) need a fatter solid
        // so worm feet and hitscan catch them. Three steps covers ~3px tips.
        mask.dilate_once();
        mask.dilate_once();
        mask.dilate_once();
        mask
    }

    pub fn from_strip(strip: &SpriteStrip) -> Option<Self> {
        strip.frames.first().map(Self::from_frame)
    }

    #[inline]
    pub fn test(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return false;
        }
        let i = (y as usize) * (self.width as usize) + (x as usize);
        (self.bits[i / 8] & (1 << (i % 8))) != 0
    }

    fn set_bit(&mut self, x: i32, y: i32) {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        let i = (y as usize) * (self.width as usize) + (x as usize);
        self.bits[i / 8] |= 1 << (i % 8);
    }

    fn dilate_once(&mut self) {
        let w = self.width as i32;
        let h = self.height as i32;
        let mut add: Vec<(i32, i32)> = Vec::new();
        for y in 0..h {
            for x in 0..w {
                if !self.test(x, y) {
                    continue;
                }
                for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                    let nx = x + dx;
                    let ny = y + dy;
                    if !self.test(nx, ny) {
                        add.push((nx, ny));
                    }
                }
            }
        }
        for (x, y) in add {
            self.set_bit(x, y);
        }
    }

    /// Any opaque bit in the rectangle [x0,x1]×[y0,y1] (inclusive).
    pub fn any_in_rect(&self, x0: i32, y0: i32, x1: i32, y1: i32) -> bool {
        let x0 = x0.max(0);
        let y0 = y0.max(0);
        let x1 = x1.min(self.width as i32 - 1);
        let y1 = y1.min(self.height as i32 - 1);
        if x1 < x0 || y1 < y0 {
            return false;
        }
        for y in y0..=y1 {
            for x in x0..=x1 {
                if self.test(x, y) {
                    return true;
                }
            }
        }
        false
    }
}

pub fn is_sheet_path(name: &str) -> bool {
    let n = name.replace('\\', "/").to_ascii_lowercase();
    n.contains('/') || n.ends_with(".png") || n.ends_with(".gif")
}

pub fn load_png_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("png header: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("png frame: {e}"))?;
    let width = info.width;
    let height = info.height;
    buf.truncate(info.buffer_size());
    let rgba = to_rgba(info.color_type, info.bit_depth, width, height, &buf)?;
    Ok((width, height, rgba))
}

fn to_rgba(
    color: png::ColorType,
    bit_depth: png::BitDepth,
    width: u32,
    height: u32,
    buf: &[u8],
) -> Result<Vec<u8>, String> {
    if bit_depth != png::BitDepth::Eight {
        return Err("only 8-bit PNG is supported".into());
    }
    let n = (width as usize) * (height as usize);
    match color {
        png::ColorType::Rgba => {
            if buf.len() < n * 4 {
                return Err("truncated rgba png".into());
            }
            Ok(buf[..n * 4].to_vec())
        }
        png::ColorType::Rgb => {
            if buf.len() < n * 3 {
                return Err("truncated rgb png".into());
            }
            let mut out = Vec::with_capacity(n * 4);
            for chunk in buf[..n * 3].chunks_exact(3) {
                out.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
            Ok(out)
        }
        png::ColorType::Grayscale => {
            if buf.len() < n {
                return Err("truncated gray png".into());
            }
            let mut out = Vec::with_capacity(n * 4);
            for &v in &buf[..n] {
                out.extend_from_slice(&[v, v, v, 255]);
            }
            Ok(out)
        }
        png::ColorType::GrayscaleAlpha => {
            if buf.len() < n * 2 {
                return Err("truncated gray-alpha png".into());
            }
            let mut out = Vec::with_capacity(n * 4);
            for chunk in buf[..n * 2].chunks_exact(2) {
                out.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
            Ok(out)
        }
        other => Err(format!("unsupported png color type {other:?}")),
    }
}

pub fn slice_strip(width: u32, height: u32, rgba: &[u8], columns: u32) -> Result<SpriteStrip, String> {
    if columns == 0 {
        return Err("sprite_columns must be > 0".into());
    }
    if width % columns != 0 {
        return Err(format!(
            "png width {width} is not divisible by sprite_columns {columns}"
        ));
    }
    let fw = width / columns;
    if fw == 0 || height == 0 {
        return Err("empty sprite frame".into());
    }
    let expect = (width as usize) * (height as usize) * 4;
    if rgba.len() < expect {
        return Err("rgba buffer too small for png size".into());
    }
    let mut frames = Vec::with_capacity(columns as usize);
    for col in 0..columns {
        let mut frame = vec![0u8; (fw as usize) * (height as usize) * 4];
        for y in 0..height as usize {
            let src = (y * width as usize + (col * fw) as usize) * 4;
            let dst = y * (fw as usize) * 4;
            frame[dst..dst + (fw as usize) * 4]
                .copy_from_slice(&rgba[src..src + (fw as usize) * 4]);
        }
        frames.push(RgbaFrame {
            width: fw,
            height,
            rgba: frame,
        });
    }
    Ok(SpriteStrip {
        frames,
        fps: 10,
        looping: false,
    })
}

pub fn slice_rows(width: u32, height: u32, rgba: &[u8], rows: u32) -> Result<SpriteStrip, String> {
    if rows == 0 {
        return Err("sprite rows must be > 0".into());
    }
    if height % rows != 0 {
        return Err(format!(
            "png height {height} is not divisible by {rows} rows"
        ));
    }
    let fh = height / rows;
    if width == 0 || fh == 0 {
        return Err("empty sprite frame".into());
    }
    let expect = (width as usize) * (height as usize) * 4;
    if rgba.len() < expect {
        return Err("rgba buffer too small for png size".into());
    }
    let mut frames = Vec::with_capacity(rows as usize);
    for row in 0..rows {
        let mut frame = vec![0u8; (width as usize) * (fh as usize) * 4];
        for y in 0..fh as usize {
            let src = ((row as usize * fh as usize + y) * width as usize) * 4;
            let dst = y * (width as usize) * 4;
            frame[dst..dst + (width as usize) * 4]
                .copy_from_slice(&rgba[src..src + (width as usize) * 4]);
        }
        frames.push(RgbaFrame {
            width,
            height: fh,
            rgba: frame,
        });
    }
    Ok(SpriteStrip {
        frames,
        fps: 10,
        looping: false,
    })
}

pub fn load_strip_bytes(bytes: &[u8], columns: u32) -> Result<SpriteStrip, String> {
    let (w, h, rgba) = load_png_rgba(bytes)?;
    // Horizontal sheet: N columns, one row. Vertical sheet: one column of
    // square cells (height divisible by width).
    if columns > 1 && w % columns == 0 && w / columns > 0 && w > h {
        slice_strip(w, h, &rgba, columns)
    } else if h > w && w > 0 && h % w == 0 {
        slice_rows(w, h, &rgba, h / w)
    } else {
        Ok(SpriteStrip {
            frames: vec![RgbaFrame {
                width: w,
                height: h,
                rgba,
            }],
            fps: 10,
            looping: false,
        })
    }
}

pub fn load_strip_file(path: &Path, columns: u32) -> Result<SpriteStrip, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    load_strip_bytes(&bytes, columns).map_err(|e| format!("{}: {e}", path.display()))
}

/// PNG strip (`columns`), GIF, or a folder of `frame*.png` / sorted PNGs.
pub fn load_sprite_path(path: &Path, columns: u32) -> Result<SpriteStrip, String> {
    if path.is_dir() {
        return load_png_sequence(path);
    }
    load_sprite_file(path, columns)
}

/// Ordered PNG frames in a directory (Fantasy Spells-style `frame0000.png`).
pub fn load_png_sequence(dir: &Path) -> Result<SpriteStrip, String> {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("png"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(format!("{}: no PNG frames", dir.display()));
    }
    let mut frames = Vec::with_capacity(files.len());
    for path in files {
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (w, h, rgba) = load_png_rgba(&bytes)?;
        frames.push(RgbaFrame {
            width: w,
            height: h,
            rgba,
        });
    }
    let w0 = frames[0].width;
    let h0 = frames[0].height;
    for (i, f) in frames.iter().enumerate() {
        if f.width != w0 || f.height != h0 {
            return Err(format!(
                "{}: frame {i} is {}x{}, expected {w0}x{h0}",
                dir.display(),
                f.width,
                f.height
            ));
        }
    }
    Ok(SpriteStrip {
        frames,
        fps: 15,
        looping: true,
    })
}

/// PNG strip (uses `columns`) or GIF (native frames, `columns` ignored).
pub fn load_sprite_file(path: &Path, columns: u32) -> Result<SpriteStrip, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "gif" {
        load_gif_file(path)
    } else {
        load_strip_file(path, columns)
    }
}

/// Encode a strip as an uncompressed WA `.spr` (ProcessSprite, no 0x4000 LZSS).
///
/// Layout matches stock ProcessSprite (0x004FAB80): palette RGB is 1-based in
/// the bitmap (index 0 = transparent). ProcessSprite remaps those indices
/// through PaletteContext__MapColor into the live display palette.
pub fn encode_spr(strip: &SpriteStrip) -> Result<Vec<u8>, String> {
    if strip.frames.is_empty() {
        return Err("empty sprite strip".into());
    }
    let width = strip.frames[0].width;
    let height = strip.frames[0].height;
    if width == 0 || height == 0 || width > 0x7FFF || height > 0x7FFF {
        return Err("invalid frame size".into());
    }
    for f in &strip.frames {
        if f.width != width || f.height != height {
            return Err("sprite frames must share one size".into());
        }
    }

    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut index_of: std::collections::HashMap<(u8, u8, u8), u8> = std::collections::HashMap::new();
    let mut bitmaps: Vec<Vec<u8>> = Vec::with_capacity(strip.frames.len());

    for frame in &strip.frames {
        let mut bm = vec![0u8; (width * height) as usize];
        for i in 0..(width * height) as usize {
            let o = i * 4;
            if o + 3 >= frame.rgba.len() {
                continue;
            }
            let r = frame.rgba[o];
            let g = frame.rgba[o + 1];
            let b = frame.rgba[o + 2];
            let a = frame.rgba[o + 3];
            if spr_key(r, g, b, a) {
                bm[i] = 0;
                continue;
            }
            let key = (r, g, b);
            let idx = if let Some(&id) = index_of.get(&key) {
                id
            } else if palette.len() >= 254 {
                nearest_palette(&palette, r, g, b)
            } else {
                palette.push([r, g, b]);
                let id = palette.len() as u8; // 1-based
                index_of.insert(key, id);
                id
            };
            bm[i] = idx;
        }
        bitmaps.push(bm);
    }
    if palette.is_empty() {
        palette.push([0, 255, 0]);
    }

    let mut data = Vec::new();
    data.extend_from_slice(&0u32.to_le_bytes());
    let size_at = data.len();
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes()); // header_flags: uncompressed
    data.extend_from_slice(&(palette.len() as u16).to_le_bytes());
    for rgb in &palette {
        data.extend_from_slice(rgb);
    }

    data.extend_from_slice(&0u16.to_le_bytes()); // unknown_08
    data.extend_from_slice(&strip.fps.max(1).to_le_bytes());
    // flags bit 0: wrap anim_value (GIF loop). bit 1 would ping-pong.
    let flags: u16 = if strip.looping { 1 } else { 0 };
    data.extend_from_slice(&flags.to_le_bytes());
    data.extend_from_slice(&(width as u16).to_le_bytes());
    data.extend_from_slice(&(height as u16).to_le_bytes());
    data.extend_from_slice(&(strip.frames.len() as u16).to_le_bytes());

    while data.len() % 4 != 0 {
        data.push(0);
    }

    let frame_bytes = (width * height) as u32;
    for (i, _) in strip.frames.iter().enumerate() {
        data.extend_from_slice(&(i as u32 * frame_bytes).to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&(width as u16).to_le_bytes());
        data.extend_from_slice(&(height as u16).to_le_bytes());
    }
    for bm in &bitmaps {
        data.extend_from_slice(bm);
    }
    while data.len() % 4 != 0 {
        data.push(0);
    }
    let total = (data.len() - 4) as u32;
    data[size_at..size_at + 4].copy_from_slice(&total.to_le_bytes());
    Ok(data)
}

/// Index 0 in a `.spr`. Real alpha, near-black, and the opaque backdrops
/// sprite packs use instead of alpha (`128,128,192` and magenta).
pub fn spr_key(r: u8, g: u8, b: u8, a: u8) -> bool {
    a < 16
        || (r as u16) + (g as u16) + (b as u16) < 12
        || (r, g, b) == (128, 128, 192)
        || (r, g, b) == (255, 0, 255)
}

fn nearest_palette(palette: &[[u8; 3]], r: u8, g: u8, b: u8) -> u8 {
    let mut best = i32::MAX;
    let mut idx = 1u8;
    for (i, rgb) in palette.iter().enumerate() {
        let dist = (rgb[0] as i32 - r as i32).abs() * 3
            + (rgb[1] as i32 - g as i32).abs() * 5
            + (rgb[2] as i32 - b as i32).abs() * 2;
        if dist < best {
            best = dist;
            idx = (i + 1) as u8;
        }
    }
    idx
}

#[cfg(test)]
mod tests {
    #[test]
    fn multiply_tints_colour_and_keeps_alpha() {
        let strip = SpriteStrip {
            frames: vec![RgbaFrame {
                width: 2,
                height: 1,
                rgba: vec![255, 255, 255, 255, 200, 100, 0, 7],
            }],
            fps: 10,
            looping: false,
        };
        let t = strip.multiply([90, 90, 240]);
        assert_eq!(t.frames[0].rgba, vec![90, 90, 240, 255, 71, 35, 0, 7]);
        assert_eq!((t.fps, t.looping), (10, false));
    }

    use super::*;

    fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(rgba).unwrap();
        writer.finish().unwrap();
        out
    }

    #[test]
    fn namespaced() {
        assert_eq!(
            sprite_key("example.ice_bazooka", "sprites/icon.png"),
            "example.ice_bazooka:sprites/icon.png"
        );
        assert_eq!(stock_sprite_id("grenade"), Some(50));
        assert_eq!(stock_sprite_id("bazooka"), Some(48));
        assert_eq!(stock_sprite_id("missile"), Some(48));
        assert_eq!(stock_sprite_id("cursor"), Some(32));
        assert_eq!(stock_sprite_id("cursorr"), Some(32));
        assert_eq!(stock_sprite_id("uzihit"), Some(114));
        assert_eq!(stock_sprite_id("cluster"), Some(53));
        assert_eq!(stock_sprite_id("clustlet"), Some(54));
        assert_eq!(stock_sprite_id("pigeon"), Some(175));
        assert_eq!(stock_sprite_name(48), Some("missile"));
        assert_eq!(stock_sprite_id("petrol-1"), stock_sprite_id("petrol_1"));
        assert!(stock_sprite_id("circle").is_none());
        assert!(is_sheet_path("sprites/ball.png"));
        assert!(is_sheet_path("sprites/200w.gif"));
        assert!(is_sheet_path("foo.GIF"));
        assert!(!is_sheet_path("grenade"));
    }

    #[test]
    fn custom_weapon_pose_names_resolve() {
        assert_eq!(stock_worm_sprite_id("weaponlnk"), Some(-1));
        assert_eq!(stock_worm_sprite_id("weaponlnku"), Some(-2));
        assert_eq!(stock_worm_sprite_id("weaponlnkd"), Some(-3));
        assert_eq!(stock_worm_sprite_id("wthrow"), Some(591));
        assert_eq!(stock_worm_sprite_id("wthrowu"), Some(592));
        assert_eq!(stock_worm_sprite_id("wthrowd"), Some(593));
    }

    #[test]
    fn explicit_frames_round_trip_through_stock_phase_mapping() {
        for count in [1, 2, 10, 120, 360, 65536] {
            for index in 0..count {
                let phase = frame_phase(index as i32, count).unwrap();
                assert_eq!((phase as u64 * count as u64) >> 16, index as u64);
            }
            assert_eq!(frame_phase(-1, count), frame_phase(count as i32 - 1, count));
            assert_eq!(frame_phase(count as i32, count), Some(0));
        }
        assert_eq!(frame_phase(0, 0), None);
        assert_eq!(frame_phase(0, 65537), None);
    }

    #[test]
    fn tall_glyph_png_is_one_frame_not_a_strip() {
        let w = 20u32;
        let h = 28u32;
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            rgba[i * 4] = 255;
            rgba[i * 4 + 1] = 220;
            rgba[i * 4 + 2] = 64;
            rgba[i * 4 + 3] = 255;
        }
        let png = encode_rgba_png(w, h, &rgba);
        let strip = load_strip_bytes(&png, 8).expect("glyph");
        assert_eq!(strip.frames.len(), 1);
        assert_eq!(strip.frames[0].width, 20);
        assert_eq!(strip.frames[0].height, 28);
    }

    #[test]
    fn slices_one_row_eight_columns() {
        let fw = 2u32;
        let fh = 2u32;
        let cols = 8u32;
        let mut rgba = vec![0u8; (fw * cols * fh * 4) as usize];
        for col in 0..cols {
            let v = (col * 30) as u8;
            for y in 0..fh {
                for x in 0..fw {
                    let i = ((y * (fw * cols) + col * fw + x) * 4) as usize;
                    rgba[i] = v;
                    rgba[i + 1] = 200;
                    rgba[i + 2] = 40;
                    rgba[i + 3] = 255;
                }
            }
        }
        let png = encode_rgba_png(fw * cols, fh, &rgba);
        let strip = load_strip_bytes(&png, cols).unwrap();
        assert_eq!(strip.frames.len(), 8);
        assert_eq!(strip.frames[0].width, 2);
        assert_eq!(strip.frames[7].rgba[0], 7 * 30);
        assert_eq!(strip.frames[7].rgba[1], 200);
    }

    #[test]
    fn slices_ten_square_rows() {
        let fw = 2u32;
        let rows = 10u32;
        let mut rgba = vec![0u8; (fw * fw * rows * 4) as usize];
        for row in 0..rows {
            let v = (row * 20) as u8;
            for y in 0..fw {
                for x in 0..fw {
                    let i = (((row * fw + y) * fw + x) * 4) as usize;
                    rgba[i] = v;
                    rgba[i + 3] = 255;
                }
            }
        }
        let png = encode_rgba_png(fw, fw * rows, &rgba);
        let strip = load_strip_bytes(&png, 1).unwrap();
        assert_eq!(strip.frames.len(), 10);
        assert_eq!(strip.frames[0].width, 2);
        assert_eq!(strip.frames[0].height, 2);
        assert_eq!(strip.frames[9].rgba[0], 9 * 20);
    }

    #[test]
    fn turret_uzihit_is_ten_frames() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../mods/examples/turret/sprites/uzihit.png");
        let strip = load_strip_file(&path, 1).expect("uzihit.png");
        assert_eq!(strip.frames.len(), 10);
        assert_eq!(strip.frames[0].width, strip.frames[0].height);
        let spr = encode_spr(&strip).expect("uzihit.spr");
        let pal_n = u16::from_le_bytes([spr[10], spr[11]]) as usize;
        let pal_end = 12 + pal_n * 3;
        let header = pal_end;
        let mut meta = header + 12;
        while meta % 4 != 0 {
            meta += 1;
        }
        let bitmap = meta + 12 * 10;
        assert_eq!(spr[bitmap], 0, "pack backdrop must be .spr index 0");
    }

    #[test]
    fn example_ball_strip_is_eight_frames() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/ball-8.png");
        let strip = load_strip_file(&path, 8).expect("eight-frame fixture");
        assert_eq!(strip.frames.len(), 8);
        assert!(strip.frames[0].width > 0);
        assert_eq!(strip.frames[0].height, strip.frames[7].height);
        let spr = encode_spr(&strip).expect("encode ball.spr");
        assert!(spr.len() > 32);
        let flags = u16::from_le_bytes([spr[8], spr[9]]);
        assert_eq!(flags, 0, "uncompressed .spr, no 0x4000 LZSS");
        let frames = u16::from_le_bytes([
            spr[12 + u16::from_le_bytes([spr[10], spr[11]]) as usize * 3 + 10],
            spr[12 + u16::from_le_bytes([spr[10], spr[11]]) as usize * 3 + 11],
        ]);
        assert_eq!(frames, 8);
    }

    #[test]
    fn encode_spr_transparent_is_zero() {
        let strip = SpriteStrip {
            frames: vec![RgbaFrame {
                width: 2,
                height: 1,
                rgba: vec![0, 0, 0, 0, 0, 200, 0, 255],
            }],
            fps: 10,
            looping: false,
        };
        let spr = encode_spr(&strip).unwrap();
        let pal_n = u16::from_le_bytes([spr[10], spr[11]]) as usize;
        assert_eq!(pal_n, 1);
        let pal_end = 12 + pal_n * 3;
        let header = pal_end;
        let w = u16::from_le_bytes([spr[header + 6], spr[header + 7]]);
        let h = u16::from_le_bytes([spr[header + 8], spr[header + 9]]);
        assert_eq!((w, h), (2, 1));
        let mut meta = header + 12;
        while meta % 4 != 0 {
            meta += 1;
        }
        let bitmap = meta + 12;
        assert_eq!(spr[bitmap], 0);
        assert_eq!(spr[bitmap + 1], 1);
        let end_x = u16::from_le_bytes([spr[meta + 8], spr[meta + 9]]);
        let end_y = u16::from_le_bytes([spr[meta + 10], spr[meta + 11]]);
        assert_eq!((end_x, end_y), (2, 1), "GetSpriteFrameForBlit skips empty bbox");
    }

    fn encode_two_frame_gif(w: u16, h: u16, mut a: Vec<u8>, mut b: Vec<u8>, delay: u16) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = gif::Encoder::new(&mut out, w, h, &[]).unwrap();
            encoder.set_repeat(gif::Repeat::Infinite).unwrap();
            let mut fa = gif::Frame::from_rgba(w, h, &mut a);
            fa.delay = delay;
            encoder.write_frame(&fa).unwrap();
            let mut fb = gif::Frame::from_rgba(w, h, &mut b);
            fb.delay = delay;
            encoder.write_frame(&fb).unwrap();
        }
        out
    }

    #[test]
    fn gif_composites_and_loops() {
        let red = vec![255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255];
        let blue = vec![0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255];
        let bytes = encode_two_frame_gif(2, 2, red, blue, 10);
        let strip = load_gif_bytes(&bytes).unwrap();
        assert!(strip.looping);
        assert!(strip.frames.len() >= 2);
        assert_eq!(strip.frames[0].width, 2);
        let spr = encode_spr(&strip).unwrap();
        let pal_n = u16::from_le_bytes([spr[10], spr[11]]) as usize;
        let header = 12 + pal_n * 3;
        let flags = u16::from_le_bytes([spr[header + 4], spr[header + 5]]);
        assert_eq!(flags & 1, 1, "loop wrap");
        assert!(strip.phase_delta() > 0);
    }

    #[test]
    fn feet_platform_is_bottom_opaque_row() {
        let mut rgba = vec![0u8; 4 * 4 * 4];
        // Opaque bar on the last row, columns 1..=2.
        for x in 1..=2 {
            let i = ((3 * 4 + x) * 4) as usize;
            rgba[i] = 200;
            rgba[i + 1] = 10;
            rgba[i + 2] = 10;
            rgba[i + 3] = 255;
        }
        let feet = feet_platform(&[RgbaFrame {
            width: 4,
            height: 4,
            rgba,
        }])
        .expect("feet");
        assert_eq!(feet.y, 3);
        assert_eq!(feet.x0, 1);
        assert_eq!(feet.x1, 2);
    }

    #[test]
    fn opaque_mask_from_first_frame() {
        // One opaque pixel in the middle of a 16x16 frame. The mask is
        // dilated three times (4-neighbourhood), so bits within Manhattan
        // distance 3 are solid and anything further is not.
        let w = 16usize;
        let mut rgba = vec![0u8; w * w * 4];
        let o = (8 * w + 8) * 4;
        rgba[o] = 255;
        rgba[o + 3] = 255;
        let mask = OpaqueMask::from_frame(&RgbaFrame {
            width: w as u32,
            height: w as u32,
            rgba,
        });
        assert!(mask.test(8, 8));
        assert!(mask.test(8, 11));
        assert!(mask.test(5, 8));
        assert!(!mask.test(8, 12));
        assert!(!mask.test(4, 8));
        assert!(!mask.test(0, 0));
        assert!(!mask.test(-1, 0));
    }

    #[test]
    fn example_200w_gif_decodes() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../mods/examples/gif_donkey/sprites/200w.gif");
        let strip = load_gif_file(&path).expect("200w.gif");
        assert!(!strip.frames.is_empty());
        assert!(strip.frames[0].width > 0);
        if strip.frames.len() > 1 {
            assert!(strip.looping);
            assert!(strip.phase_delta() > 0);
        }
        encode_spr(&strip).expect("encode gif.spr");
    }

    #[test]
    fn png_sequence_folder_loads() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../mods/examples/wormcraft/sprites/absorb");
        let strip = load_png_sequence(&dir).expect("absorb frames");
        assert!(strip.frames.len() >= 2);
        assert_eq!(strip.frames[0].width, strip.frames[1].width);
        assert!(strip.looping);
    }
}
