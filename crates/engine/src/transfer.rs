//! Moving a sync generation from host to clients over WA's lobby
//! (`docs/network-asset-sync-plan.md`, step 2).
//!
//! Packets ride WA's own lobby connections as packet type 0x4657
//! (`netsync::proto`). The host offers each generation it freezes to every
//! connected slot and answers requests from memory; a client pulls the
//! manifest and the files it cannot verify locally, then publishes its own
//! session directory and reports back.
//!
//! The host speaks first, and only to a slot it already has in the lobby: one
//! whose WA lobby packets its lobby handler has been receiving for a few
//! seconds.
//! A client never starts a conversation; it answers the host's hello from its
//! own lobby handler. Tested live, a client greeting the host the moment it
//! joined was dropped with a network error: while WA is still admitting a
//! machine, a packet type it does not know is a protocol error, not ignored.
//! After the hello, everything goes only to slots that answered it.
//!
//! Receiving hooks both lobby packet handlers. Sending goes through the same
//! connection objects WA's `HostUnicast` (0x58F9F0) and the client's send
//! use: the connection's vtable+0x34, `(this, data, size)`. Everything that
//! touches WA's connections runs on the lobby thread: in a packet handler, or
//! in a timer on that thread which also drives retries. Disk work runs on
//! worker threads and hands its results back through `WORK`.
//!
//! Nothing is sent during a match: a match reuses the same connections, and
//! a stray packet there must not reach the game's own packet parser.

use crate::log::{log_error as log_error_raw, log_line as log_line_raw, panic_text, wa_directory, wormforge_dir, BUILD_ID};
use crate::rebase::rb;
use game::entity::read_i32;
use minhook::MinHook;
use netsync::download::{Download, Event};
use netsync::proto::{self, Message, State};
use netsync::store::Served;
use netsync::{Manifest, FrozenFile};
use re_catalog::Catalog;
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows_sys::Win32::UI::WindowsAndMessaging::SetTimer;

static ORIGINAL_HOST: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_CLIENT: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_HOST_END: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_CLIENT_END: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static HOST_SESSION: AtomicU32 = AtomicU32::new(0);

/// Wall-clock UTC time, so logs from two machines can be lined up.
fn clock() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let s = (ms / 1000) % 86_400;
    format!("{:02}:{:02}:{:02}.{:03}Z", s / 3600, s / 60 % 60, s % 60, ms % 1000)
}

fn log_line(text: &str) {
    log_line_raw(&format!("{} {text}", clock()));
}

fn log_error(text: &str) {
    log_error_raw(&format!("{} {text}", clock()));
}

// ---------------------------------------------------------------- diagnostics
//
// Markers next to WA.exe, for live debugging:
// - `sync_trace.txt`: trace the first TRACE_PER_PEER packets the lobby hooks
//   see from each peer, with the thread and the connection's mode flags, and
//   every send.
// - `sync_quiet.txt`: send nothing, to tell our packets apart from our hooks.
//   It is how the first live tests found that packets of our own type
//   dropped the peer, while the hooks alone did not.

const TRACE_PER_PEER: u32 = 40;
/// Packets traced per host slot, and index HOST_SLOTS for the client link.
static TRACED: [AtomicU32; HOST_SLOTS + 1] = [const { AtomicU32::new(0) }; HOST_SLOTS + 1];

fn thread_id() -> u32 {
    unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() }
}

fn tracing() -> bool {
    static TRACING: OnceLock<bool> = OnceLock::new();
    *TRACING.get_or_init(|| wa_directory().join("sync_trace.txt").is_file())
}

fn trace_packet(peer: usize, from: &str, packet: *const u8, size: usize) {
    if !tracing() {
        return;
    }
    let Some(count) = TRACED.get(peer) else { return };
    if count.fetch_add(1, Ordering::Relaxed) >= TRACE_PER_PEER {
        return;
    }
    let kind = if packet.is_null() || size < 2 {
        "short".to_string()
    } else {
        let b = unsafe { std::slice::from_raw_parts(packet, size.min(2)) };
        format!("{:#06x}", u16::from_le_bytes([b[0], b[1]]))
    };
    let link = CLIENT_LINK.load(Ordering::Relaxed) as *mut u8;
    let fields = if peer == HOST_SLOTS && !link.is_null() {
        format!(" link [{}]", unsafe { describe_connection(link) })
    } else {
        String::new()
    };
    log_line(&format!("trace: recv from {from} type {kind} size {size} thread {}{fields}", thread_id()));
}

fn quiet() -> bool {
    static QUIET: OnceLock<bool> = OnceLock::new();
    *QUIET.get_or_init(|| {
        let q = wa_directory().join("sync_quiet.txt").is_file();
        if q {
            log_line_raw("sync: sync_quiet.txt present; sending nothing");
        }
        q
    })
}

fn kind(message: &Message) -> &'static str {
    match message {
        Message::Hello { .. } => "hello",
        Message::Offer { .. } => "offer",
        Message::Withdraw { .. } => "withdraw",
        Message::Get { .. } => "get",
        Message::Data { .. } => "data",
        Message::Missing { .. } => "missing",
        Message::Status { .. } => "status",
        Message::Ack { .. } => "ack",
    }
}

/// Trace a send, except the bulk ones after the first few.
fn trace_send(to: &str, message: &Message, bytes: usize, sent: bool) {
    static BULK: AtomicU32 = AtomicU32::new(0);
    if !tracing() {
        return;
    }
    if matches!(message, Message::Get { .. } | Message::Data { .. })
        && BULK.fetch_add(1, Ordering::Relaxed) >= 20
    {
        return;
    }
    log_line(&format!(
        "trace: send {} to {to} ({bytes} bytes): {} thread {}",
        kind(message),
        if sent { "sent" } else { "not sent" },
        thread_id()
    ));
}
static CLIENT_LINK: AtomicU32 = AtomicU32::new(0);

/// Set by a lobby hook, cleared when a match starts.
static IN_LOBBY: AtomicBool = AtomicBool::new(false);
static TIMER: AtomicUsize = AtomicUsize::new(0);
const TICK_MS: u32 = 100;

