// Mockup: where the per-user 账期与有效期 lives in 用户详情, and how it is edited. Synthetic data only; never
// imported by the app. The production users pane is mounted on a stubbed backend and each option is portalled
// into it (页头 / 读数栏 / 页头横带 / 「更多」菜单), so the surrounding page is the real one.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/user-service-mockup.html?option=a&scene=term
// Query: option=a|b|c|d|current, scene=typical|term|soon|pending|expired, tab=access|forwarding,
//        role=system-admin|readonly, open=1 opens the editor on load, theme=dark|light, nav=0 hides the review bar.
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
  UserServicePolicyWrite,
} from '../../src/api';
import { theme } from '../../src/forge/theme';
import { UsersPane } from '../../src/panes/users';
import { SessionProvider } from '../../src/session';
import { SegmentedControl } from '../../src/ui/bits';
import type { Win } from '../../src/wm/store';
import {
  GiB,
  iso,
  midnight,
  periodOf,
  policyOf,
  previewOf,
  SCENE_KEYS,
  SCENES,
  stateOf,
  usageBetween,
  type Policy,
  type SceneKey,
} from './user-service-mockup-data';
import {
  BandEditor,
  LedgerTerms,
  MenuEntry,
  PolicyDialog,
  PopoverEditor,
  TermBand,
  TermChip,
} from './user-service-mockup-ui';
import '../../src/styles.css';
import './user-service-mockup.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';

type Option = 'a' | 'b' | 'c' | 'd' | 'current';
const OPTIONS: { value: Option; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 页头徽标',
    note: '有效期是用户级状态，放在名字旁边：页头一枚徽标，常态写「长期有效」或到期日，七天内到期、未生效、已到期才上色；三个页签都看得到。点它在下方弹出编辑面板。原横条去掉，重置日只在「用量与额度」里写下次重置日期。',
  },
  {
    value: 'b',
    name: 'B 读数栏',
    note: '与「计量口径」放在同一处：「用量与额度」左侧读数栏底部加「重置日」「有效期」两行，接入、转发两个页签各一份。点击在读数栏右侧弹出编辑面板。',
  },
  {
    value: 'c',
    name: 'C 页头时间轴',
    note: '页头下方加一条细横带：刻度是每次重置，圆点是今天，实线是有效区间，长期有效向右淡出；右侧写到期。点「调整」在横带里就地展开编辑，一行排开。',
  },
  {
    value: 'd',
    name: 'D 更多菜单',
    note: '常态不占版面：入口收进「更多」菜单第一项，副标题写当前账期与有效期。只有未生效、已到期或七天内到期时，页头才出现状态徽标，点它同样打开。编辑在对话框里。',
  },
  { value: 'current', name: '现状', note: '生产里现在的横条与对话框，同一份数据。' },
];

const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

/* ══ 合成后端 ══ */

const NOW = Date.now();
const tenant = 'preview';
const SELECTED = 'alice';

interface Person {
  id: string;
  policy: Policy;
  extra?: Partial<UserListItem>;
  usage: number;
}
const others: Person[] = [
  { id: 'carol', policy: { resetDay: 1, start: null, end: null }, extra: { account_type: 'test' }, usage: 161 * GiB },
  {
    id: 'dave',
    policy: { resetDay: 1, start: midnight(NOW, 3), end: null },
    usage: 0,
  },
  { id: 'erin', policy: { resetDay: 1, start: null, end: null }, extra: { status: 'disabled' }, usage: 241 * GiB },
];

const userItem = (id: string, policy: Policy, extra: Partial<UserListItem> = {}): UserListItem => ({
  tenant_id: tenant,
  id,
  status: 'active',
  created_at: '2026-01-01T00:00:00Z',
  created_revision: 1,
  uuid: '6f1c2d3e-4b5a-4c6d-8e7f-90a1b2c3d4e5',
  login_enabled: true,
  traffic_accounting_mode: 'single',
  traffic_reset_day: policy.resetDay,
  service_starts_at: iso(policy.start),
  service_expires_at: iso(policy.end),
  service_state: stateOf(policy, NOW),
  ...extra,
});

