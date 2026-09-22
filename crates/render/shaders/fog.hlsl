// Fog of war: one fullscreen multiply quad.
//
// The blend state does the work, not the shader: SRCBLEND=ZERO,
// DESTBLEND=SRCCOLOR computes dest = src_colour * dest, so whatever colour
// this outputs scales the framebuffer already holding WA's composite (and our
// sprite quads). Sampling the L8 visibility mask as that colour darkens fogged
// pixels toward black and leaves lit pixels (v == 1) untouched.
//
// Legacy sampler2D/tex2D form, not Shader Model 4 syntax: ps_2_0 is what WA's
// D3D9 device and the offline fxc shim both speak, and wkFOW's shader notes
// warn the newer form does not survive that path. Arithmetic is the binding
// constraint under ps_2_0, so this stays a single sample and a splat.
sampler2D visibility : register(s0);

float4 main(float2 suv : TEXCOORD0) : COLOR0 {
    float v = tex2D(visibility, suv).r;
    return float4(v, v, v, 1);
}
