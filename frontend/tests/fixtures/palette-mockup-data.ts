// Synthetic backend for the palette mockup. Never imported by the app.
// One fleet backs every page: the chain-wizard machines and users, the test-node mockup's chains, and
// deterministic load / traffic / latency curves that end at the real current time.
import type {
  ConsoleBootstrap,
  E2eProbeItem,
  E2eProbeStatus,
  LoadSample,
  MachineEventList,
  NodeAgentStateItem,
  NodeLoadView,
  NodePingProbeLatestList,
  NodeTrafficView,
  OnboardingState,
  RevisionList,
} from '../../src/api';
import type { NodeRealtimeEvent, RealtimeUserTrafficRate } from '../../src/node-realtime';
import { agentNodes, MACHINES, USERS } from './chain-wizard-mockup-data';
import { CHAIN_APPS, chainSnapshot } from './test-node-mockup-data';

const NOW = Date.now();
const NOW_S = Math.floor(NOW / 1000);
const MINUTE = 60;
const HOUR = 3600;
const DAY = 86_400;
const MB = 1_000_000 / 8;
const GiB = 1024 ** 3;
const iso = (secs: number) => new Date(secs * 1000).toISOString();

/** Deterministic pseudo-random sequence so screenshots stay stable between reloads. */
function noise(seed: number) {
  let state = seed % 2147483647 || 1;
  return () => {
    state = (state * 48271) % 2147483647;
    return state / 2147483647;
  };
}
/** Stateless hash in [0, 1) for per-window jitter; consecutive inputs land far apart. */
const hash01 = (value: number) => {
  const x = Math.sin(value * 12.9898 + 78.233) * 43758.5453;
  return x - Math.floor(x);
};
const seedOf = (text: string) => [...text].reduce((sum, char) => sum * 31 + char.charCodeAt(0), 7) % 100_000;

/* ══ 机队 ══ */

const OFFLINE = new Set(['sg-dp']);
const ACTIVE = MACHINES.filter(machine => !machine.retired);

interface Profile {
  /** 网卡下行基线（Mbit/s）。 */
  mbps: number;
  cpu: number;
  memGiB: number;
  memUsed: number;
  cores: number;
  ping: number;
}
const PROFILE: Record<string, Profile> = {
  'hk-hkix': { mbps: 182, cpu: 23, memGiB: 4, memUsed: 0.46, cores: 4, ping: 31 },
  'hk-bgp': { mbps: 241, cpu: 38, memGiB: 8, memUsed: 0.52, cores: 4, ping: 34 },
  'hk-home': { mbps: 46, cpu: 9, memGiB: 2, memUsed: 0.61, cores: 2, ping: 39 },
  'mo-ctm': { mbps: 64, cpu: 14, memGiB: 2, memUsed: 0.4, cores: 2, ping: 44 },
  'tw-hinet': { mbps: 92, cpu: 17, memGiB: 4, memUsed: 0.38, cores: 2, ping: 52 },
  'jp-iij': { mbps: 136, cpu: 21, memGiB: 4, memUsed: 0.44, cores: 4, ping: 68 },
  'jp-ntt': { mbps: 118, cpu: 19, memGiB: 4, memUsed: 0.57, cores: 4, ping: 71 },
  'sg-dp': { mbps: 0, cpu: 0, memGiB: 2, memUsed: 0.3, cores: 1, ping: 0 },
  'us-gia': { mbps: 74, cpu: 31, memGiB: 2, memUsed: 0.66, cores: 2, ping: 154 },
  'us-9929': { mbps: 38, cpu: 12, memGiB: 2, memUsed: 0.48, cores: 2, ping: 162 },
  'de-fra': { mbps: 12, cpu: 6, memGiB: 4, memUsed: 0.29, cores: 2, ping: 228 },
  'gb-lon': { mbps: 21, cpu: 8, memGiB: 2, memUsed: 0.35, cores: 1, ping: 236 },
};
const profileOf = (id: string): Profile => PROFILE[id] ?? PROFILE['hk-hkix'];

/** 网卡速率曲线：日周期 + 两个慢波 + 少量毛刺。返回 bytes/s。 */
function nicAt(id: string, secs: number) {
  const p = profileOf(id);
  const phase = (seedOf(id) % 628) / 100;
  const hour = ((secs / HOUR + 8) % 24) / 24;
  const daily = 0.62 + 0.38 * Math.sin((hour - 0.38) * Math.PI * 2);
  const wave = 1 + 0.16 * Math.sin(secs / 1900 + phase) + 0.1 * Math.sin(secs / 260 + phase * 2);
  const jitter = 1 + (hash01(seedOf(id) * 0.37 + Math.floor(secs / 30)) - 0.5) * 0.3;
  const rx = p.mbps * MB * daily * wave * jitter;
  return { rx: Math.round(rx), tx: Math.round(rx * (0.93 + 0.05 * Math.sin(secs / 700 + phase))) };
}

function hostOf(id: string) {
  const p = profileOf(id);
  return {
    kernel: '6.8.0-45-generic',
    cpu_model: id.startsWith('hk') ? 'AMD EPYC 7B13' : 'Intel Xeon Platinum 8272CL',
    cores: p.cores,
    cpu_freq_max_mhz: null,
    cpu_governor: null,
    cc_algo: 'bbr',
    available_cc: ['reno', 'cubic', 'bbr'],
    default_qdisc: 'fq',
    nic_qdisc: 'fq',
    nic: 'eth0',
    nic_mtu: 1500,
    mem_total_bytes: p.memGiB * GiB,
    disk_total_bytes: 40 * GiB,
    disk_mount: '/',
    disk_filesystem: 'ext4',
    disk_device: '/dev/vda1',
    disk_read_only: false,
    conntrack_max: 262_144,
    ephemeral_port_low: 32768,
    ephemeral_port_high: 60999,
    ephemeral_port_capacity: 28232,
    sysctl_managed: true,
    arch: 'x86_64',
    os_pretty: 'Debian GNU/Linux 12 (bookworm)',
    virt: 'KVM',
    rmem_max: 67_108_864,
    wmem_max: 67_108_864,
    somaxconn: 4096,
  };
}

