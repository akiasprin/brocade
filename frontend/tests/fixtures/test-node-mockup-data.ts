// Synthetic fleet, chains and test nodes for the test node mockup. Never imported by the app.
import type {
  E2eProbeItem,
  E2eProbeStatus,
  NodeAgentStateItem,
  SnapshotApp,
  SnapshotIngress,
  SnapshotStep,
} from '../../src/api';
import { agentNodes, MACHINES, snapshot as wizardSnapshot, type Machine } from './chain-wizard-mockup-data';

/** 2026-10-21 21:40 (UTC+8). The mock clock starts here and then runs in real time. */
export const NOW = Date.UTC(2026, 9, 21, 13, 40);
export const MINUTE = 60_000;
export const HOUR = 60 * MINUTE;
/** 新建的测试节点保留一天；「延长」每次再加一天。 */
export const TEST_NODE_TTL = 24 * HOUR;
/** 每个操作者同时保留的测试节点上限。 */
export const TEST_NODE_LIMIT = 10;

/* ══ 机队 ══ */

export interface FleetMachine extends Machine {
  /** 机器上已开着测试入口：建、删测试节点都只做热更新，不重启 Xray。 */
  testReady: boolean;
  online: boolean;
  /** 近 12 分钟网卡速率（Mbit/s）。 */
  nicMbps: number[];
  /** 去掉开头地区词的短名：台面上地区已经写在板上。 */
  short: string;
  /** 所在城市：台面上的一块板。 */
  city: CityCode;
  /** 控制台到这台机器的 TCP 延迟（ms），离线时没有读数。 */
  ping: number | null;
}

const EXTRA: Record<string, Partial<FleetMachine>> = {
  'jp-ntt': { testReady: false },
  'de-fra': { testReady: false },
  'sg-dp': { online: false },
};

const PING: Record<string, number> = {
  'hk-hkix': 38,
  'hk-bgp': 36,
  'hk-home': 44,
  'mo-ctm': 41,
  'tw-hinet': 52,
  'jp-iij': 88,
  'jp-ntt': 91,
  'us-gia': 156,
  'us-9929': 171,
  'de-fra': 238,
  'gb-lon': 226,
};

/* ══ 城市：地理坐标（算距离）与台面位置（摆板） ══
   台面位置是示意图：x 向东、y 向南，单位是等距步长。东亚放大、大洋与欧亚之间压缩，排成接近方形，
   好放进两侧面板之间的画幅。落地时按经纬度分段压缩自动排。 */

export type CityCode = 'HKG' | 'MFM' | 'TPE' | 'TYO' | 'SIN' | 'LAX' | 'FRA' | 'LON';

export interface City {
  name: string;
  lon: number;
  lat: number;
  /** 这座城市第一台机器在台面上的位置；同城的其余机器沿等距 x 轴向右下排开。 */
  board: { x: number; y: number };
}

export const CITIES: Record<CityCode, City> = {
  LON: { name: '伦敦', lon: -0.13, lat: 51.51, board: { x: 0.6, y: 0.3 } },
  FRA: { name: '法兰克福', lon: 8.68, lat: 50.11, board: { x: 2, y: 1.2 } },
  MFM: { name: '澳门', lon: 113.54, lat: 22.19, board: { x: 1.4, y: 5.9 } },
  SIN: { name: '新加坡', lon: 103.82, lat: 1.35, board: { x: 2.4, y: 8.2 } },
  HKG: { name: '香港', lon: 114.17, lat: 22.32, board: { x: 2.9, y: 3.9 } },
  TPE: { name: '台北', lon: 121.56, lat: 25.04, board: { x: 6.2, y: 3.2 } },
  TYO: { name: '东京', lon: 139.69, lat: 35.69, board: { x: 6.2, y: 0.6 } },
  LAX: { name: '洛杉矶', lon: -118.24, lat: 34.05, board: { x: 8.8, y: 2.2 } },
};

