//! Live world queries: landscape walk mask (Landscape+0x914), entity tree
//! (Jelly CTask children), PlaySoundLocal, stock SelectWeapon / StartFiring.

use crate::log::{log_error, log_line};
use game::entity::{read_i32, write_i32};
use game::latch::FrameLatch;
use game::offsets::*;
use std::collections::VecDeque;
use std::ffi::CString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::Mutex;

const MAX_WALK: usize = 8192;
const MAX_CHILDREN: usize = 4096;

struct CustomWav {
    slot: i32,
    path: PathBuf,
    loaded: bool,
}

static WAVS: Mutex<Vec<(String, CustomWav)>> = Mutex::new(Vec::new());
static GRAVE_OWNERS: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
static NEXT_WAV: AtomicI32 = AtomicI32::new(CUSTOM_SOUND_SLOT_MIN);

struct SpaceLatch {
    handle: u32,
    held: bool,
    slot: u32,
    gifted: bool,
    spent: bool,
}

static SPACE_FIRE: Mutex<Vec<SpaceLatch>> = Mutex::new(Vec::new());

pub fn clear_sounds() {
    if let Ok(mut g) = WAVS.lock() {
        g.clear();
    }
    NEXT_WAV.store(CUSTOM_SOUND_SLOT_MIN, Ordering::SeqCst);
    if let Ok(mut owners) = GRAVE_OWNERS.lock() { owners.clear(); }
}

pub fn remember_grave_owner(grave: u32, worm_id: u32) {
    if grave == 0 || worm_id == 0 { return; }
    if let Ok(mut owners) = GRAVE_OWNERS.lock() {
        if let Some(entry) = owners.iter_mut().find(|(handle, _)| *handle == grave) {
            entry.1 = worm_id;
        } else {
            owners.push((grave, worm_id));
        }
    }
}

/// The worm (team * 16 + index) whose gravestone WA has yet to make: dead
/// (0 HP) and not paired with a grave. 0 otherwise.
pub fn graveless_dead_worm(worm: *mut u8) -> u32 {
    if worm.is_null() || worm_hp(worm as u32) > 0 { return 0; }
    let id = unsafe {
        read_i32(worm, ENT_WORM_TEAM).wrapping_mul(16).wrapping_add(read_i32(worm, WORM_INDEX))
    } as u32;
    let paired = GRAVE_OWNERS.lock().is_ok_and(|owners| owners.iter().any(|&(_, owner)| owner == id));
    if id == 0 || paired { 0 } else { id }
}

pub fn grave_identity(grave: u32) -> u32 {
    if grave == 0 { return 0; }
    unsafe { if read_i32(grave as *mut u8, ENT_CLASS_TYPE) != CLASS_GRAVE { return 0; } }
    GRAVE_OWNERS.lock().ok().and_then(|owners|
        owners.iter().find(|(handle, _)| *handle == grave).map(|(_, id)| *id)
    ).unwrap_or(0)
}

pub fn remember_wav(key: String, path: PathBuf) -> i32 {
    if key.is_empty() {
        return 0;
    }
    if let Ok(g) = WAVS.lock() {
        if let Some((_, wav)) = g.iter().find(|(k, _)| k == &key) {
            return wav.slot;
        }
    }
    let slot = NEXT_WAV.fetch_add(1, Ordering::SeqCst);
    if let Ok(mut g) = WAVS.lock() {
        g.push((
            key,
            CustomWav {
                slot,
                path,
                loaded: false,
            },
        ));
    }
    slot
}

pub fn alias_sound(key: String, slot: i32, path: PathBuf) {
    if key.is_empty() || slot == 0 {
        return;
    }
    if let Ok(mut g) = WAVS.lock() {
        if g.iter().any(|(k, _)| k == &key) {
            return;
        }
        g.push((
            key,
            CustomWav {
                slot,
                path,
                loaded: false,
            },
        ));
    }
}

/// Jelly `registerCustomSound` + `LoadWav`: slot 451+ into DSSound, then PlaySoundLocal.
pub fn resolve_sound(key: &str) -> i32 {
    if key.is_empty() {
        return 0;
    }
    let slot = {
        let Ok(mut g) = WAVS.lock() else {
            return 0;
        };
        let Some(wav) = g
            .iter_mut()
            .find(|(k, _)| k == key || k.ends_with(key))
            .map(|(_, w)| w)
        else {
            return 0;
        };
        if wav.loaded {
            return wav.slot;
        }
        let slot = wav.slot;
        let path = wav.path.clone();
        if load_wav_file(slot, &path) {
            for (_, w) in g.iter_mut() {
                if w.slot == slot {
                    w.loaded = true;
                }
            }
            slot
        } else {
            0
        }
    };
    slot
}

pub fn solid_fixed(x: i32, y: i32) -> bool {
    let Some(grid) = live_grid() else {
        return false;
    };
    grid.solid_px(x >> 16, y >> 16)
}

/// Resolve a landscape method the way WA calls it: through the vtable.
///
/// The vtable itself must be WA's (a different layout fails closed), but the
/// slot may hold another module's hook -- wkFOW patches slots 2 and 3 in place
/// to watch terrain changes, and forwards to the original. Requiring WA's
/// exact address there made every cut fail on those machines only, while the
/// others dug: a desync. Calling what WA would call keeps every client on the
/// same chain, whatever is installed.
pub(crate) unsafe fn landscape_method(slot: usize, expected: u32) -> Result<(*mut u8, u32), String> {
    let world = session_world_ptr();
    if world.is_null() {
        return Err("terrain cut: no active world".into());
    }
    let land = *(world.add(GAME_WORLD_LANDSCAPE) as *const *mut u8);
    if land.is_null() {
        return Err("terrain cut: landscape unavailable".into());
    }
    let table = *(land as *const u32);
    if table != crate::wa::address("LANDSCAPE_VTABLE") {
        return Err(format!(
            "terrain cut: unexpected landscape vtable {table:#x}"
        ));
    }
    let method = *((table as *const u32).add(slot));
    if method == 0 {
        return Err(format!("terrain cut: landscape method {slot} is null"));
    }
    if method != expected {
        static REPORTED: [AtomicBool; 4] = [const { AtomicBool::new(false) }; 4];
        if !REPORTED[slot & 3].swap(true, Ordering::Relaxed) {
            log_line(&format!(
                "terrain cut: landscape method {slot} is hooked by another module \
                 ({method:#x}); calling through it, as WA does"
            ));
        }
    }
    Ok((land, method))
}

/// WA's vtable slot 2 edits picture, walk grid, soil, redraw, and checksum.
pub fn land_dig(x: i32, y: i32, radius: i32) -> Result<(), String> {
    if !(1..=200).contains(&radius) {
        return Err("terrain cut: radius must be 1..200 pixels".into());
    }
    unsafe {
        let (land, method) = landscape_method(2, crate::wa::address("LANDSCAPE_WRITE_RADIUS"))?;
        let cut: unsafe extern "thiscall" fn(*mut u8, i32, i32, i32) =
            std::mem::transmute(method as usize);
        cut(land, radius, x >> 16, y >> 16);
    }
    Ok(())
}

/// WA's vtable slot 3 stamps a preloaded drill/torch cut shape at pixel (x,y).
pub fn land_cut(shape: i32, x: i32, y: i32) -> Result<(), String> {
    if shape != 2 && !(6..=11).contains(&shape) {
        return Err("terrain cut: only drill and torch shapes are supported".into());
    }
    unsafe {
        let (land, method) = landscape_method(3, crate::wa::address("LANDSCAPE_WRITE_MASK_ID"))?;
        let mask = *((land.add(LANDSCAPE_SHAPES + shape as usize * 4)) as *const u32);
        if mask == 0 {
            return Err(format!("terrain cut: stock shape {shape} is not loaded"));
        }
        let cut: unsafe extern "thiscall" fn(*mut u8, i32, i32, i32) =
            std::mem::transmute(method as usize);
        cut(land, shape, x >> 16, y >> 16);
    }
    Ok(())
}

pub fn trace_fixed(x0: i32, y0: i32, x1: i32, y1: i32) -> Option<(i32, i32)> {
    let grid = live_grid()?;
    let hit = trace_pixels(x0 >> 16, y0 >> 16, x1 >> 16, y1 >> 16, |x, y| {
        grid.solid_px(x, y)
    })?;
    Some((hit.0 << 16, hit.1 << 16))
}

fn electrical_targets(include_graves: bool) -> Vec<u32> {
    let root = unsafe { session_root_ptr() };
    if root.is_null() { return Vec::new(); }
    let mut out = Vec::new();
    let mut q = VecDeque::new();
    q.push_back(root);
    let mut visited = 0;
    while let Some(node) = q.pop_front() {
        visited += 1;
        if node.is_null() || visited > MAX_WALK { break; }
        unsafe {
            let class = read_i32(node, ENT_CLASS_TYPE);
            if matches!(class, CLASS_WORM | CLASS_MINE | CLASS_CRATE | CLASS_OIL)
                || (include_graves && class == CLASS_GRAVE) {
                if class != CLASS_WORM || worm_hp(node as u32) > 0 { out.push(node as u32); }
            }
            let count = (read_i32(node, ENT_CHILDREN_WATERMARK) as usize).min(MAX_CHILDREN);
            let children = read_i32(node, ENT_CHILDREN_DATA) as *const u32;
            if !children.is_null() {
                for i in 0..count {
                    let child = *children.add(i);
                    if child != 0 { q.push_back(child as *mut u8); }
                }
            }
        }
    }
    out
}

pub fn electrical_nearby(x: i32, y: i32, radius_px: i32, graves: bool) -> Vec<u32> {
    if !(1..=1000).contains(&radius_px) { return Vec::new(); }
    let radius = radius_px as i128 * 65536;
    electrical_targets(graves).into_iter().filter(|&handle| unsafe {
        let object = handle as *mut u8;
        let dx = read_i32(object, ENT_POS_X) as i128 - x as i128;
        let dy = read_i32(object, ENT_POS_Y) as i128 - y as i128;
        dx * dx + dy * dy <= radius * radius
    }).collect()
}

