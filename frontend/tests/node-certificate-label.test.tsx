import { describe, expect, it } from 'vitest';
import { nodeCertificateLabel } from '../src/certificate';

describe('机器证书说明', () => {
  it('区分 CA 证书与自签证书', () => {
    expect(nodeCertificateLabel('public-ca')).toBe('本机 CA 证书');
    expect(nodeCertificateLabel('self-signed')).toBe('本机自签证书');
  });

  it('旧快照缺少签发轨道时使用 TLS 总称', () => {
    expect(nodeCertificateLabel(undefined)).toBe('本机 TLS 证书');
  });
});
