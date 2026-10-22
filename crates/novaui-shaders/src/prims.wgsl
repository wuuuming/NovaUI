// One source for every GPU backend: naga translates it to HLSL (D3D11) and SPIR-V (Vulkan) when
// novaui is built (novaui-shaders). One entry point per primitive kind; all kinds share one
// instance layout, so a run of equal kinds is one instanced draw. The math mirrors software.rs
// expression by expression. An image effect's module (`image::effect`) is this source with the
// effect appended and its entry point `fs_effect_main`, compiled when the app is built.

struct Globals { size: vec2<f32>, origin: vec2<f32> };   // origin: the backdrop region's (fs_backdrop)
@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var tex_a: texture_2d<f32>;
@group(0) @binding(3) var tex_b: texture_2d<f32>;

struct Inst {
    @location(0) dst: vec4<f32>,   // target-space box to cover (x, y, w, h)
    @location(1) p0: vec4<f32>,    // shape rect; glyph: x, y, uv x, uv y; layer: src.x, src.y, w, opacity
    @location(2) c0: vec4<f32>,    // colour; image: uv; effect: its parameters; layer: dst origin
    @location(3) c1: vec4<f32>,    // border colour
    @location(4) k: vec4<f32>,     // radius, border, sigma, extra (image, effect: opacity; layer: has_fade)
    @location(5) c2: vec4<f32>,    // glyph_xf: lit colour
};

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) dst: vec4<f32>,
    @location(1) @interpolate(flat) p0: vec4<f32>,
    @location(2) @interpolate(flat) c0: vec4<f32>,
    @location(3) @interpolate(flat) c1: vec4<f32>,
    @location(4) @interpolate(flat) k: vec4<f32>,
    @location(5) @interpolate(flat) c2: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, i: Inst) -> VOut {
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let p = i.dst.xy + corner * i.dst.zw;
    let ndc = vec2<f32>(p.x / g.size.x * 2.0 - 1.0, 1.0 - p.y / g.size.y * 2.0);
    var o: VOut;
    o.pos = vec4<f32>(ndc, 0.0, 1.0);
    o.dst = i.dst; o.p0 = i.p0; o.c0 = i.c0; o.c1 = i.c1; o.k = i.k; o.c2 = i.c2;
    return o;
}

fn premul(c: vec4<f32>, cov: f32) -> vec4<f32> {
    let a = c.a * cov;
    return vec4<f32>(c.rgb * a, a);
}

fn sd_round_box(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2<f32>(r, r);
    let o = max(q, vec2<f32>(0.0, 0.0));
    return sqrt(o.x * o.x + o.y * o.y) + min(max(q.x, q.y), 0.0) - r;
}

fn round_cov(rect: vec4<f32>, radius: f32, p: vec2<f32>) -> f32 {
    let h = rect.zw * 0.5;
    let c = p - (rect.xy + h);
    let r = min(min(radius, h.x), h.y);
    return clamp(0.5 - sd_round_box(c, h, r), 0.0, 1.0);
}

@fragment
fn fs_clear(v: VOut) -> @location(0) vec4<f32> {
    return premul(v.c0, 1.0);
}

// The radius of the corner `c` (from the centre) lies toward, 0 where `square` (k.z: a mask of
// corner bits, 1 top-left, 2 top-right, 4 bottom-right, 8 bottom-left) leaves it square.
fn corner_radius(c: vec2<f32>, r: f32, square: f32) -> f32 {
    let bits = u32(square + 0.5);
    var bit = 1u;
    if (c.x >= 0.0 && c.y < 0.0) { bit = 2u; }
    if (c.x >= 0.0 && c.y >= 0.0) { bit = 4u; }
    if (c.x < 0.0 && c.y >= 0.0) { bit = 8u; }
    return select(r, 0.0, (bits & bit) != 0u);
}

@fragment
fn fs_quad(v: VOut) -> @location(0) vec4<f32> {
    let rect = v.p0;
    let h = rect.zw * 0.5;
    let c = v.pos.xy - (rect.xy + h);
    let r = corner_radius(c, min(min(v.k.x, h.x), h.y), v.k.z);
    let d = sd_round_box(c, h, r);
    let cov = clamp(0.5 - d, 0.0, 1.0);
    var col = premul(v.c0, 1.0);
    let border = v.k.y;
    if (border > 0.0) {
        let di = sd_round_box(c, h - vec2<f32>(border, border), max(r - border, 0.0));
        let inside = clamp(0.5 - di, 0.0, 1.0);
        col = col * inside + premul(v.c1, 1.0) * (1.0 - inside);
    }
    return col * cov;
}

