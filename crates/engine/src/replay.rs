//! Recorder and player for WormForge replays: WA's own `.WAgame` with a
//! trailer appended (`crates/replay`, `docs/replay-wagame-plan.md`).
//!
//! WA keeps writing its ordinary `.WAgame`. While the match runs, this module
//! records the WormForge input and checkpoint streams into a pending
//! directory under `wormforge_dir()`, next to the pack table and a note naming
//! the `.WAgame`. At the next match boundary, after WA has closed its file,
//! the trailer is appended and the `.WAgame` atomically replaced -- so a file
//! with a trailer is always a complete recording.
//!
//! A crash before that leaves the `.WAgame` vanilla-looking and the pending
//! directory behind. `recover_pending` finalises such leftovers at DLL start
//! and at match boundaries; one that cannot be finalised is set aside and
//! logged, and its game stays a vanilla replay.
//!
//! Pack files whose bytes are in the `.WFAssets` collection are recorded as
//! references. On playback the `ReplayLoader` hook finds the trailer,
//! resolves references from the collection (refusing, with every missing
//! hash named, if it cannot), materialises the packs in a private directory,
//! and lets WA read the file itself: WA stops at its own end-of-stream and
//! never sees the trailer.

use crate::log::{wa_directory, wormforge_dir, BUILD_ID};
use crate::{log_error, log_line};
use game::entity::read_i32;
use game::offsets::{
    GAME_WORLD_FRAME_COUNTER, GAME_WORLD_RECORDED_FRAME,  REPLAY_ACTIVE,
    REPLAY_INPUT_PATH, REPLAY_OUTPUT_PATH, REPLAY_PATH_CAPACITY,
};
use assets::Collection;
use replay::{Checkpoint, Content, Header, InputSample, Pack, PackFile, Trailer, MAX_WF_BYTES};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use weapons::components::PackFolder;
use weapons::{modset_fingerprint, LoadedMod};

const CHECKPOINT_EVERY: i64 = 50;

enum State {
    Idle,
    Pending {
        mods: Vec<LoadedMod>,
        /// Component bundles and the frozen selection, archived with the packs.
        extras: Vec<PackFolder>,
        waiting_logged: bool,
    },
    Recording(Recorder),
    Playback(Player),
    Failed,
}

static STATE: Mutex<State> = Mutex::new(State::Idle);
static PLAYBACK_ACTIVE: AtomicBool = AtomicBool::new(false);

/// A recording in progress. Its streams live in `dir` until `complete`
/// appends them to the `.WAgame`.
struct Recorder {
    wa_path: PathBuf,
    dir: PathBuf,
    wf: BufWriter<File>,
    chk: BufWriter<File>,
    wf_bytes: u64,
    chk_bytes: u64,
    last_frame: Option<i64>,
}

struct Player {
    source: PathBuf,
    temp_root: PathBuf,
    mods_root: PathBuf,
    header: Header,
    inputs: Vec<InputSample>,
    checkpoints: Vec<Checkpoint>,
    next_input: usize,
    next_checkpoint: usize,
    rng_drift_logged: bool,
    fx_drift_logged: bool,
    net_drift_logged: bool,
    failure: Option<String>,
}

/// Arm recording for the newly loaded pack set. The replay name is allowed to
/// arrive later; WA does not always fill it before LoadSprites returns.
pub fn prepare(mods: &[LoadedMod], extras: &[PackFolder]) -> bool {
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    if let State::Playback(player) = &mut *state {
        let found = modset_fingerprint(mods);
        if found != player.header.modset {
            fail_player(
                player,
                format!(
                    "archived modset is {found:?}, header requires {:?}",
                    player.header.modset
                ),
            );
            return false;
        } else {
            log_line(&format!("replay player: loaded archived modset {found}"));
        }
        return true;
    }
    finish_state(&mut state);
    *state = State::Pending {
        mods: mods.to_vec(),
        extras: extras.to_vec(),
        waiting_logged: false,
    };
    true
}

/// Select archived packs while a WormForge replay is being constructed.
/// Installed `Mods/` are intentionally ignored: replay files carry the exact
/// scripts and assets that originally ran.
pub fn playback_mods_root() -> Option<PathBuf> {
    let state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    match &*state {
        State::Playback(player) => Some(player.mods_root.clone()),
        _ => None,
    }
}