const HOST_SLOTS: usize = 6;
const SLOT_STRIDE: usize = 0x19118;
const SLOT_STATE: usize = 0x25C;
const SLOT_CONNECTION: usize = 0x1A4;
const HOSTING: usize = 0xC;
const LINK_STATE: usize = 0xB8;
const CONNECTED: i32 = 3;
/// Connection vtable+0x34: send `(this, data, size)`.
const VT_SEND: usize = 0x34 / 4;

type HostHandlerFn = unsafe extern "thiscall" fn(*mut u8, i32, *const u8, usize) -> i32;
type ClientHandlerFn = unsafe extern "thiscall" fn(*mut u8, *const u8, usize) -> i32;
type HostEndHandlerFn = unsafe extern "thiscall" fn(*mut u8, i32, *const u8, usize);
type SendFn = unsafe extern "thiscall" fn(*mut u8, *const u8, usize) -> i32;

// WA 3.8.1 bytes after the first ones, from wkTerrainSync's signatures.
const HOST_BYTES: &[Option<u8>] = &[
    None, None, None, None, None, None, None, Some(0x83), Some(0xF8), Some(0x02), Some(0x53),
    Some(0x8B), Some(0xD9), Some(0x89), Some(0x5C), Some(0x24), Some(0x04), Some(0x0F), Some(0x82),
];
// The end screens', from the same signatures.
const HOST_END_BYTES: &[Option<u8>] = &[
    None, None, None, None, None, None, None, Some(0x64), Some(0xA1), None, None, None, None,
    Some(0x50), Some(0x64), Some(0x89), Some(0x25), None, None, None, None, Some(0x83), Some(0xEC),
    Some(0x20), Some(0x53), Some(0x55), Some(0x56), Some(0x8B), Some(0x74), Some(0x24), Some(0x40),
];
const CLIENT_END_BYTES: &[Option<u8>] = &[
    None, None, None, None, None, None, Some(0x83), Some(0xEC), Some(0x08), Some(0x56), Some(0x8B),
    Some(0x75), Some(0x08), Some(0x0F), Some(0xB7), Some(0x06),
];
const CLIENT_BYTES: &[Option<u8>] = &[
    None, None, None, None, None, None, Some(0x83), Some(0xEC), Some(0x24), Some(0x53), Some(0x8B),
    Some(0x5D), Some(0x0C), Some(0x83), Some(0xFB), Some(0x02),
];

pub fn install(catalog: &Catalog) -> Result<(), String> {
    let host = catalog.global_va("G_HOST_SESSION").ok_or("missing G_HOST_SESSION")?;
    let link = catalog.global_va("G_CLIENT_LINK").ok_or("missing G_CLIENT_LINK")?;
    let host_handler = crate::lobby::target(catalog, "HostLobby__HandlePacket", HOST_BYTES)?;
    let client_handler = crate::lobby::target(catalog, "ClientLobby__HandlePacket", CLIENT_BYTES)?;
    let host_end = crate::lobby::target(catalog, "HostEndscreen__HandlePacket", HOST_END_BYTES)?;
    let client_end = crate::lobby::target(catalog, "ClientEndscreen__HandlePacket", CLIENT_END_BYTES)?;
    unsafe {
        let tr = MinHook::create_hook(host_handler, host_packet_hook as *mut c_void)
            .map_err(|e| format!("host lobby packet hook: {e:?}"))?;
        ORIGINAL_HOST.store(tr, Ordering::SeqCst);
        let tr = MinHook::create_hook(client_handler, client_packet_hook as *mut c_void)
            .map_err(|e| format!("client lobby packet hook: {e:?}"))?;
        ORIGINAL_CLIENT.store(tr, Ordering::SeqCst);
        let tr = MinHook::create_hook(host_end, host_end_packet_hook as *mut c_void)
            .map_err(|e| format!("host end-screen packet hook: {e:?}"))?;
        ORIGINAL_HOST_END.store(tr, Ordering::SeqCst);
        let tr = MinHook::create_hook(client_end, client_end_packet_hook as *mut c_void)
            .map_err(|e| format!("client end-screen packet hook: {e:?}"))?;
        ORIGINAL_CLIENT_END.store(tr, Ordering::SeqCst);
    }
    HOST_SESSION.store(rb(host), Ordering::SeqCst);
    CLIENT_LINK.store(rb(link), Ordering::SeqCst);
    // Its first bytes are wkFOW's hook when that module is loaded, so only the
    // address is taken from the catalog; showing a note is optional.
    if let Some(va) = catalog.function_va("Lobby__DisplayMessage") {
        DISPLAY.store(rb(va), Ordering::SeqCst);
    }
    log_line(&format!("lobby transfer installed (packet type {:#06x}, protocol {})", proto::PACKET_ID, proto::PROTOCOL));
    Ok(())
}

// ------------------------------------------------------------------ sending

/// Our messages ride inside WA chat packets (type 0): chat text, a NUL, then
/// ESC "WF" and the message. Live tests found that a packet of our own type
/// never reached the peer's lobby handler and dropped the peer within 100 ms,
/// while chat always arrives. Our hooks swallow these before WA shows them. A
/// player without WormForge sees only the text before the NUL, so the hello
/// -- the one message such a player can receive -- carries a readable line.
const CHAT_MARK: &[u8] = b"\x1bWF";

fn wrap(message: &Message) -> Vec<u8> {
    let mut out = vec![0u8, 0u8];
    if let Message::Hello { .. } = message {
        out.extend_from_slice(format!("WormForge {BUILD_ID}: lobby asset sync").as_bytes());
    }
    out.push(0);
    out.extend_from_slice(CHAT_MARK);
    out.extend_from_slice(&proto::encode(message));
    out
}

fn unwrap_chat(bytes: &[u8]) -> Option<&[u8]> {
    let text = bytes.strip_prefix(&[0u8, 0u8])?;
    let nul = text.iter().position(|&b| b == 0)?;
    text[nul + 1..].strip_prefix(CHAT_MARK)
}

