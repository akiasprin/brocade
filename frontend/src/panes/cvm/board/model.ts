// 组合台的数据模型：把容器、出口网络（网卡发布）与访问入口拼成「入口 | 计算 | 出口」三栏。
// 这里只做纯计算，状态与权限以服务端返回为准。
import type {
  CvmContainerRoute,
  CvmContainerRoutes,
  CvmEntry,
  CvmEntryAddressOffer,
  CvmEntryAddressUse,
  CvmEntryBoard,
  CvmEntryProtocol,
  CvmEntryPublicationView,
  CvmHost,
  CvmInstance,
  CvmInterface,
  CvmNicCatalog,
  CvmNicPublication,
  CvmRouteService,
} from '../../../api';
import { entryProtocolText } from '../../../api';

export type Layer = 'L3' | 'L4';

/** 一台机器上发布的一个入口地址，以及当前账号看得到的入口。 */
export interface AddressRow {
  key: string;
  publication: CvmEntryPublicationView;
  offer: CvmEntryAddressOffer;
  usage: CvmEntryAddressUse | undefined;
  entries: CvmEntry[];
}
export interface EntryMachine {
  node: string;
  name: string;
  addresses: AddressRow[];
  /** 授权已撤销等原因不在发布目录里的入口，按机器单独列出。 */
  orphans: CvmEntry[];
}
export interface ExitTile {
  id: string;
  publication: CvmNicPublication;
  provider: string;
  kind: 'shared' | 'dedicated' | 'private';
  address: string | null;
}
export interface RuleLink {
  key: string;
  instance: string;
  index: number;
  route: CvmContainerRoute;
  nic: string;
  exit: string | null;
  layer: Layer;
}

export const isDefault = (destination: string) => destination === '0.0.0.0/0' || destination === '::/0';

export function entryMachines(board: CvmEntryBoard, hosts: CvmHost[]): EntryMachine[] {
  const machines = new Map<string, EntryMachine>();
  const machine = (node: string, name: string) => {
    let found = machines.get(node);
    if (!found) {
      found = { node, name, addresses: [], orphans: [] };
      machines.set(node, found);
    }
    return found;
  };
  const listed = new Set<string>();
  for (const publication of board.publications) {
    const owner = machine(publication.spec.provider_node, publication.provider_name);
    publication.spec.addresses.forEach((offer, index) => {
      const usage = publication.usage[index];
      const entries = board.entries.filter(
        entry => entry.publication_id === publication.id && entry.address === offer.address,
      );
      entries.forEach(entry => listed.add(entry.id));
      owner.addresses.push({ key: `${publication.id}:${offer.address}`, publication, offer, usage, entries });
    });
  }
  for (const entry of board.entries) {
    if (listed.has(entry.id)) continue;
    const name = hosts.find(host => host.node_id === entry.provider_node)?.name ?? entry.provider_node;
    machine(entry.provider_node, name).orphans.push(entry);
  }
  return [...machines.values()].sort((a, b) => a.name.localeCompare(b.name) || a.node.localeCompare(b.node));
}

export function exitTiles(catalog: CvmNicCatalog | undefined, hosts: CvmHost[]): ExitTile[] {
  return (catalog?.publications ?? [])
    .filter(publication => !publication.retired)
    .map(publication => {
      const spec = publication.spec;
      const provider =
        catalog?.providers.find(provider => provider.node_id === spec.provider_node)?.name ??
        hosts.find(host => host.node_id === spec.provider_node)?.name ??
        spec.provider_node;
      const kind: ExitTile['kind'] = !spec.destinations.some(isDefault)
        ? 'private'
        : spec.address_mode === 'shared_nat'
          ? 'shared'
          : 'dedicated';
      return {
        id: publication.id,
        publication,
        provider,
        kind,
        address: spec.address_mode === 'shared_nat' ? (spec.shared_ipv4 ?? spec.shared_ipv6) : null,
      };
    })
    .sort(
      (a, b) => a.provider.localeCompare(b.provider) || a.publication.spec.name.localeCompare(b.publication.spec.name),
    );
}

