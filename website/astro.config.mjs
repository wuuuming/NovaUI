// The novaui documentation site. `bun run dev` serves it; `bun run build` writes `dist/`.
// NOVAUI_DOCS_BASE sets the path it is served under: `/` by default (and for the link check); on
// GitHub Pages `/NovaUI/` for the latest release and `/NovaUI/versions/main/` for `main`
// (`.github/workflows/pages.yml`).
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { unified } from '@astrojs/markdown-remark';
import starlightLinksValidator from 'starlight-links-validator';

const sidebar = [
  { label: 'Start', translations: { 'zh-CN': '开始' }, items: ['index', 'installation', 'getting-started'] },
  { label: 'Concepts', translations: { 'zh-CN': '概念' }, items: [{ autogenerate: { directory: 'concepts' } }] },
  {
    label: 'Elements',
    translations: { 'zh-CN': '元素' },
    items: [
      'components/box', 'components/column', 'components/row', 'components/stack', 'components/spacer',
      'components/text', 'components/icon', 'components/path', 'components/image', 'components/button',
      'components/edit', 'components/custom',
    ],
  },
  {
    label: 'Widgets',
    translations: { 'zh-CN': '部件' },
    items: ['components/flyout', 'components/virtual-list', 'components/scrollbar', 'components/qr'],
  },
  { label: 'Guides', translations: { 'zh-CN': '指南' }, items: ['icons', 'theming', 'platforms', 'performance', 'licence'] },
];

const base = process.env.NOVAUI_DOCS_BASE ?? '/';

/** Pages link to each other as `/getting-started/`; under another base (`/NovaUI/`) the
 * links are given it. */
function rehypeBase() {
  const prefix = base.replace(/\/$/, '');
  const walk = (node) => {
    const href = node.properties?.href;
    if (node.tagName === 'a' && typeof href === 'string' && href.startsWith('/') && !href.startsWith('//')) {
      node.properties.href = prefix + href;
    }
    node.children?.forEach(walk);
  };
  return (tree) => (prefix ? walk(tree) : undefined);
}

export default defineConfig({
  site: 'https://wuuuming.github.io',
  base,
  markdown: { processor: unified({ rehypePlugins: [rehypeBase] }) },
  integrations: [
    starlight({
      title: 'NovaUI',
      description: 'A retained, damage-driven UI framework for Rust.',
      defaultLocale: 'root',
      locales: {
        root: { label: 'English', lang: 'en' },
        'zh-cn': { label: '简体中文', lang: 'zh-CN' },
      },
      sidebar,
      customCss: ['./src/styles/site.css'],
      // Links are checked in the build at `/` (CI); under another base they are rewritten.
      plugins: base === '/' ? [starlightLinksValidator()] : [],
    }),
  ],
  // The recipes are read from `crates/novaui/examples/`, outside this folder.
  vite: { server: { fs: { allow: ['..'] } } },
});
