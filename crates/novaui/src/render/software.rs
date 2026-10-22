//! Software backend: the reference rasteriser for every primitive, expression for expression the
//! same as `prims.wgsl`. Pixels are premultiplied, packed 0xAARRGGBB (BGRA in memory, the layout
//! of a top-down 32-bit DIB). Window presentation: `SetDIBitsToDevice` of the damage rows.

use crate::geom::*;
use crate::render::primitive::*;
use std::collections::HashMap;

pub struct Surface {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u32>,
}

impl Surface {
    fn new(w: i32, h: i32) -> Self {
        Surface { w, h, px: vec![0; (w.max(0) * h.max(0)) as usize] }
    }
    fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }
}

pub struct Software {
    pub canvas: Surface,
    layers: HashMap<u32, Surface>,
    images: HashMap<u32, Surface>,
    atlas: Vec<u8>,
    #[cfg(windows)]
    pub hwnd: Option<windows::Win32::Foundation::HWND>,
}

#[inline]
fn premul(c: Color, cov: f32) -> [f32; 4] {
    let a = c[3] * cov;
    [c[0] * a, c[1] * a, c[2] * a, a]
}

#[inline]
fn pack(c: [f32; 4]) -> u32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    (q(c[3]) << 24) | (q(c[0]) << 16) | (q(c[1]) << 8) | q(c[2])
}

#[inline]
fn over(dst: &mut u32, s: [f32; 4]) {
    if s[3] <= 0.0 && s[0] <= 0.0 && s[1] <= 0.0 && s[2] <= 0.0 {
        return;
    }
    if s[3] >= 1.0 {
        *dst = pack(s);
        return;
    }
    let d = argb(*dst);
    let k = 1.0 - s[3];
    *dst = pack([s[0] + d[0] * k, s[1] + d[1] * k, s[2] + d[2] * k, s[3] + d[3] * k]);
}

#[inline]
fn scale(c: [f32; 4], k: f32) -> [f32; 4] {
    [c[0] * k, c[1] * k, c[2] * k, c[3] * k]
}

