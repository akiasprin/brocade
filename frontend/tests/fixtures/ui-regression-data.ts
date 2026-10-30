import type {
  ConsoleSnapshot,
  ExternalOutbound,
  HostFacts,
  LoadSample,
  NodeAgentStateItem,
  NodeLoadView,
  SnapshotIngress,
  SnapshotStep,
} from '../../src/api';
import { AGENT_PROTOCOL_VERSION, DEFAULT_HOP_MUX } from '../../src/api';
import { appIdFromBytes, modelIdPairFromBytes, warpTunnelIdFromBytes } from '../../src/model-id';
export const appId = appIdFromBytes(new Uint8Array([1, 2]));
export const pairs = [1, 2].map(i => modelIdPairFromBytes(new Uint8Array([i, 0, 0, i])));
export const now = Date.UTC(2026, 9, 25, 12);
export const nodes = Array.from({ length: 12 }, (_, i) => ({
  node_id: `node-${i}`,
  tenant_id: 'platform',
  name: `东京 / Singapore 多地域备援机器 ${i} · ABCDEFGHIJKLMNOPQRSTUVWXYZ`,
  public_ipv4: `192.0.2.${i + 1}`,
  public_ipv6: null,
  dns: { t: 'system' },
  domain_strategy: 'use_ip',
  public_ipv4_nat: false,
  public_ipv6_nat: false,
  lifecycle_phase: 'active',
  egress_allowed: true,
  desired_poll_fresh: true,
  runtime_report_fresh: true,
  agent_protocol_version: AGENT_PROTOCOL_VERSION,
  token_prefix: 'test-prefix',
  token_last_used_at: new Date(now - 600_000).toISOString(),
  last_poll_at: new Date(now - 600_000).toISOString(),
  agent_version: 'b'.repeat(64),
  runtime_versions: {
    xray_installed_sha256: 'b'.repeat(64),
    xray_running_sha256: 'b'.repeat(64),
    xray_handoff: { state: 'ready' },
  },
  xray_upgrade: {
    source: { kind: 'config-deployment', deployment_id: 100 + i, claim_generation: 1 },
    phase: 'blocked',
    strategy: 'handoff',
    old: { alive: true, active_connections: 127 + i },
    candidate: { alive: true, accepting: true },
  },
})) as unknown as NodeAgentStateItem[];
export const ingresses: SnapshotIngress[] = pairs.map((pair, i) => ({
  id: pair.ingressId,
  chain: pair.chainId,
  node: 'node-0',
  bind: '0.0.0.0',
  port: 63001 + i,
  projection: {},
  identity: { public_key: 'test-key', short_ids: ['0123456789abcdef'] },
  guard: {
    no_private: true,
    no_bittorrent: true,
    no_mail: true,
    no_udp_amplification: true,
    tcp_and_quic_only: false,
    cn_source_block: 'off',
  },
  wires: {
    vless: { kind: 'vless-reality' },
    anytls: { port: 15443 + i, security: 'tls', masquerade: { kind: 'not-found' } },
    hysteria2: { port: 32000 + i, bandwidth: {}, congestion: 'brutal', masquerade: { kind: 'not-found' } },
  },
}));
export const steps: SnapshotStep[] = nodes.map((node, i) => ({
  chain: pairs[0].chainId,
  node: node.node_id,
  accept: null,
  hop_in: i ? { port: 22000 + i, security: { t: 'none' } } : null,
  rules: [
    {
      m: { t: 'any' },
      a:
        i === 11
          ? { t: 'egress', send_through: null }
          : { t: 'forward', to: nodes[i + 1].node_id, dial: { t: 'overlay' }, pool: { t: 'mux', v: DEFAULT_HOP_MUX } },
    },
  ],
}));
export const warps: ExternalOutbound[] = [1, 2].map(i => ({
  id: warpTunnelIdFromBytes(new Uint8Array([i, 1, 0, i])),
  tenant: 'platform',
  name: `WARP ${i === 1 ? 'A' : 'B'}`,
  address: `162.159.198.${i + 1}`,
  port: 443,
  protocol: { t: 'warp', v: { http_version: 'h3', remote_dns: ['1.1.1.1'] } },
  security: { t: 'none' },
  bindings: [],
}));
export const snapshot: ConsoleSnapshot = {
  snapshot: {
    revision: 7,
    nodes: [],
    external_outbounds: warps,
    apps: [
      {
        id: appId,
        label: '回归项目',
        chains: pairs.map((pair, i) => ({ id: pair.chainId, tenant: 'platform', name: `线路 ${i === 0 ? 'A' : 'B'}` })),
        ingresses,
        steps: [
          ...steps,
          {
            ...steps[0],
            chain: pairs[1].chainId,
            rules: [{ m: { t: 'any' }, a: { t: 'egress', send_through: null } }],
          },
        ],
        fronts: [],
        grants: [],
      },
    ],
  },
  node_egress_dns: [],
  redacted: false,
};
export const settings = {
  ports: { ingress_base: 13443, anytls_base: 15443, hop_base: 22000, hy2_base: 32000 },
  relay_mux: DEFAULT_HOP_MUX,
  reality_site: {
    dest: 'site.example:443',
    server_names: ['site.example'],
    fingerprint: 'chrome',
    flow: 'xtls-rprx-vision',
  },
};
export const compiled = {
  revision: 7,
  summary: { errors: 0, warnings: 0, infos: 0, can_publish: true },
  diagnostics: [],
  system: {},
  apps: [],
  redacted: false,
};
export const report: NodeLoadView = {
  node_id: 'node-0',
  range_start_unix_secs: now / 1000 - 3600,
  range_end_unix_secs: now / 1000,
  reported_at_unix_secs: now / 1000,
  clock_skew_secs: 0,
  host: { cores: 128, mem_total_bytes: 8 * 1024 ** 3, disk_total_bytes: 20 * 1024 ** 3 } as HostFacts,
  latest_sample: null,
  processes: [],
  series: Array.from(
    { length: 120 },
    (_, i) =>
      ({
        window_start_unix_secs: now / 1000 - 3600 + i * 30,
        window_end_unix_secs: now / 1000 - 3570 + i * 30,
        has_gap: false,
        cpu_user_pct: 8,
        cpu_sys_pct: 4,
        cpu_softirq_pct: 2,
        cpu_steal_pct: 0,
        cpu_peak_pct: 24,
        load1: 1,
        cpu_detail: {
          iowait_pct: 0,
          load5: 1,
          load15: 1,
          pressure_some_pct: 0,
          io_pressure_some_pct: 0,
          io_pressure_full_pct: 0,
          procs_running: 1,
          procs_total: 100,
          context_switches_per_sec: 100,
          net_rx_softirqs_per_sec: 100,
          net_tx_softirqs_per_sec: 100,
          throttled_usec: 0,
          frequency_mhz: 2800,
          cores: Array.from({ length: 128 }, (_, cpu) => ({
            cpu,
            user_pct: 2 + cpu / 4,
            system_pct: 3,
            softirq_pct: 1,
            iowait_pct: 0,
            steal_pct: 0,
          })),
        },
        mem_available_bytes: 4 * 1024 ** 3,
        swap_used_bytes: 0,
        memory_detail: null,
        oom_kills: 0,
        disk_free_bytes: 10 * 1024 ** 3,
        disk_inode_free_pct: 90,
        disk_detail: null,
        nic_rx_bps: 0,
        nic_tx_bps: 0,
        nic_rx_drop: 0,
        nic_tx_drop: 0,
        nic_err: 0,
        conntrack_count: 100,
        network_detail: null,
        uptime_secs: 10000,
      }) as LoadSample,
  ),
};
report.latest_sample = report.series.at(-1)!;
