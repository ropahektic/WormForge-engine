//! Presentation-only text overlay after stock GameRender wait CALL.

use crate::log_line;
use game::offsets::{
    DISPLAY_VIEW_X, DISPLAY_VIEW_Y, DISPLAY_VT_DRAW_SCALED, DISPLAY_WIDTH, GAME_RUNTIME_WORLD,
    GAME_SESSION_RUNTIME, GAME_WORLD_DISPLAY, GAME_WORLD_TEXTBOX, TEXTBOX_CACHED_SCALE,
    TEXTBOX_STYLE, TEXTBOX_STYLE_SMALL,
};
use lua_host::run_hud;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows_sys::Win32::System::Memory::{VirtualProtect, PAGE_EXECUTE_READWRITE};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

const PATCH_LEN: usize = 5;

static ORIG: AtomicUsize = AtomicUsize::new(0);
static WARNED: AtomicBool = AtomicBool::new(false);

type SetTextboxTextFn = unsafe extern "system" fn(
    textbox: *mut u8,
    msg: *const u8,
    font_index: i32,
    fill_color: i32,
    border_color: i32,
    out_w: *mut i32,
    out_h: *mut i32,
    scale: i32,
) -> usize;

type DrawScaledFn = unsafe extern "thiscall" fn(
    display: *mut u8,
    x: i32,
    y: i32,
    bitmap: usize,
    src_x: i32,
    src_y: i32,
    src_w: i32,
    src_h: i32,
    flags: u32,
);

pub fn install() -> Result<(), String> {
    if crate::wa::try_address("SET_TEXTBOX_TEXT").is_none() {
        return Err("HUD text function signature unavailable".into());
    }
    if crate::wa::try_address("GAME_RENDER_WAIT_CALL").is_none() {
        return Err("HUD call-site signature unavailable".into());
    }
    let site = crate::wa::check_code_pattern("GAME_RENDER_WAIT_CALL")? as usize;
    unsafe {
        let mut cur = [0u8; PATCH_LEN];
        std::ptr::copy_nonoverlapping(site as *const u8, cur.as_mut_ptr(), PATCH_LEN);
        if cur[0] != 0xE8 {
            log_line(&format!(
                "HUD call-site opcode {cur:02X?} — skip overlay (not fatal)"
            ));
            return Ok(());
        }
        let previous = (site as u32)
            .wrapping_add(5)
            .wrapping_add(i32::from_le_bytes(cur[1..5].try_into().unwrap()) as u32);
        if !crate::wa::executable(previous) {
            log_line(&format!(
                "HUD call target {previous:#010x} is not executable — skip overlay"
            ));
            return Ok(());
        }
        ORIG.store(previous as usize, Ordering::SeqCst);
        let hook = hooked_render_waiting as *const () as usize;
        let rel = (hook as isize - (site as isize + 5)) as i32;
        let mut patch = [0u8; PATCH_LEN];
        patch[0] = 0xE8;
        patch[1..5].copy_from_slice(&rel.to_le_bytes());
        let mut old = 0u32;
        if VirtualProtect(
            site as *const _,
            PATCH_LEN,
            PAGE_EXECUTE_READWRITE,
            &mut old,
        ) == 0
        {
            return Err("HUD VirtualProtect failed".into());
        }
        std::ptr::copy_nonoverlapping(patch.as_ptr(), site as *mut u8, PATCH_LEN);
        if FlushInstructionCache(GetCurrentProcess(), site as *const _, PATCH_LEN) == 0 {
            std::ptr::copy_nonoverlapping(cur.as_ptr(), site as *mut u8, PATCH_LEN);
            let _ = FlushInstructionCache(GetCurrentProcess(), site as *const _, PATCH_LEN);
            let mut ignored = 0u32;
            let _ = VirtualProtect(site as *const _, PATCH_LEN, old, &mut ignored);
            return Err("HUD FlushInstructionCache failed".into());
        }
        let mut ign = 0u32;
        let _ = VirtualProtect(site as *const _, PATCH_LEN, old, &mut ign);
    }
    log_line("HUD call-site wrap installed (presentation only)");
    Ok(())
}

