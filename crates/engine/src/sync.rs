//! Synchronized Lua input and optional network determinism diagnostics.
//!
//! Worms Armageddon is a lockstep engine: every peer must consume identical
//! simulation input and advance identical state. `wa.world.keys()` and
//! `wa.world.pointer()` therefore publish compact, change-driven records over
//! a provisional private message kind before Lua reads them on either peer.
//!
//! The production transport is always enabled in networked matches. Builds
//! with `sync-diagnostics` also exchange component hashes for WA RNG, worms,
//! and WormForge actors so a divergence can be localized without changing the
//! production wire behavior.

use crate::{log_error, log_line};
use game::entity::read_i32;
use game::hash::Fnv;
use game::offsets::{
    ACTOR_PHASE, ACTOR_SPRITE, ENT_POS_X, ENT_POS_Y, ENT_SPEED_X, ENT_SPEED_Y, GAME_RUNTIME_WORLD,
    GAME_WORLD_FRAME_COUNTER, GAME_WORLD_GAME_RNG, GAME_WORLD_NET_SESSION, GAME_WORLD_RNG_STATE_1,
    GAME_WORLD_RNG_STATE_2,
};
use game::sync_input::{self, InputSample};
use std::ffi::c_void;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

/// Last frame we hashed, so a repeated or rewound counter is visible.
static LAST_FRAME: AtomicU64 = AtomicU64::new(u64::MAX);
static FRAMES: AtomicU64 = AtomicU64::new(0);
static ANNOUNCED: AtomicU32 = AtomicU32::new(0);
/// Last frame for which the live transport queued a checkpoint. `process_input`
/// can be entered more than once without advancing the simulation.
static LAST_SENT_FRAME: AtomicU32 = AtomicU32::new(u32::MAX);
/// Input is essential rather than diagnostic, so it has its own per-frame
/// sampling guard and is active in every networked match.
static LAST_INPUT_OBSERVED_FRAME: AtomicU32 = AtomicU32::new(u32::MAX);
static LAST_INPUT_SENT_FRAME: AtomicU32 = AtomicU32::new(u32::MAX);

/// WormForge's provisional private input-message kind. WA's stock compact
/// codec only knows kinds through 0x74; the two codec hooks below add this one
/// fixed-size kind. Project X also used 0x80 on WA 3.6.31, but that does not
/// establish either the number or its delivery semantics on WA 3.8.1.
const SYNC_MESSAGE: u32 = 0x80;
/// Early diagnostic builds used 0xfe. Decode it for compatibility, but never
/// emit it. Neither private kind is part of WA's stock protocol.
const LEGACY_SYNC_MESSAGE: u32 = 0xfe;
const SYNC_PAYLOAD_SIZE: u32 = 12;
/// Combined-RNG record emitted by early diagnostic builds.
const TAG_RNG: u32 = 0x5746_0101;
const TAG_WORMS: u32 = 0x5746_0102;
const TAG_ACTORS: u32 = 0x5746_0103;
const TAG_GAME_RNG: u32 = 0x5746_0104;
const TAG_FX_RNG: u32 = 0x5746_0105;
const TAG_NET_RNG: u32 = 0x5746_0106;
const TAG_INPUT: u32 = 0x5746_0107;
/// Modset announcement: `frame` = sender session tag, `hash` = byte length of
/// the fingerprint that follows in `TAG_MODSET_DATA` records.
const TAG_MODSET_BEGIN: u32 = 0x5746_0108;
/// `frame` = sender session tag, `hash` = next four UTF-8 bytes of the
/// fingerprint (zero padded). Records are queued in order and WA dispatches
/// them in order, so no sequence number is needed.
const TAG_MODSET_DATA: u32 = 0x5746_0109;
/// Records per frame while a modset is being announced. Small enough never to
/// crowd WA's own input, large enough that a typical modset lands in a second.
const MODSET_RECORDS_PER_FRAME: usize = 4;
const MODSET_MAX_BYTES: u32 = 4096;

#[derive(Clone, Copy)]
struct LiveInput {
    sample: InputSample,
    /// The team whose turn it was when this was accepted: its input holds
    /// for that turn only.
    team: u32,
}

static LIVE_INPUT: Mutex<Option<LiveInput>> = Mutex::new(None);

/// The enabled packs of this match as `modset_fingerprint`, plus how much of
/// it has been announced to peers so far.
struct ModsetTx {
    fingerprint: String,
    /// Records already queued: 0 = nothing, 1 = BEGIN sent, 1+n = n DATA sent.
    sent: usize,
}

static LOCAL_MODSET: Mutex<Option<ModsetTx>> = Mutex::new(None);

struct ModsetRx {
    total: usize,
    bytes: Vec<u8>,
    reported: bool,
}

/// Peer announcements keyed by their session tag.
static REMOTE_MODSETS: Mutex<Vec<(u32, ModsetRx)>> = Mutex::new(Vec::new());
static SESSION_TAG: AtomicU32 = AtomicU32::new(0);

/// Random-enough per-process tag so a peer can tell our announcement from its
/// own echo (WA dispatches the local queue to the local game too).
fn session_tag() -> u32 {
    let cur = SESSION_TAG.load(Ordering::Relaxed);
    if cur != 0 {
        return cur;
    }
    let mut h = Fnv::new();
    h.write_i32(std::process::id() as i32);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    h.write_i32(nanos as i32);
    h.write_i32(&SESSION_TAG as *const _ as usize as i32);
    let tag = h.finish().max(1);
    SESSION_TAG.store(tag, Ordering::Relaxed);
    tag
}

