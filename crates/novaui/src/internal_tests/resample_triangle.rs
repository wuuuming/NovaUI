//! Pixel-exact compatibility, not a looser image-quality threshold.
use image::{
    imageops::{resize, FilterType},
    ImageBuffer, Pixel,
};
use novaui::image::resample::triangle_u8;

fn compare<P: Pixel<Subpixel = u8> + 'static>(w: u32, h: u32, nw: u32, nh: u32) {
    let c = P::CHANNEL_COUNT;
    let data: Vec<u8> = (0..w as usize * h as usize * c as usize)
        .map(|i| ((i.wrapping_mul(73) ^ (i / 7).wrapping_mul(191) ^ (i / 37)) & 255) as u8)
        .collect();
    let src: ImageBuffer<P, Vec<u8>> = ImageBuffer::from_raw(w, h, data).unwrap();
    let actual = triangle_u8(src.as_raw(), w, h, c, nw, nh).unwrap();
    let expected = resize(&src, nw, nh, FilterType::Triangle);
    let mismatches = actual.iter().zip(expected.as_raw()).filter(|(a, b)| a != b).count();
    assert_eq!(mismatches, 0, "{c} channels: {w}x{h} -> {nw}x{nh}");
    assert_eq!(actual.len(), expected.as_raw().len());
    let region = novaui::image::resample::Region {
        x: nw / 3,
        y: nh / 3,
        width: (nw - nw / 3).min(19),
        height: (nh - nh / 3).min(17),
    };
    let cropped = novaui::image::resample::triangle_u8_region(src.as_raw(), w, h, c, nw, nh, region).unwrap();
    let reference_crop =
        image::imageops::crop_imm(&expected, region.x, region.y, region.width, region.height).to_image().into_raw();
    assert_eq!(
        cropped.iter().zip(&reference_crop).filter(|(a, b)| a != b).count(),
        0,
        "cropped {c}-channel resize differs"
    );
    assert_eq!(cropped.len(), reference_crop.len());
}

#[test]
fn row_stream_is_byte_identical_to_image_triangle() {
    for (w, h) in [(1, 1), (1, 37), (53, 1), (19, 31), (300, 400), (3000, 4), (4, 3000)] {
        for (nw, nh) in [(1, 1), (2, 3), (16, 16), (37, 21), (96, 96), (513, 2), (w, h)] {
            compare::<image::Luma<u8>>(w, h, nw, nh);
            compare::<image::LumaA<u8>>(w, h, nw, nh);
            compare::<image::Rgb<u8>>(w, h, nw, nh);
            compare::<image::Rgba<u8>>(w, h, nw, nh);
        }
    }
    compare::<image::Rgb<u8>>(3000, 3000, 1040, 1040);
    assert!(triangle_u8(&[], 1, 1, 4, 1, 1).is_err());
    assert!(triangle_u8(&[0], 1, 1, 0, 1, 1).is_err());
    assert!(triangle_u8(&[0], 1, 1, 1, 0, 1).is_err());
}
