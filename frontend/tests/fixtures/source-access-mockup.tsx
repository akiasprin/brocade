// Mockup: the per-user 来源 IP 封禁 panel in 用户详情 → 接入. Synthetic data only; never imported by the app.
// The production users pane runs on a stubbed Console; each option replaces the folded ban panel and the 在线设备
// card with its own arrangement, portalled into the same spot, so the surrounding page is the real one.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/source-access-mockup.html?option=a&scene=typical
// Query: option=a|b|c|d|current, scene=typical|empty|pending|degraded|many|blind, role=system-admin|readonly,
//        open=none|ban|add|status|list|edit|paste (opens a part on load), run=0 freezes the simulated machines,
//        theme=dark|light, nav=0 hides the review bar.
import { createRoot } from 'react-dom/client';
import { useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type {
  AdminRole,
  ConsoleSnapshot,
  SnapshotApp,
  SnapshotIngress,
  SourceAccessView,
  UserCycleUsage,
  UserListItem,
  UserPresenceList,
} from '../../src/api';
import { theme } from '../../src/forge/theme';
import { UsersPane } from '../../src/panes/users';
import { SessionProvider } from '../../src/session';
import { SegmentedControl } from '../../src/ui/bits';
import type { Win } from '../../src/wm/store';
import {
  CHAINS,
  enforce,
  GiB,
  MACHINES,
  normalizeIp,
  SCENE_KEYS,
  SCENES,
  worldOf,
  type SceneKey,
  type World,
} from './source-access-mockup-data';
import {
  BanBatchCard,
  BanEntry,
  BanListCard,
  PresenceCard,
  type BanApi,
  type OpenTarget,
} from './source-access-mockup-ui';
import '../../src/styles.css';
import './source-access-mockup.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';

type Option = 'a' | 'b' | 'c' | 'd' | 'current';
const OPTIONS: { value: Option; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 并入在线设备',
    note: '封禁是对来源的操作，放回来源所在的卡。在线设备每行加禁行钮（与复制钮同样悬停出现，触屏常显），点开就地确认。已封禁的地址移出时间线，在卡底自成「已封禁」一组，沿用同一套列（最后出现、地区、接入链），行尾解除；组标题右侧是整份名单在各机器的执行情况。手动添加收在组下的「封禁其他 IP」，输入时下方写出该地址的地区与是否在线。原折叠面板去掉。',
  },
  {
    value: 'b',
    name: 'B 独立名单卡',
    note: '名单保留独立卡片，常显、不折叠，排在在线设备之后。每行写地址、地区和最后出现，卡头写数量与执行情况，卡底一行输入框直接添加；没有封禁时只剩卡头。在线设备的行同样可以直接封禁，被封禁的来源在原位标出。',
  },
  {
    value: 'c',
    name: 'C 卡头入口',
    note: '不另占版面：在线设备卡头加一枚「封禁 N」入口，点开弹出面板，输入框在上、名单在下，未生效的机器单列；有机器未生效时入口带状态点。时间线里被封禁的来源在原位标出，行内同样可以直接封禁。',
  },
  {
    value: 'd',
    name: 'D 批量编辑',
    note: '面向成批维护：卡片平时把名单排成紧凑的地址块；「编辑」后整份名单变成文本框，可整段粘贴，逐项校验，下方列出新增、解除与无效项，一次保存。行内封禁与 B 相同。',
  },
  { value: 'current', name: '现状', note: '生产中的折叠面板与在线设备卡，同一份数据。' },
];
const OPEN_TARGETS: OpenTarget[] = ['none', 'ban', 'add', 'status', 'list', 'edit', 'paste'];

const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

/* ══ 合成后端 ══ */

const tenant = 'preview';
const SELECTED = 'alice';
const SOURCE_PATH = `/users/${tenant}/${SELECTED}/source-access`;
const running = params.get('run') !== '0';