/// Called once per match from `apply::on_gfx_ready` with the packs that
/// actually loaded. Announced to peers from the first live frame.
pub(crate) fn set_local_modset(fingerprint: String) {
    if let Ok(mut g) = LOCAL_MODSET.lock() {
        *g = Some(ModsetTx {
            fingerprint,
            sent: 0,
        });
    }
    if let Ok(mut g) = REMOTE_MODSETS.lock() {
        g.clear();
    }
}

fn local_fingerprint() -> String {
    LOCAL_MODSET
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|m| m.fingerprint.clone()))
        .unwrap_or_default()
}

/// Queue the next slice of the local modset announcement. Returns false when
/// WA's input queue was full, in which case the same slice is retried next frame.
unsafe fn push_modset_slice(buffer: *mut BufferObject) -> bool {
    let Ok(mut g) = LOCAL_MODSET.lock() else {
        return true;
    };
    let Some(tx) = g.as_mut() else {
        return true;
    };
    let bytes = tx.fingerprint.as_bytes();
    let data_records = bytes.len().div_ceil(4);
    let tag = session_tag();
    for _ in 0..MODSET_RECORDS_PER_FRAME {
        if tx.sent > data_records {
            return true;
        }
        let ok = if tx.sent == 0 {
            push_checkpoint(buffer, TAG_MODSET_BEGIN, tag, bytes.len() as u32)
        } else {
            let off = (tx.sent - 1) * 4;
            let mut word = [0u8; 4];
            let end = (off + 4).min(bytes.len());
            word[..end - off].copy_from_slice(&bytes[off..end]);
            push_checkpoint(buffer, TAG_MODSET_DATA, tag, u32::from_le_bytes(word))
        };
        if !ok {
            return false;
        }
        tx.sent += 1;
        if tx.sent > data_records {
            log_line(&format!(
                "sync modset tx: announced {} ({} bytes)",
                if tx.fingerprint.is_empty() {
                    "<no packs>"
                } else {
                    tx.fingerprint.as_str()
                },
                bytes.len()
            ));
            return true;
        }
    }
    true
}

/// Accumulate a peer's announcement; compare once it is complete.
fn receive_modset(tag: u32, sender: u32, word: u32) {
    if sender == session_tag() {
        return;
    }
    let Ok(mut g) = REMOTE_MODSETS.lock() else {
        return;
    };
    if tag == TAG_MODSET_BEGIN {
        if word > MODSET_MAX_BYTES {
            log_error(&format!(
                "sync modset: peer {sender:08x} announced {word} bytes; ignored"
            ));
            return;
        }
        g.retain(|(s, _)| *s != sender);
        g.push((
            sender,
            ModsetRx {
                total: word as usize,
                bytes: Vec::with_capacity(word as usize),
                reported: false,
            },
        ));
    } else if let Some((_, rx)) = g.iter_mut().find(|(s, _)| *s == sender) {
        if rx.bytes.len() < rx.total {
            let take = (rx.total - rx.bytes.len()).min(4);
            rx.bytes.extend_from_slice(&word.to_le_bytes()[..take]);
        }
    } else {
        // Data before BEGIN: WA dispatches in order, so this means we joined
        // after the announcement started. Ignore; it repeats next match.
        return;
    }
    let complete: Vec<(u32, String)> = g
        .iter_mut()
        .filter(|(_, rx)| !rx.reported && rx.bytes.len() >= rx.total)
        .map(|(s, rx)| {
            rx.reported = true;
            (*s, String::from_utf8_lossy(&rx.bytes).into_owned())
        })
        .collect();
    drop(g);
    let local = local_fingerprint();
    for (sender, remote) in complete {
        match weapons::modset_diff(&local, &remote) {
            None => {
                log_line(&format!("sync modset rx {sender:08x}: matches ours"));
                crate::modloader::peer_report("Peer packs match yours.");
            }
            Some(diff) => {
                log_error(&format!(
                    "sync modset MISMATCH with peer {sender:08x}: {diff} (yours: {}; peer: {})",
                    if local.is_empty() {
                        "<no packs>"
                    } else {
                        &local
                    },
                    if remote.is_empty() {
                        "<no packs>"
                    } else {
                        &remote
                    }
                ));
                crate::modloader::peer_report(&format!("PEER MISMATCH - {diff}"));
            }
        }
    }
}

static ORIGINAL_PROCESS_INPUT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_DISPATCH_INPUT_MSG: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_ENCODE_INPUT_MSG: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_DECODE_INPUT_MSG: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_SERIALIZE_STATE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
struct StockState {
    frame: i32,
    bytes: Vec<u8>,
}
static STOCK_STATE: Mutex<Option<StockState>> = Mutex::new(None);
static STOCK_STATE_DUMPS: AtomicU32 = AtomicU32::new(0);
const MAX_STATE_DUMPS: u32 = 8;
const MAX_STATE_BYTES: usize = 128 * 1024;
/// Bounded stock-protocol trace for the live 3.8.1 sentry-placement failure.
static STOCK_TRACE_LINES: AtomicU32 = AtomicU32::new(0);
static STOCK_TRACE_UNTIL: AtomicU32 = AtomicU32::new(u32::MAX);

/// Log a line this often. 50 frames is one second of WA at 50 fps, which is
/// dense enough to bisect a divergence and still readable by eye.
const REPORT_EVERY: u64 = 50;

const fn diagnostics_enabled() -> bool {
    cfg!(feature = "sync-diagnostics")
}

#[repr(C)]
struct BufferMsgNode {
    size: u32,
    next: *mut BufferMsgNode,
    msg_type: u32,
}

/// The fields WA's `BufferObject__AllocSlot` uses. `GameRuntime+0x40` points
/// at this queue; messages put here are collected by `Game__process_input`.
#[repr(C)]
struct BufferObject {
    ring_start: *mut u8,
    capacity: u32,
    tail_offset: u32,
    read_offset: u32,
    tail_node: *mut BufferMsgNode,
    queue_head: *mut BufferMsgNode,
    queue_count: i32,
}

