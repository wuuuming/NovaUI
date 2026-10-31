//! Input: raw host input (`Input`), the pointer gesture (`Pointer`), and how `Ui` turns them into
//! element states and app `Event`s.

use crate::app::{Event, Ui};
use crate::element::Id;
use crate::style::*;

/// How far a held pointer moves, in physical px, before a press becomes a drag (an element's, or
/// the title bar's window move).
pub(crate) const DRAG_SLOP: i32 = 4;

/// Key codes by name (`Input::Key`, `Event::Key`). The values are Windows' virtual-key numbers:
/// the one vocabulary every host translates its own keys into (Linux's keysyms in
/// `platform::linux`), so an app matches on these names, never on a number.
pub mod key {
    pub const BACKSPACE: u32 = 0x08;
    pub const TAB: u32 = 0x09;
    pub const ENTER: u32 = 0x0D;
    pub const SHIFT: u32 = 0x10;
    pub const CONTROL: u32 = 0x11;
    pub const ALT: u32 = 0x12;
    pub const ESCAPE: u32 = 0x1B;
    pub const SPACE: u32 = 0x20;
    pub const PAGE_UP: u32 = 0x21;
    pub const PAGE_DOWN: u32 = 0x22;
    pub const END: u32 = 0x23;
    pub const HOME: u32 = 0x24;
    pub const LEFT: u32 = 0x25;
    pub const UP: u32 = 0x26;
    pub const RIGHT: u32 = 0x27;
    pub const DOWN: u32 = 0x28;
    pub const DELETE: u32 = 0x2E;
    /// The mouse's back button, and a keyboard's Back key.
    pub const BROWSER_BACK: u32 = 0xA6;

    /// A letter's key (`'a'` and `'A'` are one key).
    pub const fn letter(c: char) -> u32 {
        c.to_ascii_uppercase() as u32
    }

    /// A digit's key on the main block (`0..=9`).
    pub const fn digit(n: u32) -> u32 {
        0x30 + n
    }

    /// Function key F`n` (`1..=12`).
    pub const fn function(n: u32) -> u32 {
        0x6F + n
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// A mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Raw input from the host, in physical client pixels.
#[derive(Clone, Debug)]
pub enum Input {
    Move(i32, i32),
    Leave,
    Down(i32, i32, MouseButton),
    Up(i32, i32, MouseButton),
    DoubleClick(i32, i32),
    Wheel(i32, i32, f32),
    Key(u32, Mods),
    Char(char),
    Files(Vec<std::path::PathBuf>),
    Focus(bool),
    /// Pointer ownership changed externally, without implying keyboard/window focus changed.
    CaptureLost,
    /// The IME's composition string and its cursor (chars); empty = composition ended.
    Preedit(String, usize),
    /// Committed IME text.
    Commit(String),
    /// The modifier keys held as the pointer goes down: a host sends it before every `Down`
    /// (`Ui::press_mods`; Shift+press extends a text selection, a list reads Shift / Ctrl).
    Mods(Mods),
}

/// What the left button is doing. One gesture at a time; the transitions are in `Ui::input`,
/// `Ui::cancel_drag` and `Ui::forget_removed`:
///
/// | from | left Down | Move | left Up | right Down | cancel¹ | element removed |
/// |---|---|---|---|---|---|---|
/// | Idle | Pressed, or Selecting² | – | – | RightClick | – | – |
/// | Pressed | Pressed (again) | Dragging past the slop³ | Click (DragEnd³), Idle | RightClick | DragCancel³, Idle | DragCancel, Idle |
/// | Dragging | – | Drag | DragEnd, Idle | DragCancel, Idle | DragCancel, Idle | DragCancel, Idle |
/// | Selecting | – | the selection follows | Click, Idle | RightClick | Idle | Idle |
///
/// ¹ capture lost, window focus lost, Esc while dragging, `cancel_drag`. ² a press into a text
/// field's text. ³ only for `drag-events` elements.
///
/// The middle button is not a state here: it chords with the left one (a carry can be
/// autoscrolled), so it is a flag beside it, raising `MiddleDown` / `MiddleMove` / `MiddleUp`.
/// What it does (autoscroll) is the app's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pointer {
    #[default]
    Idle,
    /// The left button went down on `id` at (x, y).
    Pressed { id: Id, x: i32, y: i32 },
    /// A `drag-events` element is being dragged.
    Dragging { id: Id },
    /// The left button went down in a text field's text: moves select.
    Selecting { field: Id },
}

impl Pointer {
    /// The element holding the left button.
    pub fn captured(&self) -> Option<Id> {
        match *self {
            Pointer::Pressed { id, .. } | Pointer::Dragging { id } => Some(id),
            Pointer::Selecting { field } => Some(field),
            Pointer::Idle => None,
        }
    }
}

impl Ui {
    /// The left button's press on `id` begins (`on`) or ends: its `Pressed` state, and
    /// `Event::Pressed` for a `press-events` element.
    fn set_pressed(&mut self, id: Id, on: bool) {
        self.tree.set_state(id, crate::element::State::Pressed, on);
        if self.tree.alive(id) && self.tree.el(id).press_events() {
            self.events.push(Event::Pressed { id, on });
        }
    }

