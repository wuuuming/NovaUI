//! The accessibility tree (AccessKit): the element tree as a screen reader sees it. Nothing is
//! built until an assistive client asks for it; then the whole tree is sent once, and after that
//! each turn sends the nodes of the elements that turn changed (`Tree::access_touched`). What the
//! client asks of an element (press it, focus it, set its value, show it) comes back as input.
//!
//! A node per element that means something to a client (`node_role`): one with a written role or
//! label, a control (a press, a drag, a text field), a text, an offset group (whose content moves
//! by the group's transform, so scrolling sends one node). The rest stand aside: their children
//! are their nearest node's. A text inside a control that is named by its content (a button, a
//! list item) is not a node of its own: it is that control's name.

use crate::app::{Event, Ui};
use crate::element::Id;
use crate::semantics::Role;
use accesskit as ak;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// What the platform adapter's threads share with the UI thread: a client asked for the tree or
/// went away, and the requests it made. Each wakes the host's loop.
pub(crate) struct Shared {
    wanted: AtomicBool,
    gone: AtomicBool,
    requests: Mutex<Vec<ak::ActionRequest>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Shared {
    pub(crate) fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Arc<Shared> {
        Arc::new(Shared {
            wanted: AtomicBool::new(false),
            gone: AtomicBool::new(false),
            requests: Mutex::default(),
            wake,
        })
    }
}

/// The platform adapter's handlers: they only note what came and wake the loop; the tree is
/// built and the requests are carried out on the UI thread, in the next turn.
pub(crate) struct Handler(pub(crate) Arc<Shared>);

impl ak::ActivationHandler for Handler {
    fn request_initial_tree(&mut self) -> Option<ak::TreeUpdate> {
        self.0.wanted.store(true, Ordering::Release);
        (self.0.wake)();
        None
    }
}

impl ak::ActionHandler for Handler {
    fn do_action(&mut self, request: ak::ActionRequest) {
        self.0.requests.lock().unwrap_or_else(|e| e.into_inner()).push(request);
        (self.0.wake)();
    }
}

impl ak::DeactivationHandler for Handler {
    fn deactivate_accessibility(&mut self) {
        self.0.gone.store(true, Ordering::Release);
        (self.0.wake)();
    }
}

/// A platform's adapter, as the turn uses it.
pub(crate) trait Adapter {
    /// Sends the update `make` gives, if a client has the tree.
    fn update(&mut self, make: &mut dyn FnMut() -> ak::TreeUpdate);
    /// The window gained or lost the keyboard.
    fn window_focus(&mut self, on: bool);
}

/// A host's side of accessibility: its adapter, and what the client has been sent.
pub(crate) struct Bridge {
    shared: Arc<Shared>,
    adapter: Box<dyn Adapter>,
    /// The window's title: the root's name.
    pub(crate) title: String,
    /// The nodes the client has, each with its children as sent; `None` until it asks.
    sent: Option<HashMap<ak::NodeId, Vec<ak::NodeId>>>,
    focus: ak::NodeId,
    window_focused: bool,
}

impl Bridge {
    pub(crate) fn new(shared: Arc<Shared>, adapter: Box<dyn Adapter>, title: String) -> Bridge {
        Bridge { shared, adapter, title, sent: None, focus: ak::NodeId(0), window_focused: false }
    }

    /// The client's requests since the last turn, as input to `ui` (`act`).
    pub(crate) fn requests(&mut self, ui: &mut Ui) {
        let requests = std::mem::take(&mut *self.shared.requests.lock().unwrap_or_else(|e| e.into_inner()));
        for r in requests {
            act(ui, &r);
        }
    }

