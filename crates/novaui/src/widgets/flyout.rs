//! A light-dismiss flyout (WinUI's `MenuFlyout` / a ComboBox's list): one host element the app
//! fills with rows, opened against an anchor, placed after layout from its laid-out size, kept
//! inside the window (scrolling inside itself when it does not fit), moving in and out, with an
//! optional rail. The app owns what the rows are and what they do; this owns the rest.
//!
//! Turn order: `reset` (rows go), the app adds rows, `open` (the host shows, its popup is
//! registered with its anchor); after the layout barrier, `place` reads the rows' laid heights
//! and the anchor's rect, picks the side, the height, the scroll and the position, and starts the
//! motion. Every size it reads is the sheet's (the host's `width`, `max-height`, the rows').

use crate::anim::{Glide, GlideSkin, Pose, Then};
use crate::app::{Event, Ui};
use crate::element::Id;
use crate::element::Listen;
use crate::style::*;
use crate::widgets::scroll::{Parts, Range, Scrollbar, Skin};
use crate::Ease;

pub const IN: Ease = Ease::Bezier(0.1, 0.9, 0.2, 1.0);
pub const OUT: Ease = Ease::Bezier(0.7, 0.0, 1.0, 0.5);
pub const IN_S: f32 = 0.25;
pub const OUT_S: f32 = 0.15;
/// How far it slides (dips): from its anchor's side into place, and back when it leaves.
pub const RISE: f32 = 16.0;
/// The window edge it keeps clear of (dips).
const EDGE: f32 = 8.0;
/// The least height it shrinks to before it may cover its anchor's other side (dips).
const MIN_ROOM: f32 = 80.0;

/// Which side of its anchor it stands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Below,
    Above,
    Right,
    Left,
}

/// How it lines up with its anchor along that side: its start edge with the anchor's, centred
/// on it, or its end edge with the anchor's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Along {
    Start,
    Center,
    End,
}

/// What it is placed against: an element (its laid-out rect, read at `place`), or a rect in
/// dips (a pointer's position is a rect of no size).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Anchor {
    El(Id),
    Rect { x: f32, y: f32, w: f32, h: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub anchor: Anchor,
    pub side: Side,
    pub along: Along,
    /// Between the anchor and it (dips).
    pub gap: f32,
    /// Along the side, after aligning (dips).
    pub shift: f32,
    /// It may stand on the other side when that has more room for it.
    pub flip: bool,
}

impl Place {
    /// At a point (a context menu at the pointer): below and right of it.
    pub fn at(x: f32, y: f32) -> Self {
        Place {
            anchor: Anchor::Rect { x, y, w: 0.0, h: 0.0 },
            side: Side::Below,
            along: Along::Start,
            gap: 0.0,
            shift: 0.0,
            flip: true,
        }
    }

    /// Against an element, on `side`, aligned `along`, `gap` away, flipping when it lacks room.
    pub fn on(el: Id, side: Side, along: Along, gap: f32) -> Self {
        Place { anchor: Anchor::El(el), side, along, gap, shift: 0.0, flip: true }
    }

    pub fn shift(self, by: f32) -> Self {
        Place { shift: by, ..self }
    }

    pub fn fixed(self) -> Self {
        Place { flip: false, ..self }
    }
}

/// One flyout's state; its host element and rail are the caller's (passed in, as
/// `Scrollbar::sync` takes its parts), so it is made before the sheet is.
#[derive(Default)]
pub struct Flyout {
    rail: Scrollbar,
    /// Its rows' scroll (dips), gliding as every scroll does.
    scroll: Glide,
    /// How its scroll moves (`Glide`): WinUI's unless the app gives its own.
    pub glide: GlideSkin,
    /// Where it is placed after this turn's layout (`open` → `place`).
    pending: Option<Place>,
    /// It was not up when opened: `place` moves it in.
    entering: bool,
    /// The offset it moved in from, which its leaving reverses (dips).
    rise: f32,
    /// The window's top area it never covers (a title bar), in dips.
    pub top: f32,
    /// This opening's own entrance (`enter_as`), in place of the slide from its anchor.
    enter: Option<(Pose, f32, Ease)>,
}

