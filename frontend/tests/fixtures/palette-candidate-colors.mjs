// Review-only recolorings of the current production dailan theme. Production is never modified.
export const STUDIES = {
  jinzi: {
    name: '堇紫',
    recommended: 'v1',
    variants: {
      v1: { name: '灰藤', note: '与黛蓝相同灰度的中性藤紫。', hue: 300, chroma: 0.04, dataHue: 294, dataChroma: 0.035 },
      v2: { name: '鸢尾', note: '稍偏蓝，紫色识别更明确。', hue: 283, chroma: 0.055, dataHue: 280, dataChroma: 0.048 },
      v3: {
        name: '丁香',
        note: '略偏暖的柔紫，保留适量颜色。',
        hue: 312,
        chroma: 0.052,
        dataHue: 308,
        dataChroma: 0.045,
      },
      v4: { name: '烟紫', note: '更灰的紫色，强调更轻。', hue: 295, chroma: 0.025, dataHue: 291, dataChroma: 0.025 },
      v5: {
        name: '葡紫',
        note: '偏红的灰葡萄紫，冷暖区别更明显。',
        hue: 324,
        chroma: 0.05,
        dataHue: 318,
        dataChroma: 0.043,
      },
    },
  },
  dailan: { name: '黛蓝', recommended: 'current', variants: {} },
  songlv: {
    name: '松绿',
    recommended: 'v1',
    variants: {
      v1: { name: '雾松', note: '与黛蓝相同灰度的松针绿。', hue: 132, chroma: 0.04, dataHue: 129, dataChroma: 0.035 },
      v2: {
        name: '鼠尾草',
        note: '更偏灰、稍带暖意的草木色。',
        hue: 125,
        chroma: 0.028,
        dataHue: 125,
        dataChroma: 0.027,
      },
      v3: {
        name: '橄榄',
        note: '偏暖的灰橄榄，弱化鲜绿感。',
        hue: 112,
        chroma: 0.045,
        dataHue: 112,
        dataChroma: 0.039,
      },
      v4: { name: '苔青', note: '偏青的冷绿，减少黄调。', hue: 193, chroma: 0.035, dataHue: 198, dataChroma: 0.032 },
      v5: {
        name: '青玉',
        note: '更冷的青绿灰，与暖绿方向拉开。',
        hue: 205,
        chroma: 0.04,
        dataHue: 205,
        dataChroma: 0.035,
      },
    },
  },
  oufen: {
    name: '藕粉',
    recommended: 'v1',
    variants: {
      v1: {
        name: '玫瑰灰',
        note: '与黛蓝相同灰度的柔和玫瑰色。',
        hue: 350,
        chroma: 0.04,
        dataHue: 347,
        dataChroma: 0.034,
      },
      v2: { name: '藕荷', note: '偏紫的藕灰，红调更少。', hue: 326, chroma: 0.04, dataHue: 323, dataChroma: 0.035 },
      v3: { name: '烟粉', note: '更灰、更轻的粉色强调。', hue: 339, chroma: 0.028, dataHue: 336, dataChroma: 0.026 },
      v4: {
        name: '豆沙',
        note: '略偏暖的豆沙粉，保持相同明暗关系。',
        hue: 358,
        chroma: 0.04,
        dataHue: 352,
        dataChroma: 0.034,
      },
      v5: { name: '紫蔷', note: '更偏紫的蔷薇色，颜色稍浓。', hue: 315, chroma: 0.05, dataHue: 310, dataChroma: 0.043 },
    },
  },
  xuanmo: {
    name: '玄墨',
    recommended: 'v1',
    variants: {
      v1: {
        name: '石墨',
        note: '中性石墨灰，操作色明度与黛蓝一致。',
        hue: 260,
        chroma: 0.004,
        dataHue: 240,
        dataChroma: 0.012,
        surfaceScale: 0,
      },
      v2: {
        name: '暖墨',
        note: '略带暖调的炭灰，配冷灰数据。',
        hue: 75,
        chroma: 0.011,
        dataHue: 240,
        dataChroma: 0.015,
        surfaceScale: 0.55,
      },
      v3: {
        name: '青墨',
        note: '偏钢青的冷灰，色感稍明显。',
        hue: 215,
        chroma: 0.022,
        dataHue: 215,
        dataChroma: 0.022,
      },
      v4: {
        name: '铅灰',
        note: '略偏紫蓝的铅灰，保留冷静质感。',
        hue: 270,
        chroma: 0.018,
        dataHue: 266,
        dataChroma: 0.018,
      },
      v5: {
        name: '银墨',
        note: '近中性的银灰，数据轻带青调。',
        hue: 100,
        chroma: 0.005,
        dataHue: 235,
        dataChroma: 0.018,
        surfaceScale: 0.25,
      },
    },
  },
};

