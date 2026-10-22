---
title: Performance
description: What novaui measures, what it changed, and the numbers.
---

The rule is that **the work in a frame follows what changed**, and an idle window costs nothing.
The measurements behind it were made from outside the process (per-thread cycles, context
switches, DWM's load, the presents) on Windows 11, an NVIDIA GPU and a 240 Hz display, with
NovaPlayer as the workload.

## What mattered

- **The cost is per present.** The driver's user-mode work costs about the same for every
  present, whatever is drawn; presenting less is the main lever. A frame with no damage presents
  nothing, and there is no idle repeat.
- **Driver submission on the UI thread.** The D3D11 device is created with
  `PREVENT_INTERNAL_THREADING_OPTIMIZATIONS`, so the driver does not wake a worker thread for
  each present: −22 % process CPU at 240 fps, a quarter fewer context switches.
- **No spin.** A turn that drew nothing waits for the next refresh. A ticker, not a zero
  timer, asks for "every frame".
- **Moving a container costs one offset.** An element that is translated or scrolled is an
  *offset group*: its descendants keep their positions relative to it, so moving it repositions
  one element and damages its old and new bounds.
- **Clip-only changes reuse recordings**, and text is measured without placing glyphs.
- **Paced delivery.** Task results arrive within half a refresh per turn, so a burst of
  results (a 20 000-row tag fill) cannot make a long turn.

## Numbers

From the measurements (Mcyc/s: million CPU cycles per second for the whole process):

| Scenario | Before | After |
|---|---|---|
| Paused, lyrics open | 18–31 Mcyc/s, 10 idle presents/s | 2.7 Mcyc/s, 0 presents |
| Word-timed lyric sweep at 240 fps | 356 Mcyc/s, DWM 16–25 % | 242 Mcyc/s, DWM 9 % |
| 20 000-row tag fill | 99 Mcyc/s, 1 340 turns/s, worst turn 88 ms | 27 Mcyc/s |

## Measuring your app

`NOVAUI_TRACE=<file>` writes one line a second: turns and their causes (input, waker, timer,
tree work, idle), frames and presents, microseconds per phase (layout, `after_layout`, frame,
present), elements positioned, moved and painted, hit tests, and the call site of every wake. A
turn over 4 ms gets a line of its own. The targets: 0 turns a second when idle, 0 frames with
empty damage, at most 2 layouts a turn.

`NOVAUI_AUDIT=1` turns on the checks of debug builds in a release build: the phase rules and the
damage audit, which re-records every element a frame did not paint and compares it with what it
shows.
