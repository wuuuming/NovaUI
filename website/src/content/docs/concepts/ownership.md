---
title: Ownership and memory
description: Who owns what, what caches keep, and where work off the UI thread goes.
sidebar:
  order: 5
---

- **The tree owns elements.** The app holds `Id`s, which are checked: a removed element's `Id`
  refers to nothing.
- **Owners hold handles in fields.** A view that closes drops its struct, and with it every
  timer, ticker, task and subscription it held. There are no stale results to filter and no
  epochs to compare.
- **`Ui` holds weak references** to handles, so a dropped handle simply disappears.
- **Images are shared.** An `ImageHandle` is interned and reference-counted: two elements
  showing the same image hold one texture, freed when the last lets go.
- **Every cache states its limit** and what it drops first: the glyph atlas (16 pages, the page
  used longest ago), text layouts, images.
- **No allocation on per-frame paths.** Ticking, painting and hit testing reuse buffers kept
  on their owner.

## Work off the UI thread

- `ui.spawn(priority, work, done)` runs `work` on a small worker pool and delivers `done(result)`
  as a message. `Visible` work goes before `Background` work; a full background queue refuses
  more (`None`), so the caller asks again later. `work` gets a `Cancel` that tells it whether its
  `Task` was dropped.
- `ui.queue(capacity)` is a serial queue on the same workers: `latest` replaces its pending job,
  `try_background` is refused when it is full.
- `ui.waker(msg)` is a `Send` handle for threads the app owns: `wake` delivers `msg` in the next
  turn.
- Results are delivered within a budget of half a refresh per turn; the rest wait for the next
  turn, which runs at once.
