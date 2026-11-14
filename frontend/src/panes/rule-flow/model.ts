import type { ConsoleSnapshot, DestMatch, HopPool, Rule, SnapshotChain, SnapshotStep } from '../../api';

/** Only the public routing facts used by the inspector. Never project hop credentials or keys. */
export interface FlowCompiledApp {
  steps?: { chain: string; node: string; rules: { dest_match: DestMatch; action: Rule['a'] }[] }[];
  hops?: {
    chain: string;
    target_chain: string;
    from: string;
    to: string;
    address: string;
    port: number;
    path: 'local' | 'overlay' | 'direct' | 'reverse';
    security: { t: string };
    pool: HopPool;
  }[];
}

export interface FlowNodeInfo {
  node_id: string;
  name: string;
  public_ipv4: string | null;
  public_ipv6: string | null;
  public_ipv4_country?: string | null;
}

export interface FlowRule {
  id: string;
  owner: string;
  index: number;
  rule: Rule;
  generated: boolean;
  shadowedBy: number | null;
  target: string | null;
  hop: FlowHop | null;
}

export interface FlowHop {
  id: string;
  label: string;
  endpoint: string | null;
  wire: string | null;
  pool: string | null;
  plaintext: boolean;
}

export interface FlowMachine {
  id: string;
  node: string;
  chain: string;
  name: string;
  country: string | null;
  address: string;
  reference: string | null;
  depth: number;
  reachable: boolean;
  rules: FlowRule[];
  dns: ConsoleSnapshot['node_egress_dns'];
  dnsPending: boolean;
  domainStrategy: string | null;
  listen: string;
  listenPorts: number[];
}

export interface FlowModel {
  machines: FlowMachine[];
  entry: string | null;
  outbounds: { id: string; name: string; address: string; port: number; protocol: string }[];
}

export const flowKey = (chain: string, node: string) => JSON.stringify([chain, node]);
export const hostPort = (host: string, port: number) => `${host.includes(':') ? `[${host}]` : host}:${port}`;

const KINDS: Record<DestMatch['t'], string> = {
  any: '任意',
  sniffing_failed: '嗅探失败',
  domain_suffix: '域名后缀',
  domain_keyword: '域名关键词',
  domain_regex: '域名正则',
  geosite: 'GEOSITE',
  geoip: 'GEOIP',
  ip_cidr: 'IP',
  port: '端口',
  network: '传输层',
  all: '组合',
};
export function flowMatch(match: DestMatch): string {
  if (match.t === 'any') return '任意';
  if (match.t === 'sniffing_failed') return '嗅探失败';
  if (match.t === 'all') return match.v.map(flowMatch).join(' 且 ');
  return `${KINDS[match.t]} · ${Array.isArray(match.v) ? match.v.join(' / ') : match.v}`;
}

/** Only prove coverage from literal selectors. Geo sets and regular expressions need Xray. */
export function flowCovers(earlier: DestMatch, later: DestMatch): boolean {
  if (earlier.t === 'any') return true;
  if (JSON.stringify(earlier) === JSON.stringify(later)) return true;
  if (earlier.t === 'domain_suffix' && later.t === 'domain_suffix')
    return later.v.every(value => earlier.v.some(suffix => value === suffix || value.endsWith(`.${suffix}`)));
  return false;
}