function loadSample(id: string, start: number, end: number): LoadSample {
  const p = profileOf(id);
  const random = noise(seedOf(id) + start);
  const nic = nicAt(id, end);
  const busy = nic.rx / (p.mbps * MB || 1);
  const user = p.cpu * 0.55 * busy + random() * 3;
  const sys = p.cpu * 0.2 * busy + random() * 1.5;
  const softirq = p.cpu * 0.25 * busy + random() * 2;
  return {
    window_start_unix_secs: start,
    window_end_unix_secs: end,
    has_gap: false,
    cpu_user_pct: user,
    cpu_sys_pct: sys,
    cpu_softirq_pct: softirq,
    cpu_peak_pct: Math.min(100, (user + sys + softirq) * (1.4 + random() * 0.5)),
    cpu_steal_pct: id === 'us-gia' ? 2 + random() * 3 : random() * 0.4,
    load1: ((user + sys + softirq) / 100) * p.cores * 1.2,
    cpu_detail: null,
    mem_available_bytes: Math.round(p.memGiB * GiB * (1 - p.memUsed - 0.04 * Math.sin(end / 5000))),
    swap_used_bytes: 0,
    memory_detail: null,
    oom_kills: 0,
    disk_free_bytes: 27 * GiB,
    disk_inode_free_pct: 91,
    disk_detail: null,
    nic_rx_bps: nic.rx * 8,
    nic_tx_bps: nic.tx * 8,
    nic_rx_drop: 0,
    nic_tx_drop: 0,
    nic_err: 0,
    conntrack_count: Math.round(1800 + busy * 4200),
    network_detail: null,
    uptime_secs: 38 * DAY + (end - NOW_S),
  };
}

function loadView(id: string, start: number, end: number): NodeLoadView {
  const step = end - start > 2 * DAY ? 15 * MINUTE : end - start > 6 * HOUR ? 5 * MINUTE : MINUTE;
  const series: LoadSample[] = [];
  if (!OFFLINE.has(id)) {
    for (let t = Math.ceil(start / step) * step; t + step <= end; t += step) series.push(loadSample(id, t, t + step));
  }
  return {
    node_id: id,
    range_start_unix_secs: start,
    range_end_unix_secs: end,
    reported_at_unix_secs: OFFLINE.has(id) ? NOW_S - 3 * HOUR : NOW_S - 20,
    clock_skew_secs: 0,
    host: hostOf(id),
    latest_sample: series.at(-1) ?? loadSample(id, NOW_S - 3 * HOUR - 60, NOW_S - 3 * HOUR),
    series,
    processes: [
      {
        proc: 'xray',
        rss_bytes: 148 * 1024 ** 2,
        cpu_pct: profileOf(id).cpu * 0.6,
        started_at_unix_secs: NOW_S - 6 * DAY,
        fds: 1840,
        fd_limit: 1_048_576,
      },
      { proc: 'wg', rss_bytes: null, cpu_pct: null, started_at_unix_secs: null, fds: null, fd_limit: null },
      {
        proc: 'agent',
        rss_bytes: 21 * 1024 ** 2,
        cpu_pct: 0.4,
        started_at_unix_secs: NOW_S - 6 * DAY,
        fds: 38,
        fd_limit: 1024,
      },
    ],
  } as NodeLoadView;
}

function nicList(windows: number) {
  return {
    nodes: ACTIVE.filter(machine => !OFFLINE.has(machine.id)).map(machine => {
      const end = Math.floor((NOW_S - 15) / 30) * 30;
      return {
        node_id: machine.id,
        series: Array.from({ length: windows }, (_, index) => {
          const windowEnd = end - (windows - 1 - index) * 30;
          const nic = nicAt(machine.id, windowEnd);
          return {
            window_start_unix_secs: windowEnd - 30,
            window_end_unix_secs: windowEnd,
            has_gap: false,
            nic_rx_bps: nic.rx * 8,
            nic_tx_bps: nic.tx * 8,
          };
        }),
      };
    }),
  };
}

function fullAgentNodes(): NodeAgentStateItem[] {
  return agentNodes().map(base => {
    const offline = OFFLINE.has(base.node_id);
    const retired = base.lifecycle_phase === 'retired';
    return {
      ...base,
      token_prefix: 'bra_4f2c',
      token_created_at: iso(NOW_S - 60 * DAY),
      token_last_used_at: iso(offline ? NOW_S - 3 * HOUR : NOW_S - 12),
      token_revoked_at: null,
      agent_version: '0.3.2',
      agent_protocol_version: 43,
      vpngate_probe_enabled: false,
      runtime_versions: retired
        ? null
        : {
            agent: '0.3.2',
            xray: '25.10.15-brocade.3',
            phantun: null,
            openvpn: base.runtime_versions?.openvpn ?? null,
            wg_tools: 'v1.0.20210914',
            wg_backend: 'kernel',
          },
      spool_backlog: null,
      last_local_reconcile: null,
      wireguard_health: null,
      runtime_reported_at: iso(offline ? NOW_S - 3 * HOUR : NOW_S - 15),
      runtime_report_fresh: !offline && !retired,
      geodata_observed: null,
      last_poll_at: iso(offline ? NOW_S - 3 * HOUR : NOW_S - 8),
      desired_poll_fresh: !offline && !retired,
      last_usage_report_at: iso(NOW_S - 30),
      xray_started_at: iso(NOW_S - 6 * DAY),
      mtu: null,
      connection: {},
      overlay: true,
      egress_allowed: true,
      dns: { t: 'system' },
      domain_strategy: 'AsIs',
      lifecycle_epoch: 1,
      lifecycle_deployment_id: null,
      lifecycle_completed_at: null,
      lifecycle_last_error: null,
      operationally_isolated: false,
      isolated_at: null,
      convergence_debt_count: 0,
      convergence_debt_failed: false,
      service_reentry_ready: true,
      service_reentry_blockers: [],
      wg_transport_kind: 'udp',
      wg_fake_tcp_port: null,
      applied: null,
    } as unknown as NodeAgentStateItem;
  });
}

/* ══ 延迟探测 ══ */

