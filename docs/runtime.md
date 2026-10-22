# novaui runtime standard

> **Binding** for novaui and for every app built on it (novaplayer first). It describes the code
> as it is: when the code and this document disagree, fix one of them in the same commit.

This document says how work enters novaui, in what order it is handled, who owns it, what memory
it may hold, and what everything is called. The goal is that novaui stays fast without anyone
having to remember why: the fast way is the only way the API offers, and the slow ways are caught
by the framework itself (debug checks and counters), not by review.

---

## 1. The rules, in one page (for contributors and reviewers)

1. **Idle is zero.** With nothing animating and nothing pending, the process makes no turns, no
   frames and no wakes. Each extra wake has a named owner that the `NOVAUI_TRACE` log shows.
2. **Every request has an owner and a handle.**
   - Timers, tickers, tasks and subscriptions all return a handle.
   - Dropping the handle cancels the request.
   - Nothing is ever scheduled anonymously.
3. **"Every frame while it moves" is a ticker, never a zero timer.** A running ticker is served
   once per display refresh. A timer is served at its time.
4. **Work runs because something happened to it,** never because a turn happened. No view polls a
   version every turn to find out whether it has work.
5. **One turn has one fixed order (§4),** and each phase has write rules (§5). Breaking a rule is
   a debug panic, not a convention.
6. **One clock per frame.** Every animation in a frame reads the same `frame_time`. Nothing
   animates by reading the wall clock.
7. **The cost of a frame follows what changed,** not how many elements exist. A frame that
   changed one element touches O(that element + its ancestors).
8. **No allocation on per-frame paths;** every cache states its limit and what it drops first.
9. **One word, one meaning (§2).** The words in §2 are reserved. Don't use them for anything else,
   and don't invent synonyms for them.

---

## 2. Vocabulary (reserved words)

| Word | Means exactly | Never use it for |
|---|---|---|
| **turn** | One pass of the host loop: collect what happened, dispatch it, maybe make a frame, sleep | a frame |
| **frame** | One display-paced draw: tick animations, layout if needed, paint, present | a turn, a view's per-turn work |
| **present** | Handing a finished frame to the swap chain | app or view work |
| **tick** | Advancing animations to `frame_time`, inside a frame | caret blink, drag step, a view's update |
| **timer** | An owned request to be woken at a time | "next frame" |
| **ticker** | An owned registration served once per frame, with that frame's `frame_time`, while it runs (`start` / `stop`; dropping it stops it) | a timer |
| **task** | Owned work off the UI thread; its result returns to its owner | a timer |
| **message** | A typed value the app defines and novaui delivers to `App::event` (`Event::Message`) | an input `Event` |
| **notify** | "This model changed": queues a delivery to its subscribers | bumping a counter nobody hears |
| **subscribe / subscription** | An owned registration for a model's notifications | polling a version |
| **version** | The only word for a change counter | — |
| **invalidate** | Mark one phase's output stale: `invalidate_layout`, `invalidate_paint` | "needs a save", app state |
| **needs_layout / needs_paint / needs_frame** | The questions the turn asks of the tree | — |
| **owner** | The view or model that holds a handle; its drop cancels what it holds | — |
| **turn_time / frame_time** | The time sampled once at the start of a turn / once at the start of a frame | — |

A platform API's own word stays where the code speaks that API: Wayland's event `serial`,
DXGI's dirty rectangles (`Present1`).
App-side words like "dirty" for "needs saving" become `unsaved` / `save_due`; an app's flag that
something it derives must be rebuilt on its next pass is `*_stale` (`lyrics_stale`). Names that describe
the product (side panel, track, layout, drag; `principles.md` N1) stay app vocabulary.

**Naming rules for new code:**
- A function named like a query (`changed`, `is_*`, `has_*`, `needs_*`) has no side effects. One
  that consumes state is called `take_*`.
- Verbs that schedule something start with `request_`, `schedule_` or `subscribe`, and return a
  handle.
- A handler is `on_<what happened>`.
- No abbreviations beyond `x y w h id ui dt` and loop indices. Fields, parameters and test helpers
  are words.

---

## 3. Kinds of work, and who owns them

