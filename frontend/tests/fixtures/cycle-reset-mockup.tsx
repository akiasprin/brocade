// Mockup: where 用量重置（本期用量归零）lives in 用户详情. Synthetic data only; never imported by the app.
// The production users pane runs on a stubbed Console. Each option hides the production button and portals its own
// entry into the real page; every entry opens the production confirmation by clicking that hidden button, so the
// dialog and the zeroed readings afterwards are the real ones.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/cycle-reset-mockup.html?option=a
// Query: option=a|b|c|d|current, tab=access|forwarding, role=system-admin|readonly, theme=dark|light,
//        open=1 opens the entry on load (A/C/现状: 确认框, B: 账期面板, D: 更多菜单), nav=0 hides the review bar.
import { createRoot } from 'react-dom/client';
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type {
  AdminRole,
  ConsoleSnapshot,
  ForwardingOverview,
  SnapshotIngress,
  UserCycleUsage,
  UserListItem,
  UserTrafficPolicyView,
} from '../../src/api';
import { theme } from '../../src/forge/theme';
import { UsersPane } from '../../src/panes/users';
import { SessionProvider } from '../../src/session';
import { SegmentedControl } from '../../src/ui/bits';
import { Icon } from '../../src/ui/icons';
import type { Win } from '../../src/wm/store';
import '../../src/styles.css';
import './cycle-reset-mockup.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';

type Option = 'a' | 'b' | 'c' | 'd' | 'current';
type Tab = 'access' | 'forwarding';
const OPTIONS: { value: Option; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 读数角标',
    note: '「本期接入」「本期转发」标题行右端放文字按钮「归零」，紧挨它要清零的读数，不占新行；读数栏到「有效期」为止。窄卡在读数栏右上角。两个页签各一枚，点哪一枚都同时归零接入和转发。',
  },
  {
    value: 'b',
    name: 'B 账期面板',
    note: '卡片上不放入口。点「重置日 / 有效期」打开的账期面板左下角放「本期用量归零…」，取消、保存仍在右下，同 macOS 表单左下角的辅助操作。接入、转发共用这一处。',
  },
  {
    value: 'c',
    name: 'C 标题栏',
    note: '放进「用量与额度」标题栏，跟在「N 条线路 / N 个入口」之后的小按钮。两个页签的卡各一枚。',
  },
  {
    value: 'd',
    name: 'D 更多菜单',
    note: '归零作用于整个用户（接入和转发一起），收进页头「更多」菜单，自成一组，副标题写明范围；卡片上不放入口。与「重置登录密码」「更换 UUID」等低频操作同处。',
  },
  { value: 'current', name: '现状', note: '生产：读数栏底部右对齐的独立按钮。' },
];

const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

/* ══ 合成后端 ══ */

const GiB = 1024 ** 3;
const OFFSET = 8 * 3_600_000;
const NOW = Date.now();
const tenant = 'preview';
const SELECTED = 'alice';
const userPath = `/users/${tenant}/${SELECTED}`;
const iso = (ms: number) => new Date(ms).toISOString().replace('.000Z', 'Z');

/** 每月 1 日（UTC+8）为界的本期，与默认重置日一致；倒计时跟着真实时钟走。 */
const PERIOD = (() => {
  const local = new Date(NOW + OFFSET);
  const year = local.getUTCFullYear();
  const month = local.getUTCMonth();
  return { starts_at: iso(Date.UTC(year, month, 1) - OFFSET), ends_at: iso(Date.UTC(year, month + 1, 1) - OFFSET) };
})();

