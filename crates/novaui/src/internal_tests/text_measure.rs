//! Intrinsic measurement shares line breaking with painting but never touches the glyph atlas.
use novaui::text::{Align, Text, TextStyle};

#[test]
fn measurement_matches_full_layout_without_rasterizing() {
    let mut measured = Text::new();
    let mut painted = Text::new();
    for s in [
        "",
        "AVATAR office",
        "Line one\nLine two",
        "中文标点，换行测试。",
        "Latin 与中文 mixed: a longer wrapping sentence",
    ] {
        for size in [12.0, 24.0, 47.5] {
            for weight in [400, 700] {
                let style = TextStyle { size, line: size * 1.4, weight };
                for width in [0, 1, 32, 120, 600] {
                    for max_lines in [0, 1, 2] {
                        let m = measured.measure(s, &style, width, max_lines);
                        for align in [Align::Start, Align::Center, Align::End] {
                            let p = painted.layout(s, &style, width, max_lines, align);
                            assert_eq!(
                                (m.ink_w, m.h, m.lines),
                                (p.ink_w, p.h, p.lines),
                                "{s:?}, size={size}, width={width}, max_lines={max_lines}"
                            );
                        }
                        let misses = measured.counts().measured;
                        assert_eq!(measured.measure(s, &style, width, max_lines), m);
                        assert_eq!(measured.counts().measured, misses, "repeated measurement should hit cache");
                    }
                }
            }
        }
    }
    assert_eq!(measured.counts().rasterized, 0);
    assert_eq!(measured.counts().laid_out, 0);
    assert!(!measured.has_uploads(), "measurement uploaded atlas pixels");
}

/// A font's measures scale with its size; the capitals stand between the lowercase and the
/// ascent, and their middle is below a line box's when the descent is shorter than the space
/// above the capitals (as in every font a UI sets).
#[test]
fn font_metrics_scale_and_place_the_capitals() {
    let text = Text::new();
    let (a, b) = (text.font_metrics(14.0, 400), text.font_metrics(28.0, 400));
    for (x, y) in [(a.ascent, b.ascent), (a.descent, b.descent), (a.cap_height, b.cap_height), (a.x_height, b.x_height)]
    {
        assert!((2.0 * x - y).abs() < 0.01, "{a:?} {b:?}");
    }
    assert!(0.0 < b.x_height && b.x_height < b.cap_height && b.cap_height < b.ascent && b.descent < 0.0, "{b:?}");
    let middle = (b.ascent + b.descent) / 2.0 - b.cap_height / 2.0;
    assert_eq!(b.cap_middle(), middle);
}