/// The connection fields WA's framing reads, for the trace.
unsafe fn describe_connection(connection: *mut u8) -> String {
    format!(
        "state {} mode {} admitted {} flag {}",
        read_i32(connection, LINK_STATE),
        *connection.add(0xBC),
        *connection.add(0xBD),
        *connection.add(0xBE)
    )
}

unsafe fn send_on(connection: *mut u8, bytes: &[u8]) {
    let vtable = *(connection as *const *const usize);
    let send: SendFn = std::mem::transmute(*vtable.add(VT_SEND));
    send(connection, bytes.as_ptr(), bytes.len());
}

/// To one client, as `HostUnicast` does. False when that slot is not connected.
fn host_send(slot: usize, message: &Message) -> bool {
    if slot >= HOST_SLOTS || !IN_LOBBY.load(Ordering::SeqCst) {
        return false;
    }
    if quiet() {
        trace_send(&format!("slot {slot}"), message, 0, false);
        return false;
    }
    let host = HOST_SESSION.load(Ordering::Relaxed) as *mut u8;
    if host.is_null() {
        return false;
    }
    unsafe {
        if read_i32(host, HOSTING) == 0 {
            return false;
        }
        let record = host.add(slot * SLOT_STRIDE);
        if read_i32(record, SLOT_STATE) != CONNECTED {
            return false;
        }
        let bytes = wrap(message);
        let connection = record.add(SLOT_CONNECTION);
        let fields = describe_connection(connection);
        send_on(connection, &bytes);
        trace_send(&format!("slot {slot} [{fields}]"), message, bytes.len(), true);
    }
    true
}

/// To every slot that answered our hello.
fn host_broadcast(message: &Message) -> Vec<usize> {
    let peers = PEERS.lock().map(|p| *p).unwrap_or_default();
    (0..HOST_SLOTS).filter(|&s| peers[s].wormforge && host_send(s, message)).collect()
}

fn slot_connected(slot: usize) -> bool {
    let host = HOST_SESSION.load(Ordering::Relaxed) as *mut u8;
    !host.is_null()
        && unsafe {
            read_i32(host, HOSTING) != 0
                && read_i32(host.add(slot * SLOT_STRIDE), SLOT_STATE) == CONNECTED
        }
}

fn client_send(message: &Message) -> bool {
    if !IN_LOBBY.load(Ordering::SeqCst) {
        return false;
    }
    if quiet() {
        trace_send("host", message, 0, false);
        return false;
    }
    let link = CLIENT_LINK.load(Ordering::Relaxed) as *mut u8;
    if link.is_null() {
        return false;
    }
    unsafe {
        if read_i32(link, LINK_STATE) != CONNECTED {
            return false;
        }
        let bytes = wrap(message);
        let fields = describe_connection(link);
        send_on(link, &bytes);
        trace_send(&format!("host [{fields}]"), message, bytes.len(), true);
    }
    true
}

// -------------------------------------------------------------------- notes
//
// Lines for the player, in the lobby's own message log. WA's display call
// (0x493CB0, stdcall: log, text) needs a live lobby screen, and the only
// moment one is known to be alive is inside a packet hook, whose `this` is
// the screen. So notes queue up and are shown from the next hook call; the
// host acknowledges each client status, so a client always gets one soon.

/// The screen's message log, as WA's own chat handler addresses it.
const SCREEN_LOG: usize = 0x10318;
static DISPLAY: AtomicU32 = AtomicU32::new(0);
static NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());

type DisplayFn = unsafe extern "system" fn(*mut u8, *const u8) -> i32;

fn note(text: String) {
    log_line(&format!("note: {text}"));
    if let Ok(mut n) = NOTES.lock() {
        if n.len() < 32 {
            n.push(text);
        }
    }
}

/// Show queued notes on `screen`, which a lobby hook just received.
fn show_notes(screen: *mut u8) {
    let display = DISPLAY.load(Ordering::Relaxed);
    if screen.is_null() || display == 0 || !IN_LOBBY.load(Ordering::SeqCst) {
        return;
    }
    let notes: Vec<String> = NOTES.lock().map(|mut n| std::mem::take(&mut *n)).unwrap_or_default();
    for text in notes {
        // WA's system-message prefix, as wkFOW and PX use it.
        let line = format!("SYS::ALL:WormForge: {text}\0");
        unsafe {
            let f: DisplayFn = std::mem::transmute(display as usize);
            f(screen.add(SCREEN_LOG), line.as_ptr());
        }
    }
}

/// A client's player name, from its connection (+0xD0, as wkTerrainSync reads it).
fn slot_name(slot: usize) -> String {
    let host = HOST_SESSION.load(Ordering::Relaxed) as *const u8;
    if host.is_null() || slot >= HOST_SLOTS {
        return format!("slot {slot}");
    }
    let raw = unsafe { std::slice::from_raw_parts(host.add(slot * SLOT_STRIDE + SLOT_CONNECTION + 0xD0), 17) };
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let name: String = raw[..end].iter().map(|&b| b as char).filter(|c| !c.is_control()).collect();
    if name.is_empty() { format!("slot {slot}") } else { name }
}

// ---------------------------------------------------------------- lifecycle

/// A lobby hook ran: we are on the lobby thread, in a lobby.
pub fn entered_lobby() {
    IN_LOBBY.store(true, Ordering::SeqCst);
    if TIMER.load(Ordering::SeqCst) == 0 {
        let id = unsafe { SetTimer(std::ptr::null_mut(), 0, TICK_MS, Some(tick)) };
        if id == 0 {
            log_error("lobby transfer: SetTimer failed; retries will wait for lobby traffic");
        } else {
            TIMER.store(id, Ordering::SeqCst);
        }
    }
}