interface Line {
  id: string;
  label: string;
  chains: [id: string, country: string][];
  /** 本期用量：归零前、归零后新产生的。 */
  used: [number, number];
  limit: number;
  action: 'throttle' | 'suspend';
}
// APAC Standard 已超额被限速：归零最常见的起因。
const LINES: Line[] = [
  {
    id: 'apac-premium',
    label: 'APAC Premium',
    chains: [
      ['hk-01', 'HK'],
      ['jp-01', 'JP'],
      ['tw-01', 'TW'],
    ],
    used: [820 * GiB, 0.31 * GiB],
    limit: 1024 * GiB,
    action: 'throttle',
  },
  {
    id: 'apac-standard',
    label: 'APAC Standard',
    chains: [
      ['hk-02', 'HK'],
      ['sg-01', 'SG'],
    ],
    used: [412 * GiB, 0.04 * GiB],
    limit: 400 * GiB,
    action: 'throttle',
  },
  {
    id: 'noram-premium',
    label: 'NORAM Premium',
    chains: [['us-01', 'US']],
    used: [262 * GiB, 0],
    limit: 500 * GiB,
    action: 'suspend',
  },
  {
    id: 'noram-standard',
    label: 'NORAM Standard',
    chains: [
      ['us-02', 'US'],
      ['ca-01', 'CA'],
    ],
    used: [165 * GiB, 0],
    limit: 300 * GiB,
    action: 'throttle',
  },
];
const UP_SHARE = 0.114;
const RULES = [
  { id: 11, name: 'ssh-relay', port: 20022, target: '10.0.0.5', targetPort: 22, used: [12.6 * GiB, 0.02 * GiB] },
  { id: 12, name: 'game', port: 20777, target: 'play.example.net', targetPort: 7777, used: [5.4 * GiB, 0] },
] as const;

const live = { reset: null as { id: number; at: string } | null };
const usedOf = (pair: readonly [number, number]) => Math.round(pair[live.reset ? 1 : 0]);
const exhaustedOf = (line: Line) => usedOf(line.used) >= line.limit;

const person = (id: string, extra: Partial<UserListItem> = {}): UserListItem => ({
  tenant_id: tenant,
  id,
  status: 'active',
  created_at: '2026-01-01T00:00:00Z',
  created_revision: 1,
  uuid: '1ec1d194-7b2a-4c6d-8e7f-90a1b2c32eaa',
  login_enabled: true,
  traffic_accounting_mode: 'single',
  traffic_reset_day: 1,
  service_starts_at: null,
  service_expires_at: null,
  service_state: 'active',
  ...extra,
});
const USERS = [
  person(SELECTED, { direct_login_enabled: true }),
  person('carol', { account_type: 'test' }),
  person('dave'),
  person('erin', { status: 'disabled' }),
];
const OTHERS: [string, number][] = [
  ['carol', 161 * GiB],
  ['dave', 38 * GiB],
];

let port = 8448;
const snapshot = {
  snapshot: {
    revision: 7,
    apps: LINES.map(line => ({
      id: line.id,
      label: line.label,
      chains: line.chains.map(([id, country]) => ({ id, name: id.toUpperCase(), subscription_country: country })),
      steps: [],
      fronts: [],
      ingresses: line.chains.map(
        ([chain]) => ({ id: `in-${chain}`, chain, node: 'n1', port: port++ }) as SnapshotIngress,
      ),
      grants: USERS.filter(user => user.id === SELECTED || line.id === 'apac-premium').flatMap(user =>
        line.chains.map(([chain]) => ({ tenant, user: user.id, ingress: `in-${chain}` })),
      ),
    })),
    nodes: [{ id: 'n1', name: '示例节点' }],
  },
  node_egress_dns: [],
  redacted: false,
} as unknown as ConsoleSnapshot;
const NODES = { nodes: [{ node_id: 'n1', name: '示例节点' }] };
const TENANTS = { tenants: [{ id: tenant, name: '示例' }] };

const quotas = () => ({
  quotas: LINES.map(line => ({
    tenant_id: tenant,
    user_id: SELECTED,
    app_id: line.id,
    limit_bytes: line.limit,
    updated_at: '2026-09-01T00:00:00Z',
    suspended_ingresses: [],
  })),
});

const policies = (): UserTrafficPolicyView[] =>
  LINES.map(line => ({
    kind: 'access',
    app_id: line.id,
    policy: {
      quota_bytes: String(line.limit),
      exhausted_action: line.action === 'throttle' ? { kind: 'throttle', rate_bps: 5_000_000 } : { kind: 'suspend' },
    },
    used_bytes: String(usedOf(line.used)),
    exhausted: exhaustedOf(line),
    service_available: true,
    unsupported_nodes: [],
    pending_nodes: [],
  }));

const split = (used: number, up: number) => ({
  uplink_bytes: Math.round(used * up),
  downlink_bytes: used - Math.round(used * up),
});
const forwardingUsed = () => RULES.reduce((sum, rule) => sum + usedOf(rule.used), 0);

