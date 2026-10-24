// Synthetic sources, bans and machine receipts for the 来源封禁 mockup. Addresses come from the documentation
// ranges (RFC 5737 / RFC 3849); their regions and operators are made up for layout only.
import type {
  NetworkOperator,
  OnlineSourceProtocol,
  SourceAccessNode,
  UserOnlineSource,
  UserOnlineSourceAccess,
} from '../../src/api';

export const MIN = 60_000;
export const HOUR = 60 * MIN;
export const DAY = 24 * HOUR;
export const MiB = 1024 ** 2;
export const GiB = 1024 ** 3;

export const MACHINES = [
  { node_id: 'n-hk1', name: '香港-01', country: 'HK' },
  { node_id: 'n-jp1', name: '东京-01', country: 'JP' },
  { node_id: 'n-tw1', name: '台北-01', country: 'TW' },
] as const;

export const CHAINS = [
  { id: 'hk-01', name: 'HK-01', node: 'n-hk1', country: 'HK' },
  { id: 'jp-01', name: 'JP-01', node: 'n-jp1', country: 'JP' },
  { id: 'tw-01', name: 'TW-01', node: 'n-tw1', country: 'TW' },
] as const;
type ChainId = (typeof CHAINS)[number]['id'];

interface Seed {
  ip: string;
  country?: string;
  operator?: NetworkOperator;
  /** Minutes the current session has lasted; present only for online sources. */
  onlineFor?: number;
  /** Minutes since last seen; offline sources only. */
  lastSeen?: number;
  firstSeenDays: number;
  up: number;
  down: number;
  chains: [ChainId, OnlineSourceProtocol[]][];
}

const SEEDS: Seed[] = [
  {
    ip: '203.0.113.24',
    country: 'CN',
    operator: 'chinanet',
    onlineFor: 134,
    firstSeenDays: 21,
    up: 46 * MiB,
    down: 1.18 * GiB,
    chains: [['hk-01', ['vless']]],
  },
  {
    ip: '2001:db8:4c1a:20:9d3e:51ff:fe0a:7b12',
    country: 'CN',
    operator: 'cmcc',
    onlineFor: 38,
    firstSeenDays: 0,
    up: 9 * MiB,
    down: 277 * MiB,
    chains: [['jp-01', ['hysteria2']]],
  },
  {
    ip: '198.51.100.77',
    country: 'VN',
    onlineFor: 371,
    firstSeenDays: 2,
    up: 310 * MiB,
    down: 8.1 * GiB,
    chains: [
      ['tw-01', ['vless']],
      ['hk-01', ['vless']],
    ],
  },
  {
    ip: '203.0.113.61',
    country: 'CN',
    operator: 'unicom',
    lastSeen: 140,
    firstSeenDays: 12,
    up: 12 * MiB,
    down: 412 * MiB,
    chains: [['hk-01', ['vless']]],
  },
  {
    ip: '192.0.2.140',
    country: 'US',
    lastSeen: 300,
    firstSeenDays: 3,
    up: 3 * MiB,
    down: 61 * MiB,
    chains: [['jp-01', ['anytls']]],
  },
  {
    ip: '2001:db8:4c1a:20:5a2f:1bff:fe33:c901',
    country: 'CN',
    operator: 'cmcc',
    lastSeen: 26 * 60,
    firstSeenDays: 9,
    up: 21 * MiB,
    down: 690 * MiB,
    chains: [['jp-01', ['hysteria2']]],
  },
  {
    ip: '198.51.100.23',
    country: 'RU',
    lastSeen: 4 * 24 * 60 + 70,
    firstSeenDays: 6,
    up: 520 * MiB,
    down: 14.2 * GiB,
    chains: [
      ['tw-01', ['vless']],
      ['jp-01', ['vless']],
    ],
  },
  {
    ip: '192.0.2.33',
    country: 'CN',
    operator: 'chinanet',
    lastSeen: 11 * 24 * 60,
    firstSeenDays: 29,
    up: 8 * MiB,
    down: 233 * MiB,
    chains: [['hk-01', ['vless']]],
  },
  {
    ip: '203.0.113.200',
    country: 'SG',
    lastSeen: 19 * 24 * 60,
    firstSeenDays: 25,
    up: 2 * MiB,
    down: 18 * MiB,
    chains: [['hk-01', ['mtproto']]],
  },
  {
    ip: '2001:db8:77:3::1c',
    country: 'HK',
    operator: 'cmcc',
    lastSeen: 26 * 24 * 60,
    firstSeenDays: 28,
    up: 5 * MiB,
    down: 140 * MiB,
    chains: [['hk-01', ['vless']]],
  },
];

/** Regions for banned addresses that never showed up in presence (the mockup assumes the ban API returns them). */
const BANNED_ONLY_GEO: Record<string, { country?: string; operator?: NetworkOperator }> = {
  '192.0.2.250': { country: 'NL' },
};

