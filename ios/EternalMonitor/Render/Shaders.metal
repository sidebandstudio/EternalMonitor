#include <metal_stdlib>
using namespace metal;

struct DisplayUniforms {
    float2 scale;        // aspect-fit or aspect-fill scale applied to the fullscreen quad
    uint   matrixIndex;  // 0 = BT.709, 1 = BT.601 (video range)
};

struct VertexOut {
    float4 position [[position]];
    float2 texCoord;
};

// Generates an aspect-fit quad from vertex_id (no vertex buffer needed).
// Draw with .triangleStrip, vertexCount: 4. `scale` shrinks the quad on one
// axis to letterbox/pillarbox; the pass clear color shows through around it.
// For Fill, `scale` grows one axis past 1 and clipping trims those edges.
vertex VertexOut fullscreen_vertex(uint vid [[vertex_id]],
                                   constant DisplayUniforms &uniforms [[buffer(0)]]) {
    const float2 positions[4] = {
        float2(-1.0, -1.0),  // bottom-left
        float2( 1.0, -1.0),  // bottom-right
        float2(-1.0,  1.0),  // top-left
        float2( 1.0,  1.0),  // top-right
    };
    const float2 texCoords[4] = {
        float2(0.0, 1.0),
        float2(1.0, 1.0),
        float2(0.0, 0.0),
        float2(1.0, 0.0),
    };

    VertexOut out;
    out.position = float4(positions[vid] * uniforms.scale, 0.0, 1.0);
    out.texCoord = texCoords[vid];
    return out;
}

// NV12 (video-range YCbCr) → RGB. The decoder outputs biplanar 4:2:0: a full-res
// luma plane and a half-res interleaved CbCr plane. Matrix selected per frame
// from the pixel buffer's colorimetry attachment (709 default, 601 fallback).
fragment half4 display_fragment(VertexOut in [[stage_in]],
                                texture2d<float> lumaTex [[texture(0)]],
                                texture2d<float> chromaTex [[texture(1)]],
                                constant DisplayUniforms &uniforms [[buffer(0)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);

    float y  = lumaTex.sample(s, in.texCoord).r;
    float2 cbcr = chromaTex.sample(s, in.texCoord).rg;

    // Expand video range: Y [16/255, 235/255] → [0,1]; chroma centered at 128/255.
    float yl = (y - 16.0 / 255.0) * (255.0 / 219.0);
    float cb = (cbcr.x - 128.0 / 255.0) * (255.0 / 224.0);
    float cr = (cbcr.y - 128.0 / 255.0) * (255.0 / 224.0);

    float3 rgb;
    if (uniforms.matrixIndex == 1) {
        // BT.601
        rgb = float3(
            yl + 1.402 * cr,
            yl - 0.344136 * cb - 0.714136 * cr,
            yl + 1.772 * cb
        );
    } else {
        // BT.709
        rgb = float3(
            yl + 1.5748 * cr,
            yl - 0.1873 * cb - 0.4681 * cr,
            yl + 1.8556 * cb
        );
    }

    return half4(half3(saturate(rgb)), 1.0h);
}

// The PC's pointer, drawn over the video (PointerOverlay.swift). `rect` is
// the image's quad in normalized device coordinates: minX, minY, maxX, maxY.
struct PointerUniforms {
    float4 rect;
};

vertex VertexOut pointer_vertex(uint vid [[vertex_id]],
                                constant PointerUniforms &uniforms [[buffer(0)]]) {
    // Texture coordinates of the strip's corners: bottom-left, bottom-right,
    // top-left, top-right (texture rows run downward).
    const float2 corners[4] = {
        float2(0.0, 1.0),
        float2(1.0, 1.0),
        float2(0.0, 0.0),
        float2(1.0, 0.0),
    };
    float2 t = corners[vid];
    VertexOut out;
    out.position = float4(mix(uniforms.rect.x, uniforms.rect.z, t.x),
                          mix(uniforms.rect.w, uniforms.rect.y, t.y), 0.0, 1.0);
    out.texCoord = t;
    return out;
}

// Premultiplied BGRA, blended "over" the picture.
fragment half4 pointer_fragment(VertexOut in [[stage_in]],
                                texture2d<float> image [[texture(0)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    return half4(image.sample(s, in.texCoord));
}

// The invert mask as white: the pipeline's blend (1 - destination) * source
// + destination * (1 - source) turns it into "invert the picture here".
fragment half4 pointer_invert_fragment(VertexOut in [[stage_in]],
                                       texture2d<float> mask [[texture(0)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    float m = mask.sample(s, in.texCoord).r;
    return half4(m, m, m, m);
}
