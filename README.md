<h1 align="center">NovaUI</h1>

<p align="center">A retained-mode GUI framework for Rust that only redraws what changed.</p>

<p align="center">
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue" alt="License"></a>
  <img src="https://img.shields.io/badge/platform-Windows%20%7C%20Linux-lightgrey" alt="Platforms">
  <img src="https://img.shields.io/badge/rust-stable-orange" alt="Rust stable">
</p>

<p align="center">English | <a href="README.zh-CN.md">简体中文</a></p>

<p align="center"><img src="website/public/shots/material.png" alt="The same NovaUI component built under Material 3 dark and light themes" width="800"></p>

NovaUI is the framework [NovaPlayer](https://github.com/wuuuming/NovaPlayer) is drawn with. You
describe a window's structure and look in `.nui` sheets, keep your state and logic in Rust, and
NovaUI keeps the tree, lays it out and paints it. Every change records what it invalidates, so a
frame only does the work that change needs. A window where nothing moves runs no frames at all,
and when something does change, only the damaged part of the window is repainted and presented.

It runs on Windows (Win32, with D3D11 or Vulkan) and Linux (Wayland or X11, with Vulkan), and
has a software renderer that the tests compare the GPU ones against pixel by pixel.

> [!WARNING]
> NovaUI is young and grew up inside one application. The API still changes, it isn't on
> crates.io, and the widgets are the ones NovaPlayer needed. Pin it to a commit.

## A small example

The look, in `hello.nui`:

```scss
Column #root { width: 100%; height: 100%; bg: solid-base; padding: 24; gap: 12;
  Text "Hello, NovaUI" { type: title; }
  Text #count { text: $count; fg: text-secondary; }
  Button #add { align-self: start; height: 32; padding: 0, 16; radius: radius-control;
                bg: accent; on-click: add;
    Text "Click me" { fg: black; }
  }
}
```

and the behaviour, in Rust:

```rust
use novaui::dsl::{parse, Doc};
use novaui::{App, Event, NcHit, Ui};

struct Hello { doc: Doc, clicks: u32 }

impl Hello {
    fn new() -> Self {
        Hello { doc: Doc::new(parse(include_str!("hello.nui")).unwrap()), clicks: 0 }
    }
}

impl App for Hello {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).unwrap();
        self.doc.set(ui, "count", "Not clicked yet.");
    }

    fn event(&mut self, ui: &mut Ui, ev: Event) {
        if let Event::Click { action: Some(a), .. } = ev {
            if a.is("add") {
                self.clicks += 1;
                self.doc.set(ui, "count", format!("Clicked {} time(s).", self.clicks));
            }
        }
    }

    fn after_layout(&mut self, ui: &mut Ui) {
        self.doc.update(ui); // the sheet's expressions follow the window and the bindings
    }

    fn nc_hit(&self, _: &Ui, _: i32, _: i32) -> NcHit {
        NcHit::Client // no custom title bar here
    }
}
```

A click sets `$count`, which lays out and repaints that one line of text and nothing else. The
full version is [`examples/docs_hello.rs`](crates/novaui/examples/docs_hello.rs); the gallery
shows everything else:

```sh
cargo run -p novaui --example docs_hello
cargo run -p novaui --example gallery
```

## What's in it

- `.nui` sheets: elements, styles, variables, expressions and bindings, components with
  parameters, and live reload while you edit them (debug builds, with an inspector on F12).
- Flexbox and block layout (through [Taffy](https://github.com/DioxusLabs/taffy)), and animation
  of opacity, position, scale and scroll that doesn't touch layout.
- Text: variable fonts, shaping with bidi and proper line breaking, fallback per character (CJK,
  Arabic, Hebrew, Devanagari, Thai), and input methods on Windows, Wayland and X11.
- Widgets: flyouts and menus, virtual lists, scrollbars, text fields, switches, QR codes, image
  effects written in WGSL, and 89 Fluent icons.
- Themes as tokens: WinUI dark, Material 3 dark and light, or your own.
- Windows that draw their own title bar, an acrylic material, HDR output on D3D11.
- Keyboard focus and an accessibility tree for screen readers ([AccessKit](https://github.com/AccessKit/accesskit)),
  both optional.

## How it compares

If you're choosing a Rust GUI library, the established ones are probably a better bet today, and
they're worth knowing to see where NovaUI sits:

- [egui](https://github.com/emilk/egui) is immediate mode: you describe the UI every frame. It's
  wonderfully quick to write tools with. NovaUI keeps a tree instead and does nothing between
  changes, which matters for an app that stays open all day.
- [iced](https://github.com/iced-rs/iced) follows the Elm architecture, rebuilding a view from
  your state. In NovaUI you change the tree (or a bound value) directly, and the tree knows what
  that dirtied.
- [Slint](https://github.com/slint-ui/slint) is the closest in spirit: a markup language for the
  look, code for the logic. Slint compiles its `.slint` files; NovaUI reads `.nui` at run time, so
  a sheet can be reloaded in place or shipped as a style package.
- [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), Zed's framework, is built
  for a code editor on the GPU. NovaUI is built for a media app and tuned for desktop look and feel
  on Windows first.

There's no macOS, mobile or web support yet. If you'd like to see one, an issue (or a pull
request) is the way to start.

## Getting started

```toml
[dependencies]
novaui = { git = "https://github.com/wuuuming/NovaUI", rev = "<full commit hash>" }
```

Windows 10 or 11, or Linux with Wayland or X11. On Linux, `libwayland-client`, `libxkbcommon`,
`libX11` and a Vulkan driver are loaded at run time, so building needs none of them.

The [documentation site](https://wuuuming.github.io/NovaUI/) has guides, a page with a live
screenshot for every element and widget, every icon, and theming. Behind it are the
[specification](docs/novaui-spec.md) (elements, layout, text, `.nui`, backends), the
[runtime rules](docs/runtime.md) (events, ordering, ownership, performance) and
[the map](docs/map.md) (where everything lives).

## Contributing

Issues and pull requests are welcome; please read [CONTRIBUTING.md](CONTRIBUTING.md) first.

## License

Either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option. The Fluent UI System
Icons outlines in `crates/nova-icons` keep their MIT licence
([`assets/LICENSE`](crates/nova-icons/assets/LICENSE)).
