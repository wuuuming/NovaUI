//! Owned work (`docs/runtime.md`, "Kinds of work"): timers, tickers, tasks and model subscriptions. Each is a
//! handle the owner keeps in a field. Dropping the handle cancels the work: nothing is delivered
//! for it after that, and nobody checks for stale results.
//!
//! What they deliver is a **message**: a value of the app's own type, type-erased once when the
//! handle is made (`Message`, an `Rc<dyn Any>`). Every delivery hands out the same `Rc`, so a
//! ticker running at 240 Hz allocates nothing per frame. The app hears `Event::Message` and
//! downcasts it to its own type in one place.
//!
//! The `Ui` keeps only weak references: a handle that is dropped is gone from the next scan.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::trace::Site;

/// What a timer, ticker, task or subscription delivers: the app's own message type, erased.
pub type Message = Rc<dyn Any>;

// ---- phases ---------------------------------------------------------------------------------

/// Where the UI thread is in its turn (`runtime.md`, "One turn, in order"). What app code may do
/// depends on it ("What each phase may do"); the checks are contracts (`audit`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Between turns: a host's own work, `App::init`, a headless driver between `turn`s.
    Idle,
    /// Phases 2–3: input, messages and the flush, delivered to the app.
    Dispatch,
    /// Phase 4: layout (novaui only).
    Layout,
    /// Phase 5: `App::after_layout`.
    Measure,
    /// Phase 6: the frame (novaui only).
    Frame,
}

thread_local! {
    static PHASE: Cell<Phase> = const { Cell::new(Phase::Idle) };
    /// Layouts run since the measure phase began (`runtime.md`, "Decisions": it allows one).
    static RELAYOUTS: Cell<u32> = const { Cell::new(0) };
}

pub(crate) fn phase() -> Phase {
    PHASE.with(Cell::get)
}

/// The UI thread is in `p` until the guard drops; then it is back where it was.
#[must_use]
pub(crate) struct InPhase(Phase);

impl Drop for InPhase {
    fn drop(&mut self) {
        PHASE.with(|c| c.set(self.0));
    }
}

pub(crate) fn enter(p: Phase) -> InPhase {
    if p == Phase::Measure {
        RELAYOUTS.with(|c| c.set(0));
    }
    InPhase(PHASE.with(|c| c.replace(p)))
}

/// A layout ran. The measure phase may lay out once more (a view placing itself against a box
/// laid out this turn); a second layout there is a view and the layout feeding each other.
pub(crate) fn laid_out() {
    if phase() == Phase::Measure {
        let n = RELAYOUTS.with(|c| {
            c.set(c.get() + 1);
            c.get()
        });
        crate::audit::contract!(
            n <= 1,
            "after_layout laid the tree out {n} times in one turn (runtime.md: the measure phase allows one)"
        );
    }
}

// ---- timers ---------------------------------------------------------------------------------

pub(crate) struct TimerSlot {
    /// The window it fires in (host clock, s): at the first turn at or after `lo`; the host
    /// sleeps no later than `hi`. `None`: not armed.
    due: Cell<Option<(f64, f64)>>,
    msg: Message,
    site: Site,
}

/// A timer: delivers its message once, at the first turn at or after its time. It can be armed
/// again (`at`, `within`). Dropping it cancels it.
pub struct Timer(Rc<TimerSlot>);

impl Timer {
    /// Arms (or re-arms) the timer for host time `t` (s).
    pub fn at(&self, t: f64) {
        self.0.due.set(Some((t, t)));
    }

    /// Arms the timer for any turn in [lo, hi]: it fires with whatever turn comes first in the
    /// window, and the host merges windows into one wait (`runtime.md`, "Time and timers").
    pub fn within(&self, lo: f64, hi: f64) {
        self.0.due.set(Some((lo, hi.max(lo))));
    }

    pub fn cancel(&self) {
        self.0.due.set(None);
    }

    pub fn armed(&self) -> bool {
        self.0.due.get().is_some()
    }

    /// When it fires next (`None`: not armed).
    pub fn due(&self) -> Option<f64> {
        self.0.due.get().map(|d| d.0)
    }
}

// ---- tickers --------------------------------------------------------------------------------

pub(crate) struct TickerSlot {
    running: Cell<bool>,
    msg: Message,
    site: Site,
}

/// A ticker: while running, delivers its message once per display refresh, at the frame's time
/// (`Ui::now`). An animation starts it, and stops it when it has arrived. Dropping it stops it.
/// While the window cannot draw (minimised), it is held, not served.
pub struct Ticker(Rc<TickerSlot>);

impl Ticker {
    pub fn start(&self) {
        self.0.running.set(true);
    }

    pub fn stop(&self) {
        self.0.running.set(false);
    }

    /// Starts or stops it.
    pub fn run(&self, on: bool) {
        self.0.running.set(on);
    }

