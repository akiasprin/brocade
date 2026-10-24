// Mockup: Apple-style color schemes that replace the current palettes. The production App is mounted on a
// synthetic backend; each scheme only overrides CSS tokens (palette-mockup-schemes.css), so every page,
// chart and dialog shows the scheme exactly as production would.
// Two axes, like macOS appearance settings: a neutral base (iOS / macOS system grays) and an accent color.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/palette-mockup.html?base=ios&accent=blue#/nodes
// Query: base=ios|mac|current, accent=blue|purple|pink|teal|graphite, theme=dark|light, view=app|tokens,
// nav=0 hides the review dock. Routes are the app's own hashes: #/nodes, #/nodes/node/hk-hkix, #/chains,
// #/users/user/alice, #/usage, #/topo.
import { createRoot } from 'react-dom/client';
import { useEffect, useMemo, useState, useSyncExternalStore, type CSSProperties } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { App } from '../../src/app';
import { observeBrowserAppearance } from '../../src/forge/browser-appearance';
import { palette, PALETTES, type Palette } from '../../src/forge/palette';
import { theme, type Theme } from '../../src/forge/theme';
import { SegmentedControl } from '../../src/ui/bits';
import { installBackend } from './palette-mockup-data';
import '../../src/styles.css';
import './palette-mockup-schemes.css';
import './palette-mockup.css';

installBackend();
observeBrowserAppearance();

type Base = 'ios' | 'mac' | 'current';
type Accent = 'blue' | 'purple' | 'pink' | 'teal' | 'graphite';
type Look = { base: Base; accent: Accent };
type View = 'app' | 'tokens';

const BASES: { value: Base; name: string; note: string }[] = [
  { value: 'ios', name: 'iOS', note: '暗色纯黑底，卡片取 iOS 分组灰；亮色为分组灰底加白纸。' },
  { value: 'mac', name: 'macOS', note: '暗色为 macOS 中性深灰，层级更柔和；亮色为窗口灰底加白内容区。' },
  { value: 'current', name: '现状', note: '生产当前取值，可切换现有五套调色盘对照。' },
];

const ACCENTS: { value: Accent; name: string }[] = [
  { value: 'blue', name: '蓝' },
  { value: 'purple', name: '紫' },
  { value: 'pink', name: '粉' },
  { value: 'teal', name: '青' },
  { value: 'graphite', name: '石墨' },
];

const params = new URLSearchParams(location.search);
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

/* ══ 方案状态 ══
   方案只改 <html> 上的 data-base / data-accent；图表在渲染时读取令牌，需要重绘。生产图表以调色盘快照
   作为重绘依赖，这里让快照带上方案名，切换时通知订阅者，图表就地重绘，不必重挂整页。 */

let look: Look = {
  base: pick(
    params.get('base'),
    BASES.map(item => item.value),
    'ios',
  ),
  accent: pick(
    params.get('accent'),
    ACCENTS.map(item => item.value),
    'blue',
  ),
};
const lookListeners = new Set<() => void>();
const baseSnapshot = palette.snapshot;
palette.snapshot = () =>
  look.base === 'current' ? baseSnapshot() : (`${baseSnapshot()}:${look.base}:${look.accent}` as Palette);
const emitPalette = () => (palette as unknown as { emit(): void }).emit();

function writeQuery(key: string, value: string) {
  const next = new URLSearchParams(location.search);
  next.set(key, value);
  history.replaceState(history.state, '', `${location.pathname}?${next}${location.hash}`);
}

function applyLook(root: HTMLElement, target: Look) {
  if (target.base === 'current') {
    delete root.dataset.base;
    delete root.dataset.accent;
  } else {
    root.dataset.base = target.base;
    root.dataset.accent = target.accent;
  }
}

function setLook(next: Partial<Look>) {
  look = { ...look, ...next };
  applyLook(document.documentElement, look);
  writeQuery('base', look.base);
  writeQuery('accent', look.accent);
  lookListeners.forEach(listener => listener());
  emitPalette();
}

const subscribeLook = (listener: () => void) => {
  lookListeners.add(listener);
  return () => {
    lookListeners.delete(listener);
  };
};
const lookSnapshot = () => look;

/* ══ 令牌读取 ══ */