unsafe fn alloc_slot(buffer: *mut BufferObject, body_size: u32) -> *mut u32 {
    if buffer.is_null() || (*buffer).ring_start.is_null() {
        return std::ptr::null_mut();
    }
    let mut tail = (*buffer).tail_offset;
    let head = (*buffer).read_offset;
    let slot_bytes = body_size.wrapping_add(11) & !3;
    if tail < head {
        if head <= tail.wrapping_add(slot_bytes) {
            return std::ptr::null_mut();
        }
    } else if (*buffer).capacity <= tail.wrapping_add(slot_bytes) {
        if head <= slot_bytes {
            return std::ptr::null_mut();
        }
        tail = 0;
    }

    let node = (*buffer).ring_start.add(tail as usize) as *mut BufferMsgNode;
    (*node).size = body_size;
    (*node).next = std::ptr::null_mut();
    if !(*buffer).tail_node.is_null() {
        (*(*buffer).tail_node).next = node;
    }
    (*buffer).tail_node = node;
    if (*buffer).queue_head.is_null() {
        (*buffer).queue_head = node;
    }
    (*buffer).queue_count = (*buffer).queue_count.wrapping_add(1);
    (*buffer).tail_offset = tail.wrapping_add(slot_bytes);
    &raw mut (*node).msg_type
}

unsafe fn push_checkpoint(buffer: *mut BufferObject, tag: u32, frame: u32, hash: u32) -> bool {
    let slot = alloc_slot(buffer, 4 + SYNC_PAYLOAD_SIZE);
    if slot.is_null() {
        return false;
    }
    *slot = SYNC_MESSAGE;
    *slot.add(1) = tag;
    *slot.add(2) = frame;
    *slot.add(3) = hash;
    true
}

fn set_input(_frame: i32, sample: InputSample, team: u32) {
    let Ok(mut live) = LIVE_INPUT.lock() else {
        return;
    };
    *live = Some(LiveInput { sample, team });
}

/// The input of the turn in progress: none before its player's first record
/// (a new turn starts with no key held), or between turns.
fn current_input() -> Option<InputSample> {
    let (team, _) = turn_control()?;
    let live = LIVE_INPUT.lock().ok()?;
    let live = live.as_ref()?;
    (live.team == team).then_some(live.sample)
}

/// The team whose turn it is (the parent of the worm whose turn it is) and
/// the machine that controls it. Every peer reads the same, at the same
/// point of the lockstep.
fn turn_control() -> Option<(u32, i32)> {
    let worm = crate::actor::list_worms_pub(0)
        .into_iter()
        .find(|&w| w != 0 && unsafe { read_i32(w as *mut u8, game::offsets::WORM_TURN_ACTIVE) } != 0)?;
    let team = unsafe { read_i32(worm as *mut u8, game::offsets::ENT_PARENT) } as u32;
    if team == 0 {
        return None;
    }
    Some((team, unsafe { read_i32(team as *mut u8, game::offsets::TEAM_OWNER_MACHINE) }))
}

/// Whether a record counts: only one from the machine whose turn it is. One
/// that does not say who sent it (an older build) never does, nor does any
/// during a computer team's turn (a negative owner).
fn accepts(sender: Option<u8>, owner: i32) -> bool {
    sender.map(|s| s as i32) == Some(owner)
}

/// This machine's index among the players (0 the host).
fn local_machine(world: *mut u8) -> Option<u8> {
    let info = unsafe { read_i32(world, game::offsets::GAME_WORLD_GAME_INFO) } as *mut u8;
    if info.is_null() {
        return None;
    }
    Some(unsafe { *info.add(game::offsets::GAME_INFO_LOCAL_MACHINE) } as u8)
}

/// Latest state published by the player whose WA input queue is authoritative.
/// State records are change-driven, so an unchanged key remains held across
/// frames until the corresponding release record arrives.
pub(crate) fn input(_frame: i64) -> Option<(u32, i32, i32, bool)> {
    let sample = current_input()?;
    Some((sample.keys, sample.px, sample.py, sample.down))
}

unsafe fn push_input(buffer: *mut BufferObject, sample: InputSample, sender: u8) -> bool {
    let (controls, pointer) = sync_input::pack_from(sample, sender);
    push_checkpoint(buffer, TAG_INPUT, controls, pointer)
}

