// Mockup: 机器详情 → 配置 → 来源 IP 封禁. The production App runs on a synthetic backend (the palette mockup's
// fleet plus one machine's ban lists); each option is portalled in place of the production panel, so the
// neighbouring cards, the page and the theme are the real ones. Options add and lift bans through the
// production useSourceAccess hook; after a save the machine shows 同步中 for about four seconds.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/node-source-ban-mockup.html?option=a&scene=typical
// Query: option=a|b|c|d|current, scene=typical|useronly|empty|many|pending|error|unsupported|legacy,
//        open=none|add|users (opens a part on load), theme=dark|light, nav=0 hides the review dock.
import { createRoot } from 'react-dom/client';
import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from 'react';
import { createPortal } from 'react-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { App } from '../../src/app';
import { observeBrowserAppearance } from '../../src/forge/browser-appearance';
import { theme } from '../../src/forge/theme';
import { SegmentedControl } from '../../src/ui/bits';
import { installBackend, NODE, resetWorld, SCENE_KEYS, SCENES, type SceneKey } from './node-source-ban-mockup-data';
import { BanPanel, type OpenTarget, type Option } from './node-source-ban-mockup-ui';
import '../../src/styles.css';
import './node-source-ban-mockup.css';

type Choice = Option | 'current';

const OPTIONS: { value: Choice; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 分组清单',
    note: '常开卡片，去掉全部说明段落。「本机」一组可解除、可添加；「用户」一组按用户列出、只读，点名字到用户详情。两组用同一套行：禁行标记 · 地址 · 归属。',
  },
  {
    value: 'b',
    name: 'B 按地址合并',
    note: '每个地址只出现一次，行内写它对谁生效：「本机」或具体用户，命中任意一份即拒绝。只有含「本机」的行能在这里解除；解除后仍在用户名单里的地址留在列表中。',
  },
  {
    value: 'c',
    name: 'C 本机为主',
    note: '卡片只管这台机器能改的名单；用户名单收成卡底一行（头像 · 人数 · 条数），点开就地展开成与 A 相同的分组。',
  },
  {
    value: 'd',
    name: 'D 紧凑网格',
    note: '地址排成网格，只留旗帜，地区在悬停说明里；本机格子悬停出现 ×。适合几十个地址的机器，用户一行一位。',
  },
  { value: 'current', name: '现状', note: '生产当前的折叠面板（点标题展开）。' },
];

const OPEN_TARGETS = ['none', 'add', 'users'] as const;
const params = new URLSearchParams(location.search);
const pick = <T extends string>(value: string | null, values: readonly T[], fallback: T): T =>
  values.includes(value as T) ? (value as T) : fallback;

const initialScene = pick(params.get('scene'), SCENE_KEYS, 'typical');
const initialOpen = pick<OpenTarget>(params.get('open'), OPEN_TARGETS, 'none');

installBackend();
resetWorld(initialScene);
observeBrowserAppearance();
if (!location.hash.startsWith('#/nodes/node/')) location.hash = `#/nodes/node/${NODE}`;

function writeQuery(key: string, value: string) {
  const next = new URLSearchParams(location.search);
  next.set(key, value);
  history.replaceState(history.state, '', `${location.pathname}?${next}${location.hash}`);
}

/* ══ 打开「配置」页签 ══
   页签不进入地址，首次进入机器详情时替审阅者点一次。 */
function openConfigTab() {
  const observer = new MutationObserver(() => {
    const tab = [...document.querySelectorAll<HTMLButtonElement>('#root [role="tab"]')].find(
      item => item.textContent?.trim() === '配置',
    );
    if (!tab) return;
    observer.disconnect();
    if (tab.getAttribute('aria-selected') !== 'true') tab.click();
  });
  observer.observe(document.getElementById('root')!, { childList: true, subtree: true });
}
openConfigTab();

/**
 * Keeps an empty `display: contents` element right after the production panel, re-inserting it whenever the
 * pane re-renders that spot, and returns it as a portal target.
 */
