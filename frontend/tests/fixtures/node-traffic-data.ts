// Synthetic model for the machine traffic tab fixture (node-traffic-fixture.html). Never imported by the app.

/* ── 时间：固定在周日晚上，当天、当周、当月三档都有内容可看 ── */
export const GiB = 1024 ** 3;
export const HOUR = 3600;
export const DAY = 86_400;
const ZONE = 8 * 3600;
export const NOW = Date.UTC(2026, 9, 18, 13, 40) / 1000; // 2026-10-18 周日 21:40（UTC+8）
export const wall = (t: number) => new Date((t + ZONE) * 1000);
const two = (n: number) => String(n).padStart(2, '0');
export const hm = (t: number) => `${two(wall(t).getUTCHours())}:${two(wall(t).getUTCMinutes())}`;
export const md = (t: number) => `${two(wall(t).getUTCMonth() + 1)}-${two(wall(t).getUTCDate())}`;
export const weekday = (t: number) => `周${'日一二三四五六'[wall(t).getUTCDay()]}`;
export const dayStart = (t: number) => Math.floor((t + ZONE) / DAY) * DAY - ZONE;
const at = (month: number, date: number, hour = 0) => Date.UTC(2026, month - 1, date, hour) / 1000 - ZONE;

export type Period = 'day' | 'week' | 'month';
export const PERIODS = [
  { value: 'day', label: '当天' },
  { value: 'week', label: '当周' },
  { value: 'month', label: '当月' },
] as const satisfies readonly { value: Period; label: string }[];
export const periodLabel = (period: Period) => PERIODS.find(option => option.value === period)!.label;

export interface Bounds {
  period: Period;
  start: number;
  end: number;
  bucket: number;
  count: number;
}
export function boundsOf(period: Period): Bounds {
  const today = dayStart(NOW);
  const now = wall(NOW);
  let start = today;
  let end = today + DAY;
  if (period === 'week') {
    start = today - ((now.getUTCDay() + 6) % 7) * DAY;
    end = start + 7 * DAY;
  }
  if (period === 'month') {
    start = Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1) / 1000 - ZONE;
    end = Date.UTC(now.getUTCFullYear(), now.getUTCMonth() + 1, 1) / 1000 - ZONE;
  }
  const bucket = period === 'month' ? DAY : HOUR;
  return { period, start, end, bucket, count: Math.round((end - start) / bucket) };
}
export const periodDates = (b: Bounds) =>
  b.period === 'day' ? `${md(b.start)} ${weekday(b.start)}` : `${md(b.start)} – ${md(b.end - DAY)}`;
export const slotLabel = (b: Bounds, time: number) =>
  b.bucket === DAY ? `${md(time)} ${weekday(time)}` : `${md(time)} ${hm(time)}–${hm(time + b.bucket)}`;

/* ── 随机数：同一场景每次生成同一组数据 ── */
export function mulberry(seed: number) {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4_294_967_296;
  };
}
export function hash(text: string) {
  let value = 2_166_136_261;
  for (const char of text) value = Math.imul(value ^ char.charCodeAt(0), 16_777_619);
  return value >>> 0;
}
export const gauss = (random: () => number) =>
  Math.sqrt(-2 * Math.log(1 - random())) * Math.cos(2 * Math.PI * random());

const SHAPES = {
  evening: [
    0.55, 0.38, 0.24, 0.15, 0.1, 0.09, 0.12, 0.22, 0.38, 0.5, 0.56, 0.62, 0.7, 0.66, 0.62, 0.64, 0.7, 0.82, 1, 1.18,
    1.36, 1.45, 1.3, 0.92,
  ],
  office: [
    0.12, 0.08, 0.06, 0.05, 0.05, 0.06, 0.12, 0.3, 0.7, 1.1, 1.25, 1.2, 0.9, 1.1, 1.25, 1.2, 1.1, 0.85, 0.55, 0.4, 0.35,
    0.3, 0.25, 0.18,
  ],
  night: [
    1.2, 1.3, 1.25, 1.05, 0.7, 0.4, 0.2, 0.12, 0.1, 0.1, 0.14, 0.2, 0.26, 0.3, 0.32, 0.36, 0.42, 0.5, 0.62, 0.78, 0.92,
    1.04, 1.12, 1.18,
  ],
  flat: Array.from({ length: 24 }, () => 1),
};
type Shape = keyof typeof SHAPES;