impl Flyout {
    /// Its rows go (a flyout still leaving is dropped now), for a new opening to fill it. The
    /// app prunes its doc's names after.
    pub fn reset(&mut self, ui: &mut Ui, host: Id) -> Id {
        let id = host;
        ui.tree.clear_children(id);
        ui.tree.set_visual(id, |v| {
            v.hit = true;
            v.clip = false;
        });
        ui.tree.set_layout(id, |l| l.height = Len::Auto);
        ui.tree.set_scroll(id, 0.0, 0.0);
        self.scroll.jump(0.0);
        for l in [Listen::Hover, Listen::Wheel, Listen::Drag] {
            ui.tree.set_listen(id, l, false);
        }
        id
    }

    /// Shows it with the rows it now has, to be placed at `place` once laid out. `anchor`: the
    /// element whose press, while it is up, only dismisses it (`Popup`). Opening one that is up
    /// (rebuilt in place: a confirm pending, a rename) keeps it where it is, without motion.
    pub fn open(&mut self, ui: &mut Ui, host: Id, place: Place, anchor: Option<Id>) {
        let id = host;
        self.entering = !ui.tree.is_up(id);
        if !self.entering {
            ui.tree.settle(id);
        }
        ui.tree.set_layout(id, |l| {
            l.display = Display::Flex;
            l.position = Position::Absolute;
        });
        for c in ui.tree.children(id) {
            ui.tree.set_layout(c, |l| l.shrink = 0.0);
        }
        ui.add_popup(id, anchor);
        self.pending = Some(place);
        self.enter = None;
    }

    /// This opening moves in as `from` over `secs` on `ease` instead of sliding from its anchor
    /// (a drop target that fades in where it stands). Call after `open`.
    pub fn enter_as(&mut self, from: Pose, secs: f32, ease: Ease) {
        self.enter = Some((from, secs, ease));
    }

    /// After the layout barrier: places a flyout opened this turn. Returns whether it wrote
    /// layout (the frame lays it out again).
    pub fn place(&mut self, ui: &mut Ui, host: Id) -> bool {
        let Some(place) = self.pending.take() else { return false };
        let id = host;
        let s = ui.scale();
        let (win_w, win_h) = (ui.tree.window_size().0 as f32 / s, ui.tree.window_size().1 as f32 / s);
        let (ax, ay, aw, ah) = match place.anchor {
            Anchor::El(e) => {
                let r = ui.tree.rect(e);
                (r.x as f32 / s, r.y as f32 / s, r.w as f32 / s, r.h as f32 / s)
            }
            Anchor::Rect { x, y, w, h } => (x, y, w, h),
        };
        let w = (ui.tree.rect(id).w as f32 / s).min(win_w - 2.0 * EDGE);
        let content = content_h(ui, id);
        let max = match ui.tree.el(id).layout().max_height {
            Len::Px(m) => m,
            _ => f32::INFINITY,
        };
        // The room on each side: its height above or below, its width right or left.
        let room = |side: Side| match side {
            Side::Below => win_h - EDGE - (ay + ah + place.gap),
            Side::Above => ay - place.gap - self.top - EDGE,
            Side::Right => win_w - EDGE - (ax + aw + place.gap),
            Side::Left => ax - place.gap - EDGE,
        };
        let (other, need) = match place.side {
            Side::Below => (Side::Above, content.min(max)),
            Side::Above => (Side::Below, content.min(max)),
            Side::Right => (Side::Left, w),
            Side::Left => (Side::Right, w),
        };
        let mut side = place.side;
        if place.flip && need > room(side) && room(other) > room(side) {
            side = other;
        }
        let vertical = matches!(side, Side::Below | Side::Above);
        let y_room = if vertical { room(side) } else { win_h - EDGE - (ay + place.shift).max(self.top) };
        let height = content.min(max).min(y_room.max(MIN_ROOM));
        let scrolls = content > height + 0.5;
        let (x, y) = if vertical {
            let x = match place.along {
                Along::Start => ax,
                Along::Center => ax + aw / 2.0 - w / 2.0,
                Along::End => ax + aw - w,
            } + place.shift;
            (x, if side == Side::Below { ay + ah + place.gap } else { ay - place.gap - height })
        } else {
            (if side == Side::Right { ax + aw + place.gap } else { ax - place.gap - w }, ay + place.shift)
        };
        let x = x.min(win_w - w - EDGE).max(EDGE);
        let y = y.min(win_h - height - EDGE).max(self.top.min(y));
        ui.tree.set_layout(id, |l| {
            l.height = if scrolls { Len::Px(height) } else { Len::Auto };
            l.inset = [Len::Px(y), Len::Auto, Len::Auto, Len::Px(x)];
        });
        ui.tree.set_visual(id, |v| v.clip = scrolls);
        ui.tree.set_listen(id, Listen::Wheel, scrolls);
        self.rise = if side == Side::Above { RISE } else { -RISE };
        let (from, secs, ease) = self.enter.take().unwrap_or((Pose::clear_y(self.rise), IN_S, IN));
        if std::mem::take(&mut self.entering) {
            ui.tree.enter(id, from, secs, ease, ui.now);
        }
        true
    }

