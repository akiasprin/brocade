// Local visual acceptance fixture; production components and tokens, synthetic traffic only.
// Serve frontend/ with Vite and open tests/fixtures/fleet-traffic-preview.html?scene=prod.
// Query: scene=prod|few|solo|crowd|heavy|quiet|degraded|disabled|public|empty, theme=dark|light,
// w=<frame width>, run=0 freezes the rates (timestamps keep advancing), nav=0 hides the scene bar,
// delay=<milliseconds> delays content reads for entrance-motion acceptance;
// auth_delay=<milliseconds> delays only the authorization snapshot so SSE can arrive first;
// machines=1..48 sets the machine count in the heavy scene.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ConsoleInitialData, UsageMonthlySummary, Whoami } from '../../src/api';
import type { FleetTrafficNode, FleetTrafficSnapshot } from '../../src/fleet-traffic-source';
import type { NodeRealtimeEvent, RealtimeUserTrafficRate } from '../../src/node-realtime';
import { UsagePane } from '../../src/panes/usage';
import { SessionProvider } from '../../src/session';
import { theme } from '../../src/forge/theme';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = params.get('palette') ?? 'dailan';
const scene = params.get('scene') ?? 'prod';
const width = Number(params.get('w')) || 1280;
const frozen = params.get('run') === '0';
const showNav = params.get('nav') !== '0';
const readDelay = Math.max(0, Math.min(2000, Number(params.get('delay')) || 0));
const authorizationDelay = Math.max(0, Math.min(60_000, Number(params.get('auth_delay')) || 0));

/** 1 Mb/s in the bytes-per-second unit the realtime protocol carries. */
const MB = 1_000_000 / 8;

interface Machine {
  id: string;
  name: string;
  country: string | null;
  /** NIC bytes that no user owns: relay hops, system traffic. */
  relay?: number;
  condition?: 'offline' | 'partial' | 'unsupported';
}
interface Person {
  id: string;
  tenant: string;
  grants: string[];
}
interface Flow {
  user: string;
  node: string;
  down: number;
  up: number;
  /** Share of time the flow carries traffic; absent means continuous. */
  duty?: number;
  phase?: number;
}
interface World {
  machines: Machine[];
  users: Person[];
  flows: Flow[];
}