export const FWD = 'fwd';
export const UNSPLIT = 'unsplit';

export interface ChainDef {
  id: string;
  name: string;
  country: string | null;
  via: string;
}
export interface UserDef {
  id: string;
  tenant: string;
}
interface PairDef {
  user: string;
  col: string;
  /** GiB per day while active. */
  daily: number;
  up?: number;
  shape?: Shape;
  days?: number;
  quietToday?: boolean;
  bursty?: boolean;
}
export interface World {
  key: string;
  chains: ChainDef[];
  users: UserDef[];
  /** user/col，含没有流量的授权：0 是「已授权、本范围没有流量」，空白是「未授权」。 */
  grants: Set<string>;
  pairs: PairDef[];
  machineGaps: [number, number][];
  pairGaps: { user: string; col: string; from: number; to: number }[];
  chainSplitSince: number | null;
  silentSince: number | null;
  sites: boolean;
}

const TYPICAL_CHAINS: ChainDef[] = [
  { id: 'hk-direct', name: '香港 01 直出', country: 'HK', via: '本机出网' },
  { id: 'jp-iij', name: '日本 02 IIJ', country: 'JP', via: '→ 东京 IIJ' },
  { id: 'us-gia', name: '美国 03 GIA', country: 'US', via: '→ 洛杉矶 CN2 GIA' },
  { id: 'tw-hinet', name: '台湾 04 HiNet', country: 'TW', via: '→ 台北 HiNet' },
  { id: 'sg-bgp', name: '新加坡 05', country: 'SG', via: '→ 新加坡 BGP' },
];
const TYPICAL_USERS: UserDef[] = [
  ...['alice', 'bob', 'carol', 'dave', 'frank', 'grace', 'heidi', 'judy'].map(id => ({ id, tenant: 'platform' })),
  ...['mallory', 'niaj', 'olivia', 'peggy'].map(id => ({ id, tenant: 'studio' })),
];
const TYPICAL_PAIRS: PairDef[] = [
  { user: 'alice', col: 'hk-direct', daily: 24 },
  { user: 'alice', col: 'jp-iij', daily: 2.2, shape: 'office', days: 0.8 },
  { user: 'alice', col: 'us-gia', daily: 0.5, days: 0.5 },
  { user: 'alice', col: 'tw-hinet', daily: 0.15, days: 0.3 },
  { user: 'alice', col: 'sg-bgp', daily: 1, shape: 'night', days: 0.6 },
  { user: 'bob', col: 'hk-direct', daily: 2.8 },
  { user: 'bob', col: 'jp-iij', daily: 29, shape: 'night', bursty: true },
  { user: 'carol', col: 'hk-direct', daily: 3.9, shape: 'office' },
  { user: 'carol', col: 'us-gia', daily: 7.2, up: 0.22, shape: 'night' },
  { user: 'dave', col: 'jp-iij', daily: 0.9, days: 0.45, quietToday: true },
  { user: 'frank', col: 'hk-direct', daily: 1.4 },
  { user: 'frank', col: FWD, daily: 8.8, up: 0.35, shape: 'flat' },
  { user: 'grace', col: 'hk-direct', daily: 5.2 },
  { user: 'grace', col: 'tw-hinet', daily: 3 },
  { user: 'heidi', col: 'hk-direct', daily: 0.32, days: 0.85 },
  { user: 'judy', col: 'hk-direct', daily: 2, shape: 'office' },
  { user: 'judy', col: 'jp-iij', daily: 0.8, shape: 'office', days: 0.7 },
  { user: 'mallory', col: 'hk-direct', daily: 11.5, bursty: true },
  { user: 'mallory', col: FWD, daily: 1.9, up: 0.4, shape: 'flat' },
  { user: 'olivia', col: 'hk-direct', daily: 0.04 },
  { user: 'olivia', col: 'jp-iij', daily: 0.5, days: 0.8 },
  { user: 'olivia', col: 'us-gia', daily: 1.7, shape: 'office' },
  { user: 'peggy', col: 'tw-hinet', daily: 5.6, shape: 'night' },
];
const TYPICAL_IDLE_GRANTS = [
  'bob/us-gia',
  'grace/jp-iij',
  'heidi/sg-bgp',
  'judy/tw-hinet',
  'olivia/sg-bgp',
  'niaj/us-gia',
  'niaj/hk-direct',
];