    /// The pointer comes onto `id` (`on`) or leaves it: its `Hovered` state, and `Event::Hover`
    /// for a `hover-events` element.
    fn set_hovered(&mut self, id: Id, on: bool) {
        self.tree.set_state(id, crate::element::State::Hovered, on);
        if self.tree.el(id).hover_events() {
            self.events.push(Event::Hover { id, on });
        }
    }

    /// The gesture in progress ends: the element it pressed is released. Returns it.
    fn end_gesture(&mut self) -> Pointer {
        let gesture = std::mem::take(&mut self.gesture);
        if let Some(id) = gesture.captured() {
            self.set_pressed(id, false);
        }
        gesture
    }

    /// An ending gesture on `id` ends as a drag (`DragEnd`, `DragCancel`): it was dragging, or
    /// `id` hears its drags from the press on.
    fn ends_as_drag(&self, gesture: Pointer, id: Id) -> bool {
        matches!(gesture, Pointer::Dragging { .. }) || self.tree.el(id).drag_events()
    }

    pub(crate) fn set_hover(&mut self, x: i32, y: i32) {
        let mut hits = self.tree.hits(x, y, |e| e.takes_pointer());
        // The topmost of them is hovered, and its ancestors with it; what it covers is not (a
        // list row under an open flyout, a row under its scrollbar).
        if let Some(&top) = hits.first() {
            hits.retain(|&h| h == top || self.tree.is_descendant(top, h));
        }
        let target = self.tree.hit(x, y, |e| e.is_target());
        // While pressed, only the pressed element keeps hover.
        let hits = if let Some(p) = self.gesture.captured() {
            hits.into_iter().filter(|&h| h == p || self.tree.el(h).hover_events()).collect()
        } else {
            hits
        };
        for &h in &self.hovered.clone() {
            if !hits.contains(&h) && self.tree.alive(h) {
                self.set_hovered(h, false);
            }
        }
        for &h in &hits {
            if !self.hovered.contains(&h) {
                self.set_hovered(h, true);
            }
        }
        self.hovered = hits;
        self.target = target;
        self.cursor = if let Some(c) = self.cursor_override {
            c
        } else {
            match self.gesture.captured().or(target) {
                Some(t) if self.tree.alive(t) => {
                    let e = self.tree.el(t);
                    if e.disabled() {
                        Cursor::Arrow
                    } else {
                        e.visual().cursor
                    }
                }
                _ => Cursor::Arrow,
            }
        };
    }

    /// Recomputes hover after layout changed under a still pointer.
    /// After a drawn frame: what is under the pointer may have changed only where the frame drew,
    /// or anywhere when layout or structure changed. Otherwise the hit test is skipped.
    pub fn refresh_hover(&mut self) {
        let Some((x, y)) = self.pointer else { return };
        let moved = self.tree.geometry_version != self.seen_geometry_version;
        if moved || self.tree.drawn.iter().any(|r| r.contains(x, y)) {
            self.seen_geometry_version = self.tree.geometry_version;
            self.set_hover(x, y);
        }
    }