const cycles = (): UserCycleUsage => ({
  periods: USERS.map(user => ({
    tenant_id: tenant,
    user_id: user.id,
    ...PERIOD,
    reset_id: user.id === SELECTED ? (live.reset?.id ?? null) : null,
    reset_at: user.id === SELECTED ? (live.reset?.at ?? null) : null,
  })),
  traffic_policies: LINES.map(line => ({
    tenant_id: tenant,
    user_id: SELECTED,
    kind: 'access',
    app_id: line.id,
    quota_bytes: String(line.limit),
    used_bytes: String(usedOf(line.used)),
    exhausted: exhaustedOf(line),
    suspended: exhaustedOf(line) && line.action === 'suspend',
    rate_bps: line.action === 'throttle' ? 5_000_000 : null,
  })),
  views: [
    ...LINES.map(line => ({
      tenant_id: tenant,
      user_id: SELECTED,
      app_id: line.id,
      ...split(usedOf(line.used), UP_SHARE),
      has_gap: false,
    })),
    ...OTHERS.map(([user, used]) => ({
      tenant_id: tenant,
      user_id: user,
      app_id: 'apac-premium',
      ...split(used, 0.05),
      has_gap: false,
    })),
  ],
  forwarding: RULES.map(rule => ({
    tenant_id: tenant,
    user_id: SELECTED,
    node_id: 'n1',
    rule_id: rule.id,
    rule_name: rule.name,
    ...split(usedOf(rule.used), 0.4),
    has_gap: false,
  })),
});

const overview = (): ForwardingOverview => ({
  tenant_id: tenant,
  user_id: SELECTED,
  entitlements: [
    {
      node_id: 'n1',
      node_name: '示例节点',
      public_ipv4: '203.0.113.20',
      public_ipv6: null,
      enabled: true,
      tcp_allowed: true,
      udp_allowed: true,
      listen_port_start: 20000,
      listen_port_end: 20999,
      max_rules: 10,
      allow_private_targets: true,
      rule_count: RULES.length,
      desired_generation: 4,
      applied_generation: 4,
      reconcile_status: 'converged',
      last_error: null,
      reported_at: new Date(NOW - 20_000).toISOString(),
      cycle_limit_bytes: 50 * GiB,
      cycle_used_bytes: forwardingUsed(),
      quota_suspended: false,
    },
  ],
  rules: RULES.map(rule => ({
    id: rule.id,
    tenant_id: tenant,
    user_id: SELECTED,
    node_id: 'n1',
    name: rule.name,
    protocol: 'tcp',
    listen_port: rule.port,
    target_host: rule.target,
    target_port: rule.targetPort,
    enabled: true,
    runtime_status: 'active',
    runtime_error: null,
    ...split(rule.used[0], 0.4),
    created_at: '2026-09-01T00:00:00Z',
    updated_at: '2026-09-01T00:00:00Z',
  })),
});

const delay = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));

globalThis.fetch = async (input, init) => {
  const path = new URL(String(input), location.origin).pathname;
  const method = init?.method ?? 'GET';
  if (path === `${userPath}/cycle-usage/reset` && method === 'POST') {
    await delay(400);
    live.reset = { id: (live.reset?.id ?? 0) + 1, at: new Date().toISOString() };
    return Response.json({ reset_id: live.reset.id, reset_at: live.reset.at, duplicate: false });
  }
  if (path === `${userPath}/service-policy/preview`)
    return Response.json({
      ...PERIOD,
      access_used_bytes: LINES.reduce((sum, line) => sum + usedOf(line.used), 0),
      forwarding_used_bytes: forwardingUsed(),
      service_state: 'active',
    });
  if (path === `${userPath}/service-policy` && method === 'PUT') {
    await delay(300);
    return Response.json(USERS[0]);
  }
  if (path === `${userPath}/traffic-policy`) return Response.json(policies());
  if (path === `${userPath}/forwarding`) return Response.json(overview());
  if (path === `${userPath}/source-access`)
    return Response.json({
      revision: 1,
      ips: [],
      nodes: [{ node_id: 'n1', phase: 'applied' }],
      source_countries: [],
      source_operators: [],
    });
  if (path.endsWith('/presence-history'))
    return Response.json({ tenant_id: tenant, user_id: SELECTED, retention_days: 30, truncated: false, sources: [] });
  if (path === '/users') return Response.json({ users: USERS });
  if (path === '/usage/user-cycles') return Response.json(cycles());
  if (path === '/quotas') return Response.json(quotas());
  if (path === '/model/snapshot') return Response.json(snapshot);
  if (path === '/users/presence') return Response.json({ freshness_secs: 120, users: [] });
  if (path === '/tenants') return Response.json(TENANTS);
  if (path === '/nodes/agent-state') return Response.json(NODES);
  if (path.endsWith('/tcp-pings')) return Response.json([]);
  if (path === '/grant-probes/capability') return Response.json({ available: false, reason: '示例不拨测' });
  return Response.json({ error: `示例未提供 ${path}` }, { status: 404 });
};

