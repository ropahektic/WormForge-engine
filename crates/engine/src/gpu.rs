//! WA-side integration for the GPU sprite renderer.
//!
//! Everything that needs a device lives in `crates/render`; this module owns
//! the parts that need WA: when a frame starts and ends, which sprites are in
//! it, and which screen pixels a nearer stock draw already claimed.
//!
//! Frame shape, and why it is in these particular places:
//!
//! ```text
//!   MSG_RENDER_SCENE          begin_frame()   draw list cleared
//!   RQ_RenderDrawingQueue     capture()       split the real dispatch,
//!                                             snapshot before/after,
//!                                             mask = what changed
//!   after D3D9ShaderPresent   present()       upload atlas+mask, one draw
//!   D3D9ShaderResourcesCleanup release()      DEFAULT pool gone before Reset
//! ```
//!
//! The mask is the whole reason the split exists. Our quads are drawn after
//! WA's fullscreen composite, so without it they would sit on top of terrain,
//! worms, the weapon panel and the chat box alike. Splitting WA's own dispatch
//! and diffing the framebuffer records "a nearer stock draw touched this
//! pixel" independently of what shape our sprite is — which is what lets
//! rotation and scale come for free rather than breaking occlusion.
//!
//! Measured before building (`docs/gpu-renderer-s0.md`): zero kind-3 callback
//! commands across 15,600 frames of real play, so splitting the dispatch is
//! safe on this content; and a full-surface mask upload at 1280x800 costs
//! 0.30 ms, so there is no need to track dirty rectangles.

use crate::{log_error, log_line, render_queue::Queue};
use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

extern "C" {
    fn wf_gpu_init(device: *mut c_void) -> i32;
    fn wf_gpu_on_device_lost();
    fn wf_gpu_release();
    fn wf_gpu_atlas(w: u32, h: u32, bgra: *const u8) -> i32;
    fn wf_gpu_mask(w: u32, h: u32, coverage: *const u8) -> i32;
    fn wf_gpu_draw(verts: *const c_void, count: u32) -> i32;
}

/// Decided once per match at `on_gfx_ready`, never per frame.
///
/// A per-frame decision cannot work: an actor that skipped its queue command
/// has already committed to being drawn by us, so a mid-frame bail would make
/// it vanish rather than fall back.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Set when `capture` performed this frame's stock drawing itself, so the
/// dispatcher does not draw it a second time.
pub static DREW: AtomicBool = AtomicBool::new(false);
static ANNOUNCED: AtomicBool = AtomicBool::new(false);
static WARNED: AtomicBool = AtomicBool::new(false);

static FRAME: Mutex<Option<Frame>> = Mutex::new(None);

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

fn warn(message: &str) {
    if !WARNED.swap(true, Ordering::SeqCst) {
        log_error(&format!("gpu renderer: {message}; stock rendering retained"));
    }
}

/// Per-frame state, sized to WA's current surface.
struct Frame {
    display: usize,
    surface: usize,
    width: usize,
    height: usize,
    /// 255 where our sprites may paint, 0 where a nearer stock draw won.
    mask: Vec<u8>,
    /// Framebuffer snapshots either side of the cut.
    behind: Vec<u8>,
    front: Vec<u8>,
    /// Draws intercepted this frame, still in WA's coordinates. The screen
    /// transform needs `clip` and `display`, which only exist at dispatch
    /// time, so it is deferred to `capture` rather than done at intercept.
    pending: Vec<Pending>,
    /// Transformed quads, rebuilt each frame from `pending`.
    vertices: Vec<render::Vertex>,
    valid: bool,
}

impl Frame {
    fn new(display: usize, surface: usize, width: usize, height: usize) -> Frame {
        let pixels = width * height;
        Frame {
            display,
            surface,
            width,
            height,
            mask: vec![255; pixels],
            behind: vec![0; pixels],
            front: vec![0; pixels],
            pending: Vec::new(),
            vertices: Vec::new(),
            valid: false,
        }
    }

    fn matches(&self, display: usize, surface: usize, w: usize, h: usize) -> bool {
        self.display == display && self.surface == surface
            && self.width == w && self.height == h
    }
}

/// Enable or disable the GPU path for this match.
pub fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::SeqCst);
    if active && !ANNOUNCED.swap(true, Ordering::SeqCst) {
        log_line("gpu renderer active: sprites drawn as GPU quads");
    }
}

/// Drop everything owned per match.
pub fn clear() {
    ACTIVE.store(false, Ordering::SeqCst);
    DREW.store(false, Ordering::SeqCst);
    WARNED.store(false, Ordering::SeqCst);
    if let Ok(mut frame) = FRAME.lock() {
        *frame = None;
    }
}