    pub fn running(&self) -> bool {
        self.0.running.get()
    }
}

// ---- clocks ---------------------------------------------------------------------------------

/// What a clock was asked this turn, and the deadlines it holds.
#[derive(Default)]
struct ClockAsks {
    /// A frame was asked for this turn.
    frame: bool,
    /// This turn's asks, merged in when the clock is settled.
    ask_at: Option<f64>,
    ask_within: Option<(f64, f64)>,
    /// The earliest hard deadline not yet served.
    at: Option<f64>,
    /// A soft one: any time in the window (it may share a wake with another timer).
    within: Option<(f64, f64)>,
}

pub(crate) struct ClockSlot {
    asks: RefCell<ClockAsks>,
    ticker: Ticker,
    timer: Timer,
}

/// The owned time of one owner of time-driven work (a view, a save that waits, an animation):
/// during a turn it says what it needs (`frame`, `at`, `within`), and at the end of every turn
/// that ran the app the framework settles every live clock into its ticker and timer: the
/// ticker runs while frames are asked for and stops in the first turn none is; the timer is
/// armed for the earliest deadline not yet served. A frame is asked for in every turn that
/// wants one; a deadline is asked once and holds until it is served. What it delivers is its
/// message (`Event::Message`), named once, where it was made (`Ui::clock`). Dropping it stops
/// both.
pub struct Clock(Rc<ClockSlot>);

/// The earlier of a held deadline and a new one.
fn earliest(held: Option<f64>, t: f64) -> f64 {
    held.map_or(t, |x| x.min(t))
}

/// A handle's slot kept by the scheduler while its owner holds the handle: dead slots are
/// swept as a new one comes.
fn register<T>(slots: &mut Vec<Weak<T>>, slot: &Rc<T>) {
    slots.retain(|w| w.strong_count() > 0);
    slots.push(Rc::downgrade(slot));
}

/// Visits the live slots (`f`), forgetting the dropped ones.
fn each_live<T>(slots: &mut Vec<Weak<T>>, mut f: impl FnMut(Rc<T>)) {
    slots.retain(|w| match w.upgrade() {
        Some(s) => {
            f(s);
            true
        }
        None => false,
    });
}

fn earlier(a: Option<(f64, f64)>, b: Option<(f64, f64)>) -> Option<(f64, f64)> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if y.1 < x.1 { y } else { x }),
        (x, y) => x.or(y),
    }
}

impl Clock {
    /// Something moves: another frame after this one.
    pub fn frame(&self) {
        self.0.asks.borrow_mut().frame = true;
    }

    /// Look again at host time `t` (s).
    pub fn at(&self, t: f64) {
        let mut a = self.0.asks.borrow_mut();
        a.ask_at = Some(earliest(a.ask_at, t));
    }

    /// Look again any time in `[lo, hi]` (joins a nearby wake).
    pub fn within(&self, lo: f64, hi: f64) {
        let mut a = self.0.asks.borrow_mut();
        a.ask_within = earlier(a.ask_within, Some((lo, hi)));
    }

    /// Forgets every deadline asked so far, served or not: their reason is gone. Asks after it in
    /// the same turn stand.
    pub fn cancel(&self) {
        let mut a = self.0.asks.borrow_mut();
        a.at = None;
        a.within = None;
        a.ask_at = None;
        a.ask_within = None;
    }

    /// Its ticker runs: it asked for the next frame (after the turn settled it).
    pub fn ticking(&self) -> bool {
        self.0.ticker.running()
    }

    /// The deadline its timer is armed for (after the turn settled it).
    pub fn due(&self) -> Option<f64> {
        self.0.timer.due()
    }

    /// Turns this turn's asks into the ticker and the timer, at `now`.
    fn settle(&self, now: f64) {
        let mut a = self.0.asks.borrow_mut();
        self.0.ticker.run(std::mem::take(&mut a.frame));
        // The timer fired: the deadlines it served are gone (this turn ran their owner's work).
        if !self.0.timer.armed() {
            a.at = a.at.filter(|&t| t > now);
            a.within = a.within.filter(|&(lo, _)| lo > now);
        }
        if let Some(t) = a.ask_at.take() {
            a.at = Some(earliest(a.at, t));
        }
        a.within = earlier(a.within, a.ask_within.take());
        let timer = &self.0.timer;
        match (a.at, a.within) {
            (Some(t), Some((lo, _))) if t < lo => timer.at(t),
            (Some(t), Some((lo, hi))) => timer.within(lo, hi.min(t)),
            (Some(t), None) => timer.at(t),
            (None, Some((lo, hi))) => timer.within(lo, hi),
            (None, None) => timer.cancel(),
        }
    }
}

// ---- model subscriptions --------------------------------------------------------------------

pub(crate) struct SubSlot {
    /// Already queued in this round of the flush (a subscriber hears a model once per round).
    queued: Cell<bool>,
    msg: Message,
}