const userItem = (id: string, extra: Partial<UserListItem> = {}): UserListItem => ({
  tenant_id: tenant,
  id,
  status: 'active',
  created_at: '2026-01-01T00:00:00Z',
  created_revision: 1,
  uuid: '6f1c2d3e-4b5a-4c6d-8e7f-90a1b2c3d4e5',
  login_enabled: true,
  traffic_accounting_mode: 'single',
  traffic_reset_day: 1,
  service_state: 'active',
  ...extra,
});
const users = [
  userItem(SELECTED, { direct_login_enabled: true }),
  userItem('carol', { account_type: 'test' }),
  userItem('dave'),
  userItem('erin', { status: 'disabled' }),
];

const apps: SnapshotApp[] = [
  {
    id: 'apac',
    label: 'APAC Premium',
    chains: CHAINS.map(chain => ({ id: chain.id, name: chain.name, subscription_country: chain.country })),
    steps: [],
    fronts: [],
    ingresses: CHAINS.map(
      (chain, index) =>
        ({ id: `in-${chain.id}`, chain: chain.id, node: chain.node, port: 8448 + index }) as SnapshotIngress,
    ),
    grants: users.flatMap(user => CHAINS.map(chain => ({ tenant, user: user.id, ingress: `in-${chain.id}` }))),
  } as unknown as SnapshotApp,
];
const snapshot = {
  snapshot: {
    revision: 7,
    apps,
    nodes: MACHINES.map(machine => ({ id: machine.node_id, name: machine.name })),
  },
  node_egress_dns: [],
  redacted: false,
} as unknown as ConsoleSnapshot;
const nodesList = {
  nodes: MACHINES.map(machine => ({
    node_id: machine.node_id,
    name: machine.name,
    public_ipv4_country: machine.country,
  })),
};
const nameOf = (id: string) => MACHINES.find(machine => machine.node_id === id)?.name ?? id;

const NOW = Date.now();
const live = {
  world: worldOf(pick(params.get('scene'), SCENE_KEYS, 'typical'), NOW),
  client: null as QueryClient | null,
  role: 'system-admin' as AdminRole,
};
const listeners = new Set<() => void>();
let timers: number[] = [];

const viewOf = (world: World): SourceAccessView => ({ revision: world.revision, ips: world.bans, nodes: world.nodes });
const presenceOf = (world: World): UserPresenceList => ({
  freshness_secs: 120,
  users:
    world.presence === 'unavailable'
      ? []
      : [
          {
            tenant_id: tenant,
            user_id: SELECTED,
            state: 'complete',
            expected_nodes: MACHINES.length,
            reporting_nodes: MACHINES.length,
            sources: world.online,
          },
        ],
  source_countries: world.online
    .filter(source => world.countries[source.ip])
    .map(source => ({ ip: source.ip, country: world.countries[source.ip] })),
  source_operators: world.online
    .filter(source => world.operators[source.ip])
    .map(source => ({ ip: source.ip, operator: world.operators[source.ip] })),
});
const historyOf = (world: World) => ({
  tenant_id: tenant,
  user_id: SELECTED,
  retention_days: 30,
  truncated: false,
  sources: world.history,
  source_countries: world.history
    .filter(source => world.countries[source.ip])
    .map(source => ({ ip: source.ip, country: world.countries[source.ip] })),
  source_operators: world.history
    .filter(source => world.operators[source.ip])
    .map(source => ({ ip: source.ip, operator: world.operators[source.ip] })),
});

/** 改动整份世界：选项部件读 store，生产部件读查询缓存，两边同时更新。 */
function commit(world: World) {
  live.world = world;
  const client = live.client;
  if (client) {
    client.setQueryData(['source-access', SOURCE_PATH], viewOf(world));
    client.setQueryData(['user-presence', live.role], presenceOf(world));
    void client.invalidateQueries({ queryKey: ['user-presence-history'] });
  }
  listeners.forEach(listener => listener());
}

