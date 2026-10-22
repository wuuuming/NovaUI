//! Physical-pixel geometry, colours and damage lists.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Rect { x, y, w, h }
    }
    pub const EMPTY: Rect = Rect::new(0, 0, 0, 0);
    /// Effectively unbounded (used as "no clip").
    pub const HUGE: Rect = Rect::new(-(1 << 28), -(1 << 28), 1 << 29, 1 << 29);
    pub fn r(&self) -> i32 {
        self.x + self.w
    }
    pub fn b(&self) -> i32 {
        self.y + self.h
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.r().min(o.r());
        let b = self.b().min(o.b());
        if r <= x || b <= y {
            Rect::EMPTY
        } else {
            Rect::new(x, y, r - x, b - y)
        }
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        !self.intersect(o).is_empty()
    }
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect::new(x, y, self.r().max(o.r()) - x, self.b().max(o.b()) - y)
    }
    pub fn area(&self) -> u64 {
        if self.is_empty() {
            0
        } else {
            self.w as u64 * self.h as u64
        }
    }
    pub fn translate(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }
    pub fn inflate(&self, d: i32) -> Rect {
        Rect::new(self.x - d, self.y - d, self.w + 2 * d, self.h + 2 * d)
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.r() && y < self.b()
    }
    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
    pub fn to_f(&self) -> [f32; 4] {
        [self.x as f32, self.y as f32, self.w as f32, self.h as f32]
    }
}

/// Straight (non-premultiplied) RGBA in 0..1, sRGB-encoded.
pub type Color = [f32; 4];

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}
pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a]
}
/// `0xAARRGGBB`.
pub const fn argb(v: u32) -> Color {
    [
        ((v >> 16) & 255) as f32 / 255.0,
        ((v >> 8) & 255) as f32 / 255.0,
        (v & 255) as f32 / 255.0,
        (v >> 24) as f32 / 255.0,
    ]
}
pub const TRANSPARENT: Color = [0.0; 4];

pub fn with_alpha(c: Color, k: f32) -> Color {
    [c[0], c[1], c[2], c[3] * k]
}

/// Damage list: rectangles merged while the union costs at most 1.3× the parts, at most 8 kept.
#[derive(Default, Clone, Debug)]
pub struct Damage {
    pub rects: Vec<Rect>,
}

impl Damage {
    pub fn add(&mut self, r: Rect) {
        if r.is_empty() {
            return;
        }
        if self.rects.iter().any(|q| q.intersect(&r) == r) {
            return;
        }
        self.rects.retain(|q| r.intersect(q) != *q);
        self.rects.push(r);
        loop {
            let mut best: Option<(usize, usize, i64)> = None;
            for i in 0..self.rects.len() {
                for j in i + 1..self.rects.len() {
                    let (a, b) = (self.rects[i], self.rects[j]);
                    let u = a.union(&b).area() as i64;
                    let cost = u - a.area() as i64 - b.area() as i64;
                    let ok = (u as f64) <= 1.3 * (a.area() + b.area()) as f64 || self.rects.len() > 8;
                    if ok && best.is_none_or(|(_, _, c)| cost < c) {
                        best = Some((i, j, cost));
                    }
                }
            }
            match best {
                Some((i, j, _)) => {
                    let u = self.rects[i].union(&self.rects[j]);
                    self.rects.swap_remove(j);
                    self.rects[i] = u;
                }
                None => break,
            }
        }
    }
    pub fn take(&mut self) -> Vec<Rect> {
        std::mem::take(&mut self.rects)
    }
    /// Takes the rects into `out` (cleared first), keeping `out`'s storage for the next ones.
    pub fn take_into(&mut self, out: &mut Vec<Rect>) {
        out.clear();
        std::mem::swap(&mut self.rects, out);
    }
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }
}

/// erf approximation shared by the software and GPU paths (Winitzki, a = 0.147).
pub fn erf(x: f32) -> f32 {
    let a = 0.147f32;
    let x2 = x * x;
    let t = (-x2 * (4.0 / std::f32::consts::PI + a * x2) / (1.0 + a * x2)).exp();
    x.signum() * (1.0 - t).max(0.0).sqrt()
}