const PROD_MACHINES: Machine[] = [
  { id: 'hk-hkix', name: '香港 HKIX', country: 'HK' },
  { id: 'hk-bgp', name: '香港 BGP 三网', country: 'HK', relay: 18 * MB },
  { id: 'hk-cmi', name: '香港 CMI', country: 'HK' },
  { id: 'hk-home', name: '香港 家宽', country: 'HK' },
  { id: 'mo-ctm1', name: '澳门 CTM', country: 'MO' },
  { id: 'mo-ctm2', name: '澳门 CTM II', country: 'MO' },
  { id: 'mo-ctm3', name: '澳门 CTM III', country: 'MO' },
  { id: 'mo-ctm4', name: '澳门 CTM IV', country: 'MO' },
  { id: 'tw-hinet', name: '台湾 HiNet', country: 'TW' },
  { id: 'tw-home', name: '台湾 家宽', country: 'TW', condition: 'offline' },
  { id: 'jp-iij', name: '东京 IIJ', country: 'JP' },
  { id: 'jp-ntt', name: '东京 NTT', country: 'JP' },
  { id: 'jp-sb', name: '东京 软银', country: 'JP' },
  { id: 'jp-bgp', name: '东京 BGP', country: 'JP', relay: 9 * MB },
  { id: 'jp-kddi', name: '东京 KDDI', country: 'JP' },
  { id: 'jp-osaka', name: '大阪 NTT', country: 'JP' },
  { id: 'us-gia', name: '洛杉矶 CN2 GIA 4837', country: 'US' },
  { id: 'us-9929', name: '洛杉矶 9929', country: 'US' },
  { id: 'us-cmin2', name: '洛杉矶 CMIN2', country: 'US' },
  { id: 'us-t1', name: '洛杉矶 Tier 1', country: 'US' },
  { id: 'us-sjc', name: '圣何塞 云主机', country: 'US', condition: 'partial' },
  { id: 'my-jb', name: '新山 BGP', country: 'MY' },
  { id: 'de-fra', name: '法兰克福 标准', country: 'DE', condition: 'unsupported' },
  { id: 'gb-lon', name: '伦敦 家宽', country: 'GB' },
];
const ALL_PROD = PROD_MACHINES.map(machine => machine.id);
const PROD_USERS: Person[] = [
  { id: 'alice', tenant: 'platform', grants: ALL_PROD },
  {
    id: 'bob',
    tenant: 'platform',
    grants: ['hk-hkix', 'hk-bgp', 'hk-cmi', 'hk-home', 'jp-iij', 'jp-ntt', 'jp-sb', 'jp-bgp', 'jp-kddi', 'jp-osaka'],
  },
  { id: 'carol', tenant: 'platform', grants: ['hk-hkix', 'hk-bgp', 'us-gia', 'us-9929'] },
  { id: 'dave', tenant: 'platform', grants: ['jp-iij', 'jp-ntt', 'jp-sb', 'tw-hinet'] },
  { id: 'erin', tenant: 'platform', grants: ['us-gia', 'us-9929', 'us-cmin2', 'us-t1', 'us-sjc'] },
  { id: 'frank', tenant: 'platform', grants: ['hk-hkix', 'mo-ctm1', 'mo-ctm2'] },
  {
    id: 'grace',
    tenant: 'platform',
    grants: [
      'hk-hkix',
      'hk-bgp',
      'hk-cmi',
      'hk-home',
      'mo-ctm1',
      'mo-ctm2',
      'mo-ctm3',
      'mo-ctm4',
      'tw-hinet',
      'tw-home',
    ],
  },
  { id: 'heidi', tenant: 'platform', grants: ['de-fra', 'gb-lon', 'hk-hkix'] },
  {
    id: 'ivan',
    tenant: 'platform',
    grants: ['jp-iij', 'jp-ntt', 'jp-sb', 'jp-bgp', 'jp-kddi', 'jp-osaka', 'us-gia', 'us-9929'],
  },
  { id: 'judy', tenant: 'platform', grants: ['hk-bgp', 'hk-cmi', 'jp-iij'] },
  { id: 'mallory', tenant: 'studio', grants: ['my-jb', 'hk-hkix', 'hk-home'] },
  { id: 'niaj', tenant: 'studio', grants: ['us-cmin2', 'us-t1', 'jp-bgp'] },
  { id: 'olivia', tenant: 'studio', grants: ['hk-hkix', 'hk-bgp', 'hk-cmi', 'jp-iij', 'jp-ntt', 'us-gia'] },
  {
    id: 'peggy',
    tenant: 'studio',
    grants: ['tw-hinet', 'tw-home', 'hk-cmi', 'jp-kddi', 'mo-ctm3', 'us-sjc', 'gb-lon'],
  },
];
const PROD_FLOWS: Flow[] = [
  { user: 'alice', node: 'hk-hkix', down: 38 * MB, up: 1.2 * MB, phase: 0 },
  { user: 'alice', node: 'jp-iij', down: 2.4 * MB, up: 0.3 * MB, duty: 0.55, phase: 1 },
  { user: 'bob', node: 'jp-ntt', down: 126 * MB, up: 2.1 * MB, phase: 2 },
  { user: 'grace', node: 'mo-ctm1', down: 3.1 * MB, up: 2.6 * MB, phase: 3 },
  { user: 'ivan', node: 'us-gia', down: 12.5 * MB, up: 0.6 * MB, phase: 4 },
  { user: 'ivan', node: 'jp-bgp', down: 0.8 * MB, up: 0.1 * MB, duty: 0.6, phase: 5 },
  { user: 'olivia', node: 'hk-bgp', down: 0.04 * MB, up: 0.02 * MB, phase: 6 },
  { user: 'erin', node: 'us-9929', down: 6.2 * MB, up: 0.4 * MB, duty: 0.5, phase: 7 },
  { user: 'heidi', node: 'gb-lon', down: 0.32 * MB, up: 0.05 * MB, phase: 8 },
  { user: 'peggy', node: 'us-sjc', down: 4.4 * MB, up: 0.2 * MB, phase: 9 },
];

const CITIES: [string, string][] = [
  ['香港', 'HK'],
  ['澳门', 'MO'],
  ['台北', 'TW'],
  ['东京', 'JP'],
  ['大阪', 'JP'],
  ['首尔', 'KR'],
  ['新加坡', 'SG'],
  ['洛杉矶', 'US'],
  ['圣何塞', 'US'],
  ['法兰克福', 'DE'],
];
const CARRIERS = ['BGP', 'CN2', 'CMI', 'IIJ', 'NTT', 'Tier 1'];