    pub fn input(&mut self, ev: Input) {
        if let Some(tr) = &mut self.trace {
            tr.input();
        }
        self.forget_removed();
        match ev {
            Input::Move(x, y) => {
                self.pointer = Some((x, y));
                #[cfg(any(debug_assertions, test))]
                crate::dev::Dev::pointed(self, x, y);
                if self.middle_down {
                    self.events.push(Event::MiddleMove { x, y });
                }
                match self.gesture {
                    Pointer::Selecting { field } => {
                        let at = self.edit_hit(field, x);
                        if let Some(e) = self.io.edits.get_mut(&field) {
                            if e.st.caret != at {
                                e.st.move_to(at, true);
                                self.edit_changed_caret(field);
                            }
                        }
                    }
                    Pointer::Pressed { id, x: px, y: py } if self.tree.el(id).drag_events() => {
                        if !within((x, y), (px, py), DRAG_SLOP) {
                            self.gesture = Pointer::Dragging { id };
                            self.events.push(Event::DragStart { id, x: px, y: py });
                            self.events.push(Event::Drag { id, x, y });
                        }
                    }
                    Pointer::Dragging { id } => self.events.push(Event::Drag { id, x, y }),
                    _ => {}
                }
                self.set_hover(x, y);
            }
            // A held button keeps the pointer (the host has the capture).
            Input::Leave if self.gesture != Pointer::Idle => {}
            Input::Leave => {
                self.pointer = None;
                self.set_hover(-100000, -100000);
            }
            Input::Down(x, y, button) => {
                self.pointer = Some((x, y));
                self.set_hover(x, y);
                let t = self.target.filter(|&t| !self.tree.el(t).disabled());
                self.dismissing = false;
                if !self.popups.is_empty() {
                    let inside =
                        self.popups.iter().any(|p| self.tree.alive(p.id) && self.tree.rect(p.id).contains(x, y));
                    if !inside {
                        self.events.push(Event::PressOutside { x, y });
                        let under = |a: Id| t.is_some_and(|t| self.tree.is_descendant(t, a));
                        self.dismissing = self.popups.iter().any(|p| p.anchor.is_some_and(under));
                    }
                }
                // Focus follows presses: a text field takes it, anything else drops it; either
                // way the focus visual goes.
                let edit = t.and_then(|t| self.edit_of(t));
                let gained = edit != self.focus;
                self.focus_by_press(edit);
                if button == MouseButton::Left {
                    self.end_gesture();
                    if let Some(t) = t {
                        self.set_pressed(t, true);
                        self.gesture = Pointer::Pressed { id: t, x, y };
                        if self.tree.el(t).drag_events() {
                            self.events.push(Event::Press { id: t, x, y });
                        }
                    }
                    // An embedded clear/action button keeps the editor's focus but is not a
                    // press into its text and must not start a selection drag.
                    // A field that selects on focus keeps the selection the press gave it.
                    let selects =
                        gained && edit.is_some_and(|f| self.io.edits.get(&f).is_some_and(|e| e.select_on_focus));
                    if let Some(f) = edit.filter(|f| Some(*f) == t && !selects) {
                        let at = self.edit_hit(f, x);
                        let extend = self.press_mods.shift;
                        if let Some(e) = self.io.edits.get_mut(&f) {
                            e.st.move_to(at, extend);
                        }
                        self.gesture = Pointer::Selecting { field: f };
                        self.edit_changed_caret(f);
                    }
                } else if button == MouseButton::Middle {
                    self.middle_down = true;
                    if let Some(t) = self.tree.hit(x, y, |e| e.wheel_events()) {
                        self.events.push(Event::MiddleDown { id: t, x, y });
                    }
                } else if button == MouseButton::Right && matches!(self.gesture, Pointer::Dragging { .. }) {
                    // WinUI ListView reorder: a right press during a drag calls it off.
                    self.cancel_drag();
                } else if button == MouseButton::Right {
                    let t = self.tree.hit(x, y, |e| e.takes_pointer());
                    // A right press that dismissed a popup on its anchor is that press's whole
                    // effect too (the context menu it opened goes, not again).
                    if let Some(t) = t.filter(|_| !std::mem::take(&mut self.dismissing)) {
                        self.events.push(Event::RightClick { id: t, x, y });
                    }
                }
            }
            Input::Up(x, y, button) => {
                if button == MouseButton::Middle && std::mem::take(&mut self.middle_down) {
                    self.events.push(Event::MiddleUp { x, y });
                }
                if button == MouseButton::Left {
                    let gesture = self.end_gesture();
                    if let Some(id) = gesture.captured() {
                        if self.ends_as_drag(gesture, id) {
                            self.events.push(Event::DragEnd { id, x, y });
                        } else if self.tree.rect(id).contains(x, y) && !std::mem::take(&mut self.dismissing) {
                            let action = self.tree.el(id).action().cloned();
                            self.events.push(Event::Click { id, action });
                        }
                    }
                    self.set_hover(x, y);
                }
            }
            Input::DoubleClick(x, y) => {
                self.set_hover(x, y);
                if let Some(t) = self.tree.hit(x, y, |e| e.takes_pointer()) {
                    let action = self.tree.el(t).action().cloned();
                    self.events.push(Event::DoubleClick { id: t, action });
                }
                // A double click is also a press (Windows sends DBLCLK in place of the 2nd DOWN).
                self.input(Input::Down(x, y, MouseButton::Left));
                // In a field it selects the word under the pointer; moves do not extend it.
                if let Pointer::Selecting { field } = self.gesture {
                    let at = self.edit_hit(field, x);
                    if let Some(e) = self.io.edits.get_mut(&field) {
                        let (a, b) = crate::widgets::text_field::word_at(&e.st.text, at);
                        e.st.anchor = a;
                        e.st.caret = b;
                    }
                    self.gesture = Pointer::Pressed { id: field, x, y };
                    self.edit_changed_caret(field);
                }
            }
            Input::Wheel(x, y, notches) => {
                if let Some(t) = self.tree.hit(x, y, |e| e.wheel_events()) {
                    // 1 notch = 3 lines ≈ 48 dips (WinUI ScrollViewer's mouse wheel delta).
                    self.events.push(Event::Wheel { id: t, dy: notches * 48.0 });
                }
            }
            Input::Key(key::ESCAPE, _) if matches!(self.gesture, Pointer::Dragging { .. }) => self.cancel_drag(),
            #[cfg(any(debug_assertions, test))]
            Input::Key(crate::dev::INSPECT_KEY, _) => crate::dev::Dev::toggle(self),
            Input::Mods(m) => self.press_mods = m,
            Input::Key(vk, mods) => {
                if let Some(f) = self.focus {
                    if self.edit_key(f, vk, mods) {
                        return;
                    }
                }
                if self.focus_key(vk, mods) {
                    return;
                }
                self.events.push(Event::Key { vk, mods });
            }
            Input::Char(c) => {
                if let Some(f) = self.focus {
                    self.edit_char(f, c);
                }
            }
            Input::Files(p) => self.events.push(Event::Files(p)),
            Input::Preedit(t, c) => {
                if let Some(f) = self.focus {
                    if let Some(e) = self.io.edits.get_mut(&f) {
                        if !t.is_empty() && e.st.has_sel() {
                            // Composition replaces the selection, as typing does.
                            e.st.insert("");
                        }
                        e.st.preedit = t;
                        e.st.preedit_caret = c;
                    }
                    self.edit_changed_caret(f);
                }
            }
            Input::Commit(t) => {
                if let Some(f) = self.focus {
                    if let Some(e) = self.io.edits.get_mut(&f) {
                        e.st.preedit.clear();
                    }
                    self.edit_insert(f, &t);
                }
            }
            Input::CaptureLost => self.cancel_drag(),
            Input::Focus(on) => {
                if !on {
                    self.cancel_drag();
                }
                self.window.focused = on;
                self.events.push(Event::WindowFocus(on));
            }
        }
    }

