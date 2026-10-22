---
title: The frame
description: Damage, layers, presenting, and why an idle window costs nothing.
sidebar:
  order: 6
---

A frame is one display-paced draw. It runs only when something needs painting or a ticker runs:

1. **Tick**: animations advance to the frame's time.
2. **Layout**, if a tick changed it.
3. **Paint**: elements marked for paint record their drawing again, in their own coordinates.
   Elements that only moved keep their recording. Both add their old and new bounds to the
   **damage** of their layer.
4. **Render**: only damaged regions of visible layers are rasterized, then composited.
5. **Present**: only the damage is handed to the swap chain (`Present1` dirty rectangles on
   D3D11, per-image damage on Vulkan). A frame with no damage presents nothing.

## Idle is zero

With nothing animating and nothing pending, the host sleeps until input, a waker or its earliest
timer. There is no repeating idle frame. A turn that drew nothing waits for the next refresh
before running again, so it never spins.

## Pacing

On D3D11 the swap chain is flip-model with a frame latency of one, and a turn waits on its
latency object before it reads input, so input, timers and the frame share one clock reading and
presenting never blocks. While the user drags the window's border, frames are drawn inside the
system's resize loop and kept in step with the border.

## Primitives

Everything is drawn from a closed set: rounded rectangles with borders, box shadows, glyphs,
images, image effects, layers and the acrylic backdrop. Each has one WGSL expression for the GPU
backends and one in the software renderer; the tests compare them pixel for pixel, within 2/255.

## Counting

`NOVAUI_TRACE=<file>` writes, once a second, the turns and their causes, frames, presents,
microseconds per phase, elements positioned, moved and painted, and who asked for every wake.
See [Performance](/performance/).
