// Builds nova-icons' embedded outline data from the committed Fluent UI System Icons SVGs.
//
//   node tools/icons/gen.mjs fetch     download every SVG the manifest names (16/20/24 masters that
//                                      exist) and the MIT LICENSE, at the pinned upstream commit,
//                                      into crates/nova-icons/assets/ — the only step that needs a
//                                      network, and only when the manifest gains an outline
//   node tools/icons/gen.mjs           write crates/nova-icons/data/paths.bin and src/generated.rs
//   node tools/icons/gen.mjs --check   fail if either generated file differs from what it would write
//
// Inputs: crates/nova-icons/manifest.tsv (the icon table), crates/nova-icons/segoe-boxes.tsv (where
// each legacy key's Segoe glyph sat in its em box) and crates/nova-icons/assets/svg/*.svg.
//
// Paths are kept as curves. Coordinates are quantized to 1/256 of a master unit (i16), and every
// master's ink bounds are computed exactly — curve extrema included — from the quantized points, so
// the bounds the runtime reads are the bounds of the geometry it fills.
import { readFileSync, writeFileSync, readdirSync, existsSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const UPSTREAM = 'microsoft/fluentui-system-icons';
const UPSTREAM_COMMIT = '8512d0121f6abd6c8c40f0bc4eb502ccd66ce6e7';
const MASTER_SIZES = [16, 20, 24];
const QUANTUM = 256;
const SEGOE_UNITS_PER_EM = 2048;

const root = join(fileURLToPath(new URL('.', import.meta.url)), '..', '..');
const crate = join(root, 'crates', 'nova-icons');
const svgDir = join(crate, 'assets', 'svg');

function rows(path) {
  return readFileSync(path, 'utf8')
    .split(/\r?\n/)
    .filter((line) => line.trim() && !line.startsWith('#'))
    .map((line) => line.split('\t').map((cell) => cell.trim()));
}

function snake(name) {
  return name.toLowerCase().replace(/ /g, '_');
}

function readManifest() {
  const icons = rows(join(crate, 'manifest.tsv')).map(([id, key, fluent, variant, box, usedBy], index) => {
    if (!/^[A-Z][A-Za-z0-9]*$/.test(id)) throw new Error(`manifest row ${index}: bad id ${id}`);
    if (!/^[0-9A-F]{4,5}$/.test(key)) throw new Error(`manifest row ${index}: bad key ${key}`);
    if (variant !== 'regular' && variant !== 'filled') throw new Error(`manifest row ${index}: bad variant ${variant}`);
    return { id, key: parseInt(key, 16), fluent, variant, box, usedBy: usedBy ?? '' };
  });
  const ids = new Set();
  const keys = new Set();
  for (const icon of icons) {
    if (ids.has(icon.id)) throw new Error(`duplicate id ${icon.id}`);
    if (keys.has(icon.key)) throw new Error(`duplicate key ${icon.key.toString(16)}`);
    ids.add(icon.id);
    keys.add(icon.key);
  }
  return icons;
}

function readSegoeBoxes() {
  const boxes = new Map();
  for (const [code, x0, y0, x1, y1] of rows(join(crate, 'segoe-boxes.tsv'))) {
    boxes.set(parseInt(code, 16), [x0, y0, x1, y1].map(Number));
  }
  return boxes;
}

// A `fluent` cell of `nova:<Name>` names one of the kernel's own outlines, drawn for a mark Fluent
// has no match for; it is committed beside the upstream SVGs and never fetched.
const OWN = 'nova:';

function outlineFile(fluent, size, variant) {
  if (fluent.startsWith(OWN)) return `ic_nova_${snake(fluent.slice(OWN.length))}_${size}_${variant}.svg`;
  return `ic_fluent_${snake(fluent)}_${size}_${variant}.svg`;
}

async function fetchAssets(icons) {
  mkdirSync(svgDir, { recursive: true });
  const base = `https://raw.githubusercontent.com/${UPSTREAM}/${UPSTREAM_COMMIT}`;
  const license = await fetch(`${base}/LICENSE`);
  if (!license.ok) throw new Error(`LICENSE: HTTP ${license.status}`);
  writeFileSync(join(crate, 'assets', 'LICENSE'), await license.text());
  const wanted = new Set(icons.filter((icon) => !icon.fluent.startsWith(OWN)).map((icon) => `${icon.fluent}\u0000${icon.variant}`));
  for (const entry of wanted) {
    const [fluent, variant] = entry.split('\u0000');
    let found = 0;
    for (const size of MASTER_SIZES) {
      const file = outlineFile(fluent, size, variant);
      const response = await fetch(`${base}/assets/${encodeURIComponent(fluent)}/SVG/${file}`);
      if (response.status === 404) continue;
      if (!response.ok) throw new Error(`${file}: HTTP ${response.status}`);
      writeFileSync(join(svgDir, file), await response.text());
      found += 1;
    }
    if (!found) throw new Error(`no ${MASTER_SIZES.join('/')} master exists for ${fluent} ${variant}`);
    console.log(`fetched ${fluent} ${variant}: ${found} master(s)`);
  }
}

// ---- SVG path parsing ---------------------------------------------------------------------------

function parseSvg(text, file) {
  const viewBox = /viewBox="([^"]*)"/.exec(text);
  if (!viewBox) throw new Error(`${file}: no viewBox`);
  const [vx, vy, vw, vh] = viewBox[1].trim().split(/[\s,]+/).map(Number);
  if (vx !== 0 || vy !== 0 || vw !== vh) throw new Error(`${file}: unsupported viewBox ${viewBox[1]}`);
  if (/<(?:g|circle|rect|ellipse|line|polyline|polygon|use)\b/.test(text) || /\btransform=|fill-rule="evenodd"|clip-/.test(text)) {
    throw new Error(`${file}: holds an element or attribute the generator does not translate`);
  }
  const paths = [...text.matchAll(/<path\b[^>]*\sd="([^"]*)"/g)].map((match) => match[1]);
  if (!paths.length) throw new Error(`${file}: no path`);
  return { size: vw, commands: paths.flatMap((d) => parsePathData(d, file)) };
}

