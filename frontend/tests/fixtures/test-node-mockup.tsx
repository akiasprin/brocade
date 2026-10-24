// Mockup: 线路 · 实验模式。线路标题栏的开关进入实验模式：应用外壳与线路面板退场，换成铺满视口的实验台
// （等距台面 + 机器架 + 测试节点 + 组装条与日志），可再进浏览器全屏。色板与令牌沿用原主题。
// Synthetic data only; never imported by the app.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/test-node-mockup.html
// Query: lab=1|0, scene=typical|empty|failed|expiring|restart|full, open=none|compose|hover|qr|menu,
//        palette=dailan|jinzi|songlv|oufen|xuanmo, theme=dark|light, w=<frame width, 0 = viewport>,
//        run=0 freezes the simulated deployment, nav=0 hides the review bar.
import { createRoot } from 'react-dom/client';
import { Fragment, useLayoutEffect, useMemo, useRef, useState, type ReactNode, type SyntheticEvent } from 'react';
import { createPortal } from 'react-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { DEFAULT_BRANDING } from '../../src/api';
import { palette, PALETTES, type Palette } from '../../src/forge/palette';
import { theme } from '../../src/forge/theme';
import { ChainsPane } from '../../src/panes/chains';
import { SessionProvider, type Session } from '../../src/session';
import { SegmentedControl } from '../../src/ui/bits';
import { BrandIcon } from '../../src/ui/branding';
import { Icon, type IconName } from '../../src/ui/icons';
import { useNarrow } from '../../src/ui/viewport';
import type { Win } from '../../src/wm/store';
import {
  chainSnapshot,
  clientName,
  e2eProbes,
  fleetAgentNodes,
  SCENE_KEYS,
  SCENES,
  type Scene,
  type SceneKey,
  type TestNode,
} from './test-node-mockup-data';
import { LabStage, LabSwitch, type LabPreset } from './test-node-mockup-lab';
import { useTestNodes } from './test-node-mockup-ui';
import '../../src/styles.css';
import './test-node-mockup.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
const requestedPalette = PALETTES.find(option => option.key === params.get('palette'))?.key;
if (requestedPalette && palette.snapshot() !== requestedPalette) palette.set(requestedPalette);

const PRESETS: { value: LabPreset; label: string }[] = [
  { value: 'none', label: '无' },
  { value: 'compose', label: '已排好路径' },
  { value: 'hover', label: '悬停机器与节点' },
  { value: 'qr', label: '二维码' },
  { value: 'menu', label: '更多操作' },
];

const WIDTHS = [
  { value: 0, label: '视口' },
  { value: 1280, label: '1280' },
  { value: 1024, label: '1024' },
  { value: 760, label: '760' },
  { value: 390, label: '390' },
];

/* ══ 生产组件的数据：普通模式挂真实 ChainsPane ══ */

const SESSION: Session = {
  who: {
    operator_id: 'admin',
    role: 'system-admin',
    tenant_scope: 'platform',
    token_prefix: null,
    masked_assets: false,
  },
  initial: { node_count: 12, chain_group_count: [] },
};

const RESPONSES: [string, () => unknown][] = [
  ['/model/snapshot', chainSnapshot],
  ['/nodes/agent-state', () => ({ nodes: fleetAgentNodes() })],
  ['/probes/e2e', e2eProbes],
  ['/whoami', () => SESSION.who],
  ['/revisions', () => ({ current_revision: 41, revisions: [{ id: 41 }] })],
];

globalThis.fetch = async (input: RequestInfo | URL) => {
  const raw = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
  const hit = RESPONSES.find(([path]) => new URL(raw, location.href).pathname === path);
  return hit ? Response.json(hit[1]()) : Response.json({ error: '本地示例不连接生产接口' }, { status: 404 });
};

function seededClient() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity }, mutations: { retry: false } },
  });
  client.setQueryData(['snapshot'], chainSnapshot());
  client.setQueryData(['nodes'], { nodes: fleetAgentNodes() });
  client.setQueryData(['e2e-probes'], e2eProbes());
  client.setQueryData(['revisions'], { current_revision: 41, revisions: [{ id: 41 }] });
  return client;
}

const CHAINS_WIN = {
  id: -1,
  key: 'tab:chains',
  title: '线路',
  x: 0,
  y: 0,
  w: 0,
  h: 0,
  z: 0,
  min: false,
  home: 'desk',
  data: {},
} satisfies Win;

/* ══ 普通模式：生产顶栏的静态复刻 + 真实线路页 ══ */

const NAV: { key: string; label: string; icon: IconName; mobile: boolean }[] = [
  { key: 'nodes', label: '机器', icon: 'nodes', mobile: true },
  { key: 'chains', label: '线路', icon: 'chains', mobile: true },
  { key: 'users', label: '用户', icon: 'users', mobile: true },
  { key: 'deploy', label: '发布', icon: 'deploy', mobile: false },
  { key: 'usage', label: '用量', icon: 'usage', mobile: true },
];

