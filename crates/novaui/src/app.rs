//! `Ui`: the tree plus input state, focus, timers and requests to the host. Input is turned into
//! element states (hover, press) and app `Event`s here; the host only translates OS messages.

use crate::element::{Id, Tree};
use crate::input::{Mods, Pointer};
use crate::style::*;
use crate::widgets::TextEdit;
use std::collections::HashMap;

/// An element's `on-click` name, parsed once when it is set (at sheet load for `.nui`):
/// `name` up to the first `:`, `arg` the rest (`"menu:sort:title"` is `menu` with
/// `sort:title`; the app parses an argument further if it has parts).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Action {
    pub name: String,
    pub arg: String,
}

impl Action {
    pub fn parse(s: &str) -> Self {
        let (name, arg) = s.split_once(':').unwrap_or((s, ""));
        Action { name: name.to_owned(), arg: arg.to_owned() }
    }

    /// The action named `name`, without an argument.
    pub fn is(&self, name: &str) -> bool {
        self.name == name && self.arg.is_empty()
    }
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.arg.is_empty() {
            f.write_str(&self.name)
        } else {
            write!(f, "{}:{}", self.name, self.arg)
        }
    }
}

/// What the app hears. `action` is the element's `on-click` action, if it has one.
#[derive(Clone, Debug)]
pub enum Event {
    Click {
        id: Id,
        action: Option<Action>,
    },
    DoubleClick {
        id: Id,
        action: Option<Action>,
    },
    RightClick {
        id: Id,
        x: i32,
        y: i32,
    },
    /// Pressed outside every popup (the app closes menus).
    PressOutside {
        x: i32,
        y: i32,
    },
    Hover {
        id: Id,
        on: bool,
    },
    /// The left button went down on an element with `press-events` (`on`), or that press ended:
    /// released anywhere, or called off.
    Pressed {
        id: Id,
        on: bool,
    },
    /// Wheel over an element with `wheel-events`, in dips (positive = content moves up).
    Wheel {
        id: Id,
        dy: f32,
    },
    DragStart {
        id: Id,
        x: i32,
        y: i32,
    },
    Drag {
        id: Id,
        x: i32,
        y: i32,
    },
    DragEnd {
        id: Id,
        x: i32,
        y: i32,
    },
    /// A drag called off (a right press, Esc, or the window losing the keyboard): put back.
    DragCancel {
        id: Id,
    },
    /// The middle button pressed over an element with `wheel-events` (autoscroll's start).
    MiddleDown {
        id: Id,
        x: i32,
        y: i32,
    },
    /// The middle button released (anywhere).
    MiddleUp {
        x: i32,
        y: i32,
    },
    /// The pointer moved while the middle button is down (autoscroll's steering).
    MiddleMove {
        x: i32,
        y: i32,
    },
    /// A press on a slider-like element (`drag-events`) that did not become a drag.
    Press {
        id: Id,
        x: i32,
        y: i32,
    },
    /// A key went down: `vk` is an `input::key` code.
    Key {
        vk: u32,
        mods: Mods,
    },
    /// A text field's text changed. `action`: the field's `on-edit` (`TextEdit::action`).
    Edit {
        id: Id,
        action: Option<Action>,
        text: String,
    },
    /// Enter in a text field.
    Submit {
        id: Id,
        action: Option<Action>,
        text: String,
    },
    /// A text field lost focus.
    Blur {
        id: Id,
        action: Option<Action>,
    },
    /// Esc in a field with an `on-edit` action: the field has left focus (a `Blur` came first)
    /// and the key is the field's, not the window's.
    Cancel {
        id: Id,
        action: Action,
    },
    /// The keyboard or an assistive client sets a slider's value (`Semantics::range`): an arrow
    /// stepped it, Home / End took it to an end, a screen reader set it. The app applies it.
    Value {
        id: Id,
        value: f64,
    },
    /// The keyboard focus came to `id`, or an assistive client asked to see it: the app scrolls
    /// it into view where it owns the scrolling (a list, a page).
    Reveal {
        id: Id,
    },
    Files(Vec<std::path::PathBuf>),
    /// A timer, ticker, task, waker or model subscription's message (`sched`). The app downcasts
    /// it to its own message type.
    Message(crate::sched::Message),
    Resized,
    /// An overlay was moved (`Window::place` is where it is now), or an ordinary window moved,
    /// was resized, maximized or restored (`Window::bounds`).
    Moved,
    WindowFocus(bool),
}

