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
use minhook::MinHook;
use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, AtomicPtr, Ordering},
        Mutex,
    },
};

extern "C" {
    fn wf_gpu_init(device: *mut c_void) -> i32;
    fn wf_gpu_on_device_lost();
    // wf_gpu_release() is deliberately not declared here: WA never unloads
    // wkLua, so there is no point at which it could run. It exists in the
    // C++ for the S1 probe, which does tear its device down.
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
static RESOLVED_IDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static NO_SHADER_LOGGED: AtomicBool = AtomicBool::new(false);
static MASK_LOGGED: AtomicBool = AtomicBool::new(false);
#[cfg(debug_assertions)]
static DEPTHS_LOGGED: AtomicBool = AtomicBool::new(false);
static HIDDEN_LOGGED: Mutex<Vec<(i32, i32)>> = Mutex::new(Vec::new());
static WARNED: AtomicBool = AtomicBool::new(false);
/// WA's renderer setting is logged once per match.
static RENDERER_LOGGED: AtomicBool = AtomicBool::new(false);

static FRAME: Mutex<Option<Frame>> = Mutex::new(None);
/// Draws intercepted this frame.
///
/// Deliberately independent of `FRAME`: the frame is sized from WA's surface,
/// which is only readable at dispatch, while intercepts happen earlier during
/// entity render. Hanging the draw list off the frame made the two circular --
/// nothing could be intercepted until a frame existed, and no frame was built
/// until something had been intercepted -- so every sprite fell through to WA.
static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());
/// The packed atlas, held until a device exists to upload it to.
///
/// Sheets are registered at `on_gfx_ready`, which runs at LoadSprites — long
/// before WA has created its D3D9 device. Uploading there always fails, so the
/// pixels wait here and go up on the first present instead.
static ATLAS: Mutex<Option<render::Atlas>> = Mutex::new(None);
static ATLAS_UPLOADED: AtomicBool = AtomicBool::new(false);

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

fn warn(message: &str) {
    if !WARNED.swap(true, Ordering::SeqCst) {
        log_error(&format!(
            "gpu renderer: {message}; stock rendering retained"
        ));
    }
}

/// Per-frame state, sized to WA's current surface.
struct Frame {
    display: usize,
    surface: usize,
    width: usize,
    height: usize,
    /// 255 where our sprites may paint, 0 where something that should occlude
    /// them won. Two bands: `mask` is occluded by terrain and the HUD, and
    /// `mask_front` only by the HUD. Neither is occluded by worms.
    mask: Vec<u8>,
    mask_front: Vec<u8>,
    /// Framebuffer either side of the HUD, the one occluder that IS its own
    /// draw. Terrain is decided from the collision mask instead.
    front: Vec<u8>,
    hud: Vec<u8>,
    /// 255 where the HUD won the pixel this frame, 0 elsewhere. Derived from
    /// the same `hud != front` diff as `mask`, but kept as its own plane so
    /// the fog pass can exempt the HUD from the multiply: the fog visibility
    /// mask is world-space and does not know the HUD is on top of it.
    hud_region: Vec<u8>,
    /// Transformed quads, rebuilt each frame from `PENDING`, split by which
    /// mask they use. Two draw calls rather than one, which the measured
    /// cost (0.25 ms for a full-surface mask upload at 1280x800) makes a
    /// non-issue.
    vertices: Vec<render::Vertex>,
    vertices_front: Vec<render::Vertex>,
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
            mask_front: vec![255; pixels],
            front: vec![0; pixels],
            hud: vec![0; pixels],
            hud_region: vec![0; pixels],
            vertices: Vec::new(),
            vertices_front: Vec::new(),
            valid: false,
        }
    }

    fn matches(&self, display: usize, surface: usize, w: usize, h: usize) -> bool {
        self.display == display && self.surface == surface && self.width == w && self.height == h
    }
}

/// Enable or disable the GPU path for this match.
pub fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::SeqCst);
    if active && !ANNOUNCED.swap(true, Ordering::SeqCst) {
        log_line("gpu renderer active: sprites drawn as GPU quads");
    }
    if active && !RENDERER_LOGGED.swap(true, Ordering::SeqCst) {
        log_line(&crate::shader::renderer_report());
    }
}

