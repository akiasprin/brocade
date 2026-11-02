// Synthetic fleet for the chain wizard fixture. Never imported by the app.
import type { NodeAgentStateItem, SnapshotApp, UserListItem } from '../../src/api';

export interface Machine {
  id: string;
  name: string;
  country: string;
  ipv4: string | null;
  ipv6?: string | null;
  nat?: boolean;
  tenant?: string;
  /** Certificate name of the default certificate group; null = not issued yet. */
  cert?: string | null;
  egress?: boolean;
  retired?: boolean;
}

export const MACHINES: Machine[] = [
  { id: 'hk-hkix', name: '香港 HKIX', country: 'HK', ipv4: '203.0.113.11', ipv6: '2001:db8:11::1' },
  { id: 'hk-bgp', name: '香港 BGP 三网', country: 'HK', ipv4: '203.0.113.12' },
  { id: 'hk-home', name: '香港 家宽', country: 'HK', ipv4: '198.18.0.20', nat: true, cert: null },
  { id: 'mo-ctm', name: '澳门 CTM', country: 'MO', ipv4: '203.0.113.31' },
  { id: 'tw-hinet', name: '台湾 HiNet', country: 'TW', ipv4: '203.0.113.41' },
  { id: 'jp-iij', name: '东京 IIJ', country: 'JP', ipv4: '198.51.100.51', ipv6: '2001:db8:51::1' },
  { id: 'jp-ntt', name: '东京 NTT', country: 'JP', ipv4: '198.51.100.52' },
  { id: 'sg-dp', name: '新加坡 DigitalOcean', country: 'SG', ipv4: '198.51.100.61' },
  { id: 'us-gia', name: '洛杉矶 CN2 GIA', country: 'US', ipv4: '192.0.2.71' },
  { id: 'us-9929', name: '洛杉矶 9929', country: 'US', ipv4: '192.0.2.72' },
  { id: 'de-fra', name: '法兰克福 标准', country: 'DE', ipv4: '192.0.2.81', egress: false },
  { id: 'gb-lon', name: '伦敦 家宽', country: 'GB', ipv4: null, ipv6: '2001:db8:91::1', cert: null },
  { id: 'kr-old', name: '首尔 旧机', country: 'KR', ipv4: '192.0.2.99', retired: true },
];

export const USERS: { id: string; tenant: string; status?: 'active' | 'disabled' }[] = [
  { id: 'alice', tenant: 'platform' },
  { id: 'bob', tenant: 'platform' },
  { id: 'carol', tenant: 'platform' },
  { id: 'dave', tenant: 'platform' },
  { id: 'erin', tenant: 'platform' },
  { id: 'frank', tenant: 'platform' },
  { id: 'grace', tenant: 'platform' },
  { id: 'heidi', tenant: 'platform' },
  { id: 'ivan', tenant: 'platform' },
  { id: 'judy', tenant: 'platform' },
  { id: 'mallory', tenant: 'platform', status: 'disabled' },
  { id: 'niaj', tenant: 'studio' },
  { id: 'olivia', tenant: 'studio' },
  { id: 'peggy', tenant: 'studio' },
];

/** 42 people across three tenants for the crowd scene. */
export const CROWD: { id: string; tenant: string; status?: 'active' | 'disabled' }[] = [
  ...'alice bob carol dave erin frank grace heidi ivan judy ken leo mia nick oscar pat quinn rose sam tina uma vic wendy xia yuki zane'
    .split(' ')
    .map(id => ({ id, tenant: 'platform' })),
  { id: 'mallory', tenant: 'platform', status: 'disabled' as const },
  { id: 'trent', tenant: 'platform', status: 'disabled' as const },
  ...'niaj olivia peggy rupert sybil'.split(' ').map(id => ({ id, tenant: 'studio' })),
  ...'aiko bao chen dong eri fang gao hana'.split(' ').map(id => ({ id, tenant: 'reseller' })),
];

export const TENANTS = [
  { id: 'platform', name: '平台', node_count: 12, user_count: 11, operator_count: 1 },
  { id: 'studio', name: '工作室', node_count: 0, user_count: 3, operator_count: 1 },
  { id: 'reseller', name: '分销', node_count: 0, user_count: 8, operator_count: 1 },
];

