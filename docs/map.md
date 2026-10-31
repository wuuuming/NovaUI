# NovaUI map

What exists and where. It is facts only. A change that
adds, moves or removes a module or a mechanism updates this file in the same commit.

What the framework promises is in [`novaui-spec.md`](novaui-spec.md) (elements, layout, text,
`.nui`, backends) and [`runtime.md`](runtime.md) (turn order, ownership, budgets). Paths are under
`crates/novaui/src/` unless named otherwise.

## 1. Layers

```
app (implements App)            init · event(ui, Event) · after_layout · nc_hit
 └─ platform::run               host: win32.rs | wayland.rs | x11.rs (+ linux.rs shared)
     │  OS events → Input ──▶ Ui::input (input.rs, text_field.rs) ──▶ Event queue
     │  each turn: runner::begin → (host events) → runner::deliver → requests → runner::draw
     ├─ Ui (app.rs)             the app's handle: tree, window, events, focus, popups, sched, strings
     │   ├─ sched.rs            timers, tickers, model subscriptions, worker tasks, wakers, phases
     │   ├─ dsl.rs (+dsl/units) .nui sheets → Doc: elements, bindings, expressions, fragments
     │   └─ widgets/            Flyout, VirtualList, Scrollbar, TextEdit, switch, qr
     └─ Tree (element.rs)       elements, Taffy layout, positions, layers, groups, paint lists,
         │                      damage, tweens, exits, image lifetimes, hit testing
         ├─ text.rs             fonts, fallback, line layout, glyph atlas (also icons and paths)
         │   └─ text/shape.rs   runs by face, script and bidi level; harfrust shaping; breaks
         ├─ image/              ImageHandle interning, decode, resample, effects
         └─ render/             Backend trait; batch.rs (shared GPU encoding)
             ├─ d3d11.rs        Windows (DirectComposition or swap chain; image maker thread;
             │                  d3d11/timing.rs: GPU time per frame for the trace)
             ├─ vulkan.rs       Linux (Wayland, X11); not built for Windows; image maker thread
             └─ software.rs     CPU reference; X11 and Win32 fallback; headless tests

crates/novaui-shaders (build dependency only): prims.wgsl, the renderer's shader; naga → HLSL and
SPIR-V when novaui (build.rs) or an app with image effects (its build.rs, build_effect) is built
global       theme.rs (tokens), i18n.rs (Strings), font_scan.rs, image::Pool, image::effect table
```

Threads: the UI thread (host loop, `Ui`, `Tree`, every backend call); `novaui-task-{k}` workers
(1–4, made with the first `spawn`, `sched::Pool`), which run `Ui::spawn` jobs and nothing else;
the GPU backends' image maker `novaui-images` (textures of ≥ 256² made off the UI thread: D3D11
on its free-threaded device, Vulkan the image and a filled staging buffer); on Windows, during the
modal size/move loop, the vblank thread (`platform/win32_vblank.rs`) that posts one message per
refresh. A task that waits rather than computes (`Ui::spawn_waiting`; on Linux a file picker,
`kdialog`/`zenity`, a child process) runs on a `novaui-wait` thread of its own. An overlay
(`spawn_overlay`) is a UI thread of its own, `novaui-overlay`: its host loop, `Ui` and `Tree`,
with its own workers; the per-window state of the hosts is per thread.

## 2. Flows

**A turn** (`runner::turn`, `runtime.md` §4). The host blocks until input, a wake, or
`Pacer::timeout` (next timer, tick or waiting tick (`Clock::frame_at`), blink, exit end, trim, refresh after an undrawn
animation), to the microsecond: win32 on a high-resolution waitable timer (`LoopSleep`), X11
and Wayland in `ppoll`. Then `runner::turn` runs, calling the host's `runner::Host` parts in its one order:

1. Wait for the swap chain unless the turn is live (`Host::live`: resize step, modal loop,
   minimised); `ui.now`; `Tree::take_made_images`.
2. `Host::events` → `Ui::input` → state, hover, focus, gestures → `Event`s queued on `Ui`.
3. `runner::deliver` in `Phase::Dispatch`: deadlines (`Tree::serve_deadlines`), caret blink,
   queued events → `App::event`; then task results (budget: half a refresh), wakers, due timers,
   tickers as `Event::Message`; then `flush`: model subscribers, up to 8 rounds; then, if
   anything was delivered, `app::settle`: layout (`Tree::resolve_layout`, `Phase::Layout`) and
   `App::after_layout` (`Phase::Measure`, one relayout allowed).