/// A subscription to a `Model`: its message is delivered in the same turn the model changes
/// (phase 3, the flush), once per round however many times the model changed. Dropping it
/// unsubscribes.
pub struct Subscription(#[allow(dead_code)] Rc<SubSlot>);

thread_local! {
    /// Subscribers whose model changed, not yet delivered (the flush's queue).
    static NOTIFIED: RefCell<Vec<Rc<SubSlot>>> = const { RefCell::new(Vec::new()) };
}

/// A model's subscribers.
#[derive(Default)]
pub(crate) struct Subscribers(Vec<Weak<SubSlot>>);

impl std::fmt::Debug for Subscribers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} subscribers", self.0.len())
    }
}

impl Subscribers {
    pub(crate) fn add(&mut self, msg: Message) -> Subscription {
        let slot = Rc::new(SubSlot { queued: Cell::new(false), msg });
        register(&mut self.0, &slot);
        Subscription(slot)
    }

    /// The model changed: queue each live subscriber once.
    pub(crate) fn notify(&mut self) {
        if self.0.is_empty() {
            return;
        }
        crate::audit::contract!(
            phase() != Phase::Measure,
            "a model with subscribers changed in after_layout (runtime.md: phase 5 must not notify models; it would restart the flush)"
        );
        NOTIFIED.with(|q| {
            let mut q = q.borrow_mut();
            each_live(&mut self.0, |s| {
                if !s.queued.replace(true) {
                    q.push(s);
                }
            });
        });
    }
}

/// Whether a model change waits for the flush.
pub(crate) fn notified() -> bool {
    NOTIFIED.with(|q| !q.borrow().is_empty())
}

/// The flush's next round: the subscribers notified since the last round, in order.
pub(crate) fn take_notified(out: &mut Vec<Message>) {
    NOTIFIED.with(|q| {
        for s in q.borrow_mut().drain(..) {
            s.queued.set(false);
            out.push(s.msg.clone());
        }
    });
}

// ---- tasks ----------------------------------------------------------------------------------

/// Which work a worker takes first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    /// For what is on screen now (the visible rows, the current cover): always admitted, run
    /// before any background work.
    Visible,
    /// Work that can wait: admitted while the background queue has room.
    Background,
}

/// Asked by running work: whether its `Task` was dropped (the result would be discarded).
#[derive(Clone)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

type Job = Box<dyn FnOnce() -> Option<Box<dyn Any + Send>> + Send>;

struct Queued {
    id: u64,
    cancel: Arc<AtomicBool>,
    job: Job,
}

#[derive(Default)]
struct Queues {
    visible: VecDeque<Queued>,
    background: VecDeque<Queued>,
    /// The serial queues (`Queue`), each with its own id.
    serial: Vec<(u64, Serial)>,
    next_serial: u64,
    closed: bool,
}

type Errand = Box<dyn FnOnce() + Send>;

/// A `Queue`'s jobs: at most one runs at a time, so the memory of decode-sized work is one job's.
struct Serial {
    latest: Option<Errand>,
    background: VecDeque<Errand>,
    capacity: usize,
    running: bool,
}

/// What a worker takes next.
enum Work {
    Task(Queued),
    Serial(u64, Errand),
}

impl Queues {
    /// Something waits to run (`next` would give it).
    fn has_work(&self) -> bool {
        !self.visible.is_empty()
            || !self.background.is_empty()
            || self.serial.iter().any(|(_, q)| !q.running && (q.latest.is_some() || !q.background.is_empty()))
    }

    /// Visible tasks, then each queue's latest job, then background tasks, then the queues'
    /// background jobs; a queue whose job is running is skipped.
    fn next(&mut self) -> Option<Work> {
        if let Some(t) = self.visible.pop_front() {
            return Some(Work::Task(t));
        }
        if let Some((id, q)) = self.serial.iter_mut().find(|(_, q)| !q.running && q.latest.is_some()) {
            q.running = true;
            return q.latest.take().map(|f| Work::Serial(*id, f));
        }
        if let Some(t) = self.background.pop_front() {
            return Some(Work::Task(t));
        }
        let (id, q) = self.serial.iter_mut().find(|(_, q)| !q.running && !q.background.is_empty())?;
        q.running = true;
        q.background.pop_front().map(|f| Work::Serial(*id, f))
    }
}

/// A finished task: its id and its result (`None` when it was cancelled before it ran).
type Done = (u64, Option<Box<dyn Any + Send>>);

