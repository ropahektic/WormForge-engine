//! WA 3.8.1 queue dispatcher. High depth draws first (soil > sentry > terrain).
//! This is the complete queue, unlike WorldRoot's RenderScene callback which
//! can run before other producers. Layout/ABI checked against local WA.exe.

#[cfg(debug_assertions)]
use crate::log_line;
use crate::{log_error, rebase::rb};
use minhook::MinHook;
use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicPtr, Ordering},
        Mutex,
    },
};

pub const COUNT: usize = 0x10004;
const PTRS: usize = 0x10008;
const MAX: usize = 0x800;
static ORIGINAL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static DEPTHS: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());

pub fn set_depth(sprite: u32, depth: u32) {
    if let Ok(mut g) = DEPTHS.lock() {
        g.retain(|(id, _)| *id != sprite);
        g.push((sprite, depth));
    }
}
pub fn clear_depths() {
    if let Ok(mut g) = DEPTHS.lock() {
        g.clear();
    }
}

/// Validated command in the 64 KiB queue arena. No speculative pointer walks.
#[derive(Clone, Copy)]
pub struct Command {
    pub address: usize,
    pub kind: u32,
    pub depth: u32,
}
impl Command {
    pub unsafe fn word(self, index: usize) -> u32 {
        *((self.address + index * 4) as *const u32)
    }
    pub fn sprite_word(self) -> Option<usize> {
        match self.kind {
            4 | 5 => Some(4),
            6 => Some(7),
            _ => None,
        }
    }
}

pub struct Queue {
    pub base: usize,
    pub commands: Vec<Command>,
}
impl Queue {
    /// Use WA's own sorter/comparator so equal-depth sprites retain exactly
    /// the order that its real dispatcher will use (qsort is not stable).
    pub unsafe fn ordered(&self) -> Vec<Command> {
        type Compare = unsafe extern "C" fn(*const c_void, *const c_void) -> i32;
        type Sort = unsafe extern "C" fn(*mut c_void, usize, usize, Compare);
        let sort: Sort = std::mem::transmute(rb(0x005d_7890));
        let cmp: Compare = std::mem::transmute(rb(0x0054_2330));
        let mut pointers: Vec<u32> = self.commands.iter().map(|c| c.address as u32).collect();
        sort(pointers.as_mut_ptr() as _, pointers.len(), 4, cmp);
        pointers
            .iter()
            .rev()
            .map(|&p| {
                *self
                    .commands
                    .iter()
                    .find(|c| c.address == p as usize)
                    .unwrap()
            })
            .collect()
    }
    pub unsafe fn read(base: usize) -> Option<Self> {
        if base == 0 {
            return None;
        }
        let count = *((base + COUNT) as *const u32) as usize;
        if count == 0 || count > MAX {
            return None;
        }
        let mut commands = Vec::with_capacity(count);
        for &address in std::slice::from_raw_parts((base + PTRS) as *const u32, count) {
            let address = address as usize;
            let offset = address.checked_sub(base)?;
            if offset < 4 || offset > COUNT - 8 || address & 3 != 0 {
                return None;
            }
            let kind = *(address as *const u32);
            let words = match kind {
                4 | 5 => 6,
                6 => 9,
                _ => 2,
            };
            if offset > COUNT - words * 4 {
                return None;
            }
            commands.push(Command {
                address,
                kind,
                depth: *((address + 4) as *const u32),
            });
        }
        Some(Self { base, commands })
    }
    unsafe fn apply_depths(&mut self) {
        let Ok(depths) = DEPTHS.lock() else {
            return;
        };
        for c in &mut self.commands {
            let Some(word) = c.sprite_word() else {
                continue;
            };
            let id = c.word(word) & 0xffff;
            if let Some((_, depth)) = depths.iter().find(|(s, _)| *s == id) {
                *((c.address + 4) as *mut u32) = *depth;
                c.depth = *depth;
            }
        }
    }
    /// Temporarily substitute entry pointers; always restore on unwind.
    pub unsafe fn draw(&self, commands: &[Command], display: usize, clip: usize) {
        if commands.is_empty() {
            return;
        }
        struct Restore<'a>(&'a Queue);
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                unsafe {
                    self.0.restore();
                }
            }
        }
        let _restore = Restore(self);
        let ptrs = (self.base + PTRS) as *mut u32;
        for (i, c) in commands.iter().enumerate() {
            *ptrs.add(i) = c.address as u32;
            // Force this subset's already-resolved ordering. Otherwise sorting
            // it again could change equal-depth draws relative to the full queue.
            *((c.address + 4) as *mut u32) = (commands.len() - i) as u32;
        }
        *((self.base + COUNT) as *mut u32) = commands.len() as u32;
        call_original(self.base, display, clip);
    }
    unsafe fn restore(&self) {
        let ptrs = (self.base + PTRS) as *mut u32;
        for (i, c) in self.commands.iter().enumerate() {
            *ptrs.add(i) = c.address as u32;
            *((c.address + 4) as *mut u32) = c.depth;
        }
        *((self.base + COUNT) as *mut u32) = self.commands.len() as u32;
    }
}

