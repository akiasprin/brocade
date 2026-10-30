import type { UnlockConnectionObservation, UnlockPart, UnlockResult } from '../api';

const REASONS: Record<string, string> = {
  ipv6_no_address: '无 IPv6 出站',
  ipv6_not_supported: '无 IPv6 出站',
  network_unreachable: '出站网络不可达',
  host_unreachable: '目标不可达',
  connection_refused: '连接被拒绝',
  source_unavailable: '出站绑定地址不可用',
  source_family_mismatch: '出站绑定地址族不匹配',
  permission_denied: '出站连接被系统拒绝',
  connect_timeout: '出站连接超时',
  connection_reset: '出站连接被重置',
};

export function connectionFailureReason(connection: UnlockConnectionObservation): string | null {
  if (connection.version !== 2 || !connection.complete || connection.connected || !connection.error) return null;
  const reason = REASONS[connection.error];
  if (!reason) return null;
  if (connection.error.startsWith('ipv6_')) return connection.family === 'v6' ? reason : null;
  const family = connection.family === 'v6' ? 'IPv6 ' : connection.family === 'v4' ? 'IPv4 ' : '';
  return family + reason;
}

export function dialFailureReason(
  result: Partial<Pick<UnlockResult, 'connection_observations' | 'connection_requests'>> | undefined,
  part: UnlockPart,
): string | null {
  const records = result?.connection_observations?.filter(record => record.part === part) ?? [];
  if (!records.length || records.length > 64 || records.length !== result?.connection_requests?.[part]) return null;
  if (new Set(records.map(record => record.id)).size !== records.length) return null;
  const reasons = records.map(connectionFailureReason);
  return reasons[0] && reasons.every(reason => reason === reasons[0]) ? reasons[0] : null;
}