export const REFERENCE_TOKENS = [
  '--ground',
  '--sunk',
  '--surface',
  '--sheet',
  '--block',
  '--card',
  '--band',
  '--line',
  '--grid-line',
  '--line-soft',
  '--ink',
  '--ink-2',
  '--ink-3',
  '--ink-4',
  '--ok',
  '--gold',
  '--err',
  '--gold-line',
  '--gold-wash',
  '--warn',
  '--focus',
  '--action',
  '--action-hover',
  '--action-active',
  '--action-on',
  '--action-wash',
  '--data',
  '--data-secondary',
  '--data-wash',
  '--primary',
  '--primary-hover',
  '--primary-active',
  '--primary-on',
  '--primary-line',
  '--hover',
  '--hover-2',
  '--btn',
  '--btn-h',
  '--btn-a',
  '--glass',
  '--glass-strong',
  '--float',
  '--float-sheet',
  '--float-filter',
  '--float-edge',
  '--scrim',
  '--shadow',
  '--sheet-shadow',
  '--grid-image',
  '--code-bg',
  '--code-ink',
  '--code-ln',
  '--code-k',
  '--code-s',
  '--code-n',
  '--code-c',
  '--add',
  '--del',
  '--add-ink',
  '--del-ink',
  ...Array.from({ length: 4 }, (_, i) => `--hue-${i + 1}`),
  ...Array.from({ length: 10 }, (_, i) => `--observe-${i + 1}`),
];

const linear = value => (value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4);
const gamma = value => (value <= 0.0031308 ? value * 12.92 : 1.055 * value ** (1 / 2.4) - 0.055);
const channels = hex =>
  hex
    .slice(1)
    .match(/../g)
    .map(value => parseInt(value, 16) / 255);
export const luminance = hex =>
  channels(hex)
    .map(linear)
    .reduce((sum, value, index) => sum + value * [0.2126, 0.7152, 0.0722][index], 0);