function typical(key: string, extra: Partial<World> = {}): World {
  return {
    key,
    chains: TYPICAL_CHAINS,
    users: TYPICAL_USERS,
    grants: new Set([...TYPICAL_PAIRS.map(pair => `${pair.user}/${pair.col}`), ...TYPICAL_IDLE_GRANTS]),
    pairs: TYPICAL_PAIRS,
    machineGaps: [],
    pairGaps: [],
    chainSplitSince: null,
    silentSince: null,
    sites: true,
    ...extra,
  };
}

function many(): World {
  const random = mulberry(20_261_018);
  const chains: ChainDef[] = [
    ...TYPICAL_CHAINS.slice(0, 2),
    { id: 'jp-ntt', name: '日本 06 NTT', country: 'JP', via: '→ 东京 NTT' },
    ...TYPICAL_CHAINS.slice(2),
    { id: 'kr-sk', name: '韩国 07 SK', country: 'KR', via: '→ 首尔 SK' },
    { id: 'gb-lon', name: '英国 08 家宽', country: 'GB', via: '→ 香港 BGP → 伦敦' },
    { id: 'warp', name: 'WARP 09', country: null, via: '本机经 WARP 出网' },
  ];
  const names = [
    'alice',
    'bob',
    'carol',
    'dave',
    'erin',
    'frank',
    'grace',
    'heidi',
    'ivan',
    'judy',
    'mallory',
    'niaj',
    'olivia',
    'peggy',
    'quentin',
    'rupert',
    'sybil',
    'trent',
    'uma',
    'victor',
    'walter',
    'xena',
    'yuki',
    'zoe',
    'arthur',
    'bella',
    'chen',
    'dmitri',
    'elena',
    'farid',
    'gwen',
    'hiro',
    'ingrid',
    'jamal',
  ];
  const users = names.map((id, index) => ({
    id,
    tenant: index % 3 === 2 ? 'studio' : index % 5 === 4 ? 'labs' : 'platform',
  }));
  const pairs: PairDef[] = [];
  const grants = new Set<string>();
  for (const [index, user] of users.entries()) {
    const idle = index % 7 === 6;
    for (const chain of chains) {
      const chance = chain.id === 'hk-direct' ? 0.85 : chain.id === 'warp' ? 0.2 : 0.32;
      if (random() > chance) continue;
      grants.add(`${user.id}/${chain.id}`);
      if (idle || random() < 0.22) continue;
      const shapes: Shape[] = ['evening', 'office', 'night'];
      pairs.push({
        user: user.id,
        col: chain.id,
        daily: Math.min(42, 0.03 * Math.pow(1 / Math.max(0.02, random()), 1.35)),
        shape: shapes[Math.floor(random() * 3)],
        days: 0.5 + random() * 0.5,
        bursty: random() < 0.2,
      });
    }
    if (random() < 0.12) {
      grants.add(`${user.id}/${FWD}`);
      pairs.push({ user: user.id, col: FWD, daily: 0.5 + random() * 6, up: 0.35, shape: 'flat' });
    }
  }
  return {
    key: 'many',
    chains,
    users,
    grants,
    pairs,
    machineGaps: [],
    pairGaps: [],
    chainSplitSince: null,
    silentSince: null,
    sites: true,
  };
}

export const SCENES = {
  typical: () => typical('typical'),
  many,
  solo: (): World => ({
    ...typical('solo'),
    chains: TYPICAL_CHAINS.slice(0, 1),
    users: TYPICAL_USERS.filter(user => ['alice', 'frank', 'heidi'].includes(user.id)),
    pairs: [
      { user: 'alice', col: 'hk-direct', daily: 18 },
      { user: 'frank', col: 'hk-direct', daily: 1.2, shape: 'office' },
      { user: 'heidi', col: 'hk-direct', daily: 0.3 },
    ],
    grants: new Set(['alice/hk-direct', 'frank/hk-direct', 'heidi/hk-direct']),
  }),
  gaps: () =>
    typical('gaps', {
      machineGaps: [
        [at(10, 18, 3), at(10, 18, 6)],
        [at(10, 7, 13), at(10, 7, 19)],
        [at(10, 14, 2), at(10, 14, 3)],
      ],
      pairGaps: [{ user: 'carol', col: 'us-gia', from: at(10, 18, 15), to: at(10, 18, 16) }],
    }),
  history: () => typical('history', { chainSplitSince: at(10, 12) }),
  quiet: () => typical('quiet', { silentSince: at(10, 15, 9) }),
  'sites-off': () => typical('sites-off', { sites: false }),
} satisfies Record<string, () => World>;
export type SceneKey = keyof typeof SCENES;
export const SCENE_OPTIONS: { value: SceneKey; label: string }[] = [
  { value: 'typical', label: '典型 5 链 12 人' },
  { value: 'many', label: '9 链 34 人' },
  { value: 'solo', label: '单链 3 人' },
  { value: 'gaps', label: '采集缺口' },
  { value: 'history', label: '按链统计上线当月' },
  { value: 'quiet', label: '近 3 天无流量' },
  { value: 'sites-off', label: '站点统计未开启' },
];

