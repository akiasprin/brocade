import type {
  CvmEntryAddressOffer,
  CvmEntryMode,
  CvmEntryPortRange,
  CvmEntryReservedPorts,
  CvmEntryTransportSelection,
} from '../../api';
import { portIntervalText } from './entry-ports';
export type EntryTransportDraft = { enabled: boolean; all: boolean; ports: string; mappings: string };
export type EntryTrafficDraft = {
  tcp: EntryTransportDraft;
  udp: EntryTransportDraft;
  icmp: boolean;
  other_ip: boolean;
};
export function initialEntryTraffic(shared = false, port = ''): EntryTrafficDraft {
  const selection = (enabled: boolean) => ({ enabled, all: !shared, ports: port, mappings: '' });
  return { tcp: selection(true), udp: selection(!shared), icmp: !shared, other_ip: !shared };
}
export function normalizePortRanges(input: CvmEntryPortRange[]): CvmEntryPortRange[] {
  if (
    input.length > 256 ||
    input.some(
      r => !Number.isInteger(r.start) || !Number.isInteger(r.end) || r.start < 1 || r.end > 65535 || r.start > r.end,
    )
  )
    throw new Error('端口须在 1–65535，起点不能大于终点，且最多 256 段。');
  const sorted = input.map(r => ({ ...r })).sort((a, b) => a.start - b.start || a.end - b.end),
    out: CvmEntryPortRange[] = [];
  for (const range of sorted) {
    const last = out.at(-1);
    if (last && range.start <= last.end + 1) last.end = Math.max(last.end, range.end);
    else out.push(range);
  }
  return out;
}
export function parsePortRanges(text: string): CvmEntryPortRange[] {
  if (!text.trim()) return [];
  return normalizePortRanges(
    text
      .trim()
      .split(/[,，\s]+/)
      .map(value => {
        const m = /^(\d{1,5})(?:-(\d{1,5}))?$/.exec(value);
        if (!m) throw new Error(`端口片段“${value}”无效，请用 22,443,8000-8100。`);
        return { start: Number(m[1]), end: Number(m[2] ?? m[1]) };
      }),
  );
}
export function rangesText(ranges: CvmEntryPortRange[]): string {
  return ranges.map(r => portIntervalText(r.start, r.end)).join(',');
}
export function subtractPortRanges(base: CvmEntryPortRange[], cuts: CvmEntryPortRange[]): CvmEntryPortRange[] {
  const out: CvmEntryPortRange[] = [];
  for (const range of normalizePortRanges(base)) {
    let start = range.start;
    for (const cut of normalizePortRanges(cuts)) {
      if (cut.end < start || cut.start > range.end) continue;
      if (cut.start > start) out.push({ start, end: cut.start - 1 });
      start = Math.max(start, cut.end + 1);
      if (start > range.end) break;
    }
    if (start <= range.end) out.push({ start, end: range.end });
  }
  return out;
}
export function entryTrafficFromDraft(draft: EntryTrafficDraft, offer: CvmEntryAddressOffer): CvmEntryMode {
  const make = (value: EntryTransportDraft): CvmEntryTransportSelection => {
    if (!value.enabled) return { kind: 'disabled' };
    const ranges = value.all
      ? [
          {
            start: offer.kind === 'shared' ? offer.port_start : 1,
            end: offer.kind === 'shared' ? offer.port_end : 65535,
          },
        ]
      : parsePortRanges(value.ports);
    if (!ranges.length) throw new Error('请填写转发端口列表或选择全部端口。');
    if (offer.kind === 'shared' && ranges.some(r => r.start < offer.port_start || r.end > offer.port_end))
      throw new Error(`入口端口须在 ${offer.port_start}-${offer.port_end} 内。`);
    const mappings = value.mappings.trim()
      ? value.mappings
          .trim()
          .split(/[\n,，]+/)
          .map(line => {
            const match = /^\s*(\d+(?:-\d+)?)\s*(?:→|->|:)\s*(\d+(?:-\d+)?)\s*$/.exec(line);
            if (!match) throw new Error('映射请每行填写“30022 → 22”或两个等长范围。');
            const listen = parsePortRanges(match[1])[0],
              target = parsePortRanges(match[2])[0];
            if (listen.end - listen.start !== target.end - target.start)
              throw new Error('映射的入口和目标范围必须等长。');
            if (!ranges.some(r => r.start <= listen.start && listen.end <= r.end))
              throw new Error('映射必须位于转发端口范围内。');
            return { listen, target };
          })
          .sort((a, b) => a.listen.start - b.listen.start)
      : [];
    if (mappings.length > 256 || mappings.some((m, i) => i > 0 && m.listen.start <= mappings[i - 1].listen.end))
      throw new Error('端口映射不能重叠，且最多 256 项。');
    if (
      mappings.reduce((n, m) => n + (m.listen.start === m.target.start ? 0 : m.listen.end - m.listen.start + 1), 0) >
      4096
    )
      throw new Error('改端口的映射最多包含 4096 个端口。');
    return { kind: 'forward', ports: value.all ? { kind: 'all' } : { kind: 'ranges', ranges }, mappings };
  };
  if (offer.kind === 'shared' && (draft.icmp || draft.other_ip)) throw new Error('共享地址只能申请 TCP 或 UDP 端口。');
  const mode: CvmEntryMode = {
    kind: 'protocols',
    tcp: make(draft.tcp),
    udp: make(draft.udp),
    icmp: draft.icmp,
    other_ip: draft.other_ip,
  };
  if (mode.tcp.kind === 'disabled' && mode.udp.kind === 'disabled' && !mode.icmp && !mode.other_ip)
    throw new Error('至少选择一种转发协议。');
  if (offer.kind === 'shared') {
    const count = (['tcp', 'udp'] as const)
      .flatMap(p => effectiveEntryRanges(mode, p, offer))
      .reduce((n, r) => n + r.end - r.start + 1, 0);
    if (count === 0 || count > 8192) throw new Error('有效转发端口为空或超出共享入口容量。');
  }
  return mode;
}
export function effectiveEntryRanges(
  mode: CvmEntryMode,
  protocol: 'tcp' | 'udp',
  offer: CvmEntryAddressOffer,
): CvmEntryPortRange[] {
  let ranges: CvmEntryPortRange[] = [];
  if (mode.kind === 'l3') ranges = [{ start: 1, end: 65535 }];
  else if (mode.kind === 'l4_range') {
    if (mode.protocol === 'tcp_udp' || mode.protocol === protocol)
      ranges = [{ start: mode.listen_start, end: mode.listen_end }];
  } else {
    const value = mode[protocol];
    if (value.kind === 'forward')
      ranges =
        value.ports.kind === 'all'
          ? [
              {
                start: offer.kind === 'shared' ? offer.port_start : 1,
                end: offer.kind === 'shared' ? offer.port_end : 65535,
              },
            ]
          : value.ports.ranges;
  }
  return subtractPortRanges(ranges, offer.reserved_ports?.[protocol] ?? []);
}
export function reservedPortsText(value?: CvmEntryReservedPorts): string {
  return (['tcp', 'udp'] as const)
    .flatMap(p => (value?.[p].length ? [`${p.toUpperCase()} ${rangesText(value[p])}`] : []))
    .join(' · ');
}

