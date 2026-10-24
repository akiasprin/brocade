// Mockup: the per-line status icon (限速 / 暂停 / 正常) in 用户详情 → 接入 → 用量与额度. Synthetic data only; never
// imported by the app. The production users pane runs on a stubbed Console; each option replaces the quota rows
// with its own rows, portalled into the same grid cell, so the ledger and the rest of the page are the real ones.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/user-quota-status-mockup.html?option=a&scene=all
// Query: option=a|b|c|d|current, scene=all|normal|single|throttled|suspended|pending|paused,
//        role=system-admin|readonly, theme=dark|light, nav=0 hides the review bar, run=0 freezes machine sync.
import { createRoot } from 'react-dom/client';
import { useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type {
  AdminRole,
  ConsoleSnapshot,
  SnapshotIngress,
  UserCycleUsage,
  UserListItem,
  UserTrafficPolicy,
  UserTrafficPolicyView,
} from '../../src/api';
import { theme } from '../../src/forge/theme';
import { userTrafficPolicyKey } from '../../src/panes/user-traffic-policy';
import { UsersPane } from '../../src/panes/users';
import { SessionProvider } from '../../src/session';
import { SegmentedControl } from '../../src/ui/bits';
import type { Win } from '../../src/wm/store';
import {
  MACHINES,
  SCENE_KEYS,
  SCENES,
  exhaustedOf,
  viewOf,
  withdrawnOf,
  type SceneKey,
  type World,
} from './user-quota-status-mockup-data';
import { MockQuotaRows, type Option } from './user-quota-status-mockup-ui';
import '../../src/styles.css';
import './user-quota-status-mockup.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';

type Choice = Option | 'b' | 'current';
const OPTIONS: { value: Choice; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 状态字',
    note: '图标原位换成文字。常态用浅灰写出超额处理（用满后限速 5 Mbps / 用满后停用），细竖线隔开余量；需要注意时换成带色点的状态：限速中（注意色）、已停用（告警色）、服务暂停（空心灰点）。同步中跟在后面，悬停列出待同步的机器。不再需要点开。',
  },
  {
    value: 'b',
    name: 'B 额度刻度',
    note: '超额处理挂在额度条的边界下方，与机器「流量统计」的刻度跑道同一写法。常态在条尾写「用满后限速 5 Mbps」；超出额度时条按用量展开，边界留在额度处，越界部分画斜纹，标记随之移到边界并换成状态色。顶行只留余量。',
  },
  {
    value: 'c',
    name: 'C 状态符号',
    note: '保留图标形式，只在需要注意时出现：注意色乌龟（限速中）、告警色暂停（已停用）、灰色暂停（服务暂停）；常态不画。同步中是符号角上的小环，单独同步时是一个转动的环。点开是两三行的状态说明。',
  },
  {
    value: 'd',
    name: 'D 名称行状态',
    note: '与名册同一规则：需要注意时，状态顶替线路名下的接入链，并与原「系统已停用 N 个接入点」合成一处——限速中 · 2 Mbps、已停用 · 2 个接入点、服务暂停、同步中 · 香港-01。常态与现在一样只写接入链，右侧只留余量。',
  },
  { value: 'current', name: '现状', note: '已落地的 B 额度刻度，使用生产组件与正常数据流程。' },
];
const COMMON_NOTE =
  '四个方案共同的改动：常态不再用绿色图标；限速中的线路额度条、百分比与超出量改用注意色（不再标红）。';

const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

/* ══ 合成后端 ══ */

const tenant = 'preview';
const SELECTED = 'alice';
const POLICY_PATH = `/users/${tenant}/${SELECTED}/traffic-policy`;
const running = params.get('run') !== '0';
const OFFSET = 8 * 3_600_000;
const today = new Date(Date.now() + OFFSET);
const cycleStart = new Date(Date.UTC(today.getUTCFullYear(), today.getUTCMonth(), 1) - OFFSET).toISOString();
const cycleEnd = new Date(Date.UTC(today.getUTCFullYear(), today.getUTCMonth() + 1, 1) - OFFSET).toISOString();