/// A light-dismiss surface (a flyout, a menu) and the element it was opened from. A press
/// outside every popup raises `PressOutside`; when that press lands on (or inside) a popup's
/// anchor, it only dismisses: no `Click` follows, so pressing the button of an open flyout
/// closes it instead of reopening it (WinUI's light dismiss).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Popup {
    pub id: Id,
    pub anchor: Option<Id>,
}

/// Title-bar hit test answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NcHit {
    Client,
    /// Explicit interactive content overlapping the inner resize border (e.g. an edge rail).
    ClientControl,
    Caption,
    Minimize,
    Maximize,
    Close,
}

pub struct Ui {
    pub tree: Tree,
    /// Input and window events waiting for this turn's dispatch (phase 2).
    pub(crate) events: Vec<Event>,
    /// The host window: its handle and state, and what the app asks of it.
    pub window: crate::platform::Window,
    pub(crate) hovered: Vec<Id>,
    pub(crate) target: Option<Id>,
    pub(crate) gesture: Pointer,
    /// The middle button is down (see `Pointer`).
    pub(crate) middle_down: bool,
    /// `tree.removal_version` when dead ids were last dropped from the state below.
    pub(crate) seen_removals: u64,
    /// `tree.geometry_version` at the last hover refresh.
    pub(crate) seen_geometry_version: u64,
    pub(crate) pointer: Option<(i32, i32)>,
    /// The inspector and the reload banner (debug builds).
    #[cfg(any(debug_assertions, test))]
    pub(crate) dev: crate::dev::Dev,
    pub(crate) focus: Option<Id>,
    /// The focus last moved by the keyboard (or an assistive client), not by a press: the focus
    /// visual shows (`focus.rs`).
    pub(crate) keyboard_focus: bool,
    /// Whether the keyboard moves and uses the focus (`Ui::set_keyboard_navigation`).
    pub(crate) keyboard_navigation: bool,
    /// Text fields and what they and the host say to each other.
    pub(crate) io: HostIo,
    /// Popups: a press outside all of them raises `PressOutside`.
    pub(crate) popups: Vec<Popup>,
    /// The press that dismissed a popup from its own anchor: it clicks nothing (`Popup`).
    pub(crate) dismissing: bool,
    /// Timers, tickers and tasks (`sched`).
    pub(crate) sched: crate::sched::Sched,
    /// Messages to deliver this turn (kept, not allocated each turn).
    pub(crate) msgs: Vec<(crate::sched::Message, crate::trace::Site)>,
    /// `NOVAUI_TRACE`'s counters, when it is set.
    pub(crate) trace: Option<Box<crate::trace::Trace>>,
    /// The turn's one clock reading (`runtime.md`, "Time and timers").
    pub(crate) now: f64,
    /// Seconds between the display's refreshes (`refresh_period`).
    pub(crate) period: f64,
    /// A discontinuous viewport change; declarative layout transitions must settle immediately.
    pub(crate) snap_version: u64,
    pub(crate) last_blink: f64,
    /// The modifier keys held at the last pointer press (`Input::Mods`).
    pub(crate) press_mods: crate::input::Mods,
    pub(crate) cursor: Cursor,
    /// The app's cursor over all others (autoscroll's), while set.
    pub(crate) cursor_override: Option<Cursor>,
    /// The string tables and the active locale (`t`, `.nui` `@key`): the process's
    /// (`i18n::global`) unless replaced.
    pub(crate) strings: crate::i18n::Strings,
}

