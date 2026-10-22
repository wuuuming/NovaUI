//! The turn every host runs (`docs/runtime.md`, "One turn, in order"). The platform modules pump their own
//! messages and own their swap chains; the order of the turn (`turn`) and when the next one runs
//! (`Pacer`) are decided here, once for all of them. A platform implements `Host`: the parts
//! that differ by system, and nothing of their order.
//!
//! A host's turn (`turn`):
//! 1. `turn_begin`; `begin` (the swap chain's wait, then the one clock reading of the turn);
//!    `Host::events`: OS input into `Ui::input`, wakers into `Ui::woken`, size changes;
//! 2. `deliver`: edit ticks, events and messages to the app, the flush, then layout and
//!    after_layout when the app got anything;
//! 3. the host carries out the app's window requests, cursor, IME, clipboard and pickers;
//! 4. `draw`: a frame when the tree has work and the window can show it;
//! 5. the accessibility tree's update (`access`), when a client has it; `turn_end`; the host
//!    then sleeps for `Pacer::timeout_ms`.

use crate::app::{App, Event, Ui};
use crate::geom::Color;
use crate::platform::Requests;
use crate::render::primitive::Backend;

/// A platform's part of the turn: what differs by system. `turn` calls these in its one order.
pub(crate) trait Host {
    /// The clock: seconds since the host started.
    fn now(&self) -> f64;
    /// The turn draws at once, without waiting for the swap chain: a frame that must follow the
    /// window border, a modal loop, a window that cannot draw.
    fn live(&self) -> bool {
        false
    }
    /// Takes the platform's events since the last turn into `ui`: input (`Ui::input`), wakes
    /// (`Ui::woken`), size and scale (`Ui::resize`), window state, the display's refresh rate
    /// (`pacer`). `app` answers title-bar hit tests.
    fn events(&mut self, ui: &mut Ui, app: &mut dyn App, pacer: &mut Pacer);
    /// Carries out the app's window requests but `close`: minimise, maximise, fullscreen, title.
    fn requests(&mut self, ui: &mut Ui, req: Requests);
    /// The app asked to close: the window goes, the host's loop ends after this turn.
    fn close(&mut self) {}
    /// Shows `ui`'s cursor, and turns the IME on or off and moves its window (`ui.io`).
    fn cursor_and_ime(&mut self, ui: &Ui);
    /// Puts a field's copy or cut on the system's clipboard.
    fn clipboard_out(&mut self, text: String);
    /// The clipboard's text, for a field's paste and the app's reads.
    fn clipboard_in(&mut self) -> Option<String>;
    /// Opens the file pickers the app asked for (`Ui::pick`).
    fn picks(&mut self, ui: &mut Ui);
    /// The window's edge where the system draws none (`Tree::set_frame_stroke`).
    fn edge(&self, ui: &Ui) -> Option<Color>;
    /// Whether this turn may draw (a window that cannot show a frame, a frame held for its
    /// refresh, a size still to come). Called every turn before the frame, which is drawn when
    /// this allows and the tree has work.
    fn may_draw(&mut self, ui: &Ui, pacer: &mut Pacer) -> bool {
        let _ = (ui, pacer);
        true
    }
    fn backend(&mut self) -> &mut dyn Backend;
    /// The device was lost: a new backend at the window's size. False when none can be made
    /// (the loop ends).
    fn renew_backend(&mut self) -> bool;
    /// The turn's last word to the system (a size drawn, a flush).
    fn turn_done(&mut self) {}
    /// The window can show a frame (not minimised).
    fn visible(&self) -> bool {
        true
    }
    /// The window's accessibility (`access::Bridge`): its tree for assistive clients, their
    /// requests. `None` where a host has none.
    fn access(&mut self) -> Option<&mut crate::access::Bridge> {
        None
    }
}

/// Builds the app's first frame before the window shows (no flash of an empty window): `init`,
/// layout and `after_layout`, the edge, the frame.
pub(crate) fn start<H: Host + ?Sized>(host: &mut H, ui: &mut Ui, app: &mut dyn App) {
    app.init(ui);
    ui.now = host.now();
    crate::app::settle(app, ui, 0);
    ui.tree.set_frame_stroke(host.edge(ui));
    ui.frame(host.backend(), ui.now);
}