/// Terrain plus electrical target classes, in WA entity traversal order.
/// Uses integer projection for peer-stable ties and returns the nearest hit.
pub fn electrical_trace(x0: i32, y0: i32, x1: i32, y1: i32) -> Option<(i32, i32, u32)> {
    let dx = x1 as i128 - x0 as i128;
    let dy = y1 as i128 - y0 as i128;
    if dx.abs() > 10000 * 65536 || dy.abs() > 10000 * 65536 { return None; }
    let length2 = dx * dx + dy * dy;
    if length2 == 0 { return None; }
    let land = trace_fixed(x0, y0, x1, y1);
    let mut best = land.map(|(x, y)|
        ((x as i128 - x0 as i128) * dx + (y as i128 - y0 as i128) * dy).max(0)
    ).unwrap_or(length2);
    let mut hit = land.map(|(x, y)| (x, y, 0));
    for handle in electrical_targets(true) {
        unsafe {
            let object = handle as *mut u8;
            let ox = read_i32(object, ENT_POS_X);
            let oy = read_i32(object, ENT_POS_Y);
            let px = ox as i128 - x0 as i128;
            let py = oy as i128 - y0 as i128;
            let along = px * dx + py * dy;
            if along <= 0 || along >= best { continue; }
            let radius_px = match read_i32(object, ENT_CLASS_TYPE) {
                CLASS_WORM => 6, CLASS_GRAVE => 8, CLASS_MINE => 5, _ => 9,
            };
            let cross = px * dy - py * dx;
            let radius = radius_px as i128 * 65536;
            if cross * cross <= radius * radius * length2 {
                best = along;
                hit = Some((ox, oy, handle));
            }
        }
    }
    hit
}

pub fn map_size_px() -> (i32, i32) {
    live_grid()
        .map(|g| (g.width as i32, g.height as i32))
        .unwrap_or((0, 0))
}

/// Camera center (16.16) and viewport size (pixels).
pub fn view_cam() -> (i32, i32, i32, i32) {
    let world = session_world();
    if world.is_null() {
        return (0, 0, 0, 0);
    }
    unsafe {
        (
            read_i32(world, GAME_WORLD_VIEW_CX),
            read_i32(world, GAME_WORLD_VIEW_CY),
            read_i32(world, GAME_WORLD_VIEWPORT_W),
            read_i32(world, GAME_WORLD_VIEWPORT_H),
        )
    }
}

/// Last RQ dispatch view: clip origin (Fixed) + Display cam (pixels).
/// Same pair GPU entity resolve uses — **not** HUD screen-absolutes.
static VIEW_CLIP_X: AtomicI32 = AtomicI32::new(0);
static VIEW_CLIP_Y: AtomicI32 = AtomicI32::new(0);
static VIEW_CAM_X: AtomicI32 = AtomicI32::new(0);
static VIEW_CAM_Y: AtomicI32 = AtomicI32::new(0);
static VIEW_SURF_W: AtomicI32 = AtomicI32::new(0);
static VIEW_SURF_H: AtomicI32 = AtomicI32::new(0);
static VIEW_XFORM: AtomicBool = AtomicBool::new(false);

/// Latch from render-queue dispatch (`display`, `clip`). Entity path only.
pub fn latch_entity_view(display: usize, clip: usize) {
    if display == 0 || clip == 0 {
        return;
    }
    unsafe {
        VIEW_CLIP_X.store(*(clip as *const i32), Ordering::Relaxed);
        VIEW_CLIP_Y.store(*((clip + 4) as *const i32), Ordering::Relaxed);
        VIEW_CAM_X.store(
            *((display + DISPLAY_VIEW_X) as *const i32),
            Ordering::Relaxed,
        );
        VIEW_CAM_Y.store(
            *((display + DISPLAY_VIEW_Y) as *const i32),
            Ordering::Relaxed,
        );
        VIEW_SURF_W.store(
            *((display + DISPLAY_WIDTH) as *const i32),
            Ordering::Relaxed,
        );
        VIEW_SURF_H.store(
            *((display + DISPLAY_HEIGHT) as *const i32),
            Ordering::Relaxed,
        );
    }
    VIEW_XFORM.store(true, Ordering::Relaxed);
}

/// Inverse of GPU entity resolve: `world_px = screen + clip_px - cam`.
/// `sx`/`sy` are Display surface pixels (same space as `DISPLAY_VIEW_*`).
pub fn screen_to_world_fixed(sx: i32, sy: i32) -> (i32, i32) {
    if !VIEW_XFORM.load(Ordering::Relaxed) {
        // Before first frame: proven spawn point = camera centre in world Fixed.
        let (cx, cy, _, _) = view_cam();
        return (cx, cy);
    }
    let clip_x = VIEW_CLIP_X.load(Ordering::Relaxed) >> 16;
    let clip_y = VIEW_CLIP_Y.load(Ordering::Relaxed) >> 16;
    let cam_x = VIEW_CAM_X.load(Ordering::Relaxed);
    let cam_y = VIEW_CAM_Y.load(Ordering::Relaxed);
    let wx = (sx as i64 + clip_x as i64 - cam_x as i64) as i32;
    let wy = (sy as i64 + clip_y as i64 - cam_y as i64) as i32;
    (wx << 16, wy << 16)
}

/// Client hwnd pixel → Display surface pixel, then entity screen→world.
fn client_to_world_fixed(mx: i32, my: i32, cw: i32, ch: i32) -> Option<(i32, i32)> {
    if cw <= 0 || ch <= 0 {
        return None;
    }
    let sw = VIEW_SURF_W.load(Ordering::Relaxed);
    let sh = VIEW_SURF_H.load(Ordering::Relaxed);
    let (sx, sy) = if VIEW_XFORM.load(Ordering::Relaxed) && sw > 0 && sh > 0 {
        (
            ((mx as i64 * sw as i64) / cw as i64) as i32,
            ((my as i64 * sh as i64) / ch as i64) as i32,
        )
    } else {
        // No latch yet: map through GameWorld viewport onto camera centre.
        let (cx, cy, vw, vh) = view_cam();
        if vw <= 0 || vh <= 0 {
            return Some((cx, cy));
        }
        let gx = ((mx as i64 * vw as i64) / cw as i64) as i32;
        let gy = ((my as i64 * vh as i64) / ch as i64) as i32;
        return Some((
            cx.wrapping_sub(vw << 15).wrapping_add(gx << 16),
            cy.wrapping_sub(vh << 15).wrapping_add(gy << 16),
        ));
    };
    Some(screen_to_world_fixed(sx, sy))
}

/// Live aim in world 16.16 + left button. Game thread only.
///
/// Strike packs with land-cursor mode still read `World+0x7380/84`.
/// Otherwise: OS mouse → Display surface → entity screen→world (GPU inverse).
/// Never the HUD path (screen absolutes that track the camera).
pub fn pointer() -> (i32, i32, bool) {
    POINTER_SYNC_REQUESTED.store(1, Ordering::Relaxed);
    if let Some(frame) = sim_frame() {
        POINTER_SYNC_UNTIL.store((frame as u32).saturating_add(60), Ordering::Relaxed);
    } else {
        POINTER_SYNC_UNTIL.store(u32::MAX, Ordering::Relaxed);
    }
    pointer_latched()
}

fn pointer_latched() -> (i32, i32, bool) {
    latched(&POINTER, || {
        resolved_input()
            .map(|sample| (sample.1, sample.2, sample.3))
            .unwrap_or_else(local_pointer_latched)
    })
}

fn poll_pointer() -> (i32, i32, bool) {
    let down = mouse_down();
    if land_aim_mode() {
        if let Some((x, y)) = land_cursor() {
            return (x, y, down);
        }
    }
    let (cx, cy, _, _) = view_cam();
    if let Some((mx, my, cw, ch)) = client_mouse() {
        if let Some((x, y)) = client_to_world_fixed(mx, my, cw, ch) {
            return (x, y, down);
        }
    }
    // Proven fallback: camera centre is already world Fixed.
    (cx, cy, down)
}

/// TeamEntity for a team index (`ENT_WORM_TEAM` of any worm on it), or null.
pub fn team_entity(team: i32) -> *mut u8 {
    if team < 0 {
        return std::ptr::null_mut();
    }
    unsafe {
        let root = session_root_ptr();
        if root.is_null() {
            return std::ptr::null_mut();
        }
        let shared = read_i32(root, ENT_SHARED_DATA) as *mut u8;
        shared_lookup(shared, team as u32, SHARED_KEY_TEAM).unwrap_or(std::ptr::null_mut())
    }
}

/// True when WA is in land-cursor input mode (not camera-pan mode).
fn land_aim_mode() -> bool {
    unsafe {
        for h in crate::actor::list_worms_pub(0) {
            if h == 0 {
                continue;
            }
            let w = h as *mut u8;
            if read_i32(w, WORM_TURN_ACTIVE) != 0 && read_i32(w, WORM_SHOW_CURSOR) != 0 {
                return true;
            }
        }
        let root = session_root_ptr();
        if root.is_null() {
            return false;
        }
        let shared = read_i32(root, ENT_SHARED_DATA) as *mut u8;
        if shared.is_null() {
            return false;
        }
        for team in 0..=8u32 {
            if let Some(team_ent) = shared_lookup(shared, team, SHARED_KEY_TEAM) {
                if read_i32(team_ent, TEAM_CURSOR_MOVE) != 0 {
                    return true;
                }
            }
        }
        false
    }
}

/// Teleport / strike land aim (Fixed 16.16), lockstep-updated by WA.
fn land_cursor() -> Option<(i32, i32)> {
    let world = session_world();
    if world.is_null() {
        return None;
    }
    let (x, y) = unsafe {
        (
            read_i32(world, GAME_WORLD_LAND_CURSOR_X),
            read_i32(world, GAME_WORLD_LAND_CURSOR_Y),
        )
    };
    if x == 0 && y == 0 {
        return None;
    }
    Some((x, y))
}

/// Clear planted strike X without touching the live MOVE aim bit.
/// `TeamEntity__reset_cursor` is EAX-usercall (not thiscall) — calling it wrong
/// AVs. Writing MARK is enough for presentation; MOVE stays so aim continues.
pub fn reset_strike_cursors() {
    unsafe {
        let root = session_root_ptr();
        if root.is_null() {
            return;
        }
        let shared = read_i32(root, ENT_SHARED_DATA) as *mut u8;
        if shared.is_null() {
            return;
        }
        for team in 0..=8u32 {
            if let Some(team_ent) = shared_lookup(shared, team, SHARED_KEY_TEAM) {
                write_i32(team_ent, TEAM_CURSOR_MARK, 0);
            }
        }
    }
}