    /// It leaves: no longer a popup, not pressable; it slides back and fades, and the tree hides
    /// it once the motion is done. Returns when that is (the owner's clock looks then).
    pub fn close(&mut self, ui: &mut Ui, host: Id) -> Option<f64> {
        let id = host;
        self.pending = None;
        ui.remove_popup(id);
        if ui.focus.is_some_and(|f| ui.tree.is_descendant(f, id)) {
            ui.set_focus(None);
        }
        // Hidden, on its way out, or gone with its page: nothing to wait for.
        if !ui.tree.is_up(id) {
            return None;
        }
        let now = ui.now;
        ui.tree.set_visual(id, |v| v.hit = false);
        // A close while it still comes in turns it back from where it stands (`Tree::animate`).
        let end = ui.tree.exit(id, Pose::clear_y(self.rise), OUT_S, OUT, now, Then::Hide);
        for c in ui.tree.children(id) {
            ui.tree.set_state(c, crate::element::State::Disabled, true);
        }
        Some(end)
    }

    /// Placed this turn and not yet laid out there (`place` wrote its position).
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    /// A wheel over it: a notch moves 48 dips, as everywhere, and the rows glide there
    /// (`sync_rail` moves them).
    pub fn wheel(&mut self, ui: &mut Ui, host: Id, dy: f32) {
        let reach = reach(ui, host);
        self.scroll.wheel(dy, ui.now, 0.0, reach, self.glide);
    }

    /// Scrolls so `row` stands inside it (the row the keys are on: a glide); `center`: in its
    /// middle at once (the chosen row of a list just opened).
    pub fn reveal(&mut self, ui: &mut Ui, host: Id, row: Id, center: bool) {
        let id = host;
        let s = ui.scale();
        let (host, r) = (ui.tree.rect(id), ui.tree.rect(row));
        let (_, sy) = ui.tree.el(id).scroll();
        let pad = ui.tree.el(id).layout().padding;
        let (top, bottom) = ((r.y - host.y) as f32 / s - pad[0], (r.b() - host.b()) as f32 / s + pad[2]);
        let view = host.h as f32 / s;
        let to = if center {
            sy + top + pad[0] + (r.h as f32 / s - view) / 2.0
        } else if top < 0.0 {
            sy + top
        } else if bottom > 0.0 {
            sy + bottom
        } else {
            return;
        };
        let to = to.clamp(0.0, reach(ui, id));
        if center {
            self.scroll.jump(to);
            ui.tree.set_scroll(id, 0.0, to);
        } else {
            self.scroll.to(to, ui.now, self.glide);
        }
    }