const PING_TARGETS = [
  { name: '深圳电信', kind: 'icmp' as const, ipv4: '202.96.134.33', ipv6: null, base: 1, phase: 0.2 },
  { name: '深圳移动', kind: 'icmp' as const, ipv4: '120.196.165.24', ipv6: null, base: 1.35, phase: 1.4 },
  { name: 'Cloudflare', kind: 'icmp' as const, ipv4: '1.1.1.1', ipv6: '2606:4700:4700::1111', base: 0.4, phase: 2.5 },
  { name: '深圳电信', kind: 'tcp' as const, ipv4: '202.96.134.33:443', ipv6: null, base: 1.15, phase: 0.7 },
];

function pingLatency(node: string, target: number, family: 'ipv4' | 'ipv6', secs: number) {
  const spec = PING_TARGETS[target];
  const base = profileOf(node).ping * spec.base * (family === 'ipv6' ? 1.08 : 1);
  const wave = Math.sin(secs / 1400 + spec.phase) * 0.08 + Math.sin(secs / 230 + spec.phase) * 0.04;
  const spike = Math.sin(secs / 173 + spec.phase * 3) > 0.992 ? base * 0.6 : 0;
  return Math.max(0.4, base * (1 + wave) + spike);
}

function pingSeries(node: string, start: number, end: number) {
  const span = end - start;
  const step = span <= HOUR ? 60 : span <= 6 * HOUR ? 120 : span <= DAY ? 300 : 900;
  const first = Math.ceil(start / step) * step;
  const times: number[] = [];
  for (let t = first; t <= end; t += step) times.push(t);
  const columns = (target: number, family: 'ipv4' | 'ipv6', address: string) => {
    const lossAt = Math.floor(times.length * (0.31 + target * 0.17)) % times.length;
    return {
      address,
      probed_at_unix_secs: times,
      attempted: times.map(() => true),
      latency_us: times.map((t, index) =>
        index === lossAt ? null : Math.round(pingLatency(node, target, family, t) * 1000),
      ),
      skip_reason: times.map(() => null),
    };
  };
  return {
    node_id: node,
    targets: PING_TARGETS.map((spec, index) => ({
      name: spec.name,
      kind: spec.kind,
      ipv4: columns(index, 'ipv4', spec.kind === 'icmp' ? `icmp://${spec.ipv4}` : `tcp://${spec.ipv4}`),
      ipv6: spec.ipv6 ? columns(index, 'ipv6', `icmp://[${spec.ipv6}]`) : null,
    })),
  };
}

function pingLatest(): NodePingProbeLatestList {
  return {
    interval_secs: 60,
    nodes: ACTIVE.filter(machine => !OFFLINE.has(machine.id)).map(machine => ({
      node_id: machine.id,
      targets: PING_TARGETS.map((spec, index) => ({
        name: spec.name,
        kind: spec.kind,
        ipv4: {
          address: spec.kind === 'icmp' ? `icmp://${spec.ipv4}` : `tcp://${spec.ipv4}`,
          latest: {
            probed_at_unix_secs: NOW_S - 20,
            attempted: true,
            latency_us: Math.round(pingLatency(machine.id, index, 'ipv4', NOW_S) * 1000),
          },
        },
        ipv6: spec.ipv6
          ? {
              address: `icmp://[${spec.ipv6}]`,
              latest: {
                probed_at_unix_secs: NOW_S - 20,
                attempted: true,
                latency_us: Math.round(pingLatency(machine.id, index, 'ipv6', NOW_S) * 1000),
              },
            }
          : null,
      })),
    })),
  };
}

/* ══ 实时流量 ══ */

interface Flow {
  user: string;
  node: string;
  down: number;
  up: number;
  duty?: number;
  phase: number;
}
const FLOWS: Flow[] = [
  { user: 'alice', node: 'hk-hkix', down: 38 * MB, up: 1.2 * MB, phase: 0 },
  { user: 'alice', node: 'jp-iij', down: 2.4 * MB, up: 0.3 * MB, duty: 0.55, phase: 1 },
  { user: 'bob', node: 'jp-ntt', down: 86 * MB, up: 2.1 * MB, phase: 2 },
  { user: 'grace', node: 'mo-ctm', down: 3.1 * MB, up: 2.6 * MB, phase: 3 },
  { user: 'ivan', node: 'us-gia', down: 12.5 * MB, up: 0.6 * MB, phase: 4 },
  { user: 'carol', node: 'hk-bgp', down: 24 * MB, up: 0.9 * MB, phase: 5 },
  { user: 'erin', node: 'us-9929', down: 6.2 * MB, up: 0.4 * MB, duty: 0.5, phase: 7 },
  { user: 'heidi', node: 'gb-lon', down: 0.32 * MB, up: 0.05 * MB, phase: 8 },
  { user: 'dave', node: 'tw-hinet', down: 9.4 * MB, up: 0.2 * MB, phase: 9 },
  { user: 'judy', node: 'hk-bgp', down: 4.1 * MB, up: 0.3 * MB, duty: 0.7, phase: 10 },
];
const startedAt = Date.now();
const clock = () => 37 + (Date.now() - startedAt) / 1000;

function flowRate(flow: Flow, t: number) {
  if (flow.duty !== undefined && (Math.sin(t / 4.2 + flow.phase * 1.9) + 1) / 2 > flow.duty) return { down: 0, up: 0 };
  const swing = 0.8 + 0.14 * Math.sin(t / 3.1 + flow.phase) + 0.06 * Math.sin(t / 1.3 + flow.phase * 2.7);
  return { down: Math.round(flow.down * swing), up: Math.round(flow.up * (0.9 + (swing - 0.8))) };
}

function nodeEvent(id: string, at: number): NodeRealtimeEvent {
  const t = clock();
  const users: RealtimeUserTrafficRate[] = [];
  let attributed = 0;
  for (const flow of FLOWS) {
    if (flow.node !== id) continue;
    const rate = flowRate(flow, t);
    if (rate.down + rate.up === 0) continue;
    users.push({
      tenant_id: USERS.find(user => user.id === flow.user)?.tenant ?? 'platform',
      user_id: flow.user,
      ingress_id: `i-${id}`,
      downlink_bytes_per_sec: rate.down,
      uplink_bytes_per_sec: rate.up,
    });
    attributed += rate.down + rate.up;
  }
  const nic = nicAt(id, Math.floor(at / 1000));
  return {
    node_id: id,
    received_at_unix_millis: at,
    sample: {
      sequence: Math.floor(t),
      sampled_at_unix_millis: at - 140,
      rx_bytes_per_sec: Math.max(nic.rx, Math.round(attributed * 1.03)),
      tx_bytes_per_sec: Math.max(nic.tx, Math.round(attributed * 1.05)),
      has_gap: false,
      user_traffic: {
        state: 'ready',
        all_users: true,
        users,
        truncated: false,
        has_gap: false,
        forwarding_supported: true,
        forwarding_expected: false,
      },
    },
  };
}

