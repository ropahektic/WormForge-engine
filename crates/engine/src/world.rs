//! Live world queries: landscape walk mask (Landscape+0x914), entity tree
//! (Jelly CTask children), PlaySoundLocal, stock SelectWeapon / StartFiring.

use crate::log::{log_error, log_line};
use crate::rebase::rb;
use game::latch::FrameLatch;
use game::entity::{read_i32, write_i32};
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
        let Some(wav) = g.iter_mut().find(|(k, _)| k == key || k.ends_with(key)).map(|(_, w)| w) else {
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

pub fn trace_fixed(x0: i32, y0: i32, x1: i32, y1: i32) -> Option<(i32, i32)> {
    let grid = live_grid()?;
    let hit = trace_pixels(
        x0 >> 16,
        y0 >> 16,
        x1 >> 16,
        y1 >> 16,
        |x, y| grid.solid_px(x, y),
    )?;
    Some((hit.0 << 16, hit.1 << 16))
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
        VIEW_CAM_X.store(*((display + DISPLAY_VIEW_X) as *const i32), Ordering::Relaxed);
        VIEW_CAM_Y.store(*((display + DISPLAY_VIEW_Y) as *const i32), Ordering::Relaxed);
        VIEW_SURF_W.store(*((display + DISPLAY_WIDTH) as *const i32), Ordering::Relaxed);
        VIEW_SURF_H.store(*((display + DISPLAY_HEIGHT) as *const i32), Ordering::Relaxed);
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

pub fn thaw_turn() {
    let root = unsafe { session_root_ptr() };
    let saved = FROZEN_TURN.lock().ok().and_then(|mut g| g.take());
    let Some(saved) = saved else {
        return;
    };
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
    let Some((timer, display)) = FROZEN_TURN.lock().ok().and_then(|g| {
        g.as_ref().map(|f| (f.timer, f.display))
    }) else {
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
        windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(vk) as u16 & 0x8000
            != 0
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
fn sim_frame() -> Option<i64> {
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
    let f = rb(GAME_WORLD_ADVANCE_RNG) as u32;
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

/// Check the prologue once, the way `render_queue` checks its dispatcher.
/// Calling a hardcoded address on a build whose layout moved would corrupt
/// the game; refusing to draw only costs the pack its randomness.
fn advance_rng_verified() -> bool {
    match RNG_OK.load(Ordering::Relaxed) {
        0 => {
            let addr = rb(GAME_WORLD_ADVANCE_RNG) as *const u8;
            let found = unsafe {
                std::slice::from_raw_parts(addr, ADVANCE_RNG_PROLOGUE.len())
            };
            let ok = found == ADVANCE_RNG_PROLOGUE;
            RNG_OK.store(if ok { 2 } else { 1 }, Ordering::Relaxed);
            if !ok {
                log_error(&format!(
                    "GameWorld__AdvanceRNG at {addr:?} does not start with the \
                     verified prologue (found {found:02x?}); wa.random() is \
                     disabled rather than calling into the wrong place"
                ));
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

/// Bits: 1 left, 2 right, 4 up, 8 down, 16 W, 32 sprint (shift), 64 Q, 128 Space (shoot).
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

/// Team ammo for `slot`. `delta = None` is a read. Infinite stays `-1`.
pub fn ammo(handle: u32, slot: u32, delta: Option<i32>) -> i32 {
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
        let Some(d) = delta else {
            return cur;
        };
        if cur < 0 {
            return cur;
        }
        let next = cur.saturating_add(d).max(0);
        if next > 0 {
            enable_slot(world, slot);
            clear_delay(arena, team, slot);
        }
        write_ammo(arena, team, slot, next);
        get_ammo(arena, team, slot)
    }
}

unsafe fn ammo_index(arena: *mut u8, team: u32, slot: u32) -> Option<usize> {
    if arena.is_null() || team >= MAX_TEAMS {
        return None;
    }
    let rec = arena.add(8 + team as usize * 0x51C);
    let alliance = read_i32(rec, 0x508);
    if alliance < 0 || alliance >= MAX_TEAMS as i32 {
        return None;
    }
    Some(alliance as usize * TEAM_AMMO_STRIDE + slot as usize)
}

unsafe fn write_ammo(arena: *mut u8, team: u32, slot: u32, ammo: i32) {
    if let Some(idx) = ammo_index(arena, team, slot) {
        write_i32(arena, TEAM_ARENA_AMMO + idx * 4, ammo);
    }
}

/// `WormEntity__StartFiring` — EAX=this. Power bar starts at 0 and grows while held.
unsafe fn start_firing(worm: *mut u8) {
    write_i32(worm, WORM_WEAPONS_ENABLED, 1);
    let f = rb(WORM_START_FIRING) as u32;
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
    let f: MsgFn = std::mem::transmute(rb(WORM_HANDLE_MESSAGE) as usize);
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
    write_i32(entry, WEAPON_PANEL_STATE, 1);
}

unsafe fn clear_delay(arena: *mut u8, team: u32, slot: u32) {
    if arena.is_null() || team >= MAX_TEAMS {
        return;
    }
    let rec = arena.add(8 + team as usize * 0x51C);
    let alliance = read_i32(rec, 0x508);
    if alliance < 0 || alliance >= MAX_TEAMS as i32 {
        return;
    }
    let idx = alliance as usize * TEAM_AMMO_STRIDE + slot as usize;
    write_i32(arena, TEAM_ARENA_DELAY + idx * 4, 0);
}

unsafe fn get_ammo(arena: *mut u8, team: u32, slot: u32) -> i32 {
    let f = rb(TEAM_GET_AMMO) as u32;
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
    let f = rb(TEAM_ADD_AMMO) as u32;
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
    let f = rb(WORM_SELECT_WEAPON) as u32;
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
    if bits.is_null() || stride == 0 || w <= 0 || h <= 0 || w > 4096 || h > 4096 {
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

unsafe fn session_root_ptr() -> *mut u8 {
    let slot = rb(G_GAME_SESSION) as *const *mut u8;
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

unsafe fn session_world_ptr() -> *mut u8 {
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
    let f = rb(DS_SOUND_LOAD_WAV) as u32;
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

/// PlaySoundLocal (0x4FDFE0): EAX=pitch, ECX=volume, EDI=entity, stack id+flags, RET 8.
unsafe fn play_sound_local(entity: *mut u8, id: i32, flags: i32, volume: i32, pitch: i32) {
    if entity.is_null() {
        return;
    }
    let f = rb(PLAY_SOUND_LOCAL) as u32;
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

#[cfg(test)]
mod tests {
    use super::*;

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