/** 保存后的机器：可执行的机器先转为待同步，随后逐台确认；确认后关闭命中来源在该机器上的会话。 */
function converge() {
  timers.forEach(timer => clearTimeout(timer));
  timers = [];
  if (!running) return;
  const step = (delay: number, count: number) =>
    timers.push(
      window.setTimeout(() => {
        let left = count;
        const nodes = live.world.nodes.map(node => {
          if (node.phase !== 'pending' || left === 0) return node;
          left -= 1;
          return { ...node, phase: 'applied' as const };
        });
        commit(enforce({ ...live.world, nodes }, Date.now()));
      }, delay),
    );
  step(900, 1);
  step(2100, Infinity);
}

globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), location.origin);
  const path = url.pathname;
  const method = init?.method ?? 'GET';
  const world = live.world;
  if (path === SOURCE_PATH && method === 'PUT') {
    await new Promise(resolve => setTimeout(resolve, 350));
    const body = JSON.parse(String(init?.body)) as { revision: number; ips: string[] };
    if (body.revision !== live.world.revision)
      return new Response('封禁名单已被其他操作修改，请刷新后重试', { status: 409 });
    const ips = body.ips.map(ip => normalizeIp(ip));
    if (ips.some(ip => ip === null))
      return new Response('请输入有效的 IPv4 或 IPv6 地址（不含端口或网段）', { status: 400 });
    const bans = [...new Set(ips as string[])].sort();
    const changed = bans.join() !== live.world.bans.join();
    const nodes = changed
      ? live.world.nodes.map(node =>
          node.phase === 'applied' || node.phase === 'pending' ? { ...node, phase: 'pending' as const } : node,
        )
      : live.world.nodes;
    commit({ ...live.world, bans, nodes, revision: changed ? live.world.revision + 1 : live.world.revision });
    if (changed) converge();
    return Response.json(viewOf(live.world));
  }
  if (path === SOURCE_PATH) return Response.json(viewOf(world));
  if (path.endsWith('/source-access')) return Response.json({ revision: 0, ips: [], nodes: [] });
  if (path === '/users/presence') return Response.json(presenceOf(world));
  if (path.endsWith(`/${SELECTED}/presence-history`)) return Response.json(historyOf(world));
  if (path.endsWith('/presence-history'))
    return Response.json({ tenant_id: tenant, user_id: '', retention_days: 30, truncated: false, sources: [] });
  if (path === '/users') return Response.json({ users });
  if (path === '/usage/user-cycles') return Response.json(cycles);
  if (path === '/quotas') return Response.json({ quotas: QUOTAS });
  if (path === '/model/snapshot') return Response.json(snapshot);
  if (path === '/tenants') return Response.json({ tenants: [{ id: tenant, name: '示例' }] });
  if (path === '/nodes/agent-state') return Response.json(nodesList);
  if (path.endsWith('/traffic-policy')) return Response.json([trafficPolicy]);
  if (path.endsWith('/forwarding'))
    return Response.json({ tenant_id: tenant, user_id: SELECTED, entitlements: [], rules: [] });
  if (path.endsWith('/tcp-pings')) return Response.json([]);
  if (path === '/grant-probes/capability') return Response.json({ available: false, reason: '示例不拨测' });
  return Response.json({ error: `示例未提供 ${path}` }, { status: 404 });
};

/** 选项部件的保存：与生产面板走同一个桩接口，冲突与校验也一样。 */
async function save(ips: string[]) {
  const response = await fetch(SOURCE_PATH, {
    method: 'PUT',
    body: JSON.stringify({ revision: live.world.revision, ips }),
  });
  if (!response.ok) throw new Error(await response.text());
}