/// The pack folder a network round loads from, chosen as the round starts:
/// the host's lit generation, or the copy a client took of the host's latest
/// offer. `None` for an offline round, or when no synced copy fits, and then
/// the round loads installed `Mods/` as before.
pub fn round_packs() -> Option<PathBuf> {
    let host = HOST_SESSION.load(Ordering::Relaxed) as *mut u8;
    let hosting = !host.is_null() && unsafe { read_i32(host, HOSTING) } != 0;
    if hosting {
        let generation = crate::lobby::current_generation();
        let served = SERVED.lock().ok().and_then(|s| s.as_ref().map(|s| s.generation));
        let dir = SERVED_DIR.lock().ok().and_then(|d| d.clone());
        return match (generation, served == Some(generation), dir) {
            (0, _, _) => None,
            (g, true, Some(dir)) => {
                log_line(&format!("sync: round loads generation {g} from {}", dir.display()));
                Some(dir.join(netsync::PACKS_DIR))
            }
            (g, _, _) => {
                log_error(&format!("sync: generation {g} is not frozen yet; round loads installed packs"));
                None
            }
        };
    }
    if !client_link_up() {
        return None;
    }
    let offered = LAST_OFFER.lock().ok().and_then(|o| *o);
    let ready = READY.lock().ok().and_then(|r| r.clone());
    match (offered, ready) {
        (Some((g, digest)), Some((have, dir))) if have == digest => {
            log_line(&format!("sync: round loads the host's generation {g} from {}", dir.display()));
            Some(dir.join(netsync::PACKS_DIR))
        }
        (Some((g, _)), _) => {
            log_error(&format!("sync: host's generation {g} is not here; round loads installed packs"));
            None
        }
        (None, _) => None,
    }
}

fn client_link_up() -> bool {
    let link = CLIENT_LINK.load(Ordering::Relaxed) as *mut u8;
    !link.is_null() && unsafe { read_i32(link, LINK_STATE) } == CONNECTED
}

pub fn match_started() {
    IN_LOBBY.store(false, Ordering::SeqCst);
    if let Ok(mut n) = NOTES.lock() {
        n.clear();
    }
    for t in &TRACED {
        t.store(0, Ordering::Relaxed);
    }
    // Who runs WormForge, and whom we greeted, holds for as long as the
    // connection does: the end screen between rounds has no greeting phase.
    // `greet` forgets a slot once its connection is gone.
    if let Ok(mut job) = CLIENT.lock() {
        if let Some(j) = job.take() {
            log_line(&format!("sync: download of generation {} stopped: the round started", j.generation()));
        }
    }
}

extern "system" fn tick(_: *mut c_void, _: u32, _: usize, _: u32) {
    if !IN_LOBBY.load(Ordering::SeqCst) {
        return;
    }
    if let Err(p) = std::panic::catch_unwind(pump) {
        log_error(&format!("sync pump panicked: {}", panic_text(p)));
    }
}

fn pump() {
    host_pump();
    client_pump();
}

// --------------------------------------------------------------------- host

/// The generation being served, once loaded, and its session directory.
static SERVED: Mutex<Option<Arc<Served>>> = Mutex::new(None);
static SERVED_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
static OFFER_PENDING: AtomicBool = AtomicBool::new(false);
/// What the host knows of each client slot.
#[derive(Clone, Copy, Default)]
struct Peer {
    /// When its lobby handler first saw a WA packet from this slot.
    seen: Option<Instant>,
    /// When we said hello.
    greeted: Option<Instant>,
    /// Answered our hello: runs WormForge.
    wormforge: bool,
    /// Told the lobby this player has no WormForge.
    reported: bool,
}

static PEERS: Mutex<[Peer; HOST_SLOTS]> = Mutex::new(
    [Peer { seen: None, greeted: None, wormforge: false, reported: false }; HOST_SLOTS],
);
/// A greeted player who has not answered in this long has no WormForge.
const ANSWER: Duration = Duration::from_secs(15);
/// How long a slot must have been in the lobby before it is greeted.
const SETTLE: Duration = Duration::from_secs(3);

/// Latest status per client slot, for the Start gate in step 3.
static SLOT_STATUS: Mutex<[Option<(u32, State)>; HOST_SLOTS]> = Mutex::new([None; HOST_SLOTS]);

/// Called by the freeze worker once a generation is published.
pub fn serve(served: Served, dir: PathBuf) {
    let generation = served.generation;
    if crate::lobby::current_generation() != generation {
        return; // withdrawn while it loaded
    }
    if let Ok(mut s) = SERVED.lock() {
        *s = Some(Arc::new(served));
    }
    if let Ok(mut d) = SERVED_DIR.lock() {
        *d = Some(dir);
    }
    OFFER_PENDING.store(true, Ordering::SeqCst);
    log_line(&format!("sync generation {generation}: ready to offer"));
}

/// Called on the lobby thread when the host's bulb goes out.
pub fn withdraw(generation: u32) {
    let current = SERVED.lock().ok().and_then(|mut s| s.take());
    OFFER_PENDING.store(false, Ordering::SeqCst);
    if current.is_some_and(|s| s.generation == generation) {
        let slots = host_broadcast(&Message::Withdraw { generation });
        log_line(&format!("sync generation {generation}: withdrawn from slots {slots:?}"));
    }
}

fn host_pump() {
    greet();
    if !OFFER_PENDING.swap(false, Ordering::SeqCst) {
        return;
    }
    let Some(served) = SERVED.lock().ok().and_then(|s| s.clone()) else {
        return;
    };
    let slots = host_broadcast(&served.offer());
    log_line(&format!("sync generation {}: offered to slots {slots:?}", served.generation));
    if !slots.is_empty() {
        note(format!("sending the packs to {} player(s)", slots.len()));
    }
}

