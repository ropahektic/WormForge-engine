//! Recorder and player for `.WF_WAgame`.
//!
//! WA keeps writing its ordinary `.WAgame`; this module records only the
//! WormForge input/checkpoint streams alongside it. A phase-one container is
//! created with `complete = false` as soon as WA supplies an output filename.
//! At the next match boundary, after WA has closed its file, the vanilla bytes
//! are embedded verbatim and an atomic replace publishes `complete = true`.
//! A crash or write failure therefore leaves a file playback will refuse.
//!
//! On playback the `ReplayLoader` hook validates the container before WA sees
//! it, materialises the archived packs in a private directory, extracts the
//! byte-exact `[wa]` section, and redirects WA's input path to that file.

use crate::log::{wa_directory, wormforge_dir, BUILD_ID};
use crate::{log_error, log_line};
use game::entity::read_i32;
use game::offsets::{
    GAME_WORLD_FRAME_COUNTER, GAME_WORLD_RECORDED_FRAME, G_REPLAY_PREFIX, REPLAY_ACTIVE,
    REPLAY_INPUT_PATH, REPLAY_OUTPUT_PATH, REPLAY_PATH_CAPACITY,
};
use replay::{Checkpoint, Header, InputSample, Pack, PackFile, MAX_WF_BYTES};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use weapons::{modset_fingerprint, LoadedMod};
use windows_sys::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};

const CHECKPOINT_EVERY: i64 = 50;

enum State {
    Idle,
    Pending {
        mods: Vec<LoadedMod>,
        waiting_logged: bool,
    },
    Recording(Recorder),
    Playback(Player),
    Failed,
}

static STATE: Mutex<State> = Mutex::new(State::Idle);
static PLAYBACK_ACTIVE: AtomicBool = AtomicBool::new(false);

struct Recorder {
    wa_path: PathBuf,
    output: PathBuf,
    wf_path: PathBuf,
    chk_path: PathBuf,
    wf: BufWriter<File>,
    chk: BufWriter<File>,
    wf_bytes: u64,
    chk_bytes: u64,
    last_frame: Option<i64>,
    header: Header,
    pack: Vec<u8>,
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
    failure: Option<String>,
}