const TOKENS = [
  'ground',
  'sheet',
  'surface',
  'block',
  'card',
  'band',
  'sunk',
  'line',
  'grid-line',
  'line-soft',
  'grid-image',
  'ink',
  'ink-2',
  'ink-3',
  'ink-4',
  'action',
  'action-hover',
  'action-active',
  'action-wash',
  'action-on',
  'focus',
  'primary',
  'primary-hover',
  'primary-active',
  'primary-on',
  'primary-line',
  'data',
  'data-secondary',
  'data-wash',
  ...Array.from({ length: 10 }, (_, index) => `observe-${index + 1}`),
  'hue-1',
  'hue-2',
  'hue-3',
  'hue-4',
  'ok',
  'gold',
  'err',
  'warn',
  'gold-line',
  'gold-wash',
  'hover',
  'hover-2',
  'btn',
  'btn-h',
  'btn-a',
  'glass',
  'glass-strong',
  'shadow',
  'sheet-shadow',
  'code-bg',
  'code-ink',
  'code-k',
  'code-s',
  'code-n',
  'code-c',
] as const;
type TokenMap = Record<(typeof TOKENS)[number], string>;

/** 临时把 <html> 切到某方案读取全部令牌，同一任务内复原，不会产生中间帧。 */
function readTokens(target: Look, themeName: Theme): TokenMap {
  const root = document.documentElement;
  const saved = { base: root.dataset.base, accent: root.dataset.accent, theme: root.dataset.theme };
  applyLook(root, target);
  root.dataset.theme = themeName;
  const style = getComputedStyle(root);
  const values = Object.fromEntries(TOKENS.map(name => [name, style.getPropertyValue(`--${name}`).trim()])) as TokenMap;
  for (const key of ['base', 'accent', 'theme'] as const) {
    if (saved[key] === undefined) delete root.dataset[key];
    else root.dataset[key] = saved[key];
  }
  return values;
}

let probe: HTMLSpanElement | null = null;
/** 任意 CSS 颜色（含 color-mix）→ [r, g, b, a]，0–255。 */
function rgbOf(color: string): [number, number, number, number] {
  if (!probe) {
    probe = document.createElement('span');
    probe.style.display = 'none';
    document.body.append(probe);
  }
  probe.style.color = '';
  probe.style.color = color;
  const value = getComputedStyle(probe).color;
  const srgb = /color\(srgb ([\d.e-]+) ([\d.e-]+) ([\d.e-]+)(?: \/ ([\d.e-]+))?\)/.exec(value);
  if (srgb) return [+srgb[1] * 255, +srgb[2] * 255, +srgb[3] * 255, srgb[4] === undefined ? 1 : +srgb[4]];
  const rgb = /rgba?\(([\d.]+),? ([\d.]+),? ([\d.]+)(?:[,/] ?([\d.]+))?\)/.exec(value);
  if (rgb) return [+rgb[1], +rgb[2], +rgb[3], rgb[4] === undefined ? 1 : +rgb[4]];
  return [0, 0, 0, 1];
}

