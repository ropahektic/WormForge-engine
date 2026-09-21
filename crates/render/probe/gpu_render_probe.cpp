// S1 gate: exercise the PRODUCTION renderer (native/wf_gpu.cpp), not a copy.
//
// The S0 probe proved D3D9 on this machine can do what the design needs, using
// inline test code. This one links the real shim and drives its real entry
// points, so what passes here is the code that will run inside WA. It still
// never loads WA: a failure here is a renderer bug, with no game state or hook
// ordering to confuse the diagnosis.
//
// Vertices are built by hand in the same layout crates/render/src/geom.rs
// emits, which is checked independently by that crate's unit tests. The
// arrangement is deliberate -- geometry is verified on the host where it is
// cheap to test, and only the COM behaviour is verified here.
#include <windows.h>
#include <d3d9.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <initializer_list>

extern "C" {
int  wf_gpu_init(void* device);
void wf_gpu_on_device_lost();
void wf_gpu_release();
int  wf_gpu_atlas(unsigned w, unsigned h, const unsigned char* bgra);
int  wf_gpu_mask(unsigned w, unsigned h, const unsigned char* coverage);
int  wf_gpu_draw(const void* verts, unsigned count);
}

namespace {

int failures = 0;
const char* current = "";

#define CHECK(x) do { if (!(x)) { \
    std::printf("  FAIL %s:%d  %s\n", current, __LINE__, #x); \
    failures++; return; } } while (0)

const UINT W = 256, H = 128;
const UINT ATLAS = 64;

struct Vertex {                 // must match crates/render/src/geom.rs
    float x, y, z, rhw;
    DWORD colour;
    float u, v, su, sv;
};

D3DPRESENT_PARAMETERS wa_present_params(HWND window) {
    D3DPRESENT_PARAMETERS pp = {};
    pp.BackBufferWidth = W;
    pp.BackBufferHeight = H;
    pp.BackBufferFormat = D3DFMT_X8R8G8B8;
    pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_COPY;
    pp.hDeviceWindow = window;
    pp.Windowed = TRUE;
    pp.EnableAutoDepthStencil = FALSE;
    return pp;
}

bool readback(IDirect3DDevice9* dev, BYTE* out) {
    IDirect3DSurface9 *back = nullptr, *sys = nullptr;
    if (FAILED(dev->GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO, &back))) return false;
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

// A 64x64 atlas whose top-left 32x32 is opaque red: one frame, premultiplied
// BGRA, exactly as crates/render/src/atlas.rs assembles it.
void build_atlas(BYTE* bgra) {
    std::memset(bgra, 0, ATLAS * ATLAS * 4);
    for (UINT y = 0; y < 32; y++) {
        for (UINT x = 0; x < 32; x++) {
            BYTE* p = bgra + (y * ATLAS + x) * 4;
            p[0] = 0; p[1] = 0; p[2] = 255; p[3] = 255;   // B,G,R,A
        }
    }
}

// Same half-texel inset as SheetRect::uv() in atlas.rs.
void quad(Vertex* out, float cx, float cy, float half, float degrees,
          DWORD tint) {
    const float r = degrees * 3.14159265f / 180.0f;
    const float c = std::cos(r), s = std::sin(r);
    const float ox[4] = { -half, half, half, -half };
    const float oy[4] = { -half, -half, half, half };
    const float u0 = 0.5f / ATLAS, v0 = 0.5f / ATLAS;
    const float u1 = 31.5f / ATLAS, v1 = 31.5f / ATLAS;
    const float uu[4] = { u0, u1, u1, u0 };
    const float vv[4] = { v0, v0, v1, v1 };
    Vertex v[4];
    for (int i = 0; i < 4; i++) {
        v[i].x = cx + ox[i] * c - oy[i] * s - 0.5f;
        v[i].y = cy + ox[i] * s + oy[i] * c - 0.5f;
        v[i].z = 0.0f; v[i].rhw = 1.0f;
        v[i].colour = tint;
        v[i].u = uu[i]; v[i].v = vv[i];
        v[i].su = (v[i].x + 0.5f) / (float)W;
        v[i].sv = (v[i].y + 0.5f) / (float)H;
    }
    out[0]=v[0]; out[1]=v[1]; out[2]=v[2];
    out[3]=v[0]; out[4]=v[2]; out[5]=v[3];
}

