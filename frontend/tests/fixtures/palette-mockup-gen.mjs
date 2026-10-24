// Generates the Apple-style schemes of the palette mockup as CSS token blocks. Never imported by the app.
// Usage (from frontend/): node tests/fixtures/palette-mockup-gen.mjs tests/fixtures/palette-mockup-schemes.css
//                         && npx prettier --write tests/fixtures/palette-mockup-schemes.css
// Prints a contrast report. Two axes, like macOS: a neutral base (iOS / macOS system grays) and an accent.
import { writeFileSync } from 'node:fs';

/* ══ Color math ══ */
const lin = c => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
const enc = c => (c <= 0.0031308 ? 12.92 * c : 1.055 * c ** (1 / 2.4) - 0.055);

function toLinear(L, C, H) {
  const a = C * Math.cos((H * Math.PI) / 180);
  const b = C * Math.sin((H * Math.PI) / 180);
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
}
const inGamut = rgb => rgb.every(v => v >= -1e-5 && v <= 1 + 1e-5);
const hexOfLinear = rgb =>
  '#' +
  rgb
    .map(v => Math.round(Math.min(1, Math.max(0, enc(v))) * 255))
    .map(v => v.toString(16).padStart(2, '0'))
    .join('');

/** OKLCH → hex; out-of-gamut colors keep L and H and lose chroma. */
function oklch(L, C, H) {
  if (inGamut(toLinear(L, C, H))) return hexOfLinear(toLinear(L, C, H));
  let lo = 0;
  let hi = C;
  for (let i = 0; i < 30; i += 1) {
    const mid = (lo + hi) / 2;
    if (inGamut(toLinear(L, mid, H))) lo = mid;
    else hi = mid;
  }
  return hexOfLinear(toLinear(L, lo, H));
}

const channels = hex => [1, 3, 5].map(i => parseInt(hex.slice(i, i + 2), 16) / 255);