unsafe fn shared_lookup(table: *mut u8, key_esi: u32, key_edi: u32) -> Option<*mut u8> {
    if table.is_null() {
        return None;
    }
    let bucket = (key_esi.wrapping_mul(0x11).wrapping_add(key_edi) & 0xFF) as usize;
    let mut node = *(table.add(bucket * 4) as *const *mut u8);
    let mut hops = 0u32;
    while !node.is_null() && hops < 64 {
        hops += 1;
        let n_edi = *(node.add(4) as *const u32);
        let n_esi = *(node.add(8) as *const u32);
        if n_edi == key_edi && n_esi == key_esi {
            let found = *(node.add(12) as *const *mut u8);
            if !found.is_null() {
                return Some(found);
            }
            return None;
        }
        node = *(node as *const *mut u8);
    }
    None
}

struct FrozenTurn {
    timer: i32,
    display: i32,
    active: i32,
}

static FROZEN_TURN: Mutex<Option<FrozenTurn>> = Mutex::new(None);

/// Pin the HUD turn clock like a grenade in flight.
pub fn freeze_turn() {
    let root = unsafe { session_root_ptr() };
    if root.is_null() {
        return;
    }
    unsafe {
        if let Ok(mut g) = FROZEN_TURN.lock() {
            if g.is_none() {
                *g = Some(FrozenTurn {
                    timer: read_i32(root, WORLD_ROOT_TURN_TIMER),
                    display: read_i32(root, WORLD_ROOT_TURN_TIMER_DISPLAY),
                    active: read_i32(root, WORLD_ROOT_WORM_ACTIVE),
                });
                crate::log_line(&format!(
                    "turn frozen (a worm is carried): timer {} active {}",
                    read_i32(root, WORLD_ROOT_TURN_TIMER),
                    read_i32(root, WORLD_ROOT_WORM_ACTIVE)
                ));
            }
        }
        write_i32(root, WORLD_ROOT_WORM_ACTIVE, 1);
        let world = read_i32(root, ENT_WORLD) as *mut u8;
        if !world.is_null() {
            write_i32(world, GAME_WORLD_WORM_ACTIVE, 1);
        }
        hold_turn_timer();
    }
}

/// Drop WA's wish for an instant replay of this turn (see
/// `GAME_WORLD_ACTION_REPLAY_WANTED`), logging each one.
pub unsafe fn cancel_action_replay(root: *mut u8) {
    if root.is_null() {
        return;
    }
    let world = read_i32(root, ENT_WORLD) as *mut u8;
    if world.is_null() || read_i32(world, GAME_WORLD_ACTION_REPLAY_WANTED) == 0 {
        return;
    }
    write_i32(world, GAME_WORLD_ACTION_REPLAY_WANTED, 0);
    crate::log_line(&format!(
        "instant replay cancelled at f={} (WormForge plays none)",
        read_i32(world, GAME_WORLD_FRAME_COUNTER)
    ));
}

pub fn thaw_turn() {
    let root = unsafe { session_root_ptr() };
    let saved = FROZEN_TURN.lock().ok().and_then(|mut g| g.take());
    let Some(saved) = saved else {
        return;
    };
    crate::log_line(&format!("turn thawed: active back to {}", saved.active));
    if root.is_null() {
        return;
    }
    unsafe {
        write_i32(root, WORLD_ROOT_WORM_ACTIVE, saved.active);
        let world = read_i32(root, ENT_WORLD) as *mut u8;
        if !world.is_null() {
            write_i32(world, GAME_WORLD_WORM_ACTIVE, saved.active);
        }
    }
}

pub fn hold_turn_timer() {
    let root = unsafe { session_root_ptr() };
    if root.is_null() {
        return;
    }
    let Some((timer, display)) = FROZEN_TURN
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|f| (f.timer, f.display)))
    else {
        return;
    };
    unsafe {
        write_i32(root, WORLD_ROOT_TURN_TIMER, timer);
        write_i32(root, WORLD_ROOT_TURN_TIMER_DISPLAY, display);
        write_i32(root, WORLD_ROOT_WORM_ACTIVE, 1);
    }
}

fn mouse_down() -> bool {
    key_down(0x01)
}

fn key_down(vk: i32) -> bool {
    unsafe {
        windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(vk) as u16 & 0x8000 != 0
    }
}

/// Input, sampled once per simulation frame.
///
/// Everything that reads input goes through here -- packs via
/// `wa.world.keys()` / `pointer()`, and `pump_space_fire` below -- so that
/// within one frame they all see the same thing. Live network games resolve
/// the peer aggregate supplied by `sync`; offline games resolve the OS-local
/// latches below; replay playback resolves its recorded sample first.
///
/// It is also the one place a recorder needs to tap: "the input this frame was
/// X" now has a single answer to write down.
///
/// Keyed on WA's own frame counter. A consequence worth knowing: while the
/// simulation is not advancing, the sample does not change either. That is
/// correct for anything that feeds the simulation. A pack wanting a live
/// cursor for pure decoration would need a separate, explicitly-unlatched
/// call, which does not exist yet because nothing has asked for one.
/// The public pair and OS-local pair are separate: the process-input hook must
/// be able to sample this peer without recursively asking for the aggregate.
static KEYS: FrameLatch<u32> = FrameLatch::new();
static POINTER: FrameLatch<(i32, i32, bool)> = FrameLatch::new();
static LOCAL_KEYS: FrameLatch<u32> = FrameLatch::new();
static LOCAL_POINTER: FrameLatch<(i32, i32, bool)> = FrameLatch::new();
static POINTER_SYNC_REQUESTED: AtomicU32 = AtomicU32::new(0);
/// Keep sending pointer motion for a while after a pack last called `pointer()`.
/// The one-shot `POINTER_SYNC_REQUESTED` bit is sampled in the net queue *before*
/// Lua brains run, so without stickiness a cursor actor only got the ~1s heartbeat
/// and spawned/stuck near world (0,0).
static POINTER_SYNC_UNTIL: AtomicU32 = AtomicU32::new(0);

/// WA's sim frame, or `None` outside a game (frontend, map editor).
pub(crate) fn sim_frame() -> Option<i64> {
    let world = session_world();
    if world.is_null() {
        return None;
    }
    Some(unsafe { read_i32(world, GAME_WORLD_FRAME_COUNTER) } as i64)
}

/// Outside a game there is no frame to be consistent with, so answer live
/// rather than serving a sample held over from the last match.
fn latched<T: Copy>(latch: &FrameLatch<T>, sample: impl FnOnce() -> T) -> T {
    match sim_frame() {
        None => sample(),
        Some(frame) => latch.get(frame, sample),
    }
}

/// Draw from WA's own shared RNG.
///
/// **This is the only randomness a pack may have, and the reason is lockstep.**
/// `math.random` is nil in the sandbox because a host RNG is seeded per
/// process, so two peers would draw different numbers immediately. WA's RNG is
/// part of the simulation: both peers advance it in step, and a replay
/// reproduces it exactly. Project X reached the same conclusion — the LCG
/// multiplier `0x19660D` appears zero times in `wkPX.dll`, so all ~227 of its
/// `GS->GetRandom()` script calls defer to WA's.
///
/// We call WA's function rather than reimplementing the formula. It is six
/// instructions and we know them, but calling cannot drift: if WA's arithmetic
/// ever differed from our copy by one detail, every draw afterwards would
/// diverge and the cause would be invisible.
///
/// **Drawing advances shared state.** The next stock draw — crate placement,
/// barrel spread, damage jitter — gets the value after ours rather than the
/// one it would have had. That is deliberate and safe for lockstep, because
/// both peers and a replay all take the same draws in the same order. But a
/// game played with a pack that draws is not the same game as one played
/// without it, which is a thing to know rather than a bug.
///
/// Returns `None` before the world exists, or if the function's first
/// instruction is not the one we verified.
pub fn draw_random() -> Option<u32> {
    let world = session_world();
    if world.is_null() {
        return None;
    }
    if !advance_rng_verified() {
        return None;
    }
    let f = crate::wa::address("GAME_WORLD_ADVANCE_RNG") as u32;
    let args = [world as u32, f];
    let out: u32;
    unsafe {
        core::arch::asm!(
            "push esi",
            "push edi",
            "mov ecx, dword ptr [ebx]",
            "call dword ptr [ebx + 4]",
            "pop edi",
            "pop esi",
            in("ebx") args.as_ptr(),
            lateout("eax") out,
            out("ecx") _,
            out("edx") _,
        );
    }
    Some(out)
}

/// Validate the live entry once. A prior module's supported jump is callable.
fn advance_rng_verified() -> bool {
    match RNG_OK.load(Ordering::Relaxed) {
        0 => {
            let result = crate::wa::check_callable("GAME_WORLD_ADVANCE_RNG");
            let ok = result.is_ok();
            RNG_OK.store(if ok { 2 } else { 1 }, Ordering::Relaxed);
            if let Err(e) = result {
                log_error(&format!("wa.random() disabled: {e}"));
            }
            ok
        }
        v => v == 2,
    }
}

static RNG_OK: AtomicU32 = AtomicU32::new(0);

/// True when peers exist. Input itself is synchronized; packs can use this for
/// behavior that intentionally differs between offline and network play.
///
/// `GameWorld+0x1C` is WA's per-game network session. OpenWA's constructor and
/// every online/offline branch agree on this field; `GameWorld+0x28` is a
/// different network object and was observed null in a real online round.
pub fn is_networked() -> bool {
    let world = session_world();
    if world.is_null() {
        return false;
    }
    unsafe { read_i32(world, GAME_WORLD_NET_SESSION) != 0 }
}

/// Drop all held samples. A new match restarts WA's frame counter, and the
/// first frame must not inherit the last frame of the previous game.
pub fn clear_input_latch() {
    KEYS.clear();
    POINTER.clear();
    LOCAL_KEYS.clear();
    LOCAL_POINTER.clear();
    POINTER_SYNC_REQUESTED.store(0, Ordering::Relaxed);
    POINTER_SYNC_UNTIL.store(0, Ordering::Relaxed);
}

/// Bits: 1 left, 2 right, 4 up, 8 down, 16 W, 32 sprint (shift), 64 Q, 128 Space, 4096 Ctrl.
pub fn keys() -> u32 {
    keys_latched()
}

fn keys_latched() -> u32 {
    latched(&KEYS, || {
        resolved_input()
            .map(|sample| sample.0)
            .unwrap_or_else(local_keys_latched)
    })
}

fn resolved_input() -> Option<(u32, i32, i32, bool)> {
    let frame = sim_frame()?;
    if let Some(sample) = crate::replay::playback_input(frame) {
        return Some(sample);
    }
    if is_networked() {
        // Never fall through to OS-local input online. Prefer the last latched
        // sync sample; if none has arrived yet, use map-centre (not 0,0) so a
        // cursor actor does not spawn in the corner during the first heartbeat.
        return Some(crate::sync::input(frame).unwrap_or_else(neutral_network_input));
    }
    None
}

