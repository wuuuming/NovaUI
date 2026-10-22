//! Gain-map JPEG (Ultra HDR, Adobe's gain map): an SDR JPEG that carries a second, small JPEG,
//! the gain map, and the metadata that turns the two into the HDR picture (`hdrgm` XMP). An SDR
//! output shows the SDR picture as it is; an HDR output shows it raised by the gain map, as far
//! as its headroom allows (`Gain::hdr`).
//!
//! Found through the primary image's MPF index (its second image) or, without one, the XMP
//! container's gain-map length (the file's last bytes). The metadata is the gain map's XMP, else
//! the primary's. Only XMP (`hdrgm`, version 1.0) is read; ISO 21496-1's binary form, written
//! beside it by the same encoders, is not. An HDR base rendition (`BaseRenditionIsHDR`) is not
//! taken: the picture is shown as plain SDR.
//!
//! Only the D3D11 backend (Windows) shows HDR; elsewhere `decode` does not read gain maps.
#![cfg_attr(not(windows), allow(dead_code))]

/// The `hdrgm` metadata, per channel (one value given stands for all three). Logs are base 2.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Meta {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub gamma: [f32; 3],
    pub offset_sdr: [f32; 3],
    pub offset_hdr: [f32; 3],
    pub capacity_min: f32,
    pub capacity_max: f32,
}

/// A decoded gain map, fitted like the image it belongs to: one or three channels per pixel.
#[derive(Clone, PartialEq)]
pub(crate) struct Gain {
    pub meta: Meta,
    pub channels: usize,
    pub px: Vec<u8>,
}

/// The gain map's JPEG and the metadata, when `jpeg` carries them.
pub(crate) fn find(jpeg: &[u8]) -> Option<(Meta, &[u8])> {
    if !jpeg.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let primary = segments(jpeg);
    let primary_xmp = primary.iter().find_map(|s| xmp(s));
    let gain = mpf_second(jpeg, &primary)
        .or_else(|| {
            let len: usize = attr(primary_xmp?, "Item:Length", Some("GainMap"))?.parse().ok()?;
            jpeg.len().checked_sub(len).map(|at| &jpeg[at..])
        })
        .filter(|g| g.starts_with(&[0xFF, 0xD8]))?;
    let gain_xmp = segments(gain).iter().find_map(|s| xmp(s));
    let meta = gain_xmp.and_then(meta).or_else(|| primary_xmp.and_then(meta))?;
    Some((meta, gain))
}

/// The APP segments before the first scan: (marker, payload).
fn segments(jpeg: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    let mut i = 2;
    while i + 4 <= jpeg.len() && jpeg[i] == 0xFF {
        let marker = jpeg[i + 1];
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let len = usize::from(u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]));
        if len < 2 || i + 2 + len > jpeg.len() {
            break;
        }
        out.push((marker, &jpeg[i + 4..i + 2 + len]));
        i += 2 + len;
    }
    out
}

/// An APP1 XMP packet's text.
fn xmp<'a>(seg: &(u8, &'a [u8])) -> Option<&'a str> {
    const NS: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
    (seg.0 == 0xE1).then_some(())?;
    std::str::from_utf8(seg.1.strip_prefix(NS)?).ok()
}

