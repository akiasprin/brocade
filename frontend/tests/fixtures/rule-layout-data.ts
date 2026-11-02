// Synthetic API snapshot reproducing the reported two-rule, six-DNS-policy scene.
import {
  DEFAULT_HOP_MUX,
  type ConsoleSnapshot,
  type EgressDnsResolution,
  type Rule,
  type SnapshotStep,
} from '../../src/api';

export const APP = 'app';
export const CHAIN = 'hk-ai';
export const TENANT = 'preview';

export const NODES = [
  {
    node_id: 'hk-fxt',
    name: '香港 FXT II',
    public_ipv4: '203.0.113.21',
    public_ipv6: '2001:db8:21:7e00::21',
  },
  {
    node_id: 'hk-akile',
    name: '香港 AKILE II',
    public_ipv4: '198.51.100.42',
    public_ipv6: '2001:db8:42:5d05::42',
  },
  {
    node_id: 'tw-hinet',
    name: '台湾 AKILE HiNET',
    public_ipv4: '192.0.2.88',
    public_ipv6: '2001:db8:b030:a42d:5d05:8e1f:3c2a:1',
  },
] as const;

const dns = (address: string): EgressDnsResolution => ({
  address,
  port: 53,
  transport: 'udp',
  address_strategy: 'use_ip',
  fallback: 'machine',
});
/** WireGuard 内网地址（生产从编译视图读取；这里直接给出）。 */
export const OVERLAY: Record<string, string> = {
  'hk-fxt': '10.66.0.1',
  'hk-akile': '10.66.0.2',
  'tw-hinet': '10.66.0.3',
};

export const DNS_SITES = ['dazn', 'dmm', 'abema', 'disney', 'tiktok', 'spotify'];
export const DNS_ADDRESS = '198.51.100.207, 203.0.113.66';

const forward = (to: string, dial: { t: 'addr'; v: string } | { t: 'overlay' }): Rule['a'] =>
  ({ t: 'forward', to, dial, pool: { t: 'mux', v: { ...DEFAULT_HOP_MUX } } }) as Rule['a'];

export const STEPS: SnapshotStep[] = [
  {
    chain: CHAIN,
    node: 'hk-fxt',
    accept: null,
    hop_in: null,
    rules: [{ m: { t: 'any' }, a: forward('hk-akile', { t: 'overlay' }) }],
  },
  {
    chain: CHAIN,
    node: 'hk-akile',
    accept: { uuid: 'preview-only', label: '中继' },
    hop_in: { port: 20001, security: { t: 'none' } },
    rules: [
      {
        m: { t: 'geosite', v: ['category-ai-!cn'] },
        a: forward('tw-hinet', { t: 'addr', v: '[2001:db8:b030:a42d:5d05:8e1f:3c2a:1]:20002' }),
      },
      { m: { t: 'any' }, a: { t: 'egress', send_through: null } },
    ],
  },
  {
    chain: CHAIN,
    node: 'tw-hinet',
    accept: { uuid: 'preview-only-2', label: '中继' },
    hop_in: { port: 20002, security: { t: 'none' } },
    rules: [{ m: { t: 'any' }, a: { t: 'egress', send_through: null } }],
  },
];

const chain = {
  id: CHAIN,
  name: '香港 AI',
  tenant: TENANT,
  revision: 1,
  overlay: { ipv4: true, ipv6: false },
  auto_exits: [],
  egress_via: [],
  carry_via: [],
  mtu: 1420,
  hop_mode: 'direct',
  hop_port: 20000,
  projection: {},
  guard: {
    no_private: true,
    no_bittorrent: true,
    no_mail: true,
    no_udp_amplification: true,
    tcp_and_quic_only: false,
    cn_source_block: 'off',
  },
  identity: { public_key: 'preview', short_ids: ['0123abcd'] },
  wires: { vless: { kind: 'vless-reality' } },
};

export const SNAPSHOT = {
  snapshot: {
    revision: 1,
    settings: { relay_mux: DEFAULT_HOP_MUX },
    external_outbounds: [],
    nodes: NODES.map(node => ({ id: node.node_id, name: node.name })),
    apps: [
      {
        id: APP,
        label: '预览项目',
        chains: [chain],
        steps: STEPS,
        fronts: [],
        grants: [],
        ingresses: [],
      },
    ],
  },
  node_egress_dns: DNS_SITES.map((site, position) => ({
    node: 'hk-akile',
    position,
    selector: { t: 'geosite', v: [site] },
    resolution: dns(DNS_ADDRESS),
  })),
  redacted: false,
} as unknown as ConsoleSnapshot;

export const AGENT_NODES = {
  nodes: NODES.map(node => ({
    ...node,
    tenant_id: TENANT,
    egress_allowed: true,
    retired_at: null,
    public_ipv4_nat: false,
    public_ipv6_nat: false,
  })),
};

export const SETTINGS = { ports: { hop_base: 20000 } };
export const REVISIONS = { current_revision: null, revisions: [] };
