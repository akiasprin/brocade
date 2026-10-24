// Synthetic lines, quotas and exhaustion policies for the 用量与额度 status mockup. Machine names, chains and
// usage are made up for layout only.
import type { UserTrafficPolicy, UserTrafficPolicyView } from '../../src/api';

export const GiB = 1024 ** 3;
export const MBPS = 1_000_000;

export const MACHINES = [
  { node_id: 'n-hk1', name: '香港-01' },
  { node_id: 'n-jp1', name: '东京-01' },
  { node_id: 'n-tw1', name: '台北-01' },
  { node_id: 'n-us1', name: '洛杉矶-01' },
  { node_id: 'n-de1', name: '法兰克福-01' },
  { node_id: 'n-nl1', name: '阿姆斯特丹-01' },
] as const;
export const machineName = (id: string) => MACHINES.find(machine => machine.node_id === id)?.name ?? id;

export interface LineChain {
  id: string;
  name: string;
  country: string;
  node: string;
}

/** One app (线路) in the selected user's quota list. `used` already includes the accounting multiplier. */
export interface Line {
  id: string;
  label: string;
  chains: LineChain[];
  quota: number | null;
  used: number;
  action: UserTrafficPolicy['exhausted_action'] | null;
  /** Machines that have not confirmed the user's throttle policy yet. */
  pending: string[];
}

export interface World {
  lines: Line[];
  serviceAvailable: boolean;
  serviceState: 'active' | 'expired';
  expiresAt: string | null;
}

const LINES: Record<string, Line> = {
  apac: {
    id: 'apac',
    label: 'APAC Premium',
    chains: [
      { id: 'hk-01', name: 'HK-01', country: 'HK', node: 'n-hk1' },
      { id: 'jp-01', name: 'JP-01', country: 'JP', node: 'n-jp1' },
      { id: 'tw-01', name: 'TW-01', country: 'TW', node: 'n-tw1' },
    ],
    quota: 500 * GiB,
    used: 121 * GiB,
    action: { kind: 'throttle', rate_bps: 5 * MBPS },
    pending: [],
  },
  jp: {
    id: 'jp',
    label: 'JP Budget',
    chains: [{ id: 'jp-budget', name: 'JP-Budget', country: 'JP', node: 'n-jp1' }],
    quota: 50 * GiB,
    used: 44.2 * GiB,
    action: { kind: 'suspend' },
    pending: [],
  },
  us: {
    id: 'us',
    label: 'US West',
    chains: [
      { id: 'us-lax', name: 'US-LAX', country: 'US', node: 'n-us1' },
      { id: 'us-sjc', name: 'US-SJC', country: 'US', node: 'n-us1' },
    ],
    quota: 100 * GiB,
    used: 121.4 * GiB,
    action: { kind: 'throttle', rate_bps: 2 * MBPS },
    pending: [],
  },
  eu: {
    id: 'eu',
    label: 'EU Lite',
    chains: [
      { id: 'de-fra', name: 'DE-FRA', country: 'DE', node: 'n-de1' },
      { id: 'nl-ams', name: 'NL-AMS', country: 'NL', node: 'n-nl1' },
    ],
    quota: 30 * GiB,
    used: 30.4 * GiB,
    action: { kind: 'suspend' },
    pending: [],
  },
  hk: {
    id: 'hk',
    label: 'HK Direct',
    chains: [{ id: 'hk-direct', name: 'HK-Direct', country: 'HK', node: 'n-hk1' }],
    quota: null,
    used: 8.6 * GiB,
    action: null,
    pending: [],
  },
};
export const ALL_LINES = Object.values(LINES);

const line = (id: keyof typeof LINES, extra: Partial<Line> = {}): Line => ({ ...LINES[id], ...extra });

export const SCENE_KEYS = ['all', 'normal', 'single', 'throttled', 'suspended', 'pending', 'paused'] as const;
export type SceneKey = (typeof SCENE_KEYS)[number];

