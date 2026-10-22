//! QR codes as images: any product that shows a code to scan (a sign-in, a pairing link) needs
//! the same few lines, so they live here and not in each app.

use qrcodegen::{QrCode, QrCodeEcc};

/// The QR code of `text` as a square image (a 4-module quiet zone, 4 px per module). Text too
/// long for a code gives a 1 px white image.
pub fn image(text: &str) -> crate::image::ImageHandle {
    let (side, px) = rgba(text);
    crate::image::ImageHandle::rgba8(side, side, px)
}

fn rgba(text: &str) -> (i32, Vec<u8>) {
    let Ok(q) = QrCode::encode_text(text, QrCodeEcc::Medium) else { return (1, vec![255; 4]) };
    let (n, border, px) = (q.size(), 4, 4);
    let side = (n + 2 * border) * px;
    let mut out = vec![255u8; (side * side * 4) as usize];
    for y in 0..side {
        for x in 0..side {
            if q.get_module(x / px - border, y / px - border) {
                let o = ((y * side + x) * 4) as usize;
                out[o..o + 3].fill(0);
            }
        }
    }
    (side, out)
}