    /// After the turn's frame: the whole tree to a client that just asked for it, else the
    /// nodes of what changed (and the focus). A client that went away is sent nothing more and
    /// the tree stops keeping track.
    pub(crate) fn update(&mut self, ui: &mut Ui) {
        if self.shared.gone.swap(false, Ordering::AcqRel) {
            self.sent = None;
            ui.tree.access_track(false);
        }
        if ui.window.focused != self.window_focused {
            self.window_focused = ui.window.focused;
            self.adapter.window_focus(self.window_focused);
        }
        if self.shared.wanted.swap(false, Ordering::AcqRel) {
            ui.tree.access_track(true);
            let _ = ui.tree.access_touched();
            let mut sent = HashMap::new();
            let root = ui.tree.root();
            let mut nodes = Vec::new();
            subtree(ui, root, &self.title, &mut sent, &mut nodes, &mut HashSet::new());
            self.focus = focus_of(ui, &sent);
            let mut update = Some(ak::TreeUpdate {
                nodes,
                tree: Some(tree_info(node_id(root))),
                tree_id: ak::TreeId::ROOT,
                focus: self.focus,
            });
            let focus = self.focus;
            self.adapter.update(&mut || update.take().unwrap_or_else(|| empty(focus)));
            self.sent = Some(sent);
            return;
        }
        let Some(sent) = &mut self.sent else { return };
        let touched = ui.tree.access_touched();
        let mut nodes = Vec::new();
        if !touched.is_empty() {
            changed(ui, &touched, &self.title, sent, &mut nodes);
        }
        let focus = focus_of(ui, sent);
        if nodes.is_empty() && focus == self.focus {
            return;
        }
        self.focus = focus;
        let mut update = Some(ak::TreeUpdate { nodes, tree: None, tree_id: ak::TreeId::ROOT, focus });
        self.adapter.update(&mut || update.take().unwrap_or_else(|| empty(focus)));
    }
}

fn tree_info(root: ak::NodeId) -> ak::TreeInfo {
    let mut t = ak::TreeInfo::new(root);
    t.toolkit_name = Some("NovaUI".into());
    t
}

/// An update that changes nothing (an adapter asking twice).
fn empty(focus: ak::NodeId) -> ak::TreeUpdate {
    ak::TreeUpdate { nodes: Vec::new(), tree: None, tree_id: ak::TreeId::ROOT, focus }
}

/// The node an element is (its id and generation, so a reused slot is another node).
pub(crate) fn node_id(id: Id) -> ak::NodeId {
    ak::NodeId(id.to_bits())
}

/// The focus as the client sees it: the focused element's node, else the window's.
fn focus_of(ui: &Ui, sent: &HashMap<ak::NodeId, Vec<ak::NodeId>>) -> ak::NodeId {
    match ui.focus.map(node_id).filter(|n| sent.contains_key(n)) {
        Some(n) => n,
        None => node_id(ui.tree.root()),
    }
}

/// The nodes of the elements a turn touched: each one that is a node, and the node holding it
/// (its children or its name may have changed). New children come with their subtrees; children
/// gone leave the record with theirs.
fn changed(
    ui: &Ui,
    touched: &[Id],
    title: &str,
    sent: &mut HashMap<ak::NodeId, Vec<ak::NodeId>>,
    nodes: &mut Vec<(ak::NodeId, ak::Node)>,
) {
    let tree = &ui.tree;
    let mut again: Vec<Id> = Vec::new();
    for &id in touched {
        if !tree.alive(id) {
            continue;
        }
        if node_role(ui, id).is_some() {
            again.push(id);
        }
        if let Some(holder) = holder(ui, id) {
            again.push(holder);
        }
    }
    again.sort();
    again.dedup();
    // An update names each node once: a new one sent with its holder's subtree is not sent again
    // for having been touched itself (AccessKit refuses a child listed twice).
    let mut done = HashSet::new();
    for id in again {
        let n = node_id(id);
        // Not sent: new, and sent with its holder's children; or out of the tree.
        if done.contains(&n) || !sent.contains_key(&n) || !tree.el(id).landable() {
            continue;
        }
        done.insert(n);
        let (node, kids) = node(ui, id, title);
        let old = sent.insert(n, kids.clone()).unwrap_or_default();
        nodes.push((n, node));
        // Gone first: a node that stopped being one hands its children to this one, which then
        // takes them as new.
        for k in old.iter().filter(|k| !kids.contains(k)) {
            forget(sent, *k);
        }
        for k in &kids {
            if !sent.contains_key(k) {
                if let Some(kid) = tree.id_from_bits(k.0) {
                    subtree(ui, kid, title, sent, nodes, &mut done);
                }
            }
        }
    }
}

/// `id`'s node and every node under it, as new.
fn subtree(
    ui: &Ui,
    id: Id,
    title: &str,
    sent: &mut HashMap<ak::NodeId, Vec<ak::NodeId>>,
    nodes: &mut Vec<(ak::NodeId, ak::Node)>,
    done: &mut HashSet<ak::NodeId>,
) {
    let mut stack = vec![id];
    while let Some(id) = stack.pop() {
        if !done.insert(node_id(id)) {
            continue;
        }
        let (node, kids) = node(ui, id, title);
        for k in &kids {
            if let Some(kid) = ui.tree.id_from_bits(k.0) {
                stack.push(kid);
            }
        }
        nodes.push((node_id(id), node));
        sent.insert(node_id(id), kids);
    }
}

/// A node gone from its holder's children: it and its subtree leave the record.
fn forget(sent: &mut HashMap<ak::NodeId, Vec<ak::NodeId>>, n: ak::NodeId) {
    let mut stack = vec![n];
    while let Some(n) = stack.pop() {
        if let Some(kids) = sent.remove(&n) {
            stack.extend(kids);
        }
    }
}

/// The nearest ancestor of `id` that is a node.
fn holder(ui: &Ui, id: Id) -> Option<Id> {
    ui.tree.ancestors(id).find(|&a| node_role(ui, a).is_some())
}

/// What `id` is to a client, or `None` when it is no node (its children are its holder's).
fn node_role(ui: &Ui, id: Id) -> Option<ak::Role> {
    let tree = &ui.tree;
    if id == tree.root() {
        return Some(ak::Role::Window);
    }
    if field_part(ui, id) {
        return None;
    }
    let el = tree.el(id);
    let written = el.semantics();
    if let Some(r) = written.and_then(|s| s.role) {
        return role(r, ui, id);
    }
    if let Some(e) = ui.io.edits.get(&id) {
        return Some(if e.secret { ak::Role::PasswordInput } else { ak::Role::TextInput });
    }
    if written.is_some_and(|s| s.range.is_some()) {
        return Some(ak::Role::Slider);
    }
    if el.action().is_some() {
        return Some(ak::Role::Button);
    }
    if el.drag_events() {
        return Some(ak::Role::Slider);
    }
    if tree.access_group(id) {
        return Some(ak::Role::GenericContainer);
    }
    if written.is_some_and(|s| s.label.is_some()) {
        return Some(match el.content() {
            crate::style::Content::Image(_) | crate::style::Content::Icon(_) | crate::style::Content::Path(_) => {
                ak::Role::Image
            }
            _ => ak::Role::Group,
        });
    }
    let text = tree.text_of(id).is_some_and(|t| !t.trim().is_empty());
    (text && !named_by_content_above(ui, id)).then_some(ak::Role::Label)
}

/// A written role as AccessKit's.
fn role(r: Role, ui: &Ui, id: Id) -> Option<ak::Role> {
    Some(match r {
        Role::None => return None,
        Role::Group => ak::Role::Group,
        Role::Button => ak::Role::Button,
        Role::Switch => ak::Role::Switch,
        Role::Slider => ak::Role::Slider,
        Role::List => ak::Role::List,
        Role::ListItem => ak::Role::ListItem,
        Role::TextInput => match ui.io.edits.get(&id) {
            Some(e) if e.secret => ak::Role::PasswordInput,
            _ => ak::Role::TextInput,
        },
        Role::Label => ak::Role::Label,
        Role::Heading => ak::Role::Heading,
        Role::Image => ak::Role::Image,
        Role::Menu => ak::Role::Menu,
        // One of a set (`Semantics::checked`): a radio item, its check said with it.
        Role::MenuItem if ui.tree.el(id).semantics().is_some_and(|s| s.checked.is_some()) => ak::Role::MenuItemRadio,
        Role::MenuItem => ak::Role::MenuItem,
        Role::Dialog => ak::Role::Dialog,
    })
}

/// A role whose name is the text inside it.
fn named_by_content(r: ak::Role) -> bool {
    matches!(
        r,
        ak::Role::Button
            | ak::Role::Switch
            | ak::Role::ListItem
            | ak::Role::MenuItem
            | ak::Role::MenuItemRadio
            | ak::Role::Heading
    )
}

/// An ancestor of `id` is named by its content: `id`'s text is that ancestor's name.
fn named_by_content_above(ui: &Ui, id: Id) -> bool {
    ui.tree.ancestors(id).any(|a| node_role(ui, a).is_some_and(named_by_content))
}

/// The text of `id` and inside it, shown, in document order: a row is called by all it says
/// (its links too).
fn content_name(ui: &Ui, id: Id) -> String {
    let tree = &ui.tree;
    let mut out = String::new();
    let mut stack = vec![id];
    while let Some(c) = stack.pop() {
        if !tree.el(c).landable() || field_part(ui, c) {
            continue;
        }
        if let Some(t) = tree.text_of(c).map(str::trim).filter(|t| !t.is_empty()) {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(t);
        }
        stack.extend(tree.children(c).into_iter().rev());
    }
    out
}

/// The nodes that are `id`'s children: the nearest nodes under it, shown, in document order.
fn node_children(ui: &Ui, id: Id) -> Vec<ak::NodeId> {
    let tree = &ui.tree;
    let mut out = Vec::new();
    let mut stack: Vec<Id> = tree.children(id).into_iter().rev().collect();
    while let Some(c) = stack.pop() {
        if !tree.el(c).landable() || field_part(ui, c) {
            continue;
        }
        if node_role(ui, c).is_some() {
            out.push(node_id(c));
        } else {
            stack.extend(tree.children(c).into_iter().rev());
        }
    }
    out
}

/// One of the elements a text field draws itself with (its text, caret, selection): the field's
/// node says all of it.
fn field_part(ui: &Ui, id: Id) -> bool {
    ui.tree
        .parent(id)
        .and_then(|p| ui.io.edits.get(&p))
        .is_some_and(|e| [e.text_el, e.caret_el, e.sel_el, e.ime_el].contains(&id))
}

/// `id`'s node and its children's ids.
fn node(ui: &Ui, id: Id, title: &str) -> (ak::Node, Vec<ak::NodeId>) {
    let tree = &ui.tree;
    let el = tree.el(id);
    let r = node_role(ui, id).unwrap_or(ak::Role::GenericContainer);
    let mut n = ak::Node::new(r);
    let written = el.semantics();
    // Its box in its offset group's space; a group's content moves by the group's offset.
    let (b, off) = tree.access_box(id);
    if let Some((dx, dy)) = off {
        n.set_transform(ak::Affine::translate((dx as f64, dy as f64)));
    }
    let (dx, dy) = off.unwrap_or((0, 0));
    n.set_bounds(ak::Rect {
        x0: (b.x - dx) as f64,
        y0: (b.y - dy) as f64,
        x1: (b.r() - dx) as f64,
        y1: (b.b() - dy) as f64,
    });
    let label = match written.and_then(|s| s.label.clone()) {
        Some(l) => Some(l),
        None if id == tree.root() => Some(title.to_string()),
        None if named_by_content(r) => Some(content_name(ui, id)).filter(|s| !s.is_empty()),
        // A field without a written name is called by its hint ("Search").
        None => ui.io.edits.get(&id).map(|e| e.hint.clone()).filter(|h| !h.is_empty()),
    };
    if let Some(l) = label {
        n.set_label(l);
    }
    // A label's text is its value (AccessKit names a label by it).
    if r == ak::Role::Label {
        if let Some(t) = tree.text_of(id) {
            n.set_value(t.trim());
        }
    }
    if let Some(e) = ui.io.edits.get(&id) {
        if !e.secret {
            n.set_value(e.st.text.clone());
        }
        if !e.hint.is_empty() {
            n.set_placeholder(e.hint.clone());
        }
    }
    if let Some(range) = written.and_then(|s| s.range) {
        n.set_numeric_value(range.value);
        n.set_min_numeric_value(range.min);
        n.set_max_numeric_value(range.max);
        n.set_numeric_value_step(range.step);
        n.add_action(ak::Action::Increment);
        n.add_action(ak::Action::Decrement);
        n.add_action(ak::Action::SetValue);
    }
    match r {
        ak::Role::Switch => n.set_toggled(ak::Toggled::from(el.lit())),
        ak::Role::MenuItemRadio => {
            n.set_toggled(ak::Toggled::from(written.and_then(|s| s.checked).unwrap_or(false)));
            n.set_selected(el.lit());
        }
        ak::Role::ListItem | ak::Role::MenuItem => n.set_selected(el.lit()),
        _ => {}
    }
    if written.is_some_and(|s| s.live) {
        n.set_live(ak::Live::Polite);
    }
    if el.disabled() {
        n.set_disabled();
    } else {
        if el.focusable() {
            n.add_action(ak::Action::Focus);
        }
        if el.action().is_some() {
            n.add_action(ak::Action::Click);
        }
    }
    n.add_action(ak::Action::ScrollIntoView);
    let kids = node_children(ui, id);
    n.set_children(kids.clone());
    (n, kids)
}

/// A client's request, as input: a press is the element's click, a focus the keyboard's, a
/// value the slider's `Event::Value`, a request to show it `Event::Reveal`.
fn act(ui: &mut Ui, r: &ak::ActionRequest) {
    let Some(id) = ui.tree.id_from_bits(r.target_node.0).filter(|&id| ui.tree.alive(id)) else { return };
    let el = ui.tree.el(id);
    if el.disabled() && r.action != ak::Action::ScrollIntoView {
        return;
    }
    let range = el.semantics().and_then(|s| s.range);
    match r.action {
        ak::Action::Click => match el.action().cloned() {
            Some(action) => ui.events.push(Event::Click { id, action: Some(action) }),
            None if el.focusable() => ui.focus_by_key(id),
            None => {}
        },
        ak::Action::Focus if el.focusable() => ui.focus_by_key(id),
        ak::Action::Blur if ui.focus == Some(id) => ui.set_focus(None),
        ak::Action::Increment | ak::Action::Decrement => {
            if let Some(range) = range {
                let steps = if r.action == ak::Action::Increment { 1.0 } else { -1.0 };
                ui.events.push(Event::Value { id, value: range.stepped(steps) });
            }
        }
        ak::Action::SetValue => {
            if let (Some(range), Some(ak::ActionData::NumericValue(v))) = (range, &r.data) {
                let value = v.clamp(range.min.min(range.max), range.max.max(range.min));
                ui.events.push(Event::Value { id, value });
            }
        }
        ak::Action::ScrollIntoView => ui.events.push(Event::Reveal { id }),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::{parse, Doc};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// An adapter that keeps what it is sent.
    struct Kept(Rc<RefCell<Vec<ak::TreeUpdate>>>);

    impl Adapter for Kept {
        fn update(&mut self, make: &mut dyn FnMut() -> ak::TreeUpdate) {
            self.0.borrow_mut().push(make());
        }
        fn window_focus(&mut self, _: bool) {}
    }

    const SHEET: &str = r#"
      Column { padding: 8; gap: 4;
        Button #play { width: 40; height: 24; label: "Play"; on-click: play; }
        Box #seek { width: 200; height: 20; hit: true; drag-events: true; value: 30; min: 0; max: 100; step: 5; label: "Seek"; }
        Column #list { role: list;
          Button #row { height: 24; on-click: row; Text #title { text: "Paper Lantern"; } }
        }
        Text #note { text: "Hello"; }
        Box #pane { shown: false; Text { text: "Hidden"; } }
      }
    "#;

    /// What an adapter was sent.
    type Sent = Rc<RefCell<Vec<ak::TreeUpdate>>>;

    fn scene() -> (Ui, Doc, Bridge, Arc<Shared>, Sent) {
        let mut ui = Ui::new(320, 240, 1.0);
        let mut doc = Doc::new(parse(SHEET).unwrap());
        let root = ui.tree.root();
        doc.build(&mut ui, root).unwrap();
        ui.tree.frame(&mut crate::render::software::Software::new(320, 240), 0.0);
        let sent = Rc::new(RefCell::new(Vec::new()));
        let shared = Shared::new(Arc::new(|| {}));
        let bridge = Bridge::new(shared.clone(), Box::new(Kept(sent.clone())), "Player".into());
        (ui, doc, bridge, shared, sent)
    }

    /// No node twice in one update, and no child under two parents (AccessKit panics on both).
    fn each_once(u: &ak::TreeUpdate) {
        let mut nodes = HashSet::new();
        let mut kids = HashSet::new();
        for (n, node) in &u.nodes {
            assert!(nodes.insert(*n), "node {n:?} sent twice in one update");
            for k in node.children() {
                assert!(kids.insert(*k), "child {k:?} listed twice in one update");
            }
        }
    }

    fn find(u: &ak::TreeUpdate, id: Id) -> Option<&ak::Node> {
        u.nodes.iter().find(|(n, _)| *n == node_id(id)).map(|(_, n)| n)
    }

    #[test]
    fn nothing_is_built_until_a_client_asks_then_the_whole_tree_once() {
        let (mut ui, doc, mut bridge, shared, sent) = scene();
        bridge.update(&mut ui);
        assert!(sent.borrow().is_empty(), "no client, no tree");
        assert!(ui.tree.access_touched().is_empty(), "nothing kept for a client that is not there");
        assert!(ak::ActivationHandler::request_initial_tree(&mut Handler(shared)).is_none());
        bridge.update(&mut ui);
        let u = &sent.borrow()[0];
        assert!(u.tree.is_some());
        let root = find(u, ui.tree.root()).unwrap();
        assert_eq!((root.role(), root.label()), (ak::Role::Window, Some("Player")));
        let play = find(u, doc.id("play")).unwrap();
        assert_eq!((play.role(), play.label()), (ak::Role::Button, Some("Play")));
        let seek = find(u, doc.id("seek")).unwrap();
        assert_eq!(
            (seek.role(), seek.numeric_value(), seek.max_numeric_value()),
            (ak::Role::Slider, Some(30.0), Some(100.0))
        );
        assert_eq!(find(u, doc.id("list")).unwrap().role(), ak::Role::List);
        let row = find(u, doc.id("row")).unwrap();
        assert_eq!(row.label(), Some("Paper Lantern"), "a button is named by the text inside it");
        assert!(find(u, doc.id("title")).is_none(), "the text a button is named by is no node of its own");
        let note = find(u, doc.id("note")).unwrap();
        assert_eq!((note.role(), note.value()), (ak::Role::Label, Some("Hello")));
        assert!(find(u, doc.id("pane")).is_none(), "what is not shown is not sent");
        assert_eq!(u.focus, node_id(ui.tree.root()));
    }

    #[test]
    fn a_turn_sends_what_it_changed_and_the_focus() {
        let (mut ui, doc, mut bridge, shared, sent) = scene();
        let _ = ak::ActivationHandler::request_initial_tree(&mut Handler(shared));
        bridge.update(&mut ui);
        bridge.update(&mut ui);
        assert_eq!(sent.borrow().len(), 1, "an unchanged turn sends nothing");
        ui.tree.set_text(doc.id("title"), "Glass Harbour");
        ui.tree.frame(&mut crate::render::software::Software::new(320, 240), 0.1);
        bridge.update(&mut ui);
        {
            let s = sent.borrow();
            let u = s.last().unwrap();
            assert_eq!(find(u, doc.id("row")).unwrap().label(), Some("Glass Harbour"));
            assert!(find(u, doc.id("play")).is_none(), "an unchanged node is not sent again");
        }
        // The pane shows: its text comes with it, as new.
        ui.tree.set_layout(doc.id("pane"), |l| l.display = crate::style::Display::Flex);
        ui.tree.frame(&mut crate::render::software::Software::new(320, 240), 0.2);
        bridge.update(&mut ui);
        assert!(sent.borrow().last().unwrap().nodes.iter().any(|(_, n)| n.value() == Some("Hidden")));
        // New elements under a node that is sent: each comes once, though made (touched) this
        // turn too (NovaPlayer died on "TreeUpdate includes duplicate child").
        let row = doc.id("row");
        let inner = ui.tree.add(row, Default::default(), Default::default(), Default::default());
        ui.tree.set_action(inner, Some("more"));
        let deeper = ui.tree.add(inner, Default::default(), Default::default(), Default::default());
        ui.tree.set_action(deeper, Some("deeper"));
        ui.tree.frame(&mut crate::render::software::Software::new(320, 240), 0.3);
        bridge.update(&mut ui);
        for u in sent.borrow().iter() {
            each_once(u);
        }
        ui.focus_by_key(doc.id("play"));
        bridge.update(&mut ui);
        assert_eq!(sent.borrow().last().unwrap().focus, node_id(doc.id("play")));
    }

    #[test]
    fn a_clients_requests_come_back_as_input() {
        let (mut ui, doc, mut bridge, shared, _) = scene();
        let ask = |action, id: Id, data| ak::ActionRequest {
            action,
            target_tree: ak::TreeId::ROOT,
            target_node: node_id(id),
            data,
        };
        let mut h = Handler(shared);
        ak::ActionHandler::do_action(&mut h, ask(ak::Action::Click, doc.id("play"), None));
        ak::ActionHandler::do_action(&mut h, ask(ak::Action::Increment, doc.id("seek"), None));
        let to = Some(ak::ActionData::NumericValue(250.0));
        ak::ActionHandler::do_action(&mut h, ask(ak::Action::SetValue, doc.id("seek"), to));
        ak::ActionHandler::do_action(&mut h, ask(ak::Action::Focus, doc.id("row"), None));
        bridge.requests(&mut ui);
        let ev = ui.take_events();
        assert!(matches!(&ev[0], Event::Click { id, action: Some(a) } if *id == doc.id("play") && a.is("play")));
        assert!(matches!(ev[1], Event::Value { id, value } if id == doc.id("seek") && value == 35.0));
        assert!(matches!(ev[2], Event::Value { value, .. } if value == 100.0), "a value is kept in the range");
        assert_eq!(ui.focus(), Some(doc.id("row")));
    }

    /// A menu item said to be one of a set (`Semantics::checked`) is a radio item that says
    /// whether it is the one; its check mark is only its look.
    #[test]
    fn a_checked_menu_item_says_whether_it_is_the_one() {
        let (mut ui, doc, mut bridge, shared, sent) = scene();
        for (id, on) in [(doc.id("play"), true), (doc.id("row"), false)] {
            ui.tree.set_semantics(id, |s| {
                s.role = Some(Role::MenuItem);
                s.checked = Some(on);
            });
        }
        let _ = ak::ActivationHandler::request_initial_tree(&mut Handler(shared));
        bridge.update(&mut ui);
        let u = &sent.borrow()[0];
        let play = find(u, doc.id("play")).unwrap();
        assert_eq!((play.role(), play.toggled()), (ak::Role::MenuItemRadio, Some(ak::Toggled::True)));
        let row = find(u, doc.id("row")).unwrap();
        assert_eq!((row.role(), row.toggled()), (ak::Role::MenuItemRadio, Some(ak::Toggled::False)));
        assert_eq!(row.label(), Some("Paper Lantern"), "named by the text inside it");
    }
}