const iso = (t: number) => new Date(t).toISOString();

function sourceOf(seed: Seed, now: number): UserOnlineSource {
  const online = seed.onlineFor !== undefined;
  const last = online ? now - 20_000 : now - seed.lastSeen! * MIN;
  const since = online ? now - seed.onlineFor! * MIN : last - 47 * MIN;
  const first = Math.min(since, now - seed.firstSeenDays * DAY - 3 * HOUR);
  const share = 1 / seed.chains.length;
  const accesses: UserOnlineSourceAccess[] = seed.chains.map(([chain, protocols], index) => ({
    node_id: CHAINS.find(item => item.id === chain)!.node,
    ingress_id: `in-${chain}`,
    protocols,
    last_observed_at: iso(last),
    xray_last_seen_at: iso(last - (index + 1) * 40_000),
    online_since_at: iso(since + index * 9 * MIN),
    session_uplink_bytes: Math.round(seed.up * share),
    session_downlink_bytes: Math.round(seed.down * share),
  }));
  return {
    ip: seed.ip,
    first_observed_at: iso(first),
    last_observed_at: iso(last),
    xray_last_seen_at: iso(last - 40_000),
    online_since_at: iso(since),
    session_uplink_bytes: accesses.reduce((sum, access) => sum + (access.session_uplink_bytes ?? 0), 0),
    session_downlink_bytes: accesses.reduce((sum, access) => sum + (access.session_downlink_bytes ?? 0), 0),
    node_ids: [...new Set(accesses.map(access => access.node_id))],
    ingress_ids: accesses.map(access => access.ingress_id),
    accesses,
  };
}

/** Deterministic filler for the long-list scene: mixed IPv4 / IPv6, a few with a region. */
function manyBans(count: number): { ips: string[]; geo: Record<string, { country?: string }> } {
  const regions = ['RU', 'VN', 'BR', 'IN', 'ID', 'US', 'DE', 'TR', 'UA', 'KR'];
  const ips: string[] = [];
  const geo: Record<string, { country?: string }> = {};
  for (let i = 0; i < count; i++) {
    const ip =
      i % 5 === 4
        ? `2001:db8:${(0x3a00 + i * 37).toString(16)}:${(i * 13) % 97}::${(((i * 7919) % 0xfffe) + 1).toString(16)}`
        : `${['192.0.2', '198.51.100', '203.0.113'][i % 3]}.${((i * 53 + 11) % 254) + 1}`;
    if (ips.includes(ip)) continue;
    ips.push(ip);
    if (i % 4 !== 3) geo[ip] = { country: regions[(i * 7) % regions.length] };
  }
  return { ips, geo };
}

export type SceneKey = 'typical' | 'empty' | 'pending' | 'degraded' | 'many' | 'blind';
export const SCENE_KEYS: SceneKey[] = ['typical', 'empty', 'pending', 'degraded', 'many', 'blind'];

export interface World {
  revision: number;
  bans: string[];
  nodes: SourceAccessNode[];
  presence: 'complete' | 'unavailable';
  online: UserOnlineSource[];
  history: UserOnlineSource[];
  countries: Record<string, string>;
  operators: Record<string, NetworkOperator>;
}

const applied = (): SourceAccessNode[] => MACHINES.map(machine => ({ node_id: machine.node_id, phase: 'applied' }));

export const SCENES: Record<
  SceneKey,
  { label: string; bans: string[]; nodes: () => SourceAccessNode[]; blind?: true }
> = {
  typical: { label: '常态 · 2 个封禁', bans: ['198.51.100.23', '192.0.2.250'], nodes: applied },
  empty: { label: '无封禁', bans: [], nodes: applied },
  pending: {
    label: '刚封禁 · 同步中',
    bans: ['198.51.100.23', '192.0.2.250', '198.51.100.77'],
    nodes: () => [
      { node_id: 'n-hk1', phase: 'applied' },
      { node_id: 'n-jp1', phase: 'pending' },
      { node_id: 'n-tw1', phase: 'pending' },
    ],
  },
  degraded: {
    label: '部分机器未生效',
    bans: ['198.51.100.23', '192.0.2.250', '198.51.100.77'],
    nodes: () => [
      { node_id: 'n-hk1', phase: 'applied' },
      { node_id: 'n-jp1', phase: 'unsupported' },
      { node_id: 'n-tw1', phase: 'error', error: '写入策略文件失败：No space left on device' },
    ],
  },
  many: { label: '大量地址', bans: [], nodes: applied },
  blind: { label: '无在线统计', bans: ['198.51.100.23', '192.0.2.250'], nodes: applied, blind: true },
};