export const APPS: SnapshotApp[] = [
  {
    id: 'app-global',
    label: '全球加速',
    chains: [
      { id: 'c-hk-direct', tenant: 'platform', name: '香港直出' },
      { id: 'c-jp-relay', tenant: 'platform', name: '香港 → 东京' },
    ],
    ingresses: [
      {
        id: 'i-hk-direct',
        chain: 'c-hk-direct',
        node: 'hk-hkix',
        port: 13443,
        wires: {
          vless: { kind: 'vless-reality' },
          anytls: { port: 14443 },
          hysteria2: { port: 30000, hop: { start: 30000, end: 30099 } },
        },
      },
    ],
    steps: [
      {
        chain: 'c-jp-relay',
        node: 'hk-bgp',
        accept: null,
        hop_in: null,
        rules: [
          {
            m: { t: 'any' },
            a: { t: 'forward', to: 'jp-iij', dial: { t: 'addr', v: '198.51.100.51:20000' }, pool: { t: 'none' } },
          },
        ],
      },
      {
        chain: 'c-jp-relay',
        node: 'jp-iij',
        accept: {},
        hop_in: { port: 20000, security: { t: 'encryption' } },
        rules: [{ m: { t: 'any' }, a: { t: 'egress', send_through: null } }],
      },
    ],
    fronts: [],
    grants: [],
  } as unknown as SnapshotApp,
  {
    id: 'app-stream',
    label: '流媒体解锁',
    chains: [],
    ingresses: [],
    steps: [],
    fronts: [],
    grants: [],
  } as unknown as SnapshotApp,
];

export function agentNodes(): NodeAgentStateItem[] {
  return MACHINES.map(
    machine =>
      ({
        node_id: machine.id,
        tenant_id: machine.tenant ?? 'platform',
        name: machine.name,
        public_ipv4: machine.ipv4,
        public_ipv4_country: machine.country,
        public_ipv6: machine.ipv6 ?? null,
        public_ipv4_nat: machine.nat ?? false,
        public_ipv6_nat: false,
        route_ipv4: null,
        route_ipv6: null,
        retired_at: machine.retired ? '2026-09-01T00:00:00Z' : null,
        lifecycle_phase: machine.retired ? 'retired' : 'active',
        agent_protocol_version: 99,
        runtime_report_fresh: true,
        runtime_versions: { openvpn: machine.id.startsWith('jp') ? 'OpenVPN 2.6.14' : null },
      }) as unknown as NodeAgentStateItem,
  );
}

export function usersList(crowd = false): { users: UserListItem[] } {
  return {
    users: (crowd ? CROWD : USERS).map(
      user =>
        ({
          tenant_id: user.tenant,
          id: user.id,
          status: user.status ?? 'active',
          created_at: '2026-09-01T00:00:00Z',
          created_revision: 1,
        }) as UserListItem,
    ),
  };
}

export function snapshot() {
  return {
    snapshot: {
      revision: 41,
      nodes: MACHINES.map(machine => ({
        id: machine.id,
        certificate_name: machine.cert === null ? null : `${machine.id}.nodes.example.net`,
        certificate_track: machine.cert === null ? null : 'public-ca',
        egress_allowed: machine.egress ?? true,
      })),
      apps: APPS,
      external_outbounds: [
        {
          id: 'ob-warp',
          tenant: 'platform',
          name: 'Cloudflare WARP',
          address: 'engage.cloudflareclient.com',
          port: 2408,
          protocol: { t: 'warp' },
          security: null,
          bindings: [],
        },
        {
          id: 'ob-netflix-sg',
          tenant: 'platform',
          name: '新加坡家宽 Socks',
          address: '198.51.100.200',
          port: 1080,
          protocol: { t: 'socks5' },
          security: null,
          bindings: [],
        },
      ],
    },
    node_egress_dns: [],
    redacted: false,
  };
}

export const SETTINGS = {
  ports: {
    ingress_base: 13443,
    vless_encryption_base: 13800,
    anytls_base: 14443,
    hy2_base: 30000,
    mtproto_base: 28800,
    hop_base: 20000,
  },
  reality_site: { dest: 'www.cloudflare.com:443', server_names: ['www.cloudflare.com'] },
};

/** System-level ports taken on every machine (WireGuard etc.) as the compile view reports them. */
export function compileView() {
  return {
    diagnostics: [],
    system: {
      nodes: MACHINES.map(machine => ({ id: machine.id, wg_listen_port: 51820 })),
    },
  };
}
