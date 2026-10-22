---
title: Events and their lifecycle
description: What reaches App::event, in what order, and what each phase may do.
sidebar:
  order: 4
---

An app implements `App`:

| Method | Called |
|---|---|
| `init` | once, before the first frame |
| `event(ui, ev)` | for every input event and every message of the app's own handles |
| `after_layout(ui)` | after the layout of every turn that delivered the app something, and once after `init` |
| `nc_hit(ui, x, y)` | when the host asks which part of the window a point is (caption, client, edges) |

## Events

Input arrives as `Event`s: `Click` (with the element's action), `Hover`, `Pressed`, `DragStart`, `Drag`,
`DragEnd`, `DragCancel`, `Wheel`, `Key`, `Edit`, `Submit`, `Cancel` and `Blur` from a text field,
`PressOutside` when a press lands outside an open flyout, and more. Elements say what they want
to hear: `on-click`, `hover-events`, `press-events`, `drag-events`, `wheel-events`.

**Messages** are the app's own values. A handle is made with one and delivers it as
`Event::Message`:

| Handle | Made with | Delivers its message |
|---|---|---|
| `Timer` | `ui.timer(msg)`, then `at(t)` or `within(lo, hi)` | at its time |
| `Ticker` | `ui.ticker(msg)`, then `start` / `stop` | once per display refresh while it runs |
| `Task` | `ui.spawn(priority, work, done)` | when the work, run off the UI thread, is done |
| `Waker` | `ui.waker(msg)`, then `wake` from any thread | in the next turn |
| `Subscription` | `model.subscribe(msg)` | in the same turn the model changed |

Dropping a handle cancels it. A message is stored once, when its handle is made, so a ticker
at 240 Hz allocates nothing per frame. The app tells its messages apart by type:
`m.is::<MyMessage>()`.

## One turn

```text
turn (wait for the swap chain; read the clock once)
 1. collect    OS input, finished tasks, wakes, due timers
 2. dispatch   input in arrival order, then task results, then timers   -> App::event
 3. flush      model notifications, each subscriber once per round      -> App::event
 4. layout     only if something needs it
 5. measure    App::after_layout: may change layout once more
 6. frame      only if something needs painting or a ticker runs:
               tickers -> App::event, tweens to the frame's time, layout, paint, present
 7. sleep      until input, a wake, the earliest timer, or the next refresh
```

Everything one input causes is applied before its frame, so no frame shows half a change.
Animations read the frame's time, never the wall clock: they can be interrupted at any frame and
never have to finish.

## What each phase may do

| Phase | May write | Must not |
|---|---|---|
| dispatch, flush | models, element properties, handles | read this turn's layout; block on I/O |
| `after_layout` | the asking view's elements | notify models; change layout twice |
| tickers | animated properties | add or remove elements; notify models |

These rules are checked: in debug builds, and in any build with `NOVAUI_AUDIT=1`, breaking one
panics.
