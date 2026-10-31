//! Primitive → instance encoding shared by GPU backends. Runs of equal kind and equal texture
//! bindings become one instanced draw each.

use crate::geom::*;
use crate::render::primitive::*;

/// The renderer's shader (novaui-shaders' `prims.wgsl`) as HLSL, compiled when novaui was built.
#[cfg(windows)]
pub const HLSL: &str = include_str!(concat!(env!("OUT_DIR"), "/prims.hlsl"));

#[cfg(not(windows))]
mod spirv {
    include!(concat!(env!("OUT_DIR"), "/prims_spirv.rs"));
}

/// The renderer's SPIR-V module of `entry` (one per entry point), compiled when novaui was built.
#[cfg(not(windows))]
pub fn spirv(entry: &str) -> Option<&'static [u8]> {
    spirv::SPIRV.iter().find(|(e, _)| *e == entry).map(|(_, b)| *b)
}

pub const K_QUAD: u8 = 0;
pub const K_SHADOW: u8 = 1;
pub const K_GLYPH: u8 = 2;
pub const K_LAYER: u8 = 3;
pub const K_IMAGE: u8 = 4;
pub const K_CLEAR: u8 = 5;
pub const K_BACKDROP: u8 = 6;
pub const K_DOWN: u8 = 7;
pub const K_UP: u8 = 8;
pub const K_GLYPH_XF: u8 = 9;
/// The first registered image effect (`image::effect`); effect `k` is `K_EFFECT + k`.
pub const K_EFFECT: u8 = 10;
/// The built-in fragment entry points, by kind.
pub const FS: [&str; 10] = [
    "fs_quad",
    "fs_shadow",
    "fs_glyph",
    "fs_layer",
    "fs_image",
    "fs_clear",
    "fs_backdrop",
    "fs_down",
    "fs_up",
    "fs_glyph_xf",
];

/// The fragment entry point of an image effect's module (novaui-shaders' `EFFECT_ENTRY`).
pub const EFFECT_ENTRY: &str = "fs_effect_main";

/// Whether a batch can be drawn: every texture it names is there (`have`). A batch missing
/// one (a layer not allocated yet or any more, an image still being made) is left out whole,
/// on every backend.
pub fn drawable(tex: (Tex, Tex), have: impl Fn(Tex) -> bool) -> bool {
    [tex.0, tex.1].into_iter().all(|t| t == Tex::None || have(t))
}

/// Whether draws of `kind` blend over what is drawn (premultiplied source-over); clears and blur
/// passes replace it.
pub fn blends(kind: u8) -> bool {
    !matches!(kind, K_CLEAR | K_DOWN | K_UP)
}

/// Render targets grow in 256-px steps and are reused within a step: a window resize or a layer
/// that changes size by a few pixels per frame does not create (and leave for deferred
/// destruction) a texture per frame.
pub fn bucket(v: i32) -> i32 {
    ((v.max(1) + 255) / 256) * 256
}

/// Whether a target of `have` (width, height) is reused for `w`×`h`: it fits and is not more
/// than twice too large on an axis.
pub fn fits(have: (i32, i32), w: i32, h: i32) -> bool {
    w <= have.0 && h <= have.1 && bucket(w) * 2 > have.0 && bucket(h) * 2 > have.1
}

/// A `Prim::Backdrop`'s instance read back (as `encode` writes it): its plate, its Kawase levels
/// and the margin they read.
pub fn backdrop_of(i: &Inst) -> (Rect, u8, i32) {
    let rect = Rect::new(i.p0[0] as i32, i.p0[1] as i32, i.p0[2] as i32, i.p0[3] as i32);
    (rect, i.k[1] as u8, i.k[2] as i32)
}