/// Some replay launch paths bypass the hooked ReplayLoader. Recover from the
/// frontend replay path before InitGraphic chooses which packs to load.
pub fn recover_playback_at_mod_load() -> Result<(), String> {
    if PLAYBACK_ACTIVE.load(Ordering::Acquire) {
        return Ok(());
    }
    let prefix = unsafe { replay_prefix() } as *mut u8;
    if prefix.is_null() || unsafe { prefix.add(REPLAY_ACTIVE).read() } == 0 {
        return Ok(());
    }
    log_line("replay player: ReplayLoader hook was bypassed; checking replay at InitGraphic");
    unsafe { redirect_playback(prefix) }?;
    Ok(())
}

/// Called before WA's mode-1 `ReplayLoader`. Returns false for an ordinary
/// `.WAgame`, leaving the original loader completely untouched.
pub unsafe fn redirect_playback(prefix: *mut u8) -> Result<bool, String> {
    if prefix.is_null() {
        return Ok(false);
    }
    let Some(raw_path) = (unsafe { prefix_path(prefix, REPLAY_INPUT_PATH) }) else {
        log_line("replay player: mode-1 loader supplied no input path");
        return Ok(false);
    };
    let selected = absolute_wa_path(raw_path);
    log_line(&format!(
        "replay player: mode-1 loader selected {}",
        selected.display()
    ));
    // Under the macOS Wine bundle, WA can hand the loader a host path with a
    // drive letter glued on ("C:/Users/<you>/Applications/...") that does not
    // exist inside Wine. WA copes by looking in User/Games; so do we.
    let source = if selected.is_file() {
        selected
    } else {
        let games = selected
            .file_name()
            .map(|name| wa_directory().join("User").join("Games").join(name));
        match games {
            Some(games) if games.is_file() => {
                log_line(&format!(
                    "replay player: loader path does not exist; using {}",
                    games.display()
                ));
                games
            }
            _ => {
                log_line("replay player: loader path does not exist; leaving it to WA");
                return Ok(false);
            }
        }
    };
    let bytes = read_all(&source)?;
    let trailer = match Trailer::find(&bytes).map_err(|e| e.to_string())? {
        Some(trailer) => trailer,
        None => {
            log_line(&format!(
                "replay player: {} has no WormForge trailer; vanilla playback",
                source.display()
            ));
            return Ok(false);
        }
    };
    trailer
        .require_wa_version("3.8.1")
        .map_err(|e| e.to_string())?;
    let header = trailer.header().clone();
    let packs = replay::decode_pack(trailer.pack()).map_err(|e| e.to_string())?;
    let references = replay::asset_refs(&packs).len();
    let packs = resolve_assets(packs, header.collection_version)?;
    let inputs = replay::decode_inputs(trailer.wf()).map_err(|e| e.to_string())?;
    let checkpoints = replay::decode_checkpoints(trailer.chk(), trailer.format_version())
        .map_err(|e| e.to_string())?;

    {
        let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
        finish_state(&mut state);
    }
    let temp_root = wormforge_dir().join(format!("replay-playback-{}", std::process::id()));
    if temp_root.exists() {
        std::fs::remove_dir_all(&temp_root)
            .map_err(|e| format!("cannot clear {}: {e}", temp_root.display()))?;
    }
    let mods_root = temp_root.join("Mods");
    materialize_packs(&mods_root, &packs)?;
    // WA reads the file itself and stops at its own end-of-stream; the input
    // path stays exactly as WA chose it.
    unsafe { prefix.add(REPLAY_ACTIVE).write(1) };

    let player = Player {
        source: source.clone(),
        temp_root,
        mods_root,
        header,
        inputs,
        checkpoints,
        next_input: 0,
        next_checkpoint: 0,
        rng_drift_logged: false,
        fx_drift_logged: false,
        net_drift_logged: false,
        failure: None,
    };
    log_line(&format!(
        "replay player: accepted {} ({} frames, {} checkpoints, {} archived packs, \
         {references} assets from the collection)",
        source.display(),
        player.inputs.len(),
        player.checkpoints.len(),
        packs.len()
    ));
    if player.header.build_hash != BUILD_ID {
        log_line(&format!(
            "replay player: recorded by build {}, running compatible format on {BUILD_ID}",
            player.header.build_hash
        ));
    }
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    *state = State::Playback(player);
    PLAYBACK_ACTIVE.store(true, Ordering::Release);
    Ok(true)
}

