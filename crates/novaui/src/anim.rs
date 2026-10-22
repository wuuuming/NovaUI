//! Property animations on the tree. Curves are WinUI's (Microsoft Learn, "Timing and easing"):
//! `Decelerate` = cubic-bezier(0, 0, 0, 1) (FastOutSlowIn / "fast out"), `Accelerate` =
//! cubic-bezier(1, 0, 1, 1), `Standard` = cubic-bezier(0.8, 0, 0.2, 1); plus linear.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prop {
    Opacity,
    X,
    Y,
    Scale,
    /// `Visual::scale_x`: the box's width only.
    ScaleX,
    ScrollY,
    FgAlpha,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ease {
    Linear,
    Decelerate,
    Accelerate,
    Standard,
    /// Cubic ease-out: 1 - (1 - t)^3.
    OutCubic,
    /// Cubic ease-in-out: 4t³ to the middle, then its mirror (an indeterminate progress bar's
    /// sweep).
    InOutCubic,
    /// Smoothstep, t²(3 − 2t): the lane's page turn.
    Smooth,
    Bezier(f32, f32, f32, f32),
}

/// What an exit leaves at its end (`Tree::exit`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Then {
    /// The element stays where the motion leaves it.
    Stay,
    /// The element goes `shown: false`, its children dropped, its offset reset.
    Hide,
}

/// WinUI durations (Microsoft Learn): fast 167 ms, normal 250 ms, slow 333 ms.
pub const FAST: f32 = 0.167;
pub const NORMAL: f32 = 0.25;
pub const SLOW: f32 = 0.333;

/// Where an entering element comes from, or an exiting one goes: its opacity and offset from
/// rest (dips, as `Tree::set_translate`). An axis left `None` is not touched, so another
/// motion on it (a list's rise under a page turn) carries on. See `Tree::enter` / `exit`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub opacity: f32,
    pub dx: Option<f32>,
    pub dy: Option<f32>,
}

impl Pose {
    /// Transparent; offsets untouched.
    pub const CLEAR: Pose = Pose { opacity: 0.0, dx: None, dy: None };
    /// Transparent, offsets reset to rest (a reveal with no travel over a moved element).
    pub const CLEAR_PLACED: Pose = Pose { opacity: 0.0, dx: Some(0.0), dy: Some(0.0) };
    /// Transparent, `dx` across (a horizontal turn).
    pub const fn clear_x(dx: f32) -> Pose {
        Pose { opacity: 0.0, dx: Some(dx), dy: None }
    }
    /// Transparent, `dy` down (a rise or drop).
    pub const fn clear_y(dy: f32) -> Pose {
        Pose { opacity: 0.0, dx: None, dy: Some(dy) }
    }
    /// The same axes at rest.
    pub fn rest(self) -> Pose {
        Pose { opacity: 1.0, dx: self.dx.map(|_| 0.0), dy: self.dy.map(|_| 0.0) }
    }
}

/// A value easing from `from` to `to` over `dur` seconds from `t0`, on `ease`: what a motion is,
/// read at a frame's time. An element's property animates with `Tree::animate` (which holds one);
/// a value that is not one (an app's open amount, a list's scroll, a row giving way) holds a
/// `Tween` and asks for a frame while it is not `done`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tween {
    pub from: f32,
    pub to: f32,
    pub t0: f64,
    pub dur: f64,
    pub ease: Ease,
}

impl Default for Tween {
    fn default() -> Self {
        Tween::rest(0.0)
    }
}

impl Tween {
    pub fn new(from: f32, to: f32, t0: f64, dur: f64, ease: Ease) -> Tween {
        Tween { from, to, t0, dur, ease }
    }

    /// Standing at `v`.
    pub fn rest(v: f32) -> Tween {
        Tween { from: v, to: v, t0: 0.0, dur: 0.0, ease: Ease::Linear }
    }

    /// The value at `now`.
    pub fn at(&self, now: f64) -> f32 {
        self.from + (self.to - self.from) * self.ease.at(self.progress(now))
    }

    /// The share of the time gone, 0 to 1.
    pub fn progress(&self, now: f64) -> f32 {
        if self.dur <= 0.0 {
            1.0
        } else {
            ((now - self.t0) / self.dur).clamp(0.0, 1.0) as f32
        }
    }

    /// It has arrived (no frame is needed for it any more).
    pub fn done(&self, now: f64) -> bool {
        self.progress(now) >= 1.0
    }

    /// Heads for `to` from where it stands at `now` (no jump), over `dur` on `ease`. A tween
    /// already heading there keeps going as it was.
    pub fn retarget(&mut self, to: f32, now: f64, dur: f64, ease: Ease) {
        if to == self.to {
            return;
        }
        *self = Tween { from: self.at(now), to, t0: now, dur, ease };
    }
}

