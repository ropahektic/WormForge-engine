// Standalone GPU regression: exercises the production COM bridge under D3D9.
// Does not load WA or its prefix. A hidden window supplies a test device.
#include <windows.h>
#include <d3d9.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
extern "C" int wf_color_upload(void*, UINT, UINT, const BYTE*, const BYTE*);
extern "C" int wf_color_bind(void*);
extern "C" void wf_color_restore();
extern "C" void wf_color_release();
#define CHECK(x) do { if (!(x)) { std::fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #x); std::exit(1); } } while (0)
int main() {
    HWND window = CreateWindowExA(0, "STATIC", "WormForge renderer test", WS_OVERLAPPEDWINDOW,
        0,0,64,64,nullptr,nullptr,GetModuleHandleA(nullptr),nullptr);
    CHECK(window);
    auto* d3d = Direct3DCreate9(D3D_SDK_VERSION); CHECK(d3d);
    D3DPRESENT_PARAMETERS pp = {};
    pp.Windowed=TRUE; pp.SwapEffect=D3DSWAPEFFECT_DISCARD; pp.hDeviceWindow=window;
    pp.BackBufferWidth=8; pp.BackBufferHeight=4; pp.BackBufferFormat=D3DFMT_X8R8G8B8;
    IDirect3DDevice9* dev=nullptr;
    CHECK(SUCCEEDED(d3d->CreateDevice(D3DADAPTER_DEFAULT,D3DDEVTYPE_HAL,window,
        D3DCREATE_SOFTWARE_VERTEXPROCESSING,&pp,&dev)));
    IDirect3DTexture9 *frame=nullptr, *palette=nullptr;
    CHECK(SUCCEEDED(dev->CreateTexture(8,4,1,0,D3DFMT_L8,D3DPOOL_MANAGED,&frame,nullptr)));
    CHECK(SUCCEEDED(dev->CreateTexture(256,1,1,0,D3DFMT_A8R8G8B8,D3DPOOL_MANAGED,&palette,nullptr)));
    D3DLOCKED_RECT lock={};
    CHECK(SUCCEEDED(frame->LockRect(0,&lock,nullptr,0)));
    for (int y=0;y<4;y++) std::memset((BYTE*)lock.pBits+y*lock.Pitch,32,8);
    CHECK(SUCCEEDED(frame->UnlockRect(0)));
    CHECK(SUCCEEDED(palette->LockRect(0,&lock,nullptr,0)));
    for (int i=0;i<256;i++) ((DWORD*)lock.pBits)[i]=0xff000000u|i|(i<<8)|(i<<16);
    CHECK(SUCCEEDED(palette->UnlockRect(0)));
    BYTE rgba[8*4*4]={}, underlay[8*4]; std::memset(underlay,128,sizeof underlay);
    for (int y=0;y<4;y++) {
        BYTE* p=rgba+y*8*4;
        p[2]=128; p[3]=128; // half red over grey128, not the proxy's grey32
        p[4]=255; p[7]=255; // saturated blue absent from the greyscale palette
        p[11]=255;         // opaque black
    }
    CHECK(wf_color_upload(dev,8,4,rgba,underlay));
    CHECK(SUCCEEDED(dev->SetTexture(0,frame))); CHECK(SUCCEEDED(dev->SetTexture(1,palette)));
    for (int s=0;s<2;s++) {
        dev->SetSamplerState(s,D3DSAMP_MINFILTER,D3DTEXF_POINT);
        dev->SetSamplerState(s,D3DSAMP_MAGFILTER,D3DTEXF_POINT);
        dev->SetSamplerState(s,D3DSAMP_ADDRESSU,D3DTADDRESS_CLAMP);
        dev->SetSamplerState(s,D3DSAMP_ADDRESSV,D3DTADDRESS_CLAMP);
    }
    dev->SetRenderState(D3DRS_ZENABLE,FALSE); dev->SetRenderState(D3DRS_LIGHTING,FALSE);
    dev->SetRenderState(D3DRS_CULLMODE,D3DCULL_NONE); dev->SetRenderState(D3DRS_ALPHABLENDENABLE,FALSE);
    dev->SetFVF(D3DFVF_XYZRHW|D3DFVF_TEX1);
    CHECK(wf_color_bind(dev));
    struct Vertex { float x,y,z,rhw,u,v; } v[4]={
        {-0.5f,-0.5f,0,1,0,0},{7.5f,-0.5f,0,1,1,0},
        {-0.5f,3.5f,0,1,0,1},{7.5f,3.5f,0,1,1,1}};
    CHECK(SUCCEEDED(dev->BeginScene()));
    CHECK(SUCCEEDED(dev->DrawPrimitiveUP(D3DPT_TRIANGLESTRIP,2,v,sizeof(Vertex))));
    CHECK(SUCCEEDED(dev->EndScene()));
    wf_color_restore();
    IDirect3DPixelShader9* shader=nullptr; CHECK(SUCCEEDED(dev->GetPixelShader(&shader)) && !shader);
    IDirect3DBaseTexture9* stage=nullptr; CHECK(SUCCEEDED(dev->GetTexture(2,&stage)) && !stage);
    IDirect3DSurface9 *back=nullptr,*read=nullptr;
    CHECK(SUCCEEDED(dev->GetRenderTarget(0,&back)));
    CHECK(SUCCEEDED(dev->CreateOffscreenPlainSurface(8,4,D3DFMT_X8R8G8B8,D3DPOOL_SYSTEMMEM,&read,nullptr)));
    CHECK(SUCCEEDED(dev->GetRenderTargetData(back,read)));
    CHECK(SUCCEEDED(read->LockRect(&lock,nullptr,D3DLOCK_READONLY)));
    const int expected[4][3]={{64,64,192},{255,0,0},{0,0,0},{32,32,32}};
    for(int y=0;y<4;y++) for(int x=0;x<4;x++) for(int c=0;c<3;c++) {
        int value=((BYTE*)lock.pBits)[y*lock.Pitch+x*4+c];
        if(std::abs(value-expected[x][c])>1) {
            std::fprintf(stderr,"pixel %d,%d channel %d: %d expected %d\n",x,y,c,value,expected[x][c]);
            return 1;
        }
    }
    read->UnlockRect(); read->Release(); back->Release();
    CHECK(!wf_color_upload(nullptr,8,4,rgba,underlay)); CHECK(!wf_color_bind(dev));
    wf_color_release();
    dev->SetTexture(0,nullptr); dev->SetTexture(1,nullptr); frame->Release(); palette->Release();
    CHECK(SUCCEEDED(dev->Reset(&pp))); // catches leaked default-pool resources/stateblocks
    CHECK(wf_color_upload(dev,8,4,rgba,underlay));
    wf_color_release();
    dev->Release(); d3d->Release(); DestroyWindow(window);
    std::puts("PASS: truecolor, partial alpha/underlay, opaque black, stock fallback, state restore, device reset/recreate");
    return 0;
}
