//! Development aids, in debug builds only (`cfg(any(debug_assertions, test))`; a release build
//! has none of this, not even its fields): what each element is (the label its builder gave it)
//! and why it was last damaged, the inspector that shows both (F12), and the banner that shows
//! a `.nui` reload's error (`Doc::watch`).

use crate::app::Ui;
use crate::element::Id;
use crate::geom::{rgb, Color};
use crate::style::Content;
use crate::style::{Layout, Len, Position, Visual};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::panic::Location;

/// What changed an element last: the kind of change and who asked (a setter's caller, through
/// `track_caller` up to the app's own call where every step forwards it).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Why {
    pub what: &'static str,
    pub at: &'static Location<'static>,
}

/// `Tree`'s development records, by element slot and generation (a slot's next element does
/// not read the last one's).
#[derive(Default)]
pub(crate) struct TreeDev {
    labels: HashMap<u32, (u32, String)>,
    why: HashMap<u32, (u32, Why)>,
}

/// A slot's record, if it is the element's now: kept at the element's generation.
fn of_gen<T>(records: &HashMap<u32, (u32, T)>, i: u32, gen: u32) -> Option<&T> {
    records.get(&i).filter(|(g, _)| *g == gen).map(|(_, r)| r)
}

impl TreeDev {
    pub fn label(&mut self, i: u32, gen: u32, label: String) {
        self.labels.insert(i, (gen, label));
    }
    pub fn label_of(&self, i: u32, gen: u32) -> Option<&str> {
        of_gen(&self.labels, i, gen).map(String::as_str)
    }
    pub fn why(&mut self, i: u32, gen: u32, what: &'static str, at: &'static Location<'static>) {
        self.why.insert(i, (gen, Why { what, at }));
    }
    pub fn why_of(&self, i: u32, gen: u32) -> Option<Why> {
        of_gen(&self.why, i, gen).copied()
    }
}

/// The key that opens and closes the inspector.
pub(crate) const INSPECT_KEY: u32 = crate::input::key::function(12);

/// Tags the inspector's own elements: it never inspects them.
const OWN: u64 = 0x6e6f_7661_6465_7600;

/// The inspector and the reload banner: elements of their own on top of the window.
#[derive(Default)]
pub(crate) struct Dev {
    on: bool,
    /// The element under the pointer.
    target: Option<Id>,
    /// What the panel shows now (written only when it changes).
    shown: Option<(Id, crate::geom::Rect, String)>,
    outline: Option<Id>,
    panel: Option<Id>,
    panel_text: Option<Id>,
    /// A reload's error, until the next reload succeeds.
    banner: Option<Id>,
    banner_text: Option<Id>,
}

const PANEL_BG: Color = [0.07, 0.08, 0.10, 0.94];
const OUTLINE: Color = rgb(0x4c, 0xc2, 0xff);
const BANNER_BG: Color = rgb(0x9b, 0x1c, 0x1c);
const MONO: f32 = 12.0;

impl Dev {
    /// F12: the inspector opens or closes.
    pub fn toggle(ui: &mut Ui) {
        ui.dev.on = !ui.dev.on;
        if !ui.dev.on {
            for id in [ui.dev.outline, ui.dev.panel].into_iter().flatten() {
                ui.tree.set_visible(id, false);
            }
            ui.dev.target = None;
            ui.dev.shown = None;
        } else if let Some((x, y)) = ui.pointer {
            Dev::pointed(ui, x, y);
        }
    }

    /// The pointer moved: the element under it is the one inspected.
    pub fn pointed(ui: &mut Ui, x: i32, y: i32) {
        if ui.dev.on {
            let root = ui.tree.root();
            ui.dev.target = Some(deepest(&ui.tree, root, x, y).unwrap_or(root));
        }
    }

