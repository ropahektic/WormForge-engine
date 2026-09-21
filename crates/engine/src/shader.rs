//! WA 3.8.1 D3D9 shader lifecycle and optional RGBA composite.
//! The queue captures color/coverage independently of the shared palette. The
//! final draw uses it only when upload succeeds; all failures retain stock art.

#[cfg(debug_assertions)]
use crate::log_line;
use crate::{log_error, rebase::rb};
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
static ORIG_DRAW: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
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
    #[cfg(debug_assertions)]
    {
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
pub fn install(catalog: &Catalog) -> Result<(), String> {
    let sites: [(&str, &[u8], *mut c_void, &AtomicPtr<c_void>); 3] = [
        (
            "Display__D3D9ShaderPresent",
            &[0x83, 0xec, 0x0c, 0x53, 0x57, 0x8d, 0x44, 0x24, 0x08],
            shader_present_hook as _,
            &ORIG_PRESENT,
        ),
        (
            "Display__D3D9ShaderResourcesCleanup",
            &[0x56, 0x8b, 0xf1, 0x8b, 0x86, 0x88, 0, 0, 0],
            shader_cleanup_hook as _,
            &ORIG_CLEANUP,
        ),
        (
            "Display__D3D9ShaderFogDraw",
            &[0x8b, 0xf3, 0x8b, 0xc7, 0xe8],
            shader_draw_hook as _,
            &ORIG_DRAW,
        ),
    ];
    let mut installed = Vec::new();
    for (name, bytes, detour, original) in sites {
        let result = (|| {
            let va = catalog
                .function_va(name)
                .ok_or_else(|| format!("missing {name}"))?;
            let target = rb(va) as *mut c_void;
            if unsafe { std::slice::from_raw_parts(target as *const u8, bytes.len()) } != bytes {
                return Err(format!(
                    "{name} bytes differ or another module owns the hook"
                ));
            }
            let trampoline = unsafe { MinHook::create_hook(target, detour) }
                .map_err(|e| format!("{name}: {e:?}"))?;
            original.store(trampoline, Ordering::SeqCst);
            installed.push(target);
            Ok::<(), String>(())
        })();
        if let Err(e) = result {
            for target in installed {
                let _ = unsafe { MinHook::remove_hook(target) };
            }
            log_error(&format!(
                "truecolor disabled: {e}; stock palette rendering retained"
            ));
            return Ok(());
        }
    }
    #[cfg(debug_assertions)]
    log_line("shader renderer truecolor/lifecycle hooks installed");
    Ok(())
}