function fleetTrafficSnapshot() {
  const now = Date.now();
  return {
    policy: { enabled: true },
    nodes: ACTIVE.map(machine => ({
      node_id: machine.id,
      connected: !OFFLINE.has(machine.id),
      active: !OFFLINE.has(machine.id),
      interval_secs: 1,
      samples: [nodeEvent(machine.id, OFFLINE.has(machine.id) ? now - 3 * HOUR * 1000 : now)],
    })),
    expected: ACTIVE.map(machine => ({
      node_id: machine.id,
      users:
        grantsByNode()
          .get(machine.id)
          ?.map(user => ({ tenant_id: USERS.find(item => item.id === user)?.tenant ?? 'platform', user_id: user })) ??
        [],
    })),
  };
}

/** 用户 → 授权的入口机器，取自线路快照。 */
function grantsByNode() {
  const result = new Map<string, string[]>();
  for (const app of CHAIN_APPS) {
    for (const grant of app.grants as unknown as { user: string; ingress: string }[]) {
      const ingress = app.ingresses.find(item => item.id === grant.ingress) as unknown as { node: string } | undefined;
      if (!ingress) continue;
      const list = result.get(ingress.node) ?? [];
      if (!list.includes(grant.user)) list.push(grant.user);
      result.set(ingress.node, list);
    }
  }
  return result;
}

class FixtureEventSource {
  onerror: (() => void) | null = null;
  onopen: (() => void) | null = null;
  private listeners = new Map<string, ((event: MessageEvent) => void)[]>();
  private timer: ReturnType<typeof setInterval> | undefined;

  constructor(readonly url: string) {
    const path = new URL(url, location.origin).pathname;
    setTimeout(() => this.start(path), 30);
  }

  private start(path: string) {
    const live = ACTIVE.filter(machine => !OFFLINE.has(machine.id)).map(machine => machine.id);
    if (path === '/realtime/nodes/events' || path === '/realtime/fleet-traffic/events') {
      const snapshot = fleetTrafficSnapshot();
      this.emit(
        'snapshot',
        path === '/realtime/nodes/events'
          ? { nodes: snapshot.nodes }
          : { policy: snapshot.policy, nodes: snapshot.nodes },
      );
      this.timer = setInterval(() => {
        const now = Date.now();
        for (const id of live) this.emit('sample', nodeEvent(id, now));
      }, 1_000);
      return;
    }
    const node = /^\/realtime\/nodes\/([^/]+)\/events$/.exec(path)?.[1];
    if (node) {
      const id = decodeURIComponent(node);
      const offline = OFFLINE.has(id);
      this.emit('snapshot', { node_id: id, connected: !offline, samples: offline ? [] : [nodeEvent(id, Date.now())] });
      if (!offline) this.timer = setInterval(() => this.emit('sample', nodeEvent(id, Date.now())), 1_000);
    }
  }

  addEventListener(type: string, listener: (event: MessageEvent) => void) {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }

  removeEventListener(type: string, listener: (event: MessageEvent) => void) {
    this.listeners.set(
      type,
      (this.listeners.get(type) ?? []).filter(item => item !== listener),
    );
  }

  close() {
    if (this.timer) clearInterval(this.timer);
  }

