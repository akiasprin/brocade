// Mockup: 解锁检测 in 机器详情 (页签) and 线路详情 (内嵌块 / 页头弹窗). Synthetic data only; never imported by the app.
// The production NodesPane / ChainsPane run on a stubbed Console. Each option replaces the unlock panel in the
// machine tab, or the chain head button, with its own part portalled into the same spot; everything around it is
// the production page. 「重新检测」 runs a simulated job: queued → running → complete in about ten seconds.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/unlock-mockup.html?option=a&page=node&scene=typical
// Query: option=a|b|c|d|current, page=node|chain, entry=inline|dialog, scene=typical|clear|running|queued|never|
//        expired|obsolete|path|old, role=system-admin|readonly, theme=dark|light, nav=0 hides the review bar.
import { createRoot } from 'react-dom/client';
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { AdminRole, NodeAgentStateItem, NodeUnlockProbe } from '../../src/api';
import { theme } from '../../src/forge/theme';
import { ChainsPane } from '../../src/panes/chains';
import { NodesPane } from '../../src/panes/nodes';
import { SessionProvider } from '../../src/session';
import { SegmentedControl } from '../../src/ui/bits';
import type { Win } from '../../src/wm/store';
import {
  APP_ID,
  CHAIN_ID,
  CHAIN_NAME,
  MACHINES,
  REVISION,
  SCENE_KEYS,
  SCENES,
  compiled,
  e2eProbes,
  machineState,
  nowSecs,
  settings,
  snapshot,
  type Scope,
  type SceneKey,
} from './unlock-mockup-data';
import { trafficItem } from './node-list-mockup-data';
import { ChainUnlockDialog, ChainUnlockInline, MachineUnlock, type Option } from './unlock-mockup-ui';
import '../../src/styles.css';
import './unlock-mockup.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';

type Choice = Option | 'current';
type Entry = 'inline' | 'dialog';

const OPTIONS: { value: Choice; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 清单',
    note: '按 AI、流媒体、社区分成三张圆角清单：左侧是服务标志与名称，右侧是地区和结论（符号加短词）。失败原因、客户端 DNS 与上次不同的读数写在名称下方。宽屏三组并排，窄屏上下排列；内嵌时去掉分组，两列排开。',
  },
  {
    value: 'b',
    name: 'B 磁贴',
    note: '每项服务一块磁贴：地区码放大成主读数，结论符号在右上角，同类服务排成一行。状态行前是可用数和九格条，九格与磁贴同序，颜色即结论。',
  },
  {
    value: 'c',
    name: 'C 结论',
    note: '按结论分行：可用、部分可用、地区不支持、访问受限、检测失败，空行不画。每行列出对应服务，地区跟在服务名后，失败行写出原因。最紧凑，内嵌到线路详情只占几行。',
  },
  {
    value: 'd',
    name: 'D 地区',
    note: '按服务回报的地区分卡：卡头是旗帜、地区名和可用数，卡内列出服务与结论，未回报地区的单独一张。线路分流时，每项服务从哪个地区出去一眼可见；同一台机器被不同服务判成不同地区时也会分开。',
  },
  { value: 'current', name: '现状', note: '生产页面，未改动。' },
];
const COMMON_NOTE =
  '共同改动：去掉页签里重复的标题、说明句和底注，页签补开锁图标；时间写相对值，悬停看绝对时间；结论统一为符号加短词，检测失败为灰色，不算不可用；与上次不同的服务标出上次读数（金色）；重新检测、超时或链路失败时保留上次结果并减淡。YouTube 只多一行要点（网页与 Premium 不一致时写网页结论，以及视频 CDN 位置），HTTP 状态、页面配置 GL、CDN 节点与用时在 ⓘ 里。';
const ENTRY_NOTES: Record<Entry, string> = {
  inline: '线路：结果直接放在连通性横幅下面，与它同一种块结构，页头不再有按钮。',
  dialog: '线路：保留页头按钮，按钮上带可用数；弹窗改成订阅弹窗那种标题栏（线路名、关闭），正文与机器页签相同。',
};

const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

/* ══ 合成后端 ══ */

const live: { scene: SceneKey; jobs: Record<Scope, NodeUnlockProbe | null>; client: QueryClient | null } = {
  scene: 'typical',
  jobs: { node: null, chain: null },
  client: null,
};
let timers: number[] = [];

function stopTimers() {
  timers.forEach(timer => clearTimeout(timer));
  timers = [];
}

function refresh(scope: Scope) {
  void live.client?.invalidateQueries({ queryKey: [scope === 'chain' ? 'chain-unlock-probe' : 'node-unlock-probe'] });
}