function crowdWorld(): World {
  const machines: Machine[] = Array.from({ length: 40 }, (_, index) => {
    const [city, country] = CITIES[index % CITIES.length];
    const carrier = CARRIERS[Math.floor(index / CITIES.length) % CARRIERS.length];
    return {
      id: `m${String(index).padStart(2, '0')}`,
      name: `${city} ${carrier}${index >= 30 ? ' II' : ''}`,
      country,
      condition: index === 7 ? 'offline' : index === 23 ? 'partial' : undefined,
    };
  });
  const users: Person[] = Array.from({ length: 120 }, (_, index) => ({
    id: `user-${String(index + 1).padStart(3, '0')}`,
    tenant: index % 9 === 0 ? 'studio' : 'platform',
    grants: machines
      .filter((_, machine) => (machine * 7 + index * 13) % 11 < 2 + (index % 4))
      .map(machine => machine.id),
  }));
  const flows: Flow[] = users
    .filter((_, index) => index % 5 === 0)
    .map((user, index) => ({
      user: user.id,
      node: user.grants[index % Math.max(1, user.grants.length)] ?? machines[0].id,
      down: (0.2 + ((index * 37) % 50)) * MB,
      up: (0.05 + ((index * 11) % 5) / 4) * MB,
      duty: index % 3 === 0 ? 0.5 : undefined,
      phase: index,
    }));
  return { machines, users, flows };
}

const HEAVY_CARRIERS = ['CN2 GIA 4837 高防', 'BGP 三网优化', 'CMIN2 精品', 'IIJ 原生 IP', 'Tier 1 大带宽'];

/** Large readings and long names at once: Gbit/s rates, TiB months, carrier-length machine names,
 * an email-length top user, and more machines than the matrix shows without scrolling. */
function heavyWorld(): World {
  const machineCount = Math.max(1, Math.min(48, Number(params.get('machines')) || 48));
  const machines: Machine[] = Array.from({ length: machineCount }, (_, index) => {
    const [city, country] = CITIES[index % CITIES.length];
    const carrier = HEAVY_CARRIERS[Math.floor(index / CITIES.length) % HEAVY_CARRIERS.length];
    return {
      id: `h${String(index).padStart(2, '0')}`,
      name: `${city} ${carrier} ${String(index + 1).padStart(2, '0')}`,
      country,
      relay: 2_000 * MB,
      condition: index === 7 ? 'offline' : index === 23 ? 'partial' : undefined,
    };
  });
  const users: Person[] = Array.from({ length: 160 }, (_, index) => ({
    id: index === 0 ? 'enterprise-customer-0001@example.com' : `user-${String(index + 1).padStart(4, '0')}`,
    tenant: 'platform',
    grants: machines
      .filter((_, machine) => machine === 0 || (machine * 7 + index * 13) % 11 < 2 + (index % 4))
      .map(machine => machine.id),
  }));
  const flows: Flow[] = users
    .filter((_, index) => index % 4 === 0)
    .flatMap((user, index) =>
      user.grants.slice(0, 3).map((node, step) => ({
        user: user.id,
        node,
        down: (index === 0 && step === 0 ? 20_000 : 600 + ((index * 37 + step * 11) % 40) * 100) * MB,
        up: (150 + ((index * 11 + step) % 9) * 60) * MB,
        phase: index + step,
      })),
    );
  return { machines, users, flows };
}

function worldOf(name: string): World {
  if (name === 'crowd') return crowdWorld();
  if (name === 'heavy') return heavyWorld();
  if (name === 'few')
    return {
      machines: PROD_MACHINES.filter(machine => ['hk-hkix', 'jp-iij', 'us-gia'].includes(machine.id)),
      users: [
        { id: 'alice', tenant: 'platform', grants: ['hk-hkix', 'jp-iij', 'us-gia'] },
        { id: 'bob', tenant: 'platform', grants: ['jp-iij'] },
      ],
      flows: [
        { user: 'alice', node: 'hk-hkix', down: 24 * MB, up: 0.9 * MB },
        { user: 'bob', node: 'jp-iij', down: 3.2 * MB, up: 0.4 * MB, duty: 0.7, phase: 2 },
      ],
    };
  if (name === 'solo')
    return {
      machines: PROD_MACHINES.filter(machine => machine.id === 'hk-hkix'),
      users: [{ id: 'alice', tenant: 'platform', grants: ['hk-hkix'] }],
      flows: [{ user: 'alice', node: 'hk-hkix', down: 18 * MB, up: 0.7 * MB }],
    };
  if (name === 'empty') return { machines: PROD_MACHINES, users: [], flows: [] };
  if (name === 'quiet') return { machines: PROD_MACHINES, users: PROD_USERS, flows: [] };
  return { machines: PROD_MACHINES, users: PROD_USERS, flows: PROD_FLOWS };
}

