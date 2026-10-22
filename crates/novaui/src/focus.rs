//! The keyboard focus: which element the keys go to first, how Tab moves it, and what a focused
//! element does with its keys (`novaui-spec.md`, "Keyboard focus").
//!
//! Any focusable element can hold the focus (`El::focusable`): a text field, a button, a slider,
//! a list's item. A press focuses a text field and drops the focus anywhere else; Tab and
//! Shift+Tab move it through the Tab stops in document order, a list counting as one stop; an
//! assistive client can put it anywhere. Focus that came from the keyboard (or an assistive
//! client) shows the focus visual; focus a press gave does not.

use crate::app::{Event, Ui};
use crate::element::{Id, State};
use crate::input::{key, Mods};
use crate::semantics::Role;

impl Ui {
    /// The element that has the keyboard focus.
    pub fn focus(&self) -> Option<Id> {
        self.focus
    }

    /// The text field that has the focus (`None` when the focus is elsewhere or nowhere).
    pub fn focused_field(&self) -> Option<Id> {
        self.focus.filter(|f| self.io.edits.contains_key(f))
    }

    /// Gives `f` the keyboard focus (`None`: nobody). A text field that loses it hears `Blur`.
    /// The focus visual follows how the focus last moved: shown after the keyboard, not after a
    /// press.
    pub fn set_focus(&mut self, f: Option<Id>) {
        self.forget_removed();
        if self.focus == f {
            return;
        }
        if let Some(old) = self.focus.take() {
            self.tree.set_state(old, State::Focused, false);
            self.tree.set_ring(old, false);
            self.edit_blur(old);
        }
        self.focus = f;
        if let Some(n) = f {
            self.tree.set_state(n, State::Focused, true);
            self.show_ring();
            self.edit_focus(n);
        }
    }

    /// Moves the focus as the keyboard (or an assistive client) does: the focus visual shows,
    /// and the app hears `Reveal` so it can scroll the element into view.
    pub(crate) fn focus_by_key(&mut self, id: Id) {
        self.keyboard_focus = true;
        self.set_focus(Some(id));
        self.show_ring();
        self.events.push(Event::Reveal { id });
    }

    /// A press moves the focus (or drops it): no focus visual from here on.
    pub(crate) fn focus_by_press(&mut self, f: Option<Id>) {
        self.keyboard_focus = false;
        if let Some(old) = self.focus {
            self.tree.set_ring(old, false);
        }
        self.set_focus(f);
    }

    /// The focus visual is drawn around the focused element when the focus came from the
    /// keyboard; a text field shows its focus in its own look, never with the ring.
    fn show_ring(&mut self) {
        if let Some(f) = self.focus {
            let ring = self.keyboard_focus && !self.io.edits.contains_key(&f);
            self.tree.set_ring(f, ring);
        }
    }

    /// Whether the keyboard moves the focus and uses it (Tab, Enter / Space, the arrows on a
    /// slider or a list). On by default.
    pub fn keyboard_navigation(&self) -> bool {
        self.keyboard_navigation
    }

    /// Turns keyboard navigation on or off. Off, every key goes to a text field or the app as
    /// it would without a focus (`Event::Key`, Tab included), and a focus outside a text field
    /// is dropped; presses and assistive clients still move the focus.
    pub fn set_keyboard_navigation(&mut self, on: bool) {
        self.keyboard_navigation = on;
        if !on && self.focused_field().is_none() {
            self.focus_by_press(None);
        }
    }

    /// A key for the focus: Tab moves it; Enter and Space press the focused element; the arrows
    /// step a slider or move along a list. True when the key was used (the app does not hear it).
    pub(crate) fn focus_key(&mut self, vk: u32, mods: Mods) -> bool {
        if !self.keyboard_navigation || mods.ctrl || mods.alt {
            return false;
        }
        if vk == key::TAB {
            let to = self.tab_target(mods.shift);
            if let Some(to) = to {
                self.focus_by_key(to);
            }
            return true;
        }
        // While a popup is open, a focus outside it (the button that opened it) keeps its keys
        // for the app: Enter picks in the popup, it does not press the button again.
        let scope = self.focus_scope();
        let Some(f) = self
            .focus
            .filter(|&f| self.tree.alive(f) && !self.io.edits.contains_key(&f))
            .filter(|&f| f == scope || self.tree.is_descendant(f, scope))
        else {
            return false;
        };
        let el = self.tree.el(f);
        if el.disabled() {
            return false;
        }
        if let Some(r) = el.semantics().and_then(|s| s.range) {
            let value = match vk {
                key::LEFT | key::DOWN => r.stepped(-1.0),
                key::RIGHT | key::UP => r.stepped(1.0),
                key::PAGE_DOWN => r.stepped(-10.0),
                key::PAGE_UP => r.stepped(10.0),
                key::HOME => r.min,
                key::END => r.max,
                _ => return false,
            };
            self.events.push(Event::Value { id: f, value });
            return true;
        }
        if matches!(vk, key::ENTER | key::SPACE) {
            if let Some(action) = el.action().cloned() {
                self.events.push(Event::Click { id: f, action: Some(action) });
                return true;
            }
        }
        if matches!(vk, key::UP | key::DOWN) {
            if let Some(list) = self.list_of(f) {
                if let Some(to) = self.list_step(list, f, vk == key::DOWN) {
                    self.focus_by_key(to);
                }
                // At either end the key stays the list's: it is not the app's shortcut.
                return true;
            }
        }
        false
    }