  private emit(type: string, payload: unknown) {
    const event = new MessageEvent(type, { data: JSON.stringify(payload) });
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
}

/* ══ 线路拨测 ══ */

const CHAIN_TTFB: Record<string, number | null> = {
  'c-hk-direct': 46,
  'c-jp-relay': 98,
  'c-tw-us': 186,
  'c-mo-sg': null,
  'c-jp-nf': 112,
  'c-us-disney': 214,
};

function e2eProbes(): { chains: E2eProbeItem[] } {
  const end = Math.floor(NOW_S / 60) * 60;
  return {
    chains: CHAIN_APPS.flatMap(app =>
      app.chains.map((chain, index) => {
        const random = noise(seedOf(chain.id) + index);
        const ttfb = CHAIN_TTFB[chain.id] ?? 120;
        const ok = CHAIN_TTFB[chain.id] !== null;
        const points = 360;
        const probed_at_unix_secs = Array.from({ length: points }, (_, point) => end - (points - 1 - point) * 60);
        const status: E2eProbeStatus[] = probed_at_unix_secs.map((_, point) =>
          !ok && point > 180 ? 'chain-broken' : 'ok',
        );
        const ttfb_ms = status.map(value =>
          value === 'ok' ? Math.round((ttfb ?? 120) * (0.82 + random() * 0.36 + (random() > 0.97 ? 0.5 : 0))) : null,
        );
        const steps = (app.steps as unknown as { chain: string; node: string }[]).filter(
          step => step.chain === chain.id,
        );
        const exit = MACHINES.find(machine => machine.id === steps.at(-1)?.node);
        return {
          app_id: app.id,
          chain_id: chain.id,
          chain_name: chain.name,
          node_id: steps[0]?.node ?? '',
          status: ok ? 'ok' : 'chain-broken',
          ttfb_ms: ok ? (ttfb_ms.at(-1) ?? null) : null,
          exit_ip: ok ? (exit?.ipv4 ?? null) : null,
          exit_loc: ok ? (chain.subscription_country ?? null) : null,
          exit_intelligence: null,
          exit_verdict: ok ? 'match' : 'unknown',
          detail: ok ? null : '新加坡 DigitalOcean 失联，链在第二跳断开',
          probed_at: iso(end),
          samples: { probed_at_unix_secs, status, ttfb_ms },
        } as E2eProbeItem;
      }),
    ),
  };
}

/* ══ 外壳 ══ */

const BOOTSTRAP: ConsoleBootstrap = {
  who: { operator_id: 'admin', role: 'system-admin', tenant_scope: null, token_prefix: null, masked_assets: false },
  initial: { node_count: ACTIVE.length, chain_group_count: [], onboarding: 'complete' },
};

const ONBOARDING: OnboardingState = {
  status: 'complete',
  root_tenant_id: 'platform',
  started_at: null,
  node_id: null,
  app_id: null,
  chain_id: null,
  ingress_id: null,
  user_tenant_id: null,
  user_id: null,
  target_revision_id: null,
  deployment_id: null,
} as OnboardingState;

const REVISIONS: RevisionList = {
  current_revision: 41,
  revisions: [41, 40, 39, 38].map((id, index) => ({
    id,
    created_at: iso(NOW_S - index * 2 * DAY - 3 * HOUR),
    author: 'admin',
    note: ['调整东京出口规则', '新增台湾 → 洛杉矶', '轮换 REALITY 密钥', '纳管伦敦家宽'][index],
    status: 'published',
    current: index === 0,
    has_snapshot: true,
  })),
};

function notifications(): MachineEventList {
  return {
    retention_days: 14,
    latest_event_id: 18,
    last_seen_event_id: 16,
    cleared_through_event_id: 0,
    unread_count: 1,
    active: [
      {
        event_id: 18,
        node_id: 'sg-dp',
        node_name: '新加坡 DigitalOcean',
        incident_kind: 'control_plane_offline',
        started_at: iso(NOW_S - 3 * HOUR),
        detected_at: iso(NOW_S - 3 * HOUR + 90),
        last_observed_at: iso(NOW_S - 3 * HOUR),
        current_value: null,
        peak_value: null,
      },
    ],
    events: [
      {
        id: 18,
        node_id: 'sg-dp',
        node_name: '新加坡 DigitalOcean',
        event_kind: 'node_offline',
        family: null,
        previous_value: null,
        current_value: null,
        last_contact_at: iso(NOW_S - 3 * HOUR),
        occurred_at: iso(NOW_S - 3 * HOUR + 90),
      },
      {
        id: 17,
        node_id: 'hk-home',
        node_name: '香港 家宽',
        event_kind: 'public_ip_changed',
        family: 4,
        previous_value: '198.18.0.19',
        current_value: '198.18.0.20',
        last_contact_at: null,
        occurred_at: iso(NOW_S - 9 * HOUR),
      },
    ],
  };
}

function nodeTraffic(): NodeTrafficView {
  const periodStart = NOW_S - 22 * DAY;
  return {
    nodes: ACTIVE.map(machine => {
      const total = Math.round(profileOf(machine.id).mbps * 0.11 * 22 * GiB);
      return {
        node_id: machine.id,
        tenant_id: 'platform',
        name: machine.name,
        cycle_kind: 'monthly',
        accounting_mode: 'bidirectional_sum',
        reset_month: null,
        reset_day: 1,
        quota_bytes: machine.id === 'hk-bgp' ? String(4096 * GiB) : machine.id === 'us-gia' ? String(1024 * GiB) : null,
        period_start_unix_secs: periodStart,
        period_end_unix_secs: periodStart + 31 * DAY,
        rx_bytes: String(Math.round(total * 0.52)),
        tx_bytes: String(Math.round(total * 0.48)),
        total_bytes: String(total),
        interface: 'eth0',
        tracking_started_at_unix_secs: NOW_S - 90 * DAY,
        last_reported_at_unix_secs: NOW_S - 30,
        calibrated_at_unix_secs: null,
        last_gap_at_unix_secs: null,
        last_gap_reason: null,
        has_gap: false,
      };
    }),
  } as NodeTrafficView;
}

/* ══ 用户与用量 ══ */

const TEST_USERS = new Set(['carol']);
const EXPIRED_USERS = new Set(['dave']);
const monthStart = (() => {
  const date = new Date(NOW);
  return Math.floor(Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), 1) / 1000) - 8 * HOUR;
})();
const monthEnd = (() => {
  const date = new Date(NOW);
  return Math.floor(Date.UTC(date.getUTCFullYear(), date.getUTCMonth() + 1, 1) / 1000) - 8 * HOUR;
})();
const elapsedDays = Math.max(1, Math.floor((NOW_S - monthStart) / DAY) + 1);
const USER_GIB: Record<string, number> = {
  alice: 41.2,
  bob: 168.4,
  carol: 12.6,
  dave: 0,
  erin: 38.1,
  frank: 9.7,
  grace: 74.9,
  heidi: 3.2,
  ivan: 121.5,
  judy: 22.4,
  mallory: 0,
  niaj: 6.1,
  olivia: 57.3,
  peggy: 18.8,
};

function userList(includeDisabled: boolean) {
  return {
    users: USERS.filter(user => includeDisabled || user.status !== 'disabled').map(user => ({
      tenant_id: user.tenant,
      id: user.id,
      uuid: `6f1c2d3e-4b5a-4c6d-8e7f-${String(seedOf(user.id)).padStart(12, '0')}`,
      status: user.status ?? 'active',
      account_type: TEST_USERS.has(user.id) ? 'test' : 'formal',
      traffic_accounting_mode: 'single',
      traffic_reset_day: 1,
      service_starts_at: null,
      service_expires_at: EXPIRED_USERS.has(user.id) ? iso(NOW_S - 2 * DAY) : null,
      service_state: EXPIRED_USERS.has(user.id) ? 'expired' : 'active',
      login_enabled: true,
      direct_login_enabled: user.id === 'alice',
      created_at: iso(NOW_S - 80 * DAY),
      created_revision: 3,
    })),
  };
}

/** 用户 → 授权的项目，取自线路快照。 */
function userApps(user: string) {
  return CHAIN_APPS.filter(app => (app.grants as unknown as { user: string }[]).some(grant => grant.user === user)).map(
    app => app.id,
  );
}