pub fn playback_loader_failed(code: u32) {
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    if let State::Playback(player) = &mut *state {
        fail_player(player, format!("WA ReplayLoader returned {code:#010x}"));
    }
}

pub fn playback_failed(detail: impl Into<String>) {
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    if let State::Playback(player) = &mut *state {
        fail_player(player, detail.into());
    }
}

/// Append one complete simulation frame. The mutex deliberately blocks: the
/// log writer's `try_lock` and drop policy is unacceptable for replay data.
pub unsafe fn observe(world: *mut u8) {
    if world.is_null() {
        return;
    }
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    if let State::Playback(player) = &mut *state {
        unsafe { verify_playback_frame(player, world) };
        return;
    }
    if matches!(*state, State::Pending { .. }) && read_i32(world, GAME_WORLD_RECORDED_FRAME) < 0 {
        log_line("replay recorder: playback is active; not recording");
        *state = State::Idle;
        return;
    }
    try_start(&mut state);
    let result = match &mut *state {
        State::Recording(recorder) => record_frame(recorder, world),
        _ => return,
    };
    if let Err(e) = result {
        fail(&mut state, e);
    }
}

/// Input replacement used by both frame latches. `Some` means a WormForge
/// replay is active, including the neutral value returned after a missing
/// frame has been reported. `None` means ordinary live input should be polled.
pub fn playback_input(frame: i64) -> Option<(u32, i32, i32, bool)> {
    if !PLAYBACK_ACTIVE.load(Ordering::Acquire) {
        return None;
    }
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    let State::Playback(player) = &mut *state else {
        return None;
    };
    if player.failure.is_some() {
        // Stop applying custom input after the first diagnosed divergence.
        // WA may continue rendering its vanilla stream, but WormForge no
        // longer pretends the custom simulation is valid.
        return Some((0, 0, 0, false));
    }
    match player
        .inputs
        .binary_search_by_key(&frame, |sample| sample.frame)
    {
        Ok(index) => {
            let sample = player.inputs[index];
            Some((sample.keys, sample.px, sample.py, sample.down))
        }
        Err(_) => {
            fail_player(
                player,
                format!("input stream has no sample for frame {frame}"),
            );
            Some((0, 0, 0, false))
        }
    }
}

unsafe fn verify_playback_frame(player: &mut Player, world: *mut u8) {
    if player.failure.is_some() {
        return;
    }
    let frame = unsafe { read_i32(world, GAME_WORLD_FRAME_COUNTER) as i64 };
    let Some(sample) = player.inputs.get(player.next_input).copied() else {
        fail_player(
            player,
            format!("simulation reached frame {frame} after the input stream ended"),
        );
        return;
    };
    if sample.frame != frame {
        fail_player(
            player,
            format!(
                "simulation reached frame {frame}, input stream expected frame {}",
                sample.frame
            ),
        );
        return;
    }
    player.next_input += 1;

    let Some(expected) = player.checkpoints.get(player.next_checkpoint).copied() else {
        return;
    };
    if expected.frame < frame {
        fail_player(
            player,
            format!(
                "simulation passed checkpoint frame {} at frame {frame}",
                expected.frame
            ),
        );
        return;
    }
    if expected.frame != frame {
        return;
    }
    let (rng, worms, actors) = crate::sync::checkpoint(world);
    let mut different = Vec::new();
    if let Some([game, fx, net]) = expected.rng_words {
        let (local_game, local_fx, local_net) = unsafe { crate::sync::rng_words(world) };
        if local_game != game {
            different.push(format!("game RNG {local_game:08x}!={game:08x}"));
        }
        if local_fx != fx && !player.fx_drift_logged {
            log_line(&format!("replay player: visual RNG drift at frame {frame}: {local_fx:08x}!={fx:08x}"));
            player.fx_drift_logged = true;
        }
        if local_net != net && !player.net_drift_logged {
            log_line(&format!("replay player: network RNG drift at frame {frame}: {local_net:08x}!={net:08x}"));
            player.net_drift_logged = true;
        }
    } else if rng != expected.rng && !player.rng_drift_logged {
        log_line(&format!(
            "replay player: version 2 combined RNG drift at frame {frame}: {rng:08x}!={:08x}; gameplay RNG cannot be isolated",
            expected.rng
        ));
        player.rng_drift_logged = true;
    }
    if worms != expected.worms {
        different.push(format!("worms {:08x}!={:08x}", worms, expected.worms));
    }
    if actors != expected.actors {
        different.push(format!("actors {:08x}!={:08x}", actors, expected.actors));
    }
    if let Some(states) = expected.worm_states {
        let local = unsafe { crate::sync::hash_worm_states() };
        if local != states {
            different.push(format!("worm states {local:08x}!={states:08x}"));
        }
    }
    if !different.is_empty() {
        fail_player(
            player,
            format!(
                "checkpoint mismatch at frame {frame}: {}",
                different.join(", ")
            ),
        );
        return;
    }
    player.next_checkpoint += 1;
}