function Shell({ children }: { children: ReactNode }) {
  const narrow = useNarrow();
  const faces = NAV.filter(face => !narrow || face.mobile);
  return (
    <div className={`forge mk-forge${narrow ? ' narrow' : ''}`}>
      <div className="fg-left">
        <div className="fg-desk">
          <div className={`fg-top${narrow ? ' fg-navrow' : ''}`}>
            <button type="button" className="fg-home" aria-label="返回机器首页" tabIndex={-1}>
              <BrandIcon branding={DEFAULT_BRANDING} />
              <span className="fg-brand">{DEFAULT_BRANDING.site_name}</span>
            </button>
            {!narrow && <span className="fg-vr" />}
            <nav className={narrow ? 'fg-facebar' : 'fg-nav'} aria-label="主导航">
              {faces.map(face => (
                <button
                  key={face.key}
                  type="button"
                  className="fg-nv"
                  aria-current={face.key === 'chains' ? 'true' : 'false'}
                  aria-label={face.label}
                  tabIndex={-1}
                >
                  <Icon of={face.icon} size={14} className="fg-nv-ic" />
                  <span className="fg-nv-label">{face.label}</span>
                </button>
              ))}
            </nav>
            {!narrow && <span className="sp" />}
            <div className="fg-menuwrap machine-notification-wrap">
              <button type="button" className={narrow ? 'fg-ico' : 'fg-tgl'} aria-label="机器通知" tabIndex={-1}>
                <Icon of="bell" size={narrow ? 13 : 15} className="fg-tgl-ic" />
                {!narrow && <span className="fg-tgl-label">通知</span>}
              </button>
            </div>
            {!narrow && <span className="fg-vr" />}
            <div className="fg-menuwrap">
              <button
                type="button"
                className={`fg-who${narrow ? ' compact' : ''}`}
                aria-label="admin，系统管理员：账户与更多功能"
                tabIndex={-1}
              >
                <span className="fg-who-plate" aria-hidden="true">
                  A
                </span>
                {!narrow && <span className="fg-who-name">admin</span>}
                {!narrow && <Icon of="chevronDown" size={12} className="fg-who-caret" />}
              </button>
            </div>
          </div>
          {!narrow && (
            <div className="fg-crumb">
              <span className="cur">线路</span>
            </div>
          )}
          <div className="fg-view" role="main" aria-label="线路内容">
            {children}
          </div>
        </div>
      </div>
    </div>
  );
}

/** 线路页里只看排版：点链卡、拖动排序、建链都不跳转。 */
function holdNavigation(event: SyntheticEvent) {
  const target = event.target as HTMLElement;
  if (target.closest('.tn-inject')) return;
  if (!target.closest('.chain-card, .new-chain, .order-grip, .chain-section-name')) return;
  event.preventDefault();
  event.stopPropagation();
}

function ChainsView({ count, onEnter }: { count: number; onEnter: () => void }) {
  const host = useRef<HTMLDivElement>(null);
  const [slot, setSlot] = useState<HTMLElement | null>(null);

  useLayoutEffect(() => {
    // 真实 ChainsPane 还没有这个开关：插进它的标题栏，位置即落地后的位置。
    const spacer = host.current?.querySelector<HTMLElement>('.chain-list-panel > header > .sp');
    if (!spacer) return;
    const span = document.createElement('span');
    span.className = 'tn-inject';
    spacer.after(span);
    setSlot(span);
    return () => span.remove();
  }, []);

  return (
    <div ref={host} onClickCapture={holdNavigation} onPointerDownCapture={holdNavigation}>
      <ChainsPane win={CHAINS_WIN} />
      {slot && createPortal(<LabSwitch count={count} onEnter={onEnter} />, slot)}
    </div>
  );
}

/* ══ 两种模式：同一份测试节点数据，切换时只换画面 ══ */

function Mockup({
  scene,
  preset,
  running,
  lab,
  onLab,
  onPromote,
}: {
  scene: Scene;
  preset: LabPreset;
  running: boolean;
  lab: boolean;
  onLab: (lab: boolean) => void;
  onPromote: (node: TestNode) => void;
}) {
  const store = useTestNodes(scene.nodes, running);
  return lab ? (
    <LabStage store={store} scene={scene} preset={preset} onExit={() => onLab(false)} onPromote={onPromote} />
  ) : (
    <Shell>
      <ChainsView count={store.nodes.length} onEnter={() => onLab(true)} />
    </Shell>
  );
}

/* ══ 审阅外壳 ══ */

const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