// Abramowitz–Stegun style erf used by Evan Wallace's rounded-box shadow.
fn erf_ws(x: f32) -> f32 {
    let s = sign(x);
    let a = abs(x);
    var t = 1.0 + (0.278393 + (0.230389 + 0.078108 * (a * a)) * a) * a;
    t = t * t;
    return s - s / (t * t);
}

fn gaussian(x: f32, sigma: f32) -> f32 {
    return exp(-(x * x) / (2.0 * sigma * sigma)) / (2.5066283 * sigma);
}

fn shadow_x(x: f32, y: f32, sigma: f32, corner: f32, half: vec2<f32>) -> f32 {
    let delta = min(half.y - corner - abs(y), 0.0);
    let curved = half.x - corner + sqrt(max(0.0, corner * corner - delta * delta));
    let k = 0.70710678 / sigma;
    let lo = 0.5 + 0.5 * erf_ws((x - curved) * k);
    let hi = 0.5 + 0.5 * erf_ws((x + curved) * k);
    return hi - lo;
}

fn shadow_alpha(rect: vec4<f32>, corner_in: f32, sigma: f32, p_in: vec2<f32>) -> f32 {
    let half = rect.zw * 0.5;
    let corner = min(min(corner_in, half.x), half.y);
    let p = p_in - (rect.xy + half);
    let low = p.y - half.y;
    let high = p.y + half.y;
    let start = clamp(-3.0 * sigma, low, high);
    let end = clamp(3.0 * sigma, low, high);
    let step = (end - start) / 4.0;
    var y = start + step * 0.5;
    var value = 0.0;
    for (var i = 0; i < 4; i = i + 1) {
        value = value + shadow_x(p.x, p.y - y, sigma, corner, half) * gaussian(y, sigma) * step;
        y = y + step;
    }
    return value;
}

@fragment
fn fs_shadow(v: VOut) -> @location(0) vec4<f32> {
    var a = shadow_alpha(v.p0, v.k.x, v.k.z, v.pos.xy);
    if (v.k.w > 0.5) {
        // The shadow of a translucent plate: nothing under the plate itself.
        a = a * (1.0 - round_cov(v.c1, v.k.x, floor(v.pos.xy) + vec2<f32>(0.5, 0.5)));
    }
    return premul(v.c0, a);
}

@fragment
fn fs_glyph(v: VOut) -> @location(0) vec4<f32> {
    let t = vec2<i32>(floor(v.pos.xy)) - vec2<i32>(v.p0.xy) + vec2<i32>(v.p0.zw);
    let a = textureLoad(atlas, t, 0).r;
    return premul(v.c0, a);
}

// A glyph's coverage at cell pixel p, zero outside its cell (size) at org in the atlas.
fn glyph_cell(p: vec2<i32>, size: vec2<i32>, org: vec2<i32>) -> f32 {
    if (p.x < 0 || p.y < 0 || p.x >= size.x || p.y >= size.y) {
        return 0.0;
    }
    return textureLoad(atlas, org + p, 0).r;
}

// A glyph through an affine map: the pixel's centre taken back into the glyph (c1, the inverse
// map; p0.xy, where glyph pixel 0 lands), sampled bilinear; lit behind the karaoke edge (k.z,
// soft over k.w glyph pixels).
@fragment
fn fs_glyph_xf(v: VOut) -> @location(0) vec4<f32> {
    let q = floor(v.pos.xy) + vec2<f32>(0.5, 0.5) - v.p0.xy;
    let l = vec2<f32>(v.c1.x * q.x + v.c1.z * q.y, v.c1.y * q.x + v.c1.w * q.y);
    let size = vec2<i32>(v.k.xy);
    let org = vec2<i32>(v.p0.zw);
    let t = l - vec2<f32>(0.5, 0.5);
    let f = floor(t);
    let w = t - f;
    let i = vec2<i32>(f);
    let a = glyph_cell(i, size, org);
    let b = glyph_cell(i + vec2<i32>(1, 0), size, org);
    let c = glyph_cell(i + vec2<i32>(0, 1), size, org);
    let d = glyph_cell(i + vec2<i32>(1, 1), size, org);
    let cov = mix(mix(a, b, w.x), mix(c, d, w.x), w.y);
    let lit = clamp((v.k.z - l.x) / v.k.w, 0.0, 1.0);
    return premul(mix(v.c0, v.c2, lit), cov);
}

