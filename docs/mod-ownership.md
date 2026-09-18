# Multi-mod ownership

WA 3.8.1 has **71 weapon slots**. v1 overlays those slots. Extra panel pages are later. Two packs cannot both own bazooka.

Composition is a **dispatcher**: named ids, first loaded wins, no vtable stacking.

## Weapons

Load mods sorted by `id`. Each overlay claim names a stock slot. **The first claim for that slot is applied. Later claims for the same slot are skipped.** The rest of the later pack still loads (other slots, logs, actor kinds, hooks).

The match is not aborted. The log names the winner and the skipped pack.

- Alice overlays bazooka, Bob overlays grenade — both apply.
- Alice overlays bazooka, Bob overlays bazooka — Alice wins if `alice...` sorts first; Bob’s bazooka claim is skipped; Bob can still overlay grenade or register actors.
- Duplicate folder ids — the later folder is ignored.

`based_on` is always the **stock** template snapshotted after `InitWeaponTable`, before any overlay.

Per-weapon fire/explode handlers (when wired) belong to the **slot owner** only. Two `on_fire` functions do not chain.

## Actors

Click-spawn is Lua (`on_click` function or GUI spawn table); `wa.actors.spawn` / `on_fire` create more instances of the same native type.

## Sprites

Keys are `mod_id:relative/path`. Not a global WA sprite index. Two packs cannot collide by numeric id.

## What we do not do

- Last-write-wins
- Stacking two handlers on one slot (`FireFinal` chains)
- Growing the table to 72+ weapons in v1
- Failing the whole match because two packs wanted the same slot
- Dumping raw `CWorm*` / field offsets into Lua
