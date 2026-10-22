//! The closed primitive set. Every primitive has one expression in `prims.wgsl` (GPU) and the same
//! expression in `software.rs` (reference); image effects bring both their own (`image::effect`); the pixel test holds them within 2/255.

use crate::geom::*;

/// A rounded rectangle's corners, as bits of a mask (`Prim::Quad::square`, `Visual::square`):
/// `novaui::corner`.
pub mod corner {
    pub const TOP_LEFT: u8 = 1;
    pub const TOP_RIGHT: u8 = 2;
    pub const BOTTOM_RIGHT: u8 = 4;
    pub const BOTTOM_LEFT: u8 = 8;
    pub const TOP: u8 = TOP_LEFT | TOP_RIGHT;
    pub const BOTTOM: u8 = BOTTOM_LEFT | BOTTOM_RIGHT;
    pub const LEFT: u8 = TOP_LEFT | BOTTOM_LEFT;
    pub const RIGHT: u8 = TOP_RIGHT | BOTTOM_RIGHT;
    pub const ALL: u8 = TOP | BOTTOM;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Prim {
    /// Rounded rectangle with an optional inner border. `square`: the corners left square, a mask
    /// of [`corner`] bits (0: all rounded).
    Quad { rect: Rect, radius: f32, square: u8, fill: Color, border: f32, border_color: Color },
    /// Gaussian shadow of a rounded rectangle (Evan Wallace's closed form along x, 4 taps in y).
    /// `cut`: nothing inside this rounded rect (the plate a translucent element's shadow falls
    /// from; the shadow itself is offset from it by its dy).
    Shadow { rect: Rect, radius: f32, sigma: f32, color: Color, cut: Option<Rect> },
    /// A material over what is already drawn under `rect` (acrylic): the target as drawn so far
    /// in `rect` grown by `extent`, blurred by `levels` dual-Kawase passes (Bjorge, SIGGRAPH
    /// 2015), its luminance pulled toward `lum` (rgb, opacity in a), tinted by `tint` (rgb,
    /// opacity in a), with 2 % static noise; in the rounded rect, times `opacity`. The tree
    /// guarantees the grown rect is freshly drawn in the same draw (`Tree::frame`).
    Backdrop { rect: Rect, radius: f32, levels: u8, extent: i32, tint: Color, lum: Color, opacity: f32 },
    /// A8 coverage glyph from the atlas; (x, y) is the top-left target pixel.
    Glyph { x: i32, y: i32, uv: Rect, color: Color },
    /// A8 coverage glyph from the atlas through an affine map, sampled bilinear (zero outside its
    /// cell): glyph pixel (u, v), 0..uv.w by 0..uv.h, lands at
    /// `o + (m[0]·u + m[2]·v, m[1]·u + m[3]·v)`. A moving, scaled or turned text unit
    /// (`style::Units`); at rest a unit is a `Glyph`. `color` before the karaoke edge, `lit`
    /// behind it: `sweep` is how far the glyph is sung (`text::glyph_swept`'s `k`).
    GlyphXf { o: [f32; 2], m: [f32; 4], uv: Rect, color: Color, lit: Color, sweep: f32 },
    /// RGBA8 premultiplied image, bilinear, `uv` normalised (u0, v0, u1, v1), rounded clip. With
    /// `hdr`, its HDR rendition where the backend shows HDR (`Backend::set_output`).
    Image { rect: Rect, image: u32, uv: [f32; 4], radius: f32, opacity: f32, hdr: bool },
    /// An image drawn over `rect` through the registered image effect `effect`
    /// (`image::effect`), which reads `params`; times `opacity`.
    Effect { rect: Rect, image: u32, effect: u8, params: [f32; 4], opacity: f32 },
    /// Samples a layer texture: target pixel p reads layer at p - dst.xy + src.
    /// With `fade`, c = (1 - w)·A + w·B. Then multiplied by opacity.
    Layer { dst: Rect, layer: u32, src: (i32, i32), opacity: f32, fade: Option<(u32, f32)> },
}

impl Prim {
    pub fn bounds(&self) -> Rect {
        match *self {
            Prim::Quad { rect, .. }
            | Prim::Image { rect, .. }
            | Prim::Effect { rect, .. }
            | Prim::Backdrop { rect, .. } => rect,
            Prim::Shadow { rect, sigma, .. } => rect.inflate((sigma * 3.0).ceil() as i32 + 1),
            Prim::Glyph { x, y, uv, .. } => Rect::new(x, y, uv.w, uv.h),
            Prim::GlyphXf { o, m, uv, .. } => {
                let (w, h) = (uv.w as f32, uv.h as f32);
                let xs = [0.0, m[0] * w, m[2] * h, m[0] * w + m[2] * h];
                let ys = [0.0, m[1] * w, m[3] * h, m[1] * w + m[3] * h];
                let lo = |v: [f32; 4]| v.iter().fold(f32::INFINITY, |a, b| a.min(*b));
                let hi = |v: [f32; 4]| v.iter().fold(f32::NEG_INFINITY, |a, b| a.max(*b));
                // A pixel whose centre falls within a texel of the cell can read it.
                let x0 = (o[0] + lo(xs) - 1.0).floor() as i32;
                let y0 = (o[1] + lo(ys) - 1.0).floor() as i32;
                let x1 = (o[0] + hi(xs) + 1.0).ceil() as i32;
                let y1 = (o[1] + hi(ys) + 1.0).ceil() as i32;
                Rect::new(x0, y0, x1 - x0, y1 - y0)
            }
            Prim::Layer { dst, .. } => dst,
        }
    }
    /// The same primitive moved by (dx, dy).
    pub fn translated(&self, dx: i32, dy: i32) -> Prim {
        let mut p = *self;
        match &mut p {
            Prim::Shadow { rect, cut, .. } => {
                *rect = rect.translate(dx, dy);
                if let Some(c) = cut {
                    *c = c.translate(dx, dy);
                }
            }
            Prim::Quad { rect, .. }
            | Prim::Image { rect, .. }
            | Prim::Effect { rect, .. }
            | Prim::Backdrop { rect, .. } => *rect = rect.translate(dx, dy),
            Prim::Glyph { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
            Prim::GlyphXf { o, .. } => {
                o[0] += dx as f32;
                o[1] += dy as f32;
            }
            Prim::Layer { dst, .. } => *dst = dst.translate(dx, dy),
        }
        p
    }
    /// Scales the primitive's alpha (used for element opacity without a layer).
    pub fn faded(&self, k: f32) -> Prim {
        let mut p = *self;
        match &mut p {
            Prim::Quad { fill, border_color, .. } => {
                fill[3] *= k;
                border_color[3] *= k;
            }
            Prim::Shadow { color, .. } | Prim::Glyph { color, .. } => color[3] *= k,
            Prim::GlyphXf { color, lit, .. } => {
                color[3] *= k;
                lit[3] *= k;
            }
            Prim::Image { opacity, .. }
            | Prim::Effect { opacity, .. }
            | Prim::Layer { opacity, .. }
            | Prim::Backdrop { opacity, .. } => *opacity *= k,
        }
        p
    }
}

/// Dual-Kawase passes and the margin they read, for a blur of standard deviation `sigma` px.
/// No blur (`sigma` 0, a flat material): no passes, nothing read beyond the plate.
pub fn kawase(sigma: f32) -> (u8, i32) {
    if sigma <= 0.0 {
        return (0, 0);
    }
    let n = (sigma.max(2.0) / 2.0).log2().ceil().clamp(1.0, 6.0) as u8;
    (n, 3 << n)
}

/// The region a material reads and blurs: its plate grown by the margin the passes read
/// (`kawase`), cut to `clip` (already cut to the target).
pub fn backdrop_region(rect: Rect, extent: i32, clip: Rect) -> Rect {
    rect.inflate(extent).intersect(&clip)
}

/// The order of a Kawase chain's passes: down from level 0 to `levels`, then back up; each
/// `(from, to, up)` reads level `from` and writes level `to`.
pub fn kawase_passes(levels: u8) -> impl Iterator<Item = (usize, usize, bool)> {
    let n = levels as usize;
    (0..n).map(|k| (k, k + 1, false)).chain((0..n).rev().map(|k| (k + 1, k, true)))
}

/// A shadow's blur is at least half a pixel, on every backend.
pub fn shadow_sigma(sigma: f32) -> f32 {
    sigma.max(0.5)
}

/// Logical sizes of the Kawase chain over a `w`×`h` region: level 0 is the region itself.
pub fn kawase_sizes(w: i32, h: i32, levels: u8) -> Vec<(i32, i32)> {
    let mut v = vec![(w.max(1), h.max(1))];
    for _ in 0..levels {
        let (a, b) = *v.last().expect("the chain starts with level 0");
        v.push(((a + 1) / 2, (b + 1) / 2));
    }
    v
}

/// The material's static noise, the same integer hash on every backend (0..=1).
pub fn noise(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374761393).wrapping_add((y as u32).wrapping_mul(668265263));
    h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    h ^= h >> 16;
    (h & 0xFFFF) as f32 / 65535.0
}

pub const NOISE_OPACITY: f32 = 0.02;

/// A primitive as submitted: clipped to `clip` (target coordinates) in addition to the draw's
/// scissor. Rectangular clips are exact and free: they shrink the covered box, not the shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Draw {
    pub prim: Prim,
    pub clip: Rect,
}