/// How a scroll offset moves to a new end: WinUI's offset change (ScrollPresenter's position
/// animation, microsoft-ui-xaml `ScrollPresenter.h`): 5 ms per dip travelled, held between 50 ms
/// and 1 s, on Fluent's "fast out, slow in" curve (`Ease::Decelerate`); a new end mid-way starts
/// again from where it stands. Quick wheel notches the same way go further. `Default` is WinUI's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlideSkin {
    /// Seconds per dip of distance, held between `min` and `max`.
    pub per_dip: f64,
    pub min: f64,
    pub max: f64,
    pub ease: Ease,
    /// Notches the same way within `accel_window` seconds of each other build speed, each by
    /// `accel_step`, up to `accel` notches' worth; a pause or a turn back starts from one.
    pub accel: f32,
    pub accel_window: f64,
    pub accel_step: f32,
}

impl GlideSkin {
    /// WinUI's: 5 ms a dip, 50 ms to 1 s, decelerating; up to three notches' worth.
    pub const WINUI: GlideSkin = GlideSkin {
        per_dip: 0.005,
        min: 0.05,
        max: 1.0,
        ease: Ease::Decelerate,
        accel: 3.0,
        accel_window: 0.14,
        accel_step: 1.35,
    };
}

impl Default for GlideSkin {
    fn default() -> Self {
        GlideSkin::WINUI
    }
}

/// A scroll offset that glides to where it is sent (a wheel notch, a reveal, a page): every
/// scrolled thing moves the same way. The holder reads `at(now)` each frame and asks for a frame
/// while it is not `done`; a resize or a new content is not a scroll and `jump`s.
#[derive(Clone, Copy, Debug, Default)]
pub struct Glide {
    tween: Tween,
    wheel_at: f64,
    wheel_sign: f32,
    boost: f32,
}

impl Glide {
    /// Standing at `v`.
    pub fn rest(v: f32) -> Glide {
        Glide { tween: Tween::rest(v), ..Default::default() }
    }

    /// Where it is at `now`.
    pub fn at(&self, now: f64) -> f32 {
        self.tween.at(now)
    }

    /// Where it is heading (where it stands once `done`).
    pub fn target(&self) -> f32 {
        self.tween.to
    }

    /// It has arrived: no frame is needed for it.
    pub fn done(&self, now: f64) -> bool {
        self.tween.done(now)
    }

    /// Heads for `to` from where it stands at `now`, over a time that follows the distance. A
    /// glide already heading there (within half a dip) keeps going as it was; one standing there
    /// settles on it.
    pub fn to(&mut self, to: f32, now: f64, skin: GlideSkin) {
        if (to - self.tween.to).abs() <= 0.5 {
            if self.done(now) {
                self.jump(to);
            }
            return;
        }
        let from = self.at(now);
        let dur = ((to - from).abs() as f64 * skin.per_dip).clamp(skin.min, skin.max);
        self.tween = Tween::new(from, to, now, dur, skin.ease);
    }

    /// Stands at `v` at once.
    pub fn jump(&mut self, v: f32) {
        self.tween = Tween::rest(v);
    }

    /// A wheel notch of `dy` dips (positive: the content moves up): how far it carries the
    /// offset back, quick notches the same way going further (`GlideSkin::accel`).
    pub fn notch(&mut self, dy: f32, now: f64, skin: GlideSkin) -> f32 {
        let sign = dy.signum();
        let whole = dy.abs() >= 47.5;
        self.boost = if whole && now - self.wheel_at <= skin.accel_window && sign == self.wheel_sign {
            (self.boost.max(1.0) * skin.accel_step).min(skin.accel.max(1.0))
        } else {
            1.0
        };
        self.wheel_at = now;
        self.wheel_sign = sign;
        dy * self.boost
    }

    /// A wheel notch of `dy` dips: heads `notch` dips back from where it was heading, held
    /// between `lo` and `hi`.
    pub fn wheel(&mut self, dy: f32, now: f64, lo: f32, hi: f32, skin: GlideSkin) {
        let by = self.notch(dy, now, skin);
        let to = (self.target().clamp(lo, hi.max(lo)) - by).clamp(lo, hi.max(lo));
        self.to(to, now, skin);
    }

    /// Kept between `lo` and `hi` (the content changed under it): a target past them is
    /// brought in, at once where the offset itself is past them.
    pub fn hold(&mut self, lo: f32, hi: f32, now: f64, skin: GlideSkin) {
        let hi = hi.max(lo);
        let (at, to) = (self.at(now), self.target());
        if at < lo || at > hi {
            self.jump(at.clamp(lo, hi));
        } else if to < lo || to > hi {
            self.to(to.clamp(lo, hi), now, skin);
        }
    }
}