const CITY: Record<string, CityCode> = {
  'hk-hkix': 'HKG',
  'hk-bgp': 'HKG',
  'hk-home': 'HKG',
  'mo-ctm': 'MFM',
  'tw-hinet': 'TPE',
  'jp-iij': 'TYO',
  'jp-ntt': 'TYO',
  'sg-dp': 'SIN',
  'us-gia': 'LAX',
  'us-9929': 'LAX',
  'de-fra': 'FRA',
  'gb-lon': 'LON',
};

function wave(seed: number, base: number, swing: number): number[] {
  return Array.from({ length: 24 }, (_, index) =>
    Math.max(0.4, base + swing * Math.sin(index / 3.1 + seed) + (swing / 2) * Math.sin(index * 1.7 + seed * 2)),
  );
}

export const FLEET: FleetMachine[] = MACHINES.filter(machine => !machine.retired).map((machine, index) => ({
  ...machine,
  testReady: true,
  online: true,
  nicMbps: wave(index + 1, 18 + (index % 4) * 21, 6 + (index % 3) * 9),
  short: machine.name.split(' ').slice(1).join(' ') || machine.name,
  city: CITY[machine.id] ?? 'HKG',
  ping: PING[machine.id] ?? null,
  ...EXTRA[machine.id],
}));

export const fleetById = new Map(FLEET.map(machine => [machine.id, machine]));

/** 当前读数：曲线的最后一个窗口。 */
export const loadOf = (machine: FleetMachine) => Math.round(machine.nicMbps.at(-1) ?? 0);

/* ══ 距离与时延下限 ══ */

const EARTH_KM = 6371;

function haversineKm(a: City, b: City): number {
  const rad = Math.PI / 180;
  const dLat = (b.lat - a.lat) * rad;
  const dLon = (b.lon - a.lon) * rad;
  const h = Math.sin(dLat / 2) ** 2 + Math.cos(a.lat * rad) * Math.cos(b.lat * rad) * Math.sin(dLon / 2) ** 2;
  return 2 * EARTH_KM * Math.asin(Math.sqrt(h));
}

/** 相邻两台机器之间的大圆距离（千米）；同城为 0。 */
export function hopKm(from: string, to: string): number {
  const a = fleetById.get(from);
  const b = fleetById.get(to);
  return a && b ? haversineKm(CITIES[a.city], CITIES[b.city]) : 0;
}

export function pathKm(path: string[]): number {
  return path.slice(1).reduce((total, id, index) => total + hopKm(path[index], id), 0);
}

/** 光纤里光速约为真空的 2/3（每毫秒约 200 km），往返时延不会低于这个值。不含隧道和最后一跳之后的路程。 */
export const minRttMs = (km: number) => (km * 2) / 200;

export const kmText = (km: number) => `${Math.round(km).toLocaleString('en-US')} km`;

/** 外部出口（隧道）。只能作为最后一跳之后的出网方式。 */
export interface Tunnel {
  id: string;
  name: string;
  badge: string;
  /** 拨测时看到的出口地区与 IP。 */
  country: string;
  exitIp: string;
}

export const TUNNELS: Tunnel[] = [
  { id: 'ob-warp', name: 'Cloudflare WARP', badge: 'WARP', country: 'US', exitIp: '104.28.214.37' },
  { id: 'ob-netflix-sg', name: '新加坡家宽 Socks', badge: 'SOCKS', country: 'SG', exitIp: '116.88.41.209' },
];

export const tunnelById = new Map(TUNNELS.map(tunnel => [tunnel.id, tunnel]));

/** 隧道不在任何城市：台面上单独一块「外部」板，放在东南角空着的地方，坐标含义同 CITIES.board。 */
export const TUNNEL_BOARD = { x: 7.9, y: 6.2 };

/* ══ 线路页现有的链（普通模式的背景） ══ */

interface ChainSeed {
  app: string;
  id: string;
  name: string;
  country: string;
  path: string[];
  users: string[];
  /** 平均落点首字节；null 表示近 3 小时不通。 */
  ttfb: number | null;
}