    /// Each turn while the inspector is open: the outline around the element and the panel
    /// saying what it is (after the layout, so that its box is the one drawn).
    pub fn sync(ui: &mut Ui) {
        if !ui.dev.on {
            return;
        }
        let Some(t) = ui.dev.target.filter(|t| ui.tree.alive(*t)) else {
            if let Some(o) = ui.dev.outline {
                ui.tree.set_visible(o, false);
            }
            return;
        };
        let r = ui.tree.rect(t);
        let text = describe(ui, t);
        if ui.dev.shown.as_ref().is_some_and(|(id, rr, s)| *id == t && *rr == r && *s == text) {
            return;
        }
        Dev::ensure(ui);
        let s = ui.tree.scale().max(0.01);
        let (Some(outline), Some(panel), Some(pt)) = (ui.dev.outline, ui.dev.panel, ui.dev.panel_text) else {
            return;
        };
        let dip = |v: i32| v as f32 / s;
        ui.tree.set_layout(outline, |l| {
            l.inset = [Len::Px(dip(r.y)), Len::Auto, Len::Auto, Len::Px(dip(r.x))];
            l.width = Len::Px(dip(r.w));
            l.height = Len::Px(dip(r.h));
        });
        ui.tree.set_visible(outline, true);
        // The panel stands at the window's bottom left, or top left when the element is there.
        let (_, wh) = ui.tree.window_size();
        let low = r.y + r.h / 2 > wh / 2;
        ui.tree.set_layout(panel, |l| {
            l.inset = if low {
                [Len::Px(8.0), Len::Auto, Len::Auto, Len::Px(8.0)]
            } else {
                [Len::Auto, Len::Auto, Len::Px(8.0), Len::Px(8.0)]
            };
        });
        ui.tree.set_text(pt, &text);
        ui.tree.set_visible(panel, true);
        Dev::raise(ui);
        ui.dev.shown = Some((t, r, text));
    }

    /// A `.nui` reload failed (`Some`: the message), or succeeded (`None`).
    pub fn banner(ui: &mut Ui, msg: Option<&str>) {
        match msg {
            Some(m) => {
                Dev::ensure(ui);
                if let (Some(b), Some(bt)) = (ui.dev.banner, ui.dev.banner_text) {
                    ui.tree.set_text(bt, m);
                    ui.tree.set_visible(b, true);
                    Dev::raise(ui);
                }
            }
            None => {
                if let Some(b) = ui.dev.banner {
                    ui.tree.set_visible(b, false);
                }
            }
        }
    }

    /// Its elements stay the root's last children: drawn over everything the app adds.
    fn raise(ui: &mut Ui) {
        let root = ui.tree.root();
        let n = ui.tree.children(root).len();
        for id in [ui.dev.outline, ui.dev.panel, ui.dev.banner].into_iter().flatten() {
            ui.tree.reorder(id, n);
        }
    }

    /// Makes its elements the first time they are needed.
    fn ensure(ui: &mut Ui) {
        if ui.dev.outline.is_some_and(|o| ui.tree.alive(o)) {
            return;
        }
        let root = ui.tree.root();
        let abs = |inset| Layout { position: Position::Absolute, inset, ..Layout::default() };
        let auto = [Len::Auto; 4];
        let outline = ui.tree.add(
            root,
            abs(auto),
            Visual {
                border: 2.0,
                border_color: OUTLINE,
                bg: Some([0.30, 0.76, 1.0, 0.12]),
                visible: false,
                ..Visual::default()
            },
            Content::None,
        );
        let panel = ui.tree.add(
            root,
            Layout { padding: [8.0; 4], max_width: Len::Px(560.0), ..abs(auto) },
            Visual { bg: Some(PANEL_BG), radius: 6.0, visible: false, ..Visual::default() },
            Content::None,
        );
        let panel_text = ui.tree.add(panel, text_layout(), text_visual(), mono_text());
        let banner = ui.tree.add(
            root,
            Layout { padding: [8.0, 12.0, 8.0, 12.0], ..abs([Len::Auto, Len::Px(8.0), Len::Px(8.0), Len::Px(8.0)]) },
            Visual { bg: Some(BANNER_BG), radius: 6.0, visible: false, ..Visual::default() },
            Content::None,
        );
        let banner_text = ui.tree.add(banner, text_layout(), text_visual(), mono_text());
        for id in [outline, panel, panel_text, banner, banner_text] {
            ui.tree.set_tag(id, OWN);
        }
        let d = &mut ui.dev;
        (d.outline, d.panel, d.panel_text, d.banner, d.banner_text) =
            (Some(outline), Some(panel), Some(panel_text), Some(banner), Some(banner_text));
    }
}

/// The innermost shown element under (x, y), the last drawn first (the hit test's order is for
/// what takes input; the inspector names any element).
fn deepest(t: &crate::element::Tree, id: Id, x: i32, y: i32) -> Option<Id> {
    for c in t.children(id).into_iter().rev() {
        let e = t.el(c);
        if e.tag() == OWN || !e.shows_itself() {
            continue;
        }
        if t.rect(c).contains(x, y) {
            return Some(deepest(t, c, x, y).unwrap_or(c));
        }
    }
    None
}

