# Custom weapon icons in the stock weapon panel

> **Status (2026-09-24): implemented** on branch `panel-icons` (on top of
> #17, the ammo-write dirty-flag fix). S0 was answered from the binary and
> confirmed in a live match setup (stock icons 32×32, 76 colours); the gate's
> in-panel check is the remaining step. Author docs: [`weapon-panel.md`](weapon-panel.md).
>
> Deviation from the plan: a PNG of the full icon size (32×32) is accepted
> and its window used, because every PX weapon icon is one -- they then work
> byte-for-byte and dedupe into `.WFAssets`. S0 and S3
> findings go in `docs/weapon-panel.md`; this file is the plan.

## Context

Custom weapons should show their **own icon** in WA's weapon panel. Everything
else about the panel stays 100% native. The panel keeps its 71-id / 5-column
layout; PX-style expandable pages are ruled out.

The first instinct was to re-implement the menu. Research says not to:

- **Project X does not replace the stock panel either.** It swaps icons inside
  it with `GS->SetWeaponIcon(Page, Weapon, icon)`. What is known of how it
  rebuilds is script-side only: its script authors re-set each icon *to
  itself* after every ammo change, in overrides of `SetWeaponEx`,
  `AddWeaponEx` and `AddWeapon`, under the comment "rebuild weap panel when
  adding weaps through SetWeaponEx function"
  (`utils_pyroman/classes_global_extentions.txt`,
  `scheme_cardmageddon/WeaponPanel.txt`, `lantern2016/Lemming.txt`). So in PX
  a script ammo change did **not** refresh the panel by itself, and
  `SetWeaponIcon` refreshes it as a side effect.

  **What wkPX.dll does (RE, 2026-09-24).** The script binding registers
  `SetWeaponIcon@iiPCCBitmap32@` → native `0x493AF4` (`GetWeaponIcon` →
  `0x493A9C`). `SetWeaponIcon(page, weapon, icon)`:
  1. bounds-checks page ≥ 1 and weapon 0..71;
  2. stores `icon` in PX's own page table (its expandable pages);
  3. only if `page` is the page on screen, writes the **same** pointer into
     both `GameWorld+0x144+8·weapon` and `+0x148+8·weapon` -- enabled and
     disabled frame alike -- with a plain memory write (`0x4B8F70`);
  4. calls `0x4B8DF4`, which is exactly `[[GameWorld+0x548]+0x1CC] = 1`, the
     dirty flag WA 3.8.1 also uses (same offsets on PX's 3.6.31).

  So PX never drew the panel itself: it wrote WA's icon table and set WA's
  dirty flag, and WA's own `redraw_panel` did the rest. `0x4B8DF4` has six
  callers (`SetWeaponIcon` plus page switching and other natives), and a
  second routine (`0x4AC480`) rewrites the on-screen page's icons the same
  way. It also means a PX icon has no separate greyed frame. Our planned
  approach (images in `+0x144`, rebuild via the dirty flag) is what PX does,
  minus the pages; the dirty-flag half landed as PR #17. Its "Page" argument is the
  expandable-menu system we are not adopting. Its separate `WeaponPanelEx`
  (`script_weapon_panel/wpanel_s.txt`) is an extra spectator panel, not a
  replacement.
- **WormForge already writes into that same place.** `GameWorld+0x144` holds
  two icon image pointers per weapon id (`GAME_WORLD_PANEL_IMG`,
  `crates/game/src/offsets.rs:120`). `copy_panel_icon`
  (`crates/engine/src/apply.rs:429`) already copies a stock weapon's icon
  there through `panel_icon = "banana"`.
- **A re-implementation would have to reproduce a lot:** the frame, the
  slide-in, the cursor, the hover ammo/delay text, the greyed states, keyboard
  and F-key navigation, and weapon selection. Selection is a lockstep input
  (`WeaponPanelCheckClick` 0x5690B0), and a local click path is exactly the
  class of desync the replay work has been removing.

**Decisions taken:**
- **Palette icons.** The PNG is quantized into a WA image and the stock panel
  draws it. No GPU overlay.
- **Icons are fixed per match; the panel's contents are not.** Which image a
  weapon id shows is decided at match setup. But each row lists only the
  weapons with ammo, packed left to right, so the panel is rebuilt mid-match
  all the time: crate collects, the highlander deal, `inventory.apply`,
  `worm:ammo`. The icons written at setup are picked up by every rebuild,
  because the rebuild reads `GameWorld+0x144` each time. Nothing here needs
  its own rebuild path -- but see "The panel only rebuilds when told to".
- **A custom weapon keeps the row of the slot it replaces.** Mortar's
  replacement sits where mortar sits.

## What is known about the panel

| Fact | Source |
|---|---|
| Rows = `WeaponEntry.panel_state`: F1–F12 = rows 1–12, 5 ids each; row 5 holds 6 (ids 21–26); row 0 = id 0 + ids 62–70 (utilities) | openwa `AssignWeaponPanelSlots` 0x537130 |
| Each row lists its *available* weapons, packed left to right in id order | PX `WPGetPos`/`WPCompact` mirror it |
| Tile draw reads `[world + 4*(frame + 2*id) + 0x144]`; `frame` is forced to 0 when `WeaponPanelIcons__weapon_disabled` (0x5679C0) is true | r2, 0x567E7C–0x567EAB |
| Tile draw blits a **24×24** sub-rect at src (4,4) of that image (`call 0x4F6910`) into a cached panel surface at `[world+0x138]` | r2, 0x567EA2–0x567EC7 |
| Function map: constructor 0x567770, draw_cursor 0x567AA0, draw_text 0x567F80, redraw_panel 0x568220, cursor2pos 0x569130 | openwa `re/WeaponPanelIcons.toml` |
| The panel object is `[GameWorld+0x548]` | caller of 0x5697B0 at 0x5540CD; the router's dirty writes load the same pointer (0x556F49) |
| `redraw_panel` has exactly one caller, 0x569811 in `WeaponPanelIcons__sub_5697B0`, and runs only while `[panel+0x1CC] != 0`, clearing it after | objdump of 3.8.1 |
| The dirty flag is set by `GameMessageRouter` (ten sites, 0x556F56..0x558E94) and `GameRuntime__AdvanceWormFrame` (0x55DDC7) | objdump |
| `TeamArena__AddAmmo` (0x522640) is a bare write into the ammo table: it does not touch the panel | objdump |
| The table at `+0x144` **owns** its images: teardown deletes every slot's pair. Two slots sharing one image is a double free, a crash at match end | PR #13 (the `panel_icon` crash), 5/5 before the fix, 0/5 after |

### The panel only rebuilds when told to

WA rebuilds the panel surface only on its dirty flag, and only its own message
paths set it. WormForge's ammo writes (`worm:ammo`, `inventory.apply`, rule
packs like highlander) go straight through `TeamArena__AddAmmo`, which does
not set it -- so after them the panel shows stale contents until some stock
event happens to dirty it. That is PX's `SetWeaponIcon`-to-itself workaround
again, and the fix is its native equivalent: set `[[world+0x548]+0x1CC] = 1`
after every WormForge ammo write. It is display state, deterministic like the
write itself. This is a fix for today's ammo verbs, independent of custom
icons, and should land first.

## Approach

For each claim that names a PNG icon, at match setup, where `copy_panel_icon`
runs today:

1. **Clone a stock icon image**, the one belonging to the slot being replaced.
   It has the right class, size and palette context. Only its pixels are
   replaced, so we never build a WA image from scratch.
2. **Quantize the PNG** to the palette indices the stock icons already use,
   collected from all 71×2 stock images. The icon then uses only colours
   already reserved for the panel. It takes no palette entries, and it can't
   collide with terrain or custom sprites.
3. **Make the disabled frame** with an index-to-index map *learned* from the
   stock pairs: for every stock icon, which index in the enabled frame becomes
   which index in the disabled frame. A custom icon then greys out exactly the
   way stock ones do.
4. **Write both pointers** into `GameWorld+0x144` for the slot.
5. **Put the stock pointers back** at teardown, before WA frees anything. The
   clones belong to us and are reused across matches.

Presentation only: nothing here reaches the simulation. The W0 sync gate
confirms it with one run (the `rng` column is unchanged).

## Stages

### S0 — RE spike (gate; no product code)

Answer these in `re/WeaponPanelIcons.toml` (ported from openwa and extended)
and in `docs/weapon-panel.md`:

1. What the image object at `+0x144` is: vtable/class, struct size, pixel
   buffer and stride fields, and pixel format (expected 8bpp indexed). Start
   from the constructor 0x567770 (calls 0x53EA30 and 0x541340) and the blit
   0x4F6910.
2. The full image size (expected 32×32 with the 24×24 visible window at
   (4,4)), and which of the two frames is the enabled one and which the
   disabled one.
3. ~~Ownership~~ **Answered:** teardown frees every slot's images (PR #13).
   Clones must be restored out of `+0x144` before `GameEngine::Shutdown`, as
   `restore_panel_icons` does for copied stock icons.
4. ~~Rebuild trigger~~ **Answered:** only on the dirty flag at
   `[panel+0x1CC]`, so the pointers may be written any time before the next
   dirtying event; setting the flag after writing them makes it immediate.

**Gate:** a throwaway hook inverts the pixels of one *cloned* stock icon and
installs it. It must be visible in the panel, greyed correctly while disabled,
and survive two matches in a row plus a quit to the menu.

### S1 — Pixel pipeline (host-testable)

`crates/gfx/src/icon.rs` (new; `gfx` already decodes PNG strips and runs on
darwin):

- `quantize(rgba, w, h, allowed: &[Rgb; N]) -> Vec<u8>`: nearest colour in the
  allowed set, alpha below a threshold → the transparent index. No dithering;
  the icons are pixel art.
- `learn_disabled_map(pairs) -> [u8; 256]`: most frequent enabled→disabled
  index per source index, falling back to the nearest learned neighbour.
- A size rule. The image must fit the visible window from S0: anything smaller
  is centred, anything larger is refused with a message naming the expected
  size.
- Tests for exact-colour round-trip, transparency, centering, refusal, and the
  learned map on synthetic pairs.

### S2 — Install, API, example

- `crates/weapons/src/overlay.rs:337`: `panel_icon: Option<StockWeapon>` →
  `Option<PanelIcon>`, where `PanelIcon` is `Stock(StockWeapon)` or
  `File(String)`.
- `crates/lua-host/src/api.rs:340`: a string ending in `.png` is a pack file,
  resolved like sprite paths (`resolve_sprite_name`, `spawn.rs`, against
  `wa._mod`). Anything else goes through `parse_panel_icon` as today.
  `panel_icon = "banana"` keeps working.
- `crates/engine/src/panel_icon.rs` (new): collect the stock palette subset and
  the disabled map once per match, clone, quantize, install, and keep
  restore-on-teardown. The `Stock` case keeps using the existing
  `copy_panel_icon` code.
- `crates/engine/src/apply.rs:247`: call the new module in place of
  `copy_panel_icon`, and log per claim, e.g. `example.x panel icon
  sprites/icon.png (quantized, 23 colours)`.
- `crates/game/src/offsets.rs`: add the image-object fields from S0.
- New example `mods/examples/panel_icon/`, a single weapon with `icon =
  "sprites/icon.png"`.

### S3 — Docs and preview

- `docs/weapon-panel.md`: how to author an icon, the visible window size, the
  palette limitation (and why: native look, no palette cost), and what greys
  it out.
- A preview dump: with the existing marker-file pattern, write each installed
  icon, both frames re-expanded to RGBA, to the game's `WormForge` dir, so
  quantization can be checked without opening the panel.

## Critical files

- `crates/engine/src/apply.rs` (`copy_panel_icon`, claim loop)
- `crates/weapons/src/overlay.rs`, `crates/weapons/src/stock.rs`
  (`parse_panel_icon`)
- `crates/lua-host/src/api.rs` (param parsing), `crates/lua-host/src/spawn.rs`
  (path resolution)
- `crates/gfx/src/icon.rs` (new), `crates/engine/src/panel_icon.rs` (new)
- `crates/game/src/offsets.rs`, `re/WeaponPanelIcons.toml`

## Risks

- **A small palette subset.** If the stock icons use few distinct indices,
  colourful icons will band. The S3 preview shows this before anyone opens the
  panel. A GPU truecolor overlay is the upgrade path, deliberately deferred.
- **Teardown ownership (S0.3).** Getting it wrong is a crash at match end, so
  S0 has to settle it before S2 writes a single pointer.
- **Row 0 and row 5 exceed 5 entries.** Placement is inherited from the stock
  slot and never chosen, so we add no overflow of our own. Stock overflow
  behaviour is not our concern.
- **`world.rs:1016` `enable_slot` writes `panel_state = 1`** (moves an
  equipped weapon into the F1 row). That code is unrelated but surprising;
  note it rather than change it.

## Verification

- `cargo test -p gfx --target aarch64-apple-darwin` for the S1 tests.
- `./tools/build-win32.sh`, then `python3 tools/install-local.py`, then install
  `mods/examples/panel_icon` into the live `Mods/`.
- In game:
  - the icon shows at the replaced slot's position
  - it greys out under a scheme delay exactly like its neighbours
  - the cursor, the hover text and F-key cycling are unchanged
- Two matches back to back, then disable the pack in the loader and start a
  third: the stock icon is back and nothing crashed at any teardown.
- Replay playback of a match that used the icon: same icon, no crash.
- The W0 sync gate with the pack on vs off: the `rng` column is identical.
