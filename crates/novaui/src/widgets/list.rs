//! A virtualized list: rows of a fixed pitch, of which only those in view (plus one either side)
//! exist in the tree. Scrolling repositions the few live rows; a list of a million items costs
//! what a screenful does. The app supplies how a row is made and how it is filled.
//!
//! ```ignore
//! let mut list = VirtualList::new(viewport, 24.0);
//! // each update:
//! list.sync(&mut ui, items.len(), |ui| make_row(ui), |ui, row, i| fill(ui, row, i));
//! // on Event::Wheel for the viewport:
//! list.wheel(dy);
//! ```
//!
//! Rows are made under `viewport` by `make` (an app may instantiate a DSL component there); the
//! list places them absolutely at the full width and, unless a row has a height of its own, the
//! pitch's height, and moves them with translation, so scrolling never relayouts. Rows that leave the view are hidden and reused.
//!
//! An item is a *place*: the list's n-th row. What a place shows is the app's (a filtered list
//! maps places to its items in `fill`). A place may be carried (`carry`: a drag to reorder): its
//! row stays live wherever it is, and the rows between its place and where it would land give way,
//! easing to their new places (`GIVE_WAY`).

use crate::anim::{Ease, Tween};
use crate::element::Id;
use crate::style::{Len, Position};
use crate::Ui;
use std::collections::HashMap;

/// How rows give way to a carried one: WinUI's reorder, 0.25 s on the fast-out curve.
pub const GIVE_WAY: (f64, Ease) = (0.25, Ease::Bezier(0.1, 0.9, 0.2, 1.0));

pub struct VirtualList {
    /// The clipping element the rows live in.
    pub viewport: Id,
    /// Row pitch, dips.
    pub pitch: f32,
    /// Scroll offset from the top, dips (clamped at each sync).
    pub offset: f32,
    /// Space above the first row, dips.
    pub lead: f32,
    count: usize,
    /// Live rows by place.
    live: HashMap<usize, Id>,
    /// Rows out of view, kept for reuse.
    spare: Vec<Id>,
    stale: bool,
    /// The place being carried and its row's top (dips, from the list's top, not scrolled).
    carry: Option<(usize, f32)>,
    /// Rows giving way: each one's top (unscrolled), easing to where it now stands.
    tops: HashMap<usize, Tween>,
}

impl VirtualList {
    pub fn new(viewport: Id, pitch: f32) -> Self {
        VirtualList {
            viewport,
            pitch,
            offset: 0.0,
            lead: 0.0,
            count: 0,
            live: HashMap::new(),
            spare: Vec::new(),
            stale: false,
            carry: None,
            tops: HashMap::new(),
        }
    }

    /// The furthest the list scrolls, for a viewport `h` dips tall.
    pub fn reach(&self, h: f32) -> f32 {
        (self.count as f32 * self.pitch - h).max(0.0)
    }

    /// A wheel turn of `dy` dips (positive: toward the top).
    pub fn wheel(&mut self, dy: f32) {
        self.offset -= dy;
    }

    /// Scrolls the least that brings item `i` into a viewport `h` dips tall.
    pub fn reveal(&mut self, i: usize, h: f32) {
        let top = i as f32 * self.pitch;
        if top < self.offset {
            self.offset = top;
        } else if top + self.pitch > self.offset + h {
            self.offset = top + self.pitch - h;
        }
    }

    /// Every live row is refilled at the next sync (the items' content changed).
    pub fn refresh(&mut self) {
        self.stale = true;
    }

    /// The live row showing item `i`, if it is in view.
    pub fn row(&self, i: usize) -> Option<Id> {
        self.live.get(&i).copied()
    }

    /// The item a live row shows.
    pub fn item_of(&self, row: Id) -> Option<usize> {
        self.live.iter().find(|(_, &r)| r == row).map(|(&i, _)| i)
    }

    /// Live rows.
    pub fn live(&self) -> usize {
        self.live.len()
    }

