# S0 — GPU renderer viability probe

The gate on the whole GPU-renderer fork: can this machine's D3D9 host a sprite
renderer at all, under the runtime WA actually uses? Answered before any WA
integration, by a standalone exe that never loads the game.

Run it:

```sh
crates/render/probe/compile-shader.py      # regenerate shaders/sprite.h
crates/render/probe/build.sh
crates/render/probe/run.sh                 # private throwaway prefix

# the decisive one: WA's own Wine, WA's own prefix
WFGPU_WINE="/Users/eule/Applications/Worms Armageddon.app/Contents/SharedSupport/wine/bin/wine32on64" \
WFGPU_PREFIX="/Users/eule/Applications/Worms Armageddon.app/Contents/SharedSupport/prefix" \
  crates/render/probe/run.sh
```

The game prefix is 32-bit and Homebrew's Wine is WoW64, so it refuses that
prefix; the bundle's `wine32on64` is the loader that runs there. (The bundle's
plain `wine` is an i386 Mach-O binary that modern macOS cannot execute at all.)

## Result: PASS, in the live game prefix

```
caps: pixelShader=3.0 maxTexture=16384x16384

  ok   rotated 37deg quad, tint, and screen-space mask
  ok   second BeginScene/EndScene before Present is preserved
  ok   additive via premultiplied alpha (a=0) saturates
  ok   state block restores states, FVF, shader and textures
  ok   MANAGED atlas survives Reset; DEFAULT released/recreated
  ..    40 quads/frame: DrawPrimitiveUP   3.78 ms   batched VB   0.09 ms  (40.3x)
  ..   500 quads/frame: DrawPrimitiveUP  45.62 ms   batched VB   0.15 ms  (296.3x)
  ..   WA's own frame budget at 50 fps is 20.00 ms
```

## What each result licenses

**The second scene survives.** WA's present method (`0x59CDE0`) calls
`BeginScene`/`DrawPrimitive`/`EndScene` at `0x59C7A0` and Presents separately at
`0x59C8A0`, so `after_shader_present` sits *between* `EndScene` and `Present`.
Drawing there means opening a second scene, and its content reaching the
front buffer is the premise of the entire design. It does. The fallback
in-scene hook at `0x59C86A` is not needed.

**Rotation, scale, tint and screen-space masking all work in one `ps_2_0`
pass.** A 37° quad lands on the pixels computed on the CPU, the diagonal
extends past the unrotated edge and stops where predicted, and the mask gates
a second identical quad out of existence. That is the sentry gun's 120 baked
rotations replaced by one sheet and a float.

**Additive is free.** `rgb=c, a=0` under `SRCBLEND=ONE, DESTBLEND=INVSRCALPHA`
saturates correctly against a grey ground while leaving the ground visible
elsewhere. Opaque and additive sprites therefore share one pipeline state and
can be mixed inside a single batch — no state change, no second draw call.
`bake_sentry.py`'s `glow_alpha()` luminance→alpha hack can go.

**`D3DPOOL_MANAGED` survives `Reset`.** Atlases do not need re-uploading when
the resolution changes (wkZoom does this routinely; this machine's wkFOW logs
show 640×480 ⇄ 1280×800 flips). `DEFAULT`-pool resources must still be
released in the cleanup hook first, which is the existing contract.

**Batching is not an optimisation, it is the design.** 500 quads cost 45.6 ms
as individual `DrawPrimitiveUP` calls and 0.15 ms through one dynamic vertex
buffer — **296× faster**, 0.75% of a 20 ms frame. The first version of this
probe drew per-quad and reported "22.6 fps", which read as a kill signal and
was entirely an artefact of the wrong draw path. The renderer must fill one
`D3DUSAGE_DYNAMIC` VB per frame with `D3DLOCK_DISCARD` and issue a single
`DrawPrimitive`; anything per-sprite is off by two orders of magnitude.

## Corrections to earlier assumptions

- **A state block *does* restore the pixel shader and textures here.** The
  first run reported otherwise, but the test was wrong, not wined3d: it
  asserted a restore back to *no shader bound*, which is not the case the
  renderer is in. Restoring to a previously-bound shader — what actually
  happens when we borrow WA's device mid-frame — works. Kept as a real
  assertion rather than deleted, since silently corrupting WA's own state is
  the failure mode that would be hardest to diagnose later.
