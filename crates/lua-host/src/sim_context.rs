//! Terrain writes are only legal while a lockstep simulation callback runs.

use std::cell::Cell;

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

pub(crate) fn active() -> bool {
    DEPTH.with(|depth| depth.get() != 0)
}

pub(crate) fn with_sim<T>(f: impl FnOnce() -> T) -> T {
    DEPTH.with(|depth| {
        depth.set(depth.get() + 1);
        struct Exit;
        impl Drop for Exit {
            fn drop(&mut self) {
                DEPTH.with(|depth| depth.set(depth.get() - 1));
            }
        }
        let _exit = Exit;
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
}