    /// Ends the gesture in progress without a drop: a `drag-events` element hears `DragCancel`.
    pub fn cancel_drag(&mut self) {
        self.forget_removed();
        let gesture = self.end_gesture();
        if let Some(id) = gesture.captured() {
            if self.ends_as_drag(gesture, id) {
                self.events.push(Event::DragCancel { id });
            }
        }
        if let Some((x, y)) = self.pointer {
            self.set_hover(x, y);
        }
    }

    pub fn is_dragging(&self) -> bool {
        matches!(self.gesture, Pointer::Dragging { .. })
    }

    /// What the left button is doing.
    pub fn gesture(&self) -> Pointer {
        self.gesture
    }

    /// After `Tree::remove`: state that named a removed element goes with it (the remove
    /// cascade's `Ui` half). A gesture on it ends with `DragCancel` (a drag never commits against
    /// a replacement generation); focus, edit state, hover and popup entries go silently (the
    /// app removed the element: there is no field left to blur).
    /// Runs on every input and frame; nothing to do while nothing was removed.
    pub fn forget_removed(&mut self) {
        if self.seen_removals == self.tree.removal_version {
            return;
        }
        self.seen_removals = self.tree.removal_version;
        let tree = &self.tree;
        self.hovered.retain(|&h| tree.alive(h));
        self.popups.retain(|p| tree.alive(p.id));
        self.io.edits.retain(|&id, _| tree.alive(id));
        if self.target.is_some_and(|t| !tree.alive(t)) {
            self.target = None;
        }
        if let Some(id) = self.gesture.captured().filter(|&id| !tree.alive(id)) {
            self.gesture = Pointer::Idle;
            self.events.push(Event::DragCancel { id });
        }
        if self.focus.is_some_and(|f| !tree.alive(f)) {
            self.focus = None;
            self.io.ime_at = None;
        }
    }
}

/// Point `a` lies within `slop` px of `b` on both axes (a press that has not moved, a second
/// click at the first's place).
pub(crate) fn within(a: (i32, i32), b: (i32, i32), slop: i32) -> bool {
    (a.0 - b.0).abs() <= slop && (a.1 - b.1).abs() <= slop
}