/// The second image the primary's MPF index (APP2 `MPF\0`) names: offsets count from the index's
/// TIFF header.
fn mpf_second<'a>(jpeg: &'a [u8], segs: &[(u8, &'a [u8])]) -> Option<&'a [u8]> {
    let tiff = segs.iter().find_map(|&(m, p)| (m == 0xE2).then(|| p.strip_prefix(b"MPF\0")).flatten())?;
    // Where the TIFF header sits in the file: offsets are relative to it.
    let base = tiff.as_ptr() as usize - jpeg.as_ptr() as usize;
    let le = match tiff.get(..4)? {
        [b'I', b'I', 42, 0] => true,
        [b'M', b'M', 0, 42] => false,
        _ => return None,
    };
    let u16_at = |o: usize| {
        tiff.get(o..o + 2).map(|b| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
    };
    let u32_at = |o: usize| {
        tiff.get(o..o + 4).map(|b| {
            let a = [b[0], b[1], b[2], b[3]];
            if le {
                u32::from_le_bytes(a)
            } else {
                u32::from_be_bytes(a)
            }
        })
    };
    let ifd = u32_at(4)? as usize;
    let n = usize::from(u16_at(ifd)?);
    let entries = (0..n).find_map(|k| {
        let e = ifd + 2 + 12 * k;
        (u16_at(e)? == 0xB002).then(|| Some((u32_at(e + 4)? as usize, u32_at(e + 8)? as usize))).flatten()
    })?;
    let (count, at) = entries;
    if count < 32 {
        return None;
    }
    // The second 16-byte entry: attributes, size, offset.
    let size = u32_at(at + 16 + 4)? as usize;
    let offset = u32_at(at + 16 + 8)? as usize;
    let start = base.checked_add(offset)?;
    jpeg.get(start..start.checked_add(size)?)
}

/// `hdrgm` metadata from an XMP packet; `None` without `hdrgm:Version` or with an HDR base.
fn meta(x: &str) -> Option<Meta> {
    attr(x, "hdrgm:Version", None)?;
    if attr(x, "hdrgm:BaseRenditionIsHDR", None).is_some_and(|v| v.eq_ignore_ascii_case("true")) {
        return None;
    }
    let three = |name: &str, default: f32| -> Option<[f32; 3]> {
        let v = values(x, name);
        match v.len() {
            0 => Some([default; 3]),
            1 => Some([v[0]; 3]),
            3 => Some([v[0], v[1], v[2]]),
            _ => None,
        }
    };
    let one = |name: &str, default: Option<f32>| values(x, name).first().copied().or(default);
    let max = three("hdrgm:GainMapMax", f32::NAN)?;
    if max.iter().any(|v| v.is_nan()) {
        return None;
    }
    let capacity_max = one("hdrgm:HDRCapacityMax", None)?;
    let m = Meta {
        min: three("hdrgm:GainMapMin", 0.0)?,
        max,
        gamma: three("hdrgm:Gamma", 1.0)?,
        offset_sdr: three("hdrgm:OffsetSDR", 1.0 / 64.0)?,
        offset_hdr: three("hdrgm:OffsetHDR", 1.0 / 64.0)?,
        capacity_min: one("hdrgm:HDRCapacityMin", Some(0.0))?,
        capacity_max,
    };
    let finite =
        m.min.iter().chain(&m.max).chain(&m.gamma).chain(&m.offset_sdr).chain(&m.offset_hdr).all(|v| v.is_finite());
    (finite && m.gamma.iter().all(|&g| g > 0.0) && m.capacity_min.is_finite() && m.capacity_max.is_finite())
        .then_some(m)
}

/// A property's numbers: an attribute (`name="1.5"`), an element (`<name>1.5</name>`) or a
/// sequence of them (`<name><rdf:Seq><rdf:li>1</rdf:li>…`).
fn values(x: &str, name: &str) -> Vec<f32> {
    if let Some(v) = attr(x, name, None) {
        return v.trim().parse().map(|v| vec![v]).unwrap_or_default();
    }
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let Some(a) = x.find(&open).map(|a| a + open.len()) else { return Vec::new() };
    let Some(b) = x[a..].find(&close).map(|b| a + b) else { return Vec::new() };
    let body = &x[a..b];
    if !body.contains("<rdf:li") {
        return body.trim().parse().map(|v| vec![v]).unwrap_or_default();
    }
    body.split("<rdf:li")
        .skip(1)
        .filter_map(|li| {
            let v = &li[li.find('>')? + 1..];
            v[..v.find('<')?].trim().parse().ok()
        })
        .collect()
}

/// An attribute's value (`name="v"`); with `item`, the one in the element that also has
/// `Item:Semantic="item"` (the XMP container's directory).
fn attr<'a>(x: &'a str, name: &str, item: Option<&str>) -> Option<&'a str> {
    let key = format!("{name}=");
    let mut from = 0;
    while let Some(at) = x[from..].find(&key).map(|a| from + a) {
        from = at + key.len();
        let rest = &x[from..];
        let q = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let end = rest[1..].find(q)? + 1;
        let v = &rest[1..end];
        match item {
            None => return Some(v),
            Some(sem) => {
                // The element the attribute is in: from its `<` to its `>`.
                let start = x[..at].rfind('<').unwrap_or(0);
                let stop = x[at..].find('>').map_or(x.len(), |e| at + e);
                if x[start..stop].contains(&format!("Item:Semantic=\"{sem}\"")) {
                    return Some(v);
                }
            }
        }
    }
    None
}

impl Gain {
    /// The HDR rendition for an output with `headroom` (its peak over SDR white, ≥ 1), from the
    /// SDR picture's premultiplied RGBA8 `sdr`: RGBA half floats, sRGB-encoded (extended past
    /// 1.0, which is where SDR white stays) and premultiplied, as an HDR canvas holds colour.
    pub fn hdr(&self, sdr: &[u8], headroom: f32) -> Vec<u16> {
        let m = &self.meta;
        let h = headroom.max(1.0).log2();
        let weight = if m.capacity_max > m.capacity_min {
            ((h - m.capacity_min) / (m.capacity_max - m.capacity_min)).clamp(0.0, 1.0)
        } else if h >= m.capacity_max {
            1.0
        } else {
            0.0
        };
        // Per channel and gain byte: the boost's log, already weighted.
        let mut boost = [[0f32; 256]; 3];
        for (c, table) in boost.iter_mut().enumerate() {
            for (g, b) in table.iter_mut().enumerate() {
                let r = (g as f32 / 255.0).powf(1.0 / m.gamma[c]);
                *b = (m.min[c] * (1.0 - r) + m.max[c] * r) * weight;
                *b = b.exp2();
            }
        }
        let mut out = Vec::with_capacity(sdr.len());
        for (k, p) in sdr.chunks_exact(4).enumerate() {
            let a = f32::from(p[3]) / 255.0;
            for c in 0..3 {
                let g = self.px[k * self.channels + if self.channels == 3 { c } else { 0 }];
                // Straight colour, linear light.
                let s = if p[3] == 0 { 0.0 } else { srgb_to_linear(f32::from(p[c]) / 255.0 / a) };
                let lin = (s + m.offset_sdr[c]) * boost[c][usize::from(g)] - m.offset_hdr[c];
                out.push(f16(linear_to_srgb(lin.max(0.0)) * a));
            }
            out.push(f16(a));
        }
        out
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB's encoding, carried on past 1.0 (an HDR value encodes above 1).
fn linear_to_srgb(l: f32) -> f32 {
    if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// `v` as an IEEE half, rounded to nearest even; past the largest half, the largest.
pub(crate) fn f16(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xFF) as i32;
    let man = b & 0x7F_FFFF;
    if exp == 0xFF {
        return sign | if man != 0 { 0x7E00 } else { 0x7C00 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1F {
        return sign | 0x7BFF;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        // Subnormal: the implicit bit joins the mantissa, shifted into place.
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = m >> shift;
        let rest = m & ((1 << shift) - 1);
        let mid = 1 << (shift - 1);
        let r = half + u32::from(rest > mid || (rest == mid && half & 1 == 1));
        return sign | r as u16;
    }
    let half = ((e as u32) << 10) | (man >> 13);
    let rest = man & 0x1FFF;
    let r = half + u32::from(rest > 0x1000 || (rest == 0x1000 && half & 1 == 1));
    sign | if r >= 0x7C00 { 0x7BFF } else { r as u16 }
}

/// A half as `f32` (readback, tests).
pub(crate) fn f32_of(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = i32::from((h >> 10) & 0x1F);
    let m = f32::from(h & 0x3FF);
    sign * match e {
        0 => m * (2f32).powi(-24),
        0x1F => f32::INFINITY,
        _ => (1.0 + m / 1024.0) * (2f32).powi(e - 15),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An Ultra HDR file as encoders write it: the primary with its XMP (the container's
    /// directory) and MPF index, then the gain map with its `hdrgm` XMP.
    pub(crate) fn ultra_hdr(primary: &[u8], gain: &[u8], gain_xmp: &str) -> Vec<u8> {
        let app = |marker: u8, payload: &[u8]| {
            let mut s = vec![0xFF, marker];
            s.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
            s.extend_from_slice(payload);
            s
        };
        let xmp_of = |x: &str| {
            let mut p = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
            p.extend_from_slice(x.as_bytes());
            p
        };
        let gain_file = [&gain[..2], &app(0xE1, &xmp_of(gain_xmp)), &gain[2..]].concat();
        let container = format!(
            "<x:xmpmeta><rdf:Description hdrgm:Version=\"1.0\"><Container:Directory><rdf:Seq>\
             <rdf:li><Container:Item Item:Semantic=\"Primary\" Item:Mime=\"image/jpeg\"/></rdf:li>\
             <rdf:li><Container:Item Item:Semantic=\"GainMap\" Item:Mime=\"image/jpeg\" Item:Length=\"{}\"/></rdf:li>\
             </rdf:Seq></Container:Directory></rdf:Description></x:xmpmeta>",
            gain_file.len()
        );
        // MPF: a little-endian TIFF header, one IFD with the entry tag, two 16-byte entries.
        let mpf = |primary_len: u32, gain_off: u32| {
            let mut t = b"MPF\0II\x2A\0\x08\0\0\0".to_vec();
            t.extend_from_slice(&1u16.to_le_bytes());
            t.extend_from_slice(&0xB002u16.to_le_bytes());
            t.extend_from_slice(&7u16.to_le_bytes());
            t.extend_from_slice(&32u32.to_le_bytes());
            t.extend_from_slice(&26u32.to_le_bytes()); // entries at TIFF offset 8 + 2 + 12 + 4
            t.extend_from_slice(&0u32.to_le_bytes());
            for (size, off) in [(primary_len, 0u32), (gain_file.len() as u32, gain_off)] {
                t.extend_from_slice(&0u32.to_le_bytes());
                t.extend_from_slice(&size.to_le_bytes());
                t.extend_from_slice(&off.to_le_bytes());
                t.extend_from_slice(&0u32.to_le_bytes());
            }
            t
        };
        let head = [&primary[..2], &app(0xE1, &xmp_of(&container))].concat();
        let mpf_len = app(0xE2, &mpf(0, 0)).len();
        let primary_len = (head.len() + mpf_len + primary.len() - 2) as u32;
        // The TIFF header sits 4 (marker, length) + 4 ("MPF\0") bytes into the APP2 segment.
        let tiff_at = (head.len() + 8) as u32;
        let file = [head, app(0xE2, &mpf(primary_len, primary_len - tiff_at)), primary[2..].to_vec()].concat();
        [file, gain_file].concat()
    }

    pub(crate) fn jpeg(w: u32, h: u32, f: impl Fn(u32, u32) -> image::Rgb<u8>, gray: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 100);
        if gray {
            let img = image::GrayImage::from_fn(w, h, |x, y| image::Luma([f(x, y).0[0]]));
            img.write_with_encoder(enc).unwrap();
        } else {
            image::RgbImage::from_fn(w, h, f).write_with_encoder(enc).unwrap();
        }
        out
    }

    pub(crate) const XMP: &str = "<x:xmpmeta><rdf:Description hdrgm:Version=\"1.0\" hdrgm:GainMapMin=\"0\" \
        hdrgm:GainMapMax=\"2\" hdrgm:Gamma=\"1\" hdrgm:OffsetSDR=\"0\" hdrgm:OffsetHDR=\"0\" \
        hdrgm:HDRCapacityMin=\"0\" hdrgm:HDRCapacityMax=\"2\"/></x:xmpmeta>";

    #[test]
    fn finds_the_gain_map_by_mpf_and_by_the_container() {
        let primary = jpeg(8, 8, |_, _| image::Rgb([128, 128, 128]), false);
        let gain = jpeg(4, 4, |_, _| image::Rgb([255, 255, 255]), true);
        let file = ultra_hdr(&primary, &gain, XMP);
        let (m, g) = find(&file).unwrap();
        assert_eq!((m.max, m.capacity_max, m.offset_sdr), ([2.0; 3], 2.0, [0.0; 3]));
        assert!(g.starts_with(&[0xFF, 0xD8]) && g.ends_with(&[0xFF, 0xD9]));
        // Without the MPF index: the container's length.
        let segs = segments(&file);
        let mpf = segs.iter().find(|s| s.0 == 0xE2).unwrap().1;
        let at = mpf.as_ptr() as usize - file.as_ptr() as usize - 4;
        let without = [&file[..at], &file[at + 4 + mpf.len()..]].concat();
        assert_eq!(find(&without).map(|(_, g)| g.len()), Some(g.len()));
        // Not a gain-map file.
        assert!(find(&primary).is_none());
    }

    #[test]
    fn sequences_and_elements_are_read() {
        let x = "<x:xmpmeta><rdf:Description hdrgm:Version=\"1.0\"><hdrgm:GainMapMax><rdf:Seq>\
                 <rdf:li>1</rdf:li><rdf:li>2</rdf:li><rdf:li>3</rdf:li></rdf:Seq></hdrgm:GainMapMax>\
                 <hdrgm:HDRCapacityMax>3</hdrgm:HDRCapacityMax></rdf:Description></x:xmpmeta>";
        let m = meta(x).unwrap();
        assert_eq!((m.max, m.capacity_max, m.gamma, m.offset_hdr), ([1.0, 2.0, 3.0], 3.0, [1.0; 3], [1.0 / 64.0; 3]));
        assert!(meta(&x.replace("hdrgm:Version=\"1.0\"", "hdrgm:Version=\"1.0\" hdrgm:BaseRenditionIsHDR=\"True\""))
            .is_none());
    }

    #[test]
    fn the_hdr_rendition_follows_the_headroom() {
        let meta = meta(XMP).unwrap();
        // A full gain on mid grey, none on the second pixel.
        let gain = Gain { meta, channels: 1, px: vec![255, 0] };
        let sdr = [188, 188, 188, 255, 188, 188, 188, 255];
        let lin = |h: u16| srgb_to_linear(f32_of(h));
        let grey = srgb_to_linear(188.0 / 255.0);
        // No headroom: the SDR picture, to the half's precision.
        let at1 = gain.hdr(&sdr, 1.0);
        assert!((f32_of(at1[0]) - 188.0 / 255.0).abs() < 1e-3 && f32_of(at1[3]) == 1.0);
        // Headroom 2 (log2 1, half of capacity 2): the boost is 2^(2 × 0.5).
        let at2 = gain.hdr(&sdr, 2.0);
        assert!((lin(at2[0]) / grey - 2.0).abs() < 0.01, "{}", lin(at2[0]) / grey);
        // Headroom 4 and past it: the full 2^2.
        for h in [4.0, 16.0] {
            assert!((lin(gain.hdr(&sdr, h)[0]) / grey - 4.0).abs() < 0.02);
        }
        assert!((f32_of(at2[4]) - 188.0 / 255.0).abs() < 1e-3);
    }

    #[test]
    fn halves_round_trip() {
        for v in [0.0, 1.0, -2.5, 0.5, 65504.0, 1e-5, 2.7, 1.0 / 255.0] {
            let back = f32_of(f16(v));
            assert!((back - v).abs() <= v.abs() * 1e-3 + 1e-7, "{v} {back}");
        }
        assert_eq!(f16(1e9), 0x7BFF);
    }
}
