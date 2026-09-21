//! Process attach: rebase, version gate, wrap stock entry points.
//!
//! Dispatch model (see docs/architecture.md): wrap, then Lua by owner id.
//! Do not trap FireWeapon / InitWeaponTable. Do not stack VFTs.

mod actor;
mod apply;
mod hook;
mod hud;
mod hurt;
mod overlay;
mod log;
mod physics;
pub mod render_queue;
pub mod shader;
mod truecolor;
mod rebase;
mod sprite;
mod version;
mod world;
mod worm_sprite;

pub use log::{attach_console, error_log_path, log_error, log_line, log_path, panic_text};
pub use rebase::{init_rebase, rb, wa_base};

use std::sync::atomic::{AtomicBool, Ordering};

static STARTED: AtomicBool = AtomicBool::new(false);

pub fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    log::init();
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
        Ok(()) => log_line("hooks installed"),
        Err(e) => log_error(&format!("hook install failed: {e}")),
    }
}