fn text_layout() -> Layout {
    Layout { shrink: 1.0, min_width: Len::Px(0.0), ..Layout::default() }
}

fn text_visual() -> Visual {
    Visual { fg: [0.92, 0.94, 0.96, 1.0], ..Visual::default() }
}

fn mono_text() -> Content {
    Content::Text(crate::style::TextContent {
        size: MONO,
        line: MONO * 1.4,
        wrap: true,
        lines: 0,
        ..Default::default()
    })
}

/// The panel's text: what the element is, where it stands, its style where it differs from the
/// default, and its last damage.
fn describe(ui: &Ui, id: Id) -> String {
    let t = &ui.tree;
    let mut s = String::new();
    let name = |id: Id| t.dev_label(id).map_or_else(|| "element".to_string(), str::to_string);
    let _ = writeln!(s, "{}", name(id));
    // The path: its builder's labels from the root down (unlabelled ones by their kind).
    let mut path = Vec::new();
    let mut at = t.parent(id);
    while let Some(p) = at {
        if p == t.root() {
            break;
        }
        path.push(t.dev_label(p).map_or("·", |l| l.split(' ').next().unwrap_or("·")).to_string());
        at = t.parent(p);
    }
    path.reverse();
    if !path.is_empty() {
        let _ = writeln!(s, "in {}", path.join(" › "));
    }
    let r = t.rect(id);
    let sc = t.scale().max(0.01);
    let _ = writeln!(s, "box {},{} {}×{} px ({:.1}×{:.1} dips)", r.x, r.y, r.w, r.h, r.w as f32 / sc, r.h as f32 / sc);
    let e = t.el(id);
    let lay = differs(&format!("{:?}", e.layout()), &format!("{:?}", Layout::default()));
    if !lay.is_empty() {
        let _ = writeln!(s, "layout {lay}");
    }
    let vis = differs(&format!("{:?}", e.visual()), &format!("{:?}", Visual::default()));
    if !vis.is_empty() {
        let _ = writeln!(s, "visual {vis}");
    }
    match e.content() {
        Content::Text(tc) => {
            let _ = writeln!(s, "text {:?} {}/{} w{}", ellipsis(&tc.text, 60), tc.size, tc.line, tc.weight);
        }
        Content::None => {}
        c => {
            let _ = writeln!(s, "content {}", ellipsis(&format!("{c:?}"), 80));
        }
    }
    match t.dev_why(id) {
        Some(w) => {
            let _ = write!(s, "last damage: {} from {}:{}", w.what, w.at.file(), w.at.line());
        }
        None => {
            let _ = write!(s, "last damage: none since it was made");
        }
    }
    s
}

/// The fields of a `Debug` struct (`Name { a: 1, b: x }`) that differ from the default's.
fn differs(now: &str, default: &str) -> String {
    let a = fields(now);
    let b = fields(default);
    a.into_iter().filter(|f| !b.contains(f)).collect::<Vec<_>>().join(", ")
}

/// A `Debug` struct's top-level `field: value` parts.
fn fields(s: &str) -> Vec<&str> {
    let Some(open) = s.find('{') else { return Vec::new() };
    let body = s[open + 1..].trim_end().trim_end_matches('}').trim();
    let (mut out, mut depth, mut start) = (Vec::new(), 0i32, 0);
    let bytes = body.as_bytes();
    let mut quoted = false;
    for (i, &c) in bytes.iter().enumerate() {
        match c {
            b'"' => quoted = !quoted,
            b'(' | b'[' | b'{' if !quoted => depth += 1,
            b')' | b']' | b'}' if !quoted => depth -= 1,
            b',' if depth == 0 && !quoted => {
                out.push(body[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = body[start..].trim();
    if !last.is_empty() {
        out.push(last);
    }
    out
}

fn ellipsis(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).chain(['…']).collect()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn differs_lists_only_changed_fields() {
        let d = super::differs("L { a: 1, b: [1, 2], c: X { d: 3 } }", "L { a: 1, b: [0, 0], c: X { d: 3 } }");
        assert_eq!(d, "b: [1, 2]");
    }
}
