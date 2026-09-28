//! Process attach: rebase, version gate, wrap stock entry points.
//!
//! Dispatch model (see docs/architecture.md): wrap, then Lua by owner id.
//! Do not trap FireWeapon / InitWeaponTable. Do not stack VFTs.

mod actor;
mod apply;
mod panel_icon;
#[cfg(target_os = "windows")]
mod contact;
mod crt;
mod hook;
mod http;
mod hud;
mod hurt;
mod overlay;
mod particles;
mod perf;
mod lobby;
mod log;
mod mask_collide;
mod mine;
mod missile_boom;
mod modloader;
mod physics;
mod rebase;
mod replay;
mod sprite;
mod queue_census;
mod sync;
mod transfer;
mod truecolor;
mod shader;
pub mod render_queue;
mod gpu;
mod version;
mod world;
mod worm_sprite;
mod zip;

pub use log::{attach_console, error_log_path, log_error, log_line, log_path, panic_text};
pub use rebase::{init_rebase, rb, wa_base};

use std::sync::atomic::{AtomicBool, Ordering};

static STARTED: AtomicBool = AtomicBool::new(false);

pub fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    #[cfg(target_os = "windows")]
    let legacy_printf_options = unsafe { crt::use_standard_printf_options() };
    log::init();
    #[cfg(target_os = "windows")]
    log_line(&format!(
        "CRT printf options {legacy_printf_options:#x} -> 0x0 (Wine-compatible)"
    ));
    log_line("wkLua starting");
    unsafe {
        rebase::init_rebase();
    }
    log_line(&format!("WA base {:#010x} delta {}", wa_base(), rebase::delta()));
    match version::check_381() {
        Ok(()) => log_line("WA version string is 3.8.1"),
        Err(e) => {
            log_error(&format!("refusing to hook: {e}"));
            return;
        }
    }
    match hook::install() {
        Ok(()) => {
            log_line("hooks installed");
            // Recordings a crash left unfinished get their trailer now.
            replay::recover_pending();
        }
        Err(e) => log_error(&format!("hook install failed: {e}")),
    }
    modloader::start();
}
