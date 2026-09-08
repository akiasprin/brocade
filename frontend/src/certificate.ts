import type { CertificateTrack } from './api';

/**
 * 面向用户说明机器当前使用的证书类型。
 *
 * 轨道来自当前 serving 证书；不能用域名或 issuer 推断。旧快照没有该字段时使用明确的
 * TLS 总称，避免把 CA 证书和自签证书重新归入含混的旧称。
 */
export function nodeCertificateLabel(track: CertificateTrack | null | undefined): string {
  if (track === 'public-ca') return '本机 CA 证书';
  if (track === 'self-signed') return '本机自签证书';
  return '本机 TLS 证书';
}
