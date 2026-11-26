import './chips.css';
import type { ReactNode } from 'react';
import { entryStateText, type CvmEntry, type CvmInstance, type CvmResources } from '../../api';
import { isDefault } from './network-model';
import { entryPortText, entryTrafficText } from './entry-ports';
import { GIB, MIB } from './queries';
import { targetKey } from './route-target';

/* 清单与详情概览共用的网络标记。入站方式按「协议 · 入站端口 → 容器端口」写，地址进提示：
   同一台入口机器上的多条入口通常共用一个地址，逐条重复地址会把端口挤出视野。 */

export type NetChip = {
  key: string;
  /** 协议、整 IP 等前缀，用弱化的小字写在主文前。 */
  lead?: string;
  text: string;
  /** 转发目标或补充说明，写在主文后。 */
  tail?: string;
  title?: string;
  kind?: 'l3' | 'via' | 'chain' | 'local';
  tone?: 'warn' | 'bad' | 'draft';
};

export function entryChip(entry: CvmEntry): NetChip {
  const state = entryStateText[entry.state];
  const tone = entry.state === 'failed' ? 'bad' : entry.state === 'active' ? undefined : 'warn';
  if (entry.mode.kind === 'l3')
    return {
      key: entry.id,
      lead: 'L3',
      text: entry.visible_address,
      title: `整 IP ${entry.visible_address} · 全部协议 · ${state}`,
      kind: 'l3',
      tone,
    };
  if (entry.mode.kind === 'protocols') {
    const traffic = entryTrafficText(entry.mode);
    return {
      key: entry.id,
      lead: 'IP',
      text: entry.visible_address,
      tail: traffic,
      title: `${entry.visible_address} · ${traffic} · ${state}`,
      kind: 'l3',
      tone,
    };
  }
  const protocol = entry.mode.protocol === 'tcp_udp' ? 'TCP+UDP' : entry.mode.protocol.toUpperCase();
  const listen = entryPortText(entry.mode);
  const target = entryPortText(entry.mode, true);
  return {
    key: entry.id,
    lead: protocol,
    text: listen,
    tail: target === listen ? undefined : `→${target}`,
    title: `${protocol} ${entry.visible_address}:${listen} → 容器 ${target} · ${state}`,
    tone,
  };
}

/** 路由引用到的出口，按首次出现顺序；没有整段默认路由时补一枚「其余 本机」。 */
export function exitChips(
  instance: CvmInstance,
  publicationName: (id: string) => string | undefined,
  chainName: (id: string) => string | undefined,
): NetChip[] {
  const spec = instance.desired.spec;
  const groups = new Map<string, Route[]>();
  for (const route of spec.routing.routes) {
    const key = targetKey(route);
    groups.set(key, [...(groups.get(key) ?? []), route]);
  }
  const chips = [...groups].map(([key, routes]): NetChip => {
    const whole = routes.some(route => isDefault(route.destination) && !route.service);
    // 整段默认路由不需要再写范围；只有分流规则时写第一条，多条再注明条数。
    const tail = whole ? undefined : `${routeScope(routes[0])}${routes.length > 1 ? ` 等 ${routes.length} 条` : ''}`;
    const first = routes[0];
    if (first.chain_id) {
      const name = chainName(first.chain_id) ?? first.chain_id;
      return { key, text: name, tail, title: `线路出口 · ${name}`, kind: 'chain' };
    }
    const nic = spec.interfaces.find(item => item.id === first.interface_id);
    const publicationId = nic?.allocated?.publication_id;
    const text = publicationId ? (publicationName(publicationId) ?? '出站方式') : nic ? '默认出站' : '出站方式待确认';
    return { key, text, tail, kind: 'via' };
  });
  if (!spec.routing.routes.some(route => isDefault(route.destination) && !route.service))
    chips.push({
      key: 'local',
      lead: '其余',
      text: spec.execution.kind === 'baremetal' ? '宿主路由' : '本机',
      title: '未匹配路由的流量经默认出站方式发送',
      kind: 'local',
    });
  return chips;
}

type Route = CvmInstance['desired']['spec']['routing']['routes'][number];

function routeScope(route: Route) {
  if (!route.service) return route.destination;
  const { protocol, port_start, port_end } = route.service;
  const ports = port_start === port_end ? `${port_start}` : `${port_start}-${port_end}`;
  return `${protocol}/${ports}`;
}

export function NetChipView({ chip, onClick }: { chip: NetChip; onClick?: () => void }) {
  const body: ReactNode = (
    <>
      {chip.lead && <i>{chip.lead}</i>}
      <span>{chip.text}</span>
      {chip.tail && <i>{chip.tail}</i>}
    </>
  );
  const className = `cvm-chip${chip.kind ? ` ${chip.kind}` : ''}`;
  return onClick ? (
    <button
      type="button"
      className={className}
      data-tone={chip.tone}
      title={chip.title ?? chip.text}
      aria-label={chip.title ?? chip.text}
      onClick={onClick}
    >
      {body}
    </button>
  ) : (
    <span className={className} data-tone={chip.tone} title={chip.title ?? chip.text}>
      {body}
    </span>
  );
}

const shortSize = (bytes: number) =>
  bytes >= GIB ? `${Number((bytes / GIB).toFixed(bytes >= 10 * GIB ? 0 : 1))}G` : `${Math.round(bytes / MIB)}M`;

/** 清单里的紧凑规格：`2C · 2G · 20G`。完整写法交给提示。 */
export function compactSpec(resources: CvmResources) {
  return `${resources.vcpus}C · ${shortSize(resources.memory_bytes)} · ${shortSize(resources.root_disk_bytes)}`;
}
