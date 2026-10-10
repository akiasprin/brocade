// 地址栏保存导航状态。前进后退、刷新、分享链接都依赖该状态。
//
// 此前控制台未调用过 history API：页面切换记录在 localStorage，下钻状态记录在
// wm.data，两者都不体现在地址中。因此浏览器的后退键在本应用中等同于离开该站点——
// 从机器详情返回机器列表只能点击面包屑，一次误操作即会退出当前会话。
//
// 使用 hash 而非 path 的原因：前端构建产物嵌入在控制面二进制中，按路径查表命中才返回
// （console 的 assets.rs），没有 SPA fallback：`/nodes/hk-01` 这类深链接会返回 404。
// 使用 path 需要同时修改后端，而该 fallback 一旦调整会导致前端刷新失效——前端不应
// 单方面依赖后端的路由结构。hash 不经过服务端。
//
// 进入地址的内容：仅当前位置，即所在页面和页面内下钻到的对象。不进入地址的内容：
// - 诊断气泡、产物栏展开状态、明暗主题——属于偏好和临时状态，state.ts 中已说明
//   刷新后应关闭；进入地址后后退键会改变气泡的展开状态。
// - 纳管向导的步骤。填表页面是 `#/nodes/provision`；提交后机器已入库，
//   后续页面是为该机器安装 agent，标识是 node_id 而非一次性的创建响应——
//   即 `#/nodes/install/hk-01`，恢复后停留在安装命令页面。步骤号本身不进入地址：
//   它表示流程进度而非位置。
// - 预览的幂等键。它是单次点击的标识而非位置；写入地址会导致刷新后复用旧键。
//   DeployPane 遇到没有携带键的 plan 时会自行生成。

import { wm } from '../wm/store';
import { cancelVisualTransition } from '../ui/motion';
import { confirmDiscardChanges } from '../ui/navigation-guard';
import { canonicalLoadRangeQuery } from '../ui/observe-range';
import { DEFAULT_NAV, forge, isNavKey, type NavKey } from './state';

// 下钻状态在各页面中是私有的 `type Drill`，此处只将其视为一组字段。
// 两侧的一致性由下面的表保证：字段名写错时 TS 无法检查，但页面会立即变为空白。
type Drill = Record<string, unknown>;

export interface Loc {
  nav: NavKey;
  drill?: Drill;
}

interface Field {
  name: string;
  // 反序列化时需要还原类型：地址栏中全部是字符串，而 deploy 的 id 是数字，
  // `deployments.find(d => d.id === id)` 传入 '12' 时不会匹配到任何记录。
  num?: boolean;
  optional?: boolean;
  pattern?: RegExp;
}

interface DrillSpec {
  /* 地址中的该段，同时也是 drill.p 的取值 */
  seg: string;
  fields?: Field[];
  /* 页面内稳定选择可以放在 hash 的查询段中，不占用资源详情的路径段。 */
  queryFields?: Field[];
  /* 查询段字段之间有组合约束时（固定区间的起止必须成对且跨度合法），在解析和写出两个方向
     规范化：非法组合去掉而不是让整个地址失效，手改出的地址随后被改写为规范形式。 */
  canonical?: (drill: Drill) => Drill;
  /* 恢复时补全的、不进入地址的字段（向导的起始步骤） */
  rest?: Drill;
}