const chains = ['hk-01', 'jp-01', 'tw-01'];
const snapshot = {
  snapshot: {
    revision: 7,
    apps: [
      {
        id: 'apac',
        label: 'APAC Premium',
        chains: chains.map(id => ({ id, name: id.toUpperCase(), subscription_country: id.slice(0, 2).toUpperCase() })),
        steps: [],
        fronts: [],
        ingresses: chains.map(
          (chain, i) => ({ id: `in-${chain}`, chain, node: 'n1', port: 8448 + i }) as SnapshotIngress,
        ),
        grants: [SELECTED, ...others.map(person => person.id)].flatMap(user =>
          chains.map(chain => ({ tenant, user, ingress: `in-${chain}` })),
        ),
      },
    ],
    nodes: [{ id: 'n1', name: '示例节点' }],
  },
  node_egress_dns: [],
  redacted: false,
} as unknown as ConsoleSnapshot;

const RULES = [
  { id: 11, name: 'ssh-relay', port: 20022, target: '10.0.0.5', targetPort: 22, share: 0.7 },
  { id: 12, name: 'game', port: 20777, target: 'play.example.net', targetPort: 7777, share: 0.3 },
];

/** 选中用户的一切读数都由当前策略推出；保存后整页（名册、页头灯、读数栏）跟着变。 */
function world(policy: Policy) {
  const period = periodOf(policy.resetDay, NOW);
  const used = usageBetween(period.start, period.end, NOW, live.served);
  const users = [
    userItem(SELECTED, policy, { direct_login_enabled: true }),
    ...others.map(person => userItem(person.id, person.policy, person.extra)),
  ];
  const cycles: UserCycleUsage = {
    periods: users.map(user => {
      const own = periodOf(user.traffic_reset_day ?? 1, NOW);
      return { tenant_id: tenant, user_id: user.id, starts_at: iso(own.start)!, ends_at: iso(own.end)! };
    }),
    views: [
      {
        tenant_id: tenant,
        user_id: SELECTED,
        app_id: 'apac',
        uplink_bytes: Math.round(used.access * 0.03),
        downlink_bytes: Math.round(used.access * 0.97),
        has_gap: false,
      },
      ...others
        .filter(person => person.usage > 0)
        .map(person => ({
          tenant_id: tenant,
          user_id: person.id,
          app_id: 'apac',
          uplink_bytes: GiB,
          downlink_bytes: person.usage - GiB,
          has_gap: false,
        })),
    ],
    forwarding: RULES.map(rule => ({
      tenant_id: tenant,
      user_id: SELECTED,
      node_id: 'n1',
      rule_id: rule.id,
      rule_name: rule.name,
      uplink_bytes: Math.round(used.forwarding * rule.share * 0.4),
      downlink_bytes: Math.round(used.forwarding * rule.share * 0.6),
      has_gap: false,
    })),
  };
  const suspended = stateOf(policy, NOW) !== 'active';
  const overview: ForwardingOverview = {
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
        cycle_used_bytes: used.forwarding,
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
      runtime_status: suspended ? 'service-paused' : 'active',
      runtime_error: null,
      uplink_bytes: Math.round(used.forwarding * rule.share * 0.4),
      downlink_bytes: Math.round(used.forwarding * rule.share * 0.6),
      created_at: '2026-09-01T00:00:00Z',
      updated_at: '2026-09-01T00:00:00Z',
    })),
  };
  return { users, cycles, overview };
}

/**
 * 生产卡与改版共用的「后端」：一份当前策略，接口和查询缓存都从它取。served 是场景里用户实际有服务的区间，
 * 日账本只在这段时间里有流量；保存新策略不改写过去。
 */
