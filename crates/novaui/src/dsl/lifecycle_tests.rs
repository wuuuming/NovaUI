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