/* ── 小时序列：从当月一日和本周一里较早的一个开始，到「现在」所在的小时为止 ── */
const SERIES_START = Math.min(boundsOf('month').start, boundsOf('week').start);
const HOURS = Math.ceil((NOW - SERIES_START) / HOUR);
const CURRENT_FRACTION = ((NOW - SERIES_START) % HOUR) / HOUR || 1;

interface Lane {
  down: Float64Array;
  up: Float64Array;
  gap: Uint8Array;
}
export interface Built {
  world: World;
  lanes: Map<string, Lane>;
  machineGap: Uint8Array;
}
const newLane = (): Lane => ({
  down: new Float64Array(HOURS),
  up: new Float64Array(HOURS),
  gap: new Uint8Array(HOURS),
});

export function build(world: World): Built {
  const lanes = new Map<string, Lane>();
  const machineGap = new Uint8Array(HOURS);
  for (let h = 0; h < HOURS; h++) {
    const time = SERIES_START + h * HOUR;
    if (world.machineGaps.some(([from, to]) => time >= from && time < to)) machineGap[h] = 1;
  }
  const todayIndex = Math.floor((dayStart(NOW) - SERIES_START) / DAY);
  for (const pair of world.pairs) {
    const random = mulberry(hash(`${world.key}/${pair.user}/${pair.col}`));
    const lane = newLane();
    lanes.set(`${pair.user}/${pair.col}`, lane);
    const shape = SHAPES[pair.shape ?? 'evening'];
    const shapeSum = shape.reduce((sum, value) => sum + value, 0);
    const days = Math.ceil(HOURS / 24) + 1;
    const active = Array.from({ length: days }, () => random() < (pair.days ?? 1));
    if (pair.quietToday) active[todayIndex] = false;
    const level = Array.from({ length: days }, () => Math.exp(gauss(random) * 0.35));
    for (let h = 0; h < HOURS; h++) {
      const time = SERIES_START + h * HOUR;
      const day = Math.floor(h / 24);
      const noise = Math.exp(gauss(random) * 0.42);
      const burst = pair.bursty && random() < 0.035 ? 3 + random() * 6 : 1;
      const upJitter = 0.75 + random() * 0.5;
      if (machineGap[h] || !active[day] || (world.silentSince !== null && time >= world.silentSince)) continue;
      const date = wall(time);
      const weekend = date.getUTCDay() === 0 || date.getUTCDay() === 6 ? 1.22 : 1;
      let value = pair.daily * GiB * (shape[date.getUTCHours()] / shapeSum) * weekend * level[day] * noise * burst;
      if (h === HOURS - 1) value *= CURRENT_FRACTION;
      if (
        world.pairGaps.some(gap => gap.user === pair.user && gap.col === pair.col && time >= gap.from && time < gap.to)
      ) {
        lane.gap[h] = 1;
        value *= 0.25;
      }
      lane.up[h] = value * (pair.up ?? 0.045) * upJitter;
      lane.down[h] = value - lane.up[h];
    }
  }
  // 按链统计上线前只有「用户 × 机器」的日汇总：链上的流量并入每个用户的余额列，不能再分到链。
  if (world.chainSplitSince !== null) {
    for (const user of world.users) {
      const rest = newLane();
      let carried = false;
      for (const chain of world.chains) {
        const lane = lanes.get(`${user.id}/${chain.id}`);
        if (!lane) continue;
        for (let h = 0; h < HOURS && SERIES_START + h * HOUR < world.chainSplitSince; h++) {
          rest.down[h] += lane.down[h];
          rest.up[h] += lane.up[h];
          carried ||= lane.down[h] > 0;
          lane.down[h] = 0;
          lane.up[h] = 0;
        }
      }
      if (carried) lanes.set(`${user.id}/${UNSPLIT}`, rest);
    }
  }
  return { world, lanes, machineGap };
}