/// Deterministic placeholder before the first lockstep input record arrives.
fn neutral_network_input() -> (u32, i32, i32, bool) {
    let (w, h) = map_size_px();
    if w > 0 && h > 0 {
        (0, (w / 2) << 16, (h / 2) << 16, false)
    } else {
        (0, 0, 0, false)
    }
}

/// The exact per-frame samples consumed by packs and the engine. Calling this
/// at FRAME_FINISH returns an earlier sample if something already read input
/// this frame, or takes the one and only sample otherwise.
pub(crate) fn replay_input() -> (u32, i32, i32, bool) {
    let keys = keys_latched();
    let (px, py, down) = pointer_latched();
    (keys, px, py, down)
}

/// The OS-local sample contributed to the lockstep carrier. This is separate
/// from `keys_latched` / `pointer_latched`, which expose the merged peer value.
pub(crate) fn local_input_for_sync() -> (u32, i32, i32, bool) {
    let keys = local_keys_latched();
    let (px, py, down) = local_pointer_latched();
    (keys, px, py, down)
}

pub(crate) fn take_pointer_sync_request() -> bool {
    let one_shot = POINTER_SYNC_REQUESTED.swap(0, Ordering::Relaxed) != 0;
    let until = POINTER_SYNC_UNTIL.load(Ordering::Relaxed);
    let sticky = match sim_frame() {
        Some(frame) => (frame as u32) <= until,
        // Frontend / no world: if something asked, keep requesting.
        None => until != 0,
    };
    one_shot || sticky
}

fn local_keys_latched() -> u32 {
    latched(&LOCAL_KEYS, poll_keys)
}

fn local_pointer_latched() -> (i32, i32, bool) {
    latched(&LOCAL_POINTER, poll_pointer)
}

/// True while Space is held, as sampled for this frame. `pump_space_fire` and
/// a pack reading `keys().space` therefore always agree.
fn space_held() -> bool {
    keys_latched() & 128 != 0
}

fn poll_keys() -> u32 {
    let mut m = 0u32;
    if key_down(0x25) || key_down(0x41) {
        m |= 1;
    }
    if key_down(0x27) || key_down(0x44) {
        m |= 2;
    }
    if key_down(0x26) {
        m |= 4;
    }
    if key_down(0x28) || key_down(0x53) {
        m |= 8;
    }
    if key_down(0x57) {
        m |= 16;
    }
    if key_down(0x10) {
        m |= 32;
    }
    if key_down(0x51) {
        m |= 64;
    }
    if key_down(0x20) {
        m |= 128;
    }
    if key_down(0x11) {
        m |= game::sync_input::CTRL;
    }
    // The digit held, top row or keypad (the lowest, if several): 1..10 for
    // 0..9 in bits 8..11 (game::sync_input::DIGIT_SHIFT).
    if let Some(d) = (0..10u32).find(|&d| key_down(0x30 + d as i32) || key_down(0x60 + d as i32)) {
        m |= (d + 1) << game::sync_input::DIGIT_SHIFT;
    }
    m
}

/// Give this worm's team a shot of `slot` if empty, then stock-select it
/// (`WormEntity__SelectWeapon`, same body as F1's SelectWeapon message).
/// Space after this is stock charge / fire (`StartFiring` / `ReleaseWeapon`).
pub fn equip_weapon(handle: u32, slot: u32) -> i32 {
    if handle == 0 || slot == 0 || slot >= STOCK_WEAPON_COUNT as u32 {
        return 0;
    }
    let worm = handle as *mut u8;
    unsafe {
        if read_i32(worm, WORM_TURN_ACTIVE) == 0 {
            return 0;
        }
        let world = read_i32(worm, ENT_WORLD) as *mut u8;
        if world.is_null() {
            return 0;
        }
        enable_slot(world, slot);
        let arena = world.add(GAME_WORLD_TEAM_ARENA);
        let team = read_i32(worm, ENT_WORM_TEAM) as u32;
        clear_delay(arena, team, slot);
        let mut ammo = get_ammo(arena, team, slot);
        let mut gifted = false;
        if ammo == 0 {
            add_ammo(arena, team, slot, 1);
            ammo = get_ammo(arena, team, slot);
            if ammo != 0 {
                gifted = true;
            }
        }
        if ammo == 0 {
            write_ammo(arena, team, slot, 1);
            ammo = get_ammo(arena, team, slot);
            if ammo == 0 {
                ammo = 1;
            }
            gifted = true;
        }
        mark_panel_dirty(world);
        select_weapon(worm, slot, ammo);
        arm_stock_fire(worm, slot, ammo, gifted);
        ammo
    }
}

/// After `worm:equip`, Space-down starts the stock power bar and Space-up shoots.
pub fn pump_space_fire(worm: *mut u8) {
    if worm.is_null() {
        return;
    }
    let handle = worm as u32;
    unsafe {
        if read_i32(worm, WORM_TURN_ACTIVE) == 0 {
            reclaim_gift(worm, handle);
            drop_space_latch(handle);
            return;
        }
        if read_i32(worm, WORM_SELECTED_WEAPON) == 0 {
            drop_space_latch(handle);
            return;
        }
    }
    let watching = SPACE_FIRE
        .lock()
        .ok()
        .map(|g| g.iter().any(|s| s.handle == handle))
        .unwrap_or(false);
    if !watching {
        return;
    }
    unsafe {
        keep_stock_fire_armed(worm);
    }
    // The latched sample, not a fresh poll: this runs inside a worm's
    // FRAME_FINISH, and a pack's keys() may run in the same frame. Polling
    // separately let them disagree about whether Space was held.
    let down = space_held();
    let mut was = false;
    if let Ok(mut g) = SPACE_FIRE.lock() {
        if let Some(s) = g.iter_mut().find(|s| s.handle == handle) {
            was = s.held;
            s.held = down;
        }
    }
    if down && !was {
        unsafe {
            start_firing(worm);
        }
        log_line(&format!("space charge worm={handle:#x}"));
    } else if !down && was {
        unsafe {
            release_shot(worm);
            spend_gift(worm, handle);
        }
        log_line(&format!("space shoot worm={handle:#x}"));
    }
}

unsafe fn arm_stock_fire(worm: *mut u8, slot: u32, ammo: i32, gifted: bool) {
    write_i32(worm, WORM_WEAPONS_ENABLED, 1);
    write_i32(worm, WORM_SELECTED_AMMO, ammo);
    let world = read_i32(worm, ENT_WORLD) as *mut u8;
    if !world.is_null() {
        let table = *(world.add(GAME_WORLD_WEAPON_TABLE) as *const *mut u8);
        if !table.is_null() {
            let entry = table.add(slot as usize * WEAPON_ENTRY_SIZE);
            write_i32(worm, WORM_ACTIVE_WEAPON, entry as i32);
        }
    }
    let handle = worm as u32;
    if let Ok(mut g) = SPACE_FIRE.lock() {
        if let Some(s) = g.iter_mut().find(|s| s.handle == handle) {
            s.held = false;
            s.slot = slot;
            s.gifted = gifted;
            s.spent = false;
        } else {
            g.push(SpaceLatch {
                handle,
                held: false,
                slot,
                gifted,
                spent: false,
            });
        }
    }
    log_line(&format!(
        "equip fire-ready slot={slot} ammo={ammo} gifted={gifted} worm={handle:#x}"
    ));
}

unsafe fn keep_stock_fire_armed(worm: *mut u8) {
    write_i32(worm, WORM_WEAPONS_ENABLED, 1);
    let slot = read_i32(worm, WORM_SELECTED_WEAPON) as u32;
    if slot == 0 {
        return;
    }
    let world = read_i32(worm, ENT_WORLD) as *mut u8;
    if world.is_null() {
        return;
    }
    let table = *(world.add(GAME_WORLD_WEAPON_TABLE) as *const *mut u8);
    if table.is_null() {
        return;
    }
    let entry = table.add(slot as usize * WEAPON_ENTRY_SIZE);
    write_i32(worm, WORM_ACTIVE_WEAPON, entry as i32);
}

fn drop_space_latch(handle: u32) {
    if let Ok(mut g) = SPACE_FIRE.lock() {
        g.retain(|s| s.handle != handle);
    }
}

unsafe fn spend_gift(worm: *mut u8, handle: u32) {
    let (slot, gifted, spent) = match SPACE_FIRE.lock().ok().and_then(|g| {
        g.iter()
            .find(|s| s.handle == handle)
            .map(|s| (s.slot, s.gifted, s.spent))
    }) {
        Some(v) => v,
        None => return,
    };
    if !gifted || spent || slot == 0 {
        return;
    }
    take_team_ammo(worm, slot);
    write_i32(worm, WORM_SELECTED_AMMO, 0);
    if let Ok(mut g) = SPACE_FIRE.lock() {
        if let Some(s) = g.iter_mut().find(|s| s.handle == handle) {
            s.spent = true;
            s.gifted = false;
        }
    }
}

unsafe fn reclaim_gift(worm: *mut u8, handle: u32) {
    spend_gift(worm, handle);
}

unsafe fn take_team_ammo(worm: *mut u8, slot: u32) {
    let world = read_i32(worm, ENT_WORLD) as *mut u8;
    if world.is_null() {
        return;
    }
    let arena = world.add(GAME_WORLD_TEAM_ARENA);
    let team = read_i32(worm, ENT_WORM_TEAM) as u32;
    write_ammo(arena, team, slot, 0);
    mark_panel_dirty(world);
    let left = get_ammo(arena, team, slot);
    log_line(&format!("ammo spent slot={slot} team={team} left={left}"));
}

pub fn worm_hp(handle: u32) -> i32 {
    if handle == 0 {
        return 0;
    }
    unsafe {
        let worm = handle as *mut u8;
        let world = read_i32(worm, ENT_WORLD) as *mut u8;
        if world.is_null() {
            return 0;
        }
        let team = read_i32(worm, ENT_WORM_TEAM);
        let idx = read_i32(worm, WORM_INDEX);
        if team < 0 || idx < 0 {
            return 0;
        }
        let off = ARENA_WORM_HP
            + (team as usize) * WORLD_TEAM_STRIDE
            + (idx as usize) * ARENA_WORM_STRIDE;
        read_i32(world, off)
    }
}

