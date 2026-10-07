use crate::actor;
use crate::apply::after_init_weapon_table;
use crate::hud;
use crate::log::{log_error, log_line, panic_text};
use crate::physics;
use minhook::MinHook;
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};

type InitWeaponTableFn = unsafe extern "system" fn(*mut u8);

static ORIGINAL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_SHUTDOWN: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_REPLAY_LOADER: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

pub fn install() -> Result<(), String> {
    let result = install_inner();
    if result.is_err() {
        crate::wa::rollback_hooks();
    }
    result
}

fn install_inner() -> Result<(), String> {
    for (name, detour, original) in [
        ("INIT_WEAPON_TABLE", init_weapon_table_hook as *mut c_void, &ORIGINAL),
        ("GAME_ENGINE_SHUTDOWN", shutdown_hook as *mut c_void, &ORIGINAL_SHUTDOWN),
        ("REPLAY_LOADER", replay_loader_hook as *mut c_void, &ORIGINAL_REPLAY_LOADER),
    ] {
        let target = crate::wa::address(name) as *mut c_void;
        let trampoline = unsafe { crate::wa::create_hook(target, detour) }?;
        original.store(trampoline, Ordering::SeqCst);
    }

    crate::body::install()?;
    physics::install()?;
    crate::arrow_source::install()?;
    actor::install()?;
    crate::grants::install()?;
    crate::worm_sprite::install()?;
    crate::mine::install()?;
    crate::missile_boom::install()?;
    crate::particles::install()?;
    crate::contact::install()?;
    crate::shader::install()?;
    let checkpoint = crate::wa::hook_checkpoint();
    if let Err(e) = crate::perf::install() {
        crate::wa::rollback_to(checkpoint);
        log_error(&format!("profiler flip timing disabled: {e}"));
    }
    let checkpoint = crate::wa::hook_checkpoint();
    if let Err(e) = crate::fire::install() {
        crate::wa::rollback_to(checkpoint);
        log_error(&format!("fire tracking on free disabled: {e}"));
    }
    let checkpoint = crate::wa::hook_checkpoint();
    if let Err(e) = crate::windup::install() {
        crate::wa::rollback_to(checkpoint);
        log_error(&format!("worm.windup disabled: {e}"));
    }
    let checkpoint = crate::wa::hook_checkpoint();
    if let Err(e) = crate::state_trace::install() {
        crate::wa::rollback_to(checkpoint);
        log_error(&format!("worm state trace disabled: {e}"));
    }
    let checkpoint = crate::wa::hook_checkpoint();
    if let Err(e) = crate::render_queue::install() {
        crate::wa::rollback_to(checkpoint);
        log_error(&format!("render queue feature disabled: {e}"));
    }
    let checkpoint = crate::wa::hook_checkpoint();
    if let Err(e) = crate::gpu::install_rq_draw() {
        crate::wa::rollback_to(checkpoint);
        log_error(&format!("GPU sprite feature disabled: {e}"));
    }
    crate::sync::install()?;
    let checkpoint = crate::wa::hook_checkpoint();
    if let Err(e) = crate::lobby::install().and_then(|_| crate::transfer::install()) {
        crate::wa::rollback_to(checkpoint);
        log_error(&format!("lobby asset sync unavailable: {e}"));
    }
    unsafe {
        MinHook::enable_all_hooks().map_err(|e| format!("MinHook enable: {e:?}"))?;
    }
    if let Err(e) = hud::install() {
        log_error(&format!("HUD overlay disabled: {e}"));
    }
    Ok(())
}

/// `ReplayLoader(prefix, mode)`. Mode 1 is playback; the other modes extract
/// maps/schemes or repair an ordinary `.WAgame` and pass through unchanged.
unsafe extern "system" fn replay_loader_hook(prefix: *mut u8, mode: i32) -> u32 {
    let result = std::panic::catch_unwind(|| unsafe {
        let orig = ORIGINAL_REPLAY_LOADER.load(Ordering::SeqCst);
        if orig.is_null() {
            return u32::MAX;
        }
        let f: unsafe extern "system" fn(*mut u8, i32) -> u32 = std::mem::transmute(orig);
        if mode != 1 {
            log_line(&format!("replay loader mode {mode}: stock pass-through"));
            return f(prefix, mode);
        }
        match crate::replay::redirect_playback(prefix) {
            Ok(false) => f(prefix, mode),
            Ok(true) => {
                let code = f(prefix, mode);
                if code != 0 {
                    crate::replay::playback_loader_failed(code);
                }
                code
            }
            Err(e) => {
                log_error(&format!("WormForge replay refused: {e}"));
                // WA's own `InvalidFormat` code. Its caller supplies the UI;
                // the WormForge log preserves the precise refusal above.
                (-2i32) as u32
            }
        }
    });
    match result {
        Ok(code) => code,
        Err(p) => {
            log_error(&format!("panic in ReplayLoader wrap: {}", panic_text(p)));
            (-2i32) as u32
        }
    }
}

/// WA closes and destroys the replay writer inside GameEngine::Shutdown.
/// Finish the recording after the original returns, so the `.WAgame` is final
/// and no longer held open when the trailer is appended to it.
unsafe extern "system" fn shutdown_hook(state_buf: *mut u8) {
    // Before the original: its teardown deletes the panel images, and a
    // copied icon must not leave two slots owning the same one.
    if std::panic::catch_unwind(crate::apply::restore_panel_icons).is_err() {
        log_error("panic while restoring panel icons");
    }
    if std::panic::catch_unwind(crate::panel_icon::restore).is_err() {
        log_error("panic while restoring custom panel icons");
    }
    // Same reason for the sprite table: WA deletes every sprite it holds, and
    // ours are not WA's to delete.
    if std::panic::catch_unwind(crate::sprite::release_before_teardown).is_err() {
        log_error("panic while releasing custom sprites");
    }
    let orig = ORIGINAL_SHUTDOWN.load(Ordering::SeqCst);
    if !orig.is_null() {
        let f: unsafe extern "system" fn(*mut u8) = std::mem::transmute(orig);
        f(state_buf);
    }
    let result = std::panic::catch_unwind(|| {
        crate::replay::finish();
        crate::modloader::match_ended();
        crate::apply::match_ended();
    });
    if let Err(p) = result {
        log_error(&format!(
            "panic after GameEngine::Shutdown: {}",
            panic_text(p)
        ));
    }
}

unsafe extern "system" fn init_weapon_table_hook(runtime: *mut u8) {
    let result = std::panic::catch_unwind(|| unsafe {
        let orig = ORIGINAL.load(Ordering::SeqCst);
        if !orig.is_null() {
            let f: InitWeaponTableFn = std::mem::transmute(orig);
            f(runtime);
        }
        if let Err(e) = after_init_weapon_table(runtime) {
            log_error(&format!("overlay failed: {e}"));
        }
    });
    if let Err(p) = result {
        log_error(&format!("panic in InitWeaponTable wrap: {}", panic_text(p)));
    }
}
