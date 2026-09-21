// COM boundary for the GPU sprite renderer. Deliberately thin: geometry,
// atlas packing and mask building all live in Rust where they are testable
// off-Windows. This file owns only what must speak to IDirect3DDevice9.
//
// Everything here was proven against WA's exact device parameters by the S0
// probe first (crates/render/probe, docs/gpu-renderer-s0.md). Two findings
// from it are load-bearing and must not be quietly undone:
//
//   * Sprites are drawn through ONE dynamic vertex buffer with a single
//     DrawPrimitive. Per-sprite DrawPrimitiveUP measured 296x slower under
//     this machine's translation layer -- 45.6 ms vs 0.15 ms for 500 quads.
//   * The atlas is D3DPOOL_MANAGED so it survives a device Reset untouched;
//     only the mask and the vertex buffer are DEFAULT and must be released
//     from WA's cleanup hook before it resets the shared device.
//
// The device is borrowed from WA and shared, so every entry point captures a
// state block and restores it before returning: WA's own fullscreen draw runs
// immediately after ours and must see the device exactly as it left it.
#include <windows.h>
#include <d3d9.h>
#include <cstring>
#include <cstdint>

#include "../shaders/sprite.h"

namespace {

// Borrowed, never owned: WA creates and destroys this device.
IDirect3DDevice9* device = nullptr;

IDirect3DPixelShader9* shader = nullptr;
IDirect3DTexture9* atlas = nullptr;          // MANAGED, survives Reset
IDirect3DTexture9* mask = nullptr;           // DEFAULT|DYNAMIC
IDirect3DVertexBuffer9* vertices = nullptr;  // DEFAULT|DYNAMIC
IDirect3DStateBlock9* saved = nullptr;

UINT atlas_w = 0, atlas_h = 0;
UINT mask_w = 0, mask_h = 0;
UINT vertex_capacity = 0;                    // in vertices, not bytes

struct Vertex {
    float x, y, z, rhw;
    DWORD colour;
    float u, v;      // atlas
    float su, sv;    // screen space, samples the coverage mask
};
const DWORD FVF = D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX2;

// WA's surface never exceeds this; refuse anything implausible rather than
// trusting a size that came from a struct read.
const UINT MAX_DIMENSION = 4096;

void release_default_pool() {
    if (mask) { mask->Release(); mask = nullptr; mask_w = mask_h = 0; }
    if (vertices) { vertices->Release(); vertices = nullptr; vertex_capacity = 0; }
}

// Upload tightly-packed rows into a locked texture, honouring its pitch.
bool upload(IDirect3DTexture9* texture, const uint8_t* src, UINT row_bytes,
            UINT rows, DWORD flags) {
    D3DLOCKED_RECT lock = {};
    if (FAILED(texture->LockRect(0, &lock, nullptr, flags))) return false;
    bool ok = lock.Pitch >= (int)row_bytes;
    if (ok) {
        for (UINT y = 0; y < rows; y++)
            std::memcpy((uint8_t*)lock.pBits + (size_t)y * lock.Pitch,
                        src + (size_t)y * row_bytes, row_bytes);
    }
    return SUCCEEDED(texture->UnlockRect(0)) && ok;
}

bool ensure_vertex_buffer(UINT count) {
    if (vertices && vertex_capacity >= count) return true;
    if (vertices) { vertices->Release(); vertices = nullptr; }
    // Grow in steps so a frame that creeps upward does not reallocate every
    // time; 1024 vertices is 170 quads.
    UINT capacity = 1024;
    while (capacity < count) capacity *= 2;
    if (FAILED(device->CreateVertexBuffer(capacity * sizeof(Vertex),
            D3DUSAGE_DYNAMIC | D3DUSAGE_WRITEONLY, FVF, D3DPOOL_DEFAULT,
            &vertices, nullptr))) {
        vertex_capacity = 0;
        return false;
    }
    vertex_capacity = capacity;
    return true;
}

} // namespace

