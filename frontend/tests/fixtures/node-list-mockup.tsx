// Mockup: 机器页 (machine list) with a page header in the machine detail's language, plus layout options.
// Synthetic data only; never imported by the app. The real App and shell run on a stubbed Console. On the list
// route each option replaces the production list, portalled into the shell's work area; detail pages, the top
// bar and the crumb are the production ones, so clicking a machine opens its real detail for comparison.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/node-list-mockup.html?option=a#/nodes
// Query: option=a|b|c|d|current, scene=typical|calm|trouble|few|many|empty, role=system-admin|readonly|public,
//        theme=dark|light (read by the HTML before modules load), nav=0 hides the review bar.
import { createRoot } from 'react-dom/client';
import { useEffect, useLayoutEffect, useState, useSyncExternalStore, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { AdminRole, ConsoleSnapshot, Whoami } from '../../src/api';
import { App } from '../../src/app';
import { subscribeRouteHistory } from '../../src/forge/route';
import { SessionProvider, type Session } from '../../src/session';
import { SegmentedControl } from '../../src/ui/bits';
import {
  PROBE_TARGETS,
  SCENES,
  WINDOW_SECS,
  agentState,
  linkHealth,
  nicView,
  pingLatest,
  reports,
  trafficItem,
  type Machine,
  type SceneKey,
} from './node-list-mockup-data';
import { MockNodePage, type Option } from './node-list-mockup-ui';
import '../../src/styles.css';
import './node-list-mockup.css';

const params = new URLSearchParams(location.search);
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

type Choice = Option | 'current';
const OPTIONS: { value: Choice; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 页签',
    note: '页头左侧是「机器」身份牌（右下角是整批状态灯）与台数、地区数；中间页签「在用 / 需处理 / 退役」；右侧纳管与 ⋯。退役机器不再是第二张卡，多选退役与移除收进 ⋯。卡片网格不变。',
  },
  {
    value: 'b',
    name: 'B 按地区',
    note: '页头同 A。卡片按 IPv4 归属地分组：台数多的地区在前，小地区并排放进同一行，每组一行小标题（旗帜、地区、台数）。',
  },
  {
    value: 'c',
    name: 'C 卡片 / 列表',
    note: '页头同 A，右侧多一个卡片 / 列表切换。列表一行一台、列对齐：状态与名称、近 12 分钟网卡走势、当前速率、本月与额度、TCP 延迟、地址、上报；24 台一屏放下。窄屏每台三行。',
  },
  {
    value: 'd',
    name: 'D 侧栏筛选',
    note: '页头身份牌与工具同 A，中间换成搜索（名称、ID、地址）。左侧一列分组：全部在用、需处理、失联、未上报、退役，下面是各地区与台数；右侧是所选分组的卡片。窄屏分组横排成一行。',
  },
  { value: 'current', name: '现状', note: '生产页面，未改动。' },
];
const COMMON_NOTE =
  '四个方案的页头都直接使用机器详情的页头结构（连续纸、身份牌、页签横梁、右侧工具，桌面吸顶）；「需处理」里的卡片在名称下写出原因。点任意机器进入的是真实详情页，可以来回对照。';

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
const sceneKey = pick(params.get('scene'), Object.keys(SCENES) as SceneKey[], 'typical');
const machines: Machine[] = SCENES[sceneKey].machines();
const session: Session = { who, initial: { node_count: machines.length, chain_group_count: [] } };
const nowSecs = () => Math.floor(Date.now() / 1000);
const masked = (ip: string | null) =>
  !who.masked_assets || !ip
    ? ip
    : ip.includes(':')
      ? `${ip.split(':').slice(0, 2).join(':')}:***`
      : ip.replace(/\.\d+\.\d+$/, '.***.***');

const snapshot = (): ConsoleSnapshot =>
  ({
    snapshot: {
      revision: 41,
      apps: [],
      nodes: machines.map(machine => ({ id: machine.id, tenant: 'platform', name: machine.name, overlay: true })),
    },
    node_egress_dns: [],
    redacted: who.masked_assets,
  }) as unknown as ConsoleSnapshot;