/** 最近一个有用户流量的小时（后端可由小时明细或日汇总给出）。 */
export function lastTraffic(built: Built): number | null {
  for (let h = HOURS - 1; h >= 0; h--) {
    for (const lane of built.lanes.values()) if (lane.down[h] + lane.up[h] > 0) return SERIES_START + h * HOUR;
  }
  return null;
}
export const lastTrafficText = (time: number) => `${md(time)} ${weekday(time)} ${hm(time)}–${hm(time + HOUR)}`;

/* ── 按统计范围汇总 ── */
export interface LaneSum {
  down: number[];
  up: number[];
  gap: boolean;
  downTotal: number;
  upTotal: number;
  total: number;
}
export interface Bucket {
  at: number;
  future: boolean;
  /** none：这一格机器没有上报；partial：格内有一段没有上报。 */
  observed: 'full' | 'partial' | 'none';
}
export interface Agg {
  bounds: Bounds;
  lanes: Map<string, LaneSum>;
  buckets: Bucket[];
  gapHours: number;
}

export function aggregate(built: Built, period: Period): Agg {
  const bounds = boundsOf(period);
  const offset = (bounds.start - SERIES_START) / HOUR;
  const per = bounds.bucket / HOUR;
  const lanes = new Map<string, LaneSum>();
  for (const [key, lane] of built.lanes) {
    const sum: LaneSum = {
      down: Array.from({ length: bounds.count }, () => 0),
      up: Array.from({ length: bounds.count }, () => 0),
      gap: false,
      downTotal: 0,
      upTotal: 0,
      total: 0,
    };
    for (let index = 0; index < bounds.count; index++) {
      for (let step = 0; step < per; step++) {
        const h = offset + index * per + step;
        if (h < 0 || h >= HOURS) continue;
        sum.down[index] += lane.down[h];
        sum.up[index] += lane.up[h];
        sum.gap ||= lane.gap[h] === 1;
      }
      sum.downTotal += sum.down[index];
      sum.upTotal += sum.up[index];
    }
    sum.total = sum.downTotal + sum.upTotal;
    lanes.set(key, sum);
  }
  let gapHours = 0;
  const buckets = Array.from({ length: bounds.count }, (_, index): Bucket => {
    const start = bounds.start + index * bounds.bucket;
    let seen = 0;
    let missing = 0;
    for (let step = 0; step < per; step++) {
      const h = offset + index * per + step;
      if (h < 0 || h >= HOURS) continue;
      seen++;
      missing += built.machineGap[h];
    }
    gapHours += missing;
    return {
      at: start,
      future: start >= NOW,
      observed: seen === 0 || missing === seen ? 'none' : missing === 0 ? 'full' : 'partial',
    };
  });
  return { bounds, lanes, buckets, gapHours };
}

/* ── 流入：用户 × 接入链 ── */
export interface ChainView {
  id: string;
  kind: 'chain' | 'fwd' | 'unsplit';
  name: string;
  country: string | null;
  via: string;
  /** 数据色；null 表示未单独着色（本月前五以外的链）。 */
  token: string | null;
  color: string;
}
/** 链的数据色按本月流量固定前五（同站点流量卡的规则），换范围不换色；其余链、转发与余额用中性色。 */
const CHAIN_TOKENS = ['--observe-1', '--observe-6', '--observe-4', '--observe-5', '--observe-7'];
export function chainViews(world: World, agg: Agg, month: Agg): ChainView[] {
  const monthTotal = (col: string) =>
    [...month.lanes].reduce((sum, [key, lane]) => (key.endsWith(`/${col}`) ? sum + lane.total : sum), 0);
  const ranked = [...world.chains].sort((a, b) => monthTotal(b.id) - monthTotal(a.id));
  const views: ChainView[] = ranked.map((chain, index) => {
    const token = index < CHAIN_TOKENS.length && monthTotal(chain.id) > 0 ? CHAIN_TOKENS[index] : null;
    return {
      id: chain.id,
      kind: 'chain',
      name: chain.name,
      country: chain.country,
      via: chain.via,
      token,
      color: token ? `var(${token})` : 'color-mix(in srgb, var(--observe-10) 75%, transparent)',
    };
  });
  if ([...world.grants].some(grant => grant.endsWith(`/${FWD}`)))
    views.push({
      id: FWD,
      kind: 'fwd',
      name: '端口转发',
      country: null,
      via: '用户自建规则',
      token: null,
      color: 'color-mix(in srgb, var(--ink) 40%, transparent)',
    });
  if ([...agg.lanes].some(([key, lane]) => key.endsWith(`/${UNSPLIT}`) && lane.total > 0))
    views.push({
      id: UNSPLIT,
      kind: 'unsplit',
      name: '未按接入链记录',
      country: null,
      via: '统计上线前的余额',
      token: null,
      color: 'color-mix(in srgb, var(--ink) 20%, transparent)',
    });
  return views;
}