unsafe extern "C" fn dispatch(base: usize, display: usize, clip: usize) {
    let capture = std::panic::catch_unwind(|| {
        if let Some(mut q) = Queue::read(base) {
            q.apply_depths();
            // Measurement only, behind a marker file: see queue_census.rs.
            crate::queue_census::observe(&q);
            // Fog of war: rebuild this frame's vision sources and light grid.
            // Runs once per render frame here, alongside the GPU/truecolor
            // captures, with display+clip in hand. The mask it caches is taken
            // by the render pass separately (fog::build_mask / cached_mask).
            crate::fog::on_render_frame(display, clip);
            // The GPU path owns the split when it is active; the two must
            // never both cut the dispatch, so this is exclusive by construction.
            if !crate::gpu::capture(&q, display, clip) {
                crate::truecolor::capture(&q, display, clip);
            }
        }
    });
    if capture.is_err() {
        crate::truecolor::begin_frame();
        log_error("truecolor queue capture panicked; using stock draw");
    }
    // The real frame ALWAYS contains every stock draw, including our palette
    // approximations. A shader/upload failure therefore never hides sprites.
    // capture() may have drawn the frame itself, splitting the dispatch to take
    // coverage from what actually changed; only draw again if it did not.
    if !crate::gpu::DREW.swap(false, Ordering::SeqCst)
        && !crate::truecolor::DREW.swap(false, Ordering::SeqCst)
    {
        call_original(base, display, clip);
    }
    crate::truecolor::after_stock(display);
}

/// EAX=queue; [ESP+4]=display, [ESP+8]=clip, callee pops 8.
#[unsafe(naked)]
unsafe extern "C" fn hook() {
    core::arch::naked_asm!(
        "push ebp", "mov ebp, esp", "pushfd", "pushad",
        "push dword ptr [ebp+12]", "push dword ptr [ebp+8]", "push eax",
        "call {dispatch}", "add esp, 12", "popad", "popfd", "pop ebp", "ret 8",
        dispatch = sym dispatch,
    );
}
#[unsafe(naked)]
unsafe extern "C" fn call_original(_queue: usize, _display: usize, _clip: usize) {
    core::arch::naked_asm!(
        "mov eax, [esp+4]", "push dword ptr [esp+12]", "push dword ptr [esp+12]",
        "call dword ptr [{original}]", "ret", original = sym ORIGINAL,
    );
}

pub fn install() -> Result<(), String> {
    let addr = rb(0x0054_2350) as *mut c_void;
    let expected = [0x83, 0xec, 0x1c, 0x53, 0x55, 0x8b, 0x6c, 0x24, 0x28];
    if unsafe { std::slice::from_raw_parts(addr as *const u8, expected.len()) } != expected {
        log_error("queue dispatcher bytes differ; depth/truecolor hook disabled");
        return Ok(());
    }
    let trampoline = unsafe { MinHook::create_hook(addr, hook as *mut c_void) }
        .map_err(|e| format!("render queue hook: {e:?}"))?;
    ORIGINAL.store(trampoline, Ordering::SeqCst);
    #[cfg(debug_assertions)]
    log_line("render queue depth/truecolor hook installed");
    Ok(())
}