const CHAINS: ChainSeed[] = [
  {
    app: 'app-global',
    id: 'c-hk-direct',
    name: '香港直出',
    country: 'HK',
    path: ['hk-hkix'],
    users: ['alice', 'bob', 'carol', 'dave', 'erin', 'frank', 'grace', 'heidi', 'ivan'],
    ttfb: 46,
  },
  {
    app: 'app-global',
    id: 'c-jp-relay',
    name: '香港 → 东京',
    country: 'JP',
    path: ['hk-bgp', 'jp-iij'],
    users: ['alice', 'bob', 'carol', 'dave', 'erin', 'judy', 'grace'],
    ttfb: 98,
  },
  {
    app: 'app-global',
    id: 'c-tw-us',
    name: '台湾 → 洛杉矶',
    country: 'US',
    path: ['tw-hinet', 'us-gia'],
    users: ['alice', 'frank', 'heidi', 'ivan'],
    ttfb: 186,
  },
  {
    app: 'app-global',
    id: 'c-mo-sg',
    name: '澳门 → 新加坡',
    country: 'SG',
    path: ['mo-ctm', 'sg-dp'],
    users: ['bob', 'carol', 'judy'],
    ttfb: null,
  },
  {
    app: 'app-stream',
    id: 'c-jp-nf',
    name: '东京 Netflix',
    country: 'JP',
    path: ['hk-hkix', 'jp-ntt'],
    users: ['alice', 'bob', 'dave', 'erin', 'grace', 'ivan'],
    ttfb: 112,
  },
  {
    app: 'app-stream',
    id: 'c-us-disney',
    name: '美国 Disney+',
    country: 'US',
    path: ['hk-bgp', 'us-9929'],
    users: ['carol', 'heidi'],
    ttfb: 214,
  },
];

function ingressOf(seed: ChainSeed, index: number): SnapshotIngress {
  return {
    id: `i-${seed.id.slice(2)}`,
    chain: seed.id,
    node: seed.path[0],
    bind: '0.0.0.0',
    port: 13443 + index,
    projection: {},
    guard: {},
    identity: {},
    wires: {
      vless: { kind: 'vless-reality' },
      anytls: { port: 14443 + index },
      hysteria2: { port: 30000 + index * 100, hop: { start: 30000 + index * 100, end: 30099 + index * 100 } },
    },
  } as unknown as SnapshotIngress;
}

function stepsOf(seed: ChainSeed, index: number): SnapshotStep[] {
  return seed.path.map((node, hop) => {
    const next = seed.path[hop + 1];
    const target = next ? fleetById.get(next) : undefined;
    return {
      chain: seed.id,
      node,
      accept: hop === 0 ? null : {},
      hop_in: hop === 0 ? null : { port: 20000 + index, security: { t: 'encryption' } },
      rules: [
        next
          ? {
              m: { t: 'any' },
              a: {
                t: 'forward',
                to: next,
                dial: { t: 'addr', v: `${target?.ipv4 ?? next}:${20000 + index}` },
                pool: { t: 'none' },
              },
            }
          : { m: { t: 'any' }, a: { t: 'egress', send_through: null } },
      ],
    } as unknown as SnapshotStep;
  });
}

function appOf(id: string, label: string): SnapshotApp {
  const seeds = CHAINS.filter(seed => seed.app === id);
  const offset = CHAINS.findIndex(seed => seed.app === id);
  return {
    id,
    label,
    chains: seeds.map(seed => ({
      id: seed.id,
      tenant: 'platform',
      name: seed.name,
      subscription_country: seed.country,
    })),
    ingresses: seeds.map((seed, index) => ingressOf(seed, offset + index)),
    steps: seeds.flatMap((seed, index) => stepsOf(seed, offset + index)),
    fronts: [],
    grants: seeds.flatMap((seed, index) =>
      seed.users.map(user => ({
        tenant: 'platform',
        user,
        ingress: ingressOf(seed, offset + index).id,
        enabled: true,
      })),
    ),
  } as unknown as SnapshotApp;
}

export const CHAIN_APPS: SnapshotApp[] = [appOf('app-global', '全球加速'), appOf('app-stream', '流媒体解锁')];