const world = worldOf(scene);
const startedAt = Date.now();
const clock = () => (frozen ? 37 : 37 + (Date.now() - startedAt) / 1000);

function flowRate(flow: Flow, t: number) {
  const phase = flow.phase ?? 0;
  if (flow.duty !== undefined && (Math.sin(t / 4.2 + phase * 1.9) + 1) / 2 > flow.duty) return { down: 0, up: 0 };
  const swing = 0.8 + 0.14 * Math.sin(t / 3.1 + phase) + 0.06 * Math.sin(t / 1.3 + phase * 2.7);
  return { down: Math.round(flow.down * swing), up: Math.round(flow.up * (0.9 + (swing - 0.8))) };
}

function nodeEvent(machine: Machine, at: number): NodeRealtimeEvent {
  const t = clock();
  const users: RealtimeUserTrafficRate[] = [];
  let attributed = 0;
  for (const flow of world.flows) {
    if (flow.node !== machine.id) continue;
    const rate = flowRate(flow, t);
    if (rate.down + rate.up === 0) continue;
    const person = world.users.find(user => user.id === flow.user)!;
    users.push({
      tenant_id: person.tenant,
      user_id: person.id,
      ingress_id: `${machine.id}-reality`,
      downlink_bytes_per_sec: rate.down,
      uplink_bytes_per_sec: rate.up,
    });
    attributed += rate.down + rate.up;
  }
  const relay = (machine.relay ?? 0.04 * MB) * (0.9 + 0.1 * Math.sin(t / 2.3));
  return {
    node_id: machine.id,
    received_at_unix_millis: at,
    sample: {
      sequence: Math.floor(t),
      sampled_at_unix_millis: at - 140,
      rx_bytes_per_sec: Math.round(attributed * 1.03 + relay),
      tx_bytes_per_sec: Math.round(attributed * 1.05 + relay * 0.97),
      has_gap: false,
      user_traffic:
        machine.condition === 'unsupported'
          ? undefined
          : {
              state: 'ready',
              all_users: true,
              users,
              truncated: machine.condition === 'partial',
              has_gap: false,
              forwarding_supported: true,
              forwarding_expected: false,
            },
    },
  };
}

function fleetNodes(): FleetTrafficNode[] {
  const now = Date.now();
  return world.machines.map(machine => {
    const offline = machine.condition === 'offline';
    return {
      node_id: machine.id,
      connected: !offline,
      active: !offline,
      interval_secs: 1,
      samples: [nodeEvent(machine, offline ? now - 45_000 : now)],
    };
  });
}

function snapshot(): FleetTrafficSnapshot {
  return {
    policy: { enabled: scene !== 'disabled' },
    nodes: fleetNodes(),
    expected: world.machines.map(machine => ({
      node_id: machine.id,
      users: world.users
        .filter(user => user.grants.includes(machine.id))
        .map(user => ({ tenant_id: user.tenant, user_id: user.id })),
    })),
  };
}

const streamDown = scene === 'degraded';
let snapshotReads = 0;

class FixtureEventSource {
  onerror: (() => void) | null = null;
  private listeners = new Map<string, ((event: MessageEvent) => void)[]>();
  private timer: ReturnType<typeof setInterval> | undefined;

  constructor(readonly url: string) {
    const initialDelay = Math.max(30, readDelay);
    setTimeout(() => {
      if (streamDown) {
        this.onerror?.();
        return;
      }
      const { expected: _expected, ...payload } = snapshot();
      this.emit('snapshot', payload);
      this.timer = setInterval(() => {
        const now = Date.now();
        for (const machine of world.machines) {
          if (machine.condition !== 'offline') this.emit('sample', nodeEvent(machine, now));
        }
      }, 1_000);
    }, initialDelay);
  }

  addEventListener(type: string, listener: (event: MessageEvent) => void) {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }

  close() {
    if (this.timer) clearInterval(this.timer);
  }

