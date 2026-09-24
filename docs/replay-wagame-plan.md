# Replays in `.WAgame` + the `.WFAssets` collection

> **Status (2026-09-24): S0–S4 done** on branch `wagame_replay`. Docs:
> [`replay.md`](replay.md), [`wfassets.md`](wfassets.md). Attaching the
> collection to a GitHub release is the maintainer's release step
> (`wfassets.md`, "Shipping it"). S3 was verified in-game: a match recorded with
> `gravler_strike` and `sentry_gun` stored 11 files as collection references,
> completed at match end, and played back with checkpoints matching. That
> test also found the match-end crash fixed in PR #13 (`panel_icon` double
> free).
> Supersedes the `.WF_WAgame` container described in `replay.md` and
> `replay-plan.md`.

## Context

Today a WormForge replay is a separate `.WF_WAgame` container: WA's `.WAgame`
bytes copied into a `[wa]` section, plus `[pack]` (every pack's files,
deflated), `[wf]` (input stream) and `[chk]` (checkpoints)
(`crates/replay`, `crates/engine/src/replay.rs`). Two problems:

- **Two files per game.** Playback has to find a sibling next to WA's file or
  in `User/Games`.
- **Size.** `[pack]` embeds every asset, so a match with `sentry_gun` produces
  a replay of several MB.

**Goal:** everything goes into WA's own `.WAgame`. Old `.WF_WAgame` files are
dropped with no compatibility path. Assets shrink through a central,
user-immutable **`.WFAssets`** collection shipped with each release:

- A pack file whose exact bytes are in the collection is recorded as a
  32-byte reference.
- Everything else is embedded in the `.WAgame`.
- Lua is always embedded.

**Decisions taken:**

- **v1 collection = all raw PX assets.** That's the PNG and WAV files from the
  PX extraction (about 3,400 PNGs and 424 WAVs, 150+ MB). It's shipped as one
  file alongside the release, not in git.
- **Invisible dedupe by content.** Packs don't change. The key is SHA-256 of
  the file bytes.
- **Lua and `mod.toml` are always embedded**, even when unchanged.
- **Append-only across releases:** every asset in any shipped collection is in
  every later one. Keeping that true is our job, and the tooling enforces it.

**A consequence, stated up front:** dedupe only hits **byte-identical** files.
The sentry's 93 sheets are baked by `bake_sentry.py`, not raw PX, so its
replays stay large until the pack ships the raw PX sprites and rotates them
on the GPU (which the renderer now supports). That's a follow-up, not part of
this plan. Pack authors porting PX art should **copy files, never re-save
them**.

## File layout

```
<name>.WAgame
  [WA's own bytes, untouched]      header "WA" .. stream .. event 0x16
  [WF trailer]                     section directory, same shape as today:
      hdr   wa_version, build_hash, modset, collection_version
      pack  packs; each file is Raw(bytes) or Asset(sha256)
      wf    input stream              (unchanged encoding)
      chk   checkpoints               (unchanged encoding)
  [footer, fixed 32 bytes]         "WFTR", u16 format, u16 flags,
                                   u64 trailer_len, u32 crc32(trailer),
                                   u32 wa_len, 8 bytes reserved
```

- **Detection** reads only the last 32 bytes. With no valid footer (magic,
  lengths that add up, CRC), the file is vanilla and is left to WA untouched.
  The CRC plus the length check rule out false positives on an ordinary
  replay.
- **`complete` becomes structural.** The trailer is written once, whole,
  with an atomic replace, so "has a trailer" means "complete". The two-phase
  header flag goes away.
- **Reused as they are:** `[wf]`/`[chk]` encoders, the directory reader, and
  the refuse-don't-degrade errors (`crates/replay/src/format.rs`,
  `error.rs`).
- **`[wa]` goes away,** because WA's bytes are the prefix of the file itself.

## `.WFAssets`

One file, `WormForge.WFAssets`, next to `wkLua.dll`. It's installed by the
release and by `tools/install-local.py`, and never edited by the loader or the
user.

```
"WFAS", u16 format, u32 collection_version (release number), u32 count
index, sorted by sha256:  sha256[32], u64 offset, u32 len, u8 method, path
blobs                     stored raw (PNG/WAV don't deflate; `method` leaves room)
footer                    sha256(header + index)
```

- **Reading** loads the index only (about 330 KB for v1) and fetches a blob on
  demand.