/** 手动检测：排队 2.5 秒后开始，再过 7 秒完成。上一次完成的结果挪到 previous，与 Console 相同。 */
function startJob(scope: Scope): NodeUnlockProbe {
  const scene = SCENES[live.scene];
  const last = live.jobs[scope];
  const now = nowSecs();
  const completed = last && last.results.length > 0 && last.completed_at ? last : null;
  const base: NodeUnlockProbe = last ?? {
    id: '',
    node_id: scene.machine,
    state: 'queued',
    created_at: now,
    completed_at: null,
    results: [],
    ...(scope === 'chain' ? { app_id: APP_ID, chain_id: CHAIN_ID, protocol: 'VLESS REALITY' } : {}),
  };
  const queued: NodeUnlockProbe = {
    ...base,
    id: `mock-${scope}-${now}`,
    state: 'queued',
    trigger: 'manual',
    created_at: now,
    completed_at: null,
    next_run_at: now + 86_400,
    results: [],
    path_error: null,
    previous_results: completed ? completed.results : (last?.previous_results ?? []),
    previous_completed_at: completed ? completed.completed_at : (last?.previous_completed_at ?? null),
    ...(scope === 'chain' ? { configuration_revision: REVISION } : {}),
  };
  live.jobs[scope] = queued;
  timers.push(
    window.setTimeout(() => {
      if (live.jobs[scope]?.id !== queued.id) return;
      live.jobs[scope] = { ...queued, state: 'running' };
      refresh(scope);
    }, 2_500),
  );
  timers.push(
    window.setTimeout(() => {
      if (live.jobs[scope]?.id !== queued.id) return;
      const done = nowSecs();
      live.jobs[scope] = {
        ...queued,
        state: 'complete',
        completed_at: done,
        next_run_at: done + 86_400,
        results: scene.fresh(scope),
      };
      refresh(scope);
    }, 9_500),
  );
  return queued;
}

const machineOf = (key: SceneKey) => MACHINES.find(machine => machine.id === SCENES[key].machine) ?? MACHINES[0];
const nodesOf = (key: SceneKey): NodeAgentStateItem[] => {
  const scene = SCENES[key];
  return MACHINES.map(machine => machineState(machine, machine.id === scene.machine ? scene.protocol : 48));
};

globalThis.fetch = async (input, init) => {
  const url = new URL(String(input instanceof Request ? input.url : input), location.origin);
  const path = url.pathname;
  const method = (init?.method ?? 'GET').toUpperCase();
  const json = (body: unknown, status = 200) => Response.json(body, { status });
  if (path.endsWith('/unlock-probe')) {
    const scope: Scope = path.startsWith('/apps/') ? 'chain' : 'node';
    await new Promise(resolve => setTimeout(resolve, method === 'POST' ? 280 : 60));
    if (method === 'POST') return json(startJob(scope));
    return json(live.jobs[scope]);
  }
  if (path === '/nodes/agent-state') return json({ nodes: nodesOf(live.scene) });
  if (path === '/node-traffic') return json({ nodes: MACHINES.map(machine => trafficItem(machine, nowSecs())) });
  if (path.startsWith('/usage/node-series')) return json({ nodes: [], since: '', month_start: '' });
  if (path.startsWith('/ping-probe/')) return json({ node_id: machineOf(live.scene).id, targets: [] });
  if (path.startsWith('/probes/e2e')) return json(e2eProbes());
  if (path === '/certs') return json({ groups: [], nodes: [] });
  if (path.startsWith('/deployments')) return json({ deployments: [] });
  if (path === '/model/snapshot') return json(snapshot);
  if (path === '/settings') return json(settings);
  if (path === '/revisions') return json({ current_revision: REVISION, revisions: [] });
  if (path.startsWith('/compile/')) return json(compiled);
  if (path === '/agent-log-policy') return json({ global: { max_mib: 64 }, nodes: [] });
  if (path.startsWith('/apps/') && method === 'POST') return json({ revision_id: REVISION });
  return json({ error: `示例未提供 ${path}` }, 404);
};

/** 换场景时从头开始：停掉模拟检测，按场景重置两份任务和查询缓存。 */
function restart(key: SceneKey): QueryClient {
  stopTimers();
  live.scene = key;
  live.jobs = { node: SCENES[key].job('node'), chain: SCENES[key].job('chain') };
  live.client = seededClient(key);
  return live.client;
}

