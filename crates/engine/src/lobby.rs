//! The host's ready bulb, where network asset sync begins
//! (`docs/network-asset-sync-plan.md`, step 1).
//!
//! Lighting the bulb in a hosted lobby starts a new sync generation: the
//! enabled pack set is frozen into a session directory off the lobby thread
//! (`netsync::freeze`). Putting the bulb out withdraws it; lighting it again
//! freezes a fresh one, so edits to `Mods/` in between are picked up.
//!
//! The bulb is read from WA's own lobby table, `CPlayers` (0x8779D8): this
//! machine's record is `local_slot` (+8), its ready flag +0x82, active +0x80,
//! records 0x78 apart. Slot 0 is the host. Three places observe it, because
//! which of them runs for the host's own click is what this step confirms in
//! a live lobby: both player lists' `UpdateReady` and the host's Start gate.
//! Every transition is logged with the observer that saw it first.

use crate::log::{log_error, log_line, panic_text, wa_directory, wormforge_dir};
use game::entity::read_i32;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

static ORIGINAL_LIST: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_NET_LIST: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_GATE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// `crate::wa::address("G_CPLAYERS")`, once installed.
static PLAYERS: AtomicU32 = AtomicU32::new(0);
/// The local bulb as last observed: (host, lit).
static LAST: Mutex<Option<(bool, bool)>> = Mutex::new(None);
/// The current sync generation. 0 means none: the host's bulb is out.
static GENERATION: AtomicU32 = AtomicU32::new(0);
/// Generations started by this process.
static STARTED: AtomicU32 = AtomicU32::new(0);
/// Names this run's session directories. The process id alone repeats:
/// Wine hands WA the same small id on every launch.
static RUN: OnceLock<String> = OnceLock::new();

const RECORD_STRIDE: usize = 0x78;
const RECORD_ACTIVE: usize = 0x80;
const RECORD_READY: usize = 0x82;
const RECORD_NAME: usize = 0xC;
const LOCAL_SLOT: usize = 0x8;
/// `IsOkToStartNetGame` walks 13 records.
const RECORDS: i32 = 13;

type UpdateReadyFn = unsafe extern "thiscall" fn(*mut u8);

pub fn install() -> Result<(), String> {
    let players = crate::wa::try_address("G_CPLAYERS").ok_or("missing G_CPLAYERS locator")?;
    let list = target("LOBBY_LIST_UPDATE_READY")?;
    let net_list = target("LOBBY_NET_LIST_UPDATE_READY")?;
    let gate = target("CPLAYERS_OK_TO_START_NET_GAME")?;
    unsafe {
        ORIGINAL_LIST.store(crate::wa::create_hook(list, list_update_ready_hook as *mut c_void)?, Ordering::SeqCst);
        ORIGINAL_NET_LIST.store(crate::wa::create_hook(net_list, net_list_update_ready_hook as *mut c_void)?, Ordering::SeqCst);
        ORIGINAL_GATE.store(crate::wa::create_hook(gate, start_gate_hook as *mut c_void)?, Ordering::SeqCst);
    }
    PLAYERS.store(players, Ordering::SeqCst);
    log_line("lobby ready-bulb hooks installed");
    Ok(())
}

pub(crate) fn target(name: &str) -> Result<*mut c_void, String> {
    crate::wa::check_callable(name).map(|addr| addr as *mut c_void)
}

/// This machine's bulb: (host, lit), or `None` outside a network lobby.
unsafe fn local_bulb() -> Option<(bool, bool)> {
    let players = PLAYERS.load(Ordering::Relaxed) as *mut u8;
    if players.is_null() {
        return None;
    }
    let slot = read_i32(players, LOCAL_SLOT);
    if !(0..RECORDS).contains(&slot) {
        return None;
    }
    let record = players.add(slot as usize * RECORD_STRIDE);
    if *record.add(RECORD_ACTIVE) == 0 {
        return None;
    }
    Some((slot == 0, *record.add(RECORD_READY) != 0))
}