export function chainSnapshot() {
  const base = wizardSnapshot();
  return { ...base, snapshot: { ...base.snapshot, apps: CHAIN_APPS } };
}

export function fleetAgentNodes(): NodeAgentStateItem[] {
  return agentNodes().map(node =>
    fleetById.get(node.node_id)?.online === false
      ? ({
          ...node,
          runtime_report_fresh: false,
          last_poll_at: new Date(NOW - 3 * HOUR).toISOString(),
        } as NodeAgentStateItem)
      : node,
  );
}

/** Deterministic pseudo-random sequence so screenshots stay stable. */
function noise(seed: number) {
  let state = seed % 2147483647 || 1;
  return () => {
    state = (state * 48271) % 2147483647;
    return state / 2147483647;
  };
}

export function e2eProbes(): { chains: E2eProbeItem[] } {
  const end = Math.floor(NOW / 1000);
  return {
    chains: CHAINS.map((seed, index) => {
      const random = noise(index * 7919 + 17);
      const points = 360;
      const probed_at_unix_secs = Array.from({ length: points }, (_, point) => end - (points - 1 - point) * 60);
      const status: E2eProbeStatus[] = probed_at_unix_secs.map((_, point) =>
        seed.ttfb === null && point > 180 ? 'chain-broken' : 'ok',
      );
      const ttfb_ms = status.map(value =>
        value === 'ok' ? Math.round((seed.ttfb ?? 120) * (0.82 + random() * 0.36 + (random() > 0.97 ? 0.5 : 0))) : null,
      );
      const exit = fleetById.get(seed.path.at(-1) ?? '');
      const ok = seed.ttfb !== null;
      return {
        app_id: seed.app,
        chain_id: seed.id,
        chain_name: seed.name,
        node_id: seed.path[0],
        status: ok ? 'ok' : 'chain-broken',
        ttfb_ms: ok ? (ttfb_ms.at(-1) ?? null) : null,
        exit_ip: ok ? (exit?.ipv4 ?? null) : null,
        exit_loc: ok ? seed.country : null,
        exit_intelligence: null,
        exit_verdict: ok ? 'match' : 'unknown',
        detail: ok ? null : '新加坡 DigitalOcean 失联，链在第二跳断开',
        probed_at: new Date(NOW).toISOString(),
        samples: { probed_at_unix_secs, status, ttfb_ms },
      } satisfies E2eProbeItem;
    }),
  };
}

/* ══ 测试节点 ══ */

export type HopState = 'wait' | 'run' | 'ok' | 'fail';
export type TestNodeState = 'deploying' | 'ready' | 'failed' | 'removing';

export interface ProbeResult {
  status: 'running' | 'ok' | 'fail';
  exitIp?: string;
  country?: string;
  /** 经这个测试节点取一次测速地址的首字节时间。 */
  ms?: number;
  detail?: string;
}

export interface TestNode {
  id: string;
  /** 机器 id，第一台是入口，最后一台是出口机器。 */
  path: string[];
  /** 出口机器之后的隧道；null 表示从出口机器直接出网。 */
  tunnel: string | null;
  createdAt: number;
  expiresAt: number;
  state: TestNodeState;
  /** 每台机器的收敛进度。 */
  hops: Record<string, HopState>;
  failure: { machine: string; reason: string } | null;
  probe: ProbeResult | null;
  /** 演示用：这台机器会收敛失败。 */
  failAt?: string;
}

export type Protocol = 'vless' | 'hy2' | 'anytls';

export const PROTOCOL_LABEL: Record<Protocol, string> = { vless: 'VLESS', hy2: 'HY2', anytls: 'AnyTLS' };

/** 机器上测试入口的固定端口：同一台机器上的测试节点共用监听，靠凭据区分。 */
export const TEST_PORTS: Record<Protocol, number> = { vless: 13990, anytls: 14990, hy2: 39990 };

function hex(seed: string, length: number): string {
  let hash = 2166136261;
  let out = '';
  for (let round = 0; out.length < length; round += 1) {
    for (const char of `${seed}:${round}`) hash = Math.imul(hash ^ char.charCodeAt(0), 16777619);
    out += (hash >>> 0).toString(16).padStart(8, '0');
  }
  return out.slice(0, length);
}

