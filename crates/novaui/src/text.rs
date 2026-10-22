//! Text: a font fallback chain (per grapheme), shaping (`shape`: runs by face, script and
//! direction, bidi, break opportunities), greedy line breaking at UAX #14's opportunities with
//! max lines and an ellipsis, lines in visual order, a cache of laid-out paragraphs, a paged A8
//! glyph atlas (LRU pages), and icons from `nova-icons` in the same atlas.

mod shape;

use crate::geom::*;
use crate::render::primitive::Prim;
use crate::style::{Sweep, Units};
use ab_glyph::VariableFont;
use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont};
use nova_icons::Icon;
/// Measurements keyed by short strings: a fast, per-process seeded hash.
type HashMap<K, V> = std::collections::HashMap<K, V, foldhash::fast::RandomState>;
use std::rc::Rc;

pub const ATLAS: i32 = 2048;
const RUN_CAP: usize = 2048;
/// A cluster's family and face (`pick_cluster`).
type Pick = (u8, u8);

/// Most (char, weight) face picks kept (`pick_cluster`); past it they start over.
const PICK_CAP: usize = 8192;
/// The atlas is cut into `PAGES` bands `PAGE_H` rows tall, each shelf-packed on its own. When it
/// is full the page used longest ago goes (`evict`), not the whole atlas.
const PAGE_H: i32 = 128;
const PAGES: usize = (ATLAS / PAGE_H) as usize;

/// The pages (bits) atlas rows `uv` lies on.
pub(crate) fn page_mask(uv: Rect) -> u64 {
    if uv.h <= 0 {
        return 0;
    }
    let (a, b) = ((uv.y / PAGE_H) as u32, ((uv.y + uv.h - 1) / PAGE_H) as u32);
    (u64::MAX >> (63 - b)) & (u64::MAX << a)
}

/// The pages a page mask names, lowest first.
fn pages(mask: u64) -> impl Iterator<Item = usize> {
    let mut m = mask;
    std::iter::from_fn(move || {
        (m != 0).then(|| {
            let p = m.trailing_zeros() as usize;
            m &= m - 1;
            p
        })
    })
}

/// Where a pen position falls: its whole pixel and its quarter-pixel phase (glyphs are rastered
/// at the phase).
fn pen_cell(x: f32) -> (i32, u8) {
    let cell = (x * PHASES).round() as i32;
    (cell.div_euclid(PHASES as i32), cell.rem_euclid(PHASES as i32) as u8)
}

/// The raster size step of a pixel size (1/`SIZE_STEPS` px).
fn size_step(px: f32) -> u32 {
    (px * SIZE_STEPS).round() as u32
}

/// One band of the atlas: its shelf (pen `x`, shelf top `y` and height `h`, in the band), the
/// frame it was last used in, and how many elements show glyphs from it.
#[derive(Clone, Copy)]
struct Page {
    x: i32,
    y: i32,
    h: i32,
    used: bool,
    stamp: u64,
    refs: u32,
}

impl Page {
    const EMPTY: Page = Page { x: 1, y: 1, h: 0, used: false, stamp: 0, refs: 0 };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Align {
    Start,
    Center,
    End,
}

/// (icon, pixel size, optional cut-out icon and its inset).
type IconKey = (Icon, u32, Option<(Icon, u8)>);

/// What a paragraph is laid out with. Sizes are physical pixels (already scaled by DPI).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub size: f32,
    pub line: f32,
    pub weight: u16,
}

impl TextStyle {
    /// The same style, its size and line times `f`.
    pub(crate) fn scaled(self, f: f32) -> Self {
        TextStyle { size: self.size * f, line: self.line * f, weight: self.weight }
    }
}

/// A two-generation cache by (key, string): a lookup borrows the string (no allocation); an
/// entry not used since the last roll-over goes at the next one.
struct StrCache<K, V> {
    cur: HashMap<K, HashMap<String, V>>,
    old: HashMap<K, HashMap<String, V>>,
    n: usize,
    n_old: usize,
}

impl<K: Copy + Eq + std::hash::Hash, V: Clone> StrCache<K, V> {
    fn new() -> Self {
        StrCache { cur: HashMap::default(), old: HashMap::default(), n: 0, n_old: 0 }
    }
    fn len(&self) -> usize {
        self.n + self.n_old
    }
    fn clear(&mut self) {
        self.cur.clear();
        self.old.clear();
        (self.n, self.n_old) = (0, 0);
    }
    fn get(&mut self, key: K, s: &str) -> Option<V> {
        if let Some(v) = self.cur.get(&key).and_then(|m| m.get(s)) {
            return Some(v.clone());
        }
        let (s, v) = self.old.get_mut(&key)?.remove_entry(s)?;
        self.n_old -= 1;
        self.insert(key, s, v.clone());
        Some(v)
    }
    /// Keeps the entries `f` holds to.
    fn retain(&mut self, f: impl Fn(&V) -> bool) {
        for (gen, n) in [(&mut self.cur, &mut self.n), (&mut self.old, &mut self.n_old)] {
            for m in gen.values_mut() {
                let before = m.len();
                m.retain(|_, v| f(v));
                *n -= before - m.len();
            }
        }
    }
    fn insert(&mut self, key: K, s: String, v: V) {
        if self.n >= RUN_CAP {
            self.old = std::mem::take(&mut self.cur);
            (self.n_old, self.n) = (self.n, 0);
        }
        if self.cur.entry(key).or_default().insert(s, v).is_none() {
            self.n += 1;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    size: u32,
    line: u32,
    weight: u16,
    width: i32,
    lines: u16,
    align: Align,
}

impl Key {
    /// What makes two layouts or measures the same: the style (size and line to 1/64 px, the
    /// weight), the width, the line limit and the alignment.
    fn new(style: &TextStyle, width: i32, lines: u16, align: Align) -> Self {
        Key {
            size: (style.size * 64.0) as u32,
            line: (style.line * 64.0) as u32,
            weight: style.weight,
            width,
            lines,
            align,
        }
    }
}

/// A glyph slot's key (family, face, glyph, size step, phase) and a halo's radius.
type GlowKey = ((u8, u8, u32, u32, u8), u8);

#[derive(Clone, Copy)]
struct Slot {
    uv: Rect,
    bx: i32,
    by: i32,
}

/// Intrinsic dimensions, independent of glyph atlas contents and alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measurement {
    pub ink_w: i32,
    pub h: i32,
    /// Lines it breaks into.
    pub lines: usize,
}

/// A laid-out paragraph: glyphs relative to its top-left, its size, and each line's extent.
pub struct Para {
    pub glyphs: Vec<(i32, i32, Rect)>,
    /// The char (of the text, `\r` not counted) each glyph draws; an ellipsis counts as the
    /// first char it stands for. For a sweep (`Visual::sweep`).
    pub chars: Vec<u32>,
    pub w: i32,
    pub h: i32,
    pub lines: usize,
    /// Width of the widest line, before alignment.
    pub ink_w: i32,
    pub truncated: bool,
    /// What each glyph is, to raster it again at another size (a scaled unit, `Units`).
    src: Vec<GlyphSrc>,
    /// The size laid out, in `SIZE_STEPS` per px.
    q: u32,
    /// The atlas pages its glyphs are on (`page_mask`).
    pages: u64,
}

/// A glyph's font, id, pen (px, from the paragraph's left, its offset included), advance,
/// baseline, how far it is raised off the baseline (px; a mark's offset), and whether its run
/// reads right to left.
#[derive(Clone, Copy)]
struct GlyphSrc {
    fam: u8,
    face: u8,
    id: GlyphId,
    pen: f32,
    adv: f32,
    base: i32,
    rise: f32,
    rtl: bool,
}

/// A glyph whose char is `k` sung (`Sweep::at` less the char: 1 or more lit, 0 or less unlit):
/// the char being sung is cut into columns whose colour moves from lit to unlit over a soft
/// edge (`SWEEP_SOFT` of the glyph's width) that enters at its left as the char starts and
/// leaves at its right as it ends (its left, in a right-to-left run), so the sweep never jumps
/// between glyphs. No backend work: the columns are ordinary glyph primitives over narrower atlas
/// cells.
/// `glyph_swept` for a glyph at (x, y) that `clip` shows; nothing for one it does not.
#[allow(clippy::too_many_arguments)]
fn glyph_swept_in(
    clip: Rect,
    x: i32,
    y: i32,
    uv: Rect,
    unlit: Color,
    lit: Color,
    k: f32,
    rtl: bool,
    out: &mut Vec<Prim>,
) {
    if clip.intersects(&Rect::new(x, y, uv.w, uv.h)) {
        glyph_swept(x, y, uv, unlit, lit, k, rtl, out);
    }
}

#[allow(clippy::too_many_arguments)]
fn glyph_swept(x: i32, y: i32, uv: Rect, unlit: Color, lit: Color, k: f32, rtl: bool, out: &mut Vec<Prim>) {
    if k <= 0.0 || k >= 1.0 || uv.w <= 0 {
        out.push(Prim::Glyph { x, y, uv, color: if k >= 1.0 { lit } else { unlit } });
        return;
    }
    // Lit share of column `col` (centre, counted from where the sweep enters): 1 before the
    // edge, 0 past the soft band.
    let w = uv.w as f32;
    let soft = (w * SWEEP_SOFT).max(2.0);
    let edge = k * (w + soft);
    let lit_at = |col: i32| ((edge - (col as f32 + 0.5)) / soft).clamp(0.0, 1.0);
    // Whole-lit columns [0, a), the soft band [a, b), whole-unlit [b, w), from the entry side.
    let a = ((edge - soft).floor() as i32).clamp(0, uv.w);
    let b = (edge.ceil() as i32).clamp(a, uv.w);
    let strip = |c0: i32, c1: i32, col: Color, out: &mut Vec<Prim>| {
        let (c0, c1) = if rtl { (uv.w - c1, uv.w - c0) } else { (c0, c1) };
        if c1 > c0 {
            let uv = Rect::new(uv.x + c0, uv.y, c1 - c0, uv.h);
            out.push(Prim::Glyph { x: x + c0, y, uv, color: col });
        }
    };
    strip(0, a, lit, out);
    for col in a..b {
        let t = lit_at(col);
        let mix = std::array::from_fn(|i| unlit[i] + (lit[i] - unlit[i]) * t);
        strip(col, col + 1, mix, out);
    }
    strip(b, uv.w, unlit, out);
}

impl Para {
    pub(crate) fn prims(&self, x: i32, y: i32, color: Color, clip: Rect, out: &mut Vec<Prim>) {
        for &(gx, gy, uv) in &self.glyphs {
            let g = Prim::Glyph { x: x + gx, y: y + gy, uv, color };
            if clip.intersects(&g.bounds()) {
                out.push(g);
            }
        }
    }

