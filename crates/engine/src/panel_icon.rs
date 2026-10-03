//! Custom weapon icons in the stock weapon panel (`panel_icon = "x.png"`).
//!
//! WA keeps two icon images per weapon id at `GameWorld+0x144`, loaded from
//! `lo\<name>.img` (`+0x144 + 8*id`, the greyed frame) and `hi\<name>.img`
//! (`+0x148 + 8*id`) by `GameWorld__LoadHudAndWeaponSprites` (0x53D0E0). Both
//! are 8bpp BitGrids decoded against display layer 1's PaletteContext. The
//! panel's tile draw copies a 24x24 window at (4,4) out of one of them with
//! `BitGrid__BlitSpriteRect` (0x4F6910, flags 0: index 0 transparent), taking
//! the `lo` frame while the weapon is disabled.
//!
//! A custom icon is a clone of the replaced slot's two BitGrids -- WA's own
//! 0x2C-byte header, our own pixel buffer -- with the PNG quantized into the
//! window using only colours the stock icons already use (`gfx::icon`). It
//! takes no palette entries and greys out the way its neighbours do.
//!
//! The table owns its images: WA's teardown deletes every slot's pair, so the
//! stock pointers go back before `GameEngine::Shutdown` (`restore`), exactly
//! as for copied stock icons (`apply::restore_panel_icons`). The clones are
//! Rust memory and are never handed to WA's delete.

use crate::log::log_error;
use crate::log_line;
use game::entity::read_i32;
use game::offsets::{
    DISPLAY_LAYER_CONTEXTS, GAME_RUNTIME_WORLD, GAME_SESSION_RUNTIME, GAME_WORLD_DISPLAY,
    GAME_WORLD_PANEL_IMG,
};
use gfx::icon::{self, PaletteColour};
use std::sync::Mutex;

/// The window the panel's tile draw copies (0x567EA2..0x567EC7).
const WINDOW_X: usize = 4;
const WINDOW_Y: usize = 4;
pub const WINDOW_W: u32 = 24;
pub const WINDOW_H: u32 = 24;
/// Stock weapon ids that carry icons.
const WEAPON_IDS: usize = 71;
/// BitGrid header (openwa's byte-asserted layout, `wkFOW/BitGrid.h`).
const BITGRID_BYTES: usize = 0x2C;
const BITGRID_EXTERNAL: usize = 0x04;
const BITGRID_DATA: usize = 0x08;
const BITGRID_STRIDE: usize = 0x10;
const BITGRID_WIDTH: usize = 0x14;
const BITGRID_HEIGHT: usize = 0x18;
/// PaletteContext RGB table (`sprite.rs` `PAL_RGB`): 4 bytes per index.
const PAL_RGB: usize = 0x04;

/// One cloned image: WA's header with our pixels. Boxed so the addresses
/// written into WA's table stay put.
struct Image {
    header: Box<[u32; BITGRID_BYTES / 4]>,
    _pixels: Box<[u8]>,
}

impl Image {
    fn ptr(&self) -> u32 {
        self.header.as_ptr() as u32
    }
}

struct Installed {
    world: usize,
    slot: usize,
    original: [u32; 2],
    clones: [Image; 2],
}

static INSTALLED: Mutex<Vec<Installed>> = Mutex::new(Vec::new());

/// The stock icons' colours and greyed-frame map, learned once per world.
struct Learned {
    world: usize,
    allowed: Vec<PaletteColour>,
    disabled: [u8; 256],
    rgb: [[u8; 3]; 256],
}

static LEARNED: Mutex<Option<Learned>> = Mutex::new(None);

struct Grid {
    data: *const u8,
    stride: usize,
    width: usize,
    height: usize,
}

unsafe fn grid(ptr: u32) -> Option<Grid> {
    if ptr == 0 {
        return None;
    }
    let p = ptr as *const u8;
    let g = Grid {
        data: read_i32(p as *mut u8, BITGRID_DATA) as usize as *const u8,
        stride: read_i32(p as *mut u8, BITGRID_STRIDE) as usize,
        width: read_i32(p as *mut u8, BITGRID_WIDTH) as usize,
        height: read_i32(p as *mut u8, BITGRID_HEIGHT) as usize,
    };
    let fits = g.width >= WINDOW_X + WINDOW_W as usize
        && g.height >= WINDOW_Y + WINDOW_H as usize
        && g.stride >= g.width
        && g.width <= 256
        && g.height <= 256;
    (fits && !g.data.is_null()).then_some(g)
}

