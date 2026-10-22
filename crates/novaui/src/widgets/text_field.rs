//! Single-line text fields (WinUI TextBox, single line): caret, selection, clipboard, word
//! navigation, mouse selection, inline IME composition.
//!
//! A field is an element made editable by `Ui::make_edit`; the field owns three children: the
//! selection plate, the text, and the caret. IME: the host turns off the IME's own composition
//! window (WM_IME_SETCONTEXT without ISC_SHOWUICOMPOSITIONWINDOW) and forwards the composition
//! string; the field draws it inline at the caret with a dotted underline, and the candidate
//! window is placed under it (`Ui::ime_at`). This is what WinUI's TextBox does with TSF.
//!
//! Behaviour follows the Windows edit control:
//! - Left/Right move by a grapheme cluster (what the user sees as one character: a letter with
//!   its marks, a conjunct), Ctrl by a word; Home/End; Shift extends the selection;
//! - Backspace/Delete remove the selection, else Backspace the char before the caret (so a mark
//!   comes off its letter) and Delete the cluster after it (Ctrl: a word);
//! - the caret stands at a cluster's leading edge in its direction (`Text::carets`); a selection
//!   across both directions is shown as one span from its leftmost edge to its rightmost;
//! - Ctrl+A selects all, Ctrl+C/X/V copy, cut, paste (the host's clipboard);
//! - a press places the caret, a drag selects, a double click selects a word;
//! - typing replaces the selection; Escape gives up the focus.

/// Char-index range helpers over a `String` (fields are short: linear scans are fine).
pub fn byte_at(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map_or(s.len(), |(i, _)| i)
}

pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// The char indices where a grapheme cluster (what the user sees as one character: a letter and
/// its marks, a conjunct, an emoji sequence) starts, and the text's end: where a caret may stand.
pub fn grapheme_stops(s: &str) -> Vec<usize> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut out = Vec::new();
    let mut ci = 0;
    for g in s.graphemes(true) {
        out.push(ci);
        ci += g.chars().count();
    }
    out.push(ci);
    out
}

/// The caret one grapheme cluster left of `at` (Left, Shift+Left).
pub fn grapheme_left(s: &str, at: usize) -> usize {
    grapheme_stops(s).into_iter().rev().find(|&c| c < at).unwrap_or(0)
}

/// The caret one grapheme cluster right of `at` (Right, Delete).
pub fn grapheme_right(s: &str, at: usize) -> usize {
    let stops = grapheme_stops(s);
    let end = stops.last().copied().unwrap_or(0);
    stops.into_iter().find(|&c| c > at).unwrap_or(end)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Class for word motion: CJK ideographs are each a word (as in the Windows edit control).
fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if crate::text::is_cjk_char(c) {
        3
    } else if is_word(c) {
        1
    } else {
        2
    }
}

/// The caret after Ctrl+Left from `at`.
pub fn word_left(s: &str, at: usize) -> usize {
    let cs: Vec<char> = s.chars().collect();
    let mut i = at.min(cs.len());
    while i > 0 && class(cs[i - 1]) == 0 {
        i -= 1;
    }
    if i == 0 {
        return 0;
    }
    let k = class(cs[i - 1]);
    if k == 3 {
        return i - 1;
    }
    while i > 0 && class(cs[i - 1]) == k {
        i -= 1;
    }
    i
}

/// The caret after Ctrl+Right from `at`.
pub fn word_right(s: &str, at: usize) -> usize {
    let cs: Vec<char> = s.chars().collect();
    let n = cs.len();
    let mut i = at.min(n);
    if i < n {
        let k = class(cs[i]);
        if k == 3 {
            i += 1;
        } else if k != 0 {
            while i < n && class(cs[i]) == k {
                i += 1;
            }
        }
    }
    while i < n && class(cs[i]) == 0 {
        i += 1;
    }
    i
}