| Kind | Created by | Served | Delivered as | Cancelled by |
|---|---|---|---|---|
| Input | the OS | in the next turn, in arrival order | `Event` to `App::event` | — |
| Timer | `ui.schedule(at, msg) -> Timer` | at the first turn at or after `at` (merged into one OS wait, §6) | `msg` | dropping the `Timer` |
| Ticker | `ui.ticker(msg) -> Ticker`, then `start` / `stop` | once per display refresh while running | `msg`, with `frame_time` | `stop`, or dropping it |
| Task | `ui.spawn(priority, work, on_done: msg) -> Task` | when the work finishes, on the UI thread, in phase 2 | `msg(result)` | dropping the `Task` (the result is discarded; queued work is dropped, running work stops if it checks its token) |
| Model change | `model.update(…)` / `notify` | the same turn, in the effects flush (phase 3) | the subscriber's `msg` | dropping the `Subscription` |
| Cross-thread wake | `ui.waker(msg) -> Waker`, then `wake` from any thread | the next turn, in phase 2 (wakes before it are one delivery) | `msg` | dropping every clone of the `Waker` |

**As built, `src/sched.rs`:**
- `ui.timer(msg)` makes an unarmed timer, which `Timer::at(t)` or `Timer::within(lo, hi)` arms
  again any number of times (`cancel` disarms it); `ui.schedule(t, msg)` makes one already armed.
- `ui.ticker(msg)` (`start` / `stop` / `run(bool)`). A ticker runs at most once per refresh
  period, at the turn's time, and is held while the window cannot draw.
- `ui.clock(msg) -> Clock`: an owner's time asked during a turn (`frame`, `at`, `within`,
  `cancel`) and settled by the framework at the end of every turn that ran the app, for every live
  clock: its ticker runs while frames are asked for and stops in the first turn none is; its
  timer is armed for the earliest deadline not yet served (one asked holds until served). An
  owner that asks per turn needs no list of its own to turn asks into handles.
- `ui.spawn(priority, work(&Cancel) -> R, done(R) -> msg) -> Option<Task>` (`work`'s `Cancel`
  tells it whether its `Task` was dropped):
  - two priorities, `Visible` before `Background`, on a small worker pool; when the system
    starts no worker thread, the UI thread runs the queued work, one item per turn, so every
    task still ends (and says so once on stderr);
  - `Background` is refused (`None`) when 256 jobs are queued, so the caller keeps the request
    and asks again later;
  - a finished task wakes the host through the platform's own wake, as a `Waker` does.
- `ui.spawn_waiting(work, done) -> Task`: work that waits rather than computes (the network,
  another process, the user) runs on a thread of its own, so it holds none of the pool's workers
  while it waits; always admitted, delivered and cancelled as `spawn`'s tasks are.