/// The Start gate, from WA's `IsOkToStartNetGame` on the host: WA's own
/// answer, overruled while a lit generation has not reached every player.
pub fn start_allowed(wa_allows: bool) -> bool {
    static LAST: Mutex<Option<Result<(), String>>> = Mutex::new(None);
    let generation = crate::lobby::current_generation();
    let served = SERVED.lock().ok().and_then(|s| s.as_ref().map(|s| (s.generation, s.packs)));
    let now = Instant::now();
    let peers = PEERS.lock().map(|p| *p).unwrap_or_default();
    let statuses = SLOT_STATUS.lock().map(|s| *s).unwrap_or([None; HOST_SLOTS]);
    let views: Vec<netsync::gate::PeerView> = (0..HOST_SLOTS)
        .filter(|&slot| slot_connected(slot))
        .map(|slot| netsync::gate::PeerView {
            name: slot_name(slot),
            in_lobby: peers[slot].seen.is_some(),
            wormforge: peers[slot].wormforge,
            gave_up: peers[slot].greeted.is_some_and(|t| now.duration_since(t) >= ANSWER),
            status: statuses[slot],
        })
        .collect();
    let verdict = netsync::gate::verdict(generation, served, &views);
    if let Ok(mut last) = LAST.lock() {
        if last.as_ref() != Some(&verdict) {
            match &verdict {
                Ok(()) if generation != 0 && !views.is_empty() => note("every player has the packs".into()),
                Ok(()) => {}
                Err(why) => {
                    log_line(&format!("sync: start held: {why}"));
                    if wa_allows {
                        note(format!("start waits: {why}"));
                    }
                }
            }
            *last = Some(verdict.clone());
        }
    }
    wa_allows && verdict.is_ok()
}

/// Greet each slot that has settled into the lobby; forget slots that left.
fn greet() {
    let now = Instant::now();
    let mut hello = Vec::new();
    if let Ok(mut peers) = PEERS.lock() {
        for (slot, peer) in peers.iter_mut().enumerate() {
            if !slot_connected(slot) {
                if peer.seen.is_some() {
                    log_line(&format!("sync: slot {slot} left"));
                    TRACED[slot].store(0, Ordering::Relaxed);
                    if let Ok(mut st) = SLOT_STATUS.lock() {
                        st[slot] = None;
                    }
                }
                *peer = Peer::default();
            } else if peer.greeted.is_none() && peer.seen.is_some_and(|t| now.duration_since(t) >= SETTLE) {
                peer.greeted = Some(now);
                hello.push(slot);
            } else if !peer.wormforge
                && !peer.reported
                && peer.greeted.is_some_and(|t| now.duration_since(t) >= ANSWER)
            {
                peer.reported = true;
                note(format!("{} has no WormForge; they cannot join a round with synced packs", slot_name(slot)));
            }
        }
    }
    for slot in hello {
        if host_send(slot, &Message::Hello { build: BUILD_ID.into() }) {
            log_line(&format!("sync: greeted slot {slot}"));
        }
    }
}

/// A WA packet from `slot` reached the host's lobby handler.
fn seen_in_lobby(slot: i32) {
    if let Ok(mut peers) = PEERS.lock() {
        if let Some(peer) = usize::try_from(slot).ok().and_then(|s| peers.get_mut(s)) {
            peer.seen.get_or_insert_with(Instant::now);
        }
    }
}

/// Returns true when the message can change the Start gate's verdict.
fn host_receive(slot: usize, message: Message) -> bool {
    let changes_gate = matches!(message, Message::Hello { .. } | Message::Status { .. });
    if let Ok(mut peers) = PEERS.lock() {
        if peers[slot].greeted.is_none() {
            return false; // not ours to answer: we never spoke to this slot
        }
        if let Message::Hello { .. } = message {
            peers[slot].wormforge = true;
        }
    }
    match message {
        Message::Hello { build } => {
            log_line(&format!("sync: slot {slot} runs WormForge {build}"));
            if let Some(served) = SERVED.lock().ok().and_then(|s| s.clone()) {
                if host_send(slot, &served.offer()) {
                    log_line(&format!("sync generation {}: offered to slot {slot}", served.generation));
                }
            }
        }
        request @ Message::Get { .. } => {
            let served = SERVED.lock().ok().and_then(|s| s.clone());
            let answer = match served {
                Some(s) => s.answer(&request),
                None => match request {
                    Message::Get { generation, object, .. } => Some(Message::Missing { generation, object }),
                    _ => None,
                },
            };
            if let Some(a) = answer {
                host_send(slot, &a);
            }
        }
        Message::Status { generation, state, have, total, text, .. } => {
            if let Ok(mut st) = SLOT_STATUS.lock() {
                st[slot] = Some((generation, state));
            }
            match state {
                State::Working => log_line(&format!(
                    "sync: slot {slot} generation {generation}: {have} of {total} bytes"
                )),
                State::Ready => {
                    log_line(&format!("sync: slot {slot} has generation {generation}"));
                    note(format!("{} has the packs", slot_name(slot)));
                }
                State::Failed => {
                    log_error(&format!("sync: slot {slot} could not take generation {generation}: {text}"));
                    note(format!("{} could not take the packs: {text}", slot_name(slot)));
                }
            }
            if state != State::Working {
                host_send(slot, &Message::Ack { generation });
            }
        }
        _ => {}
    }
    changes_gate
}

/// WA evaluates its Start check only on lobby events (a bulb, a join), not
/// when our status arrives. Ask the host screen to refresh its Start button,
/// as its own bulb handler does: vtable+0x170 (0x4B7E20) calls
/// `IsOkToStartNetGame` and enables or disables Start. Only from a host
/// packet hook, where `screen` is the live host lobby screen.
unsafe fn refresh_start(screen: *mut u8) {
    if screen.is_null() {
        return;
    }
    let vtable = *(screen as *const *const usize);
    let refresh: unsafe extern "thiscall" fn(*mut u8) = std::mem::transmute(*vtable.add(0x170 / 4));
    refresh(screen);
}

// ------------------------------------------------------------------- client

static CLIENT: Mutex<Option<ClientJob>> = Mutex::new(None);
/// The last session this client published: (manifest digest, directory).
static READY: Mutex<Option<([u8; 32], PathBuf)>> = Mutex::new(None);
static CLIENT_SESSIONS: AtomicU32 = AtomicU32::new(0);
/// The host's latest offer: generation and manifest digest.
static LAST_OFFER: Mutex<Option<(u32, [u8; 32])>> = Mutex::new(None);

