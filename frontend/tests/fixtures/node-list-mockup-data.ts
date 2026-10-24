// Synthetic fleet for the machine list (机器页) header and layout mockup. Never imported by the app.
// Names follow the production pattern 「城市 线路」; addresses are documentation ranges.
import type {
  LinkHealthItem,
  NodeAgentStateItem,
  NodeNicSample,
  NodeNicView,
  NodePingProbeLatestView,
  NodeTrafficItem,
} from '../../src/api';

export const WINDOW_SECS = 30;
const GiB = 1024 ** 3;
const TiB = 1024 ** 4;
export const PROBE_TARGETS = ['上海电信', '北京联通', '广州移动'] as const;

export type MachineState =
  'ok' | 'offline' | 'wg' | 'isolated' | 'never' | 'reconcile' | 'retiring' | 'retired' | 'cleanup';

export interface Machine {
  id: string;
  name: string;
  country: string;
  ipv4: string | null;
  ipv6?: string | null;
  nat?: boolean;
  /** Typical NIC rate (RX + TX) in Mbit/s. */
  mbps: number;
  /** Month-to-date NIC total in GiB. */
  monthGiB: number;
  quotaTiB?: number | null;
  /** Latest TCP latency per probe target in ms; null = no reply in the latest round. */
  ping: (number | null)[];
  state?: MachineState;
  /** Seconds since the last poll, for offline machines. */
  offlineSecs?: number;
  /** Chains this machine takes part in: [entry, relay, exit]. */
  roles?: [number, number, number];
}

