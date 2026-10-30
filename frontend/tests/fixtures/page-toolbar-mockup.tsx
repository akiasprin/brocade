// Mockup: one button language for every page-head toolbar (time range, 节点 / 订阅, 更多, the page's main
// action). The production App is mounted on a synthetic backend; each option is a CSS layer keyed on
// <html data-tb>, so the real toolbars, menus and dialogs show the option in place. Options sit on the Apple
// system colors from the palette mockup (iOS base × blue); 现状 keeps the production palette.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/page-toolbar-mockup.html?tb=a#/nodes/node/hk-hkix
// Query: tb=a|b|c|d|current, theme=dark|light, nav=0 hides the review dock.
// Toolbars live on #/nodes/node/hk-hkix (观测 / 解锁 tabs), #/users/user/alice and #/deploy.
import { createRoot } from 'react-dom/client';
import { useEffect, useState, useSyncExternalStore } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { App } from '../../src/app';
import { observeBrowserAppearance } from '../../src/forge/browser-appearance';
import { palette, type Palette } from '../../src/forge/palette';
import { theme } from '../../src/forge/theme';
import { SegmentedControl } from '../../src/ui/bits';
import { installBackend } from './page-toolbar-mockup-data';
import '../../src/styles.css';
import './palette-mockup-schemes.css';
import './page-toolbar-mockup.css';

installBackend();
observeBrowserAppearance();

type Option = 'a' | 'b' | 'c' | 'd' | 'current';

const OPTIONS: { value: Option; name: string; note: string }[] = [
  { value: 'a', name: 'A 灰底胶囊', note: '系统灰胶囊，图标与文字取强调色；主操作为蓝色实底。' },
  { value: 'b', name: 'B 无底蓝字', note: '平时只有蓝色图标与文字，悬停才出现灰底；主操作为蓝色实底。' },
  { value: 'c', name: 'C 玻璃分组', note: '相关操作合进同一枚玻璃胶囊，图标与文字用正文色；主操作为蓝色玻璃。' },
  { value: 'd', name: 'D 原生按钮', note: '白色浮起的圆角按钮，细阴影收边；时间范围带蓝色上下箭头。' },
  { value: 'current', name: '现状', note: '生产当前外观与配色。' },
];

const PAGES = [
  { value: '#/nodes/node/hk-hkix', label: '机器' },
  { value: '#/users/user/alice', label: '用户' },
  { value: '#/deploy', label: '发布' },
];

const params = new URLSearchParams(location.search);
const pickOption = (value: string | null): Option =>
  OPTIONS.some(item => item.value === value) ? (value as Option) : 'a';

/* ══ 方案状态 ══
   方案只改 <html> 上的 data-tb / data-base / data-accent；图表在渲染时读取令牌，需要重绘。生产图表以调色盘
   快照作为重绘依赖，这里让快照带上方案名，切换时通知订阅者，图表就地重绘，不必重挂整页。 */

let option: Option = pickOption(params.get('tb'));
const optionListeners = new Set<() => void>();
const baseSnapshot = palette.snapshot;
palette.snapshot = () => (option === 'current' ? baseSnapshot() : (`${baseSnapshot()}:tb` as Palette));
const emitPalette = () => (palette as unknown as { emit(): void }).emit();

function writeQuery(key: string, value: string) {
  const next = new URLSearchParams(location.search);
  next.set(key, value);
  history.replaceState(history.state, '', `${location.pathname}?${next}${location.hash}`);
}

function setOption(next: Option) {
  const wasCurrent = option === 'current';
  option = next;
  const root = document.documentElement;
  if (next === 'current') {
    delete root.dataset.tb;
    delete root.dataset.base;
    delete root.dataset.accent;
  } else {
    root.dataset.tb = next;
    root.dataset.base = 'ios';
    root.dataset.accent = 'blue';
  }
  writeQuery('tb', next);
  optionListeners.forEach(listener => listener());
  // 只有在苹果配色与生产配色之间切换时图表才需要重画。
  if (wasCurrent !== (next === 'current')) emitPalette();
}

const subscribeOption = (listener: () => void) => {
  optionListeners.add(listener);
  return () => {
    optionListeners.delete(listener);
  };
};
const optionSnapshot = () => option;

const pageOf = (hash: string) => PAGES.find(page => hash.startsWith(page.value))?.value ?? '';
const subscribeHash = (listener: () => void) => {
  window.addEventListener('hashchange', listener);
  return () => window.removeEventListener('hashchange', listener);
};

/* ══ 评审条 ══ */

function Dock() {
  const current = useSyncExternalStore(subscribeOption, optionSnapshot);
  const themeName = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const page = useSyncExternalStore(subscribeHash, () => pageOf(location.hash));
  const [folded, setFolded] = useState(false);
  const spec = OPTIONS.find(item => item.value === current)!;

  useEffect(() => writeQuery('theme', themeName), [themeName]);

  return (
    <div className={`mk-dock${folded ? ' folded' : ''}`} role="region" aria-label="工具栏方案评审">
      {folded ? (
        <button type="button" className="mk-unfold" onClick={() => setFolded(false)}>
          {spec.name}
        </button>
      ) : (
        <>
          <div className="mk-dock-row">
            <SegmentedControl
              value={current}
              options={OPTIONS.map(item => ({ value: item.value, label: item.name }))}
              onChange={setOption}
              ariaLabel="工具栏方案"
            />
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
            <p className="mk-dock-note">{spec.note}</p>
          </div>
        </>
      )}
    </div>
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