#[inline]
pub fn sd_round_box(px: f32, py: f32, bx: f32, by: f32, r: f32) -> f32 {
    let qx = px.abs() - bx + r;
    let qy = py.abs() - by + r;
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

fn round_cov(rect: Rect, radius: f32, px: f32, py: f32) -> f32 {
    let hw = rect.w as f32 * 0.5;
    let hh = rect.h as f32 * 0.5;
    let r = radius.min(hw).min(hh);
    (0.5 - sd_round_box(px - (rect.x as f32 + hw), py - (rect.y as f32 + hh), hw, hh, r)).clamp(0.0, 1.0)
}

/// The radius of the corner (cx, cy) from the centre lies toward: 0 where `square` (a mask of
/// `primitive::corner` bits) leaves it square. `prims.wgsl`'s `corner_radius`.
fn corner_radius(cx: f32, cy: f32, r: f32, square: u8) -> f32 {
    use crate::render::primitive::corner;
    let bit = match (cx >= 0.0, cy >= 0.0) {
        (false, false) => corner::TOP_LEFT,
        (true, false) => corner::TOP_RIGHT,
        (true, true) => corner::BOTTOM_RIGHT,
        (false, true) => corner::BOTTOM_LEFT,
    };
    if square & bit != 0 {
        0.0
    } else {
        r
    }
}

#[allow(clippy::too_many_arguments, reason = "the quad's fields, as the shader takes them")]
pub fn quad_color(
    rect: Rect,
    radius: f32,
    square: u8,
    fill: Color,
    border: f32,
    bc: Color,
    px: f32,
    py: f32,
) -> [f32; 4] {
    let hw = rect.w as f32 * 0.5;
    let hh = rect.h as f32 * 0.5;
    let cx = px - (rect.x as f32 + hw);
    let cy = py - (rect.y as f32 + hh);
    let r = corner_radius(cx, cy, radius.min(hw).min(hh), square);
    let d = sd_round_box(cx, cy, hw, hh, r);
    let cov = (0.5 - d).clamp(0.0, 1.0);
    if cov <= 0.0 {
        return [0.0; 4];
    }
    let fill_p = premul(fill, 1.0);
    let c = if border > 0.0 {
        let di = sd_round_box(cx, cy, hw - border, hh - border, (r - border).max(0.0));
        let inside = (0.5 - di).clamp(0.0, 1.0);
        let b = premul(bc, 1.0);
        let mut o = [0.0; 4];
        for i in 0..4 {
            o[i] = fill_p[i] * inside + b[i] * (1.0 - inside);
        }
        o
    } else {
        fill_p
    };
    scale(c, cov)
}

fn erf_ws(x: f32) -> f32 {
    let s = x.signum();
    let a = x.abs();
    let mut t = 1.0 + (0.278393 + (0.230389 + 0.078108 * (a * a)) * a) * a;
    t *= t;
    s - s / (t * t)
}

fn gaussian(x: f32, sigma: f32) -> f32 {
    (-(x * x) / (2.0 * sigma * sigma)).exp() / (2.5066283 * sigma)
}

fn shadow_x(x: f32, y: f32, sigma: f32, corner: f32, hx: f32, hy: f32) -> f32 {
    let delta = (hy - corner - y.abs()).min(0.0);
    let curved = hx - corner + (corner * corner - delta * delta).max(0.0).sqrt();
    let k = std::f32::consts::FRAC_1_SQRT_2 / sigma;
    let lo = 0.5 + 0.5 * erf_ws((x - curved) * k);
    let hi = 0.5 + 0.5 * erf_ws((x + curved) * k);
    hi - lo
}

pub fn shadow_alpha(rect: Rect, corner: f32, sigma: f32, px: f32, py: f32) -> f32 {
    let (hx, hy) = (rect.w as f32 * 0.5, rect.h as f32 * 0.5);
    let corner = corner.min(hx).min(hy);
    let (x, y0) = (px - (rect.x as f32 + hx), py - (rect.y as f32 + hy));
    let low = y0 - hy;
    let high = y0 + hy;
    let start = (-3.0 * sigma).clamp(low, high);
    let end = (3.0 * sigma).clamp(low, high);
    let step = (end - start) / 4.0;
    let mut y = start + step * 0.5;
    let mut v = 0.0;
    for _ in 0..4 {
        v += shadow_x(x, y0 - y, sigma, corner, hx, hy) * gaussian(y, sigma) * step;
        y += step;
    }
    v
}

fn load_clamped(s: &Surface, x: i32, y: i32) -> [f32; 4] {
    let x = x.clamp(0, s.w - 1);
    let y = y.clamp(0, s.h - 1);
    argb(s.px[(y * s.w + x) as usize])
}

fn bilinear(s: &Surface, tx: f32, ty: f32) -> [f32; 4] {
    let (qx, qy) = (tx - 0.5, ty - 0.5);
    let (fx, fy) = (qx.floor(), qy.floor());
    let (wx, wy) = (qx - fx, qy - fy);
    let (ix, iy) = (fx as i32, fy as i32);
    let a = load_clamped(s, ix, iy);
    let b = load_clamped(s, ix + 1, iy);
    let c = load_clamped(s, ix, iy + 1);
    let d = load_clamped(s, ix + 1, iy + 1);
    let mut o = [0.0; 4];
    for i in 0..4 {
        let top = a[i] + (b[i] - a[i]) * wx;
        let bot = c[i] + (d[i] - c[i]) * wx;
        o[i] = top + (bot - top) * wy;
    }
    o
}

/// Dual-Kawase blur of `region` of `src` (Bjorge 2015), each level stored at 8 bits as the GPU's
/// RGBA8 levels are: `levels` downsamples, then as many upsamples back to the region's size.
fn kawase_blur(src: &Surface, region: Rect, levels: u8) -> Surface {
    let sizes = kawase_sizes(region.w, region.h, levels);
    let mut lv: Vec<Surface> = Vec::with_capacity(sizes.len());
    let mut l0 = Surface::new(region.w, region.h);
    for y in 0..region.h {
        let o = ((region.y + y) * src.w + region.x) as usize;
        l0.px[(y * region.w) as usize..((y + 1) * region.w) as usize]
            .copy_from_slice(&src.px[o..o + region.w as usize]);
    }
    lv.push(l0);
    let pass = |from: &Surface, w: i32, h: i32, up: bool| -> Surface {
        let mut out = Surface::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let mut c = [0.0f32; 4];
                let mut add = |dx: f32, dy: f32, k: f32, sx: f32, sy: f32| {
                    let v = bilinear(from, sx + dx, sy + dy);
                    for i in 0..4 {
                        c[i] += v[i] * k;
                    }
                };
                if up {
                    let (sx, sy) = (px * 0.5, py * 0.5);
                    add(-2.0, 0.0, 1.0, sx, sy);
                    add(-1.0, 1.0, 2.0, sx, sy);
                    add(0.0, 2.0, 1.0, sx, sy);
                    add(1.0, 1.0, 2.0, sx, sy);
                    add(2.0, 0.0, 1.0, sx, sy);
                    add(1.0, -1.0, 2.0, sx, sy);
                    add(0.0, -2.0, 1.0, sx, sy);
                    add(-1.0, -1.0, 2.0, sx, sy);
                    c = scale(c, 1.0 / 12.0);
                } else {
                    let (sx, sy) = (px * 2.0, py * 2.0);
                    add(0.0, 0.0, 4.0, sx, sy);
                    add(1.0, 1.0, 1.0, sx, sy);
                    add(-1.0, -1.0, 1.0, sx, sy);
                    add(1.0, -1.0, 1.0, sx, sy);
                    add(-1.0, 1.0, 1.0, sx, sy);
                    c = scale(c, 1.0 / 8.0);
                }
                out.px[(y * w + x) as usize] = pack(c);
            }
        }
        out
    };
    for (from, to, up) in kawase_passes(levels) {
        let (w, h) = sizes[to];
        let next = pass(&lv[from], w, h, up);
        if up {
            lv[to] = next;
        } else {
            lv.push(next);
        }
    }
    lv.swap_remove(0)
}