// 哪些页面的下钻状态写入地址。未列出的页面（settings / topo 等）只有一个位置，
// 一个 nav 即可表示；未列出的 `p`（provision 的步骤）保留在 wm.data 中不进入地址。
const DRILL: Partial<Record<NavKey, DrillSpec[]>> = {
  nodes: [
    // 观测时间范围进入地址：每次切换是一条浏览器历史，前进后退、刷新、分享链接都保留所选时段。
    // 取值与组合规则见 ui/observe-range.ts；页签不进入地址（见 nodes.tsx 的 NodeDetail）。
    {
      seg: 'node',
      fields: [{ name: 'id' }],
      queryFields: [
        { name: 'range', optional: true },
        { name: 'from', optional: true, num: true },
        { name: 'to', optional: true, num: true },
      ],
      canonical: canonicalLoadRangeQuery,
    },
    { seg: 'chain', fields: [{ name: 'id' }] },
    { seg: 'provision', rest: { step: 1 } },
    // 机器在提交表单时即已入库，因此安装页的标识是 node_id，切换、刷新、后退都可恢复。
    // 创建响应中的明文 token 不进入地址；恢复安装页时可以重新签发一枚。
    { seg: 'install', fields: [{ name: 'node' }], rest: { step: 4 } },
  ],
  chains: [
    { seg: 'chain', fields: [{ name: 'app' }, { name: 'chain' }] },
    { seg: 'new', fields: [{ name: 'app' }] },
  ],
  tunnels: [
    // 外部出口 id 在数据库中全局唯一；租户只用于服务端授权边界，不进入浏览器地址。
    { seg: 'custom', fields: [{ name: 'id', pattern: /^custom-[0-9a-f]{4}-[0-9a-f]{4}$/ }] },
    { seg: 'warp', fields: [{ name: 'id', pattern: /^warp-[0-9a-f]{4}-[0-9a-f]{4}$/ }] },
    {
      seg: 'vpngate',
      fields: [{ name: 'id', optional: true, pattern: /^vpngate-[0-9a-f]{4}-[0-9a-f]{4}$/ }],
      queryFields: [{ name: 'country', optional: true, pattern: /^[A-Z]{2}$/ }],
    },
  ],
  /* 单租户阶段用户 id 足以恢复详情，内部归属不进入可见地址。 */
  users: [{ seg: 'new' }, { seg: 'user', fields: [{ name: 'id' }] }],
  deploy: [{ seg: 'plan' }, { seg: 'detail', fields: [{ name: 'id', num: true }] }],
};

const specFor = (nav: NavKey, p: unknown): DrillSpec | undefined =>
  typeof p === 'string' ? DRILL[nav]?.find(s => s.seg === p) : undefined;

// ══ 位置与地址的相互转换 ══
//
// 这两个是纯函数，也是该模块最容易出错的部分（少一段、多一层、数字转为字符串）。
// 导出它们是为了能脱离浏览器直接使用样例数据测试——见 scripts/route-check.mjs。

export function serialize(loc: Loc): string {
  const parts: string[] = [loc.nav];
  const spec = specFor(loc.nav, loc.drill?.p);
  const drill = spec?.canonical && loc.drill ? spec.canonical(loc.drill) : loc.drill;
  const query = new URLSearchParams();
  if (spec && drill) {
    parts.push(spec.seg);
    for (const f of spec.fields ?? []) {
      const v = drill[f.name];
      // 字段缺失时回退到该页面的根路径：少一层优于生成 `#/deploy/detail/undefined`
      // ——该地址解析后会得到一个 id 为空的详情页。
      if (v == null) {
        if (f.optional) continue;
        return `#/${loc.nav}`;
      }
      const raw = String(v);
      if (f.pattern && !f.pattern.test(raw)) return `#/${loc.nav}`;
      parts.push(encodeURIComponent(raw));
    }
    for (const f of spec.queryFields ?? []) {
      const value = drill[f.name];
      if (value == null) {
        if (f.optional) continue;
        return `#/${loc.nav}`;
      }
      const raw = String(value);
      if (f.pattern && !f.pattern.test(raw)) return `#/${loc.nav}`;
      query.set(f.name, raw);
    }
  }
  const suffix = query.size ? `?${query.toString()}` : '';
  return `#/${parts.join('/')}${suffix}`;
}