// prettier-ignore
const FLEET: Machine[] = [
  { id: 'hk-hkix', name: '香港 HKIX', country: 'HK', ipv4: '203.0.113.11', ipv6: '2001:db8:11::1', mbps: 6.2, monthGiB: 612, quotaTiB: 2, ping: [38, 41, 35], roles: [3, 1, 0] },
  { id: 'hk-cmi', name: '香港 CMI', country: 'HK', ipv4: '203.0.113.12', mbps: 2.1, monthGiB: 316, quotaTiB: 1, ping: [12, 14, 9], roles: [2, 0, 1] },
  // GeoIP 把这台判成美国：旗帜跟着 IP 库走，名字是纳管时写的。生产里确有这种情况。
  { id: 'hk-bgp', name: '香港 BGP 三网', country: 'US', ipv4: '203.0.113.13', mbps: 2.3, monthGiB: 253, ping: [177, 168, null], roles: [1, 0, 0] },
  { id: 'hk-home', name: '香港 家宽', country: 'HK', ipv4: '198.18.0.20', nat: true, mbps: 0.4, monthGiB: 37, ping: [44, 47, 52], roles: [0, 0, 2] },
  { id: 'mo-ctm', name: '澳门 CTM', country: 'MO', ipv4: '203.0.113.31', mbps: 1.3, monthGiB: 15, quotaTiB: 1, ping: [41, 46, 39], roles: [1, 0, 1] },
  { id: 'mo-ctm-2', name: '澳门 CTM II', country: 'MO', ipv4: '203.0.113.32', mbps: 0.05, monthGiB: 91, ping: [43, null, 40], roles: [0, 0, 1] },
  { id: 'tw-hinet', name: '台北 HiNet', country: 'TW', ipv4: '203.0.113.41', mbps: 0.8, monthGiB: 326, quotaTiB: 1, ping: [58, 61, 66], roles: [0, 1, 2] },
  { id: 'tw-tfn', name: '台北 TFN', country: 'TW', ipv4: '203.0.113.42', mbps: 0.2, monthGiB: 17, quotaTiB: 1, ping: [64, 70, 72], state: 'isolated', roles: [0, 0, 1] },
  { id: 'jp-iij', name: '东京 IIJ', country: 'JP', ipv4: '198.51.100.51', ipv6: '2001:db8:51::1', mbps: 2.1, monthGiB: 900, quotaTiB: 1, ping: [73, 77, 80], roles: [0, 2, 3] },
  { id: 'jp-ntt', name: '东京 NTT', country: 'JP', ipv4: '198.51.100.52', mbps: 2.1, monthGiB: 15, ping: [79, 84, 90], roles: [0, 0, 1] },
  { id: 'jp-sb', name: '东京 SoftBank', country: 'JP', ipv4: '198.51.100.53', mbps: 5.8, monthGiB: 369, quotaTiB: 2, ping: [88, 95, 84], state: 'wg', roles: [0, 1, 2] },
  { id: 'jp-osa', name: '大阪 IIJ', country: 'JP', ipv4: '198.51.100.54', mbps: 0.1, monthGiB: 104, quotaTiB: 0.1, ping: [61, 66, 70], roles: [0, 0, 1] },
  { id: 'jp-bgp', name: '东京 BGP 优化', country: 'JP', ipv4: '198.51.100.55', mbps: 3.8, monthGiB: 55, quotaTiB: 0.3, ping: [72, 74, null], roles: [2, 0, 0] },
  { id: 'kr-kt', name: '首尔 KT', country: 'KR', ipv4: '198.51.100.56', mbps: 0.6, monthGiB: 48, ping: [56, 60, 62], roles: [0, 0, 1] },
  { id: 'sg-dp', name: '新加坡 DigitalOcean', country: 'SG', ipv4: '198.51.100.61', mbps: 0.4, monthGiB: 62, ping: [88, 92, 79], roles: [0, 0, 1] },
  { id: 'my-jb', name: '新山 BGP', country: 'MY', ipv4: '198.51.100.62', mbps: 0.1, monthGiB: 61, ping: [328, 253, null], roles: [0, 0, 1] },
  { id: 'us-gia', name: '洛杉矶 CN2 GIA', country: 'US', ipv4: '192.0.2.71', mbps: 0.3, monthGiB: 638, quotaTiB: 2, ping: [166, 164, 170], roles: [0, 0, 2] },
  { id: 'us-9929', name: '洛杉矶 9929', country: 'US', ipv4: '192.0.2.72', mbps: 0.7, monthGiB: 297, quotaTiB: 1.68, ping: [null, 196, 201], state: 'offline', offlineSecs: 380, roles: [0, 0, 1] },
  { id: 'us-cmin2', name: '洛杉矶 CMIN2', country: 'US', ipv4: '192.0.2.73', mbps: 0.4, monthGiB: 216, quotaTiB: 0.78, ping: [162, 158, 157], roles: [0, 0, 1] },
  { id: 'us-sjc', name: '圣何塞 标准', country: 'US', ipv4: '192.0.2.74', mbps: 1.2, monthGiB: 418, quotaTiB: 10, ping: [185, 188, 175], roles: [0, 0, 1] },
  { id: 'us-sea', name: '西雅图 标准', country: 'US', ipv4: '192.0.2.75', mbps: 0.1, monthGiB: 244, quotaTiB: 1, ping: [229, 217, 240], roles: [0, 0, 1] },
  { id: 'de-fra', name: '法兰克福 标准', country: 'DE', ipv4: '192.0.2.81', mbps: 0.4, monthGiB: 30, ping: [208, 247, 233], roles: [0, 0, 1] },
  { id: 'gb-lon', name: '伦敦 家宽', country: 'GB', ipv4: null, ipv6: '2001:db8:91::1', mbps: 0.2, monthGiB: 40, quotaTiB: 0.5, ping: [195, 158, 201], roles: [0, 0, 1] },
  { id: 'nl-ams', name: '阿姆斯特丹 标准', country: 'NL', ipv4: '192.0.2.83', mbps: 0, monthGiB: 0, ping: [null, null, null], state: 'never' },
];

// prettier-ignore
const RETIRED: Machine[] = [
  { id: 'hk-old', name: '香港 旧 BGP', country: 'HK', ipv4: '203.0.113.19', mbps: 0, monthGiB: 12, ping: [], state: 'retiring' },
  { id: 'kr-old', name: '首尔 旧机', country: 'KR', ipv4: '192.0.2.99', mbps: 0, monthGiB: 0, ping: [], state: 'retired' },
  { id: 'us-old', name: '洛杉矶 旧 GIA', country: 'US', ipv4: '192.0.2.98', mbps: 0, monthGiB: 0, ping: [], state: 'retired' },
];