/// The text fields' state and the host protocol around them: the IME, the clipboard. The
/// fields write it, the host reads and answers it once per turn; no app code touches it.
#[derive(Default)]
pub(crate) struct HostIo {
    pub(crate) edits: HashMap<Id, TextEdit>,
    /// Caret rectangle for the IME's candidate window (physical client px: x, y, height).
    pub(crate) ime_at: Option<(i32, i32, i32)>,
    /// Text for the host's clipboard (Ctrl+C / Ctrl+X in a field).
    pub(crate) clipboard_out: Option<String>,
    /// Ctrl+V in a field: the host reads the clipboard (`clipboard_read`).
    pub(crate) paste_wanted: bool,
    /// The app's clipboard reads (`Ui::read_clipboard`), answered by the same read.
    pub(crate) clipboard_reads: Vec<ClipboardDone>,
    /// File pickers the app asked for (`Ui::pick`), opened by the host.
    pub(crate) picks: Vec<(FilePick, PickDone, crate::trace::Site)>,
    /// Pickers open on a thread of their own (Linux's child processes): answered as tasks.
    #[cfg(target_os = "linux")]
    pub(crate) waiting: Vec<crate::sched::Task>,
}

/// What turns the clipboard's text into the app's message.
pub(crate) type ClipboardDone = Box<dyn FnOnce(Option<String>) -> crate::sched::Message>;
/// What turns a picker's answer into the app's message.
pub(crate) type PickDone = Box<dyn FnOnce(Vec<std::path::PathBuf>) -> crate::sched::Message>;

/// A file picker the app asks the system for (`Ui::pick`). A filter is a name and its patterns
/// (`("Music", "*.mp3;*.flac")`).
#[derive(Clone, Debug, PartialEq)]
pub enum FilePick {
    /// Files to open; several when `multi`.
    Open { multi: bool, filters: Vec<(String, String)> },
    /// A folder to open.
    Folder,
    /// Where to save, proposing `name`; a name typed without an extension takes the first
    /// filter's.
    Save { name: String, filters: Vec<(String, String)> },
}

impl FilePick {
    /// The extension a saved name without one takes: the first filter's first pattern's, when
    /// that is a plain `*.ext`.
    pub(crate) fn save_extension(filters: &[(String, String)]) -> Option<&str> {
        filters.first().and_then(|(_, p)| p.split(';').next()?.strip_prefix("*.")).filter(|e| !e.contains(['*', '?']))
    }

    /// What the pick asks of a platform's dialogs: `open(folders, multi, filters)` for files or a
    /// folder (one), `save(name, filters)` for zero or one path to save to. The paths chosen.
    pub(crate) fn answer(
        &self,
        open: impl FnOnce(bool, bool, &[(String, String)]) -> Vec<std::path::PathBuf>,
        save: impl FnOnce(&str, &[(String, String)]) -> Option<std::path::PathBuf>,
    ) -> Vec<std::path::PathBuf> {
        match self {
            FilePick::Open { multi, filters } => open(false, *multi, filters),
            FilePick::Folder => open(true, false, &[]),
            FilePick::Save { name, filters } => save(name, filters).into_iter().collect(),
        }
    }
}

impl Ui {
    pub fn new(w: i32, h: i32, scale: f32) -> Self {
        Ui {
            tree: Tree::new(w, h, scale),
            events: Vec::new(),
            window: crate::platform::Window::default(),
            hovered: Vec::new(),
            target: None,
            gesture: Pointer::Idle,
            middle_down: false,
            seen_removals: 0,
            seen_geometry_version: u64::MAX,
            pointer: None,
            #[cfg(any(debug_assertions, test))]
            dev: Default::default(),
            focus: None,
            keyboard_focus: false,
            keyboard_navigation: true,
            io: HostIo::default(),
            popups: Vec::new(),
            dismissing: false,
            sched: Default::default(),
            msgs: Vec::new(),
            trace: crate::trace::Trace::from_env(),
            now: 0.0,
            period: 1.0 / 60.0,
            snap_version: 0,
            last_blink: 0.0,
            press_mods: Default::default(),
            cursor: Cursor::Arrow,
            cursor_override: None,
            strings: crate::i18n::global().clone(),
        }
    }

    /// The turn's time (s, host clock): one reading per turn, the same for every timer,
    /// animation and handler in it.
    pub fn now(&self) -> f64 {
        self.now
    }

    /// Seconds between the refreshes of the display the window is on (1/60 until a host says,
    /// and headless): a motion whose next visible step is further off than this asks for a
    /// frame then (`Clock::at`), not at every refresh (`Clock::frame`).
    pub fn refresh_period(&self) -> f64 {
        self.period
    }