function userCycles() {
  return {
    periods: USERS.map(user => ({
      tenant_id: user.tenant,
      user_id: user.id,
      starts_at: iso(monthStart),
      ends_at: iso(monthEnd),
    })),
    views: USERS.flatMap(user =>
      userApps(user.id).map((app, index) => {
        const total = (USER_GIB[user.id] ?? 0) * GiB * (index === 0 ? 0.72 : 0.28);
        return {
          tenant_id: user.tenant,
          user_id: user.id,
          app_id: app,
          uplink_bytes: Math.round(total * 0.04),
          downlink_bytes: Math.round(total * 0.96),
          has_gap: false,
        };
      }),
    ),
    forwarding: [],
  };
}

function quotas() {
  return {
    quotas: USERS.flatMap(user =>
      userApps(user.id)
        .slice(0, 1)
        .map(app => ({
          tenant_id: user.tenant,
          user_id: user.id,
          app_id: app,
          limit_bytes: (user.id === 'bob' ? 200 : user.id === 'ivan' ? 150 : 100) * GiB,
          updated_at: iso(NOW_S - 30 * DAY),
          suspended_ingresses: [],
        })),
    ),
  };
}

function presence() {
  const online = ['alice', 'bob', 'grace', 'ivan', 'carol', 'olivia'];
  return {
    freshness_secs: 120,
    users: USERS.filter(user => user.status !== 'disabled').map(user => {
      const nodes = [...grantsByNode()].filter(([, users]) => users.includes(user.id)).map(([node]) => node);
      const index = online.indexOf(user.id);
      return {
        tenant_id: user.tenant,
        user_id: user.id,
        state: 'complete',
        expected_nodes: nodes.length,
        reporting_nodes: nodes.length,
        sources:
          index < 0
            ? []
            : Array.from({ length: index % 3 === 0 ? 2 : 1 }, (_, source) => ({
                ip: `113.${88 + index}.${17 + source * 40}.${index * 13 + 4}`,
                first_observed_at: iso(NOW_S - (index + 1) * 5 * HOUR),
                last_observed_at: iso(NOW_S - 10),
                xray_last_seen_at: iso(NOW_S - 10),
                online_since_at: iso(NOW_S - (index + 1) * 47 * MINUTE),
                session_uplink_bytes: Math.round((index + 1) * 0.04 * GiB),
                session_downlink_bytes: Math.round((index + 1) * 1.3 * GiB),
                node_ids: nodes.slice(0, 1),
                ingress_ids: [],
              })),
      };
    }),
    source_countries: [],
    source_operators: [],
  };
}

function monthly(offset: string | null) {
  const previous = offset === '-1';
  const start = previous ? monthStart - 30 * DAY : monthStart;
  const days = previous ? 30 : elapsedDays;
  const daily = Array.from({ length: days }, (_, index) => {
    const downlink = Math.round((22 + 9 * Math.sin(index / 2.3) + (index % 7 === 5 ? 14 : 0)) * GiB);
    return {
      day: new Date((start + 8 * HOUR + index * DAY) * 1000).toISOString().slice(0, 10),
      uplink_bytes: Math.round(downlink * 0.06),
      downlink_bytes: downlink,
      has_gap: false,
    };
  });
  return {
    month_start: new Date((start + 8 * HOUR) * 1000).toISOString().slice(0, 10) + ' 00:00:00',
    month_end: new Date((monthEnd + 8 * HOUR) * 1000).toISOString().slice(0, 10) + ' 00:00:00',
    views: userCycles().views,
    days: daily,
  };
}

function nodeSeries(url: URL) {
  const { start, end } = range(url, 1800);
  const only = url.searchParams.get('node_id');
  // 生产接口按 30 秒窗口上报；图表以这个周期识别断档。
  const step = 30;
  const from = Math.max(start, end - DAY);
  return {
    since: iso(start),
    month_start: iso(monthStart),
    nodes: ACTIVE.filter(machine => !only || machine.id === only).map(machine => {
      const buckets = [];
      if (!OFFLINE.has(machine.id)) {
        for (let t = Math.ceil(from / step) * step; t <= end; t += step) {
          const nic = nicAt(machine.id, t);
          const user = 0.82;
          buckets.push({
            window_end: iso(t),
            user_uplink_bytes: Math.round(nic.tx * step * user * 0.05),
            user_downlink_bytes: Math.round(nic.rx * step * user),
            relay_uplink_bytes: Math.round(nic.tx * step * (1 - user) * 0.5),
            relay_downlink_bytes: Math.round(nic.rx * step * (1 - user) * 0.5),
          });
        }
      }
      const month = profileOf(machine.id).mbps * 0.11 * elapsedDays * GiB;
      return {
        node_id: machine.id,
        buckets,
        month_user_uplink_bytes: Math.round(month * 0.04),
        month_user_downlink_bytes: Math.round(month * 0.78),
        month_relay_uplink_bytes: Math.round(month * 0.09),
        month_relay_downlink_bytes: Math.round(month * 0.09),
        month_has_gap: false,
      };
    }),
  };
}

function usageSamples() {
  const end = Math.floor(NOW_S / 60) * 60;
  let id = 1;
  return {
    samples: CHAIN_APPS.flatMap(app =>
      (app.grants as unknown as { tenant: string; user: string; ingress: string }[]).map(grant => {
        const ingress = app.ingresses.find(item => item.id === grant.ingress) as unknown as { node: string };
        const flow = FLOWS.find(item => item.user === grant.user && item.node === ingress.node);
        const down = flow ? flow.down * 60 : (seedOf(grant.user + grant.ingress) % 7) * 0.02 * MB * 60;
        return {
          id: id++,
          sampled_at: iso(end),
          window_start: iso(end - 60),
          window_end: iso(end),
          node_id: ingress.node,
          tenant_id: grant.tenant,
          user_id: grant.user,
          ingress_id: grant.ingress,
          grant_label: `${grant.user}@${grant.ingress}`,
          uplink_bytes: Math.round(down * 0.04),
          downlink_bytes: Math.round(down),
          has_gap: false,
          revision_id: 41,
          deployment_id: null,
        };
      }),
    ),
    chain_samples: chainSamples(),
  };
}

/* ══ 编译 IR（拓扑页） ══ */

interface StepSeed {
  chain: string;
  node: string;
}

function chainPaths() {
  return CHAIN_APPS.flatMap(app =>
    app.chains.map(chain => ({
      app: app.id,
      chain: chain.id,
      path: (app.steps as unknown as StepSeed[]).filter(step => step.chain === chain.id).map(step => step.node),
    })),
  );
}