  private emit(type: string, data: unknown) {
    const event = new MessageEvent(type, { data: JSON.stringify(data) });
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
}
(globalThis as { EventSource: unknown }).EventSource = FixtureEventSource;

const who: Whoami = {
  operator_id: scene === 'public' ? 'public' : 'fixture-admin',
  role: scene === 'public' ? 'readonly' : 'system-admin',
  tenant_scope: null,
  token_prefix: null,
  masked_assets: scene === 'public',
};
const initial: ConsoleInitialData = { node_count: world.machines.length, chain_group_count: [] };

function monthly(offset: string | null): UsageMonthlySummary {
  const previous = offset === '-1';
  const month = previous ? '2026-09' : '2026-10';
  const daysInMonth = previous ? 30 : 19;
  // The heavy scene carries hundreds of TiB a month, so every ledger reading is at its widest.
  const unit = scene === 'heavy' ? 400 * 1024 ** 3 : 1024 ** 3;
  const days = Array.from({ length: daysInMonth }, (_, index) => {
    const downlink = Math.round((18 + 9 * Math.sin(index / 2.3) + (index % 7 === 5 ? 14 : 0)) * unit);
    return {
      day: `${month}-${String(index + 1).padStart(2, '0')}`,
      uplink_bytes: Math.round(downlink * 0.06),
      downlink_bytes: downlink,
      has_gap: false,
    };
  });
  const total = days.reduce((sum, day) => sum + day.downlink_bytes, 0);
  return {
    month_start: `${month}-01 00:00:00`,
    month_end: previous ? '2026-10-01 00:00:00' : '2026-11-01 00:00:00',
    views: world.users.slice(0, 12).map((user, index) => ({
      tenant_id: user.tenant,
      user_id: user.id,
      app_id: index % 3 === 0 ? 'direct' : 'relay',
      uplink_bytes: Math.round((total * 0.06) / (index + 2)),
      downlink_bytes: Math.round(total / (index + 2)),
      has_gap: false,
    })),
    days,
  };
}

globalThis.fetch = async (input: RequestInfo | URL) => {
  const url = new URL(input instanceof Request ? input.url : String(input), location.origin);
  const path = url.pathname;
  if (readDelay > 0 && (path === '/usage/monthly-summary' || path === '/realtime/fleet-traffic/snapshot')) {
    await new Promise(resolve => setTimeout(resolve, readDelay));
  }
  if (path === '/whoami') return Response.json(who);
  if (path === '/nodes/agent-state')
    return Response.json({
      nodes: world.machines.map(machine => ({
        node_id: machine.id,
        tenant_id: 'platform',
        name: machine.name,
        public_ipv4: null,
        public_ipv4_country: machine.country,
        lifecycle_phase: 'active',
      })),
    });
  if (path === '/users')
    return Response.json({
      users: world.users.map(user => ({
        tenant_id: user.tenant,
        id: user.id,
        status: 'active',
        created_at: '2026-09-01T00:00:00Z',
        created_revision: 1,
      })),
    });
  if (path === '/usage/monthly-summary') return Response.json(monthly(url.searchParams.get('month_offset')));
  if (path === '/realtime/fleet-traffic/snapshot') {
    if (authorizationDelay > 0) await new Promise(resolve => setTimeout(resolve, authorizationDelay));
    snapshotReads += 1;
    if (streamDown && snapshotReads > 1) return Response.json({ error: 'fixture stream down' }, { status: 503 });
    return Response.json(snapshot());
  }
  return Response.json({ error: 'Unknown fixture request' }, { status: 404 });
};

const SCENES: Record<string, string> = {
  prod: '生产规模 24 × 14',
  few: '三台 · 两位',
  solo: '一台 · 一位',
  crowd: '40 台 · 120 位',
  heavy: '大流量 · 长名称',
  quiet: '全部空闲',
  degraded: '实时连接中断',
  disabled: '已关闭',
  public: '访客',
  empty: '没有用户',
};

createRoot(document.getElementById('root')!).render(
  <QueryClientProvider
    client={new QueryClient({ defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false } } })}
  >
    <SessionProvider value={{ who, initial }} onIdentityChange={() => {}}>
      {showNav && (
        <nav
          aria-label="验收场景"
          style={{ display: 'flex', gap: 12, flexWrap: 'wrap', padding: '12px 24px 0', fontSize: 12 }}
        >
          <span>本地示例 · 生产组件</span>
          {Object.entries(SCENES).map(([value, label]) => (
            <a key={value} href={`?scene=${value}&theme=${params.get('theme') ?? 'dark'}&w=${width}`}>
              {label}
            </a>
          ))}
          <a href={`?scene=${scene}&theme=${params.get('theme') === 'light' ? 'dark' : 'light'}&w=${width}`}>
            切换主题
          </a>
        </nav>
      )}
      <div className="fg-view" style={{ width: `min(100%, ${width}px)`, margin: '0 auto' }}>
        <UsagePane />
      </div>
    </SessionProvider>
  </QueryClientProvider>,
);