    /// Sets `refresh_period` (a headless driver pacing its own frames).
    pub fn set_refresh_period(&mut self, secs: f64) {
        self.period = secs;
    }

    /// Sets the clock for the next turn. Hosts read the clock themselves; this is for headless
    /// drivers (tests, scripted scenarios), between turns.
    pub fn set_now(&mut self, t: f64) {
        crate::audit::contract!(
            crate::sched::phase() == crate::sched::Phase::Idle,
            "the clock set inside a turn (it is one reading per turn)"
        );
        self.now = t;
    }

    /// Resizes the window's tree and queues `Event::Resized` for the app (what a host does when
    /// the window's size or scale changed). False when nothing changed.
    pub fn resize(&mut self, w: i32, h: i32, scale: f32) -> bool {
        if (w, h, scale) == (self.tree.window_size().0, self.tree.window_size().1, self.tree.scale()) {
            return false;
        }
        self.tree.resize(w, h, scale);
        self.events.push(Event::Resized);
        true
    }

    /// Events waiting for the next dispatch.
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Takes the events waiting for the next dispatch (a headless driver delivering them
    /// itself, a test reading what input raised).
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// The modifier keys held as the pointer last went down (Shift+click, Ctrl+click): what a
    /// `Click` or `RightClick` was pressed with.
    pub fn press_mods(&self) -> crate::input::Mods {
        self.press_mods
    }

    /// Where the pointer is (physical client px), if over the window.
    pub fn pointer(&self) -> Option<(i32, i32)> {
        self.pointer
    }

    /// The hovered elements: the topmost hoverable element under the pointer, then those of its
    /// ancestors that hover. Elements it covers are not hovered.
    pub fn hovered(&self) -> &[Id] {
        &self.hovered
    }

    /// A text field's editor (`text_field`); `None` when `id` is not one.
    pub fn text_edit(&self, id: Id) -> Option<&TextEdit> {
        self.io.edits.get(&id)
    }

    /// Registers `id` as a light-dismiss surface opened from `anchor` (`Popup`).
    pub fn add_popup(&mut self, id: Id, anchor: Option<Id>) {
        self.popups.retain(|p| p.id != id);
        self.popups.push(Popup { id, anchor });
    }

    /// `id` is no longer a popup. A focus inside it returns to its anchor.
    pub fn remove_popup(&mut self, id: Id) {
        if let Some(at) = self.popups.iter().position(|p| p.id == id) {
            let popup = self.popups.remove(at);
            self.popup_closed(popup);
        }
    }

    pub fn is_popup(&self, id: Id) -> bool {
        self.popups.iter().any(|p| p.id == id)
    }

    /// The cursor the host shows.
    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// A cursor over every element's own (autoscroll's), shown at once; `None` gives the
    /// elements theirs back at the next pointer move.
    pub fn set_cursor_override(&mut self, c: Option<Cursor>) {
        self.cursor_override = c;
        if let Some(c) = c {
            self.cursor = c;
        }
    }

    /// The string tables and the active locale.
    pub fn strings(&self) -> &crate::i18n::Strings {
        &self.strings
    }

    pub fn strings_mut(&mut self) -> &mut crate::i18n::Strings {
        &mut self.strings
    }

    /// `key`'s text in the active locale (else the fallback locale's, else the key), with
    /// `{name}` replaced by the argument `name` (`i18n`).
    pub fn t(&self, key: &str, args: &[(&str, &str)]) -> String {
        self.strings.get(key, args)
    }

    /// Settle responsive layout tweens at their new target on the next document update.
    /// Resets document tweens, not the tree's independent content/page animations.
    pub fn snap_layout(&mut self) {
        self.snap_version = self.snap_version.wrapping_add(1);
    }

    /// Width (physical px) of `s` on one line at `size` dips and `weight`.
    pub fn text_width(&mut self, s: &str, size: f32, weight: u16) -> f32 {
        let k = self.scale();
        let st = crate::text::TextStyle { size: size * k, line: size * k * 1.4, weight };
        self.tree.text_mut().layout(s, &st, 0, 1, crate::text::Align::Start).ink_w as f32
    }

    /// The text's first family's measures (`text::FontMetrics`) at em size `size` (dips) and
    /// `weight`, in dips.
    pub fn font_metrics(&mut self, size: f32, weight: u16) -> crate::text::FontMetrics {
        let k = self.scale();
        let m = self.tree.text_mut().font_metrics(size * k, weight);
        crate::text::FontMetrics {
            ascent: m.ascent / k,
            descent: m.descent / k,
            cap_height: m.cap_height / k,
            x_height: m.x_height / k,
        }
    }

    pub fn scale(&self) -> f32 {
        self.tree.scale()
    }

    pub fn next_wake(&self) -> Option<f64> {
        // A running ticker wants the next frame: at once, paced by the swap chain (`Pacer`).
        // One that waits (`Clock::frame_at`) wants the frame at its time.
        let tick = self.sched.ticking(self.now).then_some(self.now);
        let waiting = self.sched.next_waiting_tick(self.now);
        [self.next_app_wake(), self.tree.trim_at(), self.tree.next_leave(), tick, waiting]
            .into_iter()
            .flatten()
            .reduce(f64::min)
    }

    /// App timers remain runnable while the native window cannot draw (tickers are held). A
    /// backend trim needs a frame, so its overdue deadline must not spin a minimized host.
    pub(crate) fn next_app_wake(&self) -> Option<f64> {
        [self.sched.next_timer(), self.next_blink()].into_iter().flatten().reduce(f64::min)
    }

    /// A timer, unarmed: `Timer::at` / `within` arm it. It delivers `msg` (`Event::Message`).
    #[track_caller]
    pub fn timer(&mut self, msg: impl std::any::Any) -> crate::sched::Timer {
        self.sched.timer(std::rc::Rc::new(msg), std::panic::Location::caller())
    }

    /// A timer armed for host time `t` (s).
    #[track_caller]
    pub fn schedule(&mut self, t: f64, msg: impl std::any::Any) -> crate::sched::Timer {
        let tm = self.sched.timer(std::rc::Rc::new(msg), std::panic::Location::caller());
        tm.at(t);
        tm
    }

    /// A clock (`sched::Clock`): an owner's frames and deadlines, asked during a turn and settled
    /// by the framework at its end; it delivers `msg` (`Event::Message`).
    #[track_caller]
    pub fn clock(&mut self, msg: impl std::any::Any) -> crate::sched::Clock {
        self.sched.clock(std::rc::Rc::new(msg), std::panic::Location::caller())
    }

    /// A ticker, stopped: `start` it to get `msg` once per frame.
    #[track_caller]
    pub fn ticker(&mut self, msg: impl std::any::Any) -> crate::sched::Ticker {
        self.sched.ticker(std::rc::Rc::new(msg), std::panic::Location::caller())
    }

    /// Runs `work` on a worker thread; when it finishes, `done(result)` makes the message
    /// delivered on the UI thread. `None` when `Background` work finds its queue full: the
    /// caller keeps the request and asks again later (a burst is bounded, not queued forever).
    #[track_caller]
    pub fn spawn<R: Send + 'static, M: 'static>(
        &mut self,
        prio: crate::sched::Priority,
        work: impl FnOnce(&crate::sched::Cancel) -> R + Send + 'static,
        done: impl FnOnce(R) -> M + 'static,
    ) -> Option<crate::sched::Task> {
        let site = std::panic::Location::caller();
        self.sched.spawn(prio, work, move |r| std::rc::Rc::new(done(r)) as crate::sched::Message, site)
    }

    /// Runs `work` that waits rather than computes (the network, another process, the user) on a
    /// thread of its own, so it holds none of `spawn`'s workers while it waits; delivered and
    /// cancelled as `spawn`'s tasks are, and always admitted.
    #[track_caller]
    pub fn spawn_waiting<R: Send + 'static, M: 'static>(
        &mut self,
        work: impl FnOnce(&crate::sched::Cancel) -> R + Send + 'static,
        done: impl FnOnce(R) -> M + 'static,
    ) -> crate::sched::Task {
        let site = std::panic::Location::caller();
        self.sched.spawn_waiting(work, move |r| std::rc::Rc::new(done(r)) as crate::sched::Message, site)
    }

    /// A serial queue on the same workers as `spawn`: fire-and-forget jobs, one at a time, with
    /// a latest-request slot and a backlog of `capacity` (`sched::Queue`).
    pub fn queue(&mut self, capacity: usize) -> crate::sched::Queue {
        self.sched.queue(capacity)
    }

    /// Asks for the clipboard's text: `done(text)` is delivered (`Event::Message`) in a later
    /// turn, `None` when it holds no text. The host reads it as it does for a field's paste, so
    /// the UI thread never waits on another process for it.
    pub fn read_clipboard<M: 'static>(&mut self, done: impl FnOnce(Option<String>) -> M + 'static) {
        self.io.clipboard_reads.push(Box::new(move |t| std::rc::Rc::new(done(t)) as crate::sched::Message));
    }

