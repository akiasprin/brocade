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
import { forge, isNavKey, type NavKey } from './state';

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
}

interface DrillSpec {
  /* 地址中的该段，同时也是 drill.p 的取值 */
  seg: string;
  fields?: Field[];
  /* 恢复时补全的、不进入地址的字段（向导的起始步骤） */
  rest?: Drill;
}

// 哪些页面的下钻状态写入地址。未列出的页面（settings / topo 等）只有一个位置，
// 一个 nav 即可表示；未列出的 `p`（provision 的步骤）保留在 wm.data 中不进入地址。
const DRILL: Partial<Record<NavKey, DrillSpec[]>> = {
  nodes: [
    { seg: 'node', fields: [{ name: 'id' }] },
    { seg: 'chain', fields: [{ name: 'id' }] },
    { seg: 'provision', rest: { step: 1 } },
    // 安装相关的几个页面。机器在提交表单时即已入库，因此此处的标识是 node_id：
    // 切换、刷新、后退都可恢复。恢复后停留在第 4 屏（安装命令）——前两屏是创建时的
    // 一次性回显，重复显示没有意义，详情页有更完整的内容。
    { seg: 'install', fields: [{ name: 'node' }], rest: { step: 4 } },
  ],
  chains: [{ seg: 'chain', fields: [{ name: 'app' }, { name: 'chain' }] }],
  /* 开户表单和用户详情都需要进入地址。用户 id 只在租户内唯一，详情必须同时携带完整
     tenant 路径；否则同名用户会在刷新后定位到错误对象。 */
  users: [{ seg: 'new' }, { seg: 'user', fields: [{ name: 'tenant' }, { name: 'id' }] }],
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
  if (spec && loc.drill) {
    parts.push(spec.seg);
    for (const f of spec.fields ?? []) {
      const v = loc.drill[f.name];
      // 字段缺失时回退到该页面的根路径：少一层优于生成 `#/deploy/detail/undefined`
      // ——该地址解析后会得到一个 id 为空的详情页。
      if (v == null) return `#/${loc.nav}`;
      parts.push(encodeURIComponent(String(v)));
    }
  }
  return `#/${parts.join('/')}`;
}

export function parse(hash: string): Loc | null {
  const parts = hash.replace(/^#\/?/, '').split('/').filter(Boolean).map(decodeURIComponent);
  const [nav, seg, ...rest] = parts;
  if (!nav || !isNavKey(nav)) return null;

  const spec = specFor(nav, seg);
  if (!spec) return { nav };

  const drill: Drill = { p: spec.seg, ...spec.rest };
  const fields = spec.fields ?? [];
  // 缺少路径段时视为未下钻：地址可被手动修改，`#/deploy/detail` 不应渲染为
  // id 为 undefined 的详情页。
  if (rest.length < fields.length) return { nav };
  fields.forEach((f, i) => {
    const raw = rest[i];
    if (!f.num) {
      drill[f.name] = raw;
      return;
    }
    const n = Number(raw);
    drill[f.name] = Number.isFinite(n) ? n : raw;
  });
  return { nav, drill };
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

// 页面名称由外壳管理（NAV / MORE 两张表），在 startRouting 时传入；
// 在此处 import 会形成循环依赖。只在打开窗口时使用一次，默认值为 nav 本身。
let labelOf: (nav: NavKey) => string = nav => nav;

function apply(loc: Loc) {
  applying = true;
  try {
    forge.setNav(loc.nav);
    // 只有支持下钻的页面需要在此写入状态。其他页面（topo / links / 设置等）由外壳的 Work
    // 自行打开窗口——topo 和 links 不使用 wm，在此为它们创建 `tab:` 窗口时，该窗口不会被
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
    修改 DRILL 表。 */
export function navigate(nav: NavKey, drill?: Loc['drill']) {
  const loc: Loc = { nav, drill };
  // apply 期间抑制 sync（它连续修改 forge 和 wm，每次修改都会触发一次），完成后
  // 统一写入一条。中间的过渡状态不是实际访问过的位置，不应各占一条历史记录。
  apply(loc);
  const next = serialize(loc);
  if (next !== window.location.hash) window.history.pushState(null, '', next);
}

/* ══ 启动 ══ */

let started = false;

/** 登录之后、外壳挂载时调。label 由外壳给：面的名字归它管（NAV / MORE），
    这里去 import 会绕回一个环。

    每次登录都要调，不是只调第一次。退出再进来（尤其是换个人）时 wm 被清空、
    外壳重新挂载，而地址栏还停在上一个人走到的地方；不重新对齐一次的话，界面显示
    的是 forge 记着的那一面，地址栏写的是另一处，直到下一次点击才被纠正。
    订阅和监听只挂一次。 */
export function startRouting(label: (nav: NavKey) => string) {
  labelOf = label;
  // 首帧：地址中有位置时以地址为准，没有时将当前位置（forge 从 localStorage 恢复的页面）
  // 写入地址。使用 replace——进入时即写入一条历史记录会使后退键退到空白页。
  const initial = parse(window.location.hash);
  if (initial) {
    apply(initial);
  } else {
    window.history.replaceState(null, '', serialize(current()));
  }

  if (started) return;
  started = true;

  const sync = () => {
    if (applying) return;
    const next = serialize(current());
    /* 地址未变化时不写入历史记录。wm 的每次拖动窗口和测量高度都会触发该函数，它们不属于导航。 */
    if (next === window.location.hash) return;
    window.history.pushState(null, '', next);
  };
  forge.subscribe(sync);
  wm.subscribe(sync);

  // popstate 处理前进后退；hashchange 处理手动修改地址栏。同一次导航同时触发两者不影响结果，
  // apply 到相同位置时不产生任何操作。
  const restore = () => {
    const loc = parse(window.location.hash);
    if (loc) apply(loc);
  };
  window.addEventListener('popstate', restore);
  window.addEventListener('hashchange', restore);
}
