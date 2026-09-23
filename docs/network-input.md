# Synchronized Lua input

`wa.world.keys()` and `wa.world.pointer()` are synchronized during networked
matches. The local sample is latched once per simulation frame, encoded as a
12-byte WormForge record, and dispatched through WA's normal lockstep input
queue before Lua consumes it.

The carrier uses provisional message kind `0x80`. Stock WA does not understand
this kind, so every peer in the match must run the same WormForge build. Input
records are emitted on key/button changes, while a requested pointer is moving,
and as a low-rate heartbeat. A networked reader never falls back to local OS
input if a record is absent; it receives neutral input instead.

The production DLL always enables synchronized input and needs no marker file.
`sync.txt` is not read. Expensive state hashing, stock-message traces, and dump
files are compiled only when building with:

```sh
cargo build --release -p wk-lua --target i686-pc-windows-msvc \
  --features sync-diagnostics
```

Normal release builds omit those diagnostics while retaining the input carrier.
The diagnostic tools are `tools/sync-state-diff.py` and
`tools/sync-input-matrix.py`.

The implementation also closes deterministic-state gaps exposed during network
testing:

- actor and worm IDs visible to Lua are stable identities rather than pointers;
- `wa.random()` advances WA's shared gameplay RNG;
- `wa.sorted(table)` provides a deterministic order for scalar table keys;
- callbacks restore the registering pack's `wa._mod` before running;
- Lua's process-local RNG and coroutine library remain unavailable;
- empty stock sprite metadata is initialized before WA serializes animation
  state.

Replay recording and playback are deliberately outside this change.