4. Close ends the turn and the loop (`Host::close`); else the host carries out `Window` requests
   (minimise, maximise, fullscreen, title), cursor, IME state and position, clipboard out,
   clipboard in (a field's paste and the app's reads), pickers, frame stroke (`Host::edge`).
5. `runner::draw` → `Ui::frame` → `Tree::frame` (`Phase::Frame`): tick tweens; rebuild lists if
   the structure changed; layout/position; `paint` (record changed elements to `Draw` lists,
   damage old and new rects); audit (`NOVAUI_AUDIT`); free/upload images and atlas pages; draw
   layers innermost first, then the canvas per damage rect; present the damage.
   Only when `Host::may_draw` allows (Win32 holds modal-loop frames to their refresh).
6. Device lost → `Host::renew_backend`, `Tree::invalidate_all`.
7. Accessibility (`Host::access`): a client's requests were taken in after step 2's events; after
   the frame the bridge sends what changed (`access.rs`), once a client has asked.

The first frame, before the window shows, is `runner::start`: `init`, settle, edge, frame.

Events created while the app handles events wait for the next turn. That turn comes at once
when the window can draw (`Ui::has_work` counts queued events); while it is minimised or a frame
is held, it comes with the next input, wake or deadline (`Pacer::timeout`).

**A wake from another thread**: `Waker::wake` or a finished task → the hub's host closure
(`Ui::set_task_wake`) → Win32 `PostMessage(WM_APP_WAKE)`, Linux a pipe write → the loop wakes →
`Ui::woken` → `deliver`.

**A sheet**: `dsl::parse` → `Sheet` (styles, components, decls, roots) → `Doc::new` →
`Doc::instantiate` makes elements (`inst_at`, component expansion, `kind_defaults`, `apply` per
property). `Doc::set`/`set_in` change bindings; `Doc::update` (each turn the app calls it)
re-evaluates the `let`s, latches, tweens and the dynamic properties that read what changed
(`bind_index`), and re-applies `@key` texts when the locale changes.

**A fragment** (plugin controls, lyric styles): `dsl::parse_fragment(src, Profile, keys)` checks
bytes, kinds, properties, pure calls, string keys and the elements an instance makes (`expansion`,
which every sheet passes: nesting ≤ 64, no component inside itself) → `Doc::adopt` renames its
styles and components under `~mN` → `Doc::instantiate_scoped` mounts it with its own bindings →
`Doc::unmount` / `drop_adopted`.

**An image**: bytes → `image::decode(bytes, Fit)` (on a worker; coalesced by content in
`WeakCache`) → `image::rgba8` interns equal pixels to one `ImageHandle` → content of an element →
`Tree::use_images` → backend `image_upload` (or `image_make` on the GPU backend's maker thread for
large ones, D3D11 and Vulkan) → the texture is freed one frame after the last handle drops, or 10 s after nothing shows
it (`IMAGE_KEEP`).