const withState = (list: Machine[], states: Record<string, Partial<Machine>>) =>
  list.map(machine => ({ ...machine, state: 'ok' as MachineState, ...states[machine.id] }));

export type SceneKey = 'typical' | 'calm' | 'trouble' | 'few' | 'many' | 'empty';

export const SCENES: Record<SceneKey, { label: string; machines: () => Machine[] }> = {
  typical: { label: '典型 · 24 台 + 3 台退役', machines: () => [...FLEET, ...RETIRED] },
  calm: {
    label: '全部正常 · 24 台',
    machines: () =>
      FLEET.map(machine => ({
        ...machine,
        state: 'ok',
        mbps: machine.state === 'never' ? 0.6 : machine.mbps,
        monthGiB: machine.state === 'never' ? 21 : machine.monthGiB,
        ping: machine.state === 'never' ? [212, 230, 218] : machine.ping,
      })),
  },
  trouble: {
    label: '多处异常',
    machines: () => [
      ...withState(FLEET, {
        'hk-cmi': { state: 'offline', offlineSecs: 95 },
        'us-9929': { state: 'offline', offlineSecs: 4 * 3600 },
        'de-fra': { state: 'offline', offlineSecs: 2 * 86400 },
        'jp-sb': { state: 'wg' },
        'jp-iij': { state: 'wg' },
        'sg-dp': { state: 'reconcile' },
        'tw-tfn': { state: 'isolated' },
        'nl-ams': { state: 'never' },
      }),
      ...RETIRED,
      { ...RETIRED[0], id: 'jp-old', name: '东京 旧 NTT', country: 'JP', state: 'cleanup' },
    ],
  },
  few: {
    label: '三台',
    machines: () =>
      withState(
        FLEET.filter(m => ['hk-hkix', 'jp-iij', 'us-gia'].includes(m.id)),
        {},
      ),
  },
  many: {
    label: '48 台',
    machines: () => [
      ...FLEET,
      ...FLEET.map(machine => ({
        ...machine,
        id: `${machine.id}-b`,
        name: `${machine.name} II`.replace(' II II', ' III'),
        ipv4: machine.ipv4 ? machine.ipv4.replace(/\.(\d+)$/, (_, last: string) => `.${Number(last) + 100}`) : null,
        mbps: machine.mbps * 0.7,
        monthGiB: machine.monthGiB * 0.6,
        state: machine.state === 'never' || machine.state === 'offline' ? 'ok' : machine.state,
        ping: machine.state === 'never' ? [200, 210, 190] : machine.ping,
      })),
      ...RETIRED,
    ],
  },
  empty: { label: '还没有机器', machines: () => [] },
};

/* ══ 由规格生成接口返回值 ══ */

/** Deterministic noise in [0, 1). */
function noise(seed: number): number {
  const x = Math.sin(seed * 12.9898 + 78.233) * 43758.5453;
  return x - Math.floor(x);
}

const hash = (text: string) => [...text].reduce((sum, char) => (sum * 31 + char.charCodeAt(0)) % 9973, 7);

const iso = (unixSecs: number) => new Date(unixSecs * 1000).toISOString();

const retiredState = (state: MachineState | undefined) =>
  state === 'retiring' || state === 'retired' || state === 'cleanup';

/** Machines that report: everything except never-polled and retired ones. */
export const reports = (machine: Machine) => machine.state !== 'never' && !retiredState(machine.state);

