// Mockup: four ways to put color back into the console, which reads as grayscale since 黛蓝 moved to 灰雾蓝.
// The production App is mounted on a synthetic backend. Every option keeps production's neutral surfaces and
// only changes role colors (color-scheme-mockup-schemes.css) plus, for C and D, a CSS layer that moves where
// color appears (color-scheme-mockup.css). 现状 is production as it is today.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/color-scheme-mockup.html?scheme=a#/nodes/node/hk-hkix
// Query: scheme=a|b|c|d|current, theme=dark|light, view=page|grid (grid shows all five side by side),
// nav=0 hides the review dock.
import { createRoot } from 'react-dom/client';
import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { App } from '../../src/app';
import { observeBrowserAppearance } from '../../src/forge/browser-appearance';
import { palette, type Palette } from '../../src/forge/palette';
import { theme } from '../../src/forge/theme';
import { SegmentedControl } from '../../src/ui/bits';
import { installBackend } from './page-toolbar-mockup-data';
import '../../src/styles.css';
import './color-scheme-mockup-schemes.css';
import './color-scheme-mockup.css';

installBackend();
observeBrowserAppearance();

type Scheme = 'a' | 'b' | 'c' | 'd' | 'current';
type View = 'page' | 'grid';

const SCHEMES: { value: Scheme; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 系统蓝',
    note: '苹果系统蓝：导航选中与主按钮为蓝色实底白字，链接、页签与曲线取蓝；图表与状态改用系统色。',
  },
  {
    value: 'b',
    name: 'B 柔和蓝',
    note: '介于原宝蓝与现灰雾蓝之间：蓝色可辨但不刺眼，暗色主按钮仍为洗色；状态色不变。',
  },
  {
    value: 'c',
    name: 'C 多彩数据',
    note: '导航与页签回到中性，颜色留给数据：每项指标一个固定颜色，图表与状态用系统色；主按钮为系统蓝。',
  },
  {
    value: 'd',
    name: 'D 分区色',
    note: '每个分区一个颜色：导航图标是彩色图块，页面标题图标随分区着色；操作统一为系统蓝。',
  },
  { value: 'current', name: '现状', note: '生产当前配色（2026-10-31 已落地 B）。' },
];

const PAGES = [
  { value: '#/nodes/node/hk-hkix', label: '机器详情' },
  { value: '#/nodes', label: '机器' },
  { value: '#/chains', label: '线路' },
  { value: '#/users/user/alice', label: '用户' },
  { value: '#/usage', label: '用量' },
  { value: '#/deploy', label: '发布' },
];

const params = new URLSearchParams(location.search);
const pickScheme = (value: string | null): Scheme =>
  SCHEMES.some(item => item.value === value) ? (value as Scheme) : 'a';

/* ══ 方案状态 ══
   方案只改 <html> 上的 data-scheme；图表在渲染时读取令牌，需要重绘。生产图表以调色盘快照作为重绘依赖，
   这里让快照带上方案名，切换时通知订阅者，图表就地重绘，不必重挂整页。 */

let scheme: Scheme = pickScheme(params.get('scheme'));
const schemeListeners = new Set<() => void>();
const baseSnapshot = palette.snapshot;
palette.snapshot = () => (scheme === 'current' ? baseSnapshot() : (`${baseSnapshot()}:${scheme}` as Palette));
const emitPalette = () => (palette as unknown as { emit(): void }).emit();

function writeQuery(key: string, value: string) {
  const next = new URLSearchParams(location.search);
  next.set(key, value);
  history.replaceState(history.state, '', `${location.pathname}?${next}${location.hash}`);
}

function setScheme(next: Scheme) {
  scheme = next;
  const root = document.documentElement;
  if (next === 'current') delete root.dataset.scheme;
  else root.dataset.scheme = next;
  writeQuery('scheme', next);
  schemeListeners.forEach(listener => listener());
  emitPalette();
}

const subscribeScheme = (listener: () => void) => {
  schemeListeners.add(listener);
  return () => {
    schemeListeners.delete(listener);
  };
};
const schemeSnapshot = () => scheme;

