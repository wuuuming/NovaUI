use super::*;
use crate::{dsl, render::software::Software, Ui};

#[test]
fn wrapped_text_is_invariant_under_subpixel_translation() {
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let mut ui = Ui::new(1842, 997, scale);
        let mut doc = dsl::Doc::new(
            dsl::parse(
                r#"
            Box #parent { left: 10.3; top: 20.4; width: 900; height: 180;
                Text #line { width: 843.03314; font-size: 41.024834; line: 53.332283; weight: 700;
                    lines: 3; text: "その笑顔 超魔法級につき イタズラしちゃう！"; }
            }
        "#,
            )
            .unwrap(),
        );
        let root = ui.tree.root;
        doc.build(&mut ui, root).unwrap();
        let line = doc.id("line");
        // The same physical wrapping threshold at each DPI.
        ui.tree.set_layout(line, |l| l.width = Len::Px(843.03314 / scale));
        if let Content::Text(mut text) = ui.tree.el(line).content.clone() {
            text.size /= scale;
            text.line /= scale;
            ui.tree.set_content(line, Content::Text(text));
        }
        let mut backend = Software::new(1842, 997);
        let mut expected = None;
        let mut widths = std::collections::HashSet::new();
        for step in 0..120 {
            // The text itself moves: a moved parent is an offset group, moved in whole pixels.
            ui.tree.set_translate(line, step as f32 * 0.01 / scale, step as f32 * 0.013 / scale);
            // Force recording as a real fade/hover does, rather than simply translating a cache.
            ui.tree.set_visual(line, |v| v.fg[3] = 0.5 + step as f32 * 0.003);
            ui.tree.frame(&mut backend, step as f64 / 60.0);
            let e = ui.tree.el(line);
            widths.insert(e.trect.w);
            let glyphs: Vec<_> = e
                .prims
                .iter()
                .filter_map(|p| match p {
                    Prim::Glyph { x, y, .. } => Some((*x, *y)),
                    _ => None,
                })
                .collect();
            assert!(!glyphs.is_empty());
            if let Some(expected) = &expected {
                assert_eq!(&glyphs, expected, "scale {scale}, translation step {step}");
            } else {
                expected = Some(glyphs);
            }
        }
        // (Inside a layer the content is pixel-anchored and widths no longer alternate.)
        assert!(widths.len() > 1, "fixture must exercise rounded-width alternation at DPI {scale}: {widths:?}");
    }
}