void fill_mask(BYTE* mask, bool block_left) {
    for (UINT y = 0; y < H; y++)
        for (UINT x = 0; x < W; x++)
            mask[y * W + x] = (block_left && x < W / 2) ? 0 : 255;
}

// The production path, end to end: init, atlas, mask, one batched draw.
void test_draws_through_production_api(IDirect3DDevice9* dev, BYTE* fb) {
    current = "production_draw";
    BYTE atlas[ATLAS * ATLAS * 4];
    build_atlas(atlas);
    BYTE* mask = (BYTE*)std::malloc(W * H);
    CHECK(mask);
    fill_mask(mask, /*block_left=*/true);

    CHECK(wf_gpu_init(dev));
    CHECK(wf_gpu_atlas(ATLAS, ATLAS, atlas));
    CHECK(wf_gpu_mask(W, H, mask));

    CHECK(SUCCEEDED(dev->Clear(0, nullptr, D3DCLEAR_TARGET, 0xFF000000, 1.0f, 0)));
    // Two sprites in ONE batch: right is visible, left is masked out.
    Vertex verts[12];
    quad(verts,     W * 0.75f, H * 0.5f, 24.0f, 37.0f, 0xFFFFFFFF);
    quad(verts + 6, W * 0.25f, H * 0.5f, 24.0f, 37.0f, 0xFFFFFFFF);
    CHECK(wf_gpu_draw(verts, 12));
    CHECK(readback(dev, fb));

    DWORD visible = pixel(fb, (int)(W * 0.75f), (int)(H * 0.5f));
    CHECK(close_to(ch(visible, 16), 255));
    CHECK(close_to(ch(visible, 8), 0));

    DWORD masked = pixel(fb, (int)(W * 0.25f), (int)(H * 0.5f));
    CHECK(close_to(ch(masked, 16), 0));

    // Rotation really applied: past the unrotated edge, inside the diagonal.
    CHECK(close_to(ch(pixel(fb, (int)(W * 0.75f) + 28, (int)(H * 0.5f)), 16), 255));
    CHECK(close_to(ch(pixel(fb, (int)(W * 0.75f) + 40, (int)(H * 0.5f)), 16), 0));

    std::free(mask);
    std::printf("  ok   wf_gpu_init/atlas/mask/draw renders a masked rotated batch\n");
}

// Tint and additive travel in the vertex colour, so one greyscale sheet can
// serve six team colours and an additive flash in the same batch.
void test_tint_and_additive_share_one_batch(IDirect3DDevice9* dev, BYTE* fb) {
    current = "tint_and_additive";
    BYTE* mask = (BYTE*)std::malloc(W * H);
    CHECK(mask);
    fill_mask(mask, false);
    CHECK(wf_gpu_mask(W, H, mask));

    CHECK(SUCCEEDED(dev->Clear(0, nullptr, D3DCLEAR_TARGET, 0xFF202020, 1.0f, 0)));
    Vertex verts[12];
    // Left: red atlas texel tinted to half green -> modulates to near-black,
    // proving the tint multiplies rather than replaces.
    quad(verts,     W * 0.25f, H * 0.5f, 20.0f, 0.0f, 0xFF00FF00);
    // Right: additive (alpha 0) over the grey ground -> red saturates, the
    // other channels keep the ground.
    quad(verts + 6, W * 0.75f, H * 0.5f, 20.0f, 0.0f, 0x00FFFFFF);
    CHECK(wf_gpu_draw(verts, 12));
    CHECK(readback(dev, fb));

    DWORD tinted = pixel(fb, (int)(W * 0.25f), (int)(H * 0.5f));
    CHECK(close_to(ch(tinted, 16), 0));    // red texel * green tint = no red
    CHECK(close_to(ch(tinted, 8), 0));

    DWORD added = pixel(fb, (int)(W * 0.75f), (int)(H * 0.5f));
    CHECK(close_to(ch(added, 16), 255));   // 0x20 + 0xFF saturates
    CHECK(close_to(ch(added, 8), 0x20));   // ground survives underneath

    std::free(mask);
    std::printf("  ok   tint modulates and additive saturates in one batch\n");
}

