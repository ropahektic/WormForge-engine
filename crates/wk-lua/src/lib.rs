//! WormKit module (`wkLua.dll`). Place next to `WA.exe`.
//! Lua mods + native wraps; not Project X `.pxl` / VFT stacking.

#![allow(non_snake_case)]

use windows_sys::Win32::Foundation::HINSTANCE;
use windows_sys::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};

#[no_mangle]
pub unsafe extern "system" fn DllMain(
    _hinst: HINSTANCE,
    reason: u32,
    _reserved: *mut core::ffi::c_void,
) -> i32 {
    match reason {
        DLL_PROCESS_ATTACH => {
            engine::attach_console();
            std::thread::spawn(engine::start);
            1
        }
        DLL_PROCESS_DETACH => 1,
        _ => 1,
    }
}