function loadOverview(machine: Machine, start: number, end: number) {
  const step = Math.max(WINDOW_SECS, Math.floor((end - start) / 240 / WINDOW_SECS) * WINDOW_SECS);
  const count = Math.max(2, Math.floor((end - start) / step));
  const nic = nicView(machine, end, count).series;
  const column = <T,>(pickValue: (index: number) => T) => nic.map((_, index) => pickValue(index));
  const wave = (index: number, base: number, swing: number) => base + swing * Math.sin(index / 7 + machine.id.length);
  const latest = nic.at(-1);
  return {
    node_id: machine.id,
    range_start_unix_secs: start,
    range_end_unix_secs: end,
    reported_at_unix_secs: reports(machine) ? (latest?.window_end_unix_secs ?? null) : null,
    clock_skew_secs: 0,
    host: reports(machine)
      ? {
          kernel: '6.12.48+deb13-amd64',
          cpu_model: 'AMD EPYC 7B13',
          cores: 2,
          cc_algo: 'bbr',
          available_cc: ['reno', 'cubic', 'bbr'],
          default_qdisc: 'fq',
          nic_qdisc: 'fq',
          nic: 'eth0',
          nic_mtu: 1500,
          mem_total_bytes: 2 * 1024 ** 3,
          disk_total_bytes: 40 * 1024 ** 3,
          conntrack_max: 262144,
          sysctl_managed: true,
          arch: 'x86_64',
          os_pretty: 'Debian GNU/Linux 13 (trixie)',
          virt: 'KVM',
          rmem_max: 67108864,
          wmem_max: 67108864,
          somaxconn: 4096,
        }
      : null,
    latest_sample: null,
    series: {
      window_start_unix_secs: nic.map(sample => sample.window_end_unix_secs - step),
      window_end_unix_secs: nic.map(sample => sample.window_end_unix_secs),
      has_gap: nic.map(() => false),
      cpu_user_pct: column(index => wave(index, 6, 3)),
      cpu_sys_pct: column(index => wave(index, 3, 1)),
      cpu_softirq_pct: column(index => wave(index, 2, 1)),
      cpu_peak_pct: column(index => wave(index, 18, 8)),
      cpu_steal_pct: column(() => 0.2),
      load1: column(index => wave(index, 0.3, 0.15)),
      mem_available_bytes: column(index => 1.1 * 1024 ** 3 + wave(index, 0, 60 * 1024 ** 2)),
      swap_used_bytes: column(() => 0),
      oom_kills: column(() => 0),
      disk_free_bytes: column(() => 28 * 1024 ** 3),
      disk_inode_free_pct: column(() => 91),
      nic_rx_bps: nic.map(sample => sample.nic_rx_bps),
      nic_tx_bps: nic.map(sample => sample.nic_tx_bps),
      nic_rx_drop: column(() => 0),
      nic_tx_drop: column(() => 0),
      nic_err: column(() => 0),
      conntrack_count: column(index => Math.round(wave(index, 1400, 300))),
      uptime_secs: column(index => 1_900_000 + index * step),
    },
    processes: reports(machine)
      ? [
          {
            proc: 'xray',
            rss_bytes: 96 * 1024 ** 2,
            cpu_pct: 2.4,
            started_at_unix_secs: end - 6 * 86400,
            fds: 412,
            fd_limit: 65535,
          },
          {
            proc: 'agent',
            rss_bytes: 18 * 1024 ** 2,
            cpu_pct: 0.3,
            started_at_unix_secs: end - 9 * 86400,
            fds: 24,
            fd_limit: 65535,
          },
          { proc: 'wg', rss_bytes: null, cpu_pct: null, started_at_unix_secs: null, fds: null, fd_limit: null },
        ]
      : [],
  };
}

