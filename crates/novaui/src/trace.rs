//! `NOVAUI_TRACE=<file>`: the runtime's counters (`docs/runtime.md`, "How this is enforced"), one line per second of
//! turns, and one line for every turn slower than `SLOW_US`. Off (and costing one branch per
//! turn) unless the variable is set when the `Ui` is made.
//!
//! A second's line holds totals over that second (µs summed, not averaged), so a script can add
//! up any window of lines:
//!
//! ```text
//! t=12.001 turns=240 idle=0 input=0 waker=0 timer=0 tree=240 frames=240 empty=0 presents=240
//!   us_turn=.. us_update=.. us_layout=.. us_after=.. us_frame=.. us_present=.. us_wait=..
//!   positioned=.. moved=.. painted=.. prims=.. hits=.. hit_visits=.. layouts_max=1
//!   allocs=.. allocs_frame_max=.. wakes=views/x.rs:12=240,..
//! ```
//!
//! (one line in the file). With `NOVAUI_TRACE_TURNS=1` every turn also writes
//! `turn t=<start s> <cause> <drew|empty|-> wait=<µs> us=<µs> [<wake site>]`. A turn is counted under the first cause that applies: `input` (OS
//! input reached `Ui::input`), `waker` (another thread's `Waker`), `timer` (a due `Timer` or a running
//! `Ticker`, its call site counted under `wakes`), `tree` (pending tree work or animation), else `idle`: a turn
//! that had nothing to do, which must stay 0. `us_turn` excludes `us_wait`, the time spent waiting
//! for the swap chain to take a frame. Allocations are counted only when the app installs
//! `CountingAlloc` as its global allocator; otherwise `allocs` is absent.

use std::collections::HashMap;
use std::io::Write;
use std::panic::Location;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// A turn slower than this is written on its own line.
const SLOW_US: u64 = 4_000;

/// Allocations made in this process since start, when `CountingAlloc` is the global allocator.
pub static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static COUNTING: AtomicBool = AtomicBool::new(false);

/// The system allocator, counting allocations for `NOVAUI_TRACE`. An app opts in with
/// `#[global_allocator] static A: novaui::trace::CountingAlloc = novaui::trace::CountingAlloc;`
/// (a measuring build, not the product's default).
pub struct CountingAlloc;

/// An allocation is counted, and counting is known to be on.
fn counted() {
    ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
}

// SAFETY: every call goes to `System` with the caller's own arguments, so `System`'s
// guarantees are this allocator's; the counting is an atomic add and allocates nothing.
unsafe impl std::alloc::GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        counted();
        std::alloc::System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        std::alloc::System.dealloc(ptr, layout)
    }
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        counted();
        std::alloc::System.alloc_zeroed(layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        std::alloc::System.realloc(ptr, layout, new_size)
    }
}

fn allocations() -> u64 {
    ALLOCATIONS.load(Ordering::Relaxed)
}

/// Where a wake was asked for.
pub(crate) type Site = &'static Location<'static>;

#[derive(Default)]
struct Totals {
    turns: u64,
    idle: u64,
    input: u64,
    waker: u64,
    timer: u64,
    tree: u64,
    frames: u64,
    empty: u64,
    presents: u64,
    stalls: u64,
    us_turn: u64,
    us_update: u64,
    us_layout: u64,
    us_after: u64,
    us_frame: u64,
    us_present: u64,
    us_wait: u64,
    positioned: u64,
    moved: u64,
    painted: u64,
    prims: u64,
    layouts_max: u64,
    allocs: u64,
    allocs_frame_max: u64,
}

/// What the current turn has done so far.
#[derive(Default)]
struct Turn {
    inputs: u64,
    wakers: u64,
    timers: u64,
    tree_work: bool,
    layout_version: u64,
    allocs: u64,
    us_update: u64,
    us_layout: u64,
    us_after: u64,
    us_frame: u64,
    us_wait: u64,
    /// Whether this turn drew (presented) a frame.
    drew: Option<bool>,
    /// The first wake site served this turn.
    site: Option<Site>,
}

pub(crate) struct Trace {
    out: std::io::BufWriter<std::fs::File>,
    t0: Instant,
    window: Instant,
    started: Instant,
    turn: Turn,
    sum: Totals,
    wakes: HashMap<Site, u64>,
    hits: (u64, u64),
    /// `NOVAUI_TRACE_TURNS=1`: one line per turn as well.
    turns: bool,
    /// When the last frame was presented (`stalls`).
    last_present: Instant,
    /// The start of the first turn of the current run of turns that ended with drawable work
    /// pending (`stalls`); `None` when the last turn ended with none.
    pending_since: Option<Instant>,
}