unsafe fn record_frame(recorder: &mut Recorder, world: *mut u8) -> Result<(), String> {
    let frame = read_i32(world, GAME_WORLD_FRAME_COUNTER) as i64;
    if frame < 0 || recorder.last_frame.is_some_and(|last| frame <= last) {
        return Err(format!(
            "frame {frame} did not strictly follow {:?}",
            recorder.last_frame
        ));
    }
    let (keys, px, py, down) = crate::world::replay_input();
    let sample = InputSample {
        frame,
        keys,
        px,
        py,
        down,
    };
    let wf = replay::encode_inputs(&[sample]).map_err(|e| e.to_string())?;
    append_capped(&mut recorder.wf, &mut recorder.wf_bytes, &wf)
        .map_err(|e| format!("input stream: {e}"))?;

    if frame % CHECKPOINT_EVERY == 0 {
        let (rng, worms, actors) = crate::sync::checkpoint(world);
        let (game_rng, fx_rng, net_rng) = unsafe { crate::sync::rng_words(world) };
        let checkpoint = Checkpoint {
            frame,
            rng,
            worms,
            actors,
            rng_words: Some([game_rng, fx_rng, net_rng]),
            worm_states: Some(unsafe { crate::sync::hash_worm_states() }),
        };
        let bytes = replay::encode_checkpoints(&[checkpoint]).map_err(|e| e.to_string())?;
        append_capped(&mut recorder.chk, &mut recorder.chk_bytes, &bytes)
            .map_err(|e| format!("checkpoint stream: {e}"))?;
        // Hand both streams to the OS once a second. A crash then loses at
        // most a second of input rather than everything still buffered, so
        // `recover_pending` has a stream that reaches almost to the end.
        recorder.wf.flush().map_err(|e| format!("input stream: {e}"))?;
        recorder.chk.flush().map_err(|e| format!("checkpoint stream: {e}"))?;
    }
    recorder.last_frame = Some(frame);
    Ok(())
}

/// Complete the previous recording at the next match boundary. Leftovers
/// from earlier boundaries are retried first, so a completion that fails now
/// (WA's file still unreadable) gets another chance at the next one rather
/// than being set aside at once. A failed completion keeps its pending
/// directory; the `.WAgame` stays vanilla until one succeeds.
pub fn finish() {
    recover_pending();
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    finish_state(&mut state);
}

/// LoadSprites is also a fallback boundary for recordings whose shutdown hook
/// did not run. A playback prepared by ReplayLoader belongs to the match now
/// being constructed and must survive this boundary.
pub fn finish_recording() {
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    if matches!(*state, State::Playback(_)) {
        return;
    }
    finish_state(&mut state);
}

fn try_start(state: &mut State) {
    let State::Pending {
        mods,
        extras,
        waiting_logged,
    } = state
    else {
        return;
    };
    let Some(wa_path) = (unsafe { output_path() }) else {
        if !*waiting_logged {
            log_line("replay recorder: WA supplied no output path yet; waiting without recording");
            *waiting_logged = true;
        }
        return;
    };
    let mods = mods.clone();
    let extras = extras.clone();
    match Recorder::start(wa_path, &mods, &extras) {
        Ok(recorder) => {
            log_line(&format!(
                "replay recorder: recording {} (streams in {})",
                recorder.wa_path.display(),
                recorder.dir.display()
            ));
            *state = State::Recording(recorder);
        }
        Err(e) => fail(state, e),
    }
}