unsafe extern "C" fn hooked_render_waiting() {
    let runtime: usize;
    unsafe {
        core::arch::asm!("mov {}, edi", out(reg) runtime);
        let orig = ORIG.load(Ordering::SeqCst);
        // WA's routine takes the runtime in EAX and may change EAX, ECX and
        // EDX, as a call does: all three are declared, so none is assumed to
        // survive it.
        core::arch::asm!(
            "call ecx",
            inout("eax") runtime => _,
            inout("ecx") orig => _,
            out("edx") _,
        );
    }
    // Once per rendered frame: where the profiler's frames end.
    crate::perf::frame();
    flush_hud();
}

fn flush_hud() {
    let _perf = crate::perf::probe(crate::perf::Section::Hud);
    let lines = run_hud().unwrap_or_default();
    let Some(world) = resolve_world() else {
        return;
    };
    let mut y = 10i32;
    for line in lines {
        if line.text.is_empty() {
            continue;
        }
        let draw_y = if line.y >= 0 { line.y } else { y };
        if blit_line(world, line.x, draw_y, &line.text, Line::default()) {
            y = draw_y + 14;
        } else {
            break;
        }
    }
    // `wa.perf.enable{ counter = true }`, at the top right.
    if let Some(text) = crate::perf::counter() {
        let small = Line { right: true, style: Some(TEXTBOX_STYLE_SMALL) };
        blit_line(world, 10, 6, &text, small);
    }
    let display = unsafe { *((world + GAME_WORLD_DISPLAY) as *const *mut u8) };
    crate::overlay::present(display);
    crate::overlay::clear();
}

pub(crate) fn resolve_world() -> Option<usize> {
    unsafe {
        let session = *(crate::wa::address("G_GAME_SESSION") as *const usize);
        if session == 0 {
            return None;
        }
        let runtime = *((session + GAME_SESSION_RUNTIME) as *const usize);
        if runtime == 0 {
            return None;
        }
        let world = *((runtime + GAME_RUNTIME_WORLD) as *const usize);
        (world != 0).then_some(world)
    }
}

#[derive(Clone, Copy, Default)]
struct Line {
    /// `screen_x` is the gap between the text's right edge and the screen's.
    right: bool,
    /// Bake in this font style rather than the world textbox's own, which is
    /// put back after.
    style: Option<i32>,
}

fn blit_line(world: usize, screen_x: i32, screen_y: i32, text: &str, how: Line) -> bool {
    unsafe {
        let display = *((world + GAME_WORLD_DISPLAY) as *const usize);
        let textbox = *((world + GAME_WORLD_TEXTBOX) as *const usize);
        if display == 0 || textbox == 0 {
            return false;
        }
        let Ok(c) = CString::new(text) else {
            return false;
        };
        let mut w = 0i32;
        let mut h = 0i32;
        let set_text: SetTextboxTextFn =
            std::mem::transmute(crate::wa::address("SET_TEXTBOX_TEXT") as usize);
        let style_at = (textbox + TEXTBOX_STYLE) as *mut i32;
        let own_style = *style_at;
        if let Some(style) = how.style {
            *style_at = style;
        }
        let bm = set_text(
            textbox as *mut u8,
            c.as_ptr() as *const u8,
            6,
            0,
            0,
            &mut w,
            &mut h,
            0x1_0000,
        );
        if how.style.is_some() {
            *style_at = own_style;
            // The cache does not compare the style: the next line redraws.
            *((textbox + TEXTBOX_CACHED_SCALE) as *mut i32) = -1;
        }
        if bm == 0 || w <= 0 || h <= 0 {
            if !WARNED.swap(true, Ordering::SeqCst) {
                log_line("HUD textbox bake failed");
            }
            return false;
        }
        let vt = *(display as *const usize);
        if vt == 0 {
            return false;
        }
        let slot = *((vt + DISPLAY_VT_DRAW_SCALED) as *const usize);
        if slot == 0 {
            return false;
        }
        let draw: DrawScaledFn = std::mem::transmute(slot);
        let screen_x = if how.right {
            *((display + DISPLAY_WIDTH) as *const i32) - screen_x - w
        } else {
            screen_x
        };
        let cam_x = *((display + DISPLAY_VIEW_X) as *const i32);
        let cam_y = *((display + DISPLAY_VIEW_Y) as *const i32);
        let x = ((screen_x - cam_x + w / 2) << 16) as i32;
        let y = ((screen_y - cam_y + h / 2) << 16) as i32;
        draw(display as *mut u8, x, y, bm, 0, 0, w, h, 0x10_0000);
        true
    }
}
