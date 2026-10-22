//! Live `.nui` reload, in debug builds only (`Doc::watch`): the sheet's file is watched, and a
//! change is applied to the elements in place. Every element the doc built keeps its id, the
//! app's handles and models stay as they are, and what the app changed on an element at run
//! time (a text it set, an opacity) stays unless the file changed that same thing.
//!
//! How: the doc remembers which build made each element (`Rec`: `build`, or one `instantiate`).
//! On a change it builds each of them twice aside (hidden, under the root), from the old sheet
//! and from the new one, with the bindings as they are now, and merges three ways: where the
//! old and the new copy differ the live element takes the new one's value, elsewhere it keeps
//! its own. Children are matched by kind and name, in order: a new one is moved in from the new
//! copy, a removed one is removed. A parse or build error keeps the old tree and shows the error
//! (`dev::Dev::banner`). Fragments mounted from outside (`mount`, `instantiate_scoped`) are not
//! the file's: they stay as they are.

use super::*;
use crate::dev::Dev;
use crate::element::Listen;
use crate::style::{Content, Layout, Visual};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// One build of the doc's: what it built, and how to build it again.
#[derive(Clone)]
pub(super) enum RecKind {
    /// `Doc::build`: the sheet's roots.
    Build,
    /// `Doc::instantiate`: a component, its arguments, its ids' prefix.
    Instance { name: String, args: Vec<Val>, prefix: String },
}

#[derive(Clone)]
pub(super) struct Rec {
    kind: RecKind,
    /// Where its elements stand: their parent.
    parent: Id,
}

/// The doc's development records.
#[derive(Default)]
pub(crate) struct DocDev {
    recs: HashMap<u32, Rec>,
    next: u32,
    /// The build running now (`inst_at` notes its elements under it).
    pub(super) cur: Option<u32>,
    /// A reload builds its copies: nothing is recorded.
    aside: bool,
    /// Which build made each element.
    origin: HashMap<Id, u32>,
    watch: Option<Watch>,
}

/// A watched sheet file: a thread looks at its modification time and wakes the app
/// (`SheetChanged`) when it moved.
struct Watch {
    path: PathBuf,
    changed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl DocDev {
    /// A build starts; returns the one it interrupts (`end`'s argument).
    pub(super) fn begin(&mut self, kind: RecKind, parent: Id) -> Option<u32> {
        if self.aside {
            return self.cur;
        }
        let r = self.next;
        self.next += 1;
        self.recs.insert(r, Rec { kind, parent });
        self.cur.replace(r)
    }

    pub(super) fn end(&mut self, prev: Option<u32>) {
        if !self.aside {
            self.cur = prev;
        }
    }

    pub(super) fn made(&mut self, id: Id) {
        if let Some(r) = self.cur {
            self.origin.insert(id, r);
        }
    }

    /// Elements gone: their records go, and builds with nothing left.
    pub(super) fn prune(&mut self, ui: &Ui) {
        self.origin.retain(|id, _| ui.tree.alive(*id));
        let live: HashSet<u32> = self.origin.values().copied().collect();
        self.recs.retain(|r, _| live.contains(r));
    }
}

/// An element's label (`inst_at`): its kind, name and classes, then its line.
pub(super) fn label(n: &Node) -> String {
    format!("{} :{}", label_key(n), n.line)
}

/// A label without its line.
pub(super) fn label_key(n: &Node) -> String {
    let mut s = n.kind.clone();
    if let Some(i) = &n.id {
        s.push('#');
        s.push_str(i);
    }
    for c in &n.classes {
        s.push_str(" .");
        s.push_str(c);
    }
    s
}

/// What children are matched by: the label without its line (lines move with every edit).
fn key(ui: &Ui, id: Id) -> &str {
    let l = ui.tree.dev_label(id).unwrap_or("");
    l.rsplit_once(" :").map_or(l, |(k, _)| k)
}

impl Doc {
    /// Debug builds: watches `path`, the file this doc's sheet was read from, and applies its
    /// changes in place (the module's rules) at the first `update` after the app hears
    /// `SheetChanged`. A release build does nothing here.
    pub fn watch(&mut self, ui: &mut Ui, path: impl AsRef<Path>) {
        let path = path.as_ref().to_path_buf();
        let changed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let waker = ui.waker(SheetChanged);
        let (c, s, p) = (changed.clone(), stop.clone(), path.clone());
        let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let spawned = std::thread::Builder::new().name("novaui-nui-watch".into()).spawn(move || {
            let mut last = mtime(&p);
            while !s.load(Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(250));
                let now = mtime(&p);
                if now.is_some() && now != last {
                    last = now;
                    c.store(true, Ordering::Release);
                    waker.wake();
                }
            }
        });
        match spawned {
            Ok(_) => self.dev.watch = Some(Watch { path, changed, stop }),
            Err(e) => eprintln!("novaui: cannot watch {}: {e}", path.display()),
        }
    }

