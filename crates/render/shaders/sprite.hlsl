// GPU sprite quad: atlas texel * vertex tint, masked by screen-space coverage.
//
// Occlusion is a screen-space mask produced by diffing WA's own framebuffer
// across a split of its render-queue dispatch, so it records "a nearer stock
// draw touched this pixel" independently of our silhouette. Rotation and scale
// are baked into the four corner positions on the CPU (the device is
// SOFTWARE_VERTEXPROCESSING, so there is nothing to gain from doing it here),
// which is why this stays a pure pixel shader.
//
// Colour is premultiplied. With SRCBLEND=ONE, DESTBLEND=INVSRCALPHA a texel
// carrying rgb=c, a=0 is additive and one carrying a=1 is opaque, so both
// blend modes share a single pipeline state and can mix inside one draw list.
sampler2D atlas : register(s0);
sampler2D mask  : register(s1);

float4 main(float4 tint : COLOR0,
            float2 uv   : TEXCOORD0,
            float2 suv  : TEXCOORD1) : COLOR0 {
    float4 texel = tex2D(atlas, uv) * tint;
    return texel * tex2D(mask, suv).r;
}
