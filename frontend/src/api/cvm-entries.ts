// 容器访问入口：任意机器的地址按 L3（整 IP）或 L4（协议 + 端口）投递到容器主网卡，不占网卡槽位。
// 入口属于容器：删除容器即释放；同一用户的容器之间可以改派，地址与端口不变。
import { api } from './client';
import type { CvmTransportOffer } from './cvm-nics';

export type CvmEntryProtocol = 'tcp' | 'udp' | 'tcp_udp';
export type CvmEntryMode =
  { kind: 'l3' } | { kind: 'l4'; protocol: CvmEntryProtocol; listen_port: number; target_port: number };
export type CvmEntryAddressOffer =
  | { address: string; public_address?: string | null; kind: 'shared'; port_start: number; port_end: number }
  | { address: string; public_address?: string | null; kind: 'dedicated' };
export interface CvmEntryPublicationSpec {
  name: string;
  provider_node: string;
  addresses: CvmEntryAddressOffer[];
  transports: CvmTransportOffer[];
  grants: { tenant_id: string; user_id: string }[];
  max_entries: number;
}
export interface CvmEntryPublication {
  id: string;
  revision: number;
  admitting: boolean;
  retired: boolean;
  spec: CvmEntryPublicationSpec;
}
export interface CvmEntryPortUse {
  protocol: 'tcp' | 'udp';
  port_start: number;
  port_end: number;
  /** 当前账号可见的入口；他人的入口、转发与宿主服务只显示为已占用。 */
  entry_id: string | null;
}
export interface CvmEntryAddressUse {
  address: string;
  whole_taken: boolean;
  whole_entry_id: string | null;
  ports: CvmEntryPortUse[];
}
export interface CvmEntryPublicationView extends CvmEntryPublication {
  provider_name: string;
  active_entries: number;
  usage: CvmEntryAddressUse[];
}
export type CvmEntryState = 'pending' | 'active' | 'failed' | 'offline' | 'unauthorized' | 'no_target' | 'releasing';
export interface CvmEntry {
  id: string;
  revision: number;
  publication_id: string;
  provider_node: string;
  instance_id: string;
  /** 入口机器上匹配的地址。 */
  address: string;
  /** 访问者使用的地址（云厂商一对一 NAT 时与 address 不同）。 */
  visible_address: string;
  mode: CvmEntryMode;
  remote: boolean;
  state: CvmEntryState;
  error: string | null;
}
export interface CvmEntryBoard {
  publications: CvmEntryPublicationView[];
  entries: CvmEntry[];
  can_manage: boolean;
}

export const fetchCvmEntries = () => api<CvmEntryBoard>('/cvm/entries');
export const fetchCvmInstanceEntries = (id: string) => api<CvmEntry[]>(`/cvm/${encodeURIComponent(id)}/entries`);
export const createCvmEntry = (body: {
  request_id: string;
  instance_id: string;
  publication_id: string;
  address: string;
  mode: CvmEntryMode;
}) => api<CvmEntry>('/cvm/entries', '', { method: 'POST', body: JSON.stringify(body) });
export const repointCvmEntry = (
  id: string,
  body: { request_id: string; expected_revision: number; instance_id: string },
) => api<CvmEntry>(`/cvm/entries/${encodeURIComponent(id)}/target`, '', { method: 'PUT', body: JSON.stringify(body) });
export const releaseCvmEntry = (id: string, body: { request_id: string; expected_revision: number }) =>
  api<CvmEntry>(`/cvm/entries/${encodeURIComponent(id)}`, '', { method: 'DELETE', body: JSON.stringify(body) });
export const writeCvmEntryPublication = (
  id: string | null,
  body: { request_id: string; expected_revision: number; spec: CvmEntryPublicationSpec },
) =>
  api<CvmEntryPublication>(id ? `/cvm/entry-publications/${encodeURIComponent(id)}` : '/cvm/entry-publications', '', {
    method: id ? 'PUT' : 'POST',
    body: JSON.stringify(body),
  });
export const setCvmEntryAdmission = (
  id: string,
  body: { request_id: string; expected_revision: number; admitting: boolean },
) =>
  api<CvmEntryPublication>(`/cvm/entry-publications/${encodeURIComponent(id)}/admission`, '', {
    method: 'PUT',
    body: JSON.stringify(body),
  });
export const deleteCvmEntryPublication = (
  id: string,
  body: { request_id: string; expected_revision: number; admitting: boolean },
) =>
  api<CvmEntryPublication>(`/cvm/entry-publications/${encodeURIComponent(id)}`, '', {
    method: 'DELETE',
    body: JSON.stringify(body),
  });

export const entryProtocolText = (protocol: CvmEntryProtocol) =>
  protocol === 'tcp_udp' ? 'TCP+UDP' : protocol.toUpperCase();
export const entryStateText: Record<CvmEntryState, string> = {
  pending: '生效中',
  active: '已生效',
  failed: '执行失败',
  offline: '机器离线',
  unauthorized: '授权已撤销',
  no_target: '容器未运行',
  releasing: '撤销中',
};