/// Runs immediately before WA collects authoritative local input. Controls are
/// published on change, pointer motion while it is in use, and otherwise with
/// a one-second heartbeat. The optional five-record checkpoint remains gated
/// by the `sync-diagnostics` build feature.
unsafe extern "C" fn queue_live_checkpoint(runtime: *mut u8) {
    let result = std::panic::catch_unwind(|| unsafe {
        if runtime.is_null() {
            return;
        }
        let world = *(runtime.add(GAME_RUNTIME_WORLD) as *const *mut u8);
        if world.is_null() || read_i32(world, GAME_WORLD_NET_SESSION) == 0 {
            return;
        }
        let frame = read_i32(world, GAME_WORLD_FRAME_COUNTER);
        if frame < 0 {
            return;
        }
        let frame_u32 = frame as u32;
        // Do not inject during frame-zero initialization. This is not an
        // authority test: both peers also enter during the ready period. Only
        // records dispatched by WA may update the simulation's input state.
        if frame_u32 == 0 {
            return;
        }
        let buffer = *(runtime.add(0x40) as *const *mut BufferObject);

        // Only the machine whose team is taking its turn publishes input: the
        // other player's keys are not this turn's. (Records from anyone else
        // are dropped on arrival, too.)
        if LAST_INPUT_OBSERVED_FRAME.swap(frame_u32, Ordering::Relaxed) != frame_u32 {
            let me = local_machine(world);
            let my_turn = match (me, turn_control()) {
                (Some(me), Some((_, owner))) => owner == me as i32,
                _ => false,
            };
            let local = crate::world::local_input_for_sync();
            // Quantize locally through the wire representation too, so the
            // sender consumes exactly the value received by its peers.
            let local = InputSample {
                keys: local.0,
                px: local.1,
                py: local.2,
                down: local.3,
            };
            let (controls, pointer) = sync_input::pack(local);
            let wire =
                sync_input::unpack(controls, pointer).expect("locally packed input is valid");
            let sender = me.unwrap_or(0);
            let pointer_requested = crate::world::take_pointer_sync_request();
            let last_sent = LAST_INPUT_SENT_FRAME.load(Ordering::Relaxed);
            let should_send = my_turn
                && sync_input::should_send(
                current_input(),
                wire,
                frame_u32,
                last_sent,
                pointer_requested,
            );
            if should_send {
                if push_input(buffer, wire, sender) {
                    LAST_INPUT_SENT_FRAME.store(frame_u32, Ordering::Relaxed);
                    log_line(&format!(
                        "sync input tx f={frame} keys={:#04x} pointer={},{} down={}",
                        wire.keys,
                        wire.px >> 16,
                        wire.py >> 16,
                        wire.down
                    ));
                } else {
                    log_error(&format!(
                        "sync input: WA input queue full at frame {frame}; input not sent"
                    ));
                    return;
                }
            }
            if !push_modset_slice(buffer) {
                log_line(&format!(
                    "sync modset: WA input queue full at frame {frame}; retrying next frame"
                ));
            }
        }

        if !diagnostics_enabled() || frame_u32 as u64 % REPORT_EVERY != 0 {
            return;
        }
        if LAST_SENT_FRAME.swap(frame_u32, Ordering::Relaxed) == frame_u32 {
            return;
        }
        let (game_rng, fx_rng, net_rng) = rng_words(world);
        let (_, worms, actors) = checkpoint(world);
        let ok = push_checkpoint(buffer, TAG_GAME_RNG, frame_u32, game_rng)
            && push_checkpoint(buffer, TAG_FX_RNG, frame_u32, fx_rng)
            && push_checkpoint(buffer, TAG_NET_RNG, frame_u32, net_rng)
            && push_checkpoint(buffer, TAG_WORMS, frame_u32, worms)
            && push_checkpoint(buffer, TAG_ACTORS, frame_u32, actors);
        if ok {
            log_line(&format!(
                "sync tx f={frame} game-rng={game_rng:08x} fx-rng={fx_rng:08x} \
                 net-rng={net_rng:08x} worms={worms:08x} actors={actors:08x}"
            ));
        } else {
            log_error(&format!(
                "sync transport: input queue full at frame {frame}; checkpoint not sent"
            ));
        }
    });
    if let Err(p) = result {
        log_error(&format!(
            "panic queuing sync checkpoint: {}",
            crate::log::panic_text(p)
        ));
    }
}