function usageSeries(machine: Machine, start: number, end: number) {
  const nic = nicView(machine, end, Math.max(2, Math.min(240, Math.floor((end - start) / WINDOW_SECS)))).series;
  return {
    since: new Date(start * 1000).toISOString(),
    month_start: new Date(Date.UTC(new Date().getUTCFullYear(), new Date().getUTCMonth(), 1)).toISOString(),
    nodes: [
      {
        node_id: machine.id,
        buckets: nic.map(sample => ({
          window_end: new Date(sample.window_end_unix_secs * 1000).toISOString(),
          user_uplink_bytes: Math.round((sample.nic_tx_bps * WINDOW_SECS) / 8 / 2.2),
          user_downlink_bytes: Math.round((sample.nic_rx_bps * WINDOW_SECS) / 8 / 2.2),
          relay_uplink_bytes: 0,
          relay_downlink_bytes: 0,
        })),
        month_user_uplink_bytes: Math.round(machine.monthGiB * 0.14 * 1024 ** 3),
        month_user_downlink_bytes: Math.round(machine.monthGiB * 0.3 * 1024 ** 3),
        month_relay_uplink_bytes: 0,
        month_relay_downlink_bytes: 0,
        month_has_gap: false,
      },
    ],
  };
}

function pingSeries(machine: Machine, start: number, end: number) {
  const step = Math.max(10, Math.floor((end - start) / 360));
  const times = Array.from({ length: Math.floor((end - start) / step) }, (_, index) => start + (index + 1) * step);
  return {
    node_id: machine.id,
    targets: PROBE_TARGETS.map((name, target) => {
      const base = machine.ping[target] ?? machine.ping.find(value => value != null) ?? 120;
      return {
        name,
        kind: 'tcp',
        ipv4: {
          address: `tcp://203.0.113.${200 + target}:443`,
          probed_at_unix_secs: times,
          attempted: times.map(() => true),
          latency_us: times.map((time, index) => {
            const jitter = Math.sin(time * 12.9898 + target * 78.233) * 43758.5453;
            const spread = jitter - Math.floor(jitter);
            if (spread > 0.992) return null;
            return Math.round((base + 3 * Math.sin(index / 23 + target) + spread * spread * 9) * 1000);
          }),
          skip_reason: times.map(() => null),
        },
        ipv6: null,
      };
    }),
  };
}

