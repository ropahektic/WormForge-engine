//! WA 3.8.1 D3D9 shader lifecycle and optional RGBA composite.
//! The queue captures color/coverage independently of the shared palette. The
//! final draw uses it only when upload succeeds; all failures retain stock art.

use crate::log_error;
#[cfg(debug_assertions)]
use crate::log_line;
use std::{
    ffi::c_void,
    sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering},
};

/// Shader renderer layout (3.8.1). Offsets from the renderer object.
const OFF_WIDTH: usize = 0x1C;
const OFF_HEIGHT: usize = 0x20;
const OFF_DEVICE: usize = 0x40;
// Read only by the debug probe log, but kept unconditionally: they are the
// map of this struct, and a future reader looking for the palette texture
// should find it here rather than in a disassembler.
#[cfg_attr(not(debug_assertions), allow(dead_code))]
const OFF_FRAME_TEXTURE: usize = 0x7C;
#[cfg_attr(not(debug_assertions), allow(dead_code))]
const OFF_PIXEL_SHADER: usize = 0x84;
#[cfg_attr(not(debug_assertions), allow(dead_code))]
const OFF_PALETTE_TEXTURE: usize = 0x88;

const PROBE_WAITING: u32 = 0;
const PROBE_READY: u32 = 1;
const PROBE_FAILED: u32 = 2;

static ORIG_PRESENT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_DRAW: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_CLEANUP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static PROBE: AtomicU32 = AtomicU32::new(PROBE_WAITING);
/// Last renderer seen by the present hook. Later stages read their device and
/// textures from this rather than resolving it again.
static RENDERER: AtomicUsize = AtomicUsize::new(0);

/// WA's `Renderer` option (Options section, registry or INI as WA itself is
/// set up), read with WA's own reader, `COptions`'s int getter: a thiscall on
/// its config object taking (section, name, default).
/// The one renderer the GPU path draws with: its vtable (0x6767F8, built by
/// the case-4 constructor 0x59D020 in the frontend's renderer switch at
/// 0x4E829D) holds `Display__D3D9ShaderPresent`.
pub const RENDERER_D3D9_SHADER: i32 = 4;

/// The renderer WA is set to, or None when it is not stored (WA then picks 4
/// on a capable machine, else 1) or this is not the WA build we know.
pub fn renderer_setting() -> Option<i32> {
    unsafe {
        let text = |va: u32, s: &[u8]| {
            std::slice::from_raw_parts(va as *const u8, s.len()) == s
        };
        if !text(crate::wa::address("STR_OPTIONS"), b"Options\0") || !text(crate::wa::address("STR_RENDERER"), b"Renderer\0") {
            return None;
        }
        type ReadInt = unsafe extern "thiscall" fn(*mut u8, *const u8, *const u8, i32) -> i32;
        let read: ReadInt = std::mem::transmute(crate::wa::address("OPTIONS_READ_INT") as usize);
        let v = read(
            crate::wa::address("OPTIONS_CONFIG") as *mut u8,
            crate::wa::address("STR_OPTIONS") as *const u8,
            crate::wa::address("STR_RENDERER") as *const u8,
            -1,
        );
        (v >= 0).then_some(v)
    }
}

/// WA's name for a `Renderer` value, in the order of its option strings
/// (DIRECTDRAW8 .. OPENGL_SHADER). 4 is verified from the code; 0 and 1 are
/// the two CompatRenderer (DirectDraw) vtables and 5 is OpenGLCPU.
pub fn renderer_name(value: i32) -> &'static str {
    match value {
        0 => "DirectDraw 8-bit",
        1 => "DirectDraw 32-bit",
        2 => "Direct3D 7",
        3 => "Direct3D 9 (software)",
        4 => "Direct3D 9 (shader)",
        5 => "OpenGL (CPU palette)",
        6 => "OpenGL (shader)",
        _ => "unknown",
    }
}

/// One line for the log: which renderer WA is set to.
pub fn renderer_report() -> String {
    match renderer_setting() {
        Some(v) => format!("WA renderer setting: {v}, {}", renderer_name(v)),
        None => "WA renderer setting: not stored (WA's default: Direct3D 9 (shader) \
                 where supported, else DirectDraw 32-bit)"
            .to_string(),
    }
}

