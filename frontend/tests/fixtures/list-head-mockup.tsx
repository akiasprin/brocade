// Mockup: 线路 and 用户 list pages with the 机器 page head (identity plate with a lamp, group tabs, tools and ⋯).
// Synthetic data only; never imported by the app. The real App and shell run on a stubbed Console. On the two list
// routes the mockup renders the production pane a second time inside a new page frame, portalled into the shell's
// work area, and hides the shell's own copy; everything below the head is therefore the production page.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/list-head-mockup.html#/chains
// Query: look=new|current, users=span|roster, role=system-admin|readonly|public, theme=dark|light (read by the
//        HTML before modules load), nav=0 hides the review bar.
import { createRoot } from 'react-dom/client';
import { useEffect, useLayoutEffect, useState, useSyncExternalStore, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { AdminRole, Whoami } from '../../src/api';
import type { ModelOp } from '../../src/draft';
import { App } from '../../src/app';
import { subscribeRouteHistory } from '../../src/forge/route';
import { SessionProvider, type Session } from '../../src/session';
import { SegmentedControl } from '../../src/ui/bits';
import {
  MACHINES,
  TENANT,
  USER_LIST,
  chainSnapshot,
  e2eProbes,
  previewSnapshot,
  quotas,
  trafficPolicies,
  userCycles,
  userPresence,
} from './list-head-mockup-data';
import { agentState, linkHealth, nicView, pingLatest, reports, trafficItem } from './node-list-mockup-data';
import { MockChainPage, MockUsersPage, type UsersLayout } from './list-head-mockup-ui';
import '../../src/styles.css';
import './list-head-mockup.css';

const params = new URLSearchParams(location.search);
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

type Look = 'new' | 'current';

const NOTES: Record<'chains' | 'users' | 'other', Record<Look, string>> = {
  chains: {
    new: '页头与机器页相同：身份牌（右下角是全部链的状态灯）、页签「全部 / 降级」、右侧新建分组与 ⋯。多选删除收进 ⋯；降级页签只列出要处理的链，卡片名称下写出原因。分组、链卡、拖动排序与改名不变。',
    current: '生产页面，未改动。',
  },
  users: {
    new: '',
    current: '生产页面，未改动。',
  },
  other: {
    new: '机器页是已落地的生产页面，作为对照。点顶栏的「线路」「用户」查看本次 mockup。',
    current: '机器页是已落地的生产页面，作为对照。点顶栏的「线路」「用户」查看本次 mockup。',
  },
};
const USERS_NOTES: Record<UsersLayout, string> = {
  span: '通栏：页头横跨名册与详情，与机器页、线路页在同一位置——身份牌（右下角是需处理的提示灯）、页签「全部 / 需处理 / 测试 / 已停用」、新增用户。名册只留搜索与列表；详情不变。窄屏进入某个用户时，页头随名册一起收起。',
  roster:
    '名册栏：页头放在名册一栏顶上，第一层身份牌与新增用户，第二层四个页签，下面是搜索与列表。详情和它的页头不变，两栏页头顶边对齐。',
};

/* ══ 合成后端 ══ */

const role = pick(params.get('role'), ['system-admin', 'readonly', 'public'] as const, 'system-admin');
const who: Whoami =
  role === 'public'
    ? { operator_id: 'public', role: 'readonly', tenant_scope: null, token_prefix: null, masked_assets: true }
    : {
        operator_id: 'operator',
        role: role as AdminRole,
        tenant_scope: null,
        token_prefix: null,
        masked_assets: role === 'readonly',
      };
const session: Session = { who, initial: { node_count: MACHINES.length, chain_group_count: [] } };
const nowSecs = () => Math.floor(Date.now() / 1000);
const masked = (ip: string | null) =>
  !who.masked_assets || !ip
    ? ip
    : ip.includes(':')
      ? `${ip.split(':').slice(0, 2).join(':')}:***`
      : ip.replace(/\.\d+\.\d+$/, '.***.***');

const numberParam = (url: URL, name: string) => Number(url.searchParams.get(name));
const COMPILE_VIEW = {
  revision: 41,
  summary: { errors: 0, warnings: 0, infos: 0, can_publish: true },
  diagnostics: [],
  system: null,
  apps: null,
  redacted: false,
};

globalThis.fetch = async (input, init) => {
  const url = new URL(String(input instanceof Request ? input.url : input), location.origin);
  const path = url.pathname;
  const method = (init?.method ?? 'GET').toUpperCase();
  const now = nowSecs();
  const json = (body: unknown) => Response.json(body);
  if (path === '/branding') return json({ site_name: 'Brocade', icon_data_url: null });
  if (path === '/auth/state') return json({ initialized: true, public_open: role === 'public' });
  if (path === '/bootstrap') return json(session);
  if (path === '/whoami') return json(who);
  if (path === '/revisions')
    return json({
      current_revision: 41,
      revisions: [41, 40, 39].map(id => ({
        id,
        created_at: '2026-10-23T10:00:00Z',
        author: 'operator',
        note: null,
        status: 'committed',
        current: id === 41,
        has_snapshot: true,
      })),
    });
  if (path.startsWith('/compile/')) return json(COMPILE_VIEW);
  if (path === '/model/preview' && method === 'POST') {
    const { ops } = JSON.parse(String(init?.body ?? '{}')) as { ops?: ModelOp[] };
    return json({
      snapshot: previewSnapshot(chainSnapshot(who.masked_assets), ops ?? []),
      compile: COMPILE_VIEW,
      artifacts: { revision: 41, artifacts: [] },
    });
  }
  if (path === '/deployments/verify' && method === 'POST')
    return json({
      revision_id: 41,
      converged: true,
      summary: {
        total_targets: 24,
        changed_targets: 0,
        skipped_targets: 0,
        deferred_targets: 0,
        disruptive_targets: 0,
        max_wave: 0,
      },
      targets: [],
    });
  if (path === '/deployments') return json({ deployments: [] });
  if (path === '/grants/automation')
    return json({
      pending_jobs: 0,
      retrying_jobs: 0,
      failed_jobs: 0,
      max_attempts: 5,
      latest_revision_id: 41,
      oldest_pending_at: null,
      last_attempt_at: null,
      last_error: null,
    });
  if (path === '/notifications')
    return json({
      retention_days: 30,
      latest_event_id: 0,
      last_seen_event_id: 0,
      cleared_through_event_id: 0,
      unread_count: 0,
      active: [],
      events: [],
    });
  if (path === '/model/snapshot') return json(chainSnapshot(who.masked_assets));
  if (path === '/settings') return json({ ports: { hop_base: 20000 } });
  if (path === '/tenants') return json({ tenants: [{ id: TENANT, name: '平台' }] });

  /* 机器页：生产页面作对照，接口与机器 mockup 相同。 */
  if (path === '/nodes/agent-state')
    return json({
      nodes: MACHINES.map(machine => {
        const state = agentState(machine, now);
        return { ...state, public_ipv4: masked(state.public_ipv4), public_ipv6: masked(state.public_ipv6) };
      }),
    });
  if (path === '/node-traffic') return json({ nodes: MACHINES.map(machine => trafficItem(machine, now)) });
  if (path === '/load/nodes/nic')
    return json({
      nodes: MACHINES.filter(reports).map(machine => nicView(machine, now, numberParam(url, 'windows') || 24)),
    });
  if (path === '/ping-probe/nodes/latest')
    return json({ interval_secs: 10, nodes: MACHINES.flatMap(machine => pingLatest(machine, now) ?? []) });
  if (path === '/links/health') return json({ hops: linkHealth(MACHINES, now) });
  if (path === '/links/mtu') return json({ links: [], nodes: [], default_mtu: 1420 });
  if (path === '/agent-log-policy') return json({ global: { max_mib: 64 }, nodes: [] });

  /* 线路 */
  if (path === '/probes/e2e') return json(e2eProbes(now));

  /* 用户 */
  if (path === '/users') return json({ users: USER_LIST });
  if (path === '/users/presence') return json(userPresence());
  if (path === '/usage/user-cycles') return json(userCycles());
  if (path === '/quotas') return json(quotas());
  if (path === '/grant-probes/capability') return json({ available: false, reason: '示例不拨测' });
  const userScoped = path.match(
    /^\/users\/([^/]+)\/([^/]+)\/(traffic-policy|forwarding|presence-history|source-access)$/,
  );
  if (userScoped && method === 'GET') {
    const [, tenant, user, part] = userScoped.map(decodeURIComponent);
    if (part === 'traffic-policy') return json(trafficPolicies(user));
    if (part === 'forwarding')
      return json({
        tenant_id: tenant,
        user_id: user,
        rows: [],
        series: [],
        sources: [],
        entitlements: [],
        rules: [],
        nodes: [],
      });
    if (part === 'presence-history')
      return json({ tenant_id: tenant, user_id: user, retention_days: 30, truncated: false, sources: [] });
    return json({ revision: 1, ips: [], nodes: [] });
  }
  return Response.json({ error: `示例未提供 ${path}` }, { status: 404 });
};

/** The realtime channels stay silent; every reading falls back to the polled queries. */
class SilentEventSource extends EventTarget {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSED = 2;
  readonly readyState = 0;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  constructor(readonly url: string) {
    super();
  }
  close() {}
}
Object.defineProperty(window, 'EventSource', { value: SilentEventSource, configurable: true });

/* ══ 插槽：在外壳的工作区里给方案页面留位置 ══ */

/* 外壳用 pushState 换页，不会触发 hashchange；路由模块在每次写入历史后通知，前进后退另有 popstate。 */
const subscribeHash = (listener: () => void) => {
  const stop = subscribeRouteHistory(listener);
  addEventListener('hashchange', listener);
  addEventListener('popstate', listener);
  return () => {
    stop();
    removeEventListener('hashchange', listener);
    removeEventListener('popstate', listener);
  };
};

type Route = { page: 'chains' } | { page: 'users'; user: string | null } | { page: 'other' };

const readRoute = (): string => location.hash;
function routeOf(hash: string): Route {
  if (/^#\/?chains\/?$/.test(hash)) return { page: 'chains' };
  const users = hash.match(/^#\/?users(?:\/user\/([^/?#]+))?\/?$/);
  if (users) return { page: 'users', user: users[1] ? decodeURIComponent(users[1]) : null };
  return { page: 'other' };
}

/** Keeps a portal target as the first child of the shell's `.fg-view` while `enabled`. */
function useWorkSlot(enabled: boolean) {
  const [slot] = useState(() => {
    const element = document.createElement('div');
    element.className = 'mk-slot';
    return element;
  });
  useEffect(() => {
    if (!enabled) return;
    const place = () => {
      const view = document.querySelector('.fg-view');
      if (!view || view.firstChild === slot) return;
      view.insertBefore(slot, view.firstChild);
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

function Overlay({ look, usersLayout }: { look: Look; usersLayout: UsersLayout }) {
  const route = routeOf(useSyncExternalStore(subscribeHash, readRoute));
  const replace = look === 'new' && route.page !== 'other';
  const slot = useWorkSlot(replace);
  useLayoutEffect(() => {
    document.documentElement.dataset.mkReplace = replace ? 'true' : 'false';
  }, [replace]);
  if (!slot || !replace) return null;
  if (route.page === 'chains') return createPortal(<MockChainPage />, slot);
  if (route.page === 'users')
    return createPortal(<MockUsersPage layout={usersLayout} user={route.user} key={usersLayout} />, slot);
  return null;
}

/* ══ 审阅外壳 ══ */

const client = new QueryClient({
  defaultOptions: {
    queries: { staleTime: 5_000, refetchOnWindowFocus: false, refetchOnReconnect: false, retry: false },
  },
});

function ReviewBar({
  look,
  onLook,
  usersLayout,
  onUsersLayout,
}: {
  look: Look;
  onLook: (next: Look) => void;
  usersLayout: UsersLayout;
  onUsersLayout: (next: UsersLayout) => void;
}) {
  const route = routeOf(useSyncExternalStore(subscribeHash, readRoute));
  const reload = (name: string, value: string) => {
    const next = new URLSearchParams(location.search);
    next.set(name, value);
    location.search = next.toString();
  };
  const note = route.page === 'users' && look === 'new' ? USERS_NOTES[usersLayout] : NOTES[route.page][look];
  const label = route.page === 'chains' ? '线路' : route.page === 'users' ? '用户' : '机器';
  return (
    <div className="mk-bar">
      <div className="mk-bar-row">
        <span className="mk-title">
          线路与用户 · 页头<small>MOCKUP</small>
        </span>
        <div className="mk-controls">
          <label className="mk-ctl">
            <span className="mk-ctl-label">页头</span>
            <SegmentedControl
              value={look}
              options={[
                { value: 'new' as const, label: '新页头' },
                { value: 'current' as const, label: '现状' },
              ]}
              ariaLabel="页头"
              onChange={onLook}
            />
          </label>
          <label className="mk-ctl">
            <span className="mk-ctl-label">用户页</span>
            <SegmentedControl
              value={usersLayout}
              options={[
                { value: 'span' as const, label: '通栏' },
                { value: 'roster' as const, label: '名册栏' },
              ]}
              ariaLabel="用户页"
              disabled={look === 'current'}
              onChange={onUsersLayout}
            />
          </label>
          <label className="mk-ctl">
            <span className="mk-ctl-label">身份</span>
            <select value={role} onChange={event => reload('role', event.target.value)}>
              <option value="system-admin">管理员</option>
              <option value="readonly">只读</option>
              <option value="public">访客</option>
            </select>
          </label>
          <label className="mk-ctl">
            <span className="mk-ctl-label">主题</span>
            <select
              value={document.documentElement.dataset.theme}
              onChange={event => reload('theme', event.target.value)}
            >
              <option value="dark">深</option>
              <option value="light">浅</option>
            </select>
          </label>
        </div>
      </div>
      <p className="mk-scene-note">
        <b>{label}</b>
        {note}
      </p>
    </div>
  );
}

function Root({ children }: { children: ReactNode }) {
  return (
    <QueryClientProvider client={client}>
      <SessionProvider value={session}>{children}</SessionProvider>
    </QueryClientProvider>
  );
}

function Mockup() {
  const [look, setLook] = useState<Look>(pick<Look>(params.get('look'), ['new', 'current'], 'new'));
  const [usersLayout, setUsersLayout] = useState<UsersLayout>(
    pick<UsersLayout>(params.get('users'), ['span', 'roster'], 'span'),
  );
  const nav = params.get('nav') !== '0';
  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('look', look);
    next.set('users', usersLayout);
    history.replaceState(history.state, '', `?${next}${location.hash}`);
  }, [look, usersLayout]);
  useLayoutEffect(() => {
    document.documentElement.dataset.mkNav = nav ? 'true' : 'false';
  }, [nav]);
  return (
    <>
      {nav && <ReviewBar look={look} onLook={setLook} usersLayout={usersLayout} onUsersLayout={setUsersLayout} />}
      <Overlay look={look} usersLayout={usersLayout} />
    </>
  );
}

if (!location.hash) history.replaceState(null, '', `${location.search}#/chains`);
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <App />
  </QueryClientProvider>,
);
createRoot(document.getElementById('mk-root')!).render(
  <Root>
    <Mockup />
  </Root>,
);