/// The window's pixels, row-major.
unsafe fn window(g: &Grid) -> Vec<u8> {
    let mut out = Vec::with_capacity((WINDOW_W * WINDOW_H) as usize);
    for y in 0..WINDOW_H as usize {
        let row = g.data.add((WINDOW_Y + y) * g.stride + WINDOW_X);
        out.extend_from_slice(std::slice::from_raw_parts(row, WINDOW_W as usize));
    }
    out
}

unsafe fn table(world: *mut u8) -> *mut u32 {
    world.add(GAME_WORLD_PANEL_IMG) as *mut u32
}

/// Learn from the stock icons: every colour used inside the window of an
/// enabled (`hi`) frame, and how each index greys out in the `lo` frame.
unsafe fn learn(world: *mut u8) -> Result<Learned, String> {
    let display = read_i32(world, GAME_WORLD_DISPLAY) as usize as *mut u8;
    if display.is_null() {
        return Err("no display".into());
    }
    let ctx = *((display as usize + DISPLAY_LAYER_CONTEXTS + 4) as *const usize) as *const u8;
    if ctx.is_null() {
        return Err("no layer-1 palette context".into());
    }
    let mut rgb = [[0u8; 3]; 256];
    for (i, c) in rgb.iter_mut().enumerate() {
        let p = ctx.add(PAL_RGB + i * 4);
        *c = [*p, *p.add(1), *p.add(2)];
    }
    let t = table(world);
    let mut used = [false; 256];
    let mut pairs = Vec::new();
    for id in 0..WEAPON_IDS {
        let (Some(hi), Some(lo)) = (grid(*t.add(id * 2 + 1)), grid(*t.add(id * 2))) else {
            continue;
        };
        let (h, l) = (window(&hi), window(&lo));
        for &i in &h {
            used[i as usize] = true;
        }
        pairs.push((h, l));
    }
    let allowed: Vec<PaletteColour> =
        (1..256).filter(|&i| used[i]).map(|i| (i as u8, rgb[i])).collect();
    if allowed.is_empty() {
        return Err("no stock icons to learn colours from".into());
    }
    let borrowed: Vec<(&[u8], &[u8])> =
        pairs.iter().map(|(h, l)| (h.as_slice(), l.as_slice())).collect();
    let size = grid(*t.add(1 * 2 + 1)).map(|g| (g.width, g.height)).unwrap_or((0, 0));
    log_line(&format!(
        "panel icons: stock images {}x{}, {} icons, {} colours to quantize to",
        size.0,
        size.1,
        pairs.len(),
        allowed.len()
    ));
    Ok(Learned {
        world: world as usize,
        allowed,
        disabled: icon::learn_disabled_map(&borrowed, &rgb),
        rgb,
    })
}

/// Clone `source`'s header and pixels, then write `tile` into the window.
unsafe fn clone_with(source: u32, g: &Grid, tile: &[u8]) -> Image {
    let mut pixels: Box<[u8]> =
        std::slice::from_raw_parts(g.data, g.stride * g.height).into();
    for y in 0..WINDOW_H as usize {
        let at = (WINDOW_Y + y) * g.stride + WINDOW_X;
        pixels[at..at + WINDOW_W as usize]
            .copy_from_slice(&tile[y * WINDOW_W as usize..(y + 1) * WINDOW_W as usize]);
    }
    let mut header = Box::new([0u32; BITGRID_BYTES / 4]);
    std::ptr::copy_nonoverlapping(
        source as *const u8,
        header.as_mut_ptr() as *mut u8,
        BITGRID_BYTES,
    );
    header[BITGRID_EXTERNAL / 4] = 1;
    header[BITGRID_DATA / 4] = pixels.as_ptr() as u32;
    Image { header, _pixels: pixels }
}