// Every call must fail closed rather than drawing something wrong or crashing:
// inside WA a bad frame is far worse than a skipped one.
void test_rejects_bad_input(IDirect3DDevice9* dev) {
    current = "rejects_bad_input";
    CHECK(!wf_gpu_init(nullptr));
    CHECK(wf_gpu_init(dev));                       // re-init must stay valid

    BYTE tiny[4] = { 0, 0, 0, 0 };
    CHECK(!wf_gpu_atlas(0, 4, tiny));             // zero dimension
    CHECK(!wf_gpu_atlas(4, 4, nullptr));           // null pixels
    CHECK(!wf_gpu_atlas(99999, 4, tiny));         // implausible size
    CHECK(!wf_gpu_mask(0, 0, tiny));

    Vertex v[6];
    quad(v, 10.0f, 10.0f, 4.0f, 0.0f, 0xFFFFFFFF);
    CHECK(!wf_gpu_draw(nullptr, 6));               // null buffer
    CHECK(!wf_gpu_draw(v, 0));                     // empty
    CHECK(!wf_gpu_draw(v, 4));                     // not whole triangles

    std::printf("  ok   invalid input is refused, not drawn\n");
}

// The cleanup hook runs before WA resets the shared device. DEFAULT-pool
// resources must be gone by then; the MANAGED atlas must NOT be, so sheets
// survive a resolution change without re-uploading.
void test_survives_device_reset(IDirect3DDevice9* dev, HWND window, BYTE* fb) {
    current = "device_reset";
    wf_gpu_on_device_lost();

    D3DPRESENT_PARAMETERS pp = wa_present_params(window);
    CHECK(SUCCEEDED(dev->Reset(&pp)));
    CHECK(wf_gpu_init(dev));

    // Only the mask is re-supplied: the atlas is deliberately not re-uploaded.
    BYTE* mask = (BYTE*)std::malloc(W * H);
    CHECK(mask);
    fill_mask(mask, false);
    CHECK(wf_gpu_mask(W, H, mask));

    CHECK(SUCCEEDED(dev->Clear(0, nullptr, D3DCLEAR_TARGET, 0xFF000000, 1.0f, 0)));
    Vertex v[6];
    quad(v, W * 0.5f, H * 0.5f, 20.0f, 0.0f, 0xFFFFFFFF);
    CHECK(wf_gpu_draw(v, 6));
    CHECK(readback(dev, fb));

    DWORD centre = pixel(fb, W / 2, H / 2);
    CHECK(close_to(ch(centre, 16), 255));  // the MANAGED atlas kept its pixels
    CHECK(close_to(ch(centre, 8), 0));

    std::free(mask);
    std::printf("  ok   cleanup + Reset keeps the MANAGED atlas usable\n");
}

// WA's own fullscreen draw runs immediately after ours on the same device.
void test_restores_wa_device_state(IDirect3DDevice9* dev) {
    current = "restores_state";
    dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
    dev->SetRenderState(D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
    dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_CCW);
    dev->SetFVF(D3DFVF_XYZ);

    DWORD blend0 = 0, src0 = 0, cull0 = 0, fvf0 = 0;
    dev->GetRenderState(D3DRS_ALPHABLENDENABLE, &blend0);
    dev->GetRenderState(D3DRS_SRCBLEND, &src0);
    dev->GetRenderState(D3DRS_CULLMODE, &cull0);
    dev->GetFVF(&fvf0);

    Vertex v[6];
    quad(v, W * 0.5f, H * 0.5f, 16.0f, 12.0f, 0xFFFFFFFF);
    CHECK(wf_gpu_draw(v, 6));

    DWORD blend1 = 0, src1 = 0, cull1 = 0, fvf1 = 0;
    IDirect3DBaseTexture9* tex = nullptr;
    IDirect3DPixelShader9* ps = nullptr;
    dev->GetRenderState(D3DRS_ALPHABLENDENABLE, &blend1);
    dev->GetRenderState(D3DRS_SRCBLEND, &src1);
    dev->GetRenderState(D3DRS_CULLMODE, &cull1);
    dev->GetFVF(&fvf1);
    dev->GetTexture(0, &tex);
    dev->GetPixelShader(&ps);

    CHECK(blend1 == blend0);
    CHECK(src1 == src0);
    CHECK(cull1 == cull0);
    CHECK(fvf1 == fvf0);
    CHECK(tex == nullptr);   // our atlas must not still be bound
    CHECK(ps == nullptr);    // nor our shader
    if (tex) tex->Release();
    if (ps) ps->Release();

    std::printf("  ok   device state is handed back exactly as found\n");
}

