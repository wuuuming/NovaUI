# NovaUI documentation site

Astro + Starlight, in English (`src/content/docs/`) and 简体中文 (`src/content/docs/zh-cn/`).

```sh
npm install        # or: bun install
npm run dev        # http://localhost:4321
npm run build      # writes dist/ and checks every link
```

`NOVAUI_DOCS_BASE=/NovaUI/ npm run build` builds it to be served under another path. It is
published at <https://wuuuming.github.io/NovaUI/>: the latest release at the root, `main` under
`versions/main/` (`.github/workflows/pages.yml`).

## Code and screenshots

Every code sample is a recipe in `crates/novaui/examples/`: `docs_<name>.rs` and its
`docs_<name>.nui`, shown verbatim by `src/components/Recipe.astro` and compiled by CI with the
other examples. Every screenshot in `public/shots/` is rendered by NovaUI from those recipes, with
the software renderer and the bundled Noto fonts:

```sh
cargo run --release -p novaui --example docs-shots   # needs NOVAUI_FONTS: the Noto fonts folder
```

Run it again after changing a recipe or the renderer, and commit the shots with the change.