fn load_clamped(tex: texture_2d<f32>, p: vec2<i32>) -> vec4<f32> {
    let d = vec2<i32>(textureDimensions(tex, 0));
    return textureLoad(tex, clamp(p, vec2<i32>(0, 0), d - vec2<i32>(1, 1)), 0);
}

// Bilinear filtering with clamp-to-edge; `t` in texel units (texel centres at +0.5).
fn bilinear(tex: texture_2d<f32>, t: vec2<f32>) -> vec4<f32> {
    let q = t - vec2<f32>(0.5, 0.5);
    let f = floor(q);
    let w = q - f;
    let i = vec2<i32>(f);
    let a = load_clamped(tex, i);
    let b = load_clamped(tex, i + vec2<i32>(1, 0));
    let c = load_clamped(tex, i + vec2<i32>(0, 1));
    let d = load_clamped(tex, i + vec2<i32>(1, 1));
    return mix(mix(a, b, w.x), mix(c, d, w.x), w.y);
}

@fragment
fn fs_image(v: VOut) -> @location(0) vec4<f32> {
    let rect = v.p0;
    let p = floor(v.pos.xy) + vec2<f32>(0.5, 0.5);
    let t = (p - rect.xy) / rect.zw;
    let uv = v.c0.xy + t * (v.c0.zw - v.c0.xy);
    let size = vec2<f32>(textureDimensions(tex_a, 0));
    let c = bilinear(tex_a, uv * size);
    return c * (round_cov(rect, v.k.x, p) * v.k.w);
}

// ---- the acrylic material (Prim::Backdrop) ----------------------------------------------------
// Dual-Kawase blur (Bjorge, "Bandwidth-efficient rendering", SIGGRAPH 2015): each pass reads the
// level above it bilinearly; p0.xy = the source level's logical size (its texture may be larger).

fn load_in(tex: texture_2d<f32>, p: vec2<i32>, size: vec2<i32>) -> vec4<f32> {
    return textureLoad(tex, clamp(p, vec2<i32>(0, 0), size - vec2<i32>(1, 1)), 0);
}

fn bilinear_in(tex: texture_2d<f32>, t: vec2<f32>, size: vec2<i32>) -> vec4<f32> {
    let q = t - vec2<f32>(0.5, 0.5);
    let f = floor(q);
    let w = q - f;
    let i = vec2<i32>(f);
    let a = load_in(tex, i, size);
    let b = load_in(tex, i + vec2<i32>(1, 0), size);
    let c = load_in(tex, i + vec2<i32>(0, 1), size);
    let d = load_in(tex, i + vec2<i32>(1, 1), size);
    return mix(mix(a, b, w.x), mix(c, d, w.x), w.y);
}

@fragment
fn fs_down(v: VOut) -> @location(0) vec4<f32> {
    let size = vec2<i32>(v.p0.xy);
    let s = (floor(v.pos.xy) + vec2<f32>(0.5, 0.5)) * 2.0;
    var c = bilinear_in(tex_a, s, size) * 4.0;
    c = c + bilinear_in(tex_a, s + vec2<f32>(1.0, 1.0), size);
    c = c + bilinear_in(tex_a, s - vec2<f32>(1.0, 1.0), size);
    c = c + bilinear_in(tex_a, s + vec2<f32>(1.0, -1.0), size);
    c = c + bilinear_in(tex_a, s - vec2<f32>(1.0, -1.0), size);
    return c / 8.0;
}