export function agentState(machine: Machine, nowSecs: number): NodeAgentStateItem {
  const seed = hash(machine.id);
  const retired = retiredState(machine.state);
  const offline = machine.state === 'offline';
  const pollAge = offline ? (machine.offlineSecs ?? 300) : 2 + (seed % 11);
  const polled = machine.state !== 'never' && !(machine.state === 'retired' || machine.state === 'cleanup');
  return {
    node_id: machine.id,
    tenant_id: 'platform',
    name: machine.name,
    public_ipv4: machine.ipv4,
    public_ipv4_country: machine.country,
    public_ipv4_country_detected: machine.country,
    public_ipv4_country_override: null,
    public_ipv6: machine.ipv6 ?? null,
    public_ipv4_nat: machine.nat ?? false,
    public_ipv6_nat: false,
    route_ipv4: machine.ipv4,
    route_ipv6: machine.ipv6 ?? null,
    token_prefix: machine.state === 'never' ? 'bk_9f3a' : 'bk_2c41',
    token_created_at: '2026-09-02T08:00:00Z',
    token_last_used_at: polled ? iso(nowSecs - pollAge) : null,
    token_revoked_at: null,
    agent_version: polled ? 'brocade-agent/0.3.2' : null,
    agent_protocol_version: polled ? 45 : null,
    vpngate_probe_enabled: false,
    runtime_versions: polled
      ? {
          agent: 'brocade-agent/0.3.2',
          xray: 'Xray 26.9.12 (Brocade)',
          phantun: null,
          wg_tools: 'wireguard-tools v1.0.20210914',
          wg_backend: 'kernel',
        }
      : null,
    spool_backlog: polled ? { observation: 0, usage: 0, load: 0, dropped: 0, usage_dropped: 0 } : null,
    last_local_reconcile:
      machine.state === 'reconcile'
        ? { at: nowSecs - 120, actions: [], error: 'nft 表 brocade_hop 被外部改写，重建失败' }
        : polled
          ? { at: nowSecs - 600, actions: [], error: null }
          : null,
    wireguard_health: polled
      ? {
          enabled: true,
          error: null,
          peers:
            machine.state === 'wg'
              ? [
                  {
                    peer_node_id: 'hk-hkix',
                    overlay_ip: '10.88.0.11',
                    handshake_age_secs: 412,
                    status: 'down',
                    detail: '握手超过 180 秒',
                  },
                ]
              : [
                  {
                    peer_node_id: 'hk-hkix',
                    overlay_ip: '10.88.0.11',
                    handshake_age_secs: 21,
                    status: 'up',
                    detail: null,
                  },
                ],
        }
      : null,
    runtime_reported_at: polled ? iso(nowSecs - pollAge - 40) : null,
    runtime_report_fresh: polled && !offline,
    geodata_observed: null,
    last_poll_at: polled ? iso(nowSecs - pollAge) : null,
    desired_poll_fresh: polled && !offline,
    last_usage_report_at: polled ? iso(nowSecs - pollAge) : null,
    usage_last_result: polled
      ? { accepted_readings: 18, inserted_samples: 18, skipped_counters: 0, rejected_counters: 0, gap_samples: 0 }
      : null,
    xray_started_at: polled ? iso(nowSecs - 6 * 86400) : null,
    mtu: null,
    connection: {} as NodeAgentStateItem['connection'],
    overlay: true,
    egress_allowed: true,
    dns: { t: 'system' },
    domain_strategy: 'use_ip',
    retired_at: retired ? '2026-10-02T03:00:00Z' : null,
    lifecycle_phase: machine.state === 'retiring' ? 'retiring' : retired ? 'retired' : 'active',
    lifecycle_epoch: 1,
    lifecycle_deployment_id: machine.state === 'retiring' ? 58 : null,
    lifecycle_completed_at: machine.state === 'retired' || machine.state === 'cleanup' ? '2026-10-02T03:10:00Z' : null,
    lifecycle_last_error: machine.state === 'cleanup' ? 'WARP 账户注销超时' : null,
    operationally_isolated: machine.state === 'isolated',
    isolated_at: machine.state === 'isolated' ? iso(nowSecs - 3 * 3600) : null,
    convergence_debt_count: machine.state === 'isolated' ? 2 : 0,
    convergence_debt_failed: false,
    service_reentry_ready: false,
    service_reentry_blockers: [],
    wg_transport_kind: 'udp',
    wg_fake_tcp_port: null,
    initialization:
      machine.state === 'never' ? { state: 'waiting-agent', revision_id: 41, deployment_id: null, reason: null } : null,
    applied: polled
      ? {
          phantun: { state: 'disabled' },
          wireguard: { state: 'present', sha256: 'a1' },
          xray: { state: 'present', sha256: 'b2' },
          hy2_port_hop: { state: 'disabled' },
          grants: { state: 'present' },
          source_deployment_id: 41,
          observed_at: iso(nowSecs - pollAge),
        }
      : null,
  } as NodeAgentStateItem;
}