/// Start of a rendered frame: nothing from the previous one carries over.
pub fn begin_frame() {
    if let Ok(mut state) = FRAME.lock() {
        if let Some(frame) = state.as_mut() {
            frame.pending.clear();
            frame.vertices.clear();
            frame.valid = false;
        }
    }
}

/// Release D3D resources before WA resets the shared device.
pub fn release() {
    unsafe { wf_gpu_on_device_lost() };
    begin_frame();
}

/// Tear down completely.
pub fn shutdown() {
    unsafe { wf_gpu_release() };
    clear();
}

/// Split WA's dispatch around our sprites' depth and derive the coverage mask.
///
/// `cut_depth` is the render-queue depth our sprites occupy. Commands with a
/// HIGHER depth draw first and are behind us; everything else is in front.
///
/// Returns true when it performed the stock drawing itself.
pub unsafe fn capture(q: &Queue, display: usize, clip: usize, cut_depth: u32) -> bool {
    if !is_active() || clip == 0 {
        return false;
    }
    let Ok(mut state) = FRAME.lock() else {
        return false;
    };
    // Nothing of ours this frame: let the caller do its normal single draw.
    if state.as_ref().is_none_or(|f| f.pending.is_empty()) {
        return false;
    }
    let Some((surface, width, height, read)) = surface_reader(display) else {
        warn("render surface is not a supported 8-bit target");
        return false;
    };
    if !state
        .as_ref()
        .is_some_and(|f| f.matches(display, surface, width, height))
    {
        // Carry this frame's intercepted draws across the reallocation.
        let pending = state.as_mut().map(|f| std::mem::take(&mut f.pending));
        let mut frame = Frame::new(display, surface, width, height);
        if let Some(pending) = pending {
            frame.pending = pending;
        }
        *state = Some(frame);
    }
    let frame = state.as_mut().unwrap();
    resolve(frame, display, clip);
    if frame.vertices.is_empty() {
        // Everything of ours was culled or unresolvable.
        return false;
    }

    let ordered = q.ordered();
    // `ordered` runs back to front, so the first command at or in front of our
    // depth is the cut. Everything before it is behind us.
    let cut = ordered
        .iter()
        .position(|c| c.depth <= cut_depth)
        .unwrap_or(ordered.len());

    q.draw(&ordered[..cut], display, clip);
    read(&mut frame.behind);
    q.draw(&ordered[cut..], display, clip);
    read(&mut frame.front);

    // Any pixel a nearer draw changed belongs to it, not to us.
    for i in 0..frame.mask.len() {
        frame.mask[i] = if frame.behind[i] == frame.front[i] { 255 } else { 0 };
    }
    frame.valid = true;
    DREW.store(true, Ordering::SeqCst);
    true
}

/// Draw the frame's sprites. Called after WA's fullscreen composite and
/// before its Present, where a second scene is legal and survives — which the
/// S0 probe established against WA's real device parameters.
pub unsafe fn present(device: *mut c_void) {
    if !is_active() || device.is_null() {
        return;
    }
    let Ok(mut state) = FRAME.lock() else {
        return;
    };
    let Some(frame) = state.as_mut().filter(|f| f.valid && !f.vertices.is_empty()) else {
        return;
    };
    // Consume once: a stale frame must never be drawn twice.
    frame.valid = false;

    if wf_gpu_init(device) == 0 {
        warn("device init failed");
        return;
    }
    if wf_gpu_mask(frame.width as u32, frame.height as u32, frame.mask.as_ptr()) == 0 {
        warn("coverage mask upload failed");
        return;
    }
    let count = frame.vertices.len() as u32;
    if wf_gpu_draw(frame.vertices.as_ptr() as *const c_void, count) == 0 {
        warn("sprite draw failed");
    }
}

/// Upload the packed atlas. Once per match, not per frame: it is
/// `D3DPOOL_MANAGED` and survives a device reset untouched.
pub fn upload_atlas(atlas: &render::Atlas) -> bool {
    unsafe { wf_gpu_atlas(atlas.width, atlas.height, atlas.pixels.as_ptr()) != 0 }
}

/// One intercepted draw, in WA's own coordinates.
#[derive(Clone, Copy)]
struct Pending {
    /// 16.16 fixed, exactly as `RQ_DrawSpriteLocal` would have stored it.
    x: i32,
    y: i32,
    /// Low 16 bits are the sprite id; 0x40000 is WA's horizontal mirror flag.
    sprite: u32,
    anim: u32,
    /// Presentation state, read from the actor at intercept time. Kept per
    /// draw rather than looked up later because the actor may be freed
    /// between here and the dispatch.
    angle: f32,
    scale: f32,
    tint: u32,
}

