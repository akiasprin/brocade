// 检视台面的数据：全部来自服务端 materialize 和 compile 的结果
// （浏览器中不执行编译，不维护第二份模型）。
// 此处只完成 IR 到画布坐标的几何转换，不做任何推导。

export interface SystemNodeIr {
  id: string;
  tenant: string;
  /* 不在 overlay 中的机器（只作中继）该项为 null——它没有 overlay 地址。 */
  overlay_addr: string | null;
  /* 该机器 wg0 的 MTU（单独设置的值或全局默认值）。 */
  mtu?: number;
  // 三种状态，不能只判断 endpoint：
  // - `null` 表示该机器不在 overlay 中（只作中继，没有 wg 配置）
  // - `{endpoint: null}` 表示在 overlay 中但位于 NAT 之后，只能主动发起连接
  // - `{endpoint: "1.2.3.4:51820"}` 表示有落点，可被其他机器连接
  wireguard?: { endpoint: string | null } | null;
}
// 一条 overlay 链路。三个属性均由编译器计算（ir/system.rs）：
//
// - `dial`：连接方向。判定依据是对端是否有本机可达的落点（has_landing）。
//   序列化格式为 kebab-case：both / ato-b / bto-a。`ato-b` 表示由 a 连接 b。
// - `wrap`：是否使用 TCP 封装。启用时两端同时启用，servers 的键是部署 phantun 服务端的机器。
// - `keepalive_secs`：单向连接时写在发起一侧。
//
// 此处不含 MTU：MTU 是 wg 接口的属性（SystemNodeIr.mtu），不是链路的属性。链路的
// path_mtu 由探测得出，位于 /links/mtu，两者需要在界面上关联才有意义。
export interface LinkIr {
  id: string;
  a: string;
  b: string;
  dial?: 'both' | 'ato-b' | 'bto-a';
  keepalive_secs?: number;
  wrap?: { t: 'udp' } | { t: 'fake_tcp'; v?: { servers?: Record<string, string> } };
}
export interface SystemIr {
  revision: number;
  overlay_cidr: string;
  node_count: number;
  nodes: SystemNodeIr[];
  links: LinkIr[];
}

export interface HopIr {
  chain: string;
  from: string;
  to: string;
  link: string;
  address: string;
  port: number;
  path: 'overlay' | 'direct' | 'reverse';
  security?: { t: string } | null;
}

// 一跳的稳定标识。IR 中 hop 没有 id——它由所属链和起止节点唯一确定
// （同一条链中同一对节点之间只有一跳）。检视卡据此定位对应的 hop。
export function hopKey(h: { chain: string; from: string; to: string }): string {
  return `${h.chain}|${h.from}|${h.to}`;
}
export interface IngressIr {
  id: string;
  chain: string;
  node: string;
  port: number;
  front?: string | null;
}
export interface AppIr {
  app_id: string | null;
  nodes: { id: string; name: string; egress_allowed: boolean }[];
  chains: { id: string; name: string; root: string | null }[];
  ingresses: IngressIr[];
  fronts: { id: string; name: string; via: string[] }[];
  hops: HopIr[];
  grants: { tenant: string; user: string; ingress: string }[];
}

export function undirectedNodePairKey(a: string, b: string) {
  return a <= b ? `${a}|${b}` : `${b}|${a}`;
}

// ══════════════════════════════════════════════════════════════
// 项目视图：一条链一张图
//
// 不采用一个项目一张图的原因：合并一个项目所有链的 hops 后，边集可能存在环
// （app-hk-01 的三条链合并后即为 sg → au → jp → my → sg）。单条链无环，合并后有环。
// 即不存在一种分层方式使所有边都朝同一方向，用一张图表示整个项目在几何上无解。
// 修改该布局之前的代码不报错，是因为它按角色分三列，完全丢弃了顺序信息。
// ══════════════════════════════════════════════════════════════

export interface ChainView {
  id: string;
  name: string;
  /* 链头即接入面所在的机器。没有接入面的链无法绘制（缺少起点）。 */
  head: string | null;
  ingress: IngressIr | null;
  hops: HopIr[];
}

export function chainsOf(app: AppIr): ChainView[] {
  return app.chains.map(c => {
    const ingress = c.root === null ? null : (app.ingresses.find(i => i.chain === c.id && i.node === c.root) ?? null);
    return {
      id: c.id,
      name: c.name || c.id,
      head: c.root,
      ingress,
      hops: app.hops.filter(h => h.chain === c.id),
    };
  });
}