    /// `prims` with a karaoke sweep: glyphs of chars before `sweep.at` in `sweep.lit`, after it
    /// in `color`, the one being sung blended across its width (`glyph_swept`).
    pub(crate) fn prims_swept(&self, x: i32, y: i32, color: Color, sweep: Sweep, clip: Rect, out: &mut Vec<Prim>) {
        for ((&(gx, gy, uv), &c), s) in self.glyphs.iter().zip(&self.chars).zip(&self.src) {
            glyph_swept_in(clip, x + gx, y + gy, uv, color, sweep.lit, sweep.at - c as f32, s.rtl, out);
        }
    }
}

impl Text {
    /// `Para::prims` (or `prims_swept`) with each unit drawn its own way (`Units`): faded, given
    /// its colour, and, unless at rest, moved by its offset (`k` px per dip), scaled and turned
    /// about its centre (across its glyphs, at the middle of their line's body) as a
    /// `Prim::GlyphXf`: sub-pixel, so a unit glides and settles without stepping a pixel. A
    /// unit at rest is the crisp glyph laid out. Scaled, a glyph is rastered at the nearest
    /// whole-px size (an animated scale adds a few atlas sizes, not one per frame) and the rest
    /// of the scale is the map's. A glowing unit has a halo under every glyph of the text (its
    /// glyph blurred once, `glow_slot`); `turn` (degrees clockwise, about the text's centre plus
    /// an origin in px) turns the whole text after.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prims_units(
        &mut self,
        p: &Para,
        x: i32,
        y: i32,
        color: Color,
        sweep: Option<Sweep>,
        units: &Units,
        turn: (f32, [f32; 2]),
        k: f32,
        clip: Rect,
        out: &mut Vec<Prim>,
    ) {
        let size = p.q as f32 / SIZE_STEPS;
        let still = |f: &crate::style::UnitFx| {
            f.dx == 0.0 && f.dy == 0.0 && f.rotate == 0.0 && f.scale == 1.0 && f.scale_y == 1.0
        };
        // Each unit's horizontal centre, in pen px.
        let mut centre = Vec::new();
        if !units.fx.iter().all(still) {
            let mut ext = vec![(f32::INFINITY, f32::NEG_INFINITY); units.fx.len()];
            for (s, &c) in p.src.iter().zip(&p.chars) {
                if let Some(u) = units.of(c) {
                    ext[u] = (ext[u].0.min(s.pen), ext[u].1.max(s.pen + s.adv));
                }
            }
            centre = ext.into_iter().map(|(a, b)| (a + b) * 0.5).collect();
        }
        // The text's turn about its centre and the origin (px from it).
        let (turn, origin) = turn;
        let (ts, tc) = turn.to_radians().sin_cos();
        let tm = [tc, ts, -ts, tc];
        let (tx, ty) = (x as f32 + p.w as f32 * 0.5 + origin[0], y as f32 + p.h as f32 * 0.5 + origin[1]);
        let turned = turn != 0.0;
        let place = |o: [f32; 2], m: [f32; 4]| -> ([f32; 2], [f32; 4]) {
            if !turned {
                return (o, m);
            }
            let (rx, ry) = (o[0] - tx, o[1] - ty);
            (
                [tx + tm[0] * rx + tm[2] * ry, ty + tm[1] * rx + tm[3] * ry],
                [
                    tm[0] * m[0] + tm[2] * m[1],
                    tm[1] * m[0] + tm[3] * m[1],
                    tm[0] * m[2] + tm[2] * m[3],
                    tm[1] * m[2] + tm[3] * m[3],
                ],
            )
        };
        // Halos go under every glyph of the text.
        let (mut halos, start) = (Vec::new(), out.len());
        for (j, (&(gx, gy, uv), &c)) in p.glyphs.iter().zip(&p.chars).enumerate() {
            let u = units.of(c);
            let fx = u.map(|u| units.fx[u]).unwrap_or_default();
            let mut unlit = color;
            if let Some(f) = fx.fg {
                unlit[..3].copy_from_slice(&f[..3]);
            }
            unlit[3] *= fx.opacity;
            let mut lit = sweep.map_or(unlit, |s| s.lit);
            lit[3] = sweep.map_or(unlit[3], |s| s.lit[3] * fx.opacity);
            let ks = sweep.map_or(0.0, |s| s.at - c as f32);
            let glow = (fx.glow * k).round() as i32;
            let glowing = glow > 0 && fx.glow_fg[3] * unlit[3] > 1.0 / 255.0;
            let moved = u.is_some() && !still(&fx);
            let s = p.src[j];
            if !moved && !turned && !glowing {
                glyph_swept_in(clip, x + gx, y + gy, uv, unlit, lit, ks, s.rtl, out);
                continue;
            }
            // The raster: as laid out, or at the nearest whole-px size to the larger scale about
            // the unit's centre (where this glyph's line has its body); `key`: its slot.
            let body = s.base as f32 - size * 0.35;
            let px = if moved { (size * fx.scale.max(fx.scale_y)).round().max(1.0) } else { size };
            let r = px / size;
            let q = if px == size { p.q } else { size_step(px) };
            let cu = u.filter(|_| moved).map_or(s.pen, |u| centre[u]);
            let pen = cu + (s.pen - cu) * r;
            let (whole, phase) = pen_cell(pen);
            let (gpos, uv) = if px == size {
                (((x + gx) as f32, (y + gy) as f32), uv)
            } else {
                let Some(slot) = self.slot(s.fam, s.face, s.id, q, phase) else { continue };
                let base = body + (s.base as f32 - s.rise - body) * r;
                (((x + whole + slot.bx) as f32, y as f32 + base + slot.by as f32), slot.uv)
            };
            // The rest of the scale, then the unit's turn (clockwise), about its centre; then
            // the move; then the text's turn.
            let (o, m) = if moved {
                let (cx, cy) = (x as f32 + cu, y as f32 + body);
                let (sx, sy) = (fx.scale / r, fx.scale_y / r);
                let (sin, cos) = fx.rotate.to_radians().sin_cos();
                let m = [cos * sx, sin * sx, -sin * sy, cos * sy];
                let (rx, ry) = (gpos.0 - cx, gpos.1 - cy);
                ([cx + fx.dx * k + m[0] * rx + m[2] * ry, cy + fx.dy * k + m[1] * rx + m[3] * ry], m)
            } else {
                ([gpos.0, gpos.1], [1.0, 0.0, 0.0, 1.0])
            };
            let (o, m) = place(o, m);
            if glowing {
                if let Some(h) = self.glow_slot(s.fam, s.face, s.id, q, phase, glow.min(32)) {
                    let g = glow.min(32) as f32;
                    let ho = [o[0] - (m[0] + m[2]) * g, o[1] - (m[1] + m[3]) * g];
                    let mut hc = fx.glow_fg;
                    hc[3] *= unlit[3];
                    let halo = Prim::GlyphXf { o: ho, m, uv: h.uv, color: hc, lit: hc, sweep: 0.0 };
                    if clip.intersects(&halo.bounds()) {
                        halos.push(halo);
                    }
                }
            }
            if !moved && !turned {
                glyph_swept_in(clip, gpos.0 as i32, gpos.1 as i32, uv, unlit, lit, ks, s.rtl, out);
                continue;
            }
            let g = Prim::GlyphXf { o, m, uv, color: unlit, lit, sweep: ks };
            if clip.intersects(&g.bounds()) {
                out.push(g);
            }
        }
        if !halos.is_empty() {
            out.splice(start..start, halos);
        }
    }

    /// A glyph's slot blurred by `r` px (two box passes each way, about a gaussian of radius
    /// `r`), its cell grown by `r` on every side: a halo (`prims_units`), rastered once.
    #[allow(clippy::too_many_arguments)]
    fn glow_slot(&mut self, fam: u8, face: u8, id: GlyphId, q: u32, phase: u8, r: i32) -> Option<Slot> {
        let key: GlowKey = ((fam, face, id.0 as u32, q, phase), r as u8);
        if let Some(&s) = self.glows.get(&key) {
            return self.hit(s);
        }
        let base = self.slot(fam, face, id, q, phase)?;
        let (w, h) = (base.uv.w + 2 * r, base.uv.h + 2 * r);
        let mut buf = vec![0f32; (w * h) as usize];
        for yy in 0..base.uv.h {
            for xx in 0..base.uv.w {
                let v = self.atlas[((base.uv.y + yy) * ATLAS + base.uv.x + xx) as usize];
                buf[((yy + r) * w + xx + r) as usize] = v as f32;
            }
        }
        // Box blur of radius b, twice each way.
        let b = (r / 2).max(1);
        let mut tmp = vec![0f32; buf.len()];
        let pass = |src: &[f32], dst: &mut [f32], horizontal: bool| {
            let (n, lines) = if horizontal { (w, h) } else { (h, w) };
            let at = |line: i32, i: i32| if horizontal { (line * w + i) as usize } else { (i * w + line) as usize };
            for line in 0..lines {
                let mut sum = 0.0;
                for i in -b..=b {
                    if (0..n).contains(&i) {
                        sum += src[at(line, i)];
                    }
                }
                for i in 0..n {
                    dst[at(line, i)] = sum / (2 * b + 1) as f32;
                    let (add, sub) = (i + b + 1, i - b);
                    if add < n {
                        sum += src[at(line, add)];
                    }
                    if sub >= 0 {
                        sum -= src[at(line, sub)];
                    }
                }
            }
        };
        for horizontal in [true, true, false, false] {
            pass(&buf, &mut tmp, horizontal);
            std::mem::swap(&mut buf, &mut tmp);
        }
        let slot = self.alloc(w, h).map(|(ox, oy)| {
            for yy in 0..h {
                for xx in 0..w {
                    // Brighter than the glyph's average: a halo, not a shadow.
                    let v = (buf[(yy * w + xx) as usize] * 2.0).min(255.0);
                    self.atlas[((oy + yy) * ATLAS + ox + xx) as usize] = (v + 0.5) as u8;
                }
            }
            let uv = Rect::new(ox, oy, w, h);
            self.wrote(uv);
            Slot { uv, bx: base.bx - r, by: base.by - r }
        });
        self.glows.insert(key, slot);
        slot
    }
}

/// A text's units: its words (`words`) or its chars, each with the time it is sung from and
/// to. `timing` is spans `(first char, from, to)` in order, each lasting to the next one's first
/// char (chars before the first belong to it); none: one span at 0. Words are cut where lines
/// break (at spaces, around a CJK char, never before closing punctuation); chars are every char
/// but a space. A span cut into several units shares its time out by its chars (spaces take none). Chars count as in
/// `Para::chars` (`\r` not counted).
pub fn units(text: &str, words: bool, timing: &[(u32, f32, f32)]) -> Vec<(std::ops::Range<u32>, f32, f32)> {
    let cs: Vec<char> = text.chars().filter(|c| *c != '\r').collect();
    let n = cs.len() as u32;
    let mut spans: Vec<(u32, u32, f32, f32)> = Vec::with_capacity(timing.len().max(1));
    if timing.is_empty() {
        spans.push((0, n, 0.0, 0.0));
    }
    for (i, &(a, t0, t1)) in timing.iter().enumerate() {
        let a = if i == 0 { 0 } else { a.min(n) };
        let b = timing.get(i + 1).map_or(n, |s| s.0.min(n)).max(a);
        spans.push((a, b, t0, t1));
    }
    let space = |c: char| c.is_whitespace();
    let mut out = Vec::new();
    for (a, b, t0, t1) in spans {
        // Time goes to the span's chars that are not spaces.
        let sung = |c: u32| cs[a as usize..c as usize].iter().filter(|c| !space(**c)).count() as f32;
        let len = sung(b).max(1.0);
        let at = |c: u32| t0 + (t1 - t0) * sung(c) / len;
        let mut c = a;
        while c < b {
            if space(cs[c as usize]) {
                c += 1;
                continue;
            }
            let mut e = c + 1;
            if words && !is_cjk(cs[c as usize]) {
                while e < b && !space(cs[e as usize]) && !is_cjk(cs[e as usize]) {
                    e += 1;
                }
            }
            // Closing punctuation stays with what it closes.
            while e < b && no_break_before(cs[e as usize]) {
                e += 1;
            }
            out.push((c..e, at(c), at(e)));
            c = e;
        }
    }
    out
}

