//! WA's D3D9 shader renderer: detection and lifecycle.
//!
//! The map palette is shared by every pack, holds only what the map needed, and
//! cannot express a saturated tint: one measured live had 106 colours in use and
//! exactly two blues, both desaturated slate, so a blue sprite snapped to black.
//! Writing colours into it to fix that corrupts every other pack's art, because
//! an index means one colour for the whole frame.
//!
//! This renderer is the way out. It uploads the frame as palette INDICES plus a
//! palette texture and resolves colour on the GPU, so a pack can own the
//! index -> colour step for the indices it reserved while WA keeps doing
//! geometry, ordering and occlusion. No second renderer, and nothing the render
//! queue's depth work has to give back.
//!
//! This stage is deliberately visual-neutral: it detects the renderer, proves
//! the object is readable, and reports. Anything that changes on screen is a
//! bug. It is also entirely optional -- WA's software renderer has no such
//! object, so every failure here logs and leaves sprites rendering exactly as
//! they do today.

use crate::{log_error, log_line, rebase::rb};
use minhook::MinHook;
use re_catalog::Catalog;
use std::{
    ffi::c_void,
    sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering},
};

/// Shader renderer layout (3.8.1). Offsets from the renderer object.
const OFF_WIDTH: usize = 0x1C;
const OFF_HEIGHT: usize = 0x20;
const OFF_DEVICE: usize = 0x40;
const OFF_FRAME_TEXTURE: usize = 0x7C;
const OFF_PIXEL_SHADER: usize = 0x84;
const OFF_PALETTE_TEXTURE: usize = 0x88;

const PROBE_WAITING: u32 = 0;
const PROBE_READY: u32 = 1;
const PROBE_FAILED: u32 = 2;

static ORIG_PRESENT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_CLEANUP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static PROBE: AtomicU32 = AtomicU32::new(PROBE_WAITING);
/// Last renderer seen by the present hook. Later stages read their device and
/// textures from this rather than resolving it again.
static RENDERER: AtomicUsize = AtomicUsize::new(0);

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
    let (width, height, device, frame, shader, palette) = unsafe {
        (
            *((renderer + OFF_WIDTH) as *const u32),
            *((renderer + OFF_HEIGHT) as *const u32),
            *((renderer + OFF_DEVICE) as *const usize),
            *((renderer + OFF_FRAME_TEXTURE) as *const usize),
            *((renderer + OFF_PIXEL_SHADER) as *const usize),
            *((renderer + OFF_PALETTE_TEXTURE) as *const usize),
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
    log_line(&format!(
        "shader renderer {renderer:#x}: device {device:#x} surface {width}x{height}"
    ));
    // The palette texture is the whole reason this path is interesting: it is
    // where an index becomes a colour, and it is per-frame GPU state rather
    // than the shared CPU palette.
    log_line(&format!(
        "shader renderer textures: frame {frame:#x} palette {palette:#x} pixel_shader {shader:#x}"
    ));
}

unsafe extern "C" fn on_shader_present(renderer: usize) {
    let _ = std::panic::catch_unwind(|| probe(renderer));
}

unsafe extern "C" fn on_shader_cleanup(renderer: usize) {
    // WA is releasing its D3DPOOL_DEFAULT textures, so anything we create on
    // this device has to go with them. Nothing owns resources yet; forgetting
    // the renderer keeps a stale pointer from outliving the device.
    if RENDERER.load(Ordering::SeqCst) == renderer {
        RENDERER.store(0, Ordering::SeqCst);
        PROBE.store(PROBE_WAITING, Ordering::SeqCst);
        log_line("shader renderer released its resources; re-probing on next present");
    }
}

/// __fastcall(renderer in ECX, result buffer in EDX, framebuffer on stack),
/// with a third argument riding EAX. Preserve every register and flag, then
/// tail-jump to the trampoline so WA sees exactly the call it expected.
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
        "jmp dword ptr [{original}]",
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

/// Install the detection wraps.
///
/// Never fails the caller: a build without these sites, or a MinHook refusal,
/// means the shader path is unavailable and everything that depends on it stays
/// switched off. That is a supported configuration, not an error.
pub fn install(catalog: &Catalog) -> Result<(), String> {
    let Some(present_va) = catalog.function_va("Display__D3D9ShaderPresent") else {
        log_line("no Display__D3D9ShaderPresent in the catalogue; shader path disabled");
        return Ok(());
    };
    match unsafe {
        MinHook::create_hook(
            rb(present_va) as *mut c_void,
            shader_present_hook as *mut c_void,
        )
    } {
        Ok(trampoline) => ORIG_PRESENT.store(trampoline, Ordering::SeqCst),
        Err(e) => {
            log_error(&format!("shader present hook: {e:?}; shader path disabled"));
            return Ok(());
        }
    }

    if let Some(cleanup_va) = catalog.function_va("Display__D3D9ShaderResourcesCleanup") {
        match unsafe {
            MinHook::create_hook(
                rb(cleanup_va) as *mut c_void,
                shader_cleanup_hook as *mut c_void,
            )
        } {
            Ok(trampoline) => ORIG_CLEANUP.store(trampoline, Ordering::SeqCst),
            Err(e) => log_error(&format!("shader cleanup hook: {e:?}; device resets unhandled")),
        }
    }
    log_line("shader renderer detection installed");
    Ok(())
}
