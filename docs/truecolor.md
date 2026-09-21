# Truecolor sprites on the fork

WormForge's optional RGBA compositor runs inside `wkLua.dll` on WA 3.8.1's
**Direct3D 9 shader** renderer. It keeps PNG/GIF colors and partial alpha, without
allocating or changing shared map palette entries. Software and other renderer
paths retain the stock palette approximation. Gameplay and lockstep are unchanged.

Opt sheets into it in a pack:

```lua
params = {
  truecolor_prefix = "sprites/turret_",
  depth = 0x190000,
  depth_prefix = "sprites/turret_",
}
```

Prefixes match pack-relative sprite paths. An empty truecolor prefix opts in all
custom sheets in that claim; omitted means palette rendering. The sentry enables
only turret sheets. Worm equip/put-away animations keep their usual foreground
placement. Depth is independent of the display's palette layer.

## Ordering and composition

Measured queue depths in this installation, drawn from back to front:

| Draw | Depth |
|---|---:|
| Soil | `0x1b0000` |
| Sentry | `0x190000` |
| Solid terrain | `0x180000` |
| Worms, names, HUD | Smaller values |

Depth overrides now run at `RQ_RenderDrawingQueue` (0x542350), after all queue
producers, instead of inside WorldRoot's render message. Queue pointers are
bounded to the command arena; the previous speculative inline/pointer walk is
gone. Types 4/5 carry the sprite word at index 4; type 6 carries it at index 7.
The sorter and comparator are WA's own, including the ordering of equal depths.

For opted-in sprites, the compositor builds two companion textures:

- Premultiplied BGRA sprite color, composited in queue order.
- An indexed underlay, rendered by WA into scratch memory with those sprites
  omitted, so partial alpha blends against the scene rather than its own palette
  approximation.

Stock draws nearer than each custom sprite are replayed over two scratch clears
(`0x55`, `0xaa`). Every pixel written on either clear removes the underlying RGBA
coverage. This detects opaque writes even when their palette index matches one
clear value. Terrain, worms and queued HUD therefore occlude the actual sprite
silhouette. Temporary surface fields, render lock, queue entry pointers, counts
and depths are restored before the real dispatch.

The real stock frame is always drawn in full. At present, a small offline-compiled
`ps_2_0` shader substitutes RGBA only where coverage survives. Texture upload,
shader binding or backend failures leave a complete stock frame. Device state is
captured/restored around the final draw, and default-pool resources are released
before WA's cleanup/reset. Each submitted frame is consumed once.

## Scope and limits

This first implementation supports unscaled queue sprite types 4/5, including
horizontal mirroring and stock animation phase selection. Unknown transform flags
and offset/scaled sprite commands remain on the stock path. Targets must be 8bpp,
no larger than 4096x4096 and a multiple of four pixels wide. Callback-containing
queues fall back rather than replaying arbitrary code. Scratch replay adds CPU
work, particularly when many depth groups alternate.

Later queued HUD draws are merged through the same coverage path. For draws
outside the queue, changed indexed pixels clear RGBA coverage before present;
an out-of-queue draw that writes exactly the same palette index cannot be detected
by this check. ESC/chat transitions therefore need live checking. Concurrent
wkFOW/wkZoom/other final-shader ownership is not claimed: installation checks the
hook sites and keeps the palette path if an existing detour occupies them.

The sentry baker now preserves source shading and alpha by default. The earlier
posterized art can still be reproduced with `--palette-art`. The 30-frame chunks
remain because the native sprite loader still builds the palette fallback.

## Verification and reproduction

Portable compositor tests cover premultiplied alpha, opaque black, low alpha,
mirroring/clipping, animation, overlapping sprites and foreground coverage whose
color matches a sentinel. The sentry tests cover its current chunked sheets and
placement/activation behavior.

`bash tools/test-truecolor-win32.sh` builds a standalone D3D9 test executable.
Run `target/truecolor-test/test.exe` on Windows or in a separate Wine test prefix.
It checks actual GPU readback for saturated color, partial alpha over the correct
underlay, opaque black, unaffected stock pixels, restored device state and resource
release/recreation across a device reset. It does not launch WA.

`bash tools/build-win32.sh --offline` builds the DLL. After installation, restart
WA, select its Direct3D 9 shader renderer, then test a sentry on a soil-backed edge:
terrain must hide the sunk feet while the exposed turret covers soil. Check blue
and cyan teams, firing muzzle alpha, walking worms, ESC/chat, resolution changes,
and a second match. `wkLua.log` should contain `render depth: sprite ... -> 0x190000`
and `truecolor active: RGBA sprites with stock queue occlusion; palette unchanged`.
A fallback logs a reason. Live game integration still requires this play test.

The HLSL source and compiled header are both checked in. Regenerate the header with:

```text
python3 tools/compile-truecolor-shader.py --fxc /path/to/fxc.exe
```

There is no runtime shader compiler dependency. The build verifies that the header
names the current HLSL source hash, to avoid shipping stale shader bytecode.

## Evidence

- Local WA.exe disassembly: queue dispatch/sort at `0x542350`, comparator
  `0x542330`, sorter `0x5d7890`, local sprite producer `0x542060`, shader draw
  at `0x59cf9d`, present `0x59cde0`, resource cleanup `0x59cd70`.
- Local wkFOW `FogRender.cpp`, `BitGrid.h`, `ShaderRenderer.cpp` and
  `RenderQueue.h`: ABI/layout and the scratch coverage technique. No wkFOW or
  openwa implementation files are vendored.
- Local PX `DirectX2D.dll`: imports `DirectDrawCreate`, exports `reBitmap*` and
  `reDisplay*`. This establishes a legacy rendering interface, not proof that
  this DLL alone implements PX's truecolor path. It is not loaded by WormForge.
- Microsoft: [dynamic texture locking](https://learn.microsoft.com/en-us/windows/win32/api/d3d9/nf-d3d9-idirect3dtexture9-lockrect)
  and [resource requirements before Reset](https://learn.microsoft.com/en-us/windows/win32/api/d3d9/nf-d3d9-idirect3ddevice9-reset).