export interface InflowCell {
  down: number;
  up: number;
  total: number;
  gap: boolean;
  granted: boolean;
}
export interface InflowRow {
  id: string;
  tenant: string;
  down: number;
  up: number;
  total: number;
  cells: Map<string, InflowCell>;
}
export interface Inflow {
  rows: InflowRow[];
  idle: InflowRow[];
  grand: number;
  down: number;
  up: number;
  columnTotal: (col: string) => number;
  activeChains: number;
}
export function inflowOf(world: World, agg: Agg, views: ChainView[]): Inflow {
  const all = world.users.map(user => {
    const cells = new Map<string, InflowCell>();
    for (const view of views) {
      const lane = agg.lanes.get(`${user.id}/${view.id}`);
      cells.set(view.id, {
        down: lane?.downTotal ?? 0,
        up: lane?.upTotal ?? 0,
        total: lane?.total ?? 0,
        gap: (lane?.gap ?? false) && (lane?.total ?? 0) > 0,
        granted: view.kind === 'unsplit' ? (lane?.total ?? 0) > 0 : world.grants.has(`${user.id}/${view.id}`),
      });
    }
    const values = [...cells.values()];
    return {
      id: user.id,
      tenant: user.tenant,
      down: values.reduce((sum, cell) => sum + cell.down, 0),
      up: values.reduce((sum, cell) => sum + cell.up, 0),
      total: values.reduce((sum, cell) => sum + cell.total, 0),
      cells,
    };
  });
  const rows = all.filter(row => row.total > 0).sort((a, b) => b.total - a.total || a.id.localeCompare(b.id));
  const idle = all.filter(row => row.total === 0 && [...row.cells.values()].some(cell => cell.granted));
  const columnTotal = (col: string) => all.reduce((sum, row) => sum + (row.cells.get(col)?.total ?? 0), 0);
  return {
    rows,
    idle,
    grand: rows.reduce((sum, row) => sum + row.total, 0),
    down: rows.reduce((sum, row) => sum + row.down, 0),
    up: rows.reduce((sum, row) => sum + row.up, 0),
    columnTotal,
    activeChains: views.filter(view => view.kind === 'chain' && columnTotal(view.id) > 0).length,
  };
}
/** 每格（小时 / 天）的分布；user、col 为 null 表示不限。 */
export function seriesOf(agg: Agg, user: string | null, col: string | null) {
  const values = Array.from({ length: agg.bounds.count }, () => 0);
  for (const [key, lane] of agg.lanes) {
    if (user !== null && !key.startsWith(`${user}/`)) continue;
    if (col !== null && !key.endsWith(`/${col}`)) continue;
    for (let index = 0; index < values.length; index++) values[index] += lane.down[index] + lane.up[index];
  }
  return values;
}