function seededClient(): QueryClient {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity, refetchOnWindowFocus: false } },
  });
  client.setQueryData(['snapshot'], snapshot);
  client.setQueryData(['nodes'], NODES);
  client.setQueryData(['users'], { users: USERS });
  client.setQueryData(['tenants'], TENANTS);
  client.setQueryData(['quotas'], quotas());
  client.setQueryData(['user-cycles'], cycles());
  client.setQueryData(['forwarding', tenant, SELECTED], overview());
  return client;
}

/* ══ 插槽：在生产页面里留一个空位给改版入口 ══ */

/**
 * Keeps an empty `display: contents` element right after `after` inside `parent` (or first when `after` is null),
 * re-inserting it whenever the production pane re-renders that spot, and returns it as a portal target.
 * Watches the whole body: the 账期面板 is a dialog layer outside #root.
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
    element.className = 'crm-slot';
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
    observer.observe(document.body, { childList: true, subtree: true });
    return () => {
      observer.disconnect();
      element.remove();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled, ...deps]);
  return slot;
}

const detail = () => document.querySelector('.user-split-detail');
const panel = (tab: Tab) => detail()?.querySelector(`[role="tabpanel"][id$="-${tab}-panel"]`);
const within = (parent: Element | null | undefined, after: string) =>
  parent ? { parent, after: parent.querySelector(`:scope > ${after}`) } : null;

/** 打开生产的归零确认框：点隐藏起来的那枚生产按钮。 */
function confirmReset() {
  detail()?.querySelector<HTMLButtonElement>('[role="tabpanel"]:not([hidden]) .user-cycle-reset-action')?.click();
}

/** 各方案的入口打开后是什么：A/C/现状 打开确认框，B 打开账期面板，D 打开更多菜单。 */
function openEntry(option: Option) {
  if (option === 'b') {
    detail()?.querySelector<HTMLButtonElement>('[role="tabpanel"]:not([hidden]) button.user-service-terms')?.click();
  } else if (option === 'd') detail()?.querySelector<HTMLButtonElement>('.user-dact-more')?.click();
  else confirmReset();
}

/* ══ 各方案挂到生产页面上 ══ */

function Overlay({ option, editable }: { option: Option; editable: boolean }) {
  const on = (value: Option) => option === value && editable;
  const ledger = (tab: Tab) => () => {
    const element = panel(tab)?.querySelector('.user-usage-ledger');
    return element ? { parent: element, after: null } : null;
  };
  const header = (tab: Tab) => () => within(panel(tab)?.querySelector('.user-usage-card > header'), '.rt');
  const accessCorner = useSlot(on('a'), ledger('access'), [option, editable]);
  const forwardingCorner = useSlot(on('a'), ledger('forwarding'), [option, editable]);
  const popoverFooter = useSlot(
    on('b'),
    () => {
      const footer = document.querySelector('.user-service-pop > footer');
      return footer ? { parent: footer, after: null } : null;
    },
    [option, editable],
  );
  const accessHeader = useSlot(on('c'), header('access'), [option, editable]);
  const forwardingHeader = useSlot(on('c'), header('forwarding'), [option, editable]);
  const menu = useSlot(
    on('d'),
    () => {
      const element = detail()?.querySelector('.user-action-menu');
      if (!element) return null;
      const rules = element.querySelectorAll(':scope > hr');
      return { parent: element, after: rules[rules.length - 1] ?? null };
    },
    [option, editable],
  );

  const corner = (
    <button type="button" className="crm-corner" onClick={confirmReset}>
      归零
    </button>
  );
  const headerButton = (
    <button type="button" className="btn sm crm-head" onClick={confirmReset}>
      <Icon of="refresh" size={12} />
      用量重置
    </button>
  );
  const parts: ReactNode[] = [];
  if (accessCorner) parts.push(createPortal(corner, accessCorner, 'a-access'));
  if (forwardingCorner) parts.push(createPortal(corner, forwardingCorner, 'a-forwarding'));
  if (popoverFooter)
    parts.push(
      createPortal(
        <button type="button" className="btn crm-pop" onClick={confirmReset}>
          本期用量归零…
        </button>,
        popoverFooter,
        'b',
      ),
    );
  if (accessHeader) parts.push(createPortal(headerButton, accessHeader, 'c-access'));
  if (forwardingHeader) parts.push(createPortal(headerButton, forwardingHeader, 'c-forwarding'));
  if (menu)
    parts.push(
      createPortal(
        <>
          <button role="menuitem" type="button" onClick={confirmReset}>
            <Icon of="refresh" size={14} className="user-action-menu-icon" />
            <span>
              本期用量归零
              <small>接入和转发从现在重新累计</small>
            </span>
          </button>
          <hr />
        </>,
        menu,
        'd',
      ),
    );
  return <>{parts}</>;
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
          who: { operator_id: 'operator', role, tenant_scope: null, token_prefix: null, masked_assets: false },
          initial: { node_count: 1, chain_group_count: [] },
        }}
      >
        <UsersPane win={win} />
      </SessionProvider>
    </QueryClientProvider>
  );
}