impl Recorder {
    fn start(wa_path: PathBuf, mods: &[LoadedMod], extras: &[PackFolder]) -> Result<Self, String> {
        let packs = archive_packs(mods, extras)?;
        let collection = collection();
        let pack = replay::encode_dedup(&packs, |sha| collection.is_some_and(|c| c.contains(sha)))
            .map_err(|e| e.to_string())?;
        let header = Header {
            wa_version: "3.8.1".into(),
            build_hash: BUILD_ID.into(),
            modset: modset_fingerprint(mods),
            collection_version: collection.map_or(0, |c| c.version()),
        };
        let dir = pending_root().join(format!("{}-{}", std::process::id(), unique_suffix()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        game::overwrite::write(&dir.join(PENDING_PACK), &pack)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let wf = create_stream(&dir.join(PENDING_WF)).map_err(|e| e.to_string())?;
        let chk = create_stream(&dir.join(PENDING_CHK)).map_err(|e| e.to_string())?;
        // The note last: a directory without one is not yet a recording.
        game::overwrite::write(&dir.join(PENDING_NOTE), write_note(&wa_path, &header).as_bytes())
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Self {
            wa_path,
            dir,
            wf,
            chk,
            wf_bytes: 0,
            chk_bytes: 0,
            last_frame: None,
        })
    }

    fn complete(mut self) -> Result<PathBuf, String> {
        self.wf.flush().map_err(|e| e.to_string())?;
        self.chk.flush().map_err(|e| e.to_string())?;
        self.wf.get_ref().sync_all().map_err(|e| e.to_string())?;
        self.chk.get_ref().sync_all().map_err(|e| e.to_string())?;
        drop(self.wf);
        drop(self.chk);
        finalise(&self.dir)?;
        Ok(self.wa_path)
    }
}

const PENDING_NOTE: &str = "note.txt";
const PENDING_PACK: &str = "pack.bin";
const PENDING_WF: &str = "wf.bin";
const PENDING_CHK: &str = "chk.bin";

fn pending_root() -> PathBuf {
    wormforge_dir().join("replay-pending")
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// `key=value` lines: the `.WAgame` the trailer belongs on, and its header.
fn write_note(wa_path: &Path, header: &Header) -> String {
    format!(
        "target={}\nwa_version={}\nbuild_hash={}\nmodset={}\ncollection_version={}\nformat_version={}\n",
        wa_path.display(),
        header.wa_version,
        header.build_hash,
        header.modset,
        header.collection_version,
        replay::FORMAT_VERSION
    )
}

fn read_note(text: &str) -> Result<(PathBuf, Header, u16), String> {
    let mut target = None;
    let mut header = Header::default();
    // Notes created before version 3 have no format_version line.
    let mut format = replay::LEGACY_FORMAT_VERSION;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "target" => target = Some(PathBuf::from(value)),
            "wa_version" => header.wa_version = value.into(),
            "build_hash" => header.build_hash = value.into(),
            "modset" => header.modset = value.into(),
            "collection_version" => {
                header.collection_version = value
                    .parse()
                    .map_err(|_| format!("bad collection_version {value:?}"))?
            }
            "format_version" => {
                format = value
                    .parse()
                    .map_err(|_| format!("bad format_version {value:?}"))?
            }
            _ => {}
        }
    }
    Ok((target.ok_or("note names no target")?, header, format))
}