const initial = SCENES[pick(params.get('scene'), SCENE_KEYS, 'term')].policy(NOW);
const live = { policy: initial, served: initial as Pick<Policy, 'start' | 'end'>, client: null as QueryClient | null };
const listeners = new Set<(policy: Policy) => void>();
function commit(policy: Policy) {
  live.policy = policy;
  const { users, cycles, overview } = world(policy);
  live.client?.setQueryData(['users'], { users });
  live.client?.setQueryData(['user-cycles'], cycles);
  live.client?.setQueryData(['forwarding', tenant, SELECTED], overview);
  listeners.forEach(listener => listener(policy));
}

globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), location.origin);
  const path = url.pathname;
  const method = init?.method ?? 'GET';
  const { users, cycles, overview } = world(live.policy);
  if (path.endsWith('/service-policy/preview')) {
    const policy = policyOf(JSON.parse(String(init?.body)) as UserServicePolicyWrite);
    const preview = previewOf(policy, NOW, live.served);
    return Response.json({
      starts_at: iso(preview.start),
      ends_at: iso(preview.end),
      access_used_bytes: preview.access,
      forwarding_used_bytes: preview.forwarding,
      service_state: preview.state,
    });
  }
  if (path.endsWith('/service-policy') && method === 'PUT') {
    await new Promise(resolve => setTimeout(resolve, 300));
    commit(policyOf(JSON.parse(String(init?.body)) as UserServicePolicyWrite));
    return Response.json(world(live.policy).users[0]);
  }
  if (path === '/users') return Response.json({ users });
  if (path === '/usage/user-cycles') return Response.json(cycles);
  if (path === '/quotas') return Response.json({ quotas: QUOTAS });
  if (path === '/model/snapshot') return Response.json(snapshot);
  if (path === '/users/presence') return Response.json({ freshness_secs: 120, users: [] });
  if (path === '/tenants') return Response.json({ tenants: [{ id: tenant, name: '示例' }] });
  if (path === '/nodes/agent-state') return Response.json({ nodes: [{ node_id: 'n1', name: '示例节点' }] });
  if (path.endsWith('/forwarding')) return Response.json(overview);
  if (path.endsWith('/tcp-pings')) return Response.json([]);
  if (path.endsWith('/presence-history'))
    return Response.json({ tenant_id: tenant, user_id: SELECTED, retention_days: 30, truncated: false, sources: [] });
  if (path === '/grant-probes/capability') return Response.json({ available: false, reason: '示例不拨测' });
  return Response.json({ error: `示例未提供 ${path}` }, { status: 404 });
};

const QUOTAS = [
  {
    tenant_id: tenant,
    user_id: SELECTED,
    app_id: 'apac',
    limit_bytes: 200 * GiB,
    updated_at: '2026-09-01T00:00:00Z',
    suspended_ingresses: [],
  },
];

function seededClient(policy: Policy): QueryClient {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity, refetchOnWindowFocus: false } },
  });
  const { users, cycles, overview } = world(policy);
  client.setQueryData(['snapshot'], snapshot);
  client.setQueryData(['nodes'], { nodes: [{ node_id: 'n1', name: '示例节点' }] });
  client.setQueryData(['users'], { users });
  client.setQueryData(['tenants'], { tenants: [{ id: tenant, name: '示例' }] });
  client.setQueryData(['quotas'], { quotas: QUOTAS });
  client.setQueryData(['user-cycles'], cycles);
  client.setQueryData(['forwarding', tenant, SELECTED], overview);
  return client;
}

/* ══ 插槽：在生产页面里留一个空位给改版部件 ══ */

/**
 * Keeps an empty `display: contents` element right after `after` inside `parent` (or first when `after` is null),
 * re-inserting it whenever the production pane re-renders that spot, and returns it as a portal target.
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
    element.className = 'usm-slot';
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

const detail = () => document.querySelector('.user-split-detail');
const panel = (tab: 'access' | 'forwarding') => detail()?.querySelector(`[role="tabpanel"][id$="-${tab}-panel"]`);
const within = (parent: Element | null | undefined, after: string) =>
  parent ? { parent, after: parent.querySelector(`:scope > ${after}`) } : null;

/* ══ 各方案挂到生产页面上 ══ */

