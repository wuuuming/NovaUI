---
title: Elements and layout
description: The retained tree, what an element is made of, and how it is laid out.
sidebar:
  order: 1
---

## The tree

A window has one tree of elements, owned by `Ui::tree`. An element is three parts:

| Part | Type | Holds |
|---|---|---|
| layout | `Layout` | size, padding, margin, gap, direction, alignment, grow and shrink, position |
| look | `Visual` | background, border, radius, shadow, opacity, clip, foreground colour, material |
| content | `Content` | nothing, text, an icon, a path, an image, an image effect |

Elements are made from a sheet (`Doc::build`, `Doc::instantiate`) or in Rust
(`ui.tree.add(parent, layout, visual, content)`); both give an `Id`. An `Id` is checked: once
its element is removed, the `Id` refers to nothing, and a stale one is never confused with a new
element in the same slot.

The tree owns its elements. Reading is `ui.tree.el(id)`; every change goes through a `Tree`
setter (`set_layout`, `set_visual`, `set_content`, `set_text`, `set_opacity`, `set_translate`,
`set_lit`, `set_disabled`, …) that knows what the change invalidates:

- a change of size or of what is drawn **paints** the element again;
- a change of place only **moves** it: its recorded drawing is kept and drawn elsewhere;
- opacity and translation of an element on its own layer touch only how the layer is
  composited.

## Layout

Layout is flexbox, computed by [Taffy](https://github.com/DioxusLabs/taffy) and only for the
subtrees that changed.

- `Column` lays its children top to bottom, `Row` left to right; `Box` is a column.
- `align` places children across the direction (`start`, `center`, `end`, `stretch`), `justify`
  along it (`start`, `center`, `end`, `between`).
- `gap` is the space between children, `padding` the space inside, `margin` outside.
- `grow` shares the room that is left; `shrink` lets a child give room up; `Spacer` is a child
  that grows.
- `width` and `height` take dips, percentages (`100%`) or `auto`; `size` is a square.
- `left`, `top`, `right`, `bottom` take an element out of the flow and place it against its
  parent: children of a `Stack` overlap this way.

All lengths are **dips** (device-independent pixels): the window's scale turns them into
physical pixels.

## Layers

`layer: true` gives an element its own texture. Content that changes on its own (a panel that
slides, a list that scrolls) goes on a layer, so a change in it does not re-raster what is
around it, and moving or fading it re-rasters nothing.

## See also

[Box](/components/box/), [Column](/components/column/), [Row](/components/row/),
[Stack](/components/stack/), [Spacer](/components/spacer/).