/// Append a pending recording's trailer to its `.WAgame` and remove the
/// pending directory. The `.WAgame` is replaced atomically: a crash leaves
/// either WA's file or the finished one.
fn finalise(dir: &Path) -> Result<PathBuf, String> {
    let note = std::fs::read_to_string(dir.join(PENDING_NOTE))
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let (target, header, format) = read_note(&note)?;
    let pack = read_all(&dir.join(PENDING_PACK))?;
    let wf = read_all(&dir.join(PENDING_WF))?;
    let chk = read_all(&dir.join(PENDING_CHK))?;
    // Decode before publishing: catches a torn stream even when every
    // individual append succeeded.
    replay::decode_inputs(&wf).map_err(|e| e.to_string())?;
    replay::decode_checkpoints(&chk, format).map_err(|e| e.to_string())?;
    let wa = read_all(&target)?;
    if Trailer::find(&wa).map_err(|e| format!("{}: {e}", target.display()))?.is_some() {
        // Finalised already; the crash came between the replace and the
        // clean-up.
        log_line(&format!(
            "replay recorder: {} already carries its trailer",
            target.display()
        ));
    } else {
        let file = replay::append_trailer_with_format(&wa, &header, &pack, &wf, &chk, format)
            .map_err(|e| e.to_string())?;
        game::overwrite::write(&target, &file).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    if let Err(e) = std::fs::remove_dir_all(dir) {
        log_error(&format!("replay recorder: cannot remove {}: {e}", dir.display()));
    }
    Ok(target)
}

/// Finalise recordings a crash or a failed boundary left behind. Skips the
/// active recording and any directory owned by another running WA. One that
/// cannot be finalised is renamed `*.failed` so it is reported once, not on
/// every boundary; its `.WAgame` stays a vanilla replay.
pub fn recover_pending() {
    let active = {
        let state = STATE.lock().unwrap_or_else(|p| p.into_inner());
        match &*state {
            State::Recording(r) => Some(r.dir.clone()),
            _ => None,
        }
    };
    let Ok(entries) = std::fs::read_dir(pending_root()) else {
        return;
    };
    let me = std::process::id();
    for entry in entries.flatten() {
        let dir = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !dir.is_dir() || name.ends_with(".failed") || Some(&dir) == active.as_ref() {
            continue;
        }
        let owner = name.split('-').next().and_then(|p| p.parse::<u32>().ok());
        if owner.is_some_and(|pid| pid != me && process_alive(pid)) {
            continue;
        }
        if !dir.join(PENDING_NOTE).is_file() {
            // Crashed during start: no recording was made.
            let _ = std::fs::remove_dir_all(&dir);
            continue;
        }
        match finalise(&dir) {
            Ok(path) => log_line(&format!("replay recorder: recovered {}", path.display())),
            Err(e) => {
                log_error(&format!(
                    "replay recorder: cannot recover {}: {e}; that game stays a vanilla replay",
                    dir.display()
                ));
                let _ = std::fs::rename(&dir, suffix(&dir, ".failed"));
            }
        }
    }
}

fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let alive = GetExitCodeProcess(handle, &mut code) != 0 && code == STILL_ACTIVE as u32;
        CloseHandle(handle);
        alive
    }
}

/// The `.WFAssets` collection next to `wkLua.dll`, index only, loaded once.
pub(crate) fn collection() -> Option<&'static Collection<File>> {
    static COLLECTION: OnceLock<Option<Collection<File>>> = OnceLock::new();
    COLLECTION
        .get_or_init(|| {
            let path = wa_directory().join(assets::FILE_NAME);
            if !path.is_file() {
                log_line(&format!(
                    "replay: no {} next to wkLua.dll; replays embed every asset and \
                     cannot play ones that reference the collection",
                    assets::FILE_NAME
                ));
                return None;
            }
            match Collection::open(&path) {
                Ok(c) => {
                    log_line(&format!(
                        "replay: {} release {} ({} assets)",
                        assets::FILE_NAME,
                        c.version(),
                        c.entries().len()
                    ));
                    Some(c)
                }
                Err(e) => {
                    log_error(&format!("replay: {} unusable: {e}", path.display()));
                    None
                }
            }
        })
        .as_ref()
}

/// Fill in every collection reference, or refuse naming what is missing and
/// which collection release the replay was recorded with.
fn resolve_assets(packs: Vec<Pack>, recorded_with: u32) -> Result<Vec<Pack>, String> {
    if replay::asset_refs(&packs).is_empty() {
        return Ok(packs);
    }
    let collection = collection();
    replay::resolve_packs(packs, |sha| {
        let c = collection?;
        match c.get(sha) {
            Ok(bytes) => bytes,
            Err(e) => {
                // A tampered blob: loud, and treated as missing.
                log_error(&format!("replay: {e}"));
                None
            }
        }
    })
    .map_err(|e| {
        let have = collection.map_or("no collection".to_string(), |c| {
            format!("collection release {}", c.version())
        });
        format!(
            "{e}. This replay was recorded with collection release {recorded_with}; \
             this install has {have}. A newer WormForge release is needed."
        )
    })
}