const live = {
  world: SCENES[pick(params.get('scene'), SCENE_KEYS, 'all')].world(),
  client: null as QueryClient | null,
};
const listeners = new Set<() => void>();
let timers: number[] = [];

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
  service_starts_at: null,
  service_expires_at: null,
  ...extra,
});
const usersOf = (world: World) => [
  userItem(SELECTED, { service_state: world.serviceState, service_expires_at: world.expiresAt }),
  userItem('carol', { account_type: 'test' }),
  userItem('dave'),
];

const snapshotOf = (world: World) => {
  const users = usersOf(world);
  return {
    snapshot: {
      revision: 7,
      apps: world.lines.map(item => {
        const withdrawn = new Set(withdrawnOf(item));
        return {
          id: item.id,
          label: item.label,
          chains: item.chains.map(chain => ({ id: chain.id, name: chain.name, subscription_country: chain.country })),
          steps: [],
          fronts: [],
          ingresses: item.chains.map(
            (chain, index) =>
              ({ id: `in-${chain.id}`, chain: chain.id, node: chain.node, port: 8440 + index }) as SnapshotIngress,
          ),
          // 被配额执行器停用的接入点会撤销授权，名称列靠 quotas.suspended_ingresses 认出它们。
          grants: users.flatMap(user =>
            item.chains
              .filter(chain => user.id !== SELECTED || !withdrawn.has(`in-${chain.id}`))
              .map(chain => ({ tenant, user: user.id, ingress: `in-${chain.id}` })),
          ),
        };
      }),
      nodes: MACHINES.map(machine => ({ id: machine.node_id, name: machine.name })),
    },
    node_egress_dns: [],
    redacted: false,
  } as unknown as ConsoleSnapshot;
};
const quotasOf = (world: World) => ({
  quotas: world.lines
    .filter(item => item.quota !== null)
    .map(item => ({
      tenant_id: tenant,
      user_id: SELECTED,
      app_id: item.id,
      limit_bytes: item.quota,
      updated_at: '2026-10-01T00:00:00Z',
      suspended_ingresses: withdrawnOf(item),
    })),
});
const split = (total: number) => {
  const uplink = Math.round(total * 0.03);
  return { uplink_bytes: uplink, downlink_bytes: Math.round(total) - uplink };
};
const cyclesOf = (world: World): UserCycleUsage => {
  const users = usersOf(world);
  return {
    periods: users.map(user => ({ tenant_id: tenant, user_id: user.id, starts_at: cycleStart, ends_at: cycleEnd })),
    views: [
      ...world.lines.map(item => ({
        tenant_id: tenant,
        user_id: SELECTED,
        app_id: item.id,
        ...split(item.used),
        has_gap: false,
      })),
      ...users
        .filter(user => user.id !== SELECTED)
        .map((user, index) => ({
          tenant_id: tenant,
          user_id: user.id,
          app_id: world.lines[0].id,
          ...split((40 + index * 30) * 1024 ** 3),
          has_gap: false,
        })),
    ],
    forwarding: [],
    traffic_policies: world.lines
      .filter(item => item.action)
      .map(item => ({
        tenant_id: tenant,
        user_id: SELECTED,
        kind: 'access' as const,
        app_id: item.id,
        quota_bytes: item.quota === null ? null : String(Math.round(item.quota)),
        used_bytes: String(Math.round(item.used)),
        exhausted: exhaustedOf(item),
        suspended: withdrawnOf(item).length > 0,
        rate_bps: item.action?.kind === 'throttle' ? item.action.rate_bps : null,
      })),
  } as UserCycleUsage;
};
const viewsOf = (world: World): UserTrafficPolicyView[] => [
  ...world.lines.map(item => viewOf(world, item)),
  {
    kind: 'forwarding',
    app_id: null,
    policy: null,
    used_bytes: '0',
    exhausted: false,
    service_available: world.serviceAvailable,
    unsupported_nodes: [],
    pending_nodes: [],
  },
];
const nodesList = { nodes: MACHINES.map(machine => ({ node_id: machine.node_id, name: machine.name })) };

