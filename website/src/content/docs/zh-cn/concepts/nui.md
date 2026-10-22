---
title: .nui 语言
description: 用一门小型声明式语言写结构、布局和样式；逻辑留在 Rust 里。
sidebar:
  order: 3
---

`.nui` 样式表描述窗口长什么样。它没有逻辑：应用的 Rust 代码改状态，样式表把状态呈现出来。

```scss
// 样式：用 `.title` 应用；命名为 `Text` 时应用到每个 Text。
style title { type: title; fg: text-primary; }

Column #face { align: center; gap: 16;
  Text #title .title { text: $title; lines: 2; text-align: center; }
  Button #play { size: 40; radius: radius-control; on-click: play;
    Icon PlayFilled { icon-size: 20; } }
}
```

| 写法 | 含义 |
|---|---|
| `Column`、`Row`、`Box`、`Stack`、`Spacer`、`Text`、`Icon`、`Path`、`Image`、`Button`、`Edit`、`Custom` | 一个元素 |
| `#name` | 元素的 id：在 Rust 里是 `doc.id("name")` |
| `.style` | 应用一个 `style` |
| `Text "Hello"`、`Icon Play` | 写在种类后面的文字或字形 |
| `$name` | 绑定：`doc.set(ui, "name", value)` 设置它，只有读它的元素会变 |
| `@key` | 按键取字符串，来自应用的字符串表（`i18n`） |
| `on-click: name` | 点击时向应用发送动作 `name` |
| `token name = value;` | 样式表自己的令牌，优先于主题的 |
| `let name = expression;` | 由窗口和绑定算出的值 |
| `component Name(params) { … }` | 可复用的片段；用 `Name(args)` 或 `doc.instantiate` 创建 |

不带前缀的名字（`accent`、`body`、`radius-control`）是[主题令牌](/zh-cn/theming/)。数字的单位是 dip。

## 表达式

任何属性都可以是表达式。`window.w` 和 `window.h` 是窗口的尺寸（dip）；有 `min`、`max`、`clamp`、`lerp`、`smoothstep`、`pow`、`sqrt`、`round` 和常见运算符；`latch` 提供滞回，`tween(value, across, duration)` 在条件翻转时让值缓动过去。

```scss
let compact = window.w < 720;
Column #side { width: compact ? 0 : 280; }
```

`Doc::update`（在 `App::after_layout` 里调用）求值它们：只在窗口或它们读的绑定变化时求值，并且只在值变化时重新应用属性。

## 列表

`repeat $list as row { … }` 适合短列表。长列表用 Rust 部件 [VirtualList](/zh-cn/components/virtual-list/)，它只创建可见的行。

## 加载

样式表在应用加载时解析；应用可以用 `include_str!` 把它编译进程序。解析错误是 `Error::Parse { line, msg }`。实时重载还没有做。