const overlayOf = (id: string) => `10.88.0.${ACTIVE.findIndex(machine => machine.id === id) + 1}`;
const linkOf = (a: string, b: string) => (a < b ? `${a}~${b}` : `${b}~${a}`);

function compileView() {
  const pairs = new Set<string>();
  for (const { path } of chainPaths()) for (let i = 1; i < path.length; i += 1) pairs.add(linkOf(path[i - 1], path[i]));
  for (const [a, b] of [
    ['hk-hkix', 'hk-bgp'],
    ['hk-bgp', 'jp-ntt'],
    ['hk-hkix', 'tw-hinet'],
    ['jp-iij', 'us-gia'],
    ['hk-home', 'hk-hkix'],
    ['de-fra', 'gb-lon'],
  ])
    pairs.add(linkOf(a, b));
  const system = {
    revision: 41,
    overlay_cidr: '10.88.0.0/16',
    node_count: ACTIVE.length,
    nodes: ACTIVE.map(machine => ({
      id: machine.id,
      tenant: 'platform',
      overlay_addr: overlayOf(machine.id),
      mtu: 1420,
      wireguard: { endpoint: machine.nat || !machine.ipv4 ? null : `${machine.ipv4}:51820` },
    })),
    links: [...pairs].map(id => {
      const [a, b] = id.split('~');
      return { id, a, b, dial: 'both', keepalive_secs: 25, wrap: { t: 'udp' } };
    }),
  };
  const apps = CHAIN_APPS.map(app => {
    const paths = chainPaths().filter(item => item.app === app.id);
    const nodes = [...new Set(paths.flatMap(item => item.path))];
    return {
      app_id: app.id,
      nodes: nodes.map(id => ({
        id,
        name: MACHINES.find(machine => machine.id === id)?.name ?? id,
        egress_allowed: true,
      })),
      chains: app.chains.map(chain => ({
        id: chain.id,
        name: chain.name,
        root: paths.find(item => item.chain === chain.id)?.path[0] ?? null,
      })),
      ingresses: (app.ingresses as unknown as { id: string; chain: string; node: string; port: number }[]).map(
        item => ({
          id: item.id,
          chain: item.chain,
          node: item.node,
          port: item.port,
          front: null,
        }),
      ),
      fronts: [],
      hops: paths.flatMap((item, index) =>
        item.path.slice(1).map((to, hop) => ({
          chain: item.chain,
          from: item.path[hop],
          to,
          link: linkOf(item.path[hop], to),
          address: overlayOf(to),
          port: 20000 + index,
          path: 'overlay',
          security: { t: 'encryption' },
        })),
      ),
      grants: (app.grants as unknown as { tenant: string; user: string; ingress: string }[]).map(grant => ({
        tenant: grant.tenant,
        user: grant.user,
        ingress: grant.ingress,
      })),
    };
  });
  return {
    revision: 41,
    summary: { errors: 0, warnings: 0, infos: 2, can_publish: true },
    diagnostics: [],
    system,
    apps,
    redacted: false,
  };
}

function chainSamples() {
  const end = Math.floor(NOW_S / 60) * 60;
  let id = 1;
  return chainPaths().flatMap(({ app, chain, path }) =>
    path.slice(1).map((node, hop) => {
      const bytes = (profileOf(path[0]).mbps * 0.3 + hop * 3) * MB * 60;
      return {
        id: id++,
        sampled_at: iso(end),
        window_start: iso(end - 60),
        window_end: iso(end),
        node_id: node,
        tenant_id: 'platform',
        app_id: app,
        chain_id: chain,
        hop_label: `${chain}@${node}`,
        uplink_bytes: Math.round(bytes * 0.05),
        downlink_bytes: Math.round(bytes),
        has_gap: false,
        revision_id: 41,
        deployment_id: null,
      };
    }),
  );
}

/* ══ 机器详情 ══ */

function loadOverview(id: string, start: number, end: number) {
  const view = loadView(id, start, end);
  const column = <K extends keyof LoadSample>(key: K) => view.series.map(sample => sample[key]);
  return {
    ...view,
    series: {
      window_start_unix_secs: column('window_start_unix_secs'),
      window_end_unix_secs: column('window_end_unix_secs'),
      has_gap: column('has_gap'),
      cpu_user_pct: column('cpu_user_pct'),
      cpu_sys_pct: column('cpu_sys_pct'),
      cpu_softirq_pct: column('cpu_softirq_pct'),
      cpu_peak_pct: column('cpu_peak_pct'),
      cpu_steal_pct: column('cpu_steal_pct'),
      load1: column('load1'),
      mem_available_bytes: column('mem_available_bytes'),
      swap_used_bytes: column('swap_used_bytes'),
      oom_kills: column('oom_kills'),
      disk_free_bytes: column('disk_free_bytes'),
      disk_inode_free_pct: column('disk_inode_free_pct'),
      nic_rx_bps: column('nic_rx_bps'),
      nic_tx_bps: column('nic_tx_bps'),
      nic_rx_drop: column('nic_rx_drop'),
      nic_tx_drop: column('nic_tx_drop'),
      nic_err: column('nic_err'),
      conntrack_count: column('conntrack_count'),
      uptime_secs: column('uptime_secs'),
    },
  };
}

function loadMetrics(id: string, start: number, end: number, metrics: string[]) {
  const view = loadView(id, start, end);
  return {
    node_id: id,
    range_start_unix_secs: start,
    range_end_unix_secs: end,
    window_end_unix_secs: view.series.map(sample => sample.window_end_unix_secs),
    has_gap: view.series.map(sample => sample.has_gap),
    metrics: Object.fromEntries(
      metrics.map((metric, index) => [
        metric,
        view.series.map(sample => {
          const busy = sample.nic_rx_bps / (profileOf(id).mbps * 8 * MB || 1);
          return Math.max(0, Math.round((busy * (40 + index * 17) + (index % 3) * 4) * 10) / 10);
        }),
      ]),
    ),
  };
}

function forwardingOverview(tenant: string, user: string) {
  return { tenant_id: tenant, user_id: user, entitlements: [], rules: [] };
}

/* ══ 路由 ══ */