/// Drop everything owned per match.
pub fn clear() {
    ACTIVE.store(false, Ordering::SeqCst);
    ATLAS_UPLOADED.store(false, Ordering::SeqCst);
    if let Ok(mut held) = ATLAS.lock() {
        *held = None;
    }
    DREW.store(false, Ordering::SeqCst);
    WARNED.store(false, Ordering::SeqCst);
    RENDERER_LOGGED.store(false, Ordering::SeqCst);
    if let Ok(mut seen) = RESOLVED_IDS.lock() {
        seen.clear();
    }
    NO_SHADER_LOGGED.store(false, Ordering::SeqCst);
    MASK_LOGGED.store(false, Ordering::SeqCst);
    if let Ok(mut seen) = HIDDEN_LOGGED.lock() {
        seen.clear();
    }
    if let Ok(mut pending) = PENDING.lock() {
        pending.clear();
    }
    if let Ok(mut frame) = FRAME.lock() {
        *frame = None;
    }
}

/// Start of a rendered frame: nothing from the previous one carries over.
pub fn begin_frame() {
    if let Ok(mut pending) = PENDING.lock() {
        pending.clear();
    }
    if let Ok(mut state) = FRAME.lock() {
        if let Some(frame) = state.as_mut() {
            frame.vertices.clear();
            frame.vertices_front.clear();
            frame.valid = false;
        }
    }
}

/// Release D3D resources before WA resets the shared device.
pub fn release() {
    unsafe { wf_gpu_on_device_lost() };
    begin_frame();
}

