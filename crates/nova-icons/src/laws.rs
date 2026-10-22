//! Laws of the rasterizer and of the embedded data.

use crate::{
    generated, master_points, master_verbs, metrics,
    raster::{self, Transform, CLOSE, CUBIC, LINE, MOVE, QUAD},
    rasterize, CoverageTile, Icon, MasterEntry, Rasterizer, BLOB,
};

const SIZES: [f32; 9] = [8.0, 9.0, 10.0, 12.0, 14.0, 16.0, 20.0, 24.0, 36.0];
const DENSITIES: [f32; 4] = [1.0, 1.25, 1.5, 2.0];

fn verbs(master: &MasterEntry) -> &'static [u8] {
    master_verbs(master, BLOB)
}

fn every_master() -> impl Iterator<Item = (Icon, &'static MasterEntry)> {
    Icon::ALL.into_iter().flat_map(|icon| icon.glyph().masters().iter().map(move |master| (icon, master)))
}

/// A synthetic outline's coverage in a `w × h` tile.
fn fill(verbs: &[u8], points: &[[f32; 2]], w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    raster::fill(
        &mut Default::default(),
        verbs,
        &mut points.iter().copied(),
        Transform { scale: 1.0, offset: [0.0, 0.0] },
        w,
        h,
        &mut out,
    );
    out
}

fn coverage_sum(a8: &[u8]) -> f32 {
    a8.iter().map(|value| f32::from(*value) / 255.0).sum()
}

/// The flattened contours of the master an icon fills at a size, in tile pixels.
fn polygon(icon: Icon, size: f32, ppu: f32) -> (CoverageTile, Vec<Vec<[f32; 2]>>) {
    let tile = rasterize(icon, size, ppu);
    let plan = crate::plan(icon.glyph(), size, ppu);
    let mut contours: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut last: Option<[f32; 2]> = None;
    raster::flatten(verbs(plan.master), &mut master_points(plan.master, BLOB), plan.transform, |from, to| {
        if last != Some(from) {
            contours.push(vec![from]);
        }
        contours.last_mut().expect("a contour").push(to);
        last = Some(to);
    });
    (tile, contours)
}

fn winding(contours: &[Vec<[f32; 2]>], x: f32, y: f32) -> i32 {
    let mut winding = 0;
    for contour in contours {
        for pair in contour.windows(2) {
            let ([x0, y0], [x1, y1]) = (pair[0], pair[1]);
            if (y0 <= y) != (y1 <= y) {
                let cross_x = x0 + (y - y0) / (y1 - y0) * (x1 - x0);
                if cross_x > x {
                    winding += if y1 > y0 { 1 } else { -1 };
                }
            }
        }
    }
    winding
}

#[test]
fn every_icon_has_path_data_and_a_round_tripping_key() {
    assert_eq!(generated::ICONS.len(), Icon::ALL.len());
    for (index, icon) in Icon::ALL.into_iter().enumerate() {
        assert_eq!(icon as usize, index);
        assert!(!icon.glyph().masters().is_empty(), "{icon:?} has no master");
        assert_eq!(Icon::from_key(icon.key()), Some(icon), "{icon:?} key does not round-trip");
        assert_eq!(icon.key_str(), icon.key().to_string());
        assert!(icon.master_sizes().all(|size| [16, 20, 24].contains(&size)));
    }
    assert!(generated::KEYS.windows(2).all(|pair| pair[0].0 < pair[1].0), "keys are unique and sorted");
    assert_eq!(Icon::from_key('A'), None);
    assert_eq!(Icon::from_key('\u{E001}'), None);
    for (icon, master) in every_master() {
        let verbs = verbs(master);
        assert_eq!(verbs.first(), Some(&MOVE), "{icon:?} {} does not start with a move", master.design);
        let operands: usize = verbs
            .iter()
            .map(|verb| match *verb {
                MOVE | LINE => 1,
                QUAD => 2,
                CUBIC => 3,
                CLOSE => 0,
                other => panic!("{icon:?} holds verb {other}"),
            })
            .sum();
        assert_eq!(operands, master.point_len as usize, "{icon:?} {} verb/point count", master.design);
        let [x0, y0, x1, y1] = master.ink;
        let design = f32::from(master.design);
        assert!(
            0.0 <= x0 && x0 < x1 && x1 <= design && 0.0 <= y0 && y0 < y1 && y1 <= design,
            "{icon:?} {} ink {:?}",
            master.design,
            master.ink
        );
    }
}

