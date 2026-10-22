//! Reusable vertical scrollbar mechanics. The product supplies its declarative skin and nodes.
use crate::anim::Ease;
use crate::{Id, Len, Position, Ui};

#[derive(Clone, Copy, Debug, Default)]
pub struct Range {
    pub viewport: f32,
    pub content: f32,
    pub offset: f32,
}
impl Range {
    pub fn reach(self) -> f32 {
        (self.content - self.viewport).max(0.0)
    }
    /// It scrolls at all: its reach is more than half a dip.
    fn scrolls(self) -> bool {
        self.reach() > 0.5
    }
    /// An offset kept within its reach.
    fn clamp(self, offset: f32) -> f32 {
        offset.clamp(0.0, self.reach())
    }
    pub fn thumb(self, minimum: f32) -> (f32, f32) {
        let height = self.viewport.max(0.0);
        if height == 0.0 || !self.scrolls() {
            return (0.0, height);
        }
        let length = (height * height / self.content).clamp(minimum.min(height).max(0.0), height);
        let top = (height - length) * (self.offset / self.reach()).clamp(0.0, 1.0);
        (top, length)
    }
}
#[derive(Clone, Copy)]
pub struct Skin {
    pub thin: f32,
    pub width: f32,
    pub thumb_width: f32,
    pub minimum: f32,
    pub enter_delay: f64,
    pub leave_delay: f64,
    pub duration: f64,
    pub ease: Ease,
}
#[derive(Clone, Copy)]
pub struct Parts {
    pub root: Id,
    pub track: Id,
    pub thumb: Id,
}
#[derive(Clone, Copy)]
enum Press {
    Thumb(f32),
    Track(f32),
}
#[derive(Default)]
pub struct Scrollbar {
    range: Range,
    minimum: f32,
    press: Option<Press>,
    moved: bool,
    expanded: bool,
    hover_since: Option<f64>,
    leave_since: Option<f64>,
    /// How far it is expanded, 0 to 1.
    pose: crate::anim::Tween,
}
impl Scrollbar {
    pub fn offset(&self) -> f32 {
        self.range.offset
    }
    pub fn grabbed(&self) -> bool {
        matches!(self.press, Some(Press::Thumb(_)))
    }
    pub fn cancel(&mut self) {
        self.press = None;
        self.moved = false;
    }
    pub fn press(&mut self, y: f32) {
        let (top, length) = self.range.thumb(self.minimum);
        self.press = if y >= top && y <= top + length && length > 0.0 {
            Some(Press::Thumb((y - top) / length))
        } else {
            Some(Press::Track(if y < top { -1.0 } else { 1.0 }))
        };
        self.moved = false;
    }
    fn dragged_to(&self, y: f32) -> Option<f32> {
        let Press::Thumb(grab) = self.press? else { return None };
        let (_, length) = self.range.thumb(self.minimum);
        let travel = self.range.viewport - length;
        (travel > 0.0).then(|| self.range.clamp((y - grab * length) / travel * self.range.reach()))
    }
    pub fn drag(&mut self, y: f32) -> Option<f32> {
        self.moved = true;
        let offset = self.dragged_to(y);
        if let Some(offset) = offset {
            self.range.offset = offset;
        }
        offset
    }
    pub fn release(&mut self, y: f32) -> Option<f32> {
        let result = match self.press {
            Some(Press::Thumb(_)) => self.dragged_to(y),
            Some(Press::Track(direction)) if !self.moved => {
                Some(self.range.clamp(self.range.offset + direction * self.range.viewport))
            }
            _ => None,
        };
        if let Some(offset) = result {
            self.range.offset = offset;
        }
        self.cancel();
        result
    }
    fn pose(&mut self, now: f64, hovered: bool, skin: Skin) -> (f32, Option<f64>) {
        let before = self.expanded;
        let mut wake = None;
        if self.grabbed() && self.moved {
            self.expanded = true;
            self.hover_since = None;
            self.leave_since = None;
        } else if hovered {
            self.leave_since = None;
            if !before {
                match dwell(&mut self.hover_since, skin.enter_delay, now) {
                    None => self.expanded = true,
                    deadline => wake = deadline,
                }
            }
        } else {
            self.hover_since = None;
            if before {
                match dwell(&mut self.leave_since, skin.leave_delay, now) {
                    None => self.expanded = false,
                    deadline => wake = deadline,
                }
            }
        }
        let target = if self.expanded { 1.0 } else { 0.0 };
        self.pose.retarget(target, now, skin.duration, skin.ease);
        if !self.pose.done(now) {
            wake = Some(now);
        }
        (self.pose.at(now), wake)
    }
    /// Geometry is in DIPs. The root's placement and the brushes are owned by the DSL skin.
    ///
    /// Returns when it must be synced again, which its owner arranges with its own handles: a
    /// time at or before `ui.now` means every frame (it is moving: a ticker), a later one a hover
    /// delay (a timer); `None` at rest.
    #[must_use]
    pub fn sync(
        &mut self,
        ui: &mut Ui,
        parts: Parts,
        range: Range,
        skin: Skin,
        active: bool,
        opacity: f32,
    ) -> Option<f64> {
        self.range = range;
        self.minimum = skin.minimum;
        let shown = active && range.scrolls() && range.viewport > 0.0;
        ui.tree.set_visible(parts.root, shown);
        if !shown {
            *self = Self::default();
            return None;
        }
        let hovered = ui.tree.el(parts.root).hovered();
        let (pose, wake) = self.pose(ui.now, hovered, skin);
        ui.tree.set_opacity(parts.root, opacity);
        let gutter = skin.thin + (skin.width - skin.thin) * pose;
        let width = skin.thin + (skin.thumb_width - skin.thin) * pose;
        let (top, length) = range.thumb(skin.minimum);
        // Stable layout boxes inherit the skin root's size. Range/hover/drag only change the
        // painted plates, never invalidate the surrounding flex tree after its layout barrier.
        // The root owns hit testing; press/drag above use the same DIP range as these plates.
        for id in [parts.track, parts.thumb] {
            ui.tree.set_layout(id, |l| {
                l.position = Position::Absolute;
                l.width = Len::Pct(100.0);
                l.height = Len::Pct(100.0);
                l.inset = [Len::Px(0.0), Len::Auto, Len::Auto, Len::Px(0.0)];
            });
        }
        ui.tree.set_visual(parts.track, |v| {
            v.radius = gutter * 0.5;
            v.plate = Some([skin.width - gutter, 0.0, gutter, range.viewport]);
        });
        ui.tree.set_opacity(parts.track, pose);
        ui.tree.set_visual(parts.thumb, |v| {
            v.radius = width * 0.5;
            v.plate = Some([skin.width - (gutter + width) * 0.5, top, width, length]);
        });
        wake
    }
}