/// Consume one decoded WormForge carrier before WA's stock input dispatcher,
/// which intentionally has no case for either private kind.
unsafe extern "C" fn receive_live_checkpoint(
    payload: *const u8,
    runtime: *mut u8,
    msg_type: u32,
    size: u32,
) -> u32 {
    let result = std::panic::catch_unwind(|| unsafe {
        if msg_type != SYNC_MESSAGE && msg_type != LEGACY_SYNC_MESSAGE {
            return 0;
        }
        if payload.is_null() || size != SYNC_PAYLOAD_SIZE {
            log_error(&format!(
                "sync transport: malformed kind {msg_type:#04x} payload ({size} bytes)"
            ));
            return 1;
        }
        let tag = std::ptr::read_unaligned(payload as *const u32);
        let frame = std::ptr::read_unaligned(payload.add(4) as *const u32);
        let remote = std::ptr::read_unaligned(payload.add(8) as *const u32);
        if tag == TAG_INPUT {
            if runtime.is_null() {
                log_error("sync input: received input without a runtime");
                return 1;
            }
            let world = *(runtime.add(GAME_RUNTIME_WORLD) as *const *mut u8);
            if world.is_null() {
                log_error("sync input: received input without a world");
                return 1;
            }
            let current = read_i32(world, GAME_WORLD_FRAME_COUNTER);
            let Some((sample, sender)) = sync_input::unpack_from(frame, remote) else {
                log_error(&format!("sync input: malformed control bits {frame:#010x}"));
                return 1;
            };
            // Only the input of the machine whose turn it is counts. A record
            // still on the wire from the last turn's player, or sent by an
            // older build that did not say who it came from, is dropped.
            let Some((team, owner)) = turn_control() else {
                return 1;
            };
            if !accepts(sender, owner) {
                log_line(&format!(
                    "sync input rx f={current} keys={:#04x} from machine {sender:?} dropped: \
                     machine {owner}'s turn",
                    sample.keys
                ));
                return 1;
            }
            if diagnostics_enabled() && sample.keys & 0x80 != 0 && current >= 0 {
                STOCK_TRACE_LINES.store(0, Ordering::Relaxed);
                STOCK_TRACE_UNTIL.store((current as u32).saturating_add(25), Ordering::Relaxed);
                log_line(&format!(
                    "stock dispatch trace armed at frame {current} through {}",
                    (current as u32).saturating_add(25)
                ));
            }
            set_input(current, sample, team);
            log_line(&format!(
                "sync input rx f={current} keys={:#04x} pointer={},{} down={}",
                sample.keys,
                sample.px >> 16,
                sample.py >> 16,
                sample.down
            ));
            return 1;
        }
        if tag == TAG_MODSET_BEGIN || tag == TAG_MODSET_DATA {
            receive_modset(tag, frame, remote);
            return 1;
        }
        // Always consume known checkpoint tags for wire compatibility, but a
        // normal build does not spend time hashing or logging diagnostics.
        if !diagnostics_enabled() {
            return match tag {
                TAG_RNG | TAG_WORMS | TAG_ACTORS | TAG_GAME_RNG | TAG_FX_RNG | TAG_NET_RNG => 1,
                _ => {
                    log_error(&format!("sync transport: unknown payload tag {tag:#010x}"));
                    1
                }
            };
        }
        let component = match tag {
            TAG_RNG => "rng-legacy",
            TAG_WORMS => "worms",
            TAG_ACTORS => "actors",
            TAG_GAME_RNG => "game-rng",
            TAG_FX_RNG => "fx-rng",
            TAG_NET_RNG => "net-rng",
            _ => {
                log_error(&format!("sync transport: unknown payload tag {tag:#010x}"));
                return 1;
            }
        };
        if runtime.is_null() {
            log_error("sync transport: received checkpoint without a runtime");
            return 1;
        }
        let world = *(runtime.add(GAME_RUNTIME_WORLD) as *const *mut u8);
        if world.is_null() {
            log_error("sync transport: received checkpoint without a world");
            return 1;
        }
        let current = read_i32(world, GAME_WORLD_FRAME_COUNTER);
        let (rng, worms, actors) = checkpoint(world);
        let (game_rng, fx_rng, net_rng) = rng_words(world);
        let local = match tag {
            TAG_RNG => rng,
            TAG_WORMS => worms,
            TAG_ACTORS => actors,
            TAG_GAME_RNG => game_rng,
            TAG_FX_RNG => fx_rng,
            _ => net_rng,
        };
        if current < 0 || current as u32 != frame {
            log_error(&format!(
                "sync DESYNC f={frame} {component}: checkpoint arrived at local frame {current}"
            ));
        }
        if local == remote {
            log_line(&format!(
                "sync rx f={frame} {component}={remote:08x} local={local:08x} OK"
            ));
        } else if tag == TAG_FX_RNG {
            // `fxrandom` drives client presentation entities such as smoke,
            // score bubbles and sea bubbles. Peers may render a different
            // number of those, so retain the drift as renderer telemetry
            // without declaring the lockstep simulation desynchronised.
            log_line(&format!(
                "sync fx drift f={frame}: remote={remote:08x} local={local:08x}"
            ));
        } else {
            log_error(&format!(
                "sync DESYNC f={frame} {component}: remote={remote:08x} local={local:08x}"
            ));
        }
        1
    });
    match result {
        Ok(handled) => handled,
        Err(p) => {
            log_error(&format!(
                "panic receiving sync checkpoint: {}",
                crate::log::panic_text(p)
            ));
            1
        }
    }
}

/// Capture WA's own serialization, without calling it a second time or
/// changing its bytes. 0x532330 takes (runtime, buffer), RET 8; the main stream
/// is buffer[0] with byte length buffer+0x0c, as read by 0x546140. Kind 8 hashes
/// this simulation state and adds the world+0x20 object's virtual +0x48 result.
unsafe extern "system" fn serialize_state_hook(runtime: *mut u8, buffer: *mut u8) -> u32 {
    let original: unsafe extern "system" fn(*mut u8, *mut u8) -> u32 =
        std::mem::transmute(ORIGINAL_SERIALIZE_STATE.load(Ordering::Relaxed));
    let result = original(runtime, buffer);
    let captured = std::panic::catch_unwind(|| unsafe {
        if runtime.is_null()
            || buffer.is_null()
            || STOCK_STATE_DUMPS.load(Ordering::Relaxed) >= MAX_STATE_DUMPS
        {
            return;
        }
        let world = *(runtime.add(GAME_RUNTIME_WORLD) as *const *mut u8);
        if world.is_null() || read_i32(world, GAME_WORLD_NET_SESSION) == 0 {
            return;
        }
        let bytes = *(buffer as *const *const u8);
        let size = read_i32(buffer, 0x0c);
        if bytes.is_null() || size < 0 || size as usize > MAX_STATE_BYTES {
            if let Ok(mut state) = STOCK_STATE.lock() {
                *state = None;
            }
            log_error(&format!(
                "stock state: invalid or oversized snapshot ({size} bytes)"
            ));
            return;
        }
        if let Ok(mut state) = STOCK_STATE.lock() {
            *state = Some(StockState {
                frame: read_i32(world, GAME_WORLD_FRAME_COUNTER),
                bytes: std::slice::from_raw_parts(bytes, size as usize).to_vec(),
            });
        }
    });
    if let Err(p) = captured {
        log_error(&format!(
            "panic capturing stock state: {}",
            crate::log::panic_text(p)
        ));
    }
    result
}

/// Keep evidence in the ordinary log, so the next comparison needs no marker
/// or extra files. Limit volume to eight snapshots per match; hex goes only
/// to disk, not the console. Capture precedes checksum calculation and dispatch.
fn dump_stock_state(expected_frame: i32) {
    if !diagnostics_enabled() {
        return;
    }
    if STOCK_STATE_DUMPS.load(Ordering::Relaxed) >= MAX_STATE_DUMPS {
        return;
    }
    let Ok(mut state) = STOCK_STATE.lock() else {
        return;
    };
    let Some(snapshot) = state.as_ref() else {
        return;
    };
    if snapshot.frame != expected_frame {
        log_line(&format!(
            "stock state missing f={expected_frame} captured-f={}",
            snapshot.frame
        ));
        return;
    }
    let snapshot = state.take().unwrap();
    drop(state);
    STOCK_STATE_DUMPS.fetch_add(1, Ordering::Relaxed);
    // Same rotate/add operation as WA 3.8.1's 0x546140, excluding the separate
    // world-object contribution and the alternate 0x546170 checksum variant.
    let hash = snapshot.bytes.iter().fold(0u32, |sum, &byte| {
        sum.rotate_left(3).wrapping_add(byte as u32)
    });
    log_line(&format!(
        "stock state f={expected_frame} bytes={} stream-hash={hash:08x}",
        snapshot.bytes.len()
    ));
    let mut text = String::new();
    for (i, chunk) in snapshot.bytes.chunks(128).enumerate() {
        let _ = write!(
            text,
            "stock state data f={expected_frame} off={:08x} hex=",
            i * 128
        );
        for byte in chunk {
            let _ = write!(text, "{byte:02x}");
        }
        text.push_str("\r\n");
    }
    let _ = write!(text, "stock state end f={expected_frame}\r\n");
    crate::log::diagnostic_file(&text);
}