impl Draw {
    /// What of the target this draw may touch when drawn at `off` within `clip`: its
    /// primitive's bounds, cut to both clips.
    pub fn cover(&self, clip: Rect, off: (i32, i32)) -> Rect {
        self.prim.bounds().translate(off.0, off.1).intersect(&clip).intersect(&self.clip.translate(off.0, off.1))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    Canvas,
    Layer(u32),
}

impl Target {
    /// What a draw clears this target to: the canvas to `canvas`, a layer to transparent.
    pub fn clear(self, canvas: Color) -> Color {
        match self {
            Target::Canvas => canvas,
            Target::Layer(_) => [0.0; 4],
        }
    }
}

/// A pixel of RGBA8 bytes as `Backend::readback` gives it, 0xAARRGGBB.
pub fn argb_of_rgba8(p: &[u8]) -> u32 {
    (p[3] as u32) << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32
}

/// Whether `r` of an atlas `aw` pixels wide lies inside it and its rows inside `len` bytes.
pub fn atlas_rows_fit(r: Rect, aw: i32, len: usize) -> bool {
    if r.is_empty() || r.x < 0 || r.y < 0 || r.r() > aw {
        return false;
    }
    let end = ((r.b() - 1) * aw + r.r()) as usize;
    len >= end
}

/// Framework-owned GPU texture capacities, excluding swap-chain buffers and driver caches.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BackendResources {
    pub texture_bytes: u64,
    pub canvas_bytes: u64,
    pub atlas_bytes: u64,
    pub layer_bytes: u64,
    pub image_bytes: u64,
    pub scratch_bytes: u64,
}