export function parse(hash: string): Loc | null {
  const body = hash.replace(/^#\/?/, '');
  const queryStart = body.indexOf('?');
  const path = queryStart < 0 ? body : body.slice(0, queryStart);
  const query = new URLSearchParams(queryStart < 0 ? '' : body.slice(queryStart + 1));
  const parts = path.split('/').filter(Boolean).map(decodeURIComponent);
  const [nav, seg, ...rest] = parts;
  if (!nav || !isNavKey(nav)) return null;

  const spec = specFor(nav, seg);
  if (!spec) return { nav };

  const drill: Drill = { p: spec.seg, ...spec.rest };
  const fields = spec.fields ?? [];
  const values = rest;
  // 路径段数量必须与当前路由表完全一致。手改出的缺段或多段地址都回到页面根部，
  // 不把半个标识传给详情页。
  const requiredFields = fields.filter(field => !field.optional).length;
  if (values.length < requiredFields || values.length > fields.length) return { nav };
  let valid = true;
  fields.forEach((f, i) => {
    const raw = values[i];
    if (raw == null) return;
    if (f.pattern && !f.pattern.test(raw)) {
      valid = false;
      return;
    }
    if (!f.num) {
      drill[f.name] = raw;
      return;
    }
    const n = Number(raw);
    drill[f.name] = Number.isFinite(n) ? n : raw;
  });
  for (const f of spec.queryFields ?? []) {
    const raw = query.get(f.name);
    if (raw == null) continue;
    if (f.pattern && !f.pattern.test(raw)) {
      valid = false;
      continue;
    }
    if (!f.num) {
      drill[f.name] = raw;
      continue;
    }
    const n = Number(raw);
    drill[f.name] = Number.isFinite(n) ? n : raw;
  }
  if (!valid) return { nav };
  return { nav, drill: spec.canonical ? spec.canonical(drill) : drill };
}

/* ══ 位置与应用状态的相互转换 ══ */

function current(): Loc {
  const nav = forge.snapshot().nav;
  const win = wm.snapshot().wins.find(w => w.key === `tab:${nav}`);
  return { nav, drill: win?.data.drill as Drill | undefined };
}

// 恢复期间不写回地址：apply 会连续修改 forge 和 wm，每次修改都会触发 sync，
// 而这些变化的来源即是地址栏——再次 push 会重复写入自身的历史记录。
let applying = false;

const ROUTE_HISTORY_KEY = 'brocadeRoute';

interface RouteHistoryState {
  index: number;
  scrollTop: number;
  fromHash?: string;
}

type RoutePosition = Pick<RouteHistoryState, 'scrollTop'>;

const POSITION_SAVE_DELAY = 250;
const POSITION_SAVE_INTERVAL = 1_000;
const POSITION_CACHE_LIMIT = 100;
const positionCache = new Map<number, RoutePosition>();
let historyIndex = 0;
let historyHash = '';
let approvedTraversal = false;
let revertingTraversal = false;
let positionSaveTimer = 0;
let lastPositionSave = -Infinity;
let lastHistoryWarning = -Infinity;
let positionRestoreCleanup: (() => void) | null = null;

const stateRecord = (value: unknown): Record<string, unknown> =>
  value !== null && typeof value === 'object' ? (value as Record<string, unknown>) : {};

const routeHistoryState = (value: unknown = window.history.state): RouteHistoryState | null => {
  const raw = stateRecord(value)[ROUTE_HISTORY_KEY];
  if (raw === null || typeof raw !== 'object') return null;
  const state = raw as Partial<RouteHistoryState>;
  if (
    typeof state.index !== 'number' ||
    !Number.isSafeInteger(state.index) ||
    typeof state.scrollTop !== 'number' ||
    !Number.isFinite(state.scrollTop)
  )
    return null;
  return {
    index: state.index,
    scrollTop: Math.max(0, state.scrollTop),
    fromHash: typeof state.fromHash === 'string' ? state.fromHash : undefined,
  };
};

/* 当前历史项变化的订阅。页面据此读出「这一条是从哪个地址进入的」（previousRouteHash）：
   push、前进后退、启动对齐之后通知；只改滚动位置的 replaceState 不改变来源，不通知。 */
const historyListeners = new Set<() => void>();
const notifyHistory = () => {
  for (const listener of historyListeners) listener();
};

export function subscribeRouteHistory(listener: () => void): () => void {
  historyListeners.add(listener);
  return () => {
    historyListeners.delete(listener);
  };
}

/** 当前历史项的来源地址：push 时记录，手改地址栏时由 restore 补记；没有记录时为 null。 */
export function previousRouteHash(): string | null {
  return routeHistoryState()?.fromHash ?? null;
}

const withRouteHistoryState = (state: RouteHistoryState, source: unknown = window.history.state) => ({
  ...stateRecord(source),
  [ROUTE_HISTORY_KEY]: state,
});

/** A rejected or silently ignored write must never advance the application's history index. */
const writeRouteHistory = (method: 'pushState' | 'replaceState', state: RouteHistoryState, hash: string): boolean => {
  let failure = 'HistoryWriteIgnored';
  try {
    window.history[method](withRouteHistoryState(state), '', hash);
    const written = routeHistoryState();
    if (
      window.location.hash === hash &&
      written?.index === state.index &&
      written.scrollTop === state.scrollTop &&
      written.fromHash === state.fromHash
    )
      return true;
  } catch (error) {
    // Exception messages may contain URLs. Keep diagnostics bounded and free of route data.
    failure = error instanceof Error ? error.name : 'HistoryWriteFailed';
  }
  if (performance.now() - lastHistoryWarning >= 10_000) {
    lastHistoryWarning = performance.now();
    console.warn(`浏览器历史写入失败（${method} / ${failure}）；保留当前导航和内存位置。`);
  }
  return false;
};

const cachePosition = (index: number, position: RoutePosition) => {
  positionCache.delete(index);
  positionCache.set(index, position);
  if (positionCache.size > POSITION_CACHE_LIMIT) {
    const oldest = positionCache.keys().next().value;
    if (oldest !== undefined) positionCache.delete(oldest);
  }
};

const cancelPositionSave = () => {
  if (positionSaveTimer) window.clearTimeout(positionSaveTimer);
  positionSaveTimer = 0;
};

const workspaceScroller = (): Element | null => {
  const desk = document.querySelector<HTMLElement>('.fg-desk');
  if (!desk) return null;
  // CSS owns the layout mode: touch pages scroll the document, while desktop and topology
  // retain the workspace viewport. Read the actual layout rather than duplicating its breakpoint.
  return getComputedStyle(desk).overflowY === 'visible' ? document.scrollingElement : desk;
};

const rememberCurrentPosition = () => {
  // Do not replace a pending restoration with the outgoing DOM or a partially loaded list.
  if (typeof document === 'undefined' || applying || positionRestoreCleanup) return false;
  const previous = positionCache.get(historyIndex) ?? { scrollTop: 0 };
  const scroller = workspaceScroller();
  cachePosition(historyIndex, {
    // Safari's elastic overscroll can briefly report a negative document offset.
    scrollTop: Math.max(0, scroller?.scrollTop ?? previous.scrollTop),
  });
  return true;
};

const persistCurrentPosition = () => {
  cancelPositionSave();
  // popstate already exposes the destination history entry while the old page is still mounted.
  if (window.location.hash !== historyHash || revertingTraversal) return;
  const previous = routeHistoryState();
  const position = positionCache.get(historyIndex);
  if (!position || previous?.index !== historyIndex) return;
  if (previous.scrollTop === position.scrollTop) return;
  lastPositionSave = performance.now();
  writeRouteHistory('replaceState', { ...previous, ...position }, historyHash);
};

const schedulePositionSave = () => {
  cancelPositionSave();
  const index = historyIndex;
  const hash = historyHash;
  positionSaveTimer = window.setTimeout(
    () => {
      positionSaveTimer = 0;
      if (index === historyIndex && hash === historyHash) persistCurrentPosition();
    },
    Math.max(POSITION_SAVE_DELAY, lastPositionSave + POSITION_SAVE_INTERVAL - performance.now()),
  );
};

/** Restore after React has replaced the route body. Resize/DOM observers cover async list data. */
const restoreWorkspacePosition = (state: Pick<RouteHistoryState, 'scrollTop'>) => {
  if (typeof document === 'undefined') return;
  positionRestoreCleanup?.();

  let stopped = false;
  let resizeObserver: ResizeObserver | null = null;
  let mutationObserver: MutationObserver | null = null;
  let timeout = 0;
  const stop = () => {
    if (stopped) return;
    stopped = true;
    resizeObserver?.disconnect();
    mutationObserver?.disconnect();
    if (timeout) window.clearTimeout(timeout);
    positionRestoreCleanup = null;
  };
  positionRestoreCleanup = stop;

  const applyPosition = () => {
    if (stopped) return;
    const scroller = workspaceScroller();
    const surface = document.querySelector<HTMLElement>('.fg-view, .fg-topo');
    if (!surface) return;

    const maxScroll = scroller ? Math.max(0, scroller.scrollHeight - scroller.clientHeight) : 0;
    if (scroller) scroller.scrollTop = Math.min(state.scrollTop, maxScroll);
    surface.focus({ preventScroll: true });

    const scrollReady = !scroller || state.scrollTop <= maxScroll + 1;
    if (scrollReady) stop();
  };

  // One pre-paint frame is enough for useSyncExternalStore to commit the route body. Waiting for a
  // second frame exposes the new page at the previous page's scrollTop for one paint; on a narrow
  // screen that moves almost the whole viewport and looks like a full-screen flash when going back.
  timeout = window.setTimeout(() => {
    applyPosition();
    stop();
  }, 4_000);
  window.requestAnimationFrame(() => {
    if (stopped) return;
    applyPosition();
    if (stopped) return;
    const surface = document.querySelector<HTMLElement>('.fg-view, .fg-topo') ?? workspaceScroller() ?? document.body;
    if (!surface) return;
    if (typeof ResizeObserver !== 'undefined') {
      resizeObserver = new ResizeObserver(applyPosition);
      resizeObserver.observe(surface);
    }
    if (typeof MutationObserver !== 'undefined') {
      mutationObserver = new MutationObserver(applyPosition);
      mutationObserver.observe(surface, { childList: true, subtree: true });
    }
  });
};

const currentWorkspacePosition = (): RoutePosition => ({
  scrollTop: Math.max(0, workspaceScroller()?.scrollTop ?? 0),
});

const pushLocation = (hash: string, position: RoutePosition = { scrollTop: 0 }): boolean => {
  const fromHash = window.location.hash;
  rememberCurrentPosition();
  persistCurrentPosition();
  const nextIndex = historyIndex + 1;
  if (!writeRouteHistory('pushState', { index: nextIndex, ...position, fromHash }, hash)) return false;
  // A push after back replaces the forward branch, including cached entries whose indices recur.
  for (const index of positionCache.keys()) {
    if (index >= nextIndex) positionCache.delete(index);
  }
  historyIndex = nextIndex;
  historyHash = hash;
  cachePosition(historyIndex, position);
  notifyHistory();
  return true;
};

// 页面名称由外壳管理（NAV / MORE 两张表），在 startRouting 时传入；
// 在此处 import 会形成循环依赖。只在打开窗口时使用一次，默认值为 nav 本身。
let labelOf: (nav: NavKey) => string = nav => nav;
let normalizeLocation: (location: Loc) => Loc = location => location;

function apply(loc: Loc) {
  applying = true;
  try {
    forge.setNav(loc.nav);
    // 只有支持下钻的页面需要在此写入状态。其他页面（设置等）由外壳的 Work
    // 自行打开窗口——topo 不使用 wm，在此为它创建 `tab:` 窗口时，该窗口不会被
    // 渲染（WinLayer 过滤掉 tab: 前缀），但会在布局中长期占用一条记录。
    if (!DRILL[loc.nav]) return;
    // 页面尚未打开时先打开：后退到 `#/nodes/node/hk-01` 时该窗口可能不存在
    // （刚登录、切换过用户、旧布局失效），setData 找不到窗口时不执行任何操作且不报错。
    // wm.open 对同一个 key 是幂等的。
    const win = wm.open(`tab:${loc.nav}`, labelOf(loc.nav));
    // 同时清空 crumb，使其只有一个数据来源：面板的 useCrumb 会按新的 drill 重新写入。
    // 保留旧值时，从 hk-01 返回列表的那一帧面包屑仍显示「机器 / hk-01」——
    // 显示一个已不存在的位置比少一段更容易造成误判。
    wm.setData(win.id, { ...win.data, drill: loc.drill ?? { p: 'list' }, crumb: [] });
  } finally {
    applying = false;
  }
}

/** 主动导航到某个位置：点击顶栏、点击底部 tab bar、跨页面跳转都调用该函数。

    不传 drill 表示跳转到该页面的首页。页面内的下钻状态保存在 `tab:<nav>` 窗口的 data
    中，`forge.setNav` 不修改它：从机器详情切换到用户页再切回时，显示的仍是上次的机器。
    而顶栏的该项显示为「机器」，点击它需要得到机器列表——返回上次位置由后退键实现，
    不属于导航功能。

    传入 drill 表示跳转并停留在该层级，用于跨页面跳转（纳管完成后跳转到发布的计划预览）。
    该跳转此前通过 `wm.setData` 直接向其他页面的窗口写入 drill，不切换 nav 也不进入地址：
    在当前外壳下不产生任何效果，需要再次点击「发布」才能看到。

    它与后退键、地址栏直达使用同一个 `apply`——三条路径语义一致，增加下钻层级只需
    修改 DRILL 表。新导航始终创建当前访问位置并回到顶部；恢复旧滚动只属于显式返回。 */
const moveTo = (
  nav: NavKey,
  drill: Loc['drill'] | undefined,
  restorePrevious: boolean,
  preservePosition = false,
): boolean => {
  const loc = normalizeLocation({ nav, drill });
  const next = serialize(loc);
  if (!confirmDiscardChanges()) return false;
  cancelVisualTransition();
  const nextPosition = preservePosition ? currentWorkspacePosition() : { scrollTop: 0 };

  // 只有带有“返回”语义的控件才能复用上一条历史。顶栏恰好指向上一页时仍是一次新导航，
  // 否则会把上一页的中段滚动位置恢复出来，看起来像新页面从半截开始。
  if (restorePrevious && routeHistoryState()?.fromHash === next) {
    rememberCurrentPosition();
    persistCurrentPosition();
    approvedTraversal = true;
    window.history.back();
    return true;
  }

  // React 的外部 store 更新可能在事件结束前提交。先记录旧页面，避免 apply 之后焦点元素
  // 已被卸载、详情的内容高度又覆盖列表原有 scrollTop。
  if (next !== window.location.hash) {
    if (!pushLocation(next, nextPosition)) return false;
  } else {
    cancelPositionSave();
    cachePosition(historyIndex, nextPosition);
  }
  // 先确认历史写入成功，再提交外部 store；被限流时页面、URL 和索引一起留在原处。
  // apply 期间抑制 sync，避免 forge / wm 的中间状态各占一条历史记录。
  apply(loc);
  restoreWorkspacePosition(nextPosition);
  return true;
};

export function navigate(nav: NavKey, drill?: Loc['drill']): boolean {
  return moveTo(nav, drill, false);
}

/** Change an object selected inside a persistent master-detail page without jumping its roster. */
export function navigateInPlace(nav: NavKey, drill?: Loc['drill']): boolean {
  return moveTo(nav, drill, false, true);
}

/** 返回父级时优先复用紧邻的历史项，以恢复离开前的滚动位置。 */
export function returnTo(nav: NavKey, drill?: Loc['drill']): boolean {
  return moveTo(nav, drill, true);
}

/* ══ 启动 ══ */

let started = false;

/** 登录之后、外壳挂载时调。label 由外壳给：面的名字归它管（NAV / MORE），
    这里去 import 会绕回一个环。

    每次登录都要调，不是只调第一次。退出再进来（尤其是换个人）时 wm 被清空、
    外壳重新挂载，而地址栏还停在上一个人走到的地方；不重新对齐一次的话，界面显示
    的是 forge 记着的那一面，地址栏写的是另一处，直到下一次点击才被纠正。
    订阅和监听只挂一次。 */
export function startRouting(label: (nav: NavKey) => string, normalize: (location: Loc) => Loc = location => location) {
  labelOf = label;
  normalizeLocation = normalize;
  cancelPositionSave();
  positionRestoreCleanup?.();
  positionCache.clear();
  // 地址中有位置时以地址为准；站点根地址始终打开默认列表，不恢复上次页面或下钻。
  // 使用 replace，避免为首次进入额外增加一条历史记录。
  const initial = parse(window.location.hash);
  const initialLocation = normalizeLocation(initial ?? { nav: DEFAULT_NAV });
  apply(initialLocation);
  const initialHash = serialize(initialLocation);
  const restoredState = routeHistoryState();
  historyIndex = restoredState?.index ?? 0;
  historyHash = initialHash;
  const initialState = restoredState ?? { index: historyIndex, scrollTop: 0 };
  cachePosition(historyIndex, initialState);
  writeRouteHistory('replaceState', initialState, initialHash);
  notifyHistory();
  if ('scrollRestoration' in window.history) window.history.scrollRestoration = 'manual';
  restoreWorkspacePosition(restoredState ?? { scrollTop: 0 });

  if (started) return;
  started = true;

  const sync = () => {
    if (applying) return;
    const next = serialize(current());
    /* 地址未变化时不写入历史记录。wm 的每次拖动窗口和测量高度都会触发该函数，它们不属于导航。 */
    if (next === window.location.hash) return;
    if (pushLocation(next)) restoreWorkspacePosition({ scrollTop: 0 });
    else apply(parse(window.location.hash) ?? { nav: DEFAULT_NAV });
  };
  forge.subscribe(sync);
  wm.subscribe(sync);

  // 工作区滚动只采集到内存。rAF 仍可能每秒写 60/120 次 history，耗尽 WebKit
  // 与导航共用的配额；停止操作后防抖保存，并为频繁短滚动保留至少一秒的写入间隔。
  const recordPosition = () => {
    if (rememberCurrentPosition()) schedulePositionSave();
  };
  document.addEventListener(
    'scroll',
    event => {
      const scroller = workspaceScroller();
      if (!scroller) return;
      const target = scroller === document.scrollingElement ? document : scroller;
      if (event.target !== target) return;
      recordPosition();
    },
    true,
  );
  window.addEventListener('pagehide', () => {
    rememberCurrentPosition();
    persistCurrentPosition();
  });

  // popstate 处理前进后退；hashchange 处理手动修改地址栏。hash 导航在部分浏览器会连续触发
  // 两者，第二次必须按序列化位置去重，否则同一个返回动作会应用两次。
  const restore = (event: PopStateEvent | HashChangeEvent) => {
    const next = normalizeLocation(parse(window.location.hash) ?? { nav: DEFAULT_NAV });
    const canonicalHash = serialize(next);
    const previous = current();
    const poppedState = event instanceof PopStateEvent ? routeHistoryState(event.state) : routeHistoryState();
    if (revertingTraversal) {
      // history.go() 异步返回被拒绝离开前的位置。原 traversal 自己还可能补发一次
      // hashchange；那次 URL 仍是被拒绝的目标，不能提前消耗标记。只有 URL 与仍保留的
      // 应用状态重新一致时，才算真正回到了原历史项。
      if (serialize(previous) !== serialize(next)) return;
      revertingTraversal = false;
      historyIndex = poppedState?.index ?? historyIndex;
      return;
    }
    if (serialize(previous) === canonicalHash && (!poppedState || poppedState.index === historyIndex)) {
      if (canonicalHash !== window.location.hash) {
        writeRouteHistory('replaceState', poppedState ?? { index: historyIndex, scrollTop: 0 }, canonicalHash);
      }
      return;
    }

    // Direct browser traversal bypasses moveTo. Save the old DOM to its in-memory index only;
    // replaceState here would overwrite the destination entry with the departing page's position.
    rememberCurrentPosition();
    cancelPositionSave();
    if (!approvedTraversal && !confirmDiscardChanges()) {
      revertingTraversal = true;
      const delta = poppedState ? historyIndex - poppedState.index : -1;
      window.history.go(delta || -1);
      return;
    }

    approvedTraversal = false;
    cancelVisualTransition();
    const nextIndex = poppedState?.index ?? historyIndex + 1;
    if (!poppedState) {
      for (const index of positionCache.keys()) {
        if (index >= nextIndex) positionCache.delete(index);
      }
    }
    const state = poppedState ?? {
      index: nextIndex,
      scrollTop: 0,
      fromHash: serialize(previous),
    };
    const position = positionCache.get(nextIndex) ?? state;
    historyIndex = nextIndex;
    historyHash = canonicalHash;
    cachePosition(historyIndex, position);
    if (!poppedState || canonicalHash !== window.location.hash) {
      writeRouteHistory('replaceState', { ...state, ...position }, canonicalHash);
    }
    apply(next);
    notifyHistory();
    restoreWorkspacePosition(position);
  };
  window.addEventListener('popstate', restore);
  window.addEventListener('hashchange', restore);
}