function Overlay({
  option,
  editable,
  openOnLoad,
  tab,
}: {
  option: Option;
  editable: boolean;
  openOnLoad: boolean;
  tab: 'access' | 'forwarding';
}) {
  const [policy, setPolicy] = useState(live.policy);
  const [open, setOpen] = useState(openOnLoad && editable);
  // 方案 B 的两个读数栏各有一份入口；面板贴着打开它的那一份。
  const [openFrom, setOpenFrom] = useState<'access' | 'forwarding'>(tab);
  useEffect(() => {
    const listener = (next: Policy) => setPolicy(next);
    listeners.add(listener);
    return () => void listeners.delete(listener);
  }, []);
  const save = (next: Policy) => {
    commit(next);
    setOpen(false);
  };
  const chipRef = useRef<HTMLButtonElement>(null);
  const accessTermsRef = useRef<HTMLButtonElement>(null);
  const forwardingTermsRef = useRef<HTMLButtonElement>(null);

  const chipSlot = useSlot(
    option === 'a' || option === 'd',
    () => {
      const name = detail()?.querySelector('.user-dhead .dname');
      return name ? { parent: name, after: name.querySelector(':scope > .user-account-state') } : null;
    },
    [option],
  );
  const accessLedgerSlot = useSlot(
    option === 'b',
    () => within(panel('access')?.querySelector('.user-usage-ledger'), '.user-usage-accounting'),
    [option],
  );
  const forwardingLedgerSlot = useSlot(
    option === 'b',
    () => within(panel('forwarding')?.querySelector('.fwd-ledger'), '.user-usage-accounting'),
    [option],
  );
  const bandSlot = useSlot(option === 'c', () => within(detail()?.querySelector('.user-dhead'), '.user-dhead-main'), [
    option,
  ]);
  const menuSlot = useSlot(
    option === 'd' && editable,
    () => {
      const menu = detail()?.querySelector('.user-action-menu');
      return menu ? { parent: menu, after: null } : null;
    },
    [option],
  );

  const userId = SELECTED;
  const popover = (anchor: typeof chipRef, side: 'below' | 'right') =>
    open && (
      <PopoverEditor
        anchor={anchor}
        side={side}
        saved={policy}
        served={live.served}
        now={NOW}
        userId={userId}
        onClose={() => setOpen(false)}
        onSaved={save}
      />
    );

  let parts: ReactNode = null;
  if (option === 'a') {
    parts = (
      <>
        {chipSlot &&
          createPortal(
            <TermChip
              policy={policy}
              now={NOW}
              editable={editable}
              open={open}
              chipRef={chipRef}
              onOpen={() => setOpen(value => !value)}
            />,
            chipSlot,
          )}
        {popover(chipRef, 'below')}
      </>
    );
  } else if (option === 'b') {
    const terms = (tab: 'access' | 'forwarding', ref: typeof accessTermsRef) => (
      <LedgerTerms
        policy={policy}
        now={NOW}
        editable={editable}
        open={open && openFrom === tab}
        termsRef={ref}
        onOpen={() => {
          setOpenFrom(tab);
          setOpen(value => !(value && openFrom === tab));
        }}
      />
    );
    const anchor = openFrom === 'forwarding' ? forwardingTermsRef : accessTermsRef;
    parts = (
      <>
        {accessLedgerSlot && createPortal(terms('access', accessTermsRef), accessLedgerSlot)}
        {forwardingLedgerSlot && createPortal(terms('forwarding', forwardingTermsRef), forwardingLedgerSlot)}
        {popover(anchor, 'right')}
      </>
    );
  } else if (option === 'c') {
    parts =
      bandSlot &&
      createPortal(
        <TermBand
          policy={policy}
          now={NOW}
          editable={editable}
          editing={open}
          onEdit={() => setOpen(true)}
          editor={
            <BandEditor
              key={String(open)}
              saved={policy}
              served={live.served}
              now={NOW}
              onCancel={() => setOpen(false)}
              onSaved={save}
            />
          }
        />,
        bandSlot,
      );
  } else if (option === 'd') {
    parts = (
      <>
        {chipSlot &&
          createPortal(
            <TermChip
              policy={policy}
              now={NOW}
              editable={editable}
              open={open}
              chipRef={chipRef}
              onOpen={() => setOpen(true)}
              quietWhenNormal
            />,
            chipSlot,
          )}
        {menuSlot && createPortal(<MenuEntry policy={policy} now={NOW} onOpen={() => setOpen(true)} />, menuSlot)}
        {open && (
          <PolicyDialog
            userId={userId}
            saved={policy}
            served={live.served}
            now={NOW}
            onClose={() => setOpen(false)}
            onSaved={save}
          />
        )}
      </>
    );
  }
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
          who: {
            operator_id: 'operator',
            role,
            tenant_scope: null,
            token_prefix: null,
            masked_assets: false,
          },
          initial: { node_count: 1, chain_group_count: [] },
        }}
      >
        <UsersPane win={win} />
      </SessionProvider>
    </QueryClientProvider>
  );
}

