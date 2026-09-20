use crate::actor;
use crate::apply::{after_init_weapon_table, on_gfx_ready};
use crate::hud;
use crate::log::{log_error, log_line, panic_text};
use crate::physics;
use crate::rebase::rb;
use game::offsets::{
    GAME_RUNTIME_DISPLAY, GAME_RUNTIME_WORLD, GAME_WORLD_DISPLAY, GFX_HANDLER_LOAD_SPRITES,
    INIT_WEAPON_TABLE,
};
use minhook::MinHook;
use re_catalog::Catalog;
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};

type InitWeaponTableFn = unsafe extern "system" fn(*mut u8);

static ORIGINAL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_LS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

pub fn install() -> Result<(), String> {
    let catalog = Catalog::builtin()?;
    let va = catalog
        .function_va("InitWeaponTable")
        .unwrap_or(INIT_WEAPON_TABLE);
    let target = rb(va) as *mut c_void;
    let trampoline = unsafe {
        MinHook::create_hook(target, init_weapon_table_hook as *mut c_void)
            .map_err(|e| format!("MinHook create: {e:?}"))?
    };
    ORIGINAL.store(trampoline, Ordering::SeqCst);

    let ls = rb(GFX_HANDLER_LOAD_SPRITES) as *mut c_void;
    let ls_tr = unsafe {
        MinHook::create_hook(ls, load_sprites_hook as *mut c_void)
            .map_err(|e| format!("MinHook LoadSprites: {e:?}"))?
    };
    ORIGINAL_LS.store(ls_tr, Ordering::SeqCst);

    physics::install(&catalog)?;
    hud::install(&catalog)?;
    actor::install(&catalog)?;
    crate::worm_sprite::install(&catalog)?;
    crate::shader::install(&catalog)?;
    unsafe {
        MinHook::enable_all_hooks().map_err(|e| format!("MinHook enable: {e:?}"))?;
    }
    Ok(())
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
        log_error(&format!(
            "panic after LoadSprites: {}",
            panic_text(p)
        ));
    }
}