/// Take over one sprite draw, or decline it.
///
/// Returns true when this draw is ours, in which case the caller must NOT
/// enqueue it: WA would otherwise draw a palette copy underneath our quad.
pub fn intercept(
    _layer: u32,
    x: i32,
    y: i32,
    sprite: u32,
    anim: u32,
    actor: *mut u8,
) -> bool {
    if !is_active() {
        return false;
    }
    let id = sprite & 0xffff;
    if !crate::sprite::is_gpu_sheet(id) {
        return false;
    }
    let Ok(mut state) = FRAME.lock() else {
        return false;
    };
    let Some(frame) = state.as_mut() else {
        // Before the first capture the surface size is unknown. Declining
        // here lets WA draw it normally for that one frame, which is far
        // better than dropping the sprite.
        return false;
    };
    // Read presentation state now: the actor can be freed before dispatch.
    let (angle, scale, tint) = if actor.is_null() {
        (0.0, 1.0, 0xFFFF_FFFF)
    } else {
        unsafe {
            let fixed = |v: i32| v as f32 / 65536.0;
            (
                fixed(game::entity::read_i32(actor, game::offsets::ACTOR_ANGLE)),
                fixed(game::entity::read_i32(actor, game::offsets::ACTOR_SCALE)),
                game::entity::read_i32(actor, game::offsets::ACTOR_TINT) as u32,
            )
        }
    };
    frame.pending.push(Pending { x, y, sprite, anim, angle, scale, tint });
    true
}

/// Resolve pending draws to screen-space quads.
///
/// The transform mirrors what WA's own dispatcher does to a kind-5 command:
/// drop the 16.16 fraction, subtract the clip origin, then add the camera.
/// Reusing that path rather than deriving screen coordinates independently is
/// what makes the sprites follow wkZoom and camera motion for free.
unsafe fn resolve(frame: &mut Frame, display: usize, clip: usize) {
    frame.vertices.clear();
    let (w, h) = (frame.width as u32, frame.height as u32);
    let clip_x = *(clip as *const i32) >> 16;
    let clip_y = *((clip + 4) as *const i32) >> 16;
    let cam_x = *((display + 0x3560) as *const i32);
    let cam_y = *((display + 0x3564) as *const i32);

    for p in &frame.pending {
        let id = p.sprite & 0xffff;
        let Some((rect, frames, looping)) = crate::sprite::gpu_frame(id) else {
            continue;
        };
        let Some(index) = gfx::composite::frame_index(p.anim, frames, looping) else {
            continue;
        };
        let Some(rect) = rect(index) else { continue };

        let x = ((p.x >> 16).saturating_sub(clip_x)).saturating_add(cam_x);
        let y = ((p.y >> 16).saturating_sub(clip_y)).saturating_add(cam_y);

        let sprite = render::Sprite {
            x: x as f32,
            y: y as f32,
            rect,
            angle: p.angle,
            scale: p.scale,
            tint: render::Tint(p.tint),
            mirror_x: p.sprite & 0x40000 != 0,
        };
        if !sprite.offscreen(w, h) {
            sprite.emit(w, h, &mut frame.vertices);
        }
    }
}

/// Read WA's 8bpp surface: its address, size, and a row-copy closure.
///
/// Mirrors `truecolor::Grid` rather than sharing it, because the two modules
/// are meant to be separable — one of them is going away once the other
/// proves itself.
unsafe fn surface_reader(
    display: usize,
) -> Option<(usize, usize, usize, impl Fn(&mut [u8]))> {
    const SURFACE: usize = 0x3d9c;
    if display == 0 {
        return None;
    }
    let address = *((display + SURFACE) as *const u32) as usize;
    if address == 0 {
        return None;
    }
    let data = *((address + 8) as *const u32) as usize;
    let bpp = *((address + 12) as *const u32);
    let stride = *((address + 16) as *const u32) as usize;
    let w = *((address + 20) as *const u32) as usize;
    let h = *((address + 24) as *const u32) as usize;
    if data == 0 || bpp != 8 || w == 0 || h == 0 || w > 4096 || h > 4096
        || w % 4 != 0 || stride < w || stride > 16384
    {
        return None;
    }
    let read = move |dst: &mut [u8]| {
        if dst.len() < w * h {
            return;
        }
        for y in 0..h {
            let row = std::slice::from_raw_parts((data + y * stride) as *const u8, w);
            dst[y * w..(y + 1) * w].copy_from_slice(row);
        }
    };
    Some((address, w, h, read))
}