fn finish_state(state: &mut State) {
    let old = std::mem::replace(state, State::Idle);
    match old {
        State::Recording(recorder) => match recorder.complete() {
            Ok(path) => log_line(&format!("replay recorder: completed {}", path.display())),
            Err(e) => {
                log_error(&format!("replay recorder: completion failed: {e}"));
                *state = State::Failed;
            }
        },
        State::Playback(player) => {
            PLAYBACK_ACTIVE.store(false, Ordering::Release);
            if let Some(detail) = &player.failure {
                log_error(&format!(
                    "replay player: stopped {} after failure: {detail}",
                    player.source.display()
                ));
            } else if player.next_input == player.inputs.len()
                && player.next_checkpoint == player.checkpoints.len()
            {
                log_line(&format!(
                    "replay player: completed {} with all {} frames and {} checkpoints verified",
                    player.source.display(),
                    player.inputs.len(),
                    player.checkpoints.len()
                ));
            } else {
                log_line(&format!(
                    "replay player: ended early after {}/{} frames and {}/{} checkpoints",
                    player.next_input,
                    player.inputs.len(),
                    player.next_checkpoint,
                    player.checkpoints.len()
                ));
            }
            if let Err(e) = std::fs::remove_dir_all(&player.temp_root) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    log_error(&format!(
                        "replay player: cannot remove {}: {e}",
                        player.temp_root.display()
                    ));
                }
            }
        }
        _ => {}
    }
}

fn fail(state: &mut State, detail: String) {
    log_error(&format!(
        "replay recorder stopped: {detail}; this game stays a vanilla replay"
    ));
    if let State::Recording(recorder) = std::mem::replace(state, State::Failed) {
        drop(recorder.wf);
        drop(recorder.chk);
        let _ = std::fs::rename(&recorder.dir, suffix(&recorder.dir, ".failed"));
    }
}

fn fail_player(player: &mut Player, detail: String) {
    if player.failure.is_none() {
        log_error(&format!("replay player stopped: {detail}"));
        player.failure = Some(detail);
    }
}

fn append_capped(writer: &mut BufWriter<File>, size: &mut u64, bytes: &[u8]) -> Result<(), String> {
    let next = size
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| "stream size overflow".to_string())?;
    if next > MAX_WF_BYTES as u64 {
        return Err(format!("exceeded the {MAX_WF_BYTES}-byte cap"));
    }
    writer.write_all(bytes).map_err(|e| e.to_string())?;
    *size = next;
    Ok(())
}

unsafe fn replay_prefix() -> usize {
    crate::wa::address("G_REPLAY_PREFIX") as usize
}

unsafe fn output_path() -> Option<PathBuf> {
    let prefix = unsafe { replay_prefix() };
    if prefix == 0 {
        return None;
    }
    unsafe { prefix_path(prefix as *const u8, REPLAY_OUTPUT_PATH) }.map(absolute_wa_path)
}

unsafe fn prefix_path(prefix: *const u8, offset: usize) -> Option<PathBuf> {
    let text = unsafe { prefix.add(offset) };
    let bytes = unsafe { std::slice::from_raw_parts(text, REPLAY_PATH_CAPACITY) };
    let end = bytes.iter().position(|b| *b == 0)?;
    if end == 0 {
        return None;
    }
    Some(PathBuf::from(
        String::from_utf8_lossy(&bytes[..end]).into_owned(),
    ))
}

fn absolute_wa_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        wa_directory().join(path)
    }
}

fn suffix(path: &Path, text: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(text);
    PathBuf::from(name)
}

fn create_stream(path: &Path) -> std::io::Result<BufWriter<File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Not truncate(true): see game::overwrite.
    game::overwrite::create(path).map(BufWriter::new)
}

