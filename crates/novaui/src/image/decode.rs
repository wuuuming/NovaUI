//! Encoded images (JPEG, PNG, WebP, BMP) to handles: decode, fit, premultiply; a gain-map
//! JPEG's gain map decoded and fitted the same way (`gainmap`). The same bytes
//! and fit give the same handle while one is alive, and a decode asked for twice at once runs
//! once ([`super::cache::WeakCache`], keyed by the bytes' SHA-256).

use super::cache::WeakCache;
use super::resample::{triangle_u8_region, Region};
use super::ImageHandle;
use std::sync::OnceLock;

/// How a decoded image is fitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fit {
    /// Kept whole: scaled down so its longer side is at most this, never up.
    Within(u32),
    /// Scaled to cover a square of this side and cropped to it, centred.
    Square(u32),
}

/// Strong content identity, not a path or an album: equal bytes share, different bytes never do.
type Key = ([u8; 32], usize, Fit);

/// `bytes` decoded, fitted and premultiplied; `None` when they do not decode (or `Square(0)`).
/// Runs on the caller's thread: decode on a worker, not on the UI thread.
pub fn decode(bytes: &[u8], fit: Fit) -> Option<ImageHandle> {
    static CACHE: OnceLock<WeakCache<Key, ImageHandle>> = OnceLock::new();
    CACHE.get_or_init(|| WeakCache::new(512)).get_or_load(key(bytes, fit), || decode_uncached(bytes, fit))
}

fn key(bytes: &[u8], fit: Fit) -> Key {
    use sha2::{Digest, Sha256};
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&Sha256::digest(bytes));
    (digest, bytes.len(), fit)
}

fn decode_uncached(bytes: &[u8], fit: Fit) -> Option<ImageHandle> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = fitted(img, fit)?;
    let rgba = img.into_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    let mut px = rgba.into_raw();
    for p in px.chunks_exact_mut(4) {
        let a = u32::from(p[3]);
        for c in &mut p[..3] {
            *c = ((u32::from(*c) * a + 127) / 255) as u8;
        }
    }
    // Only Windows (D3D11) shows HDR: elsewhere the gain map would be decoded for nothing.
    let gain = if cfg!(any(windows, test)) { gain_map(bytes, w, h, fit) } else { None };
    Some(ImageHandle::with_gain(i32::try_from(w).ok()?, i32::try_from(h).ok()?, px, gain))
}

fn fitted(img: image::DynamicImage, fit: Fit) -> Option<image::DynamicImage> {
    Some(match fit {
        Fit::Within(side) if img.width().max(img.height()) > side => resize(img, side, false)?,
        Fit::Within(_) => img,
        Fit::Square(side) => resize(img, side, true)?,
    })
}

/// The gain map `bytes` carry, fitted as the picture was and brought to its `w`x`h` (gain maps
/// are usually smaller: a quarter of each side). A gain map that does not decode is left out:
/// the picture shows as SDR.
fn gain_map(bytes: &[u8], w: u32, h: u32, fit: Fit) -> Option<super::gainmap::Gain> {
    let (meta, jpeg) = super::gainmap::find(bytes)?;
    let g = image::load_from_memory(jpeg).ok()?;
    let gray = g.color().channel_count() < 3;
    // The same fit at the gain map's own scale keeps its crop the picture's; then to the size.
    let (gw, gh) = (g.width(), g.height());
    let own = match fit {
        Fit::Square(_) => Fit::Square(gw.min(gh)),
        Fit::Within(_) => Fit::Within(gw.max(gh)),
    };
    let g = fitted(g, own)?;
    let g = g.resize_exact(w, h, image::imageops::FilterType::Triangle);
    let (channels, px) = if gray { (1, g.into_luma8().into_raw()) } else { (3, g.into_rgb8().into_raw()) };
    Some(super::gainmap::Gain { meta, channels, px })
}