- **Every fetched blob is re-hashed.** A tampered entry counts as missing, and
  that's logged loudly. Users can't change what a replay resolves to.
- **Append-only, enforced by tooling:**
  - `assets/wfassets.lock` is committed to git. It has one line per entry:
    `sha256 size first_release path`.
  - The builder starts from the **previously shipped** `.WFAssets`, carries
    every entry forward, adds new sources, and appends their lines to the
    lock.
  - It refuses to write a collection that is missing any locked hash.
  - The shipped file is the source of truth after v1, so old sources never
    have to be kept around.
- **v1 is built from `docs/pxl_extracted`**, PNG and WAV files only. That
  extraction sits outside the repo; every later release builds from the v1
  file.

## Stages

### S0: spike, does WA tolerate a trailer? (gate)

Append 5 MB of junk plus a valid footer to corpus replays
(`~/worms_work/wkFOW/clones/openwa/testdata/replays/`, e.g. `girder.WAgame`)
and check each of these:

1. `/play` runs to the end. Use `tools/sync-run.sh`, and compare the `sync`
   hashes with the untouched file.
2. `/playat` runs.
3. WA's replay browser still lists the file, and still shows its map and
   scheme (`ReplayLoader` modes 2 and 3).
4. Export Log works.
5. WA never rewrites or truncates the file, for example through "repair"
   (mode 4).

**Result (2026-09-24): passed.** `girder.WAgame` with 5 MB of random bytes
and a valid footer appended, against the untouched file, WA 3.8.1 GOG under
Wine, with wkLua loaded:

- `/getlog` (a full headless simulation): byte-identical log, no checksum
  mismatches, the same as the corpus `_expected.log` apart from the "(Steam)"
  version tag.
- `/getmap`, `/getscheme` (`ReplayLoader` modes 2 and 3, which the browser
  uses): byte-identical `.BIT` and `.wsc`.
- Played from `User/Games` end to end.
- Every file's SHA-256 was unchanged after all of the above; mode 4 (repair)
  never ran.
- Not testable here: WA's own replay browser needs the Windows file dialog,
  which this Wine setup lacks. Modes 2 and 3 above are what it calls.

An earlier run showed checksum mismatches and a teardown crash: wkModules
were disabled by accident. Rerun with them loaded, both went away.

So WA plays the file directly, and playback needs no extract-to-temp.

**If all pass,** WA can play the file directly. **If only playback fails,**
keep today's extract-to-temp redirect (it already exists in
`redirect_playback`), which works either way. **If the browser can't list the
file,** stop and re-plan around WA's ignored in-stream events (`0x0C`, `0x6B`,
`0x70–0x73`), which have a 1 KB-per-event limit.

### S1: `crates/assets` (new, host-testable, no engine dependency)

- Build, read, and verify for the collection format.
- A lock-file parser and the rule that the collection must contain every
  locked hash.
- `bin/wf-assets`: `build --previous <file> --add <dir> --lock <file> --out
  <file>`, plus `verify` and `list`.
- **Move `crates/engine/src/sha256.rs` here.** It's only testable once it
  leaves the engine; `modloader.rs` then imports it from here.
- Tests:
  - round-trip
  - a tampered blob is refused
  - carry-forward keeps every old entry
  - a missing locked hash is refused
  - the index is sorted and lookup is binary search

### S2: `crates/replay`, the new trailer

- **Remove:** `MAGIC "WFRP"`, `TAG_WA`, the `complete` flag, `assemble`'s
  `[wa]`, and the `Container::wa`/`Incomplete` paths.
- **Add:**
  - `append_trailer(wa_bytes, header, pack, wf, chk) -> Vec<u8>`
  - `find_trailer(file) -> Option<Trailer>`, which reads the footer first
  - `Trailer::wa_prefix()`
- **`pack.rs`:** `PackFile.content` is `Raw(Vec<u8>)` or `Asset([u8; 32])`.
  `encode_dedup(packs, has: impl Fn(&[u8; 32]) -> bool)` never dedupes `.lua`
  or `mod.toml`. `resolve(packs, get)` fails by naming every missing hash.
- Bump `FORMAT_VERSION`.
- Tests:
  - a vanilla file is not detected (including a random tail that ends in the
    magic)
  - a truncated or bad-CRC trailer is refused
  - the WA prefix comes back byte-identical
  - Lua is never deduped
  - a missing asset is named

