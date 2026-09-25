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