    /// The live rows and the places they show (for state a row shows beyond what `fill` wrote:
    /// hover, the current item).
    pub fn rows(&self) -> impl Iterator<Item = (usize, Id)> + '_ {
        self.live.iter().map(|(&i, &r)| (i, r))
    }

    /// Carries place `from`: its row stands at `y` (dips from the list's top, not scrolled) and
    /// stays live wherever that is; the rows between it and where it would land (`landing`)
    /// give way. Called again as the pointer moves; `end_carry` puts every row back.
    pub fn carry(&mut self, from: usize, y: f32) {
        self.carry = Some((from, y));
    }

    /// The carry is over: the rows stand at their places again.
    pub fn end_carry(&mut self) {
        self.carry = None;
        self.tops.clear();
    }

    /// The place being carried.
    pub fn carried(&self) -> Option<usize> {
        self.carry.map(|(from, _)| from)
    }

    /// Where the carried row would land: the place its middle is over.
    pub fn landing(&self) -> Option<usize> {
        let (_, y) = self.carry?;
        let last = self.count.saturating_sub(1) as f32;
        Some(((y + self.pitch * 0.5) / self.pitch).floor().clamp(0.0, last) as usize)
    }

    /// Rows are still giving way at `now`: the owner asks for a frame.
    pub fn moving(&self, now: f64) -> bool {
        self.tops.values().any(|t| !t.done(now))
    }

    /// Where place `i`'s row stands (unscrolled), carried or giving way.
    fn rest(&self, i: usize) -> f32 {
        let at = i as f32 * self.pitch;
        let (Some((from, y)), Some(to)) = (self.carry, self.landing()) else { return at };
        if i == from {
            y
        } else if from < to && i > from && i <= to {
            at - self.pitch
        } else if from > to && i >= to && i < from {
            at + self.pitch
        } else {
            at
        }
    }

    /// Brings the tree in line with `count` items at the current offset: rows leaving the view
    /// are parked (hidden, kept); rows entering come from the park or `make`, and are filled
    /// with `fill(ui, row, item)`. Returns true if any row was filled.
    pub fn sync(
        &mut self,
        ui: &mut Ui,
        count: usize,
        mut make: impl FnMut(&mut Ui) -> Id,
        mut fill: impl FnMut(&mut Ui, Id, usize),
    ) -> bool {
        // The viewport's own geometry first (its rows do not affect it).
        ui.tree.resolve_layout();
        let (_, h) = ui.tree.size(self.viewport);
        if count != self.count {
            self.count = count;
            self.stale = true;
        }
        if self.carry.is_some_and(|(from, _)| from >= count) {
            self.end_carry();
        }
        self.offset = self.offset.clamp(0.0, self.reach(h));
        let end = (((self.offset + h) / self.pitch).ceil() as usize + 1).min(count);
        let first = ((self.offset / self.pitch).floor() as usize).saturating_sub(1).min(end);
        let carried = self.carried();
        let shows = |i: usize| (first..end).contains(&i) || carried == Some(i);
        let stale = std::mem::take(&mut self.stale);
        let gone: Vec<usize> = self.live.keys().copied().filter(|&i| !shows(i)).collect();
        for i in gone {
            let r = self.live.remove(&i).unwrap();
            ui.tree.set_visible(r, false);
            self.spare.push(r);
            self.tops.remove(&i);
        }
        let pitch = self.pitch;
        let now = ui.now();
        let mut filled = false;
        for i in (first..end).chain(carried.filter(|&c| !(first..end).contains(&c))) {
            let (row, fresh) = match self.live.get(&i) {
                Some(&r) => (r, false),
                None => {
                    let r = match self.spare.pop() {
                        Some(r) => r,
                        None => {
                            let r = make(ui);
                            ui.tree.set_layout(r, |l| {
                                l.position = Position::Absolute;
                                l.inset = [Len::Px(0.0), Len::Px(0.0), Len::Auto, Len::Px(0.0)];
                                // A row with a height of its own (a sheet's component, sized by
                                // its rules) keeps it; the others are the pitch tall.
                                if l.height == Len::Auto {
                                    l.height = Len::Px(pitch);
                                }
                            });
                            r
                        }
                    };
                    ui.tree.set_visible(r, true);
                    self.live.insert(i, r);
                    (r, true)
                }
            };
            if fresh || stale {
                fill(ui, row, i);
                filled = true;
            }
            // The carried row follows the pointer; the others ease to where they stand.
            let rest = self.rest(i);
            let top = if self.carry.is_some() && carried != Some(i) {
                let (secs, ease) = GIVE_WAY;
                let t = self.tops.entry(i).or_insert_with(|| Tween::rest(i as f32 * pitch));
                t.retarget(rest, now, secs, ease);
                t.at(now)
            } else {
                rest
            };
            // Placed by translation: a move, never a relayout.
            ui.tree.set_translate(row, 0.0, self.lead + top - self.offset);
        }
        filled
    }
}