const QUOTAS = [
  {
    tenant_id: tenant,
    user_id: SELECTED,
    app_id: 'apac',
    limit_bytes: 500 * GiB,
    updated_at: '2026-09-01T00:00:00Z',
    suspended_ingresses: [],
  },
];
const trafficPolicy = {
  kind: 'access',
  app_id: 'apac',
  policy: { quota_bytes: String(500 * GiB), exhausted_action: { kind: 'suspend' } },
  used_bytes: String(121 * GiB),
  exhausted: false,
  service_available: true,
  unsupported_nodes: [],
};
const cycleStart = new Date(NOW);
cycleStart.setDate(1);
cycleStart.setHours(0, 0, 0, 0);
const cycleEnd = new Date(cycleStart);
cycleEnd.setMonth(cycleEnd.getMonth() + 1);
const cycles: UserCycleUsage = {
  periods: users.map(user => ({
    tenant_id: tenant,
    user_id: user.id,
    starts_at: cycleStart.toISOString(),
    ends_at: cycleEnd.toISOString(),
  })),
  views: users.map((user, index) => ({
    tenant_id: tenant,
    user_id: user.id,
    app_id: 'apac',
    uplink_bytes: GiB,
    downlink_bytes: (120 + index * 40) * GiB,
    has_gap: false,
  })),
  forwarding: [],
};

function seededClient(role: AdminRole): QueryClient {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity, refetchOnWindowFocus: false } },
  });
  client.setQueryData(['snapshot'], snapshot);
  client.setQueryData(['nodes'], nodesList);
  client.setQueryData(['users'], { users });
  client.setQueryData(['tenants'], { tenants: [{ id: tenant, name: '示例' }] });
  client.setQueryData(['quotas'], { quotas: QUOTAS });
  client.setQueryData(['user-cycles'], cycles);
  client.setQueryData(['user-presence', role], presenceOf(live.world));
  client.setQueryData(['source-access', SOURCE_PATH], viewOf(live.world));
  return client;
}

const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => void listeners.delete(listener);
};
const useWorld = () => useSyncExternalStore(subscribe, () => live.world);

/* ══ 插槽：在生产页面里给改版部件留位置 ══ */

/**
 * Keeps an empty `display: contents` element right after `after` inside `parent`, re-inserting it whenever the
 * production pane re-renders that spot, and returns it as a portal target.
 */
function useSlot(enabled: boolean, find: () => { parent: Element; after: Element | null } | null, deps: unknown[]) {
  const [slot, setSlot] = useState<HTMLElement | null>(null);
  const findRef = useRef(find);
  useLayoutEffect(() => {
    findRef.current = find;
  });
  useEffect(() => {
    if (!enabled) {
      setSlot(null);
      return;
    }
    const element = document.createElement('div');
    element.className = 'mk-slot';
    const place = () => {
      const spot = findRef.current();
      if (!spot) return;
      const before = spot.after ? spot.after.nextSibling : spot.parent.firstChild;
      if (element.parentNode === spot.parent && (before === element || element.previousSibling === spot.after)) return;
      spot.parent.insertBefore(element, before);
    };
    place();
    setSlot(element);
    const observer = new MutationObserver(place);
    observer.observe(document.getElementById('root')!, { childList: true, subtree: true });
    return () => {
      observer.disconnect();
      element.remove();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled, ...deps]);
  return slot;
}

const accessPanel = () =>
  document.querySelector('.user-split-detail [role="tabpanel"][id$="-access-panel"]') ??
  document.querySelector('.user-split-detail .user-dbody');

/* ══ 各方案挂到生产页面上 ══ */

function Overlay({ option, editable, open }: { option: Option; editable: boolean; open: OpenTarget }) {
  const world = useWorld();
  const slot = useSlot(
    option !== 'current',
    () => {
      const panel = accessPanel();
      const usage = panel?.querySelector(':scope > .user-usage-card');
      return panel && usage ? { parent: panel, after: usage } : null;
    },
    [option],
  );
  if (!slot) return null;
  const api: BanApi = { world, editable, apps, nameOf, save };
  let parts: ReactNode = null;
  if (option === 'a') parts = <PresenceCard api={api} mode="group" open={open} />;
  else if (option === 'b')
    parts = (
      <>
        <PresenceCard api={api} mode="tags" open={open} />
        <BanListCard api={api} open={open} />
      </>
    );
  else if (option === 'c')
    parts = <PresenceCard api={api} mode="tags" open={open} headerExtra={<BanEntry api={api} open={open} />} />;
  else if (option === 'd')
    parts = (
      <>
        <PresenceCard api={api} mode="tags" open={open} />
        <BanBatchCard api={api} open={open} />
      </>
    );
  return createPortal(parts, slot);
}

/* ══ 页面 ══ */

const win: Win = {
  id: 1,
  key: 'tab:users',
  title: '用户',
  x: 0,
  y: 0,
  w: 1200,
  h: 900,
  z: 1,
  min: false,
  home: 'desk',
  data: { drill: { p: 'user', id: SELECTED } },
};

function Pane({ role, client }: { role: AdminRole; client: QueryClient }) {
  return (
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          who: {
            operator_id: 'operator',
            role,
            tenant_scope: null,
            token_prefix: null,
            masked_assets: false,
          },
          initial: { node_count: MACHINES.length, chain_group_count: [] },
        }}
      >
        <UsersPane win={win} />
      </SessionProvider>
    </QueryClientProvider>
  );
}