// Absolute M/L/C/Q/Z commands from SVG path data (M L H V C S Q T Z, absolute or relative).
function parsePathData(d, file) {
  const tokens = [...d.matchAll(/[MmLlHhVvCcSsQqTtZzAa]|-?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?/g)].map((m) => m[0]);
  const out = [];
  let i = 0;
  let command = null;
  let current = [0, 0];
  let start = [0, 0];
  let lastControl = null;
  let lastKind = null;
  const number = () => {
    const token = tokens[i++];
    if (token === undefined || /[A-Za-z]/.test(token)) throw new Error(`${file}: path data ends inside a command`);
    return Number(token);
  };
  while (i < tokens.length) {
    if (/[A-Za-z]/.test(tokens[i])) command = tokens[i++];
    else if (command === null) throw new Error(`${file}: path data starts with a number`);
    const relative = command === command.toLowerCase();
    const at = (x, y) => (relative ? [current[0] + x, current[1] + y] : [x, y]);
    switch (command.toUpperCase()) {
      case 'M': {
        current = at(number(), number());
        start = current;
        out.push(['M', current]);
        command = relative ? 'l' : 'L';
        lastKind = 'M';
        break;
      }
      case 'L': current = at(number(), number()); out.push(['L', current]); lastKind = 'L'; break;
      case 'H': { const x = number(); current = [relative ? current[0] + x : x, current[1]]; out.push(['L', current]); lastKind = 'L'; break; }
      case 'V': { const y = number(); current = [current[0], relative ? current[1] + y : y]; out.push(['L', current]); lastKind = 'L'; break; }
      case 'C': {
        const c1 = at(number(), number()); const c2 = at(number(), number()); const to = at(number(), number());
        out.push(['C', c1, c2, to]); lastControl = c2; current = to; lastKind = 'C'; break;
      }
      case 'S': {
        const c1 = lastKind === 'C' ? [2 * current[0] - lastControl[0], 2 * current[1] - lastControl[1]] : current;
        const c2 = at(number(), number()); const to = at(number(), number());
        out.push(['C', c1, c2, to]); lastControl = c2; current = to; lastKind = 'C'; break;
      }
      case 'Q': {
        const c = at(number(), number()); const to = at(number(), number());
        out.push(['Q', c, to]); lastControl = c; current = to; lastKind = 'Q'; break;
      }
      case 'T': {
        const c = lastKind === 'Q' ? [2 * current[0] - lastControl[0], 2 * current[1] - lastControl[1]] : current;
        const to = at(number(), number());
        out.push(['Q', c, to]); lastControl = c; current = to; lastKind = 'Q'; break;
      }
      case 'Z': out.push(['Z']); current = start; lastKind = 'Z'; break;
      default: throw new Error(`${file}: path command ${command} is not translated`);
    }
  }
  return out;
}