/// Draw the stock frame ourselves, in parts, and derive the coverage mask.
///
/// Occlusion comes from two different sources because the two occluders are
/// different in kind: the HUD is its own draw, so a framebuffer diff around
/// it is exact; terrain shares one `tiled_terrain` command with the soil, so
/// no split can separate them and the collision mask decides instead.
///
/// Returns true when it performed the stock drawing itself.
pub unsafe fn capture(q: &Queue, display: usize, clip: usize) -> bool {
    // Fog needs the HUD region even when the sprite path is inactive (a mod
    // with fog but no GPU sheets), so the split runs for fog alone. Every
    // sprite-specific step below is already gated on there being vertices.
    if !is_active() || clip == 0 {
        return false;
    }
    // Without the D3D9 shader renderer there is nowhere to draw, and a GPU
    // sheet has no palette version to fall back to -- so the sprites are
    // simply absent. That is the worst failure this design can have, because
    // it looks like a pack bug rather than a missing capability, so say it
    // loudly and exactly once. Happens on the software renderer, or when
    // another module already owns the three D3D9 hook sites.
    if !crate::shader::is_ready() {
        if !NO_SHADER_LOGGED.swap(true, Ordering::SeqCst) {
            let cause = match crate::shader::renderer_setting() {
                Some(v) if v != crate::shader::RENDERER_D3D9_SHADER => format!(
                    "WA is set to {} (Renderer {v}); GPU sprites need Direct3D 9 \
                     (shader): choose it in WA's Options > Advanced and restart WA",
                    crate::shader::renderer_name(v)
                ),
                _ => "WA is set to Direct3D 9 (shader) but never presented through \
                      it: another module owns the D3D9 hook sites (e.g. wkFOW), or \
                      the device fell back"
                    .to_string(),
            };
            log_error(&format!(
                "gpu renderer: WA's D3D9 shader renderer is unavailable, so GPU \
                 sprites cannot be drawn and packs using gpu_prefix will be \
                 invisible. Not a pack fault: {cause}."
            ));
        }
        return false;
    }
    let Ok(mut state) = FRAME.lock() else {
        return false;
    };
    // Nothing of ours this frame: let the caller do its normal single draw.
    // Fog changes that: the HUD exemption it needs comes from the same
    // dispatch split below, so when fog is active we run the split even with
    // no sprites of our own, purely to learn where the HUD landed.
    let queued = PENDING.lock().map(|p| p.len()).unwrap_or(0);
    if queued == 0 {
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
        *state = Some(Frame::new(display, surface, width, height));
    }
    let frame = state.as_mut().unwrap();
    resolve(frame, display, clip);
    let no_sprites = frame.vertices.is_empty() && frame.vertices_front.is_empty();
    if no_sprites {
        // Everything of ours was culled or unresolvable.
        return false;
    }

    // Report the scene's actual depth bands once. Choosing where a sprite
    // belongs is otherwise guesswork against half-remembered constants, and
    // the values differ per map and per renderer.
    #[cfg(debug_assertions)]
    if !DEPTHS_LOGGED.swap(true, Ordering::SeqCst) {
        // Kind as well as depth: "which layer is the soil" cannot be answered
        // from depth alone, and guessing it wrong puts the whole mask on the
        // wrong side of a layer.
        let mut seen: Vec<(u32, u32, u32)> = Vec::new();
        for c in &q.commands {
            match seen
                .iter_mut()
                .find(|(d, k, _)| *d == c.depth && *k == c.kind)
            {
                Some((_, _, n)) => *n += 1,
                None => seen.push((c.depth, c.kind, 1)),
            }
        }
        seen.sort_by(|a, b| b.0.cmp(&a.0));
        let name = |k: u32| match k {
            0 => "fill_rect",
            1 => "bitmap",
            2 => "textbox",
            4 => "sprite_g",
            5 => "sprite_l",
            6 => "sprite_o",
            13 => "tiled_bitmap",
            14 => "TERRAIN",
            _ => "?",
        };
        let list: Vec<String> = seen
            .iter()
            .map(|(d, k, n)| format!("{d:#x}:{}x{n}", name(*k)))
            .collect();
        crate::log_line(&format!("gpu depths: back to front {}", list.join(" ")));
    }

    let ordered = q.ordered();

    // Per-source occlusion rather than a single painter-order cut.
    //
    // WA draws terrain (0x180000) BEHIND worms (0xd0xxx), so no single depth
    // can put the sentry in front of worms and behind terrain at once: that
    // needs 0x180000 < d < 0xd0000, an empty interval. A painter's algorithm
    // simply cannot express it.
    //
    // The mask can, because it is built from framebuffer differences and so
    // chooses per PIXEL what occludes us, by source rather than by order:
    //
    //   a  after background and soil      (never occlude us)
    //   b  after terrain                  -> terrain painted where b != a
    //   c  after objects, worms, effects  -> terrain still visible where c == b
    //   d  after the HUD                  -> HUD painted where d != c
    //
    // A worm walking past changes the pixel between b and c, so it fails the
    // `c == b` test and does not mask us -- the sentry draws over it. Terrain
    // that nothing covered keeps its pixel and does mask us.
    //
    // Terrain is found by command kind (14 = tiled_terrain), not by a depth
    // constant: the kind is structural, the depth is per map.
    const HUD_DEPTH_MAX: u32 = 0x100;

    let hud_start = ordered
        .iter()
        .position(|c| c.depth <= HUD_DEPTH_MAX)
        .unwrap_or(ordered.len());

    q.draw(&ordered[..hud_start], display, clip);
    read(&mut frame.front);
    q.draw(&ordered[hud_start..], display, clip);
    read(&mut frame.hud);

    // The HUD is a real separate draw, so a framebuffer diff identifies it
    // exactly. Terrain is not: it and the soil arrive as one tiled_terrain
    // command, so the collision mask decides that instead -- see
    // world::solid_sampler.
    let mut covered = 0usize;
    for i in 0..frame.mask.len() {
        let hud_here = frame.hud[i] != frame.front[i];
        let open = if hud_here { 0 } else { 255 };
        frame.mask[i] = open;
        frame.mask_front[i] = open;
        // The fog pass exempts these same pixels from its multiply.
        frame.hud_region[i] = if hud_here { 255 } else { 0 };
        if hud_here {
            covered += 1;
        }
    }

    // Solid ground occludes the back band, sampled only inside the pixels our
    // sprites actually cover. A full-surface sweep would be a million lookups
    // a frame to answer a question about a few thousand pixels.
    let mut solid_px = 0usize;
    if let Some(solid) = crate::world::solid_sampler() {
        let clip_x = *(clip as *const i32) >> 16;
        let clip_y = *((clip + 4) as *const i32) >> 16;
        let cam_x = *((display + 0x3560) as *const i32);
        let cam_y = *((display + 0x3564) as *const i32);
        let w = frame.width as i64;
        let h = frame.height as i64;
        for chunk in frame.vertices.chunks(render::VERTICES_PER_QUAD) {
            let (mut x0, mut y0) = (i64::MAX, i64::MAX);
            let (mut x1, mut y1) = (i64::MIN, i64::MIN);
            for v in chunk {
                x0 = x0.min(v.x as i64);
                y0 = y0.min(v.y as i64);
                x1 = x1.max(v.x as i64);
                y1 = y1.max(v.y as i64);
            }
            for y in y0.max(0)..=y1.min(h - 1) {
                for x in x0.max(0)..=x1.min(w - 1) {
                    // screen = world - clip + cam, so invert to sample.
                    let wx = (x + clip_x as i64 - cam_x as i64) as i32;
                    let wy = (y + clip_y as i64 - cam_y as i64) as i32;
                    if solid(wx, wy) {
                        let i = (y * w + x) as usize;
                        if frame.mask[i] != 0 {
                            frame.mask[i] = 0;
                            solid_px += 1;
                        }
                    }
                }
            }
        }
    }

    if !MASK_LOGGED.swap(true, Ordering::SeqCst) {
        crate::log_line(&format!(
            "gpu mask: hud claims {covered} px, solid ground {solid_px} px \
             within sprite bounds; hud cut at {hud_start} of {} commands",
            ordered.len()
        ));
    }
    report_hidden_sprites(frame);
    frame.valid = true;
    DREW.store(true, Ordering::SeqCst);
    true
}