const pageOf = (hash: string) =>
  PAGES.slice()
    .sort((left, right) => right.value.length - left.value.length)
    .find(page => hash === page.value || hash.startsWith(`${page.value}/`))?.value ?? '';
const subscribeHash = (listener: () => void) => {
  window.addEventListener('hashchange', listener);
  return () => window.removeEventListener('hashchange', listener);
};

/* ══ 并排视图 ══
   五个方案各开一个 iframe，同一页面、同一主题，按 1440 × 900 渲染后缩放到格子宽度。 */

const FRAME = { width: 1440, height: 900 };

function Tile({ value, name, src }: { value: Scheme; name: string; src: string }) {
  const boxRef = useRef<HTMLDivElement>(null);
  const [scale, setScale] = useState(0.4);
  useLayoutEffect(() => {
    const box = boxRef.current;
    if (!box) return;
    const observer = new ResizeObserver(() => setScale(box.clientWidth / FRAME.width));
    observer.observe(box);
    return () => observer.disconnect();
  }, []);
  return (
    <figure className="mk-tile" data-tile={value}>
      <figcaption>{name}</figcaption>
      <div className="mk-tile-box" ref={boxRef} style={{ height: FRAME.height * scale }}>
        <iframe
          title={name}
          src={src}
          style={{ width: FRAME.width, height: FRAME.height, transform: `scale(${scale})` }}
        />
      </div>
    </figure>
  );
}

function GridView({ page, themeName }: { page: string; themeName: string }) {
  return (
    <div className="mk-grid-view">
      {SCHEMES.map(item => (
        <Tile
          key={`${item.value}-${themeName}-${page}`}
          value={item.value}
          name={item.name}
          src={`${location.pathname}?scheme=${item.value}&theme=${themeName}&nav=0${page || location.hash}`}
        />
      ))}
    </div>
  );
}

/* ══ 评审条 ══ */

function Dock() {
  const current = useSyncExternalStore(subscribeScheme, schemeSnapshot);
  const themeName = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const page = useSyncExternalStore(subscribeHash, () => pageOf(location.hash));
  const [view, setView] = useState<View>(params.get('view') === 'grid' ? 'grid' : 'page');
  const [folded, setFolded] = useState(false);
  const spec = SCHEMES.find(item => item.value === current)!;

  useEffect(() => writeQuery('theme', themeName), [themeName]);
  useEffect(() => {
    writeQuery('view', view);
    document.documentElement.classList.toggle('mk-grid-open', view === 'grid');
  }, [view]);

  return (
    <>
      {view === 'grid' && <GridView page={page} themeName={themeName} />}
      <div className={`mk-dock${folded ? ' folded' : ''}`} role="region" aria-label="配色方案评审">
        {folded ? (
          <button type="button" className="mk-unfold" onClick={() => setFolded(false)}>
            {view === 'grid' ? '并排' : spec.name}
          </button>
        ) : (
          <>
            <div className="mk-dock-row">
              <SegmentedControl
                value={view}
                options={[
                  { value: 'page', label: '单页' },
                  { value: 'grid', label: '并排' },
                ]}
                onChange={setView}
                ariaLabel="视图"
              />
              {view === 'page' && (
                <SegmentedControl
                  value={current}
                  options={SCHEMES.map(item => ({ value: item.value, label: item.name }))}
                  onChange={setScheme}
                  ariaLabel="配色方案"
                />
              )}
              <button type="button" className="mk-fold" aria-label="收起评审条" onClick={() => setFolded(true)}>
                ×
              </button>
            </div>
            <div className="mk-dock-row">
              <SegmentedControl
                value={page}
                options={PAGES}
                onChange={next => {
                  location.hash = next;
                }}
                ariaLabel="页面"
              />
              <SegmentedControl
                value={themeName}
                options={[
                  { value: 'dark', label: '暗' },
                  { value: 'light', label: '亮' },
                ]}
                onChange={next => next !== theme.snapshot() && theme.toggle()}
                ariaLabel="主题"
              />
            </div>
            {view === 'page' && <p className="mk-dock-note">{spec.note}</p>}
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
