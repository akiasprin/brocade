// Synthetic backend for the machine-page 来源 IP 封禁 mockup. Never imported by the app.
// Reuses the palette mockup's fleet and answers the ban list of one machine (香港 HKIX) per scene, the users'
// own lists behind the user links, and presence so a typed address can be recognised as an online source.
import type { NetworkOperator, SourceAccessNode, SourceAccessView, UserPresenceList } from '../../src/api';
import { installBackend as installFleet } from './palette-mockup-data';

export const NODE = 'hk-hkix';

export type SceneKey = 'typical' | 'useronly' | 'empty' | 'many' | 'pending' | 'error' | 'unsupported' | 'legacy';

export const SCENES: Record<SceneKey, { label: string }> = {
  typical: { label: '常见：本机 2 · 用户 4' },
  useronly: { label: '本机为空，仅用户名单' },
  empty: { label: '全部为空' },
  many: { label: '名单较长：本机 14 · 用户 9' },
  pending: { label: '同步中' },
  error: { label: '执行异常' },
  unsupported: { label: '待升级' },
  legacy: { label: '旧 Console（无用户名单）' },
};
export const SCENE_KEYS = Object.keys(SCENES) as SceneKey[];

/** 本地 GeoIP 的示例结果；地址都取文档保留段。 */
const LOCATION: Record<string, [string, NetworkOperator?]> = {
  '203.0.113.17': ['RU'],
  '192.0.2.146': ['NL'],
  '198.51.100.77': ['VN'],
  '2001:db8:abcd:ffff:1111:2222:3333:4444': ['NL'],
  '203.0.113.24': ['CN', 'chinanet'],
  '198.51.100.9': ['CN', 'cmcc'],
  '2001:db8:4c1a:20:5a2f:1bff:fe33:c901': ['CN', 'unicom'],
  '192.0.2.33': ['US'],
  '192.0.2.34': ['US'],
  '192.0.2.88': ['DE'],
  '192.0.2.201': ['SG'],
  '198.51.100.140': ['BR'],
  '198.51.100.141': ['BR'],
  '198.51.100.203': ['IN'],
  '203.0.113.60': ['KR'],
  '203.0.113.61': ['CN', 'unicom'],
  '203.0.113.99': ['TR'],
  '203.0.113.150': ['ID'],
  '203.0.113.151': ['ID'],
  '2001:db8:9:42::17': ['FR'],
  '203.0.113.240': ['CN', 'cmcc'],
  '198.51.100.31': ['TW'],
};

type UserBan = { tenant_id: string; user_id: string; ips: string[] };

const TYPICAL_NODE = ['203.0.113.17', '192.0.2.146'];
const TYPICAL_USERS: UserBan[] = [
  { tenant_id: 'platform', user_id: 'alice', ips: ['203.0.113.17', '2001:db8:abcd:ffff:1111:2222:3333:4444'] },
  { tenant_id: 'platform', user_id: 'bob', ips: ['198.51.100.77'] },
  { tenant_id: 'studio', user_id: 'niaj', ips: ['203.0.113.17'] },
];
const MANY_NODE = [
  '203.0.113.17',
  '192.0.2.146',
  '192.0.2.33',
  '192.0.2.34',
  '192.0.2.88',
  '192.0.2.201',
  '198.51.100.140',
  '198.51.100.141',
  '198.51.100.203',
  '203.0.113.60',
  '203.0.113.99',
  '203.0.113.150',
  '203.0.113.151',
  '2001:db8:9:42::17',
];
const MANY_USERS: UserBan[] = [
  ...TYPICAL_USERS.slice(0, 2),
  { tenant_id: 'platform', user_id: 'carol', ips: ['203.0.113.61', '203.0.113.240'] },
  { tenant_id: 'platform', user_id: 'grace', ips: ['198.51.100.31'] },
  { tenant_id: 'platform', user_id: 'ivan', ips: ['203.0.113.99'] },
  TYPICAL_USERS[2],
];

