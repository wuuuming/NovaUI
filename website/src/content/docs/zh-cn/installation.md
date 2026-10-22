---
title: 安装
description: 把 novaui 加进 Cargo 项目，以及运行时需要什么。
---

NovaUI 还没有发布到 crates.io。请依赖仓库并钉住一个提交：

```toml
[dependencies]
novaui = { git = "https://github.com/wuuuming/NovaUI", rev = "<完整提交哈希>" }
```

工作区里有两个 crate：

| Crate | 是什么 |
|---|---|
| `novaui` | 框架本身：元素树、布局、文字、`.nui` 语言、后端、宿主、部件 |
| `nova-icons` | [图标](/zh-cn/icons/)：Fluent UI System Icons 的轮廓和一个 CPU 光栅器；`novaui` 重新导出 `Icon` |

`backend` 特性导出渲染器接口和软件渲染器（`novaui::backend`），供无窗口的驱动使用：测试，或者像本站的 `docs-shots` 那样渲染截图的工具。

## 要求

- **Rust**：stable，edition 2021。
- **Windows**：Windows 10 或 11。默认后端是 D3D11；要求时使用 Vulkan。
- **Linux**：运行时需要 `libwayland-client`、`libxkbcommon`、`libX11` 和 Vulkan 驱动。它们都是动态加载的，构建时一个都不需要。

## 字体

有 Noto 可变字体时用它们绘制文字：先 Noto Sans，再 Noto Sans SC、JP、KR，然后是 Noto Sans Arabic、Hebrew、Devanagari、Thai。NovaUI 在可执行文件旁的 `fonts/` 文件夹里找，或在 `NOVAUI_FONTS` 指定的文件夹里找。找不到时使用系统字体（Windows 上是 Segoe UI Variable 和 Microsoft YaHei UI；Linux 上是 Noto 系列或 DejaVu）。随应用附带字体，文字在每台机器上就都一样。

## 运行示例

```sh
cargo run -p novaui --example gallery
cargo run -p novaui --example docs_button
```

本站每一页都写明它的代码来自哪个示例。
