// Offline ps_2_0 bytecode. No runtime compiler or replacement d3d9.dll needed.
sampler2D stockFrame : register(s0);
sampler2D palette : register(s1);
sampler2D rgbaLayer : register(s2); // premultiplied RGBA
sampler2D underlay : register(s3);  // WA raster without the opted-in sprites

float4 main(float2 uv : TEXCOORD0) : COLOR0 {
    float4 rgba = tex2D(rgbaLayer, uv);
    float stock = tex2D(stockFrame, uv).r;
    float behind = tex2D(underlay, uv).r;
    // Outside our coverage, retain the real frame, including later UI draws.
    float index = lerp(stock, behind, step(0.5 / 255.0, rgba.a));
    float2 palUV = float2(index, index) * (255.0 / 256.0) + 0.5 / 256.0;
    float4 base = tex2D(palette, palUV);
    return float4(rgba.rgb + base.rgb * (1.0 - rgba.a), base.a);
}
