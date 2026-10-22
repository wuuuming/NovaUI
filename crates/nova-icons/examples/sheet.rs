//! Writes a contact sheet of every icon as a grayscale PNG, for looking at the set by eye.
//!
//!   cargo run -p nova-icons --example sheet -- <out.png> [size] [pixels_per_unit] [columns]
//!
//! Each cell is the icon's em box (outlined faintly) with its tile filled black on white. No image
//! crate: the PNG is written with stored (uncompressed) deflate blocks.

use nova_icons::{rasterize, Icon};

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().expect("usage: sheet <out.png> [size] [pixels_per_unit] [columns]");
    let size: f32 = args.next().map_or(32.0, |value| value.parse().expect("size"));
    let ppu: f32 = args.next().map_or(1.0, |value| value.parse().expect("pixels per unit"));
    let columns: usize = args.next().map_or(12, |value| value.parse().expect("columns"));
    let em = (size * ppu).ceil() as usize;
    let cell = em + 12;
    let rows = Icon::ALL.len().div_ceil(columns);
    let (width, height) = (columns * cell, rows * cell);
    let mut pixels = vec![255_u8; width * height];
    for (index, icon) in Icon::ALL.iter().enumerate() {
        let (cx, cy) = ((index % columns) * cell + 6, (index / columns) * cell + 6);
        for i in 0..em {
            for (x, y) in [(cx + i, cy), (cx + i, cy + em - 1), (cx, cy + i), (cx + em - 1, cy + i)] {
                pixels[y * width + x] = 225;
            }
        }
        let tile = rasterize(*icon, size, ppu);
        for row in 0..tile.h as usize {
            for column in 0..tile.w as usize {
                let x = cx as i32 + tile.origin[0] + column as i32;
                let y = cy as i32 + tile.origin[1] + row as i32;
                if x >= 0 && y >= 0 && (x as usize) < width && (y as usize) < height {
                    let pixel = &mut pixels[y as usize * width + x as usize];
                    *pixel = pixel.saturating_sub(tile.a8[row * tile.w as usize + column]);
                }
            }
        }
    }
    std::fs::write(&out, png(width, height, &pixels)).expect("write png");
    for (index, icon) in Icon::ALL.iter().enumerate() {
        println!("{index:3} r{} c{} {:?}", index / columns, index % columns, icon);
    }
}

fn png(width: usize, height: usize, gray: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((width + 1) * height);
    for row in gray.chunks_exact(width) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut zlib = vec![0x78, 0x01];
    for (index, block) in raw.chunks(65_535).enumerate() {
        let last = (index + 1) * 65_535 >= raw.len();
        zlib.push(u8::from(last));
        zlib.extend_from_slice(&(block.len() as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut a, mut b) = (1_u32, 0_u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&(width as u32).to_be_bytes());
    header.extend_from_slice(&(height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 0, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in kind.iter().chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    out.extend_from_slice(&(!crc).to_be_bytes());
}
