use crate::actor;
use crate::apply::{after_init_weapon_table, on_gfx_ready};
use crate::hud;
use crate::log::{log_error, log_line, panic_text};
use crate::physics;
use game::offsets::{GAME_RUNTIME_DISPLAY, GAME_RUNTIME_WORLD, GAME_WORLD_DISPLAY};
use minhook::MinHook;
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};

type InitWeaponTableFn = unsafe extern "system" fn(*mut u8);

static ORIGINAL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_LS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
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
        ("GFX_HANDLER_LOAD_SPRITES", load_sprites_hook as *mut c_void, &ORIGINAL_LS),
        ("GAME_ENGINE_SHUTDOWN", shutdown_hook as *mut c_void, &ORIGINAL_SHUTDOWN),
        ("REPLAY_LOADER", replay_loader_hook as *mut c_void, &ORIGINAL_REPLAY_LOADER),
    ] {
        let target = crate::wa::address(name) as *mut c_void;
        let trampoline = unsafe { crate::wa::create_hook(target, detour) }?;
        original.store(trampoline, Ordering::SeqCst);
    }

    physics::install()?;
    actor::install()?;
    crate::worm_sprite::install()?;
    crate::mine::install()?;
    crate::missile_boom::install()?;
    crate::particles::install()?;
    crate::contact::install()?;
    crate::shader::install()?;
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

/// `GfxHandler__LoadSprites` (0x570B50): stdcall 4 stack args, ESI=PaletteContext, RET 16.
/// Caller (world ctor 0x56E76A / 0x56E81C) keeps ESI as layer_ctx; this thunk
/// must not touch ESI before the trampoline. After orig, EBP is still GameRuntime*
/// but we pass the stack runtime instead.
#[unsafe(naked)]
unsafe extern "system" fn load_sprites_hook() {
    core::arch::naked_asm!(
        "push dword ptr [esp+16]",
        "push dword ptr [esp+16]",
        "push dword ptr [esp+16]",
        "push dword ptr [esp+16]",
        "mov eax, [{orig}]",
        "call eax",
        "push dword ptr [esp+4]",
        "call {after}",
        "add esp, 4",
        "ret 16",
        orig = sym ORIGINAL_LS,
        after = sym after_load_sprites,
    );
}

unsafe extern "C" fn after_load_sprites(runtime: *mut u8) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        if runtime.is_null() {
            log_error("LoadSprites wrap: runtime is null");
            return;
        }
        let world = *(runtime.add(GAME_RUNTIME_WORLD) as *const *mut u8);
        let display = if !world.is_null() {
            *(world.add(GAME_WORLD_DISPLAY) as *const *mut u8)
        } else {
            *(runtime.add(GAME_RUNTIME_DISPLAY) as *const *mut u8)
        };
        log_line("LoadSprites returned — PX InitGraphic window (after sprites.dat)");
        on_gfx_ready(display);
    }));
    if let Err(p) = result {
        log_error(&format!("panic after LoadSprites: {}", panic_text(p)));
    }
}
