---
title: The .nui language
description: Structure, layout and style in a small declarative language; logic stays in Rust.
sidebar:
  order: 3
---

A `.nui` sheet says what a window looks like. It has no logic: the app's Rust code changes state
and the sheet presents it.

```scss
// A style: applied with `.title`, or to every Text when named `Text`.
style title { type: title; fg: text-primary; }

Column #face { align: center; gap: 16;
  Text #title .title { text: $title; lines: 2; text-align: center; }
  Button #play { size: 40; radius: radius-control; on-click: play;
    Icon PlayFilled { icon-size: 20; } }
}
```

| Syntax | Means |
|---|---|
| `Column`, `Row`, `Box`, `Stack`, `Spacer`, `Text`, `Icon`, `Path`, `Image`, `Button`, `Edit`, `Custom` | an element |
| `#name` | the element's id: `doc.id("name")` in Rust |
| `.style` | applies a `style` |
| `Text "Hello"`, `Icon Play` | the text or the glyph, written after the kind |
| `$name` | a binding: `doc.set(ui, "name", value)` sets it, and only the elements that read it change |
| `@key` | a string by key, from the app's string tables (`i18n`) |
| `on-click: name` | a click sends the action `name` to the app |
| `token name = value;` | a token of the sheet's own, before the theme's |
| `let name = expression;` | a value computed from the window and the bindings |
| `component Name(params) { … }` | a reusable piece; `Name(args)` or `doc.instantiate` makes one |

Bare names (`accent`, `body`, `radius-control`) are [theme tokens](/theming/). Numbers are dips.

## Expressions

Any property can be an expression. `window.w` and `window.h` are the window's size in dips;
`min`, `max`, `clamp`, `lerp`, `smoothstep`, `pow`, `sqrt`, `round` and the usual operators are
there; `latch` adds hysteresis and `tween(value, across, duration)` eases a value across a flip.

```scss
let compact = window.w < 720;
Column #side { width: compact ? 0 : 280; }
```

`Doc::update` (in `App::after_layout`) evaluates them, only when the window or a binding they
read changed, and re-applies a property only when its value changed.

## Lists

`repeat $list as row { … }` covers small lists. Long lists are a Rust widget,
[VirtualList](/components/virtual-list/), which makes only the rows in view.

## Loading

Sheets are parsed when the app loads them; an app may compile them in with `include_str!`. A
parse error is `Error::Parse { line, msg }`. Live reload is not built yet.
