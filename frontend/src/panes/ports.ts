import type { NodeAgentStateItem, SnapshotApp } from '../api';

// 类型使用 `import type`，运行时不引入 api.ts，因此不会关联 draft.ts 的浏览器状态。

// slug 的字符集和长度上限，与 model.rs 的 is_valid_slug / SLUG_MAX_LEN 一致。
// 违反该约束时编译报 label.charset 并阻止发布——而配置本身可以运行，
// 因此没有任何运行时表现能提前暴露该问题。
export const SLUG_MAX = 32;
export const isValidSlug = (value: string) => /^[a-z0-9._-]{1,32}$/.test(value);

export type PortOwners = Map<number, string>;

// 各机器上已占用的端口及其占用方。
// 判定与 validate.rs 的 validate_ports / validate_app_set_ports 一致：接入面、各链
// 在该机器上的中转 inbound、WireGuard 监听端口、phantun 的 TCP 端口。端口在线路间
// 共享——两个线路各自合法但组合后可能冲突，因此需要扫描全部 apps，不能只检查当前线路。
//
// 冲突的表现是 xray 启动失败而配置在界面上没有异常；编译器会报 node.port-clash，
// 但该错误要到发布前才可见，因此向导给出的默认值本身就不应冲突。
export function occupiedPorts(
  apps: SnapshotApp[],
  nodes: NodeAgentStateItem[],
  system: unknown,
  // 修改某个接入面的端口时需要将其自身排除，否则它占用的正是当前端口，
  // 打开时即报与自身冲突。
  skipIngress?: string,
  protocol: 'tcp' | 'udp' = 'tcp',
): Map<string, PortOwners> {
  const byNode = new Map<string, PortOwners>();
  const put = (node: string, port: number | null | undefined, what: string) => {
    if (!port) return;
    const owners = byNode.get(node) ?? new Map<number, string>();
    if (!owners.has(port)) owners.set(port, what);
    byNode.set(node, owners);
  };

  for (const app of apps) {
    for (const ingress of app.ingresses ?? []) {
      if (ingress.id === skipIngress) continue;
      // 两条线使用各自的端口：TCP 一侧使用 ingress.port，UDP 一侧使用 hysteria2.port。
      // 此前两侧都读取 ingress.port——那是两条线共用同一端口时的实现，端口拆分后
      // 会将 UDP 一侧计入 TCP 的端口，导致分配器给出的空闲端口实际已被占用。
      if (protocol === 'udp') {
        const hy2 = ingress.wires.hysteria2;
        if (!hy2) continue;
        put(ingress.node, hy2.port, `线路 ${app.id} 的接入面 ${ingress.id}`);
        // 跳转区间整段都计入占用：该区间内的端口全部重定向到监听端口，
        // 在该区间内开启的服务收不到数据包，而该机器上的配置在界面上没有异常。
        if (hy2.hop) {
          for (let port = hy2.hop.start; port <= hy2.hop.end; port += 1) {
            put(ingress.node, port, `线路 ${app.id} 接入面 ${ingress.id} 的跳转区间`);
          }
        }
        continue;
      }
      if (ingress.wires.vless) {
        put(ingress.node, ingress.port, `线路 ${app.id} 的接入面 ${ingress.id} VLESS`);
      }
      if (ingress.wires.anytls) {
        put(ingress.node, ingress.wires.anytls.port, `线路 ${app.id} 的接入面 ${ingress.id} AnyTLS`);
      }
    }
    if (protocol === 'tcp') {
      for (const step of app.steps ?? []) {
        put(step.node, step.hop_in?.port, `线路 ${app.id} 链 ${step.chain} 的中转口`);
      }
    }
  }
  if (protocol === 'tcp') {
    for (const n of nodes) put(n.node_id, n.wg_fake_tcp_port, 'phantun 伪 TCP 口');
  }
  /* 系统层只存在于编译产生的 IR 中。获取失败时少检查一项，不影响向导的使用。 */
  const systemNodes = (
    system as { nodes?: { id: string; wireguard?: { listen_port?: number | null } | null }[] } | undefined
  )?.nodes;
  if (Array.isArray(systemNodes)) {
    if (protocol === 'udp') {
      for (const n of systemNodes) put(n.id, n.wireguard?.listen_port, 'WireGuard');
    }
  }
  return byNode;
}

