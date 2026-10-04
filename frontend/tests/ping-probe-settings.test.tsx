import { describe, expect, it } from 'vitest';
import type { PingProbeKind, PingProbeSettings, PingProbeTarget } from '../src/api';
import { normalizePingEndpoint, pingProbeEndpointError, pingProbeFormError } from '../src/panes/settings';
import { pingLatencyMs, pingSampleText } from '../src/ui/ping-probe';

const target = (kind: PingProbeKind, ipv4: string | null, ipv6: string | null, name = 'target'): PingProbeTarget => ({
  name,
  kind,
  ipv4,
  ipv6,
});

const settings = (...targets: PingProbeTarget[]): PingProbeSettings => ({
  interval_secs: 60,
  timeout_ms: 420,
  targets,
});

describe('Ping probe settings', () => {
  it.each([
    ['tcp', 'ipv4', '1.1.1.1:443'],
    ['tcp', 'ipv4', 'example.com:80'],
    ['tcp', 'ipv6', '[2001:db8::1]:443'],
    ['tcp', 'ipv6', 'example.com:443'],
    ['icmp', 'ipv4', '1.1.1.1'],
    ['icmp', 'ipv4', 'example.com'],
    ['icmp', 'ipv6', '2001:db8::1'],
    ['icmp', 'ipv6', '[2001:db8::1]'],
    ['icmp', 'ipv4', ''],
  ] as const)('accepts %s %s endpoint %s', (kind, family, value) =>
    expect(pingProbeEndpointError(kind, family, value)).toBeNull(),
  );

  it.each([
    ['tcp', 'ipv4', 'example.com', 'TCP 地址格式应为 主机:端口'],
    ['tcp', 'ipv6', '2001:db8::1:443', 'TCP 的 IPv6 地址写作 [地址]:端口'],
    ['tcp', 'ipv6', '[not-ipv6]:443', '方括号内必须是 IPv6 地址'],
    ['tcp', 'ipv4', 'example.com:0', 'TCP 端口必须为 1–65535'],
    ['tcp', 'ipv4', 'tcp://1.1.1.1:443', 'IPv4 地址只写主机和端口，不带协议或路径'],
    ['icmp', 'ipv4', 'example.com:443', 'ICMP 不接受端口'],
    ['icmp', 'ipv4', '2001:db8::1', 'IPv4 栏不能填 IPv6 地址'],
    ['icmp', 'ipv6', '1.1.1.1', 'IPv6 栏不能填 IPv4 地址'],
    ['tcp', 'ipv6', '1.1.1.1:443', 'IPv6 栏不能填 IPv4 地址'],
  ] as const)('rejects %s %s endpoint %s', (kind, family, value, error) =>
    expect(pingProbeEndpointError(kind, family, value)).toBe(error),
  );

  it('stores ICMP IPv6 without brackets and an empty field as null', () => {
    expect(normalizePingEndpoint('icmp', ' [2001:db8::1] ')).toBe('2001:db8::1');
    expect(normalizePingEndpoint('tcp', ' [2001:db8::1]:443 ')).toBe('[2001:db8::1]:443');
    expect(normalizePingEndpoint('icmp', '   ')).toBeNull();
    expect(normalizePingEndpoint('tcp', null)).toBeNull();
  });

  it('accepts a target with only one family and mixed kinds in the same schedule', () => {
    expect(
      pingProbeFormError(
        settings(target('icmp', '1.1.1.1', null, 'v4'), target('tcp', null, '[2001:db8::1]:443', 'v6')),
      ),
    ).toBeNull();
  });

  it('requires at least one address per target and marks both fields', () => {
    expect(pingProbeFormError(settings(target('icmp', '', null, 'CF')))).toEqual({
      text: 'CF：至少填写一个地址',
      row: 0,
      families: ['ipv4', 'ipv6'],
    });
  });

  it('points a field error at its row and family', () => {
    expect(pingProbeFormError(settings(target('icmp', '1.1.1.1', null), target('tcp', null, 'example.com')))).toEqual({
      text: 'target：TCP 地址格式应为 主机:端口',
      row: 1,
      families: ['ipv6'],
    });
  });

  it('rejects a schedule below five seconds', () => {
    expect(pingProbeFormError({ interval_secs: 4, timeout_ms: 420, targets: [] })?.text).toContain('5–86400');
  });

  it('rejects a duplicate endpoint of the same kind and family only', () => {
    expect(
      pingProbeFormError(settings(target('icmp', 'Example.com', null, 'first'), target('icmp', 'example.com', null))),
    ).toEqual({ text: 'ICMP 的 IPv4 地址重复：example.com', row: 1, families: ['ipv4'] });
    // The same domain in both families of one target is a dual-stack target, not a duplicate.
    expect(pingProbeFormError(settings(target('icmp', 'example.com', 'example.com')))).toBeNull();
  });

  it('keeps capability gaps distinct from attempted timeouts', () => {
    const at = 1_700_000_000;
    expect(pingSampleText({ probed_at_unix_secs: at, attempted: false, latency_us: null })).toBe('未探测');
    expect(pingSampleText({ probed_at_unix_secs: at, attempted: true, latency_us: null })).toBe('无响应');
    expect(pingLatencyMs({ latency_us: 37_250 })).toBe(37.25);
    expect(pingSampleText({ probed_at_unix_secs: at, attempted: true, latency_us: 850 })).toBe('0.85 ms');
  });
});
