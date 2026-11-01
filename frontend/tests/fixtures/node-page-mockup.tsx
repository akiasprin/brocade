// Mockup: 机器页重新设计，四个方向。Synthetic data only; never imported by the app.
// The real App and shell run on a stubbed Console. On the machine list route each option replaces the production
// list, portalled into the shell's work area; D also takes over machine detail routes so its roster stays beside
// the production detail. Everything else (top bar, crumb, detail pages, wizards) is production.
// Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/node-page-mockup.html?option=a#/nodes
// Query: option=a|b|c|d|current, scene=typical|calm|trouble|few|many|empty, role=system-admin|readonly|public,
//        theme=dark|light (read by the HTML before modules load), nav=0 hides the review dock,
//        device=phone shows the page inside a 390×844 phone frame. The frame loads this same page with embed=1,
//        which renders touch styles and follows the dock's option through postMessage.
import { createRoot } from 'react-dom/client';
import { useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { AdminRole, ConsoleSnapshot, Whoami } from '../../src/api';
import { App } from '../../src/app';
import { parse, subscribeRouteHistory } from '../../src/forge/route';
import type { Drill } from '../../src/panes/nodes';
import { SessionProvider, type Session } from '../../src/session';
import { SegmentedControl } from '../../src/ui/bits';
import {
  PROBE_TARGETS,
  SCENES,
  WINDOW_SECS,
  agentState,
  nicView,
  pingLatest,
  reports,
  trafficItem,
  type Machine,
  type SceneKey,
} from './node-list-mockup-data';
import { MockMachinePage, type Option } from './node-page-mockup-ui';
import '../../src/styles.css';
import './node-page-mockup.css';

const params = new URLSearchParams(location.search);
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

type Choice = Option | 'current';
type Device = 'desktop' | 'phone';
const CHOICES = ['a', 'b', 'c', 'd', 'current'] as const satisfies readonly Choice[];
const DEVICES = ['desktop', 'phone'] as const satisfies readonly Device[];
/** 框内页面（手机预览）不画评审条，方案跟随外层评审条。 */
const embed = params.get('embed') === '1';
const OPTIONS: { value: Choice; name: string; note: string; phone: string }[] = [
  {
    value: 'a',
    name: 'A 小组件',
    note: '保留卡片网格，每张按「身份 → 当下 → 本月」三层排：名称行右侧是三网延迟（异常时换成原因），中间是当前速率与近 12 分钟走势，底部是本月用量、额度与进度条。',
    phone: '手机：单列，每张卡保留三层，一屏完整显示 4 台（现状也是 4 台）。',
  },
  {
    value: 'b',
    name: 'B 清单',
    note: '一行一台，列对齐后可以纵向比较：走势、当前速率、本月与额度、每个探测目标的延迟；名称下一行是地址，异常时换成原因。',
    phone:
      '手机：每台两行。左边名称与地址（异常时换成原因），右边当前速率与本月用量；走势和延迟列收起，一屏完整显示 11 台。',
  },
  {
    value: 'c',
    name: 'C 磁贴',
    note: '小磁贴一屏放下整个机队：旗面身份牌、名称和一行读数（当前速率 · 本月额度）。异常的磁贴染色并写出原因；走势和延迟进详情查看。',
    phone: '手机：两列磁贴，身份牌在上，名称与读数在下，一屏完整显示 10 台。',
  },
  {
    value: 'd',
    name: 'D 名册',
    note: '与用户页相同的双栏：左侧机器名册常驻（身份牌、名称、延迟或原因、速率与额度环），右侧是所选机器的生产详情页，切换机器时名册不动；进入机器页默认选中第一台在线机器。',
    phone: '手机：名册占满整页，一屏完整显示 11 台；点一台进入生产详情，名册收起。',
  },
  { value: 'current', name: '现状', note: '生产页面，未改动。', phone: '手机：单列卡片，一屏完整显示 4 台。' },
];
const PHONE_NOTE = '预览框 390×844，框内按触屏样式渲染（没有悬停）。';
const COMMON_NOTE =
  '四个方案共用：生产标题栏（机器、多选、纳管机器），名称排序、退役排末尾；常态不画状态灯，只有异常才出现颜色和原因文字；读数改用正文字体的等宽数字；悬停「建一条链」不再放在列表上，详情「规则」页有「添加新链」。';

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
        total_targets: machines.length,
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
      content: JSON.stringify({}),
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
const hashSnapshot = () => location.hash;

/** Keeps an element as the first child of the shell's `.fg-view` and returns it as a portal target. */
function useWorkSlot(enabled: boolean) {
  const [element] = useState(() => {
    const slot = document.createElement('div');
    slot.className = 'mk-slot';
    return slot;
  });
  useEffect(() => {
    if (!enabled) return;
    // 外壳换页会重建 .fg-view（key 是当前导航），插槽要跟着挪进新的工作区。
    const place = () => {
      const view = document.querySelector('.fg-view');
      if (!view || view.firstChild === element) return;
      view.insertBefore(element, view.firstChild);
    };
    place();
    const observer = new MutationObserver(place);
    observer.observe(document.getElementById('root')!, { childList: true, subtree: true });
    return () => {
      observer.disconnect();
      element.remove();
    };
  }, [enabled, element]);
  return enabled ? element : null;
}

function Overlay({ option }: { option: Choice }) {
  const hash = useSyncExternalStore(subscribeHash, hashSnapshot);
  const loc = useMemo(() => parse(hash), [hash]);
  const onNodes = !loc || loc.nav === 'nodes';
  const drill = ((onNodes ? loc?.drill : undefined) as Drill | undefined) ?? { p: 'list' };
  const replace = option !== 'current' && onNodes && (drill.p === 'list' || (option === 'd' && drill.p === 'node'));
  const slot = useWorkSlot(replace);
  useLayoutEffect(() => {
    document.documentElement.dataset.mkReplace = replace ? 'true' : 'false';
    document.documentElement.dataset.mkOption = option;
  }, [replace, option]);
  if (!slot || !replace) return null;
  return createPortal(<MockMachinePage option={option} drill={drill} />, slot);
}

/* ══ 评审条 ══ */

const client = new QueryClient({
  defaultOptions: {
    queries: { staleTime: 5_000, refetchOnWindowFocus: false, refetchOnReconnect: false, retry: false },
  },
});

function reload(name: string, value: string) {
  const next = new URLSearchParams(location.search);
  next.set(name, value);
  location.search = next.toString();
}

function Dock({
  option,
  onOption,
  device,
  onDevice,
}: {
  option: Choice;
  onOption: (next: Choice) => void;
  device: Device;
  onDevice: (next: Device) => void;
}) {
  const [folded, setFolded] = useState(false);
  const entry = OPTIONS.find(item => item.value === option)!;
  if (folded) {
    return (
      <div className="mk-dock folded" role="region" aria-label="方案评审">
        <button type="button" className="mk-unfold" onClick={() => setFolded(false)}>
          {entry.name}
        </button>
      </div>
    );
  }
  return (
    <div className="mk-dock" role="region" aria-label="方案评审">
      <div className="mk-dock-row">
        <SegmentedControl
          value={option}
          options={OPTIONS.map(item => ({ value: item.value, label: item.name }))}
          ariaLabel="方案"
          onChange={onOption}
        />
        <button type="button" className="mk-fold" aria-label="收起评审条" onClick={() => setFolded(true)}>
          ×
        </button>
      </div>
      <div className="mk-dock-row">
        <SegmentedControl
          className="mk-device"
          value={device}
          options={[
            { value: 'desktop', label: '电脑' },
            { value: 'phone', label: '手机' },
          ]}
          ariaLabel="设备"
          onChange={onDevice}
        />
        <select aria-label="场景" value={sceneKey} onChange={event => reload('scene', event.target.value)}>
          {(Object.keys(SCENES) as SceneKey[]).map(key => (
            <option key={key} value={key}>
              {SCENES[key].label}
            </option>
          ))}
        </select>
        <select aria-label="身份" value={role} onChange={event => reload('role', event.target.value)}>
          <option value="system-admin">管理员</option>
          <option value="readonly">只读</option>
          <option value="public">访客</option>
        </select>
        <select
          aria-label="主题"
          value={document.documentElement.dataset.theme}
          onChange={event => reload('theme', event.target.value)}
        >
          <option value="dark">暗</option>
          <option value="light">亮</option>
        </select>
      </div>
      <p className="mk-dock-note">
        {device === 'phone' && (
          <span className="mk-dock-phone">
            {entry.phone}
            <small>{PHONE_NOTE}</small>
          </span>
        )}
        {entry.note}
        {option !== 'current' && <span className="mk-dock-common">{COMMON_NOTE}</span>}
      </p>
    </div>
  );
}

/* ══ 手机预览：同一页面装进 390×844 的机框，框内按触屏样式渲染 ══ */

const PHONE = { width: 390, height: 844, bezel: 12 } as const;
const FRAME_WIDTH = PHONE.width + 2 * PHONE.bezel;
const FRAME_HEIGHT = PHONE.height + 2 * PHONE.bezel;
/** 视口至少 900 宽时评审条站在机框右侧（宽 460 + 间距 40，与样式表的断点一致）；
    更窄时评审条仍浮在底部，机框让出它实际的高度。 */
const SIDE_DOCK_MIN_VIEWPORT = 900;
const SIDE_DOCK_WIDTH = 500;
/** 底部评审条离视口底边 14px，再与机框隔开 12px。 */
const BOTTOM_DOCK_SPACE = 26;
const STAGE_PADDING = 24;

const subscribeResize = (listener: () => void) => {
  addEventListener('resize', listener);
  return () => removeEventListener('resize', listener);
};
const viewportWidth = () => innerWidth;
const viewportHeight = () => innerHeight;

/** 浮在底部的评审条高度随说明文字变化，量实际高度；收起或隐藏时为 0。 */
function useDockHeight() {
  const [height, setHeight] = useState(0);
  useEffect(() => {
    const dock = document.querySelector<HTMLElement>('.mk-dock');
    if (!dock) return;
    const observer = new ResizeObserver(() => setHeight(dock.offsetHeight));
    observer.observe(dock);
    return () => observer.disconnect();
  }, []);
  return height;
}

type FrameMessage = { mk: 'ready' } | { mk: 'option'; option: Choice };
const isChoice = (value: unknown): value is Choice => (CHOICES as readonly unknown[]).includes(value);

/** 框内页面沿用当前的场景、身份、主题与路由，去掉评审条。 */
function frameSource() {
  const next = new URLSearchParams(location.search);
  next.delete('device');
  next.delete('nav');
  next.set('embed', '1');
  return `${location.pathname}?${next}${location.hash || '#/nodes'}`;
}

function StatusGlyphs() {
  return (
    <svg className="mk-phone-glyphs" viewBox="0 0 68 12" width="68" height="12" aria-hidden="true">
      <rect x="0" y="7.5" width="3" height="4.5" rx="1" />
      <rect x="4.6" y="5.5" width="3" height="6.5" rx="1" />
      <rect x="9.2" y="3" width="3" height="9" rx="1" />
      <rect x="13.8" y="0.5" width="3" height="11.5" rx="1" />
      <path className="arc" d="M23.6 4.4a9.4 9.4 0 0 1 12.8 0" />
      <path className="arc" d="M26 7a5.8 5.8 0 0 1 8 0" />
      <circle cx="30" cy="10.2" r="1.6" />
      <rect className="shell" x="42.5" y="0.5" width="22" height="11" rx="3.4" />
      <rect x="44.5" y="2.5" width="18" height="7" rx="1.8" />
      <path className="cap" d="M66 4.2v3.6a2 2 0 0 0 0-3.6z" />
    </svg>
  );
}

function PhoneStage({ option, children }: { option: Choice; children: ReactNode }) {
  const width = useSyncExternalStore(subscribeResize, viewportWidth);
  const height = useSyncExternalStore(subscribeResize, viewportHeight);
  const dockHeight = useDockHeight();
  const side = width >= SIDE_DOCK_MIN_VIEWPORT;
  const reserve = side || dockHeight === 0 ? 0 : dockHeight + BOTTOM_DOCK_SPACE;
  // 机框按视口等比缩小；框内页面的布局视口始终是 390 宽，媒体查询不受缩放影响。
  const scale = Math.max(
    0.4,
    Math.min(
      1,
      (width - 2 * STAGE_PADDING - (side ? SIDE_DOCK_WIDTH : 0)) / FRAME_WIDTH,
      (height - 2 * STAGE_PADDING - reserve) / FRAME_HEIGHT,
    ),
  );
  const frame = useRef<HTMLIFrameElement>(null);
  const [source] = useState(frameSource);
  const latest = useRef(option);
  // 换方案不重载框内页面（保留它的路由与滚动）：框内就绪时来要一次当前方案，之后随评审条推送。
  useEffect(() => {
    latest.current = option;
    const message: FrameMessage = { mk: 'option', option };
    frame.current?.contentWindow?.postMessage(message, location.origin);
  }, [option]);
  useEffect(() => {
    const onMessage = (event: MessageEvent<FrameMessage>) => {
      const target = frame.current?.contentWindow;
      if (!target || event.source !== target || event.data?.mk !== 'ready') return;
      const message: FrameMessage = { mk: 'option', option: latest.current };
      target.postMessage(message, location.origin);
    };
    addEventListener('message', onMessage);
    return () => removeEventListener('message', onMessage);
  }, []);
  return (
    <div className="mk-stage" style={{ paddingBottom: STAGE_PADDING + reserve }}>
      <div className="mk-phone-slot" style={{ width: FRAME_WIDTH * scale, height: FRAME_HEIGHT * scale }}>
        <div className="mk-phone" style={{ transform: `scale(${scale})` }}>
          <div className="mk-phone-screen">
            <div className="mk-phone-status" aria-hidden="true">
              <span>9:41</span>
              <StatusGlyphs />
            </div>
            <iframe ref={frame} src={source} title="手机预览" />
            <i className="mk-phone-island" />
            <i className="mk-phone-home" />
          </div>
        </div>
      </div>
      {children}
    </div>
  );
}

/** 框内页面：方案由外层评审条决定，挂载后向外层要当前方案。 */
function useFramedOption(setOption: (next: Choice) => void) {
  useEffect(() => {
    if (!embed) return;
    const onMessage = (event: MessageEvent<FrameMessage>) => {
      if (event.source !== parent || event.origin !== location.origin) return;
      const data = event.data;
      if (data?.mk === 'option' && isChoice(data.option)) setOption(data.option);
    };
    addEventListener('message', onMessage);
    const ready: FrameMessage = { mk: 'ready' };
    parent.postMessage(ready, location.origin);
    return () => removeEventListener('message', onMessage);
  }, [setOption]);
}

function Root({ children }: { children: ReactNode }) {
  return (
    <QueryClientProvider client={client}>
      <SessionProvider value={session}>{children}</SessionProvider>
    </QueryClientProvider>
  );
}

function Mockup() {
  const [option, setOption] = useState<Choice>(() => pick<Choice>(params.get('option'), CHOICES, 'a'));
  const [device, setDevice] = useState<Device>(() =>
    embed ? 'desktop' : pick(params.get('device'), DEVICES, 'desktop'),
  );
  useFramedOption(setOption);
  useLayoutEffect(() => {
    const next = new URLSearchParams(location.search);
    next.set('option', option);
    if (device === 'phone') next.set('device', 'phone');
    else next.delete('device');
    history.replaceState(history.state, '', `?${next}${location.hash}`);
    if (!embed) document.documentElement.dataset.mkDevice = device;
  }, [option, device]);
  const dock = !embed && params.get('nav') !== '0' && (
    <Dock option={option} onOption={setOption} device={device} onDevice={setDevice} />
  );
  // 手机预览时外层的生产外壳只是隐藏，换回电脑时路由和多选状态都还在。
  return (
    <>
      <Overlay option={option} />
      {device === 'phone' ? <PhoneStage option={option}>{dock}</PhoneStage> : dock}
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