impl Software {
    pub fn new(w: i32, h: i32) -> Self {
        Software {
            canvas: Surface::new(w, h),
            layers: HashMap::new(),
            images: HashMap::new(),
            atlas: vec![0; (crate::text::ATLAS * crate::text::ATLAS) as usize],
            #[cfg(windows)]
            hwnd: None,
        }
    }

    #[cfg(windows)]
    pub fn blit(&self, hwnd: windows::Win32::Foundation::HWND, rects: &[Rect]) {
        use windows::Win32::Graphics::Gdi::*;
        let c = &self.canvas;
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: c.w,
                biHeight: -c.h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: the window's DC, taken and released here; each band reads `r.h` rows of `c.w`
        // pixels from row `r.y` of the canvas (`r` is clipped to it), inside `c.px`.
        unsafe {
            let dc = GetDC(hwnd);
            for r in rects {
                let r = r.intersect(&c.bounds());
                if r.is_empty() {
                    continue;
                }
                let mut band = bmi;
                band.bmiHeader.biHeight = -r.h;
                let rows = c.px[(r.y * c.w) as usize..].as_ptr();
                SetDIBitsToDevice(
                    dc,
                    r.x,
                    r.y,
                    r.w as u32,
                    r.h as u32,
                    r.x,
                    0,
                    0,
                    r.h as u32,
                    rows as _,
                    &band,
                    DIB_RGB_COLORS,
                );
            }
            ReleaseDC(hwnd, dc);
        }
    }

    fn fill(s: &mut Surface, clip: Rect, v: u32) {
        for y in clip.y..clip.b() {
            let row = (y * s.w) as usize;
            s.px[row + clip.x as usize..row + clip.r() as usize].fill(v);
        }
    }