/// A property animation of one element (`Tree::animate`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Anim {
    pub(crate) el: u32,
    pub(crate) prop: Prop,
    pub(crate) tween: Tween,
}

/// y(x) of a CSS cubic-bezier with P0 = (0,0), P3 = (1,1) (Newton + bisection on x(t)).
pub fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let bx = |t: f32| 3.0 * (1.0 - t) * (1.0 - t) * t * x1 + 3.0 * (1.0 - t) * t * t * x2 + t * t * t;
    let by = |t: f32| 3.0 * (1.0 - t) * (1.0 - t) * t * y1 + 3.0 * (1.0 - t) * t * t * y2 + t * t * t;
    let dx = |t: f32| 3.0 * (1.0 - t) * (1.0 - t) * x1 + 6.0 * (1.0 - t) * t * (x2 - x1) + 3.0 * t * t * (1.0 - x2);
    let mut t = x;
    for _ in 0..8 {
        let e = bx(t) - x;
        let d = dx(t);
        if e.abs() < 1e-5 {
            return by(t);
        }
        if d.abs() < 1e-6 {
            break;
        }
        t = (t - e / d).clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        t = (lo + hi) * 0.5;
        if bx(t) < x {
            lo = t;
        } else {
            hi = t;
        }
    }
    by(t)
}

impl Ease {
    pub fn at(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Ease::Linear => t,
            Ease::Decelerate => bezier(0.0, 0.0, 0.0, 1.0, t),
            Ease::Accelerate => bezier(1.0, 0.0, 1.0, 1.0, t),
            Ease::Standard => bezier(0.8, 0.0, 0.2, 1.0, t),
            Ease::OutCubic => 1.0 - (1.0 - t).powi(3),
            Ease::InOutCubic if t < 0.5 => 4.0 * t * t * t,
            Ease::InOutCubic => 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0,
            Ease::Smooth => t * t * (3.0 - 2.0 * t),
            Ease::Bezier(a, b, c, d) => bezier(a, b, c, d, t),
        }
    }
}

impl Anim {
    /// (value, finished)
    pub(crate) fn sample(&self, now: f64) -> (f32, bool) {
        (self.tween.at(now), self.tween.done(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_glide_takes_longer_the_further_it_goes_within_its_bounds_and_turns_without_a_jump() {
        let skin = GlideSkin::default();
        let mut g = Glide::rest(0.0);
        g.to(100.0, 0.0, skin);
        assert!(!g.done(0.49) && g.done(0.5), "100 dips at 5 ms a dip");
        g.to(105.0, 1.0, skin);
        assert!(g.done(1.05) && !g.done(1.049), "never under 50 ms");
        let mut far = Glide::rest(0.0);
        far.to(10_000.0, 0.0, skin);
        assert!(far.done(1.0), "never over a second");
        let at = far.at(0.3);
        far.to(0.0, 0.3, skin);
        assert_eq!(far.at(0.3), at, "a new end starts from where it stands");
        assert_eq!(far.target(), 0.0);
    }

    #[test]
    fn quick_notches_the_same_way_go_further_and_a_pause_or_a_turn_starts_from_one() {
        let skin = GlideSkin::default();
        let mut g = Glide::rest(1000.0);
        let steps: Vec<f32> = [0.0, 0.1, 0.2, 0.3, 0.4].iter().map(|&t| g.notch(48.0, t, skin)).collect();
        assert_eq!(steps[0], 48.0);
        assert!(steps.windows(2).all(|w| w[1] >= w[0]));
        assert!((steps[4] - 48.0 * skin.accel).abs() < 1e-3, "up to accel notches' worth: {steps:?}");
        assert_eq!(g.notch(48.0, 1.0, skin), 48.0, "a pause");
        g.notch(48.0, 1.05, skin);
        assert_eq!(g.notch(-48.0, 1.1, skin), -48.0, "a turn back");
        g.wheel(48.0, 2.0, 0.0, 2000.0, skin);
        assert_eq!(g.target(), 952.0, "positive: the offset goes back");
        g.wheel(-5000.0, 2.0, 0.0, 2000.0, skin);
        assert_eq!(g.target(), 2000.0, "held at the end");
    }

    #[test]
    fn hold_brings_a_glide_inside_new_bounds() {
        let skin = GlideSkin::default();
        let mut g = Glide::rest(0.0);
        g.to(500.0, 0.0, skin);
        g.hold(0.0, 300.0, 0.1, skin);
        assert_eq!(g.target(), 300.0);
        assert!(!g.done(0.1), "still gliding, from where it was");
        g.hold(0.0, 10.0, 0.2, skin);
        assert_eq!((g.at(0.2), g.done(0.2)), (10.0, true), "standing past the end: at the end at once");
    }
}