/// Record stock messages surrounding custom-weapon fire and the incoming and
/// locally expected kind-8 state checksums. A checksum-state of -1 means WA is
/// waiting for its peer's checksum, not that it has already detected an error.
unsafe extern "C" fn trace_stock_dispatch(
    payload: *const u8,
    runtime: *mut u8,
    msg_type: u32,
    size: u32,
) {
    if !diagnostics_enabled() {
        return;
    }
    let result = std::panic::catch_unwind(|| unsafe {
        if runtime.is_null() || msg_type == SYNC_MESSAGE || msg_type == LEGACY_SYNC_MESSAGE {
            return;
        }
        if msg_type == 8 {
            dump_stock_state(read_i32(runtime, 0x264));
        }
        let world = *(runtime.add(GAME_RUNTIME_WORLD) as *const *mut u8);
        let frame = if world.is_null() {
            -1
        } else {
            read_i32(world, GAME_WORLD_FRAME_COUNTER)
        };
        let until = STOCK_TRACE_UNTIL.load(Ordering::Relaxed);
        let trace_armed = until != u32::MAX && frame >= 0 && frame as u32 <= until;
        if msg_type != 8 && msg_type != 9 && !trace_armed {
            return;
        }
        if STOCK_TRACE_LINES.fetch_add(1, Ordering::Relaxed) >= 256 {
            return;
        }
        let word = |offset: usize| {
            if !payload.is_null() && size as usize >= offset + 4 {
                std::ptr::read_unaligned(payload.add(offset) as *const u32)
            } else {
                0
            }
        };
        let checksum = if msg_type == 8 || msg_type == 9 {
            format!(
                " expected-frame={} expected-checksum={:08x} checksum-state={} local-input={} remote-input={}",
                read_i32(runtime, 0x264),
                read_i32(runtime, 0x268) as u32,
                read_i32(runtime, 0x270),
                *runtime.add(0x490),
                *runtime.add(0x491)
            )
        } else {
            String::new()
        };
        log_line(&format!(
            "stock dispatch f={frame} kind={msg_type:#04x} size={size} \
             data={:08x},{:08x},{:08x}{checksum}",
            word(0),
            word(4),
            word(8)
        ));
    });
    if let Err(p) = result {
        log_error(&format!(
            "panic tracing stock input: {}",
            crate::log::panic_text(p)
        ));
    }
}

/// EAX=GameRuntime, no stack arguments.
#[unsafe(naked)]
unsafe extern "C" fn process_input_hook() {
    core::arch::naked_asm!(
        "pushfd",
        "pushad",
        "push eax",
        "call {queue}",
        "add esp, 4",
        "popad",
        "popfd",
        "jmp dword ptr [{original}]",
        queue = sym queue_live_checkpoint,
        original = sym ORIGINAL_PROCESS_INPUT,
    );
}

/// EAX=payload; stack=(runtime, kind, payload_size), callee pops 12.
#[unsafe(naked)]
unsafe extern "C" fn dispatch_input_hook() {
    core::arch::naked_asm!(
        "push ebp",
        "mov ebp, esp",
        "pushfd",
        "pushad",
        "push dword ptr [ebp+16]",
        "push dword ptr [ebp+12]",
        "push dword ptr [ebp+8]",
        "push eax",
        "call {trace}",
        "add esp, 16",
        "popad",
        "popfd",
        "pop ebp",
        "cmp dword ptr [esp+8], {kind}",
        "je 1f",
        "cmp dword ptr [esp+8], {legacy_kind}",
        "jne 3f",
        "1:",
        "push ebp",
        "mov ebp, esp",
        "pushfd",
        "pushad",
        "push dword ptr [ebp+16]",
        "push dword ptr [ebp+12]",
        "push dword ptr [ebp+8]",
        "push eax",
        "call {receive}",
        "add esp, 16",
        "test eax, eax",
        "jnz 2f",
        "popad",
        "popfd",
        "pop ebp",
        "jmp dword ptr [{original}]",
        "2:",
        "popad",
        "popfd",
        "pop ebp",
        // Dispatch's caller at 0x531f0d branches on EAX: positive means
        // handled, zero forwards to the game task, negative stops dispatch.
        // Returning the restored payload pointer was an undefined status.
        "mov eax, 1",
        "ret 12",
        "3:",
        "jmp dword ptr [{original}]",
        kind = const SYNC_MESSAGE,
        legacy_kind = const LEGACY_SYNC_MESSAGE,
        trace = sym trace_stock_dispatch,
        receive = sym receive_live_checkpoint,
        original = sym ORIGINAL_DISPATCH_INPUT_MSG,
    );
}