fn materialize_packs(root: &Path, packs: &[Pack]) -> Result<(), String> {
    let mut ids = std::collections::HashSet::new();
    for pack in packs {
        if !ids.insert(pack.id.clone()) {
            return Err(format!("pack archive repeats id {:?}", pack.id));
        }
        let pack_dir = root.join(safe_component(&pack.id)?);
        std::fs::create_dir_all(&pack_dir).map_err(|e| format!("{}: {e}", pack_dir.display()))?;
        let mut paths = std::collections::HashSet::new();
        for file in &pack.files {
            let relative = safe_relative_path(&file.path)?;
            if !paths.insert(relative.clone()) {
                return Err(format!(
                    "pack {} repeats file {}",
                    pack.id,
                    relative.display()
                ));
            }
            let output = pack_dir.join(relative);
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            let mut handle = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&output)
                .map_err(|e| format!("{}: {e}", output.display()))?;
            let Content::Raw(bytes) = &file.content else {
                return Err(format!("pack {} file {} was never resolved", pack.id, file.path));
            };
            handle
                .write_all(bytes)
                .map_err(|e| format!("{}: {e}", output.display()))?;
        }
    }
    Ok(())
}

fn safe_component(text: &str) -> Result<PathBuf, String> {
    let path = safe_relative_path(text)?;
    if path.components().count() != 1 {
        return Err(format!("pack id is not one path component: {text:?}"));
    }
    Ok(path)
}

fn safe_relative_path(text: &str) -> Result<PathBuf, String> {
    let path = Path::new(text);
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            _ => return Err(format!("unsafe archived path: {text:?}")),
        }
    }
    if clean.as_os_str().is_empty() {
        return Err("archived path is empty".into());
    }
    Ok(clean)
}

fn read_all(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|mut f| f.read_to_end(&mut bytes))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(bytes)
}

fn archive_packs(mods: &[LoadedMod], extras: &[PackFolder]) -> Result<Vec<Pack>, String> {
    let mut packs = Vec::with_capacity(mods.len() + extras.len());
    let folders = mods
        .iter()
        .map(|m| (&m.manifest.id, &m.manifest.version, &m.root))
        .chain(extras.iter().map(|e| (&e.id, &e.version, &e.root)));
    for (id, version, root) in folders {
        let mut files = Vec::new();
        collect_files(root, root, &mut files)?;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        packs.push(Pack {
            id: id.clone(),
            version: version.clone(),
            files,
        });
    }
    packs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(packs)
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PackFile>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let ty = entry
            .file_type()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if ty.is_symlink() {
            return Err(format!("pack contains a symlink: {}", path.display()));
        }
        if ty.is_dir() {
            collect_files(root, &path, out)?;
        } else if ty.is_file() {
            let relative = path.strip_prefix(root).map_err(|e| e.to_string())?;
            let mut parts = Vec::new();
            for component in relative.components() {
                match component {
                    Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
                    _ => return Err(format!("invalid pack path: {}", relative.display())),
                }
            }
            out.push(PackFile {
                path: parts.join("/"),
                content: Content::Raw(
                    std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?,
                ),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod archive_tests {
    use super::*;

    #[test]
    fn the_selection_and_its_bundles_are_archived_with_the_packs() {
        let root = std::env::temp_dir().join("wf-replay-archive-extras");
        let _ = std::fs::remove_dir_all(&root);
        let pack = root.join("a");
        std::fs::create_dir_all(&pack).unwrap();
        let toml = "id = \"a.pack\"\napi_version = 1\nentry = \"init.lua\"\n";
        std::fs::write(pack.join("mod.toml"), toml).unwrap();
        std::fs::write(pack.join("init.lua"), "-- a").unwrap();
        let selection = root.join("stage").join("wormforge.profile");
        std::fs::create_dir_all(&selection).unwrap();
        std::fs::write(selection.join("profile.toml"), "scheme = \"x\"\n").unwrap();
        let mods = [LoadedMod { manifest: weapons::parse_manifest(toml).unwrap(), root: pack }];
        let extras = [PackFolder { id: "wormforge.profile".into(), version: "1".into(), root: selection }];
        let packs = archive_packs(&mods, &extras).unwrap();
        let ids: Vec<_> = packs.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["a.pack", "wormforge.profile"]);
        let file = &packs[1].files[0];
        assert_eq!(file.path, "profile.toml");
        assert_eq!(file.content, Content::Raw(b"scheme = \"x\"\n".to_vec()));
    }
}