struct Shared {
    queues: Mutex<Queues>,
    ready: Condvar,
    /// Finished work.
    done: Mutex<Vec<Done>>,
    /// `done.len()`, readable without the lock.
    done_n: AtomicUsize,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

pub(crate) struct TaskSlot {
    cancel: Arc<AtomicBool>,
    #[allow(clippy::type_complexity)]
    deliver: RefCell<Option<Box<dyn FnOnce(Box<dyn Any + Send>) -> Message>>>,
    site: Site,
}

/// Work off the UI thread. When it finishes, its result is turned into the app's message on the
/// UI thread and delivered (phase 2). Dropping the `Task` discards the result; queued work is
/// then skipped, and running work stops early if it asks its `Cancel`.
pub struct Task(Rc<TaskSlot>);

impl Drop for Task {
    fn drop(&mut self) {
        self.0.cancel.store(true, Ordering::Relaxed);
    }
}

impl Task {
    /// Whether the result is still to come.
    pub fn pending(&self) -> bool {
        self.0.deliver.borrow().is_some()
    }
}

/// The workers, made with the first task.
pub(crate) struct Pool {
    shared: Arc<Shared>,
    /// Workers that started. With none (the system refused every thread), the UI thread runs
    /// the work itself, one item per turn (`take_done`): slower, but every task still ends.
    workers: usize,
    next: u64,
    tasks: HashMap<u64, Weak<TaskSlot>>,
    background_capacity: usize,
}

/// How many background tasks may wait at once. A burst is bounded here, not by how fast the UI
/// asks (`runtime.md`, "Memory and threads").
const BACKGROUND_CAPACITY: usize = 256;

impl Pool {
    fn new(wake: Option<Arc<dyn Fn() + Send + Sync>>) -> Pool {
        let shared = Arc::new(Shared {
            queues: Mutex::new(Queues::default()),
            ready: Condvar::new(),
            done: Mutex::new(Vec::new()),
            done_n: AtomicUsize::new(0),
            wake: Mutex::new(wake),
        });
        // Leave a core for the UI thread and one for audio; at least one worker, at most four.
        let n = std::thread::available_parallelism().map_or(2, |n| n.get()).saturating_sub(2).clamp(1, 4);
        let mut workers = 0;
        for k in 0..n {
            let s = shared.clone();
            match std::thread::Builder::new().name(format!("novaui-task-{k}")).spawn(move || worker(&s)) {
                Ok(_) => workers += 1,
                Err(e) => eprintln!("novaui: worker thread {k} could not start: {e}"),
            }
        }
        if workers == 0 {
            eprintln!("novaui: no worker thread; tasks run on the UI thread");
        }
        Pool { shared, workers, next: 1, tasks: HashMap::new(), background_capacity: BACKGROUND_CAPACITY }
    }

    /// No worker runs the queued work: the UI thread must (`take_done`).
    fn inline(&self) -> bool {
        self.workers == 0 && self.shared.queues.lock().unwrap().has_work()
    }
}

fn worker(s: &Shared) {
    loop {
        let work = {
            let mut q = s.queues.lock().unwrap();
            loop {
                if q.closed {
                    return;
                }
                if let Some(w) = q.next() {
                    break w;
                }
                q = s.ready.wait(q).unwrap();
            }
        };
        run(s, work);
    }
}

/// Runs one item of work and posts a task's result; a worker's, or the UI thread's without one.
fn run(s: &Shared, work: Work) {
    let q = match work {
        Work::Task(q) => q,
        Work::Serial(id, f) => {
            // A job that panics is dropped; the queue's later jobs still run.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
            let mut q = s.queues.lock().unwrap();
            if let Some((_, serial)) = q.serial.iter_mut().find(|(i, _)| *i == id) {
                serial.running = false;
            }
            return;
        }
    };
    let result = if q.cancel.load(Ordering::Relaxed) {
        None
    } else {
        // A task that panics delivers nothing; later tasks still run.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(q.job)).ok().flatten()
    };
    s.done.lock().unwrap().push((q.id, result));
    s.done_n.fetch_add(1, Ordering::Release);
    if let Some(w) = s.wake.lock().unwrap().as_ref() {
        w();
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        let mut q = self.shared.queues.lock().unwrap();
        q.closed = true;
        let pending =
            (std::mem::take(&mut q.visible), std::mem::take(&mut q.background), std::mem::take(&mut q.serial));
        drop(q);
        self.shared.ready.notify_all();
        drop(pending);
    }
}

// ---- serial queues --------------------------------------------------------------------------

/// Fire-and-forget work on the pool's workers that runs one job at a time (`Ui::queue`): decode
/// -sized work whose memory must not fan out with the number of requests. A job reports its
/// result itself (a channel and a `Waker`); nothing is delivered for it.
///
/// One slot holds the latest not-yet-started foreground job (`latest`), and a bounded backlog
/// the rest (`try_background`). Cheap to clone and `Send`; when the last clone is dropped, the
/// pending jobs are dropped, and a running one finishes.
#[derive(Clone)]
pub struct Queue(Arc<QueueOwner>);

struct QueueOwner {
    shared: Arc<Shared>,
    id: u64,
}

impl Drop for QueueOwner {
    fn drop(&mut self) {
        let mut q = self.shared.queues.lock().unwrap();
        let gone = q.serial.iter().position(|(i, _)| *i == self.id).map(|k| q.serial.swap_remove(k));
        drop(q);
        // The jobs' captures drop outside the lock: their destructors are the app's.
        drop(gone);
    }
}

impl Queue {
    fn with<R>(&self, f: impl FnOnce(&mut Serial) -> R) -> Option<R> {
        let mut q = self.0.shared.queues.lock().unwrap();
        let id = self.0.id;
        q.serial.iter_mut().find(|(i, _)| *i == id).map(|(_, s)| f(s))
    }

