// Draws one layer of the frame: a 4:2:0 picture, its luma and chroma in separate textures,
// turned into RGB here and laid over what is under it.

struct Uniforms {
    // Takes (Y, Cb, Cr, 1) as sampled to gamma-encoded RGB.
    to_rgb: mat4x4<f32>,
    // Where the picture's corners go in the frame (-1 to 1 across it, y up): a 2x2 matrix,
    // column by column.
    corner_columns: vec4<f32>,
    // xy: the offset after that. zw: the share of the viewer the frame covers once fitted.
    offset_and_frame: vec4<f32>,
    // x: how much of the layer is seen, 0 to 1. y: above zero when the target stores linear
    // light and encodes it again on write.
    opacity_and_target: vec4<f32>,
    // During a wipe the layer shows where dot(xy, the point of the frame) is at most z.
    wipe: vec4<f32>,
    // Its colour (see `Grade::uniforms`): contrast, the brightness's shift of luma, saturation,
    // and the shift of Cb.
    grade_color: vec4<f32>,
    // The shift of Cr, sharpening, vignette, and the blur's radius as a share of the height.
    grade_filters: vec4<f32>,
    // The picture's height over its width, and a pixel's width and height in texture units.
    picture: vec4<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var luma: texture_2d<f32>;
@group(0) @binding(2) var chroma: texture_2d<f32>;
@group(0) @binding(3) var smooth_sampler: sampler;
// How much of each pixel is seen: the picture's own size for words, one white texel otherwise.
@group(0) @binding(4) var alpha: texture_2d<f32>;

struct Corner {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    // Where in the frame, -1 to 1 across it, y up.
    @location(1) in_frame: vec2<f32>,
}

// Four corners of a triangle strip; the viewport is the viewer's area.
@vertex
fn vertex(@builtin(vertex_index) index: u32) -> Corner {
    let corner = vec2<f32>(f32(index & 1u), f32(index >> 1u));
    let across = corner * 2.0 - 1.0;
    let columns = uniforms.corner_columns;
    let in_frame = vec2<f32>(columns.x, columns.y) * across.x
        + vec2<f32>(columns.z, columns.w) * across.y
        + uniforms.offset_and_frame.xy;
    var out: Corner;
    out.position = vec4<f32>(in_frame * uniforms.offset_and_frame.zw, 0.0, 1.0);
    out.uv = vec2<f32>(corner.x, 1.0 - corner.y);
    out.in_frame = in_frame;
    return out;
}

fn srgb_to_linear(encoded: vec3<f32>) -> vec3<f32> {
    let low = encoded / 12.92;
    let high = pow((encoded + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, encoded <= vec3<f32>(0.04045));
}

// Luma and chroma at `uv`, as stored.
fn yuv_at(uv: vec2<f32>) -> vec3<f32> {
    let y = textureSampleLevel(luma, smooth_sampler, uv, 0.0).r;
    let cb_cr = textureSampleLevel(chroma, smooth_sampler, uv, 0.0).rg;
    return vec3<f32>(y, cb_cr);
}

const TAU: f32 = 6.2831853;

// The picture's own colours with the clip's colour and filters applied, as `Grade` and
// `FilmPicture::adjust` work them for an export: blur, sharpen, colour, vignette.
fn graded(uv: vec2<f32>) -> vec3<f32> {
    var yuv = yuv_at(uv);
    let filters = uniforms.grade_filters;
    if filters.w > 0.0 {
        // Soft: the middle and two rings round it, taken together.
        let reach = vec2<f32>(filters.w * uniforms.picture.x, filters.w);
        var sum = yuv;
        for (var i = 0; i < 8; i++) {
            let angle = f32(i) * TAU / 8.0;
            sum += yuv_at(uv + reach * vec2<f32>(cos(angle), sin(angle)));
            let between = angle + TAU / 16.0;
            sum += yuv_at(uv + 0.5 * reach * vec2<f32>(cos(between), sin(between)));
        }
        yuv = sum / 17.0;
    }
    if filters.y > 0.0 {
        let step = uniforms.picture.zw;
        let around = (yuv_at(uv + vec2<f32>(step.x, 0.0)).x + yuv_at(uv - vec2<f32>(step.x, 0.0)).x
            + yuv_at(uv + vec2<f32>(0.0, step.y)).x + yuv_at(uv - vec2<f32>(0.0, step.y)).x) / 4.0;
        yuv.x = clamp(yuv.x + filters.y * (yuv.x - around), 0.0, 1.0);
    }
    let color = uniforms.grade_color;
    let y = clamp((yuv.x - 0.5) * color.x + 0.5 + color.y, 0.0, 1.0);
    var chroma = (yuv.yz - 0.5) * color.z + vec2<f32>(color.w, filters.x);
    chroma = clamp(chroma + 0.5, vec2<f32>(0.0), vec2<f32>(1.0)) - 0.5;
    // Darker towards the corners: across the picture, -1 to 1 each way.
    var kept = 1.0;
    if filters.z > 0.0 {
        let out = length(uv * 2.0 - 1.0);
        kept = 1.0 - filters.z * smoothstep(0.35, 1.35, out);
    }
    return vec3<f32>(y * kept, chroma * kept + 0.5);
}

@fragment
fn fragment(in: Corner) -> @location(0) vec4<f32> {
    // Sampled first: a texture is only sampled where every fragment around still runs.
    let yuv = graded(in.uv);
    let seen = textureSampleLevel(alpha, smooth_sampler, in.uv, 0.0).r;
    if dot(uniforms.wipe.xy, in.in_frame) > uniforms.wipe.z {
        discard;
    }
    var rgb = (uniforms.to_rgb * vec4<f32>(yuv, 1.0)).rgb;
    rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    if uniforms.opacity_and_target.y > 0.0 {
        rgb = srgb_to_linear(rgb);
    }
    return vec4<f32>(rgb, uniforms.opacity_and_target.x * seen);
}