function App() {
  const [lab, setLab] = useState(params.get('lab') !== '0');
  const [sceneKey, setSceneKey] = useState<SceneKey>(pick(params.get('scene'), SCENE_KEYS, 'typical'));
  const [preset, setPreset] = useState<LabPreset>(
    pick(
      params.get('open'),
      PRESETS.map(item => item.value),
      'none',
    ),
  );
  const [width, setWidth] = useState(Number(params.get('w')) || 0);
  const [mode, setMode] = useState(theme.snapshot());
  const [tone, setTone] = useState<Palette>(palette.snapshot());
  const [running, setRunning] = useState(params.get('run') !== '0');
  const [round, setRound] = useState(0);
  const [flash, setFlash] = useState<string | null>(null);
  const client = useMemo(() => seededClient(), []);
  const bar = useRef<HTMLDivElement>(null);
  const nav = params.get('nav') !== '0';

  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('lab', lab ? '1' : '0');
    next.set('scene', sceneKey);
    next.set('open', preset);
    next.set('w', String(width));
    next.set('theme', mode);
    next.set('palette', tone);
    if (running) next.delete('run');
    else next.set('run', '0');
    history.replaceState(null, '', `?${next}`);
  }, [lab, sceneKey, preset, width, mode, tone, running]);

  useLayoutEffect(() => {
    // 实验台画框 = 视口减去审阅栏；审阅栏会折行，按实际高度算。
    const node = bar.current;
    if (!node) return;
    const observer = new ResizeObserver(([entry]) =>
      document.documentElement.style.setProperty('--mk-bar-h', `${Math.ceil(entry.borderBoxSize[0].blockSize)}px`),
    );
    observer.observe(node);
    return () => observer.disconnect();
  }, []);

  const scene: Scene = SCENES[sceneKey];
  return (
    <QueryClientProvider client={client}>
      <SessionProvider value={SESSION} onIdentityChange={() => undefined}>
        {nav && (
          <div ref={bar} className="mk-bar">
            <div className="mk-bar-row">
              <span className="mk-title">
                线路 · 实验模式<small>MOCKUP</small>
              </span>
              <div className="mk-controls">
                <span className="mk-ctl">
                  <span className="mk-ctl-label">模式</span>
                  <SegmentedControl
                    ariaLabel="模式"
                    value={lab}
                    options={[
                      { value: false, label: '线路' },
                      { value: true, label: '实验模式' },
                    ]}
                    onChange={setLab}
                  />
                </span>
                <label className="mk-ctl">
                  <span className="mk-ctl-label">场景</span>
                  <select value={sceneKey} onChange={event => setSceneKey(event.target.value as SceneKey)}>
                    {SCENE_KEYS.map(key => (
                      <option key={key} value={key}>
                        {SCENES[key].label}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="mk-ctl">
                  <span className="mk-ctl-label">打开</span>
                  <select value={preset} onChange={event => setPreset(event.target.value as LabPreset)}>
                    {PRESETS.map(item => (
                      <option key={item.value} value={item.value}>
                        {item.label}
                      </option>
                    ))}
                  </select>
                </label>
                <span className="mk-ctl">
                  <span className="mk-ctl-label">下发</span>
                  <SegmentedControl
                    ariaLabel="模拟下发"
                    value={running}
                    options={[
                      { value: true, label: '推进' },
                      { value: false, label: '暂停' },
                    ]}
                    onChange={setRunning}
                  />
                </span>
                <button
                  type="button"
                  className="btn mk-reset"
                  onClick={() => {
                    setFlash(null);
                    setRound(current => current + 1);
                  }}
                >
                  重置
                </button>
                <span className="mk-ctl">
                  <span className="mk-ctl-label">宽度</span>
                  <SegmentedControl ariaLabel="画框宽度" value={width} options={WIDTHS} onChange={setWidth} />
                </span>
                <label className="mk-ctl">
                  <span className="mk-ctl-label">色调</span>
                  <select
                    value={tone}
                    onChange={event => {
                      const next = event.target.value as Palette;
                      palette.set(next);
                      setTone(next);
                    }}
                  >
                    {PALETTES.map(item => (
                      <option key={item.key} value={item.key}>
                        {item.name}
                      </option>
                    ))}
                  </select>
                </label>
                <span className="mk-ctl">
                  <span className="mk-ctl-label">主题</span>
                  <SegmentedControl
                    ariaLabel="主题"
                    value={mode}
                    options={[
                      { value: 'dark', label: '暗' },
                      { value: 'light', label: '亮' },
                    ]}
                    onChange={next => {
                      if (theme.snapshot() !== next) theme.toggle();
                      setMode(next);
                    }}
                  />
                </span>
              </div>
            </div>
            <p className="mk-scene-note">{flash ?? scene.note}</p>
          </div>
        )}
        <div className={`mk-stage${lab ? ' lab' : ''}`}>
          <div className={`mk-frame${lab ? ' lab' : ''}`} style={{ width: width ? `min(100%, ${width}px)` : '100%' }}>
            <Fragment key={`${sceneKey}/${preset}/${round}`}>
              <Mockup
                scene={scene}
                preset={preset}
                running={running}
                lab={lab}
                onLab={setLab}
                onPromote={node =>
                  setFlash(`「转为线路」会带着 ${clientName(node)} 的路径打开建链向导（示意，本页不跳转）`)
                }
              />
            </Fragment>
          </div>
        </div>
      </SessionProvider>
    </QueryClientProvider>
  );
}

createRoot(document.getElementById('root')!).render(<App />);