/* ── 实时：近 1 小时的 30 秒窗口与每秒速率 ── */
export const SLOT_SECS = 30;
const SLOTS = HOUR / SLOT_SECS;
const LAST_SLOT_END = Math.floor(NOW / SLOT_SECS) * SLOT_SECS;
export interface LiveHistory {
  /** 每个窗口的结束时刻。 */
  slots: number[];
  totalDown: number[];
  totalUp: number[];
  users: Map<string, { down: number[]; up: number[]; hourBytes: number; chainShare: Map<string, number> }>;
}
/** 近 1 小时：每位用户的 30 秒窗口字节数（这一路数据没有接入链维度）；空闲窗口是 0，不是缺口。 */
export function liveHistory(built: Built): LiveHistory {
  const slots = Array.from({ length: SLOTS }, (_, index) => LAST_SLOT_END - (SLOTS - 1 - index) * SLOT_SECS);
  const users: LiveHistory['users'] = new Map();
  for (const user of built.world.users) {
    const random = mulberry(hash(`live/${built.world.key}/${user.id}`));
    const down = slots.map(() => 0);
    const up = slots.map(() => 0);
    const shares = new Map<string, number>();
    let active = random() < 0.7;
    for (const [index, end] of slots.entries()) {
      const hour = Math.floor((end - 1 - SERIES_START) / HOUR);
      if (random() < (active ? 0.06 : 0.1)) active = !active;
      let slotDown = 0;
      let slotUp = 0;
      for (const [key, lane] of built.lanes) {
        if (!key.startsWith(`${user.id}/`) || key.endsWith(`/${UNSPLIT}`) || hour < 0 || hour >= HOURS) continue;
        const scale = hour === HOURS - 1 ? CURRENT_FRACTION : 1;
        const base = (lane.down[hour] + lane.up[hour]) / (scale * SLOTS);
        const value = active ? (base / 0.62) * Math.exp(gauss(random) * 0.5) : 0;
        const upShare = lane.down[hour] + lane.up[hour] > 0 ? lane.up[hour] / (lane.down[hour] + lane.up[hour]) : 0;
        slotDown += value * (1 - upShare);
        slotUp += value * upShare;
        if (index >= SLOTS - 6) {
          const col = key.slice(user.id.length + 1);
          shares.set(col, (shares.get(col) ?? 0) + value);
        }
      }
      down[index] = slotDown;
      up[index] = slotUp;
    }
    const hourBytes = down.reduce((sum, value) => sum + value, 0) + up.reduce((sum, value) => sum + value, 0);
    if (hourBytes > 0) users.set(user.id, { down, up, hourBytes, chainShare: shares });
  }
  const totalDown = slots.map((_, index) => [...users.values()].reduce((sum, user) => sum + user.down[index], 0));
  const totalUp = slots.map((_, index) => [...users.values()].reduce((sum, user) => sum + user.up[index], 0));
  return { slots, totalDown, totalUp, users };
}

export interface LiveRate {
  id: string;
  tenant: string;
  down: number;
  up: number;
  /** 每条接入链（或端口转发）当前的速率，字节/秒。 */
  chains: { col: string; down: number; up: number }[];
  /** 最后一次有流量的时刻；正在传输时为 null。 */
  idleSince: number | null;
}
/** 每秒速率：最近几个窗口的走势加上逐秒抖动；最近 3 个窗口都没有流量的用户算空闲。 */
export function liveRates(built: Built, history: LiveHistory, second: number): LiveRate[] {
  const rates: LiveRate[] = [];
  for (const user of built.world.users) {
    const past = history.users.get(user.id);
    if (!past) continue;
    const recent = past.down.slice(-3).reduce((sum, value) => sum + value, 0);
    let lastActive = -1;
    past.down.forEach((value, index) => {
      if (value > 0) lastActive = index;
    });
    const random = mulberry(hash(`${built.world.key}/${user.id}/${second}`));
    const level = recent > 0 ? (past.down.at(-1)! + past.up.at(-1)! || recent / 3) / SLOT_SECS : 0;
    const jitter = Math.exp(gauss(random) * 0.28);
    const shareTotal = [...past.chainShare.values()].reduce((sum, value) => sum + value, 0);
    const upRatio = past.up.at(-1)! / Math.max(1, past.down.at(-1)! + past.up.at(-1)!);
    const chains = [...past.chainShare]
      .filter(([, share]) => share > 0 && level > 0)
      .map(([col, share]) => {
        const value = level * jitter * (share / shareTotal);
        return { col, down: value * (1 - upRatio), up: value * upRatio };
      });
    rates.push({
      id: user.id,
      tenant: user.tenant,
      down: chains.reduce((sum, chain) => sum + chain.down, 0),
      up: chains.reduce((sum, chain) => sum + chain.up, 0),
      chains,
      idleSince: level > 0 ? null : lastActive >= 0 ? history.slots[lastActive] : null,
    });
  }
  return rates;
}