function luminance(hex) {
  const [r, g, b] = channels(hex).map(lin);
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(a, b) {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
}

function lstar(hex) {
  const y = luminance(hex);
  return y > 0.008856 ? 116 * Math.cbrt(y) - 16 : 903.3 * y;
}

function toOklch(hex) {
  const [r, g, b] = channels(hex).map(lin);
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  const L = 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s;
  const A = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
  const B = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
  let H = (Math.atan2(B, A) * 180) / Math.PI;
  if (H < 0) H += 360;
  return { L, C: Math.hypot(A, B), H };
}

function rgba(hex, alpha) {
  const [r, g, b] = channels(hex).map(v => Math.round(v * 255));
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

const hueDistance = (a, b) => {
  const d = Math.abs(a - b) % 360;
  return d > 180 ? 360 - d : d;
};

/** Shift OKLCH lightness (hue and chroma kept, chroma clipped to the gamut). */
function shift(hex, dL) {
  const { L, C, H } = toOklch(hex);
  return oklch(Math.min(1, Math.max(0, L + dL)), C, H);
}

/** The color closest to `hex` along its own hue that reaches `min` contrast against every background.
    Dark backgrounds push it lighter, light backgrounds darker; a color that already passes is returned as is. */
function fit(hex, backgrounds, min) {
  const passes = c => backgrounds.every(bg => contrast(c, bg) >= min);
  if (passes(hex)) return hex;
  const { L, C, H } = toOklch(hex);
  const lighter = luminance(backgrounds[0]) < 0.18;
  let lo = lighter ? L : 0;
  let hi = lighter ? 1 : L;
  for (let i = 0; i < 40; i += 1) {
    const mid = (lo + hi) / 2;
    if (passes(oklch(mid, C, H)) === lighter) hi = mid;
    else lo = mid;
  }
  return oklch(lighter ? hi : lo, C, H);
}

/** A fill for white text: the source color darkened until white reaches `min`. */
function fillFor(hex, min = 4.6) {
  if (contrast('#ffffff', hex) >= min) return hex;
  const { L, C, H } = toOklch(hex);
  let lo = 0;
  let hi = L;
  for (let i = 0; i < 40; i += 1) {
    const mid = (lo + hi) / 2;
    if (contrast('#ffffff', oklch(mid, C, H)) >= min) lo = mid;
    else hi = mid;
  }
  return oklch(lo, C, H);
}

/* ══ Apple system colors (UIColor / NSColor, dark and light appearance) ══
   pink is the macOS accent pink (hue 355°), not systemPink (16°): the latter sits 12° from systemRed. */
const APPLE = {
  blue: { dark: '#0a84ff', light: '#007aff' },
  indigo: { dark: '#5e5ce6', light: '#5856d6' },
  purple: { dark: '#bf5af2', light: '#af52de' },
  pink: { dark: '#f74f9e', light: '#f74f9e' },
  teal: { dark: '#40c8e0', light: '#30b0c7' },
  cyan: { dark: '#64d2ff', light: '#32ade6' },
  mint: { dark: '#63e6e2', light: '#00c7be' },
  green: { dark: '#30d158', light: '#34c759' },
  yellow: { dark: '#ffd60a', light: '#ffcc00' },
  orange: { dark: '#ff9f0a', light: '#ff9500' },
  red: { dark: '#ff453a', light: '#ff3b30' },
  brown: { dark: '#ac8e68', light: '#a2845e' },
  gray: { dark: '#8e8e93', light: '#8e8e93' },
};
const hueOf = key => toOklch(APPLE[key].dark).H;

/* Chart slots follow the production rules with Apple hues: slot 1 is the data color; slots 2–4 are picked
   greedily for hue distance from the cool/pink pool (no green, orange or red — status hues, and slots 1–2
   are the rx/tx pair); hues within 25° of slot 1 wait until the end. Then yellow, the rest of the pool,
   brown; slot 10 is systemGray for "free memory". */
const POOL = ['pink', 'purple', 'indigo', 'teal', 'cyan', 'mint', 'blue'];

function observeOrder(dataKey) {
  const first = hueOf(dataKey);
  const picked = [dataKey];
  const score = key => {
    const ds = picked.map(p => hueDistance(hueOf(key), hueOf(p))).sort((a, b) => a - b);
    return ds[0] * 1000 + (ds[1] ?? 0);
  };
  const greedy = keys => keys.reduce((best, key) => (score(key) > score(best) ? key : best));
  const far = POOL.filter(key => key !== dataKey && hueDistance(hueOf(key), first) >= 25);
  for (let i = 0; i < 3; i += 1) picked.push(greedy(far.filter(key => !picked.includes(key))));
  const late = ['yellow'];
  let rest = far.filter(key => !picked.includes(key));
  while (rest.length) {
    const key = greedy(rest);
    late.push(key);
    picked.push(key);
    rest = rest.filter(k => k !== key);
  }
  late.push('brown', ...POOL.filter(key => key !== dataKey && !picked.includes(key)));
  return [dataKey, ...picked.slice(1, 4), ...late.slice(0, 5), 'gray'];
}

/* ══ Neutral bases ══
   iOS: dark is the grouped scheme on true black — cards are secondarySystemBackground #1c1c1e, which keeps
   systemBlue #0a84ff above 4.5 as text. Light is systemGroupedBackground #f2f2f7 with white paper.
   macOS: dark is neutral window gray (#1e1e1e) with lifted panels, so link text moves to NSColor.linkColor's
   lighter blue. Light is windowBackgroundColor #ececec with white content. */
const BASES = {
  ios: {
    name: 'iOS',
    dark: {
      ground: '#000000',
      surface: '#0d0d0e',
      sheet: '#121214',
      block: '#1c1c1e',
      band: '#18181a',
      sunk: '#0a0a0b',
      line: '#2a2a2c',
      ink: '#f5f5f7',
      ink2: '#d1d1d6',
      ink3: '#98989d',
      ink4: '#6c6c70',
      btn: '#2c2c2e',
      btnH: '#3a3a3c',
      btnA: '#48484a',
    },
    light: {
      ground: '#f2f2f7',
      surface: '#fafafc',
      sheet: '#ffffff',
      block: '#fbfbfd',
      card: '#ffffff',
      band: '#f5f5f7',
      sunk: '#f2f2f7',
      line: '#e5e5ea',
      ink: '#1d1d1f',
      ink2: '#424245',
      ink3: '#6e6e73',
      ink4: '#86868b',
      btn: '#ffffff',
      btnH: '#f5f5f7',
      btnA: '#ebebf0',
    },
  },
  mac: {
    name: 'macOS',
    dark: {
      ground: '#1e1e1e',
      surface: '#1e1e1e',
      sheet: '#252525',
      block: '#2d2d2d',
      band: '#292929',
      sunk: '#1a1a1a',
      line: '#3a3a3a',
      ink: '#ececec',
      ink2: '#c8c8c8',
      ink3: '#9a9a9a',
      ink4: '#737373',
      btn: '#3a3a3a',
      btnH: '#444444',
      btnA: '#4e4e4e',
    },
    light: {
      ground: '#ececec',
      surface: '#f6f6f6',
      sheet: '#ffffff',
      block: '#fcfcfc',
      card: '#ffffff',
      band: '#f5f5f5',
      sunk: '#f3f3f3',
      line: '#e0e0e0',
      ink: '#1f1f1f',
      ink2: '#444444',
      ink3: '#6e6e6e',
      ink4: '#8e8e8e',
      btn: '#ffffff',
      btnH: '#f5f5f5',
      btnA: '#ebebeb',
    },
  },
};

/** Status colors are systemGreen / systemOrange / systemRed, moved along their hue until readable as text. */
function status(t, light) {
  const papers = [t.sheet, light ? t.card : t.block];
  return {
    ok: fit(APPLE.green[light ? 'light' : 'dark'], papers, light ? 4.2 : 4.5),
    gold: fit(APPLE.orange[light ? 'light' : 'dark'], papers, light ? 3.6 : 4.5),
    err: fit(APPLE.red[light ? 'light' : 'dark'], papers, 4.5),
  };
}

/* ══ Accents ══ */
const ACCENTS = [
  { key: 'blue', name: '蓝', source: 'blue' },
  { key: 'purple', name: '紫', source: 'purple' },
  { key: 'pink', name: '粉', source: 'pink' },
  { key: 'teal', name: '青', source: 'teal' },
  { key: 'graphite', name: '石墨', source: null, data: 'blue' },
];

/* apple.com's button blue: #0071e3 at rest, #0077ed on hover, #006edb pressed; white text 4.70.
   apple.com's hover fails 4.5 by a hair (4.32), so the hover step keeps the hue and stops at 4.5. */
const BLUE_FILL = { primary: '#0071e3', primaryHover: fillFor('#0077ed', 4.5), primaryActive: '#006edb' };

function accentTokens(accent, t, light) {
  const theme = light ? 'light' : 'dark';
  const papers = light ? [t.sheet, t.card, t.surface, t.ground] : [t.sheet, t.block, t.surface];
  const cardish = light ? [t.card, t.block, t.sheet] : [t.block, t.sheet];
  const dataKey = accent.data ?? accent.source;
  const data = fit(APPLE[dataKey][theme], cardish, 4.5);
  const order = observeOrder(dataKey);
  const observeColors = order.map((key, index) =>
    index === 0 ? data : fit(APPLE[key][theme], cardish, light ? 3.2 : 3.6),
  );
  const common = {
    data,
    dataSecondary: fit(APPLE.gray[theme], cardish, 3.2),
    dataWashAlpha: light ? 0.09 : 0.14,
    observe: observeColors,
    order,
  };
  if (accent.source === null) {
    return light
      ? {
          ...common,
          action: t.ink,
          actionHover: t.ink2,
          actionActive: '#000000',
          washAlpha: 0.07,
          actionOn: '#ffffff',
          primary: t.ink,
          primaryHover: shift(t.ink, 0.08),
          primaryActive: '#000000',
          primaryOn: '#ffffff',
          codeN: fit(APPLE.purple.light, cardish, 4.5),
        }
      : {
          ...common,
          action: t.ink,
          actionHover: '#ffffff',
          actionActive: t.ink2,
          washAlpha: 0.12,
          actionOn: '#1d1d1f',
          primary: t.ink,
          primaryHover: '#ffffff',
          primaryActive: t.ink2,
          primaryOn: '#1d1d1f',
          codeN: fit(APPLE.purple.dark, cardish, 4.5),
        };
  }
  const source = APPLE[accent.source][theme];
  const action = accent.key === 'blue' && light ? '#0066cc' : fit(source, papers, 4.5);
  const fill =
    accent.key === 'blue'
      ? BLUE_FILL
      : (() => {
          const primary = fillFor(APPLE[accent.source].light);
          return {
            primary,
            primaryHover: fillFor(shift(primary, 0.03), 4.5),
            primaryActive: shift(primary, -0.04),
          };
        })();
  return {
    ...common,
    action,
    actionHover: shift(action, light ? 0.06 : 0.06),
    actionActive: shift(action, light ? -0.06 : -0.06),
    washAlpha: light ? 0.1 : 0.18,
    actionOn: '#ffffff',
    ...fill,
    primaryOn: '#ffffff',
    codeN: fit(APPLE[accent.key === 'pink' ? 'teal' : 'pink'][theme], cardish, 4.5),
  };
}

/* ══ CSS ══ */

function baseBlock(selector, t, light) {
  const s = status(t, light);
  const lines = [
    `--ground: ${t.ground};`,
    `--sheet: ${t.sheet};`,
    `--surface: ${t.surface};`,
    `--block: ${t.block};`,
    light ? `--card: ${t.card};` : null,
    `--band: ${t.band};`,
    `--sunk: ${t.sunk};`,
    `--line: ${t.line};`,
    `--grid-line: ${t.line};`,
    `--line-soft: ${light ? 'rgba(0, 0, 0, 0.04)' : 'rgba(255, 255, 255, 0.05)'};`,
    '--grid-image: none;',
    `--ink: ${t.ink};`,
    `--ink-2: ${t.ink2};`,
    `--ink-3: ${t.ink3};`,
    `--ink-4: ${t.ink4};`,
    `--ok: ${s.ok};`,
    `--gold: ${s.gold};`,
    `--err: ${s.err};`,
    `--add: ${rgba(s.ok, light ? 0.1 : 0.16)};`,
    `--del: ${rgba(s.err, light ? 0.09 : 0.14)};`,
    `--add-ink: ${s.ok};`,
    `--del-ink: ${s.err};`,
    `--btn: ${t.btn};`,
    `--btn-h: ${t.btnH};`,
    `--btn-a: ${t.btnA};`,
    `--hover: color-mix(in srgb, var(--ink) ${light ? 5 : 6}%, transparent);`,
    `--hover-2: color-mix(in srgb, var(--ink) ${light ? 8 : 10}%, transparent);`,
    ...['250', '205', '300', '230'].map(
      (h, i) => `--hue-${i + 1}: ${oklch(light ? 0.5 : 0.7, light ? 0.035 : 0.03, +h)};`,
    ),
    light ? '--scrim: rgba(0, 0, 0, 0.32);' : '--scrim: rgba(0, 0, 0, 0.5);',
    light
      ? '--shadow: 0 2px 8px rgba(0, 0, 0, 0.06), 0 12px 32px rgba(0, 0, 0, 0.12);'
      : '--shadow: 0 2px 8px rgba(0, 0, 0, 0.5), 0 12px 32px rgba(0, 0, 0, 0.45);',
    light
      ? '--sheet-shadow: 0 1px 2px rgba(0, 0, 0, 0.04), 0 2px 10px rgba(0, 0, 0, 0.04);'
      : '--sheet-shadow: 0 1px 3px rgba(0, 0, 0, 0.3), 0 3px 10px rgba(0, 0, 0, 0.25);',
  ].filter(Boolean);
  return `${selector} {\n${lines.map(l => `  ${l}`).join('\n')}\n}\n`;
}

function accentBlock(selector, a) {
  const lines = [
    `--action: ${a.action};`,
    `--action-hover: ${a.actionHover};`,
    `--action-active: ${a.actionActive};`,
    `--action-wash: ${rgba(a.action, a.washAlpha)};`,
    `--action-on: ${a.actionOn};`,
    `--data: ${a.data};`,
    `--data-secondary: ${a.dataSecondary};`,
    `--data-wash: ${rgba(a.data, a.dataWashAlpha)};`,
    `--primary: ${a.primary};`,
    `--primary-hover: ${a.primaryHover};`,
    `--primary-active: ${a.primaryActive};`,
    `--primary-on: ${a.primaryOn};`,
    '--primary-line: transparent;',
    ...a.observe.map((c, i) => `--observe-${i + 1}: ${c};`),
    `--code-n: ${a.codeN};`,
  ];
  return `${selector} {\n${lines.map(l => `  ${l}`).join('\n')}\n}\n`;
}

const DARK = ":not([data-theme='light'])";
const LIGHT = "[data-theme='light']";
let css = `/* 由 palette-mockup-gen.mjs 生成：苹果系配色的完整令牌，只在 palette-mockup 里加载。
   两个轴与 macOS 相同：<html> 的 data-base 选中性底（iOS / macOS 系统灰），data-accent 选强调色。
   data-palette 保留；暗色块带 :not([data-theme='light'])，特异度不低于生产的主题块与调色盘块，且在它们之后加载。 */\n`;
const report = [];
for (const [baseKey, base] of Object.entries(BASES)) {
  css += `\n/* ── ${base.name} ── */\n`;
  css += baseBlock(`:root[data-base='${baseKey}']${DARK}`, base.dark, false);
  css += baseBlock(`:root[data-base='${baseKey}']${LIGHT}`, base.light, true);
  for (const accent of ACCENTS) {
    const dark = accentTokens(accent, base.dark, false);
    const light = accentTokens(accent, base.light, true);
    css += `\n/* ${base.name} · ${accent.name} */\n`;
    css += accentBlock(`:root[data-base='${baseKey}'][data-accent='${accent.key}']${DARK}`, dark);
    css += accentBlock(`:root[data-base='${baseKey}'][data-accent='${accent.key}']${LIGHT}`, light);
    report.push({ baseKey, accent, dark, light, base });
  }
}

css += `
/* ── 结构差异（不是令牌）──
   苹果把「着色」与「填充」分开：链接、页签文字取 --action，选中与按下的实底取 --primary，白字对比不低于 4.5。
   生产里下列 8 处是 --action 实底配 --action-on，这里改指 --primary 一组；落地时同样改这 8 处。 */
:root[data-accent]
  :is(
    .fg-menu button:hover,
    .pv-steps li.current i,
    .wz-stages li.current > span,
    .wz-protocols > label.on i,
    .observe-range-menu .observe-range-apply,
    .external-outbound-tabs button.on,
    .ov-chips button[aria-pressed='true'],
    .cg-steps li.current i
  ) {
  border-color: var(--primary);
  background: var(--primary);
  color: var(--primary-on);
}

/* 石墨的操作色与正文同色，链接靠下划线辨认（同生产玄墨）。 */
:root[data-accent='graphite'] a {
  text-decoration: underline;
  text-underline-offset: 2px;
}
`;
writeFileSync(process.argv[2], css);

/* ══ Report ══ */
const f = n => n.toFixed(2);
for (const { baseKey, accent, dark, light, base } of report) {
  for (const [theme, a, t] of [
    ['dark', dark, base.dark],
    ['light', light, base.light],
  ]) {
    const card = theme === 'light' ? t.card : t.block;
    const s = status(t, theme === 'light');
    console.log(
      `${baseKey} ${accent.key} ${theme}`.padEnd(20),
      `action/sheet ${f(contrast(a.action, t.sheet))} /card ${f(contrast(a.action, card))}`,
      `primaryOn ${f(contrast(a.primaryOn, a.primary))} hover ${f(contrast(a.primaryOn, a.primaryHover))}`,
      `data/card ${f(contrast(a.data, card))}`,
      `ink3/sheet ${f(contrast(t.ink3, t.sheet))} /card ${f(contrast(t.ink3, card))}`,
      `ok ${f(contrast(s.ok, card))} gold ${f(contrast(s.gold, card))} err ${f(contrast(s.err, card))}`,
    );
    console.log('   action', a.action, 'primary', a.primary, 'data', a.data, 'status', s.ok, s.gold, s.err);
    console.log('   observe', a.observe.map((c, i) => `${a.order[i]}:${c}(${f(contrast(c, card))})`).join(' '));
  }
  console.log(
    '   L*',
    ['dark', 'light']
      .map(theme =>
        ['ground', 'surface', 'sunk', 'sheet', 'band', 'block', 'line']
          .map(k => `${k} ${lstar(base[theme][k]).toFixed(1)}`)
          .join(' · '),
      )
      .join('  ||  '),
  );
}