- **Fixed-function is not the safer path under Wine.** PX used it because it
  targeted 2003 hardware. This machine reports `pixelShader=3.0`, WA itself
  runs `ps_2_0` on this exact path, and wined3d implements fixed-function by
  generating shaders anyway. Pretransformed `XYZRHW` vertices plus a short
  `ps_2_0` is the conservative choice.
- Shaders are compiled offline to bytecode and embedded, never compiled at
  runtime — wkFOW hit Wine's HLSL compiler being unreliable and documents it
  in `src/ShaderRenderer.cpp:166`. `compile-shader.py` stamps an FNV-1a of the
  source into the header so an edited `.hlsl` that was never recompiled fails
  the build instead of shipping stale bytecode.

## Not answered here

S0 covers the GPU primitives only. Still open, and belonging to S1/S2:

- whether the split-dispatch mask can be restricted to sprite bounding rects
  cheaply enough to avoid the full-frame memcpy
- how often queue **kind-3 (callback)** commands appear — a one-line histogram
  in `dispatch`, and the gate on whether a world renderer is possible at all
- coexistence with wkFOW/wkZoom when both want the same hook sites

---

# S1 (renderer half) — the production shim is correct

S0 proved the primitives with inline test code. This proves the code that will
actually run inside WA: `crates/render/probe/gpu_render_probe.cpp` links
`native/wf_gpu.cpp` itself and drives its real entry points. Still no WA, no
hooks, no game state — so a failure here is a renderer bug and nothing else.

```sh
crates/render/probe/build.sh
WFGPU_PREFIX="/Users/eule/Applications/Worms Armageddon.app/Contents/SharedSupport/prefix" \
  crates/render/probe/run.sh s1
```

## Result: PASS, in the live game prefix

```
  ok   wf_gpu_init/atlas/mask/draw renders a masked rotated batch
  ok   tint modulates and additive saturates in one batch
  ok   invalid input is refused, not drawn
  ok   device state is handed back exactly as found
  ok   cleanup + Reset keeps the MANAGED atlas usable
  ..    40 sprites/frame incl. full-surface mask: 0.22 ms
  ..   500 sprites/frame incl. full-surface mask: 0.23 ms
```

Geometry and atlas packing are verified separately by `cargo test -p render`
(13 tests, host-runnable) rather than here: the arithmetic is cheap to test
where a failure prints a number, and only COM behaviour needs a device.

## A finding that contradicts the plan

The plan called the dirty-rect mask "the single largest perf lever in the
design; build it in from the start, not later." **That is wrong on this
machine.** 40 sprites and 500 sprites both cost ~0.22 ms *including* a
full-surface 256x128 mask upload every frame — the cost is fixed per-frame
work (the state block and the upload), not sprite count, and the whole thing
is ~1% of a 20 ms budget.

So the dirty-rect optimisation is **deferred, not dropped**. It should be
reconsidered only if a real 640x480 or 1280x800 surface changes the picture —
this probe runs at 256x128, so the mask upload here is 32 KB against 1 MB at
1280x800. Re-measure at full size before assuming either way. What this does
establish is that per-sprite cost is not the constraint, so nothing about the
draw path needs redesigning for throughput.

## What is proven, and what S1 still needs

Proven: the renderer draws correctly, batches, tints, blends additively,
refuses bad input rather than drawing something wrong, hands WA's device back
untouched, and survives a device reset with its atlas intact.

Not yet touched — the WA-side half of S1:

- the `draw_sprite_local` intercept in `crates/engine/src/actor.rs`
- building the coverage mask from the split-dispatch snapshots
- `gpu::install` / the `after_shader_present` call site
- deciding `gpu_active` once per match, never per frame

Also still open and worth an hour before any M2 decision: a histogram of
**kind-3 (callback) queue commands**, which `truecolor.rs` already bails the
whole frame on. If they appear in most frames, a world renderer is impossible
regardless of how well the sprite renderer works.