/// One turn, in order (the module's list). False when the window closed (the host's loop ends).
pub(crate) fn turn<H: Host + ?Sized>(host: &mut H, ui: &mut Ui, app: &mut dyn App, pacer: &mut Pacer) -> bool {
    ui.turn_begin();
    // Unless the turn is live, it first waits until the swap chain can take a frame, as DXGI's
    // frame-latency object is meant to be used: input, timers, the app and the frame then all see
    // the same time, the time of the frame they make (`runtime.md`, "One turn, in order": one
    // `frame_time`). The wait only blocks while a presented frame is still queued, at most one
    // refresh. Images the backend made off the UI thread since the last turn are taken in (their
    // elements paint again).
    if !host.live() {
        ui.wait_frame(host.backend());
    }
    ui.now = host.now();
    pacer.begin(ui.now);
    ui.tree.take_made_images(host.backend());
    host.events(ui, app, pacer);
    if let Some(a) = host.access() {
        a.requests(ui);
    }
    deliver(ui, app, pacer.period);
    let req = std::mem::take(&mut ui.window.req);
    if req.close {
        host.close();
        ui.turn_end(host.visible());
        return false;
    }
    // A fullscreen change lays out at once (no tween), on every host.
    if req.fullscreen.is_some_and(|f| f != ui.window.fullscreen) {
        ui.snap_layout();
    }
    host.requests(ui, req);
    host.cursor_and_ime(ui);
    if let Some(t) = ui.io.clipboard_out.take() {
        host.clipboard_out(t);
    }
    if ui.wants_clipboard() {
        let t = host.clipboard_in();
        ui.clipboard_read(t);
    }
    host.picks(ui);
    ui.tree.set_frame_stroke(host.edge(ui));
    let mut open = true;
    if host.may_draw(ui, pacer) && ui.tree.has_work() {
        draw(ui, host.backend(), pacer);
        if host.backend().lost() {
            open = host.renew_backend();
            ui.tree.invalidate_all();
        }
    }
    // The inspector follows what this frame laid out (its own change is the next frame's).
    #[cfg(any(debug_assertions, test))]
    crate::dev::Dev::sync(ui);
    // What this turn changed, laid out and placed, to an assistive client that has the tree.
    if let Some(a) = host.access() {
        a.update(ui);
    }
    host.turn_done();
    ui.turn_end(host.visible());
    open
}

/// When the next turn runs.
///
/// **The next refresh is a deadline, not a signal.** Without a present most hosts have no
/// "vblank happened" signal: DXGI's waitable object says there is room in the present queue,
/// which is already true after a turn that drew nothing, and Wayland's frame callback fires only
/// after a commit. A turn that drew nothing therefore runs again no sooner than the next refresh
/// after the last one, counted in whole periods from the last present; a drawn frame is paced
/// by the swap chain itself. Without this, an animation whose step changes no pixel (a wake
/// asked "at once" every turn) would turn the loop into a spin.
pub(crate) struct Pacer {
    /// Seconds per display refresh.
    pub(crate) period: f64,
    /// When the last frame was presented, and when the last turn ran and whether it drew.
    pub(crate) last_frame_at: f64,
    last_turn_at: f64,
    last_turn_drew: bool,
    /// A frame the host held back (win32's modal loop draws at most one per refresh): the turn
    /// that may draw it.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) hold: Option<f64>,
}

/// A lost backend is made again (`renew_backend`): the old one goes first, a window taking one
/// presenter at a time (a composition target, a native surface), then `make` makes the new
/// one. `false` when it could not: nothing draws until the next try.
pub(crate) fn replace_backend(
    slot: &mut Box<dyn crate::render::primitive::Backend>,
    make: impl FnOnce() -> Option<Box<dyn crate::render::primitive::Backend>>,
) -> bool {
    *slot = Box::new(crate::render::software::Software::new(1, 1));
    match make() {
        Some(be) => {
            *slot = be;
            true
        }
        None => false,
    }
}

/// Whole milliseconds from `now` until `t` (rounded up, at least 0), at most a minute: how long a
/// host sleeps for it.
pub(crate) fn ms_until(t: f64, now: f64) -> u32 {
    (((t - now) * 1000.0).ceil().max(0.0) as u32).min(60_000)
}

impl Pacer {
    pub(crate) fn new(hz: f64) -> Self {
        let mut p = Pacer { period: 0.0, last_frame_at: 0.0, last_turn_at: 0.0, last_turn_drew: true, hold: None };
        p.set_hz(hz);
        p
    }