    /// `update`'s first step: a watched file that changed is read and applied.
    pub(super) fn reload_if_changed(&mut self, ui: &mut Ui) {
        let Some(w) = &self.dev.watch else { return };
        if !w.changed.swap(false, Ordering::AcqRel) {
            return;
        }
        let path = w.path.clone();
        let res = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|src| parse(&src).map_err(|e| e.to_string()))
            .and_then(|sheet| self.reload(ui, sheet).map_err(|e| e.to_string()));
        match res {
            Ok(()) => {
                eprintln!("novaui: {} reloaded", path.display());
                Dev::banner(ui, None);
            }
            Err(e) => {
                let msg = format!("{}: {e}", path.display());
                eprintln!("novaui: {msg} (the last good sheet stays)");
                Dev::banner(ui, Some(&msg));
            }
        }
    }

    /// What a reload moves between the live tree and the copies: the records the copies made.
    fn take_records(&mut self) -> Records {
        let mut r = std::mem::take(&mut self.rec);
        r.bind_index = None;
        r
    }

    fn put_records(&mut self, mut r: Records) {
        r.bind_index = None;
        self.rec = r;
    }

    /// Builds every recorded build again under `aside` (one hidden box each, in `recs`' order),
    /// unrecorded. Returns the boxes.
    fn build_aside(&mut self, ui: &mut Ui, aside: Id, recs: &[(u32, Rec)]) -> Result<Vec<Id>, Error> {
        let cur = self.dev.cur.take();
        self.dev.aside = true;
        let mut boxes = Vec::new();
        let res = (|| {
            for (_, rec) in recs {
                let b = ui.tree.add(aside, Layout::default(), Visual::default(), Content::None);
                boxes.push(b);
                match &rec.kind {
                    RecKind::Build => {
                        self.eval_decls(None);
                        for n in self.sheet.roots.clone() {
                            self.inst(ui, b, &n, &HashMap::default(), None)?;
                        }
                    }
                    RecKind::Instance { name, args, prefix } => {
                        self.instantiate(ui, b, None, name, args, prefix)?;
                    }
                }
            }
            Ok(())
        })();
        self.dev.aside = false;
        self.dev.cur = cur;
        res.map(|_| boxes)
    }

    /// Applies `new` to the elements in place (the module's rules); on an error nothing changed.
    pub fn reload(&mut self, ui: &mut Ui, mut new: Sheet) -> Result<(), Error> {
        self.prune(ui);
        // What was taken in from outside (`adopt`) is not the file's: it stays.
        for (k, v) in &self.sheet.styles {
            if super::adopted_name(k) {
                new.styles.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        for (k, v) in &self.sheet.components {
            if super::adopted_name(k) {
                new.components.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        let mut recs: Vec<(u32, Rec)> =
            self.dev.recs.iter().filter(|(_, r)| ui.tree.alive(r.parent)).map(|(k, r)| (*k, r.clone())).collect();
        recs.sort_by_key(|(k, _)| *k);
        let root = ui.tree.root();
        let hidden = Layout { display: crate::style::Display::None, ..Layout::default() };
        let aside = ui.tree.add(root, hidden, Visual::default(), Content::None);
        let live = self.take_records();
        // The old sheet's copies, then the new sheet's: the old copies' records are dropped,
        // the new ones' kept for the elements that end up live.
        let old_boxes = self.build_aside(ui, aside, &recs);
        let _ = self.take_records();
        let old_sheet = std::mem::replace(&mut self.sheet, new);
        let new_boxes = old_boxes.and_then(|o| self.build_aside(ui, aside, &recs).map(|n| (o, n)));
        let (old_boxes, new_boxes) = match new_boxes {
            Ok(b) => b,
            Err(e) => {
                let _ = self.take_records();
                self.sheet = old_sheet;
                self.eval_decls(None);
                self.put_records(live);
                ui.tree.remove(aside);
                self.prune(ui);
                return Err(e);
            }
        };
        let fresh = self.take_records();
        let mut m = Merge {
            to_live: HashMap::default(),
            moved: Vec::new(),
            merged: HashSet::default(),
            origin: self.dev.origin.clone(),
            rec: 0,
        };
        for (k, (r, rec)) in recs.iter().enumerate() {
            m.rec = *r;
            let lc = made_by(ui, &self.dev.origin, rec.parent, *r);
            let oc = ui.tree.children(old_boxes[k]);
            let nc = ui.tree.children(new_boxes[k]);
            m.children(ui, rec.parent, &lc, &oc, &nc);
        }
        // Records: the live ones of elements outside the merge stay; the new copy's take the
        // place of the rest, on the live ids.
        let Records { ids, binds, dyn_props, keyed, unit_progs, .. } = live;
        let gone = |id: &Id| m.merged.contains(id);
        let to = |id: Id| m.to_live.get(&id).copied().unwrap_or(id);
        let mut out = Records {
            ids: ids.into_iter().filter(|(_, id)| !gone(id)).collect(),
            binds: binds
                .into_iter()
                .map(|(k, l)| (k, l.into_iter().filter(|(id, _)| !gone(id)).collect::<Vec<_>>()))
                .collect(),
            dyn_props: dyn_props.into_iter().filter(|p| !gone(&p.id)).collect(),
            keyed: keyed.into_iter().filter(|(id, _, _)| !gone(id)).collect(),
            unit_progs: unit_progs.into_iter().filter(|p| !gone(&p.id)).collect(),
            bind_index: None,
        };
        for (k, id) in fresh.ids {
            out.ids.insert(k, to(id));
        }
        for (k, l) in fresh.binds {
            out.binds.entry(k).or_default().extend(l.into_iter().map(|(id, p)| (to(id), p)));
        }
        for mut p in fresh.dyn_props {
            p.move_to(to(p.id));
            out.dyn_props.push(p);
        }
        out.keyed.extend(fresh.keyed.into_iter().map(|(id, p, k)| (to(id), p, k)));
        for mut p in fresh.unit_progs {
            p.id = to(p.id);
            out.unit_progs.push(p);
        }
        self.put_records(out);
        // The elements moved in are the builds' now.
        for (id, r) in m.moved {
            for e in subtree(ui, id) {
                self.dev.origin.insert(e, r);
            }
        }
        ui.tree.remove(aside);
        self.prune(ui);
        self.evaluate_all_again();
        Ok(())
    }
}

/// `id` and every element under it.
fn subtree(ui: &Ui, id: Id) -> Vec<Id> {
    let (mut out, mut stack) = (Vec::new(), vec![id]);
    while let Some(e) = stack.pop() {
        out.push(e);
        stack.extend(ui.tree.children(e));
    }
    out
}

/// The children of `parent` that build `rec` made (`origin`).
fn made_by(ui: &Ui, origin: &HashMap<Id, u32>, parent: Id, rec: u32) -> Vec<Id> {
    ui.tree.children(parent).into_iter().filter(|c| origin.get(c) == Some(&rec)).collect()
}

/// A reload's merge: new-copy ids taken over by live ones, subtrees moved in, live elements the
/// merge decided (their old records go).
struct Merge {
    to_live: HashMap<Id, Id>,
    moved: Vec<(Id, u32)>,
    merged: HashSet<Id>,
    /// Which build made each live element, and the build being merged.
    origin: HashMap<Id, u32>,
    rec: u32,
}

impl Merge {
    /// The children of live `parent` that one build made (`lc`), their old copies (`oc`, the
    /// same in number and order unless the app moved them) and their new ones (`nc`).
    fn children(&mut self, ui: &mut Ui, parent: Id, lc: &[Id], oc: &[Id], nc: &[Id]) {
        let n = lc.len().min(oc.len());
        let mut j = 0;
        let mut last_live: Option<Id> = None;
        for &nw in nc {
            let k = (j..n).find(|&k| key(ui, oc[k]) == key(ui, nw));
            match k {
                Some(k) => {
                    for &gone in &lc[j..k] {
                        self.remove(ui, gone);
                    }
                    self.element(ui, lc[k], oc[k], nw);
                    last_live = Some(lc[k]);
                    j = k + 1;
                }
                None => {
                    let kids = ui.tree.children(parent);
                    let at = match last_live {
                        Some(l) => kids.iter().position(|&c| c == l).map_or(kids.len(), |p| p + 1),
                        None => lc.first().and_then(|f| kids.iter().position(|c| c == f)).unwrap_or(kids.len()),
                    };
                    ui.tree.dev_move(nw, parent, at);
                    self.moved.push((nw, self.rec));
                    last_live = Some(nw);
                }
            }
        }
        for &gone in &lc[j.min(lc.len())..] {
            self.remove(ui, gone);
        }
    }

    fn remove(&mut self, ui: &mut Ui, id: Id) {
        self.merged.extend(subtree(ui, id));
        ui.tree.remove(id);
    }

    /// One live element `l`, its old copy `o`, its new copy `nw`: three ways, then their children.
    fn element(&mut self, ui: &mut Ui, l: Id, o: Id, nw: Id) {
        self.merged.insert(l);
        self.to_live.insert(nw, l);
        let (oe, ne) = (ui.tree.el(o), ui.tree.el(nw));
        let mut layout = *ui.tree.el(l).layout();
        merge_layout(&mut layout, oe.layout(), ne.layout());
        let mut visual = ui.tree.el(l).visual().clone();
        merge_visual(&mut visual, oe.visual(), ne.visual());
        let content = merge_content(ui.tree.el(l).content(), oe.content(), ne.content());
        let action = (oe.action() != ne.action()).then(|| ne.action().map(|a| a.to_string()));
        let listen = [
            (Listen::Hover, oe.hover_events(), ne.hover_events()),
            (Listen::Press, oe.press_events(), ne.press_events()),
            (Listen::Wheel, oe.wheel_events(), ne.wheel_events()),
            (Listen::Drag, oe.drag_events(), ne.drag_events()),
        ];
        let promoted = (oe.promoted() != ne.promoted()).then(|| ne.promoted());
        let units = (oe.unit_src() != ne.unit_src()).then(|| ne.unit_src().cloned().unwrap_or_default());
        ui.tree.set_layout(l, |x| *x = layout);
        ui.tree.set_visual(l, |x| *x = visual);
        if let Some(c) = content {
            ui.tree.set_content(l, c);
        }
        if let Some(a) = action {
            ui.tree.set_action(l, a.as_deref());
        }
        for (which, was, now) in listen {
            if was != now {
                ui.tree.set_listen(l, which, now);
            }
        }
        if let Some(p) = promoted {
            ui.tree.set_promoted(l, p);
        }
        if let Some(u) = units {
            ui.tree.set_unit_src(l, |x| *x = u);
        }
        // Their children: the live element's that the doc made (an app's rows stay as they are).
        let lc = made_by(ui, &self.origin, l, self.rec);
        // In the copies, the doc's elements are the labelled ones: the parts a widget made for
        // itself (a field's hint and caret) are the widget's, in the live element already.
        let made = |id: Id| -> Vec<Id> {
            ui.tree.children(id).into_iter().filter(|c| ui.tree.dev_label(*c).is_some()).collect()
        };
        let (oc, nc) = (made(o), made(nw));
        self.children(ui, l, &lc, &oc, &nc);
    }
}

/// Field by field: where the old and the new copy differ, the live value becomes the new one.
macro_rules! three_way {
    ($live:expr, $old:expr, $new:expr; $($f:ident),* $(,)?) => {
        $( if $old.$f != $new.$f { $live.$f = $new.$f.clone(); } )*
    };
}

fn merge_layout(live: &mut Layout, old: &Layout, new: &Layout) {
    // Every field is named: a new one fails to compile here until it is merged too.
    let Layout {
        display: _,
        direction: _,
        position: _,
        width: _,
        height: _,
        min_width: _,
        min_height: _,
        max_width: _,
        max_height: _,
        grow: _,
        shrink: _,
        basis: _,
        gap: _,
        padding: _,
        margin: _,
        inset: _,
        align_items: _,
        align_self: _,
        justify: _,
        aspect: _,
        wrap: _,
    } = new;
    three_way!(live, old, new; display, direction, position, width, height, min_width, min_height,
        max_width, max_height, grow, shrink, basis, gap, padding, margin, inset, align_items,
        align_self, justify, aspect, wrap);
}

fn merge_visual(live: &mut Visual, old: &Visual, new: &Visual) {
    let Visual {
        bg: _,
        bg_hover: _,
        bg_press: _,
        bg_lit: _,
        bg_lit_press: _,
        bg_focus: _,
        border_focus: _,
        underline: _,
        underline_focus: _,
        border: _,
        border_color: _,
        radius: _,
        square: _,
        plate: _,
        shadow: _,
        material: _,
        fg: _,
        fg_hover: _,
        fg_disabled: _,
        fg_off: _,
        opacity: _,
        clip: _,
        cursor: _,
        visible: _,
        hit: _,
        scale: _,
        scale_x: _,
        underline_hover: _,
        sweep: _,
        sweep_fg: _,
        units: _,
        text_rotate: _,
        text_origin: _,
    } = new;
    three_way!(live, old, new; bg, bg_hover, bg_press, bg_lit, bg_lit_press, bg_focus,
        border_focus, underline, underline_focus, border, border_color, radius, square, plate,
        shadow, material, fg, fg_hover, fg_disabled, fg_off, opacity, clip, cursor, visible, hit,
        scale, scale_x, underline_hover, sweep, sweep_fg, units, text_rotate, text_origin);
}

/// The live element's content after the merge (`None`: unchanged). A text's string and its
/// style merge apart: a text the app set stays when only the file's size changed.
fn merge_content(live: &Content, old: &Content, new: &Content) -> Option<Content> {
    if old == new {
        return None;
    }
    match (live, old, new) {
        (Content::Text(l), Content::Text(o), Content::Text(n)) => {
            let mut t = l.clone();
            three_way!(t, o, n; text, size, line, weight, align, lines, wrap);
            (t != *l).then_some(Content::Text(t))
        }
        _ => Some(new.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = r#"
Column#app {
  Text#title { text: "Hello"; font-size: 14; }
  Box#panel { bg: #202020; padding: 4; }
  Text#tail { text: $tail; }
}
"#;

    fn ui() -> Ui {
        Ui::new(400, 300, 1.0)
    }

    #[test]
    fn a_reload_keeps_ids_and_run_time_changes_and_applies_the_files() {
        let mut ui = ui();
        let mut doc = Doc::new(parse(A).unwrap());
        let root = ui.tree.root();
        doc.build(&mut ui, root).unwrap();
        doc.set(&mut ui, "tail", "bound");
        let (title, panel, tail) = (doc.id("title"), doc.id("panel"), doc.id("tail"));
        // The app's own changes at run time.
        ui.tree.set_text(title, "Set by the app");
        ui.tree.set_opacity(panel, 0.5);
        let edited = A
            .replace("font-size: 14;", "font-size: 20;")
            .replace("bg: #202020;", "bg: #ff0000;")
            .replace("Text#tail", "Text#added { text: \"new\"; }\n  Text#tail");
        doc.reload(&mut ui, parse(&edited).unwrap()).unwrap();
        doc.update(&mut ui);
        assert_eq!((doc.id("title"), doc.id("panel"), doc.id("tail")), (title, panel, tail));
        let Content::Text(t) = ui.tree.el(title).content() else { panic!() };
        assert_eq!((t.text.as_str(), t.size), ("Set by the app", 20.0));
        assert_eq!(ui.tree.el(panel).visual().opacity, 0.5);
        assert_eq!(ui.tree.el(panel).visual().bg, Some(crate::geom::rgb(0xff, 0, 0)));
        assert_eq!(ui.tree.text_of(tail), Some("bound"));
        let added = doc.id("added");
        let app = doc.id("app");
        assert_eq!(ui.tree.children(app), vec![title, panel, added, tail]);
        // The binding still reaches the element it was read by.
        doc.set(&mut ui, "tail", "again");
        assert_eq!(ui.tree.text_of(tail), Some("again"));
        // A removed element goes.
        doc.reload(&mut ui, parse(&edited.replace("Box#panel { bg: #ff0000; padding: 4; }", "")).unwrap()).unwrap();
        assert!(!ui.tree.alive(panel));
        assert_eq!(ui.tree.children(app), vec![title, added, tail]);
    }

    #[test]
    fn a_fields_own_parts_are_not_copied_in() {
        let src = "Column#app { Edit#field { width: 200; height: 32; hint: \"Search\"; } }";
        let mut ui = ui();
        let mut doc = Doc::new(parse(src).unwrap());
        let root = ui.tree.root();
        doc.build(&mut ui, root).unwrap();
        let field = doc.id("field");
        let parts = ui.tree.children(field);
        doc.reload(&mut ui, parse(&src.replace("height: 32;", "height: 40;")).unwrap()).unwrap();
        assert_eq!(ui.tree.children(field), parts);
        assert_eq!(ui.tree.el(field).layout().height, crate::style::Len::Px(40.0));
    }

    #[test]
    fn a_sheet_that_does_not_build_keeps_the_tree() {
        let mut ui = ui();
        let mut doc = Doc::new(parse(A).unwrap());
        let root = ui.tree.root();
        doc.build(&mut ui, root).unwrap();
        let title = doc.id("title");
        let before = ui.tree.children(root).len();
        assert!(doc.reload(&mut ui, parse(&A.replace("Box#panel", "Nothing#panel")).unwrap()).is_err());
        assert_eq!(doc.id("title"), title);
        assert_eq!(ui.tree.children(root).len(), before);
        assert_eq!(ui.tree.text_of(title), Some("Hello"));
    }
}