export interface Layered {
  cols: string[][];
  depth: number;
  on: Set<string>;
  // 边集中存在环——单条链不应出现该情况（编译前已校验）。若确实出现也不应白屏，
  // 无法分层的节点排在最后一列，并在界面上给出说明。
  cyclic: boolean;
}

// 分层：层号取从链头出发的最长路径长度。
// 不能使用最短路径（普通 BFS）：c3 的 hk→au 是第一跳、c2 的 sg→au 是第二跳，
// 最短路径会将 au 排入第一层，导致 sg→au 这条边需要反向绘制。最长路径保证所有边同向。
export function layerChain(hops: HopIr[], head: string): Layered {
  const ns = new Set<string>([head]);
  for (const h of hops) {
    ns.add(h.from);
    ns.add(h.to);
  }
  const outs = new Map<string, string[]>([...ns].map(n => [n, []]));
  const indeg = new Map<string, number>([...ns].map(n => [n, 0]));
  for (const h of hops) {
    outs.get(h.from)!.push(h.to);
    indeg.set(h.to, indeg.get(h.to)! + 1);
  }

  const layer = new Map<string, number>([...ns].map(n => [n, 0]));
  const queue = [...ns].filter(n => indeg.get(n) === 0);
  let done = 0;
  while (queue.length) {
    const u = queue.shift()!;
    done += 1;
    for (const v of outs.get(u)!) {
      layer.set(v, Math.max(layer.get(v)!, layer.get(u)! + 1));
      indeg.set(v, indeg.get(v)! - 1);
      if (indeg.get(v) === 0) queue.push(v);
    }
  }
  const cyclic = done !== ns.size;
  const depth = Math.max(0, ...layer.values());

  /* 层内先按 id 排序得到稳定的初始顺序，否则同一份数据每次的排列结果不同 */
  const cols: string[][] = Array.from({ length: depth + 1 }, () => []);
  [...ns].sort().forEach(n => cols[layer.get(n)!].push(n));

  /* 再按父节点在上一层的位置计算重心，执行两轮即可消除大部分交叉 */
  const parents = new Map<string, string[]>([...ns].map(n => [n, []]));
  for (const h of hops) parents.get(h.to)!.push(h.from);
  for (let pass = 0; pass < 2; pass += 1) {
    const pos = new Map<string, number>();
    cols.forEach(c => c.forEach((n, i) => pos.set(n, i)));
    const bary = (n: string) => {
      const ps = parents.get(n)!.filter(p => pos.has(p));
      return ps.length ? ps.reduce((s, p) => s + pos.get(p)!, 0) / ps.length : 0;
    };
    for (let ci = 1; ci < cols.length; ci += 1) {
      cols[ci].sort((a, b) => bary(a) - bary(b) || (a < b ? -1 : 1));
    }
  }
  return { cols, depth, on: ns, cyclic };
}

/* 画布可用区域的像素尺寸。 */
export interface Box {
  w: number;
  h: number;
}

// ══════════════════════════════════════════════════════════════
// 项目视图：等距分层
//
// z 轴表示链这一维度。一条链一块板，板内部可以从左到右排列——
// 一个项目的链合并后存在环（app-hk-01 即 sg→au→jp→my→sg），
// 在单个平面上无法分层；拆分到多块板后各自无环。
// ══════════════════════════════════════════════════════════════

const ISO_LAYER_MUL = 2.6;
/* 左侧板名和右侧机器名各自需要的空间 */
const ISO_LABEL_PAD = 186;
/* 板相对内容多出的外边距 */
const ISO_M = 0.72;

export interface IsoPlate {
  chain: ChainView;
  /* 机器在该板上的格坐标。gx 表示第几跳，gy 表示同一跳中的第几个分叉。 */
  pos: Map<string, { gx: number; gy: number }>;
  depth: number;
  y0: number;
  y1: number;
  z: number;
}

export interface IsoGeometry {
  plates: IsoPlate[];
  /* 步长。通过调整它来填满画幅，而非缩放整张图——缩放会同时改变文字大小。 */
  k: number;
  node: { w: number; h: number };
  clientDrop: number;
  /* 格坐标转画布坐标 */
  at: (gx: number, gy: number, z: number) => { x: number; y: number };
  width: number;
  height: number;
  contentW: number;
  contentH: number;
  x0: number;
  y0: number;
  /* 该项目的链均未经过的机器 */
  idle: string[];
}