/// Arm recording for the newly loaded pack set. The replay name is allowed to
/// arrive later; WA does not always fill it before LoadSprites returns.
pub fn prepare(mods: &[LoadedMod]) -> bool {
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

/// Called before WA's mode-1 `ReplayLoader`. Returns false for an ordinary
/// `.WAgame`, leaving the original loader completely untouched.
pub unsafe fn redirect_playback(prefix: *mut u8) -> Result<bool, String> {
    if prefix.is_null() {
        return Ok(false);
    }
    let Some(raw_path) = (unsafe { prefix_path(prefix, REPLAY_INPUT_PATH) }) else {
        return Ok(false);
    };
    if !raw_path
        .extension()
        .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("WF_WAgame"))
    {
        return Ok(false);
    }
    let source = absolute_wa_path(raw_path);
    let bytes = read_all(&source)?;
    let container = replay::Container::parse(&bytes).map_err(|e| e.to_string())?;
    container
        .require_wa_version("3.8.1")
        .map_err(|e| e.to_string())?;
    let header = container.header().clone();
    let wa = container.wa().map_err(|e| e.to_string())?.to_vec();
    let packs = replay::decode_pack(container.pack().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let inputs = replay::decode_inputs(container.wf().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let checkpoints = replay::decode_checkpoints(container.chk().map_err(|e| e.to_string())?)
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
    let extracted = temp_root.join("playback.WAgame");
    write_synced(&extracted, &wa)?;
    unsafe { write_prefix_path(prefix, REPLAY_INPUT_PATH, &extracted)? };
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
        failure: None,
    };
    log_line(&format!(
        "replay player: accepted {} ({} frames, {} checkpoints, {} archived packs)",
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
    if rng != expected.rng {
        different.push(format!("rng {:08x}!={:08x}", rng, expected.rng));
    }
    if worms != expected.worms {
        different.push(format!("worms {:08x}!={:08x}", worms, expected.worms));
    }
    if actors != expected.actors {
        different.push(format!("actors {:08x}!={:08x}", actors, expected.actors));
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
        let checkpoint = Checkpoint {
            frame,
            rng,
            worms,
            actors,
        };
        let bytes = replay::encode_checkpoints(&[checkpoint]).map_err(|e| e.to_string())?;
        append_capped(&mut recorder.chk, &mut recorder.chk_bytes, &bytes)
            .map_err(|e| format!("checkpoint stream: {e}"))?;
    }
    recorder.last_frame = Some(frame);
    Ok(())
}

/// Complete the previous recording at the next match boundary. If WA's file
/// is absent or still unreadable, the phase-one container remains incomplete.
pub fn finish() {
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
    match Recorder::start(wa_path, &mods) {
        Ok(recorder) => {
            log_line(&format!(
                "replay recorder: pending {}",
                recorder.output.display()
            ));
            *state = State::Recording(recorder);
        }
        Err(e) => fail(state, e),
    }
}

impl Recorder {
    fn start(wa_path: PathBuf, mods: &[LoadedMod]) -> Result<Self, String> {
        let output = container_path(&wa_path)?;
        let wf_path = suffix(&output, ".wf.tmp");
        let chk_path = suffix(&output, ".chk.tmp");
        let packs = archive_packs(mods)?;
        let pack = replay::encode_pack(&packs).map_err(|e| e.to_string())?;
        let header = Header {
            wa_version: "3.8.1".into(),
            build_hash: BUILD_ID.into(),
            modset: modset_fingerprint(mods),
            flags: 0,
            complete: false,
        };
        let phase_one =
            replay::assemble(&header, &pack, b"", b"", b"").map_err(|e| e.to_string())?;
        atomic_write(&output, &phase_one).map_err(|e| e.to_string())?;
        let wf = create_stream(&wf_path).map_err(|e| e.to_string())?;
        let chk = create_stream(&chk_path).map_err(|e| e.to_string())?;
        Ok(Self {
            wa_path,
            output,
            wf_path,
            chk_path,
            wf,
            chk,
            wf_bytes: 0,
            chk_bytes: 0,
            last_frame: None,
            header,
            pack,
        })
    }

    fn complete(mut self) -> Result<PathBuf, String> {
        self.wf.flush().map_err(|e| e.to_string())?;
        self.chk.flush().map_err(|e| e.to_string())?;
        self.wf.get_ref().sync_all().map_err(|e| e.to_string())?;
        self.chk.get_ref().sync_all().map_err(|e| e.to_string())?;
        let wa = read_all(&self.wa_path)?;
        let wf = read_all(&self.wf_path)?;
        let chk = read_all(&self.chk_path)?;
        // Decode before publishing. This catches a torn temp stream even if
        // each individual append succeeded.
        replay::decode_inputs(&wf).map_err(|e| e.to_string())?;
        replay::decode_checkpoints(&chk).map_err(|e| e.to_string())?;
        self.header.complete = true;
        let file = replay::assemble(&self.header, &self.pack, &wa, &wf, &chk)
            .map_err(|e| e.to_string())?;
        atomic_write(&self.output, &file).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&self.wf_path);
        let _ = std::fs::remove_file(&self.chk_path);
        Ok(self.output)
    }
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
        "replay recorder stopped: {detail}; incomplete container retained"
    ));
    *state = State::Failed;
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
    crate::rebase::rb(G_REPLAY_PREFIX) as usize
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

unsafe fn write_prefix_path(prefix: *mut u8, offset: usize, path: &Path) -> Result<(), String> {
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    if bytes.contains(&0) {
        return Err("extracted replay path contains a NUL byte".into());
    }
    if bytes.len() >= REPLAY_PATH_CAPACITY {
        return Err(format!(
            "extracted replay path is {} bytes; WA accepts at most {}",
            bytes.len(),
            REPLAY_PATH_CAPACITY - 1
        ));
    }
    let target =
        unsafe { std::slice::from_raw_parts_mut(prefix.add(offset), REPLAY_PATH_CAPACITY) };
    target.fill(0);
    target[..bytes.len()].copy_from_slice(bytes);
    Ok(())
}

fn container_path(wa: &Path) -> Result<PathBuf, String> {
    let stem = wa
        .file_stem()
        .ok_or_else(|| format!("output replay has no filename: {}", wa.display()))?;
    let mut name = stem.to_os_string();
    name.push(".WF_WAgame");
    Ok(wa.with_file_name(name))
}

fn suffix(path: &Path, text: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(text);
    PathBuf::from(name)
}

fn create_stream(path: &Path) -> std::io::Result<BufWriter<File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map(BufWriter::new)
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(bytes)
        .and_then(|_| file.flush())
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("{}: {e}", path.display()))
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
            handle
                .write_all(&file.raw)
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

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temp = suffix(path, ".tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
    }
    let from: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn archive_packs(mods: &[LoadedMod]) -> Result<Vec<Pack>, String> {
    let mut packs = Vec::with_capacity(mods.len());
    for m in mods {
        let mut files = Vec::new();
        collect_files(&m.root, &m.root, &mut files)?;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        packs.push(Pack {
            id: m.manifest.id.clone(),
            version: m.manifest.version.clone(),
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
                raw: std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?,
            });
        }
    }
    Ok(())
}
