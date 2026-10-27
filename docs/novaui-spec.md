# NovaUI specification

What novaui does today: its modules, the frame, elements, primitives, text, the `.nui` language,
widgets, backends and platform hosts. It describes the code at the head of `main`;
when the two disagree, fix one of them in the same commit ([principles](https://github.com/wuuuming/NovaPlayer/blob/main/docs/principles.md), N3).

Companion documents:

- [`runtime.md`](runtime.md) — the runtime standard: vocabulary, turn order, phase rules, time,
  memory and threads, how it is enforced. Its rules are not repeated here.
- [NovaPlayer's `verification.md`](https://github.com/wuuuming/NovaPlayer/blob/main/docs/verification.md) — how to build and test.

The performance model: **work in each frame is proportional to what changed**, and idle is zero
frames.

## 1 Modules

`lib.rs` exports these; the rest is crate-private.

| Module | Owns | Does not own |
|---|---|---|
| `geom`, `style` (re-exported) | rects, colours, element styles and content | — |
| `app` | `Ui` (the tree, input state, the host `Window`), `App` (what an app implements), `Event`, `Action`, `Popup`, `NcHit` | app state |
| `sched` | owned time and work: `Timer`, `Ticker`, `Task`, `Queue` (serial jobs on the same workers: a latest-wins slot and a bounded backlog), `Waker`, `Subscription`, `Message`, `Priority`, `Cancel` | what the work is |
| `model` | `Model<T>`: app state declared to the framework (value, version, subscribers) | the state's meaning |
| `input` | hit testing, hover/press/capture, focus, keyboard, wheel, IME text (`Input`, `Mods`, `Pointer`) | OS messages |
| `semantics` | what an element is to the keyboard and to assistive clients: `Role`, `Semantics` (role, label, focusable, a slider's `Range`); the keyboard focus that reads it is `Ui`'s (crate-private `focus`, §6a'') | what the app's controls mean |
| `element` | elements (slot vector + generation-checked `Id`), styles, change flags, per-element display lists, layers, damage, Taffy layout | app state |
| `dsl` | `.nui` parser → element templates; bindings, expressions, keys and actions by name. A `Sheet` is opaque (its syntax tree is the parser's): `reads`, `actions`, `component_names`; a `Doc`'s elements by name (`get`, `id`, `names`) | app logic |
| `text` | font fallback chain, glyph atlas, shaped-run cache, line breaking, per-unit text | where text goes |
| `image` | `ImageHandle` (interned, freed on drop); encoded bytes to a handle (`decode(bytes, fit)`, fit `Within` or `Square`: JPEG/PNG/WebP/BMP, fitted with Triangle, premultiplied, shared by content while alive); image effects (`effect`); resampling and the decode cache inside | what an image is of |
| `anim` | curves (`Ease`), `Tween` (an eased value of time), `Glide` (a scroll offset gliding to where it is sent, `GlideSkin`), poses for enter/exit; property animations run on the tree (`Tree::animate`) | policy |
| `theme` | the token table the DSL resolves names in (`Theme`, `install`); presets: WinUI dark, Material 3 baseline dark / light (`md-sys-*` names) | which theme an app uses |
| `font_scan` (re-exported) | installed families (`system_fonts`, each file read only to its `name` / `OS/2` tables, once per process); `Tree::set_font(Some(family))` puts one ahead of the default order | which family an app offers |
| `i18n` | strings by key (`Strings`, `i18n::global()`, `t`): `key = text` tables per locale, owner namespaces (`add_under`, `replace_under`), `{name}` arguments, fallback active → `en` → the key (logged once); `platform::locale()` | the app's tables and its locale choice |
| `widgets` | `TextEdit` (`text_field`), `Flyout`, `VirtualList` (`list`), `Scrollbar` (`scroll`), `switch`, `qr` | what a row shows |
| `error` (re-exported) | `Error`: what a fallible call returns (`Parse { line, msg }`, `Platform`); a broken invariant inside novaui is a `debug_assert!`, not an `Error` | how the app reports it |
| `trace` | `NOVAUI_TRACE` runtime counters (`runtime.md` §9) | — |
| `platform` | `Window` (handle, state, requests), `run`, overlays (`run_overlay`, `spawn_overlay`, §7.2), `open` (a URL or a path, with the system's opener, off the UI thread), `locale`, per OS: window, custom caption, DPI, event loop, frame pacing, drag-drop, file pickers, clipboard | tree semantics |

Crate-private: `render` (`primitive` — the closed primitive set and `Backend`; `batch` — instances
and batches for the GPU backends; `software`, `vulkan`, `d3d11`), `runner` (the turn), `vector`
(`Shape` fills). With the `backend` feature, `novaui::backend` exports `Backend`, `Software`,
`Vulkan`, `D3d11` (Windows), the primitives `Draw`, `Prim` and `Target`, and `frame` for headless
drivers.

The app talks to `app`, `sched`, `model`, `element`, `dsl` and `platform`.

## 2 Frame

1. The host pumps messages. Input goes to `input`, which updates element states (hover/press) and queues actions. Hover goes to the topmost hoverable element under the pointer (`hit`, an action, `bg-hover`/`bg-press`, `hover-events`, …) and those of its ancestors that hover; elements it covers are not hovered.
   `Ui` keeps the input state (hover, capture, focus, popups, the cursor, the turn's clock) and the text fields' host protocol (IME, clipboard: `HostIo`) private; apps and tests read them through getters (`now()`, `focus()`, `hovered()`, `text_edit(id)`, `events()`, `strings()`) and change them through methods (`resize`, `set_now`, `add_popup`, `set_cursor_override`, `strings_mut`).
2. The app handles actions and sets properties or bindings. Every setter that changes something invalidates the element's layout, paint or position (move).
3. `anim` advances running animations, again through setters.
3a. The app runs only for what was delivered to it (`runtime.md` §4): input events and the
   messages of its own handles (timers, tickers, tasks, wakers, model subscriptions), then one
   layout barrier and `App::after_layout`, the post-layout phase, in every turn that delivered
   it something. There is no per-turn `update`. Whatever is placed against another element's
   laid-out box (a tip over a button, a marker over a laid-out row) is read and written there,
   so no view calls `resolve_layout` itself. Only layout-affecting writes require another layout
   in the frame; translation and paint-only geometry do not. Scrollbar track/thumb plates use
   stable absolute boxes with paint-only rectangles, so their range and hover changes do not
   invalidate page layout.
3b. App state that views present is declared as `Model<T>` (`model.rs`): a value and a version
   from one counter, changed only through `edit`/`set`; a view subscribes to the models it reads
   and hears the change in the same turn (the flush). Time is asked for with owned handles: a
   `Timer` (`at`, or `within(earliest, latest)`: a wake that may come early or late joins a
   nearby one, so a 10 Hz drift and a 1 Hz clock share wakes) and a `Ticker` while something
   moves every frame. Dropping a handle cancels it.
4. `layout` runs only when an element needs layout. Taffy's cache recomputes only the changed subtrees. Changed rects become MOVE (same size) or PAINT (new size).
5. `paint` re-records the display lists of PAINT elements (node-local, see MOVE). MOVE elements keep their list. Both add their old and new bounds to the damage of their layer. Text outside the inherited/render-target clip defers glyph preparation with a conservative ink-overhang guard. Deferred text is not runnable work; position/clip changes prepare it when it can re-enter. Layout still measures variable-height offscreen content when needed.
6. `render` re-rasters damaged regions of visible layers, composites damaged canvas regions, and presents only the damage. Hidden-layer damage is retained for reveal but is not runnable work. Window exposure is a separate presentation request: unchanged canvas pixels are re-presented without rasterization. Every new canvas is initialized over its full extent before incremental rendering begins.
7. If nothing needs layout or paint, nothing is animating and no ticker runs, the host sleeps until the next message, waker or the earliest armed timer. An unchanged window presents nothing: idle is zero frames (`principles.md` R1). A turn that drew nothing runs again no sooner than the next refresh (`runner::Pacer`): a drawn frame is paced by the swap chain, an undrawn turn by the refresh deadline.

The device is created with `D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS`: the driver submits on the UI thread instead of handing each frame to its own worker. A UI's few draws a frame cost less than the hand-off (measured at 240 fps: −22 % process CPU, a quarter fewer context switches).

Pacing: a flip-model swap chain with `SetMaximumFrameLatency(1)`, where the host waits on the frame-latency waitable object before it builds a frame (Microsoft's recommended low-latency pattern). Present then never blocks; measured present time went from 6.9 ms to 0.1 ms.

## 3 Elements

- Storage is a slot vector with generation-checked `Id`s and free-list reuse. Children are ordered vectors.
  Every public `Tree` entry goes from an `Id` to its slot through one check (`Tree::slot`): an id
  whose element was removed, even once its slot holds another, reads as an empty element (no
  parent, no children, default values) and changes nothing; `add` under it makes nothing and
  returns a dead id.
- The tree owns its elements: `El`, `Tree` and `Id` have no public fields. `Tree::el(id)` reads (`layout()`, `visual()`, `content()`, `tag()`, the states); every write is a `Tree` setter (`set_layout`, `set_visual`, `set_content`, `set_state(id, State, on)`, `set_listen`, `set_clear`, …) that knows what the change invalidates (`principles.md` R5).
- Styles are `Layout` (novaui's own: `Display`, `Position`, `FlexDirection`, `AlignItems`, `JustifyContent`, lengths; Taffy computes it and never shows) plus visual properties: background, border, radius, shadow, opacity, clip, text (string, text style, colour, alignment, max lines, wrap, ellipsis), icon, and image (texture, fit, radius). `corners: all | top | bottom | left | right | none` says which corners `radius` rounds (the rest stay square; `Visual::square`, `Prim::Quad::square`), for a header and its rows meeting square inside one rounded plate; it shapes the fill and border, while a material or shadow keeps all four round. `Visual::plate` optionally supplies a local DIP rectangle for the plate brushes without changing layout/content/hit geometry. `Tree::painted_rect` reports own painted window bounds after a frame; `rect` continues to report the layout/hit box.
- `layer: true` promotes an element to its own texture. The rule comes from Flutter's `RepaintBoundary` and Chromium's composited layers: content that changes independently of its surroundings goes on a layer, so a change in one does not re-raster the other. Opacity and translation animations of a promoted element touch only its composite, never its raster; a panel slide and a crossfade re-raster nothing.
- A scroll container is an offset group (or a layer) with a content offset. A virtualized list
  (`widgets::VirtualList`) keeps a screenful of rows (plus one either side), places them by
  translation and reuses a row that leaves the view for one that enters it: scrolling records
  nothing again. A place can be carried (a drag to reorder): its row stays live wherever it is,
  and the rows between it and where it would land give way, easing over 0.25 s on WinUI's
  fast-out curve (`GIVE_WAY`).
- A custom element supplies a `paint(&mut Recorder, size)` callback. An image drawn through an app's own program is an image effect (§4).

## 4 Primitives

The primitives are a closed set:

- `Quad` (rounded rect, border), `Shadow` (erf box shadow, now with a corner radius), `Glyph` (A8 atlas), `GlyphXf` (an A8 glyph through an affine map, bilinear, with a karaoke edge), `Layer` (with fade).
- `Image { rect, image, uv, radius, opacity }`: RGBA8 premultiplied texture, bilinear filtering, rounded clip.
- `Effect { rect, image, effect, params, opacity }`: an image drawn through a program the app registers (`image::effect::register`): a WGSL function added to the shader and compiled when the app is built (`novaui_shaders::build_effect` in its `build.rs`, `effect_shaders!`), and the same function in Rust for the software renderer. The element draws it with `Content::Effect`; animating it is changing `params`. An app's animated colour field is one: the GPU does the work, the CPU records one primitive.
- `Content::ImageCrossfade { from, to, mix }`: two `Image` primitives crossfading.
- `Backdrop { rect, radius, levels, extent, tint, lum, opacity }`: the acrylic material (`material: acrylic`). It reads what is already drawn under it and around it (`extent` px), runs a dual-Kawase blur on it (Bjorge, SIGGRAPH 2015: `levels` 5-tap downsamples and as many 8-tap upsamples, all at 8 bits), pulls the luminance toward `lum`, lays `tint` over that, then adds 2 % integer-hash noise, all in gamma space as Windows composes it. The values are WinUI's dark AcrylicInAppFillColorDefault: tint #2C2C2C at 0.15, luminosity at 0.96, blur 30. The element's shadow is drawn after the material and cut out under the plate, so the glass never samples its own shadow. Before the canvas pass, the tree widens every damage rect that touches a material's reach until it holds the whole reach. That way the material always blurs pixels from this frame, drawn earlier in the same draw call. `material: acrylic-flat` is the same colour and opacity with no blur and no noise (`levels` 0, `extent` 0): the pixels under the plate pulled to the luminosity and tinted, for a plate at rest over a backdrop that is already soft, where grain over a large fill would only irritate. A material reads the target it is drawn into: inside a promoted layer (`layer: true` on an ancestor) that is the layer's own surface, empty under it, so the plate comes out solid. Put the material on the layer itself, as a flyout does, or keep the ancestors unpromoted.

Every primitive has the same expression in WGSL (GPU) and in `render::software` (reference); an image effect brings both. The GPU/reference pixel test holds every GPU backend to it within 2/255.

## 5 Text

- Fonts are rasterised with `ab_glyph` and shaped with `harfrust` (HarfBuzz's Rust port), both
  over the same mapped file. Order: the bundled Noto variable fonts (`fonts/` beside the
  executable, or `NOVAUI_FONTS`): Noto Sans, then Noto Sans SC / JP / KR, then those of Noto Sans
  Arabic / Hebrew / Devanagari / Thai that are there; without the first four, the system's first
  Latin family (Segoe UI Variable, Segoe UI, Noto Sans, DejaVu Sans, Liberation Sans) and first
  CJK one (Microsoft YaHei UI, Noto Sans CJK SC, Noto Sans SC, Source Han Sans SC, WenQuanYi,
  Droid Sans Fallback); then every one installed of Yu Gothic, Malgun Gothic, Segoe UI, Segoe UI
  Symbol, Nirmala UI, Leelawadee UI, Segoe UI Emoji, Noto Sans Arabic / Hebrew / Devanagari /
  Thai, Noto Sans Symbols (2) and DejaVu Sans (a family already in the chain is not added
  twice). Families are found by name among the system's fonts (`system_fonts`' scan, once per
  process, read in parallel: about 30 ms on Windows), wherever their files are; a family's
  upright faces of normal width are used. Fallback is chosen per grapheme cluster (a letter and
  its marks, a conjunct, an emoji sequence): the first family with a glyph for each of its chars,
  default-ignorable ones (joiners, variation selectors) aside. No font at all is an `Error` from
  `platform::run`.
  An app's `Tree::set_font(Some(family))` goes first; the chain still supplies what it lacks.
- `Tree::text_lines(id, width)`: how many lines an element's text takes at a width, broken as
  its layout breaks it, from the text cache (a rule that grows for a two-line title reads it).
- Weights: a variable family is kept at every 50 from 400 to 700 (`text::WEIGHTS`), so a weight
  written as an expression gets a face of its own; static families give their nearest weight.
- Shaping (`text/shape.rs`): a text is cut into runs of one face, script (UAX #24; a char of no
  script takes its neighbours') and bidi level (UAX #9, each paragraph's direction from its first
  strong char; a text with nothing right-to-left skips the algorithm), and each run is shaped
  with the font's OpenType features: kerning, ligatures, Arabic joining, Indic reordering and
  conjuncts, mark placement. The result is in ems, cached per text and weight (bounded, two
  generations), so every size and width a text is laid out at reuses it.
- Paragraphs are cached by string, size/line metrics, weight, width, line limit and alignment.
  Intrinsic measurement has a bounded, atlas-independent metric cache and shares the
  line-breaking routine with painting, without generating positioned glyphs. Line breaking is
  greedy over clusters at UAX #14's opportunities (after spaces, between CJK characters, after
  hyphens; closing punctuation kept with what it closes); a cluster wider than the line breaks
  before it. A run cut by a break keeps the glyphs it was shaped with (a break inside a word of a
  joining script is a line too narrow for the word). Each line's runs are placed in visual order
  (rule L2); trailing spaces of a broken line are neither counted nor placed. Max lines and an
  ellipsis are supported, the ellipsis at the end of the line in its paragraph's direction, with
  left, centre and right alignment (alignment is the element's: a right-to-left paragraph in a
  left-aligned element starts at the left).
- `Text::carets(s, style)`: where a caret stands before each char of a one-line text, at its
  cluster's leading edge in the cluster's direction (a share of a cluster's width inside one of
  several chars). The text field places its caret, selection and composition by it, moves by
  grapheme cluster, and hits the cluster edge nearest the pointer (§6a).
- Offscreen paint culling uses cached curve extents of the actual faces, including ellipsis and
  outward pixel rounding. Paragraph emission keeps only glyph boxes intersecting the element's
  padded box. String/weight and glyph-extent caches are bounded and atlas-independent.
- Glyphs are rasterized at physical size into a 2048² A8 atlas cut into 16 pages (bands 128
  rows tall, each shelf-packed; a taller tile takes whole pages). When it is full the page used
  longest ago is evicted, preferring pages no element shows: its tiles and the paragraphs using
  them go, and the elements that showed glyphs from it are painted again in the same frame. A
  page used in the current frame is never evicted; only a frame that needs more than the whole
  atlas starts it over and re-records every text (`NOVAUI_TEXT_LOG` reports that).
- `Ui::font_metrics(size, weight)` gives the first family's measures in dips (ascent, descent,
  the capitals' and the lowercase's heights, from `H`'s and `x`'s ink); `cap_middle` is how far
  below a line box's middle the capitals' middle stands, where a symbol beside the text centres
  (Apple centres SF Symbols on the capitals).

## 6 The `.nui` language

A small declarative language for structure, layout and style. Logic stays in Rust. Example:

```
style title { font-size: 28; line: 36; weight: 700; color: text-primary; }

Column #face { align: center; gap: 16;
  Image #cover { width: 520; height: 520; radius: overlay; shadow: dialog; image: $cover; }
  Text #title { style: title; text: $title; lines: 2; align: center; }
  Button #play { icon: PlayFilled; size: 40; on-click: play; }
}
```

- `$name` is a binding. The app sets it with `ui.set("title", "…")`, and the framework updates only the elements that read it (through the same change flags).
- Layout that depends on the window is written in the sheet as expressions: `let`, `latch` (hysteresis: `enter` / `keep`), `tween(value, across, duration)` (eases a value across a flip of `across`), `window.w/h`, the math functions (`min max clamp pow exp lerp smoothstep smoothstep5 …`) and the operators `?:`, `|| &&`, comparisons, `+ - * /`. Any property can take an expression. On each `Doc::update` the declarations are evaluated in source order, but only when the window size or a binding changed, or while a tween is running; an expression property is re-applied only when its value changes. An app's whole size law (scales, latched breakpoints, blocks placed against each other) can live in its sheet; the app supplies only state (`$panel-open`, `$title-rows` …) and reads `doc.num("panel_x")` where Rust code needs a placed value.
- `@key` is a string by key (`text: @settings.title`, or `Text @settings.title`): resolved
  through `Ui::t` when applied, and wherever a value goes (a literal, a component argument, a
  binding's value `Val::Key`, an expression's result). When `ui.strings` changes (a table added,
  `set_locale`), the next `Doc::update` presents every keyed text again; nothing restarts. An
  `Edit`'s `hint: @key` is looked up once, when the field is made. `select-on-focus: true` makes
  focus (a press too) select the field's whole text, so typing replaces it (WinUI's editable
  ComboBox). An `Edit`'s `font-size`, `line` and `weight` are its text's (default 14 / 20), and an
  expression for them follows the window as any other property does. `on-edit: name` names the
  field's action: its `Edit`, `Submit` and `Blur` events carry it, and Esc in it is `Cancel`
  (the field's, not the window's). One word, one concept: `size` is a square box on any
  element, `font-size` a text's size, `icon-size` an icon's or a path's; a key that does not
  apply to the element is a load error. Fragments from outside the
  app (`parse_fragment(src, profile, keys)`) read the app's strings never: with `keys:
  Some(ns)` a fragment's `@title` reads `ns:title` (its owner's table, added with
  `Strings::add_under` or `replace_under`), with `None` `@key` is a load error. A fragment's
  profile bounds the work it causes, not only its text: `max_bytes` its source, `max_nodes` the
  elements one instance of any of its components makes with the components it names expanded.
  `Sheet::reads()` lists the bindings a sheet reads anywhere (expressions and styles included),
  `Sheet::actions()` what its presses send: what an app checks a fragment against.
  A fragment writes its `on-click` out (a binding or an expression is refused: it could name
  any action), and a mount prefixes every `on-click` it takes in, its styles' and the host
  styles' it was offered included.
  Every sheet, a fragment or the app's own, is refused when elements or expressions nest more
  than 64 deep (in the source or once components are expanded) or a component contains itself.
- `on-click: name` sends the action `name` (with the element id) to the app.
- One name per property: `shown: false` takes an element out of the layout (`visible: false`
  only stops painting it), `fg` is its ink, `disabled` disables its subtree.
- Paint-only text and motion properties: `fg-alpha` (the fg's alpha alone), `translate: x, y`
  (moves the element as drawn, layout keeps its place), and the karaoke sweep: `sweep` = chars
  sung (fractional; negative = none), `sweep-fg` / `sweep-alpha` = the sung chars' colour. The
  glyphs before the sweep draw in the sung colour, those after it in the fg, and the glyph being
  sung is cut into one-pixel atlas columns blending the two over a soft edge (0.6 of the glyph's
  width) that enters at its left as the char starts and leaves at its right as it ends. It works
  across wrapped lines and on every backend, since the columns are ordinary `Glyph` primitives.
- A text drawn per unit (karaoke words, subtitles): `units: word | char | none`, `timing:
  [first char, from, to, …]` (seconds; a span lasts to the next one's first char) and `time`
  (seconds, the time now). Words are cut where lines break (spaces, around CJK, closing
  punctuation kept with what it closes), chars are every non-space char; a span cut into several
  units shares its time by its chars (spaces take none). `u-translate: x, y` (dips), `u-scale`
  (`s` or `sx, sy`) and `u-rotate` (degrees clockwise), both about the unit's centre,
  `u-opacity`, `u-fg` and `u-glow: radius, colour[, strength]` (a halo under the unit: its
  glyphs blurred once per whole-px radius and kept in the atlas, drawn under every glyph of the
  text; animate its strength, not its radius) are expressions evaluated once per unit,
  reading `unit.i unit.n unit.t0 unit.t1 unit.dur unit.age unit.p unit.hash unit.start unit.end`
  (`p`: <0 waiting, 0..1 sung, >1 passed; `hash`: a stable 0..1 from the index; `start`, `end`:
  the whole text's) and the bindings. Pure helpers for
  them: `keys(x, at0, v0, at1, v1, …)` (keyframes, eased between stops), `spring(t, freq,
  damping)`, `mix(a, b, t)` (colours or numbers), `sin`, `cos`; `tween` is the same as its value
  (no frame state). A unit's look is a function of time, never of frames drawn. Guards: the
  expressions run only when the text, its `units`/`timing`/`time` or what they read changed;
  every value is clamped where it enters paint (not finite → the default, moved at most one
  line, scale 0.5–2, turned ±45°, opacity 0–1, glow 0–16); at most 512 units (more: drawn plain); paint
  only (layout never moves). A unit at rest is its crisp `Glyph`s as laid out; moved, scaled or
  turned it is `GlyphXf`s: the glyph through an affine map, sampled bilinear (with the sweep),
  so it glides by fractions of a pixel and settles without stepping. `text-rotate` (degrees
  clockwise, ±90, paint only) turns a whole text about its centre after its units, with or without
  units; `text-origin: x, y` (dips from the centre, ±4096) moves the pivot (a line on a wheel
  turns about the hub). Scaled, a glyph is rastered
  at the nearest whole-px size and the map does the rest, so an animated scale adds a few atlas
  sizes, not one per frame.
- A fragment can be taken in once (`Doc::adopt`) and built many times (`instantiate_scoped`),
  each instance with its own bindings (`{scope}/{name}`): many styled rows without a mount per
  row. `Doc::update` visits only the expressions reading a binding that changed when nothing
  else did (no window change, no let moved, no tween).
- `component Name(params) { … }` defines reusable pieces, and `Name(args)` instantiates them. Long lists are Rust widgets (`VirtualList`).
- Style values can be tokens (`text-primary`, `overlay`, `body-large`, `material: acrylic`), which resolve through the installed `theme::Theme` (`theme::install`; `Theme::winui_dark()` until an app installs another; `Theme::material3_dark()` / `material3_light()` give the Material 3 baseline under the M3 system names, `md-sys-color-primary`, `md-sys-typescale-body-large`, `md-sys-shape-corner-medium`, `md-sys-elevation-level2`, `md-sys-state-hover-state-layer-opacity`, without letter spacing, with one shadow per elevation and no dynamic colour). Names resolve when a sheet is built. Numbers are dips.
- Files are parsed when the app loads them (an app may compile its sheets into the executable).
  Debug builds reload them live: `Doc::watch(ui, path)` watches the file a doc's sheet came from
  (a release build does nothing there); a change wakes the app with `dsl::SheetChanged`, and the
  next `Doc::update` applies it in place (`dsl/reload.rs`). Every element keeps its id, the
  app's handles and models stay, and what the app set on an element at run time (a text, an
  opacity) stays unless the file changed that same field: each build the doc made (`build`, each
  `instantiate`) is built again aside from the old and the new sheet, and the live element
  takes, field by field, what differs between the two. Children are matched by kind and name
  in order: an added one comes in, a removed one goes. Fragments from outside (`mount`,
  `instantiate_scoped`) are not the file's and stay. A sheet that does not parse or build
  leaves the tree as it was and shows its error in a banner at the window's bottom.
- The inspector (debug builds, F12): the element under the pointer outlined, and a panel with
  its label (kind, name, classes, line in its sheet), its builders' path, its box, its layout
  and visual where they differ from the defaults, its text, and its last damage: which setter
  changed it and from where (the setters forward their caller, `track_caller`, up to the app's
  own call where every step does). A release build has none of it: no fields, no code.
- Window-manager viewport transitions use `Ui::snap_layout()` to settle document tweens on the next update; retained tree animations are independent. Minimized dimensions are not layout input; app timers remain runnable, but frame-only trim deadlines wait for restore. Manual border resizing still uses the declarative law and its curves.
- Win32 owns normal/maximized/minimized placement and transitions; the host only adapts native
  notifications to layout. Borderless fullscreen saves/restores `WINDOWPLACEMENT`. Returning to a
  saved maximized placement temporarily suppresses DWM's fresh normal-to-maximized zoom while
  the native restore is committed; ordinary maximize/restore animations are unchanged. No separate
  transition state machine or window-size animation is maintained.

## 6a Text fields

`Ui::make_edit` turns a Row into a single-line field, implemented in `widgets/text_field.rs`/`app.rs`, and behaves like the Windows edit control:

- caret and selection; Left/Right move by grapheme cluster (a letter with its marks, a conjunct), Shift extends the selection, Ctrl moves by word (each CJK ideograph counts as a word); Home/End;
- the caret stands at its cluster's leading edge in that cluster's direction (`Text::carets`, §5); a selection is drawn as one span from its leftmost edge to its rightmost, so in a line of both directions it can cover what lies between its two visual pieces;
- Backspace/Delete remove the selection, else Backspace the char before the caret (a mark comes off its letter, as in the Windows edit control) and Delete the cluster after it (with Ctrl, one word);
- Ctrl+A/C/X/V, using the host clipboard (`clipboard_out` / `paste_wanted`);
- a press places the caret, a drag selects, a double click selects the word;
- typing replaces the selection; text scrolls horizontally to keep the caret in view.

The app's own clipboard reads go through the same host read: `Ui::read_clipboard(done)`
delivers `done(text)` as a message in a later turn. `Ui::pick(FilePick, done)` asks the system's
file picker (files to open, a folder, a place to save) and delivers `done(paths)` once it is
answered (none: cancelled). The UI thread never waits on another process for either: Linux's
pickers are child processes waited for on a thread of their own, Windows' picker is modal to
the window as the system's own are.

IME:

- The host removes `ISC_SHOWUICOMPOSITIONWINDOW` in `WM_IME_SETCONTEXT`, which turns off the IME's own composition window, and forwards `GCS_COMPSTR`/`GCS_RESULTSTR` as `Input::Preedit`/`Input::Commit`. The field draws the composition inline at the caret with an underline, the way WinUI's TextBox does through TSF.
- The candidate window is placed under the caret with `CFS_EXCLUDE`.
- The input context is attached only while a field has focus (`ImmAssociateContextEx`), so Space, the arrows and the other shortcuts reach the app everywhere else.
- Linux, the same three things through each display server's input-method protocol; `Ui`'s side (`Input::Preedit`/`Input::Commit`, `ime_on`, `ime_at`) is the one Windows uses:
  - Wayland: `zwp_text_input_v3`, enabled while a field has focus (in a focused window), disabled elsewhere; its preedit (cursor in bytes, turned into chars) and commits are applied at each `done`; the cursor rectangle is the caret's (`ime_at`, in surface coordinates). fcitx5 and IBus reach it through the compositor (KWin: System Settings › Virtual Keyboard).
  - X11: XIM (`platform/xim.rs`), over-the-spot (`XIMPreeditPosition`: the server draws the preedit and its candidates at the spot) where the server offers it, else on-the-spot (`XIMPreeditCallbacks`: the preedit drawn in the field; Xlib sends no spot with it, so fcitx5 puts its candidates at the window's corner), else committed text only. The context has the focus while a field does; a field that loses it drops the composition (`Xutf8ResetIC`). The spot (the candidates' place) is the caret's line bottom. A server that starts after the window or restarts is taken up. `XMODIFIERS` names the server (`@im=fcitx`).

Field styling:

- Fields have a focused state, styled with `bg-focus` and `border-focus`.
- A bottom line can be explicitly set with `underline`/`underline-focus`: e.g. 1 px ControlStrongStroke at rest and 2 px accent when focused. This is an opt-in styling capability, not a mandatory editor appearance.

## 6a' Popups and flyouts

`ui.popups` lists the light-dismiss popups, each a `Popup { id, anchor }`. A press outside every
popup raises `PressOutside`; a press on a popup's `anchor` (the button that opened it) raises
`PressOutside` and nothing else, so pressing the button again closes it rather than reopening it
(WinUI's light dismiss). The app does not latch "was open".

`widgets::Flyout` is one host element shown as a flyout: `reset` clears its rows, the app adds
rows, `open(place, anchor)` shows it and registers the popup, and after the layout barrier
`place` reads the laid rows and the anchor's rect and picks the side (`Place { side, along,
gap, shift, flip }`: below / above / right / left, flipping to the roomier side), height (the
host's `max-height`, the window's room) and scroll, then starts the motion (`enter_as` gives one
opening its own). `close` reverses it. `wheel`, `reveal` and `sync_rail` / `rail_event` scroll
it: a wheel notch, a key's reveal and a page from the rail glide (`Glide`, the flyout's `glide`
skin, WinUI's by default; `sync_rail` stands the rows where the glide is and asks for frames
while it moves), a dragged thumb and an opening's centred row move it at once. The widget holds no ids: the host and rail parts are passed in, as `Scrollbar::sync` takes
its parts. Widths and row heights are the sheet's; the widget sets none.

`widgets::switch` is a switch's motion (WinUI's ToggleSwitch): the sheet makes the switch of its
looks (the track's off and on, the knob's off and on, the knob's box at its pressed size, placed
at the end shown); `show` stands a built one still, or, given how the one it replaces stood
(`Was`), slides its knob from the other end and cross-fades the looks; `feel`, on `Hover` and
`Pressed` (`press-events`), gives the knob its size (`Prop::Scale`, and `Prop::ScaleX`, which
widens a box without widening its corners). Paint only; the timings and sizes are the product's
(`Skin`).

## 6a'' Keyboard focus and semantics

What an element is to the keyboard and to assistive clients is its `Semantics` (`semantics.rs`),
written in `.nui` or by `Tree::set_semantics`: `role` (`none group button switch slider list
list-item text-input label heading image menu menu-item dialog`), `label` (the name it is
announced by, `@key` following the locale), `focusable`, and a slider's `value`, `min`, `max`,
`step`. What is not written is inferred: an element is focusable when it has an `on-click`,
hears drags (`drag-events`) or is a text field.

The focus (`focus.rs`, `Ui::focus`; `Ui::focused_field` when it is a text field):

- A press gives it to a text field and takes it from anything else. Tab and Shift+Tab move it
  to the next and previous Tab stop in document order, round from the end to the start: the
  focusable elements that are shown (as of the last frame) and enabled. A `role: list` is one
  stop, its item that has the focus or else its first item in view; its items are the
  focusable elements in it not inside another, in the order they stand (top to bottom, left to
  right, so a virtual list's reused rows go by place), and Up / Down move along them (at an end
  the key is still the list's).
- Keys go to the focus first: a text field's (§6a); Enter and Space press a focused element
  with an `on-click` (`Event::Click`); on an element with a `value`, Left / Down and Right / Up
  step it by `step`, Page Down / Page Up by ten steps, Home / End to `min` / `max`, and the app
  hears `Event::Value { id, value }` and applies it. Tab is never the app's; every other key the
  focus does not use is (`Event::Key`).
- Focus moved by the keyboard raises `Event::Reveal { id }`: the app scrolls the element into
  view where it owns the scrolling.
- Keyboard navigation can be turned off (`Ui::set_keyboard_navigation(false)`; on by default):
  then the focus uses no key (Tab, Enter, Space and the arrows reach a text field or the app as
  `Event::Key`), and a focus outside a text field is dropped. Presses and assistive clients
  still move the focus.
- While a popup is open Tab stays inside the topmost one, and a focus outside it (the button
  that opened it) leaves Enter, Space and the arrows to the app. When the popup is removed, a
  focus inside it goes back to its anchor.
- The focus visual: focus that came from the keyboard draws WinUI's focus visual around the
  element (a 2 dip ring in the theme's `focus-stroke-outer`, its outer edge 3 dips out, and a
  1 dip ring in `focus-stroke-inner` inside it, both following the element's corners), over
  what the element draws and never faded with it (an invisible hit area shows it too). After a
  press it is not drawn. A text field shows its focus in its own look (`bg-focus`,
  `border-focus`, `underline-focus`), which apply to any focused element where written.

### Accessibility (screen readers)

`access.rs` gives assistive clients (Narrator, NVDA, Orca) the element tree through AccessKit:
UI Automation on Windows (`accesskit_windows`, answering `WM_GETOBJECT`), AT-SPI on Wayland and
X11 (`accesskit_unix`), in a window whose app asks for it (`Options::accessibility`; without it
no adapter is made and the system's asks go unanswered). Nothing is built while no client asks: the adapter only notes the
client's first ask and wakes the loop; that turn sends the whole tree, and from then each turn
sends, after its frame, the nodes of the elements it changed (every setter that marks an element
for paint or move, a structure change, an action, semantics, a box that moved) and the focus. A
client that goes away (Linux) stops it.

- **Nodes.** The root is the window (named by its title). An element is a node when it has a
  written `role` (`none` makes it none) or `label`, is a text field (a password field when
  secret), has a `value` (slider), an `on-click` (button), `drag-events` (slider), is an offset
  group (its content moves by the group's transform, so a scroll sends one node), or shows
  text (a label, its text as the value). The rest stand aside: their children are their
  nearest node's. Only what is shown is sent.
- **Names.** `label` where written; a button, switch, list item, menu item or heading without
  one is named by all the text it and its subtree show (a queue row by its title, artist and
  length), which is then no node of its own unless it is a control itself. A text field's value
  is its text (not a secret one's), its name (unless written) and placeholder the hint. A switch is toggled while lit, a list or menu item selected while lit. `live: true`
  makes a node a polite live region: a client announces its name when it changes.
- **Requests** come back as input in the next turn: a press is the element's `Click` (an element
  without an action takes the focus), focus is the keyboard's (`Reveal` follows), increment,
  decrement and a value are `Event::Value` within the range, "scroll into view" is
  `Event::Reveal`.
- Bounds are the boxes in window pixels (Windows: the adapter adds the window's place). X11's
  and Wayland's window position is not given to AT-SPI.

## 6b Backends

One WGSL source (`novaui-shaders/src/prims.wgsl`, plus the app's image effects) is the only GPU
shader. naga translates it to HLSL (D3D11) and SPIR-V (Vulkan) when the program is built
(novaui-shaders: novaui's build script for the renderer, the app's for its effects), so no shader
compiler ships in a program; D3D11 compiles the HLSL at start with the system's compiler. A
backend makes the built-in programs when it is made and each image effect on its own at the
first draw after it is registered, whenever that was: one that a backend cannot make has its
draws skipped there, and nothing else changes. `render::batch::encode` turns primitives into instances and batches for both, so a frame
issues the same draws on either API. `render::software` is the reference every GPU path is checked against.

| Backend | Platforms | Window | Notes |
|---|---|---|---|
| `d3d11` | Windows | flip-model swap chain shown through DirectComposition, else on the window (§7.1); `Present1` dirty rects, frame-latency object | default on Windows |
| `vk` | Windows, Linux | FIFO swap chain, per-image damage history, BGRA via 1:1 blit | default on Linux; `backend=vk` on Windows |
| `sw` | all | GDI (Win32) / `XPutImage` (X11) | fallback, reference |

The Vulkan backend keeps every image in `GENERAL` layout with a full memory barrier after each
pass, records one command buffer per frame and keeps at most one frame in flight; per-frame rings
(instances, uniforms, descriptor sets, staging) are recycled when that frame's fence signals. Render
targets use the D3D11 256-px bucket policy. The loader is process-global: drivers keep threads that
run loader code after a device is destroyed.

### 6b.1 HDR output (Windows, D3D11)

On a monitor in HDR mode (Windows reports its output in BT.2100 PQ) the D3D11 backend draws into
half-float targets (canvas, layers, the blur chain) holding the same sRGB-encoded colour as on SDR,
and its swap chain is half floats in scRGB (`RGB_FULL_G10_NONE_P709`: linear light, 1.0 = 80
nits). The present does not copy: `fs_present` decodes each damaged pixel with sRGB's curve and
scales it by the output's SDR white over 80 nits, which is what Windows does to an SDR window, so
the UI looks as it does there. An image drawn with `ImageContent::hdr` and carrying a gain map
(`image/gainmap.rs`: Ultra HDR / Adobe gain-map JPEG, `hdrgm` XMP, found through the MPF index or
the XMP container) takes its HDR rendition instead: the SDR picture raised by the gain map as far
as the output's headroom (peak over SDR white) allows, sRGB-encoded past 1.0. Everything else stays
at or under SDR white: the material reads the blur at most at 1.0 (`fs_backdrop`), so an HDR
picture under a flyout does not glow through it. SDR outputs keep RGBA8 targets and the copy.

The host (`platform/win32_hdr.rs`) finds the DXGI output of the monitor the window is mostly on,
its SDR white (`DISPLAYCONFIG_SDR_WHITE_LEVEL`) and peak (`MaxLuminance`), and tells the backend
(`Backend::set_output`) at the first turn, when the window changes monitor, when the display
configuration changes, when the window is activated (the SDR white slider sends nothing) and when
DXGI's factory goes stale. A change remakes the targets and has everything drawn and uploaded
again (`Tree::invalidate_all`). `NOVAUI_HDR=0` keeps SDR. Read back, an HDR canvas gives the
8-bit values it encodes (clamped at 1.0), so the GPU-versus-reference checks hold on HDR
monitors too (`d3d11_matches_reference` runs both).

Windows' compositor decides what reaches the screen: a window straddling an SDR and an HDR monitor
is clipped to SDR, and overlapping windows can make it drop an HDR window's highlights (reported
on multiplane overlays). Captures that are not taken in half floats (most screenshot tools) show
an HDR window wrongly; Windows.Graphics.Capture in `R16G16B16A16Float` shows it as composed.

The X11 host (`platform/x11.rs`, Xlib loaded at run time) draws no WM decorations; the edges hand
the press to the window manager with `_NET_WM_MOVERESIZE`. Keys are mapped to the key codes
of `input::key` (Windows' virtual-key numbers, named). Committed IME text arrives through XIM; inline
preedit is not implemented on X11. Fonts: the chain of §5, by family name. Verification: `cargo test -p novaui` (includes `vulkan_matches_reference`) and the
`gallery` example's end-to-end mode, see [NovaPlayer's verification.md](https://github.com/wuuuming/NovaPlayer/blob/main/docs/verification.md).

## 6c The caption (title bar) per platform

A press on the app's caption (`NcHit::Caption`) is first an ordinary press to the app, so it
closes a flyout (`PressOutside`) and drops focus like a press anywhere else; then the title-bar
actions follow, the same on every host, as GTK4 (`GtkWindowHandle`) and Chromium
(`HandleMouseInputForCaption`, `WindowEventFilterLinux`) do them:

| Action | Windows | X11 | Wayland |
|---|---|---|---|
| Left drag | `DefWindowProc`'s modal move loop (snap included), entered after the app's press; the app's release follows when it returns | `_NET_WM_MOVERESIZE` once the held pointer leaves `DRAG_SLOP` | `xdg_toplevel.move` with the press's serial, once it leaves the slop |
| Double-click | maximize / restore (Windows' `WM_NCLBUTTONDBLCLK`) | `CaptionPress`: 400 ms, within the slop | same |
| Right-click | the system menu, opened by the host (`GetSystemMenu` + `TrackPopupMenu`): with the frame drawn by the app, `DefWindowProc` opens none | `_GTK_SHOW_WINDOW_MENU` when the WM lists it in `_NET_SUPPORTED` (KWin, Mutter, Xfwm) | `xdg_toplevel.show_window_menu` |

The app sees the drag's press end (`Input::Up`) before the window manager takes the pointer, so
nothing is left held. The pointer moves over the caption as over the rest of the window (hover):
Win32 sends its non-client moves as `Input::Move` and a leave only when the pointer is off the
window, not when it crosses from the caption to the client.

## 6d Window edge and shadow per platform

The window is frameless everywhere (the app draws its title bar). What gives it an edge and a
shadow:

| Platform | Shadow | Corners / edge | How |
|---|---|---|---|
| Windows 11 (22000+) | DWM | rounded, 1-px `#3A3A3A` | `DWMWA_WINDOW_CORNER_PREFERENCE = ROUND`, `DWMWA_BORDER_COLOR`; DWM drops both when maximized |
| Windows 10 | DWM (1-px frame margin) | square, 1-px `#3A3A3A` drawn by the tree | DWM refuses the Win11 attributes; `Tree::set_frame_stroke` draws the edge, off when maximized/fullscreen |
| KDE Plasma X11 | KWin, `_KDE_NET_WM_SHADOW` | square, 1-px edge drawn by the tree | eight ARGB32 pixmaps, a 12-px-sigma falloff 28 px out (34 below, 22 above), alpha 0.42 |
| KDE Plasma Wayland | KWin, `org_kde_kwin_shadow` (the same tiles) | same | the native Wayland host (§7) binds it; under XWayland (`NOVAUI_X11=1`) KWin honours the X11 property |

`Tree::set_frame_stroke(Some(color))` draws a 1-px line on the outermost pixels over everything,
damaging only the edges when it changes (`tests/frame_stroke.rs`).

Verified on Windows 11 26200: DWM reports corner preference 2 and accepts the border colour. KDE
X11: KWin's own capture (ScreenShot2) shows the shadow falling 0xD8→0xAC over a light background
and the 0x3A edge. Plasma Wayland — in a nested `kwin_wayland --virtual`
session with XWayland the property is set on NovaPlayer's window; that compositor exposed no
screenshot interface, so its rendering was not captured. Windows 10 is not verified yet.

## 7 Platform hosts

| Host | Selected | Input method | Decorations / shadow |
|---|---|---|---|
| Win32 | Windows | IMM32: inline preedit, candidate window at the caret (§6a) | DWM (§6d) |
| Wayland (`platform/wayland.rs`) | a Wayland session; `NOVAUI_X11=1` forces X11 | `zwp_text_input_v3`: inline preedit, candidates at the caret (§6a) | client-side; `org_kde_kwin_shadow` on KWin |
| X11 (`platform/x11.rs`) | otherwise, or when Wayland cannot give a window (no xdg-shell, no Vulkan surface) | XIM (`platform/xim.rs`): over-the-spot at the caret, else on-the-spot (§6a) | `_KDE_NET_WM_SHADOW` on KWin |

Wayland: an xdg-shell toplevel without server decorations; moving and resizing are the
compositor's (`xdg_toplevel.move` / `.resize`); fractional scale from `wp_fractional_scale_v1`
with `wp_viewporter`, else the outputs' integer scale; keys through xkbcommon into the
`input::key` codes. Linux hosts prepare the process environment once
(`platform::linux::prepare_env`, guarded by `NOVAUI_ENV_READY`), e.g. the NVIDIA Wayland Vulkan
producer link Debian's 535 packages lack.

### 7.1 Live resize (Win32)

When the user drags the border, Win32 runs its own modal loop inside `DefWindowProc`:

- `WM_SIZE` runs the whole turn synchronously (update, layout, paint, render, present) from a
  thread-local `Runner` shared by the main loop and the window procedure: border and content
  change size in the same step. A message arriving during a turn waits for the next one.
- `WM_ENTERSIZEMOVE` starts a 10 ms timer, `WM_EXITSIZEMOVE` stops it, so playback, animations and
  timers keep going inside the modal loop.
- During a resize, frames skip the frame-latency wait and present with sync interval 0 (with 1,
  DWM held `Present1` for 200–850 ms). Frames that are not the border's own are held to the
  monitor's refresh and gated on the last present having retired (`Backend::frame_ready`).
- Present modes, the fastest the machine has. The mode is probed by trying, before the window
  exists, because the window's style depends on it. `NOVAUI_PRESENT` forces one.
  - `composition` (default): the window is made with `WS_EX_NOREDIRECTIONBITMAP`, and a
    DirectComposition visual shows a composition swap chain (`DCompositionCreateDevice` is looked
    up at run time).
    - Its buffers are larger than the window, shown 1:1 by `SetSourceSize` plus a scaling
      matrix that apply with the present.
    - At rest they come in 256-px buckets. In a live resize they never shrink and grow once, to
      the monitor, so a drag reallocates at most once.
    - The trim 0.5 s after the last resize, or after the modal loop ends, takes them back.
  - `flip`: a swap chain on the window, exactly the client size with `DXGI_SCALING_NONE`.
    Every size step reallocates it; DWM stretches any smaller source region.
  - The `composition` probe fails on RDP, VMs and old drivers; the window then gets the `flip`
    swap chain.
- Freed textures are flushed after the frame that released them.
- Textures of images of 256\u00d7256 px and more are made on a thread of their own (the device is
  free-threaded): such an image shows from the frame after the one that first drew it. Smaller
  ones are made at once.

### 7.2 Overlays

An overlay (`platform::OverlayOptions`; `run_overlay` on the calling thread, `spawn_overlay` on a
thread of its own, `novaui-overlay`) is a window over every other one: desktop lyrics, a notice.
It runs the same `App` and `Ui` as a main window, with its own turn, tree and scheduler on its
thread; another thread reaches it by a channel and its waker. It has no frame, no shadow and no
taskbar entry, is never activated (it never takes the keyboard), and is transparent where nothing
paints: the app sets the tree's clear colour transparent (`Tree::set_clear`), and the canvas's
premultiplied alpha is what the system composes.

- **Size and place.** The app sizes it (`Window::set_size`, dips), usually to its content's
  laid-out box after layout (the content laid out on a stage that does not depend on the
  window's size). It grows and shrinks about its bottom centre. Its place (`Place`, the bottom
  centre in the host's own measure) is `Window::place`; `Event::Moved` says the user moved it.
  The app keeps the place and hands it back (`OverlayOptions::place`); a place on no display,
  or none, opens it centred at the bottom of the main display's work area, `lift` of its height
  up. `Window::work_area` is the work area of its display, in dips.
- **Moving.** Where the app answers `NcHit::Caption` the user drags it, as a main window's
  caption; there is no resize border, no maximize and no window menu.
- **Click-through.** `Window::set_click_through(true)` lets presses through to what is under it;
  the pointer coming and going over it still arrives as `Input::Move` / `Input::Leave`, so hover
  works.

| Host | Window | Transparent | Click-through, and hover while it is on |
|---|---|---|---|
| Win32 | `WS_POPUP` with `WS_EX_TOPMOST`, `WS_EX_TOOLWINDOW`, `WS_EX_NOACTIVATE` (and `MA_NOACTIVATE`), `WS_EX_LAYERED`; DWM's corners, border and transitions off | DirectComposition only: a composition swap chain with `DXGI_ALPHA_MODE_PREMULTIPLIED` (no composition, no overlay: `run_overlay` fails) | `WS_EX_TRANSPARENT`; a low-level mouse hook (`WH_MOUSE_LL`) on its thread checks the pointer against the window's rectangle |
| X11 | not yet | | |
| Wayland | not yet | | |

An overlay draws SDR (no HDR output) and has no accessibility tree.

## 8 Environment switches

| Variable | Effect |
|---|---|
| `NOVAUI_TRACE=<file>`, `NOVAUI_TRACE_TURNS=1` | runtime counters per second / per turn (`runtime.md` §9) |
| `NOVAUI_FONTS=<dir>` | where the bundled fonts are (default `fonts/` beside the executable) |
| `NOVAUI_TEXT_LOG` | log glyph-atlas resets |
| `NOVAUI_HDR=0` | Windows: draw for SDR on an HDR monitor too (§6b.1) |
| `NOVAUI_X11=1` | force X11 in a Wayland session |
| `NOVAUI_PRESENT=composition\|flip` | Windows, D3D11: force a present mode (§7.1; default: the fastest the machine has) |
| `NOVAUI_AUDIT=1` / `0` | the debug contracts and the damage audit (`runtime.md` §5) in any build / off (default: on in debug builds) |
| `NOVA_FONTS_DIR` | tests only: the fonts the text tests need |

## 9 Known limits

Compared with mature toolkits (GPUI, egui, Flutter, browsers), these are not there yet.

| Area | Today | Missing |
|---|---|---|
| Accessibility | keyboard focus; the tree through UI Automation and AT-SPI (AccessKit, §6a'') | the window's screen position for AT-SPI; text ranges (a client reads a field's text whole, no caret or selection) |
| Text | shaping (harfrust), bidi, UAX #14 breaking, greedy | colour emoji; dictionary breaking for Thai, Lao, Khmer (they break at spaces); vertical text; justification |
| Input method | inline on Win32, Wayland (`text-input-v3`) and X11 (XIM) | surrounding text (reconversion), a status window |
| Tooling | `NOVAUI_TRACE`, screenshot tests, inspector and live `.nui` reload (debug builds) | a layout overlay (flex lines, padding) |
| Windows | one main window per process, and overlays on threads of their own (§7.2) | more main windows, popups as OS windows |
| Output | SDR; HDR on Windows through D3D11 (§6b.1), gain-map JPEG images | HDR on Vulkan (Linux needs a compositor with colour management, KWin 6); AVIF and ISO 21496-1 gain maps; GLES / Metal backends |
| Widgets | single-line field, flyout, list, scrollbar, QR | multi-line text, slider, table / tree |