/// The same shared health row PX's SetWormHealth changes. Only live worms may
/// be healed; resurrection has its own path and must rebuild an entity.
pub fn heal_worm(handle: u32, amount: i32) -> i32 {
    if handle == 0 || amount <= 0 || amount > 10000 {
        return worm_hp(handle);
    }
    unsafe {
        let worm = handle as *mut u8;
        if read_i32(worm, ENT_CLASS_TYPE) != CLASS_WORM { return 0; }
        let world = read_i32(worm, ENT_WORLD) as *mut u8;
        if world.is_null() { return 0; }
        let team = read_i32(worm, ENT_WORM_TEAM);
        let idx = read_i32(worm, WORM_INDEX);
        if !(1..=MAX_TEAMS as i32).contains(&team) || !(1..=MAX_WORMS_PER_TEAM as i32).contains(&idx) {
            return 0;
        }
        let off = ARENA_WORM_HP + team as usize * WORLD_TEAM_STRIDE + idx as usize * ARENA_WORM_STRIDE;
        let current = read_i32(world, off);
        if current <= 0 { return current; }
        let next = current.saturating_add(amount);
        write_i32(world, off, next);
        next
    }
}

/// Arm a settled stock mine using MineEntity's own fuse/trigger fields. WA's
/// FrameFinish handler continues its beep, dud and explosion paths.
pub fn arm_mine(handle: u32, fuse_ms: i32) -> bool {
    if handle == 0 || !(0..=10_000).contains(&fuse_ms) { return false; }
    unsafe {
        let mine = handle as *mut u8;
        if read_i32(mine, ENT_CLASS_TYPE) != CLASS_MINE
            || read_i32(mine, 0x120) <= 0 // special mines cannot trigger
            || read_i32(mine, 0x128) != 0 // already triggered
            || read_i32(mine, 0x11C) > 0 { return false; }
        write_i32(mine, 0x118, fuse_ms);
        write_i32(mine, 0x128, 1);
        write_i32(mine, 0x12C, fuse_ms / 250);
        true
    }
}

pub fn mine_fuse(handle: u32) -> i32 {
    if handle == 0 { return 0; }
    unsafe {
        let mine = handle as *mut u8;
        if read_i32(mine, ENT_CLASS_TYPE) != CLASS_MINE { return 0; }
        read_i32(mine, 0x118)
    }
}

/// A missile explosion can give Lua a missile or a former worm as its owner.
/// Never pass that pointer or its arbitrary team field to WA's hit handler:
/// WA indexes its team table directly with HIT_TEAM.
pub(crate) fn hit_sender(from: u32) -> (u32, i32) {
    if from == 0 {
        return (0, 0);
    }
    if !crate::actor::list_worms_pub(0).contains(&from) {
        log_line(&format!("hit sender {from:#x} is no longer a live worm; using neutral attribution"));
        return (0, 0);
    }
    let team = unsafe { read_i32(from as *mut u8, ENT_WORM_TEAM) };
    if !(1..=MAX_TEAMS as i32).contains(&team) {
        log_line(&format!("hit sender {from:#x} has invalid team {team}; using neutral attribution"));
        return (0, 0);
    }
    (from, team)
}

/// Send WA's SpecialImpact to a mine, crate or oil drum. The target owns its normal
/// damage and detonation path, including knockback and weapon attribution.
pub fn hit_object(handle: u32, from: u32, damage: i32, vx: i32, vy: i32, projectile: bool) -> bool {
    if handle == 0 || !(1..=1000).contains(&damage) { return false; }
    unsafe {
        let target = handle as *mut u8;
        if !matches!(read_i32(target, ENT_CLASS_TYPE), CLASS_MINE | CLASS_CRATE | CLASS_OIL) { return false; }
        let (from, team) = hit_sender(from);
        // PX sends a 0x408-byte M_GUNEXP buffer with flag 0. Stock oil drums
        // then take the direct-trigger path; crates receive the damage field.
        let mut data = [0i32; 0x408 / 4];
        // Projectile impacts use stock flag 3; electrical direct hits keep flag 0.
        // Mines receive this through their handler and discard HP damage themselves.
        data[0] = if projectile { 3 } else { 0 };
        data[1] = read_i32(target, ENT_POS_X);
        data[2] = read_i32(target, ENT_POS_Y);
        data[3] = vx;
        data[4] = vy;
        data[HIT_AMOUNT / 4] = damage;
        data[HIT_TEAM / 4] = team;
        let vtable = *(target as *const *const u32);
        if vtable.is_null() { return false; }
        let message: unsafe extern "thiscall" fn(*mut u8, u32, u32, u32, *mut u8) =
            std::mem::transmute(*vtable.add(2) as usize);
        message(target, from, MSG_HIT, 0x408, data.as_mut_ptr() as *mut u8);
        true
    }
}

/// Revive the worm tied to a gravestone. The pack binds graves to stable worm
/// IDs at death; WA's CrossEntity itself does not store that identity. This
/// uses the same allocator, setup slot and WormEntity constructor as
/// TeamEntity::Constructor in WA 3.8.1 (verified against OpenWA's bridge),
/// then removes the consumed grave through its own vtable.
/// Build worm `index` of `team` with WA's own constructor from its setup
/// entry, as the team's constructor does (0x555CC0): 0x41C bytes, the first
/// 0x3FC zeroed.
unsafe fn construct_worm(parent: *mut u8, team: u32, index: u32, setup: *mut u8) -> Option<*mut u8> {
    let malloc: unsafe extern "C" fn(usize) -> *mut u8 =
        std::mem::transmute(crate::wa::address("WA_MALLOC") as usize);
    let worm = malloc(0x41C);
    if worm.is_null() { return None; }
    std::ptr::write_bytes(worm, 0, 0x3FC);
    // The constructor may append the worm to TeamEntity's selection
    // roster. The team's constructor lists every index (1-8) up front, and a
    // dead worm stays listed, so restore the roster if the index was already
    // present rather than leaving a duplicate turn.
    let roster_count = read_i32(parent, 0x218).clamp(0, 100) as usize;
    let already_listed = (0..roster_count).any(|i| read_i32(parent, 0x88 + i * 4) == index as i32);
    let ctor: unsafe extern "stdcall" fn(*mut u8, *mut u8, u32, u32, *mut u8) -> *mut u8 =
        std::mem::transmute(crate::wa::address("WORM_ENTITY_CTOR") as usize);
    let built = ctor(worm, parent, team, index, setup);
    if built.is_null() || read_i32(worm, ENT_CLASS_TYPE) != CLASS_WORM { return None; }
    if already_listed {
        write_i32(parent, 0x218, roster_count as i32);
    }
    Some(worm)
}

/// A GameInfoWormSetup (0x28 bytes, copied whole to the worm at +0x110 by
/// its constructor) for a new worm: `template`'s (the team's first worm's),
/// with the worm's starting health (+0x00, u16; the label's counter starts
/// at it), its name (+0x03, 0x11 bytes) and a fixed place (+0x1E set, x and
/// y in pixels at +0x20 / +0x24), which makes the constructor stand the worm
/// there (0x50C040) instead of leaving it to be placed.
fn fill_setup(setup: &mut [u8; 0x28], template: &[u8; 0x28], name: &str, health: i32, x_px: i32, y_px: i32) {
    *setup = *template;
    setup[0..2].copy_from_slice(&(health.clamp(1, 9999) as u16).to_le_bytes());
    setup[3..3 + 0x11].fill(0);
    let bytes: Vec<u8> = name.bytes().filter(|b| (0x20..0x7F).contains(b)).take(0x10).collect();
    setup[3..3 + bytes.len()].copy_from_slice(&bytes);
    setup[0x1E] = 1;
    setup[0x20..0x24].copy_from_slice(&x_px.to_le_bytes());
    setup[0x24..0x28].copy_from_slice(&y_px.to_le_bytes());
}

/// `wa.worms.spawn{ team, x, y, name, health }`: a new worm for `team` in its
/// next unused place (Project X's RessurectWormEx at GetFreeWormIndex), built
/// by WA's own constructor and standing at (x, y), 16.16. The team's setup
/// record gains the worm (its count, GameInfo team record +0xBB4, which WA
/// reads during the match, e.g. counting living worms at 0x55C06F) and the
/// arena its health. An error when the team already has 8 worms.
pub fn spawn_worm(team: u32, x: i32, y: i32, name: &str, health: i32) -> Result<u32, String> {
    if !(1..=MAX_TEAMS).contains(&team) { return Err(format!("no team {team}")); }
    if !(1..=1000).contains(&health) { return Err(format!("health is 1 to 1000, got {health}")); }
    unsafe {
        let world = session_world_ptr();
        if world.is_null() { return Err("no match".into()); }
        let parent = team_entity(team as i32);
        if parent.is_null() { return Err(format!("no team {team} in this match")); }
        let info = read_i32(world, GAME_WORLD_GAME_INFO) as *mut u8;
        if info.is_null() { return Err("no match".into()); }
        let record = info.add(0x450 + (team as usize - 1) * 0xBB8);
        let count = *record.add(0xBB4) as u32;
        if count >= MAX_WORMS_PER_TEAM {
            return Err(format!("team {team} already has {MAX_WORMS_PER_TEAM} worms"));
        }
        if count == 0 { return Err(format!("team {team} has no worms")); }
        let index = count + 1;
        let setup = record.add(0xA74 + count as usize * 0x28);
        let template = *(record.add(0xA74) as *const [u8; 0x28]);
        let mut filled = [0u8; 0x28];
        fill_setup(&mut filled, &template, name, health, x >> 16, y >> 16);
        std::ptr::copy_nonoverlapping(filled.as_ptr(), setup, 0x28);
        *record.add(0xBB4) = index as u8;
        // Its health before the constructor, which writes the label from it.
        let hp = ARENA_WORM_HP + team as usize * WORLD_TEAM_STRIDE + index as usize * ARENA_WORM_STRIDE;
        write_i32(world, hp, health);
        let Some(worm) = construct_worm(parent, team, index, setup) else {
            *record.add(0xBB4) = count as u8;
            write_i32(world, hp, 0);
            return Err("WA's worm constructor refused".into());
        };
        write_i32(worm, ENT_POS_X, x);
        write_i32(worm, ENT_POS_Y, y);
        write_i32(worm, ENT_SPEED_X, 0);
        write_i32(worm, ENT_SPEED_Y, 0);
        Ok(worm as u32)
    }
}

