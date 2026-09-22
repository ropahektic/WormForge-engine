use std::sync::atomic::{AtomicU32, Ordering};

const IMAGE_BASE: u32 = 0x0040_0000;

static BASE: AtomicU32 = AtomicU32::new(IMAGE_BASE);
static DELTA: AtomicU32 = AtomicU32::new(0);

pub fn wa_base() -> u32 {
    BASE.load(Ordering::Relaxed)
}

pub fn delta() -> u32 {
    DELTA.load(Ordering::Relaxed)
}

pub fn rb(ghidra_va: u32) -> u32 {
    ghidra_va.wrapping_add(DELTA.load(Ordering::Relaxed))
}

/// # Safety
/// Called once after attach; uses GetModuleHandle(NULL) = WA.exe.
pub unsafe fn init_rebase() {
    let handle =
        unsafe { windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null()) };
    let base = handle as u32;
    BASE.store(base, Ordering::Relaxed);
    DELTA.store(base.wrapping_sub(IMAGE_BASE), Ordering::Relaxed);
}