function luminance([r, g, b]: number[]) {
  const lin = (c: number) => {
    const v = c / 255;
    return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}

/** 前景（可半透明）压在背景上之后的 WCAG 对比度。 */
function contrast(foreground: string, background: string) {
  const bg = rgbOf(background);
  const fg = rgbOf(foreground);
  const mixed = fg.slice(0, 3).map((channel, index) => channel * fg[3] + bg[index] * (1 - fg[3]));
  const [x, y] = [luminance(mixed), luminance(bg)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
}

const hexOf = (color: string) =>
  '#' +
  rgbOf(color)
    .slice(0, 3)
    .map(channel => Math.round(channel).toString(16).padStart(2, '0'))
    .join('');

/* ══ 令牌对照页 ══ */

const SURFACES: { token: keyof TokenMap; label: string }[] = [
  { token: 'ground', label: '台面' },
  { token: 'surface', label: '外框' },
  { token: 'sheet', label: '纸' },
  { token: 'block', label: '块' },
  { token: 'band', label: '带' },
  { token: 'sunk', label: '输入格' },
  { token: 'line', label: '边框' },
];

function Ratio({ value, min }: { value: number; min: number }) {
  return <b className={value >= min ? 'mk-ratio' : 'mk-ratio low'}>{value.toFixed(2)}</b>;
}

function Spark({ seed }: { seed: number }) {
  const points = Array.from({ length: 48 }, (_, index) => {
    const y = 0.55 + 0.22 * Math.sin(index / 5 + seed) + 0.1 * Math.sin(index / 1.7 + seed * 2);
    return `${(index / 47) * 100},${(1 - y) * 40 + 4}`;
  });
  const second = Array.from({ length: 48 }, (_, index) => {
    const y = 0.3 + 0.12 * Math.sin(index / 4 + seed + 1.3) + 0.05 * Math.sin(index / 1.3 + seed);
    return `${(index / 47) * 100},${(1 - y) * 40 + 4}`;
  });
  return (
    <svg className="mk-spark" viewBox="0 0 100 48" preserveAspectRatio="none" aria-hidden="true">
      <polygon points={`0,48 ${points.join(' ')} 100,48`} fill="var(--data-wash)" />
      <polyline
        points={points.join(' ')}
        fill="none"
        stroke="var(--data)"
        strokeWidth="1.4"
        vectorEffect="non-scaling-stroke"
      />
      <polyline
        points={second.join(' ')}
        fill="none"
        stroke="var(--observe-2)"
        strokeWidth="1.4"
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  );
}

function SchemeColumn({ target, title, themeName }: { target: Look; title: string; themeName: Theme }) {
  const tokens = useMemo(() => readTokens(target, themeName), [target, themeName]);
  const [seg, setSeg] = useState('day');
  const style = Object.fromEntries(TOKENS.map(name => [`--${name}`, tokens[name]])) as CSSProperties;
  return (
    <section className="mk-col" style={{ ...style, colorScheme: themeName }} data-theme={themeName}>
      <header className="mk-col-head">
        <h2>{title}</h2>
      </header>

      <div className="mk-stage">
        <div className="mk-stage-top">
          <span className="mk-stage-brand">Brocade</span>
          <button type="button" className="fg-nv" aria-current="true">
            机器
          </button>
          <button type="button" className="fg-nv">
            线路
          </button>
        </div>
        <div className="mk-stage-sheet">
          <div className="mk-stage-block">
            <div className="mk-stage-band">
              <span>用量与额度</span>
              <span className="st st-ok">在线</span>
            </div>
            <div className="mk-stage-body">
              <p className="mk-ink">
                <span style={{ color: 'var(--ink)' }}>正文 41.20 GiB</span>
                <span style={{ color: 'var(--ink-2)' }}>次要文字</span>
                <span style={{ color: 'var(--ink-3)' }}>说明文字</span>
                <span style={{ color: 'var(--ink-4)' }}>弱化</span>
              </p>
              <input className="f" defaultValue="hk-hkix" aria-label="示例输入" />
              <div className="mk-row">
                <button type="button" className="btn primary">
                  保存
                </button>
                <button type="button" className="btn">
                  取消
                </button>
                <SegmentedControl
                  value={seg}
                  options={[
                    { value: 'day', label: '日' },
                    { value: 'month', label: '月' },
                  ]}
                  onChange={setSeg}
                  ariaLabel="示例分段"
                />
                <a href="#/nodes" style={{ color: 'var(--action)' }} onClick={event => event.preventDefault()}>
                  查看详情
                </a>
              </div>
              <div className="mk-row">
                <span className="st st-ok">已收敛</span>
                <span className="st st-gold">已变更</span>
                <span className="st st-halted">失败</span>
                <span className="st st-running">推送中</span>
              </div>
              <Spark seed={title.length} />
            </div>
          </div>
        </div>
      </div>

      <dl className="mk-swatches">
        {SURFACES.map(item => (
          <div key={item.token}>
            <dt>{item.label}</dt>
            <dd>
              <i style={{ background: `var(--${item.token})` }} />
              <code>{hexOf(tokens[item.token])}</code>
            </dd>
          </div>
        ))}
      </dl>

      <div className="mk-roles">
        {(['action', 'action-hover', 'primary', 'data', 'data-secondary'] as const).map(token => (
          <span key={token} title={`--${token}`}>
            <i style={{ background: `var(--${token})` }} />
            <code>{hexOf(tokens[token])}</code>
          </span>
        ))}
      </div>

      <div className="mk-observe" title="--observe-1 … --observe-10">
        {Array.from({ length: 10 }, (_, index) => (
          <i key={index} style={{ background: `var(--observe-${index + 1})` }} />
        ))}
      </div>
      <div className="mk-observe mk-hue" title="--hue-1 … --hue-4 · --ok --gold --err">
        {[1, 2, 3, 4].map(index => (
          <i key={index} style={{ background: `var(--hue-${index})` }} />
        ))}
        <b />
        {(['ok', 'gold', 'err'] as const).map(token => (
          <i key={token} style={{ background: `var(--${token})` }} />
        ))}
      </div>

      <table className="mk-contrast">
        <tbody>
          <tr>
            <th>操作色文字 / 纸</th>
            <td>
              <Ratio value={contrast(tokens.action, tokens.sheet)} min={4.5} />
            </td>
          </tr>
          <tr>
            <th>主按钮文字 / 底</th>
            <td>
              <Ratio value={contrast(tokens['primary-on'], tokens.primary)} min={4.5} />
            </td>
          </tr>
          <tr>
            <th>说明文字 / 纸</th>
            <td>
              <Ratio value={contrast(tokens['ink-3'], tokens.sheet)} min={4.5} />
            </td>
          </tr>
          <tr>
            <th>数据线 / 卡</th>
            <td>
              <Ratio value={contrast(tokens.data, tokens.card)} min={3} />
            </td>
          </tr>
          <tr>
            <th>块 / 纸 明度差</th>
            <td>
              <b className="mk-ratio">{contrast(tokens.block, tokens.sheet).toFixed(2)}</b>
            </td>
          </tr>
        </tbody>
      </table>
    </section>
  );
}

/** 对照页：当前中性底下的五种强调色并排，最后一列是生产现状。 */
function TokenSheet({ base, themeName }: { base: Base; themeName: Theme }) {
  const shown = base === 'current' ? 'ios' : base;
  const baseName = BASES.find(item => item.value === shown)!.name;
  const columns = useMemo(
    () => [
      ...ACCENTS.map(item => ({
        key: item.value,
        title: `${baseName} · ${item.name}`,
        target: { base: shown, accent: item.value },
      })),
      { key: 'current', title: '现状', target: { base: 'current' as const, accent: 'blue' as const } },
    ],
    [shown, baseName],
  );
  return (
    <div className="mk-sheet">
      <div className="mk-grid">
        {columns.map(column => (
          <SchemeColumn
            key={`${column.key}-${themeName}`}
            target={column.target}
            title={column.title}
            themeName={themeName}
          />
        ))}
      </div>
    </div>
  );
}

/* ══ 评审条 ══ */

function Dock() {
  const current = useSyncExternalStore(subscribeLook, lookSnapshot);
  const themeName = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const paletteKey = useSyncExternalStore(palette.subscribe, baseSnapshot);
  const [view, setView] = useState<View>(pick(params.get('view'), ['app', 'tokens'] as const, 'app'));
  const [folded, setFolded] = useState(false);
  const base = BASES.find(item => item.value === current.base)!;
  const accent = ACCENTS.find(item => item.value === current.accent)!;
  const label = current.base === 'current' ? base.name : `${base.name} · ${accent.name}`;

  useEffect(() => {
    writeQuery('view', view);
    document.documentElement.classList.toggle('mk-tokens-open', view === 'tokens');
  }, [view]);
  useEffect(() => writeQuery('theme', themeName), [themeName]);

  return (
    <>
      {view === 'tokens' && <TokenSheet base={current.base} themeName={themeName} />}
      <div className={`mk-dock${folded ? ' folded' : ''}`} role="region" aria-label="配色评审">
        {folded ? (
          <button type="button" className="mk-unfold" onClick={() => setFolded(false)}>
            {label}
          </button>
        ) : (
          <>
            <div className="mk-dock-row">
              <SegmentedControl
                value={current.base}
                options={BASES.map(item => ({ value: item.value, label: item.name }))}
                onChange={next => setLook({ base: next })}
                ariaLabel="中性底"
              />
              {current.base === 'current' ? (
                <SegmentedControl
                  value={paletteKey}
                  options={PALETTES.map(option => ({ value: option.key, label: option.name }))}
                  onChange={next => palette.set(next)}
                  ariaLabel="现有调色盘"
                />
              ) : (
                <SegmentedControl
                  value={current.accent}
                  options={ACCENTS.map(item => ({ value: item.value, label: item.name }))}
                  onChange={next => setLook({ accent: next })}
                  ariaLabel="强调色"
                />
              )}
              <SegmentedControl
                value={themeName}
                options={[
                  { value: 'dark', label: '暗' },
                  { value: 'light', label: '亮' },
                ]}
                onChange={next => next !== theme.snapshot() && theme.toggle()}
                ariaLabel="主题"
              />
              <SegmentedControl
                value={view}
                options={[
                  { value: 'app', label: '界面' },
                  { value: 'tokens', label: '对照' },
                ]}
                onChange={setView}
                ariaLabel="视图"
              />
              <button type="button" className="mk-fold" aria-label="收起评审条" onClick={() => setFolded(true)}>
                ×
              </button>
            </div>
            <div className="mk-dock-row mk-dock-note">
              <span>{base.note}</span>
            </div>
          </>
        )}
      </div>
    </>
  );
}

const client = new QueryClient({
  defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false, refetchOnReconnect: false } },
});

createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <App />
  </QueryClientProvider>,
);
if (params.get('nav') !== '0') createRoot(document.getElementById('mk-dock')!).render(<Dock />);