function App() {
  const [option, setOption] = useState<Option>(
    pick<Option>(params.get('option'), ['a', 'b', 'c', 'd', 'current'], 'a'),
  );
  const [sceneKey, setSceneKey] = useState<SceneKey>(pick(params.get('scene'), SCENE_KEYS, 'typical'));
  const [role, setRole] = useState<AdminRole>(
    pick(params.get('role'), ['system-admin', 'readonly'] as const, 'system-admin'),
  );
  const [mode, setMode] = useState(theme.snapshot());
  const [generation, setGeneration] = useState(0);
  const nav = params.get('nav') !== '0';
  const open = generation === 0 ? pick(params.get('open'), OPEN_TARGETS, 'none') : 'none';
  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('option', option);
    next.set('scene', sceneKey);
    next.set('role', role);
    next.set('theme', mode);
    if (generation > 0) next.delete('open');
    history.replaceState(null, '', `?${next}`);
  }, [option, sceneKey, role, mode, generation]);
  // 场景、身份或方案变化时整页重挂：生产页面的展开状态与草稿都从头开始。
  const stamp = `${option}-${sceneKey}-${role}-${generation}`;
  const client = useMemo(() => {
    timers.forEach(timer => clearTimeout(timer));
    timers = [];
    live.world = worldOf(sceneKey, Date.now());
    live.role = role;
    live.client = seededClient(role);
    return live.client;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stamp]);
  useEffect(() => {
    listeners.forEach(listener => listener());
  }, [stamp]);
  const entry = OPTIONS.find(item => item.value === option)!;
  return (
    <>
      {nav && (
        <div className="mk-bar">
          <div className="mk-bar-row">
            <span className="mk-title">
              用户 · 来源 IP 封禁<small>MOCKUP</small>
            </span>
            <div className="mk-controls">
              <label className="mk-ctl">
                <span className="mk-ctl-label">方案</span>
                <SegmentedControl
                  value={option}
                  options={OPTIONS.map(item => ({ value: item.value, label: item.name }))}
                  ariaLabel="方案"
                  onChange={next => {
                    setOption(next);
                    setGeneration(value => value + 1);
                  }}
                />
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">场景</span>
                <select
                  value={sceneKey}
                  onChange={event => {
                    setSceneKey(event.target.value as SceneKey);
                    setGeneration(value => value + 1);
                  }}
                >
                  {SCENE_KEYS.map(key => (
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
                  onChange={next => {
                    setRole(next);
                    setGeneration(value => value + 1);
                  }}
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
            <b>{entry.name}</b>
            {entry.note}
          </p>
        </div>
      )}
      <div className={`mk-stage${option === 'current' ? '' : ' mk-hide-legacy'}`}>
        <div className="mk-frame">
          <Pane key={`pane-${stamp}`} role={role} client={client} />
          <Overlay key={`overlay-${stamp}`} option={option} editable={role === 'system-admin'} open={open} />
        </div>
      </div>
    </>
  );
}

createRoot(document.getElementById('root')!).render(<App />);