export function buildFlowModel({
  chain,
  root,
  steps,
  snapshot,
  nodes,
  compiled = [],
  localRules = {},
  uncompiledNodes = new Set(),
  pendingDnsNodes = new Set(),
  pendingListeners = new Set(),
}: {
  chain: SnapshotChain;
  root: string | undefined;
  steps: SnapshotStep[];
  snapshot?: ConsoleSnapshot;
  nodes: FlowNodeInfo[];
  compiled?: FlowCompiledApp[];
  localRules?: Record<string, Rule[]>;
  uncompiledNodes?: ReadonlySet<string>;
  pendingDnsNodes?: ReadonlySet<string>;
  pendingListeners?: ReadonlySet<string>;
}): FlowModel {
  const apps = snapshot?.snapshot.apps ?? [];
  const allSteps = new Map(apps.flatMap(app => app.steps).map(step => [flowKey(step.chain, step.node), step]));
  for (const step of steps) allSteps.set(flowKey(step.chain, step.node), step);
  const compiledSteps = new Map(
    compiled.flatMap(app => app.steps ?? []).map(step => [flowKey(step.chain, step.node), step]),
  );
  const chainNames = new Map(apps.flatMap(app => app.chains).map(item => [item.id, item.name || item.id]));
  const infoOf = new Map(nodes.map(node => [node.node_id, node]));
  const modelNodes = new Map((snapshot?.snapshot.nodes ?? []).map(node => [node.id, node]));
  const hops = compiled.flatMap(app => app.hops ?? []);
  const machines = new Map<string, FlowMachine>();
  const entry = root ? flowKey(chain.id, root) : null;
  const queue = root ? [{ chain: chain.id, node: root, depth: 0, reachable: true }] : [];
  // Retain orphan tables as inspectable facts, without suggesting traffic reaches them.
  const appendOrphans = () => {
    for (const step of steps)
      if (!machines.has(flowKey(step.chain, step.node)))
        queue.push({ chain: step.chain, node: step.node, depth: 0, reachable: false });
  };
  for (let i = 0; ; i += 1) {
    if (i === queue.length) appendOrphans();
    const at = queue[i];
    if (!at) break;
    const id = flowKey(at.chain, at.node);
    if (machines.has(id)) continue;
    const step = allSteps.get(id);
    const written = at.chain === chain.id ? (localRules[at.node] ?? step?.rules ?? []) : (step?.rules ?? []);
    const full = compiledSteps.get(id)?.rules ?? [];
    const tail = full.at(-1);
    // A compiler tail must actually be additional to the persisted table. A removed explicit
    // catch-all must not be relabelled as a generated fallback from a stale compile response.
    const fallback =
      written.at(-1)?.m.t !== 'any' && full.length > (step?.rules.length ?? 0) && tail?.dest_match.t === 'any'
        ? { m: tail.dest_match, a: tail.action }
        : null;
    const rules = [...written, ...(fallback ? [fallback] : [])].map((rule, index): FlowRule => {
      const action = rule.a;
      const targetChain = action.t === 'reuse_listener' ? action.listener.chain : at.chain;
      const targetNode =
        action.t === 'forward' ? action.to : action.t === 'reuse_listener' ? action.listener.node : null;
      const target = targetNode ? flowKey(targetChain, targetNode) : null;
      if (targetNode)
        queue.push({ chain: targetChain, node: targetNode, depth: at.depth + 1, reachable: at.reachable });
      const ir = targetNode
        ? hops.find(
            hop =>
              hop.chain === at.chain &&
              hop.target_chain === targetChain &&
              hop.from === at.node &&
              hop.to === targetNode,
          )
        : null;
      const localChanged =
        uncompiledNodes.has(at.node) ||
        (at.chain === chain.id &&
          localRules[at.node] !== undefined &&
          JSON.stringify(written) !== JSON.stringify(step?.rules ?? []));
      // Compile facts describe the saved draft. An unsaved dial or listener edit invalidates them.
      const compiledHop = ir && !localChanged ? ir : null;
      const wire = compiledHop
        ? ({ none: 'VLESS 不加密', encryption: 'VLESS 加密', reality: 'REALITY', shadowsocks2022: 'SS2022' }[
            compiledHop.security.t
          ] ?? compiledHop.security.t)
        : null;
      const family = compiledHop?.address.includes(':') ? 'IPv6' : 'IPv4';
      const peer = targetNode ? infoOf.get(targetNode) : null;
      const publicAddress = !!compiledHop && [peer?.public_ipv4, peer?.public_ipv6].includes(compiledHop.address);
      const hop: FlowHop | null = target
        ? {
            id: JSON.stringify([id, target]),
            label: compiledHop
              ? compiledHop.path === 'overlay'
                ? 'WireGuard'
                : compiledHop.path === 'local'
                  ? '本机回环'
                  : compiledHop.path === 'reverse'
                    ? `反向 ${family}`
                    : `${publicAddress ? '公网' : '直连'} ${family}`
              : '待编译',
            endpoint: compiledHop ? hostPort(compiledHop.address, compiledHop.port) : null,
            wire,
            pool: compiledHop
              ? compiledHop.pool.t === 'none'
                ? '每流新建'
                : `Mux · ${compiledHop.pool.v ? `并发 ${compiledHop.pool.v.concurrency}` : '跟随全局'}`
              : null,
            plaintext:
              !!compiledHop && ['direct', 'reverse'].includes(compiledHop.path) && compiledHop.security.t === 'none',
          }
        : null;
      return {
        id: JSON.stringify([id, index]),
        owner: id,
        index,
        rule,
        generated: index >= written.length,
        shadowedBy: (() => {
          const by = written.slice(0, index).findIndex(earlier => flowCovers(earlier.m, rule.m));
          return by < 0 ? null : by + 1;
        })(),
        target,
        hop,
      };
    });
    const info = infoOf.get(at.node);
    const modelNode = modelNodes.get(at.node);
    const ingressPorts = apps
      .flatMap(app => app.ingresses)
      .filter(ingress => ingress.chain === at.chain && ingress.node === at.node)
      .map(ingress => ingress.port);
    const entryListener = id === entry && ingressPorts.length > 0;
    const listenPorts = pendingListeners.has(at.node)
      ? []
      : entryListener
        ? ingressPorts
        : step?.hop_in
          ? [step.hop_in.port]
          : [];
    machines.set(id, {
      id,
      node: at.node,
      chain: at.chain,
      name: info?.name || modelNode?.name || at.node,
      country: info?.public_ipv4_country ?? null,
      address: [info?.public_ipv4, info?.public_ipv6].filter(Boolean).join(' · '),
      reference: at.chain === chain.id ? null : (chainNames.get(at.chain) ?? at.chain),
      depth: at.depth,
      reachable: at.reachable,
      rules,
      dns: (snapshot?.node_egress_dns ?? [])
        .filter(policy => policy.node === at.node)
        .sort((a, b) => a.position - b.position),
      dnsPending: pendingDnsNodes.has(at.node),
      domainStrategy: modelNode?.domain_strategy ?? null,
      listen: pendingListeners.has(at.node)
        ? '中转配置有未保存修改'
        : listenPorts.length
          ? `${entryListener ? '入口' : '中转'}端口 ${listenPorts.join(' / ')}`
          : '未配置中转监听',
      listenPorts,
    });
  }
  // Longest acyclic distance keeps a shared downstream card to the right of every upstream.
  const visiting = new Set<string>();
  const visited = new Set<string>();
  const order: FlowMachine[] = [];
  const visit = (id: string) => {
    if (visiting.has(id) || visited.has(id)) return;
    const machine = machines.get(id);
    if (!machine) return;
    visiting.add(id);
    for (const rule of machine.rules) if (rule.target) visit(rule.target);
    visiting.delete(id);
    visited.add(id);
    order.push(machine);
  };
  for (const id of machines.keys()) visit(id);
  for (const machine of order.reverse())
    for (const rule of machine.rules) {
      const target = rule.target ? machines.get(rule.target) : null;
      if (target && target.id !== entry && target.depth <= machine.depth && machine.depth < machines.size)
        target.depth = machine.depth + 1;
    }
  return {
    machines: [...machines.values()],
    entry,
    outbounds: (snapshot?.snapshot.external_outbounds ?? []).map(outbound => ({
      id: outbound.id,
      name: outbound.name,
      address: outbound.address,
      port: outbound.port,
      protocol: outbound.protocol.t,
    })),
  };
}
