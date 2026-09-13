// WGSL Fragment Shader to display Hap Q (YCoCg in DXT5) directly on GPU
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@group(0) @binding(0) var t_diffuse: texture_2d<f32>;
@group(0) @binding(1) var s_diffuse: sampler;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let raw = textureSample(t_diffuse, s_diffuse, in.uv);
    let y = raw.a;
    let blue = raw.b * 255.0;
    let scale = (blue / 8.0) + 1.0;

    let co = (raw.r * 255.0 - 128.0) / scale;
    let cg = (raw.g * 255.0 - 128.0) / scale;

    let r = clamp((y * 255.0 + co - cg) / 255.0, 0.0, 1.0);
    let g = clamp((y * 255.0 + cg) / 255.0, 0.0, 1.0);
    let b = clamp((y * 255.0 - co - cg) / 255.0, 0.0, 1.0);

    return vec4<f32>(r, g, b, 1.0);
}
