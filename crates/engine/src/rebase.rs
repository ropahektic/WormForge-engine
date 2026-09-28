use std::sync::atomic::{AtomicU32, Ordering};

static BASE: AtomicU32 = AtomicU32::new(0);

pub fn wa_base() -> u32 {
    BASE.load(Ordering::Relaxed)
}

/// # Safety
/// Called once after attach; uses GetModuleHandle(NULL) = WA.exe.
pub unsafe fn init_rebase() {
    let handle =
        unsafe { windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null()) };
    let base = handle as u32;
    BASE.store(base, Ordering::Relaxed);
}
