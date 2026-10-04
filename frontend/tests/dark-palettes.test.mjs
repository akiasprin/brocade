import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const paletteSource = readFileSync(new URL('../src/forge/palette.ts', import.meta.url), 'utf8');
const PALETTES = ['jinzi', 'dailan', 'songlv', 'oufen', 'xuanmo'];

const tokens = selector => {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const block = styles.match(new RegExp(`^${escaped} \\{([^}]*)\\}`, 'm'))?.[1];
  assert.ok(block, `${selector} block exists`);
  return new Map([...block.matchAll(/(--[\w-]+):\s*([^;]+);/g)].map(([, name, value]) => [name, value.trim()]));
};
const dark = Object.fromEntries(PALETTES.map(key => [key, tokens(`:root[data-palette='${key}']`)]));
const light = Object.fromEntries(
  PALETTES.map(key => [key, tokens(`:root[data-theme='light'][data-palette='${key}']`)]),
);

const channels = hex => [1, 3, 5].map(i => parseInt(hex.slice(i, i + 2), 16) / 255);
const luminance = hex => {
  const [r, g, b] = channels(hex).map(c => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
};
const contrast = (a, b) => {
  const [high, low] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (high + 0.05) / (low + 0.05);
};
const oklch = hex => {
  const [r, g, b] = channels(hex).map(c => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  const a = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
  const ab = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
  return {
    lightness: 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    chroma: Math.hypot(a, ab),
    hue: (Math.atan2(ab, a) * 180) / Math.PI,
  };
};

const hueGap = (a, b) => {
  const difference = Math.abs(a - b) % 360;
  return Math.min(difference, 360 - difference);
};

test('dark data colors are in-family tints of the theme color', () => {
  for (const key of PALETTES) {
    const t = dark[key];
    const data = oklch(t.get('--data'));
    // 曲线按 72% 不透明度绘制；明度 0.72 才能在暗色底上读清，0.565 的旧取值发暗发浑。
    assert.ok(Math.abs(data.lightness - 0.72) < 0.005, `${key} data lightness`);
    for (const background of ['--sheet', '--block']) {
      assert.ok(contrast(t.get('--data'), t.get(background)) >= 4.5, `${key} data on ${background}`);
    }
    const action = oklch(t.get('--action'));
    if (action.chroma > 0.05) {
      // 同族：色相比操作色低 12°，彩度为操作色的 75%（上限 0.13）。
      assert.ok(hueGap(data.hue, action.hue - 12) < 3, `${key} data hue sits 12° below its action`);
      assert.ok(Math.abs(data.chroma - Math.min(action.chroma * 0.75, 0.13)) < 0.008, `${key} data chroma`);
    } else {
      // 灰白操作色没有可用的色相，数据色锚在低彩度的钢青。
      assert.ok(data.chroma < 0.05, `${key} achromatic palette keeps a muted data color`);
    }
    const rgb = channels(t.get('--data')).map(c => Math.round(c * 255));
    assert.equal(t.get('--data-wash'), `rgba(${rgb.join(', ')}, 0.13)`, `${key} data wash`);
  }
});

test('dark palettes carry their own chart palette led by the data color', () => {
  const status = ['--ok', '--gold', '--err'].map(
    name => oklch(styles.match(new RegExp(`${name}: (#[0-9a-f]{6});`))[1]).hue,
  );
  for (const key of PALETTES) {
    const chart = tokens(`:root[data-palette='${key}']:not([data-theme='light'])`);
    const slots = Array.from({ length: 10 }, (_, index) => chart.get(`--observe-${index + 1}`));
    assert.ok(slots.every(Boolean), `${key} declares ten chart slots`);
    assert.equal(new Set(slots).size, 10, `${key} chart slots are distinct`);
    assert.equal(slots[0], dark[key].get('--data'), `${key} leads charts with its data color`);
    // 前四档是最常同屏出现的序列（收发、用户态 / 内核态 / SoftIRQ / IOwait），不能读成状态色。
    for (const hex of slots.slice(0, 4)) {
      const color = oklch(hex);
      for (const hue of status) assert.ok(hueGap(color.hue, hue) >= 25, `${key} ${hex} stays clear of status hues`);
    }
    assert.ok(oklch(slots[9]).chroma < 0.03, `${key} slot 10 stays neutral for free memory`);
  }
});

test('light chart palettes follow their dark counterparts and lead with the KPI data color', () => {
  for (const key of PALETTES) {
    const t = light[key];
    const darkChart = tokens(`:root[data-palette='${key}']:not([data-theme='light'])`);
    const slots = Array.from({ length: 10 }, (_, index) => t.get(`--observe-${index + 1}`));
    assert.ok(
      slots.every(color => /^#[0-9a-f]{6}$/.test(color)),
      `${key} has ten canvas-safe colors`,
    );
    assert.equal(new Set(slots).size, 10, `${key} chart slots are distinct`);
    assert.equal(slots[0], t.get('--data'), `${key} chart and KPI primary colors agree`);
    for (let index = 0; index < slots.length; index += 1) {
      const lightColor = oklch(slots[index]);
      const darkColor = oklch(darkChart.get(`--observe-${index + 1}`));
      assert.ok(hueGap(lightColor.hue, darkColor.hue) < 6, `${key} slot ${index + 1} keeps its hue`);
      assert.ok(lightColor.lightness < darkColor.lightness, `${key} slot ${index + 1} is deeper on white`);
    }
    assert.ok(oklch(slots[9]).chroma < 0.03, `${key} free-memory color stays neutral`);
  }
});

test('light chart colors remain readable on white cards and palette surfaces', () => {
  for (const key of PALETTES) {
    const t = light[key];
    for (let index = 1; index <= 10; index += 1) {
      const color = t.get(`--observe-${index}`);
      assert.ok(color, `${key} declares slot ${index}`);
      for (const paper of ['#ffffff', t.get('--sheet'), t.get('--block')]) {
        assert.ok(contrast(color, paper) >= 4.5, `${key} slot ${index} on ${paper}`);
      }
    }
    const rgb = channels(t.get('--data')).map(c => Math.round(c * 255));
    assert.equal(t.get('--data-wash'), `rgba(${rgb.join(', ')}, 0.09)`, `${key} data wash follows its hue`);
  }
});

test('unstacked chart fills are tinted against the resolved card color', () => {
  const observeChart = readFileSync(new URL('../src/ui/observe-chart.ts', import.meta.url), 'utf8');
  const telemetry = readFileSync(new URL('../src/panes/telemetry.tsx', import.meta.url), 'utf8');
  const nodeCharts = readFileSync(new URL('../src/panes/node-observation-charts.tsx', import.meta.url), 'utf8');
  assert.match(observeChart, /const paper = paperChannels\(options\.paper, key\);/);
  const unstacked = [telemetry, nodeCharts].flatMap(source => [
    ...source.matchAll(/observeAreaStyle\([^)]*\{ count: [^}]*\}\)/g),
  ]);
  assert.equal(unstacked.length, 3);
  for (const [call] of unstacked) assert.match(call, /paper: cv\('--card'/);
});

test('jinzi describes its in-family data colors in the palette menu', () => {
  assert.match(paletteSource, /key: 'jinzi',\s*name: '堇紫',\s*description: '深紫操作 · 同色系数据'/);
});

test('dark palettes share one tinted-gray ladder', () => {
  const lstar = hex => {
    const y = luminance(hex);
    return y > 0.008856 ? 116 * Math.cbrt(y) - 16 : 903.3 * y;
  };
  // 台面 L* 1.5，其余各档沿用纯黑色阶的层间距；每档取不低于目标的第一个 8 位色。
  const ladder = {
    '--ground': 0,
    '--sunk': 1.91,
    '--surface': 3.32,
    '--band': 3.96,
    '--sheet': 4.31,
    '--block': 8.24,
    '--line': 12.24,
  };
  // 底调取主题色相；玄墨的灰白主题色没有色相，取数据色的钢青。
  const hues = { jinzi: 298, dailan: 262, songlv: 132, oufen: 348, xuanmo: 225 };
  for (const key of PALETTES) {
    for (const [name, step] of Object.entries(ladder)) {
      const value = dark[key].get(name);
      const target = 1.5 + step;
      assert.ok(lstar(value) >= target && lstar(value) < target + 0.4, `${key} ${name} ${value} sits at L* ${target}`);
      const color = oklch(value);
      // 彩度 0.008 左右：能看出底调，又不会让面板读成彩色卡纸。
      assert.ok(color.chroma >= 0.004 && color.chroma <= 0.012, `${key} ${name} ${value} is a low-chroma gray`);
      // 低明度下 8 位量化会让色相偏离设定值，20° 以内仍属于同一色系。
      assert.ok(hueGap(color.hue, hues[key]) < 20, `${key} ${name} ${value} leans toward ${hues[key]}°`);
    }
  }
});

test('dark theme colors keep tab, link and filled-control text at WCAG AA', () => {
  for (const key of PALETTES) {
    assert.ok(contrast(dark[key].get('--action'), dark[key].get('--sheet')) >= 4.5, `${key} action on sheet`);
    assert.ok(contrast(dark[key].get('--action-on'), dark[key].get('--action')) >= 4.5, `${key} text on action`);
  }
});

test('primary buttons and the selected navigation item read the primary token set', () => {
  assert.match(
    styles,
    /^\.btn\.primary \{[^}]*border-color: var\(--primary-line\);[^}]*background: var\(--primary\);[^}]*color: var\(--primary-on\);/m,
  );
  assert.match(styles, /\.btn\.primary:hover:not\(:disabled\) \{\s*background: var\(--primary-hover\);/);
  assert.match(styles, /^\.btn\.primary:active:not\(:disabled\) \{\s*background: var\(--primary-active\);/m);
  assert.match(
    styles,
    /^\.fg-nv\[aria-current='true'\] \{[^}]*background: var\(--primary\);[^}]*color: var\(--primary-on\);[^}]*box-shadow: inset 0 0 0 1px var\(--primary-line\);/m,
  );
  assert.match(styles, /^\.fg-nv\[aria-current='true'\] \.mark \{\s*background: var\(--primary-on\);/m);
});

test('dark primary buttons are washes with readable labels', () => {
  for (const key of PALETTES) {
    const t = dark[key];
    assert.notEqual(t.get('--primary'), t.get('--action'), key);
    assert.ok(
      contrast(t.get('--primary'), t.get('--band')) < contrast(t.get('--action'), t.get('--band')),
      `${key} wash is quieter`,
    );
    for (const state of ['--primary', '--primary-hover', '--primary-active']) {
      assert.ok(contrast(t.get('--primary-on'), t.get(state)) >= 4.5, `${key} label on ${state}`);
    }
  }
});

test('every dark palette token is redeclared for the light theme', () => {
  for (const key of PALETTES) {
    for (const name of dark[key].keys())
      assert.ok(light[key].has(name), `${name} is missing from the light ${key} block`);
  }
});

test('palette menu swatches show the dark action colors', () => {
  for (const key of PALETTES) {
    const swatch = paletteSource.match(new RegExp(`key: '${key}',[\\s\\S]*?action: '(#[0-9a-f]{6})'`))?.[1];
    assert.equal(swatch, dark[key].get('--action'), key);
  }
});

test('interface edges stay independent of and softer than the background grid', () => {
  const cases = [
    ['default dark', tokens("[data-theme='dark']")],
    ['default light', tokens("[data-theme='light']")],
    ...PALETTES.flatMap(key => [
      [`dark ${key}`, dark[key]],
      [`light ${key}`, light[key]],
    ]),
  ];
  for (const [name, t] of cases) {
    assert.match(t.get('--grid-line'), /^#[0-9a-f]{6}$/, `${name} grid has its own color`);
    assert.match(t.get('--line'), /^#[0-9a-f]{6}$/, `${name} edge has its own color`);
    for (const surface of ['--sheet', '--block']) {
      assert.ok(
        contrast(t.get('--line'), t.get(surface)) < contrast(t.get('--grid-line'), t.get(surface)),
        `${name} edge is softer on ${surface}`,
      );
    }
  }
});

test('dark grid lines follow muted palette hues at a consistent low lightness', () => {
  const previousLightLines = ['#dedce4', '#d9dde6', '#dcded6', '#e2dce0', '#dbdcda'];
  assert.equal(new Set(PALETTES.map(key => dark[key].get('--grid-line'))).size, PALETTES.length);
  for (const [index, key] of PALETTES.entries()) {
    const color = oklch(dark[key].get('--grid-line'));
    const neutral = key === 'xuanmo';
    const reference = oklch(dark[key].get(neutral ? '--data' : '--action'));
    assert.ok(Math.abs(color.lightness - 0.278) < 0.004, `${key} grid does not brighten the canvas`);
    assert.ok(Math.abs(color.chroma - (neutral ? 0.009 : 0.02)) < 0.004, `${key} grid remains muted`);
    assert.ok(hueGap(color.hue, reference.hue) < 12, `${key} grid follows its palette hue`);
    assert.equal(light[key].get('--grid-line'), previousLightLines[index], `${key} light grid is unchanged`);
  }
});

test('workspace backgrounds share the theme-aware square grid without painting over content', () => {
  // Both theme scopes rebuild the image so a nested light surface uses its own colors.
  for (const theme of ['dark', 'light']) {
    const t = tokens(`[data-theme='${theme}']`);
    const image = t.get('--grid-image');
    assert.ok(image);
    assert.equal((image.match(/linear-gradient/g) ?? []).length, 2);
    assert.match(
      image,
      /linear-gradient\(color-mix\(in srgb, var\(--grid-line\) 85%, transparent\) 1px, transparent 1px\)/,
    );
    assert.match(
      image,
      /linear-gradient\(90deg, color-mix\(in srgb, var\(--grid-line\) 85%, transparent\) 1px, transparent 1px\)/,
    );
    assert.doesNotMatch(image, /var\(--line(?:-soft)?\)/);
    assert.doesNotMatch(image, /url\(/);
  }
  assert.doesNotMatch(styles, /--desk-mist-(?:image|opacity)/);
  assert.match(
    styles,
    /#stage::before,\s*\.topo-deck::before,\s*\.loading-skeleton-canvas::before,\s*\.fg-desk > \.fg-view::before\s*\{[^}]*z-index:\s*-1;[^}]*pointer-events:\s*none;[^}]*background-image:\s*var\(--grid-image\);[^}]*background-size:\s*32px 32px;[^}]*background-position:\s*center top;[^}]*background-attachment:\s*fixed;/s,
  );
});
