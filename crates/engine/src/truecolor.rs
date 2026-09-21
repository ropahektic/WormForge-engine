//! RGBA companion to WA's indexed renderer. The real framebuffer is never
//! replaced: it remains a complete palette fallback. Scratch stock draws give
//! us an underlay and exact occlusion at each custom sprite's queue depth.
use crate::{
    log_error, log_line,
    render_queue::{Command, Queue},
};
use gfx::{composite, SpriteStrip};
use std::{
    collections::HashMap,
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, Ordering},
        LazyLock, Mutex,
    },
};

const SURFACE: usize = 0x3d9c;
const RENDER_LOCK: usize = 0x3d98;
static SHEETS: LazyLock<Mutex<HashMap<u32, SpriteStrip>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static FRAME: Mutex<Option<Frame>> = Mutex::new(None);
static ANNOUNCED: AtomicBool = AtomicBool::new(false);
static WARNED: AtomicBool = AtomicBool::new(false);
static NEARER_LOGGED: AtomicBool = AtomicBool::new(false);
static REPLAY_LOGGED: AtomicBool = AtomicBool::new(false);
static GRID_LOGGED: AtomicBool = AtomicBool::new(false);
static LAYERS_LOGGED: AtomicBool = AtomicBool::new(false);
/// Set when capture performed the frame's stock drawing itself.
pub static DREW: AtomicBool = AtomicBool::new(false);
static PENDING: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn wf_color_upload(
        device: *mut c_void,
        w: u32,
        h: u32,
        rgba: *const u8,
        underlay: *const u8,
    ) -> i32;
    fn wf_color_bind(device: *mut c_void) -> i32;
    fn wf_color_restore();
    fn wf_color_release();
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Grid {
    vtable: u32,
    external: u32,
    data: u32,
    bpp: u32,
    stride: u32,
    w: u32,
    h: u32,
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}
impl Grid {
    unsafe fn read(display: usize) -> Option<(usize, Self)> {
        if display == 0 {
            return None;
        }
        let address = *((display + SURFACE) as *const u32) as usize;
        if address == 0 {
            return None;
        }
        let g = *(address as *const Grid);
        if g.data == 0
            || g.bpp != 8
            || g.w == 0
            || g.h == 0
            || g.w > 4096
            || g.h > 4096
            || g.w % 4 != 0
            || g.stride < g.w
            || g.stride > 16384
            || g.right > g.w
            || g.bottom > g.h
            || g.left > g.right
            || g.top > g.bottom
        {
            return None;
        }
        Some((address, g))
    }
    unsafe fn copy_pixels(self, dst: &mut [u8]) {
        for y in 0..self.h as usize {
            let row = std::slice::from_raw_parts(
                (self.data as usize + y * self.stride as usize) as *const u8,
                self.w as usize,
            );
            dst[y * self.w as usize..(y + 1) * self.w as usize].copy_from_slice(row);
        }
    }
}
struct Frame {
    display: usize,
    surface: usize,
    width: usize,
    height: usize,
    rgba: Vec<u8>,
    underlay: Vec<u8>,
    baseline: Vec<u8>,
    a: Vec<u8>,
    b: Vec<u8>,
    valid: bool,
}
impl Frame {
    fn new(display: usize, surface: usize, g: Grid) -> Self {
        let n = g.w as usize * g.h as usize;
        Self {
            display,
            surface,
            width: g.w as usize,
            height: g.h as usize,
            rgba: vec![0; n * 4],
            underlay: vec![0; n],
            baseline: vec![0; n],
            a: vec![0; n],
            b: vec![0; n],
            valid: false,
        }
    }
    fn matches(&self, display: usize, surface: usize, g: Grid) -> bool {
        self.display == display
            && self.surface == surface
            && self.width == g.w as usize
            && self.height == g.h as usize
    }
    /// Direct UI draws after the queue must not be painted over by our shader.
    unsafe fn reconcile(&mut self, g: Grid) {
        for y in 0..self.height {
            let row = std::slice::from_raw_parts(
                (g.data as usize + y * g.stride as usize) as *const u8,
                self.width,
            );
            for (x, &value) in row.iter().enumerate() {
                let i = y * self.width + x;
                if value != self.baseline[i] {
                    self.rgba[i * 4..i * 4 + 4].fill(0);
                    self.underlay[i] = value;
                }
            }
        }
    }
}

pub fn register(id: u32, strip: SpriteStrip) {
    if let Ok(mut g) = SHEETS.lock() {
        g.insert(id, strip);
    }
}
pub fn clear() {
    if let Ok(mut g) = SHEETS.lock() {
        g.clear();
    }
    if let Ok(mut f) = FRAME.lock() {
        *f = None;
    }
    PENDING.store(false, Ordering::SeqCst);
    ANNOUNCED.store(false, Ordering::SeqCst);
    WARNED.store(false, Ordering::SeqCst);
}
pub fn begin_frame() {
    PENDING.store(false, Ordering::SeqCst);
    if let Ok(mut f) = FRAME.lock() {
        if let Some(f) = f.as_mut() {
            f.valid = false;
        }
    }
}
fn warn(reason: &str) {
    if !WARNED.swap(true, Ordering::SeqCst) {
        log_error(&format!("truecolor fallback: {reason}"));
    }
}

