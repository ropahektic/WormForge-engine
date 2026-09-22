// Small COM boundary; queue policy and composition live in Rust.
#include <windows.h>
#include <d3d9.h>
#include <cstring>
#include "../shaders/truecolor.h"

namespace {
IDirect3DDevice9* device = nullptr; // borrowed, render thread only
IDirect3DTexture9* rgba = nullptr;
IDirect3DTexture9* indices = nullptr;
IDirect3DPixelShader9* shader = nullptr;
IDirect3DStateBlock9* saved = nullptr;
UINT width = 0, height = 0;
bool submitted = false;
template<class T> void release(T*& p) { if (p) { p->Release(); p = nullptr; } }

bool upload(IDirect3DTexture9* texture, const BYTE* pixels, UINT rowBytes) {
    D3DLOCKED_RECT lock = {};
    if (FAILED(texture->LockRect(0, &lock, nullptr, D3DLOCK_DISCARD))) return false;
    bool ok = lock.pBits && lock.Pitch >= static_cast<int>(rowBytes);
    if (ok) for (UINT y = 0; y < height; ++y)
        std::memcpy(static_cast<BYTE*>(lock.pBits) + y * lock.Pitch, pixels + y * rowBytes, rowBytes);
    return SUCCEEDED(texture->UnlockRect(0)) && ok;
}
}

extern "C" void wf_color_restore() {
    if (saved) { saved->Apply(); release(saved); }
}

extern "C" void wf_color_release() {
    wf_color_restore();
    release(rgba); release(indices); release(shader);
    device = nullptr; width = height = 0; submitted = false;
}

extern "C" int wf_color_upload(void* borrowedDevice, UINT w, UINT h,
    const BYTE* pixels, const BYTE* background) {
    auto* current = static_cast<IDirect3DDevice9*>(borrowedDevice);
    submitted = false;
    if (!current || !pixels || !background || !w || !h || w > 4096 || h > 4096
        || current->TestCooperativeLevel() != D3D_OK) return 0;
    if (device != current || width != w || height != h || !rgba || !indices || !shader) {
        wf_color_release(); device = current; width = w; height = h;
        if (FAILED(device->CreateTexture(w, h, 1, D3DUSAGE_DYNAMIC, D3DFMT_A8R8G8B8,
                D3DPOOL_DEFAULT, &rgba, nullptr))
            || FAILED(device->CreateTexture(w, h, 1, D3DUSAGE_DYNAMIC, D3DFMT_L8,
                D3DPOOL_DEFAULT, &indices, nullptr))
            || FAILED(device->CreatePixelShader(reinterpret_cast<const DWORD*>(WF_TRUECOLOR_SHADER), &shader))) {
            wf_color_release(); return 0;
        }
    }
    submitted = upload(rgba, pixels, w * 4) && upload(indices, background, w);
    return submitted ? 1 : 0;
}

extern "C" int wf_color_bind(void* borrowedDevice) {
    if (!submitted || device != borrowedDevice) return 0;
    submitted = false;
    if (FAILED(device->CreateStateBlock(D3DSBT_ALL, &saved))) return 0;
    bool ok = SUCCEEDED(saved->Capture())
        && SUCCEEDED(device->SetTexture(2, rgba))
        && SUCCEEDED(device->SetTexture(3, indices));
    for (DWORD stage = 2; stage <= 3; ++stage) {
        ok = SUCCEEDED(device->SetSamplerState(stage, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP)) && ok;
        ok = SUCCEEDED(device->SetSamplerState(stage, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP)) && ok;
        ok = SUCCEEDED(device->SetSamplerState(stage, D3DSAMP_MINFILTER, D3DTEXF_POINT)) && ok;
        ok = SUCCEEDED(device->SetSamplerState(stage, D3DSAMP_MAGFILTER, D3DTEXF_POINT)) && ok;
        ok = SUCCEEDED(device->SetSamplerState(stage, D3DSAMP_MIPFILTER, D3DTEXF_NONE)) && ok;
    }
    ok = ok && SUCCEEDED(device->SetPixelShader(shader));
    if (!ok) wf_color_restore();
    return ok ? 1 : 0;
}