export function uuidOf(id: string): string {
  const raw = hex(id, 32);
  return `${raw.slice(0, 8)}-${raw.slice(8, 12)}-4${raw.slice(13, 16)}-a${raw.slice(17, 20)}-${raw.slice(20, 32)}`;
}

export function nameOf(id: string): string {
  return fleetById.get(id)?.name ?? tunnelById.get(id)?.name ?? id;
}

/** 客户端里显示的节点名。 */
export function clientName(node: Pick<TestNode, 'path' | 'tunnel'>): string {
  const hops = node.path.map(nameOf);
  if (node.tunnel) hops.push(nameOf(node.tunnel));
  return `测试 ${hops.join(' → ')}`;
}

/** 入口有证书时开三种协议；没有证书时只有借全局伪装站点的 VLESS。 */
export function protocolsOf(node: Pick<TestNode, 'path'>): Protocol[] {
  const entry = fleetById.get(node.path[0]);
  return entry?.cert === null ? ['vless'] : ['vless', 'hy2', 'anytls'];
}

export function linkOf(node: Pick<TestNode, 'id' | 'path' | 'tunnel'>, protocol: Protocol): string {
  const entry = fleetById.get(node.path[0]);
  const host = entry?.ipv4 ?? `[${entry?.ipv6 ?? '::1'}]`;
  const secret = uuidOf(node.id);
  const name = encodeURIComponent(clientName(node));
  const sni = `${entry?.id ?? 'entry'}.nodes.example.net`;
  if (protocol === 'vless')
    return `vless://${secret}@${host}:${TEST_PORTS.vless}?encryption=none&security=reality&sni=www.cloudflare.com&fp=safari&pbk=${hex(`${node.path[0]}:pbk`, 43)}&sid=${hex(node.id, 8)}&type=tcp#${name}`;
  if (protocol === 'hy2')
    return `hysteria2://${secret}@${host}:${TEST_PORTS.hy2}?sni=${sni}&obfs=salamander&obfs-password=${hex(`${node.id}:obfs`, 16)}#${name}`;
  return `anytls://${secret}@${host}:${TEST_PORTS.anytls}?sni=${sni}#${name}`;
}

export const TEST_SUBSCRIPTION_URL = 'https://console.example.net/sub/test/7c1f2d9e0b4a8c63e5f1a92e?flag=clash';

/** 机器在某个位置上为什么不能选；null 表示可以选。 */
export function machineBlock(machine: FleetMachine, position: 'entry' | 'later'): string | null {
  if (!machine.online) return '离线';
  if (position === 'entry' && machine.nat) return '在 NAT 后，不能作入口';
  return null;
}

/** 整条路径的问题。只报第一条：它也是「创建」被禁用的原因。 */
export function pathIssue(path: string[], tunnel: string | null): string | null {
  if (path.length === 0) return '先选入口机器';
  const machines = path.map(id => fleetById.get(id)).filter((machine): machine is FleetMachine => !!machine);
  const offline = machines.find(machine => !machine.online);
  if (offline) return `${offline.name} 离线`;
  if (machines[0]?.nat) return `${machines[0].name} 在 NAT 后，不能作入口`;
  const exit = machines.at(-1);
  if (!tunnel && exit && exit.egress === false) return `${exit.name} 不允许出网，换一台出口或接隧道`;
  return null;
}

export function restartMachines(path: string[]): FleetMachine[] {
  return path
    .map(id => fleetById.get(id))
    .filter((machine): machine is FleetMachine => !!machine && !machine.testReady);
}

/* ══ 场景 ══ */

function node(
  id: string,
  path: string[],
  options: Partial<Omit<TestNode, 'id' | 'path'>> & { age?: number; left?: number } = {},
): TestNode {
  const { age = 80 * MINUTE, left, ...rest } = options;
  const createdAt = NOW - age;
  return {
    id,
    path,
    tunnel: null,
    createdAt,
    expiresAt: left === undefined ? createdAt + TEST_NODE_TTL : NOW + left,
    state: 'ready',
    hops: Object.fromEntries(path.map(machine => [machine, 'ok'])),
    failure: null,
    probe: null,
    ...rest,
  };
}