// ---- Quantization and exact bounds --------------------------------------------------------------

const quantize = (value) => {
  const q = Math.round(value * QUANTUM);
  if (q < -32768 || q > 32767) throw new Error(`coordinate ${value} leaves the i16 range`);
  return q;
};

function extend(bounds, [x, y]) {
  bounds[0] = Math.min(bounds[0], x); bounds[1] = Math.min(bounds[1], y);
  bounds[2] = Math.max(bounds[2], x); bounds[3] = Math.max(bounds[3], y);
}

// Roots in (0, 1) of the derivative of a 1-D quadratic (a, b, c) or cubic (a, b, c, d) Bézier.
function quadExtrema(a, b, c) {
  const denominator = a - 2 * b + c;
  if (denominator === 0) return [];
  const t = (a - b) / denominator;
  return t > 0 && t < 1 ? [t] : [];
}

function cubicExtrema(a, b, c, d) {
  // B'(t)/3 = (b-a)(1-t)^2 + 2(c-b)(1-t)t + (d-c)t^2 = A t^2 + B t + C
  const A = -a + 3 * b - 3 * c + d;
  const B = 2 * (a - 2 * b + c);
  const C = b - a;
  const roots = [];
  if (Math.abs(A) < 1e-12) {
    if (B !== 0) roots.push(-C / B);
  } else {
    const disc = B * B - 4 * A * C;
    if (disc >= 0) {
      const s = Math.sqrt(disc);
      roots.push((-B + s) / (2 * A), (-B - s) / (2 * A));
    }
  }
  return roots.filter((t) => t > 0 && t < 1);
}

function inkBounds(commands) {
  const bounds = [Infinity, Infinity, -Infinity, -Infinity];
  let current = [0, 0];
  for (const [verb, ...points] of commands) {
    if (verb === 'M' || verb === 'L') { extend(bounds, points[0]); current = points[0]; }
    else if (verb === 'Q') {
      const [c, to] = points;
      extend(bounds, current); extend(bounds, to);
      for (const axis of [0, 1]) {
        for (const t of quadExtrema(current[axis], c[axis], to[axis])) {
          const u = 1 - t;
          extend(bounds, [0, 1].map((k) => u * u * current[k] + 2 * u * t * c[k] + t * t * to[k]));
        }
      }
      current = to;
    } else if (verb === 'C') {
      const [c1, c2, to] = points;
      extend(bounds, current); extend(bounds, to);
      for (const axis of [0, 1]) {
        for (const t of cubicExtrema(current[axis], c1[axis], c2[axis], to[axis])) {
          const u = 1 - t;
          extend(bounds, [0, 1].map((k) => u * u * u * current[k] + 3 * u * u * t * c1[k] + 3 * u * t * t * c2[k] + t * t * t * to[k]));
        }
      }
      current = to;
    }
  }
  return bounds;
}

// ---- Generation ---------------------------------------------------------------------------------

const VERB = { M: 0, L: 1, Q: 2, C: 3, Z: 4 };

function f32(value) {
  const rounded = Math.fround(value);
  const text = String(rounded);
  return /[.eE]/.test(text) ? text : `${text}.0`;
}

function rustString(text) {
  return JSON.stringify(text);
}