    /// Its rail (every frame it may change): over its right edge, inside its padding, still
    /// while the rows scroll; shown while it is up and scrolls, with its opacity. The rows stand
    /// where their glide is now. Returns the next wake (a frame while they glide).
    #[must_use]
    pub fn sync_rail(&mut self, ui: &mut Ui, host: Id, parts: Parts, skin: Skin) -> Option<f64> {
        let id = host;
        let up = ui.popups.iter().any(|p| p.id == id) && ui.tree.el(id).visual().clip;
        let content = content_h(ui, id);
        let s = ui.scale();
        let r = ui.tree.rect(id);
        let pad = ui.tree.el(id).layout().padding;
        let (view, total) = (r.h as f32 / s - pad[0] - pad[2], content - pad[0] - pad[2]);
        if up {
            let (x, y) = ((r.x + r.w) as f32 / s - 2.0 - skin.width, r.y as f32 / s + pad[0]);
            let at = ui.tree.rect(parts.root);
            let px = |d: f32| (d * s).round() as i32;
            if at.x != px(x) || at.y != px(y) || at.h != px(view) {
                ui.tree.set_layout(parts.root, |l| {
                    l.inset = [Len::Px(y), Len::Auto, Len::Auto, Len::Px(x)];
                    l.height = Len::Px(view);
                });
            }
        }
        let now = ui.now;
        self.scroll.hold(0.0, reach(ui, id), now, self.glide);
        let sy = self.scroll.at(now);
        if (ui.tree.el(id).scroll().1 - sy).abs() > 0.01 {
            ui.tree.set_scroll(id, 0.0, sy);
        }
        let opacity = ui.tree.el(id).visual().opacity;
        let wake = self.rail.sync(ui, parts, Range { viewport: view, content: total, offset: sy }, skin, up, opacity);
        if self.scroll.done(now) {
            wake
        } else {
            Some(now)
        }
    }

    /// A press, drag or wheel on its rail: it scrolls as a list does under its rail (the thumb
    /// carries the rows; a press on the track pages). False when the event is not the rail's.
    pub fn rail_event(&mut self, ui: &mut Ui, host: Id, parts: Parts, ev: &Event) -> bool {
        let root = parts.root;
        let (id, y) = match *ev {
            Event::Press { id, y, .. } | Event::Drag { id, y, .. } | Event::DragEnd { id, y, .. } => (id, y),
            Event::DragCancel { id } => (id, 0),
            Event::Wheel { id, dy } if id == root => {
                self.wheel(ui, host, dy);
                return true;
            }
            _ => return false,
        };
        if id != root {
            return false;
        }
        let y = (y - ui.tree.rect(root).y) as f32 / ui.scale();
        let bar = &mut self.rail;
        let offset = match *ev {
            Event::Press { .. } => {
                bar.press(y);
                None
            }
            Event::Drag { .. } => bar.drag(y),
            Event::DragEnd { .. } => bar.release(y),
            _ => {
                bar.cancel();
                None
            }
        };
        // The thumb carries the rows as it moves; a page from the track glides there.
        if let Some(offset) = offset {
            if matches!(ev, Event::Drag { .. }) {
                self.scroll.jump(offset);
                ui.tree.set_scroll(host, 0.0, offset);
            } else {
                self.scroll.to(offset, ui.now, self.glide);
            }
        }
        true
    }
}

/// How far `host`'s rows reach past its bottom (dips): its scroll's end.
fn reach(ui: &Ui, host: Id) -> f32 {
    let view = ui.tree.rect(host).h as f32 / ui.scale();
    (content_h(ui, host) - view).max(0.0)
}

/// The rows laid end to end, with the host's padding (dips): their own heights, not their
/// on-screen rects, which move as it scrolls.
pub fn content_h(ui: &Ui, host: Id) -> f32 {
    let s = ui.scale();
    let kids = ui.tree.children(host);
    let pad = ui.tree.el(host).layout().padding;
    let (Some(&first), Some(&last)) = (kids.first(), kids.last()) else { return pad[0] + pad[2] };
    let m = |v: Len| if let Len::Px(x) = v { x } else { 0.0 };
    use crate::style::side;
    let (a, b) = (ui.tree.el(first).layout().margin[side::TOP], ui.tree.el(last).layout().margin[side::BOTTOM]);
    (ui.tree.rect(last).b() - ui.tree.rect(first).y) as f32 / s + pad[0] + pad[2] + m(a) + m(b)
}
