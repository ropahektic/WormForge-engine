//! Windows CRT compatibility used by vendored Lua.
//!
//! The UCRT stores printf mode flags in this module-local cell.  The MSVC
//! libraries linked into wkLua initialize it with legacy flags (0x18).  Wine
//! implements the formatting operation but logs a `fixme` for those flags on
//! every Lua number conversion.  Lua only uses ordinary narrow formats here,
//! so the standard zero flag set is the correct mode.

unsafe extern "C" {
    fn __local_stdio_printf_options() -> *mut u64;
}

/// Select standard narrow printf behavior for the DLL's vendored Lua runtime.
/// Returns the legacy value for one startup diagnostic.
pub unsafe fn use_standard_printf_options() -> u64 {
    let options = __local_stdio_printf_options();
    let previous = options.read();
    options.write(0);
    previous
}
