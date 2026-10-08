//! The processor's float settings every simulation callback runs under.
//!
//! A thread's float settings live in two registers: MXCSR for SSE2 (rounding
//! mode, flush-to-zero) and the x87 control word (precision, rounding mode).
//! Any code in the process may change them for the thread: Direct3D 9 sets
//! the x87 precision to 24 bits when a device is created without
//! D3DCREATE_FPU_PRESERVE, and injected overlays, hooks and old plugins
//! have been seen to change either. Lua's arithmetic is SSE2 and its x87 use
//! is loads and explicitly rounded stores, but a pack's result must not
//! depend on what ran before it on one machine. So a simulation callback
//! saves both registers, runs under Windows' defaults, and puts back what it
//! found.

/// SSE2: every exception masked, round to nearest, no flush-to-zero or
/// denormals-are-zero (the processor's reset value).
pub(crate) const MXCSR: u32 = 0x1F80;
/// x87: every exception masked, 53-bit (double) precision, round to nearest
/// (Windows' default for a 32-bit process).
pub(crate) const X87_CONTROL: u16 = 0x027F;

#[derive(Clone, Copy)]
pub(crate) struct Saved {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    mxcsr: u32,
    #[cfg(target_arch = "x86")]
    x87: u16,
}

/// Save the thread's float settings and set the simulation's.
pub(crate) fn pin() -> Saved {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    let mxcsr = {
        let old = read_mxcsr();
        write_mxcsr(MXCSR);
        old
    };
    #[cfg(target_arch = "x86")]
    let x87 = {
        let old = read_x87();
        write_x87(X87_CONTROL);
        old
    };
    Saved {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        mxcsr,
        #[cfg(target_arch = "x86")]
        x87,
    }
}

/// Put back the settings `pin` found.
pub(crate) fn restore(saved: Saved) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    write_mxcsr(saved.mxcsr);
    #[cfg(target_arch = "x86")]
    write_x87(saved.x87);
    let _ = saved;
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub(crate) fn read_mxcsr() -> u32 {
    let mut value = 0u32;
    unsafe {
        core::arch::asm!("stmxcsr dword ptr [{0}]", in(reg) &mut value as *mut u32,
            options(nostack, preserves_flags));
    }
    value
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub(crate) fn write_mxcsr(value: u32) {
    unsafe {
        core::arch::asm!("ldmxcsr dword ptr [{0}]", in(reg) &value as *const u32,
            options(nostack, preserves_flags, readonly));
    }
}

#[cfg(target_arch = "x86")]
pub(crate) fn read_x87() -> u16 {
    let mut value = 0u16;
    unsafe {
        core::arch::asm!("fnstcw word ptr [{0}]", in(reg) &mut value as *mut u16,
            options(nostack, preserves_flags));
    }
    value
}

#[cfg(target_arch = "x86")]
pub(crate) fn write_x87(value: u16) {
    unsafe {
        core::arch::asm!("fldcw word ptr [{0}]", in(reg) &value as *const u16,
            options(nostack, preserves_flags, readonly));
    }
}