#[test]
fn stored_ink_bounds_are_the_exact_extent_of_the_stored_curves() {
    // Flattened 64 pixels to the unit, every chord point is on the curve and the chords come
    // within 0.02/64 of a unit of it: their extent is the curve's, from inside, to that tolerance.
    const SCALE: f32 = 64.0;
    for (icon, master) in every_master() {
        let mut extent = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
        raster::flatten(
            verbs(master),
            &mut master_points(master, BLOB),
            Transform { scale: SCALE, offset: [0.0, 0.0] },
            |from, to| {
                for [x, y] in [from, to] {
                    extent = [
                        extent[0].min(x / SCALE),
                        extent[1].min(y / SCALE),
                        extent[2].max(x / SCALE),
                        extent[3].max(y / SCALE),
                    ];
                }
            },
        );
        for (side, (got, stored)) in extent.iter().zip(&master.ink).enumerate() {
            assert!(
                (got - stored).abs() < 2e-3,
                "{icon:?} {} side {side}: stored {stored} flattened {got}",
                master.design,
            );
        }
        assert!(
            extent[0] >= master.ink[0] - 1e-4
                && extent[1] >= master.ink[1] - 1e-4
                && extent[2] <= master.ink[2] + 1e-4
                && extent[3] <= master.ink[3] + 1e-4
        );
    }
}

#[test]
fn coverage_of_a_polygon_sums_to_its_area() {
    // An axis-aligned rectangle at fractional corners and a right triangle.
    let rectangle =
        fill(&[MOVE, LINE, LINE, LINE, CLOSE], &[[1.25, 1.25], [7.75, 1.25], [7.75, 5.5], [1.25, 5.5]], 9, 7);
    assert!((coverage_sum(&rectangle) - 6.5 * 4.25).abs() <= 63.0 * 0.5 / 255.0);
    assert_eq!(rectangle[3 * 9 + 4], 255, "an interior pixel is fully covered");
    assert_eq!(rectangle[0], 0, "an exterior pixel is empty");
    let triangle = fill(&[MOVE, LINE, LINE, CLOSE], &[[0.5, 0.5], [9.5, 0.5], [0.5, 6.5]], 10, 7);
    assert!((coverage_sum(&triangle) - 0.5 * 9.0 * 6.0).abs() <= 70.0 * 0.5 / 255.0);
    // Winding either way fills the same.
    let reversed = fill(&[MOVE, LINE, LINE, CLOSE], &[[0.5, 0.5], [0.5, 6.5], [9.5, 0.5]], 10, 7);
    assert_eq!(triangle, reversed);
    // A hole wound the other way is empty; the same square wound the same way twice is filled once.
    let outer = [[1.0, 1.0], [9.0, 1.0], [9.0, 9.0], [1.0, 9.0]];
    let inner = [[3.0, 3.0], [3.0, 7.0], [7.0, 7.0], [7.0, 3.0]];
    let ring = fill(&[MOVE, LINE, LINE, LINE, CLOSE, MOVE, LINE, LINE, LINE, CLOSE], &[outer, inner].concat(), 10, 10);
    assert!((coverage_sum(&ring) - (64.0 - 16.0)).abs() < 0.01);
    assert_eq!(ring[5 * 10 + 5], 0);
    let doubled =
        fill(&[MOVE, LINE, LINE, LINE, CLOSE, MOVE, LINE, LINE, LINE, CLOSE], &[outer, outer].concat(), 10, 10);
    assert_eq!(doubled, fill(&[MOVE, LINE, LINE, LINE, CLOSE], &outer, 10, 10), "nonzero: overlap fills once");
}

#[test]
fn coverage_is_mirror_symmetric_for_a_symmetric_outline() {
    // A diamond and a circle of four cubics, both centred on the tile.
    let diamond = fill(&[MOVE, LINE, LINE, LINE, CLOSE], &[[6.0, 0.7], [11.3, 6.0], [6.0, 11.3], [0.7, 6.0]], 12, 12);
    let (c, r) = (6.0_f32, 4.6_f32);
    let k = 0.552_284_8 * r;
    let circle_points = [
        [c + r, c],
        [c + r, c + k],
        [c + k, c + r],
        [c, c + r],
        [c - k, c + r],
        [c - r, c + k],
        [c - r, c],
        [c - r, c - k],
        [c - k, c - r],
        [c, c - r],
        [c + k, c - r],
        [c + r, c - k],
        [c + r, c],
    ];
    let circle = fill(&[MOVE, CUBIC, CUBIC, CUBIC, CUBIC, CLOSE], &circle_points, 12, 12);
    for tile in [&diamond, &circle] {
        for y in 0..12 {
            for x in 0..12 {
                let value = i32::from(tile[y * 12 + x]);
                for mirrored in [tile[y * 12 + (11 - x)], tile[(11 - y) * 12 + x], tile[x * 12 + y]] {
                    assert!((value - i32::from(mirrored)).abs() <= 1, "({x}, {y}): {value} vs {mirrored}");
                }
            }
        }
    }
    // Chords cut inside the curve by at most the flattening tolerance (0.02 px) along the whole
    // perimeter; each of the 144 pixels rounds to the nearest 1/255.
    let area = coverage_sum(&circle);
    let exact = std::f32::consts::PI * r * r;
    assert!(
        (area - exact).abs() < 2.0 * std::f32::consts::PI * r * 0.02 + 144.0 * 0.5 / 255.0,
        "circle area {area} vs {exact}"
    );
}