pub fn revive_grave(grave_handle: u32, worm_id: u32, health: i32) -> u32 {
    if grave_handle == 0 || !(1..=1000).contains(&health) { return 0; }
    if grave_identity(grave_handle) != worm_id { return 0; }
    let team = worm_id / 16;
    let index = worm_id % 16;
    if !(1..=MAX_TEAMS).contains(&team) || !(1..=MAX_WORMS_PER_TEAM).contains(&index) { return 0; }
    unsafe {
        let grave = grave_handle as *mut u8;
        if read_i32(grave, ENT_CLASS_TYPE) != CLASS_GRAVE { return 0; }
        let world = read_i32(grave, ENT_WORLD) as *mut u8;
        if world.is_null() || world != session_world_ptr() { return 0; }
        let parent = team_entity(team as i32);
        if parent.is_null() { return 0; }
        let shared = read_i32(grave, ENT_SHARED_DATA) as *mut u8;
        if shared.is_null() || shared_lookup(shared, worm_id, SHARED_KEY_WORM).is_some() { return 0; }

        let info = read_i32(world, GAME_WORLD_GAME_INFO) as *mut u8;
        if info.is_null() { return 0; }
        // GameInfo::team_records starts at +0x450; each team record is 0xBB8,
        // and its eight 0x28-byte GameInfoWormSetup slots start at +0xA74.
        let record = info.add(0x450 + (team as usize - 1) * 0xBB8);
        if index > *record.add(0xBB4) as u32 { return 0; }
        let setup = record.add(0xA74 + (index as usize - 1) * 0x28);
        let Some(worm) = construct_worm(parent, team, index, setup) else { return 0 };

        let x = read_i32(grave, ENT_POS_X);
        let y = read_i32(grave, ENT_POS_Y).saturating_sub(5 << 16);
        write_i32(worm, ENT_POS_X, x);
        write_i32(worm, ENT_POS_Y, y);
        write_i32(worm, ENT_SPEED_X, 0);
        write_i32(worm, ENT_SPEED_Y, 0);
        let hp = ARENA_WORM_HP + team as usize * WORLD_TEAM_STRIDE
            + index as usize * ARENA_WORM_STRIDE;
        write_i32(world, hp, health);
        show_health(worm, health);
        // With worm placement in the scheme, WA's constructor leaves a worm
        // waiting to be placed (state 0x64, static, flagged awaiting
        // placement), which WA neither draws nor moves: at the start of a
        // match every worm waits so until it is placed. Leave it as the
        // constructor leaves a worm without placement: standing (0x65), not
        // static, placed. Standing, WA checks the ground under it itself;
        // not static, hits and blasts move it.
        let set_state: unsafe extern "thiscall" fn(*mut u8, u32) =
            std::mem::transmute(crate::wa::address("WORM_SET_STATE") as usize);
        crate::state_trace::with_cause("revive", || set_state(worm, WORM_STATE_STANDING));
        write_i32(worm, ENT_IS_STATIC, 0);
        write_i32(worm, WORM_AWAITING_PLACEMENT, 0);
        // TeamArena::logical_teams[team-1].eliminated_flag. A revived last
        // worm must put its team back into selection and score calculations.
        write_i32(world, GAME_WORLD_TEAM_ARENA + 8
            + (team as usize - 1) * WORLD_TEAM_STRIDE + 0x4FC, 0);

        let vtable = *(grave as *const *const u32);
        if !vtable.is_null() {
            let free: unsafe extern "thiscall" fn(*mut u8, u32) -> *mut u8 =
                std::mem::transmute(*vtable.add(1) as usize);
            free(grave, 1);
        }
        if let Ok(mut owners) = GRAVE_OWNERS.lock() {
            owners.retain(|(handle, _)| *handle != grave_handle);
        }
        worm as u32
    }
}

/// Team ammo for `slot`. `None` is a read. `Add` leaves infinite (`-1`)
/// alone; `Set` overwrites it. See `lua_host::AmmoChange`.
pub fn ammo(handle: u32, slot: u32, change: Option<lua_host::AmmoChange>) -> i32 {
    if handle == 0 || slot == 0 || slot >= STOCK_WEAPON_COUNT as u32 {
        return 0;
    }
    unsafe {
        let worm = handle as *mut u8;
        let world = read_i32(worm, ENT_WORLD) as *mut u8;
        if world.is_null() {
            return 0;
        }
        let arena = world.add(GAME_WORLD_TEAM_ARENA);
        let team = read_i32(worm, ENT_WORM_TEAM) as u32;
        let cur = get_ammo(arena, team, slot);
        let next = match change {
            None => return cur,
            Some(lua_host::AmmoChange::Add(_)) if cur < 0 => return cur,
            Some(lua_host::AmmoChange::Add(d)) => cur.saturating_add(d).max(0),
            Some(lua_host::AmmoChange::Set(n)) => n.max(-1),
        };
        // Anything but zero is usable -- infinite included -- so the slot must
        // be selectable and not held back by a scheme delay.
        if next != 0 {
            enable_slot(world, slot);
            clear_delay(arena, team, slot);
        }
        write_ammo(arena, team, slot, next);
        mark_panel_dirty(world);
        get_ammo(arena, team, slot)
    }
}

/// Ask WA to rebuild the weapon panel on its next update.
///
/// Each panel row lists only the weapons with ammo, packed left to right, so
/// every ammo, delay or availability change reflows it. WA only rebuilds on
/// its dirty flag, and only its own message paths set that; our writes go
/// straight into the ammo table, so without this the panel kept showing the
/// old contents until some stock event happened to dirty it. PX needed the
/// same nudge: its scripts re-set a weapon icon to itself after every ammo
/// change to force a rebuild. Display state only, like the write it follows.
unsafe fn mark_panel_dirty(world: *mut u8) {
    let panel = read_i32(world, GAME_WORLD_WEAPON_PANEL) as *mut u8;
    if !panel.is_null() {
        write_i32(panel, WEAPON_PANEL_DIRTY, 1);
    }
}

/// The ammo/delay row a team uses, read exactly where WA's own
/// `TeamArena__GetAmmo` / `AddAmmo` read it. Teams are 1-based (the first team
/// is 1, as in the worm's `ENT_WORM_TEAM` and wkFOW's team records). This row
/// is independent of the visible coalition colour.
unsafe fn ammo_row(arena: *mut u8, team: u32) -> Option<usize> {
    if arena.is_null() || team == 0 || team > MAX_TEAMS {
        return None;
    }
    let ammo_group = read_i32(
        arena,
        TEAM_ARENA_AMMO_GROUP + team as usize * WORLD_TEAM_STRIDE,
    );
    if ammo_group < 0 || ammo_group >= MAX_TEAMS as i32 {
        return None;
    }
    Some(ammo_group as usize * TEAM_AMMO_STRIDE)
}

unsafe fn ammo_index(arena: *mut u8, team: u32, slot: u32) -> Option<usize> {
    Some(ammo_row(arena, team)? + slot as usize)
}

unsafe fn write_ammo(arena: *mut u8, team: u32, slot: u32, ammo: i32) {
    if let Some(idx) = ammo_index(arena, team, slot) {
        write_i32(arena, TEAM_ARENA_AMMO + idx * 4, ammo);
    }
}

/// `WormEntity__StartFiring` — EAX=this. Power bar starts at 0 and grows while held.
unsafe fn start_firing(worm: *mut u8) {
    write_i32(worm, WORM_WEAPONS_ENABLED, 1);
    let f = crate::wa::address("WORM_START_FIRING") as u32;
    let args = [worm as u32, f];
    core::arch::asm!(
        "push esi",
        "push edi",
        "mov eax, dword ptr [ebx]",
        "call dword ptr [ebx + 4]",
        "pop edi",
        "pop esi",
        in("ebx") args.as_ptr(),
        out("eax") _,
        out("ecx") _,
        out("edx") _,
    );
}

/// Space-up: stock `ReleaseWeapon` (msg 0x27) then `StartFiring`'s sibling launch.
unsafe fn release_shot(worm: *mut u8) {
    write_i32(worm, WORM_WEAPONS_ENABLED, 1);
    type MsgFn = unsafe extern "thiscall" fn(*mut u8, u32, u32, u32, *mut u8);
    let f: MsgFn = std::mem::transmute(crate::wa::address("WORM_HANDLE_MESSAGE") as usize);
    f(
        worm,
        worm as u32,
        MSG_RELEASE_WEAPON,
        0,
        core::ptr::null_mut(),
    );
}

unsafe fn enable_slot(world: *mut u8, slot: u32) {
    let table = *(world.add(GAME_WORLD_WEAPON_TABLE) as *const *mut u8);
    if table.is_null() {
        return;
    }
    let entry = table.add(slot as usize * WEAPON_ENTRY_SIZE);
    write_i32(entry, WEAPON_DEFINED, 1);
    write_i32(entry, WEAPON_ENABLED, 1);
    write_i32(entry, WEAPON_AVAILABILITY, 1);
    // `panel_state` is the weapon's panel row. Stock slots already have one
    // from `AssignWeaponPanelSlots` (0x537130), and writing 1 moved every
    // granted weapon into the F1 row. Only a slot with no row yet gets one.
    if read_i32(entry, WEAPON_PANEL_STATE) < 0 {
        write_i32(entry, WEAPON_PANEL_STATE, 1);
    }
}

unsafe fn clear_delay(arena: *mut u8, team: u32, slot: u32) {
    if let Some(idx) = ammo_index(arena, team, slot) {
        write_i32(arena, TEAM_ARENA_DELAY + idx * 4, 0);
    }
}

unsafe fn get_ammo(arena: *mut u8, team: u32, slot: u32) -> i32 {
    let f = crate::wa::address("TEAM_GET_AMMO") as u32;
    let args = [team, arena as u32, slot, f];
    let ret: i32;
    core::arch::asm!(
        "push esi",
        "mov eax, dword ptr [ebx]",
        "mov esi, dword ptr [ebx + 4]",
        "mov edx, dword ptr [ebx + 8]",
        "call dword ptr [ebx + 12]",
        "pop esi",
        in("ebx") args.as_ptr(),
        lateout("eax") ret,
        out("ecx") _,
        out("edx") _,
    );
    ret
}

unsafe fn add_ammo(arena: *mut u8, team: u32, slot: u32, amount: i32) {
    let f = crate::wa::address("TEAM_ADD_AMMO") as u32;
    let args = [team, amount as u32, slot, arena as u32, f];
    core::arch::asm!(
        "push esi",
        "mov eax, dword ptr [ebx]",
        "mov edx, dword ptr [ebx + 4]",
        "push dword ptr [ebx + 8]",
        "push dword ptr [ebx + 12]",
        "call dword ptr [ebx + 16]",
        "pop esi",
        in("ebx") args.as_ptr(),
        out("eax") _,
        out("ecx") _,
        out("edx") _,
    );
}