export const contrast = (a, b) => {
  const [high, low] = [luminance(a), luminance(b)].sort((a, b) => b - a);
  return (high + 0.05) / (low + 0.05);
};
export function toLch(hex) {
  const [r, g, b] = channels(hex).map(linear);
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  const a = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
  const ab = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
  return {
    l: 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    c: Math.hypot(a, ab),
    h: ((Math.atan2(ab, a) * 180) / Math.PI + 360) % 360,
  };
}
function lchChannels(lightness, chroma, hue) {
  const angle = (hue * Math.PI) / 180;
  const a = chroma * Math.cos(angle),
    b = chroma * Math.sin(angle);
  const l = (lightness + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (lightness - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (lightness - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
}
export function color(lightness, chroma, hue) {
  let rgb = lchChannels(lightness, chroma, hue);
  // Retain hue and lightness at the sRGB boundary; only chroma is reduced.
  for (let attempt = 0; attempt < 80 && rgb.some(value => value < 0 || value > 1); attempt++) {
    chroma *= 0.92;
    rgb = lchChannels(lightness, chroma, hue);
  }
  return (
    '#' +
    rgb
      .map(value =>
        Math.round(Math.max(0, Math.min(1, gamma(value))) * 255)
          .toString(16)
          .padStart(2, '0'),
      )
      .join('')
  );
}
export function mix(a, b, share) {
  const bChannels = channels(b);
  return (
    '#' +
    channels(a)
      .map((value, index) =>
        Math.round((value * (1 - share) + bChannels[index] * share) * 255)
          .toString(16)
          .padStart(2, '0'),
      )
      .join('')
  );
}
function atContrast(background, ratio, chroma, hue, dark) {
  const base = toLch(background).l;
  let low = dark ? base : 0,
    high = dark ? 1 : base;
  for (let attempt = 0; attempt < 30; attempt++) {
    const middle = (low + high) / 2;
    const actual = contrast(color(middle, chroma, hue), background);
    if (dark ? actual < ratio : actual >= ratio) low = middle;
    else high = middle;
  }
  return color(dark ? high : low, chroma, hue);
}
const wash = (hex, alpha) =>
  `rgba(${channels(hex)
    .map(value => Math.round(value * 255))
    .join(', ')}, ${alpha})`;

const hueGap = (a, b) => Math.min(Math.abs(a - b) % 360, 360 - (Math.abs(a - b) % 360));
const matchTone = (source, chroma, hue) => atContrast('#000000', contrast(source, '#000000'), chroma, hue, true);
const alphaOf = value => {
  const found = value.match(/,\s*([\d.]+)\)$/);
  if (!found) throw new Error(`Expected a direct RGBA wash: ${value}`);
  return Number(found[1]);
};

export function candidateTokens(palette, variant, theme, blue) {
  if (variant === 'current') return {};
  const spec = STUDIES[palette]?.variants[variant];
  if (!spec || palette === 'dailan') throw new Error(`Unknown color study: ${palette}/${variant}`);
  for (const key of REFERENCE_TOKENS) if (!blue?.[key]) throw new Error(`Missing live dailan reference: ${key}`);
  const dark = theme === 'dark';
  const t = { ...blue };
  const actionChroma = toLch(blue['--action']).c;
  const scale = spec.chroma / actionChroma;
  const shifted = (source, hue = spec.hue, chroma = Math.min(toLch(source).c * scale, 0.05)) =>
    matchTone(source, chroma, hue);
  // Keep the blue ladder's luminance and tiny tint; only its hue changes.
  for (const key of [
    '--ground',
    '--sunk',
    '--surface',
    '--sheet',
    '--block',
    '--card',
    '--band',
    '--line',
    '--grid-line',
  ]) {
    t[key] = matchTone(blue[key], toLch(blue[key]).c * (spec.surfaceScale ?? 1), spec.hue);
  }
  for (const key of ['--action', '--action-hover', '--action-active'])
    t[key] = shifted(blue[key], spec.hue, spec.chroma);
  t['--action-on'] = dark ? shifted(blue['--action-on']) : blue['--action-on'];
  t['--data'] = shifted(blue['--data'], spec.dataHue, spec.dataChroma);
  t['--data-secondary'] = shifted(blue['--data-secondary'], spec.dataHue, Math.min(spec.dataChroma * 0.6, 0.025));
  t['--action-wash'] = wash(t['--action'], alphaOf(blue['--action-wash']));
  t['--data-wash'] = wash(t['--data'], alphaOf(blue['--data-wash']));
  for (const key of ['--primary', '--primary-hover', '--primary-active', '--primary-on']) t[key] = shifted(blue[key]);
  t['--primary-line'] = dark ? shifted(blue['--primary-line']) : blue['--primary-line'];
  if (!dark) {
    t['--primary'] = t['--action'];
    t['--primary-hover'] = t['--action-hover'];
    t['--primary-active'] = t['--action-active'];
    t['--primary-on'] = blue['--primary-on'];
  }
  // Rebind derived colors to the candidate while preserving the production formulas and opacity.
  const roles = ['--grid-line', '--surface', '--sheet', '--block', '--sunk', '--action', '--data', '--ink', '--gold'];
  for (const key of REFERENCE_TOKENS) {
    if (!/(?:color-mix|gradient)\(/.test(blue[key])) continue;
    let value = blue[key];
    for (const role of roles) value = value.replaceAll(blue[role], `var(${role})`);
    t[key] = value;
  }
  t['--focus'] = 'var(--action)';
  t['--btn'] = dark ? 'var(--surface)' : 'var(--sheet)';
  t['--code-bg'] = 'var(--sunk)';
  t['--code-k'] = dark ? 'var(--action-hover)' : 'var(--action)';
  const numeric = toLch(blue['--code-n']);
  t['--code-n'] =
    hueGap(spec.hue, numeric.h) < 45 ? matchTone(blue['--code-n'], Math.min(numeric.c, 0.07), 250) : blue['--code-n'];
  for (let index = 1; index <= 4; index++) {
    const key = `--hue-${index}`;
    const old = toLch(blue[key]);
    t[key] = matchTone(
      blue[key],
      Math.min(old.c, spec.chroma),
      (old.h + spec.hue - toLch(blue['--action']).h + 360) % 360,
    );
  }
  t['--observe-1'] = t['--data'];
  const used = [spec.dataHue];
  for (let index = 2; index <= 10; index++) {
    const key = `--observe-${index}`;
    const old = toLch(blue[key]);
    let hue = old.h;
    if (index <= 4 && used.some(h => hueGap(hue, h) < 28)) {
      hue = [200, 235, 270, 305, 340].sort(
        (a, b) => Math.min(...used.map(h => hueGap(b, h))) - Math.min(...used.map(h => hueGap(a, h))),
      )[0];
    }
    t[key] = index === 10 ? shifted(blue[key], spec.dataHue, 0.01) : matchTone(blue[key], old.c, hue);
    used.push(hue);
  }
  return t;
}