/// Restore all target fields even if capture unwinds. The render lock stops WA
/// from reacquiring its real surface on the first scratch draw.
unsafe fn raster(
    q: &Queue,
    commands: &[Command],
    display: usize,
    clip: usize,
    surface: usize,
    mut grid: Grid,
    pixels: &mut [u8],
) {
    struct Restore {
        display: usize,
        surface: usize,
        grid: Grid,
        lock: u32,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                *(self.surface as *mut Grid) = self.grid;
                *((self.display + SURFACE) as *mut u32) = self.surface as u32;
                *((self.display + RENDER_LOCK) as *mut u32) = self.lock;
            }
        }
    }
    let _restore = Restore {
        display,
        surface,
        grid,
        lock: *((display + RENDER_LOCK) as *const u32),
    };
    grid.data = pixels.as_mut_ptr() as u32;
    grid.stride = grid.w;
    *(surface as *mut Grid) = grid;
    // The queue draws through display->vtable[19], which resolves its target
    // from a layer binding -- but not necessarily layer 0. wkFOW's precedent
    // redirects 0x3d9c for a direct blit of a HUD sprite; world draws may use
    // another layer, which would explain a replay that holds its binding and
    // still writes nothing. Redirect every bound layer at once: they all feed
    // the same scratch, and coverage does not care which one drew.
    let mut others: [(usize, Grid); 3] = [(0, grid); 3];
    for (n, slot) in (1..4).enumerate() {
        let addr = *((display + SURFACE + slot * 4) as *const u32) as usize;
        if addr == 0 || addr == surface {
            continue;
        }
        let mut g = *(addr as *const Grid);
        if g.data == 0 || g.w != grid.w || g.h != grid.h {
            continue;
        }
        others[n] = (addr, g);
        g.data = pixels.as_mut_ptr() as u32;
        g.stride = g.w;
        *(addr as *mut Grid) = g;
    }
    if !LAYERS_LOGGED.swap(true, Ordering::SeqCst) {
        #[cfg(debug_assertions)]
        log_line(&format!(
            "truecolor layers: {:#x} {:#x} {:#x} {:#x}",
            *((display + SURFACE) as *const u32),
            *((display + SURFACE + 4) as *const u32),
            *((display + SURFACE + 8) as *const u32),
            *((display + SURFACE + 12) as *const u32),
        ));
    }
    // Held at 1 so DisplayGfx cannot rebind layer_0 while the temporary target
    // is installed -- the dispatcher draws through display->vtable[19]
    // (blit_sprite), which resolves its target from that binding.
    *((display + RENDER_LOCK) as *mut u32) = 1;
    q.draw(commands, display, clip);
    // Did the binding survive the replay, and did the target stay ours? This
    // separates "the draws were skipped" from "the draws went somewhere else".
    for (addr, g) in others.iter() {
        if *addr != 0 {
            *(*addr as *mut Grid) = *g;
        }
    }
    if !GRID_LOGGED.swap(true, Ordering::SeqCst) {
        #[cfg(debug_assertions)]
        log_line(&format!(
            "truecolor raster: bound {:#x} (expected {:#x}), data {:#x} (scratch {:#x}), stride {} w {} h {}",
            *((display + SURFACE) as *const u32) as usize,
            surface,
            *(surface as *const u32).byte_add(8),
            pixels.as_ptr() as u32,
            (*(surface as *const Grid)).stride,
            (*(surface as *const Grid)).w,
            (*(surface as *const Grid)).h
        ));
    }
}

fn supported(c: Command, sheets: &HashMap<u32, SpriteStrip>) -> bool {
    if c.kind != 4 && c.kind != 5 {
        return false;
    }
    let op = unsafe { c.word(4) };
    op & !0x0004_ffff == 0 && sheets.contains_key(&(op & 0xffff))
}