    pub(crate) fn set_hz(&mut self, hz: f64) {
        self.period = Self::period_of(hz);
    }

    /// Seconds per refresh at `hz`, paced at 30 Hz at least.
    pub(crate) fn period_of(hz: f64) -> f64 {
        1.0 / hz.max(30.0)
    }

    /// The first refresh after `t`, counted from the last present.
    pub(crate) fn refresh_after(&self, t: f64) -> f64 {
        let n = ((t - self.last_frame_at) / self.period).floor() + 1.0;
        self.last_frame_at + n.max(1.0) * self.period
    }

    /// The next turn's time (`None`: nothing scheduled). `can_draw` is false while the window
    /// cannot show a frame (minimised): only the app's own timers count then.
    pub(crate) fn next_turn(&self, ui: &Ui, can_draw: bool) -> Option<f64> {
        let wake = if can_draw { ui.next_wake() } else { ui.next_app_wake() };
        let t = [wake, self.hold.filter(|_| can_draw)].into_iter().flatten().reduce(f64::min)?;
        Some(if self.last_turn_drew { t } else { t.max(self.refresh_after(self.last_turn_at)) })
    }

    /// How long a host sleeps before the next turn, in ms (`None`: until a message comes).
    /// `pending` is true when the host already holds messages or other work for the next turn.
    pub(crate) fn timeout_ms(&self, ui: &Ui, now: f64, can_draw: bool, pending: bool) -> Option<u32> {
        // Finished tasks and model changes are delivered even while the window cannot draw.
        // Tree work the host holds back (`hold`) waits for its refresh: run at once, every turn
        // would hold it again, a spin of empty turns until then.
        // Animations whose last frame drew nothing (an element out of view, a change under a
        // pixel) wait for the next refresh, as after a drawn frame (whose present the next turn
        // waits for) or a ticker: run at once, they spun empty turns until they ended, ~10 000
        // a second.
        let animation_only = !self.last_turn_drew && ui.only_animating();
        if pending || ui.sched.pending() || (can_draw && ui.has_work() && self.hold.is_none() && !animation_only) {
            return Some(0);
        }
        if can_draw && animation_only && self.hold.is_none() {
            // Nothing earlier: after a turn that drew nothing, `next_turn` waits for it too.
            let t = self.refresh_after(self.last_turn_at);
            return Some(ms_until(t, now));
        }
        self.next_turn(ui, can_draw).map(|t| ms_until(t, now))
    }

    /// A turn starts at `now`.
    pub(crate) fn begin(&mut self, now: f64) {
        self.last_turn_at = now;
        self.last_turn_drew = false;
    }
}

/// How many rounds the flush runs before it leaves the rest for the next turn (`runtime.md`, "Decisions").
const FLUSH_ROUNDS: usize = 8;

/// Phases 2–5, after the host has fed its input:
/// - 2 dispatch: input events in arrival order, finished tasks (within the delivery budget,
///   half a refresh), wakers, due timers, then running tickers (once per refresh, at the
///   turn's time, which is the frame's);
/// - 3 flush: the subscribers of models changed by 2 (and by 3 itself), each once per round;
/// - 4–5: layout and `after_layout`, only in a turn that delivered the app something (a turn
///   of tree work alone, an exit's end or a caret blink, does not call the app).
pub(crate) fn deliver<A: App + ?Sized>(ui: &mut Ui, app: &mut A, period: f64) {
    use crate::sched::Phase;
    crate::audit::contract!(
        crate::sched::phase() == Phase::Idle,
        "a turn inside a turn (app code ran a turn or settle)"
    );
    let start = std::time::Instant::now();
    let dispatch = crate::sched::enter(Phase::Dispatch);
    let budget = start + std::time::Duration::from_secs_f64(period * 0.5);
    // An exit that ended, or a resize's trim that came, is tree work of this turn.
    ui.tree.serve_deadlines(ui.now);
    ui.tick_edits(ui.now);
    let evs = std::mem::take(&mut ui.events);
    let mut delivered = !evs.is_empty();
    for e in evs {
        app.event(ui, e);
    }
    let mut msgs = std::mem::take(&mut ui.msgs);
    msgs.clear();
    ui.sched.take_done(budget, &mut msgs);
    ui.sched.take_wakes(&mut msgs);
    let tasks = msgs.len();
    ui.sched.take_timers(ui.now, &mut msgs);
    ui.sched.take_ticks(ui.now, period, &mut msgs);
    if let Some(tr) = &mut ui.trace {
        for (k, (_, site)) in msgs.iter().enumerate() {
            if k < tasks {
                tr.waker();
            } else {
                tr.timer(Some(site));
            }
        }
    }
    delivered |= !msgs.is_empty();
    for (m, _) in msgs.drain(..) {
        app.event(ui, Event::Message(m));
    }
    ui.msgs = msgs;
    delivered |= flush(ui, app);
    drop(dispatch);
    if delivered {
        crate::app::settle(app, ui, start.elapsed().as_micros() as u64);
    }
}