type Handler = (url: URL, init?: RequestInit) => unknown;
const num = (url: URL, key: string) => Number(url.searchParams.get(key));
const range = (url: URL, fallback = DAY) => {
  const start = num(url, 'start_unix_secs');
  const end = num(url, 'end_unix_secs');
  if (Number.isFinite(start) && Number.isFinite(end) && end > start) return { start, end };
  const windows = num(url, 'windows');
  if (windows > 0) return { start: NOW_S - windows * 60, end: NOW_S };
  const span = num(url, 'window_secs') || fallback;
  return { start: NOW_S - span, end: NOW_S };
};

const ROUTES: [RegExp, Handler][] = [
  [/^\/branding$/, () => ({ site_name: 'Brocade', icon_data_url: null })],
  [/^\/auth\/state$/, () => ({ initialized: true, public_open: false })],
  [/^\/bootstrap$/, () => BOOTSTRAP],
  [/^\/whoami$/, () => BOOTSTRAP.who],
  [/^\/onboarding$/, () => ONBOARDING],
  [/^\/revisions$/, () => REVISIONS],
  [/^\/compile\/\d+$/, compileView],
  [
    /^\/deployments\/verify$/,
    () => ({
      revision_id: 41,
      converged: true,
      summary: {
        total_targets: 12,
        changed_targets: 0,
        skipped_targets: 12,
        deferred_targets: 0,
        disruptive_targets: 0,
        max_wave: 0,
      },
      targets: [],
    }),
  ],
  [/^\/deployments$/, () => ({ deployments: [] })],
  [
    /^\/grants\/automation$/,
    () => ({
      pending_jobs: 0,
      retrying_jobs: 0,
      failed_jobs: 0,
      max_attempts: 5,
      latest_revision_id: 41,
      oldest_pending_at: null,
      last_attempt_at: null,
      last_error: null,
    }),
  ],
  [/^\/notifications$/, notifications],
  [/^\/nodes\/agent-state$/, () => ({ nodes: fullAgentNodes() })],
  [/^\/node-traffic$/, nodeTraffic],
  [/^\/load\/nodes\/nic$/, url => nicList(Math.max(1, num(url, 'windows') || 24))],
  [
    /^\/load\/nodes$/,
    url => {
      const { start, end } = range(url);
      return { nodes: ACTIVE.map(machine => loadView(machine.id, start, end)) };
    },
  ],
  [
    /^\/load\/nodes\/[^/]+\/overview$/,
    url => {
      const { start, end } = range(url);
      return loadOverview(decodeURIComponent(url.pathname.split('/')[3]), start, end);
    },
  ],
  [
    /^\/load\/nodes\/[^/]+\/metrics$/,
    url => {
      const { start, end } = range(url);
      const metrics = (url.searchParams.get('metrics') ?? '').split(',').filter(Boolean);
      return loadMetrics(decodeURIComponent(url.pathname.split('/')[3]), start, end, metrics);
    },
  ],
  [
    /^\/nodes\/[^/]+\/public-ip-history$/,
    url => ({
      node_id: decodeURIComponent(url.pathname.split('/')[2]),
      visible_days: 14,
      retention_days: 30,
      events: [],
    }),
  ],
  [/^\/links\/health$/, () => ({ hops: [] })],
  [
    /^\/load\/nodes\/[^/]+$/,
    url => {
      const { start, end } = range(url);
      return loadView(decodeURIComponent(url.pathname.split('/')[3]), start, end);
    },
  ],
  [/^\/ping-probe\/nodes\/latest$/, pingLatest],
  [
    /^\/ping-probe\/nodes\/[^/]+\/series$/,
    url => {
      const { start, end } = range(url);
      return pingSeries(decodeURIComponent(url.pathname.split('/')[3]), start, end);
    },
  ],
  [
    /^\/ping-probe\/settings$/,
    () => ({
      targets: PING_TARGETS.map(({ name, kind, ipv4, ipv6 }) => ({ name, kind, ipv4, ipv6 })),
      interval_secs: 60,
      timeout_ms: 420,
    }),
  ],
  [/^\/model\/snapshot$/, chainSnapshot],
  [/^\/probes\/e2e$/, e2eProbes],
  [/^\/realtime\/fleet-traffic\/snapshot$/, fleetTrafficSnapshot],
  [/^\/users$/, url => userList(url.searchParams.get('include_disabled') !== 'false')],
  [/^\/users\/presence$/, presence],
  [
    /^\/tenants$/,
    () => ({
      tenants: [
        { id: 'platform', name: '平台' },
        { id: 'studio', name: '工作室' },
      ],
    }),
  ],
  [/^\/usage\/user-cycles$/, userCycles],
  [/^\/quotas$/, quotas],
  [/^\/usage\/monthly-summary$/, url => monthly(url.searchParams.get('month_offset'))],
  [/^\/usage\/node-series$/, nodeSeries],
  [/^\/usage\/samples$/, usageSamples],
  [
    /^\/users\/[^/]+\/[^/]+\/forwarding$/,
    url => forwardingOverview(url.pathname.split('/')[2], url.pathname.split('/')[3]),
  ],
  [
    /^\/users\/[^/]+\/[^/]+\/presence-history$/,
    url => ({
      tenant_id: url.pathname.split('/')[2],
      user_id: url.pathname.split('/')[3],
      retention_days: 30,
      truncated: false,
      sources: [],
    }),
  ],
  [/\/tcp-pings$/, () => []],
  [/^\/grant-probes\/capability$/, () => ({ available: false, reason: '示例不拨测' })],
];

export const misses = new Set<string>();

export function installBackend() {
  globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const raw = input instanceof Request ? input.url : String(input);
    const url = new URL(raw, location.origin);
    const hit = ROUTES.find(([pattern]) => pattern.test(url.pathname));
    if (hit) return Response.json(hit[1](url, init));
    const key = `${init?.method ?? 'GET'} ${url.pathname}${url.search}`;
    if (!misses.has(key)) console.info('[mock miss]', key);
    misses.add(key);
    return Response.json({ error: `示例未提供 ${url.pathname}` }, { status: 404 });
  };
  (globalThis as { EventSource: unknown }).EventSource = FixtureEventSource;
}