/// One instance of a font: rasterised through `font` (ab_glyph), shaped through `shaper`
/// (harfrust), both over the same mapped file at the same weight.
struct Face {
    font: FontRef<'static>,
    shaper: harfrust::Font,
    /// For variable faces: the weight this instance is set to.
    weight: u16,
    /// Unscaled ascent and descent and units per em, read once: in a variable face ttf_parser
    /// recomputes them from the variation tables on every call.
    ascent: f32,
    descent: f32,
    upem: f32,
}

impl Face {
    /// Face `index` of the file `data`; at `weight` when it is variable (`Some`), else as it is.
    fn new(data: &'static [u8], index: u32, variable: Option<u16>, weight: u16) -> Option<Face> {
        let mut font = FontRef::try_from_slice_and_index(data, index).ok()?;
        let mut shaper = harfrust::Font::new(data, index)?;
        if let Some(w) = variable {
            font.set_variation(b"wght", w as f32);
            shaper = shaper.instance_builder().variations([("wght", w as f32)]).build();
        }
        let (ascent, descent) = (font.ascent_unscaled(), font.descent_unscaled());
        let upem = font.units_per_em().unwrap_or(1000.0);
        Some(Face { font, shaper, weight, ascent, descent, upem })
    }

    /// `em(font, size)` from the kept metrics.
    fn em(&self, size: f32) -> PxScale {
        PxScale::from(size * (self.ascent - self.descent) / self.upem)
    }

    /// Scaled (ascent, descent) at em size `size`.
    fn ascent_descent(&self, size: f32) -> (f32, f32) {
        let v = self.em(size).y / (self.ascent - self.descent);
        (v * self.ascent, v * self.descent)
    }
}

/// A font's measures at one size (px, or dips where `Ui::font_metrics` gives them): `ascent`
/// above the baseline, `descent` below it (negative), the capitals' height (`H`'s ink) and the
/// lowercase's (`x`'s).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontMetrics {
    pub ascent: f32,
    pub descent: f32,
    pub cap_height: f32,
    pub x_height: f32,
}

impl FontMetrics {
    /// How far below the middle of a line box the middle of the capitals stands, the text set in
    /// it as a line is (centred by ascent and descent): where a symbol beside the text centres.
    /// The capitals' middle is Apple's rule for SF Symbols; for Chinese it is within 2% of an em
    /// of the ideographs' middle, so a symbol stands level with either.
    pub fn cap_middle(&self) -> f32 {
        (self.ascent + self.descent) / 2.0 - self.cap_height / 2.0
    }
}

/// A family: faces by weight (nearest is used).
struct Family {
    faces: Vec<Face>,
}

impl Family {
    fn face(&self, weight: u16) -> usize {
        let mut best = 0;
        for (i, f) in self.faces.iter().enumerate() {
            if f.weight.abs_diff(weight) < self.faces[best].weight.abs_diff(weight) {
                best = i;
            }
        }
        best
    }
}

/// A sweep's soft edge, as a share of the sung glyph's width.
pub(crate) const SWEEP_SOFT: f32 = 0.6;

/// Raster sizes per pixel, and pen positions per pixel, glyphs are kept at.
const SIZE_STEPS: f32 = 4.0;
const PHASES: f32 = 4.0;

/// Coverage as blended: lifted a little (gamma 1/1.25). Blending linear coverage in sRGB thins
/// light text on a dark ground (its antialiased edge reads darker than it is), and makes stem
/// weight swing with where the stem falls on the grid; the lift evens both.
fn coverage(v: f32) -> f32 {
    v.clamp(0.0, 1.0).powf(0.8)
}

/// The glyph's outline with its bounds taken from its curves. A variable font's glyph keeps the
/// default instance's bounding box while its curves move with the weight: at 700 an edge of
/// Noto Sans' `y` lies a fraction left of that box, the rasterizer drops what falls outside, and
/// every row then accumulates a constant -13 % coverage: a grey box round the glyph, its ink dim.
fn outline_bounds(o: &ab_glyph::Outline) -> Option<ab_glyph::Rect> {
    use ab_glyph::OutlineCurve::*;
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for c in &o.curves {
        let pts = match c {
            Line(a, b) => [*a, *b, *b, *b],
            Quad(a, b, c) => [*a, *b, *c, *c],
            Cubic(a, b, c, d) => [*a, *b, *c, *d],
        };
        for p in pts {
            (x0, x1, y0, y1) = (x0.min(p.x), x1.max(p.x), y0.min(p.y), y1.max(p.y));
        }
    }
    (x0 < x1 && y0 < y1).then(|| ab_glyph::Rect {
        // Outline bounds are unscaled, y up: min is (left, top), max is (right, bottom).
        min: ab_glyph::point(x0, y1),
        max: ab_glyph::point(x1, y0),
    })
}

fn outline_exact(font: &FontRef<'static>, glyph: ab_glyph::Glyph) -> Option<ab_glyph::OutlinedGlyph> {
    let mut o = font.outline(glyph.id)?;
    if let Some(bounds) = outline_bounds(&o) {
        o.bounds = bounds;
    }
    let sf = font.as_scaled(glyph.scale).scale_factor();
    Some(ab_glyph::OutlinedGlyph::new(glyph, o, sf))
}

/// ab_glyph's `PxScale` is the font's ascent-to-descent height; CSS/WinUI sizes are em sizes.
/// `Face::em` is the one the text uses (from its kept metrics); this one reads them from a bare
/// font, for the tests.
#[cfg(test)]
fn em(f: &FontRef<'static>, size: f32) -> PxScale {
    PxScale::from(size * f.height_unscaled() / f.units_per_em().unwrap_or(1000.0))
}

/// A read-only view of a whole file, mapped for the life of the process. Font files are large
/// (Microsoft YaHei is 20 MB) and only a few glyphs of them are ever read: mapped pages are
/// file-backed and shared, load on first touch, and are not the process's private memory.
#[cfg(windows)]
fn map_file(path: &str) -> Option<&'static [u8]> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use windows::core::HSTRING;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::*;
    use windows::Win32::System::Memory::*;
    static MAPS: Mutex<Option<HashMap<String, (usize, usize)>>> = Mutex::new(None);
    let mut maps = MAPS.lock().ok()?;
    let maps = maps.get_or_insert_with(HashMap::new);
    if let Some(&(p, n)) = maps.get(path) {
        // SAFETY: a view mapped below, `n` bytes, never unmapped.
        return Some(unsafe { std::slice::from_raw_parts(p as *const u8, n) });
    }
    // SAFETY: the path is a live HSTRING for the call; the handles are closed once each; the view
    // covers the whole file at the size read from the same handle, which (FILE_SHARE_READ only)
    // no writer holds while it is open, and Windows refuses to shrink a file with a mapped view,
    // so all `len` bytes stay readable for the life of the process (the view is never unmapped).
    unsafe {
        let f = CreateFileW(
            &HSTRING::from(path),
            FILE_GENERIC_READ.0,
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .ok()?;
        let mut size = 0i64;
        let sized = GetFileSizeEx(f, &mut size).is_ok();
        let len = size as usize;
        if !sized || len == 0 {
            let _ = CloseHandle(f);
            return None;
        }
        let m = CreateFileMappingW(f, None, PAGE_READONLY, 0, 0, None);
        let _ = CloseHandle(f);
        let m = m.ok()?;
        let v = MapViewOfFile(m, FILE_MAP_READ, 0, 0, 0);
        let _ = CloseHandle(m);
        if v.Value.is_null() {
            return None;
        }
        maps.insert(path.to_string(), (v.Value as usize, len));
        Some(std::slice::from_raw_parts(v.Value as *const u8, len))
    }
}

#[cfg(unix)]
fn map_file(path: &str) -> Option<&'static [u8]> {
    use std::collections::HashMap;
    use std::os::unix::io::AsRawFd;
    use std::sync::Mutex;
    static MAPS: Mutex<Option<HashMap<String, (usize, usize)>>> = Mutex::new(None);
    let mut maps = MAPS.lock().ok()?;
    let maps = maps.get_or_insert_with(HashMap::new);
    if let Some(&(p, n)) = maps.get(path) {
        // SAFETY: a mapping made below, `n` bytes, never unmapped.
        return Some(unsafe { std::slice::from_raw_parts(p as *const u8, n) });
    }
    let f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len() as usize;
    if len == 0 {
        return None;
    }
    // The mapping outlives the descriptor; pages stay file-backed and shared.
    // SAFETY: a fresh read-only private mapping of `len` bytes of an open descriptor; the kernel
    // chooses the address.
    let p = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, f.as_raw_fd(), 0) };
    if p == libc::MAP_FAILED {
        return None;
    }
    maps.insert(path.to_string(), (p as usize, len));
    // SAFETY: the mapping just made, `len` bytes, never unmapped. Linux cannot stop another
    // process truncating the file (a read past the new end would fault); like every font stack
    // that maps its files, this relies on font files not being rewritten in place.
    Some(unsafe { std::slice::from_raw_parts(p as *const u8, len) })
}

fn load(path: &str, index: u32, variable: Option<u16>, weight: u16) -> Option<Face> {
    Face::new(map_file(path)?, index, variable, weight)
}

/// The instances a variable family is kept at: every 50 from 400 to 700, so a weight that moves
/// with the window (a heading lighter as it grows) has a face of its own; a text asks for one of
/// these, the nearest is used otherwise.
pub const WEIGHTS: [u16; 7] = [400, 450, 500, 550, 600, 650, 700];

fn load_variable(path: &str, weights: &[u16]) -> Vec<Face> {
    weights.iter().filter_map(|&w| load(path, 0, Some(w), w)).collect()
}

/// A rasterized vector shape: its tile and the ink box within it.
type ShapeTile = (Slot, [f32; 4]);

/// Work a `Text` did since it was made (diagnostics, tests).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextCounts {
    /// Glyphs, halos, icons and shapes rasterized into the atlas.
    pub rasterized: u64,
    /// Paragraphs laid out (cache misses).
    pub laid_out: u64,
    /// Intrinsic measurements made (cache misses).
    pub measured: u64,
}