function App() {
  const [option, setOption] = useState<Option>(
    pick(params.get('option'), ['a', 'b', 'c', 'd', 'current'] as const, 'a'),
  );
  const [sceneKey, setSceneKey] = useState<SceneKey>(pick(params.get('scene'), SCENE_KEYS, 'term'));
  const [tab, setTab] = useState<'access' | 'forwarding'>(
    pick(params.get('tab'), ['access', 'forwarding'] as const, 'access'),
  );
  const [role, setRole] = useState<AdminRole>(
    pick(params.get('role'), ['system-admin', 'readonly'] as const, 'system-admin'),
  );
  const [mode, setMode] = useState(theme.snapshot());
  const [generation, setGeneration] = useState(0);
  const nav = params.get('nav') !== '0';
  const openOnLoad = params.get('open') === '1' && generation === 0;
  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('option', option);
    next.set('scene', sceneKey);
    next.set('tab', tab);
    next.set('role', role);
    next.set('theme', mode);
    next.delete('open');
    history.replaceState(null, '', `?${next}`);
  }, [option, sceneKey, tab, role, mode]);
  // 场景、身份或方案变化时整页重挂：生产页面的菜单、草稿和页签都从头开始。
  const stamp = `${option}-${sceneKey}-${role}-${generation}`;
  const client = useMemo(() => {
    live.policy = SCENES[sceneKey].policy(NOW);
    live.served = live.policy;
    live.client = seededClient(live.policy);
    return live.client;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stamp]);
  useEffect(() => {
    listeners.forEach(listener => listener(live.policy));
  }, [stamp]);
  useEffect(() => {
    let frame = 0;
    let tries = 0;
    const select = () => {
      const button = detail()?.querySelector<HTMLButtonElement>(`.user-detail-tabs button[id$="-${tab}-tab"]`);
      if (button) {
        if (button.getAttribute('aria-selected') !== 'true') button.click();
        return;
      }
      if (tries++ < 120) frame = requestAnimationFrame(select);
    };
    select();
    return () => cancelAnimationFrame(frame);
  }, [tab, stamp]);
  const entry = OPTIONS.find(item => item.value === option)!;
  return (
    <>
      {nav && (
        <div className="mk-bar">
          <div className="mk-bar-row">
            <span className="mk-title">
              用户 · 账期与有效期<small>MOCKUP</small>
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
      <div className={`mk-stage${option === 'current' ? '' : ' mk-hide-legacy'}`}>
        <div className="mk-frame">
          <Pane key={`pane-${stamp}`} role={role} client={client} />
          <Overlay
            key={`overlay-${stamp}`}
            option={option}
            editable={role === 'system-admin'}
            openOnLoad={openOnLoad}
            tab={tab}
          />
        </div>
      </div>
    </>
  );
}

createRoot(document.getElementById('root')!).render(<App />);