const machineOf = (id: string) => machines.find(machine => machine.id === decodeURIComponent(id));
const numberParam = (url: URL, name: string) => Number(url.searchParams.get(name));
const rangeOf = (url: URL) => {
  const end = url.searchParams.has('end_unix_secs') ? numberParam(url, 'end_unix_secs') : nowSecs();
  const start = url.searchParams.has('start_unix_secs')
    ? numberParam(url, 'start_unix_secs')
    : end - (numberParam(url, 'window_secs') || 3600);
  return { start, end };
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
  if (path.startsWith('/compile/'))
    return json({
      revision: 41,
      summary: { errors: 0, warnings: 0, infos: 0, can_publish: true },
      diagnostics: [],
      system: null,
      apps: null,
      redacted: false,
    });
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
  if (path === '/model/snapshot') return json(snapshot());
  if (path === '/nodes/agent-state')
    return json({
      nodes: machines.map(machine => {
        const state = agentState(machine, now);
        return { ...state, public_ipv4: masked(state.public_ipv4), public_ipv6: masked(state.public_ipv6) };
      }),
    });
  if (path === '/node-traffic') return json({ nodes: machines.map(machine => trafficItem(machine, now)) });
  if (path === '/load/nodes/nic')
    return json({
      nodes: machines.filter(reports).map(machine => nicView(machine, now, numberParam(url, 'windows') || 24)),
    });
  if (path === '/ping-probe/nodes/latest')
    return json({ interval_secs: 10, nodes: machines.flatMap(machine => pingLatest(machine, now) ?? []) });
  if (path === '/links/health') return json({ hops: linkHealth(machines, now) });
  if (path === '/links/mtu') return json({ links: [], nodes: [], default_mtu: 1420 });
  if (path === '/agent-log-policy') return json({ global: { max_mib: 64 }, nodes: [] });
  if (path === '/tenants') return json({ tenants: [{ id: 'platform', name: '平台' }] });
  const detail = path.match(/^\/load\/nodes\/([^/]+)\/overview$/);
  if (detail) {
    const machine = machineOf(detail[1]);
    const { start, end } = rangeOf(url);
    if (machine) return json(loadOverview(machine, start, end));
  }
  if (path === '/usage/node-series') {
    const machine = machineOf(url.searchParams.get('node_id') ?? '');
    const { start, end } = rangeOf(url);
    if (machine) return json(usageSeries(machine, start, end));
    return json({ since: new Date(start * 1000).toISOString(), month_start: new Date().toISOString(), nodes: [] });
  }
  const ping = path.match(/^\/ping-probe\/nodes\/([^/]+)\/series$/);
  if (ping) {
    const machine = machineOf(ping[1]);
    const { start, end } = rangeOf(url);
    if (machine) return json(pingSeries(machine, start, end));
  }
  const history = path.match(/^\/nodes\/([^/]+)\/public-ip-history$/);
  if (history)
    return json({ node_id: decodeURIComponent(history[1]), visible_days: 14, retention_days: 90, events: [] });
  if (path.startsWith('/artifacts/content/'))
    return json({
      revision: 41,
      target_kind: 'node',
      target_id: '',
      artifact_kind: 'xray',
      state: 'present',
      sha256: null,
      byte_len: null,
      content: JSON.stringify({ burstObservatory: { subjectSelector: [] } }),
      redacted: false,
    });
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
const isListRoute = () => /^#\/?nodes\/?$/.test(location.hash) || location.hash === '' || location.hash === '#/';

/** Keeps an element as the first child of the shell's `.fg-view` and returns it as a portal target. */
function useWorkSlot(enabled: boolean) {
  const [slot, setSlot] = useState<HTMLElement | null>(null);
  useEffect(() => {
    if (!enabled) {
      setSlot(null);
      return;
    }
    const element = document.createElement('div');
    element.className = 'mk-slot';
    const place = () => {
      const view = document.querySelector('.fg-view');
      if (!view || view.firstChild === element) return;
      view.insertBefore(element, view.firstChild);
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

function Overlay({ option }: { option: Choice }) {
  const list = useSyncExternalStore(subscribeHash, isListRoute);
  const slot = useWorkSlot(option !== 'current' && list);
  useLayoutEffect(() => {
    document.documentElement.dataset.mkReplace = option !== 'current' && list ? 'true' : 'false';
  }, [option, list]);
  if (!slot || option === 'current') return null;
  return createPortal(<MockNodePage option={option} scene={sceneKey} machines={machines} />, slot);
}

/* ══ 审阅外壳 ══ */

const client = new QueryClient({
  defaultOptions: {
    queries: { staleTime: 5_000, refetchOnWindowFocus: false, refetchOnReconnect: false, retry: false },
  },
});

function ReviewBar({ option, onOption }: { option: Choice; onOption: (next: Choice) => void }) {
  const entry = OPTIONS.find(item => item.value === option)!;
  const reload = (name: string, value: string) => {
    const next = new URLSearchParams(location.search);
    next.set(name, value);
    location.search = next.toString();
  };
  return (
    <div className="mk-bar">
      <div className="mk-bar-row">
        <span className="mk-title">
          机器页 · 页头与布局<small>MOCKUP</small>
        </span>
        <div className="mk-controls">
          <label className="mk-ctl">
            <span className="mk-ctl-label">方案</span>
            <SegmentedControl
              value={option}
              options={OPTIONS.map(item => ({ value: item.value, label: item.name }))}
              ariaLabel="方案"
              onChange={onOption}
            />
          </label>
          <label className="mk-ctl">
            <span className="mk-ctl-label">场景</span>
            <select value={sceneKey} onChange={event => reload('scene', event.target.value)}>
              {(Object.keys(SCENES) as SceneKey[]).map(key => (
                <option key={key} value={key}>
                  {SCENES[key].label}
                </option>
              ))}
            </select>
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
        <b>{entry.name}</b>
        {entry.note}
        {option !== 'current' && <span className="mk-common">{COMMON_NOTE}</span>}
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
  const [option, setOption] = useState<Choice>(
    pick<Choice>(params.get('option'), ['a', 'b', 'c', 'd', 'current'], 'a'),
  );
  const nav = params.get('nav') !== '0';
  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('option', option);
    history.replaceState(history.state, '', `?${next}${location.hash}`);
  }, [option]);
  useLayoutEffect(() => {
    document.documentElement.dataset.mkNav = nav ? 'true' : 'false';
  }, [nav]);
  return (
    <>
      {nav && <ReviewBar option={option} onOption={setOption} />}
      <Overlay option={option} />
    </>
  );
}

if (!location.hash) history.replaceState(null, '', `${location.search}#/nodes`);
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
