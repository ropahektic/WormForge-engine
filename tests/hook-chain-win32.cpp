// Independent x86 regression for the MinHook chain and HUD CALL-site policy.
// Runs in its own process; it does not load or modify WA.
#include <windows.h>
#include <MinHook.h>
#include <cstdint>
#include <cstdio>
#include <cstring>

using Fn = int (__cdecl *)();
static Fn prior_original = nullptr;
static Fn wf_original = nullptr;
static Fn callsite_original = nullptr;
static volatile int prior_calls = 0;
static volatile int wf_calls = 0;
static volatile int callsite_prior_calls = 0;
static volatile int callsite_wf_calls = 0;

extern "C" __declspec(noinline) int __cdecl prior_hook() {
    ++prior_calls;
    return prior_original() + 1;
}

extern "C" __declspec(noinline) int __cdecl wf_hook() {
    ++wf_calls;
    return wf_original() + 1;
}

extern "C" __declspec(noinline) int __cdecl previous_call_target() {
    ++callsite_prior_calls;
    return 10;
}

extern "C" __declspec(noinline) int __cdecl wf_callsite_hook() {
    ++callsite_wf_calls;
    return callsite_original() + 1;
}

static void write_rel32(unsigned char *site, unsigned char opcode, const void *target) {
    site[0] = opcode;
    const auto displacement = static_cast<std::uint32_t>(
        reinterpret_cast<std::uintptr_t>(target) -
        (reinterpret_cast<std::uintptr_t>(site) + 5));
    std::memcpy(site + 1, &displacement, 4);
    FlushInstructionCache(GetCurrentProcess(), site, 5);
}

static Fn decode_call(unsigned char *site) {
    if (site[0] != 0xE8) return nullptr;
    std::int32_t displacement;
    std::memcpy(&displacement, site + 1, 4);
    return reinterpret_cast<Fn>(site + 5 + displacement);
}

int main() {
    auto *original = static_cast<unsigned char *>(VirtualAlloc(nullptr, 64,
        MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE));
    auto *trampoline = static_cast<unsigned char *>(VirtualAlloc(nullptr, 64,
        MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE));
    auto *callsite = static_cast<unsigned char *>(VirtualAlloc(nullptr, 64,
        MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE));
    if (!original || !trampoline || !callsite) return 1;

    // Stock: mov eax,3; ret. The prior module copies the first five bytes,
    // then places an E9 at the stock entry to its own wrapper.
    const unsigned char stock[] = {0xB8, 3, 0, 0, 0, 0xC3};
    std::memcpy(original, stock, sizeof(stock));
    std::memcpy(trampoline, stock, 5);
    write_rel32(trampoline + 5, 0xE9, original + 5);
    prior_original = reinterpret_cast<Fn>(trampoline);
    write_rel32(original, 0xE9, reinterpret_cast<void *>(&prior_hook));

    if (MH_Initialize() != MH_OK) return 2;
    if (MH_CreateHook(original, reinterpret_cast<void *>(&wf_hook),
                      reinterpret_cast<void **>(&wf_original)) != MH_OK) return 3;
    if (MH_EnableHook(original) != MH_OK) return 4;
    const int chained = reinterpret_cast<Fn>(original)();
    if (chained != 5 || prior_calls != 1 || wf_calls != 1) {
        std::printf("entry chain failed: value=%d prior=%d wf=%d\n",
                    chained, prior_calls, wf_calls);
        return 5;
    }

    // The HUD call site may already CALL another module. Decode that live
    // target, replace only the CALL operand, then invoke it from our wrapper.
    write_rel32(callsite, 0xE8, reinterpret_cast<void *>(&previous_call_target));
    callsite[5] = 0xC3;
    callsite_original = decode_call(callsite);
    if (callsite_original != &previous_call_target) return 6;
    write_rel32(callsite, 0xE8, reinterpret_cast<void *>(&wf_callsite_hook));
    const int called = reinterpret_cast<Fn>(callsite)();
    if (called != 11 || callsite_prior_calls != 1 || callsite_wf_calls != 1) {
        std::printf("CALL chain failed: value=%d prior=%d wf=%d\n",
                    called, callsite_prior_calls, callsite_wf_calls);
        return 7;
    }
    MH_DisableHook(original);
    MH_RemoveHook(original);
    MH_Uninitialize();
    std::puts("entry jump and CALL-site chains passed");
    return 0;
}