pub struct Text {
    families: Vec<Family>,
    /// The family chosen over the default ones (`set_font`), first in the fallback order.
    font: Option<String>,
    pub(crate) atlas: Vec<u8>,
    slots: HashMap<(u8, u8, u32, u32, u8), Option<Slot>>,
    /// Glyph halos (`glow_slot`): a slot's key and the blur's radius.
    glows: HashMap<GlowKey, Option<Slot>>,
    icons: HashMap<IconKey, Option<Slot>>,
    /// Vector shapes by (digest, px).
    shapes: HashMap<(u64, u32), Option<ShapeTile>>,
    /// The ink bounds and tile origin of each knocked-out mark, as rasterized.
    marks: HashMap<IconKey, ([f32; 4], [i32; 2])>,
    runs: StrCache<Key, Rc<Para>>,
    measures: StrCache<Key, Measurement>,
    /// Texts shaped, by weight (`shaped`): in ems, for every size and width.
    shaped: StrCache<u16, Rc<shape::Shaped>>,
    shaper: shape::Shaper,
    /// Largest glyph extent in ems per text/weight, independent of size, wrapping and atlas UVs.
    ink_spans: HashMap<u16, HashMap<String, f32>>,
    ink_count: usize,
    /// Reuse curve extents across different strings, including when the text cache rolls over.
    ink_glyphs: HashMap<(u8, u8, u16), f32>,
    /// The face each (char, weight) falls back to (`pick_cluster`).
    picks: std::cell::RefCell<HashMap<(char, u16), Pick>>,
    pages: [Page; PAGES],
    /// The page being filled.
    cur: usize,
    /// Frames since start (`between_frames`): a page used in this one is not evicted.
    frame: u64,
    /// Pages evicted while elements showed glyphs from them (`take_stale`).
    stale: u64,
    /// Pages evicted since start (diagnostics).
    #[cfg(test)]
    pub(crate) evictions: u64,
    /// Atlas regions written since the backend last took them (`take_uploads`).
    uploads: Vec<Rect>,
    /// Set when the atlas was reset: every cached paragraph and every text element is stale.
    pub(crate) reset: bool,
    /// Resets since start (diagnostics).
    #[cfg(test)]
    pub(crate) resets: u64,
    pub(crate) stats_rasterized: u64,
    pub(crate) stats_laid_out: u64,
    pub(crate) stats_measured: u64,
}

pub fn is_cjk_char(c: char) -> bool {
    is_cjk(c)
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF | 0x2E80..=0x2FDF | 0x3000..=0x30FF | 0x3100..=0x31FF | 0x3200..=0x9FFF |
        0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
}

/// `s` less its `\r`s (a text is laid out, and its chars counted, without them).
fn without_cr(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains('\r') {
        s.replace('\r', "").into()
    } else {
        s.into()
    }
}

/// Family and face for the grapheme `g` at `weight`: the first family with a glyph for each of
/// its chars (default-ignorable ones aside), else the first with one for its first char.
/// Memoized per (char, weight) for a grapheme of one char: the fallback walk looks up every
/// family's cmap until one has it.
/// The first family's face nearest `weight`: what a line is laid out by, and the fallback a
/// cluster no family has is drawn with.
fn primary_pick(families: &[Family], weight: u16) -> Pick {
    (0, families[0].face(weight) as u8)
}

fn pick_cluster(
    families: &[Family],
    picks: &std::cell::RefCell<HashMap<(char, u16), Pick>>,
    g: &str,
    weight: u16,
) -> Pick {
    let mut cs = g.chars();
    let Some(c) = cs.next() else { return primary_pick(families, weight) };
    let single = cs.next().is_none();
    if single {
        if let Some(&hit) = picks.borrow().get(&(c, weight)) {
            return hit;
        }
    }
    let has = |fam: &Family, c: char| fam.faces[fam.face(weight)].font.glyph_id(c).0 != 0;
    let all = |fam: &Family| g.chars().all(|c| has(fam, c) || shape::ignorable(c));
    let hit = families
        .iter()
        .position(all)
        .or_else(|| families.iter().position(|f| has(f, c)))
        .map_or(primary_pick(families, weight), |i| (i as u8, families[i].face(weight) as u8));
    if single {
        let mut picks = picks.borrow_mut();
        if picks.len() >= PICK_CAP {
            picks.clear();
        }
        picks.insert((c, weight), hit);
    }
    hit
}

/// Characters a line may not start with (closing punctuation, CJK kinsoku subset).
fn no_break_before(c: char) -> bool {
    matches!(
        c,
        '，' | '。'
            | '、'
            | '；'
            | '：'
            | '！'
            | '？'
            | '）'
            | '」'
            | '』'
            | '】'
            | '》'
            | '〉'
            | ','
            | '.'
            | ';'
            | ':'
            | '!'
            | '?'
            | ')'
            | ']'
            | '}'
            | '…'
            | '・'
            | 'ー'
            | 'ゃ'
            | 'ゅ'
            | 'ょ'
            | 'っ'
            | 'ャ'
            | 'ュ'
            | 'ョ'
            | 'ッ'
    )
}

/// The fonts shipped beside the program (`fonts/` next to the executable, or `NOVAUI_FONTS`):
/// Noto Sans (Latin), then Noto Sans SC / JP / KR, each one variable file giving every weight;
/// then those of `BUNDLED_SCRIPTS` that are there. One design across scripts and platforms, and
/// every script has its bold (the system fallbacks had a regular face only for Korean). Empty
/// when the first four are not there: the system's fonts then.
fn bundled_families() -> Vec<Family> {
    let dir = std::env::var_os("NOVAUI_FONTS")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("fonts"))));
    let Some(dir) = dir else { return Vec::new() };
    load_bundled(&dir)
}

fn load_bundled(dir: &std::path::Path) -> Vec<Family> {
    let mut families = Vec::new();
    for name in ["NotoSans-VF.ttf", "NotoSansSC-VF.ttf", "NotoSansJP-VF.ttf", "NotoSansKR-VF.ttf"] {
        let faces = load_variable(&dir.join(name).to_string_lossy(), &WEIGHTS);
        if faces.is_empty() {
            return Vec::new();
        }
        families.push(Family { faces });
    }
    for name in BUNDLED_SCRIPTS {
        let faces = load_variable(&dir.join(name).to_string_lossy(), &WEIGHTS);
        if !faces.is_empty() {
            families.push(Family { faces });
        }
    }
    families
}

/// The bundle's fonts for scripts the first four lack, each used when it is there.
const BUNDLED_SCRIPTS: [&str; 4] =
    ["NotoSansArabic-VF.ttf", "NotoSansHebrew-VF.ttf", "NotoSansDevanagari-VF.ttf", "NotoSansThai-VF.ttf"];

/// An installed family's upright faces of normal width: a variable one at `WEIGHTS`, static ones
/// at their weights (the first file of a weight).
fn scanned_family(name: &str) -> Family {
    let mut faces: Vec<Face> = Vec::new();
    for f in crate::font_scan::scanned().iter().filter(|f| f.family == name && !f.italic && f.normal_width) {
        let path = f.path.to_string_lossy();
        if f.variable {
            for w in WEIGHTS {
                if faces.iter().any(|x| x.weight == w) {
                    continue;
                }
                faces.extend(load(&path, f.index, Some(w), w));
            }
        } else if !faces.iter().any(|x| x.weight == f.weight) {
            faces.extend(load(&path, f.index, None, f.weight));
        }
    }
    faces.sort_by_key(|f| f.weight);
    Family { faces }
}

/// The system's fonts the chain is made of, by role, each a list of family names in order of
/// preference (Windows' and the common Linux distributions' names together: a family that is not
/// installed is skipped). Found by name through `font_scan`, not by file path, so any system that
/// has the family has it wherever its files are (NixOS, Flatpak, a user's own folder).
const LATIN: &[&str] = &["Segoe UI Variable", "Segoe UI", "Noto Sans", "DejaVu Sans", "Liberation Sans"];
/// Simplified Chinese first (the app's second language), then the CJK families of distributions.
const CJK: &[&str] = &[
    "Microsoft YaHei UI",
    "Microsoft YaHei",
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "Source Han Sans SC",
    "WenQuanYi Zen Hei",
    "WenQuanYi Micro Hei",
    "Droid Sans Fallback",
];
/// After the first two: every one installed, for the scripts and symbols those lack (Segoe UI
/// for Arabic and Hebrew where the first is Segoe UI Variable, which has neither; Nirmala UI for
/// the Indic scripts; Leelawadee UI for Thai; Noto's per-script families on Linux).
const MORE: &[&str] = &[
    "Yu Gothic",
    "Malgun Gothic",
    "Segoe UI",
    "Segoe UI Symbol",
    "Nirmala UI",
    "Leelawadee UI",
    "Segoe UI Emoji",
    "Noto Sans Arabic",
    "Noto Sans Hebrew",
    "Noto Sans Devanagari",
    "Noto Sans Thai",
    "Noto Sans Symbols",
    "Noto Sans Symbols 2",
    "DejaVu Sans",
];

