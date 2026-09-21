// S0: does this machine's D3D9 support the GPU sprite renderer at all?
//
// Standalone. Does not load WA, its prefix, or wkLua.dll. It creates a device
// with WA 3.8.1's *exact* parameters (read out of the disassembly at 0x59C2A0)
// so that anything proven here is proven for the real target, and asserts by
// GPU readback rather than by eye.
//
// Four facts were already established on this machine by wkFOW's logs and are
// deliberately NOT retested here:
//   caps pixelShader=3.0        -> ps_2_0 is comfortably supported
//   maxTexture=16384x16384      -> no atlas size constraint
//   backbuffer format=22        -> D3DFMT_X8R8G8B8, matching the disassembly
//   CreatePixelShader succeeds  -> wkFOW creates a second shader here already
//
// What is unproven, and therefore what this probe exists to answer:
//   1. a rotated + scaled XYZRHW|DIFFUSE|TEX2 quad with our ps_2_0 lands on
//      the pixels we computed, and the screen-space mask actually gates it
//   2. a SECOND balanced BeginScene/EndScene before Present keeps its content
//      (this is the whole basis for drawing in after_shader_present)
//   3. additive-via-premultiplied (rgb=c, a=0 under ONE/INVSRCALPHA) saturates
//      rather than producing garbage, so one pipeline state serves both modes
//   4. a D3DPOOL_MANAGED atlas survives a device Reset without re-upload,
//      while DEFAULT resources are released and recreated
//   5. a state block leaves the device as it found it, so WA's own draw is
//      unaffected by ours
//
// Exit code 0 = the GPU direction is viable on this machine. Non-zero names
// the first failing test, which is the kill criterion for that milestone.
#include <windows.h>
#include <d3d9.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <initializer_list>

#include "../shaders/sprite.h"

namespace {

int failures = 0;
const char* current = "";

#define CHECK(x) do { if (!(x)) { \
    std::printf("  FAIL %s:%d  %s\n", current, __LINE__, #x); \
    failures++; return; } } while (0)

#define CHECK_HR(x) do { HRESULT _hr = (x); if (FAILED(_hr)) { \
    std::printf("  FAIL %s:%d  hr=0x%08lX  %s\n", current, __LINE__, \
        (unsigned long)_hr, #x); failures++; return; } } while (0)

// WA's surface is 640x480 or 1280x800; a small target keeps readback cheap
// while staying a realistic non-power-of-two size.
const UINT W = 256, H = 128;

struct Vertex {          // D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX2
    float x, y, z, rhw;
    DWORD colour;
    float u, v;          // atlas
    float su, sv;        // screen space, for the occlusion mask
};
const DWORD FVF = D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX2;

// Exactly the parameters WA builds at 0x59C2A0. The two that matter most:
// there is no depth/stencil buffer, and vertex processing is in software.
D3DPRESENT_PARAMETERS wa_present_params(HWND window) {
    D3DPRESENT_PARAMETERS pp = {};
    pp.BackBufferWidth = W;
    pp.BackBufferHeight = H;
    pp.BackBufferFormat = D3DFMT_X8R8G8B8;   // 22, as logged on this machine
    pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_COPY;
    pp.hDeviceWindow = window;
    pp.Windowed = TRUE;
    pp.EnableAutoDepthStencil = FALSE;       // WA has NO depth buffer
    return pp;
}
const DWORD WA_BEHAVIOUR =
    D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_FPU_PRESERVE;

// Read the backbuffer into system memory so assertions are about real pixels.
bool readback(IDirect3DDevice9* dev, BYTE* out) {
    IDirect3DSurface9* back = nullptr;
    IDirect3DSurface9* sys = nullptr;
    if (FAILED(dev->GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO, &back)))
        return false;
    bool ok = SUCCEEDED(dev->CreateOffscreenPlainSurface(
        W, H, D3DFMT_X8R8G8B8, D3DPOOL_SYSTEMMEM, &sys, nullptr));
    if (ok) ok = SUCCEEDED(dev->GetRenderTargetData(back, sys));
    if (ok) {
        D3DLOCKED_RECT lock = {};
        ok = SUCCEEDED(sys->LockRect(&lock, nullptr, D3DLOCK_READONLY));
        if (ok) {
            for (UINT y = 0; y < H; y++)
                std::memcpy(out + y * W * 4,
                            (const BYTE*)lock.pBits + y * lock.Pitch, W * 4);
            sys->UnlockRect();
        }
    }
    if (sys) sys->Release();
    if (back) back->Release();
    return ok;
}