/// Phase 3: delivers model changes until none is left, at most `FLUSH_ROUNDS` rounds.
/// Returns whether it delivered anything.
fn flush<A: App + ?Sized>(ui: &mut Ui, app: &mut A) -> bool {
    let mut round = Vec::new();
    let mut delivered = false;
    for _ in 0..FLUSH_ROUNDS {
        crate::sched::take_notified(&mut round);
        if round.is_empty() {
            return delivered;
        }
        delivered = true;
        for m in round.drain(..) {
            app.event(ui, Event::Message(m));
        }
    }
    if crate::sched::notified() {
        // A subscriber that changes a model that notifies it back: a cycle. The rest waits for
        // the next turn, so the UI does not hang.
        debug_assert!(false, "model notifications still pending after {FLUSH_ROUNDS} rounds (a cycle)");
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| eprintln!("novaui: model notifications still pending after {FLUSH_ROUNDS} rounds (a cycle)"));
    }
    delivered
}

/// Phase 6: a frame at the turn's time, if the tree has work (the swap chain was waited for in
/// `begin`). Returns whether anything was presented.
pub(crate) fn draw(ui: &mut Ui, be: &mut dyn Backend, pacer: &mut Pacer) -> bool {
    if !ui.tree.has_work() {
        return false;
    }
    let now = ui.now;
    let drew = ui.frame(be, now);
    pacer.last_turn_drew = drew;
    if drew {
        pacer.last_frame_at = now;
        ui.refresh_hover();
    }
    drew
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame the host held back is drawn at its refresh: until then the loop sleeps instead of
    /// running empty turns (12 000 a second inside win32's size/move loop).
    #[test]
    fn a_held_frame_waits_for_its_refresh() {
        let ui = Ui::new(100, 100, 1.0);
        assert!(ui.has_work(), "a new window has its first frame to draw");
        let mut pacer = Pacer::new(240.0);
        assert_eq!(pacer.timeout_ms(&ui, 0.0, true, false), Some(0));
        pacer.hold = Some(0.0035);
        assert_eq!(pacer.timeout_ms(&ui, 0.0, true, false), Some(4));
        assert_eq!(pacer.timeout_ms(&ui, 0.0, true, true), Some(0), "queued messages still go at once");
    }

    /// An animation that shows nothing (here, of a hidden element) leaves the tree with work
    /// after a frame that drew nothing: the loop sleeps until the next refresh instead of running
    /// empty turns until the animation ends (~10 000 a second in a one-hour playback trace).
    #[test]
    fn an_animation_that_draws_nothing_waits_for_its_refresh() {
        use crate::anim::{Ease, Prop};
        let mut ui = Ui::new(100, 100, 1.0);
        let mut be = crate::render::software::Software::new(100, 100);
        let mut pacer = Pacer::new(240.0);
        let root = ui.tree.root();
        let el = ui.tree.add(root, Default::default(), Default::default(), crate::style::Content::None);
        ui.tree.set_visible(el, false);
        pacer.begin(0.0);
        assert!(draw(&mut ui, &mut be, &mut pacer), "the first frame");
        ui.tree.animate(el, Prop::Opacity, 0.5, 1.0, Ease::Linear, 0.0);
        pacer.begin(0.01);
        ui.now = 0.01;
        assert!(!draw(&mut ui, &mut be, &mut pacer), "a hidden element's animation draws nothing");
        assert!(ui.has_work(), "the animation runs on");
        let ms = pacer.timeout_ms(&ui, 0.01, true, false);
        assert!(ms.is_some_and(|ms| ms >= 1), "the next turn waits for the refresh, not {ms:?}");
        assert_eq!(pacer.timeout_ms(&ui, 0.01, true, true), Some(0), "queued messages still go at once");
    }
}