/// A machine's name in the lobby (`CPlayers` record +0xC, 16 bytes at most:
/// `CPlayers::AddMachine`), `None` when the slot is empty or out of range.
/// Slot 0 is the host.
pub(crate) fn machine_name(slot: i32) -> Option<String> {
    let players = PLAYERS.load(Ordering::Relaxed) as *const u8;
    if players.is_null() || !(0..RECORDS).contains(&slot) {
        return None;
    }
    unsafe {
        let record = players.add(slot as usize * RECORD_STRIDE);
        if *record.add(RECORD_ACTIVE) == 0 {
            return None;
        }
        let raw = std::slice::from_raw_parts(record.add(RECORD_NAME), 0x10);
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let name: String = raw[..end].iter().map(|&b| b as char).filter(|c| !c.is_control()).collect();
        (!name.is_empty()).then_some(name)
    }
}

/// This machine's name in the lobby.
pub(crate) fn local_name() -> Option<String> {
    let players = PLAYERS.load(Ordering::Relaxed) as *mut u8;
    if players.is_null() {
        return None;
    }
    machine_name(unsafe { read_i32(players, LOCAL_SLOT) })
}

/// The generation being offered, or 0 when the host's bulb is out.
pub(crate) fn current_generation() -> u32 {
    GENERATION.load(Ordering::SeqCst)
}

fn observe(source: &str) {
    let Some(now) = (unsafe { local_bulb() }) else {
        return;
    };
    crate::transfer::entered_lobby();
    let Ok(mut last) = LAST.lock() else {
        return;
    };
    if *last == Some(now) {
        return;
    }
    let was_lit_host = *last == Some((true, true));
    *last = Some(now);
    drop(last);

    let (host, lit) = now;
    log_line(&format!(
        "lobby bulb: {} {} (seen by {source}, thread {})",
        if host { "host" } else { "client" },
        if lit { "lit" } else { "out" },
        unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() }
    ));
    if host && lit {
        start_generation();
    } else if was_lit_host {
        let old = GENERATION.swap(0, Ordering::SeqCst);
        if old != 0 {
            log_line(&format!("sync generation {old} withdrawn: host bulb out"));
            crate::transfer::withdraw(old);
        }
    }
}

fn start_generation() {
    let n = STARTED.fetch_add(1, Ordering::SeqCst) + 1;
    GENERATION.store(n, Ordering::SeqCst);
    log_line(&format!("sync generation {n}: freezing enabled packs"));
    // Disk work stays off the lobby thread.
    let spawned = std::thread::Builder::new()
        .name(format!("wf-sync-{n}"))
        .spawn(move || {
            if let Err(p) = std::panic::catch_unwind(|| freeze_generation(n)) {
                log_error(&format!("sync generation {n} panicked: {}", panic_text(p)));
            }
        });
    if let Err(e) = spawned {
        log_error(&format!("sync generation {n}: cannot start freeze thread: {e}"));
    }
}

fn sync_root() -> PathBuf {
    wormforge_dir().join("sync")
}

fn run_id() -> &'static str {
    RUN.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{}-{nanos}", std::process::id())
    })
}

fn session_name(generation: u32) -> String {
    format!("host-{}-{generation}", run_id())
}