    fn raster(&self, target: &mut Surface, clip: Rect, off: (i32, i32), d: &Draw) {
        let b = d.cover(clip, off);
        if b.is_empty() {
            return;
        }
        let p = d.prim.translated(off.0, off.1);
        if let Prim::Backdrop { rect, radius, levels, extent, tint, lum, opacity } = p {
            let region = backdrop_region(rect, extent, clip);
            if region.is_empty() {
                return;
            }
            let blurred = kawase_blur(target, region, levels);
            for y in b.y..b.b() {
                for x in b.x..b.r() {
                    let (x, y) = (x as f32 + 0.5, y as f32 + 0.5);
                    let (ix, iy) = (x as i32, y as i32);
                    let c = argb(blurred.px[((iy - region.y) * blurred.w + ix - region.x) as usize]);
                    let mut rgb = [c[0], c[1], c[2]];
                    let lb = rgb[0] * 0.299 + rgb[1] * 0.587 + rgb[2] * 0.114;
                    let ll = lum[0] * 0.299 + lum[1] * 0.587 + lum[2] * 0.114;
                    let n = noise(ix, iy);
                    for i in 0..3 {
                        let l = (rgb[i] + (ll - lb)).clamp(0.0, 1.0);
                        rgb[i] = rgb[i] * (1.0 - lum[3]) + l * lum[3];
                        rgb[i] = rgb[i] * (1.0 - tint[3]) + tint[i] * tint[3];
                        // The frost's grain: a flat material (no passes) has none.
                        if levels > 0 {
                            rgb[i] = rgb[i] * (1.0 - NOISE_OPACITY) + n * NOISE_OPACITY;
                        }
                    }
                    let cov = round_cov(rect, radius, x, y) * opacity;
                    let w = target.w;
                    over(&mut target.px[(iy * w + ix) as usize], [rgb[0] * cov, rgb[1] * cov, rgb[2] * cov, cov]);
                }
            }
            return;
        }
        let w = target.w;
        let mut each = |f: &mut dyn FnMut(f32, f32) -> [f32; 4]| {
            for y in b.y..b.b() {
                let row = (y * w) as usize;
                for x in b.x..b.r() {
                    let c = f(x as f32 + 0.5, y as f32 + 0.5);
                    over(&mut target.px[row + x as usize], c);
                }
            }
        };
        match p {
            Prim::Quad { rect, radius, square, fill, border, border_color } => {
                each(&mut |x, y| quad_color(rect, radius, square, fill, border, border_color, x, y));
            }
            Prim::Shadow { rect, radius, sigma, color, cut } => {
                let sigma = shadow_sigma(sigma);
                each(&mut |x, y| {
                    let mut a = shadow_alpha(rect, radius, sigma, x, y);
                    if let Some(c) = cut {
                        a *= 1.0 - round_cov(c, radius, x, y);
                    }
                    premul(color, a)
                });
            }
            Prim::Backdrop { .. } => unreachable!("backdrops are drawn by the pass above"),
            Prim::Glyph { x: gx, y: gy, uv, color } => {
                let aw = crate::text::ATLAS;
                let atlas = &self.atlas;
                each(&mut |x, y| {
                    let (x, y) = (x as i32, y as i32);
                    let a = atlas[((uv.y + y - gy) * aw + uv.x + x - gx) as usize] as f32 / 255.0;
                    premul(color, a)
                });
            }
            Prim::GlyphXf { o, m, uv, color, lit, sweep } => {
                // As `fs_glyph_xf`.
                let Some((inv, edge, soft)) = crate::render::batch::glyph_xf_params(m, uv, sweep) else { return };
                let [ox, oy] = o;
                let aw = crate::text::ATLAS;
                let atlas = &self.atlas;
                let cell = |px: i32, py: i32| {
                    if px < 0 || py < 0 || px >= uv.w || py >= uv.h {
                        0.0
                    } else {
                        atlas[((uv.y + py) * aw + uv.x + px) as usize] as f32 / 255.0
                    }
                };
                each(&mut |x, y| {
                    let (qx, qy) = (x - ox, y - oy);
                    let (lx, ly) = (inv[0] * qx + inv[2] * qy, inv[1] * qx + inv[3] * qy);
                    let (tx, ty) = (lx - 0.5, ly - 0.5);
                    let (fx, fy) = (tx.floor(), ty.floor());
                    let (wx, wy) = (tx - fx, ty - fy);
                    let (ix, iy) = (fx as i32, fy as i32);
                    let top = cell(ix, iy) + (cell(ix + 1, iy) - cell(ix, iy)) * wx;
                    let bot = cell(ix, iy + 1) + (cell(ix + 1, iy + 1) - cell(ix, iy + 1)) * wx;
                    let cov = top + (bot - top) * wy;
                    let t = ((edge - lx) / soft).clamp(0.0, 1.0);
                    let c: [f32; 4] = std::array::from_fn(|i| color[i] + (lit[i] - color[i]) * t);
                    premul(c, cov)
                });
            }
            Prim::Image { rect, image, uv, radius, opacity, .. } => {
                let Some(img) = self.images.get(&image) else { return };
                each(&mut |x, y| {
                    let tx = (x - rect.x as f32) / rect.w as f32;
                    let ty = (y - rect.y as f32) / rect.h as f32;
                    let u = uv[0] + tx * (uv[2] - uv[0]);
                    let v = uv[1] + ty * (uv[3] - uv[1]);
                    let c = bilinear(img, u * img.w as f32, v * img.h as f32);
                    scale(c, round_cov(rect, radius, x, y) * opacity)
                });
            }
            Prim::Effect { rect, image, effect, params, opacity } => {
                let Some(img) = self.images.get(&image) else { return };
                let Some(prog) = crate::image::effect::program(crate::image::effect::Effect(effect)) else { return };
                let sample = |x: f32, y: f32| bilinear(img, x, y);
                let (w, h, size) = (rect.w as f32, rect.h as f32, (img.w as f32, img.h as f32));
                each(&mut |x, y| {
                    let px = crate::image::effect::EffectPixel {
                        x: x - rect.x as f32,
                        y: y - rect.y as f32,
                        w,
                        h,
                        params,
                        size,
                        sample: &sample,
                    };
                    scale((prog.cpu)(&px), opacity)
                });
            }
            Prim::Layer { dst, layer, src, opacity, fade } => {
                let Some(la) = self.layers.get(&layer) else { return };
                // A fade into a layer that is not there draws nothing (`batch::drawable`).
                let lb = match fade {
                    Some((id, _)) => match self.layers.get(&id) {
                        Some(l) => Some(l),
                        None => return,
                    },
                    None => None,
                };
                let wgt = fade.map_or(0.0, |(_, w)| w);
                let sample = |l: &Surface, x: i32, y: i32| -> [f32; 4] {
                    let (sx, sy) = (x - dst.x + src.0, y - dst.y + src.1);
                    if sx < 0 || sy < 0 || sx >= l.w || sy >= l.h {
                        [0.0; 4]
                    } else {
                        argb(l.px[(sy * l.w + sx) as usize])
                    }
                };
                each(&mut |x, y| {
                    let (x, y) = (x as i32, y as i32);
                    let mut c = sample(la, x, y);
                    if let Some(lb) = lb {
                        let d = sample(lb, x, y);
                        for i in 0..4 {
                            c[i] = c[i] * (1.0 - wgt) + d[i] * wgt;
                        }
                    }
                    scale(c, opacity)
                });
            }
        }
    }
}

