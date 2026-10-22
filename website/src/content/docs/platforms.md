---
title: Platforms and backends
description: Which hosts and renderers novaui has, and how a window looks on each.
---

## Backends

One WGSL shader is the only GPU source; naga translates it to HLSL for D3D11 and to SPIR-V for
Vulkan, so both APIs issue the same draws. The software renderer is the reference every GPU path
is tested against.

| Backend | Platforms | Window | Notes |
|---|---|---|---|
| `d3d11` | Windows | flip-model swap chain, `Present1` dirty rectangles, frame-latency object | default on Windows |
| `vk` | Windows, Linux | FIFO swap chain, damage kept per image | default on Linux; on Windows when `Options::backend` is `BackendKind::Vulkan` |
| `sw` | all | GDI (Win32) / `XPutImage` (X11) | fallback, reference |

## Hosts

| Host | Selected | Input method | Shadow and edge |
|---|---|---|---|
| Win32 | Windows | IMM32: inline composition, the candidate window at the caret | DWM: rounded corners and a 1-px border on Windows 11, a 1-px edge drawn by novaui on Windows 10 |
| Wayland | a Wayland session | not yet (`text-input-v3`) | client-side; KWin's shadow protocol on KDE |
| X11 | otherwise, or `NOVAUI_X11=1` | XIM, committed text only | `_KDE_NET_WM_SHADOW` on KWin |

Every window is frameless: the app draws its title bar and answers `App::nc_hit`. A press on the
caption is first an ordinary press to the app (it closes a flyout), then the platform's
title-bar actions follow: drag to move, double-click to maximize, right-click for the window
menu.

Wayland uses fractional scaling when the compositor offers it. On Windows the window's scale
follows the monitor's DPI.

## Environment

| Variable | Effect |
|---|---|
| `NOVAUI_FONTS=<dir>` | where the bundled fonts are (default `fonts/` beside the executable) |
| `NOVAUI_TRACE=<file>` | runtime counters, once a second ([Performance](/performance/)) |
| `NOVAUI_AUDIT=1` | the phase and damage checks in a release build |
| `NOVAUI_TEXT_LOG` | log glyph-atlas start-overs |
| `NOVAUI_X11=1` | force X11 in a Wayland session |

## Not yet

Accessibility (a semantics tree for UI Automation and AT-SPI), colour emoji, more than one
window per process, HDR output, GLES and Metal backends, multi-line text, sliders, tables.