/// An HDR output (`Backend::set_output`): the luminance SDR white is shown at and the display's
/// peak, in nits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrOutput {
    pub sdr_white: f32,
    pub peak: f32,
}

impl HdrOutput {
    /// How far above SDR white the display goes (≥ 1): what an HDR image may use.
    pub fn headroom(&self) -> f32 {
        (self.peak / self.sdr_white.max(1.0)).max(1.0)
    }
}

/// Wakes the host's loop from any thread (each platform's own wake).
pub type Wake = std::sync::Arc<dyn Fn() + Send + Sync>;

/// Images of this many pixels and more have their textures made off the UI thread
/// (`Backend::image_make`) by the GPU backends. Smaller ones (thumbnails, icons) are made at once:
/// they cost little, and a list row that came into view without its thumbnail for a frame would
/// show.
pub(crate) const MAKE_APART_PX: i64 = 256 * 256;

/// Whether a GPU backend makes the texture of a `w`×`h` image off the UI thread (`MAKE_APART_PX`).
pub(crate) fn make_apart(w: i32, h: i32) -> bool {
    i64::from(w) * i64::from(h) >= MAKE_APART_PX
}

/// The backend contract. `draw` first clears `clip` (canvas: `clear`, layer: transparent), then
/// draws `draws` translated by `offset`, each clipped to its own clip and to `clip`.
pub trait Backend {
    fn name(&self) -> &'static str;
    fn resize(&mut self, w: i32, h: i32);
    /// Copies `rect` of the atlas (`atlas_w` pixels a row) to the backend's; after a reset, the
    /// whole atlas.
    fn upload_atlas(&mut self, rect: Rect, atlas: &[u8], atlas_w: i32);
    /// The atlas's side in pixels.
    fn atlas_size(&self) -> i32;
    /// Ensures capacity; contents are invalid afterward. The caller must draw/clear the
    /// complete logical layer before sampling it. Physical capacity outside it is never sampled.
    fn layer_alloc(&mut self, id: u32, w: i32, h: i32);
    fn layer_free(&mut self, id: u32);
    /// RGBA8 premultiplied pixels, row-major. `id` names one image's content (equal pixels
    /// share it, `images::ImageHandle`); a re-upload of the same id replaces the texture.
    fn image_upload(&mut self, id: u32, w: i32, h: i32, rgba: &[u8]);
    /// The tree's way to upload `img`: `image_upload` at once, unless the backend makes the
    /// texture off the UI thread. Until then `draw` leaves the image out; once it is made, `wake`
    /// runs (on the making thread) and `images_made` names it. Without a `wake` a backend makes
    /// it at once.
    fn image_make(&mut self, img: &crate::image::ImageHandle, wake: Option<&Wake>) {
        let _ = wake;
        self.image_upload(img.id(), img.width(), img.height(), img.pixels());
    }
    /// Adds to `made` the images made off the UI thread since the last call (`image_make`).
    fn images_made(&mut self, made: &mut Vec<u32>) {
        let _ = made;
    }
    fn image_free(&mut self, id: u32);
    /// Finish resource bookkeeping even on a frame that did not need presentation.
    fn end_frame(&mut self) {}
    fn draw(&mut self, target: Target, clip: Rect, offset: (i32, i32), clear: Color, draws: &[Draw]);
    /// Makes `damage` visible.
    fn present(&mut self, damage: &[Rect]);
    /// Canvas contents as 0xAARRGGBB, for verification only; empty when the device could not
    /// read them back.
    fn readback(&mut self) -> Vec<u32>;
    /// Blocks until the swap chain can take another frame (frame-latency waitable).
    fn wait_frame(&mut self) {}
    /// True when a frame can be queued without waiting (the previous present retired).
    fn frame_ready(&mut self) -> bool {
        true
    }
    fn finish(&mut self) {}
    /// Presents without waiting for vblank (used while the window is being resized: a frame
    /// per size step matters more than tear-free pacing).
    fn set_immediate(&mut self, _on: bool) {}
    /// A resize has settled: give back memory the driver keeps for released resources, and any
    /// surface a live resize grew past the window. Presents itself when it remakes what is shown.
    fn trim(&mut self) {}
    /// Images' textures were freed while the window stays still: give back the memory the driver
    /// keeps for released resources (`trim`'s first half; nothing is presented).
    fn release_pools(&mut self) {}
    fn adapter(&self) -> String {
        String::new()
    }
    #[cfg(test)]
    fn resources(&self) -> Option<BackendResources> {
        None
    }
    /// The output the window is shown on: HDR (`Some`) or SDR. On an HDR output the backend
    /// draws into half-float targets and presents linear light (scRGB), SDR content at the
    /// output's SDR white, so that it looks as it does on SDR; images drawn with `hdr` show their
    /// HDR rendition. `true` when it remade its targets: everything must be drawn and uploaded
    /// again (`Tree::invalidate_all`). A backend without HDR keeps SDR.
    fn set_output(&mut self, hdr: Option<HdrOutput>) -> bool {
        let _ = hdr;
        false
    }
    /// The device was lost: the host must recreate the backend.
    fn lost(&self) -> bool {
        false
    }
}