/* 一条链在其所属的板上分层，同一跳中的分叉纵向排列 */
function plateOf(chain: ChainView): Omit<IsoPlate, 'z'> | null {
  if (!chain.head) return null;
  const layer = layerChain(chain.hops, chain.head);
  const pos = new Map<string, { gx: number; gy: number }>();
  layer.cols.forEach((ids, gx) => {
    ids.forEach((id, ri) => pos.set(id, { gx, gy: ri - (ids.length - 1) / 2 }));
  });
  const gys = [...pos.values()].map(v => v.gy);
  return {
    chain,
    pos,
    depth: layer.depth,
    y0: Math.min(...gys),
    y1: Math.max(...gys),
  };
}

export function isoProject(chains: ChainView[], allNodeIds: string[], box: Box): IsoGeometry {
  const plates = chains
    .map(plateOf)
    .filter((p): p is Omit<IsoPlate, 'z'> => !!p)
    .map((p, ci) => ({ ...p, z: ci }));

  // 先计算内容在格单位下的尺寸，再反推步长——使图形正好填满画幅，
  // 而不是绘制后再整体缩放。
  let uMin = Infinity;
  let uMax = -Infinity;
  let vMin = Infinity;
  let vMax = -Infinity;
  for (const pl of plates) {
    for (const gx of [-ISO_M, pl.depth + ISO_M]) {
      for (const gy of [pl.y0 - ISO_M, pl.y1 + ISO_M]) {
        uMin = Math.min(uMin, gx - gy);
        uMax = Math.max(uMax, gx - gy);
        vMin = Math.min(vMin, gx + gy);
        vMax = Math.max(vMax, gx + gy);
      }
    }
  }
  const uSpan = Math.max(0.8, uMax - uMin);
  const vSpan = Math.max(0.8, vMax - vMin);
  const kW = (Math.max(720, box.w) - ISO_LABEL_PAD * 2) / (0.866 * uSpan);
  const kH = Math.max(420, box.h - 120) / (0.5 * vSpan + ISO_LAYER_MUL * Math.max(0, plates.length - 1));
  const k = Math.max(74, Math.min(205, Math.min(kW, kH)));
  const layerH = k * ISO_LAYER_MUL;
  plates.forEach((pl, ci) => {
    pl.z = ci * layerH;
  });

  /* 方块尺寸随步长变化，否则连线拉长后方块会过小；字号不随之变化。 */
  const nw = Math.round(Math.max(15, Math.min(24, k * 0.17)));
  const node = { w: nw, h: Math.round(nw * 0.76) };
  const clientDrop = Math.round(Math.max(64, Math.min(112, k * 0.82)));

  const iso = (gx: number, gy: number, z: number) => ({
    sx: (gx - gy) * k * 0.866,
    sy: (gx + gy) * k * 0.5 - z,
  });

  const corners: { sx: number; sy: number }[] = [];
  for (const pl of plates) {
    for (const gx of [-ISO_M, pl.depth + ISO_M]) {
      for (const gy of [pl.y0 - ISO_M, pl.y1 + ISO_M]) corners.push(iso(gx, gy, pl.z));
    }
  }
  /* 客户端部分同样属于内容，计入边界计算 */
  const lowest = new Map<string, IsoPlate>();
  for (const pl of plates) {
    const head = pl.chain.head!;
    const cur = lowest.get(head);
    if (!cur || pl.z < cur.z) lowest.set(head, pl);
  }
  for (const [head, pl] of lowest) {
    const gp = pl.pos.get(head);
    if (!gp) continue;
    const base = iso(gp.gx, gp.gy, pl.z);
    corners.push({ sx: base.sx, sy: base.sy + node.w * 0.5 + node.h + clientDrop + 22 });
  }

  const padY = 66;
  const xs = corners.map(p => p.sx);
  const ys = corners.map(p => p.sy);
  const spanX = corners.length ? Math.max(...xs) - Math.min(...xs) : 0;
  const spanY = corners.length ? Math.max(...ys) - Math.min(...ys) : 0;
  const width = Math.max(box.w, spanX + ISO_LABEL_PAD * 2);
  /* 内容宽度小于画幅时居中显示，不靠左对齐 */
  const ox = corners.length ? (width - spanX) / 2 - Math.min(...xs) : width / 2;
  const oy = corners.length ? padY - Math.min(...ys) : padY;

  const used = new Set<string>();
  plates.forEach(pl => {
    for (const id of pl.pos.keys()) used.add(id);
  });

  return {
    plates,
    k,
    node,
    clientDrop,
    at: (gx, gy, z) => {
      const p = iso(gx, gy, z);
      return { x: ox + p.sx, y: oy + p.sy };
    },
    width,
    height: spanY + padY * 2,
    contentW: spanX,
    contentH: spanY,
    x0: (width - spanX) / 2,
    y0: padY,
    idle: allNodeIds.filter(id => !used.has(id)).sort(),
  };
}