/// Say so when a sprite is drawn but almost entirely masked away.
///
/// Once per sprite id per match. The threshold is deliberately high: partial
/// occlusion is normal and expected -- the turret is meant to sit behind
/// terrain -- but a sprite losing essentially all of itself is the signature
/// of an occlusion bug rather than of scenery.
fn report_hidden_sprites(frame: &Frame) {
    const HIDDEN: f64 = 0.98;
    let w = frame.width as i64;
    let h = frame.height as i64;
    for chunk in frame.vertices.chunks(render::VERTICES_PER_QUAD) {
        let (mut x0, mut y0) = (i64::MAX, i64::MAX);
        let (mut x1, mut y1) = (i64::MIN, i64::MIN);
        for v in chunk {
            x0 = x0.min(v.x as i64);
            y0 = y0.min(v.y as i64);
            x1 = x1.max(v.x as i64);
            y1 = y1.max(v.y as i64);
        }
        let (cx0, cy0) = (x0.max(0), y0.max(0));
        let (cx1, cy1) = (x1.min(w - 1), y1.min(h - 1));
        if cx1 < cx0 || cy1 < cy0 {
            continue;
        }
        let mut seen = 0usize;
        let mut blocked = 0usize;
        for y in cy0..=cy1 {
            for x in cx0..=cx1 {
                seen += 1;
                if frame.mask[(y * w + x) as usize] == 0 {
                    blocked += 1;
                }
            }
        }
        if seen == 0 {
            continue;
        }
        let ratio = blocked as f64 / seen as f64;
        if ratio < HIDDEN {
            continue;
        }
        let key = (x0 as i32, y0 as i32);
        let first = HIDDEN_LOGGED
            .lock()
            .map(|mut seen| {
                if seen.contains(&key) {
                    false
                } else {
                    seen.push(key);
                    true
                }
            })
            .unwrap_or(false);
        if first {
            crate::log_line(&format!(
                "gpu mask: a sprite at ({x0},{y0})..({x1},{y1}) is {:.0}% \
                 occluded by nearer stock draws -- it will look absent",
                ratio * 100.0
            ));
        }
    }
}

