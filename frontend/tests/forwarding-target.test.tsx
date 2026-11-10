import { describe, expect, it } from 'vitest';

import {
  blockedAddress,
  endpointText,
  isLinkLocalAddress,
  parseTarget,
  submittedHost,
  type ParsedTarget,
} from '../src/ui/forwarding-target';

const parsed = (text: string) => {
  const result = parseTarget(text);
  if ('error' in result) throw new Error(result.error);
  return result as ParsedTarget;
};

describe('转发目标地址', () => {
  it('粘贴 URL 时去掉协议头、路径和查询串', () => {
    expect(parseTarget('https://CDN.example.net:8443/path?x=1')).toEqual({
      host: 'cdn.example.net',
      port: 8443,
      kind: '域名',
      blocked: false,
    });
  });

  it('IPv6 要写方括号；提交时保留方括号，显示时不重复', () => {
    expect(parseTarget('2001:4860::1:443')).toEqual({ error: expect.stringContaining('方括号') });
    const target = parsed('[2001:4860::1]:443');
    expect(target).toMatchObject({ host: '2001:4860::1', port: 443, kind: 'IPv6', blocked: false });
    expect(submittedHost(target)).toBe('[2001:4860::1]');
    expect(endpointText('[2001:4860::1]', 443)).toBe('[2001:4860::1]:443');
    expect(endpointText('2001:4860::1', 443)).toBe('[2001:4860::1]:443');
  });

  it('缺端口、端口越界和无效 IPv4 给出原因', () => {
    expect(parseTarget('')).toEqual({ error: expect.stringContaining('目标地址') });
    expect(parseTarget('example.com')).toEqual({ error: expect.stringContaining('端口') });
    expect(parseTarget('example.com:70000')).toEqual({ error: expect.stringContaining('1–65535') });
    expect(parseTarget('300.1.1.1:80')).toEqual({ error: 'IPv4 地址无效' });
    expect(parseTarget('[zz::1]:80')).toEqual({ error: expect.stringContaining('IPv6') });
  });

  it('私网与保留地址的判断与服务端 target_ip_is_blocked 的用例一致', () => {
    for (const address of [
      '0.1.2.3',
      '10.0.0.1',
      '100.64.0.1',
      '127.0.0.1',
      '169.254.169.254',
      '172.16.0.1',
      '192.0.0.1',
      '192.168.1.1',
      '198.18.0.1',
      '203.0.113.1',
      '224.0.0.1',
      '255.255.255.255',
      '::',
      '::1',
      'fc00::1',
      'fe80::1',
      'ff02::1',
      '2001:db8::1',
      '::ffff:127.0.0.1',
      '::ffff:10.0.0.1',
      '[fe80::1]',
    ]) {
      expect(blockedAddress(address), address).toBe(true);
    }
    for (const address of ['1.1.1.1', '2606:4700:4700::1111', '::ffff:1.1.1.1', 'example.com']) {
      expect(blockedAddress(address), address).toBe(false);
    }
    expect(parsed('192.168.1.10:22').blocked).toBe(true);
  });
});

it('recognizes link-local address families without treating private networks as link-local', () => {
  for (const address of ['169.254.1.2', 'fe80::1', '[FE9F::2]', 'febf::1', '::ffff:169.254.1.2'])
    expect(isLinkLocalAddress(address), address).toBe(true);
  for (const address of ['10.60.0.2', '192.168.0.21', 'fd60::2', '2001:db8::1', 'fec0::1', 'example.com'])
    expect(isLinkLocalAddress(address), address).toBe(false);
});
