use super::*;

#[test]
fn virtualized_components_release_expressions_without_a_layout_update() {
    let mut ui = Ui::new(800, 600, 1.0);
    let mut doc = Doc::new(
        parse(
            r#"
        component Row() { Box { width: window.w / 2; height: 40; opacity: tween($on ? 1 : 0, $on, 300ms); } }
    "#,
        )
        .unwrap(),
    );
    doc.set(&mut ui, "on", false);
    let parent = ui.tree.root();
    doc.build(&mut ui, parent).unwrap();
    let kept = doc.instantiate(&mut ui, parent, None, "Row", &[], "kept").unwrap();
    let count = doc.rec.dyn_props.len();
    let tweens = doc.tweens.len();
    for i in 0..4000 {
        let row = doc.instantiate(&mut ui, parent, None, "Row", &[], &format!("row{i}")).unwrap();
        ui.tree.remove(row);
        doc.prune(&ui);
        assert_eq!(doc.rec.dyn_props.len(), count);
        assert_eq!(doc.tweens.len(), tweens);
    }
    ui.now = 1.0;
    doc.set(&mut ui, "on", true);
    doc.update(&mut ui);
    ui.now = 1.15;
    doc.update(&mut ui);
    assert!((ui.tree.prop(kept, crate::Prop::Opacity) - 0.5).abs() < 0.01);
}

/// A tween flipped back while it runs turns around where it stands and takes only the time it
/// already went to get back; flipped back and forth faster than its duration it keeps up
/// (each flip restarted the whole duration from rest, falling ever further behind).
#[test]
fn tween_flipped_back_retraces_from_where_it_stands() {
    let mut ui = Ui::new(800, 600, 1.0);
    let mut doc =
        Doc::new(parse("Box #b { width: 10; height: 10; opacity: tween($on ? 1 : 0, $on, 300ms); }").unwrap());
    doc.set(&mut ui, "on", false);
    let parent = ui.tree.root();
    doc.build(&mut ui, parent).unwrap();
    let b = doc.id("b");
    let op = |ui: &Ui| ui.tree.prop(b, crate::Prop::Opacity);
    let at = |ui: &mut Ui, doc: &mut Doc, t: f64| {
        ui.now = t;
        doc.update(ui);
    };
    doc.set(&mut ui, "on", true);
    at(&mut ui, &mut doc, 1.0);
    at(&mut ui, &mut doc, 1.1);
    let there = op(&ui);
    assert!(there > 0.1 && there < 0.5, "a third of the way: {there}");
    // Back: no jump, and the way it came takes what it took (0.1 s), not 0.3 s.
    doc.set(&mut ui, "on", false);
    at(&mut ui, &mut doc, 1.1);
    assert!((op(&ui) - there).abs() < 0.01, "turns where it stands: {} vs {there}", op(&ui));
    at(&mut ui, &mut doc, 1.15);
    let mid = op(&ui);
    assert!(mid < there && mid > 0.0, "on its way back: {mid}");
    at(&mut ui, &mut doc, 1.2);
    assert!(op(&ui).abs() < 0.001, "back by the time it went: {}", op(&ui));
    // Flipped every 50 ms it stays near the middle of its swing, never far behind the flips.
    let mut t = 2.0;
    for i in 0..20 {
        doc.set(&mut ui, "on", i % 2 == 0);
        at(&mut ui, &mut doc, t);
        t += 0.05;
        at(&mut ui, &mut doc, t);
    }
    assert!(op(&ui) < 0.3, "it follows the flips: {}", op(&ui));
}