function useSlot(enabled: boolean) {
  const [slot, setSlot] = useState<HTMLElement | null>(null);
  useEffect(() => {
    if (!enabled) {
      setSlot(null);
      return;
    }
    const element = document.createElement('div');
    element.className = 'mk-slot';
    const place = () => {
      const panel = document.querySelector('#root .nd-tab-config .source-access-panel');
      if (!panel?.parentNode) return;
      if (element.previousSibling === panel) return;
      panel.parentNode.insertBefore(element, panel.nextSibling);
    };
    place();
    setSlot(element);
    const observer = new MutationObserver(place);
    observer.observe(document.getElementById('root')!, { childList: true, subtree: true });
    return () => {
      observer.disconnect();
      element.remove();
    };
  }, [enabled]);
  return slot;
}

function Overlay({ option, open, stamp }: { option: Choice; open: OpenTarget; stamp: number }) {
  const slot = useSlot(option !== 'current');
  if (!slot || option === 'current') return null;
  return createPortal(<BanPanel key={`${option}-${stamp}`} option={option} nodeId={NODE} open={open} />, slot);
}

/* ══ 评审条 ══ */

function Dock({
  option,
  scene,
  onOption,
  onScene,
}: {
  option: Choice;
  scene: SceneKey;
  onOption: (next: Choice) => void;
  onScene: (next: SceneKey) => void;
}) {
  const themeName = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const [folded, setFolded] = useState(false);
  const spec = OPTIONS.find(item => item.value === option)!;
  useEffect(() => writeQuery('theme', themeName), [themeName]);
  return (
    <div className={`mk-dock${folded ? ' folded' : ''}`} role="region" aria-label="封禁面板方案评审">
      {folded ? (
        <button type="button" className="mk-unfold" onClick={() => setFolded(false)}>
          {spec.name}
        </button>
      ) : (
        <>
          <div className="mk-dock-row">
            <SegmentedControl
              value={option}
              options={OPTIONS.map(item => ({ value: item.value, label: item.name }))}
              onChange={onOption}
              ariaLabel="方案"
            />
            <button type="button" className="mk-fold" aria-label="收起评审条" onClick={() => setFolded(true)}>
              ×
            </button>
          </div>
          <div className="mk-dock-row">
            <select
              className="f mk-scene"
              value={scene}
              aria-label="场景"
              onChange={event => onScene(event.target.value as SceneKey)}
            >
              {SCENE_KEYS.map(key => (
                <option key={key} value={key}>
                  {SCENES[key].label}
                </option>
              ))}
            </select>
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
          <p className="mk-dock-note">{spec.note}</p>
        </>
      )}
    </div>
  );
}

const client = new QueryClient({
  defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false, refetchOnReconnect: false } },
});

function Mockup() {
  const [option, setOption] = useState<Choice>(pick(params.get('option'), OPTIONS.map(item => item.value), 'a'));
  const [scene, setScene] = useState<SceneKey>(initialScene);
  // 场景或方案切换后，打开页面时预先展开的部分不再生效，面板也从头挂载。
  const [stamp, setStamp] = useState(0);
  const first = useRef(true);
  useLayoutEffect(() => {
    document.documentElement.classList.toggle('mk-hide-legacy', option !== 'current');
    writeQuery('option', option);
    writeQuery('scene', scene);
  }, [option, scene]);
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    resetWorld(scene);
    void client.invalidateQueries({ queryKey: ['source-access'] });
  }, [scene]);
  return (
    <>
      <App />
      <Overlay option={option} open={stamp === 0 ? initialOpen : 'none'} stamp={stamp} />
      {params.get('nav') !== '0' &&
        createPortal(
          <Dock
            option={option}
            scene={scene}
            onOption={next => {
              setOption(next);
              setStamp(value => value + 1);
            }}
            onScene={next => {
              setScene(next);
              setStamp(value => value + 1);
            }}
          />,
          document.getElementById('mk-dock')!,
        )}
    </>
  );
}

createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <Mockup />
  </QueryClientProvider>,
);