/** 当前在这台机器上在线的来源：输入框认出这些地址时写出地区和谁在用。 */
const ONLINE: { tenant_id: string; user_id: string; ip: string }[] = [
  { tenant_id: 'platform', user_id: 'alice', ip: '203.0.113.24' },
  { tenant_id: 'platform', user_id: 'bob', ip: '203.0.113.24' },
  { tenant_id: 'platform', user_id: 'carol', ip: '198.51.100.9' },
  { tenant_id: 'studio', user_id: 'olivia', ip: '2001:db8:4c1a:20:5a2f:1bff:fe33:c901' },
];

const WAIT = '等待 Xray 应用接入策略';
const FAILURE = 'Xray 接入策略状态异常，请检查本地策略和回执';
const UNSUPPORTED = 'Xray 尚不支持来源封禁，请升级 Agent 和 Xray';

interface World {
  revision: number;
  ips: string[];
  users: UserBan[] | undefined;
  phase: SourceAccessNode['phase'];
  error: string | null;
  /** 场景固有的状态：保存后先「同步中」，随后回到这个状态。 */
  settled: SourceAccessNode['phase'];
  settledError: string | null;
}

function worldOf(scene: SceneKey): World {
  const ips = scene === 'many' ? MANY_NODE : scene === 'useronly' || scene === 'empty' ? [] : TYPICAL_NODE;
  const users = scene === 'legacy' ? undefined : scene === 'empty' ? [] : scene === 'many' ? MANY_USERS : TYPICAL_USERS;
  const phase: SourceAccessNode['phase'] =
    scene === 'pending' ? 'pending' : scene === 'error' ? 'error' : scene === 'unsupported' ? 'unsupported' : 'applied';
  const error = scene === 'pending' ? WAIT : scene === 'error' ? FAILURE : scene === 'unsupported' ? UNSUPPORTED : null;
  return {
    revision: 7,
    ips: [...ips],
    users: users?.map(user => ({ ...user, ips: [...user.ips] })),
    phase,
    error,
    settled: scene === 'pending' ? 'applied' : phase,
    settledError: scene === 'pending' ? null : error,
  };
}

let world = worldOf('typical');
let settleTimer: ReturnType<typeof setTimeout> | undefined;

export function resetWorld(scene: SceneKey) {
  clearTimeout(settleTimer);
  world = worldOf(scene);
}

const located = (ips: string[]) => ({
  source_countries: ips.filter(ip => LOCATION[ip]).map(ip => ({ ip, country: LOCATION[ip][0] })),
  source_operators: ips.filter(ip => LOCATION[ip]?.[1]).map(ip => ({ ip, operator: LOCATION[ip][1]! })),
});

/** 节点视图。生产 Console 只给节点名单本身附归属；这里连同用户名单一起附上（方案需要 Console 补这一处）。 */
function nodeView(): SourceAccessView {
  const all = [...world.ips, ...(world.users ?? []).flatMap(user => user.ips)];
  return {
    revision: world.revision,
    ips: [...world.ips],
    nodes: [{ node_id: NODE, phase: world.phase, error: world.error }],
    user_bans: world.users?.filter(user => user.ips.length > 0).map(user => ({ ...user, ips: [...user.ips] })),
    ...located([...new Set(all)]),
  };
}

function userView(tenant: string, user: string): SourceAccessView {
  const ips = world.users?.find(item => item.tenant_id === tenant && item.user_id === user)?.ips ?? [];
  return {
    revision: 3,
    ips: [...ips],
    nodes: [{ node_id: NODE, phase: world.phase, error: world.error }],
    ...located(ips),
  };
}

function presence(): UserPresenceList {
  const now = Date.now();
  const iso = (ms: number) => new Date(ms).toISOString();
  const users = new Map<string, UserPresenceList['users'][number]>();
  for (const source of ONLINE) {
    const key = `${source.tenant_id}/${source.user_id}`;
    const entry = users.get(key) ?? {
      tenant_id: source.tenant_id,
      user_id: source.user_id,
      state: 'complete' as const,
      expected_nodes: 1,
      reporting_nodes: 1,
      sources: [],
    };
    entry.sources.push({
      ip: source.ip,
      first_observed_at: iso(now - 3 * 3600_000),
      last_observed_at: iso(now - 10_000),
      xray_last_seen_at: iso(now - 10_000),
      online_since_at: iso(now - 47 * 60_000),
      session_uplink_bytes: 40_000_000,
      session_downlink_bytes: 1_300_000_000,
      node_ids: [NODE],
      ingress_ids: [],
    });
    users.set(key, entry);
  }
  return { freshness_secs: 120, users: [...users.values()], ...located(ONLINE.map(source => source.ip)) };
}