/// The word around `at` (double click).
pub fn word_at(s: &str, at: usize) -> (usize, usize) {
    let cs: Vec<char> = s.chars().collect();
    let n = cs.len();
    if n == 0 {
        return (0, 0);
    }
    let at = at.min(n - 1);
    let k = class(cs[at]);
    if k == 3 {
        return (at, at + 1);
    }
    let (mut a, mut b) = (at, at + 1);
    while a > 0 && class(cs[a - 1]) == k {
        a -= 1;
    }
    while b < n && class(cs[b]) == k {
        b += 1;
    }
    (a, b)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditState {
    pub text: String,
    /// Caret, in chars.
    pub caret: usize,
    /// The selection's other end; == caret when nothing is selected.
    pub anchor: usize,
    /// The IME's composition, shown at the caret, not yet part of `text`.
    pub preedit: String,
    /// The IME's cursor inside the composition (chars).
    pub preedit_caret: usize,
}

impl EditState {
    pub fn sel(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    pub fn has_sel(&self) -> bool {
        self.caret != self.anchor
    }

    pub fn selected(&self) -> &str {
        let (a, b) = self.sel();
        &self.text[byte_at(&self.text, a)..byte_at(&self.text, b)]
    }

    pub fn move_to(&mut self, at: usize, extend: bool) {
        self.caret = at.min(char_len(&self.text));
        if !extend {
            self.anchor = self.caret;
        }
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = char_len(&self.text);
    }

    /// Replaces the selection (or inserts at the caret) with `s`.
    pub fn insert(&mut self, s: &str) {
        let (a, b) = self.sel();
        let (ba, bb) = (byte_at(&self.text, a), byte_at(&self.text, b));
        self.text.replace_range(ba..bb, s);
        self.caret = a + char_len(s);
        self.anchor = self.caret;
    }

    /// Deletes the selection; else the range from the caret to `to`.
    pub fn delete_to(&mut self, to: usize) {
        if !self.has_sel() {
            self.anchor = to.min(char_len(&self.text));
        }
        self.insert("");
    }

    /// The text as shown: the composition spliced in at the caret.
    pub fn shown(&self) -> String {
        if self.preedit.is_empty() {
            return self.text.clone();
        }
        let at = byte_at(&self.text, self.caret);
        let mut s = String::with_capacity(self.text.len() + self.preedit.len());
        s.push_str(&self.text[..at]);
        s.push_str(&self.preedit);
        s.push_str(&self.text[at..]);
        s
    }
}

// ---- the text field on `Ui` ---------------------------------------------------------------

use crate::app::{Event, Ui};
use crate::element::Id;
use crate::geom::*;
use crate::input::Mods;
use crate::style::*;

const BLINK: f64 = 0.53;

pub struct TextEdit {
    pub id: Id,
    pub st: crate::widgets::text_field::EditState,
    pub hint: String,
    pub caret_el: Id,
    pub text_el: Id,
    pub sel_el: Id,
    pub ime_el: Id,
    /// A secret (password) field: bullets are shown, nothing is copied, a word is not a unit.
    pub secret: bool,
    /// Focus selects the whole text, a press too (WinUI's editable ComboBox): typing replaces it.
    pub select_on_focus: bool,
    /// What the field's events name (`on-edit`): `Edit`, `Submit`, `Blur` and `Cancel` carry it,
    /// so the app routes a field as it routes a click, not by comparing ids. A field with one
    /// owns Esc (`Event::Cancel`); without one, Esc leaves the field and goes on as a key.
    pub action: Option<crate::app::Action>,
    pub blink_on: bool,
    /// Horizontal scroll (dips) keeping the caret in view.
    pub scroll: f32,
    pub(crate) fg: Color,
    pub(crate) hint_fg: Color,
}

impl TextEdit {
    /// What the field shows for `text`: bullets for a secret, else the text.
    fn as_shown(&self, text: String) -> String {
        if self.secret {
            mask(&text)
        } else {
            text
        }
    }
}

impl Ui {
    // ---- text fields ----------------------------------------------------------------------

    pub(crate) fn edit_of(&self, id: Id) -> Option<Id> {
        if self.io.edits.contains_key(&id) {
            return Some(id);
        }
        self.tree.ancestors(id).find(|a| self.io.edits.contains_key(a))
    }

    /// Turns `id` (a Row element) into a single-line text field with `hint`: its text set as
    /// `style` in `fg`, the hint in `hint_fg`.
    pub fn make_edit(&mut self, id: Id, style: TextContent, fg: Color, hint_fg: Color, hint: &str) {
        let sel_bg = crate::theme::color("accent").unwrap_or([0.38, 0.8, 1.0, 1.0]);
        let abs = |l: Layout| Layout { position: Position::Absolute, ..l };
        // Children: the selection plate (under the text), the text, the composition's
        // underline, the caret. The field clips them; the text is absolutely placed so it can
        // scroll under the caret.
        let sel_el = self.tree.add(
            id,
            abs(Layout {
                width: Len::Px(0.0),
                height: Len::Px(style.line),
                inset: [Len::Auto, Len::Auto, Len::Auto, Len::Px(0.0)],
                ..Default::default()
            }),
            Visual { bg: Some(sel_bg), visible: false, ..Default::default() },
            Content::None,
        );
        let text_el = self.tree.add(
            id,
            Layout { grow: 1.0, shrink: 1.0, min_width: Len::Px(0.0), ..Default::default() },
            Visual { fg: hint_fg, ..Default::default() },
            Content::Text(TextContent { text: hint.to_string(), ..style }),
        );
        let ime_el = self.tree.add(
            id,
            abs(Layout {
                width: Len::Px(0.0),
                height: Len::Px(1.0),
                inset: [Len::Auto, Len::Auto, Len::Auto, Len::Px(0.0)],
                ..Default::default()
            }),
            Visual { bg: Some(fg), visible: false, ..Default::default() },
            Content::None,
        );
        let caret_el = self.tree.add(
            id,
            abs(Layout {
                width: Len::Px(1.0),
                height: Len::Px(style.line),
                inset: [Len::Auto, Len::Auto, Len::Auto, Len::Px(0.0)],
                ..Default::default()
            }),
            Visual { bg: Some(fg), visible: false, ..Default::default() },
            Content::None,
        );
        self.tree.set_visual(id, |v| {
            v.cursor = Cursor::Text;
            v.hit = true;
            v.clip = true;
        });
        self.tree.set_editable(id);
        self.io.edits.insert(
            id,
            TextEdit {
                id,
                st: Default::default(),
                hint: hint.to_string(),
                caret_el,
                text_el,
                sel_el,
                ime_el,
                secret: false,
                select_on_focus: false,
                action: None,
                blink_on: false,
                scroll: 0.0,
                fg,
                hint_fg,
            },
        );
        self.sync_edit(id);
    }

    /// Makes the field a secret one: its text is shown as bullets and never reaches the clipboard.
    pub fn set_edit_secret(&mut self, id: Id, secret: bool) {
        if let Some(e) = self.io.edits.get_mut(&id) {
            e.secret = secret;
            self.sync_edit(id);
        }
    }

    /// Focus selects the field's whole text (`TextEdit::select_on_focus`).
    pub fn set_edit_select_on_focus(&mut self, id: Id, on: bool) {
        if let Some(e) = self.io.edits.get_mut(&id) {
            e.select_on_focus = on;
        }
    }

    /// The action the field's events carry (`TextEdit::action`, `.nui` `on-edit`).
    pub fn set_edit_action(&mut self, id: Id, action: Option<&str>) {
        if let Some(e) = self.io.edits.get_mut(&id) {
            e.action = action.map(crate::app::Action::parse);
        }
    }

    pub fn edit_text(&self, id: Id) -> &str {
        self.io.edits.get(&id).map_or("", |e| e.st.text.as_str())
    }

    pub fn set_edit_text(&mut self, id: Id, s: &str) {
        if let Some(e) = self.io.edits.get_mut(&id) {
            if e.st.text != s {
                e.st.text = s.to_string();
                let n = crate::widgets::text_field::char_len(s);
                e.st.caret = n;
                e.st.anchor = n;
                self.sync_edit(id);
            }
        }
    }

    /// A text field has the focus: the host lets the IME compose (else it is disabled).
    pub(crate) fn ime_on(&self) -> bool {
        self.focus.is_some_and(|f| self.io.edits.contains_key(&f))
    }

    /// Field `old` loses the focus: its composition goes, its selection collapses, `Blur`.
    pub(crate) fn edit_blur(&mut self, old: Id) {
        if let Some(e) = self.io.edits.get_mut(&old) {
            e.st.preedit.clear();
            e.st.anchor = e.st.caret;
            let action = e.action.clone();
            self.events.push(Event::Blur { id: old, action });
            self.sync_edit(old);
        }
    }

    /// Field `n` takes the focus by keyboard or code: the caret at the end (a press moves it
    /// after), or the whole text selected.
    pub(crate) fn edit_focus(&mut self, n: Id) {
        if let Some(e) = self.io.edits.get_mut(&n) {
            let len = crate::widgets::text_field::char_len(&e.st.text);
            e.st.caret = len;
            e.st.anchor = if e.select_on_focus { 0 } else { len };
            self.edit_changed_caret(n);
        }
    }

    /// A text field's text style (size, line height, weight, ...; dips): its text — the hint
    /// too — selection and caret follow. What it shows stays the field's own.
    pub fn set_edit_style(&mut self, id: Id, f: impl FnOnce(&mut TextContent)) {
        let Some(e) = self.io.edits.get(&id) else { return };
        let (text_el, sel_el, caret_el) = (e.text_el, e.sel_el, e.caret_el);
        if let Content::Text(mut t) = self.tree.el(text_el).content().clone() {
            let shown = std::mem::take(&mut t.text);
            f(&mut t);
            t.text = shown;
            for el in [sel_el, caret_el] {
                self.tree.set_layout(el, |x| x.height = Len::Px(t.line));
            }
            self.tree.set_content(text_el, Content::Text(t));
            self.sync_edit(id);
        }
    }

    /// The text a field shows while empty.
    pub fn set_edit_hint(&mut self, id: Id, hint: &str) {
        if let Some(e) = self.io.edits.get_mut(&id) {
            if e.hint != hint {
                e.hint = hint.to_string();
                self.sync_edit(id);
            }
        }
    }

    pub(crate) fn edit_style(&self, e: &TextEdit) -> Option<crate::text::TextStyle> {
        match &self.tree.el(e.text_el).content() {
            Content::Text(t) => {
                let s = self.tree.scale();
                Some(t.style(s))
            }
            _ => None,
        }
    }

    /// The caret position (a char index at a grapheme cluster's edge) nearest the pointer at
    /// client x `x` (physical px).
    pub(crate) fn edit_hit(&mut self, id: Id, x: i32) -> usize {
        let Some(e) = self.io.edits.get(&id) else { return 0 };
        let Some(st) = self.edit_style(e) else { return 0 };
        let s = self.tree.scale();
        let r = self.tree.rect(id);
        let pad = self.tree.el(id).layout().padding[crate::style::side::LEFT] * s;
        let local = (x - r.x) as f32 - pad + e.scroll * s;
        let text = e.as_shown(e.st.text.clone());
        let carets = self.tree.text_mut().carets(&text, &st);
        let at = |i: usize| carets.get(i).copied().unwrap_or(0.0);
        grapheme_stops(&text)
            .into_iter()
            .min_by(|&a, &b| (at(a) - local).abs().total_cmp(&(at(b) - local).abs()))
            .unwrap_or(0)
    }

    pub(crate) fn sync_edit(&mut self, id: Id) {
        let Some(e) = self.io.edits.get(&id) else { return };
        let Some(style) = self.edit_style(e) else { return };
        let focused = self.focus == Some(id);
        let s = self.tree.scale();
        let (text_el, caret_el, sel_el, ime_el) = (e.text_el, e.caret_el, e.sel_el, e.ime_el);
        let (fg, hint_fg) = (e.fg, e.hint_fg);
        let shown = e.as_shown(e.st.shown());
        let empty = shown.is_empty();
        let label = if empty { e.hint.clone() } else { shown.clone() };
        let (caret, anchor, preedit_caret) = (e.st.caret, e.st.anchor, e.st.preedit_caret);
        let pre = crate::widgets::text_field::char_len(&e.st.preedit);
        // Caret positions over what is shown (text with the composition spliced in), in dips.
        let carets: Vec<f32> = self.tree.text_mut().carets(&shown, &style).into_iter().map(|v| v / s).collect();
        let at = |i: usize| carets.get(i).copied().unwrap_or(0.0);
        // A range's extent on the line: from its leftmost caret to its rightmost (one span, also
        // where mixed directions make the range two pieces on screen).
        let span = |a: usize, b: usize| {
            (a..=b).map(at).fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), x| (lo.min(x), hi.max(x)))
        };
        let caret_ci = caret + if pre > 0 { preedit_caret.min(pre) } else { 0 };
        let caret_x = at(caret_ci);
        let (sa, sb) = (caret.min(anchor), caret.max(anchor));
        let sel = (sa != sb && pre == 0).then(|| span(sa, sb));
        let ime = (pre > 0).then(|| span(caret, caret + pre));
        let Some(e) = self.io.edits.get(&id) else { return };
        let blink = e.blink_on;
        let lay = &self.tree.el(id).layout();
        let (pad_l, pad_r) = (lay.padding[crate::style::side::LEFT], lay.padding[crate::style::side::RIGHT]);
        let (w, h) = self.tree.size(id);
        let room = (w - pad_l - pad_r).max(1.0);
        // Keep the caret in view: scroll only as far as needed (WinUI TextBox).
        let full = carets.iter().copied().fold(0.0, f32::max);
        let mut scroll = e.scroll;
        if caret_x - scroll > room - 1.0 {
            scroll = caret_x - room + 1.0;
        }
        if caret_x - scroll < 0.0 {
            scroll = caret_x;
        }
        scroll = scroll.clamp(0.0, (full - room + 1.0).max(0.0));
        if !focused {
            scroll = 0.0;
        }
        if let Some(e) = self.io.edits.get_mut(&id) {
            e.scroll = scroll;
        }
        let line_y = ((h - style.line / s) * 0.5).max(0.0);
        // Where text x `at` (dips from the text's start) is in the field: the left padding less
        // the scroll.
        let in_field = |at: f32| pad_l + at - scroll;
        self.tree.set_text(text_el, &label);
        self.tree.set_visual(text_el, |v| v.fg = if empty { hint_fg } else { fg });
        self.tree.set_layout(text_el, |l| {
            // The text runs unclipped to its own width while focused (it scrolls); at rest it
            // takes the field and ellipsizes.
            if focused {
                l.position = Position::Absolute;
                l.inset = [Len::Px(line_y), Len::Auto, Len::Auto, Len::Px(in_field(0.0))];
                l.width = Len::Px(full.max(room) + 2.0);
                l.grow = 0.0;
            } else {
                l.position = Position::Relative;
                l.inset = [Len::Auto; 4];
                l.width = Len::Auto;
                l.grow = 1.0;
            }
        });
        self.tree
            .set_layout(caret_el, |l| l.inset = [Len::Px(line_y), Len::Auto, Len::Auto, Len::Px(in_field(caret_x))]);
        self.tree.set_visual(caret_el, |v| v.visible = focused && blink && sel.is_none());
        match sel.filter(|_| focused) {
            Some((a, b)) => {
                self.tree.set_layout(sel_el, |l| {
                    l.inset = [Len::Px(line_y), Len::Auto, Len::Auto, Len::Px(in_field(a))];
                    l.width = Len::Px(b - a);
                });
                self.tree.set_visual(sel_el, |v| v.visible = true);
                // Selected text reads on the accent in the text-on-accent colour; one colour per
                // run is all the text element has, so the plate is translucent instead.
                self.tree.set_visual(sel_el, |v| v.opacity = 0.45);
            }
            None => self.tree.set_visual(sel_el, |v| v.visible = false),
        }
        match ime.filter(|_| focused) {
            Some((a, b)) => {
                let base = line_y + style.line / s - 2.0;
                self.tree.set_layout(ime_el, |l| {
                    l.inset = [Len::Px(base), Len::Auto, Len::Auto, Len::Px(in_field(a))];
                    l.width = Len::Px(b - a);
                });
                self.tree.set_visual(ime_el, |v| v.visible = true);
            }
            None => self.tree.set_visual(ime_el, |v| v.visible = false),
        }
        if focused {
            let r = self.tree.rect(id);
            let x = r.x + (in_field(caret_x) * s) as i32;
            let y = r.y + (line_y * s) as i32;
            self.io.ime_at = Some((x, y, (style.line).round() as i32));
        } else if !self.ime_on() {
            self.io.ime_at = None;
        }
    }

    /// When the focused field's caret blinks next: a deadline of the `Ui` (in `next_wake`), served
    /// by `tick_edits` at the start of the turn.
    pub(crate) fn next_blink(&self) -> Option<f64> {
        self.focus.filter(|f| self.io.edits.contains_key(f)).map(|_| self.last_blink + BLINK)
    }

    /// Caret blink; run at the start of every turn (`runner::deliver`).
    pub(crate) fn tick_edits(&mut self, now: f64) {
        self.now = now;
        self.forget_removed();
        if let Some(f) = self.focus {
            if now - self.last_blink >= BLINK {
                self.last_blink = now;
                if let Some(e) = self.io.edits.get_mut(&f) {
                    e.blink_on = !e.blink_on;
                }
                self.sync_edit(f);
            }
        }
    }

    pub(crate) fn edit_changed(&mut self, id: Id) {
        self.edit_changed_caret(id);
        let e = &self.io.edits[&id];
        let (text, action) = (e.st.text.clone(), e.action.clone());
        self.events.push(Event::Edit { id, action, text });
    }

    pub(crate) fn edit_changed_caret(&mut self, id: Id) {
        self.last_blink = self.now;
        if let Some(e) = self.io.edits.get_mut(&id) {
            e.blink_on = true;
        }
        self.sync_edit(id);
    }

    pub(crate) fn edit_char(&mut self, id: Id, c: char) {
        if c.is_control() {
            return;
        }
        let Some(e) = self.io.edits.get_mut(&id) else { return };
        let mut b = [0u8; 4];
        e.st.insert(c.encode_utf8(&mut b));
        self.edit_changed(id);
    }

    /// Inserts `s` into the focused field (the host's answer to `paste_wanted`, or a drop).
    pub(crate) fn paste(&mut self, s: &str) {
        let Some(f) = self.focus else { return };
        if !self.io.edits.contains_key(&f) {
            return;
        }
        self.edit_insert(f, s);
    }

    /// Text from outside the keyboard (a paste, an input method's commit) goes into field `f`
    /// at its caret, without control characters.
    /// Nothing, when `f` is not a field (an app may focus any element).
    pub(crate) fn edit_insert(&mut self, f: Id, s: &str) {
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        let Some(e) = self.io.edits.get_mut(&f) else { return };
        e.st.insert(&clean);
        self.edit_changed(f);
    }

    /// Returns true when the key was the field's.
    pub(crate) fn edit_key(&mut self, id: Id, vk: u32, mods: Mods) -> bool {
        use crate::input::key::{self, BACKSPACE, DELETE, END, ENTER, ESCAPE, HOME, LEFT, RIGHT};
        use crate::widgets::text_field::{char_len, grapheme_left, grapheme_right, word_left, word_right};
        let Some(e) = self.io.edits.get_mut(&id) else { return false };
        // While the IME composes, its keys are its own.
        if !e.st.preedit.is_empty() {
            return true;
        }
        let secret = e.secret;
        let st = &mut e.st;
        let n = char_len(&st.text);
        let ext = mods.shift;
        match vk {
            BACKSPACE => {
                if st.has_sel() {
                    st.insert("");
                } else if st.caret > 0 {
                    let to = if mods.ctrl && !secret { word_left(&st.text, st.caret) } else { st.caret - 1 };
                    st.delete_to(to);
                } else {
                    return true;
                }
                self.edit_changed(id);
            }
            DELETE => {
                if st.has_sel() {
                    st.insert("");
                } else if st.caret < n {
                    let to = if mods.ctrl && !secret {
                        word_right(&st.text, st.caret)
                    } else {
                        grapheme_right(&st.text, st.caret)
                    };
                    st.delete_to(to);
                } else {
                    return true;
                }
                self.edit_changed(id);
            }
            LEFT => {
                let to = if st.has_sel() && !ext && !mods.ctrl {
                    st.sel().0
                } else if mods.ctrl && !secret {
                    word_left(&st.text, st.caret)
                } else {
                    grapheme_left(&st.text, st.caret)
                };
                st.move_to(to, ext);
                self.edit_changed_caret(id);
            }
            RIGHT => {
                let to = if st.has_sel() && !ext && !mods.ctrl {
                    st.sel().1
                } else if mods.ctrl && !secret {
                    word_right(&st.text, st.caret)
                } else {
                    grapheme_right(&st.text, st.caret)
                };
                st.move_to(to, ext);
                self.edit_changed_caret(id);
            }
            HOME => {
                st.move_to(0, ext);
                self.edit_changed_caret(id);
            }
            END => {
                st.move_to(n, ext);
                self.edit_changed_caret(id);
            }
            ENTER => {
                let text = st.text.clone();
                let action = e.action.clone();
                self.events.push(Event::Submit { id, action, text });
            }
            ESCAPE => {
                let action = e.action.clone();
                self.set_focus(None);
                match action {
                    Some(action) => self.events.push(Event::Cancel { id, action }),
                    None => self.events.push(Event::Key { vk, mods }),
                }
            }
            k if k == key::letter('A') && mods.ctrl => {
                st.select_all();
                self.edit_changed_caret(id);
            }
            k if k == key::letter('C') && mods.ctrl => {
                if st.has_sel() && !secret {
                    self.io.clipboard_out = Some(st.selected().to_string());
                }
            }
            k if k == key::letter('X') && mods.ctrl => {
                if st.has_sel() {
                    if !secret {
                        self.io.clipboard_out = Some(st.selected().to_string());
                    }
                    st.insert("");
                    self.edit_changed(id);
                }
            }
            k if k == key::letter('V') && mods.ctrl => self.io.paste_wanted = true,
            _ => {
                // Printable keys arrive as WM_CHAR; swallow them here so shortcuts don't fire.
                // Up / Down / Page Up / Page Down are not a single line's: they go on (a list
                // under the field steps with them, as under WinUI's editable ComboBox).
                // (Windows' numbering: the keys from `)` to `Z`, the punctuation keys, the
                // number pad.)
                return !mods.ctrl && !mods.alt && (vk == key::SPACE || (0x29..=0x5A).contains(&vk))
                    || (0xBA..=0xE2).contains(&vk)
                    || (0x60..=0x6F).contains(&vk);
            }
        }
        true
    }
}

