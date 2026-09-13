// Shared endpoint-refinement helpers, prepended to every compression shader.
struct Params {
    width: u32,
    height: u32,
    blocks_x: u32,
    blocks_y: u32,
    refine_iters: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_blocks: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

struct Endpoints {
    e0: vec4<f32>,
    e1: vec4<f32>,
}

var<private> fit_px: array<vec4<f32>, 16>;
var<private> fit_w: array<f32, 16>;

fn refit_endpoints(cur: Endpoints) -> Endpoints {
    var a = 0.0;
    var b = 0.0;
    var c = 0.0;
    var r0 = vec4<f32>(0.0);
    var r1 = vec4<f32>(0.0);

    for (var i = 0u; i < 16u; i = i + 1u) {
        let w = fit_w[i];
        let v = 1.0 - w;
        a = a + v * v;
        b = b + v * w;
        c = c + w * w;
        r0 = r0 + v * fit_px[i];
        r1 = r1 + w * fit_px[i];
    }

    let det = a * c - b * b;
    if abs(det) < 1e-6 {
        return cur;
    }
    let lo = vec4<f32>(0.0);
    let hi = vec4<f32>(255.0);
    return Endpoints(
        clamp((c * r0 - b * r1) / det, lo, hi),
        clamp((a * r1 - b * r0) / det, lo, hi),
    );
}

fn unpack_rgba(packed: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(packed & 0xFFu),
        f32((packed >> 8u) & 0xFFu),
        f32((packed >> 16u) & 0xFFu),
        f32((packed >> 24u) & 0xFFu),
    );
}

fn rgb_to_565(r: f32, g: f32, b: f32) -> u32 {
    let r5 = u32(clamp(round(r * 31.0 / 255.0), 0.0, 31.0));
    let g6 = u32(clamp(round(g * 63.0 / 255.0), 0.0, 63.0));
    let b5 = u32(clamp(round(b * 31.0 / 255.0), 0.0, 31.0));
    return (r5 << 11u) | (g6 << 5u) | b5;
}

fn rgb565_to_rgb(c: u32) -> vec3<f32> {
    let r5 = (c >> 11u) & 31u;
    let g6 = (c >> 5u) & 63u;
    let b5 = c & 31u;
    return vec3<f32>(
        f32((r5 * 527u + 23u) >> 6u),
        f32((g6 * 259u + 33u) >> 6u),
        f32((b5 * 527u + 23u) >> 6u),
    );
}

var<private> fit_pixels: array<vec3<f32>, 16>;
var<private> fit_lock_b5: i32 = -1;

struct ColorFit {
    color0: u32,
    color1: u32,
    indices: u32,
    err: f32,
}

fn color_weight(idx: u32) -> f32 {
    if idx == 0u { return 0.0; }
    if idx == 1u { return 1.0; }
    if idx == 2u { return 1.0 / 3.0; }
    return 2.0 / 3.0;
}

fn color_fit_565(c0_in: u32, c1_in: u32) -> ColorFit {
    var c0 = c0_in;
    var c1 = c1_in;
    if fit_lock_b5 >= 0 {
        let b = u32(fit_lock_b5) & 31u;
        c0 = (c0 & 0xFFE0u) | b;
        c1 = (c1 & 0xFFE0u) | b;
    }
    if c0 < c1 {
        let t = c0;
        c0 = c1;
        c1 = t;
    } else if c0 == c1 {
        if (c0 & 0xF800u) < 0xF800u {
            c0 = c0 + 0x0800u;
        } else if c0 > 0u {
            c1 = c0 - 1u;
        }
    }

    var pal: array<vec3<f32>, 4>;
    pal[0] = rgb565_to_rgb(c0);
    pal[1] = rgb565_to_rgb(c1);
    pal[2] = (pal[0] * 2.0 + pal[1]) / 3.0;
    pal[3] = (pal[0] + pal[1] * 2.0) / 3.0;

    var indices = 0u;
    var err = 0.0;
    for (var i = 0u; i < 16u; i = i + 1u) {
        let px = fit_pixels[i];
        var best_idx = 0u;
        var best_dist = dot(px - pal[0], px - pal[0]);
        for (var j = 1u; j < 4u; j = j + 1u) {
            let d = dot(px - pal[j], px - pal[j]);
            if d < best_dist {
                best_dist = d;
                best_idx = j;
            }
        }
        indices = indices | (best_idx << (i * 2u));
        err = err + best_dist;
    }
    return ColorFit(c0, c1, indices, err);
}

