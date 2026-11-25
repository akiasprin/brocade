import type { CvmEntryMode, CvmEntryProtocol, CvmEntryReservedPorts, CvmEntryAddressOffer } from '../../api';

export const MAX_ENTRY_RANGE_PORTS = 4096;
export function portIntervalText(start: number, end: number): string {
  return start === end ? String(start) : `${start}-${end}`;
}
export function entryPorts(mode: CvmEntryMode) {
  if (mode.kind !== 'l4_range') return null;
  return mode;
}
export function entryPortText(mode: CvmEntryMode, target = false): string {
  const ports = entryPorts(mode);
  if (mode.kind === 'protocols') return entryTrafficText(mode);
  if (!ports) return '';
  return target
    ? portIntervalText(ports.target_start, ports.target_end)
    : portIntervalText(ports.listen_start, ports.listen_end);
}
export function entrySshPort(
  mode: CvmEntryMode,
  reserved?: CvmEntryReservedPorts,
  offer?: CvmEntryAddressOffer,
): number | null {
  const allowed = (port: number | null) =>
    port !== null && !reserved?.tcp.some(r => r.start <= port && port <= r.end) ? port : null;
  if (mode.kind === 'protocols') return protocolSshPort(mode, reserved, offer);
  if (mode.kind === 'l3') return allowed(22);
  const ports = entryPorts(mode)!;
  return allowed(
    ports.protocol !== 'udp' && ports.target_start <= 22 && 22 <= ports.target_end
      ? ports.listen_start + 22 - ports.target_start
      : null,
  );
}
function parseInterval(value: string): [number, number] {
  const match = /^(\d{1,5})(?:-(\d{1,5}))?$/.exec(value.trim());
  if (!match) throw new Error('请填写端口或范围，例如 30000-30099。');
  const start = Number(match[1]),
    end = Number(match[2] ?? match[1]);
  if (start < 1 || end > 65535 || start > end) throw new Error('端口须在 1–65535，结束端口不能小于起始端口。');
  return [start, end];
}
export function entryModeFromPorts(protocol: CvmEntryProtocol, listen: string, target: string): CvmEntryMode {
  const [listen_start, listen_end] = parseInterval(listen);
  const [target_start, target_end] = parseInterval(target);
  if (listen_end - listen_start !== target_end - target_start)
    throw new Error('入口与容器端口范围须包含相同数量的端口。');
  if (listen_end - listen_start + 1 > MAX_ENTRY_RANGE_PORTS)
    throw new Error(`每条入口最多支持 ${MAX_ENTRY_RANGE_PORTS} 个连续端口。`);
  return { kind: 'l4_range', protocol, listen_start, listen_end, target_start, target_end };
}

export function entryTrafficText(mode: CvmEntryMode): string {
  if (mode.kind === 'l3') return '全部协议';
  if (mode.kind === 'l4_range')
    return `${mode.protocol === 'tcp_udp' ? 'TCP+UDP' : mode.protocol.toUpperCase()} ${portIntervalText(mode.listen_start, mode.listen_end)}`;
  return (
    [
      ['TCP', mode.tcp],
      ['UDP', mode.udp],
    ] as const
  )
    .flatMap(([name, value]) =>
      value.kind === 'disabled'
        ? []
        : [
            `${name} ${value.ports.kind === 'all' ? '全部端口' : value.ports.ranges.map(r => portIntervalText(r.start, r.end)).join(',')}${value.mappings.length ? '；映射 ' + value.mappings.map(m => portIntervalText(m.listen.start, m.listen.end) + ' → ' + portIntervalText(m.target.start, m.target.end)).join(',') : ''}`,
          ],
    )
    .concat(mode.icmp ? ['ICMP'] : [], mode.other_ip ? ['其他 IP 协议'] : [])
    .join(' · ');
}
function protocolSshPort(
  mode: Extract<CvmEntryMode, { kind: 'protocols' }>,
  reserved?: CvmEntryReservedPorts,
  offer?: CvmEntryAddressOffer,
): number | null {
  const offered = (port: number) => offer?.kind !== 'shared' || (offer.port_start <= port && port <= offer.port_end);
  if (mode.tcp.kind === 'disabled') return null;
  const mappings = mode.tcp.mappings.filter(m => m.target.start <= 22 && 22 <= m.target.end);
  for (const mapping of mappings) {
    const listen = mapping.listen.start + 22 - mapping.target.start;
    if (offered(listen) && !reserved?.tcp.some(r => r.start <= listen && listen <= r.end)) return listen;
  }
  if (!offered(22) || reserved?.tcp.some(r => r.start <= 22 && 22 <= r.end)) return null;
  if (mode.tcp.mappings.some(m => m.listen.start <= 22 && 22 <= m.listen.end)) return null;
  return mode.tcp.ports.kind === 'all' || mode.tcp.ports.ranges.some(r => r.start <= 22 && 22 <= r.end) ? 22 : null;
}
