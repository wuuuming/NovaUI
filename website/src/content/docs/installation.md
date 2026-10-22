---
title: Installation
description: Adding novaui to a Cargo project, and what it needs at run time.
---

NovaUI is not on crates.io yet. Depend on the repository, pinned to a commit:

```toml
[dependencies]
novaui = { git = "https://github.com/wuuuming/NovaUI", rev = "<full commit hash>" }
```

The workspace has two crates:

| Crate | What it is |
|---|---|
| `novaui` | the framework: tree, layout, text, the `.nui` language, backends, hosts, widgets |
| `nova-icons` | the [icons](/icons/): Fluent UI System Icons outlines and a CPU rasterizer; `novaui` re-exports `Icon` |

The `backend` feature exports the renderer interface and the software renderer
(`novaui::backend`) for headless drivers: tests, or a tool that renders screenshots, as this
site's `docs-shots` does.

## Requirements

- **Rust**: stable, edition 2021.
- **Windows**: Windows 10 or 11. D3D11 is the default backend; Vulkan is used when asked for.
- **Linux**: `libwayland-client`, `libxkbcommon`, `libX11` and a Vulkan driver at run time. All
  are loaded dynamically, so building needs none of them.

## Fonts

Text is drawn with the Noto variable fonts when they are present: Noto Sans, then Noto Sans SC,
JP and KR, then Noto Sans Arabic, Hebrew, Devanagari and Thai. NovaUI looks for them in a `fonts/` folder beside the executable, or in the folder
`NOVAUI_FONTS` names. Without them it uses the system's fonts (Segoe UI Variable and Microsoft
YaHei UI on Windows; the Noto families or DejaVu on Linux). Shipping the fonts makes text look
the same on every machine.

## Running the examples

```sh
cargo run -p novaui --example gallery
cargo run -p novaui --example docs_button
```

Every page of this site names the example its code comes from.
