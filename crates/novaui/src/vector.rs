//! Vector marks given as data: an SVG path outline (`M L H V C Q Z`, absolute and relative) in
//! a square `view` box, filled with the non-zero rule into an A8 coverage tile. The `Path`
//! element draws one in its `fg`, as an icon is drawn; apps and plugin fragments (dsl.rs
//! `parse_fragment`) use it for marks the built-in icon set does not have.

use ab_glyph_rasterizer::{point, Rasterizer};
use std::sync::Arc;

/// At most this many commands in one outline.
pub const MAX_CMDS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Cmd {
    Move(f32, f32),
    Line(f32, f32),
    Quad(f32, f32, f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Close,
}

/// A parsed outline: its commands in absolute view units, its box, and a digest that keys the
/// rasterized tiles (two equal outlines share them).
#[derive(Clone, Debug)]
pub struct Shape {
    cmds: Arc<[Cmd]>,
    pub view: f32,
    pub key: u64,
}

impl PartialEq for Shape {
    fn eq(&self, o: &Self) -> bool {
        self.key == o.key && self.view == o.view
    }
}

impl Shape {
    pub fn empty() -> Shape {
        Shape { cmds: Arc::from(Vec::new()), view: 24.0, key: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }
}

/// Parses an SVG path's `d` in a `view`-unit square. Coordinates must stay within
/// [-view, 2·view]; `S T A` are not supported.
pub fn parse(d: &str, view: f32) -> Result<Shape, String> {
    if !(view > 0.0 && view <= 4096.0) {
        return Err(format!("view must be in (0, 4096], got {view}"));
    }
    let b = d.as_bytes();
    let mut i = 0;
    let skip = |i: &mut usize| {
        while *i < b.len() && (b[*i].is_ascii_whitespace() || b[*i] == b',') {
            *i += 1;
        }
    };
    let number = |i: &mut usize| -> Option<f32> {
        skip(i);
        let s = *i;
        if *i < b.len() && (b[*i] == b'-' || b[*i] == b'+') {
            *i += 1;
        }
        let mut dot = false;
        let mut digits = 0;
        while *i < b.len() && (b[*i].is_ascii_digit() || (b[*i] == b'.' && !dot)) {
            dot |= b[*i] == b'.';
            digits += usize::from(b[*i].is_ascii_digit());
            *i += 1;
        }
        if digits > 0 && *i < b.len() && (b[*i] == b'e' || b[*i] == b'E') {
            let save = *i;
            *i += 1;
            if *i < b.len() && (b[*i] == b'-' || b[*i] == b'+') {
                *i += 1;
            }
            let e0 = *i;
            while *i < b.len() && b[*i].is_ascii_digit() {
                *i += 1;
            }
            if *i == e0 {
                *i = save;
            }
        }
        if digits == 0 {
            *i = s;
            return None;
        }
        d[s..*i].parse().ok()
    };
    let (lo, hi) = (-view, 2.0 * view);
    let mut out: Vec<Cmd> = Vec::new();
    let (mut cx, mut cy, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut cmd = 0u8;
    loop {
        skip(&mut i);
        if i >= b.len() {
            break;
        }
        if b[i].is_ascii_alphabetic() {
            cmd = b[i];
            i += 1;
        } else if cmd == 0 {
            return Err(format!("a path starts with a command, found '{}'", b[i] as char));
        }
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (cx, cy) } else { (0.0, 0.0) };
        let n = |i: &mut usize| number(i).ok_or_else(|| format!("'{}' needs numbers", cmd as char));
        let p = match cmd.to_ascii_uppercase() {
            b'M' => {
                let (x, y) = (n(&mut i)? + ox, n(&mut i)? + oy);
                (cx, cy, sx, sy) = (x, y, x, y);
                // Further pairs after M are lines.
                cmd = if rel { b'l' } else { b'L' };
                Cmd::Move(x, y)
            }
            b'L' => {
                (cx, cy) = (n(&mut i)? + ox, n(&mut i)? + oy);
                Cmd::Line(cx, cy)
            }
            b'H' => {
                cx = n(&mut i)? + ox;
                Cmd::Line(cx, cy)
            }
            b'V' => {
                cy = n(&mut i)? + if rel { cy } else { 0.0 };
                Cmd::Line(cx, cy)
            }
            b'Q' => {
                let (x1, y1) = (n(&mut i)? + ox, n(&mut i)? + oy);
                (cx, cy) = (n(&mut i)? + ox, n(&mut i)? + oy);
                Cmd::Quad(x1, y1, cx, cy)
            }
            b'C' => {
                let (x1, y1) = (n(&mut i)? + ox, n(&mut i)? + oy);
                let (x2, y2) = (n(&mut i)? + ox, n(&mut i)? + oy);
                (cx, cy) = (n(&mut i)? + ox, n(&mut i)? + oy);
                Cmd::Cubic(x1, y1, x2, y2, cx, cy)
            }
            b'Z' => {
                (cx, cy) = (sx, sy);
                cmd = 0;
                Cmd::Close
            }
            c => return Err(format!("path command '{}' is not supported (M L H V C Q Z)", c as char)),
        };
        let pts: &[f32] = match &p {
            Cmd::Move(x, y) | Cmd::Line(x, y) => &[*x, *y],
            Cmd::Quad(a, b2, c, d2) => &[*a, *b2, *c, *d2],
            Cmd::Cubic(a, b2, c, d2, e, f) => &[*a, *b2, *c, *d2, *e, *f],
            Cmd::Close => &[],
        };
        if pts.iter().any(|v| !v.is_finite() || *v < lo || *v > hi) {
            return Err(format!("path coordinates must stay within [{lo}, {hi}]"));
        }
        out.push(p);
        if out.len() > MAX_CMDS {
            return Err(format!("a path has at most {MAX_CMDS} commands"));
        }
    }
    // FNV-1a over the commands' bits and the box: the tiles' key.
    let mut h: u64 = 0xcbf29ce484222325;
    let mut feed = |v: u32| {
        for byte in v.to_le_bytes() {
            h = (h ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
    };
    feed(view.to_bits());
    for c in &out {
        match *c {
            Cmd::Move(x, y) => [1.0, x, y].iter().for_each(|v| feed(v.to_bits())),
            Cmd::Line(x, y) => [2.0, x, y].iter().for_each(|v| feed(v.to_bits())),
            Cmd::Quad(a, b2, c2, d2) => [3.0, a, b2, c2, d2].iter().for_each(|v| feed(v.to_bits())),
            Cmd::Cubic(a, b2, c2, d2, e, f) => [4.0, a, b2, c2, d2, e, f].iter().for_each(|v| feed(v.to_bits())),
            Cmd::Close => feed(5),
        }
    }
    Ok(Shape { cmds: Arc::from(out), view, key: h.max(1) })
}

/// A rasterized shape: `px`² A8 coverage and the box its ink covers (x0, y0, x1, y1) in pixels.
pub struct Tile {
    pub px: u32,
    pub a8: Vec<u8>,
    pub ink: [f32; 4],
}

/// Fills the shape (non-zero) into a `px`-square tile; open contours are closed.
pub fn rasterize(s: &Shape, px: u32) -> Tile {
    let px = px.clamp(1, 1024);
    let k = px as f32 / s.view;
    let mut r = Rasterizer::new(px as usize, px as usize);
    let (mut cx, mut cy, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut open = false;
    let p = |x: f32, y: f32| point(x * k, y * k);
    // A contour ends with a line back to its start, unless it is there already.
    let close = |r: &mut Rasterizer, (cx, cy): (f32, f32), (sx, sy): (f32, f32)| {
        if (cx, cy) != (sx, sy) {
            r.draw_line(p(cx, cy), p(sx, sy));
        }
    };
    for c in s.cmds.iter() {
        match *c {
            Cmd::Move(x, y) => {
                if open {
                    close(&mut r, (cx, cy), (sx, sy));
                }
                (cx, cy, sx, sy) = (x, y, x, y);
                open = true;
            }
            Cmd::Line(x, y) => {
                r.draw_line(p(cx, cy), p(x, y));
                (cx, cy) = (x, y);
            }
            Cmd::Quad(x1, y1, x, y) => {
                r.draw_quad(p(cx, cy), p(x1, y1), p(x, y));
                (cx, cy) = (x, y);
            }
            Cmd::Cubic(x1, y1, x2, y2, x, y) => {
                r.draw_cubic(p(cx, cy), p(x1, y1), p(x2, y2), p(x, y));
                (cx, cy) = (x, y);
            }
            Cmd::Close => {
                close(&mut r, (cx, cy), (sx, sy));
                (cx, cy) = (sx, sy);
                open = false;
            }
        }
    }
    if open {
        close(&mut r, (cx, cy), (sx, sy));
    }
    let mut a8 = vec![0u8; (px * px) as usize];
    r.for_each_pixel_2d(|x, y, a| a8[(y * px + x) as usize] = (a.clamp(0.0, 1.0) * 255.0).round() as u8);
    let (mut x0, mut y0, mut x1, mut y1) = (px, px, 0, 0);
    for y in 0..px {
        for x in 0..px {
            if a8[(y * px + x) as usize] > 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    let ink = if x1 > x0 { [x0 as f32, y0 as f32, x1 as f32, y1 as f32] } else { [0.0; 4] };
    Tile { px, a8, ink }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cov(t: &Tile, x: u32, y: u32) -> u8 {
        t.a8[(y * t.px + x) as usize]
    }

    #[test]
    fn a_square_fills_its_box_and_nothing_else() {
        let s = parse("M4 4H20V20H4Z", 24.0).unwrap();
        let t = rasterize(&s, 24);
        assert_eq!(cov(&t, 12, 12), 255);
        assert_eq!(cov(&t, 2, 2), 0);
        assert_eq!(t.ink, [4.0, 4.0, 20.0, 20.0]);
        // Doubling the pixels doubles the ink box.
        assert_eq!(rasterize(&s, 48).ink, [8.0, 8.0, 40.0, 40.0]);
    }

    #[test]
    fn relative_commands_match_absolute_ones() {
        let a = parse("M4 4 L20 4 L20 20 L4 20 Z", 24.0).unwrap();
        let r = parse("m4 4 h16 v16 h-16 z", 24.0).unwrap();
        assert_eq!(rasterize(&a, 24).a8, rasterize(&r, 24).a8);
        // Implicit lines after a move.
        let m = parse("M4 4 20 4 20 20 4 20z", 24.0).unwrap();
        assert_eq!(rasterize(&a, 24).a8, rasterize(&m, 24).a8);
    }

    #[test]
    fn a_hole_wound_the_other_way_stays_empty() {
        let s = parse("M2 2H22V22H2Z M8 8V16H16V8Z", 24.0).unwrap();
        let t = rasterize(&s, 24);
        assert_eq!(cov(&t, 12, 12), 0, "the hole");
        assert_eq!(cov(&t, 4, 12), 255, "the ring");
    }

    #[test]
    fn curves_fill_inside_their_hull() {
        let s = parse("M2 12 C2 4 22 4 22 12 C22 20 2 20 2 12Z", 24.0).unwrap();
        let t = rasterize(&s, 24);
        assert_eq!(cov(&t, 12, 12), 255);
        assert_eq!(cov(&t, 1, 1), 0);
        let q = parse("M2 20 Q12 0 22 20Z", 24.0).unwrap();
        assert_eq!(cov(&rasterize(&q, 24), 12, 16), 255);
    }

    #[test]
    fn bad_outlines_are_refused() {
        assert!(parse("L4 4", 24.0).is_ok(), "a line from the origin is still an outline");
        assert!(parse("4 4", 24.0).is_err());
        assert!(parse("M0 0 A5 5 0 0 1 10 10", 24.0).is_err(), "arcs unsupported");
        assert!(parse("M0 0 L100 0", 24.0).is_err(), "out of the box's reach");
        assert!(parse("M0 0 L", 24.0).is_err(), "missing numbers");
        assert!(parse("M0 0", 0.0).is_err());
        let long = "M0 0".to_string() + &" L1 1".repeat(MAX_CMDS + 1);
        assert!(parse(&long, 24.0).is_err());
    }

    #[test]
    fn equal_outlines_share_a_key() {
        let a = parse("M4 4H20V20H4Z", 24.0).unwrap();
        let b = parse("M 4,4 H 20 V 20 H 4 Z", 24.0).unwrap();
        let c = parse("M4 4H20V21H4Z", 24.0).unwrap();
        assert_eq!(a.key, b.key);
        assert_ne!(a.key, c.key);
    }
}