/// 3.8.1 compact encoder ABI: ECX=wire dst, EDX=payload, stack=
/// (wire capacity, kind, protocol fields), callee pops 16. Payload size is
/// implicit in the kind, which is why this private kind must always be 12 bytes.
#[unsafe(naked)]
unsafe extern "C" fn encode_input_hook() {
    core::arch::naked_asm!(
        "cmp dword ptr [esp+8], {kind}",
        "jne 2f",
        "cmp dword ptr [esp+4], 13",
        "jb 2f",
        "mov byte ptr [ecx], {kind}",
        "mov eax, dword ptr [edx]",
        "mov dword ptr [ecx+1], eax",
        "mov eax, dword ptr [edx+4]",
        "mov dword ptr [ecx+5], eax",
        "mov eax, dword ptr [edx+8]",
        "mov dword ptr [ecx+9], eax",
        "mov eax, 13",
        "ret 16",
        "2:",
        "jmp dword ptr [{original}]",
        kind = const SYNC_MESSAGE,
        original = sym ORIGINAL_ENCODE_INPUT_MSG,
    );
}

/// Matching decoder ABI: EAX=kind out, ECX=wire src, ESI=payload out,
/// stack=(wire bytes, payload-size out, three protocol fields), pops 20.
/// Decode only. Network packets may arrive many simulation frames ahead;
/// applying input here bypasses WA's queue timing and duplicates dispatch.
#[unsafe(naked)]
unsafe extern "C" fn decode_input_hook() {
    core::arch::naked_asm!(
        "cmp dword ptr [esp+4], 13",
        "jb 2f",
        "cmp byte ptr [ecx], {kind}",
        "je 1f",
        "cmp byte ptr [ecx], {legacy_kind}",
        "jne 2f",
        "1:",
        "movzx edx, byte ptr [ecx]",
        "mov dword ptr [eax], edx",
        "mov edx, dword ptr [ecx+1]",
        "mov dword ptr [esi], edx",
        "mov edx, dword ptr [ecx+5]",
        "mov dword ptr [esi+4], edx",
        "mov edx, dword ptr [ecx+9]",
        "mov dword ptr [esi+8], edx",
        "mov edx, dword ptr [esp+8]",
        "mov dword ptr [edx], {size}",
        "mov eax, 13",
        "ret 20",
        "2:",
        "jmp dword ptr [{original}]",
        kind = const SYNC_MESSAGE,
        legacy_kind = const LEGACY_SYNC_MESSAGE,
        size = const SYNC_PAYLOAD_SIZE,
        original = sym ORIGINAL_DECODE_INPUT_MSG,
    );
}

unsafe fn create_hook(
    symbol: &str,
    detour: *mut c_void,
    original: &AtomicPtr<c_void>,
) -> Result<(), String> {
    let target = crate::wa::address(symbol) as *mut c_void;
    let trampoline = crate::wa::create_hook(target, detour)
        .map_err(|e| format!("sync transport {symbol}: {e:?}"))?;
    original.store(trampoline, Ordering::SeqCst);
    Ok(())
}

/// Install the carrier codec and input injector. Synchronized input is always
/// active in networked matches; the build feature controls only diagnostics.
/// A live sender's network peers still need WormForge because stock WA does
/// not know message kind 0x80.
pub(crate) fn install() -> Result<(), String> {
    unsafe {
        create_hook(
            "PROCESS_INPUT",
            process_input_hook as *mut c_void,
            &ORIGINAL_PROCESS_INPUT,
        )?;
        create_hook(
            "DISPATCH_INPUT_MSG",
            dispatch_input_hook as *mut c_void,
            &ORIGINAL_DISPATCH_INPUT_MSG,
        )?;
        create_hook(
            "ENCODE_INPUT_MSG",
            encode_input_hook as *mut c_void,
            &ORIGINAL_ENCODE_INPUT_MSG,
        )?;
        create_hook(
            "DECODE_INPUT_MSG",
            decode_input_hook as *mut c_void,
            &ORIGINAL_DECODE_INPUT_MSG,
        )?;
        if diagnostics_enabled() && crate::wa::try_address("SERIALIZE_STATE").is_some() {
            create_hook(
                "SERIALIZE_STATE",
                serialize_state_hook as *mut c_void,
                &ORIGINAL_SERIALIZE_STATE,
            )?;
        }
    }
    log_line(
        "synchronized input enabled (provisional kind 0x80, change-driven 12-byte records); network peers require WormForge",
    );
    if diagnostics_enabled() && crate::wa::try_address("SERIALIZE_STATE").is_some() {
        log_line(
            "sync diagnostics build enabled; hashing simulation state and sending checkpoints",
        );
    }
    Ok(())
}

pub(crate) unsafe fn rng_words(world: *mut u8) -> (u32, u32, u32) {
    (
        read_i32(world, GAME_WORLD_GAME_RNG) as u32,
        read_i32(world, GAME_WORLD_RNG_STATE_1) as u32,
        read_i32(world, GAME_WORLD_RNG_STATE_2) as u32,
    )
}

/// Hash all three RNG words for local diagnostics. The live peer carrier
/// sends the words separately because `fxrandom` is deliberately
/// client-local while gameplay RNG and `netrandom` must agree across peers.
unsafe fn hash_rng(world: *mut u8) -> u32 {
    let mut h = Fnv::new();
    let (game_rng, fx_rng, net_rng) = rng_words(world);
    h.write_i32(game_rng as i32);
    h.write_i32(fx_rng as i32);
    h.write_i32(net_rng as i32);
    h.finish()
}