/** Retries `step` once per frame until it reports done, for at most two seconds. */
function untilReady(step: () => boolean): () => void {
  let frame = 0;
  let tries = 0;
  const run = () => {
    if (!step() && tries++ < 120) frame = requestAnimationFrame(run);
  };
  run();
  return () => cancelAnimationFrame(frame);
}

function App() {
  const [option, setOption] = useState<Option>(
    pick(params.get('option'), ['a', 'b', 'c', 'd', 'current'] as const, 'a'),
  );
  const [tab, setTab] = useState<Tab>(pick(params.get('tab'), ['access', 'forwarding'] as const, 'access'));
  const [role, setRole] = useState<AdminRole>(
    pick(params.get('role'), ['system-admin', 'readonly'] as const, 'system-admin'),
  );
  const [mode, setMode] = useState(theme.snapshot());
  const [generation, setGeneration] = useState(0);
  const nav = params.get('nav') !== '0';
  const openOnLoad = useRef(params.get('open') === '1');
  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('option', option);
    next.set('tab', tab);
    next.set('role', role);
    next.set('theme', mode);
    next.delete('open');
    history.replaceState(null, '', `?${next}`);
  }, [option, tab, role, mode]);
  // 方案、身份变化或重置数据时整页重挂：菜单、面板和页签都从头开始。
  const stamp = `${option}-${role}-${generation}`;
  const client = useMemo(() => {
    live.reset = null;
    return seededClient();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stamp]);
  useEffect(
    () =>
      untilReady(() => {
        const button = detail()?.querySelector<HTMLButtonElement>(`.user-detail-tabs button[id$="-${tab}-tab"]`);
        if (!button) return false;
        if (button.getAttribute('aria-selected') !== 'true') button.click();
        return true;
      }),
    [tab, stamp],
  );
  useEffect(() => {
    if (!openOnLoad.current || role !== 'system-admin') return;
    openOnLoad.current = false;
    // 等生产卡与入口都渲染好（读数栏有了生产按钮，切到目标页签）再打开。
    return untilReady(() => {
      const ready = detail()?.querySelector(
        `[role="tabpanel"][id$="-${tab}-panel"]:not([hidden]) .user-cycle-reset-action`,
      );
      if (!ready) return false;
      setTimeout(() => openEntry(option), 150);
      return true;
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stamp]);
  const entry = OPTIONS.find(item => item.value === option)!;
  return (
    <>
      {nav && (
        <div className="mk-bar">
          <div className="mk-bar-row">
            <span className="mk-title">
              用户 · 用量重置位置<small>MOCKUP</small>
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
                <span className="mk-ctl-label">页签</span>
                <SegmentedControl
                  value={tab}
                  options={[
                    { value: 'access' as const, label: '接入' },
                    { value: 'forwarding' as const, label: '转发' },
                  ]}
                  ariaLabel="页签"
                  onChange={setTab}
                />
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
          </p>
        </div>
      )}
      <div className={`mk-stage mk-opt-${option}${option === 'current' ? '' : ' mk-hide-legacy'}`}>
        <Pane key={`pane-${stamp}`} role={role} client={client} />
        <Overlay key={`overlay-${stamp}`} option={option} editable={role === 'system-admin'} />
      </div>
    </>
  );
}

createRoot(document.getElementById('root')!).render(<App />);