/// The chain: the bundled Noto when it is there, else the system's Latin and CJK families; then
/// the system's others for what those lack. Empty when no font at all loads.
fn families() -> Vec<Family> {
    let first =
        |names: &[&'static str]| names.iter().map(|&n| (n, scanned_family(n))).find(|(_, f)| !f.faces.is_empty());
    let mut chain = bundled_families();
    let mut used = Vec::new();
    if chain.is_empty() {
        for (name, f) in first(LATIN).into_iter().chain(first(CJK)) {
            used.push(name);
            chain.push(f);
        }
    }
    for &n in MORE.iter().filter(|n| !used.contains(n)) {
        let f = scanned_family(n);
        if !f.faces.is_empty() {
            chain.push(f);
        }
    }
    chain
}

/// Whether any font loads (`platform::run` reports none as an error before a `Tree` exists).
pub(crate) fn check_fonts() -> Result<(), crate::Error> {
    if families().is_empty() {
        return Err(crate::Error::Platform(
            "no usable font: install one (or put the bundle in fonts/ beside the program, or set NOVAUI_FONTS)".into(),
        ));
    }
    Ok(())
}

impl Text {
    /// Entry counts of growable caches (leak checks): slots, icons, runs+old, atlas-independent metrics.
    pub fn sizes(&self) -> [usize; 4] {
        [
            self.slots.len(),
            self.icons.len(),
            self.runs.len(),
            self.measures.len()
                + self.shaped.len()
                + self.ink_count
                + self.ink_glyphs.len()
                + self.picks.borrow().len(),
        ]
    }
    /// The text system with the default chain.
    ///
    /// # Panics
    ///
    /// When no font loads at all; `platform::run` reports that as an `Error` before it makes one.
    pub fn new() -> Self {
        let families = families();
        assert!(!families.is_empty(), "no usable font");
        Text {
            families,
            font: None,
            atlas: vec![0; (ATLAS * ATLAS) as usize],
            slots: HashMap::default(),
            glows: HashMap::default(),
            icons: HashMap::default(),
            shapes: HashMap::default(),
            marks: HashMap::default(),
            runs: StrCache::new(),
            measures: StrCache::new(),
            shaped: StrCache::new(),
            shaper: Default::default(),
            ink_spans: HashMap::default(),
            ink_count: 0,
            ink_glyphs: HashMap::default(),
            picks: Default::default(),
            pages: [Page::EMPTY; PAGES],
            cur: 0,
            frame: 1,
            stale: 0,
            #[cfg(test)]
            evictions: 0,
            uploads: Vec::new(),
            reset: false,
            #[cfg(test)]
            resets: 0,
            stats_rasterized: 0,
            stats_laid_out: 0,
            stats_measured: 0,
        }
    }

    /// Puts an installed family (`font_scan::system_fonts`) first, ahead of the default ones,
    /// which still give what it lacks (CJK in a Latin font, symbols); `None`: the defaults alone.
    /// Every cached glyph, run and measure is dropped. False, and nothing changes, when no
    /// upright face of `family` loads.
    pub(crate) fn set_font(&mut self, family: Option<&str>) -> bool {
        if self.font.as_deref() == family {
            return true;
        }
        let first = match family {
            None => None,
            Some(name) => {
                let fam = scanned_family(name);
                if fam.faces.is_empty() {
                    return false;
                }
                Some(fam)
            }
        };
        let mut families = families();
        families.splice(0..0, first);
        self.families = families;
        self.font = family.map(str::to_owned);
        self.start_over();
        self.measures.clear();
        self.shaped.clear();
        self.shaper.clear();
        self.ink_spans.clear();
        self.ink_count = 0;
        self.ink_glyphs.clear();
        self.picks.get_mut().clear();
        true
    }

    /// The chosen family (`set_font`).
    pub fn font(&self) -> Option<&str> {
        self.font.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn counts(&self) -> TextCounts {
        TextCounts { rasterized: self.stats_rasterized, laid_out: self.stats_laid_out, measured: self.stats_measured }
    }

    /// Atlas pixels written that no backend has taken yet.
    pub fn has_uploads(&self) -> bool {
        !self.uploads.is_empty()
    }

    /// The whole atlas goes to the backend again (new backend, device loss).
    pub(crate) fn reupload_all(&mut self) {
        self.uploads.clear();
        self.uploads.push(Rect::new(0, 0, ATLAS, ATLAS));
    }

    pub(crate) fn take_uploads(&mut self) -> Vec<Rect> {
        std::mem::take(&mut self.uploads)
    }

    /// Conservative glyph overhang beyond the paragraph's local primitive-culling box.
    /// Compute curve bounds once per text/weight; no rasterization or atlas lookup. A glyph
    /// intersecting that box cannot reach farther than its full ink extent outside it.
    pub(crate) fn ink_margin(&mut self, text: &str, weight: u16, size: f32) -> i32 {
        let span = match self.ink_spans.get(&weight).and_then(|m| m.get(text)).copied() {
            Some(span) => span,
            None => {
                let mut span = 0.0_f32;
                // The glyphs the text is shaped into; truncation can append an ellipsis which was
                // not in the input.
                let clean = without_cr(text);
                let (sh, ell) = (self.shaped(&clean, weight), self.shaped("…", weight));
                for g in sh.glyphs.iter().chain(&ell.glyphs) {
                    let (family, face, id) = (g.fam, g.face, GlyphId(g.id));
                    let key = (family, face, id.0);
                    let extent = match self.ink_glyphs.get(&key).copied() {
                        Some(extent) => extent,
                        None => {
                            let kept = &self.families[family as usize].faces[face as usize];
                            let extent =
                                kept.font.outline(id).and_then(|o| outline_bounds(&o)).map_or(0.0, |b| {
                                    (b.max.x - b.min.x).abs().max((b.max.y - b.min.y).abs()) / kept.upem
                                });
                            if self.ink_glyphs.len() >= RUN_CAP {
                                self.ink_glyphs.clear();
                            }
                            self.ink_glyphs.insert(key, extent);
                            extent
                        }
                    };
                    span = span.max(extent);
                }
                if self.ink_count >= RUN_CAP {
                    self.ink_spans.clear();
                    self.ink_count = 0;
                }
                self.ink_spans.entry(weight).or_default().insert(text.to_owned(), span);
                self.ink_count += 1;
                span
            }
        };
        // Raster sizes round to quarter pixels; pixel bounds round both edges outward.
        let size = (size.max(0.0) * SIZE_STEPS).ceil() / SIZE_STEPS;
        (span * size).ceil() as i32 + 2
    }

    /// `t` (no `\r`) shaped at `weight`, from the cache when it was shaped before.
    fn shaped(&mut self, t: &str, weight: u16) -> Rc<shape::Shaped> {
        if let Some(sh) = self.shaped.get(weight, t) {
            return sh;
        }
        let (families, picks) = (&self.families, &self.picks);
        let pick = |g: &str| pick_cluster(families, picks, g, weight);
        let sh = Rc::new(self.shaper.shape(families, t, &pick));
        self.shaped.insert(weight, t.to_owned(), sh.clone());
        sh
    }

    /// Room for a `w` x `h` tile: on the page being filled, else on an empty page, else on the
    /// page used longest ago (evicted). A tile taller than a page takes whole pages. Only when
    /// this frame uses every page does the atlas start over.
    fn alloc(&mut self, w: i32, h: i32) -> Option<(i32, i32)> {
        if w + 2 > ATLAS || h + 2 > ATLAS {
            return None;
        }
        if h + 2 > PAGE_H {
            return Some(self.alloc_tall(h));
        }
        let p = match self.fit(self.cur, w, h) {
            true => self.cur,
            false => {
                let p = match self.pages.iter().position(|p| !p.used) {
                    Some(p) => p,
                    None => match self.oldest(1) {
                        Some(p) => {
                            self.evict(p);
                            p
                        }
                        None => {
                            self.start_over();
                            0
                        }
                    },
                };
                self.cur = p;
                p
            }
        };
        let pg = &mut self.pages[p];
        if pg.x + w + 1 > ATLAS {
            pg.x = 1;
            pg.y += pg.h + 1;
            pg.h = 0;
        }
        let at = (pg.x, p as i32 * PAGE_H + pg.y);
        pg.x += w + 1;
        pg.h = pg.h.max(h);
        pg.used = true;
        pg.stamp = self.frame;
        Some(at)
    }

    /// Whether page `p` has room for `w` x `h` (on its shelf or a new one).
    fn fit(&self, p: usize, w: i32, h: i32) -> bool {
        let pg = &self.pages[p];
        let y = if pg.x + w + 1 > ATLAS { pg.y + pg.h + 1 } else { pg.y };
        y + h < PAGE_H
    }

    /// A tile taller than a page: the run of whole pages used longest ago, emptied and closed to
    /// other tiles.
    fn alloc_tall(&mut self, h: i32) -> (i32, i32) {
        let k = ((h + 2 + PAGE_H - 1) / PAGE_H) as usize;
        let start = self.oldest(k).unwrap_or_else(|| {
            self.start_over();
            0
        });
        for p in start..start + k {
            if self.pages[p].used {
                self.evict(p);
            }
            self.pages[p] = Page { x: ATLAS, y: PAGE_H, used: true, stamp: self.frame, ..self.pages[p] };
        }
        (1, start as i32 * PAGE_H + 1)
    }

    /// The first of `k` pages in a row, none used this frame, that is cheapest to evict: fewest
    /// pages shown by elements (repainted when it goes), then used longest ago.
    fn oldest(&self, k: usize) -> Option<usize> {
        (0..=PAGES - k).filter(|&s| self.pages[s..s + k].iter().all(|p| !p.used || p.stamp < self.frame)).min_by_key(
            |&s| {
                let run = &self.pages[s..s + k];
                let shown = run.iter().filter(|p| p.used && p.refs > 0).count();
                let last = run.iter().filter(|p| p.used).map(|p| p.stamp).max().unwrap_or(0);
                (shown, last)
            },
        )
    }

    /// Empties page `p`: every tile, halo, icon and shape on it and every paragraph using it
    /// goes. Elements that show glyphs from it are stale (`take_stale`).
    fn evict(&mut self, p: usize) {
        let band = Rect::new(0, p as i32 * PAGE_H, ATLAS, PAGE_H);
        let on = |s: &Option<Slot>| s.is_some_and(|s| s.uv.intersects(&band));
        self.slots.retain(|_, s| !on(s));
        self.glows.retain(|_, s| !on(s));
        let marks = &mut self.marks;
        self.icons.retain(|k, s| {
            let gone = on(s);
            if gone {
                marks.remove(k);
            }
            !gone
        });
        self.shapes.retain(|_, s| !s.is_some_and(|(s, _)| s.uv.intersects(&band)));
        let bit = 1u64 << p;
        self.runs.retain(|para| para.pages & bit == 0);
        let rows = (p * PAGE_H as usize * ATLAS as usize)..((p + 1) * PAGE_H as usize * ATLAS as usize);
        self.atlas[rows].fill(0);
        self.uploads.push(band);
        let pg = &mut self.pages[p];
        if pg.refs > 0 {
            self.stale |= bit;
        }
        *pg = Page { refs: pg.refs, stamp: pg.stamp, ..Page::EMPTY };
        #[cfg(test)]
        {
            self.evictions += 1;
        }
    }

    /// The pages `uv` lies on are used this frame.
    fn touch(&mut self, mask: u64) {
        for p in pages(mask) {
            self.pages[p].stamp = self.frame;
        }
    }

    /// A cached tile was hit: its pages are used this frame.
    fn hit(&mut self, s: Option<Slot>) -> Option<Slot> {
        if let Some(s) = s {
            self.touch(page_mask(s.uv));
        }
        s
    }

    /// A tile was written into the atlas at `uv`: queued for upload, and counted.
    fn wrote(&mut self, uv: Rect) {
        self.uploads.push(uv);
        self.stats_rasterized += 1;
    }

    /// An element that showed glyphs from pages `old` now shows them from `new` (bits,
    /// `page_mask`): a page an element shows is evicted last, and that element is then stale.
    pub(crate) fn repage(&mut self, old: u64, new: u64) {
        if old == new {
            return;
        }
        for p in pages(old) {
            self.pages[p].refs = self.pages[p].refs.saturating_sub(1);
        }
        for p in pages(new) {
            self.pages[p].refs += 1;
        }
    }

    /// Pages evicted since the last call while elements showed glyphs from them: those
    /// elements are painted again.
    pub(crate) fn take_stale(&mut self) -> u64 {
        std::mem::take(&mut self.stale)
    }

    pub(crate) fn has_stale(&self) -> bool {
        self.stale != 0
    }

    /// Empties the atlas: every paragraph is laid out again (and every text repainted).
    fn start_over(&mut self) {
        self.atlas.fill(0);
        self.slots.clear();
        self.glows.clear();
        self.icons.clear();
        self.shapes.clear();
        self.marks.clear();
        self.runs.clear();
        for p in &mut self.pages {
            *p = Page { refs: p.refs, ..Page::EMPTY };
        }
        self.cur = 0;
        self.stale = 0;
        self.uploads.clear();
        self.uploads.push(Rect::new(0, 0, ATLAS, ATLAS));
        self.reset = true;
        #[cfg(test)]
        {
            self.resets += 1;
        }
        if std::env::var_os("NOVAUI_TEXT_LOG").is_some() {
            eprintln!("novaui: glyph atlas full, reset (every text is laid out again)");
        }
    }

    /// Called before a frame lays anything out: a new frame, whose pages are kept while it runs.
    /// A resize rasters each text at every size it passes through; the pages holding the sizes
    /// passed are the ones evicted when room runs out.
    pub(crate) fn between_frames(&mut self) {
        self.frame += 1;
    }

    /// The tile of glyph `id` at size `q` / `SIZE_STEPS` px, its pen `phase` / `PHASES` px right
    /// of a pixel edge.
    fn slot(&mut self, fam: u8, face: u8, id: GlyphId, q: u32, phase: u8) -> Option<Slot> {
        let key = (fam, face, id.0 as u32, q, phase);
        if let Some(&s) = self.slots.get(&key) {
            return self.hit(s);
        }
        let kept = &self.families[fam as usize].faces[face as usize];
        let font = &kept.font;
        let at = ab_glyph::point(phase as f32 / PHASES, 0.0);
        let glyph = id.with_scale_and_position(kept.em(q as f32 / SIZE_STEPS), at);
        let slot = outline_exact(font, glyph).and_then(|og| {
            let b = og.px_bounds();
            let (w, h) = (b.width().ceil() as i32, b.height().ceil() as i32);
            if w <= 0 || h <= 0 {
                return None;
            }
            let mut buf = vec![0u8; (w * h) as usize];
            og.draw(|x, y, v| {
                if (x as i32) < w && (y as i32) < h {
                    buf[(y as i32 * w + x as i32) as usize] = (coverage(v) * 255.0 + 0.5) as u8;
                }
            });
            Some((b, w, h, buf))
        });
        let slot = match slot {
            None => None,
            Some((b, w, h, buf)) => {
                let (ox, oy) = self.alloc(w, h)?;
                for y in 0..h {
                    let d = ((oy + y) * ATLAS + ox) as usize;
                    self.atlas[d..d + w as usize].copy_from_slice(&buf[(y * w) as usize..((y + 1) * w) as usize]);
                }
                let uv = Rect::new(ox, oy, w, h);
                self.wrote(uv);
                Some(Slot { uv, bx: b.min.x.floor() as i32, by: b.min.y.floor() as i32 })
            }
        };
        self.slots.insert(key, slot);
        slot
    }

    /// An icon tile at `size` physical pixels (em box) with `cut` (an icon and its size in percent) knocked out of it: the cut-out's ink
    /// centred across the icon's ink and level with the middle of its body (the rows whose ink
    /// reaches its right edge: a speech bubble less its tail), in whole pixels.
    pub(crate) fn mark(
        &mut self,
        icon: Icon,
        cut: Option<(Icon, u8)>,
        size: f32,
    ) -> Option<(Rect, [f32; 4], [i32; 2])> {
        let px = size.round().max(1.0) as u32;
        let key = (icon, px, cut);
        // A mark kept as empty is a hit too: it is not rasterized again (`hit`).
        if let Some(&kept) = self.icons.get(&key) {
            let s = self.hit(kept)?;
            if let Some(&(ink, origin)) = self.marks.get(&key) {
                return Some((s.uv, ink, origin));
            }
            let m = nova_icons::metrics(icon, px as f32, 1.0);
            return Some((s.uv, m.ink_bounds, m.origin));
        }
        let mut tile = nova_icons::rasterize(icon, px as f32, 1.0);
        let (w, h) = (tile.w as i32, tile.h as i32);
        if w <= 0 || h <= 0 {
            self.icons.insert(key, None);
            return None;
        }
        if let Some((inner, pct)) = cut {
            let ins = nova_icons::rasterize(inner, px as f32 * pct as f32 / 100.0, 1.0);
            let reaches = |row: i32| tile.a8[(row * w + w - 1) as usize] >= 128;
            let cx = (tile.ink_bounds[0] + tile.ink_bounds[2]) * 0.5 - tile.origin[0] as f32;
            let cy = match ((0..h).find(|&r| reaches(r)), (0..h).rev().find(|&r| reaches(r))) {
                (Some(a), Some(b)) => (a + b + 1) as f32 * 0.5,
                _ => (tile.ink_bounds[1] + tile.ink_bounds[3]) * 0.5 - tile.origin[1] as f32,
            };
            let ix = (ins.ink_bounds[0] + ins.ink_bounds[2]) * 0.5 - ins.origin[0] as f32;
            let iy = (ins.ink_bounds[1] + ins.ink_bounds[3]) * 0.5 - ins.origin[1] as f32;
            let (dx, dy) = ((cx - ix).round() as i32, (cy - iy).round() as i32);
            for row in 0..ins.h as i32 {
                for col in 0..ins.w as i32 {
                    let (x, y) = (col + dx, row + dy);
                    if x < 0 || y < 0 || x >= w || y >= h {
                        continue;
                    }
                    let hole = ins.a8[(row * ins.w as i32 + col) as usize] as u32;
                    let p = &mut tile.a8[(y * w + x) as usize];
                    *p = ((*p as u32 * (255 - hole) + 127) / 255) as u8;
                }
            }
            self.marks.insert(key, (tile.ink_bounds, tile.origin));
        }
        let (ox, oy) = self.alloc(w, h)?;
        for y in 0..h {
            let d = ((oy + y) * ATLAS + ox) as usize;
            self.atlas[d..d + w as usize].copy_from_slice(&tile.a8[(y * w) as usize..((y + 1) * w) as usize]);
        }
        let uv = Rect::new(ox, oy, w, h);
        self.wrote(uv);
        self.icons.insert(key, Some(Slot { uv, bx: tile.origin[0], by: tile.origin[1] }));
        Some((uv, tile.ink_bounds, tile.origin))
    }

    /// Glyph primitives of a vector shape `size` px square, centred on (cx, cy) by its ink.
    pub(crate) fn shape_prims(
        &mut self,
        shape: &crate::vector::Shape,
        size: f32,
        cx: f32,
        cy: f32,
        color: Color,
        out: &mut Vec<Prim>,
    ) {
        let px = size.round().clamp(1.0, 1024.0) as u32;
        let key = (shape.key, px);
        let hit = match self.shapes.get(&key) {
            Some(&v) => {
                if let Some((s, _)) = v {
                    self.touch(page_mask(s.uv));
                }
                v
            }
            None => {
                let t = crate::vector::rasterize(shape, px);
                let v = if t.ink[2] > t.ink[0] {
                    // Only the ink goes in the atlas.
                    let (x0, y0) = (t.ink[0] as i32, t.ink[1] as i32);
                    let (w, h) = (t.ink[2] as i32 - x0, t.ink[3] as i32 - y0);
                    self.alloc(w, h).map(|(ox, oy)| {
                        for y in 0..h {
                            let d = ((oy + y) * ATLAS + ox) as usize;
                            let s = ((y0 + y) as u32 * t.px + x0 as u32) as usize;
                            self.atlas[d..d + w as usize].copy_from_slice(&t.a8[s..s + w as usize]);
                        }
                        let uv = Rect::new(ox, oy, w, h);
                        self.wrote(uv);
                        (Slot { uv, bx: x0, by: y0 }, t.ink)
                    })
                } else {
                    None
                };
                self.shapes.insert(key, v);
                v
            }
        };
        let Some((slot, ink)) = hit else { return };
        // The ink's own centre on (cx, cy), as an icon's.
        let x = (cx - (ink[2] - ink[0]) * 0.5).round() as i32;
        let y = (cy - (ink[3] - ink[1]) * 0.5).round() as i32;
        out.push(Prim::Glyph { x, y, uv: slot.uv, color });
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mark_prims(
        &mut self,
        icon: Icon,
        cut: Option<(Icon, u8)>,
        size: f32,
        cx: f32,
        cy: f32,
        color: Color,
        out: &mut Vec<Prim>,
    ) {
        if let Some((uv, ink, origin)) = self.mark(icon, cut, size) {
            let icx = (ink[0] + ink[2]) * 0.5;
            let icy = (ink[1] + ink[3]) * 0.5;
            let x = (cx - icx).round() as i32 + origin[0];
            let y = (cy - icy).round() as i32 + origin[1];
            out.push(Prim::Glyph { x, y, uv, color });
        }
    }

    fn metrics(&self, style: &TextStyle) -> (f32, f32) {
        self.primary(style.weight).ascent_descent(style.size)
    }

    /// The face a line is laid out by: the first family's, nearest `weight`.
    fn primary(&self, weight: u16) -> &Face {
        let (fam, face) = primary_pick(&self.families, weight);
        &self.families[fam as usize].faces[face as usize]
    }

    /// The first family's measures at em size `size` (px) and `weight`: what a line of text is
    /// laid out by, and where its capitals and lowercase stand.
    pub fn font_metrics(&self, size: f32, weight: u16) -> FontMetrics {
        let face = self.primary(weight);
        let (ascent, descent) = face.ascent_descent(size);
        let ink = |c: char| {
            let id = face.font.glyph_id(c);
            face.font.outline(id).map_or(0.0, |o| (o.bounds.max.y - o.bounds.min.y).abs() * size / face.upem)
        };
        FontMetrics { ascent, descent, cap_height: ink('H'), x_height: ink('x') }
    }

    /// Where a caret stands before each char of `s` (laid out as one line, `\r` not counted),
    /// and after the last: `chars + 1` x positions, px from the line's left. Before a cluster's
    /// first char, its leading edge (the left of a left-to-right cluster, the right of a
    /// right-to-left one); before a later char of a cluster of several (a ligature, a conjunct),
    /// that share of its width; after the last char, the trailing edge of the last cluster. The
    /// advances and order `layout` places glyphs by (caret placement, hit testing, selection).
    pub fn carets(&mut self, s: &str, style: &TextStyle) -> Vec<f32> {
        let t = without_cr(s);
        let sh = self.shaped(&t, style.weight);
        let size = style.size;
        // Each cluster's extent on the line, and whether it reads right to left.
        let mut ext = vec![(f32::INFINITY, f32::NEG_INFINITY, false); sh.clusters.len()];
        let mut pen = 0.0;
        for (gs, level, _) in shape::visual(&sh, 0..t.len() as u32, None) {
            for g in gs {
                if let Ok(i) = sh.clusters.binary_search_by_key(&g.cluster, |c| c.at) {
                    let e = &mut ext[i];
                    (e.0, e.1, e.2) = (e.0.min(pen), e.1.max(pen + g.adv * size), shape::rtl(level));
                }
                pen += g.adv * size;
            }
        }
        let mut out = Vec::with_capacity(t.len() + 1);
        let mut trailing = 0.0;
        for (i, c) in sh.clusters.iter().enumerate() {
            let end = sh.clusters.get(i + 1).map_or(t.len(), |n| n.at as usize);
            // A cluster with no glyph (a hard break) stands where the one before it ended.
            let (x0, x1, rtl) = if ext[i].0.is_finite() { ext[i] } else { (trailing, trailing, false) };
            let n = t[c.at as usize..end].chars().count();
            for k in 0..n {
                let f = k as f32 / n as f32;
                out.push(if rtl { x1 - f * (x1 - x0) } else { x0 + f * (x1 - x0) });
            }
            trailing = if rtl { x0 } else { x1 };
        }
        out.push(trailing);
        out
    }

    /// Lays out `s` within `width` px (0 = unbounded), at most `max_lines` (0 = unbounded).
    /// Layout measurement must not populate the atlas or retain positioned glyph arrays.
    /// Alignment affects placement, not dimensions, so it is canonicalized here.
    pub fn measure(&mut self, s: &str, style: &TextStyle, width: i32, max_lines: u16) -> Measurement {
        let key = Key::new(style, width, max_lines, Align::Start);
        if let Some(m) = self.measures.get(key, s) {
            return m;
        }
        self.stats_measured += 1;
        let p = self.layout_uncached(s, style, width, max_lines, Align::Start, false);
        let m = Measurement { ink_w: p.ink_w, h: p.h, lines: p.lines };
        self.measures.insert(key, s.to_owned(), m);
        m
    }

    pub fn layout(&mut self, s: &str, style: &TextStyle, width: i32, max_lines: u16, align: Align) -> Rc<Para> {
        let key = Key::new(style, width, max_lines, align);
        if let Some(r) = self.runs.get(key, s) {
            self.touch(r.pages);
            return r;
        }
        self.stats_laid_out += 1;
        let para = Rc::new(self.layout_uncached(s, style, width, max_lines, align, true));
        self.runs.insert(key, s.to_owned(), para.clone());
        para
    }

    fn layout_uncached(
        &mut self,
        s: &str,
        style: &TextStyle,
        width: i32,
        max_lines: u16,
        align: Align,
        emit_glyphs: bool,
    ) -> Para {
        // Glyphs are rastered at the size laid out, to 1/8 px, and at the pen's position to 1/4 px.
        // Rastered at whole pixel sizes, a fluidly scaled UI (the size following the window
        // through a resize) switched raster size every pixel: unhinted, each size lands its
        // stems on the pixel grid differently, so text went sharp, soft, sharp through the drag.
        let q = size_step(style.size).max(SIZE_STEPS as u32);
        let (ascent, descent) = self.metrics(style);
        let line_h = style.line.max(ascent - descent);
        let baseline = ((line_h - (ascent - descent)) * 0.5 + ascent).round() as i32;
        let t = without_cr(s);
        let (sh, ell) = (self.shaped(&t, style.weight), self.shaped("\u{2026}", style.weight));
        let size = style.size;
        let cl = &sh.clusters;
        let adv = |i: usize| cl[i].adv * size;
        let at = |i: usize| cl.get(i).map_or(t.len() as u32, |c| c.at);
        let first_char = |i: usize| t[cl[i].at as usize..].chars().next().unwrap_or(' ');
        // Greedy breaking into lines of cluster ranges, each with its width and whether it was
        // broken at an opportunity (its trailing spaces then neither counted nor placed).
        let limit = if width > 0 { width as f32 } else { f32::INFINITY };
        let mut lines: Vec<(usize, usize, f32, bool)> = Vec::new();
        let (mut start, mut x, mut last_break) = (0usize, 0f32, None::<(usize, f32)>);
        // `x` at the end of the last cluster that is not a space.
        let mut ink = 0f32;
        let mut i = 0;
        while i < cl.len() {
            let c = cl[i];
            if c.hard {
                lines.push((start, i, x, false));
                start = i + 1;
                (x, ink) = (0.0, 0.0);
                last_break = None;
                i += 1;
                continue;
            }
            if x + adv(i) > limit && i > start && !c.space {
                // Break: at the last opportunity if any, else before this cluster (one earlier
                // when this one may not start a line).
                let (end, w) = match last_break {
                    Some((b, w)) if b > start => (b, w),
                    _ => {
                        let mut e = i;
                        if no_break_before(first_char(i)) && e > start + 1 {
                            e -= 1;
                        }
                        (e, (start..e).map(adv).sum())
                    }
                };
                lines.push((start, end, w, true));
                start = end;
                while start < cl.len() && cl[start].space {
                    start += 1;
                }
                (x, ink) = (0.0, 0.0);
                for (k, c) in cl.iter().enumerate().take(i.max(start)).skip(start) {
                    x += adv(k);
                    if !c.space {
                        ink = x;
                    }
                }
                last_break = None;
                if start > i {
                    i = start;
                    continue;
                }
            }
            x += adv(i);
            if !c.space {
                ink = x;
            }
            if c.brk == shape::Break::May {
                last_break = Some((i + 1, ink));
            }
            i += 1;
        }
        lines.push((start, cl.len(), x, false));
        let mut truncated = false;
        if max_lines > 0 && lines.len() > max_lines as usize {
            lines.truncate(max_lines as usize);
            truncated = true;
        }
        // Ellipsis on the last line when truncated or overflowing.
        let ell_adv: f32 = ell.glyphs.iter().map(|g| g.adv * size).sum();
        // The char each byte starts (`Para::chars`).
        let mut char_at = Vec::new();
        if emit_glyphs {
            char_at = vec![0u32; t.len() + 1];
            let mut n = 0;
            for (b, _) in t.char_indices() {
                char_at[b] = n;
                n += 1;
            }
            char_at[t.len()] = n;
        }
        let mut glyphs = Vec::new();
        let mut glyph_chars = Vec::new();
        let mut src = Vec::new();
        let mut ink_w = 0i32;
        let n = lines.len();
        for (li, &(a, b, w, soft)) in lines.iter().enumerate() {
            let mut end = b;
            let mut lw = w;
            let last = li + 1 == n;
            let overflow = lw > limit + 0.5;
            let ellipsis = last && (truncated || overflow) && width > 0;
            if ellipsis {
                while end > a && lw + ell_adv > limit {
                    end -= 1;
                    lw -= adv(end);
                }
                while end > a && cl[end - 1].space {
                    end -= 1;
                    lw -= adv(end);
                }
                lw += ell_adv;
                truncated = true;
            }
            let lwi = lw.ceil() as i32;
            ink_w = ink_w.max(lwi);
            if !emit_glyphs {
                continue;
            }
            let x0 = match align {
                Align::Start => 0.0,
                Align::Center => ((width.max(lwi) - lwi) as f32 * 0.5).floor(),
                Align::End => (width.max(lwi) - lwi) as f32,
            };
            let y = li as i32 * line_h.round() as i32 + baseline;
            let mut shown = end;
            while soft && shown > a && cl[shown - 1].space {
                shown -= 1;
            }
            // The ellipsis follows the line's text in its paragraph's direction.
            let level = sh.para_level(at(a));
            let extra = ellipsis.then_some((&ell.glyphs[..], level));
            let mut pen = x0;
            for (gs, level, is_ell) in shape::visual(&sh, at(a)..at(shown), extra) {
                for g in gs {
                    let gx = pen + g.dx * size;
                    pen += g.adv * size;
                    if !is_ell && t.as_bytes()[g.cluster as usize] == b' ' {
                        continue;
                    }
                    let k = char_at[if is_ell { at(end) } else { g.cluster } as usize];
                    let rise = g.dy * size;
                    let (whole, phase) = pen_cell(gx);
                    let id = GlyphId(g.id);
                    if let Some(slot) = self.slot(g.fam, g.face, id, q, phase) {
                        glyphs.push((whole + slot.bx, y - rise.round() as i32 + slot.by, slot.uv));
                        glyph_chars.push(k);
                        let (fam, face, adv) = (g.fam, g.face, g.adv * size);
                        src.push(GlyphSrc { fam, face, id, pen: gx, adv, base: y, rise, rtl: shape::rtl(level) });
                    }
                }
            }
        }
        let h = (n as f32 * line_h).round() as i32;
        let w = if width > 0 { width.max(0) } else { ink_w };
        let pages = glyphs.iter().fold(0, |m, g| m | page_mask(g.2));
        Para { glyphs, chars: glyph_chars, w, h, lines: n, ink_w, truncated, src, q, pages }
    }
}

impl Default for Text {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod exact_bounds {
    /// Noto Sans' bold `y`: no coverage where it has no ink (between its arms at the top, the
    /// bottom-right corner). Through ab_glyph's own bounds those pixels carried a flat leak.
    #[test]
    fn bold_variable_glyph_has_a_clean_tile() {
        let Some(dir) = std::env::var_os("NOVA_FONTS_DIR") else {
            eprintln!("skipped: NOVA_FONTS_DIR not set");
            return;
        };
        let path = std::path::Path::new(&dir).join("NotoSans-VF.ttf");
        let path = path.to_str().unwrap();
        if !std::path::Path::new(path).is_file() {
            eprintln!("skipped: no {path}");
            return;
        }
        use ab_glyph::Font;
        let faces = super::load_variable(path, &[700]);
        let f = &faces[0].font;
        let render = |og: ab_glyph::OutlinedGlyph| {
            let b = og.px_bounds();
            let (w, h) = (b.width() as usize, b.height() as usize);
            let mut buf = vec![0f32; w * h];
            og.draw(|x, y, v| buf[y as usize * w + x as usize] = v);
            [buf[w / 2], buf[(h - 1) * w + w - 1]]
        };
        for px in [14u32, 20, 28, 36] {
            let g = f.glyph_id('y').with_scale_and_position(super::em(f, px as f32), ab_glyph::point(0.0, 0.0));
            let old = render(f.outline_glyph(g.clone()).unwrap());
            let new = render(super::outline_exact(f, g).unwrap());
            eprintln!("px {px}: ab_glyph bounds {old:.3?}  exact bounds {new:.3?}");
            for v in new {
                assert!(v < 0.02, "px {px}: coverage {v} where the y has no ink");
            }
        }
    }
}

#[cfg(test)]
mod drag_atlas {
    /// A resize of a page of large text: four lines of mixed CJK / Latin at a size sweeping 24 -> 36 px
    /// over 400 frames. The atlas must hold the whole drag without starting over.
    #[test]
    fn a_fluid_resize_fits_in_the_atlas() {
        let Some(dir) = std::env::var_os("NOVA_FONTS_DIR").map(std::path::PathBuf::from) else {
            eprintln!("skipped: NOVA_FONTS_DIR not set");
            return;
        };
        if !dir.join("NotoSans-VF.ttf").is_file() {
            eprintln!("skipped: no bundled fonts");
            return;
        }
        // Not through NOVAUI_FONTS: the variable is process-wide and the other tests of this
        // binary build their own Text at the same time.
        let mut t = super::Text::new();
        t.families = super::load_bundled(&dir);
        let lines = [
            "夜明けの空に 光る星を 数えてた daydream",
            "Every single day I think of you, yeah",
            "君がいた季節 忘れないよ 사랑해 영원히",
            "insane dream — 拂晓 HOYO-MiX 2026",
        ];
        let (t0, mut peak) = (std::time::Instant::now(), 0usize);
        // Starting over between frames is fine; a frame filling the atlas half-way is not.
        let (mut mid, mut t_mid) = (0, 0);
        for f in 0..400 {
            let size = 24.0 + 12.0 * f as f32 / 399.0;
            for (k, l) in lines.iter().enumerate() {
                let st = super::TextStyle { size, line: size * 1.4, weight: if k == 0 { 700 } else { 400 } };
                let _ = t.layout(l, &st, 0, 0, super::Align::Start);
            }
            peak = peak.max(t.slots.len());
            let before = t.resets;
            t.between_frames();
            mid += before - t_mid;
            t_mid = t.resets;
        }
        eprintln!(
            "400 frames: {:.1} ms total, peak slots {peak}, resets {}",
            t0.elapsed().as_secs_f64() * 1e3,
            t.resets
        );
        assert_eq!(mid, 0, "the atlas filled in the middle of a frame");
    }

    /// A long lyric whose units glow and scale, each at its own scale, over 300 frames: far more
    /// tiles than the atlas holds over the run, a frame's worth at a time. The pages used
    /// longest ago make room; the atlas never starts over.
    #[test]
    fn a_long_glowing_lyric_at_several_scales_does_not_reset_the_atlas() {
        let Some(dir) = std::env::var_os("NOVA_FONTS_DIR").map(std::path::PathBuf::from) else {
            eprintln!("skipped: NOVA_FONTS_DIR not set");
            return;
        };
        if !dir.join("NotoSans-VF.ttf").is_file() {
            eprintln!("skipped: no bundled fonts");
            return;
        }
        let mut t = super::Text::new();
        t.families = super::load_bundled(&dir);
        let lines = [
            "夜明けの空に 光る星を 数えてた",
            "君がいた季節 忘れないよ",
            "風が吹いて 花が散って 春が終わる",
            "遠い街の灯り ひとつずつ消えて",
            "Every single day I think of you",
            "사랑해 영원히 너와 함께",
            "拂晓的光 照亮回家的路",
            "雨上がりの道 二人で歩いた",
        ];
        let st = super::TextStyle { size: 32.0, line: 44.0, weight: 400 };
        let clip = super::Rect::new(0, 0, 4096, 4096);
        let mut out = Vec::new();
        for f in 0..300u32 {
            for l in &lines {
                let p = t.layout(l, &st, 0, 0, super::Align::Start);
                let n = p.chars.iter().max().map_or(0, |&c| c + 1);
                let units = crate::style::Units {
                    chars: (0..n).map(|c| c..c + 1).collect(),
                    fx: (0..n)
                        .map(|u| crate::style::UnitFx {
                            scale: 1.0 + 0.04 * ((f / 4 + u % 3) % 16) as f32,
                            glow: 6.0,
                            glow_fg: [1.0; 4],
                            ..Default::default()
                        })
                        .collect(),
                };
                out.clear();
                t.prims_units(&p, 0, 0, [1.0; 4], None, &units, (0.0, [0.0; 2]), 1.0, clip, &mut out);
                assert!(!out.is_empty());
            }
            t.between_frames();
        }
        eprintln!("300 frames: {} slots, {} evictions, {} resets", t.slots.len(), t.evictions, t.resets);
        assert!(t.evictions > 0, "the lyric fits in the atlas: the test does not fill it");
        assert_eq!(t.resets, 0, "the atlas started over");
    }
}

#[cfg(test)]
mod scripts {
    //! Lines of the scripts shaping exists for (Arabic, Hebrew, Devanagari, Thai), laid out with
    //! the bundled Noto fonts and drawn from the atlas: right where per-char layout was wrong.
    use super::{Align, Para, Text, TextStyle, ATLAS};
    use ab_glyph::Font;

    /// A `Text` over the bundle, with its script fonts; `None` (skipped) without them.
    fn bundled() -> Option<Text> {
        let Some(dir) = std::env::var_os("NOVA_FONTS_DIR").map(std::path::PathBuf::from) else {
            eprintln!("skipped: NOVA_FONTS_DIR not set");
            return None;
        };
        if !super::BUNDLED_SCRIPTS.iter().all(|f| dir.join(f).is_file()) {
            eprintln!("skipped: the bundle has no script fonts");
            return None;
        }
        let mut t = Text::new();
        t.families = super::load_bundled(&dir);
        Some(t)
    }

    fn line(t: &mut Text, s: &str) -> std::rc::Rc<Para> {
        t.layout(s, &TextStyle { size: 40.0, line: 60.0, weight: 400 }, 0, 0, Align::Start)
    }

    /// The paragraph's glyphs drawn from the atlas (coverage, the most of overlapping glyphs).
    fn draw(t: &Text, p: &Para, only: impl Fn(usize) -> bool) -> (usize, Vec<u8>) {
        let (w, h) = (p.w.max(1) as usize + 64, p.h.max(1) as usize + 64);
        let mut img = vec![0u8; w * h];
        for &(gx, gy, uv) in p.glyphs.iter().enumerate().filter(|(i, _)| only(*i)).map(|(_, g)| g) {
            for y in 0..uv.h {
                for x in 0..uv.w {
                    let (px, py) = (gx + x + 32, gy + y + 32);
                    if px < 0 || py < 0 || px as usize >= w || py as usize >= h {
                        continue;
                    }
                    let v = t.atlas[((uv.y + y) * ATLAS + uv.x + x) as usize];
                    let d = &mut img[py as usize * w + px as usize];
                    *d = (*d).max(v);
                }
            }
        }
        (w, img)
    }

    /// The columns holding ink (any coverage of 64 or more), left to right.
    fn ink_columns(w: usize, img: &[u8]) -> Vec<usize> {
        (0..w).filter(|&x| img.iter().skip(x).step_by(w).any(|&v| v >= 64)).collect()
    }

    /// The clusters (their first chars), left to right.
    fn order(p: &Para) -> Vec<u32> {
        let mut g: Vec<(i32, u32)> = p.src.iter().zip(&p.chars).map(|(s, &c)| ((s.pen * 4.0) as i32, c)).collect();
        g.sort();
        let mut o: Vec<u32> = g.into_iter().map(|g| g.1).collect();
        o.dedup();
        o
    }

    /// Glyphs of no advance (marks, set on their letter by the font) and the glyph of their
    /// cluster each stands on.
    fn marks(p: &Para) -> Vec<(usize, usize)> {
        let is_mark = |i: usize| p.src[i].adv == 0.0;
        (0..p.glyphs.len())
            .filter(|&i| is_mark(i))
            .filter_map(|i| (0..p.glyphs.len()).find(|&b| !is_mark(b) && p.chars[b] == p.chars[i]).map(|b| (i, b)))
            .collect()
    }

    #[test]
    fn arabic_joins_its_letters_and_reads_right_to_left() {
        let Some(mut t) = bundled() else { return };
        // ب ي ت all join: drawn as one stroke, the first letter on the right.
        let s = "بيت";
        let p = line(&mut t, s);
        assert_eq!(order(&p), [2, 1, 0], "not right to left");
        let (w, img) = draw(&t, &p, |_| true);
        let cols = ink_columns(w, &img);
        let (a, b) = (cols[0], *cols.last().unwrap());
        let gaps: Vec<usize> = (a..=b).filter(|x| !cols.contains(x)).collect();
        assert!(gaps.is_empty(), "the word is broken at columns {gaps:?}: its letters do not join");
        // Each letter takes its joining form: the glyph it has alone is not drawn.
        for (c, ch) in s.chars().enumerate() {
            let face = &t.families[p.src[0].fam as usize].faces[p.src[0].face as usize].font;
            let alone = face.glyph_id(ch);
            let drawn = p.src.iter().zip(&p.chars).any(|(g, &k)| k == c as u32 && g.id == alone);
            assert!(!drawn, "char {c} drawn in its isolated form");
        }
        // In a left-to-right line, an Arabic word and the number after it read right to left as
        // one stretch: the number left of the word, its digits in their own order.
        let p = line(&mut t, "abc بيت 123");
        let o = order(&p);
        assert_eq!(o, [0, 1, 2, 8, 9, 10, 6, 5, 4], "{o:?}");
    }

    #[test]
    fn hebrew_reads_right_to_left_with_its_points_on_their_letters() {
        let Some(mut t) = bundled() else { return };
        // בְּרֵאשִׁית: bet with dagesh and sheva, resh with tsere, alef, shin with its dot and
        // hiriq, yod, tav; a letter and its points are one cluster.
        let p = line(&mut t, "בְּרֵאשִׁית");
        assert_eq!(order(&p), [10, 9, 6, 5, 3, 0], "the letters do not read right to left");
        // Every point stands over, in or under the letter it belongs to.
        let m = marks(&p);
        assert!(m.len() >= 4, "{} points placed as marks", m.len());
        for (i, b) in m {
            let (mk, l) = (p.glyphs[i], p.glyphs[b]);
            let mid = mk.0 + mk.2.w / 2;
            assert!(l.0 <= mid && mid <= l.0 + l.2.w, "a point at x {mid}, its letter spans {}..{}", l.0, l.0 + l.2.w);
        }
    }

    #[test]
    fn devanagari_sets_its_i_sign_before_the_consonant_and_joins_conjuncts() {
        let Some(mut t) = bundled() else { return };
        // कि: the sign of i (after क in the text) is drawn first, left of क.
        let p = line(&mut t, "कि");
        let face = &t.families[p.src[0].fam as usize].faces[p.src[0].face as usize].font;
        let ka = p.src.iter().position(|g| g.id == face.glyph_id('क')).expect("क is drawn");
        let sign = (0..p.glyphs.len()).find(|&i| i != ka).expect("the i sign is drawn");
        assert!(p.src[sign].pen < p.src[ka].pen, "the i sign is set after क");
        let (w, all) = draw(&t, &p, |_| true);
        let (_, alone) = draw(&t, &p, |i| i == sign);
        assert_eq!(ink_columns(w, &all)[0], ink_columns(w, &alone)[0], "the i sign is not the leftmost ink");
        // क्ष (ka, virama, ssa) is one conjunct glyph, not ka with a visible virama and ssa.
        let p = line(&mut t, "क्ष");
        assert_eq!(p.glyphs.len(), 1, "क्ष drawn as {} glyphs", p.glyphs.len());
    }

    #[test]
    fn thai_stacks_its_marks_and_breaks_between_words() {
        let Some(mut t) = bundled() else { return };
        // ที่: the tone mark stands above the vowel above the consonant, not on it.
        let p = line(&mut t, "ที่");
        let m = marks(&p);
        assert_eq!(m.len(), 2, "{} marks", m.len());
        let (mut a, mut b) = (p.glyphs[m[0].0], p.glyphs[m[1].0]);
        if b.1 < a.1 {
            std::mem::swap(&mut a, &mut b);
        }
        assert!(a.1 + a.2.h <= b.1 + 1, "the tone mark ({a:?}) overlaps the vowel ({b:?})");
        // Two words a line can only hold one of break at the space between them, neither cut.
        let st = TextStyle { size: 40.0, line: 60.0, weight: 400 };
        let one = t.measure("สวัสดี", &st, 0, 0).ink_w;
        let p = t.layout("สวัสดี ครับ", &st, one + 4, 0, Align::Start);
        assert_eq!((p.lines, p.truncated), (2, false));
        let top = p.src[0].base;
        let on = |first: bool| -> Vec<u32> {
            p.chars.iter().zip(&p.src).filter(|(_, s)| (s.base == top) == first).map(|(&c, _)| c).collect()
        };
        let (first, second) = (on(true), on(false));
        assert!(first.iter().all(|&c| c < 6) && second.iter().all(|&c| c >= 7), "{first:?} / {second:?}");
    }

    #[test]
    fn carets_stand_at_cluster_edges_in_their_direction() {
        let Some(mut t) = bundled() else { return };
        let st = TextStyle { size: 40.0, line: 60.0, weight: 400 };
        // Left to right: rising; right to left: falling, the end at the line's left.
        let c = t.carets("abc", &st);
        assert!(c.windows(2).all(|w| w[0] < w[1]), "{c:?}");
        let c = t.carets("שלום", &st);
        assert!(c.windows(2).all(|w| w[0] > w[1]), "{c:?}");
        assert!(c[4].abs() < 0.5, "{c:?}");
        // One cluster of two chars (क and its i sign): the caret before it at its left edge.
        let c = t.carets("कि", &st);
        assert_eq!(c.len(), 3);
        assert!(c[0].abs() < 0.5 && c[2] > c[0], "{c:?}");
    }
}

#[cfg(test)]
mod halo_count {
    /// A halo written into the atlas counts as rasterized, as `TextCounts::rasterized` says (it
    /// was uploaded without being counted); one already kept does not count again.
    #[test]
    fn a_halo_counts_once_as_rasterized() {
        let Some(dir) = std::env::var_os("NOVA_FONTS_DIR").map(std::path::PathBuf::from) else {
            eprintln!("skipped: NOVA_FONTS_DIR not set");
            return;
        };
        if !dir.join("NotoSans-VF.ttf").is_file() {
            eprintln!("skipped: no bundled fonts");
            return;
        }
        let mut t = super::Text::new();
        t.families = super::load_bundled(&dir);
        let st = super::TextStyle { size: 24.0, line: 32.0, weight: 400 };
        let p = t.layout("A", &st, 0, 0, super::Align::Start);
        let s = p.src[0];
        let q = super::size_step(st.size);
        assert!(t.slot(s.fam, s.face, s.id, q, 0).is_some());
        let before = t.stats_rasterized;
        assert!(t.glow_slot(s.fam, s.face, s.id, q, 0, 4).is_some());
        assert_eq!(t.stats_rasterized, before + 1, "the halo");
        assert!(t.glow_slot(s.fam, s.face, s.id, q, 0, 4).is_some());
        assert_eq!(t.stats_rasterized, before + 1, "kept: nothing rasterized again");
    }
}