enum ClientJob {
    Downloading { download: Download, manifest: Option<Arc<Manifest>>, sources: Option<Arc<Sources>>, last_report: Instant },
    Assembling { generation: u32 },
}

impl ClientJob {
    fn generation(&self) -> u32 {
        match self {
            ClientJob::Downloading { download, .. } => download.generation,
            ClientJob::Assembling { generation } => *generation,
        }
    }
}

/// Where each file of a manifest can be had on this machine, by hash.
#[derive(Default)]
struct Sources {
    local: HashMap<[u8; 32], PathBuf>,
    collection: std::collections::HashSet<[u8; 32]>,
}

enum Work {
    Resolved { generation: u32, sources: Sources, need: Vec<([u8; 32], u64)> },
    Published { generation: u32, digest: [u8; 32], dir: PathBuf, total: u64 },
    Failed { generation: u32, digest: [u8; 32], reason: String },
}

static WORK: Mutex<Vec<Work>> = Mutex::new(Vec::new());

fn client_receive(message: Message) {
    match message {
        Message::Hello { build } => {
            log_line(&format!("sync: host runs WormForge {build}"));
            if client_send(&Message::Hello { build: BUILD_ID.into() }) {
                log_line("sync: answered the host's hello");
            }
        }
        Message::Offer { generation, digest, size } => offer(generation, digest, size),
        Message::Ack { .. } => {} // its hook call shows our queued notes
        Message::Withdraw { generation } => {
            if let Ok(mut offer) = LAST_OFFER.lock() {
                if offer.is_some_and(|(g, _)| g == generation) {
                    *offer = None;
                }
            }
            if let Ok(mut job) = CLIENT.lock() {
                if job.as_ref().is_some_and(|j| j.generation() == generation) {
                    *job = None;
                    log_line(&format!("sync: host withdrew generation {generation}"));
                }
            }
        }
        m @ (Message::Data { .. } | Message::Missing { .. }) => {
            // Refill the window at once rather than on the next tick, so the
            // transfer runs at the link's round trip, not the timer's.
            let mut requests = Vec::new();
            let event = match CLIENT.lock() {
                Ok(mut job) => match job.as_mut() {
                    Some(ClientJob::Downloading { download, .. }) => {
                        let event = download.receive(&m);
                        if event.is_none() {
                            requests = download.poll(Instant::now()).unwrap_or_default();
                        }
                        event
                    }
                    _ => None,
                },
                Err(_) => None,
            };
            for r in &requests {
                client_send(r);
            }
            if let Some(e) = event {
                download_event(e);
            }
        }
        _ => {}
    }
}

fn offer(generation: u32, digest: [u8; 32], size: u32) {
    let hex = netsync::hex(&digest);
    if let Ok(mut o) = LAST_OFFER.lock() {
        *o = Some((generation, digest));
    }
    let ready = READY.lock().ok().and_then(|r| r.clone());
    if ready.as_ref().is_some_and(|(d, _)| *d == digest) {
        if let Ok(mut job) = CLIENT.lock() {
            *job = None;
        }
        log_line(&format!("sync: generation {generation} ({}) is already here", &hex[..12]));
        note("the host's packs are already here".into());
        report(generation, digest, State::Ready, 0, 0, "");
        return;
    }
    let Ok(mut job) = CLIENT.lock() else { return };
    if let Some(ClientJob::Downloading { download, .. }) = job.as_ref() {
        if download.generation == generation && download.digest == digest {
            return; // the host re-offered what we are already fetching
        }
    }
    match Download::new(generation, digest, size) {
        Ok(download) => {
            log_line(&format!("sync: host offers generation {generation} ({}), fetching its manifest", &hex[..12]));
            note("receiving the host's packs".into());
            *job = Some(ClientJob::Downloading { download, manifest: None, sources: None, last_report: Instant::now() });
        }
        Err(e) => {
            *job = None;
            drop(job);
            log_error(&format!("sync: generation {generation} refused: {e}"));
            report(generation, digest, State::Failed, 0, 0, &e);
        }
    }
}

fn report(generation: u32, digest: [u8; 32], state: State, have: u64, total: u64, text: &str) {
    client_send(&Message::Status { generation, digest, state, have, total, text: text.into() });
}

/// An event from the download, on the lobby thread.
fn download_event(event: Event) {
    let Ok(mut guard) = CLIENT.lock() else { return };
    let Some(ClientJob::Downloading { download, manifest, sources, .. }) = guard.as_mut() else {
        return;
    };
    let generation = download.generation;
    let digest = download.digest;
    match event {
        Event::Manifest(m) => {
            log_line(&format!(
                "sync: generation {generation} manifest: {} packs, {} files, {} bytes; checking what is here",
                m.packs.len(),
                m.files.len(),
                m.total_bytes()
            ));
            let m = Arc::new(m);
            *manifest = Some(m.clone());
            worker(format!("wf-sync-resolve-{generation}"), move || resolve(generation, &m));
        }
        Event::Complete(blobs) => {
            let Some(m) = manifest.clone() else { return };
            let s = sources.clone().unwrap_or_default();
            *guard = Some(ClientJob::Assembling { generation });
            drop(guard);
            worker(format!("wf-sync-publish-{generation}"), move || publish(generation, digest, &m, &s, blobs));
        }
        Event::Failed(reason) => {
            *guard = None;
            drop(guard);
            log_error(&format!("sync: generation {generation} failed: {reason}"));
            report(generation, digest, State::Failed, 0, 0, &reason);
        }
    }
}

fn client_pump() {
    let done: Vec<Work> = WORK.lock().map(|mut w| std::mem::take(&mut *w)).unwrap_or_default();
    for work in done {
        client_work(work);
    }
    let mut failure = None;
    let mut progress = None;
    if let Ok(mut guard) = CLIENT.lock() {
        if let Some(ClientJob::Downloading { download, last_report, .. }) = guard.as_mut() {
            let now = Instant::now();
            match download.poll(now) {
                Ok(requests) => {
                    for r in &requests {
                        client_send(r);
                    }
                }
                Err(e) => failure = Some((download.generation, download.digest, e)),
            }
            if now.duration_since(*last_report) >= Duration::from_secs(2) {
                *last_report = now;
                let (have, total) = download.progress();
                if total > 0 {
                    progress = Some((download.generation, download.digest, have, total));
                }
            }
        }
        if failure.is_some() {
            *guard = None;
        }
    }
    if let Some((g, d, e)) = failure {
        log_error(&format!("sync: generation {g} failed: {e}"));
        report(g, d, State::Failed, 0, 0, &e);
    }
    if let Some((g, d, have, total)) = progress {
        report(g, d, State::Working, have, total, "");
    }
}