/** 改动整份世界：方案部件读 store，生产部件读查询缓存，两边同时更新。 */
function commit(world: World) {
  live.world = world;
  const client = live.client;
  if (client) {
    client.setQueryData(['snapshot'], snapshotOf(world));
    client.setQueryData(['users'], { users: usersOf(world) });
    client.setQueryData(['quotas'], quotasOf(world));
    client.setQueryData(['user-cycles'], cyclesOf(world));
    client.setQueryData(userTrafficPolicyKey(usersOf(world)[0]), viewsOf(world));
  }
  listeners.forEach(listener => listener());
}

/** 限速策略保存后，相关机器先列为待同步，随后逐台确认。 */
function converge() {
  timers.forEach(timer => clearTimeout(timer));
  timers = [];
  if (!running) return;
  const step = (delay: number) =>
    timers.push(
      window.setTimeout(() => {
        let done = false;
        const lines = live.world.lines.map(item => {
          if (done || item.pending.length === 0) return item;
          done = true;
          return { ...item, pending: item.pending.slice(1) };
        });
        commit({ ...live.world, lines });
      }, delay),
    );
  step(1600);
  step(3400);
  step(5200);
}

globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), location.origin);
  const path = url.pathname;
  const method = init?.method ?? 'GET';
  const world = live.world;
  if (path.startsWith(`${POLICY_PATH}/access/`) && method === 'PUT') {
    await new Promise(resolve => setTimeout(resolve, 300));
    const app = decodeURIComponent(path.slice(`${POLICY_PATH}/access/`.length));
    const body = JSON.parse(String(init?.body)) as UserTrafficPolicy;
    const lines = world.lines.map(item => {
      if (item.id !== app) return item;
      const action = body.exhausted_action;
      const changedRate =
        action.kind === 'throttle' && (item.action?.kind !== 'throttle' || item.action.rate_bps !== action.rate_bps);
      return {
        ...item,
        quota: body.quota_bytes === null ? null : Number(body.quota_bytes),
        action,
        pending:
          action.kind !== 'throttle'
            ? []
            : changedRate
              ? [...new Set(item.chains.map(chain => chain.node))]
              : item.pending,
      };
    });
    commit({ ...world, lines });
    converge();
    return Response.json(viewsOf(live.world));
  }
  if (path === POLICY_PATH) return Response.json(viewsOf(world));
  if (path.endsWith('/traffic-policy')) return Response.json([]);
  if (path === '/users') return Response.json({ users: usersOf(world) });
  if (path === '/usage/user-cycles') return Response.json(cyclesOf(world));
  if (path === '/quotas') return Response.json(quotasOf(world));
  if (path === '/model/snapshot') return Response.json(snapshotOf(world));
  if (path === '/tenants') return Response.json({ tenants: [{ id: tenant, name: '示例' }] });
  if (path === '/nodes/agent-state') return Response.json(nodesList);
  if (path === '/users/presence') return Response.json({ freshness_secs: 120, users: [] });
  if (path.endsWith('/presence-history'))
    return Response.json({ tenant_id: tenant, user_id: SELECTED, retention_days: 30, truncated: false, sources: [] });
  if (path.endsWith('/source-access')) return Response.json({ revision: 0, ips: [], nodes: [] });
  if (path.endsWith('/forwarding'))
    return Response.json({ tenant_id: tenant, user_id: SELECTED, entitlements: [], rules: [] });
  if (path.endsWith('/tcp-pings')) return Response.json([]);
  if (path === '/grant-probes/capability') return Response.json({ available: false, reason: '示例不拨测' });
  return Response.json({ error: `示例未提供 ${path}` }, { status: 404 });
};

function seededClient(world: World, role: AdminRole): QueryClient {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity, refetchOnWindowFocus: false } },
  });
  client.setQueryData(['snapshot'], snapshotOf(world));
  client.setQueryData(['nodes'], nodesList);
  client.setQueryData(['users'], { users: usersOf(world) });
  client.setQueryData(['tenants'], { tenants: [{ id: tenant, name: '示例' }] });
  client.setQueryData(['quotas'], quotasOf(world));
  client.setQueryData(['user-cycles'], cyclesOf(world));
  client.setQueryData(['user-presence', role], { freshness_secs: 120, users: [] });
  client.setQueryData(userTrafficPolicyKey(usersOf(world)[0]), viewsOf(world));
  return client;
}