/// image's fit (`resize`) or fill (`resize_to_fill`) dimensions and Triangle pixels, streaming the
/// common 8-bit layouts. Consumes the source so a large decoded buffer goes before the RGBA
/// conversion and the premultiply.
fn resize(img: image::DynamicImage, side: u32, fill: bool) -> Option<image::DynamicImage> {
    use image::DynamicImage as D;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return None;
    }
    let wr = f64::from(side) / f64::from(w);
    let hr = f64::from(side) / f64::from(h);
    let ratio = if fill { wr.max(hr) } else { wr.min(hr) };
    let (nw, nh) = ((f64::from(w) * ratio).round().max(1.0), (f64::from(h) * ratio).round().max(1.0));
    let max = f64::from(u32::MAX);
    let (nw, nh) = if nw > max {
        (u32::MAX, (f64::from(h) * (max / f64::from(w))).round().max(1.0) as u32)
    } else if nh > max {
        ((f64::from(w) * (max / f64::from(h))).round().max(1.0) as u32, u32::MAX)
    } else {
        (nw as u32, nh as u32)
    };
    let region = if fill {
        if side == 0 || nw < side || nh < side {
            return None;
        }
        Region { x: (nw - side) / 2, y: (nh - side) / 2, width: side, height: side }
    } else {
        Region { x: 0, y: 0, width: nw, height: nh }
    };
    macro_rules! stream {
        ($buffer:ident, $variant:ident, $pixel:ty, $channels:expr) => {{
            let bytes = triangle_u8_region($buffer.as_raw(), w, h, $channels, nw, nh, region).ok()?;
            let mut out = image::ImageBuffer::<$pixel, Vec<u8>>::from_raw(region.width, region.height, bytes)?;
            out.set_color_space($buffer.color_space()).ok()?;
            D::$variant(out)
        }};
    }
    Some(match img {
        D::ImageLuma8(buffer) => stream!(buffer, ImageLuma8, image::Luma<u8>, 1),
        D::ImageLumaA8(buffer) => stream!(buffer, ImageLumaA8, image::LumaA<u8>, 2),
        D::ImageRgb8(buffer) => stream!(buffer, ImageRgb8, image::Rgb<u8>, 3),
        D::ImageRgba8(buffer) => stream!(buffer, ImageRgba8, image::Rgba<u8>, 4),
        // Keep high-bit-depth behaviour; never quantize a source to save intermediate memory.
        other => {
            let out = other.resize_exact(nw, nh, image::imageops::FilterType::Triangle);
            if fill {
                out.crop_imm(region.x, region.y, region.width, region.height)
            } else {
                out
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_triangle_fit_fill_matches_original() {
        use image::{imageops::FilterType, DynamicImage as D, ImageBuffer};
        for (w, h) in [(1, 1), (1, 53), (79, 1), (31, 73), (191, 43)] {
            let sources = [
                D::ImageLuma8(ImageBuffer::from_fn(w, h, |x, y| image::Luma([(x * 71 + y * 37) as u8]))),
                D::ImageLumaA8(ImageBuffer::from_fn(w, h, |x, y| image::LumaA([(x * 13) as u8, (y * 43) as u8]))),
                D::ImageRgb8(ImageBuffer::from_fn(w, h, |x, y| {
                    image::Rgb([(x * 71) as u8, (y * 37) as u8, (x + y) as u8])
                })),
                D::ImageRgba8(ImageBuffer::from_fn(w, h, |x, y| {
                    image::Rgba([(x * 17) as u8, (y * 39) as u8, (x + y) as u8, (x * y) as u8])
                })),
                D::ImageRgb16(ImageBuffer::from_fn(w, h, |x, y| {
                    image::Rgb([(x * 1237) as u16, (y * 733) as u16, (x * y) as u16])
                })),
            ];
            for src in sources {
                for side in [1, 3, 16, 96] {
                    for fill in [false, true] {
                        let expected = if fill {
                            src.resize_to_fill(side, side, FilterType::Triangle)
                        } else {
                            src.resize(side, side, FilterType::Triangle)
                        };
                        let got = resize(src.clone(), side, fill).unwrap();
                        assert_eq!(
                            (got.width(), got.height(), got.color()),
                            (expected.width(), expected.height(), expected.color())
                        );
                        let (a, b) = (got.into_rgba8().into_raw(), expected.into_rgba8().into_raw());
                        assert_eq!(
                            a.iter().zip(&b).filter(|(a, b)| a != b).count(),
                            0,
                            "{w}x{h} side={side} fill={fill}"
                        );
                        assert_eq!(a.len(), b.len());
                    }
                }
            }
        }
    }

    #[test]
    fn a_gain_map_comes_with_its_picture_fitted_alike() {
        use crate::image::gainmap::tests::{jpeg, ultra_hdr, XMP};
        // A 16x8 picture whose gain map (8x4) is full on the right half only.
        let primary = jpeg(16, 8, |_, _| image::Rgb([200, 200, 200]), false);
        let gain = jpeg(8, 4, |x, _| image::Rgb([if x >= 4 { 255 } else { 0 }; 3]), true);
        let file = ultra_hdr(&primary, &gain, XMP);
        let plain = decode(&primary, Fit::Within(16)).unwrap();
        let hdr = decode(&file, Fit::Within(16)).unwrap();
        assert!(!plain.has_gain_map() && hdr.has_gain_map());
        // The same picture: SDR outputs show it as before.
        assert_eq!(plain.pixels(), hdr.pixels());
        let px = hdr.hdr_pixels(4.0).unwrap();
        let at = |x: usize, y: usize| crate::image::gainmap::f32_of(px[(y * 16 + x) * 4]);
        assert!(at(1, 4) < 0.8 && at(14, 4) > 1.0, "{} {}", at(1, 4), at(14, 4));
        // Cropped to a square: the right half's gain stays on the right.
        let sq = decode(&file, Fit::Square(8)).unwrap();
        let px = sq.hdr_pixels(4.0).unwrap();
        let at = |x: usize| crate::image::gainmap::f32_of(px[(4 * 8 + x) * 4]);
        assert!(at(0) < 0.8 && at(7) > 1.0, "{} {}", at(0), at(7));
    }
}