unsafe fn select_weapon(worm: *mut u8, slot: u32, ammo: i32) {
    let f = crate::wa::address("WORM_SELECT_WEAPON") as u32;
    let args = [worm as u32, ammo as u32, slot, f];
    core::arch::asm!(
        "push esi",
        "push edi",
        "mov edi, dword ptr [ebx]",
        "push dword ptr [ebx + 4]",
        "push dword ptr [ebx + 8]",
        "call dword ptr [ebx + 12]",
        "pop edi",
        "pop esi",
        in("ebx") args.as_ptr(),
        out("eax") _,
        out("ecx") _,
        out("edx") _,
    );
}

fn client_mouse() -> Option<(i32, i32, i32, i32)> {
    let world = session_world();
    if world.is_null() {
        return None;
    }
    unsafe {
        let display = *(world.add(GAME_WORLD_DISPLAY) as *const *mut u8);
        if display.is_null() {
            return None;
        }
        let hwnd = *(display.add(DISPLAY_HWND) as *const windows_sys::Win32::Foundation::HWND);
        if hwnd.is_null() {
            return None;
        }
        let mut rc = windows_sys::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc) == 0 {
            return None;
        }
        let cw = rc.right - rc.left;
        let ch = rc.bottom - rc.top;
        if cw <= 0 || ch <= 0 {
            return None;
        }
        let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
        if windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt) == 0 {
            return None;
        }
        if windows_sys::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt) == 0 {
            return None;
        }
        Some((pt.x, pt.y, cw, ch))
    }
}

/// WA's cavern flag for the current match. False when there is no world.
pub fn is_cavern() -> bool {
    let world = session_world();
    if world.is_null() {
        return false;
    }
    unsafe { read_i32(world, GAME_WORLD_IS_CAVERN) != 0 }
}

/// WA's gravity and wind, 16.16: the collision manager's +0x22C / +0x230,
/// the base WA's physics scale by each object's own factors. Project X's
/// `Env->Gravity` / `Env->Wind` are these: its `CEnv` is the manager 0x108
/// on (`Water`, `Gravity`, `Wind` at 0x330 / 0x334 / 0x338).
pub fn forces() -> (i32, i32) {
    let world = session_world();
    if world.is_null() {
        return (0, 0);
    }
    unsafe {
        let cm = read_i32(world, GAME_WORLD_COLLISION_MANAGER) as *mut u8;
        game::entity::read_cm_forces(cm)
    }
}

/// Set the match's gravity (GameInfo +0xD9A8, the scheme's). WA recomputes
/// the collision manager's +0x22C from it every frame
/// (`TurnManager__ProcessFrame`, 0x55FDA0) as gravity x the low-gravity
/// percentage / 100, so a write to +0x22C alone lasts until the turn manager
/// runs; this sets both, the same way. Project X writes `Env->Gravity`.
pub fn set_gravity(v: i32) {
    let world = session_world();
    if world.is_null() {
        return;
    }
    unsafe {
        let info = read_i32(world, game::offsets::GAME_WORLD_GAME_INFO) as *mut u8;
        let cm = read_i32(world, GAME_WORLD_COLLISION_MANAGER) as *mut u8;
        if info.is_null() || cm.is_null() {
            return;
        }
        game::entity::write_i32(info, game::offsets::GAME_INFO_GRAVITY, v);
        let pct = read_i32(world, game::offsets::GAME_WORLD_GRAVITY_PCT);
        game::entity::write_i32(cm, game::offsets::CM_PARTICLE_GRAVITY_Y, v.wrapping_mul(pct) / 100);
    }
}

/// Set WA's wind as WA's own wind change does (`RandomiseWind`, 0x55BC80):
/// message 0x54 to the object under shared key 0x14, a 0x408-byte payload
/// whose first dword is the wind. Its handler (0x5541A1) stores it into the
/// collision manager's +0x230 and passes it on, so the clouds and the wind
/// bar follow (Project X's `M_UPDATEWIND`).
pub fn set_wind(v: i32) {
    unsafe {
        let root = session_root_ptr();
        if root.is_null() {
            return;
        }
        let Some(target) = crate::actor::shared_object(root, game::offsets::SHARED_KEY_WIND) else {
            return;
        };
        let mut data = [0u8; 0x408];
        data[..4].copy_from_slice(&v.to_le_bytes());
        let vt = *(target as *const *const u32);
        let handle: unsafe extern "thiscall" fn(*mut u8, u32, u32, u32, *mut u8) =
            std::mem::transmute(*vt.add(2) as usize);
        handle(target, root as u32, game::offsets::MSG_SET_WIND, data.len() as u32, data.as_mut_ptr());
    }
}

pub fn water_fixed() -> i32 {
    let world = session_world();
    if world.is_null() {
        return 0;
    }
    unsafe { read_i32(world, GAME_WORLD_WATER_LEVEL) << 16 }
}

pub fn list_kind(class: i32, from: u32) -> Vec<u32> {
    unsafe { walk_class(class, from as *mut u8) }
}

pub fn play_sound(handle: u32, id: i32, flags: i32) {
    if handle == 0 {
        return;
    }
    unsafe { play_sound_local(handle as *mut u8, id, flags, 0x10000, 0x10000) }
}

pub fn session_world() -> *mut u8 {
    unsafe { session_world_ptr() }
}

struct GridView {
    bits: *mut u8,
    stride: usize,
    width: usize,
    height: usize,
    bpp: u32,
}

impl GridView {
    fn solid_px(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 {
            return false;
        }
        pixel_solid(
            self.bits,
            self.stride,
            self.width,
            self.height,
            self.bpp,
            x as usize,
            y as usize,
        )
    }

    /// Set or clear one collision bit. Returns the previous solid state, or
    /// `None` if out of bounds.
    fn set_solid_px(&self, x: i32, y: i32, solid: bool) -> Option<bool> {
        if x < 0 || y < 0 {
            return None;
        }
        let x = x as usize;
        let y = y as usize;
        if x >= self.width || y >= self.height || self.bits.is_null() {
            return None;
        }
        if self.bpp >= 8 {
            let i = y.saturating_mul(self.stride).saturating_add(x);
            let prev = unsafe { *self.bits.add(i) != 0 };
            unsafe {
                *self.bits.add(i) = if solid { 1 } else { 0 };
            }
            return Some(prev);
        }
        let i = y.saturating_mul(self.stride).saturating_add(x >> 3);
        let mask = 1u8 << (x & 7);
        let prev = unsafe { (*self.bits.add(i) & mask) != 0 };
        unsafe {
            if solid {
                *self.bits.add(i) |= mask;
            } else {
                *self.bits.add(i) &= !mask;
            }
        }
        Some(prev)
    }
}

fn live_grid() -> Option<GridView> {
    unsafe { read_grid() }
}

/// Stamp / clear a walk-mask pixel. Used by actor `mask` silhouettes so stock
/// worm walk and hitscan see GIF/PNG solids as dirt.
pub fn set_collision_px(x: i32, y: i32, solid: bool) -> Option<bool> {
    live_grid()?.set_solid_px(x, y, solid)
}

unsafe fn read_grid() -> Option<GridView> {
    let world = session_world_ptr();
    if world.is_null() {
        return None;
    }
    let land = *(world.add(GAME_WORLD_LANDSCAPE) as *const *mut u8);
    if land.is_null() {
        return None;
    }
    let grid = *(land.add(LANDSCAPE_COLLISION) as *const *mut u8);
    if grid.is_null() {
        return None;
    }
    let bits = *(grid.add(BITGRID_BITS) as *const *mut u8);
    let stride = read_i32(grid, BITGRID_STRIDE) as usize;
    let mut w = read_i32(grid, BITGRID_W);
    let mut h = read_i32(grid, BITGRID_H);
    if w <= 0 || h <= 0 {
        w = read_i32(world, GAME_WORLD_MAP_W);
        h = read_i32(world, GAME_WORLD_MAP_H);
    }
    // A sanity bound only: WA's maps can be well over 4096 pixels tall, and
    // with a 4096 bound such a map read as no map at all (wa.world.size,
    // trace, land.solid and the actor cull all saw nothing).
    if bits.is_null() || stride == 0 || w <= 0 || h <= 0 || w > 0x8000 || h > 0x8000 {
        return None;
    }
    // +0x914 is the walk mask (FUN_0057cc90). +0x910/+0x91C are paint. +0x0D0 is plot scratch.
    Some(GridView {
        bits,
        stride,
        width: w as usize,
        height: h as usize,
        bpp: 1,
    })
}

fn pixel_solid(
    bits: *const u8,
    stride: usize,
    width: usize,
    height: usize,
    bpp: u32,
    x: usize,
    y: usize,
) -> bool {
    if x >= width || y >= height || bits.is_null() {
        return false;
    }
    if bpp >= 8 {
        let i = y.saturating_mul(stride).saturating_add(x);
        return unsafe { *bits.add(i) != 0 };
    }
    let i = y.saturating_mul(stride).saturating_add(x >> 3);
    let bit = unsafe { *bits.add(i) };
    (bit >> (x & 7)) & 1 != 0
}

