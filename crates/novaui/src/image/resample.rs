//! Row-streamed separable Triangle resizing for tightly packed 8-bit channels.
//! Coordinate conventions, normalized weights and final nearest rounding match image 0.25.10
//! (image-rs/image, imageops/sample.rs). No full source_width × target_height float image.
//! Samples are filtered in their supplied domain, with no implicit gamma or alpha conversion.

fn weights(input: usize, output: usize, at: usize, taps: &mut Vec<f32>) -> Result<usize, &'static str> {
    let ratio = input as f32 / output as f32;
    let scale = ratio.max(1.0);
    let center = (at as f32 + 0.5) * ratio;
    let first = ((center - scale).floor() as i64).clamp(0, input as i64 - 1) as usize;
    let end = ((center + scale).ceil() as i64).clamp(first as i64 + 1, input as i64) as usize;
    let center = center - 0.5;
    taps.clear();
    taps.try_reserve(end - first).map_err(|_| "weight allocation failed")?;
    let mut sum = 0.0;
    for source in first..end {
        let distance = ((source as f32 - center) / scale).abs();
        let value = if distance < 1.0 { 1.0 - distance } else { 0.0 };
        taps.push(value);
        sum += value;
    }
    for value in taps.iter_mut() {
        *value /= sum;
    }
    Ok(first)
}

#[derive(Clone, Copy, Debug)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Resize packed L/LA/RGB/RGBA bytes without changing channel semantics. Extra trailing source
/// bytes are ignored; row padding is not supported. Scratch is linear in widths, not image area.
/// (The reference the cropping resample is checked against.)
#[cfg(test)]
pub(crate) fn triangle_u8(
    source: &[u8],
    width: u32,
    height: u32,
    channels: u8,
    new_width: u32,
    new_height: u32,
) -> Result<Vec<u8>, &'static str> {
    triangle_u8_region(
        source,
        width,
        height,
        channels,
        new_width,
        new_height,
        Region { x: 0, y: 0, width: new_width, height: new_height },
    )
}

/// Only materialize a region of the resized image, with the same sampling coordinates as a
/// full resize followed by a crop. In particular, fill-mode panoramas need no oversized output.
pub fn triangle_u8_region(
    source: &[u8],
    width: u32,
    height: u32,
    channels: u8,
    new_width: u32,
    new_height: u32,
    region: Region,
) -> Result<Vec<u8>, &'static str> {
    if width == 0 || height == 0 || new_width == 0 || new_height == 0 {
        return Err("zero dimension");
    }
    if !(1..=4).contains(&channels) {
        return Err("unsupported channel count");
    }
    let size = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(channels as usize))
        .ok_or("source size overflow")?;
    if source.len() < size {
        return Err("short source");
    }
    if region.width == 0
        || region.height == 0
        || region.x.checked_add(region.width).is_none_or(|r| r > new_width)
        || region.y.checked_add(region.height).is_none_or(|b| b > new_height)
    {
        return Err("invalid output region");
    }
    if (width, height) == (new_width, new_height) {
        let row_bytes = region.width as usize * channels as usize;
        let mut out = Vec::new();
        out.try_reserve_exact(row_bytes.checked_mul(region.height as usize).ok_or("output size overflow")?)
            .map_err(|_| "output allocation failed")?;
        for y in region.y..region.y + region.height {
            let start = (y as usize * width as usize + region.x as usize) * channels as usize;
            out.extend_from_slice(&source[start..start + row_bytes]);
        }
        return Ok(out);
    }
    match channels {
        1 => resize::<1>(source, width as usize, height as usize, new_width as usize, new_height as usize, region),
        2 => resize::<2>(source, width as usize, height as usize, new_width as usize, new_height as usize, region),
        3 => resize::<3>(source, width as usize, height as usize, new_width as usize, new_height as usize, region),
        4 => resize::<4>(source, width as usize, height as usize, new_width as usize, new_height as usize, region),
        _ => unreachable!(),
    }
}

fn resize<const C: usize>(
    src: &[u8],
    w: usize,
    h: usize,
    full_w: usize,
    full_h: usize,
    region: Region,
) -> Result<Vec<u8>, &'static str> {
    let (nw, nh) = (region.width as usize, region.height as usize);
    let len = nw.checked_mul(nh).and_then(|n| n.checked_mul(C)).ok_or("output size overflow")?;
    let mut out = Vec::new();
    out.try_reserve_exact(len).map_err(|_| "output allocation failed")?;
    out.resize(len, 0);
    let mut row = Vec::new();
    row.try_reserve_exact(w).map_err(|_| "row allocation failed")?;
    row.resize(w, [0.0f32; C]);
    // Reuse horizontal weights across rows. Each descriptor is (source start, weight start, count).
    let mut horizontal = Vec::new();
    horizontal.try_reserve_exact(nw).map_err(|_| "descriptor allocation failed")?;
    let mut all_weights = Vec::new();
    let mut taps = Vec::new();
    for x in 0..nw {
        let first = weights(w, full_w, region.x as usize + x, &mut taps)?;
        horizontal.push((first, all_weights.len(), taps.len()));
        all_weights.try_reserve(taps.len()).map_err(|_| "weight allocation failed")?;
        all_weights.extend_from_slice(&taps);
    }
    for y in 0..nh {
        let first = weights(h, full_h, region.y as usize + y, &mut taps)?;
        // This is one row of the original vertical pass; preserve accumulation order exactly.
        for (x, pixel) in row.iter_mut().enumerate() {
            *pixel = [0.0; C];
            for (i, &weight) in taps.iter().enumerate() {
                let from = ((first + i) * w + x) * C;
                for c in 0..C {
                    pixel[c] += src[from + c] as f32 * weight;
                }
            }
        }
        // Consume that row immediately instead of retaining every vertically filtered row.
        for (x, &(first, start, count)) in horizontal.iter().enumerate() {
            let mut pixel = [0.0f32; C];
            for (i, &weight) in all_weights[start..start + count].iter().enumerate() {
                for c in 0..C {
                    pixel[c] += row[first + i][c] * weight;
                }
            }
            let to = (y * nw + x) * C;
            for c in 0..C {
                out[to + c] = pixel[c].clamp(0.0, 255.0).round() as u8;
            }
        }
    }
    Ok(out)
}
