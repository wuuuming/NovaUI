---
title: 元素与布局
description: 保留模式的元素树、元素由什么组成、怎样布局。
sidebar:
  order: 1
---

## 元素树

一个窗口有一棵元素树，归 `Ui::tree` 所有。一个元素由三部分组成：

| 部分 | 类型 | 内容 |
|---|---|---|
| 布局 | `Layout` | 尺寸、内边距、外边距、间距、方向、对齐、伸展与收缩、定位 |
| 外观 | `Visual` | 背景、边框、圆角、阴影、不透明度、裁剪、前景色、材质 |
| 内容 | `Content` | 无、文字、图标、路径、图片、图片效果 |

元素可以由样式表创建（`Doc::build`、`Doc::instantiate`），也可以在 Rust 里创建（`ui.tree.add(parent, layout, visual, content)`）；两者都返回一个 `Id`。`Id` 带检查：元素被移除后，它的 `Id` 不再指向任何东西，旧 `Id` 也不会和同一槽位里的新元素混淆。

元素归树所有。读用 `ui.tree.el(id)`；所有修改都经过 `Tree` 的 setter（`set_layout`、`set_visual`、`set_content`、`set_text`、`set_opacity`、`set_translate`、`set_lit`、`set_disabled`……），setter 知道这次修改让什么失效：

- 尺寸或所画内容变了，元素会**重新绘制**；
- 只是位置变了，元素只**移动**：保留已录好的绘制，画到新位置；
- 自带图层的元素改不透明度或平移，只影响图层的合成方式。

## 布局

布局是 flexbox，由 [Taffy](https://github.com/DioxusLabs/taffy) 计算，而且只算变化了的子树。

- `Column` 从上到下排子元素，`Row` 从左到右；`Box` 是一个 column。
- `align` 在交叉方向上放置子元素（`start`、`center`、`end`、`stretch`），`justify` 在主方向上放置（`start`、`center`、`end`、`between`）。
- `gap` 是子元素之间的距离，`padding` 是内边距，`margin` 是外边距。
- `grow` 分配剩余空间；`shrink` 让子元素可以让出空间；`Spacer` 就是一个会伸展的子元素。
- `width` 和 `height` 取 dip、百分比（`100%`）或 `auto`；`size` 是正方形。
- `left`、`top`、`right`、`bottom` 让元素脱离流、贴着父元素放置：`Stack` 的子元素就是这样重叠的。

所有长度都是 **dip**（与设备无关的像素）：窗口的缩放把它们换成物理像素。

## 图层

`layer: true` 给元素一张自己的纹理。独立变化的内容（滑动的面板、滚动的列表）放在图层上，它的变化就不会重新光栅化周围的东西，移动或淡入淡出它也不会重新光栅化任何东西。

## 另见

[Box](/zh-cn/components/box/)、[Column](/zh-cn/components/column/)、[Row](/zh-cn/components/row/)、[Stack](/zh-cn/components/stack/)、[Spacer](/zh-cn/components/spacer/)。