/* ── 流出：本机认证代理流量按目的站点归类 ── */
const SITES: [string, string, number][] = [
  ['youtube', 'YouTube', 30],
  ['netflix', 'Netflix', 12],
  ['openai', 'OpenAI / ChatGPT', 3],
  ['twitter', 'X / Twitter', 3.5],
  ['telegram', 'Telegram', 4],
  ['instagram', 'Instagram', 4],
  ['facebook', 'Facebook', 2],
  ['tiktok', 'TikTok', 4.5],
  ['github', 'GitHub', 1.6],
  ['anthropic', 'Claude', 1.2],
  ['steam', 'Steam', 2.5],
  ['discord', 'Discord', 1],
  ['spotify', 'Spotify', 1.2],
  ['google', 'Google', 2.4],
  ['disney', 'Disney+', 1.5],
  ['microsoft', 'Microsoft', 1.4],
  ['apple', 'Apple', 1.1],
  ['bytedance', '字节跳动', 0.6],
  ['tencent', '腾讯', 0.5],
  ['baidu', '百度', 0.2],
];
const NON_SITES: [string, string, number][] = [
  ['other', '其他站点', 9],
  ['sniffing-failed', '嗅探失败', 6],
  ['unidentified', '未识别', 0.4],
];
export const NON_SITE_IDS = NON_SITES.map(([id]) => id);
const SITE_BOOST: Record<string, Record<string, number>> = {
  alice: { youtube: 1.6, netflix: 1.5, openai: 3 },
  bob: { steam: 9, youtube: 1.4, discord: 3 },
  carol: { github: 6, microsoft: 6, google: 3, 'sniffing-failed': 2.4 },
  grace: { instagram: 3, telegram: 2 },
  mallory: { netflix: 3.5, disney: 4 },
  peggy: { tiktok: 4, youtube: 1.3 },
  frank: { telegram: 3 },
  judy: { anthropic: 5, openai: 2 },
};
function weightsOf(user: string) {
  const random = mulberry(hash(`sites/${user}`));
  const entries = [...SITES, ...NON_SITES].map(
    ([id, , base]) => [id, base * (SITE_BOOST[user]?.[id] ?? 1) * Math.exp(gauss(random) * 0.5)] as const,
  );
  const sum = entries.reduce((total, [, weight]) => total + weight, 0);
  return new Map(entries.map(([id, weight]) => [id, weight / sum]));
}
export interface SiteRow {
  id: string;
  name: string;
  total: number;
  down: number;
  up: number;
  values: number[];
  users: { id: string; total: number }[];
}
export function siteRowsOf(built: Built, agg: Agg): SiteRow[] {
  const perUser = built.world.users.map(user => {
    const values = Array.from({ length: agg.bounds.count }, () => 0);
    let down = 0;
    let up = 0;
    for (const [key, lane] of agg.lanes) {
      if (!key.startsWith(`${user.id}/`) || key.endsWith(`/${FWD}`)) continue;
      for (let index = 0; index < values.length; index++) values[index] += lane.down[index] + lane.up[index];
      down += lane.downTotal;
      up += lane.upTotal;
    }
    return { id: user.id, values, down, up, weights: weightsOf(user.id) };
  });
  const rows = (built.world.sites ? [...SITES, ...NON_SITES] : [])
    .map(([id, name]) => {
      const values = Array.from({ length: agg.bounds.count }, () => 0);
      const users: { id: string; total: number }[] = [];
      let down = 0;
      let up = 0;
      for (const user of perUser) {
        const weight = user.weights.get(id) ?? 0;
        let total = 0;
        for (let index = 0; index < values.length; index++) {
          values[index] += user.values[index] * weight;
          total += user.values[index] * weight;
        }
        down += user.down * weight;
        up += user.up * weight;
        if (total > 0) users.push({ id: user.id, total });
      }
      users.sort((a, b) => b.total - a.total);
      return { id, name, values, down, up, total: down + up, users };
    })
    .filter(row => row.total > 0);
  const forwarding: SiteRow = {
    id: 'forwarding',
    name: '端口转发',
    down: 0,
    up: 0,
    total: 0,
    values: Array.from({ length: agg.bounds.count }, () => 0),
    users: [],
  };
  for (const user of built.world.users) {
    const lane = agg.lanes.get(`${user.id}/${FWD}`);
    if (!lane || lane.total === 0) continue;
    forwarding.down += lane.downTotal;
    forwarding.up += lane.upTotal;
    forwarding.total += lane.total;
    forwarding.users.push({ id: user.id, total: lane.total });
    for (let index = 0; index < forwarding.values.length; index++) {
      forwarding.values[index] += lane.down[index] + lane.up[index];
    }
  }
  forwarding.users.sort((a, b) => b.total - a.total);
  if (forwarding.total > 0) rows.push(forwarding);
  return rows.sort((a, b) => b.total - a.total);
}