    /// Where Tab (or Shift+Tab, `back`) takes the focus: the next Tab stop in document order
    /// inside the topmost popup (or the window), round from the end to the start.
    fn tab_target(&self, back: bool) -> Option<Id> {
        let (stops, at) = self.tab_stops(self.focus_scope());
        if stops.is_empty() {
            return None;
        }
        let n = stops.len();
        let i = match at {
            // The focus is a stop: the one after it (before it).
            Some(Ok(i)) => {
                if back {
                    (i + n - 1) % n
                } else {
                    (i + 1) % n
                }
            }
            // The focus is between stops (hidden, or not one): the first after it (before it).
            Some(Err(i)) => {
                if back {
                    (i + n - 1) % n
                } else {
                    i % n
                }
            }
            None if back => n - 1,
            None => 0,
        };
        Some(stops[i])
    }

    /// Where the focus can go: the topmost popup, else the window.
    fn focus_scope(&self) -> Id {
        self.popups.iter().rev().map(|p| p.id).find(|&p| self.tree.alive(p)).unwrap_or(self.tree.root())
    }

    /// The Tab stops under `scope` in document order, and where the focus stands among them:
    /// `Ok(i)` it is stop `i`, `Err(i)` it comes just before stop `i`. A list is one stop: the
    /// item that has the focus, else its first item in view.
    fn tab_stops(&self, scope: Id) -> (Vec<Id>, Option<Result<usize, usize>>) {
        let tree = &self.tree;
        let mut stops = Vec::new();
        let mut at = None;
        let mut stack = vec![scope];
        while let Some(id) = stack.pop() {
            let el = tree.el(id);
            if Some(id) == self.focus {
                at = Some(Err(stops.len()));
            }
            if !el.landable() || el.disabled() {
                continue;
            }
            if el.role() == Some(Role::List) {
                let items = self.list_items(id);
                let focused = self.focus.filter(|f| items.contains(f));
                if let Some(stop) = focused.or_else(|| self.first_in_view(id, &items)) {
                    stops.push(stop);
                    if focused.is_some() {
                        at = Some(Ok(stops.len() - 1));
                    }
                }
                continue;
            }
            if el.reachable() {
                if Some(id) == self.focus {
                    at = Some(Ok(stops.len()));
                }
                stops.push(id);
            }
            stack.extend(tree.children(id).into_iter().rev());
        }
        (stops, at)
    }

    /// A list's items: the reachable elements in it that are not inside another one, in the
    /// order they stand (top to bottom, then left to right): a virtual list reuses its rows, so
    /// their order as children says nothing.
    fn list_items(&self, list: Id) -> Vec<Id> {
        let tree = &self.tree;
        let mut items = Vec::new();
        let mut stack = tree.children(list);
        while let Some(id) = stack.pop() {
            let el = tree.el(id);
            if !el.landable() || el.disabled() {
                continue;
            }
            if el.reachable() {
                items.push(id);
            } else {
                stack.extend(tree.children(id));
            }
        }
        items.sort_by_key(|&i| {
            let r = tree.rect(i);
            (r.y, r.x)
        });
        items
    }

    /// The first of `items` whose box reaches into the list's.
    fn first_in_view(&self, list: Id, items: &[Id]) -> Option<Id> {
        let view = self.tree.rect(list);
        items.iter().copied().find(|&i| self.tree.rect(i).intersects(&view)).or(items.first().copied())
    }

    /// The list `item` is an item of.
    fn list_of(&self, item: Id) -> Option<Id> {
        let list = self.tree.ancestors(item).find(|&a| self.tree.el(a).role() == Some(Role::List))?;
        self.list_items(list).contains(&item).then_some(list)
    }

    /// The item after (`down`) or before `item` in `list`, if any.
    fn list_step(&self, list: Id, item: Id, down: bool) -> Option<Id> {
        let items = self.list_items(list);
        let i = items.iter().position(|&x| x == item)?;
        if down {
            items.get(i + 1).copied()
        } else {
            i.checked_sub(1).map(|i| items[i])
        }
    }

    /// A popup goes: the focus inside it returns to the element it was opened from.
    pub(crate) fn popup_closed(&mut self, popup: crate::app::Popup) {
        let inside = self.focus.is_some_and(|f| f == popup.id || self.tree.is_descendant(f, popup.id));
        if inside {
            match popup.anchor.filter(|&a| self.tree.alive(a)) {
                Some(a) => {
                    self.set_focus(Some(a));
                    self.show_ring();
                }
                None => self.set_focus(None),
            }
        }
    }
}