/// Draw the frame's sprites, then the fog. Called after WA's fullscreen
/// composite and before its Present, where a second scene is legal and
/// survives — which the S0 probe established against WA's real device
/// parameters.
pub unsafe fn present(device: *mut c_void) {
    // The sprite path and the fog share this second scene and this device.
    // Either one being active is reason enough to run; both off is a no-op.
    let sprites = is_active();
    if device.is_null() || !sprites {
        return;
    }

    if wf_gpu_init(device) == 0 {
        warn("device init failed");
        return;
    }

    // Draw the sprite bands and grab the HUD region for the fog, all while
    // the frame lock is held, then drop it before the fog upload so the fog
    // path never re-locks FRAME (std Mutex is not reentrant).
    {
        let Ok(mut state) = FRAME.lock() else {
            return;
        };
        // A frame is worth drawing sprites from when it is valid and has
        // quads. Fog needs no quads -- only the HUD region capture produced --
        // so it is not gated on the vertex lists.
        let has_quads = state
            .as_ref()
            .is_some_and(|f| f.valid && !(f.vertices.is_empty() && f.vertices_front.is_empty()));
        if let Some(frame) = state.as_mut() {
            // Consume once: a stale frame must never be drawn twice.
            frame.valid = false;
            if sprites && has_quads && ensure_atlas() {
                // Back band first: it is occluded by terrain, so it must not
                // paint over the front band.
                for (mask, verts) in [
                    (&frame.mask, &frame.vertices),
                    (&frame.mask_front, &frame.vertices_front),
                ] {
                    if verts.is_empty() {
                        continue;
                    }
                    if wf_gpu_mask(frame.width as u32, frame.height as u32, mask.as_ptr())
                        == 0
                    {
                        warn("coverage mask upload failed");
                        return;
                    }
                    if wf_gpu_draw(verts.as_ptr() as *const c_void, verts.len() as u32) == 0
                    {
                        warn("sprite draw failed");
                    }
                }
            }
        }
    }

}


/// Hand over the packed atlas, to be uploaded when a device first appears.
///
/// Deliberately cannot fail: the sheets it describes have already been
/// registered and have no palette fallback, so refusing here would make them
/// vanish rather than degrade. Upload problems surface at `present`.
pub fn set_atlas(atlas: render::Atlas) {
    if let Ok(mut held) = ATLAS.lock() {
        *held = Some(atlas);
    }
    ATLAS_UPLOADED.store(false, Ordering::SeqCst);
}

/// Upload the held atlas once a device exists. `D3DPOOL_MANAGED`, so it
/// survives a device reset and this runs once per match rather than per frame.
fn ensure_atlas() -> bool {
    if ATLAS_UPLOADED.load(Ordering::SeqCst) {
        return true;
    }
    let Ok(held) = ATLAS.lock() else {
        return false;
    };
    let Some(atlas) = held.as_ref() else {
        return false;
    };
    let ok = unsafe { wf_gpu_atlas(atlas.width, atlas.height, atlas.pixels.as_ptr()) != 0 };
    if ok {
        ATLAS_UPLOADED.store(true, Ordering::SeqCst);
        log_line(&format!(
            "gpu atlas uploaded: {}x{}",
            atlas.width, atlas.height
        ));
    } else {
        warn("atlas upload failed");
    }
    ok
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
    /// Occlusion band, as a render-queue depth.
    depth: u32,
}

/// Take over one sprite draw, or decline it.
///
/// Returns true when this draw is ours, in which case the caller must NOT
/// enqueue it: WA would otherwise draw a palette copy underneath our quad.
pub fn intercept(_layer: u32, x: i32, y: i32, sprite: u32, anim: u32, actor: *mut u8) -> bool {
    if !is_active() {
        return false;
    }
    // Virtual ids are 0x01000000 | n, so masking to 16 bits throws the tag
    // away and leaves a small number that looks like a stock sprite. WA packs
    // flags alongside the id (the mirror bit, and others); `sheet_id` drops
    // them.
    let id = crate::sprite::sheet_id(sprite);
    if !crate::sprite::is_gpu_sheet(id) {
        // A virtual id that reached here without a sheet must NOT be passed
        // on: WA would index its own sprite table with it and draw whatever
        // happens to live there. Swallow the draw and say so once.
        if crate::sprite::is_virtual(id as i32) {
            warn("virtual sprite id with no GPU sheet");
            return true;
        }
        return false;
    }
    // Read presentation state now: the actor can be freed before dispatch.
    let (angle, scale, tint, depth) = if actor.is_null() {
        // RQ-intercepted worm/body draws have no actor presentation state.
        // Depth must sit *in front of* terrain (0x180000): the previous
        // default (0x190000) put them in the back band, so solid ground /
        // grass occluded them and they rendered under the landscape.
        // Stock worms live around 0xd0xxx in WA's queue.
        (0.0f32, 1.0f32, 0xFFFF_FFFFu32, 0x000D_0000u32)
    } else {
        unsafe {
            let fixed = |v: i32| v as f32 / 65536.0;
            (
                fixed(game::entity::read_i32(actor, game::offsets::ACTOR_ANGLE)),
                fixed(game::entity::read_i32(actor, game::offsets::ACTOR_SCALE)),
                game::entity::read_i32(actor, game::offsets::ACTOR_TINT) as u32,
                game::entity::read_i32(actor, game::offsets::ACTOR_DEPTH) as u32,
            )
        }
    };
    let Ok(mut pending) = PENDING.lock() else {
        return false;
    };
    pending.push(Pending {
        x,
        y,
        sprite,
        // WA's reverse flag is read here, as BlitSprite reads it, before
        // anything rebuilds the sprite value.
        anim: crate::sprite::draw_phase(sprite, anim),
        angle,
        scale,
        tint,
        depth,
    });
    true
}

