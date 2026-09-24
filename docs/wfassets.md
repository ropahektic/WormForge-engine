# The `.WFAssets` collection

`WormForge.WFAssets` is a read-only store of raw assets, shipped with each
WormForge release and installed next to `wkLua.dll`. Its only job is to keep
replays small: when a pack file's exact bytes are in the collection, a replay
records the file's SHA-256 (32 bytes) instead of the file, and playback reads
it back from the collection.

Release 1 holds every PNG and WAV from the Project X extraction: 3,822 files,
of which 500 were byte-identical copies, so **3,322 entries, 150.6 MB**.

Nothing breaks without it. A player without the collection records replays
with every asset embedded (larger files), and can play any replay that does
not reference it. A replay that does reference it is refused with the missing
hashes named, never played with guessed assets.

## For pack authors: copy, never re-save

Deduplication matches **exact bytes**. A PX sprite copied into your pack is
stored in a replay as 32 bytes. The same sprite opened and saved again --
even unchanged, even by a tool that only rewrites metadata -- is new bytes,
and every replay embeds all of it.

- Copy PX files into your pack as they are. Rotate, tint or scale them at
  runtime (the GPU renderer supports it) rather than baking new files.
- Lua and `mod.toml` are **always** embedded, whatever the collection holds:
  they define the behaviour, which must replay exactly as recorded.
- See what a replay would store for your pack:

  ```sh
  cargo run -p assets --bin wf-assets -- check WormForge.WFAssets path/to/your_pack
  ```

  Each file is listed as `asset` (resolved from the collection) or `embed`
  (stored in every replay), with the totals.

For example, `sentry_gun`'s seven sounds are PX files and resolve from the
collection; its 93 sprite sheets are baked by `bake_sentry.py`, so a replay
embeds them (147 KB). Shipping the raw PX sprites and rotating them on the GPU
would remove most of that.

## The rule: append-only

**Every asset in any shipped collection is in every later one.** A replay
recorded on release N references release-N assets, and must resolve on every
release after N. Removing an entry would break every replay that uses it --
silently from the player's point of view, until they try to watch one.

Two things enforce it:

- **`assets/wfassets.lock`**, committed to git: one line per entry ever
  shipped, `sha256 size first_release path`. The path is where the blob first
  came from, for humans; identity is the hash.
- **`wf-assets build`** starts from the previously shipped collection and
  carries every entry forward, and **refuses to write** a collection that is
  missing any locked hash, or whose release number does not rise.

After release 1 the shipped file is the source of truth: new collections are
built from it, so the original extraction never has to be kept around or
re-run. (A re-run of `pxl_extract.py` that changed bytes would produce
different hashes; release 1 freezes what it produced.)

## Integrity

- Opening a collection reads only the header and index (about 330 KB for
  release 1), and checks the footer hash over both.
- **Every blob is re-hashed on every read.** A blob whose bytes were changed
  counts as missing and is logged loudly: users cannot change what a replay
  resolves to.

## Cutting a new collection

Only when a release adds assets worth referencing; otherwise ship the
previous file again unchanged.

```sh
# 1. Build from the last SHIPPED collection, adding new sources.
cargo run --release -p assets --bin wf-assets -- build \
    --previous WormForge.WFAssets.release-1 \
    --add path/to/new/assets \
    --version 2 \
    --lock assets/wfassets.lock \
    --out tmp/WormForge.WFAssets

# 2. Verify every blob, and that every locked hash is present.
cargo run --release -p assets --bin wf-assets -- verify tmp/WormForge.WFAssets \
    --lock assets/wfassets.lock

# 3. Commit the appended lock lines with the release.
git add assets/wfassets.lock
```

`--add` takes PNG and WAV by default (`--ext` changes that) and may be given
several times. Files already in the collection are skipped as duplicates.
`--version` is the release number and must be higher than the previous
collection's.

Release 1 was built with:

```sh
wf-assets build --version 1 --lock assets/wfassets.lock \
    --out tmp/WormForge.WFAssets --add ../docs/pxl_extracted
```

## Shipping it

The collection is 150 MB, so it lives beside the release rather than in git
or in the DLL zip:

1. Attach `WormForge.WFAssets` to the GitHub release as its own asset, next
   to the DLL zip. Keep every shipped version attached to its release: the
   next build needs it as `--previous`.
2. The install note says: put `WormForge.WFAssets` next to `wkLua.dll`
   (the WA folder). Players who skip it lose nothing but replay size and the
   ability to watch replays that reference it.
3. Locally, `tools/install-local.py` installs `tmp/WormForge.WFAssets` when
   it has changed (the file is too large to copy on every install).

The in-game loader does not download the collection yet; a follow-up could
fetch it from the release like packs are fetched from the index.

## File format

```text
"WFAS", u16 format (1), u32 release, u32 count
index, sorted by sha256, one entry per blob:
    sha256[32], u64 offset, u32 len, u8 method, u16 path_len, path (UTF-8)
blobs, at the offsets the index names
footer: sha256(header + index)
```

Little-endian throughout; offsets are absolute. `method` 0 stores the blob
raw (PNG and WAV do not deflate); readers refuse methods they do not know.
Lookup is a binary search on the sorted index. Code: `crates/assets`.
