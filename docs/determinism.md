# Lockstep

Worms Armageddon is deterministic lockstep. Lua must not introduce host-only noise.

- Lua 5.4 (`mlua`, vendored). Not LuaJIT, not PXS.
- Scripts run once at gfx `InitGraphic` (before `InitWeaponTable` overlays slots). Definitions stay fixed for the match. Next match: new interpreter.
- Sandbox: no `io`, `os`, `debug`, `package`, `dofile`, `loadfile`, `load`, `require`, `math.random`.
- Integers in `params`; Lua sees handles/tables, not raw pointers.
- Enabled set sorted by mod `id`. `weapons::modset_fingerprint` (`id@version`) is the seed of a future multiplayer/replay check. Phase 1 does not hash files and does not gate netplay.

Do not read the clock or the filesystem from Lua. Asset paths are declared in tables and resolved by native code inside that pack’s folder only.

Anything that **moves or deals damage** must run on the game thread and on messages both peers already see. HUD is presentation-only.

## Physics / scripted missiles

`wa.on.gravity_before` / `gravity_after` wrap `CollisionManager__apply_projectile_gravity` (gravity+wind+damp leaf). Bounce/move is `CollisionManager__rope` in `update_0`. Mutate scales before stock, velocity after.

Do not drive fuse, arm, or detonate from `GetAsyncKeyState` or `Instant`. Count `wa.msg.FRAME_FINISH` if you need a rest timer.

When LuaActor exists, its `HandleMessage` is the same rule: sim on the message pump, draw on render messages. `on.frame` runs on FrameFinish; blit on RenderScene. `wa.actors.spawn`, `on_click`, and `on_fire` run on StrikeFire / CreateWeaponProjectile / FRAME_FINISH — messages both peers already see. HUD must not spawn. `wa.land` is a read of the live grid both peers share.

## Input that already replicates

Jump (`0x24`) and JumpUp (`0x25`) are stock worm messages (Enter / Backspace). They are already in the multiplayer stream.

`wa.on.worm_message(function(worm, msg) ... end)` runs on that HandleMessage. `worm` is a handle: `id`, `team`, writable `x`/`y`/`vx`/`vy` (16.16). Repositioning a worm here is applied by both peers because they both see the message. HUD must not write worm pos.

The worm does not jump under an **anchored** scheme. Swallowing Jump (`return "eat"`) is extra and not required for that. Only eat when the scheme already ignores that action.

## Custom lockstep packets (standby)

Sending arbitrary inputs through lockstep (so a mod does not hijack Jump) is not implemented. If it stays hard, leave it on standby and keep using existing messages.
