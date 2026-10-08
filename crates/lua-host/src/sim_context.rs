//! Terrain writes are only legal while a lockstep simulation callback runs.

use std::cell::Cell;

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

pub(crate) fn active() -> bool {
    DEPTH.with(|depth| depth.get() != 0)
}

/// Refuse a write that reaches the lockstep simulation outside a simulation
/// callback: from `wa.on.draw` / `wa.on.hud` (render time, differently on
/// every peer) it would desync the match.
pub(crate) fn require(what: &str) -> mlua::Result<()> {
    if active() {
        Ok(())
    } else {
        Err(mlua::Error::external(format!(
            "{what}: only from a simulation callback (worm_message, a brain, on_fire, ...), \
             never from wa.on.draw or wa.on.hud"
        )))
    }
}

/// Run a simulation callback: writes to the simulation are allowed, and the
/// processor's float settings are the simulation's (`float_env`), whatever
/// ran before on this thread; both are put back on return or panic.
pub(crate) fn with_sim<T>(f: impl FnOnce() -> T) -> T {
    DEPTH.with(|depth| {
        let floats = crate::float_env::pin();
        depth.set(depth.get() + 1);
        struct Exit(crate::float_env::Saved);
        impl Drop for Exit {
            fn drop(&mut self) {
                DEPTH.with(|depth| depth.set(depth.get() - 1));
                crate::float_env::restore(self.0);
            }
        }
        let _exit = Exit(floats);
        f()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_context_restores_after_return_and_panic() {
        assert!(!active());
        with_sim(|| {
            assert!(active());
            with_sim(|| assert!(active()));
            assert!(active());
        });
        assert!(!active());
        let _ = std::panic::catch_unwind(|| with_sim(|| panic!("callback failed")));
        assert!(!active());
    }

    /// 1/10 in binary is 0.000110011…: what is cut off after the last bit is
    /// over half of it, so rounding to nearest rounds up and rounding toward
    /// zero doesn't.
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    fn tenth() -> u64 {
        (std::hint::black_box(1.0f64) / std::hint::black_box(10.0f64)).to_bits()
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn a_callback_runs_under_the_simulations_float_settings_and_gives_them_back() {
        use crate::float_env::*;
        const NEAREST_TENTH: u64 = 0x3FB9_9999_9999_999A;
        // MXCSR's low six bits are sticky exception flags (1/10 sets the
        // inexact one); the settings are the rest.
        let control = || read_mxcsr() & !0x3F;
        // As another module might leave the thread: SSE2 rounding toward zero
        // with flush-to-zero, x87 at 24-bit precision (Direct3D 9's).
        let foreign_mxcsr = MXCSR | 0x6000 | 0x8000;
        let original = read_mxcsr();
        write_mxcsr(foreign_mxcsr);
        #[cfg(target_arch = "x86")]
        let original_x87 = read_x87();
        #[cfg(target_arch = "x86")]
        write_x87(0x007F);
        assert_ne!(tenth(), NEAREST_TENTH, "toward zero drops the last bit");

        with_sim(|| {
            assert_eq!(control(), MXCSR);
            #[cfg(target_arch = "x86")]
            assert_eq!(read_x87(), X87_CONTROL);
            assert_eq!(tenth(), NEAREST_TENTH);
            with_sim(|| assert_eq!(tenth(), NEAREST_TENTH));
            assert_eq!(control(), MXCSR, "a nested callback gives back the pinned settings");
        });
        assert_eq!(control(), foreign_mxcsr);
        #[cfg(target_arch = "x86")]
        assert_eq!(read_x87(), 0x007F);

        let _ = std::panic::catch_unwind(|| with_sim(|| panic!("callback failed")));
        assert_eq!(control(), foreign_mxcsr, "given back after a panic too");

        write_mxcsr(original);
        #[cfg(target_arch = "x86")]
        write_x87(original_x87);
    }
}
