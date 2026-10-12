// Draws a 4:2:0 picture: luma and chroma come in as separate textures and are turned into RGB here.

struct Uniforms {
    // Takes (Y, Cb, Cr, 1) as sampled to gamma-encoded RGB.
    to_rgb: mat4x4<f32>,
    // The share of the viewer the picture covers once it is fitted inside.
    covered: vec2<f32>,
    // Above zero when the target stores linear light and encodes it again on write.
    linear_target: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var luma: texture_2d<f32>;
@group(0) @binding(2) var chroma: texture_2d<f32>;
@group(0) @binding(3) var smooth_sampler: sampler;

struct Corner {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// Four corners of a triangle strip; the viewport is the viewer's area.
@vertex
fn vertex(@builtin(vertex_index) index: u32) -> Corner {
    let corner = vec2<f32>(f32(index & 1u), f32(index >> 1u));
    var out: Corner;
    out.position = vec4<f32>((corner * 2.0 - 1.0) * uniforms.covered, 0.0, 1.0);
    out.uv = vec2<f32>(corner.x, 1.0 - corner.y);
    return out;
}

fn srgb_to_linear(encoded: vec3<f32>) -> vec3<f32> {
    let low = encoded / 12.92;
    let high = pow((encoded + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, encoded <= vec3<f32>(0.04045));
}

@fragment
fn fragment(in: Corner) -> @location(0) vec4<f32> {
    let y = textureSample(luma, smooth_sampler, in.uv).r;
    let cb_cr = textureSample(chroma, smooth_sampler, in.uv).rg;
    var rgb = (uniforms.to_rgb * vec4<f32>(y, cb_cr, 1.0)).rgb;
    rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    if uniforms.linear_target > 0.0 {
        rgb = srgb_to_linear(rgb);
    }
    return vec4<f32>(rgb, 1.0);
}