// 从 `from` 起向上查找第一个在这些机器上均未被占用的端口。中转端口对非入口节点使用
// 同一取值，因此需要一并检查：只在其中一台上空闲不满足条件。
export function freePortAcross(taken: Map<string, PortOwners>, nodeIds: string[], from: number): number {
  let port = from;
  while (port < 65536 && nodeIds.some(id => taken.get(id)?.has(port))) port += 1;
  return port;
}

// 端口跳转需要连续的一段端口而非单个端口。单端口版本不适用于此：从 `from` 向上找到第一个
// 空闲端口后，若其后第 3 个已被占用则整段不成立，需要从更后的位置重新查找。
//
// 返回区间起点。查找失败时（一直到 65535 都冲突）返回 `from`，由上层通过 portClash 报出
// 占用方——直接返回一个冲突的区间会导致发布时才出现 node.port-clash，而操作者认为未修改过端口。
export function freeSpanAcross(taken: Map<string, PortOwners>, nodeIds: string[], from: number, span: number): number {
  const busy = (port: number) => nodeIds.some(id => taken.get(id)?.has(port));
  let start = from;
  while (start + span - 1 <= 65535) {
    let hit = -1;
    for (let i = 0; i < span; i += 1) {
      if (busy(start + i)) hit = i;
    }
    if (hit < 0) return start;
    // 跳转到冲突端口之后而非 start+1：区间内任一端口被占用时，该端口之前的起点都不成立。
    start += hit + 1;
  }
  return from;
}

// 该区间内第一个冲突端口的占用方。区间表示该机器上这段 UDP 端口全部被占用，因此逐个检查。
export function spanClash(
  taken: Map<string, PortOwners>,
  nodeIds: string[],
  start: number,
  end: number,
): string | null {
  for (let port = start; port <= end; port += 1) {
    const owner = portClash(taken, nodeIds, port);
    if (owner) return owner;
  }
  return null;
}

export function portClash(taken: Map<string, PortOwners>, nodeIds: string[], port: number): string | null {
  for (const id of nodeIds) {
    const owner = taken.get(id)?.get(port);
    if (owner) return `${id} 的 ${port} 已经被${owner}占着`;
  }
  return null;
}

// 某一跳的中转端口位于哪台机器。
//
// 常规档位由下游监听、上游连接；反向两档相反——下游连接上游，端口开在上游
// （ir/hops.rs 的 `entry_hop_in`：转发取对端的配置，反向取本机的）。放在此处而非
// 向导组件中，是因为该规则出错时没有任何运行时表现能提前暴露：端口写到不监听的
// 机器上时，编译报的是 relay.no-hop-in，且指向的是另一台机器。
export const hopListener = (spine: string[], i: number, reverse: boolean): string =>
  reverse ? spine[i - 1] : spine[i];

// 整条主干上需要开启中转端口的机器，按主干顺序排列并去重。
//
// 去重是必要的：`hop_in` 在模型中关联在 `(chain, node)` 上，一台机器在一条链上只有
// 一个端口。前一跳常规进入、后一跳反向发出的机器会被计入两次，而两者本应是
// 同一个端口——拆分为两份状态时，两者写入同一条记录，后写入的覆盖先写入的。
export function hopListeners(spine: string[], isReverseHop: (i: number) => boolean): string[] {
  const out: string[] = [];
  for (let i = 1; i < spine.length; i += 1) {
    const host = hopListener(spine, i, isReverseHop(i));
    if (!out.includes(host)) out.push(host);
  }
  return out;
}