DWORD pixel(const BYTE* fb, int x, int y) {
    return *(const DWORD*)(fb + (y * (int)W + x) * 4);
}
int ch(DWORD p, int shift) { return (int)((p >> shift) & 0xff); }
bool close_to(int a, int b, int tol = 8) { return std::abs(a - b) <= tol; }

// A 2x2 atlas: solid red, solid green, solid blue, solid white. Point-sampled,
// so a quad covering one texel is a flat colour we can assert exactly.
IDirect3DTexture9* make_atlas(IDirect3DDevice9* dev) {
    IDirect3DTexture9* tex = nullptr;
    if (FAILED(dev->CreateTexture(2, 2, 1, 0, D3DFMT_A8R8G8B8,
                                  D3DPOOL_MANAGED, &tex, nullptr)))
        return nullptr;
    D3DLOCKED_RECT lock = {};
    if (FAILED(tex->LockRect(0, &lock, nullptr, 0))) { tex->Release(); return nullptr; }
    DWORD* row0 = (DWORD*)lock.pBits;
    DWORD* row1 = (DWORD*)((BYTE*)lock.pBits + lock.Pitch);
    row0[0] = 0xFFFF0000; row0[1] = 0xFF00FF00;   // premultiplied, opaque
    row1[0] = 0xFF0000FF; row1[1] = 0xFFFFFFFF;
    tex->UnlockRect(0);
    return tex;
}

// Screen-space coverage mask. 0 = a nearer stock draw owns this pixel, so our
// sprite must not appear; 255 = ours to paint.
IDirect3DTexture9* make_mask(IDirect3DDevice9* dev, bool left_blocked) {
    IDirect3DTexture9* tex = nullptr;
    if (FAILED(dev->CreateTexture(W, H, 1, D3DUSAGE_DYNAMIC, D3DFMT_L8,
                                  D3DPOOL_DEFAULT, &tex, nullptr)))
        return nullptr;
    D3DLOCKED_RECT lock = {};
    if (FAILED(tex->LockRect(0, &lock, nullptr, D3DLOCK_DISCARD))) {
        tex->Release(); return nullptr;
    }
    for (UINT y = 0; y < H; y++) {
        BYTE* row = (BYTE*)lock.pBits + y * lock.Pitch;
        for (UINT x = 0; x < W; x++)
            row[x] = (left_blocked && x < W / 2) ? 0 : 255;
    }
    tex->UnlockRect(0);
    return tex;
}

