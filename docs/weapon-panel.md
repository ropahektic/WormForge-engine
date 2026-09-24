# Weapon panel icons

A weapon a pack replaces can show its own icon in WA's stock weapon panel:

```lua
wa.weapons.replace({
  weapon = "holy_grenade",
  name = "Hellish Hand Grenade",
  panel_icon = "sprites/hhh_ico.png",   -- a pack PNG
})
```

`panel_icon = "banana"` (a stock weapon's name) still borrows that weapon's
icon. Everything else about the panel stays native: layout, cursor, hover
text, F-key rows, selection. Example pack: `mods/examples/panel_icon`.

## Authoring an icon

- **Size.** The panel shows a **24×24** window of each icon. Give either
  - a **32×32** image, WA's full icon size -- the panel shows its centre
    (pixels 4..27), exactly as with WA's own icons. **Every Project X weapon
    icon is 32×32 and works as is**; or
  - anything up to 24×24, centred in the window.

  Other sizes are refused with a message naming both.
- **Transparency.** Pixels with alpha below half are transparent.
- **Colours.** The icon is matched, pixel by pixel, to the nearest of the
  colours WA's own icons use (76 on 3.8.1). It takes no palette entries and
  cannot clash with terrain or sprites, but a very colourful icon will band.
  Pixel art in a palette close to WA's looks native.
- **Greyed out.** The disabled frame (no ammo, scheme delay) is derived
  automatically, learned from how WA's own icons grey out, so a custom icon
  dims exactly like its neighbours.
- **Copy PX icons, never re-save them.** A byte-identical PX file is stored in
  replays as a 32-byte reference to the `.WFAssets` collection
  ([`wfassets.md`](wfassets.md)); a re-saved one is embedded in every replay.

## Checking the result without the panel

Put an empty `panel_icons.txt` next to `wkLua.log` and start a match. Each
installed icon's frames are written there as `panel-icon-<slot>-hi.png`
(enabled) and `-lo.png` (greyed), at 4× size. The log also says:

```
panel icons: stock images 32x32, 68 icons, 76 colours to quantize to
example.panel_icon panel icon sprites/hhh_ico.png (quantized, 33 colours)
```

## How it works (3.8.1)

| | |
|---|---|
| Icon table | `GameWorld+0x144`: per weapon id, `+0x144 + 8·id` is `lo\<name>.img` (greyed), `+0x148 + 8·id` is `hi\<name>.img`, loaded by `GameWorld__LoadHudAndWeaponSprites` (0x53D0E0) |
| Images | 8bpp BitGrids, 32×32, decoded against display layer 1's PaletteContext |
| Tile draw | `BitGrid__BlitSpriteRect` (0x4F6910) copies 24×24 at (4,4), flags 0: index 0 transparent; the `lo` frame is used while `WeaponPanelIcons__weapon_disabled` (0x5679C0) holds |
| Rebuild | only on the dirty flag `[[GameWorld+0x548]+0x1CC]`; WormForge sets it after every ammo write |
| Ownership | the table owns its images: WA's teardown deletes every slot's pair |

A custom icon is a clone of the replaced slot's two BitGrids -- WA's
0x2C-byte header, WormForge's own pixel buffer -- written into the table at
match setup (`crates/engine/src/panel_icon.rs`). The stock pointers go back
before `GameEngine::Shutdown`, so WA never deletes WormForge's memory and
still deletes its own icons exactly once. The pixel pipeline is
`crates/gfx/src/icon.rs`.

Project X does the same thing, minus its extra pages: its `SetWeaponIcon`
writes the image pointer into this table and sets the same dirty flag
(`docs/weapon-panel-plan.md` has the RE).
