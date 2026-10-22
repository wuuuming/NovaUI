---
title: 事件及其生命周期
description: 什么会到达 App::event、按什么顺序，以及每个阶段能做什么。
sidebar:
  order: 4
---

应用实现 `App`：

| 方法 | 何时调用 |
|---|---|
| `init` | 一次，在第一帧之前 |
| `event(ui, ev)` | 每个输入事件，以及应用自己句柄的每条消息 |
| `after_layout(ui)` | 在每个给应用送来东西的轮次的布局之后；`init` 之后也一次 |
| `nc_hit(ui, x, y)` | 宿主询问某点属于窗口哪一部分时（标题栏、客户区、边缘） |

## 事件

输入以 `Event` 到达：`Click`（带元素的动作）、`Hover`、`Pressed`、`DragStart`、`Drag`、`DragEnd`、`DragCancel`、`Wheel`、`Key`，文本框发出的 `Edit`、`Submit`、`Cancel`、`Blur`，按在打开的浮出层外时的 `PressOutside`，等等。元素声明自己想听什么：`on-click`、`hover-events`、`press-events`、`drag-events`、`wheel-events`。

**消息**是应用自己的值。句柄用消息创建，并以 `Event::Message` 投递：

| 句柄 | 创建 | 何时投递消息 |
|---|---|---|
| `Timer` | `ui.timer(msg)`，再 `at(t)` 或 `within(lo, hi)` | 到点时 |
| `Ticker` | `ui.ticker(msg)`，再 `start` / `stop` | 运行期间每次显示刷新一次 |
| `Task` | `ui.spawn(priority, work, done)` | 在 UI 线程外运行的工作完成时 |
| `Waker` | `ui.waker(msg)`，再从任意线程 `wake` | 下一轮 |
| `Subscription` | `model.subscribe(msg)` | 模型变化的同一轮 |

丢掉句柄就取消。消息只在句柄创建时存一次，所以 240 Hz 的 ticker 每帧不分配内存。应用按类型区分消息：`m.is::<MyMessage>()`。

## 一轮

```text
轮次（等交换链；只读一次时钟）
 1. 收集    系统输入、完成的任务、唤醒、到点的定时器
 2. 分发    按到达顺序的输入，然后任务结果，然后定时器   -> App::event
 3. 冲刷    模型通知，每轮每个订阅者一次                 -> App::event
 4. 布局    只在有东西需要时
 5. 测量    App::after_layout：可以再改一次布局
 6. 帧      只在有东西需要绘制或有 ticker 运行时：
            ticker -> App::event，补间推进到帧时间，布局，绘制，呈现
 7. 睡眠    直到输入、唤醒、最早的定时器或下一次刷新
```

一次输入引起的一切都在它的帧之前生效，所以没有一帧会显示改了一半的状态。动画读帧时间，从不读墙上时钟：它可以在任何一帧被打断，也从不需要“播完”。

## 每个阶段能做什么

| 阶段 | 可以写 | 不可以 |
|---|---|---|
| 分发、冲刷 | 模型、元素属性、句柄 | 读本轮的布局；阻塞在 I/O 上 |
| `after_layout` | 发起测量的视图的元素 | 通知模型；改两次布局 |
| ticker | 动画属性 | 增删元素；通知模型 |

这些规则会被检查：debug 构建中，以及任何设置了 `NOVAUI_AUDIT=1` 的构建中，违反即 panic。
