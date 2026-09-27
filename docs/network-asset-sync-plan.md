# Host-to-client asset sync

## Summary

When the host turns on their green ready bulb, WormForge freezes the **entire enabled pack set** and starts syncing it to every connected client. The host cannot start the round until each client has verified and staged the same files on disk. The implementation is self-contained; wkTerrainSync supplies the lobby-transfer pattern, not a runtime dependency.

## Implementation

- Hook the WA 3.8.1 host ready-bulb transition to create a new sync generation. Reuse WormForge's pack discovery rules, then copy the selected `mod.toml`, Lua, sprites, sounds, and other pack files into an immutable session directory. Confirm the exact bulb transition in the lobby before wiring the hook.
- Add a versioned WormForge lobby protocol, separate from wkTerrainSync's packet ID. Send a canonical manifest of pack IDs, paths, sizes, and SHA-256 hashes; clients request only blobs they cannot verify from local packs or `.WFAssets`. Transfer bounded chunks with acknowledgements and retries, verify each blob, and publish the completed session directory atomically. Keep disk work off the lobby thread.
- Track completion by client slot and sync generation. A late joiner receives the current manifest; an unready host or changed pack selection creates a new generation. Gate the host's Start path until every connected client reports the exact manifest digest and compatible WormForge build. Show transfer progress or the blocking error in the lobby.
- Load network rounds from the frozen session directory, with replay playback retaining priority over lobby assets. Leave installed `Mods/` untouched. Preserve the session files until every round recording that uses them has been finalized or recovered.
- On round end, after WA closes `.WAgame`, build the existing replay pack table from the frozen session files. Embed Lua, manifests, and assets absent from the local `.WFAssets`; store verified collection assets as **SHA-256 references**, never copy their bytes into the replay. Keep enough session metadata in the pending recording for crash recovery. The replay trailer format and Lua API need no change.

## Verification

Test manifest and path validation, changed files with unchanged pack versions, interrupted or corrupt transfers, late joins, disconnects, repeated ready transitions, and Start gating. In a two-client WA lobby, verify identical loaded bytes and a second round after returning to the lobby. Record replays with and without `.WFAssets`, remove the installed packs, and verify playback and crash recovery. Test alongside wkTerrainSync, then run the Rust checks and Windows DLL build.

## Assumptions

“Assets” means all files in enabled packs. “RA” refers to `.WFAssets`: its contents are referenced in replays when available, while missing collection blobs are embedded locally. Unsupported clients cannot participate in a host-lit WormForge round.

## Progress

**Step 1, the host's ready bulb (done).** `crates/engine/src/lobby.rs` reads the bulb from WA's lobby table, `CPlayers` (0x8779D8): this machine's slot is at +8, and slot 0 is the host; each record's ready flag is at +0x82, active at +0x80, records 0x78 apart. It observes the table from both player lists' `UpdateReady` and the host's Start gate (`re/Lobby.toml`). In a solo hosted online lobby, the host's own click was reported by the player list (`LobbyList__UpdateReady`, 0x482700) on every toggle. Each lit edge freezes the enabled pack set on a worker thread (`crates/netsync`): 7 packs, 672 files, 9.4 MB in about 0.5 s. Two lit edges over unchanged packs gave the same manifest digest, and only the newest session directory was kept. Still to confirm: whether the bulb comes back out when a round returns to the lobby, which decides whether a second round gets a new generation.

**Step 2, the lobby transfer (done).** `crates/engine/src/transfer.rs` hooks the host and client packet handlers of the lobby and of the end screen between rounds. It sends through the connection's vtable+0x34, as WA's own sends do, on the lobby thread. The host speaks first, and only to a slot whose WA packets have reached its lobby handler for 3 s; clients only answer. Messages ride inside WA chat packets (type 0) behind an ESC "WF" marker, and our hooks swallow them. In live tests over WormNAT2, a packet of our own type 0x4657 never reached the peer's handler and dropped the peer within 100 ms, while chat and wkTerrainSync's 0x6666 pass; the cause is not found. The hello starts with a readable chat line, the only thing a player without WormForge sees. Clients pull the manifest, verify files against installed packs, `.WFAssets` and their last session, and fetch the rest. Live: 394 files local, 53 fetched (41 KB), ready 1.2 s after the offer.

**Step 3, the Start gate (done).** The hook on `CPlayers::IsOkToStartNetGame` overrules WA's answer while a lit generation with packs has not reached every player in the lobby (`netsync::gate`). A player without WormForge blocks such a round; with no packs enabled, the gate stays out of the way. WA only re-evaluates Start on lobby events, so the host refreshes its Start button (the screen's vtable+0x170, as WA does after a bulb packet) whenever a client's status arrives. Progress and the reason Start waits show as local lines in each player's lobby chat.

**Step 4, rounds from the synced copy (done).** At match start, a network round loads its packs from the host's lit generation, or from the client's copy of the host's latest offer; offline rounds and replay playback are unchanged. Live: both machines loaded generation 3 from their session directories and their modsets matched. In a later test the client emptied its `Mods/`: it fetched the whole pack set over the lobby, played a Highlander round, and after the host disabled Highlander on the end screen, played a second round with the remaining custom weapons.