export function worldOf(key: SceneKey, now: number): World {
  const scene = SCENES[key];
  const sources = SEEDS.map(seed => ({ seed, source: sourceOf(seed, now) }));
  const countries: Record<string, string> = {};
  const operators: Record<string, NetworkOperator> = {};
  for (const seed of SEEDS) {
    if (seed.country) countries[seed.ip] = seed.country;
    if (seed.operator) operators[seed.ip] = seed.operator;
  }
  let bans = scene.bans;
  const extraGeo: Record<string, { country?: string; operator?: NetworkOperator }> = { ...BANNED_ONLY_GEO };
  if (key === 'many') {
    const filler = manyBans(44);
    const fresh = filler.ips.filter(ip => !SEEDS.some(seed => seed.ip === ip));
    bans = ['198.51.100.23', '192.0.2.250', ...fresh];
    for (const ip of fresh) if (filler.geo[ip]) extraGeo[ip] = filler.geo[ip];
  }
  for (const [ip, geo] of Object.entries(extraGeo)) {
    if (geo.country) countries[ip] = geo.country;
    if (geo.operator) operators[ip] = geo.operator;
  }
  return enforce(
    {
      revision: 7,
      bans: [...bans].sort(),
      nodes: scene.nodes(),
      presence: scene.blind ? 'unavailable' : 'complete',
      online: scene.blind ? [] : sources.filter(item => item.seed.onlineFor !== undefined).map(item => item.source),
      history: scene.blind ? [] : sources.filter(item => item.seed.onlineFor === undefined).map(item => item.source),
      countries,
      operators,
    },
    now,
  );
}

/**
 * What the machines do with the list: an applied machine closes a banned source's sessions on its ingresses. A source
 * stays online only through machines that have not applied the list (still syncing, outdated or failing).
 */
/** Accesses a source had before machines started closing it, so its history row still names every chain. */
const ACCESSES_BEFORE_BAN = new Map<string, UserOnlineSourceAccess[]>();

export function enforce(world: World, now: number): World {
  const banned = new Set(world.bans);
  const phase = new Map(world.nodes.map(node => [node.node_id, node.phase]));
  const online: UserOnlineSource[] = [];
  const closed: UserOnlineSource[] = [];
  for (const source of world.online) {
    if (!banned.has(source.ip)) {
      online.push(source);
      continue;
    }
    const accesses = (source.accesses ?? []).filter(access => phase.get(access.node_id) !== 'applied');
    if (accesses.length === (source.accesses ?? []).length) {
      online.push(source);
      continue;
    }
    if (!ACCESSES_BEFORE_BAN.has(source.ip)) ACCESSES_BEFORE_BAN.set(source.ip, source.accesses ?? []);
    if (accesses.length > 0)
      online.push({
        ...source,
        accesses,
        node_ids: [...new Set(accesses.map(access => access.node_id))],
        ingress_ids: accesses.map(access => access.ingress_id),
      });
    else {
      const all = ACCESSES_BEFORE_BAN.get(source.ip) ?? source.accesses ?? [];
      closed.push({
        ...source,
        accesses: all,
        node_ids: [...new Set(all.map(access => access.node_id))],
        ingress_ids: all.map(access => access.ingress_id),
        last_observed_at: new Date(now).toISOString(),
      });
    }
  }
  if (closed.length === 0 && online.length === world.online.length && online.every((s, i) => s === world.online[i]))
    return world;
  return {
    ...world,
    online,
    history: [...closed, ...world.history.filter(source => !closed.some(item => item.ip === source.ip))],
  };
}

/** Client-side mirror of brocade-deployment `normalize_ip`: one numeric address, no port, prefix or zone. */
export function normalizeIp(raw: string): string | null {
  const value = raw.trim();
  if (!value || /[\s/%[\]]/.test(value)) return null;
  const v4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(value);
  if (v4) {
    const parts = v4.slice(1);
    if (parts.some(part => (part.length > 1 && part.startsWith('0')) || Number(part) > 255)) return null;
    return parts.map(Number).join('.');
  }
  if (!value.includes(':')) return null;
  let host: string;
  try {
    host = new URL(`http://[${value}]/`).hostname.slice(1, -1);
  } catch {
    return null;
  }
  const mapped = /^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/.exec(host);
  if (mapped) {
    const high = parseInt(mapped[1], 16);
    const low = parseInt(mapped[2], 16);
    return [high >> 8, high & 255, low >> 8, low & 255].join('.');
  }
  return host;
}

/** Numeric order for display: IPv4 before IPv6, each by value. */
export function compareIp(a: string, b: string): number {
  const key = (ip: string) =>
    ip.includes(':')
      ? `6${new URL(`http://[${ip}]/`).hostname
          .slice(1, -1)
          .split('::')
          .map(half => half.split(':').filter(Boolean))
          .reduce<string[]>((groups, half, index, halves) => {
            if (index === 0) return half;
            return [...groups, ...Array(8 - halves.flat().length).fill('0'), ...half];
          }, [])
          .map(group => group.padStart(4, '0'))
          .join('')}`
      : `4${ip
          .split('.')
          .map(part => part.padStart(3, '0'))
          .join('')}`;
  return key(a).localeCompare(key(b));
}
