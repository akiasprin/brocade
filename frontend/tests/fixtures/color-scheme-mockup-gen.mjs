// Generates the token blocks of the color-scheme mockup. Never imported by the app.
// Usage (from frontend/): node tests/fixtures/color-scheme-mockup-gen.mjs tests/fixtures/color-scheme-mockup-schemes.css
// Prints a contrast report. Neutral surfaces stay production's 黛蓝 ladder; only role colors change:
//   A / C / D share Apple system blue and the system status colors (they differ in CSS layers, see
//   color-scheme-mockup.css); B is a softened blue between production's old 宝蓝 and today's 灰雾蓝.
import { writeFileSync } from 'node:fs';

/* ══ Color math (same as palette-mockup-gen.mjs) ══ */
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

/** A fill for white text: the source color darkened along its hue until white reaches `min`. */
function whiteFill(hex, min = 4.6) {
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

/* ══ Sources ══ */

// Production 黛蓝 surfaces the role colors must read on (styles.css, data-palette='dailan').
const SURFACE = {
  dark: { sheet: '#111317', card: '#191b20', band: '#101316', surface: '#0f1114' },
  light: { sheet: '#fefefe', card: '#ffffff', band: '#eef0f5', surface: '#e8ebf1', block: '#fafafb' },
};

// Apple system colors (HIG), default appearance.
const APPLE = {
  dark: {
    blue: '#0a84ff',
    green: '#30d158',
    indigo: '#5e5ce6',
    orange: '#ff9f0a',
    pink: '#ff375f',
    purple: '#bf5af2',
    red: '#ff453a',
    teal: '#40c8e0',
    cyan: '#64d2ff',
    mint: '#66d4cf',
    yellow: '#ffd60a',
    brown: '#ac8e68',
    gray: '#8e8e93',
  },
  light: {
    blue: '#007aff',
    green: '#34c759',
    indigo: '#5856d6',
    orange: '#ff9500',
    pink: '#ff2d55',
    purple: '#af52de',
    red: '#ff3b30',
    teal: '#30b0c7',
    cyan: '#32ade6',
    mint: '#00c7be',
    yellow: '#ffcc00',
    brown: '#a2845e',
    gray: '#8e8e93',
  },
};

// apple.com's own blues: #0071e3 buttons (white text 4.70), #0066cc links on white, #2997ff links on black.
const WEB = {
  button: '#0071e3',
  buttonHover: '#0077ed',
  buttonActive: '#006edb',
  link: '#0066cc',
  linkDark: '#2997ff',
};

// Magenta-pink for the second series. systemPink sits 15° from systemRed, which would read as the error
// color next to it; this keeps Apple pink's lightness and chroma and moves the hue to 350°.
const magenta = theme => {
  const { L, C } = toOklch(APPLE[theme].pink);
  return oklch(L + (theme === 'dark' ? 0.04 : -0.02), C * 0.92, 350);
};

/* ══ Schemes ══ */

const report = [];
const check = (scheme, theme, label, value, min) =>
  report.push({ scheme, theme, label, value: Number(value.toFixed(2)), ok: value >= min, min });

function appleScheme(theme) {
  const s = SURFACE[theme];
  const a = APPLE[theme];
  const surfaces = theme === 'dark' ? [s.sheet, s.card, s.band] : [s.sheet, s.card, s.band, s.block];
  const text = hex => fit(hex, surfaces, 4.5);
  const line = hex => fit(hex, [s.card], theme === 'dark' ? 3.6 : 3.2);
  const action = theme === 'dark' ? text(WEB.linkDark) : text(WEB.link);
  const t = {
    '--action': action,
    '--action-hover': theme === 'dark' ? shift(action, 0.05) : shift(action, 0.06),
    '--action-active': theme === 'dark' ? shift(action, -0.04) : shift(action, -0.05),
    '--action-wash': rgba(action, theme === 'dark' ? 0.16 : 0.09),
    // Text on a solid --action fill (menu hover, step markers, chips): dark ink on the light dark-theme blue.
    '--action-on': theme === 'dark' ? '#00122b' : '#ffffff',
    '--primary': WEB.button,
    // apple.com hovers lighter, to #0077ed, but white text drops to 4.32 there; hover and press darken instead.
    '--primary-hover': shift(WEB.button, -0.03),
    '--primary-active': shift(WEB.button, -0.06),
    '--primary-on': '#ffffff',
    '--primary-line': 'transparent',
    '--data': line(a.blue),
    '--data-secondary': line(a.teal),
    '--data-wash': rgba(line(a.blue), theme === 'dark' ? 0.12 : 0.08),
    '--ok': text(a.green),
    '--gold': text(a.orange),
    '--err': text(a.red),
  };
  // Slots 1–4 avoid the status hues (green / orange / red); slots 1–2 are the rx / tx pair.
  const observe = [a.blue, magenta(theme), a.teal, a.purple, a.mint, a.orange, a.indigo, a.yellow, a.brown, a.gray].map(
    line,
  );
  observe.forEach((hex, index) => (t[`--observe-${index + 1}`] = hex));
  t['--observe-1'] = t['--data'];
  // Apple hovers are neutral: production's light theme tints hover with the action color.
  if (theme === 'light') {
    t['--hover'] = 'color-mix(in srgb, var(--ink) 5%, transparent)';
    t['--hover-2'] = 'color-mix(in srgb, var(--ink) 9%, transparent)';
    t['--btn-h'] = 'color-mix(in srgb, var(--ink) 5%, var(--sheet))';
    t['--btn-a'] = 'color-mix(in srgb, var(--ink) 10%, var(--sheet))';
  }
  t['--code-k'] = theme === 'dark' ? shift(action, 0.05) : action;
  return t;
}

function softScheme(theme) {
  const s = SURFACE[theme];
  if (theme === 'dark') {
    const action = fit(oklch(0.68, 0.1, 258), [s.sheet, s.card, s.band], 4.5);
    const data = oklch(0.71, 0.08, 250);
    const t = {
      '--action': action,
      '--action-hover': shift(action, 0.05),
      '--action-active': shift(action, -0.03),
      '--action-wash': rgba(action, 0.15),
      '--action-on': '#07101f',
      // Production's dark-theme wash recipe, with the chroma between 宝蓝 and 灰雾蓝.
      '--primary': oklch(0.285, 0.045, 258),
      '--primary-hover': oklch(0.32, 0.055, 258),
      '--primary-active': oklch(0.35, 0.062, 258),
      '--primary-on': oklch(0.83, 0.055, 258),
      '--primary-line': oklch(0.4, 0.075, 258),
      '--data': data,
      '--data-secondary': oklch(0.66, 0.05, 250),
      '--data-wash': rgba(data, 0.11),
      '--observe-1': data,
      '--observe-2': oklch(0.76, 0.1, 350),
      '--observe-3': oklch(0.76, 0.085, 200),
      '--observe-4': oklch(0.73, 0.1, 295),
      '--code-k': shift(action, 0.05),
    };
    return t;
  }
  const action = fit(oklch(0.5, 0.12, 260), [s.sheet, s.card, s.band, s.block], 4.5);
  const data = oklch(0.53, 0.11, 250);
  return {
    '--action': action,
    '--action-hover': shift(action, 0.055),
    '--action-active': shift(action, -0.05),
    '--action-wash': rgba(action, 0.09),
    '--action-on': '#ffffff',
    '--primary': action,
    '--primary-hover': shift(action, 0.04),
    '--primary-active': shift(action, -0.05),
    '--primary-on': '#ffffff',
    '--primary-line': 'transparent',
    '--data': data,
    '--data-secondary': oklch(0.55, 0.06, 250),
    '--data-wash': rgba(data, 0.08),
    '--observe-1': data,
    '--observe-2': oklch(0.54, 0.12, 350),
    '--observe-3': oklch(0.54, 0.09, 200),
    '--observe-4': oklch(0.52, 0.11, 295),
    '--code-k': action,
  };
}

const SCHEMES = {
  apple: { selector: "[data-scheme='a'], [data-scheme='c'], [data-scheme='d']", build: appleScheme },
  soft: { selector: "[data-scheme='b']", build: softScheme },
};

/* ══ Report + output ══ */

let css = `/* 生成文件：node tests/fixtures/color-scheme-mockup-gen.mjs tests/fixtures/color-scheme-mockup-schemes.css
   中性底沿用生产黛蓝；这里只覆盖角色色。A / C / D 共用苹果系统蓝与系统状态色，差别在
   color-scheme-mockup.css 的结构层；B 是宝蓝与灰雾蓝之间的柔和蓝。 */\n`;

for (const [name, scheme] of Object.entries(SCHEMES)) {
  for (const theme of ['dark', 'light']) {
    const tokens = scheme.build(theme);
    const s = SURFACE[theme];
    const sel =
      theme === 'dark'
        ? `:root:is(${scheme.selector}):not([data-theme='light'])`
        : `:root[data-theme='light']:is(${scheme.selector})`;
    css += `${sel} {\n${Object.entries(tokens)
      .map(([key, value]) => `  ${key}: ${value};`)
      .join('\n')}\n}\n`;
    check(name, theme, 'action / sheet', contrast(tokens['--action'], s.sheet), 4.5);
    check(name, theme, 'action / band', contrast(tokens['--action'], s.band), 4.5);
    check(name, theme, 'primary-on / primary', contrast(tokens['--primary-on'], tokens['--primary']), 4.5);
    check(name, theme, 'primary-on / hover', contrast(tokens['--primary-on'], tokens['--primary-hover']), 4.5);
    check(name, theme, 'data / card', contrast(tokens['--data'], s.card), theme === 'dark' ? 3.6 : 3.2);
    for (let i = 1; i <= 10; i += 1) {
      const value = tokens[`--observe-${i}`];
      if (value) check(name, theme, `observe-${i} ${value}`, contrast(value, s.card), theme === 'dark' ? 3.6 : 3.2);
    }
    for (const role of ['--ok', '--gold', '--err'])
      if (tokens[role]) check(name, theme, `${role} ${tokens[role]} / card`, contrast(tokens[role], s.card), 4.5);
  }
}

const out = process.argv[2];
if (out) writeFileSync(out, css);
else process.stdout.write(css);
for (const row of report)
  console.error(
    `${row.ok ? ' ' : '!'} ${row.scheme.padEnd(5)} ${row.theme.padEnd(5)} ${row.label.padEnd(30)} ${row.value}`,
  );