impl Trace {
    /// The trace asked for by `NOVAUI_TRACE`, if any.
    pub(crate) fn from_env() -> Option<Box<Trace>> {
        let path = std::env::var_os("NOVAUI_TRACE")?;
        let file = std::fs::File::create(path).ok()?;
        let now = Instant::now();
        Some(Box::new(Trace {
            out: std::io::BufWriter::new(file),
            t0: now,
            window: now,
            started: now,
            turn: Turn::default(),
            sum: Totals::default(),
            wakes: HashMap::new(),
            hits: (0, 0),
            turns: std::env::var_os("NOVAUI_TRACE_TURNS").is_some(),
            last_present: now,
            pending_since: None,
        }))
    }

    pub(crate) fn turn_begin(&mut self, tree: &crate::element::Tree) {
        self.started = Instant::now();
        self.turn = Turn {
            tree_work: tree.has_work(),
            layout_version: tree.layout_version,
            allocs: allocations(),
            ..Turn::default()
        };
    }

    pub(crate) fn input(&mut self) {
        self.turn.inputs += 1;
    }

    pub(crate) fn waker(&mut self) {
        self.turn.wakers += 1;
    }

    pub(crate) fn timer(&mut self, site: Option<Site>) {
        self.turn.timers += 1;
        if self.turn.site.is_none() {
            self.turn.site = site;
        }
        if let Some(s) = site {
            *self.wakes.entry(s).or_default() += 1;
        }
    }

    pub(crate) fn update_phases(&mut self, update: u64, layout: u64, after: u64) {
        self.turn.us_update += update;
        self.turn.us_layout += layout;
        self.turn.us_after += after;
    }

    pub(crate) fn waited(&mut self, us: u64) {
        self.turn.us_wait += us;
    }

    pub(crate) fn frame(&mut self, tree: &crate::element::Tree, drew: bool, us: u64, allocs: u64) {
        let s = &mut self.sum;
        let f = &tree.last();
        s.frames += 1;
        s.empty += u64::from(!drew);
        s.presents += u64::from(drew);
        if drew {
            self.last_present = Instant::now();
        }
        s.us_frame += us;
        s.us_present += f.us_present;
        s.positioned += f.positioned;
        s.moved += f.moved;
        s.painted += f.painted;
        s.prims += f.prims;
        s.allocs_frame_max = s.allocs_frame_max.max(allocs);
        self.turn.us_frame += us;
        self.turn.drew = Some(drew);
    }

    /// `animating`: drawable work is pending at the end of the turn (tree work or a running
    /// ticker) and the window is visible. Such a turn is a stall when the work has waited more
    /// than 34 ms for a frame (`runtime.md`, "Liveness"; [`stalled`]).
    pub(crate) fn turn_end(&mut self, tree: &crate::element::Tree, animating: bool) {
        if stalled(&mut self.pending_since, animating, self.started, self.last_present, Instant::now()) {
            self.sum.stalls += 1;
        }
        let t = &self.turn;
        // The swap chain's wait is pacing, not work.
        let us = (self.started.elapsed().as_micros() as u64).saturating_sub(t.us_wait);
        let s = &mut self.sum;
        s.turns += 1;
        // A turn is counted under the first cause that applies.
        let (cause, count) = if t.inputs > 0 {
            ("input", &mut s.input)
        } else if t.wakers > 0 {
            ("waker", &mut s.waker)
        } else if t.timers > 0 {
            ("timer", &mut s.timer)
        } else if t.tree_work {
            ("tree", &mut s.tree)
        } else {
            ("idle", &mut s.idle)
        };
        *count += 1;
        s.us_turn += us;
        s.us_wait += t.us_wait;
        s.us_update += t.us_update;
        s.us_layout += t.us_layout;
        s.us_after += t.us_after;
        s.layouts_max = s.layouts_max.max(tree.layout_version - t.layout_version);
        s.allocs += allocations() - t.allocs;
        if self.turns {
            let site = t.site.map(|l| format!(" {}", site(l))).unwrap_or_default();
            let drew = match t.drew {
                Some(true) => "drew",
                Some(false) => "empty",
                None => "-",
            };
            let start = (self.started - self.t0).as_secs_f64();
            let _ = writeln!(self.out, "turn t={start:.5} {cause} {drew} wait={} us={us}{site}", t.us_wait);
        }
        if us > SLOW_US {
            let _ = writeln!(
                self.out,
                "slow t={:.3} us_turn={us} us_update={} us_layout={} us_after={} us_frame={}",
                self.t0.elapsed().as_secs_f64(),
                t.us_update,
                t.us_layout,
                t.us_after,
                t.us_frame,
            );
        }
        if self.window.elapsed().as_secs_f64() >= 1.0 {
            self.flush(tree);
        }
    }

