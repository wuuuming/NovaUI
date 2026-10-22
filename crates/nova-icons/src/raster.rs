//! Exact-area coverage rasterization of one outline into an A8 tile.
//!
//! The algorithm is signed-area accumulation (the one font-rs, stb_truetype 2 and ab_glyph use):
//! every line segment deposits, into each pixel it crosses, the exact signed area it sweeps to its
//! right inside that pixel; a running sum along each row then turns those deposits into the signed
//! coverage of every pixel. `min(|sum|, 1)` is the nonzero fill for outlines whose contours do not
//! cross each other with opposite winding — which is how Fluent outlines are drawn (holes run the
//! other way and never overlap their outer contour) and which `tests/laws.rs` checks against a
//! point-sampled nonzero reference for every icon.
//!
//! Curves are flattened on a Wang's-formula segment count, evaluated at uniform parameters: no
//! recursion and no data-dependent branching beyond the count, so the same inputs give the same
//! bytes on every platform (plain IEEE f32 — Rust never fuses multiply-adds implicitly).

/// The flattening tolerance, in pixels: how far a chord may stray from its curve.
const TOLERANCE_PX: f32 = 0.02;
/// Enough segments for any curve an icon master holds at any size a UI draws.
const MAX_SEGMENTS: u32 = 128;

/// Path verbs as stored in `data/paths.bin`.
pub(crate) const MOVE: u8 = 0;
pub(crate) const LINE: u8 = 1;
pub(crate) const QUAD: u8 = 2;
pub(crate) const CUBIC: u8 = 3;
pub(crate) const CLOSE: u8 = 4;

/// Master units to tile pixels: `p * scale + offset`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Transform {
    pub scale: f32,
    pub offset: [f32; 2],
}

impl Transform {
    fn apply(self, point: [f32; 2]) -> [f32; 2] {
        [point[0] * self.scale + self.offset[0], point[1] * self.scale + self.offset[1]]
    }
}

/// Reusable working memory: the accumulation buffer. Kept by `crate::Rasterizer` so rasterizing
/// again into a warm tile allocates nothing.
#[derive(Default)]
pub(crate) struct Scratch {
    acc: Vec<f32>,
}

/// Accumulates one tile. Rows are `stride = w + 2` wide: a segment's deposit may land one cell past
/// its last covered column, and it must not spill into the next row.
struct Accumulator<'a> {
    acc: &'a mut [f32],
    w: usize,
    h: usize,
    stride: usize,
}

impl Accumulator<'_> {
    fn line(&mut self, from: [f32; 2], to: [f32; 2]) {
        let clamp = |point: [f32; 2]| [point[0].clamp(0.0, self.w as f32), point[1].clamp(0.0, self.h as f32)];
        let (p0, p1) = (clamp(from), clamp(to));
        if p0[1] == p1[1] {
            return;
        }
        let (direction, p0, p1) = if p0[1] < p1[1] { (1.0_f32, p0, p1) } else { (-1.0_f32, p1, p0) };
        let dxdy = (p1[0] - p0[0]) / (p1[1] - p0[1]);
        let (x_low, x_high) = (p0[0].min(p1[0]), p0[0].max(p1[0]));
        let mut x = p0[0];
        let y_first = p0[1] as usize;
        let y_end = (p1[1].ceil() as usize).min(self.h);
        for y in y_first..y_end {
            let row = y * self.stride;
            let bottom = (y as f32 + 1.0).min(p1[1]);
            let dy = bottom - (y as f32).max(p0[1]);
            // Where the segment leaves this row, from its start rather than by stepping, and held
            // inside its own x range: a step that drifts past an integer (0.0 to -1e-8) would move
            // the whole row's deposit into the wrong column.
            let x_next = if bottom == p1[1] { p1[0] } else { (p0[0] + dxdy * (bottom - p0[1])).clamp(x_low, x_high) };
            let d = dy * direction;
            let (x0, x1) = if x < x_next { (x, x_next) } else { (x_next, x) };
            let x0_floor = x0.floor();
            let x0_index = x0_floor as usize;
            let x1_ceil = x1.ceil();
            let x1_index = x1_ceil as usize;
            if x1_index <= x0_index + 1 {
                // The segment stays inside one pixel column: the area right of its mean x.
                let mean = 0.5 * (x + x_next) - x0_floor;
                self.acc[row + x0_index] += d - d * mean;
                self.acc[row + x0_index + 1] += d * mean;
            } else {
                let inverse = (x1 - x0).recip();
                let x0_fraction = x0 - x0_floor;
                let first = 0.5 * inverse * (1.0 - x0_fraction) * (1.0 - x0_fraction);
                let x1_fraction = x1 - x1_ceil + 1.0;
                let last = 0.5 * inverse * x1_fraction * x1_fraction;
                self.acc[row + x0_index] += d * first;
                if x1_index == x0_index + 2 {
                    self.acc[row + x0_index + 1] += d * (1.0 - first - last);
                } else {
                    let second = inverse * (1.5 - x0_fraction);
                    self.acc[row + x0_index + 1] += d * (second - first);
                    for column in x0_index + 2..x1_index - 1 {
                        self.acc[row + column] += d * inverse;
                    }
                    let before_last = second + (x1_index - x0_index - 3) as f32 * inverse;
                    self.acc[row + x1_index - 1] += d * (1.0 - before_last - last);
                }
                self.acc[row + x1_index] += d * last;
            }
            x = x_next;
        }
    }
}