function generate() {
  const icons = readManifest();
  const segoe = readSegoeBoxes();
  const verbs = [];
  const coords = [];
  const masters = [];
  const outlines = [];
  const outlineIndex = new Map();
  for (const icon of icons) {
    const name = `${icon.fluent}\u0000${icon.variant}`;
    if (outlineIndex.has(name)) continue;
    const first = masters.length;
    for (const size of MASTER_SIZES) {
      const file = outlineFile(icon.fluent, size, icon.variant);
      const path = join(svgDir, file);
      if (!existsSync(path)) continue;
      const svg = parseSvg(readFileSync(path, 'utf8'), file);
      if (svg.size !== size) throw new Error(`${file}: viewBox is ${svg.size}, not ${size}`);
      const verbStart = verbs.length;
      const coordStart = coords.length / 2;
      const quantized = svg.commands.map(([verb, ...points]) => [verb, ...points.map(([x, y]) => [quantize(x), quantize(y)])]);
      for (const [verb, ...points] of quantized) {
        verbs.push(VERB[verb]);
        for (const [x, y] of points) coords.push(x, y);
      }
      const bounds = inkBounds(quantized.map(([verb, ...points]) => [verb, ...points.map(([x, y]) => [x / QUANTUM, y / QUANTUM])]));
      masters.push({ design: size, verbStart, verbLen: verbs.length - verbStart, coordStart, coordLen: coords.length / 2 - coordStart, ink: bounds, file });
    }
    if (masters.length === first) throw new Error(`no committed master for ${icon.fluent} ${icon.variant}; run \`node tools/icons/gen.mjs fetch\``);
    outlineIndex.set(name, outlines.length);
    outlines.push({ fluent: icon.fluent, variant: icon.variant, first, count: masters.length - first });
  }
  const unused = readdirSync(svgDir).filter((file) => file.endsWith('.svg') && !masters.some((master) => master.file === file));
  if (unused.length) throw new Error(`committed SVGs no icon uses: ${unused.join(', ')}`);

  // Where each icon's ink goes in its em box (em units, y down).
  const place = (icon) => {
    const outline = outlines[outlineIndex.get(`${icon.fluent}\u0000${icon.variant}`)];
    if (icon.box === 'native') {
      const master = masters[outline.first + outline.count - 1];
      return master.ink.map((value) => value / master.design);
    }
    const code = icon.box === 'segoe' ? icon.key : parseInt(/^segoe:([0-9A-F]{4,5})$/.exec(icon.box)?.[1] ?? 'x', 16);
    const box = segoe.get(code);
    if (!box) throw new Error(`${icon.id}: no Segoe box for ${icon.box}`);
    const [x0, y0, x1, y1] = box;
    return [x0 / SEGOE_UNITS_PER_EM, 1 - y1 / SEGOE_UNITS_PER_EM, x1 / SEGOE_UNITS_PER_EM, 1 - y0 / SEGOE_UNITS_PER_EM];
  };

  // paths.bin: "NVIC", u16 version, u16 zero, u32 verb count, u32 coordinate count (i16 values),
  // the verbs, one pad byte when their count is odd, then the coordinates little-endian.
  const pad = verbs.length % 2;
  const blob = Buffer.alloc(16 + verbs.length + pad + coords.length * 2);
  blob.write('NVIC', 0, 'latin1');
  blob.writeUInt16LE(1, 4);
  blob.writeUInt16LE(0, 6);
  blob.writeUInt32LE(verbs.length, 8);
  blob.writeUInt32LE(coords.length, 12);
  Buffer.from(verbs).copy(blob, 16);
  coords.forEach((value, index) => blob.writeInt16LE(value, 16 + verbs.length + pad + index * 2));

  const lines = [];
  const push = (line = '') => lines.push(line);
  push('// @generated by `node tools/icons/gen.mjs` from crates/nova-icons/manifest.tsv — do not edit.');
  push(`// Upstream: https://github.com/${UPSTREAM} at ${UPSTREAM_COMMIT} (MIT, see assets/LICENSE).`);
  push();
  push('use crate::{IconEntry, MasterEntry, OutlineEntry, Variant};');
  push();
  push('/// Every icon the kernel draws: one row of `manifest.tsv` each.');
  push('///');
  push('/// | Icon | Key | Fluent UI System Icons | Variant | Masters | Placement | Used by |');
  push('/// |---|---|---|---|---|---|---|');
  for (const icon of icons) {
    const outline = outlines[outlineIndex.get(`${icon.fluent}\u0000${icon.variant}`)];
    const sizes = masters.slice(outline.first, outline.first + outline.count).map((master) => master.design).join('/');
    const placement = icon.box === 'native' ? 'own 24-grid ink box' : `Segoe U+${icon.box === 'segoe' ? icon.key.toString(16).toUpperCase() : icon.box.slice(6)} box`;
    push(`/// | \`${icon.id}\` | U+${icon.key.toString(16).toUpperCase()} | ${icon.fluent} | ${icon.variant === 'filled' ? 'Filled' : 'Regular'} | ${sizes} | ${placement} | ${icon.usedBy} |`);
  }
  push('#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]');
  push('#[repr(u8)]');
  push('pub enum Icon {');
  for (const icon of icons) {
    push(`    /// Fluent \`${icon.fluent}\` ${icon.variant === 'filled' ? 'Filled' : 'Regular'}; key U+${icon.key.toString(16).toUpperCase()}.${icon.usedBy ? ` ${icon.usedBy}` : ''}`);
    push(`    ${icon.id},`);
  }
  push('}');
  push();
  push('impl Icon {');
  push(`    /// Every icon, in manifest order.`);
  push(`    pub const ALL: [Icon; ${icons.length}] = [`);
  for (const icon of icons) push(`        Icon::${icon.id},`);
  push('    ];');
  push('}');
  push();
  push(`pub(crate) static ICONS: [IconEntry; ${icons.length}] = [`);
  for (const icon of icons) {
    const key = icon.key.toString(16).toUpperCase();
    push(`    IconEntry { name: ${rustString(icon.id)}, key: '\\u{${key}}', key_str: "\\u{${key}}", fluent: ${rustString(icon.fluent)}, variant: Variant::${icon.variant === 'filled' ? 'Filled' : 'Regular'}, outline: ${outlineIndex.get(`${icon.fluent}\u0000${icon.variant}`)}, place: [${place(icon).map(f32).join(', ')}] },`);
  }
  push('];');
  push();
  push('/// `(key, icon)` sorted by key, for `Icon::from_key`.');
  push(`pub(crate) static KEYS: [(char, Icon); ${icons.length}] = [`);
  for (const icon of [...icons].sort((a, b) => a.key - b.key)) push(`    ('\\u{${icon.key.toString(16).toUpperCase()}}', Icon::${icon.id}),`);
  push('];');
  push();
  push(`pub(crate) static OUTLINES: [OutlineEntry; ${outlines.length}] = [`);
  for (const outline of outlines) push(`    OutlineEntry { first_master: ${outline.first}, masters: ${outline.count} }, // ${outline.fluent} ${outline.variant}`);
  push('];');
  push();
  push(`pub(crate) static MASTERS: [MasterEntry; ${masters.length}] = [`);
  for (const master of masters) {
    push(`    MasterEntry { design: ${master.design}, verbs: ${master.verbStart}, verb_len: ${master.verbLen}, points: ${master.coordStart}, point_len: ${master.coordLen}, ink: [${master.ink.map(f32).join(', ')}] }, // ${master.file}`);
  }
  push('];');
  push();
  return { blob, source: lines.join('\n'), icons, outlines, masters };
}

const mode = process.argv[2];
if (mode === 'fetch') {
  await fetchAssets(readManifest());
} else {
  const { blob, source, icons, outlines, masters } = generate();
  const blobPath = join(crate, 'data', 'paths.bin');
  const sourcePath = join(crate, 'src', 'generated.rs');
  if (mode === '--check') {
    const same = existsSync(blobPath) && existsSync(sourcePath)
      && Buffer.compare(readFileSync(blobPath), blob) === 0
      && readFileSync(sourcePath, 'utf8') === source;
    if (!same) {
      console.error('nova-icons generated data is stale: run `node tools/icons/gen.mjs`');
      process.exit(1);
    }
    console.log(`nova-icons generated data is current: ${icons.length} icons, ${outlines.length} outlines, ${masters.length} masters, ${blob.length} bytes`);
  } else if (mode === undefined) {
    mkdirSync(join(crate, 'data'), { recursive: true });
    writeFileSync(blobPath, blob);
    writeFileSync(sourcePath, source);
    console.log(`wrote ${icons.length} icons, ${outlines.length} outlines, ${masters.length} masters, ${blob.length} bytes`);
  } else {
    console.error('usage: node tools/icons/gen.mjs [fetch | --check]');
    process.exit(2);
  }
}
