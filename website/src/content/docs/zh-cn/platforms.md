---
title: 平台与后端
description: novaui 有哪些宿主和渲染器，以及窗口在各平台上的样子。
---

## 后端

GPU 只有一份 WGSL 着色器源码；naga 把它翻译成 D3D11 的 HLSL 和 Vulkan 的 SPIR-V，所以两套 API 发出相同的绘制。软件渲染器是参照，每条 GPU 路径都要和它比对。

| 后端 | 平台 | 窗口 | 说明 |
|---|---|---|---|
| `d3d11` | Windows | flip 模型交换链、`Present1` 脏矩形、帧延迟对象 | Windows 默认 |
| `vk` | Windows、Linux | FIFO 交换链，按图像保存损坏区域 | Linux 默认；Windows 上 `Options::backend` 为 `BackendKind::Vulkan` 时 |
| `sw` | 全部 | GDI（Win32）/ `XPutImage`（X11） | 后备、参照 |

## 宿主

| 宿主 | 何时选用 | 输入法 | 阴影和边缘 |
|---|---|---|---|
| Win32 | Windows | IMM32：行内组字，候选窗在光标处 | DWM：Windows 11 上圆角和 1 像素边框，Windows 10 上由 novaui 画 1 像素边 |
| Wayland | Wayland 会话 | 尚无（`text-input-v3`） | 客户端绘制；KDE 上用 KWin 的阴影协议 |
| X11 | 其他情况，或 `NOVAUI_X11=1` | XIM，只有上屏文字 | KWin 上用 `_KDE_NET_WM_SHADOW` |

所有窗口都没有系统边框：应用画自己的标题栏，并回答 `App::nc_hit`。在标题栏上按下先是给应用的普通按下（会关闭浮出菜单），然后才是平台的标题栏动作：拖动移动、双击最大化、右键打开窗口菜单。

合成器提供时，Wayland 使用分数缩放。Windows 上窗口缩放跟随显示器 DPI。

## 环境变量

| 变量 | 作用 |
|---|---|
| `NOVAUI_FONTS=<dir>` | 附带字体所在位置（默认是可执行文件旁的 `fonts/`） |
| `NOVAUI_TRACE=<file>` | 每秒一行的运行时计数（[性能](/zh-cn/performance/)） |
| `NOVAUI_AUDIT=1` | 在 release 构建里打开阶段检查和损坏审计 |
| `NOVAUI_TEXT_LOG` | 记录字形图集重来 |
| `NOVAUI_X11=1` | 在 Wayland 会话里强制使用 X11 |

## 还没有的

彩色 emoji、一个进程多个主窗口（应用可以在旁边打开[浮层](/zh-cn/components/overlay/)）、Vulkan 上的 HDR 输出、GLES 和 Metal 后端、多行文本、滑块、表格。
