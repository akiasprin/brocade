import { describe, expect, it } from 'vitest';
import {
  dnsInputError,
  firstProvisionError,
  nodeIdError,
  portInputError,
  provisionFormErrors,
  validIpv4Endpoint,
  validIpv4Literal,
  validIpv6Literal,
} from '../src/provision-form';

describe('纳管机器表单校验', () => {
  it('在提交前识别重复或不合法的机器 ID', () => {
    const existing = new Set(['hk-01']);
    expect(nodeIdError('', existing)).toBe('填写机器 ID');
    expect(nodeIdError('HK-01', existing)).toContain('只能使用');
    expect(nodeIdError('hk-01', existing)).toBe('该 ID 已存在');
    expect(nodeIdError('tw.edge-02', existing)).toBeNull();
  });

  it('区分合法的 IPv4、IPv6 与不完整地址', () => {
    expect(validIpv4Literal('203.0.113.10')).toBe(true);
    expect(validIpv4Literal('203.0.113.999')).toBe(false);
    expect(validIpv4Literal('203.0.113')).toBe(false);
    expect(validIpv6Literal('2001:db8::10')).toBe(true);
    expect(validIpv6Literal('[2001:db8::10]')).toBe(true);
    expect(validIpv6Literal('[2001:db8::10')).toBe(false);
    expect(validIpv6Literal('2001:db8::10]')).toBe(false);
    expect(validIpv6Literal('2001:db8::zz')).toBe(false);
    expect(validIpv6Literal('203.0.113.10')).toBe(false);
  });

  it('公网 IPv4 也接受动态 DNS 主机名，但不把畸形数字地址当作域名', () => {
    expect(validIpv4Endpoint('edge.example.com')).toBe(true);
    expect(validIpv4Endpoint('edge.example.com.')).toBe(true);
    expect(validIpv4Endpoint('203.0.113.999')).toBe(false);
    expect(validIpv4Endpoint('https://edge.example.com')).toBe(false);
  });

  it('允许管理端口留空，但拒绝零、超范围或非数字端口', () => {
    expect(portInputError('51820')).toBeNull();
    expect(portInputError('', true)).toBeNull();
    expect(portInputError('')).not.toBeNull();
    expect(portInputError('0')).not.toBeNull();
    expect(portInputError('65536')).not.toBeNull();
    expect(portInputError('12.5')).not.toBeNull();
  });

  it('拒绝会被解析成空 DNS 项的逗号列表', () => {
    expect(dnsInputError('system')).toBeNull();
    expect(dnsInputError('1.1.1.1,8.8.8.8')).toBeNull();
    expect(dnsInputError('1.1.1.1,,8.8.8.8')).toContain('不能留空项');
    expect(dnsInputError(',')).toContain('不能留空项');
  });

  it('按页面顺序返回第一个阻塞原因', () => {
    const errors = provisionFormErrors(
      {
        id: 'new-node',
        public_ipv4: '203.0.113.999',
        public_ipv6: '2001:db8::zz',
        wg_listen_port: '0',
        api_port: '',
        dns: 'system',
      },
      new Set(),
    );
    expect(firstProvisionError(errors)).toBe('请输入完整的 IPv4 地址或主机名');
  });
});