/** The latest `count` 30-second NIC windows ending at `endSecs`. */
export function nicSeries(machine: Machine, endSecs: number, count: number): NodeNicSample[] {
  if (!reports(machine)) return [];
  const seed = hash(machine.id);
  const stop = machine.state === 'offline' ? endSecs - (machine.offlineSecs ?? 300) : endSecs;
  const last = Math.floor(stop / WINDOW_SECS) * WINDOW_SECS;
  return Array.from({ length: count }, (_, index) => {
    const end = last - (count - 1 - index) * WINDOW_SECS;
    const slot = end / WINDOW_SECS;
    const wave = 0.55 + 0.35 * Math.sin(slot / 9 + seed) + 0.25 * Math.sin(slot / 3.3 + seed * 2);
    const burst = noise(seed + slot) > 0.93 ? 2.2 + noise(slot) * 3 : 1;
    const total = Math.max(0.01, machine.mbps * wave * burst) * 1e6;
    const rxShare = 0.62 + 0.2 * noise(seed);
    return {
      window_start_unix_secs: end - WINDOW_SECS,
      window_end_unix_secs: end,
      has_gap: false,
      nic_rx_bps: Math.round(total * rxShare),
      nic_tx_bps: Math.round(total * (1 - rxShare)),
    };
  });
}

export function nicView(machine: Machine, endSecs: number, count: number): NodeNicView {
  return { node_id: machine.id, series: nicSeries(machine, endSecs, count) };
}

export function trafficItem(machine: Machine, nowSecs: number): NodeTrafficItem {
  const now = new Date(nowSecs * 1000);
  const start = Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1) / 1000;
  const end = Date.UTC(now.getUTCFullYear(), now.getUTCMonth() + 1, 1) / 1000;
  // 月累计带上零头：整 GiB 的读数一眼看得出是造的。
  const total = machine.monthGiB > 0 ? Math.round((machine.monthGiB + (hash(machine.id) % 97) / 97) * GiB) : 0;
  const rx = Math.round(total * 0.68);
  const measured = machine.state !== 'never' && machine.state !== 'retired';
  return {
    node_id: machine.id,
    tenant_id: 'platform',
    name: machine.name,
    cycle_kind: 'monthly',
    accounting_mode: 'bidirectional_sum',
    reset_month: null,
    reset_day: 1,
    quota_bytes: machine.quotaTiB ? String(Math.round(machine.quotaTiB * TiB)) : null,
    period_start_unix_secs: start,
    period_end_unix_secs: end,
    rx_bytes: String(rx),
    tx_bytes: String(total - rx),
    total_bytes: String(total),
    interface: 'eth0',
    tracking_started_at_unix_secs: measured ? start : null,
    last_reported_at_unix_secs: measured ? nowSecs - 20 : null,
    calibrated_at_unix_secs: null,
    last_gap_at_unix_secs: null,
    last_gap_reason: null,
    has_gap: false,
  };
}

export function pingLatest(machine: Machine, nowSecs: number): NodePingProbeLatestView | null {
  if (!reports(machine)) return null;
  const at = machine.state === 'offline' ? nowSecs - (machine.offlineSecs ?? 300) : nowSecs - 6;
  return {
    node_id: machine.id,
    targets: PROBE_TARGETS.map((name, index) => {
      const ms = machine.ping[index];
      return {
        name,
        kind: 'tcp' as const,
        ipv4: {
          address: `tcp://203.0.113.${200 + index}:443`,
          latest: { probed_at_unix_secs: at, attempted: true, latency_us: ms == null ? null : Math.round(ms * 1000) },
        },
        ipv6: null,
      };
    }),
  };
}

export function linkHealth(machines: Machine[], nowSecs: number): LinkHealthItem[] {
  return machines
    .filter(reports)
    .slice(0, 6)
    .map(machine => ({
      node_id: machine.id,
      chain_id: 'apac-main',
      peer_node_id: 'hk-hkix',
      alive: true,
      downlink_bytes: 1_240_000,
      window_secs: 30,
      checked_at: iso(nowSecs - 8),
    }));
}

/** Month totals and quota for the header readouts. */
export const monthBytesOf = (machine: Machine) => Math.round(machine.monthGiB * GiB);