function seededClient(key: SceneKey): QueryClient {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: 5_000, refetchOnWindowFocus: false } },
  });
  client.setQueryData(['nodes'], { nodes: nodesOf(key) });
  client.setQueryData(['snapshot'], snapshot);
  client.setQueryData(['settings'], settings);
  client.setQueryData(['revisions'], { current_revision: REVISION, revisions: [] });
  client.setQueryData(['compile', REVISION], compiled);
  client.setQueryData(['e2e-probes'], e2eProbes());
  client.setQueryData(['deployments'], { deployments: [] });
  client.setQueryData(['certs'], { groups: [], nodes: [] });
  return client;
}

/* ══ 插槽：在生产页面里给方案部件留位置 ══ */

/**
 * Keeps an empty `display: contents` element right after `after` inside `parent` while `enabled`, re-inserting it
 * whenever the production pane re-renders that spot, and returns it as a portal target.
 */
function useSlot(enabled: boolean, find: () => { parent: Element; after: Element } | null) {
  const [slot] = useState(() => {
    const element = document.createElement('div');
    element.className = 'mk-slot';
    return element;
  });
  const findRef = useRef(find);
  useLayoutEffect(() => {
    findRef.current = find;
  });
  useEffect(() => {
    if (!enabled) return;
    const place = () => {
      const spot = findRef.current();
      if (!spot || slot.previousSibling === spot.after) return;
      spot.parent.insertBefore(slot, spot.after.nextSibling);
    };
    place();
    const observer = new MutationObserver(place);
    observer.observe(document.getElementById('root')!, { childList: true, subtree: true });
    return () => {
      observer.disconnect();
      slot.remove();
    };
  }, [enabled, slot]);
  return enabled ? slot : null;
}

function Overlay({
  option,
  page,
  entry,
  editable,
  node,
}: {
  option: Choice;
  page: Scope;
  entry: Entry;
  editable: boolean;
  node: NodeAgentStateItem;
}) {
  const replaced = option !== 'current';
  const tabSlot = useSlot(replaced && page === 'node', () => {
    const panel = document.querySelector('.nd-tab-panel[aria-labelledby$="-unlock-tab"]');
    const legacy = panel?.querySelector(':scope > .node-unlock');
    return panel && legacy ? { parent: panel, after: legacy } : null;
  });
  const headSlot = useSlot(replaced && page === 'chain' && entry === 'dialog', () => {
    const tools = document.querySelector('.chain-detail-head .nd-tools');
    const legacy = tools?.querySelector(':scope > .btn:not(.ul-head-btn)');
    return tools && legacy ? { parent: tools, after: legacy } : null;
  });
  const inlineSlot = useSlot(replaced && page === 'chain' && entry === 'inline', () => {
    const column = document.querySelector('.chain-config-grid > .col');
    const banner = column?.querySelector(':scope > .blk:not(.ul-blk)');
    return column && banner ? { parent: column, after: banner } : null;
  });
  if (!replaced) return null;
  return (
    <>
      {tabSlot && createPortal(<MachineUnlock option={option} node={node} editable={editable} />, tabSlot)}
      {headSlot &&
        createPortal(
          <ChainUnlockDialog option={option} app={APP_ID} chain={CHAIN_ID} name={CHAIN_NAME} editable={editable} />,
          headSlot,
        )}
      {inlineSlot &&
        createPortal(
          <ChainUnlockInline option={option} app={APP_ID} chain={CHAIN_ID} editable={editable} />,
          inlineSlot,
        )}
    </>
  );
}

/* ══ 页面 ══ */

const winOf = (page: Scope, machine: string): Win => ({
  id: 9100,
  key: page === 'chain' ? 'tab:chains' : 'tab:nodes',
  title: page === 'chain' ? '线路' : '机器',
  x: 0,
  y: 0,
  w: 1200,
  h: 900,
  z: 1,
  min: false,
  home: 'desk',
  data: {
    drill: page === 'chain' ? { p: 'chain', app: APP_ID, chain: CHAIN_ID } : { p: 'node', id: machine, tab: 'unlock' },
  },
});

function Pane({
  role,
  client,
  page,
  machine,
  children,
}: {
  role: AdminRole;
  client: QueryClient;
  page: Scope;
  machine: string;
  children: ReactNode;
}) {
  return (
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          who: { operator_id: 'operator', role, tenant_scope: null, token_prefix: null, masked_assets: false },
          initial: { node_count: MACHINES.length, chain_group_count: [] },
        }}
      >
        <div className={`forge mk-forge${innerWidth < 900 ? ' narrow' : ''}`}>
          <main className="fg-desk mk-desk">
            {page === 'chain' ? (
              <ChainsPane win={winOf(page, machine)} />
            ) : (
              <NodesPane win={winOf(page, machine)} bare />
            )}
          </main>
        </div>
        {children}
      </SessionProvider>
    </QueryClientProvider>
  );
}