export const SCENES: Record<SceneKey, { label: string; world: () => World }> = {
  all: {
    label: '全部状态',
    world: () => ({
      lines: [line('apac', { pending: ['n-hk1'] }), line('jp'), line('us'), line('eu'), line('hk')],
      serviceAvailable: true,
      serviceState: 'active',
      expiresAt: null,
    }),
  },
  normal: {
    label: '常态',
    world: () => ({
      lines: [line('apac'), line('jp'), line('hk')],
      serviceAvailable: true,
      serviceState: 'active',
      expiresAt: null,
    }),
  },
  single: {
    label: '单条线路',
    world: () => ({ lines: [line('apac')], serviceAvailable: true, serviceState: 'active', expiresAt: null }),
  },
  throttled: {
    label: '限速中',
    world: () => ({
      lines: [line('us'), line('apac')],
      serviceAvailable: true,
      serviceState: 'active',
      expiresAt: null,
    }),
  },
  suspended: {
    label: '额度用满停用',
    world: () => ({
      lines: [line('eu'), line('jp')],
      serviceAvailable: true,
      serviceState: 'active',
      expiresAt: null,
    }),
  },
  pending: {
    label: '限速同步中',
    world: () => ({
      lines: [line('apac', { pending: ['n-hk1', 'n-jp1'] }), line('jp')],
      serviceAvailable: true,
      serviceState: 'active',
      expiresAt: null,
    }),
  },
  paused: {
    label: '服务到期暂停',
    world: () => ({
      lines: [line('apac'), line('jp')],
      serviceAvailable: false,
      serviceState: 'expired',
      expiresAt: '2026-10-19T16:00:00Z',
    }),
  },
};

export const exhaustedOf = (item: Line) => item.quota !== null && item.used >= item.quota;

/** The enforcer withdraws every ingress of a line whose quota ran out under 「用满后停用」. */
export const withdrawnOf = (item: Line) =>
  exhaustedOf(item) && item.action?.kind === 'suspend' ? item.chains.map(chain => `in-${chain.id}`) : [];

/** The production traffic-policy view for one line, as the Console would return it. */
export function viewOf(world: World, item: Line): UserTrafficPolicyView {
  return {
    kind: 'access',
    app_id: item.id,
    policy: item.action
      ? { quota_bytes: item.quota === null ? null : String(Math.round(item.quota)), exhausted_action: item.action }
      : null,
    used_bytes: String(Math.round(item.used)),
    exhausted: exhaustedOf(item),
    service_available: world.serviceAvailable,
    unsupported_nodes: [],
    // The Console reports synchronisation only for throttle policies.
    pending_nodes: item.action?.kind === 'throttle' ? item.pending.map(machineName) : [],
  };
}

/** What a row has to say about its exhaustion policy, in priority order. */
export type LineStatus =
  | { kind: 'paused' }
  | { kind: 'suspended'; withdrawn: number }
  | { kind: 'throttled'; rate: number }
  | { kind: 'normal' };

export function statusOf(world: World, item: Line): LineStatus {
  if (!world.serviceAvailable) return { kind: 'paused' };
  if (exhaustedOf(item) && item.action?.kind === 'suspend')
    return { kind: 'suspended', withdrawn: withdrawnOf(item).length };
  if (exhaustedOf(item) && item.action?.kind === 'throttle') return { kind: 'throttled', rate: item.action.rate_bps };
  return { kind: 'normal' };
}

/** The next cycle boundary in UTC+8; every mockup user resets on the 1st. */
export function resetText(now = Date.now()) {
  const local = new Date(now + 8 * 3_600_000);
  return `${((local.getUTCMonth() + 1) % 12) + 1} 月 1 日`;
}

export const rateText = (rate: number) => (rate >= MBPS ? `${rate / MBPS} Mbps` : `${rate / 1000} Kbps`);