fn trace_pixels(
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    solid: impl Fn(i32, i32) -> bool,
) -> Option<(i32, i32)> {
    let mut x = x0;
    let mut y = y0;
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    for _ in 0..4096 {
        if solid(x, y) {
            return Some((x, y));
        }
        if x == x1 && y == y1 {
            return None;
        }
        let e2 = err * 2;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
    None
}

unsafe fn walk_class(class: i32, from: *mut u8) -> Vec<u32> {
    let root = if from.is_null() {
        session_root_ptr()
    } else {
        climb_root(from)
    };
    if root.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut q = VecDeque::new();
    q.push_back(root);
    let mut n = 0usize;
    while let Some(node) = q.pop_front() {
        n += 1;
        if n > MAX_WALK || node.is_null() {
            break;
        }
        if read_i32(node, ENT_CLASS_TYPE) == class {
            out.push(node as u32);
        }
        let watermark = read_i32(node, ENT_CHILDREN_WATERMARK) as usize;
        let data = read_i32(node, ENT_CHILDREN_DATA) as *const u32;
        if data.is_null() || watermark == 0 {
            continue;
        }
        let cap = watermark.min(MAX_CHILDREN);
        for i in 0..cap {
            let child = *data.add(i);
            if child != 0 {
                q.push_back(child as *mut u8);
            }
        }
    }
    out
}

unsafe fn climb_root(from: *mut u8) -> *mut u8 {
    let mut p = from;
    for _ in 0..32 {
        if p.is_null() {
            return session_root_ptr();
        }
        let parent = read_i32(p, ENT_PARENT) as *mut u8;
        if parent.is_null() {
            return p;
        }
        p = parent;
    }
    session_root_ptr()
}

/// The retreat countdown in ms, 0 before retreat starts (or with no match).
/// The turn clock in ms left (root +0x18C, the timer the engine already
/// reads to let go of held worms). Traced in replays: it stands still while
/// WA's turn-ended flag (root +0x150) is up after a shot, about 120 frames
/// each time, and resumes when everything has settled.
pub fn turn_clock_ms() -> i32 {
    unsafe {
        let root = session_root_ptr();
        if root.is_null() {
            return 0;
        }
        read_i32(root, game::offsets::WORLD_ROOT_TURN_TIMER)
    }
}

pub fn retreat_ms() -> i32 {
    unsafe {
        let root = session_root_ptr();
        if root.is_null() {
            return 0;
        }
        read_i32(root, game::offsets::WORLD_ROOT_RETREAT_TIMER).max(0)
    }
}

unsafe fn session_root_ptr() -> *mut u8 {
    let slot = crate::wa::address("G_GAME_SESSION") as *const *mut u8;
    if slot.is_null() {
        return std::ptr::null_mut();
    }
    let session = *slot;
    if session.is_null() {
        return std::ptr::null_mut();
    }
    let runtime = *(session.add(GAME_SESSION_RUNTIME) as *const *mut u8);
    if runtime.is_null() {
        return std::ptr::null_mut();
    }
    *(runtime.add(GAME_RUNTIME_WORLD_ROOT) as *const *mut u8)
}

pub(crate) unsafe fn session_world_ptr() -> *mut u8 {
    let root = session_root_ptr();
    if root.is_null() {
        return std::ptr::null_mut();
    }
    read_i32(root, ENT_WORLD) as *mut u8
}

fn load_wav_file(slot: i32, path: &std::path::Path) -> bool {
    let world = session_world();
    if world.is_null() {
        log_error("LoadWav: no GameWorld");
        return false;
    }
    let sound = unsafe { *(world.add(GAME_WORLD_SOUND) as *const *mut u8) };
    if sound.is_null() {
        log_error("LoadWav: no DSSound");
        return false;
    }
    let Some(path_str) = path.to_str() else {
        log_error(&format!("LoadWav: non-utf8 path {}", path.display()));
        return false;
    };
    let Ok(c_path) = CString::new(path_str) else {
        log_error(&format!("LoadWav: bad path {}", path.display()));
        return false;
    };
    let ok = unsafe { dssound_load_wav(sound, slot, c_path.as_ptr()) };
    if ok {
        log_line(&format!("LoadWav slot {slot} {}", path.display()));
    } else {
        log_error(&format!("LoadWav failed slot {slot} {}", path.display()));
    }
    ok
}

/// DSSound__LoadWav (0x573FF0): thiscall ECX=this, stack slot_idx + path.
unsafe fn dssound_load_wav(this: *mut u8, slot: i32, path: *const i8) -> bool {
    if this.is_null() || path.is_null() {
        return false;
    }
    let f = crate::wa::address("DS_SOUND_LOAD_WAV") as u32;
    let args = [this as u32, slot as u32, path as u32, f];
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let ret: u32;
        core::arch::asm!(
            "push esi",
            "mov ecx, dword ptr [ebx]",
            "push dword ptr [ebx + 8]",
            "push dword ptr [ebx + 4]",
            "call dword ptr [ebx + 12]",
            "pop esi",
            in("ebx") args.as_ptr(),
            lateout("eax") ret,
            out("ecx") _,
            out("edx") _,
        );
        ret
    }));
    match result {
        Ok(n) => n != 0,
        Err(_) => {
            log_error("LoadWav panic");
            false
        }
    }
}

/// Whether `entity` carries the sound emitter WA's WorldEntity constructor
/// embeds at +0xE8 (0x4FED8A), the one PlaySoundLocal calls through. A
/// missile's owner, for one, has none: a pack that played a sound on an
/// explosion's `ev.worm` made WA call through garbage. Without the locator
/// the check cannot be made and every entity passes, as before it.
unsafe fn has_sound_emitter(entity: *mut u8) -> bool {
    match crate::wa::try_address("SOUND_EMITTER_VTABLE") {
        Some(vtable) => read_i32(entity, ENT_SOUND_EMITTER) as u32 == vtable,
        None => true,
    }
}

/// PlaySoundLocal (0x4FDFE0): EAX=pitch, ECX=volume, EDI=entity, stack id+flags, RET 8.
unsafe fn play_sound_local(entity: *mut u8, id: i32, flags: i32, volume: i32, pitch: i32) {
    if entity.is_null() {
        return;
    }
    if !has_sound_emitter(entity) {
        static REPORTED: AtomicBool = AtomicBool::new(false);
        if !REPORTED.swap(true, Ordering::Relaxed) {
            log_error(&format!(
                "sound.play: {:#x} is not a game entity (no sound emitter); not played",
                entity as u32
            ));
        }
        return;
    }
    let f = crate::wa::address("PLAY_SOUND_LOCAL") as u32;
    let args = [
        entity as u32,
        pitch as u32,
        volume as u32,
        id as u32,
        flags as u32,
        f,
    ];
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ret: u32;
        core::arch::asm!(
            "push esi",
            "push edi",
            "mov edi, dword ptr [ebx]",
            "mov eax, dword ptr [ebx + 4]",
            "mov ecx, dword ptr [ebx + 8]",
            "push dword ptr [ebx + 16]",
            "push dword ptr [ebx + 12]",
            "call dword ptr [ebx + 20]",
            "pop edi",
            "pop esi",
            in("ebx") args.as_ptr(),
            lateout("eax") _ret,
            out("ecx") _,
            out("edx") _,
        );
    }));
    if result.is_err() {
        log_error("PlaySoundLocal panic");
    }
}

/// Make `worm`'s label show `health` from its next frame, through WA's own
/// counter: the target is the health and the shown value half a point off
/// it, so WA's count (0x5147C0) snaps to the target and writes the label
/// text with its own formatter. A revived worm needs it: the constructor
/// starts the shown value at the scheme's starting health but writes the
/// label from the health it had then (0, before revive sets it), and WA only
/// raises a label at once, never lowers it before the turn commits.
unsafe fn show_health(worm: *mut u8, health: i32) {
    let target = health.clamp(0, 9999) << 16;
    write_i32(worm, WORM_HEALTH_TARGET, target);
    write_i32(worm, WORM_HEALTH_SHOWN, target + 0x8000);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_worms_setup_is_its_teams_with_its_name_health_and_place() {
        let mut template = [0u8; 0x28];
        template[0..2].copy_from_slice(&100u16.to_le_bytes());
        template[3..3 + 6].copy_from_slice(b"Boggy\0");
        template[0x18] = 0x5A; // a field after the name, which the template keeps
        let mut setup = [0xFFu8; 0x28];
        fill_setup(&mut setup, &template, "Reinforcement", 200, 640, -12);
        assert_eq!(u16::from_le_bytes([setup[0], setup[1]]), 200);
        assert_eq!(&setup[3..3 + 14], b"Reinforcement\0");
        assert!(setup[3 + 13..3 + 0x11].iter().all(|&b| b == 0), "the rest of the name cleared");
        assert_eq!(setup[0x18], 0x5A);
        assert_eq!(setup[0x1E], 1, "a fixed place: no waiting to be placed");
        assert_eq!(i32::from_le_bytes(setup[0x20..0x24].try_into().unwrap()), 640);
        assert_eq!(i32::from_le_bytes(setup[0x24..0x28].try_into().unwrap()), -12);
        fill_setup(&mut setup, &template, "A very long reinforcement name", 5, 0, 0);
        assert_eq!(&setup[3..3 + 0x10], b"A very long rein", "16 characters at most");
        assert_eq!(setup[3 + 0x10], 0, "and a terminating zero");
    }

    #[test]
    fn a_revived_worms_label_counts_to_its_health_next_frame() {
        let mut worm = vec![0u8; 0x41C];
        let p = worm.as_mut_ptr();
        unsafe {
            write_i32(p, WORM_HEALTH_SHOWN, 100 << 16);
            write_i32(p, WORM_HEALTH_TARGET, 100 << 16);
            show_health(p, 10);
            assert_eq!(read_i32(p, WORM_HEALTH_TARGET), 10 << 16);
            let shown = read_i32(p, WORM_HEALTH_SHOWN);
            assert_ne!(shown, 10 << 16, "they differ, so WA rewrites the label");
            assert_eq!(shown >> 16, 10, "but its whole part is already the health");
            assert!((shown - (10 << 16)).abs() < 0x10000, "close enough that WA snaps, not counts");
        }
    }

    fn pixel_solid_slice(
        packed: &[u8],
        stride: usize,
        width: usize,
        height: usize,
        bpp: u32,
        x: usize,
        y: usize,
    ) -> bool {
        if x >= width || y >= height {
            return false;
        }
        if bpp >= 8 {
            let i = y.saturating_mul(stride).saturating_add(x);
            return packed.get(i).copied().unwrap_or(0) != 0;
        }
        let i = y.saturating_mul(stride).saturating_add(x >> 3);
        packed
            .get(i)
            .map(|b| (b >> (x & 7)) & 1 != 0)
            .unwrap_or(false)
    }

    #[test]
    fn byte_plane_pixel() {
        let mut packed = vec![0u8; 8 * 8];
        packed[3 * 8 + 2] = 1;
        assert!(pixel_solid_slice(&packed, 8, 8, 8, 8, 2, 3));
        assert!(!pixel_solid_slice(&packed, 8, 8, 8, 8, 1, 1));
    }

    #[test]
    fn bit_plane_pixel() {
        let mut packed = vec![0u8; 8];
        packed[0] = 1 << 2;
        assert!(pixel_solid_slice(&packed, 1, 8, 1, 1, 2, 0));
        assert!(!pixel_solid_slice(&packed, 1, 8, 1, 1, 1, 0));
    }

    #[test]
    fn trace_hits_first_solid() {
        let hit = trace_pixels(0, 0, 5, 0, |x, _| x == 3);
        assert_eq!(hit, Some((3, 0)));
        assert_eq!(trace_pixels(0, 0, 2, 0, |_, _| false), None);
    }
}


pub fn solid_sampler() -> Option<impl Fn(i32, i32) -> bool> {
    let grid = live_grid()?;
    Some(move |x: i32, y: i32| grid.solid_px(x, y))
}