/// Our sprites in painter's order: their render-queue depth, high (further
/// back) first. They are collected in whatever order WA happens to draw their
/// owners -- a worm's GPU sheet after the actors in front of it -- so without
/// this, depth only ever decided terrain occlusion, never which of two GPU
/// sprites is on top. The sort is stable: equal depths keep collection order.
fn back_to_front(pending: &[Pending]) -> Vec<&Pending> {
    let mut order: Vec<&Pending> = pending.iter().collect();
    order.sort_by(|a, b| b.depth.cmp(&a.depth));
    order
}

/// Resolve pending draws to screen-space quads, back to front.
///
/// The transform mirrors what WA's own dispatcher does to a kind-5 command:
/// drop the 16.16 fraction, subtract the clip origin, then add the camera.
/// Reusing that path rather than deriving screen coordinates independently is
/// what makes the sprites follow wkZoom and camera motion for free.
unsafe fn resolve(frame: &mut Frame, display: usize, clip: usize) {
    frame.vertices.clear();
    frame.vertices_front.clear();
    let Ok(pending) = PENDING.lock() else {
        return;
    };
    let (w, h) = (frame.width as u32, frame.height as u32);
    let clip_x = *(clip as *const i32) >> 16;
    let clip_y = *((clip + 4) as *const i32) >> 16;
    let cam_x = *((display + 0x3560) as *const i32);
    let cam_y = *((display + 0x3564) as *const i32);

    for p in back_to_front(&pending) {
        let id = crate::sprite::sheet_id(p.sprite);
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
        // At or behind terrain's own depth, terrain occludes; in front of it
        // only the HUD does.
        const TERRAIN_DEPTH: u32 = 0x0018_0000;
        let behind_terrain = p.depth >= TERRAIN_DEPTH;
        if !sprite.offscreen(w, h) {
            // Once per distinct sprite id, so every sheet reports itself
            // rather than only whichever happens to resolve first. A sheet
            // that never appears here is not reaching the renderer at all,
            // which is a different bug from one that draws wrongly.
            let first_time = RESOLVED_IDS
                .lock()
                .map(|mut seen| {
                    if seen.contains(&id) {
                        false
                    } else {
                        seen.push(id);
                        true
                    }
                })
                .unwrap_or(false);
            if first_time {
                let (bx0, by0, bx1, by1) = sprite.bounds();
                crate::log_line(&format!(
                    "gpu resolve: id={id} frame={index}/{frames} \
                     rect=({},{} {}x{} in {}x{}) angle={:.1} scale={:.3} \
                     tint={:#010x} raw=({},{}) clip=({clip_x},{clip_y}) \
                     cam=({cam_x},{cam_y}) -> screen=({x},{y}) \
                     bounds=({bx0},{by0})..({bx1},{by1}) surface={w}x{h}",
                    rect.x,
                    rect.y,
                    rect.w,
                    rect.h,
                    rect.atlas_w,
                    rect.atlas_h,
                    sprite.angle,
                    sprite.scale,
                    sprite.tint.0,
                    p.x >> 16,
                    p.y >> 16,
                ));
            }
            if behind_terrain {
                sprite.emit(w, h, &mut frame.vertices);
            } else {
                sprite.emit(w, h, &mut frame.vertices_front);
            }
        }
    }
}