#[test]
fn every_icon_matches_a_point_sampled_nonzero_fill() {
    // 8×8 samples a pixel: an edge through a pixel is off by at most about an eighth, and a pixel
    // the accumulation got wrong because windings of opposite sign met there would be off by up to
    // a whole pixel.
    for icon in Icon::ALL {
        for (size, ppu) in [(16.0, 1.0), (24.0, 1.5), (36.0, 2.0)] {
            let (tile, contours) = polygon(icon, size, ppu);
            let mut total_error = 0.0_f32;
            for y in 0..tile.h {
                for x in 0..tile.w {
                    let mut inside = 0;
                    for sy in 0..8 {
                        for sx in 0..8 {
                            let (px, py) = (x as f32 + (sx as f32 + 0.5) / 8.0, y as f32 + (sy as f32 + 0.5) / 8.0);
                            inside += i32::from(winding(&contours, px, py) != 0);
                        }
                    }
                    let reference = inside as f32 / 64.0;
                    let coverage = f32::from(tile.a8[(y * tile.w + x) as usize]) / 255.0;
                    let error = (reference - coverage).abs();
                    assert!(
                        error <= 0.3,
                        "{icon:?} at {size}×{ppu}: pixel ({x}, {y}) coverage {coverage} vs sampled {reference}"
                    );
                    total_error += error;
                }
            }
            let mean = total_error / (tile.w * tile.h) as f32;
            assert!(mean < 0.03, "{icon:?} at {size}×{ppu}: mean error {mean}");
        }
    }
}

#[test]
fn tiles_are_the_outward_rounding_of_exact_ink_bounds() {
    for icon in Icon::ALL {
        for size in SIZES {
            for ppu in DENSITIES {
                let tile = rasterize(icon, size, ppu);
                assert_eq!(metrics(icon, size, ppu).ink_bounds, tile.ink_bounds);
                let [x0, y0, x1, y1] = tile.ink_bounds;
                assert_eq!(tile.origin, [x0.floor() as i32, y0.floor() as i32], "{icon:?} {size}×{ppu}");
                assert_eq!(
                    (tile.w, tile.h),
                    ((x1.ceil() - x0.floor()) as u32, (y1.ceil() - y0.floor()) as u32),
                    "{icon:?} {size}×{ppu}"
                );
                assert_eq!(tile.a8.len(), (tile.w * tile.h) as usize);
                assert_eq!(tile.advance, size);
                // Ink reaches every edge of the tile, to within a pixel: the tile is not padded.
                let (mut ix0, mut iy0, mut ix1, mut iy1) = (u32::MAX, u32::MAX, 0, 0);
                for y in 0..tile.h {
                    for x in 0..tile.w {
                        if tile.a8[(y * tile.w + x) as usize] != 0 {
                            (ix0, iy0, ix1, iy1) = (ix0.min(x), iy0.min(y), ix1.max(x + 1), iy1.max(y + 1));
                        }
                    }
                }
                assert!(
                    ix0 <= 1 && iy0 <= 1 && ix1 + 1 >= tile.w && iy1 + 1 >= tile.h,
                    "{icon:?} {size}×{ppu}: ink {ix0},{iy0}..{ix1},{iy1} in {}×{}",
                    tile.w,
                    tile.h
                );
            }
        }
    }
}

#[test]
fn icons_land_centred_in_their_placement_box() {
    for icon in Icon::ALL {
        let place = icon.glyph().entry().place;
        for (size, ppu) in [(16.0, 1.0), (12.0, 1.5), (36.0, 2.0)] {
            let em = size * ppu;
            let [x0, y0, x1, y1] = metrics(icon, size, ppu).ink_bounds;
            let (box_w, box_h) = ((place[2] - place[0]) * em, (place[3] - place[1]) * em);
            assert!(((x0 + x1) * 0.5 - (place[0] + place[2]) * 0.5 * em).abs() < 1e-3, "{icon:?} centre x");
            assert!(((y0 + y1) * 0.5 - (place[1] + place[3]) * 0.5 * em).abs() < 1e-3, "{icon:?} centre y");
            assert!(x1 - x0 <= box_w + 1e-3 && y1 - y0 <= box_h + 1e-3, "{icon:?} overflows its box");
            assert!(
                (x1 - x0 - box_w).abs() < 1e-3 || (y1 - y0 - box_h).abs() < 1e-3,
                "{icon:?} fits its box on one axis"
            );
        }
    }
}

#[test]
fn rasterization_is_deterministic_and_buffer_reuse_changes_nothing() {
    let mut rasterizer = Rasterizer::new();
    let mut reused = CoverageTile::default();
    for icon in Icon::ALL {
        for size in SIZES {
            for ppu in DENSITIES {
                let first = rasterize(icon, size, ppu);
                assert_eq!(first, rasterize(icon, size, ppu));
                rasterizer.rasterize_into(icon, size, ppu, &mut reused);
                assert_eq!(first, reused);
            }
        }
    }
}

#[test]
fn degenerate_requests_are_empty() {
    for (size, ppu) in [(0.0, 1.0), (16.0, 0.0), (-4.0, 1.0), (f32::NAN, 1.0), (16.0, f32::INFINITY)] {
        let tile = rasterize(Icon::Dismiss, size, ppu);
        assert_eq!((tile.w, tile.h, tile.a8.len()), (0, 0, 0));
    }
}