    /// Puts `text` on the clipboard (the host writes it at the end of this turn, as a field's
    /// Ctrl+C): a menu's 复制, a share link.
    pub fn write_clipboard(&mut self, text: impl Into<String>) {
        self.io.clipboard_out = Some(text.into());
    }

    /// What this turn put on the clipboard, taken: for a headless driver, which has no host to
    /// write it (as `Window::discard_requests` drops the window's requests).
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.io.clipboard_out.take()
    }

    /// Asks the system's file picker: `done(paths)` is delivered (`Event::Message`) once the user
    /// has answered, no paths when they cancelled. The window keeps turning while it is open
    /// where the platform allows (Linux: the picker is a child process; Windows' picker is modal
    /// to the window, as the system's own are).
    #[track_caller]
    pub fn pick<M: 'static>(&mut self, pick: FilePick, done: impl FnOnce(Vec<std::path::PathBuf>) -> M + 'static) {
        let site = std::panic::Location::caller();
        self.io.picks.push((pick, Box::new(move |p| std::rc::Rc::new(done(p)) as crate::sched::Message), site));
    }

    /// The host has a clipboard read to make (a field's paste or the app's).
    pub(crate) fn wants_clipboard(&self) -> bool {
        self.io.paste_wanted || !self.io.clipboard_reads.is_empty()
    }

    /// The host's read of the clipboard: a field's paste takes it, the app's reads get it as
    /// their messages.
    pub(crate) fn clipboard_read(&mut self, text: Option<String>) {
        if std::mem::take(&mut self.io.paste_wanted) {
            if let Some(t) = &text {
                self.paste(t);
            }
        }
        for done in std::mem::take(&mut self.io.clipboard_reads) {
            self.events.push(Event::Message(done(text.clone())));
        }
    }

    /// A waker: another thread's `wake` delivers `msg` (`Event::Message`) in the next turn.
    #[track_caller]
    pub fn waker(&mut self, msg: impl std::any::Any) -> crate::sched::Waker {
        self.sched.waker(std::rc::Rc::new(msg), std::panic::Location::caller())
    }

    /// Lets finished tasks, wakers and images made off the UI thread wake the host (each
    /// platform's own wake).
    pub(crate) fn set_task_wake(&mut self, wake: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.tree.image_wake = Some(wake.clone());
        self.sched.set_wake(wake);
    }

    /// Another thread woke the host (a waker or a finished task; delivered in `deliver`).
    pub(crate) fn woken(&mut self) {
        if let Some(tr) = &mut self.trace {
            tr.waker();
        }
    }

    /// A host turn starts (`NOVAUI_TRACE`'s counters).
    pub(crate) fn turn_begin(&mut self) {
        if let Some(tr) = &mut self.trace {
            tr.turn_begin(&self.tree);
        }
    }

    /// A host turn ends; `visible` is false while the window cannot show a frame (minimised).
    pub(crate) fn turn_end(&mut self, visible: bool) {
        if let Some(tr) = &mut self.trace {
            let animating = visible && (self.tree.has_work() || self.sched.ticking(self.now));
            tr.turn_end(&self.tree, animating);
        }
    }

    /// Waits until `be` can take another frame (the swap chain's pacing). The wait is not turn
    /// work: `NOVAUI_TRACE` counts it apart.
    pub(crate) fn wait_frame(&mut self, be: &mut dyn crate::render::Backend) {
        let Some(tr) = &mut self.trace else { return be.wait_frame() };
        let t = std::time::Instant::now();
        be.wait_frame();
        tr.waited(t.elapsed().as_micros() as u64);
    }

    /// Draws the tree's pending frame into `be` at `now`. Returns false when nothing was drawn.
    pub(crate) fn frame(&mut self, be: &mut dyn crate::render::Backend, now: f64) -> bool {
        let Some(tr) = &mut self.trace else { return self.tree.frame(be, now) };
        let (t, a) = (std::time::Instant::now(), crate::trace::ALLOCATIONS.load(std::sync::atomic::Ordering::Relaxed));
        tr.gpu_timing(be);
        let drew = self.tree.frame(be, now);
        let allocs = crate::trace::ALLOCATIONS.load(std::sync::atomic::Ordering::Relaxed) - a;
        tr.frame(&self.tree, drew, t.elapsed().as_micros() as u64, allocs);
        tr.gpu_frames(be);
        drew
    }

    pub fn has_work(&self) -> bool {
        self.tree.has_work() || !self.events.is_empty() || self.sched.pending()
    }

    /// All the work there is: running animations (`Tree::only_animating`).
    pub(crate) fn only_animating(&self) -> bool {
        self.events.is_empty() && !self.sched.pending() && self.tree.only_animating()
    }
}