    /// Replaces the pending foreground job (a job that already started finishes; its result
    /// is checked against what is current by whoever receives it). Never use it for work whose
    /// every side effect must happen (a save): a replaced job never runs.
    pub fn latest(&self, f: impl FnOnce() + Send + 'static) {
        let old = self.with(|s| s.latest.replace(Box::new(f)));
        self.0.shared.ready.notify_one();
        drop(old);
    }

    /// Queues `f` if the backlog has room, without blocking. On `false` the caller keeps the
    /// request and asks again later; nothing costly (file IO, a decoder) should come before it.
    pub fn try_background(&self, f: impl FnOnce() + Send + 'static) -> bool {
        let mut f = Some(f);
        let admitted = self
            .with(|s| {
                if s.background.len() >= s.capacity {
                    return false;
                }
                if let Some(f) = f.take() {
                    s.background.push_back(Box::new(f));
                }
                true
            })
            .unwrap_or(false);
        if admitted {
            self.0.shared.ready.notify_one();
        }
        drop(f);
        admitted
    }

    /// Jobs waiting (not the running one).
    pub fn pending(&self) -> usize {
        self.with(|s| s.background.len() + usize::from(s.latest.is_some())).unwrap_or(0)
    }
}

// ---- wakers ---------------------------------------------------------------------------------

pub(crate) struct WakeSlot {
    /// Woken since its message was last delivered.
    fired: AtomicBool,
}

/// What every waker of one `Ui` shares: whether any fired, and how to wake the host.
#[derive(Default)]
struct WakeHub {
    any: AtomicBool,
    host: std::sync::OnceLock<Arc<dyn Fn() + Send + Sync>>,
}

/// Another thread's way to reach the UI thread: `wake` delivers the waker's message there
/// (`Event::Message`) in the next turn. Wakes that come before the delivery are one delivery.
/// `Send` and cheap to clone; when the last clone is dropped, the registration goes with it.
#[derive(Clone)]
pub struct Waker {
    slot: Arc<WakeSlot>,
    hub: Arc<WakeHub>,
}

impl Waker {
    pub fn wake(&self) {
        if !self.slot.fired.swap(true, Ordering::AcqRel) {
            self.hub.any.store(true, Ordering::Release);
            if let Some(host) = self.hub.host.get() {
                host();
            }
        }
    }
}

impl std::fmt::Debug for Waker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Waker")
    }
}

// ---- the Ui's side --------------------------------------------------------------------------

/// The `Ui`'s registry of handles.
pub(crate) struct Sched {
    timers: Vec<Weak<TimerSlot>>,
    tickers: Vec<Weak<TickerSlot>>,
    clocks: Vec<Weak<ClockSlot>>,
    wakers: Vec<(std::sync::Weak<WakeSlot>, Message, Site)>,
    hub: Arc<WakeHub>,
    pool: Option<Pool>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Host time of the last ticker delivery.
    last_tick: f64,
}

impl Default for Sched {
    fn default() -> Self {
        Sched {
            timers: Vec::new(),
            tickers: Vec::new(),
            clocks: Vec::new(),
            wakers: Vec::new(),
            hub: Default::default(),
            pool: None,
            wake: None,
            last_tick: f64::NEG_INFINITY,
        }
    }
}

