// Mockup: four Apple design languages for the whole console. The production App is mounted on a synthetic
// backend; each language is a CSS layer keyed on <html data-lang>, on top of the Apple system color tokens from
// the palette mockup (palette-mockup-schemes.css), so every page, chart and dialog shows the language in place.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/design-language-mockup.html?lang=glass#/nodes
// Query: lang=glass|native|grouped|showcase|current, theme=dark|light, nav=0 hides the review dock.
// Routes are the app's own hashes: #/nodes, #/nodes/node/hk-hkix, #/chains, #/users/user/alice, #/usage, #/deploy.
import { createRoot } from 'react-dom/client';
import { useEffect, useState, useSyncExternalStore } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { App } from '../../src/app';
import { observeBrowserAppearance } from '../../src/forge/browser-appearance';
import { palette, type Palette } from '../../src/forge/palette';
import { theme } from '../../src/forge/theme';
import { SegmentedControl } from '../../src/ui/bits';
import { installBackend } from './design-language-mockup-data';
import '../../src/styles.css';
import './palette-mockup-schemes.css';
import './design-language-mockup-common.css';
import './design-language-mockup-glass.css';
import './design-language-mockup-native.css';
import './design-language-mockup-grouped.css';
import './design-language-mockup-showcase.css';
import './design-language-mockup.css';

installBackend();
observeBrowserAppearance();

type Lang = 'glass' | 'native' | 'grouped' | 'showcase' | 'current';

const LANGS: { value: Lang; name: string; base?: 'ios' | 'mac'; note: string }[] = [
  {
    value: 'glass',
    name: '液态玻璃',
    base: 'ios',
    note: 'macOS 26 的 Liquid Glass：侧栏与工具按钮是浮在内容之上的玻璃层，控件为胶囊形，圆角与外框同心。',
  },
  {
    value: 'native',
    name: '原生桌面',
    base: 'mac',
    note: 'macOS 原生应用：贴边侧栏与彩色图标块，工具栏标题，分组框与紧凑控件，信息密度最高。',
  },
  {
    value: 'grouped',
    name: '分组卡片',
    base: 'ios',
    note: 'iPadOS 与「健康」：顶部浮动页签，大标题，白卡分组，彩色分类标签，字号与点按尺寸放大。',
  },
  {
    value: 'showcase',
    name: '展示页',
    base: 'ios',
    note: 'apple.com：细全局导航，大字号标题与读数，无边框色块，胶囊按钮，留白最多。',
  },
  { value: 'current', name: '现状', note: '生产当前外观。' },
];

const params = new URLSearchParams(location.search);
const pickLang = (value: string | null): Lang =>
  LANGS.some(item => item.value === value) ? (value as Lang) : 'glass';

/* ══ 语言状态 ══
   语言只改 <html> 上的 data-lang / data-base / data-accent；图表在渲染时读取令牌，需要重绘。生产图表以调色盘
   快照作为重绘依赖，这里让快照带上语言名，切换时通知订阅者，图表就地重绘，不必重挂整页。 */

let lang: Lang = pickLang(params.get('lang'));
const langListeners = new Set<() => void>();
const baseSnapshot = palette.snapshot;
palette.snapshot = () => (lang === 'current' ? baseSnapshot() : (`${baseSnapshot()}:${lang}` as Palette));
const emitPalette = () => (palette as unknown as { emit(): void }).emit();

function writeQuery(key: string, value: string) {
  const next = new URLSearchParams(location.search);
  next.set(key, value);
  history.replaceState(history.state, '', `${location.pathname}?${next}${location.hash}`);
}

function setLang(next: Lang) {
  lang = next;
  const root = document.documentElement;
  const spec = LANGS.find(item => item.value === next)!;
  if (spec.base) {
    root.dataset.lang = next;
    root.dataset.base = spec.base;
    root.dataset.accent = 'blue';
  } else {
    delete root.dataset.lang;
    delete root.dataset.base;
    delete root.dataset.accent;
  }
  writeQuery('lang', next);
  langListeners.forEach(listener => listener());
  emitPalette();
}

const subscribeLang = (listener: () => void) => {
  langListeners.add(listener);
  return () => {
    langListeners.delete(listener);
  };
};
const langSnapshot = () => lang;

/* ══ 评审条 ══ */

function Dock() {
  const current = useSyncExternalStore(subscribeLang, langSnapshot);
  const themeName = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const [folded, setFolded] = useState(false);
  const spec = LANGS.find(item => item.value === current)!;

  useEffect(() => writeQuery('theme', themeName), [themeName]);

  return (
    <div className={`mk-dock${folded ? ' folded' : ''}`} role="region" aria-label="设计语言评审">
      {folded ? (
        <button type="button" className="mk-unfold" onClick={() => setFolded(false)}>
          {spec.name}
        </button>
      ) : (
        <>
          <div className="mk-dock-row">
            <SegmentedControl
              value={current}
              options={LANGS.map(item => ({ value: item.value, label: item.name }))}
              onChange={setLang}
              ariaLabel="设计语言"
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
            <button type="button" className="mk-fold" aria-label="收起评审条" onClick={() => setFolded(true)}>
              ×
            </button>
          </div>
          <p className="mk-dock-note">{spec.note}</p>
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