**Text**: `Content::Text` → the string shaped once per weight (`Text::shaped`: runs, glyphs in
ems, break opportunities) → layout measures through `Text::measure` (no atlas) → `paint` calls
`Text::layout` (cached by `Key`; lines broken, each line's runs in visual order) → glyphs
rasterised into the 2048² atlas, 16 pages of 128 rows, least-recently-used page evicted;
elements on an evicted page repaint (`take_stale`); a full reset when nothing can be evicted.

## 3. Modules

| Module | Owns |
|---|---|
| `lib.rs` | public surface; re-exports `Align`, `Icon`, `corner`; `backend` feature exports renderer types |
| `app.rs` | `Ui` (the app's handle), `App` trait, `Event`, `Action`, `Popup`, `NcHit`, `settle`, headless `turn` |
| `runner.rs` | the turn: `Host` (a platform's parts), `start`, `turn`, `Pacer`, `deliver`, `flush`, `draw` |
| `sched.rs` | `Phase`; `Timer`, `Ticker`, `Subscription`/`Subscribers`, `Task`/`Cancel`/`Priority`, `Queue`, `Waker`, `Pool` (workers) |
| `model.rs` | `Model<T>`: a value with a version and subscribers |
| `input.rs` | `Input` (what hosts send), `Mods`, `DRAG_SLOP`; the pointer state machine; hover, press, drag, wheel, keys, popups' outside press |
| `focus.rs` | the keyboard focus on `Ui`: `set_focus`, Tab stops (a list one stop), keys for the focus (press, step a slider, move along a list), the focus visual's on/off, a popup's focus back to its anchor |
| `access.rs` | the accessibility tree (AccessKit): `Bridge` (a host's adapter and what the client has), nodes from elements, incremental updates from `Tree::access_touched`, requests as input; `Shared` / `Handler` for the adapter's threads |
| `semantics.rs` | `Role`, `Semantics` (role, label, focusable, `Range`): what an element is to the keyboard and to assistive clients |
| `element.rs` | `Tree`, `Id`, `El`, `State`, `Listen`, `FrameStats`; layout, positions, groups, layers, paint, damage, tweens, exits, images' lifetimes, hit tests |
| `style.rs` | `Layout`, `Visual`, `Content` and its parts, `Material`, `Shadow`, `Cursor`, `Units` |
| `geom.rs` | `Rect`, `Color`, `Damage`, `erf` |
| `anim.rs` | `Prop`, `Ease`, `Tween` (an eased value of time), `Glide` + `GlideSkin` (a scroll offset's motion and the wheel's speed), `Then`, `Pose`, `Anim` (an element's tween) |
| `dsl.rs`, `dsl/units.rs` | lexer, parser, `Sheet`, `Doc`, expressions (`Eval`), `Profile`/`parse_fragment`, property table (`apply`); per-unit text programs |
| `dsl/reload.rs` | debug builds: `Doc::watch`, live reload (each build made again aside from the old and the new sheet, merged three ways into the live elements) |
| `dev.rs` | debug builds: element labels and last damage (`TreeDev`), the inspector (F12) and the reload banner (`Dev`) |
| `text.rs` | font loading (bundle, `NOVAUI_FONTS`, the system's by family name), fallback order (per grapheme cluster), measure, line breaking and layout in visual order, carets, glyph atlas, icon and path marks, `FontMetrics` |
| `text/shape.rs` | `Shaper` (harfrust plans per face, script, direction), `Shaped` (glyphs in ems, runs, clusters with UAX #14 break opportunities, paragraphs' levels), bidi levels (UAX #9), `visual` (a line's runs in visual order, L2) |
| `font_scan.rs` | system font enumeration by name tables (`system_fonts`; also the default chain's source) |
| `vector.rs` | SVG path parsing (bounded) and A8 rasterising for `Path` |
| `image/mod.rs` | `ImageHandle`, interning `Pool`, `DROP_VERSION` counter |
| `image/decode.rs`, `cache.rs`, `resample.rs` | decode with `Fit` (and `load` for an app's re-encoding), both within 16384 px a side and 256 MB a decode, refused from the header; `WeakCache`, streaming triangle resampling |
| `image/effect.rs` | `Effect` registry: a GPU program (compiled at the app's build, `effect_shaders!`) and a CPU function per effect (≤ 32) |
| `image/gainmap.rs` | gain-map JPEG (Ultra HDR): find the gain map and its `hdrgm` metadata, the HDR rendition for a headroom, half floats |
| `theme.rs` | `Theme` tokens; `winui_dark`, `material3_dark`/`light`; process-global `install`/`current` |
| `i18n.rs` | `Strings`: tables per locale, fallback, `{name}` arguments, prefixes for plugins |
| `widgets/` | `Flyout` + `Place` (placement, enter/exit, rail), `VirtualList` (rows by place, reused; a carried place and rows giving way), `Scrollbar` (its expansion a `Tween`), `TextEdit` (`text_field.rs`), `switch`, `qr` |
| `trace.rs` | `NOVAUI_TRACE` per-second and per-turn lines, stall count, GPU time per frame (`Backend::gpu_frames`), counting allocator |
| `audit.rs` | `contract!` checks, on in debug or with `NOVAUI_AUDIT=1` |
| `error.rs` | `Error` (`Parse`, `Platform`) |
| `render/primitive.rs` | `Prim`, `Draw`, `Target`, `Backend` trait, Kawase sizes |
| `render/batch.rs` | the renderer's shader as built (`HLSL`, `spirv`), instance encoding shared by D3D11 and Vulkan |
| `render/d3d11.rs`, `vulkan.rs`, `software.rs` | the three backends |
| `platform/mod.rs` | `Window` and its requests, `Options`, `Bounds` (an ordinary window's kept place and size), `run` (Linux: Wayland, else X11); `OverlayOptions`, `Place`, `run_overlay`, `spawn_overlay` |
| `platform/win32.rs` (+`win32_vblank.rs`, `window_state.rs`) | Win32 host, file dialogs, clipboard, locale; working-set trims (spec §7.0a) |
| `platform/win32_overlay.rs` | Win32 overlays: the window, its place about its bottom centre, click-through and the pointer hook |
| `platform/win32_hdr.rs` | the window's output, HDR or SDR: SDR white and peak (`Backend::set_output`) |
| `platform/wayland.rs` | Wayland host (Vulkan only; falls back to X11 when it cannot open); input methods through `zwp_text_input_v3`; overlays as layer surfaces (`Layered`: margins, the host's own drag) |
| `platform/x11.rs` | X11 host (Vulkan or software `XPutImage`) |
| `platform/x11_overlay.rs` | X11 overlays: the ARGB window and its properties, the main display's work area, the place about its bottom centre, click-through (XFixes) and raw motion (XInput2) |
| `platform/xim.rs` | X11 input methods (XIM): over-the-spot at the caret, on-the-spot (preedit through callbacks) where that is all, server start and restart |
| `platform/linux.rs` | shared by both Linux hosts: wake pipe, keysym → VK, the input method's preedit to `Ui` (`Preedit`), KDE shadow tiles, caption press, dialogs and clipboard via child processes, locale, environment preparation |

Tests: unit tests sit beside their code — `src/internal_tests/` (backend resources, cache stats)
and small modules in `sched.rs`, `runner.rs`, `model.rs`, `text.rs`, `vector.rs`, `trace.rs`,
`widgets/text_field.rs`, `widgets/scroll.rs`, `image/`, `platform/`. The integration tests through
the public API with the `backend` feature (`damage_render`: pixels against a full redraw, per
backend; `input`; `layout_text`; `resources`) are maintained separately, named in §5 by file.

## 4. Mechanisms and the semantics they carry

A new semantic first looks here for the mechanism that can carry it (principles D2). Scope is
what a mechanism lives as long as; thread is where it runs.

| Mechanism | Where | Scope · thread | Semantics it carries | The part a semantic hands it |
|---|---|---|---|---|
| Turn | `runner::turn` over each host's `runner::Host`; each host's loop waits | the window · UI | input, wakes, timers, ticks, model flush, layout, frame in one order | — |
| Pacer | `runner.rs` `Pacer` | the window · UI | idle wait, refresh-paced frames, held frames in Win32's modal loop | a refresh rate |
| Phase | `sched.rs` `Phase`, `enter`, `laid_out` | the thread · UI | which calls are legal when (no notify in Measure, one relayout, no nested turn) | a contract |
| Timer | `sched.rs` `Timer` via `Ui::timer`/`schedule` | its handle · UI | a deadline, a coalesced window (`within`), cancel by drop | a message |
| Ticker | `sched.rs` `Ticker` via `Ui::ticker` | its handle · UI | per-refresh work while running (held while minimised) | a message |
| Clock | `sched.rs` `Clock` via `Ui::clock`; `Sched::settle_clocks` at the end of `app::settle` | its owner · UI | an owner's frames and deadlines asked per turn, turned into its ticker and timer by the framework for every live clock | a message |
| Model | `model.rs`, `sched.rs` `Subscribers` | its owner · UI | "this changed": subscribers once per flush round | a value |
| Task | `sched.rs` `Task`, `Pool` via `Ui::spawn`; `Ui::spawn_waiting` | its handle · worker (or a `novaui-wait` thread) → UI | off-thread work, cancel on drop, visible before background, bounded backlog (256); no worker: the UI thread runs one item per turn; work that waits on a thread of its own, holding no worker | a closure and its result |
| Serial queue | `sched.rs` `Queue` via `Ui::queue` | its handle · worker | one job at a time, latest foreground request wins, bounded backlog | a capacity |
| Waker | `sched.rs` `Waker`, host `HostWake` | its handle · any thread → UI | another thread's "look now" | a message |
| Element tree | `element.rs` `Tree` (`slot`: the one checked `Id` → element) | the window · UI | structure, layout (Taffy), state, the flags each setter invalidates; a dead id reads empty | `Layout`, `Visual`, `Content` |
| Offset group | `Tree::set_translate`/`set_scroll`, `group_moved` | an element · UI | scrolling, sliding, moves that re-record nothing | an offset |
| Layer | `Tree::set_promoted`, `Layer` | an element · UI | opacity and motion composited without re-raster; material plates | an element |
| Damage | `geom.rs` `Damage`, `Tree::damage_in`, `widen_for_materials` | a frame · UI | what to redraw and present (≤ 8 rects) | a rect |
| Tween | `anim::Tween` (a value of time: `at`, `done`, `retarget`); `Anim` = an element's prop + a `Tween`, `Tree::animate` | its holder · UI | an eased value from a to b over d s: element properties (opacity, X, Y, scale, scroll, fg alpha) through the tree; any other (a scrollbar's expansion, an app's motion) held by its owner, who asks for a frame while it is not done | a `Prop` or a holder, an `Ease` |
| Scroll motion | `anim::Glide` (`to`, `wheel`, `notch`, `hold`, `jump`) on a `GlideSkin` (`STANDARD`: 3 ms a dip, 80 to 400 ms, cubic ease-out; quick notches up to three; a new end mid-way taken on at the speed it has, `Ease::Carry`) | its holder · UI | every scrolled offset moves the same way: a wheel notch, a reveal, a page glide; a drag or a new content jumps; the holder reads `at(now)` and asks for frames while not `done` (`Flyout`'s rows; an app's lists and pages) | a holder, a `GlideSkin` |
| Enter/exit | `Tree::enter`/`exit`/`leaving`, `Pose` | an element · UI | appear and disappear motions; hide or keep at the end | a `Pose`, `Then` |
| Popups | `Ui::add_popup`/`remove_popup`, `input.rs` | the window · UI | outside press closes, anchor press swallowed | an element and anchor |
| Pointer gestures | `input.rs` `Pointer` | the window · UI | click, double click, drag with slop, capture, cancel, select in a field | listen flags |
| Accessibility tree | `access.rs` `Bridge` in each host (`runner::Host::access`); `Tree::access_touch` in the setters | the window · UI (adapters' handlers: their threads → a wake) | screen readers: nodes, names, values, focus, live regions; their presses, focus, values as input | `Semantics`, the elements' actions and text |
| Keyboard focus | `focus.rs` on `Ui`; `El::focusable`, the focus visual in `Tree::record` | the window · UI | Tab order, a list as one stop, Enter / Space press, a slider stepped (`Event::Value`), `Reveal`, popups holding Tab | `Semantics` (role, focusable, range) |
| Text editing | `widgets/text_field.rs` (caret x from `Text::carets`) | an edit element · UI | caret and selection by grapheme cluster, words, IME composition, clipboard, actions on edit/submit/cancel | an element |
| Hit test | `Tree::hit`, `hits` | a frame · UI | pointer target, hover chain, occlusion | a predicate |
| Text cache and atlas | `text.rs` `Text` | the tree · UI | shaped texts (per weight, in ems), paragraphs, measurements, glyphs, icons, vector paths; LRU pages | a string and style |
| Image interning | `image/mod.rs` `Pool`, `image/cache.rs` | the process · any | equal pixels share one handle and texture; same bytes decoded once | pixels |
| Image lifetime | `Tree::use_images`, `look_at_images`, `image_free` | the tree · UI | upload when shown, free when dropped or 10 s unshown | a handle |
| Effects | `image/effect.rs`, novaui-shaders `build_effect` (one effect per module, compiled at the app's build, made at a backend's next draw) | the process · UI | per-pixel image programs on GPU and CPU | WGSL + CPU fn |
| Backend | `render/primitive.rs` `Backend` | the window · UI | drawing `Draw` lists to canvas and layers, present, readback, loss | a primitive |
| Sheet bindings | `dsl.rs` `Doc` | a document · UI | `$binding`, `let`, latches, `tween()`, `window.*`, `@key` texts, selective re-evaluation | a binding name |
| Fragment profile | `dsl.rs` `Profile`, `parse_fragment`, `adopt`, `Sheet::reads` / `actions` | a fragment · UI | what a plugin's markup may name; its isolation from the host's | a profile |
| Units | `style.rs` `Units`, `dsl/units.rs` | a text element · UI | per-character/word motion, glow, timing (karaoke) | `u-*` programs |
| Theme tokens | `theme.rs` | the process | colours, sizes, type ramp, shadows, materials by name | a token |
| Strings | `i18n.rs` | the process | locale, fallback, prefixed tables per plugin | a key |
| Trace and audit | `trace.rs`, `audit.rs`, `FrameStats` | the process · UI | turn and frame costs, stalls, contracts | — |
| Host window requests | `platform/mod.rs` `Window`/`Requests`; each host | the window · UI | close, minimise, maximise, fullscreen, title; an overlay's size and click-through | a request |
| Overlay | `platform::run_overlay` / `spawn_overlay`, each host's overlay window | the overlay · its own thread (`novaui-overlay`) | a window over the others: transparent, never activated, sized by the app about its bottom centre, moved by its caption, its place kept by the app (`Place`, `Event::Moved`), click-through with hover | an `App`, `OverlayOptions` |
| Host clipboard | `Ui::io` `clipboard_out`/`paste_wanted`, `clipboard_read`; each host | the window · UI | a field's copy, cut, paste; the app's reads share the read | text |
| App clipboard and pickers | `Ui::read_clipboard`, `Ui::write_clipboard`, `Ui::pick` (`FilePick`); `HostIo`, each host's read, `serve_picks` | a request · UI → host (Linux pickers: a `novaui-wait` thread) | the app's paste, file choice: answered as the app's message, never waited for | a message |
| Locale | `platform::locale` | a call · UI | the user's language | — |
| Open | `platform::open` → each OS's `open_now` (ShellExecute, `xdg-open`) | a call · `novaui-open` thread | a URL in the browser, a folder in the file manager, a file in its app; never waited for | a path or URL |

## 5. Features → where → tests

| Feature | Where | Tests (here: unit tests; the rest maintained separately) |
|---|---|---|
| Incremental paint equals a full redraw | `element.rs` `paint`, `frame` | `damage_render/incremental.rs` (`incremental_equals_reference`, `vulkan_matches_reference`, `d3d11_matches_reference`) |
| Layers move without re-raster | `element.rs` layers | `damage_render/layer_motion.rs`, `layer_storage.rs`, `hidden_layer_idle.rs` |
| Clips, corners, frame stroke, expose | `element.rs`, `primitive.rs` | `damage_render/clip_reuse.rs`, `corners.rs`, `frame_stroke.rs`, `expose_present.rs` |
| Materials (acrylic) | `element.rs` record, backends' backdrop | `damage_render/materials.rs` |
| Performance budgets | whole frame | `damage_render/perf_core.rs` |
| Hover, press, drag, capture | `input.rs` | `input/hover_occlusion.rs`, `press_events.rs`, `pointer_release.rs` |
| Keyboard focus | `focus.rs`, `semantics.rs` | `input/keyboard_focus.rs` |
| Accessibility tree | `access.rs` | unit tests in `access.rs` |
| Switch, virtual list, scrollbar | `widgets/` | `input/switch_motion.rs`, `virtual_list.rs`; `scroll.rs` unit test |
| Text field | `widgets/text_field.rs` | unit tests in `text_field.rs`; `input/pointer_release.rs` (focus) |
| Text measure and layout | `text.rs` | `layout_text/text_measure.rs`; `text.rs` unit tests (atlas) |
| Shaping, bidi, scripts | `text.rs`, `text/shape.rs` | `text.rs` unit tests `scripts` (Arabic, Hebrew, Devanagari, Thai lines drawn from the atlas; carets) |
| Units (karaoke) | `style.rs`, `dsl/units.rs`, `element.rs` record | `layout_text/units.rs` |
| Fragments | `dsl.rs` `parse_fragment`, `adopt` | `layout_text/fragments.rs` |
| Layout barrier, window layout | `element.rs`, `dsl.rs` `set_window` | `layout_text/layout_barrier.rs`, `window_layout.rs` |
| Fonts | `text.rs`, `font_scan.rs` | `resources/fonts.rs` |
| Strings | `i18n.rs` | `resources/strings.rs` |
| Images: sharing, lifetime, decode | `image/`, `element.rs` | `internal_tests/image_sharing.rs`, `internal_tests/weak_cache.rs`, `internal_tests/resample_triangle.rs` |
| Serial queue | `sched.rs` `Queue` | `resources/serial_queue.rs` |
| Timers, tickers, models, tasks, pacing | `sched.rs`, `model.rs`, `runner.rs` | unit tests in those files |
| Paths | `vector.rs` | unit tests in `vector.rs`; `fragments.rs` (`a_path_draws_its_outline_in_its_colour`) |
| Win32 capture, minimised sizes, clipboard text | `platform/win32.rs`, `window_state.rs` | `win32_capture_tests.rs`, unit tests |