/// True once the renderer has been found and read successfully.
pub fn is_ready() -> bool {
    PROBE.load(Ordering::SeqCst) == PROBE_READY
}

/// The shader renderer object, or 0 when the shader path is not in use.
pub fn renderer() -> usize {
    if is_ready() {
        RENDERER.load(Ordering::SeqCst)
    } else {
        0
    }
}

/// Read the renderer once and report what it holds.
///
/// Every field is checked before it is trusted: a software renderer, a torn
/// object during the frontend transition, or a different WA build all show up
/// as a null or implausible value here rather than as a crash later.
fn probe(renderer: usize) {
    if PROBE.load(Ordering::SeqCst) != PROBE_WAITING {
        return;
    }
    if renderer == 0 {
        return;
    }
    let (width, height, device) = unsafe {
        (
            *((renderer + OFF_WIDTH) as *const u32),
            *((renderer + OFF_HEIGHT) as *const u32),
            *((renderer + OFF_DEVICE) as *const usize),
        )
    };
    if device == 0 || width == 0 || height == 0 || width > 16384 || height > 16384 {
        PROBE.store(PROBE_FAILED, Ordering::SeqCst);
        log_error(&format!(
            "shader renderer at {renderer:#x} did not read sanely (device {device:#x} {width}x{height}); \
             staying on the palette path"
        ));
        return;
    }
    RENDERER.store(renderer, Ordering::SeqCst);
    PROBE.store(PROBE_READY, Ordering::SeqCst);
    #[cfg(debug_assertions)]
    {
        log_line(&format!(
            "shader renderer {renderer:#x}: device {device:#x} surface {width}x{height}"
        ));
        // The palette texture is the whole reason this path is interesting: it is
        // where an index becomes a colour, and it is per-frame GPU state rather
        // than the shared CPU palette.
        let (frame, shader, palette) = unsafe {
            (
                *((renderer + OFF_FRAME_TEXTURE) as *const usize),
                *((renderer + OFF_PIXEL_SHADER) as *const usize),
                *((renderer + OFF_PALETTE_TEXTURE) as *const usize),
            )
        };
        log_line(&format!(
            "shader renderer textures: frame {frame:#x} palette {palette:#x} pixel_shader {shader:#x}"
        ));
    }
}

unsafe extern "C" fn on_shader_present(renderer: usize) {
    let _ = std::panic::catch_unwind(|| {
        probe(renderer);
        if is_ready() {
            crate::truecolor::present(*((renderer + OFF_DEVICE) as *const *mut c_void));
        }
    });
}

unsafe extern "C" fn on_shader_cleanup(renderer: usize) {
    // Release our default-pool textures before WA resets the shared device.
    if RENDERER.load(Ordering::SeqCst) == renderer {
        crate::gpu::release();
        crate::truecolor::release();
        RENDERER.store(0, Ordering::SeqCst);
        PROBE.store(PROBE_WAITING, Ordering::SeqCst);
        #[cfg(debug_assertions)]
        log_line("shader renderer released its resources; re-probing on next present");
    }
}

/// __fastcall(renderer in ECX, result buffer in EDX, framebuffer on stack),
/// with a third argument riding EAX. Forward its stack argument and preserve
/// registers/flags around callbacks. Restore our D3D state after the draw.
#[unsafe(naked)]
unsafe extern "C" fn shader_present_hook() {
    core::arch::naked_asm!(
        "pushfd",
        "pushad",
        "push ecx",
        "call {on_present}",
        "add esp, 4",
        "popad",
        "popfd",
        "push dword ptr [esp+4]",
        "call dword ptr [{original}]",
        "pushfd", "pushad",
        "call {after_present}",
        "popad", "popfd", "ret 4",
        after_present = sym after_shader_present,
        on_present = sym on_shader_present,
        original = sym ORIG_PRESENT,
    );
}