function App() {
  const [option, setOption] = useState<Choice>(
    pick<Choice>(params.get('option'), ['a', 'b', 'c', 'd', 'current'], 'a'),
  );
  const [page, setPage] = useState<Scope>(pick<Scope>(params.get('page'), ['node', 'chain'], 'node'));
  const [entry, setEntry] = useState<Entry>(pick<Entry>(params.get('entry'), ['inline', 'dialog'], 'inline'));
  const [sceneKey, setSceneKey] = useState<SceneKey>(pick(params.get('scene'), SCENE_KEYS, 'typical'));
  const [role, setRole] = useState<AdminRole>(
    pick(params.get('role'), ['system-admin', 'readonly'] as const, 'system-admin'),
  );
  const [mode, setMode] = useState(theme.snapshot());
  const [generation, setGeneration] = useState(0);
  const nav = params.get('nav') !== '0';
  // 场景只在支持它的页面出现：失效与客户端不可用只属于线路，Agent 版本只属于机器。
  const scene = SCENES[sceneKey].scopes.includes(page) ? sceneKey : 'typical';
  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('option', option);
    next.set('page', page);
    next.set('entry', entry);
    next.set('scene', scene);
    next.set('role', role);
    next.set('theme', mode);
    history.replaceState(null, '', `?${next}`);
    document.documentElement.dataset.mkOption = option;
  }, [option, page, entry, scene, role, mode]);
  // 方案、页面、场景或身份变化时整页重挂：生产页面与模拟检测都从头开始。
  const stamp = `${option}-${page}-${entry}-${scene}-${role}-${generation}`;
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const client = useMemo(() => restart(scene), [stamp]);
  const machine = SCENES[scene].machine;
  const node = nodesOf(scene).find(item => item.node_id === machine)!;
  const entryInfo = OPTIONS.find(item => item.value === option)!;
  return (
    <>
      {nav && (
        <div className="mk-bar">
          <div className="mk-bar-row">
            <span className="mk-title">
              解锁检测<small>MOCKUP</small>
            </span>
            <div className="mk-controls">
              <label className="mk-ctl">
                <span className="mk-ctl-label">方案</span>
                <SegmentedControl
                  value={option}
                  options={OPTIONS.map(item => ({ value: item.value, label: item.name }))}
                  ariaLabel="方案"
                  onChange={setOption}
                />
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">页面</span>
                <SegmentedControl
                  value={page}
                  options={[
                    { value: 'node' as const, label: '机器' },
                    { value: 'chain' as const, label: '线路' },
                  ]}
                  ariaLabel="页面"
                  onChange={setPage}
                />
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">线路入口</span>
                <SegmentedControl
                  value={entry}
                  options={[
                    { value: 'inline' as const, label: '内嵌' },
                    { value: 'dialog' as const, label: '弹窗' },
                  ]}
                  ariaLabel="线路入口"
                  disabled={page !== 'chain' || option === 'current'}
                  onChange={setEntry}
                />
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">场景</span>
                <select value={scene} onChange={event => setSceneKey(event.target.value as SceneKey)}>
                  {SCENE_KEYS.filter(key => SCENES[key].scopes.includes(page)).map(key => (
                    <option key={key} value={key}>
                      {SCENES[key].label}
                    </option>
                  ))}
                </select>
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">身份</span>
                <SegmentedControl
                  value={role}
                  options={[
                    { value: 'system-admin' as const, label: '管理员' },
                    { value: 'readonly' as const, label: '只读' },
                  ]}
                  ariaLabel="身份"
                  onChange={setRole}
                />
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">主题</span>
                <SegmentedControl
                  value={mode}
                  options={[
                    { value: 'dark' as const, label: '深' },
                    { value: 'light' as const, label: '浅' },
                  ]}
                  ariaLabel="主题"
                  onChange={next => {
                    if (next !== theme.snapshot()) theme.toggle();
                    setMode(theme.snapshot());
                  }}
                />
              </label>
              <button type="button" className="btn" onClick={() => setGeneration(value => value + 1)}>
                重置数据
              </button>
            </div>
          </div>
          <p className="mk-scene-note">
            <b>{entryInfo.name}</b>
            {entryInfo.note}
            {option !== 'current' && (
              <span className="mk-common">
                {COMMON_NOTE}
                {page === 'chain' ? ` ${ENTRY_NOTES[entry]}` : ''}
              </span>
            )}
          </p>
        </div>
      )}
      <Pane key={`pane-${stamp}`} role={role} client={client} page={page} machine={machine}>
        <Overlay option={option} page={page} entry={entry} editable={role === 'system-admin'} node={node} />
      </Pane>
    </>
  );
}

createRoot(document.getElementById('root')!).render(<App />);