/// Read WA's 8bpp surface: its address, size, and a row-copy closure.
///
/// Mirrors `truecolor::Grid` rather than sharing it, because the two modules
/// are meant to be separable — one of them is going away once the other
/// proves itself.
unsafe fn surface_reader(display: usize) -> Option<(usize, usize, usize, impl Fn(&mut [u8]))> {
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
    if data == 0
        || bpp != 8
        || w == 0
        || h == 0
        || w > 4096
        || h > 4096
        || w % 4 != 0
        || stride < w
        || stride > 16384
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

/// Hook stock `RQ_DrawSpriteLocal` so worm/body draws of virtual GPU sheets
/// are intercepted the same way actor blits are (they never enter actor::blit).
static ORIG_RQ_DRAW: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

unsafe extern "C" fn rq_should_intercept(layer: u32, x: i32, y: i32, sprite: u32, anim: u32) -> u8 {
    // TeamEntity draw_cursor: 0x18000 = live land cursor, 0x18001 = planted X.
    // An eat_fire pack draws its own cursor actor; skip WA's so there is one.
    if (layer == 0x1_8000 || layer == 0x1_8001) && crate::actor::hide_team_cursor() {
        return 1;
    }
    // Only steal draws that are definitely ours. The broader `intercept`
    // path also swallows unknown virtual ids; doing that on every stock
    // RQ_DrawSpriteLocal call can drop the aim cursor / crosshair.
    if !is_active() {
        return 0;
    }
    // Virtual ids are 0x01000000|n — never mask to 16 bits here. Worm aim
    // sheets (worm_sprites) arrive with the mirror bit OR'd in and must match
    // on the full id.
    let gpu_id = crate::sprite::sheet_id(sprite);
    if !crate::sprite::is_gpu_sheet(gpu_id) {
        return 0;
    }
    intercept(layer, x, y, sprite, anim, std::ptr::null_mut()) as u8
}

/// ECX=queue, EAX=y; stdcall stack: layer, x, sprite, anim.
#[unsafe(naked)]
unsafe extern "C" fn rq_draw_hook() {
    core::arch::naked_asm!(
        "push ebp",
        "mov ebp, esp",
        "pushad",
        "push dword ptr [ebp + 20]",
        "push dword ptr [ebp + 16]",
        "push eax",
        "push dword ptr [ebp + 12]",
        "push dword ptr [ebp + 8]",
        "call {check}",
        "add esp, 20",
        "test al, al",
        "jnz 2f",
        "popad",
        "pop ebp",
        "jmp dword ptr [{orig}]",
        "2:",
        "popad",
        "pop ebp",
        "ret 16",
        check = sym rq_should_intercept,
        orig = sym ORIG_RQ_DRAW,
    );
}

pub fn install_rq_draw() -> Result<(), String> {
    let addr = crate::rebase::rb(game::offsets::RQ_DRAW_SPRITE_LOCAL) as *mut c_void;
    let trampoline = unsafe {
        MinHook::create_hook(addr, rq_draw_hook as *mut c_void)
            .map_err(|e| format!("RQ_DrawSpriteLocal hook: {e:?}"))?
    };
    ORIG_RQ_DRAW.store(trampoline, Ordering::SeqCst);
    Ok(())
}

#[cfg(test)]
mod depth_order_tests {
    use super::{back_to_front, Pending};

    fn at(depth: u32, sprite: u32) -> Pending {
        Pending { x: 0, y: 0, sprite, anim: 0, angle: 0.0, scale: 1.0, tint: 0, depth }
    }

    #[test]
    fn gpu_sprites_draw_back_to_front_by_depth() {
        // Collected as WA drew their owners: an actor held in front of the
        // worm (0x10000) before the worm's own sheet (0xD0000), a default
        // actor behind terrain (0x190000) last.
        let pending = [at(0x10000, 1), at(0xD0000, 2), at(0x190000, 3), at(0xD0000, 4)];
        let order: Vec<u32> = back_to_front(&pending).iter().map(|p| p.sprite).collect();
        assert_eq!(order, vec![3, 2, 4, 1], "equal depths keep collection order");
    }
}