const PROBED_IIJ: ProbeResult = { status: 'ok', exitIp: '198.51.100.51', country: 'JP', ms: 96 };

export interface Compose {
  path: string[];
  tunnel?: string | null;
  confirm?: boolean;
}

export interface Scene {
  label: string;
  note: string;
  nodes: () => TestNode[];
  /** 场景自带的组装状态，例如「待确认重启」。 */
  compose?: Compose;
}

export const SCENES = {
  typical: {
    label: '一个可用 · 一个下发中',
    note: '香港 HKIX → 东京 IIJ 已可用并拨测过；三跳那条正在等洛杉矶 CN2 GIA 领取。',
    nodes: () => [
      node('t-3fa1', ['hk-bgp', 'tw-hinet', 'us-gia'], {
        age: 25_000,
        state: 'deploying',
        hops: { 'hk-bgp': 'ok', 'tw-hinet': 'ok', 'us-gia': 'wait' },
      }),
      node('t-9c27', ['hk-hkix', 'jp-iij'], { probe: PROBED_IIJ }),
    ],
  },
  empty: {
    label: '没有测试节点',
    note: '还没有测试节点。',
    nodes: () => [],
  },
  failed: {
    label: '收敛失败',
    note: '洛杉矶 9929 上测试入口的 UDP 端口被别的进程占用，收敛失败。',
    nodes: () => [
      node('t-51d0', ['hk-hkix', 'us-9929'], {
        age: 3 * MINUTE,
        state: 'failed',
        hops: { 'hk-hkix': 'ok', 'us-9929': 'fail' },
        failure: { machine: 'us-9929', reason: '测试入口 UDP 39990 已被占用' },
      }),
      node('t-9c27', ['hk-hkix', 'jp-iij'], { probe: PROBED_IIJ }),
    ],
  },
  expiring: {
    label: '快到期',
    note: '第一条 12 分钟后到期，到期自动删除；直出那条还剩 5 小时。',
    nodes: () => [
      node('t-9c27', ['hk-hkix', 'jp-iij'], { age: 23 * HOUR + 48 * MINUTE, left: 12 * MINUTE, probe: PROBED_IIJ }),
      node('t-0b6e', ['mo-ctm'], { age: 19 * HOUR, left: 5 * HOUR }),
      node('t-77a4', ['hk-bgp', 'jp-iij'], { age: 6 * HOUR, tunnel: 'ob-warp' }),
    ],
  },
  restart: {
    label: '需要重启 Xray',
    note: '东京 NTT 还没开测试入口：首次在它上面建测试节点要重启 Xray，创建前先确认。',
    nodes: () => [node('t-9c27', ['hk-hkix', 'jp-iij'], { probe: PROBED_IIJ })],
    compose: { path: ['hk-hkix', 'jp-ntt'], confirm: true },
  },
  full: {
    label: '已达上限',
    note: `同时保留的测试节点已到 ${TEST_NODE_LIMIT} 个，新建被拦下。`,
    nodes: () =>
      [
        ['hk-hkix', 'jp-iij'],
        ['hk-hkix', 'tw-hinet'],
        ['hk-bgp', 'us-gia'],
        ['hk-bgp', 'us-9929'],
        ['mo-ctm'],
        ['tw-hinet'],
        ['hk-hkix', 'tw-hinet', 'us-gia'],
        ['hk-bgp', 'gb-lon'],
        ['hk-hkix'],
        ['mo-ctm', 'jp-iij'],
      ].map((path, index) => node(`t-f${index}a${index}`, path, { age: (index + 1) * 47 * MINUTE })),
  },
} satisfies Record<string, Scene>;

export type SceneKey = keyof typeof SCENES;
export const SCENE_KEYS = Object.keys(SCENES) as SceneKey[];