fn freeze_generation(n: u32) {
    let started = std::time::Instant::now();
    let mods_dir = lua_host::mods_root_from(&wa_directory());
    let discovered = match weapons::discover_mods(&mods_dir) {
        Ok(d) => d,
        Err(e) => {
            log_error(&format!("sync generation {n}: discover {}: {e}", mods_dir.display()));
            return;
        }
    };
    let (mods, skipped) = weapons::filter_requirements(&discovered);
    for s in skipped {
        log_line(&format!("sync generation {n}: {s}"));
    }
    let root = sync_root();
    if let Err(e) = std::fs::create_dir_all(&root) {
        log_error(&format!("sync generation {n}: {}: {e}", root.display()));
        return;
    }
    let dest = root.join(session_name(n));
    // The component selection and the bundles it uses travel with the packs,
    // so every peer runs the host's selection, not its own settings.
    let stage = root.join(format!("selection-{n}"));
    let selection = crate::selection::load(&mods_dir, &stage);
    let frozen = netsync::freeze_with(&mods, &selection.folders, &dest);
    let _ = std::fs::remove_dir_all(&stage);
    let session = match frozen {
        Ok(s) => s,
        Err(e) => {
            log_error(&format!("sync generation {n}: freeze failed: {e}"));
            return;
        }
    };
    if GENERATION.load(Ordering::SeqCst) != n {
        // The bulb went out, or was lit again, while this one was copying.
        let _ = std::fs::remove_dir_all(&session.dir);
        log_line(&format!("sync generation {n}: superseded before it finished; discarded"));
        return;
    }
    let m = &session.manifest;
    log_line(&format!(
        "sync generation {n} frozen: {} packs, {} files, {} bytes, manifest {} in {} ms at {}",
        m.packs.len(),
        m.files.len(),
        m.total_bytes(),
        netsync::hex(&m.digest()),
        started.elapsed().as_millis(),
        session.dir.display()
    ));
    prune(n);
    match netsync::store::Served::load(n, &session) {
        Ok(served) if GENERATION.load(Ordering::SeqCst) == n => {
            let packs: Vec<String> = m.packs.iter().map(|p| format!("{} {}", p.id, p.version)).collect();
            crate::transfer::serve(served, session.dir.clone(), packs)
        }
        Ok(_) => {}
        Err(e) => log_error(&format!("sync generation {n}: cannot serve: {e}")),
    }
}

/// Drop every host session but the current one, this run's earlier
/// generations and earlier runs' leftovers alike. Nothing reads them yet:
/// loading rounds and recording replays from a session come in later steps,
/// and must then keep a session for as long as a recording uses it.
///
/// A later generation of this run, staged or published, is left alone: the
/// bulb may have been lit again while this one was being pruned.
fn prune(current: u32) {
    let ours = format!("host-{}-", run_id());
    let Ok(entries) = std::fs::read_dir(sync_root()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("host-") {
            continue;
        }
        let generation = name
            .strip_prefix(&ours)
            .and_then(|rest| rest.trim_end_matches(".partial").parse::<u32>().ok());
        if generation.is_some_and(|g| g >= current) {
            continue;
        }
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

unsafe extern "thiscall" fn list_update_ready_hook(this: *mut u8) {
    let orig = ORIGINAL_LIST.load(Ordering::Relaxed);
    if !orig.is_null() {
        let f: UpdateReadyFn = std::mem::transmute(orig);
        f(this);
    }
    let _ = std::panic::catch_unwind(|| observe("player list"));
}

unsafe extern "thiscall" fn net_list_update_ready_hook(this: *mut u8) {
    let orig = ORIGINAL_NET_LIST.load(Ordering::Relaxed);
    if !orig.is_null() {
        let f: UpdateReadyFn = std::mem::transmute(orig);
        f(this);
    }
    let _ = std::panic::catch_unwind(|| observe("network list"));
}

/// Returns the answer WA gets: its own, held back while the lit generation
/// has not reached every player (`transfer::start_allowed`).
unsafe extern "system" fn after_start_gate(result: u32, players: u32) -> u32 {
    if players != PLAYERS.load(Ordering::Relaxed) {
        return result;
    }
    let _ = std::panic::catch_unwind(|| observe("start gate"));
    let wa_allows = result & 0xFF != 0;
    match std::panic::catch_unwind(|| crate::transfer::start_allowed(wa_allows)) {
        Ok(allowed) if wa_allows && !allowed => 0,
        _ => result,
    }
}

/// `CPlayers::IsOkToStartNetGame`: EDI = CPlayers, no stack arguments, the
/// result in EAX. Every other register and the flags survive the observer;
/// EAX becomes its answer, written into the slot `pushad` saved it in.
#[unsafe(naked)]
unsafe extern "C" fn start_gate_hook() {
    core::arch::naked_asm!(
        "call dword ptr [{original}]",
        "pushfd",
        "pushad",
        "push edi",
        "push eax",
        "call {after}",
        "mov dword ptr [esp + 28], eax",
        "popad",
        "popfd",
        "ret",
        original = sym ORIGINAL_GATE,
        after = sym after_start_gate,
    );
}