    fn flush(&mut self, tree: &crate::element::Tree) {
        self.window = Instant::now();
        let s = std::mem::take(&mut self.sum);
        let hits = (tree.hit_tests.get(), tree.hit_visits.get());
        let (h, v) = (hits.0 - self.hits.0, hits.1 - self.hits.1);
        self.hits = hits;
        let mut wakes: Vec<_> = self.wakes.drain().collect();
        wakes.sort_by_key(|w| std::cmp::Reverse(w.1));
        let wakes: Vec<String> = wakes.iter().map(|(l, n)| format!("{}={n}", site(l))).collect();
        let allocs = if COUNTING.load(Ordering::Relaxed) {
            format!(" allocs={} allocs_frame_max={}", s.allocs, s.allocs_frame_max)
        } else {
            String::new()
        };
        let _ = writeln!(
            self.out,
            "t={:.3} turns={} idle={} input={} waker={} timer={} tree={} frames={} empty={} presents={} \
             us_turn={} us_update={} us_layout={} us_after={} us_frame={} us_present={} us_wait={} positioned={} \
             moved={} painted={} prims={} hits={h} hit_visits={v} layouts_max={} stalls={}{allocs} wakes={}",
            self.t0.elapsed().as_secs_f64(),
            s.turns,
            s.idle,
            s.input,
            s.waker,
            s.timer,
            s.tree,
            s.frames,
            s.empty,
            s.presents,
            s.us_turn,
            s.us_update,
            s.us_layout,
            s.us_after,
            s.us_frame,
            s.us_present,
            s.us_wait,
            s.positioned,
            s.moved,
            s.painted,
            s.prims,
            s.layouts_max,
            s.stalls,
            wakes.join(","),
        );
        let _ = self.out.flush();
    }
}

/// Whether a turn that ends at `now` is a stall: drawable work is pending (`animating`) and has
/// had no frame for more than 34 ms. The wait counts from the later of the last present and the
/// start of the run of turns that have ended with work pending (`pending`, kept here; such a run
/// begins at its first turn's `turn_start`). The last present alone is no deadline: a window
/// that was quiet presented long ago, and work that arrives now waits only from now.
fn stalled(
    pending: &mut Option<Instant>,
    animating: bool,
    turn_start: Instant,
    last_present: Instant,
    now: Instant,
) -> bool {
    if !animating {
        *pending = None;
        return false;
    }
    let since = *pending.get_or_insert(turn_start);
    now.saturating_duration_since(since.max(last_present)) > std::time::Duration::from_millis(34)
}

/// A source path from its crate's `src/` on (call sites from other crates are long).
/// A call site as the trace writes it: `file:line`.
fn site(l: Site) -> String {
    format!("{}:{}", short(l.file()), l.line())
}

fn short(file: &str) -> &str {
    let f = file.rsplit_once("src\\").or_else(|| file.rsplit_once("src/")).map_or(file, |(_, f)| f);
    f
}

impl Drop for Trace {
    fn drop(&mut self) {
        let _ = self.out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::stalled;
    use std::time::{Duration, Instant};

    /// Work that arrives after a quiet stretch waits from its own turn, not from the last
    /// present; work that then goes on waiting without a frame is a stall.
    #[test]
    fn stall_counts_from_when_work_became_pending() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let mut pending = None;
        // Quiet since the present at 0 ms.
        assert!(!stalled(&mut pending, false, at(50), at(0), at(51)));
        // Work arrives at 100 ms; its frame is held to the next refresh.
        assert!(!stalled(&mut pending, true, at(100), at(0), at(101)));
        assert!(!stalled(&mut pending, true, at(110), at(0), at(111)));
        // Still no frame 40 ms after it arrived.
        assert!(stalled(&mut pending, true, at(139), at(0), at(140)));
        // A frame at 141 ms; the animation goes on and is fed.
        assert!(!stalled(&mut pending, true, at(150), at(141), at(151)));
        // The frames stop.
        assert!(stalled(&mut pending, true, at(180), at(141), at(181)));
        // Work done; the next arrival starts over.
        assert!(!stalled(&mut pending, false, at(190), at(141), at(191)));
        assert!(!stalled(&mut pending, true, at(300), at(141), at(301)));
    }
}