/// One blur pass's instance: a full-level quad of level `to` reading level `from` (`sizes` from
/// `kawase_sizes`).
pub fn kawase_pass(sizes: &[(i32, i32)], from: usize, to: usize) -> Inst {
    let (fw, fh) = sizes[from];
    let (w, h) = sizes[to];
    Inst { dst: [0.0, 0.0, w as f32, h as f32], p0: [fw as f32, fh as f32, 0.0, 0.0], ..Default::default() }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Inst {
    pub dst: [f32; 4],
    pub p0: [f32; 4],
    pub c0: [f32; 4],
    pub c1: [f32; 4],
    pub k: [f32; 4],
    pub c2: [f32; 4],
}

/// Vec4 attributes per instance (`Inst`).
pub const INST_ATTRS: u32 = 6;

pub const INST_SIZE: usize = std::mem::size_of::<Inst>();

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tex {
    None,
    Layer(u32),
    Image(u32),
    /// An image's HDR rendition where the backend shows HDR, else the image (`Prim::Image`).
    ImageHdr(u32),
}

pub struct Batch {
    pub kind: u8,
    pub first: u32,
    pub count: u32,
    pub tex: (Tex, Tex),
}

fn r4(r: Rect) -> [f32; 4] {
    r.to_f()
}

/// Encodes a clear of `clip` + `draws` (offset applied, boxes clipped) into instances and batches.
pub fn encode(
    clear: Color,
    clip: Rect,
    off: (i32, i32),
    draws: &[Draw],
    out: &mut Vec<Inst>,
    batches: &mut Vec<Batch>,
) {
    out.clear();
    batches.clear();
    let push = |kind: u8, tex: (Tex, Tex), i: Inst, out: &mut Vec<Inst>, batches: &mut Vec<Batch>| {
        match batches.last_mut() {
            Some(b) if b.kind == kind && b.tex == tex => b.count += 1,
            _ => batches.push(Batch { kind, first: out.len() as u32, count: 1, tex }),
        }
        out.push(i);
    };
    let none = (Tex::None, Tex::None);
    push(K_CLEAR, none, Inst { dst: r4(clip), c0: clear, ..Default::default() }, out, batches);
    for d in draws {
        let b = d.cover(clip, off);
        if b.is_empty() {
            continue;
        }
        let (kind, tex, i) = match d.prim.translated(off.0, off.1) {
            Prim::Quad { rect, radius, square, fill, border, border_color } => (
                K_QUAD,
                none,
                Inst {
                    dst: r4(b),
                    p0: r4(rect),
                    c0: fill,
                    c1: border_color,
                    k: [radius, border, f32::from(square), 0.0],
                    ..Default::default()
                },
            ),
            Prim::Shadow { rect, radius, sigma, color, cut } => (
                K_SHADOW,
                none,
                Inst {
                    dst: r4(b),
                    p0: r4(rect),
                    c0: color,
                    c1: cut.map_or([0.0; 4], r4),
                    k: [radius, 0.0, shadow_sigma(sigma), cut.is_some() as i32 as f32],
                    ..Default::default()
                },
            ),
            // One batch per backdrop: the backend blurs before drawing it.
            Prim::Backdrop { rect, radius, levels, extent, tint, lum, opacity } => {
                let inst = Inst {
                    dst: r4(b),
                    p0: r4(rect),
                    c0: tint,
                    c1: lum,
                    k: [radius, levels as f32, extent as f32, opacity],
                    ..Default::default()
                };
                batches.push(Batch { kind: K_BACKDROP, first: out.len() as u32, count: 1, tex: none });
                out.push(inst);
                continue;
            }
            Prim::Glyph { x, y, uv, color } => (
                K_GLYPH,
                none,
                Inst {
                    dst: r4(b),
                    p0: [x as f32, y as f32, uv.x as f32, uv.y as f32],
                    c0: color,
                    ..Default::default()
                },
            ),
            Prim::GlyphXf { o, m, uv, color, lit, sweep } => {
                let Some((inv, edge, soft)) = glyph_xf_params(m, uv, sweep) else { continue };
                (
                    K_GLYPH_XF,
                    none,
                    Inst {
                        dst: r4(b),
                        p0: [o[0], o[1], uv.x as f32, uv.y as f32],
                        c0: color,
                        c1: inv,
                        k: [uv.w as f32, uv.h as f32, edge, soft],
                        c2: lit,
                    },
                )
            }
            Prim::Image { rect, image, uv, radius, opacity, hdr } => (
                K_IMAGE,
                (if hdr { Tex::ImageHdr(image) } else { Tex::Image(image) }, Tex::None),
                Inst { dst: r4(b), p0: r4(rect), c0: uv, k: [radius, 0.0, 0.0, opacity], ..Default::default() },
            ),
            Prim::Effect { rect, image, effect, params, opacity } => (
                K_EFFECT + effect,
                (Tex::Image(image), Tex::None),
                Inst { dst: r4(b), p0: r4(rect), c0: params, k: [0.0, 0.0, 0.0, opacity], ..Default::default() },
            ),
            Prim::Layer { dst: dd, layer, src, opacity, fade } => {
                let w = fade.map_or(0.0, |f| f.1);
                (
                    K_LAYER,
                    (Tex::Layer(layer), fade.map_or(Tex::None, |f| Tex::Layer(f.0))),
                    Inst {
                        dst: r4(b),
                        p0: [src.0 as f32, src.1 as f32, w, opacity],
                        c0: [dd.x as f32, dd.y as f32, 0.0, 0.0],
                        k: [0.0, 0.0, 0.0, fade.is_some() as i32 as f32],
                        ..Default::default()
                    },
                )
            }
        };
        push(kind, tex, i, out, batches);
    }
}

/// A `Prim::GlyphXf`'s inverse map (target to glyph pixel), its karaoke edge in glyph pixels
/// and the edge's softness (`text::glyph_swept`: before the band lit, past it unlit); none when the
/// map is degenerate. Shared by the GPU encoding and the software renderer.
pub fn glyph_xf_params(m: [f32; 4], uv: Rect, sweep: f32) -> Option<([f32; 4], f32, f32)> {
    let det = m[0] * m[3] - m[2] * m[1];
    if det.abs() < 1e-6 {
        return None;
    }
    let inv = [m[3] / det, -m[1] / det, -m[2] / det, m[0] / det];
    let w = uv.w as f32;
    let soft = (w * crate::text::SWEEP_SOFT).max(2.0);
    let edge = if sweep <= 0.0 {
        -1.0e4
    } else if sweep >= 1.0 {
        1.0e4
    } else {
        sweep * (w + soft)
    };
    Some((inv, edge, soft))
}