/// Install a pack PNG as `slot`'s icon. Returns how many colours it used.
pub unsafe fn install(
    world: *mut u8,
    slot: usize,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<usize, String> {
    if world.is_null() || slot >= WEAPON_IDS {
        return Err("invalid slot".into());
    }
    let mut learned = LEARNED.lock().unwrap_or_else(|p| p.into_inner());
    if learned.as_ref().map(|l| l.world) != Some(world as usize) {
        *learned = Some(learn(world)?);
    }
    let l = learned.as_ref().unwrap();

    let t = table(world);
    let (lo_ptr, hi_ptr) = (*t.add(slot * 2), *t.add(slot * 2 + 1));
    let (Some(lo), Some(hi)) = (grid(lo_ptr), grid(hi_ptr)) else {
        return Err("the slot has no stock icon to replace".into());
    };
    // A full-size image (a PX icon is WA's whole 32x32 image) is used as is:
    // the panel shows its window. Anything else must fit the window.
    let tile = if (width as usize, height as usize) == (hi.width, hi.height) {
        let window = icon::crop(rgba, width, WINDOW_X as u32, WINDOW_Y as u32, WINDOW_W, WINDOW_H);
        icon::quantize(&window, WINDOW_W, WINDOW_H, &l.allowed, WINDOW_W, WINDOW_H)?
    } else {
        icon::quantize(rgba, width, height, &l.allowed, WINDOW_W, WINDOW_H).map_err(|e| {
            format!("{e} (or be a full {}x{} icon image)", hi.width, hi.height)
        })?
    };
    let greyed = icon::disable(&tile, &l.disabled);
    let colours = {
        let mut seen = [false; 256];
        tile.iter().filter(|&&i| i != 0 && !std::mem::replace(&mut seen[i as usize], true)).count()
    };
    let clones = [clone_with(lo_ptr, &lo, &greyed), clone_with(hi_ptr, &hi, &tile)];
    preview(slot, &tile, &greyed, &l.rgb);

    let mut installed = INSTALLED.lock().unwrap_or_else(|p| p.into_inner());
    // A different world is a new match; its records describe freed memory.
    installed.retain(|r| r.world == world as usize);
    let original = match installed.iter().position(|r| r.slot == slot) {
        // Installed twice on one slot: keep WA's original, drop our old clone.
        Some(i) => installed.remove(i).original,
        None => [lo_ptr, hi_ptr],
    };
    *t.add(slot * 2) = clones[0].ptr();
    *t.add(slot * 2 + 1) = clones[1].ptr();
    installed.push(Installed { world: world as usize, slot, original, clones });
    Ok(colours)
}

/// With a `panel_icons.txt` marker next to `wkLua.log`, write each installed
/// icon's two frames as PNGs (4x, index 0 transparent), so quantization can
/// be judged without opening the panel.
fn preview(slot: usize, hi: &[u8], lo: &[u8], rgb: &[[u8; 3]; 256]) {
    let dir = crate::log::wormforge_dir();
    if !dir.join("panel_icons.txt").is_file() {
        return;
    }
    const SCALE: u32 = 4;
    for (name, tile) in [("hi", hi), ("lo", lo)] {
        let (w, h) = (WINDOW_W * SCALE, WINDOW_H * SCALE);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let i = tile[((y / SCALE) * WINDOW_W + x / SCALE) as usize];
                let c = rgb[i as usize];
                rgba.extend_from_slice(&[c[0], c[1], c[2], if i == 0 { 0 } else { 255 }]);
            }
        }
        let path = dir.join(format!("panel-icon-{slot}-{name}.png"));
        // Encoded in memory, then written through game::overwrite: the
        // previews of the last match are already there, and the user's Wine
        // cannot truncate a file in place (os error 120).
        let mut png_bytes = Vec::new();
        let encoded = {
            let mut enc = png::Encoder::new(&mut png_bytes, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().and_then(|mut wr| wr.write_image_data(&rgba))
        };
        let written = encoded
            .map_err(|e| e.to_string())
            .and_then(|()| game::overwrite::write(&path, &png_bytes).map_err(|e| e.to_string()));
        if let Err(e) = written {
            log_error(&format!("panel icon preview: could not write {}: {e}", path.display()));
        }
    }
}

/// Put WA's own icons back before `GameEngine::Shutdown` deletes the table's
/// images. Only into the live world, and only where a slot still holds our
/// clone; anything else means the table changed under us.
pub fn restore() {
    let mut installed = INSTALLED.lock().unwrap_or_else(|p| p.into_inner());
    let live = unsafe { runtime_world() } as usize;
    for r in installed.drain(..) {
        if r.world != live || live == 0 {
            continue;
        }
        unsafe {
            let t = table(r.world as *mut u8);
            for (frame, clone) in r.clones.iter().enumerate() {
                let at = t.add(r.slot * 2 + frame);
                if *at == clone.ptr() {
                    *at = r.original[frame];
                } else {
                    log_error(&format!(
                        "panel icon slot {} frame {frame} changed after install; left as it is",
                        r.slot
                    ));
                }
            }
        }
    }
    if let Ok(mut l) = LEARNED.lock() {
        *l = None;
    }
}

unsafe fn runtime_world() -> *mut u8 {
    let slot = crate::wa::address("G_GAME_SESSION") as *const *mut u8;
    if slot.is_null() || (*slot).is_null() {
        return std::ptr::null_mut();
    }
    let runtime = *((*slot).add(GAME_SESSION_RUNTIME) as *const *mut u8);
    if runtime.is_null() {
        return std::ptr::null_mut();
    }
    *(runtime.add(GAME_RUNTIME_WORLD) as *const *mut u8)
}