fn client_work(work: Work) {
    match work {
        Work::Resolved { generation, sources, need } => {
            let event = {
                let Ok(mut guard) = CLIENT.lock() else { return };
                let Some(ClientJob::Downloading { download, sources: slot, .. }) = guard.as_mut() else { return };
                if download.generation != generation {
                    return;
                }
                let bytes: u64 = need.iter().map(|(_, s)| s).sum();
                log_line(&format!(
                    "sync: generation {generation}: {} files here, {} to fetch ({bytes} bytes)",
                    sources.local.len() + sources.collection.len(),
                    need.len()
                ));
                if !need.is_empty() {
                    note(format!("fetching {} file(s), {} KB", need.len(), bytes.div_ceil(1024)));
                }
                *slot = Some(Arc::new(sources));
                download.need(&need)
            };
            if let Some(e) = event {
                download_event(e);
            }
        }
        Work::Published { generation, digest, dir, total } => {
            if let Ok(mut guard) = CLIENT.lock() {
                if guard.as_ref().is_some_and(|j| j.generation() == generation) {
                    *guard = None;
                }
            }
            log_line(&format!("sync: generation {generation} ready ({total} bytes) at {}", dir.display()));
            note("the host's packs are ready".into());
            if let Ok(mut r) = READY.lock() {
                *r = Some((digest, dir));
            }
            report(generation, digest, State::Ready, total, total, "");
        }
        Work::Failed { generation, digest, reason } => {
            if let Ok(mut guard) = CLIENT.lock() {
                if guard.as_ref().is_some_and(|j| j.generation() == generation) {
                    *guard = None;
                }
            }
            log_error(&format!("sync: generation {generation} failed: {reason}"));
            note(format!("could not take the host's packs: {reason}"));
            report(generation, digest, State::Failed, 0, 0, &reason);
        }
    }
}

fn worker(name: String, job: impl FnOnce() + Send + 'static) {
    let label = name.clone();
    let spawned = std::thread::Builder::new().name(name.clone()).spawn(move || {
        if let Err(p) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)) {
            log_error(&format!("{label} panicked: {}", panic_text(p)));
        }
    });
    if let Err(e) = spawned {
        log_error(&format!("{name}: cannot start: {e}"));
    }
}

fn post(work: Work) {
    if let Ok(mut w) = WORK.lock() {
        w.push(work);
    }
}

/// Which files are already here: in an installed pack of the same id at the
/// same path, in the `.WFAssets` collection, or in the last session this
/// client published. Each is checked by hash, never trusted by name.
///
/// A `sync_fetch_all.txt` marker next to `WA.exe` skips all of that and
/// fetches every file, so two WA instances sharing one `Mods/` folder still
/// exercise the transfer.
fn resolve(generation: u32, manifest: &Manifest) {
    if wa_directory().join("sync_fetch_all.txt").is_file() {
        log_line("sync: sync_fetch_all.txt present; fetching every file from the host");
        let mut need: Vec<([u8; 32], u64)> = Vec::new();
        for f in &manifest.files {
            if !need.iter().any(|(s, _)| *s == f.sha256) {
                need.push((f.sha256, f.size));
            }
        }
        post(Work::Resolved { generation, sources: Sources::default(), need });
        return;
    }
    let mut roots: HashMap<String, PathBuf> = HashMap::new();
    if let Ok(entries) = weapons::list_mods(&lua_host::mods_root_from(&wa_directory())) {
        for e in entries {
            if let Ok(m) = e.manifest {
                roots.entry(m.id).or_insert(e.root);
            }
        }
    }
    let previous = READY.lock().ok().and_then(|r| r.clone()).map(|(_, dir)| dir);
    let collection = crate::replay::collection();
    let mut sources = Sources::default();
    let mut need = Vec::new();
    for f in &manifest.files {
        if sources.local.contains_key(&f.sha256) || sources.collection.contains(&f.sha256) {
            continue;
        }
        let candidates = [
            roots.get(&f.pack).map(|r| r.join(&f.path)),
            previous.as_ref().map(|d| d.join(netsync::PACKS_DIR).join(&f.pack).join(&f.path)),
        ];
        let local = candidates.into_iter().flatten().find(|p| matches(p, f));
        if let Some(path) = local {
            sources.local.insert(f.sha256, path);
        } else if collection.is_some_and(|c| c.contains(&f.sha256)) {
            sources.collection.insert(f.sha256);
        } else if !need.iter().any(|(s, _)| *s == f.sha256) {
            need.push((f.sha256, f.size));
        }
    }
    post(Work::Resolved { generation, sources, need });
}

fn matches(path: &std::path::Path, f: &FrozenFile) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == f.size)
        && std::fs::read(path).is_ok_and(|b| netsync::digest(&b) == f.sha256)
}

fn run_id() -> &'static str {
    static RUN: OnceLock<String> = OnceLock::new();
    RUN.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{}-{nanos}", std::process::id())
    })
}

