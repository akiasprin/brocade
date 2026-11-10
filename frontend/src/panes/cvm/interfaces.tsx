import { type CvmHost, type CvmInterfaceRequest } from '../../api';

export function initialInterfaces(host: CvmHost | undefined): CvmInterfaceRequest[] {
  const sources = host?.capabilities.network_sources ?? [];
  const contact = sources.find(source => source.kind === 'public');
  const rows: CvmInterfaceRequest[] = contact
    ? [{ source_id: contact.id, purpose: 'contact', default_ipv4: false, default_ipv6: false }]
    : [];
  return rows;
}

export function interfaceProblem(rows: CvmInterfaceRequest[], host: CvmHost | undefined): string | null {
  if (!host || !host.fresh || !host.capabilities.create) return '机器尚未就绪。';
  if (!rows.some(row => row.purpose === 'contact')) return '至少保留一张主网卡。';
  if (rows.length > host.capabilities.max_interfaces) return '网卡数量超出机器能力。';
  if (
    host.capabilities.network_sources.some(
      source =>
        source.max_attachments != null &&
        rows.filter(row => row.source_id === source.id).length > source.max_attachments,
    )
  )
    return '同一接入来源的挂载数量超出执行能力。';
  if (rows.filter(row => row.default_ipv4).length > 1 || rows.filter(row => row.default_ipv6).length > 1)
    return '每个地址族只能选择一个默认业务出口。';
  if (
    rows.some(
      row =>
        row.purpose === 'contact' &&
        host.capabilities.network_sources.find(source => source.id === row.source_id)?.kind !== 'public',
    )
  )
    return '主网卡请选择公网连接来源。';
  return null;
}