/// __thiscall(renderer in ECX), no stack arguments.
#[unsafe(naked)]
unsafe extern "C" fn shader_cleanup_hook() {
    core::arch::naked_asm!(
        "pushfd",
        "pushad",
        "push ecx",
        "call {on_cleanup}",
        "add esp, 4",
        "popad",
        "popfd",
        "jmp dword ptr [{original}]",
        on_cleanup = sym on_shader_cleanup,
        original = sym ORIG_CLEANUP,
    );
}

/// Runs after `D3D9ShaderPresent` returns — that is, after WA has ended its
/// own scene at 0x59C7A0 and before it Presents separately at 0x59C8A0. The
/// S0 probe confirmed a second balanced scene here survives to the front
/// buffer, which is what makes drawing GPU sprites at this point viable.
unsafe extern "C" fn after_shader_present() {
    crate::truecolor::restore();
    let renderer = RENDERER.load(Ordering::SeqCst);
    if renderer != 0 && is_ready() {
        let _ = std::panic::catch_unwind(|| {
            crate::gpu::present(*((renderer + OFF_DEVICE) as *const *mut c_void));
        });
    }
}

unsafe extern "C" fn on_shader_draw(renderer: usize) {
    let _ = std::panic::catch_unwind(|| {
        if renderer == RENDERER.load(Ordering::SeqCst) && is_ready() {
            crate::truecolor::bind(*((renderer + OFF_DEVICE) as *const *mut c_void));
        }
    });
}

#[unsafe(naked)]
unsafe extern "C" fn shader_draw_hook() {
    core::arch::naked_asm!(
        "pushfd", "pushad", "push edi", "call {before}", "add esp, 4",
        "popad", "popfd", "jmp dword ptr [{original}]",
        before = sym on_shader_draw, original = sym ORIG_DRAW,
    );
}

/// Install all three wraps as one optional capability.
///
/// Never fails the caller: a build without these sites, or a MinHook refusal,
/// means the shader path is unavailable and everything that depends on it stays
/// switched off. That is a supported configuration, not an error.
pub fn install() -> Result<(), String> {
    let required = [
        ("DISPLAY_SHADER_PRESENT", shader_present_hook as *mut c_void, &ORIG_PRESENT),
        ("DISPLAY_SHADER_CLEANUP", shader_cleanup_hook as *mut c_void, &ORIG_CLEANUP),
    ];
    if required.iter().any(|(name, _, _)| crate::wa::try_address(name).is_none()) {
        log_error("gpu present hook disabled: shader signatures unavailable");
        return Ok(());
    }
    let checkpoint = crate::wa::hook_checkpoint();
    for (name, detour, original) in required {
        let target = crate::wa::address(name) as *mut c_void;
        match unsafe { crate::wa::create_hook(target, detour) } {
            Ok(trampoline) => {
                original.store(trampoline, Ordering::SeqCst);
            }
            Err(e) => {
                crate::wa::rollback_to(checkpoint);
                log_error(&format!("gpu present hook disabled: {e}; GPU sprites will not draw"));
                return Ok(());
            }
        }
    }
    // Optional FogDraw (truecolor bind site)
    if let Some(target) = crate::wa::try_address("DISPLAY_SHADER_FOG_DRAW") {
        if let Ok(trampoline) =
            unsafe { crate::wa::create_hook(target as *mut c_void, shader_draw_hook as _) }
        {
            ORIG_DRAW.store(trampoline, Ordering::SeqCst);
        }
    }
    crate::log_line("shader renderer GPU present/cleanup hooks installed");
    Ok(())
}

#[cfg(test)]
mod renderer_name_tests {
    use super::{renderer_name, RENDERER_D3D9_SHADER};

    #[test]
    fn the_gpu_path_needs_the_d3d9_shader_renderer() {
        assert_eq!(renderer_name(RENDERER_D3D9_SHADER), "Direct3D 9 (shader)");
        assert_eq!(renderer_name(1), "DirectDraw 32-bit", "WA's fallback default");
        assert_eq!(renderer_name(6), "OpenGL (shader)");
        assert_eq!(renderer_name(7), "unknown");
    }
}