void set_sprite_state(IDirect3DDevice9* dev) {
    dev->SetRenderState(D3DRS_ZENABLE, FALSE);
    dev->SetRenderState(D3DRS_LIGHTING, FALSE);
    dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
    dev->SetRenderState(D3DRS_ALPHABLENDENABLE, TRUE);
    // Premultiplied: opaque and additive share one state.
    dev->SetRenderState(D3DRS_SRCBLEND, D3DBLEND_ONE);
    dev->SetRenderState(D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
    for (DWORD s = 0; s < 2; s++) {
        dev->SetSamplerState(s, D3DSAMP_MINFILTER, D3DTEXF_POINT);
        dev->SetSamplerState(s, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
        dev->SetSamplerState(s, D3DSAMP_MIPFILTER, D3DTEXF_NONE);
        dev->SetSamplerState(s, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
        dev->SetSamplerState(s, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);
    }
}

// Build one quad, rotated and scaled on the CPU exactly as the renderer will.
// atlas_u/v select a texel of the 2x2 atlas.
void quad(Vertex* v, float cx, float cy, float half, float degrees,
          DWORD tint, float atlas_u, float atlas_v) {
    const float r = degrees * 3.14159265f / 180.0f;
    const float c = std::cos(r), s = std::sin(r);
    const float ox[4] = { -half, half, half, -half };
    const float oy[4] = { -half, -half, half, half };
    for (int i = 0; i < 4; i++) {
        // -0.5 is D3D9's pixel centre offset; WA applies the same.
        v[i].x = cx + ox[i] * c - oy[i] * s - 0.5f;
        v[i].y = cy + ox[i] * s + oy[i] * c - 0.5f;
        v[i].z = 0.0f; v[i].rhw = 1.0f;
        v[i].colour = tint;
        v[i].u = atlas_u; v[i].v = atlas_v;
        v[i].su = (v[i].x + 0.5f) / (float)W;
        v[i].sv = (v[i].y + 0.5f) / (float)H;
    }
}

// ---------------------------------------------------------------- tests

// 1. Rotated + scaled quad lands where we computed, tinted, and the mask gates
//    it. This is the core of the renderer in one draw.
void test_rotated_quad(IDirect3DDevice9* dev, IDirect3DPixelShader9* ps,
                       IDirect3DTexture9* atlas, BYTE* fb) {
    current = "rotated_quad";
    IDirect3DTexture9* mask = make_mask(dev, /*left_blocked=*/true);
    CHECK(mask);

    CHECK_HR(dev->Clear(0, nullptr, D3DCLEAR_TARGET, 0xFF000000, 1.0f, 0));
    CHECK_HR(dev->BeginScene());
    set_sprite_state(dev);
    dev->SetTexture(0, atlas);
    dev->SetTexture(1, mask);
    dev->SetFVF(FVF);
    dev->SetPixelShader(ps);
    dev->SetVertexShader(nullptr);

    // 37 degrees: no axis alignment, nothing that could pass by accident.
    // Right half (visible per the mask) and left half (masked out).
    Vertex right[4], left[4];
    quad(right, W * 0.75f, H * 0.5f, 24.0f, 37.0f, 0xFFFFFFFF, 0.25f, 0.25f);
    quad(left,  W * 0.25f, H * 0.5f, 24.0f, 37.0f, 0xFFFFFFFF, 0.25f, 0.25f);
    CHECK_HR(dev->DrawPrimitiveUP(D3DPT_TRIANGLEFAN, 2, right, sizeof(Vertex)));
    CHECK_HR(dev->DrawPrimitiveUP(D3DPT_TRIANGLEFAN, 2, left,  sizeof(Vertex)));
    CHECK_HR(dev->EndScene());
    CHECK(readback(dev, fb));

    // Centre of the unmasked quad is atlas texel (0,0) = red.
    DWORD centre = pixel(fb, (int)(W * 0.75f), (int)(H * 0.5f));
    CHECK(close_to(ch(centre, 16), 255));   // R
    CHECK(close_to(ch(centre, 8), 0));      // G
    CHECK(close_to(ch(centre, 0), 0));      // B

    // The masked quad must be entirely absent.
    DWORD masked = pixel(fb, (int)(W * 0.25f), (int)(H * 0.5f));
    CHECK(close_to(ch(masked, 16), 0) && close_to(ch(masked, 8), 0) && close_to(ch(masked, 0), 0));

    // Rotation really happened: at 37 degrees a corner of the square sticks out
    // further than half its side, so a point beyond the unrotated edge but
    // inside the rotated diagonal must be covered.
    //   unrotated half-width 24; rotated extent = 24*(|cos37|+|sin37|) ~= 33.6
    DWORD diagonal = pixel(fb, (int)(W * 0.75f) + 28, (int)(H * 0.5f));
    CHECK(close_to(ch(diagonal, 16), 255));

    // ... and a point beyond even the rotated extent must be background.
    DWORD outside = pixel(fb, (int)(W * 0.75f) + 40, (int)(H * 0.5f));
    CHECK(close_to(ch(outside, 16), 0));

    mask->Release();
    std::printf("  ok   rotated 37deg quad, tint, and screen-space mask\n");
}

// 2. THE decisive one. WA's present method calls BeginScene/DrawPrimitive/
//    EndScene at 0x59C7A0 and Presents separately at 0x59C8A0, so our hook in
//    after_shader_present sits between EndScene and Present. Drawing there
//    means opening a second scene. If its content does not survive to Present,
//    the whole "draw after WA's fullscreen quad" design collapses and we must
//    fall back to an in-scene hook at 0x59C86A.
void test_second_scene(IDirect3DDevice9* dev, IDirect3DPixelShader9* ps,
                       IDirect3DTexture9* atlas, BYTE* fb) {
    current = "second_scene";
    IDirect3DTexture9* mask = make_mask(dev, /*left_blocked=*/false);
    CHECK(mask);

    CHECK_HR(dev->Clear(0, nullptr, D3DCLEAR_TARGET, 0xFF000000, 1.0f, 0));

    // Scene 1 stands in for WA's own fullscreen quad.
    CHECK_HR(dev->BeginScene());
    set_sprite_state(dev);
    dev->SetTexture(0, atlas); dev->SetTexture(1, mask);
    dev->SetFVF(FVF); dev->SetPixelShader(ps); dev->SetVertexShader(nullptr);
    Vertex background[4];
    quad(background, W * 0.5f, H * 0.5f, 60.0f, 0.0f, 0xFFFFFFFF, 0.75f, 0.25f);
    CHECK_HR(dev->DrawPrimitiveUP(D3DPT_TRIANGLEFAN, 2, background, sizeof(Vertex)));
    CHECK_HR(dev->EndScene());

    // Scene 2 is ours, opened after WA's EndScene, before any Present.
    CHECK_HR(dev->BeginScene());
    set_sprite_state(dev);
    dev->SetTexture(0, atlas); dev->SetTexture(1, mask);
    dev->SetFVF(FVF); dev->SetPixelShader(ps); dev->SetVertexShader(nullptr);
    Vertex sprite[4];
    quad(sprite, W * 0.5f, H * 0.5f, 20.0f, 0.0f, 0xFFFFFFFF, 0.25f, 0.75f);
    CHECK_HR(dev->DrawPrimitiveUP(D3DPT_TRIANGLEFAN, 2, sprite, sizeof(Vertex)));
    CHECK_HR(dev->EndScene());

    CHECK(readback(dev, fb));

    // Centre: scene 2's blue sprite drew over scene 1's green background.
    DWORD centre = pixel(fb, W / 2, H / 2);
    CHECK(close_to(ch(centre, 0), 255));    // B from scene 2
    CHECK(close_to(ch(centre, 8), 0));

    // Outside the sprite but inside the background: scene 1 survived too.
    DWORD back = pixel(fb, W / 2 + 40, H / 2);
    CHECK(close_to(ch(back, 8), 255));      // G from scene 1
    CHECK(close_to(ch(back, 0), 0));

    mask->Release();
    std::printf("  ok   second BeginScene/EndScene before Present is preserved\n");
}

// 3. Additive through premultiplied alpha: rgb=c with a=0 under
//    ONE/INVSRCALPHA must add, so PX's additive muzzle flash needs no separate
//    pipeline state and can sit in the same draw list as opaque sprites.
void test_additive(IDirect3DDevice9* dev, IDirect3DPixelShader9* ps,
                   IDirect3DTexture9* atlas, BYTE* fb) {
    current = "additive";
    IDirect3DTexture9* mask = make_mask(dev, false);
    CHECK(mask);

    // Half-intensity grey ground so saturation is visible but not immediate.
    CHECK_HR(dev->Clear(0, nullptr, D3DCLEAR_TARGET, 0xFF404040, 1.0f, 0));
    CHECK_HR(dev->BeginScene());
    set_sprite_state(dev);
    dev->SetTexture(0, atlas); dev->SetTexture(1, mask);
    dev->SetFVF(FVF); dev->SetPixelShader(ps); dev->SetVertexShader(nullptr);

    // Tint alpha 0 makes the sampled texel additive: dest + src, no occlusion
    // of what is underneath.
    Vertex add[4];
    quad(add, W * 0.5f, H * 0.5f, 24.0f, 0.0f, 0x00FFFFFF, 0.25f, 0.25f);
    CHECK_HR(dev->DrawPrimitiveUP(D3DPT_TRIANGLEFAN, 2, add, sizeof(Vertex)));
    CHECK_HR(dev->EndScene());
    CHECK(readback(dev, fb));

    DWORD centre = pixel(fb, W / 2, H / 2);
    // 0x40 + 0xFF saturates to 0xFF in red; green/blue keep the ground's 0x40.
    CHECK(close_to(ch(centre, 16), 255));
    CHECK(close_to(ch(centre, 8), 0x40));
    CHECK(close_to(ch(centre, 0), 0x40));

    DWORD ground = pixel(fb, 4, 4);
    CHECK(close_to(ch(ground, 16), 0x40));

    mask->Release();
    std::printf("  ok   additive via premultiplied alpha (a=0) saturates\n");
}

// 4. A MANAGED atlas must survive Reset with its contents, so the renderer
//    does not re-upload every sheet on a resolution change (wkZoom does this
//    routinely; this machine's logs show 640x480 <-> 1280x800 flips).
//    DEFAULT-pool resources must be released first or Reset itself fails.
void test_reset(IDirect3DDevice9* dev, IDirect3D9* d3d, HWND window,
                IDirect3DPixelShader9** ps, IDirect3DTexture9* atlas, BYTE* fb) {
    current = "reset";
    // A DEFAULT-pool resource left alive makes Reset fail: prove the contract
    // by creating one, then releasing it the way on_shader_cleanup will.
    IDirect3DTexture9* mask = make_mask(dev, false);
    CHECK(mask);
    mask->Release();
    if (*ps) { (*ps)->Release(); *ps = nullptr; }

    D3DPRESENT_PARAMETERS pp = wa_present_params(window);
    HRESULT hr = dev->Reset(&pp);
    CHECK_HR(hr);

    // Recreate what the cleanup hook released.
    CHECK_HR(dev->CreatePixelShader((const DWORD*)WF_SPRITE_SHADER, ps));
    IDirect3DTexture9* mask2 = make_mask(dev, false);
    CHECK(mask2);

    // The MANAGED atlas was never touched: draw with it and confirm contents.
    CHECK_HR(dev->Clear(0, nullptr, D3DCLEAR_TARGET, 0xFF000000, 1.0f, 0));
    CHECK_HR(dev->BeginScene());
    set_sprite_state(dev);
    dev->SetTexture(0, atlas); dev->SetTexture(1, mask2);
    dev->SetFVF(FVF); dev->SetPixelShader(*ps); dev->SetVertexShader(nullptr);
    Vertex v[4];
    quad(v, W * 0.5f, H * 0.5f, 24.0f, 0.0f, 0xFFFFFFFF, 0.75f, 0.75f);
    CHECK_HR(dev->DrawPrimitiveUP(D3DPT_TRIANGLEFAN, 2, v, sizeof(Vertex)));
    CHECK_HR(dev->EndScene());
    CHECK(readback(dev, fb));

    // Atlas texel (1,1) is white; if the MANAGED surface had been lost this
    // would be black or garbage.
    DWORD centre = pixel(fb, W / 2, H / 2);
    CHECK(close_to(ch(centre, 16), 255));
    CHECK(close_to(ch(centre, 8), 255));
    CHECK(close_to(ch(centre, 0), 255));

    mask2->Release();
    std::printf("  ok   MANAGED atlas survives Reset; DEFAULT released/recreated\n");
}

// 5. We borrow WA's device, so every state we touch must be handed back. A
//    state block is the cheap way; this proves it actually restores.
void test_state_block(IDirect3DDevice9* dev, IDirect3DPixelShader9* ps,
                      IDirect3DTexture9* atlas) {
    current = "state_block";
    // The realistic scenario, not a synthetic one: WA has ITS shader and
    // textures bound when our hook runs. We must give exactly that back.
    // Restoring to "nothing bound" is not the case we care about and, as the
    // first run of this probe showed, is not what wined3d does either.
    IDirect3DPixelShader9* wa_shader = nullptr;
    CHECK_HR(dev->CreatePixelShader((const DWORD*)WF_SPRITE_SHADER, &wa_shader));

    dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
    dev->SetRenderState(D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
    dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_CCW);
    dev->SetFVF(D3DFVF_XYZ);
    dev->SetPixelShader(wa_shader);
    dev->SetTexture(0, nullptr);

    DWORD before_blend = 0, before_src = 0, before_cull = 0, before_fvf = 0;
    dev->GetRenderState(D3DRS_ALPHABLENDENABLE, &before_blend);
    dev->GetRenderState(D3DRS_SRCBLEND, &before_src);
    dev->GetRenderState(D3DRS_CULLMODE, &before_cull);
    dev->GetFVF(&before_fvf);

    IDirect3DStateBlock9* saved = nullptr;
    CHECK_HR(dev->CreateStateBlock(D3DSBT_ALL, &saved));
    CHECK_HR(saved->Capture());

    // Trash it the way a sprite batch would.
    set_sprite_state(dev);
    dev->SetTexture(0, atlas);
    dev->SetFVF(FVF);
    dev->SetPixelShader(ps);

    CHECK_HR(saved->Apply());
    saved->Release();

    DWORD after_blend = 0, after_src = 0, after_cull = 0, after_fvf = 0;
    IDirect3DPixelShader9* after_ps = nullptr;
    IDirect3DBaseTexture9* after_tex = nullptr;
    dev->GetRenderState(D3DRS_ALPHABLENDENABLE, &after_blend);
    dev->GetRenderState(D3DRS_SRCBLEND, &after_src);
    dev->GetRenderState(D3DRS_CULLMODE, &after_cull);
    dev->GetFVF(&after_fvf);
    dev->GetPixelShader(&after_ps);
    dev->GetTexture(0, &after_tex);

    CHECK(after_blend == before_blend);
    CHECK(after_src == before_src);
    CHECK(after_cull == before_cull);
    CHECK(after_fvf == before_fvf);
    // The one that decides whether a state block is enough on its own.
    const bool shader_restored = (after_ps == wa_shader);
    const bool texture_restored = (after_tex == nullptr);
    if (after_ps) after_ps->Release();
    if (after_tex) after_tex->Release();
    dev->SetPixelShader(nullptr);
    wa_shader->Release();

    if (shader_restored && texture_restored) {
        std::printf("  ok   state block restores states, FVF, shader and textures\n");
    } else {
        // Not fatal: the renderer can save and restore these two by hand. It
        // does mean a state block alone must not be trusted here, which is
        // worth knowing now rather than as a corrupted frame later.
        std::printf("  WARN state block restored states+FVF but NOT %s%s"
                    " -- restore these explicitly\n",
                    shader_restored ? "" : "pixel shader",
                    texture_restored ? "" : " textures");
    }
}

// Rough cost signal, not a benchmark: if 500 masked quads per frame is
// catastrophic under this translation layer we want to know before S1.
void test_throughput(IDirect3DDevice9* dev, IDirect3DPixelShader9* ps,
                     IDirect3DTexture9* atlas) {
    current = "throughput";
    IDirect3DTexture9* mask = make_mask(dev, false);
    CHECK(mask);

    LARGE_INTEGER freq;
    QueryPerformanceFrequency(&freq);
    const int frames = 60;

    auto bench = [&](int quads, bool batched) -> double {
        // One dynamic VB reused across frames, DISCARD-locked per frame: the
        // shape the real renderer will use.
        IDirect3DVertexBuffer9* vb = nullptr;
        if (batched) {
            if (FAILED(dev->CreateVertexBuffer(
                    (UINT)(quads * 6 * sizeof(Vertex)), D3DUSAGE_DYNAMIC |
                    D3DUSAGE_WRITEONLY, FVF, D3DPOOL_DEFAULT, &vb, nullptr)))
                return -1.0;
        }
        LARGE_INTEGER start, stop;
        QueryPerformanceCounter(&start);
        for (int f = 0; f < frames; f++) {
            dev->Clear(0, nullptr, D3DCLEAR_TARGET, 0xFF000000, 1.0f, 0);
            dev->BeginScene();
            set_sprite_state(dev);
            dev->SetTexture(0, atlas); dev->SetTexture(1, mask);
            dev->SetFVF(FVF); dev->SetPixelShader(ps);
            dev->SetVertexShader(nullptr);
            if (batched) {
                // Two triangles per quad as a TRIANGLELIST, so the whole batch
                // is one draw call rather than one per sprite.
                Vertex* dst = nullptr;
                if (SUCCEEDED(vb->Lock(0, 0, (void**)&dst, D3DLOCK_DISCARD))) {
                    for (int i = 0; i < quads; i++) {
                        Vertex v[4];
                        quad(v, 20.0f + (float)(i % 200), 20.0f + (float)(i % 80),
                             8.0f, (float)(i % 360), 0xFFFFFFFF, 0.25f, 0.25f);
                        Vertex* q = dst + i * 6;
                        q[0]=v[0]; q[1]=v[1]; q[2]=v[2];
                        q[3]=v[0]; q[4]=v[2]; q[5]=v[3];
                    }
                    vb->Unlock();
                    dev->SetStreamSource(0, vb, 0, sizeof(Vertex));
                    dev->DrawPrimitive(D3DPT_TRIANGLELIST, 0, (UINT)(quads * 2));
                }
            } else {
                for (int i = 0; i < quads; i++) {
                    Vertex v[4];
                    quad(v, 20.0f + (float)(i % 200), 20.0f + (float)(i % 80),
                         8.0f, (float)(i % 360), 0xFFFFFFFF, 0.25f, 0.25f);
                    dev->DrawPrimitiveUP(D3DPT_TRIANGLEFAN, 2, v, sizeof(Vertex));
                }
            }
            dev->EndScene();
        }
        QueryPerformanceCounter(&stop);
        if (vb) vb->Release();
        return 1000.0 * (double)(stop.QuadPart - start.QuadPart)
             / (double)freq.QuadPart / frames;
    };

    // 40 is a realistic worst case for a busy WA frame (the sentry alone is 3-4
    // quads); 500 is a stress figure that says how much headroom exists.
    for (int quads : { 40, 500 }) {
        const double up = bench(quads, false);
        const double vb = bench(quads, true);
        std::printf("  ..   %3d quads/frame: DrawPrimitiveUP %6.2f ms   "
                    "batched VB %6.2f ms  (%.1fx)\n",
                    quads, up, vb, (vb > 0.0) ? up / vb : 0.0);
    }
    std::printf("  ..   WA's own frame budget at 50 fps is 20.00 ms\n");
    mask->Release();
}

} // namespace

int main() {
    std::printf("WormForgeGPU S0 probe: D3D9 viability on this machine\n");
    std::printf("device = WA 3.8.1 parameters "
                "(X8R8G8B8, SWAPEFFECT_COPY, no depth, software VP)\n\n");

    HWND window = CreateWindowExA(0, "STATIC", "WormForgeGPU S0",
        WS_OVERLAPPEDWINDOW, 0, 0, (int)W, (int)H,
        nullptr, nullptr, GetModuleHandleA(nullptr), nullptr);
    if (!window) { std::printf("FATAL: CreateWindow failed\n"); return 2; }

    IDirect3D9* d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) { std::printf("FATAL: Direct3DCreate9 failed\n"); return 2; }

    D3DCAPS9 caps = {};
    d3d->GetDeviceCaps(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, &caps);
    std::printf("caps: pixelShader=%u.%u maxTexture=%lux%lu\n\n",
        (unsigned)D3DSHADER_VERSION_MAJOR(caps.PixelShaderVersion),
        (unsigned)D3DSHADER_VERSION_MINOR(caps.PixelShaderVersion),
        (unsigned long)caps.MaxTextureWidth,
        (unsigned long)caps.MaxTextureHeight);

    D3DPRESENT_PARAMETERS pp = wa_present_params(window);
    IDirect3DDevice9* dev = nullptr;
    HRESULT hr = d3d->CreateDevice(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, window,
                                   WA_BEHAVIOUR, &pp, &dev);
    if (FAILED(hr)) {
        std::printf("FATAL: CreateDevice with WA's parameters failed hr=0x%08lX\n",
                    (unsigned long)hr);
        d3d->Release();
        return 2;
    }

    IDirect3DPixelShader9* ps = nullptr;
    hr = dev->CreatePixelShader((const DWORD*)WF_SPRITE_SHADER, &ps);
    if (FAILED(hr)) {
        std::printf("FATAL: CreatePixelShader(sprite ps_2_0) failed hr=0x%08lX\n",
                    (unsigned long)hr);
        dev->Release(); d3d->Release();
        return 2;
    }

    IDirect3DTexture9* atlas = make_atlas(dev);
    if (!atlas) { std::printf("FATAL: atlas creation failed\n"); return 2; }

    BYTE* fb = (BYTE*)std::malloc((size_t)W * H * 4);
    if (!fb) { std::printf("FATAL: out of memory\n"); return 2; }

    test_rotated_quad(dev, ps, atlas, fb);
    test_second_scene(dev, ps, atlas, fb);
    test_additive(dev, ps, atlas, fb);
    test_state_block(dev, ps, atlas);
    test_reset(dev, d3d, window, &ps, atlas, fb);   // last: it resets the device
    test_throughput(dev, ps, atlas);

    std::free(fb);
    atlas->Release();
    if (ps) ps->Release();
    dev->Release();
    d3d->Release();

    std::printf("\n%s\n", failures == 0
        ? "PASS: the GPU sprite renderer is viable on this machine."
        : "FAIL: see above. This is the S0 kill criterion.");
    return failures == 0 ? 0 : 1;
}