### S3: engine, recording and playback

- **Recording** (`crates/engine/src/replay.rs`): at the match boundary, after
  WA has closed its file (the same point as today):
  - read WA's file, dedupe the packs against the loaded collection, append the
    trailer
  - replace the file with `game::overwrite::write`, which uses temp + rename
    and is safe under Wine
  - drop the `.WF_WAgame` sibling
- **Recovery:** keep the temp stream files in `wormforge_dir()` together with
  a small pending note naming the `.WAgame` path. Finalise leftovers at the
  next match boundary or at DLL start. If one can't be finalised, log it. The
  file then stays vanilla-looking, which `docs/replay.md` has to say plainly.
- **Playback** (`redirect_playback`):
  - no footer means vanilla, return `false`
  - otherwise resolve assets from the collection. If anything is missing,
    refuse and name the hashes and the `collection_version` the replay needs
    ("recorded with a newer WormForge release").
  - materialise the packs as today
  - redirect WA to the file itself, or to the extracted prefix, per S0
- **Delete:** the sibling lookup (`container_path`) and the `.WF_WAgame`
  extension handling. **Keep a `User/Games` fallback:** under the macOS Wine
  bundle WA hands the loader a host path with a drive letter glued on, which
  does not exist inside Wine (found in testing).
- **Load the collection once** at startup, index only. `hdr.collection_version`
  records which version the recorder had.

### S4: release and docs

- `tools/install-local.py`: also install `WormForge.WFAssets` (skip it when
  unchanged, since it's 150 MB). Done locally; `tools/` is not tracked.
- Release packaging: attach the collection file to the GitHub release; keep
  every shipped version.
- Docs:
  - `docs/replay.md` (new layout, recovery, what an old `.WF_WAgame` is now)
  - new `docs/wfassets.md` (append-only rule, the lock, the "copy, don't
    re-save" rule for pack authors, how to cut a new collection)

## Critical files

- `crates/replay/src/{format.rs, pack.rs, lib.rs, error.rs, bin/wf-replay.rs}`
- `crates/engine/src/replay.rs` (recorder finalisation, `redirect_playback`,
  sibling code to delete)
- `crates/engine/src/sha256.rs` → moves to `crates/assets`;
  `crates/engine/src/modloader.rs` imports it from there
- `crates/assets/` (new), `assets/wfassets.lock` (new)
- `crates/game/src/overwrite.rs` (reused for the atomic replace)
- `tools/install-local.py`, `docs/replay.md`, `docs/wfassets.md`

## Risks

- **S0 is the whole plan's premise.** If WA's browser can't list a file with a
  trailer, the carrier changes to in-stream events.
- **A crash before finalisation** leaves a modded game in a vanilla-looking
  file. Recovery covers ordinary crashes, not a deleted temp directory. This
  gets documented, not hidden.
- **Replays recorded on a newer release** may reference assets an older
  release lacks. That's refused cleanly, never guessed around. Append-only
  means the opposite direction always works.
- **Release size grows by about 150 MB.** Only the index is read at startup.
- **The v1 collection is exactly what `pxl_extract.py` produced.** A re-run
  that changes bytes would create different hashes. v1 freezes that; after
  it, the shipped file is the source.

## Verification

- `cargo test -p assets -p replay --target aarch64-apple-darwin`
- `wf-assets build` for v1, then `wf-assets verify`; `wf-assets list | wc -l`
  is the **unique** PX PNG + WAV count. Done for v1: 3,398 PNG + 424 WAV =
  3,822 files, 500 of them byte-identical copies, so 3,322 entries, 150.6 MB.
- Record a real match with `highlander`, `sentry_gun`, and a test pack that
  ships one PX PNG byte-for-byte:
  - the `.WAgame` grows by the Lua plus the sentry's sheets, **not** by the
    PX PNG
  - `wf-replay` shows that file as `Asset(sha…)`
- **Play it back with `Mods/` emptied:** same `sync` hashes as the recording
  (`tools/sync-diff.sh`).
- **Tamper with one blob in `.WFAssets`:** playback of a replay that uses it
  refuses and names the hash.
- **An old `.WF_WAgame`** is not picked up. **A vanilla `.WAgame`** plays as
  before.
- WA's replay browser lists the new file with the right map and scheme (from
  S0).