fn color_refine(initial: ColorFit) -> ColorFit {
    var best = initial;
    for (var it = 0u; it < params.refine_iters; it = it + 1u) {
        for (var i = 0u; i < 16u; i = i + 1u) {
            fit_px[i] = vec4<f32>(fit_pixels[i], 0.0);
            fit_w[i] = color_weight((best.indices >> (i * 2u)) & 3u);
        }
        let cur = Endpoints(
            vec4<f32>(rgb565_to_rgb(best.color0), 0.0),
            vec4<f32>(rgb565_to_rgb(best.color1), 0.0),
        );
        let r = refit_endpoints(cur);
        let cand = color_fit_565(rgb_to_565(r.e0.x, r.e0.y, r.e0.z),
                                 rgb_to_565(r.e1.x, r.e1.y, r.e1.z));
        if cand.err >= best.err {
            break;
        }
        best = cand;
    }
    return best;
}

fn encode_color_block() -> ColorFit {
    var lo = vec3<f32>(255.0);
    var hi = vec3<f32>(0.0);
    for (var i = 0u; i < 16u; i = i + 1u) {
        lo = min(lo, fit_pixels[i]);
        hi = max(hi, fit_pixels[i]);
    }
    let inset = (hi - lo) / 16.0;
    let a = clamp(hi - inset, vec3<f32>(0.0), vec3<f32>(255.0));
    let b = clamp(lo + inset, vec3<f32>(0.0), vec3<f32>(255.0));
    let base = color_fit_565(rgb_to_565(a.x, a.y, a.z), rgb_to_565(b.x, b.y, b.z));
    return color_refine(base);
}

var<private> fit_scalars: array<f32, 16>;

struct AlphaFit {
    a0: u32,
    a1: u32,
    words: vec2<u32>,
    err: f32,
}

fn alpha_weight(idx: u32) -> f32 {
    if idx == 0u { return 0.0; }
    if idx == 1u { return 1.0; }
    return f32(idx - 1u) / 7.0;
}

fn alpha_fit(a0_in: u32, a1_in: u32) -> AlphaFit {
    var a0 = a0_in;
    var a1 = a1_in;
    if a0 < a1 {
        let t = a0;
        a0 = a1;
        a1 = t;
    }
    var palette: array<f32, 8>;
    palette[0] = f32(a0);
    palette[1] = f32(a1);
    for (var j = 2u; j < 8u; j = j + 1u) {
        palette[j] = mix(f32(a0), f32(a1), f32(j - 1u) / 7.0);
    }

    var indices_lo = 0u;
    var indices_hi = 0u;
    var err = 0.0;

    for (var i = 0u; i < 16u; i = i + 1u) {
        var best_idx = 0u;
        var best_dist = abs(fit_scalars[i] - palette[0]);
        for (var j = 1u; j < 8u; j = j + 1u) {
            let d = abs(fit_scalars[i] - palette[j]);
            if d < best_dist {
                best_idx = j;
                best_dist = d;
            }
        }
        err = err + best_dist * best_dist;

        let bit_pos = i * 3u;
        if bit_pos < 32u {
            indices_lo = indices_lo | (best_idx << bit_pos);
            if bit_pos > 29u {
                indices_hi = indices_hi | (best_idx >> (32u - bit_pos));
            }
        } else {
            indices_hi = indices_hi | (best_idx << (bit_pos - 32u));
        }
    }

    let word0 = a0 | (a1 << 8u) | ((indices_lo & 0xFFFFu) << 16u);
    let word1 = (indices_lo >> 16u) | (indices_hi << 16u);
    return AlphaFit(a0, a1, vec2<u32>(word0, word1), err);
}

fn encode_alpha_block_from_scalars() -> AlphaFit {
    var lo = fit_scalars[0];
    var hi = fit_scalars[0];
    for (var i = 1u; i < 16u; i = i + 1u) {
        lo = min(lo, fit_scalars[i]);
        hi = max(hi, fit_scalars[i]);
    }
    var best = alpha_fit(u32(clamp(hi, 0.0, 255.0)), u32(clamp(lo, 0.0, 255.0)));

    for (var it = 0u; it < params.refine_iters; it = it + 1u) {
        let ilo = (best.words.x >> 16u) | (best.words.y << 16u);
        let ihi = best.words.y >> 16u;
        for (var i = 0u; i < 16u; i = i + 1u) {
            let bit_pos = i * 3u;
            var idx = 0u;
            if bit_pos >= 32u {
                idx = ihi >> (bit_pos - 32u);
            } else {
                idx = ilo >> bit_pos;
                if bit_pos > 29u {
                    idx = idx | (ihi << (32u - bit_pos));
                }
            }
            fit_px[i] = vec4<f32>(fit_scalars[i], 0.0, 0.0, 0.0);
            fit_w[i] = alpha_weight(idx & 7u);
        }
        let cur = Endpoints(vec4<f32>(f32(best.a0), 0.0, 0.0, 0.0),
                            vec4<f32>(f32(best.a1), 0.0, 0.0, 0.0));
        let r = refit_endpoints(cur);
        let cand = alpha_fit(u32(round(clamp(r.e0.x, 0.0, 255.0))),
                             u32(round(clamp(r.e1.x, 0.0, 255.0))));
        if cand.err >= best.err {
            break;
        }
        best = cand;
    }
    return best;
}