/// What the app implements.
pub trait App {
    /// Builds the UI (called once, after the window exists).
    fn init(&mut self, ui: &mut Ui);
    /// Handles one event: input, or a message of one of the app's handles (`Event::Message`:
    /// a timer, ticker, task, waker or subscription). There is no call per turn: app work runs
    /// because something was delivered to it (`docs/runtime.md`, "The rules", rule 4).
    fn event(&mut self, ui: &mut Ui, ev: Event);
    /// The post-layout phase (`runtime.md`, "One turn, in order", phase 5): called once this turn's writes are laid
    /// out, in every turn that delivered the app something, and once after `init`. Geometry
    /// that depends on another element's laid-out box (a tip over a button, a list scrolled to
    /// a row's measured top) is read and written here, so no view needs its own layout barrier.
    /// Writes made here are laid out again with the frame (only what they touched). The default
    /// does nothing.
    fn after_layout(&mut self, ui: &mut Ui) {
        let _ = ui;
    }
    /// Title-bar hit test in physical client pixels.
    fn nc_hit(&self, ui: &Ui, x: i32, y: i32) -> NcHit;
}

/// Phases 4–5: the layout barrier, then `after_layout`. Hosts run it after `init` (before the
/// first frame) and in every turn that delivered the app something (`runner::deliver`).
/// `dispatch_us` is how long phases 2–3 took (`NOVAUI_TRACE`).
pub fn settle<A: App + ?Sized>(app: &mut A, ui: &mut Ui, dispatch_us: u64) {
    use crate::sched::{enter, phase, Phase};
    crate::audit::contract!(phase() == Phase::Idle, "settle inside a turn (app code ran it)");
    // Timed only when traced (`NOVAUI_TRACE`).
    let traced = ui.trace.is_some();
    let t1 = traced.then(std::time::Instant::now);
    ui.tree.resolve_layout();
    let t2 = traced.then(std::time::Instant::now);
    let measure = enter(Phase::Measure);
    app.after_layout(ui);
    drop(measure);
    // What the app's owners asked of their clocks this turn becomes their handles.
    ui.sched.settle_clocks(ui.now);
    if let (Some(t1), Some(t2), Some(tr)) = (t1, t2, &mut ui.trace) {
        let t3 = std::time::Instant::now();
        let us = |a: std::time::Instant, b: std::time::Instant| (b - a).as_micros() as u64;
        tr.update_phases(dispatch_us, us(t1, t2), us(t2, t3));
    }
}

/// One turn without a window (headless drivers, tests): phases 2–5 at `ui.now`, in the order
/// a host runs them (`runtime.md`, "One turn, in order"). Drawing the frame is the driver's.
pub fn turn<A: App + ?Sized>(app: &mut A, ui: &mut Ui) {
    crate::runner::deliver(ui, app, 1.0 / 60.0);
}
