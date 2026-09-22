# Project X script dumps

Read-only reference. Nothing here is built, compiled, or shipped by WormForge —
it exists so the PX script corpus is greppable from the editor alongside our own
code, instead of living in an out-of-tree extraction folder.

## What this is

The `scripts` section of every `.pxl` library in **WA: Project X, build by 0rang3,
upd. 21.05.2026** (`Libs/`, 628 libraries), extracted with `pxl_extract.py` and
laid out one folder per library:

    docs/px-scripts/<library>/<script>.txt

560 libraries carry scripts; 1048 files in total. The language is PX's C++-like
scripting dialect (`override void CTurnGame::Message(...)`, `GetAttachment`,
`LoadSprite`, …), which is why these read like source but are only data to us.

## What this is not

- **Not the whole `.pxl`.** Each library also holds `files` (PNG/WAV/BMP assets)
  and `weapons` (opaque binary weapon blocks). Neither is text, so neither is
  here. The full extraction — assets, weapon blobs, and the rendered sprite
  gallery — stays out of tree.
- **Not ours, and not a dependency.** This is third-party work by the Project X
  authors, vendored purely as searchable reference. Do not copy code out of it
  into packs or crates; use it to understand what PX does, then write our own.

## Refreshing

Re-extract from a newer PX build and copy the `scripts` subfolders across:

    pxl_extract.py "<PX build>/Libs" <out-dir>
    for d in <out-dir>/*/scripts; do ... ; done

Files keep their original CRLF line endings — `.gitattributes` sets `* -text` so
git never rewrites them, and the tree stays byte-identical to what the archives
contain.