/// What a secret field shows for `s`: one bullet per char.
fn mask(s: &str) -> String {
    "\u{2022}".repeat(crate::widgets::text_field::char_len(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(t: &str, caret: usize, anchor: usize) -> EditState {
        EditState { text: t.into(), caret, anchor, ..Default::default() }
    }

    #[test]
    fn words_move_like_the_windows_edit_control() {
        let s = "hello  wide-world 你好";
        assert_eq!(word_right(s, 0), 7);
        assert_eq!(word_right(s, 7), 11);
        assert_eq!(word_right(s, 11), 12);
        assert_eq!(word_right(s, 12), 18);
        assert_eq!(word_right(s, 18), 19);
        assert_eq!(word_left(s, 20), 19);
        assert_eq!(word_left(s, 18), 12);
        assert_eq!(word_left(s, 7), 0);
        assert_eq!(word_at(s, 2), (0, 5));
        assert_eq!(word_at(s, 19), (19, 20));
    }

    #[test]
    fn typing_replaces_the_selection_and_delete_removes_it() {
        let mut e = st("abcdef", 4, 1);
        assert_eq!(e.selected(), "bcd");
        e.insert("X");
        assert_eq!((e.text.as_str(), e.caret, e.anchor), ("aXef", 2, 2));
        e.delete_to(1);
        assert_eq!((e.text.as_str(), e.caret), ("aef", 1));
        let mut e = st("ab中文cd", 4, 2);
        e.delete_to(0);
        assert_eq!(e.text, "abcd");
    }

    #[test]
    fn composition_shows_at_the_caret_without_entering_the_text() {
        let mut e = st("ab", 1, 1);
        e.preedit = "zhong".into();
        assert_eq!(e.shown(), "azhongb");
        assert_eq!(e.text, "ab");
    }
}
