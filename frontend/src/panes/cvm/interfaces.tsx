import { type CvmHost, type CvmInterfaceRequest } from '../../api';

export function initialInterfaces(host: CvmHost | undefined): CvmInterfaceRequest[] {
  const contact = host?.capabilities.network_sources[0];
  const rows: CvmInterfaceRequest[] = contact
    ? [{ source_id: contact.id, purpose: 'contact', default_ipv4: false, default_ipv6: false }]
    : [];
  return rows;
}

export function interfaceProblem(rows: CvmInterfaceRequest[], host: CvmHost | undefined): string | null {
  if (!host || !host.fresh || !host.capabilities.create) return '机器尚未就绪。';
  if (rows.length !== 1 || rows[0].purpose !== 'contact') return '创建时仅配置一张管理主网卡。';
  if (rows[0].source_id !== host.capabilities.network_sources[0]?.id) return '主网卡接入来源与宿主不匹配。';
  if (rows[0].default_ipv4 || rows[0].default_ipv6) return '主网卡不能设为业务出口。';
  return null;
}