fn segments(control_deviation: f32, degree_factor: f32) -> u32 {
    let n = (degree_factor * control_deviation / TOLERANCE_PX).sqrt().ceil();
    if n.is_nan() || n < 1.0 {
        1
    } else {
        (n as u32).min(MAX_SEGMENTS)
    }
}

fn length(v: [f32; 2]) -> f32 {
    (v[0] * v[0] + v[1] * v[1]).sqrt()
}

/// The outline's verbs and points, already in tile pixels, handed to `each_line` as line segments.
/// Every contour is closed, whether or not it ends in an explicit close.
pub(crate) fn flatten(
    verbs: &[u8],
    points: &mut dyn Iterator<Item = [f32; 2]>,
    transform: Transform,
    mut each_line: impl FnMut([f32; 2], [f32; 2]),
) {
    let mut start = [0.0_f32; 2];
    let mut current = [0.0_f32; 2];
    let mut open = false;
    let mut next = || transform.apply(points.next().expect("path data holds a point for every verb operand"));
    for &verb in verbs {
        match verb {
            MOVE => {
                if open {
                    close(current, start, &mut each_line);
                }
                start = next();
                current = start;
                open = true;
            }
            LINE => {
                let to = next();
                each_line(current, to);
                current = to;
            }
            QUAD => {
                let (c, to) = (next(), next());
                let deviation = length([current[0] - 2.0 * c[0] + to[0], current[1] - 2.0 * c[1] + to[1]]);
                let from = current;
                chords(segments(deviation, 0.25), from, to, &mut each_line, |t, u| {
                    [
                        u * u * from[0] + 2.0 * u * t * c[0] + t * t * to[0],
                        u * u * from[1] + 2.0 * u * t * c[1] + t * t * to[1],
                    ]
                });
                current = to;
            }
            CUBIC => {
                let (c1, c2, to) = (next(), next(), next());
                let deviation = length([current[0] - 2.0 * c1[0] + c2[0], current[1] - 2.0 * c1[1] + c2[1]])
                    .max(length([c1[0] - 2.0 * c2[0] + to[0], c1[1] - 2.0 * c2[1] + to[1]]));
                let from = current;
                chords(segments(deviation, 0.75), from, to, &mut each_line, |t, u| {
                    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    [a * from[0] + b * c1[0] + c * c2[0] + d * to[0], a * from[1] + b * c1[1] + c * c2[1] + d * to[1]]
                });
                current = to;
            }
            CLOSE => {
                close(current, start, &mut each_line);
                current = start;
                open = false;
            }
            other => panic!("path data holds an unknown verb {other}"),
        }
    }
    if open {
        close(current, start, &mut each_line);
    }
}

/// A curve from `from` to `to` handed to `each_line` as `n` chords; `at(t, 1 - t)` is its point
/// at `t`, and the last chord ends exactly at `to`.
fn chords(
    n: u32,
    from: [f32; 2],
    to: [f32; 2],
    each_line: &mut impl FnMut([f32; 2], [f32; 2]),
    at: impl Fn(f32, f32) -> [f32; 2],
) {
    let mut previous = from;
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let point = if i == n { to } else { at(t, 1.0 - t) };
        each_line(previous, point);
        previous = point;
    }
}

/// A contour ending at `current` is closed back to its `start`, unless it is already there.
fn close(current: [f32; 2], start: [f32; 2], each_line: &mut impl FnMut([f32; 2], [f32; 2])) {
    if current != start {
        each_line(current, start);
    }
}

/// Fills `out` (`w * h` bytes, row-major) with the nonzero coverage of the outline under
/// `transform`, reusing `scratch`.
pub(crate) fn fill(
    scratch: &mut Scratch,
    verbs: &[u8],
    points: &mut dyn Iterator<Item = [f32; 2]>,
    transform: Transform,
    w: u32,
    h: u32,
    out: &mut Vec<u8>,
) {
    let (w, h) = (w as usize, h as usize);
    let stride = w + 2;
    scratch.acc.clear();
    scratch.acc.resize(stride * h, 0.0);
    let mut accumulator = Accumulator { acc: &mut scratch.acc, w, h, stride };
    flatten(verbs, points, transform, |from, to| accumulator.line(from, to));
    out.clear();
    out.reserve(w * h);
    for row in scratch.acc.chunks_exact(stride) {
        let mut sum = 0.0_f32;
        for &delta in &row[..w] {
            sum += delta;
            let coverage = sum.abs().min(1.0);
            out.push((coverage * 255.0 + 0.5) as u8);
        }
    }
}