impl Backend for Software {
    fn name(&self) -> &'static str {
        "software"
    }
    fn resize(&mut self, w: i32, h: i32) {
        self.canvas = Surface::new(w, h);
    }
    fn atlas_size(&self) -> i32 {
        crate::text::ATLAS
    }
    fn upload_atlas(&mut self, rect: Rect, atlas: &[u8], aw: i32) {
        for y in rect.y..rect.b() {
            let o = (y * aw) as usize;
            self.atlas[o + rect.x as usize..o + rect.r() as usize]
                .copy_from_slice(&atlas[o + rect.x as usize..o + rect.r() as usize]);
        }
    }
    fn layer_alloc(&mut self, id: u32, w: i32, h: i32) {
        self.layers.insert(id, Surface::new(w, h));
    }
    fn layer_free(&mut self, id: u32) {
        self.layers.remove(&id);
    }
    fn image_upload(&mut self, id: u32, w: i32, h: i32, rgba: &[u8]) {
        let mut s = Surface::new(w, h);
        for (i, p) in rgba.chunks_exact(4).enumerate().take((w * h) as usize) {
            s.px[i] = argb_of_rgba8(p);
        }
        self.images.insert(id, s);
    }
    fn image_free(&mut self, id: u32) {
        self.images.remove(&id);
    }
    fn draw(&mut self, target: Target, clip: Rect, off: (i32, i32), clear: Color, draws: &[Draw]) {
        let mut t = match target {
            Target::Canvas => std::mem::replace(&mut self.canvas, Surface::new(0, 0)),
            Target::Layer(id) => match self.layers.remove(&id) {
                Some(l) => l,
                None => return,
            },
        };
        let clip = clip.intersect(&t.bounds());
        if !clip.is_empty() {
            let c = pack(premul(target.clear(clear), 1.0));
            Self::fill(&mut t, clip, c);
            for d in draws {
                self.raster(&mut t, clip, off, d);
            }
        }
        match target {
            Target::Canvas => self.canvas = t,
            Target::Layer(id) => {
                self.layers.insert(id, t);
            }
        }
    }
    fn present(&mut self, _damage: &[Rect]) {
        #[cfg(windows)]
        if let Some(hwnd) = self.hwnd {
            self.blit(hwnd, _damage);
        }
    }
    fn readback(&mut self) -> Vec<u32> {
        self.canvas.px.clone()
    }
}