const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => void listeners.delete(listener);
};
const useWorld = () => useSyncExternalStore(subscribe, () => live.world);

/* ══ 插槽：在生产页面里给方案的线路行留位置 ══ */

/**
 * Keeps an empty `display: contents` element right after `after` inside `parent`, re-inserting it whenever the
 * production pane re-renders that spot, and returns it as a portal target.
 */
function useSlot(enabled: boolean, find: () => { parent: Element; after: Element } | null) {
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
      if (!spot || element.previousSibling === spot.after) return;
      spot.parent.insertBefore(element, spot.after.nextSibling);
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

function Overlay({ option, editable }: { option: Choice; editable: boolean }) {
  const world = useWorld();
  const slot = useSlot(option !== 'current' && option !== 'b', () => {
    const usage = document.querySelector('.user-split-detail .user-usage-card:not(.user-forwarding-usage) .user-usage');
    const legacy = usage?.querySelector(':scope > .user-access-quotas:not(.qs-own)');
    return usage && legacy ? { parent: usage, after: legacy } : null;
  });
  if (!slot || option === 'current' || option === 'b') return null;
  return createPortal(
    <MockQuotaRows option={option} world={world} user={usersOf(world)[0]} editable={editable} />,
    slot,
  );
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

/** The production pane plus the option rows; the rows portal into the pane but need the same query client. */
function Pane({ role, client, children }: { role: AdminRole; client: QueryClient; children: ReactNode }) {
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
        {children}
      </SessionProvider>
    </QueryClientProvider>
  );
}

function App() {
  const [option, setOption] = useState<Choice>(
    pick<Choice>(params.get('option'), ['a', 'b', 'c', 'd', 'current'], 'b'),
  );
  const [sceneKey, setSceneKey] = useState<SceneKey>(pick(params.get('scene'), SCENE_KEYS, 'all'));
  const [role, setRole] = useState<AdminRole>(
    pick(params.get('role'), ['system-admin', 'readonly'] as const, 'system-admin'),
  );
  const [mode, setMode] = useState(theme.snapshot());
  const [generation, setGeneration] = useState(0);
  const nav = params.get('nav') !== '0';
  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('option', option);
    next.set('scene', sceneKey);
    next.set('role', role);
    next.set('theme', mode);
    history.replaceState(null, '', `?${next}`);
  }, [option, sceneKey, role, mode]);
  // 场景、身份或方案变化时整页重挂：生产页面的展开状态与草稿都从头开始。
  const stamp = `${option}-${sceneKey}-${role}-${generation}`;
  const client = useMemo(() => {
    timers.forEach(timer => clearTimeout(timer));
    timers = [];
    live.world = SCENES[sceneKey].world();
    live.client = seededClient(live.world, role);
    return live.client;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stamp]);
  useEffect(() => {
    listeners.forEach(listener => listener());
    if (live.world.lines.some(item => item.pending.length > 0)) converge();
  }, [stamp]);
  const entry = OPTIONS.find(item => item.value === option)!;
  return (
    <>
      {nav && (
        <div className="mk-bar">
          <div className="mk-bar-row">
            <span className="mk-title">
              用户 · 用量与额度状态<small>MOCKUP</small>
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
            <b>{entry.name}</b>
            {entry.note}
            {option !== 'current' && <span className="mk-common">{COMMON_NOTE}</span>}
          </p>
        </div>
      )}
      <div className={`mk-stage${option === 'current' || option === 'b' ? '' : ' mk-hide-legacy'}`}>
        <div className="mk-frame">
          <Pane key={`pane-${stamp}`} role={role} client={client}>
            <Overlay option={option} editable={role === 'system-admin'} />
          </Pane>
        </div>
      </div>
    </>
  );
}

createRoot(document.getElementById('root')!).render(<App />);