export function entryUsageConflict(
  mode: CvmEntryMode,
  offer: CvmEntryAddressOffer,
  usage?: { whole_taken: boolean; ports: { protocol: 'tcp' | 'udp'; port_start: number; port_end: number }[] },
): boolean {
  return (
    !!usage &&
    (usage.whole_taken ||
      (offer.kind === 'dedicated' && usage.ports.length > 0) ||
      usage.ports.some(used =>
        effectiveEntryRanges(mode, used.protocol, offer).some(
          r => r.start <= used.port_end && used.port_start <= r.end,
        ),
      ))
  );
}
export function entryModesOverlap(a: CvmEntryMode, b: CvmEntryMode, offer: CvmEntryAddressOffer): boolean {
  if (offer.kind === 'dedicated') return true;
  return (['tcp', 'udp'] as const).some(p =>
    effectiveEntryRanges(a, p, offer).some(x =>
      effectiveEntryRanges(b, p, offer).some(y => x.start <= y.end && y.start <= x.end),
    ),
  );
}

export function entryContainsPort(mode: CvmEntryMode, port: number, reserved?: CvmEntryReservedPorts): boolean {
  if (!Number.isInteger(port) || port < 1 || port > 65535) return false;
  if (mode.kind === 'l3') return false;
  if (mode.kind === 'l4_range')
    return (
      (mode.listen_start <= port && port <= mode.listen_end) || (mode.target_start <= port && port <= mode.target_end)
    );
  const offer: CvmEntryAddressOffer = { kind: 'dedicated', address: '', reserved_ports: reserved };
  return (['tcp', 'udp'] as const).some(p => {
    const selection = mode[p];
    if (selection.kind === 'disabled') return false;
    const effective = effectiveEntryRanges(mode, p, offer);
    return (
      (selection.ports.kind === 'ranges' && effective.some(r => r.start <= port && port <= r.end)) ||
      selection.mappings.some(
        m => m.listen.start <= port && port <= m.listen.end && effective.some(r => r.start <= port && port <= r.end),
      ) ||
      selection.mappings.some(
        m =>
          m.target.start <= port &&
          port <= m.target.end &&
          effective.some(
            r => r.start <= m.listen.start + port - m.target.start && m.listen.start + port - m.target.start <= r.end,
          ),
      )
    );
  });
}