fn publish(generation: u32, digest: [u8; 32], manifest: &Manifest, sources: &Sources, blobs: HashMap<[u8; 32], Vec<u8>>) {
    let n = CLIENT_SESSIONS.fetch_add(1, Ordering::SeqCst) + 1;
    let root = wormforge_dir().join("sync");
    let dest = root.join(format!("client-{}-{n}", run_id()));
    let result = std::fs::create_dir_all(&root)
        .map_err(|e| format!("{}: {e}", root.display()))
        .and_then(|_| {
            netsync::assemble(manifest, &dest, |f| {
                if let Some(b) = blobs.get(&f.sha256) {
                    return Ok(b.clone());
                }
                if let Some(p) = sources.local.get(&f.sha256) {
                    return std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()));
                }
                if sources.collection.contains(&f.sha256) {
                    if let Some(c) = crate::replay::collection() {
                        if let Ok(Some(b)) = c.get(&f.sha256) {
                            return Ok(b);
                        }
                    }
                }
                Err(format!("no verified copy of {}:{}", f.pack, f.path))
            })
        });
    match result {
        Ok(session) => {
            prune_clients(&dest);
            post(Work::Published { generation, digest, dir: session.dir, total: manifest.total_bytes() });
        }
        Err(reason) => post(Work::Failed { generation, digest, reason }),
    }
}

/// Keep only the newest client session. Nothing reads them yet.
fn prune_clients(keep: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(wormforge_dir().join("sync")) else { return };
    let ours = format!("client-{}-", run_id());
    let keep_n = keep
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix(&ours))
        .and_then(|n| n.parse::<u32>().ok())
        .unwrap_or(0);
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("client-") {
            continue;
        }
        let n = name.strip_prefix(&ours).and_then(|r| r.trim_end_matches(".partial").parse::<u32>().ok());
        if n.is_some_and(|n| n >= keep_n) {
            continue;
        }
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

// -------------------------------------------------------------------- hooks

fn take(packet: *const u8, size: usize) -> Option<Result<Message, String>> {
    if packet.is_null() || size < 2 {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(packet, size) };
    if let Some(inner) = unwrap_chat(bytes) {
        return Some(proto::decode(inner));
    }
    proto::is_ours(bytes).then(|| proto::decode(bytes))
}

/// Log a packet we could not read once per reason, not once per packet.
fn refused(from: &str, reason: String) {
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    if let Ok(mut seen) = SEEN.lock() {
        if !seen.contains(&reason) {
            log_error(&format!("sync: ignored a packet from {from}: {reason}"));
            seen.push(reason);
        }
    }
}

/// Our part of a host-side packet handler, lobby or end screen. Returns
/// true when the packet was ours and is swallowed.
///
/// Both screens' message logs sit at `this + 0x10318`, and both refresh
/// their Start button through their own vtable+0x170, as WA's handler at
/// 0x4AC0D0 does after a bulb packet.
unsafe fn host_packet(this: *mut u8, slot: i32, packet: *const u8, size: usize) -> bool {
    let _ = std::panic::catch_unwind(|| {
        if let Ok(s) = usize::try_from(slot) {
            trace_packet(s, &format!("slot {slot}"), packet, size);
        }
    });
    if let Some(parsed) = take(packet, size) {
        let refresh = std::panic::catch_unwind(|| {
            entered_lobby();
            let refresh = match parsed {
                Ok(m) if (0..HOST_SLOTS as i32).contains(&slot) => host_receive(slot as usize, m),
                Ok(_) => {
                    refused(&format!("slot {slot}"), "slot out of range".into());
                    false
                }
                Err(e) => {
                    refused(&format!("slot {slot}"), e);
                    false
                }
            };
            show_notes(this);
            refresh
        });
        if refresh.unwrap_or(false) {
            refresh_start(this);
        }
        return true;
    }
    // Notes before WA's handler: it may close the screen (a round starting).
    let _ = std::panic::catch_unwind(|| {
        entered_lobby();
        seen_in_lobby(slot);
        show_notes(this);
    });
    false
}

/// Our part of a client-side packet handler, lobby or end screen.
unsafe fn client_packet(this: *mut u8, packet: *const u8, size: usize) -> bool {
    let _ = std::panic::catch_unwind(|| trace_packet(HOST_SLOTS, "host", packet, size));
    if let Some(parsed) = take(packet, size) {
        let _ = std::panic::catch_unwind(|| {
            entered_lobby();
            match parsed {
                Ok(m) => client_receive(m),
                Err(e) => refused("the host", e),
            }
            show_notes(this);
        });
        return true;
    }
    let _ = std::panic::catch_unwind(|| {
        entered_lobby();
        show_notes(this);
    });
    false
}

unsafe extern "thiscall" fn host_packet_hook(this: *mut u8, slot: i32, packet: *const u8, size: usize) -> i32 {
    if host_packet(this, slot, packet, size) {
        return 0;
    }
    let orig = ORIGINAL_HOST.load(Ordering::Relaxed);
    if orig.is_null() {
        return 0;
    }
    let f: HostHandlerFn = std::mem::transmute(orig);
    f(this, slot, packet, size)
}

/// The end screen between rounds (0x4AC0D0). The host lobby handler also
/// hands it every type it does not handle itself, so a WA packet can pass
/// both hooks; ours are chat packets, which the lobby handler keeps.
unsafe extern "thiscall" fn host_end_packet_hook(this: *mut u8, slot: i32, packet: *const u8, size: usize) {
    if host_packet(this, slot, packet, size) {
        return;
    }
    let orig = ORIGINAL_HOST_END.load(Ordering::Relaxed);
    if !orig.is_null() {
        let f: HostEndHandlerFn = std::mem::transmute(orig);
        f(this, slot, packet, size);
    }
}

unsafe extern "thiscall" fn client_packet_hook(this: *mut u8, packet: *const u8, size: usize) -> i32 {
    if client_packet(this, packet, size) {
        return 0;
    }
    let orig = ORIGINAL_CLIENT.load(Ordering::Relaxed);
    if orig.is_null() {
        return 0;
    }
    let f: ClientHandlerFn = std::mem::transmute(orig);
    f(this, packet, size)
}

unsafe extern "thiscall" fn client_end_packet_hook(this: *mut u8, packet: *const u8, size: usize) -> i32 {
    if client_packet(this, packet, size) {
        return 0;
    }
    let orig = ORIGINAL_CLIENT_END.load(Ordering::Relaxed);
    if orig.is_null() {
        return 0;
    }
    let f: ClientHandlerFn = std::mem::transmute(orig);
    f(this, packet, size)
}
