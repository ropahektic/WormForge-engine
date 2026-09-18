//! Live world queries: landscape solid (Blas Landscape+0x910), entity tree
//! (Jelly CTask children), PlaySoundLocal. Read-only except sound.

use crate::log::{log_error, log_line};
use crate::rebase::rb;
use game::entity::{read_i32, write_i32};
use game::offsets::*;
use std::collections::VecDeque;
use std::ffi::CString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
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

/// Live mouse in world 16.16, plus left button. Game thread only.
///
/// OS mouse (so a held drag tracks). Client pixels are scaled onto the
/// display so windowed / DPI stretch still covers the whole map.
pub fn pointer() -> (i32, i32, bool) {
    let down = mouse_down();
    let (cx, cy, vw, vh) = view_cam();
    if let Some((mx, my, cw, ch)) = client_mouse() {
        if vw > 0 && vh > 0 && cw > 0 && ch > 0 {
            let (dw, dh) = display_wh();
            let dw = if dw > 0 { dw } else { cw };
            let dh = if dh > 0 { dh } else { ch };
            let gx = (mx as i64 * dw as i64) / cw as i64;
            let gy = (my as i64 * dh as i64) / ch as i64;
            let x = cx
                .wrapping_sub((vw as i32) << 15)
                .wrapping_add((gx as i32) << 16);
            let y = cy
                .wrapping_sub((vh as i32) << 15)
                .wrapping_add((gy as i32) << 16);
            return (x, y, down);
        }
    }
    let world = session_world();
    if world.is_null() {
        return (0, 0, down);
    }
    unsafe {
        (
            read_i32(world, GAME_WORLD_LAND_CURSOR_X),
            read_i32(world, GAME_WORLD_LAND_CURSOR_Y),
            down,
        )
    }
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
    unsafe {
        windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(0x01) as u16 & 0x8000
            != 0
    }
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

fn display_wh() -> (i32, i32) {
    let world = session_world();
    if world.is_null() {
        return (0, 0);
    }
    unsafe {
        let display = *(world.add(GAME_WORLD_DISPLAY) as *const *mut u8);
        if display.is_null() {
            return (0, 0);
        }
        (
            *(display.add(DISPLAY_WIDTH) as *const i32),
            *(display.add(DISPLAY_HEIGHT) as *const i32),
        )
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
    bits: *const u8,
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
}

fn live_grid() -> Option<GridView> {
    unsafe { read_grid() }
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
    let grid = *(land.add(LANDSCAPE_SOLID_GRID) as *const *mut u8);
    if grid.is_null() {
        return None;
    }
    let bits = *(grid.add(BITGRID_BITS) as *const *const u8);
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
    let bpp = read_i32(grid, BITGRID_BPP) as u32;
    Some(GridView {
        bits,
        stride,
        width: w as usize,
        height: h as usize,
        bpp,
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