/** 与 Console 的 normalize_ip 同一结论：只接受单个 IP，IPv4-mapped IPv6 合并为 IPv4。 */
function normalize(raw: string): string | null {
  const value = raw.trim();
  if (/^\d{1,3}(\.\d{1,3}){3}$/.test(value)) {
    const parts = value.split('.').map(Number);
    return parts.every(part => part <= 255) ? parts.join('.') : null;
  }
  if (!value.includes(':') || /[\s/%[\]]/.test(value)) return null;
  try {
    return new URL(`http://[${value}]/`).hostname.slice(1, -1);
  } catch {
    return null;
  }
}

function saveNode(body: { revision: number; ips: string[] }): Response {
  if (body.revision !== world.revision) {
    return Response.json({ error: '名单已被他人修改，请重新读取后再保存' }, { status: 409 });
  }
  const ips: string[] = [];
  for (const raw of body.ips) {
    const ip = normalize(raw);
    if (!ip) return Response.json({ error: '请输入有效的 IPv4 或 IPv6 地址（不含端口或网段）' }, { status: 400 });
    if (!ips.includes(ip)) ips.push(ip);
  }
  if (ips.length > 256) return Response.json({ error: '封禁名单最多 256 个 IP，且版本必须有效' }, { status: 400 });
  world.ips = ips;
  world.revision += 1;
  // 保存后这台机器先回到「同步中」，约 4 秒后确认（与 Agent 约 3 秒复查一致）。
  world.phase = world.settled === 'unsupported' ? 'unsupported' : 'pending';
  world.error = world.settled === 'unsupported' ? UNSUPPORTED : WAIT;
  clearTimeout(settleTimer);
  settleTimer = setTimeout(() => {
    world.phase = world.settled;
    world.error = world.settledError;
  }, 4_000);
  return Response.json(nodeView());
}

export function installBackend() {
  installFleet();
  const fleet = globalThis.fetch;
  globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const raw = input instanceof Request ? input.url : String(input);
    const url = new URL(raw, location.origin);
    const parts = url.pathname.split('/').map(decodeURIComponent);
    if (parts[1] === 'nodes' && parts[2] === NODE && parts[3] === 'source-access' && parts.length === 4) {
      await new Promise(resolve => setTimeout(resolve, 160));
      if (init?.method === 'PUT') return saveNode(JSON.parse(String(init.body)));
      return Response.json(nodeView());
    }
    if (parts[1] === 'nodes' && parts[3] === 'source-access' && parts.length === 4) {
      return Response.json({ revision: 0, ips: [], nodes: [], user_bans: [] });
    }
    if (parts[1] === 'users' && parts[4] === 'source-access' && parts.length === 5) {
      return Response.json(userView(parts[2], parts[3]));
    }
    if (url.pathname === '/users/presence') return Response.json(presence());
    // 同页其他卡片的读数：只为让周围的卡片完整显示。
    if (url.pathname === '/certs') return Response.json({ groups: [], nodes: [] });
    if (url.pathname === '/links/mtu') return Response.json({ links: [], nodes: [], default_mtu: 1420 });
    if (url.pathname === '/agent-log-policy') {
      const limits = { agent_journal_mib: 64, xray_mib: 128, phantun_mib: 16 };
      const empty = { agent_journal_mib: null, xray_mib: null, phantun_mib: null };
      return Response.json({
        global: limits,
        nodes: [{ node_id: NODE, tenant_id: 'platform', name: '香港 HKIX', overrides: empty, effective: limits }],
      });
    }
    return fleet(input, init);
  };
}