/// A dwell of `delay` from when it began (`since`, begun now if not yet): `None` once it has
/// passed (and `since` is cleared), else when it will have.
fn dwell(since: &mut Option<f64>, delay: f64, now: f64) -> Option<f64> {
    let deadline = *since.get_or_insert(now) + delay;
    if now >= deadline {
        *since = None;
        None
    } else {
        Some(deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn skin() -> Skin {
        Skin {
            thin: 2.0,
            width: 12.0,
            thumb_width: 6.0,
            minimum: 24.0,
            enter_delay: 0.4,
            leave_delay: 2.0,
            duration: 0.167,
            ease: Ease::Bezier(0.0, 0.0, 0.0, 1.0),
        }
    }
    #[test]
    fn geometry_drag_page_cancel_and_dwell() {
        let mut bar = Scrollbar {
            range: Range { viewport: 200.0, content: 1000.0, offset: 400.0 },
            minimum: 24.0,
            ..Default::default()
        };
        assert_eq!(bar.range.thumb(24.0), (80.0, 40.0));
        bar.press(90.0);
        assert_eq!(bar.drag(110.0), Some(500.0));
        assert_eq!(bar.release(999.0), Some(800.0));
        bar.range.offset = 400.0;
        bar.press(10.0);
        assert_eq!(bar.release(10.0), Some(200.0));
        bar.range.offset = 400.0;
        bar.press(190.0);
        assert_eq!(bar.release(190.0), Some(600.0));
        bar.range.offset = 400.0;
        bar.press(90.0);
        bar.cancel();
        assert_eq!(bar.release(100.0), None);
        assert_eq!(Range { viewport: 10.0, content: 1000.0, offset: 20.0 }.thumb(24.0), (0.0, 10.0));
        assert_eq!(bar.pose(1.0, true, skin()).0, 0.0);
        assert_eq!(bar.pose(1.39, true, skin()).0, 0.0);
        bar.pose(1.4, true, skin());
        assert!((bar.pose(1.6, true, skin()).0 - 1.0).abs() < 0.001);
        bar.pose(2.0, false, skin());
        assert_eq!(bar.pose(3.99, false, skin()).0, 1.0);
        bar.pose(4.0, false, skin());
        assert!(bar.pose(4.2, false, skin()).0.abs() < 0.001);
    }
}