extern "C" {

// Bind to WA's device. Safe to call repeatedly; re-binding a different device
// discards everything built against the old one.
int wf_gpu_init(void* raw_device) {
    auto* dev = (IDirect3DDevice9*)raw_device;
    if (!dev) return 0;
    if (dev != device) {
        // A different device means every resource we hold is stale.
        release_default_pool();
        if (atlas) { atlas->Release(); atlas = nullptr; atlas_w = atlas_h = 0; }
        if (shader) { shader->Release(); shader = nullptr; }
        device = dev;
    }
    if (dev->TestCooperativeLevel() != D3D_OK) return 0;
    if (!shader && FAILED(dev->CreatePixelShader(
            (const DWORD*)WF_SPRITE_SHADER, &shader))) {
        return 0;
    }
    return 1;
}

// Called from WA's D3D9ShaderResourcesCleanup, which runs before every device
// Reset. DEFAULT-pool resources must be gone by then or the Reset fails; the
// MANAGED atlas is deliberately kept so sheets are not re-uploaded on a
// resolution change.
void wf_gpu_on_device_lost() {
    if (saved) { saved->Release(); saved = nullptr; }
    release_default_pool();
}

void wf_gpu_release() {
    wf_gpu_on_device_lost();
    if (atlas) { atlas->Release(); atlas = nullptr; atlas_w = atlas_h = 0; }
    if (shader) { shader->Release(); shader = nullptr; }
    device = nullptr;
}

// Replace the whole atlas. Rust packs every opted-in sheet into one BGRA
// image so the entire frame is a single draw call with no texture changes.
int wf_gpu_atlas(uint32_t w, uint32_t h, const uint8_t* bgra) {
    if (!device || !bgra || !w || !h || w > MAX_DIMENSION || h > MAX_DIMENSION)
        return 0;
    if (atlas && (atlas_w != w || atlas_h != h)) {
        atlas->Release(); atlas = nullptr; atlas_w = atlas_h = 0;
    }
    if (!atlas) {
        if (FAILED(device->CreateTexture(w, h, 1, 0, D3DFMT_A8R8G8B8,
                D3DPOOL_MANAGED, &atlas, nullptr)))
            return 0;
        atlas_w = w; atlas_h = h;
    }
    // MANAGED cannot be DISCARD-locked; a plain lock is correct here and this
    // runs once per match, not per frame.
    if (!upload(atlas, bgra, w * 4, h, 0)) {
        atlas->Release(); atlas = nullptr; atlas_w = atlas_h = 0;
        return 0;
    }
    return 1;
}

// Screen-space coverage: 0 where a nearer stock draw owns the pixel, 255
// where the sprite may paint. One byte per pixel, full surface.
int wf_gpu_mask(uint32_t w, uint32_t h, const uint8_t* coverage) {
    if (!device || !coverage || !w || !h || w > MAX_DIMENSION || h > MAX_DIMENSION)
        return 0;
    if (mask && (mask_w != w || mask_h != h)) {
        mask->Release(); mask = nullptr; mask_w = mask_h = 0;
    }
    if (!mask) {
        if (FAILED(device->CreateTexture(w, h, 1, D3DUSAGE_DYNAMIC, D3DFMT_L8,
                D3DPOOL_DEFAULT, &mask, nullptr)))
            return 0;
        mask_w = w; mask_h = h;
    }
    // DISCARD on a DEFAULT|DYNAMIC texture is the fast path; a sub-rect lock
    // without it is a read-modify-write and stalls under wined3d.
    if (!upload(mask, coverage, w, h, D3DLOCK_DISCARD)) return 0;
    return 1;
}

// Draw every sprite for this frame in one call. `count` is a vertex count and
// must be a multiple of 3 (TRIANGLELIST); Rust has already transformed,
// tinted and triangulated each quad.
int wf_gpu_draw(const void* verts, uint32_t count) {
    if (!device || !verts || count < 3 || count % 3) return 0;
    if (!shader || !atlas || !mask) return 0;
    if (device->TestCooperativeLevel() != D3D_OK) return 0;
    if (!ensure_vertex_buffer(count)) return 0;

    void* dst = nullptr;
    if (FAILED(vertices->Lock(0, count * sizeof(Vertex), &dst, D3DLOCK_DISCARD)))
        return 0;
    std::memcpy(dst, verts, (size_t)count * sizeof(Vertex));
    if (FAILED(vertices->Unlock())) return 0;

    // WA's device is shared and its own draw follows immediately, so capture
    // everything we are about to touch and put it back before returning.
    //
    // The block is created ONCE and re-captured per call, not allocated each
    // time. D3DSBT_ALL is an expensive object under wined3d, and with two
    // occlusion bands this runs twice a frame -- allocating and destroying it
    // at that rate is both wasteful and a slow-burn way to exhaust driver
    // resources.
    if (!saved && FAILED(device->CreateStateBlock(D3DSBT_ALL, &saved))) {
        saved = nullptr;
        return 0;
    }
    if (FAILED(saved->Capture())) {
        saved->Release(); saved = nullptr;
        return 0;
    }

    int ok = 0;
    // Our hook sits between WA's EndScene and its Present, so this is a
    // second, balanced scene. The S0 probe proved its content survives.
    if (SUCCEEDED(device->BeginScene())) {
        device->SetRenderState(D3DRS_ZENABLE, FALSE);
        device->SetRenderState(D3DRS_LIGHTING, FALSE);
        device->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
        device->SetRenderState(D3DRS_ALPHABLENDENABLE, TRUE);
        device->SetRenderState(D3DRS_ALPHATESTENABLE, FALSE);
        device->SetRenderState(D3DRS_FOGENABLE, FALSE);
        device->SetRenderState(D3DRS_STENCILENABLE, FALSE);
        device->SetRenderState(D3DRS_SCISSORTESTENABLE, FALSE);
        device->SetRenderState(D3DRS_COLORWRITEENABLE,
            D3DCOLORWRITEENABLE_RED | D3DCOLORWRITEENABLE_GREEN |
            D3DCOLORWRITEENABLE_BLUE | D3DCOLORWRITEENABLE_ALPHA);
        // Premultiplied: an opaque sprite and an additive one (alpha 0)
        // share this single state, so both can sit in one batch.
        device->SetRenderState(D3DRS_SRCBLEND, D3DBLEND_ONE);
        device->SetRenderState(D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
        for (DWORD s = 0; s < 2; s++) {
            device->SetSamplerState(s, D3DSAMP_MINFILTER, D3DTEXF_POINT);
            device->SetSamplerState(s, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
            device->SetSamplerState(s, D3DSAMP_MIPFILTER, D3DTEXF_NONE);
            device->SetSamplerState(s, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
            device->SetSamplerState(s, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);
        }
        device->SetTexture(0, atlas);
        device->SetTexture(1, mask);
        device->SetFVF(FVF);
        device->SetVertexShader(nullptr);
        device->SetPixelShader(shader);
        device->SetStreamSource(0, vertices, 0, sizeof(Vertex));
        ok = SUCCEEDED(device->DrawPrimitive(D3DPT_TRIANGLELIST, 0, count / 3));
        device->EndScene();
    }

    // Kept for the next call; released only on device loss, where every
    // DEFAULT-pool object must go before WA resets the shared device.
    saved->Apply();
    return ok;
}

} // extern "C"