- `ui.queue(capacity) -> Queue`: serial fire-and-forget jobs on the same workers (one of the
  queue's jobs runs at a time, so decode-sized work does not fan out): `latest(f)` replaces the
  pending foreground job, `try_background(f)` is refused when `capacity` jobs wait. A job reports
  its result itself (a channel and a `Waker`); dropping the last `Queue` drops its pending jobs.
  The order a worker takes work in: `Visible` tasks, the queues' latest jobs, `Background` tasks,
  the queues' backlogs.
- `model.subscribe(msg) -> Subscription`. A subscriber is queued once per round however often
  the model changed.
- Delivery is `Event::Message(Rc<dyn Any>)`.
- Phase 2 order: input events; finished tasks (stopping at half a refresh of work, the rest
  next turn, with that turn runnable at once); due timers; tickers. Then the flush (phase 3,
  at most 8 rounds).
- `ui.waker(msg)`: a `Send` handle; `wake` delivers `msg` in the next turn. `platform::run`
  gives the app's constructor the `Ui` (`make: FnMut(&mut Ui) -> Box<dyn App>`), so the app
  makes its wakers there.
- Every wake is an owned handle, every reader of a model holds a subscription; the `Ui` holds
  weak references only, so a dropped handle simply disappears.

**Why messages, not closures:** a message is a plain value the app defines (Iced's model, §10).
Rust's borrow rules make closures that hold `&mut App` awkward, and a message names its owner by
its own variant. That makes routing explicit and greppable: `Msg::SidePanel(SidePanelMsg::SlideStep)`.

**How a message is stored:** `Ui` is not generic over the app's message type. A message is
type-erased once, when its handle is made (`Rc<dyn Any>`), and every delivery hands out the same
`Rc` (`Event::Message`); the app downcasts it to its own type in one place. A ticker that runs at
240 Hz therefore allocates nothing per frame. This is also why a ticker is a held registration
that is started and stopped (Flutter's `Ticker`), not a one-shot request asked again every frame:
a one-shot request would need a new message per frame.

**Owners hold handles in fields.** A view that closes drops its struct, and with it every timer,
ticker, task and subscription it held. There are no stale-result checks and no epoch fields.

---

## 4. One turn, in order

The order of a turn is novaui's, written once (`runner::turn`). A platform module (win32,
wayland, x11) only waits (for messages, wakers or the next deadline) and provides its parts of
the turn (`runner::Host`): its events, its window requests, cursor, IME, clipboard and pickers,
its edge, whether it may draw, its backend. The host's loop calls `turn`, and so may a callback
the OS runs inside its own loop (win32's modal size/move loop, `WM_PAINT`): a turn asked for
while one is running only leaves its messages queued for the running one.

**As built, `src/runner.rs`:**
- `Pacer` holds the refresh deadline.
- `turn` is the order, once for every host: the swap chain's wait, the turn's one clock
  reading, the host's events, `deliver` (phases 1–5), the window requests, cursor, IME,
  clipboard and pickers, then `draw` (phase 6) when the host allows it.
- `Host` is what a platform implements: its events, its requests, its cursor and IME, its
  clipboard, its edge, whether it may draw, its backend. It holds nothing of the order.

Each host keeps its own message pump and re-entry guard (win32's `step_now`) and calls `turn`.

```
turn (the swap chain waited for, unless the turn is live; then turn_time sampled once)
 1. collect     OS messages → input queue; finished tasks; cross-thread wakes; due timers
 2. dispatch    input events in arrival order (hit-tested against the last frame)
                → task results → due timers                         ── App::event(msg)
 3. flush       notifications queued by 2 (and by 3 itself) delivered FIFO,
                each subscriber at most once per round, until the queue is empty   ── App::event(msg)
 4. layout      only if needs_layout
 5. measure     after-layout handlers for views that asked to measure (§5)
                may invalidate layout once more; a second change in the same turn is a debug panic
 6. frame       only if (needs_paint or a ticker runs) and the display is ready:
                  frame_time = turn_time
                  6a running tickers delivered ── App::event(msg), at frame_time
                  6b tick: all tweens (Tree and Doc) to frame_time
                  6c layout if 6a/6b invalidated it
                  6d paint the invalidated elements; record damage
                  6e present (only if damage is non-empty)
 6f accessibility  a client that has the tree is sent the nodes this turn changed (novaui-spec,
                  "Accessibility"); its requests were taken in with 1's input
 7. sleep       until: OS message | waker | earliest live timer | next refresh deadline
                (that last one only while a ticker runs or paint is pending)
```

**Why the wait comes first.** The turn waits until the swap chain can take a frame before it
reads input and runs anything, which is how DXGI's frame-latency object is meant to be used.
The wait blocks only while a presented frame is still queued, and for at most one refresh.

After it, input, timers, the app's tweens and the frame all see one time: the time of the frame
they make. So `frame_time` and `turn_time` are the same reading.

A **live** turn does not wait:
- a size step, or any turn inside win32's modal size/move loop, where waiting would hold the
  border;
- a minimised window.

**The next refresh is a deadline, not a signal.** Most hosts have no "vblank happened" signal
without a present: Wayland's `wl_surface.frame` fires only after a commit, and DXGI's waitable
object says the present queue has room, which is already true after a turn that presented
nothing (a loop that waited on it spun). So the turn's `Pacer` computes the next refresh as the
last present time plus whole refresh periods (the platform's refresh rate), never "now"
(`Pacer`). When a frame presents, the swap chain paces the next one; when it draws nothing, the
deadline does.

**Properties this order guarantees:**
- **A turn that draws nothing ends in 7, and 7 never returns at once** unless something is
  actually pending: the loop cannot spin.
- **Everything a turn caused is settled before layout.** Notifications are delivered in this
  turn, not the next one. Events the app's handlers create (a field's `Edit` from a paste)
  are delivered in the next turn, which runs at once when the window can draw.
- **Input is hit-tested against what the user saw:** the last presented frame's hit list (§7).
- **Animations in one frame see one time.**
- **A ticker that stops stops costing.** An animation that has arrived calls `stop` (or its owner
  drops the ticker); nothing is served for it after that frame.

**There is no call per turn.** App work runs because something was delivered to it;
`App::after_layout` runs only in a turn that delivered the app something (and once after `init`).

---

## 5. What each phase may do

| Phase | May read | May write | Must not |
|---|---|---|---|
| 2 dispatch, 3 flush | models, last frame's geometry | models (→ notify), element properties (→ invalidate), handles | read layout of the current turn; block (no file or network I/O on the UI thread: use a task) |
| 4 layout | element properties | geometry (novaui only) | call app code |
| 5 measure | this turn's geometry | element properties of the asking view; its own state | notify models (that would restart 3). Changing layout twice is a panic. |
| 6a tickers | `frame_time`, geometry | animated properties of the owner's elements | change structure (add or remove elements); notify models |
| 6b–6e tick, layout, paint, present | — | novaui only | call app code |

**Enforcement:** the UI thread knows its current phase (`sched::Phase`: idle, dispatch, layout,
measure, frame); Chromium's `DocumentLifecycle` does the same (§10). The rules are contracts
(`src/audit.rs`): on in debug builds, and in any build with `NOVAUI_AUDIT=1` (CI's tests);
breaking one panics. As built:
- a model with subscribers changed in `after_layout` (phase 5 must not notify);
- `after_layout` laying the tree out more than once in a turn (§11.4);
- a turn, or `settle`, started inside a turn; `Ui::set_now` inside a turn;
- a frame drawn inside dispatch, layout or `after_layout`;
- the flush still pending after 8 rounds (a `debug_assert!`, §11.3);
- the **damage audit**: after each frame's paint, every live element the paint did not handle
  is recorded again and compared with what it shows, and its painted box with where it is. A
  difference is a change that reached an element without its flag (a setter that missed
  `PAINT` or `MOVE`).

### 5.1 What is atomic, and what is not

The atomic unit is the **turn**: everything delivered in it is applied before its frame, so no
frame shows half of a change. These are atomic:

1. **Everything one input event causes.**
2. **A model change and its subscribers' reactions** (the flush, phase 3).
3. **One layout.**
4. **One resize step:** the new size, its layout and its frame together, in step with the border
   (a live frame).
5. **Applying one task's result.** The delivery budget (§8) splits a batch of results
   across turns, never one result.

These are **not** atomic:

- **Animations.** An animation presents a decision; it is not state. The decision is atomic
  (a latch flips, a model changes, in the turn of its cause), and the animation is a value
  computed from `frame_time`.
  - It can be interrupted at any frame and retargets from its current value.
  - It never has to finish: input never waits for one, and neither does the app.
  - It never pauses: frames that were not drawn are skipped, not replayed, and the next frame
    shows the value for its own time.
  - The one thing that waits for an animation is the tree removing an element after its exit
    (`leaving`), and that is a deadline on the clock, not a completion event.
- **Streams of work** (a library scan, a 20 000-row tag fill) are spread over turns on purpose,
  in batches the UI can absorb.

### 5.2 Liveness: a running animation always gets frames

While a tween or a ticker runs and the window is visible, a frame is drawn at every display
refresh, whatever the input state (a held border, the modal size/move loop, a pointer capture).

Pacing signals (DXGI's frame-latency object, Wayland's frame callback) are hints. They may
delay a frame by at most a bounded time (34 ms on d3d11). They never hold frames until
something else happens. A frame that waits on a signal that was lost (the presents a
`ResizeBuffers` discarded) is the bug this rule names.

`NOVAUI_TRACE` counts the violations as `stalls`: turns with drawable work pending and no frame
for more than 34 ms while the window is visible, counted from the later of the last frame and
the turn the work became pending (a window that was quiet presented long ago; work that arrives
now waits only from now). The target is 0.

---

## 6. Time and timers

- **One clock reading per turn,** taken after the swap chain's wait (§4). It is `turn_time` for
  timers and `frame_time` for tweens (Tree and Doc), ticker handlers and scrolling. The tree's
  own deadlines are on the same clock, served at the start of the turn like timers:
  - an exit's end (`leaving`);
  - a settled resize's backend trim (`trim_at`).
- **No wall-clock reads in app code to drive anything visible.** The playback position is a
  model, updated by its owner.
- **Coalescing:** `Timer::within(lo, hi)` arms a timer for a window rather than an instant
  (`principles.md` R1); the host merges all live windows into one OS wait.
- **Cadences are owned timers,** for example the 10 Hz backdrop and the 1 Hz clock. They stop
  when their owner drops them, and the host shows them in the `NOVAUI_TRACE` log.
- **Minimised or occluded windows:** tickers are held, not served. Timers still fire. A
  muted animation keeps its time, like Flutter's `TickerMode` (§10).

---

## 7. Frame cost follows change

- **Per-phase invalidation lists:**
  - The tree keeps `needs_layout` roots and `needs_paint` elements as lists, so a frame walks
    only those.
  - Ancestors carry a "has invalid descendant" bit, so walks skip clean subtrees, like Masonry's
    pass flags (§10).
- **Hit testing** uses the hit list recorded during the last paint (gpui's hitboxes, §10), in
  reverse paint order. It is not a scan of all elements after each frame.
- **Draw lists are rebuilt when the structure changes** (an element added, removed or
  reordered): a walk of the tree. A long list therefore reuses its rows (`VirtualList`) rather
  than adding and removing them as it scrolls.
- **Moving a container costs one offset, not its subtree.** Transform and opacity skip layout,
  and a translated or scrolled element is an **offset group**: its descendants keep positions
  relative to it in their own draw list, like a layer without a texture, and `collect` and hit
  testing add the group's offset. Moving it re-positions one element and damages its old and
  new bounds (the browser's "compositor-only properties"). Text
  deferred while it was outside the group's clip is checked over the group's items when it
  moves (`reveal_text`).
- **Text and glyph caches** keep their entries across an atlas trim and re-upload only what is
  drawn. A full atlas evicts its page used longest ago and re-paints only the elements that
  showed glyphs from it; a start-over is left for a frame that needs the whole atlas.

**A target held by NovaPlayer's checks:** the 600-group lyric frame costs the same as the
30-group one when one line changes.

---

## 8. Memory and threads

- **Every cache states its limit and its eviction rule** in a doc comment. Covered: the
  glyph atlas, image cache, text layout cache, cover cache, row cache, and lyric layouts.
- **A texture lives while something shows it.** An image's texture goes the frame after its last
  handle does, or 10 s after the last shown element drawing it was hidden or removed while the
  app still holds the handle (a cache, a hidden page); an element that records it again uploads
  it again. Checked when an element drawing images is recorded or removed, and on the first turn
  after that deadline; never by a frame-by-frame scan, and never by a wake of its own (an idle
  window keeps what it holds until something else wakes it, R1).
- **No allocation on per-frame paths:** tick, paint, hit list, `lyric_layout`. Reuse buffers kept
  on the owner. Debug builds count allocations per frame (§9).
- **Big shared data is shared, not copied.** A list snapshot for a save holds the same songs
  (`Arc`), so a save never deep-copies 20 000 songs on the UI thread.
- **Background work goes through `ui.spawn`, `ui.spawn_waiting` or a `ui.queue`.** Threads that outlive their owner are not allowed;
  a thread that must outlive one window (the engine) is owned by the app and documented as such.
- **Threads block until told.** No timeouts used as polling: the engine, the decoder and the
  store writer all wait on their channels. Cross-thread news arrives through a `Waker` with a
  message.
- **Deliveries have a per-turn budget.** Task results and notifications beyond a time budget (half
  a refresh) wait for the next turn, so a burst (the 20 000-row tag fill) can't make one 88 ms
  turn. Deferred deliveries keep the next turn runnable at once (they are pending work, not a
  timer), so a budget never stalls them.
- **Tasks have a priority and the queue has a capacity.** What is on screen (visible rows, the
  current cover) goes before what is not; work for rows that scrolled away is dropped with its
  `Task`. A burst of background work is bounded by the queue, not by how fast the UI asks.

---

## 9. How this is enforced

Counters live in `Tree::stats` and the `Runner`, are written by `NOVAUI_TRACE`, and are checked
by the tests that the measuring scripts already drive:

| Counter | Must be | Catches |
|---|---|---|
| turns per second when idle | 0 | a leaked timer, a polling loop |
| turns that ran with nothing delivered | 0 | spin, an old `update` poll |
| frames with empty damage | 0 | presents with nothing new |
| flush rounds per turn | ≤ 8 (panic in debug above that) | notify cycles |
| layouts per turn | ≤ 2 | measure writing layout repeatedly |
| allocations per frame (debug) | 0 on a steady animation | hot-path allocation |
| elements visited per frame | O(invalidated) | whole-tree walks |
| wake owners | listed by the `NOVAUI_TRACE` log (like egui's `repaint_causes`, §10) | anonymous wakes |

**Review checklist for any change touching the runtime:**
- Which phase does the new code run in, and does §5 allow its writes?
- What owns each new timer, ticker, task and subscription, and does dropping it cancel?
- Does anything run every turn or every frame without a cause?
- Does a new cache state its limit?
- Are the §2 words used with their meaning only?
- Measure before and after (`NOVAUI_TRACE`, novaplayer's `tools/check.py`).

---

## 10. Prior art (what we take, and from where)

| Runtime | What we take | Source (fetched 2026-10) |
|---|---|---|
| **gpui** (Zed) | **Effect queue:** updates queue `Notify` / `Emit` effects, and `flush_effects` drains them FIFO when the outermost update ends, then schedules frames for invalidated windows. **Next frame:** `on_next_frame` creates "frame demand without dirtying the window", and `request_animation_frame` = next frame + notify. **Tasks:** `Task` drop cancels; `detach` lets it run on. | `crates/gpui/src/app.rs`, `window.rs` (zed main); docs.rs `gpui::Task` |
| **Flutter** | Named scheduler phases: idle → transient callbacks (animations) → mid-frame microtasks → persistent callbacks (build, layout, paint) → post-frame callbacks. `Ticker` calls once per frame; `TickerMode` mutes hidden subtrees. | api.flutter.dev `SchedulerPhase`, `Ticker` |
| **Jetpack Compose** | State reads are tracked per phase (composition, layout, drawing), and only the phases that read the changed state re-run. Moving a read later skips earlier phases. | developer.android.com `compose/phases` |
| **Browser** | `requestAnimationFrame` is one-shot, returns a request ID, and is cancelled by `cancelAnimationFrame`. All callbacks in one frame get the same timestamp. It pauses in background tabs. | MDN `requestAnimationFrame` |
| **Chromium** | `DocumentLifecycle`: explicit states (`kStyleClean`, `kLayoutClean`, `kPrePaintClean`, `kPaintClean` …), plus `DisallowTransitionScope` / `CheckNoTransitionScope` that forbid phase changes. Our phase checks in §5 take this approach. | `blink/renderer/core/dom/document_lifecycle.h` |
| **React** | Effects that must run before paint (`useLayoutEffect`) are the exception and "can hurt performance"; everything else runs after paint. Our measure phase (§4.5) is that exception, limited to one re-layout. | react.dev `useLayoutEffect` |
| **Signals** (TC39) | Push "dirty", pull values lazily and glitch-free; an effect's notify only schedules, and the framework decides when to run. Our models: notify queues, the flush in phase 3 runs. | `tc39/proposal-signals` README |
| **Masonry** (Xilem) | Passes run only when their invalidation flag is set, and flags propagate to ancestors. Rewrite passes loop until clean, with a fixed cap. Render passes are pure and may be skipped. | docs.rs masonry, pass system |
| **Slint** | A `Timer` stops when dropped. | docs.rs `slint::Timer` |
| **Iced** | Subscriptions identified by value: when the state stops returning one, the runtime kills it. Messages as the routing unit. | docs.rs `iced::Subscription` |
| **egui** (contrast) | Earliest-wins `request_repaint_after` ("only the smallest duration will be considered") was novaui's first wake model, replaced by owned handles. Even egui adds `repaint_causes()` to find who keeps repainting, which is our `NOVAUI_TRACE` log. | docs.rs `egui::Context` |

**What we do not take:**
- gpui's entity/lease system, Compose's compiler-driven recomposition, and React's reconciler.
  Each is a large machine for a problem novaui doesn't have: novaui's tree is retained and edited
  directly.
- Immediate mode (egui). It reruns everything per frame, which is the cost novaui's retained tree avoids.

---

## 11. Decisions

1. **Messages, not closures** (§3), type-erased once per handle so that delivery allocates
   nothing.
2. **No call per turn:** the app hears only what was delivered to it (§4).
3. **The flush cap is 8 rounds:** a debug build panics above it; a release build stops the
   flush for this turn and logs once.
4. **The measure phase allows one re-layout.** A second layout change in the same turn is a debug
   panic.
5. **Vocabulary as in §2,** with `ticker` in place of the one-shot "frame request".