/// Every live worm, in team/index order rather than address order — the walk
/// in `list_worms` is keyed by (team, index), so it is already reproducible.
///
/// As with actors, the handle is not hashed: it is an entity pointer, so it
/// differs between runs even when the worm is in exactly the same state. The
/// (team, index) walk order is the identity that matters.
unsafe fn hash_worms() -> u32 {
    let mut h = Fnv::new();
    for handle in crate::actor::live_worms() {
        let worm = handle as *mut u8;
        h.write_i32(read_i32(worm, ENT_POS_X));
        h.write_i32(read_i32(worm, ENT_POS_Y));
        h.write_i32(read_i32(worm, ENT_SPEED_X));
        h.write_i32(read_i32(worm, ENT_SPEED_Y));
        h.write_i32(crate::world::worm_hp(handle));
    }
    h.finish()
}

/// Every live worm's WA state (+0x44), in the same (team, index) order as
/// [`hash_worms`]. Kept apart from it so older replays, which have no state
/// hash, still compare positions: a worm frozen in the wrong state shows here
/// before it has moved anywhere.
pub(crate) unsafe fn hash_worm_states() -> u32 {
    let mut h = Fnv::new();
    for handle in crate::actor::live_worms() {
        h.write_i32(read_i32(handle as *mut u8, game::offsets::WORM_STATE));
    }
    h.finish()
}

/// Our own actors. `LIVE` is a `Vec` and `retain` preserves order, so the walk
/// is insertion-ordered and reproducible.
///
/// The handle itself is deliberately NOT hashed: it is a heap address, so it
/// differs between runs even when the simulation agrees. Hashing it would make
/// every run look divergent. That the *count* and the *order* still have to
/// match is what this actually tests.
unsafe fn hash_actors() -> u32 {
    let mut h = Fnv::new();
    for handle in crate::actor::live_actors() {
        let a = handle as *mut u8;
        h.write_i32(read_i32(a, ENT_POS_X));
        h.write_i32(read_i32(a, ENT_POS_Y));
        h.write_i32(read_i32(a, ENT_SPEED_X));
        h.write_i32(read_i32(a, ENT_SPEED_Y));
        h.write_i32(read_i32(a, ACTOR_SPRITE));
        h.write_i32(read_i32(a, ACTOR_PHASE));
    }
    h.finish()
}

/// The recorder's checkpoint payload. Kept here so the diagnostic probe and
/// replay files hash exactly the same fields in exactly the same order.
pub(crate) unsafe fn checkpoint(world: *mut u8) -> (u32, u32, u32) {
    (hash_rng(world), hash_worms(), hash_actors())
}

/// Called at the end of every world-root `FRAME_FINISH`, after WA's own
/// handler has run — so this observes end-of-frame state, which is the point
/// at which WA increments its frame counter.
pub unsafe fn observe(world: *mut u8) {
    if !diagnostics_enabled() || world.is_null() {
        return;
    }
    let frame = read_i32(world, GAME_WORLD_FRAME_COUNTER);
    if frame < 0 {
        return;
    }
    let frame = frame as u64;

    // WA's counter, not one of ours. A repeat means this hook ran twice for
    // one sim frame, which would silently double-count and is worth knowing.
    let previous = LAST_FRAME.swap(frame, Ordering::Relaxed);
    if previous == frame {
        return;
    }

    if ANNOUNCED.swap(1, Ordering::Relaxed) == 0 {
        let net = read_i32(world, game::offsets::GAME_WORLD_NET_SESSION);
        log_line(&format!(
            "sync probe: world {world:?} first frame {frame}, net_session {net:#x} ({})",
            if net != 0 { "NETWORKED" } else { "local" }
        ));
    }

    let (rng, worms, actors) = checkpoint(world);

    let n = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    if n % REPORT_EVERY == 0 {
        // One line per report, fixed field order, so two runs diff cleanly
        // with `diff <(grep "^sync f=" a.log) <(grep "^sync f=" b.log)`.
        log_line(&format!(
            "sync f={frame} rng={rng:08x} worms={worms:08x} actors={actors:08x}"
        ));
    }
}

/// Flush a final line at match end, so a short session still yields a tail to
/// compare even if it did not reach a report boundary.
pub fn finish() {
    if diagnostics_enabled() {
        let frames = FRAMES.load(Ordering::Relaxed);
        if frames > 0 {
            log_line(&format!("sync probe: {frames} frames hashed this match"));
        }
    }
    FRAMES.store(0, Ordering::Relaxed);
    LAST_FRAME.store(u64::MAX, Ordering::Relaxed);
    ANNOUNCED.store(0, Ordering::Relaxed);
    LAST_SENT_FRAME.store(u32::MAX, Ordering::Relaxed);
    LAST_INPUT_OBSERVED_FRAME.store(u32::MAX, Ordering::Relaxed);
    LAST_INPUT_SENT_FRAME.store(u32::MAX, Ordering::Relaxed);
    STOCK_TRACE_LINES.store(0, Ordering::Relaxed);
    STOCK_TRACE_UNTIL.store(u32::MAX, Ordering::Relaxed);
    STOCK_STATE_DUMPS.store(0, Ordering::Relaxed);
    if let Ok(mut state) = STOCK_STATE.lock() {
        *state = None;
    }
    if let Ok(mut live) = LIVE_INPUT.lock() {
        *live = None;
    }
    if let Ok(mut g) = LOCAL_MODSET.lock() {
        *g = None;
    }
    if let Ok(mut g) = REMOTE_MODSETS.lock() {
        g.clear();
    }
    crate::world::clear_input_latch();
}

#[cfg(test)]
mod input_owner_tests {
    use super::accepts;

    #[test]
    fn only_the_turns_own_machine_steers_it() {
        assert!(accepts(Some(0), 0), "the host on the host's turn");
        assert!(accepts(Some(1), 1), "a client on its own turn");
        assert!(!accepts(Some(1), 0), "the client's keys on the host's turn");
        assert!(!accepts(Some(0), 1));
        assert!(!accepts(None, 0), "an unstamped record");
        assert!(!accepts(Some(0), -20), "a computer team's turn");
    }
}