/** 容器路由表中的每一条路由就是一条出口连线。 */
export function ruleLinks(instances: CvmInstance[]): RuleLink[] {
  return instances.flatMap(instance =>
    instance.desired.spec.routing.routes.map((route, index) => {
      const nic = instance.desired.spec.interfaces.find(item => item.id === route.interface_id);
      return {
        key: `${instance.desired.id}:${index}`,
        instance: instance.desired.id,
        index,
        route,
        nic: route.interface_id,
        exit: nic?.allocated?.publication_id ?? null,
        layer: route.service ? 'L4' : 'L3',
      };
    }),
  );
}

export const nicFor = (instance: CvmInstance, publication: string): CvmInterface | undefined =>
  instance.desired.spec.interfaces.find(nic => nic.allocated?.publication_id === publication);

export function routeLabel(route: CvmContainerRoute): string {
  if (!route.service) return route.destination;
  const { protocol, port_start, port_end } = route.service;
  const ports = port_start === port_end ? `${port_start}` : `${port_start}-${port_end}`;
  return `${protocol}/${ports}${isDefault(route.destination) ? '' : ` ${route.destination}`}`;
}

/** `tcp/443`、`udp/1000-2000`。 */
export function parseService(text: string): CvmRouteService | null {
  const match = /^(tcp|udp)\/(\d{1,5})(?:-(\d{1,5}))?$/i.exec(text.trim());
  if (!match) return null;
  const port_start = Number(match[2]);
  const port_end = Number(match[3] ?? match[2]);
  if (port_start < 1 || port_end < port_start || port_end > 65535) return null;
  return { protocol: match[1].toLowerCase() as 'tcp' | 'udp', port_start, port_end };
}

/** 新路由加在末尾；网卡还没有优先级时补一个比现有都低的。 */
export function withRoute(
  routing: CvmContainerRoutes,
  route: CvmContainerRoute,
  nic: CvmInterface,
): CvmContainerRoutes {
  const next = structuredClone(routing);
  next.routes.push(route);
  if (!next.priorities.some(priority => priority.interface_id === nic.id)) {
    const priority = Math.min(65535, Math.max(0, ...next.priorities.flatMap(p => [p.ipv4 ?? 0, p.ipv6 ?? 0])) + 10);
    next.priorities.push({
      interface_id: nic.id,
      ipv4: nic.allocated?.ipv4 ? priority : null,
      ipv6: nic.allocated?.ipv6 ? priority : null,
    });
  }
  return next;
}
export function withoutRoute(routing: CvmContainerRoutes, index: number): CvmContainerRoutes {
  const next = structuredClone(routing);
  next.routes.splice(index, 1);
  return next;
}
export function repointRoute(routing: CvmContainerRoutes, index: number, nic: CvmInterface): CvmContainerRoutes {
  const route = routing.routes[index];
  return withRoute(withoutRoute(routing, index), { ...route, interface_id: nic.id }, nic);
}

const protocols = (protocol: CvmEntryProtocol): ('tcp' | 'udp')[] =>
  protocol === 'tcp_udp' ? ['tcp', 'udp'] : [protocol];
export function portTaken(usage: CvmEntryAddressUse | undefined, protocol: CvmEntryProtocol, port: number) {
  return (usage?.ports ?? []).some(
    use => protocols(protocol).includes(use.protocol) && port >= use.port_start && port <= use.port_end,
  );
}
/** 共享地址端口段内第一个空闲端口，优先 `preferred`。 */
export function nextFreePort(
  offer: Extract<CvmEntryAddressOffer, { kind: 'shared' }>,
  usage: CvmEntryAddressUse | undefined,
  protocol: CvmEntryProtocol,
  preferred?: number,
): number | null {
  if (
    preferred &&
    preferred >= offer.port_start &&
    preferred <= offer.port_end &&
    !portTaken(usage, protocol, preferred)
  )
    return preferred;
  for (let port = offer.port_start; port <= offer.port_end; port += 1)
    if (!portTaken(usage, protocol, port)) return port;
  return null;
}

export function entryText(entry: CvmEntry): { layer: Layer; badge: string; text: string; target: string } {
  if (entry.mode.kind === 'l3') return { layer: 'L3', badge: 'L3', text: entry.visible_address, target: '整 IP' };
  return {
    layer: 'L4',
    badge: entryProtocolText(entry.mode.protocol),
    text: `${entry.visible_address}:${entry.mode.listen_port}`,
    target: `→ ${entry.mode.target_port}`,
  };
}