impl Sched {
    pub(crate) fn set_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        if let Some(p) = &self.pool {
            *p.shared.wake.lock().unwrap() = Some(wake.clone());
        }
        let _ = self.hub.host.set(wake.clone());
        self.wake = Some(wake);
    }

    pub(crate) fn waker(&mut self, msg: Message, site: Site) -> Waker {
        let slot = Arc::new(WakeSlot { fired: AtomicBool::new(false) });
        self.wakers.retain(|w| w.0.strong_count() > 0);
        self.wakers.push((Arc::downgrade(&slot), msg, site));
        Waker { slot, hub: self.hub.clone() }
    }

    /// The messages of the wakers woken since the last look.
    pub(crate) fn take_wakes(&mut self, out: &mut Vec<(Message, Site)>) {
        if !self.hub.any.swap(false, Ordering::AcqRel) {
            return;
        }
        self.wakers.retain(|(w, msg, site)| match w.upgrade() {
            Some(s) => {
                if s.fired.swap(false, Ordering::AcqRel) {
                    out.push((msg.clone(), *site));
                }
                true
            }
            None => false,
        });
    }

    pub(crate) fn timer(&mut self, msg: Message, site: Site) -> Timer {
        let slot = Rc::new(TimerSlot { due: Cell::new(None), msg, site });
        register(&mut self.timers, &slot);
        Timer(slot)
    }

    pub(crate) fn ticker(&mut self, msg: Message, site: Site) -> Ticker {
        let slot = Rc::new(TickerSlot { running: Cell::new(false), msg, site });
        register(&mut self.tickers, &slot);
        Ticker(slot)
    }

    pub(crate) fn clock(&mut self, msg: Message, site: Site) -> Clock {
        let ticker = self.ticker(msg.clone(), site);
        let timer = self.timer(msg, site);
        let slot = Rc::new(ClockSlot { asks: RefCell::new(ClockAsks::default()), ticker, timer });
        register(&mut self.clocks, &slot);
        Clock(slot)
    }

    /// Settles every live clock (`Clock`): the end of a turn that ran the app.
    pub(crate) fn settle_clocks(&mut self, now: f64) {
        each_live(&mut self.clocks, |c| Clock(c).settle(now));
    }

    /// The worker pool, made with the first task or queue.
    fn pool(&mut self) -> &mut Pool {
        let wake = self.wake.clone();
        self.pool.get_or_insert_with(|| Pool::new(wake))
    }

    pub(crate) fn queue(&mut self, capacity: usize) -> Queue {
        let pool = self.pool();
        let mut q = pool.shared.queues.lock().unwrap();
        q.next_serial += 1;
        let id = q.next_serial;
        q.serial.push((id, Serial { latest: None, background: VecDeque::new(), capacity, running: false }));
        drop(q);
        Queue(Arc::new(QueueOwner { shared: pool.shared.clone(), id }))
    }

    pub(crate) fn spawn<R: Send + 'static>(
        &mut self,
        prio: Priority,
        work: impl FnOnce(&Cancel) -> R + Send + 'static,
        deliver: impl FnOnce(R) -> Message + 'static,
        site: Site,
    ) -> Option<Task> {
        self.start(Some(prio), work, deliver, site)
    }

    /// A task whose work waits rather than computes (the network, another process, the user):
    /// it runs on a thread of its own, so it holds no worker of the pool while it waits.
    /// Delivered as `spawn`'s are. Should the thread not start, the pool runs it.
    pub(crate) fn spawn_waiting<R: Send + 'static>(
        &mut self,
        work: impl FnOnce(&Cancel) -> R + Send + 'static,
        deliver: impl FnOnce(R) -> Message + 'static,
        site: Site,
    ) -> Task {
        self.start(None, work, deliver, site).expect("a waiting task is always admitted")
    }

    /// Queues work by `prio`, or (`None`) starts a thread for it.
    fn start<R: Send + 'static>(
        &mut self,
        prio: Option<Priority>,
        work: impl FnOnce(&Cancel) -> R + Send + 'static,
        deliver: impl FnOnce(R) -> Message + 'static,
        site: Site,
    ) -> Option<Task> {
        let pool = self.pool();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = pool.next;
        let token = Cancel(cancel.clone());
        let job: Job = Box::new(move || {
            let r = work(&token);
            (!token.cancelled()).then(|| Box::new(r) as Box<dyn Any + Send>)
        });
        let item = Queued { id, cancel: cancel.clone(), job };
        let queued = match prio {
            Some(prio) => Some((prio, item)),
            None => {
                let held = Arc::new(Mutex::new(Some(item)));
                let (s, mine) = (pool.shared.clone(), held.clone());
                let started = std::thread::Builder::new().name("novaui-wait".into()).spawn(move || {
                    if let Some(item) = mine.lock().unwrap().take() {
                        run(&s, Work::Task(item));
                    }
                });
                match started {
                    Ok(_) => None,
                    Err(_) => held.lock().unwrap().take().map(|item| (Priority::Visible, item)),
                }
            }
        };
        if let Some((prio, item)) = queued {
            let mut q = pool.shared.queues.lock().unwrap();
            if prio == Priority::Background && q.background.len() >= pool.background_capacity {
                return None;
            }
            match prio {
                Priority::Visible => q.visible.push_back(item),
                Priority::Background => q.background.push_back(item),
            }
            drop(q);
            pool.shared.ready.notify_one();
        }
        pool.next += 1;
        let deliver: Box<dyn FnOnce(Box<dyn Any + Send>) -> Message> =
            Box::new(move |r| deliver(*r.downcast::<R>().expect("a task's result has its own type")));
        let slot = Rc::new(TaskSlot { cancel, deliver: RefCell::new(Some(deliver)), site });
        pool.tasks.insert(id, Rc::downgrade(&slot));
        Some(Task(slot))
    }

    /// Whether finished tasks or model changes wait to be delivered (the next turn runs at once).
    pub(crate) fn pending(&self) -> bool {
        notified()
            || self.hub.any.load(Ordering::Acquire)
            || self.pool.as_ref().is_some_and(|p| p.shared.done_n.load(Ordering::Acquire) > 0 || p.inline())
    }

    /// The earliest time an armed timer must be served by (its window's end).
    pub(crate) fn next_timer(&self) -> Option<f64> {
        self.timers.iter().filter_map(|w| w.upgrade()?.due.get().map(|d| d.1)).reduce(f64::min)
    }

    pub(crate) fn ticking(&self) -> bool {
        self.tickers.iter().any(|w| w.upgrade().is_some_and(|t| t.running.get()))
    }

    /// The timers due at `now`, disarmed, as messages (with their owners, for the trace).
    pub(crate) fn take_timers(&mut self, now: f64, out: &mut Vec<(Message, Site)>) {
        each_live(&mut self.timers, |t| {
            if t.due.get().is_some_and(|d| d.0 <= now) {
                t.due.set(None);
                out.push((t.msg.clone(), t.site));
            }
        });
    }

    /// The running tickers' messages, if a refresh passed since the last delivery (a turn
    /// woken by input between two refreshes does not tick them again).
    pub(crate) fn take_ticks(&mut self, now: f64, period: f64, out: &mut Vec<(Message, Site)>) {
        if now - self.last_tick < period * 0.5 {
            return;
        }
        let n = out.len();
        each_live(&mut self.tickers, |t| {
            if t.running.get() {
                out.push((t.msg.clone(), t.site));
            }
        });
        if out.len() > n {
            self.last_tick = now;
        }
    }

    /// Finished tasks' messages, until `budget` runs out; the rest wait for the next turn.
    pub(crate) fn take_done(&mut self, budget: std::time::Instant, out: &mut Vec<(Message, Site)>) {
        let Some(pool) = &mut self.pool else { return };
        if pool.workers == 0 {
            let work = pool.shared.queues.lock().unwrap().next();
            if let Some(w) = work {
                run(&pool.shared, w);
            }
        }
        if pool.shared.done_n.load(Ordering::Acquire) == 0 {
            return;
        }
        let mut done = std::mem::take(&mut *pool.shared.done.lock().unwrap());
        let mut k = 0;
        while k < done.len() && (k == 0 || std::time::Instant::now() < budget) {
            let (id, r) = (done[k].0, done[k].1.take());
            k += 1;
            let Some(slot) = pool.tasks.remove(&id).and_then(|w| w.upgrade()) else { continue };
            let (Some(r), Some(f)) = (r, slot.deliver.borrow_mut().take()) else { continue };
            if !slot.cancel.load(Ordering::Relaxed) {
                out.push((f(r), slot.site));
            }
        }
        let rest = done.split_off(k);
        let left = rest.len();
        if left > 0 {
            let mut d = pool.shared.done.lock().unwrap();
            let newer = std::mem::replace(&mut *d, rest);
            d.extend(newer);
        }
        pool.shared.done_n.fetch_sub(k, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use crate::app::{App, Event, NcHit, Ui};
    use crate::sched::Priority;
    use crate::Model;

    #[derive(Debug, PartialEq, Clone, Copy)]
    enum Msg {
        Timer,
        Tick,
        Changed,
        Done(u32),
    }

    #[derive(Default)]
    struct Log(Vec<Msg>);

    impl App for Log {
        fn init(&mut self, _: &mut Ui) {}
        fn event(&mut self, _: &mut Ui, ev: Event) {
            if let Event::Message(m) = ev {
                self.0.push(*m.downcast_ref::<Msg>().unwrap());
            }
        }
        fn nc_hit(&self, _: &Ui, _: i32, _: i32) -> NcHit {
            NcHit::Client
        }
    }

    fn turn(ui: &mut Ui, app: &mut Log, now: f64) -> Vec<Msg> {
        ui.now = now;
        crate::runner::deliver(ui, app, 1.0 / 60.0);
        std::mem::take(&mut app.0)
    }

    #[test]
    fn timers_fire_once_at_their_time_and_dropping_cancels() {
        let (mut ui, mut app) = (Ui::new(100, 100, 1.0), Log::default());
        let t = ui.schedule(1.0, Msg::Timer);
        assert_eq!(ui.next_wake(), Some(1.0));
        assert_eq!(turn(&mut ui, &mut app, 0.5), []);
        assert_eq!(turn(&mut ui, &mut app, 1.0), [Msg::Timer]);
        assert_eq!(turn(&mut ui, &mut app, 2.0), [], "fired once");
        t.at(3.0);
        drop(t);
        assert_eq!(ui.next_wake(), None, "a dropped timer is not waited for");
        assert_eq!(turn(&mut ui, &mut app, 4.0), []);
    }

    #[test]
    fn a_ticker_ticks_once_per_refresh_while_running() {
        let (mut ui, mut app) = (Ui::new(100, 100, 1.0), Log::default());
        let k = ui.ticker(Msg::Tick);
        assert_eq!(turn(&mut ui, &mut app, 1.0), []);
        k.start();
        assert!(ui.next_wake().is_some());
        assert_eq!(turn(&mut ui, &mut app, 1.0), [Msg::Tick]);
        assert_eq!(turn(&mut ui, &mut app, 1.001), [], "an input turn in the same refresh");
        assert_eq!(turn(&mut ui, &mut app, 1.0 + 1.0 / 60.0), [Msg::Tick]);
        k.stop();
        assert_eq!(turn(&mut ui, &mut app, 2.0), []);
        assert_eq!(ui.next_wake(), None);
    }

    /// A clock is settled at the end of every turn that ran the app: frames while they are asked
    /// for, a deadline held until it is served; dropping it stops both.
    #[test]
    fn a_clock_ticks_while_asked_and_holds_a_deadline_until_served() {
        let (mut ui, mut app) = (Ui::new(100, 100, 1.0), Log::default());
        let c = ui.clock(Msg::Tick);
        let _go = ui.schedule(0.0, Msg::Timer);
        c.frame();
        c.at(2.0);
        assert_eq!(turn(&mut ui, &mut app, 0.0), [Msg::Timer]);
        assert!(c.ticking(), "a frame was asked for");
        assert_eq!(c.due(), Some(2.0));
        assert_eq!(turn(&mut ui, &mut app, 1.0), [Msg::Tick]);
        assert!(!c.ticking(), "no frame asked in this turn");
        assert_eq!(c.due(), Some(2.0), "the deadline holds until it is served");
        assert_eq!(turn(&mut ui, &mut app, 2.0), [Msg::Tick]);
        assert_eq!(c.due(), None, "served");
        c.frame();
        drop(c);
        assert_eq!(ui.next_wake(), None, "a dropped clock is not waited for");
    }

    #[test]
    fn a_model_change_reaches_its_subscribers_once_in_the_same_turn() {
        let (mut ui, mut app) = (Ui::new(100, 100, 1.0), Log::default());
        let mut m = Model::new(0);
        let sub = m.subscribe(Msg::Changed);
        m.set(1);
        m.update(|v| *v += 1);
        assert!(ui.has_work(), "a pending notification runs the next turn at once");
        assert_eq!(turn(&mut ui, &mut app, 0.0), [Msg::Changed]);
        drop(sub);
        m.set(5);
        assert_eq!(turn(&mut ui, &mut app, 0.0), []);
    }

    #[test]
    fn a_task_delivers_its_result_and_a_dropped_one_does_not() {
        let (mut ui, mut app) = (Ui::new(100, 100, 1.0), Log::default());
        let keep = ui.spawn(Priority::Visible, |_| 7u32, Msg::Done).unwrap();
        let gone = ui.spawn(Priority::Background, |_| 8u32, Msg::Done).unwrap();
        drop(gone);
        let start = std::time::Instant::now();
        let mut got = Vec::new();
        while keep.pending() && start.elapsed().as_secs() < 5 {
            got.extend(turn(&mut ui, &mut app, 0.0));
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        got.extend(turn(&mut ui, &mut app, 0.0));
        assert_eq!(got, [Msg::Done(7)]);
        assert!(!ui.sched.pending());
    }

    /// Work that waits runs on a thread of its own: the pool's tasks finish while it waits.
    #[test]
    fn a_waiting_task_holds_no_worker() {
        let (mut ui, mut app) = (Ui::new(100, 100, 1.0), Log::default());
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let waiting = ui.spawn_waiting(
            move |_| {
                let _ = wait.recv();
                1u32
            },
            Msg::Done,
        );
        let tasks: Vec<_> = (0..8).map(|k| ui.spawn(Priority::Visible, move |_| 10 + k, Msg::Done).unwrap()).collect();
        let until = |ui: &mut Ui, app: &mut Log, f: &dyn Fn() -> bool| {
            let start = std::time::Instant::now();
            let mut got = Vec::new();
            while !f() && start.elapsed().as_secs() < 5 {
                got.extend(turn(ui, app, 0.0));
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            got
        };
        let got = until(&mut ui, &mut app, &|| tasks.iter().all(|t| !t.pending()));
        assert_eq!(got.len(), 8, "every pool task finished while the waiting one waits");
        assert!(waiting.pending());
        go.send(()).unwrap();
        assert_eq!(until(&mut ui, &mut app, &|| !waiting.pending()), [Msg::Done(1)]);
    }
}