pub unsafe fn capture(q: &Queue, display: usize, clip: usize) {
    if !crate::shader::is_ready() || clip == 0 {
        return;
    }
    let Ok(sheets) = SHEETS.lock() else {
        return;
    };
    if sheets.is_empty() {
        return;
    }
    let Ok(mut state) = FRAME.lock() else {
        return;
    };
    let has_sprite = q.commands.iter().any(|c| supported(*c, &sheets));
    if !has_sprite && !state.as_ref().is_some_and(|f| f.valid) {
        return;
    }
    // A callback can mutate state. Never replay arbitrary code for coverage.
    if q.commands.iter().any(|c| c.kind == 3 || c.kind > 14) {
        if let Some(f) = state.as_mut() {
            f.valid = false;
        }
        warn("queue contains a callback or unknown command");
        return;
    }
    let Some((surface, grid)) = Grid::read(display) else {
        warn("render surface is not a supported 8-bit target");
        return;
    };
    if !state
        .as_ref()
        .is_some_and(|f| f.matches(display, surface, grid))
    {
        *state = Some(Frame::new(display, surface, grid));
    }
    let f = state.as_mut().unwrap();
    if f.valid {
        f.reconcile(grid);
    } else {
        f.rgba.fill(0);
        grid.copy_pixels(&mut f.underlay);
    }

    let ordered = q.ordered();
    let first = ordered.iter().position(|c| supported(*c, &sheets));
    let last = ordered.iter().rposition(|c| supported(*c, &sheets));
    let (Some(first), Some(last)) = (first, last) else {
        // Nothing of ours in this queue: leave the stock draw to the caller.
        f.valid = false;
        return;
    };

    // Composite our sprites' RGBA. They share one depth, so they sit together
    // in the ordered queue and the split below is a clean three-way cut.
    for c in ordered[first..=last]
        .iter()
        .filter(|c| supported(**c, &sheets))
    {
        let op = c.word(4);
        let sheet = &sheets[&(op & 0xffff)];
        let Some(index) = composite::frame_index(c.word(5), sheet.frames.len(), sheet.looping)
        else {
            continue;
        };
        let mut x = (c.word(2) as i32) >> 16;
        let mut y = (c.word(3) as i32) >> 16;
        if c.kind == 5 {
            x = x.saturating_sub(*(clip as *const i32) >> 16);
            y = y.saturating_sub(*((clip + 4) as *const i32) >> 16);
        }
        x = x.saturating_add(*((display + 0x3560) as *const i32));
        y = y.saturating_add(*((display + 0x3564) as *const i32));
        composite::over(
            &mut f.rgba,
            f.width,
            f.height,
            [
                grid.left as usize,
                grid.top as usize,
                grid.right as usize,
                grid.bottom as usize,
            ],
            &sheet.frames[index],
            [x, y],
            op & 0x40000 != 0,
        );
    }

    // Coverage comes from the REAL dispatch, in three parts, rather than from
    // replaying commands into a redirected scratch surface. That replay held
    // its binding perfectly -- right pointer, right data, right stride -- and
    // still wrote 0 of 1,024,000 pixels, through five different hypotheses.
    // These draws demonstrably work: they are the ones that put the frame on
    // screen. Every command kind is handled, including the kind 1 bitmaps that
    // draw the weapon panel.
    q.draw(&ordered[..first], display, clip);
    grid.copy_pixels(&mut f.underlay); // scene without our sprites
    q.draw(&ordered[first..=last], display, clip);
    grid.copy_pixels(&mut f.a); // ... with them
    q.draw(&ordered[last + 1..], display, clip);
    grid.copy_pixels(&mut f.b); // ... and everything nearer

    // Anything a nearer draw changed is in front of us: drop our colour there.
    for i in 0..f.a.len() {
        if f.a[i] != f.b[i] {
            f.rgba[i * 4..i * 4 + 4].fill(0);
        }
    }
    DREW.store(true, Ordering::SeqCst);
    f.valid = true;
}

pub unsafe fn after_stock(display: usize) {
    if let Ok(mut state) = FRAME.lock() {
        if let Some(f) = state.as_mut().filter(|f| f.valid) {
            if let Some((surface, grid)) = Grid::read(display) {
                if f.matches(display, surface, grid) {
                    grid.copy_pixels(&mut f.baseline);
                } else {
                    f.valid = false;
                }
            } else {
                f.valid = false;
            }
        }
    }
}

/// Runs before shader present, consumes this frame once, and never removes the
/// complete stock framebuffer on upload failure or unsupported renderer paths.
pub unsafe fn present(device: *mut c_void) {
    PENDING.store(false, Ordering::SeqCst);
    let Ok(mut state) = FRAME.lock() else {
        return;
    };
    let Some(f) = state.as_mut().filter(|f| f.valid) else {
        return;
    };
    f.valid = false;
    let Some((surface, grid)) = Grid::read(f.display) else {
        return;
    };
    if !f.matches(f.display, surface, grid) {
        return;
    }
    f.reconcile(grid);
    if !f.rgba.chunks_exact(4).any(|p| p[3] != 0) {
        return;
    }
    if wf_color_upload(
        device,
        f.width as u32,
        f.height as u32,
        f.rgba.as_ptr(),
        f.underlay.as_ptr(),
    ) != 0
    {
        PENDING.store(true, Ordering::SeqCst);
    } else {
        warn("D3D9 texture upload failed; stock sprites retained");
    }
}
pub unsafe fn bind(device: *mut c_void) {
    if PENDING.swap(false, Ordering::SeqCst) {
        if wf_color_bind(device) != 0 {
            if !ANNOUNCED.swap(true, Ordering::SeqCst) {
                log_line(
                    "truecolor active: RGBA sprites with stock queue occlusion; palette unchanged",
                );
            }
        } else {
            warn("D3D9 shader bind failed; stock sprites retained");
        }
    }
}
pub unsafe fn restore() {
    wf_color_restore();
}
pub unsafe fn release() {
    begin_frame();
    wf_color_release();
}