void test_frame_cost(IDirect3DDevice9* dev) {
    current = "frame_cost";
    BYTE* mask = (BYTE*)std::malloc(W * H);
    CHECK(mask);
    fill_mask(mask, false);

    LARGE_INTEGER freq, start, stop;
    QueryPerformanceFrequency(&freq);
    const int frames = 60;
    for (int quads : { 40, 500 }) {
        Vertex* verts = (Vertex*)std::malloc(sizeof(Vertex) * quads * 6);
        if (!verts) { std::free(mask); return; }
        QueryPerformanceCounter(&start);
        for (int f = 0; f < frames; f++) {
            for (int i = 0; i < quads; i++)
                quad(verts + i * 6, 20.0f + (float)(i % 200),
                     20.0f + (float)(i % 80), 8.0f, (float)(i % 360), 0xFFFFFFFF);
            // A real frame re-uploads the mask as well as the geometry.
            wf_gpu_mask(W, H, mask);
            wf_gpu_draw(verts, (unsigned)(quads * 6));
        }
        QueryPerformanceCounter(&stop);
        const double ms = 1000.0 * (double)(stop.QuadPart - start.QuadPart)
                        / (double)freq.QuadPart / frames;
        std::printf("  ..   %3d sprites/frame incl. full-surface mask: %.2f ms\n",
                    quads, ms);
        std::free(verts);
    }
    std::printf("  ..   WA's frame budget at 50 fps is 20.00 ms\n");
    std::free(mask);
}

} // namespace

int main() {
    std::printf("WormForgeGPU S1 probe: the production renderer under D3D9\n");
    std::printf("links native/wf_gpu.cpp; no WA, no hooks, no game state\n\n");

    HWND window = CreateWindowExA(0, "STATIC", "WormForgeGPU S1",
        WS_OVERLAPPEDWINDOW, 0, 0, (int)W, (int)H,
        nullptr, nullptr, GetModuleHandleA(nullptr), nullptr);
    if (!window) { std::printf("FATAL: CreateWindow failed\n"); return 2; }

    IDirect3D9* d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) { std::printf("FATAL: Direct3DCreate9 failed\n"); return 2; }

    D3DPRESENT_PARAMETERS pp = wa_present_params(window);
    IDirect3DDevice9* dev = nullptr;
    HRESULT hr = d3d->CreateDevice(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, window,
        D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_FPU_PRESERVE, &pp, &dev);
    if (FAILED(hr)) {
        std::printf("FATAL: CreateDevice failed hr=0x%08lX\n", (unsigned long)hr);
        d3d->Release();
        return 2;
    }

    BYTE* fb = (BYTE*)std::malloc((size_t)W * H * 4);
    if (!fb) { std::printf("FATAL: out of memory\n"); return 2; }

    test_draws_through_production_api(dev, fb);
    test_tint_and_additive_share_one_batch(dev, fb);
    test_rejects_bad_input(dev);
    test_restores_wa_device_state(dev);
    test_survives_device_reset(dev, window, fb);   // resets the device
    test_frame_cost(dev);

    wf_gpu_release();
    std::free(fb);
    dev->Release();
    d3d->Release();

    std::printf("\n%s\n", failures == 0
        ? "PASS: the production renderer is correct on this machine."
        : "FAIL: see above.");
    return failures == 0 ? 0 : 1;
}