@fragment
fn fs_up(v: VOut) -> @location(0) vec4<f32> {
    let size = vec2<i32>(v.p0.xy);
    let s = (floor(v.pos.xy) + vec2<f32>(0.5, 0.5)) * 0.5;
    var c = bilinear_in(tex_a, s + vec2<f32>(-2.0, 0.0), size);
    c = c + bilinear_in(tex_a, s + vec2<f32>(-1.0, 1.0), size) * 2.0;
    c = c + bilinear_in(tex_a, s + vec2<f32>(0.0, 2.0), size);
    c = c + bilinear_in(tex_a, s + vec2<f32>(1.0, 1.0), size) * 2.0;
    c = c + bilinear_in(tex_a, s + vec2<f32>(2.0, 0.0), size);
    c = c + bilinear_in(tex_a, s + vec2<f32>(1.0, -1.0), size) * 2.0;
    c = c + bilinear_in(tex_a, s + vec2<f32>(0.0, -2.0), size);
    c = c + bilinear_in(tex_a, s + vec2<f32>(-1.0, -1.0), size) * 2.0;
    return c / 12.0;
}

fn noise(p: vec2<i32>) -> f32 {
    var h = u32(p.x) * 374761393u + u32(p.y) * 668265263u;
    h = (h ^ (h >> 13u)) * 1274126177u;
    h = h ^ (h >> 16u);
    return f32(h & 65535u) / 65535.0;
}

// The material (WinUI acrylic, in gamma space as Windows composes it): the blurred backdrop, its
// luminance pulled toward the luminosity colour, the tint over it, 2 % noise over that (k.y, the
// blur's passes: a flat material, none, has no noise).
@fragment
fn fs_backdrop(v: VOut) -> @location(0) vec4<f32> {
    let ip = vec2<i32>(floor(v.pos.xy));
    let b = textureLoad(tex_a, ip - vec2<i32>(g.origin), 0);
    // An HDR image under the material shows through at SDR white at most (an HDR canvas holds
    // values above 1 only there; SDR ones never reach it).
    var c = min(b.rgb, vec3<f32>(1.0));
    let luma = vec3<f32>(0.299, 0.587, 0.114);
    let lb = dot(c, luma);
    let ll = dot(v.c1.rgb, luma);
    let lum = clamp(c + vec3<f32>(ll - lb), vec3<f32>(0.0), vec3<f32>(1.0));
    c = mix(c, lum, v.c1.a);
    c = mix(c, v.c0.rgb, v.c0.a);
    if (v.k.y > 0.5) {
        c = mix(c, vec3<f32>(noise(ip)), 0.02);
    }
    let cov = round_cov(v.p0, v.k.x, floor(v.pos.xy) + vec2<f32>(0.5, 0.5)) * v.k.w;
    return vec4<f32>(c * cov, cov);
}

// HDR output (D3D11, scRGB): the canvas holds sRGB-encoded colour as on SDR (an HDR image's
// above 1.0); the swap chain takes linear light where 1.0 is 80 nits. Each pixel is decoded with
// sRGB's curve, carried on past 1.0, and scaled by c0.x (the SDR white over 80 nits): SDR content
// shows as Windows shows an SDR window. Opaque: the window is.
fn srgb_linear(c: f32) -> f32 {
    let a = abs(c);
    let l = select(pow((a + 0.055) / 1.055, 2.4), a / 12.92, a <= 0.04045);
    return sign(c) * l;
}

@fragment
fn fs_present(v: VOut) -> @location(0) vec4<f32> {
    let c = textureLoad(tex_a, vec2<i32>(floor(v.pos.xy)), 0);
    let l = vec3<f32>(srgb_linear(c.r), srgb_linear(c.g), srgb_linear(c.b));
    return vec4<f32>(l * v.c0.x, 1.0);
}

fn load_or_zero(tex: texture_2d<f32>, p: vec2<i32>) -> vec4<f32> {
    let d = vec2<i32>(textureDimensions(tex, 0));
    if (p.x < 0 || p.y < 0 || p.x >= d.x || p.y >= d.y) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    return textureLoad(tex, p, 0);
}

@fragment
fn fs_layer(v: VOut) -> @location(0) vec4<f32> {
    // c0.xy = unclipped layer destination origin.
    let p = vec2<i32>(floor(v.pos.xy)) - vec2<i32>(v.c0.xy) + vec2<i32>(v.p0.xy);
    var c = load_or_zero(tex_a, p);
    if (v.k.w > 0.5) {
        c = c * (1.0 - v.p0.z) + load_or_zero(tex_b, p) * v.p0.z;
    }
    return c * v.p0.w;
}
